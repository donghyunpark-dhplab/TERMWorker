// TermWorker script API (docs/SCRIPTING.md), built on the native functions in
// `__tw` (src-tauri/src/script/mod.rs). Evaluated as the module "prelude.js"
// before the user's script; frames from this file are left out of error stacks.

const tw = globalThis.__tw;
delete globalThis.__tw;

const STOP = "\u0000stop";
const STOPPED = "\u0000stopped";
/** Unread text kept per session; past this only the newest part is kept. */
const MAX_BUFFER = 1 << 20;
const KEEP_BUFFER = 256 << 10;
/** Received HDLC frames kept for `waitFrame`. */
const MAX_FRAMES = 1000;
const DEFAULT_TIMEOUT = 30000;
const DEFAULT_PROMPT = /[#$>] ?$/;

class TimeoutError extends Error {
  constructor(message) {
    super(message);
    this.name = "TimeoutError";
  }
}

class ScriptStopped extends Error {
  constructor() {
    super("Script stopped");
    this.name = "ScriptStopped";
  }
}

// ---------------------------------------------------------------- output

function show(v) {
  if (typeof v === "string") return v;
  if (v instanceof Error) return `${v.name}: ${v.message}`;
  if (v instanceof Uint8Array) return hex(v);
  if (v === undefined || typeof v === "function" || typeof v === "symbol") return String(v);
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}

const print = (...args) => tw.print("out", args.map(show).join(" "));
const printError = (...args) => tw.print("err", args.map(show).join(" "));

function hex(bytes) {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0").toUpperCase()).join(" ");
}

/** Calls a user callback; an error is reported instead of ending the event pump. */
function safe(fn, ...args) {
  try {
    const r = fn(...args);
    if (r && typeof r.then === "function") r.then(undefined, (e) => e instanceof ScriptStopped || printError(e));
  } catch (e) {
    if (!(e instanceof ScriptStopped)) printError(e);
  }
}

// ---------------------------------------------------------------- requests to the UI

async function request(kind, args = {}) {
  // Made before the first await, so its stack points at the script line that asked.
  const error = new Error();
  const r = JSON.parse(await tw.request(kind, JSON.stringify(args)));
  if ("err" in r) {
    if (r.err === STOPPED) throw new ScriptStopped();
    error.message = r.err;
    throw error;
  }
  return r.ok;
}

// ---------------------------------------------------------------- timers and the event pump

const timers = new Set();

function timer(ms, fire) {
  const t = { at: Date.now() + Math.max(0, ms), fire };
  timers.add(t);
  tw.poke(); // the pump may be waiting with a longer timeout
  return t;
}

const cancelTimer = (t) => t && timers.delete(t);

const sessionsById = new Map();

async function pump() {
  for (;;) {
    const now = Date.now();
    let wait = Infinity;
    for (const t of [...timers]) {
      if (t.at <= now) {
        timers.delete(t);
        safe(t.fire);
      } else {
        wait = Math.min(wait, t.at - now);
      }
    }
    const raw = await tw.next(wait === Infinity ? -1 : wait);
    if (raw === STOP) break;
    if (!raw) continue; // timeout or a new timer
    const ev = JSON.parse(raw);
    sessionsById.get(ev.tab)?._event(ev);
  }
}
pump();

function sleep(ms) {
  if (!(ms > 0)) return Promise.resolve();
  if (!Number.isFinite(ms)) return new Promise(() => {}); // until the script is stopped
  return new Promise((resolve) => timer(ms, resolve));
}

// ---------------------------------------------------------------- helpers

function toBytes(data) {
  if (typeof data === "string") return tw.utf8(data);
  if (data instanceof Uint8Array || Array.isArray(data)) return Array.from(data, (b) => b & 0xff);
  if (data instanceof ArrayBuffer) return Array.from(new Uint8Array(data));
  throw new TypeError("Expected bytes: a Uint8Array, an array of numbers or a string");
}

/** Received bytes not yet read by the script. */
class ByteBuffer {
  constructor() {
    this.a = new Uint8Array(4096);
    this.n = 0;
  }

  /** `data`: one char U+0000–U+00FF per byte (see Tap::bytes). */
  push(data) {
    if (this.n + data.length > this.a.length) {
      let size = this.a.length;
      while (size < this.n + data.length) size *= 2;
      const a = new Uint8Array(size);
      a.set(this.a.subarray(0, this.n));
      this.a = a;
    }
    for (let i = 0; i < data.length; i++) this.a[this.n++] = data.charCodeAt(i);
    if (this.n > MAX_BUFFER) this.take(this.n - KEEP_BUFFER);
  }

  view() {
    return this.a.subarray(0, this.n);
  }

  /** Removes and returns the first `count` bytes. */
  take(count) {
    const out = this.a.slice(0, count);
    this.a.copyWithin(0, count, this.n);
    this.n -= count;
    return out;
  }

  clear() {
    this.n = 0;
  }
}

/** Index of `needle` in `hay`, or -1. */
function indexOfBytes(hay, needle) {
  const last = hay.length - needle.length;
  outer: for (let i = 0; i <= last; i++) {
    for (let j = 0; j < needle.length; j++) if (hay[i + j] !== needle[j]) continue outer;
    return i;
  }
  return -1;
}

function tailBytes(bytes, n = 32) {
  if (!bytes.length) return "(nothing)";
  return (bytes.length > n ? "… " : "") + hex(bytes.subarray(Math.max(0, bytes.length - n)));
}

function compile(p) {
  if (p instanceof RegExp) return new RegExp(p.source, p.flags.replace(/[gy]/g, ""));
  if (typeof p === "string") {
    if (!p) throw new TypeError("Empty pattern");
    return p;
  }
  throw new TypeError("A pattern is a string or a regular expression");
}

/** Earliest match of any pattern in `text` (ties: the lower index). */
function findFirst(text, patterns) {
  let best = null;
  patterns.forEach((p, index) => {
    let m;
    if (typeof p === "string") {
      const at = text.indexOf(p);
      if (at >= 0) m = { start: at, end: at + p.length, match: p, groups: [], named: undefined };
    } else {
      const r = p.exec(text);
      if (r) m = { start: r.index, end: r.index + r[0].length, match: r[0], groups: r.slice(1), named: r.groups };
    }
    if (m && (!best || m.start < best.start)) best = { ...m, index };
  });
  return best;
}

function describePatterns(patterns) {
  return patterns.map((p) => (typeof p === "string" ? JSON.stringify(p) : String(p))).join(" or ");
}

function tail(text, n = 80) {
  const t = text.length > n ? `…${text.slice(-n)}` : text;
  return JSON.stringify(t);
}

const seconds = (ms) => `${+(ms / 1000).toFixed(1)} s`;

function stripAnsi(text) {
  return String(text).replace(/\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]/g, "");
}

