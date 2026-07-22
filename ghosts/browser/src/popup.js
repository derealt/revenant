// REVENANT Ghost - popup script
// Separate file because MV3 extension-page CSP blocks inline scripts.

chrome.runtime.sendMessage({ type: "getActiveGhost" }, (response) => {
  const container = document.getElementById("content");
  if (!response?.card) return;

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
  dismiss.textContent = "Dismiss Ghost";
  dismiss.addEventListener("click", () => {
    chrome.runtime.sendMessage({ type: "dismissGhost" }, () => {
      container.textContent = "";
      const done = document.createElement("div");
      done.className = "no-ghost";
      done.textContent = "Ghost dismissed.";
      container.appendChild(done);
    });
  });
  container.appendChild(dismiss);
});
