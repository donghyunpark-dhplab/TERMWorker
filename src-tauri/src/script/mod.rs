//! Scripts: JavaScript files run by an embedded QuickJS engine (rquickjs).
//!
//! Each run gets its own thread with a tokio current-thread runtime and its own
//! QuickJS runtime, so a busy or stuck script never blocks the UI or another run.
//! The API scripts see is written in JavaScript (`prelude.js`) on top of a few
//! native functions on a hidden `__tw` object:
//!
//! - Text and HDLC frames received by a tab reach the run through a [`Tap`]
//!   into one queue per run (`next`).
//! - Sends go straight to the backend session of a tab.
//! - Anything that changes what the UI shows (dialogs, opening tabs, logging,
//!   transfers, DTR/RTS, encoding) is a request to the UI (`script:request`),
//!   answered with the `script_reply` command.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rquickjs::prelude::Async;
use rquickjs::{
    AsyncContext, AsyncRuntime, CatchResultExt, CaughtError, Coerced, Ctx, Exception, FromJs, Function, Module, Object,
};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tokio::sync::{mpsc, oneshot, watch};

use crate::commands::AppState;
use crate::session::Session;
use crate::transport::LinkState;

const PRELUDE: &str = include_str!("prelude.js");
/// Queue item that ends the event pump of a run.
const STOP: &str = "\u{0}stop";
/// Error text the prelude turns into a "script stopped" error.
const STOPPED: &str = "\u{0}stopped";
const MEMORY_LIMIT: usize = 256 << 20;
const STACK_LIMIT: usize = 1 << 20;
/// After the script's top-level code finishes, pending callbacks get this long to settle.
const SETTLE: Duration = Duration::from_secs(2);

// ------------------------------------------------------------------ taps

/// Fan-out of one tab's received text, link state and HDLC frames to the scripts watching it.
/// Lives as long as the tab: a reconnect makes a new backend session with the same tap.
pub struct Tap {
    tab: String,
    subs: Mutex<Vec<(u64, mpsc::UnboundedSender<String>)>>,
    last_state: Mutex<Option<(LinkState, Option<String>)>>,
}

impl Tap {
    pub fn new(tab: &str) -> Self {
        Self { tab: tab.to_string(), subs: Mutex::new(Vec::new()), last_state: Mutex::new(None) }
    }

    /// Adds a run's queue (once per run). The run first hears the current link state.
    fn watch(&self, run: u64, tx: mpsc::UnboundedSender<String>) {
        let mut subs = self.subs.lock().unwrap();
        if subs.iter().any(|(r, _)| *r == run) {
            return;
        }
        if let Some((state, message)) = self.last_state.lock().unwrap().clone() {
            let _ = tx.send(self.status_json(state, message.as_deref()));
        }
        subs.push((run, tx));
    }

    fn publish(&self, make: impl FnOnce() -> String) {
        let mut subs = self.subs.lock().unwrap();
        if subs.is_empty() {
            return;
        }
        let msg = make();
        // A finished run has dropped its queue: forget it.
        subs.retain(|(_, tx)| tx.send(msg.clone()).is_ok());
    }

    fn status_json(&self, state: LinkState, message: Option<&str>) -> String {
        json!({ "t": "status", "tab": self.tab, "state": state, "message": message }).to_string()
    }

    pub fn data(&self, text: &str) {
        self.publish(|| json!({ "t": "data", "tab": self.tab, "text": text }).to_string());
    }

    /// Bytes as received, before character decoding (HDLC frames included).
    /// Sent as a string of chars U+0000–U+00FF, one per byte: far smaller than a number array.
    pub fn bytes(&self, bytes: &[u8]) {
        self.publish(|| {
            let data: String = bytes.iter().map(|&b| b as char).collect();
            json!({ "t": "bytes", "tab": self.tab, "data": data }).to_string()
        });
    }

    pub fn status(&self, state: LinkState, message: Option<&str>) {
        *self.last_state.lock().unwrap() = Some((state, message.map(str::to_owned)));
        self.publish(|| self.status_json(state, message));
    }

