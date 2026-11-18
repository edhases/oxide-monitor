//! Official version/update status for the external temperature providers.
//!
//! The monitor only discovers versions and stages updates; it never installs a driver
//! and never executes a downloaded binary. The settings UI renders these entries in the
//! Sources tab.

use serde::{Deserialize, Serialize};

/// Official LHM release page (used for manual updates and the UI link).
pub const LHM_OFFICIAL_URL: &str =
    "https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/releases";
/// Official PawnIO site. PawnIO is a kernel driver and is never installed by the monitor.
pub const PAWNIO_OFFICIAL_URL: &str = "https://pawnio.eu";

/// How the detected installed copy is managed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderInstallKind {
    /// Nothing installed/detected.
    #[default]
    None,
    /// Portable copy under `%LOCALAPPDATA%\OxideMonitor\providers\LHM\<version>`.
    Managed,
    /// A copy running from another path (never modified by the monitor).
    External,
    /// Kernel driver installed by the official PawnIO installer.
    Driver,
}

/// Lifecycle of the version check / staged update.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUpdateState {
    #[default]
    Unknown,
    UpToDate,
    UpdateAvailable,
    /// A verified update is staged on disk and waiting for the user to start it.
    UpdateReady,
    Updating,
    Error,
}

/// One row in the Sources tab.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderUpdateStatus {
    /// `lhm` or `pawnio`.
    pub id: String,
    pub display_name: String,
    pub installed_version: Option<String>,
    pub latest_version: Option<String>,
    pub install_kind: ProviderInstallKind,
    pub state: ProviderUpdateState,
    pub auto_update: bool,
    /// Official release/site link for manual updates.
    pub official_url: String,
    /// Staged executable when `state == update_ready`.
    pub staged_path: Option<String>,
    pub detail: String,
    pub last_error: Option<String>,
    pub checked_at_unix_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_status_serde_defaults_are_backward_compatible() {
        let status = ProviderUpdateStatus {
            id: "lhm".into(),
            display_name: "LibreHardwareMonitor".into(),
            installed_version: Some("0.9.5".into()),
            latest_version: Some("0.9.6".into()),
            install_kind: ProviderInstallKind::Managed,
            state: ProviderUpdateState::UpdateAvailable,
            auto_update: true,
            official_url: LHM_OFFICIAL_URL.into(),
            staged_path: None,
            detail: "enable auto-update".into(),
            last_error: None,
            checked_at_unix_ms: Some(1),
        };
        let text = serde_json::to_string(&status).unwrap();
        let back: ProviderUpdateStatus = serde_json::from_str(&text).unwrap();
        assert_eq!(back, status);

        // An older payload without optional fields decodes with defaults.
        let legacy: ProviderUpdateStatus =
            serde_json::from_str(r#"{"id":"pawnio","display_name":"PawnIO"}"#).unwrap();
        assert_eq!(legacy.state, ProviderUpdateState::Unknown);
        assert_eq!(legacy.install_kind, ProviderInstallKind::None);
        assert!(!legacy.auto_update);
    }
}
