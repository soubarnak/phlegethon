//! App-managed Extractor Toolchain and Extractor Plugin update manager.
//!
//! Manual check plus optional on-launch check (off by default). Updates apply
//! only when idle or are deferred to the next launch. Pin and rollback supported.
//! Sidecars are never swapped while a transfer is active.

use crate::fsutil;
use crate::plugin_sidecar::{
    install_plugin_package, load_manifest, PluginManifest,
    MANIFEST_FILENAME as PLUGIN_MANIFEST_FILENAME,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Filename for the Extractor Toolchain manifest under a sidecar root.
pub const TOOLCHAIN_MANIFEST_FILENAME: &str = "toolchain-manifest.json";

/// Metadata directory under a sidecar root.
const SIDECAR_META_DIR: &str = ".phlegethon";

/// Persisted sidecar runtime state (deferred apply, rollback snapshots).
const SIDECAR_STATE_FILENAME: &str = "sidecar-state.json";

/// Optional update catalog shipped beside sidecar assets.
pub const UPDATE_CATALOG_FILENAME: &str = "update-catalog.json";

/// User-facing update manager settings (persisted with the Library Root queue).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarUpdateSettings {
    /// When true, Core checks for updates on launch and applies when idle.
    #[serde(default)]
    pub check_on_launch: bool,
    /// When set, the toolchain stays on this version (no auto-upgrade past pin).
    #[serde(default)]
    pub toolchain_pin: Option<String>,
    /// When set, the Extractor Plugin package stays on this version.
    #[serde(default)]
    pub plugin_pin: Option<String>,
}

impl Default for SidecarUpdateSettings {
    fn default() -> Self {
        Self {
            check_on_launch: false,
            toolchain_pin: None,
            plugin_pin: None,
        }
    }
}

/// Which app-managed sidecar component is being updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidecarComponent {
    Toolchain,
    Plugin,
}

/// Installed vs available status for one sidecar component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarComponentStatus {
    pub component: SidecarComponent,
    pub installed_version: Option<String>,
    pub available_version: Option<String>,
    pub pinned_version: Option<String>,
    pub update_available: bool,
    /// Why the available version is unknown (release lookup failed, catalog
    /// unreadable). `None` when the lookup worked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lookup_error: Option<String>,
}

/// Result of a manual or on-launch update check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarUpdateCheckResult {
    pub toolchain: SidecarComponentStatus,
    pub plugin: SidecarComponentStatus,
}

/// How an apply request concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidecarApplyOutcome {
    Applied,
    DeferredUntilIdle,
    DeferredUntilNextLaunch,
    NothingToApply,
    BlockedMidTransfer,
}

/// Result of attempting to apply pending / available sidecar updates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarApplyResult {
    pub outcome: SidecarApplyOutcome,
    pub applied: Vec<SidecarComponent>,
}

/// Metadata for the Extractor Toolchain binary under a sidecar root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolchainManifest {
    pub version: String,
    #[serde(default = "default_toolchain_binary_name")]
    pub binary_name: String,
}

fn default_toolchain_binary_name() -> String {
    if cfg!(windows) {
        "yt-dlp.exe".to_string()
    } else {
        "yt-dlp".to_string()
    }
}

impl ToolchainManifest {
    /// Reject a `binary_name` that could leave the sidecar root (M-37).
    pub fn validate(&self) -> Result<(), String> {
        if fsutil::is_single_component(&self.binary_name) {
            Ok(())
        } else {
            Err(format!(
                "toolchain manifest binary_name {:?} is not a plain file name",
                self.binary_name
            ))
        }
    }

    pub fn stub(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            binary_name: default_toolchain_binary_name(),
        }
    }
}

/// On-disk catalog entry for one component version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogVersionEntry {
    /// Path to a toolchain binary file or plugin package directory.
    ///
    /// Empty for network entries until [`SidecarUpdateSource::materialize`] runs.
    #[serde(default)]
    pub source_path: PathBuf,
    /// Download URL for a network-backed toolchain asset.
    #[serde(default)]
    pub asset_url: Option<String>,
    /// File name the asset is installed under (becomes the manifest `binary_name`).
    #[serde(default)]
    pub asset_name: Option<String>,
    /// Expected SHA-256 (hex) of the asset, verified after download.
    #[serde(default)]
    pub sha256: Option<String>,
}

impl CatalogVersionEntry {
    /// Entry backed by a local file or directory.
    pub fn local(source_path: impl Into<PathBuf>) -> Self {
        Self {
            source_path: source_path.into(),
            asset_url: None,
            asset_name: None,
            sha256: None,
        }
    }
}

