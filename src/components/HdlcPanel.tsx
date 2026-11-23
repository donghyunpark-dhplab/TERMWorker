import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { message, open, save } from "@tauri-apps/plugin-dialog";

import { formatBytes, formatDuration } from "../lib/transfer";
import type { HdlcConfig, HdlcFrame } from "../lib/types";
import type { TermSession } from "../terminal/TermSession";
import { IconClose } from "./Icons";

const DEFAULT_CONFIG: HdlcConfig = { fcs: "fcs16", addressControl: false, escapeControlChars: false, maxFrame: 4096 };
const SETTINGS_KEY = "termworker.hdlc";

/** Last used settings, shared by all tabs. */
function loadSettings(): { config: HdlcConfig; chunk: number; gapMs: number; format: "hex" | "text" } {
  try {
    const saved = JSON.parse(localStorage.getItem(SETTINGS_KEY) ?? "{}");
    return {
      config: { ...DEFAULT_CONFIG, ...saved.config },
      chunk: Number(saved.chunk) || 256,
      gapMs: Number(saved.gapMs) || 0,
      format: saved.format === "text" ? "text" : "hex",
    };
  } catch {
    return { config: DEFAULT_CONFIG, chunk: 256, gapMs: 0, format: "hex" };
  }
}

function storeSettings(patch: Partial<ReturnType<typeof loadSettings>>) {
  localStorage.setItem(SETTINGS_KEY, JSON.stringify({ ...loadSettings(), ...patch }));
}

/** "01 02 0a", "0x01,0x02", "01020A" → bytes. Returns an error message for anything else. */
export function parseHex(input: string): number[] | string {
  const cleaned = input.replace(/0x/gi, " ").replace(/[,;:\-_]/g, " ").trim();
  if (!cleaned) return "Enter some bytes, e.g. 01 02 0A FF";
  const groups = cleaned.split(/\s+/);
  const out: number[] = [];
  for (const g of groups) {
    if (!/^[0-9a-fA-F]+$/.test(g)) return `Not a hex value: ${g}`;
    if (g.length % 2 !== 0 && groups.length === 1) return "Odd number of hex digits";
    if (g.length <= 2) out.push(parseInt(g, 16));
    else {
      if (g.length % 2 !== 0) return `Odd number of hex digits: ${g}`;
      for (let i = 0; i < g.length; i += 2) out.push(parseInt(g.slice(i, i + 2), 16));
    }
  }
  return out;
}

/** Bytes shown per frame in the list; the rest is in the tooltip. Keeps long captures fast. */
const LIST_BYTES = 64;
/** Frames drawn in the list (the newest); older ones stay in memory up to the session's limit. */
const LIST_FRAMES = 500;

function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

function hex(bytes: number[], max = 256): string {
  const shown = bytes.slice(0, max).map((b) => b.toString(16).padStart(2, "0").toUpperCase());
  return shown.join(" ") + (bytes.length > max ? " …" : "");
}

function ascii(bytes: number[], max = 256): string {
  return bytes
    .slice(0, max)
    .map((b) => (b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : "·"))
    .join("");
}

