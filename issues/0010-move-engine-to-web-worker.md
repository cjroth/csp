# 0010 — Obsidian sync freezes the UI; move the engine to a Web Worker

**Severity:** High  **Status:** Open  **Owner:** —

## Summary

The wasm engine runs synchronously on Obsidian's renderer (UI) thread.
Every commit calls `engine.commit_from_files(...)` and
`engine.to_bytes()` — synchronous wasm calls — and while they run the
renderer cannot paint. The user-visible symptom is a multi-second
freeze of the Obsidian UI on every sync (a local edit, or the first
catch-up after connect).

Move the engine off the renderer thread into a Web Worker so the UI
stays responsive regardless of how much work a sync involves.

## Why

[[0009]] cuts the *amount* of per-edit work, but any synchronous wasm
call on the renderer thread blocks paint for its duration. On a large
vault, or initial catch-up against a big remote, that is still enough
to stutter. A Worker makes engine cost invisible to the UI no matter
its size — the definitive fix for the freeze.

## Design

Split the plugin across two threads:

- **Worker (`csp-engine.worker.ts`)** — owns the wasm engine and the
  WebSocket transport (Workers can open WebSockets).
- **Main thread** — keeps everything that needs Obsidian APIs: the
  `app.vault` file bridge and the `.context/` storage adapter
  (`app.vault.adapter` is main-thread only).

Message-passing seam:

- **Main → Worker**: file events (`path changed, here are the bytes`),
  lifecycle (`start` / `stop` / `resync`), settings.
- **Worker → Main**: materialize ops (`write/remove file X`), status
  transitions, and **storage requests** — the worker cannot touch
  `.context/` directly, so `StorageAdapter` becomes a proxy: worker
  posts `loadState` / `saveState` / `getObject` / `putObject` → main
  runs the real `ObsidianStorageAdapter` → posts the result back.
- The inlined wasm bytes (`__CSP_WASM_B64__`) are message-passed into
  the worker at startup — no `fetch`, so it stays mobile-WebView-safe
  (the same constraint that drove inlining in the first place).

### Constraints / risks

- Mobile WebView (Capacitor) must run Workers + instantiate wasm inside
  them. Both are supported on the iOS/Android WebView versions the
  plugin already targets, but verify on-device.
- esbuild must emit the worker as a separate bundle (or an inlined
  `Blob` worker) — Obsidian ships a single `main.js`, so the worker
  source likely has to be inlined as a string and started from a
  `Blob` URL.
- Big payloads crossing the boundary (a large `saveState` blob, or
  initial-catch-up file sets) should use transferable `ArrayBuffer`s,
  not structured-clone copies.
- Serialization ordering: the existing bridge serial queue
  (`opQueue`) and commit debounce must be preserved across the
  thread boundary — define the protocol so outbound writes and
  inbound applies still cannot interleave.

## Acceptance

- Editing a file in a large vault does not drop a frame on the
  renderer thread (engine work happens in the worker).
- Initial catch-up against a large remote streams in without freezing
  the UI.
- Desktop (Electron) and mobile (Capacitor WebView) both work —
  verified on-device for mobile.
- Plugin unit + e2e suites green; the worker seam has its own
  message-protocol tests.
- No regression in convergence correctness (the `ctx`-parity path
  still converges).

## Relation to other issues

- [[0009]] — incremental commits. Independent; recommended to land
  first so the worker is moving an already-cheap engine off-thread.
- [[0011]] — incremental persistence. With the storage adapter on the
  main thread, a non-incremental `saveState` posts a multi-MB blob
  across the worker boundary on every commit; 0011 shrinks that to a
  few small objects. Compounding, not blocking.
