import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";

import { api } from "../lib/api";
import {
  autoName,
  BAUD_RATES,
  defaultConnection,
  ENCODINGS,
  KIND_LABEL,
  validateConnection,
} from "../lib/profiles";
import type {
  Connection,
  ConnectionKind,
  PortInfo,
  Profile,
  SerialConnection,
  SshConnection,
  TcpConnection,
  TerminalOptions,
} from "../lib/types";
import { IconChevronDown, IconFolder, IconRefresh, KindIcon } from "./Icons";
import { Modal } from "./Modal";

export interface ConnectDialogProps {
  initial: Profile;
  mode: "new" | "edit";
  onCancel: () => void;
  onConnect: (profile: Profile, secret: string | null, save: boolean) => void;
  onSave: (profile: Profile) => void;
}

const KINDS: ConnectionKind[] = ["serial", "ssh", "telnet", "raw"];

export function ConnectDialog({ initial, mode, onCancel, onConnect, onSave }: ConnectDialogProps) {
  const [drafts, setDrafts] = useState<Record<ConnectionKind, Connection>>(() => ({
    serial: defaultConnection("serial"),
    ssh: defaultConnection("ssh"),
    telnet: defaultConnection("telnet"),
    raw: defaultConnection("raw"),
    [initial.connection.kind]: structuredClone(initial.connection),
  }));
  const [kind, setKind] = useState<ConnectionKind>(initial.connection.kind);
  const [name, setName] = useState(mode === "edit" ? initial.name : "");
  const [options, setOptions] = useState<TerminalOptions>(structuredClone(initial.options));
  const [secret, setSecret] = useState("");
  const [save, setSave] = useState(true);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ports, setPorts] = useState<PortInfo[]>([]);

  const conn = drafts[kind];
  const update = useCallback(
    <C extends Connection>(patch: Partial<C>) => {
      setDrafts((d) => ({ ...d, [kind]: { ...d[kind], ...patch } as Connection }));
      setError(null);
    },
    [kind],
  );

  const refreshPorts = useCallback(async () => {
    const list = await api.listSerialPorts().catch(() => [] as PortInfo[]);
    setPorts(list);
    setDrafts((d) => {
      const s = d.serial as SerialConnection;
      return !s.port && list.length ? { ...d, serial: { ...s, port: list[0].name } } : d;
    });
  }, []);

  useEffect(() => {
    void refreshPorts();
  }, [refreshPorts]);

  const profile = useMemo<Profile>(
    () => ({ id: initial.id, name: name.trim() || autoName(conn), connection: conn, options }),
    [initial.id, name, conn, options],
  );

  const submit = (action: "connect" | "save") => {
    const problem = validateConnection(conn);
    if (problem) {
      setError(problem);
      return;
    }
    if (action === "save") onSave(profile);
    else onConnect(profile, secret || null, mode === "edit" || save);
  };

  return (
    <Modal
      title={mode === "edit" ? `Edit session` : "New session"}
      width={600}
      onClose={onCancel}
      footer={
        <>
          <div className="footer-left">
            {error ? (
              <span className="form-error">{error}</span>
            ) : (
              mode === "new" && (
                <label className="check">
                  <input type="checkbox" checked={save} onChange={(e) => setSave(e.target.checked)} />
                  Save to sessions
                </label>
              )
            )}
          </div>
          <button className="btn" onClick={onCancel}>
            Cancel
          </button>
          {mode === "edit" && (
            <button className="btn" onClick={() => submit("save")}>
              Save
            </button>
          )}
          <button className="btn primary" type="submit" form="connect-form">
            {mode === "edit" ? "Save & connect" : "Connect"}
          </button>
        </>
      }
    >
      <form
        id="connect-form"
        onSubmit={(e) => {
          e.preventDefault();
          submit("connect");
        }}
      >
        <div className="segmented kinds" role="tablist">
          {KINDS.map((k) => (
            <button
              type="button"
              key={k}
              role="tab"
              aria-selected={k === kind}
              className={k === kind ? "active" : ""}
              onClick={() => {
                setKind(k);
                setError(null);
              }}
            >
              <KindIcon kind={k} size={15} />
              {KIND_LABEL[k]}
            </button>
          ))}
        </div>

        <div className="form-grid">
          <label>Name</label>
          <input
            className="input"
            value={name}
            placeholder={autoName(conn)}
            onChange={(e) => setName(e.target.value)}
          />

          {conn.kind === "serial" && (
            <SerialFields c={conn} ports={ports} onRefresh={refreshPorts} update={update} />
          )}
          {conn.kind === "ssh" && <SshFields c={conn} secret={secret} setSecret={setSecret} update={update} />}
          {(conn.kind === "telnet" || conn.kind === "raw") && <TcpFields c={conn} update={update} />}
        </div>

        <button type="button" className="disclosure" onClick={() => setShowAdvanced((v) => !v)}>
          <span className={showAdvanced ? "caret open" : "caret"}>▸</span> Terminal options
        </button>
        {showAdvanced && (
          <div className="form-grid">
            <label>Encoding</label>
            <select
              className="input"
              value={options.encoding}
              onChange={(e) => setOptions({ ...options, encoding: e.target.value })}
            >
              {ENCODINGS.map((enc) => (
                <option key={enc.value} value={enc.value}>
                  {enc.label}
                </option>
              ))}
            </select>

            <label>Enter key sends</label>
            <div className="segmented small">
              {(["cr", "lf", "crlf"] as const).map((v) => (
                <button
                  type="button"
                  key={v}
                  className={options.enterSends === v ? "active" : ""}
                  onClick={() => setOptions({ ...options, enterSends: v })}
                >
                  {v === "cr" ? "CR" : v === "lf" ? "LF" : "CR+LF"}
                </button>
              ))}
            </div>

            <span />
            <div className="checks">
              <label className="check">
                <input
                  type="checkbox"
                  checked={options.localEcho}
                  onChange={(e) => setOptions({ ...options, localEcho: e.target.checked })}
                />
                Local echo
              </label>
              <label className="check">
                <input
                  type="checkbox"
                  checked={options.implicitCr}
                  onChange={(e) => setOptions({ ...options, implicitCr: e.target.checked })}
                />
                Treat LF as CR+LF
              </label>
            </div>
          </div>
        )}
      </form>
    </Modal>
  );
}

