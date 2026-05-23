// Owns the CSP thin-node session lifecycle and brokers state between the
// rest of the plugin. The state machine here is what the status bar
// visualizes and what the settings tab's Connect button toggles — a
// PROJECTION of engine-reported connectivity (CSP spec.md §6.5), not a
// protocol state machine (that is the engine's).
//
//   idle ──start──> connecting ──connected──> connected
//                                  └──error───> reconnecting ──→ connecting
//   any ──stop──> idle
//
// CSP recast of the agentsync controller: the entire hub / probeHub /
// vaultId-vs-doc reconciliation is GONE (CSP §5 — no server-minted id, so
// that whole bug class disappears). On disconnect the engine just
// reconnects and re-runs catch-up — there is no separate resync path (CSP
// §6.5); "Resync now" re-runs the host-side reconcile pass.

import {
  type Snapshot as CspSnapshot,
  type FileMeta,
  type IdentityInstance,
  Pubkey,
  type StorageAdapter,
  type TransportAdapter,
  Vault,
  type VaultEvent,
  type VaultInstance,
  initCsp,
} from '@csp/sdk/web-init';
import { type MinimalVault, ObsidianVaultBridge } from './bridge.js';
import { shouldSync } from './path-filter.js';
import { planReconcile } from './reconcile.js';
import type { CspSettings } from './settings.js';
import type { MinimalDataAdapter } from './storage-adapter.js';

export type ControllerState = 'idle' | 'connecting' | 'connected' | 'reconnecting' | 'error';

/** How `openOrCreate` wants the engine-side vault built. The controller
 * decides the mode (existing local state → open, peer set → clone, else
 * create) and hands this to `makeVault`; the in-process default and the
 * plugin's Web Worker factory (issue 0010) both consume it. */
export interface VaultSpec {
  mode: 'create' | 'open' | 'clone';
  peerUrl?: string;
  /** Raw pinned-peer-key bytes (already decoded from the SSH form). */
  peerPubkey?: Uint8Array;
  authKey?: string;
}

export interface ControllerDeps {
  storage: StorageAdapter;
  vault: MinimalVault;
  /** The host filesystem under the vault. Used only by `resetLocalState()`
   * to wipe the entire `.context/` folder — the engine's `StorageAdapter`
   * has no recursive-delete primitive. */
  fs?: MinimalDataAdapter;
  settings: CspSettings;
  /** The device identity, owned by the plugin (resolved from
   * `~/.context/id_ed25519` on desktop). Passed straight into the engine so
   * it never auto-generates or persists a seed itself (CSP §10). The plugin
   * frees it on unload — the controller must not. */
  identity: IdentityInstance;
  saveSettings: (s: CspSettings) => Promise<void>;
  /** Optional inlined wasm bytes — if absent, callers must call initCsp()
   * out of band. (Mock ignores the bytes; see @csp/sdk.) */
  wasmBytes?: Uint8Array;
  /** Optional pre-loaded session for tests; bypasses Vault.create/open. */
  sdkOverride?: VaultInstance;
  /** Optional WebSocket transport. Defaults to the engine's auto-detection. */
  transport?: TransportAdapter;
  /** Builds the engine-side vault from a [`VaultSpec`]. When absent the
   * controller builds an in-process `RealVault` (the SDK default, used by
   * tests). The plugin injects a Web Worker-backed factory (issue 0010) so
   * the wasm engine runs off the renderer thread and never freezes the UI. */
  makeVault?: (spec: VaultSpec) => Promise<VaultInstance>;
  /** Test seam — emitter for status notices. */
  notice?: (msg: string) => void;
  log?: (msg: string) => void;
  /** Allow tests to inject their own clock for retry backoff. */
  now?: () => number;
}

type Listener = (s: ControllerState) => void;

export class SyncController {
  state: ControllerState = 'idle';
  private sdk: VaultInstance | null = null;
  private bridge: ObsidianVaultBridge | null = null;
  private listeners = new Set<Listener>();
  private unsubscribeSdk: (() => void) | null = null;
  private connectPromise: Promise<void> | null = null;

  constructor(private readonly deps: ControllerDeps) {}

