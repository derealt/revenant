//! Tiny HTTP server on localhost:7711 - the browser extension's bridge
//! and the shell hook's return signal.
//!
//! GET  /ghost      -> the current ghost card as JSON (extension polls this)
//! POST /tab        -> extension reports the active tab (stored only when
//!                     `signals.browser = true`; discarded otherwise)
//! POST /inject     -> set the current ghost card (used by `rvn test`)
//! POST /clear      -> clear the current ghost card (used by `rvn clear`)
//! POST /shell-open -> a new shell announces itself with its cwd; the
//!                     daemon decides whether that is a return worth a
//!                     ghost and answers with rendered MOTD text
//!
//! Bound to 127.0.0.1 only. Requests larger than 16KB are rejected.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{error, info, warn};

use crate::detector::{SwitchDetector, SwitchEvent, SwitchKind};
use crate::signals::browser::BrowserTab;
use crate::store::ContextStore;

const MAX_REQUEST_BYTES: usize = 16 * 1024;
const CLEAR_JSON: &str = r#"{"type":"clear"}"#;

/// The current ghost plus when it was injected, so it can expire.
/// Ghosts are transient by design: without this, the browser banner
/// would be served forever.
struct CurrentGhost {
    json: String,
    injected_at: std::time::Instant,
    ttl: std::time::Duration,
}

impl CurrentGhost {
    fn cleared() -> Self {
        Self {
            json: CLEAR_JSON.to_string(),
            injected_at: std::time::Instant::now(),
            ttl: std::time::Duration::MAX,
        }
    }

    fn expired(&self) -> bool {
        self.injected_at.elapsed() > self.ttl
    }
}

/// Everything the /shell-open endpoint needs to answer a freshly opened
/// shell: the detector decides whether this is a return, the store
/// supplies the card, the channel wakes the other ghost channels.
pub struct ShellBridge {
    pub detector: Arc<SwitchDetector>,
    pub store: Arc<ContextStore>,
    pub switch_tx: tokio::sync::mpsc::Sender<SwitchEvent>,
    /// The terminal MOTD path: a live MOTD is served to every shell
    /// that opens inside its window
    pub motd_path: PathBuf,
    /// The greeting window: which card was last greeted on a return, and
    /// when. New shells are greeted while it lasts (the card's TTL), then
    /// stay silent until the next return opens a new one.
    pub greeted: Arc<Mutex<Option<(String, std::time::SystemTime)>>>,
}

/// Holds the current ghost card as JSON string, set by the dispatcher.
pub struct GhostHttpServer {
    current: Arc<Mutex<CurrentGhost>>,
    /// Whether POSTed tab reports are stored (config: signals.browser)
    accept_tab_reports: bool,
    /// Banner lifetime cap (config: ghosts.browser.banner_seconds).
    /// The banner goes off after 1 minute by default; users may raise it.
    banner_seconds: u64,
    /// Present in the daemon; absent in contexts with no store/detector
    shell: Option<Arc<ShellBridge>>,
}

/// Cap the card's TTL to the configured banner lifetime and rewrite
/// ttl_seconds in the served JSON, so the page-side fade timer and the
/// server-side expiry always agree.
fn effective_ghost(card_json: &str, cap_secs: u64) -> (String, std::time::Duration) {
    match serde_json::from_str::<serde_json::Value>(card_json) {
        Ok(mut v) => {
            let secs = v
                .get("card")
                .and_then(|c| c.get("ttl_seconds"))
                .and_then(|t| t.as_u64())
                .unwrap_or(cap_secs)
                .min(cap_secs);
            if let Some(card) = v.get_mut("card") {
                card["ttl_seconds"] = serde_json::json!(secs);
            }
            (
                serde_json::to_string(&v).unwrap_or_else(|_| card_json.to_string()),
                std::time::Duration::from_secs(secs),
            )
        }
        Err(_) => (
            card_json.to_string(),
            std::time::Duration::from_secs(cap_secs),
        ),
    }
}

impl GhostHttpServer {
    pub fn new(accept_tab_reports: bool, banner_seconds: u64) -> Self {
        Self {
            current: Arc::new(Mutex::new(CurrentGhost::cleared())),
            accept_tab_reports,
            banner_seconds: banner_seconds.max(1),
            shell: None,
        }
    }

