// Two-way sync between Obsidian's `app.vault` and the CSP thin-node engine.
//
//   Obsidian → engine : listens for create/modify/delete/rename and pushes
//                        via vault.writeTextFile/deleteFile/renameFile.
//   engine → Obsidian : on `tree-changed` (CSP spec.md §6.5 — a merged tree
//                        arrived from the connected full node, or a local
//                        primitive was folded) walks listFiles() and
//                        applies diffs.
//
// This module is HOST I/O, not the no-feedback-loop algorithm. CSP §5.6
// specifies materialize-vs-user-edit reconcile (content-hash compare, atomic
// writes, contended-path defer); the host's job is to provide atomic writes
// + faithful events and let the engine own the reconcile. The suppression
// set + content-equality short-circuit below are defense-in-depth, retained
// from the agentsync plugin; correctness rests on the engine's §5.6.

import type { FileMeta, VaultInstance } from '@csp/sdk/web-init';

// We avoid importing types from `obsidian` directly here because the npm
// package ships only type declarations (no runtime). We declare the
// structural slice we actually need.

/** Structural shape of an Obsidian abstract file (file or folder). */
export interface MinimalAbstractFile {
  path: string;
  name: string;
}

/** Structural shape of an Obsidian text file. */
export interface MinimalFile extends MinimalAbstractFile {
  extension: string;
}

/** Structural shape of `App.vault` — exactly the methods the bridge calls. */
export interface MinimalVault {
  getFiles(): MinimalFile[];
  getAbstractFileByPath(path: string): MinimalAbstractFile | null;
  read(file: MinimalFile): Promise<string>;
  create(path: string, data: string): Promise<MinimalFile>;
  modify(file: MinimalFile, data: string): Promise<void>;
  delete(file: MinimalAbstractFile, force?: boolean): Promise<void>;
  rename(file: MinimalAbstractFile, newPath: string): Promise<void>;
  createFolder(path: string): Promise<unknown>;
}

export interface BridgeDeps {
  vault: MinimalVault;
  sdk: VaultInstance;
  filter: (path: string) => boolean;
  log?: (msg: string) => void;
  /** Test seam — whether `f` is a file (not a folder). */
  isFile?: (f: MinimalAbstractFile | null) => f is MinimalFile;
}

const defaultIsFile = (f: MinimalAbstractFile | null): f is MinimalFile =>
  !!f && (f as MinimalFile).extension !== undefined;

/** Yield control back to the event loop so the renderer can paint. */
function yieldToEventLoop(): Promise<void> {
  return new Promise((r) => setTimeout(r, 0));
}

/** Vault-relative parent directory of `path`, or '' at the root. */
function parentDir(path: string): string {
  const i = path.lastIndexOf('/');
  return i <= 0 ? '' : path.slice(0, i);
}

export class ObsidianVaultBridge {
  /** Paths the bridge wrote — modify/create handlers eat one token each. */
  private suppressed = new Map<string, number>();
  /**
   * Snapshot of alive engine paths from the last `applyRemoteState` call.
   * `listFiles()` includes tombstones (via `deleted_at`); a path vanishing
   * from this set on the next call is the delete signal.
   */
  private knownSdkPaths = new Set<string>();
  /** Counters since `start()` — exposed for tests + status. */
  pushed = 0;
  pulled = 0;
  skipped = 0;

  private isFile: (f: MinimalAbstractFile | null) => f is MinimalFile;
  private log: (msg: string) => void;
  /** Coalesce bursts of `tree-changed` — initial catch-up against a large
   * remote can fire many in a row; one pass per burst is plenty. */
  private applyTimer: ReturnType<typeof setTimeout> | null = null;
  private applyPending = false;
  private applyInFlight = false;
  /**
   * Per-bridge serial queue. Outbound Obsidian-event handlers AND inbound
   * applyRemoteState all run through this so they never interleave. Without
   * serialization a remote-apply pass can snapshot SDK state before an
   * in-flight local rename has propagated, re-create the moved-from file
   * in Obsidian, and produce the user-visible duplication bug.
   */
  private opQueue: Promise<void> = Promise.resolve();
  /** Set in `dispose()` — gates the debounce so a late timer firing after
   * the controller stopped doesn't poke a freed engine session. */
  private disposed = false;
  private static readonly YIELD_EVERY = 25;
  private static readonly APPLY_DEBOUNCE_MS = 200;

