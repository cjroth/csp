# Context Sync Protocol

Context Sync Protocol (CSP) syncs agent context across tools, devices, and apps.

It is designed for the folders where agents and humans share working memory: notes, plans, prompts, task state, project context, and other files that need to follow you between a CLI, desktop app, editor plugin, or remote machine.

Your context stays as normal files on disk, so any app or agent can read and edit it directly. CSP watches for changes, syncs them in real time, keeps local history, and lets you recover the folder from an earlier point in time.

CSP stores that history in a Git-compatible format, so full nodes can inspect past versions with regular Git tools without turning the synced folder into a Git repo. It can run through a server, a desktop app, or direct peer connections; the protocol does not require one specific hosting model.

If two devices edit different parts of a file, both edits are kept. If they edit the same part at the same time, CSP chooses one result in a predictable way and keeps the other version in history. It does not write conflict markers into your files.

## Components

- **Core** - `csp-core`, the Rust protocol engine. It owns the object model, identity, signing, storage, deterministic fold/merge, and sync state.
- **SDK** - WebAssembly and TypeScript bindings that let apps and plugins use CSP without rebuilding the protocol themselves.
- **CLI** - `ctx`, the command-line tool for setting up context folders, running realtime sync, connecting devices, checking status, inspecting history, and restoring older versions.
- **Desktop** - Context Desktop, a background app for managing and syncing context folders from a regular desktop interface.
- **Obsidian** - Context for Obsidian, a plugin for syncing Obsidian vaults as agent-readable context on desktop and mobile.

## Principle

One core, thin bindings. Protocol behavior lives in Rust once; every surface calls into the same engine.

## Requirements

- Rust (stable, 1.89+) — `rustup`
- `git` on `PATH` — used only by the read-only `ctx git` inspector
- For the wasm/TypeScript SDK: the `wasm32-unknown-unknown` target, `wasm-pack`, and `bun`

```sh
rustup target add wasm32-unknown-unknown   # SDK only
```

## Build

```sh
cargo build --release -p ctx               # the `ctx` CLI → target/release/ctx
cargo build --workspace                    # everything (core, CLI, wasm)
```

Optionally put it on your `PATH`:

```sh
install -m755 target/release/ctx ~/.local/bin/ctx
# or: cargo install --path crates/ctx
```

## Test

```sh
cargo test --workspace                     # core + headline conformance + multi-process e2e
```

This runs the deterministic-fold conformance/property suite (the headline gate), the
multi-process end-to-end suite (real `ctx` processes: sync, relay, offline/reconnect,
auth, snapshot/restore, genuine git-coherence), and the cross-surface interop tests.

## Quick start (two devices syncing a folder)

The simplest path is **init + listen on one device, `clone` on the other** —
`clone` bootstraps trust automatically (trust-on-first-use), so there is no
manual key exchange.

```sh
# --- device A: create a vault and start serving it ---
ctx --dir ~/team-notes init --name team-notes        # name → folder/display
ctx --dir ~/team-notes watch --listen --no-tls        # binds 0.0.0.0:9000

# --- device B: clone it and keep it synced, in one command ---
ctx clone ws://A-HOST:9000 --watch                    # creates ./team-notes/
```

`ctx clone` records the origin (like git), so even without `--watch` you can
just `cd team-notes && ctx watch` later. `init`/`clone` also take `--watch`
to stay running as the daemon immediately.

`ctx watch` logs what it's doing — peer connect, handshake outcome (and *why*
it was rejected, e.g. unauthorized key or vault-id mismatch), catch-up, and
each commit. If a peer is refused, the listener prints the exact
`ctx authorize "ssh-ed25519 …"` line to run.

Now edit any text file under `~/team-notes` and it appears under the cloned
`./team-notes` (and vice versa) in well under a second. Concurrent edits to different regions
of a file both survive; same-region conflicts resolve deterministically with
the losing version kept in history — never with conflict markers.

