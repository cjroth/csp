// Hand-rolled in-memory fakes for the slice of `obsidian` the plugin
// touches. Used by both unit and e2e tests; intentionally minimal — we
// match the production behaviors that matter for sync (event ordering,
// path normalization, atomicity) and ignore the rest. SDK-agnostic: a
// verbatim port of the agentsync plugin's mock (no agentsync coupling)
// plus a `list()` for the CSP object-store adapter.

import type { MinimalVault } from '../../src/bridge.js';
import type { MinimalDataAdapter } from '../../src/storage-adapter.js';

// ---------- TFile / TFolder / TAbstractFile ----------

export class FakeTAbstractFile {
  constructor(public path: string) {}

  get name(): string {
    const slash = this.path.lastIndexOf('/');
    return slash === -1 ? this.path : this.path.slice(slash + 1);
  }
}

export class FakeTFile extends FakeTAbstractFile {
  get extension(): string {
    const dot = this.name.lastIndexOf('.');
    return dot === -1 ? '' : this.name.slice(dot + 1);
  }
  get basename(): string {
    const dot = this.name.lastIndexOf('.');
    return dot === -1 ? this.name : this.name.slice(0, dot);
  }
}

export class FakeTFolder extends FakeTAbstractFile {
  children: FakeTAbstractFile[] = [];
}

// ---------- DataAdapter ----------

export class FakeDataAdapter implements MinimalDataAdapter {
  /** path → content (string for text, ArrayBuffer for binary) or null for dir. */
  private store = new Map<string, string | ArrayBuffer | null>();

  async read(path: string): Promise<string> {
    const v = this.store.get(path);
    if (v == null) throw new Error(`ENOENT: ${path}`);
    if (typeof v !== 'string') throw new Error(`not text: ${path}`);
    return v;
  }
  async readBinary(path: string): Promise<ArrayBuffer> {
    const v = this.store.get(path);
    if (v == null) throw new Error(`ENOENT: ${path}`);
    if (typeof v === 'string') {
      const bytes = new TextEncoder().encode(v);
      return bytes.buffer.slice(
        bytes.byteOffset,
        bytes.byteOffset + bytes.byteLength,
      ) as ArrayBuffer;
    }
    return v;
  }
  async write(path: string, data: string): Promise<void> {
    this.store.set(path, data);
  }
  async writeBinary(path: string, data: ArrayBuffer): Promise<void> {
    this.store.set(path, data.slice(0));
  }
  async exists(path: string): Promise<boolean> {
    return this.store.has(path);
  }
  async mkdir(path: string): Promise<void> {
    this.store.set(path, null);
  }
  async remove(path: string): Promise<void> {
    if (!this.store.has(path)) throw new Error(`ENOENT: ${path}`);
    this.store.delete(path);
  }
  async rename(oldPath: string, newPath: string): Promise<void> {
    if (!this.store.has(oldPath)) throw new Error(`ENOENT: ${oldPath}`);
    const v = this.store.get(oldPath);
    this.store.delete(oldPath);
    this.store.set(newPath, v ?? null);
  }
  /** Direct children of `path`, classified by stored value type. */
  async list(path: string): Promise<{ files: string[]; folders: string[] }> {
    const prefix = `${path}/`;
    const files: string[] = [];
    const folders: string[] = [];
    for (const [k, v] of this.store) {
      if (!k.startsWith(prefix)) continue;
      const rest = k.slice(prefix.length);
      if (rest.length === 0 || rest.includes('/')) continue; // not a direct child
      if (v === null) folders.push(k);
      else files.push(k);
    }
    return { files, folders };
  }

  /** Test helper — list everything stored. */
  entries(): Array<[string, string | ArrayBuffer | null]> {
    return Array.from(this.store.entries());
  }
}

// ---------- Vault + EventBus ----------

type VaultEventName = 'create' | 'modify' | 'delete' | 'rename';
type VaultEventHandler = (file: FakeTAbstractFile, oldPath?: string) => void;

export class FakeEventRef {
  constructor(
    public readonly name: VaultEventName,
    public readonly handler: VaultEventHandler,
  ) {}
}

