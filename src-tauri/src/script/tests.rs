//! Scripts end to end against a fake U-Boot console on a local TCP port (raw TCP
//! session), with a mock Tauri app standing in for the UI.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::ipc::Channel;
use tauri::test::{mock_builder, mock_context, noop_assets, MockRuntime};
use tauri::{App, Listener, Manager};

use crate::commands::{self, AppState};
use crate::script::{script_reply, script_run, script_stop};

pub(super) fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tw-script-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub(super) fn app(dir: &Path) -> App<MockRuntime> {
    mock_builder().manage(AppState::new(dir.to_path_buf())).build(mock_context(noop_assets())).unwrap()
}

/// A board on a TCP port: autoboot countdown, then a `=> ` prompt that answers a few commands.
fn fake_uboot() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        sock.write_all(b"\r\nU-Boot 2024.01\r\nHit any key to stop autoboot:  3 ").unwrap();
        let mut byte = [0u8; 1];
        loop {
            if sock.read(&mut byte).unwrap_or(0) == 0 {
                return;
            }
            if byte[0] == b' ' {
                break;
            }
        }
        sock.write_all(b"\r\n=> ").unwrap();
        let mut line = Vec::new();
        while sock.read(&mut byte).unwrap_or(0) == 1 {
            if byte[0] != b'\r' {
                line.push(byte[0]);
                continue;
            }
            let cmd = String::from_utf8_lossy(&line).to_string();
            line.clear();
            let out = match cmd.as_str() {
                "version" => "U-Boot 2024.01 (Jan 01 2024)\r\n",
                "printenv ipaddr" => "ipaddr=192.168.1.1\r\n",
                "" => "",
                _ => "Unknown command\r\n",
            };
            // Echo in two writes so matching must work across chunks.
            sock.write_all(format!("{cmd}\r\n").as_bytes()).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            sock.write_all(format!("{out}=> ").as_bytes()).unwrap();
        }
    });
    port
}

fn open_raw(app: &App<MockRuntime>, tab: &str, port: u16) {
    let connection = serde_json::from_value(json!({ "kind": "raw", "host": "127.0.0.1", "port": port })).unwrap();
    commands::session_open(
        app.state(),
        tab.into(),
        connection,
        Default::default(),
        None,
        80,
        24,
        Channel::new(|_| Ok(())),
    )
    .unwrap();
}

pub(super) struct Finished {
    pub outcome: String,
    pub message: Option<String>,
    pub output: Vec<String>,
}

/// Runs `source` from the tab `tab`, answering UI requests with `answer`.
pub(super) fn run_script(
    app: &App<MockRuntime>,
    dir: &Path,
    name: &str,
    source: &str,
    tab: Option<&str>,
    answer: impl Fn(&str, &Value) -> Option<Result<Value, String>> + Send + 'static,
    before_wait: impl FnOnce(u64),
) -> Finished {
    let path = dir.join(name);
    std::fs::write(&path, source).unwrap();

    let (state_tx, state_rx) = mpsc::channel::<Value>();
    let (out_tx, out_rx) = mpsc::channel::<String>();
    app.listen("script:state", move |e| {
        let _ = state_tx.send(serde_json::from_str(e.payload()).unwrap());
    });
    app.listen("script:output", move |e| {
        let v: Value = serde_json::from_str(e.payload()).unwrap();
        let _ = out_tx.send(format!("{}: {}", v["stream"].as_str().unwrap(), v["text"].as_str().unwrap()));
    });
    let handle = app.handle().clone();
    app.listen("script:request", move |e| {
        let v: Value = serde_json::from_str(e.payload()).unwrap();
        let (run, id) = (v["run"].as_u64().unwrap(), v["id"].as_u64().unwrap());
        let (ok, value) = match answer(v["kind"].as_str().unwrap(), &v["args"]) {
            Some(Ok(v)) => (true, v),
            Some(Err(e)) => (false, Value::String(e)),
            None => return, // never answered
        };
        script_reply(handle.state(), run, id, ok, value);
    });

    let run = script_run(
        app.handle().clone(),
        app.state(),
        path.display().to_string(),
        tab.map(str::to_owned),
        Some("Board".into()),
    )
    .unwrap();
    before_wait(run);
    loop {
        let st = state_rx.recv_timeout(Duration::from_secs(20)).unwrap_or_else(|_| panic!("{name} did not finish"));
        if st["run"] == run && st["running"] == false {
            std::thread::sleep(Duration::from_millis(50)); // late output events
            return Finished {
                outcome: st["outcome"].as_str().unwrap().to_string(),
                message: st["message"].as_str().map(str::to_owned),
                output: out_rx.try_iter().collect(),
            };
        }
    }
}

