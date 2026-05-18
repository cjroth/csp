// In-memory mock of a CSP thin-node vault session (the `Vault` contract in
// `../vault.ts`). Offline-first: file ops work with no connection. When a
// `peerUrl` is set and connected, it joins the shared `Room` (CSP spec.md
// §6.1 stand-in) and converges working-tree state with other mock vaults on
// the same URL — exercising the host plugin's two-way sync, reconcile, and
// event handling without the real `csp-wasm` runtime.
//
// NOT the CSP fold (CSP §5): convergence is whole-file LWW by a monotonic
// rev (`broker.nextRev()`). The plugin asserts no fold SHAs (CSP §13), so
// this is sufficient and intentional. Real merge lands with csp-wasm,
// behind this exact contract — no plugin changes.

import type { FileMeta, ReconnectOptions, Snapshot, VaultEvent } from '../types.js';
import type { CloneOptions, CreateOptions, Identity, OpenOptions, Vault } from '../vault.js';
import { type Room, type RoomMember, nextRev, roomFor } from './broker.js';
import { MockIdentity } from './identity.js';

interface LocalFile {
  id: string;
  content: string;
  deleted: boolean;
  rev: number;
  created_at: number;
  updated_at: number;
}

interface PersistShape {
  files: Array<[string, LocalFile]>;
  snapshots: Array<{ name: string; created_at_ms: number; files: Array<[string, LocalFile]> }>;
}

let fileSeq = 0;
function newFileId(): string {
  return `f${(++fileSeq).toString(16)}-${Date.now().toString(16)}`;
}

export class MockVault implements Vault, RoomMember {
  private files = new Map<string, LocalFile>();
  private snaps = new Map<string, { created_at_ms: number; files: Map<string, LocalFile> }>();
  private listeners = new Set<(e: VaultEvent) => void>();
  private room: Room | null = null;
  private connected = false;
  private closed = false;
  private reconnectAbort: AbortController | null = null;

  private constructor(
    private readonly identity: Identity,
    private readonly ownsIdentity: boolean,
    private readonly storage: CreateOptions['storage'],
    private readonly peerUrl: string | undefined,
  ) {}

  // ---- Factories ----

  static async create(opts: CreateOptions): Promise<MockVault> {
    const { identity, owns } = await resolveIdentity(opts);
    const v = new MockVault(identity, owns, opts.storage, opts.peerUrl);
    await v.persist();
    return v;
  }

  static async open(opts: OpenOptions): Promise<MockVault> {
    const raw = await opts.storage.loadState();
    if (!raw) throw new Error('no vault on disk; call create()/clone() first');
    const { identity, owns } = await resolveIdentity(opts);
    const v = new MockVault(identity, owns, opts.storage, opts.peerUrl);
    v.hydrate(raw);
    return v;
  }

  static async clone(opts: CloneOptions): Promise<MockVault> {
    // A thin-node clone is create-then-catch-up (CSP §17 `ctx clone`);
    // the caller connects, which joins the peer's room and converges.
    const { identity, owns } = await resolveIdentity(opts);
    const v = new MockVault(identity, owns, opts.storage, opts.peerUrl);
    await v.persist();
    return v;
  }

  // ---- File operations ----

  async writeTextFile(path: string, content: string): Promise<string> {
    const now = Date.now();
    const prev = this.files.get(path);
    const f: LocalFile = {
      id: prev?.id ?? newFileId(),
      content,
      deleted: false,
      rev: nextRev(),
      created_at: prev?.created_at ?? now,
      updated_at: now,
    };
    this.files.set(path, f);
    await this.persist();
    this.publish(path, f);
    return f.id;
  }

  async readTextFile(path: string): Promise<string> {
    const f = this.files.get(path);
    if (!f || f.deleted) throw new Error(`ENOENT: ${path}`);
    return f.content;
  }

  fileExists(path: string): boolean {
    const f = this.files.get(path);
    return !!f && !f.deleted;
  }

  async deleteFile(path: string): Promise<void> {
    const prev = this.files.get(path);
    if (!prev || prev.deleted) return;
    const f: LocalFile = { ...prev, deleted: true, rev: nextRev(), updated_at: Date.now() };
    this.files.set(path, f);
    await this.persist();
    this.publish(path, f);
  }

  async renameFile(from: string, to: string): Promise<void> {
    const src = this.files.get(from);
    if (!src || src.deleted) return;
    await this.writeTextFile(to, src.content);
    await this.deleteFile(from);
  }

  listFiles(): FileMeta[] {
    const out: FileMeta[] = [];
    for (const [path, f] of this.files) {
      out.push({
        id: f.id,
        path,
        kind: 'Text',
        size: f.content.length,
        created_at: f.created_at,
        updated_at: f.updated_at,
        deleted_at: f.deleted ? f.updated_at : null,
      });
    }
    return out;
  }

  // ---- Snapshots / recovery (CSP §8) ----

  async createSnapshot(name: string): Promise<void> {
    const files = new Map<string, LocalFile>();
    for (const [p, f] of this.files) files.set(p, { ...f });
    this.snaps.set(name, { created_at_ms: Date.now(), files });
    await this.persist();
  }

  async deleteSnapshot(name: string): Promise<void> {
    this.snaps.delete(name);
    await this.persist();
  }

  listSnapshots(): Snapshot[] {
    return [...this.snaps.entries()].map(([name, s]) => ({
      name,
      created_at_ms: s.created_at_ms,
      frontier: [...s.files.keys()],
    }));
  }

