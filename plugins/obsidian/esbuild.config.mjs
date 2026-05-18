// Build the Obsidian plugin to a single CJS bundle (`main.js`) consumable
// by Obsidian on desktop AND mobile.
//
// The CSP wasm is read from the SDK's freshly-built `dist/web-pkg/` and
// inlined as a base64 constant under the global `__CSP_WASM_B64__`, so the
// plugin can call `initCsp()` synchronously without fetching at runtime
// (mobile WebViews can't fetch arbitrary local URLs).
//
// `csp-wasm` is a residual gate (CSP spec.md §13.2 / obsidian-plugin-spec
// §14): until it is built the .wasm is absent — we inline an EMPTY constant
// and continue, because the @csp/sdk seam runs on an in-memory mock. The
// plugin still builds, unit-tests, and e2e-tests; real cross-device sync
// requires csp-wasm. This is the ONLY place that changes when it lands.

import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import esbuild from 'esbuild';

const __dirname = dirname(fileURLToPath(import.meta.url));
const prod = process.argv.includes('production');

const wasmPath = resolve(
  __dirname,
  '..',
  '..',
  'sdks',
  'typescript',
  'dist',
  'web-pkg',
  'csp_wasm_bg.wasm',
);
let wasmB64 = '';
if (existsSync(wasmPath)) {
  wasmB64 = readFileSync(wasmPath).toString('base64');
} else {
  console.warn(
    `[esbuild] csp-wasm not found at ${wasmPath}\n` +
      '[esbuild] Inlining an empty constant — the @csp/sdk seam runs on the\n' +
      '[esbuild] in-memory mock (CSP §13.2; repo task #6). The plugin builds\n' +
      '[esbuild] and tests; real cross-device sync requires csp-wasm.',
  );
}

const banner = `/*
  Context for Obsidian — bundled by esbuild.
  Context Sync Protocol (see spec.md). plugins/obsidian/
*/`;

const buildOpts = {
  banner: { js: banner },
  entryPoints: ['src/main.ts'],
  bundle: true,
  format: 'cjs',
  target: 'ES2020',
  platform: 'browser',
  external: [
    'obsidian',
    'electron',
    // Node builtins are reached only on desktop via a guarded require()
    // (NodeHomeIdentityIO); externalize so the browser/mobile bundle never
    // tries to resolve them.
    'node:fs',
    'node:os',
    'node:path',
    '@codemirror/autocomplete',
    '@codemirror/collab',
    '@codemirror/commands',
    '@codemirror/language',
    '@codemirror/lint',
    '@codemirror/search',
    '@codemirror/state',
    '@codemirror/view',
    '@lezer/common',
    '@lezer/highlight',
    '@lezer/lr',
  ],
  define: {
    __CSP_WASM_B64__: JSON.stringify(wasmB64),
    'process.env.NODE_ENV': JSON.stringify(prod ? 'production' : 'development'),
    // wasm-pack `web` glue (future) has a dead-code default-input branch
    // that touches `import.meta.url`; we always pass bytes explicitly, but
    // esbuild still warns under format=cjs. Replace with a literal.
    'import.meta.url': JSON.stringify('context-plugin://main'),
  },
  outfile: 'main.js',
  sourcemap: prod ? false : 'inline',
  treeShaking: true,
  minify: prod,
  logLevel: 'info',
};

if (prod) {
  await esbuild.build(buildOpts);
} else {
  const ctx = await esbuild.context(buildOpts);
  await ctx.watch();
  console.log('[esbuild] watching for changes…');
}
