// REVENANT Ghost - Chrome Extension Background Service Worker
// Polls the local daemon's HTTP endpoint for ghost cards.

const DAEMON_URL = "http://127.0.0.1:7711/ghost";
const TAB_REPORT_URL = "http://127.0.0.1:7711/tab";
const POLL_INTERVAL = 3000;

interface ContextCard {
  id: string;
  summary: string;
  next_step: string;
  project_dir: string;
  project_name: string;
  ttl_seconds: number;
}

interface DaemonMessage {
  type: "inject" | "clear";
  card?: ContextCard;
}

let activeCard: ContextCard | null = null;

async function pollDaemon() {
  try {
    const resp = await fetch(DAEMON_URL);
    if (!resp.ok) return;
    const msg: DaemonMessage = await resp.json();

    if (msg.type === "inject" && msg.card) {
      if (!activeCard || activeCard.id !== msg.card.id) {
        activeCard = msg.card;
        const tabs = await chrome.tabs.query({});
        for (const tab of tabs) {
          if (tab.id && tab.url && !tab.url.startsWith("chrome://")) {
            chrome.tabs.sendMessage(tab.id, { type: "showGhost", card: activeCard }).catch(() => {});
          }
        }
        chrome.action.setBadgeText({ text: "!" });
        chrome.action.setBadgeBackgroundColor({ color: "#0a8a7e" });
      }
    } else if (msg.type === "clear" && activeCard) {
      activeCard = null;
      const tabs = await chrome.tabs.query({});
      for (const tab of tabs) {
        if (tab.id) chrome.tabs.sendMessage(tab.id, { type: "clearGhost" }).catch(() => {});
      }
      chrome.action.setBadgeText({ text: "" });
    }
  } catch {
    // Daemon not running
  }
}

setInterval(pollDaemon, POLL_INTERVAL);
pollDaemon();

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg.type === "getActiveGhost") {
    sendResponse({ card: activeCard });
  } else if (msg.type === "dismissGhost") {
    activeCard = null;
    chrome.tabs.query({}).then(tabs => {
      for (const tab of tabs) {
        if (tab.id) chrome.tabs.sendMessage(tab.id, { type: "clearGhost" }).catch(() => {});
      }
    });
    chrome.action.setBadgeText({ text: "" });
    sendResponse({ ok: true });
  }
  return true;
});

chrome.tabs.onActivated.addListener(async (info) => {
  if (activeCard && info.tabId) {
    setTimeout(() => {
      chrome.tabs.sendMessage(info.tabId, { type: "showGhost", card: activeCard }).catch(() => {});
    }, 500);
  }
});

// Report the active tab to the daemon as a context signal.
// The daemon discards these unless the user set `signals.browser = true`,
// so nothing is stored without an explicit opt-in.
async function reportActiveTab() {
  try {
    const [tab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
    if (!tab?.url || !/^https?:/.test(tab.url)) return;
    await fetch(TAB_REPORT_URL, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify([
        {
          url: tab.url,
          title: tab.title ?? "",
          timestamp: Math.floor(Date.now() / 1000),
        },
      ]),
    });
  } catch {
    // Daemon not running
  }
}

chrome.tabs.onActivated.addListener(() => { reportActiveTab(); });
chrome.tabs.onUpdated.addListener((_tabId, changeInfo) => {
  if (changeInfo.status === "complete") reportActiveTab();
});
