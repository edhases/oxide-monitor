//! Minimal file logging for the elevated helper (no console, no tracing stack).
//!
//! Logs live under `%LOCALAPPDATA%\OxideMonitor\logs\pawnio-host.log` and rotate at
//! 256 KiB. They never contain pipe contents or temperature history beyond a line.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LOG_BYTES: u64 = 256 * 1024;

pub fn log_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("OxideMonitor")
        .join("logs")
        .join("pawnio-host.log")
}

pub fn line(message: &str) {
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_LOG_BYTES) {
        let previous = path.with_extension("log.1");
        let _ = fs::remove_file(&previous);
        let _ = fs::rename(&path, previous);
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "[{stamp}] {message}");
    }
}
