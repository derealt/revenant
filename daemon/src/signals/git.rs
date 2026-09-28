//! Git state signal - branch, status, recent commits, diff stats
//!
//! Shells out to `git` binary rather than linking libgit2,
//! keeping the binary small and avoiding C dependency chains.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitState {
    /// Current branch name (or detached HEAD ref)
    pub branch: String,
    /// Number of staged files
    pub staged: usize,
    /// Number of modified (unstaged) files
    pub modified: usize,
    /// Number of untracked files
    pub untracked: usize,
    /// Files in the diff (staged + unstaged), with change type
    pub changed_files: Vec<ChangedFile>,
    /// Recent commits (last 5)
    pub recent_commits: Vec<CommitSummary>,
    /// Diff stat summary (e.g., "3 files changed, 47 insertions(+), 12 deletions(-)")
    pub diff_stat: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub status: ChangeStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitSummary {
    pub hash: String,
    pub message: String,
    pub author: String,
    pub timestamp: String,
}

/// Capture the full git state for a project directory
pub fn capture(project_dir: &str) -> Result<GitState> {
    // Verify this is a git repo
    let git_dir = std::path::Path::new(project_dir).join(".git");
    if !git_dir.exists() {
        bail!("not a git repository: {project_dir}");
    }

    let branch = git_branch(project_dir)?;
    let (staged, modified, untracked, changed_files) = git_status(project_dir)?;
    let recent_commits = git_log(project_dir, 5)?;
    let diff_stat = git_diff_stat(project_dir)?;

    Ok(GitState {
        branch,
        staged,
        modified,
        untracked,
        changed_files,
        recent_commits,
        diff_stat,
    })
}

/// Just the current branch: cheap enough to poll, used by the daemon's
/// branch-change detection without paying for a full state capture
pub fn current_branch(project_dir: &str) -> Result<String> {
    git_branch(project_dir)
}

fn git_cmd(project_dir: &str, args: &[&str]) -> Result<String> {
    Ok(git_raw(project_dir, args)?.trim().to_string())
}

/// Output with only trailing whitespace removed. Porcelain status needs this:
/// its first column is a space for an unstaged-only change, and a full trim
/// shifted the first line left, reading " M web/app.ts" as a STAGED
/// "eb/app.ts".
fn git_raw(project_dir: &str, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(["-C", project_dir])
        .args(args)
        .output()
        .context("failed to execute git")?;

    Ok(String::from_utf8_lossy(&output.stdout).trim_end().to_string())
}

/// Files a tool rewrites on its own: compiler caches, lockfiles, source maps,
/// minified bundles. Some repos commit them, so git reports them as changed
/// after every build or install, but they are never what the user was doing.
/// They never become a card's topic and never count as in-flight edits.
pub fn is_generated_artifact(path: &str) -> bool {
    const LOCKFILES: &[&str] = &[
        "package-lock.json", "npm-shrinkwrap.json", "yarn.lock", "pnpm-lock.yaml",
        "bun.lock", "bun.lockb", "Cargo.lock", "go.sum", "poetry.lock", "Pipfile.lock",
        "uv.lock", "Gemfile.lock", "composer.lock", "flake.lock", "Podfile.lock",
        "mix.lock", "pubspec.lock", "packages.lock.json", ".DS_Store",
    ];
    const SUFFIXES: &[&str] = &[
        ".tsbuildinfo", ".map", ".min.js", ".min.css", ".pyc", ".pyo", ".class",
    ];
    const DIRS: &[&str] = &[
        "node_modules", ".next", ".nuxt", ".svelte-kit", ".turbo", "__pycache__",
        ".pytest_cache", ".mypy_cache", ".gradle",
    ];

    let path = path.trim_matches('"');
    let name = path.rsplit('/').next().unwrap_or(path);
    LOCKFILES.contains(&name)
        || SUFFIXES.iter().any(|s| name.ends_with(s))
        || path.split('/').any(|part| DIRS.contains(&part))
}

fn git_branch(project_dir: &str) -> Result<String> {
    let branch = git_cmd(project_dir, &["branch", "--show-current"])?;
    if branch.is_empty() {
        // Detached HEAD - get the short ref
        git_cmd(project_dir, &["rev-parse", "--short", "HEAD"])
    } else {
        Ok(branch)
    }
}

fn git_status(
    project_dir: &str,
) -> Result<(usize, usize, usize, Vec<ChangedFile>)> {
    let output = git_raw(project_dir, &["status", "--porcelain=v1"])?;
    let mut staged = 0usize;
    let mut modified = 0usize;
    let mut untracked = 0usize;
    let mut files = Vec::new();

    for line in output.lines() {
        if line.len() < 3 {
            continue;
        }
        let index = line.as_bytes()[0];
        let worktree = line.as_bytes()[1];
        // A rename reads "old -> new"; the file now lives at new
        let raw = &line[3..];
        let path = raw.rsplit(" -> ").next().unwrap_or(raw).to_string();
        if is_generated_artifact(&path) {
            continue;
        }

        // Index column: staged changes
        match index {
            b'A' | b'M' | b'D' | b'R' => staged += 1,
            b'?' => {
                untracked += 1;
                files.push(ChangedFile {
                    path,
                    status: ChangeStatus::Untracked,
                });
                continue;
            }
            _ => {}
        }

        // Worktree column: unstaged changes
        if worktree == b'M' || worktree == b'D' {
            modified += 1;
        }

        let status = match index {
            b'A' => ChangeStatus::Added,
            b'D' => ChangeStatus::Deleted,
            b'R' => ChangeStatus::Renamed,
            _ => ChangeStatus::Modified,
        };

        files.push(ChangedFile { path, status });
    }

    Ok((staged, modified, untracked, files))
}

fn git_log(project_dir: &str, count: usize) -> Result<Vec<CommitSummary>> {
    let format = "%H|%s|%an|%ai";
    let output = git_cmd(
        project_dir,
        &[
            "log",
            &format!("-{count}"),
            &format!("--format={format}"),
            "--no-decorate",
        ],
    )?;

    let mut commits = Vec::new();
    for line in output.lines() {
        let parts: Vec<&str> = line.splitn(4, '|').collect();
        if parts.len() == 4 {
            commits.push(CommitSummary {
                hash: parts[0][..8].to_string(),
                message: parts[1].to_string(),
                author: parts[2].to_string(),
                timestamp: parts[3].to_string(),
            });
        }
    }

    Ok(commits)
}

fn git_diff_stat(project_dir: &str) -> Result<String> {
    // Combined stat of staged + unstaged
    let staged = git_cmd(project_dir, &["diff", "--cached", "--stat"])?;
    let unstaged = git_cmd(project_dir, &["diff", "--stat"])?;

    let mut summary = String::new();
    if !staged.is_empty() {
        if let Some(last_line) = staged.lines().last() {
            summary.push_str("staged: ");
            summary.push_str(last_line.trim());
        }
    }
    if !unstaged.is_empty() {
        if !summary.is_empty() {
            summary.push_str(" | ");
        }
        if let Some(last_line) = unstaged.lines().last() {
            summary.push_str("unstaged: ");
            summary.push_str(last_line.trim());
        }
    }

    Ok(summary)
}
