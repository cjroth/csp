// Engine Web Worker (issue 0010) — the `WorkerVault` ⇄ `EngineWorkerHost`
// pair, wired in-process through `linkedPorts` so the whole stack runs
// deterministically with no real Worker/DOM. The pair behaves exactly like
// a `RealVault`, just message-mediated; these exercise the worker layer's
// own surface and edge cases (the shadow, storage proxying, event ordering,
// error propagation, close semantics).

import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { Identity, type StorageAdapter, initCsp, memoryStorage } from '../src/web-init.js';
import { type Port, linkedPorts } from '../src/worker/channel.js';
import { EngineWorkerHost } from '../src/worker/engine-host.js';
import type { FromWorker, InitPayload, ToWorker } from '../src/worker/protocol.js';
import { WorkerVault } from '../src/worker/worker-vault.js';

beforeEach(async () => {
  await initCsp();
});
afterEach(() => {});

const seed = (n: number) => new Uint8Array(32).fill(n);

/** Stand up a `WorkerVault` backed by an in-process `EngineWorkerHost`. */
async function spawn(
  storage: StorageAdapter,
  payload: Partial<InitPayload> & { mode: InitPayload['mode'] },
): Promise<{ vault: WorkerVault; host: EngineWorkerHost }> {
  const [mainPort, workerPort] = linkedPorts<ToWorker, FromWorker>();
  const host = new EngineWorkerHost(workerPort as Port<FromWorker, ToWorker>);
  const full: InitPayload = {
    seed: seed(1),
    wasmBytes: new Uint8Array(0), // nodejs glue already loaded by initCsp()
    ...payload,
  };
  const vault = await WorkerVault.start(mainPort as Port<ToWorker, FromWorker>, storage, full);
  return { vault, host };
}

describe('WorkerVault — init', () => {
  test('create → identity + empty file list available synchronously', async () => {
    const { vault } = await spawn(memoryStorage(), { mode: 'create' });
    expect(vault.identityPubkeySsh().startsWith('ssh-ed25519 ')).toBe(true);
    expect(vault.listFiles()).toEqual([]);
    expect(vault.isConnected()).toBe(false);
    await vault.close();
  });

  test('the device key is deterministic from the seed', async () => {
    const a = await spawn(memoryStorage(), { mode: 'create', seed: seed(7) });
    const b = await spawn(memoryStorage(), { mode: 'create', seed: seed(7) });
    expect(a.vault.identityPubkeySsh()).toBe(b.vault.identityPubkeySsh());
    await a.vault.close();
    await b.vault.close();
  });

  test('open with no prior state rejects with a clear error', async () => {
    await expect(spawn(memoryStorage(), { mode: 'open' })).rejects.toThrow(/no vault on disk/);
  });
});

describe('WorkerVault — file operations through the worker', () => {
  test('write → the shadow updates synchronously for the bridge', async () => {
    const { vault } = await spawn(memoryStorage(), { mode: 'create' });
    const p = vault.writeTextFile('a.md', 'hello');
    // `fileExists` is synchronous — the optimistic shadow must be live
    // before the write promise even resolves.
    expect(vault.fileExists('a.md')).toBe(true);
    await p;
    expect(vault.listFiles().map((f) => f.path)).toEqual(['a.md']);
    await vault.close();
  });

  test('read round-trips to the worker', async () => {
    const { vault } = await spawn(memoryStorage(), { mode: 'create' });
    await vault.writeTextFile('a.md', 'payload');
    expect(await vault.readTextFile('a.md')).toBe('payload');
    await vault.close();
  });

  test('reading a missing file rejects with ENOENT', async () => {
    const { vault } = await spawn(memoryStorage(), { mode: 'create' });
    await expect(vault.readTextFile('ghost.md')).rejects.toThrow(/ENOENT/);
    await vault.close();
  });

  test('delete removes the file and clears the shadow', async () => {
    const { vault } = await spawn(memoryStorage(), { mode: 'create' });
    await vault.writeTextFile('gone.md', 'x');
    await vault.deleteFile('gone.md');
    expect(vault.fileExists('gone.md')).toBe(false);
    expect(vault.listFiles()).toEqual([]);
    await vault.close();
  });

  test('rename moves the file in one step', async () => {
    const { vault } = await spawn(memoryStorage(), { mode: 'create' });
    await vault.writeTextFile('old.md', 'body');
    await vault.renameFile('old.md', 'new.md');
    expect(vault.fileExists('old.md')).toBe(false);
    expect(vault.fileExists('new.md')).toBe(true);
    expect(await vault.readTextFile('new.md')).toBe('body');
    await vault.close();
  });

  test('listFiles reports size matching RealVault (string length)', async () => {
    const { vault } = await spawn(memoryStorage(), { mode: 'create' });
    await vault.writeTextFile('m.md', 'twelve chars');
    expect(vault.listFiles()[0]?.size).toBe('twelve chars'.length);
    await vault.close();
  });
});

