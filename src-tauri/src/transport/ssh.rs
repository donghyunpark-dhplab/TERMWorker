//! SSH transport (pure Rust, via `russh`).
//!
//! * Host keys are verified against TermWorker's own `known_hosts` file
//!   (OpenSSH format). Unknown keys are shown to the user with their SHA-256
//!   fingerprint; changed keys are refused outright.
//! * Authentication: password (falling back to keyboard-interactive, which many
//!   servers use for passwords) or a private key file with optional passphrase.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use russh::client::{self, KeyboardInteractiveAuthResponse};
use russh::keys::{self, HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};

use super::{
    EventSink, HostKeyDecision, HostKeyPrompt, HostKeySlot, LinkState, SpawnContext, TransportCmd, TransportEvent,
};
use crate::config::{SshAuth, SshConfig};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const TERM: &str = "xterm-256color";

struct Handler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    sink: EventSink,
    slot: HostKeySlot,
}

impl client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, server_key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key: PublicKey = match server_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(cert) => PublicKey::from(cert.public_key().clone()),
        };

        match keys::check_known_hosts_path(&self.host, self.port, &key, &self.known_hosts) {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(keys::Error::KeyChanged { line }) => {
                self.sink.notice(format!(
                    "WARNING: the host key for {}:{} has CHANGED. This may be a man-in-the-middle attack.\r\n\
                     If the server was legitimately reinstalled, remove line {line} of {} and reconnect.",
                    self.host,
                    self.port,
                    self.known_hosts.display()
                ));
                return Ok(false);
            }
            // Missing or unreadable file: treat the host as unknown.
            Err(_) => {}
        }

        let (tx, rx) = oneshot::channel();
        *self.slot.lock().unwrap() = Some(tx);
        self.sink.send(TransportEvent::HostKey(HostKeyPrompt {
            host: self.host.clone(),
            port: self.port,
            algorithm: key.algorithm().as_str().to_string(),
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
        }));

        match rx.await {
            Ok(HostKeyDecision::AcceptAndSave) => {
                if let Err(e) =
                    keys::known_hosts::learn_known_hosts_path(&self.host, self.port, &key, &self.known_hosts)
                {
                    self.sink.notice(format!("Could not save host key: {e}"));
                }
                Ok(true)
            }
            Ok(HostKeyDecision::AcceptOnce) => Ok(true),
            Ok(HostKeyDecision::Reject) | Err(_) => Ok(false),
        }
    }

    async fn auth_banner(&mut self, banner: &str, _session: &mut client::Session) -> Result<(), Self::Error> {
        let text = banner.replace("\r\n", "\n").replace('\n', "\r\n");
        self.sink.data(text.into_bytes());
        Ok(())
    }
}

pub async fn run(cfg: SshConfig, ctx: SpawnContext, cmd_rx: mpsc::UnboundedReceiver<TransportCmd>, sink: EventSink) {
    let SpawnContext { cols, rows, known_hosts, hostkey_slot, secret } = ctx;
    sink.state(LinkState::Connecting, format!("Connecting to {}@{}:{}…", cfg.username, cfg.host, cfg.port));

    let handle = match connect(&cfg, known_hosts, hostkey_slot, secret, &sink).await {
        Ok(h) => h,
        Err(msg) => {
            sink.state(LinkState::Disconnected, msg);
            return;
        }
    };

    if let Some(reason) = shell(&handle, &cfg, cols, rows, cmd_rx, &sink).await {
        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        sink.state(LinkState::Disconnected, reason);
    } else {
        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
    }
}

