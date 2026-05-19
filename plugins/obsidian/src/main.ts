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
// "Configured" is gated on `[obsidian] onboarded`, NOT merely on
// `.context/config` existing. Setup writes config up front (it's the
// CLI-shared file — a partial config is harmless), but the vault only
// counts as configured once setup actually works end to end: create-mode
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

import { type IdentityInstance, initCsp, isInitialized } from '@csp/sdk/web-init';
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
import { ConfigStore, type CspSettings, DEFAULT_SETTINGS } from './settings.js';
import { StatusBar } from './status-bar.js';
import { ObsidianStorageAdapter } from './storage-adapter.js';
import { SyncController } from './sync-controller.js';

declare const __CSP_WASM_B64__: string;

/** The build-time inlined wasm token. esbuild's `define` replaces this with
 * a string literal in the bundle; under unit tests (no esbuild) the
 * identifier is absent, so `typeof` guards the ReferenceError and the SDK's
 * nodejs glue (loaded by `test/setup.ts`) provides the engine instead. */
export function inlinedWasmB64(): string {
  return typeof __CSP_WASM_B64__ === 'string' ? __CSP_WASM_B64__ : '';
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
            const connect = this.settings.autoConnectOnStart && !!this.settings.peerUrl;
            await this.controller?.start({ connect });
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
   * generates) the device identity, writes `.context/config`, then brings
   * the controller online. */
  async runSetup(opts: SetupOptions): Promise<void> {
    await this.initWasm();
    const io = this.resolveIdentityIO();
    const { identity } = await loadOrCreateIdentity(io);
    this.identity?.free();
    this.identity = identity;

    const s: CspSettings = { ...DEFAULT_SETTINGS };
    s.syncEnabled = true;
    if (opts.peerUrl) s.peerUrl = opts.peerUrl.trim();
    // Mobile keeps the key in-vault (under the excluded `.context/`); record
    // it so a `ctx` on a synced copy resolves the same file. Desktop leaves
    // it unset (CLI default `~/.context/id_ed25519`).
    if (!Platform.isDesktopApp) s.identityPath = VAULT_IDENTITY_PATH;

    // Persist `.context/config` up front (CLI-shared; partial is harmless)
    // but DON'T latch "configured" yet — `onboarded` flips only once setup
    // actually works.
    s.onboarded = false;
    this.settings = s;
    await this.configStore?.save(s); // ← creates .context/config
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

  /** Flip the master sync switch. Persists `[obsidian] sync_enabled`. */
  async setSyncEnabled(on: boolean): Promise<void> {
    if (!this.configured) return;
    this.settings.syncEnabled = on;
    await this.configStore?.save(this.settings);
    if (on) {
      await this.ensureController();
      const connect = this.settings.autoConnectOnStart && !!this.settings.peerUrl;
      await this.controller?.start({ connect });
    } else {
      await this.controller?.stop();
    }
  }

  async saveSettings(): Promise<void> {
    await this.configStore?.save(this.settings);
  }

  // ---- Internal ----

  private async initWasm(): Promise<void> {
    if (!isInitialized() && this.wasmBytes && this.wasmBytes.length > 0) {
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
      vault: this.app.vault,
      settings: this.settings,
      identity: this.identity,
      saveSettings: async (s) => {
        this.settings = s;
        await this.configStore?.save(s);
      },
      wasmBytes: this.wasmBytes,
      notice: (m) => {
        this.lastNotice = m;
        new Notice(m);
      },
      log: (m) => console.log('[context]', m),
    });
    this.controller.on((st) => this.statusBar?.set(st));
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
