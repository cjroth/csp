// Exhaustive coverage for file/folder move, rename, and delete operations
// driven through real Obsidian event semantics. The FakeVault mock fires
// events the way real Obsidian does:
//   - folder rename → folder event THEN per-child file rename events
//   - folder delete → per-child file delete events THEN folder event
//   - external rename → create(new) + delete(old), no rename event
// The bridge is wired to the vault as the production plugin does in main.ts:
// fire-and-forget dispatch. Tests use `settle()` to wait for all in-flight
// bridge work to drain before asserting.

import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { Vault, type VaultInstance, memoryStorage } from '@csp/sdk/web-init';
import { ObsidianVaultBridge } from '../../src/bridge.js';
import { shouldSync } from '../../src/path-filter.js';
import {
  FakeDataAdapter,
  type FakeTAbstractFile,
  type FakeTFile,
  FakeVault,
  type FolderEventStrategy,
} from '../mocks/obsidian.js';

// ---------- shared setup ----------

let sdk: VaultInstance;
let vault: FakeVault;
let bridge: ObsidianVaultBridge;
let pending: Set<Promise<void>>;
let settle: () => Promise<void>;

function makeBridge(opts: { ignoreGlobs?: string[] } = {}): ObsidianVaultBridge {
  const b = new ObsidianVaultBridge({
    vault,
    sdk,
    filter: (p) => shouldSync(p, opts.ignoreGlobs ?? []),
    log: () => {},
  });
  return b;
}

/**
 * Wire vault events to bridge handlers the way main.ts does — fire-and-forget
 * dispatch. Returns a `settle()` that waits for every dispatched bridge call
 * (and any cascading work) to drain.
 */
function wire(b: ObsidianVaultBridge, v: FakeVault): { settle: () => Promise<void> } {
  const p = new Set<Promise<void>>();
  const track = (q: Promise<void> | undefined): void => {
    if (!q) return;
    p.add(q);
    q.catch(() => {}).finally(() => p.delete(q));
  };
  v.on('create', (f) => track(b.handleObsidianWrite(f)));
  v.on('modify', (f) => track(b.handleObsidianWrite(f)));
  v.on('delete', (f) => track(b.handleObsidianDelete(f)));
  v.on('rename', (f, oldPath) => track(b.handleObsidianRename(f, oldPath as string)));
  return {
    settle: async () => {
      // Drain in waves — a bridge handler may dispatch more work which
      // schedules a new vault op, which fires a new event, which dispatches
      // another bridge call. Loop until quiescent.
      for (let i = 0; i < 50 && p.size > 0; i++) {
        await Promise.allSettled([...p]);
      }
      if (p.size > 0) throw new Error(`bridge did not settle: ${p.size} pending`);
    },
  };
}

/** Snapshot of every alive engine path. Tombstones excluded. */
function engineState(s: VaultInstance): string[] {
  return s
    .listFiles()
    .filter((m) => !m.deleted_at && m.kind === 'Text')
    .map((m) => m.path)
    .sort();
}

/** Snapshot of every TFile in the FakeVault, sorted. */
function vaultFiles(v: FakeVault): string[] {
  return v
    .getFiles()
    .map((f) => f.path)
    .sort();
}

beforeEach(async () => {
  vault = new FakeVault();
  sdk = await Vault.create({ storage: memoryStorage() });
  bridge = makeBridge();
  const w = wire(bridge, vault);
  pending = new Set();
  settle = w.settle;
});

afterEach(async () => {
  bridge.dispose();
  await sdk.close();
});

// ============================================================================
// FILE-LEVEL OPS (single file, no folder traversal)
// ============================================================================