  on(l: Listener): () => void {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  }

  private setState(s: ControllerState): void {
    if (this.state === s) return;
    this.state = s;
    for (const l of this.listeners) {
      try {
        l(s);
      } catch {
        // listener errors don't propagate
      }
    }
  }

  /** Wire-format device public key — paste into the peer's
   * `authorized_keys` via `ctx authorize` (CSP §10). Derived from the
   * injected identity, so available immediately. */
  identityPubkeySsh(): string | null {
    const pk = this.deps.identity.pubkey();
    try {
      return pk.toSshString();
    } finally {
      pk.free();
    }
  }

  listSnapshots(): CspSnapshot[] {
    return this.sdk?.listSnapshots() ?? [];
  }

  /**
   * Bring the controller online.
   *
   * - `prepare()` / `start({ connect: false })` open/create the session +
   *   run a reconcile but DON'T connect. After this the device pubkey is
   *   available so the user can get it authorized on the peer first.
   * - `start({ connect: true })` (default) also runs the reconnect loop.
   *
   * Idempotent: a second start() while running is a no-op.
   */
  async start(opts: { connect?: boolean } = {}): Promise<void> {
    if (this.state !== 'idle' && this.state !== 'error') return;
    const wantConnect = (opts.connect ?? true) && this.deps.settings.peerUrl !== '';

    if (this.deps.wasmBytes) {
      await initCsp(this.deps.wasmBytes);
    }

    if (wantConnect) this.setState('connecting');
    try {
      const sdk = this.deps.sdkOverride ?? (await this.openOrCreate());
      this.sdk = sdk;

      this.bridge = new ObsidianVaultBridge({
        vault: this.deps.vault,
        sdk,
        filter: (p) => shouldSync(p, this.deps.settings.ignoreGlobs),
        log: this.deps.log ?? (() => {}),
      });

      // Bring the two sides byte-equal before live events stream.
      await this.runReconcile();

      this.unsubscribeSdk = sdk.subscribe((e) => this.onVaultEvent(e));

      if (!wantConnect) {
        this.setState('idle');
        return;
      }

      // Fire-and-forget the reconnect loop; we listen to its events.
      this.connectPromise = sdk.connectWithReconnect({}).catch((err) => {
        this.deps.log?.(`reconnect supervisor exited with error: ${err}`);
      });
    } catch (err) {
      this.setState('error');
      // Don't notice here: start() always rethrows, and every caller already
      // surfaces the failure exactly once (the settings-tab "setup failed"
      // toast + the wizard's inline `onboardingError`; onload's own catch).
      // A notice here too produced a duplicate failure toast on every failed
      // "Set up Context".
      throw err;
    }
  }

  /** Convenience: open the session without connecting. */
  async prepare(): Promise<void> {
    return this.start({ connect: false });
  }

  /** Disconnect and tear down. Safe to call from any state. */
  async stop(): Promise<void> {
    if (this.unsubscribeSdk) {
      this.unsubscribeSdk();
      this.unsubscribeSdk = null;
    }
    const sdk = this.sdk;
    this.bridge?.dispose();
    this.sdk = null;
    this.bridge = null;
    if (sdk) {
      try {
        await sdk.disconnect();
      } catch {}
      try {
        await sdk.close();
      } catch {}
    }
    this.connectPromise = null;
    this.setState('idle');
  }

  /** Re-run the bidirectional reconcile pass. With the real engine a
   * reconnect already re-runs catch-up (CSP §6.5); this is the manual
   * recovery action. */
  async resyncNow(): Promise<void> {
    if (!this.sdk || !this.bridge) {
      this.deps.notice?.('Context: not running.');
      return;
    }
    await this.runReconcile();
    this.deps.notice?.('Context: resynced.');
  }

  async createSnapshot(name: string): Promise<void> {
    if (!this.sdk) return;
    await this.sdk.createSnapshot(name);
  }

  async restoreToSnapshot(name: string): Promise<void> {
    if (!this.sdk || !this.bridge) return;
    await this.sdk.restoreToSnapshot(name);
    await this.bridge.applyRemoteState();
  }

