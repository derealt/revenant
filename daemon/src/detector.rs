//! Context switch detection - the brain of REVENANT
//!
//! Detects three types of context switches:
//! 1. Project change - user starts working in a different directory
//! 2. Long absence - no activity for >15min (configurable)
//! 3. Branch change - git branch switches within the same project
//!
//! Each detection triggers a Departure (save state) and/or Return (restore state).

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use crate::config::RevenantConfig;

/// A context switch event
#[derive(Debug, Clone)]
pub struct SwitchEvent {
    pub kind: SwitchKind,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub enum SwitchKind {
    /// User left a project (save snapshot)
    Departure { project_dir: String },
    /// User returned to a project (restore ghost)
    Return {
        project_dir: String,
        /// Which project the user came from (if this was a project switch)
        from_project: Option<String>,
    },
    /// Activity timeout - user has been back long enough, clear ghosts
    Timeout,
}

/// Per-project tracking state
struct ProjectTracker {
    /// Last known git branch
    last_branch: Option<String>,
    /// When we last saw activity in this project
    last_activity: Instant,
    /// Whether a ghost is currently displayed for this project
    ghost_active: bool,
    /// When the ghost was displayed
    ghost_displayed_at: Option<Instant>,
}

pub struct SwitchDetector {
    absence_threshold: std::time::Duration,
    ghost_ttl: std::time::Duration,
    /// Tracked projects
    projects: Mutex<HashMap<String, ProjectTracker>>,
    /// Currently active project
    active_project: Mutex<Option<String>>,
}

impl SwitchDetector {
    pub fn new(config: &RevenantConfig) -> Self {
        Self {
            absence_threshold: std::time::Duration::from_secs(
                config.daemon.absence_threshold_minutes * 60,
            ),
            ghost_ttl: std::time::Duration::from_secs(config.daemon.ghost_ttl_minutes * 60),
            projects: Mutex::new(HashMap::new()),
            active_project: Mutex::new(None),
        }
    }

    /// Record activity in a project, return switch event if context changed
    pub fn record_activity(&self, project_dir: &str) -> Option<SwitchEvent> {
        let now = Instant::now();
        let mut projects = self.projects.lock().ok()?;
        let mut active = self.active_project.lock().ok()?;

        // Check if this is a project switch
        let switched_from = if let Some(ref current) = *active {
            if current != project_dir {
                Some(current.clone())
            } else {
                None
            }
        } else {
            None
        };

        // Update tracker for this project
        let tracker = projects
            .entry(project_dir.to_string())
            .or_insert_with(|| ProjectTracker {
                last_branch: None,
                last_activity: now,
                ghost_active: false,
                ghost_displayed_at: None,
            });

        let was_absent = now.duration_since(tracker.last_activity) > self.absence_threshold;
        tracker.last_activity = now;
        *active = Some(project_dir.to_string());

        // Determine event
        if switched_from.is_some() {
            // Project switch - departure from old, return to new
            Some(SwitchEvent {
                kind: SwitchKind::Return {
                    project_dir: project_dir.to_string(),
                    from_project: switched_from,
                },
                timestamp: Utc::now(),
            })
        } else if was_absent {
            // Long absence - treat as return
            Some(SwitchEvent {
                kind: SwitchKind::Return {
                    project_dir: project_dir.to_string(),
                    from_project: None,
                },
                timestamp: Utc::now(),
            })
        } else {
            None
        }
    }

    /// Record that a ghost was displayed for a project
    pub fn mark_ghost_active(&self, project_dir: &str) {
        if let Ok(mut projects) = self.projects.lock() {
            if let Some(tracker) = projects.get_mut(project_dir) {
                tracker.ghost_active = true;
                tracker.ghost_displayed_at = Some(Instant::now());
            }
        }
    }

    /// Check for time-based events (ghost TTL expiry)
    pub fn check_time_based(&self) -> Option<SwitchEvent> {
        let now = Instant::now();
        let mut projects = self.projects.lock().ok()?;

        for (_dir, tracker) in projects.iter_mut() {
            if tracker.ghost_active {
                if let Some(displayed_at) = tracker.ghost_displayed_at {
                    if now.duration_since(displayed_at) > self.ghost_ttl {
                        // Ghost has been visible long enough - user is back in flow
                        tracker.ghost_active = false;
                        tracker.ghost_displayed_at = None;
                        return Some(SwitchEvent {
                            kind: SwitchKind::Timeout,
                            timestamp: Utc::now(),
                        });
                    }
                }
            }
        }

        None
    }

    /// Detect git branch change within a project
    pub fn check_branch_change(
        &self,
        project_dir: &str,
        current_branch: &str,
    ) -> Option<SwitchEvent> {
        let mut projects = self.projects.lock().ok()?;
        let tracker = projects.get_mut(project_dir)?;

        let changed = tracker
            .last_branch
            .as_ref()
            .map(|prev| prev != current_branch)
            .unwrap_or(false);

        tracker.last_branch = Some(current_branch.to_string());

        if changed {
            Some(SwitchEvent {
                kind: SwitchKind::Return {
                    project_dir: project_dir.to_string(),
                    from_project: None,
                },
                timestamp: Utc::now(),
            })
        } else {
            None
        }
    }
}
