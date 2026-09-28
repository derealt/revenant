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
        // Unknown, as in snapshots stored before the field existed
        own_commits: None,
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
            vec!["batch_size", "legacy-importer", "import", "pipeline", "exploring"],
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
fn cursor_anchor_sharpens_the_next_step_and_rides_the_card() {
    use revenant::signals::editor::{CursorAnchor, EditorState};

    let mut s = state("anchored", None, None);
    s.editor = Some(EditorState {
        open_files: vec![],
        active_file: Some("src/reconnect.rs".into()),
        active_language: Some("rust".into()),
        cursor: Some(CursorAnchor {
            file: "/tmp/corpus/anchored/src/reconnect.rs".into(),
            line: 47,
        }),
    });

    let card = rule_based_compress(&s);
    assert!(
        card.next_step.contains("reconnect.rs:47"),
        "next step should point at the cursor, got: {}",
        card.next_step
    );

    // The anchor must survive into the card for the editor ghost to use
    let signals: serde_json::Value = serde_json::from_str(&card.signals_json).unwrap();
    assert_eq!(
        signals["editor"]["cursor"]["file"],
        "/tmp/corpus/anchored/src/reconnect.rs"
    );
    assert_eq!(signals["editor"]["cursor"]["line"], 47);
}

#[test]
fn next_step_restores_intent_and_never_coaches() {
    use revenant::signals::editor::{CursorAnchor, EditorState};

    // Many files in flight: the old predictor lectured ("consider
    // breaking them into smaller commits"). A next step restores what
    // YOU were about to do; it never judges how you work.
    let many: Vec<(&str, ChangeStatus)> = vec![
        ("theme/archive.php", ChangeStatus::Modified),
        ("theme/single.php", ChangeStatus::Modified),
        ("theme/style.css", ChangeStatus::Modified),
        ("theme/functions.php", ChangeStatus::Modified),
        ("theme/header.php", ChangeStatus::Modified),
        ("theme/footer.php", ChangeStatus::Modified),
        ("theme/reader.php", ChangeStatus::Modified),
        ("theme/fonts.css", ChangeStatus::Modified),
        ("theme/nav.php", ChangeStatus::Modified),
        ("theme/page.php", ChangeStatus::Modified),
    ];
    let mut s = state(
        "wp-theme",
        Some(git_state(
            "dev",
            "archive reader body: floor font at 18px; bump v1.0.16",
            &many,
            "unstaged: 10 files changed",
        )),
        None,
    );
    let card = rule_based_compress(&s);
    assert!(
        !card.next_step.to_lowercase().contains("consider"),
        "next step must not coach, got: {}",
        card.next_step
    );
    assert!(
        !card.next_step.to_lowercase().contains("lot of changes"),
        "next step must not judge volume, got: {}",
        card.next_step
    );

    // With a cursor anchor present, the anchor outranks generic hints
    // even when git is busy
    s.editor = Some(EditorState {
        open_files: vec![],
        active_file: Some("theme/reader.php".into()),
        active_language: Some("php".into()),
        cursor: Some(CursorAnchor {
            file: "/tmp/corpus/wp-theme/theme/reader.php".into(),
            line: 88,
        }),
    });
    let card = rule_based_compress(&s);
    assert!(
        card.next_step.contains("reader.php:88"),
        "cursor anchor must outrank generic git hints, got: {}",
        card.next_step
    );
}

