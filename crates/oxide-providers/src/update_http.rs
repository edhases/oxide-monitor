//! HTTPS release metadata and asset download with an official-host allowlist.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use ureq::Agent;

use crate::update::{
    CACHE_TTL, MAX_DOWNLOAD_BYTES, OfficialProvider, ReleaseFetcher, ReleaseInfo, USER_AGENT,
    UpdateError, Version, is_allowed_url, load_any_cache, load_fresh_cache, now_unix_ms,
    store_cache,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

fn build_agent() -> Agent {
    // System TLS (SChannel on Windows) via ureq's native-tls provider.
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(REQUEST_TIMEOUT))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .new_agent()
}

/// Cached GitHub release metadata fetcher (24 h by default).
pub struct HttpFetcher {
    agent: Agent,
    cache_root: PathBuf,
    ttl: Duration,
}

impl HttpFetcher {
    pub fn new(cache_root: PathBuf) -> Self {
        Self::with_ttl(cache_root, CACHE_TTL)
    }

    pub fn with_ttl(cache_root: PathBuf, ttl: Duration) -> Self {
        Self {
            agent: build_agent(),
            cache_root,
            ttl,
        }
    }

    fn fetch_release(&self, provider: OfficialProvider) -> Result<ReleaseInfo, UpdateError> {
        let body = get_bytes(
            &self.agent,
            provider.release_api_url(),
            "application/vnd.github+json",
        )?;
        let json: Value = serde_json::from_slice(&body)
            .map_err(|err| UpdateError::Parse(format!("release JSON: {err}")))?;
        let tag = json["tag_name"]
            .as_str()
            .ok_or_else(|| UpdateError::Parse("release has no tag_name".into()))?;
        let version = Version::parse(tag)
            .ok_or_else(|| UpdateError::Parse(format!("unrecognised release tag '{tag}'")))?;
        let page_url = json["html_url"]
            .as_str()
            .unwrap_or_else(|| provider.official_url())
            .to_string();
        let (asset_name, asset_url, sha256) = select_asset(provider, &json);

        Ok(ReleaseInfo {
            version: version.as_string(),
            page_url,
            asset_name,
            asset_url,
            sha256,
            fetched_at_unix_ms: now_unix_ms(),
        })
    }
}

impl ReleaseFetcher for HttpFetcher {
    fn latest(&self, provider: OfficialProvider) -> Result<ReleaseInfo, UpdateError> {
        let now = now_unix_ms();
        if let Some(cached) = load_fresh_cache(&self.cache_root, provider, self.ttl, now) {
            return Ok(cached);
        }

        match self.fetch_release(provider) {
            Ok(release) => {
                if let Err(err) = store_cache(&self.cache_root, provider, &release) {
                    tracing::debug!(target: "oxide_providers", %err, "could not cache release metadata");
                }
                Ok(release)
            }
            Err(err) => {
                // A stale cached result is still the best known official version.
                if let Some(stale) = load_any_cache(&self.cache_root, provider) {
                    tracing::warn!(
                        target: "oxide_providers",
                        %err,
                        provider = provider.id(),
                        version = %stale.version,
                        "release check failed; using the cached official version"
                    );
                    Ok(stale)
                } else {
                    Err(err)
                }
            }
        }
    }
}

/// Downloads an official release asset (bounded by [`MAX_DOWNLOAD_BYTES`]).
pub struct HttpDownloader {
    agent: Agent,
}

impl Default for HttpDownloader {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpDownloader {
    pub fn new() -> Self {
        Self {
            agent: build_agent(),
        }
    }
}

impl crate::update::ArtifactDownloader for HttpDownloader {
    fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError> {
        let bytes = get_bytes(&self.agent, url, "application/octet-stream")?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(dest, bytes)?;
        Ok(())
    }
}

/// GET `url`, following up to three redirects, every hop allowlisted.
fn get_bytes(agent: &Agent, url: &str, accept: &str) -> Result<Vec<u8>, UpdateError> {
    let mut current = url.to_string();
    for _ in 0..=3 {
        if !is_allowed_url(&current) {
            return Err(UpdateError::NotAllowed(current));
        }
        let mut response = agent
            .get(&current)
            .header("User-Agent", USER_AGENT)
            .header("Accept", accept)
            .call()
            .map_err(classify_transport)?;
        let status = response.status().as_u16();
        match status {
            200 => {
                return response
                    .body_mut()
                    .with_config()
                    .limit(MAX_DOWNLOAD_BYTES)
                    .read_to_vec()
                    .map_err(classify_transport);
            }
            301 | 302 | 303 | 307 | 308 => {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| UpdateError::Parse("redirect without Location".into()))?;
                if !location.starts_with("https://") {
                    return Err(UpdateError::NotAllowed(location.to_string()));
                }
                current = location.to_string();
            }
            403 | 429 => return Err(UpdateError::RateLimited),
            other => return Err(UpdateError::Http(other)),
        }
    }
    Err(UpdateError::Network("too many redirects".into()))
}

