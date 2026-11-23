import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { open } from "@tauri-apps/plugin-dialog";

import { scripts, type ScriptDialog } from "../lib/script";
import type { TermSession } from "../terminal/TermSession";
import { IconClose } from "./Icons";
import { Modal } from "./Modal";

function useScripts() {
  useSyncExternalStore(scripts.subscribe, scripts.getSnapshot);
  return scripts;
}

const baseName = (p: string) => p.split(/[\\/]/).pop() ?? p;
/** Long paths keep their end (the folder the script is in). */
const shortPath = (p: string, max = 40) => (p.length > max ? `…${p.slice(-(max - 1))}` : p);

async function pickAndRun(tab: TermSession | null) {
  const path = await open({
    title: "Run script",
    multiple: false,
    filters: [{ name: "JavaScript", extensions: ["js", "mjs"] }],
  });
  if (typeof path === "string") await scripts.run(path, tab);
}

/** Status bar: run a script, see what is running, stop it. */
export function ScriptMenu({ tab }: { tab: TermSession | null }) {
  const s = useScripts();
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

  const running = s.running;
  const first = running[0];
  const run = (fn: () => Promise<void> | void) => {
    setOpen(false);
    void fn();
  };

  return (
    <span className="popover-anchor" ref={ref}>
      <button
        className={`status-btn ${running.length ? "script-running" : ""}`}
        title={running.length ? `Running: ${running.map((r) => r.name).join(", ")}` : "Run a JavaScript file against this tab"}
        onClick={() => setOpen(!open)}
      >
        <span className="script-glyph">{running.length ? "▶" : "{}"}</span>
        {first ? (
          <span className="script-chip">
            {first.name}
            {first.status && <span className="muted"> · {first.status}</span>}
            {running.length > 1 && <span className="muted"> +{running.length - 1}</span>}
          </span>
        ) : (
          "Script"
        )}
      </button>
      {open && (
        <div className="popover up right script-menu">
          <button className="menu-item two-line" onClick={() => run(() => pickAndRun(tab))}>
            <span>Run script…</span>
            <span className="menu-hint">{tab ? `tab = ${tab.title}` : "No tab: the script opens its own sessions"}</span>
          </button>
          {s.recent.length > 0 && (
            <>
              <div className="menu-sep" />
              <div className="menu-heading">Recent</div>
              {s.recent.map((p) => (
                <button key={p} className="menu-item two-line" title={p} onClick={() => run(() => s.run(p, tab))}>
                  <span>{baseName(p)}</span>
                  <span className="menu-hint mono">{shortPath(p)}</span>
                </button>
              ))}
            </>
          )}
          {running.length > 0 && (
            <>
              <div className="menu-sep" />
              <div className="menu-heading">Running</div>
              {running.map((r) => (
                <button key={r.run} className="menu-item" onClick={() => run(() => s.stop(r.run))}>
                  Stop {r.name}
                </button>
              ))}
            </>
          )}
          <div className="menu-sep" />
          <button className="menu-item" onClick={() => run(() => s.setPanel(!s.panelOpen))}>
            {s.panelOpen ? "Hide output" : "Show output"}
          </button>
        </div>
      )}
    </span>
  );
}

/** Output of scripts, docked above the status bar. */
export function ScriptPanel() {
  const snapshot = useSyncExternalStore(scripts.subscribe, scripts.getSnapshot);
  const s = scripts;
  const listRef = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const lines = s.output;

  useLayoutEffect(() => {
    const el = listRef.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [snapshot]);

  if (!s.panelOpen) return null;
  const running = s.running;
  const names = new Map(s.runs.map((r) => [r.run, r.name]));
  const several = new Set(lines.map((l) => l.run)).size > 1;

  return (
    <section className="script-panel" onMouseDown={(e) => e.stopPropagation()}>
      <header className="script-head">
        <span className="script-title">Script output</span>
        {running.map((r) => (
          <span key={r.run} className="script-run">
            <span className="script-dot" />
            {r.name}
            {r.status && <span className="muted"> · {r.status}</span>}
            <button className="btn small" onClick={() => s.stop(r.run)}>
              Stop
            </button>
          </span>
        ))}
        <span className="grow" />
        <button className="btn small" disabled={!lines.length} onClick={() => s.clearOutput()}>
          Clear
        </button>
        <button className="icon-btn" title="Hide" onClick={() => s.setPanel(false)}>
          <IconClose size={14} />
        </button>
      </header>
      <div
        ref={listRef}
        className="script-lines mono selectable"
        onScroll={(e) => {
          const el = e.currentTarget;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
        }}
      >
        {lines.length === 0 && <div className="muted">Output of ui.print() and errors appear here.</div>}
        {lines.map((l) => (
          <div key={l.seq} className={`script-line ${l.stream}`}>
            {several && <span className="script-src">{names.get(l.run) ?? ""}</span>}
            {l.text || " "}
          </div>
        ))}
      </div>
    </section>
  );
}

/** ui.prompt / password / confirm / choose. */
export function ScriptDialogs() {
  const s = useScripts();
  const d = s.dialogs[0];
  return d ? <Dialog key={`${d.run}-${d.id}`} d={d} /> : null;
}

function Dialog({ d }: { d: ScriptDialog }) {
  const [value, setValue] = useState(d.value ?? "");
  const cancelValue = d.kind === "confirm" ? false : null;
  const cancel = () => scripts.answer(d, cancelValue);

  if (d.kind === "confirm" || d.kind === "choose") {
    return (
      <Modal
        title={d.script}
        width={440}
        onClose={cancel}
        footer={
          d.kind === "confirm" ? (
            <>
              <button className="btn" onClick={() => scripts.answer(d, false)}>
                No
              </button>
              <button className="btn primary" autoFocus onClick={() => scripts.answer(d, true)}>
                Yes
              </button>
            </>
          ) : (
            <button className="btn" onClick={cancel}>
              Cancel
            </button>
          )
        }
      >
        <p className="script-message">{d.message}</p>
        {d.kind === "choose" && (
          <div className="script-choices">
            {(d.options ?? []).map((o, i) => (
              <button key={i} className="btn" autoFocus={i === 0} onClick={() => scripts.answer(d, o)}>
                {o}
              </button>
            ))}
          </div>
        )}
      </Modal>
    );
  }

  return (
    <Modal
      title={d.script}
      width={440}
      onClose={cancel}
      footer={
        <>
          <button className="btn" onClick={cancel}>
            Cancel
          </button>
          <button className="btn primary" form="script-form" type="submit">
            OK
          </button>
        </>
      }
    >
      <form
        id="script-form"
        onSubmit={(e) => {
          e.preventDefault();
          scripts.answer(d, value);
        }}
      >
        <p className="script-message">{d.message}</p>
        <input
          className="input"
          type={d.kind === "password" ? "password" : "text"}
          autoComplete="off"
          spellCheck={false}
          autoFocus
          value={value}
          onChange={(e) => setValue(e.target.value)}
        />
      </form>
    </Modal>
  );
}
