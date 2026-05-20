// The plugin is just another front-end over the same vault directory the
// native `ctx` CLI reads/writes, so it does NOT keep a private `data.json`.
// Persistence is split into two files, each owned by a clear party:
//
//   .context/config          The canonical, SHARED vault config. Exactly the
//                             bytes `ctx` reads/writes, round-tripped through
//                             the single Rust codec (wasm). The plugin only
//                             projects `peerUrl` onto its `peers` list and
//                             leaves every other field (vault_id, include,
//                             debounce_ms, …) untouched: it parses the file
//                             that is on disk, mutates `peers`, and
//                             re-serializes, so anything `ctx` wrote survives.
//                             If this file does not exist the plugin does NOT
//                             invent one (it has no authority to mint a
//                             vault_id); the peer URL still survives in the
//                             node-local sidecar and is reconciled into the
//                             canonical config once the vault exists.
//
//   .context/obsidian.json   A node-local sidecar the PLUGIN fully owns. It
//                             lives under `.context/`, which is never synced
//                             and is excluded from the engine repo — the same
//                             rationale that makes `authorized_keys`/`exclude`
//                             node-local. It holds every plugin-only knob
//                             (peerPubkey, syncEnabled, onboarded, ignoreGlobs,
//                             identityPath) plus a fallback copy of peerUrl
//                             for the pre-config window. To keep the file
//                             minimal, a knob is only written when it differs
//                             from its default.
//
// The Rust config codec is strict and flat: it only round-trips the known
// `VaultConfig` fields, so plugin-only knobs could never have lived safely in
// `.context/config` anyway — they belong in the sidecar.
//
// Pure module — no `obsidian` runtime import — so it is fully unit testable.
// File IO is injected via `MinimalDataAdapter`.

import { type VaultConfig, parseConfig, serializeConfig } from '@csp/sdk/web-init';
import type { MinimalDataAdapter } from './storage-adapter.js';

export interface CspSettings {
  /** The full node (running in listen mode) this thin node syncs with.
   * Empty → offline-first local-only node (won't converge until it connects
   * to a full node). Mapped onto `VaultConfig.peers` in `.context/config`. */
  peerUrl: string;
  /** Pinned peer identity, SSH wire format (`ssh-ed25519 AAAA…`). Set on the
   * first successful connect. Plugin-owned (sidecar). */
  peerPubkey: string;
  /** Master switch. While false the plugin opens no engine session and makes
   * no connection. Plugin-owned (sidecar). */
  syncEnabled: boolean;
  /** True only once setup fully succeeded (create: local vault built;
   * connect: peer handshake + first catch-up reached). Plugin-owned
   * (sidecar). */
  onboarded: boolean;
  /** Extra globs to skip, on top of the always-on text-allowlist +
   * `.context/` exclusion. Plugin-owned (sidecar). */
  ignoreGlobs: string[];
  /** Vault-relative identity location. Set on mobile; unset on desktop so it
   * defaults to the CLI-shared `~/.context/id_ed25519`. Plugin-owned
   * (sidecar). */
  identityPath: string;
  /** §10 enrollment secret used at clone time. Persisted in the sidecar
   * (under the never-synced `.context/`) so the user doesn't have to
   * re-paste during setup retries. Plugin-owned. */
  authKey: string;
}

export const DEFAULT_SETTINGS: CspSettings = {
  peerUrl: '',
  peerPubkey: '',
  syncEnabled: false,
  onboarded: false,
  ignoreGlobs: [],
  identityPath: '',
  authKey: '',
};

/** Parse a textarea value (one glob per line) into a clean list. */
export function parseIgnoreGlobs(input: string): string[] {
  return input
    .split('\n')
    .map((s) => s.trim())
    .filter((s) => s.length > 0 && !s.startsWith('#'));
}

/** Normalize a user-entered peer URL so a bare domain like `host.example`
 * dials `wss://host.example:443` — what every TLS-terminating front (Fly,
 * Railway, …) actually exposes. Browsers' `WebSocket` rejects schemeless
 * inputs, so we fill in `wss://` (or `ws://` when explicitly requested) and
 * supply the default port if the authority is portless. Idempotent. */