#[test]
fn uboot_session_end_to_end() {
    let dir = temp_dir("uboot");
    let app = app(&dir);
    let port = fake_uboot();
    let source = r#"
const s = tab;
print("tab", s.name);
await s.waitFor("Hit any key", { timeout: 5000 });
s.send(" ");
await s.waitFor(/=> $/);
const v = await s.command("version", { prompt: /=> $/ });
print("version", JSON.stringify(v));
const env = await s.command("printenv ipaddr", { prompt: /=> $/ });
if (env.trim() !== "ipaddr=192.168.1.1") throw new Error("env: " + JSON.stringify(env));
const name = await ui.prompt("Your name?", "x");
print("name", name);
try {
  await s.waitFor("never", { timeout: 200 });
  throw new Error("no timeout");
} catch (e) {
  if (!(e instanceof TimeoutError)) throw e;
  print("timeout ok");
}
s.sendLine("bogus");
const r = await s.waitForAny([/Unknown command/, /ipaddr=/], { timeout: 2000 });
print("any", r.index, r.match);
await s.waitForQuiet(100);
fs.writeText("out/v.txt", v);
print("file", fs.readText("out/v.txt") === v);
await sleep(30);
"#;
    let port_open = port;
    let app_ref = &app;
    let f = run_script(
        &app,
        &dir,
        "uboot.js",
        source,
        Some("t1"),
        |kind, args| match kind {
            "prompt" => {
                assert_eq!(args["message"], "Your name?");
                Some(Ok(json!("tester")))
            }
            other => Some(Err(format!("unexpected request {other}"))),
        },
        // The script must be watching the tab before the board starts talking.
        |_| {
            std::thread::sleep(Duration::from_millis(300));
            open_raw(app_ref, "t1", port_open)
        },
    );
    assert_eq!(f.outcome, "done", "{:?} {:?}", f.message, f.output);
    assert_eq!(
        f.output,
        vec![
            "out: tab Board",
            "out: version \"U-Boot 2024.01 (Jan 01 2024)\\n\"",
            "out: name tester",
            "out: timeout ok",
            "out: any 0 Unknown command",
            "out: file true",
        ]
    );
    assert!(dir.join("out/v.txt").is_file());
}

#[test]
fn error_reports_script_line() {
    let dir = temp_dir("error");
    let app = app(&dir);
    let f = run_script(
        &app,
        &dir,
        "fail.js",
        "print('a');\nawait sleep(10);\nthrow new Error('boom');\n",
        None,
        |_, _| Some(Err("no".into())),
        |_| {},
    );
    assert_eq!(f.outcome, "failed");
    let msg = f.message.unwrap();
    assert!(msg.starts_with("Error: boom"), "{msg}");
    assert!(msg.contains("fail.js:3"), "{msg}");
    assert!(!msg.contains("prelude.js"), "{msg}");
}

