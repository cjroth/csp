// Main-thread proxy for the engine Web Worker (issue 0010).
//
// `WorkerVault` implements the public `Vault` contract by forwarding to an
// `EngineWorkerHost` running in a Worker. It is a drop-in for `RealVault` —
// `SyncController` and the Obsidian bridge use it unchanged.
//
// The bridge makes synchronous reads (`fileExists`, `listFiles`,
// `listSnapshots`, `isConnected`, `identityPubkeySsh`); a Worker boundary is
// async-only, so `WorkerVault` keeps a synchronous *shadow* of that
// observable state. The shadow holds metadata only (paths + sizes, snapshot
// records) — file *content* always round-trips through `readTextFile`, so
// the shadow stays small even for a large vault. Local mutations update the
// shadow optimistically; the worker's authoritative `Observable` (delivered
// with every event, ahead of the event itself) reconciles it.

import type { FileMeta, ReconnectOptions, Snapshot, StorageAdapter, VaultEvent } from '../types.js';
import type { Vault as VaultContract } from '../vault.js';
import type { Port } from './channel.js';
import type {
  Command,
  CommandBody,
  FromWorker,
  InitPayload,
  LogMessage,
  Observable,
  ToWorker,
} from './protocol.js';

/** Host-side sink for worker-side log lines (issue 0013). The worker
 * mirrors its own `console.log` / `console.error` to this so an in-app
 * log viewer on mobile (where the dev console is unreachable) can show
 * what the engine is doing. The host typically forwards to its own
 * console *and* to a ring buffer the UI reads. */
export type WorkerLogSink = (entry: LogMessage) => void;

export class WorkerVault implements VaultContract {
  // ---- Synchronous shadow of the worker's observable state ----
  private shadowFiles = new Map<string, number>(); // path -> byte size
  private shadowSnapshots: Snapshot[] = [];
  private connected = false;
  private identitySsh = '';

  // ---- Command reply correlation ----
  private nextCmdId = 1;
  private readonly pending = new Map<
    number,
    { resolve: (v: string | undefined) => void; reject: (e: Error) => void }
  >();

  private readonly listeners = new Set<(e: VaultEvent) => void>();
  private closed = false;

  private constructor(
    private readonly port: Port<ToWorker, FromWorker>,
    private readonly storage: StorageAdapter,
    private readonly onLog: WorkerLogSink | null,
  ) {
    this.port.onMessage((msg) => void this.onMessage(msg));
  }

  /**
   * Build a `WorkerVault`, send `init`, and resolve once the worker has
   * stood up its `RealVault` *and* reported its first observable snapshot
   * (so `identityPubkeySsh()` / `listFiles()` are valid immediately).
   *
   * `storage` is the real main-thread `StorageAdapter`; the worker's engine
   * drives it through the channel.
   *
   * `onLog` (optional) receives every worker-side `console.log` /
   * `console.error` line — used by the in-app log viewer on mobile, where
   * the WebView dev console is unreachable (issue 0013).
   */
  static async start(
    port: Port<ToWorker, FromWorker>,
    storage: StorageAdapter,
    payload: InitPayload,
    onLog?: WorkerLogSink,
  ): Promise<WorkerVault> {
    const v = new WorkerVault(port, storage, onLog ?? null);
    const firstObservable = new Promise<void>((resolve) => {
      v.firstObservableResolve = resolve;
    });
    await v.command({ op: 'init', payload });
    await firstObservable;
    return v;
  }

  private firstObservableResolve: (() => void) | null = null;

  // ---- Incoming worker messages ----

  private async onMessage(msg: FromWorker): Promise<void> {
    if (msg.kind === 'reply') {
      const p = this.pending.get(msg.id);
      if (!p) return;
      this.pending.delete(msg.id);
      if (msg.ok) p.resolve(msg.value);
      else p.reject(new Error(msg.error ?? 'engine worker error'));
      return;
    }
    if (msg.kind === 'event') {
      // `observable` is processed before the event so the shadow is current
      // when a `tree-changed` reaches the bridge.
      if (msg.observable) this.applyObservable(msg.observable);
      if (msg.event) this.emit(msg.event);
      return;
    }
    if (msg.kind === 'storage-req') {
      await this.serviceStorage(msg.id, msg.method, msg.args);
      return;
    }
    if (msg.kind === 'log') {
      // A log sink throwing must not break the channel — the engine cannot
      // rely on the host having a useful logger.
      try {
        this.onLog?.(msg);
      } catch {
        // swallow — diagnostic, not load-bearing
      }
      return;
    }
  }

