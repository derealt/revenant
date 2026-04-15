//! Context compression — WorkingState → ContextCard
//!
//! Two modes:
//! 1. Rule-based: a smart engine that produces LLM-quality context cards
//!    from signals alone — no API calls, no network, pure pattern matching.
//! 2. LLM-based: sends signals to a local or remote LLM for natural language compression
//!
//! The rule-based mode is always available and produces human-quality output
//! through intent classification, path semantics, commit rewriting, file
//! clustering, next-step prediction, temporal framing, and template selection.

use anyhow::{bail, Result};
use chrono::{Datelike, Utc};
use uuid::Uuid;

use crate::config::LlmConfig;
use crate::signals::git::ChangeStatus;
use crate::snapshot::WorkingState;
use crate::store::ContextCard;

// ─── 1. Intent Classifier ───────────────────────────────────────────────────

/// What the user was fundamentally doing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Intent {
    Building,
    Fixing,
    Testing,
    Refactoring,
    Deploying,
    Reviewing,
    Documenting,
    Exploring,
    Creating,
    Cleaning,
    Merging,
    Configuring,
    Editing,
}

impl Intent {
    /// Present-participle verb for templates
    fn verb(self) -> &'static str {
        match self {
            Intent::Building => "building",
            Intent::Fixing => "fixing",
            Intent::Testing => "testing",
            Intent::Refactoring => "refactoring",
            Intent::Deploying => "deploying",
            Intent::Reviewing => "reviewing",
            Intent::Documenting => "documenting",
            Intent::Exploring => "exploring",
            Intent::Creating => "creating",
            Intent::Cleaning => "cleaning up",
            Intent::Merging => "merging",
            Intent::Configuring => "configuring",
            Intent::Editing => "editing",
        }
    }

    /// Past-tense phrasing for alternate templates
    fn past(self) -> &'static str {
        match self {
            Intent::Building => "building out",
            Intent::Fixing => "working on a fix for",
            Intent::Testing => "running tests on",
            Intent::Refactoring => "refactoring",
            Intent::Deploying => "deploying",
            Intent::Reviewing => "reviewing",
            Intent::Documenting => "writing docs for",
            Intent::Exploring => "exploring",
            Intent::Creating => "setting up",
            Intent::Cleaning => "tidying up",
            Intent::Merging => "merging",
            Intent::Configuring => "configuring",
            Intent::Editing => "making changes to",
        }
    }
}

/// Classify user intent from all available signals
fn classify_intent(state: &WorkingState) -> Intent {
    let mut scores: Vec<(Intent, i32)> = Vec::new();

    // From commit message prefix
    if let Some(ref git) = state.git {
        if let Some(commit) = git.recent_commits.first() {
            let msg = commit.message.to_lowercase();
            let stripped = strip_conventional_prefix(&msg);
            if stripped.starts_with("fix") || msg.starts_with("fix") {
                scores.push((Intent::Fixing, 10));
            } else if stripped.starts_with("add") || msg.starts_with("feat") || msg.starts_with("add") {
                scores.push((Intent::Building, 10));
            } else if stripped.starts_with("update") || stripped.starts_with("improve") {
                scores.push((Intent::Editing, 8));
            } else if stripped.starts_with("refactor") || msg.starts_with("refactor") {
                scores.push((Intent::Refactoring, 10));
            } else if stripped.starts_with("remove") || stripped.starts_with("delete") || stripped.starts_with("clean") {
                scores.push((Intent::Cleaning, 10));
            } else if stripped.starts_with("merge") || msg.starts_with("merge") {
                scores.push((Intent::Merging, 10));
            } else if stripped.starts_with("test") || msg.starts_with("test") {
                scores.push((Intent::Testing, 10));
            } else if stripped.starts_with("doc") || msg.starts_with("docs") {
                scores.push((Intent::Documenting, 10));
            } else if stripped.starts_with("deploy") || stripped.starts_with("release") {
                scores.push((Intent::Deploying, 10));
            } else if stripped.starts_with("config") || stripped.starts_with("setup") || stripped.starts_with("init") {
                scores.push((Intent::Configuring, 8));
            }
        }

        // From branch name
        let branch = git.branch.to_lowercase();
        if branch.starts_with("fix/") || branch.starts_with("bugfix/") || branch.starts_with("hotfix/") {
            scores.push((Intent::Fixing, 8));
        } else if branch.starts_with("feature/") || branch.starts_with("feat/") {
            scores.push((Intent::Building, 8));
        } else if branch.starts_with("refactor/") {
            scores.push((Intent::Refactoring, 8));
        } else if branch.starts_with("test/") {
            scores.push((Intent::Testing, 7));
        } else if branch.starts_with("docs/") || branch.starts_with("doc/") {
            scores.push((Intent::Documenting, 7));
        } else if branch.starts_with("release/") || branch.starts_with("deploy/") {
            scores.push((Intent::Deploying, 7));
        } else if branch.starts_with("chore/") || branch.starts_with("cleanup/") {
            scores.push((Intent::Cleaning, 6));
        }

        // From git status composition
        let all_added = git.changed_files.iter().all(|f| matches!(f.status, ChangeStatus::Added | ChangeStatus::Untracked));
        let all_deleted = git.changed_files.iter().all(|f| matches!(f.status, ChangeStatus::Deleted));
        if !git.changed_files.is_empty() {
            if all_added {
                scores.push((Intent::Creating, 6));
            } else if all_deleted {
                scores.push((Intent::Cleaning, 6));
            }
        }
    }

    // From terminal commands
    let commands = state.recent_commands();
    for cmd in &commands {
        let cmd_lower = cmd.to_lowercase();
        let first = cmd_lower.split_whitespace().next().unwrap_or("");
        if first == "pytest" || first == "jest" || cmd_lower.contains("cargo test")
            || cmd_lower.contains("npm test") || cmd_lower.contains("yarn test")
            || cmd_lower.contains("go test") || cmd_lower.contains("rspec")
            || cmd_lower.contains("mocha") || cmd_lower.contains("vitest")
        {
            scores.push((Intent::Testing, 7));
        } else if cmd_lower.contains("build") || first == "make" || cmd_lower.contains("cargo build")
            || cmd_lower.contains("npm run build") || cmd_lower.contains("tsc")
        {
            scores.push((Intent::Building, 5));
        } else if cmd_lower.contains("deploy") || cmd_lower.contains("kubectl")
            || cmd_lower.contains("terraform") || cmd_lower.contains("ansible")
            || cmd_lower.contains("fly deploy") || cmd_lower.contains("vercel")
            || cmd_lower.contains("netlify")
        {
            scores.push((Intent::Deploying, 8));
        } else if first == "docker" || first == "docker-compose" || first == "podman" {
            scores.push((Intent::Deploying, 4));
        } else if cmd_lower.contains("lint") || cmd_lower.contains("eslint")
            || cmd_lower.contains("clippy") || cmd_lower.contains("fmt")
            || cmd_lower.contains("prettier") || cmd_lower.contains("black")
        {
            scores.push((Intent::Refactoring, 4));
        }
    }

    // Tally scores
    if scores.is_empty() {
        return Intent::Editing;
    }

    let mut best = Intent::Editing;
    let mut best_score = 0i32;
    for (intent, score) in &scores {
        if *score > best_score {
            best_score = *score;
            best = *intent;
        }
    }
    best
}

