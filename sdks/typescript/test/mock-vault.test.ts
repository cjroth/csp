import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import {
  Identity,
  Vault,
  type VaultEvent,
  _resetBroker,
  _resetForTests,
  initCsp,
  memoryStorage,
} from '../src/web-init.js';

beforeEach(async () => {
  _resetForTests();
  _resetBroker();
  await initCsp();
});
afterEach(() => {
  _resetBroker();
});

describe('initCsp / accessors', () => {
  test('factories throw before init', () => {
    _resetForTests();
    expect(() => Identity.generate()).toThrow(/not initialized/);
  });

  test('identity pubkey is stable and ssh-formatted', () => {
    const seed = new Uint8Array(32).fill(5);
    const id = Identity.fromSeed(seed);
    const pk = id.pubkey();
    expect(pk.toSshString().startsWith('ssh-ed25519 ')).toBe(true);
    expect(Array.from(pk.bytes())).toEqual(Array.from(seed));
  });
});

describe('MockVault offline', () => {
  test('create → write → read → list, persisted to storage', async () => {
    const storage = memoryStorage();
    const v = await Vault.create({ storage, identity: Identity.generate() });
    await v.writeTextFile('a.md', 'hello');
    expect(v.fileExists('a.md')).toBe(true);
    expect(await v.readTextFile('a.md')).toBe('hello');
    expect(v.listFiles().map((f) => f.path)).toEqual(['a.md']);
    expect(await storage.loadState()).not.toBeNull();
    await v.close();

    // Reopen from the same storage.
    const v2 = await Vault.open({ storage, identity: Identity.generate() });
    expect(await v2.readTextFile('a.md')).toBe('hello');
    await v2.close();
  });

  test('open with no prior state throws', async () => {
    await expect(Vault.open({ storage: memoryStorage() })).rejects.toThrow(/no vault on disk/);
  });

  test('snapshot + restore is restore-as-edit', async () => {
    const v = await Vault.create({ storage: memoryStorage() });
    await v.writeTextFile('n.md', 'v1');
    await v.createSnapshot('s1');
    await v.writeTextFile('n.md', 'v2');
    expect(v.listSnapshots().map((s) => s.name)).toEqual(['s1']);
    await v.restoreToSnapshot('s1');
    expect(await v.readTextFile('n.md')).toBe('v1');
    await v.close();
  });
});

describe('MockVault convergence (two thin nodes, one peer URL)', () => {
  test('changes converge both directions', async () => {
    const events: VaultEvent['kind'][] = [];
    const a = await Vault.create({
      storage: memoryStorage(),
      identity: Identity.generate(),
      peerUrl: 'wss://peer:1',
    });
    const b = await Vault.create({
      storage: memoryStorage(),
      identity: Identity.generate(),
      peerUrl: 'wss://peer:1',
    });
    b.subscribe((e) => events.push(e.kind));

    await a.writeTextFile('shared.md', 'from-a');
    void a.connectWithReconnect();
    void b.connectWithReconnect();
    await new Promise((r) => setTimeout(r, 10));

    expect(b.fileExists('shared.md')).toBe(true);
    expect(await b.readTextFile('shared.md')).toBe('from-a');

    await b.writeTextFile('reply.md', 'from-b');
    await new Promise((r) => setTimeout(r, 10));
    expect(await a.readTextFile('reply.md')).toBe('from-b');
    expect(events).toContain('connected');
    expect(events).toContain('tree-changed');

    await a.close();
    await b.close();
  });

  test('offline-only vault never converges (CSP §7)', async () => {
    const a = await Vault.create({ storage: memoryStorage() }); // no peerUrl
    await a.connectWithReconnect(); // resolves immediately, no-op
    expect(a.isConnected()).toBe(false);
    await a.close();
  });
});
