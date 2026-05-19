// Extra SyncController coverage: onVaultEvent branches, the connect/error/
// clone paths, peer-key pinning (empty vs real), and the not-running guards.
// Complements sync-controller.test.ts (which covers openOrCreate + the
// mock-double wiring against the real engine).

import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { Identity, _resetBroker } from '@csp/sdk/web-init';
import type { FileMeta, VaultEvent } from '@csp/sdk/web-init';
import { type CspSettings, DEFAULT_SETTINGS } from '../../src/settings.js';
import { ObsidianStorageAdapter } from '../../src/storage-adapter.js';
import { SyncController } from '../../src/sync-controller.js';
import { FakeDataAdapter, FakeVault } from '../mocks/obsidian.js';

async function tick(ms = 0): Promise<void> {
  await new Promise((r) => setTimeout(r, ms));
}

/** A controllable VaultInstance double: lets the test emit VaultEvents into
 * the controller and decide how connectWithReconnect resolves. */
class FakeSdk {
  files = new Map<string, string>();
  listeners = new Set<(e: VaultEvent) => void>();
  connectImpl: () => Promise<void> = async () => {};
  closed = false;
  disconnected = false;

  emit(e: VaultEvent): void {
    for (const l of this.listeners) l(e);
  }
  subscribe(l: (e: VaultEvent) => void): () => void {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  }
  listFiles(): FileMeta[] {
    return [...this.files].map(([path, c]) => ({
      id: path,
      path,
      kind: 'Text' as const,
      size: c.length,
      created_at: 0,
      updated_at: 0,
      deleted_at: null,
    }));
  }
  async readTextFile(p: string): Promise<string> {
    const c = this.files.get(p);
    if (c === undefined) throw new Error(`ENOENT: ${p}`);
    return c;
  }
  async writeTextFile(p: string, c: string): Promise<string> {
    this.files.set(p, c);
    return p;
  }
  fileExists(p: string): boolean {
    return this.files.has(p);
  }
  async deleteFile(p: string): Promise<void> {
    this.files.delete(p);
  }
  async renameFile(a: string, b: string): Promise<void> {
    const c = this.files.get(a);
    if (c !== undefined) {
      this.files.set(b, c);
      this.files.delete(a);
    }
  }
  async connectWithReconnect(): Promise<void> {
    return this.connectImpl();
  }
  async disconnect(): Promise<void> {
    this.disconnected = true;
  }
  async close(): Promise<void> {
    this.closed = true;
  }
  listSnapshots() {
    return [];
  }
  async createSnapshot(): Promise<void> {}
  async restoreToSnapshot(): Promise<void> {}
}

interface Built {
  controller: SyncController;
  vault: FakeVault;
  sdk: FakeSdk;
  settings: CspSettings;
  notices: string[];
  logs: string[];
}

function buildWithFakeSdk(over: Partial<CspSettings> = {}): Built {
  const adapter = new FakeDataAdapter();
  const vault = new FakeVault(adapter);
  const settings: CspSettings = {
    ...DEFAULT_SETTINGS,
    syncEnabled: true,
    peerUrl: 'wss://peer:7777',
    ...over,
  };
  const sdk = new FakeSdk();
  const notices: string[] = [];
  const logs: string[] = [];
  const controller = new SyncController({
    storage: new ObsidianStorageAdapter(adapter),
    vault,
    settings,
    identity: Identity.generate(),
    saveSettings: async (s) => {
      Object.assign(settings, s);
    },
    // biome-ignore lint/suspicious/noExplicitAny: controllable test double
    sdkOverride: sdk as any,
    notice: (m) => notices.push(m),
    log: (m) => logs.push(m),
  });
  return { controller, vault, sdk, settings, notices, logs };
}

beforeEach(() => _resetBroker());
afterEach(() => _resetBroker());

describe('onVaultEvent branches', () => {
  test('connecting → connected → tree-changed → disconnected → error', async () => {
    const b = buildWithFakeSdk();
    await b.controller.start({ connect: true });

    b.sdk.emit({ kind: 'connecting', url: 'wss://peer:7777' });
    expect(b.controller.state).toBe('connecting');

    // Real engine emits an EMPTY peer_pubkey — must NOT pin (and must not
    // throw a swallowed error). settings.peerPubkey stays unset.
    b.sdk.emit({ kind: 'connected', peer_pubkey: new Uint8Array() });
    expect(b.controller.state).toBe('connected');
    expect(b.settings.peerPubkey).toBe('');

    // catchup-progress is intentionally silent (no state change, no throw).
    b.sdk.emit({ kind: 'catchup-progress', outbound: true });
    expect(b.controller.state).toBe('connected');

    // tree-changed schedules an applyRemoteState pass (200ms debounce).
    b.sdk.files.set('remote.md', 'pulled');
    b.sdk.emit({ kind: 'tree-changed' });
    await tick(260);
    expect(b.vault.getAbstractFileByPath('remote.md')).not.toBeNull();

    b.sdk.emit({ kind: 'disconnected', reason: 'closed' });
    expect(b.controller.state).toBe('reconnecting');

    b.sdk.emit({ kind: 'error', message: 'boom' });
    expect(b.controller.state).toBe('error');
    expect(b.notices.some((n) => n.includes('boom'))).toBe(true);

    await b.controller.stop();
  });

  test('disconnected while idle does NOT flip to reconnecting', async () => {
    const b = buildWithFakeSdk();
    await b.controller.prepare(); // connect:false → state stays idle
    expect(b.controller.state).toBe('idle');
    b.sdk.emit({ kind: 'disconnected', reason: 'n/a' });
    expect(b.controller.state).toBe('idle');
    await b.controller.stop();
  });

  test('connected with a real peer pubkey pins it once (CSP §10)', async () => {
    const b = buildWithFakeSdk();
    await b.controller.start({ connect: true });

    const pk = Identity.generate().pubkey();
    const bytes = pk.bytes();
    const ssh = pk.toSshString();
    pk.free();

    b.sdk.emit({ kind: 'connected', peer_pubkey: bytes });
    expect(b.settings.peerPubkey).toBe(ssh);

    // A second connect must not re-pin / overwrite.
    b.sdk.emit({ kind: 'connected', peer_pubkey: new Uint8Array() });
    expect(b.settings.peerPubkey).toBe(ssh);

    await b.controller.stop();
  });
});

