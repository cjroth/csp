# 0009 — Obsidian sync is O(whole-vault) per edit; make commits incremental

**Severity:** High  **Status:** Open  **Owner:** —

## Summary

Every file edit in the Obsidian plugin triggers a commit whose cost is
proportional to the *entire vault*, not the edit. Each `commitNow()`:

1. Calls `filesToJson(this.files)` — serializes **every** file in the
   working set: `enc.encode(content)` → `Array.from(...)` → a JSON array
   of integers (`{"note.md":[35,32,104,...]}`). A 1 MB file becomes
   ~4 MB of JSON text.
2. Hands that whole blob to `engine.commit_from_files(json)`, which
   re-parses it and re-hashes every file inside wasm.

So a one-character edit pays for the whole vault, twice (host-side
encode + wasm-side parse). On a moderately-sized vault this is a
multi-second synchronous stall (see also [[0010]] for the UI-thread
half of the problem).

## Why

The engine API (`commit_from_files`, `materialize_plan`) is stateless:
the host re-supplies the full scoped working set on every call so the
engine can run the §5.6 content-reconcile. Correct, but it makes the
per-edit cost scale with vault size, which does not hold up past a toy
vault.

## Design

Make the engine hold the working set; the host sends only deltas, and
file content crosses the wasm boundary as raw bytes.

- **`csp-core`** — `MemEngine` gains a `working: BTreeMap<String,
  Vec<u8>>`. New methods: `stage_write(path, bytes)`,
  `stage_remove(path)`, `commit_staged()`. The §5.6 reconcile still
  runs, but re-hashing is limited to entries that actually changed
  since the last commit (track a per-path content hash alongside the
  working set).
- **`csp-wasm`** — expose `stage_write(&str, &[u8])`,
  `stage_remove(&str)`, `commit_staged() -> Option<String>`. `&[u8]`
  parameters are near-zero-copy through `wasm-bindgen` — this removes
  the JSON integer-array encoding entirely.
- **`materialize_plan`** — same treatment: the engine tracks the host's
  last-known on-disk state instead of receiving it as a full blob each
  call.
- **`real-vault.ts`** — `writeTextFile`/`deleteFile`/`renameFile` call
  `engine.stage_write`/`stage_remove` directly. `this.files` stays as
  the host-side read cache. `commitNow()` becomes `engine.commit_staged()`.
  `filesToJson` is deleted.
- **Mock vault** — mirror the new API so unit tests keep parity.
- Rebuild both wasm targets (`pkg`, `pkg-web`).

### History-trimming mitigation (optional, fold-in)

A thin node does not need unbounded history. Pruning the local object
set to the retention horizon (§9.2) keeps `engine.to_bytes()` roughly
O(current vault) instead of O(all history ever) — a cheap, low-risk
cap on the *other* O(vault) cost ([[0011]]) without the full
incremental-persistence rewrite. Can be included here or split out.

## Acceptance

- A single-file edit in a vault of N files does work proportional to
  the edited file, not N — verified with a timing test over a
  synthetic large vault.
- `filesToJson` (the integer-array encoder) is gone.
- SDK interop (`test-vectors.json` byte-identity) and `ctx`-parity
  (bidirectional convergence vs. real `ctx`) suites still pass — the
  authored primitives must stay byte-identical to the pre-change
  engine.
- Mock-vault unit tests cover `stage_write` / `stage_remove` /
  `commit_staged`.
- Obsidian plugin unit + e2e suites green.

## Relation to other issues

- [[0010]] — Web Worker. Independent; together they kill the sync
  freeze (this issue cuts the work, 0010 moves the remainder
  off-thread). Recommended order: this issue first.
- [[0011]] — incremental persistence (`to_bytes()`). The third
  O(vault) cost; deliberately out of scope here.
