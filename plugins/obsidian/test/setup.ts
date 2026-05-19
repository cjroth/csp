// Global test setup — preloaded by `bunfig.toml` before any test file is
// imported.
//
//   1. Replace the bare `obsidian` import with the runtime shim. The npm
//      `obsidian` package ships only type declarations (no runtime) and is
//      not installed, so any module that imports it (main.ts /
//      settings-tab.ts) would fail to load. `mock.module` (registered here,
//      before the module graph is built) redirects it to the shim, which is
//      ALSO a UI recorder the settings-tab/main tests assert against.
//   2. Initialize the CSP engine once. Under Bun the `@csp/sdk` nodejs wasm
//      glue loads synchronously at import, so `initCsp()` is a no-op that
//      keeps the call shape identical to the browser/WebView path.

import { mock } from 'bun:test';
import { initCsp, isInitialized } from '@csp/sdk/web-init';
import * as obsidianShim from './mocks/obsidian-shim.ts';

mock.module('obsidian', () => obsidianShim);

if (!isInitialized()) {
  await initCsp();
}
