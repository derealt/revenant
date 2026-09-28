//! `rvn` CLI - the human interface to the REVENANT daemon
//!
//! This is a separate binary (`rvn`) that communicates with the daemon
//! via SQLite (read-only), PID files, and launchctl. It never runs the
//! daemon itself - it controls and queries it.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::process::Command;

// ─── CLI Definition ──────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "rvn",
    about = "REVENANT - cognitive context restoration",
    long_about = "Control the REVENANT daemon and query your context history.\n\
                  REVENANT captures what you were doing when you leave a task\n\
                  and restores it as ghost annotations when you return.",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize REVENANT: create directories, config, LaunchAgent, shell hook
    Init,

    /// Show daemon status, uptime, and latest context card
    Status,

    /// View context card history
    History {
        /// Filter by project directory (defaults to current directory)
        #[arg(short, long)]
        project: Option<String>,

        /// Number of cards to show
        #[arg(short = 'n', long, default_value = "10")]
        count: usize,
    },

    /// Stop the daemon (launchctl unload)
    Off,

    /// Start the daemon (launchctl load)
    On,

    /// Delete context cards from the database
    Forget {
        /// Project directory to forget (defaults to current directory)
        #[arg(short, long)]
        project: Option<String>,

        /// Forget ALL context cards across all projects
        #[arg(long)]
        all: bool,
    },

    /// Setup a ghost channel or LLM compression
    Setup {
        /// What to set up: llm, vscode, browser, obsidian, slack
        channel: String,

        /// API key (for llm setup)
        #[arg(long)]
        key: Option<String>,

        /// Provider: claude, openai, ollama (for llm setup)
        #[arg(long)]
        provider: Option<String>,

        /// Model name (for llm setup)
        #[arg(long)]
        model: Option<String>,

        /// Vault path (for obsidian setup)
        #[arg(long)]
        vault: Option<String>,

        /// Bot token (for slack setup)
        #[arg(long)]
        token: Option<String>,

        /// Slack user ID (for slack setup)
        #[arg(long)]
        user: Option<String>,

        /// Slack channel ID (for slack setup)
        #[arg(long, name = "channel-id")]
        channel_id: Option<String>,
    },

    /// Inject a synthetic context card and dispatch to all enabled ghosts
    Test,

    /// Clear the current ghost immediately
    Clear,

    /// Show a weekly attention digest across all projects
    Digest {
        /// Number of days to look back (default: 7)
        #[arg(short, long, default_value = "7")]
        days: u64,
    },
}

// ─── Paths ───────────────────────────────────────────────────────────

fn home_dir() -> PathBuf {
    dirs::home_dir().expect("could not determine home directory")
}

fn revenant_dir() -> PathBuf {
    home_dir().join(".revenant")
}

// Always ~/.config/revenant, on every platform. dirs::config_dir() would
// give ~/Library/Application Support on macOS, which breaks the shell hook
// (spaces in path) and contradicts the documented layout.
fn config_dir() -> PathBuf {
    home_dir().join(".config").join("revenant")
}

fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

fn db_path() -> PathBuf {
    // Read from config if it exists, otherwise default
    if let Ok(contents) = std::fs::read_to_string(config_file()) {
        if let Ok(parsed) = contents.parse::<toml::Table>() {
            if let Some(storage) = parsed.get("storage").and_then(|v| v.as_table()) {
                if let Some(path) = storage.get("db_path").and_then(|v| v.as_str()) {
                    return expand_path(path);
                }
            }
        }
    }
    revenant_dir().join("revenant.db")
}

fn pid_file() -> PathBuf {
    if let Ok(contents) = std::fs::read_to_string(config_file()) {
        if let Ok(parsed) = contents.parse::<toml::Table>() {
            if let Some(daemon) = parsed.get("daemon").and_then(|v| v.as_table()) {
                if let Some(path) = daemon.get("pid_file").and_then(|v| v.as_str()) {
                    return expand_path(path);
                }
            }
        }
    }
    revenant_dir().join("revenant.pid")
}

#[cfg(target_os = "macos")]
fn plist_label() -> &'static str {
    "com.revenant.daemon"
}

#[cfg(target_os = "macos")]
fn plist_path() -> PathBuf {
    home_dir()
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", plist_label()))
}

fn expand_path(path: &str) -> PathBuf {
    if path.starts_with("~/") {
        home_dir().join(&path[2..])
    } else {
        PathBuf::from(path)
    }
}

// ─── SQLite Read Helpers ─────────────────────────────────────────────
// Mirrors store.rs ContextCard but standalone for the CLI binary

#[derive(Debug)]
struct ContextCard {
    id: String,
    project_dir: String,
    project_name: String,
    summary: String,
    next_step: String,
    created_at: DateTime<Utc>,
    ttl_seconds: u64,
}

fn open_db_readonly() -> Result<Connection> {
    let path = db_path();
    if !path.exists() {
        bail!(
            "no database found at {}. Is the daemon running? Run `rvn init` first.",
            path.display()
        );
    }
    let conn = Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening database at {}", path.display()))?;

    conn.execute_batch("PRAGMA busy_timeout = 3000;")?;
    Ok(conn)
}

fn row_to_card(row: &rusqlite::Row) -> rusqlite::Result<ContextCard> {
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
        ttl_seconds: row.get::<_, i64>(6)? as u64,
    })
}

fn query_cards(conn: &Connection, project_dir: &str, limit: usize) -> Result<Vec<ContextCard>> {
    let mut stmt = conn.prepare(
        "SELECT id, project_dir, project_name, summary, next_step, created_at, ttl_seconds
         FROM context_cards
         WHERE project_dir = ?1
         ORDER BY created_at DESC
         LIMIT ?2",
    )?;

    let cards = stmt
        .query_map(params![project_dir, limit as i64], row_to_card)?
        .filter_map(|r| r.ok())
        .collect();

    Ok(cards)
}

fn query_latest(conn: &Connection, project_dir: &str) -> Result<Option<ContextCard>> {
    let cards = query_cards(conn, project_dir, 1)?;
    Ok(cards.into_iter().next())
}

fn query_all_cards(conn: &Connection, limit: usize) -> Result<Vec<ContextCard>> {
    let mut stmt = conn.prepare(
        "SELECT id, project_dir, project_name, summary, next_step, created_at, ttl_seconds
         FROM context_cards
         ORDER BY created_at DESC
         LIMIT ?1",
    )?;

    let cards = stmt
        .query_map(params![limit as i64], row_to_card)?
        .filter_map(|r| r.ok())
        .collect();

    Ok(cards)
}

fn total_cards(conn: &Connection) -> Result<usize> {
    let count: i64 =
        conn.query_row("SELECT COUNT(*) FROM context_cards", [], |row| row.get(0))?;
    Ok(count as usize)
}

