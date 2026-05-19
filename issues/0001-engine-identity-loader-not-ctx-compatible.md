# 0001 — Desktop engine identity loader is not `ctx`-compatible

- **Severity:** High
- **Status:** Open
- **Component:** `desktop/engine` (`context-desktop-engine`)
- **Found:** 2026-05-19, debugging first-run crash on host macOS

## Summary

`desktop/engine/src/csp.rs::load_or_create_identity()` only understands the
**bare-hex 32-byte seed** form of `~/.context/id_ed25519`. Its own doc comment
claims it is *"byte-for-byte the `ctx idstore` scheme"* — this is false. The
canonical `ctx` loader accepts **three** on-disk forms; the desktop accepts one.

Any device whose `~/.context/id_ed25519` is **not** bare hex makes the desktop
app fail to initialise the engine. Combined with issue
[0002](0002-identity-and-config-errors-abort-silently.md) this surfaces as a
silent `abort()` / macOS crash report with no explanation.

## Reproduction

1. On macOS, have `~/.context/id_ed25519` be a reused OpenSSH ed25519 private
   key (a documented `ctx` mode — e.g. `ctx --identity ~/.ssh/id_ed25519`, or
   copying an OpenSSH key there). An older `ctx`/`csp` build also wrote a
   `csp-identity-v1 <base64-seed>` form (seen in the wild on the host).
2. Launch the desktop app.
3. Engine init returns `Io: identity hex: Odd number of digits` (or
   `Invalid character …`), `.expect()` panics, process `abort()`s.

Observed panic:

```
thread 'main' panicked at src/lib.rs:34:18:
initialise csp engine: "Io: identity hex: Odd number of digits"
```

## Root cause

`desktop/engine/src/csp.rs:88-115` — `load_or_create_identity()`:

```rust
// comment claims: "byte-for-byte the `ctx idstore` scheme"
let hex = std::fs::read_to_string(&path)?;
let bytes = hex::decode(hex.trim())                       // ONLY accepts hex
    .map_err(|e| EngineError::io(format!("identity hex: {e}")))?;
```

The canonical loader, `crates/ctx/src/idstore.rs`:

- `parse_identity_file()` (`:169`) detects content and accepts:
  1. OpenSSH armored private key → `openssh_private_signer()` (`:193`), derives
     the 32-byte seed via the `ssh-key` crate;
  2. OpenSSH public-key line → SSH-agent signer (`:215`);
  3. bare hex seed (`:182`) — back-compat, the only one the desktop handles.

`csp-core` exposes `Identity::from_seed(&[u8;32])` and `Identity::seed()`
(`crates/csp-core/src/identity.rs:27,31`) — enough to support form (1) in the
engine the same way `ctx` does.

The `desktop/engine` crate does **not** depend on `ssh-key` yet
(`desktop/engine/Cargo.toml`); `ctx` does.

## Impact

- Desktop app is unusable for anyone using `ctx`'s OpenSSH-key identity mode.
- Identity is **device-global and shared with the `ctx` CLI** — the desktop
  silently diverging from `ctx`'s accepted formats is a correctness problem,
  not just UX.
- The false "byte-for-byte" comment will mislead future maintainers.

## Proposed fix

Make the engine actually `ctx`-compatible (in priority order):

1. **Support reused OpenSSH ed25519 private keys.** Add `ssh-key` to
   `desktop/engine/Cargo.toml`; in `load_or_create_identity`, detect
   `BEGIN OPENSSH PRIVATE KEY`, parse via `ssh_key::PrivateKey::from_openssh`,
   reject encrypted keys with a clear message, derive the 32-byte seed and
   `CoreIdentity::from_seed(&seed)`. Mirror `openssh_private_signer`
   (`crates/ctx/src/idstore.rs:193-211`). Same seed ⇒ **same NodeId**.
2. **Give a clear error for the OpenSSH-public-key / SSH-agent form** instead
   of a hex-decode error (the engine takes an owned `Identity`, so full agent
   delegation is out of scope — but the message must say so).
3. **Fix the doc comment** to state exactly which forms are accepted, or — the
   durable option — extract `ctx`'s `parse_identity_file` into a shared library
   crate both `ctx` and `desktop/engine` depend on, so they cannot drift again.

Note: the legacy `csp-identity-v1 <base64>` form is **not** produced or read by
any current code (`grep -r csp-identity` across the workspace is empty) and
should *not* be added back — normalise such files to bare hex out-of-band
(base64→hex preserves the seed/NodeId).

## References

- `desktop/engine/src/csp.rs:80-115`
- `crates/ctx/src/idstore.rs:1-21, 135-231`
- `crates/csp-core/src/identity.rs:22-31`
- `desktop/src-tauri/src/lib.rs:24-34` (calls `AppState::new` → engine init)
- Related: [0002](0002-identity-and-config-errors-abort-silently.md)
