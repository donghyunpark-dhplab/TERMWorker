import { useEffect, useState, useSyncExternalStore } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";

import { isWindows, tftp, type TftpTransfer } from "../lib/tftp";
import { formatBytes } from "../lib/transfer";
import type { TermSession } from "../terminal/TermSession";
import { loadApplied, methodLabel } from "../lib/netcfg";
import { IconClose } from "./Icons";
import { NetConfigDialog } from "./NetConfigDialog";

function useTftp() {
  useSyncExternalStore(tftp.subscribe, tftp.getSnapshot);
  return tftp;
}

/** Status bar button: shows whether the server runs and how many transfers are going. */
export function TftpButton() {
  const t = useTftp();
  useEffect(() => void t.init(), [t]);
  const running = t.status.running;
  const active = t.activeCount;
  return (
    <button
      className={`status-btn ${running ? "tftp-on" : ""}`}
      title={
        running
          ? `TFTP server on port ${t.status.port} serving ${t.status.config?.folder ?? ""}`
          : "Built-in TFTP server: let devices fetch files from this PC"
      }
      onClick={() => t.setPanel(!t.panelOpen)}
    >
      {running && <span className="tftp-dot" />}
      TFTP{active > 0 && <span className="muted"> · {active}</span>}
    </button>
  );
}

const clock = (ms: number) => {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
};

function rate(t: TftpTransfer): string {
  if (t.elapsedMs < 200 || t.bytes === 0) return "";
  return `${formatBytes(Math.round((t.bytes * 1000) / t.elapsedMs))}/s`;
}

const noSession = () => () => {};
const noSnapshot = () => 0;

