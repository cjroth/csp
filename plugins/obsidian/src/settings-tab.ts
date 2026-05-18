// PluginSettingTab UI. Imports the live `obsidian` package so this module
// only loads inside the host runtime — never from unit tests, which use the
// pure schema in `./settings.js`.
//
// Two states:
//   - Unconfigured → a setup wizard. This is the ONLY way `.context/` gets
//     created. "Connect to a peer" is the primary path (CSP spec.md §7 — a
//     thin node needs a full node to converge). "Create a local vault" is
//     offered with an explicit no-converge caveat.
//   - Configured → the sync toggle + editable config + snapshots.

import { type App, type ButtonComponent, Notice, PluginSettingTab, Setting } from 'obsidian';
import type ContextSyncPlugin from './main.js';
import { parseIgnoreGlobs } from './settings.js';

export class ContextSyncSettingTab extends PluginSettingTab {
  private unsubscribe: (() => void) | null = null;
  private redrawQueued = false;

  // Transient wizard state (not persisted until the user submits setup).
  private setupMode: 'create' | 'connect' = 'connect';
  private setupPeerUrl = '';
  private busy = false;
  private seeded = false;

  constructor(
    app: App,
    private readonly plugin: ContextSyncPlugin,
  ) {
    super(app, plugin);
  }

  override hide(): void {
    this.unsubscribe?.();
    this.unsubscribe = null;
  }

  override display(): void {
    this.unsubscribe?.();
    this.unsubscribe = null;
    const { containerEl } = this;
    containerEl.empty();
    containerEl.createEl('h2', { text: 'Context' });

    if (!this.plugin.isConfigured()) {
      this.renderSetup(containerEl);
      return;
    }
    this.renderConfigured(containerEl);
  }

  // ---- Unconfigured: setup wizard ----

  private renderSetup(containerEl: HTMLElement): void {
    if (!this.seeded) {
      const s = this.plugin.settings;
      if (s.peerUrl) {
        this.setupMode = 'connect';
        this.setupPeerUrl = s.peerUrl;
      }
      this.seeded = true;
    }

    containerEl.createEl('p', {
      text:
        'This vault is not set up yet. Setup writes .context/config, but ' +
        'syncing only activates once setup completes — reaching the peer ' +
        '(connect) or building the local vault (create).',
    });

    if (this.plugin.onboardingError) {
      containerEl
        .createEl('p', { text: `Last attempt failed: ${this.plugin.onboardingError}` })
        .addClass('mod-warning');
    }

    new Setting(containerEl)
      .setName('Setup mode')
      .setDesc('Connect this device to a peer (recommended), or create a new local vault.')
      .addDropdown((d) =>
        d
          .addOption('connect', 'Connect to a peer')
          .addOption('create', 'Create a new local vault')
          .setValue(this.setupMode)
          .onChange((v) => {
            this.setupMode = v as 'create' | 'connect';
            this.display();
          }),
      );

    if (this.setupMode === 'connect') {
      new Setting(containerEl)
        .setName('Peer URL')
        .setDesc(
          'Required — the WebSocket address of a CSP full node in listen ' +
            'mode (e.g. `ctx watch --listen` or Context Desktop), like ' +
            '`wss://host:7777`. Your device key must be authorized there.',
        )
        .addText((t) =>
          t
            .setPlaceholder('wss://host:7777')
            .setValue(this.setupPeerUrl)
            .onChange((v) => {
              this.setupPeerUrl = v;
            }),
        );
    } else {
      containerEl
        .createEl('p', {
          text:
            'Heads-up: a local vault with no peer will NOT converge across ' +
            'devices on its own. CSP requires at least one full node — add a ' +
            'Peer URL now or later, and authorize this device there.',
        })
        .addClass('mod-warning');
      new Setting(containerEl)
        .setName('Peer URL')
        .setDesc('Optional now — add it later before syncing across devices.')
        .addText((t) =>
          t
            .setPlaceholder('wss://host:7777')
            .setValue(this.setupPeerUrl)
            .onChange((v) => {
              this.setupPeerUrl = v;
            }),
        );
    }

    new Setting(containerEl).addButton((b) =>
      b
        .setButtonText(this.busy ? 'Setting up…' : 'Set up Context')
        .setCta()
        .setDisabled(this.busy)
        .onClick(async () => {
          if (this.setupMode === 'connect' && !this.setupPeerUrl.trim()) {
            new Notice('Context: a Peer URL is required to connect.');
            return;
          }
          this.busy = true;
          this.display();
          try {
            await this.plugin.runSetup({
              mode: this.setupMode,
              peerUrl: this.setupPeerUrl,
            });
            new Notice('Context: setup complete.');
          } catch (err) {
            new Notice(`Context: setup failed — ${err}`);
          } finally {
            this.busy = false;
            this.display();
          }
        }),
    );
  }

  // ---- Configured: normal settings ----

