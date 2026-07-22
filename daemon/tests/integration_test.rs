//! REVENANT Integration Tests
//!
//! These tests drive the REAL daemon code end-to-end through the library
//! target: fixture git repo -> signals::git::capture -> rule_based_compress
//! -> ContextStore roundtrip -> TerminalGhost motd render, plus the
//! SwitchDetector state machine. No logic is re-implemented in the tests;
//! if the daemon breaks, these break.
//!
//! Run with: cargo test --test integration_test

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use revenant::compressor::rule_based_compress;
use revenant::config::{RevenantConfig, TerminalGhostConfig, TerminalSignalConfig};
use revenant::detector::{SwitchDetector, SwitchKind};
use revenant::ghost::terminal::TerminalGhost;
use revenant::signals::{git, terminal};
use revenant::snapshot::{SnapshotAggregator, WorkingState};
use revenant::store::ContextStore;

/// Test helper: create a temporary git project on branch feature/auth
/// with one commit, one staged file, and one modified file.
fn setup_test_project(name: &str) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!("revenant-test-{name}"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let run = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(&tmp)
            .output()
            .unwrap()
    };

    run(&["init"]);
    run(&["config", "user.email", "test@revenant.dev"]);
    run(&["config", "user.name", "Test"]);

    fs::write(tmp.join("main.rs"), "fn main() { println!(\"hello\"); }\n").unwrap();
    fs::write(tmp.join("lib.rs"), "pub fn add(a: i32, b: i32) -> i32 { a + b }\n").unwrap();

    run(&["add", "."]);
    run(&["commit", "-m", "fix: resolve reconnect race in auth flow"]);
    run(&["checkout", "-b", "feature/auth"]);

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

    run(&["add", "auth.rs"]);

    tmp
}

