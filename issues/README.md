# Issues

Tracked defects / decisions found while debugging the desktop build & first-run
on macOS (2026-05-19). One file per issue.

| # | Title | Severity | Status |
|---|-------|----------|--------|
| [0001](0001-engine-identity-loader-not-ctx-compatible.md) | Desktop engine identity loader is not `ctx`-compatible (only accepts bare hex) | High | Open |
| [0002](0002-identity-and-config-errors-abort-silently.md) | Identity / config errors `abort()` with no user-facing message | High | Open |
| [0003](0003-dmg-bundling-requires-finder-gui.md) | `tauri build` DMG step needs a GUI Finder (fails headless/VM) | Medium | Mitigated |
| [0004](0004-wss-dial-requires-explicit-port.md) | `wss://` clone fails when the URL omits the port (no scheme-default) | High | Fixed |
| [0005](0005-ssh-agent-signing-not-wired-to-engine.md) | SSH-agent signing is built & tested but not wired to the engine (works for `ctx key` only) | Medium | Open |
| [0006](0006-vault-level-lock-for-concurrent-sync.md) | Vault-level lock to prevent concurrent sync by multiple processes | Medium | Open |
| [0007](0007-per-session-instance-id-for-concurrent-same-nodeid-syncs.md) | Concurrent same-NodeId sync sessions conflate at the relay; need an in-memory per-session instance ID | Medium | Open |
| [0008](0008-auth-key-enrollment-and-expiry.md) | Auth-key enrollment (`CTX_AUTH_KEY`) + per-key expiry (`expires=YYYY-MM-DD`, default 90d) | Feature | Implementing |
| [0009](0009-incremental-commits-for-obsidian-sync.md) | Obsidian sync is O(whole-vault) per edit; make commits incremental | High | Open |
| [0010](0010-move-engine-to-web-worker.md) | Obsidian sync freezes the UI; move the engine to a Web Worker | High | Open |
| [0011](0011-incremental-persistence-replace-to-bytes.md) | `engine.to_bytes()` re-serializes the whole vault on every commit | Medium | Open |
| [0012](0012-deleted-files-resurrect-when-stale-device-reconnects.md) | Deleted files resurrect when a stale device reconnects (false-add primitive + empty merge base) | High | Open |
| [0013](0013-git-repo-coupling-and-joint-history.md) | Couple a vault to a sibling git repo (joint clone, fast-forward, rewind) | Feature | Open |
| [0014](0014-ghost-add-guard-and-explicit-bootstrap-mode.md) | Ghost-add guard + explicit bootstrap mode (alternative fix for 0012 resurrection) | High | Open |