function clock(ms: number): string {
  const d = new Date(ms);
  const p = (n: number, w = 2) => String(n).padStart(w, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`;
}

export function HdlcPanel({ session: s }: { session: TermSession }) {
  useSyncExternalStore(s.subscribe, s.getSnapshot);
  const initial = useRef(loadSettings()).current;
  const [config, setConfig] = useState<HdlcConfig>(s.hdlc ?? initial.config);
  const [format, setFormat] = useState<"hex" | "text">(initial.format);
  const [input, setInput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [chunk, setChunk] = useState(initial.chunk);
  const [gapMs, setGapMs] = useState(initial.gapMs);

  const on = s.hdlc !== null;
  const canSend = s.canHdlcSend;

  const apply = async (next: HdlcConfig, enable = on) => {
    setConfig(next);
    storeSettings({ config: next });
    try {
      await s.setHdlc(enable ? next : null);
    } catch (e) {
      setError(String(e));
    }
  };

  const send = async () => {
    setError(null);
    let payload: number[];
    if (format === "hex") {
      const parsed = parseHex(input);
      if (typeof parsed === "string") {
        setError(parsed);
        return;
      }
      payload = parsed;
    } else {
      if (!input) {
        setError("Enter some text");
        return;
      }
      payload = Array.from(new TextEncoder().encode(input.replace(/\\r/g, "\r").replace(/\\n/g, "\n")));
    }
    try {
      await s.sendHdlc(payload);
    } catch (e) {
      setError(String(e));
    }
  };

  const sendFile = async () => {
    const picked = await open({ title: "Send a file as HDLC frames", multiple: false, directory: false });
    if (typeof picked !== "string") return;
    storeSettings({ chunk, gapMs });
    try {
      await s.sendHdlcFile(picked, chunk, gapMs);
    } catch (e) {
      await message(String(e), { title: "HDLC", kind: "error" });
    }
  };

  const record = async () => {
    if (s.hdlcRecording) {
      await s.recordHdlc(null).catch(() => {});
      return;
    }
    const path = await save({ title: "Save received HDLC payloads", defaultPath: "hdlc-payload.bin" });
    if (!path) return;
    try {
      await s.recordHdlc(path);
    } catch (e) {
      await message(String(e), { title: "HDLC", kind: "error" });
    }
  };

  const sending = s.hdlcSending;

  return (
    <aside className="hdlc-panel">
      <div className="hdlc-head">
        <span className="hdlc-title">HDLC frames</span>
        <button
          className={`hdlc-switch ${on ? "on" : ""}`}
          title={on ? "Stop splitting received data into frames" : "Split received data into HDLC frames"}
          onClick={() => void apply(config, !on)}
        >
          <span className="led" />
          {on ? "On" : "Off"}
        </button>
        <span className="grow" />
        <button className="icon-btn" title="Close panel (HDLC mode stays as it is)" onClick={() => s.setHdlcPanel(false)}>
          <IconClose />
        </button>
      </div>

      <div className="hdlc-settings">
        <label>
          FCS
          <select
            className="input small"
            value={config.fcs}
            onChange={(e) => void apply({ ...config, fcs: e.target.value as HdlcConfig["fcs"] })}
          >
            <option value="fcs16">FCS-16</option>
            <option value="fcs32">FCS-32</option>
            <option value="none">None</option>
          </select>
        </label>
        <label className="check" title="Prefix FF 03 on send and strip it on receive (PPP style)">
          <input
            type="checkbox"
            checked={config.addressControl}
            onChange={(e) => void apply({ ...config, addressControl: e.target.checked })}
          />
          Address/control FF 03
        </label>
        <label className="check" title="Escape 00–1F as well as 7E and 7D (RFC 1662 default)">
          <input
            type="checkbox"
            checked={config.escapeControlChars}
            onChange={(e) => void apply({ ...config, escapeControlChars: e.target.checked })}
          />
          Escape 00–1F
        </label>
      </div>
      {!on && (
        <p className="hdlc-off-note">
          Turn HDLC on to split received data into frames. Text between frames still goes to the terminal.
        </p>
      )}

      <FrameList frames={s.hdlcFrames} onClear={() => s.clearHdlcFrames()} />

      <div className="hdlc-send">
        <div className="row">
          <select
            className="input small"
            value={format}
            onChange={(e) => {
              const f = e.target.value as "hex" | "text";
              setFormat(f);
              storeSettings({ format: f });
            }}
          >
            <option value="hex">Hex</option>
            <option value="text">Text</option>
          </select>
          <input
            className="input mono grow"
            placeholder={format === "hex" ? "01 02 0A FF" : "text (\\r \\n allowed)"}
            value={input}
            spellCheck={false}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void send();
            }}
          />
          <button className="btn small primary" disabled={!canSend} onClick={() => void send()}>
            Send
          </button>
        </div>
        {error && <div className="hdlc-error">{error}</div>}
      </div>

      <div className="hdlc-files">
        <div className="row">
          <label title="Payload bytes per frame">
            Frame
            <input
              className="input small mono"
              type="number"
              min={1}
              max={65536}
              value={chunk}
              onChange={(e) => setChunk(Math.max(1, Math.min(65536, Number(e.target.value) || 1)))}
            />
            B
          </label>
          <label title="Pause after each frame">
            Gap
            <input
              className="input small mono"
              type="number"
              min={0}
              max={10000}
              value={gapMs}
              onChange={(e) => setGapMs(Math.max(0, Math.min(10000, Number(e.target.value) || 0)))}
            />
            ms
          </label>
          <span className="grow" />
          <button className="btn small" disabled={!canSend || !!sending} onClick={() => void sendFile()}>
            Send file…
          </button>
        </div>
        {sending && (
          <div className="hdlc-progress">
            <div className="transfer-file" title={sending.file}>
              {sending.file}
            </div>
            <div className="transfer-bar">
              <div style={{ width: `${sending.size ? Math.min(100, (sending.bytes / sending.size) * 100) : 0}%` }} />
            </div>
            <div className="transfer-stats">
              <span>
                {formatBytes(sending.bytes)} of {formatBytes(sending.size)}
              </span>
              <span>{plural(sending.frames, "frame")}</span>
              <span>{formatDuration(sending.elapsedMs)}</span>
              <span className="grow" />
              <button className="btn tiny" onClick={() => s.cancelHdlcSend()}>
                Cancel
              </button>
            </div>
          </div>
        )}
        <div className="row">
          <button className={`btn small ${s.hdlcRecording ? "danger" : ""}`} disabled={!on} onClick={() => void record()}>
            {s.hdlcRecording ? "Stop saving" : "Save received payloads…"}
          </button>
          {s.hdlcRecording && (
            <span className="hdlc-record" title={s.hdlcRecording.path}>
              {plural(s.hdlcRecording.frames, "frame")} · {formatBytes(s.hdlcRecording.bytes)}
            </span>
          )}
        </div>
      </div>
    </aside>
  );
}

function FrameList({ frames, onClear }: { frames: (HdlcFrame & { seq: number })[]; onClear: () => void }) {
  const listRef = useRef<HTMLDivElement>(null);
  const [follow, setFollow] = useState(true);
  const last = frames.length ? frames[frames.length - 1].seq : 0;

  useLayoutEffect(() => {
    const el = listRef.current;
    if (el && follow) el.scrollTop = el.scrollHeight;
  }, [last, follow]);

  useEffect(() => setFollow(true), []);

  const good = frames.filter((f) => f.fcsOk).length;
  return (
    <div className="hdlc-frames">
      <div className="hdlc-frames-head">
        <span>
          {plural(frames.length, "frame")}
          {frames.length - good ? ` · ${plural(frames.length - good, "FCS error")}` : ""}
        </span>
        <span className="grow" />
        <button className="btn tiny" disabled={!frames.length} onClick={onClear}>
          Clear
        </button>
      </div>
      <div
        ref={listRef}
        className="hdlc-list"
        onScroll={(e) => {
          const el = e.currentTarget;
          setFollow(el.scrollHeight - el.scrollTop - el.clientHeight < 24);
        }}
      >
        {frames.length === 0 && <div className="hdlc-empty">No frames yet.</div>}
        {frames.length > LIST_FRAMES && (
          <div className="hdlc-empty">{plural(frames.length - LIST_FRAMES, "older frame")} not shown.</div>
        )}
        {frames.slice(-LIST_FRAMES).map((f) => (
          <div key={f.seq} className={`hdlc-frame ${f.received ? "rx" : "tx"} ${f.fcsOk ? "" : "bad"}`}>
            <div className="hdlc-meta">
              <span className="mono">{clock(f.timeMs)}</span>
              <span className="hdlc-dir">{f.received ? "RX" : "TX"}</span>
              <span>{f.payload.length} B</span>
              {!f.fcsOk && <span className="hdlc-bad">FCS error</span>}
            </div>
            <div className="hdlc-hex mono" title={f.payload.length > LIST_BYTES ? hex(f.payload, 4096) : undefined}>
              {hex(f.payload, LIST_BYTES)}
            </div>
            <div className="hdlc-ascii mono">{ascii(f.payload, LIST_BYTES)}</div>
          </div>
        ))}
      </div>
    </div>
  );
}