async fn connect(
    cfg: &SshConfig,
    known_hosts: PathBuf,
    slot: HostKeySlot,
    secret: Option<String>,
    sink: &EventSink,
) -> Result<client::Handle<Handler>, String> {
    let target = format!("{}:{}", cfg.host, cfg.port);
    let stream = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((cfg.host.as_str(), cfg.port))).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(format!("Could not connect to {target}: {}", super::describe_io_error(&e))),
        Err(_) => return Err(format!("Could not connect to {target}: timed out")),
    };
    let _ = stream.set_nodelay(true);

    let config = Arc::new(client::Config {
        keepalive_interval: (cfg.keepalive_secs > 0).then(|| Duration::from_secs(cfg.keepalive_secs as u64)),
        keepalive_max: 3,
        inactivity_timeout: None,
        nodelay: true,
        ..Default::default()
    });
    let handler = Handler { host: cfg.host.clone(), port: cfg.port, known_hosts, sink: sink.clone(), slot };

    let mut handle = client::connect_stream(config, stream, handler).await.map_err(|e| match e {
        russh::Error::UnknownKey => "Host key was not accepted. Connection closed.".to_string(),
        other => format!("SSH handshake with {target} failed: {other}"),
    })?;

    let user = cfg.username.clone();
    let authenticated = match cfg.auth {
        SshAuth::PublicKey => {
            let path =
                cfg.key_path.as_deref().filter(|p| !p.trim().is_empty()).ok_or("No private key file selected.")?;
            let passphrase = secret.as_deref().filter(|s| !s.is_empty());
            let key = match keys::load_secret_key(path, passphrase) {
                Ok(k) => k,
                Err(keys::Error::KeyIsEncrypted) => {
                    sink.send(TransportEvent::AuthFailed);
                    return Err("The private key is encrypted. Enter its passphrase.".into());
                }
                Err(e) => return Err(format!("Cannot load private key {path}: {e}")),
            };
            let hash = handle.best_supported_rsa_hash().await.map_err(|e| e.to_string())?.flatten();
            handle
                .authenticate_publickey(&user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(|e| e.to_string())?
                .success()
        }
        SshAuth::Password => {
            let password = secret.unwrap_or_default();
            let ok = handle.authenticate_password(&user, &password).await.map_err(|e| e.to_string())?.success();
            ok || keyboard_interactive(&mut handle, &user, &password).await?
        }
    };

    if !authenticated {
        sink.send(TransportEvent::AuthFailed);
        return Err(format!("Authentication failed for {user}@{}", cfg.host));
    }
    Ok(handle)
}

/// Answer keyboard-interactive prompts with the password (covers PAM-based password logins).
async fn keyboard_interactive(
    handle: &mut client::Handle<Handler>,
    user: &str,
    password: &str,
) -> Result<bool, String> {
    let mut resp =
        handle.authenticate_keyboard_interactive_start(user, None::<String>).await.map_err(|e| e.to_string())?;
    for _ in 0..5 {
        match resp {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                let answers =
                    prompts.iter().map(|p| if p.echo { String::new() } else { password.to_string() }).collect();
                resp = handle.authenticate_keyboard_interactive_respond(answers).await.map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(false)
}

/// Run an interactive shell. Returns the disconnect reason, or `None` if the user closed the session.
async fn shell(
    handle: &client::Handle<Handler>,
    cfg: &SshConfig,
    cols: u16,
    rows: u16,
    mut cmd_rx: mpsc::UnboundedReceiver<TransportCmd>,
    sink: &EventSink,
) -> Option<String> {
    let channel = match handle.channel_open_session().await {
        Ok(c) => c,
        Err(e) => return Some(format!("Could not open a session channel: {e}")),
    };
    if let Err(e) = channel.request_pty(false, TERM, cols as u32, rows as u32, 0, 0, &[]).await {
        return Some(format!("PTY request failed: {e}"));
    }
    if let Err(e) = channel.request_shell(false).await {
        return Some(format!("Shell request failed: {e}"));
    }
    sink.state(LinkState::Connected, format!("{}@{}:{}", cfg.username, cfg.host, cfg.port));

    let (mut rd, wr) = channel.split();
    let mut exit_status: Option<u32> = None;
    loop {
        tokio::select! {
            msg = rd.wait() => match msg {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    if !sink.data(data.to_vec()) {
                        return None;
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status: code }) => exit_status = Some(code),
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            },
            cmd = cmd_rx.recv() => match cmd {
                None | Some(TransportCmd::Close) => return None,
                Some(TransportCmd::Write(data) | TransportCmd::WriteData(data)) => {
                    if let Err(e) = wr.data_bytes(data).await {
                        return Some(format!("Connection lost: {e}"));
                    }
                }
                Some(TransportCmd::Resize { cols, rows }) => {
                    let _ = wr.window_change(cols as u32, rows as u32, 0, 0).await;
                }
                Some(TransportCmd::Sync(done)) => {
                    let _ = done.send(());
                }
                Some(_) => {}
            }
        }
    }
    Some(match exit_status {
        Some(code) => format!("Remote shell exited with status {code}"),
        None => "Connection closed by remote host".into(),
    })
}

#[cfg(test)]
#[path = "ssh_tests.rs"]
mod tests;
