# Sync Engine — Design Spec

> Successor to **csp** (Context Sync Protocol). This revision incorporates the
> decisions from the design review: keep csp's proven **line-level 3-way merge**
> for text (no CRDT default), order the fold by a **Lamport logical clock** while
> **recording wall-clock timestamps for point-in-time recovery only**, keep the
> **one-engine-everywhere wasm** structure, and keep csp's **stock-git-compatible
> read-only derived history** — all on a new **SQLite/event-log substrate** that
> fixes csp's O(whole-vault) performance class.

## Intent

We are building the storage and sync layer for **agent context and a human's
Markdown vault**, shared across a person's devices and across agent sessions. An
agent should work on one machine, close the session, and resume the same context
on another; a human should edit notes in Obsidian on a phone and a laptop and
have them converge; and either should be able to **roll the workspace back to any
earlier moment** without ceremony.

The shape is git's — a versioned, content-addressed history of files — but git's
*operation* is wrong for it. Git is manual (commit + push), batch (sync on
demand), and its merge needs a human. We need the opposite: **automatic** (capture
and sync on change, no explicit commit), **real-time** (≈1s propagation),
**peer-to-peer** (no central authority in the path), and **convergent without a
human** (every node deterministically reaches the same state). One line: **git's
content-addressed storage, with an automatic, deterministic merge, on an embedded
database.**

This is the direction csp already proved works — a signed event log, a
deterministic fold, real-time push, frontier anti-entropy, automatic debounced
commits, P2P with the hub as a peer. The successor keeps that spine and changes
three things csp got wrong or expensive: the **substrate** (git object model →
SQLite, to kill the per-edit O(whole-vault) cost), the **ordering key**
(make timestamps a recorded-for-PITR field, not the merge sort key), and the
**code-conflict policy** (surface conflicts an agent can fix rather than silently
dropping a side).

