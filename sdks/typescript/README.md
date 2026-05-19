# @csp/sdk

TypeScript SDK for the Context Sync Protocol — a **thin shim over the one
Rust engine** (`csp-core`) compiled to wasm. See `../../spec.md` §16 ("one
core, thin bindings"). It is **not** a reimplementation: every
sync/merge/fold/identity/auth behavior is a call into the same `csp-core`
that `ctx` uses, so a host (the Obsidian plugin) computes its own
**byte-identical `main`**.

## What it is

- **`Vault`** (`src/real-vault.ts`) — the thin-node session: owns a
  file-level working map, drives `engine.commit_from_files` /
  `materialize_plan` (§5.6) and the shared sans-IO `Session`
  (`session_start`/`session_feed`) over an injected WebSocket transport, and
  persists `engine.to_bytes()` via the host `StorageAdapter`. No protocol
  logic in TS.
- **`WasmEngine`** — the real `csp_core::MemEngine` + `Session` via wasm.
  Two wasm-pack targets, one byte-identical core, selected by the package
  `#engine` imports map:
  - `pkg/` (nodejs) — Node/Bun: SDK tests, the §18 `ctx`-parity e2e,
    Obsidian desktop/Electron.
  - `pkg-web/` (web) — browser/WebView: the Obsidian mobile bundle (esbuild
    inlines `pkg-web/csp_wasm_bg.wasm`; `initCsp(bytes)` instantiates it).
- **Config / identity-file codecs** — the lossless `.context/config` TOML
  and the identity-file format, shared with `ctx`.
- **`src/mock/`** — an in-memory `MockVault` double, exported **for tests
  only** (offline UI tests / plugin unit tests). NOT the production path.

## Verified

`bun run build:wasm` builds both wasm targets, then:

- `bun test test` — config/identity round-trips, the cross-surface
  conformance vectors (`test/interop.test.ts` vs `test-vectors.json`: the
  wasm output is byte-identical to native), the real engine offline, and
  **`test/e2e/ctx-parity.test.ts`** — spawns the real `ctx` binary as a
  listener and proves the SDK `Vault` converges with it **bidirectionally**
  over a real WebSocket (the §18 truth oracle).

## Honest residual

`build:wasm` uses `wasm-pack --dev` (fast, large). A release build
(`--release` + `wasm-opt -Oz`) shrinks the inlined wasm substantially and is
the packaging follow-up; functionally the engine is complete and proven.