fn known_projects(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT DISTINCT project_dir FROM context_cards ORDER BY project_dir")?;
    let projects = stmt
        .query_map([], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(projects)
}

// ─── Daemon Control ──────────────────────────────────────────────────

fn daemon_pid() -> Option<u32> {
    let path = pid_file();
    if !path.exists() {
        return None;
    }
    let contents = std::fs::read_to_string(&path).ok()?;
    contents.trim().parse::<u32>().ok()
}

fn is_pid_alive(pid: u32) -> bool {
    // kill -0 checks if process exists without sending a signal
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn daemon_uptime(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "etime="])
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

// ─── Daemon Lifecycle (platform-specific) ────────────────────────────
// macOS: LaunchAgent + launchctl. Linux: systemd user unit + systemctl.

#[cfg(target_os = "macos")]
fn daemon_start() -> Result<()> {
    let plist = plist_path();
    if !plist.exists() {
        bail!(
            "LaunchAgent plist not found at {}. Run `rvn init` first.",
            plist.display()
        );
    }
    let output = Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&plist)
        .output()
        .context("failed to run launchctl load")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("already loaded") || stderr.contains("service already loaded") {
            println!("\x1b[33mrevenant is already running\x1b[0m");
        } else {
            bail!("launchctl load failed: {}", stderr);
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn daemon_stop() -> Result<()> {
    let plist = plist_path();
    if !plist.exists() {
        bail!(
            "LaunchAgent plist not found at {}. Run `rvn init` first.",
            plist.display()
        );
    }
    let output = Command::new("launchctl")
        .args(["unload", "-w"])
        .arg(&plist)
        .output()
        .context("failed to run launchctl unload")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Could not find") || stderr.contains("not loaded") {
            println!("\x1b[33mrevenant is not currently running\x1b[0m");
        } else {
            bail!("launchctl unload failed: {}", stderr);
        }
    }
    Ok(())
}

/// Write the LaunchAgent plist; returns the path for init's report
#[cfg(target_os = "macos")]
fn install_daemon_service(daemon_bin: &Path, log_dir: &Path) -> Result<PathBuf> {
    let plist = plist_path();
    let plist_dir = plist.parent().unwrap();
    std::fs::create_dir_all(plist_dir)?;

    let plist_content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>{log_dir}/revenant.out.log</string>
    <key>StandardErrorPath</key>
    <string>{log_dir}/revenant.err.log</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>RUST_LOG</key>
        <string>revenant=info</string>
    </dict>
    <key>ProcessType</key>
    <string>Background</string>
    <key>LowPriorityIO</key>
    <true/>
    <key>Nice</key>
    <integer>10</integer>
</dict>
</plist>"#,
        label = plist_label(),
        bin = daemon_bin.display(),
        log_dir = log_dir.display(),
    );

    std::fs::write(&plist, &plist_content)?;
    Ok(plist)
}

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> PathBuf {
    home_dir().join(".config/systemd/user/revenant.service")
}

#[cfg(target_os = "linux")]
fn daemon_start() -> Result<()> {
    if !systemd_unit_path().exists() {
        bail!(
            "systemd unit not found at {}. Run `rvn init` first.",
            systemd_unit_path().display()
        );
    }
    let output = Command::new("systemctl")
        .args(["--user", "enable", "--now", "revenant.service"])
        .output()
        .context("failed to run systemctl")?;
    if !output.status.success() {
        bail!(
            "systemctl enable --now failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn daemon_stop() -> Result<()> {
    let output = Command::new("systemctl")
        .args(["--user", "disable", "--now", "revenant.service"])
        .output()
        .context("failed to run systemctl")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not loaded") || stderr.contains("does not exist") {
            println!("\x1b[33mrevenant is not currently running\x1b[0m");
        } else {
            bail!("systemctl disable --now failed: {}", stderr);
        }
    }
    Ok(())
}

