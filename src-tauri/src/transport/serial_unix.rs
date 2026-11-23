//! macOS / Linux serial I/O through the `serialport` crate.
//!
//! A tty file descriptor can be read and written from two threads at once, so
//! the reader and the writer simply use two clones of the port.
//!
//! Writes do not go through `serialport`'s `Write`: it polls until *some* room
//! is free and then makes a blocking `write()` of the whole buffer, which never
//! returns while flow control (CTS low, XOFF) holds the line. Its `flush()`
//! (`tcdrain`) blocks the same way. Instead the descriptor is non-blocking and
//! the writer polls in short steps, so a stall is reported and Close is honoured.

use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serialport::{SerialPort, TTYPort};

use super::{serialport_builder, WRITE_STALL_TIMEOUT};
use crate::config::SerialConfig;

/// How often a blocked write looks at the abort flag.
const WRITE_POLL: Duration = Duration::from_millis(100);

pub struct Port {
    rx: Mutex<TTYPort>,
    tx: Mutex<TTYPort>,
    /// The descriptor shared by both clones (same open file description), for
    /// calls that do not need either lock.
    fd: RawFd,
}

impl Port {
    pub fn open(cfg: &SerialConfig, read_timeout: Duration) -> io::Result<Port> {
        let rx = serialport_builder(cfg, read_timeout).open_native().map_err(to_io)?;
        let tx = rx.try_clone_native().map_err(to_io)?;
        let fd = tx.as_raw_fd();
        // Both clones share the file status flags. Reads still wait in poll() first.
        set_nonblocking(fd)?;
        Ok(Port { rx: Mutex::new(rx), tx: Mutex::new(tx), fd })
    }

    /// Returns `Ok(0)` when nothing arrived within the read timeout.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        match self.rx.lock().unwrap().read(buf) {
            Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => Ok(0),
            other => other,
        }
    }

    /// Writes everything, unless `abort` gets set or the line makes no progress
    /// for [`WRITE_STALL_TIMEOUT`] (then `TimedOut`).
    pub fn write(&self, mut data: &[u8], abort: &AtomicBool) -> io::Result<()> {
        let tx = self.tx.lock().unwrap();
        let fd = tx.as_raw_fd();
        let mut last_progress = Instant::now();
        while !data.is_empty() {
            if abort.load(Ordering::SeqCst) {
                return Ok(());
            }
            if last_progress.elapsed() >= WRITE_STALL_TIMEOUT {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "transmit stalled"));
            }
            if !poll_writable(fd, WRITE_POLL)? {
                continue;
            }
            // SAFETY: `fd` stays open while `tx` is locked; `data` is a valid buffer.
            let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
            if n < 0 {
                let e = io::Error::last_os_error();
                match e.kind() {
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => continue,
                    _ => return Err(e),
                }
            }
            if n > 0 {
                data = &data[n as usize..];
                last_progress = Instant::now();
            }
        }
        Ok(())
    }

    pub fn set_dtr(&self, on: bool) -> io::Result<()> {
        self.tx.lock().unwrap().write_data_terminal_ready(on).map_err(to_io)
    }

    pub fn set_rts(&self, on: bool) -> io::Result<()> {
        self.tx.lock().unwrap().write_request_to_send(on).map_err(to_io)
    }

    pub fn set_break(&self, on: bool) -> io::Result<()> {
        let port = self.tx.lock().unwrap();
        let result = if on { port.set_break() } else { port.clear_break() };
        result.map_err(to_io)
    }

    /// Called when the port is about to be closed. Blocking calls end on their
    /// own within the read timeout or [`WRITE_POLL`].
    ///
    /// A tty's `close()` waits for queued output to drain: up to 30 s on Linux
    /// usb-serial (`closing_wait`, which only root may change) if flow control
    /// holds the line. So the driver's queue is discarded, and flow control is
    /// switched off so the few hundred bytes already inside the adapter chip
    /// can leave (tcflush cannot reach those).
    pub fn cancel_io(&self) {
        // SAFETY: `fd` is open for as long as `self` exists; `t` is fully written by tcgetattr.
        unsafe {
            libc::tcflush(self.fd, libc::TCOFLUSH);
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(self.fd, &mut t) == 0 {
                t.c_cflag &= !libc::CRTSCTS;
                t.c_iflag &= !(libc::IXON | libc::IXOFF | libc::IXANY);
                libc::tcsetattr(self.fd, libc::TCSANOW, &t);
            }
        }
    }
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: plain fcntl calls on a descriptor we own.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Waits up to `timeout` for room in the output queue.
fn poll_writable(fd: RawFd, timeout: Duration) -> io::Result<bool> {
    let mut pfd = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
    // SAFETY: one valid pollfd.
    let r = unsafe { libc::poll(&mut pfd, 1, timeout.as_millis() as libc::c_int) };
    if r < 0 {
        let e = io::Error::last_os_error();
        return if e.kind() == io::ErrorKind::Interrupted { Ok(false) } else { Err(e) };
    }
    if r == 0 {
        return Ok(false);
    }
    if pfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(io::Error::new(io::ErrorKind::BrokenPipe, "device stopped responding"));
    }
    Ok(pfd.revents & libc::POLLOUT != 0)
}

fn to_io(e: serialport::Error) -> io::Error {
    let kind = match e.kind() {
        // On Linux and macOS `serialport` reports EBUSY (exclusive lock held elsewhere) as NoDevice.
        serialport::ErrorKind::NoDevice => io::ErrorKind::ResourceBusy,
        serialport::ErrorKind::InvalidInput => io::ErrorKind::InvalidInput,
        serialport::ErrorKind::Io(kind) => kind,
        serialport::ErrorKind::Unknown => io::ErrorKind::Other,
    };
    io::Error::new(kind, e.description)
}

/// Other programs that have `port` open, e.g. `["screen (pid 4242)"]`.
///
/// Unlike Windows, Linux lets several programs open one serial port unless the
/// first one took an exclusive lock (TermWorker does; screen, cat or a Python
/// script usually do not). Both then get part of the incoming data each, so the
/// user should be told. Looks at the open files of processes we may inspect
/// (normally our own user's) and at UUCP lock files (minicom, picocom).
#[cfg(target_os = "linux")]
pub fn other_users(port: &str) -> Vec<String> {
    use std::path::Path;

    let Ok(dev) = std::fs::canonicalize(port) else { return Vec::new() };
    let me = std::process::id();
    let mut pids = std::collections::BTreeSet::new();

    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        if pid == me {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else { continue };
        if fds.flatten().any(|fd| std::fs::read_link(fd.path()).is_ok_and(|t| t == dev)) {
            pids.insert(pid);
        }
    }

    if let Some(name) = dev.file_name().and_then(|n| n.to_str()) {
        for dir in ["/var/lock", "/run/lock"] {
            let Ok(text) = std::fs::read_to_string(Path::new(dir).join(format!("LCK..{name}"))) else { continue };
            if let Ok(pid) = text.trim().parse::<u32>() {
                if pid != me && Path::new(&format!("/proc/{pid}")).exists() {
                    pids.insert(pid);
                }
            }
        }
    }

    pids.into_iter()
        .map(|pid| {
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            let comm = comm.trim();
            if comm.is_empty() {
                format!("pid {pid}")
            } else {
                format!("{comm} (pid {pid})")
            }
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
pub fn other_users(_port: &str) -> Vec<String> {
    Vec::new()
}
