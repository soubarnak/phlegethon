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

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

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
    /// Extractor IE keys this package provides (e.g. `"LuxureTV"`, `"XxxbpTV"`).
    pub extractors: Vec<String>,
}

impl PluginManifest {
    /// Canonical manifest for the in-tree Launch Site plugin package.
    pub fn launch_sites_default() -> Self {
        Self {
            version: "0.1.0".to_string(),
            package: PLUGIN_PACKAGE_NAME.to_string(),
            extractors: vec!["LuxureTV".to_string(), "XxxbpTV".to_string()],
        }
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
    serde_json::from_str(&raw)
        .map_err(|e| format!("failed to parse plugin manifest {}: {e}", path.display()))
}

/// Write a plugins manifest to a sidecar root.
pub fn write_manifest(sidecar_root: &Path, manifest: &PluginManifest) -> Result<(), String> {
    fs::create_dir_all(sidecar_root)
        .map_err(|e| format!("create sidecar root {}: {e}", sidecar_root.display()))?;
    let path = manifest_path(sidecar_root);
    let raw = serde_json::to_string_pretty(manifest)
        .map_err(|e| format!("serialize plugin manifest: {e}"))?;
    fs::write(&path, raw).map_err(|e| format!("write plugin manifest {}: {e}", path.display()))?;
    Ok(())
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
/// This is the independent update path: a newer plugin package can be swapped
/// without shipping a full app release.
pub fn install_plugin_package(
    sidecar_root: &Path,
    source_package: &Path,
    manifest: &PluginManifest,
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

    // Replace existing package for out-of-band updates.
    if dest.exists() {
        fs::remove_dir_all(&dest)
            .map_err(|e| format!("remove old plugin package {}: {e}", dest.display()))?;
    }
    fs::create_dir_all(&dest_parent)
        .map_err(|e| format!("create plugin dirs {}: {e}", dest_parent.display()))?;

    copy_dir_recursive(source_package, &dest)?;
    write_manifest(sidecar_root, manifest)?;

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
            fs::copy(entry.path(), &to).map_err(|e| {
                format!(
                    "copy {} -> {}: {e}",
                    entry.path().display(),
                    to.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_minimal_package(root: &Path) {
        let extractor = root
            .join("yt_dlp_plugins")
            .join("extractor");
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
        assert_eq!(loaded.version, "0.1.0");
        assert!(loaded.extractors.contains(&"LuxureTV".to_string()));
        assert!(loaded.extractors.contains(&"XxxbpTV".to_string()));
    }

    #[test]
    fn load_manifest_missing_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let err = load_manifest(tmp.path()).unwrap_err();
        assert!(err.contains("failed to read"), "got: {err}");
    }

    #[test]
    fn needs_update_when_versions_differ() {
        let installed = PluginManifest::launch_sites_default();
        assert!(needs_update(&installed, "0.2.0"));
        assert!(!needs_update(&installed, "0.1.0"));
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
        assert_eq!(loaded.version, "0.1.0");
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
        manifest.version = "0.2.0".to_string();
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
        assert_eq!(load_manifest(&sidecar).unwrap().version, "0.2.0");
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
}
