mod codec;
mod commands;
mod config;
mod hdlc;
mod logger;
mod netcfg;
mod profiles;
mod script;
mod session;
mod tftp;
mod transfer;
mod transport;

use std::sync::Arc;
use tauri::{Emitter, Manager};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            std::fs::create_dir_all(&config_dir)?;
            app.manage(commands::AppState::new(config_dir));
            let handle = app.handle().clone();
            app.manage(tftp::TftpServer::new(Arc::new(move |ev| {
                let _ = handle.emit("tftp:event", ev);
            })));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_serial_ports,
            commands::session_open,
            commands::session_write,
            commands::session_resize,
            commands::session_close,
            commands::session_set_signal,
            commands::session_send_break,
            commands::session_set_encoding,
            commands::session_hostkey_answer,
            commands::log_start,
            commands::log_stop,
            commands::default_log_path,
            commands::profiles_load,
            commands::profiles_save,
            commands::app_info,
            commands::transfer_start,
            commands::transfer_cancel,
            commands::hdlc_set,
            commands::hdlc_send,
            commands::hdlc_send_file,
            commands::hdlc_cancel_send,
            commands::hdlc_record,
            commands::zmodem_decline,
            commands::default_download_dir,
            commands::tab_closed,
            script::script_run,
            script::script_stop,
            script::script_reply,
            tftp::commands::tftp_snapshot,
            tftp::commands::tftp_start,
            tftp::commands::tftp_stop,
            tftp::commands::tftp_addresses,
            tftp::commands::tftp_default_folder,
            tftp::commands::tftp_list_files,
            netcfg::commands::netcfg_adapters,
            netcfg::commands::netcfg_set_static,
            netcfg::commands::netcfg_set_dhcp,
            netcfg::commands::netcfg_restore,
        ])
        .run(tauri::generate_context!())
        .expect("error while running TermWorker");
}
