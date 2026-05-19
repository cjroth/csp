# 0005 — SSH-agent signing is built and tested but not wired to the engine

- **Severity:** Medium (optional capability; not a regression — works for `ctx key`)
- **Status:** Open (deferred — infrastructure landed, engine seam pending)
- **Component:** `crates/csp-core` (`identity`, `vault`, `session`), `crates/ctx` (`idstore`, `sshagent`)
- **Found:** 2026-05-19, during the spec↔code reconciliation (the "node identity / SSH key" decision: reuse an existing key + delegate signing to a running agent)

## Summary

`ctx` can now reuse an existing OpenSSH ed25519 private key and talk to a
running `ssh-agent`: `crates/ctx/src/sshagent.rs` is a complete, tested
SSH-agent protocol client and `idstore::Signer` has a working `Agent`
backing (5 tests pass, incl. a real end-to-end agent-signing test). **Key
reuse from an unencrypted OpenSSH private key works fully.** What does *not*
work yet is *agent delegation in practice*: an agent-held key signs `ctx key`
(pubkey display) but **errors loudly for every vault operation**, because the
engine cannot route signing through the agent.

`idstore::Signer::identity()` deliberately returns `Err` for the `Agent`
backing rather than degrading silently. `Signer::sign` / the agent path are
marked `#[allow(dead_code)]` on purpose (see `idstore.rs` / `sshagent.rs`
doc comments) — they are kept so wiring the seam later is a local change,
not a rewrite. Do **not** "fix" the warnings by deleting them.

## Root cause

`csp-core` signs with a concrete in-process key, not an abstraction:

- `csp-core/src/identity.rs::build_primitive(id: &Identity, …) -> GitObject`
  signs the primitive commit in-process (`id.sign(payload)`), infallibly.
- `csp-core/src/vault.rs` stores `identity: Identity` and `Vault::open` /
  `Vault::create` take an owned `Identity` by value.
- `SessionVault::sign(&self, msg) -> Vec<u8>` (the handshake transcript
  signature) is already a trait method, but infallible (`Vec<u8>`, not
  `CspResult<…>`).

An SSH agent is **out-of-process and fallible** (a Unix-socket round trip),
so it cannot satisfy an owned-`Identity`, infallible-`sign` contract. The
`ctx` boundary therefore unwraps `Signer` → `Identity` and fails for the
agent backing.

## Impact

- Agent delegation (private key never read into the process) — an explicit
  spec capability — is unavailable. `ctx key` works with an agent key;
  `ctx watch` / `init` / `clone` / `status` / etc. error with a clear
  message pointing at this limitation.
- Reusing an *unencrypted on-disk* OpenSSH key works (it is loaded as an
  in-process `Identity`), so the common "reuse my `~/.ssh` key" case is
  covered; only the never-in-process agent path is blocked.
- No correctness/security regression: the failure is loud and explicit.

## Proposed fix

Introduce a tiny object-safe signing abstraction in `csp-core` and thread
it through; keep the agent implementation native-only in `ctx` so
`csp-core` stays lean / wasm-safe.

1. `csp-core`: `pub trait Sign { fn node_id(&self) -> NodeId; fn sign(&self,
   msg: &[u8]) -> CspResult<Vec<u8>>; fn to_ssh_string(&self) -> String; }`
   with `impl Sign for Identity` (sign wraps `Ok`). Object-safe,
   dependency-free.
2. `build_primitive(&dyn Sign, …) -> CspResult<GitObject>` — callers
   (`vault.rs`, `engine.rs`) are already in `CspResult` contexts; tests use
   `.unwrap()`.
3. `SessionVault::sign -> CspResult<Vec<u8>>` — exactly **one** real call
   site (`session.rs:159`, `let sig = v.sign(&script);`) and it is already
   inside a `CspResult<Step>` fn, so it becomes `v.sign(&script)?`.
4. `Vault` / `MemEngine` hold a `Box<dyn Sign>`. **Keep `Vault::open(path,
   Identity)` / `create(...)` working** (box the `Identity` internally) so
   every existing test, the e2e harness, the wasm node, and most of
   `main.rs` stay unchanged; add one `Vault::open_signed(path, Box<dyn
   Sign>)` constructor that `ctx` uses for the agent path. This keeps the
   blast radius to the csp-core internals plus two new constructors.
5. `ctx`: wire `idstore::Signer` (in-process *or* agent) into the new
   `open_signed` path in the `ctx watch` daemon.

### Open scope decision (resolve before implementing)

`ctx clone` / `net::probe` sign a bootstrap handshake with a concrete
`Identity` (and use `Vault::identity_clone()`). Two options:

- **watch-daemon only (recommended):** agent signing covers `ctx watch`
  (auto-commits + mutual-auth handshake — the long-running case where
  never-in-process matters most). `clone`/`probe` with an agent-*only* key
  stays a documented limitation (bootstrap with an in-process key, then run
  the daemon agent-backed). Smaller, contained change.
- **everything incl. clone/probe:** also route `probe`/`clone` through
  `Sign`; wider (probe signature + `identity_clone` callers), no caveat.

## Acceptance

- An agent-held ed25519 key drives `ctx watch` end to end: auto-commit
  primitives and the mutual-auth handshake both succeed, signatures verify
  against the advertised NodeId, and a peer converges — with the private
  key never read into the process.
- Existing `Identity`-based callers, wasm node, e2e harness, and the
  determinism conformance suite remain unchanged and green.
- The `#[allow(dead_code)]` markers on the agent path are removed (the code
  is now reached).

## References

- `crates/ctx/src/idstore.rs` — `Signer` enum, `Signer::identity()` (the
  documented seam), content-detected key loading.
- `crates/ctx/src/sshagent.rs` — hand-rolled SSH-agent client (5 tests).
- `crates/csp-core/src/identity.rs:101` — `build_primitive`.
- `crates/csp-core/src/session.rs:38-45,159` — `SessionVault::sign`.
- `crates/csp-core/src/vault.rs:36,55-67,250` — identity storage / use.
- Memory: `csp-spec-vs-code-reconciliation` (the deferred-seam note).
