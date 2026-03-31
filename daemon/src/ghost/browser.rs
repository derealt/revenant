//! Browser ghost — writes state file for the browser extension to poll
//!
//! The browser extension has two communication modes:
//! 1. Native messaging (Chrome/Firefox) — direct IPC, lower latency
//! 2. File polling — reads ~/.revenant/browser-state.json periodically
//!
//! This ghost writes the state file. The extension's background script
//! falls back to polling this file when native messaging is unavailable
//! (which is common -- native messaging requires extra OS-level setup).
//!
//! State file format (same protocol as obsidian.rs):
//!   {"type": "inject", "card": { ... }}   -- show the ghost banner
//!   {"type": "clear"}                     -- remove the banner
//!
//! Atomic writes prevent partial reads.

use anyhow::Result;
use std::path::PathBuf;

use crate::store::ContextCard;

pub struct BrowserGhost {
    state_path: PathBuf,
}

impl BrowserGhost {
    pub fn new() -> Result<Self> {
        let state_path = crate::expand_path("~/.revenant/browser-state.json");
        if let Some(parent) = state_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self { state_path })
    }

    /// Write a context card to the state file for the browser extension
    pub async fn inject(&self, card: &ContextCard) -> Result<()> {
        let state = serde_json::json!({
            "type": "inject",
            "card": {
                "id": card.id,
                "project_dir": card.project_dir,
                "project_name": card.project_name,
                "summary": card.summary,
                "next_step": card.next_step,
                "ttl_seconds": card.ttl_seconds,
                "created_at": card.created_at.to_rfc3339(),
            },
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });

        self.write_state(&state).await
    }

    /// Write a clear signal to the state file
    pub async fn clear(&self) -> Result<()> {
        let state = serde_json::json!({
            "type": "clear",
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });

        self.write_state(&state).await
    }

    /// Atomic write: write to .tmp, then rename
    async fn write_state(&self, state: &serde_json::Value) -> Result<()> {
        let content = serde_json::to_string_pretty(state)?;
        let tmp_path = self.state_path.with_extension("json.tmp");

        tokio::fs::write(&tmp_path, &content).await?;
        tokio::fs::rename(&tmp_path, &self.state_path).await?;

        tracing::debug!(
            "browser state written to {}",
            self.state_path.display()
        );
        Ok(())
    }

    pub fn name(&self) -> &str {
        "browser"
    }
}
