# syntax=docker/dockerfile:1.7

FROM rust:1.89-slim-bookworm AS builder
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential pkg-config perl \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY tests ./tests

RUN cargo build --release -p ctx --bin ctx

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/ctx /usr/local/bin/ctx

ENV HOME=/data \
    CTX_LOG=info \
    CTX_VAULT_NAME=vault

WORKDIR /data/vault
EXPOSE 9000

# On startup:
#   - init the vault if it doesn't exist yet, naming it after
#     $CTX_VAULT_NAME (defaults to "vault"). The name is sent in the
#     handshake so `ctx clone <url>` defaults the local dir to it.
#   - keep the node identity *on the persisted volume* (inside the vault's
#     .context/, via CTX_IDENTITY) rather than the device-global default
#     ~/.context/id_ed25519 — so the hub keeps the same identity across
#     container/machine restarts and peers don't have to re-trust it.
#   - merge any pubkeys from $CTX_AUTHORIZED_KEYS into the synced
#     authorized_keys (env var read directly by `watch`). Restart-safe:
#     keys already present are skipped.
#
# Environment knobs:
#   PORT                  bind port (default 9000)
#   CTX_CWD               vault directory (default /data/vault). Set this
#                         when the platform mounts the persistent volume
#                         somewhere else — e.g. Fly/Railway use
#                         /mnt/workspace.
#   CTX_NO_TLS=1          bind plain WS instead of WSS — use behind a
#                         reverse proxy that already terminates TLS
#                         (Fly.io, Railway, Render, Cloudflare Tunnel, …).
#                         Read by `ctx watch` directly; no CMD override
#                         needed.
#   CTX_AUTHORIZED_KEYS   ssh-ed25519 lines (or a file path) merged into the
#                         synced authorized_keys on every start.
CMD ["/bin/sh", "-c", "VAULT_DIR=\"${CTX_CWD:-/data/vault}\" && mkdir -p \"$VAULT_DIR\" && cd \"$VAULT_DIR\" && export CTX_IDENTITY=\"$VAULT_DIR/.context/id_ed25519\" && { [ -f .context/config ] || ctx init --name \"$CTX_VAULT_NAME\"; } && { [ -f \"$CTX_IDENTITY\" ] || ctx key >/dev/null; } && exec ctx watch --listen 0.0.0.0:${PORT:-9000}"]
