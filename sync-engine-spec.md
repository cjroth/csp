# Sync Engine — Design Spec

> Successor to **csp** (Context Sync Protocol). Keeps csp's spine — a signed event
> log, a deterministic fold, real-time push, automatic debounced commits, P2P with
> the hub as a peer, one-engine-everywhere wasm, and a stock-git-compatible
> read-only derived history — and changes what csp got expensive or wrong:
> the **substrate** (git object model → SQLite event log, killing the per-edit
> O(whole-vault) cost), the **ordering** (a two-layer fold — an always-on causal
> layer with a logical-clock concurrent tiebreak; wall-clock is a *post-v1 offline
> experiment*, never a live divergence-critical setting), **code-conflict policy**
> (surface conflicts an agent fixes, not silent drops), and **rename handling**
> (stable per-file identity, not delete+create).

## Intent

We are building the storage and sync layer for **agent context and a human's
Markdown vault**, shared across a person's devices and across agent sessions. An
agent should work on one machine, close the session, and resume the same context
on another; a human should edit notes in Obsidian on a phone and a laptop and have
them converge; and either should be able to **roll the workspace back to any
earlier moment** without ceremony.

The shape is git's — a versioned, content-addressed history of files — but git's
*operation* is wrong for it. Git is manual (commit + push), batch (sync on
demand), and its merge needs a human. We need the opposite: **automatic** (capture
and sync on change, no explicit commit), **real-time** (≈1s propagation),
**peer-to-peer** (no central authority in the path), and **convergent without a
human** (every node deterministically reaches the same state). One line: **git's
content-addressed storage, with an automatic, deterministic merge, on an embedded
database.**

Two consequences are deliberate. First, **no atomic multi-file commits** —
continuous sync means a peer's view is always a partial cut, so we stream changes
as they happen and accept brief cross-file inconsistency (a link dangling for a
second) as the price of liveness. Second, **all nodes are trusted** (one person's
devices + their agents), so there is no end-to-end encryption and no per-author
signing requirement — though rows remain Merkle-id'd and therefore tamper-evident.
If that trust assumption ever reverses, parts of this design change.

## Goals

- **Agent context continuity** — the same working state across sessions and
  devices.
- **Instant point-in-time rollback** — jump to the vault as of any past moment,
  immediately, without replaying from scratch.
- **Real merge where it helps** — concurrent edits to different regions of a text
  file both survive (line-level 3-way); concurrent edits to the same region of code
  surface a conflict the agent fixes. No silent loss for the cases that matter.
- **Offline-first** — a full local copy; reads/edits never wait on the network.
- **Automatic version control** — capture and sync on change; the agent/user never
  runs commit/push.
- **Efficient & real-time at scale** — many small fast changes, many files,
  network-frugal on flaky/mobile links, ≤ ~1s propagation. A single-file edit costs
  work proportional to that file, never to the whole vault.
- **Peer-to-peer** — no privileged server. An always-on hub is allowed only as
  *a peer like any other* (relay + store-and-forward), never a special endpoint.
- **One engine everywhere** — the deterministic fold/merge compiles to `wasm32`; a
  browser/Obsidian node computes byte-identical state to a native daemon.
- **Markdown-first**, other formats supported. Text and code both get **3-way
  merge** (prose resolves to a clean single file, code surfaces a conflict);
  binaries get whole-file last-writer-wins.

## Core model

- **Event-sourced.** One **append-only global log of all changes to all files** is
  the source of truth. Current files, any past state, the derived git history, and
  search indexes are all **pure functions of the log**.
- **Stable file identity.** Every file has a path-independent **`file_id`** assigned
  at creation. Log rows target a `file_id`; the **path is a mutable attribute** of
  it. A rename is a path-change row, not delete+create, so a file's edit history
  survives renames. `file_id` is assigned locally, which makes it a **convergence
  surface** in its own right — handled deliberately in *Renames & file identity*.
