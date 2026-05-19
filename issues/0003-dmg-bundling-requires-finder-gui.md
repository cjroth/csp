# 0003 — `tauri build` DMG step needs a GUI Finder (fails headless / VM)

- **Severity:** Medium (build/CI ergonomics)
- **Status:** Mitigated (build-target split); upstream limitation, not a code bug
- **Component:** build tooling — `desktop/`
- **Found:** 2026-05-19, building the app inside a headless macOS VM

## Summary

`bun run build` (`tauri build`, targets `["dmg", "app"]`) fails at the DMG
stage in any environment without a usable WindowServer/Finder (headless macOS
VM, some CI runners):

```
Running bundle_dmg.sh
failed to bundle project error running bundle_dmg.sh
error: script "tauri" exited with code 1
```

The `.app` is built fine (Tauri bundles it **before** the DMG); only the DMG
"prettifying" step fails, but its non-zero exit fails the whole build.

## Root cause

Tauri's bundler generates and runs
`src-tauri/target/release/bundle/dmg/bundle_dmg.sh` (a fork of `create-dmg`).
Its mandatory "Finder-prettifying AppleScript" step does
`tell application "Finder" …` to position icons / set the window. In a headless
VM Finder never answers Apple events:

```
osascript … tell application "Finder" → AppleEvent timed out (-1712)
```

(`-1712` = Finder not responding; not a TCC `-1743` denial — restarting Finder
does not help, there is no WindowServer for it.) The script prints
`Failed running AppleScript` and `exit 64`; Tauri reports the generic failure.

Tauri 2.11 exposes **no** config flag or env var to skip this step, and there
is no `beforeBundle` hook to patch the generated script. This is an upstream
`create-dmg`/Tauri limitation, not a defect in this codebase.

## Mitigation (applied)

DMG remains the default (`src-tauri/tauri.conf.json` →
`bundle.targets: ["dmg", "app"]`), and per-target scripts were added so a
headless environment can build just the `.app`:

- `desktop/package.json`:
  - `build` → `tauri build` (both, default — for GUI Macs / release)
  - `build:app` → `tauri build --bundles app` (no Finder needed)
  - `build:dmg` → `tauri build --bundles dmg` (GUI Mac only)
- The `ship-context` shell helper (developer's `~/.zshrc`, not in-repo) uses
  `bun run build:app` so the VM→host workflow never hits the DMG step.

## Notes / follow-up

- The produced `.app` is `arm64`, ad-hoc signed (`strip = true`). Moving it
  between machines may require `xattr -dr com.apple.quarantine` on the target;
  for real distribution it needs Developer ID signing + notarization.
- Real `.dmg` artifacts must be built on a GUI macOS host (or CI runner with a
  display) via `bun run build` / `bun run build:dmg`.
- No action required unless we later want headless DMGs — that would mean a
  Finder-free DMG path (plain `hdiutil`, no `create-dmg` AppleScript).