/// Write the systemd user unit; returns the path for init's report
#[cfg(target_os = "linux")]
fn install_daemon_service(daemon_bin: &Path, log_dir: &Path) -> Result<PathBuf> {
    let unit = systemd_unit_path();
    std::fs::create_dir_all(unit.parent().unwrap())?;

    let unit_content = format!(
        "[Unit]\n\
         Description=REVENANT cognitive context daemon\n\n\
         [Service]\n\
         ExecStart={bin}\n\
         Restart=on-failure\n\
         Environment=RUST_LOG=revenant=info\n\
         StandardOutput=append:{log_dir}/revenant.out.log\n\
         StandardError=append:{log_dir}/revenant.err.log\n\
         Nice=10\n\n\
         [Install]\n\
         WantedBy=default.target\n",
        bin = daemon_bin.display(),
        log_dir = log_dir.display(),
    );

    std::fs::write(&unit, unit_content)?;
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();
    Ok(unit)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn daemon_start() -> Result<()> {
    bail!("daemon lifecycle is not supported on this platform yet")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn daemon_stop() -> Result<()> {
    bail!("daemon lifecycle is not supported on this platform yet")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn install_daemon_service(_daemon_bin: &Path, _log_dir: &Path) -> Result<PathBuf> {
    bail!("daemon lifecycle is not supported on this platform yet")
}

// ─── Command Implementations ─────────────────────────────────────────

fn cmd_init() -> Result<()> {
    let rev_dir = revenant_dir();
    let cfg_dir = config_dir();

    println!("\x1b[2m\u{250c}\u{2500}\u{2500}\u{2500} REVENANT INIT \u{2500}\u{2500}\u{2500}\x1b[0m");

    // 1. Create ~/.revenant/
    std::fs::create_dir_all(&rev_dir)?;
    println!("\x1b[2m\u{2502}\x1b[0m \x1b[32m+\x1b[0m {}", rev_dir.display());

    // 2. Create ~/.config/revenant/
    std::fs::create_dir_all(&cfg_dir)?;
    println!("\x1b[2m\u{2502}\x1b[0m \x1b[32m+\x1b[0m {}", cfg_dir.display());

    // 3. Copy default config if none exists
    let cfg_file = config_file();
    if !cfg_file.exists() {
        let default_config = include_str!("../config.default.toml");
        std::fs::write(&cfg_file, default_config)?;
        println!("\x1b[2m\u{2502}\x1b[0m \x1b[32m+\x1b[0m {} (default config)", cfg_file.display());
    } else {
        println!(
            "\x1b[2m\u{2502}\x1b[0m \x1b[33m~\x1b[0m {} (already exists)",
            cfg_file.display()
        );
    }

    // 4. Copy shell integration scripts
    let zsh_source = include_str!("../../ghosts/terminal/revenant.zsh");
    let bash_source = include_str!("../../ghosts/terminal/revenant.bash");
    let fish_source = include_str!("../../ghosts/terminal/revenant.fish");

    let zsh_dest = cfg_dir.join("revenant.zsh");
    let bash_dest = cfg_dir.join("revenant.bash");
    let fish_dest = cfg_dir.join("revenant.fish");

    std::fs::write(&zsh_dest, zsh_source)?;
    std::fs::write(&bash_dest, bash_source)?;
    std::fs::write(&fish_dest, fish_source)?;
    println!("\x1b[2m\u{2502}\x1b[0m \x1b[32m+\x1b[0m shell scripts installed to {}", cfg_dir.display());

    // 5. Find the daemon binary path
    let daemon_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("revenant")))
        .unwrap_or_else(|| PathBuf::from("/usr/local/bin/revenant"));

    // 6. Install the daemon service (LaunchAgent on macOS, systemd user
    //    unit on Linux)
    let log_dir = rev_dir.join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let service_path = install_daemon_service(&daemon_bin, &log_dir)?;
    println!("\x1b[2m\u{2502}\x1b[0m \x1b[32m+\x1b[0m {}", service_path.display());

    // 7. Add shell hooks: zsh and bash, whichever the user has
    add_shell_hook(
        &home_dir().join(".zshrc"),
        "revenant.zsh",
        "[ -f ~/.config/revenant/revenant.zsh ] && source ~/.config/revenant/revenant.zsh",
        cfg!(target_os = "macos"), // zsh is the macOS default: create if missing
    )?;
    add_shell_hook(
        &home_dir().join(".bashrc"),
        "revenant.bash",
        "[ -f ~/.config/revenant/revenant.bash ] && source ~/.config/revenant/revenant.bash",
        cfg!(target_os = "linux"), // bash is the usual Linux default
    )?;

    // 8. Start the daemon
    println!("\x1b[2m\u{2502}\x1b[0m");
    println!("\x1b[2m\u{2502}\x1b[0m loading daemon...");
    match daemon_start() {
        Ok(()) => println!("\x1b[2m\u{2502}\x1b[0m \x1b[32m+\x1b[0m daemon loaded"),
        Err(e) => println!("\x1b[2m\u{2502}\x1b[0m \x1b[31m!\x1b[0m daemon load failed: {e}"),
    }

    println!("\x1b[2m\u{2514}\u{2500}\u{2500}\u{2500} revenant is awake \u{2500}\u{2500}\u{2500}\x1b[0m");
    println!();
    println!("  Open a new terminal to activate the shell hook.");
    println!("  Run \x1b[36mrvn status\x1b[0m to verify the daemon is running.");
    println!("  Run \x1b[36mrvn test\x1b[0m to inject a synthetic ghost.");

    Ok(())
}

fn cmd_status() -> Result<()> {
    println!("\x1b[2m\u{250c}\u{2500}\u{2500}\u{2500} REVENANT STATUS \u{2500}\u{2500}\u{2500}\x1b[0m");

    // Check PID
    match daemon_pid() {
        Some(pid) if is_pid_alive(pid) => {
            let uptime = daemon_uptime(pid).unwrap_or_else(|| "unknown".to_string());
            println!(
                "\x1b[2m\u{2502}\x1b[0m \x1b[32mdaemon\x1b[0m  running (pid {pid}, uptime {uptime})"
            );
        }
        Some(pid) => {
            println!(
                "\x1b[2m\u{2502}\x1b[0m \x1b[31mdaemon\x1b[0m  dead (stale pid {pid})"
            );
        }
        None => {
            println!("\x1b[2m\u{2502}\x1b[0m \x1b[31mdaemon\x1b[0m  not running");
        }
    }

    // Database stats
    let db = db_path();
    if db.exists() {
        match open_db_readonly() {
            Ok(conn) => {
                let total = total_cards(&conn).unwrap_or(0);
                let projects = known_projects(&conn).unwrap_or_default();
                println!(
                    "\x1b[2m\u{2502}\x1b[0m \x1b[36mstore\x1b[0m   {} context cards across {} projects",
                    total,
                    projects.len()
                );

                // Show latest card for current directory
                let cwd = std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();

                if !cwd.is_empty() {
                    let matching_project = projects.iter().find(|p| cwd.starts_with(p.as_str()));

                    if let Some(project_dir) = matching_project {
                        if let Ok(Some(card)) = query_latest(&conn, project_dir) {
                            let age = format_age_ago(card.created_at);
                            println!("\x1b[2m\u{2502}\x1b[0m");
                            println!(
                                "\x1b[2m\u{2502}\x1b[0m \x1b[2mlatest ({}, {}):\x1b[0m",
                                card.project_name, age
                            );
                            println!("\x1b[2m\u{2502}\x1b[0m \x1b[36m{}\x1b[0m", card.summary);
                            if !card.next_step.is_empty() {
                                println!(
                                    "\x1b[2m\u{2502}\x1b[0m \x1b[33mNext: {}\x1b[0m",
                                    card.next_step
                                );
                            }
                        }
                    }
                }
            }
            Err(e) => {
                println!("\x1b[2m\u{2502}\x1b[0m \x1b[31mstore\x1b[0m   error: {e}");
            }
        }
    } else {
        println!(
            "\x1b[2m\u{2502}\x1b[0m \x1b[33mstore\x1b[0m   no database yet ({})",
            db.display()
        );
    }

    // Ghost channels
    println!("\x1b[2m\u{2502}\x1b[0m");
    let rev = revenant_dir();
    print_channel_status("terminal", rev.join("motd").exists());
    print_channel_status("vscode", rev.join("vscode.sock").exists());
    print_channel_status("slack", rev.join("slack.sock").exists());
    print_channel_status("obsidian", rev.join("obsidian-state.json").exists());
    print_channel_status("browser", ghost_server_reachable());

    println!("\x1b[2m\u{2514}\u{2500}\u{2500}\u{2500}\x1b[0m");
    Ok(())
}

/// The daemon serves the browser ghost over localhost HTTP; reachable = active
fn ghost_server_reachable() -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], 7711)),
        std::time::Duration::from_millis(300),
    )
    .is_ok()
}

/// POST to the daemon's ghost HTTP server (used by clear/test for the browser channel)
fn ghost_server_post(path: &str, body: &str) -> Result<()> {
    use std::io::{Read, Write};
    let timeout = std::time::Duration::from_millis(500);
    let mut stream = std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], 7711)),
        timeout,
    )?;
    stream.set_write_timeout(Some(timeout))?;
    stream.set_read_timeout(Some(timeout))?;
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:7711\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    Ok(())
}

fn print_channel_status(name: &str, active: bool) {
    let indicator = if active {
        "\x1b[32m*\x1b[0m"
    } else {
        "\x1b[2m-\x1b[0m"
    };
    let status = if active { "active" } else { "inactive" };
    println!("\x1b[2m\u{2502}\x1b[0m {indicator} {name:<10} {status}");
}

fn cmd_history(project: Option<String>, count: usize) -> Result<()> {
    let conn = open_db_readonly()?;

    let cards = match project {
        Some(ref dir) => {
            let abs = if dir.starts_with('/') {
                dir.clone()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(dir).to_string_lossy().to_string())
                    .unwrap_or_else(|_| dir.clone())
            };
            query_cards(&conn, &abs, count)?
        }
        None => {
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();

            let cards = query_cards(&conn, &cwd, count)?;
            if cards.is_empty() {
                // Check if cwd is inside a known project
                let projects = known_projects(&conn)?;
                let matching = projects.iter().find(|p| cwd.starts_with(p.as_str()));
                if let Some(project_dir) = matching {
                    query_cards(&conn, project_dir, count)?
                } else {
                    // Show all cards across projects
                    query_all_cards(&conn, count)?
                }
            } else {
                cards
            }
        }
    };

    if cards.is_empty() {
        println!("\x1b[2mno context cards found\x1b[0m");
        return Ok(());
    }

    println!(
        "\x1b[2m\u{250c}\u{2500}\u{2500}\u{2500} REVENANT HISTORY ({} cards) \u{2500}\u{2500}\u{2500}\x1b[0m",
        cards.len()
    );

    for (i, card) in cards.iter().enumerate() {
        let age = format_age_ago(card.created_at);
        println!("\x1b[2m\u{2502}\x1b[0m");
        println!(
            "\x1b[2m\u{2502}\x1b[0m \x1b[2m[{}]\x1b[0m \x1b[35m{}\x1b[0m \x1b[2m({}) {}\x1b[0m",
            i + 1,
            card.project_name,
            card.project_dir,
            age
        );
        println!("\x1b[2m\u{2502}\x1b[0m   \x1b[36m{}\x1b[0m", card.summary);
        if !card.next_step.is_empty() {
            println!("\x1b[2m\u{2502}\x1b[0m   \x1b[33mNext: {}\x1b[0m", card.next_step);
        }
        println!(
            "\x1b[2m\u{2502}\x1b[0m   \x1b[2mid: {} | ttl: {}s\x1b[0m",
            &card.id[..std::cmp::min(8, card.id.len())],
            card.ttl_seconds
        );
    }

    println!("\x1b[2m\u{2514}\u{2500}\u{2500}\u{2500}\x1b[0m");
    Ok(())
}

