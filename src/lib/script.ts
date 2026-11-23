// The UI side of scripts (src-tauri/src/script/): runs and their output, and the
// requests a script makes of the UI — opening tabs, dialogs, logging, transfers,
// DTR/RTS — so that whatever a script changes shows in the tab as if done by hand.

import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";

import type { TermSession } from "../terminal/TermSession";
import { api } from "./api";
import { defaultConnection, ENCODINGS, KIND_LABEL, parseQuickConnect, profileFromConnection, validateConnection } from "./profiles";
import type { Connection, ConnectionKind, HdlcConfig, LogMode, Profile, TransferProtocol } from "./types";

export interface ScriptRun {
  run: number;
  name: string;
  path: string;
  running: boolean;
  outcome: "done" | "failed" | "stopped" | null;
  message: string | null;
  /** Set by ui.status(). */
  status: string | null;
  /** Set by ui.result(). */
  result: { status: "PASS" | "FAIL"; detail: string } | null;
  startedAt: number;
  endedAt: number | null;
}

export interface OutputLine {
  seq: number;
  run: number;
  stream: "out" | "err" | "info" | "pass" | "fail";
  text: string;
}

export interface ScriptDialog {
  run: number;
  id: number;
  script: string;
  kind: "prompt" | "password" | "confirm" | "choose";
  message: string;
  value?: string;
  options?: string[];
}

/** What the script host needs from the app. */
export interface ScriptApp {
  tabs(): TermSession[];
  profiles(): Profile[];
  openTab(profile: Profile, secret: string | null, tabId: string): TermSession;
  closeTab(tabId: string): void;
  activate(tabId: string): void;
}

interface Request {
  run: number;
  id: number;
  kind: string;
  args: Record<string, unknown>;
}

const MAX_LINES = 5000;
const MAX_RECENT = 8;
const RECENT_KEY = "termworker.recentScripts";

class ScriptHost {
  runs: ScriptRun[] = [];
  output: OutputLine[] = [];
  dialogs: ScriptDialog[] = [];
  panelOpen = false;
  recent: string[] = loadRecent();

  private app: ScriptApp | null = null;
  private listening = false;
  private seq = 0;
  private listeners = new Set<() => void>();
  private snapshot = 0;

  subscribe = (fn: () => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };
  getSnapshot = () => this.snapshot;
  private emit() {
    this.snapshot++;
    this.listeners.forEach((fn) => fn());
  }

  init(app: ScriptApp) {
    this.app = app;
    if (this.listening) return;
    this.listening = true;
    void listen<Omit<ScriptRun, "status" | "result" | "startedAt" | "endedAt">>("script:state", (e) => this.onState(e.payload));
    void listen<{ run: number; stream: "out" | "err"; text: string }>("script:output", (e) =>
      this.print(e.payload.run, e.payload.stream, e.payload.text),
    );
    void listen<Request>("script:request", (e) => void this.handle(e.payload));
  }

  get running(): ScriptRun[] {
    return this.runs.filter((r) => r.running);
  }

  /** Starts the script at `path`; `tab` becomes the script's `tab`. */
  async run(path: string, tab: TermSession | null) {
    this.recent = [path, ...this.recent.filter((p) => p !== path)].slice(0, MAX_RECENT);
    localStorage.setItem(RECENT_KEY, JSON.stringify(this.recent));
    this.panelOpen = true;
    this.emit();
    try {
      await api.scriptRun(path, tab?.tabId ?? null, tab?.title ?? null);
    } catch (e) {
      this.print(0, "err", String(e));
    }
  }

  stop(run: number) {
    api.scriptStop(run).catch(() => {});
  }

  setPanel(open: boolean) {
    this.panelOpen = open;
    this.emit();
  }

  clearOutput() {
    this.output = [];
    this.runs = this.runs.filter((r) => r.running);
    this.emit();
  }

  forgetRecent(path: string) {
    this.recent = this.recent.filter((p) => p !== path);
    localStorage.setItem(RECENT_KEY, JSON.stringify(this.recent));
    this.emit();
  }

  /** The user answered (or dismissed: `null`) the dialog a script asked for. */
  answer(d: ScriptDialog, value: unknown) {
    this.dialogs = this.dialogs.filter((x) => x !== d);
    this.emit();
    api.scriptReply(d.run, d.id, true, value).catch(() => {});
  }