#[test]
fn timeout_error_points_at_the_wait() {
    let dir = temp_dir("timeout");
    let app = app(&dir);
    let f = run_script(
        &app,
        &dir,
        "wait.js",
        "// nothing is connected\n\nawait tab.waitFor(/login:/, { timeout: 100 });\n",
        Some("none"),
        |_, _| Some(Err("no".into())),
        |_| {},
    );
    assert_eq!(f.outcome, "failed");
    let msg = f.message.unwrap();
    assert!(msg.starts_with("TimeoutError: Board: /login:/ did not appear within 0.1 s"), "{msg}");
    assert!(msg.contains("wait.js:3"), "{msg}");
}

#[test]
fn stop_ends_busy_and_waiting_scripts() {
    for (name, source) in
        [("busy.js", "for (;;) {}\n"), ("idle.js", "await sleep(Infinity);\n"), ("ask.js", "await ui.confirm('?');\n")]
    {
        let dir = temp_dir(name);
        let app = app(&dir);
        let handle = app.handle().clone();
        let f = run_script(
            &app,
            &dir,
            name,
            source,
            None,
            |_, _| None,
            move |run| {
                std::thread::sleep(Duration::from_millis(200));
                script_stop(handle.state(), run);
            },
        );
        assert_eq!(f.outcome, "stopped", "{name}: {:?}", f.message);
    }
}

#[test]
fn send_without_a_session_throws() {
    let dir = temp_dir("nosession");
    let app = app(&dir);
    let f = run_script(
        &app,
        &dir,
        "send.js",
        "try { tab.send('x'); } catch (e) { print(e.message); }\nprint(typeof sessions.open, typeof ui.password);\n",
        Some("none"),
        |_, _| Some(Err("no".into())),
        |_| {},
    );
    assert_eq!(f.outcome, "done", "{:?}", f.message);
    assert_eq!(f.output, vec!["out: The session is not connected", "out: function function"]);
}

#[test]
fn opened_session_hears_its_first_bytes() {
    let dir = temp_dir("open");
    let app = app(&dir);
    let port = fake_uboot();
    let handle = app.handle().clone();
    let f = run_script(
        &app,
        &dir,
        "open.js",
        r#"
const b = await sessions.open("Board");
await b.waitFor("Hit any key", { timeout: 3000 });
b.sendKeys("<SPACE>");
await b.waitFor(/=> $/);
print("opened", b.name, b.connected, await b.command("version", { prompt: /=> $/ }));
"#,
        None,
        move |kind, args| match kind {
            // What the UI does: make the tab, which connects at once and starts talking.
            "open" => {
                let tab = args["tab"].as_str().unwrap();
                let connection =
                    serde_json::from_value(json!({ "kind": "raw", "host": "127.0.0.1", "port": port })).unwrap();
                let state = handle.state::<AppState>();
                commands::session_open(
                    state,
                    tab.into(),
                    connection,
                    Default::default(),
                    None,
                    80,
                    24,
                    Channel::new(|_| Ok(())),
                )
                .unwrap();
                // Answer only once the banner is surely out: it must still be found.
                std::thread::sleep(Duration::from_millis(300));
                Some(Ok(json!({ "tab": tab, "name": "Board A" })))
            }
            other => Some(Err(format!("unexpected request {other}"))),
        },
        |_| {},
    );
    assert_eq!(f.outcome, "done", "{:?} {:?}", f.message, f.output);
    assert_eq!(f.output, vec!["out: opened Board A true U-Boot 2024.01 (Jan 01 2024)\n"]);
}

