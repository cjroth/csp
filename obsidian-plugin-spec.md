# Context for Obsidian — Design Specification

> Status: draft / greenfield design. Nothing here is implemented yet.
> Companion to `spec.md` (Context Sync Protocol / CSP). This document specifies
> an **Obsidian plugin**; it specifies **no protocol behavior** — all sync,
> merge, identity, history, scope, and auth semantics are defined by `spec.md`
> and are reused, never reimplemented.
> This is the concrete realization of the host plugin CSP §16 names as the
> first SDK target ("Host plugins (first target: an Obsidian plugin)").
> It is a port of the existing **agentsync Obsidian plugin** onto the CSP
> TypeScript/wasm SDK in place of the agentsync (Automerge-over-hub) SDK.
> Working plugin name: **Context for Obsidian** (final name TBD).
>
> **REVISED (pre-release, implemented & verified): one engine everywhere.**
> The original design below treats the plugin as a *thin node that cannot
> merge* (merge/odb "compiled out of wasm", needs a full node to compute the
> merged tree). That split is **superseded**: `csp-core`'s deterministic
> 3-way merge/fold now compiles to `wasm32`, so the plugin runs the
> **identical Rust engine** as `ctx` and **computes its own byte-identical
> `main`** (`@csp/sdk` = `WasmEngine`/`csp_core::MemEngine` + the shared
> sans-IO `Session`). The only real limit is unchanged and platform-bound:
> a WebView **cannot listen**, so the plugin is outbound-only and still needs
> a listenable peer (a `ctx watch --listen`/desktop/relay) as rendezvous.
> Wherever this document says "thin node can't merge / receives the merged
> tree from a full node / merge compiled out", read instead: *runs the same
> merge locally; only listen/relay + deep-retention are delegated*. Proven by
> the §18 SDK⇄real-`ctx` parity suite (`sdks/typescript/test/e2e`). See
> `spec.md` §4/§7 (also revised).

---

## 1. Summary

Context for Obsidian keeps an Obsidian vault's notes byte-identical across a
user's devices using CSP (`spec.md`). It runs entirely inside Obsidian on
desktop (Electron) and mobile (Capacitor WebView) — `isDesktopOnly: false`.

The plugin is a CSP **thin node** (CSP §7) built on the **single wasm module +
thin TypeScript bindings** of CSP §16/§4: it holds a complete local working
copy, authors its own **signed primitive commits** offline, and converges on
reconnect, but it **never runs the 3-way merge engine and never listens/
relays** (CSP §7 HARD INVARIANT). It connects to a CSP **full node in listen
mode** (CSP §6.1) — e.g. a `ctx watch --listen` process or Context Desktop's
per-folder listener — which carries deep history and serves the deterministic
merged tree back.

The plugin contributes only host glue: Obsidian-vault file I/O, the Obsidian
event → SDK push path, the listen/transport plumbing (an outbound WebSocket
client), settings UI, and lifecycle. **No protocol, merge, fold, ordering, or
convergence logic lives in the plugin** — that is `csp-core`, reached through
the SDK (CSP §16 "one core, thin bindings").

---

## 2. Architecture decision: thin node on the CSP TypeScript/wasm SDK

- **The plugin consumes the CSP TypeScript SDK** — `csp-core` compiled to one
  wasm module plus typed bindings and injected host adapters (CSP §16). It is
  **not** a reimplementation; there is exactly one protocol implementation
  (Rust). The wasm bytes are inlined at build time (esbuild `define`), decoded
  to a `Uint8Array`, and `init`'d once — no runtime fetch, the only path that
  loads WebAssembly reliably in a mobile WebView (unchanged from the agentsync
  plugin).
- **Thin-node / wasm profile only** (CSP §4, §7, §16): the wasm surface is
  object encode/decode, the sync state machine, auth, and framing. The
  merge/fold engine and the on-disk odb/packfiles are **compiled out** (CSP
  §16 — the same source, feature-gated by node profile). The plugin therefore
  never computes the multi-tip `main`; it appends primitives, computes only
  the trivial `|F|=1` self-collapse wrapper (CSP §7, §5.3 degenerate case),
  and receives the merged tree from the connected full node (CSP §6.5/§7).
- **Sibling of, not coupled to, Context Desktop.** Context Desktop is a *full*
  node linking `csp-core` natively (`desktop-app-spec.md` §2). This plugin is
  the *thin* counterpart on the wasm/TS path. Both are "thin wrappers over
  `csp-core`"; they differ only in node tier and the engine profile they load.
