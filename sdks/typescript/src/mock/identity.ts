// In-memory mock of the CSP device identity (CSP spec.md §10). NOT real
// ed25519 — the plugin performs no handshake against the mock, so a stable,
// unique-per-seed derivation is sufficient and is documented. Real ed25519
// (and the OpenSSH key format) land with `csp-wasm`; only this file and
// `identity-file.ts` change then — no plugin changes.

import type { Identity, Pubkey } from '../vault.js';

const STD_B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';

function b64decodeStd(s: string): Uint8Array {
  const clean = s.replace(/=+$/, '');
  const out: number[] = [];
  let acc = 0;
  let bits = 0;
  for (const ch of clean) {
    const v = STD_B64.indexOf(ch);
    if (v === -1) throw new Error(`invalid base64 character: ${JSON.stringify(ch)}`);
    acc = (acc << 6) | v;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out.push((acc >> bits) & 0xff);
    }
  }
  return Uint8Array.from(out);
}

function b64(bytes: Uint8Array): string {
  let out = '';
  for (let i = 0; i < bytes.length; i += 3) {
    const b0 = bytes[i] as number;
    const b1 = i + 1 < bytes.length ? (bytes[i + 1] as number) : 0;
    const b2 = i + 2 < bytes.length ? (bytes[i + 2] as number) : 0;
    const n = (b0 << 16) | (b1 << 8) | b2;
    const chunk = bytes.length - i;
    out += STD_B64.charAt((n >> 18) & 63) + STD_B64.charAt((n >> 12) & 63);
    out += chunk > 1 ? STD_B64.charAt((n >> 6) & 63) : '=';
    out += chunk > 2 ? STD_B64.charAt(n & 63) : '=';
  }
  return out;
}

function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

export class MockPubkey implements Pubkey {
  constructor(private readonly raw: Uint8Array) {}

  static fromBytes(bytes: Uint8Array): MockPubkey {
    return new MockPubkey(new Uint8Array(bytes));
  }

  /** Parse `ssh-ed25519 <base64>` (or a bare base64 blob). */
  static fromSshString(s: string): MockPubkey {
    const parts = s.trim().split(/\s+/);
    const blob = parts.length >= 2 ? (parts[1] as string) : (parts[0] as string);
    return new MockPubkey(b64decodeStd(blob));
  }
  toSshString(): string {
    return `ssh-ed25519 ${b64(this.raw)}`;
  }
  bytes(): Uint8Array {
    return new Uint8Array(this.raw);
  }
  fingerprint(): string {
    return hex(this.raw);
  }
  verify(): boolean {
    // No handshake runs against the mock; verification is vacuously true.
    return true;
  }
  free(): void {}
}

export class MockIdentity implements Identity {
  private constructor(private readonly seedBytes: Uint8Array) {}

  static generate(): MockIdentity {
    const seed = new Uint8Array(32);
    globalThis.crypto.getRandomValues(seed);
    return new MockIdentity(seed);
  }

  static fromSeed(seed: Uint8Array): MockIdentity {
    if (seed.length !== 32) {
      throw new Error(`identity seed wrong length: got ${seed.length}, want 32`);
    }
    return new MockIdentity(new Uint8Array(seed));
  }

  seed(): Uint8Array {
    return new Uint8Array(this.seedBytes);
  }

  pubkey(): MockPubkey {
    // Deterministic, unique per seed: the mock "pubkey" is the seed bytes.
    return new MockPubkey(new Uint8Array(this.seedBytes));
  }

  async sign(message: Uint8Array): Promise<Uint8Array> {
    // Stable 64-byte stand-in; unused by the plugin (no handshake).
    const sig = new Uint8Array(64);
    for (let i = 0; i < 64; i++) {
      sig[i] = (this.seedBytes[i % 32] ?? 0) ^ (message[i % message.length] ?? 0);
    }
    return sig;
  }

  free(): void {}
}
