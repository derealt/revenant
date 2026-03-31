//! Ghost dispatcher — routes context cards to per-tool injectors
//!
//! Each ghost channel (VS Code, terminal, Slack, browser, Obsidian)
//! receives the context card and injects it in a tool-native way.
//! Ghosts are transient — they self-destruct after the configured TTL.

pub mod browser;
pub mod http;
pub mod obsidian;
pub mod slack;
pub mod terminal;
pub mod vscode;

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::RevenantConfig;
use crate::store::ContextCard;

/// A ghost channel that can display and clear context cards
#[async_trait::async_trait]
pub trait GhostChannel: Send + Sync {
    /// Display the context card in this tool
    async fn inject(&self, card: &ContextCard) -> Result<()>;
    /// Clear/remove any displayed ghost
    async fn clear(&self) -> Result<()>;
    /// Channel name for logging
    fn name(&self) -> &str;
}

/// Note: we implement async_trait manually to avoid the dependency
/// The trait above is used as documentation; actual dispatch uses concrete types

pub struct GhostDispatcher {
    terminal: Option<terminal::TerminalGhost>,
    vscode: Option<vscode::VscodeGhost>,
    slack: Option<slack::SlackGhost>,
    obsidian: Option<obsidian::ObsidianGhost>,
    browser: Option<browser::BrowserGhost>,
    // Track active ghosts for reaping
    active_ghosts: Arc<RwLock<Vec<ActiveGhost>>>,
}

struct ActiveGhost {
    channel: String,
    injected_at: std::time::Instant,
}

impl GhostDispatcher {
    pub fn new(config: &RevenantConfig) -> Result<Self> {
        let terminal = if config.ghosts.terminal_enabled {
            Some(terminal::TerminalGhost::new(&config.ghosts.terminal_config)?)
        } else {
            None
        };

        let vscode = if config.ghosts.vscode_enabled {
            Some(vscode::VscodeGhost::new(&config.ghosts.vscode_config)?)
        } else {
            None
        };

        let slack = if config.ghosts.slack_enabled {
            Some(slack::SlackGhost::new(&config.ghosts.slack_config)?)
        } else {
            None
        };

        let obsidian = if config.ghosts.obsidian_enabled {
            Some(obsidian::ObsidianGhost::new(&config.ghosts.obsidian_config)?)
        } else {
            None
        };

        let browser = if config.ghosts.browser_enabled {
            Some(browser::BrowserGhost::new()?)
        } else {
            None
        };

        Ok(Self {
            terminal,
            vscode,
            slack,
            obsidian,
            browser,
            active_ghosts: Arc::new(RwLock::new(Vec::new())),
        })
    }

    /// Dispatch a context card to all enabled ghost channels
    pub async fn dispatch(&self, card: &ContextCard) -> Result<()> {
        let mut dispatched = Vec::new();

        if let Some(ref ghost) = self.terminal {
            match ghost.inject(card).await {
                Ok(()) => {
                    info!("terminal ghost injected");
                    dispatched.push("terminal".to_string());
                }
                Err(e) => warn!("terminal ghost injection failed: {e}"),
            }
        }

        if let Some(ref ghost) = self.vscode {
            match ghost.inject(card).await {
                Ok(()) => {
                    info!("vscode ghost injected");
                    dispatched.push("vscode".to_string());
                }
                Err(e) => warn!("vscode ghost injection failed: {e}"),
            }
        }

        if let Some(ref ghost) = self.slack {
            match ghost.inject(card).await {
                Ok(()) => {
                    info!("slack ghost injected");
                    dispatched.push("slack".to_string());
                }
                Err(e) => warn!("slack ghost injection failed: {e}"),
            }
        }

        if let Some(ref ghost) = self.obsidian {
            match ghost.inject(card).await {
                Ok(()) => {
                    info!("obsidian ghost injected");
                    dispatched.push("obsidian".to_string());
                }
                Err(e) => warn!("obsidian ghost injection failed: {e}"),
            }
        }

        if let Some(ref ghost) = self.browser {
            match ghost.inject(card).await {
                Ok(()) => {
                    info!("browser ghost injected");
                    dispatched.push("browser".to_string());
                }
                Err(e) => warn!("browser ghost injection failed: {e}"),
            }
        }

        // Record active ghosts for TTL reaping
        let mut active = self.active_ghosts.write().await;
        for channel in dispatched {
            active.push(ActiveGhost {
                channel,
                injected_at: std::time::Instant::now(),
            });
        }

        Ok(())
    }

    /// Clear all active ghosts
    pub async fn clear_all(&self) -> Result<()> {
        if let Some(ref ghost) = self.terminal {
            ghost.clear().await?;
        }
        if let Some(ref ghost) = self.vscode {
            ghost.clear().await?;
        }
        if let Some(ref ghost) = self.slack {
            ghost.clear().await?;
        }
        if let Some(ref ghost) = self.obsidian {
            ghost.clear().await?;
        }
        if let Some(ref ghost) = self.browser {
            ghost.clear().await?;
        }

        let mut active = self.active_ghosts.write().await;
        active.clear();

        info!("all ghosts cleared");
        Ok(())
    }

    /// Clear ghosts that have exceeded their TTL
    pub async fn reap_expired(&self, ttl: Duration) -> Result<()> {
        let now = std::time::Instant::now();
        let mut active = self.active_ghosts.write().await;

        let expired: Vec<String> = active
            .iter()
            .filter(|g| now.duration_since(g.injected_at) > ttl)
            .map(|g| g.channel.clone())
            .collect();

        if expired.is_empty() {
            return Ok(());
        }

        for channel in &expired {
            match channel.as_str() {
                "terminal" => {
                    if let Some(ref ghost) = self.terminal {
                        ghost.clear().await?;
                    }
                }
                "vscode" => {
                    if let Some(ref ghost) = self.vscode {
                        ghost.clear().await?;
                    }
                }
                "slack" => {
                    if let Some(ref ghost) = self.slack {
                        ghost.clear().await?;
                    }
                }
                "obsidian" => {
                    if let Some(ref ghost) = self.obsidian {
                        ghost.clear().await?;
                    }
                }
                "browser" => {
                    if let Some(ref ghost) = self.browser {
                        ghost.clear().await?;
                    }
                }
                _ => {}
            }
        }

        active.retain(|g| !expired.contains(&g.channel));
        info!("reaped {} expired ghosts", expired.len());

        Ok(())
    }
}
