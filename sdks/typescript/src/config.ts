// `.context/config` model + a small, lossless TOML codec.
//
// This is the file the native `ctx` CLI and any TS consumer (the Obsidian
// plugin) share byte-for-byte (CSP spec.md §9.1/§17.1 — `ctx` is one
// front-end; the plugin is another). The codec implements only the TOML
// subset the schema needs — section tables, double-quoted strings,
// integers, booleans, string arrays — but it round-trips *unknown* tables
// and keys untouched, so a newer `ctx` adding fields never has them
// clobbered by an older plugin (and vice versa).
//
// PROVISIONAL: the canonical `.context/config` schema is CSP-owned and not
// yet frozen (obsidian-plugin-spec.md §14). The codec is generic; only the
// `CspConfig` projection below changes when CSP fixes the schema. The TOML
// codec itself is a verbatim port of the proven agentsync codec.

import type {
  CspConfig,
  IdentitySection,
  PeerSection,
  ScopeSection,
  TomlDoc,
  TomlValue,
} from './types.js';

export type { CspConfig, IdentitySection, PeerSection, ScopeSection, TomlDoc, TomlValue };

/** Matches the default scope (CSP spec §11 — text-only by default). */
export function defaultScopeSection(): ScopeSection {
  return { extensions: ['md', 'markdown'], include: [] };
}

export function defaultConfig(): CspConfig {
  return { peer: {}, identity: {}, scope: defaultScopeSection() };
}

// ---- Parser ----

/** Parse TOML into an ordered {@link TomlDoc}. Throws on input it can't
 * represent rather than guessing. */
export function parseTomlDoc(text: string): TomlDoc {
  const doc: TomlDoc = new Map();
  let table = ensureTable(doc, '');
  const lines = text.split('\n');

  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i] ?? '';
    const line = stripComment(raw).trim();
    if (line === '') continue;

    const header = /^\[([^\]]+)\]$/.exec(line);
    if (header) {
      table = ensureTable(doc, (header[1] ?? '').trim());
      continue;
    }

    const eq = line.indexOf('=');
    if (eq === -1) throw new Error(`.context/config: malformed line ${i + 1}: ${raw}`);
    const key = line.slice(0, eq).trim();
    let rhs = line.slice(eq + 1).trim();

    if (rhs.startsWith('[')) {
      while (!hasClosingBracket(rhs)) {
        i += 1;
        if (i >= lines.length) throw new Error('.context/config: unterminated array');
        rhs += `\n${lines[i]}`;
      }
      table.set(key, parseStringArray(rhs));
    } else {
      table.set(key, parseScalar(rhs));
    }
  }
  return doc;
}

function ensureTable(doc: TomlDoc, name: string): Map<string, TomlValue> {
  let t = doc.get(name);
  if (!t) {
    t = new Map();
    doc.set(name, t);
  }
  return t;
}

/** Drop a trailing `# comment` not inside a double-quoted string. */
function stripComment(line: string): string {
  let inStr = false;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (c === '"' && line[i - 1] !== '\\') inStr = !inStr;
    else if (c === '#' && !inStr) return line.slice(0, i);
  }
  return line;
}

function hasClosingBracket(s: string): boolean {
  return s.includes(']');
}

function parseScalar(s: string): TomlValue {
  if (s.startsWith('"')) return parseTomlString(s);
  if (s === 'true') return true;
  if (s === 'false') return false;
  if (/^[+-]?[0-9_]+$/.test(s)) {
    const n = Number(s.replace(/_/g, ''));
    if (!Number.isSafeInteger(n)) throw new Error(`.context/config: integer out of range: ${s}`);
    return n;
  }
  throw new Error(`.context/config: unsupported value: ${s}`);
}

function parseTomlString(s: string): string {
  if (!s.startsWith('"')) throw new Error(`.context/config: expected string, got: ${s}`);
  let out = '';
  for (let i = 1; i < s.length; i++) {
    const c = s.charAt(i);
    if (c === '"') return out;
    if (c !== '\\') {
      out += c;
      continue;
    }
    const e = s.charAt(i + 1);
    i += 1;
    if (e === 'n') out += '\n';
    else if (e === 't') out += '\t';
    else if (e === 'r') out += '\r';
    else if (e === '"') out += '"';
    else if (e === '\\') out += '\\';
    else if (e === 'u') {
      out += String.fromCharCode(Number.parseInt(s.slice(i + 1, i + 5), 16));
      i += 4;
    } else out += e;
  }
  throw new Error(`.context/config: unterminated string: ${s}`);
}

