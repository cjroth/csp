import { describe, expect, test } from 'bun:test';
import { StatusBar, type SyncState } from '../../src/status-bar.js';
import { makeFakeHTMLElement } from '../mocks/obsidian.js';

const STATES: SyncState[] = ['idle', 'connecting', 'connected', 'reconnecting', 'error'];

describe('StatusBar', () => {
  test('constructor sets the base class and the idle label', () => {
    const el = makeFakeHTMLElement();
    new StatusBar(el);
    expect(el.hasClass('context-status')).toBe(true);
    expect(el.hasClass('context-state-idle')).toBe(true);
    expect(el._text()).toBe('Context: idle');
  });

  test('set() swaps the state class and updates the label for every state', () => {
    const el = makeFakeHTMLElement();
    const bar = new StatusBar(el);
    for (const s of STATES) {
      bar.set(s);
      expect(el._text()).toContain('Context');
      expect(el.hasClass(`context-state-${s}`)).toBe(true);
      // Exactly one state class is present at a time.
      const others = STATES.filter((x) => x !== s);
      for (const o of others) expect(el.hasClass(`context-state-${o}`)).toBe(false);
    }
  });

  test('set() with a detail appends it in parentheses', () => {
    const el = makeFakeHTMLElement();
    const bar = new StatusBar(el);
    bar.set('error', 'boom');
    expect(el._text()).toBe('Context: error (boom)');
    bar.set('connected');
    expect(el._text()).toBe('Context: connected');
  });

  test('onClick registers a clickable handler that fires on click', () => {
    const el = makeFakeHTMLElement();
    const bar = new StatusBar(el);
    let clicks = 0;
    bar.onClick(() => {
      clicks += 1;
    });
    expect(el.hasClass('mod-clickable')).toBe(true);
    el.dispatchEvent(new Event('click'));
    el.dispatchEvent(new Event('click'));
    expect(clicks).toBe(2);
  });
});
