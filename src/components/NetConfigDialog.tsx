import { useCallback, useEffect, useState } from "react";
import { ask } from "@tauri-apps/plugin-dialog";

import {
  describeSaved,
  listAdapters,
  loadApplied,
  methodLabel,
  pruneApplied,
  PREFIXES,
  restore,
  setDhcp,
  setStatic,
  suggestFor,
  type Adapter,
} from "../lib/netcfg";
import { isWindows } from "../lib/tftp";
import { Modal } from "./Modal";

const ADMIN_WAIT = isWindows
  ? "Waiting for Windows — accept the administrator prompt…"
  : "Waiting for the system — enter your password if asked…";

/** Lists this PC's adapters; puts one on a board's network and back. */
export function NetConfigDialog({ focusIp, onClose, onChanged }: { focusIp?: string; onClose: () => void; onChanged: () => void }) {
  const [adapters, setAdapters] = useState<Adapter[] | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);
  /** Per adapter: DHCP from a board gives no default route or DNS (on unless unticked). */
  const [noRoute, setNoRoute] = useState<Record<string, boolean>>({});

  const load = useCallback(async () => {
    const list = await listAdapters().catch(() => []);
    pruneApplied(list);
    setAdapters(list);
    return list;
  }, []);

  useEffect(() => {
    void load().then((list) => {
      const hit = focusIp && list.find((a) => a.addresses.some((x) => x.ip === focusIp));
      if (hit) setEditing(hit.id);
    });
  }, [load, focusIp]);

  const run = async (a: Adapter, what: string, fn: () => Promise<Adapter>) => {
    setError(null);
    setDone(null);
    setBusy(a.id);
    try {
      const after = await fn();
      const ips = after.addresses.map((x) => `${x.ip}/${x.prefix}`).join(", ");
      setDone(`${a.name}: ${what} — ${ips ? `now ${ips}` : "waiting for an address from DHCP…"}`);
      setEditing(null);
      // A DHCP lease can take a few seconds: look again.
      for (const ms of [3000, 8000]) {
        window.setTimeout(() => {
          void load().then((list) => {
            const now = list.find((x) => x.id === a.id);
            const got = now?.addresses.map((x) => `${x.ip}/${x.prefix}`).join(", ");
            if (got) setDone(`${a.name}: ${what} — now ${got}`);
          });
          onChanged();
        }, ms);
      }
    } catch (e) {
      setError(`${a.name}: ${String(e)}`);
    } finally {
      setBusy(null);
      await load();
      onChanged();
    }
  };

  const applied = loadApplied();

  return (
    <Modal title="PC network adapters" width={640} onClose={busy ? () => {} : onClose}>
      <p className="hint">
        Give the adapter cabled to the board an address on the board's network, and go back to DHCP when you are done.{" "}
        {isWindows
          ? "Changing an adapter needs administrator rights: Windows asks each time."
          : "The system may ask for your password."}
      </p>
      {adapters === null && <p className="muted">Reading adapters…</p>}
      {adapters?.length === 0 && <p className="muted">No Ethernet or Wi-Fi adapter found.</p>}
      <div className="net-list">
        {adapters?.map((a) => {
          const mine = applied.find((x) => x.id === a.id);
          return (
            <div key={a.id} className={`net-adapter ${editing === a.id ? "open" : ""}`}>
              <div className="net-head">
                <div className="net-names">
                  <span className="net-name">{a.name}</span>
                  {a.description && <span className="muted net-desc">{a.description}</span>}
                </div>
                <span className={`net-mode ${a.method === "manual" ? "static" : ""}`}>{methodLabel(a.method)}</span>
              </div>
              <div className="net-addrs mono">
                {!a.connected && <span className="muted">{a.wireless ? "not connected" : "cable unplugged"}  </span>}
                {a.addresses.length ? a.addresses.map((x) => `${x.ip}/${x.prefix}`).join("  ") : "no address"}
                {a.gateways.length > 0 && <span className="muted">  gateway {a.gateways.join(", ")}</span>}
              </div>
              {mine && (
                <div className="net-mine">
                  Set by TermWorker. Before: {describeSaved(mine.before)}
                </div>
              )}
              {busy === a.id ? (
                <div className="net-busy">{ADMIN_WAIT}</div>
              ) : editing === a.id ? (
                <StaticForm
                  adapter={a}
                  onCancel={() => setEditing(null)}
                  onApply={async (ip, prefix, gateway) => {
                    if (a.gateways.length && !gateway) {
                      const ok = await ask(
                        `${a.name} has a gateway (${a.gateways.join(", ")}). If this PC reaches the office network or the internet through it, ` +
                          "that connection stops while the static address is set. Continue?",
                        { title: "Change network adapter", kind: "warning" },
                      );
                      if (!ok) return;
                    }
                    await run(a, `static ${ip}/${prefix}`, () => setStatic(a, ip, prefix, gateway));
                  }}
                />
              ) : (
                <div className="net-actions">
                  <button className="btn small" disabled={busy !== null} onClick={() => setEditing(a.id)}>
                    Set static IP…
                  </button>
                  <label className="net-check" title="The board's DHCP gives this PC no default route and no DNS: office and internet traffic keep their own way">
                    <input
                      type="checkbox"
                      checked={noRoute[a.id] ?? true}
                      onChange={(e) => setNoRoute({ ...noRoute, [a.id]: e.target.checked })}
                    />
                    <span>Board link: no internet route</span>
                  </label>
                  <button
                    className="btn small"
                    disabled={busy !== null}
                    onClick={async () => {
                      const boardLink = noRoute[a.id] ?? true;
                      const uplinks = (adapters ?? []).filter((x) => x.connected && x.gateways.length > 0);
                      if (boardLink && uplinks.length === 1 && uplinks[0].id === a.id) {
                        const ok = await ask(
                          `${a.name} is this PC's only way to the network or internet. With "Board link: no internet route" ` +
                            "the PC will not use it for that. Continue?",
                          { title: "Change network adapter", kind: "warning" },
                        );
                        if (!ok) return;
                      }
                      await run(a, boardLink ? "DHCP (board link)" : "DHCP", () => setDhcp(a, boardLink));
                    }}
                  >
                    Use DHCP
                  </button>
                  {mine && mine.before.method !== "dhcp" && (
                    <button className="btn small" disabled={busy !== null} onClick={() => void run(a, "restored", () => restore(a, mine))}>
                      Restore {methodLabel(mine.before.method)}
                    </button>
                  )}
                </div>
              )}
            </div>
          );
        })}
      </div>
      {done && <div className="net-done">{done}</div>}
      {error && <div className="tftp-error">{error}</div>}
    </Modal>
  );
}

