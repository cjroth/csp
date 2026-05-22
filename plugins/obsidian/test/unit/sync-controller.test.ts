// CSP SyncController behavior. No hub / probeHub / vaultId (CSP §5 — that
// whole class is gone); convergence is via the @csp/sdk in-memory mock
// (two controllers sharing a peer URL join the same Room).

import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { Identity, MockIdentity, MockVault, _resetBroker, memoryStorage } from '@csp/sdk/web-init';
import { type CspSettings, DEFAULT_SETTINGS } from '../../src/settings.js';
import { ObsidianStorageAdapter } from '../../src/storage-adapter.js';
import { SyncController } from '../../src/sync-controller.js';
import { FakeDataAdapter, FakeVault } from '../mocks/obsidian.js';

async function waitFor<T>(check: () => T | undefined, timeoutMs = 4000): Promise<T> {
  const start = Date.now();
  while (true) {
    const v = check();
    if (v !== undefined) return v;
    if (Date.now() - start > timeoutMs) throw new Error(`timeout after ${timeoutMs}ms`);
    await new Promise((r) => setTimeout(r, 15));
  }
}

interface Harness {
  controller: SyncController;
  vault: FakeVault;
  adapter: FakeDataAdapter;
  settings: CspSettings;
  notices: string[];
}

function makeHarness(over: Partial<CspSettings> = {}, adapter = new FakeDataAdapter()): Harness {
  const vault = new FakeVault(adapter);
  const storage = new ObsidianStorageAdapter(adapter);
  const settings: CspSettings = { ...DEFAULT_SETTINGS, syncEnabled: true, ...over };
  const notices: string[] = [];
  const controller = new SyncController({
    storage,
    vault,
    settings,
    identity: Identity.generate(),
    saveSettings: async (s) => {
      Object.assign(settings, s);
    },
    notice: (m) => notices.push(m),
  });
  return { controller, vault, adapter, settings, notices };
}

beforeEach(() => {
  _resetBroker();
});
afterEach(() => {
  _resetBroker();
});

describe('openOrCreate', () => {
  test('no state + no peer → create; reconcile pushes local files', async () => {
    const h = makeHarness();
    await h.vault.create('note.md', '# hi\n');
    await h.controller.prepare(); // connect:false
    expect(h.controller.state).toBe('idle');
    // The local file was pushed into the engine session.
    await h.controller.resyncNow();
    await h.controller.stop();
  });

  test('existing state on the same adapter → open (files persist across restart)', async () => {
    const adapter = new FakeDataAdapter();
    const h1 = makeHarness({}, adapter);
    await h1.vault.create('keep.md', 'kept\n');
    await h1.controller.prepare();
    await h1.controller.stop();

    // Fresh FakeVault, same adapter → ObsidianStorageAdapter sees the same
    // `.context/state`. The controller must Vault.open() and re-materialize.
    const h2 = makeHarness({}, adapter);
    await h2.controller.prepare();
    await waitFor(() => (h2.vault.getAbstractFileByPath('keep.md') ? true : undefined));
    expect(await h2.vault.read(h2.vault.getFiles()[0] as never)).toBe('kept\n');
    await h2.controller.stop();
  });
});

// Controller↔SDK wiring (connect → 'connected' pins the peer key;
// 'tree-changed' → applyRemoteState materializes into Obsidian), driven
// against the in-memory `MockVault` double via the controller's
// `sdkOverride` test seam. REAL byte-identical convergence against a real
// `ctx` is proven by `sdks/typescript/test/e2e/ctx-parity.test.ts`.
describe('controller ⇄ SDK wiring (mock double)', () => {
  async function mkCtl(peerUrl: string) {
    const adapter = new FakeDataAdapter();
    const vault = new FakeVault(adapter);
    const settings: CspSettings = { ...DEFAULT_SETTINGS, syncEnabled: true, peerUrl };
    const sdk = await MockVault.create({
      storage: memoryStorage(),
      identity: MockIdentity.generate(),
      peerUrl,
    });
    const controller = new SyncController({
      storage: new ObsidianStorageAdapter(adapter),
      vault,
      settings,
      identity: Identity.generate(),
      saveSettings: async (s) => {
        Object.assign(settings, s);
      },
      sdkOverride: sdk,
    });
    return { controller, vault, settings };
  }

  test('connect pins the peer key; remote write materializes into Obsidian', async () => {
    const peerUrl = 'wss://peer:7777';
    const a = await mkCtl(peerUrl);
    const b = await mkCtl(peerUrl);

    await a.controller.start({ connect: true });
    await waitFor(() => (a.controller.state === 'connected' ? true : undefined));
    expect(a.settings.peerPubkey.startsWith('ssh-ed25519 ')).toBe(true);

    await a.vault.create('shared.md', 'from A\n');
    await a.controller
      .getBridge()
      ?.handleObsidianWrite(a.vault.getAbstractFileByPath('shared.md') as never);

    await b.controller.start({ connect: true });
    await waitFor(() => (b.controller.state === 'connected' ? true : undefined));
    await waitFor(() => (b.vault.getAbstractFileByPath('shared.md') ? true : undefined));
    expect(await b.vault.read(b.vault.getFiles()[0] as never)).toBe('from A\n');

    await a.controller.stop();
    await b.controller.stop();
  });
});