- **Module decomposition is retained from the agentsync plugin**, re-pointed
  at CSP concepts (§7). The split (entry/lifecycle, controller, host bridge,
  storage adapter, identity store, settings, settings UI, path filter,
  catch-up, status bar) is good host-glue factoring and survives the SDK swap;
  what changes is the *protocol model underneath each module*, not the module
  boundaries.

**HARD INVARIANT — no protocol logic in the plugin.** Mirrors CSP §16. Every
sync/merge/identity/auth/history/scope behavior is a call into the SDK. Any
behavioral difference between this plugin and the `ctx` CLI for the same
engine operation is a bug (CSP §16 "one core, thin bindings"; a feature is not
done until reachable from both the CLI and the SDK and covered by CSP §18
tests).

---

## 3. Goals

1. **Notes sync, zero ceremony.** Point the plugin at a peer, authorize the
   device, and the vault stays byte-identical across devices in near real time
   (CSP §2, §14).
2. **Offline-first, inherited not implemented.** The plugin adds no offline
   logic; thin-node offline-first is CSP §7. It reflects engine-reported
   connectivity only.
3. **Desktop + mobile from one bundle.** The same wasm/TS core runs in
   Electron and the mobile WebView; only the host adapters differ (CSP §16).
4. **CLI-interchangeable on disk.** A vault folder is identical whether driven
   by `ctx` or this plugin — same `.context/`, same `.context/config`, same
   device key resolution (CSP §9.1) — structurally removing the config-vs-
   on-disk divergence bug class.
5. **Faithful to CSP.** The plugin never weakens or reinterprets a CSP
   guarantee; it exposes engine operations and engine-reported state.

## 4. Non-goals

- **Never a listener/relay.** CSP §7 HARD INVARIANT: thin nodes never listen.
  The plugin only makes *outbound* connections to a full node. A vault synced
  only between thin nodes (e.g. two phones, no full node) is explicitly
  unsupported and will not converge (CSP §7) — see §8 and §14.
- **No conflict-resolution UI.** CSP resolves deterministically with no human
  step (CSP §3, §12). The plugin may *notify* that a same-region edit was
  superseded and offer recovery (§10); nothing more. No conflict markers ever.
- **No git UI / no `ctx git`.** The repo is engine-owned and `ctx git` is a
  read-only inspection path (CSP §4, §17); it is out of scope for the plugin
  UI. The plugin never writes the repo out of band (CSP §4 — clobbered).
- **No deep PITR locally.** Thin nodes are bounded by a retention horizon (CSP
  §9.2); deep point-in-time recovery is delegated to full nodes (CSP §7, §8).
  Named snapshots remain durable even on thin nodes (CSP §9.2); see §10.
- **Binary attachments are not merged.** Text-only by default; binaries are
  excluded by scope unless explicitly opted in, then whole-file last-writer-
  wins by total order with no chunking (CSP §11). v1 plugin scope is text.
- **The plugin never touches `.context/`.** It never reads, writes, syncs, or
  exposes anything under a vault's `.context/` as note content (CSP §11 HARD
  INVARIANT). Obsidian's content APIs skip dotfolders, which aligns with this.

---

## 5. What changes from the agentsync plugin (the migration delta)

The module boundaries survive; the model under them is replaced. This is the
authoritative list of conceptual substitutions.

