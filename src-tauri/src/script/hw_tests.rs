//! The example scripts over a real serial line. Ignored by default: wire two
//! USB-serial adapters to each other (TX↔RX, RX↔TX, GND↔GND) and run
//!
//! ```text
//! TW_HW_PORT=/dev/ttyUSB2 TW_HW_PEER=/dev/ttyUSB3 cargo test --lib -- --ignored --test-threads=1 --nocapture hw_script
//! ```
//!
//! TermWorker opens `TW_HW_PORT` (a serial session in a mock app) and runs the
//! file from `examples/scripts/` unchanged. On `TW_HW_PEER` the test plays the
//! device: a U-Boot console, an AT modem or a binary request/response protocol.
//! Swap the two variables to run each adapter on the other side.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};
use serialport::SerialPort;
use tauri::ipc::Channel;
use tauri::test::MockRuntime;
use tauri::{App, Manager};

use super::tests::{app, run_script, temp_dir};
use crate::commands;

const BAUD: u32 = 115_200;

fn ports() -> Option<(String, String)> {
    let var = |n| std::env::var(n).ok().filter(|v: &String| !v.trim().is_empty());
    match (var("TW_HW_PORT"), var("TW_HW_PEER")) {
        (Some(a), Some(b)) => Some((a, b)),
        _ => {
            eprintln!("skipped: set TW_HW_PORT and TW_HW_PEER");
            None
        }
    }
}

fn open_serial(app: &App<MockRuntime>, tab: &str, port: &str) -> u32 {
    let connection = serde_json::from_value(json!({
        "kind": "serial", "port": port, "baudRate": BAUD, "autoReconnect": false,
    }))
    .unwrap();
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
    .unwrap()
}

/// The device end of the line, driven by the test.
struct Device {
    port: Box<dyn SerialPort>,
    stop: Arc<AtomicBool>,
}

impl Device {
    /// The next byte, or None once the test is over.
    fn byte(&mut self) -> Option<u8> {
        let mut b = [0u8; 1];
        while !self.stop.load(Ordering::Relaxed) {
            match self.port.read(&mut b) {
                Ok(1) => return Some(b[0]),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => panic!("peer read: {e}"),
            }
        }
        None
    }

