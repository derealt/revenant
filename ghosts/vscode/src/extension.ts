/**
 * REVENANT Ghost — VS Code Extension
 *
 * Listens for context cards from the daemon via Unix socket and renders
 * them as transient inline decorations. These decorations are visual
 * overlays — they NEVER modify the actual file content.
 *
 * The ghost appears as a subtle, dimmed annotation at the top of the
 * file where you last had your cursor. It self-destructs after the
 * configured TTL (default 5 minutes of activity).
 */

import * as vscode from 'vscode';
import * as net from 'net';
import * as path from 'path';
import * as os from 'os';
import * as fs from 'fs';

// ─── Types ─────────────────────────────────────────────────────────

interface ContextCard {
  id: string;
  project_dir: string;
  project_name: string;
  summary: string;
  next_step: string;
  ttl_seconds: number;
}

interface DaemonMessage {
  type: 'inject' | 'clear';
  card?: ContextCard;
}

// ─── State ─────────────────────────────────────────────────────────

let ghostDecorationType: vscode.TextEditorDecorationType | undefined;
let activeGhostCard: ContextCard | undefined;
let ghostTimer: NodeJS.Timeout | undefined;
let socketServer: net.Server | undefined;
let activityTracker: ActivityTracker | undefined;
let statusBarItem: vscode.StatusBarItem | undefined;

// ─── Activation ────────────────────────────────────────────────────

export function activate(context: vscode.ExtensionContext): void {
  console.log('REVENANT ghost extension activating');

  // Create status bar item (subtle, rightmost)
  statusBarItem = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Right,
    0
  );
  statusBarItem.text = '$(eye-closed)';
  statusBarItem.tooltip = 'REVENANT — no active ghost';
  statusBarItem.command = 'revenant.history';
  statusBarItem.show();
  context.subscriptions.push(statusBarItem);

  // Initialize activity tracker for TTL management
  activityTracker = new ActivityTracker();

  // Register commands
  context.subscriptions.push(
    vscode.commands.registerCommand('revenant.showGhost', showGhostCommand),
    vscode.commands.registerCommand('revenant.clearGhost', clearAllGhosts),
    vscode.commands.registerCommand('revenant.history', showHistory)
  );

  // Start listening for daemon messages
  startSocketServer(context);

  // Track editor activity for ghost TTL
  context.subscriptions.push(
    vscode.workspace.onDidChangeTextDocument(() => {
      activityTracker?.recordActivity();
      checkGhostExpiry();
    }),
    vscode.window.onDidChangeActiveTextEditor(() => {
      activityTracker?.recordActivity();
      checkGhostExpiry();
      // Re-render ghost on editor change if still active
      if (activeGhostCard) {
        renderGhost(activeGhostCard);
      }
    }),
    vscode.window.onDidChangeTextEditorSelection(() => {
      activityTracker?.recordActivity();
    })
  );

  // Report open files to daemon on workspace state changes
  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument(() => reportEditorState()),
    vscode.workspace.onDidCloseTextDocument(() => reportEditorState())
  );

  // Initial state report
  reportEditorState();

  console.log('REVENANT ghost extension activated');
}

export function deactivate(): void {
  clearAllGhosts();
  socketServer?.close();
  console.log('REVENANT ghost extension deactivated');
}

// ─── Socket Server ─────────────────────────────────────────────────

function getSocketPath(): string {
  const config = vscode.workspace.getConfiguration('revenant');
  const configured = config.get<string>('socketPath', '~/.revenant/vscode.sock');
  return configured.replace(/^~/, os.homedir());
}

