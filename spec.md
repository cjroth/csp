# Context Sync Protocol (CSP) — Design Specification

> Status: draft / greenfield design. Nothing here is implemented yet.
> This document supersedes any earlier draft.
> The reference implementation ships as a single CLI, `ctx`.

---

## 1. Summary

CSP keeps a designated set of files **byte-identical across many devices**, in
near real time, while every full participant retains a complete, **stock-git-
compatible** history with fully local point-in-time recovery.

Files live on the real filesystem as ordinary files, so any program reads and
writes them directly with no adapter or special client. The history is a real,
stock-git-compatible repository — but CSP **never plants a `.git` at the scope
root** (so it can coexist with a user's own git repo in the same project). The
repository lives at `<scope>/.context/git/` with a decoupled worktree; `ctx git …`
runs unmodified git against it **read-only** for inspection (the repo is
engine-owned; §4, §17), and every full node sees the *same* history.

There is **no central server**. Every node runs the same engine; any node may
accept connections and relay. Nodes are symmetric peers.

Concurrent edits are merged with **real git three-way merge**: edits to
different regions of a file both survive; a true overlapping conflict is
resolved automatically and deterministically (never with conflict markers,
never by a human). Simultaneous co-editing of the *same region* of one file is
not a supported workflow — one side deterministically wins (and the other
remains in history).

---

## 2. Goals

1. **Files on disk.** Synced data is materialized as plain files at natural
   paths. Any tool reads/edits them without going through CSP.
2. **Near-real-time propagation.** While connected, a change reaches other
   connected devices well under a second. No polling.
3. **Stock-git-compatible history, everywhere.** A full node holds a real
   `.git`. `git log`/`checkout` work with unmodified git and show identical
   history on every full node. Point-in-time recovery is fully local.
4. **Decentralized & symmetric.** No privileged server. Any node may listen and
   relay. Topology is arbitrary.
5. **Automatic, deterministic merge.** Real 3-way merge; non-overlapping
   concurrent edits both survive; overlaps auto-resolved by a fixed total
   order. No markers, no human resolution.
6. **Compact history.** Storing N edits costs ≈ the N distinct contents
   (content-addressed git objects), not N tree copies.
7. **Coexists in a shared directory.** CSP manages an explicit subtree;
   never touches anything outside its scope.
8. **One engine, everywhere.** Pure-Rust core compiled to a single wasm module
   shared by all clients; thin TypeScript only for host file I/O, transport
   plumbing, and UI.

## 3. Non-goals

- Simultaneous co-editing of the same region of one file (one side wins
  deterministically; the loser stays in history but not in the working tree).
- Conflict markers or any human conflict-resolution step.
- Git’s network protocols (smart HTTP/SSH), refs negotiation, or pull/poll
  workflow — replaced entirely by the realtime transport (§6).
- Being a developer VCS workflow tool (branching/PR/rebase UX). Git here is a
  storage + merge + history *substrate*, exercised programmatically.

---

## 4. Engine

- **A pure-Rust, hand-rolled, stock-git-compatible object/ref layer** is the
  only git engine (`sha1` + `flate2` for stock object encoding; minimal
  loose-object/pack/ref handling). Native nodes and the wasm module share
  this one codebase. No `git` binary dependency. Deliberately *not* a
  general git library — not isomorphic-git (JS), not libgit2 (C; will not
  wasm), and not `gitoxide` (kept out so the one engine stays a small wasm
  payload, §16). Stock git only ever *reads* the result (§4 read-only
  passthrough, proven git-coherent by the §18 e2e).
- **On-disk format is real, stock-git-compatible — but not at `.git`.** Object
  encoding, repository layout (`objects/`, `refs/`, `HEAD`, `config`, packs),
  and refs are exactly what unmodified git expects, but the git directory is
  `<scope>/.context/git/` with a decoupled worktree (`GIT_DIR=<scope>/.context/git`,
  `GIT_WORK_TREE=<scope>`) — **never a `.git` at the scope root**. This is what
  lets CSP coexist with a user's own git repo in the same project (§11). It is
  still genuine stock git for *reading*: `ctx git <args>` is a **read-only**
  passthrough (§17). The repository is **engine-owned** — `refs/heads/main`,
  `refs/heads/node/*`, `refs/tags/snap/*` are engine-managed; CSP force-
  recomputes `main`, owns the refs, and GCs. **Out-of-band writes via raw
  `GIT_DIR=… git …` are unsupported and will be clobbered without warning.**
- **Hash: SHA-1.** Plain SHA-1, byte-identical to `sha1dc` on every non-
  colliding input (so determinism and stock-git coherence are unaffected).
  Collision-detection (`sha1dc`) is **intentionally not bundled**: git’s
  hashing is *not* CSP’s security boundary (that is node-key auth + transport
  content verification, §10), so SHA-1’s collision weakness is not load-bearing
  here, and the always-on engine is lean by construction for the wasm payload
  (§16) — adding the UBC tables would cost size for a non-load-bearing
  property. **v1 is SHA-1, period;** SHA-256 is explicitly out of scope for v1
  (a different hash = a different repository — no in-place migration) and
  revisited post-v1 only if ecosystem support warrants.
- **One engine everywhere — the merge engine compiles to wasm too.**
  *(Revised, pre-release: supersedes the earlier "merge is native/full-node
  only" design.)* The deterministic 3-way merge/fold is in the always-on
  surface and compiles to `wasm32`, so **every node — including a plugin —
  runs the identical Rust `compute_main` and computes its own byte-identical
  `main`** (§5.4 holds by construction; the cross-surface conformance suite is
  the headline guarantee). Only genuinely platform-bound pieces stay
  native-only (`cfg`-gated): the on-disk odb/packfiles, the tokio socket
  driver, and TLS provisioning. The only node distinction is therefore
  **whether it can listen/relay — a platform fact, not a configured tier**
  (§7; there is no `tier` knob in v1, and a bounded retention horizon is
  post-v1). A wasm node still cannot *bind a listener* (no server sockets in a
  browser/WebView) — that, not merge, is the only thing it delegates, and it
  is enforced structurally (the listener does not compile to wasm). This is
  proven, not assumed: the SDK (wasm engine) converges with the real `ctx` in
  the §18 parity suite.

---

## 5. History model

### 5.1 Identity, signing, and the strict total order

- Each node has a stable **NodeId** derived from an ed25519 keypair (the same
  key used for transport auth and authorization, §10).
- Each node keeps a monotonic **logical counter** (u64): a new local commit's
  counter = `max(every counter the node has observed) + 1`. The counter is
  **durably persisted in `.context/state`** and survives restart.
- **Strict total order** on commits: `(counter, NodeId, commitSHA)` — compare
  counter, then NodeId bytewise, then commit SHA bytewise. The SHA tiebreaker
  makes this a *true total order with no ties ever* — even if two replicas
  share a NodeId and independently choose the same counter. It is the sole
  basis for fold order and conflict tiebreak; never wall-clock dependent.
- **Every primitive commit is signed** by its author's NodeId key (§5.2); the
  signature is part of the authored-once object. Synthetic fold commits (§5.3)
  are **unsigned** — derived and verified by recomputation, not authored.
- **Single-writer-per-vault (operational invariant, not a correctness
  precondition).** A NodeId SHOULD be an active writer in at most one replica
  of a given vault at a time. Reusing one key across *different* vaults is
  fine; the hazard is the *same key actively writing two replicas of the same
  vault* (same-NodeId concurrent primitives, possibly equal counter).
  Correctness does **not** depend on this — the SHA tiebreaker keeps the order
  total and signed primitives keep authorship sound — but such history is
  semantically confusing, so the counter is durably persisted and `ctx clone`
  / restore MUST fork a fresh NodeId or warn rather than silently resume
  authoring under a key that may be live elsewhere.

### 5.2 Primitive commits and synthetic fold commits

Two object kinds: **primitive commits** (a node's own authored, *signed* edit)
and **synthetic fold commits** (derived, *unsigned*, deterministic — §5.3).
`refs/heads/main` is always a synthetic fold commit.

**Primitive commit** — a node's own change.

- Genesis `M₀` is a deterministic root synthetic fold commit over the empty
  tree (no parents, fixed identity §5.4); globally identical SHA.
- A primitive's git **parent is the synthetic fold commit the author held**
  when authoring — a real commit object, **not** the node's own previous
  primitive. Its tree is the new in-scope working tree.
- It is **signed by the author's NodeId key** and authored exactly once;
  replicated **verbatim** (the signature is in the object, so the SHA is
  stable). Carries author identity + wall-clock time + `(counter, NodeId)`
  trailer (order §5.1; PITR §8).
- Successive edits each branch from the latest synthetic fold commit the node
  holds — **no private linear lineage off a node's own previous primitive**.
  (On an offline thin node that fold commit is the trivial `|F|=1`
  self-wrapper over its own previous tip — §7 — keeping its run linear.)

**Synthetic fold commit** — converged state, a first-class history object.

- Built by §5.3. A real git commit (deterministic identity/time §5.4),
  **always distinct, never collapsed to a primitive**. The *final* synthetic
  fold commit of the fold is `refs/heads/main`.
- **The fold-chain structurally encodes the frontier.** Its parents are
  `[previous synthetic fold commit, next tip]` (§5.3); the primitive tips it
  folded are exactly those reachable through the chain and not below an earlier
  fold commit. The input set is recoverable from the graph — not from any
  trailer.
- **Replicated as ordinary reachable objects, verifiable not trusted.** A
  primitive's parent is a synthetic fold commit; a dangling parent is an
  invalid git DAG; reachable-object replication therefore carries the parent
  fold commit → its parents → … → `M₀`. Any received fold commit is checked by
  recomputing the fold over *its own* parent list and asserting the SHA
  matches (recursively to primitives/`M₀`; §5.4 determinism makes this exact).
  A received synthetic fold commit is recompute-verified **exactly once, on
  receipt** — at integration, recursively to primitives/`M₀` — and then
  **never re-verified during steady-state `main` recomputation**. A node
  recomputes only the **current** `main` for *materialization*; "need not
  recompute [historical fold commits]" therefore means *not re-derived every
  fold*, **not** "accepted untrusted": a fold commit that fails recompute-
  verification on receipt is dropped and not forwarded (§6.3, §13.2).
- `git log main` is identical on every node holding the same primitive set.

### 5.3 The deterministic fold (the central algorithm)

> The single most important algorithm in CSP. Get the per-step **merge base**
> and **fold order** exactly right → real 3-way merge (disjoint concurrent
> edits both survive) *and* byte-determinism; get them wrong → either add/add
> LWW (feature gutted) or divergence. Headline gate, §13.2.

**Frontier.** Over the node's known primitives, the frontier is the set not an
ancestor of any other known primitive (the un-merged tips), computed over the
*complete* DAG — complete because every parent fold commit is itself a
replicated reachable object (§5.2/§6.3), so no ancestry query hits a dangling
parent.

**Binary left-fold into synthetic fold commits.** `main` is a chain of real
deterministic commits, **not** a flat octopus:

1. Sort the frontier tips by the strict total order `(counter, NodeId, SHA)`
   (§5.1).
2. `acc₀` = the lowest-ordered tip (a real commit).
3. For each next tip `Tₖ` in order:
   - `base = git-merge-base(accₖ₋₁, Tₖ)` over the real DAG — well-defined
     because `accₖ₋₁` is a real commit whose ancestry bottoms out at fold
     commits / `M₀` (no synthetic-acc hand-waving: every step's base is the
     graph merge-base of two real objects). When the real graph yields **more
     than one maximal common ancestor** (a criss-cross), select the one
     **lowest by commit-SHA bytewise** — a content-only, wall-clock-free
     tiebreak consistent with the §5.1 strict-total-order SHA tiebreaker.
     **Never** tiebreak a merge base by committer/author time: that would
     reintroduce a wall-clock-derived input forbidden by §5.1 and §5.4(2).
   - `tree = 3way-merge(base.tree, accₖ₋₁.tree, Tₖ.tree)`: disjoint regions
     both survive; an overlapping hunk resolves to the operand later in the
     strict order; **no conflict markers ever**; binary / non-mergeable files
     fall back to whole-file total-order selection.
   - `accₖ` = a **synthetic fold commit**: that `tree`, parents
     `[accₖ₋₁, Tₖ]` (canonical), byte-pinned identity (§5.4) ⇒ deterministic
     SHA.
4. `main` = the final `accₙ`. Degenerate cases: `|F|=0` → `M₀`; `|F|=1` → one
   synthetic fold commit `(tree = the tip's tree, parent = [tip])` — the
   trivial self-wrapper, **not a 3-way merge** (no merge-base, no diff3;
   thin-node safe, §7). Update `refs/heads/main`; materialize (§5.6).

Every intermediate `accₖ` is a real, persisted, content-addressed,
deterministic, **verifiable** object (reproducible by recomputing the fold
over its own parent list; replicated like any reachable object). A node with
only a subset computes an earlier but still deterministic `main` and converges
on catch-up; it never recomputes historical fold commits.

Determinism rests on four **co-equal, hard** requirements (§5.4): (i) the DAG
is complete (every parent fold commit replicated → no dangling parent);
(ii) per-step base = `git-merge-base(accₖ₋₁, Tₖ)` over the real graph, never
per-node "last converged" state; (iii) the strict total order fixes both fold
order and conflict tiebreak with no ties; (iv) the fold commit object is
byte-pinned. **Order-only associativity must hold:** the result must depend on
nothing but the DAG and the strict order — the §13.2 reference fold must
property-test exactly that.

### 5.4 HARD INVARIANT — byte-deterministic synthetic fold commits

Stock-git compatibility *requires* `main` (and every synthetic fold commit) to
converge to one SHA everywhere given the same primitive set. Each synthetic
fold commit object is **byte-for-byte pinned**:

- **tree** — the §5.3 fold result, deterministic;
- **parents** — `[accₖ₋₁, Tₖ]` (the binary left-fold; `M₀`: none; `|F|=1`
  wrapper: `[tip]`), exact, canonical;
- **author & committer** — a fixed constant identity (`csp <csp@localhost>`),
  never the local node;
- **author & committer time** — `max(committer time of all parents)`; because
  `accₖ₋₁` is always a parent, this is **monotone non-decreasing along the
  spine** (so `git log` / `--since` over `main` are sound under author skew;
  primitive author-time skew only affects time-restore §8);
- **message & encoding** — fixed template, fixed bytes;
- **unsigned** — only *primitives* are signed (§5.1/§5.2). A signature on a
  fold commit would be node-local non-determinism; fold commits must be
  reproducible byte-identically by any node.

Any leak of local/non-deterministic state silently re-diverges every node.
The invariant has **four co-equal, hard parts** — all must hold, and the
§13.2 conformance suite (CI-blocking) must exercise all four:

1. **Complete DAG** — every parent fold commit is a real replicated object
   (§5.2/§6.3); no dangling parent, ever.
2. **Deterministic per-step base** — `git-merge-base(accₖ₋₁, Tₖ)` over the
   real graph; never per-node "last converged" state. A criss-cross with
   multiple maximal common ancestors is tie-broken by **commit-SHA bytewise
   (lowest wins)** — content-only, never committer/author time, so the base
   is wall-clock-free and byte-identical on every node.
3. **Strict total order** — `(counter, NodeId, SHA)` fixes fold order *and*
   conflict tiebreak with no ties (incl. same-NodeId concurrency, §5.1).
   3-way merge is non-associative, so this is load-bearing, not an
   optimization.
4. **Byte-pinned fold commit object** — the field list above.

### 5.5 Refs summary

- **Primitive commits** — replicated, content-addressed, authored-once,
  **signed** objects (§5.1/§5.2), each parented on a synthetic fold commit —
  not a private lineage. `refs/heads/node/<NodeId>` MAY point at a node's
  latest primitive for discovery / `git log --all`.
- **`refs/heads/main`** — the **final synthetic fold commit** of the §5.3
  binary left-fold; recomputed locally; identical SHA on every node with the
  same primitive set; inspected read-only via `ctx git log`/`show`/`diff`
  (§17). Restore is `ctx restore`, never `git checkout`.
- Intermediate synthetic fold commits are history objects reachable through
  the fold-chain spine from `main` (GC-safe — §9.2).
- `refs/tags/snap/<name>` — named snapshots (§8), deterministic commits.

### 5.6 Materialization vs. user edits — no feedback loop

CSP both **writes** the working tree (materialize, §5.3 step 5) and **watches**
it for user edits (`ctx watch`, §17). Without an explicit rule these feed back:
every received merge → watcher fires → spurious primitive commit → re-fold →
unbounded churn. This is the single most common file-sync bug class; it is
specified here, not left to implementers.

**Reconcile by content, never by intercepting writes.**

- For every in-scope path CSP records the **content hash it last
  materialized** (the hash from the current `main` tree), persisted in
  `.context/state`. This is the authoritative "what CSP put there" record.
- An FS event alone never creates a commit. On each debounced settle CSP
  rescans in-scope paths and compares each file's current content hash to its
  last-materialized hash:
  - **equal** → CSP's own write (or a no-op touch) → ignore; no commit.
  - **different** → genuine user edit → include in the next primitive commit,
    then update the last-materialized record.
- Materialization writes are **atomic** (temp + rename): readers never see a
  torn file and the watcher sees one event per path.

Because a self-write's resulting hash equals what CSP just recorded, self-
writes are inherently non-events — no fragile path/timestamp suppression, and
it is race-tolerant.

**The genuine race — user edits a file while CSP is materializing it.**

- The filesystem is the source of truth for "what the user currently has."
  After settle, the hash compare runs as above; an edit made during
  materialization has on-disk hash ≠ materialized hash → correctly taken as a
  user edit and committed **parented on the just-applied `main`**, then folded
  in by §5.3 against the proper DAG base. The guarantee is exactly §12's, no
  stronger: a disjoint region survives; a *same-region* collision with a
  concurrent remote change is resolved deterministically by the strict total
  order — the losing side (possibly the user's deferred edit) is **not in the
  working tree but is durably in history and recoverable**. It is *not* "no
  edit is ever lost"; it is "no *silent* loss — deterministic resolution, loser
  retained in history."
- Materialization **MUST NOT clobber a contended path**: if a path's on-disk
  hash differs from its last-materialized hash (a pending user edit) *and*
  the new `main` wants yet different content, CSP **defers** that path —
  leaves the user's bytes, lets them become a primitive commit, and
  re-materializes from the next `main`. Disjoint files materialize normally;
  only the contended path defers.

---

## 6. Replication protocol

### 6.1 Topology

A **full** node — run via the `ctx` CLI — may enter **listen mode**
(`ctx watch --listen`), accepting inbound connections and **relaying** objects
between its peers (only full nodes may listen — §7). A listener is an ordinary
full peer with good connectivity — **not** a merge authority and owning no
canonical state: every full node derives the *identical* deterministic `main`,
so no listener is privileged. Because object integration is idempotent and the
merge deterministic, any topology (star, mesh, chain, gossip) converges to the
same `main`.

**Trust is connection-level; signatures are content integrity.** Each listener
admits inbound connections against its local `authorized_keys` (§10) — that
admission is the load-bearing trust gate. Once a peer is admitted, every
primitive it sends or relays is integrated regardless of who originally
authored it. Primitives still carry their author signature for *integrity*:
the signature must verify (a primitive with a missing or invalid signature is
corrupt and dropped, the same way a fold commit that fails recompute-verify
is dropped, §5.2) — but the author NodeId is **not** required to appear in
the receiver's `authorized_keys`. Trust is therefore *intentionally
transitive through admitted connections*: `A → relay → B` converges because
both `A` and `B` admit the relay and the relay admits both of them; content
authored by anyone the relay admits flows to every reader. This is what
makes a multi-writer single-relay topology — the common hub-and-spoke
deployment — converge without each reader having to enumerate every writer's
key. The trade-off: a compromised relay can forward primitives signed by
anyone (including freshly-minted keys), so the operator's responsibility is
to gate writers at each listener's admission. Synthetic fold commits are
unsigned and admitted only by recompute-verification (§5.2) from
already-accepted primitives.

### 6.2 Transport

A persistent, reliable, ordered, message-oriented, bidirectional channel
(WebSocket). Binary length-delimited frames. Fully symmetric. **Git’s smart
protocol is not used. There is no polling.**

### 6.3 What is replicated

**Primitive commits and every object reachable from them.** A primitive's
parent is a synthetic fold commit (§5.2), so reachable-object replication
carries that fold commit, its parents, transitively to `M₀`: synthetic fold
commits **are** transmitted as ordinary reachable git objects (a dangling
parent is an invalid DAG). They are **verified, not trusted** — each
recompute-verifies from its own parent list (§5.2). Primitive commits are
**accepted iff the author signature verifies** (§6.1/§10) — a primitive with
a missing or invalid signature is corrupt and dropped. The author NodeId is
*not* required to be in any local set: admission is the connection-level
gate (§10), not per-primitive. Unverifiable objects (bad signature, malformed
commit, or a fold commit that fails recompute-verify) are dropped and not
forwarded.
**Every node — browser/WebView included — recomputes its own current `main`**
(one engine everywhere, §4/§7); no node recomputes *historical* fold commits
(those are recompute-verified once on receipt, §5.2, then trusted). Snapshots
(§8) replicate as small records.

### 6.4 Catch-up

Catch-up is **frontier-set anti-entropy**, not a scalar version vector. On
connect each side advertises a compact **digest of its current frontier**
(the set of un-merged primitive tip SHAs — small: one per concurrent
lineage). Each side requests the tip SHAs it lacks, then pulls each tip's
reachable closure (which backfills its parent fold commits → `M₀`); objects
are content-addressed, deduplicated, idempotent, hash-verified, and
signature-verified per primitive (§6.3). Then both sides recompute `main` and
materialize.

A scalar per-NodeId version vector is **unsound as the correctness
mechanism** under arbitrary gossip/mesh topology and same-NodeId concurrency
(§5.1): a high-water counter cannot express "missing a below-high-water
concurrent sibling," and a frontier tip is no one's parent so SHA-reachability
never requests it. The protocol therefore reconciles the *frontier set
directly* (SHA-exact, topology- and author-behaviour-agnostic). A version
vector MAY still be exchanged as an *optimization hint* to skip the common
fast path, but it is never the thing correctness depends on.

### 6.5 Live

After catch-up, each new local primitive commit is pushed immediately to all
connected peers (commit + missing objects). **Every** receiver integrates the
primitive commit and **recomputes its own `main`** (identical engine
everywhere — there is no "thin receiver applies a tree served by a full peer"
path; that pre-revision design is gone, §4/§7), then materializes. Listenable
(relay) nodes forward onward. On disconnect, a node simply reconnects and
re-runs catch-up — there is no separate resync path.

### 6.6 Message kinds (sketch)

`Hello`, `AuthProof`, `FrontierDigest`, `WantTips`, `Objects`,
`Live(signed primitive)`, `Ping`, `Pong`. The handshake challenge is the
channel-binding transcript carried by `Hello`/`AuthProof` (no separate
`AuthChallenge`). The listener's `Hello` **advertises its channel binding**
— the SHA-256 of its TLS certificate, or an all-zero/empty "binding
disabled" marker under `--no-tls` — and *both* sides sign the transcript
over that single advertised value (§10), so a TLS-terminating front proxy
no longer desynchronizes the two transcripts. Object exchange is
closure-based (`WantTips` → `Objects`,
no separate `Commits`/`WantObjects`); snapshots are local refs replicated as
ordinary objects (no `Snapshot` message). `FrontierDigest`/`WantTips` is the
sole, authoritative reconciliation (no scalar-VV fast-path).

---

## 7. Node roles (platform-derived, not a configured tier)

> **Revised (pre-release): one engine everywhere; no `tier` knob in v1.** The
> original design split *tiers* by *merge capability* (thin nodes couldn't
> merge). The deterministic 3-way merge/fold compiles to `wasm32`, so **every
> node — including a wasm plugin — runs the identical Rust engine and computes
> its own byte-identical `main`**. With merge no longer a tier axis, the only
> remaining distinction is **whether a node can listen/relay**, and that is a
> **platform fact, not a configured tier**: there is therefore **no `tier`
> config field, flag, or env var in v1**. A bounded **retention horizon** is
> *post-v1* (§9.2); v1 keeps complete local history everywhere. The role
> terms "full" / "thin" survive only as **descriptive shorthand** for the
> platform-derived capability below, never as a stored setting. Proven by the
> §18 SDK⇄`ctx` parity suite.

Every node runs the same engine and protocol and is **offline-first**: it
always has a complete local working copy, reads and writes entirely offline,
records its own edits locally, computes the deterministic `main` itself
(§5.3), and reconciles on reconnect. Nothing a node does — merge included —
depends on a role; role governs only *whether it can accept inbound
connections*.

- **Full (listenable) node.** A **native** node (real on-disk odb + the
  ability to bind inbound server sockets). It *may* enter listen/relay mode,
  retains the entire primitive-commit DAG, offers deep point-in-time
  recovery, and bootstraps others. Desktop / server / always-on. This is a
  *capability of the platform it runs on*, established by it being a native
  build with the on-disk odb — not selected by config.
- **Thin (outbound-only) node.** A **wasm/WebView/browser** node. Same
  engine, same `compute_main` — it **computes its own merge**, holds a full
  local working copy, appends its own **signed primitive commits**, folds the
  frontier exactly like any node — but **cannot listen** because a browser/
  WebView has no server sockets (a compile-time platform fact: the listener
  module is native-only, §16). It converges over an *outbound* connection to
  a listenable peer. The §5.3 `|F|=1` self-wrapper is still the degenerate
  case of the *same* fold (not a special thin path).

**HARD INVARIANT — listen/relay ⇒ native/listenable node.** A node in
listen/relay mode MUST be a native node — not because of merge (every node
merges identically) but because listening requires inbound server sockets +
the on-disk odb, which are native-only. This is **enforced structurally**:
the wasm build does not compile the listener at all (§16), so a browser/
WebView node is outbound-only by construction, with no `tier` string to set
or check.

**Deployment requirement.** Every deployment MUST contain at least one
listenable (native) node as the rendezvous/relay point — two browser/WebView
nodes cannot connect *to each other* directly (neither can accept a
connection), even though each merges on its own. This is a transport/platform
constraint, not a merge one.

**Still fully offline-first.** No node can merge changes it has not received —
convergence inherently requires connectivity for *everyone*. A browser/WebView
node authors and reads entirely offline, **computes its own merge**, and
converges on reconnect; the only thing it cannot do is *listen/relay*, never
computing the merge and never its ability to work disconnected.

---

## 8. Point-in-time recovery

Every **full node** can recover prior state with no network, because it holds
every primitive commit (each carrying its author wall-clock time and version
trailer, §5.2).

- **Restore to time T:** take the *set* of primitive commits with author-time
  ≤ T and run the deterministic fold (§5.3) over that subset's frontier → the
  historical tree. (Same algorithm as `main`, just over a time-filtered
  subset; deterministic for the same reason.)
- **Restore to a named snapshot:** a snapshot records the **frontier
  primitive-commit SHA set** at creation plus a label; it replicates as a
  small record and is materialized as a deterministic commit under
  `refs/tags/snap/<name>` — inspectable read-only (`ctx git show`/`ls-tree`);
  to actually restore it use `ctx restore <name>` (never `git checkout`).
- **Applying a restore** is just editing: the restoring node writes the
  historical tree into its working files and commits it onto its own primitive
  lineage. It then propagates and converges through the normal protocol. The
  pre-restore state remains fully in history and is itself restorable. There is
  no special “rewind” message and no possibility of divergence.

Time-based restore is *approximate* under author-clock skew (it relies on the
advisory wall-clock trailer). The logical total order remains authoritative for
correctness; only the “which moment” selection uses wall time. Snapshots give
exact, skew-free recovery points.

---

## 9. Storage & compaction

### 9.1 On-disk layout (full node)

```
<scope-root>/                 # materialized working files (real, what tools read)
<scope-root>/.context/            # CSP's ENTIRE footprint — one dir; gitignore this
<scope-root>/.context/git/        #   the real stock-git-compatible repo (GIT_DIR)
<scope-root>/.context/config          #   scope, peers, listen/transport, knob settings
<scope-root>/.context/authorized_keys #   admitted peer keys — LOCAL, NOT synced (§10)
<scope-root>/.context/state           #   sync/peer state, last-materialized hashes,
                                  #   AND named snapshot records (small, §8)
<scope-root>/.context/state.lock      #   state write lock (internal)
<scope-root>/.context/tls.crt         #   persisted self-signed listener cert (§10)
<scope-root>/.context/tls.key         #   persisted self-signed listener key (§10)
<scope-root>/.contextignore           # user exclude file (synced); see §11
~/.context/id_ed25519             # device identity (default; see §10) — NOT per-vault
```

There is deliberately **no `.git` at `<scope-root>`** — the git directory is
`<scope-root>/.context/git/` with the worktree being `<scope-root>` itself. This is
what lets CSP live in a project that also has its own user-owned git repo
(§11). CSP's whole on-disk footprint is the single `<scope-root>/.context/`
directory (plus the synced `.contextignore`).

**Naming principle (two distinct layers).**

- **Protocol / on-disk format → protocol-anchored, frozen.** `.context/`,
  `.contextignore`, `~/.context/…`, and the merge-commit identity constant
  (§5.4) are part of the CSP format, written/read identically by *every*
  implementation (Rust core, wasm/TS SDK, host plugins). The `ctx` CLI is just
  one front-end and MUST NOT lend its name to the format. Changing any of
  these is an on-disk format break.
- **CLI / launcher surface → tool-anchored.** CLI flags, environment
  variables (`CTX_*`), and the config file are read by the **`ctx` CLI /
  native launcher only** — the engine (csp-core) does not read process env,
  the SDK does not, and host plugins use host settings, not `CTX_DIR`. They
  are configuration of one front-end, exactly parallel to CLI flags (a flag is
  not "part of the spec"; neither is an env var). Hence the tool-anchored
  `CTX_` prefix, not `CSP_`. (`PORT` is kept as the platform convention.)

`<scope-root>/.context/` is the default per-vault state directory; a host (e.g. a
browser/mobile thin node) may override it to use host-provided storage instead.
The **device identity (private key) is device-global by default** — `~/.context/
id_ed25519`, or a reused `~/.ssh` key / SSH agent (§10) — *not* stored inside a
vault's `.context/`: one device may join several vaults with one key, the key must
survive deleting a vault's `.context/`, and keeping the most sensitive file out of
the synced subtree minimizes blast radius. Per-vault identity is an opt-in for
stronger isolation. `.context/` holds only a *reference* to which identity to use,
never the private key by default.

A working node keeps its identity reference, `config`, the working files, and
only the objects backing current `main`.

### 9.2 Packing, GC & retention

- **Packing/GC (automatic hygiene, non-negotiable).** Debounced auto-commits
  create many small loose objects; full nodes auto-pack (the engine's own
  pack-objects) on a size/count threshold or on idle, and GC unreachable
  objects. Required maintenance, not a policy choice. **GC-safety:**
  historical synthetic fold commits and primitives are reachable through the
  fold-chain spine from `refs/heads/main` (and from snapshot tags), so a
  correct reachability GC already retains them — an implementer must **not**
  special-case "prune synthetic fold commits": they are load-bearing history
  (parent edges of later primitives/folds), not garbage.
- **All nodes keep complete local history in v1** — this *is* the PITR
  guarantee and the durable archive, and it holds on every node regardless of
  platform (v1 has no `tier` knob, §7).
- **Retention horizon is post-v1.** A bounded retention horizon (recent
  history + working state kept; deeper history pruned locally and back-filled
  from a listenable peer on demand) — the thing that would let a storage-
  constrained browser/WebView node bound unbounded growth — is **explicitly
  deferred to post-v1**. No pruning-by-horizon, no on-demand deep-history
  back-fill protocol, and no per-node retention policy ships in v1; every
  node retains everything. (This resolves the prior §7-vs-§9.2 tension: §7
  no longer promises a "by default" horizon.)
- **Snapshots are never pruned by truncation.** Snapshot-reachable states
  survive any future horizon, so named recovery points stay exact and durable
  — relevant once a horizon exists post-v1.

Compaction is local and uncoordinated; nodes may run different policies.
**Deliberately low priority:** at the markdown/text scale CSP targets,
content-addressed history packs so tightly that growth is a non-issue for a
long time — the simple defaults above suffice. Aggressive compaction *and* the
thin-node retention horizon are explicitly deferred, not a v1 concern.

---

## 10. Security & authentication

- **Node identity is an SSH key.** Every node has an ed25519 keypair; the
  public key is its durable identity, in standard OpenSSH public-key format
  (`ssh-ed25519 AAAA… [comment]`). An existing user SSH key may be reused, and
  signing MAY be delegated to a running SSH agent rather than holding the
  private key in process.
- **Key location is device-global by default.** The private key defaults to
  `~/.context/id_ed25519` (or a reused `~/.ssh` key / SSH agent) — a per-*device*
  identity reusable across every vault the device joins, surviving deletion of
  any vault's `.context/`. It is **never** stored in a vault's `.context/` by default
  and is never synced. A per-vault key is an opt-in for stronger isolation
  (a compromised vault dir can't then expose a key used elsewhere), at the
  cost of key sprawl.
- **Authorization via node-local `authorized_keys` — the load-bearing trust
  gate.** A listening (full) node admits only peers whose public key appears
  in `<scope-root>/.context/authorized_keys` — one key per line, `#` comments,
  same syntax/semantics as SSH's. **This is the *only* trust gate**:
  primitives received over an admitted connection are integrated regardless
  of who originally authored them (§6.1) — signatures gate *integrity*, not
  admission. The set is **node-local and NOT synced** (it is under `.context/`,
  which §11's HARD INVARIANT excludes from replication entirely):
  authorization is per-node config, never propagated. Managed via
  `ctx authorize <pubkey>` / `ctx revoke <pubkey>`, the `CTX_AUTHORIZED_KEYS`
  env var (merge-on-start, idempotent), or seeding at `ctx init`. `ctx key`
  prints a node's own public key for sharing. (Trade-off, chosen
  deliberately: adding a key must be done on each listener and does not
  propagate — simpler, removes the "a peer pushes a malicious key to every
  node" vector, and matches the topology — listeners are few full nodes, §7,
  so writer admission converges to managing one set per relay.)
- **Bootstrap: trust-on-first-use, bounded to the empty-set window.** When a
  listening node has **no** local authorized set yet (genuine first-peer
  bootstrap), it MAY trust-on-first-use: the first connecting key is recorded
  into `.context/authorized_keys`, and from then on the local authorized set is
  authoritative. TOFU applies *only* while the set is empty/absent — never as
  an ongoing admission policy. `CTX_AUTHORIZED_KEYS` (or `ctx authorize` /
  seeding at `ctx init`) may pre-populate the set so the TOFU window never
  opens. A `--no-tofu` switch (and config equivalent) disables TOFU entirely
  for hardened or internet-exposed deployments. **Honest caveat:** an
  internet-reachable listener with an empty authorized set and TOFU enabled
  trusts whichever key connects first — operators exposing a fresh listener
  publicly must pre-seed keys or disable TOFU.
- **Mutual authentication.** The handshake requires each side to sign, with its
  ed25519 key, a transcript covering both nonces and a binding to the
  underlying transport, so a captured handshake cannot be replayed or relayed
  onto another channel. Both directions authenticate: a connecting node also
  verifies the listener's key, enabling key pinning.
- **Advertised channel binding (the listener owns it).** The transport
  binding mixed into the signed transcript is **not** each side's local view
  of the certificate (that desynchronizes the moment a benign TLS-terminating
  proxy sits in front of the listener — the connector binds to the proxy's
  cert, the listener to its own/none, and the two transcripts can never
  agree, surfacing as an opaque signature failure). Instead, the **listener
  advertises one channel-binding value in its `Hello`** — the SHA-256 of the
  certificate it serves, or an all-zero/empty *binding-disabled* marker when
  it runs `--no-tls` behind a TLS terminator — and **both sides sign the
  transcript over that single advertised value**. Separately, and *only as an
  explicit check with its own distinct error* (never as silent transcript
  divergence), the connector enforces the binding:
  - *Advertised binding disabled* (all-zero/empty): degraded mode. The
    connector skips the certificate comparison; trust falls back to the
    **TOFU-pinned listener identity** (the transcript also covers the
    listener's NodeId, which a MITM cannot forge — and `ctx clone` already
    pins the listener key, §6.1). Required configuration behind a
    re-terminating reverse proxy.
  - *Binding advertised but unobservable* (plaintext `ws://`, or a browser
    `WebSocket` that cannot read the peer cert — §7): degraded as above; the
    connector SHOULD warn.
  - *Binding advertised and observable*: the connector MUST verify the
    advertised fingerprint equals the certificate it actually saw and MUST
    abort with a distinct channel-binding error (not a generic signature
    failure) on mismatch — this is the live MITM / cert-substitution defense.
  A handshake-transcript or framing change is a coordinated break: the wire
  `proto` version is bumped so skew is reported as a clear version-mismatch
  rather than an opaque signature error.
- **Per-primitive signature verification (not a second admission filter).**
  Transport admission (above) is the trust gate; primitive signatures are
  *content integrity*, not policy. Every **primitive commit is signed by its
  author NodeId key** (§5.1/§5.2); receivers verify that signature on receipt
  and drop any primitive whose signature is missing or invalid — that is a
  corrupt or forged object, structurally not a valid CSP primitive. Once the
  signature verifies, the primitive is admitted regardless of whether the
  author key is locally known: relays explicitly extend trust to whatever
  their admitted peers send, and content authored by anyone the relay admits
  flows through to readers connected to that relay. This makes a multi-writer
  single-relay topology converge naturally — `A`, `B`, both connect through
  relay `R`; `R` admits both at the connection layer; `A`'s primitives reach
  `B` (and vice versa) via `R`'s broadcast without either side enumerating
  the other's key. Synthetic fold commits are unsigned and admitted only by
  recompute-verification (§5.2). Bootstrap of a new replica: `ctx clone`
  pins the peer's NodeId for connection-level trust on subsequent
  reconnects; no per-author authorization set needs to be conveyed.
  **Trade-off:** a compromised listener admits forged authors as freely as
  legitimate ones. The mitigation is operational — gate writers at each
  listener's admission, treat a relay compromise as a vault compromise — not
  cryptographic re-verification at every hop (which would only reproduce
  connection-level admission in a more expensive form, since each hop is
  already mutually authenticated).
- **Identity / single-writer protection.** A NodeId is the ed25519 key.
  Reusing it across *different* vaults is fine; the *same key actively writing
  two replicas of one vault* is the hazard (§5.1). The counter is durably
  persisted; `ctx clone` / restore MUST fork a fresh NodeId or warn rather
  than resume authoring under a possibly-live key. Correctness survives a
  violation (strict total order + signatures), but history becomes confusing —
  this protection keeps it clean.
- **Transport confidentiality.** The default transport is **`wss://`**: a
  listener serves TLS using a **self-signed certificate it generates and
  persists** under the never-synced `.context/` (CSP ships **no embedded
  certificate authority**, so the X.509 layer is *not* the trust boundary).
  Connectors accept any server certificate at the TLS layer; trust is
  established by the ed25519 mutual-auth handshake (§10), which binds the
  channel and enables listener-key pinning. TLS therefore adds confidentiality
  only — protocol-level mutual auth and content integrity hold regardless.
  **`--no-tls` / `CTX_NO_TLS`** opts a listener out into plaintext `ws://`,
  for running behind a fronting proxy that already terminates TLS (optionally
  with a CA-trusted cert) or on a trusted/local network. Connectors select
  TLS by URL scheme (`wss://` vs `ws://`). A listener reached **through a
  TLS-terminating reverse proxy (Fly.io, Railway, Render, Cloudflare Tunnel,
  …) MUST run `--no-tls`**: the proxy re-terminates TLS, so the certificate a
  connector observes is the proxy's, never the listener's — only the
  advertised binding-disabled marker keeps the handshake coherent (above).
  Connectors then still dial `wss://` so the proxy hop stays encrypted; the
  TLS layer authenticates the proxy and the pinned listener identity
  authenticates the peer.
- **Integrity.** Objects are **content-addressed and self-verifying**: a
  received object is stored under the SHA recomputed from its own bytes, so a
  corrupted or substituted object cannot masquerade as another and is never
  referenced by a valid DAG. Trust does **not** come from the bytes hashing —
  it comes from **connection-level admission** (§10) plus *content integrity*:
  **primitive signatures** (§5.1/§10) ensure each primitive is well-formed by
  its claimed author, and **fold-commit recompute-verification on receipt**
  (§5.2/§6.3) ensures synthetic fold commits are derivable. Unsigned,
  signature-invalid, or recompute-failing data is dropped and not forwarded.

---

## 11. Scope & coexistence

- The synced set is an **explicit allowlist scope**: a configured subtree
  and/or include patterns, plus exclusions. CSP must never read or write
  outside scope. An allowlist (not “everything minus a denylist”) makes the
  default failure mode *syncing too little*, never exfiltrating secrets or
  build output. The **default include is `**`** (everything under the scope
  root). This is deliberately usable out of the box, bounded by three
  always-on guards that keep the blast radius small even at the permissive
  default: (i) `.context/` is unconditionally excluded (the HARD INVARIANT
  below); (ii) non-text/binary files are excluded unless explicitly opted in;
  (iii) the synced `.contextignore` removes patterns. The allowlist mechanism
  exists for *narrowing* to a dedicated subtree; `**` is the safe default
  because the guarded classes (CSP state, secrets-as-binaries, ignored paths)
  are never in scope regardless.
- **Text-only by default; binaries are opt-in and never merged.** v1 syncs
  only text/mergeable files. Binary / non-text files are excluded by the
  allowlist unless explicitly opted into the scope; when opted in they are
  **whole-file, last-writer-wins by total order** — **not a separate code
  path** but the §5.3 fold's existing non-mergeable fallback: a binary /
  non-UTF-8 file is never diff3'd; the whole file resolves to the operand
  later in the strict total order. No chunking anywhere (objects are whole
  blobs). Content-defined chunking/dedup for large binaries is explicitly out
  of scope for v1.
- **`.contextignore` — the user exclude file.** A gitignore-syntax file at
  `<scope-root>/.contextignore`, scope-relative, applied as exclusions *under* the
  allowlist scope (allowlist decides what's eligible; `.contextignore` removes
  patterns from it). It sits at the scope root as ordinary user-managed
  content and **is itself synced** (it is *not* under `.context/`), so the
  exclusion policy is shared across nodes. An optional node-local
  `<scope-root>/.context/exclude` (gitignore syntax, never synced — it is under
  `.context/`) adds machine-only excludes — the analog of git's
  `.git/info/exclude`.
- **Empty directories — the `.keep` sentinel.** The history is stock-git-
  compatible (§1), and git has no representation for a directory: a tree is
  derived purely from the paths of the files in it, so an empty directory
  cannot exist in `main`, replicate, or materialize. To preserve
  user-created empty folders, an in-scope directory that is otherwise empty
  is represented by a single zero-byte file `<dir>/.keep`. It is an ordinary
  in-scope, git-tracked file (default include `**`; *not* under `.context/`;
  exempt from the text/binary test as a CSP control sentinel, like
  `.contextignore`). The rule is **engine-owned and deterministic** so every
  node converges (§12): when the engine builds a primitive's tree it
  canonicalizes — `<dir>/.keep` is present **iff** no in-scope file exists
  anywhere under `<dir>/`. Adding the first real file to a folder therefore
  deterministically drops its `.keep`; deleting the last file
  deterministically re-adds it; nested empty folders each carry their own
  `.keep` at the leaf so the full structure round-trips. The host's only
  duty is to surface its empty in-scope directories (a `ctx` node walks the
  filesystem; the Obsidian host enumerates vault folders) by injecting the
  sentinel into the working set it hands the engine; the engine's
  canonicalization (strip-redundant, normalize-to-empty) makes the result
  identical regardless of host quirks. Obsidian hides dotfiles, so `.keep`
  is invisible in its explorer; it is a visible `.keep` in the CLI directory
  and `git log`, the conventional `.gitkeep`-style marker.
- **Coexistence with a user-owned git repo (no collision by construction).**
  CSP plants **no `.git` at the scope root**; its repository is
  `<scope-root>/.context/git/` with the worktree being `<scope-root>` (§4, §9).
  So a user's own git repo can own the surrounding project (prompts,
  instructions, code) while CSP owns the context/memory subtree, with zero
  `.git` conflict. **Default model:** CSP's scope is a dedicated subtree
  (e.g. `project/context/`); the enclosing project repo gitignores that
  subtree; the two never overlap. Same-directory interleaving of git-tracked
  and CSP-managed files is possible only because there is no `.git` at the
  root, but it additionally requires the allowlist scope to precisely
  partition ownership and is an advanced configuration with sharper edges —
  the supported default is the dedicated subtree.
- **HARD INVARIANT — CSP never replicates, commits, or exposes its own state.**
  `<scope-root>/.context/` is unconditionally excluded from the sync scope and
  from any enclosing repo's tracked content, regardless of include patterns;
  CSP must never transmit, materialize from a peer, or commit anything under
  `.context/`. This keeps the device key reference, peer list, and local state out
  of history and off other nodes — the same secrets-safety reasoning as the
  allowlist, applied to CSP's own footprint. (There is no `.git` at the scope
  root to exclude — CSP doesn't create one.)

---

## 12. Consistency model & guarantees

- **Convergence.** Given the same set of primitive commits, every node computes
  the identical `main` SHA and working tree, regardless of delivery order or
  topology — provided the §5.4 byte-determinism invariant and canonical fold
  order hold. Integration is idempotent.
- **Eventual consistency.** Connected nodes converge within a round trip;
  disconnected nodes converge on reconnect via catch-up.
- **Merge behavior.** Concurrent edits to *different regions* of a file both
  survive (real 3-way merge). Concurrent edits to the *same region* are
  resolved deterministically by total order — the losing edit is not in the
  working tree but **remains in history** (its primitive commit and objects
  persist on full nodes) and is recoverable via §8. This is intentional;
  same-region co-editing is out of scope (§3).
- **Offline writes** are first-class: a node edits locally, commits to its
  lineage, and reconciles on reconnect; whether a contested region “wins” is
  decided by the total order, not arrival time.

---

## 13. Resolved decisions & residual gates

### 13.1 Resolved decisions

- **Binary/large files:** text-only by default; binaries excluded by the
  allowlist unless explicitly opted in, then whole-file LWW by total order, no
  chunking. No content-defined chunking in v1. (§11)
- **Trust bootstrap:** `authorized_keys` is **node-local, NOT synced** (lives
  in `.context/`, §10/§11); authorization is per-node config and does not
  propagate. TOFU only while a node's local set is empty (genuine first-peer
  bootstrap); thereafter the local set is authoritative. Managed via `ctx
  authorize`/`revoke`/`key`, `CTX_AUTHORIZED_KEYS` (merge-on-start), or
  `ctx init` seeding; `--no-tofu` disables TOFU. (§10) *(Reverses the earlier
  interview lean toward a synced authorized_keys — chosen for simplicity and
  to remove the malicious-key-propagation vector.)*
- **Clock skew:** snapshots are the exact, skew-free recovery mechanism;
  `ctx restore <time>` is best-effort and warns on detected skew. (§8)
- **Auto-commit debounce:** default ~1 s (1000 ms), configurable with full
  three-way parity — `--debounce <ms>` / `CTX_DEBOUNCE` / config `debounce_ms`
  (flag > env > config). (§14, §17.1)
- **Hash:** plain SHA-1 for v1 (byte-compatible with `sha1dc` on non-colliding
  inputs; collision-detection intentionally not bundled — not the security
  boundary, §10, and the wasm engine is lean by construction, §16). SHA-256
  explicitly out of scope (no in-place migration — a different hash is a
  different repo); revisit post-v1 only if ecosystem support warrants. (§4)
- **git coexistence:** CSP never plants a `.git` at the scope root; the repo is
  `<scope>/.context/git/` with a decoupled worktree, accessed via `ctx git`.
  Default model: a dedicated subtree the enclosing project repo gitignores.
  (§4, §9, §11)
- **User excludes:** a synced, gitignore-syntax `.contextignore` (plus optional
  node-local `.context/exclude`) layered under the allowlist scope. (§11)
- **Node roles / wasm (revised, pre-release):** **one engine everywhere** —
  the deterministic fold/merge compiles to `wasm32`, so *every* node
  (including a wasm plugin) runs the identical `compute_main` and computes
  its own byte-identical `main`. With merge no longer a tier axis, **there is
  no `tier` config field/flag/env in v1**; the only node distinction —
  whether it can listen/relay — is a **platform fact** (native server sockets
  + on-disk odb), enforced structurally (the listener is not compiled to
  wasm). A bounded **retention horizon is post-v1** (§9.2); v1 keeps complete
  local history on every node. ≥1 listenable (native) node is still required
  as the rendezvous/relay point — two browser/WebView nodes cannot accept
  each other's connection; that is a transport/platform constraint, not a
  merge one. All-browser meshes remain unsupported for *connectivity*, not
  for merge. Proven by the §18 SDK⇄`ctx` parity suite. (§7, §4, §16)
- **CTX_\* env surface:** full deployment knobs (`CTX_DIR` [renamed from
  `CTX_CWD`], `CTX_NO_TLS`, `CTX_NO_TOFU`, `CTX_AUTHORIZED_KEYS`, `CTX_LOG`,
  `CTX_DEBOUNCE`, `PORT`, generic `CTX_*` overrides). Three-way parity
  (flag/env/config, flag > env > config) for every knob **except the vault
  locator** (`--dir`/`CTX_DIR`), which by nature has no config-file key (it
  locates the config itself — circular). (§17.1)
- **Conflict representation:** jj-style commutative conflicts evaluated and
  declined for v1; deterministic 3-way merge + total-order tiebreak retained;
  suppressed sides surfaced as a derived view over the DAG, not a stored
  layer. (A clean single materialized version per file is required, which
  nullifies jj's main payoff while adding cost and weakening git compat.)
- **Watcher ↔ materialize feedback loop:** specified in §5.6 — reconcile by
  last-materialized content hash (persisted in `.context/state`), atomic writes,
  and a defined no-clobber rule for the user-edits-during-materialization
  race. Self-writes are non-events by construction.

### 13.2 Residual gates & risks (must hold before/at release)

- **THE HEADLINE GATE — the binary-left-fold protocol (§5.1–§5.4, §6.3–§6.4,
  §10), as one interlocking unit.** CSP's single highest-risk, make-or-break
  property. The fold-chain, frontier anti-entropy, the strict total order, and
  per-primitive signatures share invariants and must be validated *together*,
  not as independent pieces. Correctness hinges, co-equally, on:
  1. **Complete DAG** — synthetic fold commits are transmitted as reachable
     objects; no node ever has a dangling parent (§5.2/§6.3).
  2. **Deterministic per-step base** — `git-merge-base(accₖ₋₁, Tₖ)` over the
     real graph; never per-node "last converged" state; criss-cross
     multi-base tie-broken by commit-SHA bytewise, never by time (§5.3/§5.4).
  3. **Strict total order** `(counter, NodeId, SHA)` — fixes fold order *and*
     conflict tiebreak with no ties, including same-NodeId concurrency (§5.1).
  4. **Byte-pinned, unsigned fold commit object** (§5.4).
  5. **Frontier-set anti-entropy** delivers the full primitive set under
     arbitrary gossip/mesh; a scalar VV does not (§6.4).
  6. **Per-primitive signature verification** for content integrity, plus
     **connection-level admission** as the trust gate (§6.1/§10) — relays are
     trusted by virtue of admission, primitive signatures gate corruption not
     authorship policy.
  **Conformance suite (hard, CI-blocking).** N simulated independent nodes;
  scenarios MUST include: concurrent commits with *differing parent fold
  commits* and multi-tip frontiers; the **offline-then-merge** case (A authors
  off a fold commit B never computed; B resolves it from the transmitted
  object); **same-NodeId concurrent authoring** (two replicas of one vault,
  equal counter → SHA tiebreak keeps a strict total order, convergence holds);
  a **relay delivering a primitive signed by a NodeId the receiver does not
  locally know** (admitted — connection admission is the trust gate, §10);
  a **relay delivering a primitive with an invalid signature** (dropped, not
  forwarded; corruption defense, not policy); and **gossip/mesh delivery**
  that would defeat a scalar VV. Assert: (a) identical `main` SHA *and* tree
  across all nodes and all delivery orders; (b) every received synthetic fold
  commit **recursively recompute-verifies** from its own parent list to its
  exact SHA. Build a small reference fold and property-test exactly this
  (order-only associativity, recursive verification) **before anything else is
  built** — if it cannot be made deterministic in practice, the architecture
  does not work. Non-negotiable.
- **Full-engine-in-wasm — DISCHARGED (was a residual gate; pre-release
  decision moved it here).** The original gate validated only a *reduced*
  wasm surface (no merge). Superseded by "one engine everywhere": the
  **full** engine — fold/merge (`compute_main`), the sans-IO `Session`,
  auth, framing, scope, config — compiles and runs under `wasm32` (browser/
  WebView), with only the native odb/sockets/TLS `cfg`-gated out. Proven,
  not assumed: the §18 cross-surface interop (byte-identity vs the shared
  vectors) and the SDK⇄real-`ctx` parity suite show the wasm node converges
  bit-for-bit with native. Wasm footprint is held down by the hand-rolled
  scope/config codecs (no `regex`/`toml`) and a size build profile; this is
  a maintenance concern, no longer a correctness gate. (§4, §7, §16, §18)
- **TOFU exposure — operational caveat, not a code gate.** An internet-
  reachable listener with an empty authorized set and TOFU enabled trusts the
  first connector. Deployments exposing a fresh listener publicly MUST
  pre-seed `CTX_AUTHORIZED_KEYS` or run `--no-tofu`. Document prominently;
  consider auto-disabling TOFU when the listen address is non-loopback. (§10)
- **`ctx git` read-only allowlist — data-loss-critical guard.** The repo is
  engine-owned; a write reaching it (e.g. a mis-allowlisted mutating verb, or
  an agent invoking `ctx git commit`/`checkout`) is silent corruption or loss
  (force-recomputed `main`, stomped worktree → §5.6 mass-commit). The
  allowlist is therefore **deny-by-default** and conservative — unknown/
  ambiguous verbs and any write-capable flags are refused, not best-guessed —
  and has its own test suite asserting every mutating verb is rejected (§17).
  Raw-`GIT_DIR` bypass remains explicitly unsupported and clobberable (§4).

---

## 14. Latency & performance

End-to-end (file saved on A → visible on B), with the dominant terms:

| Stage | Cost |
|---|---|
| Auto-commit **debounce** on A | **default ~1 s; configurable `--debounce`/`CTX_DEBOUNCE`/`debounce_ms` (≈200 ms–1 s; §17.1)** |
| Hash + commit locally on A | ~1–5 ms |
| Serialize + WebSocket send | sub-ms |
| Network A→(relay)→B | LAN ~5–30 ms; cross-region ~30–150 ms; far geo 200 ms+ |
| Integrate + deterministic head-fold + write on B | ~5–10 ms (no concurrent conflict; more with many concurrent heads) |
| Receiving host re-reads / re-renders | tens–low-hundreds ms (host-dependent) |

- Propagation itself (commit → wire → applied on B’s disk) is **sub-100 ms on a
  decent network**. The connection is persistent, so there is no per-message
  handshake; one small text edit is a few KB of objects.
- Perceived latency is dominated by the **debounce you choose** and the
  **receiving host’s own file-reload/redraw**, not the protocol.
- Realistic end-to-end: ~**300–450 ms** (LAN, tight debounce, desktop) to
  ~**0.7–1.2 s** (cross-region relay, conservative debounce, host refresh).
- Tradeoffs / honest caveats: tighter debounce → snappier but more commits,
  more concurrent heads, heavier folds, noisier history. A burst of many nodes
  editing at once makes the head-fold non-trivial (bounded, not free). Mobile
  OSes suspend sockets when backgrounded → not realtime until resumed
  (catch-up on wake). “Same instant on both screens” is not achievable by any
  system; sub-second on desktop/good-network is the target, gated by debounce +
  host redraw, not the protocol.
- **Thin vs full asymmetry (acknowledged).** A full node collapses its own
  edits into a linear spine each tick (it folds). A thin node cannot 3-way
  merge — without mitigation a long offline editing run (the
  editor-plugin/mobile case) would emit O(n) sibling tips, an O(n) fold on
  reconnect. The §7 self-collapse rule (thin nodes compute the trivial
  non-merge `|F|=1` wrapper over their own tip) removes this: an offline run
  contributes a single linear chain, O(1) to the frontier. Real-merge cost
  still lands on the full node it reconnects to, which is the intended place
  for it.

---

## 15. Why this shape

- **Real git, programmatically driven.** Git is battle-tested for content-
  addressed storage, 3-way merge, and history/PITR. We use exactly those,
  expose a real `.git` for transparency and stock tooling, and drive it
  automatically — without git’s slow/pull-based network layer.
- **Replicate the commit DAG, derive current state deterministically.**
  Authored-once, **signed** primitive commits replicate verbatim; their parent
  **synthetic fold commits** replicate as ordinary reachable, deterministic,
  recompute-*verifiable* objects (DAG never dangling); only the *current*
  `main` is recomputed, byte-identically, everywhere (convergent, no authority,
  stock-git-coherent). Frontier-set anti-entropy keeps replication sound under
  arbitrary gossip topology; per-primitive signatures keep content well-formed;
  connection-level admission (§10) is the trust gate — relays are trusted
  through that gate, not bypassed by signature checks.
- **Realtime transport, not git’s.** Persistent push + delta catch-up gives
  near-real-time sync with zero polling and no commit-then-wait latency.
- **One Rust/wasm engine; one behavior.** The same core runs everywhere and
  every node computes its own merge identically; storage-constrained
  browser/WebView clients are full working nodes that merge offline, while
  desktop/server nodes additionally listen/relay and carry git-compatible
  history and PITR. The only distinction is the platform-derived ability to
  listen — not a tier, not merge (§7).

---

## 16. Implementation architecture & SDK layering

The protocol is implemented **exactly once**, in Rust. Everything else is thin
bindings or host glue. This is a hard structural rule, not a preference.

- **`csp-core` (Rust crate).** The entire engine: object store, commit DAG,
  deterministic fold/**merge**, sync protocol state machine, identity & auth,
  scope/ignore, config codec. *All* protocol, merge, and convergence logic
  lives here and nowhere else. **One engine everywhere — the always-on
  surface (object/oid, fold+merge incl. `compute_main`, the sans-IO
  `Session`, identity/auth, wire framing, scope, the flat-TOML config codec,
  the engine-state model, the in-memory store) compiles to `wasm32`
  unchanged.** Only genuinely platform-bound pieces are behind the native
  `full` feature (`cfg(not(wasm32) + full)`): the on-disk odb/packfiles
  (`repo`), the disk-backed vault, the tokio socket driver (`net`), and TLS.
  It is emphatically **not** "merge compiled out of wasm" — every node,
  including a wasm plugin, runs the identical `compute_main` (§4/§7). Pure
  Rust; I/O injected via traits (storage, transport, clock, rng).
- **Sans-IO protocol core.** The replication state machine is a **sans-IO
  `Session`** (`session.rs`: handshake/transcript, frontier anti-entropy,
  integrate+verify, the `|F|=1` self-wrapper): it consumes inbound frame
  bytes and emits outbound frame bytes + effects, with **no sockets, fs, or
  clock of its own**. `ctx`'s `net.rs` (tokio) and the wasm/SDK node are
  both thin drivers over the *same* `Session` — the protocol is executed by
  one codebase on every surface, not reimplemented per host.
- **Lean by construction.** Because the one engine must also be the wasm
  payload, `csp-core` carries **no heavy general-purpose deps**: scope uses
  a hand-rolled gitignore matcher (no `regex`) and config a hand-rolled
  flat-TOML codec (no `toml`/`toml_edit`). Each replaced crate is kept as a
  **dev-only differential-test oracle** — the hand-rolled scope matcher is
  proven byte-for-byte against the old `regex` over tens of thousands of
  generated cases, and the config codec proven to round-trip and to emit
  TOML the real parser reads back identically (§18). Equivalence is proven,
  not assumed.
- **`ctx` (native CLI).** A thin driver over `csp-core`: argument parsing,
  process lifecycle, the filesystem watcher, the listen socket, the native
  on-disk odb/packing — feeding the shared sans-IO `Session`. **No protocol
  logic.**
- **TypeScript SDK.** `csp-core` compiled to a single **wasm** module plus
  thin TS bindings and injected host adapters (filesystem, WebSocket). It is
  **not a reimplementation** — there is exactly one protocol implementation
  (Rust); the SDK is a typed surface over it.
- **Host plugins (first target: an Obsidian plugin).** As thin as possible over
  the TS SDK: host file I/O via the host's storage adapter, transport plumbing,
  UI, lifecycle. **No protocol or merge logic.** Behavior parity with the CLI
  is *structural* (shared core), never hand-maintained.
- **Invariant — “one core, thin bindings.”** Any behavioral difference between
  the CLI and a host plugin is a bug, because both drive the identical
  `csp-core`. New capabilities land in Rust once and are exposed through every
  surface; a feature is not “done” until it is reachable from both the CLI and
  the SDK and covered by §18 tests.

---

## 17. `ctx` CLI surface & ergonomics

A single binary, `ctx`, exposes the **full** engine capability set — nothing
the protocol can do may be CLI-inaccessible. Command sketch (final names TBD):

- `ctx init [path]` — create a new, empty scoped vault and this node's SSH
  key identity. An explicit `[path]` is created if missing (`git init
  <dir>` spirit), `.` = current dir; as the most explicit form it wins
  over the global `--dir`/`CTX_DIR`, which it falls back to (then the
  current dir) when omitted.
  **Identity model:** `vault_id` is an **opaque protocol id**
  — a fresh **UUID** by default (it must not leak the node key and is only
  the handshake's "same vault?" equality guard, since all vaults share the
  global genesis `M₀`); `--vault-id` overrides it to deliberately share a
  memorable id. A separate optional **human name** (`--name`, else the
  scope directory's basename, git-spirit "the folder is the name", else
  empty) is *not* a uniqueness guarantee — it travels in config + `Hello`
  purely for display and clone-folder naming. `--watch` stays running as
  the sync daemon afterward.
- `ctx clone <url> [into]` — bootstrap a new node from an existing vault
  served by a listening node: authenticate, full catch-up (download the
  primitive DAG + objects), materialize the working tree, write local
  identity + config, and **record the source URL as a peer** (like git's
  `origin`, so a bare `ctx watch` here reconnects automatically).
  **Target directory:** with no `[into]` it creates `./<name>/` (the human
  name, falling back to a short id slug — never the raw opaque id, and
  never silently littering the current folder); `.` clones into the current
  folder; an explicit path uses that path. It refuses to clobber an
  existing vault. `--watch` transitions straight into the sync daemon after
  bootstrap (one command to clone-and-stay-synced); without it `clone`
  catches up and exits (git-clone semantics) and prints the exact `cd … &&
  ctx watch` next step.
- `ctx watch [--listen [addr]]` — **the primary long-running command.** Open
  the configured vault, watch the scoped tree (debounced auto-commit, with
  self-write suppression per §5.6), connect to its configured peer(s), and run
  the continuous realtime sync loop until stopped. `--listen` additionally
  accepts inbound peers (acts as a relay/hub). Bare `--listen` binds
  `0.0.0.0:9000` (an unprivileged default — *not* 443, which implies TLS the
  engine does not originate and is privileged); override with an explicit
  `addr`, `--port`, or `PORT`. Default transport is `wss://` (§10);
  `--no-tls` serves plaintext `ws://`. This is the everyday "keep this folder
  synced" daemon, and it emits operator-visible logging (peer connect /
  handshake outcome with reason / catch-up / integrate / commit) at `INFO`.
- `ctx key` — generate / show the node SSH key; print the public key in
  OpenSSH format; use an SSH agent if available.
- `ctx authorize <pubkey>` / `ctx revoke <pubkey>` — manage `authorized_keys`.
- `ctx status` — node identity, peers, sync state, head/`main` SHA.
- `ctx snapshot <name>` / `ctx restore <name|time>` — point-in-time recovery.
- `ctx log` — history (wraps / defers to the underlying git history).
- `ctx git <args…>` — **read-only** git inspection of CSP's engine-owned
  repository: a passthrough that sets `GIT_DIR=<scope>/.context/git` +
  `GIT_WORK_TREE=<scope>` and execs git, but **deny-by-default**: only an
  allowlist of read-only subcommands runs (`log`, `show`, `diff`, `status`,
  `blame`, `cat-file`, `ls-tree`, `ls-files`, `rev-list`, `rev-parse`, `grep`,
  `for-each-ref`, `describe`, `shortlog`, `reflog show`, …). Any mutating verb
  (`commit`, `checkout`, `switch`, `reset`, `merge`, `rebase`, `branch`/`tag`
  create, `gc`, `prune`, `update-ref`, `apply`, `cherry-pick`, `restore`,
  `clean`, `stash`, `fetch`, `push`, `filter-branch`, config writes, …) is
  **refused** with a pointer to the proper `ctx` command. Rationale: the repo
  is engine-owned (§4) — there is no legitimate write workflow through git
  (commit = edit files + `ctx watch`; restore = `ctx restore`; tag a point =
  `ctx snapshot`; gc is engine-internal, §9.2). The repo discovery path is
  `ctx git`, since there is no `.git` at the scope root (§4, §11). A user who
  bypasses this with raw `GIT_DIR=… git …` is unsupported and **will be
  clobbered** (§4).
- `ctx scope` — show / edit the synced scope and `.contextignore`.
- `ctx completions <bash|zsh|fish|powershell>` — emit shell completion.

Ergonomic requirements (treated as acceptance criteria, not nice-to-haves):

- `--help` on every command and subcommand; generated, accurate.
- Shell completion for bash, zsh, fish, and powershell.
- `--json` machine-readable output on read/status commands so agents and
  scripts can drive CSP non-interactively.
- No required interactive prompts; sensible, documented exit codes. Every
  deployment knob has **all three** forms — a CLI flag, a `CTX_*` env var, and
  a config-file key — with precedence **flag > env > config file** (§17.1). No
  knob is env-only or flag-only.
- Every engine capability (init, clone, watch/relay, identity, auth, status,
  snapshot, restore, scope) is reachable from the CLI.

### 17.1 Environment & deployment knobs

For headless / container / managed-platform deployment the CLI honors these
environment variables (CLI flags override env; env overrides the config file):

Every deployment knob is a **global option** (accepted on any subcommand, not
just `watch`) with **three-way parity** — a CLI flag, a `CTX_*` env var, **and**
a config-file key — resolved strictly **flag > env > config**. Precedence is
*non-destructive*: supplying a flag/env value does **not** rewrite the
persisted config file, and a flag/env value can override a config value in
*both* directions (a flag-supplied `false` overrides a config `true`, not only
the reverse). The **one documented exception** is the vault locator
(`--dir`/`CTX_DIR`) — it cannot have a config-file key because it *locates the
config file itself* (a config key for "where is the config" is circular), so it
is flag+env only, by nature.

- `--dir <path>` / `CTX_DIR` — the vault/scope root, **decoupled from the
  process working directory** *(env renamed from the misleading `CTX_CWD`: this
  is NOT the process cwd; `DIR` mirrors git's `GIT_DIR`)*. Set when the
  platform mounts persistent storage somewhere other than where the process
  starts — the classic cause of "state silently re-initialized on every
  deploy." Must always resolve to the persistent volume, never an ephemeral
  path. **Flag+env only** (the parity exception above): no config-file key.
- `--no-tls` / `CTX_NO_TLS` / config `no_tls` — serve a plaintext `ws://`
  listener instead of the default self-signed `wss://` (§10), for running
  behind a reverse proxy / edge that already terminates TLS, or on a
  trusted/local network.
- `--listen [addr]` / `--port <n>` / `PORT` / config `listen` — listen
  address/port for a listenable (native) node (`ctx watch --listen`). Bare
  `--listen` defaults to `0.0.0.0:9000` (unprivileged; deliberately not 443);
  `--port` / `PORT` (managed platforms inject `PORT`) or an explicit `addr`
  override it. A config-file `listen` value **actually starts a listener**
  (it is read, not merely stored), subject to flag > env > config.
- `--log <level>` / `CTX_LOG` / config `log` — log level / filter. Default
  surfaces operator-visible `INFO` (connections, handshake outcomes, catch-up,
  integrate, commits); `csp_core=debug` for protocol detail.
- `--debounce <ms>` / `CTX_DEBOUNCE` / config `debounce_ms` — auto-commit
  debounce in milliseconds (default 1000; §13.1/§14). `--debounce-ms` is kept
  as a hidden backward-compatible alias of `--debounce`.
- `--authorized-keys <keys|file>` / `CTX_AUTHORIZED_KEYS` — public keys
  (newline- or comma-separated, or a file path) merged into this node's
  **local** `authorized_keys` (`.context/authorized_keys`, never synced — §10)
  on startup, idempotently. (Not a persisted config key: it is merged into the
  side `authorized_keys` file, not the vault config — that *is* its parity
  form.) The supported way to pre-seed trust on a fresh hosted listener so the
  TOFU window never opens (§10).
- `--no-tofu` / `CTX_NO_TOFU` / config `no_tofu` — disable trust-on-first-use
  entirely (§10). Resolved flag > env > config **without** rewriting the
  persisted config (the prior "sticky" behavior — flag silently persisting
  `no_tofu=true` so it could never be turned back off — is disallowed).
- **General rule:** every deployment knob has *all three* of a `--flag`, a
  `CTX_*` env var, and a config-file key — **except** the vault locator
  (`--dir`/`CTX_DIR`, flag+env only, structurally) and `--authorized-keys`
  (whose persisted form is the `authorized_keys` side file, not vault config).
  No knob is env-only or flag-only. This is a requirement, not a coincidence.

A hosted listener is thus fully configurable by flags *or* env, with no file
editing or container-command override — e.g.
`ctx watch --listen --no-tls --dir /data/vol --authorized-keys "$KEYS"`, or the
exact `CTX_*` equivalents.

---

## 18. Testing, verification & cross-surface parity

Correctness here is a release gate, not aspirational. CI must run all of the
below green before any release.

- **Unit tests (`csp-core`).** Object model, total order, version vectors,
  deterministic fold, conflict resolution, scope filtering, the auth handshake,
  catch-up.
- **Determinism conformance suite** (guards §5.4 / §13.2). N simulated
  independent nodes; identical primitive commits fed in shuffled orders; assert
  identical `main` SHA *and* identical tree. A hard build gate.
- **End-to-end tests.** Spawn multiple real `ctx` processes including a
  listening relay; exercise create / modify / delete / rename, empty
  directories, disjoint *and* overlapping concurrent edits, offline→reconnect
  catch-up, snapshot/restore. Assert convergence (identical `main` SHA and
  byte-identical working trees on every node), PITR correctness, and that the
  result is genuinely git-coherent (an unmodified `git` can `log`/`checkout`
  it).
- **Cross-surface interop.** A wasm/TS node must interoperate with a native
  node — handshake, replication, identical convergence and SHAs. This is the
  guarantee that the wasm path is bindings over the same core, not a divergent
  reimplementation. Includes a **byte-identity vector check** (wasm output ==
  shared test vectors == live `ctx`) and a **SDK⇄real-`ctx` parity** e2e
  (spawn the real binary; assert bidirectional convergence). Host-plugin
  behavior is validated through the TS SDK e2e harness.
- **Differential-equivalence oracles (build gate).** Every hand-rolled
  substitute for a dropped general-purpose dep must prove equivalence to the
  original, kept dev-only: the scope matcher byte-for-byte vs the former
  `regex` implementation over tens of thousands of generated path/pattern
  cases; the config codec round-trips every config and emits TOML the real
  `toml` parser reads back identically. "We didn't change behavior by
  shrinking the engine" is a passing test, not an assertion.
- **No-regression / parity requirement.** The CLI + SDK must be at least as
  capable and ergonomic as a mature reference sync tool: full command coverage,
  scriptable (`--json`), shell completions, robust SSH-key auth, snapshot/
  restore. This is verified by the e2e suite exercising the *entire* command
  surface — “we didn't lose anything” must be a passing test, not an assertion.