  constructor(private readonly deps: BridgeDeps) {
    this.isFile = deps.isFile ?? defaultIsFile;
    this.log = deps.log ?? (() => {});
  }

  /** Suppress the next event for `path`. */
  suppress(path: string): void {
    this.suppressed.set(path, (this.suppressed.get(path) ?? 0) + 1);
  }

  /** Returns true if `path` was suppressed (and consumes one token). */
  consumeSuppression(path: string): boolean {
    const n = this.suppressed.get(path);
    if (!n) return false;
    if (n === 1) this.suppressed.delete(path);
    else this.suppressed.set(path, n - 1);
    return true;
  }

  /**
   * Chain `fn` onto the bridge's serial queue. Used to ensure outbound
   * handlers and inbound apply passes never interleave on shared state
   * (`knownSdkPaths`, the SDK working map, Obsidian's vault). Errors are
   * propagated to the caller but don't poison the queue for later ops.
   */
  private enqueue<T>(fn: () => Promise<T>): Promise<T> {
    const result = this.opQueue.then(fn, fn);
    this.opQueue = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  }

  // ---- Obsidian → engine ----

  /** Handle a create or modify event from Obsidian. */
  handleObsidianWrite(file: MinimalAbstractFile): Promise<void> {
    return this.enqueue(() => this.runHandleObsidianWrite(file));
  }

  private async runHandleObsidianWrite(file: MinimalAbstractFile): Promise<void> {
    // Obsidian fires `create` for a TFolder. The engine is file-only;
    // preserve a user-created empty folder with a `.keep` sentinel (§11).
    // The engine's canonicalization drops it once the folder gains a real
    // file, so asserting it whenever the folder has no engine content is
    // safe and idempotent.
    if (!file) return;
    if (!this.isFile(file)) {
      const keep = `${file.path}/.keep`;
      if (this.deps.filter(keep) && this.engineChildren(file.path).length === 0) {
        await this.deps.sdk.writeTextFile(keep, '');
        this.pushed += 1;
        this.log(`push (empty-dir keep): ${keep}`);
      }
      return;
    }
    if (this.consumeSuppression(file.path)) return;
    if (!this.deps.filter(file.path)) {
      this.skipped += 1;
      this.log(`skip (filter): ${file.path}`);
      return;
    }
    const content = await this.deps.vault.read(file);
    // Equality short-circuit — avoids a redundant primitive when the
    // suppression set missed (e.g. a re-save with identical bytes). The
    // engine's §5.6 content-hash reconcile is the real guard.
    if (this.deps.sdk.fileExists(file.path)) {
      const remote = await this.deps.sdk.readTextFile(file.path);
      if (remote === content) return;
    }
    await this.deps.sdk.writeTextFile(file.path, content);
    this.pushed += 1;
    this.log(`push: ${file.path} (${content.length}B)`);
  }

  /** Engine paths at-or-under a folder (the engine is file-only, so a
   * folder is just a path prefix; Obsidian fires ONE folder-level event). */
  private engineChildren(folderPath: string): string[] {
    const prefix = `${folderPath}/`;
    return this.deps.sdk
      .listFiles()
      .map((m) => m.path)
      .filter((p) => p === folderPath || p.startsWith(prefix));
  }

  /** Handle a delete event from Obsidian. */
  handleObsidianDelete(file: MinimalAbstractFile): Promise<void> {
    return this.enqueue(() => this.runHandleObsidianDelete(file));
  }