function startSocketServer(context: vscode.ExtensionContext): void {
  const socketPath = getSocketPath();

  // Ensure directory exists
  const dir = path.dirname(socketPath);
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true });
  }

  // Clean up stale socket
  if (fs.existsSync(socketPath)) {
    try {
      fs.unlinkSync(socketPath);
    } catch {
      // ignore
    }
  }

  socketServer = net.createServer((connection) => {
    let buffer = '';

    connection.on('data', (data) => {
      buffer += data.toString();
      // Messages are newline-delimited JSON
      const lines = buffer.split('\n');
      buffer = lines.pop() || '';

      for (const line of lines) {
        if (line.trim()) {
          try {
            const message: DaemonMessage = JSON.parse(line);
            handleDaemonMessage(message);
          } catch (err) {
            console.error('REVENANT: failed to parse daemon message:', err);
          }
        }
      }
    });

    connection.on('error', (err) => {
      console.error('REVENANT: socket connection error:', err);
    });
  });

  socketServer.listen(socketPath, () => {
    console.log(`REVENANT: listening on ${socketPath}`);
  });

  socketServer.on('error', (err) => {
    console.error('REVENANT: socket server error:', err);
  });

  // Cleanup on dispose
  context.subscriptions.push({
    dispose: () => {
      socketServer?.close();
      try {
        fs.unlinkSync(socketPath);
      } catch {
        // ignore
      }
    },
  });
}

// ─── Message Handling ──────────────────────────────────────────────

function handleDaemonMessage(message: DaemonMessage): void {
  switch (message.type) {
    case 'inject':
      if (message.card) {
        injectGhost(message.card);
      }
      break;
    case 'clear':
      clearAllGhosts();
      break;
  }
}

function injectGhost(card: ContextCard): void {
  // Check if this card's project matches the current workspace
  const workspaceFolders = vscode.workspace.workspaceFolders;
  if (workspaceFolders) {
    const match = workspaceFolders.some(
      (folder) => card.project_dir === folder.uri.fsPath
    );
    if (!match) {
      // Card is for a different project — ignore
      return;
    }
  }

  activeGhostCard = card;
  activityTracker?.reset();

  renderGhost(card);
  updateStatusBar(card);

  // Set self-destruct timer
  if (ghostTimer) {
    clearTimeout(ghostTimer);
  }
  ghostTimer = setTimeout(() => {
    clearAllGhosts();
  }, card.ttl_seconds * 1000);
}

// ─── Rendering ─────────────────────────────────────────────────────

function renderGhost(card: ContextCard): void {
  const editor = vscode.window.activeTextEditor;
  if (!editor) return;

  // Clear previous decoration
  if (ghostDecorationType) {
    ghostDecorationType.dispose();
  }

  const config = vscode.workspace.getConfiguration('revenant');
  const ghostColor = config.get<string>('ghostColor', '#4EC9B0');
  const ghostOpacity = config.get<number>('ghostOpacity', 0.7);

  // Build the ghost text
  let ghostText = `  REVENANT: ${card.summary}`;
  if (card.next_step) {
    ghostText += ` | Next: ${card.next_step}`;
  }

  // Create decoration type with inline "after" content
  // This renders AFTER the first line of the file — purely visual
  ghostDecorationType = vscode.window.createTextEditorDecorationType({
    isWholeLine: true,
    after: {
      contentText: ghostText,
      color: ghostColor,
      fontStyle: 'italic',
      margin: '0 0 0 2em',
      // Use CSS opacity
      textDecoration: `none; opacity: ${ghostOpacity}`,
    },
    // Subtle background tint on the line
    backgroundColor: `${ghostColor}10`,
    overviewRulerColor: ghostColor,
    overviewRulerLane: vscode.OverviewRulerLane.Right,
  });

  // Apply to line 0 (top of file)
  const range = new vscode.Range(0, 0, 0, 0);
  editor.setDecorations(ghostDecorationType, [range]);
}

function updateStatusBar(card: ContextCard | undefined): void {
  if (!statusBarItem) return;

  if (card) {
    statusBarItem.text = '$(eye) REVENANT';
    statusBarItem.tooltip = `Ghost active: ${card.summary}`;
    statusBarItem.backgroundColor = new vscode.ThemeColor(
      'statusBarItem.warningBackground'
    );
  } else {
    statusBarItem.text = '$(eye-closed)';
    statusBarItem.tooltip = 'REVENANT — no active ghost';
    statusBarItem.backgroundColor = undefined;
  }
}

// ─── Ghost Lifecycle ───────────────────────────────────────────────

function clearAllGhosts(): void {
  if (ghostDecorationType) {
    ghostDecorationType.dispose();
    ghostDecorationType = undefined;
  }
  activeGhostCard = undefined;
  if (ghostTimer) {
    clearTimeout(ghostTimer);
    ghostTimer = undefined;
  }
  updateStatusBar(undefined);
}

