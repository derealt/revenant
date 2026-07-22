/**
 * REVENANT Ghost - Obsidian Plugin
 *
 * Monitors a state file written by the daemon and displays a transient
 * callout block at the top of the note you were last editing. The callout
 * is rendered as a virtual element in the editor - it NEVER modifies
 * the actual note content.
 *
 * The ghost disappears after the configured TTL of activity.
 */

import {
  App,
  Editor,
  MarkdownView,
  Notice,
  Plugin,
  PluginSettingTab,
  Setting,
  WorkspaceLeaf,
  MarkdownPostProcessorContext,
  EditorPosition,
} from 'obsidian';

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';

// ─── Types ─────────────────────────────────────────────────────────

interface ContextCard {
  id: string;
  project_dir: string;
  project_name: string;
  summary: string;
  next_step: string;
  ttl_seconds: number;
}

interface GhostState {
  card: ContextCard | null;
  displayedAt: number | null;
}

interface RevenantSettings {
  statePath: string;
  pollInterval: number;
  ghostColor: string;
}

const DEFAULT_SETTINGS: RevenantSettings = {
  statePath: path.join(os.homedir(), '.revenant', 'obsidian-state.json'),
  pollInterval: 3000,
  ghostColor: '#4EC9B0',
};

// ─── Plugin ────────────────────────────────────────────────────────

export default class RevenantGhostPlugin extends Plugin {
  settings: RevenantSettings = DEFAULT_SETTINGS;
  ghostState: GhostState = { card: null, displayedAt: null };
  pollTimer: ReturnType<typeof setInterval> | null = null;
  ghostElement: HTMLElement | null = null;
  activityStart: number | null = null;

  async onload(): Promise<void> {
    await this.loadSettings();

    // Add settings tab
    this.addSettingTab(new RevenantSettingTab(this.app, this));

    // Start polling for daemon state
    this.pollTimer = setInterval(() => this.checkForGhost(), this.settings.pollInterval);

    // Track activity for ghost TTL
    this.registerDomEvent(document, 'keydown', () => this.recordActivity());
    this.registerDomEvent(document, 'click', () => this.recordActivity());

    // Command to manually show/dismiss ghost
    this.addCommand({
      id: 'show-ghost',
      name: 'Show active ghost',
      callback: () => {
        if (this.ghostState.card) {
          new Notice(
            `REVENANT: ${this.ghostState.card.summary}${
              this.ghostState.card.next_step
                ? `\n\nNext: ${this.ghostState.card.next_step}`
                : ''
            }`,
            10000
          );
        } else {
          new Notice('REVENANT: No active ghost.');
        }
      },
    });

    this.addCommand({
      id: 'dismiss-ghost',
      name: 'Dismiss ghost',
      callback: () => this.clearGhost(),
    });

    // Report current note to daemon
    this.registerEvent(
      this.app.workspace.on('active-leaf-change', (leaf: WorkspaceLeaf | null) => {
        this.reportEditorState();
        this.recordActivity();
        // Re-render ghost on active leaf change
        if (this.ghostState.card) {
          this.renderGhost(this.ghostState.card);
        }
      })
    );

    console.log('REVENANT ghost plugin loaded');
  }

  onunload(): void {
    if (this.pollTimer) {
      clearInterval(this.pollTimer);
    }
    this.clearGhost();
    console.log('REVENANT ghost plugin unloaded');
  }

  // ─── Ghost State Polling ───────────────────────────────────────

  checkForGhost(): void {
    try {
      if (!fs.existsSync(this.settings.statePath)) return;

      const content = fs.readFileSync(this.settings.statePath, 'utf8');
      const state: { type: string; card?: ContextCard } = JSON.parse(content);

      if (state.type === 'inject' && state.card) {
        // New ghost - only inject if different from current
        if (!this.ghostState.card || this.ghostState.card.id !== state.card.id) {
          this.injectGhost(state.card);
        }
      } else if (state.type === 'clear') {
        this.clearGhost();
      }
    } catch {
      // File might be mid-write - ignore parse errors
    }

    // Check TTL expiry
    this.checkExpiry();
  }

  injectGhost(card: ContextCard): void {
    this.ghostState = {
      card,
      displayedAt: Date.now(),
    };
    this.activityStart = null; // Reset activity tracking

    this.renderGhost(card);

    new Notice(`REVENANT ghost: ${card.project_name}`, 3000);
  }

  clearGhost(): void {
    this.ghostState = { card: null, displayedAt: null };
    this.activityStart = null;

    if (this.ghostElement) {
      this.ghostElement.remove();
      this.ghostElement = null;
    }
  }

  checkExpiry(): void {
    if (!this.ghostState.card || !this.activityStart) return;

    const activeMs = Date.now() - this.activityStart;
    const ttlMs = this.ghostState.card.ttl_seconds * 1000;

    if (activeMs >= ttlMs) {
      console.log(`REVENANT: ghost expired after ${activeMs / 1000}s of activity`);
      this.clearGhost();
    }
  }

  recordActivity(): void {
    if (this.ghostState.card && !this.activityStart) {
      this.activityStart = Date.now();
    }
  }

  // ─── Rendering ─────────────────────────────────────────────────