  async restoreToSnapshot(name: string): Promise<void> {
    const s = this.snaps.get(name);
    if (!s) throw new Error(`no such snapshot: ${name}`);
    // Restore-as-edit (CSP §8): re-author the snapshot's tree onto the
    // current lineage rather than rewinding.
    const paths = new Set([...this.files.keys(), ...s.files.keys()]);
    for (const p of paths) {
      const snap = s.files.get(p);
      if (snap && !snap.deleted) await this.writeTextFile(p, snap.content);
      else if (this.fileExists(p)) await this.deleteFile(p);
    }
  }

  async restoreToTime(targetMs: number): Promise<void> {
    // Best-effort, horizon-bounded (CSP §8/§9.2): restore the newest
    // snapshot at or before `targetMs`.
    let best: { name: string; at: number } | null = null;
    for (const [name, s] of this.snaps) {
      if (s.created_at_ms <= targetMs && (!best || s.created_at_ms > best.at)) {
        best = { name, at: s.created_at_ms };
      }
    }
    if (!best) throw new Error('no snapshot at or before the requested time');
    await this.restoreToSnapshot(best.name);
  }

  // ---- Connection ----

  async connectWithReconnect(_opts: ReconnectOptions = {}): Promise<void> {
    if (!this.peerUrl) return; // offline-first local-only thin node (CSP §7)
    this.reconnectAbort = new AbortController();
    const signal = this.reconnectAbort.signal;
    this.emit({ kind: 'connecting', url: this.peerUrl });
    this.room = roomFor(this.peerUrl);
    this.room.join(this);
    // Catch-up: push local newer files up, pull the room down (CSP §6.4).
    for (const [path, f] of this.files) this.room.publish(path, f);
    this.connected = true;
    const pk = this.identity.pubkey();
    try {
      this.emit({ kind: 'connected', peer_pubkey: pk.bytes() });
    } finally {
      pk.free();
    }
    this.pullFromRoom();
    this.room.broadcast(this);
    // Hold "connected" until disconnect() aborts (mock connect never fails).
    await new Promise<void>((res) => {
      if (signal.aborted) return res();
      signal.addEventListener('abort', () => res(), { once: true });
    });
  }

  async disconnect(): Promise<void> {
    this.reconnectAbort?.abort();
    this.reconnectAbort = null;
    if (this.room) {
      this.room.leave(this);
      this.room = null;
    }
    if (this.connected) {
      this.connected = false;
      this.emit({ kind: 'disconnected', reason: 'disconnected' });
    }
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    await this.disconnect();
    await this.persist();
    if (this.ownsIdentity) {
      try {
        this.identity.free();
      } catch {}
    }
    await this.storage.close();
  }

  // ---- Events / accessors ----

  subscribe(listener: (e: VaultEvent) => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  identityPubkeySsh(): string {
    const pk = this.identity.pubkey();
    try {
      return pk.toSshString();
    } finally {
      pk.free();
    }
  }

  isConnected(): boolean {
    return this.connected;
  }

  // ---- RoomMember ----

  /** Adopt any room file newer than the local copy; emit tree-changed if
   * anything changed (CSP §6.5 — the merged tree arrived from the peer). */
  pullFromRoom(): void {
    if (!this.room) return;
    let changed = false;
    for (const [path, rf] of this.room.files) {
      const cur = this.files.get(path);
      if (cur && cur.rev >= rf.rev) continue;
      const now = Date.now();
      this.files.set(path, {
        id: cur?.id ?? newFileId(),
        content: rf.content,
        deleted: rf.deleted,
        rev: rf.rev,
        created_at: cur?.created_at ?? now,
        updated_at: now,
      });
      changed = true;
    }
    if (changed) {
      void this.persist();
      this.emit({ kind: 'tree-changed' });
    }
  }

  // ---- Internal ----

  private publish(path: string, f: LocalFile): void {
    if (!this.connected || !this.room) return;
    if (this.room.publish(path, { content: f.content, deleted: f.deleted, rev: f.rev })) {
      this.emit({ kind: 'catchup-progress', outbound: true });
      this.room.broadcast(this);
    }
  }

  private emit(e: VaultEvent): void {
    for (const l of this.listeners) {
      try {
        l(e);
      } catch {
        // listener throws don't propagate
      }
    }
  }

  private async persist(): Promise<void> {
    if (this.closed) return;
    const shape: PersistShape = {
      files: [...this.files.entries()],
      snapshots: [...this.snaps.entries()].map(([name, s]) => ({
        name,
        created_at_ms: s.created_at_ms,
        files: [...s.files.entries()],
      })),
    };
    await this.storage.saveState(new TextEncoder().encode(JSON.stringify(shape)));
  }

  private hydrate(raw: Uint8Array): void {
    const shape = JSON.parse(new TextDecoder().decode(raw)) as PersistShape;
    this.files = new Map(shape.files);
    this.snaps = new Map(
      shape.snapshots.map((s) => [
        s.name,
        { created_at_ms: s.created_at_ms, files: new Map(s.files) },
      ]),
    );
  }
}

async function resolveIdentity(
  opts: CreateOptions,
): Promise<{ identity: Identity; owns: boolean }> {
  if (opts.identity) return { identity: opts.identity, owns: false };
  const seed = await opts.storage.loadIdentitySeed();
  if (seed) return { identity: MockIdentity.fromSeed(seed), owns: true };
  const id = MockIdentity.generate();
  await opts.storage.saveIdentitySeed(id.seed());
  return { identity: id, owns: true };
}
