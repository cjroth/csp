# 0002 — Identity / config errors `abort()` with no user-facing message

- **Severity:** High (UX / supportability)
- **Status:** Open
- **Component:** `desktop/src-tauri` (`context-desktop`)
- **Found:** 2026-05-19, debugging first-run crash on host macOS

## Summary

Every recoverable startup error (identity file unreadable, app-config dir
unresolvable, engine init failure) is turned into a hard Rust panic via
`.expect()` inside the Tauri `setup` closure. Because that closure runs inside
AppKit's `applicationDidFinishLaunching` (a non-unwinding Obj-C frame), the
panic becomes a `SIGABRT`. The user sees the app bounce and vanish; the **only**
diagnostic is a macOS `.ips` crash report, which does **not** contain the panic
message. There is no dialog, no log, no actionable text.

## Reproduction

Trigger any engine-init failure (e.g. issue
[0001](0001-engine-identity-loader-not-ctx-compatible.md): a non-hex
`~/.context/id_ed25519`), then launch the bundled `.app` normally.

- GUI launch: app fails to start, only a crash report in
  `~/Library/Logs/DiagnosticReports/`.
- Crash report shows `EXC_CRASH (SIGABRT)`, `abort() called`, faulting in
  `__CFNOTIFICATIONCENTER_IS_CALLING_OUT_TO_AN_OBSERVER__` →
  `NSApplicationDidFinishLaunching` — **no panic string**.
- Only running the inner binary from a terminal reveals the cause:

  ```
  thread 'main' panicked at src/lib.rs:34:18:
  initialise csp engine: "Io: identity hex: Odd number of digits"
  thread 'main' panicked at .../core/src/panicking.rs:225:5:
  panic in a function that cannot unwind
  thread caused non-unwinding panic. aborting.
  ```

## Root cause

`desktop/src-tauri/src/lib.rs`, inside `.setup(|app| { … })`:

- `:29-32` `app.path().app_config_dir().expect("resolve app config dir")`
- `:33-34` `tauri::async_runtime::block_on(AppState::new(cfg_dir)).expect("initialise csp engine")`
- `:90-91` `.build(tauri::generate_context!()).expect("error while building Context Desktop")`

`AppState::new` (`desktop/src-tauri/src/state.rs:17-22`) already returns
`Result<Self, String>` — the error is available and is being **discarded** by
`.expect()`. The panic unwinds into the AppKit `didFinishLaunching` notification
callout, which is `extern "C"` / non-unwinding ⇒ `abort()`, so even the panic
hook's stderr is the only place the message appears.

## Impact

- A desktop app that crashes with zero on-screen explanation for a routine,
  recoverable condition (wrong/old identity file).
- Effectively undebuggable for a normal user; required attaching a terminal to
  the inner Mach-O to recover a one-line message.
- Masks the real defects behind it (e.g. [0001](0001-engine-identity-loader-not-ctx-compatible.md)).

## Proposed fix

Fail **gracefully and visibly** for recoverable startup errors:

1. In `setup`, replace `.expect()` on `app_config_dir()` / `AppState::new()`
   with error handling that, on failure, shows a native error dialog
   (`tauri-plugin-dialog` is already a dependency) with the concrete message
   (e.g. *"Couldn't load your identity at `~/.context/id_ed25519`: <reason>.
   …"*), then exits non-zero cleanly — no panic across the Obj-C frame.
2. Keep `.expect()` only for truly-unrecoverable invariants (e.g. embedded
   `generate_context!()`), and even then prefer a logged, explained exit.
3. Install a panic hook early that writes the message to the unified log /
   `~/Library/Logs` so GUI-launch failures leave a breadcrumb that is *not*
   just the symbol-stripped `.ips`.

## References

- `desktop/src-tauri/src/lib.rs:18-91`
- `desktop/src-tauri/src/state.rs:13-23`
- Trigger example: [0001](0001-engine-identity-loader-not-ctx-compatible.md)
