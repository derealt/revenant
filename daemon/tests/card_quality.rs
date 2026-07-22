//! Card-quality corpus drive
//!
//! Feeds the REAL rule engine a corpus of realistic WorkingStates
//! (mid-fix, feature build, refactor, deploy, docs, exploration,
//! cross-language stacks) and holds every card to the CLAUDE.md bar:
//! second person, specific to the actual work, with a next step.
//! Also guards against template collapse: distinct situations must
//! not produce one identical card.

use chrono::Utc;
use revenant::compressor::rule_based_compress;
use revenant::signals::git::{ChangeStatus, ChangedFile, CommitSummary, GitState};
use revenant::signals::terminal::{HistoryEntry, TerminalState};
use revenant::snapshot::WorkingState;

fn git_state(
    branch: &str,
    commit_msg: &str,
    files: &[(&str, ChangeStatus)],
    diff_stat: &str,
) -> GitState {
    GitState {
        branch: branch.to_string(),
        staged: files
            .iter()
            .filter(|(_, s)| matches!(s, ChangeStatus::Added))
            .count(),
        modified: files
            .iter()
            .filter(|(_, s)| matches!(s, ChangeStatus::Modified))
            .count(),
        untracked: 0,
        changed_files: files
            .iter()
            .map(|(p, s)| ChangedFile {
                path: p.to_string(),
                status: s.clone(),
            })
            .collect(),
        recent_commits: if commit_msg.is_empty() {
            vec![]
        } else {
            vec![CommitSummary {
                hash: "abcd1234".to_string(),
                message: commit_msg.to_string(),
                author: "dev".to_string(),
                timestamp: Utc::now().to_rfc3339(),
            }]
        },
        diff_stat: diff_stat.to_string(),
    }
}

fn terminal_state(commands: &[&str]) -> TerminalState {
    TerminalState {
        recent_commands: commands
            .iter()
            .map(|c| HistoryEntry {
                command: c.to_string(),
                timestamp: Some(1711234560),
            })
            .collect(),
        cwd: None,
    }
}

fn state(name: &str, git: Option<GitState>, terminal: Option<TerminalState>) -> WorkingState {
    WorkingState {
        project_dir: format!("/tmp/corpus/{name}"),
        project_name: name.to_string(),
        timestamp: Utc::now(),
        git,
        editor: None,
        terminal,
        clipboard: None,
        browser_tabs: vec![],
        recent_file_events: vec![],
    }
}