/// Append a guarded source line to a shell rc file, once
fn add_shell_hook(
    rc_file: &Path,
    marker: &str,
    hook_line: &str,
    create_if_missing: bool,
) -> Result<()> {
    if !rc_file.exists() && !create_if_missing {
        return Ok(());
    }
    let already_hooked = rc_file.exists()
        && std::fs::read_to_string(rc_file)
            .map(|c| c.contains(marker))
            .unwrap_or(false);

    if already_hooked {
        println!(
            "\x1b[2m\u{2502}\x1b[0m \x1b[33m~\x1b[0m shell hook already in {}",
            rc_file.display()
        );
        return Ok(());
    }

    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(rc_file)?;
    writeln!(file)?;
    writeln!(file, "# REVENANT - cognitive context restoration")?;
    writeln!(file, "{}", hook_line)?;
    println!(
        "\x1b[2m\u{2502}\x1b[0m \x1b[32m+\x1b[0m shell hook added to {}",
        rc_file.display()
    );
    Ok(())
}

fn cmd_off() -> Result<()> {
    daemon_stop()?;
    println!("\x1b[2mrevenant resting\x1b[0m");
    Ok(())
}

fn cmd_on() -> Result<()> {
    daemon_start()?;
    println!("\x1b[2mrevenant waking\x1b[0m");

    // Wait briefly and verify
    std::thread::sleep(std::time::Duration::from_secs(1));
    match daemon_pid() {
        Some(pid) if is_pid_alive(pid) => {
            println!("\x1b[32mdaemon running\x1b[0m (pid {pid})");
        }
        _ => {
            println!("\x1b[33mdaemon may still be starting...\x1b[0m");
            println!("run \x1b[36mrvn status\x1b[0m in a moment to check");
        }
    }
    Ok(())
}

fn cmd_forget(project: Option<String>, all: bool) -> Result<()> {
    let db = db_path();
    if !db.exists() {
        bail!("no database found at {}", db.display());
    }

    // Open read-write for deletion
    let conn = Connection::open(&db).context("opening database")?;
    conn.execute_batch("PRAGMA busy_timeout = 3000;")?;

    if all {
        print!("\x1b[33mdelete ALL context cards? [y/N] \x1b[0m");
        use std::io::{self, BufRead, Write};
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().lock().read_line(&mut answer)?;

        if answer.trim().to_lowercase() != "y" {
            println!("cancelled");
            return Ok(());
        }

        let deleted = conn.execute("DELETE FROM context_cards", [])?;
        conn.execute("DELETE FROM ghost_log", [])?;
        conn.execute("DELETE FROM project_state", [])?;
        println!("\x1b[2mforgotten {deleted} context cards\x1b[0m");
    } else {
        let project_dir = match project {
            Some(dir) => {
                if dir.starts_with('/') {
                    dir
                } else {
                    std::env::current_dir()
                        .map(|cwd| cwd.join(&dir).to_string_lossy().to_string())
                        .unwrap_or(dir)
                }
            }
            None => std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .context("could not determine current directory")?,
        };

        let deleted = conn.execute(
            "DELETE FROM context_cards WHERE project_dir = ?1",
            params![project_dir],
        )?;
        conn.execute(
            "DELETE FROM ghost_log WHERE card_id NOT IN (SELECT id FROM context_cards)",
            [],
        )?;
        conn.execute(
            "DELETE FROM project_state WHERE project_dir = ?1",
            params![project_dir],
        )?;

        if deleted > 0 {
            println!("\x1b[2mforgotten {deleted} context cards for {project_dir}\x1b[0m");
        } else {
            println!("\x1b[2mno context cards found for {project_dir}\x1b[0m");
        }
    }

    Ok(())
}

fn read_stdin_line(prompt: &str) -> String {
    eprint!("{}", prompt);
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap_or_default();
    input.trim().to_string()
}

fn update_config_value(key_path: &str, value: &str) -> Result<()> {
    let config = config_file();
    let contents = std::fs::read_to_string(&config).unwrap_or_default();
    let mut lines: Vec<String> = contents.lines().map(String::from).collect();

    // Simple key=value replacement in TOML
    let parts: Vec<&str> = key_path.split('.').collect();
    let section = if parts.len() > 1 { parts[..parts.len()-1].join(".") } else { String::new() };
    let key = parts.last().unwrap_or(&"");

    let section_header = if section.is_empty() { String::new() } else { format!("[{}]", section) };
    let mut in_section = section.is_empty();
    let mut found = false;

    for line in lines.iter_mut() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == section_header;
        }
        if in_section && trimmed.starts_with(&format!("{} =", key)) || in_section && trimmed.starts_with(&format!("{}=", key)) {
            *line = format!("{} = {}", key, value);
            found = true;
            break;
        }
    }

    if !found {
        if section.is_empty() {
            lines.push(format!("{} = {}", key, value));
        } else if let Some(header_idx) = lines.iter().position(|l| l.trim() == section_header) {
            // Insert right after the existing section header, NOT at end of
            // file where it would land in whatever section comes last
            lines.insert(header_idx + 1, format!("{} = {}", key, value));
        } else {
            lines.push(String::new());
            lines.push(section_header);
            lines.push(format!("{} = {}", key, value));
        }
    }

    std::fs::write(&config, lines.join("\n") + "\n")?;
    Ok(())
}

