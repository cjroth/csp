// SYNC PROTOCOL REGRESSION SUITE — real `ctx` listener + real `@csp/sdk`
// `Vault` driver over a real WebSocket. Guards the failure classes we
// already hit in production:
//
//   * chunked catch-up admits atomically (no partial known-set / no
//     divergent main on receiver / no poisoned relay) — issue 0016.
//   * author-after-clone over the relay converges and keeps the server
//     healthy — issue 0015 cascade.
//   * reset → reclone recovery works end to end — the path we walked
//     post-0.1.15.
//   * cross-peer relay convergence (3 nodes: ctx + A + B) — author on A,
//     B (already synced) sees the edit via relay.
//
// Each test gets its own fresh `ctx watch` peer + own vault dir so
// per-test state can't smear: a buggy 0.1.15-style cascade in one test
// would otherwise corrupt the shared listener and silently fail every
// subsequent test.

import { afterEach, beforeAll, describe, expect, test } from 'bun:test';
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { Identity, Vault, memoryStorage } from '../../src/web-init.js';

const repoRoot = resolve(import.meta.dir, '..', '..', '..', '..');
let ctxBin = '';

interface Peer {
  home: string;
  dir: string;
  port: number;
  /** Pre-shared auth-key — every SDK client in a test presents this
   * so ctx self-enrolls each new peer-key (CSP §10). Without it ctx
   * TOFU-admits only the FIRST peer and rejects every subsequent
   * one with `peer not authorized`, which breaks the multi-peer
   * and reset-then-reclone tests. */
  authKey: string;
  // biome-ignore lint/suspicious/noExplicitAny: child process handle
  watch: any;
}
const liveCtx: Peer[] = [];

const AUTH_KEY = 'sync-regression-test-key';

async function waitFor<T>(f: () => T | undefined | Promise<T | undefined>, ms = 30000): Promise<T> {
  const start = Date.now();
  while (Date.now() - start < ms) {
    const v = await f();
    if (v !== undefined) return v;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error('timeout');
}

/** Spawn a fresh `ctx watch` listener on a random local port. Tests own
 * their peer end-to-end — no shared vault dir, no cross-test corruption. */
async function startCtx(tag: string): Promise<Peer> {
  const home = mkdtempSync(join(tmpdir(), `csp-sync-home-${tag}-`));
  const dir = mkdtempSync(join(tmpdir(), `csp-sync-vault-${tag}-`));
  const env = {
    ...process.env,
    HOME: home,
    CTX_DIR: dir,
    CTX_LOG: 'error',
    // Pre-shared auth-key for the WS upgrade: every SDK that presents
    // this gets enrolled into authorized_keys on connect (§10). Without
    // it ctx's default TOFU policy admits only the first peer.
    CTX_AUTH_KEY: AUTH_KEY,
  };
  const init = spawnSync(ctxBin, ['init', '--vault-id', `sync-${tag}`], {
    env,
    encoding: 'utf8',
  });
  if (init.status !== 0) throw new Error(`ctx init failed: ${init.stderr}`);

  const watch = spawn(
    ctxBin,
    ['watch', '--listen', '127.0.0.1:0', '--no-tls', '--debounce-ms', '250'],
    {
      env: { ...env, CTX_LOG: 'ctx=info,csp_core=info' },
    },
  );
  // Forward every line ctx logs to test stdout, prefixed with the tag —
  // when a regression fails ("recv loop end immediately" on the SDK
  // side), the matching `session ended: ...` reason on the server side
  // is the diagnostic we need but were dropping.
  watch.stderr.on('data', (buf: Buffer) => {
    for (const line of buf.toString().split('\n')) {
      if (line) console.log(`[ctx-${tag}] ${line}`);
    }
  });
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
        setTimeout(() => res(undefined), 600);
      }),
    30000,
  );
  const peer: Peer = { home, dir, port, authKey: AUTH_KEY, watch };
  liveCtx.push(peer);
  return peer;
}

function stopCtx(p: Peer): void {
  try {
    p.watch?.kill('SIGTERM');
  } catch {}
  for (const d of [p.home, p.dir]) if (d) rmSync(d, { recursive: true, force: true });
}

