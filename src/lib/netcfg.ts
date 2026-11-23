// PC network adapters (src-tauri/src/netcfg.rs): put an adapter on a board's
// network with a static address, and back afterwards. What TermWorker changed is
// remembered with the exact setting before, so the panel can remind and undo.

import { invoke } from "@tauri-apps/api/core";

export interface IfAddress {
  ip: string;
  prefix: number;
}

export type Method = "dhcp" | "manual" | "link-local" | "shared" | "disabled" | "none" | "unknown" | string;

export interface Adapter {
  id: string;
  name: string;
  description: string;
  method: Method;
  dhcp: boolean | null;
  /** A cable / link is there. */
  connected: boolean;
  addresses: IfAddress[];
  gateways: string[];
  wireless: boolean;
}

/** How an adapter was set up, to put it back exactly. */
export interface Saved {
  method: Method;
  /** "ip/prefix" */
  addresses: string[];
  gateway: string | null;
  /** Linux: the NetworkManager connection; null when there was none. */
  connection: string | null;
  neverDefault?: boolean | null;
  ignoreAutoDns?: boolean | null;
  /** Windows interface metric: "auto" or a number. */
  metric?: string | null;
}

/** An adapter TermWorker gave a static address, and how it was before. */
export interface Applied {
  id: string;
  name: string;
  ip: string;
  prefix: number;
  before: Saved;
}

export const METHOD_LABEL: Record<string, string> = {
  dhcp: "DHCP",
  manual: "Static",
  "link-local": "Link-local",
  shared: "Shared",
  disabled: "IPv4 off",
  none: "Not set up",
  unknown: "Unknown",
};

export const methodLabel = (m: Method) => METHOD_LABEL[m] ?? m;

/** "static 192.168.1.10/24", "DHCP", "Link-local". */
export function describeSaved(s: Saved): string {
  if (s.method === "manual") return `static ${s.addresses.join(", ") || "(no address)"}`;
  return methodLabel(s.method);
}

const KEY = "termworker.netcfg.applied";

export function loadApplied(): Applied[] {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    // Entries from before the exact "before" was kept have no method: drop them.
    return Array.isArray(v) ? v.filter((x) => x?.before && typeof x.before.method === "string") : [];
  } catch {
    return [];
  }
}

function saveApplied(list: Applied[]) {
  localStorage.setItem(KEY, JSON.stringify(list));
}

const forget = (id: string) => saveApplied(loadApplied().filter((x) => x.id !== id));

export const listAdapters = () => invoke<Adapter[]>("netcfg_adapters");

interface Changed {
  adapter: Adapter;
  before: Saved;
}

export async function setStatic(a: Adapter, ip: string, prefix: number, gateway: string | null): Promise<Adapter> {
  const r = await invoke<Changed>("netcfg_set_static", { id: a.id, ip, prefix, gateway });
  const list = loadApplied();
  // Keep the first "before": that is what the user had.
  const before = list.find((x) => x.id === a.id)?.before ?? r.before;
  saveApplied([...list.filter((x) => x.id !== a.id), { id: a.id, name: a.name, ip, prefix, before }]);
  return r.adapter;
}

/** oardLink: the board gives no default route or DNS to this PC. */
export async function setDhcp(a: Adapter, boardLink: boolean): Promise<Adapter> {
  const r = await invoke<Changed>("netcfg_set_dhcp", { id: a.id, boardLink });
  forget(a.id);
  return r.adapter;
}

/** Back exactly to what the adapter had before TermWorker changed it. */
export async function restore(a: Adapter, applied: Applied): Promise<Adapter> {
  const after = await invoke<Adapter>("netcfg_restore", { id: a.id, saved: applied.before });
  forget(a.id);
  return after;
}

/** Forget adapters that are no longer on a static address (changed outside TermWorker). */
export function pruneApplied(adapters: Adapter[]) {
  const list = loadApplied();
  const next = list.filter((x) => {
    const a = adapters.find((y) => y.id === x.id);
    return !a || a.method === "manual";
  });
  if (next.length !== list.length) saveApplied(next);
}

/** A PC address on the board's /24: .100, or .101 when the board is .100. */
export function suggestFor(board: string): string | null {
  const m = board.trim().match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
  if (!m || m.slice(1).some((x) => Number(x) > 255)) return null;
  const last = Number(m[4]) === 100 ? 101 : 100;
  return `${m[1]}.${m[2]}.${m[3]}.${last}`;
}

export const PREFIXES = [
  { prefix: 24, label: "/24 (255.255.255.0)" },
  { prefix: 16, label: "/16 (255.255.0.0)" },
  { prefix: 8, label: "/8 (255.0.0.0)" },
];
