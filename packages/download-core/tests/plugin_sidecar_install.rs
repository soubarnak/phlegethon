//! Plugin sidecar install path against the in-tree Extractor Plugin package.

use download_core::{
    install_plugin_package, load_manifest, plugin_dirs_path, PluginManifest, PLUGIN_PACKAGE_NAME,
};
use std::path::PathBuf;

fn in_tree_plugin_package() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("extractor-plugins")
        .join("phlegethon-launch-sites")
}

#[test]
fn install_in_tree_launch_site_plugins_into_sidecar() {
    let source = in_tree_plugin_package();
    assert!(
        source
            .join("yt_dlp_plugins")
            .join("extractor")
            .join("luxuretv.py")
            .is_file(),
        "missing luxuretv plugin at {}",
        source.display()
    );
    assert!(
        source
            .join("yt_dlp_plugins")
            .join("extractor")
            .join("xxxbp.py")
            .is_file(),
        "missing xxxbp plugin at {}",
        source.display()
    );

    let tmp = tempfile::tempdir().unwrap();
    let sidecar = tmp.path().join("sidecar");
    let manifest = PluginManifest::launch_sites_default();
    let plugin_dirs = install_plugin_package(&sidecar, &source, &manifest).unwrap();

    assert_eq!(plugin_dirs_path(&sidecar), Some(plugin_dirs.clone()));
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
    assert_eq!(loaded.version, "0.2.0");
    assert!(loaded.extractors.contains(&"LuxureTV".to_string()));
    assert!(loaded.extractors.contains(&"XxxbpTV".to_string()));
}

/// The in-tree `plugins-manifest.json` and the Rust default must not drift apart.
#[test]
fn in_tree_manifest_matches_rust_default() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../extractor-plugins/plugins-manifest.json");
    let on_disk: PluginManifest =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(on_disk, PluginManifest::launch_sites_default());
}
