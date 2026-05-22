// Message protocol for the engine Web Worker (issue 0010).
//
// The wasm engine + WebSocket transport run inside a Worker so the heavy
// synchronous engine calls (`commit_staged`, `to_bytes`, `session_feed`,
// the fold) never block the host's renderer thread. The main thread keeps
// the Obsidian-facing surface: a `WorkerVault` that implements the public
// `Vault` contract by proxying to the worker, and the `.context/` storage
// adapter (which needs main-thread-only host APIs).
//
// Three message families cross the channel:
//   • command  (main → worker)  — a `Vault` method call, correlated by id.
//   • reply    (worker → main)  — the result of a command, by the same id.
//   • event    (worker → main)  — unsolicited: VaultEvents + observable
//                                 state deltas the main-thread shadow needs.
//   • storage  (worker ↔ main)  — the worker's RealVault drives storage
//                                 through a proxy that round-trips here.
//
// Everything is plain structured-clone-safe data (no functions, no class
// instances) so it survives `postMessage` unchanged.

import type { Snapshot, VaultEvent } from '../types.js';

/** How the worker's `RealVault` should be constructed. */
export type InitMode = 'create' | 'open' | 'clone';

/** The `init` command payload — everything the worker needs to stand up a
 * `RealVault`. The wasm bytes are shipped in so the worker never fetches
 * (mobile-WebView-safe, same constraint that drives the plugin's inlining). */
export interface InitPayload {
  mode: InitMode;
  /** 32-byte device identity seed. */
  seed: Uint8Array;
  /** Inlined csp-core wasm — instantiated inside the worker. */
  wasmBytes: Uint8Array;
  /** `create`/`clone` only — explicit vault id (`create`) is optional;
   * `clone` learns it from the peer. */
  vaultId?: string;
  peerUrl?: string;
  /** Raw pinned peer key bytes (CSP §10) — the caller decodes the SSH
   * form before `init` so the worker stays codec-free. */
  peerPubkey?: Uint8Array;
  authKey?: string;
}

/** A `Vault`-method command. `id` correlates the reply. */
export type Command =
  | { kind: 'cmd'; id: number; op: 'init'; payload: InitPayload }
  | { kind: 'cmd'; id: number; op: 'writeTextFile'; path: string; content: string }
  | { kind: 'cmd'; id: number; op: 'readTextFile'; path: string }
  | { kind: 'cmd'; id: number; op: 'deleteFile'; path: string }
  | { kind: 'cmd'; id: number; op: 'renameFile'; from: string; to: string }
  | { kind: 'cmd'; id: number; op: 'connectWithReconnect' }
  | { kind: 'cmd'; id: number; op: 'disconnect' }
  | { kind: 'cmd'; id: number; op: 'close' }
  | { kind: 'cmd'; id: number; op: 'createSnapshot'; name: string }
  | { kind: 'cmd'; id: number; op: 'deleteSnapshot'; name: string }
  | { kind: 'cmd'; id: number; op: 'restoreToSnapshot'; name: string }
  | { kind: 'cmd'; id: number; op: 'restoreToTime'; targetMs: number };

/** The reply to a `Command`, by the same `id`. */
export interface Reply {
  kind: 'reply';
  id: number;
  ok: boolean;
  /** Present when `ok` and the op returns a value (`readTextFile`). */
  value?: string;
  /** Present when `!ok` — a human-readable error string. */
  error?: string;
}

/** Observable state the main-thread shadow tracks so the bridge's
 * synchronous reads (`fileExists` / `listFiles` / `listSnapshots` /
 * `isConnected` / `identityPubkeySsh`) keep working without a round-trip.
 * Carries metadata only — file *content* always round-trips via
 * `readTextFile`, so this stays small even for a big vault. */
export interface Observable {
  /** Every live path + byte size — feeds `listFiles` / `fileExists`. */
  files: Array<{ path: string; size: number }>;
  snapshots: Snapshot[];
  connected: boolean;
  /** Device public key, OpenSSH form — constant after `init`. */
  identitySsh: string;
}

/** Unsolicited worker → main message: a VaultEvent and/or a fresh
 * Observable. They ride together (and `observable` is delivered first in
 * program order) so the shadow is already current when a `tree-changed`
 * event reaches the bridge. */
export interface EventMessage {
  kind: 'event';
  /** A VaultEvent to forward to subscribers, when this message carries one. */
  event?: VaultEvent;
  /** A refreshed observable snapshot, when state changed. */
  observable?: Observable;
}

/** Worker → main: the worker's storage proxy needs a `StorageAdapter` call
 * serviced on the main thread (the real adapter touches host APIs). */
export interface StorageRequest {
  kind: 'storage-req';
  id: number;
  method: string;
  /** Structured-clone-safe args (strings, `Uint8Array`s). */
  args: unknown[];
}

/** Main → worker: the result of a `StorageRequest`. */
export interface StorageResponse {
  kind: 'storage-res';
  id: number;
  ok: boolean;
  value?: unknown;
  error?: string;
}

/** Distributive `Omit` — preserves the discriminated union when stripping
 * the envelope fields, unlike the built-in `Omit` which collapses it. */
type DistributiveOmit<T, K extends keyof never> = T extends unknown ? Omit<T, K> : never;

/** A command minus the envelope fields (`kind`, `id`) — what
 * `WorkerVault.command` accepts; the envelope is filled in there. */
export type CommandBody = DistributiveOmit<Command, 'kind' | 'id'>;

/** Anything the main thread sends into the worker. */
export type ToWorker = Command | StorageResponse;
/** Anything the worker sends back to the main thread. */
export type FromWorker = Reply | EventMessage | StorageRequest;
