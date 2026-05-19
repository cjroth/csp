// §18 PARITY e2e — the truth oracle. Spawns the **real `ctx` binary** as a
// full-node listener and drives the **real `@csp/sdk` `Vault`** (wasm engine
// = the same Rust core) as a thin peer over a real WebSocket. Proves the
// plugin path actually converges with `ctx` (not an in-process shortcut):
//   1. clone bootstrap (probe → vault id + key, TOFU-admitted)
//   2. SDK → ctx: a file authored in the SDK lands in ctx's working dir
//   3. ctx → SDK: a file written in ctx's dir lands in the SDK
//
// Builds ctx on demand (cargo). Plaintext `ws://` + `--no-tls` like the Rust
// harness (Bun's WebSocket → tungstenite server).

import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { Identity, Vault, memoryStorage } from '../../src/web-init.js';

// test/e2e → test → typescript → sdks → repo root
const repoRoot = resolve(import.meta.dir, '..', '..', '..', '..');
let ctxBin = '';
let home = '';
let vaultDir = '';
// biome-ignore lint/suspicious/noExplicitAny: child process handle
let watch: any = null;
let port = 0;

function run(args: string[], extraEnv: Record<string, string> = {}): string {
  const r = spawnSync(ctxBin, args, {
    env: { ...process.env, HOME: home, CTX_DIR: vaultDir, CTX_LOG: 'error', ...extraEnv },
    encoding: 'utf8',
  });
  if (r.status !== 0) throw new Error(`ctx ${args.join(' ')} failed: ${r.stderr}`);
  return r.stdout;
}

async function waitFor<T>(f: () => T | undefined | Promise<T | undefined>, ms = 20000): Promise<T> {
  const start = Date.now();
  while (Date.now() - start < ms) {
    const v = await f();
    if (v !== undefined) return v;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error('timeout');
}

beforeAll(async () => {
  const b = spawnSync('cargo', ['build', '-p', 'ctx'], { cwd: repoRoot, encoding: 'utf8' });
  if (b.status !== 0) throw new Error(`cargo build -p ctx failed: ${b.stderr}`);
  ctxBin = join(repoRoot, 'target', 'debug', 'ctx');

  home = mkdtempSync(join(tmpdir(), 'csp-parity-home-'));
  vaultDir = mkdtempSync(join(tmpdir(), 'csp-parity-vault-'));
  run(['init', '--vault-id', 'parity-v']);

  watch = spawn(ctxBin, ['watch', '--listen', '127.0.0.1:0', '--no-tls', '--debounce-ms', '250'], {
    env: { ...process.env, HOME: home, CTX_DIR: vaultDir, CTX_LOG: 'ctx=info,csp_core=warn' },
  });
  port = await waitFor<number>(
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
        setTimeout(() => res(undefined), 300);
      }),
  );
}, 180_000);

afterAll(() => {
  if (watch) watch.kill('SIGTERM');
  for (const d of [home, vaultDir]) if (d) rmSync(d, { recursive: true, force: true });
});

describe('SDK ⇄ real ctx parity', () => {
  test('clone + bidirectional converge over a real websocket', async () => {
    const url = `ws://127.0.0.1:${port}`;
    const v = await Vault.clone({
      storage: memoryStorage(),
      identity: Identity.generate(),
      peerUrl: url,
    });
    const events: string[] = [];
    v.subscribe((e) => events.push(e.kind));
    void v.connectWithReconnect();
    await waitFor(() => (v.isConnected() ? true : undefined));

    // SDK → ctx: author in the SDK, it must materialize in ctx's dir.
    await v.writeTextFile('from-sdk.md', 'hello from the plugin');
    const onCtx = await waitFor<string>(() => {
      try {
        return readFileSync(join(vaultDir, 'from-sdk.md'), 'utf8');
      } catch {
        return undefined;
      }
    });
    expect(onCtx).toBe('hello from the plugin');

    // ctx → SDK: a file in ctx's working dir (its watcher auto-commits)
    // must propagate into the SDK's working tree.
    writeFileSync(join(vaultDir, 'from-ctx.md'), 'hello from ctx');
    const onSdk = await waitFor<string>(async () => {
      try {
        return await v.readTextFile('from-ctx.md');
      } catch {
        return undefined;
      }
    });
    expect(onSdk).toBe('hello from ctx');

    expect(events).toContain('connected');
    expect(events).toContain('tree-changed');
    await v.close();
  }, 60000);
});
