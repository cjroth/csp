# Architecture Direction — sync engine

**Status:** Recommendation / direction (supersedes the earlier draft of this file)
**Scope:** The CSP sync engine — `crates/csp-core`, `csp-wasm`, the TypeScript SDK,
and the Obsidian plugin.

This is the conclusion of a design review that started from a concrete bug — a
folder deletion that wouldn't sync from Obsidian — and worked outward to "what
should the engine actually be," including a survey of every existing tool that
might replace it. It is meant as a standing reference for the next round of work.

---

## TL;DR

Do **not** replace the engine, adopt Automerge, or "switch to full git."
Research confirmed there is **no off-the-shelf tool that fits** (see "Prior
art"). The engine's *instincts* are right for our constraints; two specific
choices hurt us, and they are **separable**:

1. **The fold's implementation** materializes the *whole vault tree* three times
   per merge step (`fold.rs:580`). That is the O(vault) cost — an implementation
   defect, **not** a consequence of whole-tree merge (git keeps whole-tree
   commits and stays O(diff) via structural sharing + path-scoped diff).
2. **Deletes are modelled as "path absent from the tree,"** which is what makes
   them ambiguous and resurrection-prone.

**Direction:** keep the deterministic, order-independent merge **and** the
whole-tree commit/snapshot model (both are right for this use case), and fix the
two defects — make the fold **diff-based / structural-sharing**, and make
**deletes explicit (tombstones)**. Improve the wire and persistence layers.
Bring in true peer-to-peer transport (**iroh**) as **Phase 2**, after the engine
fixes land over the existing transport.

---

## Requirements (what drives every decision)

- Agent context syncing between sessions; agents may work in parallel, **ideally
  on disjoint files** (true same-file concurrency is rare).
- **Point-in-time rollback** to any past state — as a coherent **whole-vault**
  snapshot, with a git-like history/log/diff view.
- **Offline-first** — no remote round-trip to view/edit.
- **Automatic** version control (the agent never manually commits/pushes).
- **Efficient at scale**: many small fast changes, many files, network-efficient
  on mobile/flaky links, low-latency. Target **≤ 1 s** propagation.
- **Peer-to-peer**, no privileged central server. An always-on hub is fine **so
  long as it is a node like any other**, not a special protocol endpoint.
- Optimized for Markdown / plain text; other formats supported.
- **All nodes are trusted.** No E2EE or hardware-key isolation required.

---

## Key conclusions from the review

### 1. The deterministic merge is the right property — keep it

`main = f(set of changes)`, independent of arrival order, is exactly what
optimistic P2P needs: every node converges to identical state **and history**
without coordination. **Git's 3-way merge cannot provide this** — it produces
conflicts and is not associative/commutative (merge order changes the result).
This is the genuine reason the custom fold exists, and it is sound.

### 2. The performance bug and the "per-file vs whole-tree" question are separate

They *feel* linked (both touch "the whole vault") but they're orthogonal:

- **Performance** is about *how the merge is computed.* `compute_main` reads
  every file into `path→bytes` maps ×3 per merge step. Git proves whole-tree
  commits can be O(diff): diff base→ours / base→theirs, touch only changed
  paths, share unchanged subtrees by hash. Fixable **without** changing the
  merge unit.
- **Per-file vs whole-tree** is a *semantic* choice about the unit of history
  and atomicity — independent of the perf fix.

**Fix the perf bug regardless; decide granularity on its own merits.**

### 3. For this use case, whole-tree commits win

Three things all point the same way:

- **Interdependent files** → whole-tree commits give **atomic multi-file
  consistency**: a note and the note it links to land together. Per-file
  registers merge each path independently, allowing transient dangling links
  during a sync window.
- **Rollback** → a commit *is already* a whole-vault snapshot, so "state as of
  T" is "check out one commit" (O(1) to identify). Per-file registers would
  require reconstructing every path's value at T. (The earlier register proposal
  is the weaker fit here, which is why this doc leads with whole-tree.)
