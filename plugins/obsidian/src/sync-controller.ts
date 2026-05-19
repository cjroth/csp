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

export type ControllerState = 'idle' | 'connecting' | 'connected' | 'reconnecting' | 'error';

export interface ControllerDeps {
  storage: StorageAdapter;
  vault: MinimalVault;
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
   * Wipe the local thin-node object/sync state and re-open a fresh session.
   * Cleared: `.context/{state,frontier,snapshots}`. KEPT: `.context/config`
   * and the device identity (`~/.context/id_ed25519`, shared with `ctx`).
   * The Obsidian vault contents are never touched.
   *
   * NOTE (CSP §5.1): this resumes authoring under the SAME device key. If
   * that key may be live on another replica of this vault, history becomes
   * confusing; CSP prefers a fresh NodeId. The plugin cannot fork the key
   * itself — it warns.
   */
  async resetLocalState(): Promise<void> {
    await this.stop();
    // Zero-length blobs make the adapter report null → the engine treats
    // them as absent and rebuilds from config on next start().
    await this.deps.storage.saveState(new Uint8Array(0));
    await this.deps.storage.saveFrontier(new Uint8Array(0));
    await this.deps.storage.saveSnapshots(new Uint8Array(0));
    this.deps.notice?.(
      'Context: local state cleared. Resuming under the same device key — ' +
        'per CSP §5.1, prefer a fresh key if it may be active elsewhere.',
    );
    await this.prepare();
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
    const { peerUrl, peerPubkey } = this.deps.settings;
    const transport = this.deps.transport;
    const base = {
      storage: this.deps.storage,
      identity: this.deps.identity,
      ...(peerUrl ? { peerUrl } : {}),
      ...(peerPubkey ? { peerPubkey: sshPubkeyBytes(peerPubkey) } : {}),
      ...(transport ? { transport } : {}),
    };

    const existing = await this.deps.storage.loadState();
    if (existing && existing.length > 0) {
      return Vault.open(base);
    }
    if (peerUrl) {
      // CSP §17 `ctx clone <url>` — catch up + materialize from the peer.
      return Vault.clone({ ...base, peerUrl });
    }
    // Offline local create. Per CSP §7 this will NOT converge across
    // devices until a full node joins this vault — the wizard says so.
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
  }

  private onVaultEvent(e: VaultEvent): void {
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
        // Coalesce bursts — initial catch-up can fire many in a row.
        this.bridge?.scheduleApplyRemoteState();
        break;
      case 'error':
        this.setState('error');
        this.deps.notice?.(`Context: ${e.message}`);
        break;
    }
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
