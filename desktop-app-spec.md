# Context Desktop — Design Specification

> Status: draft / greenfield design. Nothing here is implemented yet.
> Companion to `spec.md` (Context Sync Protocol / CSP). This document specifies
> a desktop application; it specifies **no protocol behavior** — all sync,
> merge, identity, and history semantics are defined by `spec.md` and are
> reused, never reimplemented.
> Working app name: **Context Desktop** (final name TBD).

---

## 1. Summary

Context Desktop is a **normal desktop application** — a regular resizable
application window, in the model of OrbStack or Docker Desktop — that also
installs a menu-bar (tray) icon. The tray icon is a quick-access **menu and
status indicator only**; it does **not** present a popover/panel that drops
down under the menu bar. Closing the window leaves the app running in the
background (tray-only is a valid steady state); the window is reopened from
the tray or the dock/taskbar.

The app lets a user designate one or more folders to be kept byte-identical
across their devices using the Context Sync Protocol (CSP, `spec.md`). A
folder is either created/attached **locally** or **cloned from a remote
peer** and then watched continuously — both are first-class v1 flows. Each
folder is one CSP **vault**. The app runs continuously in the background as a
**full node** (CSP §7): it watches enabled folders, auto-commits, replicates,
and computes the deterministic merge — entirely via the engine, with no
app-side protocol logic.

The user can: add a new local folder or **clone + watch a remote peer's
folder**, remove folders, toggle sync per folder, let another node connect to
a folder (per-folder listener with a shown `wss://` address and firewall
guidance), reveal a folder in the OS file manager, and manage identity (SSH
key) and global settings.

---

## 2. Architecture decision: Tauri, linking `csp-core` directly

- **Tauri v2.** Rust backend, web frontend (the UI runs in Tauri's webview).
  The frontend stack is fixed, not load-bearing on the protocol but pinned for
  consistency: **React + TypeScript**, **Vite** (dev server + build), **Bun**
  (package manager and JS runtime/test runner), **Tailwind CSS** +
  **shadcn/ui** (components), **React Router** (in-app navigation between the
  Folders and Settings views), and **Biome** for linting and formatting (in
  place of ESLint/Prettier). The webview hosts a **normal application window**
  (resizable, with dock/taskbar presence), not a menu-bar popover — see §1
  and §6.
- **The backend links the native `csp-core` crate directly** at the
  **full-node / native profile** (CSP §16: merge engine + on-disk
  odb/packfiles compiled *in*). The app is architecturally a sibling of the
  `ctx` CLI — "a thin wrapper over `csp-core`" — **not** a consumer of the
  wasm/TS SDK. No `ctx` subprocess, no FFI shim, no wasm.
- **Cross-platform by construction.** macOS is the v1 target; the same binary
  builds for Windows/Linux (the tray, dialog, and reveal-in-folder surfaces are
  the only platform-touching code and all have Tauri support). macOS-specific
  polish is allowed but must not fork the architecture.

**HARD INVARIANT — no protocol logic in the app.** Mirrors CSP §16's
"one core, thin bindings." Every sync/merge/identity/auth/history behavior is a
call into `csp-core`. The app contributes: process lifecycle, the watcher host
glue, the listen socket host glue, UI, and its *own* small app-level config
(§7). Any behavioral difference between Context Desktop and the `ctx` CLI for
the same engine operation is a bug.

---

## 3. Goals

1. **Zero-friction folder sync.** Add a local folder or clone a remote one —
   it syncs. Toggle off, it stops.
2. **Always-on, invisible.** Lives in the menu bar; no window required to keep
   syncing. Survives login (optional autostart).
3. **One node, many vaults.** A single app process hosts N independent
   `csp-core` engine instances, one per enabled folder (the in-process
   equivalent of running one `ctx watch` per folder).
4. **Let peers connect, safely and legibly.** Per-folder listen mode with a
   copyable address, honest firewall guidance, and a native prompt for the
   CSP trust-on-first-use (TOFU) window (CSP §10).
5. **Legible identity.** Surface the device SSH key (CSP §10) — view, copy,
   reuse an existing `~/.ssh` key or agent, opt into a per-vault key.
6. **Faithful to CSP.** The app never weakens or reinterprets a CSP guarantee;
   it only exposes engine operations and engine-reported state.

## 4. Non-goals

- **Not a git client / not a VCS UI.** No branching, diffs-as-workflow, or
  manual commits. (CSP §3.) `ctx git`-style read-only inspection is out of
  scope for v1 UI.
- **No conflict-resolution UI.** CSP resolves deterministically with no human
  step (CSP §3, §12); there is nothing to ask the user. The app may *notify*
  that a same-region edit was superseded and offer recovery (§9), nothing more.
- **No protocol multiplexing.** One listening folder = one listen socket = one
  port (the literal mapping of `ctx watch --listen` per vault). Serving many
  vaults over one port would require a CSP protocol change and is out of scope.
- **No embedded certificate authority / no built-in public-internet TLS.**
  Matches CSP §10. The app guides toward LAN/VPN; public exposure is
  discouraged and carries CSP's TOFU caveat (CSP §13.2).
- **Not a mobile app.** Thin-node / mobile clients are the SDK + host-plugin
  path in CSP §16, not this app.
- **No editing of vault internals.** The app never reads, writes, exposes, or
  syncs anything under a vault's `.context/` (CSP §11 HARD INVARIANT). It
  surfaces engine-reported state only.

---

## 5. Process & engine model

- **One background process**, started at login (optional) or on app launch.
  It owns a **normal application window** (à la OrbStack/Docker Desktop) plus
  a tray icon. The window can be closed while the process keeps syncing
  (tray-only is a valid steady state) and is reopened from the tray, the dock/
  taskbar, or by re-activating the app. The tray icon never spawns a popover
  panel under the menu bar (§6.1).
- **Per enabled folder: one `csp-core` engine instance** running the CSP
  watch/sync loop (CSP §6, §17 `ctx watch`). Disabling a folder tears its
  instance down cleanly (stop watcher, close peers, flush state); re-enabling
  re-opens it and runs normal catch-up (CSP §6.4) — there is no app-specific
  resync path.
- **Per folder with "allow connections" on: additionally a listen socket**
  bound to that folder's port (the in-process equivalent of
  `ctx watch --listen`; CSP §6.1 — only full nodes may listen, which this app
  always is).
