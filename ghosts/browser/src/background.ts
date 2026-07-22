// REVENANT Ghost - Chrome Extension Background Service Worker
//
// MV3 service workers are suspended after ~30s of idle: setInterval dies
// and in-memory state is wiped. So the heartbeat is a chrome.alarms timer
// (which wakes the worker), state lives in chrome.storage.session (which
// survives suspends), and page loads trigger a live fetch when the cache
// is cold. Polls also fire on tab activation and navigation, which is
// when a ghost actually needs to appear.

const DAEMON_URL = "http://127.0.0.1:7711/ghost";
const TAB_REPORT_URL = "http://127.0.0.1:7711/tab";
const POLL_ALARM = "revenant-poll";

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

interface GhostState {
  active: ContextCard | null;
  dismissedId: string | null;
}

// ─── State (survives service worker suspends) ──────────────────────

async function getState(): Promise<GhostState> {
  const s = await chrome.storage.session.get(["active", "dismissedId"]);
  return {
    active: (s.active as ContextCard | undefined) ?? null,
    dismissedId: (s.dismissedId as string | undefined) ?? null,
  };
}

async function setState(patch: Partial<GhostState>): Promise<void> {
  await chrome.storage.session.set(patch);
}

// ─── Daemon I/O ────────────────────────────────────────────────────

async function fetchGhost(): Promise<ContextCard | null> {
  try {
    const resp = await fetch(DAEMON_URL);
    if (!resp.ok) return null;
    const msg: DaemonMessage = await resp.json();
    return msg.type === "inject" && msg.card ? msg.card : null;
  } catch {
    // Daemon not running
    return null;
  }
}

// ─── Broadcast to tabs ─────────────────────────────────────────────

async function broadcast(message: object): Promise<void> {
  const tabs = await chrome.tabs.query({});
  for (const tab of tabs) {
    if (tab.id && tab.url && /^https?:/.test(tab.url)) {
      chrome.tabs.sendMessage(tab.id, message).catch(() => {});
    }
  }
}

function setBadge(active: boolean): void {
  chrome.action.setBadgeText({ text: active ? "!" : "" });
  if (active) {
    chrome.action.setBadgeBackgroundColor({ color: "#0a8a7e" });
  }
}

// ─── Poll ──────────────────────────────────────────────────────────

async function pollDaemon(): Promise<void> {
  const card = await fetchGhost();
  const { active, dismissedId } = await getState();

  if (card && card.id !== dismissedId) {
    if (!active || active.id !== card.id) {
      await setState({ active: card });
      await broadcast({ type: "showGhost", card });
      setBadge(true);
    }
  } else if (!card && active) {
    await setState({ active: null });
    await broadcast({ type: "clearGhost" });
    setBadge(false);
  }
}

// ─── Heartbeat: alarms wake the worker, unlike setInterval ─────────

chrome.alarms.create(POLL_ALARM, { periodInMinutes: 0.5 });
chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === POLL_ALARM) pollDaemon();
});

// Poll whenever the worker boots (install, browser start, wake-up)
pollDaemon();

// ─── Messages from content scripts and popup ───────────────────────

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg.type === "getActiveGhost") {
    (async () => {
      const { active, dismissedId } = await getState();
      if (active) {
        sendResponse({ card: active });
        return;
      }
      // Cold cache (worker just woke): ask the daemon directly
      const card = await fetchGhost();
      if (card && card.id !== dismissedId) {
        await setState({ active: card });
        setBadge(true);
        sendResponse({ card });
      } else {
        sendResponse({ card: null });
      }
    })();
    return true; // keep sendResponse alive for the async reply
  }

  if (msg.type === "dismissGhost") {
    (async () => {
      const { active } = await getState();
      await setState({ active: null, dismissedId: active?.id ?? null });
      await broadcast({ type: "clearGhost" });
      setBadge(false);
      sendResponse({ ok: true });
    })();
    return true;
  }

  return false;
});

// ─── Tab events: re-show the ghost and report the active tab ───────

async function reportActiveTab(): Promise<void> {
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

chrome.tabs.onActivated.addListener(async (info) => {
  reportActiveTab();
  await pollDaemon();
  const { active } = await getState();
  if (active && info.tabId) {
    setTimeout(() => {
      chrome.tabs.sendMessage(info.tabId, { type: "showGhost", card: active }).catch(() => {});
    }, 500);
  }
});

chrome.tabs.onUpdated.addListener((_tabId, changeInfo) => {
  if (changeInfo.status === "complete") {
    reportActiveTab();
    pollDaemon();
  }
});
