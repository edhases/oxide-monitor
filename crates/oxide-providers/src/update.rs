//! Official version discovery and safe staging for PawnIO / LibreHardwareMonitor.
//!
//! Policy:
//! * Only the official GitHub release APIs (`LibreHardwareMonitor/LibreHardwareMonitor` and
//!   `namazso/PawnIO.Setup`) and their download hosts are contacted over HTTPS.
//! * LHM is staged as a portable copy under `%LOCALAPPDATA%\OxideMonitor\providers\LHM\<version>`.
//!   A user-installed/running LHM is never touched; it is reported as “update manually”.
//! * PawnIO is a kernel driver: only version detection and the official link are provided.
//! * Nothing is executed and no LHM process is stopped or restarted.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use oxide_core::{
    LHM_OFFICIAL_URL, PAWNIO_OFFICIAL_URL, ProviderInstallKind, ProviderUpdateState,
    ProviderUpdateStatus, ProvidersConfig,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Official release metadata is cached for one day (GitHub unauthenticated rate limit).
pub const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
pub const MAX_DOWNLOAD_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_EXTRACTED_BYTES: u64 = 256 * 1024 * 1024;
pub const LHM_MANAGED_EXE: &str = "LibreHardwareMonitor.exe";
pub const USER_AGENT: &str = concat!("OxideMonitor/", env!("CARGO_PKG_VERSION"));

/// Hosts allowed for version checks and downloads (HTTPS only).
pub const ALLOWED_HOSTS: &[&str] = &[
    "api.github.com",
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OfficialProvider {
    Lhm,
    PawnIo,
}

impl OfficialProvider {
    pub fn id(self) -> &'static str {
        match self {
            Self::Lhm => "lhm",
            Self::PawnIo => "pawnio",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Lhm => "LibreHardwareMonitor",
            Self::PawnIo => "PawnIO",
        }
    }

    /// Verified official release feed.
    pub fn release_api_url(self) -> &'static str {
        match self {
            Self::Lhm => {
                "https://api.github.com/repos/LibreHardwareMonitor/LibreHardwareMonitor/releases/latest"
            }
            // `namazso/PawnIO` is the source repo and has no releases; the official
            // installer releases live in `namazso/PawnIO.Setup` (linked from pawnio.eu).
            Self::PawnIo => "https://api.github.com/repos/namazso/PawnIO.Setup/releases/latest",
        }
    }

    pub fn official_url(self) -> &'static str {
        match self {
            Self::Lhm => LHM_OFFICIAL_URL,
            Self::PawnIo => PAWNIO_OFFICIAL_URL,
        }
    }

    pub fn cache_file(self) -> &'static str {
        match self {
            Self::Lhm => "lhm-latest.json",
            Self::PawnIo => "pawnio-latest.json",
        }
    }
}

/// Comparable dotted version (`v0.9.6`, `0.9.6.0`, `2.2.0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub build: u32,
}

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let core = text.split(['-', '+']).next()?.trim();
        if core.is_empty() {
            return None;
        }
        let mut parts = [0u32; 4];
        let mut count = 0usize;
        for (index, part) in core.split('.').enumerate() {
            if index >= parts.len() || part.is_empty() {
                return None;
            }
            parts[index] = part.parse().ok()?;
            count = index + 1;
        }
        (count >= 2).then_some(Self {
            major: parts[0],
            minor: parts[1],
            patch: parts[2],
            build: parts[3],
        })
    }

    pub fn as_string(self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_string())
    }
}

/// Cached official release metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReleaseInfo {
    pub version: String,
    pub page_url: String,
    #[serde(default)]
    pub asset_name: Option<String>,
    #[serde(default)]
    pub asset_url: Option<String>,
    /// Lowercase hex SHA-256 published by GitHub for the asset, when present.
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub fetched_at_unix_ms: u64,
}

impl ReleaseInfo {
    pub fn version_parsed(&self) -> Option<Version> {
        Version::parse(&self.version)
    }
}

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("network error: {0}")]
    Network(String),
    #[error("HTTP {0}")]
    Http(u16),
    #[error("GitHub rate limit reached; using the cached result")]
    RateLimited,
    #[error("URL is not an official HTTPS source: {0}")]
    NotAllowed(String),
    #[error("invalid release metadata: {0}")]
    Parse(String),
    #[error("verification failed: {0}")]
    Verify(String),
    #[error("I/O error: {0}")]
    Io(String),
}

impl From<io::Error> for UpdateError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

