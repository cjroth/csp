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