  /**
   * Stop the controller and recursively delete the entire `<vault>/.context/`
   * folder — engine state, snapshots, shared config, plugin sidecar, and (on
   * mobile, where the device key lives in-vault) the device identity. The
   * Obsidian vault contents themselves are never touched. After this returns
   * the plugin is back to its unconfigured state and the setup wizard takes
   * over.
   *
   * Desktop keeps its device key intact: it lives in `~/.context/id_ed25519`
   * (shared with `ctx`), outside the vault.
   */
  async resetLocalState(): Promise<void> {
    await this.stop();
    if (this.deps.fs) {
      // Loud about what was deleted + every error that occurred — issue
      // 0014: on mobile, a silent reset that "succeeded" without
      // actually removing `.context/` was indistinguishable from a real
      // wipe, and the next start always picked `mode=open` from the
      // stale state file. Surface the report so the user can see why a
      // reset didn't take.
      const report = await removeDirRecursive(this.deps.fs, '.context');
      this.deps.log?.(
        `resetLocalState removed files=${report.filesRemoved} folders=${report.foldersRemoved} errors=${report.errors.length}`,
      );
      for (const e of report.errors) this.deps.log?.(`  reset error: ${e}`);
      if (report.errors.length > 0) {
        this.deps.notice?.(
          `Context: local state partially cleared — ${report.errors.length} error(s); see logs.`,
        );
        return;
      }
    } else {
      // Fallback for callers that didn't wire `fs` (older tests). Best-effort
      // zero-out of the engine-owned blobs so the engine rebuilds on restart.
      await this.deps.storage.saveState(new Uint8Array(0));
      await this.deps.storage.saveFrontier(new Uint8Array(0));
      await this.deps.storage.saveSnapshots(new Uint8Array(0));
    }
    this.deps.notice?.('Context: local state cleared.');
  }

  /** The bridge is exposed for the Obsidian event listeners in main.ts. */
  getBridge(): ObsidianVaultBridge | null {
    return this.bridge;
  }

  // ---- Internal ----

  /**
   * Open the existing local session, else clone from the peer, else create
   * a fresh local thin vault. No id negotiation (CSP §5): a thin node has
   * no server-minted id — it just catches up its frontier (CSP §6.4).
   */
  private async openOrCreate(): Promise<VaultInstance> {
    const { peerUrl, peerPubkey, authKey } = this.deps.settings;
    // Existing local state → open; a peer set → clone (catch up from it,
    // CSP §17); else a fresh offline-local vault (CSP §7 — won't converge
    // until a full node joins).
    const existing = await this.deps.storage.loadState();
    const stateBytes = existing?.length ?? 0;
    const mode = existing && existing.length > 0 ? 'open' : peerUrl ? 'clone' : 'create';
    // The mode-decision is load-bearing — a stale state file makes a
    // freshly-installed peer choose `open` instead of `clone`, which
    // bypasses catch-up and is the exact mobile-no-sync symptom (issue
    // 0014). Log the inputs so the next capture pins it.
    this.deps.log?.(
      `openOrCreate mode=${mode} stateBytes=${stateBytes} peerUrl=${peerUrl || '-'} peerPubkey=${peerPubkey ? 'pinned' : '-'}`,
    );
    const spec: VaultSpec = {
      mode,
      ...(peerUrl ? { peerUrl } : {}),
      ...(peerPubkey ? { peerPubkey: sshPubkeyBytes(peerPubkey) } : {}),
      ...(authKey ? { authKey } : {}),
    };
    if (this.deps.makeVault) return this.deps.makeVault(spec);
    return this.defaultMakeVault(spec);
  }

  /** In-process `RealVault` — the SDK default. The plugin overrides this
   * with a Web Worker factory via `deps.makeVault` (issue 0010). */
  private defaultMakeVault(spec: VaultSpec): Promise<VaultInstance> {
    const base = {
      storage: this.deps.storage,
      identity: this.deps.identity,
      ...(spec.peerUrl ? { peerUrl: spec.peerUrl } : {}),
      ...(spec.peerPubkey ? { peerPubkey: spec.peerPubkey } : {}),
      ...(this.deps.transport ? { transport: this.deps.transport } : {}),
      ...(spec.authKey ? { authKey: spec.authKey } : {}),
    };
    if (spec.mode === 'open') return Vault.open(base);
    if (spec.mode === 'clone') return Vault.clone({ ...base, peerUrl: spec.peerUrl ?? '' });
    return Vault.create(base);
  }

