// Plugin entry point. Owns the SyncController lifecycle, registers
// commands, the settings tab, the status bar, and the Obsidian event
// listeners that drive Obsidian → engine pushes.
//
// Lifecycle:
//   unconfigured ──runSetup()──┬─(succeeds)─> configured ──sync on/off──
//                              └─(fails)────> stays unconfigured; the
//                                  wizard stays up, pre-filled, with the
//                                  error so the user can fix and retry.
//
// "Configured" is gated on the sidecar's `onboarded` flag, NOT merely on
// the node-local sidecar existing. Setup writes the sidecar up front (peer
// URL + plugin knobs), but the vault only counts as configured once setup
// actually works end to end: create-mode
// latches as soon as the local vault is built; connect-mode only after the
// peer handshake + first catch-up succeeds (the exact step where an
// unauthorized device key fails). Deleting `.context/` returns the plugin
// to the unconfigured state rather than silently regenerating anything.
//
// The real csp-core wasm bytes are inlined at build time by esbuild via the
// `__CSP_WASM_B64__` define — no fetch at runtime, the only way mobile
// WebViews reliably load WebAssembly. `initCsp(bytes)` instantiates the same
// Rust engine `ctx` runs (one engine everywhere — CSP spec.md §16); the
// plugin then computes its own byte-identical `main`.

import {
  type FromWorker,
  type IdentityInstance,
  type ToWorker,
  WorkerVault,
  initCsp,
  isInitialized,
  workerPort,
} from '@csp/sdk/web-init';
import { Notice, Platform, Plugin, type TAbstractFile, type TFile } from 'obsidian';
import {
  type IdentityIO,
  NodeHomeIdentityIO,
  VAULT_IDENTITY_PATH,
  VaultAdapterIdentityIO,
  loadIdentity,
  loadOrCreateIdentity,
} from './identity-store.js';
import { ContextSyncSettingTab } from './settings-tab.js';
import { ConfigStore, type CspSettings, DEFAULT_SETTINGS, normalizePeerUrl } from './settings.js';
import { StatusBar } from './status-bar.js';
import { ObsidianStorageAdapter } from './storage-adapter.js';
import { SyncController, type VaultSpec } from './sync-controller.js';

declare const __CSP_WASM_B64__: string;
/** Build-time inlined source of the engine Web Worker (issue 0010). esbuild
 * replaces this with the phase-1 bundle string; absent under unit tests. */
declare const __ENGINE_WORKER_SRC__: string;

/** The build-time inlined wasm token. esbuild's `define` replaces this with
 * a string literal in the bundle; under unit tests (no esbuild) the
 * identifier is absent, so `typeof` guards the ReferenceError and the SDK's
 * nodejs glue (loaded by `test/setup.ts`) provides the engine instead. */
export function inlinedWasmB64(): string {
  return typeof __CSP_WASM_B64__ === 'string' ? __CSP_WASM_B64__ : '';
}

/** The inlined engine-worker bundle, or '' under unit tests. */
function inlinedWorkerSrc(): string {
  return typeof __ENGINE_WORKER_SRC__ === 'string' ? __ENGINE_WORKER_SRC__ : '';
}

/** Decode the build-time inlined csp-core wasm (base64) into a Uint8Array
 * for `initCsp()`. esbuild errors at build time if the wasm is missing. */