  // ---- events

  private print(run: number, stream: OutputLine["stream"], text: string) {
    for (const line of text.split(/\r?\n/)) this.output.push({ seq: ++this.seq, run, stream, text: line });
    if (this.output.length > MAX_LINES) this.output.splice(0, this.output.length - MAX_LINES);
    this.emit();
  }

  private onState(p: Omit<ScriptRun, "status" | "result" | "startedAt" | "endedAt">) {
    let r = this.runs.find((x) => x.run === p.run);
    if (!r) {
      r = { ...p, status: null, result: null, startedAt: Date.now(), endedAt: null };
      this.runs.push(r);
      this.print(p.run, "info", `▶ ${p.name}  ${p.path}`);
      return;
    }
    Object.assign(r, p);
    if (p.running) return;
    r.endedAt = Date.now();
    r.status = null;
    this.dialogs = this.dialogs.filter((d) => d.run !== p.run);
    const took = `${((r.endedAt - r.startedAt) / 1000).toFixed(1)} s`;
    if (p.outcome === "done") this.print(p.run, "info", `✔ ${p.name} finished in ${took}`);
    else if (p.outcome === "stopped") this.print(p.run, "info", `■ ${p.name} stopped after ${took}`);
    else this.print(p.run, "err", `✖ ${p.name} failed after ${took}\n${p.message ?? ""}`.trimEnd());
  }

  private async handle(req: Request) {
    try {
      const value = await this.dispatch(req);
      if (value !== undefined) await api.scriptReply(req.run, req.id, true, value);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      await api.scriptReply(req.run, req.id, false, msg).catch(() => {});
    }
  }

  private tab(args: Record<string, unknown>): TermSession {
    const t = this.app?.tabs().find((t) => t.tabId === args.tab);
    if (!t) throw new Error("The tab was closed");
    return t;
  }

  /** The answer to a request; `undefined` when it is answered later (dialogs). */
  private async dispatch(req: Request): Promise<unknown> {
    const app = this.app;
    if (!app) throw new Error("The window is not ready");
    const a = req.args;
    const run = this.runs.find((r) => r.run === req.run);
    switch (req.kind) {
      case "open": {
        const { profile, secret } = resolveTarget(a.target, app.profiles());
        const t = app.openTab(profile, secret, String(a.tab));
        if (a.wait !== false) await t.waitConnected(Number(a.timeout ?? 60000));
        return { tab: t.tabId, name: t.title };
      }
      case "list":
        return app.tabs().map((t) => ({ tab: t.tabId, name: t.title, state: t.state }));
      case "activate":
        app.activate(this.tab(a).tabId);
        return null;
      case "close":
        app.closeTab(this.tab(a).tabId);
        return null;
      case "signal": {
        const t = this.tab(a);
        if (t.profile.connection.kind !== "serial") throw new Error("DTR and RTS exist only on serial sessions");
        t.setSignal(a.signal === "rts" ? "rts" : "dtr", !!a.value);
        return null;
      }
      case "encoding": {
        const t = this.tab(a);
        const enc = String(a.encoding).toLowerCase();
        const known = ENCODINGS.find((e) => e.value === enc || e.label.toLowerCase().startsWith(enc));
        if (!known) throw new Error(`Unknown encoding "${a.encoding}". Use one of: ${ENCODINGS.map((e) => e.value).join(", ")}`);
        await t.setEncoding(known.value);
        return known.value;
      }
      case "log": {
        const t = this.tab(a);
        if (a.path == null) await t.stopLog();
        else await t.startLog(String(a.path), a.mode as LogMode);
        return t.logPath;
      }
      case "hdlc": {
        const t = this.tab(a);
        const config = (a.config as HdlcConfig | null) ?? null;
        await t.setHdlc(config);
        if (config) t.setHdlcPanel(true);
        return null;
      }
      case "transfer": {
        const t = this.tab(a);
        const protocol = a.protocol as TransferProtocol;
        if (a.direction === "send") {
          return await t.runTransfer({ protocol, direction: "send", files: a.files as string[] });
        }
        let dest = a.dest as string | null;
        if (!dest) {
          if (protocol === "xmodem" || protocol === "xmodem1k") throw new Error("xmodem.receive needs { file: \"name\" }");
          dest = await api.defaultDownloadDir();
        }
        return await t.runTransfer({ protocol, direction: "receive", dest });
      }
      case "status":
        if (run) run.status = (a.text as string | null) || null;
        this.emit();
        return null;
      case "result": {
        const status = a.status === "PASS" ? "PASS" : "FAIL";
        const detail = String(a.detail ?? "");
        if (run) run.result = { status, detail };
        this.print(req.run, status === "PASS" ? "pass" : "fail", `${status}${detail ? `  ${detail}` : ""}`);
        return null;
      }
      case "openFile": {
        const p = await open({ title: (a.title as string) ?? "Open file", multiple: false, directory: !!a.folder });
        return typeof p === "string" ? p : null;
      }
      case "saveFile":
        return (await save({ title: (a.title as string) ?? "Save file", defaultPath: a.name as string | undefined })) ?? null;
      case "prompt":
      case "password":
      case "confirm":
      case "choose":
        this.dialogs.push({
          run: req.run,
          id: req.id,
          script: run?.name ?? "Script",
          kind: req.kind,
          message: String(a.message ?? ""),
          value: a.value as string | undefined,
          options: a.options as string[] | undefined,
        });
        this.emit();
        return undefined;
      default:
        throw new Error(`Unknown request ${req.kind}`);
    }
  }
}

