# Agent Sync Protocol (ASP) — Design Spec

> Successor to **csp** (Context Sync Protocol). The CLI/binary is **`asp`** and the
> protocol is **ASP — Agent Sync Protocol** (csp's `ctx` is renamed to `asp`
> throughout). Keeps csp's spine — a signed/Merkle-id'd event log, a deterministic
> fold, real-time push, automatic debounced commits, P2P with the hub as a peer,
> **SSH-pubkey connection auth**, **one-engine-everywhere wasm**, a **desktop app**
> and an **Obsidian plugin** as thin/full surfaces over that one engine, and a
> stock-git-compatible read-only derived history — and changes what csp got
> expensive or wrong: the **substrate** (git object model → SQLite event log,
> killing the per-edit O(whole-vault) cost), the **ordering** (a two-layer fold — an
> always-on causal layer with a logical-clock concurrent tiebreak; wall-clock is a
> *post-v1 offline experiment*, never a live divergence-critical setting),
> **code-conflict policy** (surface conflicts an agent fixes, not silent drops), and
> **rename handling** (stable per-file identity, not delete+create). It also moves
> csp's node-local `authorized_keys` **file** into a **SQLite table** (keeping the
> `AUTH_KEY` enrollment mechanism), and makes a **robust, csp-style test suite with
> opt-in network-wide telemetry** a first-class deliverable aiming at **100% e2e
> coverage of every edge case**.

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
signing *requirement* — though rows remain Merkle-id'd and therefore tamper-evident,
and **admission is still gated by SSH-pubkey connection auth** (below) so a stranger
can't join the mesh. If that trust assumption ever reverses, parts of this design
change — and because every row already carries its authoring `site_id` (= the
device's ed25519 NodeId), turning on mandatory per-row author signatures is a flag,
not a redesign.

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
- **Authenticated admission, not open mesh** — every connection is mutually
  authenticated with **ed25519 SSH keys**; a node admits only peers whose key is in
  its **node-local authorized set** (now a SQLite table, §*Security*). Pubkey auth
  is the load-bearing trust gate carried forward from csp, unchanged in semantics.
- **One engine everywhere** — the deterministic fold/merge compiles to `wasm32`; a
  browser/Obsidian node computes byte-identical state to a native daemon. The same
  core is driven by the **`asp` CLI**, **Context Desktop** (native full node), and
  the **Obsidian plugin** (wasm/TS thin node) — thin bindings, never re-implemented.
- **Markdown-first**, other formats supported. Text and code both get **3-way
  merge** (prose resolves to a clean single file, code surfaces a conflict);
  binaries get whole-file last-writer-wins.
- **A robust test suite is part of the product.** We build out testing the way csp
  does — unit tests in the core, a determinism conformance gate, multi-process e2e
  against the real `asp` binary, and cross-surface (wasm↔native) byte-identity
  vectors — and we **aim for 100% e2e coverage of every edge case** (§*Testing*).
  Each node can **opt in** to streaming its raw event log + console log to a
  **central debug server** that keeps an append-only, network-wide record of every
  operation on every node, so a divergence can be traced to the exact row where two
  nodes parted.

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
- **`site_id` is the device's NodeId.** The authoring device's `site_id` is its
  ed25519 public key (the same key that authenticates its connections, §*Security*).
  Identity, admission, and ordering therefore share one notion of "who."

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

The engine keeps a private directory at the scope root, **`.asp/`**, holding the
SQLite database (`.asp/asp.db`) and the derived git object store (`.asp/git`) —
**no `.git` at the vault root**, so the engine coexists with a project's own repo.
The node's own keypair is **not** in the DB and is **never synced**: it lives
device-globally at `~/.asp/id_ed25519` (or a reused `~/.ssh` key / SSH agent), so
one device identity serves every vault it joins and survives deletion of any
vault's `.asp/`.

Two representations are kept on purpose: diffs make *sync* a pre-computed `SELECT`
(compute once, ship to every peer); full content-addressed bytes make *point-in-
time* **instant** (a query + blob lookup, never a diff-chain replay).

```sql
-- Immutable bytes, content-addressed: file snapshots, line/blob payloads. LOCAL.
blobs(content_hash TEXT PRIMARY KEY, bytes BLOB);

-- Append-only, SYNCED global log = the source of truth. One row per change.
log(
  id          TEXT PRIMARY KEY,   -- Merkle id = hash of this row (tamper-evident, dedup)
  site_id     TEXT,               -- authoring device = its ed25519 NodeId (§Security)
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
  sig         BLOB,               -- OPTIONAL ed25519 author signature over the row (off by default; §Security)
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

-- Per-peer sync cursor = a version vector across all known devices. LOCAL.
peer_state(site_id TEXT PRIMARY KEY, last_seq INTEGER);

-- Known peers: dial URL + TOFU-pinned listener NodeId (git's 'origin'). LOCAL, not synced.
peers(url TEXT PRIMARY KEY, node_id TEXT, pinned_at INTEGER);

-- Node-local ADMISSION SET (replaces csp's `.context/authorized_keys` FILE; §Security).
-- LOCAL, NEVER synced — per-node trust config, never propagated. One row per device key.
authorized_keys(
  ssh_pubkey  TEXT PRIMARY KEY,  -- full OpenSSH line: 'ssh-ed25519 <base64> [comment]'
  node_id     TEXT NOT NULL,     -- ed25519 pubkey hex = the admission identity (and a peer's site_id)
  expires_at  INTEGER,           -- absolute UTC unix seconds; NULL+never=0 = 'unset, apply default at listen-start'
  never       INTEGER DEFAULT 0, -- 1 = explicit opt-out: never expires, never rewritten by migration
  added_at    INTEGER,
  source      TEXT               -- 'init' | 'env' | 'cli' | 'tofu' | 'enroll'
);
-- Admit iff: never=1 OR (expires_at IS NULL  -- pre-migration grace, never silently rejected)
--                     OR now_unix < expires_at.

-- Synced vault config (routing map, CRDT version pins, ...). Most keys are ordinary
-- folded rows; fold-parameterizing keys (tiebreak_key) are GENESIS-IMMUTABLE.
config(key TEXT PRIMARY KEY, value TEXT);
```

> Large/binary files don't keep a full copy of every version: store periodic
> **keyframes + diffs** (video-codec style) so point-in-time is a bounded replay
> from the nearest keyframe. Not needed for Markdown.

> **Telemetry has no vault schema.** The opt-in debug stream (§*Testing*) is a side
> channel: it ships copies of `log` rows and console lines off-device but writes
> nothing into the vault DB and is never folded. It cannot affect convergence.

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
  `embeddings`, the `authorized_keys`/`peers` tables, and the derived git history
  are computed/kept locally (blobs referenced by a row are fetched on demand).
- **Authenticated session first.** Before any rows move, the two nodes complete the
  ed25519 mutual-auth handshake and the listener applies admission (§*Security*).
  Only over an admitted session do frames flow.
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
- **Transport, phased.** *Phase 1:* WebSockets (default `wss://`, §*Security*) to an
  always-on hub peer. *Phase 2:* **iroh** (QUIC + NAT hole-punching + relay
  fallback) for true device-to-device P2P — LAN-speed direct sync, the hub demoted
  to relay-of-last-resort, connection survival across Wi-Fi↔cellular. Phase 2 is an
  upgrade, not required for correctness.

## Security & authentication

Carried forward from csp essentially unchanged — **connection-level admission is the
trust gate** — with one storage change: the authorized set moves from the
`.context/authorized_keys` **file** to the `authorized_keys` **SQLite table**
(§*Data model*). Same semantics, same OpenSSH-compatible key text, now a transacted,
queryable, wasm-portable table instead of a side file.

- **Node identity is an SSH key.** Every node has an ed25519 keypair; the public key
  is its durable identity (`site_id`/NodeId), in standard OpenSSH public-key format
  (`ssh-ed25519 AAAA… [comment]`). An existing user SSH key may be reused, and
  signing MAY be delegated to a running SSH agent rather than holding the private key
  in process. The key is device-global (`~/.asp/id_ed25519`), never stored in the
  vault, never synced; a per-vault key is an opt-in for stronger isolation.

- **Authorization via the node-local `authorized_keys` table — the load-bearing
  trust gate.** A listening (full) node admits only peers whose public key has a row
  in its `authorized_keys` table. **This is the *only* trust gate**: rows received
  over an admitted connection are integrated regardless of who originally authored
  them — admission gates *trust*, the row's Merkle id (and optional `sig`) gate
  *integrity*. The set is **node-local and NOT synced**: authorization is per-node
  config, never propagated. Managed via `asp authorize <pubkey>` / `asp revoke
  <pubkey>`, the `ASP_AUTHORIZED_KEYS` env var (merge-on-start, idempotent), or
  seeding at `asp init`. `asp key` prints a node's own public key for sharing.
  (Trade-off, deliberate: adding a key is done on each listener and does not
  propagate — simpler, and removes the "a peer pushes a malicious key to every node"
  vector. Listeners are few; writer admission converges to managing one set per
  relay.) *Moving the set into SQLite keeps every property csp's file had —
  OpenSSH-compatible key text, comments, per-key expiry — while gaining transactional
  edits, a clean `asp auth list` query, and identical behavior on the wasm surface
  through the same storage trait.*

- **Bootstrap: trust-on-first-use, bounded to the empty-set window.** When a
  listening node has **no** rows in `authorized_keys` yet (genuine first-peer
  bootstrap) AND no auth key is configured, it MAY trust-on-first-use: the first
  connecting key is inserted into `authorized_keys`, and from then on the local set
  is authoritative. TOFU applies *only* while the set is empty — never as an ongoing
  policy. `ASP_AUTHORIZED_KEYS` (or `asp authorize` / seeding at `asp init`) may
  pre-populate the set so the TOFU window never opens. `--no-tofu` / `ASP_NO_TOFU`
  disables TOFU entirely for hardened / internet-exposed deployments. **Auth keys
  (below) implicitly disable TOFU.** *Honest caveat:* an internet-reachable listener
  with an empty set, no auth key, and TOFU on trusts whichever key connects first —
  operators exposing a fresh listener publicly must pre-seed keys, configure an auth
  key, or disable TOFU.

- **Bootstrap: auth-key enrollment (the `AUTH_KEY` mechanism, kept from csp; the
  recommended path for fresh deployments).** A listener MAY be configured with one or
  more **auth keys** — shared secrets that authorize a *new* peer to enroll itself
  into `authorized_keys`. Configured via `ASP_AUTH_KEY` (or `--auth-key`;
  comma-separated for multiple, supports rotation). On connect the client presents
  the secret in the WebSocket upgrade — preferred `Authorization: Bearer <key>`;
  fallback for clients that cannot set headers (e.g. browser `WebSocket`): the
  `?auth_key=<key>` query parameter or the `bearer.<key>` subprotocol. On match the
  upgrade succeeds and, after the mutual ed25519 handshake completes, the listener
  **inserts the client's public key as a row in `authorized_keys`** (with a default
  expiry) and proceeds normally. **From the next connection onward the client is a
  plain authorized peer** — the auth key is not used again unless the row is removed
  or expires. A mismatching key returns HTTP 401 at the upgrade with no fall-through,
  so operator misconfiguration fails loudly. Absent the header entirely the upgrade
  proceeds to the handshake — already-enrolled peers connect without the auth key.
  The auth key is **a bootstrap secret, not an API key**: rotating/removing it stops
  *future* enrollments but never severs already-enrolled peers (revoke a specific
  peer by deleting its row). A compromised shared secret therefore has a bounded
  blast radius — only the new pubkeys it could enroll while still valid.

