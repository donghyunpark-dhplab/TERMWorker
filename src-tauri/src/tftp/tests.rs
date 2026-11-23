//! TFTP server against a small client over loopback.

use super::*;
use std::sync::Mutex as StdMutex;

struct Fixture {
    server: TftpServer,
    port: u16,
    dir: PathBuf,
    events: Arc<StdMutex<Vec<TftpEvent>>>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.stop();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fixture(name: &str, allow_upload: bool) -> Fixture {
    let dir = std::env::temp_dir().join(format!("tw-tftp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let events = Arc::new(StdMutex::new(Vec::new()));
    let ev = events.clone();
    let server = TftpServer::new(Arc::new(move |e| ev.lock().unwrap_or_else(|p| p.into_inner()).push(e)));
    let status = server
        .start(TftpConfig { folder: dir.display().to_string(), port: 0, allow_upload, bind: Some("127.0.0.1".into()) })
        .unwrap();
    Fixture { server, port: status.port.unwrap(), dir, events }
}

/// The client is done when it sends its last ACK; the server a moment later.
async fn settled(server: &TftpServer) -> Vec<TransferInfo> {
    for _ in 0..100 {
        let list = server.transfers();
        if list.iter().all(|t| t.state != TransferState::Running) {
            return list;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("transfers still running: {:?}", server.transfers());
}

fn data(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
}

fn request(op: u16, name: &str, mode: &str, opts: &[(&str, &str)]) -> Vec<u8> {
    let mut p = op.to_be_bytes().to_vec();
    for s in [name, mode].iter().chain(opts.iter().flat_map(|(k, v)| [k, v])) {
        p.extend_from_slice(s.as_bytes());
        p.push(0);
    }
    p
}

fn parse_oack(p: &[u8]) -> Vec<(String, String)> {
    let f: Vec<String> = p[2..].split(|&b| b == 0).map(|s| String::from_utf8_lossy(s).into_owned()).collect();
    f.chunks(2).filter(|c| c.len() == 2 && !c[0].is_empty()).map(|c| (c[0].clone(), c[1].clone())).collect()
}

#[derive(Debug)]
struct Got {
    data: Vec<u8>,
    oack: Option<Vec<(String, String)>>,
}

/// Error code and message from the server.
type Fail = (u16, String);

async fn recv(sock: &UdpSocket, buf: &mut [u8]) -> (usize, SocketAddr) {
    tokio::time::timeout(Duration::from_secs(10), sock.recv_from(buf)).await.expect("no answer").unwrap()
}

fn fail_of(p: &[u8]) -> Fail {
    (block_of(p).unwrap(), String::from_utf8_lossy(&p[4..]).trim_end_matches('\0').to_string())
}

/// RRQ client. `skip_first_ack` leaves DATA 1 unanswered once, to see it sent again.
async fn get(port: u16, name: &str, mode: &str, opts: &[(&str, &str)], skip_first_ack: bool) -> Result<Got, Fail> {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&request(RRQ, name, mode, opts), ("127.0.0.1", port)).await.unwrap();
    let mut buf = vec![0u8; MAX_BLKSIZE + 4];
    let mut out = Vec::new();
    let mut blksize = 512;
    let mut expect: u16 = 1;
    let mut oack_opts = None;
    let mut skipped = !skip_first_ack;
    loop {
        let (n, from) = recv(&sock, &mut buf).await;
        let p = &buf[..n];
        match opcode(p) {
            ERROR => return Err(fail_of(p)),
            OACK => {
                let o = parse_oack(p);
                if let Some((_, v)) = o.iter().find(|(k, _)| k == "blksize") {
                    blksize = v.parse().unwrap();
                }
                oack_opts = Some(o);
                sock.send_to(&packet(ACK, 0, &[]), from).await.unwrap();
            }
            DATA => {
                let b = block_of(p).unwrap();
                if b != expect {
                    continue;
                }
                if !skipped {
                    skipped = true;
                    continue; // the server must send it again
                }
                out.extend_from_slice(&p[4..]);
                sock.send_to(&packet(ACK, b, &[]), from).await.unwrap();
                if n - 4 < blksize {
                    return Ok(Got { data: out, oack: oack_opts });
                }
                expect = expect.wrapping_add(1);
            }
            op => panic!("unexpected opcode {op}"),
        }
    }
}

/// WRQ client.
async fn put(port: u16, name: &str, body: &[u8], opts: &[(&str, &str)]) -> Result<(), Fail> {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&request(WRQ, name, "octet", opts), ("127.0.0.1", port)).await.unwrap();
    let mut buf = vec![0u8; 1024];
    let (n, peer) = recv(&sock, &mut buf).await;
    let p = &buf[..n];
    let blksize = match opcode(p) {
        ERROR => return Err(fail_of(p)),
        ACK => 512,
        OACK => parse_oack(p).iter().find(|(k, _)| k == "blksize").map(|(_, v)| v.parse().unwrap()).unwrap_or(512),
        op => panic!("unexpected opcode {op}"),
    };
    let mut block: u16 = 1;
    let mut chunks: Vec<&[u8]> = body.chunks(blksize).collect();
    if body.len().is_multiple_of(blksize) {
        chunks.push(&[]);
    }
    for c in chunks {
        sock.send_to(&packet(DATA, block, c), peer).await.unwrap();
        let (n, _) = recv(&sock, &mut buf).await;
        let p = &buf[..n];
        if opcode(p) == ERROR {
            return Err(fail_of(p));
        }
        assert_eq!((opcode(p), block_of(p)), (ACK, Some(block)));
        block = block.wrapping_add(1);
    }
    Ok(())
}

#[test]
fn requests_parse() {
    let r = parse_request(&request(RRQ, "fw.bin", "OCTET", &[("BLKSIZE", "1468"), ("tsize", "0")])).unwrap();
    assert_eq!(
        r,
        Request {
            write: false,
            file: "fw.bin".into(),
            netascii: false,
            options: vec![("blksize".into(), "1468".into()), ("tsize".into(), "0".into())],
        }
    );
    assert!(parse_request(&request(WRQ, "a", "netascii", &[])).unwrap().write);
    assert!(parse_request(&request(RRQ, "a", "mail", &[])).is_err());
    assert!(parse_request(&request(RRQ, "", "octet", &[])).is_err());
    assert!(parse_request(&[0, 3, 0, 1]).is_err());
}

#[test]
fn names_stay_in_the_folder() {
    let root = Path::new("/srv/tftp");
    assert_eq!(resolve(root, "fw.bin").unwrap(), root.join("fw.bin"));
    assert_eq!(resolve(root, "/fw.bin").unwrap(), root.join("fw.bin"));
    assert_eq!(resolve(root, "boards/a.bin").unwrap(), root.join("boards").join("a.bin"));
    assert_eq!(resolve(root, "boards\\a.bin").unwrap(), root.join("boards").join("a.bin"));
    assert_eq!(resolve(root, "./x//y").unwrap(), root.join("x").join("y"));
    for bad in ["../x", "a/../../x", "..", "C:\\x", "c:x", "", "/", "a/.."] {
        assert!(resolve(root, bad).is_err(), "{bad} should be refused");
    }
}

#[test]
fn netascii_round_trip() {
    let text = b"line 1\nline 2\r\nbare \r here\n".to_vec();
    let wire = to_netascii(&text);
    assert_eq!(&wire[..8], b"line 1\r\n");
    assert_eq!(from_netascii(&wire), text);
}

#[tokio::test]
async fn downloads_any_length() {
    let f = fixture("get", false);
    for len in [0, 1, 511, 512, 513, 1024, 100_000] {
        let body = data(len);
        std::fs::write(f.dir.join("fw.bin"), &body).unwrap();
        let got = get(f.port, "fw.bin", "octet", &[], false).await.unwrap();
        assert_eq!(got.data, body, "length {len}");
        assert!(got.oack.is_none());
    }
    let done = settled(&f.server).await;
    assert_eq!(done.len(), 7);
    assert!(done.iter().all(|t| t.state == TransferState::Done && !t.upload));
    assert_eq!(done.last().unwrap().bytes, 100_000);
    assert_eq!(done.last().unwrap().size, Some(100_000));
}

#[tokio::test]
async fn options_are_negotiated() {
    let f = fixture("opts", false);
    let body = data(70_000);
    std::fs::create_dir(f.dir.join("boards")).unwrap();
    std::fs::write(f.dir.join("boards").join("a.bin"), &body).unwrap();
    let got = get(
        f.port,
        "/boards/a.bin",
        "octet",
        &[("blksize", "1468"), ("tsize", "0"), ("timeout", "3"), ("windowsize", "4")],
        false,
    )
    .await
    .unwrap();
    assert_eq!(got.data, body);
    assert_eq!(
        got.oack.unwrap(),
        vec![("blksize".into(), "1468".into()), ("tsize".into(), "70000".into()), ("timeout".into(), "3".into())]
    );
    assert_eq!(settled(&f.server).await[0].block_size, 1468);
    // Too big is capped; too small is ignored.
    let got = get(f.port, "boards/a.bin", "octet", &[("blksize", "99999")], false).await.unwrap();
    assert_eq!(got.oack.unwrap(), vec![("blksize".into(), MAX_BLKSIZE.to_string())]);
    let got = get(f.port, "boards/a.bin", "octet", &[("blksize", "4")], false).await.unwrap();
    assert!(got.oack.is_none());
}

#[tokio::test]
async fn block_numbers_wrap_around() {
    let f = fixture("wrap", false);
    // 8-byte blocks: more than 65535 blocks in half a megabyte.
    let body = data(8 * 65_536 + 3);
    std::fs::write(f.dir.join("big.bin"), &body).unwrap();
    let got = get(f.port, "big.bin", "octet", &[("blksize", "8")], false).await.unwrap();
    assert_eq!(got.data.len(), body.len());
    assert!(got.data == body);
}

#[tokio::test]
async fn netascii_download() {
    let f = fixture("ascii", false);
    std::fs::write(f.dir.join("cfg.txt"), b"a\nb\n").unwrap();
    let got = get(f.port, "cfg.txt", "netascii", &[], false).await.unwrap();
    assert_eq!(got.data, b"a\r\nb\r\n");
}

#[tokio::test]
async fn lost_ack_is_resent() {
    let f = fixture("resend", false);
    let body = data(2000);
    std::fs::write(f.dir.join("fw.bin"), &body).unwrap();
    let got = get(f.port, "fw.bin", "octet", &[], true).await.unwrap();
    assert_eq!(got.data, body);
}

#[tokio::test]
async fn refusals() {
    let f = fixture("refuse", false);
    std::fs::write(f.dir.join("fw.bin"), b"x").unwrap();
    assert_eq!(get(f.port, "nope.bin", "octet", &[], false).await.unwrap_err().0, 1);
    assert_eq!(get(f.port, "../fw.bin", "octet", &[], false).await.unwrap_err().0, 2);
    // A folder is not a file.
    std::fs::create_dir(f.dir.join("sub")).unwrap();
    assert_eq!(get(f.port, "sub", "octet", &[], false).await.unwrap_err().0, 1);
    // Uploads are off.
    let (code, msg) = put(f.port, "new.bin", b"abc", &[]).await.unwrap_err();
    assert_eq!(code, 2);
    assert!(msg.contains("Uploads are turned off"), "{msg}");
    assert!(!f.dir.join("new.bin").exists());
    let failed = settled(&f.server).await.into_iter().filter(|t| t.state == TransferState::Failed).count();
    assert_eq!(failed, 4);
}

#[tokio::test]
async fn uploads_when_allowed() {
    let f = fixture("put", true);
    for len in [0, 512, 3000] {
        let body = data(len);
        let name = format!("up{len}.bin");
        put(f.port, &name, &body, &[]).await.unwrap();
        assert_eq!(std::fs::read(f.dir.join(&name)).unwrap(), body);
    }
    let body = data(5000);
    put(f.port, "logs/dev.log", &body, &[("blksize", "1024"), ("tsize", "5000")]).await.unwrap();
    assert_eq!(std::fs::read(f.dir.join("logs").join("dev.log")).unwrap(), body);
    let last = settled(&f.server).await.pop().unwrap();
    assert!(last.upload);
    assert_eq!((last.size, last.bytes, last.block_size), (Some(5000), 5000, 1024));
    // Never over an existing file, never outside the folder; no part files left.
    assert_eq!(put(f.port, "up512.bin", b"zzz", &[]).await.unwrap_err().0, 6);
    assert_eq!(std::fs::read(f.dir.join("up512.bin")).unwrap(), data(512));
    assert_eq!(put(f.port, "../escape.bin", b"zzz", &[]).await.unwrap_err().0, 2);
    let leftovers: Vec<_> = std::fs::read_dir(&f.dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
        .collect();
    assert!(leftovers.is_empty());
}

#[tokio::test]
async fn start_stop_and_events() {
    let f = fixture("events", false);
    std::fs::write(f.dir.join("fw.bin"), data(10)).unwrap();
    get(f.port, "fw.bin", "octet", &[], false).await.unwrap();
    settled(&f.server).await;
    {
        let events = f.events.lock().unwrap();
        assert!(matches!(&events[0], TftpEvent::Status { status } if status.running));
        assert!(events
            .iter()
            .any(|e| matches!(e, TftpEvent::Transfer { transfer } if transfer.state == TransferState::Done)));
    }
    // Same configuration: no restart. Missing folder: refused.
    let cfg = f.server.status().config.unwrap();
    assert_eq!(f.server.start(cfg.clone()).unwrap().port, Some(f.port));
    let err = f.server.start(TftpConfig { folder: f.dir.join("missing").display().to_string(), ..cfg }).unwrap_err();
    assert!(err.contains("does not exist"), "{err}");
    f.server.stop();
    assert!(!f.server.status().running);
    // Nothing answers any more.
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&request(RRQ, "fw.bin", "octet", &[]), ("127.0.0.1", f.port)).await.unwrap();
    let mut buf = [0u8; 600];
    assert!(tokio::time::timeout(Duration::from_millis(500), sock.recv_from(&mut buf))
        .await
        .map_or(true, |r| r.is_err()));
}

#[test]
fn port_in_use_is_explained() {
    let busy = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    let dir = std::env::temp_dir();
    let server = TftpServer::new(Arc::new(|_| {}));
    let err = server
        .start(TftpConfig {
            folder: dir.display().to_string(),
            port,
            allow_upload: false,
            bind: Some("127.0.0.1".into()),
        })
        .unwrap_err();
    assert!(err.contains(&format!("Port {port}")), "{err}");
}

#[test]
#[ignore = "prints this PC's addresses"]
fn show_local_addresses() {
    for a in local_addresses() {
        println!("IFADDR {a:?}");
    }
}

#[tokio::test]
async fn upload_is_done_before_the_dally() {
    let f = fixture("dally", true);
    let started = Instant::now();
    put(f.port, "slow.bin", &data(1500), &[("timeout", "5")]).await.unwrap();
    // Done as soon as the file is saved, not after the 5 s the server keeps listening.
    let list = settled(&f.server).await;
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    let t = list.last().unwrap();
    assert_eq!((t.state, t.bytes), (TransferState::Done, 1500));
    assert!(t.elapsed_ms < 2000, "{}", t.elapsed_ms);
    assert!(f.dir.join("slow.bin").is_file());
}
