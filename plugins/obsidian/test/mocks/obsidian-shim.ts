// Runtime stub for the `obsidian` npm package, which ships only type
// declarations and would error at import time inside Bun. We re-export
// minimal class shapes that make the production code (Plugin / Modal /
// Setting / etc.) executable inside tests.
//
// `test/setup.ts` redirects bare `obsidian` imports here via Bun.plugin so
// any test that imports (transitively) from a plugin source file gets these
// implementations instead of the empty `obsidian` package.
//
// Beyond making the code *run*, this shim is a faithful UI **recorder**: the
// settings tab and plugin lifecycle are pure UI orchestration, so to test
// them we capture every `Setting`, its components, and their change/click
// handlers, plus every `createEl`. Tests drive the real production code,
// then read back / invoke what it registered (see `__obsidian` helpers).

// ---------- Notice ----------

export class Notice {
  static log: string[] = [];
  constructor(public message: string) {
    Notice.log.push(message);
  }
}

// ---------- Recorder model ----------

export interface RecordedComponent {
  kind: 'text' | 'textarea' | 'toggle' | 'button' | 'dropdown';
  value?: string;
  placeholder?: string;
  disabled?: boolean;
  toggleValue?: boolean;
  buttonText?: string;
  tooltip?: string;
  cta?: boolean;
  warning?: boolean;
  options?: Array<[string, string]>;
  selected?: string;
  onChangeText?: (v: string) => unknown;
  onChangeToggle?: (v: boolean) => unknown;
  onChangeSelect?: (v: string) => unknown;
  onClick?: () => unknown;
}

export interface RecordedSetting {
  name: string;
  desc: string;
  components: RecordedComponent[];
}

export interface RecordedEl {
  tag: string;
  text: string;
  cls: string;
}

let recordedSettings: RecordedSetting[] = [];
let recordedEls: RecordedEl[] = [];

/** Reset all capture buffers. Call in `beforeEach`. (Note: `display()` also
 * resets the Setting/createEl buffers via `containerEl.empty()`, so a test
 * inspecting the latest render does not need to reset first; resetting
 * `Notice.log` between actions is still the test's job.) */
export function __resetObsidian(): void {
  recordedSettings = [];
  recordedEls = [];
  Notice.log = [];
}

export const __obsidian = {
  settings: (): RecordedSetting[] => recordedSettings,
  /** Last Setting whose name equals `name`. */
  setting: (name: string): RecordedSetting | undefined =>
    [...recordedSettings].reverse().find((s) => s.name === name),
  /** All components of a kind across every recorded Setting. */
  components: (kind: RecordedComponent['kind']): RecordedComponent[] =>
    recordedSettings.flatMap((s) => s.components.filter((c) => c.kind === kind)),
  /** First button whose text matches `text` (string or regexp). */
  button: (text: string | RegExp): RecordedComponent | undefined =>
    recordedSettings
      .flatMap((s) => s.components)
      .find(
        (c) =>
          c.kind === 'button' &&
          c.buttonText !== undefined &&
          (typeof text === 'string' ? c.buttonText === text : text.test(c.buttonText)),
      ),
  els: (): RecordedEl[] => recordedEls,
  /** True if any createEl text contains `needle`. */
  hasText: (needle: string): boolean => recordedEls.some((e) => e.text.includes(needle)),
  /** First createEl whose text contains `needle`. */
  el: (needle: string): RecordedEl | undefined => recordedEls.find((e) => e.text.includes(needle)),
};

// ---------- Component stubs (bound to a RecordedComponent) ----------

export class TextComponent {
  inputEl = makeStubEl();
  constructor(private readonly rec: RecordedComponent = { kind: 'text' }) {}
  setPlaceholder(p: string): this {
    this.rec.placeholder = p;
    return this;
  }
  setValue(v: string): this {
    this.rec.value = v;
    return this;
  }
  getValue(): string {
    return this.rec.value ?? '';
  }
  setDisabled(b: boolean): this {
    this.rec.disabled = b;
    return this;
  }
  onChange(cb: (v: string) => unknown): this {
    this.rec.onChangeText = cb;
    return this;
  }
}

export class ToggleComponent {
  constructor(private readonly rec: RecordedComponent = { kind: 'toggle' }) {}
  setValue(b: boolean): this {
    this.rec.toggleValue = b;
    return this;
  }
  setDisabled(_: boolean): this {
    return this;
  }
  onChange(cb: (v: boolean) => unknown): this {
    this.rec.onChangeToggle = cb;
    return this;
  }
}

