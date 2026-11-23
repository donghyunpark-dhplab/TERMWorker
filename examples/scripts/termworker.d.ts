// Type definitions for TermWorker scripts (docs/SCRIPTING.md).
//
// Editors such as VS Code use this file for completion and checking. Keep it
// next to your scripts and start each script with:
//
//   /// <reference path="termworker.d.ts" />

type Pattern = string | RegExp;
type Bytes = Uint8Array | number[] | string;

interface WaitOptions {
  /** Milliseconds; 0 waits forever. Default 30000. */
  timeout?: number;
}

interface Match {
  /** Which pattern matched (waitForAny). */
  index: number;
  /** The matched text. */
  match: string;
  /** Capture groups of a regular expression. */
  groups: string[];
  /** Named capture groups. */
  named?: Record<string, string>;
  /** Text received before the match. */
  before: string;
}

interface Frame {
  payload: Uint8Array;
  fcsOk: boolean;
  time: Date;
}

interface TransferResult {
  files: { name: string; path: string | null; bytes: number }[];
  bytes: number;
  elapsedMs: number;
}

interface Transfer {
  /** Paths are relative to the script's folder. Settles when the transfer ends; throws if it fails. */
  send(files: string | string[]): Promise<TransferResult>;
  /** ZMODEM/YMODEM: `dir` (default: Downloads). XMODEM: `file` is required. */
  receive(opts?: { dir?: string; file?: string }): Promise<TransferResult>;
}

interface Session {
  readonly id: string;
  name: string;
  readonly state: "connecting" | "connected" | "reconnecting" | "disconnected";
  readonly connected: boolean;
  /** Default prompt for command(). Default /[#$>] ?$/. */
  prompt: Pattern;

  /** Sends text; "\r" becomes the session's Enter setting (CR, LF or CR+LF). */
  send(text: string): void;
  /** send(text + Enter). */
  sendLine(text?: string): void;
  sendBytes(bytes: Bytes): void;
  /** "^C", "<ESC>", "<F1>", "<UP>", "<CR>"… and plain text, sent exactly. */
  sendKeys(keys: string): void;
  /** 250 ms break (serial). */
  sendBreak(): Promise<void>;

  /** Waits for text received since the last match (or the start of the script). */
  waitFor(pattern: Pattern, opts?: WaitOptions): Promise<Match>;
  /** Whichever pattern appears first. */
  waitForAny(patterns: Pattern[], opts?: WaitOptions): Promise<Match>;
  /** Everything up to and including the pattern. */
  readUntil(pattern: Pattern, opts?: WaitOptions): Promise<string>;
  /** sendLine + wait for the prompt; returns the output in between without the echoed command. Unread output is discarded first. */
  command(line: string, opts?: WaitOptions & { prompt?: Pattern }): Promise<string>;
  /** Resolves once nothing has arrived for `ms`. */
  waitForQuiet(ms: number, opts?: WaitOptions): Promise<void>;
  waitConnected(opts?: WaitOptions): Promise<Session>;
  /** Forgets output received so far (text and bytes). */
  discard(): void;
  /** Returns a function that unsubscribes. */
  onData(fn: (text: string) => void): () => void;
  onLine(fn: (line: string) => void): () => void;

  /**
   * Received bytes exactly as they arrived, before character decoding (HDLC
   * frames included). Collected from the start of the script, separately from
   * the text that waitFor() searches. Not during a file transfer.
   */
  readonly bytes: {
    /** Exactly `count` bytes. */
    read(count: number, opts?: WaitOptions): Promise<Uint8Array>;
    /** Up to and including `pattern` (a string is matched as UTF-8). */
    readUntil(pattern: Bytes, opts?: WaitOptions): Promise<Uint8Array>;
    /** Number of bytes received and not read yet. */
    readonly available: number;
    /** Everything received and not read yet (may be empty). */
    take(): Uint8Array;
    discard(): void;
    /** Called with each chunk as it arrives. Returns a function that unsubscribes. */
    onData(fn: (chunk: Uint8Array) => void): () => void;
  };

  setDtr(on: boolean): Promise<void>;
  setRts(on: boolean): Promise<void>;
  /** "utf-8", "euc-kr" (CP949), "gb18030", "big5", "windows-1252", "iso-8859-2", "windows-1251". */
  setEncoding(name: string): Promise<string>;
  /** Brings the tab to the front. */
  activate(): Promise<void>;
  close(): Promise<void>;

  readonly log: {
    start(path: string, opts?: { mode?: "timestamp" | "text" | "raw" }): Promise<string>;
    stop(): Promise<void>;
  };
  readonly zmodem: Transfer;
  readonly ymodem: Transfer;
  readonly xmodem: Transfer;
  readonly xmodem1k: Transfer;
  readonly hdlc: {
    enable(opts?: { fcs?: 16 | 32 | 0; addressControl?: boolean; escapeControlChars?: boolean; maxFrame?: number }): Promise<void>;
    disable(): Promise<void>;
    send(payload: Bytes): void;
    /** The next good frame (also frames with FCS errors if `fcsOk: false`). */
    waitFrame(opts?: WaitOptions & { filter?: (f: Frame) => boolean; fcsOk?: boolean }): Promise<Frame>;
    onFrame(fn: (f: Frame) => void): () => void;
  };
}

