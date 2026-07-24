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
        /// Why this counts as a return - decides which channels wake
        cause: ReturnCause,
    },
    /// Activity timeout - user has been back long enough, clear ghosts
    Timeout,
}

/// Why a Return happened. Push surfaces (the browser banner) only wake
/// for a machine return; everything else is mid-flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReturnCause {
    /// The machine itself was away - sleep, or a long gap in ALL
    /// activity - and the user just came back
    MachineReturn,
    /// A hop between projects while actively working
    ProjectSwitch,
    /// A branch switch inside the active project
    BranchChange,
    /// A new shell asked "where was I" with no machine absence
    ShellOpen,
}

impl ReturnCause {
    /// THE QUIET LAW (Toyin, 2026-07-24): the browser banner pushes
    /// into a surface the user is actively using, so it speaks only
    /// when the user just came back to the machine. Mid-flow project
    /// hops, branch flits, and shell opens stay off it. The terminal
    /// keeps its own covenant (a new shell is a pull, not a push).
    pub fn wakes_browser(&self, machine_return_only: bool) -> bool {
        !machine_return_only || matches!(self, ReturnCause::MachineReturn)
    }
}

/// Per-project tracking state
struct ProjectTracker {
    /// Last known git branch
    last_branch: Option<String>,
    /// When we last saw activity in this project. Wall clock, not
    /// Instant: on macOS Instant stops counting while the machine
    /// sleeps, so an overnight absence would register as minutes.
    last_activity: std::time::SystemTime,
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
    /// Last activity anywhere on the machine. Wall clock (SystemTime)
    /// so sleep counts. None until the first activity after daemon
    /// start - a fresh daemon must not read as a machine absence.
    last_global_activity: Mutex<Option<std::time::SystemTime>>,
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
            last_global_activity: Mutex::new(None),
        }
    }

    /// True when there was no activity anywhere on the machine for
    /// longer than the absence threshold. Advances the global clock.
    fn machine_was_absent(&self, now: std::time::SystemTime) -> bool {
        let mut global = match self.last_global_activity.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let absent = global
            .map(|t| now.duration_since(t).unwrap_or_default() > self.absence_threshold)
            .unwrap_or(false);
        *global = Some(now);
        absent
    }

    /// Record activity in a project, return switch event if context changed
    pub fn record_activity(&self, project_dir: &str) -> Option<SwitchEvent> {
        let now = std::time::SystemTime::now();
        let machine_absent = self.machine_was_absent(now);
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

        let was_absent = now
            .duration_since(tracker.last_activity)
            .unwrap_or_default()
            > self.absence_threshold;
        tracker.last_activity = now;
        *active = Some(project_dir.to_string());

        // Determine event. A machine absence outranks the hop as the
        // cause: coming back from sleep into a different project is
        // still a machine return.
        let cause = if machine_absent {
            ReturnCause::MachineReturn
        } else {
            ReturnCause::ProjectSwitch
        };
        if switched_from.is_some() {
            // Project switch - departure from old, return to new
            Some(SwitchEvent {
                kind: SwitchKind::Return {
                    project_dir: project_dir.to_string(),
                    from_project: switched_from,
                    cause,
                },
                timestamp: Utc::now(),
            })
        } else if was_absent {
            // Long absence - treat as return
            Some(SwitchEvent {
                kind: SwitchKind::Return {
                    project_dir: project_dir.to_string(),
                    from_project: None,
                    cause,
                },
                timestamp: Utc::now(),
            })
        } else {
            None
        }
    }

    /// A new shell opened on a project. Unlike record_activity, a project
    /// the daemon has never seen counts as an absence: a fresh shell on an
    /// untracked project deserves its ghost, because the daemon cannot
    /// prove the user was recently here. A shell opened mid-flow stays
    /// silent.
    pub fn shell_opened(&self, project_dir: &str) -> Option<SwitchEvent> {
        let now = std::time::SystemTime::now();
        let machine_absent = self.machine_was_absent(now);
        let mut projects = self.projects.lock().ok()?;
        let mut active = self.active_project.lock().ok()?;

        let switched_from = match *active {
            Some(ref current) if current != project_dir => Some(current.clone()),
            _ => None,
        };

        let first_sighting = !projects.contains_key(project_dir);
        let tracker = projects
            .entry(project_dir.to_string())
            .or_insert_with(|| ProjectTracker {
                last_branch: None,
                last_activity: now,
                ghost_active: false,
                ghost_displayed_at: None,
            });

        let was_absent = first_sighting
            || now
                .duration_since(tracker.last_activity)
                .unwrap_or_default()
                > self.absence_threshold;

        // Only a summoning shell resets the absence clock. A silent
        // mid-flow tab must not push the clock forward, or occasional
        // tab-opens could suppress a deserved ghost indefinitely.
        if switched_from.is_some() || was_absent {
            tracker.last_activity = now;
        }
        *active = Some(project_dir.to_string());

        // A shell open is a pull on the terminal channel; it only
        // counts as a machine return for the push channels when the
        // whole machine was actually away
        let cause = if machine_absent {
            ReturnCause::MachineReturn
        } else {
            ReturnCause::ShellOpen
        };
        if switched_from.is_some() {
            Some(SwitchEvent {
                kind: SwitchKind::Return {
                    project_dir: project_dir.to_string(),
                    from_project: switched_from,
                    cause,
                },
                timestamp: Utc::now(),
            })
        } else if was_absent {
            Some(SwitchEvent {
                kind: SwitchKind::Return {
                    project_dir: project_dir.to_string(),
                    from_project: None,
                    cause,
                },
                timestamp: Utc::now(),
            })
        } else {
            None
        }
    }

    /// The project the user is currently working in, if known
    pub fn active_project(&self) -> Option<String> {
        self.active_project.lock().ok()?.clone()
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
                    cause: ReturnCause::BranchChange,
                },
                timestamp: Utc::now(),
            })
        } else {
            None
        }
    }
}