export class ButtonComponent {
  buttonEl = makeStubEl();
  constructor(private readonly rec: RecordedComponent = { kind: 'button' }) {}
  setButtonText(t: string): this {
    this.rec.buttonText = t;
    return this;
  }
  setTooltip(t: string): this {
    this.rec.tooltip = t;
    return this;
  }
  setCta(): this {
    this.rec.cta = true;
    return this;
  }
  setWarning(): this {
    this.rec.warning = true;
    return this;
  }
  setDisabled(b: boolean): this {
    this.rec.disabled = b;
    return this;
  }
  onClick(h: () => unknown): this {
    this.rec.onClick = h;
    return this;
  }
  click(): void {
    void this.rec.onClick?.();
  }
}

export class DropdownComponent {
  constructor(private readonly rec: RecordedComponent = { kind: 'dropdown', options: [] }) {
    this.rec.options ??= [];
  }
  addOption(v: string, l: string): this {
    this.rec.options?.push([v, l]);
    return this;
  }
  setValue(v: string): this {
    this.rec.selected = v;
    return this;
  }
  onChange(cb: (v: string) => unknown): this {
    this.rec.onChangeSelect = cb;
    return this;
  }
}

export class Setting {
  private readonly rec: RecordedSetting = { name: '', desc: '', components: [] };
  constructor(public containerEl: unknown) {
    recordedSettings.push(this.rec);
  }
  setName(n: string): this {
    this.rec.name = n;
    return this;
  }
  setDesc(d: string): this {
    this.rec.desc = d;
    return this;
  }
  setHeading(): this {
    return this;
  }
  private push(kind: RecordedComponent['kind']): RecordedComponent {
    const c: RecordedComponent = { kind };
    this.rec.components.push(c);
    return c;
  }
  addText(cb: (t: TextComponent) => void): this {
    cb(new TextComponent(this.push('text')));
    return this;
  }
  addTextArea(cb: (t: TextComponent) => void): this {
    cb(new TextComponent(this.push('textarea')));
    return this;
  }
  addToggle(cb: (t: ToggleComponent) => void): this {
    cb(new ToggleComponent(this.push('toggle')));
    return this;
  }
  addButton(cb: (b: ButtonComponent) => void): this {
    cb(new ButtonComponent(this.push('button')));
    return this;
  }
  addDropdown(cb: (d: DropdownComponent) => void): this {
    const rec = this.push('dropdown');
    rec.options = [];
    cb(new DropdownComponent(rec));
    return this;
  }
}

// ---------- App / Plugin / settings tab ----------

export class App {
  // biome-ignore lint/suspicious/noExplicitAny: structural test stub
  vault: any;
  setting = { open: () => {}, openTabById: (_: string) => {} };
  private layoutReadyCbs: Array<() => void> = [];
  workspace = {
    onLayoutReady: (cb: () => void): void => {
      this.layoutReadyCbs.push(cb);
    },
  };
  // biome-ignore lint/suspicious/noExplicitAny: structural test stub
  constructor(vault?: any) {
    this.vault = vault ?? {};
  }
  /** Test helper — fire the deferred `onLayoutReady` callbacks. */
  flushLayoutReady(): void {
    const cbs = this.layoutReadyCbs;
    this.layoutReadyCbs = [];
    for (const cb of cbs) cb();
  }
}

class _ComponentBase {
  private eventRefs: unknown[] = [];
  registerEvent(e: unknown): void {
    this.eventRefs.push(e);
  }
  registerInterval(_i: unknown): number {
    return 0;
  }
  registerDomEvent(_t: unknown, _ev: string, _h: unknown): void {}
  /** Test helper — registered event refs (for unload simulation). */
  __eventRefs(): unknown[] {
    return this.eventRefs;
  }
}

export class Plugin extends _ComponentBase {
  app: App;
  manifest: { id: string; name: string; version: string };
  settingTabs: unknown[] = [];
  commands: Array<{ id: string; name: string; cb: () => void }> = [];
  statusBarItems: HTMLElementLike[] = [];
  private data: unknown = null;

  constructor(app: App, manifest: { id: string; name: string; version: string }) {
    super();
    this.app = app;
    this.manifest = manifest;
  }