    pub fn frame(&self, payload: &[u8], fcs_ok: bool) {
        self.publish(|| json!({ "t": "frame", "tab": self.tab, "payload": payload, "fcsOk": fcs_ok }).to_string());
    }
}

// ------------------------------------------------------------------ runs

#[derive(Default)]
pub struct Scripts {
    next_run: AtomicU64,
    runs: Mutex<HashMap<u64, Arc<Run>>>,
}

struct Run {
    id: u64,
    /// Folder of the script: relative paths in the script are resolved against it.
    dir: PathBuf,
    /// Makes running JavaScript throw (QuickJS interrupt handler).
    halt: Arc<AtomicBool>,
    /// Milliseconds after `started` from which running JavaScript is halted too.
    halt_after: Arc<AtomicU64>,
    started: Instant,
    /// The user pressed Stop.
    stopped_by_user: AtomicBool,
    /// Ends pending native calls (`next`, `request`).
    cancel: watch::Sender<bool>,
    queue: mpsc::UnboundedSender<String>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next_request: AtomicU64,
}

impl Run {
    fn resolve(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.dir.join(p)
        }
    }

    fn halt(&self) {
        self.halt.store(true, Ordering::SeqCst);
        let _ = self.cancel.send(true);
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RequestEvent {
    run: u64,
    id: u64,
    kind: String,
    args: Value,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct OutputEvent<'a> {
    run: u64,
    stream: &'a str,
    text: &'a str,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct StateEvent {
    run: u64,
    name: String,
    path: String,
    running: bool,
    /// `done`, `failed` or `stopped` once finished.
    outcome: Option<&'static str>,
    message: Option<String>,
}

type R<T> = Result<T, String>;

/// Starts `path`; `tab` is the tab the user ran it from (the script's `tab`).
#[tauri::command]
pub fn script_run<Rt: Runtime>(
    app: AppHandle<Rt>,
    state: State<'_, AppState>,
    path: String,
    tab: Option<String>,
    tab_name: Option<String>,
) -> R<u64> {
    let path = PathBuf::from(path);
    let source = std::fs::read_to_string(&path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let scripts = state.scripts();
    let id = scripts.next_run.fetch_add(1, Ordering::Relaxed) + 1;
    let (queue, queue_rx) = mpsc::unbounded_channel();
    let run = Arc::new(Run {
        id,
        dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
        halt: Arc::new(AtomicBool::new(false)),
        halt_after: Arc::new(AtomicU64::new(u64::MAX)),
        started: Instant::now(),
        stopped_by_user: AtomicBool::new(false),
        cancel: watch::channel(false).0,
        queue,
        pending: Mutex::new(HashMap::new()),
        next_request: AtomicU64::new(1),
    });
    scripts.runs.lock().unwrap().insert(id, run.clone());
    log::info!("script {id}: {}", path.display());

    std::thread::Builder::new()
        .name(format!("script-{id}"))
        .spawn(move || run_thread(app, run, path, source, tab.map(|t| (t, tab_name)), queue_rx))
        .map_err(|e| format!("Cannot start the script: {e}"))?;
    Ok(id)
}

#[tauri::command]
pub fn script_stop(state: State<'_, AppState>, run: u64) {
    if let Some(r) = state.scripts().runs.lock().unwrap().get(&run) {
        r.stopped_by_user.store(true, Ordering::SeqCst);
        r.halt();
    }
}

/// The UI's answer to a `script:request`: `value` on success, or an error message.
#[tauri::command]
pub fn script_reply(state: State<'_, AppState>, run: u64, id: u64, ok: bool, value: Value) {
    let Some(r) = state.scripts().runs.lock().unwrap().get(&run).cloned() else { return };
    let tx = r.pending.lock().unwrap().remove(&id);
    if let Some(tx) = tx {
        let _ = tx.send(if ok {
            Ok(value)
        } else {
            Err(value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string()))
        });
    }
}

type StartTab = Option<(String, Option<String>)>;

fn run_thread<Rt: Runtime>(
    app: AppHandle<Rt>,
    run: Arc<Run>,
    path: PathBuf,
    source: String,
    tab: StartTab,
    queue_rx: mpsc::UnboundedReceiver<String>,
) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "script".into());
    let state = |running, outcome, message| StateEvent {
        run: run.id,
        name: name.clone(),
        path: path.display().to_string(),
        running,
        outcome,
        message,
    };
    let _ = app.emit("script:state", state(true, None, None));

    let result = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt.block_on(execute(&app, &run, &name, source, tab, queue_rx)),
        Err(e) => Err(e.to_string()),
    };

    app.state::<AppState>().scripts().runs.lock().unwrap().remove(&run.id);
    run.pending.lock().unwrap().clear();
    let (outcome, message) = if run.stopped_by_user.load(Ordering::SeqCst) {
        ("stopped", None)
    } else {
        match result {
            Ok(()) => ("done", None),
            Err(e) => ("failed", Some(e)),
        }
    };
    log::info!("script {}: {outcome}", run.id);
    let _ = app.emit("script:state", state(false, Some(outcome), message));
}

