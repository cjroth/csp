// Single TypeScript source of the engine contract. Mirrors
// `engine/src/types.rs` + `events.rs` field-for-field (Rust serializes
// camelCase / `type`-tagged unions). Both the Tauri `invoke` path and the
// browser mock return exactly these shapes.

export type VaultId = string;

export type SyncState = "idle" | "syncing" | "synced" | "offline" | "attention";
export type BindScope = "loopback" | "lan";
export type PortStrategy = "auto" | "fixed";
export type ErrorKind = "notFound" | "conflict" | "portInUse" | "io" | "auth" | "unsupported";

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
  port: number;
  bindScope: BindScope;
  tlsExpected: boolean;
}

export interface ConnectAddress {
  scheme: string;
  lanIp: string;
  port: number;
  address: string;
  firewallGuidance: string;
  isNonLoopback: boolean;
  exposureCaveat: string | null;
}

export interface AuthorizedKey {
  fingerprint: string;
  openssh: string;
  comment: string;
  addedAt: string;
}

export interface TofuRequest {
  requestId: string;
  vaultId: VaultId;
  peerFingerprint: string;
  peerOpenssh: string;
  address: string;
}

export type IdentitySource =
  | { kind: "deviceGlobal" }
  | { kind: "sshKey"; path: string }
  | { kind: "sshAgent" }
  | { kind: "perVault"; vaultId: VaultId };

export interface Identity {
  openssh: string;
  fingerprint: string;
  source: IdentitySource;
}

export interface NewListenerDefaults {
  portStrategy: PortStrategy;
  portRangeStart: number;
  bindScope: BindScope;
  tofuEnabled: boolean;
  tlsExpected: boolean;
}

export interface NotificationToggles {
  tofu: boolean;
  peerConnect: boolean;
  peerDisconnect: boolean;
  offline: boolean;
  syncError: boolean;
  supersededEdit: boolean;
}

export interface AppBehavior {
  startAtLogin: boolean;
  logLevel: string;
  notifications: NotificationToggles;
}

export interface AppSettings {
  newListener: NewListenerDefaults;
  behavior: AppBehavior;
}

export interface Snapshot {
  name: string;
  createdAt: string;
  frontierShas: string[];
}

export type RestoreTarget =
  | { kind: "named"; name: string }
  | { kind: "time"; rfc3339: string; skewWarning: string };

export interface CloneOutcome {
  vault: Vault;
  nodeIdWarning: string | null;
}

export interface PeerInfo {
  fingerprint: string;
  address: string;
  connectedSince: string;
  syncState: SyncState;
}

export interface VaultStatus {
  id: VaultId;
  state: SyncState;
  peersConnected: number;
  mainShortSha: string;
  lastActivity: string | null;
  listener: ListenerInfo | null;
  peers: PeerInfo[];
  pendingTofu: TofuRequest[];
}

export interface AggregateStatus {
  state: SyncState;
  vaultCount: number;
  syncingCount: number;
  attentionCount: number;
}

export interface EngineError {
  kind: ErrorKind;
  message: string;
}

export type EngineEvent =
  | {
      type: "statusTick";
      id: VaultId;
      state: SyncState;
      peersConnected: number;
      mainShortSha: string;
    }
  | { type: "aggregateTick"; state: SyncState }
  | { type: "peerConnected"; id: VaultId; peerFingerprint: string }
  | { type: "peerDisconnected"; id: VaultId; peerFingerprint: string }
  | { type: "tofuRequested"; request: TofuRequest }
  | { type: "tofuResolved"; requestId: string; allowed: boolean }
  | { type: "supersededEdit"; id: VaultId; path: string; snapshotHint: string }
  | { type: "snapshotCreated"; id: VaultId; snapshot: Snapshot }
  | { type: "listenerChanged"; id: VaultId; listener: ListenerInfo }
  | { type: "error"; id: VaultId | null; message: string };

/** The methods the UI calls — identical across Tauri and the browser mock. */
export interface EngineApi {
  listVaults(): Promise<Vault[]>;
  addLocalFolder(path: string): Promise<Vault>;
  cloneRemote(dest: string, url: string): Promise<CloneOutcome>;
  removeVault(id: VaultId): Promise<void>;
  setEnabled(id: VaultId, on: boolean): Promise<void>;
  setAllowConnections(id: VaultId, on: boolean): Promise<ListenerInfo>;
  getConnectAddress(id: VaultId): Promise<ConnectAddress>;
  listAuthorized(id: VaultId): Promise<AuthorizedKey[]>;
  authorize(id: VaultId, pubkey: string): Promise<void>;
  revoke(id: VaultId, fingerprint: string): Promise<void>;
  respondTofu(requestId: string, allow: boolean): Promise<void>;
  getIdentity(): Promise<Identity>;
  setIdentitySource(src: IdentitySource): Promise<Identity>;
  getSettings(): Promise<AppSettings>;
  setSettings(settings: AppSettings): Promise<AppSettings>;
  createSnapshot(id: VaultId, name: string): Promise<Snapshot>;
  listSnapshots(id: VaultId): Promise<Snapshot[]>;
  restore(id: VaultId, target: RestoreTarget): Promise<void>;
  getStatus(id: VaultId): Promise<VaultStatus>;
  getAggregateStatus(): Promise<AggregateStatus>;
  devTriggerTofu(): Promise<void>;
  devTriggerSuperseded(): Promise<void>;
  /** Subscribe to the live event stream; returns an unsubscribe fn. */
  subscribe(cb: (e: EngineEvent) => void): () => void;
  /** Best-effort: keep the native tray menu in sync (no-op in browser). */
  refreshTray(): Promise<void>;
}
