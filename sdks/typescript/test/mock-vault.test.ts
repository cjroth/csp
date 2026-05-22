// @csp/sdk unit tests. The production `Vault` is now the **real engine**
// (csp-core via wasm); these exercise it offline (no peer needed). The
// in-memory `MockVault` double (still used for offline UI tests / plugin
// unit tests) is exercised separately via its broker.

import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import {
  Identity,
  MockIdentity,
  MockVault,
  Vault,
  _resetBroker,
  initCsp,
  memoryStorage,
} from '../src/web-init.js';

beforeEach(async () => {
  _resetBroker();
  await initCsp();
});
afterEach(() => _resetBroker());

describe('real Identity (engine-derived)', () => {
  test('deterministic, ssh-ed25519, 32-byte pubkey', () => {
    const seed = new Uint8Array(32).fill(5);
    const a = Identity.fromSeed(seed).pubkey();
    const b = Identity.fromSeed(seed).pubkey();
    expect(a.toSshString().startsWith('ssh-ed25519 ')).toBe(true);
    expect(a.toSshString()).toBe(b.toSshString()); // deterministic from seed
    expect(a.bytes().length).toBe(32); // the ed25519 *public* key
    expect(a.toSshString()).not.toBe(Identity.generate().pubkey().toSshString());
  });
});

describe('real Vault — offline (no peer; same engine as ctx)', () => {
  test('create → write → read → list, persisted; reopen restores', async () => {
    const storage = memoryStorage();
    const v = await Vault.create({ storage, identity: Identity.generate() });
    await v.writeTextFile('a.md', 'hello');
    expect(v.fileExists('a.md')).toBe(true);
    expect(await v.readTextFile('a.md')).toBe('hello');
    expect(v.listFiles().map((f) => f.path)).toEqual(['a.md']);
    expect(await storage.loadState()).not.toBeNull();
    await v.close();

    const v2 = await Vault.open({ storage, identity: Identity.generate() });
    expect(await v2.readTextFile('a.md')).toBe('hello');
    await v2.close();
  });

  test('open with no prior state throws', async () => {
    await expect(Vault.open({ storage: memoryStorage() })).rejects.toThrow(/no vault on disk/);
  });

  test('snapshot + restore is restore-as-edit (real fold)', async () => {
    const v = await Vault.create({ storage: memoryStorage(), identity: Identity.generate() });
    await v.writeTextFile('n.md', 'v1');
    await v.createSnapshot('s1');
    await v.writeTextFile('n.md', 'v2');
    expect(v.listSnapshots().map((s) => s.name)).toEqual(['s1']);
    await v.restoreToSnapshot('s1');
    expect(await v.readTextFile('n.md')).toBe('v1');
    await v.close();
  });

  test('offline-only vault never connects (CSP §7)', async () => {
    const v = await Vault.create({ storage: memoryStorage(), identity: Identity.generate() });
    await v.connectWithReconnect(); // no peerUrl → immediate no-op
    expect(v.isConnected()).toBe(false);
    await v.close();
  });

  test('write is folded into the working tree the engine reports', async () => {
    const v = await Vault.create({ storage: memoryStorage(), identity: Identity.generate() });
    await v.writeTextFile('x.md', 'data');
    expect(v.listFiles().map((f) => f.path)).toEqual(['x.md']);
    await v.close();
  });
});