- **Per-key expiry in the table.** Each row carries an `expires_at` (and a `never`
  flag), mirroring csp's `authorized_keys` comment tokens:
  - `expires_at = <unix s>` — absolute expiry. After that UTC instant the listener
    refuses admission via this row (the row stays for audit but is skipped at admit
    time).
  - `never = 1` — explicit opt-out, never expires; never rewritten by listen-start
    migration.
  - `expires_at IS NULL` & `never = 0` — "unset, apply default": admitted at run time
    (so a hand-inserted/seeded key is never silently rejected) and rewritten by
    listen-start migration to `today + ASP_DEFAULT_KEY_TTL`.

  At startup, when a listener comes up (`asp watch --listen`), it scans the table and
  **fills `expires_at` on any unset row** with `today + ASP_DEFAULT_KEY_TTL` (default
  90 days). `never=1` rows and rows that already have an `expires_at` are left
  untouched. The migration is a single transaction, idempotent, and logged with a
  one-line summary. Enrollment writes rows with `expires_at = today + default`. An
  expired row re-enrolling via a valid auth key refreshes its `expires_at` to a fresh
  default-TTL window — expired peers re-enroll through the front door. Default TTL is
  `ASP_DEFAULT_KEY_TTL` (`90d`, `1y`, or `never`). Per-key overrides at the CLI:
  `asp authorize <pubkey> --ttl 30d|never`, `asp auth extend <peer> 30d`, `asp auth
  list` to inspect. The CLI also accepts a `ttl=NNd added=YYYY-MM-DD` input form,
  normalizing to absolute `expires_at`. Clock skew is irrelevant at day-granularity.