function StaticForm({
  adapter,
  onCancel,
  onApply,
}: {
  adapter: Adapter;
  onCancel: () => void;
  onApply: (ip: string, prefix: number, gateway: string | null) => Promise<void>;
}) {
  const current = adapter.addresses[0];
  const [board, setBoard] = useState("");
  const [ip, setIp] = useState(adapter.dhcp === false && current ? current.ip : "");
  const [prefix, setPrefix] = useState(current && adapter.dhcp === false ? current.prefix : 24);
  const [gateway, setGateway] = useState("");
  const suggestion = suggestFor(board);

  return (
    <form
      className="net-form"
      onSubmit={(e) => {
        e.preventDefault();
        void onApply(ip.trim(), prefix, gateway.trim() || null);
      }}
    >
      <label>
        <span>Board's IP</span>
        <input
          className="input mono"
          placeholder="optional, e.g. 192.168.1.1"
          value={board}
          onChange={(e) => {
            setBoard(e.target.value);
            const s = suggestFor(e.target.value);
            if (s) {
              setIp(s);
              setPrefix(24);
            }
          }}
        />
        <span className="menu-hint">{suggestion ? `PC gets ${suggestion}/24` : "fills in the PC address below"}</span>
      </label>
      <label>
        <span>PC address</span>
        <input className="input mono" required placeholder="192.168.1.100" value={ip} onChange={(e) => setIp(e.target.value)} />
        <select className="input" value={prefix} onChange={(e) => setPrefix(Number(e.target.value))}>
          {PREFIXES.map((p) => (
            <option key={p.prefix} value={p.prefix}>
              {p.label}
            </option>
          ))}
          {!PREFIXES.some((p) => p.prefix === prefix) && <option value={prefix}>/{prefix}</option>}
        </select>
      </label>
      <label>
        <span>Gateway</span>
        <input
          className="input mono"
          placeholder="none"
          value={gateway}
          onChange={(e) => setGateway(e.target.value)}
        />
        <span className="menu-hint">leave empty for a board link</span>
      </label>
      <div className="net-actions">
        <button type="button" className="btn small" onClick={onCancel}>
          Cancel
        </button>
        <button type="submit" className="btn small primary" disabled={!ip.trim()}>
          Apply
        </button>
      </div>
    </form>
  );
}
