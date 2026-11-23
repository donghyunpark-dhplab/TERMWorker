import { useEffect, useRef, useState, useSyncExternalStore } from "react";

import { formatBytes, formatDuration, PROTOCOL_LABEL } from "../lib/transfer";
import type { TermSession } from "../terminal/TermSession";
import { HdlcPanel } from "./HdlcPanel";
import { IconChevronDown, IconChevronUp, IconClose, IconSearch, IconTransfer } from "./Icons";

interface Props {
  session: TermSession;
  active: boolean;
  findOpen: boolean;
  onCloseFind: () => void;
}

export function TerminalPane({ session, active, findOpen, onCloseFind }: Props) {
  const hostRef = useRef<HTMLDivElement>(null);
  useSyncExternalStore(session.subscribe, session.getSnapshot);

  useEffect(() => {
    if (hostRef.current) session.attach(hostRef.current);
  }, [session]);

  useEffect(() => {
    if (!active) return;
    const id = requestAnimationFrame(() => session.focus());
    return () => cancelAnimationFrame(id);
  }, [active, session]);

  return (
    <div className={`terminal-pane ${active ? "" : "hidden"} ${session.hdlcPanelOpen ? "with-side" : ""}`}>
      <div ref={hostRef} className="terminal-host" />
      {session.hdlcPanelOpen && <HdlcPanel session={session} />}
      <TransferPanel session={session} />
      {active && findOpen && (
        <FindBar
          session={session}
          onClose={() => {
            session.search.clearDecorations();
            onCloseFind();
            session.focus();
          }}
        />
      )}
    </div>
  );
}

function FindBar({ session, onClose }: { session: TermSession; onClose: () => void }) {
  const [query, setQuery] = useState(session.term.getSelection());
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [notFound, setNotFound] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.select();
  }, []);

  const find = (dir: "next" | "prev", incremental = false) => {
    if (!query) {
      setNotFound(false);
      return;
    }
    const opts = { caseSensitive, incremental };
    const ok = dir === "next" ? session.search.findNext(query, opts) : session.search.findPrevious(query, opts);
    setNotFound(!ok);
  };

  return (
    <div className="findbar" onMouseDown={(e) => e.stopPropagation()}>
      <IconSearch size={14} className="muted" />
      <input
        ref={inputRef}
        className={`input ${notFound ? "not-found" : ""}`}
        value={query}
        placeholder="Find in scrollback"
        spellCheck={false}
        onChange={(e) => {
          setQuery(e.target.value);
          const ok = e.target.value ? session.search.findNext(e.target.value, { caseSensitive, incremental: true }) : true;
          setNotFound(!ok);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") find(e.shiftKey ? "prev" : "next");
          if (e.key === "Escape") {
            e.stopPropagation();
            onClose();
          }
        }}
      />
      <button
        className={`icon-btn toggle ${caseSensitive ? "on" : ""}`}
        title="Match case"
        onClick={() => setCaseSensitive((v) => !v)}
      >
        Aa
      </button>
      <button className="icon-btn" title="Previous (Shift+Enter)" onClick={() => find("prev")}>
        <IconChevronUp size={15} />
      </button>
      <button className="icon-btn" title="Next (Enter)" onClick={() => find("next")}>
        <IconChevronDown size={15} />
      </button>
      <button className="icon-btn" title="Close (Esc)" onClick={onClose}>
        <IconClose size={14} />
      </button>
    </div>
  );
}

/** Progress of a running file transfer, docked over the bottom of the terminal. */
function TransferPanel({ session }: { session: TermSession }) {
  useSyncExternalStore(session.subscribe, session.getSnapshot);
  const t = session.transfer;
  // Tick once a second so the clock runs while the remote side is silent.
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!t) return;
    const id = setInterval(() => setTick((n) => n + 1), 1000);
    return () => clearInterval(id);
  }, [t !== null]); // eslint-disable-line react-hooks/exhaustive-deps
  if (!t) return null;

  const elapsed = Math.max(t.elapsedMs, Date.now() - session.transferStartedAt);
  const secs = t.elapsedMs / 1000;
  const rate = secs > 0.5 ? t.totalBytes / secs : 0;
  const pct = t.size ? Math.min(100, (t.bytes / t.size) * 100) : null;
  const left = t.size && rate > 0 ? ((t.size - t.bytes) / rate) * 1000 : null;
  const waiting = !t.file;

  return (
    <div className="transfer-panel" onMouseDown={(e) => e.stopPropagation()}>
      <div className="transfer-head">
        <IconTransfer size={14} />
        <span className="transfer-title">
          {t.sending ? "Sending" : "Receiving"} · {PROTOCOL_LABEL[t.protocol]}
        </span>
        <span className="grow" />
        <button className="btn small" onClick={() => session.cancelTransfer()}>
          Cancel
        </button>
      </div>
      <div className="transfer-file mono" title={t.path ?? undefined}>
        {waiting ? (t.sending ? "Waiting for the receiver to start…" : "Waiting for the sender…") : t.file}
        {t.filesDone > 0 && <span className="muted"> · file {t.filesDone + 1}</span>}
      </div>
      <div className={`transfer-bar ${pct === null ? "indeterminate" : ""}`}>
        <div style={{ width: pct === null ? undefined : `${pct}%` }} />
      </div>
      <div className="transfer-stats mono">
        <span>
          {formatBytes(t.bytes)}
          {t.size !== null && ` of ${formatBytes(t.size)}`}
        </span>
        {rate > 0 && <span>{formatBytes(rate)}/s</span>}
        <span className="grow" />
        <span>{left !== null ? `${formatDuration(left)} left` : formatDuration(elapsed)}</span>
      </div>
    </div>
  );
}