describe('file ops — drag-move equivalents (single rename event)', () => {
  test('rename file in place (same folder)', async () => {
    const f = await vault.create('a.md', 'one');
    await settle();
    expect(engineState(sdk)).toEqual(['a.md']);

    await vault.rename(f, 'renamed.md');
    await settle();

    expect(engineState(sdk)).toEqual(['renamed.md']);
    expect(await sdk.readTextFile('renamed.md')).toBe('one');
    expect(sdk.fileExists('a.md')).toBe(false);
  });

  test('move file across two folders (same depth)', async () => {
    await vault.createFolder('A');
    await vault.createFolder('B');
    const f = await vault.create('A/note.md', 'payload');
    await settle();
    expect(engineState(sdk)).toEqual(['A/note.md']);

    await vault.rename(f, 'B/note.md');
    await settle();

    expect(engineState(sdk)).toEqual(['B/note.md']);
    expect(await sdk.readTextFile('B/note.md')).toBe('payload');
    expect(sdk.fileExists('A/note.md')).toBe(false);
  });

  test('move file into a deeper folder', async () => {
    await vault.createFolder('A');
    await vault.createFolder('A/B');
    await vault.createFolder('A/B/C');
    const f = await vault.create('A/note.md', 'deep');
    await settle();

    await vault.rename(f, 'A/B/C/note.md');
    await settle();

    expect(engineState(sdk)).toEqual(['A/B/C/note.md']);
    expect(await sdk.readTextFile('A/B/C/note.md')).toBe('deep');
  });

  test('move file out of a deep folder to root', async () => {
    await vault.createFolder('X');
    await vault.createFolder('X/Y');
    const f = await vault.create('X/Y/note.md', 'up');
    await settle();

    await vault.rename(f, 'note.md');
    await settle();

    expect(engineState(sdk)).toEqual(['note.md']);
    expect(await sdk.readTextFile('note.md')).toBe('up');
  });

  test('rename file changes only the basename', async () => {
    await vault.createFolder('A');
    const f = await vault.create('A/old.md', 'x');
    await settle();

    await vault.rename(f, 'A/new.md');
    await settle();

    expect(engineState(sdk)).toEqual(['A/new.md']);
  });

  test('two sequential moves of the same file', async () => {
    await vault.createFolder('A');
    await vault.createFolder('B');
    await vault.createFolder('C');
    const f = await vault.create('A/x.md', 'travel');
    await settle();

    await vault.rename(f, 'B/x.md');
    await settle();
    await vault.rename(f, 'C/x.md');
    await settle();

    expect(engineState(sdk)).toEqual(['C/x.md']);
    expect(await sdk.readTextFile('C/x.md')).toBe('travel');
  });

  test('move file then modify it', async () => {
    await vault.createFolder('A');
    await vault.createFolder('B');
    const f = await vault.create('A/x.md', 'first');
    await settle();

    await vault.rename(f, 'B/x.md');
    await settle();
    await vault.modify(f, 'second');
    await settle();

    expect(engineState(sdk)).toEqual(['B/x.md']);
    expect(await sdk.readTextFile('B/x.md')).toBe('second');
  });
});

// ============================================================================
// FOLDER RENAME (real-obsidian: folder event + per-child events)
// ============================================================================

