//! Context Desktop — Tauri v2 shell.
//!
//! Contributes only: process lifecycle, the tray, window glue, and the
//! command/event bridge. Every sync/merge/identity/auth/history behavior is
//! a call into the engine (spec §2 HARD INVARIANT — no protocol logic here).

mod commands;
mod state;
mod tray;
mod window;

use context_desktop_engine::Engine;
use tauri::{Emitter, Manager};

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            let handle = app.handle().clone();

            // Build the REAL engine over native csp-core. App config lives
            // in the OS app-config dir (spec §7), never in any .context/.
            let cfg_dir = app
                .path()
                .app_config_dir()
                .expect("resolve app config dir");
            let state = tauri::async_runtime::block_on(AppState::new(cfg_dir))
                .expect("initialise csp engine");
            app.manage(state);
            let engine = app.state::<AppState>().engine.clone();

            // Build the native tray menu and wire click handling (spec §6.1).
            tray::refresh(&handle);
            tray::install_handlers(&handle);

            // Forward the engine event stream to the webview. The app is a
            // pure projection of these events (spec §13).
            let mut rx = engine.subscribe();
            let ev_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(event) => {
                            let _ = ev_handle.emit("engine://event", event);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });

            Ok(())
        })
        .on_window_event(|win, event| {
            // Close = hide to tray; the process keeps syncing (spec §1/§5).
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if win.label() == window::MAIN {
                    api.prevent_close();
                    let _ = win.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_vaults,
            commands::add_local_folder,
            commands::clone_remote,
            commands::remove_vault,
            commands::set_enabled,
            commands::set_allow_connections,
            commands::get_connect_address,
            commands::list_authorized,
            commands::authorize,
            commands::revoke,
            commands::get_identity,
            commands::get_settings,
            commands::set_settings,
            commands::create_snapshot,
            commands::list_snapshots,
            commands::restore,
            commands::get_status,
            commands::get_aggregate_status,
            commands::refresh_tray,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Context Desktop")
        .run(|app, event| match event {
            // Closing/hiding the last window must NOT quit — only the tray
            // "Quit" item does (spec §1/§5: tray-only is a valid state).
            tauri::RunEvent::ExitRequested { api, .. } => {
                api.prevent_exit();
            }
            // Dock icon click with no visible window → bring it back.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => {
                window::show_and_focus(app);
            }
            _ => {
                let _ = app;
            }
        });
}
