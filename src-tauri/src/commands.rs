//! Tauri commands: the complete API surface the UI talks to.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};

use crate::config::{Connection, TerminalOptions};
use crate::logger::LogMode;
use crate::profiles;
use crate::script::{Scripts, Tap};
use crate::session::{OpenParams, Session, SessionEvent};
use crate::transfer::{Job, Protocol};
use crate::transport::serial::{self, PortInfo};
use crate::transport::HostKeyDecision;

pub struct AppState {
    sessions: Mutex<HashMap<u32, Arc<Session>>>,
    next_id: AtomicU32,
    config_dir: PathBuf,
    /// UI tab → its current backend session (a reconnect replaces it).
    tabs: Mutex<HashMap<String, u32>>,
    /// UI tab → what scripts watching it receive.
    taps: Mutex<HashMap<String, Arc<Tap>>>,
    scripts: Scripts,
}

impl AppState {
    pub fn new(config_dir: PathBuf) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            next_id: AtomicU32::new(1),
            config_dir,
            tabs: Mutex::new(HashMap::new()),
            taps: Mutex::new(HashMap::new()),
            scripts: Scripts::default(),
        }
    }

    fn get(&self, id: u32) -> Result<Arc<Session>, String> {
        self.sessions.lock().unwrap().get(&id).cloned().ok_or_else(|| format!("No session {id}"))
    }

    pub fn session_for_tab(&self, tab: &str) -> Result<Arc<Session>, String> {
        let id = self.tabs.lock().unwrap().get(tab).copied();
        id.and_then(|id| self.sessions.lock().unwrap().get(&id).cloned())
            .ok_or_else(|| "The session is not connected".to_string())
    }

    pub fn tap(&self, tab: &str) -> Arc<Tap> {
        self.taps.lock().unwrap().entry(tab.to_string()).or_insert_with(|| Arc::new(Tap::new(tab))).clone()
    }

    pub fn scripts(&self) -> &Scripts {
        &self.scripts
    }
}

type R<T> = Result<T, String>;

