# 0014 — Ghost-add guard + explicit bootstrap mode (alternative fix for resurrection)

**Severity:** High  **Status:** Open  **Owner:** —

## Summary

Alternative design for the resurrection bug in [[0012]]. Instead of
tree-resident tombstones or runtime peer-reach gating, this proposal
fixes the root cause at the *publish site*: a primitive whose tree
adds a path that the broader DAG has already deleted is a **ghost-add**,
and ghost-adds are filtered locally before they're authored and (as a
backstop) when they're integrated. Bootstrap intent is declared once
at `init` / `join`, not inferred from peer reachability at every
commit.

The user-facing contract is **fully invisible**: no new CLI verbs, no
prompts, no notifications. The signal that distinguishes "stale file
from an offline device" from "user creating a new file at a
previously-deleted path" is `state.materialized` — the engine's
existing record of what bytes it last wrote to disk. The only
user-visible artifact is a `.context/orphans/` folder that surfaces
naturally in the vault for the rare residual conflict case.

This sidesteps the structural problems with [[0012]]'s design
(tombstone re-create ambiguity, GC unsolvability, wire-format
breakage, no-clobber invariant violation, "all peers reachable"
quorum question) while resolving the same bug cases.

## Why

The resurrection bug is not a weak-delete-propagation problem. Deletes
propagate fine when C's parent pointer reflects C's actual knowledge.
The bug is that C publishes a primitive whose *parent* claims C knows
nothing while C's *tree* asserts a file exists that the DAG already
deleted. The contradiction lives at the publish site, not the merge
site — fix it there.

A primitive's `tree` is implicitly a delta against `parent.tree`. Call
a primitive a **ghost-add for path `q`** iff:

1. `q ∈ tree(P)` (P asserts q exists), and
2. `q ∉ tree(parent(P))` (P is adding q, not propagating it), and
3. The most recent primitive in `parent(P)`'s ancestor closure that
   touched `q` is a *delete* (i.e. P's own parent chain already
   contains a deletion of q that P is contradicting), where **"most
   recent" is defined as the §5.1 strict total order** (`counter`,
   `node`, then commit-SHA bytewise) restricted to primitives in
   `parent(P)`'s ancestor closure that mention `q`. "Touched" means
   `q ∈ tree(prim)` differs from `q ∈ tree(prim's parent)` — either a
   transition `absent → present` (add/re-add) or `present → absent`
   (delete).

The predicate is **content-only and node-independent** — every node
computes the same answer from the same `known` set and the same store.
That's the lever that lets us filter ghost-adds deterministically
without changing the fold or the tree format.

## Design

Three layers. Layer 2 covers state-loss / fresh-device cases by
waiting; Layer 1 catches the steady-state stale-disk cases; Layer 3 is
the adversarial backstop.

### Layer 1 — Local pre-publish ghost-add guard

In `commit_scoped` (`engine.rs:297-341`), after computing `scoped` and
the prospective `parent = self.main`, before authoring the primitive:

For each path `p ∈ scoped`:

- If `p ∈ tree(parent)` and bytes match → no-op (already there).
- If `p ∈ tree(parent)` and bytes differ → genuine modify, proceed.
- If `p ∉ tree(parent)`:
  - Walk `parent`'s ancestor closure for the most recent primitive
    that touched `p` (§5.1 ordering, definition above).
  - None found → genuine novel add, proceed.
  - Found, and it's an *add* → re-emerged path, proceed.
  - Found, and it's a *delete* → consult the **intent signal**:
    - **`state.materialized.contains_key(p)`** (the device knew about
      this file before and never cleaned it up) → **ghost-add.** Drop
      `p` from the to-be-published tree and quarantine the on-disk
      file to `.context/orphans/<utc-iso>/<path>` via a new
      `MaterializeOp::Quarantine { from, to }`. **No notification, no
      prompt.** The file appears in the orphans folder; that's the
      entire surface.
    - **`!state.materialized.contains_key(p)`** (the device has no
      prior record of this file — user just created it fresh) →
      genuine new add, proceed. Publishes a normal primitive
      containing `p`. The fold's natural behavior on the next round
      will produce `main` with `p` present, because the user's
      primitive is concurrent with the delete in lineage terms (its
      parent is post-delete `main`, but the predicate confirms intent
      via `materialized` absence — Layer 3 must use the *same*
      check, see below).

