# 0008 — Auth-key enrollment + per-key expiry in `authorized_keys`

**Severity:** Feature  **Status:** Implementing  **Owner:** —

## Summary

Add a second bootstrap path orthogonal to TOFU: a shared **auth key**
(`CTX_AUTH_KEY` / `--auth-key`) that, when presented at the WebSocket
upgrade, authorizes a new peer to enroll itself into the listener's
`authorized_keys`. After enrollment the peer is indistinguishable from
one added manually with `ctx authorize` — every subsequent connection
is plain pubkey auth.

Also: add a per-key expiry token to `authorized_keys` entries, default
**90 days**, applied at enrollment and via a listen-start migration pass
to entries without one. `expires=never` is the explicit opt-out.

## Why

1. **Cloud / fresh-deployment onboarding.** Today the only bootstrap is
   TOFU (race-y on internet-reachable listeners) or seeding a key list
   before first boot (chicken-and-egg for `ctx clone`). An auth key
   collapses `ctx clone <remote> --auth-key <key>` into a single,
   non-interactive flow.
2. **Bounded blast radius for an unrevoked peer.** Without expiry, a
   pubkey added on day one and never revoked is trusted forever even
   if the device that owned it is long gone. A 90-day default forces
   silent staleness into a visible reconnect-or-die event, while
   `expires=never` remains a clean explicit escape hatch.
3. **Operator footgun protection.** A manually-pasted line without an
   expiry token gets a default expiry on next listen start, rather than
   silently being a permanent grant the operator forgot about.

## Design

See spec.md §10 (updated). Key points:

- **Two trust roots, one steady state.** Auth key authorizes *new*
  enrollments; pubkey is the durable identity. After enrollment the
  auth key is not consulted again until the entry expires or is removed.
- **Wire form (listener).** Preferred: `Authorization: Bearer <key>` on
  the WS upgrade. Fallbacks for clients that cannot set headers (browser
  `WebSocket`): `?auth_key=<key>` query parameter or
  `Sec-WebSocket-Protocol: bearer.<key>`.
- **Invalid key → HTTP 401, no fall-through.** Stale rotated key on an
  enrolled client fails loudly. Absent header → handshake proceeds and
  the pubkey path runs as today.
- **TOFU implicitly disabled when any auth key is configured.** TOFU is
  the *no-auth-key* fallback; the two are mutually exclusive bootstrap
  modes.
- **On-disk expiry format**: `expires=YYYY-MM-DD` (UTC) or
  `expires=never`, in the comment field of the existing OpenSSH line.
  Standard SSH tooling still parses these as comment text.
- **Listen-start migration**: rewrites entries with no expiry token to
  add `expires=<today + CTX_DEFAULT_KEY_TTL>` (default 90d). Atomic temp
  file + rename. Idempotent.
- **Re-enrollment refreshes TTL.** An expired peer reconnecting with a
  valid auth key gets a fresh `expires=`.

## Rotation / revoke semantics

| Action | Effect |
|---|---|
| Unset / remove `CTX_AUTH_KEY` | New enrollments rejected; existing peers unaffected. |
| Rotate `CTX_AUTH_KEY` to a new value | Old key stops enrolling immediately; existing peers unaffected. |
| Remove a pubkey from `authorized_keys` | That peer revoked; new peers can still enroll if an auth key is set. |
| `expires=` reached on an entry | Peer refused at admit time; entry left for audit. Re-enroll via auth key refreshes. |

## CLI surface

- `CTX_AUTH_KEY=<key1>,<key2>` / `--auth-key <k>` — listener-side
  configured set; client-side credential to send on connect.
- `CTX_DEFAULT_KEY_TTL=90d` (or `1y` / `never`) — listener default.
- `ctx clone <url> --auth-key <key>` — enrollment in one shot.
- `ctx authorize <pubkey> [--ttl 30d|never]` — manual add with TTL.
- `ctx auth list` — print authorized peers + remaining TTL.
- `ctx auth extend <peer-prefix> <duration>` — bump `expires=`.

## Acceptance

- All matrix cases in `tests/e2e/tests/auth_key.rs` pass (see test
  file for the table).
- Listen-start migration is idempotent (running `ctx watch` twice
  produces no further diff).
- `ctx clone --auth-key X` succeeds against a listener with empty
  `authorized_keys` and no_tofu enabled.
- `ctx clone --auth-key WRONG` fails at the upgrade with a 401-equivalent
  error message (not an opaque signature failure).
- Stale rotation: a previously-enrolled peer reconnecting *without*
  sending an auth key still works after `CTX_AUTH_KEY` is removed/rotated.
- Expiry: an entry with `expires=2020-01-01` is refused at admit time
  even though its pubkey is in the file.
- `expires=never` is preserved across listen starts (not migrated).