describe('folder rename — real-Obsidian event semantics', () => {
  test('rename a folder containing one file', async () => {
    await vault.createFolder('Old');
    await vault.create('Old/a.md', 'aa');
    await settle();
    expect(engineState(sdk)).toEqual(['Old/a.md']);

    const folder = vault.getAbstractFileByPath('Old');
    expect(folder).not.toBeNull();
    await vault.rename(folder as FakeTAbstractFile, 'New');
    await settle();

    expect(engineState(sdk)).toEqual(['New/a.md']);
    expect(sdk.fileExists('Old/a.md')).toBe(false);
    expect(await sdk.readTextFile('New/a.md')).toBe('aa');
  });

  test('rename a folder containing multiple files', async () => {
    await vault.createFolder('Old');
    for (const n of ['a', 'b', 'c', 'd', 'e']) {
      await vault.create(`Old/${n}.md`, n);
    }
    await settle();
    expect(engineState(sdk)).toEqual(
      ['Old/a.md', 'Old/b.md', 'Old/c.md', 'Old/d.md', 'Old/e.md'].sort(),
    );

    const folder = vault.getAbstractFileByPath('Old');
    await vault.rename(folder as FakeTAbstractFile, 'New');
    await settle();

    expect(engineState(sdk)).toEqual(
      ['New/a.md', 'New/b.md', 'New/c.md', 'New/d.md', 'New/e.md'].sort(),
    );
    // None of the old paths linger.
    for (const n of ['a', 'b', 'c', 'd', 'e']) {
      expect(sdk.fileExists(`Old/${n}.md`)).toBe(false);
    }
  });

  test('rename folder under a deeper parent (folder move across parents)', async () => {
    await vault.createFolder('Parent1');
    await vault.createFolder('Parent2');
    await vault.createFolder('Parent1/Sub');
    await vault.create('Parent1/Sub/a.md', 'A');
    await vault.create('Parent1/Sub/b.md', 'B');
    await settle();

    const folder = vault.getAbstractFileByPath('Parent1/Sub');
    await vault.rename(folder as FakeTAbstractFile, 'Parent2/Sub');
    await settle();

    expect(engineState(sdk)).toEqual(['Parent2/Sub/a.md', 'Parent2/Sub/b.md'].sort());
  });

  test('rename folder containing nested subfolders (recursive)', async () => {
    await vault.createFolder('A');
    await vault.createFolder('A/B');
    await vault.createFolder('A/B/C');
    await vault.create('A/top.md', 'top');
    await vault.create('A/B/mid.md', 'mid');
    await vault.create('A/B/C/deep.md', 'deep');
    await settle();
    expect(engineState(sdk)).toEqual(['A/B/C/deep.md', 'A/B/mid.md', 'A/top.md']);

    const folder = vault.getAbstractFileByPath('A');
    await vault.rename(folder as FakeTAbstractFile, 'Z');
    await settle();

    expect(engineState(sdk)).toEqual(['Z/B/C/deep.md', 'Z/B/mid.md', 'Z/top.md']);
    // No old paths.
    expect(sdk.fileExists('A/top.md')).toBe(false);
    expect(sdk.fileExists('A/B/mid.md')).toBe(false);
    expect(sdk.fileExists('A/B/C/deep.md')).toBe(false);
  });

  test('rename folder 5 levels deep', async () => {
    const path = 'a/b/c/d/e';
    for (let i = 1; i <= 5; i++) await vault.createFolder(path.split('/').slice(0, i).join('/'));
    await vault.create('a/b/c/d/e/leaf.md', 'leaf');
    await settle();

    const folder = vault.getAbstractFileByPath('a/b/c');
    await vault.rename(folder as FakeTAbstractFile, 'a/b/X');
    await settle();

    expect(engineState(sdk)).toEqual(['a/b/X/d/e/leaf.md']);
    expect(sdk.fileExists('a/b/c/d/e/leaf.md')).toBe(false);
  });

  test('rename folder with 200 files (large folder stress)', async () => {
    await vault.createFolder('Big');
    const names: string[] = [];
    for (let i = 0; i < 200; i++) {
      const n = `f${String(i).padStart(3, '0')}.md`;
      names.push(n);
      await vault.create(`Big/${n}`, `payload-${i}`);
    }
    await settle();
    expect(engineState(sdk).length).toBe(200);

    const folder = vault.getAbstractFileByPath('Big');
    await vault.rename(folder as FakeTAbstractFile, 'Huge');
    await settle();

    const want = names.map((n) => `Huge/${n}`).sort();
    expect(engineState(sdk)).toEqual(want);
    // Every old path is gone.
    for (const n of names) expect(sdk.fileExists(`Big/${n}`)).toBe(false);
  });

  test('rename folder that itself sits inside a folder (mid-tree rename)', async () => {
    await vault.createFolder('Root');
    await vault.createFolder('Root/Inner');
    await vault.create('Root/Inner/leaf.md', 'leaf');
    await vault.create('Root/sibling.md', 'sib');
    await settle();

    const folder = vault.getAbstractFileByPath('Root/Inner');
    await vault.rename(folder as FakeTAbstractFile, 'Root/Renamed');
    await settle();

    expect(engineState(sdk)).toEqual(['Root/Renamed/leaf.md', 'Root/sibling.md']);
  });

  test('rename folder to a sibling under a different parent', async () => {
    await vault.createFolder('P1');
    await vault.createFolder('P2');
    await vault.createFolder('P1/Inner');
    await vault.create('P1/Inner/x.md', 'x');
    await vault.create('P1/Inner/y.md', 'y');
    await settle();

    const folder = vault.getAbstractFileByPath('P1/Inner');
    await vault.rename(folder as FakeTAbstractFile, 'P2/Inner');
    await settle();

    expect(engineState(sdk)).toEqual(['P2/Inner/x.md', 'P2/Inner/y.md']);
    expect(sdk.fileExists('P1/Inner/x.md')).toBe(false);
  });

  test('rename folder back to original name (round-trip)', async () => {
    await vault.createFolder('A');
    await vault.create('A/x.md', '1');
    await settle();

    const f1 = vault.getAbstractFileByPath('A');
    await vault.rename(f1 as FakeTAbstractFile, 'B');
    await settle();
    expect(engineState(sdk)).toEqual(['B/x.md']);

    const f2 = vault.getAbstractFileByPath('B');
    await vault.rename(f2 as FakeTAbstractFile, 'A');
    await settle();
    expect(engineState(sdk)).toEqual(['A/x.md']);
  });
});

