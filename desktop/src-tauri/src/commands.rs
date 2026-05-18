//! Tauri command bridge. Every command is a thin pass-through to the
//! `Engine` trait — no protocol/merge/identity logic here (spec §2 HARD
//! INVARIANT). Engine errors serialize straight back to the webview.

use context_desktop_engine::{
    AggregateStatus, AppSettings, AuthorizedKey, CloneOutcome, ConnectAddress, Engine,
    EngineError, Identity, IdentitySource, ListenerInfo, RestoreTarget, Snapshot, Vault,
    VaultStatus,
};
use tauri::State;

use crate::state::AppState;

type R<T> = Result<T, EngineError>;

#[tauri::command]
pub async fn list_vaults(state: State<'_, AppState>) -> R<Vec<Vault>> {
    let e = state.engine.clone();
    e.list_vaults().await
}

#[tauri::command]
pub async fn add_local_folder(state: State<'_, AppState>, path: String) -> R<Vault> {
    let e = state.engine.clone();
    e.add_local_folder(path).await
}

#[tauri::command]
pub async fn clone_remote(
    state: State<'_, AppState>,
    dest: String,
    url: String,
) -> R<CloneOutcome> {
    let e = state.engine.clone();
    e.clone_remote(dest, url).await
}

#[tauri::command]
pub async fn remove_vault(state: State<'_, AppState>, id: String) -> R<()> {
    let e = state.engine.clone();
    e.remove_vault(id).await
}

#[tauri::command]
pub async fn set_enabled(state: State<'_, AppState>, id: String, on: bool) -> R<()> {
    let e = state.engine.clone();
    e.set_enabled(id, on).await
}

#[tauri::command]
pub async fn set_allow_connections(
    state: State<'_, AppState>,
    id: String,
    on: bool,
) -> R<ListenerInfo> {
    let e = state.engine.clone();
    e.set_allow_connections(id, on).await
}

#[tauri::command]
pub async fn get_connect_address(
    state: State<'_, AppState>,
    id: String,
) -> R<ConnectAddress> {
    let e = state.engine.clone();
    e.get_connect_address(id).await
}

#[tauri::command]
pub async fn list_authorized(
    state: State<'_, AppState>,
    id: String,
) -> R<Vec<AuthorizedKey>> {
    let e = state.engine.clone();
    e.list_authorized(id).await
}

#[tauri::command]
pub async fn authorize(state: State<'_, AppState>, id: String, pubkey: String) -> R<()> {
    let e = state.engine.clone();
    e.authorize(id, pubkey).await
}

#[tauri::command]
pub async fn revoke(state: State<'_, AppState>, id: String, fingerprint: String) -> R<()> {
    let e = state.engine.clone();
    e.revoke(id, fingerprint).await
}

#[tauri::command]
pub async fn respond_tofu(
    state: State<'_, AppState>,
    request_id: String,
    allow: bool,
) -> R<()> {
    let e = state.engine.clone();
    e.respond_tofu(request_id, allow).await
}

#[tauri::command]
pub async fn get_identity(state: State<'_, AppState>) -> R<Identity> {
    let e = state.engine.clone();
    e.get_identity().await
}

#[tauri::command]
pub async fn set_identity_source(
    state: State<'_, AppState>,
    src: IdentitySource,
) -> R<Identity> {
    let e = state.engine.clone();
    e.set_identity_source(src).await
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> R<AppSettings> {
    let e = state.engine.clone();
    e.get_settings().await
}

#[tauri::command]
pub async fn set_settings(
    state: State<'_, AppState>,
    settings: AppSettings,
) -> R<AppSettings> {
    let e = state.engine.clone();
    e.set_settings(settings).await
}

#[tauri::command]
pub async fn create_snapshot(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> R<Snapshot> {
    let e = state.engine.clone();
    e.create_snapshot(id, name).await
}

#[tauri::command]
pub async fn list_snapshots(
    state: State<'_, AppState>,
    id: String,
) -> R<Vec<Snapshot>> {
    let e = state.engine.clone();
    e.list_snapshots(id).await
}

#[tauri::command]
pub async fn restore(
    state: State<'_, AppState>,
    id: String,
    target: RestoreTarget,
) -> R<()> {
    let e = state.engine.clone();
    e.restore(id, target).await
}

#[tauri::command]
pub async fn get_status(state: State<'_, AppState>, id: String) -> R<VaultStatus> {
    let e = state.engine.clone();
    e.get_status(id).await
}

#[tauri::command]
pub async fn get_aggregate_status(state: State<'_, AppState>) -> R<AggregateStatus> {
    let e = state.engine.clone();
    e.get_aggregate_status().await
}

#[tauri::command]
pub async fn dev_trigger_tofu(state: State<'_, AppState>) -> R<()> {
    let e = state.engine.clone();
    e.dev_trigger_tofu().await
}

#[tauri::command]
pub async fn dev_trigger_superseded(state: State<'_, AppState>) -> R<()> {
    let e = state.engine.clone();
    e.dev_trigger_superseded().await
}

/// Rebuild the tray menu from current engine state. The UI calls this after
/// add/clone/remove/toggle so the native menu stays in sync (spec §6.1).
#[tauri::command]
pub fn refresh_tray(app: tauri::AppHandle) {
    crate::tray::refresh(&app);
}
