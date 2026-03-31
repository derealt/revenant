//! Browser signal — active tab URL and title via native messaging
//!
//! The browser extension communicates via Chrome's native messaging protocol.
//! When the daemon isn't running or native messaging isn't configured,
//! this signal gracefully returns empty.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserTab {
    pub url: String,
    pub title: String,
    pub timestamp: u64,
}

/// Capture current browser tabs from the native messaging state file
///
/// The browser extension writes current tab info to a well-known file
/// that the daemon reads. This avoids keeping a persistent connection.
pub fn capture() -> Result<Vec<BrowserTab>> {
    let state_file = crate::expand_path("~/.revenant/browser-state.json");

    if !state_file.exists() {
        return Ok(vec![]);
    }

    // Only read if file was modified in the last 60 seconds
    let metadata = std::fs::metadata(&state_file)?;
    let age = metadata
        .modified()?
        .elapsed()
        .unwrap_or_default();

    if age.as_secs() > 60 {
        return Ok(vec![]);
    }

    let contents = std::fs::read_to_string(&state_file)?;
    let tabs: Vec<BrowserTab> = serde_json::from_str(&contents)?;

    Ok(tabs)
}

/// Install the native messaging host manifest for Chrome/Chromium
pub fn install_native_host(daemon_path: &Path) -> Result<()> {
    let manifest = serde_json::json!({
        "name": "com.revenant.ghost",
        "description": "REVENANT ghost annotation bridge",
        "path": daemon_path.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": [
            "chrome-extension://revenant-ghost-extension-id/"
        ]
    });

    let host_dir = if cfg!(target_os = "macos") {
        dirs::home_dir()
            .unwrap_or_default()
            .join("Library/Application Support/Google/Chrome/NativeMessagingHosts")
    } else {
        dirs::config_dir()
            .unwrap_or_default()
            .join("google-chrome/NativeMessagingHosts")
    };

    std::fs::create_dir_all(&host_dir)?;
    let manifest_path = host_dir.join("com.revenant.ghost.json");
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;

    tracing::info!(
        "native messaging host installed at {}",
        manifest_path.display()
    );
    Ok(())
}
