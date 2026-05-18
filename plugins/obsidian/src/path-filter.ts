// Decides which vault paths are in CSP scope. Pure functions, no Obsidian
// runtime dependency — fully unit-testable.
//
// CSP scope is an explicit ALLOWLIST (CSP spec.md §11): the failure mode is
// syncing too little, never exfiltrating secrets/build output. Rules:
//   1. Only text files (TEXT_EXTS) sync. Binaries are opt-in whole-file LWW
//      (CSP §11) and out of scope for the v1 plugin.
//   2. `.contextignore` is the synced, shared exclusion file (CSP §11) — it
//      has no text extension but IS in scope so the policy replicates.
//   3. `.context/` (CSP's own footprint) is excluded unconditionally —
//      HARD INVARIANT, CSP §11: never replicate/commit/expose it.
//   4. User ignore globs (`.contextignore` / node-local `.context/exclude`)
//      are applied on top; a match → skip.
//
// Glob syntax is an intentionally minimal, documented gitignore subset
// (`*`, `**`, `?`, literals). Full gitignore semantics are the engine's to
// own once the real SDK lands (obsidian-plugin-spec §7.8/§14); this host
// filter stays conservative.

/** File extensions we consider "text" — sync these. Lowercase, no dot. */
export const TEXT_EXTS: ReadonlySet<string> = new Set([
  'md',
  'mdx',
  'txt',
  'canvas',
  'json',
  'css',
  'yaml',
  'yml',
  'csv',
]);

/** CSP's own state dir — never in scope (CSP §11 HARD INVARIANT). */
const CONTEXT_DIR = '.context';

/** Synced shared exclusion file (CSP §11) — always eligible despite having
 * no text extension, so the exclusion policy replicates between nodes. */
const SYNCED_CONTROL_FILES: ReadonlySet<string> = new Set(['.contextignore']);

/** Return the lowercase extension (no dot) of `path`, or '' if none. */
export function extOf(path: string): string {
  const slash = path.lastIndexOf('/');
  const base = slash === -1 ? path : path.slice(slash + 1);
  const dot = base.lastIndexOf('.');
  if (dot <= 0) return '';
  return base.slice(dot + 1).toLowerCase();
}

/** True if `path` ends in a text extension we sync. */
export function isTextPath(path: string): boolean {
  if (!path) return false;
  return TEXT_EXTS.has(extOf(path));
}

/** True if `path` is inside CSP's own `.context/` footprint. */
export function isContextInternal(path: string): boolean {
  return path === CONTEXT_DIR || path.startsWith(`${CONTEXT_DIR}/`);
}

/**
 * Compile a glob to a regex. Exposed for testing; callers should use
 * `matchesAnyGlob` or `shouldSync`.
 */
export function globToRegex(glob: string): RegExp {
  let re = '^';
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i] as string;
    if (c === '*') {
      if (glob[i + 1] === '*') {
        re += '.*';
        i++;
      } else {
        re += '[^/]*';
      }
    } else if (c === '?') {
      re += '[^/]';
    } else if ('\\^$+.()|{}[]'.includes(c)) {
      re += `\\${c}`;
    } else {
      re += c;
    }
  }
  re += '$';
  return new RegExp(re);
}

/** True if `path` matches any of `globs`. Empty list returns false. */
export function matchesAnyGlob(path: string, globs: readonly string[]): boolean {
  for (const g of globs) {
    if (!g) continue;
    if (globToRegex(g).test(path)) return true;
  }
  return false;
}

/**
 * Top-level decision: is this path in CSP scope? Combines the text-extension
 * allowlist, the synced control-file allowance, the `.context/` HARD
 * INVARIANT exclusion, and user ignore globs (CSP §11).
 */
export function shouldSync(path: string, ignoreGlobs: readonly string[]): boolean {
  if (!path) return false;
  if (isContextInternal(path)) return false;
  if (matchesAnyGlob(path, ignoreGlobs)) return false;
  if (SYNCED_CONTROL_FILES.has(path)) return true;
  if (!isTextPath(path)) return false;
  return true;
}
