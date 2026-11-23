//! Built-in TFTP server: devices on the bench (U-Boot `tftpboot`, busybox `tftp`)
//! fetch firmware from a folder on this PC, and optionally upload files into it.
//!
//! RFC 1350 with the option extension (RFC 2347) and its blksize, timeout and tsize
//! options (RFC 2348, 2349). Downloads are always allowed from the shared folder;
//! uploads only when turned on, and never over an existing file. Names cannot leave
//! the folder. One server per app, on its own thread and tokio runtime, each
//! transfer on its own UDP port as the protocol requires.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::watch;

pub const DEFAULT_PORT: u16 = 69;
const RRQ: u16 = 1;
const WRQ: u16 = 2;
const DATA: u16 = 3;
const ACK: u16 = 4;
const ERROR: u16 = 5;
const OACK: u16 = 6;
const DEFAULT_BLKSIZE: usize = 512;
const MAX_BLKSIZE: usize = 65464;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(1);
const RETRIES: u32 = 6;
const KEEP_TRANSFERS: usize = 100;
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

// ------------------------------------------------------------------ public types

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TftpConfig {
    /// The shared folder.
    pub folder: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Devices may upload new files into the folder.
    #[serde(default)]
    pub allow_upload: bool,
    /// Listen on one address only; all addresses when empty.
    #[serde(default)]
    pub bind: Option<String>,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferState {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferInfo {
    pub id: u64,
    /// Address of the device.
    pub peer: String,
    /// The name the device asked for.
    pub file: String,
    pub upload: bool,
    /// Total size when known (downloads, or uploads that sent tsize).
    pub size: Option<u64>,
    pub bytes: u64,
    pub block_size: usize,
    pub state: TransferState,
    pub error: Option<String>,
    /// Unix time in milliseconds.
    pub started: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub running: bool,
    pub config: Option<TftpConfig>,
    /// The port actually in use (differs from the config only for port 0, in tests).
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum TftpEvent {
    Status { status: ServerStatus },
    Transfer { transfer: TransferInfo },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAddress {
    /// Adapter name ("Ethernet", "eth0").
    pub name: String,
    pub ip: String,
    pub prefix: u8,
    /// 169.254.x.x: what a PC falls back to without DHCP.
    pub link_local: bool,
}

/// IPv4 addresses of this PC on adapters that are up, without loopback.
/// 169.254.x.x is kept: it is what a PC falls back to when a board's DHCP goes away.
pub fn local_addresses() -> Vec<LocalAddress> {
    let mut out: Vec<LocalAddress> = crate::netcfg::ipv4_addresses()
        .into_iter()
        .filter(|(_, ip, _)| !ip.is_loopback() && !ip.is_unspecified())
        .map(|(name, ip, prefix)| LocalAddress { name, ip: ip.to_string(), prefix, link_local: ip.is_link_local() })
        .collect();
    // Usable addresses first.
    out.sort_by(|a, b| (a.link_local, &a.name).cmp(&(b.link_local, &b.name)));
    out
}

pub type Notify = Arc<dyn Fn(TftpEvent) + Send + Sync>;

// ------------------------------------------------------------------ server

pub struct TftpServer {
    shared: Arc<Shared>,
    running: Mutex<Option<Running>>,
}

struct Running {
    config: TftpConfig,
    port: u16,
    stop: watch::Sender<bool>,
}

struct Shared {
    notify: Notify,
    transfers: Mutex<VecDeque<TransferInfo>>,
    next_id: AtomicU64,
}

impl TftpServer {
    pub fn new(notify: Notify) -> Self {
        Self {
            shared: Arc::new(Shared { notify, transfers: Mutex::new(VecDeque::new()), next_id: AtomicU64::new(1) }),
            running: Mutex::new(None),
        }
    }

    pub fn status(&self) -> ServerStatus {
        match &*self.running.lock().unwrap() {
            Some(r) => ServerStatus { running: true, config: Some(r.config.clone()), port: Some(r.port) },
            None => ServerStatus::default(),
        }
    }

    pub fn transfers(&self) -> Vec<TransferInfo> {
        self.shared.transfers.lock().unwrap().iter().cloned().collect()
    }

    /// Starts serving (or restarts with a new configuration). Errors are ready to show.
    pub fn start(&self, config: TftpConfig) -> Result<ServerStatus, String> {
        let mut running = self.running.lock().unwrap();
        if let Some(r) = &*running {
            if r.config == config {
                return Ok(ServerStatus { running: true, config: Some(config), port: Some(r.port) });
            }
        }
        if let Some(r) = running.take() {
            let _ = r.stop.send(true);
        }

        let root = PathBuf::from(&config.folder);
        if !root.is_dir() {
            return Err(format!("The folder does not exist: {}", root.display()));
        }
        let root = root.canonicalize().map_err(|e| format!("Cannot use {}: {e}", root.display()))?;
        let ip: IpAddr = match config.bind.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => s.parse().map_err(|_| format!("Not an IP address: {s}"))?,
            None => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        };
        let socket =
            std::net::UdpSocket::bind(SocketAddr::new(ip, config.port)).map_err(|e| bind_error(&e, config.port))?;
        socket.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = socket.local_addr().map_err(|e| e.to_string())?.port();

        let (stop, stop_rx) = watch::channel(false);
        let shared = self.shared.clone();
        let (cfg, thread_ip) = (config.clone(), ip);
        std::thread::Builder::new()
            .name("tftp".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => return log::error!("tftp: {e}"),
                };
                rt.block_on(serve(socket, thread_ip, root, cfg, shared, stop_rx));
            })
            .map_err(|e| format!("Cannot start the TFTP server: {e}"))?;

        log::info!("tftp: serving {} on port {port}", config.folder);
        *running = Some(Running { config: config.clone(), port, stop });
        let status = ServerStatus { running: true, config: Some(config), port: Some(port) };
        drop(running);
        (self.shared.notify)(TftpEvent::Status { status: status.clone() });
        Ok(status)
    }

    pub fn stop(&self) {
        if let Some(r) = self.running.lock().unwrap().take() {
            let _ = r.stop.send(true);
            log::info!("tftp: stopped");
            (self.shared.notify)(TftpEvent::Status { status: ServerStatus::default() });
        }
    }
}

impl Drop for TftpServer {
    fn drop(&mut self) {
        if let Some(r) = self.running.lock().unwrap().take() {
            let _ = r.stop.send(true);
        }
    }
}

/// Linux lets only privileged programs listen below 1024 (net.ipv4.ip_unprivileged_port_start).
fn low_port_help(port: u16, alt: &str) -> String {
    let first = format!("Port {port} needs extra permission on Linux. Use {alt}");
    // An AppImage runs from a fresh read-only mount each time: a file capability cannot stick.
    if std::env::var_os("APPIMAGE").is_some() {
        return format!("{first}.");
    }
    match std::env::current_exe() {
        Ok(exe) => format!(
            "{first}, or allow low ports for TermWorker with: sudo setcap cap_net_bind_service=+ep '{}' \
             (needed again after TermWorker is updated).",
            exe.display()
        ),
        Err(_) => format!("{first}."),
    }
}

fn bind_error(e: &io::Error, port: u16) -> String {
    let alt = "a port above 1023 such as 6969 (U-Boot: setenv tftpdstp 6969)";
    if e.kind() == io::ErrorKind::AddrInUse {
        return format!("Port {port} is already in use, probably by another TFTP server. Close it, or use {alt}.");
    }
    if e.kind() == io::ErrorKind::PermissionDenied {
        if cfg!(unix) && port < 1024 {
            return low_port_help(port, alt);
        }
        return format!("Port {port} is in use or reserved by another program. Close it, or use {alt}.");
    }
    format!("Cannot listen on port {port}: {e}")
}

impl Shared {
    fn now_ms() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
    }

    fn begin(&self, peer: SocketAddr, file: &str, upload: bool) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let info = TransferInfo {
            id,
            peer: peer.ip().to_string(),
            file: file.to_string(),
            upload,
            size: None,
            bytes: 0,
            block_size: DEFAULT_BLKSIZE,
            state: TransferState::Running,
            error: None,
            started: Self::now_ms(),
            elapsed_ms: 0,
        };
        let mut list = self.transfers.lock().unwrap();
        list.push_back(info);
        while list.len() > KEEP_TRANSFERS {
            list.pop_front();
        }
        id
    }

    fn update(&self, id: u64, f: impl FnOnce(&mut TransferInfo)) {
        let info = {
            let mut list = self.transfers.lock().unwrap();
            let Some(t) = list.iter_mut().find(|t| t.id == id) else { return };
            // The time stops when the transfer ends (an upload's final dally does not count).
            let was_running = t.state == TransferState::Running;
            f(t);
            if was_running {
                t.elapsed_ms = Self::now_ms().saturating_sub(t.started);
            }
            t.clone()
        };
        (self.notify)(TftpEvent::Transfer { transfer: info });
    }
}