- **`ctx git`** → the native node maintains a real on-disk `.git`, and `ctx git`
  is a read-only, deny-by-default passthrough to real `git` (`gitpass.rs`):
  `log`, `reflog show`, `diff` against a real commit DAG. **That feature exists
  because there is a commit DAG.** Per-file registers don't have one.

Per-file registers become the thing to reach for **only if** parallel
disjoint-file concurrency becomes common — which the requirements say it won't.

> Corrections to earlier claims in this review: CSP **does** have working history
> (`createSnapshot` / `restoreToSnapshot` / `restoreToTime` + the primitive DAG);
> the gap is no compaction and snapshot/time-based rather than every-commit
> addressable. And the native node **does** keep a real on-disk `.git`.

---

## What we keep / change / lose

**Keep:** the deterministic merge; whole-tree commit/snapshot semantics;
content-addressed blobs; the slim "git-compatible" object store (we are **not**
pulling in gitoxide — wasm bloat; the value was always git's *semantics*, not the
library); the host/engine boundary and the Obsidian bridge
(`plugins/obsidian/src/bridge.ts` — its suppression-set, reconcile, atomic
writes, `.keep` handling); offline-first; automatic debounced commit; the CLI
(`ctx git` and friends survive because the commit DAG survives); the interop
**test vectors** (the regression net).

**Change:** the fold internals (diff-based / structural-sharing); delete
semantics (explicit tombstones); persistence (incremental); framing (bound every
frame); add history compaction; replace closure-exchange anti-entropy with
range-based set reconciliation; (Phase 2) transport.

**Lose:** very little on this route — mainly migration/regression risk on the
fold rewrite, guarded by the test vectors. The signed "verified-not-trusted"
fold is moot under the trust assumption (it forecloses an untrusted-peer future,
which we've decided we don't need).

**Migrate, do not rewrite.** The valuable parts (bridge, store, CLI, `ctx git`,
SDK, test vectors, edge-case knowledge: cold cache, suppression, empty folders,
ghost-add) are reusable and orthogonal to the fold fix. A from-scratch rewrite
re-derives the same lessons (second-system trap). Replace the engine internals
behind the existing test vectors.

---

## What to incorporate from the research

Ranked by value-to-effort. "Use it" vs "study it" is called out.

1. **Range-based set reconciliation (RBSR)** — *use it.* (iroh-docs / Willow /
   nostr's Negentropy; Aljoscha Meyer's paper.) Replaces the
   `FrontierDigest → WantTips → export_closure` anti-entropy (which re-ships
   whole closures — the redundant `integrated=0` flood) with an O(log n)
   round-trip, small-message, **resumable** reconciliation. Directly targets the
   worst real symptom: catch-up over slow/lossy mobile links. Do this regardless
   of other decisions.
2. **Sedimentree-style history compaction** — *use the design.* (Automerge's
   Beelay.) Recursively compress older history into coarser chunks, keep recent
   history fine-grained. The answer to unbounded history growth (issues
   0009/0011). Borrow the design; Beelay itself is not production-ready.
3. **BLAKE3 + verified, resumable streaming for blobs** — *use it.* (iroh-blobs
   / hypercore.) Chunk-verified, resumable transfer so a large attachment over a
   flaky link never restarts from zero and never delivers a corrupt blob. Pairs
   with frame chunking.
4. **Pijul's patch commutativity** — *study it.* (`libpijul`, Rust.) The
   rigorous treatment of the exact property our fold provides (apply in any order
   → identical state + history). Read it to validate the fold's guarantees;
   don't adopt (it's line-level + batch).
5. **Append-only signed log as history substrate** — *validation.* (hypercore.)
   Confirms our commit-DAG instinct: lean into append-only + content-addressing
   for cheap, tamper-evident, replayable history (cheap R2).