const KEYS = {
  ESC: "\x1b", CR: "\r", LF: "\n", TAB: "\t", BS: "\b", DEL: "\x7f", SPACE: " ", NUL: "\0",
  UP: "\x1b[A", DOWN: "\x1b[B", RIGHT: "\x1b[C", LEFT: "\x1b[D", HOME: "\x1b[H", END: "\x1b[F",
  INS: "\x1b[2~", PGUP: "\x1b[5~", PGDN: "\x1b[6~",
  F1: "\x1bOP", F2: "\x1bOQ", F3: "\x1bOR", F4: "\x1bOS", F5: "\x1b[15~", F6: "\x1b[17~",
  F7: "\x1b[18~", F8: "\x1b[19~", F9: "\x1b[20~", F10: "\x1b[21~", F11: "\x1b[23~", F12: "\x1b[24~",
};

/** "^C", "<ESC>", "<F1>" and plain text → the characters to send. "^^" is a literal "^". */
function parseKeys(spec) {
  let out = "";
  for (let i = 0; i < spec.length; i++) {
    const c = spec[i];
    if (c === "^" && i + 1 < spec.length) {
      const n = spec[i + 1];
      if (n === "^") out += "^";
      else {
        const code = n.toUpperCase().charCodeAt(0);
        if (code < 0x40 || code > 0x5f) throw new SyntaxError(`sendKeys: no control key ^${n}`);
        out += String.fromCharCode(code - 0x40);
      }
      i++;
    } else if (c === "<") {
      const end = spec.indexOf(">", i);
      const name = end > i ? spec.slice(i + 1, end).toUpperCase() : "";
      if (name in KEYS) {
        out += KEYS[name];
        i = end;
      } else out += c;
    } else out += c;
  }
  return out;
}