/// THE CARD LAW enforcement: a next step restores where you were;
/// it never orders, coaches, or judges. The only imperatives allowed
/// are resume anchors to the user's own position.
fn assert_restores_never_orders(label: &str, next: &str) {
    let lower = next.to_lowercase();
    const FORBIDDEN: &[&str] = &[
        "commit them",
        "commit when",
        "commit if",
        "consider",
        "don't forget",
        "make sure",
        "might be ready",
        "run the tests",
        "run tests",
        "run install",
        "run it against",
        "stage your",
        "then stage",
        "and commit",
        "fix the build errors",
        "verify the",
        "try again",
        "continue with your",
    ];
    for phrase in FORBIDDEN {
        assert!(
            !lower.contains(phrase),
            "{label}: next step orders/coaches (\"{phrase}\"): {next}"
        );
    }
    const ORDER_STARTERS: &[&str] = &[
        "run ", "fix ", "stage ", "verify ", "wire ", "commit ", "check ", "don't ",
        "make ", "add ", "update ", "install ",
    ];
    if !(next.starts_with("Pick up at ") || next.starts_with("Continue from ")) {
        for starter in ORDER_STARTERS {
            assert!(
                !lower.starts_with(starter),
                "{label}: next step opens with an order (\"{starter}\"): {next}"
            );
        }
    }
}

#[test]
fn next_step_never_orders_across_the_whole_rule_table() {
    use revenant::signals::editor::EditorState;

    // One state per predictor rule, none with a cursor anchor, so every
    // generic branch is held to the law
    let mut table: Vec<(&str, WorkingState)> = vec![
        (
            "test command last, fix in flight",
            state(
                "fixer",
                Some(git_state(
                    "fix/timer",
                    "clear the timer on success",
                    &[("src/timer.rs", ChangeStatus::Modified)],
                    "unstaged: 1 file changed",
                )),
                Some(terminal_state(&["cargo test timer"])),
            ),
        ),
        (
            "test command last, tree quiet",
            state("greenish", None, Some(terminal_state(&["cargo test"]))),
        ),
        (
            "staged only, commit in flight",
            state(
                "stager",
                Some(git_state(
                    "dev",
                    "utility sweep",
                    &[
                        ("src/util/dates.ts", ChangeStatus::Added),
                        ("src/util/strings.ts", ChangeStatus::Added),
                    ],
                    "staged: 2 files changed",
                )),
                None,
            ),
        ),
        (
            "staged plus unstaged test edits",
            state(
                "half-staged",
                Some(git_state(
                    "dev",
                    "extract the parser",
                    &[
                        ("src/parser.rs", ChangeStatus::Added),
                        ("tests/parser_test.rs", ChangeStatus::Modified),
                    ],
                    "mixed",
                )),
                None,
            ),
        ),
        (
            "unstaged edits with tests present",
            state(
                "test-toucher",
                Some(git_state(
                    "dev",
                    "harden the decoder",
                    &[
                        ("src/decode.rs", ChangeStatus::Modified),
                        ("tests/decode_test.rs", ChangeStatus::Modified),
                    ],
                    "unstaged: 2 files changed",
                )),
                None,
            ),
        ),
        (
            "single new file being built out",
            state(
                "greenfield",
                Some(git_state(
                    "feature/export",
                    "scaffold the exporter",
                    &[
                        ("src/export/csv.rs", ChangeStatus::Added),
                        ("src/export/mod.rs", ChangeStatus::Modified),
                    ],
                    "mixed",
                )),
                None,
            ),
        ),
        (
            "dependency manifest edited",
            state(
                "dep-shift",
                Some(git_state(
                    "dev",
                    "pull in the retry crate",
                    &[
                        ("Cargo.toml", ChangeStatus::Modified),
                        ("src/net.rs", ChangeStatus::Modified),
                    ],
                    "unstaged: 2 files changed",
                )),
                None,
            ),
        ),
        (
            "fresh migration written",
            state(
                "migrator",
                Some(git_state(
                    "feature/index",
                    "index the lookups",
                    &[
                        ("migrations/0004_add_index.sql", ChangeStatus::Untracked),
                        ("migrations/0005_backfill.sql", ChangeStatus::Untracked),
                    ],
                    "",
                )),
                None,
            ),
        ),
        (
            "feature branch at a clean point",
            state(
                "cleanstop",
                Some(git_state("feature/settled", "land the settlement", &[], "")),
                None,
            ),
        ),
        (
            "unstaged only, no tests",
            state(
                "loose-edits",
                Some(git_state(
                    "dev",
                    "reword the banner",
                    &[("src/banner.rs", ChangeStatus::Modified)],
                    "unstaged: 1 file changed",
                )),
                None,
            ),
        ),
        (
            "build failing in terminal",
            state(
                "builder",
                None,
                Some(terminal_state(&["cargo build 2>&1 | tee build-errors.log"])),
            ),
        ),
        (
            "fresh install in terminal",
            state("installer", None, Some(terminal_state(&["npm install zod"]))),
        ),
        (
            "work parked in a stash",
            state("stasher", None, Some(terminal_state(&["git stash push -m wip"]))),
        ),
        (
            "files just staged in terminal",
            state("adder", None, Some(terminal_state(&["git add -A"]))),
        ),
        (
            "deployment in flight",
            state("shipper", None, Some(terminal_state(&["fly deploy"]))),
        ),
        (
            "dev server running",
            state("server", None, Some(terminal_state(&["npm run dev"]))),
        ),
        ("nothing on record", state("bare", None, None)),
    ];

    // Active-file fallback: the allowed anchor form without a cursor
    let mut anchored = state("resumer", None, None);
    anchored.editor = Some(EditorState {
        open_files: vec![],
        active_file: Some("src/resume.rs".into()),
        active_language: Some("rust".into()),
        cursor: None,
    });
    table.push(("active file fallback", anchored));

    for (label, s) in &table {
        let card = rule_based_compress(s);
        assert_restores_never_orders(label, &card.next_step);
        assert!(!card.next_step.is_empty(), "{label}: empty next step");
    }

    // The corpus is held to the same law
    for (label, s, _) in corpus() {
        let card = rule_based_compress(&s);
        assert_restores_never_orders(label, &card.next_step);
    }
}

