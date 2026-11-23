import { describeConnection } from "../lib/profiles";
import type { ConnectionKind, Profile } from "../lib/types";
import { KindIcon } from "./Icons";

interface Props {
  profiles: Profile[];
  onNew: (kind: ConnectionKind) => void;
  onOpen: (p: Profile) => void;
  onAbout: () => void;
}

const SHORTCUTS: [string, string][] = [
  ["Ctrl+Shift+T", "New session"],
  ["Ctrl+Shift+W", "Close tab"],
  ["Ctrl+Tab", "Next tab"],
  ["Ctrl+Shift+F", "Find in scrollback"],
  ["Ctrl+Shift+C / V", "Copy / paste"],
  ["Right click", "Copy selection, or paste"],
  ["Ctrl+K", "Quick connect"],
];

export function Welcome({ profiles, onNew, onOpen, onAbout }: Props) {
  const recent = profiles.slice(0, 6);
  return (
    <div className="welcome">
      <div className="welcome-inner">
        <img src="/logo.svg" alt="" width={56} height={56} />
        <h1>TermWorker</h1>
        <p className="muted">Serial, SSH, Telnet and raw TCP — in one clean terminal.</p>

        <div className="welcome-actions">
          {(["serial", "ssh", "telnet", "raw"] as const).map((k) => (
            <button key={k} className="welcome-card" onClick={() => onNew(k)}>
              <span className={`kind-badge large ${k}`}>
                <KindIcon kind={k} size={20} />
              </span>
              <span>{k === "serial" ? "Serial port" : k === "ssh" ? "SSH" : k === "telnet" ? "Telnet" : "Raw TCP"}</span>
            </button>
          ))}
        </div>

        {recent.length > 0 && (
          <div className="welcome-recent">
            <h3>Saved sessions</h3>
            {recent.map((p) => (
              <button key={p.id} className="recent-item" onClick={() => onOpen(p)}>
                <KindIcon kind={p.connection.kind} size={14} />
                <span className="recent-name">{p.name}</span>
                <span className="muted">{describeConnection(p.connection)}</span>
              </button>
            ))}
          </div>
        )}

        <dl className="shortcuts">
          {SHORTCUTS.map(([k, v]) => (
            <div key={k}>
              <dt>
                <kbd>{k}</kbd>
              </dt>
              <dd>{v}</dd>
            </div>
          ))}
        </dl>

        <button className="link-btn welcome-about" onClick={onAbout}>
          About TermWorker
        </button>
      </div>
    </div>
  );
}
