---
title: Quick start
description: Sync a folder across two devices with the ctx CLI in two commands.
---

The fastest way to see CSP work is **init + listen on one device, `clone` on
the other**. `clone` bootstraps trust automatically (trust-on-first-use), so
there is no manual key exchange.

## Requirements

- Rust (stable, 1.89+) — via `rustup`
- `git` on `PATH` — used only by the read-only `ctx git` inspector
- For the wasm / TypeScript SDK: the `wasm32-unknown-unknown` target,
  `wasm-pack`, and `bun`

## Build the CLI

```sh
cargo build --release -p ctx          # → target/release/ctx
install -m755 target/release/ctx ~/.local/bin/ctx   # optional: put it on PATH
```

## Two devices syncing a folder

```sh
# --- device A: create a vault and start serving it ---
ctx --dir ~/team-notes init --name team-notes   # name → folder / display
ctx --dir ~/team-notes watch --listen --no-tls   # binds 0.0.0.0:9000

# --- device B: clone it and keep it synced, in one command ---
ctx clone ws://A-HOST:9000 --watch               # creates ./team-notes/
```

Now edit any text file under `~/team-notes` and it appears under the cloned
`./team-notes` (and vice versa) in well under a second. Concurrent edits to
different regions of a file both survive; same-region conflicts resolve
deterministically with the losing version kept in history — never with
conflict markers.

`ctx clone` records the origin (like git), so even without `--watch` you can
`cd team-notes && ctx watch` later. `ctx watch` logs each peer connect,
handshake outcome (and *why* a peer was rejected), catch-up, and commit. If a
peer is refused, the listener prints the exact `ctx authorize "ssh-ed25519 …"`
line to run.

:::note[Transport defaults]
The default transport is `wss://` with a self-signed cert — trust is the
ed25519 handshake, not a CA. `--no-tls` serves plaintext `ws://` (used above
for a no-friction localhost demo); drop it and dial `wss://` for the encrypted
default. Bare `--listen` binds `0.0.0.0:9000` (override with an explicit
address, `--port`, or `PORT`).
:::

## Explicit mutual authorization (without clone)

Each `ctx init` mints its own UUID `vault_id`, so two independently `init`-ed
vaults must be given the **same explicit id** to be the same vault:

```sh
ctx --dir ~/notesA init --vault-id team-notes
ctx --dir ~/notesB init --vault-id team-notes
ctx --dir ~/notesA authorize "$(ctx --dir ~/notesB key)"
ctx --dir ~/notesB authorize "$(ctx --dir ~/notesA key)"
```

## Everyday commands

```sh
ctx --dir ~/notesA status --json              # identity, peers, head SHA, frontier
ctx --dir ~/notesA snapshot before-refactor   # exact, skew-free recovery point
ctx --dir ~/notesA restore before-refactor    # or: restore <unix-time>
ctx --dir ~/notesA log --oneline              # read-only history
ctx --dir ~/notesA git show main:notes.md     # read-only stock-git inspection
ctx completions zsh                            # bash | zsh | fish | powershell
```

Every deployment knob also has a `CTX_*` env var and a config-file key
(precedence: flag > env > config), so a headless listener needs no flags:

```sh
CTX_CWD=/data/vault CTX_AUTHORIZED_KEYS="$KEYS" ctx watch --listen
```

The synced folder is plain files plus one `.context/` directory (engine state,
never synced). There is deliberately **no `.git` at the folder root**, so CSP
coexists with a project's own git repo. See the [CLI overview](/cli/overview/)
for the full command surface.
