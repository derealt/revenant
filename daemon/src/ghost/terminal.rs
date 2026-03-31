//! Terminal ghost — writes context cards to ~/.revenant/motd
//!
//! The shell integration scripts (revenant.zsh, revenant.bash, revenant.fish)
//! source this file on every new shell session, displaying the ghost
//! as a subtle MOTD that fades after the TTL expires.

use anyhow::Result;
use chrono::Utc;
use std::path::PathBuf;

use crate::config::TerminalGhostConfig;
use crate::store::ContextCard;

pub struct TerminalGhost {
    motd_path: PathBuf,
}

impl TerminalGhost {
    pub fn new(config: &TerminalGhostConfig) -> Result<Self> {
        let motd_path = crate::expand_path(&config.motd_file);
        if let Some(parent) = motd_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self { motd_path })
    }

    /// Write the context card as a terminal MOTD
    pub async fn inject(&self, card: &ContextCard) -> Result<()> {
        let now = Utc::now();
        let age = now.signed_duration_since(card.created_at);
        let age_str = format_duration(age);

        // Build the MOTD content
        // Uses ANSI escape codes for subtle styling
        let motd = format!(
            concat!(
                "\x1b[2m", // dim
                "┌─── REVENANT ─── {project} ─── {age} ago ───\x1b[0m\n",
                "\x1b[2m│\x1b[0m \x1b[36m{summary}\x1b[0m\n",
                "{next_line}",
                "\x1b[2m└─── ghost expires in {ttl}min of activity ───\x1b[0m\n",
            ),
            project = card.project_name,
            age = age_str,
            summary = card.summary,
            next_line = if card.next_step.is_empty() {
                String::new()
            } else {
                format!(
                    "\x1b[2m│\x1b[0m \x1b[33mNext: {}\x1b[0m\n",
                    card.next_step
                )
            },
            ttl = card.ttl_seconds / 60,
        );

        // Write the MOTD file atomically (write to temp, rename)
        let tmp_path = self.motd_path.with_extension("tmp");
        tokio::fs::write(&tmp_path, &motd).await?;
        tokio::fs::rename(&tmp_path, &self.motd_path).await?;

        // Also write a machine-readable metadata file for the shell scripts
        let meta = serde_json::json!({
            "card_id": card.id,
            "project": card.project_name,
            "created_at": card.created_at.to_rfc3339(),
            "ttl_seconds": card.ttl_seconds,
            "expires_at": (card.created_at + chrono::Duration::seconds(card.ttl_seconds as i64)).to_rfc3339(),
        });
        let meta_path = self.motd_path.with_extension("json");
        tokio::fs::write(&meta_path, serde_json::to_string_pretty(&meta)?).await?;

        Ok(())
    }

    /// Clear the MOTD file
    pub async fn clear(&self) -> Result<()> {
        if self.motd_path.exists() {
            tokio::fs::remove_file(&self.motd_path).await?;
        }
        let meta_path = self.motd_path.with_extension("json");
        if meta_path.exists() {
            tokio::fs::remove_file(&meta_path).await?;
        }
        Ok(())
    }
}

/// Format a chrono Duration into a human-readable string
fn format_duration(d: chrono::Duration) -> String {
    let total_seconds = d.num_seconds().max(0);

    if total_seconds < 60 {
        "just now".to_string()
    } else if total_seconds < 3600 {
        let mins = total_seconds / 60;
        format!("{mins}min")
    } else if total_seconds < 86400 {
        let hours = total_seconds / 3600;
        let mins = (total_seconds % 3600) / 60;
        if mins > 0 {
            format!("{hours}h {mins}min")
        } else {
            format!("{hours}h")
        }
    } else {
        let days = total_seconds / 86400;
        let hours = (total_seconds % 86400) / 3600;
        if hours > 0 {
            format!("{days}d {hours}h")
        } else {
            format!("{days}d")
        }
    }
}