- **Offline-first is inherited, not implemented.** The app adds no offline
  logic; `csp-core` is offline-first per CSP §7. The app only reflects
  engine-reported connectivity.
- **Graceful shutdown** stops every watch loop and closes every listener
  before exit; the engine's durable state (`.context/state`, CSP §5.1/§5.6)
  carries across restarts unchanged.

---

## 6. UI surfaces

### 6.1 Menu-bar (tray) icon & menu

The tray icon is a **native OS menu + status indicator**, not a window
surface: clicking it opens the OS context menu in place; it never renders an
in-app popover/panel under the menu bar. All real UI lives in the main
application window (§6.2/§6.3), opened via "Open main window", the dock/
taskbar, or re-activating the app — the OrbStack/Docker Desktop model.

- **Icon reflects aggregate state**, derived only from engine status across all
  enabled folders: *idle/synced*, *syncing*, *offline* (no peers), *attention*
  (auth/listen/error needing the user). No new state machine — a projection of
  per-vault `ctx status`-equivalent data.
- **Tray menu (native OS menu):**
  - Per enabled folder: name, status glyph, last-synced/`main` short SHA,
    submenu → Open in file manager · Toggle sync · Allow connections · Copy
    connect address · Settings.
  - Global: Open main window · Add folder… · Connect to remote folder… ·
    Pause all / Resume all · Quit.

### 6.2 Main window — Folders

A list; each row is one vault:

- Path + display name; engine status (peers connected, `main` short SHA, last
  activity) — all from the engine, read-only.
- **Enable/disable sync toggle** → starts/stops that folder's engine instance
  (§5).
- **Allow connections toggle** → starts/stops that folder's listener; when on,
  shows the connect block (§8).
- **Open in file manager** button → Tauri reveal (`revealItemInDir`; Finder /
  Explorer / xdg).
- **Authorized peers** disclosure: list of keys in that vault's
  `.context/authorized_keys` with fingerprints; **Authorize…** and **Revoke**
  actions (map exactly to `ctx authorize` / `ctx revoke`, CSP §10). Pending
  TOFU connections surface here and via §8.3.
- **Remove folder** → stops + detaches the folder from the app. Honest
  confirm copy: this stops syncing and removes app tracking; it does **not**
  delete the folder, the working files, or the vault's `.context/` history
  (which remains a valid CSP vault, re-addable later).

**Add folder** offers two first-class v1 entry points (both reachable from the
main window's Add menu and from the tray's "Connect to remote folder…", §6.1):

*Add a local folder* → native directory picker (Tauri dialog), then:
- If the directory already contains a CSP vault → attach and start it.
- Else → engine `init` (CSP §17 `ctx init`): create the scoped vault here.

*Clone + watch a remote folder* → pick a local destination directory, paste
the peer's `wss://` (or `ws://`, §8.1) connect address, authenticate. The app
runs engine `clone` (CSP §17 `ctx clone <url>`) to catch up and materialize
the working tree, then **immediately attaches and starts the normal watch
loop** (§5) so the cloned folder stays synced like any other vault — clone
and watch are one continuous flow, not two steps the user must stitch
together. Per CSP §5.1 the clone path MUST fork a fresh NodeId or warn —
surface that warning verbatim; do not silently resume a possibly-live key.