/**
 * How FakeVault fires events for folder operations. Real Obsidian (verified
 * against community plugins / forum posts):
 *   - `real-obsidian` (default): a folder rename fires `rename` for the folder
 *     FIRST, then `rename` for every descendant file with its new/old path; a
 *     folder delete fires `delete` for every descendant file FIRST, then a
 *     `delete` for the folder itself. Recursive.
 *   - `per-child-only`: no folder-level event; per-child events only. Some
 *     plugins / older builds skip the folder-level event.
 *   - `folder-only`: legacy assumption the bridge used to bake in — only the
 *     folder-level event fires, no per-child events. Kept so we can demonstrate
 *     the bridge still handles it for backward compat.
 *   - `external`: the move/delete happened outside Obsidian (e.g. on disk).
 *     Renames decompose into `create` (new path) + `delete` (old path);
 *     deletes still fire per-child + folder.
 */
export type FolderEventStrategy = 'real-obsidian' | 'per-child-only' | 'folder-only' | 'external';

export class FakeVault implements MinimalVault {
  private files = new Map<string, FakeTFile>();
  private folders = new Map<string, FakeTFolder>();
  private listeners = new Map<VaultEventName, Set<VaultEventHandler>>();

  /** Folder-op event strategy. Mutable so a single vault instance can flip
   * strategies between tests. Defaults to the real-Obsidian behavior. */
  folderEventStrategy: FolderEventStrategy = 'real-obsidian';

  /** Stale-cache mode for `getAbstractFileByPath`: paths in this set return
   * null even though the file exists. Used to reproduce the Obsidian metadata
   * cache returning null while the file is still physically present. */
  staleLookupPaths = new Set<string>();

  /** Capture every event the vault fires (after suppression-free dispatch),
   * in order. Tests use this to assert event ordering matches real Obsidian. */
  readonly emitted: Array<{ name: VaultEventName; path: string; oldPath?: string }> = [];

  constructor(public readonly adapter: FakeDataAdapter = new FakeDataAdapter()) {}

  // Obsidian's app.vault.configDir — points at the dot-prefixed config root.
  configDir = '.obsidian';

  // ---- Read APIs ----

  getFiles(): FakeTFile[] {
    return Array.from(this.files.values());
  }

  getMarkdownFiles(): FakeTFile[] {
    return this.getFiles().filter((f) => f.extension === 'md');
  }

  getAbstractFileByPath(path: string): FakeTAbstractFile | null {
    if (this.staleLookupPaths.has(path)) return null;
    return this.files.get(path) ?? this.folders.get(path) ?? null;
  }

  async read(file: FakeTFile): Promise<string> {
    return this.adapter.read(file.path);
  }

  // ---- Write APIs ----

  async create(path: string, data: string): Promise<FakeTFile> {
    if (this.files.has(path)) throw new Error(`exists: ${path}`);
    const f = new FakeTFile(path);
    this.files.set(path, f);
    this.emit('create', f);
    await this.adapter.write(path, data);
    return f;
  }

  async modify(file: FakeTFile, data: string): Promise<void> {
    if (!this.files.has(file.path)) throw new Error(`not found: ${file.path}`);
    this.emit('modify', file);
    await this.adapter.write(file.path, data);
  }

  async delete(file: FakeTAbstractFile): Promise<void> {
    if (this.files.has(file.path)) {
      this.files.delete(file.path);
      this.emit('delete', file);
      try {
        await this.adapter.remove(file.path);
      } catch {}
      return;
    }
    if (this.folders.has(file.path)) {
      // Real Obsidian fires `delete` for each descendant file first, then the
      // folder itself. Snapshot the descendants BEFORE mutating internal maps
      // so emission ordering is deterministic.
      const prefix = `${file.path}/`;
      const childFiles = [...this.files.values()].filter((f) => f.path.startsWith(prefix));
      const childFolders = [...this.folders.values()].filter(
        (f) => f.path !== file.path && f.path.startsWith(prefix),
      );

      // Remove from internal maps (sync) then emit events (sync) BEFORE the
      // async adapter removes — matches real Obsidian's order.
      for (const f of childFiles) this.files.delete(f.path);
      for (const f of childFolders) this.folders.delete(f.path);
      this.folders.delete(file.path);

      const strategy = this.folderEventStrategy;
      if (strategy === 'real-obsidian' || strategy === 'per-child-only') {
        for (const f of childFiles) this.emit('delete', f);
      }
      if (strategy !== 'per-child-only') this.emit('delete', file);

      // Adapter removes (async). Errors swallowed — best-effort.
      for (const f of childFiles) {
        try {
          await this.adapter.remove(f.path);
        } catch {}
      }
      return;
    }
    throw new Error(`not found: ${file.path}`);
  }