  private applyObservable(o: Observable): void {
    this.shadowFiles = new Map(o.files.map((f) => [f.path, f.size]));
    this.shadowSnapshots = o.snapshots;
    this.connected = o.connected;
    this.identitySsh = o.identitySsh;
    if (this.firstObservableResolve) {
      this.firstObservableResolve();
      this.firstObservableResolve = null;
    }
  }

  private async serviceStorage(id: number, method: string, args: unknown[]): Promise<void> {
    try {
      // The adapter surface is fixed (StorageAdapter); `method` only ever
      // comes from the worker's own ProxyStorage, never user input.
      const fn = (this.storage as unknown as Record<string, (...a: unknown[]) => unknown>)[method];
      if (typeof fn !== 'function') {
        throw new Error(`storage proxy: no such method ${method}`);
      }
      const value = await fn.apply(this.storage, args);
      this.port.post({ kind: 'storage-res', id, ok: true, value });
    } catch (err) {
      this.port.post({
        kind: 'storage-res',
        id,
        ok: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  }

  private emit(e: VaultEvent): void {
    for (const l of this.listeners) {
      try {
        l(e);
      } catch {
        // a listener throwing must not break the others
      }
    }
  }

  /** Send a command and await its reply. */
  private command(c: CommandBody): Promise<string | undefined> {
    if (this.closed && c.op !== 'close') {
      return Promise.reject(new Error('WorkerVault is closed'));
    }
    const id = this.nextCmdId++;
    return new Promise<string | undefined>((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.port.post({ kind: 'cmd', id, ...c } as Command);
    });
  }

  // ---- Vault: file operations ----

  async writeTextFile(path: string, content: string): Promise<string> {
    // Optimistic shadow update so an immediately-following sync read sees
    // it; the worker's observable reconciles authoritatively right after.
    // `size` mirrors `RealVault.listFiles` — string length, not byte length.
    this.shadowFiles.set(path, content.length);
    await this.command({ op: 'writeTextFile', path, content });
    return path;
  }

  async readTextFile(path: string): Promise<string> {
    const value = await this.command({ op: 'readTextFile', path });
    if (value === undefined) throw new Error(`ENOENT: ${path}`);
    return value;
  }

  fileExists(path: string): boolean {
    return this.shadowFiles.has(path);
  }

  async deleteFile(path: string): Promise<void> {
    this.shadowFiles.delete(path);
    await this.command({ op: 'deleteFile', path });
  }

  async renameFile(from: string, to: string): Promise<void> {
    const size = this.shadowFiles.get(from);
    if (size !== undefined) {
      this.shadowFiles.delete(from);
      this.shadowFiles.set(to, size);
    }
    await this.command({ op: 'renameFile', from, to });
  }

  listFiles(): FileMeta[] {
    const out: FileMeta[] = [];
    for (const [path, size] of this.shadowFiles) {
      out.push({
        id: path,
        path,
        kind: 'Text',
        size,
        created_at: 0,
        updated_at: 0,
        deleted_at: null,
      });
    }
    return out;
  }

  // ---- Vault: snapshots ----

  async createSnapshot(name: string): Promise<void> {
    await this.command({ op: 'createSnapshot', name });
  }
  async deleteSnapshot(name: string): Promise<void> {
    await this.command({ op: 'deleteSnapshot', name });
  }
  listSnapshots(): Snapshot[] {
    return this.shadowSnapshots.slice();
  }
  async restoreToSnapshot(name: string): Promise<void> {
    await this.command({ op: 'restoreToSnapshot', name });
  }
  async restoreToTime(targetMs: number): Promise<void> {
    await this.command({ op: 'restoreToTime', targetMs });
  }

  // ---- Vault: connection ----

  async connectWithReconnect(_opts?: ReconnectOptions): Promise<void> {
    // The worker fires its reconnect supervisor and acks immediately;
    // connection progress arrives as VaultEvents.
    await this.command({ op: 'connectWithReconnect' });
  }
  async disconnect(): Promise<void> {
    await this.command({ op: 'disconnect' });
  }
  async close(): Promise<void> {
    if (this.closed) return;
    await this.command({ op: 'close' });
    this.closed = true;
    // Any still-pending command will never get a reply now.
    for (const p of this.pending.values()) p.reject(new Error('WorkerVault closed'));
    this.pending.clear();
  }

  // ---- Vault: events / accessors ----

  subscribe(listener: (e: VaultEvent) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
  identityPubkeySsh(): string {
    return this.identitySsh;
  }
  isConnected(): boolean {
    return this.connected;
  }
}