### 6.3 Main window — Settings

- **Identity (CSP §10).** Show this device's public key in OpenSSH format with
  **Copy**. Choose key source: default device-global `~/.context/id_ed25519`,
  reuse an existing `~/.ssh` key, or use the running SSH agent. Per-vault key
  is an explicit opt-in (CSP §10, stronger isolation, key-sprawl cost) — and
  must be presented as such. The app never copies the private key into a
  vault's `.context/` or anywhere synced.
- **Defaults for new listeners:** port-assignment strategy (§8.1), bind scope
  (loopback vs LAN — default LAN-only-on-explicit-opt-in), TOFU on/off
  (`--no-tofu` equivalent, CSP §10), TLS expectation (`--no-tls` equivalent for
  reverse-proxy deployments, CSP §10).
- **App behavior:** start at login, notifications on/off per category, log
  level (`CTX_LOG` equivalent).

These map to CSP's three-way knob parity (CSP §17.1); the app is one more
front-end and stores its choices in **app config**, not in any `.context/`.

---

## 7. App-level state & where it lives

- The app keeps a small config: tracked folders, per-folder `{enabled,
  allowConnections, port, displayName}`, and global preferences (§6.3).
- **This lives in the OS app-config directory** (e.g. macOS
  `~/Library/Application Support/<bundle id>/`), **never** inside any vault's
  `.context/` (CSP §11 HARD INVARIANT) and never inside a synced scope.
- The device private key remains at its CSP-defined engine-owned location
  (`~/.context/id_ed25519` by default, CSP §9.1/§10). The app references it; it
  does not relocate, duplicate, or sync it.
- App config is local to the device and is **not** synced by CSP (it is not a
  CSP concept). Two devices are independent app installs that happen to share
  vaults.

---

## 8. Letting another node connect (per-folder listener)

### 8.1 Address & port

- A folder with **Allow connections** on binds its own listener (CSP §6.2
  WebSocket). Port: auto-assigned from a configurable range by default,
  user-overridable per folder; persisted in app config (§7) so it is stable
  across restarts (stable port = stable peer config).
- The app detects the device's LAN address and shows the **connect address**:
  `wss://<lan-ip>:<port>` (or `ws://` when TLS is terminated elsewhere /
  trusted LAN — consistent with CSP §10 plaintext-on-trusted-network), with a
  **Copy** button. The peer pastes this into their own Add-folder → Connect
  flow (§6.2) — the `ctx clone <url>` equivalent.

### 8.2 Firewall guidance (honest, OS-accurate)

The naïve "whitelist this port" instruction is wrong on macOS and the spec must
say so:

- **macOS:** the Application Firewall is **per-application, not per-port**. On
  first listen the OS prompts to allow incoming connections *for the app*. The
  app's guidance text: (1) accept that OS prompt; if previously denied, link to
  System Settings → Network → Firewall to allow "Context Desktop"; (2) the port
  itself needs no separate macOS rule. Only a *router/NAT* (for non-LAN reach)
  involves the port — and that path is discouraged (§8.4).
- **Windows/Linux (when those targets ship):** show the actual port and the
  per-OS allow guidance (Windows Defender Firewall inbound rule; `ufw`/`firewalld`).
- The block always shows: the copyable address, the OS-appropriate allow
  guidance, the detected LAN IP, and the explicit recommendation in §8.4.

### 8.3 Trust-on-first-use, surfaced natively

CSP §10's TOFU window (empty `authorized_keys`, first connector trusted) must
not be silent in a GUI:

- When a folder's authorized set is empty and a peer connects, the app raises a
  **native notification + dialog**: peer key fingerprint, folder, **Allow** /
  **Deny**. Allow → `ctx authorize`-equivalent (record into that vault's
  node-local `.context/authorized_keys`, CSP §10/§11 — never synced).
- If the user pre-authorized keys (§6.2 Authorized peers) or TOFU is disabled
  in Settings, the window never opens — exactly CSP §10's
  `CTX_AUTHORIZED_KEYS` / `--no-tofu` semantics, just via UI.
- Default posture: if a listener binds a **non-loopback** address, the app
  defaults to **TOFU-prompt** (never silent-trust) and shows the §8.4 warning —
  aligned with CSP §13.2's "consider auto-disabling TOFU when the listen
  address is non-loopback."

### 8.4 Honest exposure caveat (carried from CSP §13.2)

The connect block, whenever the address is non-loopback, states plainly: an
internet-reachable listener with an empty authorized set + TOFU trusts whoever
connects first. The app **recommends LAN or a private overlay (VPN/Tailscale)**
and **discourages public port-forwarding**; if the user does expose it, it
directs them to pre-seed authorized keys or disable TOFU first. The app does
not provide a "publish to the internet" button.

---