/// Fetches the latest official release metadata (cached by implementations).
pub trait ReleaseFetcher {
    fn latest(&self, provider: OfficialProvider) -> Result<ReleaseInfo, UpdateError>;
}

/// Downloads an official asset to `dest`.
pub trait ArtifactDownloader {
    fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError>;
}

/// Authenticode outcome. LHM's official release is currently **unsigned**, so “unsigned”
/// is distinct from a broken signature: it is only accepted when the published SHA-256
/// matched. A present-but-invalid signature is always rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignatureStatus {
    Trusted,
    Unsigned,
}

/// Cryptographic verification used before a download is staged.
pub trait ArtifactVerifier {
    fn sha256_file(&self, path: &Path) -> Result<[u8; 32], UpdateError>;
    fn verify_authenticode(&self, path: &Path) -> Result<SignatureStatus, UpdateError>;
}

/// Per-user provider data root.
#[derive(Clone, Debug)]
pub struct ProviderPaths {
    root: PathBuf,
}

impl ProviderPaths {
    /// `%LOCALAPPDATA%\OxideMonitor\providers` (temp dir fallback).
    pub fn per_user() -> Self {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        Self {
            root: base.join("OxideMonitor").join("providers"),
        }
    }

    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn lhm_root(&self) -> PathBuf {
        self.root.join("LHM")
    }

    pub fn cache_root(&self) -> PathBuf {
        self.root.join("cache")
    }

