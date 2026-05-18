//! The `Engine` trait — the spec §13 "engine embedding contract".
//!
//! This is the single boundary the app talks to. `ctx` and Context Desktop
//! are both thin wrappers over an `Engine`. v1 ships `StubEngine`; the real
//! `csp-core`-backed impl slots in here unchanged for callers.

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::events::EngineEvent;
use crate::types::*;

#[async_trait]
pub trait Engine: Send + Sync + 'static {
    // ---- event stream ----
    /// Subscribe to the live engine event stream (spec §13).
    fn subscribe(&self) -> broadcast::Receiver<EngineEvent>;

    // ---- vault lifecycle ----
    async fn list_vaults(&self) -> EngineResult<Vec<Vault>>;
    /// `ctx init` (new) or attach (existing CSP vault) — spec §6.2/§10.
    async fn add_local_folder(&self, path: String) -> EngineResult<Vault>;
    /// `ctx clone <url>` then start the watch loop — spec §6.2/§5.1.
    async fn clone_remote(&self, dest: String, url: String) -> EngineResult<CloneOutcome>;
    /// Stop + detach; never deletes the folder or `.context/` (spec §6.2).
    async fn remove_vault(&self, id: VaultId) -> EngineResult<()>;
    /// Start/stop the watch loop for this vault (spec §5/§6).
    async fn set_enabled(&self, id: VaultId, on: bool) -> EngineResult<()>;
    /// Bind/unbind this vault's listener (spec §8 / `ctx watch --listen`).
    async fn set_allow_connections(&self, id: VaultId, on: bool) -> EngineResult<ListenerInfo>;
    /// Derived `wss://<lan-ip>:<port>` connect block (spec §8.1).
    async fn get_connect_address(&self, id: VaultId) -> EngineResult<ConnectAddress>;

    // ---- authorization (spec §10) ----
    async fn list_authorized(&self, id: VaultId) -> EngineResult<Vec<AuthorizedKey>>;
    async fn authorize(&self, id: VaultId, pubkey: String) -> EngineResult<()>;
    async fn revoke(&self, id: VaultId, fingerprint: String) -> EngineResult<()>;
    /// Resolve a pending TOFU window (spec §8.3).
    async fn respond_tofu(&self, request_id: String, allow: bool) -> EngineResult<()>;

    // ---- identity (spec §10) ----
    async fn get_identity(&self) -> EngineResult<Identity>;
    async fn set_identity_source(&self, src: IdentitySource) -> EngineResult<Identity>;

    // ---- settings (spec §6.3, app config — never in any `.context/`) ----
    async fn get_settings(&self) -> EngineResult<AppSettings>;
    async fn set_settings(&self, settings: AppSettings) -> EngineResult<AppSettings>;

    // ---- recovery (spec §9) ----
    async fn create_snapshot(&self, id: VaultId, name: String) -> EngineResult<Snapshot>;
    async fn list_snapshots(&self, id: VaultId) -> EngineResult<Vec<Snapshot>>;
    async fn restore(&self, id: VaultId, target: RestoreTarget) -> EngineResult<()>;

    // ---- status (read-only) ----
    async fn get_status(&self, id: VaultId) -> EngineResult<VaultStatus>;
    async fn get_aggregate_status(&self) -> EngineResult<AggregateStatus>;

    // ---- dev-only simulation triggers (stub; no-ops on a real engine) ----
    async fn dev_trigger_tofu(&self) -> EngineResult<()>;
    async fn dev_trigger_superseded(&self) -> EngineResult<()>;
}