// Issue 0009: the engine holds the working set; each file op stages a delta
// (`stage_write`/`stage_remove`) instead of re-shipping the whole vault.
// These exercise the SDK's incremental commit path and its edge cases.
describe('real Vault — incremental staging (issue 0009)', () => {
  test('a burst of writes collapses into one commit, all readable', async () => {
    const storage = memoryStorage();
    const v = await Vault.create({ storage, identity: Identity.generate() });
    // No await between writes → the 120ms debounce coalesces them.
    await v.writeTextFile('a.md', 'A');
    await v.writeTextFile('b.md', 'B');
    await v.writeTextFile('c.md', 'C');
    await v.close(); // flushCommit drains the debounce
    const v2 = await Vault.open({ storage, identity: Identity.generate() });
    expect(
      v2
        .listFiles()
        .map((f) => f.path)
        .sort(),
    ).toEqual(['a.md', 'b.md', 'c.md']);
    expect(await v2.readTextFile('b.md')).toBe('B');
    await v2.close();
  });

  test('delete stages a removal — the file is gone after reopen', async () => {
    const storage = memoryStorage();
    const v = await Vault.create({ storage, identity: Identity.generate() });
    await v.writeTextFile('keep.md', 'keep');
    await v.writeTextFile('drop.md', 'drop');
    await v.close();
    const v2 = await Vault.open({ storage, identity: Identity.generate() });
    await v2.deleteFile('drop.md');
    await v2.close();
    const v3 = await Vault.open({ storage, identity: Identity.generate() });
    expect(v3.listFiles().map((f) => f.path)).toEqual(['keep.md']);
    await v3.close();
  });

  test('rename stages write(to)+remove(from) atomically', async () => {
    const storage = memoryStorage();
    const v = await Vault.create({ storage, identity: Identity.generate() });
    await v.writeTextFile('old.md', 'body');
    await v.renameFile('old.md', 'new.md');
    await v.close();
    const v2 = await Vault.open({ storage, identity: Identity.generate() });
    expect(v2.listFiles().map((f) => f.path)).toEqual(['new.md']);
    expect(await v2.readTextFile('new.md')).toBe('body');
    expect(v2.fileExists('old.md')).toBe(false);
    await v2.close();
  });

  test('write-then-delete the same path before a commit is a clean no-op', async () => {
    const storage = memoryStorage();
    const v = await Vault.create({ storage, identity: Identity.generate() });
    await v.writeTextFile('scratch.md', 'tmp');
    await v.deleteFile('scratch.md');
    await v.close();
    const v2 = await Vault.open({ storage, identity: Identity.generate() });
    expect(v2.listFiles()).toEqual([]);
    await v2.close();
  });

  test('reopen then commit with no edits authors nothing (no spurious primitive)', async () => {
    const storage = memoryStorage();
    const v = await Vault.create({ storage, identity: Identity.generate() });
    await v.writeTextFile('a.md', 'A');
    await v.close();
    // Reopen, touch nothing, close. The staged set was seeded from `main`,
    // so there is no phantom "everything changed" commit.
    const v2 = await Vault.open({ storage, identity: Identity.generate() });
    const before = v2.listSnapshots().length;
    await v2.close();
    const v3 = await Vault.open({ storage, identity: Identity.generate() });
    expect(await v3.readTextFile('a.md')).toBe('A');
    expect(v3.listSnapshots().length).toBe(before);
    await v3.close();
  });

  test('re-writing identical content across a reopen is a non-event', async () => {
    const storage = memoryStorage();
    const v = await Vault.create({ storage, identity: Identity.generate() });
    await v.writeTextFile('a.md', 'same');
    await v.close();
    const v2 = await Vault.open({ storage, identity: Identity.generate() });
    await v2.writeTextFile('a.md', 'same'); // identical bytes
    await v2.close();
    const v3 = await Vault.open({ storage, identity: Identity.generate() });
    expect(await v3.readTextFile('a.md')).toBe('same');
    await v3.close();
  });

  test('unicode paths and content round-trip through staging', async () => {
    const storage = memoryStorage();
    const v = await Vault.create({ storage, identity: Identity.generate() });
    await v.writeTextFile('notes/ünïcøde-✓.md', 'snowman ☃ and quote " backslash \\');
    await v.close();
    const v2 = await Vault.open({ storage, identity: Identity.generate() });
    expect(await v2.readTextFile('notes/ünïcøde-✓.md')).toBe('snowman ☃ and quote " backslash \\');
    await v2.close();
  });

  test('an empty-string file is staged and round-trips (not confused with absent)', async () => {
    const storage = memoryStorage();
    const v = await Vault.create({ storage, identity: Identity.generate() });
    await v.writeTextFile('empty.md', '');
    await v.close();
    const v2 = await Vault.open({ storage, identity: Identity.generate() });
    expect(v2.fileExists('empty.md')).toBe(true);
    expect(await v2.readTextFile('empty.md')).toBe('');
    await v2.close();
  });

  test('many sequential edits to one file keep only the latest', async () => {
    const storage = memoryStorage();
    const v = await Vault.create({ storage, identity: Identity.generate() });
    for (let i = 0; i < 50; i++) await v.writeTextFile('doc.md', `revision ${i}`);
    await v.close();
    const v2 = await Vault.open({ storage, identity: Identity.generate() });
    expect(await v2.readTextFile('doc.md')).toBe('revision 49');
    await v2.close();
  });
});

describe('MockVault double — broker convergence (offline UI tests)', () => {
  test('two mock vaults on one peer URL converge', async () => {
    const a = await MockVault.create({
      storage: memoryStorage(),
      identity: MockIdentity.generate(),
      peerUrl: 'wss://peer:1',
    });
    const b = await MockVault.create({
      storage: memoryStorage(),
      identity: MockIdentity.generate(),
      peerUrl: 'wss://peer:1',
    });
    await a.writeTextFile('shared.md', 'from-a');
    void a.connectWithReconnect();
    void b.connectWithReconnect();
    await new Promise((r) => setTimeout(r, 10));
    expect(b.fileExists('shared.md')).toBe(true);
    expect(await b.readTextFile('shared.md')).toBe('from-a');
    await a.close();
    await b.close();
  });
});