/// Optional catalog listing fetchable sidecar versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UpdateCatalog {
    #[serde(default)]
    pub toolchain_latest: Option<String>,
    #[serde(default)]
    pub toolchain_versions: std::collections::HashMap<String, CatalogVersionEntry>,
    #[serde(default)]
    pub plugin_latest: Option<String>,
    #[serde(default)]
    pub plugin_versions: std::collections::HashMap<String, CatalogVersionEntry>,
    /// Set by a source when the toolchain half could not be loaded, so a failed
    /// lookup does not hide the plugin half (L-31).
    #[serde(skip)]
    pub toolchain_error: Option<String>,
    /// Set by a source when the plugin catalog file is unreadable.
    #[serde(skip)]
    pub plugin_error: Option<String>,
}

/// Persisted runtime state for deferred apply and rollback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SidecarRuntimeState {
    #[serde(default)]
    pub deferred_apply: bool,
    #[serde(default)]
    pub rollback: SidecarRollbackState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SidecarRollbackState {
    pub toolchain: Option<RollbackSnapshot>,
    pub plugin: Option<RollbackSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackSnapshot {
    pub version: String,
    pub backup_root: PathBuf,
}

/// Injectable update source — production reads `update-catalog.json` (plugins)
/// and GitHub releases (toolchain); tests use fakes.
pub trait SidecarUpdateSource: Send + Sync {
    fn load_catalog(&self, sidecar_root: &Path) -> Result<UpdateCatalog, String>;

    /// Drop any cached lookup so the next [`Self::load_catalog`] asks upstream
    /// again. A manual Check calls this (L-25).
    fn invalidate_cache(&self) {}

    /// Make `entry` available as a local path (download + verify for network
    /// sources). Default: the entry already points at a local path.
    fn materialize(
        &self,
        _sidecar_root: &Path,
        _component: SidecarComponent,
        _version: &str,
        entry: &CatalogVersionEntry,
    ) -> Result<PathBuf, String> {
        if entry.source_path.as_os_str().is_empty() {
            return Err("catalog entry has no local source path".to_string());
        }
        Ok(entry.source_path.clone())
    }
}

/// Default source: read `update-catalog.json` beside sidecar assets.
#[derive(Debug, Default)]
pub struct FileSidecarUpdateSource;

impl SidecarUpdateSource for FileSidecarUpdateSource {
    fn load_catalog(&self, sidecar_root: &Path) -> Result<UpdateCatalog, String> {
        let path = sidecar_root.join(UPDATE_CATALOG_FILENAME);
        if !path.is_file() {
            return Ok(UpdateCatalog::default());
        }
        let raw = fs::read_to_string(&path)
            .map_err(|e| format!("read update catalog {}: {e}", path.display()))?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("parse update catalog {}: {e}", path.display()))
    }
}

/// In-memory catalog for acceptance tests.
#[derive(Debug, Default)]
pub struct FakeSidecarUpdateSource {
    catalog: Mutex<UpdateCatalog>,
}

impl FakeSidecarUpdateSource {
    pub fn new(catalog: UpdateCatalog) -> Arc<Self> {
        Arc::new(Self {
            catalog: Mutex::new(catalog),
        })
    }

    pub fn set_catalog(&self, catalog: UpdateCatalog) {
        *self.catalog.lock().unwrap() = catalog;
    }
}

impl SidecarUpdateSource for FakeSidecarUpdateSource {
    fn load_catalog(&self, _sidecar_root: &Path) -> Result<UpdateCatalog, String> {
        Ok(self.catalog.lock().unwrap().clone())
    }
}

pub fn toolchain_manifest_path(sidecar_root: &Path) -> PathBuf {
    sidecar_root.join(TOOLCHAIN_MANIFEST_FILENAME)
}

pub fn toolchain_binary_path(sidecar_root: &Path, manifest: &ToolchainManifest) -> PathBuf {
    sidecar_root.join(&manifest.binary_name)
}

pub fn sidecar_state_path(sidecar_root: &Path) -> PathBuf {
    sidecar_root
        .join(SIDECAR_META_DIR)
        .join(SIDECAR_STATE_FILENAME)
}

pub fn load_toolchain_manifest(sidecar_root: &Path) -> Result<Option<ToolchainManifest>, String> {
    let path = toolchain_manifest_path(sidecar_root);
    if !path.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("read toolchain manifest {}: {e}", path.display()))?;
    let manifest: ToolchainManifest = serde_json::from_str(&raw)
        .map_err(|e| format!("parse toolchain manifest {}: {e}", path.display()))?;
    manifest.validate()?;
    Ok(Some(manifest))
}

