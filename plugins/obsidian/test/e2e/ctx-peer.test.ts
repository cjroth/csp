// REAL-PEER e2e — the truth oracle for the Obsidian plugin. Spawns the
// actual `ctx` binary as a full-node listener and drives the plugin's REAL
// `SyncController` + `ContextSyncPlugin` (the real wasm engine = the same
// Rust core as `ctx`) over a real WebSocket. This proves the plugin
// converges with `ctx` for real — not via the in-process mock double.
//
//   Peer A (SyncController, real ObsidianStorageAdapter + FakeVault):
//     1. connect over ws://
//     2. Obsidian → ctx : a vault write materializes in ctx's working dir
//     3. ctx → Obsidian : a file written in ctx's dir lands in the vault
//     4. delete propagates (tombstone materializes in ctx's dir)
//     5. restart persistence: a fresh controller re-opens .context/state
//
//   Peer B (full ContextSyncPlugin):
//     6. the "connect to an existing vault" onboarding flow reaches a real
//        peer end to end (runSetup connect-mode → onboarded), then a vault
//        event round-trips to ctx
//
// Plaintext ws:// + `--no-tls` like the SDK parity test / Rust harness.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { Identity } from '@csp/sdk/web-init';
import type { IdentityIO } from '../../src/identity-store.js';
import ContextSyncPlugin from '../../src/main.js';
import { type CspSettings, DEFAULT_SETTINGS } from '../../src/settings.js';
import { ObsidianStorageAdapter } from '../../src/storage-adapter.js';
import { SyncController } from '../../src/sync-controller.js';
import { App } from '../mocks/obsidian-shim.js';
import { FakeDataAdapter, type FakeTFile, FakeVault } from '../mocks/obsidian.js';

// plugins/obsidian/test/e2e → test → obsidian → plugins → repo root
const repoRoot = resolve(import.meta.dir, '..', '..', '..', '..');
let ctxBin = '';

interface Peer {
  home: string;
  dir: string;
  port: number;
  // biome-ignore lint/suspicious/noExplicitAny: child process handle
  watch: any;
}
const peers: Peer[] = [];

