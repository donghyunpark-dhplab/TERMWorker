//! Hardware-in-the-loop tests for the serial transport.
//!
//! Ignored by default because they need real adapters. Wire two USB-serial
//! adapters to each other (TX↔RX, RX↔TX, GND↔GND) and run:
//!
//! ```text
//! TW_HW_PORT=COM4 TW_HW_PEER=COM6 cargo test hw_ -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! `TW_HW_PORT` is driven through the transport under test, `TW_HW_PEER`
//! directly by the test. Only TX/RX/GND are required; tests that need the
//! modem-control lines say so.
//!
//! `TW_HW_MAX_BAUD` (default 921600) caps the speed for adapter pairs that
//! cannot talk to each other reliably at the top rate. Check such a pair with
//! a plain terminal first: that is a hardware limit, not a transport bug.
//!
//! `hw_unplug_reconnect` needs a person at the desk: set `TW_HW_UNPLUG` to a
//! port that will be physically removed and reinserted when the test asks.

use std::io::{ErrorKind, Read};
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use serialport::SerialPort;
use tokio::sync::mpsc::{self, error::TryRecvError};

use super::{open_serialport, port_present_strict, spawn, write_fully};
use crate::config::{FlowControl, Parity, SerialConfig};
use crate::transport::{EventSink, LinkState, TransportCmd, TransportEvent};

const XON: u8 = 0x11;
const XOFF: u8 = 0x13;

/// All tests share the same physical ports.
static BENCH: Mutex<()> = Mutex::new(());

struct Bench {
    dut: String,
    peer: String,
    _lock: MutexGuard<'static, ()>,
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn bench() -> Option<Bench> {
    let lock = BENCH.lock().unwrap_or_else(|e| e.into_inner());
    match (env_var("TW_HW_PORT"), env_var("TW_HW_PEER")) {
        (Some(dut), Some(peer)) => Some(Bench { dut, peer, _lock: lock }),
        _ => {
            eprintln!("skipped: set TW_HW_PORT and TW_HW_PEER");
            None
        }
    }
}

/// Opens a port directly through `serialport` (peer side, CTS checks, release checks).
fn open(cfg: &SerialConfig) -> serialport::Result<Box<dyn SerialPort>> {
    open_serialport(cfg, Duration::from_millis(50))
}

fn max_baud() -> u32 {
    env_var("TW_HW_MAX_BAUD").and_then(|v| v.parse().ok()).unwrap_or(921_600)
}

fn config(port: &str, baud: u32) -> SerialConfig {
    SerialConfig {
        port: port.into(),
        baud_rate: baud,
        data_bits: 8,
        parity: Parity::None,
        stop_bits: 1,
        flow_control: FlowControl::None,
        auto_reconnect: false,
        dtr: true,
        rts: true,
    }
}

fn bits_per_char(cfg: &SerialConfig) -> u32 {
    1 + cfg.data_bits as u32 + (cfg.parity != Parity::None) as u32 + cfg.stop_bits as u32
}

/// Deterministic test data that contains every byte value (masked to the data width).
fn payload(len: usize, mask: u8, seed: u32) -> Vec<u8> {
    let mut v: Vec<u8> = (0..=255u8).map(|b| b & mask).collect();
    let mut x = seed | 1;
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        v.push((x >> 8) as u8 & mask);
    }
    v.truncate(len);
    v
}

fn assert_same(label: &str, got: &[u8], want: &[u8]) {
    if got == want {
        return;
    }
    let first_diff = got.iter().zip(want).position(|(a, b)| a != b).unwrap_or(got.len().min(want.len()));
    panic!(
        "{label}: received {} of {} bytes, first difference at offset {first_diff} (got {:02x?}, want {:02x?})",
        got.len(),
        want.len(),
        got.get(first_diff..(first_diff + 8).min(got.len())).unwrap_or(&[]),
        want.get(first_diff..(first_diff + 8).min(want.len())).unwrap_or(&[]),
    );
}

// ---------------------------------------------------------------- peer side

fn open_peer(name: &str, like: &SerialConfig) -> Box<dyn SerialPort> {
    let mut cfg = like.clone();
    cfg.port = name.into();
    cfg.flow_control = FlowControl::None;
    let mut p = open(&cfg).unwrap_or_else(|e| panic!("open peer {name}: {e}"));
    p.set_timeout(Duration::from_millis(20)).unwrap();
    p.clear(serialport::ClearBuffer::All).unwrap();
    p
}

fn peer_write(p: &mut dyn SerialPort, data: &[u8]) {
    write_fully(p, data, &AtomicBool::new(false)).expect("peer write");
}

/// Reads until `want` bytes arrived or `timeout` elapsed.
fn peer_read(p: &mut dyn SerialPort, want: usize, timeout: Duration) -> Vec<u8> {
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    while out.len() < want && Instant::now() < deadline {
        match p.read(&mut buf) {
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
            Err(e) => panic!("peer read: {e}"),
        }
    }
    out
}

// ----------------------------------------------------------- transport side

/// The transport under test, driven exactly like `Session` drives it.
struct Dut {
    cmd: Option<mpsc::UnboundedSender<TransportCmd>>,
    events: mpsc::UnboundedReceiver<TransportEvent>,
    data: Vec<u8>,
    notices: Vec<String>,
}

impl Dut {
    fn start(cfg: SerialConfig) -> Self {
        let (cmd, cmd_rx) = mpsc::unbounded_channel();
        let (ev_tx, events) = mpsc::unbounded_channel();
        spawn(cfg, cmd_rx, EventSink::new(ev_tx));
        Self { cmd: Some(cmd), events, data: Vec::new(), notices: Vec::new() }
    }

    fn connected(cfg: SerialConfig) -> Self {
        let mut d = Self::start(cfg);
        // Generous: an FTDI adapter on a flaky hub took 5 s to open.
        d.wait_state(LinkState::Connected, Duration::from_secs(15));
        d
    }

    fn send(&self, cmd: TransportCmd) {
        self.cmd.as_ref().unwrap().send(cmd).expect("transport command channel closed");
    }

