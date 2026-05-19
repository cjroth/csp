import { describe, expect, test } from 'bun:test';
import {
  TEXT_EXTS,
  extOf,
  globToRegex,
  isContextInternal,
  isTextPath,
  matchesAnyGlob,
  shouldSync,
} from '../../src/path-filter.js';

describe('extOf', () => {
  test('returns lowercase extension', () => {
    expect(extOf('foo.MD')).toBe('md');
    expect(extOf('foo.png')).toBe('png');
  });
  test('returns empty string for no extension', () => {
    expect(extOf('foo')).toBe('');
    expect(extOf('foo/bar')).toBe('');
  });
  test('returns empty string for dotfiles', () => {
    expect(extOf('.gitignore')).toBe('');
    expect(extOf('foo/.env')).toBe('');
  });
  test('handles deep paths and uses last dot only', () => {
    expect(extOf('a/b/c.md')).toBe('md');
    expect(extOf('archive.tar.gz')).toBe('gz');
  });
});

describe('isTextPath', () => {
  test('returns true for every TEXT_EXTS entry', () => {
    for (const ext of TEXT_EXTS) {
      expect(isTextPath(`note.${ext}`)).toBe(true);
    }
  });
  test('returns false for binary extensions', () => {
    for (const ext of ['png', 'jpg', 'pdf', 'mp3', 'excalidraw', 'gif', 'mp4']) {
      expect(isTextPath(`x.${ext}`)).toBe(false);
    }
  });
  test('returns false for empty / extension-less', () => {
    expect(isTextPath('')).toBe(false);
    expect(isTextPath('LICENSE')).toBe(false);
  });
});

describe('isContextInternal (CSP §11 HARD INVARIANT)', () => {
  test('matches the .context dir and anything inside it', () => {
    expect(isContextInternal('.context')).toBe(true);
    expect(isContextInternal('.context/config')).toBe(true);
    expect(isContextInternal('.context/objects/ab')).toBe(true);
  });
  test('does not match .contextignore or unrelated paths', () => {
    expect(isContextInternal('.contextignore')).toBe(false);
    expect(isContextInternal('notes/.context-notes.md')).toBe(false);
  });
});

describe('globToRegex', () => {
  test('plain literal', () => {
    expect(globToRegex('hello.md').test('hello.md')).toBe(true);
    expect(globToRegex('hello.md').test('Xhello.md')).toBe(false);
  });
  test('single-* matches within a segment', () => {
    expect(globToRegex('Drafts/*.md').test('Drafts/foo.md')).toBe(true);
    expect(globToRegex('Drafts/*.md').test('Drafts/sub/foo.md')).toBe(false);
  });
  test('double-** crosses segments', () => {
    expect(globToRegex('Drafts/**').test('Drafts/sub/foo.md')).toBe(true);
    expect(globToRegex('**/foo.md').test('a/b/foo.md')).toBe(true);
  });
  test('? matches a single character', () => {
    expect(globToRegex('?.md').test('a.md')).toBe(true);
    expect(globToRegex('?.md').test('ab.md')).toBe(false);
  });
  test('escapes regex metacharacters', () => {
    expect(globToRegex('foo.bar+baz.md').test('foo.bar+baz.md')).toBe(true);
    expect(globToRegex('foo.bar+baz.md').test('fooXbarYbaz.md')).toBe(false);
  });
});

describe('matchesAnyGlob', () => {
  test('false on empty list', () => {
    expect(matchesAnyGlob('foo.md', [])).toBe(false);
  });
  test('true if any glob matches', () => {
    expect(matchesAnyGlob('Drafts/x.md', ['*.tmp', 'Drafts/**'])).toBe(true);
  });
  test('false if no glob matches; skips empty globs', () => {
    expect(matchesAnyGlob('Notes/x.md', ['Drafts/**'])).toBe(false);
    expect(matchesAnyGlob('foo.md', [''])).toBe(false);
  });
});

describe('shouldSync', () => {
  test('rejects empty path', () => {
    expect(shouldSync('', [])).toBe(false);
  });
  test('rejects anything under .context/ (CSP §11 HARD INVARIANT)', () => {
    expect(shouldSync('.context/config', [])).toBe(false);
    expect(shouldSync('.context/objects/ab', [])).toBe(false);
    // Even if an ignore-allow somehow named it, the invariant wins.
    expect(shouldSync('.context/state', [])).toBe(false);
  });
  test('syncs the shared .contextignore (CSP §11) despite no extension', () => {
    expect(shouldSync('.contextignore', [])).toBe(true);
  });
  test('rejects binary files even without ignore globs', () => {
    expect(shouldSync('img.png', [])).toBe(false);
  });
  test('rejects when an ignore glob matches', () => {
    expect(shouldSync('Drafts/foo.md', ['Drafts/**'])).toBe(false);
  });
  test('accepts text files when nothing matches', () => {
    expect(shouldSync('Notes/foo.md', ['Drafts/**'])).toBe(true);
    expect(shouldSync('foo.canvas', [])).toBe(true);
  });
  test('the .keep directory sentinel is in scope (CSP §11)', () => {
    expect(shouldSync('Empty/.keep', [])).toBe(true);
    expect(shouldSync('a/b/c/.keep', [])).toBe(true);
    expect(shouldSync('.keep', [])).toBe(true);
    // …but the HARD INVARIANT and ignore globs still win.
    expect(shouldSync('.context/x/.keep', [])).toBe(false);
    expect(shouldSync('Drafts/.keep', ['Drafts/**'])).toBe(false);
  });
});