// ============================================================================
// FOLDER DELETE
// ============================================================================

describe('folder delete — real-Obsidian event semantics', () => {
  test('delete a folder containing one file', async () => {
    await vault.createFolder('D');
    await vault.create('D/a.md', 'a');
    await settle();
    expect(engineState(sdk)).toEqual(['D/a.md']);

    const folder = vault.getAbstractFileByPath('D');
    await vault.delete(folder as FakeTAbstractFile);
    await settle();

    expect(engineState(sdk)).toEqual([]);
  });

  test('delete a folder containing many files', async () => {
    await vault.createFolder('D');
    for (let i = 0; i < 10; i++) await vault.create(`D/f${i}.md`, `${i}`);
    await settle();
    expect(engineState(sdk).length).toBe(10);

    const folder = vault.getAbstractFileByPath('D');
    await vault.delete(folder as FakeTAbstractFile);
    await settle();

    expect(engineState(sdk)).toEqual([]);
  });

  test('delete a folder with nested subfolders and files', async () => {
    await vault.createFolder('Root');
    await vault.createFolder('Root/A');
    await vault.createFolder('Root/A/B');
    await vault.create('Root/top.md', 't');
    await vault.create('Root/A/mid.md', 'm');
    await vault.create('Root/A/B/leaf.md', 'l');
    await settle();
    expect(engineState(sdk).length).toBe(3);

    const folder = vault.getAbstractFileByPath('Root');
    await vault.delete(folder as FakeTAbstractFile);
    await settle();

    expect(engineState(sdk)).toEqual([]);
  });

  test('delete a deeply-nested folder leaves siblings alone', async () => {
    await vault.createFolder('Root');
    await vault.createFolder('Root/A');
    await vault.createFolder('Root/B');
    await vault.create('Root/A/keep.md', 'k');
    await vault.create('Root/B/gone.md', 'g');
    await settle();

    const folder = vault.getAbstractFileByPath('Root/B');
    await vault.delete(folder as FakeTAbstractFile);
    await settle();

    expect(engineState(sdk)).toEqual(['Root/A/keep.md']);
  });

  test('delete a folder with 100 files', async () => {
    await vault.createFolder('Big');
    for (let i = 0; i < 100; i++) await vault.create(`Big/f${i}.md`, 'x');
    await settle();

    const folder = vault.getAbstractFileByPath('Big');
    await vault.delete(folder as FakeTAbstractFile);
    await settle();

    expect(engineState(sdk)).toEqual([]);
  });
});

// ============================================================================
// APPLYING REMOTE STATE — bridge as receiver
// ============================================================================