// ─── 2. Path Semantics Engine ───────────────────────────────────────────────

/// Human-readable description of a file path
struct PathDescription {
    short_name: String,
    category: &'static str,
}

/// Map a file path to a human-readable description
fn describe_path(path: &str) -> PathDescription {
    let lower = path.to_lowercase();
    let filename = path.rsplit('/').next().unwrap_or(path);
    let filename_lower = filename.to_lowercase();

    // Check extension first
    let ext = filename.rsplit('.').next().unwrap_or("");

    // Special files
    if filename_lower == "package.json" {
        return PathDescription { short_name: "package.json".into(), category: "config" };
    }
    if filename_lower == "cargo.toml" {
        return PathDescription { short_name: "Cargo.toml".into(), category: "config" };
    }
    if filename_lower == "dockerfile" || filename_lower.starts_with("docker-compose") {
        return PathDescription { short_name: filename.into(), category: "infra" };
    }
    if filename_lower == "tsconfig.json" || filename_lower == "tsconfig.build.json" {
        return PathDescription { short_name: "TypeScript config".into(), category: "config" };
    }
    if filename_lower == ".env" || filename_lower.starts_with(".env.") {
        return PathDescription { short_name: "environment config".into(), category: "config" };
    }
    if filename_lower == "makefile" || filename_lower == "justfile" {
        return PathDescription { short_name: filename.into(), category: "build" };
    }
    if filename_lower == "readme.md" || filename_lower == "claude.md" {
        return PathDescription { short_name: filename.into(), category: "docs" };
    }

    // Directory-based patterns

    // Test files
    if lower.contains("/test/") || lower.contains("/tests/") || lower.contains("/__tests__/")
        || lower.contains("/spec/") || filename_lower.contains(".test.")
        || filename_lower.contains(".spec.") || filename_lower.contains("_test.")
        || filename_lower.starts_with("test_")
    {
        let domain = infer_domain_from_path(path);
        let name = if let Some(d) = domain {
            format!("{d} tests")
        } else {
            "tests".into()
        };
        return PathDescription { short_name: name, category: "test" };
    }

    // Migration files
    if lower.contains("/migration") || lower.contains("/migrate") {
        return PathDescription { short_name: "database migrations".into(), category: "database" };
    }

    // API/route files
    if lower.contains("/api/") || lower.contains("/routes/") || lower.contains("/endpoints/")
        || lower.contains("/handlers/") || lower.contains("/controllers/")
    {
        let domain = infer_domain_from_filename(filename);
        let name = if let Some(d) = domain {
            format!("the {d} API")
        } else {
            "API endpoints".into()
        };
        return PathDescription { short_name: name, category: "api" };
    }

    // Component files
    if lower.contains("/components/") || lower.contains("/views/") || lower.contains("/pages/") {
        let domain = infer_domain_from_filename(filename);
        let name = if let Some(d) = domain {
            format!("the {d} component")
        } else {
            "UI components".into()
        };
        return PathDescription { short_name: name, category: "ui" };
    }

    // Service/module files
    if lower.contains("/services/") || lower.contains("/service/") {
        let domain = infer_domain_from_filename(filename);
        let name = if let Some(d) = domain {
            format!("the {d} service")
        } else {
            "service layer".into()
        };
        return PathDescription { short_name: name, category: "service" };
    }

    // Agent files
    if lower.contains("/agents/") || lower.contains("/agent/") {
        let domain = infer_domain_from_filename(filename);
        let name = if let Some(d) = domain {
            format!("the {d} agent")
        } else {
            "agent modules".into()
        };
        return PathDescription { short_name: name, category: "agent" };
    }

    // Model/schema files
    if lower.contains("/models/") || lower.contains("/schema/") || lower.contains("/schemas/")
        || lower.contains("/entities/") || lower.contains("/types/")
    {
        return PathDescription { short_name: "data models".into(), category: "model" };
    }

    // Middleware
    if lower.contains("/middleware/") {
        return PathDescription { short_name: "middleware".into(), category: "middleware" };
    }

    // Utils/helpers
    if lower.contains("/utils/") || lower.contains("/helpers/") || lower.contains("/lib/") {
        return PathDescription { short_name: "utility code".into(), category: "util" };
    }

    // Config directories
    if lower.contains("/config/") || lower.contains("/configs/") {
        return PathDescription { short_name: "configuration".into(), category: "config" };
    }

    // CI/CD
    if lower.contains("/.github/") || lower.contains("/.gitlab") || lower.contains("/ci/")
        || lower.contains("/.circleci")
    {
        return PathDescription { short_name: "CI/CD pipeline".into(), category: "infra" };
    }

    // Hooks
    if lower.contains("/hooks/") {
        return PathDescription { short_name: "hooks".into(), category: "hooks" };
    }

    // Store/state management
    if lower.contains("/store/") || lower.contains("/stores/") || lower.contains("/state/")
        || lower.contains("/redux/") || lower.contains("/zustand/")
    {
        return PathDescription { short_name: "state management".into(), category: "state" };
    }

    // Styles
    if ext == "css" || ext == "scss" || ext == "sass" || ext == "less" || ext == "styl" {
        return PathDescription { short_name: "styles".into(), category: "style" };
    }

    // Docs
    if ext == "md" || ext == "rst" || ext == "adoc" || lower.contains("/docs/") || lower.contains("/doc/") {
        return PathDescription { short_name: "documentation".into(), category: "docs" };
    }

    // SQL
    if ext == "sql" {
        return PathDescription { short_name: "SQL queries".into(), category: "database" };
    }

    // Proto/GraphQL
    if ext == "proto" || ext == "graphql" || ext == "gql" {
        return PathDescription { short_name: "schema definitions".into(), category: "schema" };
    }

    // Scripts
    if ext == "sh" || ext == "bash" || ext == "zsh" || ext == "fish"
        || lower.contains("/scripts/") || lower.contains("/bin/")
    {
        return PathDescription { short_name: "scripts".into(), category: "script" };
    }

    // WebSocket patterns
    if lower.contains("/ws/") || lower.contains("/websocket/") || lower.contains("/socket/") {
        return PathDescription { short_name: "WebSocket code".into(), category: "networking" };
    }

    // Auth patterns
    if lower.contains("/auth/") || lower.contains("/authentication/") || lower.contains("/login/") {
        return PathDescription { short_name: "authentication".into(), category: "auth" };
    }

    // Payment
    if lower.contains("/payment/") || lower.contains("/billing/") || lower.contains("/stripe/") {
        return PathDescription { short_name: "payment logic".into(), category: "payment" };
    }

    // Email/notifications
    if lower.contains("/email/") || lower.contains("/mail/") || lower.contains("/notification/") {
        return PathDescription { short_name: "notifications".into(), category: "notification" };
    }

    // Cron/jobs
    if lower.contains("/jobs/") || lower.contains("/cron/") || lower.contains("/workers/")
        || lower.contains("/queues/") || lower.contains("/tasks/")
    {
        return PathDescription { short_name: "background jobs".into(), category: "worker" };
    }

    // Signals directory (revenant-specific)
    if lower.contains("/signals/") {
        return PathDescription { short_name: "signal modules".into(), category: "signal" };
    }

    // Ghost directory (revenant-specific)
    if lower.contains("/ghost/") || lower.contains("/ghosts/") {
        return PathDescription { short_name: "ghost injectors".into(), category: "ghost" };
    }

    // Fallback: use the directory name + filename
    let dir_domain = infer_domain_from_path(path);
    let stem = filename.rsplit('.').last().unwrap_or(filename);
    let name = if let Some(d) = dir_domain {
        format!("{d}/{stem}")
    } else {
        stem.to_string()
    };

    PathDescription { short_name: name, category: "code" }
}

