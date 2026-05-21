// Build the **one Rust engine** (`crates/csp-wasm` → `csp-core`) to wasm —
// the FULL engine, merge included (§4/§7/§16, one engine everywhere). Two
// wasm-pack `--release` targets, same byte-identical core:
//   • nodejs → ./pkg      (Node/Bun: SDK tests, the §18 ctx-parity e2e,
//                           Obsidian desktop/Electron)
//   • web    → ./pkg-web  (browser/WebView: the Obsidian mobile bundle —
//                           esbuild inlines pkg-web/csp_wasm_bg.wasm)
// These wasm-pack builds run under a SIZE profile — `opt-level=z` +
// `panic=abort` — set via CARGO_PROFILE_RELEASE_* **only for this
// subprocess**, so the workspace `[profile.release]` (and the native `ctx`
// binary / `cargo test`) keep `opt-level=3` unchanged. Measured: the wasm
// goes 674 KB → 505 KB (−25%); the engine-speed cost is immaterial for an
// occasional-sync plugin. The .wasm is then `wasm-opt -Oz`'d when binaryen
// is on PATH. None of this changes observable behavior — the SDK interop
// (byte-identity vs test-vectors.json) + ctx-parity (bidirectional
// convergence vs the real `ctx`) suites prove the size-optimized wasm stays
// byte-identical to native. (gzip-inlining the wasm was evaluated and
// rejected: a CDN already compresses the download, the runtime path is a
// local file with no transport, and a runtime inflate would add an iOS
// <16.4 DecompressionStream gap for a mostly on-disk-only win.)

import { spawnSync } from 'node:child_process';
import { existsSync, statSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const crateDir = resolve(here, '..', '..', '..', 'crates', 'csp-wasm');
const root = resolve(here, '..');

if (!existsSync(resolve(crateDir, 'Cargo.toml'))) {
  console.error('[build-wasm] crates/csp-wasm not found');
  process.exit(1);
}

const hasWasmOpt =
  spawnSync('wasm-opt', ['--version'], { stdio: 'ignore' }).status === 0;
if (!hasWasmOpt) {
  console.warn(
    '[build-wasm] wasm-opt (binaryen) not on PATH — skipping -Oz. Install ' +
      'binaryen for a ~7% smaller wasm; functionally identical without it.',
  );
}
const mb = (p) => (statSync(p).size / 1048576).toFixed(2);

// Pin the wasm feature set to the wasm "MVP-plus" baseline that every
// shipping WebView understands. Recent rustc/LLVM enable reference-types,
// multivalue, bulk-memory, sign-ext, mutable-globals, and nontrapping-fptoint
// by default; the type tags they introduce break parsing on older iOS Safari
// (the symptom we hit was `CompileError: WebAssembly.Module doesn't parse at
// byte N: ...Type is non-Func, non-Struct, and non-Array`). Disabling
// reference-types is the load-bearing knob — without it the Type section
// only ever emits `funcref` / non-ref types — but we drop the whole 2.0
// bundle for the widest reach. The native (`ctx`) build is unaffected: this
// is the `wasm32-unknown-unknown` target only, scoped to this subprocess.
const WASM_MVP_FEATURES = [
  '-reference-types',
  '-multivalue',
  '-bulk-memory',
  '-sign-ext',
  '-mutable-globals',
  '-nontrapping-fptoint',
].join(',');
const RUSTFLAGS = `${process.env.RUSTFLAGS ?? ''} -C target-feature=${WASM_MVP_FEATURES}`.trim();

for (const [target, out] of [
  ['nodejs', resolve(root, 'pkg')],
  ['web', resolve(root, 'pkg-web')],
]) {
  console.log(`[build-wasm] wasm-pack build --release --target ${target} → ${out}`);
  const res = spawnSync('wasm-pack', ['build', '--release', '--target', target, '--out-dir', out], {
    cwd: crateDir,
    stdio: 'inherit',
    // Size profile, scoped to this subprocess only (workspace
    // [profile.release] / native `ctx` stay opt-level=3).
    env: {
      ...process.env,
      CARGO_PROFILE_RELEASE_OPT_LEVEL: 'z',
      CARGO_PROFILE_RELEASE_PANIC: 'abort',
      RUSTFLAGS,
    },
  });
  if (res.status !== 0) {
    console.error('[build-wasm] wasm-pack failed. Install: https://drager.github.io/wasm-pack/');
    process.exit(res.status ?? 1);
  }
  const wasm = join(out, 'csp_wasm_bg.wasm');
  if (hasWasmOpt) {
    const before = mb(wasm);
    const tmp = `${wasm}.opt`;
    // The two load-bearing flags: `--disable-reference-types` +
    // `--disable-gc`. With `--all-features` (the wasm-pack default the
    // script used to pass) binaryen freely emits typed-function-references
    // value types (e.g. `0x64`) and unfamiliar type-section tags during
    // optimization, even when the source wasm was MVP-only. Older iOS
    // WebKit can't parse those tags, so the optimized output silently
    // breaks on every iPhone/iPad — the `Type is non-Func, non-Struct,
    // and non-Array` CompileError. (GC subsumes typed-function-references
    // in newer binaryen, so disabling both covers the type universe that
    // tripped WebKit.) We avoid wasm-opt's newer per-feature toggles
    // (`--disable-stack-switching` etc.) because Ubuntu's apt binaryen
    // can be a few versions behind and exits on unknown flags.
    const o = spawnSync(
      'wasm-opt',
      [
        '-Oz',
        '--enable-bulk-memory', // Rust emits `memory.fill` — required input feature.
        '--enable-sign-ext',
        '--enable-mutable-globals',
        '--enable-nontrapping-float-to-int',
        '--enable-multivalue',
        '--disable-reference-types',
        '--disable-gc',
        '-o',
        tmp,
        wasm,
      ],
      { stdio: 'inherit' },
    );
    if (o.status !== 0) {
      console.error('[build-wasm] wasm-opt failed');
      process.exit(o.status ?? 1);
    }
    spawnSync('mv', [tmp, wasm]);
    console.log(`[build-wasm]   ${target}: ${before} MB → ${mb(wasm)} MB (wasm-opt -Oz)`);
  } else {
    console.log(`[build-wasm]   ${target}: ${mb(wasm)} MB (release; no wasm-opt)`);
  }
}
console.log('[build-wasm] one engine, two wasm targets (nodejs + web) built.');
