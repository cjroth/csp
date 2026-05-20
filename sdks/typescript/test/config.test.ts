import { describe, expect, test } from 'bun:test';
import { defaultConfig, parseConfig, serializeConfig } from '../src/config.js';
import type { VaultConfig } from '../src/types.js';

// `.context/config` text as the native `ctx` codec emits it (the shared
// flat `VaultConfig` schema; the codec lives once in Rust — these tests
// drive it through the wasm bridge, never a TS reimplementation).
const CTX_SAMPLE = `vault_id = "3ba23523-b267-447d-b842-e037fa12fed7"
name = "my vault"
peers = ["wss://node.example:7777"]
listen = "0.0.0.0:7777"
no_tofu = true
no_tls = false
log = "debug"
debounce_ms = 500
allow_binary = false
include = [
    "**",
]
`;

describe('parseConfig (wasm-backed)', () => {
  test('parses a ctx-written config onto the typed schema', () => {
    const cfg = parseConfig(CTX_SAMPLE);
    expect(cfg.vault_id).toBe('3ba23523-b267-447d-b842-e037fa12fed7');
    expect(cfg.name).toBe('my vault');
    expect(cfg.peers).toEqual(['wss://node.example:7777']);
    expect(cfg.listen).toBe('0.0.0.0:7777');
    expect(cfg.no_tofu).toBe(true);
    expect(cfg.no_tls).toBe(false);
    expect(cfg.log).toBe('debug');
    expect(cfg.debounce_ms).toBe(500);
    expect(cfg.allow_binary).toBe(false);
    expect(cfg.include).toEqual(['**']);
  });

  test('applies serde-equivalent defaults for a minimal file', () => {
    const cfg = parseConfig('vault_id = "x"\n');
    expect(cfg).toEqual(defaultConfig('x'));
  });

  test('throws when the required vault_id is missing', () => {
    expect(() => parseConfig('name = "x"\n')).toThrow();
  });

  test('tolerates comments, blank lines, CRLF and an ignored table header', () => {
    const cfg = parseConfig(
      '# hello\r\n\r\n[ignored]\r\nvault_id = "y" # trailing\r\nno_tofu = true\r\n',
    );
    expect(cfg.vault_id).toBe('y');
    expect(cfg.no_tofu).toBe(true);
  });
});

describe('serializeConfig (wasm-backed)', () => {
  test('round-trips through the one shared codec', () => {
    const cfg: VaultConfig = {
      vault_id: 'id',
      name: 'n',
      peers: ['p1', 'p2'],
      listen: 'ls',
      no_tofu: true,
      no_tls: false,
      log: 'info',
      debounce_ms: 250,
      allow_binary: false,
      include: ['**'],
      auth_keys: [],
      default_key_ttl_days: null,
    };
    expect(parseConfig(serializeConfig(cfg))).toEqual(cfg);
  });

  test('omits listen/log when null (matching serde/toml semantics)', () => {
    const out = serializeConfig(defaultConfig('v-1'));
    expect(out).toContain('vault_id = "v-1"');
    expect(out).not.toContain('listen =');
    expect(out).not.toContain('log =');
    expect(parseConfig(out)).toEqual(defaultConfig('v-1'));
  });

  test('preserves escape-relevant strings across a round-trip', () => {
    const cfg = defaultConfig('v');
    cfg.name = 'quote"and\\slash\ttab';
    cfg.peers = ['ünïcøde-✓', 'a#b'];
    expect(parseConfig(serializeConfig(cfg))).toEqual(cfg);
  });
});