| agentsync model | CSP model (`spec.md`) |
|---|---|
| Central **hub**; `rendezvousUrl` | **No server.** Connect to a *full node in listen mode* (CSP §6.1). "Hub URL" → "Peer URL" (the full node's `wss://`). |
| **Vault id minted/owned by the hub**; `probeHub`, pinned-id-vs-hub-vs-doc reconciliation | **No vault id negotiation.** Identity is genesis `M₀` (globally identical, CSP §5.2) + the replicated primitive set. A joining thin node just catches up its frontier (CSP §6.4). The entire `vault_id mismatch` bug class is gone. |
| **Automerge `Doc`** + per-peer `SyncState`; CRDT op merge | **Primitive-commit DAG + deterministic fold** (CSP §5). The plugin (thin) appends signed primitives and the `|F|=1` self-wrapper only; the *full node* computes `main` (CSP §5.3, §7). |
| `loadSyncState`/`saveSyncState` (Automerge sync state) | **Frontier-set anti-entropy** state (CSP §6.4): the un-merged primitive tip set, not a scalar version vector. |
| `.agentsync/` footprint; `config.toml` | **`.context/`** footprint; **`.context/config`** (CSP §9.1) — same CLI-shared file model. |
| Local-only `ignoreGlobs` in `[obsidian]` | **`.contextignore`** at the scope root, gitignore-syntax, **synced** (CSP §11); plus optional node-local `.context/exclude` (never synced). |
| `~/.agentsync/id_ed25519`; in-vault `identity.seed` | **`~/.context/id_ed25519`** (device-global, desktop) or per-vault opt-in (mobile), OpenSSH ed25519, never synced (CSP §9.1/§10). |
| Hub `authorized_keys` (a remote concept) | **Node-local `.context/authorized_keys` on the *full node*** (CSP §10) — never synced; the plugin's job is to get its device pubkey authorized *there*. |
| `createLabel`/`restoreToLabel`/`restoreToTime` | **`ctx snapshot`** / **`ctx restore <name\|time>`** (CSP §8); snapshot = frontier primitive-SHA set + label, replicated as a small record. |
| TOFU-pinned `hubPubkey` (`[vault] hub_pubkey`) | **Peer/listener key pinning** from mutual auth (CSP §10) — the connecting node also verifies the listener's key; pin it the same way. |
| Plugin-side `planReconcile` bidirectional planner | **Engine-side** §5.6 reconcile + §6.4 catch-up. The plugin provides host FS access; it does not plan convergence (§9). |

Net simplifications worth stating: no hub, no server-minted id, no id
negotiation, no Automerge sync-state files, no `probeHub`. Net new
obligations: honor CSP §5.6 (materialize-by-content-hash, atomic writes,
contended-path no-clobber), CSP §11 (synced `.contextignore`, never touch
`.context/`), CSP §7 (thin-only meshes don't converge — steer the UX).

---

## 6. Node tier & engine model

- **Thin node (CSP §7), always.** Offline-first: holds a full local working
  copy, reads/writes offline, appends its own **signed primitive commits**
  (CSP §5.1/§5.2). Between reconnects it advances its own spine with the
  trivial `|F|=1` synthetic-fold-commit wrapper over its own latest tip (CSP
  §7 self-collapse, §5.3 degenerate case) — **not a 3-way merge** (wasm-safe),
  keeping an offline editing run linear (O(1) frontier contribution, CSP §14).
- **Never the merge authority.** On reconnect it runs frontier-set anti-
  entropy catch-up (CSP §6.4) and receives the merged tree served by a
  connected full node (CSP §6.5/§7). It recomputes no multi-tip `main`.
- **Deployment requires ≥1 full node (CSP §7).** This plugin alone (or a mesh
  of only thin nodes — e.g. phone ↔ laptop-plugin with no full node) will not
  converge concurrent edits, by CSP design. The setup UX therefore makes
  *connect to a full node* the primary, recommended path (§8).
- **Retention horizon (CSP §9.2).** Recent history + working state are kept
  locally; deeper history is pruned and fetched from a full node on demand.
  This is what lets a phone be a real offline-first node without unbounded
  growth. Named snapshots are never truncated (CSP §9.2) — see §10.
- **wasm surface is the reduced one (CSP §4).** Object encode/decode, sync
  state machine, auth, framing — no diff/3-way merge, no on-disk odb/
  packfiles. Validating that this reduced surface compiles and runs under
  `wasm32` in a mobile WebView is a CSP residual gate (CSP §13.2) this plugin
  directly depends on (§14).

---

## 7. Module architecture

Each module keeps its agentsync-era responsibility; the CSP role is stated.

### 7.1 Plugin entry & lifecycle (`main.ts`)

Owns the controller's lifecycle, the settings tab, status bar, commands, and
the Obsidian `create/modify/delete/rename` listeners that drive the
Obsidian → SDK push path. The unconfigured → configured gate is retained:
`.context/config` may exist before setup completes, but the vault counts as
configured only once setup works end to end (connect-mode: the full-node
handshake + first catch-up succeeded — the exact step where an unauthorized
device key fails). Deleting `.context/` returns the plugin to unconfigured;
nothing is silently regenerated. Identity is owned by the plugin and handed to
the SDK; the SDK never auto-generates or persists a key (CSP §10).

### 7.2 Sync controller (`sync-controller.ts`)

Owns the SDK vault-session lifecycle and a UI-facing state machine
`idle → connecting → connected → reconnecting → error`. The state is a
**projection of engine-reported connectivity**, not a protocol state machine
(that is the SDK's). On disconnect the controller simply lets the SDK
reconnect and re-run catch-up — **there is no separate resync path** (CSP
§6.5). The "Resync now" command therefore triggers reconnect + catch-up (CSP
§6.4), not a bespoke bidirectional plan. `reset local state` wipes the local
object store + sync state but keeps `.context/config` and the device key;
because it then re-catches-up under the same NodeId, it MUST fork a fresh
NodeId or warn (CSP §5.1 — the counter is durably persisted; never silently
resume authoring under a possibly-live key).

### 7.3 Obsidian host bridge (`bridge.ts`)

Two-way adapter between Obsidian's `app.vault` and the SDK. **It is host I/O,
not the no-feedback-loop algorithm.** CSP §5.6 specifies the
materialize-vs-user-edit reconcile (compare each in-scope path's current
content hash to the last-materialized hash; equal → self-write, ignore;
different → genuine edit, commit; atomic temp+rename writes; defer a contended
path rather than clobber). The plugin's obligation is to *provide the host
primitives that algorithm needs* — atomic writes through Obsidian's adapter,
content read, and faithful create/modify/delete/rename events — and let the
engine own the reconcile. The agentsync-era suppression set + content-equality
short-circuit is retained as defense-in-depth, but correctness rests on CSP
§5.6's content-hash reconcile, which is race-tolerant by construction. Burst
coalescing of remote-apply events (debounce) is retained for renderer health.

### 7.4 Storage adapter (`storage-adapter.ts`)

A CSP host-storage backend over Obsidian's `app.vault.adapter`, identical on
desktop and mobile. CSP §9.1 explicitly permits a host (browser/mobile thin
node) to override `.context/` to host-provided storage — this adapter is that
override. It persists the thin-node object subset (objects backing current
`main` + history within the retention horizon, CSP §9.2), `.context/state`
(last-materialized hashes §5.6, the durable logical counter §5.1, the frontier
§6.4), and `.context/config`. Writes are atomic (temp + rename). `.context/`
is a dotfolder Obsidian's content APIs skip, so plugin state never round-trips
through sync (CSP §11 HARD INVARIANT). `.context/authorized_keys` is **not**
relevant on a thin node (it never listens); it is the *full node's* local file
(CSP §10) and is never synced here.

### 7.5 Identity store (`identity-store.ts`)

Resolves the device keypair and hands it to the SDK (CSP §10 — node identity
is an SSH key, OpenSSH ed25519 format; NodeId derived from it). Location
mirrors `ctx`:

- **Desktop:** `~/.context/id_ed25519` (+ `.pub`), the *same file `ctx` uses*
  — one device, one key across every vault it joins; survives deleting a
  vault's `.context/` (CSP §9.1/§10). Reusing an existing `~/.ssh` key or an
  SSH agent is permitted (CSP §10) and surfaced in settings.
- **Mobile:** no home dir → per-vault key under the host storage, recorded as
  the identity reference in `.context/config`. This is the per-vault opt-in
  CSP §10 describes (stronger isolation, key-sprawl cost). The private key is
  **never synced** (CSP §10/§11); the in-vault location must be inside the
  excluded `.context/`, never the synced scope.

The plugin never silently regenerates a key; a missing key on a configured
vault is surfaced, not papered over.

### 7.6 Settings (`settings.ts`)

Settings *are* `.context/config` — the same file `ctx` reads/writes (CSP
§9.1), so a folder is interchangeable between CLI and plugin. Schema-defined
keys are CLI-shared; plugin-only knobs live in a namespace the CLI ignores
(the agentsync `[obsidian]`-table pattern), preserved losslessly on round-
trip. This aligns with CSP §17.1's three-way knob parity (flag / `CTX_*` env /
config key): the plugin is one more front-end, exactly parallel to a flag. The
`.context/config` on-disk format is defined by CSP, not this plugin; the
plugin is a consumer of it (§14 residual gate).

### 7.7 Settings tab (`settings-tab.ts`)

Two states, recast for CSP:

- **Unconfigured → setup wizard.** The only path that creates `.context/`.
  Two modes:
  - *Connect to a peer* (primary, recommended): paste the full node's
    `wss://`/`ws://` connect address, run the engine clone + watch (CSP §17
    `ctx clone <url>` then the watch loop). Completion is gated on the full-
    node handshake + first catch-up (CSP §6.4). Per CSP §5.1 the clone path
    MUST fork a fresh NodeId or warn — surface that verbatim.
  - *Create a local vault*: engine `init` (CSP §17 `ctx init`) here. The
    wizard MUST state plainly that a vault with no full node will not converge
    across devices (CSP §7) — creating here is only useful once a full node
    joins it. No silent thin-only-mesh promise.
- **Configured → normal settings.** Enable-sync master switch; device public
  key (OpenSSH, copyable — paste into the full node's `authorized_keys` via
  `ctx authorize`, CSP §10); Peer URL; synced `.contextignore` editor + node-
  local `.context/exclude`; pinned peer key (from mutual auth, CSP §10);
  connection state + Reconnect/Resync (= catch-up, CSP §6.4/§6.5); reset local
  state (with the CSP §5.1 fresh-NodeId warning); snapshots list +
  create/restore (CSP §8).

### 7.8 Path filter (`path-filter.ts`)

Maps to CSP §11 scope: an **explicit allowlist** (text extensions by default
— never "everything minus a denylist", so the failure mode is syncing too
little, never exfiltrating), minus the synced `.contextignore` and the node-
local `.context/exclude`, and unconditionally minus `.context/` itself (CSP
§11 HARD INVARIANT). Binaries are excluded unless explicitly opted into scope,
then whole-file LWW by total order, no chunking (CSP §11) — out of v1 plugin
scope. Glob semantics follow gitignore (CSP §11), replacing the agentsync
plugin's minimal in-house globber.

### 7.9 Catch-up / convergence (`reconcile.ts`)

The agentsync plugin-side bidirectional planner is **subsumed by the engine**.
First attach and every reconnect run CSP §6.4 frontier-set anti-entropy
(advertise frontier digest, request missing tips, pull reachable closure,
recompute) and CSP §5.6 materialize. The plugin only adapts Obsidian's vault
to the SDK's host-filesystem surface (list/read/write/hash) and renders
progress. The boundary "what the engine does vs. what the plugin provides"
must be confirmed against the published SDK surface (§14).

### 7.10 Status bar (`status-bar.ts`)

Unchanged in shape: a projection of the controller's UI state (idle /
connecting / connected / reconnecting / error). It visualizes engine-reported
connectivity (CSP §6.5); it owns no state.

---

## 8. Setup & connection flows

- **Connect to a full node (primary).** The user obtains a full node's connect
  address (a `ctx watch --listen` host, or Context Desktop's per-folder
  listener, `desktop-app-spec.md` §8). They paste it; the plugin runs the
  clone + watch flow (CSP §17 `ctx clone <url>`, then the continuous watch
  loop). The flow is one continuous operation, not two manual steps.
- **Device authorization (CSP §10).** Authorization is **per primitive author,
  on the full node**, not per connection (CSP §6.1/§10). The plugin surfaces
  the device's OpenSSH public key with a Copy action; the full-node operator
  adds it via `ctx authorize <pubkey>` (or `CTX_AUTHORIZED_KEYS` / `ctx init`
  seeding). Until the device key is in the full node's node-local
  `authorized_keys`, primitives are dropped (CSP §6.1/§6.3) and the handshake/
  catch-up fails — this is the expected, legible failure the wizard reports.
- **The plugin is the connector, never the listener.** It does **not** open a
  TOFU window — TOFU is the *listening* node's behavior on an empty authorized
  set (CSP §10), and thin nodes never listen (CSP §7). The plugin instead
  performs mutual auth, verifies the full node's key, and pins it (CSP §10
  "a connecting node also verifies the listener's key, enabling key pinning").
