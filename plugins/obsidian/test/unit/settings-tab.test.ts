// ContextSyncSettingTab — the connect-vs-create setup wizard and the
// configured settings view. The tab is pure UI orchestration; the
// obsidian-shim records every Setting/component so we can drive the real
// production handlers and assert what they call.

import { beforeAll, beforeEach, describe, expect, test } from 'bun:test';
import { ContextSyncSettingTab } from '../../src/settings-tab.js';
import { DEFAULT_SETTINGS } from '../../src/settings.js';
import {
  Notice,
  type RecordedComponent,
  __obsidian,
  __resetObsidian,
} from '../mocks/obsidian-shim.js';

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

interface RunSetupCall {
  mode: 'create' | 'connect';
  peerUrl?: string;
}

function makeController(over: Record<string, unknown> = {}) {
  const calls: string[] = [];
  let onCb: (() => void) | null = null;
  const ctl = {
    state: 'idle' as string,
    snapshots: [] as Array<{ name: string; created_at_ms: number; frontier: string[] }>,
    on(cb: () => void) {
      onCb = cb;
      calls.push('on');
      return () => {
        onCb = null;
        calls.push('unsub');
      };
    },
    fireStateChange() {
      onCb?.();
    },
    identityPubkeySsh: () => 'ssh-ed25519 AAAAFAKEKEY device',
    listSnapshots() {
      return ctl.snapshots;
    },
    async stop() {
      calls.push('stop');
    },
    async prepare() {
      calls.push('prepare');
    },
    async start(o?: { connect?: boolean }) {
      calls.push(`start:${o?.connect ?? ''}`);
    },
    async resyncNow() {
      calls.push('resyncNow');
    },
    async resetLocalState() {
      calls.push('resetLocalState');
    },
    async createSnapshot(n: string) {
      calls.push(`createSnapshot:${n}`);
    },
    async restoreToSnapshot(n: string) {
      calls.push(`restoreToSnapshot:${n}`);
    },
    calls,
    ...over,
  };
  return ctl;
}

function makePlugin(over: Record<string, unknown> = {}) {
  const runSetupCalls: RunSetupCall[] = [];
  const saved: number[] = [];
  const resets: number[] = [];
  const plugin = {
    settings: { ...DEFAULT_SETTINGS },
    onboardingError: null as string | null,
    controller: null as ReturnType<typeof makeController> | null,
    _configured: false,
    isConfigured() {
      return plugin._configured;
    },
    runSetupImpl: async (_o: RunSetupCall) => {},
    async runSetup(o: RunSetupCall) {
      runSetupCalls.push(o);
      await plugin.runSetupImpl(o);
    },
    async setSyncEnabled(v: boolean) {
      plugin.settings.syncEnabled = v;
      saved.push(1);
    },
    async saveSettings() {
      saved.push(1);
    },
    async resetLocalState() {
      resets.push(1);
    },
    runSetupCalls,
    saved,
    resets,
    ...over,
  };
  return plugin;
}

function makeTab(plugin: unknown) {
  // biome-ignore lint/suspicious/noExplicitAny: shim App/plugin structural cast
  return new ContextSyncSettingTab({} as any, plugin as any);
}

beforeEach(() => __resetObsidian());

