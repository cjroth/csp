// Default WebSocket `TransportAdapter`. A thin node is outbound-only and
// never listens (CSP spec §7). Uses the runtime's global `WebSocket`
// (Bun, Node ≥ 22, every browser/WebView). Binary length-delimited frames
// (§6.2) — each frame is one MessagePack `Msg`. TLS/`wss://` is the socket's
// concern; channel binding is empty for `ws://` (trusted/proxied, §10) — a
// browser can't expose the peer cert anyway.

import type { TransportAdapter, TransportConn } from './types.js';

export function makeWebSocketTransport(WS: typeof globalThis.WebSocket): TransportAdapter {
  return {
    async connect(url: string): Promise<TransportConn> {
      const ws = new WS(url);
      ws.binaryType = 'arraybuffer';
      await new Promise<void>((res, rej) => {
        ws.addEventListener('open', () => res(), { once: true });
        ws.addEventListener('error', () => rej(new Error(`websocket error to ${url}`)), {
          once: true,
        });
      });
      const inbox: Uint8Array[] = [];
      let wake: ((v: Uint8Array | null) => void) | null = null;
      let closed = false;
      ws.addEventListener('message', (ev: MessageEvent) => {
        const d = ev.data;
        const bytes =
          d instanceof ArrayBuffer
            ? new Uint8Array(d)
            : ArrayBuffer.isView(d)
              ? new Uint8Array(d.buffer, d.byteOffset, d.byteLength)
              : null;
        if (!bytes) return;
        if (wake) {
          const w = wake;
          wake = null;
          w(bytes);
        } else {
          inbox.push(bytes);
        }
      });
      const onClose = () => {
        closed = true;
        if (wake) {
          const w = wake;
          wake = null;
          w(null);
        }
      };
      ws.addEventListener('close', onClose, { once: true });
      return {
        async send(bytes: Uint8Array): Promise<void> {
          ws.send(bytes);
        },
        async *recv(): AsyncGenerator<Uint8Array> {
          while (true) {
            if (inbox.length > 0) {
              yield inbox.shift() as Uint8Array;
              continue;
            }
            if (closed) return;
            const next = await new Promise<Uint8Array | null>((r) => {
              wake = r;
            });
            if (next === null) return;
            yield next;
          }
        },
        channelBinding(): Uint8Array | null {
          return null;
        },
        async close(): Promise<void> {
          try {
            ws.close();
          } catch {
            /* already closed */
          }
        },
      };
    },
  };
}

/** Resolve the runtime's global `WebSocket` lazily (so importing the SDK in
 * a non-WS context doesn't throw). */
export function defaultTransport(): TransportAdapter {
  const WS = (globalThis as { WebSocket?: typeof globalThis.WebSocket }).WebSocket;
  if (typeof WS !== 'function') {
    throw new Error(
      'no global WebSocket; pass `transport` explicitly (Node < 22 needs a polyfill)',
    );
  }
  return makeWebSocketTransport(WS);
}