// ------------------------------------------------------------------ packets

#[derive(Debug, PartialEq, Eq)]
struct Request {
    write: bool,
    file: String,
    netascii: bool,
    /// Option names lowercased, in the order sent.
    options: Vec<(String, String)>,
}

fn parse_request(p: &[u8]) -> Result<Request, String> {
    if p.len() < 4 {
        return Err("short request".into());
    }
    let op = u16::from_be_bytes([p[0], p[1]]);
    if op != RRQ && op != WRQ {
        return Err(format!("unexpected opcode {op}"));
    }
    let mut fields = p[2..].split(|&b| b == 0).map(|f| String::from_utf8_lossy(f).into_owned());
    let file = fields.next().filter(|f| !f.is_empty()).ok_or("no file name")?;
    let mode = fields.next().ok_or("no transfer mode")?.to_ascii_lowercase();
    let netascii = match mode.as_str() {
        "octet" => false,
        "netascii" => true,
        other => return Err(format!("unsupported mode \"{other}\"")),
    };
    let rest: Vec<String> = fields.collect();
    let options = rest
        .chunks(2)
        .filter(|kv| kv.len() == 2 && !kv[0].is_empty())
        .map(|kv| (kv[0].to_ascii_lowercase(), kv[1].clone()))
        .collect();
    Ok(Request { write: op == WRQ, file, netascii, options })
}