- **Mutual authentication.** The handshake requires each side to sign, with its
  ed25519 key, a transcript covering both nonces and a binding to the underlying
  transport, so a captured handshake cannot be replayed/relayed onto another channel.
  Both directions authenticate: a connecting node also verifies the listener's key,
  enabling key pinning (`asp clone` pins the listener's NodeId into `peers`).

- **Advertised channel binding (the listener owns it).** The transport binding mixed
  into the signed transcript is **not** each side's local view of the certificate
  (that desynchronizes the moment a benign TLS-terminating proxy sits in front of the
  listener). Instead, the **listener advertises one channel-binding value in its
  `Hello`** — the SHA-256 of the certificate it serves, or an all-zero/empty
  *binding-disabled* marker when it runs `--no-tls` behind a TLS terminator — and
  **both sides sign over that single advertised value**. Separately, and only as an
  explicit check with its own distinct error, the connector enforces the binding:
  - *Advertised binding disabled* (all-zero/empty): degraded mode. The connector
    skips the certificate comparison; trust falls back to the **TOFU-pinned listener
    identity** (the transcript covers the listener's NodeId, which a MITM cannot
    forge). Required behind a re-terminating reverse proxy.
  - *Binding advertised but unobservable* (plaintext `ws://`, or a browser
    `WebSocket` that cannot read the peer cert): degraded as above; SHOULD warn.
  - *Binding advertised and observable*: the connector MUST verify the advertised
    fingerprint equals the certificate it actually saw and MUST abort with a distinct
    channel-binding error on mismatch — the live MITM / cert-substitution defense.
  A handshake-transcript or framing change bumps the wire `proto` version so skew is
  reported as a clear version-mismatch, not an opaque signature error.

- **Per-row integrity (signatures optional by default).** Every row is
  **content-addressed by its Merkle `id`**, so a corrupted or substituted row cannot
  masquerade as another and is never referenced by a valid DAG — tamper-evidence is
  inherent. Because all nodes are trusted (one person's devices), **per-author
  signing is not required for admission**; the `sig` column is reserved and, when
  the trust assumption reverses, can be made mandatory (receivers verify `sig`
  against `site_id` and drop unsigned/invalid rows) without a schema or capture
  change. Connection-level admission remains the trust gate either way.

- **Single-writer protection.** A `site_id` is the ed25519 key. Reusing it across
  *different* vaults is fine; the *same key actively writing two replicas of one
  vault* is the hazard. The Lamport/`seq` counters are durably persisted; `asp clone`
  / restore MUST fork a fresh `site_id` or warn rather than resume authoring under a
  possibly-live key. Correctness survives a violation (causal fold + Merkle ids +
  content-hash tiebreak keep equal-counter same-site replicas total); this protection
  keeps history clean.

- **Transport confidentiality.** Default transport is **`wss://`**: a listener serves
  TLS using a **self-signed certificate it generates and persists** under the
  never-synced `.asp/` (ASP ships **no embedded CA** — the X.509 layer is *not* the
  trust boundary). Connectors accept any server certificate at the TLS layer; trust
  is established by the ed25519 mutual-auth handshake, which binds the channel and
  enables listener-key pinning. TLS adds confidentiality only. **`--no-tls` /
  `ASP_NO_TLS`** opts a listener into plaintext `ws://` for running behind a fronting
  proxy that already terminates TLS or on a trusted/local network. A listener reached
  **through a TLS-terminating reverse proxy (Fly.io, Railway, Render, Cloudflare
  Tunnel, …) MUST run `--no-tls`**: the proxy re-terminates TLS, so only the
  advertised binding-disabled marker keeps the handshake coherent. Connectors still
  dial `wss://` so the proxy hop stays encrypted; the pinned listener identity
  authenticates the peer.

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
  vault, where a delete is usually final. The alternative — **last-touch-in-fold-order
  wins**, where a higher-ordered concurrent edit re-creates the file — is simpler and
  more uniform but surprises on intentional deletes. Write the full truth table
  (delete vs edit vs rename, concurrent and causal) before coding the fold;
  tombstones GC past the retention horizon (snapshots excepted).

## Implementation: one engine, wasm everywhere

A hard structural rule from csp: **the protocol is implemented once, in Rust, and
runs identically on every surface.**

- **Single core crate (`asp-core`)** — object/oid model, the deterministic fold +
  3-way merge, the sans-IO sync `Session` (handshake, anti-entropy, integrate),
  identity & auth, wire framing, scope/ignore, config, and the `authorized_keys`
  admission logic. **All** convergence/merge/auth logic lives here.
- **Compiles to `wasm32` unchanged.** Native daemon, desktop, and in-browser/
  Obsidian node run the **identical** fold/merge and compute **byte-identical**
  state. I/O is injected via traits (storage, transport, clock, rng); only
  platform-bound pieces (on-disk SQLite backend, listen socket, TLS) are `cfg`-gated
  behind a native feature. The browser/Obsidian node uses **libSQL-over-OPFS** through
  the same storage trait, so its `authorized_keys`/`blobs`/`log` tables and fold are
  the same code as native.
- **Sans-IO `Session`.** The replication state machine consumes inbound frame bytes
  and emits outbound frame bytes + effects, with no sockets/fs/clock of its own. The
  native `asp` driver (tokio) and the wasm/SDK node are both thin drivers over the
  *same* `Session` — the handshake, anti-entropy, and integrate are executed by one
  codebase on every surface.
- **Lean by construction.** The core carries no heavy general-purpose deps where a
  differentially-tested hand-rolled equivalent will do (e.g. a hand-rolled gitignore
  matcher instead of `regex`, a flat-TOML codec instead of `toml`). Each replaced
  crate is kept as a **dev-only differential oracle** (§*Testing*). **Any CRDT, if
  adopted, is held to the same bar:** wasm-byte-identical native↔browser,
  version-pinned in the synced config.
- **The wasm/TypeScript SDK** is `asp-core` compiled to **one wasm module** plus thin
  TS bindings and injected host adapters (filesystem/OPFS, WebSocket). The wasm bytes
  are inlined at build time and `init`'d once (no runtime fetch — the only path that
  loads WebAssembly reliably in a mobile WebView). The high-level engine binding is
  the *real* full engine; it computes its own byte-identical state via the same
  `compute_main`/merge/fold as native. Low-level functions are retained for the
  cross-surface conformance vectors (§*Testing*).
- **Thin bindings, every surface.** The native CLI (`asp`), Context Desktop, and the
  Obsidian plugin are thin drivers over the same `Session`/core. Any cross-surface
  behavioral difference is a bug. A feature is not "done" until it is reachable from
  the CLI **and** the SDK and covered by §*Testing* tests.
- **Headline gate — two interlocking properties, both CI-blocking.**
  1. **Merge determinism.** N simulated nodes, all delivery orders (offline-then-
     merge, gossip/mesh, same-`site_id` concurrency), converge to **identical** state;
     the wasm node converges **bit-for-bit** with native against shared test vectors.
     Build the reference fold and property-test order-determinism **first** — if it
     can't be made deterministic in practice, the architecture doesn't work.
  2. **Identity convergence.** Two devices, **concurrent same-path create** and
     **unpaired rename** (A pairs it, B sees delete+create), must converge to the
     *same* set of `file_id`s and the *same* path assignments (including deterministic
     ` (n)` suffixing) across all delivery orders.
- **Dev-only re-fold harness.** Re-folds a captured log under an alternate concurrent
  tiebreak (and other what-ifs) and diffs the materialized output. This is how
  `wall_clock` vs `lamport` is evaluated — offline, on identical inputs — and is also
  the engine behind the telemetry collector's divergence-bisect (§*Testing*).

## Surfaces

All three surfaces are thin drivers over `asp-core` — "one core, thin bindings."
They differ only in **node tier** (full vs thin, csp's platform-derived role) and the
**engine profile** they load (native odb + listen socket vs wasm/OPFS, outbound-only).

### The `asp` CLI (native full node)

A single binary, `asp`, exposes the **full** engine capability set — nothing the
protocol can do may be CLI-inaccessible. Command sketch:

- `asp init [path]` — create a new scoped vault and this node's SSH-key identity.
- `asp clone <url> [into]` — bootstrap a new node from a listening peer: authenticate,
  full catch-up, materialize, write local identity/config, **pin the listener's
  NodeId and record the source URL as a peer** (git's `origin`). `--watch` stays
  running as the daemon.
- `asp watch [--listen [addr]]` — **the primary long-running command.** Open the
  vault, watch the scoped tree (debounced auto-commit, self-write suppression),
  connect to configured peer(s), run the realtime sync loop. `--listen` additionally
  accepts inbound peers (relay/hub) and binds `0.0.0.0:9000` by default (unprivileged;
  not 443). Default transport `wss://`; `--no-tls` serves `ws://`. Emits
  operator-visible logging (peer connect / handshake outcome with reason / catch-up /
  integrate / commit) at `INFO`.
- `asp key` — generate / show the node SSH key (OpenSSH format); use an SSH agent if
  available.
- `asp authorize <pubkey> [--ttl 30d|never]` / `asp revoke <pubkey>` — manage the
  `authorized_keys` **table**. `asp auth list` / `asp auth extend <peer> <ttl>`.
- `asp status` — node identity, peers, sync state, head/`main` SHA.
- `asp snapshot <name>` / `asp restore <name|time>` — point-in-time recovery.
- `asp log` — history (wraps the derived git history).
- `asp git <args…>` — **read-only** git inspection of the engine-owned repository
  (`.asp/git`): deny-by-default allowlist (`log`, `show`, `diff`, `status`, `blame`,
  `cat-file`, `ls-tree`, `ls-files`, `rev-list`, `rev-parse`, `grep`, `for-each-ref`,
  `describe`, `shortlog`, `reflog show`, …). Every mutating verb is **refused** with a
  pointer to the proper `asp` command.
- `asp scope` — show / edit the synced scope and `.aspignore`.
- `asp completions <bash|zsh|fish|powershell>`.

Ergonomics are acceptance criteria, not nice-to-haves: `--help` everywhere; shell
completions; `--json` machine-readable output on read/status commands; no required
interactive prompts; documented exit codes. **Every deployment knob has all three
forms — a CLI flag, an `ASP_*` env var, and a config-file key — resolved flag > env >
config, non-destructively** (a flag/env value never silently rewrites the persisted
config). The documented exceptions: the vault locator (`--dir`/`ASP_DIR`, flag+env
only, since it locates the config file itself) and `--authorized-keys`/
`ASP_AUTHORIZED_KEYS` (whose persisted form is the `authorized_keys` table, not vault
config). Knobs: `--dir`/`ASP_DIR`, `--no-tls`/`ASP_NO_TLS`, `--listen`/`--port`/
`PORT`, `--log`/`ASP_LOG`, `--debounce`/`ASP_DEBOUNCE`, `--authorized-keys`/
`ASP_AUTHORIZED_KEYS`, `--auth-key`/`ASP_AUTH_KEY`, `--default-key-ttl`/
`ASP_DEFAULT_KEY_TTL`, `--no-tofu`/`ASP_NO_TOFU`, and `--telemetry`/`ASP_TELEMETRY`
(§*Testing*). A hosted listener is fully configurable by flags *or* env with no file
editing, e.g. `asp watch --listen --no-tls --dir /data/vol --authorized-keys "$KEYS"`.

### Context Desktop (native full node, Tauri)

A normal desktop application (resizable window, dock/taskbar presence, à la
OrbStack/Docker Desktop) that **also** installs a menu-bar (tray) status indicator;
closing the window leaves it syncing in the background.

- **Tauri v2**, Rust backend + web frontend (React + TypeScript, Vite, Bun, Tailwind +
  shadcn/ui, React Router, Biome). The backend **links the native `asp-core` crate
  directly** at the full-node profile (merge engine + on-disk SQLite/git compiled in)
  — architecturally a sibling of the `asp` CLI, **not** a consumer of the wasm/TS SDK.
  No `asp` subprocess, no FFI shim, no wasm.
- **One background process, N engine instances** — one `asp-core` engine per enabled
  folder (the in-process equivalent of one `asp watch` per folder). Disabling a folder
  tears its instance down cleanly; re-enabling runs normal catch-up. Each folder with
  "allow connections" on additionally binds a per-folder **listen socket** (one
  listening folder = one socket = one port; the literal `asp watch --listen` mapping).
- **TOFU surfaced natively.** When the engine opens the TOFU window (empty
  `authorized_keys`, no auth key), the app raises a native prompt to approve the
  connecting key; otherwise the user authorizes a key explicitly. Identity is legible:
  view/copy the device SSH key, reuse a `~/.ssh` key or agent, opt into a per-vault
  key. **No conflict-resolution UI** — the engine resolves deterministically; the app
  may *notify* that a same-region edit was superseded and offer recovery, nothing more.
- **HARD INVARIANT — no protocol logic in the app.** Every sync/merge/identity/auth/
  history behavior is a call into `asp-core`; the app contributes process lifecycle,
  watcher/listen host glue, UI, and its own small app-level config. Any behavioral
  difference from the `asp` CLI is a bug.

### Obsidian plugin (thin node, wasm/TS SDK)

Keeps an Obsidian vault byte-identical across a user's devices, running entirely
inside Obsidian on **desktop (Electron) and mobile (Capacitor WebView)** —
`isDesktopOnly: false`.

- **A thin node on the wasm/TS SDK.** It holds a complete local working copy, authors
  its own primitive rows offline, and converges on reconnect, but it **never runs the
  multi-tip merge and never listens/relays** (csp's thin-node HARD INVARIANT). It makes
  *outbound* connections to a **full node in listen mode** — an `asp watch --listen`
  process or Context Desktop's per-folder listener — which carries deep history and
  serves the deterministic merged tree back. A vault synced only between thin nodes
  (e.g. two phones, no full node) is explicitly unsupported and will not converge.
- **One wasm module + thin TS bindings**, wasm inlined at build time and `init`'d once
  (reliable in a mobile WebView). Storage is **libSQL-over-OPFS** through the same
  storage trait as native, so the on-disk vault is **CLI-interchangeable**: identical
  `.asp/`, identical config, same device-key resolution, whether driven by `asp` or the
  plugin.
- **Host glue only.** Obsidian-vault file I/O, the Obsidian event → SDK push path, the
  outbound WebSocket client, settings UI, lifecycle, a status bar, and a log buffer/
  modal (which is also the local source for opt-in telemetry, §*Testing*). Module
  decomposition (entry/lifecycle, sync controller, host bridge, storage adapter,
  identity store, settings + settings tab, path filter, catch-up/reconcile, status
  bar) is host-glue factoring, not protocol. **No protocol/merge/fold/ordering/auth
  logic lives in the plugin** — any behavioral difference from the `asp` CLI for the
  same engine operation is a bug. No conflict markers, no git UI, no editing of
  `.asp/`.

## Derived git history (read-only, stock-git compatible)

- **Derived from the log, not the source of truth.** At settle boundaries a full node
  materializes converged bytes into a real git object store (`.asp/git`), inspectable
  via read-only `asp git` or unmodified `git --git-dir`. **No `.git` at the vault
  root**, so the engine coexists with a project's own repo.
- **Deterministic commits** (fixed identity/template, derived non-decreasing times) so
  SHAs converge across nodes — `git log`/`diff`/`bisect` work without making git the
  convergence substrate.
- **Read-only is a data-loss guard.** The repo is engine-owned; a write reaching it is
  silent corruption. The `asp git` allowlist is **deny-by-default** with its own suite
  asserting every mutating verb is rejected. Restore is `asp restore`, never `git
  checkout`.
- A **minimal git object writer**, not full git/gitoxide — git's semantics and a
  readable history without the library or its wasm bloat.

## Embeddings / RAG

- Append-only `embeddings(content_hash, model_id, vector)` — content-addressed (embed
  each unique content once) and **model-versioned** (re-embed without touching the
  log).
- **Sync is optional and directional.** Embeddings can exceed a small file's size and
  some devices can't run the model — capable nodes compute and *share* to weaker ones;
  others recompute locally. Native vector/ANN gives semantic search/RAG over current
  *and* historical content.

## Live structured data (out of scope for v1)

**Chat history, agent conversation logs, app/settings state must survive a vault
rollback** — you shouldn't lose this week's chats by checking out last week. Such data
lives in a **separate, non-versioned domain** (its own tables, synced but not folded
into the vault history, never subject to vault rollback). A mutable-state CRDT (e.g.
cr-sqlite, favoring libSQL) is the natural fit if it grows beyond a few fixed tables.
Boundary rule: "survive a rollback?" → live; "roll back with the vault?" → versioned.

## Testing, verification & telemetry

**The test suite is part of the product, built out the way csp builds it.**
Correctness here is a release gate, not aspirational: CI must run all of the below
green before any release. The explicit target is **100% e2e coverage of every edge
case** — every row in the matrix below has a test that spawns real nodes and asserts
convergence, not an in-process shortcut.

**The csp-style layered approach (kept):**

- **Unit tests (`asp-core`).** Object model, total order, version vectors, the
  deterministic fold, conflict resolution per `merge_class`, scope filtering, the
  `authorized_keys` table (parse/expiry/migration), the auth handshake, catch-up.
- **Determinism conformance suite (the headline gate).** N simulated nodes; identical
  rows fed in shuffled delivery orders; assert identical materialized state *and*
  identical derived `main` SHA + tree. A hard build gate. Includes the **identity-
  convergence** gate (concurrent same-path create; unpaired rename) converging to the
  same `file_id` set and ` (n)` suffixing across all orders.
- **Multi-process end-to-end tests.** Spawn multiple **real `asp` processes** in
  isolated temp dirs (each with its own `$HOME` device identity), including a listening
  relay; exercise the matrix below; assert byte-identical working trees + identical
  derived `main` SHA on every node, PITR correctness, and genuine git-coherence (an
  unmodified `git` can `log`/`checkout` the derived repo).
- **Cross-surface interop.** A wasm/TS node must interoperate with a native node —
  handshake, replication, identical convergence and SHAs — with a **byte-identity
  vector check** (wasm output == shared test vectors == live `asp`) and an
  **SDK⇄real-`asp` parity** e2e (spawn the real binary; assert bidirectional
  convergence). Desktop (native full) and Obsidian (wasm thin) are validated through
  the same harness; their behavior parity with the CLI is structural, not hand-checked.
- **Differential-equivalence oracles (build gate).** Every hand-rolled substitute for a
  dropped general-purpose dep proves byte-for-byte equivalence to the original (the
  scope matcher vs the former `regex` over tens of thousands of generated cases; the
  config codec round-trips and emits TOML the real parser reads back identically).
- **No-regression / parity requirement.** The CLI + SDK must be at least as capable and
  ergonomic as a mature reference sync tool, verified by the e2e suite exercising the
  *entire* command surface — "we didn't lose anything" is a passing test.

**100% e2e edge-case matrix (every row is a spawned-node test):**

- *Sync core:* two-peer create/modify/delete; clone + full catch-up; offline →
  reconnect catch-up via version vectors (sends exactly what's missing); empty
  directories; large/binary keyframe+diff PITR; bounded/chunked frames on a throttled
  link.
- *Merge:* disjoint concurrent text edits both survive; same-region text resolves
  deterministically (loser in history); code same-region **conflict surfaced**
  byte-deterministically; binary whole-file LWW; `reclass` boundary seeds a fresh base
  and stops reinterpreting older line-diffs.
- *Identity & renames:* host-signal rename keeps `file_id` + edit history; concurrent
  rename + edit both apply; concurrent same-path create splits with ` (n)` suffix;
  unpaired rename (A pairs, B sees delete+create) converges to the same `file_id` set;
  rename-into-occupied and concurrent-rename-to-same-path deterministic suffixing.
- *Delete truth table:* delete vs edit vs rename × concurrent/causal; v1 remove-wins
  (concurrent edit does not resurrect, kept in history); tombstone GC past retention.
- *Ordering & re-fold:* late low-`lamport` row folds at position and recomputes only
  affected files + downstream (`fold_cache` hits elsewhere); Lamport durable across
  restart; equal-counter same-`site_id` replicas kept total by content-hash; `clone`/
  restore forks a fresh `site_id`.
- *Capture:* startup reconciliation (edits while daemon off; crash-buffer recovery);
  bootstrap-before-publish (no false-add / no resurrection); debounce squash net-effect
  (typed-then-deleted → nothing); self-write echo suppression.
- *PITR:* named snapshot exact, skew-free, GC-root; "as of T" best-effort; instant via
  memoized checkpoint + bounded replay.
- *Topology:* relay/hub forward-then-merge; relay mesh; two clones through one relay;
  transitive relay trust (multi-writer single-relay converges without each enumerating
  the other's key).
- *Auth (pubkey):* `authorized_keys`-table admission; **`AUTH_KEY` enrollment**
  (`Bearer` / `?auth_key=` / `bearer.<key>` subprotocol; 401 on mismatch, no
  fall-through; absent header proceeds for already-enrolled peers); **TOFU** bounded to
  the empty-set window; `--no-tofu`; per-key **expiry** + listen-start default-fill
  migration (idempotent) + `auth extend`/`auth list`; expired peer re-enrolls and
  refreshes TTL; same-`site_id`/single-writer protection.
- *Transport:* `wss://` self-signed default; `--no-tls` plaintext behind a proxy;
  advertised channel binding (disabled-marker behind a re-terminating proxy; mismatch
  aborts with a distinct error); listener-key pinning on `clone`.
- *Derived git:* read-only allowlist deny-by-default (assert every mutating verb is
  rejected); deterministic SHAs converge cross-node; coexists with a project's own
  `.git`.
- *Cross-surface:* native ↔ wasm (browser/Obsidian) ↔ desktop full node all converge
  byte-identical against shared vectors; SDK⇄real-`asp` bidirectional parity.
- *Embeddings:* optional/directional sync (capable node shares, weak node recomputes);
  model-versioned re-embed without touching the log.

### Telemetry — opt-in network-wide debug log

A divergence in a distributed deterministic fold is only as debuggable as your record
of **what each node did and in what order**. So each node can **opt in** (off by
default) to streaming, to a **central debug server**, two things:

1. **The raw event log** — every `log` row it appends or integrates, verbatim (Merkle
   `id`, `site_id`, `lamport`, `seq`, `ts`, `kind`, `file_id`, hashes, and the diff
   payload), and
2. **The console/operational log** — its structured INFO/DEBUG lines: capture
   decisions, fold steps, materialize writes, handshake outcomes, merge/conflict
   resolutions, path-collision suffixing, snapshot/GC events.

- **The debug server keeps an append-only, network-wide record.** Every frame is
  stamped with its node id, vault id, and a monotonic local clock, then appended — so
  the collector holds an **append-only log of every operation from every node in the
  network**, reconstructable into the exact cross-node interleaving. Each row's own
  `(site_id, seq, lamport, id)` makes the reconstruction unambiguous.
- **Strictly a side channel.** Telemetry is **not** on the sync path and **never**
  affects convergence (it writes nothing into any vault DB and is never folded).
  Enabling it is the only thing that ships raw content off-device, so it is **opt-in
  only** — a dev / CI / triage tool, configured by `--telemetry <url>` /
  `ASP_TELEMETRY` / config `telemetry` (all three forms; off when unset). It degrades
  to a no-op if the collector is unreachable; it never blocks or slows the engine.
- **It turns "it diverged somewhere" into "row X is where A and B parted."** Because
  the engine is deterministic, the collector can replay each node's received-row stream
  through the **same dev re-fold harness** (§*Implementation*) and **bisect to the
  first row at which two nodes' materialized output differs** — naming the exact row,
  author, and lamport position of the divergence, plus the console context around it.
  This is the observability layer that makes the 100% e2e goal actionable: when a
  property test or e2e case fails (locally or in CI), the append-only cross-node log is
  the ground truth of every operation, in order, on every node.

## Deliberately not doing

- **No wall-clock as a live ordering setting in v1.** The causal layer is always on;
  the concurrent tiebreak is `lamport`. `wall_clock` is at most a *post-v1* concurrent
  tiebreak, validated first by the offline re-fold harness, and never a key that can
  reorder across a causal dependency.
- **No fold-parameterizing setting that mutates on a populated vault.** `tiebreak_key`
  is genesis-immutable (no chicken-and-egg with synced config).
- **No CRDT for text by default** — line-level 3-way is sufficient and keeps text in
  the global fold with crisp PITR; a (Yrs) CRDT is a per-path `reclass` opt-in only.
- **No silent loss of concurrent code edits** — code surfaces a conflict.
- **No atomic multi-file commits** — incompatible with continuous real-time sync.
- **No whole-file LWW for text** — binary fallback only.
- **No rename as delete+create** — `file_id` keeps identity and history across renames
  (with identity convergence held to the headline gate).
- **No whole-vault structured CRDT adopted wholesale** — at most a narrow per-file text
  CRDT, opt-in.
- **No "full git" / gitoxide dependency** — a minimal object writer for a readable,
  deterministic, read-only derived history.
- **No server-ordered op-log / cloud platform** — needs a central authority.
- **No syncing of the authorized set** — `authorized_keys` is node-local, never
  propagated; the table is a storage change from csp's file, not a policy change.
- **No embedded certificate authority** — TLS adds confidentiality only; trust is the
  ed25519 mutual-auth handshake + node-local admission.
- **No mandatory per-author signatures in v1** — rows are Merkle-id'd (tamper-evident);
  the `sig` column is reserved for when the trust assumption reverses.
- **No protocol logic in any surface** — the Desktop app and Obsidian plugin are thin
  bindings over `asp-core`; a behavioral difference from the CLI is a bug.
- **No telemetry on by default** — the central debug log is opt-in, a side channel, and
  never on the convergence path.
- **No cr-sqlite in the vault path** — the deterministic fold needs no CRDT extension;
  cr-sqlite is reserved for the live-data domain.

## Known tradeoffs & open questions

- **`wall_clock` experiment (post-v1).** Evaluate via the offline re-fold harness on
  identical logs, not two live vaults. Promote only if clearly better, as a
  genesis-immutable property with both code paths under the headline gate.
- **`file_id` minting.** v1 = random site-local (splitting on same-path collision is
  visible/recoverable, the safer failure). Revisit only if uncoordinated same-path
  creation proves common enough to want name-derived ids.
- **Rename detection thresholds.** Host signal is reliable; the content-similarity
  fallback must exclude empty/template matches and stay conservative. Tune against real
  Obsidian/agent traces.
- **Code conflict representation** — deterministic in-file markers (loud, localized,
  break the file on every node) vs. side-by-side conflict-copies (cheaper, file stays
  runnable). Both must be byte-deterministic. Start with one, measure.
- **Delete-vs-edit truth table** — v1 default remove-wins; last-touch is the
  alternative. Write the full table (delete/edit/rename × concurrent/causal) before
  coding the fold.
- **Concurrent rename to the same path** — deterministic suffix + flag; confirm the UX,
  and keep it inside the identity-convergence gate.
- **Lamport persistence & same-site concurrency** — counter durably persisted across
  restart; equal-counter same-`site_id` replicas kept total by content-hash;
  `clone`/restore should fork a fresh `site_id` (or warn).
- **`authorized_keys` table vs file** — the table is node-local and unsynced like the
  file was; confirm the listen-start default-fill migration is idempotent and that
  `asp authorize`/`revoke`/`auth list` stay OpenSSH-key-text compatible for operators
  who paste keys.
- **Auth-key (`AUTH_KEY`) rotation** — comma-separated multi-secret rotation stops
  future enrollments without severing enrolled peers; confirm the 401-vs-fall-through
  behavior under each client transport (header / query / subprotocol).
- **Telemetry privacy & retention** — it ships raw content off-device, so opt-in only;
  decide collector retention, redaction options, and whether CI runs always-on while
  field deployments default off.
- **Character-level same-line co-editing** — add **Yrs** as a per-path `reclass` opt-in
  only if real usage demands it.
- **PITR precision under skew** — best-effort at the T-boundary; snapshots are exact.
- **Tombstone & blob GC** timing vs. retention horizon (snapshots are GC roots);
  **criss-cross / multiple LCA** broken by lowest content-hash; **routing determinism**
  (extension map + synced config; real axis is human-edited vs. machine-rewritten);
  **embedding granularity** (file vs. chunk) and producer/consumer sync policy.
