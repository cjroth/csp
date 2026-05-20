//! Tauri command bridge. Every command is a thin pass-through to the real
//! `Engine` (csp-core). No protocol/merge/identity logic here (spec §2 HARD
//! INVARIANT). Engine errors serialize straight back to the webview.

use context_desktop_engine::{
    AggregateStatus, AppSettings, AuthorizedKey, ConnectAddress, Engine, EngineError,
    Identity, ListenerInfo, RestoreTarget, Snapshot, Vault, VaultStatus,
};
use tauri::State;

use crate::state::AppState;

type R<T> = Result<T, EngineError>;

#[tauri::command]
pub async fn list_vaults(state: State<'_, AppState>) -> R<Vec<Vault>> {
    state.engine.list_vaults().await
}

#[tauri::command]
pub async fn add_local_folder(state: State<'_, AppState>, path: String) -> R<Vault> {
    state.engine.add_local_folder(path).await
}

#[tauri::command]
pub async fn clone_remote(
    state: State<'_, AppState>,
    dest: String,
    url: String,
    #[allow(non_snake_case)] authKey: Option<String>,
) -> R<Vault> {
    // The webview sends `authKey` (camelCase); Rust receives it as
    // `auth_key` after Tauri's name translation.
    state.engine.clone_remote(dest, url, authKey).await
}

#[tauri::command]
pub async fn remove_vault(state: State<'_, AppState>, id: String) -> R<()> {
    state.engine.remove_vault(id).await
}

#[tauri::command]
pub async fn set_enabled(state: State<'_, AppState>, id: String, on: bool) -> R<()> {
    state.engine.set_enabled(id, on).await
}

#[tauri::command]
pub async fn set_allow_connections(
    state: State<'_, AppState>,
    id: String,
    on: bool,
) -> R<ListenerInfo> {
    state.engine.set_allow_connections(id, on).await
}

#[tauri::command]
pub async fn get_connect_address(
    state: State<'_, AppState>,
    id: String,
) -> R<ConnectAddress> {
    state.engine.get_connect_address(id).await
}

#[tauri::command]
pub async fn list_authorized(
    state: State<'_, AppState>,
    id: String,
) -> R<Vec<AuthorizedKey>> {
    state.engine.list_authorized(id).await
}

#[tauri::command]
pub async fn authorize(state: State<'_, AppState>, id: String, pubkey: String) -> R<()> {
    state.engine.authorize(id, pubkey).await
}

#[tauri::command]
pub async fn revoke(state: State<'_, AppState>, id: String, fingerprint: String) -> R<()> {
    state.engine.revoke(id, fingerprint).await
}

#[tauri::command]
pub async fn get_identity(state: State<'_, AppState>) -> R<Identity> {
    state.engine.get_identity().await
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> R<AppSettings> {
    state.engine.get_settings().await
}

#[tauri::command]
pub async fn set_settings(
    state: State<'_, AppState>,
    settings: AppSettings,
) -> R<AppSettings> {
    state.engine.set_settings(settings).await
}

#[tauri::command]
pub async fn create_snapshot(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> R<Snapshot> {
    state.engine.create_snapshot(id, name).await
}

#[tauri::command]
pub async fn list_snapshots(
    state: State<'_, AppState>,
    id: String,
) -> R<Vec<Snapshot>> {
    state.engine.list_snapshots(id).await
}

#[tauri::command]
pub async fn restore(
    state: State<'_, AppState>,
    id: String,
    target: RestoreTarget,
) -> R<()> {
    state.engine.restore(id, target).await
}

#[tauri::command]
pub async fn get_status(state: State<'_, AppState>, id: String) -> R<VaultStatus> {
    state.engine.get_status(id).await
}

#[tauri::command]
pub async fn get_aggregate_status(state: State<'_, AppState>) -> R<AggregateStatus> {
    state.engine.get_aggregate_status().await
}

/// Rebuild the tray menu from current engine state. The UI calls this after
/// add/clone/remove/toggle so the native menu stays in sync (spec §6.1).
#[tauri::command]
pub fn refresh_tray(app: tauri::AppHandle) {
    crate::tray::refresh(&app);
}
