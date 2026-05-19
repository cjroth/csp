// Build the Obsidian plugin to a single CJS bundle (`main.js`) for desktop
// AND mobile.
//
// One engine everywhere (CSP spec.md §16): the plugin runs the *real* Rust
// engine via `@csp/sdk`, whose `#engine` imports map resolves to the
// wasm-pack **web** glue under esbuild's `browser` condition. The wasm bytes
// are read from the SDK's `pkg-web/` and inlined as base64
// (`__CSP_WASM_B64__`); `main.ts` passes them to `initCsp()` so the WebView
// never has to fetch (mobile can't fetch arbitrary local URLs).

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
  'pkg-web',
  'csp_wasm_bg.wasm',
);
if (!existsSync(wasmPath)) {
  console.error(
    `[esbuild] csp wasm not found at ${wasmPath}\n` +
      '[esbuild] Run `bun run build:wasm` in sdks/typescript first ' +
      '(builds the nodejs + web wasm targets).',
  );
  process.exit(1);
}
const wasmB64 = readFileSync(wasmPath).toString('base64');

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
  // Resolve `@csp/sdk`'s `#engine` imports map to the web wasm glue.
  conditions: ['browser'],
  external: [
    'obsidian',
    'electron',
    // Node builtins are reached only on desktop via a guarded require()
    // (NodeHomeIdentityIO); externalize so the browser/mobile bundle never
    // tries to resolve them.
    'node:fs',
    'node:os',
    'node:path',
    'node:module',
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
  loader: { '.wasm': 'binary' },
  define: {
    __CSP_WASM_B64__: JSON.stringify(wasmB64),
    'process.env.NODE_ENV': JSON.stringify(prod ? 'production' : 'development'),
    // The wasm-pack `web` glue's no-arg init branch touches
    // `import.meta.url`; we always pass bytes explicitly, so neutralize it
    // (esbuild warns on `import.meta` under format=cjs otherwise).
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
