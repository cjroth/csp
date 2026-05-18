// Device identity resolution. The plugin owns the keypair and hands it to
// the engine via `Vault` options.identity — the engine then never auto-
// generates or writes a seed into `.context/` (CSP spec.md §10).
//
// Location mirrors the `ctx` CLI convention (CSP §9.1/§10):
//   - Desktop: `~/.context/id_ed25519` (+ `.pub` sidecar) via Node fs —
//     the *same file `ctx` uses*, device-global: one device, one key across
//     every vault it joins; survives deleting a vault's `.context/`.
//   - Mobile: no home dir, so `<vault>/.context/id_ed25519` via the vault
//     adapter (the per-vault opt-in of CSP §10). It lives INSIDE the
//     excluded `.context/` so it is never synced (CSP §10/§11). Setup
//     records `[identity] path` so a desktop `ctx` on a synced copy can
//     still resolve it.
//
// The pure `loadOrCreateIdentity` + `IdentityIO` seam keeps this unit
// testable.

import {
  Identity,
  type IdentityInstance,
  formatCspIdentity,
  formatPubkeySidecar,
  parseCspIdentity,
} from '@csp/sdk/web-init';
import type { MinimalDataAdapter } from './storage-adapter.js';

// The plugin ships as a CJS bundle; on desktop (Electron) Obsidian provides
// a real `require`. `fs`/`os`/`path` are externalized in esbuild, so these
// stay runtime requires and are only reached inside NodeHomeIdentityIO,
// which mobile never constructs.
declare const require: ((id: string) => unknown) | undefined;

/** Where the identity bytes live. `read` resolves null when absent. */
export interface IdentityIO {
  read(): Promise<string | null>;
  /** Write the identity body and its `.pub` sidecar atomically-ish. */
  write(body: string, pub: string): Promise<void>;
  /** Human-readable location, for logs / settings UI. */
  describe(): string;
}

/** Load an existing identity, or null if none is on disk. Never creates one
 * — startup uses this so a missing key is surfaced, not silently
 * regenerated. The caller owns `identity.free()`. */
export async function loadIdentity(io: IdentityIO): Promise<IdentityInstance | null> {
  const existing = await io.read();
  if (!existing) return null;
  return Identity.fromSeed(parseCspIdentity(existing));
}

/**
 * Load the existing identity, or generate + persist a fresh one. Only the
 * explicit setup flow calls this; `created` lets the caller surface "new
 * device key — add it to the peer's authorized_keys" (CSP §10). The caller
 * owns `identity.free()`.
 */
export async function loadOrCreateIdentity(
  io: IdentityIO,
): Promise<{ identity: IdentityInstance; created: boolean }> {
  const existing = await loadIdentity(io);
  if (existing) return { identity: existing, created: false };
  const identity = Identity.generate();
  const pk = identity.pubkey();
  try {
    await io.write(formatCspIdentity(identity.seed()), formatPubkeySidecar(pk.toSshString()));
  } finally {
    pk.free();
  }
  return { identity, created: true };
}

/** Vault-relative identity path used on mobile and recorded in
 * `[identity] path` so a `ctx` on a synced copy resolves the same file. It
 * sits inside the excluded `.context/` and is never synced (CSP §10/§11). */
export const VAULT_IDENTITY_PATH = '.context/id_ed25519';

/** Mobile / no-home-dir backend: identity lives inside the vault's
 * `.context/` via Obsidian's data adapter. */
export class VaultAdapterIdentityIO implements IdentityIO {
  private readonly path = VAULT_IDENTITY_PATH;
  constructor(private readonly adapter: MinimalDataAdapter) {}

  async read(): Promise<string | null> {
    if (!(await this.adapter.exists(this.path))) return null;
    const text = (await this.adapter.read(this.path)).trim();
    return text.length > 0 ? text : null;
  }

  async write(body: string, pub: string): Promise<void> {
    if (!(await this.adapter.exists('.context'))) await this.adapter.mkdir('.context');
    await this.adapter.write(this.path, body);
    await this.adapter.write(`${this.path}.pub`, pub);
  }

  describe(): string {
    return `<vault>/${this.path}`;
  }
}

/** Desktop backend: `~/.context/id_ed25519`, the same file `ctx` uses. Node
 * modules are required lazily so this file stays importable on the mobile
 * bundle (where this class is never constructed). */
export class NodeHomeIdentityIO implements IdentityIO {
  private readonly dir: string;
  private readonly file: string;
  // biome-ignore lint/suspicious/noExplicitAny: lazily-required node:fs
  private readonly fs: any;

  /** `homeDirOverride` is a test seam; production passes nothing and we
   * resolve `os.homedir()`. */
  constructor(homeDirOverride?: string) {
    if (typeof require !== 'function') {
      throw new Error('NodeHomeIdentityIO requires a desktop (Node) runtime');
    }
    this.fs = require('node:fs') as typeof import('node:fs');
    const os = require('node:os') as typeof import('node:os');
    const path = require('node:path') as typeof import('node:path');
    const home = homeDirOverride ?? os.homedir();
    this.dir = path.join(home, '.context');
    this.file = path.join(this.dir, 'id_ed25519');
  }

  async read(): Promise<string | null> {
    try {
      const text = (this.fs.readFileSync(this.file, 'utf8') as string).trim();
      return text.length > 0 ? text : null;
    } catch {
      return null; // ENOENT (or unreadable) → treat as absent
    }
  }

  async write(body: string, pub: string): Promise<void> {
    this.fs.mkdirSync(this.dir, { recursive: true });
    this.fs.writeFileSync(this.file, body, { mode: 0o600 });
    this.fs.writeFileSync(`${this.file}.pub`, pub);
    try {
      this.fs.chmodSync(this.file, 0o600);
    } catch {}
  }

  describe(): string {
    return this.file;
  }
}
