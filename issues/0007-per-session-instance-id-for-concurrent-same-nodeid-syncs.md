# 0007 — Concurrent same-NodeId sync sessions conflate at the relay; need an in-memory per-session instance ID

- **Severity:** Medium (breaks the common dev/test workflow; eventual correctness survives via the §5.1 SHA tiebreaker, but live convergence between same-key replicas does not)
- **Status:** Open — design exploration
- **Component:** `crates/csp-core` (`net` relay bus, `session` handshake/anti-entropy, `state`/`vault` integrate), `plugins/obsidian` (`sync-controller`, identity resolution)
- **Raised:** 2026-05-19, debugging two Obsidian windows on one Mac syncing one vault through the Railway relay

## Summary

Running **two sync sessions for the same vault on the same machine under one
SSH key** is a routine thing to do — testing sync end-to-end, a clone-and-poke
session, two editor windows on a cloned vault — yet it currently fails to
converge. Observed concretely with two Obsidian windows (a vault cloned
*including* the plugin config) both pointed at the same Railway relay:

- Edits in window **A** reach the relay correctly.
- The relay never propagates them back into window **B**.

Root of the conflation: on desktop the plugin resolves identity to the
**device-global** `~/.context/id_ed25519`
(`plugins/obsidian/src/main.ts:250-254`; `main.ts:164` only gives *mobile* a
vault-local key), so both windows present the **same NodeId** to the relay.
A NodeId *is* the ed25519 key (spec.md §6.1, ~698); the wire handshake
identifies a peer solely by `node_ssh` (`crates/csp-core/src/session.rs:124-133`,
`157-160`). The two live instances are therefore **indistinguishable to every
peer-tracking and anti-entropy decision**, even though they are genuinely
distinct running replicas.

Per spec §5.1 (spec.md:128-145) the *single-writer-per-vault* rule is a soft
operational invariant: the strict total order `(counter, NodeId, commitSHA)`
keeps history correct via the SHA tiebreaker even under a shared NodeId, so
this is **not** a correctness defect. The defect is that **live convergence
between the two instances stalls** — which is exactly the case a developer
hits first and most often. The plugin already knows the situation is
unsupported and can only *warn*: `sync-controller.ts:210-219` documents that
resuming under the same device key when it may be live elsewhere is the §5.1
hazard and "the plugin cannot fork the key itself — it warns."

## Why it stalls (and why it is the *common* case, not an edge case)

1. **Catch-up is one-shot at handshake.** The listener emits one
   `FrontierDigest` when the session establishes
   (`session.rs:256-260`); thereafter deltas flow only via the Live relay
   bus. A second instance that connected before the first authored — or whose
   advertised frontier looks already-current — gets no further catch-up
   without a reconnect.

2. **Cloned `.context/` makes B look caught-up.** A vault copied "including
   the plugin config" carries an identical `known` set, frontier, and
   persisted counter. Combined with the identical NodeId, B's
   `FrontierDigest`/`WantTips` exchange (`session.rs:263-284`) concludes there
   is nothing to fetch, while A keeps authoring fresh content-addressed SHAs
   the relay accepts (`vault.rs:402-427`; dedup is by commit SHA at `:415`, so
   A's new objects *are* admitted upstream).

3. **The relay bus has no notion of "which instance."** `Node` fans new
   closures to a process-wide `tokio::broadcast` (`net.rs:35-59`), and each
   accepted connection independently `subscribe()`s
   (`net.rs:436-484`). There is no per-peer table keyed by anything, and
   `Lagged` is silently swallowed (`net.rs:479`). Nothing lets the relay say
   "peer B is a *different live replica* that still needs A's delta" — because
   B and A are the same NodeId and there is no finer identity to key on.

4. **Counter collision under one NodeId.** Two live instances independently
   advance `state.observe(counter)` (`vault.rs:419`) and author at
   potentially-equal counters; the fold stays total only by the SHA
   tiebreaker. Order is *correct* but arbitrary and unattributable to a
   specific running instance.

None of (1)–(4) is exotic. "Same repo, same machine, same key, two syncs at
once" is the default shape of *testing the sync at all*.

## Proposed direction — an in-memory per-session instance ID

Mint a random **session/instance ID at the start of each sync session**, held
**in memory only for that session's lifetime** (regenerated on every
connect/reconnect; never persisted, never authored into an object). It
represents *this live instance*, distinct from the durable NodeId.

Layering — this is the crucial scoping decision:

- **It is a transport/relay-layer disambiguator, not a history-layer fix.**
  Every primitive is signed by the NodeId key and authored-once; the order
  tuple is `(counter, NodeId, commitSHA)`. An ephemeral, unsigned ID
  *cannot* enter authored history or the order tuple without becoming durable
  and signed (which would just be "a second NodeId"). So the session ID
  **makes concurrent same-NodeId instances converge and stay
  distinguishable at the relay/anti-entropy/Live layer**; it does **not**
  remove the §5.1 history-confusion or counter-collision (those still require
  a genuinely distinct NodeId — the spec's prescribed fix, which this issue
  does **not** replace, only complements for the common dev case).