type Update = <C extends Connection>(patch: Partial<C>) => void;

function SerialFields({
  c,
  ports,
  onRefresh,
  update,
}: {
  c: SerialConnection;
  ports: PortInfo[];
  onRefresh: () => void;
  update: Update;
}) {
  const u = update as (p: Partial<SerialConnection>) => void;
  const current = ports.find((p) => p.name === c.port);
  return (
    <>
      <label>Port</label>
      <div className="row">
        <PortCombo value={c.port} ports={ports} onChange={(port) => u({ port })} />
        <button type="button" className="icon-btn bordered" title="Rescan ports" onClick={onRefresh}>
          <IconRefresh />
        </button>
      </div>
      {current && (
        <>
          <span />
          <div className="hint port-hint">
            {current.description}
            {current.vid !== null && (
              <span className="mono">
                {" "}
                · {hex4(current.vid)}:{hex4(current.pid!)}
              </span>
            )}
          </div>
        </>
      )}

      <label>Baud rate</label>
      <input
        className="input"
        list="baud-rates"
        inputMode="numeric"
        value={Number.isFinite(c.baudRate) ? String(c.baudRate) : ""}
        onChange={(e) => u({ baudRate: Number(e.target.value.replace(/\D/g, "")) })}
      />
      <datalist id="baud-rates">
        {BAUD_RATES.map((b) => (
          <option key={b} value={b} />
        ))}
      </datalist>

      <label>Frame</label>
      <div className="row">
        <select
          className="input"
          value={c.dataBits}
          title="Data bits"
          onChange={(e) => u({ dataBits: Number(e.target.value) as SerialConnection["dataBits"] })}
        >
          {[8, 7, 6, 5].map((b) => (
            <option key={b} value={b}>
              {b} data bits
            </option>
          ))}
        </select>
        <select
          className="input"
          value={c.parity}
          title="Parity"
          onChange={(e) => u({ parity: e.target.value as SerialConnection["parity"] })}
        >
          <option value="none">No parity</option>
          <option value="even">Even parity</option>
          <option value="odd">Odd parity</option>
        </select>
        <select
          className="input"
          value={c.stopBits}
          title="Stop bits"
          onChange={(e) => u({ stopBits: Number(e.target.value) as 1 | 2 })}
        >
          <option value={1}>1 stop bit</option>
          <option value={2}>2 stop bits</option>
        </select>
      </div>

      <label>Flow control</label>
      <select
        className="input"
        value={c.flowControl}
        onChange={(e) => u({ flowControl: e.target.value as SerialConnection["flowControl"] })}
      >
        <option value="none">None</option>
        <option value="hardware">Hardware (RTS/CTS)</option>
        <option value="software">Software (XON/XOFF)</option>
      </select>

      <span />
      <div className="checks">
        <label className="check">
          <input type="checkbox" checked={c.autoReconnect} onChange={(e) => u({ autoReconnect: e.target.checked })} />
          Reconnect automatically when the device is unplugged
        </label>
        <label className="check">
          <input type="checkbox" checked={c.dtr} onChange={(e) => u({ dtr: e.target.checked })} />
          Assert DTR on open
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={c.rts}
            disabled={c.flowControl === "hardware"}
            onChange={(e) => u({ rts: e.target.checked })}
          />
          Assert RTS on open
        </label>
      </div>
    </>
  );
}