describe('setup wizard (unconfigured)', () => {
  test('defaults to connect mode with a peer URL field', () => {
    const tab = makeTab(makePlugin());
    tab.display();
    expect(__obsidian.hasText("isn't syncing yet")).toBe(true);
    const mode = __obsidian.setting('Setup mode');
    const dd = mode?.components.find((c) => c.kind === 'dropdown');
    expect(dd?.options).toEqual([
      ['connect', 'Connect to a peer'],
      ['create', 'Create a new local vault'],
    ]);
    expect(dd?.selected).toBe('connect');
    expect(__obsidian.setting('Peer URL')?.desc).toMatch(/bare domain assumes/);
  });

  test('switching to create mode shows the no-sync warning + optional URL', () => {
    const tab = makeTab(makePlugin());
    tab.display();
    const dd = __obsidian.setting('Setup mode')?.components.find((c) => c.kind === 'dropdown');
    dd?.onChangeSelect?.('create'); // re-renders synchronously
    expect(__obsidian.hasText("won't sync")).toBe(true);
    expect(__obsidian.setting('Peer URL')?.desc).toContain('Optional');
  });

  test('connect submit with an empty Peer URL is rejected before runSetup', async () => {
    const plugin = makePlugin();
    const tab = makeTab(plugin);
    tab.display();
    const submit = __obsidian.button('Set up Context');
    await submit?.onClick?.();
    expect(Notice.log.some((m) => /Peer URL is required/.test(m))).toBe(true);
    expect(plugin.runSetupCalls).toEqual([]);
  });

  test('connect submit calls runSetup with the entered URL and reports success', async () => {
    const plugin = makePlugin();
    const tab = makeTab(plugin);
    tab.display();
    const url = __obsidian.setting('Peer URL')?.components.find((c) => c.kind === 'text');
    url?.onChangeText?.('wss://node:7777');
    const submit = __obsidian.button('Set up Context');
    await submit?.onClick?.();
    expect(plugin.runSetupCalls).toEqual([{ mode: 'connect', peerUrl: 'wss://node:7777' }]);
    expect(Notice.log).toContain('Context: setup complete.');
  });

  test('a bare-domain Peer URL is accepted as-is (plugin normalizes during setup)', async () => {
    const plugin = makePlugin();
    const tab = makeTab(plugin);
    tab.display();
    const url = __obsidian.setting('Peer URL')?.components.find((c) => c.kind === 'text');
    url?.onChangeText?.('sync.example.com');
    const submit = __obsidian.button('Set up Context');
    await submit?.onClick?.();
    expect(plugin.runSetupCalls).toEqual([{ mode: 'connect', peerUrl: 'sync.example.com' }]);
  });

  test('a runSetup failure surfaces the error notice', async () => {
    const plugin = makePlugin();
    plugin.runSetupImpl = async () => {
      throw new Error('handshake refused');
    };
    const tab = makeTab(plugin);
    tab.display();
    const dd = __obsidian.setting('Setup mode')?.components.find((c) => c.kind === 'dropdown');
    dd?.onChangeSelect?.('create'); // create mode → no URL needed
    const submit = __obsidian.button('Set up Context');
    await submit?.onClick?.();
    expect(plugin.runSetupCalls).toEqual([{ mode: 'create', peerUrl: '' }]);
    expect(Notice.log.some((m) => /setup failed — Error: handshake refused/.test(m))).toBe(true);
  });

  test('a prior onboarding error is shown in the wizard', () => {
    const plugin = makePlugin();
    plugin.onboardingError = 'device key not authorized';
    makeTab(plugin).display();
    expect(__obsidian.hasText('Last attempt failed: device key not authorized')).toBe(true);
  });

  test('the wizard seeds from a previously-written peer URL', () => {
    const plugin = makePlugin();
    plugin.settings = { ...DEFAULT_SETTINGS, peerUrl: 'wss://seed:7777' };
    makeTab(plugin).display();
    const url = __obsidian.setting('Peer URL')?.components.find((c) => c.kind === 'text');
    expect(url?.value).toBe('wss://seed:7777');
  });
});

