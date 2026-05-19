// The real CSP thin-node `Vault` — a thin shim over the **one Rust engine**
// (`WasmEngine` = `csp_core::MemEngine` + the shared sans-IO `Session`,
// §16). No protocol/merge logic here: it owns a file-level working map (the
// host plugin's view), drives `engine.commit_from_files` /
// `materialize_plan` (§5.6) and the `session_start`/`session_feed` loop over
// an injected WebSocket transport, and persists `engine.to_bytes()` via the
// host StorageAdapter. The plugin computes its own byte-identical `main`
// (same code as `ctx`).

import { WasmEngine as EngineCtor, type WasmEngine, wireDecode, wireEncode } from '#engine';
import { defaultTransport } from './transport-ws.js';
import type { FileMeta, ReconnectOptions, Snapshot, TransportConn, VaultEvent } from './types.js';
import type { CloneOptions, CreateOptions, Identity, OpenOptions, Vault } from './vault.js';

const enc = new TextEncoder();
const dec = new TextDecoder();

function filesToJson(files: Map<string, string>): string {
  const obj: Record<string, number[]> = {};
  for (const [p, c] of files) obj[p] = Array.from(enc.encode(c));
  return JSON.stringify(obj);
}

function uuidv4(): string {
  const b = new Uint8Array(16);
  crypto.getRandomValues(b);
  b[6] = ((b[6] as number) & 0x0f) | 0x40;
  b[8] = ((b[8] as number) & 0x3f) | 0x80;
  const h = Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
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
    await v.materializeFromMain(); // pull the merged tree into the working map
    return v;
  }

  /** `ctx clone <url>` (§17): probe the peer for its vault id + key (the
   * shared wire codec, no protocol reimplementation), create the engine for
   * that vault, trust the peer's authoring key, then the caller connects. */
  static async clone(opts: CloneOptions): Promise<RealVault> {
    const seed = await seedOf(opts);
    const transport = opts.transport ?? defaultTransport();
    const probeId = EngineCtor.create(seed, 'probe', '');
    const conn = await transport.connect(opts.peerUrl);
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
    await this.commit();
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
    if (this.files.delete(path)) await this.commit();
  }
  async renameFile(from: string, to: string): Promise<void> {
    const c = this.files.get(from);
    if (c === undefined) return;
    this.files.set(to, c);
    this.files.delete(from);
    await this.commit();
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
    this.files.clear();
    for (const [p, bytes] of Object.entries(tree)) {
      this.files.set(p, dec.decode(Uint8Array.from(bytes)));
    }
    await this.commit(); // restore-as-edit (§8)
    this.emit({ kind: 'tree-changed' });
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
    let attempt = 0;
    while (!signal.aborted && attempt < max) {
      attempt += 1;
      try {
        await this.runSession(url);
        attempt = 0;
      } catch (e) {
        if (signal.aborted) return;
        this.emit({ kind: 'error', message: `connect failed (attempt ${attempt}): ${e}` });
      }
      if (signal.aborted) return;
      this.connected = false;
      this.emit({ kind: 'disconnected', reason: 'connection closed' });
      const delay = Math.min(initial * 2 ** Math.min(attempt - 1, 20), cap);
      await sleep(delay, signal);
    }
  }

  private async runSession(url: string): Promise<void> {
    const transport = this.opts.transport ?? defaultTransport();
    this.emit({ kind: 'connecting', url });
    const conn = await transport.connect(url);
    this.conn = conn;
    try {
      const cb = conn.channelBinding() ?? new Uint8Array();
      await conn.send(this.engine.session_start(cb));
      for await (const frame of conn.recv()) {
        const step = JSON.parse(this.engine.session_feed(frame)) as {
          out: number[][];
          integrated: number;
          established: boolean;
        };
        for (const m of step.out) await conn.send(Uint8Array.from(m));
        if (step.established && !this.connected) {
          this.connected = true;
          this.emit({ kind: 'connected', peer_pubkey: new Uint8Array() });
        }
        if (step.integrated > 0) {
          await this.materializeFromMain();
          await this.persist();
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

  /** Author a primitive from the working map (§5.6 reconcile-by-content is
   * inside the engine); on a new primitive, persist and live-push it (§6.5). */
  private async commit(): Promise<void> {
    const prim = this.engine.commit_from_files(filesToJson(this.files));
    if (!prim) return;
    await this.persist();
    if (this.connected && this.conn) {
      const closure = JSON.parse(this.engine.export_closure(JSON.stringify([prim]))) as number[][];
      const live = wireEncode(JSON.stringify({ Live: { raws: closure } }));
      await this.conn.send(live).catch(() => {});
    }
  }

  /** Apply the §5.6 no-clobber materialize plan into the working map. */
  async materializeFromMain(): Promise<void> {
    const ops = JSON.parse(this.engine.materialize_plan(filesToJson(this.files))) as MatOp[];
    let changed = false;
    for (const o of ops) {
      if (o.op === 'write' && o.content) {
        this.files.set(o.path, dec.decode(Uint8Array.from(o.content)));
        changed = true;
      } else if (o.op === 'remove') {
        this.files.delete(o.path);
        changed = true;
      } // 'defer' → leave the user's bytes (§5.6)
    }
    if (changed) this.emit({ kind: 'tree-changed' });
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
