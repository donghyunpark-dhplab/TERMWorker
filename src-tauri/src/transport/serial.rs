//! Serial port transport.
//!
//! Each session uses two OS threads: a reader (which also owns reconnection)
//! and a writer/command thread, sharing one [`Port`]. When a USB-serial adapter
//! disappears and `auto_reconnect` is enabled the tab stays open, waits for the
//! same port name to come back, reopens it and restores the DTR/RTS lines the
//! user had set.
//!
//! Port I/O is platform specific: Windows uses overlapped I/O (`serial_win.rs`),
//! other systems the `serialport` crate (`serial_unix.rs`). Port enumeration
//! uses `serialport` everywhere.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serialport::{SerialPort, SerialPortType};
use tokio::sync::mpsc;

use super::{EventSink, LinkState, TransportCmd};
use crate::config::{FlowControl, Parity, SerialConfig};

#[cfg(windows)]
#[path = "serial_win.rs"]
mod sys;
#[cfg(not(windows))]
#[path = "serial_unix.rs"]
mod sys;

use sys::Port;

/// How long a read waits for data before the reader checks for shutdown.
const READ_TIMEOUT: Duration = Duration::from_millis(50);
const REOPEN_INTERVAL: Duration = Duration::from_millis(500);
const PRESENCE_CHECK_INTERVAL: Duration = Duration::from_secs(2);
const BREAK_DURATION: Duration = Duration::from_millis(250);
/// A write that makes no progress for this long is abandoned (CTS low, XOFF held).
const WRITE_STALL_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortInfo {
    pub name: String,
    /// Human-friendly label, e.g. "Silicon Labs CP210x USB to UART Bridge".
    pub description: String,
    pub kind: &'static str,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub serial_number: Option<String>,
}

pub fn list_ports() -> Vec<PortInfo> {
    let mut ports: Vec<PortInfo> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !is_placeholder_uart(&p.port_name))
        .map(|p| {
            let (kind, description, vid, pid, serial_number) = match p.port_type {
                SerialPortType::UsbPort(u) => {
                    let desc = match (&u.manufacturer, &u.product) {
                        (_, Some(prod)) => prod.clone(),
                        (Some(m), None) => format!("{m} USB serial"),
                        _ => "USB serial".to_string(),
                    };
                    ("usb", desc, Some(u.vid), Some(u.pid), u.serial_number)
                }
                SerialPortType::PciPort => ("pci", "PCI serial port".into(), None, None, None),
                SerialPortType::BluetoothPort => ("bluetooth", "Bluetooth serial".into(), None, None, None),
                SerialPortType::Unknown => ("unknown", "Serial port".into(), None, None, None),
            };
            let description = strip_port_suffix(&description, &p.port_name);
            PortInfo { name: p.port_name, description, kind, vid, pid, serial_number }
        })
        .collect();
    // Outside Windows, built-in /dev/ttyS* names sort before /dev/ttyUSB*; list
    // USB adapters first, since those are what people plug in (and the first
    // entry is the default port for a new session).
    let usb_last = |p: &PortInfo| cfg!(not(windows)) && p.kind != "usb";
    ports.sort_by_key(|p| (usb_last(p), natural_key(&p.name)));
    ports.dedup_by(|a, b| a.name == b.name);
    ports
}

/// Linux creates /dev/ttyS0..N for every possible 8250 UART, whether or not the
/// hardware exists (often 32 of them). sysfs reports type 0 (PORT_UNKNOWN) for
/// the empty slots.
fn is_placeholder_uart(name: &str) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let Some(dev) = name.strip_prefix("/dev/").filter(|d| d.starts_with("ttyS")) else {
        return false;
    };
    std::fs::read_to_string(format!("/sys/class/tty/{dev}/type")).is_ok_and(|t| t.trim() == "0")
}

/// Windows product names end with the port, e.g. "USB Serial Port (COM4)"; the UI shows the port already.
fn strip_port_suffix(desc: &str, port: &str) -> String {
    let suffix = format!("({port})");
    match desc.strip_suffix(&suffix) {
        Some(rest) if !rest.trim().is_empty() => rest.trim_end().to_string(),
        _ => desc.to_string(),
    }
}

/// Sort "COM2" before "COM10".
fn natural_key(name: &str) -> (String, u32) {
    let digits: String = name.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
    let prefix = &name[..name.len() - digits.len()];
    let num = digits.chars().rev().collect::<String>().parse().unwrap_or(0);
    (prefix.to_string(), num)
}

fn port_present(name: &str) -> bool {
    serialport::available_ports()
        .map(|ports| ports.iter().any(|p| p.port_name.eq_ignore_ascii_case(name)))
        .unwrap_or(true)
}

