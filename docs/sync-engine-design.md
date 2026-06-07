# Sync Engine — Design Spec

> Revision: CRDT merge model. This supersedes the earlier whole-file LWW draft.
> The merge layer is now a **per-file line-level sequence CRDT (RGA)**, the
> document order is a **wall-clock-free Lamport id**, and a **derived git
> history** provides tooling + instant point-in-time. The reasoning for the
> change is summarized in "Why a CRDT, not LWW or diff-replay" below.

## Intent

We are building the storage and sync layer for **agent context and a human's
Markdown vault**, shared across a person's devices and across agent sessions. An
agent should be able to work on one machine, close the session, and pick up the
exact same context on another; a human should be able to edit notes in Obsidian
on a phone and a laptop and have them converge; and either should be able to
**roll the whole workspace back to any earlier moment** without ceremony.

The shape of the problem is git's — versioned, content-addressed history of
files — but git's *operation* is wrong for it. Git is manual (you commit and
push), batch (you sync on demand), and its merge needs a human. We need the
opposite: **automatic** (changes are captured and synced with no explicit
commit), **real-time** (edits propagate in roughly a second), **peer-to-peer**
(no central authority must be in the path), and **convergent without a human**
(every node deterministically reaches the same state). So the engine is, in one
line: **git's content-addressed storage with an automatic, convergent CRDT
merge, on an embedded database.**

A deliberate consequence of "automatic + real-time": there are **no atomic
multi-file commits**. Continuous sync means a peer's view is always a partial
cut by definition, so we stream changes as they happen rather than grouping them
into all-or-nothing units. We accept brief cross-file inconsistency (a link that
dangles for a second) as the cost of liveness — fine for a notes vault, and a
non-issue once changes settle.

