// Settings persistence is split into two files:
//   .context/config        the SHARED canonical VaultConfig (what `ctx` and
//                           the engine read); the plugin only projects
//                           `peerUrl` onto `peers` and leaves the rest alone.
//   .context/obsidian.json the node-local plugin sidecar — every plugin-only
//                           knob plus a fallback copy of `peerUrl`.
// These tests cover the peerUrl⇄peers mapping, `ctx`-written fields surviving
// a plugin save, sidecar persistence, defaults when files are absent, the
// "no canonical config minted on save" rule, and the atomic tmp+rename write.

import { describe, expect, test } from 'bun:test';
import { defaultConfig, parseConfig, serializeConfig } from '@csp/sdk/web-init';
import {
  ConfigStore,
  type CspSettings,
  DEFAULT_SETTINGS,
  parseIgnoreGlobs,
} from '../../src/settings.js';
import { FakeDataAdapter } from '../mocks/obsidian.js';

/** A canonical `.context/config` as the `ctx` CLI would have written it:
 * non-default vault_id, include, debounce_ms, and an already-set peer. */
function cliConfigText(): string {
  const cfg = defaultConfig('vault-from-ctx');
  cfg.include = ['notes/**', 'docs/**'];
  cfg.debounce_ms = 2500;
  cfg.peers = ['wss://node.example:7777'];
  return serializeConfig(cfg);
}

describe('parseIgnoreGlobs', () => {
  test('splits lines, trims, drops empty + comment lines', () => {
    expect(parseIgnoreGlobs('  Drafts/**\n\n# comment\n*.tmp.md\n')).toEqual([
      'Drafts/**',
      '*.tmp.md',
    ]);
  });
  test('empty input → empty list', () => {
    expect(parseIgnoreGlobs('')).toEqual([]);
    expect(parseIgnoreGlobs('   \n  \n')).toEqual([]);
  });
});

describe('ConfigStore.load — defaults', () => {
  test('both files absent → DEFAULT_SETTINGS', async () => {
    const store = new ConfigStore(new FakeDataAdapter());
    expect(await store.load()).toEqual(DEFAULT_SETTINGS);
  });

  test('only the sidecar present → plugin knobs restored, peerUrl from sidecar', async () => {
    const fs = new FakeDataAdapter();
    await fs.mkdir('.context');
    await fs.write(
      '.context/obsidian.json',
      JSON.stringify({
        peerUrl: 'wss://sidecar:9',
        peerPubkey: 'ssh-ed25519 AAAApin',
        syncEnabled: true,
        autoConnectOnStart: true,
        onboarded: true,
        ignoreGlobs: ['Drafts/**'],
        identityPath: '.context/id_ed25519',
      }),
    );
    const s = await new ConfigStore(fs).load();
    expect(s).toEqual({
      peerUrl: 'wss://sidecar:9',
      peerPubkey: 'ssh-ed25519 AAAApin',
      syncEnabled: true,
      autoConnectOnStart: true,
      onboarded: true,
      ignoreGlobs: ['Drafts/**'],
      identityPath: '.context/id_ed25519',
    });
  });
});

describe('peerUrl ⇄ peers mapping', () => {
  test('load: peerUrl comes from .context/config peers[0] when it exists', async () => {
    const fs = new FakeDataAdapter();
    await fs.mkdir('.context');
    await fs.write('.context/config', cliConfigText());
    const s = await new ConfigStore(fs).load();
    expect(s.peerUrl).toBe('wss://node.example:7777');
  });

  test('load: canonical config peers[0] overrides the sidecar fallback', async () => {
    const fs = new FakeDataAdapter();
    await fs.mkdir('.context');
    await fs.write('.context/config', cliConfigText()); // peers = node.example
    await fs.write('.context/obsidian.json', JSON.stringify({ peerUrl: 'wss://stale-sidecar:1' }));
    const s = await new ConfigStore(fs).load();
    expect(s.peerUrl).toBe('wss://node.example:7777');
  });

  test('load: empty peers → empty peerUrl', async () => {
    const fs = new FakeDataAdapter();
    await fs.mkdir('.context');
    await fs.write('.context/config', serializeConfig(defaultConfig('v1')));
    const s = await new ConfigStore(fs).load();
    expect(s.peerUrl).toBe('');
  });

  test('save: peerUrl maps to peers=[url]; empty → peers=[]', async () => {
    const fs = new FakeDataAdapter();
    await fs.mkdir('.context');
    await fs.write('.context/config', serializeConfig(defaultConfig('v1')));
    const store = new ConfigStore(fs);
    await store.load();

    await store.save({ ...DEFAULT_SETTINGS, peerUrl: 'wss://set:1' });
    expect(parseConfig(await fs.read('.context/config')).peers).toEqual(['wss://set:1']);

    await store.save({ ...DEFAULT_SETTINGS, peerUrl: '' });
    expect(parseConfig(await fs.read('.context/config')).peers).toEqual([]);
  });
});

describe('ctx-written canonical fields survive a plugin save', () => {
  test('changing peerUrl preserves vault_id / include / debounce_ms', async () => {
    const fs = new FakeDataAdapter();
    await fs.mkdir('.context');
    await fs.write('.context/config', cliConfigText());
    const store = new ConfigStore(fs);

    const s = await store.load();
    expect(s.peerUrl).toBe('wss://node.example:7777');
    s.peerUrl = 'wss://moved:7777';
    // Flip plugin-only knobs too — they must NOT leak into .context/config.
    s.syncEnabled = true;
    s.onboarded = true;
    await store.save(s);

    const cfg = parseConfig(await fs.read('.context/config'));
    expect(cfg.peers).toEqual(['wss://moved:7777']);
    expect(cfg.vault_id).toBe('vault-from-ctx');
    expect(cfg.include).toEqual(['notes/**', 'docs/**']);
    expect(cfg.debounce_ms).toBe(2500);
    // No plugin knob bled into the shared file.
    const raw = await fs.read('.context/config');
    expect(raw).not.toContain('sync_enabled');
    expect(raw).not.toContain('onboarded');
    expect(raw).not.toContain('[obsidian]');
  });
});

