//! App-managed Extractor Plugin sidecar assets.
//!
//! Phlegethon-owned yt-dlp plugins (luxuretv, xxxbp.tv) ship beside the
//! Extractor Toolchain and are updateable independently of a full app release.
//!
//! Layout (portable / app-managed):
//! ```text
//! <sidecar_root>/
//!   yt-dlp-plugins/                 ← pass to yt-dlp --plugin-dirs
//!     phlegethon-launch-sites/
//!       yt_dlp_plugins/
//!         extractor/
//!           luxuretv.py
//!           xxxbp.py
//!   plugins-manifest.json           ← independent update metadata
//! ```

use crate::fsutil;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Package directory name under `yt-dlp-plugins/`.
pub const PLUGIN_PACKAGE_NAME: &str = "phlegethon-launch-sites";

/// Filename for the sidecar manifest (independent update path).
pub const MANIFEST_FILENAME: &str = "plugins-manifest.json";

/// Relative path from sidecar root to the directory passed as `--plugin-dirs`.
pub const PLUGIN_DIRS_REL: &str = "yt-dlp-plugins";

/// Metadata for app-managed Extractor Plugin assets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Semver-ish version string for the plugin package.
    pub version: String,
    /// Package directory name under `yt-dlp-plugins/`.
    pub package: String,
    /// Extractor IE keys this package provides (e.g. `"LuxureTV"`, `"XxxbpTVList"`).
    pub extractors: Vec<String>,
    /// SHA-256 of the files installed from the copy bundled in the app. Lets a
    /// same-version edit (or a damaged install) be refreshed on launch. Absent for
    /// packages installed from a catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundled_sha256: Option<String>,
    /// SHA-256 of the installed package files, written at install time. Lets a
    /// deleted or edited file be noticed instead of reported as up to date
    /// (M-41). Absent in manifests written by older builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_sha256: Option<String>,
}

impl PluginManifest {
    /// Reject a manifest whose `package` could leave the plugin directory.
    ///
    /// The value is joined into a path that is later removed with
    /// `remove_dir_all`, so `..`, separators and absolute paths are refused
    /// (M-37).
    pub fn validate(&self) -> Result<(), String> {
        if !fsutil::is_single_component(&self.package) {
            return Err(format!(
                "plugin manifest package {:?} is not a plain directory name",
                self.package
            ));
        }
        if self.extractors.is_empty() {
            return Err("plugin manifest lists no extractors".to_string());
        }
        for key in &self.extractors {
            if !is_extractor_key(key) {
                return Err(format!(
                    "plugin manifest extractor {key:?} is not a yt-dlp extractor key"
                ));
            }
        }
        Ok(())
    }

