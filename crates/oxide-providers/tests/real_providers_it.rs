//! Optional real-provider probe.
//!
//! Reads HWiNFO shared memory when available; never touches the network. Skips gracefully
//! when HWiNFO is not running. Run with `OXIDE_PROVIDER_IT=1`.

#![cfg(windows)]

use oxide_core::{ProviderHealth, ProvidersConfig};
use oxide_providers::ExternalSensors;

#[test]
fn hwinfo_status_probe_never_panics_and_maps_to_a_known_health() {
    if std::env::var_os("OXIDE_PROVIDER_IT").is_none() {
        eprintln!("skipped: set OXIDE_PROVIDER_IT=1 to probe the local HWiNFO provider");
        return;
    }

    let config = ProvidersConfig {
        hwinfo: true,
        lhm_http: false,
        ..ProvidersConfig::default()
    };
    let mut sensors = ExternalSensors::new(config);
    let readings = sensors.poll();
    let statuses = sensors.statuses();
    assert_eq!(statuses.len(), 2);

    let hwinfo = statuses
        .iter()
        .find(|status| status.id == "hwinfo")
        .expect("HWiNFO status");
    assert!(hwinfo.enabled);
    assert!(
        matches!(
            hwinfo.health,
            ProviderHealth::Connected
                | ProviderHealth::NotRunning
                | ProviderHealth::Stale
                | ProviderHealth::Error
        ),
        "unexpected health {:?}",
        hwinfo.health
    );
    if hwinfo.health == ProviderHealth::Connected {
        assert!(!readings.is_empty(), "connected HWiNFO must yield readings");
        assert!(hwinfo.sensor_count > 0);
        assert!(hwinfo.last_update_age_ms.is_some());
    } else {
        assert!(!hwinfo.detail.is_empty(), "failure must explain itself");
    }
}
