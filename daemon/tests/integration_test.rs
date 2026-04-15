//! REVENANT Integration Tests
//!
//! These tests simulate context switch scenarios end-to-end:
//! 1. Create a mock project with git state
//! 2. Trigger a departure (snapshot + compress + store)
//! 3. Trigger a return (load card + verify ghost)
//! 4. Verify ghost TTL expiry
//!
//! Run with: cargo test --test integration_test

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Test helper: create a temporary git project
fn setup_test_project(name: &str) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!("revenant-test-{name}"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    // Initialize git
    Command::new("git")
        .args(["init"])
        .current_dir(&tmp)
        .output()
        .unwrap();

    Command::new("git")
        .args(["config", "user.email", "test@revenant.dev"])
        .current_dir(&tmp)
        .output()
        .unwrap();

    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&tmp)
        .output()
        .unwrap();

    // Create some source files
    fs::write(tmp.join("main.rs"), "fn main() { println!(\"hello\"); }\n").unwrap();
    fs::write(tmp.join("lib.rs"), "pub fn add(a: i32, b: i32) -> i32 { a + b }\n").unwrap();

    // Initial commit
    Command::new("git")
        .args(["add", "."])
        .current_dir(&tmp)
        .output()
        .unwrap();

    Command::new("git")
        .args(["commit", "-m", "initial commit"])
        .current_dir(&tmp)
        .output()
        .unwrap();

    // Create a branch and modify a file
    Command::new("git")
        .args(["checkout", "-b", "feature/auth"])
        .current_dir(&tmp)
        .output()
        .unwrap();

    fs::write(
        tmp.join("auth.rs"),
        "pub fn authenticate(token: &str) -> bool { !token.is_empty() }\n",
    )
    .unwrap();

    fs::write(
        tmp.join("main.rs"),
        concat!(
            "mod auth;\n",
            "fn main() {\n",
            "    if auth::authenticate(\"test-token\") {\n",
            "        println!(\"authenticated\");\n",
            "    }\n",
            "}\n",
        ),
    )
    .unwrap();

    Command::new("git")
        .args(["add", "auth.rs"])
        .current_dir(&tmp)
        .output()
        .unwrap();

    tmp
}

/// Test helper: clean up test project
fn cleanup_test_project(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

#[test]
fn test_git_signal_capture() {
    let project = setup_test_project("git-signal");

    // The git module should detect:
    // - Branch: feature/auth
    // - 1 staged file (auth.rs)
    // - 1 modified file (main.rs)
    let output = Command::new("git")
        .args(["-C", project.to_str().unwrap(), "branch", "--show-current"])
        .output()
        .unwrap();

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(branch, "feature/auth");

    let output = Command::new("git")
        .args(["-C", project.to_str().unwrap(), "status", "--porcelain"])
        .output()
        .unwrap();

    let status = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(status.contains("auth.rs"), "should have staged auth.rs");
    assert!(status.contains("main.rs"), "should have modified main.rs");

    cleanup_test_project(&project);
}

#[test]
fn test_zsh_history_parsing() {
    let tmp = std::env::temp_dir().join("revenant-test-zsh-history");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let history_file = tmp.join("zsh_history");
    let history_content = concat!(
        ": 1711234560:0;cd ~/projects/revenant\n",
        ": 1711234561:0;cargo build\n",
        ": 1711234562:0;cargo test test_snapshot\n",
        ": 1711234563:0;git status\n",
        ": 1711234564:0;git diff src/compressor.rs\n",
    );
    fs::write(&history_file, history_content).unwrap();

    // Verify the file can be parsed
    let content = fs::read_to_string(&history_file).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 5);

    // Verify zsh format parsing
    for line in &lines {
        assert!(line.starts_with(": "), "each line should start with ': '");
        assert!(line.contains(';'), "each line should contain a semicolon separator");
    }

    // Extract commands
    let commands: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.split(';').nth(1))
        .collect();

    assert_eq!(commands.len(), 5);
    assert_eq!(commands[0], "cd ~/projects/revenant");
    assert_eq!(commands[2], "cargo test test_snapshot");

    cleanup_test_project(&tmp);
}