async fn execute<Rt: Runtime>(
    app: &AppHandle<Rt>,
    run: &Arc<Run>,
    name: &str,
    source: String,
    tab: StartTab,
    queue_rx: mpsc::UnboundedReceiver<String>,
) -> R<()> {
    let qrt = AsyncRuntime::new().map_err(|e| e.to_string())?;
    qrt.set_memory_limit(MEMORY_LIMIT).await;
    qrt.set_max_stack_size(STACK_LIMIT).await;
    let (halt, halt_after, started) = (run.halt.clone(), run.halt_after.clone(), run.started);
    qrt.set_interrupt_handler(Some(Box::new(move || {
        halt.load(Ordering::Relaxed) || started.elapsed().as_millis() as u64 >= halt_after.load(Ordering::Relaxed)
    })))
    .await;
    let ctx = AsyncContext::full(&qrt).await.map_err(|e| e.to_string())?;
    let queue = Arc::new(tokio::sync::Mutex::new(queue_rx));

    let result: R<()> = ctx
        .async_with(async |ctx| {
            install(&ctx, app, run, queue, tab).catch(&ctx).map_err(describe)?;
            let prelude = Module::evaluate(ctx.clone(), "prelude.js", PRELUDE).catch(&ctx).map_err(describe)?;
            prelude.into_future::<()>().await.catch(&ctx).map_err(describe)?;
            let promise = Module::evaluate(ctx.clone(), name, source).catch(&ctx).map_err(describe)?;
            // A script can wait on a promise nothing will settle: Stop must end it anyway.
            let mut cancel = run.cancel.subscribe();
            tokio::select! {
                r = promise.into_future::<()>() => r.catch(&ctx).map_err(describe),
                _ = cancel.wait_for(|v| *v) => Err(STOPPED.to_string()),
            }
        })
        .await;

    // The top-level code is done: end the event pump and pending native calls, and
    // give what follows from that a moment. JavaScript still running after that
    // (a callback stuck in a loop) is halted by the interrupt handler.
    let _ = run.cancel.send(true);
    let settle_until = run.started.elapsed() + SETTLE;
    run.halt_after.store(settle_until.as_millis() as u64, Ordering::SeqCst);
    let _ = tokio::time::timeout(SETTLE * 2, qrt.idle()).await;
    drop(ctx);
    result
}

/// "TimeoutError: message" and the stack without the prelude's own frames.
fn describe(e: CaughtError<'_>) -> String {
    match e {
        CaughtError::Exception(ex) => {
            let name = ex.as_object().get::<_, Option<Coerced<String>>>("name").ok().flatten().map(|c| c.0);
            let mut out = format!("{}: {}", name.as_deref().unwrap_or("Error"), ex.message().unwrap_or_default());
            if let Some(stack) = ex.stack() {
                for line in stack.lines().filter(|l| !l.trim().is_empty() && !l.contains("prelude.js")) {
                    out.push('\n');
                    out.push_str(line);
                }
            }
            out
        }
        other => other.to_string(),
    }
}

fn session<Rt: Runtime>(app: &AppHandle<Rt>, tab: &str) -> R<Arc<Session>> {
    app.state::<AppState>().session_for_tab(tab)
}

