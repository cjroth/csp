import { describe, expect, test } from 'bun:test';
import {
  applyConfigToDoc,
  configFromDoc,
  defaultConfig,
  parseConfig,
  parseTomlDoc,
  serializeConfig,
  stringifyTomlDoc,
} from '../src/config.js';

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

describe('configFromDoc', () => {
  test('maps a ctx-written config onto the typed schema', () => {
    const cfg = configFromDoc(parseTomlDoc(CLI_SAMPLE));
    expect(cfg.peer.url).toBe('wss://node.example:7777');
    expect(cfg.peer.pubkey).toBe('ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBexample');
    expect(cfg.identity.path).toBe('.context/id_ed25519');
    expect(cfg.scope.extensions).toEqual(['md', 'markdown']);
    expect(cfg.scope.include).toEqual([]);
  });

  test('empty doc → defaults', () => {
    expect(configFromDoc(new Map())).toEqual(defaultConfig());
  });
});

describe('lossless round-trip', () => {
  test('unknown ctx tables/keys survive a plugin write', () => {
    const base = parseTomlDoc(`${CLI_SAMPLE}\n[future]\nx = "keep"\n`);
    const cfg = configFromDoc(base);
    cfg.peer.url = 'wss://changed:7777';
    const out = serializeConfig(cfg, base);
    expect(out).toContain('url = "wss://changed:7777"');
    expect(out).toContain('[future]');
    expect(out).toContain('x = "keep"');
    expect(out).toContain('path = ".context/id_ed25519"');
  });

  test('drops empty optional keys', () => {
    const out = stringifyTomlDoc(applyConfigToDoc(defaultConfig()));
    expect(out).not.toContain('url =');
    expect(out).not.toContain('pubkey =');
    expect(out).toContain('[scope]');
  });

  test('parseConfig returns typed config + raw doc', () => {
    const { config, doc } = parseConfig(CLI_SAMPLE);
    expect(config.peer.url).toBe('wss://node.example:7777');
    expect(doc.get('peer')?.get('url')).toBe('wss://node.example:7777');
  });
});
