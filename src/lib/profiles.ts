import type { Connection, ConnectionKind, Profile, TerminalOptions } from "./types";

export const BAUD_RATES = [
  300, 1200, 2400, 4800, 9600, 14400, 19200, 38400, 57600, 115200, 230400, 460800, 921600, 1000000, 1500000,
  2000000, 3000000,
];

export const ENCODINGS: { value: string; label: string }[] = [
  { value: "utf-8", label: "UTF-8" },
  { value: "euc-kr", label: "Korean (CP949 / EUC-KR)" },
  { value: "gb18030", label: "Chinese Simplified (GB18030)" },
  { value: "big5", label: "Chinese Traditional (Big5)" },
  { value: "windows-1252", label: "Western (Latin-1 / CP1252)" },
  { value: "iso-8859-2", label: "Central European (ISO-8859-2)" },
  { value: "windows-1251", label: "Cyrillic (CP1251)" },
];

export function encodingLabel(value: string): string {
  return ENCODINGS.find((e) => e.value === value)?.label.split(" (")[0] ?? value.toUpperCase();
}

export const KIND_LABEL: Record<ConnectionKind, string> = {
  serial: "Serial",
  ssh: "SSH",
  telnet: "Telnet",
  raw: "Raw TCP",
};

export function defaultOptions(): TerminalOptions {
  return { encoding: "utf-8", enterSends: "cr", localEcho: false, implicitCr: false };
}

export function defaultConnection(kind: ConnectionKind, serialPort = ""): Connection {
  switch (kind) {
    case "serial":
      return {
        kind,
        port: serialPort,
        baudRate: 115200,
        dataBits: 8,
        parity: "none",
        stopBits: 1,
        flowControl: "none",
        autoReconnect: true,
        dtr: true,
        rts: true,
      };
    case "ssh":
      return { kind, host: "", port: 22, username: "", auth: "password", keyPath: "", keepaliveSecs: 30 };
    case "telnet":
      return { kind, host: "", port: 23 };
    case "raw":
      return { kind, host: "", port: 23 };
  }
}

export function newId(): string {
  return crypto.randomUUID();
}

export function describeConnection(c: Connection): string {
  switch (c.kind) {
    case "serial":
      return `${c.port || "?"} · ${c.baudRate} ${c.dataBits}${c.parity[0].toUpperCase()}${c.stopBits}`;
    case "ssh":
      return `${c.username ? c.username + "@" : ""}${c.host}${c.port !== 22 ? ":" + c.port : ""}`;
    case "telnet":
    case "raw":
      return `${c.host}:${c.port}`;
  }
}

export function autoName(c: Connection): string {
  switch (c.kind) {
    case "serial":
      return c.port || "Serial";
    case "ssh":
      return c.host ? `${c.username ? c.username + "@" : ""}${c.host}` : "SSH";
    default:
      return c.host ? `${c.host}:${c.port}` : KIND_LABEL[c.kind];
  }
}

export function validateConnection(c: Connection): string | null {
  switch (c.kind) {
    case "serial":
      if (!c.port.trim()) return "Select a serial port.";
      if (!Number.isInteger(c.baudRate) || c.baudRate <= 0) return "Enter a valid baud rate.";
      return null;
    case "ssh":
      if (!c.host.trim()) return "Enter a host name or IP address.";
      if (!c.username.trim()) return "Enter a user name.";
      if (c.auth === "publicKey" && !c.keyPath?.trim()) return "Choose a private key file.";
      return validPort(c.port);
    default:
      if (!c.host.trim()) return "Enter a host name or IP address.";
      return validPort(c.port);
  }
}

function validPort(p: number): string | null {
  return Number.isInteger(p) && p > 0 && p < 65536 ? null : "Port must be between 1 and 65535.";
}

/**
 * Parse quick-connect input. Accepted forms:
 *   COM3            COM3 115200        /dev/ttyUSB0 9600
 *   user@host       user@host:2222     ssh host
 *   telnet host     telnet host:2323   raw host:5000    host:port (raw)
 */
export function parseQuickConnect(input: string): Connection | null {
  const text = input.trim();
  if (!text) return null;
  const parts = text.split(/\s+/);

  const serial = /^(COM\d+|\/dev\/\S+)$/i;
  if (serial.test(parts[0])) {
    const c = defaultConnection("serial", parts[0].toUpperCase().startsWith("COM") ? parts[0].toUpperCase() : parts[0]);
    if (parts[1] && /^\d+$/.test(parts[1]) && c.kind === "serial") c.baudRate = Number(parts[1]);
    return c;
  }

  let kind: ConnectionKind | null = null;
  let target = parts[0];
  if (["ssh", "telnet", "raw"].includes(parts[0].toLowerCase()) && parts[1]) {
    kind = parts[0].toLowerCase() as ConnectionKind;
    target = parts[1];
  }

  const m = /^(?:([^@\s]+)@)?(\[[^\]]+\]|[^:\s]+)(?::(\d+))?$/.exec(target);
  if (!m) return null;
  const [, user, rawHost, port] = m;
  const host = rawHost.replace(/^\[|\]$/g, "");
  if (!kind) kind = user || !port ? "ssh" : "raw";

  const c = defaultConnection(kind);
  if (c.kind === "serial") return null;
  c.host = host;
  if (port) c.port = Number(port);
  if (c.kind === "ssh") c.username = user ?? "";
  return c;
}

export function profileFromConnection(connection: Connection, name?: string): Profile {
  return { id: newId(), name: name?.trim() || autoName(connection), connection, options: defaultOptions() };
}

/** Fill fields added in later versions so old saved sessions keep working. */
export function normalizeProfile(p: Profile): Profile {
  const base = defaultConnection(p.connection.kind);
  return {
    ...p,
    connection: { ...base, ...p.connection } as Connection,
    options: { ...defaultOptions(), ...p.options },
  };
}
