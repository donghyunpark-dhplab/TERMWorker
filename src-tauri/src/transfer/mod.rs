//! File transfer protocols: XMODEM, YMODEM and ZMODEM, implemented in-house.
//!
//! A transfer takes over a session's byte stream while it runs. The protocol
//! code is plain blocking code on its own thread, talking to the remote side
//! through a [`Link`]. That keeps it independent of the transport (serial,
//! SSH, Telnet, raw TCP) and lets the tests run sender against receiver over
//! an in-memory pipe.

mod crc;
mod link;
mod xymodem;
mod zmodem;

#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub use link::TransportLink;
pub use zmodem::{Detected as ZmodemDetected, Detector as ZmodemDetector};

/// Sent to abort the remote side: CANs end every protocol, backspaces erase
/// them again if the remote was actually sitting at a shell prompt.
pub const ABORT_SEQUENCE: &[u8] = &[
    0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08,
    0x08,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// 128-byte blocks; CRC-16 or checksum, whichever the receiver asks for.
    Xmodem,
    /// 1024-byte blocks (CRC-16 only).
    Xmodem1k,
    Ymodem,
    Zmodem,
}

#[derive(Debug, Clone)]
pub enum Job {
    Send {
        protocol: Protocol,
        files: Vec<PathBuf>,
        /// ZMODEM only: the remote `rz` is already running, so don't type `rz\r`.
        remote_ready: bool,
    },
    Receive {
        protocol: Protocol,
        /// A folder for YMODEM/ZMODEM (the sender names the files), a file for XMODEM.
        dest: PathBuf,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cancelled")]
    Cancelled,
    #[error("cancelled by the remote side")]
    RemoteCancelled,
    #[error("timed out waiting for the remote side")]
    Timeout,
    #[error("{0}")]
    Protocol(String),
    #[error("{0}")]
    File(String),
    #[error("connection closed")]
    LinkClosed,
}

pub type Result<T> = std::result::Result<T, Error>;

fn protocol_err(msg: impl Into<String>) -> Error {
    Error::Protocol(msg.into())
}

fn file_err(path: &Path, e: std::io::Error) -> Error {
    Error::File(format!("{}: {}", path.display(), describe_file_error(&e)))
}

fn describe_file_error(e: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => "file not found".into(),
        PermissionDenied => "permission denied".into(),
        AlreadyExists => "file already exists".into(),
        _ => match e.raw_os_error() {
            Some(code) => format!("I/O error (os error {code})"),
            None => e.to_string(),
        },
    }
}

/// The remote side, as seen by a protocol.
pub trait Link {
    /// Waits up to `timeout` for incoming bytes; an empty result means nothing arrived.
    fn recv(&mut self, timeout: Duration) -> Result<Vec<u8>>;
    /// Queues bytes for sending. May block while too much is still in flight.
    fn send(&mut self, data: &[u8]) -> Result<()>;
    /// Waits until everything sent so far has left.
    fn flush(&mut self) -> Result<()>;
    /// The link mangles some bytes (Telnet: CR and NUL), so protocols that can
    /// escape them should (ZMODEM sends CR as ZDLE 'M' and NUL as ZDLE '@').
    fn mangles_cr(&self) -> bool {
        false
    }
}

/// What a running transfer reports.
#[derive(Debug, Clone, PartialEq)]
pub enum Report {
    /// A file begins. `path` is the local file.
    File { name: String, path: PathBuf, size: Option<u64> },
    /// Bytes of the current file confirmed so far.
    Progress { bytes: u64 },
    /// The current file is complete (or was skipped by the receiver).
    FileDone { bytes: u64, skipped: bool },
}

pub struct Summary {
    pub files: Vec<(String, PathBuf, u64)>,
}

/// Runs a job to completion. On failure the remote side is told to abort.
pub fn run(job: &Job, link: &mut dyn Link, cancel: &AtomicBool, report: &mut dyn FnMut(Report)) -> Result<Summary> {
    let mut ch = Chan::new(link, cancel);
    let mut summary = Summary { files: Vec::new() };
    let mut rep = |r: Report| {
        if let Report::File { name, path, .. } = &r {
            summary.files.push((name.clone(), path.clone(), 0));
        }
        if let Report::FileDone { bytes, .. } = &r {
            if let Some(last) = summary.files.last_mut() {
                last.2 = *bytes;
            }
        }
        report(r);
    };
    let result = match job {
        Job::Send { protocol, files, remote_ready } => match protocol {
            Protocol::Xmodem | Protocol::Xmodem1k => {
                let [file] = files.as_slice() else {
                    return Err(protocol_err("XMODEM sends exactly one file"));
                };
                xymodem::send_xmodem(&mut ch, file, *protocol == Protocol::Xmodem1k, &mut rep)
            }
            Protocol::Ymodem => xymodem::send_ymodem(&mut ch, files, &mut rep),
            Protocol::Zmodem => zmodem::send(&mut ch, files, *remote_ready, &mut rep),
        },
        Job::Receive { protocol, dest } => match protocol {
            Protocol::Xmodem | Protocol::Xmodem1k => xymodem::receive_xmodem(&mut ch, dest, &mut rep),
            Protocol::Ymodem => xymodem::receive_ymodem(&mut ch, dest, &mut rep),
            Protocol::Zmodem => zmodem::receive(&mut ch, dest, &mut rep),
        },
    };
    match result {
        Ok(()) => Ok(summary),
        Err(e) => {
            if !matches!(e, Error::RemoteCancelled | Error::LinkClosed) {
                let _ = ch.link.send(ABORT_SEQUENCE);
                let _ = ch.link.flush();
            }
            Err(e)
        }
    }
}

