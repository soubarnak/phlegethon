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
//! manager copies it into place (and sets the executable bit).
//!
//! Extractor Plugin versions still come from the local `update-catalog.json`
//! (see [`FileSidecarUpdateSource`]); this source merges both.

use crate::http::HttpBackend;
use crate::sidecar_updates::{
    CatalogVersionEntry, FileSidecarUpdateSource, SidecarComponent, SidecarUpdateSource,
    UpdateCatalog,
};
use crate::toolchain_discovery::{Libc, PlatformInfo};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
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
pub fn catalog_from_release(
    json: &str,
    sums: Option<&str>,
    platform: &PlatformInfo,
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
    let sha256 = sums
        .map(parse_sha256sums)
        .and_then(|m| m.get(wanted).cloned());
    let mut catalog = UpdateCatalog::default();
    catalog.toolchain_latest = Some(release.tag_name.clone());
    catalog.toolchain_versions.insert(
        release.tag_name,
        CatalogVersionEntry {
            source_path: PathBuf::new(),
            asset_url: Some(asset.browser_download_url.clone()),
            asset_name: Some(asset.name.clone()),
            sha256,
        },
    );
    Ok(catalog)
}

/// Hex SHA-256 of a file.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

/// yt-dlp releases on GitHub (toolchain) + local catalog (plugins).
pub struct GitHubReleaseUpdateSource {
    http: Arc<dyn HttpBackend>,
    repo: String,
    platform: PlatformInfo,
    cache: Mutex<Option<(Instant, UpdateCatalog)>>,
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
            cache: Mutex::new(None),
            plugins: FileSidecarUpdateSource,
        }
    }

    /// Asset this host will receive.
    pub fn asset_name(&self) -> &'static str {
        select_asset(&self.platform)
    }

    fn latest_url(&self) -> String {
        format!("https://api.github.com/repos/{}/releases/latest", self.repo)
    }

    /// Drop the cached release lookup (tests / explicit refresh).
    pub fn invalidate(&self) {
        if let Ok(mut guard) = self.cache.lock() {
            *guard = None;
        }
    }

    fn fetch_toolchain_catalog(&self) -> Result<UpdateCatalog, String> {
        if let Ok(guard) = self.cache.lock() {
            if let Some((at, catalog)) = guard.as_ref() {
                if at.elapsed() < CATALOG_CACHE_TTL {
                    return Ok(catalog.clone());
                }
            }
        }
        let (body, _, _) = self
            .http
            .fetch_capped(&self.latest_url(), METADATA_CAP)
            .map_err(|e| format!("GitHub release lookup failed: {e}"))?;
        let json = String::from_utf8_lossy(&body);
        let release: ReleaseJson =
            serde_json::from_str(&json).map_err(|e| format!("parse GitHub release JSON: {e}"))?;
        let sums = release
            .assets
            .iter()
            .find(|a| a.name == "SHA2-256SUMS")
            .and_then(|a| {
                self.http
                    .fetch_capped(&a.browser_download_url, METADATA_CAP)
                    .ok()
            })
            .map(|(bytes, _, _)| String::from_utf8_lossy(&bytes).into_owned());
        let catalog = catalog_from_release(&json, sums.as_deref(), &self.platform)?;
        if let Ok(mut guard) = self.cache.lock() {
            *guard = Some((Instant::now(), catalog.clone()));
        }
        Ok(catalog)
    }
}