fn cleanup_test_project(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

/// Capture a WorkingState for a fixture project using the real git signal,
/// with terminal/editor/clipboard/browser signals absent.
fn working_state_for(project: &Path) -> WorkingState {
    let dir = project.to_str().unwrap().to_string();
    let name = project.file_name().unwrap().to_string_lossy().to_string();
    WorkingState {
        project_dir: dir.clone(),
        project_name: name,
        timestamp: chrono::Utc::now(),
        git: Some(git::capture(&dir).expect("git capture should succeed on fixture")),
        editor: None,
        terminal: None,
        clipboard: None,
        browser_tabs: vec![],
        recent_file_events: vec![],
    }
}

#[test]
fn git_signal_captures_branch_status_and_commits() {
    let project = setup_test_project("git-signal");
    let state = git::capture(project.to_str().unwrap()).unwrap();

    assert_eq!(state.branch, "feature/auth");
    assert_eq!(state.staged, 1, "auth.rs should be staged");
    assert_eq!(state.modified, 1, "main.rs should be modified");
    assert!(
        state.changed_files.iter().any(|f| f.path == "auth.rs"),
        "changed files should include auth.rs, got {:?}",
        state.changed_files
    );
    assert_eq!(state.recent_commits.len(), 1);
    assert_eq!(
        state.recent_commits[0].message,
        "fix: resolve reconnect race in auth flow"
    );
    assert!(!state.diff_stat.is_empty(), "diff stat should be populated");

    cleanup_test_project(&project);
}

#[test]
fn git_capture_fails_outside_a_repo() {
    let tmp = std::env::temp_dir().join("revenant-test-not-a-repo");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    assert!(git::capture(tmp.to_str().unwrap()).is_err());

    cleanup_test_project(&tmp);
}

#[test]
fn terminal_signal_parses_zsh_history_newest_first() {
    let tmp = std::env::temp_dir().join("revenant-test-zsh-history");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    // File name must contain "zsh_history" for the parser to use zsh format
    let history_file = tmp.join("zsh_history");
    fs::write(
        &history_file,
        concat!(
            ": 1711234560:0;cd ~/projects/revenant\n",
            ": 1711234561:0;cargo build\n",
            ": 1711234562:0;cargo test test_snapshot\n",
            ": 1711234563:0;git status\n",
            ": 1711234564:0;git diff src/compressor.rs\n",
        ),
    )
    .unwrap();

    let config = TerminalSignalConfig {
        history_files: vec![history_file.to_str().unwrap().to_string()],
        recent_command_count: 3,
    };
    let state = terminal::capture(&config).unwrap();

    assert_eq!(state.recent_commands.len(), 3, "should honor recent_command_count");
    assert_eq!(
        state.recent_commands[0].command, "git diff src/compressor.rs",
        "newest command must come first"
    );
    assert_eq!(state.recent_commands[0].timestamp, Some(1711234564));

    cleanup_test_project(&tmp);
}

#[test]
fn terminal_signal_dedupes_consecutive_commands() {
    let tmp = std::env::temp_dir().join("revenant-test-zsh-dedup");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let history_file = tmp.join("zsh_history");
    fs::write(
        &history_file,
        concat!(
            ": 1711234560:0;cargo test\n",
            ": 1711234561:0;cargo test\n",
            ": 1711234562:0;cargo test\n",
            ": 1711234563:0;git status\n",
        ),
    )
    .unwrap();

    let config = TerminalSignalConfig {
        history_files: vec![history_file.to_str().unwrap().to_string()],
        recent_command_count: 10,
    };
    let state = terminal::capture(&config).unwrap();

    let test_runs = state
        .recent_commands
        .iter()
        .filter(|c| c.command == "cargo test")
        .count();
    assert_eq!(test_runs, 1, "consecutive identical commands should collapse to one");

    cleanup_test_project(&tmp);
}

#[test]
fn rule_based_compression_produces_specific_second_person_card() {
    let project = setup_test_project("compress");
    let state = working_state_for(&project);
    let card = rule_based_compress(&state);

    assert!(!card.id.is_empty());
    assert_eq!(card.project_dir, state.project_dir);
    assert!(!card.summary.is_empty());
    let s = card.summary.to_lowercase();
    assert!(
        s.contains("you"),
        "cards are written to future-you, got: {}",
        card.summary
    );
    // The fixture's signals are a fix-flavored commit on a feature branch
    // touching auth files. A specific card must surface at least one of
    // those concrete facts; a generic card surfaces none.
    assert!(
        s.contains("auth") || s.contains("reconnect") || s.contains("fix"),
        "summary must reference the actual work, got: {}",
        card.summary
    );
    assert!(!card.next_step.is_empty(), "card must propose a next step");
    assert!(card.ttl_seconds > 0);

    cleanup_test_project(&project);
}

#[test]
fn store_roundtrips_real_cards() {
    let tmp = std::env::temp_dir().join("revenant-test-db");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let project = setup_test_project("store");
    let state = working_state_for(&project);
    let card = rule_based_compress(&state);

    let store = ContextStore::open(&tmp.join("test.db")).unwrap();
    store.migrate().unwrap();
    store.save_card(&card).unwrap();

    let loaded = store
        .latest_card(&card.project_dir)
        .unwrap()
        .expect("saved card must be retrievable");
    assert_eq!(loaded.id, card.id);
    assert_eq!(loaded.summary, card.summary);
    assert_eq!(loaded.next_step, card.next_step);
    assert_eq!(loaded.ttl_seconds, card.ttl_seconds);

    // Unknown project: no card
    assert!(store.latest_card("/nonexistent/project").unwrap().is_none());

    // Recent-cards query sees it too
    let recent = store
        .cards_since(chrono::Utc::now() - chrono::Duration::hours(1), 10)
        .unwrap();
    assert!(recent.iter().any(|c| c.id == card.id));

    cleanup_test_project(&project);
    cleanup_test_project(&tmp);
}

#[test]
fn store_skips_near_duplicate_cards() {
    let tmp = std::env::temp_dir().join("revenant-test-db-dedup");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let project = setup_test_project("store-dedup");
    let state = working_state_for(&project);
    let card = rule_based_compress(&state);

    let store = ContextStore::open(&tmp.join("test.db")).unwrap();
    store.migrate().unwrap();
    store.save_card(&card).unwrap();

    // Saving an identical card again must not grow the store
    let mut dup = card.clone();
    dup.id = "different-id".to_string();
    store.save_card(&dup).unwrap();

    assert_eq!(
        store.total_cards().unwrap(),
        1,
        "near-duplicate card should be skipped"
    );

    cleanup_test_project(&project);
    cleanup_test_project(&tmp);
}

#[tokio::test]
async fn full_pipeline_snapshot_compress_store_restore() {
    let tmp = std::env::temp_dir().join("revenant-test-pipeline");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let project = setup_test_project("pipeline");

    // Departure: real aggregator (git signal only, so the test is hermetic:
    // no dependence on this machine's shell history or editor state)
    let mut config = RevenantConfig::default();
    config.signals.terminal_enabled = false;
    config.signals.editor_enabled = false;

    let aggregator = SnapshotAggregator::new(&config);
    let state = aggregator.capture(project.to_str().unwrap()).await.unwrap();
    assert_eq!(state.branch(), Some("feature/auth"));

    let card = rule_based_compress(&state);

    let store = ContextStore::open(&tmp.join("pipeline.db")).unwrap();
    store.migrate().unwrap();
    store.save_card(&card).unwrap();

    // Return: load the card and render the real terminal ghost
    let restored = store
        .latest_card(project.to_str().unwrap())
        .unwrap()
        .expect("card must be restorable on return");

    let ghost_config = TerminalGhostConfig {
        motd_file: tmp.join("motd").to_str().unwrap().to_string(),
    };
    let ghost = TerminalGhost::new(&ghost_config).unwrap();
    ghost.inject(&restored).await.unwrap();

    let motd = fs::read_to_string(tmp.join("motd")).unwrap();
    assert!(motd.contains("REVENANT"));
    assert!(motd.contains(&restored.project_name));
    assert!(motd.contains(&restored.summary));
    assert!(motd.contains("ghost fades"));

    // Machine-readable metadata for the shell scripts
    let meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(tmp.join("motd.json")).unwrap()).unwrap();
    assert_eq!(meta["card_id"], serde_json::json!(restored.id));

    // Clear removes both files
    ghost.clear().await.unwrap();
    assert!(!tmp.join("motd").exists());
    assert!(!tmp.join("motd.json").exists());

    cleanup_test_project(&project);
    cleanup_test_project(&tmp);
}