/// Try to extract a human-readable domain from the parent directory
fn infer_domain_from_path(path: &str) -> Option<String> {
    let segments: Vec<&str> = path.split('/').collect();
    // Walk backwards, skip the filename, find the first meaningful directory
    for seg in segments.iter().rev().skip(1) {
        let s = seg.to_lowercase();
        // Skip generic directories
        if matches!(s.as_str(), "src" | "lib" | "app" | "main" | "core" | "internal"
            | "pkg" | "cmd" | "build" | "dist" | "out" | "target" | "node_modules"
            | "." | ".." | "")
        {
            continue;
        }
        return Some(humanize_segment(seg));
    }
    None
}

/// Try to extract a domain from a filename (strip extension, convert casing)
fn infer_domain_from_filename(filename: &str) -> Option<String> {
    let stem = if let Some(pos) = filename.find('.') {
        &filename[..pos]
    } else {
        filename
    };
    if stem.is_empty() || stem == "index" || stem == "mod" || stem == "main" || stem == "lib" {
        return None;
    }
    Some(humanize_segment(stem))
}

/// Convert a code segment like "userAuth" or "web-socket" to "user auth" / "web socket"
fn humanize_segment(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 4);
    let mut prev_lower = false;
    for (i, ch) in s.chars().enumerate() {
        if ch == '-' || ch == '_' {
            result.push(' ');
            prev_lower = false;
        } else if ch.is_uppercase() && prev_lower {
            result.push(' ');
            result.push(ch.to_lowercase().next().unwrap_or(ch));
            prev_lower = false;
        } else {
            if i == 0 {
                result.push(ch.to_lowercase().next().unwrap_or(ch));
            } else {
                result.push(ch);
            }
            prev_lower = ch.is_lowercase();
        }
    }
    result
}

// ─── 3. Commit Message Rewriter ─────────────────────────────────────────────

/// Strip conventional commit prefixes: "feat:", "fix:", "chore:", etc.
fn strip_conventional_prefix(msg: &str) -> &str {
    let prefixes = [
        "feat:", "fix:", "chore:", "docs:", "refactor:", "test:", "style:",
        "perf:", "ci:", "build:", "revert:", "feat(", "fix(", "chore(",
        "docs(", "refactor(", "test(", "style(", "perf(", "ci(", "build(",
    ];
    let trimmed = msg.trim();
    for prefix in &prefixes {
        if trimmed.starts_with(prefix) {
            // For "feat(scope): msg" style, find the closing ):
            if prefix.ends_with('(') {
                if let Some(close) = trimmed.find("):") {
                    return trimmed[close + 2..].trim();
                }
            }
            return trimmed[prefix.len()..].trim();
        }
    }
    trimmed
}

/// Rewrite a terse commit message into flowing, readable context
fn rewrite_commit_message(raw: &str) -> String {
    let msg = strip_conventional_prefix(raw.trim());
    if msg.is_empty() {
        return String::new();
    }

    let lower = msg.to_lowercase();
    let first_word = lower.split_whitespace().next().unwrap_or("");
    let rest = msg.splitn(2, char::is_whitespace).nth(1).unwrap_or("").trim();

    // Detect verb prefix and transform
    match first_word {
        "fix" | "fixes" | "fixed" => {
            if rest.is_empty() {
                "fixing an issue".into()
            } else {
                format!("fixing {}", humanize_commit_body(rest))
            }
        }
        "add" | "adds" | "added" => {
            if rest.is_empty() {
                "adding new functionality".into()
            } else {
                format!("adding {}", humanize_commit_body(rest))
            }
        }
        "update" | "updates" | "updated" => {
            if rest.is_empty() {
                "updating the code".into()
            } else {
                format!("updating {}", humanize_commit_body(rest))
            }
        }
        "refactor" | "refactors" | "refactored" => {
            if rest.is_empty() {
                "refactoring".into()
            } else {
                format!("refactoring {}", humanize_commit_body(rest))
            }
        }
        "remove" | "removes" | "removed" | "delete" | "deletes" | "deleted" => {
            if rest.is_empty() {
                "removing unused code".into()
            } else {
                format!("removing {}", humanize_commit_body(rest))
            }
        }
        "merge" | "merges" | "merged" => {
            if rest.is_empty() {
                "merging branches".into()
            } else {
                format!("merging {}", humanize_commit_body(rest))
            }
        }
        "test" | "tests" | "tested" => {
            if rest.is_empty() {
                "writing tests".into()
            } else {
                format!("testing {}", humanize_commit_body(rest))
            }
        }
        "bump" | "bumps" | "bumped" => {
            if rest.is_empty() {
                "bumping versions".into()
            } else {
                format!("bumping {}", humanize_commit_body(rest))
            }
        }
        "revert" | "reverts" | "reverted" => {
            if rest.is_empty() {
                "reverting a change".into()
            } else {
                format!("reverting {}", humanize_commit_body(rest))
            }
        }
        "initial" | "init" => {
            if rest.is_empty() {
                "initializing the project".into()
            } else {
                format!("setting up {}", humanize_commit_body(rest))
            }
        }
        "implement" | "implements" | "implemented" => {
            if rest.is_empty() {
                "implementing new functionality".into()
            } else {
                format!("implementing {}", humanize_commit_body(rest))
            }
        }
        "move" | "moves" | "moved" | "rename" | "renames" | "renamed" => {
            if rest.is_empty() {
                "reorganizing files".into()
            } else {
                format!("reorganizing {}", humanize_commit_body(rest))
            }
        }
        "wip" => {
            if rest.is_empty() {
                "working on something in progress".into()
            } else {
                format!("working on {}", humanize_commit_body(rest))
            }
        }
        "enable" | "enables" | "enabled" | "disable" | "disables" | "disabled" => {
            format!("{} {}", first_word, humanize_commit_body(if rest.is_empty() { "a feature" } else { rest }))
        }
        "create" | "creates" | "created" => {
            if rest.is_empty() {
                "creating new files".into()
            } else {
                format!("creating {}", humanize_commit_body(rest))
            }
        }
        "improve" | "improves" | "improved" | "enhance" | "enhances" | "enhanced" => {
            if rest.is_empty() {
                "improving the codebase".into()
            } else {
                format!("improving {}", humanize_commit_body(rest))
            }
        }
        _ => {
            // No recognized verb prefix — use the message as-is, lowercased
            let mut s = msg.to_string();
            if let Some(first_char) = s.get_mut(..1) {
                first_char.make_ascii_lowercase();
            }
            s
        }
    }
}