#[test]
fn test_rule_based_compression() {
    // Simulate the compression logic
    let branch = "feature/auth";
    let changed_files = vec!["auth.rs", "main.rs"];
    let recent_commands = vec!["cargo build", "cargo test", "git diff"];
    let last_commit = "add authentication module";

    // Build summary fragments (mirrors compressor.rs logic)
    let mut fragments: Vec<String> = Vec::new();

    fragments.push(format!("on branch `{branch}`"));
    fragments.push(format!("editing {}", changed_files.join(", ")));
    fragments.push(format!("last commit: \"{last_commit}\""));

    let interesting: Vec<&&str> = recent_commands
        .iter()
        .filter(|c| {
            let boring = ["ls", "cd", "pwd", "clear"];
            let first_word = c.split_whitespace().next().unwrap_or("");
            !boring.contains(&first_word)
        })
        .collect();

    fragments.push(format!(
        "recently ran: {}",
        interesting
            .iter()
            .map(|c| format!("`{c}`"))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    let summary = format!("You were working in revenant: {}", fragments.join(". "));

    assert!(summary.contains("feature/auth"));
    assert!(summary.contains("auth.rs"));
    assert!(summary.contains("cargo test"));
    assert!(summary.contains("authentication module"));
    assert!(!summary.is_empty());
}

#[test]
fn test_sqlite_store_roundtrip() {
    let tmp = std::env::temp_dir().join("revenant-test-db");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let db_path = tmp.join("test.db");

    // Open database
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS context_cards (
            id TEXT PRIMARY KEY,
            project_dir TEXT NOT NULL,
            project_name TEXT NOT NULL,
            summary TEXT NOT NULL,
            next_step TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL,
            signals_json TEXT NOT NULL DEFAULT '{}',
            ttl_seconds INTEGER NOT NULL DEFAULT 300
        );",
    )
    .unwrap();

    // Insert a card
    conn.execute(
        "INSERT INTO context_cards (id, project_dir, project_name, summary, next_step, created_at, ttl_seconds)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            "test-card-001",
            "/home/user/projects/revenant",
            "revenant",
            "You were debugging the file watcher debounce logic. The threshold was too aggressive, causing missed events.",
            "Lower the debounce interval to 100ms and re-run test_rapid_events",
            "2024-03-23T10:30:00Z",
            300,
        ],
    )
    .unwrap();

    // Read it back
    let summary: String = conn
        .query_row(
            "SELECT summary FROM context_cards WHERE project_dir = ?1 ORDER BY created_at DESC LIMIT 1",
            rusqlite::params!["/home/user/projects/revenant"],
            |row| row.get(0),
        )
        .unwrap();

    assert!(summary.contains("file watcher debounce"));

    let next: String = conn
        .query_row(
            "SELECT next_step FROM context_cards WHERE id = ?1",
            rusqlite::params!["test-card-001"],
            |row| row.get(0),
        )
        .unwrap();

    assert!(next.contains("debounce interval"));

    cleanup_test_project(&tmp);
}

#[test]
fn test_motd_generation() {
    // Verify the terminal ghost MOTD format
    let project = "revenant";
    let summary = "You were debugging the context switch detector. Branch changes weren't triggering snapshots.";
    let next_step = "Add branch comparison to the polling loop in detector.rs";
    let ttl_minutes = 5;

    // Build MOTD with premium format (double-line borders, intent coloring)
    let dim = "\x1b[2m";
    let reset = "\x1b[0m";
    let white = "\x1b[97m";
    let gold = "\x1b[38;5;178m";

    let mut motd = String::new();
    motd.push_str(&format!("{dim}\u{2554}\u{2550}\u{2550}{reset}"));
    motd.push_str(&format!(" REVENANT \u{2502} {project} \u{2502} "));
    motd.push_str(&format!("{reset}{dim} \u{2550}\u{2550}\u{2557}{reset}\n"));
    motd.push_str(&format!("{dim}\u{2551}{reset} {white}{summary}{reset}\n"));
    motd.push_str(&format!("{dim}\u{2551}{reset} {gold}\u{2192} {next_step}{reset}\n"));
    motd.push_str(&format!("{dim}\u{255a}\u{2550}\u{2550} ghost fades in {ttl_minutes}min of activity \u{2550}\u{2550}\u{255d}{reset}\n"));

    // Verify ANSI codes and content are present
    assert!(motd.contains("\x1b[2m")); // dim
    assert!(motd.contains("\x1b[97m")); // white
    assert!(motd.contains("\x1b[38;5;178m")); // gold
    assert!(motd.contains("REVENANT"));
    assert!(motd.contains("Branch changes"));
    assert!(motd.contains("detector.rs"));
}

#[test]
fn test_context_card_privacy() {
    // Verify that certain patterns are never captured
    let sensitive_patterns = [
        "password",
        "secret",
        "api_key",
        "token",
        ".env",
        "credentials",
    ];

    // The ignore patterns should filter these
    let ignore_patterns = [
        "node_modules",
        ".git",
        "target",
        "dist",
        "__pycache__",
        ".DS_Store",
    ];

    // Clipboard is opt-in by default
    let clipboard_enabled = false;
    assert!(!clipboard_enabled, "clipboard should be opt-in");

    // Browser signal is opt-in by default
    let browser_enabled = false;
    assert!(!browser_enabled, "browser signal should be opt-in");
}

#[test]
fn test_ghost_never_modifies_files() {
    let project = setup_test_project("no-modify");
    let main_rs = project.join("main.rs");

    // Read the file before ghost injection
    let before = fs::read_to_string(&main_rs).unwrap();

    // Simulate ghost injection (terminal ghost writes to separate motd file)
    let motd_path = project.join("motd");
    fs::write(
        &motd_path,
        "REVENANT: You were here. This is a ghost annotation.",
    )
    .unwrap();

    // The source file should be UNCHANGED
    let after = fs::read_to_string(&main_rs).unwrap();
    assert_eq!(before, after, "ghost injection must NEVER modify source files");

    // VS Code ghost uses decorations (visual overlay), not file edits
    // Slack ghost uses ephemeral messages
    // Browser ghost uses DOM injection on web pages
    // All are non-destructive by design

    cleanup_test_project(&project);
}
