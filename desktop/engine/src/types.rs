//! Wire types for the engine embedding contract.
//!
//! These describe ONLY what the native `csp-core` engine can truthfully
//! report. Fields the engine cannot observe (live peer list, sync progress,
//! superseded edits) are intentionally absent — the app surfaces engine
//! truth, it does not fabricate (spec §6.6/§12).

use serde::{Deserialize, Serialize};

pub type VaultId = String;

/// Coarse, truthful per-vault state. csp-core exposes no peer/connection
/// registry, so finer states (e.g. live "syncing") are not derivable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncState {
    /// Sync toggled off (no watch loop running).
    Disabled,
    /// Sync on, but no `main` computed yet (fresh / empty).
    Idle,
    /// Sync on and a `main` fold-commit exists.
    Active,
    /// The vault failed to open/serve; see the error event.
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Vault {
    pub id: VaultId,
    pub display_name: String,
    pub path: String,
    pub enabled: bool,
    pub allow_connections: bool,
    /// Listener port (0 = OS-assigned until first bind, then pinned).
    pub port: u16,
    /// Did `<path>/.context` already exist when added (attach vs init)?
    pub is_csp_vault: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListenerInfo {
    pub bound: bool,
    pub scheme: String, // "ws" | "wss"
    pub port: u16,
    pub address: String,
}

/// Connect block (spec §8). Firewall guidance + the empty-authorized note
/// are engine-sourced so the copy stays single-sourced and truthful.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectAddress {
    pub scheme: String,
    pub lan_ip: String,
    pub port: u16,
    pub address: String,
    pub firewall_guidance: String,
    /// True when the authorized set is empty. With `no_tofu=true` (our
    /// default) that means NO peer can connect until you authorize a key —
    /// not "trusts whoever connects first".
    pub no_authorized_keys: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizedKey {
    /// OpenSSH-style `SHA256:<base64>` fingerprint of the key blob.
    pub fingerprint: String,
    pub openssh: String,
    pub comment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub openssh: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub start_at_login: bool,
    pub log_level: String,
    /// Default for a folder's first listener bind.
    pub listen_by_default: bool,
    /// Plaintext `ws://` instead of self-signed `wss://` (proxy-terminated).
    pub no_tls_by_default: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            start_at_login: true,
            log_level: "info".into(),
            listen_by_default: false,
            no_tls_by_default: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub name: String,
    pub created_at: String,
    pub frontier: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RestoreTarget {
    /// Exact and skew-free (spec §8).
    Named { name: String },
    /// Best-effort under clock skew (spec §8) — UI shows the caveat.
    Time { rfc3339: String },
}

/// Read-only projection of vault state — only what csp-core exposes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultStatus {
    pub id: VaultId,
    pub state: SyncState,
    pub main_short_sha: Option<String>,
    pub known_count: u32,
    pub frontier_count: u32,
    pub authorized_count: u32,
    pub listener: Option<ListenerInfo>,
    pub configured_peers: Vec<String>,
    pub last_commit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregateStatus {
    pub state: SyncState,
    pub vault_count: u32,
    pub active_count: u32,
    pub error_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    NotFound,
    Io,
    Engine,
    Network,
    Unsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineError {
    pub kind: ErrorKind,
    pub message: String,
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}
impl std::error::Error for EngineError {}

impl EngineError {
    pub fn not_found(m: impl Into<String>) -> Self {
        Self { kind: ErrorKind::NotFound, message: m.into() }
    }
    pub fn io(m: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Io, message: m.into() }
    }
    pub fn engine(m: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Engine, message: m.into() }
    }
    pub fn network(m: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Network, message: m.into() }
    }
    pub fn unsupported(m: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Unsupported, message: m.into() }
    }
}

impl From<csp_core::CspError> for EngineError {
    fn from(e: csp_core::CspError) -> Self {
        EngineError::engine(e.to_string())
    }
}
impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        EngineError::io(e.to_string())
    }
}

pub type EngineResult<T> = Result<T, EngineError>;