/// Clean up the body of a commit message — expand abbreviations, add articles
fn humanize_commit_body(body: &str) -> String {
    let mut s = body.to_string();

    // Expand common abbreviations
    let abbreviations = [
        ("WS", "WebSocket"),
        ("ws", "WebSocket"),
        ("DB", "database"),
        ("db", "database"),
        ("API", "API"),
        ("api", "API"),
        ("UI", "UI"),
        ("ui", "UI"),
        ("auth", "authentication"),
        ("Auth", "authentication"),
        ("config", "configuration"),
        ("Config", "configuration"),
        ("deps", "dependencies"),
        ("Deps", "dependencies"),
        ("env", "environment"),
        ("Env", "environment"),
        ("msg", "message"),
        ("msgs", "messages"),
        ("req", "request"),
        ("reqs", "requests"),
        ("res", "response"),
        ("fn", "function"),
        ("fns", "functions"),
        ("impl", "implementation"),
        ("pkg", "package"),
        ("cmd", "command"),
        ("cmds", "commands"),
        ("err", "error"),
        ("errs", "errors"),
    ];

    // Only replace whole words
    for (abbr, expanded) in &abbreviations {
        s = replace_whole_word(&s, abbr, expanded);
    }

    // Replace colons with dashes for readability
    // "Fix WebSocket: correct subscription format" → "the WebSocket — correcting subscription format"
    if let Some(colon_pos) = s.find(':') {
        let before = s[..colon_pos].trim();
        let after = s[colon_pos + 1..].trim();
        if !after.is_empty() {
            s = format!("{before} — {after}");
        }
    }

    s
}

/// Replace whole-word occurrences only
fn replace_whole_word(text: &str, word: &str, replacement: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(pos) = remaining.find(word) {
        // Check word boundaries
        let before_ok = pos == 0 || !remaining.as_bytes()[pos - 1].is_ascii_alphanumeric();
        let after_pos = pos + word.len();
        let after_ok = after_pos >= remaining.len()
            || !remaining.as_bytes()[after_pos].is_ascii_alphanumeric();

        if before_ok && after_ok {
            result.push_str(&remaining[..pos]);
            result.push_str(replacement);
            remaining = &remaining[after_pos..];
        } else {
            result.push_str(&remaining[..pos + word.len()]);
            remaining = &remaining[pos + word.len()..];
        }
    }
    result.push_str(remaining);
    result
}

// ─── 4. File Change Clusterer ───────────────────────────────────────────────

/// Human-readable summary of changed files
fn cluster_file_changes(state: &WorkingState) -> Option<String> {
    let git = state.git.as_ref()?;
    if git.changed_files.is_empty() {
        return None;
    }

    let files = &git.changed_files;
    let total = files.len();

    // Group by category
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for f in files {
        let desc = describe_path(&f.path);
        let cat = desc.category;
        if let Some(group) = groups.iter_mut().find(|(c, _)| *c == cat) {
            group.1.push(&f.path);
        } else {
            groups.push((cat, vec![&f.path]));
        }
    }

    // Parse diff stats for numbers
    let (additions, deletions) = parse_diff_stats(&git.diff_stat);

    // Build the cluster description
    if total == 1 {
        let desc = describe_path(&files[0].path);
        let mut s = format!("You touched {}", desc.short_name);
        if additions > 0 || deletions > 0 {
            s.push_str(&format_diff_natural(additions, deletions));
        }
        Some(s)
    } else if total <= 3 {
        // Name each file
        let names: Vec<String> = files.iter().map(|f| describe_path(&f.path).short_name).collect();
        let mut s = format!("You edited {}", join_natural(&names));
        if additions > 0 || deletions > 0 {
            s.push_str(&format_diff_natural(additions, deletions));
        }
        Some(s)
    } else {
        // Cluster by category
        let mut parts: Vec<String> = Vec::new();
        // Sort groups by count descending
        groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

        for (cat, paths) in groups.iter().take(3) {
            let count = paths.len();
            let cat_name = category_plural(cat, count);
            if count == 1 {
                let desc = describe_path(paths[0]);
                parts.push(desc.short_name);
            } else {
                parts.push(format!("{count} {cat_name}"));
            }
        }

        let remainder = total - groups.iter().take(3).map(|(_, p)| p.len()).sum::<usize>();
        if remainder > 0 {
            parts.push(format!("{remainder} other file{}", if remainder == 1 { "" } else { "s" }));
        }

        let mut s = format!("{total} files changed — {}", parts.join(", "));
        if additions > 0 || deletions > 0 {
            s.push_str(&format_diff_natural(additions, deletions));
        }
        Some(s)
    }
}

/// Parse "3 files changed, 47 insertions(+), 12 deletions(-)" into (additions, deletions)
fn parse_diff_stats(stat: &str) -> (usize, usize) {
    let mut additions = 0usize;
    let mut deletions = 0usize;

    for part in stat.split(',') {
        let part = part.trim();
        if part.contains("insertion") {
            if let Some(n) = part.split_whitespace().next().and_then(|s| s.parse().ok()) {
                additions = n;
            }
        } else if part.contains("deletion") {
            if let Some(n) = part.split_whitespace().next().and_then(|s| s.parse().ok()) {
                deletions = n;
            }
        }
    }

    (additions, deletions)
}

/// Format diff stats naturally: " — 45 lines added, 12 removed"
fn format_diff_natural(additions: usize, deletions: usize) -> String {
    match (additions > 0, deletions > 0) {
        (true, true) => format!(" — {additions} line{} added, {deletions} removed",
            if additions == 1 { "" } else { "s" }),
        (true, false) => format!(" — {additions} line{} added",
            if additions == 1 { "" } else { "s" }),
        (false, true) => format!(" — {deletions} line{} removed",
            if deletions == 1 { "" } else { "s" }),
        (false, false) => String::new(),
    }
}

/// Pluralize a category name
fn category_plural(cat: &str, count: usize) -> &'static str {
    if count == 1 {
        match cat {
            "test" => "test",
            "api" => "endpoint",
            "ui" => "component",
            "service" => "service",
            "model" => "model",
            "config" => "config file",
            "style" => "stylesheet",
            "docs" => "doc",
            "database" => "migration",
            "script" => "script",
            "infra" => "infra file",
            "middleware" => "middleware",
            "util" => "utility",
            "agent" => "agent",
            "worker" => "job",
            _ => "file",
        }
    } else {
        match cat {
            "test" => "tests",
            "api" => "endpoints",
            "ui" => "components",
            "service" => "services",
            "model" => "models",
            "config" => "config files",
            "style" => "stylesheets",
            "docs" => "docs",
            "database" => "migrations",
            "script" => "scripts",
            "infra" => "infra files",
            "middleware" => "middlewares",
            "util" => "utilities",
            "agent" => "agents",
            "worker" => "jobs",
            _ => "files",
        }
    }
}

/// Join items with commas and "and": ["a", "b", "c"] → "a, b, and c"
fn join_natural(items: &[String]) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        2 => format!("{} and {}", items[0], items[1]),
        _ => {
            let (last, rest) = items.split_last().unwrap();
            format!("{}, and {last}", rest.join(", "))
        }
    }
}

// ─── 5. Next Step Predictor ─────────────────────────────────────────────────