#[tauri::command]
pub fn list_serial_ports() -> Vec<PortInfo> {
    serial::list_ports()
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn session_open(
    state: State<'_, AppState>,
    tab: String,
    connection: Connection,
    options: TerminalOptions,
    secret: Option<String>,
    cols: u16,
    rows: u16,
    on_event: Channel<SessionEvent>,
) -> R<u32> {
    log::info!("open session: {}", connection.describe());
    let session = Session::open(
        OpenParams {
            connection,
            options,
            secret,
            cols,
            rows,
            known_hosts: state.config_dir.join("known_hosts"),
            tap: state.tap(&tab),
        },
        on_event,
    )?;
    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    state.sessions.lock().unwrap().insert(id, session);
    state.tabs.lock().unwrap().insert(tab, id);
    Ok(id)
}

#[tauri::command]
pub fn session_write(state: State<'_, AppState>, id: u32, data: String, binary: Option<bool>) -> R<()> {
    let s = state.get(id)?;
    if binary.unwrap_or(false) {
        // xterm.js reports some mouse sequences as "binary" strings: one char per byte.
        s.write_bytes(data.chars().map(|c| c as u32 as u8).collect())
    } else {
        s.write_text(&data)
    }
}

#[tauri::command]
pub fn session_resize(state: State<'_, AppState>, id: u32, cols: u16, rows: u16) -> R<()> {
    state.get(id)?.resize(cols, rows)
}

#[tauri::command]
pub fn session_close(state: State<'_, AppState>, id: u32) {
    state.tabs.lock().unwrap().retain(|_, v| *v != id);
    if let Some(s) = state.sessions.lock().unwrap().remove(&id) {
        s.close();
    }
}

/// The tab is gone for good (not just reconnecting): scripts stop hearing from it.
#[tauri::command]
pub fn tab_closed(state: State<'_, AppState>, tab: String) {
    if let Some(tap) = state.taps.lock().unwrap().remove(&tab) {
        tap.status(crate::transport::LinkState::Disconnected, Some("the tab was closed"));
    }
}

#[tauri::command]
pub fn session_set_signal(state: State<'_, AppState>, id: u32, signal: String, value: bool) -> R<()> {
    let s = state.get(id)?;
    match signal.as_str() {
        "dtr" => s.set_dtr(value),
        "rts" => s.set_rts(value),
        other => Err(format!("Unknown signal {other}")),
    }
}

#[tauri::command]
pub fn session_send_break(state: State<'_, AppState>, id: u32) -> R<()> {
    state.get(id)?.send_break()
}

#[tauri::command]
pub fn session_set_encoding(state: State<'_, AppState>, id: u32, encoding: String) -> R<String> {
    state.get(id)?.set_encoding(&encoding)
}

#[tauri::command]
pub fn session_hostkey_answer(state: State<'_, AppState>, id: u32, decision: HostKeyDecision) -> R<()> {
    state.get(id)?.answer_hostkey(decision)
}

#[tauri::command]
pub fn log_start(state: State<'_, AppState>, id: u32, path: String, mode: LogMode) -> R<String> {
    state.get(id)?.start_log(&PathBuf::from(path), mode)
}

#[tauri::command]
pub fn log_stop(state: State<'_, AppState>, id: u32) -> R<()> {
    state.get(id)?.stop_log();
    Ok(())
}

/// Suggested log file: `Documents/TermWorker/logs/<name>_<YYYYmmdd_HHMMSS>.log`.
#[tauri::command]
pub fn default_log_path(app: AppHandle, name: String) -> R<String> {
    let base = app.path().document_dir().or_else(|_| app.path().home_dir()).map_err(|e| e.to_string())?;
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let file = format!("{}_{stamp}.log", log_file_stem(&name));
    Ok(base.join("TermWorker").join("logs").join(file).display().to_string())
}

/// A tab title made safe for a file name: "/dev/ttyUSB0" → "ttyUSB0", "root@10.0.0.1" → "root_10.0.0.1".
fn log_file_stem(name: &str) -> String {
    let name = name.trim();
    let name = name.strip_prefix("/dev/").unwrap_or(name);
    let safe: String =
        name.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect();
    let safe = safe.trim_matches(|c| c == '_' || c == '.');
    if safe.is_empty() {
        "session".into()
    } else {
        safe.into()
    }
}

#[tauri::command]
pub fn profiles_load(state: State<'_, AppState>) -> R<serde_json::Value> {
    profiles::load(&state.config_dir)
}

#[tauri::command]
pub fn profiles_save(state: State<'_, AppState>, data: serde_json::Value) -> R<()> {
    profiles::save(&state.config_dir, &data)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    version: &'static str,
    config_dir: String,
}

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> AppInfo {
    AppInfo { version: env!("CARGO_PKG_VERSION"), config_dir: state.config_dir.display().to_string() }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Send,
    Receive,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferRequest {
    protocol: Protocol,
    direction: Direction,
    /// Files to send.
    #[serde(default)]
    files: Vec<String>,
    /// Where received files go: a folder (YMODEM/ZMODEM) or a file (XMODEM).
    dest: Option<String>,
    /// ZMODEM: the remote `rz` already announced itself.
    #[serde(default)]
    remote_ready: bool,
}

#[tauri::command]
pub fn transfer_start(state: State<'_, AppState>, id: u32, request: TransferRequest) -> R<()> {
    let session = state.get(id)?;
    let job = match request.direction {
        Direction::Send => {
            if request.files.is_empty() {
                return Err("Choose at least one file to send".into());
            }
            let files: Vec<PathBuf> = request.files.iter().map(PathBuf::from).collect();
            if let Some(missing) = files.iter().find(|p| !p.is_file()) {
                return Err(format!("Not a file: {}", missing.display()));
            }
            if matches!(request.protocol, Protocol::Xmodem | Protocol::Xmodem1k) && files.len() > 1 {
                return Err("XMODEM sends one file at a time; use YMODEM or ZMODEM for several".into());
            }
            Job::Send { protocol: request.protocol, files, remote_ready: request.remote_ready }
        }
        Direction::Receive => {
            let dest = PathBuf::from(request.dest.ok_or("Choose where to save the received files")?);
            if !matches!(request.protocol, Protocol::Xmodem | Protocol::Xmodem1k) {
                std::fs::create_dir_all(&dest).map_err(|e| format!("Cannot use folder {}: {e}", dest.display()))?;
            }
            Job::Receive { protocol: request.protocol, dest }
        }
    };
    log::info!("transfer: {job:?}");
    session.start_transfer(job)
}

#[tauri::command]
pub fn hdlc_set(state: State<'_, AppState>, id: u32, config: Option<crate::hdlc::HdlcConfig>) -> R<()> {
    state.get(id)?.set_hdlc(config);
    Ok(())
}

#[tauri::command]
pub fn hdlc_send(state: State<'_, AppState>, id: u32, payload: Vec<u8>) -> R<()> {
    state.get(id)?.hdlc_send(payload)
}

#[tauri::command]
pub fn hdlc_send_file(state: State<'_, AppState>, id: u32, path: String, chunk: usize, gap_ms: u64) -> R<()> {
    state.get(id)?.hdlc_send_file(std::path::Path::new(&path), chunk, gap_ms)
}

#[tauri::command]
pub fn hdlc_cancel_send(state: State<'_, AppState>, id: u32) -> R<()> {
    state.get(id)?.hdlc_cancel_send();
    Ok(())
}

#[tauri::command]
pub fn hdlc_record(state: State<'_, AppState>, id: u32, path: Option<String>) -> R<()> {
    state.get(id)?.hdlc_record(path.as_deref().map(std::path::Path::new))
}

#[tauri::command]
pub fn transfer_cancel(state: State<'_, AppState>, id: u32) -> R<()> {
    state.get(id)?.cancel_transfer();
    Ok(())
}

#[tauri::command]
pub fn zmodem_decline(state: State<'_, AppState>, id: u32) -> R<()> {
    state.get(id)?.decline_zmodem()
}

/// Suggested folder for received files: `Downloads`, or the home folder.
#[tauri::command]
pub fn default_download_dir(app: AppHandle) -> R<String> {
    let dir = app.path().download_dir().or_else(|_| app.path().home_dir()).map_err(|e| e.to_string())?;
    Ok(dir.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::log_file_stem;

    #[test]
    fn log_names_are_tidy() {
        assert_eq!(log_file_stem("/dev/ttyUSB2"), "ttyUSB2");
        assert_eq!(log_file_stem("COM4"), "COM4");
        assert_eq!(log_file_stem("root@10.0.0.1"), "root_10.0.0.1");
        assert_eq!(log_file_stem("/dev/serial/by-id/usb-FTDI"), "serial_by-id_usb-FTDI");
        assert_eq!(log_file_stem("라우터 콘솔"), "라우터_콘솔");
        assert_eq!(log_file_stem("///"), "session");
        assert_eq!(log_file_stem(""), "session");
    }
}