    /// Drops both channel ends without a `Close`, as when a session disappears.
    fn abandon(mut self) {
        self.cmd = None;
    }

    fn next_event(&mut self, deadline: Instant) -> Option<TransportEvent> {
        loop {
            match self.events.try_recv() {
                Ok(ev) => return Some(ev),
                Err(TryRecvError::Empty) if Instant::now() < deadline => thread::sleep(Duration::from_millis(1)),
                Err(_) => return None,
            }
        }
    }

    /// Waits for `want`, buffering data and notices that arrive meanwhile.
    fn wait_state(&mut self, want: LinkState, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        while let Some(ev) = self.next_event(deadline) {
            match ev {
                TransportEvent::State { state, message } => {
                    let msg = message.unwrap_or_default();
                    if state == want {
                        return msg;
                    }
                    if state == LinkState::Disconnected {
                        panic!("Disconnected while waiting for {want:?}: {msg}");
                    }
                }
                TransportEvent::Data(d) => self.data.extend(d),
                TransportEvent::Notice(n) => self.notices.push(n),
                other => panic!("unexpected event {other:?}"),
            }
        }
        panic!("timed out waiting for {want:?}");
    }

    /// Collects received bytes until `want` arrived or `timeout` elapsed.
    fn read(&mut self, want: usize, timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        while self.data.len() < want {
            match self.next_event(deadline) {
                Some(TransportEvent::Data(d)) => self.data.extend(d),
                Some(TransportEvent::Notice(n)) => self.notices.push(n),
                Some(TransportEvent::State { state, message }) => {
                    panic!("link went {state:?}: {message:?}")
                }
                Some(other) => panic!("unexpected event {other:?}"),
                None => break,
            }
        }
        std::mem::take(&mut self.data)
    }

    fn wait_notice(&mut self, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        while self.notices.is_empty() {
            match self.next_event(deadline)? {
                TransportEvent::Data(d) => self.data.extend(d),
                TransportEvent::Notice(n) => self.notices.push(n),
                TransportEvent::State { state, message } => {
                    panic!("link went {state:?}: {message:?}")
                }
                other => panic!("unexpected event {other:?}"),
            }
        }
        Some(self.notices.remove(0))
    }
}

impl Drop for Dut {
    /// Closes the session and waits for both transport threads to finish, so the
    /// next test finds the port free.
    fn drop(&mut self) {
        let Some(cmd) = &self.cmd else { return };
        let _ = cmd.send(TransportCmd::Close);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match self.events.try_recv() {
                Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(2)),
                Ok(_) => {}
            }
        }
        eprintln!("warning: transport threads still running 5 s after Close");
    }
}

/// Polls until the port can be opened again (i.e. the transport released it).
fn time_until_reopenable(port: &str, limit: Duration) -> Option<Duration> {
    let start = Instant::now();
    while start.elapsed() < limit {
        if open(&config(port, 115200)).is_ok() {
            // An open() can itself block while the kernel finishes the previous close.
            let t = start.elapsed();
            return (t < limit).then_some(t);
        }
        thread::sleep(Duration::from_millis(5));
    }
    None
}

fn is_english(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii() || c == '…' || c == '–')
}

// -------------------------------------------------------------------- tests

#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_receive_integrity() {
    let Some(b) = bench() else { return };
    for baud in [9600, 115_200, 460_800, 921_600].into_iter().filter(|b| *b <= max_baud()) {
        let cfg = config(&b.dut, baud);
        let line_rate = baud / bits_per_char(&cfg);
        let len = (line_rate as usize * 3 / 2).clamp(1024, 256 * 1024);
        let mut peer = open_peer(&b.peer, &cfg);
        let mut dut = Dut::connected(cfg);
        let data = payload(len, 0xFF, baud);

        let t = Instant::now();
        peer_write(peer.as_mut(), &data);
        let got = dut.read(len, Duration::from_secs_f64(len as f64 / line_rate as f64 * 2.0 + 2.0));
        let secs = t.elapsed().as_secs_f64();
        eprintln!(
            "  peer -> transport {baud:>7} baud: {} B in {secs:.2}s = {:.0} B/s ({:.0}% of line rate)",
            got.len(),
            got.len() as f64 / secs,
            got.len() as f64 / secs / line_rate as f64 * 100.0
        );
        assert_same(&format!("receive @ {baud}"), &got, &data);
    }
}

#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_transmit_integrity() {
    let Some(b) = bench() else { return };
    for baud in [9600, 115_200, 460_800, 921_600].into_iter().filter(|b| *b <= max_baud()) {
        let cfg = config(&b.dut, baud);
        let line_rate = baud / bits_per_char(&cfg);
        let len = (line_rate as usize * 3 / 2).clamp(1024, 256 * 1024);
        let mut peer = open_peer(&b.peer, &cfg);
        let dut = Dut::connected(cfg);
        let data = payload(len, 0xFF, baud ^ 0x5A5A);

        let t = Instant::now();
        // One large write, like pasting a big block into the terminal.
        dut.send(TransportCmd::Write(data.clone()));
        let got = peer_read(peer.as_mut(), len, Duration::from_secs_f64(len as f64 / line_rate as f64 * 3.0 + 2.0));
        let secs = t.elapsed().as_secs_f64();
        eprintln!(
            "  transport -> peer {baud:>7} baud: {} B in {secs:.2}s = {:.0} B/s ({:.0}% of line rate)",
            got.len(),
            got.len() as f64 / secs,
            got.len() as f64 / secs / line_rate as f64 * 100.0
        );
        assert_same(&format!("transmit @ {baud}"), &got, &data);
    }
}

