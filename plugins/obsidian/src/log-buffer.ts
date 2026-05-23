// In-memory ring buffer of recent `[context …]` + `[engine-worker …]`
// trace lines, plus host-side errors (issue 0013). Backs the in-app log
// viewer — on iOS Obsidian the WebView dev console is unreachable, so the
// only way to read what the plugin is doing is to capture it ourselves.
//
// Bounded so a long-running session can't grow unbounded; ordered oldest
// first. Subscribers are notified on every append so an open modal can
// stream live.

/** Single captured line. */
export interface LogEntry {
  /** `HH:MM:SS.mmm` wall clock at capture. */
  ts: string;
  /** Which side of the channel produced it. `main` covers `ctxLog`/
   * `ctxErr` (Obsidian-process lifecycle, Obsidian events, the sync
   * controller). `worker` covers the engine worker's mirrored
   * `console.log`/`console.error` (frame round-trips, commit/persist,
   * session_feed timings). Letting the UI label rows by source makes a
   * mobile-side capture readable without needing two separate viewers. */
  source: 'main' | 'worker';
  level: 'info' | 'error';
  msg: string;
}

/** Single shared ring buffer for the plugin. ~500 lines × ~120 chars ≈ 60
 * KB — fine to keep in memory; not persisted (a relaunch clears it, which
 * matches the user's mental model: "show me what's happened since I
 * started watching"). */
export class LogBuffer {
  private readonly entries: LogEntry[] = [];
  private readonly subscribers = new Set<(e: LogEntry) => void>();

  constructor(private readonly capacity = 500) {}

  /** Append an entry, evict the oldest when at capacity, fan out. */
  append(entry: LogEntry): void {
    this.entries.push(entry);
    if (this.entries.length > this.capacity) {
      this.entries.shift();
    }
    for (const fn of this.subscribers) {
      try {
        fn(entry);
      } catch {
        // a subscriber throwing must not break the others or the buffer
      }
    }
  }

  /** Stream future appends. Returns an unsubscribe. */
  subscribe(fn: (e: LogEntry) => void): () => void {
    this.subscribers.add(fn);
    return () => this.subscribers.delete(fn);
  }

  /** A snapshot of the current entries, oldest first. */
  snapshot(): LogEntry[] {
    return this.entries.slice();
  }

  clear(): void {
    this.entries.length = 0;
  }

  /** Plain-text dump matching the existing console form, one line per
   * entry — what the "Copy all" button puts on the clipboard so the user
   * can paste a capture into a chat with no extra reformatting:
   *
   *   [context HH:MM:SS.mmm] message
   *   [engine-worker HH:MM:SS.mmm] message
   *   [context HH:MM:SS.mmm] ERROR message
   */
  toText(): string {
    return this.entries
      .map((e) => {
        const tag = e.source === 'main' ? 'context' : 'engine-worker';
        const lvl = e.level === 'error' ? 'ERROR ' : '';
        return `[${tag} ${e.ts}] ${lvl}${e.msg}`;
      })
      .join('\n');
  }
}

/** `HH:MM:SS.mmm` formatter — shared so worker-mirrored lines and
 * host-side lines time-align in the buffer. */
export function ctxTimestamp(d: Date = new Date()): string {
  const p = (n: number, w = 2) => String(n).padStart(w, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`;
}
