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
use revenant::detector::{ReturnCause, SwitchDetector, SwitchKind};
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

/// Field regression (2026-09-28): a repo that COMMITS a compiler output
/// (web/tsconfig.tsbuildinfo) shows it as modified after every build, and the
/// card said "you were configuring tsconfig.tsbuildinfo" for eight hours.
/// Generated files are not the user's work: they never become a topic and
/// never count as in-flight edits.
#[test]
fn generated_artifacts_never_become_the_card() {
    let tmp = std::env::temp_dir().join("revenant-test-artifacts");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("web")).unwrap();
    let run = |args: &[&str]| {
        Command::new("git").args(args).current_dir(&tmp).output().unwrap()
    };
    run(&["init"]);
    run(&["config", "user.email", "test@revenant.dev"]);
    run(&["config", "user.name", "Test"]);
    fs::write(tmp.join("web/tsconfig.tsbuildinfo"), "{\"v\":1}").unwrap();
    fs::write(tmp.join("web/package-lock.json"), "{}").unwrap();
    fs::write(tmp.join("web/app.ts"), "export const a = 1;\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-m", "initial"]);

    // A build rewrites the artifacts; the user touched nothing
    fs::write(tmp.join("web/tsconfig.tsbuildinfo"), "{\"v\":2}").unwrap();
    fs::write(tmp.join("web/package-lock.json"), "{\"x\":1}").unwrap();
    fs::write(tmp.join("web/app.js.map"), "{}").unwrap();

    let state = working_state_for(&tmp);
    let g = state.git.as_ref().unwrap();
    assert!(
        g.changed_files.is_empty(),
        "generated files are not the user's work, got {:?}",
        g.changed_files
    );
    assert_eq!((g.staged, g.modified, g.untracked), (0, 0, 0));
    let card = rule_based_compress(&state);
    let text = format!("{} {}", card.summary, card.next_step).to_lowercase();
    for noise in ["tsbuildinfo", "package-lock", ".map", "unstaged"] {
        assert!(!text.contains(noise), "card spoke an artifact ({noise}): {text}");
    }

    // A real edit next to the artifacts is still the card's subject
    fs::write(tmp.join("web/app.ts"), "export const a = 2;\n").unwrap();
    let state = working_state_for(&tmp);
    let g = state.git.as_ref().unwrap();
    let paths: Vec<&str> = g.changed_files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["web/app.ts"]);
    // An unstaged-only edit on the FIRST status line stays unstaged (the
    // porcelain column once got trimmed away and it read as staged)
    assert_eq!((g.staged, g.modified), (0, 1));

    cleanup_test_project(&tmp);
}

/// Field regression (2026-09-28): the most common flow is "branch off main,
/// start editing, get pulled away". The branch has no commits of its own
/// yet, so the latest commit is main's past work, and the card said "you
/// were building exponential backoff" while the user was fixing the
/// reconnect timer. A commit only speaks for the work if it was made on
/// this branch; until then the branch name does.
#[test]
fn fresh_branch_speaks_for_itself_not_mains_last_commit() {
    let tmp = std::env::temp_dir().join("revenant-test-fresh-branch");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("src")).unwrap();
    let run = |args: &[&str]| {
        Command::new("git").args(args).current_dir(&tmp).output().unwrap()
    };
    run(&["init", "-b", "main"]);
    run(&["config", "user.email", "test@revenant.dev"]);
    run(&["config", "user.name", "Test"]);
    fs::write(tmp.join("src/reconnect.rs"), "pub fn on_connect() {}\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-m", "Add exponential backoff to reconnect"]);
    run(&["checkout", "-b", "fix/reconnect-timer"]);
    fs::write(tmp.join("src/reconnect.rs"), "pub fn on_connect() { /* clear timer */ }\n").unwrap();

    let card = rule_based_compress(&working_state_for(&tmp));
    let text = card.summary.to_lowercase();
    assert!(!text.contains("backoff"), "card spoke main's past commit: {}", card.summary);
    assert!(text.contains("reconnect timer"), "card must name the branch's work: {}", card.summary);
    assert!(text.contains("fixing"), "a fix/ branch is fixing: {}", card.summary);

    // Once a commit is made ON the branch, it speaks for the work again
    run(&["commit", "-am", "fix: clear retry timer after connect"]);
    fs::write(tmp.join("src/reconnect.rs"), "pub fn on_connect() { /* clear timer */ }\n// more\n").unwrap();
    let card = rule_based_compress(&working_state_for(&tmp));
    assert!(card.summary.to_lowercase().contains("retry timer"), "got: {}", card.summary);

    cleanup_test_project(&tmp);
}