#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_full_duplex() {
    let Some(b) = bench() else { return };
    let mut failures = Vec::new();
    for baud in [115_200, max_baud()] {
        let cfg = config(&b.dut, baud);
        let len = (baud / 10 * 2) as usize; // ~2 s of traffic each way
        let mut peer = open_peer(&b.peer, &cfg);
        let mut dut = Dut::connected(cfg);
        let to_dut = payload(len, 0xFF, 1);
        let to_peer = payload(len, 0xFF, 2);

        let t = Instant::now();
        dut.send(TransportCmd::Write(to_peer.clone()));
        let got_peer = peer_exchange(peer.as_mut(), &to_dut, len, Duration::from_secs(8));
        let got_dut = dut.read(len, Duration::from_secs(3));
        eprintln!(
            "  @ {baud}: transport received {}/{len}, peer received {}/{len} in {:.2}s",
            got_dut.len(),
            got_peer.len(),
            t.elapsed().as_secs_f64()
        );
        for (label, got, want) in [("transport received", &got_dut, &to_dut), ("peer received", &got_peer, &to_peer)] {
            if got != want {
                failures.push(format!("@ {baud} {label}"));
            }
        }
    }
    assert!(failures.is_empty(), "data lost in full duplex: {failures:?}");
}

/// Pasting into a console that echoes: everything sent comes straight back.
#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_paste_with_echo() {
    let Some(b) = bench() else { return };
    let mut failures = Vec::new();
    for baud in [115_200, max_baud()] {
        let cfg = config(&b.dut, baud);
        let len = (baud / 10 * 2) as usize;
        let mut peer_cfg = cfg.clone();
        peer_cfg.port = b.peer.clone();
        let peer = super::sys::Port::open(&peer_cfg, Duration::from_millis(20)).expect("open peer");
        let mut dut = Dut::connected(cfg);
        let paste = payload(len, 0xFF, 5);

        let t = Instant::now();
        dut.send(TransportCmd::Write(paste.clone()));
        let echoed = peer_echo(&peer, len, Duration::from_secs(8));
        let got = dut.read(len, Duration::from_secs(3));
        eprintln!(
            "  @ {baud}: peer echoed {}/{len}, transport got back {}/{len} in {:.2}s",
            echoed,
            got.len(),
            t.elapsed().as_secs_f64()
        );
        if got != paste {
            failures.push(baud);
        }
    }
    assert!(failures.is_empty(), "echo of a paste came back incomplete at {failures:?} baud");
}

/// Sends `out` while collecting `want` bytes, from a single thread: two threads
/// on one synchronous Windows handle starve each other, which would make the
/// peer – not the code under test – lose data.
fn peer_exchange(p: &mut dyn SerialPort, out: &[u8], want: usize, timeout: Duration) -> Vec<u8> {
    let deadline = Instant::now() + timeout;
    let (mut got, mut sent) = (Vec::new(), 0);
    let mut buf = vec![0u8; 64 * 1024];
    while (got.len() < want || sent < out.len()) && Instant::now() < deadline {
        let avail = p.bytes_to_read().expect("peer bytes_to_read") as usize;
        if avail > 0 {
            let cap = avail.min(buf.len());
            let n = p.read(&mut buf[..cap]).expect("peer read");
            got.extend_from_slice(&buf[..n]);
        }
        if sent < out.len() {
            match p.write(&out[sent..(sent + 256).min(out.len())]) {
                Ok(n) => sent += n,
                Err(e) if e.kind() == ErrorKind::TimedOut => {}
                Err(e) => panic!("peer write: {e}"),
            }
        } else if avail == 0 {
            thread::sleep(Duration::from_millis(1));
        }
    }
    got
}

/// Echoes everything it receives until `total` bytes went back. Returns the count.
///
/// Uses the transport's own `Port`: `serialport`'s synchronous writes on Windows
/// may report a timeout after part of the data went out, and a retry then
/// duplicates bytes – the peer, not the code under test, would corrupt the echo.
/// `hw_ext_echo` repeats this against an independent .NET peer.
fn peer_echo(p: &super::sys::Port, total: usize, timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;
    let never = AtomicBool::new(false);
    let mut echoed = 0;
    let mut buf = vec![0u8; 64 * 1024];
    while echoed < total && Instant::now() < deadline {
        let n = p.read(&mut buf).expect("peer read");
        p.write(&buf[..n], &never).expect("peer write");
        echoed += n;
    }
    echoed
}

#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_frame_formats() {
    let Some(b) = bench() else { return };
    let formats = [
        (7, Parity::Even, 1),
        (7, Parity::Odd, 1),
        (7, Parity::None, 2),
        (8, Parity::Even, 1),
        (8, Parity::Odd, 1),
        (8, Parity::None, 2),
    ];
    for (bits, parity, stop) in formats {
        let mut cfg = config(&b.dut, 115_200);
        cfg.data_bits = bits;
        cfg.parity = parity;
        cfg.stop_bits = stop;
        let mask = if bits == 7 { 0x7F } else { 0xFF };
        let mut peer = open_peer(&b.peer, &cfg);
        let mut dut = Dut::connected(cfg);
        let label = format!("{bits}{parity:?}{stop}");

        let down = payload(4096, mask, bits as u32 * 31 + stop as u32);
        peer_write(peer.as_mut(), &down);
        assert_same(&format!("{label} receive"), &dut.read(down.len(), Duration::from_secs(3)), &down);

        let up = payload(4096, mask, bits as u32 * 17 + stop as u32);
        dut.send(TransportCmd::Write(up.clone()));
        assert_same(&format!("{label} transmit"), &peer_read(peer.as_mut(), up.len(), Duration::from_secs(3)), &up);
        eprintln!("  {label}: ok");
    }
}

#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_break() {
    let Some(b) = bench() else { return };
    let cfg = config(&b.dut, 115_200);
    let mut peer = open_peer(&b.peer, &cfg);
    let mut dut = Dut::connected(cfg);

    // Transport sends a break: a UART sees it as a NUL with a framing error.
    dut.send(TransportCmd::SendBreak);
    let deadline = Instant::now() + Duration::from_millis(800);
    let mut seen = Vec::new();
    let mut errors = Vec::new();
    let mut buf = [0u8; 64];
    while Instant::now() < deadline {
        match peer.read(&mut buf) {
            Ok(n) => seen.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::TimedOut => {}
            Err(e) => errors.push(e.to_string()),
        }
    }
    eprintln!("  peer saw after break: bytes {seen:02x?}, read errors {errors:?}");
    assert!(seen.contains(&0) || !errors.is_empty(), "peer did not detect the break");

    // The link must keep working afterwards.
    dut.send(TransportCmd::Write(b"after-break".to_vec()));
    assert_eq!(peer_read(peer.as_mut(), 11, Duration::from_secs(1)), b"after-break");

    // Peer sends a break: what reaches the terminal?
    peer.set_break().unwrap();
    thread::sleep(Duration::from_millis(250));
    peer.clear_break().unwrap();
    peer_write(peer.as_mut(), b"X");
    let got = dut.read(2, Duration::from_secs(1));
    eprintln!("  transport received after peer break: {got:02x?}");
    assert_eq!(got.last(), Some(&b'X'), "data after a received break was lost");
}