    pub fn downloads_root(&self) -> PathBuf {
        self.root.join("downloads")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InstalledLhm {
    pub version: Option<Version>,
    pub path: PathBuf,
    /// True when LHM runs from a user-managed path (never modified).
    pub external: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InstalledPawnIo {
    pub version: Option<Version>,
    pub path: Option<PathBuf>,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InstalledState {
    pub lhm: Option<InstalledLhm>,
    pub pawnio: Option<InstalledPawnIo>,
}

/// Highest complete portable copy under `<root>/LHM/<version>/LibreHardwareMonitor.exe`.
pub fn scan_managed_lhm(lhm_root: &Path) -> Option<InstalledLhm> {
    let mut best: Option<(Version, PathBuf)> = None;
    for entry in fs::read_dir(lhm_root).ok()?.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(version) = Version::parse(&name) else {
            continue;
        };
        let exe = entry.path().join(LHM_MANAGED_EXE);
        if !exe.is_file() {
            continue;
        }
        if best.as_ref().is_none_or(|(current, _)| version > *current) {
            best = Some((version, exe));
        }
    }
    best.map(|(version, path)| InstalledLhm {
        version: Some(version),
        path,
        external: false,
    })
}

/// Prefer a running LHM outside the managed folder (never touched); otherwise use the
/// highest managed copy so an already staged version is not downloaded again just
/// because an older managed copy happens to be running.
pub fn resolve_installed_lhm(
    managed: Option<InstalledLhm>,
    running: Option<InstalledLhm>,
    lhm_root: &Path,
) -> Option<InstalledLhm> {
    match running {
        Some(running) if !running.path.starts_with(lhm_root) => Some(running),
        Some(mut running) => {
            running.external = false;
            managed.or(Some(running))
        }
        None => managed,
    }
}

/// Evaluate both providers and, when allowed, stage an LHM update.
pub struct UpdateManager<'a> {
    pub paths: &'a ProviderPaths,
    pub fetcher: &'a dyn ReleaseFetcher,
    pub downloader: &'a dyn ArtifactDownloader,
    pub verifier: &'a dyn ArtifactVerifier,
    pub now_unix_ms: u64,
}

impl UpdateManager<'_> {
    pub fn evaluate(
        &self,
        config: &ProvidersConfig,
        installed: &InstalledState,
    ) -> Vec<ProviderUpdateStatus> {
        vec![
            self.evaluate_lhm(config, installed),
            self.evaluate_pawnio(installed),
        ]
    }

    fn evaluate_lhm(
        &self,
        config: &ProvidersConfig,
        installed: &InstalledState,
    ) -> ProviderUpdateStatus {
        let mut status = base_status(
            OfficialProvider::Lhm,
            config.lhm_auto_update,
            self.now_unix_ms,
        );
        if let Some(current) = &installed.lhm {
            status.installed_version = current.version.map(|v| v.as_string());
            status.install_kind = if current.external {
                ProviderInstallKind::External
            } else {
                ProviderInstallKind::Managed
            };
        }

        let latest = match self.fetcher.latest(OfficialProvider::Lhm) {
            Ok(latest) => latest,
            Err(err) => {
                return failed(
                    status,
                    err,
                    "could not check the latest official LHM version",
                );
            }
        };
        status.latest_version = latest
            .version_parsed()
            .map(|v| v.as_string())
            .or_else(|| Some(latest.version.clone()));
        let Some(latest_version) = latest.version_parsed() else {
            return failed(
                status,
                UpdateError::Parse(format!("unrecognised LHM version '{}'", latest.version)),
                "could not parse the latest official LHM version",
            );
        };

        if installed
            .lhm
            .as_ref()
            .and_then(|current| current.version)
            .is_some_and(|current| current >= latest_version)
        {
            status.state = ProviderUpdateState::UpToDate;
            status.detail = "up to date".to_string();
            return status;
        }

        status.state = ProviderUpdateState::UpdateAvailable;
        if let Some(current) = installed.lhm.as_ref().filter(|current| current.external) {
            status.detail = format!(
                "LHM is running from {}; update it manually from the official releases page",
                current.path.display()
            );
            return status;
        }
        if !config.lhm_auto_update {
            status.detail = "update available; enable “auto-update” to download it".to_string();
            return status;
        }
        if !config.lhm_http {
            status.detail =
                "update available; enable the LibreHardwareMonitor provider first".to_string();
            return status;
        }

        match self.stage_lhm(&latest, latest_version) {
            Ok((path, signature)) => {
                status.state = ProviderUpdateState::UpdateReady;
                status.staged_path = Some(path.display().to_string());
                status.detail = match signature {
                    SignatureStatus::Trusted => {
                        "verified update staged; start LHM from the staged folder when ready"
                            .to_string()
                    }
                    SignatureStatus::Unsigned => {
                        "staged and verified by the published SHA-256; the official LHM \
                         executable is not Authenticode-signed"
                            .to_string()
                    }
                };
                status.last_error = None;
                tracing::info!(
                    target: "oxide_providers",
                    version = %latest_version,
                    path = %path.display(),
                    "staged a verified LibreHardwareMonitor update"
                );
            }
            Err(err) => {
                status.state = ProviderUpdateState::Error;
                status.detail = "could not stage the update".to_string();
                status.last_error = Some(err.to_string());
                tracing::warn!(
                    target: "oxide_providers",
                    %err,
                    "failed to stage LibreHardwareMonitor"
                );
            }
        }
        status
    }

    fn evaluate_pawnio(&self, installed: &InstalledState) -> ProviderUpdateStatus {
        let mut status = base_status(OfficialProvider::PawnIo, false, self.now_unix_ms);
        if let Some(current) = &installed.pawnio {
            status.installed_version = current.version.map(|v| v.as_string());
            status.install_kind = ProviderInstallKind::Driver;
            status.detail = current.detail.clone();
        } else {
            status.detail = "PawnIO is not installed".to_string();
        }

        let latest = match self.fetcher.latest(OfficialProvider::PawnIo) {
            Ok(latest) => latest,
            Err(err) => {
                return failed(
                    status,
                    err,
                    "could not check the latest official PawnIO version",
                );
            }
        };
        status.latest_version = latest
            .version_parsed()
            .map(|v| v.as_string())
            .or_else(|| Some(latest.version.clone()));
        let Some(latest_version) = latest.version_parsed() else {
            return failed(
                status,
                UpdateError::Parse(format!("unrecognised PawnIO version '{}'", latest.version)),
                "could not parse the latest official PawnIO version",
            );
        };

        if installed
            .pawnio
            .as_ref()
            .and_then(|current| current.version)
            .is_some_and(|current| current >= latest_version)
        {
            status.state = ProviderUpdateState::UpToDate;
            status.detail = "up to date".to_string();
            return status;
        }

        status.state = ProviderUpdateState::UpdateAvailable;
        status.detail = "PawnIO is a kernel driver and is never installed by Oxide Monitor; \
                         update manually from pawnio.eu (administrator/UAC required)"
            .to_string();
        status
    }

    fn stage_lhm(
        &self,
        latest: &ReleaseInfo,
        version: Version,
    ) -> Result<(PathBuf, SignatureStatus), UpdateError> {
        let asset_url = latest
            .asset_url
            .as_deref()
            .ok_or_else(|| UpdateError::Parse("no portable LHM zip asset in the release".into()))?;
        let asset_name = latest
            .asset_name
            .as_deref()
            .ok_or_else(|| UpdateError::Parse("release asset has no name".into()))?;

        let downloads = self.paths.downloads_root();
        fs::create_dir_all(&downloads)?;
        let zip_path = downloads.join(sanitize_file_name(asset_name));
        self.downloader.download(asset_url, &zip_path)?;

        if let Some(expected) = &latest.sha256 {
            let actual = self.verifier.sha256_file(&zip_path)?;
            if !sha256_matches(expected, &actual) {
                let _ = fs::remove_file(&zip_path);
                return Err(UpdateError::Verify(format!(
                    "SHA-256 mismatch for {asset_name}: published {expected}, downloaded {}",
                    hex_encode(&actual)
                )));
            }
        }

        let lhm_root = self.paths.lhm_root();
        fs::create_dir_all(&lhm_root)?;
        let staging = lhm_root.join(format!(
            "{}.staging-{}",
            version.as_string(),
            std::process::id()
        ));
        if staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }

        let staged = (|| {
            let exe = extract_zip(&zip_path, &staging)?;
            let signature = self.verifier.verify_authenticode(&exe)?;
            if signature == SignatureStatus::Unsigned && latest.sha256.is_none() {
                return Err(UpdateError::Verify(
                    "executable is not Authenticode-signed and the release publishes no SHA-256"
                        .into(),
                ));
            }
            let relative = exe
                .strip_prefix(&staging)
                .map_err(|_| UpdateError::Verify("staged executable escaped its folder".into()))?
                .to_path_buf();
            let final_dir = lhm_root.join(version.as_string());
            if final_dir.exists() {
                fs::remove_dir_all(&final_dir)?;
            }
            fs::rename(&staging, &final_dir)?;
            Ok::<(PathBuf, SignatureStatus), UpdateError>((final_dir.join(relative), signature))
        })();

        if staged.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        let _ = fs::remove_file(&zip_path);
        staged
    }
}

