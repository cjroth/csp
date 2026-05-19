// ConfigStore write-chain recovery: connect-mode setup can fire two
// near-simultaneous saves (peer-key pin on `connected` + the onboarding
// latch). The chain must survive a failed save and still apply the next.

import { describe, expect, test } from 'bun:test';
import { ConfigStore, DEFAULT_SETTINGS } from '../../src/settings.js';
import { FakeDataAdapter } from '../mocks/obsidian.js';

/** A FakeDataAdapter whose first `write` to the tmp file fails once. */
class FlakyAdapter extends FakeDataAdapter {
  failNextWrite = false;
  override async write(path: string, data: string): Promise<void> {
    if (this.failNextWrite && path.endsWith('.tmp')) {
      this.failNextWrite = false;
      throw new Error('EIO: simulated write fault');
    }
    return super.write(path, data);
  }
}

describe('ConfigStore.save resilience', () => {
  test('a failed save rejects but the chain recovers on the next save', async () => {
    const fs = new FlakyAdapter();
    const store = new ConfigStore(fs);
    await store.load();

    fs.failNextWrite = true;
    await expect(store.save({ ...DEFAULT_SETTINGS, peerUrl: 'wss://first:1' })).rejects.toThrow(
      /simulated write fault/,
    );

    // The chain's error arm must still run the subsequent save.
    await store.save({ ...DEFAULT_SETTINGS, peerUrl: 'wss://second:2' });
    const reloaded = await new ConfigStore(fs).load();
    expect(reloaded.peerUrl).toBe('wss://second:2');
  });

  test('serialized concurrent saves all apply in order', async () => {
    const fs = new FakeDataAdapter();
    const store = new ConfigStore(fs);
    await store.load();
    await Promise.all([
      store.save({ ...DEFAULT_SETTINGS, peerUrl: 'wss://a:1' }),
      store.save({ ...DEFAULT_SETTINGS, peerUrl: 'wss://b:2', onboarded: true }),
    ]);
    const reloaded = await new ConfigStore(fs).load();
    expect(reloaded.peerUrl).toBe('wss://b:2');
    expect(reloaded.onboarded).toBe(true);
  });
});
