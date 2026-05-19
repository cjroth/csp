//! The `Engine` trait — the spec §13 embedding contract.
//!
//! Every method maps to a real `csp-core` capability. There is no
//! interactive-TOFU / dev-trigger / identity-source surface: csp-core does
//! not expose those, so the contract does not pretend to.

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::events::EngineEvent;
use crate::types::*;

#[async_trait]
pub trait Engine: Send + Sync + 'static {
    fn subscribe(&self) -> broadcast::Receiver<EngineEvent>;

    // ---- vault lifecycle ----
    async fn list_vaults(&self) -> EngineResult<Vec<Vault>>;
    /// Attach an existing CSP vault, or `ctx init` a new one here (§6.2).
    async fn add_local_folder(&self, path: String) -> EngineResult<Vault>;
    /// `ctx clone <url>` then watch — one continuous flow (§6.2).
    async fn clone_remote(&self, dest: String, url: String) -> EngineResult<Vault>;
    /// Stop + detach. Never deletes the folder or `.context/` (§6.2).
    async fn remove_vault(&self, id: VaultId) -> EngineResult<()>;
    async fn set_enabled(&self, id: VaultId, on: bool) -> EngineResult<()>;
    async fn set_allow_connections(&self, id: VaultId, on: bool)
        -> EngineResult<ListenerInfo>;
    async fn get_connect_address(&self, id: VaultId) -> EngineResult<ConnectAddress>;

    // ---- authorization (spec §10) — the real first-trust mechanism ----
    async fn list_authorized(&self, id: VaultId) -> EngineResult<Vec<AuthorizedKey>>;
    async fn authorize(&self, id: VaultId, pubkey: String) -> EngineResult<()>;
    async fn revoke(&self, id: VaultId, fingerprint: String) -> EngineResult<()>;

    // ---- identity (device-global; csp-core has no other source) ----
    async fn get_identity(&self) -> EngineResult<Identity>;

    // ---- settings (app-level, never inside any .context/, spec §7) ----
    async fn get_settings(&self) -> EngineResult<AppSettings>;
    async fn set_settings(&self, settings: AppSettings) -> EngineResult<AppSettings>;

    // ---- recovery (spec §9) ----
    async fn create_snapshot(&self, id: VaultId, name: String) -> EngineResult<Snapshot>;
    async fn list_snapshots(&self, id: VaultId) -> EngineResult<Vec<Snapshot>>;
    async fn restore(&self, id: VaultId, target: RestoreTarget) -> EngineResult<()>;

    // ---- status (read-only) ----
    async fn get_status(&self, id: VaultId) -> EngineResult<VaultStatus>;
    async fn get_aggregate_status(&self) -> EngineResult<AggregateStatus>;
}