  async rename(file: FakeTAbstractFile, newPath: string): Promise<void> {
    const oldPath = file.path;
    if (this.files.has(oldPath)) {
      const f = this.files.get(oldPath) as FakeTFile;
      this.files.delete(oldPath);
      f.path = newPath;
      this.files.set(newPath, f);
      // Real Obsidian fires the rename event synchronously after the
      // in-memory rename and BEFORE the on-disk operation completes (per
      // forum threads documenting "the rename event fires before the file
      // operation completes"). Modelling that ordering matters: it lets the
      // bridge's event handler enqueue itself before any other async work
      // the test starts in the same turn.
      if (this.folderEventStrategy === 'external') {
        const tmpOld = new FakeTFile(oldPath);
        this.emit('create', f);
        this.emit('delete', tmpOld);
      } else {
        this.emit('rename', f, oldPath);
      }
      try {
        await this.adapter.rename(oldPath, newPath);
      } catch {}
      return;
    }
    if (this.folders.has(oldPath)) {
      // Snapshot descendants before mutating state, so emission order is
      // deterministic.
      const prefix = `${oldPath}/`;
      const childFiles: Array<{ child: FakeTFile; oldChildPath: string; newChildPath: string }> =
        [];
      for (const p of [...this.files.keys()]) {
        if (!p.startsWith(prefix)) continue;
        const child = this.files.get(p) as FakeTFile;
        const np = `${newPath}${p.slice(oldPath.length)}`;
        childFiles.push({ child, oldChildPath: p, newChildPath: np });
      }
      // Re-key files in the in-memory map (sync).
      for (const { child, oldChildPath, newChildPath } of childFiles) {
        this.files.delete(oldChildPath);
        child.path = newChildPath;
        this.files.set(newChildPath, child);
      }
      // Re-key folders (folder itself + nested subfolders).
      for (const p of [...this.folders.keys()]) {
        if (p !== oldPath && !p.startsWith(prefix)) continue;
        const fol = this.folders.get(p) as FakeTFolder;
        const np = p === oldPath ? newPath : `${newPath}${p.slice(oldPath.length)}`;
        this.folders.delete(p);
        fol.path = np;
        this.folders.set(np, fol);
      }

      // Emit per the configured strategy, BEFORE the adapter renames (real
      // Obsidian fires events synchronously after the in-memory rename).
      const strategy = this.folderEventStrategy;
      if (strategy === 'external') {
        for (const { child, oldChildPath } of childFiles) {
          this.emit('create', child);
          this.emit('delete', new FakeTFile(oldChildPath));
        }
      } else {
        if (strategy !== 'per-child-only') this.emit('rename', file, oldPath);
        if (strategy === 'real-obsidian' || strategy === 'per-child-only') {
          for (const { child, oldChildPath } of childFiles) {
            this.emit('rename', child, oldChildPath);
          }
        }
      }

      // Now perform the on-disk renames (async). Failures are swallowed —
      // the adapter rename is best-effort in the fake.
      for (const { oldChildPath, newChildPath } of childFiles) {
        try {
          await this.adapter.rename(oldChildPath, newChildPath);
        } catch {}
      }
      return;
    }
    throw new Error(`not found: ${oldPath}`);
  }

  async createFolder(path: string): Promise<FakeTFolder> {
    const existing = this.folders.get(path);
    if (existing) return existing;
    const f = new FakeTFolder(path);
    this.folders.set(path, f);
    return f;
  }

  // ---- Events ----

  on(name: VaultEventName, handler: VaultEventHandler): FakeEventRef {
    let set = this.listeners.get(name);
    if (!set) {
      set = new Set();
      this.listeners.set(name, set);
    }
    set.add(handler);
    return new FakeEventRef(name, handler);
  }

  offref(ref: FakeEventRef): void {
    this.listeners.get(ref.name)?.delete(ref.handler);
  }

  private emit(name: VaultEventName, file: FakeTAbstractFile, oldPath?: string): void {
    const record: { name: VaultEventName; path: string; oldPath?: string } = {
      name,
      path: file.path,
    };
    if (oldPath !== undefined) record.oldPath = oldPath;
    this.emitted.push(record);
    const set = this.listeners.get(name);
    if (!set) return;
    for (const h of [...set]) {
      try {
        h(file, oldPath);
      } catch {
        // Listener errors don't propagate — matches Obsidian's behavior.
      }
    }
  }

