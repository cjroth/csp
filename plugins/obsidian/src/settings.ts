// The plugin's settings ARE `<vault>/.context/config` — the same file the
// native `ctx` CLI reads/writes (CSP spec.md §9.1/§17.1: `ctx` is one
// front-end, the plugin another). There is no separate `data.json`: a vault
// directory works identically from the CLI or this plugin, which
// structurally removes the "configured-vs-on-disk" divergence bug class.
//
// Schema-defined tables (`[peer]`, `[identity]`, `[scope]`) are shared with
// `ctx`. Plugin-only knobs live in an `[obsidian]` table `ctx` ignores; the
// codec round-trips unknown tables losslessly so neither clobbers the
// other.
//
// Pure module — no `obsidian` runtime import — so it is fully unit
// testable. File IO is injected via `MinimalDataAdapter`.
//
// CSP recast of the agentsync settings: no `vaultId`/`vaultName` (no
// server-minted id, CSP §5); `rendezvous_url`→`[peer] url`,
// `hub_pubkey`→`[peer] pubkey` (no hub — a thin node connects to a full
// node in listen mode, CSP §6.1).

import {
  type CspConfig,
  type TomlDoc,
  type TomlValue,
  applyConfigToDoc,
  configFromDoc,
  parseTomlDoc,
  stringifyTomlDoc,
} from '@csp/sdk/web-init';
import type { MinimalDataAdapter } from './storage-adapter.js';

/** Plugin-only TOML table `ctx` ignores. */
const OBSIDIAN_TABLE = 'obsidian';

export interface CspSettings {
  /** `[peer] url` — the full node (in listen mode, CSP §6.1) to sync with.
   * Empty → offline-first local-only thin node (won't converge until it
   * connects to a full node — CSP §7). */
  peerUrl: string;
  /** `[peer] pubkey` — pinned peer identity, SSH wire format
   * (`ssh-ed25519 AAAA…`). Set on first successful connect (CSP §10 key
   * pinning). */
  peerPubkey: string;
  /** `[obsidian] sync_enabled` — master switch. While false the plugin
   * opens no engine session and makes no connection. */
  syncEnabled: boolean;
  /** `[obsidian] auto_connect` — open the peer connection on launch (vs.
   * staying prepared). Only meaningful while `syncEnabled`. */
  autoConnectOnStart: boolean;
  /** `[obsidian] onboarded` — true only once setup fully succeeded
   * (create: local vault built; connect: peer handshake + first catch-up
   * reached). `ctx` ignores this key. */
  onboarded: boolean;
  /** `[obsidian] ignore_globs` — extra globs to skip, on top of the
   * always-on text-allowlist + `.context/` exclusion. */
  ignoreGlobs: string[];
  /** `[identity] path` — vault-relative identity location. Set on mobile;
   * unset on desktop so it defaults to the CLI-shared
   * `~/.context/id_ed25519` (CSP §9.1/§10). */
  identityPath: string;
}

export const DEFAULT_SETTINGS: CspSettings = {
  peerUrl: '',
  peerPubkey: '',
  syncEnabled: false,
  // Default OFF: a first connect against a populated peer can pull many
  // files; the user opts in after configuring the peer URL.
  autoConnectOnStart: false,
  onboarded: false,
  ignoreGlobs: [],
  identityPath: '',
};

/** Parse a textarea value (one glob per line) into a clean list. */
export function parseIgnoreGlobs(input: string): string[] {
  return input
    .split('\n')
    .map((s) => s.trim())
    .filter((s) => s.length > 0 && !s.startsWith('#'));
}

// ---- Pure config ⇄ settings mapping (unit-testable) ----

function obsidianTable(doc: TomlDoc): Map<string, TomlValue> | undefined {
  return doc.get(OBSIDIAN_TABLE);
}

/** Project a parsed `.context/config` doc onto the plugin's settings view. */
export function settingsFromTomlDoc(doc: TomlDoc): CspSettings {
  const cfg = configFromDoc(doc);
  const ob = obsidianTable(doc);
  const auto = ob?.get('auto_connect');
  const sync = ob?.get('sync_enabled');
  const onboarded = ob?.get('onboarded');
  const globs = ob?.get('ignore_globs');
  return {
    peerUrl: cfg.peer.url ?? '',
    peerPubkey: cfg.peer.pubkey ?? '',
    syncEnabled: sync === true,
    autoConnectOnStart: auto === true,
    onboarded: onboarded === true,
    ignoreGlobs: Array.isArray(globs) ? globs.slice() : [],
    identityPath: cfg.identity.path ?? '',
  };
}

/**
 * Layer settings back onto `base` (the doc parsed from disk, so unknown
 * `ctx`-written content survives). Empty/false plugin knobs are removed so
 * a default config is byte-identical to what `ctx` would write — no stray
 * empty `[obsidian]` table.
 */
export function writeSettingsToTomlDoc(s: CspSettings, base?: TomlDoc): TomlDoc {
  const prev: CspConfig = configFromDoc(base ?? new Map());
  // Only the fields the plugin owns are overwritten; scope.* and any
  // unknown `ctx` content are preserved. Empty string → drop the key (so
  // we never persist `url = ""`).
  prev.peer.url = s.peerUrl || undefined;
  prev.peer.pubkey = s.peerPubkey || undefined;
  prev.identity.path = s.identityPath || undefined;
  const doc = applyConfigToDoc(prev, base);

  const ob = new Map<string, TomlValue>();
  if (s.syncEnabled) ob.set('sync_enabled', true);
  if (s.autoConnectOnStart) ob.set('auto_connect', true);
  if (s.onboarded) ob.set('onboarded', true);
  if (s.ignoreGlobs.length > 0) ob.set('ignore_globs', s.ignoreGlobs.slice());
  if (ob.size > 0) doc.set(OBSIDIAN_TABLE, ob);
  else doc.delete(OBSIDIAN_TABLE);
  return doc;
}

// ---- File-backed store ----

/**
 * Reads/writes `<vault-root>/.context/config`. The last-parsed doc is
 * retained so saves are lossless w.r.t. anything `ctx` wrote that the
 * plugin doesn't model.
 */
export class ConfigStore {
  static readonly PATH = '.context/config';
  private doc: TomlDoc = new Map();
  /** Serializes saves: connect-mode setup can fire two near-simultaneous
   * writes (the peer-key pin on `connected` and the onboarding latch), and
   * the tmp-write/rename dance is not concurrency-safe. */
  private writeChain: Promise<void> = Promise.resolve();

  constructor(private readonly adapter: MinimalDataAdapter) {}

  /** True once setup has written `.context/config`. The plugin treats this
   * as the single "is this vault configured?" signal. */
  exists(): Promise<boolean> {
    return this.adapter.exists(ConfigStore.PATH);
  }

  async load(): Promise<CspSettings> {
    if (await this.adapter.exists(ConfigStore.PATH)) {
      const text = await this.adapter.read(ConfigStore.PATH);
      this.doc = parseTomlDoc(text);
    } else {
      this.doc = new Map();
    }
    return settingsFromTomlDoc(this.doc);
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
    this.doc = writeSettingsToTomlDoc(s, this.doc);
    const text = stringifyTomlDoc(this.doc);
    if (!(await this.adapter.exists('.context'))) {
      await this.adapter.mkdir('.context');
    }
    const tmp = `${ConfigStore.PATH}.tmp`;
    await this.adapter.write(tmp, text);
    if (await this.adapter.exists(ConfigStore.PATH)) {
      await this.adapter.remove(ConfigStore.PATH);
    }
    await this.adapter.rename(tmp, ConfigStore.PATH);
  }
}
