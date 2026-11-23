// One terminal tab: an xterm.js instance bound to a backend session.
//
// This is deliberately not a React component. Terminal output can arrive at
// megabytes per second, so the data path (backend -> xterm) never touches React.
// React only subscribes to the small amount of UI state (status, logging, prompts).

import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebglAddon } from "@xterm/addon-webgl";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";

import { api } from "../lib/api";
import { describeConnection } from "../lib/profiles";
import { formatBytes, formatDuration, PROTOCOL_LABEL } from "../lib/transfer";
import type {
  HdlcConfig,
  HdlcFrame,
  HdlcSendStatus,
  HostKeyDecision,
  HostKeyPrompt,
  LinkState,
  LogMode,
  Profile,
  SessionEvent,
  SignalName,
  TransferProtocol,
  TransferRequest,
  TransferStatus,
} from "../lib/types";
import { darkTheme, TERMINAL_FONT } from "./theme";

export interface SecretPrompt {
  kind: "password" | "passphrase";
  error: string | null;
}

const DIM = "\x1b[0;38;5;244m";
const YELLOW = "\x1b[0;33m";
const RED = "\x1b[0;31m";
const GREEN = "\x1b[0;32m";
const RESET = "\x1b[0m";
/** Frames kept for the HDLC panel per tab. */
const HDLC_MAX_FRAMES = 2000;

let tabCounter = 0;

/** What a script gets back from a finished transfer. */
export interface TransferResult {
  files: { name: string; path: string | null; bytes: number }[];
  bytes: number;
  elapsedMs: number;
}

export class TermSession {
  /** Stable for the life of the tab (backend sessions change on reconnect). Scripts address tabs by it. */
  readonly tabId: string;
  profile: Profile;
  title: string;

  state: LinkState = "connecting";
  message: string | null = null;
  logPath: string | null = null;
  logMode: LogMode | null = null;
  hostKey: HostKeyPrompt | null = null;
  secretPrompt: SecretPrompt | null = null;
  signals: Record<SignalName, boolean>;
  /** The running file transfer, if any. Keyboard input is held back meanwhile. */
  transfer: TransferStatus | null = null;
  /** The remote side started `sz` or `rz` and is waiting for an answer. */
  zmodemRequest: { remoteSends: boolean } | null = null;
  private transferDone: { name: string; path: string | null; bytes: number }[] = [];
  /** Local start time, so the panel clock runs while waiting for the remote side. */
  transferStartedAt = 0;
  /** HDLC mode: settings while on, `null` while off. */
  hdlc: HdlcConfig | null = null;
  /** The HDLC side panel is shown (it can stay open with HDLC mode off). */
  hdlcPanelOpen = false;
  /** Frames seen in this tab, newest last (bounded). */
  hdlcFrames: (HdlcFrame & { seq: number })[] = [];
  hdlcSending: HdlcSendStatus | null = null;
  hdlcRecording: { path: string; bytes: number; frames: number } | null = null;
  private hdlcSeq = 0;
  private hdlcRedraw = 0;
  cols = 80;
  rows = 24;

  readonly term: Terminal;
  readonly search = new SearchAddon();
  private readonly fit = new FitAddon();
  private readonly element: HTMLDivElement;
  private opened = false;
  private started = false;
  private disposed = false;
  private backendId: number | null = null;
  private generation = 0;
  private secret: string | null;
  private listeners = new Set<() => void>();
  private snapshot = 0;
  private resizeObserver: ResizeObserver | null = null;
  private fitFrame = 0;
  /** A script waiting for the running transfer to end. */
  private transferWaiter: { resolve: (r: TransferResult) => void; reject: (e: Error) => void } | null = null;

