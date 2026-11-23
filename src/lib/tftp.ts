// The built-in TFTP server (src-tauri/src/tftp/): its settings, state, transfers,
// and this PC's addresses, which change when a board reboots and DHCP goes away.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface TftpConfig {
  folder: string;
  port: number;
  allowUpload: boolean;
  bind?: string | null;
}

export interface TftpStatus {
  running: boolean;
  config: TftpConfig | null;
  port: number | null;
}

export interface TftpTransfer {
  id: number;
  peer: string;
  file: string;
  upload: boolean;
  size: number | null;
  bytes: number;
  blockSize: number;
  state: "running" | "done" | "failed";
  error: string | null;
  started: number;
  elapsedMs: number;
}

export interface LocalAddress {
  name: string;
  ip: string;
  prefix: number;
  linkLocal: boolean;
}

export interface FileEntry {
  name: string;
  size: number;
}

type TftpEvent = { t: "status"; status: TftpStatus } | { t: "transfer"; transfer: TftpTransfer };

const SETTINGS_KEY = "termworker.tftp";
const POLL_MS = 3000;

interface Settings {
  folder: string;
  port: number;
  allowUpload: boolean;
}

function loadSettings(): Settings {
  try {
    const s = JSON.parse(localStorage.getItem(SETTINGS_KEY) ?? "{}");
    return {
      folder: typeof s.folder === "string" ? s.folder : "",
      port: Number.isInteger(s.port) && s.port > 0 && s.port < 65536 ? s.port : 69,
      allowUpload: s.allowUpload === true,
    };
  } catch {
    return { folder: "", port: 69, allowUpload: false };
  }
}

export const isWindows = navigator.userAgent.includes("Windows");

class TftpStore {
  status: TftpStatus = { running: false, config: null, port: null };
  transfers: TftpTransfer[] = [];
  addresses: LocalAddress[] = [];
  files: FileEntry[] = [];
  settings: Settings = loadSettings();
  panelOpen = false;
  error: string | null = null;
  /** Addresses when the server started; a later change is worth a warning. */
  private startedWith: string[] | null = null;

  private listeners = new Set<() => void>();
  private snapshot = 0;
  private started = false;
  private timer: number | null = null;

  subscribe = (fn: () => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };
  getSnapshot = () => this.snapshot;
  private emit() {
    this.snapshot++;
    this.listeners.forEach((fn) => fn());
  }

  async init() {
    if (this.started) return;
    this.started = true;
    void listen<TftpEvent>("tftp:event", (e) => this.onEvent(e.payload));
    if (!this.settings.folder) {
      try {
        this.settings.folder = await invoke<string>("tftp_default_folder");
        this.save();
      } catch {
        // chosen by hand then
      }
    }
    await this.refresh();
  }

  private onEvent(ev: TftpEvent) {
    if (ev.t === "status") {
      this.status = ev.status;
      if (!ev.status.running) this.startedWith = null;
    } else {
      const i = this.transfers.findIndex((t) => t.id === ev.transfer.id);
      if (i >= 0) this.transfers[i] = ev.transfer;
      else this.transfers = [...this.transfers, ev.transfer].slice(-100);
      this.transfers = [...this.transfers];
      if (ev.transfer.upload && ev.transfer.state === "done") void this.loadFiles();
    }
    this.syncPolling();
    this.emit();
  }

  async refresh() {
    try {
      const s = await invoke<{ status: TftpStatus; transfers: TftpTransfer[]; addresses: LocalAddress[] }>("tftp_snapshot");
      this.status = s.status;
      this.transfers = s.transfers;
      this.addresses = s.addresses;
      if (s.status.running && !this.startedWith) this.startedWith = s.addresses.map((a) => a.ip);
    } catch (e) {
      this.error = String(e);
    }
    await this.loadFiles();
    this.syncPolling();
    this.emit();
  }

  async loadFiles() {
    const folder = this.status.config?.folder ?? this.settings.folder;
    if (!folder) return;
    try {
      this.files = await invoke<FileEntry[]>("tftp_list_files", { folder });
    } catch {
      this.files = [];
    }
    this.emit();
  }

  /** Addresses are watched while the panel is open or the server runs. */
  private syncPolling() {
    const want = this.panelOpen || this.status.running;
    if (want && this.timer === null) {
      this.timer = window.setInterval(() => void this.pollAddresses(), POLL_MS);
    } else if (!want && this.timer !== null) {
      window.clearInterval(this.timer);
      this.timer = null;
    }
  }

  private async pollAddresses() {
    try {
      const next = await invoke<LocalAddress[]>("tftp_addresses");
      const key = (l: LocalAddress[]) => l.map((a) => `${a.name}=${a.ip}`).join(",");
      if (key(next) !== key(this.addresses)) {
        this.addresses = next;
        this.emit();
      }
    } catch {
      // keep the last list
    }
  }

  /** IPs the server had when it started that this PC no longer has. */
  get lostAddresses(): string[] {
    if (!this.status.running || !this.startedWith) return [];
    const now = new Set(this.addresses.map((a) => a.ip));
    return this.startedWith.filter((ip) => !now.has(ip));
  }

  get activeCount(): number {
    return this.transfers.filter((t) => t.state === "running").length;
  }

  setPanel(open: boolean) {
    this.panelOpen = open;
    if (open) void this.refresh();
    this.syncPolling();
    this.emit();
  }

  update(patch: Partial<Settings>) {
    this.settings = { ...this.settings, ...patch };
    this.save();
    if (patch.folder) void this.loadFiles();
    this.emit();
  }

  private save() {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify(this.settings));
  }

  async start() {
    this.error = null;
    try {
      const { folder, port, allowUpload } = this.settings;
      this.status = await invoke<TftpStatus>("tftp_start", { config: { folder, port, allowUpload, bind: null } });
      this.addresses = await invoke<LocalAddress[]>("tftp_addresses");
      this.startedWith = this.addresses.map((a) => a.ip);
    } catch (e) {
      this.error = String(e);
    }
    await this.loadFiles();
    this.syncPolling();
    this.emit();
  }

  async stop() {
    this.error = null;
    await invoke("tftp_stop").catch(() => {});
    this.status = { running: false, config: null, port: null };
    this.startedWith = null;
    this.syncPolling();
    this.emit();
  }

  clearFinished() {
    this.transfers = this.transfers.filter((t) => t.state === "running");
    this.emit();
  }

  /**
   * What a U-Boot prompt needs to fetch from this PC at `a`. A 169.254.x.x address
   * (no DHCP) also needs the board on that network: its own address and netmask.
   */
  ubootCommand(a: LocalAddress): string {
    const port = this.status.port ?? this.settings.port;
    const parts: string[] = [];
    if (a.linkLocal) {
      const o = a.ip.split(".").map(Number);
      parts.push(`setenv ipaddr ${o[0]}.${o[1]}.${o[2]}.${o[3] === 10 ? 11 : 10}`, "setenv netmask 255.255.0.0");
    }
    parts.push(`setenv serverip ${a.ip}`);
    if (port !== 69) parts.push(`setenv tftpdstp ${port}`);
    return parts.join("; ");
  }
}

export const tftp = new TftpStore();
