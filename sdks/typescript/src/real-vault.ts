// The real CSP thin-node `Vault` — a thin shim over the **one Rust engine**
// (`WasmEngine` = `csp_core::MemEngine` + the shared sans-IO `Session`,
// §16). No protocol/merge logic here: it owns a file-level working map (the
// host plugin's view), drives the engine's incremental staging API
// (`stage_write`/`stage_remove`/`commit_staged`, §5.6) and the
// `session_start`/`session_feed` loop over
// an injected WebSocket transport, and persists `engine.to_bytes()` via the
// host StorageAdapter. The plugin computes its own byte-identical `main`
// (same code as `ctx`).

import { WasmEngine as EngineCtor, type WasmEngine, wireDecode, wireEncode } from '#engine';
import { defaultTransport } from './transport-ws.js';
import type { FileMeta, ReconnectOptions, Snapshot, TransportConn, VaultEvent } from './types.js';
import type { CloneOptions, CreateOptions, Identity, OpenOptions, Vault } from './vault.js';

const enc = new TextEncoder();
const dec = new TextDecoder();

/** Worker-side wall-clock trace prefix. Lines from `[engine-worker]` show
 * up in the host's dev console (Workers forward `console.log` to the
 * parent), so a live-sync latency hunt can read the exact gap between
 * "frame in" and "tree-changed out". */
