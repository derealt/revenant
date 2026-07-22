/**
 * REVENANT Ghost - VS Code Extension
 *
 * Listens for context cards from the daemon via Unix socket and renders
 * them as transient inline decorations. These decorations are visual
 * overlays - they NEVER modify the actual file content.
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

interface GhostAnchor {
  /** Absolute path of the file the cursor was in at departure */
  file: string;
  /** 1-based line number */
  line: number;
}

interface ContextCard {
  id: string;
  project_dir: string;
  project_name: string;
  summary: string;
  next_step: string;
  ttl_seconds: number;
  /** Where your head was: the resume point captured at departure */
  anchor?: GhostAnchor | null;
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
let stateReportTimer: NodeJS.Timeout | undefined;

// ─── Activation ────────────────────────────────────────────────────

export function activate(context: vscode.ExtensionContext): void {
  console.log('REVENANT ghost extension activating');

  // Create status bar item (subtle, rightmost)
  statusBarItem = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Right,
    0
  );
  statusBarItem.text = '$(eye-closed)';
  statusBarItem.tooltip = 'REVENANT - no active ghost';
  statusBarItem.command = 'revenant.history';
  statusBarItem.show();
  context.subscriptions.push(statusBarItem);

  // Initialize activity tracker for TTL management
  activityTracker = new ActivityTracker();

  // Register commands
  context.subscriptions.push(
    vscode.commands.registerCommand('revenant.showGhost', showGhostCommand),
    vscode.commands.registerCommand('revenant.clearGhost', clearAllGhosts),
    vscode.commands.registerCommand('revenant.history', showHistory),
    vscode.commands.registerCommand('revenant.resume', resumeAtAnchor)
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
      // The cursor position IS the resume point: keep the daemon's view
      // of it fresh, debounced so typing doesn't hammer the disk
      scheduleStateReport();
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
      // Card is for a different project - ignore
      return;
    }
  }

  activeGhostCard = card;
  activityTracker?.reset();

  renderGhost(card);
  updateStatusBar(card);

  // Ghosts are transient: 1 minute by default, configurable upward
  const ghostSeconds = vscode.workspace
    .getConfiguration('revenant')
    .get<number>('ghostSeconds', 60);

  // The north star: offer to put the cursor back where your head was
  if (card.anchor && fs.existsSync(card.anchor.file)) {
    const anchorName = path.basename(card.anchor.file);
    vscode.window
      .showInformationMessage(
        `REVENANT: ${card.summary}`,
        `Resume at ${anchorName}:${card.anchor.line}`
      )
      .then((choice) => {
        if (choice) {
          resumeAtAnchor();
        }
      });
  }

  // Set self-destruct timer
  if (ghostTimer) {
    clearTimeout(ghostTimer);
  }
  ghostTimer = setTimeout(() => {
    clearAllGhosts();
  }, ghostSeconds * 1000);
}

/**
 * Open the anchored file, restore the cursor to the departure line,
 * and center it. One action from "where was I" to "back in it".
 */
async function resumeAtAnchor(): Promise<void> {
  const anchor = activeGhostCard?.anchor;
  if (!anchor) {
    vscode.window.showInformationMessage(
      'REVENANT: No resume point on the current ghost.'
    );
    return;
  }
  if (!fs.existsSync(anchor.file)) {
    vscode.window.showWarningMessage(
      `REVENANT: Resume point ${anchor.file} no longer exists.`
    );
    return;
  }

  const doc = await vscode.workspace.openTextDocument(anchor.file);
  const editor = await vscode.window.showTextDocument(doc);
  // Clamp: the file may have shrunk since departure
  const line = Math.min(Math.max(anchor.line - 1, 0), doc.lineCount - 1);
  const pos = new vscode.Position(line, 0);
  editor.selection = new vscode.Selection(pos, pos);
  editor.revealRange(
    new vscode.Range(pos, pos),
    vscode.TextEditorRevealType.InCenter
  );
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

  // Create decoration type with inline "after" content - purely visual
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

  // Anchor the ghost at the exact line you left, when this editor is
  // showing that file; otherwise it floats at the top of the file
  let line = 0;
  if (card.anchor && editor.document.uri.fsPath === card.anchor.file) {
    line = Math.min(
      Math.max(card.anchor.line - 1, 0),
      editor.document.lineCount - 1
    );
  }
  const range = new vscode.Range(line, 0, line, 0);
  editor.setDecorations(ghostDecorationType, [range]);
}

function updateStatusBar(card: ContextCard | undefined): void {
  if (!statusBarItem) return;

  if (card) {
    statusBarItem.text = '$(eye) REVENANT';
    statusBarItem.tooltip = card.anchor
      ? `Ghost active: ${card.summary} (click to resume at ${path.basename(card.anchor.file)}:${card.anchor.line})`
      : `Ghost active: ${card.summary}`;
    statusBarItem.command = card.anchor ? 'revenant.resume' : 'revenant.history';
    statusBarItem.backgroundColor = new vscode.ThemeColor(
      'statusBarItem.warningBackground'
    );
  } else {
    statusBarItem.text = '$(eye-closed)';
    statusBarItem.tooltip = 'REVENANT - no active ghost';
    statusBarItem.command = 'revenant.history';
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

  // If user has been active for longer than the configured ghost
  // lifetime, clear the ghost
  const ghostSeconds = vscode.workspace
    .getConfiguration('revenant')
    .get<number>('ghostSeconds', 60);
  const activeSeconds = activityTracker.activeSeconds();
  if (activeSeconds >= ghostSeconds) {
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

/** Debounced reportEditorState for high-frequency events (cursor moves) */
function scheduleStateReport(): void {
  if (stateReportTimer) {
    clearTimeout(stateReportTimer);
  }
  stateReportTimer = setTimeout(() => reportEditorState(), 1500);
}

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

  const activeEditor = vscode.window.activeTextEditor;
  const activeFile =
    activeEditor && activeEditor.document.uri.scheme === 'file'
      ? activeEditor.document.uri.fsPath
      : undefined;
  // 1-based, so the daemon and the card speak human line numbers
  const activeLine =
    activeFile && activeEditor ? activeEditor.selection.active.line + 1 : null;

  const state = {
    projectDir,
    openFiles,
    activeFile: activeFile || null,
    activeLine,
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
