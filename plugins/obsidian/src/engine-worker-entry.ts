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
  addEventListener: (name: string, h: (e: { reason?: unknown; message?: string }) => void) => void;
};

// Mirror the worker's console output back to the main thread as `log`
// messages so the host can render an in-app log viewer (issue 0013). On
// iOS Obsidian the WebView dev console is unreachable, so without this
// every `[engine-worker …]` line and any worker-side error is invisible.
// Done BEFORE `EngineWorkerHost` is constructed so init-time output
// (wasm decode failures, identity errors, …) is captured.
{
  const ts = (): string => {
    const d = new Date();
    const p = (n: number, w = 2) => String(n).padStart(w, '0');
    return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`;
  };
  const stringify = (args: unknown[]): string =>
    args
      .map((a) => {
        if (typeof a === 'string') return a;
        if (a instanceof Error) return `${a.name}: ${a.message}`;
        try {
          return JSON.stringify(a);
        } catch {
          return String(a);
        }
      })
      .join(' ');
  // Capture the originals so we still emit to the host's console on
  // desktop (where the dev console *does* work and a developer expects
  // to see worker output there too).
  const origLog = console.log.bind(console);
  const origErr = console.error.bind(console);
  console.log = (...args: unknown[]): void => {
    origLog(...args);
    try {
      scope.postMessage({ kind: 'log', level: 'info', ts: ts(), msg: stringify(args) });
    } catch {
      // The channel may not be up yet (very early init) — drop, the
      // original `console.log` above already captured it locally.
    }
  };
  console.error = (...args: unknown[]): void => {
    origErr(...args);
    try {
      scope.postMessage({ kind: 'log', level: 'error', ts: ts(), msg: stringify(args) });
    } catch {
      // see above
    }
  };
}

// Unhandled promise rejections do NOT auto-propagate to the host's
// `worker.onerror`; they vanish into the worker void otherwise. Throwing
// re-raises them as script errors, which DO reach `worker.onerror` on the
// main thread — so an operator sees them in the Obsidian console instead
// of staring at a worker that silently did nothing.
scope.addEventListener('unhandledrejection', (e) => {
  const reason = e.reason instanceof Error ? e.reason : new Error(String(e.reason));
  // Defer one tick so the rejection-handling completes first.
  queueMicrotask(() => {
    throw reason;
  });
});

new EngineWorkerHost(selfPort<FromWorker, ToWorker>(scope));
