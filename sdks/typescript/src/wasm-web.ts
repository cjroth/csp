// **Browser/WebView** glue for the one Rust engine — same byte-identical
// `csp-core` as the node glue (`./wasm.ts`), via the wasm-pack `web` target
// (`../pkg-web`). The package `imports` map (`#engine`) selects this under
// the `browser` condition (esbuild/Obsidian); Node/Bun get `./wasm.ts`.
//
// wasm-pack `web` instantiation is async: the host inlines the .wasm bytes
// and calls `initEngine(bytes)` once at startup (the Obsidian plugin does
// this in `main.ts`). All exports below are valid only after that resolves.

import init, * as wasm from '../pkg-web/csp_wasm.js';
import type { WasmEngine as WasmEngineInstance } from './wasm.js';

// A *declared* interface (not a type-alias re-export) so it merges with the
// `WasmEngine` value (ctor) below — type + value of one name, exactly like
// `./wasm.ts`'s interface+const.
export interface WasmEngine extends WasmEngineInstance {}

type WasmInput = Parameters<typeof init>[0];
let ready = false;

/** Instantiate the wasm module from inlined bytes (or a URL/Response).
 * Idempotent. The browser `web` target requires this before any use. */
export async function initEngine(input?: WasmInput): Promise<void> {
  if (ready) return;
  // biome-ignore lint/suspicious/noExplicitAny: wasm-bindgen input shape
  await init(input ? ({ module_or_path: input } as any) : undefined);
  ready = true;
}

function assertReady(): void {
  if (!ready) {
    throw new Error('csp: wasm not initialized — call `await initCsp(wasmBytes)` first');
  }
}

interface WasmEngineCtor {
  create(seed: Uint8Array, vaultId: string, name: string): WasmEngineInstance;
  open(seed: Uint8Array, persisted: Uint8Array, ignore: string): WasmEngineInstance;
}

/** Same surface as `./wasm.ts` `WasmEngine` (post-init). */
export const WasmEngine: WasmEngineCtor = {
  create(seed, vaultId, name) {
    assertReady();
    return wasm.WasmEngine.create(seed, vaultId, name) as unknown as WasmEngineInstance;
  },
  open(seed, persisted, ignore) {
    assertReady();
    return wasm.WasmEngine.open(seed, persisted, ignore) as unknown as WasmEngineInstance;
  },
};

export function blobOid(content: Uint8Array): string {
  assertReady();
  return wasm.blob_oid(content);
}
export function nodeIdHex(seed: Uint8Array): string {
  assertReady();
  return wasm.node_id_hex(seed);
}
export function sshPubkey(seed: Uint8Array, comment: string): string {
  assertReady();
  return wasm.ssh_pubkey(seed, comment);
}
export function buildPrimitiveObject(
  seed: Uint8Array,
  treeHex: string,
  parentHex: string,
  counter: bigint,
  wallTime: bigint,
  subject: string,
): Uint8Array {
  assertReady();
  return wasm.build_primitive_object(seed, treeHex, parentHex, counter, wallTime, subject);
}
export function objectOid(framed: Uint8Array): string {
  assertReady();
  return wasm.object_oid(framed);
}
export function verifyPrimitiveObject(framed: Uint8Array): string {
  assertReady();
  return wasm.verify_primitive_object(framed);
}
export function wireEncode(json: string): Uint8Array {
  assertReady();
  return wasm.wire_encode(json);
}
export function wireDecode(bytes: Uint8Array): string {
  assertReady();
  return wasm.wire_decode(bytes);
}
export function configParse(text: string): string {
  assertReady();
  return wasm.config_parse(text);
}
export function configToToml(json: string): string {
  assertReady();
  return wasm.config_to_toml(json);
}