describe('configured view', () => {
  function configured(over: Record<string, unknown> = {}) {
    const plugin = makePlugin();
    plugin._configured = true;
    plugin.controller = makeController(over);
    return plugin;
  }

  test('renders the device key, subscribes for live updates, hide() unsubscribes', () => {
    const plugin = configured();
    const tab = makeTab(plugin);
    tab.display();
    expect(__obsidian.setting('Device public key')?.components[0]?.value).toBe(
      'ssh-ed25519 AAAAFAKEKEY device',
    );
    expect(plugin.controller?.calls).toContain('on');
    tab.hide();
    expect(plugin.controller?.calls).toContain('unsub');
  });

  test('Enable sync toggle calls setSyncEnabled', async () => {
    const plugin = configured();
    makeTab(plugin).display();
    const toggle = __obsidian.setting('Enable sync')?.components.find((c) => c.kind === 'toggle');
    await toggle?.onChangeToggle?.(false);
    expect(plugin.settings.syncEnabled).toBe(false);
  });

  test('Copy button writes the SSH key to the clipboard', async () => {
    clipboardWrites.length = 0;
    const plugin = configured();
    makeTab(plugin).display();
    await __obsidian.button('Copy')?.onClick?.();
    expect(clipboardWrites).toContain('ssh-ed25519 AAAAFAKEKEY device');
  });

  test('Peer URL / Ignore patterns persist via saveSettings', async () => {
    const plugin = configured();
    makeTab(plugin).display();
    const peer = __obsidian.setting('Peer URL')?.components.find((c) => c.kind === 'text');
    await peer?.onChangeText?.('  wss://edited:7777  ');
    expect(plugin.settings.peerUrl).toBe('wss://edited:7777');

    const ig = __obsidian.setting('Ignore patterns')?.components.find((c) => c.kind === 'textarea');
    await ig?.onChangeText?.('Drafts/**\n# c\n\n*.tmp.md');
    expect(plugin.settings.ignoreGlobs).toEqual(['Drafts/**', '*.tmp.md']);
    expect(plugin.saved.length).toBeGreaterThanOrEqual(2);
  });

  test('a bare-domain Peer URL is normalized to wss://host:443 on save', async () => {
    const plugin = configured();
    makeTab(plugin).display();
    const peer = __obsidian.setting('Peer URL')?.components.find((c) => c.kind === 'text');
    await peer?.onChangeText?.('sync.example.com');
    expect(plugin.settings.peerUrl).toBe('wss://sync.example.com:443');
  });

  test('there is no Auto-connect on start setting', () => {
    const plugin = configured();
    makeTab(plugin).display();
    expect(__obsidian.setting('Auto-connect on start')).toBeUndefined();
  });

  test('Pinned peer key shows (none yet) when unset, the actual pin once set, and is read-only', () => {
    const plugin = configured();
    plugin.settings.peerPubkey = '';
    makeTab(plugin).display();
    const empty = __obsidian.setting('Pinned peer key');
    expect(empty?.components[0]?.value).toBe('(none yet)');
    // No clear-pin button — there is no way to unset it from the UI.
    expect(empty?.components.some((c) => c.kind === 'button')).toBe(false);

    plugin.settings.peerPubkey = 'ssh-ed25519 PINNED';
    __resetObsidian();
    makeTab(plugin).display();
    expect(__obsidian.setting('Pinned peer key')?.components[0]?.value).toBe('ssh-ed25519 PINNED');
    expect(__obsidian.button('Clear pin')).toBeUndefined();
  });

  test('there is no Connection / Resync now setting — the sync toggle covers it', () => {
    const plugin = configured({ state: 'connected' });
    makeTab(plugin).display();
    expect(__obsidian.setting('Connection')).toBeUndefined();
    expect(__obsidian.button(/^Disconnect$/)).toBeUndefined();
    expect(__obsidian.button(/^Reconnect$/)).toBeUndefined();
    expect(__obsidian.button('Resync now')).toBeUndefined();
  });

  test('Enable sync description reflects the current controller state', () => {
    const plugin = configured({ state: 'connected' });
    makeTab(plugin).display();
    expect(__obsidian.setting('Enable sync')?.desc).toContain('connected');
  });

  test('Reset local state calls plugin.resetLocalState and notifies', async () => {
    const plugin = configured();
    makeTab(plugin).display();
    await __obsidian.button('Reset')?.onClick?.();
    expect(plugin.resets.length).toBe(1);
    expect(Notice.log).toContain('Context: local state cleared.');
  });

  test('Snapshots: empty state, create, and restore', async () => {
    const plugin = configured();
    makeTab(plugin).display();
    expect(__obsidian.hasText('No snapshots yet')).toBe(true);
    await __obsidian.button('Create')?.onClick?.();
    expect(plugin.controller?.calls.some((c) => /^createSnapshot:snapshot-/.test(c))).toBe(true);

    // With a snapshot present → a Restore row.
    const ctl = plugin.controller;
    if (!ctl) throw new Error('controller was unexpectedly null');
    ctl.snapshots = [{ name: 'snap-A', created_at_ms: Date.now(), frontier: [] }];
    __resetObsidian();
    makeTab(plugin).display();
    const snapRow = __obsidian.setting('snap-A');
    expect(snapRow).toBeDefined();
    const restore = snapRow?.components.find(
      (c: RecordedComponent) => c.kind === 'button' && c.buttonText === 'Restore',
    );
    await restore?.onClick?.();
    expect(plugin.controller?.calls).toContain('restoreToSnapshot:snap-A');
  });

  test('a controller state change schedules a live re-render', async () => {
    const plugin = configured({ state: 'idle' });
    const tab = makeTab(plugin);
    tab.display();
    plugin.controller?.fireStateChange();
    plugin.controller?.fireStateChange(); // coalesced
    await new Promise((r) => setTimeout(r, 5));
    // Re-render happened without throwing; the view is still intact.
    expect(__obsidian.setting('Enable sync')).toBeDefined();
  });

  test('configured view tolerates a not-yet-built controller', () => {
    const plugin = makePlugin();
    plugin._configured = true;
    plugin.controller = null;
    makeTab(plugin).display();
    expect(__obsidian.setting('Device public key')?.components[0]?.value).toBe('(loading…)');
    expect(__obsidian.setting('Enable sync')?.desc).toContain('idle');
  });
});
