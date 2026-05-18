//! Context Desktop engine — the spec §13 "engine embedding contract".
//!
//! `csp-core`'s in-process library API does not exist yet, so this crate
//! provides the trait the app codes against plus a simulated `StubEngine`.
//! When `csp-core` lands, add a `native` module implementing [`Engine`]
//! over it and swap the constructor — no caller changes (spec §2 HARD
//! INVARIANT: no protocol logic in the app).

pub mod api;
pub mod events;
pub mod stub;
pub mod types;

pub use api::Engine;
pub use events::EngineEvent;
pub use stub::{StubEngine, CLONE_NODEID_WARNING};
pub use types::*;