Two consequences are deliberate. First, **no atomic multi-file commits** —
continuous sync means a peer's view is always a partial cut, so we stream changes
as they happen and accept brief cross-file inconsistency (a link dangling for a
second) as the price of liveness. Second, **all nodes are trusted** (one person's
devices + their agents), so there is no end-to-end encryption and no per-author
signing requirement. That assumption removes a lot of machinery and is
load-bearing — if it ever reverses, parts of this design change. (Rows remain
Merkle-id'd and therefore tamper-evident regardless.)

## Goals

- **Agent context continuity** — the same working state across sessions and
  devices.
- **Instant point-in-time rollback** — jump to the vault as of any past moment,
  immediately, without replaying from scratch.
- **Real merge where it helps** — concurrent edits to different regions of a text
  file both survive (line-level 3-way); concurrent edits to the same region of
  code surface a conflict the agent can fix. No silent data loss for the cases
  that matter.
- **Offline-first** — a full local copy; reads/edits never wait on the network.
- **Automatic version control** — the system captures and syncs on change; the
  agent/user never runs commit/push.
- **Efficient & real-time at scale** — many small fast changes, many files,
  network-frugal on flaky/mobile links, ≤ ~1s propagation. A single-file edit
  costs work proportional to that file, never to the whole vault.
- **Peer-to-peer** — no privileged server. An always-on hub is allowed only as
  *a peer like any other* (relay + store-and-forward), never a special endpoint.
- **One engine everywhere** — the deterministic fold/merge compiles to `wasm32`;
  a browser/Obsidian node computes byte-identical state to a native daemon.
- **Markdown-first**, other formats supported. Text and code both get **3-way
  merge**; prose resolves to a clean single file, code surfaces a conflict,
  binaries get whole-file last-writer-wins.

## Core model

- **Event-sourced.** One **append-only global log of all changes to all files** is
  the source of truth. Current files, any past state, the derived git history, and
  search indexes are all **pure functions of the log**.
- **Deterministic by re-fold.** The log has a **total order** —
  `(lamport, site_id, content_hash)` — and state is the deterministic fold of the
  log in that order. The order is **wall-clock-free**: it is a function only of
  data that travels with each row, so any two devices holding the same set of rows
  compute the identical state. Eventually consistent and deterministic.
- **Timestamps recorded, never sorted.** Every row carries an authoring wall-clock
  `ts`, used **only** for point-in-time queries and display — never as the fold
  order or the conflict tiebreak. (See *Clocks & ordering*.)
- **Merges are derived, not synced.** Only genuine edit rows cross the wire. The
  merged state the fold produces is recomputed identically on every node, so it is
  never transmitted.

## The merge model (the centerpiece)

There is **one merge engine — 3-way merge against the last common ancestor** —
applied by folding the per-path diffs in the global total order. What varies by
file type is the **conflict policy**, not the merge algorithm:

- **Text / Markdown → 3-way region merge, clean-resolve.** This is csp's proven
  algorithm. Concurrent edits to **different lines/regions both survive**; edits to
  the **same region resolve deterministically** by sort position in the total
  order, with the **losing side kept in history** (recoverable, never on disk).
  **No conflict markers** — the materialized file is always a clean, single,
  coherent version. Line-level granularity is sufficient for human+agent
  collaboration: with ~1s sync the window for *same-line* concurrency is tiny, and
  different-region edits already both survive.

- **Code → 3-way against the LCA, conflict-surface.** Same 3-way merge, but a
  same-region conflict is **made visible for the user's agent to resolve** rather
  than silently dropped — silently losing a function is far more dangerous than a
  dangling note. Agents are good at exactly this. The representation (deterministic
  in-file markers vs. side-by-side conflict-copies) is an open question below;
  whichever is chosen must be **byte-deterministic** so every node produces
  identical bytes.

- **Binary / non-text → whole-file last-writer-wins** by `(lamport, content_hash)`.

**Convergence.** This works *because* the order is global, not pairwise. In a
3-way merge, "ours vs theirs" is decided by **sort position in the shared total
order**, identical on every node — not by local perspective — so every device
computes byte-identical results. To make 3-way output fully deterministic we fix:
(a) a **canonical ordering** of operands (by the total order), (b) **deterministic
conflict representation** for code (fixed labels like `site:A`/`site:B`, never
`HEAD`/branch names), and (c) **pinned merge heuristics** (conflict style, rename
detection, whitespace). With a **deterministic LCA selection** (criss-cross
multi-base broken by lowest content-hash), two nodes with the same rows produce
byte-identical merged bytes.

**Why not a CRDT for text?** Considered and **deferred**, not adopted. csp's 3-way
merge is *already line-level* — "at least line level" is what we have today. A
sequence CRDT (Automerge/Yrs) only adds **character-level same-line concurrent
survival**, which is rare under ~1s sync, and it costs: a heavy wasm dependency,
tombstone blow-up under agent *wholesale-rewrite* (the common case in an agent
vault — modeled as delete-all + insert-all), and a muddier point-in-time story
(CRDT state has no clean wall-clock order, so historical reconstruction needs
change-replay + periodic doc checkpoints). Keeping 3-way for text also means text
**participates in the global total-order fold** (a CRDT would sit outside it) and
keeps point-in-time trivially crisp. A CRDT is reserved as a **possible per-path
opt-in** for genuinely live human+agent co-edited files — and if it is ever added,
**Yrs** (lighter, faster, wasm-proven; we own history via the log so we don't need
Automerge's heavier built-in history), version-pinned in the synced routing
config, not hand-rolled (sequence-CRDT correctness is a research minefield).

## Why this merge model (and not the others)

We considered, in order, and rejected as defaults:

- **Whole-file LWW everywhere** — trivially convergent, but silently clobbers
  concurrent edits to different regions of the same text file. Kept only as the
  binary regime.
- **A whole-vault structured CRDT (Automerge/Yjs adopted wholesale)** — we'd use a
  fraction of it, and it frankenmerges *code* into files that don't compile.
- **A per-file text CRDT as the default** — deferred (see above): the increment
  over csp's line-level 3-way is narrow (same-line concurrency only), the cost is
  real (wasm weight, tombstone growth under agent-rewrite, fuzzier PITR), and it
  pulls text out of the global fold for no gain on the common case.
- **csp's silent loser-to-history for *code*** — fine for prose, wrong for code:
  dropping a concurrently-edited function out of the worktree is invisible data
  loss. Code surfaces the conflict instead.

The throughline: **deterministic order + content-addressed history is git's good
half; the automatic, convergent, human-free merge is the half git lacks.** csp
proved the fold; the successor moves it onto a substrate that scales and fixes the
ordering and code-conflict choices.

## Clocks & ordering

- **Total order is `(lamport, site_id, content_hash)`.** `lamport` is a **Lamport
  logical clock**: a per-device integer set to `max(every counter the device has
  observed) + 1` on each new local change, **durably persisted**. The hash tiebreak
  makes the order total even when two devices share a `site_id` and pick the same
  counter.
  - **Causal consistency.** If change A *happened-before* B (B authored by a device
    that had already observed A), then `lamport(A) < lamport(B)`. Concurrent
    changes may compare either way, but **identically on every node**, because the
    order is a function only of `(lamport, site_id, hash)` — all of which replicate
    with the row. This is exactly why the fold is byte-deterministic regardless of
    clocks, skew, or delivery timing.
  - **Note on "chatty" devices.** The counter counts *events*, not bytes, so a
    device that makes many small changes advances its counter faster. This never
    buries a quiet device (a change sorts by *its own* counter, near its causal
    position; disjoint edits survive regardless of order). The only observable
    effect is a mild "more-recent-local-activity wins" bias on *same-region
    concurrent* edits — deterministic, bounded, and already mitigated by
    debounce-squash (a burst of keystrokes is one counter tick, not many).

- **Timestamps are recorded, never the sort key.** Every row carries `ts`, the
  authoring wall-clock. It is used **only** for point-in-time queries ("state as of
  T") and display. It **never** participates in fold order or conflict tiebreak.
  This is the load-bearing separation: *recording* a timestamp and *ordering by* a
  timestamp are different things, and only the second is harmful.
  - **Why this matters:** under a 3-way fold the order *is* the merge sequence, so
    ordering by a skewed wall-clock would fold an edit in at the wrong position and
    produce a merge **no device authored** — silently, on every node. The Lamport
    key removes that failure mode entirely while costing nothing: csp already runs
    a logical counter in production.

- **Two distinct counters.** `lamport` (causal, `max(observed)+1`, drives the fold)
  is **not** `seq` (dense per-device `0,1,2,…`, drives version vectors and gap
  detection). Both are stored; neither substitutes for the other.

## Data model & storage

**Engine:** SQLite — Turso's pure-Rust rewrite if mature enough for our targets,
else **libSQL**. One mature dependency gives incremental durable persistence,
transactional integrity, a single-file store, SQL + full-text query for agents,
**and native vector/ANN** for embeddings — and it compiles to `wasm32` (OPFS) so
the same engine runs in the browser/Obsidian node. We do **not** use the engine's
built-in (server-centric) replication; our P2P sync rides our own transport.

We keep **two representations on purpose**: diffs make *sync* a pre-computed
`SELECT` (compute once, ship to every peer, no per-sync recompute); full
content-addressed bytes make *point-in-time* **instant** (a query + blob lookup,
never a diff-chain replay). For Markdown the redundancy is negligible.

```sql
-- Immutable bytes, content-addressed: file snapshots, line/blob payloads. LOCAL.
blobs(content_hash TEXT PRIMARY KEY, bytes BLOB);

-- Append-only, SYNCED global log = the source of truth. One row per change.
log(
  id           TEXT PRIMARY KEY,  -- Merkle id = hash of this row (tamper-evident, dedup)
  site_id      TEXT,              -- authoring device
  lamport      INTEGER,           -- logical clock = max(observed)+1; durably persisted
                                  --   (lamport, site_id, id) is the GLOBAL TOTAL ORDER
  seq          INTEGER,           -- per-device DENSE counter (version vector, gap detection)
  ts           TEXT,              -- authoring wall-clock; RECORDED FOR PITR ONLY, never sorted on
  path         TEXT,              -- vault-relative file
  merge_class  TEXT,              -- 'text' | 'code' | 'binary'  (deterministic routing)
  parent       TEXT,              -- previous log id for this path (defines LCA / apply order)
  base_hash    TEXT,              -- content the diff applies to (NULL on create)
  result_hash  TEXT,              -- resulting content hash (NULL on delete) -> instant point-in-time
  payload      BLOB,              -- text/code: line diff | binary: full/keyframe ref
  UNIQUE(site_id, seq)
);

-- Materialized current state: cache of the fold. Drives disk writes + fast reads.
manifest(path TEXT PRIMARY KEY, result_hash TEXT, lamport INTEGER, site_id TEXT);

-- Memoized fold steps: a late row only recomputes affected files + downstream.
fold_cache(step_key TEXT PRIMARY KEY, output_hash TEXT);  -- key = (base_hash, input_hashes...)

-- Content-addressed embeddings. Append-only; sync optional/directional.
embeddings(content_hash TEXT, model_id TEXT, vector F32_BLOB,
           PRIMARY KEY(content_hash, model_id));

-- Per-peer sync cursor = a version vector across all known devices.
peer_state(site_id TEXT PRIMARY KEY, last_seq INTEGER);
```

> Large/binary files don't keep a full copy of every version: store periodic
> **keyframes + diffs** (video-codec style) so point-in-time is a bounded replay
> from the nearest keyframe. Not needed for Markdown.

## Capture

- Listen for change events per host: inotify (native daemon) / the Obsidian vault
  API / OPFS in the browser.
- **Diff → change bridge.** We watch *files*, so a change arrives as new bytes.
  Reconstruct the change against the file's current state: for text and code, a
  line diff (base = current `result_hash`); for binary, a new full version.
- **Stateful engine, delta API.** The engine **holds the working set** (fixing
  csp's O(whole-vault)-per-edit cost): the host calls `stage_write(path, bytes)` /
  `stage_remove(path)` / `commit_staged()`, file content crosses the wasm boundary
  as **raw bytes** (near-zero-copy), and re-hashing is limited to paths that
  actually changed. A one-character edit does work proportional to that file.
- **Startup reconciliation.** On launch, diff actual files on disk against the
  `manifest` and emit changes for any divergence. This recovers anything lost from
  an in-memory debounce buffer on a crash *and* picks up edits made by other tools
  while the daemon was off. Disk is ground truth at boot.
- **Bootstrap before publish.** If local state is empty but disk has files **and**
  peers are known, **defer** the first commit until after handshake + catch-up,
  then publish only genuine divergence parented on the synced state. This closes
  the "stale device republishes a deleted file" resurrection class (csp issue
  0012): in an explicit op-log a delete is a durable, ordered row, so a
  reconnecting device learns it via catch-up instead of emitting a false-add.

## Op creation — debounce & squash

- **Debounce before appending** to `log`: coalesce a burst into one net change
  (diff the pre-burst state against post-burst bytes once).
- **Max-interval flush** bounds a *continuous* stream — flush at least every N
  seconds even if it never goes quiet.
- **Net-effect in the window:** typed-then-deleted within one window → nothing;
  several edits → one net change.
- The squash boundary is the **snapshot boundary** — it sets the granularity of
  rollback and of the derived git history, and it bounds the Lamport counter (one
  tick per window, not per keystroke).

> The squash boundary *is* a commit boundary: it decides what's worth keeping as
> history. This is the "version control like git, but automatic" goal realized —
> commits with a deterministic trigger instead of a human typing `commit`.

## Sync protocol

- **The log is the synced unit.** `blobs`, `manifest`, `fold_cache`, `embeddings`,
  and the derived git history are computed locally (blobs referenced by a row are
  fetched on demand).
- **Optimistic real-time push.** On a new local row, push it immediately as a small
  frame. Because state is a deterministic function of the row set, a receiver folds
  it in and converges — no round-trip, no permission.
- **Hub is a peer: forward-then-merge.** An always-on hub forwards a row to peers
  immediately and folds it into its own copy asynchronously, so it is never a
  serialization bottleneck.
- **Reconnect via version vectors.** Each node tracks the latest `seq` it holds per
  device (`peer_state`); on connect, peers exchange vectors and each sends exactly
  what the other is missing. (A single cursor is wrong for a multi-writer mesh;
  range-based set reconciliation is the heavier fallback under arbitrary gossip.)
- **Bound every frame** (chunk large transfers) so one oversized message can't kill
  a flaky mobile link.
- **Transport, phased.** *Phase 1:* WebSockets to an always-on hub peer — simple,
  all traffic through a publicly-reachable node. *Phase 2:* **iroh** (QUIC + NAT
  hole-punching + relay fallback) for true device-to-device P2P — LAN-speed direct
  sync, the hub demoted to relay-of-last-resort, connection survival across
  Wi-Fi↔cellular. Phase 2 is an upgrade, not required for correctness.

## Materialize to disk

- Fold → update `manifest` → render changed files → write each **once**, per-file
  **atomically** (temp + rename). Self-writes are non-events (rendered hash already
  matches the manifest), which suppresses **inotify echo storms**. Reconcile by
  last-materialized content hash so a user edit during materialization is never
  clobbered.
- No cross-file atomicity is attempted (we opted out of atomic commits); a
  partially-applied set is acceptable and self-heals as rows settle.

## History, rollback & re-fold

This is the subtle part; stated precisely:

- **Two histories live in the one synced log.** The **global merged history** (the
  fold of all rows in Lamport order) is *mutable* — a late row with a low Lamport
  value folds in at its position and recomputes states after it, so the canonical
  "state as of version V" can change as stragglers arrive. Each device's
  **authored history** (its own rows' immutable `base → result` hash chain) is
  *immutable* and is itself in the log.
- **Point-in-time, two flavors:**
  - **Named snapshots = exact, skew-free.** A snapshot is a *logical* marker over
    the fold (not a wall-clock), content-addressed, instant to restore. This is the
    primary recovery mechanism — use it when you want an exact moment.
  - **"State as of wall-clock T" = best-effort query.** Filter rows by recorded
    `ts ≤ T`, then fold *those* in Lamport order. Skew only **blurs the T-boundary**
    (you might land on a neighboring state); it **never corrupts convergence**,
    because the fold is ordered by Lamport, not by `ts`. For exactness, drop a
    snapshot.
- **Instant point-in-time** = nearest memoized checkpoint + bounded replay, read
  from `blobs`. Checkpoints are content-addressed, so unchanged files dedup.
- **Re-fold cost is bounded.** A late row only triggers real recomputation for the
  file(s) it touches and their downstream merges; `fold_cache` turns the rest into
  hits.
- **Deletes are explicit, ordered rows** (`result_hash = NULL`), not "an absent
  path." This — plus the bootstrap-before-publish rule — dissolves the spurious
  delete-resurrection class. A *genuine* offline edit to a remotely-deleted file is
  still a real concurrent delete-vs-edit conflict, resolved by a **tombstone
  policy** (the delete dominates unless a later same-region edit is explicitly
  ordered after it); tombstones are GC'd past the retention horizon.

## Implementation: one engine, wasm everywhere

A hard structural rule inherited from csp: **the protocol is implemented exactly
once, in Rust, and runs identically on every surface.**

- **Single core crate.** The entire engine — object/oid model, the deterministic
  fold + 3-way merge, the sans-IO sync `Session` (handshake, frontier anti-entropy,
  integrate), identity/auth, wire framing, scope/ignore, config — lives in one
  crate. **All** convergence and merge logic lives here and nowhere else.
- **Compiles to `wasm32` unchanged.** Every node — native daemon, desktop, and the
  in-browser/Obsidian plugin — runs the **identical** fold/merge and computes
  **byte-identical** state. I/O is injected via traits (storage, transport, clock,
  rng). Only genuinely platform-bound pieces are `cfg`-gated behind a native
  feature: the on-disk SQLite/odb backend, the listen socket, TLS. The browser node
  uses libSQL-over-OPFS through the same storage trait.
- **Lean by construction.** Because the one engine is also the wasm payload, the
  core carries no heavy general-purpose deps where a hand-rolled, differentially
  tested equivalent will do (e.g. the gitignore matcher and config codec — proven
  byte-for-byte against the reference crate as a dev-only oracle). **Any CRDT, if
  adopted later, is held to the same bar:** wasm-byte-identical across native and
  browser, and its exact version **pinned in the synced routing config** (a node on
  a different CRDT version is a node that diverges).
- **Thin bindings.** The native CLI and the TypeScript/wasm SDK are thin drivers
  over the same `Session`; a host plugin (first target: Obsidian) is as thin as
  possible over the SDK. Any behavioral difference between surfaces is a bug.
- **Headline gate (from csp).** N simulated independent nodes, all delivery orders,
  including offline-then-merge, gossip/mesh, and same-`site_id` concurrency, must
  converge to **identical merged state**; the wasm node must converge **bit-for-bit**
  with native against shared test vectors. Build the reference fold and
  property-test order-determinism **before** anything else — if it cannot be made
  deterministic in practice, the architecture does not work.

## Derived git history (keep csp's stock-git compatibility)

We **keep csp's git story**: a real, **stock-git-compatible, read-only** history
that any unmodified `git` can inspect.

- **Derived from the log, not the source of truth.** At settle boundaries a full
  node materializes converged bytes into a real git object store (e.g.
  `.context/git`), inspectable via the bundled read-only `ctx git` or by pointing
  unmodified `git --git-dir` at it. **No `.git` at the vault root**, so the engine
  coexists with a project's own git repo.
- **Deterministic commits.** Commit objects are byte-pinned (fixed identity, fixed
  template, derived non-decreasing times) so SHAs **converge across nodes** — `git
  log` is identical everywhere, giving git tooling (`log`/`diff`/`bisect`) without
  making git the convergence substrate.
- **Read-only is a data-loss-critical guard.** The repo is engine-owned; a write
  reaching it (a mis-allowlisted mutating verb, an agent running `ctx git commit`/
  `checkout`) is silent corruption. The `ctx git` allowlist is **deny-by-default**
  and conservative, with its own test suite asserting every mutating verb is
  rejected. Restore is `ctx restore`, never `git checkout`.

We use a **minimal git object writer**, not a full git/gitoxide dependency — git's
semantics and a readable history, without the library or its wasm bloat.

## Embeddings / RAG

- Append-only `embeddings(content_hash, model_id, vector)` — content-addressed
  (embed each unique content once, dedup'd) and **model-versioned** (re-embed with
  a new model without touching the log).
- **Sync is optional and directional.** Embeddings can be larger than a small
  Markdown file, and some devices (mobile) can't run the model — so capable nodes
  compute and *share* to weaker nodes; nodes that can recompute locally do. Native
  vector/ANN gives semantic search/RAG over current *and* historical content.

## Live structured data (out of scope for v1)

Not everything belongs on the vault's version timeline. **Chat history, agent
conversation logs, and app/settings state must survive a vault rollback** — you
shouldn't lose this week's chats by checking out last week. Such data lives in a
**separate, non-versioned domain** (its own tables, synced but not folded into the
vault history, never subject to vault rollback). A mutable-state CRDT (e.g.
cr-sqlite) is the natural fit there if it grows beyond a few fixed tables.
Deliberately kept out of the core engine; the boundary is explicit — "survive a
rollback?" → live; "roll back with the vault?" → versioned.

## Deliberately not doing

- **No wall-clock as the merge sort key** — timestamps are recorded for PITR and
  display only; the fold orders by the Lamport logical clock. (Ordering by
  wall-clock would make merges non-reproducible and clock-skew-sensitive in their
  *content*, not just the winner.)
- **No CRDT for text by default** — csp's line-level 3-way is sufficient and keeps
  text in the global fold with crisp point-in-time; a (Yrs) CRDT is a possible
  per-path opt-in only.
- **No silent loss of concurrent code edits** — code surfaces a conflict the agent
  resolves rather than dropping a side.
- **No atomic multi-file commits** — incompatible with continuous real-time sync,
  unnecessary for a notes vault (dangling links self-heal).
- **No whole-file LWW for text** — kept only as the binary fallback.
- **No whole-vault structured CRDT adopted wholesale** — at most a narrow per-file
  text CRDT, opt-in.
- **No "full git" / gitoxide dependency** — a minimal object writer for a readable,
  deterministic, read-only derived history.
- **No server-ordered op-log / cloud sync platform** — needs a central authority,
  violating the P2P goal.
- **No cr-sqlite in the vault path** — the deterministic fold needs no CRDT
  extension; cr-sqlite is reserved for the live-data domain.

## Known tradeoffs & open questions

- **Code conflict representation — markers vs. conflict-copies.** Markers are loud
  and localized but break the file until resolved (and, being deterministic, break
  it on *every* node at once); copies are cheaper, keep the file runnable, and are
  trivially convergent. Both must be byte-deterministic. Start with one, measure.
- **Lamport persistence & same-site concurrency.** The counter must be durably
  persisted across restart; two replicas authoring under one `site_id` with equal
  counters are kept total by the content-hash tiebreak. `clone`/restore should fork
  a fresh `site_id` (or warn) rather than silently resume a key that may be live
  elsewhere.
- **Is character-level same-line co-editing ever needed?** If real usage shows
  meaningful same-line human+agent concurrency, add **Yrs** as a per-path opt-in
  (not Automerge, not hand-rolled). Until then, line-level 3-way is the default.
- **PITR precision under skew.** "As of T" is best-effort at the T-boundary;
  snapshots are the exact mechanism. Acceptable; revisit if it bites.
- **Tombstone semantics & GC.** Genuine offline-edit-vs-delete needs a clear
  dominance rule; tombstone compaction timing vs. the retention horizon.
- **Criss-cross / multiple LCA** in a wide mesh needs a deterministic base pick
  (lowest content-hash, or deterministic recursive merge).
- **Routing must be deterministic and agreed** across nodes (extension map + synced
  config; the real axis is *human-edited vs. machine-rewritten*, of which file-type
  is only a proxy), or two nodes pick different regimes and diverge.
- **Retention horizon** length vs. how far back rollback must reach; log/tombstone
  compaction timing.
- **Embedding granularity** (file-level vs. chunk-level) and producer/consumer sync
  policy.