  private renderConfigured(containerEl: HTMLElement): void {
    // Keep this view live: re-render on any controller state change.
    this.unsubscribe =
      this.plugin.controller?.on(() => {
        if (this.redrawQueued) return;
        this.redrawQueued = true;
        setTimeout(() => {
          this.redrawQueued = false;
          this.display();
        }, 0);
      }) ?? null;

    new Setting(containerEl)
      .setName('Enable sync')
      .setDesc(
        'Master switch. While off, the plugin makes no connection and opens ' +
          'no session. Turn off to pause syncing without losing config.',
      )
      .addToggle((t) =>
        t.setValue(this.plugin.settings.syncEnabled).onChange(async (v) => {
          await this.plugin.setSyncEnabled(v);
          this.display();
        }),
      );

    const pubkey = this.plugin.controller?.identityPubkeySsh() ?? '(loading…)';
    new Setting(containerEl)
      .setName('Device public key')
      .setDesc(
        'Authorize this device on your peer: run `ctx authorize <thiskey>` ' +
          'on the full node (CSP §10). It is never synced.',
      )
      .addText((t) => t.setValue(pubkey).setDisabled(true))
      .addButton((b: ButtonComponent) =>
        b
          .setButtonText('Copy')
          .setTooltip('Copy public key to clipboard')
          .onClick(async () => {
            const ssh = this.plugin.controller?.identityPubkeySsh();
            if (ssh) await navigator.clipboard.writeText(ssh);
          }),
      );

    new Setting(containerEl)
      .setName('Peer URL')
      .setDesc(
        'WebSocket address of a CSP full node in listen mode, e.g. ' +
          '`wss://host:7777`. Empty = offline-only (will not converge).',
      )
      .addText((t) =>
        t
          .setPlaceholder('wss://host:7777')
          .setValue(this.plugin.settings.peerUrl)
          .onChange(async (v) => {
            this.plugin.settings.peerUrl = v.trim();
            await this.plugin.saveSettings();
          }),
      );

    new Setting(containerEl)
      .setName('Auto-connect on start')
      .setDesc('Open the peer connection automatically when Obsidian launches.')
      .addToggle((t) =>
        t.setValue(this.plugin.settings.autoConnectOnStart).onChange(async (v) => {
          this.plugin.settings.autoConnectOnStart = v;
          await this.plugin.saveSettings();
        }),
      );

    new Setting(containerEl)
      .setName('Ignore patterns')
      .setDesc(
        'One glob per line; `#` lines are comments. Layered under the ' +
          'text-allowlist. (CSP also syncs a shared `.contextignore`; ' +
          'binary files are always skipped.)',
      )
      .addTextArea((t) =>
        t
          .setPlaceholder('# example\nDrafts/**\n*.tmp.md')
          .setValue(this.plugin.settings.ignoreGlobs.join('\n'))
          .onChange(async (v) => {
            this.plugin.settings.ignoreGlobs = parseIgnoreGlobs(v);
            await this.plugin.saveSettings();
          }),
      );

    const pinned = this.plugin.settings.peerPubkey || '(none yet)';
    new Setting(containerEl)
      .setName('Pinned peer key')
      .setDesc(
        'Set on first successful connect (CSP §10 key pinning), stored as ' +
          '`[peer] pubkey`. Clear to allow connecting to a different peer.',
      )
      .addText((t) => t.setValue(pinned).setDisabled(true))
      .addButton((b) =>
        b
          .setButtonText('Clear pin')
          .setWarning()
          .onClick(async () => {
            this.plugin.settings.peerPubkey = '';
            await this.plugin.saveSettings();
            this.display();
          }),
      );

    const state = this.plugin.controller?.state ?? 'idle';
    const connected = state === 'connected';
    new Setting(containerEl)
      .setName('Connection')
      .setDesc(`Current state: ${state}.`)
      .addButton((b) =>
        b
          .setButtonText(connected ? 'Disconnect' : state === 'idle' ? 'Connect' : 'Reconnect')
          .setCta()
          .onClick(async () => {
            await this.plugin.controller?.stop();
            if (connected) await this.plugin.controller?.prepare();
            else await this.plugin.controller?.start({ connect: true });
            this.display();
          }),
      )
      .addButton((b) =>
        b
          .setButtonText('Resync now')
          .setTooltip('Re-run the bidirectional reconcile pass.')
          .onClick(async () => {
            await this.plugin.controller?.resyncNow();
          }),
      );

    new Setting(containerEl)
      .setName('Reset local state')
      .setDesc(
        'Rebuilds .context/{state,frontier,snapshots}; .context/config and ' +
          'your device key are kept (the key lives in ~/.context, shared ' +
          'with `ctx`). Your Obsidian vault contents are NOT touched. ' +
          'Resumes under the same device key (CSP §5.1).',
      )
      .addButton((b) =>
        b
          .setButtonText('Reset')
          .setWarning()
          .onClick(async () => {
            await this.plugin.controller?.resetLocalState();
            this.plugin.settings.peerPubkey = '';
            await this.plugin.saveSettings();
            new Notice('Context: local state cleared.');
            this.display();
          }),
      );

    containerEl.createEl('h3', { text: 'Snapshots' });
    const snaps = this.plugin.controller?.listSnapshots() ?? [];
    if (snaps.length === 0) {
      containerEl.createEl('p', {
        text: 'No snapshots yet. Snapshots are point-in-time recovery points (CSP §8).',
      });
    } else {
      for (const snap of snaps) {
        const created = new Date(snap.created_at_ms).toLocaleString();
        new Setting(containerEl)
          .setName(snap.name)
          .setDesc(`Created ${created}`)
          .addButton((b) =>
            b
              .setButtonText('Restore')
              .setWarning()
              .onClick(async () => {
                await this.plugin.controller?.restoreToSnapshot(snap.name);
                this.display();
              }),
          );
      }
    }

    new Setting(containerEl).setName('Create snapshot').addButton((b) =>
      b
        .setButtonText('Create')
        .setCta()
        .onClick(async () => {
          const name = `snapshot-${new Date().toISOString().replace(/[:.]/g, '-')}`;
          await this.plugin.controller?.createSnapshot(name);
          this.display();
        }),
    );
  }
}
