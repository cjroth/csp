// The thin transport seam under the worker protocol (issue 0010).
//
// A `Port` is just "post a message / receive messages" — the minimal slice
// of `Worker` / `DedicatedWorkerGlobalScope` the protocol needs. Production
// wraps a real `Worker` (main side) and `self` (worker side). Tests use
// `linkedPorts()` to wire the two halves in-process, so the entire
// `WorkerVault ⇄ EngineWorkerHost` stack runs deterministically under
// `bun test` with no real Worker / DOM — that pair behaves exactly like a
// `RealVault`, just message-mediated, which is what makes "100% edge case"
// coverage tractable.

/** One end of a message channel. `TOut` is what this end sends, `TIn` what
 * it receives. */
export interface Port<TOut, TIn> {
  post(msg: TOut): void;
  /** Register the (single) message handler. Replaces any previous one. */
  onMessage(handler: (msg: TIn) => void): void;
}

/** Wrap a real `Worker` (the main-thread end). */
export function workerPort<TOut, TIn>(worker: Worker): Port<TOut, TIn> {
  return {
    post: (msg) => worker.postMessage(msg),
    onMessage: (handler) => {
      worker.onmessage = (ev: MessageEvent) => handler(ev.data as TIn);
    },
  };
}

/** Wrap the worker global scope (`self`, the worker-thread end). */
export function selfPort<TOut, TIn>(scope: {
  postMessage(m: unknown): void;
  onmessage: ((ev: MessageEvent) => void) | null;
}): Port<TOut, TIn> {
  return {
    post: (msg) => scope.postMessage(msg),
    onMessage: (handler) => {
      scope.onmessage = (ev: MessageEvent) => handler(ev.data as TIn);
    },
  };
}

/**
 * Two in-process ports wired back-to-back — the test double for a real
 * `Worker` boundary. Delivery is deferred through `queueMicrotask` so it
 * mimics `postMessage`'s always-async dispatch (a handler never observes a
 * message synchronously inside its own `post`), which is the property the
 * protocol's request/reply correlation relies on.
 */
export function linkedPorts<A, B>(): [Port<A, B>, Port<B, A>] {
  let handlerA: ((msg: B) => void) | null = null;
  let handlerB: ((msg: A) => void) | null = null;
  // Messages posted before the far end registered its handler must not be
  // dropped — queue them and flush on registration.
  const pendingForA: B[] = [];
  const pendingForB: A[] = [];

  const portA: Port<A, B> = {
    post: (msg) =>
      queueMicrotask(() => {
        if (handlerB) handlerB(msg);
        else pendingForB.push(msg);
      }),
    onMessage: (handler) => {
      handlerA = handler;
      const drain = pendingForA.splice(0);
      for (const m of drain) queueMicrotask(() => handler(m));
    },
  };
  const portB: Port<B, A> = {
    post: (msg) =>
      queueMicrotask(() => {
        if (handlerA) handlerA(msg);
        else pendingForA.push(msg);
      }),
    onMessage: (handler) => {
      handlerB = handler;
      const drain = pendingForB.splice(0);
      for (const m of drain) queueMicrotask(() => handler(m));
    },
  };
  return [portA, portB];
}
