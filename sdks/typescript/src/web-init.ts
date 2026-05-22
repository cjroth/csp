// Explicit-init entry — the surface a thin-node host (the Obsidian plugin)
// imports as `@csp/sdk/web-init`. Backed by the **real one Rust engine** via
// wasm (`csp_core::MemEngine` + the shared sans-IO `Session`, §16): the
// plugin computes its own byte-identical `main` exactly like `ctx`. The
// in-memory mock remains exported for the SDK's own unit tests only.

import { initEngine } from '#engine';
import { Identity as RealIdentityNS, Pubkey as RealPubkeyNS } from './identity-real.js';
import { MemoryStorage, memoryStorage } from './mock/memory-storage.js';
import { RealVault } from './real-vault.js';
import type { CloneOptions, CreateOptions, OpenOptions } from './vault.js';

// The wasm module is loaded synchronously by `./wasm.js` (wasm-pack `nodejs`
// glue via `createRequire`) at import time, so the engine is ready as soon
// as this module is imported. `initCsp` is kept for API parity with the
// browser `web` target (where wasm instantiation is async) and is a no-op
// here. Accepts anything the wasm-bindgen `web` target would accept.
type WasmInput = Uint8Array | ArrayBuffer | Response | URL | WebAssembly.Module | string;

let ready = false;

/** Initialize the engine. Idempotent. Node/Bun: a no-op (the nodejs glue is
 * loaded synchronously by `require` at import). Browser/WebView: instantiate
 * the wasm module from the host-inlined `input` bytes (the `#engine` imports
 * map selects the right glue per runtime). */
export async function initCsp(input?: WasmInput): Promise<void> {
  await initEngine(input);
  ready = true;
}
export function isInitialized(): boolean {
  return ready;
}

/** High-level CSP thin-node vault factory — the real engine. */
export const Vault = {
  create(opts: CreateOptions) {
    return RealVault.create(opts);
  },
  open(opts: OpenOptions) {
    return RealVault.open(opts);
  },
  /** `ctx clone <url>` (§17): probe the peer for its vault id + key, build
   * the engine for that vault, trust the peer's key. Per CSP §5.1 the host
   * MUST fork a fresh NodeId or warn rather than resume a possibly-live key. */
  clone(opts: CloneOptions) {
    return RealVault.clone(opts);
  },
};

// Real device identity over the engine (CSP §10).
export const Identity = RealIdentityNS;
export const Pubkey = RealPubkeyNS;

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
  FileMeta,
  ReconnectOptions,
  Snapshot,
  StorageAdapter,
  TransportAdapter,
  TransportConn,
  VaultConfig,
  VaultEvent,
  VaultOptions,
} from './types.js';

export { MemoryStorage, memoryStorage };
export { defaultTransport, makeWebSocketTransport } from './transport-ws.js';

export { defaultConfig, parseConfig, serializeConfig } from './config.js';

export { formatCspIdentity, formatPubkeySidecar, parseCspIdentity } from './identity-file.js';

// ---- Engine Web Worker (issue 0010) — run the wasm engine off the host's
// renderer thread. `EngineWorkerHost` runs inside the Worker; `WorkerVault`
// is the main-thread `Vault` proxy; the `Port` helpers wire the channel. ----
export { EngineWorkerHost } from './worker/engine-host.js';
export { WorkerVault } from './worker/worker-vault.js';
export { workerPort, selfPort, linkedPorts } from './worker/channel.js';
export type { Port } from './worker/channel.js';
export type { EngineWorkerHostOptions } from './worker/engine-host.js';
export type { InitPayload, InitMode, ToWorker, FromWorker } from './worker/protocol.js';

// ---- Test-only: the in-memory mock (used by the SDK's own unit tests and
// available to host plugins for offline UI tests). NOT the production path. ----
export { MockVault } from './mock/vault.js';
export { _resetBroker, memoryTransportPair } from './mock/broker.js';
export { MockIdentity, MockPubkey } from './mock/identity.js';

/** @internal — test reset hook (no-op for the real engine; kept for the
 * mock tests). */
export function _resetForTests(): void {
  ready = true;
}