/// A card is written when you leave and read when you come back, often hours
/// later. A time phrase baked in at departure is always "moments ago", so a
/// card read six hours later under a "6h ago" header said "You were just
/// configuring...". The ghost's header carries the real age; the summary
/// states no time of its own.
#[test]
fn card_summary_carries_no_stale_time_phrase() {
    let project = setup_test_project("no-stale-time");
    for _ in 0..12 {
        // variants are picked at random; sample enough to hit them all
        let card = rule_based_compress(&working_state_for(&project));
        let s = card.summary.to_lowercase();
        for phrase in ["just now", "moments ago", "you were just", "a little while ago",
                       "not long ago", "recently you", "earlier", "a few hours ago", "yesterday"] {
            assert!(!s.contains(phrase), "stale time phrase '{phrase}' in: {}", card.summary);
        }
    }
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
    // A fresh card reads "just now", never "just now ago"
    assert!(motd.contains("just now") && !motd.contains("just now ago"), "{motd}");

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

    // Hopping to another project: Return carrying the origin, and a
    // mid-flow cause that keeps the browser quiet
    let event = detector
        .record_activity("/tmp/project-b")
        .expect("project hop must emit a switch event");
    match event.kind {
        SwitchKind::Return {
            project_dir,
            from_project,
            cause,
        } => {
            assert_eq!(project_dir, "/tmp/project-b");
            assert_eq!(from_project.as_deref(), Some("/tmp/project-a"));
            assert_eq!(cause, ReturnCause::ProjectSwitch);
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
fn detector_shell_open_gates_on_absence() {
    let detector = SwitchDetector::new(&RevenantConfig::default());

    // A shell opening on a project the daemon has never seen is a
    // return: the daemon cannot prove the user was recently here
    let event = detector
        .shell_opened("/tmp/shell-a")
        .expect("first sighting must summon a ghost");
    assert!(matches!(event.kind, SwitchKind::Return { .. }));

    // A second shell moments later is mid-flow: silent
    assert!(detector.shell_opened("/tmp/shell-a").is_none());

    // Watcher-fed activity keeps new shells silent too
    detector.record_activity("/tmp/shell-a");
    assert!(detector.shell_opened("/tmp/shell-a").is_none());

    // A shell opening on a different project is a switch: the Return
    // carries the origin, like the watcher's project-hop path - but
    // with no machine absence its cause keeps the push channels quiet
    let event = detector
        .shell_opened("/tmp/shell-b")
        .expect("project hop via shell must summon a ghost");
    match event.kind {
        SwitchKind::Return {
            project_dir,
            from_project,
            cause,
        } => {
            assert_eq!(project_dir, "/tmp/shell-b");
            assert_eq!(from_project.as_deref(), Some("/tmp/shell-a"));
            assert_eq!(cause, ReturnCause::ShellOpen);
        }
        other => panic!("expected Return, got {other:?}"),
    }
}

#[test]
fn detector_shell_open_returns_after_absence() {
    // Threshold zero: any measurable gap counts as an absence
    let mut config = RevenantConfig::default();
    config.daemon.absence_threshold_minutes = 0;
    let detector = SwitchDetector::new(&config);

    detector.shell_opened("/tmp/shell-c");
    std::thread::sleep(std::time::Duration::from_millis(10));
    let event = detector
        .shell_opened("/tmp/shell-c")
        .expect("a shell after an absence must summon a ghost");
    match event.kind {
        SwitchKind::Return { from_project, cause, .. } => {
            assert!(from_project.is_none());
            // The whole machine was away: this return wakes everything
            assert_eq!(cause, ReturnCause::MachineReturn);
        }
        other => panic!("expected Return, got {other:?}"),
    }
}

#[test]
fn browser_wakes_only_on_machine_return() {
    // THE QUIET LAW (2026-07-24): the browser banner pushes into a
    // surface the user is actively using, so it speaks only when the
    // user just came back to the machine. Mid-flow hops, branch flits
    // and shell opens stay off it; the terminal keeps its covenant.
    assert!(ReturnCause::MachineReturn.wakes_browser(true));
    assert!(!ReturnCause::ProjectSwitch.wakes_browser(true));
    assert!(!ReturnCause::BranchChange.wakes_browser(true));
    assert!(!ReturnCause::ShellOpen.wakes_browser(true));
    // Config escape hatch restores the old every-return behavior
    assert!(ReturnCause::ProjectSwitch.wakes_browser(false));
    // The default config keeps the law on
    assert!(RevenantConfig::default().ghosts.browser_config.machine_return_only);

    // A branch change carries its quiet cause end to end
    let detector = SwitchDetector::new(&RevenantConfig::default());
    detector.record_activity("/tmp/quiet-a");
    detector.check_branch_change("/tmp/quiet-a", "main");
    let event = detector
        .check_branch_change("/tmp/quiet-a", "feature/x")
        .expect("branch change must emit a switch event");
    match event.kind {
        SwitchKind::Return { cause, .. } => assert_eq!(cause, ReturnCause::BranchChange),
        other => panic!("expected Return, got {other:?}"),
    }

    // A machine absence outranks the hop: waking into a DIFFERENT
    // project is still a machine return and deserves the banner
    let mut config = RevenantConfig::default();
    config.daemon.absence_threshold_minutes = 0;
    let detector = SwitchDetector::new(&config);
    detector.record_activity("/tmp/quiet-b");
    std::thread::sleep(std::time::Duration::from_millis(10));
    let event = detector
        .record_activity("/tmp/quiet-c")
        .expect("hop after machine absence must emit a switch event");
    match event.kind {
        SwitchKind::Return { from_project, cause, .. } => {
            assert_eq!(from_project.as_deref(), Some("/tmp/quiet-b"));
            assert_eq!(cause, ReturnCause::MachineReturn);
        }
        other => panic!("expected Return, got {other:?}"),
    }
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
    let server = Arc::new(GhostHttpServer::new(false, 60));
    let serve = Arc::clone(&server);
    tokio::spawn(async move { serve.serve(port).await });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let post = |path: &'static str, body: &'static str| async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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
        s.write_all(format!("GET /ghost HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").as_bytes())
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

    // Ghosts are transient: a card expires off the wire by its TTL
    let brief = r#"{"type":"inject","card":{"id":"t2","summary":"Brief ghost.","next_step":"","project_dir":"/tmp/x","project_name":"x","ttl_seconds":1}}"#;
    let resp = post("/inject", brief).await;
    assert!(resp.starts_with("HTTP/1.1 204"), "got: {resp}");
    assert!(get().await.contains("Brief ghost."), "fresh card should be served");
    tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
    assert!(
        get().await.contains(r#"{"type":"clear"}"#),
        "expired card must be served as cleared"
    );
}

/// The card names your projects, files and cursor line. A web page you visit
/// must never read it or write into the banner: the server used to answer
/// every origin with `Access-Control-Allow-Origin: *`. Only the browser
/// extension (its own extension origin) and local tools (no Origin at all)
/// get through, and a rebinding Host (evil.example resolving to 127.0.0.1)
/// is refused.
#[tokio::test]
async fn http_server_refuses_web_pages() {
    use revenant::ghost::http::GhostHttpServer;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let port = 17713u16;
    let server = Arc::new(GhostHttpServer::new(false, 60));
    let serve = Arc::clone(&server);
    tokio::spawn(async move { serve.serve(port).await });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    async fn send(port: u16, req: String) -> String {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(req.as_bytes()).await.unwrap();
        let mut resp = String::new();
        s.read_to_string(&mut resp).await.unwrap();
        resp
    }
    let get = |host: String, origin: Option<&'static str>| {
        let origin = origin.map(|o| format!("Origin: {o}\r\n")).unwrap_or_default();
        send(port, format!("GET /ghost HTTP/1.1\r\nHost: {host}\r\n{origin}Connection: close\r\n\r\n"))
    };
    let post = |path: &'static str, origin: Option<&'static str>, body: &'static str| {
        let origin = origin.map(|o| format!("Origin: {o}\r\n")).unwrap_or_default();
        send(port, format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{origin}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ))
    };
    let local = format!("127.0.0.1:{port}");
    let secret = r#"{"type":"inject","card":{"id":"s1","summary":"You were editing payroll/secret.rs.","next_step":"","project_dir":"/tmp/x","project_name":"x","ttl_seconds":120}}"#;
    let fake = r#"{"type":"inject","card":{"id":"f1","summary":"Session expired, sign in at evil.example","next_step":"","project_dir":"/tmp/x","project_name":"x","ttl_seconds":120}}"#;

    // A local tool (rvn, the shell hook: no Origin) sets the card
    assert!(post("/inject", None, secret).await.starts_with("HTTP/1.1 204"));

    // A web page cannot read it
    let resp = get(local.clone(), Some("https://evil.example")).await;
    assert!(resp.starts_with("HTTP/1.1 403"), "got: {resp}");
    assert!(!resp.contains("secret.rs"), "card leaked to a web page: {resp}");

    // A web page cannot write into the banner, clear it, or open a shell return
    for (path, body) in [("/inject", fake), ("/clear", ""), ("/tab", "[]"), ("/shell-open", r#"{"cwd":"/tmp"}"#)] {
        let resp = post(path, Some("https://evil.example"), body).await;
        assert!(resp.starts_with("HTTP/1.1 403"), "{path} from a web page got: {resp}");
    }
    let resp = get(local.clone(), None).await;
    assert!(resp.contains("secret.rs") && !resp.contains("evil.example"), "card was altered: {resp}");

    // Sandboxed frames send Origin: null; also refused
    let resp = get(local.clone(), Some("null")).await;
    assert!(resp.starts_with("HTTP/1.1 403"), "got: {resp}");

    // DNS rebinding: same-origin requests carry no Origin but a foreign Host
    let resp = get(format!("evil.example:{port}"), None).await;
    assert!(resp.starts_with("HTTP/1.1 403"), "got: {resp}");
    assert!(!resp.contains("secret.rs"));

    // The browser extension reads it, by either loopback name
    let resp = get(local.clone(), Some("chrome-extension://abcdefghijklmnop")).await;
    assert!(resp.contains("secret.rs"), "extension must read the card: {resp}");
    let resp = get(format!("localhost:{port}"), Some("moz-extension://1234")).await;
    assert!(resp.contains("secret.rs"), "got: {resp}");

    // No response ever grants cross-origin reads
    assert!(!resp.to_lowercase().contains("access-control-allow-origin: *"));
}

#[tokio::test]
async fn http_shell_open_answers_a_new_shell() {
    use revenant::ghost::http::{GhostHttpServer, ShellBridge};
    use revenant::store::ContextCard;
    use std::sync::Arc;

    let tmp = std::env::temp_dir().join("revenant-test-shell-open");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("project/.git")).unwrap();
    let project_dir = tmp.join("project").to_str().unwrap().to_string();

    // A real store holding one card for the project
    let store = Arc::new(ContextStore::open(&tmp.join("shell.db")).unwrap());
    store.migrate().unwrap();
    store
        .save_card(&ContextCard {
            id: "sh1".into(),
            project_dir: project_dir.clone(),
            project_name: "project".into(),
            summary: "You were wiring the shell-open bridge.".into(),
            next_step: "Drive the endpoint test.".into(),
            created_at: chrono::Utc::now(),
            signals_json: "{}".into(),
            ttl_seconds: 120,
        })
        .unwrap();

    let (switch_tx, mut switch_rx) = tokio::sync::mpsc::channel(8);
    let motd_path = tmp.join("motd");
    let server = Arc::new(GhostHttpServer::new(false, 60).with_shell_bridge(ShellBridge {
        detector: Arc::new(SwitchDetector::new(&RevenantConfig::default())),
        store: Arc::clone(&store),
        switch_tx,
        motd_path: motd_path.clone(),
    }));
    let port = 17712u16;
    let serve = Arc::clone(&server);
    tokio::spawn(async move { serve.serve(port).await });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let shell_open = |cwd: String| async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let body = format!(r#"{{"cwd":"{cwd}"}}"#);
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let req = format!(
            "POST /shell-open HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut resp = String::new();
        s.read_to_string(&mut resp).await.unwrap();
        resp
    };

    // A shell opening inside the project pulls its ghost and fires a Return
    let resp = shell_open(project_dir.clone()).await;
    assert!(resp.starts_with("HTTP/1.1 200"), "got: {resp}");
    assert!(resp.contains("You were wiring the shell-open bridge."));
    assert!(resp.contains("REVENANT"), "response must be the rendered MOTD");
    let event = switch_rx.recv().await.expect("shell-open must emit a switch event");
    assert!(matches!(event.kind, SwitchKind::Return { .. }));

    // A second shell moments later is STILL greeted (opening a terminal
    // is asking "where was I?"), but mid-flow it must not wake the
    // other channels: no switch event
    let resp = shell_open(project_dir.clone()).await;
    assert!(resp.starts_with("HTTP/1.1 200"), "got: {resp}");
    assert!(resp.contains("You were wiring the shell-open bridge."));
    assert!(switch_rx.try_recv().is_err(), "mid-flow shell must not emit an event");

    // A shell outside any project still gets the freshest card on record
    let resp = shell_open("/".to_string()).await;
    assert!(resp.starts_with("HTTP/1.1 200"), "got: {resp}");
    assert!(resp.contains("You were wiring the shell-open bridge."));

    // With a live MOTD on disk, a shell in THAT project sees it as-is
    fs::write(&motd_path, "LIVE GHOST BODY").unwrap();
    fs::write(
        tmp.join("motd.json"),
        format!(r#"{{"card_id":"live","project_dir":"{project_dir}"}}"#),
    )
    .unwrap();
    let resp = shell_open(project_dir.clone()).await;
    assert!(resp.starts_with("HTTP/1.1 200"), "got: {resp}");
    assert!(resp.contains("LIVE GHOST BODY"));

    // Field regression (2026-09-28): the live MOTD belonged to another
    // project (you had just hopped to billing) and a new tab opened in
    // harbor was greeted with billing's note. A shell gets its own
    // project's card; another project's live ghost is not its greeting.
    fs::write(
        tmp.join("motd.json"),
        r#"{"card_id":"live","project_dir":"/somewhere/else"}"#,
    )
    .unwrap();
    let resp = shell_open(project_dir.clone()).await;
    assert!(!resp.contains("LIVE GHOST BODY"), "served another project's ghost: {resp}");
    assert!(resp.contains("You were wiring the shell-open bridge."), "got: {resp}");
    fs::remove_file(&motd_path).unwrap();
    fs::remove_file(tmp.join("motd.json")).unwrap();

    // Malformed body is rejected
    let bad = {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(
            format!("POST /shell-open HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 8\r\nConnection: close\r\n\r\nnot json").as_bytes(),
        )
        .await
        .unwrap();
        let mut resp = String::new();
        s.read_to_string(&mut resp).await.unwrap();
        resp
    };
    assert!(bad.starts_with("HTTP/1.1 400"), "got: {bad}");

    cleanup_test_project(&tmp);
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