/// Byte-level access to a [`Link`] with timeouts and cancellation.
pub(crate) struct Chan<'a> {
    link: &'a mut dyn Link,
    cancel: &'a AtomicBool,
    buf: VecDeque<u8>,
}

/// How often a blocked read looks at the cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(100);

impl<'a> Chan<'a> {
    pub fn new(link: &'a mut dyn Link, cancel: &'a AtomicBool) -> Self {
        Self { link, cancel, buf: VecDeque::new() }
    }

    fn check_cancel(&self) -> Result<()> {
        if self.cancel.load(Ordering::SeqCst) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Next byte, or `None` if nothing arrived within `timeout`.
    pub fn byte(&mut self, timeout: Duration) -> Result<Option<u8>> {
        if let Some(b) = self.buf.pop_front() {
            return Ok(Some(b));
        }
        let deadline = Instant::now() + timeout;
        loop {
            self.check_cancel()?;
            let left = deadline.saturating_duration_since(Instant::now());
            let data = self.link.recv(left.min(CANCEL_POLL))?;
            if !data.is_empty() {
                self.buf.extend(data);
                return Ok(self.buf.pop_front());
            }
            if left.is_zero() {
                return Ok(None);
            }
        }
    }

    /// Takes whatever has already arrived, without waiting.
    pub fn poll(&mut self) -> Result<bool> {
        self.check_cancel()?;
        if self.buf.is_empty() {
            let data = self.link.recv(Duration::ZERO)?;
            self.buf.extend(data);
        }
        Ok(!self.buf.is_empty())
    }

    /// Drops input that has already arrived (stale replies, noise), without waiting.
    /// Two CANs in a row among the dropped bytes mean the remote side aborted
    /// (X/YMODEM): without this check an abort that lands between two blocks
    /// is thrown away and the sender keeps retrying until it times out.
    pub fn discard_pending_watching_cancel(&mut self) -> Result<()> {
        const CAN: u8 = 0x18;
        let mut prev_can = false;
        let mut scan = |data: &mut dyn Iterator<Item = u8>| -> bool {
            for b in data {
                if b == CAN && prev_can {
                    return true;
                }
                prev_can = b == CAN;
            }
            false
        };
        let mut cancelled = scan(&mut self.buf.drain(..));
        loop {
            let data = self.link.recv(Duration::ZERO)?;
            if data.is_empty() {
                break;
            }
            cancelled |= scan(&mut data.into_iter());
        }
        if cancelled {
            Err(Error::RemoteCancelled)
        } else {
            Ok(())
        }
    }

    /// Bytes received but not yet read.
    pub fn buffered(&self) -> &VecDeque<u8> {
        &self.buf
    }

    /// Puts a byte back so the next read returns it.
    pub fn unread(&mut self, b: u8) {
        self.buf.push_front(b);
    }

    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        self.check_cancel()?;
        self.link.send(data)
    }

    pub fn flush(&mut self) -> Result<()> {
        self.link.flush()
    }

    pub fn mangles_cr(&self) -> bool {
        self.link.mangles_cr()
    }

    /// Discards input until the line has been quiet for `quiet` (at most `max`).
    pub fn purge(&mut self, quiet: Duration, max: Duration) -> Result<()> {
        self.buf.clear();
        let end = Instant::now() + max;
        while Instant::now() < end {
            if self.byte(quiet)?.is_none() {
                break;
            }
            self.buf.clear();
        }
        Ok(())
    }
}

/// A file name received from the remote side, made safe to create locally:
/// no directories, no characters Windows rejects, no reserved device names.
pub fn safe_file_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut name: String = base
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    name = name.trim_matches(|c| c == ' ' || c == '.').to_string();
    if name.is_empty() {
        name = "received.bin".into();
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        name.insert(0, '_');
    }
    if name.chars().count() > 200 {
        name = name.chars().take(200).collect();
    }
    name
}

/// `dir/name`, or `dir/name (1).ext` etc. if that already exists.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).expect("unbounded search")
}

/// Name sent to the remote side: the file's own name, without any folders.
fn send_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into())
}

fn mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}

fn set_mtime(path: &Path, secs: u64) {
    if secs == 0 {
        return;
    }
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
    }
}
