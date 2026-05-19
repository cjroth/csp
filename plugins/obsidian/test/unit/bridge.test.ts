import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { Vault, type VaultInstance, memoryStorage } from '@csp/sdk/web-init';
import { ObsidianVaultBridge } from '../../src/bridge.js';
import { shouldSync } from '../../src/path-filter.js';
import { type FakeTAbstractFile, FakeTFile, FakeVault } from '../mocks/obsidian.js';

let sdk: VaultInstance;
let vault: FakeVault;
let bridge: ObsidianVaultBridge;
let logged: string[];

function makeBridge(opts: { ignoreGlobs?: string[] } = {}) {
  logged = [];
  bridge = new ObsidianVaultBridge({
    vault,
    sdk,
    filter: (p) => shouldSync(p, opts.ignoreGlobs ?? []),
    log: (m) => logged.push(m),
  });
  return bridge;
}

beforeEach(async () => {
  vault = new FakeVault();
  sdk = await Vault.create({ storage: memoryStorage() });
  makeBridge();
});

afterEach(async () => {
  await sdk.close();
});

describe('default isFile predicate', () => {
  test('uses TFile.extension to discriminate when no override is passed', async () => {
    const b = new ObsidianVaultBridge({ vault, sdk, filter: () => true });
    const tfile = await vault.create('hello.md', 'hi');
    await b.handleObsidianWrite(tfile);
    expect(sdk.fileExists('hello.md')).toBe(true);
    const folderShape = { path: 'Drafts', name: 'Drafts' } as FakeTAbstractFile;
    await b.handleObsidianWrite(folderShape);
    expect(sdk.fileExists('Drafts')).toBe(false);
    // biome-ignore lint/suspicious/noExplicitAny: covering the null branch
    await b.handleObsidianWrite(null as any);
  });
});

describe('handleObsidianWrite', () => {
  test('pushes a text file on create/modify', async () => {
    const f = await vault.create('a.md', 'hello');
    await bridge.handleObsidianWrite(f);
    expect(sdk.fileExists('a.md')).toBe(true);
    expect(await sdk.readTextFile('a.md')).toBe('hello');
    expect(bridge.pushed).toBe(1);
  });

  test('a folder event preserves the empty folder via a .keep sentinel', async () => {
    const folder = { path: 'Drafts', name: 'Drafts' } as FakeTAbstractFile;
    await bridge.handleObsidianWrite(folder);
    expect(sdk.fileExists('Drafts/.keep')).toBe(true);
    expect(bridge.pushed).toBe(1);
    // Idempotent: the folder already has its sentinel → no extra push.
    await bridge.handleObsidianWrite(folder);
    expect(bridge.pushed).toBe(1);
  });

  test('skips paths the filter rejects (binary)', async () => {
    const f = await vault.create('img.png', 'binarydata');
    await bridge.handleObsidianWrite(f);
    expect(sdk.fileExists('img.png')).toBe(false);
    expect(bridge.skipped).toBe(1);
  });

  test('skips when content already equal (loop short-circuit)', async () => {
    const f = await vault.create('a.md', 'same');
    await sdk.writeTextFile('a.md', 'same');
    await bridge.handleObsidianWrite(f);
    expect(bridge.pushed).toBe(0);
  });

  test('respects suppression — consumes one token then bails', async () => {
    const f = await vault.create('a.md', 'hello');
    bridge.suppress('a.md');
    await bridge.handleObsidianWrite(f);
    expect(sdk.fileExists('a.md')).toBe(false);
    await bridge.handleObsidianWrite(f);
    expect(sdk.fileExists('a.md')).toBe(true);
  });

  test('suppression count > 1 decrements rather than deletes', async () => {
    const f = await vault.create('a.md', 'hello');
    bridge.suppress('a.md');
    bridge.suppress('a.md');
    await bridge.handleObsidianWrite(f);
    await bridge.handleObsidianWrite(f);
    expect(sdk.fileExists('a.md')).toBe(false);
    await bridge.handleObsidianWrite(f);
    expect(sdk.fileExists('a.md')).toBe(true);
  });
});

