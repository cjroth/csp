// Global test setup — preloaded by `bunfig.toml` before any test file is
// imported.
//
//   1. Redirect bare `obsidian` imports to the runtime shim (the npm
//      package ships only types and would error at import time in Bun) so
//      any test that transitively imports an obsidian-importing module
//      (main.ts / settings-tab.ts) still runs.
//   2. Initialize the CSP engine once. Today @csp/sdk runs on the in-memory
//      mock, so `initCsp()` takes no wasm bytes (CSP spec §13.2 — csp-wasm
//      is a residual gate); the call shape stays identical to what the real
//      wasm path will use, so no test changes when it lands.

import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { initCsp, isInitialized } from '@csp/sdk/web-init';
import { plugin } from 'bun';

const setupDir = dirname(fileURLToPath(import.meta.url));

plugin({
  name: 'obsidian-shim',
  setup(build) {
    build.onResolve({ filter: /^obsidian$/ }, () => ({
      path: resolve(setupDir, 'mocks', 'obsidian-shim.ts'),
    }));
  },
});

if (!isInitialized()) {
  await initCsp();
}