impl SidecarUpdateSource for GitHubReleaseUpdateSource {
    fn load_catalog(&self, sidecar_root: &Path) -> Result<UpdateCatalog, String> {
        let mut catalog = self.plugins.load_catalog(sidecar_root).unwrap_or_default();
        let toolchain = self.fetch_toolchain_catalog()?;
        catalog.toolchain_latest = toolchain.toolchain_latest;
        catalog.toolchain_versions = toolchain.toolchain_versions;
        Ok(catalog)
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
        let dir = sidecar_root.join(DOWNLOADS_DIR).join(version);
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let dest = dir.join(name);

        // Reuse a previous, verified download.
        if dest.is_file() {
            match &entry.sha256 {
                Some(expected) if sha256_file(&dest)? == *expected => return Ok(dest),
                None => return Ok(dest),
                Some(_) => {
                    let _ = std::fs::remove_file(&dest);
                }
            }
        }

        let tmp = dir.join(format!("{name}.part"));
        {
            let mut file = std::fs::File::create(&tmp)
                .map_err(|e| format!("create {}: {e}", tmp.display()))?;
            self.http
                .fetch(url, None, &mut file)
                .map_err(|e| format!("download {name} {version}: {e}"))?;
            file.flush()
                .map_err(|e| format!("flush {}: {e}", tmp.display()))?;
        }
        if let Some(expected) = &entry.sha256 {
            let actual = sha256_file(&tmp)?;
            if actual != *expected {
                let _ = std::fs::remove_file(&tmp);
                return Err(format!(
                    "checksum mismatch for {name} {version}: expected {expected}, got {actual}"
                ));
            }
        }
        std::fs::rename(&tmp, &dest)
            .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), dest.display()))?;
        Ok(dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::FetchResult;
    use crate::proxy_settings::ProxySettings;

    struct CannedHttp {
        bodies: HashMap<String, Vec<u8>>,
        calls: Mutex<Vec<String>>,
    }

    impl HttpBackend for CannedHttp {
        fn fetch(
            &self,
            url: &str,
            _range: Option<std::ops::Range<u64>>,
            sink: &mut dyn Write,
        ) -> Result<FetchResult, String> {
            self.calls.lock().unwrap().push(url.to_string());
            let body = self.bodies.get(url).ok_or_else(|| format!("404 {url}"))?;
            sink.write_all(body).map_err(|e| e.to_string())?;
            Ok(FetchResult {
                total_length: body.len() as u64,
                bytes_written: body.len() as u64,
            })
        }
        fn supports_range(&self, _url: &str) -> Result<bool, String> {
            Ok(false)
        }
        fn file_size(&self, url: &str) -> Result<u64, String> {
            Ok(self.bodies.get(url).map(|b| b.len() as u64).unwrap_or(0))
        }
        fn apply_proxy_settings(&self, _proxy: &ProxySettings) {}
    }

    fn glibc_x86() -> PlatformInfo {
        PlatformInfo {
            os: "linux".into(),
            arch: "x86_64".into(),
            libc: Libc::Glibc,
        }
    }

    fn release_json() -> String {
        r#"{"tag_name":"2026.10.01","assets":[
            {"name":"yt-dlp","browser_download_url":"https://dl.example/yt-dlp"},
            {"name":"yt-dlp_linux","browser_download_url":"https://dl.example/yt-dlp_linux"},
            {"name":"yt-dlp.exe","browser_download_url":"https://dl.example/yt-dlp.exe"},
            {"name":"SHA2-256SUMS","browser_download_url":"https://dl.example/SHA2-256SUMS"}
        ]}"#
        .to_string()
    }

    fn source(binary: &[u8], sums_hex: Option<&str>) -> GitHubReleaseUpdateSource {
        let mut bodies = HashMap::new();
        bodies.insert(
            "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest".to_string(),
            release_json().into_bytes(),
        );
        bodies.insert(
            "https://dl.example/yt-dlp_linux".to_string(),
            binary.to_vec(),
        );
        let sums = format!(
            "{}  yt-dlp_linux\n{}  yt-dlp\n",
            sums_hex
                .map(str::to_string)
                .unwrap_or_else(|| format!("{:x}", Sha256::digest(binary))),
            "0".repeat(64)
        );
        bodies.insert(
            "https://dl.example/SHA2-256SUMS".to_string(),
            sums.into_bytes(),
        );
        GitHubReleaseUpdateSource::new(
            Arc::new(CannedHttp {
                bodies,
                calls: Mutex::new(Vec::new()),
            }),
            glibc_x86(),
        )
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
        let src = source(b"#!/bin/sh\necho yt-dlp\n", None);
        let catalog = src.load_catalog(tmp.path()).unwrap();
        assert_eq!(catalog.toolchain_latest.as_deref(), Some("2026.10.01"));
        let entry = &catalog.toolchain_versions["2026.10.01"];
        assert_eq!(entry.asset_name.as_deref(), Some("yt-dlp_linux"));
        assert_eq!(
            entry.asset_url.as_deref(),
            Some("https://dl.example/yt-dlp_linux")
        );
        assert!(entry.sha256.is_some());
        // Second lookup hits the cache (no new API call).
        let http_calls_before = {
            let any: &dyn std::any::Any = &src.http;
            let _ = any;
            0
        };
        let _ = http_calls_before;
        let again = src.load_catalog(tmp.path()).unwrap();
        assert_eq!(again.toolchain_latest, catalog.toolchain_latest);
    }

    #[test]
    fn materialize_downloads_verifies_and_reuses() {
        let tmp = tempfile::tempdir().unwrap();
        let body = b"#!/bin/sh\necho yt-dlp 2026.10.01\n";
        let src = source(body, None);
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
        // Reuse without re-downloading.
        let again = src
            .materialize(tmp.path(), SidecarComponent::Toolchain, "2026.10.01", entry)
            .unwrap();
        assert_eq!(again, path);
        // Plugin entries are untouched (local paths).
        let local = CatalogVersionEntry::local(PathBuf::from("/tmp/x"));
        assert_eq!(
            src.materialize(tmp.path(), SidecarComponent::Plugin, "1", &local)
                .unwrap(),
            PathBuf::from("/tmp/x")
        );
    }

    #[test]
    fn materialize_rejects_checksum_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let src = source(b"real bytes", Some(&"f".repeat(64)));
        let catalog = src.load_catalog(tmp.path()).unwrap();
        let entry = &catalog.toolchain_versions["2026.10.01"];
        let err = src
            .materialize(tmp.path(), SidecarComponent::Toolchain, "2026.10.01", entry)
            .unwrap_err();
        assert!(err.contains("checksum mismatch"), "{err}");
        assert!(!tmp
            .path()
            .join("downloads/2026.10.01/yt-dlp_linux")
            .exists());
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
}
