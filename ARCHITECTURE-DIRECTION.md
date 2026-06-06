# Architecture Direction — sync engine

**Status:** Recommendation / direction (not yet scheduled)
**Scope:** The CSP sync engine (`crates/csp-core`, `csp-wasm`, the TypeScript SDK,
and the Obsidian plugin).

This document captures the conclusion of a design review that started from a
concrete bug — a folder deletion that wouldn't sync from Obsidian — and worked
outward to "what should the engine actually be." It is meant as a standing
reference for the next round of work.

---

## TL;DR

We do **not** need to throw the architecture away, and we do **not** need
Automerge or "full git." The instincts the engine was built on are correct for
our constraints. Two specific choices are what hurt us:

1. **Representation:** `main` is computed by folding the **whole vault tree**
   (`compute_main` materializes every file ×3 per merge step), and a delete is
   modelled as "path absent from the tree." This is the source of both the
   O(vault) cost *and* the delete-resurrection ambiguity.
2. **Wire/persistence execution:** `Live` frames are unbounded, persistence
   rewrites the entire 3.3 MB state on every commit, catch-up re-ships closures
   the peer already has, and there is no history compaction.

**The direction:** keep the deterministic, order-independent merge (it is the
right property and git's merge cannot provide it), but change the
representation from *"fold whole trees"* to *"merge a map of per-file
LWW-registers with explicit tombstones."* Push changes optimistically over a
real-time transport; keep an always-on **peer** (not a privileged server) for
NAT traversal and store-and-forward. This preserves convergence while making
the engine O(diff), delete-safe, and mobile-friendly.

---

## Context: the requirements that drive this

- Agent context syncing between sessions; multiple agents may work in
  parallel, **ideally on disjoint files**.
- **Point-in-time rollback** to any change in history.
- **Offline-first** — no remote round-trip to view/edit.
- **Automatic** version control (agent never manually commits/pushes).
- **Efficient for large projects, network-efficient on mobile, low-latency,
  real-time.** Target: **≤ 1 s** propagation between devices.
- **Peer-to-peer**, no privileged central server. An always-on hub is fine
  **so long as it is a node like any other**, not a special protocol endpoint.
- Optimized for Markdown; other formats supported.
- **All nodes are trusted.** No end-to-end encryption or Secure-Enclave key
  isolation required. *(This assumption is load-bearing — see "Decided
  against.")*

---

## What we keep (these were the right calls)

- **Deterministic, order-independent merge.** `main = f(set of changes)`,
  independent of arrival order, is exactly what optimistic P2P needs so every
  node converges to identical history without coordination. **Git's 3-way merge
  cannot give this** — it produces conflicts and is not associative/commutative
  (merge order changes the result). Keep the property.
- **Content-addressed blobs.** Integrity + dedup. The register just points at a
  content hash.
- **The host/engine boundary and the Obsidian bridge** (`plugins/obsidian/src/bridge.ts`).
  The suppression-set, materialize-vs-user-edit reconcile, atomic writes, and
  `.keep`/empty-folder handling are hard-won domain knowledge. Port them.
- **Offline-first** and **automatic debounced commit** (the "git but automatic"
  requirement — the bridge + commit debounce already nail it).
- **The slim, "git-compatible" object store.** See "Object store" below — we are
  *not* pulling in gitoxide.

## What changes

### 1. Core: deterministic merge over per-file registers, not whole-tree fold

Replace the content-defined whole-tree fold with a **state-based CRDT at file
granularity**:

- Each path → an **LWW-register**: `{ content_hash, hlc_timestamp, author_id }`,
  with explicit **tombstones** for deletes.
- **Merge = per-key resolution:** highest HLC wins; ties broken by
  `author_id`/hash. Deterministic, total, order-independent, idempotent — the
  *same math* as the current fold, computed per-key instead of per-whole-tree.
- A change is one tiny op: `path + content_hash + hlc + author`. The blob ships
  separately and is dedup'd.

This yields, all at once:

- **Deterministic convergence** (the fold's real purpose) at **O(diff)** instead
  of O(vault) — fixes the `compute_main` whole-tree materialization cost.
- **First-class deletes** — a tombstone carries its own HLC, so a stale re-add
  loses to the delete. The resurrection class of bugs (issue 0012) goes away
  *structurally*, not via three layers of guards.
- **Parallel disjoint agents cost nothing to merge** — different paths never
  interact.
- **History/rollback** — the per-key log of register updates is the history.

**Same-file concurrent edits** resolve last-writer-wins (loser retained in
history, recoverable). This is not a regression: the current fold already
*defers* rather than line-merges, and agents are meant to be disjoint.

### 2. Transport & real-time: optimistic push, hub-as-peer

- **Push changes optimistically**, don't notify-then-fetch. Send the op (and the
  thin blob/packfile) in one shot. Because merge is deterministic, the receiver
  applies it immediately and still converges. This is what the current `Live`
  push gets right — keep it.
- **Transport over a QUIC P2P layer (iroh)** for hole-punching + relays. On a
  LAN this gives sub-RTT device-to-device sync that beats any cloud round-trip.
- **The hub is an always-on peer**, running the *same node software*. It is
  privileged only by being always-reachable and holding a full replica (for
  store-and-forward when both peers are offline). That is a deployment property,
  not a protocol special-case — the same full-node/thin-node split we already
  have.
- **The hub forwards, then merges — not merge-then-forward.** Because the unit
  of exchange is a signed change (not a merged result) and merge is
  deterministic, the hub **fans out the raw op to peers immediately** (≈ zero
  added latency) and merges into its own replica **asynchronously**. The hub is
  never a serialization bottleneck.

**Latency budget (small edit):** `debounce (you choose, ~100–300 ms)` +
`commit (~5 ms)` + `push RTT` + `apply (~5 ms)`. On a good link ≈ 250–350 ms;
on a poor mobile link, well under 1 s. (Note: a *git-fetch* model adds 1–3
want/have negotiation round-trips — the reason to push the payload eagerly
rather than notify-then-pull.)

### 3. Deletes: tombstones (and finish wiring 0014 in the interim)

- **Long-term:** explicit tombstone ops (above) make resurrection impossible by
  construction.
- **Interim (current engine):** the issue 0014 ghost-add guard is real but
  **incompletely wired on the Obsidian/wasm path**. Layers 1 (pre-publish
  quarantine) and 3 (fold-side backstop) run everywhere, but **Layer 2
  (bootstrap deferral) is only wired in the native `ctx clone` CLI**
  (`crates/ctx/src/main.rs:593`). `csp-wasm` doesn't expose
  `mark_bootstrap_pending`, and the SDK's `clone()` never calls it — so a new
  node joining via Obsidian can still republish deleted paths during the
  catch-up window (exactly where Layer 3 "fail-opens on partial closures").
  **Fix:** expose `mark_bootstrap_pending` in `csp-wasm` and call it from the
  SDK `clone()` (and on a state-loss `open()`), clearing on the first full
  `ObjectsBatch` integrate as the native side already does.

### 4. Persistence, framing, compaction (network/large-project efficiency)

- **Incremental persistence** (issue 0011): append changed objects instead of
  rewriting 3.3 MB `to_bytes()` per commit.
- **Bound every frame**, including `Live`. Today only catch-up `Objects` are
  chunked (256 KiB); an unbounded `Live`/relay frame (the observed 4.8 MB
  inbound) kills the connection. Chunk live pushes too.
- **Range-based set reconciliation** for catch-up (what iroh-docs/Negentropy
  use) instead of re-shipping whole closures — O(log n) round trips, tiny
  messages, resumable across drops. Directly fixes the redundant `integrated=0`
  flood.
- **History compaction / retention horizon** (issue 0009 fold-in): keep
  fine-grained recent history for rollback; squash/GC older history so the
  known set and catch-up payloads stop growing unboundedly.

### 5. Object store / git compatibility — no gitoxide

The slim "implement-the-parts-we-need, call it git-compatible" decision was
correct; pulling gitoxide back in for a mobile/Electron bundle is a bad trade
(wasm size). The value was never the *git library* — it was git's **merge and
sync semantics**, which we implement ourselves. The *only* thing that requires
real on-disk git compatibility (not just format-compatibility) is **external
tooling** (`git log`/`bisect`/`blame`, pointing GitHub at the vault). We do not
get that today anyway (we persist via `to_bytes()`, not an on-disk `.git`), so
it is an explicit, optional choice — not a reason to adopt full git.

---

## What we explicitly decided against (and why)

- **Adopt Automerge (or Yjs) wholesale.** It solves deletes and delta-sync for
  free, but (a) we'd treat files as opaque blobs and use ~20 % of it, (b) it has
  no built-in signed authorship (irrelevant now, but it shows the fit is
  partial), and (c) it's a rewrite. A simple per-file LWW-register CRDT gives us
  the parts we need without the sequence-CRDT machinery. *Reach for Yjs only if
  live collaborative editing of the open note becomes a goal — scoped to that
  one document, never as the whole-vault substrate.*
- **"Switch to full git" / gitoxide.** See "Object store." We want git's merge
  *semantics*, not the dependency.
- **Server-ordered op-log (CouchDB-style).** Cleanest option **if** a server is
  always in the path — but "P2P, no central authority" + "offline-first between
  devices that may never be co-online" rules out a single ordering authority.
  Dead on arrival given the requirements.
- **Plain git remote as the hub.** Insufficient for real-time: git has no native
  "a commit landed, fetch now" push, so peers would poll — battery/radio-hostile
  on mobile. Real-time requires a notify/gossip channel, i.e. a daemon. (It can
  still be the same node binary, deployed always-on — so it stays a peer, not a
  special server.)

---

## Sequenced plan

**Tier 1 — recover the current architecture (low risk, high value).** These
help today regardless of the larger migration:

1. Wire issue 0014 **Layer 2** into `csp-wasm` + SDK `clone()`/empty-`open()`
   (fixes new-node resurrection on Obsidian).
2. **Chunk `Live` frames** (fixes the oversized-frame disconnect loop).
3. **Incremental persistence** (issue 0011) — stop the per-commit 3.3 MB write.
4. Cap the fold to **changed sub-trees** instead of materializing the whole tree
   in `compute_main` (mitigates O(vault) merge).
5. **History compaction / retention horizon** (issue 0009 fold-in).

**Tier 2 — the architectural change.** Migrate the merge representation from
whole-tree fold to the **per-file LWW-register CRDT + tombstones**, with
optimistic op push over iroh and range-based reconciliation for catch-up. Keep
the object store, the host bridge, and the deterministic-merge guarantee.

---

## Open questions

- **Whole-tree commits vs. per-file histories.** Per-file (the register model)
  is best for parallel disjoint agents and is the recommendation. Whole-tree
  commits are simpler and keep one history DAG; given "agents ideally don't touch
  the same file at once," a deterministic resolver over whole-tree commits may be
  *enough* and less work. Decide based on how often true same-file concurrency
  actually occurs.
- **Rollback granularity in the register model.** Per-key update logs give
  per-file history naturally; confirm the global "state as of time T" query and
  compaction story are as clean as the current snapshot/DAG approach before
  committing (this is the one area the commit-DAG has a natural advantage).
- **iroh-docs vs. a thin custom layer over iroh-blobs + iroh-gossip.** Evaluate
  the current maturity of iroh-docs against building the register layer
  ourselves on iroh's connectivity + blob primitives.