    /// Enable the /shell-open endpoint
    pub fn with_shell_bridge(mut self, bridge: ShellBridge) -> Self {
        self.shell = Some(Arc::new(bridge));
        self
    }

    /// Set the current ghost card (called by dispatcher on Return events)
    pub fn inject(&self, card_json: String) {
        let (json, ttl) = effective_ghost(&card_json, self.banner_seconds);
        if let Ok(mut g) = self.current.lock() {
            *g = CurrentGhost {
                json,
                ttl,
                injected_at: std::time::Instant::now(),
            };
        }
    }

    /// Clear the current ghost (called by dispatcher on reap/departure)
    pub fn clear(&self) {
        if let Ok(mut g) = self.current.lock() {
            *g = CurrentGhost::cleared();
        }
    }

    pub async fn serve(&self, port: u16) {
        let addr = format!("127.0.0.1:{}", port);
        let listener = match TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => {
                error!("ghost http server failed to bind {}: {}", addr, e);
                return;
            }
        };
        info!("ghost http server listening on {}", addr);

        let current = self.current.clone();
        let accept_tab_reports = self.accept_tab_reports;
        let banner_seconds = self.banner_seconds;
        let shell = self.shell.clone();

        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => continue,
            };

            let current = current.clone();
            let shell = shell.clone();

            tokio::spawn(async move {
                let request = match read_request(&mut stream).await {
                    Some(r) => r,
                    None => return,
                };

                if !request_allowed(&request, port) {
                    return respond(&mut stream, &json_response("403 Forbidden", "")).await;
                }

                let (method, path) = request_line(&request);
                let response = match (method, path) {
                    ("OPTIONS", _) => json_response("204 No Content", ""),
                    ("GET", _) => {
                        // Ghosts are transient: an expired card is served
                        // (and stored) as cleared
                        let body = current
                            .lock()
                            .map(|mut g| {
                                if g.expired() {
                                    *g = CurrentGhost::cleared();
                                }
                                g.json.clone()
                            })
                            .unwrap_or_else(|_| CLEAR_JSON.to_string());
                        json_response("200 OK", &body)
                    }
                    ("POST", "/tab") => {
                        if accept_tab_reports {
                            match serde_json::from_str::<Vec<BrowserTab>>(request_body(&request)) {
                                Ok(tabs) => {
                                    if let Err(e) = crate::signals::browser::store_tabs(&tabs) {
                                        warn!("failed to store tab report: {e}");
                                    }
                                }
                                Err(_) => return respond(&mut stream, &json_response("400 Bad Request", "")).await,
                            }
                        }
                        // Opted out: accept and discard, so the extension stays quiet
                        json_response("204 No Content", "")
                    }
                    ("POST", "/inject") => {
                        let body = request_body(&request);
                        // Must be a well-formed ghost message before it's served to the extension
                        match serde_json::from_str::<serde_json::Value>(body) {
                            Ok(v) if v.get("type").and_then(|t| t.as_str()) == Some("inject")
                                && v.get("card").is_some() =>
                            {
                                let (json, ttl) = effective_ghost(body, banner_seconds);
                                if let Ok(mut g) = current.lock() {
                                    *g = CurrentGhost {
                                        json,
                                        ttl,
                                        injected_at: std::time::Instant::now(),
                                    };
                                }
                                json_response("204 No Content", "")
                            }
                            _ => json_response("400 Bad Request", ""),
                        }
                    }
                    ("POST", "/clear") => {
                        if let Ok(mut g) = current.lock() {
                            *g = CurrentGhost::cleared();
                        }
                        json_response("204 No Content", "")
                    }
                    ("POST", "/shell-open") => match shell {
                        Some(ref bridge) => {
                            let cwd = serde_json::from_str::<serde_json::Value>(
                                request_body(&request),
                            )
                            .ok()
                            .and_then(|v| {
                                v.get("cwd").and_then(|c| c.as_str()).map(String::from)
                            });
                            match cwd {
                                Some(cwd) => shell_open_response(bridge, &cwd).await,
                                None => json_response("400 Bad Request", ""),
                            }
                        }
                        None => json_response("404 Not Found", ""),
                    },
                    _ => json_response("404 Not Found", ""),
                };

                respond(&mut stream, &response).await;
            });
        }
    }
}

