//! App-level config location (spec §7).
//!
//! Tracked folders + per-folder prefs + global settings live in the OS
//! app-config dir, **never** inside any vault's `.context/` (spec §11 HARD
//! INVARIANT) and never inside a synced scope. In the v1 stub the engine
//! holds this in memory; this module only resolves where the durable file
//! will live so the persistence seam is explicit.

use std::path::PathBuf;

use tauri::{AppHandle, Manager};

/// `<os-app-config>/com.cjroth.context-desktop/config.json`.
/// Returned for the persistence seam; not yet written by the stub.
pub fn config_file_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("config.json"))
}
