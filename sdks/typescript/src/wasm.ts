// Typed passthrough to the *one* Rust engine (`csp-core`) compiled to wasm
// — **the full engine, merge included** (§4/§7/§16, one engine everywhere):
// a plugin computes its own byte-identical `main` exactly like `ctx`. NOT a
// reimplementation. Built by `bun run build:wasm` into `../pkg` via
// `wasm-pack` (nodejs target for Node/Bun; the SDK ships a `web` target for
// browser/WebView hosts).

import { createRequire } from 'node:module';

/** The real full engine (`csp_core::MemEngine` + the shared sans-IO
 * `Session`). Files-in / materialize-ops-out; host owns transport+storage.
 * Every method mirrors a `csp-core` call — there is no protocol logic in
 * TypeScript. */
export interface WasmEngine {
  free(): void;
  authorize(sshLine: string): void;
  /** `{ "path": [byte,…] }` → new primitive oid hex, or undefined (no-op). */
  commit_from_files(filesJson: string): string | undefined;
  export_closure(tipsJson: string): string;
  frontier_tips(): string[];
  integrate(rawsJson: string): number;
  known(): string[];
  main(): string;
  materialize_plan(onDiskJson: string): string;
  node_id(): string;
  node_ssh(): string;
  restore_snapshot(name: string): string;
  restore_time(tUnix: bigint): string;
  /** Feed one inbound wire frame → `{out:[[byte…]…],integrated,established}`. */
  session_feed(frame: Uint8Array): string;
  /** Opening `Hello` frame bytes (connector; the plugin never listens, §7). */
  session_start(channelBinding: Uint8Array): Uint8Array;
  set_ignore(ignore: string): void;
  snapshot(name: string): void;
  snapshots_json(): string;
  to_bytes(): Uint8Array;
  vault_id(): string;
}

interface WasmEngineCtor {
  create(seed: Uint8Array, vaultId: string, name: string): WasmEngine;
  open(seed: Uint8Array, persisted: Uint8Array, ignore: string): WasmEngine;
}

interface CspWasmModule {
  WasmEngine: WasmEngineCtor;
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
  config_parse(text: string): string;
  config_to_toml(json: string): string;
}

const req = createRequire(import.meta.url);
const m = req('../pkg/csp_wasm.js') as CspWasmModule;

/** The real full engine class (`csp-core` via wasm). Same code as `ctx`. */
export const WasmEngine = m.WasmEngine;

/** Node/Bun glue is loaded synchronously by `require` at import — init is a
 * no-op. (The browser `web` glue in `./wasm-web.ts` instantiates from inlined
 * bytes; the `#engine` imports map selects per runtime.) */
export async function initEngine(_input?: unknown): Promise<void> {}

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

/** Parse `.context/config` TOML into the typed `VaultConfig` as JSON — the
 * one Rust codec `ctx` uses, never reimplemented. */
export function configParse(text: string): string {
  return m.config_parse(text);
}

/** Serialize a `VaultConfig` (JSON) back to `.context/config` TOML text. */
export function configToToml(json: string): string {
  return m.config_to_toml(json);
}