pub fn write_toolchain_manifest(
    sidecar_root: &Path,
    manifest: &ToolchainManifest,
) -> Result<(), String> {
    fs::create_dir_all(sidecar_root)
        .map_err(|e| format!("create sidecar root {}: {e}", sidecar_root.display()))?;
    let path = toolchain_manifest_path(sidecar_root);
    let raw = serde_json::to_string_pretty(manifest)
        .map_err(|e| format!("serialize toolchain manifest: {e}"))?;
    fsutil::write_atomic(&path, raw.as_bytes())
        .map_err(|e| format!("write toolchain manifest {}: {e}", path.display()))
}

/// Load the persisted runtime state.
///
/// A state file that does not parse is moved aside to `sidecar-state.json.corrupt`
/// and replaced by defaults: the file only holds a deferred flag and rollback
/// snapshots, and failing here would leave every sidecar command reporting
/// "sidecar root not configured" for the whole session (M-38).
pub fn load_sidecar_runtime_state(sidecar_root: &Path) -> Result<SidecarRuntimeState, String> {
    let path = sidecar_state_path(sidecar_root);
    if !path.is_file() {
        return Ok(SidecarRuntimeState::default());
    }
    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("read sidecar state {}: {e}", path.display()))?;
    match serde_json::from_str(&raw) {
        Ok(state) => Ok(state),
        Err(_) => {
            let _ = fs::rename(&path, path.with_extension("json.corrupt"));
            Ok(SidecarRuntimeState::default())
        }
    }
}

pub fn save_sidecar_runtime_state(
    sidecar_root: &Path,
    state: &SidecarRuntimeState,
) -> Result<(), String> {
    let dir = sidecar_root.join(SIDECAR_META_DIR);
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = sidecar_state_path(sidecar_root);
    let raw =
        serde_json::to_string_pretty(state).map_err(|e| format!("serialize sidecar state: {e}"))?;
    fsutil::write_atomic(&path, raw.as_bytes())
        .map_err(|e| format!("write sidecar state {}: {e}", path.display()))
}

fn pinned_version<'a>(
    settings: &'a SidecarUpdateSettings,
    component: SidecarComponent,
) -> Option<&'a str> {
    match component {
        SidecarComponent::Toolchain => settings
            .toolchain_pin
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        SidecarComponent::Plugin => settings
            .plugin_pin
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    }
}

fn target_version(
    catalog: &UpdateCatalog,
    component: SidecarComponent,
    settings: &SidecarUpdateSettings,
) -> Option<String> {
    if let Some(pin) = pinned_version(settings, component) {
        return Some(pin.to_string());
    }
    match component {
        SidecarComponent::Toolchain => catalog.toolchain_latest.clone(),
        SidecarComponent::Plugin => catalog.plugin_latest.clone(),
    }
}

fn catalog_entry<'a>(
    catalog: &'a UpdateCatalog,
    component: SidecarComponent,
    version: &str,
) -> Option<&'a CatalogVersionEntry> {
    match component {
        SidecarComponent::Toolchain => catalog.toolchain_versions.get(version),
        SidecarComponent::Plugin => catalog.plugin_versions.get(version),
    }
}

fn installed_plugin_version(sidecar_root: &Path) -> Option<String> {
    load_manifest(sidecar_root).ok().map(|m| m.version)
}

fn installed_toolchain_version(sidecar_root: &Path) -> Option<String> {
    load_toolchain_manifest(sidecar_root)
        .ok()
        .flatten()
        .map(|m| m.version)
}

fn component_status(
    component: SidecarComponent,
    installed: Option<String>,
    available: Option<String>,
    settings: &SidecarUpdateSettings,
    lookup_error: Option<String>,
) -> SidecarComponentStatus {
    let pinned = pinned_version(settings, component).map(str::to_string);
    let update_available = match (&installed, &available, &pinned) {
        (_, None, _) => false,
        (Some(inst), Some(avail), _) if inst == avail => false,
        (_, Some(avail), Some(pin)) => avail != pin || installed.as_deref() != Some(pin.as_str()),
        (Some(inst), Some(avail), None) => inst != avail,
        (None, Some(_), _) => true,
    };
    SidecarComponentStatus {
        component,
        installed_version: installed,
        available_version: if pinned.is_some() {
            pinned.clone()
        } else {
            available
        },
        pinned_version: pinned,
        update_available,
        lookup_error,
    }
}