fn packet(op: u16, block: u16, data: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + data.len());
    p.extend_from_slice(&op.to_be_bytes());
    p.extend_from_slice(&block.to_be_bytes());
    p.extend_from_slice(data);
    p
}

fn error_packet(code: u16, msg: &str) -> Vec<u8> {
    let mut p = packet(ERROR, code, msg.as_bytes());
    p.push(0);
    p
}

fn oack(options: &[(String, String)]) -> Vec<u8> {
    let mut p = OACK.to_be_bytes().to_vec();
    for (k, v) in options {
        p.extend_from_slice(k.as_bytes());
        p.push(0);
        p.extend_from_slice(v.as_bytes());
        p.push(0);
    }
    p
}

fn opcode(p: &[u8]) -> u16 {
    if p.len() < 2 {
        0
    } else {
        u16::from_be_bytes([p[0], p[1]])
    }
}

fn block_of(p: &[u8]) -> Option<u16> {
    (p.len() >= 4).then(|| u16::from_be_bytes([p[2], p[3]]))
}

fn error_text(p: &[u8]) -> String {
    let msg = String::from_utf8_lossy(p.get(4..).unwrap_or_default());
    let msg = msg.trim_end_matches('\0');
    format!("the device cancelled: {}", if msg.is_empty() { "no reason given" } else { msg })
}

