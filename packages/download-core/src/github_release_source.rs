//! Network-backed sidecar update source: yt-dlp releases from GitHub.
//!
//! `check` reads `https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest`
//! (cached for an hour; the unauthenticated API allows 60 requests/hour) and
//! picks the asset that can run on this host:
//!
//! | platform              | asset                 |
//! |-----------------------|-----------------------|
//! | linux glibc x86_64    | `yt-dlp_linux`        |
//! | linux glibc aarch64   | `yt-dlp_linux_aarch64`|
//! | linux glibc armv7     | `yt-dlp_linux_armv7l` |
//! | linux musl (any arch) | `yt-dlp` (zipimport script, needs python3) |
//! | macos                 | `yt-dlp_macos`        |
//! | windows               | `yt-dlp.exe`          |
//!
//! `materialize` downloads the asset into `<sidecar_root>/downloads/<version>/`
//! and verifies it against the release's `SHA2-256SUMS` before the update
//! manager copies it into place (and sets the executable bit). A release whose
//! `SHA2-256SUMS` is missing, unreadable or has no entry for the asset is an
//! error: an unverified binary is never installed (H-22).
//!
//! Extractor Plugin versions still come from the local `update-catalog.json`
//! (see [`FileSidecarUpdateSource`]); this source merges both.

use crate::fsutil::is_single_component;
use crate::http::HttpBackend;
use crate::sidecar_updates::{
    CatalogVersionEntry, FileSidecarUpdateSource, SidecarComponent, SidecarUpdateSource,
    UpdateCatalog,
};
use crate::toolchain_discovery::{find_in_path, path_dirs_from_env, Libc, PlatformInfo};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Default upstream repository.
pub const DEFAULT_REPO: &str = "yt-dlp/yt-dlp";
/// Nightly builds repository (opt-in).
pub const NIGHTLY_REPO: &str = "yt-dlp/yt-dlp-nightly-builds";
/// How long a release lookup stays valid.
pub const CATALOG_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
/// Upper bound for the release JSON / checksum file.
const METADATA_CAP: u64 = 4 * 1024 * 1024;
/// How long a failed release lookup is remembered, so a rate-limited client
/// does not hammer the API on every call (L-32).
pub const FAILURE_CACHE_TTL: Duration = Duration::from_secs(30);
/// Subfolder under the sidecar root for fetched assets.
pub const DOWNLOADS_DIR: &str = "downloads";

/// Release asset name for a platform.
pub fn select_asset(platform: &PlatformInfo) -> &'static str {
    match platform.os.as_str() {
        "windows" => "yt-dlp.exe",
        "macos" => "yt-dlp_macos",
        "linux" => match (platform.libc, platform.arch.as_str()) {
            (Libc::Glibc, "x86_64") => "yt-dlp_linux",
            (Libc::Glibc, "aarch64") => "yt-dlp_linux_aarch64",
            (Libc::Glibc, "arm") => "yt-dlp_linux_armv7l",
            // musl (and unknown libc/arch): the PyInstaller builds need glibc,
            // so fall back to the universal zipimport script.
            _ => "yt-dlp",
        },
        _ => "yt-dlp",
    }
}

/// Whether the selected asset is the universal script that needs python3.
pub fn asset_needs_python(asset: &str) -> bool {
    asset == "yt-dlp"
}

/// Parsed `releases/latest` payload (only what we need).
#[derive(Debug, Clone, serde::Deserialize)]
struct ReleaseJson {
    tag_name: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

/// Parse a `SHA2-256SUMS` file (`<hex>  <name>` per line).
pub fn parse_sha256sums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let hex = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| (name.to_string(), hex.to_ascii_lowercase()))
        })
        .collect()
}

