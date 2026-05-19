// engine→Obsidian empty-folder reaping. The engine models files only, so a
// folder rename arrives as N file moves + N deletes; without pruning, the
// emptied source folder (and its empty subfolders) lingers in the vault.

import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { Vault, type VaultInstance, memoryStorage } from '@csp/sdk/web-init';
import { ObsidianVaultBridge } from '../../src/bridge.js';
import { FakeVault } from '../mocks/obsidian.js';

let sdk: VaultInstance;
let vault: FakeVault;
let bridge: ObsidianVaultBridge;

beforeEach(async () => {
  vault = new FakeVault();
  sdk = await Vault.create({ storage: memoryStorage() });
  bridge = new ObsidianVaultBridge({ vault, sdk, filter: () => true });
});
afterEach(async () => {
  bridge.dispose();
  await sdk.close();
});

describe('pruneEmptyFolders', () => {
  test('reaps the emptied source folder + nested subfolders after a delete', async () => {
    await sdk.writeTextFile('Notes/sub/x.md', 'hi');
    await bridge.applyRemoteState();
    expect(vault.getAbstractFileByPath('Notes/sub/x.md')).not.toBeNull();
    expect(vault.getAbstractFileByPath('Notes/sub')).not.toBeNull();
    expect(vault.getAbstractFileByPath('Notes')).not.toBeNull();

    await sdk.deleteFile('Notes/sub/x.md'); // engine tombstone
    await bridge.applyRemoteState(); // delete file + prune empties

    expect(vault.getAbstractFileByPath('Notes/sub/x.md')).toBeNull();
    expect(vault.getAbstractFileByPath('Notes/sub')).toBeNull();
    expect(vault.getAbstractFileByPath('Notes')).toBeNull();
  });

  test('keeps a folder that still holds a sibling file', async () => {
    await sdk.writeTextFile('A/x.md', 'x');
    await sdk.writeTextFile('A/y.md', 'y');
    await bridge.applyRemoteState();

    await sdk.deleteFile('A/x.md');
    await bridge.applyRemoteState();

    expect(vault.getAbstractFileByPath('A/x.md')).toBeNull();
    expect(vault.getAbstractFileByPath('A/y.md')).not.toBeNull();
    expect(vault.getAbstractFileByPath('A')).not.toBeNull();
  });

  test('a folder rename (move + tombstone) leaves no empty old folder', async () => {
    await sdk.writeTextFile('Old/a.md', '1');
    await sdk.writeTextFile('Old/b.md', '2');
    await bridge.applyRemoteState();
    expect(vault.getAbstractFileByPath('Old')).not.toBeNull();

    // Rename the folder Old/ → New/ at the engine (per-file moves).
    await sdk.renameFile('Old/a.md', 'New/a.md');
    await sdk.renameFile('Old/b.md', 'New/b.md');
    await bridge.applyRemoteState();

    expect(vault.getAbstractFileByPath('New/a.md')).not.toBeNull();
    expect(vault.getAbstractFileByPath('New/b.md')).not.toBeNull();
    expect(vault.getAbstractFileByPath('Old/a.md')).toBeNull();
    expect(vault.getAbstractFileByPath('Old')).toBeNull();
  });

  test('tombstone delete via applyOneRemoteFile also prunes', async () => {
    await sdk.writeTextFile('Dir/only.md', 'z');
    await bridge.applyRemoteState();
    await bridge.applyOneRemoteFile({
      id: 'x',
      path: 'Dir/only.md',
      kind: 'Text',
      size: 0,
      created_at: 0,
      updated_at: 0,
      deleted_at: Date.now(),
    });
    expect(vault.getAbstractFileByPath('Dir/only.md')).toBeNull();
    expect(vault.getAbstractFileByPath('Dir')).toBeNull();
  });
});
