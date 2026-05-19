//! Menu-bar (tray) icon & menu — spec §6.1.
//!
//! The tray is a NATIVE OS menu + status indicator only; it never renders an
//! in-app popover. All real UI lives in the main window. Menu items either
//! drive the window/engine directly or emit a `tray://…` event the React
//! app handles (e.g. opening a dialog).

use context_desktop_engine::{Engine, SyncState};
use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_opener::OpenerExt;

use crate::state::AppState;
use crate::window;

pub const TRAY_ID: &str = "main-tray";

fn glyph(s: SyncState) -> &'static str {
    match s {
        SyncState::Disabled => "◦",
        SyncState::Idle => "•",
        SyncState::Active => "✓",
        SyncState::Error => "!",
    }
}

/// (Re)build the tray menu from current engine state and attach it.
pub fn refresh(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    let state = app.state::<AppState>();
    let engine = state.engine.clone();

    let vaults = tauri::async_runtime::block_on(engine.list_vaults()).unwrap_or_default();
    let agg = tauri::async_runtime::block_on(engine.get_aggregate_status()).ok();

    let mut mb = MenuBuilder::new(app);

    for v in &vaults {
        let st = tauri::async_runtime::block_on(engine.get_status(v.id.clone())).ok();
        let g = st.map(|s| glyph(s.state)).unwrap_or("•");
        let sub = SubmenuBuilder::new(app, format!("{}  {}", v.display_name, g))
            .item(
                &MenuItemBuilder::with_id(
                    format!("vault:open-fs:{}", v.id),
                    "Open in file manager",
                )
                .build(app)
                .unwrap(),
            )
            .item(
                &MenuItemBuilder::with_id(
                    format!("vault:toggle-sync:{}", v.id),
                    if v.enabled { "Pause sync" } else { "Resume sync" },
                )
                .build(app)
                .unwrap(),
            )
            .item(
                &MenuItemBuilder::with_id(
                    format!("vault:toggle-listen:{}", v.id),
                    if v.allow_connections {
                        "Stop allowing connections"
                    } else {
                        "Allow connections"
                    },
                )
                .build(app)
                .unwrap(),
            )
            .item(
                &MenuItemBuilder::with_id(
                    format!("vault:copy-addr:{}", v.id),
                    "Copy connect address",
                )
                .build(app)
                .unwrap(),
            )
            .item(
                &MenuItemBuilder::with_id(format!("vault:settings:{}", v.id), "Settings")
                    .build(app)
                    .unwrap(),
            )
            .build()
            .unwrap();
        mb = mb.item(&sub);
    }

    let tooltip = match agg.as_ref().map(|a| a.state) {
        Some(SyncState::Active) => "Context Desktop — active",
        Some(SyncState::Error) => "Context Desktop — needs attention",
        Some(SyncState::Idle) => "Context Desktop — idle",
        _ => "Context Desktop",
    };

    let menu = mb
        .separator()
        .item(&MenuItemBuilder::with_id("app:open", "Open main window").build(app).unwrap())
        .item(&MenuItemBuilder::with_id("app:add-local", "Add folder…").build(app).unwrap())
        .item(
            &MenuItemBuilder::with_id("app:connect-remote", "Connect to remote folder…")
                .build(app)
                .unwrap(),
        )
        .separator()
        .item(&MenuItemBuilder::with_id("app:pause-all", "Pause all").build(app).unwrap())
        .item(&MenuItemBuilder::with_id("app:resume-all", "Resume all").build(app).unwrap())
        .separator()
        .item(&MenuItemBuilder::with_id("app:quit", "Quit Context Desktop").build(app).unwrap())
        .build()
        .unwrap();

    let _ = tray.set_menu(Some(menu));
    let _ = tray.set_tooltip(Some(tooltip));
}

/// Wire menu-click handling. Called once at setup.
pub fn install_handlers(app: &AppHandle) {
    let handle = app.clone();
    app.on_menu_event(move |app, event| {
        let id = event.id().as_ref().to_string();
        let state = app.state::<AppState>();
        let engine = state.engine.clone();

        match id.as_str() {
            "app:open" => window::show_and_focus(app),
            "app:add-local" => {
                window::show_and_focus(app);
                let _ = app.emit("tray://add-local", ());
            }
            "app:connect-remote" => {
                window::show_and_focus(app);
                let _ = app.emit("tray://connect-remote", ());
            }
            "app:pause-all" | "app:resume-all" => {
                let on = id == "app:resume-all";
                if let Ok(vaults) = tauri::async_runtime::block_on(engine.list_vaults()) {
                    for v in vaults {
                        let _ = tauri::async_runtime::block_on(
                            engine.set_enabled(v.id, on),
                        );
                    }
                }
                refresh(&handle);
            }
            "app:quit" => app.exit(0),
            other if other.starts_with("vault:") => {
                let mut parts = other.splitn(3, ':');
                let _ = parts.next();
                let action = parts.next().unwrap_or("");
                let vid = parts.next().unwrap_or("").to_string();
                handle_vault_action(app, &handle, action, vid);
                refresh(&handle);
            }
            _ => {}
        }
    });
}

fn handle_vault_action(app: &AppHandle, handle: &AppHandle, action: &str, vid: String) {
    let engine = app.state::<AppState>().engine.clone();
    match action {
        "open-fs" => {
            if let Ok(vaults) = tauri::async_runtime::block_on(engine.list_vaults()) {
                if let Some(v) = vaults.into_iter().find(|v| v.id == vid) {
                    let _ = app.opener().reveal_item_in_dir(v.path);
                }
            }
        }
        "toggle-sync" => {
            if let Ok(vaults) = tauri::async_runtime::block_on(engine.list_vaults()) {
                if let Some(v) = vaults.into_iter().find(|v| v.id == vid) {
                    let _ = tauri::async_runtime::block_on(
                        engine.set_enabled(vid.clone(), !v.enabled),
                    );
                }
            }
        }
        "toggle-listen" => {
            if let Ok(vaults) = tauri::async_runtime::block_on(engine.list_vaults()) {
                if let Some(v) = vaults.into_iter().find(|v| v.id == vid) {
                    let _ = tauri::async_runtime::block_on(
                        engine.set_allow_connections(vid.clone(), !v.allow_connections),
                    );
                }
            }
        }
        "copy-addr" => {
            if let Ok(addr) =
                tauri::async_runtime::block_on(engine.get_connect_address(vid.clone()))
            {
                let _ = app.clipboard().write_text(addr.address);
            }
        }
        "settings" => {
            window::show_and_focus(handle);
            let _ = handle.emit("tray://open-folder", vid);
        }
        _ => {}
    }
}