fn base_status(
    provider: OfficialProvider,
    auto_update: bool,
    now_unix_ms: u64,
) -> ProviderUpdateStatus {
    ProviderUpdateStatus {
        id: provider.id().to_string(),
        display_name: provider.display_name().to_string(),
        installed_version: None,
        latest_version: None,
        install_kind: ProviderInstallKind::None,
        state: ProviderUpdateState::Unknown,
        auto_update,
        official_url: provider.official_url().to_string(),
        staged_path: None,
        detail: String::new(),
        last_error: None,
        checked_at_unix_ms: Some(now_unix_ms),
    }
}

fn failed(
    mut status: ProviderUpdateStatus,
    error: UpdateError,
    detail: &str,
) -> ProviderUpdateStatus {
    status.state = ProviderUpdateState::Error;
    status.detail = detail.to_string();
    status.last_error = Some(error.to_string());
    status
}

/// `https://` + official host + optional port 443 only, no userinfo.
pub fn is_allowed_url(url: &str) -> bool {
    let Ok(uri) = ureq::http::Uri::from_str(url) else {
        return false;
    };
    if uri.scheme_str() != Some("https") {
        return false;
    }
    if uri.port_u16().is_some_and(|port| port != 443) {
        return false;
    }
    if uri
        .authority()
        .is_some_and(|authority| authority.as_str().contains('@'))
    {
        return false;
    }
    uri.host().is_some_and(|host| {
        ALLOWED_HOSTS
            .iter()
            .any(|allowed| host.eq_ignore_ascii_case(allowed))
    })
}

/// Cache file path for a provider under `<cache>/`.
pub fn cache_path(cache_root: &Path, provider: OfficialProvider) -> PathBuf {
    cache_root.join(provider.cache_file())
}

pub fn load_fresh_cache(
    cache_root: &Path,
    provider: OfficialProvider,
    ttl: Duration,
    now_unix_ms: u64,
) -> Option<ReleaseInfo> {
    let release = load_any_cache(cache_root, provider)?;
    let age = now_unix_ms.saturating_sub(release.fetched_at_unix_ms);
    (age <= ttl.as_millis() as u64).then_some(release)
}

