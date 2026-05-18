// Real reduced-surface bindings (CSP §4/§7): object encode/decode,
// identity/auth, and wire framing — NO merge, NO on-disk odb. This is a
// thin typed passthrough to the *one* Rust engine (`csp-core`) compiled to
// wasm; it is NOT a reimplementation (§16). Built by `bun run build:wasm`
// into `../pkg` via `wasm-pack`.

import { createRequire } from 'node:module';

interface CspWasmModule {
  blob_oid(content: Uint8Array): string;
  node_id_hex(seed: Uint8Array): string;
  ssh_pubkey(seed: Uint8Array, comment: string): string;
  build_primitive_object(
    seed: Uint8Array,
    treeHex: string,
    parentHex: string,
    counter: bigint,
    wallTime: bigint,
    subject: string,
  ): Uint8Array;
  object_oid(framed: Uint8Array): string;
  verify_primitive_object(framed: Uint8Array): string;
  wire_encode(json: string): Uint8Array;
  wire_decode(bytes: Uint8Array): string;
}

const req = createRequire(import.meta.url);
const m = req('../pkg/csp_wasm.js') as CspWasmModule;

/** SHA-1 object id of raw blob bytes (stock-git identical — §4). */
export function blobOid(content: Uint8Array): string {
  return m.blob_oid(content);
}

/** NodeId (ed25519 public key) hex for a 32-byte seed. */
export function nodeIdHex(seed: Uint8Array): string {
  return m.node_id_hex(seed);
}

/** OpenSSH `ssh-ed25519 …` line for `authorized_keys` (§10). */
export function sshPubkey(seed: Uint8Array, comment: string): string {
  return m.ssh_pubkey(seed, comment);
}

/** Build a signed primitive commit; returns its framed object bytes (§5.2). */
export function buildPrimitiveObject(
  seed: Uint8Array,
  treeHex: string,
  parentHex: string,
  counter: bigint,
  wallTime: bigint,
  subject: string,
): Uint8Array {
  return m.build_primitive_object(seed, treeHex, parentHex, counter, wallTime, subject);
}

/** The SHA-1 oid of framed object bytes. */
export function objectOid(framed: Uint8Array): string {
  return m.object_oid(framed);
}

/** Verify a primitive's in-object signature; returns the author NodeId hex
 * (throws on failure) — §6.3/§10. */
export function verifyPrimitiveObject(framed: Uint8Array): string {
  return m.verify_primitive_object(framed);
}

/** MessagePack-encode a wire message given as JSON (framing, §6.2/§6.6). */
export function wireEncode(json: string): Uint8Array {
  return m.wire_encode(json);
}

/** Decode a MessagePack wire frame back to JSON. */
export function wireDecode(bytes: Uint8Array): string {
  return m.wire_decode(bytes);
}
