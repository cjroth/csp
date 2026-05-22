// Worker-side host for the engine Web Worker (issue 0010).
//
// `EngineWorkerHost` runs inside the Worker. It owns a real `RealVault`
// (wasm engine + WebSocket transport) and translates the message protocol
// into `Vault` method calls. The host's renderer thread is never touched —
// the heavy synchronous engine work (`commit_staged`, `to_bytes`, the fold)
// happens here, off the UI thread.
//
// Storage can't run here — the `.context/` adapter needs main-thread host
// APIs — so the host hands its `RealVault` a `ProxyStorage` that round-trips
// every `StorageAdapter` call back across the channel.

import type { StorageAdapter, TransportAdapter, VaultEvent } from '../types.js';
import type { Vault as VaultContract } from '../vault.js';
import { initCsp, isInitialized } from '../web-init.js';
import { Identity, Vault } from '../web-init.js';
import type { Port } from './channel.js';
import type {
  Command,
  FromWorker,
  Observable,
  Reply,
  StorageResponse,
  ToWorker,
} from './protocol.js';

/** Test seam — production passes nothing; the worker's `defaultTransport()`
 * is used. Tests inject a mock transport (no real WebSocket). */
export interface EngineWorkerHostOptions {
  transport?: TransportAdapter;
}

/** A `StorageAdapter` that forwards every call across the channel to the
 * real adapter on the main thread. */
class ProxyStorage implements StorageAdapter {
  private nextId = 1;
  private readonly pending = new Map<
    number,
    { resolve: (v: unknown) => void; reject: (e: Error) => void }
  >();

  constructor(private readonly post: (m: FromWorker) => void) {}

  /** Resolve a pending call from a `storage-res` message. */
  settle(res: StorageResponse): void {
    const p = this.pending.get(res.id);
    if (!p) return;
    this.pending.delete(res.id);
    if (res.ok) p.resolve(res.value);
    else p.reject(new Error(res.error ?? 'storage proxy error'));
  }

  /** Reject every in-flight call — used when the vault closes. */
  abort(reason: string): void {
    for (const p of this.pending.values()) p.reject(new Error(reason));
    this.pending.clear();
  }

  private call(method: string, args: unknown[]): Promise<unknown> {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.post({ kind: 'storage-req', id, method, args });
    });
  }

  getObject(oid: string): Promise<Uint8Array | null> {
    return this.call('getObject', [oid]) as Promise<Uint8Array | null>;
  }
  putObject(oid: string, bytes: Uint8Array): Promise<void> {
    return this.call('putObject', [oid, bytes]) as Promise<void>;
  }
  hasObject(oid: string): Promise<boolean> {
    return this.call('hasObject', [oid]) as Promise<boolean>;
  }
  listObjectOids(): Promise<string[]> {
    return this.call('listObjectOids', []) as Promise<string[]>;
  }
  loadState(): Promise<Uint8Array | null> {
    return this.call('loadState', []) as Promise<Uint8Array | null>;
  }
  saveState(bytes: Uint8Array): Promise<void> {
    return this.call('saveState', [bytes]) as Promise<void>;
  }
  loadFrontier(): Promise<Uint8Array | null> {
    return this.call('loadFrontier', []) as Promise<Uint8Array | null>;
  }
  saveFrontier(bytes: Uint8Array): Promise<void> {
    return this.call('saveFrontier', [bytes]) as Promise<void>;
  }
  loadIdentitySeed(): Promise<Uint8Array | null> {
    return this.call('loadIdentitySeed', []) as Promise<Uint8Array | null>;
  }
  saveIdentitySeed(seed: Uint8Array): Promise<void> {
    return this.call('saveIdentitySeed', [seed]) as Promise<void>;
  }
  loadSnapshots(): Promise<Uint8Array | null> {
    return this.call('loadSnapshots', []) as Promise<Uint8Array | null>;
  }
  saveSnapshots(bytes: Uint8Array): Promise<void> {
    return this.call('saveSnapshots', [bytes]) as Promise<void>;
  }
  close(): Promise<void> {
    return this.call('close', []) as Promise<void>;
  }
}

export class EngineWorkerHost {
  private vault: VaultContract | null = null;
  private storage: ProxyStorage | null = null;
  private unsubscribe: (() => void) | null = null;

  constructor(
    private readonly port: Port<FromWorker, ToWorker>,
    private readonly opts: EngineWorkerHostOptions = {},
  ) {
    this.port.onMessage((msg) => void this.onMessage(msg));
  }