/** Docked above the status bar, like the script output. */
export function TftpPanel({ active }: { active: TermSession | null }) {
  const t = useTftp();
  // The "→ tab" buttons follow the tab's connection state.
  useSyncExternalStore(active?.subscribe ?? noSession, active?.getSnapshot ?? noSnapshot);
  const [copied, setCopied] = useState<string | null>(null);
  /** The adapter dialog, opened for the adapter with this address (or none). */
  const [net, setNet] = useState<{ ip?: string } | null>(null);
  if (!t.panelOpen) return null;
  const applied = loadApplied();

  const running = t.status.running;
  const s = t.settings;
  const folder = t.status.config?.folder ?? s.folder;
  const lost = t.lostAddresses;

  const chooseFolder = async () => {
    const picked = await open({ title: "Folder to share over TFTP", directory: true, defaultPath: s.folder || undefined });
    if (typeof picked === "string") {
      t.update({ folder: picked });
      if (running) await t.start();
    }
  };

  const copy = async (text: string) => {
    await writeText(text).catch(() => {});
    setCopied(text);
    window.setTimeout(() => setCopied((c) => (c === text ? null : c)), 1500);
  };

  return (
    <section className="tftp-panel" onMouseDown={(e) => e.stopPropagation()}>
      <header className="script-head">
        <span className="script-title">TFTP server</span>
        <span className={`tftp-state ${running ? "on" : t.error ? "failed" : ""}`}>
          {running ? `Running on port ${t.status.port}` : t.error ? "Could not start — see below" : "Stopped"}
        </span>
        {running ? (
          <button className="btn small" onClick={() => void t.stop()}>
            Stop
          </button>
        ) : (
          <button className="btn small primary" disabled={!s.folder} onClick={() => void t.start()}>
            Start
          </button>
        )}
        <span className="grow" />
        <button className="btn small" disabled={!t.transfers.some((x) => x.state !== "running")} onClick={() => t.clearFinished()}>
          Clear list
        </button>
        <button className="icon-btn" title="Hide" onClick={() => t.setPanel(false)}>
          <IconClose size={14} />
        </button>
      </header>

      <div className="tftp-body">
        <div className="tftp-settings">
          {t.error && <div className="tftp-error">{t.error}</div>}
          <div className="tftp-row">
            <span className="tftp-label">Folder</span>
            <span className="tftp-folder mono" title={folder}>
              {folder || "Choose a folder"}
            </span>
            <button className="btn small" onClick={() => void chooseFolder()}>
              Change…
            </button>
          </div>
          <div className="tftp-row">
            <span className="tftp-label">Port</span>
            <input
              className="input tftp-port"
              type="number"
              min={1}
              max={65535}
              disabled={running}
              value={s.port}
              onChange={(e) => {
                const port = Number(e.target.value);
                if (Number.isInteger(port) && port > 0 && port < 65536) t.update({ port });
              }}
            />
            <span className="menu-hint">{s.port === 69 ? "standard" : `U-Boot: setenv tftpdstp ${s.port}`}</span>
          </div>
          <label className="tftp-row tftp-check">
            <input
              type="checkbox"
              disabled={running}
              checked={s.allowUpload}
              onChange={(e) => t.update({ allowUpload: e.target.checked })}
            />
            <span>Let devices upload files here (never over an existing file)</span>
          </label>

          <div className="tftp-sub tftp-sub-row">
            <span>This PC's addresses — what the device uses as its server</span>
            <button className="link-btn" onClick={() => setNet({})}>
              Adapters…
            </button>
          </div>
          {lost.length > 0 && (
            <div className="tftp-warn">
              {lost.join(", ")} {lost.length === 1 ? "is" : "are"} gone since the server started (a board reboot can
              drop the PC's DHCP address). Use an address below, or{" "}
              <button className="link-btn" onClick={() => setNet({})}>
                give this PC a fixed address
              </button>
              .
            </div>
          )}
          {applied.map((x) => (
            <div key={x.id} className="tftp-warn">
              {x.name} has the static address {x.ip}/{x.prefix} set by TermWorker.{" "}
              <button className="link-btn" onClick={() => setNet({ ip: x.ip })}>
                Back to {x.before.method === "manual" ? "previous" : methodLabel(x.before.method)}…
              </button>
            </div>
          ))}
          {t.addresses.length === 0 && <div className="muted">No network address. Is the cable plugged in?</div>}
          <div className="tftp-addrs">
            {t.addresses.map((a) => {
              const cmd = t.ubootCommand(a);
              return (
                <div key={`${a.name}-${a.ip}`} className="tftp-addr">
                  <span className="mono tftp-ip">{a.ip}</span>
                  <span className="tftp-if" title={a.name}>
                    {a.name}
                    {a.linkLocal && <span className="muted"> · no DHCP</span>}
                  </span>
                  <span className="grow" />
                  <button className="btn small" title={`Copy ${a.ip}`} onClick={() => void copy(a.ip)}>
                    {copied === a.ip ? "Copied" : "Copy"}
                  </button>
                  <button
                    className="btn small"
                    disabled={!active || active.state !== "connected"}
                    title={active ? `Type "${cmd}" into ${active.title} (you press Enter)` : "Open the device's tab first"}
                    onClick={() => active?.typeText(cmd)}
                  >
                    {a.linkLocal ? "U-Boot → tab" : "serverip → tab"}
                  </button>
                </div>
              );
            })}
          </div>
          {running && (
            <div className="menu-hint tftp-note">
              {isWindows
                ? "If Windows asked about TermWorker and the firewall, allow it — otherwise devices cannot reach this PC."
                : `If devices cannot reach this PC, a firewall may be blocking UDP port ${t.status.port}. ufw: sudo ufw allow ${t.status.port}/udp · firewalld: sudo firewall-cmd --add-port=${t.status.port}/udp`}
            </div>
          )}
        </div>

        <div className="tftp-lists">
          <div className="tftp-sub">
            Transfers <span className="muted">(newest first)</span>
          </div>
          <div className="tftp-transfers">
            {t.transfers.length === 0 && (
              <div className="muted">
                {running ? "Waiting for a device… e.g. U-Boot: tftpboot ${loadaddr} fw.bin" : "Start the server, then fetch from the device."}
              </div>
            )}
            {[...t.transfers].reverse().map((x) => (
              <TransferRow key={x.id} x={x} />
            ))}
          </div>
          <div className="tftp-sub">
            Files in the folder{" "}
            <button className="link-btn" onClick={() => void t.loadFiles()}>
              Refresh
            </button>
          </div>
          <div className="tftp-files mono">
            {t.files.length === 0 && <div className="muted">Empty. Put the image the device should fetch here.</div>}
            {t.files.map((f) => (
              <div key={f.name} className="tftp-file" title="Click to copy the name" onClick={() => void copy(f.name)}>
                <span className="tftp-fname">{copied === f.name ? "Copied" : f.name}</span>
                <span className="muted">{formatBytes(f.size)}</span>
              </div>
            ))}
          </div>
        </div>
      </div>
      {net && <NetConfigDialog focusIp={net.ip} onClose={() => setNet(null)} onChanged={() => void t.refresh()} />}
    </section>
  );
}

function TransferRow({ x }: { x: TftpTransfer }) {
  const pct = x.size ? Math.min(100, Math.round((x.bytes / Math.max(1, x.size)) * 100)) : null;
  return (
    <div className={`tftp-transfer ${x.state}`} title={x.error ?? undefined}>
      <span className="muted mono">{clock(x.started)}</span>
      <span className="mono">{x.peer}</span>
      <span className="tftp-dir">{x.upload ? "→ PC" : "→ device"}</span>
      <span className="tftp-fname mono">{x.file}</span>
      <span className="grow" />
      {x.state === "running" && pct !== null && (
        <span className="tftp-bar">
          <span style={{ width: `${pct}%` }} />
        </span>
      )}
      <span className="mono">
        {formatBytes(x.bytes)}
        {x.size !== null && x.state === "running" ? ` / ${formatBytes(x.size)}` : ""}
      </span>
      <span className="muted mono">{rate(x)}</span>
      <span className="tftp-result">
        {x.state === "done" ? "Done" : x.state === "failed" ? (x.error ?? "Failed") : "…"}
      </span>
    </div>
  );
}
