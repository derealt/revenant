//! Editor state signal - open files, cursor positions, recent edits
//!
//! Strategy: parse VS Code's workspace state files and recently-modified
//! files to infer what the user was working on. No LSP dependency.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::config::EditorSignalConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorState {
    /// Files currently open or recently modified
    pub open_files: Vec<OpenFile>,
    /// The file most likely being actively edited (most recently modified)
    pub active_file: Option<String>,
    /// Language/filetype of the active file
    pub active_language: Option<String>,
    /// The resume point: file + line the cursor was on, as reported by
    /// the editor extension. This is what "resume where you left off"
    /// jumps back to.
    #[serde(default)]
    pub cursor: Option<CursorAnchor>,
}

/// Where the cursor was at departure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorAnchor {
    /// Absolute path
    pub file: String,
    /// 1-based line number
    pub line: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenFile {
    /// Path relative to project root
    pub path: String,
    /// Absolute path
    pub absolute_path: String,
    /// Last modification time (seconds since epoch)
    pub last_modified: u64,
    /// Whether this file was modified within the last 5 minutes
    pub recently_edited: bool,
    /// Detected language from extension
    pub language: Option<String>,
}

/// Capture editor state for a project
pub fn capture(project_dir: &str, config: &EditorSignalConfig) -> Result<EditorState> {
    let project_path = Path::new(project_dir);
    let threshold = SystemTime::now() - Duration::from_secs(300); // 5 min

    // Find recently modified source files in the project
    let mut open_files = find_recent_files(project_path, threshold)?;

    // Sort by modification time, most recent first
    open_files.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));

    // Cap at 20 files
    open_files.truncate(20);

    // Try to read VS Code workspace state if available
    let mut cursor = None;
    let vscode_state_dir = crate::expand_path(&config.vscode_state_dir);
    if vscode_state_dir.exists() {
        if let Ok(vscode_state) = read_vscode_state(&vscode_state_dir, project_dir) {
            // Merge VS Code state: files from VS Code that we didn't already find
            for vf in vscode_state.open_files {
                if !open_files.iter().any(|f| f.absolute_path == vf.absolute_path) {
                    open_files.push(vf);
                }
            }
            cursor = vscode_state.cursor;
        }
    }

    // Prefer the editor's actual active file over the mtime guess
    let active_file = cursor
        .as_ref()
        .map(|c| {
            Path::new(&c.file)
                .strip_prefix(project_dir)
                .unwrap_or(Path::new(&c.file))
                .to_string_lossy()
                .to_string()
        })
        .or_else(|| open_files.first().map(|f| f.path.clone()));
    let active_language = cursor
        .as_ref()
        .and_then(|c| detect_language(Path::new(&c.file)))
        .or_else(|| open_files.first().and_then(|f| f.language.clone()));

    Ok(EditorState {
        open_files,
        active_file,
        active_language,
        cursor,
    })
}

/// Walk the project dir and find files modified within the threshold
fn find_recent_files(project_dir: &Path, threshold: SystemTime) -> Result<Vec<OpenFile>> {
    let mut files = Vec::new();

    // Use the `ignore` crate's walker which respects .gitignore
    let walker = ignore::WalkBuilder::new(project_dir)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .max_depth(Some(6))
        .build();

    for entry in walker.flatten() {
        if !entry.file_type().map_or(false, |ft| ft.is_file()) {
            continue;
        }

        let path = entry.path();

        // Skip non-source files and tool-written artifacts (lockfiles, maps)
        if !is_source_file(path)
            || super::git::is_generated_artifact(&path.to_string_lossy())
        {
            continue;
        }

        if let Ok(metadata) = path.metadata() {
            if let Ok(modified) = metadata.modified() {
                if modified > threshold {
                    let relative = path
                        .strip_prefix(project_dir)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .to_string();
                    let abs = path.to_string_lossy().to_string();
                    let epoch = modified
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();

                    files.push(OpenFile {
                        path: relative,
                        absolute_path: abs,
                        last_modified: epoch,
                        recently_edited: true,
                        language: detect_language(path),
                    });
                }
            }
        }
    }

    Ok(files)
}

/// What the editor extension's state file yields
struct VscodeState {
    open_files: Vec<OpenFile>,
    cursor: Option<CursorAnchor>,
}

