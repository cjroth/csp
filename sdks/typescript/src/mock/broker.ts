// In-memory convergence substrate for the mock. Stands in for "a full node
// in listen mode" (CSP spec.md §6.1): mock thin-node vaults that share a
// peer URL join the same room and converge their working trees, so the
// host plugin's two-way sync + reconcile is exercised end to end without
// the real protocol. This is NOT the CSP fold/merge — convergence is
// whole-file last-writer-wins by a monotonic rev. The plugin asserts no
// fold SHAs (CSP spec §13), so this is sufficient and is documented.

import type { TransportConn } from '../types.js';

export interface RoomFile {
  content: string;
  deleted: boolean;
  /** Monotonic global revision — higher wins (deterministic LWW). */
  rev: number;
}

let revCounter = 0;
/** Allocate the next global revision (the mock's total-order stand-in). */
export function nextRev(): number {
  return ++revCounter;
}

/** A connected mock peer — implemented by MockVault. */
export interface RoomMember {
  /** Pull the room's current state into local state; emit tree-changed if
   * anything changed. Called on join and on every remote publish. */
  pullFromRoom(): void;
}

export class Room {
  readonly files = new Map<string, RoomFile>();
  private readonly members = new Set<RoomMember>();

  join(m: RoomMember): void {
    this.members.add(m);
  }
  leave(m: RoomMember): void {
    this.members.delete(m);
  }
  hasMembers(): boolean {
    return this.members.size > 0;
  }

  /** Merge one path at `rev` (LWW). Returns true if the room changed. */
  publish(path: string, file: RoomFile): boolean {
    const cur = this.files.get(path);
    if (cur && cur.rev >= file.rev) return false;
    this.files.set(path, { ...file });
    return true;
  }

  /** Notify every member except `origin` to re-pull. */
  broadcast(origin: RoomMember): void {
    for (const m of this.members) {
      if (m === origin) continue;
      m.pullFromRoom();
    }
  }
}

const rooms = new Map<string, Room>();

/** Get (or create) the room for `peerUrl`. */
export function roomFor(peerUrl: string): Room {
  let r = rooms.get(peerUrl);
  if (!r) {
    r = new Room();
    rooms.set(peerUrl, r);
  }
  return r;
}

/** Test seam — wipe all rooms + reset the rev counter between cases. */
export function _resetBroker(): void {
  rooms.clear();
  revCounter = 0;
}

// ---- Optional in-memory transport pair (interface parity only) ----
//
// The mock vault converges via `Room`, not this transport; it exists so a
// caller that insists on injecting a `TransportAdapter` gets a valid duplex
// rather than a hard dependency on a real WebSocket.

export function memoryTransportPair(): [TransportConn, TransportConn] {
  const make = (
    inbox: Uint8Array[],
    peerInbox: Uint8Array[],
    wake: { fn: (() => void) | null },
    peerWake: { fn: (() => void) | null },
  ): TransportConn => ({
    async send(bytes: Uint8Array): Promise<void> {
      peerInbox.push(new Uint8Array(bytes));
      peerWake.fn?.();
    },
    async *recv(): AsyncGenerator<Uint8Array> {
      while (true) {
        if (inbox.length > 0) {
          yield inbox.shift() as Uint8Array;
          continue;
        }
        await new Promise<void>((res) => {
          wake.fn = res;
        });
        wake.fn = null;
      }
    },
    channelBinding(): Uint8Array | null {
      return null;
    },
    async close(): Promise<void> {},
  });
  const a: Uint8Array[] = [];
  const b: Uint8Array[] = [];
  const wakeA = { fn: null as (() => void) | null };
  const wakeB = { fn: null as (() => void) | null };
  return [make(a, b, wakeA, wakeB), make(b, a, wakeB, wakeA)];
}
