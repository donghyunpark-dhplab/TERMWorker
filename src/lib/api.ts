// Thin typed wrappers around the Tauri commands in src-tauri/src/commands.rs.

import { Channel, invoke } from "@tauri-apps/api/core";
import type {
  Connection,
  HdlcConfig,
  HostKeyDecision,
  LogMode,
  PortInfo,
  Profile,
  SessionEvent,
  SignalName,
  TerminalOptions,
  TransferRequest,
} from "./types";

export interface ProfilesFile {
  version: number;
  sessions: Profile[];
}

export const api = {
  listSerialPorts: () => invoke<PortInfo[]>("list_serial_ports"),

  openSession(
    tab: string,
    connection: Connection,
    options: TerminalOptions,
    secret: string | null,
    cols: number,
    rows: number,
    onEvent: (ev: SessionEvent) => void,
  ): Promise<number> {
    const channel = new Channel<SessionEvent>();
    channel.onmessage = onEvent;
    return invoke<number>("session_open", { tab, connection, options, secret, cols, rows, onEvent: channel });
  },

  write: (id: number, data: string, binary = false) => invoke<void>("session_write", { id, data, binary }),
  resize: (id: number, cols: number, rows: number) => invoke<void>("session_resize", { id, cols, rows }),
  close: (id: number) => invoke<void>("session_close", { id }),
  tabClosed: (tab: string) => invoke<void>("tab_closed", { tab }),
  setSignal: (id: number, signal: SignalName, value: boolean) =>
    invoke<void>("session_set_signal", { id, signal, value }),
  sendBreak: (id: number) => invoke<void>("session_send_break", { id }),
  setEncoding: (id: number, encoding: string) => invoke<string>("session_set_encoding", { id, encoding }),
  answerHostKey: (id: number, decision: HostKeyDecision) =>
    invoke<void>("session_hostkey_answer", { id, decision }),

  logStart: (id: number, path: string, mode: LogMode) => invoke<string>("log_start", { id, path, mode }),
  logStop: (id: number) => invoke<void>("log_stop", { id }),
  defaultLogPath: (name: string) => invoke<string>("default_log_path", { name }),

  profilesLoad: () => invoke<ProfilesFile>("profiles_load"),
  profilesSave: (data: ProfilesFile) => invoke<void>("profiles_save", { data }),
  appInfo: () => invoke<{ version: string; configDir: string }>("app_info"),

  transferStart: (id: number, request: TransferRequest) => invoke<void>("transfer_start", { id, request }),
  transferCancel: (id: number) => invoke<void>("transfer_cancel", { id }),
  zmodemDecline: (id: number) => invoke<void>("zmodem_decline", { id }),
  defaultDownloadDir: () => invoke<string>("default_download_dir"),

  hdlcSet: (id: number, config: HdlcConfig | null) => invoke<void>("hdlc_set", { id, config }),
  hdlcSend: (id: number, payload: number[]) => invoke<void>("hdlc_send", { id, payload }),
  hdlcSendFile: (id: number, path: string, chunk: number, gapMs: number) =>
    invoke<void>("hdlc_send_file", { id, path, chunk, gapMs }),
  hdlcCancelSend: (id: number) => invoke<void>("hdlc_cancel_send", { id }),
  hdlcRecord: (id: number, path: string | null) => invoke<void>("hdlc_record", { id, path }),

  scriptRun: (path: string, tab: string | null, tabName: string | null) =>
    invoke<number>("script_run", { path, tab, tabName }),
  scriptStop: (run: number) => invoke<void>("script_stop", { run }),
  scriptReply: (run: number, id: number, ok: boolean, value: unknown) =>
    invoke<void>("script_reply", { run, id, ok, value: value ?? null }),
};
