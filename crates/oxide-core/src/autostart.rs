//! Autostart status shared with the settings UI.
//!
//! The monitor reconciles the Windows Task Scheduler task and publishes the result in
//! `%APPDATA%\OxideMonitor\autostart.toml` (portable mode: next to the executable).
//! The settings UI reads this file; it is an additive side channel because adding a
//! field/variant to `MonitorToSettings` would break older clients that match on it.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Stable scheduled-task name used for the per-user autostart task.
pub const AUTOSTART_TASK_NAME: &str = "Oxide Monitor";
/// Sidecar file written next to `config.toml`.
pub const AUTOSTART_STATUS_FILE: &str = "autostart.toml";
const AUTOSTART_STATUS_VERSION: u32 = 1;

/// Result of the last autostart reconcile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutostartStatus {
    /// Value requested by `general.autostart`.
    pub desired_enabled: bool,
    /// Whether the scheduled task is currently registered and enabled.
    pub enabled: bool,
    /// Scheduled task name (stable across updates).
    pub task_name: String,
    /// Last reconcile error, if any. Cleared on success.
    #[serde(default)]
    pub last_error: Option<String>,
}

impl Default for AutostartStatus {
    fn default() -> Self {
        Self {
            desired_enabled: false,
            enabled: false,
            task_name: AUTOSTART_TASK_NAME.to_string(),
            last_error: None,
        }
    }
}

/// On-disk envelope so the settings UI can show freshness and a schema version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutostartStatusFile {
    #[serde(default = "status_version")]
    pub version: u32,
    #[serde(default)]
    pub updated_at_unix_ms: u64,
    pub status: AutostartStatus,
}

fn status_version() -> u32 {
    AUTOSTART_STATUS_VERSION
}

/// `%APPDATA%\OxideMonitor\autostart.toml` (or next to the exe in portable mode).
pub fn autostart_status_path() -> PathBuf {
    super::Config::resolve_path().with_file_name(AUTOSTART_STATUS_FILE)
}

/// Atomically publish the latest reconcile result. Best effort: callers log failures.
pub fn save_autostart_status(status: &AutostartStatus) -> std::io::Result<()> {
    let path = autostart_status_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let updated_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let file = AutostartStatusFile {
        version: AUTOSTART_STATUS_VERSION,
        updated_at_unix_ms,
        status: status.clone(),
    };
    let text = toml::to_string_pretty(&file).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Read the last published status, if the monitor has written one.
pub fn load_autostart_status() -> Option<AutostartStatusFile> {
    let text = fs::read_to_string(autostart_status_path()).ok()?;
    toml::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_defaults_are_disabled_and_named() {
        let status = AutostartStatus::default();
        assert!(!status.desired_enabled);
        assert!(!status.enabled);
        assert_eq!(status.task_name, AUTOSTART_TASK_NAME);
        assert!(status.last_error.is_none());
    }

    #[test]
    fn status_file_roundtrips_with_version_and_timestamp() {
        let file = AutostartStatusFile {
            version: 1,
            updated_at_unix_ms: 42,
            status: AutostartStatus {
                desired_enabled: true,
                enabled: false,
                task_name: AUTOSTART_TASK_NAME.into(),
                last_error: Some("access denied".into()),
            },
        };
        let text = toml::to_string_pretty(&file).unwrap();
        let loaded: AutostartStatusFile = toml::from_str(&text).unwrap();
        assert_eq!(loaded, file);
        assert!(text.contains("[status]"));
    }
}