  /** Test helper — number of registered listeners across all events. */
  listenerCount(): number {
    let n = 0;
    for (const set of this.listeners.values()) n += set.size;
    return n;
  }
}

// ---------- App ----------

export class FakeApp {
  vault: FakeVault;
  setting = {
    open: () => {},
    openTabById: (_id: string) => {},
  };

  constructor(vault?: FakeVault) {
    this.vault = vault ?? new FakeVault();
  }
}

// ---------- Plugin / Notice ----------

export interface FakePluginManifest {
  id: string;
  name: string;
  version: string;
}

export class FakePlugin {
  app: FakeApp;
  manifest: FakePluginManifest;
  private data: unknown = null;
  private commands: Array<{ id: string; name: string; cb: () => void }> = [];
  private statusItems: HTMLElement[] = [];
  private settingTabs: unknown[] = [];
  private events: FakeEventRef[] = [];

  constructor(app: FakeApp, manifest: FakePluginManifest = defaultManifest()) {
    this.app = app;
    this.manifest = manifest;
  }

  async loadData(): Promise<unknown> {
    return this.data;
  }

  async saveData(d: unknown): Promise<void> {
    this.data = d;
  }

  registerEvent(ref: FakeEventRef): void {
    this.events.push(ref);
  }

  addCommand(c: { id: string; name: string; callback: () => void }): void {
    this.commands.push({ id: c.id, name: c.name, cb: c.callback });
  }

  addStatusBarItem(): HTMLElement {
    const el = makeFakeHTMLElement();
    this.statusItems.push(el);
    return el;
  }

  addSettingTab(tab: unknown): void {
    this.settingTabs.push(tab);
  }

  /** Test helper — invoke a registered command by id. */
  invokeCommand(id: string): void {
    const c = this.commands.find((x) => x.id === id);
    if (!c) throw new Error(`no such command: ${id}`);
    c.cb();
  }

  /** Test helper — list registered command ids. */
  commandIds(): string[] {
    return this.commands.map((c) => c.id);
  }

  /** Test helper — get the most recently-added status bar item. */
  lastStatusBarItem(): HTMLElement | null {
    return this.statusItems[this.statusItems.length - 1] ?? null;
  }

  /** Test helper — fire onunload to release events. */
  async simulateUnload(): Promise<void> {
    for (const ref of this.events) {
      this.app.vault.offref(ref);
    }
    this.events = [];
  }
}

function defaultManifest(): FakePluginManifest {
  return { id: 'context-sync', name: 'Context', version: '0.0.0' };
}

export const noticeLog: string[] = [];
export class FakeNotice {
  constructor(public message: string) {
    noticeLog.push(message);
  }
}

// ---------- Minimal HTMLElement shim for status bar / DOM ops ----------

interface FakeElementMethods {
  setText(t: string): void;
  addClass(c: string): void;
  removeClass(c: string): void;
  hasClass(c: string): boolean;
  empty(): void;
  /** Test accessors — current text + class set. */
  _text(): string;
  _classes: Set<string>;
}

export type FakeHTMLElement = HTMLElement & FakeElementMethods;

export function makeFakeHTMLElement(): FakeHTMLElement {
  const classes = new Set<string>();
  let text = '';
  const handlers = new Map<string, Set<(e: Event) => void>>();
  const el: Partial<FakeHTMLElement> & { _classes: Set<string>; _text(): string } = {
    _classes: classes,
    _text: () => text,
    setText(t: string) {
      text = t;
    },
    addClass(c: string) {
      classes.add(c);
    },
    removeClass(c: string) {
      classes.delete(c);
    },
    hasClass(c: string) {
      return classes.has(c);
    },
    empty() {
      text = '';
      classes.clear();
    },
    addEventListener(name: string, h: (e: Event) => void) {
      let s = handlers.get(name);
      if (!s) {
        s = new Set();
        handlers.set(name, s);
      }
      s.add(h);
    },
    removeEventListener(name: string, h: (e: Event) => void) {
      handlers.get(name)?.delete(h);
    },
    dispatchEvent(e: Event) {
      const set = handlers.get(e.type);
      if (set) {
        for (const h of set) h(e);
      }
      return true;
    },
  };
  return el as FakeHTMLElement;
}

/** Reset all module-level capture buffers. Call in `beforeEach`. */
export function resetMocks(): void {
  noticeLog.length = 0;
}