  private async runReconcile(): Promise<void> {
    if (!this.sdk || !this.bridge) return;
    const sdk = this.sdk;
    const bridge = this.bridge;
    const obsidianFiles = this.deps.vault.getFiles().map((f) => ({
      path: f.path,
      readText: () => this.deps.vault.read(f),
    }));
    const sdkFiles = sdk.listFiles().map((m: FileMeta) => ({
      path: m.path,
      deleted: !!m.deleted_at,
      readText: () => sdk.readTextFile(m.path),
    }));
    const plan = await planReconcile({
      obsidianFiles,
      sdkFiles,
      filter: (p) => shouldSync(p, this.deps.settings.ignoreGlobs),
    });
    // Yield to the event loop every YIELD_EVERY ops so a large initial
    // reconcile doesn't freeze the renderer.
    const YIELD_EVERY = 25;
    let i = 0;
    for (const op of plan.pushToSdk) {
      await sdk.writeTextFile(op.path, op.content);
      bridge.pushed += 1;
      if (++i % YIELD_EVERY === 0) await new Promise((r) => setTimeout(r, 0));
    }
    let j = 0;
    for (const op of plan.applyToObsidian) {
      await bridge.applyOneRemoteFile({
        id: '',
        path: op.path,
        kind: 'Text',
        size: op.content.length,
        created_at: 0,
        updated_at: 0,
        deleted_at: null,
      });
      if (++j % YIELD_EVERY === 0) await new Promise((r) => setTimeout(r, 0));
    }
    let k = 0;
    for (const path of plan.deleteInObsidian) {
      const ex = this.deps.vault.getAbstractFileByPath(path);
      if (ex) {
        bridge.suppress(path);
        await this.deps.vault.delete(ex);
      }
      if (++k % YIELD_EVERY === 0) await new Promise((r) => setTimeout(r, 0));
    }
    // Seed the remote-removal baseline from the converged engine set. Without
    // this, a reload where Obsidian + engine already match makes reconcile a
    // no-op (no applyOneRemoteFile calls), leaving `knownSdkPaths` empty — so
    // a later CLI folder delete/rename has nothing to diff against and the
    // old files/folder never get removed in Obsidian.
    bridge.seedKnownPaths();
  }

  private onVaultEvent(e: VaultEvent): void {
    // Lifecycle visibility — without these, a silent disconnect/reconnect
    // cycle in production is invisible (status bar transitions vanish
    // quickly and there's no console trail). The summary keeps each event
    // to one line so the log stays readable during initial catch-up.
    this.deps.log?.(`event ${e.kind}${eventSummary(e)}`);
    switch (e.kind) {
      case 'connecting':
        this.setState('connecting');
        break;
      case 'connected': {
        this.setState('connected');
        // Key-pin the peer identity on first connect (CSP §10 — a
        // connecting node also verifies the listener's key). SSH wire
        // format, the same representation `ctx` uses. The engine verifies
        // the listener's key inside the handshake; it does not surface the
        // raw bytes to the host yet, so `peer_pubkey` is empty on the real
        // path — only pin when it actually carries a key (forward-compatible
        // and avoids a swallowed throw on every connect).
        if (!this.deps.settings.peerPubkey && e.peer_pubkey.length > 0) {
          const pk = Pubkey.fromBytes(e.peer_pubkey);
          try {
            this.deps.settings.peerPubkey = pk.toSshString();
          } finally {
            pk.free();
          }
          void this.deps.saveSettings(this.deps.settings);
        }
        break;
      }
      case 'disconnected':
        if (this.state !== 'idle') this.setState('reconnecting');
        break;
      case 'catchup-progress':
        // Intentionally silent — too noisy for a status-bar transition.
        break;
      case 'tree-changed':
        // When the engine carries its changeset along (the fast path),
        // apply only those paths — no whole-vault scan, no per-file
        // cross-thread `readTextFile` (issue 0010 made those O(vault)
        // postMessage round-trips per sync, the source of the live-sync
        // latency complaint). The empty-changes form still falls back to
        // the full debounced scan.
        if (e.changes && e.changes.length > 0) {
          void this.bridge?.applyTreeChanges(e.changes);
        } else {
          // Coalesce bursts — initial catch-up can fire many in a row.
          this.bridge?.scheduleApplyRemoteState();
        }
        break;
      case 'error':
        this.setState('error');
        this.deps.notice?.(`Context: ${e.message}`);
        break;
    }
  }
}

