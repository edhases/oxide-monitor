//! Optional external temperature providers: HWiNFO shared memory and LHM HTTP.
//!
//! Temperatures for the CPU, mainboard and SATA disks cannot be read from user mode
//! without a kernel driver. Oxide Monitor never loads one; instead it reads the data that
//! an already-running monitoring tool publishes:
//!
//! 1. **HWiNFO** shared memory (`Global\HWiNFO_SENS_SM2`) — preferred: structured, cheap,
//!    non-admin, no network.
//! 2. **LibreHardwareMonitor** HTTP (`/metrics`, `/data.json` fallback) — loopback only.
//!
//! Provider selection is per tick and never blocks for more than ~500 ms per request: if
//! HWiNFO is enabled and yields at least one temperature it wins, otherwise LHM is tried,
//! otherwise all slots stay `None` and the UI shows `—`.
//!
//! When HWiNFO supplies readings, an enabled LHM endpoint is still probed every few
//! seconds so its [`ProviderStatus`] stays accurate without paying the HTTP timeout on
//! every tick.
//!
//! The reader can be reconfigured at runtime ([`ExternalSensors::reconfigure`]) so the
//! Settings toggles take effect without restarting the monitor.

mod heuristics;
mod hwinfo;
mod lhm;
pub mod update;
mod update_http;
mod update_windows;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use oxide_core::{ProviderHealth, ProviderStatus, ProvidersConfig};

pub use update_http::{HttpDownloader, HttpFetcher};
pub use update_windows::{WindowsVerifier, detect_pawnio, find_running_lhm};

use hwinfo::{HwinfoError, HwinfoReader};
use lhm::{LhmClient, LhmError};

/// While HWiNFO is healthy, still probe LHM this often to refresh its status.
const LHM_STATUS_PROBE_INTERVAL: Duration = Duration::from_secs(5);

/// Temperatures produced by an external provider. `None` means "no data", never a guess.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderReadings {
    pub cpu_temp_c: Option<f32>,
    pub gpu_temp_c: Option<f32>,
    pub disk_temp_c: Option<f32>,
    pub mainboard_temp_c: Option<f32>,
}

impl ProviderReadings {
    pub fn is_empty(&self) -> bool {
        self.cpu_temp_c.is_none()
            && self.gpu_temp_c.is_none()
            && self.disk_temp_c.is_none()
            && self.mainboard_temp_c.is_none()
    }

    /// Number of temperature series with data (0..=4); reported as `sensor_count`.
    pub fn sensor_count(&self) -> usize {
        [
            self.cpu_temp_c,
            self.gpu_temp_c,
            self.disk_temp_c,
            self.mainboard_temp_c,
        ]
        .iter()
        .filter(|value| value.is_some())
        .count()
    }
}

/// Which external source produced the last successful poll.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProviderSource {
    #[default]
    None,
    Hwinfo,
    Lhm,
}

impl ProviderSource {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Hwinfo => "hwinfo",
            Self::Lhm => "lhm",
        }
    }
}

/// Log-visible provider state; compared by value so `tracing` fires once per transition.
#[derive(Clone, Debug, PartialEq)]
enum State {
    Hwinfo,
    Lhm,
    Unavailable(String),
}

/// Per-provider health, last success and detail for the status surface.
#[derive(Clone, Debug)]
struct ProviderTrack {
    health: ProviderHealth,
    detail: String,
    sensor_count: usize,
    last_success: Option<Instant>,
}

impl Default for ProviderTrack {
    fn default() -> Self {
        Self {
            health: ProviderHealth::Disabled,
            detail: "disabled in settings".to_string(),
            sensor_count: 0,
            last_success: None,
        }
    }
}

impl ProviderTrack {
    fn pending(&mut self) {
        self.health = ProviderHealth::NotRunning;
        self.detail = "waiting for the first poll".to_string();
        self.sensor_count = 0;
    }

    fn disable(&mut self) {
        self.health = ProviderHealth::Disabled;
        self.detail = "disabled in settings".to_string();
        self.sensor_count = 0;
        self.last_success = None;
    }

