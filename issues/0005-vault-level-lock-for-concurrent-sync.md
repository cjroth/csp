# 0005 — Vault-level lock to prevent concurrent sync by multiple processes

- **Severity:** Medium (latent state/working-tree corruption risk; not yet observed in the wild)
- **Status:** Open — design exploration
- **Component:** `crates/csp-core` (`vault`, `state`)
- **Raised:** 2026-05-19, while reviewing what stops two engines from driving the same `.context`

## Summary

A vault is a single folder with an engine-owned `.context/` inside it
(`crates/csp-core/src/scope.rs:19`, `CONTEXT_DIR = ".context"`). Nothing
prevents two *processes* from opening and syncing the **same** folder at the
same time, e.g.:

- the desktop app (a long-running node hosting the folder, per
  `desktop-app-spec.md`) **and** a separate `ctx watch` on the same path;
- `ctx watch` (the long-running sync daemon) **and** a one-shot
  `ctx snapshot` / `ctx restore` invoked from another shell;
- two `ctx watch` instances started by mistake (two terminals, a stale
  daemon, a supervisor double-spawn).

Each process gets its own `Vault` and its own `commit_local_changes` →
`export_closure` → bus path. They independently scan the working tree, author
primitives, advance counters, and rewrite `.context/state` / git refs. The
only concurrency guards today are **intra-process** and **state-file-scoped**;
there is no folder-level "someone else already owns this vault" guard.

This issue is to explore adding a **vault-level lockfile in `.context/`**, and
— per the explicit concern raised — to make sure that lock cannot get
*stuck* if the holding process crashes.

## What already exists (and why it isn't enough)