#[test]
fn detector_emits_return_on_project_switch() {
    let detector = SwitchDetector::new(&RevenantConfig::default());

    // First activity in a project: no switch
    assert!(detector.record_activity("/tmp/project-a").is_none());
    // Continued activity in the same project: no switch
    assert!(detector.record_activity("/tmp/project-a").is_none());

    // Hopping to another project: Return carrying the origin
    let event = detector
        .record_activity("/tmp/project-b")
        .expect("project hop must emit a switch event");
    match event.kind {
        SwitchKind::Return {
            project_dir,
            from_project,
        } => {
            assert_eq!(project_dir, "/tmp/project-b");
            assert_eq!(from_project.as_deref(), Some("/tmp/project-a"));
        }
        other => panic!("expected Return, got {other:?}"),
    }
}

#[test]
fn detector_emits_return_on_branch_change() {
    let detector = SwitchDetector::new(&RevenantConfig::default());
    detector.record_activity("/tmp/project-a");

    // First observation just records the branch
    assert!(detector.check_branch_change("/tmp/project-a", "main").is_none());
    // Same branch: nothing
    assert!(detector.check_branch_change("/tmp/project-a", "main").is_none());
    // Branch switch: Return event
    let event = detector
        .check_branch_change("/tmp/project-a", "feature/auth")
        .expect("branch change must emit a switch event");
    assert!(matches!(event.kind, SwitchKind::Return { .. }));
}