const LOG_MODES = { timestamp: "timestamped", timestamped: "timestamped", text: "text", raw: "raw" };
const FCS = { 0: "none", none: "none", 16: "fcs16", fcs16: "fcs16", 32: "fcs32", fcs32: "fcs32" };

// ---------------------------------------------------------------- sessions

class Session {
  constructor(id, name) {
    this.id = id;
    this.name = name || id;
    /** "connecting", "connected", "reconnecting" or "disconnected". */
    this.state = "connecting";
    this.message = null;
    /** Default prompt for command(). */
    this.prompt = DEFAULT_PROMPT;
    this._buf = "";
    this._waiters = [];
    this._stateWaiters = new Set();
    this._line = "";
    this._onData = new Set();
    this._onLine = new Set();
    this._onFrame = new Set();
    this._frames = [];
    this._frameWaiters = [];
    this._lastData = Date.now();
    this._bin = new ByteBuffer();
    this._binWaiters = [];
    this._onBytes = new Set();

    this.bytes = {
      /** Exactly `count` bytes. */
      read: (count, opts) => {
        const n = Math.floor(Number(count));
        if (!(n > 0)) throw new TypeError("bytes.read: count must be a positive number");
        return this._waitBytes((b) => (b.length >= n ? n : -1), `${n} bytes`, opts, "bytes.read");
      },
      /** Up to and including `pattern` (bytes, or a string as UTF-8). */
      readUntil: (pattern, opts) => {
        const p = toBytes(pattern);
        if (!p.length) throw new TypeError("bytes.readUntil: empty pattern");
        return this._waitBytes(
          (b) => {
            const i = indexOfBytes(b, p);
            return i < 0 ? -1 : i + p.length;
          },
          hex(p),
          opts,
          "bytes.readUntil",
        );
      },
      /** Everything received and not read yet (may be empty). */
      take: () => this._bin.take(this._bin.n),
      discard: () => this._bin.clear(),
      onData: (fn) => this._subscribe(this._onBytes, fn),
    };
    Object.defineProperty(this.bytes, "available", { get: () => this._bin.n, enumerable: true });

    const transfer = (protocol) => ({
      send: (files) =>
        request("transfer", {
          tab: this.id,
          protocol,
          direction: "send",
          files: [].concat(files).map((f) => tw.resolve(String(f))),
        }),
      receive: (opts = {}) => {
        const dest = opts.dir ?? opts.file;
        return request("transfer", {
          tab: this.id,
          protocol,
          direction: "receive",
          dest: dest ? tw.resolve(String(dest)) : null,
        });
      },
    });
    this.zmodem = transfer("zmodem");
    this.ymodem = transfer("ymodem");
    this.xmodem = transfer("xmodem");
    this.xmodem1k = transfer("xmodem1k");

    this.log = {
      start: (path, opts = {}) => {
        const mode = LOG_MODES[opts.mode ?? "timestamp"];
        if (!mode) throw new TypeError(`log.start: mode is "timestamp", "text" or "raw"`);
        return request("log", { tab: this.id, path: tw.resolve(String(path)), mode });
      },
      stop: () => request("log", { tab: this.id, path: null }),
    };

    this.hdlc = {
      enable: (cfg = {}) => {
        const fcs = FCS[cfg.fcs ?? 16];
        if (!fcs) throw new TypeError("hdlc.enable: fcs is 16, 32 or 0");
        return request("hdlc", {
          tab: this.id,
          config: {
            fcs,
            addressControl: !!cfg.addressControl,
            escapeControlChars: !!cfg.escapeControlChars,
            maxFrame: cfg.maxFrame ?? 4096,
          },
        });
      },
      disable: () => request("hdlc", { tab: this.id, config: null }),
      send: (payload) => tw.hdlcSend(this.id, toBytes(payload)),
      waitFrame: (opts = {}) => this._waitFrame(opts),
      onFrame: (fn) => this._subscribe(this._onFrame, fn),
    };
  }