function SshFields({
  c,
  secret,
  setSecret,
  update,
}: {
  c: SshConnection;
  secret: string;
  setSecret: (s: string) => void;
  update: Update;
}) {
  const u = update as (p: Partial<SshConnection>) => void;
  const browse = async () => {
    const path = await openFileDialog({ multiple: false, directory: false, title: "Select private key" });
    if (typeof path === "string") u({ keyPath: path });
  };
  return (
    <>
      <label>Host</label>
      <div className="row">
        <input
          className="input grow"
          value={c.host}
          autoFocus
          placeholder="hostname or IP"
          spellCheck={false}
          onChange={(e) => u({ host: e.target.value.trim() })}
        />
        <input
          className="input port"
          inputMode="numeric"
          value={String(c.port)}
          title="Port"
          onChange={(e) => u({ port: Number(e.target.value.replace(/\D/g, "")) })}
        />
      </div>

      <label>Username</label>
      <input
        className="input"
        value={c.username}
        spellCheck={false}
        placeholder="root"
        onChange={(e) => u({ username: e.target.value.trim() })}
      />

      <label>Authentication</label>
      <div className="segmented small">
        <button type="button" className={c.auth === "password" ? "active" : ""} onClick={() => u({ auth: "password" })}>
          Password
        </button>
        <button
          type="button"
          className={c.auth === "publicKey" ? "active" : ""}
          onClick={() => u({ auth: "publicKey" })}
        >
          Private key
        </button>
      </div>

      {c.auth === "password" ? (
        <>
          <label>Password</label>
          <input
            className="input"
            type="password"
            value={secret}
            placeholder="Ask when connecting"
            onChange={(e) => setSecret(e.target.value)}
          />
        </>
      ) : (
        <>
          <label>Key file</label>
          <div className="row">
            <input
              className="input grow mono"
              value={c.keyPath ?? ""}
              spellCheck={false}
              placeholder="~/.ssh/id_ed25519"
              onChange={(e) => u({ keyPath: e.target.value })}
            />
            <button type="button" className="icon-btn bordered" title="Browse…" onClick={browse}>
              <IconFolder />
            </button>
          </div>
          <label>Passphrase</label>
          <input
            className="input"
            type="password"
            value={secret}
            placeholder="Only if the key is encrypted"
            onChange={(e) => setSecret(e.target.value)}
          />
        </>
      )}
      <span />
      <div className="hint">Passwords and passphrases are never saved to disk.</div>
    </>
  );
}

