//! Shared app state. The app is a controller over the real engine, not a
//! data path (spec §12) — it holds only an `Arc` to `CspEngine`.

use std::path::PathBuf;
use std::sync::Arc;

use context_desktop_engine::CspEngine;

pub struct AppState {
    pub engine: Arc<CspEngine>,
}

impl AppState {
    /// Build the real engine over native csp-core. `app_config_dir` is the
    /// OS app-config directory (spec §7) — tracked folders + settings live
    /// there, never inside any vault's `.context/`.
    pub async fn new(app_config_dir: PathBuf) -> Result<Self, String> {
        let engine = CspEngine::new(app_config_dir, None)
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self { engine })
    }
}