function wkTs(): string {
  const d = new Date();
  const pad = (n: number, w = 2) => String(n).padStart(w, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`;
}
function wkLog(msg: string): void {
  // The native Node test runner doesn't have `globalThis.console.log` =
  // worker-forwarded behaviour, but logging directly is still cheap.
  console.log(`[engine-worker ${wkTs()}]`, msg);
}

/** Append `?auth_key=<urlencoded>` to a peer URL when an auth key is set
 * (CSP §10 enrollment, browser-compatible fallback path). The standard
 * `WebSocket` constructor can't set arbitrary headers, so the SDK rides
 * the query-string form the server also accepts. */
function withAuthKey(url: string, authKey: string | undefined): string {
  if (!authKey) return url;
  const sep = url.includes('?') ? '&' : '?';
  return `${url}${sep}auth_key=${encodeURIComponent(authKey)}`;
}

function uuidv4(): string {
  const b = new Uint8Array(16);
  crypto.getRandomValues(b);
  b[6] = ((b[6] as number) & 0x0f) | 0x40;
  b[8] = ((b[8] as number) & 0x3f) | 0x80;
  const h = Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

/** Decode an OpenSSH `ssh-ed25519 <b64> [comment]` line to the trailing
 * 32-byte raw key. Returns an empty Uint8Array on a missing/garbled line so
 * the caller can pass an empty pin without throwing. */
function sshPubkeyToBytes(ssh: string): Uint8Array {
  const parts = ssh.trim().split(/\s+/);
  if (parts.length < 2) return new Uint8Array();
  const blob = parts[1] as string;
  try {
    const bin = atob(blob);
    const raw = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) raw[i] = bin.charCodeAt(i);
    return raw.length >= 32 ? raw.slice(raw.length - 32) : new Uint8Array();
  } catch {
    return new Uint8Array();
  }
}

interface MatOp {
  op: 'write' | 'remove' | 'defer';
  path: string;
  content?: number[];
}

export class RealVault implements Vault {
  private files = new Map<string, string>();
  private listeners = new Set<(e: VaultEvent) => void>();
  private conn: TransportConn | null = null;
  private connected = false;
  private closed = false;
  private abort: AbortController | null = null;

  // Commit coalescing (§5.2/§6.5). A bulk host operation — a folder rename,
  // a multi-file paste, the initial reconcile — arrives as many separate
  // writeTextFile/renameFile/deleteFile calls. Authoring + persisting the
  // full engine state + live-pushing on EVERY one is O(n·stateSize) and
  // produces n primitives + n frames (it never finishes on a big folder).
  // Instead the working map is updated synchronously (reads stay correct)
  // and a single commit is debounced so a burst collapses into ONE
  // primitive + ONE persist + ONE live push. Durability/snapshot points
  // flush explicitly.
  private static readonly COMMIT_DEBOUNCE_MS = 120;
  private commitTimer: ReturnType<typeof setTimeout> | null = null;
  private commitPending = false;
  private committing = false;
  private commitWaiters: Array<() => void> = [];

  private constructor(
    private readonly engine: WasmEngine,
    private readonly seed: Uint8Array,
    private readonly storage: CreateOptions['storage'],
    private readonly opts: CreateOptions,
  ) {}

  // ---- Factories ----

  static async create(opts: CreateOptions): Promise<RealVault> {
    const seed = await seedOf(opts);
    const engine = EngineCtor.create(seed, uuidv4(), '');
    const v = new RealVault(engine, seed, opts.storage, opts);
    await v.persist();
    return v;
  }

  static async open(opts: OpenOptions): Promise<RealVault> {
    const st = await opts.storage.loadState();
    if (!st || st.length === 0) throw new Error('no vault on disk; create()/clone() first');
    const seed = await seedOf(opts);
    const engine = EngineCtor.open(seed, st, '');
    const v = new RealVault(engine, seed, opts.storage, opts);
    // The engine's `from_bytes` already seeded its staged working set from
    // the committed tree (issue 0009); mirror it into the host-side cache
    // so reads work and incremental staging has a matching baseline.
    v.seedFilesFromEngine();
    return v;
  }

  /** `ctx clone <url>` (§17): probe the peer for its vault id + key (the
   * shared wire codec, no protocol reimplementation), create the engine for
   * that vault, trust the peer's authoring key, then the caller connects. */
  static async clone(opts: CloneOptions): Promise<RealVault> {
    const seed = await seedOf(opts);
    const transport = opts.transport ?? defaultTransport();
    const probeId = EngineCtor.create(seed, 'probe', '');
    // §10: if the caller passed an auth-key for enrollment, the probe MUST
    // also present it — a listener with `CTX_AUTH_KEY` set rejects the WS
    // upgrade outright, so even the read-only probe needs the secret.
    const conn = await transport.connect(withAuthKey(opts.peerUrl, opts.authKey));
    let vaultId = '';
    let name = '';
    let serverSsh = '';
    try {
      const hello = probeId.session_start(conn.channelBinding() ?? new Uint8Array());
      await conn.send(hello);
      for await (const frame of conn.recv()) {
        const msg = JSON.parse(wireDecode(frame)) as {
          Hello?: { vault_id: string; name: string; node_ssh: string };
        };
        if (msg.Hello) {
          vaultId = msg.Hello.vault_id;
          name = msg.Hello.name;
          serverSsh = msg.Hello.node_ssh;
          break;
        }
      }
    } finally {
      probeId.free();
      await conn.close().catch(() => {});
    }
    if (!vaultId) throw new Error('clone: peer did not announce a vault id');
    const engine = EngineCtor.create(seed, vaultId, name);
    if (serverSsh) engine.authorize(serverSsh);
    const v = new RealVault(engine, seed, opts.storage, opts);
    await v.persist();
    return v;
  }

  // ---- File operations (the plugin's file-level view) ----

  async writeTextFile(path: string, content: string): Promise<string> {
    this.files.set(path, content);
    // Stage the single changed file into the engine (issue 0009) — no
    // whole-vault re-ship. Raw bytes cross the wasm boundary, not a JSON
    // integer array.
    this.engine.stage_write(path, enc.encode(content));
    this.scheduleCommit();
    return path;
  }
  async readTextFile(path: string): Promise<string> {
    const c = this.files.get(path);
    if (c === undefined) throw new Error(`ENOENT: ${path}`);
    return c;
  }
  fileExists(path: string): boolean {
    return this.files.has(path);
  }
  async deleteFile(path: string): Promise<void> {
    if (this.files.delete(path)) {
      this.engine.stage_remove(path);
      this.scheduleCommit();
    }
  }
  async renameFile(from: string, to: string): Promise<void> {
    const c = this.files.get(from);
    if (c === undefined) return;
    this.files.set(to, c);
    this.files.delete(from);
    this.engine.stage_write(to, enc.encode(c));
    this.engine.stage_remove(from);
    this.scheduleCommit();
  }
  listFiles(): FileMeta[] {
    const out: FileMeta[] = [];
    for (const [path, c] of this.files) {
      out.push({
        id: path,
        path,
        kind: 'Text',
        size: c.length,
        created_at: 0,
        updated_at: 0,
        deleted_at: null,
      });
    }
    return out;
  }

  // ---- Snapshots / recovery (CSP §8) ----

  async createSnapshot(name: string): Promise<void> {
    await this.flushCommit(); // the snapshot frontier must include pending edits
    this.engine.snapshot(name);
    await this.persist();
  }
  async deleteSnapshot(_name: string): Promise<void> {
    // Engine keeps snapshots monotonic; deletion is not a v1 operation.
  }
  listSnapshots(): Snapshot[] {
    const raw = JSON.parse(this.engine.snapshots_json()) as Record<
      string,
      { label: string; frontier: string[]; created_unix: number }
    >;
    return Object.values(raw).map((s) => ({
      name: s.label,
      created_at_ms: s.created_unix * 1000,
      frontier: s.frontier,
    }));
  }
  async restoreToSnapshot(name: string): Promise<void> {
    const tree = JSON.parse(this.engine.restore_snapshot(name)) as Record<string, number[]>;
    await this.applyRestoredTree(tree);
  }
  async restoreToTime(targetMs: number): Promise<void> {
    const tree = JSON.parse(
      this.engine.restore_time(BigInt(Math.floor(targetMs / 1000))),
    ) as Record<string, number[]>;
    await this.applyRestoredTree(tree);
  }

  private async applyRestoredTree(tree: Record<string, number[]>): Promise<void> {
    await this.flushCommit(); // author any pending edits before re-authoring
    const next = new Map<string, string>();
    for (const [p, bytes] of Object.entries(tree)) {
      next.set(p, dec.decode(Uint8Array.from(bytes)));
    }
    // Stage the old→new delta into the engine so the staged working set
    // matches the restored tree before `commit_staged` (issue 0009).
    for (const p of this.files.keys()) {
      if (!next.has(p)) this.engine.stage_remove(p);
    }
    for (const [p, c] of next) this.engine.stage_write(p, enc.encode(c));
    this.files = next;
    await this.commitNow(); // restore-as-edit (§8)
    this.emit({ kind: 'tree-changed' });
  }

  /** Seed the host-side file cache from the engine's staged working set —
   * used once on `open()`, where `from_bytes` already restored the engine's
   * working set from the committed tree (issue 0009). */
  private seedFilesFromEngine(): void {
    const dump = JSON.parse(this.engine.working_files_json()) as Record<string, number[]>;
    this.files.clear();
    for (const [p, bytes] of Object.entries(dump)) {
      this.files.set(p, dec.decode(Uint8Array.from(bytes)));
    }
  }

  // ---- Connection: drive the one shared Session over the transport ----

  async connectWithReconnect(opts: ReconnectOptions = {}): Promise<void> {
    const url = this.opts.peerUrl;
    if (!url) return; // offline-first local-only (§7)
    this.abort = new AbortController();
    const signal = this.abort.signal;
    const initial = opts.initialBackoffMs ?? 500;
    const cap = opts.maxBackoffMs ?? 30_000;
    const max = opts.maxAttempts ?? Number.POSITIVE_INFINITY;
    const maxHandshakeFailures = opts.maxHandshakeFailures ?? 5;
    let attempt = 0;
    let handshakeFailures = 0;
    while (!signal.aborted && attempt < max) {
      attempt += 1;
      let established = false;
      try {
        await this.runSession(url, () => {
          established = true;
        });
      } catch (e) {
        if (signal.aborted) return;
        this.emit({ kind: 'error', message: `connect failed (attempt ${attempt}): ${e}` });
      }
      if (signal.aborted) return;
      this.connected = false;
      if (established) {
        // A working session dropped — transient; reconnect unbounded.
        attempt = 0;
        handshakeFailures = 0;
        this.emit({ kind: 'disconnected', reason: 'connection closed' });
      } else {
        // The peer accepted the socket but closed it before the handshake
        // completed. Repeatedly = this device's key isn't authorized on the
        // peer (or the peer is an incompatible build). Don't loop silently
        // forever — surface an actionable, terminal error and stop.
        handshakeFailures += 1;
        if (handshakeFailures >= maxHandshakeFailures) {
          this.emit({
            kind: 'error',
            message: `peer rejected this device before the handshake completed (${handshakeFailures} attempts). Authorize this device's key on the peer, then reconnect:\n  ctx authorize "${this.engine.node_ssh()}"\n(If the peer is an older or incompatible ctx build, update it.)`,
          });
          return;
        }
        this.emit({ kind: 'disconnected', reason: 'handshake not completed' });
      }
      const delay = Math.min(initial * 2 ** Math.min(attempt - 1, 20), cap);
      await sleep(delay, signal);
    }
  }

  private async runSession(url: string, onEstablished: () => void): Promise<void> {
    const transport = this.opts.transport ?? defaultTransport();
    this.emit({ kind: 'connecting', url });
    // §10: post-clone reconnects ride the pubkey path; the auth-key is
    // still appended so a listener that hasn't yet enrolled this device
    // (e.g. transient `authorized_keys` loss) can re-enroll on contact.
    const conn = await transport.connect(withAuthKey(url, this.opts.authKey));
    this.conn = conn;
    try {
      const cb = conn.channelBinding() ?? new Uint8Array();
      await conn.send(this.engine.session_start(cb));
      for await (const frame of conn.recv()) {
        // Per-frame trace lets the host see where seconds actually go on
        // the receive path — useful for the live-sync-latency hunt where
        // the wall-clock gap between "frame in" and "tree-changed out"
        // exposed an unexpectedly slow materialize+persist on the worker.
        const tFrame = wkTs();
        const step = JSON.parse(this.engine.session_feed(frame)) as {
          out: number[][];
          integrated: number;
          established: boolean;
          peer_ssh?: string;
        };
        for (const m of step.out) await conn.send(Uint8Array.from(m));
        if (step.established && !this.connected) {
          this.connected = true;
          onEstablished();
          // The engine carries the verified peer SSH key out of the handshake
          // (CSP §10). Pass it through as raw bytes so the host can render /
          // pin a stable peer identity instead of an empty placeholder.
          const peerBytes = sshPubkeyToBytes(step.peer_ssh ?? '');
          this.emit({ kind: 'connected', peer_pubkey: peerBytes });
        }
        if (step.integrated > 0) {
          wkLog(`integrate ${step.integrated} → materialize start (frame@${tFrame})`);
          await this.materializeFromMain();
          wkLog('materialize done → persist start');
          await this.persist();
          wkLog('persist done');
        }
      }
    } finally {
      this.conn = null;
      await conn.close().catch(() => {});
    }
  }

  async disconnect(): Promise<void> {
    this.abort?.abort();
    this.abort = null;
    if (this.conn) await this.conn.close().catch(() => {});
    this.conn = null;
    this.connected = false;
  }

  async close(): Promise<void> {
    if (this.closed) return;
    await this.flushCommit(); // never lose a debounced edit on shutdown
    this.closed = true;
    await this.disconnect();
    await this.persist();
    this.engine.free();
    await this.storage.close();
  }

  subscribe(listener: (e: VaultEvent) => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }
  identityPubkeySsh(): string {
    return this.engine.node_ssh();
  }
  isConnected(): boolean {
    return this.connected;
  }

  // ---- Internal ----

  private emit(e: VaultEvent): void {
    for (const l of this.listeners) {
      try {
        l(e);
      } catch {
        /* listener errors don't propagate */
      }
    }
  }

  /** Request a commit. Cheap + synchronous for the caller: a burst of host
   * file ops collapses into one debounced `commitNow` (leading-edge, same
   * shape as the bridge's apply debounce). */
  private scheduleCommit(): void {
    if (this.closed) return;
    this.commitPending = true;
    if (this.committing || this.commitTimer !== null) return;
    this.commitTimer = setTimeout(() => {
      this.commitTimer = null;
      void this.runCommit();
    }, RealVault.COMMIT_DEBOUNCE_MS);
  }

  /** Drain pending commits one at a time; ops that arrive mid-commit are
   * folded into the next pass. Resolves any `flushCommit` waiters when idle. */
  private async runCommit(): Promise<void> {
    if (this.committing) return;
    this.committing = true;
    try {
      while (this.commitPending && !this.closed) {
        this.commitPending = false;
        await this.commitNow();
      }
    } finally {
      this.committing = false;
      const waiters = this.commitWaiters;
      this.commitWaiters = [];
      for (const w of waiters) w();
    }
  }

  /** Force any scheduled/in-flight commit to complete now. Used at the
   * durability + snapshot points so a debounced edit is never lost. */
  async flushCommit(): Promise<void> {
    if (this.commitTimer !== null) {
      clearTimeout(this.commitTimer);
      this.commitTimer = null;
    }
    if (!this.commitPending && !this.committing) return;
    await new Promise<void>((resolve) => {
      this.commitWaiters.push(resolve);
      void this.runCommit();
    });
  }

  /** Author a primitive from the engine's staged working set (§5.6
   * reconcile-by-content is inside the engine); on a new primitive,
   * **live-push first**, then persist. The primitive is in the engine's
   * in-memory state regardless; persist is for local durability.
   * Persist-before-live used to hold the live frame for the full engine
   * to_bytes + storage round-trip + atomic disk write — observed at ~8 s on
   * a 450-file vault, which was most of the apparent sync latency.
   *
   * Crash-safety of the swap: if persist fails after a successful live
   * send, the peer has the primitive and the next catch-up re-integrates
   * it locally; the edit is preserved. Live-before-persist is strictly
   * less lossy in the writer-crash window than the previous order.
   */
  private async commitNow(): Promise<void> {
    wkLog('commit start');
    const prim = this.engine.commit_staged();
    if (!prim) {
      wkLog('commit non-event (no change)');
      return;
    }
    if (this.connected && this.conn) {
      const closure = JSON.parse(this.engine.export_closure(JSON.stringify([prim]))) as number[][];
      const live = wireEncode(JSON.stringify({ Live: { raws: closure } }));
      // Fire the live push immediately; persist runs after so a slow disk /
      // worker-boundary round-trip never delays a peer seeing the edit.
      await this.conn.send(live).catch(() => {});
      wkLog(`commit live sent (prim=${prim.slice(0, 12)}…)`);
    }
    await this.persist();
    wkLog('commit persist done');
  }

  /** Apply the §5.6 no-clobber materialize plan into the working map. */
  async materializeFromMain(): Promise<void> {
    // Author pending host edits first so the §5.6 plan sees them as
    // primitives (not just uncommitted working bytes) before a merge.
    await this.flushCommit();
    // `materialize_staged` plans against the engine's staged working set and
    // keeps it in step with the ops it returns (issue 0009).
    const ops = JSON.parse(this.engine.materialize_staged()) as MatOp[];
    const changes: Array<{ path: string; content: string | null }> = [];
    for (const o of ops) {
      if (o.op === 'write' && o.content) {
        const content = dec.decode(Uint8Array.from(o.content));
        this.files.set(o.path, content);
        changes.push({ path: o.path, content });
      } else if (o.op === 'remove') {
        this.files.delete(o.path);
        changes.push({ path: o.path, content: null });
      } // 'defer' → leave the user's bytes (§5.6)
    }
    // Carry the exact change set with the event so a host doesn't have to
    // re-scan + re-read every file on every materialize (it was an O(vault)
    // postMessage burst across the engine-worker boundary on every sync).
    if (changes.length) this.emit({ kind: 'tree-changed', changes });
  }

  private async persist(): Promise<void> {
    if (this.closed) return;
    await this.storage.saveState(this.engine.to_bytes());
  }
}

async function seedOf(opts: CreateOptions): Promise<Uint8Array> {
  if (opts.identity) return opts.identity.seed();
  const existing = await opts.storage.loadIdentitySeed();
  if (existing && existing.length === 32) return existing;
  const s = new Uint8Array(32);
  crypto.getRandomValues(s);
  await opts.storage.saveIdentitySeed(s);
  return s;
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((res) => {
    const t = setTimeout(res, ms);
    const onAbort = () => {
      clearTimeout(t);
      res();
    };
    if (signal.aborted) onAbort();
    else signal.addEventListener('abort', onAbort, { once: true });
  });
}

// Re-export the contract type for ergonomic single-import.
export type { Identity };
