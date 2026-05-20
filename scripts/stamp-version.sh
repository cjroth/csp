#!/usr/bin/env bash
# Stamp one version string across every component's manifest so CI builds carry
# a coherent, non-misleading version. Reads VERSION from the repo root for the
# base; CI passes the final computed string (with -dev/-pr suffix) via $1.
#
# Files patched (all the `version` fields we ship):
#   - Cargo.toml                              (workspace.package.version)
#   - desktop/src-tauri/Cargo.toml            (package.version)
#   - desktop/src-tauri/tauri.conf.json       (.version)
#   - desktop/package.json                    (.version)
#   - sdks/typescript/package.json            (.version)
#   - plugins/obsidian/manifest.json          (.version)
#   - plugins/obsidian/package.json           (.version)

set -euo pipefail

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
  echo "usage: $0 <version>" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

echo "stamping version: $VERSION"

# Workspace Cargo.toml: only the first `version =` line, which is the
# `[workspace.package]` field — every member crate inherits via `version.workspace = true`.
python3 - "$VERSION" <<'PY'
import re, sys, pathlib
v = sys.argv[1]
p = pathlib.Path("Cargo.toml")
text = p.read_text()
text = re.sub(r'(?m)^version\s*=\s*"[^"]*"', f'version = "{v}"', text, count=1)
p.write_text(text)
PY

# desktop/src-tauri/Cargo.toml: standalone crate, not in the workspace.
python3 - "$VERSION" <<'PY'
import re, sys, pathlib
v = sys.argv[1]
p = pathlib.Path("desktop/src-tauri/Cargo.toml")
text = p.read_text()
text = re.sub(r'(?m)^version\s*=\s*"[^"]*"', f'version = "{v}"', text, count=1)
p.write_text(text)
PY

# JSON manifests — targeted regex on the `version` line so the rest of the
# file's formatting (inline arrays, trailing whitespace, etc.) is preserved.
# Each manifest has exactly one top-level "version" key on its own line.
stamp_json() {
  python3 - "$VERSION" "$1" <<'PY'
import re, sys, pathlib
v, path = sys.argv[1], sys.argv[2]
p = pathlib.Path(path)
text = p.read_text()
new, n = re.subn(
    r'("version"\s*:\s*)"[^"]*"',
    lambda m: f'{m.group(1)}"{v}"',
    text,
    count=1,
)
if n != 1:
    raise SystemExit(f"stamp_json: expected 1 version match in {path}, got {n}")
p.write_text(new)
PY
}

stamp_json desktop/src-tauri/tauri.conf.json
stamp_json desktop/package.json
stamp_json sdks/typescript/package.json
stamp_json plugins/obsidian/manifest.json
stamp_json plugins/obsidian/package.json

echo "done."