describe('applyRemoteState — receiving remote file/folder ops', () => {
  test('remote file create lands in Obsidian', async () => {
    await sdk.writeTextFile('remote.md', 'hi from elsewhere');
    await bridge.applyRemoteState();

    expect(vaultFiles(vault)).toEqual(['remote.md']);
    expect(await vault.read(vault.getAbstractFileByPath('remote.md') as FakeTFile)).toBe(
      'hi from elsewhere',
    );
  });

  test('remote file move lands in Obsidian (old removed, new created)', async () => {
    await sdk.writeTextFile('A/x.md', 'one');
    await bridge.applyRemoteState();
    expect(vaultFiles(vault)).toEqual(['A/x.md']);

    await sdk.renameFile('A/x.md', 'B/x.md');
    await bridge.applyRemoteState();

    expect(vaultFiles(vault)).toEqual(['B/x.md']);
    expect(vault.getAbstractFileByPath('A/x.md')).toBeNull();
  });

  test('remote folder rename — multiple per-file moves all apply', async () => {
    for (const n of ['a', 'b', 'c']) await sdk.writeTextFile(`Old/${n}.md`, n);
    await bridge.applyRemoteState();
    expect(vaultFiles(vault).sort()).toEqual(['Old/a.md', 'Old/b.md', 'Old/c.md']);

    for (const n of ['a', 'b', 'c']) await sdk.renameFile(`Old/${n}.md`, `New/${n}.md`);
    await bridge.applyRemoteState();

    expect(vaultFiles(vault).sort()).toEqual(['New/a.md', 'New/b.md', 'New/c.md']);
    // Old folder is reaped.
    expect(vault.getAbstractFileByPath('Old')).toBeNull();
  });

  test('remote folder rename across parents', async () => {
    await sdk.writeTextFile('P1/Inner/x.md', 'x');
    await sdk.writeTextFile('P1/Inner/y.md', 'y');
    await bridge.applyRemoteState();
    expect(vaultFiles(vault).sort()).toEqual(['P1/Inner/x.md', 'P1/Inner/y.md']);

    await sdk.renameFile('P1/Inner/x.md', 'P2/Inner/x.md');
    await sdk.renameFile('P1/Inner/y.md', 'P2/Inner/y.md');
    await bridge.applyRemoteState();

    expect(vaultFiles(vault).sort()).toEqual(['P2/Inner/x.md', 'P2/Inner/y.md']);
  });

  test('remote folder delete removes all files in Obsidian', async () => {
    await sdk.writeTextFile('D/a.md', 'a');
    await sdk.writeTextFile('D/b.md', 'b');
    await sdk.writeTextFile('D/sub/c.md', 'c');
    await bridge.applyRemoteState();

    await sdk.deleteFile('D/a.md');
    await sdk.deleteFile('D/b.md');
    await sdk.deleteFile('D/sub/c.md');
    await bridge.applyRemoteState();

    expect(vaultFiles(vault)).toEqual([]);
    expect(vault.getAbstractFileByPath('D')).toBeNull();
    expect(vault.getAbstractFileByPath('D/sub')).toBeNull();
  });

  test('remote folder delete with 50 files reaps all + folder', async () => {
    for (let i = 0; i < 50; i++) await sdk.writeTextFile(`Big/f${i}.md`, 'x');
    await bridge.applyRemoteState();
    expect(vaultFiles(vault).length).toBe(50);

    for (let i = 0; i < 50; i++) await sdk.deleteFile(`Big/f${i}.md`);
    await bridge.applyRemoteState();

    expect(vaultFiles(vault)).toEqual([]);
    expect(vault.getAbstractFileByPath('Big')).toBeNull();
  });

  test('remote rename then immediate rename-back', async () => {
    await sdk.writeTextFile('a.md', 'x');
    await bridge.applyRemoteState();

    await sdk.renameFile('a.md', 'b.md');
    await bridge.applyRemoteState();
    expect(vaultFiles(vault)).toEqual(['b.md']);

    await sdk.renameFile('b.md', 'a.md');
    await bridge.applyRemoteState();
    expect(vaultFiles(vault)).toEqual(['a.md']);
  });

  test('remote partial folder delete (some files remain)', async () => {
    await sdk.writeTextFile('D/keep.md', 'k');
    await sdk.writeTextFile('D/gone.md', 'g');
    await bridge.applyRemoteState();

    await sdk.deleteFile('D/gone.md');
    await bridge.applyRemoteState();

    expect(vaultFiles(vault)).toEqual(['D/keep.md']);
    expect(vault.getAbstractFileByPath('D')).not.toBeNull();
  });
});

// ============================================================================
// THE DUPLICATION BUG — applyRemoteState + stale metadata cache
// ============================================================================