function TcpFields({ c, update }: { c: TcpConnection; update: Update }) {
  const u = update as (p: Partial<TcpConnection>) => void;
  return (
    <>
      <label>Host</label>
      <div className="row">
        <input
          className="input grow"
          value={c.host}
          autoFocus
          spellCheck={false}
          placeholder="hostname or IP"
          onChange={(e) => u({ host: e.target.value.trim() })}
        />
        <input
          className="input port"
          inputMode="numeric"
          value={String(c.port)}
          title="Port"
          onChange={(e) => u({ port: Number(e.target.value.replace(/\D/g, "")) })}
        />
      </div>
      {c.kind === "raw" && (
        <>
          <span />
          <div className="hint">Raw TCP sends and receives bytes unmodified — ideal for serial servers and debug ports.</div>
        </>
      )}
    </>
  );
}

function hex4(n: number) {
  return n.toString(16).padStart(4, "0").toUpperCase();
}

/**
 * Port picker: lists every detected port (unfiltered) with its description,
 * and still accepts a typed name for ports the OS does not enumerate.
 */
function PortCombo({ value, ports, onChange }: { value: string; ports: PortInfo[]; onChange: (port: string) => void }) {
  const [open, setOpen] = useState(false);
  const [hover, setHover] = useState(-1);
  const ref = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);

  const show = () => {
    setHover(Math.max(0, ports.findIndex((p) => p.name === value)));
    setOpen(true);
  };
  const pick = (name: string) => {
    onChange(name);
    setOpen(false);
    inputRef.current?.focus();
  };

  return (
    <div className="combo grow" ref={ref}>
      <input
        ref={inputRef}
        className="input"
        value={value}
        autoFocus
        spellCheck={false}
        placeholder={ports.length ? "Select a port" : "No ports found — type a name"}
        onChange={(e) => onChange(e.target.value)}
        onMouseDown={() => (open ? setOpen(false) : show())}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown" || e.key === "ArrowUp") {
            e.preventDefault();
            if (!open) return show();
            const step = e.key === "ArrowDown" ? 1 : -1;
            setHover((h) => (ports.length ? (h + step + ports.length) % ports.length : -1));
          } else if (e.key === "Enter" && open && ports[hover]) {
            e.preventDefault();
            pick(ports[hover].name);
          } else if (e.key === "Escape" && open) {
            e.stopPropagation();
            setOpen(false);
          }
        }}
      />
      <button
        type="button"
        className="combo-toggle"
        tabIndex={-1}
        title="Show ports"
        onMouseDown={(e) => {
          e.preventDefault();
          if (open) setOpen(false);
          else {
            show();
            inputRef.current?.focus();
          }
        }}
      >
        <IconChevronDown size={14} />
      </button>
      {open && (
        <div className="combo-list" role="listbox">
          {ports.length === 0 && <div className="combo-empty">No serial ports found. Plug in an adapter and press rescan.</div>}
          {ports.map((p, i) => (
            <div
              key={p.name}
              role="option"
              aria-selected={p.name === value}
              className={`combo-item ${i === hover ? "hover" : ""} ${p.name === value ? "selected" : ""}`}
              onMouseEnter={() => setHover(i)}
              onMouseDown={(e) => {
                e.preventDefault();
                pick(p.name);
              }}
            >
              <span className="combo-name mono">{p.name}</span>
              <span className="combo-desc">{p.description}</span>
              {p.vid !== null && (
                <span className="combo-id mono">
                  {hex4(p.vid)}:{hex4(p.pid!)}
                </span>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