  private async runHandleObsidianDelete(file: MinimalAbstractFile): Promise<void> {
    if (this.consumeSuppression(file.path)) return;
    // Real Obsidian fires per-child `delete` events for each descendant file
    // FIRST, then a folder-level `delete`. The bridge handles both correctly:
    // file events go down the file branch; the trailing folder event finds
    // no engine children and is a no-op. We still keep the folder branch so a
    // legacy `folder-only` event sequence (or another host) is also handled.
    // Obsidian fires ONE folder-level `delete` for a folder (no per-child
    // events). Expand to every known engine child.
    if (file && !this.isFile(file)) {
      for (const p of this.engineChildren(file.path)) {
        if (!this.deps.sdk.fileExists(p)) continue;
        await this.deps.sdk.deleteFile(p);
        this.pushed += 1;
        this.log(`delete (folder child): ${p}`);
      }
      return;
    }
    if (!this.deps.sdk.fileExists(file.path)) return;
    await this.deps.sdk.deleteFile(file.path);
    this.pushed += 1;
    this.log(`delete: ${file.path}`);
    // If deleting the last file emptied its folder (still present in
    // Obsidian), preserve the now-empty folder with a `.keep` (§11).
    const parent = parentDir(file.path);
    if (
      parent &&
      this.engineChildren(parent).length === 0 &&
      this.deps.vault.getAbstractFileByPath(parent)
    ) {
      const keep = `${parent}/.keep`;
      if (this.deps.filter(keep)) {
        await this.deps.sdk.writeTextFile(keep, '');
        this.pushed += 1;
        this.log(`push (empty-dir keep): ${keep}`);
      }
    }
  }

  /** Handle a rename event from Obsidian. */
  handleObsidianRename(file: MinimalAbstractFile, oldPath: string): Promise<void> {
    return this.enqueue(() => this.runHandleObsidianRename(file, oldPath));
  }

  private async runHandleObsidianRename(file: MinimalAbstractFile, oldPath: string): Promise<void> {
    if (this.consumeSuppression(file.path)) return;
    // Obsidian fires ONE folder-level `rename` for a folder move (no
    // per-child events). Re-key every known engine child by prefix.
    if (file && !this.isFile(file)) {
      const newDir = file.path;
      for (const p of this.engineChildren(oldPath)) {
        const to = p === oldPath ? newDir : `${newDir}${p.slice(oldPath.length)}`;
        const fromOk = this.deps.filter(p);
        const toOk = this.deps.filter(to);
        if (fromOk && toOk) {
          await this.deps.sdk.renameFile(p, to);
          this.pushed += 1;
          this.log(`rename (folder child): ${p} → ${to}`);
        } else if (fromOk && !toOk && this.deps.sdk.fileExists(p)) {
          await this.deps.sdk.deleteFile(p);
          this.pushed += 1;
        }
      }
      return;
    }
    const fromAllowed = this.deps.filter(oldPath);
    const toAllowed = this.deps.filter(file.path);

    if (fromAllowed && toAllowed) {
      if (this.deps.sdk.fileExists(oldPath)) {
        await this.deps.sdk.renameFile(oldPath, file.path);
      } else if (this.isFile(file)) {
        const content = await this.deps.vault.read(file);
        await this.deps.sdk.writeTextFile(file.path, content);
      }
      this.pushed += 1;
      return;
    }

    if (fromAllowed && !toAllowed) {
      if (this.deps.sdk.fileExists(oldPath)) {
        await this.deps.sdk.deleteFile(oldPath);
        this.pushed += 1;
      }
      return;
    }

    if (!fromAllowed && toAllowed && this.isFile(file)) {
      const content = await this.deps.vault.read(file);
      await this.deps.sdk.writeTextFile(file.path, content);
      this.pushed += 1;
      return;
    }
    // Neither side allowed — nothing to do.
  }

  // ---- engine → Obsidian ----