- **Deterministic by causal fold.** State is the deterministic fold of the log in a
  **canonical topological order**: the **causal layer** (always on) guarantees a
  row is folded only after the rows its `parent`/`base_hash` depend on, so a diff's
  base is always present and nothing reorders across a real dependency; among
  **concurrent** rows (no causal path between them) a **tiebreak** fixes a single
  order. Any two devices holding the same set of rows compute identical state —
  eventually consistent and deterministic.
- **Merges are derived, not synced.** Only genuine edit rows cross the wire. The
  merged state the fold produces is recomputed identically on every node, so it is
  never transmitted.

## The merge model (the centerpiece)

There is **one merge engine — 3-way merge against the last common ancestor** —
applied by folding the per-`file_id` diffs in fold order. What varies by file type
is the **conflict policy**, not the algorithm:

- **Text / Markdown → 3-way region merge, clean-resolve.** Concurrent edits to
  **different regions both survive**; edits to the **same region resolve
  deterministically** by fold order, with the **losing side kept in history**
  (recoverable via the log, never on disk). **No conflict markers** — the
  materialized file is always one clean coherent version. Granularity is
  **line-level**: diff3's unit is the line, so the *effective* granularity tracks
  the file's line structure — roughly paragraph-level when Markdown is soft-wrapped
  (one line per paragraph), sentence/line-level when hard-wrapped. Either way, edits
  to *different* lines both land and edits to the *same* line resolve
  last-writer-style (loser in history). That is sufficient for human+agent
  collaboration under ~1s sync; character-level *same-line* survival is the only
  thing it gives up, and that is the deferred CRDT opt-in below.

- **Code → 3-way against the LCA, conflict-surface.** Same 3-way merge, but a
  same-region conflict is **made visible for the user's agent to resolve** rather
  than silently dropped — silently losing a function is far more dangerous than a
  dangling note, and agents are good at resolving conflicts. Representation
  (deterministic in-file markers vs. side-by-side conflict-copies) is an open
  question; whichever is chosen must be **byte-deterministic**.

- **Binary / non-text → whole-file last-writer-wins** by fold order.

**Convergence.** This works *because* the order is global and causal, not pairwise.
"Ours vs theirs" in a 3-way merge is decided by **fold position in the shared
order**, identical on every node — not by local perspective — so every device
computes byte-identical results. To make 3-way output fully deterministic we fix:
(a) a **canonical operand ordering** (by fold order), (b) **deterministic conflict
representation** for code (fixed labels like `site:A`/`site:B`, never
`HEAD`/branch), (c) **pinned merge heuristics** (conflict style, rename detection,
whitespace), and (d) a **deterministic LCA** (criss-cross multi-base broken by
lowest content-hash). Two nodes with the same rows produce identical merged bytes.

**Routing is data, not code.** `merge_class ∈ {text, code, binary}` is set at file
creation and is constant for that `file_id`; `payload` is **opaque bytes**. This
keeps the door open to a future `text-crdt` class without a schema change — only
the engine interpreting that file's rows changes. A class change (e.g. opting a
file into the CRDT) is an **explicit boundary row** (`kind='reclass'`) that seeds
the new representation from the file's current content as a fresh base; the fold is
a per-`file_id` state machine that stops reinterpreting older line-diffs at that
boundary. Old rows are never retro-reinterpreted.

## Why not a CRDT for text (yet)

Considered and **deferred**, not rejected. Line-level 3-way already gives
"different regions both survive." A sequence CRDT adds only **character-level
same-line concurrent survival**, rare under ~1s sync, and it costs: a heavier wasm
dependency, tombstone blow-up under agent **wholesale-rewrite** (the common case in
an agent vault — modeled as delete-all + insert-all), and a muddier point-in-time
story (CRDT state has no clean order, so PITR needs change-replay + checkpoints).
Keeping 3-way also keeps text **inside the global fold** with crisp PITR. A CRDT is
reserved as a **per-path opt-in** (via the `reclass` boundary above) for genuinely
live human+agent co-edited files; if added, use **Yrs** (light, fast, wasm-proven —
we own history via the log, so we don't need Automerge's heavier built-in history),
**version-pinned in the synced config**, never hand-rolled.