/// Where a requested name lives in the shared folder. Leading slashes and either
/// separator are accepted ("/fw.bin", "boards\\a.bin"); nothing may leave the folder.
fn resolve(root: &Path, name: &str) -> Result<PathBuf, &'static str> {
    let mut out = root.to_path_buf();
    let mut any = false;
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return Err("Access violation: the name leaves the TFTP folder"),
            p => {
                if p.contains(':') || Path::new(p).components().any(|c| !matches!(c, Component::Normal(_))) {
                    return Err("Access violation: not a plain file name");
                }
                out.push(p);
                any = true;
            }
        }
    }
    if !any {
        return Err("No file name");
    }
    Ok(out)
}

/// Inside the root once links are followed (for a file or folder that exists).
fn inside(root: &Path, path: &Path) -> bool {
    path.canonicalize().map(|p| p.starts_with(root)).unwrap_or(false)
}

fn to_netascii(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 16);
    for &b in data {
        match b {
            b'\n' => out.extend_from_slice(b"\r\n"),
            b'\r' => out.extend_from_slice(b"\r\0"),
            b => out.push(b),
        }
    }
    out
}

fn from_netascii(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == b'\r' && i + 1 < data.len() && (data[i + 1] == b'\n' || data[i + 1] == 0) {
            out.push(if data[i + 1] == b'\n' { b'\n' } else { b'\r' });
            i += 2;
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

// ------------------------------------------------------------------ serving

async fn serve(
    socket: std::net::UdpSocket,
    ip: IpAddr,
    root: PathBuf,
    config: TftpConfig,
    shared: Arc<Shared>,
    mut stop: watch::Receiver<bool>,
) {
    let socket = match UdpSocket::from_std(socket) {
        Ok(s) => s,
        Err(e) => return log::error!("tftp: {e}"),
    };
    let mut buf = vec![0u8; 2048];
    let for_tasks = stop.clone();
    loop {
        tokio::select! {
            r = socket.recv_from(&mut buf) => {
                let (n, peer) = match r {
                    Ok(x) => x,
                    // Windows reports an ICMP "port unreachable" from an earlier send as an error here.
                    Err(_) => continue,
                };
                let req = match parse_request(&buf[..n]) {
                    Ok(r) => r,
                    Err(e) => {
                        log::debug!("tftp: bad request from {peer}: {e}");
                        let _ = socket.send_to(&error_packet(4, &format!("Illegal TFTP operation: {e}")), peer).await;
                        continue;
                    }
                };
                let (shared, root, config, stop) = (shared.clone(), root.clone(), config.clone(), for_tasks.clone());
                tokio::spawn(async move {
                    let sock = match UdpSocket::bind(SocketAddr::new(ip, 0)).await {
                        Ok(s) => s,
                        Err(e) => return log::error!("tftp: no port for a transfer: {e}"),
                    };
                    let id = shared.begin(peer, &req.file, req.write);
                    let started = Instant::now();
                    let mut t = Transfer { sock, peer, shared: shared.clone(), id, stop, timeout: DEFAULT_TIMEOUT, last_progress: started };
                    let result = if req.write { t.receive(&root, &req, config.allow_upload).await } else { t.send(&root, &req).await };
                    shared.update(id, |info| match result {
                        Ok(()) => {
                            if info.state == TransferState::Running {
                                info.state = TransferState::Done;
                            }
                        }
                        Err(e) => {
                            info.state = TransferState::Failed;
                            info.error = Some(e);
                        }
                    });
                    log::info!("tftp: {} {} {} in {:?}", peer, if req.write { "put" } else { "get" }, req.file, started.elapsed());
                });
            }
            _ = stop.wait_for(|v| *v) => break,
        }
    }
}

struct Transfer {
    sock: UdpSocket,
    peer: SocketAddr,
    shared: Arc<Shared>,
    id: u64,
    stop: watch::Receiver<bool>,
    timeout: Duration,
    last_progress: Instant,
}

/// What a received packet means to the exchange in progress.
enum Reply {
    Accept,
    Ignore,
    /// Send our last packet again (a repeated DATA: our ACK was lost).
    Resend,
}

impl Transfer {
    async fn fail(&self, code: u16, msg: &str) -> String {
        let _ = self.sock.send_to(&error_packet(code, msg), self.peer).await;
        msg.to_string()
    }

    /// Sends `out` and waits for a packet `judge` accepts, sending `out` again on
    /// silence. Returns the accepted packet; the device's ERROR ends it.
    async fn exchange(&mut self, out: &[u8], judge: impl Fn(&[u8]) -> Reply) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; MAX_BLKSIZE + 4];
        for _ in 0..=RETRIES {
            self.sock.send_to(out, self.peer).await.map_err(|e| format!("Network error: {e}"))?;
            let deadline = tokio::time::Instant::now() + self.timeout;
            loop {
                let r = tokio::select! {
                    r = tokio::time::timeout_at(deadline, self.sock.recv_from(&mut buf)) => Some(r),
                    _ = self.stop.wait_for(|v| *v) => None,
                };
                let Some(r) = r else {
                    return Err(self.fail(0, "The TFTP server was stopped").await);
                };
                let (n, from) = match r {
                    Err(_) => break, // timed out: send again
                    Ok(Err(_)) => continue,
                    Ok(Ok(x)) => x,
                };
                if from != self.peer {
                    let _ = self.sock.send_to(&error_packet(5, "Unknown transfer ID"), from).await;
                    continue;
                }
                let p = &buf[..n];
                if opcode(p) == ERROR {
                    return Err(error_text(p));
                }
                match judge(p) {
                    Reply::Accept => return Ok(p.to_vec()),
                    Reply::Ignore => {}
                    Reply::Resend => {
                        self.sock.send_to(out, self.peer).await.map_err(|e| format!("Network error: {e}"))?;
                    }
                }
            }
        }
        Err(self.fail(0, "Timed out: the device stopped answering").await)
    }

    fn progress(&mut self, bytes: u64, last: bool) {
        if last || self.last_progress.elapsed() >= PROGRESS_EVERY {
            self.last_progress = Instant::now();
            self.shared.update(self.id, |t| t.bytes = bytes);
        }
    }

    /// Accepted options and the block size they set. `size` answers tsize on a read.
    fn negotiate(&mut self, req: &Request, size: Option<u64>) -> (Vec<(String, String)>, usize, Option<u64>) {
        let mut accepted = Vec::new();
        let mut blksize = DEFAULT_BLKSIZE;
        let mut tsize = None;
        for (k, v) in &req.options {
            match k.as_str() {
                "blksize" => {
                    if let Ok(n) = v.parse::<usize>() {
                        if n >= 8 {
                            blksize = n.min(MAX_BLKSIZE);
                            accepted.push((k.clone(), blksize.to_string()));
                        }
                    }
                }
                "timeout" => {
                    if let Ok(s) = v.parse::<u64>() {
                        if (1..=255).contains(&s) {
                            self.timeout = Duration::from_secs(s);
                            accepted.push((k.clone(), s.to_string()));
                        }
                    }
                }
                "tsize" => match size {
                    Some(n) if !req.write => {
                        tsize = Some(n);
                        accepted.push((k.clone(), n.to_string()));
                    }
                    _ if req.write => {
                        tsize = v.parse().ok();
                        accepted.push((k.clone(), v.clone()));
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        (accepted, blksize, tsize)
    }

    /// A device reads a file (RRQ).
    async fn send(&mut self, root: &Path, req: &Request) -> Result<(), String> {
        let path = match resolve(root, &req.file) {
            Ok(p) => p,
            Err(e) => return Err(self.fail(2, e).await),
        };
        if !path.is_file() || !inside(root, &path) {
            return Err(self.fail(1, "File not found").await);
        }
        let mut source: Box<dyn Read + Send> = match std::fs::File::open(&path) {
            Ok(f) if req.netascii => {
                let mut data = Vec::new();
                let mut f = f;
                if let Err(e) = f.read_to_end(&mut data) {
                    return Err(self.fail(0, &format!("Cannot read the file: {e}")).await);
                }
                Box::new(io::Cursor::new(to_netascii(&data)))
            }
            Ok(f) => Box::new(io::BufReader::with_capacity(256 << 10, f)),
            Err(e) => return Err(self.fail(2, &format!("Cannot open the file: {e}")).await),
        };
        let size = if req.netascii { None } else { std::fs::metadata(&path).ok().map(|m| m.len()) };
        let (accepted, blksize, _) = self.negotiate(req, size);
        let total = size;
        self.shared.update(self.id, |t| {
            t.size = total;
            t.block_size = blksize;
        });

        if !accepted.is_empty() {
            self.exchange(&oack(&accepted), |p| {
                if opcode(p) == ACK && block_of(p) == Some(0) {
                    Reply::Accept
                } else {
                    Reply::Ignore
                }
            })
            .await?;
        }

        let mut block: u64 = 1;
        let mut sent: u64 = 0;
        let mut chunk = vec![0u8; blksize];
        loop {
            let n = read_full(&mut source, &mut chunk).map_err(|e| format!("Cannot read the file: {e}"))?;
            let wire = (block & 0xffff) as u16;
            let out = packet(DATA, wire, &chunk[..n]);
            self.exchange(&out, |p| {
                if opcode(p) == ACK && block_of(p) == Some(wire) {
                    Reply::Accept
                } else {
                    Reply::Ignore // duplicate ACKs are not answered (Sorcerer's Apprentice)
                }
            })
            .await?;
            sent += n as u64;
            let last = n < blksize;
            self.progress(sent, last);
            if last {
                return Ok(());
            }
            block += 1;
        }
    }

    /// A device writes a file (WRQ).
    async fn receive(&mut self, root: &Path, req: &Request, allowed: bool) -> Result<(), String> {
        if !allowed {
            return Err(self.fail(2, "Uploads are turned off in TermWorker's TFTP server").await);
        }
        let path = match resolve(root, &req.file) {
            Ok(p) => p,
            Err(e) => return Err(self.fail(2, e).await),
        };
        if path.exists() {
            return Err(self.fail(6, "File already exists").await);
        }
        let dir = path.parent().unwrap_or(root).to_path_buf();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return Err(self.fail(2, &format!("Cannot create the folder: {e}")).await);
        }
        if !inside(root, &dir) {
            return Err(self.fail(2, "Access violation: the name leaves the TFTP folder").await);
        }
        let part = dir
            .join(format!(".{}.part", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
        let mut file = match std::fs::File::create(&part) {
            Ok(f) => io::BufWriter::new(f),
            Err(e) => return Err(self.fail(2, &format!("Cannot create the file: {e}")).await),
        };

        let received = self.receive_into(req, &mut file).await;
        let saved = received.and_then(|last| {
            file.flush().map_err(|e| format!("Cannot write the file: {e}"))?;
            Ok(last)
        });
        drop(file);
        let saved = saved.and_then(|last| {
            if req.netascii {
                let data = std::fs::read(&part).map_err(|e| e.to_string())?;
                std::fs::write(&part, from_netascii(&data)).map_err(|e| e.to_string())?;
            }
            if path.exists() {
                return Err("File already exists".to_string());
            }
            std::fs::rename(&part, &path).map_err(|e| format!("Cannot save the file: {e}"))?;
            Ok(last)
        });
        match saved {
            // The device hears "done" only once the file is in place.
            Ok((ack, wire, blksize)) => {
                // The file is in place: done now, then answer a lost final ACK for a while.
                self.shared.update(self.id, |t| t.state = TransferState::Done);
                self.finish_upload(&ack, wire, blksize).await;
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                if !e.starts_with("the device") && !e.starts_with("Timed out") && !e.starts_with("The TFTP") {
                    let _ = self.sock.send_to(&error_packet(3, &e), self.peer).await;
                }
                Err(e)
            }
        }
    }

    /// Final ACK; answers a repeat of the last block in case the ACK was lost.
    async fn finish_upload(&mut self, ack: &[u8], wire: u16, blksize: usize) {
        let _ = self.sock.send_to(ack, self.peer).await;
        let mut buf = vec![0u8; blksize + 4];
        let until = tokio::time::Instant::now() + self.timeout;
        while let Ok(Ok((n, from))) = tokio::time::timeout_at(until, self.sock.recv_from(&mut buf)).await {
            if from == self.peer && opcode(&buf[..n]) == DATA && block_of(&buf[..n]) == Some(wire) {
                let _ = self.sock.send_to(ack, self.peer).await;
            }
        }
    }

    /// Receives every block; returns the final ACK (not yet sent), its block number and the block size.
    async fn receive_into(&mut self, req: &Request, file: &mut impl Write) -> Result<(Vec<u8>, u16, usize), String> {
        let (accepted, blksize, tsize) = self.negotiate(req, None);
        self.shared.update(self.id, |t| {
            t.size = tsize;
            t.block_size = blksize;
        });
        let mut out = if accepted.is_empty() { packet(ACK, 0, &[]) } else { oack(&accepted) };
        let mut block: u64 = 1;
        let mut got: u64 = 0;
        loop {
            let wire = (block & 0xffff) as u16;
            let prev = wire.wrapping_sub(1);
            let p = self
                .exchange(&out, |p| match (opcode(p), block_of(p)) {
                    (DATA, Some(b)) if b == wire => Reply::Accept,
                    (DATA, Some(b)) if b == prev => Reply::Resend,
                    _ => Reply::Ignore,
                })
                .await?;
            let data = &p[4..];
            if data.len() > blksize {
                return Err(self.fail(4, "Block larger than agreed").await);
            }
            if let Err(e) = file.write_all(data) {
                return Err(self.fail(3, &format!("Cannot write the file: {e}")).await);
            }
            got += data.len() as u64;
            out = packet(ACK, wire, &[]);
            let last = data.len() < blksize;
            self.progress(got, last);
            if last {
                return Ok((out, wire, blksize));
            }
            block += 1;
        }
    }
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

// ------------------------------------------------------------------ commands

pub mod commands {
    use super::*;
    use tauri::{AppHandle, Manager, State};

    type R<T> = Result<T, String>;

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Snapshot {
        status: ServerStatus,
        transfers: Vec<TransferInfo>,
        addresses: Vec<LocalAddress>,
    }

    #[tauri::command]
    pub fn tftp_snapshot(server: State<'_, TftpServer>) -> Snapshot {
        Snapshot { status: server.status(), transfers: server.transfers(), addresses: local_addresses() }
    }

    #[tauri::command]
    pub fn tftp_start(server: State<'_, TftpServer>, config: TftpConfig) -> R<ServerStatus> {
        server.start(config)
    }

    #[tauri::command]
    pub fn tftp_stop(server: State<'_, TftpServer>) {
        server.stop()
    }

    #[tauri::command]
    pub fn tftp_addresses() -> Vec<LocalAddress> {
        local_addresses()
    }

    /// `Documents/TermWorker/tftp`, created if missing.
    #[tauri::command]
    pub fn tftp_default_folder(app: AppHandle) -> R<String> {
        let base = app.path().document_dir().or_else(|_| app.path().home_dir()).map_err(|e| e.to_string())?;
        let dir = base.join("TermWorker").join("tftp");
        std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        Ok(dir.display().to_string())
    }

    #[derive(Serialize)]
    pub struct FileEntry {
        name: String,
        size: u64,
    }

    /// Files directly in the folder (not hidden ones), for the panel's list.
    #[tauri::command]
    pub fn tftp_list_files(folder: String) -> R<Vec<FileEntry>> {
        let mut out: Vec<FileEntry> = std::fs::read_dir(&folder)
            .map_err(|e| format!("Cannot read {folder}: {e}"))?
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let meta = e.metadata().ok()?;
                (meta.is_file() && !name.starts_with('.')).then_some(FileEntry { name, size: meta.len() })
            })
            .collect();
        out.sort_by_key(|f| f.name.to_lowercase());
        out.truncate(500);
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
