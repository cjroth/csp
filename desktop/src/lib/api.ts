// The single engine boundary. Auto-detects Tauri vs plain browser:
//   - under Tauri  → `invoke()` the Rust command bridge + `listen()` events
//   - in a browser → the in-TS mock (identical shapes & event cadence)
// Nothing else in the app talks to the engine directly.

import type {
  AggregateStatus,
  AppSettings,
  AuthorizedKey,
  CloneOutcome,
  ConnectAddress,
  EngineApi,
  EngineEvent,
  Identity,
  IdentitySource,
  ListenerInfo,
  RestoreTarget,
  Snapshot,
  Vault,
  VaultId,
  VaultStatus,
} from "@/lib/api.types";
import { mockEngine } from "@/lib/mock/stub-engine";
import { isTauri, tauriInvoke, tauriListen } from "@/lib/tauri";

const ENGINE_EVENT = "engine://event";

class TauriEngine implements EngineApi {
  listVaults = () => tauriInvoke<Vault[]>("list_vaults");
  addLocalFolder = (path: string) => tauriInvoke<Vault>("add_local_folder", { path });
  cloneRemote = (dest: string, url: string) =>
    tauriInvoke<CloneOutcome>("clone_remote", { dest, url });
  removeVault = (id: VaultId) => tauriInvoke<void>("remove_vault", { id });
  setEnabled = (id: VaultId, on: boolean) => tauriInvoke<void>("set_enabled", { id, on });
  setAllowConnections = (id: VaultId, on: boolean) =>
    tauriInvoke<ListenerInfo>("set_allow_connections", { id, on });
  getConnectAddress = (id: VaultId) => tauriInvoke<ConnectAddress>("get_connect_address", { id });
  listAuthorized = (id: VaultId) => tauriInvoke<AuthorizedKey[]>("list_authorized", { id });
  authorize = (id: VaultId, pubkey: string) => tauriInvoke<void>("authorize", { id, pubkey });
  revoke = (id: VaultId, fingerprint: string) => tauriInvoke<void>("revoke", { id, fingerprint });
  respondTofu = (requestId: string, allow: boolean) =>
    tauriInvoke<void>("respond_tofu", { request_id: requestId, allow });
  getIdentity = () => tauriInvoke<Identity>("get_identity");
  setIdentitySource = (src: IdentitySource) =>
    tauriInvoke<Identity>("set_identity_source", { src });
  getSettings = () => tauriInvoke<AppSettings>("get_settings");
  setSettings = (settings: AppSettings) => tauriInvoke<AppSettings>("set_settings", { settings });
  createSnapshot = (id: VaultId, name: string) =>
    tauriInvoke<Snapshot>("create_snapshot", { id, name });
  listSnapshots = (id: VaultId) => tauriInvoke<Snapshot[]>("list_snapshots", { id });
  restore = (id: VaultId, target: RestoreTarget) => tauriInvoke<void>("restore", { id, target });
  getStatus = (id: VaultId) => tauriInvoke<VaultStatus>("get_status", { id });
  getAggregateStatus = () => tauriInvoke<AggregateStatus>("get_aggregate_status");
  devTriggerTofu = () => tauriInvoke<void>("dev_trigger_tofu");
  devTriggerSuperseded = () => tauriInvoke<void>("dev_trigger_superseded");
  refreshTray = () => tauriInvoke<void>("refresh_tray");

  subscribe(cb: (e: EngineEvent) => void) {
    let unlisten: (() => void) | null = null;
    let stopped = false;
    void tauriListen<EngineEvent>(ENGINE_EVENT, cb).then((u) => {
      if (stopped) u();
      else unlisten = u;
    });
    return () => {
      stopped = true;
      unlisten?.();
    };
  }
}

export const api: EngineApi = isTauri() ? new TauriEngine() : mockEngine();

export const runningUnderTauri = isTauri();
