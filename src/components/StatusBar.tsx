import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { message, save } from "@tauri-apps/plugin-dialog";

import { api } from "../lib/api";
import { encodingLabel, ENCODINGS, KIND_LABEL } from "../lib/profiles";
import type { LinkState, LogMode } from "../lib/types";
import type { TermSession } from "../terminal/TermSession";
import { pickFilesToSend, pickReceiveDest, PROTOCOL_LABEL } from "../lib/transfer";
import type { TransferProtocol } from "../lib/types";
import { IconRecord, IconTransfer } from "./Icons";
import { ScriptMenu } from "./Script";
import { TftpButton } from "./TftpPanel";

const STATE_LABEL: Record<LinkState, string> = {
  connecting: "Connecting",
  connected: "Connected",
  reconnecting: "Waiting for device",
  disconnected: "Disconnected",
};

export function StatusBar({ session }: { session: TermSession | null }) {
  if (!session) {
    return (
      <footer className="statusbar">
        <span className="muted">No session</span>
        <span className="grow" />
        <TftpButton />
        <ScriptMenu tab={null} />
      </footer>
    );
  }
  return <SessionStatus key={session.tabId} s={session} />;
}

function SessionStatus({ s }: { s: TermSession }) {
  useSyncExternalStore(s.subscribe, s.getSnapshot);
  const conn = s.profile.connection;
  const isSerial = conn.kind === "serial";
  const live = s.state === "connected";

  return (
    <footer className="statusbar">
      <span className={`state-dot ${s.state}`} />
      <span className="status-state">{STATE_LABEL[s.state]}</span>
      <span className="status-sep" />
      <span className="status-kind">{KIND_LABEL[conn.kind]}</span>
      <span className="status-summary" title={s.message ?? s.summary}>
        {s.state === "connected" || !s.message ? s.summary : s.message}
      </span>

      <span className="grow" />

      {isSerial && (
        <span className="status-group">
          <SignalToggle label="DTR" on={s.signals.dtr} disabled={!live} onClick={() => s.setSignal("dtr", !s.signals.dtr)} />
          <SignalToggle
            label="RTS"
            on={s.signals.rts}
            disabled={!live || conn.flowControl === "hardware"}
            onClick={() => s.setSignal("rts", !s.signals.rts)}
          />
          <button className="status-btn" disabled={!live} title="Send a 250 ms break signal" onClick={() => s.sendBreak()}>
            Break
          </button>
        </span>
      )}

      <TftpButton />
      <ScriptMenu tab={s} />
      <TransferMenu s={s} />
      <button
        className={`status-btn ${s.hdlc ? "hdlc-on" : ""}`}
        title={s.hdlcPanelOpen ? "Hide the HDLC frame panel" : "Show the HDLC frame panel"}
        onClick={() => s.setHdlcPanel(!s.hdlcPanelOpen)}
      >
        HDLC
      </button>
      <OptionsMenu s={s} />
      <EncodingMenu s={s} />
      <LogButton s={s} />
      <span className="status-size mono" title="Terminal size (columns × rows)">
        {s.cols}×{s.rows}
      </span>
    </footer>
  );
}

function SignalToggle({ label, on, disabled, onClick }: { label: string; on: boolean; disabled: boolean; onClick: () => void }) {
  return (
    <button
      className={`status-btn signal ${on ? "on" : ""}`}
      disabled={disabled}
      title={`${label} is ${on ? "asserted" : "cleared"} — click to toggle`}
      onClick={onClick}
    >
      <span className="led" />
      {label}
    </button>
  );
}

function usePopover() {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);
  return { open, setOpen, ref };
}

function EncodingMenu({ s }: { s: TermSession }) {
  const { open, setOpen, ref } = usePopover();
  return (
    <span className="popover-anchor" ref={ref}>
      <button className="status-btn" title="Character encoding" onClick={() => setOpen(!open)}>
        {encodingLabel(s.profile.options.encoding)}
      </button>
      {open && (
        <div className="popover up">
          {ENCODINGS.map((e) => (
            <button
              key={e.value}
              className={`menu-item ${e.value === s.profile.options.encoding ? "checked" : ""}`}
              onClick={() => {
                void s.setEncoding(e.value);
                setOpen(false);
              }}
            >
              {e.label}
            </button>
          ))}
        </div>
      )}
    </span>
  );
}

function OptionsMenu({ s }: { s: TermSession }) {
  const { open, setOpen, ref } = usePopover();
  const o = s.profile.options;
  return (
    <span className="popover-anchor" ref={ref}>
      <button className="status-btn" title="Terminal options" onClick={() => setOpen(!open)}>
        Options
      </button>
      {open && (
        <div className="popover up">
          <button className={`menu-item ${o.localEcho ? "checked" : ""}`} onClick={() => s.setOption("localEcho", !o.localEcho)}>
            Local echo
          </button>
          <button className={`menu-item ${o.implicitCr ? "checked" : ""}`} onClick={() => s.setOption("implicitCr", !o.implicitCr)}>
            Treat LF as CR+LF
          </button>
          <div className="menu-sep" />
          <button
            className="menu-item"
            onClick={() => {
              s.clear();
              setOpen(false);
            }}
          >
            Clear scrollback
          </button>
        </div>
      )}
    </span>
  );
}