function loadRecent(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((p) => typeof p === "string").slice(0, MAX_RECENT) : [];
  } catch {
    return [];
  }
}

/** A saved session name, quick-connect text, or an object like `{ type: "ssh", host, user, password }`. */
function resolveTarget(target: unknown, profiles: Profile[]): { profile: Profile; secret: string | null } {
  if (typeof target === "string") {
    const name = target.trim();
    const saved = profiles.find((p) => p.name === name) ?? profiles.find((p) => p.name.toLowerCase() === name.toLowerCase());
    if (saved) return { profile: structuredClone(saved), secret: null };
    const c = parseQuickConnect(name);
    if (!c) throw new Error(`No saved session is named "${name}", and it is not a quick-connect address`);
    return checked(profileFromConnection(c), null);
  }
  if (!target || typeof target !== "object") {
    throw new Error("sessions.open: give a saved session name, quick-connect text or an object");
  }
  const o = target as Record<string, unknown>;
  const kind = String(o.type ?? o.kind ?? "") as ConnectionKind;
  if (!(kind in KIND_LABEL)) throw new Error(`sessions.open: type is "serial", "ssh", "telnet" or "raw"`);
  const c: Connection = defaultConnection(kind);
  const num = (v: unknown) => (v === undefined ? undefined : Number(v));
  if (c.kind === "serial") {
    c.port = String(o.port ?? "");
    c.baudRate = num(o.baud ?? o.baudRate) ?? c.baudRate;
    if (o.dataBits !== undefined) c.dataBits = Number(o.dataBits) as typeof c.dataBits;
    if (o.parity !== undefined) c.parity = String(o.parity) as typeof c.parity;
    if (o.stopBits !== undefined) c.stopBits = Number(o.stopBits) as typeof c.stopBits;
    if (o.flowControl !== undefined) c.flowControl = String(o.flowControl) as typeof c.flowControl;
    if (o.dtr !== undefined) c.dtr = !!o.dtr;
    if (o.rts !== undefined) c.rts = !!o.rts;
    if (o.autoReconnect !== undefined) c.autoReconnect = !!o.autoReconnect;
  } else {
    c.host = String(o.host ?? "");
    c.port = num(o.port) ?? c.port;
    if (c.kind === "ssh") {
      c.username = String(o.user ?? o.username ?? "");
      if (o.keyPath) {
        c.auth = "publicKey";
        c.keyPath = String(o.keyPath);
      }
    }
  }
  const profile = profileFromConnection(c, o.name as string | undefined);
  if (o.encoding !== undefined) profile.options.encoding = String(o.encoding);
  if (o.enterSends !== undefined) profile.options.enterSends = String(o.enterSends) as Profile["options"]["enterSends"];
  // Kept in memory only, like a password typed into the dialog.
  const secret = (o.password ?? o.passphrase ?? null) as string | null;
  return checked(profile, secret);
}

function checked(profile: Profile, secret: string | null) {
  const err = validateConnection(profile.connection);
  if (err) throw new Error(`sessions.open: ${err}`);
  return { profile, secret };
}

export const scripts = new ScriptHost();
