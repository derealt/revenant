//! VS Code ghost - communicates with the extension via Unix socket
//!
//! The daemon writes JSON messages to a Unix socket that the VS Code
//! extension listens on. The extension renders inline decorations
//! (visual overlays that don't modify the file).

use anyhow::Result;
use std::path::PathBuf;

use crate::config::VscodeGhostConfig;
use crate::store::ContextCard;

pub struct VscodeGhost {
    socket_path: PathBuf,
}

impl VscodeGhost {
    pub fn new(config: &VscodeGhostConfig) -> Result<Self> {
        let socket_path = crate::expand_path(&config.socket_path);
        Ok(Self { socket_path })
    }

    /// Send a context card to the VS Code extension via Unix socket
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

    /// Tell the VS Code extension to clear all ghost decorations
    pub async fn clear(&self) -> Result<()> {
        let message = serde_json::json!({
            "type": "clear",
        });

        self.send_message(&message).await
    }

    async fn send_message(&self, message: &serde_json::Value) -> Result<()> {
        if !self.socket_path.exists() {
            // Extension not running - not an error, just skip
            tracing::debug!(
                "vscode socket not found at {}, extension not running",
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
                Ok(())
            }
            Err(e) => {
                tracing::debug!("could not connect to vscode socket: {e}");
                Ok(()) // Non-fatal - extension may not be running
            }
        }
    }
}