## 9. Surfacing CSP's merge outcome (no resolution UI)

CSP resolves all conflicts deterministically; same-region collisions defrom a
loser that is **retained in history, not in the working tree** (CSP §12, §5.6).
The app:

- Never shows conflict markers or a merge UI (none exist in CSP).
- MAY show a passive, dismissible notification when the engine reports a
  superseded same-region edit, with a link to **recovery** (below). This is
  informational; the resolution already happened in the engine.
- **Recovery (v1, minimal):** expose `ctx snapshot` and `ctx restore
  <name|time>` (CSP §8, §17) as: "Create restore point" and a "Restore…" dialog
  (named snapshots primary — exact and skew-free per CSP §8; time-based
  secondary and labeled best-effort, carrying CSP's clock-skew warning
  verbatim). Restore is the engine's restore-as-edit (CSP §8) — the app issues
  the call and reflects convergence; it adds no rewind logic.

---

## 10. Mapping: UI action → CSP engine operation

| UI action | CSP operation (`spec.md`) |
|---|---|
| Add folder (new) | `ctx init` — §17 |
| Clone + watch a remote folder | `ctx clone <url>` then watch loop — §17/§6 (fresh NodeId / warn, §5.1) |
| Enable sync toggle (on) | open vault + run watch loop — §6, §17 `ctx watch` |
| Enable sync toggle (off) | stop watch loop, close peers — §5 |
| Allow connections (on) | bind listener — `ctx watch --listen` §6.1 |
| Copy connect address | derived `wss://<lan-ip>:<port>` — §6.2/§8 |
| Authorize peer / TOFU Allow | `ctx authorize <pubkey>` — §10 |
| Revoke peer | `ctx revoke <pubkey>` — §10 |
| Show/copy public key | `ctx key` — §17 |
| Status row (peers, SHA) | `ctx status` — §17 (read-only) |
| Create restore point | `ctx snapshot <name>` — §8/§17 |
| Restore… | `ctx restore <name\|time>` — §8/§17 |
| Open in file manager | OS reveal (app-level, not a CSP op) |

The app issues these and renders engine-reported results. It computes no merge,
chooses no base, orders no commits — all CSP §5 invariants live in `csp-core`.

---

## 11. Notifications (native)

Categories, each toggleable (§6.3): new peer connecting / TOFU prompt (§8.3);
peer connected / disconnected; folder went offline > threshold; sync error
needing attention; superseded same-region edit (§9). Notifications never
contain file contents; identity is shown as key fingerprint.

---

## 12. Security posture (inherits CSP §10, no weakening)

- Mutual auth, content integrity, and the SHA-1-not-being-the-security-boundary
  reasoning are CSP's (§10); the app changes none of it.
- The app never transmits, materializes from a peer, or stores anything under
  `.context/` (CSP §11 HARD INVARIANT) — it is a controller, not a data path.
- Private key handling is delegated (file path or SSH agent, CSP §10); the app
  never reads private key bytes into its own state and never logs them.
- Default-deny on exposure: loopback-friendly defaults, explicit opt-in for
  LAN bind, prominent §8.4 caveat, TOFU-prompt (not silent) on non-loopback.

---

## 13. Residual gates & open questions (decide before/at release)

- **Engine embedding contract.** `csp-core` must expose a stable, in-process
  Rust API (open vault, start/stop watch, start/stop listen, authorize/revoke,
  status, snapshot/restore, structured status + event stream) usable by both
  `ctx` and this app. If it is currently CLI-shaped only, defining that library
  surface is a prerequisite — this is the single highest-leverage dependency
  and should be confirmed with the `csp-core` design before app build.
- **Multi-vault in one process.** N concurrent engine instances + N listeners
  in one Tauri backend must be validated for resource use and clean teardown.
  CSP assumes one vault per `ctx watch` process; running many in-process is an
  app concern CSP does not cover — needs an explicit soak test.
- **Per-folder port stability vs. collisions.** Auto-assignment must survive
  reboot, app update, and another process taking the port (re-bind + surface a
  changed address rather than silently fail).
- **TOFU UX vs. headless reality.** The native TOFU prompt assumes a user is
  present. Define behavior when no one is (queue as pending + notify; never
  silent-trust on non-loopback — consistent with CSP §13.2).
- **Recovery UI depth.** v1 ships snapshot + restore (§9). Time-based restore's
  approximate-under-skew nature (CSP §8) must be shown verbatim, not smoothed.
- **macOS firewall first-run.** The OS allow-incoming prompt fires on first
  listen; verify the guidance copy matches the actual modern macOS flow and
  that a prior denial is recoverable from within the app's instructions.
- **Cross-platform parity.** macOS is v1; confirm tray, dialog, and reveal
  behave on Windows/Linux before claiming portability (architecture already
  supports it; only these three surfaces are platform-touching).
