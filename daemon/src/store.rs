//! SQLite persistence - save, load, query, and prune context cards
//!
//! Each context card is tied to a project directory and timestamped.
//! The store auto-prunes cards older than the configured retention period.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

/// A compressed cognitive context card
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextCard {
    /// Unique identifier
    pub id: String,
    /// Absolute path to the project root
    pub project_dir: String,
    /// Human-readable project name (directory basename)
    pub project_name: String,
    /// The context summary - what you were doing + what's next
    pub summary: String,
    /// Extracted next step (may be empty)
    pub next_step: String,
    /// When this card was created
    pub created_at: DateTime<Utc>,
    /// Raw signals JSON (for debugging / re-compression)
    pub signals_json: String,
    /// How long this ghost should live (seconds)
    pub ttl_seconds: u64,
}

pub struct ContextStore {
    conn: Mutex<Connection>,
}

impl ContextStore {
    /// Open or create the SQLite database
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening database at {}", path.display()))?;

        // Performance pragmas
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA cache_size = -2000;
             PRAGMA busy_timeout = 5000;",
        )?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Create tables if they don't exist
    pub fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS context_cards (
                id TEXT PRIMARY KEY,
                project_dir TEXT NOT NULL,
                project_name TEXT NOT NULL,
                summary TEXT NOT NULL,
                next_step TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                signals_json TEXT NOT NULL DEFAULT '{}',
                ttl_seconds INTEGER NOT NULL DEFAULT 300
            );

            CREATE INDEX IF NOT EXISTS idx_cards_project_dir
                ON context_cards(project_dir);

            CREATE INDEX IF NOT EXISTS idx_cards_created_at
                ON context_cards(created_at);

            CREATE TABLE IF NOT EXISTS ghost_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                card_id TEXT NOT NULL,
                channel TEXT NOT NULL,
                dispatched_at TEXT NOT NULL,
                cleared_at TEXT,
                FOREIGN KEY (card_id) REFERENCES context_cards(id)
            );

            CREATE TABLE IF NOT EXISTS project_state (
                project_dir TEXT PRIMARY KEY,
                last_branch TEXT,
                last_activity TEXT NOT NULL,
                last_card_id TEXT,
                FOREIGN KEY (last_card_id) REFERENCES context_cards(id)
            );",
        )?;
        Ok(())
    }

    /// Save a context card, skipping if it's a near-duplicate of the latest card
    pub fn save_card(&self, card: &ContextCard) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        // Dedup: skip if the latest card for this project is >80% similar
        let maybe_prev: Option<String> = conn
            .query_row(
                "SELECT summary FROM context_cards WHERE project_dir = ?1 ORDER BY created_at DESC LIMIT 1",
                params![card.project_dir],
                |row| row.get(0),
            )
            .ok();

        if let Some(ref prev_summary) = maybe_prev {
            // Strip temporal prefixes before comparing - "Just now you were"
            // vs "Moments ago you were" should still count as duplicates
            let a = strip_temporal_prefix(prev_summary);
            let b = strip_temporal_prefix(&card.summary);
            if text_similarity(&a, &b) > 0.75 {
                tracing::debug!(
                    "skipping near-duplicate card for {} (similarity > 75%)",
                    card.project_name
                );
                return Ok(());
            }
        }

        conn.execute(
            "INSERT OR REPLACE INTO context_cards
                (id, project_dir, project_name, summary, next_step, created_at, signals_json, ttl_seconds)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                card.id,
                card.project_dir,
                card.project_name,
                card.summary,
                card.next_step,
                card.created_at.to_rfc3339(),
                card.signals_json,
                card.ttl_seconds,
            ],
        )?;

        // Update project state
        conn.execute(
            "INSERT OR REPLACE INTO project_state (project_dir, last_activity, last_card_id)
             VALUES (?1, ?2, ?3)",
            params![
                card.project_dir,
                card.created_at.to_rfc3339(),
                card.id,
            ],
        )?;

        Ok(())
    }

    /// Get the most recent context card for a project
    pub fn latest_card(&self, project_dir: &str) -> Result<Option<ContextCard>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, project_dir, project_name, summary, next_step, created_at, signals_json, ttl_seconds
             FROM context_cards
             WHERE project_dir = ?1
             ORDER BY created_at DESC
             LIMIT 1",
        )?;

        let card = stmt
            .query_row(params![project_dir], |row| {
                Ok(ContextCard {
                    id: row.get(0)?,
                    project_dir: row.get(1)?,
                    project_name: row.get(2)?,
                    summary: row.get(3)?,
                    next_step: row.get(4)?,
                    created_at: {
                        let s: String = row.get(5)?;
                        DateTime::parse_from_rfc3339(&s)
                            .map(|dt| dt.with_timezone(&Utc))
                            .unwrap_or_else(|_| Utc::now())
                    },
                    signals_json: row.get(6)?,
                    ttl_seconds: row.get::<_, i64>(7)? as u64,
                })
            })
            .ok();

        Ok(card)
    }

    /// Get all cards for a project, newest first
    pub fn cards_for_project(
        &self,
        project_dir: &str,
        limit: usize,
    ) -> Result<Vec<ContextCard>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, project_dir, project_name, summary, next_step, created_at, signals_json, ttl_seconds
             FROM context_cards
             WHERE project_dir = ?1
             ORDER BY created_at DESC
             LIMIT ?2",
        )?;

        let cards = stmt
            .query_map(params![project_dir, limit as i64], |row| {
                Ok(ContextCard {
                    id: row.get(0)?,
                    project_dir: row.get(1)?,
                    project_name: row.get(2)?,
                    summary: row.get(3)?,
                    next_step: row.get(4)?,
                    created_at: {
                        let s: String = row.get(5)?;
                        DateTime::parse_from_rfc3339(&s)
                            .map(|dt| dt.with_timezone(&Utc))
                            .unwrap_or_else(|_| Utc::now())
                    },
                    signals_json: row.get(6)?,
                    ttl_seconds: row.get::<_, i64>(7)? as u64,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(cards)
    }

    /// Record a ghost dispatch event
    pub fn log_ghost_dispatch(&self, card_id: &str, channel: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO ghost_log (card_id, channel, dispatched_at) VALUES (?1, ?2, ?3)",
            params![card_id, channel, Utc::now().to_rfc3339()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Mark a ghost as cleared
    pub fn log_ghost_cleared(&self, log_id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE ghost_log SET cleared_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), log_id],
        )?;
        Ok(())
    }

    /// Prune context cards older than `days`
    pub fn prune(&self, days: u64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let cutoff = Utc::now() - chrono::Duration::days(days as i64);
        let deleted = conn.execute(
            "DELETE FROM context_cards WHERE created_at < ?1",
            params![cutoff.to_rfc3339()],
        )?;

        if deleted > 0 {
            tracing::info!("pruned {deleted} context cards older than {days} days");
        }

        Ok(deleted)
    }

    /// Count total cards across all projects
    pub fn total_cards(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM context_cards", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Get the count of cards for a project within a time window
    pub fn cards_in_window(
        &self,
        project_dir: &str,
        since: DateTime<Utc>,
    ) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM context_cards WHERE project_dir = ?1 AND created_at >= ?2",
            params![project_dir, since.to_rfc3339()],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Get all cards across all projects within a time window, newest first
    pub fn cards_since(&self, since: DateTime<Utc>, limit: usize) -> Result<Vec<ContextCard>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, project_dir, project_name, summary, next_step, created_at, signals_json, ttl_seconds
             FROM context_cards
             WHERE created_at >= ?1
             ORDER BY created_at DESC
             LIMIT ?2",
        )?;

        let cards = stmt
            .query_map(params![since.to_rfc3339(), limit as i64], |row| {
                Ok(ContextCard {
                    id: row.get(0)?,
                    project_dir: row.get(1)?,
                    project_name: row.get(2)?,
                    summary: row.get(3)?,
                    next_step: row.get(4)?,
                    created_at: {
                        let s: String = row.get(5)?;
                        DateTime::parse_from_rfc3339(&s)
                            .map(|dt| dt.with_timezone(&Utc))
                            .unwrap_or_else(|_| Utc::now())
                    },
                    signals_json: row.get(6)?,
                    ttl_seconds: row.get::<_, i64>(7)? as u64,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(cards)
    }

    /// Get distinct projects with card counts and last activity, ordered by recency
    pub fn project_summary_since(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<ProjectDigestEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT project_dir, project_name, COUNT(*) as card_count,
                    MAX(created_at) as last_active, MIN(created_at) as first_active
             FROM context_cards
             WHERE created_at >= ?1
             GROUP BY project_dir
             ORDER BY card_count DESC",
        )?;

        let entries = stmt
            .query_map(params![since.to_rfc3339()], |row| {
                Ok(ProjectDigestEntry {
                    project_dir: row.get(0)?,
                    project_name: row.get(1)?,
                    card_count: row.get::<_, i64>(2)? as usize,
                    last_active: {
                        let s: String = row.get(3)?;
                        DateTime::parse_from_rfc3339(&s)
                            .map(|dt| dt.with_timezone(&Utc))
                            .unwrap_or_else(|_| Utc::now())
                    },
                    first_active: {
                        let s: String = row.get(4)?;
                        DateTime::parse_from_rfc3339(&s)
                            .map(|dt| dt.with_timezone(&Utc))
                            .unwrap_or_else(|_| Utc::now())
                    },
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(entries)
    }

    /// List all known project directories
    pub fn known_projects(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT project_dir FROM context_cards ORDER BY project_dir",
        )?;
        let projects = stmt
            .query_map([], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(projects)
    }
}

/// Digest entry for a project's activity over a time window
#[derive(Debug, Clone)]
pub struct ProjectDigestEntry {
    pub project_dir: String,
    pub project_name: String,
    pub card_count: usize,
    pub last_active: DateTime<Utc>,
    pub first_active: DateTime<Utc>,
}

/// Strip temporal framing prefixes so dedup compares the substance, not the phrasing
fn strip_temporal_prefix(s: &str) -> String {
    let prefixes = [
        "Just now you were ", "Moments ago you were ", "You were just ",
        "A little while ago you were ", "Not long ago you were ", "Recently you were ",
        "Earlier today you were ", "Earlier you were ", "A few hours ago you were ",
        "Yesterday you were ", "Last session you were ",
        "A few days ago you were ", "A couple days ago you were ",
        "Last Monday you were ", "Last Tuesday you were ", "Last Wednesday you were ",
        "Last Thursday you were ", "Last Friday you were ",
        "Last Saturday you were ", "Last Sunday you were ",
        "You left off ", "Your last change was ", "Last change: ",
        "You were in the middle of ", "You were ",
        "In Limn: ", "In limn: ",  // common project prefixes
    ];
    let trimmed = s.trim();
    for prefix in &prefixes {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    // Also strip generic "In {Project}: " pattern
    if trimmed.starts_with("In ") {
        if let Some(colon_pos) = trimmed.find(": ") {
            if colon_pos < 30 {
                return trimmed[colon_pos + 2..].to_string();
            }
        }
    }
    trimmed.to_string()
}

/// Simple word-overlap similarity (Jaccard index on word sets)
/// Returns a value between 0.0 and 1.0
fn text_similarity(a: &str, b: &str) -> f64 {
    let words_a: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let words_b: std::collections::HashSet<&str> = b.split_whitespace().collect();

    if words_a.is_empty() && words_b.is_empty() {
        return 1.0;
    }

    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();

    if union == 0 {
        return 0.0;
    }

    intersection as f64 / union as f64
}