export function decodeInlinedWasm(b64: string): Uint8Array {
  if (!b64) return new Uint8Array(0);
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export interface SetupOptions {
  mode: 'create' | 'connect';
  /** Peer (full-node listener) URL. Required for connect; optional for
   * create (CSP §7 — a local vault won't converge without a peer). */
  peerUrl?: string;
  /** Pre-shared bearer auth-key (CSP §10). Sent on the WebSocket upgrade
   * so the listener — when it has `CTX_AUTH_KEY` set — enrolls this
   * device's pubkey into its `authorized_keys`. Optional: omit when
   * the listener is in TOFU mode or this device is already enrolled. */
  authKey?: string;
}

export default class ContextSyncPlugin extends Plugin {
  settings: CspSettings = { ...DEFAULT_SETTINGS };
  controller: SyncController | null = null;
  statusBar: StatusBar | null = null;
  /** True once setup completed end to end (`[obsidian] onboarded`). */
  configured = false;
  /** Last setup failure, surfaced in the wizard. Cleared on success. */
  onboardingError: string | null = null;
  /** Test seam: when set, `resolveIdentityIO()` returns this instead of the
   * platform default (which writes to the real `~/.context` on desktop).
   * Production never sets it. */
  identityIOOverride: IdentityIO | null = null;

  private configStore: ConfigStore | null = null;
  private storage: ObsidianStorageAdapter | null = null;
  private wasmBytes: Uint8Array | null = null;
  private identity: IdentityInstance | null = null;
  private lastNotice: string | null = null;

  override async onload(): Promise<void> {
    this.configStore = new ConfigStore(this.app.vault.adapter);
    this.storage = new ObsidianStorageAdapter(this.app.vault.adapter);
    this.wasmBytes = decodeInlinedWasm(inlinedWasmB64());

    const statusEl = this.addStatusBarItem();
    this.statusBar = new StatusBar(statusEl);
    this.statusBar.onClick(() => {
      // biome-ignore lint/suspicious/noExplicitAny: app.setting is private API.
      (this.app as any).setting?.open?.();
      // biome-ignore lint/suspicious/noExplicitAny: ditto.
      (this.app as any).setting?.openTabById?.(this.manifest.id);
    });

    this.addSettingTab(new ContextSyncSettingTab(this.app, this));
    this.registerObsidianEventListeners();
    this.registerCommands();

    if (await this.configStore.exists()) {
      this.settings = await this.configStore.load();
    }
    this.configured = this.settings.onboarded === true;
    if (!this.configured) {
      this.statusBar.set('idle');
      return;
    }

    // Defer the controller start — and the reconcile it triggers — until
    // Obsidian has finished restoring its workspace, so the metadata cache
    // is warm and reconcile doesn't try to re-create files that exist.
    this.app.workspace.onLayoutReady(() => {
      void (async () => {
        try {
          await this.ensureController();
          if (this.settings.syncEnabled) {
            await this.controller?.start({ connect: !!this.settings.peerUrl });
          }
        } catch (err) {
          console.error('[context] start failed:', err);
          new Notice(`Context: ${err}`);
        }
      })();
    });
  }

  override async onunload(): Promise<void> {
    await this.controller?.stop();
    this.controller = null;
    this.identity?.free();
    this.identity = null;
  }

  // ---- Setup / lifecycle API (used by the settings tab) ----

  isConfigured(): boolean {
    return this.configured;
  }

  /** The one and only path that creates `<vault>/.context/`. Resolves (or
   * generates) the device identity, writes the node-local plugin sidecar
   * (and mirrors the peer into `.context/config` when the engine has
   * created it), then brings the controller online. */
  async runSetup(opts: SetupOptions): Promise<void> {
    await this.initWasm();
    const io = this.resolveIdentityIO();
    const { identity } = await loadOrCreateIdentity(io);
    this.identity?.free();
    this.identity = identity;

    const s: CspSettings = { ...DEFAULT_SETTINGS };
    s.syncEnabled = true;
    if (opts.peerUrl) s.peerUrl = normalizePeerUrl(opts.peerUrl);
    // Auth-key: the listener's WS upgrade reads this as a bearer token and
    // (on match) enrolls our device pubkey into its `authorized_keys`.
    // Take it from opts when the wizard passes one explicitly; fall back to
    // whatever is already in-memory (the wizard's auth-key field binds
    // directly to `plugin.settings.authKey` so re-runs after a partial
    // setup don't lose it).
    if (opts.authKey !== undefined) s.authKey = opts.authKey.trim();
    else if (this.settings.authKey) s.authKey = this.settings.authKey;
    // Mobile keeps the key in-vault (under the excluded `.context/`); record
    // it so a `ctx` on a synced copy resolves the same file. Desktop leaves
    // it unset (CLI default `~/.context/id_ed25519`).
    if (!Platform.isDesktopApp) s.identityPath = VAULT_IDENTITY_PATH;

    // Persist the plugin sidecar up front (peer URL + knobs) but DON'T
    // latch "configured" yet — `onboarded` flips only once setup actually
    // works.
    s.onboarded = false;
    this.settings = s;
    await this.configStore?.save(s); // ← writes the node-local sidecar
    this.configured = false;
    this.onboardingError = null;

    this.buildController();
    try {
      await this.controller?.start({ connect: !!s.peerUrl });
      // Connect-mode isn't done until the peer handshake + first catch-up
      // succeeds — where an unauthorized device key fails. Create-mode is
      // done as soon as the local vault exists.
      if (opts.mode === 'connect') await this.waitForConnect();
      await this.completeOnboarding();
    } catch (err) {
      this.onboardingError = String(err);
      await this.controller?.stop().catch(() => {});
      this.controller = null;
      throw err;
    }
  }

  /** Latch the vault as fully configured. Idempotent. */
  private async completeOnboarding(): Promise<void> {
    this.settings.onboarded = true;
    await this.configStore?.save(this.settings);
    this.configured = true;
    this.onboardingError = null;
  }

  /** Resolve once the controller reaches `connected`; reject on `error` or
   * after `timeoutMs`. Makes connect-mode setup synchronous. */
  private waitForConnect(timeoutMs = 30_000): Promise<void> {
    const c = this.controller;
    if (!c) return Promise.reject(new Error('controller not built'));
    if (c.state === 'connected') return Promise.resolve();
    return new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => {
        off();
        reject(new Error('timed out waiting for the peer connection'));
      }, timeoutMs);
      const off = c.on((st) => {
        if (st === 'connected') {
          clearTimeout(timer);
          off();
          resolve();
        } else if (st === 'error') {
          clearTimeout(timer);
          off();
          reject(new Error(this.lastNotice ?? 'connection failed'));
        }
      });
    });
  }

  /** Wipe `.context/` (engine state + shared config + plugin sidecar + the
   * in-vault device key on mobile) and return the plugin to its unconfigured
   * state. Desktop keeps its key (it lives in `~/.context/`, outside the
   * vault); on next setup the same key is re-used. */
  async resetLocalState(): Promise<void> {
    await this.controller?.resetLocalState();
    this.controller = null;
    this.identity?.free();
    this.identity = null;
    this.settings = { ...DEFAULT_SETTINGS };
    this.configured = false;
    this.onboardingError = null;
    this.statusBar?.set('idle');
  }

  /** Flip the master sync switch. Persists `syncEnabled` to the sidecar. */
  async setSyncEnabled(on: boolean): Promise<void> {
    if (!this.configured) return;
    this.settings.syncEnabled = on;
    await this.configStore?.save(this.settings);
    if (on) {
      await this.ensureController();
      await this.controller?.start({ connect: !!this.settings.peerUrl });
    } else {
      await this.controller?.stop();
    }
  }

  async saveSettings(): Promise<void> {
    await this.configStore?.save(this.settings);
  }

  // ---- Internal ----

  private async initWasm(): Promise<void> {
    // The engine is a Rust crate compiled to wasm — there is no JS fallback.
    // The most common reasons WebAssembly is unavailable in a host that
    // otherwise runs the plugin: iOS Lockdown Mode (turns off JIT, kills
    // Wasm) and very old Android WebViews. Surface that up front so the
    // user gets an actionable message instead of `ReferenceError`. We
    // assert this even when `isInitialized()` is true so a host that lost
    // the runtime mid-session (or a test that simulates the missing global)
    // still gets the readable error.
    if (typeof WebAssembly === 'undefined') {
      throw new Error(
        "this device's WebView doesn't support WebAssembly. On iOS, " +
          'turn off Lockdown Mode (Settings → Privacy & Security → ' +
          'Lockdown Mode) and reopen Obsidian. On Android, update the ' +
          'system WebView (Google Play → Android System WebView).',
      );
    }
    if (isInitialized()) return;
    if (this.wasmBytes && this.wasmBytes.length > 0) {
      await initCsp(this.wasmBytes);
    }
  }

  private resolveIdentityIO(): IdentityIO {
    if (this.identityIOOverride) return this.identityIOOverride;
    return Platform.isDesktopApp
      ? new NodeHomeIdentityIO()
      : new VaultAdapterIdentityIO(this.app.vault.adapter);
  }

  /** Load the (already-created) identity and construct the controller.
   * Throws if configured but the identity is missing — we never silently
   * regenerate a key; the user must re-run setup. */
  private async ensureController(): Promise<void> {
    if (this.controller) return;
    await this.initWasm();
    if (!this.identity) {
      const io = this.resolveIdentityIO();
      const identity = await loadIdentity(io);
      if (!identity) {
        throw new Error(
          `device identity not found at ${io.describe()} — run Context setup again to generate one`,
        );
      }
      this.identity = identity;
    }
    this.buildController();
  }

  private buildController(): void {
    if (this.controller || !this.storage || !this.identity || !this.wasmBytes) return;
    this.controller = new SyncController({
      storage: this.storage,
      fs: this.app.vault.adapter,
      vault: this.app.vault,
      settings: this.settings,
      identity: this.identity,
      saveSettings: async (s) => {
        this.settings = s;
        await this.configStore?.save(s);
      },
      wasmBytes: this.wasmBytes,
      // Run the wasm engine in a Web Worker (issue 0010) so the heavy
      // synchronous engine work never freezes the Obsidian UI. Skipped
      // under unit tests (no `__ENGINE_WORKER_SRC__`, no `Worker`) — those
      // use `sdkOverride` or the in-process default.
      ...(inlinedWorkerSrc() && typeof Worker !== 'undefined'
        ? { makeVault: (spec: VaultSpec) => this.makeWorkerVault(spec) }
        : {}),
      notice: (m) => {
        this.lastNotice = m;
        new Notice(m);
      },
      log: (m) => console.log('[context]', m),
    });
    this.controller.on((st) => this.statusBar?.set(st));
  }

  /** Spin up an engine Web Worker and wrap it in a `WorkerVault` (issue
   * 0010). The worker runs the wasm engine + transport off the renderer
   * thread; storage is proxied back here (`app.vault.adapter` is
   * main-thread-only). The worker is started from an inlined Blob so the
   * plugin still ships one `main.js`. */
  private async makeWorkerVault(spec: VaultSpec): Promise<WorkerVault> {
    if (!this.storage || !this.identity || !this.wasmBytes) {
      throw new Error('Context: engine worker prerequisites missing');
    }
    console.log('[context] spawning engine worker', { mode: spec.mode, peerUrl: spec.peerUrl });
    const blob = new Blob([inlinedWorkerSrc()], { type: 'application/javascript' });
    const url = URL.createObjectURL(blob);
    let worker: Worker;
    try {
      worker = new Worker(url);
    } finally {
      // The Worker keeps its own reference to the script; the object URL
      // can be released immediately once construction is under way.
      URL.revokeObjectURL(url);
    }
    // Surface worker-side failures — without these, an uncaught error inside
    // the worker (wasm init, identity, transport, anything) is invisible to
    // the host and the only symptom is a vault that never seems to sync.
    worker.onerror = (e) => {
      const msg = `engine worker error: ${e.message || '(no message)'}${
        e.filename ? ` @ ${e.filename}:${e.lineno}:${e.colno}` : ''
      }`;
      console.error('[context]', msg, e);
      this.lastNotice = msg;
      new Notice(`Context: ${msg}`);
    };
    worker.onmessageerror = (e) => {
      console.error('[context] engine worker message decoding error', e);
    };
    const port = workerPort<ToWorker, FromWorker>(worker);
    const v = await WorkerVault.start(port, this.storage, {
      mode: spec.mode,
      seed: this.identity.seed(),
      wasmBytes: this.wasmBytes,
      ...(spec.peerUrl ? { peerUrl: spec.peerUrl } : {}),
      ...(spec.peerPubkey ? { peerPubkey: spec.peerPubkey } : {}),
      ...(spec.authKey ? { authKey: spec.authKey } : {}),
    });
    console.log('[context] engine worker ready', {
      filesAtBoot: v.listFiles().length,
      identity: `${v.identityPubkeySsh().slice(0, 24)}…`,
    });
    return v;
  }

  private registerCommands(): void {
    this.addCommand({
      id: 'context-connect',
      name: 'Connect to peer',
      callback: () => {
        if (!this.configured) {
          new Notice('Context: not set up yet — open settings to configure.');
          return;
        }
        void this.controller?.start();
      },
    });
    this.addCommand({
      id: 'context-disconnect',
      name: 'Disconnect from peer',
      callback: () => {
        void this.controller?.stop();
      },
    });
    this.addCommand({
      id: 'context-resync',
      name: 'Resync now',
      callback: () => {
        void this.controller?.resyncNow();
      },
    });
    this.addCommand({
      id: 'context-copy-pubkey',
      name: 'Copy device public key',
      callback: async () => {
        const ssh = this.controller?.identityPubkeySsh();
        if (!ssh) {
          new Notice('Context: not set up yet.');
          return;
        }
        await navigator.clipboard.writeText(ssh);
        new Notice('Public key copied to clipboard.');
      },
    });
    this.addCommand({
      id: 'context-create-snapshot',
      name: 'Create snapshot',
      callback: async () => {
        if (!this.controller) {
          new Notice('Context: not set up yet.');
          return;
        }
        const name = `snapshot-${new Date().toISOString().replace(/[:.]/g, '-')}`;
        await this.controller.createSnapshot(name);
        new Notice(`Snapshot created: ${name}`);
      },
    });
  }

  /** Run a bridge handler from a vault-event callback, swallowing+logging
   * any rejection. Without this a benign race (e.g. reading a file Obsidian
   * just deleted) would surface as an unhandled promise rejection. */
  private dispatch(what: string, p: Promise<void> | undefined): void {
    p?.catch((err) => console.error(`[context] ${what} handler failed:`, err));
  }

  private registerObsidianEventListeners(): void {
    this.registerEvent(
      this.app.vault.on('create', (file: TAbstractFile) => {
        this.dispatch('create', this.controller?.getBridge()?.handleObsidianWrite(file));
      }),
    );
    this.registerEvent(
      this.app.vault.on('modify', (file: TAbstractFile) => {
        this.dispatch('modify', this.controller?.getBridge()?.handleObsidianWrite(file));
      }),
    );
    this.registerEvent(
      this.app.vault.on('delete', (file: TAbstractFile) => {
        this.dispatch('delete', this.controller?.getBridge()?.handleObsidianDelete(file));
      }),
    );
    this.registerEvent(
      this.app.vault.on('rename', (file: TAbstractFile, oldPath: string) => {
        this.dispatch('rename', this.controller?.getBridge()?.handleObsidianRename(file, oldPath));
      }),
    );
  }
}

// Re-export for tests.
export type { TFile };
