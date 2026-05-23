// In-app log viewer (issue 0013). On iOS Obsidian the WebView dev console
// is unreachable, so even with `[context …]` + `[engine-worker …]` lines
// being emitted to `console.log`, an operator on mobile has no way to read
// them. This modal surfaces the same ring buffer the host fills on every
// log line and lets the user copy the lot to share in a bug report.
//
// Kept deliberately minimal — no filters, no levels picker, no search.
// Anything more is something a desktop user can do in their browser dev
// tools; for the mobile-no-sync diagnosis loop, "show me everything since
// I opened the modal" is exactly what's wanted.

import { type App, Modal, Notice } from 'obsidian';
import type { LogBuffer, LogEntry } from './log-buffer.js';

export class LogModal extends Modal {
  private unsubscribe: (() => void) | null = null;
  private bodyEl: HTMLElement | null = null;
  private emptyEl: HTMLElement | null = null;

  constructor(
    app: App,
    private readonly buffer: LogBuffer,
  ) {
    super(app);
  }

  override onOpen(): void {
    const { contentEl } = this;
    // Header.
    contentEl.createEl('h2', { text: 'Context — engine log' });
    contentEl.createEl('p', {
      text:
        'Most recent at the bottom. Copy this and share it if syncing is misbehaving — ' +
        'on mobile, this is the only window into what the engine is doing.',
      cls: 'setting-item-description',
    });

    // Action row: Copy / Clear / Close.
    const actions = contentEl.createEl('div', { cls: 'modal-button-container' });
    const copyBtn = actions.createEl('button', { text: 'Copy all', cls: 'mod-cta' });
    copyBtn.addEventListener('click', () => {
      void this.copyAll();
    });
    const clearBtn = actions.createEl('button', { text: 'Clear' });
    clearBtn.addEventListener('click', () => {
      this.buffer.clear();
      this.renderAll();
    });
    const closeBtn = actions.createEl('button', { text: 'Close' });
    closeBtn.addEventListener('click', () => {
      this.close();
    });

    // Body: a pre that holds one row per entry. Monospace is the right
    // default for trace lines — easier to read column-aligned timestamps.
    const body = contentEl.createEl('pre', { cls: 'context-log-body' });
    // Caps so a huge buffer doesn't blow up the modal layout on mobile.
    body.style.maxHeight = '60vh';
    body.style.overflowY = 'auto';
    body.style.whiteSpace = 'pre-wrap';
    body.style.wordBreak = 'break-word';
    body.style.fontSize = '12px';
    body.style.padding = '8px';
    body.style.border = '1px solid var(--background-modifier-border)';
    body.style.borderRadius = '6px';
    this.bodyEl = body;

    // Empty-state placeholder — replaced once the first line lands.
    this.emptyEl = body.createEl('span', {
      text: '(no log lines yet — interact with Context to see activity)',
    });
    this.emptyEl.style.opacity = '0.6';

    this.renderAll();

    // Stream live appends so an operator can watch a sync happen in real
    // time. Auto-scroll only if the user is already at the bottom — if
    // they've scrolled up to inspect older lines, don't yank them back.
    this.unsubscribe = this.buffer.subscribe((e) => this.append(e));
  }

  override onClose(): void {
    this.unsubscribe?.();
    this.unsubscribe = null;
    this.contentEl.empty();
  }

  private renderAll(): void {
    if (!this.bodyEl) return;
    this.bodyEl.empty();
    const entries = this.buffer.snapshot();
    if (entries.length === 0) {
      this.emptyEl = this.bodyEl.createEl('span', {
        text: '(no log lines yet — interact with Context to see activity)',
      });
      this.emptyEl.style.opacity = '0.6';
      return;
    }
    this.emptyEl = null;
    for (const e of entries) this.appendRow(e);
    this.scrollToBottom();
  }

  private append(e: LogEntry): void {
    if (!this.bodyEl) return;
    if (this.emptyEl) {
      this.bodyEl.empty();
      this.emptyEl = null;
    }
    const atBottom = this.isAtBottom();
    this.appendRow(e);
    if (atBottom) this.scrollToBottom();
  }

  private appendRow(e: LogEntry): void {
    if (!this.bodyEl) return;
    const tag = e.source === 'main' ? 'context' : 'engine-worker';
    const row = this.bodyEl.createEl('div');
    row.setText(`[${tag} ${e.ts}] ${e.msg}`);
    if (e.level === 'error') {
      // Use the text-error CSS var so it picks up the user's theme.
      row.style.color = 'var(--text-error)';
    }
  }

  private isAtBottom(): boolean {
    const el = this.bodyEl;
    if (!el) return true;
    // 8 px slack to forgive sub-pixel scroll positions when the user
    // explicitly clicked the bottom; otherwise a 1 px diff would stop
    // auto-scroll.
    return el.scrollHeight - el.scrollTop - el.clientHeight < 8;
  }

  private scrollToBottom(): void {
    if (this.bodyEl) this.bodyEl.scrollTop = this.bodyEl.scrollHeight;
  }

  private async copyAll(): Promise<void> {
    const text = this.buffer.toText();
    if (!text) {
      new Notice('Context: log is empty.');
      return;
    }
    try {
      // navigator.clipboard is available in modern Obsidian (Electron
      // desktop + iOS WKWebView + Android WebView all expose it).
      await navigator.clipboard.writeText(text);
      new Notice(`Context: copied ${text.split('\n').length} log line(s).`);
    } catch (err) {
      new Notice(`Context: copy failed — ${err instanceof Error ? err.message : String(err)}`);
    }
  }
}
