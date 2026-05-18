import { describe, expect, test } from 'bun:test';
import { formatCspIdentity, formatPubkeySidecar, parseCspIdentity } from '../src/identity-file.js';

describe('csp identity file codec', () => {
  test('round-trips a 32-byte seed', () => {
    const seed = new Uint8Array(32);
    for (let i = 0; i < 32; i++) seed[i] = (i * 7 + 3) & 0xff;
    const line = formatCspIdentity(seed);
    expect(line.startsWith('csp-identity-v1 ')).toBe(true);
    expect(line.endsWith('\n')).toBe(true);
    expect(Array.from(parseCspIdentity(line))).toEqual(Array.from(seed));
  });

  test('tolerates a .pub-style trailer (first line only)', () => {
    const seed = new Uint8Array(32).fill(9);
    const body = `${formatCspIdentity(seed)}ssh-ed25519 AAAA... comment\n`;
    expect(Array.from(parseCspIdentity(body))).toEqual(Array.from(seed));
  });

  test('rejects wrong length and wrong format', () => {
    expect(() => formatCspIdentity(new Uint8Array(16))).toThrow(/wrong length/);
    expect(() => parseCspIdentity('not-csp foo')).toThrow(/csp-identity-v1/);
  });

  test('pubkey sidecar adds a trailing newline', () => {
    expect(formatPubkeySidecar('ssh-ed25519 AAAA')).toBe('ssh-ed25519 AAAA\n');
  });
});
