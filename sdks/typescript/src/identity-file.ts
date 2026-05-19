// Codec for the on-disk device-identity file (CSP spec §10 — node identity
// is an ed25519 key; default location `~/.context/id_ed25519`,
// device-global, never synced — CSP §9.1).
//
// This file is **shared with `ctx`** (one device, one key). `ctx`
// (`crates/ctx/src/idstore.rs`) writes its self-generated key as the
// **bare hex of the 32-byte ed25519 seed** and, on read, content-detects
// three forms: an `OPENSSH PRIVATE KEY` block, an `ssh-ed25519` public line
// (agent), or the **bare-hex seed**. So the SDK writes the same bare-hex
// form `ctx` generates, and parses hex first; it also still accepts the
// legacy `csp-identity-v1 <base64>` line a previous plugin build wrote, so
// existing plugin keys keep working. `<path>.pub` carries the OpenSSH
// public line for human / `authorized_keys` use.

const LEGACY_PREFIX = 'csp-identity-v1 ';
const SEED_LEN = 32;
const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';

function b64decode(s: string): Uint8Array {
  const clean = s.replace(/=+$/, '');
  const out: number[] = [];
  let acc = 0;
  let bits = 0;
  for (const ch of clean) {
    const v = B64.indexOf(ch);
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

function hexEncode(bytes: Uint8Array): string {
  let out = '';
  for (const b of bytes) out += b.toString(16).padStart(2, '0');
  return out;
}

function hexDecode(s: string): Uint8Array {
  if (s.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(s)) {
    throw new Error('identity file is not valid hex');
  }
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = Number.parseInt(s.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

/** Serialize a 32-byte ed25519 seed to `ctx`'s native on-disk form: the
 * bare seed as lowercase hex (trailing newline; `ctx` trims on read). The
 * host writes this with 0600 perms (CSP §10). */
export function formatCspIdentity(seed: Uint8Array): string {
  if (seed.length !== SEED_LEN) {
    throw new Error(`identity seed wrong length: got ${seed.length}, want ${SEED_LEN}`);
  }
  return `${hexEncode(seed)}\n`;
}

/** Parse a device-identity file body → the 32-byte seed. Accepts `ctx`'s
 * bare-hex form (primary, interoperable) and the legacy
 * `csp-identity-v1 <base64>` line (migration). Reads only the first
 * non-empty line, so a `.pub`-style trailer is harmless. An OpenSSH
 * `-----BEGIN OPENSSH PRIVATE KEY-----` block is rejected with a clear
 * pointer (the SDK carries no OpenSSH key parser; `ctx` itself can derive a
 * seed from such a key). */
export function parseCspIdentity(text: string): Uint8Array {
  const trimmed = text.trim();
  if (trimmed.includes('BEGIN OPENSSH PRIVATE KEY')) {
    throw new Error(
      'identity file is an OpenSSH private key; the plugin uses the bare-hex ' +
        'seed form `ctx` writes — re-run setup or convert the key with `ctx`',
    );
  }
  const line = (trimmed.split('\n')[0] ?? '').trim();
  let seed: Uint8Array;
  if (line.startsWith(LEGACY_PREFIX)) {
    seed = b64decode(line.slice(LEGACY_PREFIX.length).trim());
  } else {
    seed = hexDecode(line);
  }
  if (seed.length !== SEED_LEN) {
    throw new Error(`identity seed wrong length: got ${seed.length}, want ${SEED_LEN}`);
  }
  return seed;
}

/** Content for the `<path>.pub` sidecar: the SSH wire-format pubkey plus a
 * trailing newline. */
export function formatPubkeySidecar(sshPubkey: string): string {
  return `${sshPubkey}\n`;
}