  /**
   * Apply a known set of remote changes — the fast path the controller uses
   * whenever the engine emits a `tree-changed` event with its changeset
   * attached. No `sdk.listFiles` scan, no per-file `sdk.readTextFile` (which
   * is a cross-thread round-trip under the engine Worker, issue 0010); the
   * content rides in the event payload itself. `null` content means the
   * path was tombstoned by the merge.
   */
  applyTreeChanges(
    changes: ReadonlyArray<{ path: string; content: string | null }>,
  ): Promise<void> {
    return this.enqueue(() => this.runApplyTreeChanges(changes));
  }

  private async runApplyTreeChanges(
    changes: ReadonlyArray<{ path: string; content: string | null }>,
  ): Promise<void> {
    if (this.disposed) return;
    let i = 0;
    for (const { path, content } of changes) {
      if (!this.deps.filter(path)) continue;
      if (content === null) {
        this.knownSdkPaths.delete(path);
        const existing = this.resolveVaultFile(path);
        if (existing) {
          this.suppress(path);
          try {
            await this.deps.vault.delete(existing);
            this.pulled += 1;
            this.log(`pull-delete (changeset): ${path}`);
            await this.pruneEmptyFolders(path);
          } catch (e) {
            const msg = String((e as Error)?.message ?? e);
            if (!/not found|no such|enoent/i.test(msg)) {
              this.log(`pull-delete failed for ${path}: ${msg}`);
            }
          }
        }
      } else {
        this.knownSdkPaths.add(path);
        const existing = this.deps.vault.getAbstractFileByPath(path);
        if (existing && this.isFile(existing)) {
          const cur = await this.deps.vault.read(existing);
          if (cur === content) continue; // no-op short-circuit
          this.suppress(path);
          await this.deps.vault.modify(existing, content);
          this.pulled += 1;
          this.log(`pull-modify (changeset): ${path}`);
        } else {
          await this.ensureFolderFor(path);
          this.suppress(path);
          try {
            await this.deps.vault.create(path, content);
            this.pulled += 1;
            this.log(`pull-create (changeset): ${path}`);
          } catch (e) {
            // Cold metadata cache: file physically exists but the metadata
            // lookup missed; recover with a modify. Real failures propagate.
            if (!/already exists/i.test(String((e as Error)?.message ?? e))) throw e;
            const f = this.deps.vault.getAbstractFileByPath(path);
            if (f && this.isFile(f)) {
              await this.deps.vault.modify(f, content);
              this.pulled += 1;
            }
          }
        }
      }
      if (++i % ObsidianVaultBridge.YIELD_EVERY === 0) await yieldToEventLoop();
    }
  }

  /**
   * Schedule an `applyRemoteState` pass with leading-edge debounce. The
   * engine can emit many `tree-changed` events in a burst during initial
   * catch-up; this collapses them.
   */
  scheduleApplyRemoteState(): void {
    if (this.disposed) return;
    if (this.applyInFlight) {
      this.applyPending = true;
      return;
    }
    if (this.applyTimer !== null) {
      this.applyPending = true;
      return;
    }
    this.applyTimer = setTimeout(() => {
      this.applyTimer = null;
      if (this.disposed) return;
      void this.runScheduledApply();
    }, ObsidianVaultBridge.APPLY_DEBOUNCE_MS);
  }

  private async runScheduledApply(): Promise<void> {
    if (this.disposed) return;
    this.applyInFlight = true;
    try {
      await this.applyRemoteState();
    } finally {
      this.applyInFlight = false;
    }
    if (this.applyPending && !this.disposed) {
      this.applyPending = false;
      this.scheduleApplyRemoteState();
    }
  }

  /** Tear down the debounce machinery. Called by the controller's stop()
   * before the engine session is freed. */
  dispose(): void {
    this.disposed = true;
    if (this.applyTimer !== null) {
      clearTimeout(this.applyTimer);
      this.applyTimer = null;
    }
    this.applyPending = false;
  }

