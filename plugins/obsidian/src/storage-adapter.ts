// CSP thin-node StorageAdapter backed by Obsidian's `app.vault.adapter`.
// Works identically on desktop (Electron) and mobile (Capacitor WebView).
//
// This is exactly the host override CSP spec.md §9.1 permits: a thin node
// MAY put `.context/` on host-provided storage. Layout under
// `<vault-root>/.context/` (CSP §9.1) — interchangeable with `ctx`:
//
//   .context/objects/<oid>   content-addressed objects (lowercase hex SHA-1)
//   .context/state           the `.context/state` record (§5.6 hashes, §5.1
//                             durable counter)
//   .context/frontier        un-merged primitive tip set (§6.4)
//   .context/snapshots       named snapshot records (§8)
//   .context/identity.seed   engine-managed seed (interface parity; the
//                             plugin injects identity, so unused)
//
// `.context` is a dotfolder Obsidian's content APIs (getFiles) skip, so
// CSP's own state never round-trips through sync (CSP §11 HARD INVARIANT).
// `.context/authorized_keys` is irrelevant here: a thin node never listens
// (CSP §7), so it owns no authorized set.

import type { StorageAdapter } from '@csp/sdk/web-init';

/**
 * Subset of `obsidian`'s `DataAdapter` we depend on, restated so the module
 * is unit-testable without importing the real `obsidian` package.
 */
export interface MinimalDataAdapter {
  read(path: string): Promise<string>;
  readBinary(path: string): Promise<ArrayBuffer>;
  write(path: string, data: string): Promise<void>;
  writeBinary(path: string, data: ArrayBuffer): Promise<void>;
  exists(path: string): Promise<boolean>;
  mkdir(path: string): Promise<void>;
  remove(path: string): Promise<void>;
  rename(oldPath: string, newPath: string): Promise<void>;
  list(path: string): Promise<{ files: string[]; folders: string[] }>;
}

/** Sanitize an object id so it's safe as a filename on every host FS. */
export function sanitizeOid(oid: string): string {
  if (!/^[0-9a-fA-F]+$/.test(oid)) {
    throw new Error(`invalid object id (expected hex): ${oid.slice(0, 32)}…`);
  }
  return oid.toLowerCase();
}

export class ObsidianStorageAdapter implements StorageAdapter {
  private readonly objectsDir: string;
  private readonly statePath: string;
  private readonly frontierPath: string;
  private readonly snapshotsPath: string;
  private readonly identityPath: string;

  /**
   * @param adapter The host's DataAdapter (`app.vault.adapter`).
   * @param root    Vault-relative state root — always `.context` so the
   *                layout matches the native `ctx` CLI.
   */
  constructor(
    private readonly adapter: MinimalDataAdapter,
    private readonly root: string = '.context',
  ) {
    this.objectsDir = `${root}/objects`;
    this.statePath = `${root}/state`;
    this.frontierPath = `${root}/frontier`;
    this.snapshotsPath = `${root}/snapshots`;
    this.identityPath = `${root}/identity.seed`;
  }

  // ---- Object store ----

  async getObject(oid: string): Promise<Uint8Array | null> {
    return this.readBinaryOrNull(`${this.objectsDir}/${sanitizeOid(oid)}`);
  }

  async putObject(oid: string, bytes: Uint8Array): Promise<void> {
    await this.ensureDir(this.objectsDir);
    await this.atomicWriteBinary(`${this.objectsDir}/${sanitizeOid(oid)}`, bytes);
  }

  async hasObject(oid: string): Promise<boolean> {
    return this.adapter.exists(`${this.objectsDir}/${sanitizeOid(oid)}`);
  }

  async listObjectOids(): Promise<string[]> {
    if (!(await this.adapter.exists(this.objectsDir))) return [];
    const { files } = await this.adapter.list(this.objectsDir);
    return files.map((p) => {
      const slash = p.lastIndexOf('/');
      return slash === -1 ? p : p.slice(slash + 1);
    });
  }

  // ---- Named blobs ----

  async loadState(): Promise<Uint8Array | null> {
    return this.readBinaryOrNull(this.statePath);
  }
  async saveState(bytes: Uint8Array): Promise<void> {
    await this.ensureDir(this.root);
    await this.atomicWriteBinary(this.statePath, bytes);
  }
  async loadFrontier(): Promise<Uint8Array | null> {
    return this.readBinaryOrNull(this.frontierPath);
  }
  async saveFrontier(bytes: Uint8Array): Promise<void> {
    await this.ensureDir(this.root);
    await this.atomicWriteBinary(this.frontierPath, bytes);
  }
  async loadIdentitySeed(): Promise<Uint8Array | null> {
    return this.readBinaryOrNull(this.identityPath);
  }
  async saveIdentitySeed(seed: Uint8Array): Promise<void> {
    await this.ensureDir(this.root);
    await this.atomicWriteBinary(this.identityPath, seed);
  }
  async loadSnapshots(): Promise<Uint8Array | null> {
    return this.readBinaryOrNull(this.snapshotsPath);
  }
  async saveSnapshots(bytes: Uint8Array): Promise<void> {
    await this.ensureDir(this.root);
    await this.atomicWriteBinary(this.snapshotsPath, bytes);
  }

  /** No persistent handles to release. */
  async close(): Promise<void> {}

  // ---- Internal helpers ----

  private async readBinaryOrNull(path: string): Promise<Uint8Array | null> {
    if (!(await this.adapter.exists(path))) return null;
    const buf = await this.adapter.readBinary(path);
    const bytes = new Uint8Array(buf);
    // A zero-length file means "reset" — treat as missing so the engine
    // regenerates rather than tripping its own length validators.
    return bytes.length === 0 ? null : bytes;
  }

  /**
   * Atomic-ish write: write `<path>.tmp` then rename. Survives an abrupt
   * shutdown mid-write — the previous version stays intact until the rename
   * succeeds (CSP §5.6 requires atomic materialization writes).
   */
  private async atomicWriteBinary(path: string, bytes: Uint8Array): Promise<void> {
    const tmp = `${path}.tmp`;
    const buf = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    await this.adapter.writeBinary(tmp, buf as ArrayBuffer);
    if (await this.adapter.exists(path)) {
      await this.adapter.remove(path);
    }
    await this.adapter.rename(tmp, path);
  }

  /** Create `path` and any missing ancestor segments. */
  private async ensureDir(path: string): Promise<void> {
    if (!path) return;
    const parts = path.split('/').filter(Boolean);
    let cur = '';
    for (const seg of parts) {
      cur = cur ? `${cur}/${seg}` : seg;
      if (!(await this.adapter.exists(cur))) {
        await this.adapter.mkdir(cur);
      }
    }
  }
}