const LOG_MODES: { mode: LogMode; label: string; hint: string }[] = [
  { mode: "timestamped", label: "Text with timestamps", hint: "[2026-01-31 09:15:02.123] per line" },
  { mode: "text", label: "Plain text", hint: "Escape sequences removed" },
  { mode: "raw", label: "Raw bytes", hint: "Exactly as received" },
];

function LogButton({ s }: { s: TermSession }) {
  const { open, setOpen, ref } = usePopover();

  const start = async (mode: LogMode) => {
    setOpen(false);
    try {
      const defaultPath = await api.defaultLogPath(s.title);
      const path = await save({
        title: "Save session log",
        defaultPath,
        filters: [{ name: "Log files", extensions: ["log", "txt"] }],
      });
      if (path) await s.startLog(path, mode);
    } catch (e) {
      await message(String(e), { title: "Logging", kind: "error" });
    }
  };

  return (
    <span className="popover-anchor" ref={ref}>
      <button
        className={`status-btn ${s.logPath ? "recording" : ""}`}
        title={s.logPath ? `Logging to ${s.logPath}` : "Start logging this session"}
        disabled={!s.canLog && !s.logPath}
        onClick={() => setOpen(!open)}
      >
        <IconRecord size={12} />
        {s.logPath ? "REC" : "Log"}
      </button>
      {open && (
        <div className="popover up right">
          {s.logPath ? (
            <>
              <div className="menu-note mono">{s.logPath}</div>
              <button
                className="menu-item"
                onClick={() => {
                  void s.stopLog();
                  setOpen(false);
                }}
              >
                Stop logging
              </button>
            </>
          ) : (
            LOG_MODES.map((m) => (
              <button key={m.mode} className="menu-item two-line" onClick={() => void start(m.mode)}>
                <span>{m.label}</span>
                <span className="menu-hint">{m.hint}</span>
              </button>
            ))
          )}
        </div>
      )}
    </span>
  );
}

const SEND_PROTOCOLS: TransferProtocol[] = ["zmodem", "ymodem", "xmodem", "xmodem1k"];
const RECEIVE_PROTOCOLS: TransferProtocol[] = ["zmodem", "ymodem", "xmodem"];

const RECEIVE_HINT: Partial<Record<TransferProtocol, string>> = {
  zmodem: "Into a folder · usually starts by itself when you run sz",
  ymodem: "Into a folder · the sender names the files",
  xmodem: "Into one file you name · CRC, checksum and 1K blocks",
};

const PROTOCOL_HINT: Record<TransferProtocol, string> = {
  zmodem: "Linux rz / sz · fastest, resends only damaged data",
  ymodem: "U-Boot loady · several files with names",
  xmodem: "U-Boot loadx · bootloaders, 128-byte blocks",
  xmodem1k: "1 KB blocks · faster XMODEM",
};

function TransferMenu({ s }: { s: TermSession }) {
  const { open, setOpen, ref } = usePopover();

  const send = async (p: TransferProtocol) => {
    setOpen(false);
    try {
      const files = await pickFilesToSend(p);
      if (files?.length) await s.sendFiles(p, files);
    } catch (e) {
      await message(String(e), { title: "File transfer", kind: "error" });
    }
    s.focus();
  };

  const receive = async (p: TransferProtocol) => {
    setOpen(false);
    try {
      const dest = await pickReceiveDest(p);
      if (dest) await s.receiveFiles(p, dest);
    } catch (e) {
      await message(String(e), { title: "File transfer", kind: "error" });
    }
    s.focus();
  };

  return (
    <span className="popover-anchor" ref={ref}>
      <button
        className={`status-btn ${s.transfer ? "recording" : ""}`}
        title="Send or receive files (ZMODEM, YMODEM, XMODEM)"
        disabled={!s.canTransfer}
        onClick={() => setOpen(!open)}
      >
        <IconTransfer size={13} />
        Transfer
      </button>
      {open && (
        <div className="popover up right transfer-menu">
          <div className="menu-heading">Send file</div>
          {SEND_PROTOCOLS.map((p) => (
            <button key={`s-${p}`} className="menu-item two-line" onClick={() => void send(p)}>
              <span>{PROTOCOL_LABEL[p]}…</span>
              <span className="menu-hint">{PROTOCOL_HINT[p]}</span>
            </button>
          ))}
          <div className="menu-sep" />
          <div className="menu-heading">Receive file</div>
          {RECEIVE_PROTOCOLS.map((p) => (
            <button key={`r-${p}`} className="menu-item two-line" onClick={() => void receive(p)}>
              <span>{PROTOCOL_LABEL[p]}…</span>
              <span className="menu-hint">{RECEIVE_HINT[p]}</span>
            </button>
          ))}
        </div>
      )}
    </span>
  );
}