describe('applyRemoteState — stale Obsidian metadata cache (the duplication bug)', () => {
  test('stale getAbstractFileByPath on the old path during a remote move does NOT leak the file', async () => {
    // Vault has file at Old/a.md (came in via reconcile earlier).
    await sdk.writeTextFile('Old/a.md', 'payload');
    await bridge.applyRemoteState();
    expect(vaultFiles(vault)).toEqual(['Old/a.md']);

    // Remote move: file is now at New/a.md.
    await sdk.renameFile('Old/a.md', 'New/a.md');

    // Simulate Obsidian's metadata cache lying about the old path: it claims
    // null even though the file is still physically present.
    vault.staleLookupPaths.add('Old/a.md');

    await bridge.applyRemoteState();

    // The duplication symptom: stale cache hides the file from the bridge's
    // tombstone pass, so the bridge never deletes the old path. Without the
    // fix this leaves Old/a.md in the vault alongside New/a.md. With the fix
    // the bridge must still remove it.
    expect(vaultFiles(vault)).toEqual(['New/a.md']);
  });

  test('stale cache during folder rename does not leak any old files', async () => {
    for (const n of ['a', 'b', 'c']) await sdk.writeTextFile(`Old/${n}.md`, n);
    await bridge.applyRemoteState();

    // The next pass should delete every Old/*.md and create every New/*.md.
    // Stale cache on every old path:
    vault.staleLookupPaths.add('Old/a.md');
    vault.staleLookupPaths.add('Old/b.md');
    vault.staleLookupPaths.add('Old/c.md');

    for (const n of ['a', 'b', 'c']) await sdk.renameFile(`Old/${n}.md`, `New/${n}.md`);
    await bridge.applyRemoteState();

    expect(vaultFiles(vault).sort()).toEqual(['New/a.md', 'New/b.md', 'New/c.md']);
  });

  test('stale cache on a remote delete still removes the file', async () => {
    await sdk.writeTextFile('A/note.md', 'p');
    await bridge.applyRemoteState();

    vault.staleLookupPaths.add('A/note.md');
    await sdk.deleteFile('A/note.md');
    await bridge.applyRemoteState();

    expect(vaultFiles(vault)).toEqual([]);
  });

  test('stale-cache scenario survives a subsequent pass too (no zombie state)', async () => {
    await sdk.writeTextFile('Old/x.md', 'p');
    await bridge.applyRemoteState();

    vault.staleLookupPaths.add('Old/x.md');
    await sdk.renameFile('Old/x.md', 'New/x.md');
    await bridge.applyRemoteState();
    // Clear the stale flag and reconcile again — should be idempotent.
    vault.staleLookupPaths.clear();
    await bridge.applyRemoteState();

    expect(vaultFiles(vault)).toEqual(['New/x.md']);
  });
});

// ============================================================================
// EXTERNAL RENAME (file moved outside Obsidian; events are create + delete)
// ============================================================================

describe('external rename — create+delete event pairs', () => {
  beforeEach(() => {
    vault.folderEventStrategy = 'external';
  });

  test('single file moved externally appears at new path, gone from old', async () => {
    const f = await vault.create('A/x.md', 'p');
    await settle();
    expect(engineState(sdk)).toEqual(['A/x.md']);

    await vault.rename(f, 'B/x.md');
    await settle();

    expect(engineState(sdk)).toEqual(['B/x.md']);
  });

  test('folder moved externally — every descendant fires create+delete', async () => {
    await vault.createFolder('Old');
    await vault.create('Old/a.md', 'A');
    await vault.create('Old/b.md', 'B');
    await settle();

    const folder = vault.getAbstractFileByPath('Old');
    await vault.rename(folder as FakeTAbstractFile, 'New');
    await settle();

    expect(engineState(sdk).sort()).toEqual(['New/a.md', 'New/b.md']);
  });
});

// ============================================================================
// LEGACY FOLDER-ONLY STRATEGY — bridge must still handle it
// ============================================================================

describe('legacy folder-only event strategy', () => {
  beforeEach(() => {
    vault.folderEventStrategy = 'folder-only';
  });

  test('folder rename with only a folder-level event still re-keys engine children', async () => {
    await vault.createFolder('Old');
    await vault.create('Old/a.md', '1');
    await vault.create('Old/b.md', '2');
    await settle();

    const folder = vault.getAbstractFileByPath('Old');
    await vault.rename(folder as FakeTAbstractFile, 'New');
    await settle();

    expect(engineState(sdk).sort()).toEqual(['New/a.md', 'New/b.md']);
  });

  test('folder delete with only a folder-level event still purges engine children', async () => {
    await vault.createFolder('D');
    await vault.create('D/a.md', '1');
    await vault.create('D/b.md', '2');
    await settle();

    const folder = vault.getAbstractFileByPath('D');
    await vault.delete(folder as FakeTAbstractFile);
    await settle();

    expect(engineState(sdk)).toEqual([]);
  });
});

// ============================================================================
// PER-CHILD-ONLY STRATEGY
// ============================================================================

describe('per-child-only event strategy', () => {
  beforeEach(() => {
    vault.folderEventStrategy = 'per-child-only';
  });

  test('folder rename emitting only per-child events still converges', async () => {
    await vault.createFolder('Old');
    await vault.create('Old/a.md', '1');
    await vault.create('Old/b.md', '2');
    await settle();

    const folder = vault.getAbstractFileByPath('Old');
    await vault.rename(folder as FakeTAbstractFile, 'New');
    await settle();

    expect(engineState(sdk).sort()).toEqual(['New/a.md', 'New/b.md']);
  });

  test('folder delete emitting only per-child events still purges engine state', async () => {
    await vault.createFolder('D');
    await vault.create('D/a.md', '1');
    await vault.create('D/b.md', '2');
    await settle();

    const folder = vault.getAbstractFileByPath('D');
    await vault.delete(folder as FakeTAbstractFile);
    await settle();

    expect(engineState(sdk)).toEqual([]);
  });
});