#[test]
fn cursor_anchor_outranks_the_staged_commit_hint() {
    use revenant::signals::editor::{CursorAnchor, EditorState};

    // The 2026-07-24 banner: staged-only git state used to beat the
    // cursor anchor and told the user to commit. The anchor is the
    // sharpest signal of where the hands were; it wins over ALL git
    // state, not just the hints that happened to sit below it.
    let mut s = state(
        "awatum",
        Some(git_state(
            "dev",
            "utility sweep",
            &[
                ("src/lib/dates.ts", ChangeStatus::Added),
                ("src/lib/strings.ts", ChangeStatus::Added),
            ],
            "staged: 2 files changed",
        )),
        None,
    );
    s.editor = Some(EditorState {
        open_files: vec![],
        active_file: Some("src/lib/dates.ts".into()),
        active_language: Some("typescript".into()),
        cursor: Some(CursorAnchor {
            file: "/tmp/corpus/awatum/src/lib/dates.ts".into(),
            line: 142,
        }),
    });

    let card = rule_based_compress(&s);
    assert!(
        card.next_step.contains("dates.ts:142"),
        "cursor anchor must outrank the staged-changes rule, got: {}",
        card.next_step
    );
    assert_restores_never_orders("staged with anchor", &card.next_step);
}

#[test]
fn cards_name_real_files_never_taxonomy_labels() {
    use revenant::signals::editor::{CursorAnchor, EditorState};

    // The 2026-07-24 banner: "editing utility code in Awatum" about a
    // real file under lib/. The card must speak the file's own name;
    // the folder taxonomy may count clusters but never eat the name.
    let mut s = state(
        "awatum",
        Some(git_state(
            "dev",
            "",
            &[("src/lib/reportBuilder.ts", ChangeStatus::Modified)],
            "unstaged: 1 file changed, 12 insertions(+)",
        )),
        None,
    );
    s.editor = Some(EditorState {
        open_files: vec![],
        active_file: Some("src/lib/reportBuilder.ts".into()),
        active_language: Some("typescript".into()),
        cursor: Some(CursorAnchor {
            file: "/tmp/corpus/awatum/src/lib/reportBuilder.ts".into(),
            line: 12,
        }),
    });
    let card = rule_based_compress(&s);
    let banner_card = format!("{} {}", card.summary, card.next_step);
    assert!(
        banner_card.contains("reportBuilder.ts"),
        "card must name the real file, got: {banner_card}"
    );

    const DEAD_LABELS: &[&str] = &[
        "utility code",
        "data models",
        "state management",
        "api endpoints",
        "ui components",
        "service layer",
        "agent modules",
        "schema definitions",
        "payment logic",
        "websocket code",
        "background jobs",
        "signal modules",
        "ghost injectors",
        "database migrations",
    ];
    let mut all_cards: Vec<String> = corpus()
        .into_iter()
        .map(|(_, s, _)| {
            let c = rule_based_compress(&s);
            format!("{} {}", c.summary, c.next_step)
        })
        .collect();
    all_cards.push(banner_card);
    for text in &all_cards {
        let lower = text.to_lowercase();
        for label in DEAD_LABELS {
            assert!(
                !lower.contains(label),
                "taxonomy label \"{label}\" leaked into a card: {text}"
            );
        }
    }
}