#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_xon_xoff() {
    let Some(b) = bench() else { return };
    let mut cfg = config(&b.dut, 115_200);
    cfg.flow_control = FlowControl::Software;
    let mut peer = open_peer(&b.peer, &cfg);
    let mut dut = Dut::connected(cfg);

    // Short pause: the device says XOFF, we must hold our data until XON.
    peer_write(peer.as_mut(), &[XOFF]);
    thread::sleep(Duration::from_millis(100));
    let msg = payload(200, 0xFF, 7).into_iter().filter(|b| *b != XON && *b != XOFF).collect::<Vec<_>>();
    dut.send(TransportCmd::Write(msg.clone()));
    let leaked = peer_read(peer.as_mut(), 1, Duration::from_millis(800));
    eprintln!("  bytes sent while XOFF was in effect: {}", leaked.len());
    peer_write(peer.as_mut(), &[XON]);
    let mut got = leaked;
    got.extend(peer_read(peer.as_mut(), msg.len() - got.len(), Duration::from_secs(2)));
    assert_same("data released by XON", &got, &msg);

    // XON/XOFF characters from the device are flow control, not terminal output.
    let shown = dut.read(1, Duration::from_millis(300));
    eprintln!("  XON/XOFF bytes passed through to the terminal: {shown:02x?}");

    // Long pause (> stall timeout) with a block bigger than the adapter's buffers.
    peer_write(peer.as_mut(), &[XOFF]);
    thread::sleep(Duration::from_millis(100));
    let big = payload(64 * 1024, 0xFF, 9).into_iter().filter(|b| *b != XON && *b != XOFF).collect::<Vec<_>>();
    dut.send(TransportCmd::Write(big.clone()));
    let notice = dut.wait_notice(Duration::from_secs(6));
    eprintln!("  notice during long XOFF: {notice:?}");
    let before_xon = peer_read(peer.as_mut(), big.len(), Duration::from_millis(200)).len();
    peer_write(peer.as_mut(), &[XON]);
    dut.send(TransportCmd::Write(b"<END>".to_vec()));
    let after = peer_read(peer.as_mut(), usize::MAX, Duration::from_secs(3));
    eprintln!(
        "  long XOFF: {before_xon} B leaked before XON, {} B after XON, ends with <END>: {}",
        after.len(),
        after.ends_with(b"<END>")
    );
    assert!(after.ends_with(b"<END>"), "transport did not recover after a long XOFF");
}

#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_rts_cts_stall() {
    let Some(b) = bench() else { return };
    let mut cfg = config(&b.dut, 115_200);
    {
        let mut p = open(&cfg).unwrap();
        if p.read_clear_to_send().unwrap() {
            eprintln!("skipped: CTS on {} is asserted, so it cannot stall", b.dut);
            return;
        }
    }
    cfg.flow_control = FlowControl::Hardware;
    let mut peer = open_peer(&b.peer, &cfg);
    let mut dut = Dut::connected(cfg);

    let t = Instant::now();
    dut.send(TransportCmd::Write(payload(64 * 1024, 0xFF, 3)));
    let notice = dut.wait_notice(Duration::from_secs(8));
    let stalled_after = t.elapsed();
    let leaked = peer_read(peer.as_mut(), usize::MAX, Duration::from_millis(300)).len();
    eprintln!("  CTS low: notice {notice:?} after {stalled_after:.2?}, {leaked} B got out");
    assert!(notice.is_some_and(|n| n.contains("stalled")), "no stall notice");

    // Closing a stalled tab must still release the port promptly.
    dut.send(TransportCmd::Write(payload(64 * 1024, 0xFF, 4)));
    thread::sleep(Duration::from_millis(500));
    let t = Instant::now();
    dut.send(TransportCmd::Close);
    let released = time_until_reopenable(&b.dut, Duration::from_secs(5));
    eprintln!("  closed while stalled: port reopenable after {released:.2?} (close requested {:.2?} ago)", t.elapsed());
    drop(dut);
    assert!(released.is_some(), "port stayed busy after closing a stalled session");
}

#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_open_errors() {
    let Some(b) = bench() else { return };

    let mut missing = Dut::start(config("COM250", 115_200));
    let msg = missing.wait_state(LinkState::Disconnected, Duration::from_secs(3));
    eprintln!("  missing port: {msg:?}");

    let _holder = open(&config(&b.peer, 115_200)).unwrap();
    let mut busy = Dut::start(config(&b.peer, 115_200));
    let busy_msg = busy.wait_state(LinkState::Disconnected, Duration::from_secs(3));
    eprintln!("  busy port:    {busy_msg:?}");

    assert!(msg.contains("not found"), "missing port should say so: {msg}");
    assert!(is_english(&msg), "OS text leaked into the UI: {msg}");
    assert!(busy_msg.contains("in use"), "busy port should say it is in use: {busy_msg}");
    assert!(is_english(&busy_msg), "OS text leaked into the UI: {busy_msg}");
}

#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_close_releases_port() {
    let Some(b) = bench() else { return };
    for how in ["Close command", "session dropped"] {
        let dut = Dut::connected(config(&b.dut, 115_200));
        thread::sleep(Duration::from_millis(200));
        // In the Close case the session stays alive, so only the command can stop the transport.
        let keep = if how == "Close command" {
            dut.send(TransportCmd::Close);
            Some(dut)
        } else {
            dut.abandon();
            None
        };
        let released = time_until_reopenable(&b.dut, Duration::from_secs(3));
        drop(keep);
        eprintln!("  {how}: reopenable after {released:.2?}");
        assert!(released.is_some_and(|d| d < Duration::from_millis(500)), "{how}: port not released");
    }
}