    /// A line ended by CR (LF is skipped).
    fn line(&mut self) -> Option<String> {
        let mut line = Vec::new();
        loop {
            match self.byte()? {
                b'\r' => return Some(String::from_utf8_lossy(&line).into_owned()),
                b'\n' => {}
                b => line.push(b),
            }
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.port.write_all(bytes).unwrap();
        self.port.flush().unwrap();
    }
}

struct Running {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn device(peer: &str, run: impl FnOnce(&mut Device) + Send + 'static) -> Running {
    let port = serialport::new(peer, BAUD).timeout(Duration::from_millis(50)).open().unwrap();
    let _ = port.clear(serialport::ClearBuffer::All);
    let stop = Arc::new(AtomicBool::new(false));
    let mut dev = Device { port, stop: stop.clone() };
    let thread = std::thread::spawn(move || run(&mut dev));
    Running { stop, thread: Some(thread) }
}

/// Runs an example script on `port` against `play` on `peer`; returns (outcome, output, UI result).
fn run_example(
    name: &str,
    source: &str,
    answers: Vec<(&'static str, Value)>,
    play: impl FnOnce(&mut Device) + Send + 'static,
) -> Option<(super::tests::Finished, Value)> {
    let (port, peer) = ports()?;
    eprintln!("{name}: TermWorker on {port}, device on {peer}");
    let dir = temp_dir(&format!("hw-{name}"));
    let app = app(&dir);
    let dev = device(&peer, play);
    let id = open_serial(&app, "t1", &port);
    std::thread::sleep(Duration::from_millis(300));

    let result = Arc::new(Mutex::new(Value::Null));
    let seen = result.clone();
    let f = run_script(
        &app,
        &dir,
        name,
        source,
        Some("t1"),
        move |kind, args| match kind {
            "status" => Some(Ok(Value::Null)),
            "result" => {
                *seen.lock().unwrap() = args.clone();
                Some(Ok(Value::Null))
            }
            other => answers.iter().find(|(k, _)| *k == other).map(|(_, v)| Ok(v.clone())),
        },
        |_| {},
    );
    commands::session_close(app.state(), id);
    drop(dev);
    for line in &f.output {
        eprintln!("  {line}");
    }
    let r = result.lock().unwrap().clone();
    eprintln!("  outcome {} {:?}, result {r}", f.outcome, f.message);
    let _ = std::fs::remove_dir_all(&dir);
    Some((f, r))
}

/// A U-Boot console that powers on shortly after the script starts.
fn uboot(d: &mut Device) {
    std::thread::sleep(Duration::from_millis(800));
    d.send(b"\r\nU-Boot 2024.01\r\n\r\nDRAM:  256 MiB\r\nHit any key to stop autoboot:  3 ");
    while d.byte() != Some(b' ') {
        if d.stop.load(Ordering::Relaxed) {
            return;
        }
    }
    d.send(b"\x08\x08 0 \r\n=> ");
    while let Some(cmd) = d.line() {
        d.send(format!("{cmd}\r\n").as_bytes()); // echo
        let out = match cmd.as_str() {
            c if c.starts_with("setenv ") => String::new(),
            "tftpboot 0x80000000 fw.bin" => {
                d.send(b"Using ethernet@1e100000 device\r\nTFTP from server 192.168.1.10; our IP address is 192.168.1.1\r\nLoading: ");
                for _ in 0..5 {
                    std::thread::sleep(Duration::from_millis(100));
                    d.send(b"##########");
                }
                // Split inside the number, as a slow line may deliver it: a pattern
                // ending in (\d+) would match "1" here.
                d.send(b"\r\ndone\r\nBytes transferred = 1");
                std::thread::sleep(Duration::from_millis(30));
                "048576 (100000 hex)\r\n".into()
            }
            "" => String::new(),
            other => format!("Unknown command '{other}' - try 'help'\r\n"),
        };
        d.send(format!("{out}=> ").as_bytes());
    }
}

/// An AT modem with echo on.
fn modem(d: &mut Device) {
    while let Some(cmd) = d.line() {
        d.send(format!("{cmd}\r").as_bytes());
        let out = match cmd.as_str() {
            "ATI" => "\r\nManufacturer: Example\r\nModel: LTE-TEST\r\nRevision: 1.0\r\n\r\nOK\r\n",
            "AT+CGSN" => "\r\n861234567890123\r\n\r\nOK\r\n",
            "AT+CIMI" => "\r\n450001234567890\r\n\r\nOK\r\n",
            "AT+CSQ" => "\r\n+CSQ: 23,99\r\n\r\nOK\r\n",
            "AT+COPS?" => "\r\n+COPS: 0,0,\"TEST NET\",7\r\n\r\nOK\r\n",
            "" => continue,
            _ => "\r\nERROR\r\n",
        };
        d.send(out.as_bytes());
    }
}

/// `A5 cmd len payload xor` → `5A status len payload xor`, after some boot text.
fn binary(d: &mut Device) {
    d.send(b"boot banner\r\n");
    let xor = |b: &[u8]| b.iter().fold(0u8, |a, x| a ^ x);
    loop {
        let mut req = Vec::new();
        for _ in 0..3 {
            match d.byte() {
                Some(b) => req.push(b),
                None => return,
            }
        }
        for _ in 0..=req[2] {
            match d.byte() {
                Some(b) => req.push(b),
                None => return,
            }
        }
        assert_eq!(req[0], 0xa5, "start byte");
        assert_eq!(xor(&req[..req.len() - 1]), req[req.len() - 1], "request checksum");
        let payload: &[u8] = if req[1] == 0x01 { &[0x12, 0x34, 0x56, 0x78] } else { &[] };
        let mut reply = vec![0x5a, 0x00, payload.len() as u8];
        reply.extend_from_slice(payload);
        reply.push(xor(&reply));
        d.send(&reply[..2]);
        std::thread::sleep(Duration::from_millis(30));
        d.send(&reply[2..]);
    }
}

#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_script_uboot_tftp() {
    let Some((f, r)) = run_example(
        "uboot-tftp.js",
        include_str!("../../../examples/scripts/uboot-tftp.js"),
        vec![("prompt", json!("fw.bin"))], // the image name the user would type
        uboot,
    ) else {
        return;
    };
    assert_eq!(f.outcome, "done", "{:?} {:?}", f.message, f.output);
    assert_eq!(r["status"], "PASS", "{r}");
    assert_eq!(r["detail"], "fw.bin: 1048576 bytes", "{r}");
}

#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_script_modem_info() {
    let Some((f, _)) =
        run_example("modem-info.js", include_str!("../../../examples/scripts/modem-info.js"), vec![], modem)
    else {
        return;
    };
    assert_eq!(f.outcome, "done", "{:?} {:?}", f.message, f.output);
    assert_eq!(
        &f.output[..5],
        [
            "out: ATI\tManufacturer: Example Model: LTE-TEST Revision: 1.0",
            "out: AT+CGSN\t861234567890123",
            "out: AT+CIMI\t450001234567890",
            "out: AT+CSQ\t+CSQ: 23,99",
            "out: AT+COPS?\t+COPS: 0,0,\"TEST NET\",7",
        ]
    );
}

#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_script_binary_protocol() {
    let Some((f, r)) =
        run_example("binary-protocol.js", include_str!("../../../examples/scripts/binary-protocol.js"), vec![], binary)
    else {
        return;
    };
    assert_eq!(f.outcome, "done", "{:?} {:?}", f.message, f.output);
    assert_eq!(f.output, vec!["out: ID: 12 34 56 78"]);
    assert_eq!(r["status"], "PASS", "{r}");
}

/// The short U-Boot example shown on the product page, exactly as published.
#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_script_uboot_short() {
    let Some((f, r)) = run_example(
        "uboot-tftp-short.js",
        include_str!("../../../examples/scripts/uboot-tftp-short.js"),
        vec![],
        uboot,
    ) else {
        return;
    };
    assert_eq!(f.outcome, "done", "{:?} {:?}", f.message, f.output);
    assert_eq!(r["status"], "PASS", "{r}");
    assert_eq!(r["detail"], "Bytes transferred = 1048576 ", "{r}");
}