**Do not incorporate:** iroh-docs' LWW document layer (can't give whole-vault
rollback; de-emphasized by n0); any server-required platform (Jazz / InstantDB /
ElectricSQL / PowerSync — fail the no-central-server requirement); a per-file
CRDT as the substrate (loses native whole-vault snapshots, adds intra-file merge
we don't need).

---

## Transport & real-time

### Phase 1 — keep the existing WebSocket-to-hub transport, fix the model on top

- **Push changes optimistically** (send the change, don't notify-then-fetch).
  Because merge is deterministic, the receiver applies immediately and still
  converges. This is what the current `Live` push gets right — keep it, but
  **bound every frame** (today only catch-up `Objects` are chunked to 256 KiB; an
  unbounded `Live`/relay frame — the observed 4.8 MB inbound — kills the
  connection).
- **The hub is an always-on peer**, running the same node software. Because the
  unit of exchange is a change (not a merged result) and merge is deterministic,
  the hub **forwards the change to peers immediately** (≈ zero added latency) and
  **merges into its own replica asynchronously** — *forward-then-merge*, never a
  serialization bottleneck.
- WebSockets are sufficient here: all traffic flows through the publicly
  reachable hub. Simpler, and we already have it.

**Latency budget (small edit):** debounce (~100–300 ms) + commit (~5 ms) +
push RTT + apply (~5 ms) ≈ 250–350 ms on a good link, well under 1 s on mobile.

### Phase 2 — iroh for true peer-to-peer

WebSockets can only be "P2P" by relaying through a publicly reachable server —
they **cannot** open a direct connection between two devices that are both behind
NAT (neither can accept an inbound connection). That direct path is what iroh
adds.

**NAT hole punching (why direct P2P is hard):** consumer devices have no public
IP — they sit behind a router that only lets inbound packets in as *replies* to
outbound traffic. Two NATed devices therefore can't connect directly. Hole
punching fixes this: both peers connect *out* to a shared rendezvous server
(punching a temporary hole in each NAT), the server tells each peer the other's
observed public address, and both fire packets at each other simultaneously so
each NAT accepts the other's packet as a reply. The rendezvous server then drops
out of the data path. When a NAT is too strict, traffic falls back to a relay.

**What iroh buys over WebSockets:**

- **Direct device-to-device connections through NATs** (hole punching) — two
  phones on the same Wi-Fi sync directly, no server in the path.
- **Dial by public-key identity**, not IP/URL — iroh finds the path (a roaming
  phone has no stable address for a WebSocket URL).
- **Automatic relay fallback + upgrade** to direct when possible (WebSockets only
  ever have the one path: through the server).
- **QUIC connection migration** — the connection survives a Wi-Fi→cellular
  switch (a WebSocket/TCP connection drops and must reconnect).

**Why Phase 2, not Phase 1:** the engine fixes (fold, deletes, persistence,
framing, RBSR) are independent of transport and recover the system over what we
already run. iroh is the upgrade that makes the hub *genuinely just a peer*
(used only as relay-of-last-resort + store-and-forward) and unlocks LAN-speed
direct sync — valuable, but not required for correctness. Use iroh's transport +
gossip + blobs; **not** the iroh-docs layer.

---

## Scale

- **Many small fast changes** — good once **incremental persistence** lands
  (issue 0011); the current 3.3 MB `to_bytes()` per commit is the limiter, not
  the merge.
- **Many files** — good once the fold is **diff-based / structural-sharing**
  (same fix as the perf bug). Git handles 100k-file repos; materialization is the
  problem, not the model.
- **Many nodes** — fine for the realistic count (a user's devices + a few agents
  = tens). Content-addressing gives idempotent dedup; admitted-gated relay
  terminates gossip. Honest ceiling: hub fan-out is O(nodes)/change and all-pairs
  reconciliation grows — *thousands-of-nodes* swarms would need rethinking, but
  that's not this use case.
- **History growth** is the real scale limit — unbounded without compaction.
  Sedimentree-style retention horizon is the fix.

---

## Prior art — why nothing is "adopt wholesale"

Research evaluated iroh/iroh-docs, Radicle, Willow/Earthstar, Automerge
(+ Beelay), Yjs, Syncthing, Anytype, Obsidian Sync/Git, Logseq, Pijul, Dolt,
Holepunch/hypercore, Ditto, Fireproof, PouchDB/Couchbase, Jazz, InstantDB,
ElectricSQL, PowerSync, TinyBase.

**The combination that kills every candidate:** deterministic convergent merge
(R1) **+** whole-*vault* point-in-time rollback with git-like history (R2) **+**
serverless P2P (R6), on a Rust/WASM core. Each tool nails at most two:

- **Whole-vault R2 is native only in commit-DAG systems** (git / Radicle). Every
  per-file CRDT/log model (Automerge, iroh-docs, hypercore, Syncthing) gives
  per-document/per-log history, so whole-vault snapshots are bolt-on. *This is
  exactly why we keep the commit DAG.*
- **Radicle** = P2P git (R2 + R6) but inherits git's non-deterministic merge
  (fails R1) and is batch/manual (fails automatic + realtime).
- **Pijul** = the one tool with genuine R1 (patch commutativity), Rust — but
  line-level and batch.
- **Automerge 3** = best CRDT history + Rust/WASM, but per-document (whole-vault
  snapshot is bolt-on) and bring-your-own P2P.
- **iroh-docs** = LWW (no R2), and de-emphasized by n0 (spun out in iroh 0.28,
  Nov 2024; "no longer get special treatment").
- **Server-required platforms** (Jazz, InstantDB, ElectricSQL, PowerSync) fail
  the no-central-server requirement outright.

**Conclusion:** build on our own commit-DAG engine; borrow algorithms and
transport as building blocks. Confidence high — reached independently across
multiple searches with dated primary sources.

Key sources: iroh 0.28 "Let them have crates"
(https://www.iroh.computer/blog/iroh-0-28-let-them-have-crates);
Automerge 3 (https://automerge.org/blog/automerge-3/);
Pijul patch theory (https://pijul.org/manual/theory.html);
Radicle protocol (https://radicle.dev/guides/protocol);
Holepunch autobase (https://docs.pears.com/building-blocks/autobase/);
Syncthing versioning (https://docs.syncthing.net/users/versioning.html).

---

## Phased plan

### Phase 1 — fix the engine over the existing transport (low risk, high value)

1. **Diff-based / structural-sharing fold** — stop materializing the whole tree
   in `compute_main`; merge only changed paths. (Fixes the O(vault) cost.)
2. **Explicit deletes / tombstones** — resurrection becomes structural. Interim:
   wire issue 0014 **Layer 2** (`mark_bootstrap_pending`) into `csp-wasm` + the
   SDK `clone()` / state-loss `open()`. It is currently only wired in the native
   `ctx clone` CLI (`crates/ctx/src/main.rs:593`); the wasm/Obsidian path runs on
   Layers 1 + 3 only, so a new Obsidian node can still republish deleted paths
   during catch-up.
3. **Incremental persistence** (issue 0011) — stop the per-commit 3.3 MB write.
4. **Bound every frame, including `Live`** (fixes the oversized-frame disconnect).
5. **Range-based set reconciliation** for catch-up (replaces closure exchange).
6. **History compaction / retention horizon** (sedimentree-style; issue 0009).

### Phase 2 — true peer-to-peer transport

7. **iroh** (transport + gossip + blobs): NAT traversal, relay fallback, dial-by-
   key, connection migration. Makes the hub genuinely just-a-peer and unlocks
   direct LAN-speed sync. Keep WebSocket-to-hub as fallback during rollout.

### Phase 3 — optional / future

8. Per-file register granularity **only if** parallel same-file concurrency
   becomes common. E2EE / signed authorship **only if** the trust assumption
   reverses.

---

## Open questions

- **Whole-tree commits vs. per-file histories** — resolved toward whole-tree for
  this use case (atomicity + rollback + `ctx git`). Revisit only if same-file
  concurrency turns out to be common.
- **Compaction vs. "rollback to any change"** — confirm the retention horizon
  keeps enough fine-grained recent history for real rollback needs while bounding
  growth.
- **RBSR over the existing wire vs. waiting for iroh** — RBSR is transport-
  agnostic and belongs in Phase 1; confirm it slots cleanly onto the current
  WebSocket framing before Phase 2.