    fn succeeded(&mut self, readings: &ProviderReadings, now: Instant) {
        self.health = ProviderHealth::Connected;
        self.detail.clear();
        self.sensor_count = readings.sensor_count();
        self.last_success = Some(now);
    }

    fn failed(&mut self, health: ProviderHealth, detail: impl Into<String>) {
        self.health = health;
        self.detail = detail.into();
        self.sensor_count = 0;
    }

    fn to_status(&self, id: &str, display_name: &str, enabled: bool) -> ProviderStatus {
        ProviderStatus {
            id: id.to_string(),
            display_name: display_name.to_string(),
            enabled,
            health: if enabled {
                self.health
            } else {
                ProviderHealth::Disabled
            },
            sensor_count: self.sensor_count,
            detail: self.detail.clone(),
            last_update_age_ms: self
                .last_success
                .map(|instant| instant.elapsed().as_millis() as u64),
        }
    }
}

pub struct ExternalSensors {
    config: ProvidersConfig,
    hwinfo: HwinfoReader,
    lhm: Option<LhmClient>,
    /// Set once when the configured LHM URL is rejected (e.g. non-loopback host).
    lhm_url_error: Option<String>,
    state: Option<State>,
    source: ProviderSource,
    hwinfo_track: ProviderTrack,
    lhm_track: ProviderTrack,
    last_lhm_probe: Option<Instant>,
}

impl ExternalSensors {
    pub fn new(config: ProvidersConfig) -> Self {
        let mut sensors = Self {
            config,
            hwinfo: HwinfoReader::new(),
            lhm: None,
            lhm_url_error: None,
            state: None,
            source: ProviderSource::None,
            hwinfo_track: ProviderTrack::default(),
            lhm_track: ProviderTrack::default(),
            last_lhm_probe: None,
        };
        sensors.refresh_tracks_for_config();
        sensors
    }

    /// Apply a new provider config without restarting the monitor.
    ///
    /// Disabling a provider drops its mapping/client immediately so polling stops now;
    /// enabling or changing the URL creates a fresh reader on the next poll.
    pub fn reconfigure(&mut self, config: ProvidersConfig) {
        if self.config == config {
            return;
        }
        let hwinfo_changed = self.config.hwinfo != config.hwinfo;
        let lhm_changed =
            self.config.lhm_http != config.lhm_http || self.config.lhm_url != config.lhm_url;
        self.config = config;

        if hwinfo_changed {
            self.hwinfo = HwinfoReader::new();
        }
        if lhm_changed {
            self.lhm = None;
            self.lhm_url_error = None;
            self.last_lhm_probe = None;
        }
        self.refresh_tracks_for_config();
        self.state = None;
        self.source = ProviderSource::None;

        tracing::info!(
            target: "oxide_providers",
            hwinfo = self.config.hwinfo,
            lhm_http = self.config.lhm_http,
            lhm_url = %self.config.lhm_url,
            "external temperature providers reconfigured"
        );
    }

