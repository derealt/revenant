//! Slack ghost — communicates with the Go Slack bot via Unix socket
//!
//! The daemon sends JSON messages over a Unix socket to a Go sidecar
//! that handles the actual Slack API calls (posting ephemeral messages,
//! pinning/unpinning). Same newline-delimited JSON protocol as vscode.rs.
//!
//! Socket protocol:
//!   {"type": "inject", "card": { ... }}\n
//!   {"type": "clear"}\n
//!
//! The Go bot listens on ~/.revenant/slack.sock and translates these
//! into Slack API calls (chat.postEphemeral for inject, message deletion
//! for clear).

use anyhow::Result;
use std::path::PathBuf;

use crate::config::SlackGhostConfig;
use crate::store::ContextCard;

pub struct SlackGhost {
    socket_path: PathBuf,
}

impl SlackGhost {
    pub fn new(_config: &SlackGhostConfig) -> Result<Self> {
        // The socket path is fixed by convention — the Go bot
        // and Rust daemon agree on ~/.revenant/slack.sock
        let socket_path = crate::expand_path("~/.revenant/slack.sock");
        Ok(Self { socket_path })
    }

    /// Send a context card to the Slack bot for ephemeral posting
    pub async fn inject(&self, card: &ContextCard) -> Result<()> {
        let message = serde_json::json!({
            "type": "inject",
            "card": {
                "id": card.id,
                "project_dir": card.project_dir,
                "project_name": card.project_name,
                "summary": card.summary,
                "next_step": card.next_step,
                "ttl_seconds": card.ttl_seconds,
            }
        });

        self.send_message(&message).await
    }

    /// Tell the Slack bot to remove/delete the ghost message
    pub async fn clear(&self) -> Result<()> {
        let message = serde_json::json!({
            "type": "clear",
        });

        self.send_message(&message).await
    }

    /// Send a newline-delimited JSON message over the Unix socket
    async fn send_message(&self, message: &serde_json::Value) -> Result<()> {
        if !self.socket_path.exists() {
            // Slack bot not running — not an error, just skip
            tracing::debug!(
                "slack socket not found at {}, bot not running",
                self.socket_path.display()
            );
            return Ok(());
        }

        let payload = serde_json::to_string(message)? + "\n";

        // Connect and send via Unix socket
        match tokio::net::UnixStream::connect(&self.socket_path).await {
            Ok(stream) => {
                use tokio::io::AsyncWriteExt;
                let (_, mut writer) = stream.into_split();
                writer.write_all(payload.as_bytes()).await?;
                writer.shutdown().await?;
                tracing::debug!("slack ghost message sent");
                Ok(())
            }
            Err(e) => {
                tracing::debug!("could not connect to slack socket: {e}");
                Ok(()) // Non-fatal — bot may not be running
            }
        }
    }

    pub fn name(&self) -> &str {
        "slack"
    }
}
