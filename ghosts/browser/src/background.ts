/**
 * REVENANT Ghost — Chrome Extension Background Service Worker
 *
 * Maintains connection to the daemon via Chrome's native messaging API.
 * Receives context cards and forwards them to the content script for
 * rendering as a subtle banner on the active tab.
 *
 * Also reports the current active tab URL/title back to the daemon
 * as a browser signal.
 */

const NATIVE_HOST = 'com.revenant.ghost';

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

let activeCard: ContextCard | null = null;
let ghostTimeout: ReturnType<typeof setTimeout> | null = null;
let nativePort: chrome.runtime.Port | null = null;

// ─── Native Messaging ──────────────────────────────────────────────

function connectToNativeHost(): void {
  try {
    nativePort = chrome.runtime.connectNative(NATIVE_HOST);

    nativePort.onMessage.addListener((message: DaemonMessage) => {
      handleDaemonMessage(message);
    });

    nativePort.onDisconnect.addListener(() => {
      console.log('REVENANT: native host disconnected');
      nativePort = null;
      // Retry connection after 10 seconds
      setTimeout(connectToNativeHost, 10000);
    });

    console.log('REVENANT: connected to native host');
  } catch (err) {
    console.log('REVENANT: native host not available, using file-based communication');
    // Fall back to polling the browser-state file
    startFilePolling();
  }
}

// ─── File-based Communication Fallback ─────────────────────────────

function startFilePolling(): void {
  // Report active tab periodically so the daemon has browser signals
  setInterval(reportActiveTab, 5000);
}

async function reportActiveTab(): Promise<void> {
  try {
    const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
    if (tab?.url && tab?.title) {
      // Store for the daemon to read
      await chrome.storage.local.set({
        activeTab: {
          url: tab.url,
          title: tab.title,
          timestamp: Date.now(),
        },
      });
    }
  } catch {
    // Ignore errors — tab might not be accessible
  }
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
      clearGhost();
      break;
  }
}

async function injectGhost(card: ContextCard): Promise<void> {
  activeCard = card;

  // Store the card
  await chrome.storage.local.set({ activeGhost: card });

  // Send to all active tabs' content scripts
  const tabs = await chrome.tabs.query({ active: true });
  for (const tab of tabs) {
    if (tab.id) {
      try {
        await chrome.tabs.sendMessage(tab.id, {
          type: 'showGhost',
          card,
        });
      } catch {
        // Content script might not be loaded on this tab
      }
    }
  }

  // Set self-destruct timer
  if (ghostTimeout) {
    clearTimeout(ghostTimeout);
  }
  ghostTimeout = setTimeout(() => {
    clearGhost();
  }, card.ttl_seconds * 1000);

  // Update badge
  await chrome.action.setBadgeText({ text: '!' });
  await chrome.action.setBadgeBackgroundColor({ color: '#4EC9B0' });
}

async function clearGhost(): Promise<void> {
  activeCard = null;
  await chrome.storage.local.remove('activeGhost');

  if (ghostTimeout) {
    clearTimeout(ghostTimeout);
    ghostTimeout = null;
  }

  // Clear from all tabs
  const tabs = await chrome.tabs.query({});
  for (const tab of tabs) {
    if (tab.id) {
      try {
        await chrome.tabs.sendMessage(tab.id, { type: 'clearGhost' });
      } catch {
        // ignore
      }
    }
  }

  await chrome.action.setBadgeText({ text: '' });
}

// ─── Tab Change Listener ───────────────────────────────────────────

chrome.tabs.onActivated.addListener(async (activeInfo) => {
  // Report tab change as browser signal
  reportActiveTab();

  // Show ghost on newly activated tab if one is active
  if (activeCard) {
    try {
      await chrome.tabs.sendMessage(activeInfo.tabId, {
        type: 'showGhost',
        card: activeCard,
      });
    } catch {
      // Content script not loaded
    }
  }
});

// ─── Message from Content Script / Popup ───────────────────────────

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message.type === 'getActiveGhost') {
    sendResponse({ card: activeCard });
  } else if (message.type === 'dismissGhost') {
    clearGhost();
    sendResponse({ ok: true });
  }
  return true; // async response
});

// ─── Initialize ────────────────────────────────────────────────────

connectToNativeHost();