fn classify_transport(error: ureq::Error) -> UpdateError {
    match error {
        ureq::Error::Timeout(_) => UpdateError::Network("request timed out".into()),
        ureq::Error::HostNotFound => UpdateError::Network("host not found".into()),
        ureq::Error::ConnectionFailed => UpdateError::Network("connection failed".into()),
        ureq::Error::Io(error) => UpdateError::Network(error.to_string()),
        other => UpdateError::Network(other.to_string()),
    }
}

/// Pick the portable asset: LHM's classic zip (not the .NET 10 variant), PawnIO's setup exe.
fn select_asset(
    provider: OfficialProvider,
    json: &Value,
) -> (Option<String>, Option<String>, Option<String>) {
    let Some(assets) = json["assets"].as_array() else {
        return (None, None, None);
    };
    let name = |asset: &Value| asset["name"].as_str().unwrap_or_default().to_string();
    let chosen = match provider {
        OfficialProvider::Lhm => assets
            .iter()
            .find(|asset| name(asset).eq_ignore_ascii_case("LibreHardwareMonitor.zip"))
            .or_else(|| {
                assets.iter().find(|asset| {
                    let name = name(asset);
                    name.ends_with(".zip") && !name.contains(".NET.")
                })
            })
            .or_else(|| assets.iter().find(|asset| name(asset).ends_with(".zip"))),
        OfficialProvider::PawnIo => assets
            .iter()
            .find(|asset| name(asset).eq_ignore_ascii_case("PawnIO_setup.exe"))
            .or_else(|| assets.iter().find(|asset| name(asset).ends_with(".exe"))),
    };
    let Some(asset) = chosen else {
        return (None, None, None);
    };
    let name = asset["name"].as_str().map(str::to_string);
    let url = asset["browser_download_url"]
        .as_str()
        .map(str::to_string)
        .filter(|url| is_allowed_url(url));
    let sha256 = asset["digest"]
        .as_str()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .map(|digest| digest.trim().to_ascii_lowercase());
    (name, url, sha256)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn asset_selection_prefers_the_classic_lhm_zip_and_extracts_the_digest() {
        let json = json!({
            "tag_name": "v0.9.6",
            "html_url": "https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/releases/tag/v0.9.6",
            "assets": [
                {
                    "name": "LibreHardwareMonitor.NET.10.zip",
                    "browser_download_url": "https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/releases/download/v0.9.6/LibreHardwareMonitor.NET.10.zip",
                    "digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111"
                },
                {
                    "name": "LibreHardwareMonitor.zip",
                    "browser_download_url": "https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/releases/download/v0.9.6/LibreHardwareMonitor.zip",
                    "digest": "sha256:2222222222222222222222222222222222222222222222222222222222222222"
                }
            ]
        });
        let (name, url, digest) = select_asset(OfficialProvider::Lhm, &json);
        assert_eq!(name.as_deref(), Some("LibreHardwareMonitor.zip"));
        assert!(url.unwrap().ends_with("LibreHardwareMonitor.zip"));
        assert_eq!(digest.unwrap(), "2".repeat(64));
    }

    #[test]
    fn pawnio_asset_is_the_official_setup_and_foreign_urls_are_dropped() {
        let json = json!({
            "tag_name": "2.2.0",
            "assets": [{
                "name": "PawnIO_setup.exe",
                "browser_download_url": "https://evil.example/PawnIO_setup.exe",
                "digest": "sha256:3333333333333333333333333333333333333333333333333333333333333333"
            }]
        });
        let (name, url, _) = select_asset(OfficialProvider::PawnIo, &json);
        assert_eq!(name.as_deref(), Some("PawnIO_setup.exe"));
        assert!(url.is_none(), "non-official asset URL must be dropped");
    }
}
