# @csp/sdk

TypeScript SDK **seam** for the Context Sync Protocol (CSP — see
[`../../spec.md`](../../spec.md), §16 "one core, thin bindings").

This package is the typed surface a thin-node host (the Obsidian plugin,
`../../plugins/obsidian`) binds to. Today it ships:

- **Types** — the CSP-recast public API contract (`Vault`, `StorageAdapter`,
  `TransportAdapter`, `VaultEvent`, `CspConfig`, …). No hub, no
  server-minted vault id (CSP spec §5): a thin node connects to a *peer*
  (a full node in listen mode) and converges via the engine.
- **Working helpers** — the lossless `.context/config` TOML codec and the
  identity-file codec (provisional formats; see below).
- **An in-memory mock** (`src/mock/`) implementing the `Vault` contract so
  the plugin is fully buildable and unit/e2e testable **without** the real
  `csp-wasm` runtime. The mock converges file state between vaults sharing a
  peer URL; it does **not** implement the CSP fold/merge — the plugin asserts
  no fold SHAs (CSP spec §13), so this is sufficient and is documented.

## Residual gates (CSP spec §13.2 / obsidian-plugin-spec §14)

Deferred and **isolated entirely behind this package** — when they land, the
plugin, its tests, and its module boundaries do not change:

- The real `crates/csp-wasm` reduced thin-node surface (object encode/decode,
  sync state machine, auth, framing — no merge, no on-disk odb/packfiles).
  `scripts/build-wasm.mjs` is the wiring point; it errors clearly until
  `csp-wasm` exists.
- The canonical OpenSSH `~/.context/id_ed25519` byte format (CSP spec §10).
  `src/identity-file.ts` uses a provisional `csp-identity-v1` line.
- The canonical `.context/config` schema (CSP spec §9.1/§17.1). `src/config.ts`
  models a provisional `[peer]`/`[identity]`/`[scope]` schema, lossless w.r.t.
  unknown keys so a newer `ctx` never clobbers an older plugin (and vice
  versa).

When `csp-wasm` is built, implement `Vault`/`Identity`/`Pubkey` over it and
point `web-init.ts` at the real module instead of `src/mock/`.
