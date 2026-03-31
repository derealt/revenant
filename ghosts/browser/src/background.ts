// REVENANT Ghost — Chrome Extension Background Service Worker
// Polls the local daemon's HTTP endpoint for ghost cards.

const DAEMON_URL = "http://127.0.0.1:7711/ghost";
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