  get connected() {
    return this.state === "connected";
  }

  // --- events from the backend

  _event(ev) {
    switch (ev.t) {
      case "data":
        this._data(ev.text);
        break;
      case "bytes":
        this._bytes(ev.data);
        break;
      case "status":
        this.state = ev.state;
        this.message = ev.message;
        for (const fn of [...this._stateWaiters]) fn();
        break;
      case "frame": {
        const frame = { payload: new Uint8Array(ev.payload), fcsOk: ev.fcsOk, time: new Date() };
        for (const fn of this._onFrame) safe(fn, frame);
        const w = this._frameWaiters.findIndex((w) => w.accepts(frame));
        if (w >= 0) this._frameWaiters.splice(w, 1)[0].resolve(frame);
        else {
          this._frames.push(frame);
          if (this._frames.length > MAX_FRAMES) this._frames.shift();
        }
        break;
      }
    }
  }

  _data(text) {
    this._lastData = Date.now();
    this._buf += text;
    if (this._buf.length > MAX_BUFFER) this._buf = this._buf.slice(-KEEP_BUFFER);
    for (const fn of this._onData) safe(fn, text);
    if (this._onLine.size) {
      const parts = (this._line + text).split("\n");
      this._line = parts.pop();
      if (this._line.length > MAX_BUFFER) this._line = "";
      for (const p of parts) {
        const line = p.endsWith("\r") ? p.slice(0, -1) : p;
        for (const fn of this._onLine) safe(fn, line);
      }
    }
    this._match();
  }

  _bytes(data) {
    this._lastData = Date.now();
    this._bin.push(data);
    if (this._onBytes.size) {
      const chunk = Uint8Array.from(data, (c) => c.charCodeAt(0));
      for (const fn of this._onBytes) safe(fn, chunk);
    }
    this._matchBytes();
  }

  _matchBytes() {
    for (let again = true; again && this._binWaiters.length; ) {
      again = false;
      for (const w of this._binWaiters) {
        const count = w.test(this._bin.view());
        if (count < 0) continue;
        w.done(this._bin.take(count));
        again = true;
        break;
      }
    }
  }

  /** `test(buffer)` returns how many bytes to take, or -1 to keep waiting. */
  _waitBytes(test, what, opts = {}, fn) {
    const timeout = opts.timeout ?? DEFAULT_TIMEOUT;
    const timeoutError = new TimeoutError(`${fn} timed out`);
    return new Promise((resolve, reject) => {
      const w = { test, timer: null };
      const remove = () => this._binWaiters.splice(this._binWaiters.indexOf(w), 1);
      w.done = (bytes) => {
        remove();
        cancelTimer(w.timer);
        resolve(bytes);
      };
      if (timeout > 0 && Number.isFinite(timeout)) {
        w.timer = timer(timeout, () => {
          remove();
          timeoutError.message =
            `${this.name}: ${what} did not arrive within ${seconds(timeout)}. ` +
            `Unread bytes (${this._bin.n}): ${tailBytes(this._bin.view())}`;
          reject(timeoutError);
        });
      }
      this._binWaiters.push(w);
      this._matchBytes();
    });
  }

