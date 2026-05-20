// ContextSyncPlugin lifecycle: onload (configured / unconfigured), the
// create-vs-connect `runSetup` onboarding state machine (the gate on
// `[obsidian] onboarded`), commands, Obsidian event wiring, onunload, and
// the inlined-wasm helpers. The plugin builds a REAL SyncController over the
// real wasm engine (Bun loads the nodejs glue synchronously); only the
// device-identity IO is injected via the `identityIOOverride` seam so tests
// never touch the real `~/.context`.

import { afterEach, beforeAll, beforeEach, describe, expect, test } from 'bun:test';
import { type IdentityIO, loadOrCreateIdentity } from '../../src/identity-store.js';
import ContextSyncPlugin, { decodeInlinedWasm, inlinedWasmB64 } from '../../src/main.js';
import { ConfigStore, DEFAULT_SETTINGS } from '../../src/settings.js';
import { App, Notice, Platform, __resetObsidian } from '../mocks/obsidian-shim.js';
import { FakeDataAdapter, FakeVault } from '../mocks/obsidian.js';

const clipboardWrites: string[] = [];
beforeAll(() => {
  try {
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: { clipboard: { writeText: async (s: string) => void clipboardWrites.push(s) } },
    });
  } catch {
    // biome-ignore lint/suspicious/noExplicitAny: last-resort stub
    (globalThis as any).navigator = {
      clipboard: { writeText: async (s: string) => void clipboardWrites.push(s) },
    };
  }
});

/** In-memory IdentityIO — keeps the device key out of the real ~/.context. */
class MemIO implements IdentityIO {
  body: string | null = null;
  pub: string | null = null;
  async read() {
    return this.body;
  }
  async write(body: string, pub: string) {
    this.body = body;
    this.pub = pub;
  }
  describe() {
    return '<mem>';
  }
}

const MANIFEST = { id: 'context-sync', name: 'Context', version: '0.1.0' };
const tick = (ms = 0) => new Promise((r) => setTimeout(r, ms));

function makePluginWith(adapter: FakeDataAdapter, io: IdentityIO) {
  const vault = new FakeVault(adapter);
  const app = new App(vault);
  // biome-ignore lint/suspicious/noExplicitAny: shim App vs real obsidian type
  const plugin = new ContextSyncPlugin(app as any, MANIFEST as any);
  plugin.identityIOOverride = io;
  return { plugin, app, vault, adapter };
}

function makePlugin(adapter = new FakeDataAdapter()) {
  return makePluginWith(adapter, new MemIO());
}

beforeEach(() => {
  __resetObsidian();
  Platform.isDesktopApp = true;
  clipboardWrites.length = 0;
});
afterEach(async () => {
  __resetObsidian();
});

describe('inlined-wasm helpers', () => {
  test('inlinedWasmB64() is empty under tests (no esbuild define)', () => {
    expect(inlinedWasmB64()).toBe('');
  });
  test('decodeInlinedWasm("") → empty; base64 → bytes round-trip', () => {
    expect(decodeInlinedWasm('').length).toBe(0);
    const b64 = btoa(String.fromCharCode(1, 2, 3, 250));
    expect(Array.from(decodeInlinedWasm(b64))).toEqual([1, 2, 3, 250]);
  });
});

describe('missing WebAssembly (iOS Lockdown Mode, ancient Android WebView)', () => {
  test('runSetup surfaces an actionable error instead of ReferenceError', async () => {
    const { plugin } = makePlugin();
    await plugin.onload();
    // Simulate a WebView without WebAssembly. The plugin can't init the
    // engine without it — we want a clear error pointing at the cause.
    const saved = (globalThis as { WebAssembly?: unknown }).WebAssembly;
    delete (globalThis as { WebAssembly?: unknown }).WebAssembly;
    try {
      await expect(plugin.runSetup({ mode: 'create' })).rejects.toThrow(
        /WebAssembly.*Lockdown Mode|Android System WebView/,
      );
      expect(plugin.isConfigured()).toBe(false);
    } finally {
      (globalThis as { WebAssembly?: unknown }).WebAssembly = saved;
    }
    await plugin.onunload();
  });
});