Modify-vs-delete (case #2 in [[0012]]) falls out of the same logic:
if `state.materialized[p]` exists but `p ∉ tree(parent)`, the closure
check finds a delete, `materialized` has the path → quarantine the
modified bytes, honor the delete.

Crucially, Layer 1 runs **using only the DAG C currently has
integrated**. It catches the bug whenever C has actually received the
relevant delete primitive before authoring — which is the steady-state
case after any successful sync round.

#### Why `state.materialized` and not content match?

A content-match heuristic ("if disk bytes equal the deleted bytes,
it's stale") fails the [[0012]] case #2 scenario: user touches a stale
file while offline (adds a newline), bytes diverge slightly, content
match says "not stale," file resurrects. `state.materialized` captures
the stronger invariant: *did this device ever write this path to
disk?* If yes, anything still on disk is by definition pre-existing
data (modified or not). If no, anything on disk is new user intent.

### Layer 2 — Explicit bootstrap mode (full-handshake gated)

Don't try to infer "fresh first device" vs. "state-lost rejoin" from
runtime peer reach (the wedge that sinks [[0012]]'s Part A). Encode
the distinction at engine-creation time:

- **`ctx init`** → fresh vault, `state.materialized = ∅` is canonical,
  every on-disk file is a genuine novel add. Already works. Layer 1
  is a no-op because there's no ancestry to contradict.
- **`ctx join <peer>`** → marks the engine `bootstrap_pending = true`
  in its persisted state. `commit_scoped` is **blocked** (returns
  `Ok(None)`) until the §13 / [[0007]] session handshake reports
  completion — i.e. C has been told by the peer that its known-set
  covers the peer's frontier, not just that *some* primitive was
  admitted. **Coupling to [[0007]] is intentional** — gating on "≥1
  primitive admitted" has a race window where the relevant delete may
  not be in the first batch, leaving Layer 1 to walk a closure that
  doesn't contain the delete.
- **Recovery** (`state.json` missing but `.context/objects` survives):
  on load, boot with `bootstrap_pending = true` and reconstruct
  `state.materialized` from local `main`'s tree. Equivalent to a
  `join` from the user's perspective — must catch up before
  publishing.
- **Recovery with no `.context/objects`** (true state loss): user runs
  `ctx join` from another peer. Same flow.

The "gate" is now a one-shot boolean driven by user intent at the CLI,
not a runtime quorum. A node with all peers offline that the user
explicitly initialized with `init` publishes immediately; a node that
ran `join` waits until the handshake confirms catch-up.

Blocked commits return `Ok(None)` so the host doesn't lose work, just
retries. The handshake-completion edge clears `bootstrap_pending`;
Layer 1 then takes over.

### Layer 3 — Integrate-time symmetric filter (correctness backstop)

We trust connected nodes; the protocol is not designed to defend
against byzantine peers. Layer 3 exists for the *trusted-but-stale*
or *trusted-but-buggy* author whose Layer 1 didn't fire:

- A C that published an add before ever integrating A's delete (truly
  offline-first authoring against an empty local DAG, then later
  reconnecting — Layer 2 should prevent this for `join` engines, but
  a buggy SDK could skip the gate).
- A C running an SDK older than the protocol bump that doesn't
  implement Layer 1 (the handshake should reject the peer; Layer 3
  is belt-and-suspenders if it doesn't).

For these, apply the same ghost-add predicate at fold time, on every
node, by filtering `known` before the frontier. **The predicate at
fold time must NOT depend on the receiver's `state.materialized`** —
that would make the fold non-deterministic across nodes. Instead,
Layer 3 uses only the content-addressable closure check:

```rust
fn effective_known<S: Store>(store: &S, known: &[Oid]) -> CspResult<Vec<Oid>> {
    known.iter().copied()
        .filter(|&o| !is_ghost_add_closure_only(store, o)?)
        .collect()
}
```

`is_ghost_add_closure_only(store, p)` is pure over `(p, store)` —
verdict depends only on `p`'s parent closure, which is fixed by `p`'s
oid. Every node agrees, fold determinism preserved. Wire
`effective_known` into `frontier()` in `fold.rs:85` so closure-detected
ghost-add primitives are dropped from the fold *as if they were never
authored*. Their objects remain in the store (verifiable history) but
they don't contribute to `main`.

#### Tension: Layer 1 vs. Layer 3 disagreement

Layer 1 uses `materialized` as the intent signal — a runtime, per-
device check. Layer 3 cannot use `materialized` (non-deterministic
across nodes). So they answer different questions:

- **Layer 1:** "Is this *my* commit a ghost-add given what I know
  about *my own* prior state?"
- **Layer 3:** "Is *any* commit a ghost-add given only public DAG
  content?"

This means a primitive that Layer 1 lets through (because
`materialized[p]` was absent → legitimate new add) will be
**accepted by Layer 3 too**, because the Layer 3 check is the
closure-only predicate and the publisher already passed it (Layer 1
fires *only* when closure says delete). Wait — re-examine.

Re-stated precisely:

- Layer 1 fires when: closure of `parent` has most-recent-touch =
  delete AND `materialized` has `p`. Outcome: quarantine, do not
  publish.
- Layer 1 passes when: (closure has no touch OR most-recent is add)
  OR (most-recent is delete AND `materialized` lacks `p`). Outcome:
  publish.

Layer 3 fires when: closure of `parent` has most-recent-touch =
delete. Outcome: drop from frontier.

So if Layer 1 passes a primitive in the second case (closure-delete +
`materialized`-absent — the "legitimate user re-add"), **Layer 3 will
drop it.** This is the legitimate re-add case getting filtered.

**Resolution:** Layer 3 must exempt a primitive iff it carries an
explicit intent marker proving the publisher genuinely meant the
re-add. Since we ruled out `ctx readd`, the marker is emitted
automatically by Layer 1's "pass via materialized-absent" branch.

Concretely: when Layer 1 publishes a re-add of a path that has a
delete in closure, it adds a `CSP-Readd: <delete-prim-oid>` trailer
to the commit message (named after the §5.1-most-recent delete in
closure). Layer 3's `is_ghost_add_closure_only` exempts primitives
whose trailer names a delete in the *primitive's own closure*.

The trailer is **emitted by the engine, not the user** — no CLI
verb, no UI affordance — and is **load-bearing for correctness**,
not advisory. Under the trust model the trailer is a structural
signal, not an authenticated one — no signing, no forgery concern.
**This requires a protocol-version bump; the §13 handshake refuses
to peer with SDKs below the version that emits and respects the
trailer, so mixed-version swarms cannot form.**

## Operational details

### `.context/orphans/` is local-only

The orphans folder lives at `<vault>/.context/orphans/<utc-iso>/...`
on each device. It is **never published** — the engine already
hard-excludes its own `.context/` state directory from the tree
scan, and `ctx init` seeds `.contextignore` (commit 70a95cc) to keep
the materializer from re-ingesting it on a future scan. Peers do
not share orphans; each device curates its own quarantine.

If a user wants to recover a quarantined file, they drag it from
`.context/orphans/<...>/<path>` back to `<path>`. On the next
commit, `state.materialized` lacks the destination path → Layer 1
classifies the move as a fresh add (and emits the readd trailer) →
the file republishes to the swarm.

### Quarantine failure mode

If `MaterializeOp::Quarantine { from, to }` fails (disk full,
permission denied, Windows path-length limit), `commit_scoped`
returns `Ok(None)` — same as the Layer 2 blocked-commit path. The
host retries on the next commit tick. The engine never **deletes**
the on-disk file as a fallback (that would be silent data loss) and
never **publishes** the ghost-add as a fallback (that would
resurrect the deleted path). Fail-closed is the only safe choice.

### Recovery-mode quarantine surface

`bootstrap_pending`-mode startup that reconstructs
`state.materialized` from local `main` may, on the first commit
after handshake completion, quarantine files for paths the swarm
deleted since this device last synced. Log a single INFO line
summarising the count (e.g., `quarantined N stale files to
.context/orphans/<iso>/`); do not raise a UI prompt. This is the
documented residual-loss surface from "User-visible behavior."

### Out of scope: ghost-delete

This issue handles ghost-*adds* only. A symmetric "ghost-delete"
guard (a primitive deleting a path that the broader DAG just added)
is not designed here; under the trust model no peer maliciously
deletes, and concurrent delete-vs-add is a standard merge race that
§5.1 ordering resolves.

## Why this is better than [[0012]]'s tombstones-+-deferral

| Concern from [[0012]] | This design |
|---|---|
| Tombstone vs. legitimate re-create ambiguity | Engine emits intent trailer automatically; user never thinks about it |
| "Forces a Remove regardless of theirs" clobbers pending edits | Quarantine preserves bytes in `.context/orphans/` |
| Tombstone GC story unsolvable | Nothing accumulates in the tree |
| Wire-format change breaks SDKs | No tree-format change; trailer is an additive commit-message field with a documented version bump |
| "Tombstones must not alter byte-identity of main" self-contradictory | Fold / merge byte-identical to today for paths without delete-then-readd history |
| "All peers reachable" quorum question for deferral gate | One-shot `bootstrap_pending` flag set by user intent at `init` / `join`, cleared on handshake completion |
| Fresh-first-device vs. state-loss indistinguishable | Distinguished by which CLI command created the engine |
| User edits during deferral window undefined | Deferral is one-shot and clears on handshake completion — bounded window, blocked commits return `Ok(None)` so the host doesn't lose work, just retries |

## User-visible behavior

**Steady-state Obsidian use:** Invisible. Deletes propagate, new files
publish, no banners, no prompts, no commands.

**User creates a new note at a path that was deleted long ago:**
Publishes normally. `state.materialized` lacks the path → Layer 1
classifies as fresh user creation → engine emits the readd trailer
automatically → propagates to all (sufficiently-new) peers.

**Stale device reconnects with deleted files on disk:** Files move to
`.context/orphans/<utc-iso>/<path>` silently. The orphans folder
appears in the vault tree (Obsidian sees it as a regular folder). If
the user ever wonders where a file went, they can find it there and
drag it back to the original path — at which point `materialized`
lacks the original path → publishes as a fresh add.

**User edits a stale file while offline and the swarm deleted it
meanwhile (residual loss case):** Their edits go to
`.context/orphans/`. Data preserved, not destroyed. User can recover
manually if they notice. This is the only scenario where the design
isn't perfect, but no fully-invisible system can do better without
either prompting or surfacing a conflict UI.

## Acceptance

- Same regression test as [[0012]]: A deletes `foo`, A↔B sync, then C
  (with `foo` on disk in all three state-loss / disk-touch /
  fresh-restore variants) joins — `foo` ends up deleted on every
  device, on-disk content is preserved in `.context/orphans/` on C.
- Clean reconnect (intact `state.materialized`, no local edits) is
  still a no-op publish.
- `ctx init` on a brand-new device with files publishes them
  immediately (Layer 2 only blocks `join`-mode engines).
- User legitimately re-creates a previously-deleted path on a synced
  device: file publishes, all peers (on the new SDK version) see it.
- Backstop test: a primitive missing the readd trailer for a path
  whose closure contains a delete is dropped by Layer 3 on every
  peer (covers the buggy-author / pre-bump-SDK case; we do not model
  byzantine peers).
- §13.2 deterministic-fold conformance suite (`fold.rs` tests) still
  green by construction — `is_ghost_add_closure_only` is content-only
  and the underlying fold is untouched.
- SDK interop (`test-vectors.json`) updated with: the
  delete-then-late-join scenario, the legitimate readd scenario (with
  trailer), and a ghost-add primitive (no trailer) that integrate-
  time filtering must drop. Version-mismatch is not modeled — the
  handshake refuses peering below the supported protocol version.
- New host-side surface: `MaterializeOp::Quarantine { from, to }` is
  applied by `ctx`, the Obsidian plugin, and the desktop app —
  silently, no banner. The orphans folder is just a folder in the
  vault.
- Layer 1 cost: per-(parent, path) cache for "most recent primitive
  touching path in closure" (LRU keyed on `(parent_oid, path)`,
  bounded at ~1024 entries — tune from workload). Keys are immutable
  by construction (`parent_oid` is content-addressed), so stale keys
  simply age out as `main` advances. Designed up-front because Layer
  1 runs on every commit.
- Layer 3 cost: per-primitive ghost-add verdict cache (content-
  addressed on the primitive's oid — sound across processes).

## Open questions / coupled work

- **Couples to [[0007]] for Layer 2.** Handshake completion is the
  unblock edge. Land [[0007]] before or alongside this issue, or
  define a stub handshake-completion signal that [[0007]] later
  upgrades.
- **Protocol version bump.** Ship the integrate-time filter and
  trailer-emission together in one SDK release that increments the
  protocol version; the §13 handshake refuses to peer with older
  versions. No mixed-version swarms, no fallback path.

## Relation to other issues

- Supersedes the design in [[0012]] if accepted. The bug is the same;
  the cases the regression test must cover are the same. This issue
  proposes a different mechanism.
- Depends on [[0007]] for Layer 2's handshake-completion edge.
- The integrate-time filter in Layer 3 walks ancestor closures, which
  brushes against the same hot path [[0009]] / [[0011]] are
  optimizing. The per-primitive ghost-add cache (content-addressed)
  keeps the per-integrate cost bounded.
