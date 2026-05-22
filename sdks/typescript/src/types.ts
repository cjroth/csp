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
  /** A merged tree arrived (CSP §6.5). `changes`, when present, names the
   * exact paths the materialize pass touched (and carries the new content
   * for writes, or `null` for removes). The host applies just those; the
   * absent-changes form still works as the full-scan fallback. */
  | {
      kind: 'tree-changed';
      changes?: Array<{ path: string; content: string | null }>;
    }
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
  /** Pre-shared auth key (CSP §10) — enrollment secret sent on the WS
   * upgrade. Required only when the peer listener was started with
   * `CTX_AUTH_KEY` and this device is not yet in its `authorized_keys`.
   * Sent as a `?auth_key=` query parameter (browser-WebSocket-compatible
   * — the browser `WebSocket` constructor cannot set arbitrary headers).
   * One-shot: after a successful enrollment the device's pubkey is in
   * the peer's authorized_keys and the auth-key is no longer consulted. */
  authKey?: string;
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
  /** Consecutive sessions that never complete the handshake (peer closes
   * before establish — unauthorized device key or incompatible peer) before
   * giving up with a terminal, actionable error instead of looping forever
   * (default: 5). A session that establishes resets the counter. */
  maxHandshakeFailures?: number;
}

// ---- `.context/config` typed schema (provisional — CSP spec §9.1/§17.1) ----

// This mirrors `csp_core::config::VaultConfig` exactly (field names are the
// serde JSON keys). The TOML codec is NOT reimplemented here — `config.ts`
// bridges to the one Rust codec via wasm (one engine everywhere). `listen`
// and `log` serialize as `null` when unset, matching serde's
// `Option<String>`.
export interface VaultConfig {
  /** Opaque protocol identity (a UUID by default). The handshake's
   * "same vault?" guard — not a display label. */
  vault_id: string;
  /** Optional human label (may be empty). */
  name: string;
  /** Peer (full-node listener) URLs this node syncs with (CSP §6.1). */
  peers: string[];
  /** Listen address for a full node; `null` for a thin/outbound-only node. */
  listen: string | null;
  /** Skip trust-on-first-use key pinning (CSP §10). */
  no_tofu: boolean;
  /** Serve a plaintext `ws://` listener instead of the default `wss://`. */
  no_tls: boolean;
  /** Log level / filter; `null` → the launcher's built-in default. */
  log: string | null;
  /** Auto-commit debounce, in milliseconds. */
  debounce_ms: number;
  /** Opt in to whole-file LWW binary sync (CSP §11). */
  allow_binary: boolean;
  /** The explicit include allowlist (CSP §11). */
  include: string[];
  /** Pre-shared bearer auth keys that gate `Authorization: Bearer …` HTTP
   * upgrade headers and let the listener enroll the connecting peer's pubkey
   * (CSP §10 auth-key bootstrap). Empty by default. */
  auth_keys: string[];
  /** Default TTL (days) applied when enrolling a peer pubkey via an auth
   * key. `null` → the engine's built-in default. */
  default_key_ttl_days: number | null;
}
