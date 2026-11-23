// Types shared with the Rust backend (see src-tauri/src/config.rs and session.rs).

export type ConnectionKind = "serial" | "ssh" | "telnet" | "raw";

export interface SerialConnection {
  kind: "serial";
  port: string;
  baudRate: number;
  dataBits: 5 | 6 | 7 | 8;
  parity: "none" | "odd" | "even";
  stopBits: 1 | 2;
  flowControl: "none" | "software" | "hardware";
  autoReconnect: boolean;
  dtr: boolean;
  rts: boolean;
}

export interface SshConnection {
  kind: "ssh";
  host: string;
  port: number;
  username: string;
  auth: "password" | "publicKey";
  keyPath?: string;
  keepaliveSecs: number;
}

export interface TcpConnection {
  kind: "telnet" | "raw";
  host: string;
  port: number;
}

export type Connection = SerialConnection | SshConnection | TcpConnection;

export type EnterSends = "cr" | "lf" | "crlf";

export interface TerminalOptions {
  encoding: string;
  enterSends: EnterSends;
  /** Echo typed characters locally (for devices that don't echo). */
  localEcho: boolean;
  /** Treat a bare LF as CR+LF when displaying. */
  implicitCr: boolean;
}

export interface Profile {
  id: string;
  name: string;
  connection: Connection;
  options: TerminalOptions;
}

export type LinkState = "connecting" | "connected" | "reconnecting" | "disconnected";

export interface HostKeyPrompt {
  host: string;
  port: number;
  algorithm: string;
  fingerprint: string;
}

export type SessionEvent =
  | { type: "data"; text: string }
  | { type: "status"; state: LinkState; message: string | null }
  | { type: "notice"; text: string }
  | ({ type: "hostKey" } & HostKeyPrompt)
  | { type: "authFailed" }
  | { type: "log"; path: string | null }
  | ({ type: "transfer" } & TransferStatus)
  | { type: "zmodem"; remoteSends: boolean }
  | ({ type: "hdlcFrame" } & HdlcFrame)
  | ({ type: "hdlcSend" } & HdlcSendStatus)
  | { type: "hdlcRecord"; path: string | null; bytes: number; frames: number };

/** RFC 1662 asynchronous HDLC framing settings. */
export interface HdlcConfig {
  fcs: "none" | "fcs16" | "fcs32";
  /** Prefix FF 03 on send, strip it on receive. */
  addressControl: boolean;
  /** Also escape 00–1F (otherwise only 7E and 7D). */
  escapeControlChars: boolean;
  maxFrame: number;
}

export interface HdlcFrame {
  received: boolean;
  payload: number[];
  fcsOk: boolean;
  timeMs: number;
}

export interface HdlcSendStatus {
  file: string;
  size: number;
  bytes: number;
  frames: number;
  elapsedMs: number;
  outcome: "done" | "failed" | "cancelled" | null;
  message: string | null;
}

export type TransferProtocol = "xmodem" | "xmodem1k" | "ymodem" | "zmodem";

export interface TransferRequest {
  protocol: TransferProtocol;
  direction: "send" | "receive";
  files?: string[];
  dest?: string;
  remoteReady?: boolean;
}

/** Snapshot of a running transfer; `outcome` is set on the last one. */
export interface TransferStatus {
  protocol: TransferProtocol;
  sending: boolean;
  file: string | null;
  path: string | null;
  size: number | null;
  bytes: number;
  filesDone: number;
  totalBytes: number;
  elapsedMs: number;
  outcome: "done" | "failed" | "cancelled" | null;
  message: string | null;
}

export interface PortInfo {
  name: string;
  description: string;
  kind: "usb" | "pci" | "bluetooth" | "unknown";
  vid: number | null;
  pid: number | null;
  serialNumber: string | null;
}

export type LogMode = "raw" | "text" | "timestamped";
export type HostKeyDecision = "acceptAndSave" | "acceptOnce" | "reject";
export type SignalName = "dtr" | "rts";