function checkGhostExpiry(): void {
  if (!activeGhostCard || !activityTracker) return;

  // If user has been active for longer than TTL, clear the ghost
  const activeSeconds = activityTracker.activeSeconds();
  if (activeSeconds >= activeGhostCard.ttl_seconds) {
    console.log(
      `REVENANT: ghost expired after ${activeSeconds}s of activity`
    );
    clearAllGhosts();
  }
}

// ─── Activity Tracking ─────────────────────────────────────────────

class ActivityTracker {
  private firstActivity: number | undefined;
  private lastActivity: number = 0;
  private activityCount: number = 0;

  recordActivity(): void {
    const now = Date.now();
    if (!this.firstActivity) {
      this.firstActivity = now;
    }
    this.lastActivity = now;
    this.activityCount++;
  }

  activeSeconds(): number {
    if (!this.firstActivity) return 0;
    return (Date.now() - this.firstActivity) / 1000;
  }

  reset(): void {
    this.firstActivity = undefined;
    this.lastActivity = 0;
    this.activityCount = 0;
  }
}

// ─── Editor State Reporting ────────────────────────────────────────

function reportEditorState(): void {
  // Write current editor state to a file the daemon reads
  const stateDir = path.join(os.homedir(), '.revenant', 'vscode-state');
  const workspaceFolders = vscode.workspace.workspaceFolders;
  if (!workspaceFolders || workspaceFolders.length === 0) return;

  const projectDir = workspaceFolders[0].uri.fsPath;
  const hash = simpleHash(projectDir);

  const openFiles = vscode.window.visibleTextEditors
    .map((editor) => editor.document.uri.fsPath)
    .filter((p) => !p.startsWith('extension-output'));

  const activeFile = vscode.window.activeTextEditor?.document.uri.fsPath;

  const state = {
    projectDir,
    openFiles,
    activeFile: activeFile || null,
    timestamp: Date.now(),
  };

  try {
    if (!fs.existsSync(stateDir)) {
      fs.mkdirSync(stateDir, { recursive: true });
    }
    fs.writeFileSync(
      path.join(stateDir, `${hash}.json`),
      JSON.stringify(state, null, 2)
    );
  } catch {
    // Non-fatal
  }
}

function simpleHash(s: string): string {
  let hash = 0xcbf29ce484222325n;
  for (let i = 0; i < s.length; i++) {
    hash ^= BigInt(s.charCodeAt(i));
    hash = (hash * 0x100000001b3n) & 0xffffffffffffffffn;
  }
  return hash.toString(16).padStart(16, '0');
}

// ─── Commands ──────────────────────────────────────────────────────

function showGhostCommand(): void {
  if (activeGhostCard) {
    vscode.window.showInformationMessage(
      `REVENANT: ${activeGhostCard.summary}${
        activeGhostCard.next_step ? ` | Next: ${activeGhostCard.next_step}` : ''
      }`
    );
  } else {
    vscode.window.showInformationMessage(
      'REVENANT: No active ghost. Context will appear when you return to a project.'
    );
  }
}

async function showHistory(): Promise<void> {
  // Read context cards from the database via a simple file
  const historyFile = path.join(os.homedir(), '.revenant', 'history.json');
  if (!fs.existsSync(historyFile)) {
    vscode.window.showInformationMessage('REVENANT: No context history available.');
    return;
  }

  try {
    const content = fs.readFileSync(historyFile, 'utf8');
    const cards: ContextCard[] = JSON.parse(content);

    const items = cards.map((card) => ({
      label: `$(clock) ${card.project_name}`,
      description: card.summary,
      detail: card.next_step || undefined,
      card,
    }));

    const selected = await vscode.window.showQuickPick(items, {
      placeHolder: 'Select a context card to view',
      matchOnDescription: true,
    });

    if (selected) {
      vscode.window.showInformationMessage(
        `${selected.card.summary}${
          selected.card.next_step ? `\n\nNext: ${selected.card.next_step}` : ''
        }`
      );
    }
  } catch {
    vscode.window.showErrorMessage('REVENANT: Failed to read context history.');
  }
}