describe('connectWithReconnect supervisor', () => {
  test('a rejected reconnect promise is caught and logged', async () => {
    const b = buildWithFakeSdk();
    b.sdk.connectImpl = () => Promise.reject(new Error('supervisor died'));
    await b.controller.start({ connect: true });
    await tick(10);
    expect(
      b.logs.some((l) => /reconnect supervisor exited with error.*supervisor died/.test(l)),
    ).toBe(true);
    await b.controller.stop();
  });

  test('start() is idempotent — a second call while connected is a no-op', async () => {
    const b = buildWithFakeSdk();
    await b.controller.start({ connect: true });
    b.sdk.emit({ kind: 'connected', peer_pubkey: new Uint8Array() });
    expect(b.controller.state).toBe('connected');
    expect(b.sdk.listeners.size).toBe(1);
    await b.controller.start(); // not idle/error → returns immediately
    expect(b.sdk.listeners.size).toBe(1); // not re-subscribed
    expect(b.controller.state).toBe('connected');
    await b.controller.stop();
  });
});

describe('peer-key pin decoding (sshPubkeyBytes)', () => {
  test('a valid [peer] pubkey is decoded into the engine options', async () => {
    const pk = Identity.generate().pubkey();
    const validSsh = pk.toSshString();
    pk.free();
    // No state + no peerUrl → Vault.create(base); base still computes
    // sshPubkeyBytes(peerPubkey), exercising the valid-decode path.
    const adapter = new FakeDataAdapter();
    const settings: CspSettings = {
      ...DEFAULT_SETTINGS,
      syncEnabled: true,
      peerUrl: '',
      peerPubkey: validSsh,
    };
    const controller = new SyncController({
      storage: new ObsidianStorageAdapter(adapter),
      vault: new FakeVault(adapter),
      settings,
      identity: Identity.generate(),
      saveSettings: async () => {},
    });
    await controller.prepare();
    expect(controller.state).toBe('idle');
    await controller.stop();
  });

  test('a malformed [peer] pubkey surfaces a readable error + error state', async () => {
    const adapter = new FakeDataAdapter();
    const notices: string[] = [];
    const controller = new SyncController({
      storage: new ObsidianStorageAdapter(adapter),
      vault: new FakeVault(adapter),
      settings: {
        ...DEFAULT_SETTINGS,
        syncEnabled: true,
        peerUrl: '',
        peerPubkey: 'not-a-real-ssh-key',
      },
      identity: Identity.generate(),
      saveSettings: async () => {},
      notice: (m) => notices.push(m),
    });
    await expect(controller.prepare()).rejects.toThrow(/invalid \[peer\] pubkey/);
    expect(controller.state).toBe('error');
    // start() rethrows and does NOT notice itself — the caller owns the
    // single user-facing failure toast (no duplicate).
    expect(notices.some((n) => n.includes('failed to start'))).toBe(false);
  });
});

describe('not-running guards', () => {
  test('resyncNow / createSnapshot / restoreToSnapshot / listSnapshots before start', async () => {
    const adapter = new FakeDataAdapter();
    const notices: string[] = [];
    const controller = new SyncController({
      storage: new ObsidianStorageAdapter(adapter),
      vault: new FakeVault(adapter),
      settings: { ...DEFAULT_SETTINGS },
      identity: Identity.generate(),
      saveSettings: async () => {},
      notice: (m) => notices.push(m),
    });
    expect(controller.listSnapshots()).toEqual([]);
    await controller.createSnapshot('noop'); // no sdk → returns
    await controller.restoreToSnapshot('noop'); // no sdk/bridge → returns
    await controller.resyncNow();
    expect(notices).toContain('Context: not running.');
    await controller.stop(); // safe from idle
    expect(controller.state).toBe('idle');
  });

  test('the engine→Obsidian apply path runs through applyOneRemoteFile', async () => {
    const b = buildWithFakeSdk();
    await b.controller.start({ connect: false });
    b.sdk.files.set('Notes/a.md', 'hello');
    await b.controller.getBridge()?.applyOneRemoteFile(b.sdk.listFiles()[0] as FileMeta);
    expect(b.vault.getAbstractFileByPath('Notes/a.md')).not.toBeNull();
    await b.controller.stop();
  });
});