fn tftp_server<Rt: Runtime>(app: &AppHandle<Rt>) -> R<State<'_, crate::tftp::TftpServer>> {
    app.try_state::<crate::tftp::TftpServer>().ok_or_else(|| "The TFTP server is not available".to_string())
}

fn throw<T>(ctx: &Ctx<'_>, r: R<T>) -> rquickjs::Result<T> {
    r.map_err(|e| Exception::throw_message(ctx, &e))
}

/// Native functions behind the prelude.
fn install<'js, Rt: Runtime>(
    ctx: &Ctx<'js>,
    app: &AppHandle<Rt>,
    run: &Arc<Run>,
    queue: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<String>>>,
    tab: StartTab,
) -> rquickjs::Result<()> {
    let tw = Object::new(ctx.clone())?;
    let (tab, tab_name) = tab.unzip();
    tw.set("startTab", tab)?;
    tw.set("startTabName", tab_name.flatten())?;
    tw.set("scriptDir", run.dir.display().to_string())?;

    // --- sessions
    let a = app.clone();
    tw.set(
        "write",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, tab: String, text: String, exact: bool| {
            throw(
                &ctx,
                session(&a, &tab).and_then(|s| if exact { s.write_text_exact(&text) } else { s.write_text(&text) }),
            )
        })?,
    )?;
    let a = app.clone();
    tw.set(
        "writeBytes",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, tab: String, bytes: Vec<u8>| {
            throw(&ctx, session(&a, &tab).and_then(|s| s.write_bytes(bytes)))
        })?,
    )?;
    let a = app.clone();
    tw.set(
        "sendBreak",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, tab: String| {
            throw(&ctx, session(&a, &tab).and_then(|s| s.send_break()))
        })?,
    )?;
    let a = app.clone();
    tw.set(
        "hdlcSend",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, tab: String, bytes: Vec<u8>| {
            throw(&ctx, session(&a, &tab).and_then(|s| s.hdlc_send(bytes)))
        })?,
    )?;
    let (a, r) = (app.clone(), run.clone());
    tw.set(
        "watch",
        Function::new(ctx.clone(), move |tab: String| {
            a.state::<AppState>().tap(&tab).watch(r.id, r.queue.clone());
        })?,
    )?;
    // A name for a tab the script is about to open, watched before the tab exists
    // so that not a byte of its output is missed.
    let (a, r) = (app.clone(), run.clone());
    let opened = AtomicU64::new(0);
    tw.set(
        "newTab",
        Function::new(ctx.clone(), move || {
            let tab = format!("script{}-{}", r.id, opened.fetch_add(1, Ordering::Relaxed) + 1);
            a.state::<AppState>().tap(&tab).watch(r.id, r.queue.clone());
            tab
        })?,
    )?;

    // --- event queue
    let r = run.clone();
    tw.set(
        "next",
        Function::new(
            ctx.clone(),
            Async(move |timeout_ms: f64| {
                let queue = queue.clone();
                let mut cancel = r.cancel.subscribe();
                async move {
                    if *cancel.borrow() {
                        return STOP.to_string();
                    }
                    let mut queue = queue.lock().await;
                    let sleep = async {
                        if timeout_ms.is_finite() && timeout_ms >= 0.0 {
                            tokio::time::sleep(Duration::from_secs_f64(timeout_ms / 1000.0)).await
                        } else {
                            std::future::pending::<()>().await
                        }
                    };
                    tokio::select! {
                        m = queue.recv() => m.unwrap_or_else(|| STOP.into()),
                        _ = sleep => String::new(),
                        _ = cancel.wait_for(|v| *v) => STOP.into(),
                    }
                }
            }),
        )?,
    )?;
    let r = run.clone();
    tw.set(
        "poke",
        Function::new(ctx.clone(), move || {
            let _ = r.queue.send(String::new());
        })?,
    )?;

    // --- requests to the UI
    let (a, r) = (app.clone(), run.clone());
    tw.set(
        "request",
        Function::new(
            ctx.clone(),
            Async(move |kind: String, args: String| {
                let (app, run) = (a.clone(), r.clone());
                async move {
                    let id = run.next_request.fetch_add(1, Ordering::Relaxed);
                    let args: Value = serde_json::from_str(&args).unwrap_or(Value::Null);
                    let args = if args.is_object() { args } else { json!({}) };
                    let (tx, rx) = oneshot::channel();
                    run.pending.lock().unwrap().insert(id, tx);
                    let mut cancel = run.cancel.subscribe();
                    let out = if app.emit("script:request", RequestEvent { run: run.id, id, kind, args }).is_err() {
                        Err("the window is gone".to_string())
                    } else if *cancel.borrow() {
                        Err(STOPPED.to_string())
                    } else {
                        tokio::select! {
                            r = rx => r.unwrap_or_else(|_| Err(STOPPED.to_string())),
                            _ = cancel.wait_for(|v| *v) => Err(STOPPED.to_string()),
                        }
                    };
                    run.pending.lock().unwrap().remove(&id);
                    match out {
                        Ok(v) => json!({ "ok": v }),
                        Err(e) => json!({ "err": e }),
                    }
                    .to_string()
                }
            }),
        )?,
    )?;

    // --- output
    let (a, r) = (app.clone(), run.clone());
    tw.set(
        "print",
        Function::new(ctx.clone(), move |stream: String, text: String| {
            let _ = a.emit("script:output", OutputEvent { run: r.id, stream: &stream, text: &text });
        })?,
    )?;

    // --- files (relative to the script's folder)
    let r = run.clone();
    tw.set("resolve", Function::new(ctx.clone(), move |p: String| r.resolve(&p).display().to_string())?)?;
    let r = run.clone();
    tw.set(
        "readText",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, p: String| {
            let path = r.resolve(&p);
            throw(&ctx, std::fs::read_to_string(&path).map_err(|e| format!("Cannot read {}: {e}", path.display())))
        })?,
    )?;
    let r = run.clone();
    tw.set(
        "readBytes",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, p: String| {
            let path = r.resolve(&p);
            throw(&ctx, std::fs::read(&path).map_err(|e| format!("Cannot read {}: {e}", path.display())))
        })?,
    )?;
    let r = run.clone();
    tw.set(
        "writeFile",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, p: String, data: rquickjs::Value<'js>, append: bool| {
            let path = r.resolve(&p);
            let data = match data.as_string() {
                Some(text) => text.to_string()?.into_bytes(),
                None => Vec::<u8>::from_js(&ctx, data)?,
            };
            throw(&ctx, write_file(&path, &data, append).map_err(|e| format!("Cannot write {}: {e}", path.display())))
        })?,
    )?;
    let r = run.clone();
    tw.set("exists", Function::new(ctx.clone(), move |p: String| r.resolve(&p).exists())?)?;

    // --- built-in TFTP server (one per app, shared with the TFTP panel)
    let (a, r) = (app.clone(), run.clone());
    tw.set(
        "tftpStart",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, config: String| {
            let out = tftp_server(&a).and_then(|server| {
                let mut config: crate::tftp::TftpConfig =
                    serde_json::from_str(&config).map_err(|e| format!("tftp.start: {e}"))?;
                config.folder = r.resolve(&config.folder).display().to_string();
                server.start(config).map(|s| json!(s).to_string())
            });
            throw(&ctx, out)
        })?,
    )?;
    let a = app.clone();
    tw.set(
        "tftpStop",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>| throw(&ctx, tftp_server(&a).map(|s| s.stop())))?,
    )?;
    let a = app.clone();
    tw.set(
        "tftpState",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
            throw(
                &ctx,
                tftp_server(&a).map(|s| json!({ "status": s.status(), "transfers": s.transfers() }).to_string()),
            )
        })?,
    )?;
    tw.set("tftpAddresses", Function::new(ctx.clone(), || json!(crate::tftp::local_addresses()).to_string())?)?;
    tw.set("utf8", Function::new(ctx.clone(), |text: String| -> Vec<u8> { text.into_bytes() })?)?;

    ctx.globals().set("__tw", tw)?;
    Ok(())
}

fn write_file(path: &Path, data: &[u8], append: bool) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(path)?;
    f.write_all(data)
}

#[cfg(test)]
mod hw_tests;
#[cfg(test)]
mod tests;