## Clocks & ordering

The fold order has two layers:

1. **Causal layer (always on, not configurable).** A row is folded only after the
   rows its `parent`/`base_hash` depend on — a canonical topological sort of the
   per-`file_id` DAG. This guarantees a diff's base is always present and that no
   reordering across a real dependency can occur, independent of any tiebreak.
2. **Concurrent tiebreak.** Among rows with no causal relation, order is broken by
   `(tiebreak_key, site_id, content_hash)`. **In v1 `tiebreak_key` is fixed to
   `lamport`** — a Lamport logical clock, per-device integer set to `max(every
   counter observed) + 1` on each change, **durably persisted**, wall-clock-free and
   causally consistent by construction. Concurrent ties are deterministic but
   semantically neutral.

**Why `lamport` and not the wall clock (in v1).** Under a 3-way fold the order *is*
the merge sequence. The causal layer already prevents the worst failure (reordering
across a real dependency), but among *concurrent* edits the tiebreak still chooses
the merge sequence, and because 3-way merge is non-associative that can affect the
merged **content**, not just the same-region winner. With `lamport` that choice is
fixed by logical causality; with a wall clock it would shift with skew. `lamport`
costs nothing — csp runs a logical counter in production — and removes a whole class
of "weird but deterministic" merges. So v1 ships `lamport` only.

**`wall_clock` is a post-v1 *offline* experiment, not a live setting.** A wall-clock
concurrent tiebreak is more intuitive ("the edit I made later wins") and, because
`ts` is recorded on every row and replicates, it is still fully convergent — it can
never cause divergence or a causal violation (the causal layer is independent of the
tiebreak). We may still want it. But it is **not** worth a divergence-critical,
pinned, coordinated-migration vault setting that doubles the determinism conformance
matrix, for a subjective preference. Instead:

- **Both `lamport` and `ts` are stored on every row, always**, so the experiment
  needs no schema or capture change.
- **Evaluate it with a dev-only re-fold harness** that re-folds the *same captured
  log* under an alternate concurrent tiebreak and diffs the materialized output.
  Because the log is the source of truth, folding it twice is cheap — and it is the
  *only* controlled comparison: two live vaults diverge in content the moment you use
  them, so they compare two sessions, not two orderings of identical inputs.
- If the harness shows `wall_clock` is clearly better, promote it then — as a
  **genesis-immutable** vault property (below), with both code paths under the
  headline gate.

**Config that parameterizes the fold is genesis-immutable.** `tiebreak_key` (and any
other setting the fold consults) is set once at `init` and **cannot change on a
populated vault** — avoiding the chicken-and-egg where synced `config` would need an
order to converge but *is* what defines the order. Other config keys (CRDT version
pins, routing map) are ordinary rows folded under the (lamport) causal fold;
genesis-immutable keys are the narrow exception, fixed at vault creation.

**Two distinct counters.** `lamport` (causal, `max(observed)+1`, drives the
tiebreak) is **not** `seq` (dense per-device `0,1,2,…`, drives version vectors + gap
detection). Both are stored; neither substitutes for the other.

**Chatty-device note.** The Lamport counter counts *events*, not bytes, and
debounce-squash makes a keystroke burst one tick. A very active device gets a mild
"recent-local-activity wins" bias on *same-region concurrent* edits only —
deterministic and bounded; disjoint edits survive regardless.

## Data model & storage

**Engine:** SQLite — Turso's pure-Rust rewrite if mature enough for our targets,
else **libSQL**. One mature dependency gives incremental durable persistence,
transactional integrity, a single-file store, SQL + full-text query for agents,
**and native vector/ANN** for embeddings — and it compiles to `wasm32` (OPFS) so
the same engine runs in the browser/Obsidian node. We do **not** use the engine's
built-in (server-centric) replication; our P2P sync rides our own transport.