/// Predict what the user should do next, deterministically
fn predict_next_step(state: &WorkingState) -> String {
    let commands = state.recent_commands();
    let git = state.git.as_ref();

    // Rule 1: Test command was last + common failure indicators
    if let Some(last_cmd) = commands.first() {
        let cmd = last_cmd.to_lowercase();
        if is_test_command(&cmd) {
            // We can't know exit codes directly, but if they have unstaged changes
            // after running tests, they might be fixing failures
            if git.map_or(false, |g| g.modified > 0) {
                return "Run tests again to see if your changes fix the failures.".into();
            }
            return "Tests were running — check if they pass and commit if green.".into();
        }
    }

    // Rule 2: Staged changes with nothing else pending
    if let Some(g) = git {
        if g.staged > 0 && g.modified == 0 && g.untracked == 0 {
            return "You've got staged changes ready — commit them.".into();
        }

        // Rule 3: Staged + unstaged → tests in changed files?
        if g.staged > 0 && g.modified > 0 {
            let has_test_files = g.changed_files.iter().any(|f| {
                let lower = f.path.to_lowercase();
                lower.contains("test") || lower.contains("spec")
            });
            if has_test_files {
                return "Run the tests before committing your staged changes.".into();
            }
        }

        // Rule 4: Unstaged changes only, test files present
        if g.staged == 0 && g.modified > 0 {
            let has_tests = g.changed_files.iter().any(|f| {
                let lower = f.path.to_lowercase();
                lower.contains("test") || lower.contains("spec")
            });
            if has_tests {
                return "Run tests, then stage and commit.".into();
            }
        }

        // Rule 5: Many files changed, no commits recently
        if g.changed_files.len() > 8 {
            return "That's a lot of changes — consider breaking them into smaller commits.".into();
        }

        // Rule 6: New file created that might need wiring
        let new_files: Vec<&str> = g.changed_files.iter()
            .filter(|f| matches!(f.status, ChangeStatus::Added | ChangeStatus::Untracked))
            .map(|f| f.path.as_str())
            .collect();
        if new_files.len() == 1 {
            let desc = describe_path(new_files[0]);
            return format!("Wire {} into the module and make sure it's imported.", desc.short_name);
        }

        // Rule 7: Package/dependency file changed
        if g.changed_files.iter().any(|f| {
            let name = f.path.rsplit('/').next().unwrap_or("");
            matches!(name, "package.json" | "package-lock.json" | "yarn.lock"
                | "pnpm-lock.yaml" | "Cargo.toml" | "Cargo.lock" | "go.mod"
                | "go.sum" | "Pipfile" | "requirements.txt" | "Gemfile"
                | "poetry.lock" | "pyproject.toml")
        }) {
            return "Dependencies changed — run install to sync.".into();
        }

        // Rule 8: Migration file created
        if g.changed_files.iter().any(|f| {
            let lower = f.path.to_lowercase();
            (lower.contains("migration") || lower.contains("migrate"))
                && matches!(f.status, ChangeStatus::Added | ChangeStatus::Untracked)
        }) {
            return "You've got a new migration — run it against the database.".into();
        }

        // Rule 9: On feature branch, changes look complete
        let branch = &g.branch;
        if branch != "main" && branch != "master" && branch != "dev"
            && g.staged == 0 && g.modified == 0 && g.untracked == 0
        {
            return "Branch looks clean — might be ready for a PR.".into();
        }

        // Rule 10: Unstaged changes, no tests
        if g.staged == 0 && g.modified > 0 {
            return "Stage your changes and commit when you're ready.".into();
        }
    }

    // Rule 11: Terminal commands give hints
    for cmd in commands.iter().take(3) {
        let cmd_lower = cmd.to_lowercase();

        // Build commands
        if cmd_lower.contains("build") && (cmd_lower.contains("error") || cmd_lower.contains("fail")) {
            return "Fix the build errors and try again.".into();
        }

        // Install commands
        if cmd_lower.starts_with("npm install") || cmd_lower.starts_with("yarn add")
            || cmd_lower.starts_with("cargo add") || cmd_lower.starts_with("pip install")
        {
            return "Dependencies updated — continue with your changes.".into();
        }

        // Git stash
        if cmd_lower.starts_with("git stash") && !cmd_lower.contains("pop") {
            return "Don't forget to pop your stash when you're ready.".into();
        }

        // Git add
        if cmd_lower.starts_with("git add") {
            return "Files are staged — commit when ready.".into();
        }

        // Deploy commands
        if cmd_lower.contains("deploy") || cmd_lower.contains("kubectl apply")
            || cmd_lower.contains("fly deploy")
        {
            return "Verify the deployment landed correctly.".into();
        }

        // Dev server
        if cmd_lower.contains("dev") && (cmd_lower.contains("run") || cmd_lower.contains("start")) {
            return "Dev server was running — continue building.".into();
        }
    }

    // Rule 12: Active file fallback
    if let Some(ref ed) = state.editor {
        if let Some(ref active) = ed.active_file {
            let desc = describe_path(active);
            return format!("Continue from {}.", desc.short_name);
        }
    }

    "Pick up where you left off.".into()
}

/// Check if a command is a test command
fn is_test_command(cmd: &str) -> bool {
    let first = cmd.split_whitespace().next().unwrap_or("");
    first == "pytest" || first == "jest" || first == "vitest" || first == "rspec"
        || first == "mocha" || cmd.starts_with("cargo test")
        || cmd.starts_with("npm test") || cmd.starts_with("yarn test")
        || cmd.starts_with("go test") || cmd.starts_with("npm run test")
        || cmd.starts_with("npx jest") || cmd.starts_with("npx vitest")
}

// ─── 6. Temporal Framing ────────────────────────────────────────────────────

/// Produce a time-aware opening phrase
fn temporal_frame(state: &WorkingState) -> &'static str {
    let now = Utc::now();
    let elapsed = now.signed_duration_since(state.timestamp);
    let minutes = elapsed.num_minutes();

    if minutes < 30 {
        pick_variant(&["Just now you were", "Moments ago you were", "You were just"])
    } else if minutes < 120 {
        pick_variant(&["A little while ago you were", "Not long ago you were", "Recently you were"])
    } else if minutes < 480 {
        pick_variant(&["Earlier today you were", "Earlier you were", "A few hours ago you were"])
    } else if minutes < 1440 {
        pick_variant(&["Yesterday you were", "Last session you were"])
    } else if minutes < 4320 {
        pick_variant(&["A few days ago you were", "A couple days ago you were"])
    } else {
        // > 3 days — use weekday
        let weekday = state.timestamp.weekday();
        match weekday {
            chrono::Weekday::Mon => "Last Monday you were",
            chrono::Weekday::Tue => "Last Tuesday you were",
            chrono::Weekday::Wed => "Last Wednesday you were",
            chrono::Weekday::Thu => "Last Thursday you were",
            chrono::Weekday::Fri => "Last Friday you were",
            chrono::Weekday::Sat => "Last Saturday you were",
            chrono::Weekday::Sun => "Last Sunday you were",
        }
    }
}

