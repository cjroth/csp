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
import { normalizePeerUrl, parseIgnoreGlobs } from './settings.js';

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
      text: "This vault isn't syncing yet. Choose how to set it up below.",
    });

    if (this.plugin.onboardingError) {
      containerEl
        .createEl('p', { text: `Last attempt failed: ${this.plugin.onboardingError}` })
        .addClass('mod-warning');
    }

    new Setting(containerEl)
      .setName('Setup mode')
      .setDesc("Connect to another device that's already syncing, or start a brand-new vault here.")
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
          'Address of the other device, e.g. `sync.example.com` or ' +
            '`wss://192.168.1.10:7777`. A bare domain assumes `wss://` on ' +
            'port 443. This device must be authorized on the peer first.',
        )
        .addText((t) =>
          t
            .setPlaceholder('sync.example.com')
            .setValue(this.setupPeerUrl)
            .onChange((v) => {
              this.setupPeerUrl = v;
            }),
        );
      new Setting(containerEl)
        .setName('Auth key (optional)')
        .setDesc(
          'Only needed if the peer was started with CTX_AUTH_KEY and this device has ' +
            "not enrolled yet. Used once at clone time; the peer's authorized_keys " +
            'records this device after a successful connect.',
        )
        .addText((t) => {
          t.inputEl.type = 'password';
          t.setPlaceholder('paste only if required')
            .setValue(this.plugin.settings.authKey)
            .onChange((v) => {
              this.plugin.settings.authKey = v.trim();
            });
        });
    } else {
      containerEl
        .createEl('p', {
          text: "Heads-up: a vault with no peer won't sync to your other devices.",
        })
        .addClass('mod-warning');
      new Setting(containerEl)
        .setName('Peer URL')
        .setDesc('Optional — add it later to start syncing with another device.')
        .addText((t) =>
          t
            .setPlaceholder('sync.example.com')
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

    const state = this.plugin.controller?.state ?? 'idle';
    new Setting(containerEl)
      .setName('Enable sync')
      .setDesc(`Turn syncing on or off. Current state: ${state}.`)
      .addToggle((t) =>
        t.setValue(this.plugin.settings.syncEnabled).onChange(async (v) => {
          await this.plugin.setSyncEnabled(v);
          this.display();
        }),
      );

    const pubkey = this.plugin.controller?.identityPubkeySsh() ?? '(loading…)';
    new Setting(containerEl)
      .setName('Device public key')
      .setDesc("This device's identity. Share it so the other side can authorize it.")
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
        'Address of the other device. A bare domain assumes `wss://` on ' +
          'port 443. Leave empty for offline-only.',
      )
      .addText((t) =>
        t
          .setPlaceholder('sync.example.com')
          .setValue(this.plugin.settings.peerUrl)
          .onChange(async (v) => {
            this.plugin.settings.peerUrl = normalizePeerUrl(v);
            await this.plugin.saveSettings();
          }),
      );

    new Setting(containerEl)
      .setName('Ignore patterns')
      .setDesc('One glob per line; `#` starts a comment. Files matching these are not synced.')
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
      .setDesc("The peer's identity, remembered on first connect.")
      .addText((t) => t.setValue(pinned).setDisabled(true));

    new Setting(containerEl)
      .setName('Reset local state')
      .setDesc(
        "Delete this device's local sync data and start over. Your Obsidian " +
          'notes are not touched.',
      )
      .addButton((b) =>
        b
          .setButtonText('Reset')
          .setWarning()
          .onClick(async () => {
            await this.plugin.resetLocalState();
            new Notice('Context: local state cleared.');
            this.display();
          }),
      );

    containerEl.createEl('h3', { text: 'Snapshots' });
    const snaps = this.plugin.controller?.listSnapshots() ?? [];
    if (snaps.length === 0) {
      containerEl.createEl('p', {
        text: 'No snapshots yet. A snapshot saves a point-in-time copy you can roll back to.',
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
