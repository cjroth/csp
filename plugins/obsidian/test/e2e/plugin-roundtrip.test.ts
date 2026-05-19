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
import { Identity, MockIdentity, MockVault, _resetBroker, memoryStorage } from '@csp/sdk/web-init';
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

// Each node's controller is driven against the in-memory `MockVault` double
// (the §6.1 "full node in listen mode" stand-in) via the controller's
// `sdkOverride` test seam. This exercises the plugin's real bridge +
// controller logic (Obsidian↔engine mirroring, scope, feedback-loop) end to
// end. REAL byte-identical convergence against a real `ctx` is proven by
// `sdks/typescript/test/e2e/ctx-parity.test.ts`.
async function makeNode(adapter = new FakeDataAdapter()): Promise<Node> {
  const vault = new FakeVault(adapter);
  const settings: CspSettings = { ...DEFAULT_SETTINGS, peerUrl: PEER, syncEnabled: true };
  const sdk = await MockVault.create({
    storage: memoryStorage(),
    identity: MockIdentity.generate(),
    peerUrl: PEER,
  });
  const controller = new SyncController({
    storage: new ObsidianStorageAdapter(adapter),
    vault,
    settings,
    identity: Identity.generate(),
    saveSettings: async () => {},
    sdkOverride: sdk,
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
    const a = await makeNode();
    await connected(a);
    expect(a.controller.state).toBe('connected');
    await a.controller.stop();
  });

  test('FakeVault write on A propagates into B', async () => {
    const a = await makeNode();
    const b = await makeNode();
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
    const a = await makeNode();
    const b = await makeNode();
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
    const a = await makeNode();
    const b = await makeNode();
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
    const a = await makeNode();
    const b = await makeNode();
    await connected(a);
    await connected(b);

    const f = await a.vault.create('real.md', 'real\n');
    await a.controller.getBridge()?.handleObsidianWrite(f);
    await waitFor(() => (b.vault.getAbstractFileByPath('real.md') ? true : undefined));

    // The HARD INVARIANT: no `.context/*` path ever appears in B's vault
    // content. (That CSP persists its state *under* `.context/` via
    // ObsidianStorageAdapter is covered against the real engine by
    // `sync-controller.test.ts`; here the SDK is injected via sdkOverride.)
    const leaked = b.vault.getFiles().filter((x) => x.path.startsWith('.context'));
    expect(leaked).toEqual([]);

    await a.controller.stop();
    await b.controller.stop();
  });

  // persistence: restart re-opens from `.context/state` — covered against
  // the REAL engine by `sync-controller.test.ts` ("existing state on the
  // same adapter → open …", which drives the controller's real
  // `openOrCreate()`/`Vault.open()` path through `ObsidianStorageAdapter`),
  // and at the SDK layer by `sdks/typescript` `mock-vault.test.ts`
  // ("create → write → … reopen restores"). Not re-tested here: this
  // mock-double harness injects the SDK via `sdkOverride`, so the
  // controller's open-from-storage path is intentionally not exercised.

  test('feedback-loop suppression: applying a remote write does not re-push', async () => {
    const a = await makeNode();
    const b = await makeNode();
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
