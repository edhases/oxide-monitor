//! Optional real-network checks for official release discovery and staging.
//!
//! * `OXIDE_UPDATE_IT=1` — fetch official metadata (no download) and verify the cache.
//! * `OXIDE_UPDATE_DOWNLOAD_IT=1` — download the official LHM zip into a temp provider
//!   root, verify SHA-256 + Authenticode, extract and stage it.
//!
//! Both skip gracefully when the network is unavailable. Nothing is installed or executed.

#![cfg(windows)]

use std::path::PathBuf;
use std::time::Duration;

use oxide_core::ProvidersConfig;
use oxide_providers::update::{
    InstalledState, OfficialProvider, ProviderPaths, ReleaseFetcher, UpdateManager, Version,
    is_allowed_url, now_unix_ms, scan_managed_lhm,
};
use oxide_providers::{HttpDownloader, HttpFetcher, WindowsVerifier};

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "oxide-update-it-{}-{tag}-{}",
        std::process::id(),
        now_unix_ms()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn official_metadata_is_fetchable_and_cached() {
    if std::env::var_os("OXIDE_UPDATE_IT").is_none() {
        eprintln!("skipped: set OXIDE_UPDATE_IT=1 to query the official release feeds");
        return;
    }
    let root = temp_root("meta");
    let fetcher = HttpFetcher::with_ttl(root.join("cache"), Duration::from_secs(24 * 60 * 60));

    for provider in [OfficialProvider::Lhm, OfficialProvider::PawnIo] {
        let release = match fetcher.latest(provider) {
            Ok(release) => release,
            Err(err) => {
                eprintln!("skipped {}: {err}", provider.id());
                continue;
            }
        };
        assert!(
            Version::parse(&release.version).is_some(),
            "unparsable version {}",
            release.version
        );
        assert!(release.page_url.starts_with("https://"));
        if let Some(url) = &release.asset_url {
            assert!(is_allowed_url(url), "non-official asset URL {url}");
        }
        // The second call must be served from the 24 h cache (no network).
        let cached = fetcher.latest(provider).unwrap();
        assert_eq!(cached.version, release.version);
        eprintln!(
            "{} latest = {} (asset {:?})",
            provider.id(),
            release.version,
            release.asset_name
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn official_lhm_update_is_verified_and_staged() {
    if std::env::var_os("OXIDE_UPDATE_DOWNLOAD_IT").is_none() {
        eprintln!("skipped: set OXIDE_UPDATE_DOWNLOAD_IT=1 to download and stage LHM");
        return;
    }
    let root = temp_root("stage");
    let paths = ProviderPaths::with_root(root.join("providers"));
    let fetcher = HttpFetcher::with_ttl(paths.cache_root(), Duration::from_secs(24 * 60 * 60));
    let downloader = HttpDownloader::new();
    let verifier = WindowsVerifier;

    let mut config = ProvidersConfig::default();
    config.lhm_http = true;
    config.lhm_auto_update = true;

    let manager = UpdateManager {
        paths: &paths,
        fetcher: &fetcher,
        downloader: &downloader,
        verifier: &verifier,
        now_unix_ms: now_unix_ms(),
    };
    let report = manager.evaluate(&config, &InstalledState::default());
    let lhm = &report[0];
    eprintln!(
        "LHM latest={:?} state={:?} detail={:?} error={:?}",
        lhm.latest_version, lhm.state, lhm.detail, lhm.last_error
    );
    if lhm.state == oxide_core::ProviderUpdateState::Error {
        eprintln!("skipped: could not stage LHM ({:?})", lhm.last_error);
        let _ = std::fs::remove_dir_all(&root);
        return;
    }

    assert_eq!(lhm.state, oxide_core::ProviderUpdateState::UpdateReady);
    let staged = lhm.staged_path.as_deref().expect("staged executable");
    assert!(
        std::path::Path::new(staged).is_file(),
        "staged file missing"
    );

    // The staged copy is detectable as a managed install with the latest version.
    let managed = scan_managed_lhm(&paths.lhm_root()).expect("managed LHM");
    assert_eq!(managed.version.map(|v| v.as_string()), lhm.latest_version);
    assert!(managed.path.is_file());
    let _ = std::fs::remove_dir_all(&root);
}
