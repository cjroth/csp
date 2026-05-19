// `.context/config` typed accessors. **No TOML logic lives here.** The
// codec is implemented exactly once, in Rust (`csp_core::config`), and this
// module is a thin typed bridge over that same codec compiled to wasm — the
// identical bytes `ctx` reads/writes (one engine everywhere; a second TOML
// implementation in TS would be a drift risk).
//
// PROVISIONAL: the canonical `.context/config` schema is CSP-owned and not
// yet frozen. `VaultConfig` mirrors the Rust struct field-for-field; only
// that projection changes when CSP fixes the schema.

import { configParse, configToToml } from '#engine';
import type { VaultConfig } from './types.js';

export type { VaultConfig };

/** A fresh config with serde-equivalent defaults for the given vault id
 * (mirrors `csp_core::config::VaultConfig`'s defaults). */
export function defaultConfig(vaultId: string): VaultConfig {
  return {
    vault_id: vaultId,
    name: '',
    peers: [],
    listen: null,
    no_tofu: false,
    no_tls: false,
    log: null,
    debounce_ms: 1000,
    allow_binary: false,
    include: ['**'],
  };
}

/** Parse `.context/config` text into the typed schema via the one Rust
 * codec (wasm). Applies the same defaults serde does; `vault_id` is
 * required. Throws on input the codec can't represent. */
export function parseConfig(text: string): VaultConfig {
  return JSON.parse(configParse(text)) as VaultConfig;
}

/** Serialize the typed schema back to `.context/config` text via the one
 * Rust codec (wasm) — valid TOML `ctx` and the real `toml` parser read back
 * identically. */
export function serializeConfig(cfg: VaultConfig): string {
  return configToToml(JSON.stringify(cfg));
}
