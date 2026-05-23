# 0012 — Deleted files resurrect when a stale device reconnects

**Severity:** High  **Status:** Open  **Owner:** —

## Summary

A file deleted on device A propagates correctly to every device that
was online at the time. But if a fourth device C — which still has the
file on disk — connects *after* the deletion has settled across the
others, the file is resurrected on every device, including the ones
that had successfully applied the delete.

The clean reconnect case (C offline with intact `state.materialized`,
no local edits, no state loss) actually works: `commit_scoped` is a
no-op, the frontier collapses to the delete primitive, and
`materialize_plan` removes the file from C's disk
(`engine.rs:303`, `:423-433`). The bug fires whenever C publishes a
"false add" primitive for the deleted path.

## Why

CSP uses content-defined state with a deterministic 3-way fold, not an
op-log (`merge.rs:129-167`, `fold.rs:233-266`). A deletion is just "the
path is absent from the tree." Whether the merge interprets an absent
path as *deleted* or *not-yet-added* depends entirely on the merge
base.

Three concrete situations cause C to publish a false-add primitive:

1. **State loss / fresh install with files on disk.** `.context/state`
   is missing or wiped, but the file is still on disk. `commit_scoped`
   sees `materialized[p] = None ≠ Some(hash)` → publishes a primitive
   with `tree={p: bytes}`, parented on `main = genesis(M₀)`
   (`engine.rs:301-326`).
2. **Any disk-side touch to the file while offline.** Even a
   trailing-newline diff causes the same publish, parented on C's
   stale pre-deletion `main`.
3. **Restored-from-backup / new device.** Files appear on disk before
   any sync handshake. Same shape as #1.

Once that primitive `pC1` exists, `compute_main` over `{pA1, pC1}`
calls `merge_base` (`fold.rs:129-161`). In case #1 the base is `M₀`
(empty tree): `merge_trees` sees `b=None, o=None, t=Some(bytes)` →
`o == b` → take theirs → **file resurrected** (`merge.rs:147-150`). In
case #2 the base has the file unmodified, but `pC1`'s bytes differ
slightly → "delete-vs-modify, theirs wins" (`merge.rs:155-159`) →
**file resurrected as the modified bytes**. Same outcome, different
code path.

This is structural: 3-way merge on flat `path -> bytes` cannot
distinguish "I'm asserting this path exists" from "I never heard it
was deleted."

## Design

Combine two complementary mitigations.

### A. Bootstrap protocol — don't publish until you've synced

Handles cases #1 and #3 without touching the fold/merge invariants.

- On engine start, if `state.materialized` is empty *and* disk has
  files *and* there are known peers, **defer** the first
  `commit_scoped` until after a handshake + catch-up.
- After catch-up, diff disk against the just-synced `main` and publish
  primitives only for genuine divergence, parented on the synced
  `main`.
- Also gate the engine against publishing a primitive whose parent
  would be `genesis(M₀)` if any non-empty peer `main` is reachable —
  that's the smoking-gun signature of a bootstrap-race add.

### B. Persistent tombstones in the tree

The robust backstop for everything A can't catch (concurrent local
edit during a remote delete, manual file restore, second-order races).

- Encode deletes as a sentinel entry alongside the absence — e.g.
  `.csp/tombstones/<path-hash>` blob carrying the deleted path +
  metadata.
- `materialize_plan` ignores tombstone paths when writing to disk; if
  a tombstone is in `want`, it **forces** a `Remove` of the
  corresponding path regardless of what theirs-wins produced for the
  file path itself.
- This works even when `merge_base` is empty: each path merges
  independently, so the tombstone's "ours" survives against C's empty
  theirs, while the file's "theirs" survives against `pA1`'s empty
  ours; the materializer resolves the contradiction by honoring the
  tombstone.
- Compaction: GC a tombstone once every known peer's frontier
  dominates the delete commit (or by retention horizon §9.2). Out of
  scope for the first cut — keep them forever and revisit.

### Why not just C — tighten `commit_scoped`?

A narrower fix ("don't classify byte-identical disk content as a
change") helps case #2 but does nothing for cases #1 and #3 where
`state.materialized` is genuinely empty. Insufficient on its own;
subsumed by A.

## Acceptance

- New regression test: A deletes `foo`, A↔B sync, then C (with `foo`
  on disk and the three state-loss / disk-touch / fresh-restore
  variants) joins — `foo` ends up deleted on every device, not
  resurrected.
- The clean reconnect case (intact `state.materialized`, no local
  edits) is still a no-op publish — no false primitives.
- Bootstrap-deferral does not stall a legitimately-first-ever device
  (no known peers, no prior `main`) — that device still publishes
  immediately.
- Tombstone GC story documented even if not yet implemented.
- Deterministic-fold conformance suite (`fold.rs` tests) still green;
  tombstones must not alter the byte-identity of `main` for any
  existing test vector.
- SDK interop (`test-vectors.json`) updated to cover the
  delete-then-late-join scenario.

## Relation to other issues

- Architecturally distinct from [[0009]] and [[0011]]; this is a
  correctness bug, those are performance.
- The bootstrap-deferral in part A interacts with the session
  handshake — coordinate with [[0007]] if both land in the same
  window.
