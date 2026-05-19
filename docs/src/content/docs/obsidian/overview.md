---
title: Obsidian plugin overview
description: Context for Obsidian — a CSP thin node that keeps an Obsidian vault byte-identical across desktop and mobile.
---

**Context for Obsidian** is a CSP thin node running as an Obsidian plugin on
desktop (Electron) and mobile (Capacitor WebView). It keeps a vault's notes
byte-identical across devices: it holds a complete local working copy, authors
signed primitive commits offline, and converges on reconnect by connecting to
a CSP full node — it never listens itself.

It is built on the [wasm + TypeScript SDK](/sdk/overview/), so it runs the
identical Rust engine and computes its own byte-identical merge.

## What it syncs

- In-scope **text files** by an explicit allowlist (never "everything minus a
  denylist"), respecting the synced `.contextignore` (gitignore syntax).
- `.context/` is excluded unconditionally — the plugin never touches, reads,
  or exposes engine internals.
- Path filtering is configurable per vault. Binaries are excluded unless
  explicitly opted in as whole-file last-writer-wins (out of scope for v1).
- On mobile, retention is bounded by a horizon; deeper history is fetched on
  demand from the full node, and named snapshots stay durable.

## Setup

1. Install the plugin and enable it in Obsidian.
2. In settings, choose either:
   - **Connect to a peer** — paste the full node's `wss://` (or `ws://`)
     address; the plugin runs clone + watch.
   - **Create a local vault** — engine init here (with a warning that without
     a full node there is no multi-device convergence).
3. **Authorize the device** — the plugin shows this device's OpenSSH public
   key; the full-node operator runs `ctx authorize <pubkey>`.
4. Once the handshake and first catch-up succeed, the vault syncs continuously
   and the plugin reflects engine-reported connectivity (idle / connecting /
   connected / reconnecting / error).

## Security posture

The plugin inherits CSP's security model with no weakening: per-author
ed25519 signatures, node-local authorization, and the same mutual-auth
handshake as every other surface. See the [protocol overview](/protocol/overview/)
for the trust model.

## Full specification

The complete plugin design — module architecture, the two-way materialization
and no-feedback-loop handling, snapshots, and the cross-surface parity tests —
is in the [plugin specification](/obsidian/spec/).
