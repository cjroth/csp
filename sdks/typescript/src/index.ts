// Default entry. The thin-node host imports `@csp/sdk/web-init`; this `.`
// entry re-exports the same surface for tooling/tests that resolve the
// package root.

export * from './web-init.js';

// The real reduced-surface engine bindings (CSP §4/§7/§16). The host-facing
// `Vault` seam above stays stable; `engine` is the genuine wasm passthrough
// to the one Rust core (proven byte-identical to native — §18).
export * as engine from './wasm.js';
