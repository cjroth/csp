# 0013 — Couple a vault to a sibling git repo (joint clone, fast-forward, rewind)

**Severity:** Feature  **Status:** Open  **Owner:** —

## Summary

Today a CSP vault and its enclosing project's git repo are deliberately
*decoupled* (§11 — no `.git` at scope root, CSP's history lives at
`.context/git/`, the project's git lives at the parent). They coexist
without colliding, but they don't *know about each other*. There is no
recorded relationship between a CSP commit / snapshot and the git
commit it was authored against.

This issue proposes a thin, opt-in coupling so that the two histories
can travel together:

1. **Config knows the project's git origin.** Add `git_url` (and a
   relative `git_subpath` from the git repo root to the CSP scope root)
   to the vault's identity, propagated in `Hello` so every cloner sees
   the same value.
2. **`ctx clone` can also clone the code.** With `--with-git` (or auto,
   when `git_url` is set and the user is cloning into an empty dir),
   `ctx clone <csp-url>` clones the git repo first, then creates the
   CSP scope at `<repo>/<git_subpath>` and syncs into it. One command,
   two histories aligned by construction.
3. **Snapshots record the git anchor.** A `ctx snapshot <name>` records
   the current `git HEAD` SHA (and whether the working tree was clean)
   in the snapshot tag's payload. `ctx restore <name>` *optionally*
   checks out that SHA after restoring CSP content, so context and code
   move together.

## Why

The whole point of CSP is that agents and humans share a working
folder of context (prompts, plans, task state, notes) *about* a
codebase. That context is only meaningful **relative to a specific
state of the code**:

- A plan written against `main@abc123` mentions functions, file
  layouts, and decisions that may not exist on `main@xyz789`.
- An agent that picks up where another left off needs the same working
  tree the prior session reasoned about, or its tool calls miss.
- `ctx restore` (time-travel) is currently *half* a time machine — it
  rewinds the context but leaves the code at HEAD. Anyone who actually
  uses restore to recover lost work has to manually `git checkout` to
  the right commit to make the restored context coherent again, and
  there is nothing on disk telling them which commit that is.

A loose, opt-in coupling makes the two histories *describable as a
pair* without forcing either side to swallow the other:

- The protocol still does not plant `.git` at the scope root (§11).
- Git's history is still entirely git's.
- CSP just records a pointer from each anchor in its history (named
  snapshots, optionally every commit) to the git commit it was
  authored against. Cheap to record, valuable for recovery and agent
  hand-off, ignorable when not needed.

## Design

### 1. Config: vault identity grows two optional fields

`VaultConfig` (`crates/csp-core/src/config.rs`) gains:

```rust
/// Project git repo this vault is paired with (optional). When set, a
/// fresh `ctx clone --with-git` will clone this URL into the parent
/// directory and place the CSP scope at `<repo-root>/<git_subpath>`.
#[serde(default)]
pub git_url: Option<String>,

/// Path from the paired git repo root down to the CSP scope root
/// (POSIX, no leading `/`). Empty / None ⇒ the scope IS the repo root
/// (advanced mode; the project's `.gitignore` must exclude `.context/`).
#[serde(default)]
pub git_subpath: Option<String>,
```

These are **vault-identity-level** facts (every replica should agree),
so they propagate in `Hello` the same way `vault_id` and `name` already
do. The on-disk `.context/config` is per-node (§11 HARD INVARIANT), but
the values are seeded at clone time from the peer's `Hello`, so a
cloner gets the same `git_url` / `git_subpath` automatically.

Editing them locally is allowed (it is config, not a synced object);
peers diverging on this is harmless — it only affects future clones
and the snapshot-anchor coupling.

### 2. `ctx clone` learns `--with-git` (and an auto path)

```
ctx clone wss://host:9000 --with-git
```

Order of operations, given a configured `git_url` + `git_subpath = "context"`:

1. Open the CSP handshake far enough to receive `Hello` (so we have
   `git_url`, `git_subpath`, `name`, `vault_id`). Close it.
2. Pick a target dir (default: `./<git-repo-name>/`, derived from
   `git_url`). Refuse to clobber.
3. `git clone <git_url> <target>` — plain subprocess, inherits the
   user's git config / credentials. Failures bubble up with the
   underlying git stderr; CSP does not try to be a git client.
