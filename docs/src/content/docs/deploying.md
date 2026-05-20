---
title: Deploying a hub
description: Run ctx as an always-on sync hub with the included Docker, Fly.io, and Railway configs.
---

CSP has no special server — a hub is just `ctx watch --listen` running
somewhere always-on, holding a clone of the vault on a persistent volume.
Peers `clone`/`watch` against it; it relays changes and keeps full history.
The repo ships a `Dockerfile`, `fly.toml`, and `railway.toml` to make that a
one-command deploy.

## The container

The included `Dockerfile` builds a tiny image (multi-stage Rust build →
`debian-slim`) that, on startup:

1. inits the vault at the volume path if it doesn't exist yet, naming it
   after `CTX_VAULT_NAME`;
2. keeps the node identity on the persisted volume (inside the vault's
   `.context/`, via `CTX_IDENTITY`) so the hub keeps a **stable identity**
   across restarts — peers that trusted it once stay trusting it;
3. merges `CTX_AUTHORIZED_KEYS` into the synced `authorized_keys` (skipping
   keys already present, so it's safe across restarts);
4. `exec`s `ctx watch --listen 0.0.0.0:$PORT`.

### Environment knobs

Every deployment knob is also a `CTX_*` env var (precedence: flag > env >
config), so the headless hub needs no custom command:

| Variable | Default | Purpose |
| --- | --- | --- |
| `PORT` | `9000` | Bind port. Most platforms inject this. |
| `CTX_CWD` | `/data/vault` | Vault directory. Point it at the mounted volume. |
| `CTX_VAULT_NAME` | `vault` | Name written on first init; clients' default `clone` dir. |
| `CTX_NO_TLS` | unset | `1` → bind plain `ws://` (behind an edge that terminates TLS). |
| `CTX_AUTHORIZED_KEYS` | unset | `ssh-ed25519 …` lines (or a file path) merged on every start. |
| `CTX_AUTH_KEY` | unset | Pre-shared enrollment secret (§10). Comma-separated for rotation. When set, TOFU is implicitly disabled and a new peer must present this on the WS upgrade to get its pubkey added to `authorized_keys` (with the default TTL). |
| `CTX_DEFAULT_KEY_TTL` | `90d` | Default expiry written into each new `authorized_keys` entry (auth-key enrollment, manual `ctx authorize`, listen-start migration). Accepts `90d` / `1y` / `12w` / a bare integer (days) / `never`. |
| `CTX_LOG` | `info` | Log filter. |

`CTX_NO_TLS=1` is the right setting behind a managed proxy (Fly, Railway,
Render, Cloudflare Tunnel): the platform terminates TLS at its edge and
forwards plain WS to the container. The hub identity is still verified
end-to-end via the ed25519 handshake — a proxy can't forge it — so clients
keep connecting with `wss://`. Leave `CTX_NO_TLS` unset to serve native
`wss://` with a self-signed cert (trust is the handshake, not a CA).

### Plain Docker

```sh
docker build -t csp-hub .
docker run -d --name csp-hub \
  -p 9000:9000 \
  -v csp_data:/data/vault \
  -e CTX_AUTHORIZED_KEYS="$(ctx key)" \
  csp-hub
```

Then from a client: `ctx clone wss://your-host:9000 --watch` (or `ws://…`
with `--no-tls` on a trusted network).

## Fly.io

`fly.toml` defines a `shared-cpu-1x` machine with a persistent volume at
`/mnt/workspace`, edge TLS (`force_https`), and `CTX_NO_TLS=1`.

```sh
fly launch --copy-config --no-deploy   # creates the app + volume
fly secrets set CTX_AUTHORIZED_KEYS="$(ctx key)"
fly deploy
```

`CTX_AUTHORIZED_KEYS` is set as a Fly **secret** rather than in `fly.toml`
so the key material never lives in the repo. Clients then:

```sh
ctx clone wss://your-app.fly.dev --watch
```

## Railway

`railway.toml` pins the Dockerfile builder and an always-restart policy.
Railway terminates TLS at its edge and injects `PORT`, so configure the
rest as service **Variables** in the dashboard:

```
CTX_NO_TLS = 1
CTX_CWD    = /mnt/workspace
CTX_AUTHORIZED_KEYS = ssh-ed25519 AAAA…   # mark as a secret
CTX_VAULT_NAME = vault
```

Attach a Railway **Volume** and set its mount path to the same value as
`CTX_CWD` (e.g. `/mnt/workspace`) so the vault history and node identity
survive redeploys. Clients connect via the generated domain:

```sh
ctx clone wss://your-app.up.railway.app --watch
```

## Trusting the hub

CSP clones are trust-on-first-use: the first `ctx clone` pins whatever hub
identity answers, and every later connection must match it — so a deployed
hub is trusted automatically as long as its identity is stable (which the
on-volume `CTX_IDENTITY` guarantees).

For **public-internet listeners**, prefer the **auth-key enrollment** path
over leaving TOFU on: set `CTX_AUTH_KEY=<some-secret>` and tell each new
peer to clone with `ctx clone wss://your-host --auth-key <secret>`. On a
successful clone the device's pubkey is appended to `authorized_keys` (with
a 90-day expiry by default — see `CTX_DEFAULT_KEY_TTL`), and the auth key
isn't needed on subsequent reconnects. Rotate the secret freely: enrolled
peers keep working; only *new* enrollments need the new value. Revoke a
specific peer by removing its line with `ctx revoke <pubkey>`. Inspect
remaining lifetimes with `ctx auth list`.

To verify the fingerprint out of band before first clone, read the hub's
public key from the running container:

```sh
# Fly
fly ssh console -C "ctx --dir /mnt/workspace --identity /mnt/workspace/.context/id_ed25519 key"
# plain Docker
docker exec csp-hub ctx --dir /data/vault --identity /data/vault/.context/id_ed25519 key
```

## Persistence and recovery

Everything that matters lives under the vault's `.context/` directory on the
mounted volume: the full document history, named snapshots, and the hub's
node identity. Back up the volume with any tool you like — it contains the
complete history. If a client wipes data while connected, recover from any
clone with `ctx restore <snapshot|time>` and let it sync back up; the hub
and other peers converge on the restored state.

If the operator's bootstrap key ever disappears from `authorized_keys`,
restart the hub: `ctx watch` re-merges `CTX_AUTHORIZED_KEYS` on boot, so as
long as the secret is still set the bootstrap key comes back.