  /**
   * Seed the remote-removal baseline from the engine's current alive set.
   * Called after the initial reconcile: on a reloaded vault where Obsidian
   * and the engine already match, reconcile applies nothing, so neither
   * `applyOneRemoteFile` nor `applyRemoteState` has run to populate
   * `knownSdkPaths` — and a later CLI delete/rename would have an empty
   * baseline and never remove the old files/folder.
   */
  seedKnownPaths(): void {
    const alive = new Set<string>();
    for (const m of this.deps.sdk.listFiles()) {
      if (m.kind === 'Text' && !m.deleted_at && this.deps.filter(m.path)) {
        alive.add(m.path);
      }
    }
    this.knownSdkPaths = alive;
  }

  /**
   * Apply the engine's current materialized tree to the Obsidian vault.
   * Detects deletions by diffing against the previous live snapshot.
   * Yields to the event loop every YIELD_EVERY files so a large initial
   * catch-up doesn't freeze the renderer.
   */
  applyRemoteState(): Promise<void> {
    return this.enqueue(() => this.runApplyRemoteState());
  }

  private async runApplyRemoteState(): Promise<void> {
    const currentPaths = new Set<string>();
    const sdkFiles = this.deps.sdk.listFiles();
    let i = 0;
    for (const meta of sdkFiles) {
      if (meta.kind === 'Text' && !meta.deleted_at && this.deps.filter(meta.path)) {
        currentPaths.add(meta.path);
      }
      await this.applyOneRemoteFile(meta);
      if (++i % ObsidianVaultBridge.YIELD_EVERY === 0) await yieldToEventLoop();
    }
    // Apply tombstones inferred from the diff.
    let j = 0;
    for (const path of this.knownSdkPaths) {
      if (currentPaths.has(path)) continue;
      if (!this.deps.filter(path)) continue;
      const ex = this.resolveVaultFile(path);
      if (!ex) continue;
      this.suppress(path);
      try {
        await this.deps.vault.delete(ex);
      } catch (e) {
        // Don't drop this path from the baseline if the delete failed —
        // leaving it in `knownSdkPaths` lets a later pass retry, which
        // matters most when a vault.delete races with an Obsidian internal
        // operation. The "file already gone" case is reported as a throw
        // and is safely ignorable.
        const msg = String((e as Error)?.message ?? e);
        if (!/not found|no such|enoent/i.test(msg)) {
          this.log(`pull-delete failed for ${path}: ${msg}`);
          currentPaths.add(path);
        }
        continue;
      }
      this.pulled += 1;
      this.log(`pull-delete (tombstone): ${path}`);
      await this.pruneEmptyFolders(path);
      if (++j % ObsidianVaultBridge.YIELD_EVERY === 0) await yieldToEventLoop();
    }
    this.knownSdkPaths = currentPaths;
  }

  /**
   * Look up a vault file by path with a stale-cache fallback. Obsidian's
   * metadata cache can return `null` for a path that physically still exists
   * (cold reopen, or a race between vault.rename / vault.delete and the
   * cache update). When that happens during a remote tombstone apply, the
   * file lingers at the old path → the user-reported duplication bug.
   * Falling back to `vault.getFiles()` reaches past the metadata cache.
   */
  private resolveVaultFile(path: string): MinimalAbstractFile | null {
    const direct = this.deps.vault.getAbstractFileByPath(path);
    if (direct) return direct;
    // Linear scan as a safety net. `getFiles()` reflects Obsidian's tracked
    // file set; on a cold cache it may already be populated even when the
    // path lookup returns null.
    for (const f of this.deps.vault.getFiles()) {
      if (f.path === path) return f;
    }
    return null;
  }

