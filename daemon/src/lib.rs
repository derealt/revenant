//! REVENANT core library
//!
//! Shared by the daemon binary and the integration tests, so tests can
//! drive the real detector, compressor, store, and ghost code paths.

pub mod compressor;
pub mod config;
pub mod detector;
pub mod ghost;
pub mod signals;
pub mod snapshot;
pub mod store;
pub mod watcher;

use std::path::PathBuf;

/// Expand ~ to home directory in paths
pub fn expand_path(path: &str) -> PathBuf {
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(&path[2..]);
        }
    }
    PathBuf::from(path)
}
