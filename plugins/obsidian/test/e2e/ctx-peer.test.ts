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
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import {
  Identity,
  Vault,
  type VaultEvent,
  formatCspIdentity,
  memoryStorage,
  parseCspIdentity,
} from '@csp/sdk/web-init';
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

async function startPeer(tag: string, extraWatchArgs: string[] = []): Promise<Peer> {
  const home = mkdtempSync(join(tmpdir(), `csp-ob-home-${tag}-`));
  const dir = mkdtempSync(join(tmpdir(), `csp-ob-vault-${tag}-`));
  const env = { ...process.env, HOME: home, CTX_DIR: dir, CTX_LOG: 'error' };
  const init = spawnSync(ctxBin, ['init', '--vault-id', `ob-${tag}`], { env, encoding: 'utf8' });
  if (init.status !== 0) throw new Error(`ctx init failed: ${init.stderr}`);

  const watch = spawn(
    ctxBin,
    ['watch', '--listen', '127.0.0.1:0', '--no-tls', '--debounce-ms', '250', ...extraWatchArgs],
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
    expect(key.body?.trim()).toMatch(/^[0-9a-f]{64}$/); // ctx-interop hex seed
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

describe('ctx-side folder rename → Obsidian removal', () => {
  test('renaming a folder in the CLI watcher drops the old folder in Obsidian', async () => {
    const peer = await startPeer('d');
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
    const names = ['a.md', 'b.md', 'c.md'];

    // The reported scenario: the folder ALREADY exists on the CLI node
    // before this device connects, so it arrives via the initial reconcile
    // (clone/catch-up), NOT via a live applyRemoteState pass.
    mkdirSync(join(peer.dir, 'Old'), { recursive: true });
    for (const n of names) writeFileSync(join(peer.dir, 'Old', n), `# ${n}\n`);
    await new Promise((r) => setTimeout(r, 1500)); // let ctx debounce-commit

    await controller.start({ connect: true });
    await waitFor(() => (controller.state === 'connected' ? true : undefined));
    await waitFor(async () => {
      for (const n of names) {
        const af = vault.getAbstractFileByPath(`Old/${n}`);
        if (!af) return undefined;
        if ((await vault.read(af as FakeTFile)) !== `# ${n}\n`) return undefined;
      }
      return true;
    });

    // ctx side renames the folder Old/ → New/ (the reported scenario).
    mkdirSync(join(peer.dir, 'New'), { recursive: true });
    for (const n of names) renameSync(join(peer.dir, 'Old', n), join(peer.dir, 'New', n));
    rmSync(join(peer.dir, 'Old'), { recursive: true, force: true });

    // The plugin must mirror it: New/* present, AND Old/* + the Old folder
    // gone (not "cloned" — the bug was the old folder lingering).
    const ok = await waitFor<boolean>(() => {
      const haveNew = names.every((n) => vault.getAbstractFileByPath(`New/${n}`) !== null);
      const oldFilesGone = names.every((n) => vault.getAbstractFileByPath(`Old/${n}`) === null);
      const oldDirGone = vault.getAbstractFileByPath('Old') === null;
      return haveNew && oldFilesGone && oldDirGone ? true : undefined;
    });
    expect(ok).toBe(true);

    await controller.stop();
    identity.free();
  }, 120_000);
});

describe('reloaded vault (Vault.open, reconcile is a no-op) → CLI delete', () => {
  test('a CLI folder delete removes it in Obsidian even with nothing to reconcile', async () => {
    const peer = await startPeer('e');
    const url = `ws://127.0.0.1:${peer.port}`;
    const adapter = new FakeDataAdapter();
    const settings: CspSettings = { ...DEFAULT_SETTINGS, syncEnabled: true, peerUrl: url };
    const identity = Identity.generate();
    const names = ['p.md', 'q.md', 'r.md'];

    // CLI node already hosts a folder of files.
    mkdirSync(join(peer.dir, 'Keep'), { recursive: true });
    for (const n of names) writeFileSync(join(peer.dir, 'Keep', n), `# ${n}\n`);
    await new Promise((r) => setTimeout(r, 1500)); // let ctx debounce-commit

    // Session 1: connect, catch up, materialize into vault1, then stop —
    // this persists engine state into the shared storage adapter.
    const vault1 = new FakeVault(adapter);
    const c1 = new SyncController({
      storage: new ObsidianStorageAdapter(adapter),
      vault: vault1,
      settings,
      identity,
      saveSettings: async (s) => {
        Object.assign(settings, s);
      },
      log: () => {},
    });
    await c1.start({ connect: true });
    await waitFor(() => (c1.state === 'connected' ? true : undefined));
    await waitFor(() =>
      names.every((n) => vault1.getAbstractFileByPath(`Keep/${n}`) !== null) ? true : undefined,
    );
    await c1.stop();

    // Session 2 (reload): a fresh vault that ALREADY holds the files on disk
    // (as real Obsidian does across a reload), same storage → Vault.open.
    // reconcile sees both sides equal → a no-op (no applyOneRemoteFile).
    const vault2 = new FakeVault(adapter);
    for (const n of names) await vault2.create(`Keep/${n}`, `# ${n}\n`);
    const c2 = new SyncController({
      storage: new ObsidianStorageAdapter(adapter),
      vault: vault2,
      settings: { ...settings },
      identity,
      saveSettings: async () => {},
      log: () => {},
    });
    await c2.start({ connect: true });
    await waitFor(() => (c2.state === 'connected' ? true : undefined));

    // CLI deletes the whole folder.
    rmSync(join(peer.dir, 'Keep'), { recursive: true, force: true });

    // The reload session must still remove it in Obsidian.
    const gone = await waitFor<boolean>(() =>
      names.every((n) => vault2.getAbstractFileByPath(`Keep/${n}`) === null) &&
      vault2.getAbstractFileByPath('Keep') === null
        ? true
        : undefined,
    );
    expect(gone).toBe(true);

    await c2.stop();
    identity.free();
  }, 150_000);
});

// The faithful end-to-end: drive the REAL plugin (main.ts) exactly as
// production does — onboard, then a fresh "Obsidian reload" (a new plugin
// instance over the same persisted sidecar), and assert it AUTO-connects
// and that ordinary edits + a folder delete (one Obsidian-style folder
// event through main's registered listeners) reach the real ctx peer.
// No test code ever passes `{ connect: true }` — the connect must come from
// the production default path. This is the test class that was missing.
describe('real plugin reload path ⇄ real ctx', () => {
  test('auto-connects on reload; create + folder-delete reach ctx via real wiring', async () => {
    const peer = await startPeer('f');
    const url = `ws://127.0.0.1:${peer.port}`;
    const adapter = new FakeDataAdapter();
    const keyStore: { body: string | null; pub: string | null } = { body: null, pub: null };
    const io: IdentityIO = {
      async read() {
        return keyStore.body;
      },
      async write(b, p) {
        keyStore.body = b;
        keyStore.pub = p;
      },
      describe: () => '<mem>',
    };
    const manifest = { id: 'context-sync', name: 'Context', version: '0.1.0' };

    // Session 1 — onboard via the wizard's connect flow (this writes the
    // sidecar and connects once).
    const vaultA = new FakeVault(adapter);
    const appA = new App(vaultA);
    // biome-ignore lint/suspicious/noExplicitAny: shim App/manifest cast
    const a = new ContextSyncPlugin(appA as any, manifest as any);
    a.identityIOOverride = io;
    await a.onload();
    await a.runSetup({ mode: 'connect', peerUrl: url });
    expect(a.isConfigured()).toBe(true);
    await a.onunload();

    // Session 2 — a genuine Obsidian RELOAD: brand-new plugin instance over
    // the SAME persisted sidecar + identity + engine state. Nothing forces
    // a connect; it must come from the (now correct) default.
    const vaultB = new FakeVault(adapter);
    const appB = new App(vaultB);
    // biome-ignore lint/suspicious/noExplicitAny: shim App/manifest cast
    const b = new ContextSyncPlugin(appB as any, manifest as any);
    b.identityIOOverride = io;
    await b.onload();
    expect(b.isConfigured()).toBe(true);
    // biome-ignore lint/suspicious/noExplicitAny: shim flushLayoutReady
    (appB as any).flushLayoutReady();
    await waitFor(() => (b.controller?.state === 'connected' ? true : undefined));

    // Create a file through the REAL Obsidian event → main listener → bridge.
    await vaultB.createFolder('Live');
    await vaultB.create('Live/a.md', 'alpha');
    await vaultB.create('Live/b.md', 'beta');
    await waitFor(() => {
      try {
        return readFileSync(join(peer.dir, 'Live', 'a.md'), 'utf8') === 'alpha' &&
          readFileSync(join(peer.dir, 'Live', 'b.md'), 'utf8') === 'beta'
          ? true
          : undefined;
      } catch {
        return undefined;
      }
    });

    // Delete the whole folder with ONE Obsidian folder-level `delete` event
    // (FakeVault models Obsidian: no per-child events).
    const folder = vaultB.getAbstractFileByPath('Live');
    expect(folder).not.toBeNull();
    // biome-ignore lint/suspicious/noExplicitAny: FakeTFolder is a TAbstractFile
    await vaultB.delete(folder as any);
    const removed = await waitFor<boolean>(() =>
      !existsSync(join(peer.dir, 'Live', 'a.md')) && !existsSync(join(peer.dir, 'Live', 'b.md'))
        ? true
        : undefined,
    );
    expect(removed).toBe(true);

    await b.onunload();
  }, 150_000);

  test('single-file delete via the real reload path reaches ctx', async () => {
    const peer = await startPeer('g');
    const url = `ws://127.0.0.1:${peer.port}`;
    const adapter = new FakeDataAdapter();
    const ks: { body: string | null; pub: string | null } = { body: null, pub: null };
    const io: IdentityIO = {
      async read() {
        return ks.body;
      },
      async write(b2, p) {
        ks.body = b2;
        ks.pub = p;
      },
      describe: () => '<mem>',
    };
    const manifest = { id: 'context-sync', name: 'Context', version: '0.1.0' };

    const vaultA = new FakeVault(adapter);
    // biome-ignore lint/suspicious/noExplicitAny: shim casts
    const a = new ContextSyncPlugin(new App(vaultA) as any, manifest as any);
    a.identityIOOverride = io;
    await a.onload();
    await a.runSetup({ mode: 'connect', peerUrl: url });
    await waitFor(() => (a.controller?.state === 'connected' ? true : undefined));

    const f = await vaultA.create('solo.md', 'hi');
    await waitFor(() => {
      try {
        return readFileSync(join(peer.dir, 'solo.md'), 'utf8') === 'hi' ? true : undefined;
      } catch {
        return undefined;
      }
    });

    await vaultA.delete(f);
    const removed = await waitFor<boolean>(() =>
      existsSync(join(peer.dir, 'solo.md')) ? undefined : true,
    );
    expect(removed).toBe(true);
    await a.onunload();
  }, 150_000);
});

describe('empty folders sync both directions ⇄ real ctx (§11 .keep)', () => {
  test('CLI empty dir → Obsidian, Obsidian empty dir → CLI, .keep lifecycle', async () => {
    const peer = await startPeer('h');
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
    const bridge = controller.getBridge();
    if (!bridge) throw new Error('no bridge');

    // Direction Two: an empty folder created on the CLI node appears in
    // Obsidian (as the folder + its hidden .keep).
    mkdirSync(join(peer.dir, 'FromCli'), { recursive: true });
    await waitFor(() =>
      vault.getAbstractFileByPath('FromCli') !== null &&
      vault.getAbstractFileByPath('FromCli/.keep') !== null
        ? true
        : undefined,
    );

    // Lifecycle: a real file lands in it → .keep deterministically dropped,
    // folder stays (now has the real file).
    writeFileSync(join(peer.dir, 'FromCli', 'note.md'), 'hello');
    await waitFor(() =>
      vault.getAbstractFileByPath('FromCli/note.md') !== null &&
      vault.getAbstractFileByPath('FromCli/.keep') === null
        ? true
        : undefined,
    );
    expect(vault.getAbstractFileByPath('FromCli')).not.toBeNull();

    // Direction One: an empty folder created in Obsidian appears on the CLI
    // node (a real dir with a .keep), via the real bridge folder handler.
    await vault.createFolder('FromObs');
    const folder = vault.getAbstractFileByPath('FromObs');
    if (!folder) throw new Error('FromObs folder missing');
    await bridge.handleObsidianWrite(folder);
    await waitFor(() => (existsSync(join(peer.dir, 'FromObs', '.keep')) ? true : undefined));
    expect(existsSync(join(peer.dir, 'FromObs'))).toBe(true);

    await controller.stop();
    identity.free();
  }, 150_000);
});

// The device key at ~/.context/id_ed25519 is ONE key shared by `ctx` and
// the plugin (spec §10). This proves the on-disk format is interoperable
// both ways — the bug the user hit (csp-identity-v1 vs ctx's hex seed).
describe('device-key interop with real ctx (§10 one shared key)', () => {
  const pubBlob = (sshLine: string) => sshLine.trim().split(/\s+/)[1];

  test('SDK-written key is read by ctx, and ctx-written key is read by the SDK', async () => {
    // 1) The SDK writes the key file → ctx must resolve the SAME identity.
    const home = mkdtempSync(join(tmpdir(), 'csp-idi-'));
    mkdirSync(join(home, '.context'), { recursive: true });
    const kf = join(home, '.context', 'id_ed25519');
    const id = Identity.generate();
    const sdkPub = id.pubkey().toSshString();
    writeFileSync(kf, formatCspIdentity(id.seed()), { mode: 0o600 });
    id.free();
    const r = spawnSync(ctxBin, ['key', '--identity', kf], { encoding: 'utf8' });
    expect(r.status).toBe(0);
    expect(pubBlob(r.stdout)).toBe(pubBlob(sdkPub));

    // 2) ctx generates its own key file → the SDK must resolve the SAME id.
    const kf2 = join(home, 'ctx_key');
    const g = spawnSync(ctxBin, ['key', '--identity', kf2], { encoding: 'utf8' });
    expect(g.status).toBe(0);
    const back = Identity.fromSeed(parseCspIdentity(readFileSync(kf2, 'utf8')));
    expect(pubBlob(back.pubkey().toSshString())).toBe(pubBlob(g.stdout));
    back.free();

    rmSync(home, { recursive: true, force: true });
  }, 60_000);
});

// A peer that rejects this device's key closes the socket cleanly before
// the handshake — the SDK must NOT loop silently forever; it must surface
// an actionable, terminal error and stop (the bug the user hit).
describe('handshake rejection fails fast with an actionable error', () => {
  test('unauthorized device → terminal error mentioning `ctx authorize`, loop stops', async () => {
    const foreign = Identity.generate();
    const foreignSsh = foreign.pubkey().toSshString();
    foreign.free();
    // --no-tofu + a foreign authorized key → our device is always rejected.
    const peer = await startPeer('rej', ['--no-tofu', '--authorized-keys', foreignSsh]);
    const url = `ws://127.0.0.1:${peer.port}`;

    const v = await Vault.create({
      storage: memoryStorage(),
      identity: Identity.generate(),
      peerUrl: url,
    });
    const events: VaultEvent[] = [];
    v.subscribe((e) => events.push(e));

    // connectWithReconnect must RESOLVE (loop gave up) — not hang forever.
    await v.connectWithReconnect({
      maxHandshakeFailures: 3,
      initialBackoffMs: 50,
      maxBackoffMs: 100,
    });

    expect(events.some((e) => e.kind === 'connected')).toBe(false);
    // It must give up with the terminal, actionable error (not loop forever
    // and not stay silent) — the message tells the user exactly what to do.
    const messages = events
      .filter((e): e is { kind: 'error'; message: string } => e.kind === 'error')
      .map((e) => e.message);
    expect(messages.some((m) => /ctx authorize/.test(m))).toBe(true);
    await v.close();
  }, 60_000);
});