/// Check installed vs catalog versions, honoring pins.
pub fn check_sidecar_updates(
    sidecar_root: &Path,
    settings: &SidecarUpdateSettings,
    source: &dyn SidecarUpdateSource,
) -> Result<SidecarUpdateCheckResult, String> {
    let catalog = source.load_catalog(sidecar_root)?;
    let toolchain_target = target_version(&catalog, SidecarComponent::Toolchain, settings);
    let plugin_target = target_version(&catalog, SidecarComponent::Plugin, settings);
    Ok(SidecarUpdateCheckResult {
        toolchain: component_status(
            SidecarComponent::Toolchain,
            installed_toolchain_version(sidecar_root),
            toolchain_target,
            settings,
            catalog.toolchain_error.clone(),
        ),
        plugin: component_status(
            SidecarComponent::Plugin,
            installed_plugin_version(sidecar_root),
            plugin_target,
            settings,
            catalog.plugin_error.clone(),
        ),
    })
}

fn rollback_dir(sidecar_root: &Path, component: SidecarComponent) -> PathBuf {
    sidecar_root
        .join(SIDECAR_META_DIR)
        .join("rollback")
        .join(match component {
            SidecarComponent::Toolchain => "toolchain",
            SidecarComponent::Plugin => "plugin",
        })
}

fn remove_dir_if_exists(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_dir_all(path).map_err(|e| format!("remove {}: {e}", path.display()))?;
    }
    Ok(())
}

fn copy_file(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    fs::copy(src, dst).map_err(|e| format!("copy {} -> {}: {e}", src.display(), dst.display()))?;
    Ok(())
}

fn snapshot_toolchain_for_rollback(
    sidecar_root: &Path,
    state: &mut SidecarRuntimeState,
) -> Result<(), String> {
    let Some(manifest) = load_toolchain_manifest(sidecar_root)? else {
        state.rollback.toolchain = None;
        return Ok(());
    };
    let binary = toolchain_binary_path(sidecar_root, &manifest);
    if !binary.is_file() {
        state.rollback.toolchain = None;
        return Ok(());
    }
    let backup_root = rollback_dir(sidecar_root, SidecarComponent::Toolchain);
    remove_dir_if_exists(&backup_root)?;
    fs::create_dir_all(&backup_root)
        .map_err(|e| format!("create {}: {e}", backup_root.display()))?;
    copy_file(&binary, &backup_root.join(&manifest.binary_name))?;
    let manifest_raw = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("serialize toolchain manifest: {e}"))?;
    fsutil::write_atomic(
        &backup_root.join(TOOLCHAIN_MANIFEST_FILENAME),
        manifest_raw.as_bytes(),
    )
    .map_err(|e| format!("write rollback toolchain manifest: {e}"))?;
    state.rollback.toolchain = Some(RollbackSnapshot {
        version: manifest.version.clone(),
        backup_root,
    });
    Ok(())
}

fn snapshot_plugin_for_rollback(
    sidecar_root: &Path,
    state: &mut SidecarRuntimeState,
) -> Result<(), String> {
    let Ok(manifest) = load_manifest(sidecar_root) else {
        state.rollback.plugin = None;
        return Ok(());
    };
    let package_src = sidecar_root
        .join(crate::plugin_sidecar::PLUGIN_DIRS_REL)
        .join(&manifest.package);
    if !package_src.is_dir() {
        state.rollback.plugin = None;
        return Ok(());
    }
    let backup_root = rollback_dir(sidecar_root, SidecarComponent::Plugin);
    remove_dir_if_exists(&backup_root)?;
    copy_dir_recursive(&package_src, &backup_root.join("package"))?;
    fs::copy(
        sidecar_root.join(PLUGIN_MANIFEST_FILENAME),
        backup_root.join(PLUGIN_MANIFEST_FILENAME),
    )
    .map_err(|e| format!("copy plugin manifest for rollback: {e}"))?;
    state.rollback.plugin = Some(RollbackSnapshot {
        version: manifest.version,
        backup_root,
    });
    Ok(())
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
            copy_file(&entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Copy `src` to `dst` through a temp file in the same directory, mark it
/// executable, and rename it into place. A running yt-dlp keeps the old inode;
/// nothing ever sees a half-written binary (H-23).
fn install_executable_atomic(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let tmp = fsutil::temp_sibling(dst);
    let staged = (|| {
        fs::copy(src, &tmp)
            .map_err(|e| format!("copy {} -> {}: {e}", src.display(), tmp.display()))?;
        mark_executable(&tmp)?;
        fs::File::open(&tmp)
            .and_then(|f| f.sync_all())
            .map_err(|e| format!("sync {}: {e}", tmp.display()))?;
        fsutil::rename_durable(&tmp, dst)
            .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), dst.display()))
    })();
    if staged.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    staged
}

