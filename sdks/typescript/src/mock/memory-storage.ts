// In-memory CSP thin-node StorageAdapter — for tests and ephemeral
// scenarios. Persistence vanishes when the instance is dropped. Real
// persistence is the host's adapter (the plugin's ObsidianStorageAdapter
// over `.context/`, CSP spec.md §9.1).

import type { StorageAdapter } from '../types.js';

export class MemoryStorage implements StorageAdapter {
  private objects = new Map<string, Uint8Array>();
  private state: Uint8Array | null = null;
  private frontier: Uint8Array | null = null;
  private identitySeed: Uint8Array | null = null;
  private snapshots: Uint8Array | null = null;

  async getObject(oid: string): Promise<Uint8Array | null> {
    const v = this.objects.get(oid);
    return v ? new Uint8Array(v) : null;
  }
  async putObject(oid: string, bytes: Uint8Array): Promise<void> {
    this.objects.set(oid, new Uint8Array(bytes));
  }
  async hasObject(oid: string): Promise<boolean> {
    return this.objects.has(oid);
  }
  async listObjectOids(): Promise<string[]> {
    return [...this.objects.keys()];
  }
  async loadState(): Promise<Uint8Array | null> {
    return this.state ? new Uint8Array(this.state) : null;
  }
  async saveState(bytes: Uint8Array): Promise<void> {
    this.state = new Uint8Array(bytes);
  }
  async loadFrontier(): Promise<Uint8Array | null> {
    return this.frontier ? new Uint8Array(this.frontier) : null;
  }
  async saveFrontier(bytes: Uint8Array): Promise<void> {
    this.frontier = new Uint8Array(bytes);
  }
  async loadIdentitySeed(): Promise<Uint8Array | null> {
    return this.identitySeed ? new Uint8Array(this.identitySeed) : null;
  }
  async saveIdentitySeed(seed: Uint8Array): Promise<void> {
    this.identitySeed = new Uint8Array(seed);
  }
  async loadSnapshots(): Promise<Uint8Array | null> {
    return this.snapshots ? new Uint8Array(this.snapshots) : null;
  }
  async saveSnapshots(bytes: Uint8Array): Promise<void> {
    this.snapshots = new Uint8Array(bytes);
  }
  async close(): Promise<void> {}
}

/** `memoryStorage()` reads better than `new MemoryStorage()`. */
export function memoryStorage(): MemoryStorage {
  return new MemoryStorage();
}