beforeAll(() => {
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

afterEach(() => {
  // Reap every ctx we spun up for the test that just ran. Per-test
  // isolation: leaks across tests would defeat the regression's whole
  // point.
  while (liveCtx.length) {
    const p = liveCtx.pop();
    if (p) stopCtx(p);
  }
});

/** Hex-encoded LCG stream — valid UTF-8 (ctx's default scope drops
 * binaries) with enough entropy that zlib can't shrink the catch-up
 * closure below CATCHUP_CHUNK_BYTES. Deterministic per `idx`. */
function hexPayload(idx: number, len: number): string {
  let s = BigInt('0x9e3779b97f4a7c15') * BigInt(idx + 1);
  const parts: string[] = [];
  while (parts.join('').length < len) {
    s = (s * 6364136223846793005n + 1442695040888963407n) & ((1n << 64n) - 1n);
    parts.push(s.toString(16).padStart(16, '0'));
  }
  return parts.join('').slice(0, len);
}

describe('SDK ⇄ real ctx — sync-protocol regression', () => {
  test('a clone of a large vault (multi-frame ObjectsBatch) converges atomically', async () => {
    const peer = await startCtx('bulk-clone');
    // Pre-populate the peer's vault BEFORE the SDK connects. The watcher
    // debounces 250 ms after the last write before committing, so wait
    // long enough that all writes are in one primitive (or a few small
    // commits) — the test wants the SDK to hit catch-up against an
    // already-populated server.
    const N = 200;
    for (let i = 0; i < N; i++) {
      writeFileSync(join(peer.dir, `bulk-${String(i).padStart(4, '0')}.md`), hexPayload(i, 1400));
    }
    await new Promise((r) => setTimeout(r, 2500));

    const url = `ws://127.0.0.1:${peer.port}`;
    const v = await Vault.clone({
      storage: memoryStorage(),
      identity: Identity.generate(),
      peerUrl: url,
      authKey: peer.authKey,
    });
    void v.connectWithReconnect();

    // Catch-up must populate every file. Under 0.1.15's per-chunk
    // integrate this never converges: chunks whose folds had parents in
    // later chunks dropped admission, the receiver's known-set stayed
    // a strict subset, and `listFiles()` never reaches N.
    await waitFor<true>(() => {
      const bulkCount = v.listFiles().filter((f) => f.path.startsWith('bulk-')).length;
      return bulkCount >= N ? true : undefined;
    });

    // Spot-check: the tail file's blob round-trips. The 0.1.15 bug would
    // typically leave tail-of-batch files missing (their tree's blob
    // arrived in a chunk whose verify failed earlier).
    const last = `bulk-${String(N - 1).padStart(4, '0')}.md`;
    expect(await v.readTextFile(last)).toBe(hexPayload(N - 1, 1400));

    await v.close();
  }, 120_000);

  test('author after a chunked clone — Live edit lands on ctx; a fresh clone still works', async () => {
    const peer = await startCtx('post-clone-author');
    // Seed enough files to force a multi-frame catch-up.
    const N = 200;
    for (let i = 0; i < N; i++) {
      writeFileSync(join(peer.dir, `seed-${String(i).padStart(4, '0')}.md`), hexPayload(i, 1400));
    }
    await new Promise((r) => setTimeout(r, 2500));

    const url = `ws://127.0.0.1:${peer.port}`;
    const a = await Vault.clone({
      storage: memoryStorage(),
      identity: Identity.generate(),
      peerUrl: url,
      authKey: peer.authKey,
    });
    void a.connectWithReconnect();
    await waitFor(() => (a.isConnected() ? true : undefined));
    // Don't author until catch-up has actually populated something; we
    // want the new primitive to be parented on the SAME main the server
    // computed, which only happens after the receiver finishes
    // integrating the batch.
    await waitFor<true>(() =>
      a.listFiles().filter((f) => f.path.startsWith('seed-')).length >= N ? true : undefined,
    );

    await a.writeTextFile('post-clone-edit.md', 'authored after chunked catch-up');
    const onCtx = await waitFor<string>(() => {
      try {
        return readFileSync(join(peer.dir, 'post-clone-edit.md'), 'utf8');
      } catch {
        return undefined;
      }
    });
    expect(onCtx).toBe('authored after chunked catch-up');

    // Health check: a brand-new clone of the same server must STILL
    // succeed. Under the 0.1.15 cascade ctx's `frontier_tips` errored
    // and every new session ended right after handshake; this clone
    // hung indefinitely. Reaches `connected` + sees the new file
    // through catch-up → relay is intact.
    const b = await Vault.clone({
      storage: memoryStorage(),
      identity: Identity.generate(),
      peerUrl: url,
      authKey: peer.authKey,
    });
    void b.connectWithReconnect();
    await waitFor(() => (b.isConnected() ? true : undefined));
    const echo = await waitFor<string>(async () => {
      try {
        return await b.readTextFile('post-clone-edit.md');
      } catch {
        return undefined;
      }
    });
    expect(echo).toBe('authored after chunked catch-up');

    await a.close();
    await b.close();
  }, 180_000);

  test('reset → reclone recovery (the post-0.1.15 path)', async () => {
    const peer = await startCtx('reset-reclone');
    const url = `ws://127.0.0.1:${peer.port}`;

    // Session 1: clone, write, close. The file lives on ctx; the SDK's
    // storage holds the engine state.
    {
      const v = await Vault.clone({
        storage: memoryStorage(),
        identity: Identity.generate(),
        peerUrl: url,
        authKey: peer.authKey,
      });
      void v.connectWithReconnect();
      await waitFor(() => (v.isConnected() ? true : undefined));
      await v.writeTextFile('recover-me.md', 'before reset');
      await waitFor<true>(() => {
        try {
          return readFileSync(join(peer.dir, 'recover-me.md'), 'utf8') === 'before reset'
            ? true
            : undefined;
        } catch {
          return undefined;
        }
      });
      await v.close();
    }

    // "Reset local state" = drop the storage entirely. Modeled with a
    // brand-new memoryStorage(); on the plugin this corresponds to the
    // settings-tab "Reset local state" button + a fresh device identity.
    const v2 = await Vault.clone({
      storage: memoryStorage(),
      identity: Identity.generate(),
      peerUrl: url,
      authKey: peer.authKey,
    });
    void v2.connectWithReconnect();
    await waitFor(() => (v2.isConnected() ? true : undefined));

    // After reclone, the pre-reset file is in the engine's view.
    const post = await waitFor<string>(async () => {
      try {
        return await v2.readTextFile('recover-me.md');
      } catch {
        return undefined;
      }
    });
    expect(post).toBe('before reset');

    await v2.close();
  }, 120_000);

  test('three-peer relay convergence: edit on A propagates to B via ctx', async () => {
    // The exact production topology: a relay (ctx) + two thin clients.
    // A authors, the server admits + broadcasts via its bus, B receives
    // a `Live` frame, B integrates. This is the path the 0.1.15 cascade
    // broke (B's frame referenced a fold the server didn't have).
    const peer = await startCtx('relay');
    const url = `ws://127.0.0.1:${peer.port}`;

    const a = await Vault.clone({
      storage: memoryStorage(),
      identity: Identity.generate(),
      peerUrl: url,
      authKey: peer.authKey,
    });
    const b = await Vault.clone({
      storage: memoryStorage(),
      identity: Identity.generate(),
      peerUrl: url,
      authKey: peer.authKey,
    });
    void a.connectWithReconnect();
    void b.connectWithReconnect();
    await waitFor(() => (a.isConnected() && b.isConnected() ? true : undefined));

    // A writes; ctx admits; ctx relays to B.
    await a.writeTextFile('a-to-b.md', 'from A through the relay');
    const onB = await waitFor<string>(async () => {
      try {
        return await b.readTextFile('a-to-b.md');
      } catch {
        return undefined;
      }
    });
    expect(onB).toBe('from A through the relay');

    // Reverse direction — proves the relay is symmetric.
    await b.writeTextFile('b-to-a.md', 'from B through the relay');
    const onA = await waitFor<string>(async () => {
      try {
        return await a.readTextFile('b-to-a.md');
      } catch {
        return undefined;
      }
    });
    expect(onA).toBe('from B through the relay');

    await a.close();
    await b.close();
  }, 120_000);

  test('protocol skew: a peer speaking the wrong proto fails with a clear error, not a silent hang', async () => {
    // The PROTO_VERSION bump (v3 → v4) prevents 0.1.15 from talking to
    // 0.1.16. A real version mismatch is exercised by ctx-parity's
    // proto-skew unit test in csp-core; here we just prove the
    // handshake itself completes for matched versions (i.e. the bump
    // didn't break our own client/server pair on the happy path).
    const peer = await startCtx('proto');
    const url = `ws://127.0.0.1:${peer.port}`;
    const v = await Vault.clone({
      storage: memoryStorage(),
      identity: Identity.generate(),
      peerUrl: url,
      authKey: peer.authKey,
    });
    void v.connectWithReconnect();
    await waitFor(() => (v.isConnected() ? true : undefined));
    expect(v.isConnected()).toBe(true);
    await v.close();
  }, 60_000);
});