describe('onload — unconfigured', () => {
  test('registers commands, status bar idle, stays unconfigured', async () => {
    const { plugin } = makePlugin();
    await plugin.onload();
    expect(plugin.isConfigured()).toBe(false);
    // biome-ignore lint/suspicious/noExplicitAny: shim test helpers
    const ids = (plugin as any).__commandIds();
    expect(ids).toEqual(
      expect.arrayContaining([
        'context-connect',
        'context-disconnect',
        'context-resync',
        'context-copy-pubkey',
        'context-create-snapshot',
      ]),
    );
    // biome-ignore lint/suspicious/noExplicitAny: shim test helpers
    expect((plugin as any).__lastStatusBarItem()?.getText()).toBe('Context: idle');
  });

  test('the status-bar item opens the plugin settings on click', async () => {
    const { plugin, app } = makePlugin();
    const opened: string[] = [];
    app.setting = { open: () => opened.push('open'), openTabById: (id: string) => opened.push(id) };
    await plugin.onload();
    // biome-ignore lint/suspicious/noExplicitAny: shim test helpers
    (plugin as any).__lastStatusBarItem()?.__click();
    expect(opened).toEqual(['open', 'context-sync']);
  });

  test('commands warn that the vault is not set up yet', async () => {
    const { plugin } = makePlugin();
    await plugin.onload();
    // biome-ignore lint/suspicious/noExplicitAny: shim test helpers
    (plugin as any).__invoke('context-connect');
    // biome-ignore lint/suspicious/noExplicitAny: shim test helpers
    (plugin as any).__invoke('context-copy-pubkey');
    await tick();
    expect(Notice.log.some((m) => /not set up yet/.test(m))).toBe(true);
  });
});

describe('runSetup — create mode', () => {
  test('builds a local vault, latches onboarded, writes the sidecar', async () => {
    const { plugin, adapter } = makePlugin();
    await plugin.onload();
    await plugin.runSetup({ mode: 'create' });

    expect(plugin.isConfigured()).toBe(true);
    expect(plugin.settings.onboarded).toBe(true);
    expect(plugin.controller).not.toBeNull();
    expect(plugin.controller?.state).toBe('idle'); // create mode never connects
    const side = JSON.parse(await adapter.read('.context/obsidian.json'));
    expect(side.syncEnabled).toBe(true);
    expect(side.onboarded).toBe(true);
    // Identity was generated through the injected IO, not ~/.context.
    expect((plugin.identityIOOverride as MemIO).body?.trim()).toMatch(/^[0-9a-f]{64}$/);
    await plugin.onunload();
  });
});

describe('runSetup — connect mode failure', () => {
  test('a refused peer keeps the vault unconfigured, surfaces the error', async () => {
    const { plugin, adapter } = makePlugin();
    await plugin.onload();
    await expect(
      plugin.runSetup({ mode: 'connect', peerUrl: 'ws://127.0.0.1:1' }),
    ).rejects.toBeDefined();

    expect(plugin.isConfigured()).toBe(false);
    expect(plugin.onboardingError).not.toBeNull();
    expect(plugin.controller).toBeNull();
    // Plugin-owned state (the peer URL) is captured in the node-local
    // sidecar up front even though the canonical .context/config is only
    // created once the engine builds the vault.
    expect(await adapter.exists('.context/obsidian.json')).toBe(true);
  });
});

