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
        let message = build_inject_message(card);
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

/// Build the inject message for the editor extension, carrying the
/// resume anchor when the captured signals hold one. signals_json is
/// the serialized WorkingState; the anchor lives at editor.cursor.
pub fn build_inject_message(card: &ContextCard) -> serde_json::Value {
    let anchor = serde_json::from_str::<serde_json::Value>(&card.signals_json)
        .ok()
        .and_then(|signals| signals.get("editor")?.get("cursor").cloned())
        .filter(|c| !c.is_null());

    serde_json::json!({
        "type": "inject",
        "card": {
            "id": card.id,
            "project_dir": card.project_dir,
            "project_name": card.project_name,
            "summary": card.summary,
            "next_step": card.next_step,
            "ttl_seconds": card.ttl_seconds,
            "anchor": anchor,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn card_with_signals(signals_json: &str) -> ContextCard {
        ContextCard {
            id: "t1".into(),
            project_dir: "/tmp/p".into(),
            project_name: "p".into(),
            summary: "You were testing.".into(),
            next_step: "Continue.".into(),
            created_at: Utc::now(),
            signals_json: signals_json.into(),
            ttl_seconds: 120,
        }
    }

    #[test]
    fn inject_message_carries_cursor_anchor() {
        let card = card_with_signals(
            r#"{"editor":{"open_files":[],"active_file":"src/a.rs","active_language":"rust","cursor":{"file":"/tmp/p/src/a.rs","line":47}}}"#,
        );
        let msg = build_inject_message(&card);
        assert_eq!(msg["card"]["anchor"]["file"], "/tmp/p/src/a.rs");
        assert_eq!(msg["card"]["anchor"]["line"], 47);
    }

    #[test]
    fn inject_message_omits_anchor_when_absent() {
        for signals in ["{}", r#"{"editor":null}"#, r#"{"editor":{"cursor":null}}"#, "not json"] {
            let msg = build_inject_message(&card_with_signals(signals));
            assert!(
                msg["card"]["anchor"].is_null(),
                "expected null anchor for signals {signals}"
            );
        }
    }
}
