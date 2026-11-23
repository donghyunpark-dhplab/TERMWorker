//! End-to-end tests of the SSH transport against an in-process russh server.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::keys::PrivateKey;
use russh::server::{self, Auth, Msg, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use super::run;
use crate::config::{SshAuth, SshConfig};
use crate::transport::{
    EventSink, HostKeyDecision, HostKeySlot, LinkState, SpawnContext, TransportCmd, TransportEvent,
};

const HOST_KEY: &str = include_str!("../../tests/fixtures/test_host_ed25519");

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name).display().to_string()
}

#[derive(Clone)]
struct TestServer;

impl server::Handler for TestServer {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == "tester" && password == "pw" { Auth::Accept } else { Auth::reject() })
    }

    async fn auth_publickey(&mut self, user: &str, _key: VerifiedPublicKey<'_>) -> Result<Auth, Self::Error> {
        Ok(if user == "keyuser" { Auth::Accept } else { Auth::reject() })
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        session.data(channel, b"WELCOME\r\n".to_vec())?;
        Ok(())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        let reply = format!("echo:{}", String::from_utf8_lossy(data));
        session.data(channel, reply.into_bytes())?;
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        cols: u32,
        rows: u32,
        _pw: u32,
        _ph: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, format!("resized:{cols}x{rows}").into_bytes())?;
        Ok(())
    }
}

async fn start_server() -> u16 {
    let config = Arc::new(server::Config {
        keys: vec![PrivateKey::from_openssh(HOST_KEY).unwrap()],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let config = config.clone();
            tokio::spawn(async move {
                if let Ok(running) = server::run_stream(config, stream, TestServer).await {
                    let _ = running.await;
                }
            });
        }
    });
    port
}

struct Client {
    cmd: mpsc::UnboundedSender<TransportCmd>,
    events: mpsc::UnboundedReceiver<TransportEvent>,
    slot: HostKeySlot,
}

fn connect(
    port: u16,
    user: &str,
    auth: SshAuth,
    key: Option<String>,
    secret: Option<&str>,
    known_hosts: &PathBuf,
) -> Client {
    let (cmd, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, events) = mpsc::unbounded_channel();
    let slot: HostKeySlot = Arc::new(Mutex::new(None));
    let cfg =
        SshConfig { host: "127.0.0.1".into(), port, username: user.into(), auth, key_path: key, keepalive_secs: 0 };
    let ctx = SpawnContext {
        cols: 80,
        rows: 24,
        known_hosts: known_hosts.clone(),
        hostkey_slot: slot.clone(),
        secret: secret.map(str::to_string),
    };
    tokio::spawn(run(cfg, ctx, cmd_rx, EventSink::new(ev_tx)));
    Client { cmd, events, slot }
}

impl Client {
    async fn next(&mut self) -> TransportEvent {
        tokio::time::timeout(Duration::from_secs(10), self.events.recv())
            .await
            .expect("timed out waiting for an event")
            .expect("transport ended")
    }

    /// Wait for an event matching `pred`, answering host-key prompts with `decision`.
    async fn until(
        &mut self,
        decision: HostKeyDecision,
        pred: impl Fn(&TransportEvent) -> bool,
    ) -> (TransportEvent, bool) {
        let mut prompted = false;
        loop {
            let ev = self.next().await;
            if let TransportEvent::HostKey(_) = ev {
                prompted = true;
                let tx = self.slot.lock().unwrap().take().expect("prompt without slot");
                tx.send(decision).unwrap();
                continue;
            }
            if pred(&ev) {
                return (ev, prompted);
            }
        }
    }

    async fn until_data(&mut self, needle: &str) {
        let mut seen = String::new();
        loop {
            if let TransportEvent::Data(d) = self.next().await {
                seen.push_str(&String::from_utf8_lossy(&d));
                if seen.contains(needle) {
                    return;
                }
            }
        }
    }
}

fn is_state(ev: &TransportEvent, want: LinkState) -> bool {
    matches!(ev, TransportEvent::State { state, .. } if *state == want)
}