    /// Read the best available external temperatures. Returns all-`None` when no provider
    /// is enabled, installed or healthy. Also refreshes per-provider [`ProviderStatus`].
    pub fn poll(&mut self) -> ProviderReadings {
        let now = Instant::now();
        let now_unix = unix_now();
        let mut hwinfo_readings: Option<ProviderReadings> = None;
        let mut lhm_readings: Option<ProviderReadings> = None;
        let mut reasons: Vec<String> = Vec::new();

        if self.config.hwinfo {
            match self.hwinfo.poll(now_unix) {
                Ok(readings) if !readings.is_empty() => {
                    self.hwinfo_track.succeeded(&readings, now);
                    hwinfo_readings = Some(readings);
                }
                Ok(_) => {
                    self.hwinfo_track
                        .failed(ProviderHealth::Error, "no temperature readings");
                    reasons.push("HWiNFO: no temperature readings".to_string());
                }
                Err(err) => {
                    let (health, detail) = hwinfo_health(&err);
                    self.hwinfo_track.failed(health, detail.clone());
                    reasons.push(format!("HWiNFO: {detail}"));
                }
            }
        } else {
            self.hwinfo_track.disable();
        }

        if self.config.lhm_http {
            let due = hwinfo_readings.is_none()
                || self
                    .last_lhm_probe
                    .is_none_or(|last| now.duration_since(last) >= LHM_STATUS_PROBE_INTERVAL);
            if due {
                self.last_lhm_probe = Some(now);
                self.ensure_lhm();
                match self.lhm.as_mut().map(LhmClient::poll) {
                    Some(Ok(readings)) if !readings.is_empty() => {
                        self.lhm_track.succeeded(&readings, now);
                        lhm_readings = Some(readings);
                    }
                    Some(Ok(_)) => {
                        self.lhm_track
                            .failed(ProviderHealth::Error, "no temperature readings");
                        reasons.push("LHM: no temperature readings".to_string());
                    }
                    Some(Err(err)) => {
                        let (health, detail) = lhm_health(&err);
                        self.lhm_track.failed(health, detail.clone());
                        reasons.push(format!("LHM: {detail}"));
                    }
                    None => {
                        if let Some(error) = &self.lhm_url_error {
                            let detail = format!("refusing endpoint: {error}");
                            self.lhm_track.failed(ProviderHealth::Error, detail.clone());
                            reasons.push(format!("LHM: {detail}"));
                        } else {
                            self.lhm_track
                                .failed(ProviderHealth::NotRunning, "client unavailable");
                            reasons.push("LHM: client unavailable".to_string());
                        }
                    }
                }
            }
        } else {
            self.lhm_track.disable();
        }

        match (hwinfo_readings, lhm_readings) {
            (Some(readings), _) => {
                self.set_state(State::Hwinfo);
                readings
            }
            (None, Some(readings)) => {
                self.set_state(State::Lhm);
                readings
            }
            (None, None) => {
                let reason = if !self.config.hwinfo && !self.config.lhm_http {
                    "temperature providers disabled".to_string()
                } else {
                    reasons
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "no external temperature provider available".to_string())
                };
                self.set_state(State::Unavailable(reason));
                ProviderReadings::default()
            }
        }
    }

    /// Source of the most recent successful poll.
    pub fn source(&self) -> ProviderSource {
        self.source
    }

    /// Current HWiNFO/LHM status, in UI order.
    pub fn statuses(&self) -> Vec<ProviderStatus> {
        vec![
            self.hwinfo_track
                .to_status("hwinfo", "HWiNFO", self.config.hwinfo),
            self.lhm_track
                .to_status("lhm_http", "LibreHardwareMonitor", self.config.lhm_http),
        ]
    }

    fn refresh_tracks_for_config(&mut self) {
        if self.config.hwinfo {
            self.hwinfo_track.pending();
        } else {
            self.hwinfo_track.disable();
        }
        if self.config.lhm_http {
            self.lhm_track.pending();
        } else {
            self.lhm_track.disable();
        }
    }

    fn ensure_lhm(&mut self) {
        if self.lhm.is_some() || self.lhm_url_error.is_some() {
            return;
        }
        match LhmClient::new(&self.config.lhm_url) {
            Ok(client) => self.lhm = Some(client),
            Err(err) => {
                tracing::warn!(
                    target: "oxide_providers",
                    url = %self.config.lhm_url,
                    error = %err,
                    "refusing to use LHM endpoint"
                );
                self.lhm_url_error = Some(err.to_string());
            }
        }
    }

    fn set_state(&mut self, state: State) {
        if self.state.as_ref() == Some(&state) {
            return;
        }

        let was_active = matches!(self.state.as_ref(), Some(State::Hwinfo) | Some(State::Lhm));

        match &state {
            State::Hwinfo => {
                self.source = ProviderSource::Hwinfo;
                tracing::info!(
                    target: "oxide_providers",
                    provider = "hwinfo",
                    "external temperature provider active"
                );
            }
            State::Lhm => {
                self.source = ProviderSource::Lhm;
                tracing::info!(
                    target: "oxide_providers",
                    provider = "lhm",
                    "external temperature provider active"
                );
            }
            State::Unavailable(reason) => {
                self.source = ProviderSource::None;
                if was_active {
                    tracing::warn!(
                        target: "oxide_providers",
                        reason = %reason,
                        "external temperature provider became unavailable"
                    );
                } else {
                    tracing::debug!(
                        target: "oxide_providers",
                        reason = %reason,
                        "no external temperature provider"
                    );
                }
            }
        }

        self.state = Some(state);
    }
}