pub fn load_any_cache(cache_root: &Path, provider: OfficialProvider) -> Option<ReleaseInfo> {
    let text = fs::read_to_string(cache_path(cache_root, provider)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn store_cache(
    cache_root: &Path,
    provider: OfficialProvider,
    release: &ReleaseInfo,
) -> Result<(), UpdateError> {
    fs::create_dir_all(cache_root)?;
    let path = cache_path(cache_root, provider);
    let tmp = path.with_extension("json.tmp");
    fs::write(
        &tmp,
        serde_json::to_vec_pretty(release).map_err(|e| UpdateError::Parse(e.to_string()))?,
    )?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn sanitize_file_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<PathBuf, UpdateError> {
    let file = File::open(zip_path)?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|err| UpdateError::Parse(err.to_string()))?;
    fs::create_dir_all(dest)?;

    let mut total = 0u64;
    let mut exe: Option<PathBuf> = None;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| UpdateError::Parse(err.to_string()))?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(UpdateError::Verify(
                "archive contains an unsafe path".to_string(),
            ));
        };
        let out = dest.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
            continue;
        }
        total = total.saturating_add(entry.size());
        if total > MAX_EXTRACTED_BYTES {
            return Err(UpdateError::Verify(
                "archive expands beyond the size limit".into(),
            ));
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = File::create(&out)?;
        io::copy(&mut entry, &mut file)?;
        if entry.name().is_ok_and(|name| {
            name.rsplit(['/', '\\'])
                .next()
                .is_some_and(|file| file.eq_ignore_ascii_case(LHM_MANAGED_EXE))
        }) {
            exe = Some(out);
        }
    }
    exe.ok_or_else(|| UpdateError::Parse(format!("{LHM_MANAGED_EXE} not found in the archive")))
}

