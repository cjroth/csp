import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { ObsidianStorageAdapter, sanitizeOid } from '../../src/storage-adapter.js';
import { FakeDataAdapter } from '../mocks/obsidian.js';

// Matches the CLI: state lives in `<vault-root>/.context/` (CSP §9.1).
const ROOT = '.context';

let adapter: FakeDataAdapter;
let storage: ObsidianStorageAdapter;

beforeEach(() => {
  adapter = new FakeDataAdapter();
  storage = new ObsidianStorageAdapter(adapter, ROOT);
});

afterEach(async () => {
  await storage.close();
});

test('defaults its root to .context', async () => {
  const fs = new FakeDataAdapter();
  await new ObsidianStorageAdapter(fs).saveState(new Uint8Array([7]));
  expect(await fs.exists('.context/state')).toBe(true);
});

describe('sanitizeOid', () => {
  test('lowercases hex', () => {
    expect(sanitizeOid('AABB')).toBe('aabb');
  });
  test('rejects non-hex', () => {
    expect(() => sanitizeOid('zzzz')).toThrow(/invalid object id/);
    expect(() => sanitizeOid('../etc/passwd')).toThrow();
  });
});

describe('object store', () => {
  test('get returns null when missing; put → has → get round-trips', async () => {
    expect(await storage.getObject('ab12')).toBeNull();
    expect(await storage.hasObject('ab12')).toBe(false);
    await storage.putObject('AB12', new Uint8Array([1, 2, 3]));
    expect(await storage.hasObject('ab12')).toBe(true);
    expect(Array.from((await storage.getObject('ab12')) as Uint8Array)).toEqual([1, 2, 3]);
  });
  test('listObjectOids returns basenames', async () => {
    expect(await storage.listObjectOids()).toEqual([]);
    await storage.putObject('aa', new Uint8Array([1]));
    await storage.putObject('bb', new Uint8Array([2]));
    expect((await storage.listObjectOids()).sort()).toEqual(['aa', 'bb']);
  });
});

describe('named blobs', () => {
  test('state round-trips and creates the .context root', async () => {
    expect(await storage.loadState()).toBeNull();
    await storage.saveState(new Uint8Array([1, 2, 3]));
    expect(await adapter.exists(ROOT)).toBe(true);
    expect(Array.from((await storage.loadState()) as Uint8Array)).toEqual([1, 2, 3]);
  });
  test('saveState twice replaces previous bytes (atomic-rename branch)', async () => {
    await storage.saveState(new Uint8Array([1]));
    await storage.saveState(new Uint8Array([2, 2]));
    expect(Array.from((await storage.loadState()) as Uint8Array)).toEqual([2, 2]);
  });
  test('frontier / snapshots / identity round-trip', async () => {
    await storage.saveFrontier(new Uint8Array([9]));
    expect(Array.from((await storage.loadFrontier()) as Uint8Array)).toEqual([9]);
    const snap = new TextEncoder().encode('{"snapshots":[]}');
    await storage.saveSnapshots(snap);
    expect(new TextDecoder().decode((await storage.loadSnapshots()) as Uint8Array)).toBe(
      '{"snapshots":[]}',
    );
    await storage.saveIdentitySeed(new Uint8Array(32).fill(7));
    const back = (await storage.loadIdentitySeed()) as Uint8Array;
    expect(back.length).toBe(32);
    expect(back[0]).toBe(7);
  });
});

describe('reset semantics — zero-length is reported as null', () => {
  test('state / frontier / snapshots / identity', async () => {
    await storage.saveState(new Uint8Array(0));
    expect(await storage.loadState()).toBeNull();
    await storage.saveFrontier(new Uint8Array(0));
    expect(await storage.loadFrontier()).toBeNull();
    await storage.saveSnapshots(new Uint8Array(0));
    expect(await storage.loadSnapshots()).toBeNull();
    await storage.saveIdentitySeed(new Uint8Array(0));
    expect(await storage.loadIdentitySeed()).toBeNull();
  });
});

describe('close + ensureDir', () => {
  test('close is a no-op', async () => {
    await storage.close();
    await storage.close();
  });
  test('ensureDir handles already-present directories', async () => {
    await adapter.mkdir('.obsidian');
    await storage.saveState(new Uint8Array([1]));
    expect(await storage.loadState()).not.toBeNull();
  });
  test('ensureDir handles empty path early-return', async () => {
    const s = new ObsidianStorageAdapter(adapter, '');
    await s.saveState(new Uint8Array([1]));
    expect(await s.loadState()).not.toBeNull();
  });
});
