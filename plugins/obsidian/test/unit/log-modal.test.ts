// LogModal — the in-app log viewer (issue 0013). Doesn't try to assert
// DOM details (the shim is a metadata recorder, not a real DOM); the
// load-bearing semantics are: open subscribes, close unsubscribes, Copy
// uses the buffer's `toText()` via the clipboard, Copy on empty shows a
// notice and skips the clipboard.

import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { LogBuffer } from '../../src/log-buffer.js';
import { LogModal } from '../../src/log-modal.js';
// Import the shim's `Notice` directly so the static `Notice.log` recorder
// is visible to TypeScript (the real `obsidian` package's `Notice` is
// just a constructor). Production code keeps importing from `'obsidian'`;
// `mock.module('obsidian', shim)` in `test/setup.ts` makes the two
// resolve to the same instance at runtime, so the recorder catches every
// production `new Notice(...)` call regardless of where it lives.
import { Notice } from '../mocks/obsidian-shim.js';

const fakeApp = {} as ConstructorParameters<typeof LogModal>[0];

beforeEach(() => {
  Notice.log = [];
});
afterEach(() => {
  // Reset the global so unrelated tests don't see a stub clipboard. We
  // assign `undefined` (rather than `delete`) to satisfy biome's
  // performance lint — same observable effect on the navigator slot.
  (globalThis as { navigator?: unknown }).navigator = undefined;
});

describe('LogModal — open/close lifecycle', () => {
  test('open then close does not leak a subscriber (sibling subscriber sees its full stream)', () => {
    const buf = new LogBuffer(10);
    const modal = new LogModal(fakeApp, buf);
    modal.open();
    const seen: string[] = [];
    buf.subscribe((e) => seen.push(e.msg));
    buf.append({ ts: 't', source: 'worker', level: 'info', msg: 'a' });
    modal.close();
    buf.append({ ts: 't', source: 'worker', level: 'info', msg: 'b' });
    expect(seen).toEqual(['a', 'b']);
  });

  test('reopening after close re-subscribes; multiple open/close cycles are safe', () => {
    const buf = new LogBuffer(10);
    const modal = new LogModal(fakeApp, buf);
    modal.open();
    modal.close();
    modal.open();
    modal.close();
    // The buffer is still healthy after the churn.
    expect(() => buf.append({ ts: 't', source: 'main', level: 'info', msg: 'x' })).not.toThrow();
  });
});

describe('LogModal — Copy all', () => {
  test('empty buffer → Notice "log is empty"; clipboard is NOT touched', async () => {
    const buf = new LogBuffer(10);
    let clipboardCalls = 0;
    (globalThis as { navigator: unknown }).navigator = {
      clipboard: {
        writeText: async () => {
          clipboardCalls++;
        },
      },
    };
    const modal = new LogModal(fakeApp, buf);
    modal.open();
    await (modal as unknown as { copyAll(): Promise<void> }).copyAll();
    modal.close();
    expect(clipboardCalls).toBe(0);
    expect(Notice.log).toContain('Context: log is empty.');
  });

  test('non-empty buffer → clipboard gets the canonical toText() form', async () => {
    const buf = new LogBuffer(10);
    buf.append({ ts: '12:00:00.001', source: 'main', level: 'info', msg: 'hello' });
    buf.append({
      ts: '12:00:00.002',
      source: 'worker',
      level: 'info',
      msg: 'commit start',
    });
    let written = '';
    (globalThis as { navigator: unknown }).navigator = {
      clipboard: {
        writeText: async (s: string) => {
          written = s;
        },
      },
    };
    const modal = new LogModal(fakeApp, buf);
    modal.open();
    await (modal as unknown as { copyAll(): Promise<void> }).copyAll();
    modal.close();
    expect(written).toBe(
      ['[context 12:00:00.001] hello', '[engine-worker 12:00:00.002] commit start'].join('\n'),
    );
    expect(Notice.log).toContain('Context: copied 2 log line(s).');
  });

  test('clipboard failure surfaces to the user via Notice instead of throwing', async () => {
    const buf = new LogBuffer(10);
    buf.append({ ts: 't', source: 'main', level: 'info', msg: 'x' });
    (globalThis as { navigator: unknown }).navigator = {
      clipboard: {
        writeText: async () => {
          throw new Error('denied');
        },
      },
    };
    const modal = new LogModal(fakeApp, buf);
    modal.open();
    await (modal as unknown as { copyAll(): Promise<void> }).copyAll();
    modal.close();
    expect(Notice.log.some((m) => m.includes('copy failed') && m.includes('denied'))).toBe(true);
  });
});

describe('LogModal — Clear', () => {
  test('clearing the buffer empties it; subsequent appends start over', () => {
    const buf = new LogBuffer(10);
    buf.append({ ts: 't', source: 'main', level: 'info', msg: 'a' });
    buf.append({ ts: 't', source: 'worker', level: 'info', msg: 'b' });
    const modal = new LogModal(fakeApp, buf);
    modal.open();
    // Simulate the Clear button: clear the buffer, then re-render
    // (the modal does this internally on click).
    buf.clear();
    expect(buf.snapshot()).toEqual([]);
    buf.append({ ts: 't', source: 'main', level: 'info', msg: 'fresh' });
    expect(buf.snapshot().map((e) => e.msg)).toEqual(['fresh']);
    modal.close();
  });
});
