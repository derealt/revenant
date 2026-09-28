/**
 * REVENANT Ghost - Chrome Extension Content Script
 *
 * Injects a subtle, dismissible banner at the top of web pages
 * carrying the context card. The banner is minimal, translucent,
 * and auto-dismisses with the ghost TTL.
 */

interface ContextCard {
  id: string;
  project_name: string;
  summary: string;
  next_step: string;
  ttl_seconds: number;
}

const BANNER_ID = 'revenant-ghost-banner';

let bannerFadeTimer: number | undefined;

// ─── Banner Rendering ──────────────────────────────────────────────

function showBanner(card: ContextCard): void {
  // Remove existing banner if any
  removeBanner();

  const banner = document.createElement('div');
  banner.id = BANNER_ID;
  banner.setAttribute('role', 'status');
  banner.setAttribute('aria-live', 'polite');

  // Ghost icon + content
  const icon = document.createElement('span');
  icon.className = 'revenant-icon';
  icon.textContent = '\u{1F47B}'; // ghost emoji

  const content = document.createElement('div');
  content.className = 'revenant-content';

  const label = document.createElement('span');
  label.className = 'revenant-label';
  label.textContent = 'REVENANT';

  const summary = document.createElement('span');
  summary.className = 'revenant-summary';
  summary.textContent = card.summary;

  content.appendChild(label);
  content.appendChild(summary);

  if (card.next_step) {
    const next = document.createElement('span');
    next.className = 'revenant-next';
    next.textContent = `Next: ${card.next_step}`;
    content.appendChild(next);
  }

  // Dismiss button
  const dismiss = document.createElement('button');
  dismiss.className = 'revenant-dismiss';
  dismiss.textContent = '\u{2715}'; // multiplication sign (x)
  dismiss.title = 'Dismiss';
  dismiss.addEventListener('click', (e) => {
    e.stopPropagation();
    removeBanner();
    chrome.runtime.sendMessage({ type: 'dismissGhost' });
  });

  banner.appendChild(icon);
  banner.appendChild(content);
  banner.appendChild(dismiss);

  // Insert at top of body
  document.body.insertBefore(banner, document.body.firstChild);

  // Push body content down
  document.body.style.marginTop = '48px';

  // Fade-in animation
  requestAnimationFrame(() => {
    banner.classList.add('revenant-visible');
  });

  // Ghosts are transient: the daemon serves the banner TTL in
  // ttl_seconds (1 minute by default, configurable in config.toml).
  // This local timer is the backstop so the banner dies even if no
  // poll delivers the clear.
  if (bannerFadeTimer !== undefined) {
    clearTimeout(bannerFadeTimer);
  }
  const ttl = card.ttl_seconds > 0 ? card.ttl_seconds : 60;
  bannerFadeTimer = window.setTimeout(() => removeBanner(), ttl * 1000);
}

function removeBanner(): void {
  if (bannerFadeTimer !== undefined) {
    clearTimeout(bannerFadeTimer);
    bannerFadeTimer = undefined;
  }
  const existing = document.getElementById(BANNER_ID);
  if (existing) {
    existing.classList.remove('revenant-visible');
    existing.classList.add('revenant-fading');
    setTimeout(() => {
      existing.remove();
      document.body.style.marginTop = '';
    }, 300);
  }
}

// ─── Message Listener ──────────────────────────────────────────────

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  switch (message.type) {
    case 'showGhost':
      if (message.card) {
        showBanner(message.card);
      }
      sendResponse({ ok: true });
      break;
    case 'clearGhost':
      removeBanner();
      sendResponse({ ok: true });
      break;
  }
  return true;
});

// ─── Check for Active Ghost on Page Load ───────────────────────────

chrome.runtime.sendMessage({ type: 'getActiveGhost' }, (response) => {
  if (response?.card) {
    showBanner(response.card);
  }
});