/// Like [`port_present`] but returns `false` when enumeration itself fails.
fn port_present_strict(name: &str) -> bool {
    serialport::available_ports()
        .map(|ports| ports.iter().any(|p| p.port_name.eq_ignore_ascii_case(name)))
        .unwrap_or(false)
}

/// Opens a port through the `serialport` crate (the test peer).
#[cfg_attr(not(test), allow(dead_code))]
fn open_serialport(cfg: &SerialConfig, timeout: Duration) -> serialport::Result<Box<dyn SerialPort>> {
    serialport_builder(cfg, timeout).open()
}

/// Port settings for the `serialport` crate (non-Windows I/O, and the test peer).
#[cfg_attr(all(windows, not(test)), allow(dead_code))]
fn serialport_builder(cfg: &SerialConfig, timeout: Duration) -> serialport::SerialPortBuilder {
    use serialport::{DataBits, StopBits};
    serialport::new(&cfg.port, cfg.baud_rate)
        .data_bits(match cfg.data_bits {
            5 => DataBits::Five,
            6 => DataBits::Six,
            7 => DataBits::Seven,
            _ => DataBits::Eight,
        })
        .parity(match cfg.parity {
            Parity::None => serialport::Parity::None,
            Parity::Odd => serialport::Parity::Odd,
            Parity::Even => serialport::Parity::Even,
        })
        .stop_bits(if cfg.stop_bits == 2 { StopBits::Two } else { StopBits::One })
        .flow_control(match cfg.flow_control {
            FlowControl::None => serialport::FlowControl::None,
            FlowControl::Software => serialport::FlowControl::Software,
            FlowControl::Hardware => serialport::FlowControl::Hardware,
        })
        .timeout(timeout)
}

/// `write_all` for a `serialport` port: tolerates its short write timeouts, but
/// gives up if the port makes no progress (e.g. hardware flow control with CTS held low).
/// Used by the test peer; the app's own ports write through `serial_win` / `serial_unix`.
#[cfg_attr(not(test), allow(dead_code))]
fn write_fully(port: &mut dyn SerialPort, mut data: &[u8], closed: &AtomicBool) -> io::Result<()> {
    let mut last_progress = Instant::now();
    while !data.is_empty() {
        if closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        if last_progress.elapsed() >= WRITE_STALL_TIMEOUT {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "transmit stalled"));
        }
        match port.write(data) {
            Ok(0) => {}
            Ok(n) => {
                data = &data[n..];
                last_progress = Instant::now();
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => {}
            Err(e) => return Err(e),
        }
    }
    port.flush()
}

fn describe_open_error(cfg: &SerialConfig, e: &io::Error) -> String {
    let reason = match e.raw_os_error() {
        Some(2 | 3 | 123 | 433 | 1167) if cfg!(windows) => "port not found".to_string(),
        Some(5 | 32) if cfg!(windows) => "the port is in use by another application".to_string(),
        _ if e.kind() == io::ErrorKind::NotFound => "port not found".to_string(),
        _ if e.kind() == io::ErrorKind::ResourceBusy => "the port is in use by another application".to_string(),
        _ if e.kind() == io::ErrorKind::PermissionDenied && cfg!(target_os = "linux") => {
            "permission denied. Add your user to the \"dialout\" group (sudo usermod -aG dialout $USER), then log out and back in"
                .to_string()
        }
        _ => describe_port_error(e),
    };
    format!("Cannot open {}: {reason}", cfg.port)
}

/// Why a connected port stopped working, in the same words on every OS:
/// "the device was unplugged" when the error is one that an unplugged USB
/// adapter produces (or the port has left the device list), otherwise the error.
fn disconnect_reason(e: &io::Error, gone_from_list: bool) -> String {
    if gone_from_list || is_unplug_error(e) {
        "the device was unplugged".into()
    } else {
        format!("connection lost ({})", describe_port_error(e))
    }
}

/// Errors a USB-serial adapter returns once it has been unplugged.
fn is_unplug_error(e: &io::Error) -> bool {
    if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::BrokenPipe) {
        return true;
    }
    match e.raw_os_error() {
        // FILE_NOT_FOUND, BAD_COMMAND, GEN_FAILURE, IO_DEVICE, DEVICE_NOT_CONNECTED
        Some(code) if cfg!(windows) => matches!(code, 2 | 22 | 31 | 1117 | 1167),
        // EIO, ENXIO, ENODEV
        Some(code) => matches!(code, 5 | 6 | 19),
        None => false,
    }
}

/// English text for a port error. Windows' own text is localized (e.g. Korean),
/// which reads badly in an English UI, so only the error code is kept.
fn describe_port_error(e: &io::Error) -> String {
    let Some(code) = e.raw_os_error() else {
        return e.to_string(); // our own messages, already English
    };
    if !cfg!(windows) {
        return e.to_string(); // strerror text; Rust does not localize it
    }
    let text = match code {
        2 | 3 | 123 | 433 | 1167 => "device not found",
        5 => "access denied",
        6 => "invalid handle",
        22 | 31 | 1117 => "device stopped responding",
        87 => "invalid parameter",
        121 | 1460 => "timed out",
        995 => "operation aborted",
        _ => "system error",
    };
    format!("{text} (os error {code})")
}