- **Transport confidentiality (CSP §10).** On untrusted networks the full node
  is expected behind a TLS-terminating proxy; the plugin speaks `wss://` to
  it. On a trusted LAN, `ws://` is acceptable (CSP §10). CSP ships no embedded
  CA; the plugin neither provisions nor bundles certificates. Mobile OSes
  suspend sockets when backgrounded — not realtime until resumed, catch-up on
  wake (CSP §14); the plugin reflects this as `reconnecting`, not an error.

---

## 9. Two-way materialization & the no-feedback-loop (CSP §5.6)

This is the single most common file-sync bug class; CSP §5.6 specifies it so
hosts do not reinvent it. The plugin's obligations, and only these:

- **Atomic writes.** Every plugin-applied write goes through Obsidian's
  adapter as temp + rename so readers never see a torn file and the watcher
  sees one event per path (CSP §5.6).
- **Faithful events + content/hash access.** Provide the engine the current
  content (or its hash) for every in-scope path so the §5.6 content-hash
  reconcile can classify self-write (equal → ignore) vs. genuine edit
  (different → commit). The plugin does not decide this; it feeds the engine.
- **Contended-path no-clobber.** Honor CSP §5.6: if a path has a pending user
  edit and the new `main` wants different bytes, the engine defers that path —
  the plugin must not force its own write over the user's bytes. Disjoint
  files materialize normally.