  constructor(profile: Profile, secret: string | null = null, tabId?: string) {
    this.tabId = tabId ?? `tab-${++tabCounter}`;
    this.profile = structuredClone(profile);
    this.title = profile.name;
    this.secret = secret;
    const c = this.profile.connection;
    this.signals = c.kind === "serial" ? { dtr: c.dtr, rts: c.rts } : { dtr: false, rts: false };

    this.term = new Terminal({
      allowProposedApi: true,
      fontFamily: TERMINAL_FONT,
      fontSize: 14,
      lineHeight: 1.12,
      cursorBlink: true,
      cursorStyle: "block",
      scrollback: 20000,
      convertEol: this.profile.options.implicitCr,
      theme: darkTheme,
      drawBoldTextInBrightColors: true,
      rescaleOverlappingGlyphs: true,
      customGlyphs: true,
    });
    this.term.loadAddon(this.fit);
    this.term.loadAddon(this.search);
    this.term.loadAddon(new Unicode11Addon());
    this.term.unicode.activeVersion = "11";

    this.term.onData((d) => this.handleInput(d, false));
    this.term.onBinary((d) => this.handleInput(d, true));
    this.term.onResize(({ cols, rows }) => {
      this.cols = cols;
      this.rows = rows;
      if (this.backendId !== null) api.resize(this.backendId, cols, rows).catch(() => {});
      this.emit();
    });
    this.term.onTitleChange((t) => {
      if (t.trim()) {
        this.title = t.trim();
        this.emit();
      }
    });
    // Copy on select, like PuTTY / Tera Term.
    this.term.onSelectionChange(() => {
      if (this.term.hasSelection()) writeText(this.term.getSelection()).catch(() => {});
    });

    this.element = document.createElement("div");
    this.element.className = "xterm-host";
    // Right click: copy the selection if there is one, otherwise paste.
    this.element.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      if (this.term.hasSelection()) {
        this.copySelection();
        this.term.clearSelection();
      } else {
        this.paste();
      }
    });
  }

  // ---- React subscription -------------------------------------------------

  subscribe = (fn: () => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };
  getSnapshot = () => this.snapshot;
  private emit() {
    this.snapshot++;
    this.listeners.forEach((fn) => fn());
  }

  // ---- DOM lifecycle --------------------------------------------------------

  /** Mount into a host element. The terminal is opened lazily on first mount. */
  attach(host: HTMLElement) {
    if (this.disposed) return;
    if (this.element.parentElement !== host) host.appendChild(this.element);
    if (!this.opened) {
      this.opened = true;
      this.term.open(this.element);
      try {
        const webgl = new WebglAddon();
        webgl.onContextLoss(() => webgl.dispose());
        this.term.loadAddon(webgl);
      } catch {
        // Falls back to the DOM renderer.
      }
      this.resizeObserver = new ResizeObserver(() => this.scheduleFit());
      this.resizeObserver.observe(this.element);
    }
    this.fitNow();
    if (!this.started) {
      this.started = true;
      void this.connect();
    }
  }

  focus() {
    this.fitNow();
    this.term.focus();
  }

  private scheduleFit() {
    cancelAnimationFrame(this.fitFrame);
    this.fitFrame = requestAnimationFrame(() => this.fitNow());
  }

  private fitNow() {
    if (!this.opened || this.element.offsetParent === null) return; // hidden tab
    try {
      this.fit.fit();
    } catch {
      /* not measurable yet */
    }
    this.cols = this.term.cols;
    this.rows = this.term.rows;
  }

  // ---- Connection ---------------------------------------------------------

  private needsSecret(): SecretPrompt["kind"] | null {
    const c = this.profile.connection;
    if (c.kind === "ssh" && c.auth === "password" && !this.secret) return "password";
    return null;
  }

  async connect() {
    if (this.disposed) return;
    const missing = this.needsSecret();
    if (missing) {
      this.secretPrompt = { kind: missing, error: null };
      this.setState("connecting", "Waiting for password…");
      return;
    }
    this.closeBackend();
    const gen = ++this.generation;
    this.hostKey = null;
    this.setState("connecting", null);

    try {
      const id = await api.openSession(
        this.tabId,
        this.profile.connection,
        this.profile.options,
        this.secret,
        this.term.cols,
        this.term.rows,
        (ev) => {
          if (gen === this.generation) this.onEvent(ev);
        },
      );
      if (gen !== this.generation || this.disposed) {
        api.close(id).catch(() => {});
        return;
      }
      this.backendId = id;
      this.emit(); // enables backend-dependent controls (logging)
      if (this.hdlc) await api.hdlcSet(id, this.hdlc).catch(() => {});
      if (this.logPath && this.logMode) {
        // Keep logging into the same file across manual reconnects.
        this.logPath = await api.logStart(id, this.logPath, this.logMode).catch(() => null);
      }
    } catch (e) {
      this.setState("disconnected", String(e));
      this.notice(`${RED}✖ ${String(e)}`);
    }
  }

  provideSecret(secret: string) {
    this.secret = secret;
    this.secretPrompt = null;
    this.emit();
    this.refocusAfterDialog();
    void this.connect();
  }

  cancelSecret() {
    this.secretPrompt = null;
    this.setState("disconnected", "Cancelled");
    this.notice(`${DIM}Cancelled. Press Enter to try again.`);
    this.refocusAfterDialog();
  }

  answerHostKey(decision: HostKeyDecision) {
    this.hostKey = null;
    this.emit();
    this.refocusAfterDialog();
    if (this.backendId !== null) api.answerHostKey(this.backendId, decision).catch(() => {});
  }

  /** Give keyboard focus back to the terminal once a modal has unmounted. */
  private refocusAfterDialog() {
    requestAnimationFrame(() => {
      if (!this.disposed) this.term.focus();
    });
  }

  private closeBackend() {
    if (this.backendId !== null) {
      api.close(this.backendId).catch(() => {});
      this.backendId = null;
    }
  }

  private onEvent(ev: SessionEvent) {
    switch (ev.type) {
      case "data":
        this.term.write(ev.text);
        break;
      case "status":
        this.onStatus(ev.state, ev.message);
        break;
      case "notice":
        this.notice(`${YELLOW}${ev.text}`);
        break;
      case "hostKey":
        this.hostKey = { host: ev.host, port: ev.port, algorithm: ev.algorithm, fingerprint: ev.fingerprint };
        this.emit();
        break;
      case "authFailed": {
        const c = this.profile.connection;
        this.secret = null;
        this.secretPrompt = {
          kind: c.kind === "ssh" && c.auth === "publicKey" ? "passphrase" : "password",
          error: "Authentication failed. Please try again.",
        };
        this.emit();
        break;
      }
      case "log":
        this.logPath = ev.path;
        if (!ev.path) this.logMode = null;
        this.emit();
        break;
      case "transfer":
        this.onTransfer(ev);
        break;
      case "zmodem":
        if (!this.transfer && !this.zmodemRequest) {
          this.zmodemRequest = { remoteSends: ev.remoteSends };
          this.emit();
        }
        break;
      case "hdlcFrame":
        this.pushHdlcFrame(ev);
        break;
      case "hdlcSend":
        this.onHdlcSend(ev);
        break;
      case "hdlcRecord":
        if (ev.path) {
          this.hdlcRecording = { path: ev.path, bytes: ev.bytes, frames: ev.frames };
        } else {
          if (this.hdlcRecording) {
            this.notice(`${DIM}Saved ${ev.frames} HDLC frame payloads (${formatBytes(ev.bytes)}) → ${this.hdlcRecording.path}`);
          }
          this.hdlcRecording = null;
        }
        this.emit();
        break;
    }
  }

  private onStatus(state: LinkState, message: string | null) {
    const prev = this.state;
    this.setState(state, message);
    switch (state) {
      case "connecting":
        if (message) this.notice(`${DIM}${message}`);
        break;
      case "connected":
        this.notice(`${GREEN}● ${DIM}Connected${message ? ` — ${message}` : ""}`);
        break;
      case "reconnecting":
        this.notice(`${YELLOW}◌ ${message ?? "Connection lost. Reconnecting…"}`);
        break;
      case "disconnected":
        if (prev !== "disconnected") {
          this.notice(`${RED}● ${RESET}${message ?? "Disconnected"}\r\n${DIM}Press Enter to reconnect.`);
        }
        break;
    }
  }

  private setState(state: LinkState, message: string | null) {
    this.state = state;
    this.message = message;
    this.emit();
  }

  /** Write an informational line that is clearly not remote output. */
  private notice(text: string) {
    const atLineStart = this.term.buffer.active.cursorX === 0;
    this.term.write(`${atLineStart ? "" : "\r\n"}${text}${RESET}\r\n`);
  }

  // ---- Input & actions ------------------------------------------------------

  private handleInput(data: string, binary: boolean) {
    if (this.state === "disconnected") {
      if (data.includes("\r") && !this.secretPrompt) void this.connect();
      return;
    }
    if (this.backendId === null || this.state !== "connected") return;
    // Keystrokes would corrupt a running transfer.
    if (this.transfer) return;
    api.write(this.backendId, data, binary).catch(() => {});
    if (this.profile.options.localEcho && !binary) {
      this.term.write(data.replace(/\r/g, "\r\n"));
    }
  }

  copySelection() {
    const text = this.term.getSelection();
    if (text) writeText(text).catch(() => {});
  }

  async paste() {
    const text = await readText().catch(() => "");
    if (text) this.term.paste(text);
  }

  /** Types `text` into the session like a paste (no Enter), and focuses the terminal. */
  typeText(text: string) {
    this.term.paste(text);
    this.focus();
  }

  setSignal(signal: SignalName, value: boolean) {
    this.signals = { ...this.signals, [signal]: value };
    this.emit();
    if (this.backendId !== null) api.setSignal(this.backendId, signal, value).catch(() => {});
  }

  sendBreak() {
    if (this.backendId !== null) api.sendBreak(this.backendId).catch(() => {});
  }

  async setEncoding(encoding: string) {
    this.profile.options.encoding = encoding;
    this.emit();
    if (this.backendId !== null) await api.setEncoding(this.backendId, encoding).catch(() => {});
  }

  setOption<K extends "localEcho" | "implicitCr">(key: K, value: boolean) {
    this.profile.options[key] = value;
    if (key === "implicitCr") this.term.options.convertEol = value;
    this.emit();
  }

  async startLog(path: string, mode: LogMode) {
    if (this.backendId === null) throw new Error("Not connected");
    this.logMode = mode;
    this.logPath = await api.logStart(this.backendId, path, mode);
    this.emit();
  }

  async stopLog() {
    this.logMode = null;
    this.logPath = null;
    this.emit();
    if (this.backendId !== null) await api.logStop(this.backendId).catch(() => {});
  }

  clear() {
    this.term.clear();
  }

  // ---- File transfer --------------------------------------------------------

  get canTransfer(): boolean {
    return this.backendId !== null && this.state === "connected" && !this.transfer;
  }

  async startTransfer(request: TransferRequest) {
    if (this.backendId === null) return;
    const verb = request.direction === "send" ? "Sending" : "Receiving";
    this.transferDone = [];
    this.transferStartedAt = Date.now();
    // Shown until the first progress event replaces it.
    this.transfer = {
      protocol: request.protocol,
      sending: request.direction === "send",
      file: null,
      path: null,
      size: null,
      bytes: 0,
      filesDone: 0,
      totalBytes: 0,
      elapsedMs: 0,
      outcome: null,
      message: null,
    };
    this.emit();
    this.notice(`${DIM}${verb} with ${PROTOCOL_LABEL[request.protocol]}…`);
    try {
      await api.transferStart(this.backendId, request);
    } catch (e) {
      this.transfer = null;
      this.emit();
      this.notice(`${RED}✖ ${String(e)}`);
      this.transferWaiter?.reject(new Error(String(e)));
      this.transferWaiter = null;
    }
  }

  /** For scripts: runs a transfer and settles when it ends (rejects if it fails or is cancelled). */
  runTransfer(request: TransferRequest): Promise<TransferResult> {
    if (!this.canTransfer) {
      return Promise.reject(new Error(this.transfer ? "A file transfer is already running" : "The session is not connected"));
    }
    return new Promise((resolve, reject) => {
      this.transferWaiter = { resolve, reject };
      void this.startTransfer(request);
    });
  }

  /** For scripts: settles once connected, or rejects when the attempt fails, the tab closes or time runs out. */
  waitConnected(timeoutMs: number): Promise<void> {
    return new Promise((resolve, reject) => {
      let timer = 0;
      let off = () => {};
      const finish = (err?: Error) => {
        off();
        clearTimeout(timer);
        if (err) reject(err);
        else resolve();
      };
      const check = () => {
        if (this.disposed) finish(new Error("The tab was closed"));
        else if (this.state === "connected") finish();
        else if (this.state === "disconnected") finish(new Error(`${this.title}: ${this.message ?? "could not connect"}`));
      };
      off = this.subscribe(check);
      if (timeoutMs > 0 && Number.isFinite(timeoutMs)) {
        timer = window.setTimeout(
          () => finish(new Error(`${this.title}: not connected within ${Math.round(timeoutMs / 100) / 10} s`)),
          timeoutMs,
        );
      }
      check();
    });
  }

  sendFiles(protocol: TransferProtocol, files: string[], remoteReady = false) {
    return this.startTransfer({ protocol, direction: "send", files, remoteReady });
  }

  receiveFiles(protocol: TransferProtocol, dest: string) {
    return this.startTransfer({ protocol, direction: "receive", dest });
  }

  cancelTransfer() {
    if (this.backendId !== null) api.transferCancel(this.backendId).catch(() => {});
  }

  /** The user turned down a ZMODEM request from the remote side. */
  declineZmodem() {
    this.zmodemRequest = null;
    this.emit();
    this.refocusAfterDialog();
    if (this.backendId !== null) api.zmodemDecline(this.backendId).catch(() => {});
  }

  /** Answer a ZMODEM request: files to send, or a folder to receive into. */
  acceptZmodem(target: string[] | string) {
    const req = this.zmodemRequest;
    this.zmodemRequest = null;
    this.refocusAfterDialog();
    if (!req) return;
    if (req.remoteSends) void this.receiveFiles("zmodem", target as string);
    else void this.sendFiles("zmodem", target as string[], true);
  }

  private onTransfer(st: TransferStatus) {
    // Keep a list of finished files for the summary.
    if (st.filesDone > this.transferDone.length && st.file) {
      this.transferDone.push({ name: st.file, path: st.path, bytes: st.bytes });
    }
    if (!st.outcome) {
      this.transfer = st;
      this.emit();
      return;
    }
    this.transfer = null;
    this.emit();
    const label = PROTOCOL_LABEL[st.protocol];
    const files = this.transferDone;
    const total = files.reduce((n, f) => n + f.bytes, 0);
    const took = formatDuration(st.elapsedMs);
    const waiter = this.transferWaiter;
    this.transferWaiter = null;
    if (st.outcome === "done") waiter?.resolve({ files, bytes: total, elapsedMs: st.elapsedMs });
    else if (st.outcome === "cancelled") waiter?.reject(new Error(`${label} transfer cancelled`));
    else waiter?.reject(new Error(`${label} transfer failed: ${st.message ?? "unknown error"}`));
    if (st.outcome === "done") {
      const what = files.length === 1 ? files[0].name : `${files.length} files`;
      this.notice(`${GREEN}✔ ${RESET}${st.sending ? "Sent" : "Received"} ${what} (${formatBytes(total)}) in ${took} — ${label}`);
      if (!st.sending) {
        for (const f of files) if (f.path) this.notice(`${DIM}  → ${f.path}`);
      }
    } else if (st.outcome === "cancelled") {
      this.notice(`${YELLOW}Transfer cancelled after ${formatBytes(st.totalBytes)}.`);
    } else {
      this.notice(`${RED}✖ ${RESET}${label} transfer failed: ${st.message ?? "unknown error"}`);
    }
    this.transferDone = [];
  }

  // ---- HDLC ------------------------------------------------------------------

  setHdlcPanel(open: boolean) {
    this.hdlcPanelOpen = open;
    this.emit();
  }

  /** Turns HDLC mode on with `config`, or off with `null`. */
  async setHdlc(config: HdlcConfig | null) {
    this.hdlc = config;
    if (!config) this.hdlcRecording = null;
    this.emit();
    if (this.backendId !== null) await api.hdlcSet(this.backendId, config);
  }

  get canHdlcSend(): boolean {
    return this.hdlc !== null && this.backendId !== null && this.state === "connected" && !this.transfer;
  }

  async sendHdlc(payload: number[]) {
    if (this.backendId === null) return;
    await api.hdlcSend(this.backendId, payload);
  }

  async sendHdlcFile(path: string, chunk: number, gapMs: number) {
    if (this.backendId === null) return;
    const file = path.split(/[\\/]/).pop() ?? path;
    this.hdlcSending = { file, size: 0, bytes: 0, frames: 0, elapsedMs: 0, outcome: null, message: null };
    this.emit();
    try {
      await api.hdlcSendFile(this.backendId, path, chunk, gapMs);
    } catch (e) {
      this.hdlcSending = null;
      this.emit();
      throw e;
    }
  }

  cancelHdlcSend() {
    if (this.backendId !== null) api.hdlcCancelSend(this.backendId).catch(() => {});
  }

  async recordHdlc(path: string | null) {
    if (this.backendId === null) return;
    await api.hdlcRecord(this.backendId, path);
  }

  clearHdlcFrames() {
    this.hdlcFrames = [];
    this.emit();
  }

  private pushHdlcFrame(f: HdlcFrame) {
    this.hdlcFrames.push({ ...f, seq: ++this.hdlcSeq });
    if (this.hdlcFrames.length > HDLC_MAX_FRAMES) this.hdlcFrames.splice(0, this.hdlcFrames.length - HDLC_MAX_FRAMES);
    // Frames can arrive by the thousand: redraw the panel at most every 100 ms.
    if (!this.hdlcRedraw) {
      this.hdlcRedraw = window.setTimeout(() => {
        this.hdlcRedraw = 0;
        this.hdlcFrames = this.hdlcFrames.slice(); // new identity for React
        this.emit();
      }, 100);
    }
  }

  private onHdlcSend(st: HdlcSendStatus) {
    if (!st.outcome) {
      this.hdlcSending = st;
      this.emit();
      return;
    }
    this.hdlcSending = null;
    this.emit();
    const took = formatDuration(st.elapsedMs);
    if (st.outcome === "done") {
      this.notice(`${GREEN}✔ ${RESET}Sent ${st.file} as ${st.frames} HDLC frames (${formatBytes(st.bytes)}) in ${took}`);
    } else if (st.outcome === "cancelled") {
      this.notice(`${YELLOW}HDLC send cancelled after ${st.frames} frames (${formatBytes(st.bytes)}).`);
    } else {
      this.notice(`${RED}✖ ${RESET}HDLC send failed: ${st.message ?? "unknown error"}`);
    }
  }

  get summary(): string {
    return describeConnection(this.profile.connection);
  }

  get canLog(): boolean {
    return this.backendId !== null;
  }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    this.generation++;
    this.emit(); // scripts waiting for this tab
    this.closeBackend();
    api.tabClosed(this.tabId).catch(() => {});
    this.transferWaiter?.reject(new Error("The tab was closed"));
    this.transferWaiter = null;
    this.resizeObserver?.disconnect();
    cancelAnimationFrame(this.fitFrame);
    clearTimeout(this.hdlcRedraw);
    this.term.dispose();
    this.element.remove();
    this.listeners.clear();
  }
}
