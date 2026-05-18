//! Window lifecycle (spec §1, §5, §6.1).
//!
//! Context Desktop is a normal application window (OrbStack/Docker Desktop
//! model), NOT a menu-bar popover. Closing the window only hides it; the
//! process keeps syncing in the tray. The window is reopened from the tray
//! or the dock.

use tauri::{AppHandle, Manager, WebviewWindow};

pub const MAIN: &str = "main";

pub fn main_window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(MAIN)
}

/// Bring the main window back from the tray / dock.
pub fn show_and_focus(app: &AppHandle) {
    if let Some(w) = main_window(app) {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}
