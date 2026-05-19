//! Context Desktop engine — the spec §13 embedding contract, implemented
//! for real over native `csp-core`. No stubs, no mocks.

pub mod api;
pub mod csp;
pub mod events;
pub mod types;

pub use api::Engine;
pub use csp::CspEngine;
pub use events::EngineEvent;
pub use types::*;