/// Build the toolchain half of an [`UpdateCatalog`] from release JSON + checksums.
///
/// `sums` is the text of the release's `SHA2-256SUMS`. A missing entry for the
/// selected asset is an error (H-22). `python_available` says whether `python3`
/// is on `PATH`; the zipimport asset cannot start without it (M-39).
pub fn catalog_from_release(
    json: &str,
    sums: &str,
    platform: &PlatformInfo,
    python_available: bool,
) -> Result<UpdateCatalog, String> {
    let release: ReleaseJson =
        serde_json::from_str(json).map_err(|e| format!("parse GitHub release JSON: {e}"))?;
    let wanted = select_asset(platform);
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == wanted)
        .ok_or_else(|| {
            format!(
                "release {} has no asset named {wanted} (available: {})",
                release.tag_name,
                release
                    .assets
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    // The tag becomes a directory name and the asset name a file name.
    if !is_single_component(&release.tag_name) {
        return Err(format!(
            "release tag {:?} is not a usable version name",
            release.tag_name
        ));
    }
    if !is_single_component(&asset.name) {
        return Err(format!(
            "release asset name {:?} is not a usable file name",
            asset.name
        ));
    }
    if asset_needs_python(wanted) && !python_available {
        return Err(format!(
            "this host needs the {wanted} zipimport script, which requires python3, \
             but python3 is not on PATH"
        ));
    }
    let sha256 = parse_sha256sums(sums).remove(wanted).ok_or_else(|| {
        format!(
            "SHA2-256SUMS of release {} has no checksum for {wanted}; refusing to install an unverified binary",
            release.tag_name
        )
    })?;
    let mut catalog = UpdateCatalog {
        toolchain_latest: Some(release.tag_name.clone()),
        ..UpdateCatalog::default()
    };
    catalog.toolchain_versions.insert(
        release.tag_name,
        CatalogVersionEntry {
            source_path: PathBuf::new(),
            asset_url: Some(asset.browser_download_url.clone()),
            asset_name: Some(asset.name.clone()),
            sha256: Some(sha256),
            extractors: Vec::new(),
        },
    );
    Ok(catalog)
}

/// Hex SHA-256 of a file, streamed.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// HTTP status code named in a transport error string, if any.
fn status_in_error(err: &str) -> Option<u16> {
    let rest = err.split("status code ").nth(1)?;
    rest.get(..3)?.parse().ok()
}

/// Turn a release-lookup transport error into a message that names the cause.
fn describe_lookup_error(err: &str) -> String {
    match status_in_error(err) {
        Some(403 | 429) => format!(
            "GitHub rate limit reached (unauthenticated API allows 60 requests an hour); \
             try again later: {err}"
        ),
        Some(404) => format!("GitHub release not found: {err}"),
        Some(code) if code >= 500 => format!("GitHub is unavailable (HTTP {code}): {err}"),
        _ => format!("GitHub release lookup failed: {err}"),
    }
}

/// Whether a lookup error is worth one retry.
fn is_transient(err: &str) -> bool {
    !matches!(status_in_error(err), Some(400..=499))
}

struct LookupCache {
    ok: Option<(Instant, UpdateCatalog)>,
    failed: Option<(Instant, String)>,
}

/// yt-dlp releases on GitHub (toolchain) + local catalog (plugins).
pub struct GitHubReleaseUpdateSource {
    http: Arc<dyn HttpBackend>,
    repo: String,
    platform: PlatformInfo,
    python_available: bool,
    retry_delay: Duration,
    cache: Mutex<LookupCache>,
    plugins: FileSidecarUpdateSource,
}

impl GitHubReleaseUpdateSource {
    pub fn new(http: Arc<dyn HttpBackend>, platform: PlatformInfo) -> Self {
        Self::with_repo(http, platform, DEFAULT_REPO)
    }

    pub fn with_repo(http: Arc<dyn HttpBackend>, platform: PlatformInfo, repo: &str) -> Self {
        Self {
            http,
            repo: repo.to_string(),
            platform,
            python_available: find_in_path("python3", &path_dirs_from_env()).is_some(),
            retry_delay: Duration::from_secs(1),
            cache: Mutex::new(LookupCache {
                ok: None,
                failed: None,
            }),
            plugins: FileSidecarUpdateSource,
        }
    }

    /// Override the `python3` probe (tests).
    pub fn with_python_available(mut self, available: bool) -> Self {
        self.python_available = available;
        self
    }

    /// Override the pause before the single retry (tests).
    pub fn with_retry_delay(mut self, delay: Duration) -> Self {
        self.retry_delay = delay;
        self
    }

    /// Asset this host will receive.
    pub fn asset_name(&self) -> &'static str {
        select_asset(&self.platform)
    }

    fn latest_url(&self) -> String {
        format!("https://api.github.com/repos/{}/releases/latest", self.repo)
    }

    /// GET with one retry for errors that are not a 4xx answer.
    fn fetch_metadata(&self, url: &str) -> Result<Vec<u8>, String> {
        let mut result = self.http.fetch_capped(url, METADATA_CAP);
        if matches!(&result, Err(e) if is_transient(e)) {
            std::thread::sleep(self.retry_delay);
            result = self.http.fetch_capped(url, METADATA_CAP);
        }
        result.map(|(body, _, _)| body)
    }

    fn lookup_release(&self) -> Result<UpdateCatalog, String> {
        let body = self
            .fetch_metadata(&self.latest_url())
            .map_err(|e| describe_lookup_error(&e))?;
        let json = String::from_utf8_lossy(&body);
        let release: ReleaseJson =
            serde_json::from_str(&json).map_err(|e| format!("parse GitHub release JSON: {e}"))?;
        let sums_asset = release
            .assets
            .iter()
            .find(|a| a.name == "SHA2-256SUMS")
            .ok_or_else(|| {
                format!(
                    "release {} has no SHA2-256SUMS asset; refusing to install an unverified binary",
                    release.tag_name
                )
            })?;
        let sums = self
            .fetch_metadata(&sums_asset.browser_download_url)
            .map_err(|e| format!("download SHA2-256SUMS of release {}: {e}", release.tag_name))?;
        catalog_from_release(
            &json,
            &String::from_utf8_lossy(&sums),
            &self.platform,
            self.python_available,
        )
    }

    fn fetch_toolchain_catalog(&self) -> Result<UpdateCatalog, String> {
        if let Ok(guard) = self.cache.lock() {
            if let Some((at, catalog)) = guard.ok.as_ref() {
                if at.elapsed() < CATALOG_CACHE_TTL {
                    return Ok(catalog.clone());
                }
            }
            if let Some((at, err)) = guard.failed.as_ref() {
                if at.elapsed() < FAILURE_CACHE_TTL {
                    return Err(err.clone());
                }
            }
        }
        let outcome = self.lookup_release();
        if let Ok(mut guard) = self.cache.lock() {
            match &outcome {
                Ok(catalog) => {
                    guard.ok = Some((Instant::now(), catalog.clone()));
                    guard.failed = None;
                }
                Err(err) => guard.failed = Some((Instant::now(), err.clone())),
            }
        }
        outcome
    }
}

impl SidecarUpdateSource for GitHubReleaseUpdateSource {
    fn load_catalog(&self, sidecar_root: &Path) -> Result<UpdateCatalog, String> {
        // Each half fails alone: a GitHub outage must not hide the plugin status,
        // and an unreadable plugin catalog must not block toolchain updates.
        let mut catalog = match self.plugins.load_catalog(sidecar_root) {
            Ok(catalog) => catalog,
            Err(err) => UpdateCatalog {
                plugin_error: Some(err),
                ..UpdateCatalog::default()
            },
        };
        match self.fetch_toolchain_catalog() {
            Ok(toolchain) => {
                catalog.toolchain_latest = toolchain.toolchain_latest;
                catalog.toolchain_versions = toolchain.toolchain_versions;
            }
            Err(err) => {
                catalog.toolchain_latest = None;
                catalog.toolchain_versions.clear();
                catalog.toolchain_error = Some(err);
            }
        }
        Ok(catalog)
    }

    fn invalidate_cache(&self) {
        if let Ok(mut guard) = self.cache.lock() {
            guard.ok = None;
            guard.failed = None;
        }
    }

    fn materialize(
        &self,
        sidecar_root: &Path,
        component: SidecarComponent,
        version: &str,
        entry: &CatalogVersionEntry,
    ) -> Result<PathBuf, String> {
        let (Some(url), Some(name)) = (entry.asset_url.as_deref(), entry.asset_name.as_deref())
        else {
            return Ok(entry.source_path.clone());
        };
        if component != SidecarComponent::Toolchain {
            return Ok(entry.source_path.clone());
        }
        if !is_single_component(version) || !is_single_component(name) {
            return Err(format!(
                "refusing to install {name:?} {version:?}: not usable as file names"
            ));
        }
        let expected = entry
            .sha256
            .as_deref()
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| {
                format!("refusing to install {name} {version}: the catalog entry has no checksum")
            })?;
        let downloads = sidecar_root.join(DOWNLOADS_DIR);
        let dir = downloads.join(version);
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let dest = dir.join(name);

        // Reuse a previous download only if it still matches the checksum.
        if dest.is_file() {
            if sha256_file(&dest)? == expected {
                prune_other_versions(&downloads, version);
                return Ok(dest);
            }
            let _ = std::fs::remove_file(&dest);
        }

        let tmp = dir.join(format!("{name}.part"));
        let downloaded = (|| {
            let mut file = std::fs::File::create(&tmp)
                .map_err(|e| format!("create {}: {e}", tmp.display()))?;
            self.http
                .fetch(url, None, &mut file)
                .map_err(|e| format!("download {name} {version}: {e}"))?;
            file.flush()
                .map_err(|e| format!("flush {}: {e}", tmp.display()))?;
            file.sync_all()
                .map_err(|e| format!("sync {}: {e}", tmp.display()))?;
            let actual = sha256_file(&tmp)?;
            if actual != expected {
                return Err(format!(
                    "checksum mismatch for {name} {version}: expected {expected}, got {actual}"
                ));
            }
            crate::fsutil::rename_durable(&tmp, &dest)
                .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), dest.display()))
        })();
        if let Err(err) = downloaded {
            let _ = std::fs::remove_file(&tmp);
            return Err(err);
        }
        prune_other_versions(&downloads, version);
        Ok(dest)
    }
}