export function normalizePeerUrl(input: string): string {
  const trimmed = input.trim();
  if (trimmed === '') return '';
  let secure = true;
  let rest = trimmed;
  if (trimmed.startsWith('wss://')) rest = trimmed.slice('wss://'.length);
  else if (trimmed.startsWith('ws://')) {
    secure = false;
    rest = trimmed.slice('ws://'.length);
  } else if (trimmed.startsWith('https://')) rest = trimmed.slice('https://'.length);
  else if (trimmed.startsWith('http://')) {
    secure = false;
    rest = trimmed.slice('http://'.length);
  }
  const slash = rest.indexOf('/');
  const authority = slash === -1 ? rest : rest.slice(0, slash);
  const path = slash === -1 ? '' : rest.slice(slash);
  // IPv6 literal: `[::1]:port` — the port is the `:` after `]`. For
  // names/IPv4 a bare `:` in the authority is the port.
  const closeBracket = authority.lastIndexOf(']');
  const hasPort =
    closeBracket === -1 ? authority.includes(':') : authority.slice(closeBracket).startsWith(']:');
  const scheme = secure ? 'wss' : 'ws';
  if (authority === '' || hasPort) return `${scheme}://${authority}${path}`;
  return `${scheme}://${authority}:${secure ? 443 : 80}${path}`;
}

// ---- Pure config ⇄ settings mapping (unit-testable) ----

/** The plugin-owned sidecar shape. Every field is optional — a missing key
 * means "use the default". `peerUrl` is mirrored here so it still persists
 * before a canonical `.context/config` exists. */
interface SidecarJson {
  peerUrl?: string;
  peerPubkey?: string;
  syncEnabled?: boolean;
  onboarded?: boolean;
  ignoreGlobs?: string[];
  identityPath?: string;
  authKey?: string;
}

/** Project a (possibly absent) canonical config + sidecar onto the merged
 * settings view. `.context/config`'s `peers[0]` wins for `peerUrl` when the
 * file exists; otherwise the sidecar's fallback copy is used. */
export function settingsFromParts(cfg: VaultConfig | null, side: SidecarJson | null): CspSettings {
  const s: CspSettings = { ...DEFAULT_SETTINGS };
  if (side) {
    if (typeof side.peerUrl === 'string') s.peerUrl = side.peerUrl;
    if (typeof side.peerPubkey === 'string') s.peerPubkey = side.peerPubkey;
    if (typeof side.syncEnabled === 'boolean') s.syncEnabled = side.syncEnabled;
    if (typeof side.onboarded === 'boolean') s.onboarded = side.onboarded;
    if (Array.isArray(side.ignoreGlobs)) {
      s.ignoreGlobs = side.ignoreGlobs.filter((g): g is string => typeof g === 'string');
    }
    if (typeof side.identityPath === 'string') s.identityPath = side.identityPath;
    if (typeof side.authKey === 'string') s.authKey = side.authKey;
  }
  // The shared canonical config is authoritative for the peer URL whenever it
  // exists (it is what `ctx` and the engine actually read).
  if (cfg) s.peerUrl = cfg.peers[0] ?? '';
  return s;
}

/** Build the sidecar JSON, persisting only knobs that differ from their
 * default so the file stays minimal (mirrors the old "drop empty knobs"
 * behavior). */
export function sidecarFromSettings(s: CspSettings): SidecarJson {
  const out: SidecarJson = {};
  if (s.peerUrl !== DEFAULT_SETTINGS.peerUrl) out.peerUrl = s.peerUrl;
  if (s.peerPubkey !== DEFAULT_SETTINGS.peerPubkey) out.peerPubkey = s.peerPubkey;
  if (s.syncEnabled !== DEFAULT_SETTINGS.syncEnabled) out.syncEnabled = s.syncEnabled;
  if (s.onboarded !== DEFAULT_SETTINGS.onboarded) out.onboarded = s.onboarded;
  if (s.ignoreGlobs.length > 0) out.ignoreGlobs = s.ignoreGlobs.slice();
  if (s.identityPath !== DEFAULT_SETTINGS.identityPath) out.identityPath = s.identityPath;
  if (s.authKey !== DEFAULT_SETTINGS.authKey) out.authKey = s.authKey;
  return out;
}