async function waitFor<T>(f: () => T | undefined | Promise<T | undefined>, ms = 25000): Promise<T> {
  const start = Date.now();
  while (Date.now() - start < ms) {
    const v = await f();
    if (v !== undefined) return v;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error('timeout');
}

async function startPeer(tag: string): Promise<Peer> {
  const home = mkdtempSync(join(tmpdir(), `csp-ob-home-${tag}-`));
  const dir = mkdtempSync(join(tmpdir(), `csp-ob-vault-${tag}-`));
  const env = { ...process.env, HOME: home, CTX_DIR: dir, CTX_LOG: 'error' };
  const init = spawnSync(ctxBin, ['init', '--vault-id', `ob-${tag}`], { env, encoding: 'utf8' });
  if (init.status !== 0) throw new Error(`ctx init failed: ${init.stderr}`);

  const watch = spawn(
    ctxBin,
    ['watch', '--listen', '127.0.0.1:0', '--no-tls', '--debounce-ms', '250'],
    { env: { ...env, CTX_LOG: 'ctx=info,csp_core=warn' } },
  );
  const port = await waitFor<number>(
    () =>
      new Promise<number | undefined>((res) => {
        const onData = (buf: Buffer) => {
          const m = buf.toString().match(/listening on ws:\/\/127\.0\.0\.1:(\d+)/);
          if (m) {
            watch.stderr.off('data', onData);
            res(Number(m[1]));
          }
        };
        watch.stderr.on('data', onData);
        setTimeout(() => res(undefined), 400);
      }),
    30000,
  );
  const peer = { home, dir, port, watch };
  peers.push(peer);
  return peer;
}

beforeAll(async () => {
  const debug = join(repoRoot, 'target', 'debug', 'ctx');
  const release = join(repoRoot, 'target', 'release', 'ctx');
  if (existsSync(debug)) ctxBin = debug;
  else if (existsSync(release)) ctxBin = release;
  else {
    const b = spawnSync('cargo', ['build', '-p', 'ctx'], { cwd: repoRoot, encoding: 'utf8' });
    if (b.status !== 0) throw new Error(`cargo build -p ctx failed: ${b.stderr}`);
    ctxBin = debug;
  }
}, 240_000);

afterAll(() => {
  for (const p of peers) {
    try {
      p.watch?.kill('SIGTERM');
    } catch {}
    for (const d of [p.home, p.dir]) rmSync(d, { recursive: true, force: true });
  }
});

describe('plugin SyncController ⇄ real ctx peer', () => {
  test('connect, bidirectional sync, delete, and restart persistence', async () => {
    const peer = await startPeer('a');
    const url = `ws://127.0.0.1:${peer.port}`;
    const adapter = new FakeDataAdapter();
    const vault = new FakeVault(adapter);
    const settings: CspSettings = {
      ...DEFAULT_SETTINGS,
      syncEnabled: true,
      peerUrl: url,
    };
    const identity = Identity.generate();
    const controller = new SyncController({
      storage: new ObsidianStorageAdapter(adapter),
      vault,
      settings,
      identity,
      saveSettings: async (s) => {
        Object.assign(settings, s);
      },
      log: () => {},
    });

    await controller.start({ connect: true });
    await waitFor(() => (controller.state === 'connected' ? true : undefined));

    // 2. Obsidian → ctx: author in the plugin's vault, push via the bridge.
    const f = await vault.create('from-plugin.md', '# hello from obsidian\n');
    await controller.getBridge()?.handleObsidianWrite(f);
    const onCtx = await waitFor<string>(() => {
      try {
        return readFileSync(join(peer.dir, 'from-plugin.md'), 'utf8');
      } catch {
        return undefined;
      }
    });
    expect(onCtx).toBe('# hello from obsidian\n');

    // 3. ctx → Obsidian: ctx's watcher auto-commits a file in its dir;
    //    the controller's tree-changed handler must mirror it into the vault.
    writeFileSync(join(peer.dir, 'from-ctx.md'), 'hello from ctx');
    const inVault = await waitFor<string>(async () => {
      const af = vault.getAbstractFileByPath('from-ctx.md');
      return af ? await vault.read(af as FakeTFile) : undefined;
    });
    expect(inVault).toBe('hello from ctx');

    // 4. delete on the Obsidian side → tombstone materializes in ctx's dir.
    await vault.delete(f);
    await controller.getBridge()?.handleObsidianDelete(f);
    await waitFor(() => (existsSync(join(peer.dir, 'from-plugin.md')) ? undefined : true));
    expect(existsSync(join(peer.dir, 'from-plugin.md'))).toBe(false);

    // 5. restart persistence: stop, then a fresh controller on the SAME
    //    storage must Vault.open() and re-materialize the converged tree.
    await controller.stop();
    const vault2 = new FakeVault(adapter);
    const controller2 = new SyncController({
      storage: new ObsidianStorageAdapter(adapter),
      vault: vault2,
      settings: { ...settings },
      identity,
      saveSettings: async () => {},
      log: () => {},
    });
    await controller2.prepare(); // open existing .context/state, no connect
    const restored = await waitFor<string>(async () => {
      const af = vault2.getAbstractFileByPath('from-ctx.md');
      return af ? await vault2.read(af as FakeTFile) : undefined;
    });
    expect(restored).toBe('hello from ctx');
    expect(vault2.getAbstractFileByPath('from-plugin.md')).toBeNull(); // stayed deleted
    await controller2.stop();
    identity.free();
  }, 90_000);
});

describe('ContextSyncPlugin "connect to existing vault" flow ⇄ real ctx', () => {
  test('runSetup connect-mode onboards against a real peer, then round-trips', async () => {
    const peer = await startPeer('b');
    const url = `ws://127.0.0.1:${peer.port}`;
    const adapter = new FakeDataAdapter();
    const vault = new FakeVault(adapter);
    const app = new App(vault);
    // biome-ignore lint/suspicious/noExplicitAny: shim App vs real obsidian type
    const plugin = new ContextSyncPlugin(
      app as any,
      {
        id: 'context-sync',
        name: 'Context',
        version: '0.1.0',
        // biome-ignore lint/suspicious/noExplicitAny: shim manifest cast
      } as any,
    );

    // Keep the device key out of the real ~/.context (the connect flow
    // generates + persists it through this seam).
    const key: { body: string | null; pub: string | null } = { body: null, pub: null };
    const io: IdentityIO = {
      async read() {
        return key.body;
      },
      async write(b, p) {
        key.body = b;
        key.pub = p;
      },
      describe: () => '<mem>',
    };
    plugin.identityIOOverride = io;

    await plugin.onload();
    expect(plugin.isConfigured()).toBe(false);

    // The exact "connect to an existing vault" path the user selects in the
    // wizard — against a real ctx listener (TOFU-admits the new device key).
    await plugin.runSetup({ mode: 'connect', peerUrl: url });

    expect(plugin.isConfigured()).toBe(true);
    expect(plugin.settings.onboarded).toBe(true);
    expect(plugin.controller?.state).toBe('connected');
    expect(key.body?.startsWith('csp-identity-v1 ')).toBe(true);
    expect(key.pub?.startsWith('ssh-ed25519 ')).toBe(true);

    // A vault event now round-trips to the real peer through the plugin's
    // registered Obsidian listeners + bridge.
    await vault.create('onboarded.md', 'via the connect flow\n');
    const onCtx = await waitFor<string>(() => {
      try {
        return readFileSync(join(peer.dir, 'onboarded.md'), 'utf8');
      } catch {
        return undefined;
      }
    });
    expect(onCtx).toBe('via the connect flow\n');

    await plugin.onunload();
    expect(plugin.controller).toBeNull();
  }, 90_000);
});

describe('folder rename + rename-back ⇄ real ctx peer', () => {
  test('a folder rename and its reverse both materialize on the peer', async () => {
    const peer = await startPeer('c');
    const url = `ws://127.0.0.1:${peer.port}`;
    const adapter = new FakeDataAdapter();
    const vault = new FakeVault(adapter);
    const settings: CspSettings = { ...DEFAULT_SETTINGS, syncEnabled: true, peerUrl: url };
    const identity = Identity.generate();
    const controller = new SyncController({
      storage: new ObsidianStorageAdapter(adapter),
      vault,
      settings,
      identity,
      saveSettings: async (s) => {
        Object.assign(settings, s);
      },
      log: () => {},
    });
    await controller.start({ connect: true });
    await waitFor(() => (controller.state === 'connected' ? true : undefined));

    const names = ['a.md', 'b.md', 'c.md', 'd.md', 'e.md'];

    // Populate a folder of files (Obsidian create events → bridge push).
    for (const n of names) {
      const f = await vault.create(`Notes/${n}`, `# ${n}\n`);
      await controller.getBridge()?.handleObsidianWrite(f);
    }
    await waitFor(() =>
      names.every((n) => existsSync(join(peer.dir, 'Notes', n))) ? true : undefined,
    );

    // Rename the folder Notes/ → Renamed/ — Obsidian fires one rename per
    // child file; the bridge pushes each, the SDK coalesces them.
    async function renameFolder(from: string, to: string): Promise<void> {
      for (const n of names) {
        const file = vault.getAbstractFileByPath(`${from}/${n}`);
        if (!file) throw new Error(`missing ${from}/${n}`);
        await vault.rename(file, `${to}/${n}`);
        await controller.getBridge()?.handleObsidianRename(file, `${from}/${n}`);
      }
    }

    await renameFolder('Notes', 'Renamed');
    await waitFor(() =>
      names.every((n) => existsSync(join(peer.dir, 'Renamed', n))) &&
      !existsSync(join(peer.dir, 'Notes'))
        ? true
        : undefined,
    );
    expect(existsSync(join(peer.dir, 'Notes'))).toBe(false); // bug #1: no empty old dir

    // Rename it BACK Renamed/ → Notes/ — this is the reported "doesn't sync
    // at all" case.
    await renameFolder('Renamed', 'Notes');
    const back = await waitFor<boolean>(() =>
      names.every((n) => existsSync(join(peer.dir, 'Notes', n))) &&
      !existsSync(join(peer.dir, 'Renamed'))
        ? true
        : undefined,
    );
    expect(back).toBe(true);
    for (const n of names) {
      expect(readFileSync(join(peer.dir, 'Notes', n), 'utf8')).toBe(`# ${n}\n`);
    }
    expect(existsSync(join(peer.dir, 'Renamed'))).toBe(false);

    // Rapid round-trip: rename out and straight back with NO wait between
    // (stresses the SDK commit-coalescing debounce). The net end state must
    // still converge — files under Notes/, no stray Tmp/.
    await renameFolder('Notes', 'Tmp');
    await renameFolder('Tmp', 'Notes');
    const settled = await waitFor<boolean>(() =>
      names.every((n) => existsSync(join(peer.dir, 'Notes', n))) &&
      !existsSync(join(peer.dir, 'Tmp'))
        ? true
        : undefined,
    );
    expect(settled).toBe(true);

    await controller.stop();
    identity.free();
  }, 120_000);
});
