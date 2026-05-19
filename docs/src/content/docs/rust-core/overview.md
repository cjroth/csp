---
title: Rust core · csp-core
description: The single CSP engine — object store, identity and signing, the deterministic fold/merge, and sync state. Every other surface is a thin binding over it.
---

`csp-core` is the **entire CSP engine**. The `ctx` CLI, the TypeScript SDK,
the Obsidian plugin, and the desktop app are all thin bindings — every
protocol, merge, and convergence decision lives here, nowhere else. New
capabilities land in Rust once and surface everywhere; behavior parity is
structural, not hand-maintained.

## What lives in the core

- **Merge / fold algorithm** — the deterministic binary left-fold over
  frontier tips, sorted by the strict total order `(counter, NodeId, SHA)`,
  with per-step `git-merge-base` + 3-way merge and byte-pinned synthetic fold
  commits (`fold.rs`, `merge.rs`, `order.rs`).
- **Object store & commit DAG** — `gitoxide`-backed, real stock-git object
  encoding and repository layout at `<scope>/.context/git/`
  (`object.rs`, `oid.rs`, `store.rs`, `repo.rs`).
- **Identity & auth** — ed25519 keypair derivation, signature
  generation/verification, handshake transcript binding, node-local
  authorization checks (`identity.rs`).
- **Sync protocol** — a sans-IO session state machine: frontier-set
  anti-entropy catch-up, per-author signature verification, deterministic fold
  recomputation for verification (`session.rs`, `net.rs`, `wire.rs`).
- **Scope & ignore** — allowlist filtering, `.contextignore` (gitignore
  syntax) parsing, path reconciliation (`scope.rs`).
- **Materialization** — content-hash reconciliation, atomic writes,
  last-materialized tracking, and the no-clobber rule that prevents a
  materialize→watch feedback loop (`vault.rs`, `state.rs`).
- **Config & engine** — the TOML config codec shared with every surface and
  the in-memory engine that drives it all (`config.rs`, `engine.rs`).

## Feature-gated profiles

The engine compiles to two profiles from one codebase:

- **Native** — adds the on-disk object database / packfiles, the tokio socket
  driver, and TLS provisioning. Full nodes only.
- **`wasm32`** — the sync protocol state machine, identity/auth, framing, the
  object encode/decode, *and the full deterministic merge*. This is why a
  browser or plugin thin node computes the same byte-identical `main` as the
  native CLI.

Only genuinely platform-bound pieces stay native-only; the merge is **not**
one of them.

## Source layout

The crate is at `crates/csp-core/`. Key modules: `engine.rs`, `fold.rs`,
`merge.rs`, `order.rs`, `session.rs`, `net.rs`, `vault.rs`, `scope.rs`,
`identity.rs`, `object.rs`, `store.rs`, `repo.rs`, `state.rs`, `config.rs`,
`wire.rs`. The deterministic fold and its conformance guarantees are specified
in the [protocol overview](/protocol/overview/) and the
[design specification](/protocol/spec/).