  renderGhost(card: ContextCard): void {
    // Remove existing ghost element
    if (this.ghostElement) {
      this.ghostElement.remove();
    }

    const activeLeaf = this.app.workspace.getActiveViewOfType(MarkdownView);
    if (!activeLeaf) return;

    const editorEl = activeLeaf.contentEl;
    if (!editorEl) return;

    // Create ghost callout element
    const ghost = document.createElement('div');
    ghost.className = 'revenant-ghost-callout';
    ghost.style.cssText = `
      background: rgba(78, 201, 176, 0.08);
      border-left: 3px solid ${this.settings.ghostColor};
      border-radius: 4px;
      padding: 10px 14px;
      margin: 8px 0 16px 0;
      font-size: 13px;
      color: var(--text-muted);
      position: relative;
      opacity: 0;
      transition: opacity 0.3s ease;
    `;

    // Label
    const label = document.createElement('span');
    label.style.cssText = `
      font-size: 10px;
      font-weight: 700;
      letter-spacing: 0.1em;
      text-transform: uppercase;
      color: ${this.settings.ghostColor};
      margin-right: 8px;
    `;
    label.textContent = 'REVENANT';

    // Summary
    const summary = document.createElement('span');
    summary.style.cssText = 'color: var(--text-normal);';
    summary.textContent = card.summary;

    ghost.appendChild(label);
    ghost.appendChild(summary);

    // Next step
    if (card.next_step) {
      const next = document.createElement('div');
      next.style.cssText = `
        margin-top: 4px;
        color: #FFC864;
        font-style: italic;
        font-size: 12px;
      `;
      next.textContent = `Next: ${card.next_step}`;
      ghost.appendChild(next);
    }

    // Dismiss button
    const dismiss = document.createElement('button');
    dismiss.style.cssText = `
      position: absolute;
      top: 8px;
      right: 8px;
      background: none;
      border: none;
      color: var(--text-faint);
      cursor: pointer;
      font-size: 14px;
      padding: 2px 6px;
      border-radius: 3px;
    `;
    dismiss.textContent = '\u00d7';
    dismiss.title = 'Dismiss ghost';
    dismiss.addEventListener('click', () => this.clearGhost());
    ghost.appendChild(dismiss);

    // Insert at top of editor content
    const firstChild = editorEl.querySelector('.cm-editor') || editorEl.firstChild;
    if (firstChild) {
      editorEl.insertBefore(ghost, firstChild);
    } else {
      editorEl.prepend(ghost);
    }

    this.ghostElement = ghost;

    // Fade in
    requestAnimationFrame(() => {
      ghost.style.opacity = '1';
    });
  }

  // ─── Editor State Reporting ────────────────────────────────────

  reportEditorState(): void {
    const activeView = this.app.workspace.getActiveViewOfType(MarkdownView);
    if (!activeView) return;

    const file = activeView.file;
    if (!file) return;

    const vaultPath = (this.app.vault.adapter as any).basePath || '';

    const state = {
      activeFile: file.path,
      vaultPath,
      timestamp: Date.now(),
    };

    try {
      const dir = path.dirname(this.settings.statePath);
      if (!fs.existsSync(dir)) {
        fs.mkdirSync(dir, { recursive: true });
      }
      fs.writeFileSync(
        this.settings.statePath.replace('.json', '-editor.json'),
        JSON.stringify(state, null, 2)
      );
    } catch {
      // Non-fatal
    }
  }

  // ─── Settings ──────────────────────────────────────────────────

  async loadSettings(): Promise<void> {
    this.settings = Object.assign({}, DEFAULT_SETTINGS, await this.loadData());
  }

  async saveSettings(): Promise<void> {
    await this.saveData(this.settings);
  }
}

// ─── Settings Tab ──────────────────────────────────────────────────

class RevenantSettingTab extends PluginSettingTab {
  plugin: RevenantGhostPlugin;

  constructor(app: App, plugin: RevenantGhostPlugin) {
    super(app, plugin);
    this.plugin = plugin;
  }

  display(): void {
    const { containerEl } = this;
    containerEl.empty();

    containerEl.createEl('h2', { text: 'REVENANT Ghost Settings' });

    new Setting(containerEl)
      .setName('State file path')
      .setDesc('Path where the daemon writes ghost state')
      .addText((text) =>
        text
          .setPlaceholder('~/.revenant/obsidian-state.json')
          .setValue(this.plugin.settings.statePath)
          .onChange(async (value) => {
            this.plugin.settings.statePath = value;
            await this.plugin.saveSettings();
          })
      );

    new Setting(containerEl)
      .setName('Poll interval (ms)')
      .setDesc('How often to check for new ghost state')
      .addText((text) =>
        text
          .setPlaceholder('3000')
          .setValue(String(this.plugin.settings.pollInterval))
          .onChange(async (value) => {
            const parsed = parseInt(value, 10);
            if (!isNaN(parsed) && parsed >= 1000) {
              this.plugin.settings.pollInterval = parsed;
              await this.plugin.saveSettings();
            }
          })
      );

    new Setting(containerEl)
      .setName('Ghost color')
      .setDesc('Accent color for ghost annotations')
      .addText((text) =>
        text
          .setPlaceholder('#4EC9B0')
          .setValue(this.plugin.settings.ghostColor)
          .onChange(async (value) => {
            this.plugin.settings.ghostColor = value;
            await this.plugin.saveSettings();
          })
      );
  }
}