  private async onMessage(msg: ToWorker): Promise<void> {
    if (msg.kind === 'storage-res') {
      this.storage?.settle(msg);
      return;
    }
    await this.handleCommand(msg);
  }

  private async handleCommand(cmd: Command): Promise<void> {
    try {
      const value = await this.dispatch(cmd);
      const reply: Reply = { kind: 'reply', id: cmd.id, ok: true };
      if (value !== undefined) reply.value = value;
      this.port.post(reply);
      // A command may have changed observable state — refresh the shadow.
      if (cmd.op !== 'readTextFile') this.postObservable();
    } catch (err) {
      this.port.post({
        kind: 'reply',
        id: cmd.id,
        ok: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  }

  private async dispatch(cmd: Command): Promise<string | undefined> {
    if (cmd.op === 'init') {
      await this.doInit(cmd);
      return undefined;
    }
    const v = this.vault;
    if (!v) throw new Error('engine worker: not initialized');
    switch (cmd.op) {
      case 'writeTextFile':
        await v.writeTextFile(cmd.path, cmd.content);
        return undefined;
      case 'readTextFile':
        return v.readTextFile(cmd.path);
      case 'deleteFile':
        await v.deleteFile(cmd.path);
        return undefined;
      case 'renameFile':
        await v.renameFile(cmd.from, cmd.to);
        return undefined;
      case 'connectWithReconnect':
        // Fire-and-forget: the reconnect supervisor runs for the session's
        // lifetime; awaiting it would never resolve. Connection progress
        // surfaces through VaultEvents.
        void v.connectWithReconnect();
        return undefined;
      case 'disconnect':
        await v.disconnect();
        return undefined;
      case 'close':
        await this.doClose();
        return undefined;
      case 'createSnapshot':
        await v.createSnapshot(cmd.name);
        return undefined;
      case 'deleteSnapshot':
        await v.deleteSnapshot(cmd.name);
        return undefined;
      case 'restoreToSnapshot':
        await v.restoreToSnapshot(cmd.name);
        return undefined;
      case 'restoreToTime':
        await v.restoreToTime(cmd.targetMs);
        return undefined;
    }
  }

  private async doInit(cmd: Extract<Command, { op: 'init' }>): Promise<void> {
    const p = cmd.payload;
    if (!isInitialized() && p.wasmBytes.length > 0) {
      await initCsp(p.wasmBytes);
    }
    this.storage = new ProxyStorage((m) => this.port.post(m));
    const identity = Identity.fromSeed(p.seed);
    const base = {
      storage: this.storage,
      identity,
      ...(p.peerUrl ? { peerUrl: p.peerUrl } : {}),
      ...(p.peerPubkey ? { peerPubkey: p.peerPubkey } : {}),
      ...(p.authKey ? { authKey: p.authKey } : {}),
      ...(this.opts.transport ? { transport: this.opts.transport } : {}),
    };
    if (p.mode === 'open') {
      this.vault = await Vault.open(base);
    } else if (p.mode === 'clone') {
      if (!p.peerUrl) throw new Error('clone requires a peerUrl');
      this.vault = await Vault.clone({ ...base, peerUrl: p.peerUrl });
    } else {
      this.vault = await Vault.create(base);
    }
    this.unsubscribe = this.vault.subscribe((e) => this.onVaultEvent(e));
  }

  private onVaultEvent(event: VaultEvent): void {
    // Observable + event ride together so the main-thread shadow is current
    // before a `tree-changed` reaches the bridge.
    this.port.post({ kind: 'event', event, observable: this.observable() });
  }

  private postObservable(): void {
    if (this.vault) this.port.post({ kind: 'event', observable: this.observable() });
  }

  private observable(): Observable {
    const v = this.vault;
    if (!v) {
      return { files: [], snapshots: [], connected: false, identitySsh: '' };
    }
    return {
      files: v.listFiles().map((f) => ({ path: f.path, size: f.size })),
      snapshots: v.listSnapshots(),
      connected: v.isConnected(),
      identitySsh: v.identityPubkeySsh(),
    };
  }

  private async doClose(): Promise<void> {
    this.unsubscribe?.();
    this.unsubscribe = null;
    const v = this.vault;
    this.vault = null;
    if (v) await v.close();
    this.storage?.abort('engine worker closed');
    this.storage = null;
  }
}
