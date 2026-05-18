// The CSP thin-node public API **contract** (types only). The real
// implementation wraps `csp-wasm` (CSP spec.md §16); today `src/mock/`
// implements this so the host plugin is fully buildable/testable without
// the wasm runtime. CSP recast of agentsync's `Vault`: no `vaultIdValue`
// (no server-minted id, CSP §5), no `probeHub` (no hub, CSP §6.1); labels
// → snapshots (CSP §8); `clone` joins a peer (CSP §17 `ctx clone <url>`).

import type {
  FileMeta,
  ReconnectOptions,
  Snapshot,
  TransportAdapter,
  VaultEvent,
  VaultOptions,
} from './types.js';

/** Device identity — CSP spec §10 (an ed25519 / SSH key). */
export interface Identity {
  /** 32-byte ed25519 seed. */
  seed(): Uint8Array;
  pubkey(): Pubkey;
  /** ed25519 signature (64 bytes). Async to mirror ssh-agent/HW signers. */
  sign(message: Uint8Array): Promise<Uint8Array>;
  free(): void;
}

export interface Pubkey {
  /** OpenSSH wire format, e.g. `ssh-ed25519 AAAA…`. */
  toSshString(): string;
  /** Raw 32-byte public key. */
  bytes(): Uint8Array;
  /** Hex SHA-256 fingerprint. */
  fingerprint(): string;
  verify(message: Uint8Array, signature: Uint8Array): boolean;
  free(): void;
}

export interface CreateOptions extends VaultOptions {}
export interface OpenOptions extends VaultOptions {}
/** `clone` requires a peer URL (the full node to catch up from, CSP §17). */
export interface CloneOptions extends VaultOptions {
  peerUrl: string;
}

/**
 * A CSP thin-node vault session. Offline-first (CSP §7): file ops work with
 * no connection; `connectWithReconnect` catches up (CSP §6.4) and receives
 * the merged tree from the connected full node (CSP §6.5). The host never
 * computes the fold (CSP §7) — it drives this surface and renders
 * engine-reported state.
 */
export interface Vault {
  // ---- File operations ----
  writeTextFile(path: string, content: string): Promise<string>;
  readTextFile(path: string): Promise<string>;
  fileExists(path: string): boolean;
  deleteFile(path: string): Promise<void>;
  renameFile(from: string, to: string): Promise<void>;
  listFiles(): FileMeta[];

  // ---- Snapshots / recovery (CSP §8) ----
  createSnapshot(name: string): Promise<void>;
  deleteSnapshot(name: string): Promise<void>;
  listSnapshots(): Snapshot[];
  restoreToSnapshot(name: string): Promise<void>;
  /** Best-effort, approximate under clock skew, bounded by the thin-node
   * retention horizon (CSP §8/§9.2). */
  restoreToTime(targetMs: number): Promise<void>;

  // ---- Connection ----
  connectWithReconnect(opts?: ReconnectOptions): Promise<void>;
  disconnect(): Promise<void>;
  close(): Promise<void>;

  // ---- Events / accessors ----
  subscribe(listener: (e: VaultEvent) => void): () => void;
  /** Device public key in OpenSSH format (to paste into the full node's
   * `authorized_keys` via `ctx authorize`, CSP §10). */
  identityPubkeySsh(): string;
  isConnected(): boolean;
}

/** Re-exported so the mock and host share one option shape. */
export type { TransportAdapter, ReconnectOptions };