Two representations are kept on purpose: diffs make *sync* a pre-computed `SELECT`
(compute once, ship to every peer); full content-addressed bytes make *point-in-
time* **instant** (a query + blob lookup, never a diff-chain replay).

```sql
-- Immutable bytes, content-addressed: file snapshots, line/blob payloads. LOCAL.
blobs(content_hash TEXT PRIMARY KEY, bytes BLOB);

-- Append-only, SYNCED global log = the source of truth. One row per change.
log(
  id          TEXT PRIMARY KEY,   -- Merkle id = hash of this row (tamper-evident, dedup)
  site_id     TEXT,               -- authoring device
  lamport     INTEGER,            -- logical clock = max(observed)+1; durably persisted
  seq         INTEGER,            -- per-device DENSE counter (version vector, gap detection)
  ts          TEXT,               -- authoring wall-clock; for PITR + the post-v1 wall_clock experiment
  file_id     TEXT,               -- STABLE per-file identity (survives renames)
  kind        TEXT,               -- 'create' | 'edit' | 'rename' | 'delete' | 'reclass'
  merge_class TEXT,               -- 'text' | 'code' | 'binary' (set at create; changes only via 'reclass')
  parent      TEXT,               -- previous log id for this file_id (causal dep; LCA chain)
  base_hash   TEXT,               -- content the diff applies to (NULL on create)
  result_hash TEXT,               -- resulting content hash (NULL on delete)
  path        TEXT,               -- set by 'create'/'rename'; the file's path as of this row
  payload     BLOB,               -- text/code: line diff | binary: full/keyframe ref
  UNIQUE(site_id, seq)
);
-- Fold order = causal(parent) topological; concurrent ties by (tiebreak_key, site_id, id).
-- tiebreak_key is genesis-immutable; = 'lamport' in v1.

-- Materialized current state, keyed by STABLE identity; path is a mutable attribute.
files(
  file_id     TEXT PRIMARY KEY,
  path        TEXT,               -- current path (unique among live files; see note)
  result_hash TEXT,
  merge_class TEXT,
  deleted     INTEGER,            -- tombstone
  lamport     INTEGER, site_id TEXT
);
-- live-path -> file_id, derived from files; used by capture to map an FS event to its file_id.
-- NOTE: this is an OUTPUT INVARIANT, not an enforced SQL constraint during the fold.
-- The fold can transiently produce two live files at one path (concurrent create /
-- rename-into-occupied); it resolves the collision deterministically (below) and only
-- writes the resolved `files`. Enforcing it mid-fold would throw.
CREATE UNIQUE INDEX path_index ON files(path) WHERE deleted = 0;

-- Memoized fold steps: a late row only recomputes affected files + downstream.
fold_cache(step_key TEXT PRIMARY KEY, output_hash TEXT);  -- key = (base_hash, input_hashes...)

-- Content-pinned, immutable named snapshots (see History). Frozen at creation;
-- a snapshot is a GC ROOT — every blob its tree_hash references is pinned against
-- retention GC for as long as the snapshot exists.
snapshots(snapshot_id TEXT PRIMARY KEY, created_lamport INTEGER, label TEXT, tree_hash TEXT);

-- Content-addressed embeddings. Append-only; sync optional/directional.
embeddings(content_hash TEXT, model_id TEXT, vector F32_BLOB,
           PRIMARY KEY(content_hash, model_id));

-- Per-peer sync cursor = a version vector across all known devices.
peer_state(site_id TEXT PRIMARY KEY, last_seq INTEGER);

-- Synced vault config (routing map, CRDT version pins, ...). Most keys are ordinary
-- folded rows; fold-parameterizing keys (tiebreak_key) are GENESIS-IMMUTABLE.
config(key TEXT PRIMARY KEY, value TEXT);
```

> Large/binary files don't keep a full copy of every version: store periodic
> **keyframes + diffs** (video-codec style) so point-in-time is a bounded replay
> from the nearest keyframe. Not needed for Markdown.

## Capture

- Listen for change events per host: inotify (native daemon) / the Obsidian vault
  API / OPFS in the browser.