  _match() {
    for (let again = true; again; ) {
      again = false;
      for (const w of this._waiters) {
        const m = findFirst(this._buf, w.patterns);
        if (!m) continue;
        const before = this._buf.slice(0, m.start);
        this._buf = this._buf.slice(m.end);
        w.done({ index: m.index, match: m.match, groups: m.groups, named: m.named, before });
        again = true;
        break;
      }
    }
  }

  _subscribe(set, fn) {
    if (typeof fn !== "function") throw new TypeError("Expected a function");
    set.add(fn);
    return () => set.delete(fn);
  }

  // --- sending

  send(text) {
    tw.write(this.id, String(text), false);
  }

  sendLine(text = "") {
    tw.write(this.id, `${text}\r`, false);
  }

  sendBytes(bytes) {
    tw.writeBytes(this.id, toBytes(bytes));
  }

  sendKeys(spec) {
    tw.write(this.id, parseKeys(String(spec)), true);
  }

  async sendBreak() {
    tw.sendBreak(this.id);
    await sleep(300); // the break itself lasts 250 ms
  }

  // --- waiting

  /** Patterns are searched in text received since the last match (or the start of the script). */
  _wait(patterns, opts = {}, what = "waitFor") {
    const list = patterns.map(compile);
    if (!list.length) throw new TypeError(`${what}: no patterns`);
    const timeout = opts.timeout ?? DEFAULT_TIMEOUT;
    // Made here so its stack points at the script line that waited.
    const timeoutError = new TimeoutError(`${what} timed out`);
    return new Promise((resolve, reject) => {
      const w = { patterns: list, timer: null };
      w.done = (r) => {
        this._waiters.splice(this._waiters.indexOf(w), 1);
        cancelTimer(w.timer);
        resolve(r);
      };
      if (timeout > 0 && Number.isFinite(timeout)) {
        w.timer = timer(timeout, () => {
          this._waiters.splice(this._waiters.indexOf(w), 1);
          timeoutError.message =
            `${this.name}: ${describePatterns(patterns)} did not appear within ${seconds(timeout)}. ` +
            `Last output: ${tail(this._buf)}`;
          reject(timeoutError);
        });
      }
      this._waiters.push(w);
      this._match();
    });
  }

  waitFor(pattern, opts) {
    return this._wait([pattern], opts, "waitFor");
  }

  waitForAny(patterns, opts) {
    return this._wait(Array.from(patterns), opts, "waitForAny");
  }

  async readUntil(pattern, opts) {
    const r = await this._wait([pattern], opts, "readUntil");
    return r.before + r.match;
  }

  /** Sends `line`, waits for the prompt and returns the output in between (without the echoed command). */
  async command(line, opts = {}) {
    this._buf = ""; // an old prompt in the buffer would end the wait at once
    this.sendLine(line);
    const r = await this._wait([opts.prompt ?? this.prompt], opts, "command");
    let out = r.before.replace(/\r\n?/g, "\n");
    const echo = String(line).trim();
    const nl = out.indexOf("\n");
    if (echo && out.slice(0, nl < 0 ? out.length : nl).includes(echo)) out = nl < 0 ? "" : out.slice(nl + 1);
    return out;
  }

  /** Resolves once nothing has been received for `ms` (counted from now at the earliest). */
  waitForQuiet(ms, opts = {}) {
    const started = Date.now();
    const timeout = opts.timeout ?? DEFAULT_TIMEOUT;
    const timeoutError = new TimeoutError("waitForQuiet timed out");
    return new Promise((resolve, reject) => {
      let check = null;
      const limit =
        timeout > 0 && Number.isFinite(timeout)
          ? timer(timeout, () => {
              cancelTimer(check);
              timeoutError.message = `${this.name}: output did not stop for ${seconds(ms)} within ${seconds(timeout)}`;
              reject(timeoutError);
            })
          : null;
      const test = () => {
        const quiet = Date.now() - Math.max(this._lastData, started);
        if (quiet >= ms) {
          cancelTimer(limit);
          resolve();
        } else check = timer(ms - quiet, test);
      };
      test();
    });
  }