#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_latency() {
    let Some(b) = bench() else { return };
    let cfg = config(&b.dut, 115_200);
    let mut peer = open_peer(&b.peer, &cfg);
    let mut dut = Dut::connected(cfg);
    thread::sleep(Duration::from_millis(300));

    let stats = |mut v: Vec<Duration>| {
        v.sort();
        (v[v.len() / 2], v[v.len() * 9 / 10], *v.last().unwrap())
    };

    // Keystroke: terminal -> device.
    let mut up = Vec::new();
    for i in 0..40u8 {
        let t = Instant::now();
        dut.send(TransportCmd::Write(vec![b'a' + i % 26]));
        assert_eq!(peer_read(peer.as_mut(), 1, Duration::from_secs(1)).len(), 1);
        up.push(t.elapsed());
        thread::sleep(Duration::from_millis(7 + (i as u64 * 13) % 40));
    }
    // Echo: device -> terminal.
    let mut down = Vec::new();
    for i in 0..40u8 {
        let t = Instant::now();
        peer_write(peer.as_mut(), &[b'A' + i % 26]);
        assert_eq!(dut.read(1, Duration::from_secs(1)).len(), 1);
        down.push(t.elapsed());
        thread::sleep(Duration::from_millis(7 + (i as u64 * 13) % 40));
    }
    let (m, p90, max) = stats(up);
    eprintln!("  keystroke -> device: median {m:.1?}, p90 {p90:.1?}, max {max:.1?}");
    let (m2, p90b, max2) = stats(down);
    eprintln!("  device -> terminal:  median {m2:.1?}, p90 {p90b:.1?}, max {max2:.1?}");
}

/// Interactive: unplug and replug `TW_HW_UNPLUG` when asked.
#[test]
#[ignore = "needs a person to unplug and replug TW_HW_UNPLUG"]
fn hw_unplug_reconnect() {
    let _lock = BENCH.lock().unwrap_or_else(|e| e.into_inner());
    let Some(port) = env_var("TW_HW_UNPLUG") else {
        eprintln!("skipped: set TW_HW_UNPLUG");
        return;
    };
    let cycles: u32 = env_var("TW_HW_CYCLES").and_then(|v| v.parse().ok()).unwrap_or(2);
    let wait = Duration::from_secs(env_var("TW_HW_WAIT").and_then(|v| v.parse().ok()).unwrap_or(900));

    // Watch the device list independently, to time how fast the transport reacts.
    let watch = |present: bool| {
        let start = Instant::now();
        while port_present_strict(&port) != present {
            assert!(start.elapsed() < wait, "timed out waiting for the tester");
            thread::sleep(Duration::from_millis(250));
        }
        Instant::now()
    };

    let mut cfg = config(&port, 115_200);
    cfg.auto_reconnect = true;
    let mut dut = Dut::connected(cfg.clone());
    for cycle in 1..=cycles {
        eprintln!(">>> [{cycle}/{cycles}] UNPLUG {port} NOW");
        let gone = watch(false);
        let msg = dut.wait_state(LinkState::Reconnecting, Duration::from_secs(10));
        eprintln!("  detected after {:.2?}: {msg:?}", gone.elapsed());
        assert!(is_english(&msg), "OS text leaked into the UI: {msg}");
        assert!(msg.contains("the device was unplugged"), "unplug not recognised: {msg}");

        eprintln!(">>> [{cycle}/{cycles}] PLUG {port} BACK IN");
        let back = watch(true);
        let msg = dut.wait_state(LinkState::Connected, Duration::from_secs(20));
        eprintln!("  reconnected {:.2?} after the port reappeared: {msg:?}", back.elapsed());
        assert!(msg.contains("reconnected"));

        // The reopened port must be usable.
        dut.send(TransportCmd::Write(b"ping\r".to_vec()));
        dut.send(TransportCmd::SetDtr(false));
        dut.send(TransportCmd::SetDtr(true));
        thread::sleep(Duration::from_millis(300));
        assert!(dut.notices.is_empty(), "notices after reconnect: {:?}", dut.notices);
    }
    drop(dut);
    let released = time_until_reopenable(&port, Duration::from_secs(3));
    assert!(released.is_some(), "port not released after closing the tab");

    // Without auto-reconnect the tab reports the loss once and stops.
    cfg.auto_reconnect = false;
    let mut dut = Dut::connected(cfg);
    eprintln!(">>> [auto-reconnect off] UNPLUG {port} NOW");
    let gone = watch(false);
    let msg = dut.wait_state(LinkState::Disconnected, Duration::from_secs(10));
    eprintln!("  reported after {:.2?}: {msg:?}", gone.elapsed());
    assert!(is_english(&msg), "OS text leaked into the UI: {msg}");
    assert!(msg.contains("the device was unplugged"), "unplug not recognised: {msg}");
    eprintln!(">>> PLUG {port} BACK IN (test is done)");
}

// ------------------------------------------------- against a reference peer
//
// The peer is `scripts/dev/serialpeer.ps1`, which uses .NET's overlapped I/O and
// is known to run full duplex cleanly on the same adapters. Start it first:
//
//   powershell -File scripts/dev/serialpeer.ps1 -Port COM6 -Baud 460800 -Mode duplex -Bytes 92160
//   TW_HW_PORT=COM4 TW_HW_EXT=duplex TW_HW_BAUD=460800 TW_HW_BYTES=92160 cargo test hw_ext -- --ignored --nocapture

/// Same formula as `Get-Pattern` in serialpeer.ps1.
fn pattern(len: usize, seed: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 7 + (i >> 8) * 13 + seed) & 0xFF) as u8).collect()
}

fn ext(mode: &str) -> Option<(MutexGuard<'static, ()>, SerialConfig, usize)> {
    let lock = BENCH.lock().unwrap_or_else(|e| e.into_inner());
    if env_var("TW_HW_EXT").as_deref() != Some(mode) {
        eprintln!("skipped: set TW_HW_EXT={mode} and start serialpeer.ps1 -Mode {mode}");
        return None;
    }
    let port = env_var("TW_HW_PORT").expect("TW_HW_PORT");
    let baud = env_var("TW_HW_BAUD").and_then(|v| v.parse().ok()).unwrap_or(115_200);
    let bytes = env_var("TW_HW_BYTES").and_then(|v| v.parse().ok()).unwrap_or(65_536);
    Some((lock, config(&port, baud), bytes))
}