describe('plugin-only knobs persist/restore via the sidecar', () => {
  test('save() then a fresh load() round-trips every field', async () => {
    const fs = new FakeDataAdapter();
    await fs.mkdir('.context');
    await fs.write('.context/config', serializeConfig(defaultConfig('vault-x')));
    const store = new ConfigStore(fs);
    await store.load();

    const written: CspSettings = {
      peerUrl: 'wss://peer:7777',
      peerPubkey: 'ssh-ed25519 AAAApin',
      syncEnabled: true,
      autoConnectOnStart: true,
      onboarded: true,
      ignoreGlobs: ['Drafts/**'],
      identityPath: '.context/id_ed25519',
    };
    await store.save(written);

    // Sidecar holds the plugin knobs; config holds the peer projection.
    const side = JSON.parse(await fs.read('.context/obsidian.json'));
    expect(side.syncEnabled).toBe(true);
    expect(side.onboarded).toBe(true);
    expect(side.identityPath).toBe('.context/id_ed25519');
    expect(parseConfig(await fs.read('.context/config')).peers).toEqual(['wss://peer:7777']);

    expect(await new ConfigStore(fs).load()).toEqual(written);
  });

  test('sidecar is minimal — only non-default knobs are written', async () => {
    const fs = new FakeDataAdapter();
    const store = new ConfigStore(fs);
    await store.save({ ...DEFAULT_SETTINGS });
    expect(JSON.parse(await fs.read('.context/obsidian.json'))).toEqual({});

    await store.save({ ...DEFAULT_SETTINGS, syncEnabled: true });
    expect(JSON.parse(await fs.read('.context/obsidian.json'))).toEqual({
      syncEnabled: true,
    });
  });

  test('corrupt sidecar JSON → defaults, does not throw', async () => {
    const fs = new FakeDataAdapter();
    await fs.mkdir('.context');
    await fs.write('.context/obsidian.json', '{not json');
    expect(await new ConfigStore(fs).load()).toEqual(DEFAULT_SETTINGS);
  });
});

describe('ConfigStore — no canonical config minted on save', () => {
  test('save with no .context/config writes only the sidecar', async () => {
    const fs = new FakeDataAdapter();
    const store = new ConfigStore(fs);
    await store.load();

    await store.save({ ...DEFAULT_SETTINGS, peerUrl: 'wss://p:1', syncEnabled: true });

    // The plugin must not invent a vault_id / canonical config.
    expect(await fs.exists('.context/config')).toBe(false);
    // Plugin state (incl. the peerUrl fallback) lives in the sidecar.
    const side = JSON.parse(await fs.read('.context/obsidian.json'));
    expect(side.peerUrl).toBe('wss://p:1');
    expect(side.syncEnabled).toBe(true);
    // And it round-trips back through the sidecar fallback.
    expect((await new ConfigStore(fs).load()).peerUrl).toBe('wss://p:1');
  });

  test('exists() is true once either file is present', async () => {
    const fs = new FakeDataAdapter();
    const store = new ConfigStore(fs);
    expect(await store.exists()).toBe(false);
    await store.save({ ...DEFAULT_SETTINGS, peerUrl: 'wss://p:1' });
    // Only the sidecar exists, but the plugin still has state to load.
    expect(await fs.exists('.context/config')).toBe(false);
    expect(await store.exists()).toBe(true);
  });

  test('exists() is true when only ctx wrote .context/config', async () => {
    const fs = new FakeDataAdapter();
    await fs.mkdir('.context');
    await fs.write('.context/config', cliConfigText());
    expect(await new ConfigStore(fs).exists()).toBe(true);
  });
});

describe('ConfigStore — atomic write', () => {
  test('a write goes through <path>.tmp then rename for both files', async () => {
    const ops: Array<[string, string]> = [];
    class TracingAdapter extends FakeDataAdapter {
      override async write(path: string, data: string): Promise<void> {
        ops.push(['write', path]);
        return super.write(path, data);
      }
      override async rename(oldPath: string, newPath: string): Promise<void> {
        ops.push(['rename', `${oldPath}->${newPath}`]);
        return super.rename(oldPath, newPath);
      }
    }
    const fs = new TracingAdapter();
    await fs.mkdir('.context');
    await fs.write('.context/config', serializeConfig(defaultConfig('v1')));
    ops.length = 0; // ignore fixture setup

    const store = new ConfigStore(fs);
    await store.load();
    await store.save({ ...DEFAULT_SETTINGS, peerUrl: 'wss://x:1', onboarded: true });

    // Sidecar: tmp write then rename.
    expect(ops).toContainEqual(['write', '.context/obsidian.json.tmp']);
    expect(ops).toContainEqual(['rename', '.context/obsidian.json.tmp->.context/obsidian.json']);
    // Canonical config: tmp write then rename.
    expect(ops).toContainEqual(['write', '.context/config.tmp']);
    expect(ops).toContainEqual(['rename', '.context/config.tmp->.context/config']);
    // The final files are correct.
    expect(JSON.parse(await fs.read('.context/obsidian.json')).onboarded).toBe(true);
    expect(parseConfig(await fs.read('.context/config')).peers).toEqual(['wss://x:1']);
  });
});
