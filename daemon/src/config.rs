//! Configuration loading and types for REVENANT
//!
//! Reads from ~/.config/revenant/config.toml, falls back to defaults.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevenantConfig {
    pub daemon: DaemonConfig,
    pub signals: SignalsConfig,
    pub compressor: CompressorConfig,
    pub ghosts: GhostsConfig,
    pub storage: StorageConfig,
    pub projects: ProjectsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_poll_interval")]
    pub poll_interval: u64,
    #[serde(default = "default_absence_threshold")]
    pub absence_threshold_minutes: u64,
    #[serde(default = "default_ghost_ttl")]
    pub ghost_ttl_minutes: u64,
    #[serde(default = "default_prune_days")]
    pub prune_after_days: u64,
    #[serde(default = "default_pid_file")]
    pub pid_file: String,
}

fn default_poll_interval() -> u64 { 5 }
fn default_absence_threshold() -> u64 { 15 }
fn default_ghost_ttl() -> u64 { 2 }
fn default_prune_days() -> u64 { 30 }
fn default_pid_file() -> String { "~/.revenant/revenant.pid".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalsConfig {
    #[serde(default = "default_true")]
    pub git: bool,
    #[serde(default = "default_true")]
    pub editor_enabled: bool,
    #[serde(default = "default_true")]
    pub terminal_enabled: bool,
    #[serde(default)]
    pub clipboard: bool,
    #[serde(default)]
    pub browser: bool,
    #[serde(default = "default_ignore_patterns")]
    pub ignore_patterns: Vec<String>,
    #[serde(default, rename = "terminal")]
    pub terminal_config: TerminalSignalConfig,
    #[serde(default, rename = "editor")]
    pub editor_config: EditorSignalConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TerminalSignalConfig {
    #[serde(default = "default_history_files")]
    pub history_files: Vec<String>,
    #[serde(default = "default_recent_count")]
    pub recent_command_count: usize,
}

fn default_history_files() -> Vec<String> {
    vec!["~/.zsh_history".into(), "~/.bash_history".into()]
}
fn default_recent_count() -> usize { 20 }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EditorSignalConfig {
    #[serde(default = "default_vscode_state")]
    pub vscode_state_dir: String,
}

fn default_vscode_state() -> String { "~/.revenant/vscode-state".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressorConfig {
    #[serde(default = "default_compressor_mode")]
    pub mode: String,
    #[serde(default)]
    pub llm: LlmConfig,
}

fn default_compressor_mode() -> String { "rule".into() }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlmConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
}

fn default_provider() -> String { "ollama".into() }
fn default_model() -> String { "llama3.2:3b".into() }
fn default_endpoint() -> String { "http://localhost:11434/api/generate".into() }
fn default_max_tokens() -> u32 { 256 }
fn default_temperature() -> f32 { 0.3 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhostsConfig {
    #[serde(default = "default_true")]
    pub vscode_enabled: bool,
    #[serde(default = "default_true")]
    pub terminal_enabled: bool,
    #[serde(default)]
    pub slack_enabled: bool,
    #[serde(default)]
    pub obsidian_enabled: bool,
    #[serde(default, rename = "terminal")]
    pub terminal_config: TerminalGhostConfig,
    #[serde(default, rename = "slack")]
    pub slack_config: SlackGhostConfig,
    #[serde(default, rename = "vscode")]
    pub vscode_config: VscodeGhostConfig,
    #[serde(default, rename = "obsidian")]
    pub obsidian_config: ObsidianGhostConfig,
    #[serde(default, rename = "browser")]
    pub browser_config: BrowserGhostConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserGhostConfig {
    /// How long the browser banner stays up, in seconds. The banner goes
    /// off after 1 minute by default; raise this to keep it longer.
    #[serde(default = "default_banner_seconds")]
    pub banner_seconds: u64,
    /// THE QUIET LAW: the banner pushes into a surface the user is
    /// actively using, so by default it only wakes when the user just
    /// returned to the machine (sleep or a long gap in all activity),
    /// never on mid-flow project hops or branch switches. Set false to
    /// restore the old every-return behavior.
    #[serde(default = "default_machine_return_only")]
    pub machine_return_only: bool,
}

fn default_banner_seconds() -> u64 { 60 }
fn default_machine_return_only() -> bool { true }

impl Default for BrowserGhostConfig {
    fn default() -> Self {
        Self {
            banner_seconds: default_banner_seconds(),
            machine_return_only: default_machine_return_only(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TerminalGhostConfig {
    #[serde(default = "default_motd_file")]
    pub motd_file: String,
}

fn default_motd_file() -> String { "~/.revenant/motd".into() }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SlackGhostConfig {
    pub bot_token: String,
    pub user_id: String,
    pub channel_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VscodeGhostConfig {
    #[serde(default = "default_vscode_socket")]
    pub socket_path: String,
}

fn default_vscode_socket() -> String { "~/.revenant/vscode.sock".into() }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ObsidianGhostConfig {
    pub vault_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_db_path")]
    pub db_path: String,
}

fn default_db_path() -> String { "~/.revenant/revenant.db".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectsConfig {
    #[serde(default)]
    pub roots: Vec<String>,
}

fn default_true() -> bool { true }

fn default_ignore_patterns() -> Vec<String> {
    vec![
        "node_modules".into(),
        ".git".into(),
        "target".into(),
        "dist".into(),
        "__pycache__".into(),
        "*.pyc".into(),
        ".DS_Store".into(),
    ]
}

impl Default for RevenantConfig {
    fn default() -> Self {
        Self {
            daemon: DaemonConfig {
                poll_interval: 5,
                absence_threshold_minutes: 15,
                ghost_ttl_minutes: 2,
                prune_after_days: 30,
                pid_file: default_pid_file(),
            },
            signals: SignalsConfig {
                git: true,
                editor_enabled: true,
                terminal_enabled: true,
                clipboard: false,
                browser: false,
                ignore_patterns: default_ignore_patterns(),
                terminal_config: TerminalSignalConfig::default(),
                editor_config: EditorSignalConfig::default(),
            },
            compressor: CompressorConfig {
                mode: "rule".into(),
                llm: LlmConfig::default(),
            },
            ghosts: GhostsConfig {
                vscode_enabled: true,
                terminal_enabled: true,
                slack_enabled: false,
                obsidian_enabled: false,
                terminal_config: TerminalGhostConfig::default(),
                slack_config: SlackGhostConfig::default(),
                vscode_config: VscodeGhostConfig::default(),
                obsidian_config: ObsidianGhostConfig::default(),
                browser_config: BrowserGhostConfig::default(),
            },
            storage: StorageConfig {
                db_path: default_db_path(),
            },
            projects: ProjectsConfig { roots: vec![] },
        }
    }
}

/// Load configuration from disk, falling back to defaults for missing fields
pub fn load_config() -> Result<RevenantConfig> {
    let config_path = config_file_path();

    if config_path.exists() {
        let contents = std::fs::read_to_string(&config_path)
            .with_context(|| format!("reading {}", config_path.display()))?;
        let config: RevenantConfig = toml::from_str(&contents)
            .with_context(|| format!("parsing {}", config_path.display()))?;
        Ok(config)
    } else {
        tracing::info!(
            "no config at {}, using defaults",
            config_path.display()
        );
        Ok(RevenantConfig::default())
    }
}

// Always ~/.config/revenant, matching the CLI and the documented layout
// (dirs::config_dir() would resolve to Application Support on macOS)
fn config_file_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".config/revenant/config.toml")
}

impl CompressorConfig {
    /// Compress a working state into a context card.
    /// Routes to LLM or rule-based depending on config.
    pub async fn compress(
        &self,
        state: &crate::snapshot::WorkingState,
    ) -> Result<crate::store::ContextCard> {
        match self.mode.as_str() {
            "llm" => crate::compressor::llm_compress(state, &self.llm).await,
            _ => Ok(crate::compressor::rule_based_compress(state)),
        }
    }
}