/// Answer a freshly opened shell. Opening a terminal IS asking "where
/// was I?", so the shell is ALWAYS greeted with the latest card: that
/// is the terminal channel's original design. The detector's absence
/// rules only decide the second question: whether this shell-open also
/// counts as a return that wakes the other ghost channels.
async fn shell_open_response(bridge: &ShellBridge, cwd: &str) -> String {
    // Resolve which project this shell belongs to: its own git root,
    // else wherever the user last was
    // (after a reboot there is no active project yet: the freshest card's
    // project stands in, so the first shell back is still a return)
    let project = crate::watcher::find_git_root(&PathBuf::from(cwd))
        .map(|p| p.to_string_lossy().to_string())
        .or_else(|| bridge.detector.active_project())
        .or_else(|| {
            bridge
                .store
                .cards_since(chrono::Utc::now() - chrono::Duration::days(30), 1)
                .ok()
                .and_then(|cards| cards.into_iter().next())
                .map(|c| c.project_dir)
        });

    // A live terminal ghost is served as-is to shells of ITS project, so
    // every tab opened inside its window sees the MOTD the reaper will
    // soon fade. A shell in another project gets its own greeting below:
    // hopping to billing must not greet a new harbor tab with billing.
    if let Ok(motd) = tokio::fs::read_to_string(&bridge.motd_path).await {
        let meta = tokio::fs::read_to_string(bridge.motd_path.with_extension("json"))
            .await
            .ok()
            .and_then(|m| serde_json::from_str::<serde_json::Value>(&m).ok());
        let live_project = meta
            .as_ref()
            .and_then(|m| m.get("project_dir").and_then(|p| p.as_str()).map(String::from));
        // The reaper only sweeps every few minutes, so the file can outlive
        // its window: an expired MOTD is not live, whatever is on disk
        let expired = meta
            .as_ref()
            .and_then(|m| m.get("expires_at").and_then(|e| e.as_str()))
            .and_then(|e| chrono::DateTime::parse_from_rfc3339(e).ok())
            .is_some_and(|e| e < chrono::Utc::now());
        let same_project = match (&live_project, &project) {
            (Some(live), Some(here)) => live == here,
            // Metadata from an older daemon, or a shell outside any project
            _ => true,
        };
        if expired {
            let _ = tokio::fs::remove_file(&bridge.motd_path).await;
            let _ = tokio::fs::remove_file(bridge.motd_path.with_extension("json")).await;
        } else if same_project {
            return text_response("200 OK", &motd);
        }
    }

    // Wake the other channels only when the detector calls this a
    // return (absence, project switch, or first sighting). A mid-flow
    // tab still gets its greeting below, but must not flash banners
    // in the browser and editor.
    let mut returned = false;
    if let Some(ref project) = project {
        if let Some(event) = bridge.detector.shell_opened(project) {
            returned = true;
            // A shell-open landing on a different project banks a
            // departure for the previous one first, exactly like the
            // watcher's switch path
            if let SwitchKind::Return {
                from_project: Some(ref prev),
                ..
            } = event.kind
            {
                let _ = bridge
                    .switch_tx
                    .send(SwitchEvent {
                        kind: SwitchKind::Departure {
                            project_dir: prev.clone(),
                        },
                        timestamp: chrono::Utc::now(),
                    })
                    .await;
            }
            info!("shell opened in {project}: treating as return");
            let _ = bridge.switch_tx.send(event).await;
        }
    }

    // The greeting: the project's own latest card, else the freshest
    // card on record. Silence only when the store is empty.
    let card = project
        .as_deref()
        .and_then(|p| bridge.store.latest_card(p).ok().flatten())
        .or_else(|| {
            bridge
                .store
                .cards_since(chrono::Utc::now() - chrono::Duration::days(30), 1)
                .ok()
                .and_then(|cards| cards.into_iter().next())
        });

    let Some(card) = card else {
        return text_response("204 No Content", "");
    };

    // "ghost fades in 1min of activity" must be true. A return opens the
    // greeting window for this card; shells inside it are greeted, and
    // after it the terminal is silent until the next return.
    let now = std::time::SystemTime::now();
    let window = std::time::Duration::from_secs(card.ttl_seconds.max(1));
    let greet = match bridge.greeted.lock() {
        Ok(mut greeted) => {
            if returned {
                *greeted = Some((card.id.clone(), now));
                true
            } else {
                matches!(&*greeted, Some((id, at))
                    if *id == card.id
                        && now.duration_since(*at).unwrap_or_default() < window)
            }
        }
        Err(_) => returned,
    };

    if greet {
        text_response("200 OK", &crate::ghost::terminal::render_motd(&card))
    } else {
        text_response("204 No Content", "")
    }
}