fn cmd_setup(
    channel: &str,
    key: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    vault: Option<String>,
    token: Option<String>,
    user: Option<String>,
    channel_id: Option<String>,
) -> Result<()> {
    match channel {
        "llm" => {
            println!("\x1b[2m┌─── LLM Compression Setup ───\x1b[0m");
            println!("\x1b[2m│\x1b[0m");
            println!("\x1b[2m│\x1b[0m LLM compression turns raw signals into natural-language");
            println!("\x1b[2m│\x1b[0m context cards. Without it, cards are rule-based (still works,");
            println!("\x1b[2m│\x1b[0m just less eloquent). Your data never touches REVENANT servers");
            println!("\x1b[2m│\x1b[0m - the API call goes directly from your machine to the provider.");
            println!("\x1b[2m│\x1b[0m");
            println!("\x1b[2m│\x1b[0m Providers:");
            println!("\x1b[2m│\x1b[0m   \x1b[36mclaude\x1b[0m   - Best quality. ~$0.50/month (Haiku) or ~$2/month (Sonnet)");
            println!("\x1b[2m│\x1b[0m   \x1b[36mopenai\x1b[0m   - Good quality. ~$0.30/month (GPT-4o-mini)");
            println!("\x1b[2m│\x1b[0m   \x1b[36mollama\x1b[0m   - Free. Runs locally. Needs ~4GB disk.");
            println!("\x1b[2m│\x1b[0m");

            let prov = provider.unwrap_or_else(|| read_stdin_line("\x1b[2m│\x1b[0m Provider (claude/openai/ollama): "));

            if prov.is_empty() {
                bail!("No provider specified. Run: rvn setup llm --provider claude --key YOUR_KEY");
            }

            match prov.as_str() {
                "claude" => {
                    let api_key = key.unwrap_or_else(|| read_stdin_line("\x1b[2m│\x1b[0m API key (sk-ant-...): "));
                    if api_key.is_empty() {
                        bail!("No API key provided.");
                    }
                    let mdl = model.unwrap_or_else(|| "claude-haiku-4-5-20251001".to_string());
                    update_config_value("compressor.mode", "\"llm\"")?;
                    update_config_value("compressor.llm.provider", "\"claude\"")?;
                    update_config_value("compressor.llm.model", &format!("\"{}\"", mdl))?;
                    update_config_value("compressor.llm.api_key", &format!("\"{}\"", api_key))?;
                    update_config_value("compressor.llm.endpoint", "\"https://api.anthropic.com/v1/messages\"")?;
                    println!("\x1b[2m│\x1b[0m");
                    println!("\x1b[2m│\x1b[0m \x1b[32m✓\x1b[0m LLM compression enabled: Claude {}", mdl);
                }
                "openai" => {
                    let api_key = key.unwrap_or_else(|| read_stdin_line("\x1b[2m│\x1b[0m API key (sk-...): "));
                    if api_key.is_empty() {
                        bail!("No API key provided.");
                    }
                    let mdl = model.unwrap_or_else(|| "gpt-4o-mini".to_string());
                    update_config_value("compressor.mode", "\"llm\"")?;
                    update_config_value("compressor.llm.provider", "\"openai\"")?;
                    update_config_value("compressor.llm.model", &format!("\"{}\"", mdl))?;
                    update_config_value("compressor.llm.api_key", &format!("\"{}\"", api_key))?;
                    update_config_value("compressor.llm.endpoint", "\"https://api.openai.com/v1/chat/completions\"")?;
                    println!("\x1b[2m│\x1b[0m");
                    println!("\x1b[2m│\x1b[0m \x1b[32m✓\x1b[0m LLM compression enabled: OpenAI {}", mdl);
                }
                "ollama" => {
                    let mdl = model.unwrap_or_else(|| "llama3.2:3b".to_string());
                    update_config_value("compressor.mode", "\"llm\"")?;
                    update_config_value("compressor.llm.provider", "\"ollama\"")?;
                    update_config_value("compressor.llm.model", &format!("\"{}\"", mdl))?;
                    update_config_value("compressor.llm.api_key", "\"\"")?;
                    update_config_value("compressor.llm.endpoint", "\"http://localhost:11434/api/generate\"")?;
                    println!("\x1b[2m│\x1b[0m");
                    println!("\x1b[2m│\x1b[0m \x1b[32m✓\x1b[0m LLM compression enabled: Ollama {}", mdl);
                    println!("\x1b[2m│\x1b[0m   Make sure Ollama is running: ollama serve");
                    println!("\x1b[2m│\x1b[0m   Pull the model: ollama pull {}", mdl);
                }
                other => bail!("Unknown provider: '{}'. Use: claude, openai, or ollama", other),
            }

            println!("\x1b[2m│\x1b[0m");
            println!("\x1b[2m│\x1b[0m Restart the daemon to apply: \x1b[36mrvn off && rvn on\x1b[0m");
            println!("\x1b[2m└───\x1b[0m");
        }
        "vscode" => {
            println!("\x1b[2m\u{250c}\u{2500}\u{2500}\u{2500} VS Code Ghost Setup \u{2500}\u{2500}\u{2500}\x1b[0m");
            println!("\x1b[2m\u{2502}\x1b[0m");
            println!("\x1b[2m\u{2502}\x1b[0m The VS Code extension communicates with the daemon");
            println!("\x1b[2m\u{2502}\x1b[0m via a Unix socket at ~/.revenant/vscode.sock.");
            println!("\x1b[2m\u{2502}\x1b[0m");

            let vsix = format!("revenant-ghost-{}.vsix", env!("CARGO_PKG_VERSION"));
            println!("\x1b[2m\u{2502}\x1b[0m Install the extension (VS Code; use `cursor` for Cursor):");
            println!("\x1b[2m\u{2502}\x1b[0m   curl -fsSLO {}", release_asset_url(&vsix));
            println!("\x1b[2m\u{2502}\x1b[0m   code --install-extension {vsix}");
            println!("\x1b[2m\u{2502}\x1b[0m Then reload the editor window. The ghost binds on startup.");

            let ext_dir = find_project_root().map(|r| r.join("ghosts/vscode"));
            if let Ok(ref dir) = ext_dir {
                if dir.exists() {
                    println!("\x1b[2m\u{2502}\x1b[0m");
                    println!("\x1b[2m\u{2502}\x1b[0m Or build from this checkout:");
                    println!(
                        "\x1b[2m\u{2502}\x1b[0m   cd {} && npm install && npm run build && npx @vscode/vsce package",
                        dir.display()
                    );
                }
            }

            println!("\x1b[2m\u{2502}\x1b[0m");
            println!("\x1b[2m\u{2502}\x1b[0m Ensure this is in your config.toml:");
            println!("\x1b[2m\u{2502}\x1b[0m   [ghosts]");
            println!("\x1b[2m\u{2502}\x1b[0m   vscode_enabled = true");
            println!("\x1b[2m\u{2502}\x1b[0m   [ghosts.vscode]");
            println!("\x1b[2m\u{2502}\x1b[0m   socket_path = \"~/.revenant/vscode.sock\"");
            println!("\x1b[2m\u{2514}\u{2500}\u{2500}\u{2500}\x1b[0m");
        }

        "browser" => {
            println!("\x1b[2m\u{250c}\u{2500}\u{2500}\u{2500} Browser Ghost Setup \u{2500}\u{2500}\u{2500}\x1b[0m");
            println!("\x1b[2m\u{2502}\x1b[0m");
            println!("\x1b[2m\u{2502}\x1b[0m The extension polls the daemon at http://127.0.0.1:7711/ghost");
            println!("\x1b[2m\u{2502}\x1b[0m and shows the context card as a banner. No native messaging,");
            println!("\x1b[2m\u{2502}\x1b[0m no other setup on the daemon side.");
            println!("\x1b[2m\u{2502}\x1b[0m");
            let zip = format!("revenant-chrome-extension-{}.zip", env!("CARGO_PKG_VERSION"));
            println!("\x1b[2m\u{2502}\x1b[0m Install the extension:");
            println!("\x1b[2m\u{2502}\x1b[0m   curl -fsSLO {}", release_asset_url(&zip));
            println!("\x1b[2m\u{2502}\x1b[0m   unzip {zip} -d ~/.revenant/chrome-extension");
            println!("\x1b[2m\u{2502}\x1b[0m Then open chrome://extensions, turn on Developer mode,");
            println!("\x1b[2m\u{2502}\x1b[0m click Load unpacked and pick ~/.revenant/chrome-extension");

            let ext_dir = find_project_root()
                .ok()
                .map(|r| r.join("ghosts/browser"));
            if let Some(ref dir) = ext_dir {
                if dir.exists() {
                    println!("\x1b[2m\u{2502}\x1b[0m");
                    println!("\x1b[2m\u{2502}\x1b[0m Or build from this checkout:");
                    println!(
                        "\x1b[2m\u{2502}\x1b[0m   cd {} && npm install && npm run package   (load dist/)",
                        dir.display()
                    );
                }
            }
            println!("\x1b[2m\u{2502}\x1b[0m");
            println!("\x1b[2m\u{2502}\x1b[0m The extension only draws the banner. It reads nothing from");
            println!("\x1b[2m\u{2502}\x1b[0m the pages you visit.");
            println!("\x1b[2m\u{2514}\u{2500}\u{2500}\u{2500}\x1b[0m");
        }

        "obsidian" => {
            println!("\x1b[2m\u{250c}\u{2500}\u{2500}\u{2500} Obsidian Ghost Setup \u{2500}\u{2500}\u{2500}\x1b[0m");
            println!("\x1b[2m\u{2502}\x1b[0m");
            println!("\x1b[2m\u{2502}\x1b[0m The Obsidian plugin polls ~/.revenant/obsidian-state.json");
            println!("\x1b[2m\u{2502}\x1b[0m for context cards and renders them as transient callout blocks.");
            println!("\x1b[2m\u{2502}\x1b[0m");

            let ext_dir = find_project_root()
                .ok()
                .map(|r| r.join("ghosts/obsidian"));
            if let Some(ref dir) = ext_dir {
                if dir.exists() {
                    println!("\x1b[2m\u{2502}\x1b[0m To install:");
                    println!(
                        "\x1b[2m\u{2502}\x1b[0m   cd {} && npm install && npm run build",
                        dir.display()
                    );
                    println!("\x1b[2m\u{2502}\x1b[0m   Copy main.js + manifest.json to <vault>/.obsidian/plugins/revenant-ghost/");
                }
            }

            println!("\x1b[2m\u{2502}\x1b[0m");
            if let Some(ref vault) = vault {
                update_config_value("ghosts.obsidian_enabled", "true")?;
                update_config_value("ghosts.obsidian.vault_path", &format!("\"{}\"", vault))?;
                println!("\x1b[2m\u{2502}\x1b[0m \x1b[32m✓\x1b[0m obsidian ghost enabled for vault: {vault}");
                println!("\x1b[2m\u{2502}\x1b[0m Restart the daemon to apply: \x1b[36mrvn off && rvn on\x1b[0m");
            } else {
                println!("\x1b[2m\u{2502}\x1b[0m Enable it with your vault path:");
                println!("\x1b[2m\u{2502}\x1b[0m   rvn setup obsidian --vault /path/to/your/vault");
                println!("\x1b[2m\u{2502}\x1b[0m");
                println!("\x1b[2m\u{2502}\x1b[0m Or in config.toml:");
                println!("\x1b[2m\u{2502}\x1b[0m   [ghosts]");
                println!("\x1b[2m\u{2502}\x1b[0m   obsidian_enabled = true");
                println!("\x1b[2m\u{2502}\x1b[0m   [ghosts.obsidian]");
                println!("\x1b[2m\u{2502}\x1b[0m   vault_path = \"/path/to/your/vault\"");
            }
            println!("\x1b[2m\u{2514}\u{2500}\u{2500}\u{2500}\x1b[0m");
        }

        "slack" => {
            println!("\x1b[2m\u{250c}\u{2500}\u{2500}\u{2500} Slack Ghost Setup \u{2500}\u{2500}\u{2500}\x1b[0m");
            println!("\x1b[2m\u{2502}\x1b[0m");
            println!("\x1b[2m\u{2502}\x1b[0m The Slack ghost sends context cards via a Go sidecar bot");
            println!("\x1b[2m\u{2502}\x1b[0m that communicates with the daemon over a Unix socket.");
            println!("\x1b[2m\u{2502}\x1b[0m");
            println!("\x1b[2m\u{2502}\x1b[0m 1. Create a Slack App at https://api.slack.com/apps");
            println!("\x1b[2m\u{2502}\x1b[0m 2. Add Bot Token Scopes: chat:write, chat:write.customize");
            println!("\x1b[2m\u{2502}\x1b[0m 3. Install to your workspace");
            println!("\x1b[2m\u{2502}\x1b[0m 4. Copy the Bot User OAuth Token (xoxb-...)");
            println!("\x1b[2m\u{2502}\x1b[0m 5. Find your Slack User ID (click your profile > ...)");
            println!("\x1b[2m\u{2502}\x1b[0m");
            if let (Some(ref token), Some(ref user)) = (&token, &user) {
                update_config_value("ghosts.slack_enabled", "true")?;
                update_config_value("ghosts.slack.bot_token", &format!("\"{}\"", token))?;
                update_config_value("ghosts.slack.user_id", &format!("\"{}\"", user))?;
                if let Some(ref cid) = channel_id {
                    update_config_value("ghosts.slack.channel_id", &format!("\"{}\"", cid))?;
                }
                println!("\x1b[2m\u{2502}\x1b[0m \x1b[32m✓\x1b[0m slack ghost enabled for user {user}");
                println!("\x1b[2m\u{2502}\x1b[0m Restart the daemon to apply: \x1b[36mrvn off && rvn on\x1b[0m");
            } else {
                println!("\x1b[2m\u{2502}\x1b[0m Enable it with your credentials:");
                println!("\x1b[2m\u{2502}\x1b[0m   rvn setup slack --token xoxb-... --user U0XXXXXXX --channel-id C0XXXXXXX");
                println!("\x1b[2m\u{2502}\x1b[0m");
                println!("\x1b[2m\u{2502}\x1b[0m Or in config.toml:");
                println!("\x1b[2m\u{2502}\x1b[0m   [ghosts]");
                println!("\x1b[2m\u{2502}\x1b[0m   slack_enabled = true");
                println!("\x1b[2m\u{2502}\x1b[0m   [ghosts.slack]");
                println!("\x1b[2m\u{2502}\x1b[0m   bot_token = \"xoxb-your-token\"");
                println!("\x1b[2m\u{2502}\x1b[0m   user_id = \"U0XXXXXXX\"");
                println!("\x1b[2m\u{2502}\x1b[0m   channel_id = \"C0XXXXXXX\"");
            }
            println!("\x1b[2m\u{2502}\x1b[0m");
            let sock = revenant_dir().join("slack.sock");
            println!("\x1b[2m\u{2502}\x1b[0m Socket path: {}", sock.display());
            println!("\x1b[2m\u{2502}\x1b[0m The Go bot should listen on this socket.");
            println!("\x1b[2m\u{2514}\u{2500}\u{2500}\u{2500}\x1b[0m");
        }

        other => {
            bail!(
                "unknown channel: '{other}'. Available: vscode, browser, obsidian, slack"
            );
        }
    }

    Ok(())
}