#[test]
fn cards_never_echo_the_project_name_twice() {
    // "reviewing legacy-importer in legacy-importer": a topic equal to
    // the project is no topic at all - the card says the project once
    // or says less, never pads with an echo
    let mut states: Vec<(String, WorkingState)> = corpus()
        .into_iter()
        .map(|(l, s, _)| (l.to_string(), s))
        .collect();
    // The worst case: nothing to say but the project itself
    states.push((
        "signal-poor exploration".into(),
        state(
            "lonely",
            Some(git_state("main", "", &[], "")),
            Some(terminal_state(&["git log --oneline -20", "tree src/"])),
        ),
    ));
    states.push(("bare project".into(), state("mystery", None, None)));
    for (label, s) in &states {
        let card = rule_based_compress(s);
        let count = card
            .summary
            .to_lowercase()
            .matches(&s.project_name.to_lowercase())
            .count();
        assert!(
            count <= 1,
            "{label}: project name said {count} times in: {}",
            card.summary
        );
    }
}

#[test]
fn exploration_cards_surface_the_question() {
    // The essence bar (CLAUDE.md: restore what you were TRYING to do;
    // recent searches are part of the cognitive snapshot): the engine
    // held "rg 'batch_size' src/" and used to say "reviewing
    // legacy-importer". The search string is the user's own question,
    // the closest deterministic proxy for where the head was.
    let s = state(
        "legacy-importer",
        Some(git_state("main", "import pipeline hardening", &[], "")),
        Some(terminal_state(&[
            "rg 'batch_size' src/",
            "git log --oneline -20",
            "tree src/pipeline",
        ])),
    );
    let card = rule_based_compress(&s);
    assert!(
        card.summary.contains("batch_size"),
        "exploration card must surface the search question, got: {}",
        card.summary
    );
}

#[test]
fn summaries_never_stack_gerunds() {
    // "you were deploying bumping to v2.4.0": an intent verb must never
    // be glued in front of a rewrite that carries its own gerund. The
    // topic seam strips the rewrite's gerund; every summary path must
    // go through it.
    const VERB_GERUNDS: &[&str] = &[
        "fixing", "adding", "updating", "refactoring", "removing", "merging",
        "testing", "bumping", "reverting", "setting", "implementing",
        "reorganizing", "working", "creating", "improving", "enabling",
        "disabling", "deploying", "editing", "documenting", "exploring",
        "reviewing", "building", "debugging", "clearing", "extracting",
        "rewriting", "importing", "releasing",
    ];
    for (label, s, _) in corpus() {
        let card = rule_based_compress(&s);
        let words: Vec<String> = card
            .summary
            .to_lowercase()
            .split_whitespace()
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
            .collect();
        for pair in words.windows(2) {
            assert!(
                !(VERB_GERUNDS.contains(&pair[0].as_str())
                    && VERB_GERUNDS.contains(&pair[1].as_str())),
                "{label}: stacked gerunds \"{} {}\" in: {}",
                pair[0],
                pair[1],
                card.summary
            );
        }
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
