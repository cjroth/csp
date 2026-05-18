// e2e: two plugin SyncControllers (each over an in-memory FakeVault) share
// one peer URL and converge via the @csp/sdk mock Room — the stand-in for
// "a full node in listen mode" (CSP spec.md §6.1). No external process: the
// real `ctx` listener is a residual gate (obsidian-plugin-spec §14); the
// seam isolates it, and the same assertions hold when csp-wasm + a real
// full node land.
//
// Coverage:
//   1. Both plugins reach connected
//   2. Obsidian → engine → other device: a FakeVault write propagates
//   3. delete propagates as a tombstone
//   4. rename propagates
//   5. .context/ is NEVER synced (CSP §11 HARD INVARIANT)
//   6. persistence: restart re-opens from `.context/state`
//   7. feedback-loop suppression: applying a remote write does not re-push

import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { Identity, _resetBroker, memoryStorage } from '@csp/sdk/web-init';
import { type CspSettings, DEFAULT_SETTINGS } from '../../src/settings.js';
import { ObsidianStorageAdapter } from '../../src/storage-adapter.js';
import { SyncController } from '../../src/sync-controller.js';
import { FakeDataAdapter, type FakeTFile, FakeVault } from '../mocks/obsidian.js';

const PEER = 'wss://peer:7777';

async function waitFor<T>(check: () => T | undefined, timeoutMs = 5000): Promise<T> {
  const start = Date.now();
  while (true) {
    const v = check();
    if (v !== undefined) return v;
    if (Date.now() - start > timeoutMs) throw new Error(`timeout after ${timeoutMs}ms`);
    await new Promise((r) => setTimeout(r, 15));
  }
}

interface Node {
  controller: SyncController;
  vault: FakeVault;
  adapter: FakeDataAdapter;
}

function makeNode(adapter = new FakeDataAdapter()): Node {
  const vault = new FakeVault(adapter);
  const settings: CspSettings = { ...DEFAULT_SETTINGS, peerUrl: PEER, syncEnabled: true };
  const controller = new SyncController({
    storage: new ObsidianStorageAdapter(adapter),
    vault,
    settings,
    identity: Identity.generate(),
    saveSettings: async () => {},
  });
  return { controller, vault, adapter };
}

async function connected(n: Node): Promise<void> {
  await n.controller.start({ connect: true });
  await waitFor(() => (n.controller.state === 'connected' ? true : undefined));
}

beforeEach(() => _resetBroker());
afterEach(() => _resetBroker());

describe('Context for Obsidian — end-to-end (mock peer)', () => {
  test('both plugins reach connected', async () => {
    const a = makeNode();
    await connected(a);
    expect(a.controller.state).toBe('connected');
    await a.controller.stop();
  });

  test('FakeVault write on A propagates into B', async () => {
    const a = makeNode();
    const b = makeNode();
    await connected(a);
    await connected(b);

    const f = await a.vault.create('plugin-out.md', '# from A\n');
    await a.controller.getBridge()?.handleObsidianWrite(f);

    await waitFor(() => (b.vault.getAbstractFileByPath('plugin-out.md') ? true : undefined));
    const got = b.vault.getAbstractFileByPath('plugin-out.md') as FakeTFile;
    expect(await b.vault.read(got)).toBe('# from A\n');

    await a.controller.stop();
    await b.controller.stop();
  });

  test('delete on A tombstones into B', async () => {
    const a = makeNode();
    const b = makeNode();
    await connected(a);
    await connected(b);

    const f = await a.vault.create('to-delete.md', 'doomed\n');
    await a.controller.getBridge()?.handleObsidianWrite(f);
    await waitFor(() => (b.vault.getAbstractFileByPath('to-delete.md') ? true : undefined));

    await a.vault.delete(f);
    await a.controller.getBridge()?.handleObsidianDelete(f);
    await waitFor(() =>
      b.vault.getAbstractFileByPath('to-delete.md') === null ? true : undefined,
    );
    expect(b.vault.getAbstractFileByPath('to-delete.md')).toBeNull();

    await a.controller.stop();
    await b.controller.stop();
  });

  test('rename on A propagates to B', async () => {
    const a = makeNode();
    const b = makeNode();
    await connected(a);
    await connected(b);

    const f = await a.vault.create('rename-src.md', '# rename me\n');
    await a.controller.getBridge()?.handleObsidianWrite(f);
    await waitFor(() => (b.vault.getAbstractFileByPath('rename-src.md') ? true : undefined));

    await a.vault.rename(f, 'rename-dst.md');
    await a.controller.getBridge()?.handleObsidianRename(f, 'rename-src.md');
    await waitFor(() => (b.vault.getAbstractFileByPath('rename-dst.md') ? true : undefined));
    expect(b.vault.getAbstractFileByPath('rename-dst.md')).not.toBeNull();

    await a.controller.stop();
    await b.controller.stop();
  });

  test('.context/ is never synced (CSP §11 HARD INVARIANT)', async () => {
    const a = makeNode();
    const b = makeNode();
    await connected(a);
    await connected(b);

    const f = await a.vault.create('real.md', 'real\n');
    await a.controller.getBridge()?.handleObsidianWrite(f);
    await waitFor(() => (b.vault.getAbstractFileByPath('real.md') ? true : undefined));

    // No `.context/*` path ever appears in B's vault content.
    const leaked = b.vault.getFiles().filter((x) => x.path.startsWith('.context'));
    expect(leaked).toEqual([]);
    // And A wrote its own state under `.context/` on its adapter (excluded
    // from sync, present on disk).
    expect(await a.adapter.exists('.context/state')).toBe(true);

    await a.controller.stop();
    await b.controller.stop();
  });

  test('persistence: restart re-opens from .context/state', async () => {
    const a = makeNode();
    await connected(a);
    const f = await a.vault.create('persistent.md', '# kept\n');
    await a.controller.getBridge()?.handleObsidianWrite(f);
    await a.controller.stop();

    // Fresh FakeVault, same adapter → Vault.open() must inherit the files.
    const a2 = makeNode(a.adapter);
    await a2.controller.prepare();
    await waitFor(() => (a2.vault.getAbstractFileByPath('persistent.md') ? true : undefined));
    expect(await a2.vault.read(a2.vault.getFiles()[0] as FakeTFile)).toBe('# kept\n');
    await a2.controller.stop();
  });

  test('feedback-loop suppression: applying a remote write does not re-push', async () => {
    const a = makeNode();
    const b = makeNode();
    await connected(a);
    await connected(b);

    const before = b.controller.getBridge()?.pushed ?? 0;
    const f = await a.vault.create('one-way.md', 'remote\n');
    await a.controller.getBridge()?.handleObsidianWrite(f);
    await waitFor(() => (b.vault.getAbstractFileByPath('one-way.md') ? true : undefined));
    // Let any spurious modify event fire.
    await new Promise((r) => setTimeout(r, 300));
    expect(b.controller.getBridge()?.pushed ?? 0).toBe(before);

    await a.controller.stop();
    await b.controller.stop();
  });
});
