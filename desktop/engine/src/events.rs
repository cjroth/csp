//! The engine event stream — only events csp-core can truthfully produce.

use serde::{Deserialize, Serialize};

use crate::types::{SyncState, VaultId};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EngineEvent {
    /// Per-vault status changed — UI re-fetches `get_status(id)`.
    StatusTick { id: VaultId },
    /// Aggregate (tray) state changed.
    AggregateTick { state: SyncState },
    /// The tracked-folder set changed — UI re-fetches `list_vaults()`.
    VaultsChanged,
    /// A local commit was produced & published.
    Committed { id: VaultId, short_sha: String },
    /// A non-fatal error worth surfacing.
    Error { id: Option<VaultId>, message: String },
}
