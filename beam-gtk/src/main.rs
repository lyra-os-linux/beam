#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clipboard;
mod desktop;
mod i18n;
mod manager;
mod session;

use tauri::{Manager, WindowEvent};

const APP_ID: &str = "org.lyraos.Beam";

fn main() {
    i18n::init();
    let filter = tracing_subscriber::EnvFilter::builder()
        .with_env_var("BEAM_LOG")
        .with_default_directive(tracing::level_filters::LevelFilter::WARN.into())
        .from_env_lossy();
    tracing_subscriber::fmt().with_env_filter(filter).init();

    // Cada sessão RDP roda neste runtime próprio, numa thread separada; a
    // interface só troca mensagens com ele pelos canais do beam-core.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to start the tokio runtime");
    let runtime_handle = runtime.handle().clone();
    std::thread::Builder::new()
        .name("beam-tokio".to_owned())
        .spawn(move || runtime.block_on(std::future::pending::<()>()))
        .expect("failed to spawn the tokio runtime thread");

    // O prgname mantém o app_id/WM_CLASS que liga as janelas ao org.lyraos.Beam.desktop.
    glib::set_prgname(Some(APP_ID));
    glib::set_application_name("Beam");

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .manage(manager::Profiles::default())
        .manage(session::Sessions::new(runtime_handle))
        .setup(|app| {
            desktop::watch_interface(app.handle().clone());
            clipboard::watch(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::Destroyed = event {
                if window.label().starts_with("session-") {
                    let app = window.app_handle();
                    app.state::<session::Sessions>()
                        .window_closed(app, window.label());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            desktop::app_info,
            desktop::app_log,
            desktop::interface_style,
            i18n::i18n_catalog,
            manager::profiles_list,
            manager::profile_save,
            manager::profile_duplicate,
            manager::profile_delete,
            manager::trusted_list,
            manager::trusted_remove,
            session::connect,
            session::sessions_state,
            session::session_open,
            session::session_frame_ack,
            session::session_input,
            session::session_ctrl_alt_del,
            session::session_disconnect,
            session::session_reconnect,
            session::session_answer_cert,
            session::session_answer_creds,
            session::dev::dev_autoconnect,
            session::dev::dev_snapshot_path,
            session::dev::dev_snapshot,
            session::dev::dev_dump_framebuffer,
            session::dev::dev_type_text,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start Beam");
}
