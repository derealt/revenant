//! REVENANT - Invisible cognitive context daemon
//!
//! Captures working state when you leave a task.
//! Restores it as ghost annotations when you return.
//! No UI. No app. Just traces of your previous mind.

use anyhow::{Context, Result};
use chrono::Utc;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook_tokio::Signals;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::time::{self, Duration};
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

use revenant::config::{self, RevenantConfig};
use revenant::detector::{SwitchDetector, SwitchEvent, SwitchKind};
use revenant::ghost::{self, GhostDispatcher};
use revenant::snapshot::SnapshotAggregator;
use revenant::store::ContextStore;
use revenant::{compressor, expand_path, watcher};

/// Top-level daemon state, shared across async tasks
struct Daemon {
    config: RevenantConfig,
    store: ContextStore,
    aggregator: SnapshotAggregator,
    detector: SwitchDetector,
    dispatcher: GhostDispatcher,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("revenant=info")),
        )
        .with_target(false)
        .with_thread_ids(false)
        .compact()
        .init();

    info!("revenant waking - pid {}", std::process::id());

    // Load configuration
    let config = config::load_config().context("failed to load configuration")?;

    // Write PID file
    let pid_path = expand_path(&config.daemon.pid_file);
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&pid_path, std::process::id().to_string())?;
    info!("pid file written to {}", pid_path.display());

    // Initialize storage
    let db_path = expand_path(&config.storage.db_path);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = ContextStore::open(&db_path).context("failed to open context store")?;
    store.migrate()?;
    info!("context store ready at {}", db_path.display());

    // Build components
    let aggregator = SnapshotAggregator::new(&config);
    let detector = SwitchDetector::new(&config);
    let dispatcher = GhostDispatcher::new(&config)?;

    let daemon = Arc::new(RwLock::new(Daemon {
        config: config.clone(),
        store,
        aggregator,
        detector,
        dispatcher,
    }));

    // Channel for switch events
    let (switch_tx, mut switch_rx) = mpsc::channel::<SwitchEvent>(32);

    // Spawn ghost HTTP server for browser extension polling and tab reports
    let ghost_http = Arc::new(ghost::http::GhostHttpServer::new(
        config.signals.browser,
        config.ghosts.browser_config.banner_seconds,
    ));
    let ghost_http_server = Arc::clone(&ghost_http);
    tokio::spawn(async move {
        ghost_http_server.serve(7711).await;
    });

    // Pre-load the most recent card into the HTTP ghost so the browser
    // extension has something to show immediately on startup/return,
    // even before the file watcher detects activity.
    {
        let d = daemon.read().await;
        let recent_card = d.store.cards_since(
            Utc::now() - chrono::Duration::days(7),
            1,
        );
        if let Ok(cards) = recent_card {
            if let Some(card) = cards.into_iter().next() {
                let card_json = serde_json::json!({
                    "type": "inject",
                    "card": {
                        "id": card.id,
                        "summary": card.summary,
                        "next_step": card.next_step,
                        "project_dir": card.project_dir,
                        "project_name": card.project_name,
                        "ttl_seconds": card.ttl_seconds,
                    }
                });
                ghost_http.inject(serde_json::to_string(&card_json).unwrap_or_default());
                info!("pre-loaded startup ghost for {} - {}", card.project_name, card.summary);

                // Also write the terminal MOTD so new shells see it
                if let Err(e) = d.dispatcher.dispatch(&card).await {
                    warn!("startup ghost dispatch failed: {e}");
                }
            }
        }
    }

    // Spawn file watcher
    let watcher_config = config.clone();
    let watcher_switch_tx = switch_tx.clone();
    tokio::spawn(async move {
        if let Err(e) = watcher::run_watcher(watcher_config, watcher_switch_tx).await {
            error!("file watcher failed: {e}");
        }
    });

    // Spawn polling loop (for time-based detection: absence, periodic snapshot)
    let poll_daemon = Arc::clone(&daemon);
    let poll_switch_tx = switch_tx.clone();
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let d = poll_daemon.read().await;
            // Check for time-based context switches
            if let Some(event) = d.detector.check_time_based() {
                if let Err(e) = poll_switch_tx.send(event).await {
                    warn!("failed to send time-based switch event: {e}");
                }
            }
        }
    });

    // Spawn ghost TTL reaper
    let reaper_daemon = Arc::clone(&daemon);
    tokio::spawn(async move {
        let ttl = {
            let d = reaper_daemon.read().await;
            Duration::from_secs(d.config.daemon.ghost_ttl_minutes * 60)
        };
        let mut interval = time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            let d = reaper_daemon.read().await;
            if let Err(e) = d.dispatcher.reap_expired(ttl).await {
                warn!("ghost reaper error: {e}");
            }
        }
    });

    // Spawn periodic pruning
    let prune_daemon = Arc::clone(&daemon);
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(3600)); // hourly
        loop {
            interval.tick().await;
            let d = prune_daemon.read().await;
            if let Err(e) = d.store.prune(d.config.daemon.prune_after_days) {
                warn!("prune error: {e}");
            }
        }
    });

    // Main event loop: process context switch events
    let main_daemon = Arc::clone(&daemon);
    let event_loop = tokio::spawn(async move {
        while let Some(event) = switch_rx.recv().await {
            info!("switch detected: {:?}", event.kind);
            let d = main_daemon.read().await;

            match event.kind {
                SwitchKind::Departure { project_dir } => {
                    // User left - snapshot, compress, store
                    match d.aggregator.capture(&project_dir).await {
                        Ok(working_state) => {
                            let card = d
                                .config
                                .compressor
                                .compress(&working_state)
                                .await
                                .unwrap_or_else(|e| {
                                    warn!("LLM compression failed, using rule-based: {e}");
                                    compressor::rule_based_compress(&working_state)
                                });
                            if let Err(e) = d.store.save_card(&card) {
                                error!("failed to save context card: {e}");
                            } else {
                                info!(
                                    "context card saved for {} - {}",
                                    card.project_dir, card.summary
                                );
                            }
                        }
                        Err(e) => warn!("snapshot capture failed: {e}"),
                    }
                }
                SwitchKind::Return { project_dir, from_project } => {
                    // User returned - load card, dispatch ghosts
                    match d.store.latest_card(&project_dir) {
                        Ok(Some(mut card)) => {
                            // Enrich card with cross-project context
                            if let Some(ref from) = from_project {
                                let from_name = from.rsplit('/').next().unwrap_or("unknown");
                                card.summary = format!(
                                    "{} (you switched from {})",
                                    card.summary.trim_end_matches('.'),
                                    from_name
                                );
                            }

                            info!("restoring ghost for {} - {}", card.project_dir, card.summary);
                            // Update HTTP server for browser extension
                            let card_json = serde_json::json!({
                                "type": "inject",
                                "card": {
                                    "id": card.id,
                                    "summary": card.summary,
                                    "next_step": card.next_step,
                                    "project_dir": card.project_dir,
                                    "project_name": card.project_dir.split('/').last().unwrap_or("unknown"),
                                    "ttl_seconds": card.ttl_seconds,
                                    "from_project": from_project.as_deref().and_then(|p| p.rsplit('/').next()),
                                }
                            });
                            ghost_http.inject(serde_json::to_string(&card_json).unwrap_or_default());
                            if let Err(e) = d.dispatcher.dispatch(&card).await {
                                error!("ghost dispatch failed: {e}");
                            }
                        }
                        Ok(None) => {
                            info!("no context card for {}, nothing to restore", project_dir);
                        }
                        Err(e) => error!("failed to load context card: {e}"),
                    }
                }
                SwitchKind::Timeout => {
                    // Activity timeout - clear any lingering ghosts
                    ghost_http.clear();
                    if let Err(e) = d.dispatcher.clear_all().await {
                        warn!("failed to clear ghosts on timeout: {e}");
                    }
                }
            }
        }
    });

    // Signal handling for graceful shutdown
    let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
    let shutdown_daemon = Arc::clone(&daemon);
    tokio::select! {
        _ = event_loop => {
            warn!("event loop exited unexpectedly");
        }
        sig = signals.next() => {
            if let Some(sig) = sig {
                info!("received signal {sig}, shutting down");
            }
        }
    }

    // Cleanup
    let d = shutdown_daemon.read().await;
    let _ = d.dispatcher.clear_all().await;
    let _ = std::fs::remove_file(&pid_path);
    info!("revenant resting");

    Ok(())
}

