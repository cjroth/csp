// CSP SyncController behavior. No hub / probeHub / vaultId (CSP §5 — that
// whole class is gone); convergence is via the @csp/sdk in-memory mock
// (two controllers sharing a peer URL join the same Room).

import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { Identity, _resetBroker, memoryStorage } from '@csp/sdk/web-init';
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

describe('peer convergence + events (CSP §6.1 mock Room)', () => {
  test('two controllers on one peer URL converge; peer key pinned', async () => {
    const peerUrl = 'wss://peer:7777';
    const a = makeHarness({ peerUrl });
    const b = makeHarness({ peerUrl });

    await a.controller.start({ connect: true });
    await waitFor(() => (a.controller.state === 'connected' ? true : undefined));
    // First connect pins the peer key (CSP §10 key pinning).
    expect(a.settings.peerPubkey.startsWith('ssh-ed25519 ')).toBe(true);

    await a.vault.create('shared.md', 'from A\n');
    await a.controller
      .getBridge()
      ?.handleObsidianWrite(a.vault.getAbstractFileByPath('shared.md') as never);

    await b.controller.start({ connect: true });
    await waitFor(() => (b.controller.state === 'connected' ? true : undefined));
    // b pulls shared.md via tree-changed → applyRemoteState (debounced).
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
  test('clears state, warns about the same device key (CSP §5.1)', async () => {
    const h = makeHarness();
    await h.controller.prepare();
    await h.vault.create('x.md', 'data\n');
    await h.controller.resyncNow();
    await h.controller.resetLocalState();
    expect(h.notices.some((n) => /CSP §5.1/.test(n))).toBe(true);
    expect(await h.adapter.exists('.context/state')).toBe(true); // re-prepared
    await h.controller.stop();
  });
});

describe('identity pubkey', () => {
  test('available without a connection (for ctx authorize)', () => {
    const h = makeHarness();
    const ssh = h.controller.identityPubkeySsh();
    expect(ssh?.startsWith('ssh-ed25519 ')).toBe(true);
  });
});