describe('WorkerVault — persistence through the storage proxy', () => {
  test('create → write → close, then open restores via the proxy', async () => {
    const storage = memoryStorage();
    const a = await spawn(storage, { mode: 'create' });
    await a.vault.writeTextFile('keep.md', 'durable');
    await a.vault.writeTextFile('dir/nested.md', 'deep');
    await a.vault.close();

    // The worker's engine drove `saveState` etc. through the channel into
    // this same `storage` — reopening must see it.
    const b = await spawn(storage, { mode: 'open' });
    expect(
      b.vault
        .listFiles()
        .map((f) => f.path)
        .sort(),
    ).toEqual(['dir/nested.md', 'keep.md']);
    expect(await b.vault.readTextFile('keep.md')).toBe('durable');
    await b.vault.close();
  });

  test('a burst of writes with no awaits between still all persist', async () => {
    const storage = memoryStorage();
    const a = await spawn(storage, { mode: 'create' });
    const writes = [
      a.vault.writeTextFile('1.md', 'one'),
      a.vault.writeTextFile('2.md', 'two'),
      a.vault.writeTextFile('3.md', 'three'),
    ];
    await Promise.all(writes);
    await a.vault.close();
    const b = await spawn(storage, { mode: 'open' });
    expect(
      b.vault
        .listFiles()
        .map((f) => f.path)
        .sort(),
    ).toEqual(['1.md', '2.md', '3.md']);
    await b.vault.close();
  });

  test('storage errors surface as a rejected command', async () => {
    // A storage adapter whose saveState always throws — the worker's
    // proxied call must fail and the failure must reach the caller.
    const real = memoryStorage();
    const broken = new Proxy(real, {
      get(t, p, r) {
        if (p === 'saveState') return () => Promise.reject(new Error('disk full'));
        const v = Reflect.get(t, p, r);
        return typeof v === 'function' ? v.bind(t) : v;
      },
    }) as StorageAdapter;
    await expect(spawn(broken, { mode: 'create' })).rejects.toThrow(/disk full/);
  });
});

describe('WorkerVault — snapshots + events', () => {
  test('createSnapshot → listSnapshots reflects it via the observable', async () => {
    const { vault } = await spawn(memoryStorage(), { mode: 'create' });
    await vault.writeTextFile('n.md', 'v1');
    await vault.createSnapshot('s1');
    expect(vault.listSnapshots().map((s) => s.name)).toEqual(['s1']);
    await vault.close();
  });

  test('restoreToSnapshot emits tree-changed with the shadow already current', async () => {
    const { vault } = await spawn(memoryStorage(), { mode: 'create' });
    await vault.writeTextFile('n.md', 'v1');
    await vault.createSnapshot('s1');
    await vault.writeTextFile('n.md', 'v2');

    const events: string[] = [];
    let filesAtEvent: string[] = [];
    vault.subscribe((e) => {
      events.push(e.kind);
      if (e.kind === 'tree-changed') filesAtEvent = vault.listFiles().map((f) => f.path);
    });
    await vault.restoreToSnapshot('s1');
    expect(events).toContain('tree-changed');
    // The observable rode in ahead of the event, so a bridge reacting to
    // tree-changed sees a consistent file list.
    expect(filesAtEvent).toEqual(['n.md']);
    expect(await vault.readTextFile('n.md')).toBe('v1');
    await vault.close();
  });

  test('unsubscribe stops further event delivery', async () => {
    const { vault } = await spawn(memoryStorage(), { mode: 'create' });
    await vault.writeTextFile('n.md', 'v1');
    await vault.createSnapshot('s1');
    let count = 0;
    const off = vault.subscribe(() => count++);
    await vault.restoreToSnapshot('s1');
    const afterFirst = count;
    expect(afterFirst).toBeGreaterThan(0);
    off();
    await vault.restoreToSnapshot('s1');
    expect(count).toBe(afterFirst); // no new events after unsubscribe
    await vault.close();
  });
});

describe('WorkerVault — connection lifecycle (offline)', () => {
  test('an offline vault: connect is a no-op, disconnect is safe', async () => {
    const { vault } = await spawn(memoryStorage(), { mode: 'create' });
    await vault.connectWithReconnect(); // no peerUrl → immediate no-op
    expect(vault.isConnected()).toBe(false);
    await vault.disconnect();
    expect(vault.isConnected()).toBe(false);
    await vault.close();
  });
});

describe('WorkerVault — close semantics', () => {
  test('close is idempotent', async () => {
    const { vault } = await spawn(memoryStorage(), { mode: 'create' });
    await vault.close();
    await vault.close(); // second close must not throw
  });

  test('commands after close reject', async () => {
    const { vault } = await spawn(memoryStorage(), { mode: 'create' });
    await vault.close();
    await expect(vault.writeTextFile('late.md', 'x')).rejects.toThrow(/closed/);
  });
});

describe('WorkerVault — shadow consistency under interleaving', () => {
  test('overlapping write + delete + rename resolve to a coherent state', async () => {
    const storage = memoryStorage();
    const a = await spawn(storage, { mode: 'create' });
    // Issue several mutations without awaiting between them.
    const ops = [
      a.vault.writeTextFile('x.md', 'X'),
      a.vault.writeTextFile('y.md', 'Y'),
      a.vault.deleteFile('x.md'),
      a.vault.writeTextFile('z.md', 'Z'),
      a.vault.renameFile('y.md', 'y2.md'),
    ];
    await Promise.all(ops);
    expect(
      a.vault
        .listFiles()
        .map((f) => f.path)
        .sort(),
    ).toEqual(['y2.md', 'z.md']);
    await a.vault.close();
    // The persisted state agrees with the shadow.
    const b = await spawn(storage, { mode: 'open' });
    expect(
      b.vault
        .listFiles()
        .map((f) => f.path)
        .sort(),
    ).toEqual(['y2.md', 'z.md']);
    await b.vault.close();
  });
});