  async loadData(): Promise<unknown> {
    return this.data;
  }
  async saveData(d: unknown): Promise<void> {
    this.data = d;
  }
  addCommand(c: { id: string; name: string; callback: () => void }): void {
    this.commands.push({ id: c.id, name: c.name, cb: c.callback });
  }
  addStatusBarItem(): HTMLElementLike {
    const el = makeStubEl();
    this.statusBarItems.push(el);
    return el;
  }
  addSettingTab(t: unknown): void {
    this.settingTabs.push(t);
  }
  /** Test helper — invoke a registered command's callback by id. */
  __invoke(id: string): void {
    const c = this.commands.find((x) => x.id === id);
    if (!c) throw new Error(`no such command: ${id}`);
    c.cb();
  }
  __commandIds(): string[] {
    return this.commands.map((c) => c.id);
  }
  __lastStatusBarItem(): HTMLElementLike | undefined {
    return this.statusBarItems[this.statusBarItems.length - 1];
  }
}

export class PluginSettingTab {
  containerEl = makeStubEl();
  constructor(
    public app: App,
    public plugin: Plugin,
  ) {}
  display(): void {}
  hide(): void {}
}

export class Modal {
  contentEl = makeStubEl();
  constructor(public app: App) {}
  open(): void {
    this.onOpen();
  }
  close(): void {
    this.onClose();
  }
  onOpen(): void {}
  onClose(): void {}
}

// ---------- TFile / TFolder ----------

export class TAbstractFile {
  constructor(public path: string) {}
  get name(): string {
    const slash = this.path.lastIndexOf('/');
    return slash === -1 ? this.path : this.path.slice(slash + 1);
  }
}
export class TFile extends TAbstractFile {
  get extension(): string {
    const dot = this.name.lastIndexOf('.');
    return dot === -1 ? '' : this.name.slice(dot + 1);
  }
  get basename(): string {
    const dot = this.name.lastIndexOf('.');
    return dot === -1 ? this.name : this.name.slice(0, dot);
  }
}
export class TFolder extends TAbstractFile {
  children: TAbstractFile[] = [];
}

/** Mutable Platform stub — desktop by default in tests; flip
 * `Platform.isDesktopApp = false` to exercise the mobile path. */
export const Platform = { isDesktopApp: true, isMobile: false };

// ---------- HTMLElement shim ----------

function makeStubEl(): HTMLElementLike {
  const classes = new Set<string>();
  let text = '';
  const handlers = new Map<string, Set<(e: unknown) => void>>();
  const el: HTMLElementLike = {
    setText(t: string) {
      text = t;
    },
    getText() {
      return text;
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
      // A fresh render starts with containerEl.empty(); reset the recorder
      // so tests inspect only the current view.
      recordedSettings = [];
      recordedEls = [];
    },
    createEl(tag: string, opts?: { text?: string; cls?: string }): HTMLElementLike {
      const record: RecordedEl = { tag, text: opts?.text ?? '', cls: opts?.cls ?? '' };
      recordedEls.push(record);
      const child = makeStubEl();
      // The settings tab does `createEl('p', { text }).addClass('mod-warning')`;
      // mirror the class onto the record so tests can assert it.
      const baseAdd = child.addClass;
      child.addClass = (c: string) => {
        record.cls = record.cls ? `${record.cls} ${c}` : c;
        baseAdd(c);
      };
      return child;
    },
    addEventListener(name: string, h: (e: unknown) => void) {
      let s = handlers.get(name);
      if (!s) {
        s = new Set();
        handlers.set(name, s);
      }
      s.add(h);
    },
    removeEventListener(name: string, h: (e: unknown) => void) {
      handlers.get(name)?.delete(h);
    },
    dispatchEvent(e: { type: string }) {
      for (const h of handlers.get(e.type) ?? []) h(e);
      return true;
    },
    __click() {
      for (const h of handlers.get('click') ?? []) h({ type: 'click' });
    },
  };
  return el;
}

export interface HTMLElementLike {
  setText(t: string): void;
  getText(): string;
  addClass(c: string): void;
  removeClass(c: string): void;
  hasClass(c: string): boolean;
  empty(): void;
  createEl(tag: string, opts?: { text?: string; cls?: string }): HTMLElementLike;
  addEventListener(name: string, h: (e: unknown) => void): void;
  removeEventListener(name: string, h: (e: unknown) => void): void;
  dispatchEvent(e: { type: string }): boolean;
  __click(): void;
}
