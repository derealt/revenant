//! Obsidian ghost - writes state file for the Obsidian plugin to poll
//!
//! Unlike VS Code and Slack which use Unix sockets, the Obsidian ghost
//! communicates via a JSON state file at ~/.revenant/obsidian-state.json.
//! The Obsidian plugin polls this file periodically and renders a
//! transient callout block when it finds an active context card.
//!
//! State file format:
//!   {"type": "inject", "card": { ... }}   -- show the ghost
//!   {"type": "clear"}                     -- remove the ghost
//!
//! The file is written atomically (write tmp + rename) to prevent
//! the plugin from reading a partial file.

use anyhow::Result;
use std::path::PathBuf;

use crate::config::ObsidianGhostConfig;
use crate::store::ContextCard;

pub struct ObsidianGhost {
    state_path: PathBuf,
}

impl ObsidianGhost {
    pub fn new(_config: &ObsidianGhostConfig) -> Result<Self> {
        let state_path = crate::expand_path("~/.revenant/obsidian-state.json");
        if let Some(parent) = state_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self { state_path })
    }

    /// Write a context card to the state file for the Obsidian plugin
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
            "obsidian state written to {}",
            self.state_path.display()
        );
        Ok(())
    }

    pub fn name(&self) -> &str {
        "obsidian"
    }
}