fn temp_known_hosts(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("termworker-kh-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

#[tokio::test]
async fn password_login_shell_io_resize_and_trust_on_first_use() {
    let port = start_server().await;
    let kh = temp_known_hosts("tofu");

    let mut c = connect(port, "tester", SshAuth::Password, None, Some("pw"), &kh);
    let (_, prompted) = c.until(HostKeyDecision::AcceptAndSave, |e| is_state(e, LinkState::Connected)).await;
    assert!(prompted, "first connection must ask about the unknown host key");
    c.until_data("WELCOME").await;

    c.cmd.send(TransportCmd::Write(b"hi".to_vec())).unwrap();
    c.until_data("echo:hi").await;
    c.cmd.send(TransportCmd::Resize { cols: 132, rows: 43 }).unwrap();
    c.until_data("resized:132x43").await;
    c.cmd.send(TransportCmd::Close).unwrap();

    // Second connection: key is now known, so no prompt.
    let mut c2 = connect(port, "tester", SshAuth::Password, None, Some("pw"), &kh);
    let (_, prompted) = c2.until(HostKeyDecision::Reject, |e| is_state(e, LinkState::Connected)).await;
    assert!(!prompted, "known host must not prompt again");
    let _ = std::fs::remove_file(&kh);
}

#[tokio::test]
async fn wrong_password_reports_auth_failure() {
    let port = start_server().await;
    let kh = temp_known_hosts("badpw");
    let mut c = connect(port, "tester", SshAuth::Password, None, Some("nope"), &kh);
    c.until(HostKeyDecision::AcceptOnce, |e| matches!(e, TransportEvent::AuthFailed)).await;
    c.until(HostKeyDecision::Reject, |e| is_state(e, LinkState::Disconnected)).await;
}

#[tokio::test]
async fn rejecting_host_key_aborts() {
    let port = start_server().await;
    let kh = temp_known_hosts("reject");
    let mut c = connect(port, "tester", SshAuth::Password, None, Some("pw"), &kh);
    let (ev, prompted) = c.until(HostKeyDecision::Reject, |e| is_state(e, LinkState::Disconnected)).await;
    assert!(prompted);
    match ev {
        TransportEvent::State { message: Some(m), .. } => assert!(m.contains("not accepted"), "{m}"),
        other => panic!("unexpected {other:?}"),
    }
    assert!(!kh.exists(), "rejected key must not be saved");
}

#[tokio::test]
async fn encrypted_private_key_needs_passphrase() {
    let port = start_server().await;
    let kh = temp_known_hosts("key");
    let key = fixture("test_user_ed25519_encrypted");

    let mut c = connect(port, "keyuser", SshAuth::PublicKey, Some(key.clone()), None, &kh);
    c.until(HostKeyDecision::AcceptOnce, |e| matches!(e, TransportEvent::AuthFailed)).await;

    let mut c = connect(port, "keyuser", SshAuth::PublicKey, Some(key), Some("secret-pass"), &kh);
    c.until(HostKeyDecision::AcceptOnce, |e| is_state(e, LinkState::Connected)).await;
    c.until_data("WELCOME").await;
}

#[tokio::test]
async fn changed_host_key_is_refused_without_prompt() {
    let port = start_server().await;
    let kh = temp_known_hosts("changed");
    // Record a different ed25519 key for this host:port.
    std::fs::write(
        &kh,
        format!(
            "[127.0.0.1]:{port} ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMYLaCfo3vCprAGC2cetvcF3Ae7xmiM0biLr4Uj4+CJv\n"
        ),
    )
    .unwrap();
    let mut c = connect(port, "tester", SshAuth::Password, None, Some("pw"), &kh);
    let (mut warned, mut prompted) = (false, false);
    loop {
        match c.next().await {
            TransportEvent::Notice(n) if n.contains("CHANGED") => warned = true,
            TransportEvent::HostKey(_) => prompted = true,
            TransportEvent::State { state: LinkState::Disconnected, .. } => break,
            _ => {}
        }
    }
    assert!(warned, "user must be warned about the changed key");
    assert!(!prompted, "a changed key must never be offered for acceptance");
    assert_eq!(std::fs::read_to_string(&kh).unwrap().lines().count(), 1, "known_hosts must be untouched");
    let _ = std::fs::remove_file(&kh);
}