- **`.context/` is never a sync path.** The plugin excludes it
  unconditionally (CSP §11 HARD INVARIANT) and relies on Obsidian skipping
  dotfolders as a second line of defense.

The guarantee is exactly CSP §12's, not stronger: disjoint regions both
survive; a same-region collision is resolved deterministically by the strict
total order, the losing side retained in history and recoverable (§10) — no
silent loss, but not "no edit ever lost." The plugin must not imply otherwise
in its copy.

---

## 10. Snapshots & recovery

- **Create restore point** → `ctx snapshot <name>` (CSP §8/§17): records the
  frontier primitive-commit SHA set + label, replicated as a small record,
  durable across any retention horizon including on this thin node (CSP §9.2).
- **Restore…** → `ctx restore <name|time>` (CSP §8/§17). Named snapshots are
  primary — exact and skew-free (CSP §8). Time-based restore is secondary and
  labeled **best-effort / approximate under clock skew**, carrying CSP §8's
  warning verbatim, and is additionally **bounded by the thin-node retention
  horizon** — deep time restore is delegated to a full node (CSP §7, §9.2).
- Restore is the engine's restore-as-edit (CSP §8): the plugin issues the call
  and reflects convergence; it adds no rewind logic and shows no merge UI.
- Same-region supersession (CSP §12) may surface as a passive, dismissible
  notification linking to recovery — informational only; the engine already
  resolved it deterministically.