#[derive(Default, Clone, Copy)]
struct Signals {
    dtr: Option<bool>,
    rts: Option<bool>,
}

/// The open port, if any, for the command thread.
type SharedPort = Arc<Mutex<Option<Arc<Port>>>>;

pub fn spawn(cfg: SerialConfig, cmd_rx: mpsc::UnboundedReceiver<TransportCmd>, sink: EventSink) {
    let closed = Arc::new(AtomicBool::new(false));
    let writer: SharedPort = Arc::new(Mutex::new(None));
    let signals = Arc::new(Mutex::new(Signals {
        dtr: Some(cfg.dtr),
        // With RTS/CTS flow control the driver owns RTS.
        rts: (cfg.flow_control != FlowControl::Hardware).then_some(cfg.rts),
    }));

    {
        let (cfg, closed, writer, signals, sink) =
            (cfg.clone(), closed.clone(), writer.clone(), signals.clone(), sink.clone());
        thread::Builder::new()
            .name(format!("serial-rx-{}", cfg.port))
            .spawn(move || reader_loop(cfg, closed, writer, signals, sink))
            .expect("spawn serial reader");
    }
    thread::Builder::new()
        .name(format!("serial-tx-{}", cfg.port))
        .spawn(move || command_loop(cmd_rx, closed, writer, signals, sink))
        .expect("spawn serial writer");
}

fn command_loop(
    mut cmd_rx: mpsc::UnboundedReceiver<TransportCmd>,
    closed: Arc<AtomicBool>,
    writer: SharedPort,
    signals: Arc<Mutex<Signals>>,
    sink: EventSink,
) {
    while let Some(cmd) = cmd_rx.blocking_recv() {
        let port = writer.lock().unwrap().clone();
        match cmd {
            TransportCmd::Write(data) | TransportCmd::WriteData(data) => {
                if let Some(port) = port {
                    // Hard I/O errors surface through the reader, which owns reconnection.
                    if let Err(e) = port.write(&data, &closed) {
                        if e.kind() == io::ErrorKind::TimedOut {
                            sink.notice("Transmit stalled – check flow control (CTS/XON) settings.");
                        }
                    }
                }
            }
            TransportCmd::SetDtr(v) => {
                signals.lock().unwrap().dtr = Some(v);
                if let Some(port) = port {
                    let _ = port.set_dtr(v);
                }
            }
            TransportCmd::SetRts(v) => {
                signals.lock().unwrap().rts = Some(v);
                if let Some(port) = port {
                    let _ = port.set_rts(v);
                }
            }
            TransportCmd::SendBreak => {
                if let Some(port) = port {
                    if port.set_break(true).is_ok() {
                        thread::sleep(BREAK_DURATION);
                        let _ = port.set_break(false);
                    }
                }
            }
            TransportCmd::Sync(done) => {
                let _ = done.send(());
            }
            TransportCmd::Resize { .. } => {}
            TransportCmd::Close => break,
        }
    }
    closed.store(true, Ordering::SeqCst);
    if let Some(port) = writer.lock().unwrap().take() {
        port.cancel_io(); // wake the reader so the port is released promptly
    }
}

