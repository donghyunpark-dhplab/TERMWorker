import { useMemo, useRef, useState } from "react";

import { describeConnection, KIND_LABEL, parseQuickConnect } from "../lib/profiles";
import type { Connection, Profile } from "../lib/types";
import { IconBolt, IconCopy, IconEdit, IconPlus, IconTrash, KindIcon } from "./Icons";

interface Props {
  profiles: Profile[];
  onOpen: (p: Profile) => void;
  onEdit: (p: Profile) => void;
  onDuplicate: (p: Profile) => void;
  onDelete: (p: Profile) => void;
  onNew: () => void;
  onQuickConnect: (c: Connection) => void;
  onAbout: () => void;
}

export function Sidebar({ profiles, onOpen, onEdit, onDuplicate, onDelete, onNew, onQuickConnect, onAbout }: Props) {
  const [query, setQuery] = useState("");
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  const q = query.trim().toLowerCase();
  const filtered = useMemo(
    () =>
      [...profiles]
        .sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }))
        .filter((p) => !q || p.name.toLowerCase().includes(q) || describeConnection(p.connection).toLowerCase().includes(q)),
    [profiles, q],
  );
  const quick = q ? parseQuickConnect(query) : null;

  const submit = () => {
    if (!q) return;
    if (filtered.length > 0) onOpen(filtered[0]);
    else if (quick) onQuickConnect(quick);
    else return;
    setQuery("");
  };

  return (
    <aside className="sidebar">
      <button className="brand" title="About TermWorker" onClick={onAbout}>
        <img src="/logo.svg" alt="" width={22} height={22} />
        <span>TermWorker</span>
      </button>

      <div className="quick">
        <IconBolt className="quick-icon" />
        <input
          ref={inputRef}
          id="quick-connect"
          className="input"
          value={query}
          spellCheck={false}
          placeholder="Quick connect or search"
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
            if (e.key === "Escape") setQuery("");
          }}
        />
      </div>
      <div className="quick-hint">
        {!q && <span>e.g. COM3 115200 · root@10.0.0.1 · telnet host</span>}
        {q && filtered.length > 0 && (
          <span>
            ↵ open <strong>{filtered[0].name}</strong>
          </span>
        )}
        {q && filtered.length === 0 && quick && (
          <span>
            ↵ connect via {KIND_LABEL[quick.kind]} to <strong>{describeConnection(quick)}</strong>
          </span>
        )}
        {q && filtered.length === 0 && !quick && <span className="warn">Not a recognised address</span>}
      </div>

      <div className="section-title">
        <span>Sessions</span>
        <button className="icon-btn" title="New session (Ctrl+Shift+T)" onClick={onNew}>
          <IconPlus />
        </button>
      </div>

      <ul className="session-list">
        {filtered.map((p) => (
          <li key={p.id} className="session-item" onClick={() => onOpen(p)} title={`Connect to ${p.name}`}>
            <span className={`kind-badge ${p.connection.kind}`}>
              <KindIcon kind={p.connection.kind} size={15} />
            </span>
            <span className="session-text">
              <span className="session-name">{p.name}</span>
              <span className="session-sub">{describeConnection(p.connection)}</span>
            </span>
            {confirmDelete === p.id ? (
              <span className="session-actions visible" onClick={(e) => e.stopPropagation()}>
                <button className="btn tiny danger" onClick={() => onDelete(p)}>
                  Delete
                </button>
                <button className="btn tiny" onClick={() => setConfirmDelete(null)}>
                  Keep
                </button>
              </span>
            ) : (
              <span className="session-actions" onClick={(e) => e.stopPropagation()}>
                <button className="icon-btn" title="Edit" onClick={() => onEdit(p)}>
                  <IconEdit size={14} />
                </button>
                <button className="icon-btn" title="Duplicate" onClick={() => onDuplicate(p)}>
                  <IconCopy size={14} />
                </button>
                <button className="icon-btn" title="Delete" onClick={() => setConfirmDelete(p.id)}>
                  <IconTrash size={14} />
                </button>
              </span>
            )}
          </li>
        ))}
        {profiles.length === 0 && (
          <li className="empty-list">
            No saved sessions yet.
            <button className="btn small" onClick={onNew}>
              <IconPlus size={14} /> New session
            </button>
          </li>
        )}
      </ul>
    </aside>
  );
}
