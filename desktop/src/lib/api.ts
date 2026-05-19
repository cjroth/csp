// The single engine boundary: every call is a Tauri command into the real
// native csp-core backend. No mocks, no dual mode.

import type {
  AggregateStatus,
  AppSettings,
  AuthorizedKey,
  ConnectAddress,
  EngineApi,
  EngineEvent,
  Identity,
  ListenerInfo,
  Snapshot,
  Vault,
  VaultStatus,
} from "@/lib/api.types";
import { tauriInvoke, tauriListen } from "@/lib/tauri";

/** The app only ships inside the Tauri shell now (real csp-core). */
export const runningUnderTauri = true;

const ENGINE_EVENT = "engine://event";

export const api: EngineApi = {
  listVaults: () => tauriInvoke<Vault[]>("list_vaults"),
  addLocalFolder: (path) => tauriInvoke<Vault>("add_local_folder", { path }),
  cloneRemote: (dest, url) => tauriInvoke<Vault>("clone_remote", { dest, url }),
  removeVault: (id) => tauriInvoke<void>("remove_vault", { id }),
  setEnabled: (id, on) => tauriInvoke<void>("set_enabled", { id, on }),
  setAllowConnections: (id, on) => tauriInvoke<ListenerInfo>("set_allow_connections", { id, on }),
  getConnectAddress: (id) => tauriInvoke<ConnectAddress>("get_connect_address", { id }),
  listAuthorized: (id) => tauriInvoke<AuthorizedKey[]>("list_authorized", { id }),
  authorize: (id, pubkey) => tauriInvoke<void>("authorize", { id, pubkey }),
  revoke: (id, fingerprint) => tauriInvoke<void>("revoke", { id, fingerprint }),
  getIdentity: () => tauriInvoke<Identity>("get_identity"),
  getSettings: () => tauriInvoke<AppSettings>("get_settings"),
  setSettings: (settings) => tauriInvoke<AppSettings>("set_settings", { settings }),
  createSnapshot: (id, name) => tauriInvoke<Snapshot>("create_snapshot", { id, name }),
  listSnapshots: (id) => tauriInvoke<Snapshot[]>("list_snapshots", { id }),
  restore: (id, target) => tauriInvoke<void>("restore", { id, target }),
  getStatus: (id) => tauriInvoke<VaultStatus>("get_status", { id }),
  getAggregateStatus: () => tauriInvoke<AggregateStatus>("get_aggregate_status"),
  refreshTray: () => tauriInvoke<void>("refresh_tray"),
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
  },
};
