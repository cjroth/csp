//! The engine event stream.
//!
//! Mirrors `csp-core`'s structured status/event stream (spec §13). The app
//! is a pure projection of these events — it computes no merge, orders no
//! commits. Serialized as a `type`-tagged union to match the TS mock.

use serde::{Deserialize, Serialize};

use crate::types::{ListenerInfo, Snapshot, SyncState, TofuRequest, VaultId};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EngineEvent {
    /// Per-vault status changed (spec §6.2 `ctx status`).
    StatusTick {
        id: VaultId,
        state: SyncState,
        peers_connected: u32,
        main_short_sha: String,
    },
    /// Aggregate (tray glyph) state changed (spec §6.1).
    AggregateTick { state: SyncState },
    PeerConnected { id: VaultId, peer_fingerprint: String },
    PeerDisconnected { id: VaultId, peer_fingerprint: String },
    /// A peer connected into an empty authorized set (spec §8.3).
    TofuRequested { request: TofuRequest },
    TofuResolved { request_id: String, allowed: bool },
    /// Engine resolved a same-region collision; loser is in history, not the
    /// working tree (spec §9). Informational only — no resolution UI.
    SupersededEdit {
        id: VaultId,
        path: String,
        snapshot_hint: String,
    },
    SnapshotCreated { id: VaultId, snapshot: Snapshot },
    ListenerChanged { id: VaultId, listener: ListenerInfo },
    Error { id: Option<VaultId>, message: String },
}
