---
title: CLI · ctx
description: The ctx command-line tool — the native full-node reference implementation. Init, watch, clone, status, snapshot, restore, and read-only history inspection.
---

`ctx` is the command-line reference implementation: a native **full node**.
It sets up context folders, runs realtime sync, connects devices, reports
status, and inspects or restores history. The synced folder is plain files
plus one never-synced `.context/` directory; there is deliberately no `.git`
at the folder root.

## Subcommands

| Command | What it does |
| --- | --- |
| `init [path]` | Create a new, empty scoped vault and device key here |
| `clone <url> [into]` | Bootstrap a new node from a vault served by a listener |
| `watch` | The long-running sync daemon: watch the tree, sync to peers, optionally listen |
| `key` | Generate / print this node's public key in OpenSSH format |
| `authorize <pubkey> [--ttl 90d\|never]` | Add a public key to this node's local `authorized_keys` (default TTL = 90 days) |
| `revoke <pubkey>` | Remove a public key from this node's local `authorized_keys` |
| `auth list [--json]` | Print authorized peers with remaining TTL |
| `auth extend <target> <duration>` | Bump (or set to `never`) the expiry on entries matching `<target>` |
| `auth migrate` | Manually run the listen-start expiry migration (idempotent) |
| `status` | Show identity, peers, sync state, and head / `main` SHA |
| `snapshot <name>` | Create an exact, skew-free named recovery point |
| `restore <target>` | Restore to a named snapshot or a time |
| `log [args]` | Read-only history (wraps the engine-owned git log) |
| `git [args]` | Read-only git inspection of the engine-owned repo (deny-by-default) |
| `scope [action]` | Show / edit the synced scope and `.contextignore` |
| `completions <shell>` | Emit shell completions (bash, zsh, fish, powershell) |

`init` and `clone` both accept `--watch` to bootstrap and stay running as the
sync daemon in one step. `clone` records the origin like git, so you can
`cd <vault> && ctx watch` later.

## Notable flags

- **`--listen`** — bind an inbound listener. Bare `--listen` binds
  `0.0.0.0:9000`; override with an address, `--port`, or `PORT`.
- **`--no-tls`** — serve plaintext `ws://` (behind a TLS-terminating proxy or
  on trusted networks). The default is `wss://` with a self-signed cert.
- **`--no-tofu`** — disable the trust-on-first-use window; require
  pre-authorized keys.
- **`--auth-key <secret>`** (`CTX_AUTH_KEY`) — pre-shared enrollment secret
  (§10). On a listener it authorizes new peers to add themselves to
  `authorized_keys` (with a default TTL); on a connecting node it is sent
  as `Authorization: Bearer …` on the WS upgrade. Comma-separated to
  configure multiple at once (for rotation). Setting any auth key
  implicitly disables TOFU.
- **`--default-key-ttl <duration>`** (`CTX_DEFAULT_KEY_TTL`, default
  `90d`) — expiry written into every new `authorized_keys` entry and
  applied by listen-start migration. Accepts `90d` / `1y` / `12w` / a
  bare integer (days) / `never`.
- **`--peer <url>`** — connect outbound to a peer (repeatable).
- **`--vault-id`, `--name`** — share a memorable vault id / human label
  instead of the default opaque UUID.
- **`--dir`** — the vault/scope root, decoupled from the process working
  directory.

## Configuration precedence

Every deployment knob exists in three forms with the precedence
**flag > environment variable > config file**, so a headless listener needs no
flags:

```sh
CTX_CWD=/data/vault CTX_AUTHORIZED_KEYS="$KEYS" ctx watch --listen
```

Key environment variables: `CTX_CWD` (scope root), `CTX_IDENTITY` (device key
file, default `~/.context/id_ed25519`), `CTX_LOG` (log filter, default
`ctx=info,csp_core=info`), `CTX_AUTHORIZED_KEYS`, `CTX_AUTH_KEY` (§10 auth
key for enrollment / outbound bearer), `CTX_DEFAULT_KEY_TTL` (default
`authorized_keys` expiry, default `90d`), `CTX_NO_TLS`, `CTX_NO_TOFU`, and
`PORT`.

See the [quick start](/quick-start/) for an end-to-end two-device walkthrough,
and the [design specification](/protocol/spec/) for the CLI surface rationale.
