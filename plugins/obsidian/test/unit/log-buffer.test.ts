// LogBuffer — the ring buffer that backs the in-app log viewer (issue
// 0013). Focused on the few semantics the viewer relies on: bounded
// capacity (no leak on a long session), oldest-out FIFO eviction,
// subscriber fan-out (so the open modal streams live), error isolation
// between subscribers, and the `toText()` form that matches the existing
// `[context …]` / `[engine-worker …]` console format the user is already
// reading.

import { describe, expect, test } from 'bun:test';
import { LogBuffer, type LogEntry, ctxTimestamp } from '../../src/log-buffer.js';

const entry = (overrides: Partial<LogEntry> = {}): LogEntry => ({
  ts: '12:00:00.000',
  source: 'main',
  level: 'info',
  msg: 'hello',
  ...overrides,
});

describe('LogBuffer — capacity + ordering', () => {
  test('snapshot is empty until something is appended', () => {
    const buf = new LogBuffer(10);
    expect(buf.snapshot()).toEqual([]);
  });

  test('appended entries come back in insertion order', () => {
    const buf = new LogBuffer(10);
    buf.append(entry({ msg: 'a' }));
    buf.append(entry({ msg: 'b' }));
    buf.append(entry({ msg: 'c' }));
    expect(buf.snapshot().map((e) => e.msg)).toEqual(['a', 'b', 'c']);
  });

  test('over-capacity appends evict the oldest entries FIFO', () => {
    const buf = new LogBuffer(3);
    buf.append(entry({ msg: 'a' }));
    buf.append(entry({ msg: 'b' }));
    buf.append(entry({ msg: 'c' }));
    buf.append(entry({ msg: 'd' }));
    expect(buf.snapshot().map((e) => e.msg)).toEqual(['b', 'c', 'd']);
    buf.append(entry({ msg: 'e' }));
    buf.append(entry({ msg: 'f' }));
    expect(buf.snapshot().map((e) => e.msg)).toEqual(['d', 'e', 'f']);
  });

  test('clear() empties the buffer; appends still work after', () => {
    const buf = new LogBuffer(10);
    buf.append(entry({ msg: 'a' }));
    buf.clear();
    expect(buf.snapshot()).toEqual([]);
    buf.append(entry({ msg: 'b' }));
    expect(buf.snapshot().map((e) => e.msg)).toEqual(['b']);
  });
});

describe('LogBuffer — subscribers', () => {
  test('subscribe receives every append', () => {
    const buf = new LogBuffer(10);
    const got: string[] = [];
    buf.subscribe((e) => got.push(e.msg));
    buf.append(entry({ msg: 'a' }));
    buf.append(entry({ msg: 'b' }));
    expect(got).toEqual(['a', 'b']);
  });

  test('unsubscribe stops further delivery', () => {
    const buf = new LogBuffer(10);
    const got: string[] = [];
    const off = buf.subscribe((e) => got.push(e.msg));
    buf.append(entry({ msg: 'a' }));
    off();
    buf.append(entry({ msg: 'b' }));
    expect(got).toEqual(['a']);
  });

  test('a throwing subscriber does not block siblings or the buffer', () => {
    const buf = new LogBuffer(10);
    buf.subscribe(() => {
      throw new Error('boom');
    });
    const got: string[] = [];
    buf.subscribe((e) => got.push(e.msg));
    buf.append(entry({ msg: 'a' }));
    expect(got).toEqual(['a']);
    expect(buf.snapshot().length).toBe(1);
  });

  test('a subscriber added after some appends only sees future ones', () => {
    const buf = new LogBuffer(10);
    buf.append(entry({ msg: 'before' }));
    const got: string[] = [];
    buf.subscribe((e) => got.push(e.msg));
    buf.append(entry({ msg: 'after' }));
    expect(got).toEqual(['after']);
  });
});

describe('LogBuffer — toText (copy-all form)', () => {
  test('matches the existing [context …] / [engine-worker …] console format', () => {
    const buf = new LogBuffer(10);
    buf.append(entry({ ts: '12:00:00.001', source: 'main', msg: 'hello' }));
    buf.append(entry({ ts: '12:00:00.002', source: 'worker', msg: 'commit start' }));
    buf.append(
      entry({
        ts: '12:00:00.003',
        source: 'main',
        level: 'error',
        msg: 'engine worker error: x',
      }),
    );
    expect(buf.toText()).toBe(
      [
        '[context 12:00:00.001] hello',
        '[engine-worker 12:00:00.002] commit start',
        '[context 12:00:00.003] ERROR engine worker error: x',
      ].join('\n'),
    );
  });

  test('empty buffer → empty string (modal uses this to skip clipboard)', () => {
    expect(new LogBuffer(10).toText()).toBe('');
  });
});

describe('ctxTimestamp', () => {
  test('produces an HH:MM:SS.mmm string with zero-padding', () => {
    const fixed = new Date(2026, 0, 1, 5, 3, 9, 7);
    expect(ctxTimestamp(fixed)).toBe('05:03:09.007');
  });
});