- **Map event → `file_id`.** A write/delete on a path resolves to its `file_id` via
  `path_index`. A **rename** resolves to the existing `file_id` (see below), not a
  new one.
- **Diff → change bridge.** A change arrives as new bytes; reconstruct it against
  the file's current state — for text/code a line diff (base = current
  `result_hash`), for binary a new full version.
- **Stateful engine, delta API** (fixes csp's O(whole-vault)-per-edit cost): the
  engine holds the working set; the host calls `stage_write(file_id, bytes)` /
  `stage_remove(file_id)` / `commit_staged()`; bytes cross the wasm boundary raw
  (near-zero-copy); re-hashing is limited to changed files. A one-character edit
  does work proportional to that file.
- **Startup reconciliation.** On launch, diff actual files on disk against `files`
  and emit changes for any divergence — recovering anything lost from an in-memory
  debounce buffer on a crash *and* picking up edits made while the daemon was off.
  Disk is ground truth at boot.
- **Bootstrap before publish.** If local state is empty but disk has files **and**
  peers are known, **defer** the first commit until after handshake + catch-up,
  then publish only genuine divergence parented on the synced state. In an explicit
  ordered log a delete is a durable row a reconnecting device *learns* via catch-up,
  so it never emits a false-add — dissolving csp's resurrection class (issue 0012).

## Renames & file identity

Renames are first-class because `file_id` is path-independent:

- **A rename is a `kind='rename'` row** that changes F's `path` attribute — F keeps
  its `file_id`, its edit history, and its LCA chain.
- **Concurrent rename + edit don't conflict.** Device A renames F (path attribute),
  device B edits F (content attribute) — different attributes of the same `file_id`,
  so both apply: F ends up at the new path **with B's edit intact**. (Delete+create
  would have lost B's edit on the deleted old path.)

**`file_id` is a deliberate identity trade — name it, test it.** `file_id` solves
renames, but assigning identity *locally* is itself a convergence surface, the same
shape as the delete-resurrection class we dissolved:

- **The trade.** *Path-as-identity* (git/csp) converges when two devices
  independently create the same path but loses rename history. *`file_id`* preserves
  rename history but **splits** an independent same-path creation into two files. We
  choose `file_id` — and mint it as a **random, site-local id** — on purpose:
  splitting is **visible and recoverable** (you see `todo.md` and `todo (1).md`),
  whereas silently merging two different-content files into one is not. Rename
  fidelity is common in an agent vault; uncoordinated same-path creation is rare and
  fails *loudly* under this choice, which is the safer failure.
- **Detecting the rename at capture time:**
  - **Host rename signal first** — Obsidian's vault rename event, inotify
    `IN_MOVED_FROM`/`IN_MOVED_TO` pairing, or native inode — gives old→new directly.
  - **Content-similarity inference fallback** when only delete+create is observed.
    Keep the threshold **conservative and gated**: identical content hashes are a
    weak signal for Markdown specifically (empty/templated notes collide), so do not
    infer a rename from an empty-or-template content match — require substantial,
    non-trivial similarity within a short window. A false positive merges two files'
    histories, which is unpleasant to undo.
  - **Cross-device caveat.** Detection runs per device, so two devices can disagree:
    if A pairs a rename (keeps `file_id`) but B sees only delete+create (no OS
    pairing — common over network FS, some editors, mobile), B mints a *new*
    `file_id` and the file splits. `file_id` stability is therefore *best-effort*,
    not guaranteed; the convergence story must hold regardless of which way capture
    classifies an event.
- **Convergence & collisions.** Rename rows fold like any other. Concurrent renames
  of F to different paths → last-by-fold-order wins (loser in history). Two distinct
  `file_id`s live at the **same** path (concurrent create, unpaired rename,
  rename-into-occupied) collide on the live-path invariant → resolved **in the fold**
  (lower fold-order keeps the path; the other gets a deterministic ` (n)` suffix),
  then written to `files`; flagged for the user/agent. This resolution is part of the
  **headline determinism gate**, not an afterthought (see *Implementation*).

## Op creation — debounce & squash

- **Debounce before appending** to `log`: coalesce a burst into one net change.
- **Max-interval flush** bounds a *continuous* stream — flush at least every N
  seconds even if it never goes quiet.
- **Net-effect in the window:** typed-then-deleted within one window → nothing;
  several edits → one net change.
- The squash boundary is the **snapshot boundary** — it sets rollback/derived-git
  granularity and bounds the Lamport counter (one tick per window, not per
  keystroke).

> The squash boundary *is* a commit boundary: a deterministic trigger replacing a
> human typing `commit`. This is "version control like git, but automatic."

## Sync protocol

- **The log is the synced unit.** `blobs`, `files`, `fold_cache`, `snapshots`,
  `embeddings`, and the derived git history are computed locally (blobs referenced
  by a row are fetched on demand).
- **Optimistic real-time push.** On a new local row, push it immediately as a small
  frame; a receiver folds it in and converges — no round-trip, no permission.
- **Hub is a peer: forward-then-merge.** An always-on hub forwards a row to peers
  immediately and folds it into its own copy asynchronously — never a serialization
  bottleneck.
- **Reconnect via version vectors.** Each node tracks the latest `seq` it holds per
  device (`peer_state`); on connect, peers exchange vectors and each sends exactly
  what the other is missing. (Range-based set reconciliation is the heavier fallback
  under arbitrary mesh gossip.)
- **Bound every frame** (chunk large transfers) so one oversized message can't kill
  a flaky mobile link.
- **Transport, phased.** *Phase 1:* WebSockets to an always-on hub peer. *Phase 2:*
  **iroh** (QUIC + NAT hole-punching + relay fallback) for true device-to-device P2P
  — LAN-speed direct sync, the hub demoted to relay-of-last-resort, connection
  survival across Wi-Fi↔cellular. Phase 2 is an upgrade, not required for
  correctness.

## Materialize to disk

- Fold → resolve path collisions → update `files` → render changed files → write
  each **once**, per-file **atomically** (temp + rename), at `files.path`.
  Self-writes are non-events (rendered hash already matches), suppressing **inotify
  echo storms**. Reconcile by last-materialized content hash so a user edit during
  materialization is never clobbered.
- No cross-file atomicity is attempted (we opted out of atomic commits); a
  partially-applied set is acceptable and self-heals as rows settle.

## History, rollback & re-fold

- **Two histories live in the one synced log.** The **global merged history** (the
  fold) is *mutable* — a late row with a low tiebreak value folds in at its position
  and recomputes states after it. Each device's **authored history** (its own rows'
  immutable `base → result` chain) is *immutable* and is itself in the log, so
  per-device time-travel is reconstructable from the synced log alone (no separate
  reflog needed).