#[test]
#[ignore = "needs serialpeer.ps1 -Mode echo on the other adapter"]
fn hw_ext_echo() {
    let Some((_lock, cfg, len)) = ext("echo") else {
        return;
    };
    let baud = cfg.baud_rate;
    let mut dut = Dut::connected(cfg);
    let paste = pattern(len, 3);
    let t = Instant::now();
    dut.send(TransportCmd::Write(paste.clone()));
    let got = dut.read(len, Duration::from_secs_f64(len as f64 / (baud as f64 / 10.0) * 2.0 + 3.0));
    thread::sleep(Duration::from_millis(300));
    let extra = dut.read(usize::MAX, Duration::from_millis(300)).len();
    eprintln!(
        "  echo @ {baud}: got back {}/{len} (+{extra} extra) in {:.2}s, notices {:?}",
        got.len(),
        t.elapsed().as_secs_f64(),
        dut.notices
    );
    assert_same("echoed paste", &got, &paste);
    assert_eq!(extra, 0, "received more than was echoed");
}

#[test]
#[ignore = "needs serialpeer.ps1 -Mode duplex on the other adapter"]
fn hw_ext_duplex() {
    let Some((_lock, cfg, len)) = ext("duplex") else {
        return;
    };
    let baud = cfg.baud_rate;
    let mut dut = Dut::connected(cfg);
    let t = Instant::now();
    dut.send(TransportCmd::Write(pattern(len, 2)));
    let got = dut.read(len, Duration::from_secs_f64(len as f64 / (baud as f64 / 10.0) * 2.0 + 3.0));
    eprintln!(
        "  duplex @ {baud}: transport received {}/{len} in {:.2}s, notices {:?}",
        got.len(),
        t.elapsed().as_secs_f64(),
        dut.notices
    );
    // Give the transmit side time to finish before the transport is closed.
    thread::sleep(Duration::from_secs(2));
    assert_same("transport received", &got, &pattern(len, 1));
}

// ---------------------------------------------------------- file transfers

/// Opens a serial transport and forwards its received bytes to a std channel,
/// the way the session pump feeds a running transfer.
fn transfer_end(cfg: SerialConfig) -> (mpsc::UnboundedSender<TransportCmd>, std::sync::mpsc::Receiver<Vec<u8>>) {
    let (cmd, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    spawn(cfg, cmd_rx, EventSink::new(ev_tx));
    let (in_tx, in_rx) = std::sync::mpsc::channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        while let Some(ev) = ev_rx.blocking_recv() {
            match ev {
                TransportEvent::Data(d) => {
                    if in_tx.send(d).is_err() {
                        break;
                    }
                }
                TransportEvent::State { state: LinkState::Connected, .. } => {
                    let _ = ready_tx.send(());
                }
                _ => {}
            }
        }
    });
    ready_rx.recv_timeout(Duration::from_secs(15)).expect("transport did not connect");
    (cmd, in_rx)
}

