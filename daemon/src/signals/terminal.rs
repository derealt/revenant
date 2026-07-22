//! Terminal signal - recent commands from shell history, working directory
//!
//! Parses zsh_history and bash_history to extract recent commands.
//! Zsh history format: `: timestamp:duration;command`
//! Bash history format: plain `command` per line

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use crate::config::TerminalSignalConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalState {
    /// Recent commands, newest first
    pub recent_commands: Vec<HistoryEntry>,
    /// Current working directory (from the most recent command's context)
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub command: String,
    pub timestamp: Option<u64>,
}

/// Capture terminal state from shell history
pub fn capture(config: &TerminalSignalConfig) -> Result<TerminalState> {
    let count = config.recent_command_count;
    let mut all_entries = Vec::new();

    for hist_path_str in &config.history_files {
        let hist_path = crate::expand_path(hist_path_str);
        if hist_path.exists() {
            if let Ok(entries) = parse_history_tail(&hist_path, count) {
                all_entries.extend(entries);
            }
        }
    }

    // Sort by timestamp if available, otherwise keep file order
    all_entries.sort_by(|a, b| {
        let ta = a.timestamp.unwrap_or(0);
        let tb = b.timestamp.unwrap_or(0);
        tb.cmp(&ta) // newest first
    });

    // Deduplicate consecutive identical commands
    all_entries.dedup_by(|a, b| a.command == b.command);

    // Take only the requested count
    all_entries.truncate(count);

    // Infer cwd from recent cd commands
    let cwd = infer_cwd(&all_entries);

    Ok(TerminalState {
        recent_commands: all_entries,
        cwd,
    })
}

/// Read the last N lines from a history file efficiently
/// (seeks near end of file to avoid reading entire history)
fn parse_history_tail(path: &Path, count: usize) -> Result<Vec<HistoryEntry>> {
    let file = std::fs::File::open(path)?;
    let file_len = file.metadata()?.len();

    // Read the last ~64KB which should contain enough history
    let read_from = if file_len > 65536 {
        file_len - 65536
    } else {
        0
    };

    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(read_from))?;

    // If we seeked into the middle, skip the first partial line
    if read_from > 0 {
        let mut discard = String::new();
        reader.read_line(&mut discard)?;
    }

    let is_zsh = path
        .to_string_lossy()
        .contains("zsh_history");

    let mut entries = Vec::new();
    let mut current_line = String::new();

    loop {
        current_line.clear();
        let bytes_read = reader.read_line(&mut current_line)?;
        if bytes_read == 0 {
            break;
        }

        let line = current_line.trim();
        if line.is_empty() {
            continue;
        }

        if is_zsh {
            // Zsh extended history: `: timestamp:duration;command`
            if let Some(entry) = parse_zsh_history_line(line) {
                entries.push(entry);
            }
        } else {
            // Bash: plain command per line
            entries.push(HistoryEntry {
                command: line.to_string(),
                timestamp: None,
            });
        }
    }

    // Take only the last `count` entries
    if entries.len() > count {
        entries = entries.split_off(entries.len() - count);
    }

    // Reverse so newest is first
    entries.reverse();

    Ok(entries)
}

/// Parse a zsh extended history line
/// Format: `: 1711234567:0;command here`
fn parse_zsh_history_line(line: &str) -> Option<HistoryEntry> {
    if !line.starts_with(": ") {
        // Plain command (non-extended format)
        return Some(HistoryEntry {
            command: line.to_string(),
            timestamp: None,
        });
    }

    // `: timestamp:duration;command`
    let rest = &line[2..]; // skip ": "
    let semicolon = rest.find(';')?;
    let meta = &rest[..semicolon];
    let command = rest[semicolon + 1..].to_string();

    if command.is_empty() {
        return None;
    }

    let timestamp = meta
        .split(':')
        .next()
        .and_then(|s| s.trim().parse::<u64>().ok());

    Some(HistoryEntry { command, timestamp })
}

/// Try to infer the current working directory from recent cd/pushd commands
fn infer_cwd(entries: &[HistoryEntry]) -> Option<String> {
    for entry in entries {
        let cmd = entry.command.trim();

        // Match `cd <path>` or `pushd <path>`
        let dir = if let Some(rest) = cmd.strip_prefix("cd ") {
            Some(rest.trim().to_string())
        } else if let Some(rest) = cmd.strip_prefix("pushd ") {
            Some(rest.trim().to_string())
        } else if let Some(rest) = cmd.strip_prefix("z ") {
            // zoxide
            Some(rest.trim().to_string())
        } else {
            None
        };

        if let Some(d) = dir {
            // Strip quotes
            let d = d.trim_matches(|c| c == '\'' || c == '"');
            if !d.is_empty() && !d.starts_with('-') {
                return Some(d.to_string());
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_zsh_line() {
        let line = ": 1711234567:0;git status";
        let entry = parse_zsh_history_line(line).unwrap();
        assert_eq!(entry.command, "git status");
        assert_eq!(entry.timestamp, Some(1711234567));
    }

    #[test]
    fn test_parse_zsh_multiword() {
        let line = ": 1711234567:0;cargo test --release -- test_name";
        let entry = parse_zsh_history_line(line).unwrap();
        assert_eq!(entry.command, "cargo test --release -- test_name");
    }

    #[test]
    fn test_infer_cwd() {
        let entries = vec![
            HistoryEntry {
                command: "git status".into(),
                timestamp: None,
            },
            HistoryEntry {
                command: "cd ~/projects/revenant".into(),
                timestamp: None,
            },
            HistoryEntry {
                command: "ls".into(),
                timestamp: None,
            },
        ];
        assert_eq!(
            infer_cwd(&entries),
            Some("~/projects/revenant".to_string())
        );
    }
}
