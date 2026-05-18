//! Wire types for the engine embedding contract.
//!
//! Every struct serializes `camelCase` so the TypeScript `api.types.ts`
//! mirror matches field-for-field. These types describe engine-reported
//! state only; the app never reinterprets a CSP guarantee (spec §6.6).

use serde::{Deserialize, Serialize};

pub type VaultId = String;

/// Aggregate sync state, also the tray glyph projection (spec §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncState {
    /// Up to date, nothing in flight.
    Idle,
    /// Exchanging objects / catching up.
    Syncing,
    /// Converged with at least one peer this session.
    Synced,
    /// No peers reachable (offline-first; engine-reported, not app logic).
    Offline,
    /// Needs the user: auth/listen/error (spec §6.1).
    Attention,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Vault {
    pub id: VaultId,
    pub display_name: String,
    pub path: String,
    pub enabled: bool,
    pub allow_connections: bool,
    pub port: u16,
    /// Did the folder already contain a CSP vault when added (attach vs init)?
    pub is_csp_vault: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BindScope {
    Loopback,
    Lan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListenerInfo {
    pub bound: bool,
    pub port: u16,
    pub bind_scope: BindScope,
    pub tls_expected: bool,
}

/// Connect block contents (spec §8.1/§8.2/§8.4). Firewall guidance and the
/// exposure caveat are engine-sourced so the copy stays single-sourced.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectAddress {
    pub scheme: String, // "wss" | "ws"
    pub lan_ip: String,
    pub port: u16,
    pub address: String,
    pub firewall_guidance: String,
    pub is_non_loopback: bool,
    pub exposure_caveat: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizedKey {
    pub fingerprint: String,
    pub openssh: String,
    pub comment: String,
    pub added_at: String,
}

/// A pending trust-on-first-use decision (spec §8.3 / §10).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TofuRequest {
    pub request_id: String,
    pub vault_id: VaultId,
    pub peer_fingerprint: String,
    pub peer_openssh: String,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum IdentitySource {
    /// Default device-global `~/.context/id_ed25519` (spec §10).
    DeviceGlobal,
    /// Reuse an existing `~/.ssh` key.
    SshKey { path: String },
    /// Delegate signing to the running SSH agent.
    SshAgent,
    /// Per-vault key — explicit opt-in, stronger isolation (spec §10).
    PerVault { vault_id: VaultId },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub openssh: String,
    pub fingerprint: String,
    pub source: IdentitySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortStrategy {
    Auto,
    Fixed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewListenerDefaults {
    pub port_strategy: PortStrategy,
    pub port_range_start: u16,
    pub bind_scope: BindScope,
    pub tofu_enabled: bool,
    pub tls_expected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationToggles {
    pub tofu: bool,
    pub peer_connect: bool,
    pub peer_disconnect: bool,
    pub offline: bool,
    pub sync_error: bool,
    pub superseded_edit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppBehavior {
    pub start_at_login: bool,
    pub log_level: String,
    pub notifications: NotificationToggles,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub new_listener: NewListenerDefaults,
    pub behavior: AppBehavior,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub name: String,
    pub created_at: String,
    pub frontier_shas: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RestoreTarget {
    /// Exact and skew-free (spec §8).
    Named { name: String },
    /// Best-effort; carries CSP's clock-skew warning verbatim (spec §8).
    Time { rfc3339: String, skew_warning: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloneOutcome {
    pub vault: Vault,
    /// Verbatim CSP §5.1 fresh-NodeId warning, when applicable.
    pub node_id_warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerInfo {
    pub fingerprint: String,
    pub address: String,
    pub connected_since: String,
    pub sync_state: SyncState,
}

/// Read-only projection of `ctx status` for one vault (spec §6.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultStatus {
    pub id: VaultId,
    pub state: SyncState,
    pub peers_connected: u32,
    pub main_short_sha: String,
    pub last_activity: Option<String>,
    pub listener: Option<ListenerInfo>,
    pub peers: Vec<PeerInfo>,
    pub pending_tofu: Vec<TofuRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregateStatus {
    pub state: SyncState,
    pub vault_count: u32,
    pub syncing_count: u32,
    pub attention_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    NotFound,
    Conflict,
    PortInUse,
    Io,
    Auth,
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
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self { kind: ErrorKind::NotFound, message: msg.into() }
    }
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Unsupported, message: msg.into() }
    }
}

pub type EngineResult<T> = Result<T, EngineError>;