/// Map a HWiNFO reader error to the UI health value and a human-readable detail.
fn hwinfo_health(err: &HwinfoError) -> (ProviderHealth, String) {
    match err {
        HwinfoError::NotRunning => (
            ProviderHealth::NotRunning,
            "shared memory not found (start HWiNFO with Shared Memory Support)".to_string(),
        ),
        HwinfoError::AccessDenied => (
            ProviderHealth::Error,
            "access denied to shared memory".to_string(),
        ),
        HwinfoError::Stale => (ProviderHealth::Stale, "shared memory is stale".to_string()),
        HwinfoError::Rebuild => (
            ProviderHealth::Error,
            "shared memory layout changed".to_string(),
        ),
        other => (ProviderHealth::Error, other.to_string()),
    }
}

/// Map an LHM client error to the UI health value and a human-readable detail.
fn lhm_health(err: &LhmError) -> (ProviderHealth, String) {
    match err {
        LhmError::NotLoopback(host) => (
            ProviderHealth::Error,
            format!("refused non-loopback host '{host}'"),
        ),
        LhmError::InvalidUrl(message) => (ProviderHealth::Error, format!("invalid URL: {message}")),
        LhmError::NotRunning(endpoint) => (
            ProviderHealth::NotRunning,
            format!(
                "LHM web server is not running at {endpoint} \
                 (start LHM → Options → Remote Web Server → Run)"
            ),
        ),
        LhmError::Network(message) if is_connection_refused(message) => (
            ProviderHealth::NotRunning,
            "no LHM web server at the loopback URL".to_string(),
        ),
        LhmError::Network(message) => (ProviderHealth::Error, message.clone()),
        LhmError::HttpStatus(404) => (
            ProviderHealth::NotRunning,
            "LHM endpoint not found (404) — enable the LHM web server".to_string(),
        ),
        LhmError::HttpStatus(status) => (ProviderHealth::Error, format!("HTTP {status}")),
        LhmError::Malformed(message) => (ProviderHealth::Error, message.to_string()),
        LhmError::Timeout(budget_ms) => (
            ProviderHealth::Stale,
            format!("LHM did not respond within {budget_ms} ms"),
        ),
    }
}