interface OpenTarget {
  type: "serial" | "ssh" | "telnet" | "raw";
  name?: string;
  /** Serial: "COM3", "/dev/ttyUSB0". TCP: port number. */
  port?: string | number;
  baud?: number;
  dataBits?: 5 | 6 | 7 | 8;
  parity?: "none" | "odd" | "even";
  stopBits?: 1 | 2;
  flowControl?: "none" | "software" | "hardware";
  dtr?: boolean;
  rts?: boolean;
  autoReconnect?: boolean;
  host?: string;
  user?: string;
  /** Kept in memory only. Prefer `await ui.password()` to writing it in the script. */
  password?: string | null;
  keyPath?: string;
  passphrase?: string | null;
  encoding?: string;
  enterSends?: "cr" | "lf" | "crlf";
}

/** The tab the script was run from (null if none). */
declare const tab: Session | null;

declare const sessions: {
  /** A saved session name, quick-connect text ("COM3 115200", "root@10.0.0.1", "telnet host:23") or an object. Opens a new tab. */
  open(target: string | OpenTarget, opts?: { wait?: boolean; timeout?: number }): Promise<Session>;
  /** All open tabs. */
  list(): Promise<Session[]>;
  find(name: string): Promise<Session | null>;
};

declare const ui: {
  /** To the script output panel. */
  print(...values: unknown[]): void;
  error(...values: unknown[]): void;
  /** null if cancelled. */
  prompt(message: string, value?: string): Promise<string | null>;
  /** Never shown, logged or saved. null if cancelled. */
  password(message?: string): Promise<string | null>;
  confirm(message: string): Promise<boolean>;
  choose(message: string, options: string[]): Promise<string | null>;
  openFile(opts?: { title?: string; folder?: boolean }): Promise<string | null>;
  saveFile(opts?: { title?: string; name?: string }): Promise<string | null>;
  /** Short text next to the script's name in the status bar; null clears it. */
  status(text: string | null): Promise<void>;
  /** A PASS/FAIL line in the output panel. */
  result(status: "PASS" | "FAIL", detail?: string): Promise<void>;
};

/** Paths are relative to the script's folder. */
declare const fs: {
  resolve(path: string): string;
  exists(path: string): boolean;
  readText(path: string): string;
  readBytes(path: string): Uint8Array;
  writeText(path: string, text: string): void;
  appendText(path: string, text: string): void;
  writeBytes(path: string, bytes: Bytes): void;
};

interface TftpAddress {
  /** Adapter name ("Ethernet", "eth0"). */
  name: string;
  ip: string;
  prefix: number;
  /** 169.254.x.x: the PC has no DHCP address on that adapter. */
  linkLocal: boolean;
}

interface TftpTransfer {
  id: number;
  /** The device's address. */
  peer: string;
  /** The name the device asked for. */
  file: string;
  /** true: the device sent the file to this PC. */
  upload: boolean;
  size: number | null;
  bytes: number;
  blockSize: number;
  state: "running" | "done" | "failed";
  error: string | null;
  /** Unix time, ms. */
  started: number;
  elapsedMs: number;
}

/** The built-in TFTP server, shared with the TFTP panel in the status bar. */
declare const tftp: {
  /**
   * Serves `folder` (relative to the script, default the script's folder). Port 69 by default;
   * on Linux a port above 1023 needs no extra permission (U-Boot: `setenv tftpdstp 6969`).
   * Devices can only upload when `upload` is true, and never over an existing file.
   */
  start(opts?: { folder?: string; port?: number; upload?: boolean; bind?: string }): {
    port: number;
    folder: string;
    addresses: TftpAddress[];
  };
  stop(): void;
  readonly running: boolean;
  /** IPv4 addresses of this PC, usable ones first. */
  addresses(): TftpAddress[];
  /** The last 100 transfers, oldest first. */
  transfers(): TftpTransfer[];
  /** Until a device has fetched (or uploaded) `file`: a transfer running now or started later. Throws if it failed. Default timeout 120 s. */
  waitFor(file: string, opts?: { timeout?: number; upload?: boolean }): Promise<TftpTransfer>;
};

declare function sleep(ms: number): Promise<void>;
declare function print(...values: unknown[]): void;
/** Removes ANSI escape sequences (colors, cursor movement). */
declare function stripAnsi(text: string): string;
/** "7E 01 02" */
declare function hex(bytes: Uint8Array | number[]): string;
declare const args: string[];

declare class TimeoutError extends Error {}
declare class ScriptStopped extends Error {}