describe('configured onload + event wiring', () => {
  async function configuredPlugin() {
    const adapter = new FakeDataAdapter();
    // Pre-seed: a device key in the IO + an onboarded .context/config.
    const io = new MemIO();
    const seeded = await loadOrCreateIdentity(io);
    seeded.identity.free();
    await new ConfigStore(adapter).save({
      ...DEFAULT_SETTINGS,
      syncEnabled: true,
      onboarded: true,
    });
    const { plugin, app, vault } = makePlugin(adapter);
    plugin.identityIOOverride = io;
    await plugin.onload();
    expect(plugin.isConfigured()).toBe(true);
    app.flushLayoutReady();
    // Let the deferred ensureController/start IIFE settle.
    for (let i = 0; i < 20 && !plugin.controller; i++) await tick(10);
    return { plugin, vault, adapter };
  }

  test('builds + starts the controller offline (no auto-connect)', async () => {
    const { plugin } = await configuredPlugin();
    expect(plugin.controller).not.toBeNull();
    expect(plugin.controller?.state).toBe('idle');
    await plugin.onunload();
    expect(plugin.controller).toBeNull();
  });

  test('create / modify / delete / rename events all reach the engine', async () => {
    const { plugin, vault } = await configuredPlugin();
    const bridge = plugin.controller?.getBridge();

    const f = await vault.create('note.md', '# hello\n');
    await tick(20);
    expect(bridge?.pushed).toBeGreaterThanOrEqual(1);

    await vault.modify(f, '# hello edited\n');
    await tick(20);
    const g = await vault.create('tmp.md', 'temp\n');
    await tick(20);
    await vault.rename(g, 'renamed.md');
    await tick(20);
    await vault.delete(f);
    await tick(20);
    // All four event handlers ran without an unhandled rejection.
    expect(bridge?.pushed).toBeGreaterThanOrEqual(3);

    // The configured `context-connect` command path.
    // biome-ignore lint/suspicious/noExplicitAny: shim test helper
    (plugin as any).__invoke('context-connect');
    await tick(20);
    expect(Notice.log.some((m) => /not set up yet/.test(m))).toBe(false);

    // The HARD INVARIANT: nothing under .context/ leaks into vault content.
    expect(vault.getFiles().some((x) => x.path.startsWith('.context'))).toBe(false);
    await plugin.onunload();
  });

  test('saveSettings() writes plugin settings to the node-local sidecar', async () => {
    const { plugin, adapter } = await configuredPlugin();
    plugin.settings.ignoreGlobs = ['Drafts/**'];
    await plugin.saveSettings();
    const side = JSON.parse(await adapter.read('.context/obsidian.json'));
    expect(side.ignoreGlobs).toContain('Drafts/**');
    await plugin.onunload();
  });
});

describe('runSetup — connect mode, peer unreachable', () => {
  test('pre-seeded state opens locally then waitForConnect rejects on error', async () => {
    const adapter = new FakeDataAdapter();
    const io = new MemIO();
    // Plugin 1, create mode: persists .context/state + the device key.
    const p1 = makePluginWith(adapter, io);
    await p1.plugin.onload();
    await p1.plugin.runSetup({ mode: 'create' });
    await p1.plugin.onunload();

    // Plugin 2, same adapter + identity: openOrCreate takes Vault.open (no
    // network throw); connect mode with an unreachable peer → the reconnect
    // loop emits 'error' → waitForConnect rejects → runSetup rejects.
    const p2 = makePluginWith(adapter, io);
    await p2.plugin.onload();
    await expect(
      p2.plugin.runSetup({ mode: 'connect', peerUrl: 'ws://127.0.0.1:1' }),
    ).rejects.toBeDefined();
    expect(p2.plugin.isConfigured()).toBe(false);
    expect(p2.plugin.onboardingError).not.toBeNull();
    expect(p2.plugin.controller).toBeNull();
  }, 20_000);
});