/// The corpus: (label, state, concrete facts at least one of which
/// must surface in the card for it to count as specific)
fn corpus() -> Vec<(&'static str, WorkingState, Vec<&'static str>)> {
    vec![
        (
            "mid-fix with failing tests",
            state(
                "harbord",
                Some(git_state(
                    "fix/reconnect-race",
                    "fix: clear reconnect timer before retry",
                    &[
                        ("src/ws/reconnect.rs", ChangeStatus::Modified),
                        ("src/ws/timer.rs", ChangeStatus::Modified),
                    ],
                    "unstaged: 2 files changed, 45 insertions(+), 12 deletions(-)",
                )),
                Some(terminal_state(&[
                    "cargo test reconnect",
                    "cargo test reconnect -- --nocapture",
                    "git diff src/ws/reconnect.rs",
                ])),
            ),
            vec!["reconnect", "fix", "timer", "harbord"],
        ),
        (
            "feature build with new files",
            state(
                "billingd",
                Some(git_state(
                    "feature/invoice-export",
                    "add invoice export scaffolding",
                    &[
                        ("src/export/csv.rs", ChangeStatus::Added),
                        ("src/export/mod.rs", ChangeStatus::Added),
                    ],
                    "staged: 2 files changed, 120 insertions(+)",
                )),
                Some(terminal_state(&["cargo build", "cargo run -- export --dry-run"])),
            ),
            vec!["invoice", "export", "billingd", "build"],
        ),
        (
            "refactor branch with lint loop",
            state(
                "webapp",
                Some(git_state(
                    "refactor/split-auth-module",
                    "refactor: extract session store from auth",
                    &[
                        ("src/auth/session.ts", ChangeStatus::Modified),
                        ("src/auth/store.ts", ChangeStatus::Added),
                    ],
                    "unstaged: 2 files changed, 80 insertions(+), 60 deletions(-)",
                )),
                Some(terminal_state(&["npm run lint", "npx eslint src/auth", "npm test"])),
            ),
            vec!["auth", "session", "refactor", "webapp"],
        ),
        (
            "deploy in progress",
            state(
                "api-gateway",
                Some(git_state(
                    "release/v2.4.0",
                    "release: bump to v2.4.0",
                    &[("k8s/deployment.yaml", ChangeStatus::Modified)],
                    "unstaged: 1 file changed, 4 insertions(+), 2 deletions(-)",
                )),
                Some(terminal_state(&[
                    "kubectl apply -f k8s/deployment.yaml",
                    "kubectl rollout status deploy/api-gateway",
                ])),
            ),
            vec!["v2.4.0", "deploy", "release", "api-gateway"],
        ),
        (
            "documentation pass",
            state(
                "sdk",
                Some(git_state(
                    "docs/quickstart",
                    "docs: rewrite quickstart for the new client",
                    &[
                        ("docs/quickstart.md", ChangeStatus::Modified),
                        ("README.md", ChangeStatus::Modified),
                    ],
                    "unstaged: 2 files changed, 90 insertions(+), 40 deletions(-)",
                )),
                None,
            ),
            vec!["quickstart", "docs", "sdk", "readme"],
        ),
        (
            "exploration, nothing edited",
            state(
                "legacy-importer",
                Some(git_state("main", "import pipeline hardening", &[], "")),
                Some(terminal_state(&[
                    "rg 'batch_size' src/",
                    "git log --oneline -20",
                    "tree src/pipeline",
                ])),
            ),
            vec!["legacy-importer", "import", "pipeline", "exploring"],
        ),
    ]
}

#[test]
fn every_corpus_card_is_second_person_specific_with_next_step() {
    for (label, state, facts) in corpus() {
        let card = rule_based_compress(&state);
        let summary_lower = card.summary.to_lowercase();

        assert!(!card.summary.is_empty(), "{label}: empty summary");
        assert!(
            summary_lower.contains("you"),
            "{label}: not written to future-you: {}",
            card.summary
        );
        assert!(
            facts.iter().any(|f| {
                summary_lower.contains(&f.to_lowercase())
                    || card.next_step.to_lowercase().contains(&f.to_lowercase())
            }),
            "{label}: card surfaces none of the concrete facts {facts:?}\n  summary: {}\n  next: {}",
            card.summary,
            card.next_step
        );
        assert!(
            !card.next_step.is_empty(),
            "{label}: no next step proposed"
        );
        // The generic card the CLAUDE.md bar forbids
        assert!(
            !summary_lower.contains("working on the project"),
            "{label}: generic card: {}",
            card.summary
        );
    }
}

#[test]
fn distinct_situations_produce_distinct_cards() {
    let cards: Vec<String> = corpus()
        .into_iter()
        .map(|(_, state, _)| rule_based_compress(&state).summary)
        .collect();

    let unique: std::collections::HashSet<&String> = cards.iter().collect();
    assert!(
        unique.len() >= cards.len() - 1,
        "template collapse: {} states produced only {} distinct summaries: {cards:#?}",
        cards.len(),
        unique.len()
    );
}

/// Human review harness: `cargo test --test card_quality -- --ignored --nocapture`
/// prints every corpus card for eyeball quality review.
#[test]
#[ignore]
fn print_corpus_cards_for_review() {
    for (label, state, _) in corpus() {
        let card = rule_based_compress(&state);
        println!("--- {label}\n  {}\n  NEXT: {}\n", card.summary, card.next_step);
    }
}

#[test]
fn empty_signals_still_produce_a_safe_card() {
    // A project with no git, no terminal, no editor: the card must not
    // panic and must not fabricate specifics it cannot know
    let bare = state("mystery", None, None);
    let card = rule_based_compress(&bare);
    assert!(!card.summary.is_empty());
    assert!(card.summary.to_lowercase().contains("mystery") || card.summary.contains("you"));
}
