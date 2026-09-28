//! Terminal ghost - writes context cards to ~/.revenant/motd
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
        let motd = render_motd(card);

        // Write the MOTD file atomically (write to temp, rename)
        let tmp_path = self.motd_path.with_extension("tmp");
        tokio::fs::write(&tmp_path, &motd).await?;
        tokio::fs::rename(&tmp_path, &self.motd_path).await?;

        // Also write a machine-readable metadata file for the shell scripts.
        // Expiry runs from injection, not card creation: a card restored
        // hours after it was banked must not arrive pre-expired.
        let meta = serde_json::json!({
            "card_id": card.id,
            "project": card.project_name,
            "created_at": card.created_at.to_rfc3339(),
            "ttl_seconds": card.ttl_seconds,
            "expires_at": (Utc::now() + chrono::Duration::seconds(card.ttl_seconds as i64)).to_rfc3339(),
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

/// Render a context card as the ANSI terminal ghost. Shared by the
/// MOTD writer and the /shell-open HTTP endpoint, so a freshly opened
/// shell and a pre-written MOTD always look identical.
pub fn render_motd(card: &ContextCard) -> String {
    let now = Utc::now();
    let age = now.signed_duration_since(card.created_at);
    let age_str = format_duration(age);
    let ttl_min = card.ttl_seconds / 60;

    // Intent-based accent color
    let accent = intent_color(&card.summary);
    let dim = "\x1b[2m";
    let reset = "\x1b[0m";
    let white = "\x1b[97m";
    let gold = "\x1b[38;5;178m";

    // Build premium MOTD with double-line borders and visual hierarchy
    let mut motd = String::with_capacity(512);

    // Top border with project name centered
    // "2h ago", but "just now" never gets a dangling "ago"
    let when = if age_str == "just now" { age_str.to_string() } else { format!("{age_str} ago") };
    let header = format!(" REVENANT \u{2502} {} \u{2502} {} ", card.project_name, when);
    motd.push_str(&format!(
        "{dim}\u{2554}\u{2550}\u{2550}{reset}{accent} {header}{reset}{dim} \u{2550}\u{2550}\u{2557}{reset}\n",
    ));

    // Summary line - white for legibility
    motd.push_str(&format!(
        "{dim}\u{2551}{reset} {white}{summary}{reset}\n",
        summary = card.summary,
    ));

    // Next step line - gold accent
    if !card.next_step.is_empty() {
        motd.push_str(&format!(
            "{dim}\u{2551}{reset} {gold}\u{2192} {next}{reset}\n",
            next = card.next_step,
        ));
    }

    // Bottom border with ghost TTL
    motd.push_str(&format!(
        "{dim}\u{255a}\u{2550}\u{2550} ghost fades in {ttl_min}min of activity \u{2550}\u{2550}\u{255d}{reset}\n",
    ));

    motd
}

/// Map card content to an intent-based ANSI accent color
fn intent_color(summary: &str) -> &'static str {
    let lower = summary.to_lowercase();
    if lower.contains("fixing") || lower.contains("fix ") || lower.contains("bug") {
        "\x1b[38;5;203m" // red-coral for fixes
    } else if lower.contains("testing") || lower.contains("test ") || lower.contains("tests") {
        "\x1b[38;5;114m" // green for tests
    } else if lower.contains("deploying") || lower.contains("deploy") || lower.contains("release") {
        "\x1b[38;5;213m" // pink for deploys
    } else if lower.contains("refactoring") || lower.contains("cleaning") {
        "\x1b[38;5;110m" // blue for refactors
    } else if lower.contains("building") || lower.contains("adding") || lower.contains("creating") {
        "\x1b[38;5;178m" // gold for building
    } else {
        "\x1b[38;5;145m" // silver for general work
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