describe('handleObsidianDelete', () => {
  test('deletes from engine when known', async () => {
    await sdk.writeTextFile('a.md', 'hi');
    await bridge.handleObsidianDelete(new FakeTFile('a.md'));
    expect(sdk.fileExists('a.md')).toBe(false);
    expect(bridge.pushed).toBe(1);
  });

  test('no-ops when engine does not know the path', async () => {
    await bridge.handleObsidianDelete(new FakeTFile('never.md'));
    expect(bridge.pushed).toBe(0);
  });

  test('respects suppression', async () => {
    await sdk.writeTextFile('a.md', 'hi');
    bridge.suppress('a.md');
    await bridge.handleObsidianDelete(new FakeTFile('a.md'));
    expect(sdk.fileExists('a.md')).toBe(true);
  });
});

describe('handleObsidianRename', () => {
  test('rename within scope renames in engine', async () => {
    await sdk.writeTextFile('old.md', 'x');
    await bridge.handleObsidianRename(new FakeTFile('new.md'), 'old.md');
    expect(sdk.fileExists('old.md')).toBe(false);
    expect(sdk.fileExists('new.md')).toBe(true);
    expect(bridge.pushed).toBe(1);
  });

  test('rename when engine never knew old path falls back to write', async () => {
    const tfile = await vault.create('new.md', 'fresh');
    await bridge.handleObsidianRename(tfile, 'old.md');
    expect(await sdk.readTextFile('new.md')).toBe('fresh');
  });

  test('rename out of scope deletes from engine', async () => {
    await sdk.writeTextFile('old.md', 'x');
    await bridge.handleObsidianRename(new FakeTFile('old.png'), 'old.md');
    expect(sdk.fileExists('old.md')).toBe(false);
  });

  test('rename into scope writes the file', async () => {
    const tfile = await vault.create('new.md', 'fresh');
    await bridge.handleObsidianRename(tfile, 'old.png');
    expect(await sdk.readTextFile('new.md')).toBe('fresh');
  });

  test('rename neither side allowed → no-op', async () => {
    await bridge.handleObsidianRename(new FakeTFile('img2.png'), 'img1.png');
    expect(bridge.pushed).toBe(0);
  });

  test('rename respects suppression', async () => {
    await sdk.writeTextFile('old.md', 'x');
    bridge.suppress('new.md');
    await bridge.handleObsidianRename(new FakeTFile('new.md'), 'old.md');
    expect(sdk.fileExists('old.md')).toBe(true);
    expect(sdk.fileExists('new.md')).toBe(false);
  });
});

describe('applyOneRemoteFile', () => {
  test('creates file in Obsidian when missing', async () => {
    await sdk.writeTextFile('Notes/x.md', 'hello');
    const meta = sdk.listFiles().find((m) => m.path === 'Notes/x.md');
    await bridge.applyOneRemoteFile(meta as NonNullable<typeof meta>);
    const f = vault.getAbstractFileByPath('Notes/x.md');
    expect(f).not.toBeNull();
    expect(await vault.read(f as FakeTFile)).toBe('hello');
    expect(bridge.pulled).toBe(1);
  });

  test('modifies file when content differs', async () => {
    await vault.create('a.md', 'old');
    await sdk.writeTextFile('a.md', 'new');
    const meta = sdk.listFiles().find((m) => m.path === 'a.md');
    await bridge.applyOneRemoteFile(meta as NonNullable<typeof meta>);
    expect(await vault.read(vault.getAbstractFileByPath('a.md') as FakeTFile)).toBe('new');
  });

  test('skips kind != Text', async () => {
    await bridge.applyOneRemoteFile({
      id: 'fake',
      path: 'img.bin',
      kind: 'Binary',
      size: 1,
      created_at: 0,
      updated_at: 0,
    });
    expect(bridge.pulled).toBe(0);
  });

  test('skips when filter rejects (.context/ HARD INVARIANT)', async () => {
    await bridge.applyOneRemoteFile({
      id: 'fake',
      path: '.context/state',
      kind: 'Text',
      size: 1,
      created_at: 0,
      updated_at: 0,
    });
    expect(bridge.pulled).toBe(0);
  });

  test('deletes from Obsidian when engine has tombstone', async () => {
    await vault.create('a.md', 'doomed');
    await bridge.applyOneRemoteFile({
      id: 'fake',
      path: 'a.md',
      kind: 'Text',
      size: 0,
      created_at: 0,
      updated_at: 0,
      deleted_at: Date.now(),
    });
    expect(vault.getAbstractFileByPath('a.md')).toBeNull();
    expect(bridge.pulled).toBe(1);
  });

  test('tombstone with no local file is a no-op', async () => {
    await bridge.applyOneRemoteFile({
      id: 'fake',
      path: 'never.md',
      kind: 'Text',
      size: 0,
      created_at: 0,
      updated_at: 0,
      deleted_at: Date.now(),
    });
    expect(bridge.pulled).toBe(0);
  });
});

