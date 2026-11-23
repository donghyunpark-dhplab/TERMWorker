//! Transports move bytes between a remote endpoint and a session.
//!
//! Every transport runs as an independent actor: it receives [`TransportCmd`]s
//! on a channel and reports [`TransportEvent`]s through an [`EventSink`].
//! This keeps the session layer (decoding, logging, UI forwarding) identical
//! for Serial, SSH, Telnet and raw TCP – and is where file-transfer protocols
//! (X/Y/ZMODEM) will hook in later.

pub mod serial;
pub mod ssh;
pub mod tcp;
pub mod telnet;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::config::Connection;

#[derive(Debug)]
pub enum TransportCmd {
    /// Keyboard input and other terminal text.
    Write(Vec<u8>),
    /// File-transfer data, which must reach the remote side byte for byte.
    /// Same as `Write` except on Telnet (see `Telnet::encode_data`).
    WriteData(Vec<u8>),
    Resize {
        cols: u16,
        rows: u16,
    },
    SetDtr(bool),
    SetRts(bool),
    SendBreak,
    /// Answered once every earlier command has been carried out (writes handed to
    /// the OS). File transfers use it to keep only a little data in flight.
    Sync(std::sync::mpsc::Sender<()>),
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkState {
    Connecting,
    Connected,
    /// The link dropped and the transport is trying to restore it (e.g. USB-serial unplugged).
    Reconnecting,
    Disconnected,
}

#[derive(Debug)]
pub enum TransportEvent {
    Data(Vec<u8>),
    State {
        state: LinkState,
        message: Option<String>,
    },
    /// Informational line shown in the terminal, not part of the remote output.
    Notice(String),
    HostKey(HostKeyPrompt),
    AuthFailed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyPrompt {
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostKeyDecision {
    AcceptAndSave,
    AcceptOnce,
    Reject,
}

/// Where a pending host-key question waits for the user's answer.
pub type HostKeySlot = Arc<Mutex<Option<oneshot::Sender<HostKeyDecision>>>>;

#[derive(Clone)]
pub struct EventSink(mpsc::UnboundedSender<TransportEvent>);

impl EventSink {
    pub fn new(tx: mpsc::UnboundedSender<TransportEvent>) -> Self {
        Self(tx)
    }

    /// Returns `false` once the session is gone; transports should stop then.
    pub fn send(&self, ev: TransportEvent) -> bool {
        self.0.send(ev).is_ok()
    }

    pub fn data(&self, bytes: Vec<u8>) -> bool {
        self.send(TransportEvent::Data(bytes))
    }

    pub fn state(&self, state: LinkState, message: impl Into<Option<String>>) -> bool {
        self.send(TransportEvent::State { state, message: message.into() })
    }

    pub fn notice(&self, text: impl Into<String>) -> bool {
        self.send(TransportEvent::Notice(text.into()))
    }

    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }
}

/// English description of common network errors. The OS text is localized
/// (e.g. Korean on a Korean Windows install), which reads badly in an English UI.
pub fn describe_io_error(e: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    let text = match e.kind() {
        ConnectionRefused => "connection refused",
        ConnectionReset => "connection reset by the remote host",
        ConnectionAborted => "connection aborted",
        NotConnected => "not connected",
        TimedOut => "timed out",
        AddrNotAvailable => "address not available",
        HostUnreachable => "host unreachable",
        NetworkUnreachable => "network unreachable",
        NetworkDown => "network is down",
        BrokenPipe => "connection closed",
        UnexpectedEof => "connection closed unexpectedly",
        NotFound => "not found",
        PermissionDenied => "permission denied",
        _ => return e.to_string(),
    };
    match e.raw_os_error() {
        Some(code) => format!("{text} (os error {code})"),
        None => text.to_string(),
    }
}

pub struct SpawnContext {
    pub cols: u16,
    pub rows: u16,
    pub known_hosts: PathBuf,
    pub hostkey_slot: HostKeySlot,
    /// Password or key passphrase. Never persisted.
    pub secret: Option<String>,
}

pub fn spawn(conn: Connection, ctx: SpawnContext, cmd_rx: mpsc::UnboundedReceiver<TransportCmd>, sink: EventSink) {
    match conn {
        Connection::Serial(cfg) => serial::spawn(cfg, cmd_rx, sink),
        Connection::Ssh(cfg) => {
            tauri::async_runtime::spawn(ssh::run(cfg, ctx, cmd_rx, sink));
        }
        Connection::Telnet(cfg) => {
            tauri::async_runtime::spawn(tcp::run(cfg, true, ctx.cols, ctx.rows, cmd_rx, sink));
        }
        Connection::Raw(cfg) => {
            tauri::async_runtime::spawn(tcp::run(cfg, false, ctx.cols, ctx.rows, cmd_rx, sink));
        }
    }
}