1. **In-process serialization only.** A `Node` wraps the vault in
   `Arc<Mutex<Vault>>` and every sync takes it:

   ```rust
   // crates/csp-core/src/net.rs:38-41,55-56
   pub struct Node { pub vault: Arc<Mutex<Vault>>, bus: broadcast::Sender<Vec<Vec<u8>>> }
   pub async fn commit_and_publish(&self) -> CspResult<Option<String>> {
       let mut v = self.vault.lock().await;
       ...
   ```

   This serializes syncs *within one process* (so the desktop "one node, many
   vaults" model is already safe against itself). It does nothing across
   processes.

2. **A state-file-scoped cross-process lock already exists.**
   `EngineState::save` takes an `fs2` advisory exclusive lock on
   `.context/state.lock` around the load→merge→write of `.context/state`:

   ```rust
   // crates/csp-core/src/state.rs:117-128
   // Cross-process exclusive lock: a one-shot (`ctx snapshot`/
   // `restore`) and a running `ctx watch` daemon are two writers of
   // `.context/state`. Serialize the whole load→merge→write so
   // neither clobbers the other (`.context/` is never synced, §11).
   let lock_path = context_dir.join("state.lock");
   let lock = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(&lock_path)?;
   fs2::FileExt::lock_exclusive(&lock)?;
   let _guard = LockGuard(&lock);
   ```

   Note the comment already acknowledges the multi-writer scenario. But this
   lock is held only for the duration of a single `state` save, and it merges
   monotonic fields rather than excluding the other writer. It does **not**
   cover the rest of a sync: working-tree scan, primitive authoring, git
   object/ref writes, or `Vault::open`'s own `state.save` at
   `crates/csp-core/src/vault.rs:155`. Two daemons can still interleave
   commits and ref updates between each other's `state.lock` windows.

3. **No guard at vault open/create.** `Vault::create`
   (`crates/csp-core/src/vault.rs:93-139`) and `Vault::open`
   (`:141-170`) just `create_dir_all` / open `.context` and proceed. There is
   no "is another engine already driving this folder?" check anywhere.

## The stuck-lock concern (central design point)

The requester correctly flagged the classic failure mode: **a lockfile that
outlives the crashed process and wedges every future run.** The mitigation is
in the *kind* of lock, not in cleanup heuristics:

- **OS advisory locks (good).** `fs2::FileExt::lock_exclusive` (Unix
  `flock(2)` / `fcntl`, Windows `LockFileEx`) are owned by the OS file
  handle. When the process exits *for any reason* — clean exit, panic,
  `SIGKILL`, OOM, power loss — the kernel closes the descriptor and releases
  the lock automatically. **It cannot get stuck on crash.** This is exactly
  why the existing `state.lock` is robust, and the same primitive should be
  reused for the vault-level lock.

- **Presence-based lockfile (bad).** "Create `.context/vault.lock`; if it
  exists, bail" is the design that gets stuck: a crash leaves the file behind
  and needs PID-liveness probing, staleness timeouts, or manual `rm` — all
  racy and platform-fragile. We should explicitly **not** do this.

So the recommended direction is an advisory-locked file (hold an open,
flocked handle for the `Vault`'s lifetime), which is inherently
crash-safe. The remaining caveats to call out in the design:

- **Networked filesystems.** `flock` semantics are unreliable on NFS / SMB /
  some FUSE mounts. Vaults synced inside a Dropbox/iCloud/NFS folder are
  plausible. The lock should *fail open with a warning* (or be advisory about
  its own limitation) rather than give a false guarantee.
- **Advisory, not mandatory.** It only protects against *cooperating* CSP
  processes — fine here, since the only writers are our own binaries.
- **`fork`/exec.** flock locks are not preserved across `fork` the way
  `fcntl` ranges are; not an issue for our spawn model but worth noting if the
  desktop app ever forks workers.

## Proposed direction (to validate, not yet decided)

- Add a dedicated `.context/vault.lock` (separate from `state.lock` so the
  two locking scopes stay independent and we don't change `state.rs`
  semantics).
- Acquire it in `Vault::open` / `Vault::create`
  (`crates/csp-core/src/vault.rs:93-170`), *before* the `state.save` at
  `:155`, and store the locked `File` handle in the `Vault` struct so it is
  released on drop (mirror the `LockGuard` pattern at
  `crates/csp-core/src/state.rs:93-99`).
- Decide the lock discipline:
  - **Exclusive single-instance** — simplest; one engine per folder. The
    Obsidian plugin never touches `.context/` (`obsidian-plugin-spec.md`) so
    it is unaffected; it talks to a node over the wire, not the filesystem.
  - **Shared/exclusive** — only if a real read-only consumer of `.context/`
    is anticipated. Probably YAGNI; default to exclusive.
- Decide contention behavior: **fail fast** with a clear, user-facing message
  ("folder already being synced by another csp process — pid/uid if
  obtainable") vs. **block and wait** (`lock_exclusive`) vs.
  **`try_lock_exclusive` + retry/backoff**. Fail-fast with a good message is
  likely best for `ctx watch`; a one-shot `ctx snapshot`/`restore` might
  prefer a short bounded wait. This interacts with the existing per-save
  `state.lock` and should be consistent with it.
- **WASM build:** `csp-core` targets wasm *and* native; `csp-wasm` has no
  filesystem. The lock must be `cfg`/feature-gated to native so the wasm
  build (Obsidian/browser SDK) still compiles. `fs2` is already a native-only
  dependency via `state.rs`, so this is mostly a matter of where the gating
  lives.

## Impact if left unaddressed

- Two daemons on one folder can author divergent primitives and stomp each
  other's git refs / `.context/state` between `state.lock` windows →
  inconsistent local history, redundant/duplicate primitives published to
  peers, and potential working-tree clobber under the §5.6 rules.
- Failure is silent and intermittent (timing-dependent), so it would surface
  as "mysterious sync corruption" rather than an obvious error — exactly the
  class of bug a cheap upfront lock prevents.

## Open questions

- Is single-instance-per-folder an acceptable hard constraint for all
  consumers (desktop app, `ctx`, future tools), or is there a legitimate
  concurrent-reader use case?
- Should `Vault::open` taking the lock break any existing test/tooling that
  opens the same vault twice in one process? (In-process, two `Vault::open`
  calls on the same path would now contend — confirm the test suite and the
  desktop multi-vault host don't rely on that.)
- Networked-FS detection/fallback policy: warn-and-continue, or hard-require a
  local FS for a writable node?
- Should the lock file carry a small payload (pid, start time, host) purely
  for *diagnostics* in the "already locked" error message — without using it
  for liveness logic (which would reintroduce the stuck-lock problem)?

## References

- `crates/csp-core/src/scope.rs:19` — `CONTEXT_DIR` (the `.context` dir)
- `crates/csp-core/src/vault.rs:93-139` — `Vault::create`
- `crates/csp-core/src/vault.rs:141-170` — `Vault::open` (proposed acquire site; `state.save` at `:155`)
- `crates/csp-core/src/state.rs:93-99,115-150` — existing `state.lock` (`fs2` advisory) + `LockGuard` drop pattern to mirror
- `crates/csp-core/src/net.rs:38-64` — `Node` / `commit_and_publish` (the in-process-only guard today)
- `desktop-app-spec.md` — "one node, many vaults" (in-process, already serialized; cross-process is the gap)
- `obsidian-plugin-spec.md` — plugin is outbound-only and never touches `.context/` (unaffected by the lock)
