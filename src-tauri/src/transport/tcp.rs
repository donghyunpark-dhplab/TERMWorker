//! Raw TCP and Telnet transports.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use super::telnet::Telnet;
use super::{EventSink, LinkState, TransportCmd};
use crate::config::TcpConfig;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub async fn run(
    cfg: TcpConfig,
    telnet: bool,
    cols: u16,
    rows: u16,
    mut cmd_rx: mpsc::UnboundedReceiver<TransportCmd>,
    sink: EventSink,
) {
    let target = format!("{}:{}", cfg.host, cfg.port);
    sink.state(LinkState::Connecting, format!("Connecting to {target}…"));

    let stream = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((cfg.host.as_str(), cfg.port))).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            sink.state(
                LinkState::Disconnected,
                format!("Could not connect to {target}: {}", super::describe_io_error(&e)),
            );
            return;
        }
        Err(_) => {
            sink.state(LinkState::Disconnected, format!("Could not connect to {target}: timed out"));
            return;
        }
    };
    let _ = stream.set_nodelay(true);
    let (mut rd, mut wr) = stream.into_split();

    let mut proto = telnet.then(|| Telnet::new(cols, rows));
    if let Some(t) = proto.as_mut() {
        if let Err(e) = wr.write_all(&t.initial()).await {
            sink.state(LinkState::Disconnected, format!("Connection lost: {}", super::describe_io_error(&e)));
            return;
        }
    }
    sink.state(LinkState::Connected, target.clone());

    let mut buf = vec![0u8; 32 * 1024];
    let reason = loop {
        tokio::select! {
            r = rd.read(&mut buf) => match r {
                Ok(0) => break "Connection closed by remote host".to_string(),
                Ok(n) => {
                    let payload = match proto.as_mut() {
                        Some(t) => {
                            let (mut data, mut reply) = (Vec::with_capacity(n), Vec::new());
                            t.receive(&buf[..n], &mut data, &mut reply);
                            if !reply.is_empty() {
                                if let Err(e) = wr.write_all(&reply).await {
                                    break format!("Connection lost: {}", super::describe_io_error(&e));
                                }
                            }
                            data
                        }
                        None => buf[..n].to_vec(),
                    };
                    if !payload.is_empty() && !sink.data(payload) {
                        return;
                    }
                }
                Err(e) => break format!("Connection lost: {}", super::describe_io_error(&e)),
            },
            cmd = cmd_rx.recv() => match cmd {
                None | Some(TransportCmd::Close) => {
                    let _ = wr.shutdown().await;
                    return;
                }
                Some(cmd @ (TransportCmd::Write(_) | TransportCmd::WriteData(_))) => {
                    let out = match (proto.as_ref(), cmd) {
                        (Some(t), TransportCmd::Write(data)) => t.encode_outgoing(&data),
                        (Some(t), TransportCmd::WriteData(data)) => t.encode_data(&data),
                        (None, TransportCmd::Write(data) | TransportCmd::WriteData(data)) => data,
                        _ => unreachable!(),
                    };
                    if let Err(e) = wr.write_all(&out).await {
                        break format!("Connection lost: {}", super::describe_io_error(&e));
                    }
                }
                Some(TransportCmd::Resize { cols, rows }) => {
                    if let Some(msg) = proto.as_mut().and_then(|t| t.resize(cols, rows)) {
                        let _ = wr.write_all(&msg).await;
                    }
                }
                Some(TransportCmd::Sync(done)) => {
                    let _ = done.send(());
                }
                // Modem-control commands only apply to serial ports.
                Some(_) => {}
            }
        }
    };
    sink.state(LinkState::Disconnected, reason);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::telnet::{DO, IAC, OPT_NAWS, WILL};
    use crate::transport::TransportEvent;
    use tokio::net::TcpListener;

    async fn next_data(rx: &mut mpsc::UnboundedReceiver<TransportEvent>) -> Vec<u8> {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap() {
                TransportEvent::Data(d) => return d,
                _ => continue,
            }
        }
    }

    #[tokio::test]
    async fn raw_tcp_roundtrip_and_remote_close() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            s.write_all(b"hello\r\n").await.unwrap();
            let mut buf = [0u8; 16];
            let n = s.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"ping\r");
            // dropping `s` closes the connection
        });

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let cfg = TcpConfig { host: "127.0.0.1".into(), port };
        tokio::spawn(run(cfg, false, 80, 24, cmd_rx, EventSink::new(ev_tx)));

        assert_eq!(next_data(&mut ev_rx).await, b"hello\r\n");
        cmd_tx.send(TransportCmd::Write(b"ping\r".to_vec())).unwrap();
        server.await.unwrap();

        let mut saw_disconnect = false;
        while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(5), ev_rx.recv()).await {
            if let TransportEvent::State { state: LinkState::Disconnected, .. } = ev {
                saw_disconnect = true;
                break;
            }
        }
        assert!(saw_disconnect);
    }

    #[tokio::test]
    async fn telnet_negotiates_and_strips_iac() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            // Read the client's opening negotiation.
            let mut buf = [0u8; 64];
            let n = s.read(&mut buf).await.unwrap();
            assert!(buf[..n].windows(3).any(|w| w == [IAC, WILL, OPT_NAWS]));
            s.write_all(&[IAC, DO, OPT_NAWS]).await.unwrap();
            s.write_all(b"Welcome\r\n").await.unwrap();
            let n = s.read(&mut buf).await.unwrap();
            // Client should answer DO NAWS with the window size.
            assert_eq!(&buf[..n], &[IAC, 250, OPT_NAWS, 0, 100, 0, 30, IAC, 240]);
        });

        let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let cfg = TcpConfig { host: "127.0.0.1".into(), port };
        tokio::spawn(run(cfg, true, 100, 30, cmd_rx, EventSink::new(ev_tx)));

        assert_eq!(next_data(&mut ev_rx).await, b"Welcome\r\n");
        server.await.unwrap();
    }

    /// ZMODEM through a real Telnet server that does not negotiate BINARY and
    /// passes bytes raw (busybox telnetd, common on routers), against lrzsz.
    /// "0D 00", "0D 0A" and "FF" in the file used to be mangled.
    #[test]
    #[ignore = "needs busybox telnetd and lrzsz (sz/rz)"]
    fn zmodem_over_busybox_telnetd() {
        use std::process::{Command, Stdio};
        use std::sync::atomic::AtomicBool;
        use std::sync::{mpsc as std_mpsc, Arc};

        use crate::transfer::{self, Job, Protocol, TransportLink};

        let dir = std::env::temp_dir().join(format!("tw-telnet-zmodem-{}", std::process::id()));
        let (remote, local) = (dir.join("remote"), dir.join("local"));
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        // Firmware-like noise with the awkward pairs ("CR NUL", "CR LF", "IAC") every 512 bytes.
        let mut data = Vec::new();
        let mut x = 0x2545_f491_4f6c_dd1du64;
        for i in 0..160 * 1024 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            data.push(x as u8);
            if i % 512 == 0 {
                data.extend_from_slice(b"\r\0\r\n\xff\r");
            }
        }
        std::fs::write(remote.join("down.bin"), &data).unwrap();
        std::fs::write(local.join("up.bin"), &data).unwrap();

        // TW_TELNET_PORT: use a server that is already running (e.g. behind a logging proxy).
        let given = std::env::var("TW_TELNET_PORT").ok().and_then(|p| p.parse::<u16>().ok());
        let port =
            given.unwrap_or_else(|| std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port());
        let mut server = given.is_none().then(|| {
            Command::new("busybox")
                .args(["telnetd", "-F", "-b", &format!("127.0.0.1:{port}"), "-l", "/bin/sh"])
                .current_dir(&remote)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("busybox telnetd")
        });
        std::thread::sleep(Duration::from_millis(300));

        let rt = tokio::runtime::Runtime::new().unwrap();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        rt.spawn(run(TcpConfig { host: "127.0.0.1".into(), port }, true, 80, 24, cmd_rx, EventSink::new(ev_tx)));
        let (in_tx, in_rx) = std_mpsc::channel();
        rt.spawn(async move {
            while let Some(ev) = ev_rx.recv().await {
                if let TransportEvent::Data(d) = ev {
                    let _ = in_tx.send(d);
                }
            }
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let mut link = TransportLink::new(in_rx, cmd_tx.clone(), cancel.clone()).telnet(true);
        std::thread::sleep(Duration::from_millis(500));

        // Download: remote `sz`.
        cmd_tx.send(TransportCmd::Write(format!("cd {} && sz down.bin\r", remote.display()).into_bytes())).unwrap();
        let got = transfer::run(
            &Job::Receive { protocol: Protocol::Zmodem, dest: local.clone() },
            &mut link,
            &cancel,
            &mut |_| {},
        );
        let down = std::fs::read(local.join("down.bin")).unwrap_or_default();

        // Upload: remote `rz`.
        std::thread::sleep(Duration::from_millis(300));
        // TW_RZ_CMD: e.g. run rz under strace to see what it actually reads.
        let rz = std::env::var("TW_RZ_CMD").unwrap_or_else(|_| "rz".into());
        cmd_tx.send(TransportCmd::Write(format!("{rz}\r").into_bytes())).unwrap();
        let sent = transfer::run(
            &Job::Send { protocol: Protocol::Zmodem, files: vec![local.join("up.bin")], remote_ready: true },
            &mut link,
            &cancel,
            &mut |_| {},
        );
        std::thread::sleep(Duration::from_millis(300));
        let up = std::fs::read(remote.join("up.bin")).unwrap_or_default();

        let _ = cmd_tx.send(TransportCmd::Close);
        if let Some(server) = server.as_mut() {
            let _ = server.kill();
            let _ = server.wait();
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(got.is_ok(), "download: {:?}", got.err());
        assert!(down == data, "download differs ({} of {} bytes)", down.len(), data.len());
        assert!(sent.is_ok(), "upload: {:?}", sent.err());
        assert!(up == data, "upload differs ({} of {} bytes)", up.len(), data.len());
    }
}
