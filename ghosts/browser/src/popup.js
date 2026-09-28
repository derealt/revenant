// REVENANT Ghost - popup script
// Separate file because MV3 extension-page CSP blocks inline scripts.

const INSTALL_URL = "https://github.com/derealt/revenant#install";

function show(container, className, text) {
  container.textContent = "";
  const el = document.createElement("div");
  el.className = className;
  el.textContent = text;
  container.appendChild(el);
  return el;
}

chrome.runtime.sendMessage({ type: "getActiveGhost" }, (response) => {
  const container = document.getElementById("content");

  if (response && response.running === false) {
    const el = show(container, "setup",
      "The Revenant app is not running on this computer. It writes the notes this extension shows. ");
    const link = document.createElement("a");
    link.href = INSTALL_URL;
    link.target = "_blank";
    link.rel = "noopener";
    link.textContent = "Install Revenant for Mac or Linux";
    el.appendChild(link);
    return;
  }

  if (!response?.card) {
    show(container, "no-ghost",
      "Nothing to show right now. When you come back to your computer, your note appears here and at the top of the page.");
    return;
  }

  const card = response.card;
  container.textContent = "";

  const cardEl = document.createElement("div");
  cardEl.className = "card";

  const project = document.createElement("div");
  project.className = "card-project";
  project.textContent = card.project_name;
  cardEl.appendChild(project);

  const summary = document.createElement("div");
  summary.className = "card-summary";
  summary.textContent = card.summary;
  cardEl.appendChild(summary);

  if (card.next_step) {
    const next = document.createElement("div");
    next.className = "card-next";
    next.textContent = "Next: " + card.next_step;
    cardEl.appendChild(next);
  }

  container.appendChild(cardEl);

  const dismiss = document.createElement("button");
  dismiss.className = "dismiss-btn";
  dismiss.textContent = "Dismiss";
  dismiss.addEventListener("click", () => {
    chrome.runtime.sendMessage({ type: "dismissGhost" }, () => {
      show(container, "no-ghost", "Dismissed.");
    });
  });
  container.appendChild(dismiss);
});
