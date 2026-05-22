// Entry point for the engine Web Worker (issue 0010).
//
// esbuild bundles this file on its own into a standalone IIFE string, which
// the main bundle inlines (`__ENGINE_WORKER_SRC__`) and starts as a Blob
// Worker — no separate file to ship, mobile-WebView-safe (the same
// constraint that drives wasm inlining).
//
// All this does is stand up an `EngineWorkerHost` bound to the worker
// global scope. The host owns the real `RealVault` (wasm engine + WebSocket
// transport); the heavy synchronous engine calls (`commit_staged`,
// `to_bytes`, the fold, `session_feed`) run here, off the renderer thread,
// so the Obsidian UI never freezes during a sync.

import { EngineWorkerHost, type FromWorker, type ToWorker, selfPort } from '@csp/sdk/web-init';

// `self` in a dedicated worker is the `DedicatedWorkerGlobalScope` — it has
// `postMessage` + `onmessage`, the slice `selfPort` needs.
const scope = self as unknown as {
  postMessage(m: unknown): void;
  onmessage: ((ev: MessageEvent) => void) | null;
};

new EngineWorkerHost(selfPort<FromWorker, ToWorker>(scope));