### Transport, ports, clone target

- **Default transport is `wss://`** with a self-signed cert (trust is the
  ed25519 handshake, not a CA — §10). `--no-tls` serves plaintext `ws://`
  (behind a TLS-terminating proxy, or local/trusted) — connectors then use
  `ws://`. The example above uses `--no-tls` for a no-friction localhost demo;
  drop it (and dial `wss://`) for the encrypted default.
- **Bare `--listen` binds `0.0.0.0:9000`** (unprivileged; not 443). Override
  with an explicit `addr`, `--port`, or the `PORT` env var.
- **Vault id vs name.** `vault_id` is an opaque **UUID** by default (the
  protocol's "same vault?" guard — it never leaks the node key);
  `--vault-id` overrides it to share a memorable id. The **name** is a
  separate human label (`--name`, else the init directory's basename) used
  for display and clone-folder naming — not a uniqueness guarantee.
- **`ctx clone <url>`** creates `./<name>/` (falling back to a short id
  slug, never the raw id). `ctx clone <url> .` clones into the current
  folder; `ctx clone <url> <path>` a specific one. It refuses to clobber an
  existing vault. Add `--watch` to bootstrap and stay synced in one step.

### Without clone (explicit mutual authorization)

Since each `ctx init` mints its own UUID `vault_id`, two independently
`init`-ed vaults must be given the **same explicit id** to be the same vault
(clone handles this for you):

```sh
ctx --dir ~/notesA init --vault-id team-notes        # shared, explicit id
ctx --dir ~/notesB init --vault-id team-notes
ctx --dir ~/notesA authorize "$(ctx --dir ~/notesB key)"
ctx --dir ~/notesB authorize "$(ctx --dir ~/notesA key)"
```

### Other everyday commands

```sh
ctx --dir ~/notesA status --json             # identity, peers, head SHA, frontier
ctx --dir ~/notesA snapshot before-refactor  # exact, skew-free recovery point
ctx --dir ~/notesA restore before-refactor   # or: restore <unix-time>
ctx --dir ~/notesA log --oneline             # read-only history
ctx --dir ~/notesA git show main:notes.md    # read-only stock-git inspection
ctx completions zsh                           # bash | zsh | fish | powershell
```

Every deployment knob also has a `CTX_*` env var and a config-file key
(precedence: flag > env > config), so a headless listener needs no flags:

```sh
CTX_DIR=/data/vault CTX_AUTHORIZED_KEYS="$KEYS" ctx watch --listen
```

For internet-reachable listeners, prefer the **auth-key enrollment** path
over leaving TOFU on. Set `CTX_AUTH_KEY=<secret>` (comma-separated for
rotation); each new peer clones with `ctx clone wss://host --auth-key
<secret>` and the listener appends their pubkey to `authorized_keys` with
a 90-day default expiry (override via `CTX_DEFAULT_KEY_TTL`). Inspect
remaining lifetimes with `ctx auth list`; bump them with
`ctx auth extend <peer> <duration>`; an expired peer re-enrolls by
re-presenting the auth-key. Rotating or removing the secret stops *new*
enrollments but never severs already-enrolled peers (revoke a specific
peer by removing its `authorized_keys` line).

The synced folder is plain files plus one `.context/` directory (engine state, never
synced). The engine keeps a real, stock-git-compatible history at
`.context/git` — inspect it with the bundled read-only `ctx git`, or point an
unmodified `git` at it via `--git-dir`. There is deliberately **no `.git` at the
folder root**, so CSP coexists with a project's own git repo.

## SDK (wasm + TypeScript)

```sh
cd sdks/typescript
bun install
bun run build:wasm     # compiles the reduced thin-node surface via wasm-pack
bun test               # unit + cross-surface interop (wasm ≡ native, byte-identical)
```

## Design Docs

- [Protocol](spec.md)
- [Desktop](desktop-app-spec.md)
- [Obsidian](obsidian-plugin-spec.md)