describe('setSyncEnabled + commands (configured)', () => {
  async function ready() {
    const { plugin, vault, adapter } = makePlugin();
    await plugin.onload();
    await plugin.runSetup({ mode: 'create' });
    return { plugin, vault, adapter };
  }

  test('toggling sync off stops, back on restarts; persisted to the sidecar', async () => {
    const { plugin, adapter } = await ready();
    await plugin.setSyncEnabled(false);
    expect(plugin.settings.syncEnabled).toBe(false);
    const off = JSON.parse(await adapter.read('.context/obsidian.json'));
    expect(off.syncEnabled ?? false).toBe(false);
    await plugin.setSyncEnabled(true);
    expect(plugin.settings.syncEnabled).toBe(true);
    expect(JSON.parse(await adapter.read('.context/obsidian.json')).syncEnabled).toBe(true);
    await plugin.onunload();
  });

  test('copy-pubkey, create-snapshot, resync, disconnect commands', async () => {
    const { plugin } = await ready();

    // biome-ignore lint/suspicious/noExplicitAny: shim test helpers
    (plugin as any).__invoke('context-copy-pubkey');
    await tick();
    expect(clipboardWrites.some((s) => s.startsWith('ssh-ed25519 '))).toBe(true);
    expect(Notice.log).toContain('Public key copied to clipboard.');

    // biome-ignore lint/suspicious/noExplicitAny: shim test helpers
    (plugin as any).__invoke('context-create-snapshot');
    await tick(20);
    expect(Notice.log.some((m) => /^Snapshot created: snapshot-/.test(m))).toBe(true);

    // biome-ignore lint/suspicious/noExplicitAny: shim test helpers
    (plugin as any).__invoke('context-resync');
    await tick(20);
    expect(Notice.log).toContain('Context: resynced.');

    // biome-ignore lint/suspicious/noExplicitAny: shim test helpers
    (plugin as any).__invoke('context-disconnect');
    await tick(20);
    expect(plugin.controller?.state).toBe('idle');

    await plugin.onunload();
  });
});

describe('mobile identity path', () => {
  test('non-desktop runSetup records the identity path in the sidecar', async () => {
    Platform.isDesktopApp = false;
    const { plugin, adapter } = makePlugin();
    await plugin.onload();
    await plugin.runSetup({ mode: 'create' });
    expect(plugin.settings.identityPath).toBe('.context/id_ed25519');
    expect(JSON.parse(await adapter.read('.context/obsidian.json')).identityPath).toBe(
      '.context/id_ed25519',
    );
    await plugin.onunload();
  });
});

describe('runSetup normalizes the peer URL', () => {
  test('a bare domain becomes `wss://host:443` so the WebSocket can dial it', async () => {
    const { plugin, adapter } = makePlugin();
    await plugin.onload();
    // The URL is unreachable, so connect-mode setup will reject — but the
    // normalized URL is persisted to the sidecar *before* the engine tries
    // to dial, which is all we want to observe here.
    await plugin.runSetup({ mode: 'connect', peerUrl: 'sync.example.com' }).catch(() => {});
    expect(plugin.settings.peerUrl).toBe('wss://sync.example.com:443');
    const side = JSON.parse(await adapter.read('.context/obsidian.json'));
    expect(side.peerUrl).toBe('wss://sync.example.com:443');
    await plugin.onunload();
  });
});

describe('plugin.resetLocalState', () => {
  test('wipes .context/, frees identity, returns to the unconfigured state', async () => {
    const { plugin, adapter } = makePlugin();
    await plugin.onload();
    await plugin.runSetup({ mode: 'create' });
    expect(plugin.isConfigured()).toBe(true);
    expect(await adapter.exists('.context')).toBe(true);

    await plugin.resetLocalState();

    expect(plugin.isConfigured()).toBe(false);
    expect(plugin.controller).toBeNull();
    expect(plugin.settings.syncEnabled).toBe(false);
    expect(plugin.settings.onboarded).toBe(false);
    expect(await adapter.exists('.context')).toBe(false);
    expect(await adapter.exists('.context/obsidian.json')).toBe(false);
    await plugin.onunload();
  });

  test('on desktop, the home-dir device key survives a reset and is re-used on next setup', async () => {
    Platform.isDesktopApp = true;
    const adapter = new FakeDataAdapter();
    const io = new MemIO();
    const { plugin } = makePluginWith(adapter, io);
    await plugin.onload();
    await plugin.runSetup({ mode: 'create' });
    const keyBefore = io.body;
    expect(keyBefore).not.toBeNull();

    await plugin.resetLocalState();
    // The home-dir key was NOT deleted by the in-vault wipe.
    expect(io.body).toBe(keyBefore);

    // Next setup picks up the same key — the same SSH pubkey comes back.
    await plugin.runSetup({ mode: 'create' });
    expect(io.body).toBe(keyBefore);
    await plugin.onunload();
  });
});
