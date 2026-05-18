// JS-side mirrors of the csp-core / csp-wasm data shapes a thin-node host
// (the Obsidian plugin) consumes. CSP recast of the agentsync SDK types:
// no hub, no server-minted vault id (CSP spec.md §5) — a thin node connects
// to a *peer* (a full node in listen mode, CSP §6.1) and converges via the
// engine's frontier-set anti-entropy + deterministic fold (CSP §5, §6.4).

/** File metadata as returned by `Vault.listFiles()`. */
export interface FileMeta {
  id: string;
  path: string;
  /** v1 plugin scope is text-only; binaries are opt-in whole-file LWW
   * (CSP spec §11) and out of scope for the plugin. */
  kind: 'Text' | 'Binary';
  size: number;
  created_at: number;
  updated_at: number;
  /** Soft-delete timestamp (Unix ms). Missing/undefined when alive. */
  deleted_at?: number | null;
}

/**
 * A named snapshot (CSP spec §8): the frontier primitive-commit SHA set at
 * creation plus a label. Replicates as a small record; durable across any
 * retention horizon, including on a thin node (CSP §9.2).
 */
export interface Snapshot {
  name: string;
  created_at_ms: number;
  /** Frontier primitive-commit SHAs captured at snapshot time. */
  frontier: string[];
}

/**
 * High-level event emitted by a `Vault`. The host projects these onto its
 * UI connection state; it is not a protocol state machine (that is the
 * engine's). `tree-changed` replaces agentsync's `doc-changed`: the
 * materialized working tree changed (a new merged tree arrived from the
 * connected full node, or a local primitive was folded — CSP §5.6/§6.5).
 */
export type VaultEvent =
  | { kind: 'connecting'; url: string }
  | { kind: 'connected'; peer_pubkey: Uint8Array }
  | { kind: 'disconnected'; reason: string }
  | { kind: 'catchup-progress'; outbound: boolean }
  | { kind: 'tree-changed' }
  | { kind: 'error'; message: string };

/**
 * Thin-node storage contract (CSP spec §9.1 — a host MAY override
 * `.context/` to host-provided storage). Replaces agentsync's Automerge
 * `loadDoc`/`saveDoc`/`loadSyncState`: a CSP thin node persists an object
 * subset (current `main` + history within the retention horizon, CSP §9.2),
 * the `.context/state` record (last-materialized hashes §5.6, durable
 * logical counter §5.1), and the frontier (CSP §6.4).
 */
export interface StorageAdapter {
  /** Content-addressed object store (oids are lowercase hex SHA-1, CSP §4). */
  getObject(oid: string): Promise<Uint8Array | null>;
  putObject(oid: string, bytes: Uint8Array): Promise<void>;
  hasObject(oid: string): Promise<boolean>;
  listObjectOids(): Promise<string[]>;
  /** The `.context/state` record. Null when none saved yet. */
  loadState(): Promise<Uint8Array | null>;
  saveState(bytes: Uint8Array): Promise<void>;
  /** The frontier (un-merged primitive tip set, CSP §6.4). */
  loadFrontier(): Promise<Uint8Array | null>;
  saveFrontier(bytes: Uint8Array): Promise<void>;
  /** Optional engine-managed identity seed. The plugin injects identity, so
   * these are interface-parity only (kept like agentsync). */
  loadIdentitySeed(): Promise<Uint8Array | null>;
  saveIdentitySeed(seed: Uint8Array): Promise<void>;
  /** Named snapshot records (CSP §8). */
  loadSnapshots(): Promise<Uint8Array | null>;
  saveSnapshots(bytes: Uint8Array): Promise<void>;
  /** Best-effort dispose. */
  close(): Promise<void>;
}

/** Transport contract — a thin node is outbound-only and never listens
 * (CSP spec §7 HARD INVARIANT). The host picks native `WebSocket`
 * (browser/Electron) or anything speaking binary WebSocket frames. */
export interface TransportAdapter {
  connect(url: string, opts?: TransportConnectOpts): Promise<TransportConn>;
}

export interface TransportConnectOpts {
  /** Pin the peer (full-node listener) cert; CSP §10 channel binding. */
  pinnedCertFingerprint?: Uint8Array;
}

export interface TransportConn {
  send(bytes: Uint8Array): Promise<void>;
  recv(): AsyncIterable<Uint8Array>;
  /** TLS peer cert SHA-256 if the runtime exposes it. Browsers can't. */
  channelBinding(): Uint8Array | null;
  close(): Promise<void>;
}

/** Options accepted by `Vault.create` / `open` / `clone`. */
export interface VaultOptions {
  /** Persistent thin-node state root (CSP §9.1 host override). */
  storage: StorageAdapter;
  /** Device identity (CSP §10 — node identity is an SSH key). The host
   * owns it; when absent the engine self-manages a seed via `storage`. */
  identity?: import('./vault.js').Identity;
  /** Peer (full node in listen mode, CSP §6.1) WebSocket URL, e.g.
   * `wss://host:port`. Empty/absent → offline-first local-only thin node
   * (will not converge until it connects to a full node — CSP §7). */
  peerUrl?: string;
  /** Pin the peer's identity pubkey (TOFU/key pinning, CSP §10 — a
   * connecting node also verifies the listener's key). */
  peerPubkey?: Uint8Array;
  /** WebSocket transport. Defaults to the runtime-appropriate adapter. */
  transport?: TransportAdapter;
}

export interface ReconnectOptions {
  /** Total attempts before giving up (default: Infinity). */
  maxAttempts?: number;
  /** Initial backoff in ms (default: 500). */
  initialBackoffMs?: number;
  /** Max backoff in ms (default: 30000). */
  maxBackoffMs?: number;
}

// ---- `.context/config` typed schema (provisional — CSP spec §9.1/§17.1) ----

/** A scalar or string-array TOML value — the only shapes the schema uses. */
export type TomlValue = string | number | boolean | string[];

/** An ordered TOML document: table → (key → value), insertion order kept.
 * Root (keys before any `[table]`) uses the `''` table key. */
export type TomlDoc = Map<string, Map<string, TomlValue>>;

// Optional fields are `?: T | undefined` so callers can clear them with an
// explicit `= undefined` under `exactOptionalPropertyTypes`.

/** `[peer]` — the full node this thin node syncs with (CSP §6.1). */
export interface PeerSection {
  /** Peer (full-node listener) WebSocket URL. */
  url?: string | undefined;
  /** Pinned peer identity pubkey, SSH wire format (`ssh-ed25519 AAAA…`). */
  pubkey?: string | undefined;
}

/** `[identity]` — where the device key lives (CSP §9.1/§10). */
export interface IdentitySection {
  /** Vault-relative identity path (set on mobile; unset → CLI default
   * `~/.context/id_ed25519`). */
  path?: string | undefined;
}

/** `[scope]` — the explicit allowlist (CSP §11). */
export interface ScopeSection {
  /** Synced text extensions (no dot, lowercase). */
  extensions: string[];
  /** Extra include patterns under the allowlist. */
  include: string[];
}

export interface CspConfig {
  peer: PeerSection;
  identity: IdentitySection;
  scope: ScopeSection;
}