#[test]
fn hdlc_frames_round_trip() {
    let dir = temp_dir("hdlc");
    let app = app(&dir);
    // A device that sends back whatever it gets.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        while let Ok(n @ 1..) = sock.read(&mut buf) {
            sock.write_all(&buf[..n]).unwrap();
        }
    });
    open_raw(&app, "t1", port);
    std::thread::sleep(Duration::from_millis(200));
    let handle = app.handle().clone();
    let f = run_script(
        &app,
        &dir,
        "hdlc.js",
        r#"
await tab.hdlc.enable({ fcs: 16 });
tab.hdlc.send([0x7e, 0x01, 0x7d, 0x02]);
tab.send("plain text\r\n");
const f = await tab.hdlc.waitFrame({ timeout: 3000 });
print(hex(f.payload), f.fcsOk);
await tab.waitFor("plain text", { timeout: 3000 });
print("text still reaches the terminal");
"#,
        Some("t1"),
        move |kind, args| match kind {
            "hdlc" => {
                let cfg = serde_json::from_value(args["config"].clone()).unwrap();
                handle.state::<AppState>().session_for_tab("t1").unwrap().set_hdlc(Some(cfg));
                Some(Ok(Value::Null))
            }
            other => Some(Err(format!("unexpected request {other}"))),
        },
        |_| {},
    );
    assert_eq!(f.outcome, "done", "{:?} {:?}", f.message, f.output);
    assert_eq!(f.output, vec!["out: 7E 01 7D 02 true", "out: text still reaches the terminal"]);
}

#[test]
fn raw_bytes_round_trip() {
    let dir = temp_dir("bytes");
    let app = app(&dir);
    // A device that sends back whatever it gets.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        while let Ok(n @ 1..) = sock.read(&mut buf) {
            sock.write_all(&buf[..n]).unwrap();
        }
    });
    open_raw(&app, "t1", port);
    std::thread::sleep(Duration::from_millis(200));
    let f = run_script(
        &app,
        &dir,
        "bytes.js",
        r#"
let seen = 0;
tab.bytes.onData((chunk) => { seen += chunk.length; });
const all = Uint8Array.from({ length: 256 }, (_, i) => i);   // every byte value, invalid UTF-8 included
tab.sendBytes(all);
const back = await tab.bytes.read(256, { timeout: 3000 });
print(back.length, back.every((b, i) => b === i));
tab.sendBytes([0x02, 0x10, 0xff, 0x03, 0x55]);
print(hex(await tab.bytes.readUntil([0x03], { timeout: 3000 })));
await sleep(200);
print(tab.bytes.available, hex(tab.bytes.take()), tab.bytes.available);
try {
  await tab.bytes.read(4, { timeout: 200 });
} catch (e) {
  print(e instanceof TimeoutError, e.message);
}
print(seen);
"#,
        Some("t1"),
        |kind, _| Some(Err(format!("unexpected request {kind}"))),
        |_| {},
    );
    assert_eq!(f.outcome, "done", "{:?} {:?}", f.message, f.output);
    assert_eq!(
        f.output,
        vec![
            "out: 256 true",
            "out: 02 10 FF 03",
            "out: 1 55 0",
            "out: true Board: 4 bytes did not arrive within 0.2 s. Unread bytes (0): (nothing)",
            "out: 261",
        ]
    );
}

/// The example of AN-001 section 7, run against a device that speaks its protocol.
#[test]
fn binary_protocol_example() {
    let dir = temp_dir("binary");
    let app = app(&dir);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        sock.write_all(b"boot banner\r\n").unwrap(); // text the script must not mistake for a reply
        let xor = |b: &[u8]| b.iter().fold(0u8, |a, x| a ^ x);
        let mut got = Vec::new();
        let mut buf = [0u8; 256];
        while let Ok(n @ 1..) = sock.read(&mut buf) {
            got.extend_from_slice(&buf[..n]);
            while got.len() >= 4 && got.len() >= 4 + got[2] as usize {
                let len = 4 + got[2] as usize;
                let req: Vec<u8> = got.drain(..len).collect();
                assert_eq!(req[0], 0xa5);
                assert_eq!(xor(&req[..len - 1]), req[len - 1], "request checksum");
                let payload: &[u8] = if req[1] == 0x01 { &[0x12, 0x34, 0x56, 0x78] } else { &[] };
                let mut reply = vec![0x5a, 0x00, payload.len() as u8];
                reply.extend_from_slice(payload);
                reply.push(xor(&reply));
                // In two pieces, as a slow line would deliver it.
                sock.write_all(&reply[..2]).unwrap();
                std::thread::sleep(Duration::from_millis(50));
                sock.write_all(&reply[2..]).unwrap();
            }
        }
    });
    open_raw(&app, "t1", port);
    std::thread::sleep(Duration::from_millis(200));
    let result = std::sync::Arc::new(std::sync::Mutex::new(Value::Null));
    let seen = result.clone();
    let f = run_script(
        &app,
        &dir,
        "binary-protocol.js",
        include_str!("../../../examples/scripts/binary-protocol.js"),
        Some("t1"),
        move |kind, args| match kind {
            "result" => {
                *seen.lock().unwrap() = args.clone();
                Some(Ok(Value::Null))
            }
            other => Some(Err(format!("unexpected request {other}"))),
        },
        |_| {},
    );
    assert_eq!(f.outcome, "done", "{:?} {:?}", f.message, f.output);
    assert_eq!(f.output, vec!["out: ID: 12 34 56 78"]);
    let r = result.lock().unwrap().clone();
    assert_eq!(r["status"], "PASS", "{r}");
    assert_eq!(r["detail"], "device 12 34 56 78", "{r}");
}

