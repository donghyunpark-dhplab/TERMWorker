//! A session ties one transport to one terminal tab.
//!
//! Transport events are pumped through here: received bytes are logged
//! (raw or decoded), decoded with the session's character set, coalesced,
//! and forwarded to the UI over a Tauri channel. While a file transfer runs,
//! received bytes go to the transfer instead of the terminal.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc as std_mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::ipc::Channel;
use tokio::sync::mpsc;

use crate::codec::Codec;
use crate::config::{Connection, TerminalOptions};
use crate::hdlc::{self, HdlcConfig};
use crate::logger::{LogMode, Logger};
use crate::script::Tap;
use crate::transfer::{self, Job, Protocol, Report, TransportLink, ZmodemDetector};
use crate::transport::{
    self, EventSink, HostKeyDecision, HostKeyPrompt, HostKeySlot, LinkState, SpawnContext, TransportCmd, TransportEvent,
};

/// Upper bound for coalescing queued output into one UI message.
const MAX_BATCH: usize = 256 * 1024;
/// How long ZMODEM greetings are ignored after a transfer or a declined request
/// (the remote side may repeat its greeting before it notices).
const DETECT_QUIET: Duration = Duration::from_secs(3);
/// Progress updates sent to the UI at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// HDLC: a frame still open after this much silence is given up (text or a cut-off frame).
const HDLC_IDLE_FLUSH: Duration = Duration::from_millis(100);
/// HDLC file send: wait for the transport to catch up every this many bytes.
const HDLC_SYNC_EVERY: usize = 4096;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SessionEvent {
    Data {
        text: String,
    },
    Status {
        state: LinkState,
        message: Option<String>,
    },
    Notice {
        text: String,
    },
    HostKey(HostKeyPrompt),
    AuthFailed,
    Log {
        path: Option<String>,
    },
    Transfer(TransferStatus),
    /// The remote side started `sz` (it sends) or `rz` (it receives).
    Zmodem {
        remote_sends: bool,
    },
    /// An HDLC frame received or sent.
    HdlcFrame(HdlcFrameEvent),
    /// Progress of sending a file as HDLC frames.
    HdlcSend(HdlcSendStatus),
    /// Saving received HDLC payloads to a file: `path` is `None` once stopped.
    HdlcRecord {
        path: Option<String>,
        bytes: u64,
        frames: u64,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HdlcFrameEvent {
    pub received: bool,
    pub payload: Vec<u8>,
    pub fcs_ok: bool,
    /// Milliseconds since the Unix epoch.
    pub time_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HdlcSendStatus {
    pub file: String,
    pub size: u64,
    pub bytes: u64,
    pub frames: u64,
    pub elapsed_ms: u64,
    pub outcome: Option<Outcome>,
    pub message: Option<String>,
}

struct Hdlc {
    decoder: hdlc::Decoder,
    record: Option<Recording>,
}

struct Recording {
    file: std::io::BufWriter<std::fs::File>,
    path: String,
    bytes: u64,
    frames: u64,
    last_report: Instant,
    /// Frames written since the last report to the UI.
    dirty: bool,
}

impl Recording {
    fn report(&mut self, events: &Channel<SessionEvent>) {
        let _ = std::io::Write::flush(&mut self.file);
        self.last_report = Instant::now();
        self.dirty = false;
        let _ = events.send(SessionEvent::HdlcRecord {
            path: Some(self.path.clone()),
            bytes: self.bytes,
            frames: self.frames,
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Done,
    Failed,
    Cancelled,
}

/// A complete picture of the running (or just finished) transfer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferStatus {
    pub protocol: Protocol,
    pub sending: bool,
    /// Current file: name as sent over the line, and the local path.
    pub file: Option<String>,
    pub path: Option<String>,
    pub size: Option<u64>,
    pub bytes: u64,
    pub files_done: u32,
    /// Bytes of all finished files plus the current one.
    pub total_bytes: u64,
    pub elapsed_ms: u64,
    /// Set on the final event.
    pub outcome: Option<Outcome>,
    pub message: Option<String>,
}

struct Active {
    input: std_mpsc::Sender<Vec<u8>>,
    cancel: Arc<AtomicBool>,
}

struct Shared {
    codec: Mutex<Codec>,
    logger: Mutex<Option<Logger>>,
    transfer: Mutex<Option<Active>>,
    detector: Mutex<ZmodemDetector>,
    detect_quiet_until: Mutex<Option<Instant>>,
    /// HDLC mode: received bytes are split into frames and terminal text.
    hdlc: Mutex<Option<Hdlc>>,
    /// Cancels a running HDLC file send.
    hdlc_send: Mutex<Option<Arc<AtomicBool>>>,
    /// Running scripts that watch this tab.
    tap: Arc<Tap>,
}

impl Shared {
    fn quiet_detection(&self) {
        *self.detect_quiet_until.lock().unwrap() = Some(Instant::now() + DETECT_QUIET);
    }
}

pub struct Session {
    cmd_tx: mpsc::UnboundedSender<TransportCmd>,
    telnet: bool,
    shared: Arc<Shared>,
    hostkey_slot: HostKeySlot,
    events: Channel<SessionEvent>,
}

pub struct OpenParams {
    pub connection: Connection,
    pub options: TerminalOptions,
    pub secret: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub known_hosts: std::path::PathBuf,
    pub tap: Arc<Tap>,
}

impl Session {
    pub fn open(p: OpenParams, events: Channel<SessionEvent>) -> Result<Arc<Session>, String> {
        let codec = Codec::new(&p.options.encoding, p.options.enter_sends)?;
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let hostkey_slot: HostKeySlot = Arc::new(Mutex::new(None));
        let telnet = matches!(p.connection, Connection::Telnet(_));
        let shared = Arc::new(Shared {
            codec: Mutex::new(codec),
            logger: Mutex::new(None),
            transfer: Mutex::new(None),
            detector: Mutex::new(ZmodemDetector::default()),
            detect_quiet_until: Mutex::new(None),
            hdlc: Mutex::new(None),
            hdlc_send: Mutex::new(None),
            tap: p.tap,
        });

        tauri::async_runtime::spawn(pump(ev_rx, shared.clone(), events.clone()));
        transport::spawn(
            p.connection,
            SpawnContext {
                cols: p.cols.max(1),
                rows: p.rows.max(1),
                known_hosts: p.known_hosts,
                hostkey_slot: hostkey_slot.clone(),
                secret: p.secret,
            },
            cmd_rx,
            EventSink::new(ev_tx),
        );

        Ok(Arc::new(Session { cmd_tx, telnet, shared, hostkey_slot, events }))
    }

    fn send(&self, cmd: TransportCmd) -> Result<(), String> {
        self.cmd_tx.send(cmd).map_err(|_| "Session is closed".to_string())
    }

    pub fn write_text(&self, text: &str) -> Result<(), String> {
        let bytes = self.shared.codec.lock().unwrap().encode_input(text);
        self.send(TransportCmd::Write(bytes))
    }

    /// Text without the Enter-key mapping (scripts that send exact control characters).
    pub fn write_text_exact(&self, text: &str) -> Result<(), String> {
        let bytes = self.shared.codec.lock().unwrap().encode(text);
        self.send(TransportCmd::Write(bytes))
    }

    pub fn write_bytes(&self, bytes: Vec<u8>) -> Result<(), String> {
        self.send(TransportCmd::Write(bytes))
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        self.send(TransportCmd::Resize { cols: cols.max(1), rows: rows.max(1) })
    }

    pub fn set_dtr(&self, v: bool) -> Result<(), String> {
        self.send(TransportCmd::SetDtr(v))
    }

    pub fn set_rts(&self, v: bool) -> Result<(), String> {
        self.send(TransportCmd::SetRts(v))
    }

    pub fn send_break(&self) -> Result<(), String> {
        self.send(TransportCmd::SendBreak)
    }

    pub fn set_encoding(&self, label: &str) -> Result<String, String> {
        let mut codec = self.shared.codec.lock().unwrap();
        codec.set_encoding(label)?;
        Ok(codec.encoding_name().to_string())
    }

    pub fn answer_hostkey(&self, decision: HostKeyDecision) -> Result<(), String> {
        let tx = self.hostkey_slot.lock().unwrap().take().ok_or("No host key question is pending")?;
        tx.send(decision).map_err(|_| "Connection attempt already ended".to_string())
    }

    pub fn start_log(&self, path: &Path, mode: LogMode) -> Result<String, String> {
        let logger = Logger::create(path, mode).map_err(|e| format!("Cannot open log file {}: {e}", path.display()))?;
        let shown = logger.path().display().to_string();
        *self.shared.logger.lock().unwrap() = Some(logger);
        let _ = self.events.send(SessionEvent::Log { path: Some(shown.clone()) });
        Ok(shown)
    }

    pub fn stop_log(&self) {
        if self.shared.logger.lock().unwrap().take().is_some() {
            let _ = self.events.send(SessionEvent::Log { path: None });
        }
    }

    /// Starts a file transfer; received bytes go to it until it ends.
    pub fn start_transfer(&self, job: Job) -> Result<(), String> {
        let mut slot = self.shared.transfer.lock().unwrap();
        if slot.is_some() {
            return Err("A file transfer is already running in this session".into());
        }
        let (input_tx, input_rx) = std_mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        *slot = Some(Active { input: input_tx, cancel: cancel.clone() });
        drop(slot);

        let link = TransportLink::new(input_rx, self.cmd_tx.clone(), cancel.clone()).telnet(self.telnet);
        let (shared, events) = (self.shared.clone(), self.events.clone());
        thread::Builder::new()
            .name("file-transfer".into())
            .spawn(move || run_transfer(job, link, cancel, shared, events))
            .map_err(|e| format!("Cannot start the transfer: {e}"))?;
        Ok(())
    }

    pub fn cancel_transfer(&self) {
        if let Some(active) = self.shared.transfer.lock().unwrap().as_ref() {
            active.cancel.store(true, Ordering::SeqCst);
        }
    }

    /// Answers a ZMODEM request the user turned down, so the remote `sz`/`rz` stops waiting.
    pub fn decline_zmodem(&self) -> Result<(), String> {
        self.shared.quiet_detection();
        self.send(TransportCmd::Write(transfer::ABORT_SEQUENCE.to_vec()))
    }

    // ------------------------------------------------------------------ HDLC

    /// Turns HDLC mode on (or changes its settings) or off. Turning it off hands
    /// a half-received frame back to the terminal and stops saving payloads.
    pub fn set_hdlc(&self, config: Option<HdlcConfig>) {
        let mut slot = self.shared.hdlc.lock().unwrap();
        let old = slot.take();
        let record = old.and_then(|mut h| {
            let rest = h.decoder.flush();
            let text: Vec<u8> = rest
                .into_iter()
                .filter_map(|i| if let hdlc::Item::Text(t) = i { Some(t) } else { None })
                .flatten()
                .collect();
            if !text.is_empty() {
                let text = self.shared.codec.lock().unwrap().decode(&text);
                let _ = self.events.send(SessionEvent::Data { text });
            }
            h.record
        });
        match config {
            Some(cfg) => *slot = Some(Hdlc { decoder: hdlc::Decoder::new(cfg), record }),
            None => {
                if let Some(r) = record {
                    let _ = self.events.send(SessionEvent::HdlcRecord { path: None, bytes: r.bytes, frames: r.frames });
                }
            }
        }
    }

    fn hdlc_config(&self) -> Result<HdlcConfig, String> {
        self.shared.hdlc.lock().unwrap().as_ref().map(|h| h.decoder.config().clone()).ok_or("HDLC mode is off".into())
    }

    /// Sends one frame with `payload`.
    pub fn hdlc_send(&self, payload: Vec<u8>) -> Result<(), String> {
        let cfg = self.hdlc_config()?;
        if self.shared.transfer.lock().unwrap().is_some() {
            return Err("A file transfer is running in this session".into());
        }
        self.send(TransportCmd::WriteData(hdlc::encode(&cfg, &payload)))?;
        let _ = self.events.send(SessionEvent::HdlcFrame(HdlcFrameEvent {
            received: false,
            payload,
            fcs_ok: true,
            time_ms: now_ms(),
        }));
        Ok(())
    }

    /// Sends a file as consecutive frames of at most `chunk` payload bytes, with
    /// `gap_ms` of silence after each frame. No acknowledgements are expected.
    pub fn hdlc_send_file(&self, path: &Path, chunk: usize, gap_ms: u64) -> Result<(), String> {
        let cfg = self.hdlc_config()?;
        if !(1..=65536).contains(&chunk) {
            return Err("Frame size must be between 1 and 65536 bytes".into());
        }
        if self.shared.transfer.lock().unwrap().is_some() {
            return Err("A file transfer is running in this session".into());
        }
        let mut slot = self.shared.hdlc_send.lock().unwrap();
        if slot.is_some() {
            return Err("A file is already being sent as HDLC frames".into());
        }
        let data = std::fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        let cancel = Arc::new(AtomicBool::new(false));
        *slot = Some(cancel.clone());
        drop(slot);

        let (cmd_tx, shared, events) = (self.cmd_tx.clone(), self.shared.clone(), self.events.clone());
        let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        thread::Builder::new()
            .name("hdlc-send".into())
            .spawn(move || {
                let started = Instant::now();
                let mut status = HdlcSendStatus {
                    file,
                    size: data.len() as u64,
                    bytes: 0,
                    frames: 0,
                    elapsed_ms: 0,
                    outcome: None,
                    message: None,
                };
                let report = |s: &mut HdlcSendStatus| {
                    s.elapsed_ms = started.elapsed().as_millis() as u64;
                    let _ = events.send(SessionEvent::HdlcSend(s.clone()));
                };
                report(&mut status);
                let mut last = Instant::now();
                let mut unsynced = 0usize;
                let result: Result<(), String> = (|| {
                    for piece in data.chunks(chunk) {
                        if cancel.load(Ordering::SeqCst) {
                            return Err("cancelled".into());
                        }
                        let frame = hdlc::encode(&cfg, piece);
                        unsynced += frame.len();
                        cmd_tx.send(TransportCmd::WriteData(frame)).map_err(|_| "the session closed".to_string())?;
                        // Keep only a little in flight so Cancel acts quickly and progress is real.
                        if unsynced >= HDLC_SYNC_EVERY || gap_ms > 0 {
                            let (tx, rx) = std_mpsc::channel();
                            cmd_tx.send(TransportCmd::Sync(tx)).map_err(|_| "the session closed".to_string())?;
                            let _ = rx.recv_timeout(Duration::from_secs(30));
                            unsynced = 0;
                        }
                        if gap_ms > 0 {
                            thread::sleep(Duration::from_millis(gap_ms));
                        }
                        status.bytes += piece.len() as u64;
                        status.frames += 1;
                        if last.elapsed() >= PROGRESS_INTERVAL {
                            last = Instant::now();
                            report(&mut status);
                        }
                    }
                    Ok(())
                })();
                *shared.hdlc_send.lock().unwrap() = None;
                match result {
                    Ok(()) => status.outcome = Some(Outcome::Done),
                    Err(e) if e == "cancelled" => status.outcome = Some(Outcome::Cancelled),
                    Err(e) => {
                        status.outcome = Some(Outcome::Failed);
                        status.message = Some(e);
                    }
                }
                report(&mut status);
            })
            .map_err(|e| format!("Cannot start sending: {e}"))?;
        Ok(())
    }

    pub fn hdlc_cancel_send(&self) {
        if let Some(c) = self.shared.hdlc_send.lock().unwrap().as_ref() {
            c.store(true, Ordering::SeqCst);
        }
    }

    /// Starts (`Some`) or stops (`None`) saving the payloads of good received frames to a file.
    pub fn hdlc_record(&self, path: Option<&Path>) -> Result<(), String> {
        let mut slot = self.shared.hdlc.lock().unwrap();
        let h = slot.as_mut().ok_or("HDLC mode is off")?;
        if let Some(mut old) = h.record.take() {
            let _ = std::io::Write::flush(&mut old.file);
            if path.is_none() {
                let _ = self.events.send(SessionEvent::HdlcRecord { path: None, bytes: old.bytes, frames: old.frames });
            }
        }
        if let Some(path) = path {
            let file = std::fs::File::create(path).map_err(|e| format!("Cannot create {}: {e}", path.display()))?;
            let shown = path.display().to_string();
            h.record = Some(Recording {
                file: std::io::BufWriter::new(file),
                path: shown.clone(),
                bytes: 0,
                frames: 0,
                last_report: Instant::now(),
                dirty: false,
            });
            let _ = self.events.send(SessionEvent::HdlcRecord { path: Some(shown), bytes: 0, frames: 0 });
        }
        Ok(())
    }

    pub fn close(&self) {
        self.cancel_transfer();
        self.hdlc_cancel_send();
        let _ = self.cmd_tx.send(TransportCmd::Close);
        self.stop_log();
        // Wake a pending host-key question so the transport can finish.
        self.hostkey_slot.lock().unwrap().take();
    }
}

fn run_transfer(
    job: Job,
    mut link: TransportLink,
    cancel: Arc<AtomicBool>,
    shared: Arc<Shared>,
    events: Channel<SessionEvent>,
) {
    let (protocol, sending) = match &job {
        Job::Send { protocol, .. } => (*protocol, true),
        Job::Receive { protocol, .. } => (*protocol, false),
    };
    let started = Instant::now();
    let mut status = TransferStatus {
        protocol,
        sending,
        file: None,
        path: None,
        size: None,
        bytes: 0,
        files_done: 0,
        total_bytes: 0,
        elapsed_ms: 0,
        outcome: None,
        message: None,
    };
    let mut done_bytes = 0u64;
    let mut last_sent = Instant::now() - PROGRESS_INTERVAL;
    let send = |status: &mut TransferStatus| {
        status.elapsed_ms = started.elapsed().as_millis() as u64;
        let _ = events.send(SessionEvent::Transfer(status.clone()));
    };
    send(&mut status);

    let result = transfer::run(&job, &mut link, &cancel, &mut |r| {
        match r {
            Report::File { name, path, size } => {
                status.file = Some(name);
                status.path = Some(path.display().to_string());
                status.size = size;
                status.bytes = 0;
            }
            Report::Progress { bytes } => {
                status.bytes = bytes;
                status.total_bytes = done_bytes + bytes;
                if last_sent.elapsed() < PROGRESS_INTERVAL {
                    return;
                }
            }
            Report::FileDone { bytes, .. } => {
                status.bytes = bytes;
                done_bytes += bytes;
                status.total_bytes = done_bytes;
                status.files_done += 1;
            }
        }
        last_sent = Instant::now();
        send(&mut status);
    });

    *shared.transfer.lock().unwrap() = None;
    shared.quiet_detection();
    match result {
        Ok(_) => status.outcome = Some(Outcome::Done),
        Err(transfer::Error::Cancelled) => status.outcome = Some(Outcome::Cancelled),
        Err(e) => {
            status.outcome = Some(Outcome::Failed);
            status.message = Some(e.to_string());
        }
    }
    send(&mut status);
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Terminal-bound bytes: ZMODEM detection and decoding. Returns the text for the screen.
fn terminal_text(shared: &Shared, events: &Channel<SessionEvent>, bytes: &[u8]) -> String {
    let detected = shared.detector.lock().unwrap().feed(bytes);
    if let Some(d) = detected {
        let mut quiet = shared.detect_quiet_until.lock().unwrap();
        if quiet.is_none_or(|t| Instant::now() >= t) {
            *quiet = Some(Instant::now() + DETECT_QUIET);
            let remote_sends = d == transfer::ZmodemDetected::RemoteSends;
            let _ = events.send(SessionEvent::Zmodem { remote_sends });
        }
    }
    shared.codec.lock().unwrap().decode(bytes)
}

fn hex_line(payload: &[u8]) -> String {
    payload.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

/// Splits received bytes into terminal text and HDLC frames (if HDLC mode is on).
/// Returns (text for the screen, text for a text log).
fn split_received(shared: &Shared, events: &Channel<SessionEvent>, items: Vec<hdlc::Item>) -> (String, String) {
    let (mut screen, mut log) = (String::new(), String::new());
    for item in items {
        match item {
            hdlc::Item::Text(t) => {
                let text = terminal_text(shared, events, &t);
                log.push_str(&text);
                screen.push_str(&text);
            }
            hdlc::Item::Frame(f) => {
                if !log.is_empty() && !log.ends_with('\n') {
                    log.push('\n');
                }
                log.push_str(&format!(
                    "[HDLC rx {} B{}] {}\n",
                    f.payload.len(),
                    if f.fcs_ok { "" } else { ", FCS error" },
                    hex_line(&f.payload)
                ));
                if f.fcs_ok {
                    let mut slot = shared.hdlc.lock().unwrap();
                    if let Some(rec) = slot.as_mut().and_then(|h| h.record.as_mut()) {
                        match std::io::Write::write_all(&mut rec.file, &f.payload) {
                            Ok(()) => {
                                rec.bytes += f.payload.len() as u64;
                                rec.frames += 1;
                                rec.dirty = true;
                                if rec.last_report.elapsed() >= PROGRESS_INTERVAL {
                                    rec.report(events);
                                }
                            }
                            Err(e) => {
                                let (bytes, frames) = (rec.bytes, rec.frames);
                                slot.as_mut().unwrap().record = None;
                                let _ =
                                    events.send(SessionEvent::Notice { text: format!("Saving frames stopped: {e}") });
                                let _ = events.send(SessionEvent::HdlcRecord { path: None, bytes, frames });
                            }
                        }
                    }
                }
                shared.tap.frame(&f.payload, f.fcs_ok);
                let _ = events.send(SessionEvent::HdlcFrame(HdlcFrameEvent {
                    received: true,
                    payload: f.payload,
                    fcs_ok: f.fcs_ok,
                    time_ms: now_ms(),
                }));
            }
        }
    }
    (screen, log)
}

async fn pump(mut rx: mpsc::UnboundedReceiver<TransportEvent>, shared: Arc<Shared>, events: Channel<SessionEvent>) {
    let mut pending: Option<TransportEvent> = None;
    loop {
        let ev = match pending.take() {
            Some(ev) => ev,
            None => {
                // An HDLC frame left open: give it up if the line goes quiet. Also
                // report (and flush) saved payloads once the line is quiet.
                let wait_for_quiet = shared
                    .hdlc
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|h| h.decoder.pending() || h.record.as_ref().is_some_and(|r| r.dirty));
                let next = if wait_for_quiet {
                    match tokio::time::timeout(HDLC_IDLE_FLUSH, rx.recv()).await {
                        Ok(next) => next,
                        Err(_) => {
                            let items = match shared.hdlc.lock().unwrap().as_mut() {
                                Some(h) => {
                                    if let Some(r) = h.record.as_mut().filter(|r| r.dirty) {
                                        r.report(&events);
                                    }
                                    if h.decoder.pending() {
                                        h.decoder.flush()
                                    } else {
                                        Vec::new()
                                    }
                                }
                                None => Vec::new(),
                            };
                            let (screen, log_text) = split_received(&shared, &events, items);
                            if let Some(logger) = shared.logger.lock().unwrap().as_mut() {
                                let _ = logger.write(&[], &log_text);
                            }
                            if !screen.is_empty() {
                                shared.tap.data(&screen);
                                if events.send(SessionEvent::Data { text: screen }).is_err() {
                                    break;
                                }
                            }
                            continue;
                        }
                    }
                } else {
                    rx.recv().await
                };
                match next {
                    Some(ev) => ev,
                    None => break,
                }
            }
        };
        let out = match ev {
            TransportEvent::Data(mut bytes) => {
                // Coalesce output that is already queued: far fewer IPC messages under heavy traffic.
                while bytes.len() < MAX_BATCH {
                    match rx.try_recv() {
                        Ok(TransportEvent::Data(more)) => bytes.extend_from_slice(&more),
                        Ok(other) => {
                            pending = Some(other);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                // A running file transfer owns the byte stream.
                if let Some(active) = shared.transfer.lock().unwrap().as_ref() {
                    let _ = active.input.send(bytes);
                    continue;
                }
                shared.tap.bytes(&bytes);
                let items = shared.hdlc.lock().unwrap().as_mut().map(|h| h.decoder.feed(&bytes));
                let (text, log_text) = match items {
                    Some(items) => split_received(&shared, &events, items),
                    None => {
                        let text = terminal_text(&shared, &events, &bytes);
                        (text.clone(), text)
                    }
                };
                let mut log = shared.logger.lock().unwrap();
                if let Some(logger) = log.as_mut() {
                    if let Err(e) = logger.write(&bytes, &log_text) {
                        *log = None;
                        let _ = events.send(SessionEvent::Notice { text: format!("Logging stopped: {e}") });
                        let _ = events.send(SessionEvent::Log { path: None });
                    }
                }
                drop(log);
                if text.is_empty() {
                    continue;
                }
                shared.tap.data(&text);
                SessionEvent::Data { text }
            }
            TransportEvent::State { state, message } => {
                if state != LinkState::Connected {
                    // The line is gone: dropping the input ends a running transfer
                    // with "connection closed".
                    shared.transfer.lock().unwrap().take();
                }
                shared.tap.status(state, message.as_deref());
                SessionEvent::Status { state, message }
            }
            TransportEvent::Notice(text) => SessionEvent::Notice { text },
            TransportEvent::HostKey(prompt) => SessionEvent::HostKey(prompt),
            TransportEvent::AuthFailed => SessionEvent::AuthFailed,
        };
        if events.send(out).is_err() {
            break;
        }
    }
}
