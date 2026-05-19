---
title: Protocol overview
description: What the Context Sync Protocol guarantees — files on disk, deterministic merge, stock-git-compatible history, decentralized topology.
---

The Context Sync Protocol keeps a designated set of files **byte-identical
across many devices**, in near real time, while every full participant retains
a complete, **stock-git-compatible** history with fully local point-in-time
recovery.

:::caution[Status]
The specification is a draft / greenfield design. The reference implementation
ships as a single CLI, `ctx`.
:::

## Core model

- **Scope and vault.** CSP manages an explicit allowlist subtree — the
  *scope* — with a synced `.contextignore` exclude file. One vault per scope.
  The synced set is text-only by default; opted-in binaries are whole-file,
  last-writer-wins.
- **Files on disk.** Synced data is materialized as plain files at their
  natural paths. Any tool reads and edits them with no adapter. CSP records
  the content hash it last materialized so it can tell its own writes apart
  from genuine user edits and avoid a feedback loop.
- **Stock-git-compatible history, not at `.git`.** The history is a real git
  repository at `<scope>/.context/git/` with a decoupled worktree. The object
  encoding, layout, and refs are exactly what unmodified git expects — but CSP
  **never plants a `.git` at the scope root**, so it coexists with a project's
  own git repo. The repository is engine-owned; reads via `ctx git …` are a
  read-only passthrough, out-of-band writes are unsupported.
- **Engine.** `gitoxide` (`gix`), pure Rust, is the only git engine — no `git`
  binary dependency. **Hash is SHA-1 (sha1dc)** for maximum stock-git
  compatibility; it is not the security boundary (node-key auth is), and
  SHA-256 is explicitly out of scope for v1.

## Sync & merge guarantees

- **Strict total order.** Every commit is ordered by the tuple
  `(counter, NodeId, commitSHA)` — a monotonic per-node `u64` counter, then
  the ed25519-derived NodeId, then the commit SHA as the final tiebreaker.
  This is a *true total order with no ties ever* and is the sole basis for
  merge order. It never depends on a wall clock.
- **Deterministic 3-way merge (the fold).** `main` is built by a binary
  left-fold over the frontier tips, sorted by the strict total order. Edits to
  different regions of a file both survive; a true overlapping conflict
  resolves to the operand later in the order. Synthetic fold commits are
  byte-pinned and verified by recomputation.
- **No conflict markers, ever.** Same-region co-editing is a non-goal: one
  side deterministically wins and the losing version stays in history,
  recoverable — never a marker, never a human resolution step.
- **Convergence.** Connected nodes converge within a round trip; disconnected
  nodes converge on reconnect via catch-up. Given the same set of primitives,
  every node computes an identical `main` SHA and working tree.

## Identity, trust & transport

- **ed25519 node keys.** Every node has a stable NodeId from an ed25519
  keypair (device-global by default, so one device can join many vaults).
  Every primitive commit is signed by its author; synthetic fold commits are
  unsigned and verified by recomputation.
- **Per-author authorization.** A listening node admits only peers whose
  public key is in its local, never-synced `authorized_keys`. A primitive is
  accepted only if its author signature verifies *and* the author key is
  locally authorized — regardless of which peer relayed it. Relays confer no
  trust.
- **Trust-on-first-use.** With an empty authorized set, a listener *may* TOFU
  the first connecting key (only while the set is empty; `--no-tofu` disables
  it). An internet-reachable listener with TOFU on and an empty set trusts
  whoever connects first — pre-seed keys for public deployments.
- **Transport.** A symmetric, ordered, message-oriented WebSocket. Default is
  `wss://` with a self-signed cert (TLS adds confidentiality; the ed25519
  mutual-auth handshake is the trust boundary). `--no-tls` serves `ws://`
  behind a fronting proxy or on trusted networks.
- **Topology.** Decentralized and symmetric — any full node may listen and
  relay; there is no privileged server. Any topology (star, mesh, chain,
  gossip) converges to the same `main`.

## Node tiers

The merge engine compiles to wasm, so **every node — including a plugin —
runs the identical engine and computes its own byte-identical `main`**.
Tiering distinguishes only listen/relay capability and retention horizon:

- **Full node** — retains the entire commit DAG in a real on-disk repository,
  offers deep recovery, and is the *only* tier that may listen/relay (inbound
  sockets + on-disk object database are native-only). Desktop, server,
  always-on.
- **Thin node** — same engine, computes its own merge, but keeps a bounded
  history and cannot listen. Mobile, browser/WebView, editor plugins. Deep
  point-in-time recovery is delegated to full nodes.

Every deployment needs at least one listenable full node as a rendezvous
point. Every node still authors and reads entirely offline.

## Full specification

The complete design — including the fold algorithm, replication protocol,
catch-up, storage and compaction, security model, and the cross-surface
conformance suite — is in the [design specification](/protocol/spec/).
