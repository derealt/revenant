//! File system watcher - filtered event stream feeding the detector
//!
//! Uses the `notify` crate with FSEvents (macOS) / inotify (Linux).
//! Events in ignored directories are dropped; the rest count as user
//! activity. Only TRANSITIONS emit switch events (project change,
//! absence, return), so the stream is inherently debounced at the
//! decision level.

use anyhow::{Context, Result};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::config::RevenantConfig;
use crate::detector::{SwitchDetector, SwitchEvent, SwitchKind};
use std::sync::Arc;

/// Run the file watcher, sending activity signals to the switch detector
pub async fn run_watcher(
    config: RevenantConfig,
    switch_tx: mpsc::Sender<SwitchEvent>,
    detector: Arc<SwitchDetector>,
) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<notify::Result<Event>>(256);

    // Collect all project roots to watch
    let mut roots: Vec<PathBuf> = config
        .projects
        .roots
        .iter()
        .map(|r| crate::expand_path(r))
        .collect();

    // If no explicit roots, watch common project directories
    if roots.is_empty() {
        if let Some(home) = dirs::home_dir() {
            let candidates = ["projects", "Documents", "src", "code", "work", "dev"];
            for c in &candidates {
                let p = home.join(c);
                if p.exists() && p.is_dir() {
                    roots.push(p);
                }
            }
        }
    }

    if roots.is_empty() {
        warn!("no project roots found to watch");
        return Ok(());
    }

    // Build ignore patterns
    let ignore_set: HashSet<String> = config
        .signals
        .ignore_patterns
        .iter()
        .cloned()
        .collect();

    // Create the watcher
    let notify_tx = tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = notify_tx.blocking_send(res);
        },
        Config::default().with_poll_interval(Duration::from_secs(2)),
    )
    .context("failed to create file watcher")?;

    for root in &roots {
        info!("watching {}", root.display());
        watcher.watch(root, RecursiveMode::Recursive)?;
    }

    // Transition state
    let mut last_event_time = Instant::now();
    let mut active_project: Option<String> = None;
    // Set once the periodic check has reported a departure for the current
    // absence, so we do not re-send one every poll tick while away
    let mut absence_departure_sent = false;
    let absence_threshold =
        Duration::from_secs(config.daemon.absence_threshold_minutes * 60);

    info!("file watcher running, {} roots", roots.len());

    loop {
        tokio::select! {
            Some(event_result) = rx.recv() => {
                if let Ok(event) = event_result {
                    // Filter out events in ignored directories
                    let dominated_by_ignore = event.paths.iter().all(|p| {
                        let path_str = p.to_string_lossy();
                        ignore_set.iter().any(|pattern| path_str.contains(pattern.as_str()))
                    });

                    if dominated_by_ignore {
                        continue;
                    }

                    let now = Instant::now();
                    let was_absent = now.duration_since(last_event_time) > absence_threshold;

                    // Detect which project this event belongs to
                    let project_dir = event
                        .paths
                        .first()
                        .and_then(|p| find_git_root(p));

                    if let Some(ref project) = project_dir {
                        let project_str = project.to_string_lossy().to_string();

                        // Keep the detector's view current (active project,
                        // per-project trackers for branch polling). The
                        // watcher stays the authority on switch decisions,
                        // so the detector's own verdict is discarded.
                        let _ = detector.record_activity(&project_str);

                        // Detect project change
                        let project_changed = active_project
                            .as_ref()
                            .map(|prev| prev != &project_str)
                            .unwrap_or(false);

                        if project_changed {
                            // Departure from previous project
                            let from = active_project.clone();
                            if let Some(ref prev) = from {
                                debug!("project switch: {} -> {}", prev, project_str);
                                let _ = switch_tx
                                    .send(SwitchEvent {
                                        kind: SwitchKind::Departure {
                                            project_dir: prev.clone(),
                                        },
                                        timestamp: chrono::Utc::now(),
                                    })
                                    .await;
                            }

                            // Return to new project - carry where we came from
                            let _ = switch_tx
                                .send(SwitchEvent {
                                    kind: SwitchKind::Return {
                                        project_dir: project_str.clone(),
                                        from_project: from,
                                    },
                                    timestamp: chrono::Utc::now(),
                                })
                                .await;
                        } else if was_absent {
                            // Same project but long absence - treat as return
                            debug!(
                                "return after absence to {}",
                                project_str
                            );

                            // Save departure state retroactively, unless the
                            // periodic check already captured it at absence time
                            if !absence_departure_sent {
                                let _ = switch_tx
                                    .send(SwitchEvent {
                                        kind: SwitchKind::Departure {
                                            project_dir: project_str.clone(),
                                        },
                                        timestamp: chrono::Utc::now(),
                                    })
                                    .await;
                            }

                            // Then trigger return
                            let _ = switch_tx
                                .send(SwitchEvent {
                                    kind: SwitchKind::Return {
                                        project_dir: project_str.clone(),
                                        from_project: None,
                                    },
                                    timestamp: chrono::Utc::now(),
                                })
                                .await;
                        }

                        active_project = Some(project_str);
                    }

                    last_event_time = now;
                    absence_departure_sent = false;
                }
            }
            // Periodic absence check - reports each absence exactly once
            _ = tokio::time::sleep(Duration::from_secs(30)) => {
                let elapsed = Instant::now().duration_since(last_event_time);
                if elapsed > absence_threshold && !absence_departure_sent {
                    if let Some(ref project) = active_project {
                        debug!("absence detected for {}", project);
                        let _ = switch_tx.send(SwitchEvent {
                            kind: SwitchKind::Departure {
                                project_dir: project.clone(),
                            },
                            timestamp: chrono::Utc::now(),
                        }).await;
                        absence_departure_sent = true;
                    }
                }
            }
        }
    }
}

/// Walk up from a path to find the nearest .git directory
fn find_git_root(path: &PathBuf) -> Option<PathBuf> {
    let mut current = if path.is_file() {
        path.parent()?.to_path_buf()
    } else {
        path.clone()
    };

    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}