// ============================================================================
// EDGE CASES — empty folders, .keep sentinel, suppression, filtering
// ============================================================================

describe('edge cases — empty folders, .keep, suppression, filtering', () => {
  test('rename an empty folder (no children, no .keep) is a no-op for the engine', async () => {
    await vault.createFolder('Empty');
    // Fire a create event for the folder manually, since createFolder doesn't.
    await bridge.handleObsidianWrite(vault.getAbstractFileByPath('Empty') as FakeTAbstractFile);
    // The empty-folder code wrote a .keep sentinel. Confirm.
    expect(engineState(sdk)).toEqual(['Empty/.keep']);

    const folder = vault.getAbstractFileByPath('Empty');
    await vault.rename(folder as FakeTAbstractFile, 'Renamed');
    await settle();

    expect(engineState(sdk)).toEqual(['Renamed/.keep']);
  });

  test('delete the last file in a folder asserts a .keep so the folder lives on', async () => {
    await vault.createFolder('Stub');
    const f = await vault.create('Stub/note.md', 'gone soon');
    await settle();
    expect(engineState(sdk)).toEqual(['Stub/note.md']);

    await vault.delete(f);
    await settle();

    // Stub still exists in Obsidian; bridge protects it with a .keep so the
    // engine doesn't lose track of the empty folder.
    expect(engineState(sdk)).toEqual(['Stub/.keep']);
  });

  test('move file into a folder that is currently empty (with .keep) → real file lands', async () => {
    await vault.createFolder('Empty');
    // Plant a .keep manually by writing the folder create.
    await bridge.handleObsidianWrite(vault.getAbstractFileByPath('Empty') as FakeTAbstractFile);
    expect(engineState(sdk)).toContain('Empty/.keep');

    await vault.createFolder('Src');
    const f = await vault.create('Src/note.md', 'data');
    await settle();
    await vault.rename(f, 'Empty/note.md');
    await settle();

    // The real file must be at the new path. The engine drops .keep from
    // committed trees once the folder has real content (canonicalize_keeps
    // in csp-core/src/scope.rs §11), even though the SDK working map may
    // still surface it via listFiles() — a benign divergence the bridge
    // tolerates (the engine tree is authoritative, not the working map).
    expect(engineState(sdk)).toContain('Empty/note.md');
    expect(vaultFiles(vault)).toContain('Empty/note.md');
    expect(vault.getAbstractFileByPath('Src/note.md')).toBeNull();
  });

  test('rename of a path that the filter excludes — bridge ignores cleanly', async () => {
    bridge.dispose();
    bridge = makeBridge({ ignoreGlobs: ['**/*.png'] });
    wire(bridge, vault);

    const f = await vault.create('a.md', 'p');
    await settle();
    expect(engineState(sdk)).toEqual(['a.md']);

    await vault.rename(f, 'a.png');
    await settle();

    // The new path is filter-rejected → bridge removes from engine, leaves
    // Obsidian alone.
    expect(engineState(sdk)).toEqual([]);
    expect(vaultFiles(vault)).toEqual(['a.png']);
  });

  test('rename FROM filter-excluded path INTO scope (image renamed to .md)', async () => {
    bridge.dispose();
    bridge = makeBridge({ ignoreGlobs: ['**/*.png'] });
    wire(bridge, vault);

    const f = await vault.create('a.png', 'binary-ish');
    await settle();
    expect(engineState(sdk)).toEqual([]);

    await vault.rename(f, 'a.md');
    await settle();

    expect(engineState(sdk)).toEqual(['a.md']);
    expect(await sdk.readTextFile('a.md')).toBe('binary-ish');
  });

  test('rename suppression — applyRemoteState delete event does not echo back', async () => {
    await sdk.writeTextFile('A/x.md', 'p');
    await bridge.applyRemoteState();
    expect(vaultFiles(vault)).toEqual(['A/x.md']);
    const beforePushed = bridge.pushed;

    await sdk.renameFile('A/x.md', 'B/x.md');
    await bridge.applyRemoteState();

    expect(vaultFiles(vault)).toEqual(['B/x.md']);
    // The bridge's own writes/deletes during applyRemoteState must not be
    // re-echoed back into push.
    expect(bridge.pushed - beforePushed).toBe(0);
  });
});