fn cmd_clear() -> Result<()> {
    let rev_dir = revenant_dir();

    // Clear terminal ghost
    let motd = rev_dir.join("motd");
    let active = rev_dir.join("terminal-active");
    let _ = std::fs::remove_file(&motd);
    let _ = std::fs::remove_file(&active);

    // Clear browser ghost (served by the daemon on localhost:7711)
    let _ = ghost_server_post("/clear", "");

    // Clear obsidian ghost
    let obsidian_state = rev_dir.join("obsidian-state.json");
    let _ = std::fs::write(&obsidian_state, r#"{"type":"clear"}"#);

    println!("\x1b[2m✓\x1b[0m ghost cleared");
    Ok(())
}

fn cmd_test() -> Result<()> {
    let db = db_path();
    let rev_dir = revenant_dir();
    std::fs::create_dir_all(&rev_dir)?;

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/tmp/test-project".to_string());

    let project_name = Path::new(&cwd)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "test-project".to_string());

    let card_id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now();

    println!("\x1b[2m\u{250c}\u{2500}\u{2500}\u{2500} REVENANT TEST \u{2500}\u{2500}\u{2500}\x1b[0m");
    println!("\x1b[2m\u{2502}\x1b[0m injecting synthetic context card...");
    println!("\x1b[2m\u{2502}\x1b[0m");

    // Ensure database exists with schema
    if let Some(parent) = db.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let conn = Connection::open(&db)?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS context_cards (
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
             cleared_at TEXT
         );
         CREATE TABLE IF NOT EXISTS project_state (
             project_dir TEXT PRIMARY KEY,
             last_branch TEXT,
             last_activity TEXT NOT NULL,
             last_card_id TEXT
         );",
    )?;

    let summary = format!(
        "You were testing the REVENANT ghost system in {project_name}. \
         This is a synthetic card injected by `rvn test` to verify all ghost channels."
    );
    let next_step = "Check each enabled ghost channel to confirm the annotation appeared.";

    conn.execute(
        "INSERT OR REPLACE INTO context_cards
            (id, project_dir, project_name, summary, next_step, created_at, signals_json, ttl_seconds)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            card_id,
            cwd,
            project_name,
            summary,
            next_step,
            now.to_rfc3339(),
            "{}",
            300i64,
        ],
    )?;
    println!("\x1b[2m\u{2502}\x1b[0m \x1b[32m+\x1b[0m context card saved to SQLite");

    // Dispatch to terminal ghost (write motd directly with premium format)
    let motd_path = rev_dir.join("motd");
    let dim = "\x1b[2m";
    let reset = "\x1b[0m";
    let white = "\x1b[97m";
    let gold = "\x1b[38;5;178m";
    let accent = "\x1b[38;5;145m"; // silver for test cards

    let motd = format!(
        "{dim}\u{2554}\u{2550}\u{2550}{reset}{accent} REVENANT \u{2502} {project} \u{2502} just now {reset}{dim} \u{2550}\u{2550}\u{2557}{reset}\n\
         {dim}\u{2551}{reset} {white}{summary}{reset}\n\
         {dim}\u{2551}{reset} {gold}\u{2192} {next}{reset}\n\
         {dim}\u{255a}\u{2550}\u{2550} ghost fades in 5min of activity \u{2550}\u{2550}\u{255d}{reset}\n",
        project = project_name,
        summary = summary,
        next = next_step,
    );
    std::fs::write(&motd_path, &motd)?;

    let meta = serde_json::json!({
        "card_id": card_id,
        "project": project_name,
        "created_at": now.to_rfc3339(),
        "ttl_seconds": 300,
        "expires_at": (now + chrono::Duration::seconds(300)).to_rfc3339(),
    });
    std::fs::write(rev_dir.join("motd.json"), serde_json::to_string_pretty(&meta)?)?;
    println!("\x1b[2m\u{2502}\x1b[0m \x1b[32m+\x1b[0m terminal ghost (motd)");

    // Dispatch to VS Code ghost via Unix socket, with the real captured
    // resume point when the extension has reported one for this project
    let vscode_sock = rev_dir.join("vscode.sock");
    let anchor = editor_anchor_for(&cwd);
    if let Some(ref a) = anchor {
        println!(
            "\x1b[2m\u{2502}\x1b[0m \x1b[32m+\x1b[0m resume anchor: {}:{}",
            a["file"].as_str().unwrap_or("?"),
            a["line"]
        );
    }
    dispatch_socket_ghost(&vscode_sock, "vscode", &card_id, &cwd, &project_name, &summary, next_step, anchor.as_ref())?;

    // Dispatch to Slack ghost via Unix socket
    let slack_sock = rev_dir.join("slack.sock");
    dispatch_socket_ghost(&slack_sock, "slack", &card_id, &cwd, &project_name, &summary, next_step, None)?;

    // Dispatch to Obsidian ghost (write state file)
    let card_json = serde_json::json!({
        "type": "inject",
        "card": {
            "id": card_id,
            "project_dir": cwd,
            "project_name": project_name,
            "summary": summary,
            "next_step": next_step,
            "ttl_seconds": 300,
        }
    });

    let obsidian_state = rev_dir.join("obsidian-state.json");
    std::fs::write(&obsidian_state, serde_json::to_string_pretty(&card_json)?)?;
    println!("\x1b[2m\u{2502}\x1b[0m \x1b[32m+\x1b[0m obsidian ghost (state file)");

    // Dispatch to browser ghost (the daemon serves it on localhost:7711)
    match ghost_server_post("/inject", &serde_json::to_string(&card_json)?) {
        Ok(()) => println!("\x1b[2m\u{2502}\x1b[0m \x1b[32m+\x1b[0m browser ghost (daemon http)"),
        Err(_) => println!("\x1b[2m\u{2502}\x1b[0m \x1b[2m-\x1b[0m browser ghost (daemon not running)"),
    }

    println!("\x1b[2m\u{2502}\x1b[0m");
    println!("\x1b[2m\u{2502}\x1b[0m card id: \x1b[2m{card_id}\x1b[0m");
    println!("\x1b[2m\u{2514}\u{2500}\u{2500}\u{2500} all ghosts dispatched \u{2500}\u{2500}\u{2500}\x1b[0m");
    println!();
    println!("  Open a new terminal to see the terminal ghost.");
    println!("  Check VS Code, browser, Obsidian if those channels are configured.");
    println!("  Ghosts will self-destruct after 5 minutes of activity.");

    Ok(())
}