/// Plain RRQ over loopback, blocking: what a board's `tftpboot` does.
fn tftp_fetch(port: u16, name: &str) -> Vec<u8> {
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut req = vec![0, 1];
    req.extend_from_slice(name.as_bytes());
    req.extend_from_slice(b"\0octet\0");
    sock.send_to(&req, ("127.0.0.1", port)).unwrap();
    let mut out = Vec::new();
    let mut buf = [0u8; 516];
    loop {
        let (n, from) = sock.recv_from(&mut buf).unwrap();
        assert_eq!(buf[1], 3, "expected DATA, got {:?}", &buf[..n]);
        out.extend_from_slice(&buf[4..n]);
        sock.send_to(&[0, 4, buf[2], buf[3]], from).unwrap();
        if n < 516 {
            return out;
        }
    }
}

#[test]
fn tftp_from_a_script() {
    let dir = temp_dir("tftp");
    let app = app(&dir);
    app.manage(crate::tftp::TftpServer::new(std::sync::Arc::new(|_| {})));
    std::fs::create_dir_all(dir.join("srv")).unwrap();
    let body: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
    std::fs::write(dir.join("srv").join("fw.bin"), &body).unwrap();
    let source = r#"
const info = tftp.start({ folder: "srv", port: 0, bind: "127.0.0.1" });
print("running", tftp.running, info.folder.endsWith("srv"));
if (!Array.isArray(info.addresses)) throw new Error("addresses");
fs.writeText("port.txt", String(info.port));
const t = await tftp.waitFor("FW.BIN", { timeout: 5000 });
print("got", t.file, t.bytes, t.state, t.upload);
try {
  await tftp.waitFor("other.bin", { timeout: 200 });
} catch (e) {
  print("timeout", e instanceof TimeoutError);
}
tftp.stop();
print("running", tftp.running);
"#;
    let port_file = dir.join("port.txt");
    let fetched = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let fetched2 = fetched.clone();
    let f = run_script(
        &app,
        &dir,
        "tftp.js",
        source,
        None,
        |_, _| Some(Err("no".into())),
        move |_| {
            std::thread::spawn(move || {
                let port = loop {
                    if let Ok(p) = std::fs::read_to_string(&port_file) {
                        if let Ok(p) = p.trim().parse::<u16>() {
                            break p;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(20));
                };
                *fetched2.lock().unwrap() = tftp_fetch(port, "/fw.bin");
            });
        },
    );
    assert_eq!(f.outcome, "done", "{:?} {:?}", f.message, f.output);
    assert_eq!(*fetched.lock().unwrap(), body);
    assert_eq!(
        f.output,
        vec!["out: running true true", "out: got /fw.bin 3000 done false", "out: timeout true", "out: running false"]
    );
}