/** Layer `peerUrl` onto an existing canonical config so everything `ctx`
 * wrote (vault_id, include, debounce_ms, …) is preserved. */
export function applyPeerUrl(cfg: VaultConfig, peerUrl: string): VaultConfig {
  return { ...cfg, peers: peerUrl ? [peerUrl] : [] };
}

// ---- File-backed store ----

/**
 * Reads/writes the two-file split: the SHARED `.context/config` (only the
 * peer URL is the plugin's to touch) and the node-local plugin sidecar
 * `.context/obsidian.json`.
 */
export class ConfigStore {
  /** Kept for back-compat with callers that gate on the canonical config
   * path. The sidecar path is derived from the same `.context` root. */
  static readonly PATH = '.context/config';
  static readonly SIDECAR_PATH = '.context/obsidian.json';

  /** Serializes saves: connect-mode setup can fire two near-simultaneous
   * writes (the peer-key pin on `connected` and the onboarding latch), and
   * the tmp-write/rename dance is not concurrency-safe. */
  private writeChain: Promise<void> = Promise.resolve();

  constructor(private readonly adapter: MinimalDataAdapter) {}

  /** True once the plugin or `ctx` has persisted anything for this vault —
   * i.e. either the canonical config or the plugin sidecar exists. The
   * plugin treats this as the "is there state to load?" signal. */
  async exists(): Promise<boolean> {
    return (
      (await this.adapter.exists(ConfigStore.PATH)) ||
      (await this.adapter.exists(ConfigStore.SIDECAR_PATH))
    );
  }

  async load(): Promise<CspSettings> {
    let cfg: VaultConfig | null = null;
    if (await this.adapter.exists(ConfigStore.PATH)) {
      const text = await this.adapter.read(ConfigStore.PATH);
      cfg = parseConfig(text);
    }
    let side: SidecarJson | null = null;
    if (await this.adapter.exists(ConfigStore.SIDECAR_PATH)) {
      const raw = await this.adapter.read(ConfigStore.SIDECAR_PATH);
      try {
        side = JSON.parse(raw) as SidecarJson;
      } catch {
        // A corrupt sidecar must not wedge load — fall back to defaults.
        side = null;
      }
    }
    return settingsFromParts(cfg, side);
  }

  save(s: CspSettings): Promise<void> {
    const next = this.writeChain.then(
      () => this.doSave(s),
      () => this.doSave(s),
    );
    this.writeChain = next.then(
      () => {},
      () => {},
    );
    return next;
  }

  private async doSave(s: CspSettings): Promise<void> {
    await this.ensureContextDir();

    // 1. Sidecar — plugin-owned knobs + the peerUrl fallback.
    const side = sidecarFromSettings(s);
    await this.atomicWrite(ConfigStore.SIDECAR_PATH, JSON.stringify(side, null, 2));

    // 2. Canonical config — ONLY the peer URL is ours to touch, and only if
    // the file already exists (the plugin must not mint a vault_id). Parse
    // what is on disk, mutate `peers`, re-serialize so `ctx`-written fields
    // survive.
    if (await this.adapter.exists(ConfigStore.PATH)) {
      const text = await this.adapter.read(ConfigStore.PATH);
      const cfg = applyPeerUrl(parseConfig(text), s.peerUrl);
      await this.atomicWrite(ConfigStore.PATH, serializeConfig(cfg));
    }
  }

  private async ensureContextDir(): Promise<void> {
    if (!(await this.adapter.exists('.context'))) {
      await this.adapter.mkdir('.context');
    }
  }

  /** Write `<path>.tmp` then rename over `path` — the previous version stays
   * intact until the rename succeeds. */
  private async atomicWrite(path: string, text: string): Promise<void> {
    const tmp = `${path}.tmp`;
    await this.adapter.write(tmp, text);
    if (await this.adapter.exists(path)) {
      await this.adapter.remove(path);
    }
    await this.adapter.rename(tmp, path);
  }
}