/// Simple deterministic variant picker (seeded by current minute to vary)
fn pick_variant(options: &[&'static str]) -> &'static str {
    // Use the current second as a cheap seed for variety
    let now = Utc::now();
    let idx = now.timestamp().unsigned_abs() as usize % options.len();
    options[idx]
}

// ─── 7. Template Library ────────────────────────────────────────────────────

/// All the sentence templates. The card ID seeds which one gets picked.
const TEMPLATES: &[&str] = &[
    // Standard: temporal + intent + topic. file cluster. next step.
    "{temporal} {intent} {topic}. {cluster}.",
    // Left-off style
    "You left off {intent} {topic}. {cluster}.",
    // Commit-forward
    "Your last change was {commit}. {cluster}.",
    // Cluster-first
    "{cluster}. You were {intent} {topic}.",
    // Short and punchy
    "{temporal} {intent} {topic} in {project}.",
    // Commit + doing
    "{temporal} {intent} {topic} — {commit}.",
    // Branch-aware
    "On `{branch}`, you were {intent} {topic}. {cluster}.",
    // Doing + project context
    "In {project}, you were {intent} {topic}. {cluster}.",
    // File-first, intent second
    "{cluster}. You were {past} {topic}.",
    // Minimal
    "{temporal} {intent} {topic}.",
    // Active voice, doing + commit
    "You were {intent} {topic}. Your last commit: {commit}.",
    // Project-scoped
    "{temporal} {intent} {topic} in {project}. {cluster}.",
    // Change-first
    "Last change: {commit}. {cluster}.",
    // Narrative
    "You were in the middle of {intent} {topic}. {cluster}.",
    // Branch-forward
    "You're on `{branch}`, {intent} {topic}. {cluster}.",
    // Simple past with commit
    "You were {intent} {topic} — last commit was {commit}.",
    // Cluster-led minimal
    "{cluster}. Last commit: {commit}.",
    // Project + branch combo
    "In {project} on `{branch}`, you were {intent} {topic}.",
    // Direct address
    "You left {project} while {intent} {topic}. {cluster}.",
    // Time + cluster
    "{temporal} making changes. {cluster}.",
    // With file focus
    "{temporal} focused on {active_file}, {intent} {topic}.",
    // Clean narrative
    "You were {past} {topic} — {cluster}.",
    // Commit as context, cluster as evidence
    "{commit}. {cluster}.",
    // Short branch
    "On `{branch}`: {intent} {topic}. {cluster}.",
    // Exploratory
    "{temporal} {intent} {topic}. You had {file_count} files open.",
    // Work session
    "Last session you were {intent} {topic}. {cluster}.",
    // Simple + project
    "{temporal} {intent} {topic} ({project}).",
    // Two-sentence punch
    "You were {intent} {topic}. {cluster}.",
    // Active file centric
    "{temporal} working on {active_file}. {cluster}.",
    // Minimal commit
    "Last commit was {commit}. You were {intent} {topic}.",
];

/// Card depth — controls how much context to include
#[derive(Debug, Clone, Copy, PartialEq)]
enum CardDepth {
    /// < 30min absence: 1 sentence, minimal detail
    Terse,
    /// 30min-4hr: 2 sentences, cluster + next step
    Standard,
    /// > 4hr: full context, all signals
    Rich,
}

/// Templates classified by depth
const TERSE_TEMPLATES: &[usize] = &[9, 4, 26];  // Minimal, Short+project, Simple+project
const RICH_TEMPLATES: &[usize] = &[0, 6, 7, 11, 13, 17, 18, 24, 10]; // Multi-sentence, narrative

/// Select a template based on a seed, available data, and depth
fn select_template(card_id: &str, has_commit: bool, has_cluster: bool, has_branch: bool, depth: CardDepth) -> usize {
    // Use card ID as a deterministic seed
    let seed: usize = card_id.bytes().map(|b| b as usize).sum();

    // Filter templates that we can fully populate
    let valid: Vec<usize> = TEMPLATES.iter().enumerate()
        .filter(|(_, t)| {
            let needs_commit = t.contains("{commit}");
            let needs_cluster = t.contains("{cluster}");
            let needs_branch = t.contains("{branch}");

            (!needs_commit || has_commit)
                && (!needs_cluster || has_cluster)
                && (!needs_branch || has_branch)
        })
        .map(|(i, _)| i)
        .collect();

    if valid.is_empty() {
        return 0;
    }

    // Filter by depth preference
    let depth_filtered: Vec<usize> = match depth {
        CardDepth::Terse => valid.iter()
            .copied()
            .filter(|i| TERSE_TEMPLATES.contains(i))
            .collect(),
        CardDepth::Rich => valid.iter()
            .copied()
            .filter(|i| RICH_TEMPLATES.contains(i))
            .collect(),
        CardDepth::Standard => valid.clone(),
    };

    // Fall back to all valid templates if depth filter yields nothing
    let pool = if depth_filtered.is_empty() { &valid } else { &depth_filtered };
    pool[seed % pool.len()]
}

/// Fill a template with actual values
fn fill_template(
    template_idx: usize,
    temporal: &str,
    intent_verb: &str,
    past_verb: &str,
    topic: &str,
    commit_context: &str,
    cluster: &str,
    branch: &str,
    project: &str,
    active_file: &str,
    file_count: usize,
) -> String {
    let template = TEMPLATES.get(template_idx).unwrap_or(&TEMPLATES[0]);

    let result = template
        .replace("{temporal}", temporal)
        .replace("{intent}", intent_verb)
        .replace("{past}", past_verb)
        .replace("{topic}", topic)
        .replace("{commit}", commit_context)
        .replace("{cluster}", cluster)
        .replace("{branch}", branch)
        .replace("{project}", project)
        .replace("{active_file}", active_file)
        .replace("{file_count}", &file_count.to_string());

    // Clean up double spaces, trailing periods after periods
    let result = result.replace("  ", " ");
    let result = result.replace(". .", ".");
    let result = result.replace("..", ".");
    result.trim().to_string()
}

// ─── Topic Extraction ───────────────────────────────────────────────────────

/// Extract the main topic from all available signals
fn extract_topic(state: &WorkingState) -> String {
    // Best signal: commit message tells us what they were working on
    if let Some(ref git) = state.git {
        if let Some(commit) = git.recent_commits.first() {
            let rewritten = rewrite_commit_message(&commit.message);
            if !rewritten.is_empty() {
                // The rewritten message already starts with a gerund like "fixing X"
                // We need just the object/topic part for "{intent} {topic}" templates
                // But sometimes the intent IS the topic (when commit = full context)
                return extract_object_from_rewrite(&rewritten);
            }
        }

        // Next: branch name can give a topic
        let branch = &git.branch;
        if branch != "main" && branch != "master" && branch != "dev" && branch != "develop" {
            // Strip prefix like "fix/" or "feature/"
            let topic_part = branch
                .split('/')
                .last()
                .unwrap_or(branch);
            let humanized = humanize_segment(topic_part);
            if !humanized.is_empty() {
                return humanized;
            }
        }

        // File-based topic: what domain are the changes in?
        if !git.changed_files.is_empty() {
            let desc = describe_path(&git.changed_files[0].path);
            return desc.short_name;
        }
    }

    // Active file
    if let Some(ref ed) = state.editor {
        if let Some(ref active) = ed.active_file {
            let desc = describe_path(active);
            return desc.short_name;
        }
    }

    // Fallback
    state.project_name.clone()
}

/// Extract the object/topic from a rewritten commit message
/// "fixing WebSocket reconnection" → "WebSocket reconnection"
/// "adding user authentication" → "user authentication"
fn extract_object_from_rewrite(rewrite: &str) -> String {
    // The rewrite starts with a gerund verb — try to strip it
    let gerunds = [
        "fixing ", "adding ", "updating ", "refactoring ", "removing ",
        "merging ", "testing ", "bumping ", "reverting ", "setting up ",
        "implementing ", "reorganizing ", "working on ", "creating ",
        "improving ", "enabling ", "disabling ",
    ];

    for prefix in &gerunds {
        if rewrite.starts_with(prefix) {
            let rest = &rewrite[prefix.len()..];
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }

    // If no gerund prefix matched, return as-is
    rewrite.to_string()
}

// ─── Main Entry Point ───────────────────────────────────────────────────────

/// Rule-based compression — no LLM required
///
/// Produces LLM-quality context cards by analyzing signal patterns through
/// seven engines: intent classification, path semantics, commit rewriting,
/// file clustering, next-step prediction, temporal framing, and template selection.
///
/// Card depth scales with absence duration:
/// - < 30min: terse (1 sentence, minimal detail)
/// - 30min-4hr: standard (2 sentences, cluster + next step)
/// - > 4hr: rich (full context, commit history, file details, strong next step)
pub fn rule_based_compress(state: &WorkingState) -> ContextCard {
    let card_id = Uuid::new_v4().to_string();

    // 1. Classify intent
    let intent = classify_intent(state);

    // 2. Extract topic
    let topic = extract_topic(state);

    // 3. Rewrite commit message
    let commit_context = state.git.as_ref()
        .and_then(|g| g.recent_commits.first())
        .map(|c| rewrite_commit_message(&c.message))
        .unwrap_or_default();

    // 4. Cluster file changes
    let cluster = cluster_file_changes(state).unwrap_or_default();

    // 5. Predict next step
    let next_step = predict_next_step(state);

    // 6. Temporal framing
    let temporal = temporal_frame(state);

    // 7. Template selection and filling
    let branch = state.git.as_ref()
        .map(|g| g.branch.as_str())
        .unwrap_or("main");

    let active_file = state.editor.as_ref()
        .and_then(|e| e.active_file.as_deref())
        .map(|f| describe_path(f).short_name)
        .unwrap_or_default();

    let file_count = state.editor.as_ref()
        .map(|e| e.open_files.len())
        .unwrap_or(0);

    let has_commit = !commit_context.is_empty();
    let has_cluster = !cluster.is_empty();
    let has_branch = state.git.as_ref()
        .map(|g| g.branch != "main" && g.branch != "master")
        .unwrap_or(false);

    // Determine card depth based on absence duration
    let absence_minutes = Utc::now().signed_duration_since(state.timestamp).num_minutes();
    let depth = if absence_minutes < 30 {
        CardDepth::Terse
    } else if absence_minutes < 240 {
        CardDepth::Standard
    } else {
        CardDepth::Rich
    };

    let template_idx = select_template(&card_id, has_commit, has_cluster, has_branch, depth);

    let summary = fill_template(
        template_idx,
        temporal,
        intent.verb(),
        intent.past(),
        &topic,
        &commit_context,
        &cluster,
        branch,
        &state.project_name,
        &active_file,
        file_count,
    );

    // Guard: ensure summary is always meaningful
    let summary = {
        let s = summary.trim();
        let has_project = s.to_lowercase().contains(&state.project_name.to_lowercase());
        let has_substance = s.len() > 40 && !s.ends_with("working on.") && !s.ends_with("working on");

        if has_substance && has_project {
            s.to_string()
        } else if has_substance && !has_project {
            // Good content but missing project name
            format!("In {}: {}", state.project_name, s)
        } else {
            // Thin summary — build a reliable one from raw signals
            let mut parts = vec![format!("{} in {}", temporal, state.project_name)];
            if !commit_context.is_empty() {
                parts.push(format!("you were {} {}", intent.verb(), commit_context));
            } else if !active_file.is_empty() {
                parts.push(format!("you were editing {}", active_file));
            }
            if !cluster.is_empty() {
                parts.push(cluster.clone());
            }
            parts.join(". ")
        }
    };

    // Ensure summary doesn't end mid-sentence
    let summary = clean_summary(&summary);

    // For rich depth, add extra commit context to next_step if available
    let next_step = if depth == CardDepth::Rich && has_commit {
        let recent_commits: Vec<String> = state.git.as_ref()
            .map(|g| g.recent_commits.iter().take(3)
                .map(|c| rewrite_commit_message(&c.message))
                .filter(|m| !m.is_empty())
                .collect())
            .unwrap_or_default();
        if recent_commits.len() > 1 {
            format!("{} Recent trail: {}.", next_step.trim_end_matches('.'), recent_commits[1..].join(", then "))
        } else {
            next_step
        }
    } else {
        next_step
    };

    // Scale TTL with absence: quick return → 60s, medium → 120s, long → 180s
    let ttl_seconds = match depth {
        CardDepth::Terse => 60,
        CardDepth::Standard => 120,
        CardDepth::Rich => 180,
    };

    ContextCard {
        id: card_id,
        project_dir: state.project_dir.clone(),
        project_name: state.project_name.clone(),
        summary,
        next_step,
        created_at: Utc::now(),
        signals_json: serde_json::to_string(state).unwrap_or_default(),
        ttl_seconds,
    }
}

/// Final cleanup pass on the summary
fn clean_summary(s: &str) -> String {
    let mut result = s.trim().to_string();

    // Ensure it ends with a period
    if !result.ends_with('.') && !result.ends_with('!') && !result.ends_with('?') {
        result.push('.');
    }

    // Collapse any weird whitespace artifacts
    while result.contains("  ") {
        result = result.replace("  ", " ");
    }

    // Remove orphaned punctuation
    result = result.replace(" .", ".");
    result = result.replace(" ,", ",");
    result = result.replace("()", "");
    result = result.replace("..", ".");
    result = result.replace("  ", " ");

    result
}

// ─── LLM-based compression (unchanged) ─────────────────────────────────────

/// LLM-based compression — sends signals to a model for natural language synthesis
pub async fn llm_compress(state: &WorkingState, config: &LlmConfig) -> Result<ContextCard> {
    let prompt = build_llm_prompt(state);

    let response = match config.provider.as_str() {
        "ollama" => call_ollama(&config.endpoint, &config.model, &prompt, config).await?,
        "claude" => call_claude_api(&config.api_key, &config.model, &prompt, config).await?,
        "openai" => call_openai_api(&config.api_key, &config.model, &prompt, config).await?,
        other => bail!("unknown LLM provider: {other}"),
    };

    // Parse the response into summary and next_step
    let (summary, next_step) = parse_llm_response(&response, state);

    Ok(ContextCard {
        id: Uuid::new_v4().to_string(),
        project_dir: state.project_dir.clone(),
        project_name: state.project_name.clone(),
        summary,
        next_step,
        created_at: Utc::now(),
        signals_json: serde_json::to_string(state).unwrap_or_default(),
        ttl_seconds: 300,
    })
}

fn build_llm_prompt(state: &WorkingState) -> String {
    let template = include_str!("../../models/compress.txt");

    // Build the signals section
    let mut signals = String::new();

    if let Some(ref git) = state.git {
        signals.push_str(&format!("Git branch: {}\n", git.branch));
        if !git.changed_files.is_empty() {
            signals.push_str("Changed files:\n");
            for f in &git.changed_files {
                signals.push_str(&format!("  {:?}: {}\n", f.status, f.path));
            }
        }
        if !git.diff_stat.is_empty() {
            signals.push_str(&format!("Diff stats: {}\n", git.diff_stat));
        }
        if let Some(commit) = git.recent_commits.first() {
            signals.push_str(&format!(
                "Last commit: {} ({})\n",
                commit.message, commit.hash
            ));
        }
    }

    if let Some(ref ed) = state.editor {
        if let Some(ref active) = ed.active_file {
            signals.push_str(&format!("Active file: {}\n", active));
        }
        if ed.open_files.len() > 1 {
            signals.push_str(&format!(
                "Open files: {}\n",
                ed.open_files
                    .iter()
                    .take(5)
                    .map(|f| f.path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    let commands = state.recent_commands();
    if !commands.is_empty() {
        signals.push_str("Recent terminal commands:\n");
        for cmd in commands.iter().take(10) {
            signals.push_str(&format!("  $ {}\n", cmd));
        }
    }

    signals.push_str(&format!("Project: {} ({})\n", state.project_name, state.project_dir));

    template.replace("{{SIGNALS}}", &signals)
}

/// Call Ollama's local API
async fn call_ollama(
    endpoint: &str,
    model: &str,
    prompt: &str,
    config: &LlmConfig,
) -> Result<String> {
    #[cfg(feature = "llm-api")]
    {
        let client = reqwest::Client::new();
        let body = serde_json::json!({
            "model": model,
            "prompt": prompt,
            "stream": false,
            "options": {
                "temperature": config.temperature,
                "num_predict": config.max_tokens,
            }
        });

        let resp = client
            .post(endpoint)
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await?;

        let json: serde_json::Value = resp.json().await?;
        Ok(json["response"]
            .as_str()
            .unwrap_or("Failed to generate context card")
            .to_string())
    }

    #[cfg(not(feature = "llm-api"))]
    {
        let _ = (endpoint, model, prompt, config);
        bail!("LLM API support not compiled in. Build with --features llm-api")
    }
}

/// Call Claude API
async fn call_claude_api(
    api_key: &str,
    model: &str,
    prompt: &str,
    config: &LlmConfig,
) -> Result<String> {
    #[cfg(feature = "llm-api")]
    {
        if api_key.is_empty() {
            bail!("Claude API key not configured");
        }

        let client = reqwest::Client::new();
        let body = serde_json::json!({
            "model": model,
            "max_tokens": config.max_tokens,
            "messages": [{"role": "user", "content": prompt}]
        });

        let resp = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await?;

        let json: serde_json::Value = resp.json().await?;
        Ok(json["content"][0]["text"]
            .as_str()
            .unwrap_or("Failed to generate context card")
            .to_string())
    }

    #[cfg(not(feature = "llm-api"))]
    {
        let _ = (api_key, model, prompt, config);
        bail!("LLM API support not compiled in. Build with --features llm-api")
    }
}

/// Call OpenAI-compatible API
async fn call_openai_api(
    api_key: &str,
    model: &str,
    prompt: &str,
    config: &LlmConfig,
) -> Result<String> {
    #[cfg(feature = "llm-api")]
    {
        if api_key.is_empty() {
            bail!("OpenAI API key not configured");
        }

        let client = reqwest::Client::new();
        let body = serde_json::json!({
            "model": model,
            "max_tokens": config.max_tokens,
            "temperature": config.temperature,
            "messages": [{"role": "user", "content": prompt}]
        });

        let resp = client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await?;

        let json: serde_json::Value = resp.json().await?;
        Ok(json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("Failed to generate context card")
            .to_string())
    }

    #[cfg(not(feature = "llm-api"))]
    {
        let _ = (api_key, model, prompt, config);
        bail!("LLM API support not compiled in. Build with --features llm-api")
    }
}

/// Parse LLM response into (summary, next_step)
fn parse_llm_response(response: &str, state: &WorkingState) -> (String, String) {
    let lines: Vec<&str> = response.lines().filter(|l| !l.trim().is_empty()).collect();

    if lines.len() >= 2 {
        let summary = lines[0].trim().to_string();
        let next_step = lines[1..].join(" ").trim().to_string();
        (summary, next_step)
    } else if lines.len() == 1 {
        (lines[0].trim().to_string(), String::new())
    } else {
        // Fallback to rule-based
        let card = rule_based_compress(state);
        (card.summary, card.next_step)
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_humanize_segment() {
        assert_eq!(humanize_segment("userAuth"), "user auth");
        assert_eq!(humanize_segment("web-socket"), "web socket");
        assert_eq!(humanize_segment("WebSocketClient"), "web socket client");
        assert_eq!(humanize_segment("simple"), "simple");
    }

    #[test]
    fn test_strip_conventional_prefix() {
        assert_eq!(strip_conventional_prefix("feat: add login"), "add login");
        assert_eq!(strip_conventional_prefix("fix(auth): token expiry"), "token expiry");
        assert_eq!(strip_conventional_prefix("just a message"), "just a message");
    }

    #[test]
    fn test_rewrite_commit_message() {
        assert_eq!(rewrite_commit_message("Fix WS reconnect"), "fixing WebSocket reconnect");
        assert_eq!(rewrite_commit_message("Add user auth"), "adding user authentication");
        assert_eq!(rewrite_commit_message("feat: add login page"), "adding login page");
        assert_eq!(rewrite_commit_message("Refactor payment flow"), "refactoring payment flow");
        assert_eq!(rewrite_commit_message("Remove dead code"), "removing dead code");
    }

    #[test]
    fn test_describe_path_components() {
        let desc = describe_path("src/components/UserProfile.tsx");
        assert_eq!(desc.category, "ui");
        assert!(desc.short_name.contains("user profile"));
    }

    #[test]
    fn test_describe_path_tests() {
        let desc = describe_path("src/api/__tests__/auth.test.ts");
        assert_eq!(desc.category, "test");
    }

    #[test]
    fn test_describe_path_api() {
        let desc = describe_path("src/api/payments.ts");
        assert_eq!(desc.category, "api");
        assert!(desc.short_name.contains("payment"));
    }

    #[test]
    fn test_describe_path_migration() {
        let desc = describe_path("db/migrations/001_create_users.sql");
        assert_eq!(desc.category, "database");
    }

    #[test]
    fn test_join_natural() {
        assert_eq!(join_natural(&[]), "");
        assert_eq!(join_natural(&["a".into()]), "a");
        assert_eq!(join_natural(&["a".into(), "b".into()]), "a and b");
        assert_eq!(join_natural(&["a".into(), "b".into(), "c".into()]), "a, b, and c");
    }

    #[test]
    fn test_replace_whole_word() {
        assert_eq!(replace_whole_word("fix the WS client", "WS", "WebSocket"), "fix the WebSocket client");
        assert_eq!(replace_whole_word("update WSHandler", "WS", "WebSocket"), "update WSHandler");
    }

    #[test]
    fn test_parse_diff_stats() {
        let (a, d) = parse_diff_stats("3 files changed, 47 insertions(+), 12 deletions(-)");
        assert_eq!(a, 47);
        assert_eq!(d, 12);
    }

    #[test]
    fn test_format_diff_natural() {
        assert_eq!(format_diff_natural(45, 12), " — 45 lines added, 12 removed");
        assert_eq!(format_diff_natural(1, 0), " — 1 line added");
        assert_eq!(format_diff_natural(0, 0), "");
    }

    #[test]
    fn test_clean_summary() {
        assert_eq!(clean_summary("You were fixing things"), "You were fixing things.");
        assert_eq!(clean_summary("Double  spaces  here."), "Double spaces here.");
        assert_eq!(clean_summary("Trailing period.."), "Trailing period.");
    }
}
