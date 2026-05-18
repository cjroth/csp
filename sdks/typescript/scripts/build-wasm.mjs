// Build the real reduced thin-node wasm surface (CSP spec.md §4/§7: object
// encode/decode, identity/auth, wire framing; NO merge, NO on-disk
// odb/packfiles) and emit it into ./pkg, which `src/wasm.ts` loads. This is
// the "one core, thin bindings" wiring (§16) — the SDK is a typed surface
// over csp-core, never a reimplementation.

import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const crateDir = resolve(here, '..', '..', '..', 'crates', 'csp-wasm');
const outDir = resolve(here, '..', 'pkg');

if (!existsSync(resolve(crateDir, 'Cargo.toml'))) {
  console.error('[build-wasm] crates/csp-wasm not found');
  process.exit(1);
}

const res = spawnSync(
  'wasm-pack',
  ['build', '--dev', '--target', 'nodejs', '--out-dir', outDir],
  { cwd: crateDir, stdio: 'inherit' },
);

if (res.status !== 0) {
  console.error('[build-wasm] wasm-pack failed. Install: https://drager.github.io/wasm-pack/');
  process.exit(res.status ?? 1);
}
console.log(`[build-wasm] reduced wasm surface built into ${outDir}`);