/// Remove every `downloads/<other version>` directory, keeping `keep` (L-24).
/// Best effort: a leftover directory costs disk space, not correctness.
fn prune_other_versions(downloads: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(downloads) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name() != keep && entry.path().is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::FetchResult;
    use crate::proxy_settings::ProxySettings;

    const LATEST_URL: &str = "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest";
    const SUMS_URL: &str = "https://dl.example/SHA2-256SUMS";
    const ASSET_URL: &str = "https://dl.example/yt-dlp_linux";

    struct CannedHttp {
        bodies: Mutex<HashMap<String, Vec<u8>>>,
        errors: Mutex<HashMap<String, String>>,
        calls: Mutex<Vec<String>>,
    }

    impl CannedHttp {
        fn count(&self, url: &str) -> usize {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|u| *u == url)
                .count()
        }
        fn set_body(&self, url: &str, body: &[u8]) {
            self.bodies
                .lock()
                .unwrap()
                .insert(url.to_string(), body.to_vec());
        }
        fn remove(&self, url: &str) {
            self.bodies.lock().unwrap().remove(url);
        }
        fn fail(&self, url: &str, err: &str) {
            self.errors
                .lock()
                .unwrap()
                .insert(url.to_string(), err.to_string());
        }
        fn heal(&self, url: &str) {
            self.errors.lock().unwrap().remove(url);
        }
    }

    impl HttpBackend for CannedHttp {
        fn fetch(
            &self,
            url: &str,
            _range: Option<std::ops::Range<u64>>,
            sink: &mut dyn Write,
        ) -> Result<FetchResult, String> {
            self.calls.lock().unwrap().push(url.to_string());
            if let Some(err) = self.errors.lock().unwrap().get(url) {
                return Err(err.clone());
            }
            let bodies = self.bodies.lock().unwrap();
            let body = bodies.get(url).ok_or_else(|| format!("404 {url}"))?;
            sink.write_all(body).map_err(|e| e.to_string())?;
            Ok(FetchResult {
                total_length: body.len() as u64,
                bytes_written: body.len() as u64,
                ..Default::default()
            })
        }
        fn supports_range(&self, _url: &str) -> Result<bool, String> {
            Ok(false)
        }
        fn file_size(&self, url: &str) -> Result<u64, String> {
            Ok(self
                .bodies
                .lock()
                .unwrap()
                .get(url)
                .map(|b| b.len() as u64)
                .unwrap_or(0))
        }
        fn apply_proxy_settings(&self, _proxy: &ProxySettings) -> Result<(), String> {
            Ok(())
        }
    }

    fn glibc_x86() -> PlatformInfo {
        PlatformInfo {
            os: "linux".into(),
            arch: "x86_64".into(),
            libc: Libc::Glibc,
        }
    }

    fn release_json_for(tag: &str, asset_name: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","assets":[
            {{"name":"yt-dlp","browser_download_url":"https://dl.example/yt-dlp"}},
            {{"name":"{asset_name}","browser_download_url":"{ASSET_URL}"}},
            {{"name":"yt-dlp.exe","browser_download_url":"https://dl.example/yt-dlp.exe"}},
            {{"name":"SHA2-256SUMS","browser_download_url":"{SUMS_URL}"}}
        ]}}"#
        )
    }

    fn release_json() -> String {
        release_json_for("2026.10.01", "yt-dlp_linux")
    }

    fn http_with(binary: &[u8], sums_hex: Option<&str>) -> Arc<CannedHttp> {
        let http = Arc::new(CannedHttp {
            bodies: Mutex::new(HashMap::new()),
            errors: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
        });
        http.set_body(LATEST_URL, release_json().as_bytes());
        http.set_body(ASSET_URL, binary);
        let sums = format!(
            "{}  yt-dlp_linux\n{}  yt-dlp\n",
            sums_hex
                .map(str::to_string)
                .unwrap_or_else(|| format!("{:x}", Sha256::digest(binary))),
            "0".repeat(64)
        );
        http.set_body(SUMS_URL, sums.as_bytes());
        http
    }

    fn source_over(http: &Arc<CannedHttp>) -> GitHubReleaseUpdateSource {
        GitHubReleaseUpdateSource::new(http.clone(), glibc_x86())
            .with_python_available(true)
            .with_retry_delay(Duration::ZERO)
    }

    fn source(binary: &[u8], sums_hex: Option<&str>) -> GitHubReleaseUpdateSource {
        source_over(&http_with(binary, sums_hex))
    }

    #[test]
    fn asset_selection_covers_libc_and_arch() {
        assert_eq!(select_asset(&glibc_x86()), "yt-dlp_linux");
        assert_eq!(
            select_asset(&PlatformInfo {
                arch: "aarch64".into(),
                ..glibc_x86()
            }),
            "yt-dlp_linux_aarch64"
        );
        assert_eq!(
            select_asset(&PlatformInfo {
                libc: Libc::Musl,
                ..glibc_x86()
            }),
            "yt-dlp"
        );
        assert!(asset_needs_python(select_asset(&PlatformInfo {
            libc: Libc::Musl,
            ..glibc_x86()
        })));
        assert_eq!(
            select_asset(&PlatformInfo {
                os: "windows".into(),
                ..glibc_x86()
            }),
            "yt-dlp.exe"
        );
    }

    #[test]
    fn sha256sums_are_parsed() {
        let text = format!(
            "{}  yt-dlp_linux\n{} *yt-dlp.exe\nbad line\n",
            "a".repeat(64),
            "b".repeat(64)
        );
        let m = parse_sha256sums(&text);
        assert_eq!(m["yt-dlp_linux"], "a".repeat(64));
        assert_eq!(m["yt-dlp.exe"], "b".repeat(64));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn catalog_lists_latest_release_with_platform_asset_and_checksum() {
        let tmp = tempfile::tempdir().unwrap();
        let http = http_with(b"#!/bin/sh\necho yt-dlp\n", None);
        let src = source_over(&http);
        let catalog = src.load_catalog(tmp.path()).unwrap();
        assert_eq!(catalog.toolchain_latest.as_deref(), Some("2026.10.01"));
        let entry = &catalog.toolchain_versions["2026.10.01"];
        assert_eq!(entry.asset_name.as_deref(), Some("yt-dlp_linux"));
        assert_eq!(entry.asset_url.as_deref(), Some(ASSET_URL));
        assert!(entry.sha256.is_some());
        assert_eq!(http.count(LATEST_URL), 1);
        // A second lookup is served from the cache.
        let again = src.load_catalog(tmp.path()).unwrap();
        assert_eq!(again.toolchain_latest, catalog.toolchain_latest);
        assert_eq!(
            http.count(LATEST_URL),
            1,
            "second lookup must hit the cache"
        );
        assert_eq!(http.count(SUMS_URL), 1);
    }

    #[test]
    fn invalidate_cache_forces_a_new_lookup() {
        let tmp = tempfile::tempdir().unwrap();
        let http = http_with(b"bin", None);
        let src = source_over(&http);
        src.load_catalog(tmp.path()).unwrap();
        src.invalidate_cache();
        src.load_catalog(tmp.path()).unwrap();
        assert_eq!(http.count(LATEST_URL), 2);
    }

    #[test]
    fn sums_download_failure_is_a_hard_error_not_an_unverified_entry() {
        // H-22: a transient SUMS error used to become `None` and install unverified.
        let tmp = tempfile::tempdir().unwrap();
        let http = http_with(b"bin", None);
        http.remove(SUMS_URL);
        let catalog = source_over(&http).load_catalog(tmp.path()).unwrap();
        assert!(catalog.toolchain_versions.is_empty());
        assert_eq!(catalog.toolchain_latest, None);
        let err = catalog.toolchain_error.expect("lookup error recorded");
        assert!(err.contains("SHA2-256SUMS"), "{err}");
    }

    #[test]
    fn sums_without_the_selected_asset_is_a_hard_error() {
        let tmp = tempfile::tempdir().unwrap();
        let http = http_with(b"bin", None);
        http.set_body(
            SUMS_URL,
            format!("{}  yt-dlp_macos\n", "a".repeat(64)).as_bytes(),
        );
        let catalog = source_over(&http).load_catalog(tmp.path()).unwrap();
        assert!(catalog.toolchain_versions.is_empty());
        let err = catalog.toolchain_error.expect("lookup error recorded");
        assert!(err.contains("no checksum for yt-dlp_linux"), "{err}");
    }

    #[test]
    fn release_without_a_sums_asset_is_a_hard_error() {
        let err = catalog_from_release(
            r#"{"tag_name":"1","assets":[{"name":"yt-dlp_linux","browser_download_url":"u"}]}"#,
            "",
            &glibc_x86(),
            true,
        )
        .unwrap_err();
        assert!(err.contains("no checksum"), "{err}");
    }

    #[test]
    fn materialize_refuses_an_entry_without_a_checksum() {
        let tmp = tempfile::tempdir().unwrap();
        let src = source(b"bin", None);
        let entry = CatalogVersionEntry {
            asset_url: Some(ASSET_URL.to_string()),
            asset_name: Some("yt-dlp_linux".to_string()),
            ..CatalogVersionEntry::local(PathBuf::new())
        };
        let err = src
            .materialize(
                tmp.path(),
                SidecarComponent::Toolchain,
                "2026.10.01",
                &entry,
            )
            .unwrap_err();
        assert!(err.contains("no checksum"), "{err}");
        assert!(!tmp
            .path()
            .join("downloads/2026.10.01/yt-dlp_linux")
            .exists());
    }

    #[test]
    fn materialize_does_not_reuse_an_unverified_existing_download() {
        // H-22: an existing file used to be returned as-is when `sha256` was `None`.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("downloads/2026.10.01");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("yt-dlp_linux"), b"planted").unwrap();
        let src = source(b"bin", None);
        let entry = CatalogVersionEntry {
            asset_url: Some(ASSET_URL.to_string()),
            asset_name: Some("yt-dlp_linux".to_string()),
            ..CatalogVersionEntry::local(PathBuf::new())
        };
        assert!(src
            .materialize(
                tmp.path(),
                SidecarComponent::Toolchain,
                "2026.10.01",
                &entry
            )
            .is_err());
    }

    #[test]
    fn materialize_replaces_an_existing_download_with_the_wrong_checksum() {
        let tmp = tempfile::tempdir().unwrap();
        let body = b"genuine";
        let dir = tmp.path().join("downloads/2026.10.01");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("yt-dlp_linux"), b"tampered").unwrap();
        let src = source(body, None);
        let catalog = src.load_catalog(tmp.path()).unwrap();
        let entry = &catalog.toolchain_versions["2026.10.01"];
        let path = src
            .materialize(tmp.path(), SidecarComponent::Toolchain, "2026.10.01", entry)
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), body);
    }

    #[test]
    fn materialize_downloads_verifies_and_reuses() {
        let tmp = tempfile::tempdir().unwrap();
        let body = b"#!/bin/sh\necho yt-dlp 2026.10.01\n";
        let http = http_with(body, None);
        let src = source_over(&http);
        let catalog = src.load_catalog(tmp.path()).unwrap();
        let entry = &catalog.toolchain_versions["2026.10.01"];
        let path = src
            .materialize(tmp.path(), SidecarComponent::Toolchain, "2026.10.01", entry)
            .unwrap();
        assert_eq!(
            path,
            tmp.path()
                .join("downloads")
                .join("2026.10.01")
                .join("yt-dlp_linux")
        );
        assert_eq!(std::fs::read(&path).unwrap(), body);
        assert_eq!(http.count(ASSET_URL), 1);
        // Reuse the verified file without downloading again.
        let again = src
            .materialize(tmp.path(), SidecarComponent::Toolchain, "2026.10.01", entry)
            .unwrap();
        assert_eq!(again, path);
        assert_eq!(http.count(ASSET_URL), 1, "verified download is reused");
        // Plugin entries are untouched (local paths).
        let local = CatalogVersionEntry::local(PathBuf::from("/tmp/x"));
        assert_eq!(
            src.materialize(tmp.path(), SidecarComponent::Plugin, "1", &local)
                .unwrap(),
            PathBuf::from("/tmp/x")
        );
    }

    #[test]
    fn materialize_rejects_checksum_mismatch_and_leaves_no_part_file() {
        let tmp = tempfile::tempdir().unwrap();
        let src = source(b"real bytes", Some(&"f".repeat(64)));
        let catalog = src.load_catalog(tmp.path()).unwrap();
        let entry = &catalog.toolchain_versions["2026.10.01"];
        let err = src
            .materialize(tmp.path(), SidecarComponent::Toolchain, "2026.10.01", entry)
            .unwrap_err();
        assert!(err.contains("checksum mismatch"), "{err}");
        let dir = tmp.path().join("downloads/2026.10.01");
        assert!(!dir.join("yt-dlp_linux").exists());
        assert!(!dir.join("yt-dlp_linux.part").exists());
    }

    #[test]
    fn failed_download_removes_the_part_file() {
        // L-24
        let tmp = tempfile::tempdir().unwrap();
        let http = http_with(b"bin", None);
        let src = source_over(&http);
        let catalog = src.load_catalog(tmp.path()).unwrap();
        let entry = &catalog.toolchain_versions["2026.10.01"];
        http.fail(ASSET_URL, "connection reset");
        assert!(src
            .materialize(tmp.path(), SidecarComponent::Toolchain, "2026.10.01", entry)
            .is_err());
        assert!(!tmp
            .path()
            .join("downloads/2026.10.01/yt-dlp_linux.part")
            .exists());
    }

    #[test]
    fn materialize_prunes_older_downloaded_versions() {
        // L-24
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("downloads/2025.01.01");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("yt-dlp_linux"), b"old").unwrap();
        let src = source(b"new", None);
        let catalog = src.load_catalog(tmp.path()).unwrap();
        let entry = &catalog.toolchain_versions["2026.10.01"];
        src.materialize(tmp.path(), SidecarComponent::Toolchain, "2026.10.01", entry)
            .unwrap();
        assert!(!old.exists());
        assert!(tmp
            .path()
            .join("downloads/2026.10.01/yt-dlp_linux")
            .is_file());
    }

    #[test]
    fn hostile_tag_or_asset_name_is_rejected_before_it_becomes_a_path() {
        // L-31
        let sums = format!("{}  yt-dlp_linux\n", "a".repeat(64));
        let err = catalog_from_release(
            &release_json_for("../../etc", "yt-dlp_linux"),
            &sums,
            &glibc_x86(),
            true,
        )
        .unwrap_err();
        assert!(err.contains("not a usable version name"), "{err}");
        let tmp = tempfile::tempdir().unwrap();
        let src = source(b"bin", None);
        let entry = CatalogVersionEntry {
            asset_url: Some(ASSET_URL.to_string()),
            asset_name: Some("../escape".to_string()),
            sha256: Some("a".repeat(64)),
            ..CatalogVersionEntry::local(PathBuf::new())
        };
        assert!(src
            .materialize(tmp.path(), SidecarComponent::Toolchain, "1", &entry)
            .is_err());
        assert!(!tmp.path().join("escape").exists());
    }

    #[test]
    fn musl_without_python3_does_not_offer_the_zipimport_asset() {
        // M-39
        let musl = PlatformInfo {
            libc: Libc::Musl,
            ..glibc_x86()
        };
        let sums = format!("{}  yt-dlp\n", "a".repeat(64));
        let err = catalog_from_release(&release_json(), &sums, &musl, false).unwrap_err();
        assert!(err.contains("python3"), "{err}");
        let ok = catalog_from_release(&release_json(), &sums, &musl, true).unwrap();
        assert_eq!(
            ok.toolchain_versions["2026.10.01"].asset_name.as_deref(),
            Some("yt-dlp")
        );
    }

    #[test]
    fn github_outage_does_not_hide_the_plugin_half() {
        // L-31
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(crate::sidecar_updates::UPDATE_CATALOG_FILENAME),
            r#"{"plugin_latest":"0.9.0","plugin_versions":{"0.9.0":{"source_path":"/x","sha256":"aa"}}}"#,
        )
        .unwrap();
        let http = http_with(b"bin", None);
        http.fail(LATEST_URL, "status code 500");
        let catalog = source_over(&http).load_catalog(tmp.path()).unwrap();
        assert_eq!(catalog.plugin_latest.as_deref(), Some("0.9.0"));
        assert!(catalog.toolchain_error.unwrap().contains("unavailable"));
    }

    #[test]
    fn malformed_local_catalog_is_reported_not_ignored() {
        // L-31
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path()
                .join(crate::sidecar_updates::UPDATE_CATALOG_FILENAME),
            "{ not json",
        )
        .unwrap();
        let catalog = source(b"bin", None).load_catalog(tmp.path()).unwrap();
        assert!(catalog
            .plugin_error
            .unwrap()
            .contains("parse update catalog"));
        assert!(
            catalog.toolchain_latest.is_some(),
            "toolchain half still loads"
        );
    }

    #[test]
    fn rate_limit_is_named_and_not_retried() {
        // L-32
        let tmp = tempfile::tempdir().unwrap();
        let http = http_with(b"bin", None);
        http.fail(LATEST_URL, "status code 403");
        let src = source_over(&http);
        let err = src
            .load_catalog(tmp.path())
            .unwrap()
            .toolchain_error
            .unwrap();
        assert!(err.contains("rate limit"), "{err}");
        assert_eq!(http.count(LATEST_URL), 1, "4xx is not retried");
        // Negative cache: no second request until invalidated.
        src.load_catalog(tmp.path()).unwrap();
        assert_eq!(http.count(LATEST_URL), 1);
        http.heal(LATEST_URL);
        src.invalidate_cache();
        assert!(src
            .load_catalog(tmp.path())
            .unwrap()
            .toolchain_error
            .is_none());
    }

    #[test]
    fn transient_error_is_retried_once() {
        // L-32
        let tmp = tempfile::tempdir().unwrap();
        let http = http_with(b"bin", None);
        http.fail(LATEST_URL, "connection reset");
        let catalog = source_over(&http).load_catalog(tmp.path()).unwrap();
        assert!(catalog.toolchain_error.is_some());
        assert_eq!(http.count(LATEST_URL), 2);
    }

    #[test]
    fn install_through_update_manager_sets_executable_bit() {
        use crate::sidecar_updates::{
            apply_sidecar_updates, SidecarRuntimeState, SidecarUpdateSettings,
        };
        let tmp = tempfile::tempdir().unwrap();
        let body = b"#!/bin/sh\necho yt-dlp 2026.10.01\n";
        let src = source(body, None);
        let mut state = SidecarRuntimeState::default();
        let result = apply_sidecar_updates(
            tmp.path(),
            &SidecarUpdateSettings::default(),
            &src,
            false,
            &mut state,
        )
        .unwrap();
        assert_eq!(result.applied, vec![SidecarComponent::Toolchain]);
        let manifest = crate::sidecar_updates::load_toolchain_manifest(tmp.path())
            .unwrap()
            .expect("manifest written");
        assert_eq!(manifest.version, "2026.10.01");
        assert_eq!(manifest.binary_name, "yt-dlp_linux");
        let installed = tmp.path().join("yt-dlp_linux");
        assert_eq!(std::fs::read(&installed).unwrap(), body);
        assert!(crate::toolchain_discovery::is_executable_file(&installed));
    }

    #[test]
    fn checksum_failure_leaves_the_installed_toolchain_untouched() {
        // H-22 end to end: nothing is swapped when the SUMS fetch fails.
        use crate::sidecar_updates::{
            apply_sidecar_updates, write_toolchain_manifest, SidecarRuntimeState,
            SidecarUpdateSettings, ToolchainManifest,
        };
        let tmp = tempfile::tempdir().unwrap();
        write_toolchain_manifest(tmp.path(), &ToolchainManifest::stub("2025.01.01")).unwrap();
        std::fs::write(tmp.path().join("yt-dlp"), b"old").unwrap();
        let http = http_with(b"new", None);
        http.remove(SUMS_URL);
        let src = source_over(&http);
        let mut state = SidecarRuntimeState::default();
        let outcome = apply_sidecar_updates(
            tmp.path(),
            &SidecarUpdateSettings::default(),
            &src,
            false,
            &mut state,
        );
        assert!(outcome.is_err() || outcome.unwrap().applied.is_empty());
        assert_eq!(std::fs::read(tmp.path().join("yt-dlp")).unwrap(), b"old");
        assert_eq!(
            http.count(ASSET_URL),
            0,
            "no asset downloaded without a checksum"
        );
    }
}
