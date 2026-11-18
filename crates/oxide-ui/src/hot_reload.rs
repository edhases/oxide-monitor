//! Zero-thread `config.toml` hot reload watcher (`mtime + size + FNV-1a hash`).
//!
//! Polled on the existing 2-second UI timer so idle thread/handle overhead is zero.
//! Supports self-write suppression by content hash, log-once deduplication for invalid
//! TOML, and pausing while an IPC live-preview session is active.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use oxide_core::{Config, config_hash_bytes};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileFingerprint {
    size: u64,
    mtime_nanos: u128,
    hash: u64,
}

/// Outcome of a single `ConfigWatcher::poll()` check.
#[derive(Debug)]
pub enum ReloadEvent {
    Unchanged,
    Paused,
    Reloaded { config: Box<Config>, hash: u64 },
    InvalidToml { error: String, already_logged: bool },
}

/// Tracks on-disk changes to `config.toml` without spawning background threads.
pub struct ConfigWatcher {
    path: PathBuf,
    last: Option<FileFingerprint>,
    last_bad_hash: Option<u64>,
    paused: bool,
}

impl ConfigWatcher {
    pub fn new(path: PathBuf, initial_hash: u64) -> Self {
        let last = read_meta(&path).map(|(size, mtime_nanos)| {
            let hash = fs::read(&path)
                .map(|b| config_hash_bytes(&b))
                .unwrap_or(initial_hash);
            FileFingerprint {
                size,
                mtime_nanos,
                hash,
            }
        });
        Self {
            path,
            last,
            last_bad_hash: None,
            paused: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Record a write performed by `oxide-monitor.exe` itself so the next poll ignores it.
    pub fn record_self_write(&mut self, written_hash: u64) {
        let (size, mtime_nanos) = read_meta(&self.path).unwrap_or((0, 0));
        self.last = Some(FileFingerprint {
            size,
            mtime_nanos,
            hash: written_hash,
        });
        self.last_bad_hash = None;
    }

    /// Return the current FNV-1a hash of the on-disk file, if readable.
    pub fn current_disk_hash(&self) -> Option<u64> {
        fs::read(&self.path).ok().map(|b| config_hash_bytes(&b))
    }

    /// Check whether `config.toml` has changed externally since the last accepted state.
    pub fn poll(&mut self) -> ReloadEvent {
        if self.paused {
            return ReloadEvent::Paused;
        }
        let Some((size, mtime_nanos)) = read_meta(&self.path) else {
            return ReloadEvent::Unchanged;
        };

        // Fast path: metadata unchanged -> 0 file reads.
        if let Some(prev) = self.last
            && prev.size == size
            && prev.mtime_nanos == mtime_nanos
        {
            return ReloadEvent::Unchanged;
        }

        let Ok(bytes) = fs::read(&self.path) else {
            return ReloadEvent::Unchanged;
        };
        let hash = config_hash_bytes(&bytes);

        // Same content hash (e.g. our own write or metadata-only touch) -> update metadata and ignore.
        if let Some(prev) = self.last
            && prev.hash == hash
        {
            self.last = Some(FileFingerprint {
                size,
                mtime_nanos,
                hash,
            });
            return ReloadEvent::Unchanged;
        }

        let text = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(err) => {
                let already_logged = self.last_bad_hash == Some(hash);
                self.last_bad_hash = Some(hash);
                return ReloadEvent::InvalidToml {
                    error: format!("invalid UTF-8: {err}"),
                    already_logged,
                };
            }
        };

        match Config::from_toml_str(text) {
            Ok(cfg) => {
                self.last = Some(FileFingerprint {
                    size,
                    mtime_nanos,
                    hash,
                });
                self.last_bad_hash = None;
                ReloadEvent::Reloaded {
                    config: Box::new(cfg),
                    hash,
                }
            }
            Err(err) => {
                let already_logged = self.last_bad_hash == Some(hash);
                self.last_bad_hash = Some(hash);
                ReloadEvent::InvalidToml {
                    error: err.to_string(),
                    already_logged,
                }
            }
        }
    }
}

fn read_meta(path: &Path) -> Option<(u64, u128)> {
    let meta = fs::metadata(path).ok()?;
    let size = meta.len();
    let mtime_nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some((size, mtime_nanos))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config_path(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("oxide-hot-reload-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    #[test]
    fn hot_reload_ignores_self_write_and_reloads_valid_external_edit() {
        let path = temp_config_path("valid");
        let mut cfg = Config::default();
        let initial_hash = cfg.save_to_path_returning_hash(&path).unwrap();

        let mut watcher = ConfigWatcher::new(path.clone(), initial_hash);
        assert!(matches!(watcher.poll(), ReloadEvent::Unchanged));

        // Self-write with new content must be suppressed by hash.
        cfg.surface.opacity = 200;
        let self_hash = cfg.save_to_path_returning_hash(&path).unwrap();
        watcher.record_self_write(self_hash);
        assert!(matches!(watcher.poll(), ReloadEvent::Unchanged));

        // External edit must be detected and reloaded.
        cfg.surface.opacity = 155;
        cfg.save_to_path(&path).unwrap();

        match watcher.poll() {
            ReloadEvent::Reloaded { config, .. } => {
                assert_eq!(config.surface.opacity, 155);
            }
            other => panic!("expected Reloaded, got {other:?}"),
        }
        assert!(matches!(watcher.poll(), ReloadEvent::Unchanged));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn hot_reload_keeps_working_config_and_logs_once_on_invalid_toml_and_pauses() {
        let path = temp_config_path("invalid");
        let cfg = Config::default();
        let initial_hash = cfg.save_to_path_returning_hash(&path).unwrap();
        let mut watcher = ConfigWatcher::new(path.clone(), initial_hash);

        // Broken TOML -> InvalidToml { already_logged: false } first time, true second time.
        fs::write(&path, "version = 2\n[general\nbroken = !!!").unwrap();
        match watcher.poll() {
            ReloadEvent::InvalidToml { already_logged, .. } => assert!(!already_logged),
            other => panic!("expected InvalidToml, got {other:?}"),
        }
        match watcher.poll() {
            ReloadEvent::InvalidToml { already_logged, .. } => assert!(already_logged),
            other => panic!("expected repeated InvalidToml, got {other:?}"),
        }

        // Pause during preview -> Paused even if valid TOML is written externally.
        watcher.set_paused(true);
        let mut updated = cfg.clone();
        updated.surface.opacity = 188;
        updated.save_to_path(&path).unwrap();
        assert!(matches!(watcher.poll(), ReloadEvent::Paused));

        // Resume -> picks up the valid file cleanly.
        watcher.set_paused(false);
        match watcher.poll() {
            ReloadEvent::Reloaded { config, .. } => assert_eq!(config.surface.opacity, 188),
            other => panic!("expected Reloaded after resume, got {other:?}"),
        }

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