fn is_connection_refused(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("refused") || message.contains("os error 10061")
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status<'a>(statuses: &'a [ProviderStatus], id: &str) -> &'a ProviderStatus {
        statuses
            .iter()
            .find(|status| status.id == id)
            .expect("provider status")
    }

    #[test]
    fn disabled_providers_report_disabled_status() {
        let mut sensors = ExternalSensors::new(ProvidersConfig::default());
        let readings = sensors.poll();
        assert!(readings.is_empty());
        assert_eq!(sensors.source(), ProviderSource::None);

        let statuses = sensors.statuses();
        assert_eq!(statuses.len(), 2);
        for provider in &statuses {
            assert!(!provider.enabled);
            assert_eq!(provider.health, ProviderHealth::Disabled);
            assert_eq!(provider.sensor_count, 0);
            assert!(provider.last_update_age_ms.is_none());
        }
    }

    #[test]
    fn reconfigure_toggles_providers_without_restart() {
        let mut sensors = ExternalSensors::new(ProvidersConfig::default());
        assert_eq!(sensors.statuses()[0].health, ProviderHealth::Disabled);

        let mut config = ProvidersConfig::default();
        config.hwinfo = true;
        sensors.reconfigure(config.clone());
        let statuses = sensors.statuses();
        assert!(statuses[0].enabled);
        assert_ne!(statuses[0].health, ProviderHealth::Disabled);
        assert!(!statuses[1].enabled);

        // Disabling must flip the status back and stop using the source.
        config.hwinfo = false;
        sensors.reconfigure(config);
        let statuses = sensors.statuses();
        assert!(!statuses[0].enabled);
        assert_eq!(statuses[0].health, ProviderHealth::Disabled);
        assert_eq!(sensors.source(), ProviderSource::None);
    }

    #[test]
    fn hwinfo_health_mapping_covers_every_error() {
        for (error, expected) in [
            (HwinfoError::NotRunning, ProviderHealth::NotRunning),
            (HwinfoError::AccessDenied, ProviderHealth::Error),
            (HwinfoError::Stale, ProviderHealth::Stale),
            (HwinfoError::Rebuild, ProviderHealth::Error),
            (HwinfoError::Io("io".into()), ProviderHealth::Error),
            (HwinfoError::Malformed("bad"), ProviderHealth::Error),
        ] {
            let (health, detail) = hwinfo_health(&error);
            assert_eq!(health, expected, "for {error}");
            assert!(!detail.is_empty());
        }
    }

    #[test]
    fn lhm_health_mapping_covers_every_error() {
        for (error, expected) in [
            (
                LhmError::NotLoopback("10.0.0.5".into()),
                ProviderHealth::Error,
            ),
            (LhmError::InvalidUrl("nope".into()), ProviderHealth::Error),
            (
                LhmError::NotRunning("127.0.0.1:8085".into()),
                ProviderHealth::NotRunning,
            ),
            (
                LhmError::Network("Connection refused (os error 10061)".into()),
                ProviderHealth::NotRunning,
            ),
            (
                LhmError::Network("dns failure".into()),
                ProviderHealth::Error,
            ),
            (LhmError::HttpStatus(404), ProviderHealth::NotRunning),
            (LhmError::HttpStatus(500), ProviderHealth::Error),
            (LhmError::Malformed("empty"), ProviderHealth::Error),
            (LhmError::Timeout(500), ProviderHealth::Stale),
        ] {
            let (health, detail) = lhm_health(&error);
            assert_eq!(health, expected, "for {error}");
            assert!(!detail.is_empty());
        }

        // The not-running detail must tell the user what to do and name the endpoint.
        let (health, detail) = lhm_health(&LhmError::NotRunning("127.0.0.1:8085".into()));
        assert_eq!(health, ProviderHealth::NotRunning);
        assert!(detail.contains("127.0.0.1:8085"));
        assert!(detail.contains("Remote Web Server"));

        // Timeouts are stale (server accepted the connection but did not answer).
        let (health, detail) = lhm_health(&LhmError::Timeout(500));
        assert_eq!(health, ProviderHealth::Stale);
        assert!(detail.contains("500 ms"));
    }

    #[test]
    fn non_loopback_lhm_url_is_refused_never_polled_and_reported() {
        let mut config = ProvidersConfig::default();
        config.lhm_http = true;
        config.lhm_url = "http://10.0.0.5:8085/metrics".to_string();

        let mut sensors = ExternalSensors::new(config);
        // Must return immediately, without attempting a network request.
        let readings = sensors.poll();
        assert!(readings.is_empty());
        assert_eq!(sensors.source(), ProviderSource::None);

        let statuses = sensors.statuses();
        let lhm = status(&statuses, "lhm_http");
        assert!(lhm.enabled);
        assert_eq!(lhm.health, ProviderHealth::Error);
        assert!(
            lhm.detail.contains("non-loopback"),
            "detail was {:?}",
            lhm.detail
        );
    }

    #[test]
    fn hwinfo_status_is_reported_without_hwinfo_running() {
        // On any machine without HWiNFO this maps to NotRunning/Error; never panics and
        // always reports the provider so the Settings UI can show its state.
        let mut config = ProvidersConfig::default();
        config.hwinfo = true;
        let mut sensors = ExternalSensors::new(config);
        let _ = sensors.poll();
        let statuses = sensors.statuses();
        let hwinfo = status(&statuses, "hwinfo");
        assert!(hwinfo.enabled);
        assert_eq!(hwinfo.display_name, "HWiNFO");
        assert!(matches!(
            hwinfo.health,
            ProviderHealth::Connected
                | ProviderHealth::NotRunning
                | ProviderHealth::Stale
                | ProviderHealth::Error
        ));
    }

    #[test]
    fn sensor_count_matches_available_temperature_series() {
        let readings = ProviderReadings {
            cpu_temp_c: Some(50.0),
            disk_temp_c: Some(40.0),
            ..ProviderReadings::default()
        };
        assert_eq!(readings.sensor_count(), 2);
        assert_eq!(ProviderReadings::default().sensor_count(), 0);
    }
}
