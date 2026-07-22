//! Snapshot aggregator - collects all signals into a unified WorkingState
//!
//! The WorkingState represents everything we know about what the user
//! was doing at a point in time. It's the raw material the compressor
//! turns into a human-readable context card.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::RevenantConfig;
use crate::signals::{browser, clipboard, editor, git, terminal};

/// A single observation about the user's state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Signal {
    GitState(git::GitState),
    EditorState(editor::EditorState),
    TerminalState(terminal::TerminalState),
    ClipboardContent(String),
    BrowserTab(browser::BrowserTab),
    FileEvent {
        path: String,
        kind: FileEventKind,
        timestamp: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileEventKind {
    Modified,
    Created,
    Deleted,
    Renamed { from: String },
}

/// Aggregated snapshot of all signals at a point in time
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingState {
    pub project_dir: String,
    pub project_name: String,
    pub timestamp: DateTime<Utc>,
    pub git: Option<git::GitState>,
    pub editor: Option<editor::EditorState>,
    pub terminal: Option<terminal::TerminalState>,
    pub clipboard: Option<String>,
    pub browser_tabs: Vec<browser::BrowserTab>,
    pub recent_file_events: Vec<Signal>,
}

impl WorkingState {
    /// Human-readable summary of the most-touched files
    pub fn active_files(&self) -> Vec<&str> {
        let mut files = Vec::new();
        if let Some(ref ed) = self.editor {
            for f in &ed.open_files {
                files.push(f.path.as_str());
            }
        }
        files
    }

    /// The branch currently checked out, if any
    pub fn branch(&self) -> Option<&str> {
        self.git.as_ref().map(|g| g.branch.as_str())
    }

    /// Recent terminal commands as strings
    pub fn recent_commands(&self) -> Vec<&str> {
        self.terminal
            .as_ref()
            .map(|t| t.recent_commands.iter().map(|c| c.command.as_str()).collect())
            .unwrap_or_default()
    }
}

pub struct SnapshotAggregator {
    config: RevenantConfig,
}

impl SnapshotAggregator {
    pub fn new(config: &RevenantConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }

    /// Capture a complete working state for a project directory
    pub async fn capture(&self, project_dir: &str) -> Result<WorkingState> {
        let project_name = std::path::Path::new(project_dir)
            .file_name()
            .map(|n| {
                let raw = n.to_string_lossy();
                // Capitalize: "limn" → "Limn", "AIMO3" stays "AIMO3"
                let mut chars = raw.chars();
                match chars.next() {
                    Some(c) => {
                        let first = c.to_uppercase().to_string();
                        format!("{}{}", first, chars.as_str())
                    }
                    None => raw.to_string(),
                }
            })
            .unwrap_or_else(|| "unknown".into());

        // Capture all enabled signals concurrently
        let git_state = if self.config.signals.git {
            git::capture(project_dir).ok()
        } else {
            None
        };

        let editor_state = if self.config.signals.editor_enabled {
            editor::capture(project_dir, &self.config.signals.editor_config).ok()
        } else {
            None
        };

        let terminal_state = if self.config.signals.terminal_enabled {
            terminal::capture(&self.config.signals.terminal_config).ok()
        } else {
            None
        };

        let clipboard_content = if self.config.signals.clipboard {
            clipboard::capture().ok()
        } else {
            None
        };

        let browser_tabs = if self.config.signals.browser {
            browser::capture().unwrap_or_default()
        } else {
            vec![]
        };

        Ok(WorkingState {
            project_dir: project_dir.to_string(),
            project_name,
            timestamp: Utc::now(),
            git: git_state,
            editor: editor_state,
            terminal: terminal_state,
            clipboard: clipboard_content,
            browser_tabs,
            recent_file_events: vec![], // populated by watcher feed
        })
    }
}
