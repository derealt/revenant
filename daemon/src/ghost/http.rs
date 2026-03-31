//! Tiny HTTP server for browser extension polling.
//! Serves the latest ghost from SQLite on localhost:7711
//! Card stays available until TTL expires or a new switch clears it.

use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{info, error};

/// Holds the current ghost card as JSON string, set by the dispatcher.
pub struct GhostHttpServer {
    current_json: Arc<Mutex<String>>,
}

impl GhostHttpServer {
    pub fn new() -> Self {
        Self {
            current_json: Arc::new(Mutex::new(r#"{"type":"clear"}"#.to_string())),
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
            *j = r#"{"type":"clear"}"#.to_string();
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

        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => continue,
            };

            let json = json.clone();

            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let request = String::from_utf8_lossy(&buf);

                if request.starts_with("OPTIONS") {
                    let resp = "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, OPTIONS\r\nAccess-Control-Allow-Headers: *\r\nContent-Length: 0\r\n\r\n";
                    let _ = stream.write_all(resp.as_bytes()).await;
                    return;
                }

                let body = json.lock().map(|j| j.clone()).unwrap_or_else(|_| r#"{"type":"clear"}"#.to_string());

                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            });
        }
    }
}
