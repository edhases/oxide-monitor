//! Optional elevated PawnIO CPU-temperature helper.
//!
//! The monitor never reads PawnIO directly: the driver's device object denies
//! non-elevated access. A small `requireAdministrator` helper is started only by an
//! explicit user action, serves a per-user Named Pipe, and exposes a fixed
//! read-only command set (Hello / ReadCpuTemp / Shutdown).
//!
//! This module is the serializable status surface for the Settings UI and the
//! monitor snapshot. It contains no Windows APIs so `oxide-settings` can depend on
//! it without pulling in the helper or provider crates.

use serde::{Deserialize, Serialize};

use crate::provider_update::PAWNIO_OFFICIAL_URL;

/// Named Pipe prefix; the current user SID is appended by both sides.
pub const PAWNIO_HELPER_PIPE_PREFIX: &str = r"\\.\pipe\oxide-pawnio-v1-";

/// Lifecycle of the optional PawnIO helper.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PawnioState {
    /// `providers.pawnio` is off (the default).
    #[default]
    Disabled,
    /// The PawnIO driver/install was not detected.
    NotInstalled,
    /// Installed and enabled, but the helper is not running. The user must press
    /// Enable (or re-enable) to trigger the UAC prompt.
    Stopped,
    /// The helper process was launched and the monitor is waiting for its pipe.
    Starting,
    /// Connected and returning CPU temperature at 1 Hz.
    Running,
    /// The user declined the UAC prompt; the monitor will not retry silently.
    Denied,
    Error,
}

/// Serializable status row for the Sources tab and the live snapshot/inventory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PawnioStatus {
    /// The PawnIO driver/install was detected (read-only registry/service check).
    pub installed: bool,
    /// Installed PawnIO package version when known.
    pub installed_version: Option<String>,
    /// Mirrors `providers.pawnio`.
    pub enabled: bool,
    pub state: PawnioState,
    /// Last CPU Tctl/Tdie reading while `state == running`.
    pub temperature_c: Option<f32>,
    /// Helper crate version (not the PawnIO driver version).
    pub helper_version: Option<String>,
    /// Official PawnIO site; the monitor never installs or updates the driver.
    pub official_url: String,
    /// Human-readable status/detail for the UI.
    pub detail: String,
    pub last_error: Option<String>,
    pub updated_at_unix_ms: Option<u64>,
}

impl Default for PawnioStatus {
    fn default() -> Self {
        Self {
            installed: false,
            installed_version: None,
            enabled: false,
            state: PawnioState::Disabled,
            temperature_c: None,
            helper_version: None,
            official_url: PAWNIO_OFFICIAL_URL.to_string(),
            detail: String::new(),
            last_error: None,
            updated_at_unix_ms: None,
        }
    }
}

/// Explicit user request sent from Settings to the monitor over the normal IPC.
/// Config alone is not enough: the helper must never start at monitor startup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PawnioCommand {
    Start,
    Stop,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_defaults_are_disabled_and_backward_compatible() {
        let status = PawnioStatus::default();
        assert!(!status.installed);
        assert!(!status.enabled);
        assert_eq!(status.state, PawnioState::Disabled);
        assert_eq!(status.official_url, PAWNIO_OFFICIAL_URL);

        let legacy: PawnioStatus = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy, status);
    }

    #[test]
    fn status_roundtrips_all_states() {
        for state in [
            PawnioState::Disabled,
            PawnioState::NotInstalled,
            PawnioState::Stopped,
            PawnioState::Starting,
            PawnioState::Running,
            PawnioState::Denied,
            PawnioState::Error,
        ] {
            let status = PawnioStatus {
                installed: true,
                installed_version: Some("2.1.0.0".into()),
                enabled: true,
                state,
                temperature_c: (state == PawnioState::Running).then_some(72.0),
                helper_version: Some("0.1.0".into()),
                ..PawnioStatus::default()
            };
            let text = serde_json::to_string(&status).unwrap();
            let back: PawnioStatus = serde_json::from_str(&text).unwrap();
            assert_eq!(back, status);
            assert!(text.contains("pawnio.eu"));
        }
    }
}
