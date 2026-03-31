//! Context compression — WorkingState → ContextCard
//!
//! Two modes:
//! 1. Rule-based: deterministic template filling from signals (no LLM needed)
//! 2. LLM-based: sends signals to a local or remote LLM for natural language compression
//!
//! The rule-based mode is always available. LLM mode produces more natural,
//! specific context cards but requires Ollama or an API key.

use anyhow::{bail, Result};
use chrono::Utc;
use uuid::Uuid;

use crate::config::LlmConfig;
use crate::snapshot::WorkingState;
use crate::store::ContextCard;

/// Rule-based compression — no LLM required
///
/// Produces context cards by analyzing signal patterns:
/// - Recent git changes → "You were modifying X"
/// - Recent terminal commands → "You were running Y"
/// - Active file + language → "You were working in Z"
/// - Git diff stats → "You had N files with changes"
pub fn rule_based_compress(state: &WorkingState) -> ContextCard {
    let mut fragments: Vec<String> = Vec::new();
    let mut next_step: Option<String> = None;

    // 1. What were they doing? (from git + editor + terminal)
    if let Some(ref git) = state.git {
        // Branch context
        let branch = &git.branch;
        if branch != "main" && branch != "master" {
            fragments.push(format!("on branch `{branch}`"));
        }

        // Changed files give the strongest signal about intent
        if !git.changed_files.is_empty() {
            let file_names: Vec<&str> = git
                .changed_files
                .iter()
                .take(3)
                .map(|f| {
                    f.path
                        .rsplit('/')
                        .next()
                        .unwrap_or(&f.path)
                })
                .collect();

            let files_str = file_names.join(", ");
            if git.changed_files.len() > 3 {
                fragments.push(format!(
                    "editing {files_str} and {} other files",
                    git.changed_files.len() - 3
                ));
            } else {
                fragments.push(format!("editing {files_str}"));
            }
        }

        // Diff stats give scale
        if !git.diff_stat.is_empty() {
            fragments.push(format!("({})", git.diff_stat));
        }

        // Recent commits reveal trajectory
        if let Some(last_commit) = git.recent_commits.first() {
            fragments.push(format!(
                "last commit: \"{}\"",
                truncate(&last_commit.message, 60)
            ));
        }
    }

    // 2. Terminal commands reveal process
    let commands = state.recent_commands();
    if !commands.is_empty() {
        let interesting: Vec<&str> = commands
            .iter()
            .filter(|c| is_interesting_command(c))
            .take(3)
            .copied()
            .collect();

        if !interesting.is_empty() {
            fragments.push(format!(
                "recently ran: {}",
                interesting
                    .iter()
                    .map(|c| format!("`{}`", truncate(c, 40)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        // Infer next step from terminal patterns
        next_step = infer_next_step(&commands);
    }

    // 3. Active file reveals focus
    if let Some(ref ed) = state.editor {
        if let Some(ref active) = ed.active_file {
            if let Some(ref lang) = ed.active_language {
                fragments.push(format!("focused on `{active}` ({lang})"));
            } else {
                fragments.push(format!("focused on `{active}`"));
            }
        }
    }

    // Compose the summary
    let summary = if fragments.is_empty() {
        format!(
            "You were working in {}. No specific signals captured.",
            state.project_name
        )
    } else {
        let doing = fragments.join(". ");
        let mut s = format!("You were working in {}: {}", state.project_name, doing);

        if let Some(ref next) = next_step {
            s.push_str(&format!(". Next: {next}"));
        }

        s
    };

    ContextCard {
        id: Uuid::new_v4().to_string(),
        project_dir: state.project_dir.clone(),
        project_name: state.project_name.clone(),
        summary,
        next_step: next_step.unwrap_or_default(),
        created_at: Utc::now(),
        signals_json: serde_json::to_string(state).unwrap_or_default(),
        ttl_seconds: 300, // 5 minutes default
    }
}

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
    bail!("LLM API support not compiled in. Build with --features llm-api")
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
    bail!("LLM API support not compiled in. Build with --features llm-api")
}

/// Parse LLM response into (summary, next_step)
fn parse_llm_response(response: &str, state: &WorkingState) -> (String, String) {
    let lines: Vec<&str> = response.lines().filter(|l| !l.trim().is_empty()).collect();

    if lines.len() >= 2 {
        // Expect: first line = what you were doing, second line = next step
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

/// Check if a terminal command is "interesting" (not just navigation/listing)
fn is_interesting_command(cmd: &str) -> bool {
    let boring = [
        "ls", "ll", "la", "pwd", "clear", "exit", "cd", "echo", "which", "whoami",
        "date", "cal", "history",
    ];
    let first_word = cmd.split_whitespace().next().unwrap_or("");
    !boring.contains(&first_word)
}

/// Infer what the user should do next from their command history
fn infer_next_step(commands: &[&str]) -> Option<String> {
    for cmd in commands.iter().rev() {
        let cmd = cmd.trim();

        // Test failures suggest re-running tests after fixing
        if cmd.contains("test") && cmd.contains("fail") {
            return Some("fix the failing test and re-run".to_string());
        }

        // Build errors suggest fixing compilation
        if cmd.starts_with("cargo build")
            || cmd.starts_with("npm run build")
            || cmd.starts_with("make")
        {
            return Some(format!("check if `{cmd}` succeeds now", cmd = truncate(cmd, 40)));
        }

        // Running a specific test suggests iterating on it
        if cmd.contains("test ") || cmd.starts_with("pytest") || cmd.starts_with("cargo test") {
            return Some("continue iterating on tests".to_string());
        }

        // Git operations suggest continuing the workflow
        if cmd.starts_with("git add") || cmd.starts_with("git stage") {
            return Some("commit the staged changes".to_string());
        }
        if cmd.starts_with("git stash") {
            return Some("pop the stash when ready to continue".to_string());
        }

        // Server/dev running suggests continuing development
        if cmd.contains("dev") || cmd.contains("serve") || cmd.contains("start") {
            return Some("continue development with the dev server".to_string());
        }
    }

    None
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}
