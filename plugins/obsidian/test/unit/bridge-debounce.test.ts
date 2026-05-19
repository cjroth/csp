// The engine→Obsidian debounce machinery: leading-edge schedule, in-flight
// coalescing + trailing re-run, and dispose() gating a late timer so it
// can't poke a freed engine session. (bridge.test.ts covers the file I/O.)

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

const tick = (ms: number) => new Promise((r) => setTimeout(r, ms));

describe('scheduleApplyRemoteState', () => {
  test('coalesces a burst into a single applied pass', async () => {
    await sdk.writeTextFile('a.md', '1');
    // Many events in one tick → one debounced pass.
    for (let i = 0; i < 8; i++) bridge.scheduleApplyRemoteState();
    expect(vault.getAbstractFileByPath('a.md')).toBeNull(); // not yet (debounced)
    await tick(260);
    expect(vault.getAbstractFileByPath('a.md')).not.toBeNull();
  });

  test('an event arriving during an in-flight pass triggers a trailing pass', async () => {
    await sdk.writeTextFile('first.md', 'x');
    bridge.scheduleApplyRemoteState();
    await tick(260); // first pass applied
    expect(vault.getAbstractFileByPath('first.md')).not.toBeNull();

    // Now write a second file and immediately re-schedule while a pass may
    // still be settling; the trailing-pass path must still apply it.
    await sdk.writeTextFile('second.md', 'y');
    bridge.scheduleApplyRemoteState();
    bridge.scheduleApplyRemoteState();
    await tick(300);
    expect(vault.getAbstractFileByPath('second.md')).not.toBeNull();
  });

  test('dispose() clears a pending timer and later schedules are inert', async () => {
    await sdk.writeTextFile('late.md', 'z');
    bridge.scheduleApplyRemoteState(); // arms the 200ms timer
    bridge.dispose(); // must clear it before it fires
    await tick(260);
    expect(vault.getAbstractFileByPath('late.md')).toBeNull();
    // A schedule after dispose is a no-op (no throw, nothing applied).
    bridge.scheduleApplyRemoteState();
    await tick(260);
    expect(vault.getAbstractFileByPath('late.md')).toBeNull();
  });
});
