//! Tiny HTTP server on localhost:7711 - the browser extension's bridge.
//!
//! GET  /ghost   -> the current ghost card as JSON (extension polls this)
//! POST /tab     -> extension reports the active tab (stored only when
//!                  `signals.browser = true`; discarded otherwise)
//! POST /inject  -> set the current ghost card (used by `rvn test`)
//! POST /clear   -> clear the current ghost card (used by `rvn clear`)
//!
//! Bound to 127.0.0.1 only. Requests larger than 16KB are rejected.

use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{error, info, warn};

use crate::signals::browser::BrowserTab;

const MAX_REQUEST_BYTES: usize = 16 * 1024;
const CLEAR_JSON: &str = r#"{"type":"clear"}"#;

/// Holds the current ghost card as JSON string, set by the dispatcher.
pub struct GhostHttpServer {
    current_json: Arc<Mutex<String>>,
    /// Whether POSTed tab reports are stored (config: signals.browser)
    accept_tab_reports: bool,
}

impl GhostHttpServer {
    pub fn new(accept_tab_reports: bool) -> Self {
        Self {
            current_json: Arc::new(Mutex::new(CLEAR_JSON.to_string())),
            accept_tab_reports,
        }
    }

    /// Set the current ghost card (called by dispatcher on Return events)
    pub fn inject(&self, card_json: String) {
        if let Ok(mut j) = self.current_json.lock() {
            *j = card_json;
        }
    }

    /// Clear the current ghost (called by dispatcher on reap/departure)
    pub fn clear(&self) {
        if let Ok(mut j) = self.current_json.lock() {
            *j = CLEAR_JSON.to_string();
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

        let json = self.current_json.clone();
        let accept_tab_reports = self.accept_tab_reports;

        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => continue,
            };

            let json = json.clone();

            tokio::spawn(async move {
                let request = match read_request(&mut stream).await {
                    Some(r) => r,
                    None => return,
                };

                let (method, path) = request_line(&request);
                let response = match (method, path) {
                    ("OPTIONS", _) => cors_response("204 No Content", ""),
                    ("GET", _) => {
                        let body = json
                            .lock()
                            .map(|j| j.clone())
                            .unwrap_or_else(|_| CLEAR_JSON.to_string());
                        cors_response("200 OK", &body)
                    }
                    ("POST", "/tab") => {
                        if accept_tab_reports {
                            match serde_json::from_str::<Vec<BrowserTab>>(request_body(&request)) {
                                Ok(tabs) => {
                                    if let Err(e) = crate::signals::browser::store_tabs(&tabs) {
                                        warn!("failed to store tab report: {e}");
                                    }
                                }
                                Err(_) => return respond(&mut stream, &cors_response("400 Bad Request", "")).await,
                            }
                        }
                        // Opted out: accept and discard, so the extension stays quiet
                        cors_response("204 No Content", "")
                    }
                    ("POST", "/inject") => {
                        let body = request_body(&request);
                        // Must be a well-formed ghost message before it's served to the extension
                        match serde_json::from_str::<serde_json::Value>(body) {
                            Ok(v) if v.get("type").and_then(|t| t.as_str()) == Some("inject")
                                && v.get("card").is_some() =>
                            {
                                if let Ok(mut j) = json.lock() {
                                    *j = body.to_string();
                                }
                                cors_response("204 No Content", "")
                            }
                            _ => cors_response("400 Bad Request", ""),
                        }
                    }
                    ("POST", "/clear") => {
                        if let Ok(mut j) = json.lock() {
                            *j = CLEAR_JSON.to_string();
                        }
                        cors_response("204 No Content", "")
                    }
                    _ => cors_response("404 Not Found", ""),
                };

                respond(&mut stream, &response).await;
            });
        }
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

fn cors_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    )
}

async fn respond(stream: &mut tokio::net::TcpStream, response: &str) {
    let _ = stream.write_all(response.as_bytes()).await;
}