fn sha256_matches(expected_hex: &str, actual: &[u8; 32]) -> bool {
    let Some(expected) = hex_decode(expected_hex) else {
        return false;
    };
    expected == actual
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.len() != 64 {
        return None;
    }
    (0..32)
        .map(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok())
        .collect()
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeFetcher {
        releases: Mutex<HashMap<OfficialProvider, ReleaseInfo>>,
    }

    impl FakeFetcher {
        fn new(items: Vec<(OfficialProvider, ReleaseInfo)>) -> Self {
            Self {
                releases: Mutex::new(items.into_iter().collect()),
            }
        }
    }

    impl ReleaseFetcher for FakeFetcher {
        fn latest(&self, provider: OfficialProvider) -> Result<ReleaseInfo, UpdateError> {
            self.releases
                .lock()
                .unwrap()
                .get(&provider)
                .cloned()
                .ok_or_else(|| {
                    UpdateError::Network(format!("no fake release for {}", provider.id()))
                })
        }
    }

    struct FakeDownloader {
        calls: AtomicUsize,
        zip: PathBuf,
    }

    impl ArtifactDownloader for FakeDownloader {
        fn download(&self, _url: &str, dest: &Path) -> Result<(), UpdateError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            fs::copy(&self.zip, dest)?;
            Ok(())
        }
    }

    struct FakeVerifier {
        hash: [u8; 32],
        signature: Option<SignatureStatus>,
    }

    impl ArtifactVerifier for FakeVerifier {
        fn sha256_file(&self, _path: &Path) -> Result<[u8; 32], UpdateError> {
            Ok(self.hash)
        }
        fn verify_authenticode(&self, _path: &Path) -> Result<SignatureStatus, UpdateError> {
            self.signature
                .ok_or_else(|| UpdateError::Verify("untrusted signature".into()))
        }
    }

    fn fixture_zip(path: &Path, exe_contents: &[u8]) {
        let file = File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer
            .start_file("LibreHardwareMonitor/LibreHardwareMonitor.exe", options)
            .unwrap();
        writer.write_all(exe_contents).unwrap();
        writer
            .start_file("LibreHardwareMonitor/LibreHardwareMonitor.dll", options)
            .unwrap();
        writer.write_all(b"dll").unwrap();
        writer.finish().unwrap();
    }

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "oxide-update-test-{}-{tag}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn lhm_release(version: &str) -> ReleaseInfo {
        let hash = [7u8; 32];
        ReleaseInfo {
            version: version.into(),
            page_url: "https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/releases".into(),
            asset_name: Some("LibreHardwareMonitor.zip".into()),
            asset_url: Some(
                "https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/releases/download/v0.9.6/LibreHardwareMonitor.zip".into(),
            ),
            sha256: Some(hex_encode(&hash)),
            fetched_at_unix_ms: 1,
        }
    }

    #[test]
    fn version_parsing_and_ordering() {
        assert_eq!(Version::parse("v0.9.6").unwrap().as_string(), "0.9.6");
        assert_eq!(
            Version::parse("0.9.6.0").unwrap(),
            Version::parse("0.9.6").unwrap()
        );
        assert_eq!(Version::parse("2.2.0").unwrap().as_string(), "2.2.0");
        assert_eq!(
            Version::parse("0.9.10").unwrap(),
            Version::parse("0.9.10").unwrap()
        );
        assert!(Version::parse("0.9.10").unwrap() > Version::parse("0.9.9").unwrap());
        assert!(Version::parse("2.2.0").unwrap() > Version::parse("2.1.9").unwrap());
        assert!(Version::parse("v0.9.6-beta").is_some());
        assert!(Version::parse("latest").is_none());
        assert!(Version::parse("1").is_none());
        assert!(Version::parse("1..2").is_none());
    }

    #[test]
    fn official_url_allowlist_rejects_foreign_and_plain_http() {
        assert!(is_allowed_url(
            "https://api.github.com/repos/LibreHardwareMonitor/LibreHardwareMonitor/releases/latest"
        ));
        assert!(is_allowed_url(
            "https://github.com/namazso/PawnIO.Setup/releases"
        ));
        assert!(is_allowed_url(
            "https://release-assets.githubusercontent.com/x"
        ));
        assert!(!is_allowed_url("http://api.github.com/x"));
        assert!(!is_allowed_url("https://evil.example/x"));
        assert!(!is_allowed_url("https://github.com.evil.example/x"));
        assert!(!is_allowed_url("https://github.com:8443/x"));
        assert!(!is_allowed_url("https://user:pass@github.com/x"));
    }

    #[test]
    fn cache_ttl_is_24_hours_and_corrupt_files_are_ignored() {
        let root = temp_root("cache");
        let release = ReleaseInfo {
            version: "0.9.6".into(),
            page_url: "https://github.com/x".into(),
            asset_name: None,
            asset_url: None,
            sha256: None,
            fetched_at_unix_ms: 1_000,
        };
        store_cache(&root, OfficialProvider::Lhm, &release).unwrap();
        assert!(
            load_fresh_cache(&root, OfficialProvider::Lhm, CACHE_TTL, 1_000 + 60_000).is_some()
        );
        assert!(
            load_fresh_cache(
                &root,
                OfficialProvider::Lhm,
                CACHE_TTL,
                1_000 + CACHE_TTL.as_millis() as u64 + 1
            )
            .is_none()
        );
        assert!(load_any_cache(&root, OfficialProvider::Lhm).is_some());

        fs::write(cache_path(&root, OfficialProvider::Lhm), b"not json").unwrap();
        assert!(load_any_cache(&root, OfficialProvider::Lhm).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn managed_lhm_scan_picks_the_highest_complete_version() {
        let root = temp_root("scan");
        let lhm = root.join("LHM");
        for version in ["0.9.5", "0.9.10", "0.9.6"] {
            let dir = lhm.join(version);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(LHM_MANAGED_EXE), b"exe").unwrap();
        }
        // A directory without the executable must be ignored.
        fs::create_dir_all(lhm.join("9.9.9")).unwrap();

        let found = scan_managed_lhm(&lhm).expect("managed LHM");
        assert_eq!(found.version, Version::parse("0.9.10"));
        assert!(!found.external);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn auto_update_is_off_by_default_and_never_downloads() {
        let root = temp_root("off");
        let zip = root.join("fixture.zip");
        fixture_zip(&zip, b"exe");
        let paths = ProviderPaths::with_root(root.join("providers"));
        let fetcher = FakeFetcher::new(vec![(OfficialProvider::Lhm, lhm_release("0.9.6"))]);
        let downloader = FakeDownloader {
            calls: AtomicUsize::new(0),
            zip: zip.clone(),
        };
        let verifier = FakeVerifier {
            hash: [7u8; 32],
            signature: Some(SignatureStatus::Trusted),
        };
        let manager = UpdateManager {
            paths: &paths,
            fetcher: &fetcher,
            downloader: &downloader,
            verifier: &verifier,
            now_unix_ms: 1,
        };

        let status = &manager.evaluate(&ProvidersConfig::default(), &InstalledState::default())[0];
        assert_eq!(status.state, ProviderUpdateState::UpdateAvailable);
        assert_eq!(downloader.calls.load(Ordering::SeqCst), 0);
        assert!(status.detail.contains("auto-update"));
        assert!(!status.auto_update);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn enabled_auto_update_stages_a_verified_copy() {
        let root = temp_root("stage");
        let zip = root.join("fixture.zip");
        fixture_zip(&zip, b"exe");
        let paths = ProviderPaths::with_root(root.join("providers"));
        let fetcher = FakeFetcher::new(vec![(OfficialProvider::Lhm, lhm_release("0.9.6"))]);
        let downloader = FakeDownloader {
            calls: AtomicUsize::new(0),
            zip: zip.clone(),
        };
        let verifier = FakeVerifier {
            hash: [7u8; 32],
            signature: Some(SignatureStatus::Trusted),
        };
        let manager = UpdateManager {
            paths: &paths,
            fetcher: &fetcher,
            downloader: &downloader,
            verifier: &verifier,
            now_unix_ms: 1,
        };
        let mut config = ProvidersConfig::default();
        config.lhm_http = true;
        config.lhm_auto_update = true;

        let status = &manager.evaluate(&config, &InstalledState::default())[0];
        assert_eq!(status.state, ProviderUpdateState::UpdateReady, "{status:?}");
        assert_eq!(downloader.calls.load(Ordering::SeqCst), 1);
        let staged = status.staged_path.as_deref().expect("staged path");
        assert!(Path::new(staged).is_file(), "staged exe missing: {staged}");
        assert!(staged.contains("0.9.6"));
        assert!(staged.contains("providers"));

        // A second evaluation sees the managed copy as up to date and downloads nothing.
        let installed = InstalledState {
            lhm: Some(InstalledLhm {
                version: Version::parse("0.9.6"),
                path: PathBuf::from(staged),
                external: false,
            }),
            pawnio: None,
        };
        let status = &manager.evaluate(&config, &installed)[0];
        assert_eq!(status.state, ProviderUpdateState::UpToDate);
        assert_eq!(downloader.calls.load(Ordering::SeqCst), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn checksum_or_signature_failure_discards_the_staging_area() {
        for (hash, signature, expected) in [
            (
                [8u8; 32],
                Some(SignatureStatus::Trusted),
                "SHA-256 mismatch",
            ),
            ([7u8; 32], None, "signature"),
        ] {
            let root = temp_root("verify");
            let zip = root.join("fixture.zip");
            fixture_zip(&zip, b"exe");
            let paths = ProviderPaths::with_root(root.join("providers"));
            let fetcher = FakeFetcher::new(vec![(OfficialProvider::Lhm, lhm_release("0.9.6"))]);
            let downloader = FakeDownloader {
                calls: AtomicUsize::new(0),
                zip: zip.clone(),
            };
            let verifier = FakeVerifier { hash, signature };
            let manager = UpdateManager {
                paths: &paths,
                fetcher: &fetcher,
                downloader: &downloader,
                verifier: &verifier,
                now_unix_ms: 1,
            };
            let mut config = ProvidersConfig::default();
            config.lhm_http = true;
            config.lhm_auto_update = true;

            let status = &manager.evaluate(&config, &InstalledState::default())[0];
            assert_eq!(status.state, ProviderUpdateState::Error, "{status:?}");
            assert!(
                status
                    .last_error
                    .as_deref()
                    .is_some_and(|error| error.contains(expected)),
                "error was {:?}",
                status.last_error
            );
            assert!(!paths.lhm_root().join("0.9.6").exists());
            let _ = fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn external_running_lhm_is_never_modified() {
        let root = temp_root("external");
        let zip = root.join("fixture.zip");
        fixture_zip(&zip, b"exe");
        let paths = ProviderPaths::with_root(root.join("providers"));
        let fetcher = FakeFetcher::new(vec![(OfficialProvider::Lhm, lhm_release("0.9.6"))]);
        let downloader = FakeDownloader {
            calls: AtomicUsize::new(0),
            zip: zip.clone(),
        };
        let verifier = FakeVerifier {
            hash: [7u8; 32],
            signature: Some(SignatureStatus::Trusted),
        };
        let manager = UpdateManager {
            paths: &paths,
            fetcher: &fetcher,
            downloader: &downloader,
            verifier: &verifier,
            now_unix_ms: 1,
        };
        let installed = InstalledState {
            lhm: Some(InstalledLhm {
                version: Version::parse("0.9.5"),
                path: PathBuf::from(r"C:\Tools\LHM\LibreHardwareMonitor.exe"),
                external: true,
            }),
            pawnio: None,
        };
        let mut config = ProvidersConfig::default();
        config.lhm_http = true;
        config.lhm_auto_update = true;

        let status = &manager.evaluate(&config, &installed)[0];
        assert_eq!(status.install_kind, ProviderInstallKind::External);
        assert_eq!(status.state, ProviderUpdateState::UpdateAvailable);
        assert!(status.detail.contains("update it manually"));
        assert_eq!(downloader.calls.load(Ordering::SeqCst), 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pawnio_is_detection_only_with_an_official_link() {
        let root = temp_root("pawnio");
        let paths = ProviderPaths::with_root(root.join("providers"));
        let fetcher = FakeFetcher::new(vec![(
            OfficialProvider::PawnIo,
            ReleaseInfo {
                version: "2.2.0".into(),
                page_url: PAWNIO_OFFICIAL_URL.into(),
                asset_name: Some("PawnIO_setup.exe".into()),
                asset_url: Some("https://github.com/namazso/PawnIO.Setup/releases/download/2.2.0/PawnIO_setup.exe".into()),
                sha256: None,
                fetched_at_unix_ms: 1,
            },
        )]);
        let downloader = FakeDownloader {
            calls: AtomicUsize::new(0),
            zip: PathBuf::new(),
        };
        let verifier = FakeVerifier {
            hash: [0; 32],
            signature: Some(SignatureStatus::Trusted),
        };
        let manager = UpdateManager {
            paths: &paths,
            fetcher: &fetcher,
            downloader: &downloader,
            verifier: &verifier,
            now_unix_ms: 1,
        };
        let installed = InstalledState {
            lhm: None,
            pawnio: Some(InstalledPawnIo {
                version: Version::parse("2.1.0"),
                path: None,
                detail: "service PawnIO".into(),
            }),
        };

        let status = &manager.evaluate(&ProvidersConfig::default(), &installed)[1];
        assert_eq!(status.id, "pawnio");
        assert_eq!(status.install_kind, ProviderInstallKind::Driver);
        assert_eq!(status.state, ProviderUpdateState::UpdateAvailable);
        assert_eq!(status.official_url, PAWNIO_OFFICIAL_URL);
        assert!(status.detail.contains("never installed"));
        assert!(!status.auto_update);
        assert_eq!(downloader.calls.load(Ordering::SeqCst), 0);
        let _ = fs::remove_dir_all(&root);
    }
    #[test]
    fn unsigned_upstream_is_accepted_only_with_a_published_hash() {
        for (with_hash, expected_ready) in [(true, true), (false, false)] {
            let root = temp_root("unsigned");
            let zip = root.join("fixture.zip");
            fixture_zip(&zip, b"exe");
            let paths = ProviderPaths::with_root(root.join("providers"));
            let mut release = lhm_release("0.9.6");
            if !with_hash {
                release.sha256 = None;
            }
            let fetcher = FakeFetcher::new(vec![(OfficialProvider::Lhm, release)]);
            let downloader = FakeDownloader {
                calls: AtomicUsize::new(0),
                zip: zip.clone(),
            };
            let verifier = FakeVerifier {
                hash: [7u8; 32],
                signature: Some(SignatureStatus::Unsigned),
            };
            let manager = UpdateManager {
                paths: &paths,
                fetcher: &fetcher,
                downloader: &downloader,
                verifier: &verifier,
                now_unix_ms: 1,
            };
            let mut config = ProvidersConfig::default();
            config.lhm_http = true;
            config.lhm_auto_update = true;

            let status = &manager.evaluate(&config, &InstalledState::default())[0];
            if expected_ready {
                assert_eq!(status.state, ProviderUpdateState::UpdateReady, "{status:?}");
                assert!(status.detail.contains("not Authenticode-signed"));
            } else {
                assert_eq!(status.state, ProviderUpdateState::Error, "{status:?}");
                assert!(
                    status
                        .last_error
                        .as_deref()
                        .is_some_and(|error| error.contains("no SHA-256")),
                    "error was {:?}",
                    status.last_error
                );
            }
            let _ = fs::remove_dir_all(&root);
        }
    }
    #[test]
    fn running_managed_copy_does_not_override_the_highest_managed_version() {
        let root = Path::new(r"C:\Users\me\AppData\Local\OxideMonitor\providers\LHM");
        let managed = InstalledLhm {
            version: Version::parse("0.9.6"),
            path: root.join("0.9.6").join(LHM_MANAGED_EXE),
            external: false,
        };
        let running_managed = InstalledLhm {
            version: Version::parse("0.9.5"),
            path: root.join("0.9.5").join(LHM_MANAGED_EXE),
            external: true,
        };
        let external = InstalledLhm {
            version: Version::parse("0.9.5"),
            path: PathBuf::from(r"C:\Tools\LHM\LibreHardwareMonitor.exe"),
            external: true,
        };

        let resolved = resolve_installed_lhm(Some(managed.clone()), Some(running_managed), root)
            .expect("resolved");
        assert_eq!(resolved.version, Version::parse("0.9.6"));
        assert!(!resolved.external);

        let resolved = resolve_installed_lhm(None, Some(external.clone()), root).expect("resolved");
        assert_eq!(resolved.path, external.path);
        assert!(resolved.external);
    }
}