/// Read the editor extension's persisted workspace state
fn read_vscode_state(state_dir: &Path, project_dir: &str) -> Result<VscodeState> {
    // The extension writes a JSON file per project:
    // ~/.revenant/vscode-state/<hash>.json containing
    // { "openFiles": [...], "activeFile": "path", "activeLine": 47, "timestamp": ms }
    let hash = simple_hash(project_dir);
    let state_file = state_dir.join(format!("{hash}.json"));

    if !state_file.exists() {
        return Ok(VscodeState {
            open_files: vec![],
            cursor: None,
        });
    }

    let contents = std::fs::read_to_string(&state_file)?;
    let state: serde_json::Value = serde_json::from_str(&contents)?;

    let mut files = Vec::new();
    if let Some(open) = state.get("openFiles").and_then(|v| v.as_array()) {
        for path_val in open {
            if let Some(path_str) = path_val.as_str() {
                let p = Path::new(path_str);
                let relative = p
                    .strip_prefix(project_dir)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .to_string();
                files.push(OpenFile {
                    path: relative,
                    absolute_path: path_str.to_string(),
                    last_modified: 0,
                    recently_edited: false,
                    language: detect_language(p),
                });
            }
        }
    }

    // The resume point: active file + 1-based cursor line
    let cursor = match (
        state.get("activeFile").and_then(|v| v.as_str()),
        state.get("activeLine").and_then(|v| v.as_u64()),
    ) {
        (Some(file), Some(line)) if line > 0 => Some(CursorAnchor {
            file: file.to_string(),
            line: line as u32,
        }),
        _ => None,
    };

    Ok(VscodeState {
        open_files: files,
        cursor,
    })
}

fn simple_hash(s: &str) -> String {
    // FNV-1a-like hash for stable, short directory identifiers
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_state(dir: &Path, project: &str, json: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("{}.json", simple_hash(project))), json).unwrap();
    }

    #[test]
    fn vscode_state_yields_cursor_anchor() {
        let dir = std::env::temp_dir().join("revenant-test-vscode-state");
        let _ = std::fs::remove_dir_all(&dir);
        let project = "/tmp/anchor-project";
        write_state(
            &dir,
            project,
            r#"{"projectDir":"/tmp/anchor-project",
                "openFiles":["/tmp/anchor-project/src/a.rs"],
                "activeFile":"/tmp/anchor-project/src/a.rs",
                "activeLine":47,
                "timestamp":1711234567890}"#,
        );

        let state = read_vscode_state(&dir, project).unwrap();
        let cursor = state.cursor.expect("cursor anchor must be parsed");
        assert_eq!(cursor.file, "/tmp/anchor-project/src/a.rs");
        assert_eq!(cursor.line, 47);
        assert_eq!(state.open_files.len(), 1);
        assert_eq!(state.open_files[0].path, "src/a.rs");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vscode_state_without_line_has_no_anchor() {
        let dir = std::env::temp_dir().join("revenant-test-vscode-state-noline");
        let _ = std::fs::remove_dir_all(&dir);
        let project = "/tmp/no-line-project";
        // Old extension versions wrote no activeLine: must not invent one
        write_state(
            &dir,
            project,
            r#"{"projectDir":"/tmp/no-line-project",
                "openFiles":[],
                "activeFile":"/tmp/no-line-project/main.go",
                "timestamp":1711234567890}"#,
        );

        let state = read_vscode_state(&dir, project).unwrap();
        assert!(state.cursor.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn detect_language(path: &Path) -> Option<String> {
    path.extension().and_then(|ext| {
        let ext = ext.to_string_lossy().to_lowercase();
        match ext.as_str() {
            "rs" => Some("Rust"),
            "ts" | "tsx" => Some("TypeScript"),
            "js" | "jsx" => Some("JavaScript"),
            "py" => Some("Python"),
            "go" => Some("Go"),
            "java" => Some("Java"),
            "kt" | "kts" => Some("Kotlin"),
            "c" | "h" => Some("C"),
            "cpp" | "cc" | "cxx" | "hpp" => Some("C++"),
            "rb" => Some("Ruby"),
            "swift" => Some("Swift"),
            "zig" => Some("Zig"),
            "sql" => Some("SQL"),
            "html" | "htm" => Some("HTML"),
            "css" | "scss" | "less" => Some("CSS"),
            "json" => Some("JSON"),
            "toml" => Some("TOML"),
            "yaml" | "yml" => Some("YAML"),
            "md" => Some("Markdown"),
            "sh" | "bash" | "zsh" | "fish" => Some("Shell"),
            "lua" => Some("Lua"),
            "ex" | "exs" => Some("Elixir"),
            "erl" | "hrl" => Some("Erlang"),
            "vue" => Some("Vue"),
            "svelte" => Some("Svelte"),
            _ => None,
        }
        .map(String::from)
    })
}

fn is_source_file(path: &Path) -> bool {
    detect_language(path).is_some()
}
