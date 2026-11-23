import { useSyncExternalStore } from "react";

import type { TermSession } from "../terminal/TermSession";
import { IconClose, IconPlus, IconSidebar, KindIcon } from "./Icons";

interface Props {
  tabs: TermSession[];
  activeId: string | null;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  onNew: () => void;
  onToggleSidebar: () => void;
}

export function TabBar({ tabs, activeId, onSelect, onClose, onNew, onToggleSidebar }: Props) {
  return (
    <div className="tabbar">
      <button className="icon-btn tabbar-btn" title="Toggle sidebar (Ctrl+Shift+B)" onClick={onToggleSidebar}>
        <IconSidebar />
      </button>
      <div className="tabs" role="tablist">
        {tabs.map((t) => (
          <Tab key={t.tabId} s={t} active={t.tabId === activeId} onSelect={onSelect} onClose={onClose} />
        ))}
        <button className="icon-btn tabbar-btn new-tab" title="New session (Ctrl+Shift+T)" onClick={onNew}>
          <IconPlus />
        </button>
      </div>
    </div>
  );
}

function Tab({
  s,
  active,
  onSelect,
  onClose,
}: {
  s: TermSession;
  active: boolean;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
}) {
  useSyncExternalStore(s.subscribe, s.getSnapshot);
  const attention = !active && (s.hostKey || s.secretPrompt);
  return (
    <div
      role="tab"
      aria-selected={active}
      className={`tab ${active ? "active" : ""}`}
      title={`${s.title} — ${s.summary}`}
      onMouseDown={(e) => {
        if (e.button === 1) {
          e.preventDefault();
          onClose(s.tabId);
        } else if (e.button === 0) {
          onSelect(s.tabId);
        }
      }}
    >
      <span className={`state-dot ${attention ? "attention" : s.state}`} />
      <KindIcon kind={s.profile.connection.kind} size={14} className="tab-kind" />
      <span className="tab-title">{s.title}</span>
      {s.logPath && <span className="rec-dot" title={`Logging to ${s.logPath}`} />}
      <button
        className="tab-close"
        title="Close (Ctrl+Shift+W)"
        onMouseDown={(e) => e.stopPropagation()}
        onClick={() => onClose(s.tabId)}
      >
        <IconClose size={13} />
      </button>
    </div>
  );
}