We assume **all nodes are trusted** (one person's devices + their agents), so
there is no end-to-end encryption and no signed-authorship requirement. That
assumption removes a great deal of machinery and is load-bearing — if it ever
reverses, parts of this design change.

## Goals

- **Agent context continuity** — the same working state across sessions and
  devices.
- **Real intra-file merge** — concurrent edits to *different lines* of the same
  file **both survive**; concurrent edits to the *same* line converge
  deterministically. No conflict markers, ever. (This is a goal now; the earlier
  LWW draft explicitly gave it up. See "Why a CRDT.")
- **Instant point-in-time rollback** — jump to the vault as it was at any past
  moment, *immediately*, without replaying history.
- **Offline-first** — a full local copy; reads and edits never wait on the
  network.
- **Automatic version control** — the system captures and syncs on change; the
  agent/user never runs commit/push.
- **Git-tool compatible** — a real, stock-git history is materialized so full
  nodes can inspect and diff past versions with ordinary git tools.
- **Efficient & real-time at scale** — many small fast changes, many files,
  network-frugal on flaky/mobile links, ≤ ~1s propagation.
- **Peer-to-peer** — no privileged server. An always-on hub is allowed only as
  *a peer like any other* (relay + store-and-forward), never a special endpoint.
- **Markdown-first**, other formats supported. Text gets line-level merge;
  binaries fall back to whole-file LWW. We explicitly do **not** need intra-line
  collaborative (rich-text) cursor merge.

## Why a CRDT, not LWW or diff-replay

This is the central change from the earlier draft, so it is worth stating the
reasoning explicitly.

- **Whole-file LWW** is trivially convergent and order-independent, but it throws
  away same-file concurrency: an edit to one line silently clobbers a concurrent
  edit to another line of the same file. For a vault a human edits on two devices
  (and an agent edits alongside), that is the common, painful case.
- **diff3 / 3-way merge folded over a DAG** (what the predecessor system does)
  gives real intra-file merge, but it is **non-associative**: the result depends
  on the merge *base* and the fold *order*. Making it convergent requires a
  strict total order, recording every merge result as a first-class node, and a
  completeness guarantee — i.e. a substantial DAG-fold engine.
- **Ordered diff-replay** (apply each patch's diff in timestamp order) converges,
  but because it is order-*dependent* it loses the two properties we most want:
  a late/out-of-order patch forces a re-fold of everything after it (so the fold
  is not the clean incremental update LWW promises), and "state as of T" stops
  being a lookup (so instant point-in-time needs separate snapshots that late
  arrivals invalidate).
- **A sequence CRDT** gives intra-file merge **and** genuine order-independence
  (`state = f(set of ops)`), because it does not reconcile versions at all —
  there is **no base**. Every line is an immutable element with a stable id;
  "both sides survive" falls out of integrating elements in a deterministic
  order, not from a 3-way reconciliation. Convergence needs no fold order, no
  merge base, and no coordination. This is the property the whole system is built
  around, and the CRDT is the only one of the four options that delivers it
  *with* real merge.

We use the CRDT narrowly — a **per-file line sequence**, not a whole-vault
structured CRDT — and keep git as the inspectable, content-addressed history.

## Key decisions (and why)

- **Event-sourced.** An append-only, synced **op log** is the source of truth;
  everything else (current files, any past snapshot, the git history, search
  indexes) is a pure function of it. The ops are CRDT operations (see Merge).
- **Convergent by construction, not by coordination.** `state = f(set of ops)`,
  independent of arrival order, because CRDT ops **commute**. This is what lets
  nodes converge with zero coordination and is strictly stronger than the
  earlier "order-independent" claim, which only held for LWW.
- **Per-file line-level sequence CRDT (RGA).** Conflicts resolve at line
  granularity: disjoint lines both survive; same-line concurrency is ordered
  deterministically by element id. Binary / non-text files fall back to
  whole-file LWW by element id. (Granularity rationale and the RGA→YATA/Fugue
  upgrade path are in Merge and Open questions.)
- **Wall-clock-free order.** Element order and tiebreaks use a **Lamport id**
  `(lamport, site_id)`, never a wall clock. Clock skew can therefore never affect
  convergence or which concurrent edit sorts first. Wall-clock time is retained
  only as a human-facing label on point-in-time snapshots.
- **SQLite as the substrate** (Turso's pure-Rust rewrite if mature enough for our
  targets, else **libSQL**). One mature dependency gives us incremental durable
  persistence, transactional integrity, a single-file store, SQL + full-text
  query for agents, **and native vector/ANN** for embeddings. We do **not** use
  the engine's built-in replication — our P2P op sync rides on our own transport.
- **Git as a derived history layer.** At each settle boundary we materialize the
  converged file bytes and write a stock-git commit. The git history is *derived*
  from the op log (not the source of truth), which means: ordinary git tools
  read it, point-in-time is an immutable commit lookup, and — because git is
  derived rather than the convergence substrate — the git history can be
  node-local; it need not be byte-identical across nodes.
- **Assemble, don't reinvent.** Lean on proven parts — the SQLite engine, a
  well-understood sequence CRDT (RGA), version vectors for anti-entropy, iroh for
  transport — and keep our own novel code to the thin layers that are genuinely
  ours: the filesystem bridge (diff→ops), the debounce/squash policy, and the
  materialize-to-disk-and-git step.

## Data model & storage

The op log is the source of truth. The CRDT element table, the materialized
files, the git history, and the embeddings are all derived caches over it.

```sql
-- Immutable bytes, content-addressed: line values, full file snapshots, blobs.
-- LOCAL, not synced; each node reconstructs what it needs from received ops.
blobs(content_hash TEXT PRIMARY KEY, bytes BLOB);

-- Append-only, SYNCED op log = the source of truth. Each row is one CRDT
-- operation on one file. Ops COMMUTE, so the set — not the order — defines state.
ops(
  id         TEXT PRIMARY KEY,  -- Merkle id = hash of this record (tamper-evident, dedup)
  site_id    TEXT,              -- authoring device
  seq        INTEGER,           -- per-device monotonic counter (gap detection, version vector)
  lamport    INTEGER,           -- Lamport clock value; (lamport, site_id) is the element id basis
  path       TEXT,              -- vault-relative file the op applies to
  kind       TEXT,              -- 'insert' | 'delete' | 'file_put' | 'file_delete'
  elem_id    TEXT,              -- "(lamport, site_id)" of the element this op creates/targets
  origin_id  TEXT,              -- INSERT: the element id this line is inserted AFTER (HEAD = top)
  value_hash TEXT,              -- INSERT: hash of the line bytes (in blobs); NULL otherwise
  UNIQUE(site_id, seq)
);

-- Materialized CRDT state per file: the integrated element list. A cache of the
-- fold of all `insert`/`delete` ops for the path. Drives rendering to bytes.
elements(
  path         TEXT,
  elem_id      TEXT,            -- "(lamport, site_id)" — globally unique, immutable
  origin_id    TEXT,            -- the element this was inserted after
  value_hash   TEXT,            -- line bytes in blobs(); irrelevant once deleted
  deleted      INTEGER,         -- tombstone; kept so later ops can still anchor to it
  PRIMARY KEY(path, elem_id)
);

-- Current materialized file state: a cache of "render(elements[path])". Drives
-- disk writes and fast "what is the vault right now" reads.
manifest(path TEXT PRIMARY KEY, content_hash TEXT);

-- Immutable point-in-time snapshots = the derived git history. Each row pins a
-- converged whole-vault state to a stock-git commit and a wall-clock label.
snapshots(commit_id TEXT PRIMARY KEY, wall_time TEXT, label TEXT);

-- Content-addressed embeddings. Append-only; sync is optional/directional.
embeddings(content_hash TEXT, model_id TEXT, vector F32_BLOB,
           PRIMARY KEY(content_hash, model_id));

-- Per-peer sync cursor = a version vector across all known devices.
peer_state(site_id TEXT PRIMARY KEY, last_seq INTEGER);
```

> Binary / large files do not get the CRDT: they are stored as whole-file
> versions (`file_put`/`file_delete` ops carrying a `value_hash`) and resolved
> by whole-file LWW on `(lamport, site_id)`. The video-codec escape hatch
> (periodic keyframes + diffs) still applies if per-version full copies of large
> binaries get expensive; not needed for Markdown.

## Capture

- Listen for change events per host: inotify (native daemon) / the Obsidian
  vault API / OPFS in the browser.
- **Diff → ops bridge.** We watch *files*, so each change arrives as new whole-
  file bytes, not as edit operations. Reconstruct the ops with an LCS line diff
  against the file's current materialized element list:
  - **Equal** runs → reuse the existing elements' ids (no-op).
  - **Deleted** runs → emit `delete(elem_id)` (tombstone) for each line.
  - **Inserted** runs → emit `insert(new_id, origin = id of the surviving line
    immediately before the run, value = hash(line))`, chaining each inserted
    line's `origin` to the previous newly-inserted line.
  - Each `new_id` is `(lamport, site_id)` with `lamport = max(seen) + 1`.
- **Startup reconciliation:** on launch, diff the actual files on disk against
  the materialized manifest and emit ops for any divergence. This recovers
  anything lost from an in-memory debounce buffer on a crash, *and* picks up
  edits made by other tools while the daemon was off. Disk is ground truth at
  boot.
- **Native editor ops (optional refinement):** a host that controls the editor
  (e.g. the Obsidian plugin) can feed real keystroke-level edits into the *same*
  element structure instead of diff-derived ops, capturing intent more precisely
  (a moved paragraph stays a move rather than delete+insert). Both paths produce
  ops for the identical CRDT.

## Op creation — debounce & squash

Rapid changes (an agent rewriting a file 50× a second, autosave storms) must not
become 50 synced op batches.

- **Debounce before appending** to `ops`: coalesce a burst into the net set of
  inserts/deletes (diff the pre-burst materialized state against the post-burst
  bytes once).
- **Max-interval flush** bounds a *continuous* stream — flush at least every N
  seconds even if it never goes quiet — so a constantly-changing file produces a
  bounded op rate.
- **Net-effect within the window:** a line typed then deleted inside one window →
  no ops; several edits to a line → one delete + one insert of the final text.
- The squash boundary is also the **snapshot boundary** (see History): it decides
  the granularity of the git history and of instant rollback.

## Clocks & causality

- **Lamport id `(lamport, site_id)`** per element — gives a deterministic,
  wall-clock-free total order. It fixes both element order and same-origin
  tiebreaks, and (because `child.lamport > origin.lamport` always holds) it is the
  invariant the RGA integration rule relies on. There is **no HLC** and no
  wall-clock input to the merge.
- **(site_id, seq)** per-device monotonic chain — gives intra-device order,
  natural dedup, and gap detection (a missing seq is a known hole). `seq` is the
  per-device axis of the version vector.
- **Causal delivery of origins.** An `insert` references its `origin` element; an
  op is integrated once its origin is present. Because ops commute and the
  per-device `seq` chain detects gaps, a node buffers an op whose origin it has
  not yet seen and integrates it on arrival — no global ordering needed.

## Merge — the line-level sequence CRDT (RGA)

The document for one file is a list of elements beginning with a fixed **HEAD
sentinel** (`elem_id = (0, "")`), so "insert at the top" means `origin = HEAD`.
Two operations, both referencing immutable ids — which is *why* they commute:

```
integrate_insert(new_id, origin_id, value_hash):
    i = index_of(origin_id) + 1
    # walk right, skipping any element that outranks us; stop at the first
    # element whose id is below ours (concurrent siblings sort id-descending)
    while i < len(doc) and doc[i].elem_id > new_id:
        i += 1
    doc.insert_at(i, { new_id, origin_id, value_hash, deleted: false })

integrate_delete(target_id):
    element(target_id).deleted = true     # tombstone; never unlinked
```

- **Render to bytes:** walk the element list, skip tombstones, concatenate the
  `value_hash` blobs. Cache the result content-addressed; that blob is what is
  written to disk and committed to git.
- **Convergence:** each op names an immutable `id` and `origin` (a stable id,
  never an index). `integrate_insert` is a pure, commutative function of
  `(op, current element set)`; `delete` is a flag on a permanent element. Hence
  `document = f(set of ops)`, independent of arrival order — no fold order, no
  base, no coordination.
- **Same-line concurrency:** two concurrent edits to one line become two elements
  at the same origin; both survive, ordered by id. A `same_line_collapse` policy
  may optionally keep only the higher-id element materialized (tombstoning the
  other, which stays recoverable in history) to reproduce a clean single line —
  the predecessor system's "one side wins, loser kept in history" behavior, on
  top of the CRDT.
- **Binary / non-text:** no CRDT; `file_put`/`file_delete` resolve whole-file by
  `(lamport, site_id)` LWW.
- **Baseline & upgrade path:** RGA is the simplest *correct* convergent rule. It
  has mild interleaving anomalies in pathological multi-line concurrent-insert
  cases; YATA (Yjs; adds a right-origin) and Fugue (maximal non-interleaving) are
  the known upgrades. At line granularity for a notes vault the anomalies are
  rare and mild, so v1 ships RGA.

## Sync protocol

- **Ops are the synced unit.** `blobs`, `elements`, `manifest`, the git
  `snapshots`, and `embeddings` are derived locally and not synced (blobs
  referenced by an op are fetched on demand).
- **Optimistic real-time push.** On a new local op, push it to connected peers
  immediately as a small frame. Because ops commute, a receiver integrates it on
  arrival and still converges — no round-trip, no permission. An always-on hub
  peer **forwards an op onward immediately** and integrates it into its own copy
  asynchronously, so it is never a serialization bottleneck.
- **Reconnect via version vectors.** Each node tracks the latest `seq` it holds
  per device (`peer_state`); on connect, peers exchange vectors and each sends
  exactly what the other is missing. (A single "last op" cursor is wrong for
  multi-writer topologies; range-based set reconciliation is the heavier
  alternative if divergence under a wide mesh gets arbitrary — see Open
  questions.)
- **Catch-up is the op set, not a fold.** Because state is `f(set of ops)`, a
  reconnecting peer just needs the ops it is missing; there is no squashed-vs-full
  distinction required for correctness. (Tombstone compaction can still trim very
  old, causally-stable ops — see History.)
- **Bound every frame** (chunk large transfers) so one oversized message can't
  kill a flaky mobile link. A multi-op frame is safe to apply partially —
  commutativity means a half-delivered batch is just a smaller set.
- **Transport, phased.** *Phase 1:* WebSockets to an always-on hub peer — simple,
  all traffic flows through a publicly-reachable node. *Phase 2:* **iroh** (QUIC +
  NAT hole-punching + relay fallback) for true device-to-device P2P — LAN-speed
  direct sync, the hub demoted to relay-of-last-resort, connection survival across
  Wi-Fi↔cellular. Phase 2 is an upgrade, not required for correctness.

## Materialize to disk & git

- Integrate ops → update `elements` → render changed files → write each file
  **once**, per-file **atomically** (temp file + rename).
- Materializing in the DB before writing reduces disk churn and, more
  importantly, avoids **inotify echo storms** — our own writes re-triggering the
  watcher and fighting the change-suppression set. Self-writes are non-events by
  construction (the rendered content hash already matches the manifest).
- **Snapshot to git at the settle boundary.** When a file (or a debounce window)
  settles, write the rendered bytes into a stock-git commit and record it in
  `snapshots` with a wall-clock label. The git history is a sequence of immutable
  converged snapshots; the op log remains the source of truth.
- No cross-file atomicity is attempted (we opted out of atomic commits); a
  partially-applied set is acceptable and self-heals as ops settle.

## History, rollback & compaction

- **Instant point-in-time** is an immutable commit lookup: find the `snapshots`
  row at/before the requested wall-clock T and read its git tree (+ blobs). No
  fold, no replay. Crucially, because snapshots are immutable records of converged
  *materialized* state — not recomputed folds — a **late-arriving op never
  invalidates a historical snapshot**; it only produces a new current state and a
  new snapshot. (This is the failure mode the LWW/diff-replay drafts could not
  avoid; the CRDT + immutable-snapshot model sidesteps it.)
- **Rollback granularity = snapshot cadence** (the squash/settle window). Older
  snapshots are later compacted coarser.
- **Tombstone GC.** Once the version vectors show every known peer has seen a
  delete, the tombstoned element may be physically dropped from `elements`
  (its op stays in the log until log compaction). Standard CRDT GC; the version
  vector already carries the needed causal information.
- **Compaction / retention horizon.** Keep recent history fine-grained; compact
  older ops, tombstones, and snapshots into coarser checkpoints so storage and
  catch-up cost stay bounded over a long-lived vault. The retention horizon is the
  floor on how far back rollback can reach.

## Embeddings / RAG

- Append-only `embeddings(content_hash, model_id, vector)` — content-addressed
  (each unique content embedded once, dedup'd) and **model-versioned** (a new
  model can re-embed without disturbing ops).
- **Sync is optional and directional.** An embedding can be larger than a small
  Markdown file, and some devices (mobile) may not run the model at all — so a
  capable node computes and *shares* embeddings to weaker nodes, while nodes that
  can recompute locally do so. Native vector/ANN gives semantic search and RAG
  over current *and* historical content.
- Granularity note: keying on *file* `content_hash` yields file-level vectors;
  good retrieval usually wants *chunk*-level vectors, which implies a
  deterministic chunking scheme and a chunk hash (Open questions).

## Deliberately not doing

- **No atomic multi-file commits** — incompatible with continuous real-time sync,
  and unnecessary for a notes vault (brief dangling links self-heal).
- **No whole-file LWW as the text merge** — it silently clobbers concurrent
  same-file edits; that is exactly the property the CRDT exists to provide. (LWW
  is retained only as the binary/non-text fallback.)
- **No diff3 / 3-way merge folded over a DAG, and no ordered diff-replay** — both
  are order-dependent (non-associative), so they cost the incremental-update and
  instant-point-in-time properties the CRDT keeps for free.
- **No whole-vault structured CRDT (Automerge/Yjs document model adopted
  wholesale)** — we use a *narrow* per-file line-sequence CRDT (RGA) and keep git
  as the history substrate, rather than adopting a document database we'd use a
  fraction of. (This reverses the earlier draft's blanket "no CRDT" decision: we
  do want intra-file merge, and a scoped sequence CRDT is the right tool — we just
  do not need the full structured-document machinery.)
- **No "full git" / gitoxide as a dependency** — we want git's *semantics* and a
  readable on-disk history, produced by a minimal stock-git object writer, not a
  general git library or its wasm bloat.
- **No server-ordered op-log / cloud sync platform** — would require a central
  authority, which violates the P2P goal. (Note the CRDT makes any global order
  unnecessary anyway.)
- **No end-to-end encryption or signed authorship** — load-bearing on the
  all-nodes-trusted assumption; revisit only if that assumption reverses.

## Phased plan

1. **Engine core** (highest value): the append-only op log, the RGA line CRDT
   (integrate/render), the diff→ops capture bridge, debounce/squash, startup
   reconciliation, materialize-to-disk, version-vector sync, frame chunking.
2. **Git history layer**: materialize converged snapshots to stock-git commits;
   instant point-in-time over `snapshots`.
3. **iroh transport** for true P2P (NAT traversal, relay, connection migration).
4. **Embeddings/RAG** layer over the content store.
5. **Compaction** / retention-horizon + tombstone GC once history growth warrants.

## Open questions

- **Merge granularity:** line (this spec) vs block (paragraph/heading) vs
  character. Line matches the predecessor's diff3 granularity and keeps tombstone
  count proportional to lines; block is lighter but coarser; character is overkill
  given live cursor-merge is out of scope.
- **RGA vs YATA vs Fugue** for interleaving quality as a later upgrade.
- **Version vectors vs range-based set reconciliation** as node count / mesh width
  grows.
- **Tombstone & op-log GC timing** vs how far back rollback must reach (retention
  horizon length).
- **Embedding granularity** (file-level vs chunk-level) and producer/consumer sync
  policy.
- **Keyframe-vs-every-version** retention for large/binary files.
- **Rename handling** — v1 treats a rename as delete+create (cross-rename history
  merge is a known hard problem; deferred).
