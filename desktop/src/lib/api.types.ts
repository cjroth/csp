// TypeScript mirror of the REAL engine contract (engine/src/types.rs +
// events.rs). camelCase / `type`-tagged unions exactly as serde emits.
// Only fields native csp-core can truthfully report exist here.

export type VaultId = string;

/** Coarse, truthful state (csp-core exposes no peer/connection registry). */
export type SyncState = "disabled" | "idle" | "active" | "error";

export interface Vault {
  id: VaultId;
  displayName: string;
  path: string;
  enabled: boolean;
  allowConnections: boolean;
  port: number;
  isCspVault: boolean;
}

export interface ListenerInfo {
  bound: boolean;
  scheme: string; // "ws" | "wss"
  port: number;
  address: string;
}

export interface ConnectAddress {
  scheme: string;
  lanIp: string;
  port: number;
  address: string;
  firewallGuidance: string;
  noAuthorizedKeys: boolean;
  note: string | null;
}

export interface AuthorizedKey {
  fingerprint: string;
  openssh: string;
  comment: string;
}

export interface Identity {
  openssh: string;
  fingerprint: string;
}

export interface AppSettings {
  startAtLogin: boolean;
  logLevel: string;
  listenByDefault: boolean;
  noTlsByDefault: boolean;
}

export interface Snapshot {
  name: string;
  createdAt: string;
  frontier: string[];
}

export type RestoreTarget = { kind: "named"; name: string } | { kind: "time"; rfc3339: string };

export interface VaultStatus {
  id: VaultId;
  state: SyncState;
  mainShortSha: string | null;
  knownCount: number;
  frontierCount: number;
  authorizedCount: number;
  listener: ListenerInfo | null;
  configuredPeers: string[];
  lastCommit: string | null;
}

export interface AggregateStatus {
  state: SyncState;
  vaultCount: number;
  activeCount: number;
  errorCount: number;
}

export type EngineEvent =
  | { type: "statusTick"; id: VaultId }
  | { type: "aggregateTick"; state: SyncState }
  | { type: "vaultsChanged" }
  | { type: "committed"; id: VaultId; shortSha: string }
  | { type: "error"; id: VaultId | null; message: string };

/** Methods the UI calls — 1:1 with the Tauri command bridge. */
export interface EngineApi {
  listVaults(): Promise<Vault[]>;
  addLocalFolder(path: string): Promise<Vault>;
  cloneRemote(dest: string, url: string): Promise<Vault>;
  removeVault(id: VaultId): Promise<void>;
  setEnabled(id: VaultId, on: boolean): Promise<void>;
  setAllowConnections(id: VaultId, on: boolean): Promise<ListenerInfo>;
  getConnectAddress(id: VaultId): Promise<ConnectAddress>;
  listAuthorized(id: VaultId): Promise<AuthorizedKey[]>;
  authorize(id: VaultId, pubkey: string): Promise<void>;
  revoke(id: VaultId, fingerprint: string): Promise<void>;
  getIdentity(): Promise<Identity>;
  getSettings(): Promise<AppSettings>;
  setSettings(settings: AppSettings): Promise<AppSettings>;
  createSnapshot(id: VaultId, name: string): Promise<Snapshot>;
  listSnapshots(id: VaultId): Promise<Snapshot[]>;
  restore(id: VaultId, target: RestoreTarget): Promise<void>;
  getStatus(id: VaultId): Promise<VaultStatus>;
  getAggregateStatus(): Promise<AggregateStatus>;
  refreshTray(): Promise<void>;
  subscribe(cb: (e: EngineEvent) => void): () => void;
}