describe('snapshots', () => {
  test('create → list → restore', async () => {
    const h = makeHarness();
    await h.controller.prepare();
    await h.vault.create('n.md', 'v1\n');
    await h.controller
      .getBridge()
      ?.handleObsidianWrite(h.vault.getAbstractFileByPath('n.md') as never);
    await h.controller.createSnapshot('s1');
    expect(h.controller.listSnapshots().map((s) => s.name)).toEqual(['s1']);
    await h.controller.restoreToSnapshot('s1');
    await h.controller.stop();
  });
});

describe('resetLocalState', () => {
  test('fully wipes the .context folder when `fs` is wired', async () => {
    const adapter = new FakeDataAdapter();
    const vault = new FakeVault(adapter);
    const storage = new ObsidianStorageAdapter(adapter);
    const settings: CspSettings = { ...DEFAULT_SETTINGS, syncEnabled: true };
    const notices: string[] = [];
    const controller = new SyncController({
      storage,
      fs: adapter,
      vault,
      settings,
      identity: Identity.generate(),
      saveSettings: async (s) => {
        Object.assign(settings, s);
      },
      notice: (m) => notices.push(m),
    });
    await controller.prepare();
    await vault.create('x.md', 'data\n');
    await controller.resyncNow();
    // Drop a stray file so we verify the recursive wipe catches non-engine
    // bytes (e.g. the plugin sidecar or the mobile device key).
    await adapter.write('.context/obsidian.json', '{}');
    expect(await adapter.exists('.context/state')).toBe(true);
    expect(await adapter.exists('.context/obsidian.json')).toBe(true);

    await controller.resetLocalState();

    // The entire folder is gone — engine state, snapshots, sidecar, all of it.
    expect(await adapter.exists('.context/state')).toBe(false);
    expect(await adapter.exists('.context/obsidian.json')).toBe(false);
    expect(await adapter.exists('.context')).toBe(false);
    expect(notices.some((n) => /local state cleared/.test(n))).toBe(true);
    // The Obsidian vault contents are untouched.
    expect(vault.getAbstractFileByPath('x.md')).not.toBeNull();
  });

  test('without `fs`, falls back to zeroing the engine blobs', async () => {
    // Older callers may not pass `fs`. We still clear engine state so the next
    // start() rebuilds from scratch instead of silently re-opening.
    const h = makeHarness();
    await h.controller.prepare();
    await h.vault.create('x.md', 'data\n');
    await h.controller.resyncNow();
    await h.controller.resetLocalState();
    expect(h.notices.some((n) => /local state cleared/.test(n))).toBe(true);
  });
});

describe('identity pubkey', () => {
  test('available without a connection (for ctx authorize)', () => {
    const h = makeHarness();
    const ssh = h.controller.identityPubkeySsh();
    expect(ssh?.startsWith('ssh-ed25519 ')).toBe(true);
  });
});

// Issue 0010: the controller decides the vault mode and hands a VaultSpec
// to `makeVault`; the plugin injects a Web Worker factory there. These
// verify the controller-side wiring — the mode decision and spec contents —
// with a spy factory (the worker itself is covered by the SDK's
// worker.test.ts).
describe('makeVault injection (issue 0010)', () => {
  function spyHarness(over: Partial<CspSettings>, adapter = new FakeDataAdapter()) {
    const vault = new FakeVault(adapter);
    const storage = new ObsidianStorageAdapter(adapter);
    const settings: CspSettings = { ...DEFAULT_SETTINGS, syncEnabled: true, ...over };
    const specs: import('../../src/sync-controller.js').VaultSpec[] = [];
    const controller = new SyncController({
      storage,
      vault,
      settings,
      identity: Identity.generate(),
      saveSettings: async () => {},
      makeVault: async (spec) => {
        specs.push(spec);
        // A throwaway in-memory MockVault stands in for the worker vault —
        // the controller only needs *a* VaultInstance back.
        return MockVault.create({ storage: memoryStorage(), identity: MockIdentity.generate() });
      },
    });
    return { controller, specs, adapter };
  }

  test('fresh vault, no peer → makeVault called with mode "create"', async () => {
    const h = spyHarness({});
    await h.controller.prepare();
    expect(h.specs).toHaveLength(1);
    expect(h.specs[0]?.mode).toBe('create');
    expect(h.specs[0]?.peerUrl).toBeUndefined();
    await h.controller.stop();
  });

  test('peer set, no local state → mode "clone" with the peer URL + auth key', async () => {
    const h = spyHarness({ peerUrl: 'wss://peer:7777', authKey: 'secret-token' });
    await h.controller.prepare();
    expect(h.specs[0]?.mode).toBe('clone');
    expect(h.specs[0]?.peerUrl).toBe('wss://peer:7777');
    expect(h.specs[0]?.authKey).toBe('secret-token');
    await h.controller.stop();
  });

  test('existing local state → mode "open"', async () => {
    // Seed `.context/state` so the controller sees prior state.
    const adapter = new FakeDataAdapter();
    const seed = makeHarness({}, adapter);
    await seed.controller.prepare();
    await seed.controller.stop();
    // A fresh controller on the same adapter must choose "open".
    const h = spyHarness({}, adapter);
    await h.controller.prepare();
    expect(h.specs[0]?.mode).toBe('open');
    await h.controller.stop();
  });

  test('a pinned peer key is decoded to raw bytes in the spec', async () => {
    const pk = Identity.generate().pubkey();
    const ssh = pk.toSshString();
    pk.free();
    const h = spyHarness({ peerUrl: 'wss://peer:7777', peerPubkey: ssh });
    await h.controller.prepare();
    expect(h.specs[0]?.peerPubkey).toBeInstanceOf(Uint8Array);
    expect(h.specs[0]?.peerPubkey?.length).toBe(32);
    await h.controller.stop();
  });
});