/** One-line, console-friendly summary of a VaultEvent. Lets the lifecycle
 * log show *why* a state changed without dumping the full payload. */
function eventSummary(e: VaultEvent): string {
  switch (e.kind) {
    case 'connecting':
      return ` → ${e.url}`;
    case 'connected':
      return e.peer_pubkey.length > 0 ? ' (peer key pinned)' : '';
    case 'disconnected':
      return ` (${e.reason})`;
    case 'catchup-progress':
      return e.outbound ? ' (outbound)' : ' (inbound)';
    case 'tree-changed': {
      const n = e.changes?.length ?? 0;
      return n > 0 ? ` (${n} path${n === 1 ? '' : 's'} via fast path)` : ' (no changeset)';
    }
    case 'error':
      return `: ${e.message}`;
  }
}

/** Decode an SSH-format pubkey (`[peer] pubkey`) to raw bytes for the
 * engine's pinned-peer option. Surfaces a readable error on a malformed
 * pin rather than a cryptic engine panic. */
function sshPubkeyBytes(ssh: string): Uint8Array {
  let pk: ReturnType<typeof Pubkey.fromSshString>;
  try {
    pk = Pubkey.fromSshString(ssh);
  } catch (e) {
    throw new Error(`invalid [peer] pubkey in .context/config: ${e}`);
  }
  try {
    return pk.bytes();
  } finally {
    pk.free();
  }
}

/** Outcome of [`removeDirRecursive`] — counts every path the walk touched
 * + the human-readable error string for any deletion that failed. The
 * caller (resetLocalState) logs this so a half-succeeded wipe on iOS
 * doesn't look like a clean wipe; without it, the next start picks
 * `mode=open` from the stale state file and the user can't tell why
 * (issue 0014). */
interface RemoveReport {
  filesRemoved: number;
  foldersRemoved: number;
  errors: string[];
}

/** Depth-first delete of every file + folder under `path`, then `path`
 * itself. Tolerates a missing root so callers can call it unconditionally.
 * Returns a [`RemoveReport`] — errors are RECORDED, not thrown, because a
 * partial wipe is still useful, but they must NOT be swallowed (issue
 * 0014). */
async function removeDirRecursive(fs: MinimalDataAdapter, path: string): Promise<RemoveReport> {
  const report: RemoveReport = { filesRemoved: 0, foldersRemoved: 0, errors: [] };
  if (!(await fs.exists(path))) return report;
  let children: { files: string[]; folders: string[] };
  try {
    children = await fs.list(path);
  } catch (e) {
    // Path exists but isn't listable (e.g. a stray file at the root): try to
    // remove it directly below. The list-error itself is diagnostic and
    // gets recorded.
    report.errors.push(`list(${path}): ${describeError(e)}`);
    children = { files: [], folders: [] };
  }
  for (const f of children.files) {
    try {
      await fs.remove(f);
      report.filesRemoved++;
    } catch (e) {
      report.errors.push(`remove(${f}): ${describeError(e)}`);
    }
  }
  for (const sub of children.folders) {
    const sub_report = await removeDirRecursive(fs, sub);
    report.filesRemoved += sub_report.filesRemoved;
    report.foldersRemoved += sub_report.foldersRemoved;
    report.errors.push(...sub_report.errors);
  }
  try {
    await fs.remove(path);
    report.foldersRemoved++;
  } catch (e) {
    report.errors.push(`remove(${path}): ${describeError(e)}`);
  }
  return report;
}

function describeError(e: unknown): string {
  if (e instanceof Error) return `${e.name}: ${e.message}`;
  return String(e);
}