describe('applyRemoteState + ensureFolderFor', () => {
  test('creates parent folders before file', async () => {
    await sdk.writeTextFile('a/b/c.md', 'deep');
    await bridge.applyRemoteState();
    expect(vault.getAbstractFileByPath('a')).not.toBeNull();
    expect(vault.getAbstractFileByPath('a/b')).not.toBeNull();
    expect(vault.getAbstractFileByPath('a/b/c.md')).not.toBeNull();
  });

  test('flat path needs no folder creation', async () => {
    await sdk.writeTextFile('flat.md', 'x');
    await bridge.applyRemoteState();
    expect(vault.getAbstractFileByPath('flat.md')).not.toBeNull();
  });

  test('tolerates cold-cache "Folder already exists." race', async () => {
    let calls = 0;
    const stub = {
      getFiles: () => [],
      getAbstractFileByPath: () => null,
      read: async () => '',
      create: async () => ({}) as never,
      modify: async () => {},
      delete: async () => {},
      rename: async () => {},
      createFolder: async () => {
        calls += 1;
        throw new Error('Folder already exists.');
      },
    };
    const b = new ObsidianVaultBridge({ vault: stub as never, sdk, filter: () => true });
    await b.ensureFolderFor('a/b/c.md');
    expect(calls).toBeGreaterThan(0);
  });

  test('propagates a non-"already exists" createFolder failure', async () => {
    const stub = {
      getFiles: () => [],
      getAbstractFileByPath: () => null,
      read: async () => '',
      create: async () => ({}) as never,
      modify: async () => {},
      delete: async () => {},
      rename: async () => {},
      createFolder: async () => {
        throw new Error('EACCES: permission denied');
      },
    };
    const b = new ObsidianVaultBridge({ vault: stub as never, sdk, filter: () => true });
    await expect(b.ensureFolderFor('x/y.md')).rejects.toThrow(/permission denied/);
  });
});

describe('applyOneRemoteFile — cold metadata cache race', () => {
  test('recovers from "File already exists." by writing remote content', async () => {
    await sdk.writeTextFile('note.md', 'remote-content');
    const meta = sdk.listFiles().find((m) => m.path === 'note.md');
    let getCalls = 0;
    const modifiedWith: string[] = [];
    const stub = {
      getFiles: () => [],
      getAbstractFileByPath: (p: string) => {
        if (p !== 'note.md') return null;
        getCalls += 1;
        return getCalls === 1 ? null : new FakeTFile('note.md');
      },
      read: async () => '',
      create: async () => {
        throw new Error('File already exists.');
      },
      modify: async (_f: unknown, data: string) => {
        modifiedWith.push(data);
      },
      delete: async () => {},
      rename: async () => {},
      createFolder: async () => {},
    };
    const b = new ObsidianVaultBridge({ vault: stub as never, sdk, filter: () => true });
    await b.applyOneRemoteFile(meta as NonNullable<typeof meta>);
    expect(modifiedWith).toEqual(['remote-content']);
  });

  test('propagates a non-"already exists" create failure', async () => {
    await sdk.writeTextFile('boom.md', 'x');
    const meta = sdk.listFiles().find((m) => m.path === 'boom.md');
    const stub = {
      getFiles: () => [],
      getAbstractFileByPath: () => null,
      read: async () => '',
      create: async () => {
        throw new Error('ENOSPC: no space left on device');
      },
      modify: async () => {},
      delete: async () => {},
      rename: async () => {},
      createFolder: async () => {},
    };
    const b = new ObsidianVaultBridge({ vault: stub as never, sdk, filter: () => true });
    await expect(b.applyOneRemoteFile(meta as NonNullable<typeof meta>)).rejects.toThrow(
      /no space left/,
    );
  });
});