/// Read a full HTTP request: headers plus Content-Length body, capped.
async fn read_request(stream: &mut tokio::net::TcpStream) -> Option<String> {
    let mut data = Vec::with_capacity(1024);
    let mut buf = [0u8; 1024];

    loop {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if data.len() > MAX_REQUEST_BYTES {
            return None;
        }

        if let Some(header_end) = find_header_end(&data) {
            let headers = String::from_utf8_lossy(&data[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|l| {
                    let (name, value) = l.split_once(':')?;
                    if name.eq_ignore_ascii_case("content-length") {
                        value.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);

            if content_length > MAX_REQUEST_BYTES {
                return None;
            }
            if data.len() >= header_end + 4 + content_length {
                return Some(String::from_utf8_lossy(&data).into_owned());
            }
        }
    }

    Some(String::from_utf8_lossy(&data).into_owned())
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|w| w == b"\r\n\r\n")
}

fn request_line(request: &str) -> (&str, &str) {
    let mut parts = request.lines().next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    // Strip any query string
    let path = path.split('?').next().unwrap_or(path);
    (method, path)
}

fn request_body(request: &str) -> &str {
    request
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or("")
}

/// Plain-text response for the shell hook: the body is pre-rendered
/// ANSI, printed verbatim by the opening shell
fn text_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    )
}

/// No CORS headers: the extension reaches the server through its
/// host_permissions, which need none, and nothing else should read it.
fn json_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    )
}

/// The card names your projects, files and cursor line, and the banner is
/// drawn inside the pages you use. So only two callers get through: local
/// tools (rvn, the shell hook), which send no Origin, and the browser
/// extension, which sends its own extension origin. A web page always sends
/// its origin, so a site you visit can neither read the card nor write into
/// the banner. The Host check closes DNS rebinding, where a page's own name
/// resolves to 127.0.0.1 and its requests look same-origin.
fn request_allowed(request: &str, port: u16) -> bool {
    let host_ok = header(request, "host").is_some_and(|h| {
        h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}")
    });
    let origin_ok = match header(request, "origin") {
        None => true,
        Some(o) => ["chrome-extension://", "moz-extension://", "safari-web-extension://"]
            .iter()
            .any(|scheme| o.starts_with(scheme)),
    };
    host_ok && origin_ok
}

fn header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request
        .split("\r\n\r\n")
        .next()?
        .lines()
        .skip(1)
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
        })
}

async fn respond(stream: &mut tokio::net::TcpStream, response: &str) {
    let _ = stream.write_all(response.as_bytes()).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_ttl_is_capped_and_rewritten() {
        // A 5-minute card is served as a 60s banner by default
        let five_min = r#"{"type":"inject","card":{"id":"x","ttl_seconds":300}}"#;
        let (json, ttl) = effective_ghost(five_min, 60);
        assert_eq!(ttl.as_secs(), 60);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["card"]["ttl_seconds"], 60);

        // Users may raise the cap; shorter card TTLs are still honored
        let (_, ttl) = effective_ghost(five_min, 600);
        assert_eq!(ttl.as_secs(), 300);
        let short = r#"{"type":"inject","card":{"id":"x","ttl_seconds":30}}"#;
        let (_, ttl) = effective_ghost(short, 60);
        assert_eq!(ttl.as_secs(), 30);

        // Missing TTL falls back to the cap
        let none = r#"{"type":"inject","card":{"id":"x"}}"#;
        let (json, ttl) = effective_ghost(none, 60);
        assert_eq!(ttl.as_secs(), 60);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["card"]["ttl_seconds"], 60);
    }
}