/// The editor extension's captured resume point for a project, if any:
/// reads ~/.revenant/vscode-state/<hash>.json for activeFile + activeLine
fn editor_anchor_for(project_dir: &str) -> Option<serde_json::Value> {
    // FNV-1a, matching the extension and the daemon
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in project_dir.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let state_file = revenant_dir()
        .join("vscode-state")
        .join(format!("{hash:016x}.json"));

    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(state_file).ok()?).ok()?;
    let file = state.get("activeFile")?.as_str()?;
    let line = state.get("activeLine")?.as_u64().filter(|l| *l > 0)?;
    Some(serde_json::json!({ "file": file, "line": line }))
}

/// Send a context card to a ghost channel via Unix socket (shared by vscode + slack test dispatch)
fn dispatch_socket_ghost(
    sock_path: &Path,
    name: &str,
    card_id: &str,
    cwd: &str,
    project_name: &str,
    summary: &str,
    next_step: &str,
    anchor: Option<&serde_json::Value>,
) -> Result<()> {
    if sock_path.exists() {
        let msg = serde_json::json!({
            "type": "inject",
            "card": {
                "id": card_id,
                "project_dir": cwd,
                "project_name": project_name,
                "summary": summary,
                "next_step": next_step,
                "ttl_seconds": 300,
                "anchor": anchor,
            }
        });
        match std::os::unix::net::UnixStream::connect(sock_path) {
            Ok(mut stream) => {
                use std::io::Write;
                let payload = serde_json::to_string(&msg)? + "\n";
                stream.write_all(payload.as_bytes())?;
                println!("\x1b[2m\u{2502}\x1b[0m \x1b[32m+\x1b[0m {name} ghost (socket)");
            }
            Err(e) => {
                println!(
                    "\x1b[2m\u{2502}\x1b[0m \x1b[33m~\x1b[0m {name} ghost skipped (socket error: {e})"
                );
            }
        }
    } else {
        println!("\x1b[2m\u{2502}\x1b[0m \x1b[2m-\x1b[0m {name} ghost (no socket)");
    }
    Ok(())
}

