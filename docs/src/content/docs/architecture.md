---
title: Architecture
description: How the protocol, Rust core, CLI, SDK, plugin, and desktop app fit together — one engine, thin bindings.
---

CSP has one organizing principle: **one core, thin bindings**. All protocol
behavior — the object model, identity and signing, storage, the deterministic
fold/merge, and sync state — lives in the Rust core exactly once. Every other
surface is a thin adapter that calls into it.

## The layers

| Layer | What it is | Role |
| --- | --- | --- |
| [Protocol](/protocol/overview/) | The design specification (`spec.md`) | Defines the object model, fold/merge, and replication protocol |
| [Rust core](/rust-core/overview/) | `csp-core` crate | The single engine: storage, signing, deterministic merge, sync state |
| [CLI](/cli/overview/) | `ctx` binary | Native full node: init, watch, clone, status, snapshot, restore, inspect |
| [SDK](/sdk/overview/) | `csp-wasm` + `@csp/sdk` | The same engine compiled to wasm with thin TypeScript bindings |
| [Obsidian plugin](/obsidian/overview/) | `plugins/obsidian` | A thin node embedding the SDK in Obsidian (desktop + mobile) |
| [Desktop app](/desktop/overview/) | Tauri app | A full node with a background GUI, linking `csp-core` directly |

## Why this shape

The merge engine itself compiles to `wasm32`, so **every node — including a
browser or plugin thin node — runs the identical Rust merge and computes its
own byte-identical `main`**. Tiering is therefore about *listen/relay
capability and retention horizon*, never about merge capability:

- **Full nodes** (CLI, desktop, server) retain the entire history in a real
  stock-git-compatible repository and are the only tier that can *listen and
  relay* (inbound sockets and the on-disk object database are native-only).
- **Thin nodes** (SDK in a browser, the Obsidian plugin on mobile) run the
  same engine and compute the same merge, but keep a bounded history and only
  make *outbound* connections.

Every deployment needs at least one listenable full node as a rendezvous
point; two browser/WebView thin nodes cannot connect directly. Every node —
full or thin — still authors and reads entirely offline.

Because protocol logic exists in Rust once, behavior parity across surfaces is
**structural, not hand-maintained**. A cross-surface conformance suite proves
the wasm engine and the native `ctx` converge to byte-identical results; see
the [design specification](/protocol/spec/) for the full guarantee.