fn reader_loop(
    cfg: SerialConfig,
    closed: Arc<AtomicBool>,
    writer: SharedPort,
    signals: Arc<Mutex<Signals>>,
    sink: EventSink,
) {
    let stop = || closed.load(Ordering::SeqCst) || sink.is_closed();
    let settings = format!(
        "{} {} {}{}{}",
        cfg.port,
        cfg.baud_rate,
        cfg.data_bits,
        match cfg.parity {
            Parity::None => 'N',
            Parity::Odd => 'O',
            Parity::Even => 'E',
        },
        cfg.stop_bits
    );

    sink.state(LinkState::Connecting, format!("Opening {}…", cfg.port));
    let mut port = match Port::open(&cfg, READ_TIMEOUT) {
        Ok(p) => Arc::new(p),
        Err(e) => {
            sink.state(LinkState::Disconnected, describe_open_error(&cfg, &e));
            return;
        }
    };
    #[cfg(unix)]
    {
        let others = sys::other_users(&cfg.port);
        if !others.is_empty() {
            sink.notice(format!(
                "{} is also open in {}. Incoming data will be split between the programs; close the other one.",
                cfg.port,
                others.join(", ")
            ));
        }
    }

    let mut first = true;
    loop {
        // Ports that never appear in the enumeration (some virtual drivers) are not presence-checked.
        let enumerable = port_present_strict(&cfg.port);
        {
            let s = *signals.lock().unwrap();
            if let Some(v) = s.dtr {
                let _ = port.set_dtr(v);
            }
            if let Some(v) = s.rts {
                let _ = port.set_rts(v);
            }
        }
        *writer.lock().unwrap() = Some(port.clone());
        let msg = if first { settings.clone() } else { format!("{settings} (reconnected)") };
        first = false;
        sink.state(LinkState::Connected, msg);

        let mut buf = vec![0u8; 64 * 1024];
        let mut last_check = Instant::now();
        let err: io::Error = loop {
            if stop() {
                *writer.lock().unwrap() = None;
                return;
            }
            match port.read(&mut buf) {
                Ok(0) => {}
                Ok(n) => {
                    if !sink.data(buf[..n].to_vec()) {
                        return;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => break e,
            }
            // Some USB-serial drivers keep returning timeouts after the device is unplugged.
            if enumerable && last_check.elapsed() >= PRESENCE_CHECK_INTERVAL {
                last_check = Instant::now();
                if !port_present(&cfg.port) {
                    break io::Error::new(io::ErrorKind::NotFound, "device removed");
                }
            }
        };

        *writer.lock().unwrap() = None;
        port.cancel_io(); // a write blocked on the dead device gives up now
        drop(port);
        if stop() {
            return;
        }
        let reason = disconnect_reason(&err, enumerable && !port_present_strict(&cfg.port));
        if !cfg.auto_reconnect {
            sink.state(LinkState::Disconnected, format!("{}: {reason}.", cfg.port));
            return;
        }
        sink.state(LinkState::Reconnecting, format!("{}: {reason}. Waiting for it to come back…", cfg.port));
        port = loop {
            thread::sleep(REOPEN_INTERVAL);
            if stop() {
                return;
            }
            if let Ok(p) = Port::open(&cfg, READ_TIMEOUT) {
                break Arc::new(p);
            }
        };
    }
}

#[cfg(test)]
#[path = "serial_hw_tests.rs"]
mod hw_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_sort() {
        let mut v = vec!["COM10", "COM2", "COM1", "/dev/ttyUSB10", "/dev/ttyUSB9"];
        v.sort_by_key(|n| natural_key(n));
        assert_eq!(v, vec!["/dev/ttyUSB9", "/dev/ttyUSB10", "COM1", "COM2", "COM10"]);
    }

    #[test]
    fn port_suffix_is_removed() {
        assert_eq!(strip_port_suffix("USB Serial Port(COM4)", "COM4"), "USB Serial Port");
        assert_eq!(
            strip_port_suffix("Silicon Labs CP210x USB to UART Bridge (COM3)", "COM3"),
            "Silicon Labs CP210x USB to UART Bridge"
        );
        assert_eq!(strip_port_suffix("FT232R USB UART", "COM4"), "FT232R USB UART");
        assert_eq!(strip_port_suffix("(COM4)", "COM4"), "(COM4)");
    }

    #[test]
    fn unplug_is_reported_the_same_on_every_os() {
        let unplugged = "the device was unplugged";
        // What each backend actually returned when an adapter was pulled.
        let linux = io::Error::new(io::ErrorKind::BrokenPipe, "Broken pipe");
        assert_eq!(disconnect_reason(&linux, false), unplugged);
        let removed = io::Error::new(io::ErrorKind::NotFound, "device removed");
        assert_eq!(disconnect_reason(&removed, false), unplugged);
        let code = if cfg!(windows) { 22 } else { 5 }; // ERROR_BAD_COMMAND / EIO
        assert_eq!(disconnect_reason(&io::Error::from_raw_os_error(code), false), unplugged);
        // Any error, once the port has left the device list.
        let other = io::Error::other("something else");
        assert_eq!(disconnect_reason(&other, true), unplugged);
        // Otherwise the cause is shown.
        assert_eq!(disconnect_reason(&other, false), "connection lost (something else)");
    }

    #[test]
    fn listing_ports_does_not_panic() {
        let _ = list_ports();
    }

    /// Whatever adapters this machine has: no empty ttyS slots, USB adapters first (outside Windows).
    #[test]
    fn port_list_hides_empty_uarts_and_puts_usb_first() {
        let ports = list_ports();
        eprintln!("ports: {:?}", ports.iter().map(|p| (&p.name, p.kind)).collect::<Vec<_>>());
        assert!(ports.iter().all(|p| !is_placeholder_uart(&p.name)));
        if cfg!(not(windows)) {
            let first_other = ports.iter().position(|p| p.kind != "usb").unwrap_or(ports.len());
            assert!(ports[first_other..].iter().all(|p| p.kind != "usb"), "USB ports must come first");
        }
    }
}