- **It carries no trust.** Authorization stays strictly per-NodeId against
  `authorized_keys` (`vault.rs:402-413`, spec §6.1). The session ID must
  never become a thing to authorize or it reintroduces a trust-bootstrap
  problem. It is liveness/identity-of-instance only.

- **Crash-safe by construction.** In-memory-only means it vanishes the
  instant the session/process ends — the same "cannot get stuck" virtue that
  issue [0006](0006-vault-level-lock-for-concurrent-sync.md) argues for with
  OS advisory locks, for the same reason. No staleness/PID-liveness heuristics.

Concrete leads:

- A per-session random **nonce already exists** in the handshake
  (`session.rs:124-133`, `self.my_nonce`). The cheapest path may be to
  surface that (or a sibling field) as the relay's peer key, so the bus/
  anti-entropy can track `(NodeId, sessionId)` connections independently and
  *not* treat a same-NodeId peer as already-converged. If the ID needs to
  travel on the wire to other peers (vs. staying local to the relay's peer
  table), that is a `proto` bump (spec §6.1, ~684-686).
- Relay fan-out keyed by `(NodeId, sessionId)` so A's delta is delivered to
  every *other* live instance, including a same-NodeId sibling, while still
  suppressing echo back to the originating session.
- Anti-entropy: a same-NodeId peer with a *different* sessionId should be
  treated as a distinct replica for catch-up purposes, not assumed current.

## Impact if left unaddressed

- The first thing anyone does to validate sync — run it twice on one box —
  appears broken (one-way propagation), with no error, just silent
  non-convergence. High "looks broken" cost for a non-correctness issue.
- Workarounds are heavy and manual: hand-assign a distinct `identityPath`
  per instance + authorize the extra key + re-clone without `.context/`
  (the §5.1 "fork a fresh NodeId" path). Fine for production multi-device;
  disproportionate for "me testing it."
- Adjacent to [0006](0006-vault-level-lock-for-concurrent-sync.md) but
  distinct: 0006 is two *processes* corrupting one `.context/` via the
  filesystem; this is two *sync sessions* (possibly separate vault copies)
  conflated on the *relay* because they share a NodeId. A vault lock would
  not help here (the instances may be different folders), and a session ID
  would not help 0006 (it is about on-disk state, not transport identity).

## Open questions

- **Wire-visible or relay-local?** Does the session ID need to reach other
  peers (so *they* can distinguish two same-NodeId sources), or is keying the
  relay's own peer/fan-out table enough? The former is a coordinated `proto`
  bump; the latter may be purely internal.
- **Reuse the handshake nonce, or a dedicated field?** `self.my_nonce` is
  already per-session and random — is elevating it (or deriving from it)
  sound, or does its channel-binding role (`session.rs` Hello `cb`) argue for
  a separate, single-purpose field?
- **Thin nodes.** The Obsidian plugin is a thin node; how does it mint/carry
  a session ID, and does the desktop "one node, many vaults" host need one
  ID per `(vault, connection)` rather than per process?
- **Does this paper over §5.1 too well?** If same-key multi-replica now
  *converges* silently, do we still surface the history-confusion / counter-
  collision warning (`sync-controller.ts:210-219`) loudly enough that
  production users don't lean on a shared key by accident? The session ID
  fixes the dev-ergonomics symptom without fixing the underlying history
  ambiguity — the warning must not get quieter.
- **Attribution.** Should the (ephemeral) session ID be exposed in
  diagnostics/logs only — so "which instance authored this" is answerable at
  runtime — without ever being durable (mirroring 0006's "diagnostic payload,
  not liveness logic" caveat)?

## References

- `spec.md` §5.1 (lines 128-145) — strict total order, SHA tiebreaker,
  single-writer-per-vault soft invariant
- `spec.md` §6.1 (~684-704) — wire `proto` versioning; per-author
  authorization; NodeId = ed25519 key; identity / single-writer protection
- `crates/csp-core/src/session.rs:124-133` — `Hello` carries `node_ssh` +
  per-session `my_nonce` (candidate to surface as the instance ID)
- `crates/csp-core/src/session.rs:256-260,263-304` — one-shot
  `FrontierDigest` on establish; `WantTips`/`Objects`/`Live`; relay gated on
  `admitted > 0`
- `crates/csp-core/src/net.rs:35-59,436-484` — `Node` + process-wide
  broadcast bus; per-connection `subscribe()`; no per-peer keying; `Lagged`
  swallowed
- `crates/csp-core/src/vault.rs:402-427` — `integrate`: per-author
  authorization, content-SHA dedup (`:415`), `observe(counter)` (`:419`)
- `plugins/obsidian/src/main.ts:164,250-254` — desktop shares device-global
  `~/.context/id_ed25519`; only mobile gets a vault-local key
- `plugins/obsidian/src/sync-controller.ts:210-219` — the §5.1 warning; the
  plugin cannot fork the key itself
- Related: [0006](0006-vault-level-lock-for-concurrent-sync.md) (concurrent
  *processes* on one `.context/`); [0001](0001-engine-identity-loader-not-ctx-compatible.md),
  [0005](0005-ssh-agent-signing-not-wired-to-engine.md) (identity loading/signing)
</content>
</invoke>