    /// Canonical manifest for the in-tree Launch Site plugin package.
    pub fn launch_sites_default() -> Self {
        Self {
            version: "0.2.1".to_string(),
            package: PLUGIN_PACKAGE_NAME.to_string(),
            extractors: ["LuxureTV", "LuxureTVList", "XxxbpTV", "XxxbpTVList"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            bundled_sha256: None,
            package_sha256: None,
        }
    }
}

/// A yt-dlp extractor key: ASCII letters, digits and underscores, not starting with a digit.
fn is_extractor_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `true` for a 64-character hex string.
pub(crate) fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Digest over the files of a plugin package: for each regular file in
/// ascending relative-path order, the `/`-separated path, a NUL, the content and
/// a NUL. Symlinks and other non-files are skipped, as the installer skips them.
/// Equal to [`bundled_digest`] for the embedded copy.
pub(crate) fn package_digest(dir: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
        for entry in fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| format!("read dir entry: {e}"))?;
            let ty = entry
                .file_type()
                .map_err(|e| format!("file type {}: {e}", entry.path().display()))?;
            if ty.is_dir() {
                collect(root, &entry.path(), out)?;
            } else if ty.is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|e| e.to_string())?
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push((rel, entry.path()));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    collect(dir, dir, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for (rel, path) in &files {
        let content = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        hasher.update(&content);
        hasher.update([0]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Whether the plugin package named by the installed manifest is present and
/// still matches the digest recorded at install time (M-41). A manifest from an
/// older build has no digest, so only the package files' presence is checked.
pub fn installed_package_is_intact(sidecar_root: &Path, manifest: &PluginManifest) -> bool {
    let Some(dirs) = plugin_dirs_path(sidecar_root) else {
        return false;
    };
    let package = dirs.join(&manifest.package);
    if !package.join("yt_dlp_plugins").is_dir() {
        return false;
    }
    match manifest.package_sha256.as_deref() {
        Some(expected) => package_digest(&package).is_ok_and(|actual| actual == expected),
        None => true,
    }
}

/// Resolve the directory to pass as yt-dlp `--plugin-dirs`.
///
/// Returns `sidecar_root/yt-dlp-plugins` when that directory exists.
pub fn plugin_dirs_path(sidecar_root: &Path) -> Option<PathBuf> {
    let dirs = sidecar_root.join(PLUGIN_DIRS_REL);
    if dirs.is_dir() {
        Some(dirs)
    } else {
        None
    }
}

/// Path to the plugins-manifest.json under a sidecar root.
pub fn manifest_path(sidecar_root: &Path) -> PathBuf {
    sidecar_root.join(MANIFEST_FILENAME)
}

/// Load and parse the plugins manifest from a sidecar root.
pub fn load_manifest(sidecar_root: &Path) -> Result<PluginManifest, String> {
    let path = manifest_path(sidecar_root);
    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("failed to read plugin manifest {}: {e}", path.display()))?;
    let manifest: PluginManifest = serde_json::from_str(&raw)
        .map_err(|e| format!("failed to parse plugin manifest {}: {e}", path.display()))?;
    manifest.validate()?;
    Ok(manifest)
}

/// Write a plugins manifest to a sidecar root (temp file, fsync, rename).
pub fn write_manifest(sidecar_root: &Path, manifest: &PluginManifest) -> Result<(), String> {
    fs::create_dir_all(sidecar_root)
        .map_err(|e| format!("create sidecar root {}: {e}", sidecar_root.display()))?;
    let path = manifest_path(sidecar_root);
    let raw = serde_json::to_string_pretty(manifest)
        .map_err(|e| format!("serialize plugin manifest: {e}"))?;
    fsutil::write_atomic(&path, raw.as_bytes())
        .map_err(|e| format!("write plugin manifest {}: {e}", path.display()))
}

/// File under the sidecar root that serializes writers across app instances.
const LOCK_FILENAME: &str = ".sidecar.lock";

/// Exclusive advisory lock on a sidecar root; released on drop.
///
/// Two app instances share one sidecar root. Without the lock both rewrite the
/// plugin package and manifest at startup (M-34).
pub(crate) struct SidecarLock {
    _file: fs::File,
}

/// Block until this process holds the exclusive lock on `sidecar_root`.
pub(crate) fn lock_sidecar_root(sidecar_root: &Path) -> Result<SidecarLock, String> {
    fs::create_dir_all(sidecar_root)
        .map_err(|e| format!("create sidecar root {}: {e}", sidecar_root.display()))?;
    let path = sidecar_root.join(LOCK_FILENAME);
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    file.lock_exclusive()
        .map_err(|e| format!("lock {}: {e}", path.display()))?;
    Ok(SidecarLock { _file: file })
}

/// Name prefixes of directories this module creates beside the plugin package.
const STAGING_PREFIXES: [&str; 3] = [".plugin-staging-", ".plugin-old-", ".bundled-plugins-"];

/// Remove staging and swap leftovers from a crashed run. Needs the sidecar lock,
/// so no live installer owns them (L-33).
fn sweep_staging(sidecar_root: &Path) {
    let Ok(entries) = fs::read_dir(sidecar_root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if STAGING_PREFIXES.iter().any(|p| name.starts_with(p)) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Whether an installed sidecar needs updating relative to a desired version.
pub fn needs_update(installed: &PluginManifest, desired_version: &str) -> bool {
    installed.version != desired_version
}

/// Install (or replace) a plugin package into the sidecar layout.
///
/// Copies `source_package` (a directory containing `yt_dlp_plugins/`) into
/// `sidecar_root/yt-dlp-plugins/<package>/` and writes `manifest`.
///
/// The package is copied to a staging directory beside the plugin directory and
/// moved into place, so a failed copy leaves the installed package and manifest
/// as they were, and the manifest is written last (H-23, M-36). Holds the
/// sidecar lock for the whole swap (M-34).
///
/// This is the independent update path: a newer plugin package can be swapped
/// without shipping a full app release.
pub fn install_plugin_package(
    sidecar_root: &Path,
    source_package: &Path,
    manifest: &PluginManifest,
) -> Result<PathBuf, String> {
    install_plugin_package_verified(sidecar_root, source_package, manifest, None)
}

/// [`install_plugin_package`] that also checks the copied files against
/// `expected_sha256` (see [`package_digest`]) before they replace anything.
///
/// The digest is taken from the staged copy, not from `source_package`, so a
/// source that changes after the check cannot slip in (M-43).
pub fn install_plugin_package_verified(
    sidecar_root: &Path,
    source_package: &Path,
    manifest: &PluginManifest,
    expected_sha256: Option<&str>,
) -> Result<PathBuf, String> {
    manifest.validate()?;
    let _lock = lock_sidecar_root(sidecar_root)?;
    sweep_staging(sidecar_root);
    install_plugin_package_locked(sidecar_root, source_package, manifest, expected_sha256)
}

/// [`install_plugin_package`] for a caller that already holds the sidecar lock.
fn install_plugin_package_locked(
    sidecar_root: &Path,
    source_package: &Path,
    manifest: &PluginManifest,
    expected_sha256: Option<&str>,
) -> Result<PathBuf, String> {
    if !source_package.is_dir() {
        return Err(format!(
            "plugin source is not a directory: {}",
            source_package.display()
        ));
    }
    let yt_dlp_plugins = source_package.join("yt_dlp_plugins");
    if !yt_dlp_plugins.is_dir() {
        return Err(format!(
            "plugin source missing yt_dlp_plugins/: {}",
            source_package.display()
        ));
    }

    let dest_parent = sidecar_root.join(PLUGIN_DIRS_REL);
    let dest = dest_parent.join(&manifest.package);
    fs::create_dir_all(&dest_parent)
        .map_err(|e| format!("create plugin dirs {}: {e}", dest_parent.display()))?;

    // Stage beside the plugin directory, not inside it: yt-dlp scans
    // `yt-dlp-plugins/` and would load a half-copied package.
    let staging = sidecar_root.join(format!(".plugin-staging-{}", Uuid::new_v4()));
    let staged = copy_dir_recursive(source_package, &staging).and_then(|()| {
        let actual = package_digest(&staging)?;
        match expected_sha256 {
            Some(expected) if !actual.eq_ignore_ascii_case(expected) => Err(format!(
                "plugin package checksum mismatch: expected {expected}, got {actual}"
            )),
            _ => Ok(actual),
        }
    });
    let package_sha256 = match staged {
        Ok(digest) => digest,
        Err(err) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(err);
        }
    };

    let retired = sidecar_root.join(format!(".plugin-old-{}", Uuid::new_v4()));
    let had_old = dest.exists();
    if had_old {
        if let Err(e) = fs::rename(&dest, &retired) {
            let _ = fs::remove_dir_all(&staging);
            return Err(format!("retire old plugin package {}: {e}", dest.display()));
        }
    }
    if let Err(e) = fs::rename(&staging, &dest) {
        if had_old {
            let _ = fs::rename(&retired, &dest);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(format!("install plugin package {}: {e}", dest.display()));
    }
    let _ = fsutil::sync_dir(&dest_parent);
    let _ = fs::remove_dir_all(&retired);

    let mut recorded = manifest.clone();
    recorded.package_sha256 = Some(package_sha256);
    write_manifest(sidecar_root, &recorded)?;

    Ok(dest_parent)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| format!("create {}: {e}", dst.display()))?;
    for entry in fs::read_dir(src).map_err(|e| format!("read {}: {e}", src.display()))? {
        let entry = entry.map_err(|e| format!("read dir entry: {e}"))?;
        let ty = entry
            .file_type()
            .map_err(|e| format!("file type {}: {e}", entry.path().display()))?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &to)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), &to)
                .map_err(|e| format!("copy {} -> {}: {e}", entry.path().display(), to.display()))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Plugins bundled in the app
// ---------------------------------------------------------------------------

/// Files of the in-tree Launch Site plugin package, embedded at compile time so
/// a fresh install works without any catalog, package layout or network.
const BUNDLED_FILES: &[(&str, &str)] = &[
    (
        "yt_dlp_plugins/extractor/_phlegethon_parse.py",
        include_str!(
            "../../extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/_phlegethon_parse.py"
        ),
    ),
    (
        "yt_dlp_plugins/extractor/luxuretv.py",
        include_str!(
            "../../extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/luxuretv.py"
        ),
    ),
    (
        "yt_dlp_plugins/extractor/xxxbp.py",
        include_str!(
            "../../extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/xxxbp.py"
        ),
    ),
];

const BUNDLED_MANIFEST_JSON: &str = include_str!("../../extractor-plugins/plugins-manifest.json");

/// What [`ensure_bundled_plugins`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundledInstall {
    /// The bundled package was written (first run, newer than installed, or refreshed).
    Installed { version: String },
    /// The installed package already matches the bundled one.
    UpToDate,
    /// A newer package (from an out-of-band update) is installed and was left alone.
    KeptNewer { installed: String },
}

/// Manifest of the plugin package embedded in this build.
pub fn bundled_manifest() -> PluginManifest {
    let mut manifest: PluginManifest = serde_json::from_str(BUNDLED_MANIFEST_JSON)
        .expect("bundled plugins-manifest.json is valid (checked by a unit test)");
    manifest.bundled_sha256 = Some(bundled_digest());
    manifest
}

fn bundled_digest() -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for (path, content) in BUNDLED_FILES {
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(content.as_bytes());
        hasher.update([0]);
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `a.b.c` comparison on the numeric components; non-numeric parts count as 0.
pub fn version_is_newer(candidate: &str, installed: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.trim_start_matches('v')
            .split(['.', '-', '+'])
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    }
    let (mut a, mut b) = (parts(candidate), parts(installed));
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    a > b
}

/// Install the plugin package embedded in this build into `sidecar_root`, unless
/// an equal or newer one is already there.
///
/// Call once at startup, before the yt-dlp configuration is built. A package that
/// was updated out of band to a higher version is never downgraded.
pub fn ensure_bundled_plugins(sidecar_root: &Path) -> Result<BundledInstall, String> {
    let _lock = lock_sidecar_root(sidecar_root)?;
    let bundled = bundled_manifest();
    if let Ok(installed) = load_manifest(sidecar_root) {
        if installed_package_is_intact(sidecar_root, &installed) {
            if version_is_newer(&installed.version, &bundled.version) {
                return Ok(BundledInstall::KeptNewer {
                    installed: installed.version,
                });
            }
            // A manifest from before `package_sha256` existed is rewritten once, so
            // later edits or deletions of the files are detectable.
            if installed.version == bundled.version
                && installed.bundled_sha256 == bundled.bundled_sha256
                && installed.package_sha256 == bundled.bundled_sha256
            {
                return Ok(BundledInstall::UpToDate);
            }
        }
    }

    sweep_staging(sidecar_root);
    let staging = sidecar_root.join(format!(".bundled-plugins-{}", Uuid::new_v4()));
    let result = (|| {
        for (rel, content) in BUNDLED_FILES {
            let path = staging.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("create {}: {e}", parent.display()))?;
            }
            fs::write(&path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
        }
        install_plugin_package_locked(sidecar_root, &staging, &bundled, None)
    })();
    let _ = fs::remove_dir_all(&staging);
    result.map(|_| BundledInstall::Installed {
        version: bundled.version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_minimal_package(root: &Path) {
        let extractor = root.join("yt_dlp_plugins").join("extractor");
        fs::create_dir_all(&extractor).unwrap();
        fs::write(extractor.join("luxuretv.py"), b"# fixture\n").unwrap();
        fs::write(extractor.join("xxxbp.py"), b"# fixture\n").unwrap();
    }

    #[test]
    fn plugin_dirs_path_none_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(plugin_dirs_path(tmp.path()).is_none());
    }

    #[test]
    fn plugin_dirs_path_some_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = tmp.path().join(PLUGIN_DIRS_REL);
        fs::create_dir_all(&dirs).unwrap();
        assert_eq!(plugin_dirs_path(tmp.path()), Some(dirs));
    }

    #[test]
    fn write_and_load_manifest_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = PluginManifest::launch_sites_default();
        write_manifest(tmp.path(), &manifest).unwrap();
        let loaded = load_manifest(tmp.path()).unwrap();
        assert_eq!(loaded, manifest);
        assert_eq!(loaded.version, "0.2.1");
        assert!(loaded.extractors.contains(&"LuxureTV".to_string()));
        assert!(loaded.extractors.contains(&"XxxbpTV".to_string()));
        assert!(loaded.extractors.contains(&"XxxbpTVList".to_string()));
    }

    #[test]
    fn load_manifest_missing_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let err = load_manifest(tmp.path()).unwrap_err();
        assert!(err.contains("failed to read"), "got: {err}");
    }

    #[test]
    fn install_plugin_package_copies_and_writes_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source-pkg");
        write_minimal_package(&source);

        let sidecar = tmp.path().join("sidecar");
        let manifest = PluginManifest::launch_sites_default();
        let plugin_dirs = install_plugin_package(&sidecar, &source, &manifest).unwrap();

        assert_eq!(plugin_dirs, sidecar.join(PLUGIN_DIRS_REL));
        assert!(plugin_dirs
            .join(PLUGIN_PACKAGE_NAME)
            .join("yt_dlp_plugins")
            .join("extractor")
            .join("luxuretv.py")
            .is_file());
        assert!(plugin_dirs
            .join(PLUGIN_PACKAGE_NAME)
            .join("yt_dlp_plugins")
            .join("extractor")
            .join("xxxbp.py")
            .is_file());

        let loaded = load_manifest(&sidecar).unwrap();
        assert_eq!(loaded.version, "0.2.1");
    }

    #[test]
    fn install_plugin_package_replaces_existing_for_update() {
        let tmp = tempfile::tempdir().unwrap();
        let source_v1 = tmp.path().join("v1");
        write_minimal_package(&source_v1);
        fs::write(
            source_v1
                .join("yt_dlp_plugins")
                .join("extractor")
                .join("luxuretv.py"),
            b"v1",
        )
        .unwrap();

        let sidecar = tmp.path().join("sidecar");
        let mut manifest = PluginManifest::launch_sites_default();
        install_plugin_package(&sidecar, &source_v1, &manifest).unwrap();

        let source_v2 = tmp.path().join("v2");
        write_minimal_package(&source_v2);
        fs::write(
            source_v2
                .join("yt_dlp_plugins")
                .join("extractor")
                .join("luxuretv.py"),
            b"v2-updated",
        )
        .unwrap();
        manifest.version = "0.3.0".to_string();
        install_plugin_package(&sidecar, &source_v2, &manifest).unwrap();

        let content = fs::read_to_string(
            sidecar
                .join(PLUGIN_DIRS_REL)
                .join(PLUGIN_PACKAGE_NAME)
                .join("yt_dlp_plugins")
                .join("extractor")
                .join("luxuretv.py"),
        )
        .unwrap();
        assert_eq!(content, "v2-updated");
        assert_eq!(load_manifest(&sidecar).unwrap().version, "0.3.0");
    }

    #[test]
    fn install_rejects_source_without_yt_dlp_plugins() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("bad");
        fs::create_dir_all(&source).unwrap();
        let err = install_plugin_package(
            &tmp.path().join("sidecar"),
            &source,
            &PluginManifest::launch_sites_default(),
        )
        .unwrap_err();
        assert!(err.contains("yt_dlp_plugins"), "got: {err}");
    }

    #[test]
    fn bundled_manifest_matches_the_in_tree_file_and_lists_every_extractor() {
        let m = bundled_manifest();
        assert_eq!(m.package, PLUGIN_PACKAGE_NAME);
        assert_eq!(m.version, PluginManifest::launch_sites_default().version);
        for key in ["LuxureTV", "LuxureTVList", "XxxbpTV", "XxxbpTVList"] {
            assert!(m.extractors.iter().any(|e| e == key), "{key}");
        }
        assert_eq!(m.bundled_sha256.as_deref().map(str::len), Some(64));
    }

    #[test]
    fn version_ordering_is_numeric_not_lexical() {
        assert!(version_is_newer("0.10.0", "0.9.0"));
        assert!(version_is_newer("1.0", "0.9.9"));
        assert!(!version_is_newer("0.2.0", "0.2.0"));
        assert!(!version_is_newer("0.2.0", "0.2.1"));
        assert!(version_is_newer("v0.3.0", "0.2.9"));
    }

    #[test]
    fn first_run_installs_the_bundled_package() {
        let tmp = tempfile::tempdir().unwrap();
        let outcome = ensure_bundled_plugins(tmp.path()).unwrap();
        assert!(matches!(outcome, BundledInstall::Installed { .. }));
        let base = plugin_dirs_path(tmp.path())
            .unwrap()
            .join(PLUGIN_PACKAGE_NAME);
        for file in ["_phlegethon_parse.py", "luxuretv.py", "xxxbp.py"] {
            assert!(
                base.join("yt_dlp_plugins/extractor").join(file).is_file(),
                "{file}"
            );
        }
        assert_eq!(
            load_manifest(tmp.path()).unwrap().version,
            bundled_manifest().version
        );
        // No staging directory is left behind.
        let leftovers: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".bundled-plugins")
            })
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn second_run_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_bundled_plugins(tmp.path()).unwrap();
        assert_eq!(
            ensure_bundled_plugins(tmp.path()).unwrap(),
            BundledInstall::UpToDate
        );
    }

    #[test]
    fn missing_files_or_a_changed_digest_are_repaired() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_bundled_plugins(tmp.path()).unwrap();
        let pkg = plugin_dirs_path(tmp.path())
            .unwrap()
            .join(PLUGIN_PACKAGE_NAME);
        fs::remove_dir_all(&pkg).unwrap();
        assert!(matches!(
            ensure_bundled_plugins(tmp.path()).unwrap(),
            BundledInstall::Installed { .. }
        ));

        let mut manifest = load_manifest(tmp.path()).unwrap();
        manifest.bundled_sha256 = Some("stale".to_string());
        write_manifest(tmp.path(), &manifest).unwrap();
        assert!(matches!(
            ensure_bundled_plugins(tmp.path()).unwrap(),
            BundledInstall::Installed { .. }
        ));
        assert_eq!(
            ensure_bundled_plugins(tmp.path()).unwrap(),
            BundledInstall::UpToDate
        );
    }

    #[test]
    fn older_installed_package_is_upgraded_and_newer_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_bundled_plugins(tmp.path()).unwrap();

        let mut old = load_manifest(tmp.path()).unwrap();
        old.version = "0.0.1".to_string();
        write_manifest(tmp.path(), &old).unwrap();
        assert!(matches!(
            ensure_bundled_plugins(tmp.path()).unwrap(),
            BundledInstall::Installed { .. }
        ));

        let marker = plugin_dirs_path(tmp.path())
            .unwrap()
            .join(PLUGIN_PACKAGE_NAME)
            .join("yt_dlp_plugins/extractor/xxxbp.py");
        fs::write(&marker, "# out-of-band update").unwrap();
        let mut newer = load_manifest(tmp.path()).unwrap();
        newer.version = "99.0.0".to_string();
        newer.package_sha256 = Some(
            package_digest(
                &plugin_dirs_path(tmp.path())
                    .unwrap()
                    .join(PLUGIN_PACKAGE_NAME),
            )
            .unwrap(),
        );
        write_manifest(tmp.path(), &newer).unwrap();
        assert_eq!(
            ensure_bundled_plugins(tmp.path()).unwrap(),
            BundledInstall::KeptNewer {
                installed: "99.0.0".to_string()
            }
        );
        assert_eq!(fs::read_to_string(&marker).unwrap(), "# out-of-band update");
    }

    #[test]
    fn failed_copy_keeps_the_installed_package_and_manifest() {
        // H-23 / M-36: the old package used to be deleted before the copy started.
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let sidecar = tmp.path().join("sidecar");
        let v1 = tmp.path().join("v1");
        write_minimal_package(&v1);
        let mut manifest = PluginManifest::launch_sites_default();
        install_plugin_package(&sidecar, &v1, &manifest).unwrap();

        let broken = tmp.path().join("broken");
        write_minimal_package(&broken);
        let unreadable = broken.join("yt_dlp_plugins/extractor/xxxbp.py");
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::File::open(&unreadable).is_ok() {
            return; // running as root: permissions do not stop the read
        }
        manifest.version = "0.9.0".to_string();
        assert!(install_plugin_package(&sidecar, &broken, &manifest).is_err());

        let live = sidecar.join(PLUGIN_DIRS_REL).join(PLUGIN_PACKAGE_NAME);
        assert!(live.join("yt_dlp_plugins/extractor/luxuretv.py").is_file());
        assert!(live.join("yt_dlp_plugins/extractor/xxxbp.py").is_file());
        assert_eq!(load_manifest(&sidecar).unwrap().version, "0.2.1");
        let leftovers: Vec<_> = fs::read_dir(&sidecar)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".plugin-"))
            .collect();
        assert!(leftovers.is_empty(), "staging directory left behind");
    }

    #[test]
    fn install_leaves_no_staging_or_temp_files() {
        let tmp = tempfile::tempdir().unwrap();
        let sidecar = tmp.path().join("sidecar");
        let src = tmp.path().join("src");
        write_minimal_package(&src);
        install_plugin_package(&sidecar, &src, &PluginManifest::launch_sites_default()).unwrap();
        let mut names: Vec<_> = fs::read_dir(&sidecar)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![".sidecar.lock", "plugins-manifest.json", "yt-dlp-plugins"]
        );
        let inner: Vec<_> = fs::read_dir(sidecar.join(PLUGIN_DIRS_REL))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(inner, vec![PLUGIN_PACKAGE_NAME]);
    }

    #[test]
    fn manifest_package_cannot_leave_the_plugin_directory() {
        // M-37: `"package": ".."` made `remove_dir_all` hit the sidecar root.
        let tmp = tempfile::tempdir().unwrap();
        let sidecar = tmp.path().join("sidecar");
        let src = tmp.path().join("src");
        write_minimal_package(&src);
        fs::create_dir_all(&sidecar).unwrap();
        fs::write(sidecar.join("keep-me"), b"x").unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep-me"), b"x").unwrap();

        for bad in ["..", "../..", "a/b", outside.to_str().unwrap(), ""] {
            let mut manifest = PluginManifest::launch_sites_default();
            manifest.package = bad.to_string();
            let err = install_plugin_package(&sidecar, &src, &manifest).unwrap_err();
            assert!(err.contains("not a plain directory name"), "{bad:?}: {err}");
            fs::write(
                manifest_path(&sidecar),
                serde_json::to_string(&manifest).unwrap(),
            )
            .unwrap();
            assert!(load_manifest(&sidecar).is_err(), "{bad:?}");
        }
        assert!(sidecar.join("keep-me").is_file());
        assert!(outside.join("keep-me").is_file());
    }

    #[test]
    fn install_waits_for_the_sidecar_lock() {
        // M-34: a second instance must not rewrite the package while the first is at it.
        let tmp = tempfile::tempdir().unwrap();
        let sidecar = tmp.path().join("sidecar");
        let src = tmp.path().join("src");
        write_minimal_package(&src);
        let held = lock_sidecar_root(&sidecar).unwrap();

        let (sidecar2, src2) = (sidecar.clone(), src.clone());
        let installer = std::thread::spawn(move || {
            install_plugin_package(&sidecar2, &src2, &PluginManifest::launch_sites_default())
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !manifest_path(&sidecar).exists() && !sidecar.join(PLUGIN_DIRS_REL).exists(),
            "installer ran while the lock was held"
        );
        drop(held);
        installer.join().unwrap().unwrap();
        assert!(manifest_path(&sidecar).is_file());
    }

    #[test]
    fn startup_sweeps_staging_directories_from_crashed_runs() {
        // L-33: each crash used to leave a `.bundled-plugins-<pid>` directory.
        let tmp = tempfile::tempdir().unwrap();
        for name in [
            ".bundled-plugins-4242",
            ".plugin-staging-x",
            ".plugin-old-y",
        ] {
            let dir = tmp.path().join(name);
            fs::create_dir_all(dir.join("yt_dlp_plugins")).unwrap();
        }
        ensure_bundled_plugins(tmp.path()).unwrap();
        let mut names: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![".sidecar.lock", "plugins-manifest.json", "yt-dlp-plugins"]
        );
    }

    fn package_with(root: &Path, body: &str) {
        let extractor = root.join("yt_dlp_plugins/extractor");
        fs::create_dir_all(&extractor).unwrap();
        fs::write(extractor.join("luxuretv.py"), body).unwrap();
    }

    #[test]
    fn package_digest_matches_the_bundled_digest() {
        let tmp = tempfile::tempdir().unwrap();
        for (rel, content) in BUNDLED_FILES {
            let path = tmp.path().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        assert_eq!(package_digest(tmp.path()).unwrap(), bundled_digest());
    }

    #[test]
    fn verified_install_rejects_a_wrong_digest_and_keeps_the_old_package() {
        // M-43
        let tmp = tempfile::tempdir().unwrap();
        let sidecar = tmp.path().join("sidecar");
        let v1 = tmp.path().join("v1");
        package_with(&v1, "v1");
        let manifest = PluginManifest::launch_sites_default();
        install_plugin_package(&sidecar, &v1, &manifest).unwrap();

        let v2 = tmp.path().join("v2");
        package_with(&v2, "v2 with a backdoor");
        let err = install_plugin_package_verified(&sidecar, &v2, &manifest, Some(&"0".repeat(64)))
            .unwrap_err();
        assert!(err.contains("checksum mismatch"), "{err}");
        let live = sidecar.join(PLUGIN_DIRS_REL).join(PLUGIN_PACKAGE_NAME);
        assert_eq!(
            fs::read_to_string(live.join("yt_dlp_plugins/extractor/luxuretv.py")).unwrap(),
            "v1"
        );

        let good = package_digest(&v2).unwrap();
        install_plugin_package_verified(&sidecar, &v2, &manifest, Some(&good)).unwrap();
        assert_eq!(
            fs::read_to_string(live.join("yt_dlp_plugins/extractor/luxuretv.py")).unwrap(),
            "v2 with a backdoor"
        );
        assert_eq!(load_manifest(&sidecar).unwrap().package_sha256, Some(good));
    }

    #[test]
    fn an_edited_or_deleted_package_file_is_not_intact() {
        // M-41
        let tmp = tempfile::tempdir().unwrap();
        let sidecar = tmp.path().join("sidecar");
        let src = tmp.path().join("src");
        package_with(&src, "original");
        install_plugin_package(&sidecar, &src, &PluginManifest::launch_sites_default()).unwrap();
        let manifest = load_manifest(&sidecar).unwrap();
        assert!(installed_package_is_intact(&sidecar, &manifest));

        let file = sidecar
            .join(PLUGIN_DIRS_REL)
            .join(PLUGIN_PACKAGE_NAME)
            .join("yt_dlp_plugins/extractor/luxuretv.py");
        fs::write(&file, "edited").unwrap();
        assert!(!installed_package_is_intact(&sidecar, &manifest));
        fs::remove_file(&file).unwrap();
        assert!(!installed_package_is_intact(&sidecar, &manifest));
    }

    #[test]
    fn bundled_install_repairs_an_edited_file_of_the_same_version() {
        // M-41: the doc comment promised this; only a missing directory was repaired.
        let tmp = tempfile::tempdir().unwrap();
        ensure_bundled_plugins(tmp.path()).unwrap();
        let file = plugin_dirs_path(tmp.path())
            .unwrap()
            .join(PLUGIN_PACKAGE_NAME)
            .join("yt_dlp_plugins/extractor/luxuretv.py");
        fs::write(&file, "# tampered\n").unwrap();
        assert!(matches!(
            ensure_bundled_plugins(tmp.path()).unwrap(),
            BundledInstall::Installed { .. }
        ));
        assert_ne!(fs::read_to_string(&file).unwrap(), "# tampered\n");
        assert_eq!(
            ensure_bundled_plugins(tmp.path()).unwrap(),
            BundledInstall::UpToDate
        );
    }

    #[test]
    fn manifest_extractor_keys_are_validated() {
        // L-34
        let tmp = tempfile::tempdir().unwrap();
        for bad in [
            vec![],
            vec!["".to_string()],
            vec!["Lux ure".to_string()],
            vec!["../x".to_string()],
            vec!["1Abc".to_string()],
        ] {
            let mut manifest = PluginManifest::launch_sites_default();
            manifest.extractors = bad.clone();
            fs::write(
                manifest_path(tmp.path()),
                serde_json::to_string(&manifest).unwrap(),
            )
            .unwrap();
            assert!(load_manifest(tmp.path()).is_err(), "{bad:?}");
        }
    }
}