  waitConnected(opts = {}) {
    const timeout = opts.timeout ?? 60000;
    const timeoutError = new TimeoutError("waitConnected timed out");
    return new Promise((resolve, reject) => {
      let limit = null;
      const test = () => {
        if (this.state === "connected") {
          this._stateWaiters.delete(test);
          cancelTimer(limit);
          resolve(this);
        } else if (this.state === "disconnected" && this._seenConnecting) {
          this._stateWaiters.delete(test);
          cancelTimer(limit);
          reject(new Error(`${this.name}: ${this.message || "could not connect"}`));
        } else if (this.state === "connecting") {
          this._seenConnecting = true;
        }
      };
      if (timeout > 0 && Number.isFinite(timeout)) {
        limit = timer(timeout, () => {
          this._stateWaiters.delete(test);
          timeoutError.message = `${this.name}: not connected within ${seconds(timeout)}`;
          reject(timeoutError);
        });
      }
      this._stateWaiters.add(test);
      test();
    });
  }

  _waitFrame(opts) {
    const accepts = (f) => (opts.fcsOk === false || f.fcsOk) && (!opts.filter || opts.filter(f));
    const queued = this._frames.findIndex(accepts);
    if (queued >= 0) return Promise.resolve(this._frames.splice(queued, 1)[0]);
    const timeout = opts.timeout ?? DEFAULT_TIMEOUT;
    const timeoutError = new TimeoutError("waitFrame timed out");
    return new Promise((resolve, reject) => {
      const w = { accepts, timer: null };
      w.resolve = (f) => {
        cancelTimer(w.timer);
        resolve(f);
      };
      if (timeout > 0 && Number.isFinite(timeout)) {
        w.timer = timer(timeout, () => {
          this._frameWaiters.splice(this._frameWaiters.indexOf(w), 1);
          timeoutError.message = `${this.name}: no HDLC frame within ${seconds(timeout)}`;
          reject(timeoutError);
        });
      }
      this._frameWaiters.push(w);
    });
  }

  /** Forgets text and bytes received so far (the next wait only sees new output). */
  discard() {
    this._buf = "";
    this._bin.clear();
  }

  onData(fn) {
    return this._subscribe(this._onData, fn);
  }

  onLine(fn) {
    return this._subscribe(this._onLine, fn);
  }

  // --- control (through the UI, so the tab shows the change)

  setDtr(on) {
    return request("signal", { tab: this.id, signal: "dtr", value: !!on });
  }

  setRts(on) {
    return request("signal", { tab: this.id, signal: "rts", value: !!on });
  }

  setEncoding(name) {
    return request("encoding", { tab: this.id, encoding: String(name) });
  }

  activate() {
    return request("activate", { tab: this.id });
  }

  close() {
    return request("close", { tab: this.id });
  }

  toString() {
    return `Session(${this.name})`;
  }
}

function sessionFor(id, name, watch = true) {
  let s = sessionsById.get(id);
  if (!s) {
    s = new Session(id, name);
    sessionsById.set(id, s);
    if (watch) tw.watch(id);
  } else if (name) {
    s.name = name;
  }
  return s;
}

const sessions = {
  /** A saved session name, quick-connect text ("COM3 115200", "root@host") or an object. */
  async open(target, opts = {}) {
    if (target && typeof target === "object") target = { ...target };
    // The session exists (and hears the tab) before the tab does. The UI answers
    // once the tab is connected, or could not connect.
    const id = tw.newTab();
    const s = sessionFor(id, null, false);
    try {
      const r = await request("open", { tab: id, target, wait: opts.wait !== false, timeout: opts.timeout ?? 60000 });
      s.name = r.name;
      if (opts.wait !== false) s.state = "connected";
    } catch (e) {
      sessionsById.delete(id);
      throw e;
    }
    return s;
  },
  async list() {
    const tabs = await request("list");
    return tabs.map((t) => sessionFor(t.tab, t.name));
  },
  async find(name) {
    return (await sessions.list()).find((s) => s.name === name) ?? null;
  },
};

