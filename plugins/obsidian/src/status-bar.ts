// Status-bar widget — visualizes the controller's UI state machine in the
// Obsidian UI. It is a projection of engine-reported connectivity (CSP
// spec.md §6.5), not a protocol state machine. Pure DOM manipulation.

export type SyncState = 'idle' | 'connecting' | 'connected' | 'reconnecting' | 'error';

const LABELS: Record<SyncState, string> = {
  idle: 'Context: idle',
  connecting: 'Context: connecting…',
  connected: 'Context: connected',
  reconnecting: 'Context: reconnecting…',
  error: 'Context: error',
};

export class StatusBar {
  constructor(private readonly el: HTMLElement) {
    this.el.addClass('context-status');
    this.set('idle');
  }

  set(state: SyncState, detail?: string): void {
    const text = detail ? `${LABELS[state]} (${detail})` : LABELS[state];
    this.el.setText(text);
    this.el.removeClass('context-state-idle');
    this.el.removeClass('context-state-connecting');
    this.el.removeClass('context-state-connected');
    this.el.removeClass('context-state-reconnecting');
    this.el.removeClass('context-state-error');
    this.el.addClass(`context-state-${state}`);
  }

  onClick(handler: () => void): void {
    this.el.addClass('mod-clickable');
    this.el.addEventListener('click', handler);
  }
}
