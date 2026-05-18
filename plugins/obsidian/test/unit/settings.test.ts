import { describe, expect, test } from 'bun:test';
import { parseTomlDoc, stringifyTomlDoc } from '@csp/sdk/web-init';
import {
  ConfigStore,
  type CspSettings,
  DEFAULT_SETTINGS,
  parseIgnoreGlobs,
  settingsFromTomlDoc,
  writeSettingsToTomlDoc,
} from '../../src/settings.js';
import { FakeDataAdapter } from '../mocks/obsidian.js';

const CLI_SAMPLE = `[peer]
url = "wss://node.example:7777"
pubkey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBexample"

[identity]
path = ".context/id_ed25519"

[scope]
extensions = [
    "md",
    "markdown",
]
include = []
`;

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

describe('settingsFromTomlDoc', () => {
  test('maps a ctx-written config onto the settings view', () => {
    const s = settingsFromTomlDoc(parseTomlDoc(CLI_SAMPLE));
    expect(s.peerUrl).toBe('wss://node.example:7777');
    expect(s.peerPubkey).toBe('ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBexample');
    expect(s.identityPath).toBe('.context/id_ed25519');
    expect(s.autoConnectOnStart).toBe(false);
    expect(s.ignoreGlobs).toEqual([]);
  });

  test('empty doc → defaults', () => {
    expect(settingsFromTomlDoc(new Map())).toEqual(DEFAULT_SETTINGS);
  });

  test('reads plugin-only [obsidian] knobs', () => {
    const doc = parseTomlDoc(
      '[obsidian]\nsync_enabled = true\nauto_connect = true\nignore_globs = ["Drafts/**", "*.tmp.md"]\n',
    );
    const s = settingsFromTomlDoc(doc);
    expect(s.syncEnabled).toBe(true);
    expect(s.autoConnectOnStart).toBe(true);
    expect(s.ignoreGlobs).toEqual(['Drafts/**', '*.tmp.md']);
  });
});

describe('writeSettingsToTomlDoc', () => {
  test('round-trips through the settings view', () => {
    const base = parseTomlDoc(CLI_SAMPLE);
    const s = settingsFromTomlDoc(base);
    s.peerUrl = 'wss://changed:7777';
    s.autoConnectOnStart = true;
    s.ignoreGlobs = ['Z/**'];
    const out = stringifyTomlDoc(writeSettingsToTomlDoc(s, base));
    const back = settingsFromTomlDoc(parseTomlDoc(out));
    expect(back.peerUrl).toBe('wss://changed:7777');
    expect(back.autoConnectOnStart).toBe(true);
    expect(back.ignoreGlobs).toEqual(['Z/**']);
    // ctx-managed scope fields untouched.
    expect(out).toContain('extensions = [');
  });

  test('drops empty peer keys and the [obsidian] table when unused', () => {
    const out = stringifyTomlDoc(writeSettingsToTomlDoc({ ...DEFAULT_SETTINGS }));
    expect(out).not.toContain('url =');
    expect(out).not.toContain('pubkey =');
    expect(out).not.toContain('[obsidian]');
  });

  test('preserves unknown ctx tables/keys', () => {
    const base = parseTomlDoc(`${CLI_SAMPLE}\n[future]\nx = "keep"\n`);
    const s = settingsFromTomlDoc(base);
    s.peerUrl = 'wss://moved:7777';
    const out = stringifyTomlDoc(writeSettingsToTomlDoc(s, base));
    expect(out).toContain('url = "wss://moved:7777"');
    expect(out).toContain('[future]');
    expect(out).toContain('x = "keep"');
  });
});

describe('ConfigStore', () => {
  test('load() returns defaults when .context/config is absent', async () => {
    const store = new ConfigStore(new FakeDataAdapter());
    expect(await store.load()).toEqual(DEFAULT_SETTINGS);
  });

  test('exists() reflects whether .context/config is present', async () => {
    const fs = new FakeDataAdapter();
    const store = new ConfigStore(fs);
    expect(await store.exists()).toBe(false);
    await store.save({ ...DEFAULT_SETTINGS, peerUrl: 'wss://p:1' });
    expect(await store.exists()).toBe(true);
  });

  test('save() then load() round-trips and writes .context/config', async () => {
    const fs = new FakeDataAdapter();
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
    expect(await fs.exists('.context/config')).toBe(true);
    const text = await fs.read('.context/config');
    expect(text).toContain('[peer]');
    expect(text).toContain('url = "wss://peer:7777"');
    expect(text).toContain('path = ".context/id_ed25519"');
    expect(text).toContain('sync_enabled = true');
    expect(text).toContain('onboarded = true');
    expect(text).toContain('[obsidian]');

    expect(await new ConfigStore(fs).load()).toEqual(written);
  });

  test('save() preserves a .context/config the ctx CLI wrote', async () => {
    const fs = new FakeDataAdapter();
    await fs.mkdir('.context');
    await fs.write('.context/config', CLI_SAMPLE);
    const store = new ConfigStore(fs);
    const s = await store.load();
    s.peerUrl = 'wss://moved:7777';
    await store.save(s);
    const text = await fs.read('.context/config');
    expect(text).toContain('url = "wss://moved:7777"');
    expect(text).toContain('path = ".context/id_ed25519"'); // ctx field survives
    expect(text).toContain('extensions = [');
  });
});
