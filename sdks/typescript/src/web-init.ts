// Explicit-init entry — the surface a thin-node host (the Obsidian plugin)
// imports as `@csp/sdk/web-init`, mirroring how the agentsync plugin
// imported `@agentsync/sdk/web-init`. The host inlines the wasm bytes and
// passes them to `initCsp()` once at startup.
//
// TODAY this is backed by the in-memory mock in `./mock/` (CSP spec.md
// §13.2 / obsidian-plugin-spec §14 — `csp-wasm` is a residual gate). When
// the real reduced wasm surface (CSP §4/§7) exists, swap the bindings here;
// the host, its tests, and its module boundaries do not change ("one core,
// thin bindings", CSP §16).

import { MockIdentity, MockPubkey } from './mock/identity.js';
import { MockVault } from './mock/vault.js';
import type {
  CloneOptions,
  CreateOptions,
  Identity as IdentityContract,
  OpenOptions,
  Pubkey as PubkeyContract,
} from './vault.js';

// Accepts anything the real wasm-bindgen `web` target would accept; the
// mock ignores the bytes but the host's inline-wasm path stays real so no
// host code changes when csp-wasm lands.
type WasmInput =
  | Uint8Array
  | ArrayBuffer
  | Response
  | URL
  | WebAssembly.Module
  | string
  | null
  | undefined;

let ready = false;

function assertReady(): void {
  if (!ready) {
    throw new Error('csp: not initialized — call `await initCsp(wasmBytes)` first');
  }
}

/** Load and initialize the engine. Idempotent. (Mock: marks ready; the
 * real impl instantiates the wasm module from `input`.) */
export async function initCsp(_input?: WasmInput): Promise<void> {
  ready = true;
}

/** True once the engine is initialized. */
export function isInitialized(): boolean {
  return ready;
}

/** High-level CSP thin-node vault factory. */
export const Vault = {
  create(opts: CreateOptions) {
    assertReady();
    return MockVault.create(opts);
  },
  open(opts: OpenOptions) {
    assertReady();
    return MockVault.open(opts);
  },
  /** Bootstrap from a peer (a full node in listen mode) — CSP §17
   * `ctx clone <url>`. Per CSP §5.1 the caller MUST fork a fresh NodeId or
   * warn rather than resume a possibly-live key. */
  clone(opts: CloneOptions) {
    assertReady();
    return MockVault.clone(opts);
  },
};

// ---- Lazy primitive accessors (parity with the future wasm surface) ----

export const Identity = {
  generate(): IdentityContract {
    assertReady();
    return MockIdentity.generate();
  },
  fromSeed(seed: Uint8Array): IdentityContract {
    assertReady();
    return MockIdentity.fromSeed(seed);
  },
};

export const Pubkey = {
  fromBytes(bytes: Uint8Array): PubkeyContract {
    assertReady();
    return MockPubkey.fromBytes(bytes);
  },
  fromSshString(s: string): PubkeyContract {
    assertReady();
    return MockPubkey.fromSshString(s);
  },
};

// ---- Re-exports ----

export type {
  CloneOptions,
  CreateOptions,
  Identity as IdentityInstance,
  OpenOptions,
  Pubkey as PubkeyInstance,
  Vault as VaultInstance,
} from './vault.js';

export type {
  CspConfig,
  FileMeta,
  IdentitySection,
  PeerSection,
  ReconnectOptions,
  ScopeSection,
  Snapshot,
  StorageAdapter,
  TomlDoc,
  TomlValue,
  TransportAdapter,
  TransportConn,
  VaultEvent,
  VaultOptions,
} from './types.js';

export { MemoryStorage, memoryStorage } from './mock/memory-storage.js';
export { _resetBroker, memoryTransportPair } from './mock/broker.js';

export {
  applyConfigToDoc,
  configFromDoc,
  defaultConfig,
  defaultScopeSection,
  parseConfig,
  parseTomlDoc,
  serializeConfig,
  stringifyTomlDoc,
} from './config.js';

export { formatCspIdentity, formatPubkeySidecar, parseCspIdentity } from './identity-file.js';

/** @internal — exposed only for tests. Resets module-level init state. */
export function _resetForTests(): void {
  ready = false;
}
