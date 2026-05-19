---
title: Desktop app overview
description: Context Desktop — a Tauri app that runs a full CSP node and syncs context folders in the background.
---

**Context Desktop** is a Tauri application (Rust backend linking `csp-core`
directly, with a React / TypeScript / Tailwind frontend) that runs as a full
CSP node. It appears as a normal application window plus a menu-bar tray icon,
syncs one or more local folders in the background, and lets peers connect to
folders via per-folder listeners with copyable `wss://` addresses. macOS is
the v1 target on a cross-platform architecture.

Because it is a **full node**, it retains complete history and is one of the
tiers that can listen and relay — a natural rendezvous point for thin nodes
like the [Obsidian plugin](/obsidian/overview/).

## Key capabilities

- **Add or clone folders** — init a new local folder, or clone from a remote
  peer (catch-up, then immediate watch).
- **Per-folder sync toggle** — turn sync on/off; see engine status (peers
  connected, `main` short SHA, last activity).
- **Allow connections** — bind a per-folder listener with an auto-assigned
  port, show the LAN address, with honest per-OS firewall guidance.
- **Identity management** — view / copy the device SSH key, reuse a `~/.ssh`
  key or agent, opt into a per-vault key.
- **Authorize / revoke peers** — a native trust-on-first-use prompt on the
  first untrusted connection.
- **Snapshots & recovery** — create and restore exact named snapshots
  (skew-free) or best-effort time-based snapshots with a clock-skew warning.
- **Defaults & behavior** — listener port strategy, bind scope, TOFU and TLS
  expectations, start-at-login, notifications, log level.

The app surfaces CSP's merge outcome but has **no conflict-resolution UI** —
merges are deterministic and automatic by design.

## Security posture

Context Desktop inherits CSP's security model with no weakening (per-author
signatures, node-local authorization, mutual-auth handshake). See the
[protocol overview](/protocol/overview/).

## Full specification

The complete app design — the Tauri process/engine model, UI surfaces,
per-folder listener flow, and the UI-action → engine-operation mapping — is in
the [app specification](/desktop/spec/).
