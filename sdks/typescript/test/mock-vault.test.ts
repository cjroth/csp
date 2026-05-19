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