#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_file_transfer() {
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    use crate::transfer::{self, Job, Protocol, TransportLink};

    let Some(b) = bench() else { return };
    let baud = env_var("TW_HW_XFER_BAUD").and_then(|v| v.parse().ok()).unwrap_or(115_200);
    let tmp = std::env::temp_dir().join(format!("tw-hw-xfer-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    // Seconds of line time per file (default 3).
    let secs: usize = env_var("TW_HW_XFER_SECS").and_then(|v| v.parse().ok()).unwrap_or(3);
    let len = ((baud / 10) as usize * secs).clamp(4096, 4 * 1024 * 1024);

    let mut failures = Vec::new();
    for protocol in [Protocol::Zmodem, Protocol::Ymodem, Protocol::Xmodem1k, Protocol::Xmodem] {
        for (from, to) in [(&b.dut, &b.peer), (&b.peer, &b.dut)] {
            let data = payload(len, 0xFF, baud ^ protocol as u32);
            let src = tmp.join(format!("src-{protocol:?}.bin"));
            std::fs::write(&src, &data).unwrap();
            let out_dir = tmp.join(format!("out-{protocol:?}-{from}"));
            let _ = std::fs::remove_dir_all(&out_dir);
            std::fs::create_dir_all(&out_dir).unwrap();
            let x = matches!(protocol, Protocol::Xmodem | Protocol::Xmodem1k);
            let dest = if x { out_dir.join("received.bin") } else { out_dir.clone() };

            let (cmd_s, in_s) = transfer_end(config(from, baud));
            let (cmd_r, in_r) = transfer_end(config(to, baud));
            let (cancel_s, cancel_r) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
            let mut link_s = TransportLink::new(in_s, cmd_s.clone(), cancel_s.clone());
            let mut link_r = TransportLink::new(in_r, cmd_r.clone(), cancel_r.clone());
            let send_job = Job::Send { protocol, files: vec![src.clone()], remote_ready: true };
            let recv_job = Job::Receive { protocol, dest: dest.clone() };

            let t = Instant::now();
            let (sent, received) = thread::scope(|s| {
                let rx = s.spawn(|| transfer::run(&recv_job, &mut link_r, &cancel_r, &mut |_| {}));
                let tx = s.spawn(|| transfer::run(&send_job, &mut link_s, &cancel_s, &mut |_| {}));
                (tx.join().unwrap(), rx.join().unwrap())
            });
            let secs = t.elapsed().as_secs_f64();
            let _ = cmd_s.send(TransportCmd::Close);
            let _ = cmd_r.send(TransportCmd::Close);
            let _ = cancel_s.load(Ordering::SeqCst);

            let got = std::fs::read(if x { dest.clone() } else { out_dir.join(src.file_name().unwrap()) })
                .unwrap_or_default();
            // XMODEM pads the last block; the rest must match exactly.
            let ok = sent.is_ok()
                && received.is_ok()
                && got.len() >= data.len()
                && got[..data.len()] == data[..]
                && (x || got.len() == data.len());
            eprintln!(
                "  {protocol:?} {from}->{to} @ {baud}: {} in {secs:.2}s = {:.0}% of line rate{}",
                if ok { "ok" } else { "FAILED" },
                len as f64 / secs / (baud as f64 / 10.0) * 100.0,
                match (&sent, &received) {
                    (Err(e), _) => format!(" (sender: {e})"),
                    (_, Err(e)) => format!(" (receiver: {e})"),
                    _ if !ok => format!(" (got {} of {} bytes)", got.len(), data.len()),
                    _ => String::new(),
                }
            );
            if !ok {
                failures.push(format!("{protocol:?} {from}->{to}"));
            }
            // Let both transports release their ports before the next round.
            assert!(time_until_reopenable(from, Duration::from_secs(3)).is_some());
            assert!(time_until_reopenable(to, Duration::from_secs(3)).is_some());
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(failures.is_empty(), "failed: {failures:?}");
}

/// Interoperability with lrzsz (`sz`/`rz`/`sb`/`rb`/`sx`/`rx`): our end runs on
/// `TW_HW_PORT` through the transport, lrzsz on `TW_HW_PEER` with the tty as its
/// stdin/stdout, as it would be on a device's console.
#[cfg(unix)]
#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER) and lrzsz"]
fn hw_lrzsz_interop() {
    use std::process::{Command, Stdio};
    use std::sync::Arc;

    use crate::transfer::{self, Job, Protocol, TransportLink};

    let Some(b) = bench() else { return };
    let baud: u32 = env_var("TW_HW_XFER_BAUD").and_then(|v| v.parse().ok()).unwrap_or(115_200);
    let secs: usize = env_var("TW_HW_XFER_SECS").and_then(|v| v.parse().ok()).unwrap_or(3);
    let len = ((baud / 10) as usize * secs).clamp(4096, 4 * 1024 * 1024);
    let tmp = std::env::temp_dir().join(format!("tw-hw-lrzsz-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();

    // (name, protocol, we send?, lrzsz command)
    let cases: &[(&str, Protocol, bool, &[&str])] = &[
        ("sz -> us", Protocol::Zmodem, false, &["sz", "{file}"]),
        ("us -> rz", Protocol::Zmodem, true, &["rz"]),
        ("sb -> us", Protocol::Ymodem, false, &["sb", "{file}"]),
        ("us -> rb", Protocol::Ymodem, true, &["rb"]),
        ("sx -> us", Protocol::Xmodem, false, &["sx", "{file}"]),
        ("sx -k -> us", Protocol::Xmodem, false, &["sx", "-k", "{file}"]),
        ("us(1K) -> rx", Protocol::Xmodem1k, true, &["rx", "out.bin"]),
        ("us -> rx", Protocol::Xmodem, true, &["rx", "out.bin"]),
    ];
    let mut failures = Vec::new();
    for (i, &(name, protocol, we_send, argv)) in cases.iter().enumerate() {
        let data = payload(len, 0xFF, baud ^ i as u32);
        let case_dir = tmp.join(format!("case{i}"));
        let (ours, theirs) = (case_dir.join("ours"), case_dir.join("theirs"));
        std::fs::create_dir_all(&ours).unwrap();
        std::fs::create_dir_all(&theirs).unwrap();
        let file_name = format!("lrzsz-{i}.bin");
        let src = if we_send { ours.join(&file_name) } else { theirs.join(&file_name) };
        std::fs::write(&src, &data).unwrap();
        let x = matches!(protocol, Protocol::Xmodem | Protocol::Xmodem1k);

        let st = Command::new("stty")
            .args(["-F", &b.peer, &baud.to_string(), "raw", "-echo", "-ixon", "-ixoff", "-crtscts", "clocal"])
            .status()
            .expect("stty");
        assert!(st.success(), "stty failed on {}", b.peer);

        let (cmd, input) = transfer_end(config(&b.dut, baud));
        let cancel = Arc::new(AtomicBool::new(false));
        let mut link = TransportLink::new(input, cmd.clone(), cancel.clone());
        let job = if we_send {
            Job::Send { protocol, files: vec![src.clone()], remote_ready: true }
        } else {
            Job::Receive { protocol, dest: if x { ours.join("received.bin") } else { ours.clone() } }
        };

        let tty = std::fs::OpenOptions::new().read(true).write(true).open(&b.peer).expect("open peer tty");
        let args: Vec<String> = argv[1..].iter().map(|a| a.replace("{file}", &file_name)).collect();
        let t = Instant::now();
        let (ours_result, status, stderr) = thread::scope(|s| {
            let us = s.spawn(|| transfer::run(&job, &mut link, &cancel, &mut |_| {}));
            let mut child = Command::new(argv[0])
                .args(&args)
                .current_dir(&theirs)
                .stdin(tty.try_clone().unwrap())
                .stdout(tty.try_clone().unwrap())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn lrzsz");
            // lrzsz has its own timeouts, but never wait forever.
            let deadline = Instant::now() + Duration::from_secs(60 + 3 * secs as u64);
            let status = loop {
                if let Some(st) = child.try_wait().unwrap() {
                    break Some(st);
                }
                if Instant::now() > deadline {
                    let _ = child.kill();
                    cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                    break None;
                }
                thread::sleep(Duration::from_millis(20));
            };
            let mut err = String::new();
            let _ = child.stderr.take().unwrap().read_to_string(&mut err);
            let ours_result = us.join().unwrap();
            (ours_result, status, err)
        });
        let elapsed = t.elapsed().as_secs_f64();
        let _ = cmd.send(TransportCmd::Close);
        drop(tty);

        let got_path = match (we_send, x) {
            (true, true) => theirs.join("out.bin"),
            (true, false) => theirs.join(&file_name),
            (false, true) => ours.join("received.bin"),
            (false, false) => ours.join(&file_name),
        };
        let got = std::fs::read(&got_path).unwrap_or_default();
        let content_ok = got.len() >= data.len() && got[..data.len()] == data[..] && (x || got.len() == data.len());
        let ok = content_ok && ours_result.is_ok() && status.is_some_and(|s| s.success());
        eprintln!(
            "  {name:<13} {protocol:?} @ {baud}: {} in {elapsed:.2}s = {:.0}% of line rate (got {} of {} B; ours: {}; lrzsz: {})",
            if ok { "ok" } else { "FAILED" },
            len as f64 / elapsed / (baud as f64 / 10.0) * 100.0,
            got.len(),
            data.len(),
            match &ours_result {
                Ok(_) => "ok".to_string(),
                Err(e) => e.to_string(),
            },
            match status {
                Some(s) => s.to_string(),
                None => "killed after timeout".into(),
            },
        );
        if !ok {
            let tail: String = stderr.trim().lines().rev().take(4).collect::<Vec<_>>().join(" | ");
            eprintln!("      lrzsz stderr (last lines, newest first): {tail}");
            failures.push(name);
        }
        assert!(time_until_reopenable(&b.dut, Duration::from_secs(5)).is_some(), "{} not released", b.dut);
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(failures.is_empty(), "failed: {failures:?}");
}

/// Linux lets a second program open a port that the first opened without an
/// exclusive lock; the user must be warned (they would each get part of the data).
#[cfg(target_os = "linux")]
#[test]
#[ignore = "needs a serial adapter (TW_HW_PORT)"]
fn hw_shared_port_warning() {
    let Some(b) = bench() else { return };

    let mut alone = Dut::connected(config(&b.dut, 115_200));
    let notice = alone.wait_notice(Duration::from_millis(500));
    eprintln!("  nobody else: {notice:?}");
    assert!(notice.is_none(), "unexpected notice {notice:?}");
    alone.send(TransportCmd::Close);
    drop(alone);
    assert!(time_until_reopenable(&b.dut, Duration::from_secs(3)).is_some());

    // Another program that opened the port plainly (no TIOCEXCL), like cat, screen or a Python script.
    let tty = std::fs::OpenOptions::new().read(true).write(true).open(&b.dut).expect("plain open");
    let mut holder = std::process::Command::new("sleep").arg("30").stdin(tty).spawn().expect("spawn sleep");
    let mut shared = Dut::connected(config(&b.dut, 115_200));
    let notice = shared.wait_notice(Duration::from_secs(1));
    eprintln!("  held by `sleep`: {notice:?}");
    let _ = holder.kill();
    let _ = holder.wait();
    shared.send(TransportCmd::Close);
    let notice = notice.expect("no warning about the other program");
    assert!(notice.contains("also open in sleep (pid"), "{notice}");
}

/// HDLC frames both ways over the real line: the transport sends encoded frames
/// (as the session does) and the peer decodes them, then the other way round.
#[test]
#[ignore = "needs two cross-wired serial adapters (TW_HW_PORT, TW_HW_PEER)"]
fn hw_hdlc_frames() {
    use crate::hdlc::{self, Fcs, HdlcConfig, Item};

    let Some(b) = bench() else { return };
    let baud = env_var("TW_HW_XFER_BAUD").and_then(|v| v.parse().ok()).unwrap_or(115_200);
    let cfg_line = config(&b.dut, baud);
    let mut peer = open_peer(&b.peer, &cfg_line);
    let mut dut = Dut::connected(cfg_line);

    // Frames of 1..=600 bytes with every awkward byte (7E, 7D, control characters).
    let frames: Vec<Vec<u8>> = (0..300u32)
        .map(|i| {
            let mut f = payload(1 + (i as usize * 37) % 600, 0xFF, i);
            f.extend_from_slice(&[0x7E, 0x7D, 0x00, 0x11, 0x13, 0x7E][..(i % 6) as usize]);
            f
        })
        .collect();
    let total: usize = frames.iter().map(Vec::len).sum();

    for fcs in [Fcs::Fcs16, Fcs::Fcs32] {
        for escape_control_chars in [false, true] {
            let cfg = HdlcConfig { fcs, escape_control_chars, ..HdlcConfig::default() };
            let decode = |wire: &[u8]| -> Vec<Vec<u8>> {
                let mut d = hdlc::Decoder::new(cfg.clone());
                let mut items = d.feed(wire);
                items.extend(d.flush());
                items
                    .into_iter()
                    .filter_map(|i| match i {
                        Item::Frame(f) if f.fcs_ok => Some(f.payload),
                        _ => None,
                    })
                    .collect()
            };
            let wire: Vec<u8> = frames.iter().flat_map(|f| hdlc::encode(&cfg, f)).collect();

            // Transport -> peer.
            let t = Instant::now();
            for f in &frames {
                dut.send(TransportCmd::WriteData(hdlc::encode(&cfg, f)));
            }
            let got =
                peer_read(peer.as_mut(), wire.len(), Duration::from_secs(10 + wire.len() as u64 * 10 / baud as u64));
            let out_secs = t.elapsed().as_secs_f64();
            let rx = decode(&got);

            // Peer -> transport.
            let t = Instant::now();
            peer_write(peer.as_mut(), &wire);
            let got = dut.read(wire.len(), Duration::from_secs(10 + wire.len() as u64 * 10 / baud as u64));
            let in_secs = t.elapsed().as_secs_f64();
            let tx = decode(&got);

            eprintln!(
                "  {fcs:?}{} @ {baud}: out {} of {} frames ({:.0}% of line rate), in {} of {} frames ({:.0}%)",
                if escape_control_chars { " +ACCM" } else { "" },
                rx.iter().zip(&frames).filter(|(a, b)| a == b).count(),
                frames.len(),
                wire.len() as f64 / out_secs / (baud as f64 / 10.0) * 100.0,
                tx.iter().zip(&frames).filter(|(a, b)| a == b).count(),
                frames.len(),
                wire.len() as f64 / in_secs / (baud as f64 / 10.0) * 100.0,
            );
            assert_eq!(rx, frames, "{fcs:?}: frames sent by the transport");
            assert_eq!(tx, frames, "{fcs:?}: frames received by the transport");
        }
    }
    eprintln!("  payload per pass: {total} bytes");
    dut.send(TransportCmd::Close);
}