---

## 11. Security posture (inherits CSP §10, no weakening)

- Mutual auth, per-author signature + node-local authorization, content
  integrity, and the "SHA-1 is not CSP's security boundary" reasoning are
  CSP's (§4, §10); the plugin changes none of it. A relay confers no trust;
  trust does not propagate transitively (CSP §6.1/§10).
- The plugin never transmits, materializes from a peer, or stores anything
  under `.context/` (CSP §11 HARD INVARIANT). It is a controller and host I/O
  layer, not a data path for engine state.
- Private key handling is delegated (file path or SSH agent, CSP §10). The
  plugin hands the identity to the SDK; it never logs private key bytes and
  never copies the key into a synced location (CSP §10/§11).
- The plugin is outbound-only and never binds a listener (CSP §7 HARD
  INVARIANT), which removes the entire TOFU-exposure surface from this host;
  that caveat (CSP §13.2) lives with the full node it connects to.

---

## 12. Mapping: plugin action → CSP operation

| Plugin action | CSP operation (`spec.md`) |
|---|---|
| Setup → Connect to a peer | `ctx clone <url>` then watch loop — §17/§6 (fresh NodeId / warn, §5.1) |
| Setup → Create a local vault | `ctx init` — §17 (with the "needs a full node to converge" caveat, §7) |
| Enable sync (on) | open vault + watch loop — §6, §17 `ctx watch` |
| Enable sync (off) | stop watch loop, close peer — §5/§6 |
| Reconnect / Resync now | reconnect + frontier-set catch-up — §6.4/§6.5 (no separate resync path) |
| Copy device public key | `ctx key` — §17/§10 |
| (operator authorizes it) | `ctx authorize <pubkey>` on the *full node* — §10 |
| Pinned peer key | listener-key verification from mutual auth — §10 |
| Edit a note in Obsidian | debounced signed primitive commit + `|F|=1` self-wrapper — §5.1/§5.2/§7 |
| Receive remote change | catch-up + materialize the full node's merged tree — §6.4/§6.5/§5.6 |
| Edit `.contextignore` | synced scope-exclusion change — §11 |
| Create restore point | `ctx snapshot <name>` — §8/§17 |
| Restore… | `ctx restore <name\|time>` — §8/§17 (time = best-effort, horizon-bounded) |
| Reset local state | wipe local object/sync state, re-catch-up — §6.4, with §5.1 fresh-NodeId warning |
| Connection state row | `ctx status`-equivalent, read-only — §17 |