- **Point-in-time, two flavors:**
  - **Named snapshots = exact, skew-free, immutable.** A snapshot **pins the actual
    `result_hash`es at creation time** (`snapshots.tree_hash`), so it is a frozen,
    content-addressed record — instant to restore and **unaffected by any later
    late-arriving row**. It is also a **GC root**: its blobs are retained for as long
    as the snapshot exists, regardless of the retention horizon. This is the primary
    recovery mechanism.
  - **"State as of wall-clock T" = best-effort.** Filter rows by recorded `ts ≤ T`,
    fold those. Skew only blurs the T-boundary; it never corrupts convergence (the
    fold orders by the causal layer + `lamport`, not by `ts`). For exactness, drop a
    snapshot.
- **Instant point-in-time** = nearest memoized checkpoint + bounded replay from
  `blobs` (content-addressed, so unchanged files dedup).
- **Re-fold cost is bounded.** A late row recomputes only the file(s) it touches and
  their downstream merges; `fold_cache` turns the rest into hits.
- **Deletes are explicit, ordered rows** (`kind='delete'`), not "an absent path."
  **Delete-vs-edit policy (v1 default: remove-wins).** A delete tombstones the
  `file_id`; a *concurrent* edit does **not** resurrect it — the delete dominates,
  and the edit is kept in history (recoverable). This matches intent in an agent
  vault, where a delete is usually final ("I deleted it; it shouldn't come back
  because something was mid-edit"). The alternative — **last-touch-in-fold-order
  wins**, where a higher-ordered concurrent edit re-creates the file — is simpler
  and more uniform with the rest of the fold but surprises on intentional deletes.
  Write the full truth table (delete vs edit vs rename, concurrent and causal) before
  coding the fold; tombstones GC past the retention horizon (snapshots excepted).

## Implementation: one engine, wasm everywhere

A hard structural rule from csp: **the protocol is implemented once, in Rust, and
runs identically on every surface.**

- **Single core crate** — object/oid model, the deterministic fold + 3-way merge,
  the sans-IO sync `Session` (handshake, anti-entropy, integrate), identity, wire
  framing, scope/ignore, config. **All** convergence/merge logic lives here.
- **Compiles to `wasm32` unchanged.** Native daemon, desktop, and in-browser/
  Obsidian node run the **identical** fold/merge and compute **byte-identical**
  state. I/O is injected via traits (storage, transport, clock, rng); only platform-
  bound pieces (on-disk SQLite backend, listen socket, TLS) are `cfg`-gated behind a
  native feature. The browser node uses libSQL-over-OPFS through the same storage
  trait.
- **Lean by construction.** The core carries no heavy general-purpose deps where a
  differentially-tested hand-rolled equivalent will do. **Any CRDT, if adopted, is
  held to the same bar:** wasm-byte-identical native↔browser, version-pinned in the
  synced config.
- **Thin bindings.** Native CLI and the TS/wasm SDK are thin drivers over the same
  `Session`; the Obsidian plugin is as thin as possible over the SDK. Any
  cross-surface behavioral difference is a bug.
- **Headline gate — two interlocking properties, both CI-blocking.**
  1. **Merge determinism.** N simulated nodes, all delivery orders (offline-then-
     merge, gossip/mesh, same-`site_id` concurrency), converge to **identical** state;
     the wasm node converges **bit-for-bit** with native against shared test vectors.
     Build the reference fold and property-test order-determinism **first** — if it
     can't be made deterministic in practice, the architecture doesn't work.
  2. **Identity convergence.** Two devices, **concurrent same-path create** and
     **unpaired rename** (A pairs it, B sees delete+create), must converge to the
     *same* set of `file_id`s and the *same* path assignments (including deterministic
     ` (n)` suffixing) across all delivery orders. `file_id` is a locally-assigned
     global identity, so this is a first-class gate, not an open question.
- **Dev-only re-fold harness.** Re-folds a captured log under an alternate concurrent
  tiebreak (and other what-ifs) and diffs the materialized output. This is how
  `wall_clock` vs `lamport` is evaluated — offline, on identical inputs — without a
  divergence-critical live setting.

## Derived git history (read-only, stock-git compatible)

- **Derived from the log, not the source of truth.** At settle boundaries a full
  node materializes converged bytes into a real git object store (e.g.
  `.context/git`), inspectable via read-only `ctx git` or unmodified `git
  --git-dir`. **No `.git` at the vault root**, so the engine coexists with a
  project's own repo.
- **Deterministic commits** (fixed identity/template, derived non-decreasing times)
  so SHAs converge across nodes — `git log`/`diff`/`bisect` work without making git
  the convergence substrate.
- **Read-only is a data-loss guard.** The repo is engine-owned; a write reaching it
  is silent corruption. The `ctx git` allowlist is **deny-by-default** with its own
  suite asserting every mutating verb is rejected. Restore is `ctx restore`, never
  `git checkout`.
- A **minimal git object writer**, not full git/gitoxide — git's semantics and a
  readable history without the library or its wasm bloat.

## Embeddings / RAG

- Append-only `embeddings(content_hash, model_id, vector)` — content-addressed
  (embed each unique content once) and **model-versioned** (re-embed without
  touching the log).
- **Sync is optional and directional.** Embeddings can exceed a small file's size
  and some devices can't run the model — capable nodes compute and *share* to weaker
  ones; others recompute locally. Native vector/ANN gives semantic search/RAG over
  current *and* historical content.

## Live structured data (out of scope for v1)

**Chat history, agent conversation logs, app/settings state must survive a vault
rollback** — you shouldn't lose this week's chats by checking out last week. Such
data lives in a **separate, non-versioned domain** (its own tables, synced but not
folded into the vault history, never subject to vault rollback). A mutable-state
CRDT (e.g. cr-sqlite, favoring libSQL) is the natural fit if it grows beyond a few
fixed tables. Boundary rule: "survive a rollback?" → live; "roll back with the
vault?" → versioned.

## Deliberately not doing

- **No wall-clock as a live ordering setting in v1.** The causal layer is always on;
  the concurrent tiebreak is `lamport`. `wall_clock` is at most a *post-v1*
  concurrent tiebreak, validated first by the offline re-fold harness, and never a
  key that can reorder across a causal dependency.
- **No fold-parameterizing setting that mutates on a populated vault.** `tiebreak_key`
  is genesis-immutable (no chicken-and-egg with synced config).
- **No CRDT for text by default** — line-level 3-way is sufficient and keeps text in
  the global fold with crisp PITR; a (Yrs) CRDT is a per-path `reclass` opt-in only.
- **No silent loss of concurrent code edits** — code surfaces a conflict.
- **No atomic multi-file commits** — incompatible with continuous real-time sync.
- **No whole-file LWW for text** — binary fallback only.
- **No rename as delete+create** — `file_id` keeps identity and history across
  renames (with identity convergence held to the headline gate).
- **No whole-vault structured CRDT adopted wholesale** — at most a narrow per-file
  text CRDT, opt-in.
- **No "full git" / gitoxide dependency** — a minimal object writer for a readable,
  deterministic, read-only derived history.
- **No server-ordered op-log / cloud platform** — needs a central authority.
- **No cr-sqlite in the vault path** — the deterministic fold needs no CRDT
  extension; cr-sqlite is reserved for the live-data domain.

## Known tradeoffs & open questions

- **`wall_clock` experiment (post-v1).** Evaluate via the offline re-fold harness on
  identical logs, not two live vaults. Promote only if clearly better, as a
  genesis-immutable property with both code paths under the headline gate.
- **`file_id` minting.** v1 = random site-local (splitting on same-path collision is
  visible/recoverable, the safer failure). Revisit only if uncoordinated same-path
  creation proves common enough to want name-derived ids.
- **Rename detection thresholds.** Host signal is reliable; the content-similarity
  fallback must exclude empty/template matches and stay conservative. Tune against
  real Obsidian/agent traces.
- **Code conflict representation** — deterministic in-file markers (loud, localized,
  break the file on every node) vs. side-by-side conflict-copies (cheaper, file
  stays runnable). Both must be byte-deterministic. Start with one, measure.
- **Delete-vs-edit truth table** — v1 default remove-wins; last-touch is the
  alternative. Write the full table (delete/edit/rename × concurrent/causal) before
  coding the fold.
- **Concurrent rename to the same path** — deterministic suffix + flag; confirm the
  UX, and keep it inside the identity-convergence gate.
- **Lamport persistence & same-site concurrency** — counter durably persisted across
  restart; equal-counter same-`site_id` replicas kept total by content-hash;
  `clone`/restore should fork a fresh `site_id` (or warn).
- **Character-level same-line co-editing** — add **Yrs** as a per-path `reclass`
  opt-in only if real usage demands it.
- **PITR precision under skew** — best-effort at the T-boundary; snapshots are exact.
- **Tombstone & blob GC** timing vs. retention horizon (snapshots are GC roots);
  **criss-cross / multiple LCA** broken by lowest content-hash; **routing
  determinism** (extension map + synced config; real axis is human-edited vs.
  machine-rewritten); **embedding granularity** (file vs. chunk) and
  producer/consumer sync policy.