4. Verify the project's `.gitignore` excludes `<git_subpath>/.context/`
   (and offer to append it if it doesn't — the `.context/` HARD
   INVARIANT must hold; we can't let git track CSP state).
5. `ctx clone <url> <target>/<git_subpath>` — the existing flow, just
   with a precomputed target path. Records the origin in
   `peers` exactly as today.

Auto mode: if the user runs `ctx clone <url>` with no target and the
peer's `Hello` carries `git_url`, behave as if `--with-git` was passed
unless `--no-git` is given. (Symmetrical to how `--watch` is opt-in but
the bootstrap flow is one command.)

Bare `ctx clone <url> .` keeps today's semantics (CSP-only, no git).

### 3. Snapshots gain a git anchor

A `ctx snapshot <name>` already records a deterministic CSP commit at
`refs/tags/snap/<name>` (§8). Extend the snapshot tag's payload with:

```
git-head: <40-char SHA, or empty if no git repo found>
git-dirty: <true|false>           # working tree had uncommitted changes
git-branch: <ref name, or empty>  # informational
```

These are recorded by the snapshotting node only, on its own filesystem
— there is no requirement that peers agree on the git state. The tag
itself is still the deterministic CSP commit (it has to be, for §8
convergence); the git anchor is **side-channel metadata in the tag
message**, not in the commit tree.

`ctx restore <name>` grows behavior:

- Restores CSP content as today.
- If the snapshot tag carries a `git-head` and we are inside a git
  worktree at the expected `git_subpath`:
  - **Default (interactive):** print the recorded SHA and ask before
    checking out. Refuse if the worktree is dirty.
  - `--with-git` / `--no-git` to force either way non-interactively.
  - `--force-git` to allow checkout over a dirty worktree (with a
    standard git-style "your local changes would be overwritten" check
    delegated to git itself — we do not reimplement it).

For `ctx restore <unix-time>` (the existing time-based restore), pick
the most recent snapshot at or before that time and use its anchor; if
no snapshot covers the range, print the git SHA of the most recent
commit at or before that time as a *suggestion* but do not auto-check
out.

### 4. (Optional, deferred) anchor every commit, not just snapshots

A natural extension is to write the current `git HEAD` SHA as a trailer
on every CSP primitive commit (`Git-Anchor: <sha>` in the commit
message). This lets `ctx log` show the code state for every keystroke-
debounced commit, and lets restore-by-time pick a precise anchor
without needing a named snapshot.

This is deferred because (a) it touches the primitive commit hot path
that the convergence proofs depend on, and (b) the trailer would have
to be canonicalized for byte-determinism (§5.4) — a node without a git
repo, or with a dirty HEAD, must produce the *same* commit bytes as
one with a clean HEAD or convergence breaks. The simplest
canonicalization is "never include the trailer in the commit's
deterministic body — record it in a side note ref like
`refs/notes/git-anchors`". Treat this as a follow-up once the snapshot
anchor has proven its weight.

## Open questions

1. **Subpath edits.** Can `git_subpath` change over the life of a
   vault (e.g. project layout reshuffle moves `context/` to
   `docs/agent-context/`)? Probably yes, but it breaks future
   `--with-git` clones into older snapshots. Acceptable; document it.
2. **No-git mode still valid.** Vaults without `git_url` are the
   majority and must keep working unchanged. All new behavior is gated
   on the field being set.
3. **Restore semantics when the git repo has diverged.** If the
   recorded `git-head` is unreachable from any local branch (the user
   force-pushed away), surface a clear error and let the user fetch /
   pick a strategy. Do not silently fail.
4. **Submodules / monorepos.** Out of scope for this issue. If the
   git side is a submodule arrangement, the user uses bare `ctx clone`
   and runs git themselves. Revisit only if a real user hits it.
5. **Auth.** `git clone` may need credentials (SSH key, PAT). We do
   not handle this — we shell out to `git`, which already does. If git
   fails we print its stderr and stop.

## Risks

- **Scope creep into being a git client.** We must stay a *thin
  invoker* of `git` subprocesses. The CSP engine learning to fetch /
  push / merge is a different project. Mitigation: every git
  interaction in this issue is a `git clone` or `git checkout`
  subprocess call with explicit stderr surfacing.
- **Working-tree clobber.** `ctx restore --with-git` over a dirty
  worktree could destroy uncommitted work. Mitigation: refuse by
  default; require `--force-git`; defer the actual safety check to
  `git checkout` itself (it has the canonical implementation).
- **Convergence drift if anchors enter primitive commits.** See §4 of
  Design — keep anchors out of the deterministic commit body until
  the canonicalization story is in place. Snapshot tags carry the
  anchor in their message, which is *not* part of the §5.4
  byte-determinism invariant.
- **`Hello` payload growth.** Two optional strings; trivial.
- **Subprocess-`git` portability.** The README already requires `git`
  on `PATH` for the read-only `ctx git` inspector, so this adds no new
  prerequisite for full nodes. Thin / wasm nodes don't run `git` and
  simply can't perform `--with-git` clones — they fall back to
  CSP-only.

## Acceptance

- `VaultConfig` round-trips `git_url` / `git_subpath` through the
  hand-rolled TOML codec (corpus extended in
  `crates/csp-core/src/config.rs` tests).
- `Hello` carries both fields when set; a fresh `ctx clone` against
  such a vault writes them into the cloner's `.context/config`.
- `ctx clone <url> --with-git` against a vault whose origin advertises
  `git_url = "git@github.com:org/proj"` and `git_subpath = "context"`:
  - Clones the git repo into `./proj/`.
  - Creates the CSP scope at `./proj/context/`.
  - Verifies `./proj/.gitignore` excludes `context/.context/` (offers
    to append if missing, prompts unless `--yes`).
  - Records `wss://…` in the new vault's `peers`.
- `ctx snapshot release-candidate` on a node with a sibling git repo
  records `git-head`, `git-dirty`, `git-branch` in the snapshot tag
  message. Verified by reading the tag with `ctx git` and with stock
  `git --git-dir=.context/git show snap/release-candidate`.
- `ctx restore release-candidate --with-git` on a clean worktree
  checks out the recorded SHA. On a dirty worktree, refuses unless
  `--force-git`.
- Vaults with no `git_url` configured exhibit identical behavior to
  today: no new prompts, no new files, no new flags surfaced.
- A vault whose recorded `git-head` is unreachable from any local ref
  fails `restore --with-git` with an actionable error (e.g.
  "git commit `abc123` not found locally; try `git fetch origin`").

## Relation to other issues

- [[0009]], [[0010]], [[0011]] — orthogonal. Those are about the
  *cost* of the sync path; this is about *what the sync path's output
  describes*. None of them block this issue.
- §11 (scope & coexistence) is the load-bearing prior decision: this
  issue lives inside the dedicated-subtree model it already
  recommends, and does not weaken the "no `.git` at scope root" HARD
  INVARIANT.
- §8 (snapshots / restore) is the natural seam — this issue extends it
  with a git anchor without changing the deterministic commit it
  already produces.
