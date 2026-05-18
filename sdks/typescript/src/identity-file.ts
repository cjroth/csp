// Codec for the on-disk device-identity file (CSP spec.md §10 — node
// identity is an SSH key; default location `~/.context/id_ed25519`,
// device-global, never synced — CSP §9.1).
//
//   <path>      one line: `csp-identity-v1 <base64nopad(32-byte seed)>\n`
//   <path>.pub  one line: `<ssh-ed25519 wire-format pubkey>\n`
//
// PROVISIONAL: CSP spec §10 mandates the canonical OpenSSH ed25519 format.
// Until `csp-wasm` lands the real codec, this uses a simple reversible line
// (same shape as agentsync's, prefix renamed). It is byte-stable and
// round-trips; only this file changes when the OpenSSH format is wired
// (obsidian-plugin-spec.md §14) — no plugin changes.

const PREFIX = 'csp-identity-v1 ';
const SEED_LEN = 32;
const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';

function b64encodeNoPad(bytes: Uint8Array): string {
  let out = '';
  for (let i = 0; i < bytes.length; i += 3) {
    const b0 = bytes[i] as number;
    const b1 = i + 1 < bytes.length ? (bytes[i + 1] as number) : 0;
    const b2 = i + 2 < bytes.length ? (bytes[i + 2] as number) : 0;
    const n = (b0 << 16) | (b1 << 8) | b2;
    const chunk = bytes.length - i;
    out += B64.charAt((n >> 18) & 63) + B64.charAt((n >> 12) & 63);
    if (chunk > 1) out += B64.charAt((n >> 6) & 63);
    if (chunk > 2) out += B64.charAt(n & 63);
  }
  return out;
}

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

/** Serialize a 32-byte ed25519 seed to the `csp-identity-v1` line
 * (trailing newline included). */
export function formatCspIdentity(seed: Uint8Array): string {
  if (seed.length !== SEED_LEN) {
    throw new Error(`identity seed wrong length: got ${seed.length}, want ${SEED_LEN}`);
  }
  return `${PREFIX}${b64encodeNoPad(seed)}\n`;
}

/** Parse a `csp-identity-v1` file body, returning the 32-byte seed. Reads
 * only the first line so a `.pub`-style trailer is harmless. */
export function parseCspIdentity(text: string): Uint8Array {
  const line = text.split('\n')[0] ?? '';
  if (!line.startsWith(PREFIX)) {
    throw new Error('identity file is not in csp-identity-v1 format');
  }
  const seed = b64decode(line.slice(PREFIX.length).trim());
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
