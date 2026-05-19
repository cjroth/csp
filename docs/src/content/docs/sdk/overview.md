---
title: SDK · wasm + TypeScript
description: The same Rust engine compiled to WebAssembly, with a thin TypeScript API for embedding CSP in apps and plugins.
---

The SDK (`@csp/sdk`) is a **thin TypeScript shim over the one Rust engine**
compiled to WebAssembly. It is not a reimplementation: every
sync/merge/fold/identity/auth behavior delegates to the same `csp-core` the
native `ctx` CLI uses, so a host computes its own **byte-identical `main`
fold** exactly like `ctx`. The TypeScript layer only owns transport
(WebSocket) and storage (object store + state persistence); the engine drives
the handshake, anti-entropy, and deterministic merge.

## Public API

The main entry point is `@csp/sdk` (or `@csp/sdk/web-init` for explicit
browser initialization).

**Vault factory (async)**

- `Vault.create(opts)` — initialize a new vault (generates a NodeId, persists
  state via `storage`)
- `Vault.open(opts)` — restore an existing vault from persisted state
- `Vault.clone(opts)` — join a peer vault via `peerUrl`

**File operations (offline-first)** — `writeTextFile`, `readTextFile`,
`fileExists`, `deleteFile`, `renameFile`, `listFiles`. File ops never require
a connection.

**Snapshots / PITR** — `createSnapshot`, `listSnapshots`,
`restoreToSnapshot`, `restoreToTime`.

**Connection (outbound-only)** — `connectWithReconnect()` drives the session
loop with exponential backoff; `disconnect()`, `close()`. With no `peerUrl`
the vault stays local-only.

**Events & identity** — `subscribe(listener)` for
`connecting`/`connected`/`disconnected`/`tree-changed`/`error`;
`identityPubkeySsh()` returns the device key in OpenSSH format for pasting
into a full node's `authorized_keys`; `isConnected()`.

**Identity, config & wire codecs** — `Identity` / `Pubkey`,
`parseConfig` / `serializeConfig` (shared TOML codec with native `ctx`), and
the low-level `wireEncode` / `wireDecode` and conformance helpers
(`blobOid`, `buildPrimitiveObject`, `verifyPrimitiveObject`).

Host-provided interfaces: `TransportAdapter` (a WebSocket-like binary channel)
and `StorageAdapter` (object store + state/snapshot persistence). Test doubles
(`MockVault`, `memoryTransportPair`, `memoryStorage`) are exported for
offline tests.

## Install & build

```sh
npm install @csp/sdk      # or: bun add @csp/sdk

bun run build:wasm        # or: npm run build:wasm
```

`build:wasm` runs `wasm-pack` twice — `--target nodejs` → `./pkg/` and
`--target web` → `./pkg-web/` — both with the size profile, plus an optional
`wasm-opt -Oz` pass. One byte-identical core, two wasm binaries. Other
scripts: `test` (config round-trips, conformance vectors, real-engine offline,
and `ctx`-parity e2e), `typecheck`, `lint`.

## The wasm boundary

`csp-wasm` exposes a high-level `WasmEngine` (the real full engine — file
I/O, the session loop, snapshots) plus a low-level conformance surface
(`blob_oid`, `build_primitive_object`, `verify_primitive_object`,
`wire_encode`/`wire_decode`). The wasm `WasmEngine` computes its own `main`
fold via the *same* `csp_core` merge as the native binary; the
[design specification](/protocol/spec/) makes the byte-identical /
cross-surface parity guarantee, exercised by conformance vectors and a live
`ctx`-parity end-to-end test.

## Gotchas

- **Browser vs Node/Bun.** Under Node/Bun, `initCsp()` is a no-op (wasm loads
  synchronously at import). In a browser/WebView you must
  `await initCsp(wasmBytes)` first — use the `./web-init` export.
- **Outbound-only.** A thin node never listens; it needs a `TransportAdapter`
  and a reachable full node to converge. Disconnection is not an error.
- **Sans-IO engine.** The engine binds no socket; the host carries inbound
  frames into the engine in a loop.
- **Signing is internal.** Primitives are signed inside the engine's session;
  the host only manages the 32-byte seed and reads the public key.
- **Storage is mandatory.** Every `create`/`open`/`clone` needs a
  `StorageAdapter`; the engine persists opaque state, never JSON.