// ============================================================================
// RACE: outbound rename racing with remote update on same area
// ============================================================================

describe('race conditions — outbound + remote interleaved', () => {
  test('local rename then immediate applyRemoteState on the new state is idempotent', async () => {
    await vault.createFolder('A');
    await vault.createFolder('B');
    const f = await vault.create('A/x.md', 'p');
    await settle();

    // Local rename in flight.
    const renamePromise = vault.rename(f, 'B/x.md');
    // Concurrently, applyRemoteState fires (e.g., a tree-changed from peer).
    const applyPromise = bridge.applyRemoteState();
    await Promise.all([renamePromise, applyPromise]);
    await settle();

    expect(engineState(sdk)).toEqual(['B/x.md']);
    expect(vaultFiles(vault)).toEqual(['B/x.md']);
  });

  test('local folder rename followed by applyRemoteState converges', async () => {
    await vault.createFolder('Old');
    for (const n of ['a', 'b', 'c']) await vault.create(`Old/${n}.md`, n);
    await settle();

    const folder = vault.getAbstractFileByPath('Old');
    await vault.rename(folder as FakeTAbstractFile, 'New');
    await settle();
    await bridge.applyRemoteState();

    expect(engineState(sdk).sort()).toEqual(['New/a.md', 'New/b.md', 'New/c.md']);
    expect(vaultFiles(vault).sort()).toEqual(['New/a.md', 'New/b.md', 'New/c.md']);
  });

  test('repeated folder renames in sequence — engine + vault stay in sync', async () => {
    await vault.createFolder('Step0');
    await vault.create('Step0/x.md', 'p');
    await settle();

    for (const next of ['Step1', 'Step2', 'Step3', 'Step4']) {
      const cur = vault.getAbstractFileByPath(vault.getFiles()[0]?.path.split('/')[0] ?? '');
      if (!cur) throw new Error('lost current folder');
      await vault.rename(cur, next);
      await settle();
    }

    expect(engineState(sdk)).toEqual(['Step4/x.md']);
    expect(vaultFiles(vault)).toEqual(['Step4/x.md']);
  });
});

// ============================================================================
// EVENT-SEQUENCE INVARIANTS — verify FakeVault matches real Obsidian
// ============================================================================

describe('FakeVault event-sequence invariants (real Obsidian fidelity)', () => {
  test('folder rename emits folder event first, then per-child events in path order', async () => {
    await vault.createFolder('Old');
    await vault.create('Old/a.md', '1');
    await vault.create('Old/b.md', '2');
    vault.emitted.length = 0;

    const folder = vault.getAbstractFileByPath('Old');
    await vault.rename(folder as FakeTAbstractFile, 'New');

    // First emission MUST be the folder rename.
    expect(vault.emitted[0]).toEqual({ name: 'rename', path: 'New', oldPath: 'Old' });
    // Then per-child renames.
    const childRenames = vault.emitted.slice(1);
    expect(childRenames.every((e) => e.name === 'rename')).toBe(true);
    const paths = childRenames.map((e) => e.path).sort();
    expect(paths).toEqual(['New/a.md', 'New/b.md']);
  });

  test('folder delete emits per-child events first, then the folder', async () => {
    await vault.createFolder('D');
    await vault.create('D/a.md', '1');
    await vault.create('D/b.md', '2');
    vault.emitted.length = 0;

    const folder = vault.getAbstractFileByPath('D');
    await vault.delete(folder as FakeTAbstractFile);

    // The last emission must be the folder delete.
    expect(vault.emitted[vault.emitted.length - 1]).toEqual({ name: 'delete', path: 'D' });
    // Every earlier emission is a per-child delete.
    const childDeletes = vault.emitted.slice(0, -1);
    expect(childDeletes.every((e) => e.name === 'delete')).toBe(true);
  });

  test('external rename emits create + delete and no rename event', async () => {
    vault.folderEventStrategy = 'external';
    const f = await vault.create('A/x.md', 'p');
    vault.emitted.length = 0;

    await vault.rename(f, 'B/x.md');

    const names = vault.emitted.map((e) => e.name);
    expect(names).toContain('create');
    expect(names).toContain('delete');
    expect(names).not.toContain('rename');
  });
});
