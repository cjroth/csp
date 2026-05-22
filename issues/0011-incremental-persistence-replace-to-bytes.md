# 0011 — `engine.to_bytes()` re-serializes the whole vault on every commit

**Severity:** Medium  **Status:** Open  **Owner:** —

## Summary

`RealVault.persist()` calls `engine.to_bytes()` — a serialization of the
**entire** engine state (all objects + history + refs) — and writes it
as one blob via `storage.saveState()`, on every (debounced) commit.

So each edit re-serializes and re-writes the whole vault's sync state,
not just what changed. This is the third O(whole-vault) cost in the
Obsidian sync path, alongside [[0009]] (commit) and the UI-thread
blocking of [[0010]].

## Why

The `ObsidianStorageAdapter` already exposes a content-addressed object
store (`getObject` / `putObject` / `hasObject` / `listObjectOids`) plus
separate `state` / `frontier` / `snapshots` blobs — the same on-disk
layout the native `ctx` CLI uses (`.context/objects/<oid>`, §9.1). But
the SDK's `RealVault` ignores all of it and dumps everything through
`saveState` as a single opaque blob.

Consequences:

1. **Cost.** Persist is O(vault + history) per edit instead of
   O(new objects this edit).
2. **Flash wear.** Every keystroke-commit rewrites the full state blob
   — heavy write amplification on mobile storage.
3. **Not `ctx`-compatible on disk.** Spec §9.1 says the plugin's
   `.context/` should be interchangeable with `ctx`'s; the single-blob
   `state` is not. A `ctx` process cannot open a plugin-written vault
   (and vice versa) without a conversion.
4. **Worker boundary ([[0010]]).** With the storage adapter pinned to
   the main thread, a non-incremental `saveState` posts a multi-MB
   blob across the worker boundary on every commit.

## Design

Make the SDK use the object store for what it is — content-addressed,
immutable, write-once — and only rewrite the small mutable refs.

- Engine tracks objects dirtied since the last persist (or the SDK
  diffs `listObjectOids()` against the engine's known set).
- `persist()` becomes: `putObject(oid, bytes)` for each new object,
  then rewrite only `state` + `frontier` (small ref records).
- Objects are immutable, so write order is: **objects first, refs
  last** — a crash between leaves unreferenced objects (harmless,
  GC-able), never a dangling ref.
- Adopt the `.context/objects/<oid>` + `state` + `frontier` layout so
  the result is `ctx`-interchangeable.

### Risks

- This changes the engine's core persistence model — `MemEngine`
  currently holds everything in `MemStore`; incremental persistence
  needs dirty-object tracking (a real `csp-core` change, not just
  bindings).
- Persistence bugs are the most dangerous class — a missed object is a
  corrupt vault. The current tmp-write+rename of one blob is trivially
  crash-safe; the incremental path needs explicit ordered-write and
  recovery reasoning.
- Touches the mock vault + every parity test; another wasm rebuild.

### Cheaper interim mitigation

Pruning a thin node's local object set to the retention horizon
(§9.2) keeps `to_bytes()` ~O(current vault) instead of O(all history
ever) — a low-risk cap on growth without the full rewrite. Tracked as
a fold-in option on [[0009]].

## Acceptance

- A single-file edit writes only the objects that edit produced, plus
  the two small ref records — verified by counting adapter writes.
- The on-disk `.context/` layout is openable by the native `ctx` CLI
  (and a `ctx`-written vault is openable by the plugin) — a parity
  test exercises both directions.
- Crash-safety: interrupting persist mid-write never produces a
  dangling ref; recovery is a no-op or a harmless unreferenced-object
  cleanup.
- SDK interop + `ctx`-parity suites green.

## Relation to other issues

- [[0009]], [[0010]] — the other two O(vault) costs in the sync path.
  This issue is deliberately deferred behind them: 0009 + 0010
  eliminate the user-visible *freeze*; this issue is about efficiency,
  flash wear, and `ctx` on-disk parity. Recommended to land last, as
  its own focused release with a crash-safety review.