fn cmd_digest(days: u64) -> Result<()> {
    let conn = open_db_readonly()?;
    let since = Utc::now() - chrono::Duration::days(days as i64);

    // Get project summaries
    struct ProjectEntry {
        name: String,
        cards: usize,
    }

    let projects: Vec<ProjectEntry> = {
        let mut stmt = conn.prepare(
            "SELECT project_name, COUNT(*) as card_count
             FROM context_cards
             WHERE created_at >= ?1
             GROUP BY project_dir
             ORDER BY card_count DESC",
        )?;
        let rows = stmt.query_map(params![since.to_rfc3339()], |row| {
            Ok(ProjectEntry {
                name: row.get(0)?,
                cards: row.get::<_, i64>(1)? as usize,
            })
        })?;
        rows.filter_map(|r| r.ok()).collect()
    };

    if projects.is_empty() {
        println!("\x1b[2mno activity in the last {days} days\x1b[0m");
        return Ok(());
    }

    let total_cards: usize = projects.iter().map(|p| p.cards).sum();

    // Count context switches
    let switch_count: usize = {
        let mut stmt = conn.prepare(
            "SELECT project_name FROM context_cards WHERE created_at >= ?1 ORDER BY created_at ASC",
        )?;
        let names: Vec<String> = stmt
            .query_map(params![since.to_rfc3339()], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        names.windows(2).filter(|w| w[0] != w[1]).count()
    };

    // Busiest day
    let busiest: Option<(String, usize)> = {
        let mut stmt = conn.prepare(
            "SELECT date(created_at) as day, COUNT(*) as cnt
             FROM context_cards
             WHERE created_at >= ?1
             GROUP BY day
             ORDER BY cnt DESC
             LIMIT 1",
        )?;
        stmt.query_row(params![since.to_rfc3339()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
        })
        .ok()
    };

    // Intent analysis from summaries
    let mut intent_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let summaries: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT summary FROM context_cards WHERE created_at >= ?1",
        )?;
        let rows = stmt.query_map(params![since.to_rfc3339()], |row| row.get(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    for s in &summaries {
        let lower = s.to_lowercase();
        for keyword in &["fixing", "building", "testing", "refactoring", "deploying", "reviewing", "configuring", "creating", "documenting"] {
            if lower.contains(keyword) {
                *intent_counts.entry(keyword).or_insert(0) += 1;
            }
        }
    }
    let mut intents: Vec<(&str, usize)> = intent_counts.into_iter().collect();
    intents.sort_by(|a, b| b.1.cmp(&a.1));

    // ─── Render ───

    let period_label = if days == 7 { "this week".to_string() } else { format!("last {days} days") };

    println!();
    println!(
        "\x1b[2m\u{2554}\u{2550}\u{2550}\x1b[0m\x1b[38;5;178m REVENANT DIGEST \u{2502} {period_label} \x1b[0m\x1b[2m\u{2550}\u{2550}\u{2557}\x1b[0m"
    );
    println!("\x1b[2m\u{2551}\x1b[0m");

    // Headline stats
    println!(
        "\x1b[2m\u{2551}\x1b[0m  \x1b[97m{total_cards}\x1b[0m context snapshots across \x1b[97m{}\x1b[0m projects",
        projects.len()
    );
    println!(
        "\x1b[2m\u{2551}\x1b[0m  \x1b[97m{switch_count}\x1b[0m context switches detected"
    );
    if let Some((day, count)) = busiest {
        println!(
            "\x1b[2m\u{2551}\x1b[0m  Busiest day: \x1b[97m{day}\x1b[0m ({count} snapshots)"
        );
    }
    println!("\x1b[2m\u{2551}\x1b[0m");

    // Project breakdown with visual bar
    println!("\x1b[2m\u{2551}\x1b[0m  \x1b[2mATTENTION DISTRIBUTION\x1b[0m");
    let max_cards = projects.iter().map(|p| p.cards).max().unwrap_or(1);
    let bar_width = 24usize;

    for (i, proj) in projects.iter().enumerate() {
        let bar_len = (proj.cards as f64 / max_cards as f64 * bar_width as f64).ceil() as usize;
        let bar: String = "\u{2588}".repeat(bar_len);
        let pct = (proj.cards as f64 / total_cards as f64 * 100.0) as usize;

        let color = if i == 0 { "\x1b[38;5;178m" } else { "\x1b[38;5;245m" };
        println!(
            "\x1b[2m\u{2551}\x1b[0m  {color}{bar}\x1b[0m {:<14} \x1b[2m{:>3}% ({} cards)\x1b[0m",
            proj.name, pct, proj.cards
        );
    }

    // Intent breakdown
    if !intents.is_empty() {
        println!("\x1b[2m\u{2551}\x1b[0m");
        println!("\x1b[2m\u{2551}\x1b[0m  \x1b[2mWHAT YOU WERE DOING\x1b[0m");
        for (intent, count) in intents.iter().take(5) {
            let icon = match *intent {
                "fixing" => "\x1b[38;5;203m\u{25CF}\x1b[0m",
                "building" | "creating" => "\x1b[38;5;178m\u{25CF}\x1b[0m",
                "testing" => "\x1b[38;5;114m\u{25CF}\x1b[0m",
                "deploying" => "\x1b[38;5;213m\u{25CF}\x1b[0m",
                "refactoring" => "\x1b[38;5;110m\u{25CF}\x1b[0m",
                _ => "\x1b[38;5;145m\u{25CF}\x1b[0m",
            };
            println!(
                "\x1b[2m\u{2551}\x1b[0m  {icon} {intent:<14} \x1b[2m{count}x\x1b[0m"
            );
        }
    }

    println!("\x1b[2m\u{2551}\x1b[0m");
    println!(
        "\x1b[2m\u{255a}\u{2550}\u{2550} {} \u{2550}\u{2550}\u{255d}\x1b[0m",
        if switch_count > 50 {
            "heavy multitasking - consider deeper focus blocks"
        } else if switch_count > 20 {
            "moderate context switching"
        } else {
            "focused work pattern"
        }
    );
    println!();

    Ok(())
}

// ─── Utilities ───────────────────────────────────────────────────────

/// "5min ago", but "just now" without a dangling "ago"
fn format_age_ago(dt: DateTime<Utc>) -> String {
    let age = format_age(dt);
    if age == "just now" {
        age
    } else {
        format!("{age} ago")
    }
}

fn format_age(dt: DateTime<Utc>) -> String {
    let now = Utc::now();
    let elapsed = now.signed_duration_since(dt);
    let secs = elapsed.num_seconds().max(0);

    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}min", secs / 60)
    } else if secs < 86400 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m > 0 {
            format!("{h}h {m}min")
        } else {
            format!("{h}h")
        }
    } else {
        let d = secs / 86400;
        let h = (secs % 86400) / 3600;
        if h > 0 {
            format!("{d}d {h}h")
        } else {
            format!("{d}d")
        }
    }
}

/// Download URL for a file attached to this build's GitHub release
fn release_asset_url(file: &str) -> String {
    format!(
        "https://github.com/derealt/revenant/releases/download/v{}/{file}",
        env!("CARGO_PKG_VERSION")
    )
}

/// Try to find the REVENANT project root (where Cargo.toml lives)
fn find_project_root() -> Result<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..5 {
            if let Some(ref d) = dir {
                if d.join("Cargo.toml").exists() || d.join("ghosts").exists() {
                    return Ok(d.clone());
                }
                dir = d.parent().map(|p| p.to_path_buf());
            } else {
                break;
            }
        }
    }
    bail!("could not find REVENANT project root")
}

// ─── Main ────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init => cmd_init(),
        Commands::Status => cmd_status(),
        Commands::History { project, count } => cmd_history(project, count),
        Commands::Off => cmd_off(),
        Commands::On => cmd_on(),
        Commands::Forget { project, all } => cmd_forget(project, all),
        Commands::Setup { channel, key, provider, model, vault, token, user, channel_id } => {
            cmd_setup(&channel, key, provider, model, vault, token, user, channel_id)
        }
        Commands::Test => cmd_test(),
        Commands::Clear => cmd_clear(),
        Commands::Digest { days } => cmd_digest(days),
    }
}
