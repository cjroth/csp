//! Shared app state. The app is a controller over the engine, not a data
//! path (spec §12) — it holds only an `Arc` to the engine.

use std::sync::Arc;

use context_desktop_engine::StubEngine;

pub struct AppState {
    /// v1 = `StubEngine`. The real `csp-core`-backed `Engine` slots in here
    /// unchanged for callers (spec §13 embedding contract).
    pub engine: Arc<StubEngine>,
}

impl AppState {
    pub fn new() -> Self {
        Self { engine: Arc::new(StubEngine::new()) }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
