// File-transfer helpers shared by the status bar menu, the ZMODEM prompt and the progress panel.

import { open, save } from "@tauri-apps/plugin-dialog";

import { api } from "./api";
import type { TransferProtocol } from "./types";

export const PROTOCOL_LABEL: Record<TransferProtocol, string> = {
  zmodem: "ZMODEM",
  ymodem: "YMODEM",
  xmodem: "XMODEM",
  xmodem1k: "XMODEM-1K",
};

/** Protocols that carry file names, so the receiver only needs a folder. */
export const isBatch = (p: TransferProtocol) => p === "zmodem" || p === "ymodem";

const DIR_KEY = "termworker.downloadDir";

export async function downloadDir(): Promise<string> {
  return localStorage.getItem(DIR_KEY) ?? (await api.defaultDownloadDir());
}

export function rememberDownloadDir(dir: string) {
  localStorage.setItem(DIR_KEY, dir);
}

export async function pickFilesToSend(protocol: TransferProtocol): Promise<string[] | null> {
  const picked = await open({
    title: `Send with ${PROTOCOL_LABEL[protocol]}`,
    multiple: isBatch(protocol),
    directory: false,
  });
  if (!picked) return null;
  return Array.isArray(picked) ? picked : [picked];
}

/** A folder for ZMODEM/YMODEM, a file name for XMODEM (it carries no name). */
export async function pickReceiveDest(protocol: TransferProtocol): Promise<string | null> {
  const dir = await downloadDir();
  if (isBatch(protocol)) {
    const picked = await open({ title: `Receive with ${PROTOCOL_LABEL[protocol]} into…`, directory: true, defaultPath: dir });
    if (typeof picked !== "string") return null;
    rememberDownloadDir(picked);
    return picked;
  }
  const sep = dir.includes("\\") ? "\\" : "/";
  const picked = await save({ title: `Receive with ${PROTOCOL_LABEL[protocol]} as…`, defaultPath: `${dir}${sep}received.bin` });
  if (!picked) return null;
  const cut = Math.max(picked.lastIndexOf("\\"), picked.lastIndexOf("/"));
  if (cut > 0) rememberDownloadDir(picked.slice(0, cut));
  return picked;
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

export function formatDuration(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s} s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}:${String(s % 60).padStart(2, "0")}`;
  return `${Math.floor(m / 60)}:${String(m % 60).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;
}