#[test]
fn privacy_defaults_are_opt_in() {
    // The real defaults, not local booleans: clipboard and browser signal
    // capture must ship disabled, and noisy dirs must be ignored.
    let config = RevenantConfig::default();
    assert!(!config.signals.clipboard, "clipboard capture must be opt-in");
    assert!(!config.signals.browser, "browser capture must be opt-in");
    assert!(config.signals.ignore_patterns.iter().any(|p| p == ".git"));
    assert!(config.signals.ignore_patterns.iter().any(|p| p == "node_modules"));
}

#[tokio::test]
async fn http_server_serves_inject_and_clear() {
    use revenant::ghost::http::GhostHttpServer;
    use std::sync::Arc;

    let port = 17711u16;
    let server = Arc::new(GhostHttpServer::new(false));
    let serve = Arc::clone(&server);
    tokio::spawn(async move { serve.serve(port).await });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let post = |path: &'static str, body: &'static str| async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut resp = String::new();
        s.read_to_string(&mut resp).await.unwrap();
        resp
    };
    let get = || async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(b"GET /ghost HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut resp = String::new();
        s.read_to_string(&mut resp).await.unwrap();
        resp
    };

    // Starts clear
    assert!(get().await.contains(r#"{"type":"clear"}"#));

    // Valid inject is served to subsequent polls
    let card = r#"{"type":"inject","card":{"id":"t1","summary":"You were testing the http bridge.","next_step":"","project_dir":"/tmp/x","project_name":"x","ttl_seconds":120}}"#;
    let resp = post("/inject", card).await;
    assert!(resp.starts_with("HTTP/1.1 204"), "got: {resp}");
    let served = get().await;
    assert!(served.contains("You were testing the http bridge."));

    // Malformed inject is rejected and does not clobber the card
    let resp = post("/inject", "not json").await;
    assert!(resp.starts_with("HTTP/1.1 400"), "got: {resp}");
    assert!(get().await.contains("You were testing the http bridge."));

    // Clear resets
    let resp = post("/clear", "").await;
    assert!(resp.starts_with("HTTP/1.1 204"), "got: {resp}");
    assert!(get().await.contains(r#"{"type":"clear"}"#));

    // Tab reports are accepted quietly when reporting is disabled
    let resp = post("/tab", r#"[{"url":"https://example.com","title":"t","timestamp":1}]"#).await;
    assert!(resp.starts_with("HTTP/1.1 204"), "got: {resp}");

    // Unknown paths 404
    let resp = post("/nope", "").await;
    assert!(resp.starts_with("HTTP/1.1 404"), "got: {resp}");
}

#[tokio::test]
async fn ghost_never_modifies_project_files() {
    let project = setup_test_project("no-modify");
    let main_rs = project.join("main.rs");
    let before = fs::read_to_string(&main_rs).unwrap();

    // Run the REAL terminal ghost against a card for this project.
    // The motd lives outside the project tree; the project must be untouched.
    let state = working_state_for(&project);
    let card = rule_based_compress(&state);

    let ghost_dir = std::env::temp_dir().join("revenant-test-no-modify-ghost");
    let _ = fs::remove_dir_all(&ghost_dir);
    fs::create_dir_all(&ghost_dir).unwrap();
    let ghost = TerminalGhost::new(&TerminalGhostConfig {
        motd_file: ghost_dir.join("motd").to_str().unwrap().to_string(),
    })
    .unwrap();
    ghost.inject(&card).await.unwrap();

    let after = fs::read_to_string(&main_rs).unwrap();
    assert_eq!(before, after, "ghost injection must NEVER modify source files");

    cleanup_test_project(&project);
    cleanup_test_project(&ghost_dir);
}