  /** Apply a single remote file (called from `applyRemoteState` and tests). */
  async applyOneRemoteFile(meta: FileMeta): Promise<void> {
    if (meta.kind !== 'Text') return;
    if (!this.deps.filter(meta.path)) return;

    const existing = this.deps.vault.getAbstractFileByPath(meta.path);

    if (meta.deleted_at) {
      // Drop it from the removal baseline whether or not it was present
      // locally, so a later state diff doesn't try to re-delete it.
      this.knownSdkPaths.delete(meta.path);
      if (existing) {
        this.suppress(meta.path);
        await this.deps.vault.delete(existing);
        this.pulled += 1;
        this.log(`pull-delete: ${meta.path}`);
        await this.pruneEmptyFolders(meta.path);
      }
      return;
    }

    // Seed the removal baseline here too: files pulled by the initial
    // reconcile come through this method (NOT applyRemoteState), so without
    // this a later remote folder rename has an empty `knownSdkPaths` diff
    // and the old folder is "cloned" instead of removed.
    this.knownSdkPaths.add(meta.path);

    const content = await this.deps.sdk.readTextFile(meta.path);

    if (existing && this.isFile(existing)) {
      const cur = await this.deps.vault.read(existing);
      if (cur === content) return;
      this.suppress(meta.path);
      await this.deps.vault.modify(existing, content);
      this.pulled += 1;
      this.log(`pull-modify: ${meta.path}`);
      return;
    }

    // Create — ensure parent folders exist first.
    await this.ensureFolderFor(meta.path);
    this.suppress(meta.path);
    try {
      await this.deps.vault.create(meta.path, content);
    } catch (e) {
      // Cold metadata cache on reopen: the file physically exists (a prior
      // session's sync) but getAbstractFileByPath returned null, so we took
      // the create path and Obsidian throws "File already exists." Recover
      // by writing into the existing file. Real failures still propagate.
      if (!/already exists/i.test(String((e as Error)?.message ?? e))) throw e;
      const f = this.deps.vault.getAbstractFileByPath(meta.path);
      if (f && this.isFile(f)) {
        await this.deps.vault.modify(f, content);
      } else {
        this.log(`pull-create: ${meta.path} exists but unresolved (cold cache); deferring`);
        return;
      }
    }
    this.pulled += 1;
    this.log(`pull-create: ${meta.path}`);
  }

  /**
   * After removing a synced file, reap now-empty ancestor folders. The
   * engine models files only — a folder rename is N file moves — so without
   * this the emptied source folder (and its empty subfolders) lingers in
   * the vault. Climbs parents, stopping at the first folder that still
   * holds a file (or the vault root).
   */
  async pruneEmptyFolders(filePath: string): Promise<void> {
    let dir = parentDir(filePath);
    while (dir) {
      const node = this.deps.vault.getAbstractFileByPath(dir);
      if (!node || this.isFile(node)) break;
      const prefix = `${dir}/`;
      // In use if Obsidian OR the engine still has any path under it. The
      // engine check also covers a `.keep`-only folder whose hidden dotfile
      // Obsidian's `getFiles()` does not surface (§11) — pruning that would
      // wrongly delete a deliberately-preserved empty folder.
      const stillUsed =
        this.deps.vault.getFiles().some((f) => f.path === dir || f.path.startsWith(prefix)) ||
        this.deps.sdk.listFiles().some((m) => m.path === dir || m.path.startsWith(prefix));
      if (stillUsed) break;
      this.suppress(dir);
      try {
        await this.deps.vault.delete(node, true);
      } catch {
        break; // best-effort — a failed prune must not wedge the apply pass
      }
      this.log(`pull-rmdir (empty): ${dir}`);
      dir = parentDir(dir);
    }
  }

  /** Ensure all ancestor folders for `filePath` exist in the vault. */
  async ensureFolderFor(filePath: string): Promise<void> {
    const slash = filePath.lastIndexOf('/');
    if (slash <= 0) return;
    const folder = filePath.slice(0, slash);
    if (this.deps.vault.getAbstractFileByPath(folder)) return;
    const parts = folder.split('/').filter(Boolean);
    let cur = '';
    for (const seg of parts) {
      cur = cur ? `${cur}/${seg}` : seg;
      if (this.deps.vault.getAbstractFileByPath(cur)) continue;
      try {
        await this.deps.vault.createFolder(cur);
      } catch (e) {
        // A folder that physically exists (prior session) looks absent via
        // a cold metadata cache and createFolder() throws "already exists."
        // Benign — the folder is there. Other failures must propagate.
        if (!/already exists/i.test(String((e as Error)?.message ?? e))) throw e;
      }
    }
  }
}
