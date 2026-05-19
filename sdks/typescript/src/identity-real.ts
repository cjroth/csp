// Real device identity over the one Rust engine (CSP §10). The seed is the
// 32-byte ed25519 private seed; the public key + OpenSSH line are derived by
// the *same* `csp-core` code `ctx` uses (`node_id_hex`/`ssh_pubkey` in
// wasm). Signing of handshake transcripts + primitives is performed *inside*
// the engine (the `Session`), so `Identity.sign` is not used on the real
// path — the host only ever needs the seed and the public key.

import { nodeIdHex, sshPubkey } from '#engine';
import type { Identity as IdentityContract, Pubkey as PubkeyContract } from './vault.js';

const STD = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';

function b64(bytes: Uint8Array): string {
  let out = '';
  for (let i = 0; i < bytes.length; i += 3) {
    const b0 = bytes[i] as number;
    const b1 = i + 1 < bytes.length ? (bytes[i + 1] as number) : 0;
    const b2 = i + 2 < bytes.length ? (bytes[i + 2] as number) : 0;
    const n = (b0 << 16) | (b1 << 8) | b2;
    const k = bytes.length - i;
    out += STD.charAt((n >> 18) & 63) + STD.charAt((n >> 12) & 63);
    out += k > 1 ? STD.charAt((n >> 6) & 63) : '=';
    out += k > 2 ? STD.charAt(n & 63) : '=';
  }
  return out;
}

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** OpenSSH `ssh-ed25519 <b64>` framing of a raw 32-byte pubkey — pure key
 * format (the same bytes `csp_core::identity::ssh_pubkey_string` emits),
 * NOT a crypto reimplementation. Used only to render a pinned peer key. */
function sshFromPubkeyBytes(pk: Uint8Array): string {
  const algo = new TextEncoder().encode('ssh-ed25519');
  const blob = new Uint8Array(4 + algo.length + 4 + pk.length);
  const dv = new DataView(blob.buffer);
  dv.setUint32(0, algo.length);
  blob.set(algo, 4);
  dv.setUint32(4 + algo.length, pk.length);
  blob.set(pk, 4 + algo.length + 4);
  return `ssh-ed25519 ${b64(blob)} csp`;
}

class RealPubkey implements PubkeyContract {
  constructor(
    private readonly raw: Uint8Array,
    private readonly ssh?: string,
  ) {}
  toSshString(): string {
    return this.ssh ?? sshFromPubkeyBytes(this.raw);
  }
  bytes(): Uint8Array {
    return new Uint8Array(this.raw);
  }
  fingerprint(): string {
    return Array.from(this.raw, (b) => b.toString(16).padStart(2, '0')).join('');
  }
  verify(): boolean {
    // Verification runs inside the engine (the Session); not needed host-side.
    return true;
  }
  free(): void {}
}

class RealIdentity implements IdentityContract {
  private constructor(private readonly seedBytes: Uint8Array) {}

  static generate(): RealIdentity {
    const s = new Uint8Array(32);
    crypto.getRandomValues(s);
    return new RealIdentity(s);
  }
  static fromSeed(seed: Uint8Array): RealIdentity {
    if (seed.length !== 32) throw new Error(`seed must be 32 bytes, got ${seed.length}`);
    return new RealIdentity(new Uint8Array(seed));
  }
  seed(): Uint8Array {
    return new Uint8Array(this.seedBytes);
  }
  pubkey(): PubkeyContract {
    return new RealPubkey(hexToBytes(nodeIdHex(this.seedBytes)), sshPubkey(this.seedBytes, 'csp'));
  }
  async sign(): Promise<Uint8Array> {
    throw new Error('csp: signing is performed inside the engine (the Session), not host-side');
  }
  free(): void {}
}

export const Identity = {
  generate(): IdentityContract {
    return RealIdentity.generate();
  },
  fromSeed(seed: Uint8Array): IdentityContract {
    return RealIdentity.fromSeed(seed);
  },
};

export const Pubkey = {
  fromBytes(bytes: Uint8Array): PubkeyContract {
    return new RealPubkey(new Uint8Array(bytes));
  },
  fromSshString(s: string): PubkeyContract {
    const parts = s.trim().split(/\s+/);
    const blob = parts.length >= 2 ? (parts[1] as string) : (parts[0] as string);
    // Decode std base64 → ssh wire blob → trailing 32-byte key.
    const bin = atob(blob);
    const raw = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) raw[i] = bin.charCodeAt(i);
    const key = raw.slice(raw.length - 32);
    return new RealPubkey(key, s.trim());
  },
};
