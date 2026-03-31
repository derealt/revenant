//! Clipboard signal — captures last text content (opt-in only)
//!
//! Uses platform-native clipboard access via pbpaste (macOS)
//! or xclip/xsel (Linux). Never captures images.

use anyhow::{bail, Result};
use std::process::Command;

/// Capture current clipboard text content
pub fn capture() -> Result<String> {
    let output = if cfg!(target_os = "macos") {
        Command::new("pbpaste").output()
    } else if cfg!(target_os = "linux") {
        // Try xclip first, fall back to xsel
        Command::new("xclip")
            .args(["-selection", "clipboard", "-o"])
            .output()
            .or_else(|_| Command::new("xsel").args(["--clipboard", "--output"]).output())
    } else {
        bail!("clipboard capture not supported on this platform");
    };

    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            // Truncate to prevent capturing massive clipboard contents
            let truncated = if text.len() > 2048 {
                format!("{}... [truncated]", &text[..2048])
            } else {
                text
            };
            Ok(truncated)
        }
        Ok(out) => bail!(
            "clipboard command failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(e) => bail!("clipboard command not found: {e}"),
    }
}