// ---------------------------------------------------------------- ui and files

const ui = {
  print,
  error: printError,
  prompt: (message, value = "") => request("prompt", { message: String(message), value: String(value) }),
  password: (message = "Password") => request("password", { message: String(message) }),
  confirm: (message) => request("confirm", { message: String(message) }),
  choose: (message, options) => request("choose", { message: String(message), options: Array.from(options, String) }),
  openFile: (opts = {}) => request("openFile", opts),
  saveFile: (opts = {}) => request("saveFile", opts),
  status: (text) => request("status", { text: text == null ? null : String(text) }),
  result: (status, detail = "") => {
    const s = String(status).toUpperCase();
    if (s !== "PASS" && s !== "FAIL") throw new TypeError('ui.result: status is "PASS" or "FAIL"');
    return request("result", { status: s, detail: String(detail) });
  },
};

const fs = {
  resolve: (path) => tw.resolve(String(path)),
  exists: (path) => tw.exists(String(path)),
  readText: (path) => tw.readText(String(path)),
  readBytes: (path) => new Uint8Array(tw.readBytes(String(path))),
  writeText: (path, text) => tw.writeFile(String(path), String(text), false),
  appendText: (path, text) => tw.writeFile(String(path), String(text), true),
  writeBytes: (path, bytes) => tw.writeFile(String(path), toBytes(bytes), false),
};

// ---------------------------------------------------------------- TFTP server

const tftpState = () => JSON.parse(tw.tftpState());
const fileKey = (p) => String(p).split(/[\\/]/).pop().toLowerCase();

const tftp = {
  /** Serves `folder` (relative to the script) to devices. Same server as the TFTP panel. */
  start(opts = {}) {
    const status = JSON.parse(
      tw.tftpStart(
        JSON.stringify({
          folder: String(opts.folder ?? "."),
          port: opts.port ?? 69,
          allowUpload: !!opts.upload,
          bind: opts.bind ?? null,
        }),
      ),
    );
    return { port: status.port, folder: status.config.folder, addresses: tftp.addresses() };
  },
  stop: () => tw.tftpStop(),
  get running() {
    return tftpState().status.running;
  },
  /** IPv4 addresses of this PC, usable ones first: what a device sets as its server address. */
  addresses: () => JSON.parse(tw.tftpAddresses()),
  /** Transfers so far, oldest first (the last 100). */
  transfers: () => tftpState().transfers,
  /**
   * Waits until a device has fetched (or uploaded) `file`: a transfer that is
   * running now or starts later. Returns it; throws if it failed.
   */
  async waitFor(file, opts = {}) {
    const timeout = opts.timeout ?? 120000;
    const key = fileKey(file);
    const error = new Error(); // made here, so its stack points at the caller
    const seen = new Set(tftp.transfers().filter((t) => t.state !== "running").map((t) => t.id));
    const until = timeout > 0 ? Date.now() + timeout : Infinity;
    for (;;) {
      const t = tftp
        .transfers()
        .find((t) => !seen.has(t.id) && fileKey(t.file) === key && (opts.upload === undefined || t.upload === !!opts.upload));
      if (t?.state === "done") return t;
      if (t?.state === "failed") {
        error.message = `TFTP ${t.file} (${t.peer}): ${t.error}`;
        throw error;
      }
      if (Date.now() >= until) throw new TimeoutError(`TFTP: no transfer of "${file}" finished within ${seconds(timeout)}`);
      await sleep(100);
    }
  },
};

Object.assign(globalThis, {
  tab: tw.startTab ? sessionFor(tw.startTab, tw.startTabName) : null,
  sessions,
  ui,
  fs,
  tftp,
  sleep,
  print,
  console: { log: print, info: print, warn: printError, error: printError },
  stripAnsi,
  hex,
  args: [],
  TimeoutError,
  ScriptStopped,
});