function parseStringArray(s: string): string[] {
  const inner = s.slice(s.indexOf('[') + 1, s.lastIndexOf(']'));
  const out: string[] = [];
  let i = 0;
  while (i < inner.length) {
    if (inner.charAt(i) !== '"') {
      i += 1;
      continue;
    }
    let j = i + 1;
    while (j < inner.length && !(inner.charAt(j) === '"' && inner.charAt(j - 1) !== '\\')) j++;
    out.push(parseTomlString(inner.slice(i, j + 1)));
    i = j + 1;
  }
  return out;
}

// ---- Serializer ----

function escapeTomlString(s: string): string {
  return s.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n').replace(/\t/g, '\\t');
}

function formatValue(v: TomlValue): string {
  if (typeof v === 'string') return `"${escapeTomlString(v)}"`;
  if (typeof v === 'number' || typeof v === 'boolean') return String(v);
  if (v.length === 0) return '[]';
  return `[\n${v.map((e) => `    "${escapeTomlString(e)}",`).join('\n')}\n]`;
}

/** Render a {@link TomlDoc} the way `toml::to_string_pretty` would. */
export function stringifyTomlDoc(doc: TomlDoc): string {
  const blocks: string[] = [];
  const root = doc.get('');
  if (root && root.size > 0) {
    blocks.push([...root].map(([k, v]) => `${k} = ${formatValue(v)}`).join('\n'));
  }
  for (const [name, table] of doc) {
    if (name === '') continue;
    const body = [...table].map(([k, v]) => `${k} = ${formatValue(v)}`);
    blocks.push([`[${name}]`, ...body].join('\n'));
  }
  return blocks.length ? `${blocks.join('\n\n')}\n` : '';
}

// ---- Schema mapping (lossless: unknown tables/keys survive) ----

function strOf(t: Map<string, TomlValue> | undefined, k: string): string | undefined {
  const v = t?.get(k);
  return typeof v === 'string' ? v : undefined;
}
function strArr(v: TomlValue | undefined, fallback: string[]): string[] {
  return Array.isArray(v) ? v.slice() : fallback;
}

/** Build an object with only the defined entries (keeps optional fields
 * genuinely absent under `exactOptionalPropertyTypes`). */
function compact<T extends object>(entries: Record<string, string | undefined>): T {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(entries)) if (v !== undefined) out[k] = v;
  return out as T;
}

/** Project a parsed doc onto the typed CSP schema (defaults filled). */
export function configFromDoc(doc: TomlDoc): CspConfig {
  const peer = doc.get('peer');
  const identity = doc.get('identity');
  const scope = doc.get('scope');
  const d = defaultScopeSection();
  return {
    peer: compact<PeerSection>({
      url: strOf(peer, 'url'),
      pubkey: strOf(peer, 'pubkey'),
    }),
    identity: compact<IdentitySection>({
      path: strOf(identity, 'path'),
    }),
    scope: {
      extensions: strArr(scope?.get('extensions'), d.extensions),
      include: strArr(scope?.get('include'), d.include),
    },
  };
}

/**
 * Write the typed schema back into `base` (a doc previously parsed from
 * disk, or empty), preserving any unknown tables/keys `ctx` may have
 * written. Optional `peer`/`identity` fields are removed when unset so we
 * don't persist empty `key = ""` lines.
 */
export function applyConfigToDoc(cfg: CspConfig, base?: TomlDoc): TomlDoc {
  const doc: TomlDoc = base ?? new Map();
  const put = (table: string, key: string, val: string | undefined): void => {
    const t = ensureTable(doc, table);
    if (val === undefined || val === '') t.delete(key);
    else t.set(key, val);
  };
  ensureTable(doc, 'peer');
  ensureTable(doc, 'identity');
  ensureTable(doc, 'scope');

  put('peer', 'url', cfg.peer.url);
  put('peer', 'pubkey', cfg.peer.pubkey);
  put('identity', 'path', cfg.identity.path);

  const scope = ensureTable(doc, 'scope');
  scope.set('extensions', cfg.scope.extensions.slice());
  scope.set('include', cfg.scope.include.slice());
  return doc;
}

/** Parse `.context/config` text into the typed schema + the raw doc (for
 * lossless re-serialization via {@link serializeConfig}). */
export function parseConfig(text: string): { config: CspConfig; doc: TomlDoc } {
  const doc = parseTomlDoc(text);
  return { config: configFromDoc(doc), doc };
}

/** Serialize the typed schema, layering it onto `baseDoc` if supplied so
 * unknown `ctx`-written content is preserved. */
export function serializeConfig(cfg: CspConfig, baseDoc?: TomlDoc): string {
  return stringifyTomlDoc(applyConfigToDoc(cfg, baseDoc));
}
