import { describe, expect, test } from 'bun:test';
import { formatCspIdentity, formatPubkeySidecar, parseCspIdentity } from '../src/identity-file.js';

describe('csp identity file codec (ctx-interoperable)', () => {
  test('writes the bare-hex seed `ctx` generates, and round-trips it', () => {
    const seed = new Uint8Array(32);
    for (let i = 0; i < 32; i++) seed[i] = (i * 7 + 3) & 0xff;
    const line = formatCspIdentity(seed);
    // Exactly what `ctx`'s idstore writes (`hex::encode(seed)`), + newline.
    expect(line).toBe(`${Array.from(seed, (b) => b.toString(16).padStart(2, '0')).join('')}\n`);
    expect(Array.from(parseCspIdentity(line))).toEqual(Array.from(seed));
  });

  test('parses a hex seed written by `ctx` (no trailing newline)', () => {
    const seed = new Uint8Array(32).fill(0xab);
    const ctxBody = Array.from(seed, (b) => b.toString(16).padStart(2, '0')).join('');
    expect(Array.from(parseCspIdentity(ctxBody))).toEqual(Array.from(seed));
  });

  test('still parses a legacy csp-identity-v1 key (migration)', () => {
    const legacy = 'csp-identity-v1 AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n';
    const seed = parseCspIdentity(legacy);
    expect(seed.length).toBe(32);
    expect(Array.from(seed).every((b) => b === 0)).toBe(true);
  });

  test('tolerates a .pub-style trailer (first line only)', () => {
    const seed = new Uint8Array(32).fill(9);
    const body = `${formatCspIdentity(seed)}ssh-ed25519 AAAA... comment\n`;
    expect(Array.from(parseCspIdentity(body))).toEqual(Array.from(seed));
  });

  test('rejects wrong length, non-hex, and OpenSSH private keys', () => {
    expect(() => formatCspIdentity(new Uint8Array(16))).toThrow(/wrong length/);
    expect(() => parseCspIdentity('not hex at all')).toThrow(/hex/);
    expect(() => parseCspIdentity('ab')).toThrow(/wrong length/); // 1 byte
    expect(() => parseCspIdentity('-----BEGIN OPENSSH PRIVATE KEY-----\n')).toThrow(
      /OpenSSH private key/,
    );
  });

  test('pubkey sidecar adds a trailing newline', () => {
    expect(formatPubkeySidecar('ssh-ed25519 AAAA')).toBe('ssh-ed25519 AAAA\n');
  });
});