fn install_toolchain_version(
    sidecar_root: &Path,
    version: &str,
    entry: &CatalogVersionEntry,
    source: &dyn SidecarUpdateSource,
) -> Result<(), String> {
    let src = source.materialize(sidecar_root, SidecarComponent::Toolchain, version, entry)?;
    let mut manifest = ToolchainManifest::stub(version);
    if let Some(name) = entry.asset_name.as_deref().filter(|n| !n.is_empty()) {
        manifest.binary_name = name.to_string();
    }
    manifest.validate()?;
    let previous = load_toolchain_manifest(sidecar_root).ok().flatten();
    let dest = toolchain_binary_path(sidecar_root, &manifest);
    install_executable_atomic(&src, &dest)?;
    write_toolchain_manifest(sidecar_root, &manifest)?;
    // A different asset name (glibc build to zipimport script) leaves the old file behind.
    if let Some(previous) = previous.filter(|p| p.binary_name != manifest.binary_name) {
        let _ = fs::remove_file(toolchain_binary_path(sidecar_root, &previous));
    }
    Ok(())
}

/// Ensure an installed toolchain binary can be executed (no-op off Unix).
pub fn mark_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::metadata(path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o755);
        fs::set_permissions(path, perms).map_err(|e| format!("chmod {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn install_plugin_version(
    sidecar_root: &Path,
    version: &str,
    entry: &CatalogVersionEntry,
    source: &dyn SidecarUpdateSource,
) -> Result<(), String> {
    let src = source.materialize(sidecar_root, SidecarComponent::Plugin, version, entry)?;
    let mut manifest = PluginManifest::launch_sites_default();
    manifest.version = version.to_string();
    install_plugin_package(sidecar_root, &src, &manifest)?;
    Ok(())
}

fn apply_component_if_needed(
    sidecar_root: &Path,
    component: SidecarComponent,
    status: &SidecarComponentStatus,
    catalog: &UpdateCatalog,
    state: &mut SidecarRuntimeState,
    source: &dyn SidecarUpdateSource,
) -> Result<Option<SidecarComponent>, String> {
    if !status.update_available {
        return Ok(None);
    }
    let version = status
        .available_version
        .as_deref()
        .ok_or_else(|| format!("{component:?} update available but no target version"))?;
    let entry = catalog_entry(catalog, component, version)
        .ok_or_else(|| format!("catalog missing {component:?} version {version}"))?;

    match component {
        SidecarComponent::Toolchain => snapshot_toolchain_for_rollback(sidecar_root, state)?,
        SidecarComponent::Plugin => snapshot_plugin_for_rollback(sidecar_root, state)?,
    }

    match component {
        SidecarComponent::Toolchain => {
            install_toolchain_version(sidecar_root, version, entry, source)?
        }
        SidecarComponent::Plugin => install_plugin_version(sidecar_root, version, entry, source)?,
    }
    Ok(Some(component))
}

/// Download (materialize) the pending toolchain update without touching the
/// installed sidecar. Returns `(version, local_path)` when an update is due.
///
/// Lets shells run the network transfer outside the Core lock and then call
/// [`apply_sidecar_updates`], whose `materialize` reuses the verified file.
pub fn prefetch_toolchain_update(
    sidecar_root: &Path,
    settings: &SidecarUpdateSettings,
    source: &dyn SidecarUpdateSource,
) -> Result<Option<(String, PathBuf)>, String> {
    let check = check_sidecar_updates(sidecar_root, settings, source)?;
    if !check.toolchain.update_available {
        return match check.toolchain.lookup_error {
            Some(err) => Err(err),
            None => Ok(None),
        };
    }
    let version = check
        .toolchain
        .available_version
        .ok_or_else(|| "toolchain update available but no target version".to_string())?;
    let catalog = source.load_catalog(sidecar_root)?;
    let entry = catalog_entry(&catalog, SidecarComponent::Toolchain, &version)
        .ok_or_else(|| format!("catalog missing toolchain version {version}"))?;
    let path = source.materialize(sidecar_root, SidecarComponent::Toolchain, &version, entry)?;
    Ok(Some((version, path)))
}

/// Apply available updates when idle. When transfers are active, defer instead of swapping.
pub fn apply_sidecar_updates(
    sidecar_root: &Path,
    settings: &SidecarUpdateSettings,
    source: &dyn SidecarUpdateSource,
    transfers_active: bool,
    runtime_state: &mut SidecarRuntimeState,
) -> Result<SidecarApplyResult, String> {
    if transfers_active {
        let check = check_sidecar_updates(sidecar_root, settings, source)?;
        if check.toolchain.update_available || check.plugin.update_available {
            runtime_state.deferred_apply = true;
            save_sidecar_runtime_state(sidecar_root, runtime_state)?;
            return Ok(SidecarApplyResult {
                outcome: SidecarApplyOutcome::BlockedMidTransfer,
                applied: Vec::new(),
            });
        }
        return Ok(SidecarApplyResult {
            outcome: SidecarApplyOutcome::NothingToApply,
            applied: Vec::new(),
        });
    }

    let check = check_sidecar_updates(sidecar_root, settings, source)?;
    if !check.toolchain.update_available && !check.plugin.update_available {
        runtime_state.deferred_apply = false;
        save_sidecar_runtime_state(sidecar_root, runtime_state)?;
        return Ok(SidecarApplyResult {
            outcome: SidecarApplyOutcome::NothingToApply,
            applied: Vec::new(),
        });
    }

    let catalog = source.load_catalog(sidecar_root)?;
    let mut applied = Vec::new();
    if let Some(component) = apply_component_if_needed(
        sidecar_root,
        SidecarComponent::Toolchain,
        &check.toolchain,
        &catalog,
        runtime_state,
        source,
    )? {
        applied.push(component);
    }
    if let Some(component) = apply_component_if_needed(
        sidecar_root,
        SidecarComponent::Plugin,
        &check.plugin,
        &catalog,
        runtime_state,
        source,
    )? {
        applied.push(component);
    }

    runtime_state.deferred_apply = false;
    save_sidecar_runtime_state(sidecar_root, runtime_state)?;

    Ok(SidecarApplyResult {
        outcome: if applied.is_empty() {
            SidecarApplyOutcome::NothingToApply
        } else {
            SidecarApplyOutcome::Applied
        },
        applied,
    })
}

/// On launch: honor deferred apply and optional on-launch check when idle.
pub fn apply_sidecar_updates_on_launch(
    sidecar_root: &Path,
    settings: &SidecarUpdateSettings,
    source: &dyn SidecarUpdateSource,
    transfers_active: bool,
    runtime_state: &mut SidecarRuntimeState,
) -> Result<SidecarApplyResult, String> {
    if transfers_active {
        if runtime_state.deferred_apply || settings.check_on_launch {
            runtime_state.deferred_apply = true;
            save_sidecar_runtime_state(sidecar_root, runtime_state)?;
            return Ok(SidecarApplyResult {
                outcome: SidecarApplyOutcome::DeferredUntilNextLaunch,
                applied: Vec::new(),
            });
        }
        return Ok(SidecarApplyResult {
            outcome: SidecarApplyOutcome::NothingToApply,
            applied: Vec::new(),
        });
    }

    if !runtime_state.deferred_apply && !settings.check_on_launch {
        return Ok(SidecarApplyResult {
            outcome: SidecarApplyOutcome::NothingToApply,
            applied: Vec::new(),
        });
    }

    let mut result = apply_sidecar_updates(sidecar_root, settings, source, false, runtime_state)?;
    if runtime_state.deferred_apply && result.outcome == SidecarApplyOutcome::NothingToApply {
        result.outcome = SidecarApplyOutcome::DeferredUntilNextLaunch;
    }
    Ok(result)
}

fn restore_toolchain_from_snapshot(
    sidecar_root: &Path,
    snapshot: &RollbackSnapshot,
) -> Result<(), String> {
    let manifest_path = snapshot.backup_root.join(TOOLCHAIN_MANIFEST_FILENAME);
    let raw = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read rollback toolchain manifest: {e}"))?;
    let manifest: ToolchainManifest = serde_json::from_str(&raw)
        .map_err(|e| format!("parse rollback toolchain manifest: {e}"))?;
    manifest.validate()?;
    let src = snapshot.backup_root.join(&manifest.binary_name);
    let entry = CatalogVersionEntry {
        asset_name: Some(manifest.binary_name.clone()),
        ..CatalogVersionEntry::local(src)
    };
    install_toolchain_version(
        sidecar_root,
        &snapshot.version,
        &entry,
        &FileSidecarUpdateSource,
    )
}

fn restore_plugin_from_snapshot(
    sidecar_root: &Path,
    snapshot: &RollbackSnapshot,
) -> Result<(), String> {
    let manifest_path = snapshot.backup_root.join(PLUGIN_MANIFEST_FILENAME);
    let raw = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read rollback plugin manifest: {e}"))?;
    let manifest: PluginManifest =
        serde_json::from_str(&raw).map_err(|e| format!("parse rollback plugin manifest: {e}"))?;
    install_plugin_package(
        sidecar_root,
        &snapshot.backup_root.join("package"),
        &manifest,
    )?;
    Ok(())
}

/// Roll back one component to the previous installed version when idle.
pub fn rollback_sidecar_component(
    sidecar_root: &Path,
    component: SidecarComponent,
    transfers_active: bool,
    runtime_state: &mut SidecarRuntimeState,
) -> Result<(), String> {
    if transfers_active {
        return Err("cannot rollback sidecar while a transfer is active".to_string());
    }
    let snapshot = match component {
        SidecarComponent::Toolchain => runtime_state.rollback.toolchain.clone(),
        SidecarComponent::Plugin => runtime_state.rollback.plugin.clone(),
    }
    .ok_or_else(|| format!("no rollback snapshot for {component:?}"))?;

    match component {
        SidecarComponent::Toolchain => restore_toolchain_from_snapshot(sidecar_root, &snapshot)?,
        SidecarComponent::Plugin => restore_plugin_from_snapshot(sidecar_root, &snapshot)?,
    }
    save_sidecar_runtime_state(sidecar_root, runtime_state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn check_reports_update_when_catalog_is_newer() {
        let tmp = tempfile::tempdir().unwrap();
        let sidecar = tmp.path().join("sidecar");
        fs::create_dir_all(&sidecar).unwrap();
        write_toolchain_manifest(&sidecar, &ToolchainManifest::stub("1.0.0")).unwrap();
        let binary = toolchain_binary_path(&sidecar, &ToolchainManifest::stub("1.0.0"));
        fs::write(&binary, b"v1").unwrap();

        let new_binary = tmp.path().join("yt-dlp-2");
        fs::write(&new_binary, b"v2").unwrap();
        let source = FakeSidecarUpdateSource::new(UpdateCatalog {
            toolchain_latest: Some("2.0.0".to_string()),
            toolchain_versions: HashMap::from([(
                "2.0.0".to_string(),
                CatalogVersionEntry::local(new_binary),
            )]),
            ..Default::default()
        });

        let result =
            check_sidecar_updates(&sidecar, &SidecarUpdateSettings::default(), source.as_ref())
                .unwrap();
        assert!(result.toolchain.update_available);
        assert_eq!(result.toolchain.installed_version.as_deref(), Some("1.0.0"));
        assert_eq!(result.toolchain.available_version.as_deref(), Some("2.0.0"));
    }

    #[test]
    fn pin_hides_update_available_when_installed_matches_pin() {
        let tmp = tempfile::tempdir().unwrap();
        let sidecar = tmp.path().join("sidecar");
        write_toolchain_manifest(&sidecar, &ToolchainManifest::stub("1.0.0")).unwrap();
        fs::write(
            toolchain_binary_path(&sidecar, &ToolchainManifest::stub("1.0.0")),
            b"v1",
        )
        .unwrap();

        let newer = tmp.path().join("yt-dlp-2");
        fs::write(&newer, b"v2").unwrap();
        let source = FakeSidecarUpdateSource::new(UpdateCatalog {
            toolchain_latest: Some("2.0.0".to_string()),
            toolchain_versions: HashMap::from([(
                "2.0.0".to_string(),
                CatalogVersionEntry::local(newer),
            )]),
            ..Default::default()
        });

        let settings = SidecarUpdateSettings {
            toolchain_pin: Some("1.0.0".to_string()),
            ..Default::default()
        };
        let result = check_sidecar_updates(&sidecar, &settings, source.as_ref()).unwrap();
        assert!(!result.toolchain.update_available);
        assert_eq!(result.toolchain.pinned_version.as_deref(), Some("1.0.0"));
    }

    fn catalog_with_binary(tmp: &Path, version: &str, bytes: &[u8]) -> (UpdateCatalog, PathBuf) {
        let binary = tmp.join(format!("yt-dlp-{version}"));
        fs::write(&binary, bytes).unwrap();
        (
            UpdateCatalog {
                toolchain_latest: Some(version.to_string()),
                toolchain_versions: HashMap::from([(
                    version.to_string(),
                    CatalogVersionEntry::local(binary.clone()),
                )]),
                ..Default::default()
            },
            binary,
        )
    }

    #[test]
    fn install_replaces_the_binary_without_rewriting_the_running_inode() {
        // H-23: `fs::copy` over the live file truncated it under a running yt-dlp.
        use std::io::Read;
        let tmp = tempfile::tempdir().unwrap();
        let sidecar = tmp.path().join("sidecar");
        write_toolchain_manifest(&sidecar, &ToolchainManifest::stub("1.0.0")).unwrap();
        let live = toolchain_binary_path(&sidecar, &ToolchainManifest::stub("1.0.0"));
        fs::write(&live, b"old binary contents").unwrap();
        let mut running = fs::File::open(&live).unwrap();

        let (catalog, _) = catalog_with_binary(tmp.path(), "2.0.0", b"new");
        let source = FakeSidecarUpdateSource::new(catalog);
        let mut state = SidecarRuntimeState::default();
        apply_sidecar_updates(
            &sidecar,
            &SidecarUpdateSettings::default(),
            source.as_ref(),
            false,
            &mut state,
        )
        .unwrap();

        let mut seen = String::new();
        running.read_to_string(&mut seen).unwrap();
        assert_eq!(
            seen, "old binary contents",
            "the running file was rewritten in place"
        );
        assert_eq!(fs::read(&live).unwrap(), b"new");
    }

    #[test]
    fn install_leaves_no_temp_files_and_sets_the_executable_bit() {
        let tmp = tempfile::tempdir().unwrap();
        let sidecar = tmp.path().join("sidecar");
        let (catalog, _) = catalog_with_binary(tmp.path(), "2.0.0", b"new");
        let source = FakeSidecarUpdateSource::new(catalog);
        let mut state = SidecarRuntimeState::default();
        apply_sidecar_updates(
            &sidecar,
            &SidecarUpdateSettings::default(),
            source.as_ref(),
            false,
            &mut state,
        )
        .unwrap();
        let mut names: Vec<_> = fs::read_dir(&sidecar)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![".phlegethon", "toolchain-manifest.json", "yt-dlp"]
        );
        assert!(crate::toolchain_discovery::is_executable_file(
            &sidecar.join("yt-dlp")
        ));
    }

    #[test]
    fn switching_asset_name_removes_the_previous_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let sidecar = tmp.path().join("sidecar");
        write_toolchain_manifest(&sidecar, &ToolchainManifest::stub("1.0.0")).unwrap();
        fs::write(sidecar.join("yt-dlp"), b"zipimport").unwrap();
        let (mut catalog, _) = catalog_with_binary(tmp.path(), "2.0.0", b"elf");
        catalog
            .toolchain_versions
            .get_mut("2.0.0")
            .unwrap()
            .asset_name = Some("yt-dlp_linux".to_string());
        let source = FakeSidecarUpdateSource::new(catalog);
        apply_sidecar_updates(
            &sidecar,
            &SidecarUpdateSettings::default(),
            source.as_ref(),
            false,
            &mut SidecarRuntimeState::default(),
        )
        .unwrap();
        assert!(sidecar.join("yt-dlp_linux").is_file());
        assert!(!sidecar.join("yt-dlp").exists());
    }

    #[test]
    fn toolchain_manifest_with_a_path_in_binary_name_is_rejected() {
        // M-37
        let tmp = tempfile::tempdir().unwrap();
        for bad in ["..", "../outside", "/etc/passwd", "a/b"] {
            fs::write(
                toolchain_manifest_path(tmp.path()),
                format!(r#"{{"version":"1","binary_name":{bad:?}}}"#),
            )
            .unwrap();
            let err = load_toolchain_manifest(tmp.path()).unwrap_err();
            assert!(err.contains("binary_name"), "{bad}: {err}");
        }
    }

    #[test]
    fn corrupt_runtime_state_resets_to_defaults_and_keeps_the_evidence() {
        // M-38
        let tmp = tempfile::tempdir().unwrap();
        let path = sidecar_state_path(tmp.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ truncated").unwrap();
        assert_eq!(
            load_sidecar_runtime_state(tmp.path()).unwrap(),
            SidecarRuntimeState::default()
        );
        assert!(path.with_extension("json.corrupt").is_file());
        // The next save writes a fresh, valid file.
        save_sidecar_runtime_state(tmp.path(), &SidecarRuntimeState::default()).unwrap();
        assert!(load_sidecar_runtime_state(tmp.path()).is_ok());
    }
}
