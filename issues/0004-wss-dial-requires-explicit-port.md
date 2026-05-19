# 0004 — `wss://` clone fails when the URL omits the port

- **Severity:** High (blocks cloning from any standard-TLS host, e.g. Railway/Fly)
- **Status:** Fixed
- **Component:** `crates/csp-core` (`net::dial`)
- **Found:** 2026-05-19, cloning a folder from the desktop app against a Railway-hosted server
- **Fixed:** 2026-05-19, `net::normalize_url`

## Summary

`dial()` passes the URL authority straight to `TcpStream::connect` without
ensuring a port is present. When the user enters a `wss://` URL with no
explicit port (the normal case for a TLS host on 443, e.g.
`wss://csp-production-b2b3.up.railway.app`), the connect fails immediately —
before any DNS or TCP — with a confusing `invalid socket address` error.

## Reproduction

In the desktop **Clone Remote** dialog, enter a portless `wss://` URL:

```
wss://csp-production-b2b3.up.railway.app
```

Result:

```
probe wss://csp-production-b2b3.up.railway.app: protocol error:
  tcp connect csp-production-b2b3.up.railway.app: invalid socket address
```

Workaround: append the port explicitly — `wss://csp-production-b2b3.up.railway.app:443`
— which connects fine (TLS SNI, channel binding, and the tungstenite handshake
all already handle hostnames).

## Root cause

`crates/csp-core/src/net.rs`, in `dial()` (`:197-228`):

```rust
let authority = rest.split('/').next().unwrap_or(rest);              // :199 "host" (no port)
let host = authority.rsplit_once(':').map(|(h, _)| h).unwrap_or(authority); // :200 SNI host (ok)
let tcp = TcpStream::connect(authority)                              // :201 ← fails
    .await
    .map_err(|e| CspError::Protocol(format!("tcp connect {authority}: {e}")))?;
```

Tokio's `ToSocketAddrs` impl for `&str` splits on the last `:` to separate host
from port before resolving DNS. With **no colon at all**, it cannot extract a
port and returns `invalid socket address` — the error never reaches DNS or the
network. `dial()` never supplies the scheme-default port, so any `wss://host`
(or `ws://host`) without an explicit `:port` is unconnectable, even though that
is the common form for hosts terminating TLS on 443 (Railway, Fly, any reverse
proxy).

The clone dialog's placeholder (`wss://192.168.1.42:51820`,
`desktop/src/components/dialogs/CloneRemoteDialog.tsx`) always carries a port,
so the defect is masked for the LAN/IP case and only surfaces for hosted
deployments.

## Impact

- Cloning from any standard-port TLS server fails out of the box; users must
  know to hand-type `:443`.
- The error text (`invalid socket address`) points at the address, not at the
  missing port, so it reads like a DNS/host problem and is hard to diagnose.

## Proposed fix

In `dial()`, default the port from the scheme when the authority has none:

- `wss://` → `:443`, `ws://` → `:80`, applied only when `authority` has no
  port. Use the result for `TcpStream::connect` (and the `tcp connect …` error
  string); keep line `:200`'s `host` as-is for SNI.
- Guard the IPv6 case: a bracketed literal like `[::1]` "contains `:`" but has
  no port — detect "has a port" via the host/port split rather than a naive
  `contains(':')`, consistent with the existing `host` parse on `:200`.
- Optionally update the `CloneRemoteDialog` placeholder/help text to show that
  the port is optional for `wss://`.

## Resolution

Added `net::normalize_url`, applied at the top of `dial()` so every caller
(`probe`, `connect_once`) benefits:

- **Scheme optional** — a bare `example.com` is assumed `wss://`; `https://`
  / `http://` are accepted as `wss://` / `ws://` aliases (so a pasted Railway
  URL works as-is).
- **Port optional** — the scheme default is supplied when absent: `443` for
  `wss`, `80` for `ws`. IPv6 bracket literals are handled (a `:` inside
  `[...]` is not mistaken for a port).
- UI: `CloneRemoteDialog` placeholder changed to a bare domain plus a hint
  that scheme/port are optional.
- Covered by `net::tests::normalize_url_assumes_wss_and_default_port`.

So `wss://csp-production-b2b3.up.railway.app` (or just the bare host) now
normalizes to `wss://csp-production-b2b3.up.railway.app:443` and connects.

## References

- `crates/csp-core/src/net.rs` — `normalize_url` + `dial` (applies it)
- `crates/csp-core/src/net.rs:159-178` (`probe`, the caller)
- `desktop/engine/src/csp.rs:604-613` (`clone_remote` → `probe`)
- `desktop/src/components/dialogs/CloneRemoteDialog.tsx` (URL input + placeholder)
- `crates/csp-core/src/error.rs:19-20` (`Protocol` error wrapper)
