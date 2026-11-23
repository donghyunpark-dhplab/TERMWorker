//! Connection and terminal settings shared between the frontend and the backend.
//!
//! These types are deserialized from the frontend on every `session_open` call.
//! Saved profiles are stored as opaque JSON (see `profiles.rs`) so the UI can
//! evolve its schema freely; only the fields below are interpreted by Rust.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Connection {
    Serial(SerialConfig),
    Ssh(SshConfig),
    Telnet(TcpConfig),
    Raw(TcpConfig),
}

impl Connection {
    pub fn describe(&self) -> String {
        match self {
            Connection::Serial(c) => format!("{} @ {}", c.port, c.baud_rate),
            Connection::Ssh(c) => format!("{}@{}:{}", c.username, c.host, c.port),
            Connection::Telnet(c) | Connection::Raw(c) => format!("{}:{}", c.host, c.port),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SerialConfig {
    pub port: String,
    pub baud_rate: u32,
    #[serde(default = "default_data_bits")]
    pub data_bits: u8,
    #[serde(default)]
    pub parity: Parity,
    #[serde(default = "default_stop_bits")]
    pub stop_bits: u8,
    #[serde(default)]
    pub flow_control: FlowControl,
    /// Keep the tab alive and reopen the port when a USB-serial adapter is unplugged and replugged.
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
    /// Initial DTR line state (asserted by default, like most terminal programs).
    #[serde(default = "default_true")]
    pub dtr: bool,
    /// Initial RTS line state. Ignored with hardware flow control, where the driver owns RTS.
    #[serde(default = "default_true")]
    pub rts: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Parity {
    #[default]
    None,
    Odd,
    Even,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlowControl {
    #[default]
    None,
    /// XON/XOFF
    Software,
    /// RTS/CTS
    Hardware,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConfig {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub auth: SshAuth,
    #[serde(default)]
    pub key_path: Option<String>,
    /// Seconds between keepalive packets; 0 disables keepalive.
    #[serde(default = "default_keepalive")]
    pub keepalive_secs: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SshAuth {
    #[default]
    Password,
    PublicKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TcpConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOptions {
    /// WHATWG encoding label, e.g. "utf-8", "euc-kr", "gb18030".
    #[serde(default = "default_encoding")]
    pub encoding: String,
    /// What the Enter key transmits.
    #[serde(default)]
    pub enter_sends: Newline,
}

impl Default for TerminalOptions {
    fn default() -> Self {
        Self { encoding: default_encoding(), enter_sends: Newline::default() }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Newline {
    #[default]
    Cr,
    Lf,
    Crlf,
}

impl Newline {
    pub fn as_bytes(self) -> &'static [u8] {
        match self {
            Newline::Cr => b"\r",
            Newline::Lf => b"\n",
            Newline::Crlf => b"\r\n",
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_data_bits() -> u8 {
    8
}
fn default_stop_bits() -> u8 {
    1
}
fn default_ssh_port() -> u16 {
    22
}
fn default_keepalive() -> u32 {
    30
}
fn default_encoding() -> String {
    "utf-8".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontend_connection_json() {
        let c: Connection = serde_json::from_str(
            r#"{"kind":"serial","port":"COM5","baudRate":115200,"parity":"even","flowControl":"hardware"}"#,
        )
        .unwrap();
        match c {
            Connection::Serial(s) => {
                assert_eq!(s.port, "COM5");
                assert_eq!(s.baud_rate, 115200);
                assert_eq!(s.data_bits, 8);
                assert_eq!(s.parity, Parity::Even);
                assert_eq!(s.flow_control, FlowControl::Hardware);
                assert!(s.auto_reconnect);
            }
            _ => panic!("wrong kind"),
        }

        let c: Connection = serde_json::from_str(
            r#"{"kind":"ssh","host":"10.0.0.1","username":"root","auth":"publicKey","keyPath":"C:/id_ed25519"}"#,
        )
        .unwrap();
        assert!(matches!(c, Connection::Ssh(SshConfig { port: 22, auth: SshAuth::PublicKey, .. })));

        let o: TerminalOptions = serde_json::from_str(r#"{"enterSends":"crlf","localEcho":true}"#).unwrap();
        assert_eq!(o.enter_sends, Newline::Crlf);
        assert_eq!(o.encoding, "utf-8");
    }
}
