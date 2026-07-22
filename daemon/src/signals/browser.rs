//! Browser signal - active tab URL and title, reported by the extension
//!
//! The browser extension POSTs the active tab to the daemon's local HTTP
//! server (see ghost::http), which stores it in a well-known state file.
//! The snapshot aggregator reads that file at capture time. The daemon
//! only stores tab reports when `signals.browser = true` in config, so
//! browsing data never touches disk unless the user opts in.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserTab {
    pub url: String,
    pub title: String,
    pub timestamp: u64,
}

fn state_file() -> PathBuf {
    crate::expand_path("~/.revenant/browser-state.json")
}

/// Capture current browser tabs from the extension-reported state file
pub fn capture() -> Result<Vec<BrowserTab>> {
    let state_file = state_file();

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

/// Store tabs reported by the browser extension (atomic write)
pub fn store_tabs(tabs: &[BrowserTab]) -> Result<()> {
    let state_file = state_file();
    if let Some(parent) = state_file.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = serde_json::to_string(tabs)?;
    let tmp_path = state_file.with_extension("json.tmp");
    std::fs::write(&tmp_path, content)?;
    std::fs::rename(&tmp_path, &state_file)?;
    Ok(())
}