The plugin issues these and renders engine-reported results. It computes no
fold, chooses no merge base, orders no commits — all CSP §5 invariants live in
`csp-core` (CSP §16).

---

## 13. Testing, verification & cross-surface parity

Inherits CSP §18. Specifically for this plugin:

- **Unit tests (pure plugin modules).** Path filter / scope + `.contextignore`
  glob behavior, `.context/config` ⇄ settings round-trip (lossless w.r.t. CLI-
  written keys), identity resolution seam, the host-bridge event/suppression
  logic, status projection. No protocol logic to unit-test here — it isn't in
  the plugin.
- **Determinism/conformance is NOT this plugin's gate.** CSP §5.4/§13.2/§18
  determinism is the engine's; the plugin must not host a second merge
  implementation (CSP §7/§16) and has nothing to assert about fold SHAs beyond
  "it converged with a full node."
- **Cross-surface interop (CSP §18).** A vault driven by this wasm/TS thin
  node must interoperate with a native full node — handshake, replication,
  identical convergence and SHAs after catch-up. This is the guarantee that
  the plugin is bindings over the same core, not a divergent reimplementation.
- **E2E roundtrip.** Spawn a real full node (listener) + this plugin against a
  test Obsidian vault; exercise create/modify/delete/rename, disjoint and
  overlapping concurrent edits, offline → reconnect catch-up, snapshot/restore;
  assert the working tree converges byte-identically and that `.context/` is
  never synced. Mirrors the agentsync plugin's `plugin-roundtrip` e2e against
  the new model.
- **Parity requirement (CSP §16/§18).** Any capability reachable from `ctx`
  that the protocol exposes must be reachable from the plugin (or explicitly,
  documentedly out of scope for a thin node, e.g. listen/relay, deep PITR).

---

## 14. Residual gates & open questions (decide before/at release)

- **gitoxide-wasm spike (CSP §13.2) — load-bearing for this plugin.** CSP
  rates this "low-stakes but still required" *because the merge never runs in
  wasm*. This plugin **is** that wasm thin node: the reduced surface (object
  encode/decode, sync state machine, auth, framing — no merge, no odb/
  packfiles) must compile and run under `wasm32` in a desktop Electron
  renderer **and** a mobile WebView before the plugin build is committed. This
  is the single highest-leverage dependency.
- **Published CSP TS SDK surface.** This plugin is "thin bindings over the SDK"
  (CSP §16), but `spec.md` defines the SDK only at the architecture level. The
  exact TypeScript surface (vault session lifecycle, file ops, host adapter
  interfaces, event stream, the engine/host boundary for §5.6 and §6.4) is a
  prerequisite dependency — analogous to `desktop-app-spec.md` §13's engine-
  embedding gate, on the wasm/TS side. Confirm with the CSP SDK design before
  plugin build.
- **`.context/config` format.** The plugin shares this file with `ctx` (§7.6);
  its on-disk format/parser is CSP's to define. The lossless "preserve CLI-
  written keys, plugin-only keys in an ignored namespace" round-trip depends
  on that format being specified and stable.
- **Thin-only meshes don't converge (CSP §7) — UX, not code.** Two devices
  running only this plugin will not merge concurrent edits. The setup wizard
  must steer hard toward a full node and never imply otherwise. Decide the
  exact copy and whether "Create a local vault" is even offered in v1.
- **Mobile retention horizon vs. snapshot durability.** A phone's bounded
  horizon (CSP §9.2) plus socket suspension (CSP §14) must still keep named
  snapshots exact and durable (CSP §9.2). Validate that a backgrounded phone
  that misses history still restores named snapshots correctly after wake +
  catch-up.
- **NodeId hygiene on reset/clone (CSP §5.1).** "Reset local state" and clone
  re-catch-up under a key that may be live elsewhere. The fork-fresh-NodeId-
  or-warn behavior (CSP §5.1) must be wired and surfaced, not silently
  resumed — confirm the SDK exposes the hook.
- **Identity location on mobile.** The per-vault key lives inside the host
  storage but must be provably inside the excluded `.context/` and never in
  the synced scope or replicated (CSP §10/§11). Verify the exclusion holds for
  the mobile storage adapter specifically.
