//! Real Yt-dlp Extractor Toolchain — subprocess sidecar for Probe-then-Confirm.
//!
//! Wraps yt-dlp as an app-managed subprocess, parses its JSON probe output
//! into [`ProbeResult`], and enforces the hard-fail policy: when the
//! dedicated extractor cannot resolve a URL (falls through to `generic`),
//! the probe returns an error instead of silently reporting success.
//!
//! Supports single-video and multi-item (`_type: "playlist"`) probes for
//! YouTube (playlist / channel / user tabs) and xHamster (video + user/creator
//! lists) where the upstream extractor provides them.
//!
//! ## CI / fixture path
//!
//! The existing [`FakeExtractorToolchain`](crate::FakeExtractorToolchain) in
//! `extractor.rs` serves as the deterministic test seam — no live network.
//! [`FakeYtDlpExtractor`] returns canned yt-dlp JSON through the same parser.
//! This module's unit tests use canned JSON strings.

use crate::cookie_channel::{ytdlp_cookie_args, CookieChannel};
use crate::doh_ytdlp::ytdlp_command_with_optional_doh_dns;
use crate::extractor::{
    is_generic_extractor_key, ExtractionTier, ExtractorDownloadOutcome, ExtractorProgress,
    ExtractorToolchain, MediaFetchKind, MediaItem, ProbePolicy, ProbeResult, Quality,
};
use crate::media_extras::{ffmpeg_available, ytdlp_media_extras_args, MediaExtrasOptions};
use crate::proxy_settings::ProxySettings;
use crate::sidecar_updates::{load_toolchain_manifest, toolchain_binary_path};
use crate::toolchain_discovery::{
    classify_ytdlp_failure, detect_js_runtime, discover_ytdlp, error_code, find_in_path,
    is_executable_file, path_dirs_from_env, python_has_module, run_command_watched,
    run_command_with_deadline_polling, tag_error, tool_version, version_or_unknown,
    ytdlp_js_runtime_args, CommandOutcome, DiscoveryEnv, EjsStatus, JsRuntime, PlatformInfo,
    ToolInfo, ToolchainStatus, YtDlpLauncher, YtDlpSource, ERROR_CODE_TIMEOUT,
};
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the yt-dlp sidecar subprocess.
///
/// # Production defaults
///
/// ```ignore
/// YtDlpConfig {
///     binary_path: PathBuf::from("yt-dlp"),
///     ffmpeg_dir: None,
///     has_js_runtime: false,
///     probe_timeout_secs: 30,
///     plugin_dirs: vec![],  // or with_sidecar_plugins(root)
/// }
/// ```
#[derive(Debug, Clone)]
pub struct YtDlpConfig {
    /// Path to the yt-dlp binary (or bare name on PATH). Used when
    /// [`Self::launcher`] is `None`.
    pub binary_path: PathBuf,
    /// Resolved launcher (standalone binary or `python -m yt_dlp`).
    pub launcher: Option<YtDlpLauncher>,
    /// Where the launcher came from (Toolchain panel).
    pub launcher_source: Option<YtDlpSource>,
    /// Optional directory containing ffmpeg/ffprobe binaries.
    pub ffmpeg_dir: Option<PathBuf>,
    /// Whether a JS runtime (e.g., deno, bun, node) is available for
    /// yt-dlp-ejs. Required for full YouTube format support.
    pub has_js_runtime: bool,
    /// Detected JS runtime, passed to yt-dlp as `--js-runtimes`.
    pub js_runtime: Option<JsRuntime>,
    /// Let yt-dlp fetch `yt-dlp-ejs` from GitHub (`--remote-components ejs:github`).
    pub remote_components: bool,
    /// Ask yt-dlp to impersonate a browser TLS fingerprint when it can.
    pub impersonate: bool,
    /// Timeout for probe operations (seconds). Default 30.
    pub probe_timeout_secs: u64,
    /// Extra directories passed to yt-dlp as `--plugin-dirs`.
    ///
    /// App-managed Extractor Plugins (luxuretv, xxxbp.tv) live under the
    /// sidecar `yt-dlp-plugins/` folder — see [`crate::plugin_sidecar`].
    pub plugin_dirs: Vec<PathBuf>,
}

impl Default for YtDlpConfig {
    fn default() -> Self {
        Self {
            binary_path: PathBuf::from("yt-dlp"),
            launcher: None,
            launcher_source: None,
            ffmpeg_dir: None,
            has_js_runtime: false,
            js_runtime: None,
            remote_components: false,
            impersonate: true,
            probe_timeout_secs: 30,
            plugin_dirs: Vec::new(),
        }
    }
}

impl YtDlpConfig {
    /// Discover yt-dlp, a JS runtime and ffmpeg from the real environment.
    ///
    /// Order: sidecar manifest → `YTDLP_PATH` → `yt-dlp` on PATH →
    /// `python3 -m yt_dlp`. Falls back to the bare `yt-dlp` name so a later
    /// install is picked up without restarting.
    pub fn discover(sidecar_root: Option<&Path>) -> Self {
        let path_dirs = path_dirs_from_env();
        let python_check = |python: &Path| python_has_module(python, "yt_dlp");
        let env = DiscoveryEnv {
            env_ytdlp_path: std::env::var_os("YTDLP_PATH"),
            path_dirs: &path_dirs,
            python_has_ytdlp: &python_check,
        };
        let found = discover_ytdlp(sidecar_root, &env);
        let js_runtime = detect_js_runtime(&path_dirs, &|p| tool_version(p, &["--version"]));
        let mut config = Self {
            binary_path: found
                .as_ref()
                .and_then(|(l, _)| l.binary_path().map(Path::to_path_buf))
                .unwrap_or_else(|| PathBuf::from("yt-dlp")),
            has_js_runtime: js_runtime.as_ref().is_some_and(|rt| rt.supported),
            js_runtime,
            ..Self::default()
        };
        if let Some((launcher, source)) = found {
            config.launcher = Some(launcher);
            config.launcher_source = Some(source);
        }
        if let Some(root) = sidecar_root {
            config = config.with_sidecar_plugins(root);
        }
        config
    }

    /// Attach app-managed Extractor Plugins from a sidecar root, if present.
    ///
    /// When `sidecar_root/yt-dlp-plugins` exists, it is appended to
    /// [`Self::plugin_dirs`] for `--plugin-dirs`.
    pub fn with_sidecar_plugins(mut self, sidecar_root: impl Into<PathBuf>) -> Self {
        if let Some(dirs) = crate::plugin_sidecar::plugin_dirs_path(&sidecar_root.into()) {
            self.plugin_dirs.push(dirs);
        }
        self
    }
}

// ---------------------------------------------------------------------------
// YtDlpExtractorToolchain
// ---------------------------------------------------------------------------

/// The production Extractor Toolchain — invokes yt-dlp as a sidecar process.
///
/// ## Probe flow
///
/// 1. Runs `yt-dlp -J --flat-playlist --no-warnings <url>`.
/// 2. Parses the JSON output (single video **or** playlist with `entries`).
/// 3. Validates that the resolved extractor is the **dedicated** one for
///    the Launch Site (e.g. `Youtube` / `YoutubePlaylist` for youtube.com,
///    `XHamster` / `XHamsterUser` for xhamster).  If the extractor
///    is `Generic` or mismatched, the probe **hard-fails**.
/// 4. Maps formats → [`Quality`] list (or default `best` for flat entries)
///    and produces a [`ProbeResult`].
///
/// ## Hard-fail policy
///
/// A URL that falls through to yt-dlp's `generic` extractor (or any
/// extractor not matching the expected Launch Site key) is treated as
/// **unsupported** — the probe returns `Err`.  This prevents silent
/// "supported" status for sites that yt-dlp cannot actually resolve.
pub struct YtDlpExtractorToolchain {
    config: YtDlpConfig,
    /// Sidecar-resolved yt-dlp binary (overrides [`YtDlpConfig::binary_path`] when set).
    sidecar_binary: Mutex<Option<PathBuf>>,
    /// Extra `--plugin-dirs` from the app-managed sidecar root.
    sidecar_plugin_dirs: Mutex<Vec<PathBuf>>,
    /// Lazily probed `--help` flag support (`--js-runtimes`, `--impersonate`, ...).
    /// Only a successful probe is stored, tagged with the binary it came from.
    supported_flags: Mutex<Option<Cached<Arc<HashSet<String>>>>>,
    /// Lazily probed `--list-impersonate-targets` result, stored like `supported_flags`.
    impersonation_available: Mutex<Option<Cached<bool>>>,
    /// JS runtime found on PATH when the startup config had none.
    detected_js_runtime: Mutex<Option<JsRuntime>>,
    /// Result of a background discovery (M-55). It replaces the startup config's
    /// yt-dlp launcher and JS runtime once published.
    discovered: Mutex<Option<Discovered>>,
}

/// The parts of [`YtDlpConfig`] that discovery fills in by running subprocesses.
#[derive(Debug, Clone)]
struct Discovered {
    launcher: Option<YtDlpLauncher>,
    launcher_source: Option<YtDlpSource>,
    binary_path: PathBuf,
    js_runtime: Option<JsRuntime>,
}

/// Identity of the yt-dlp launcher a cached probe answer belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BinaryFingerprint {
    path: PathBuf,
    modified: Option<SystemTime>,
}

impl BinaryFingerprint {
    fn of(launcher: &YtDlpLauncher) -> Self {
        let path = match launcher {
            YtDlpLauncher::Binary { path } => path.clone(),
            YtDlpLauncher::PythonModule { python } => python.clone(),
        };
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        Self { path, modified }
    }
}

/// A cached probe answer. `binary` is `None` for a value pinned by a test.
struct Cached<T> {
    binary: Option<BinaryFingerprint>,
    value: T,
}

impl<T: Clone> Cached<T> {
    fn get(&self, current: &BinaryFingerprint) -> Option<T> {
        match &self.binary {
            Some(binary) if binary != current => None,
            _ => Some(self.value.clone()),
        }
    }
}

impl YtDlpExtractorToolchain {
    /// Create a new toolchain with the given config.
    pub fn new(config: YtDlpConfig) -> Self {
        Self {
            config,
            sidecar_binary: Mutex::new(None),
            sidecar_plugin_dirs: Mutex::new(Vec::new()),
            supported_flags: Mutex::new(None),
            impersonation_available: Mutex::new(None),
            detected_js_runtime: Mutex::new(None),
            discovered: Mutex::new(None),
        }
    }

    /// Publish the outcome of [`YtDlpConfig::discover`] that ran on another thread.
    ///
    /// Discovery runs `python3 -c "import yt_dlp"` and `--version` probes, so a shell
    /// starts with [`YtDlpConfig::default`] and calls this once the probes finish. Only
    /// the launcher and the JS runtime are taken from `config`; the plugin dirs and
    /// the other options stay as the toolchain was built with.
    pub fn apply_discovered_config(&self, config: YtDlpConfig) {
        if let Ok(mut slot) = self.discovered.lock() {
            *slot = Some(Discovered {
                launcher: config.launcher,
                launcher_source: config.launcher_source,
                binary_path: config.binary_path,
                js_runtime: config.js_runtime,
            });
        }
        if let Ok(mut detected) = self.detected_js_runtime.lock() {
            *detected = None;
        }
        self.clear_capability_caches();
    }

    fn discovered(&self) -> Option<Discovered> {
        self.discovered.lock().ok().and_then(|g| g.clone())
    }

    /// Effective yt-dlp binary path (sidecar override, then config).
    ///
    /// For a Python-module launcher this returns the interpreter path.
    #[cfg(test)]
    fn effective_binary_path(&self) -> PathBuf {
        match self.effective_launcher() {
            YtDlpLauncher::Binary { path } => path,
            YtDlpLauncher::PythonModule { python } => python,
        }
    }

    /// Effective launcher: sidecar manifest binary → configured launcher → `binary_path`.
    pub fn effective_launcher(&self) -> YtDlpLauncher {
        // A sidecar binary that was deleted or lost its execute bit since it was bound
        // must not shadow a working yt-dlp elsewhere.
        if let Some(path) = self
            .sidecar_binary
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .filter(|p| is_executable_file(p))
        {
            return YtDlpLauncher::binary(path);
        }
        match self.discovered() {
            Some(found) => found
                .launcher
                .unwrap_or_else(|| YtDlpLauncher::binary(found.binary_path)),
            None => self
                .config
                .launcher
                .clone()
                .unwrap_or_else(|| YtDlpLauncher::binary(self.config.binary_path.clone())),
        }
    }

    /// Flags the active yt-dlp lists in `--help`.
    ///
    /// A successful answer is cached for this binary (path and mtime). A failed or timed
    /// out `--help`, such as the first start of a cold PyInstaller binary, is not cached:
    /// it yields an empty set now and is retried by the next probe. `--help` runs outside
    /// the cache lock so concurrent probes never queue behind it.
    fn supported_flag_set(&self) -> Arc<HashSet<String>> {
        let launcher = self.effective_launcher();
        let fingerprint = BinaryFingerprint::of(&launcher);
        if let Some(flags) = self
            .supported_flags
            .lock()
            .ok()
            .and_then(|g| g.as_ref().and_then(|c| c.get(&fingerprint)))
        {
            return flags;
        }
        let mut cmd = launcher.command();
        cmd.arg("--help");
        match run_command_with_deadline_polling(
            &mut cmd,
            None,
            Some(Instant::now() + Duration::from_secs(15)),
            Duration::from_millis(20),
        ) {
            Ok(CommandOutcome::Finished { status, stdout, .. }) if status.success() => {
                let help = String::from_utf8_lossy(&stdout);
                let flags: HashSet<String> = help
                    .split(|c: char| c.is_whitespace() || c == ',' || c == '=')
                    .filter(|t| t.starts_with("--"))
                    .map(|t| {
                        t.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                            .to_string()
                    })
                    .collect();
                if flags.is_empty() {
                    return Arc::new(flags);
                }
                let flags = Arc::new(flags);
                if let Ok(mut guard) = self.supported_flags.lock() {
                    *guard = Some(Cached {
                        binary: Some(fingerprint),
                        value: Arc::clone(&flags),
                    });
                }
                flags
            }
            _ => Arc::new(HashSet::new()),
        }
    }

    /// Does the active yt-dlp accept `flag` (from `--help`)?
    fn supports_flag(&self, flag: &str) -> bool {
        self.supported_flag_set().contains(flag)
    }

    /// Test seam: declare which flags the (fake) yt-dlp accepts.
    pub fn set_supported_flags_for_test(&self, flags: &[&str]) {
        if let Ok(mut guard) = self.supported_flags.lock() {
            *guard = Some(Cached {
                binary: None,
                value: Arc::new(flags.iter().map(|f| (*f).to_string()).collect()),
            });
        }
        if let Ok(mut guard) = self.impersonation_available.lock() {
            *guard = Some(Cached {
                binary: None,
                value: flags.contains(&"--impersonate"),
            });
        }
    }

    /// Forget every cached probe answer. Call it when the yt-dlp binary or its
    /// environment may have changed.
    fn clear_capability_caches(&self) {
        if let Ok(mut guard) = self.supported_flags.lock() {
            *guard = None;
        }
        if let Ok(mut guard) = self.impersonation_available.lock() {
            *guard = None;
        }
        if let Ok(mut guard) = self.detected_js_runtime.lock() {
            *guard = None;
        }
    }

    /// Can yt-dlp impersonate a browser (curl_cffi installed)? Cached for a successful
    /// listing only, like [`Self::supported_flag_set`].
    fn impersonation_available(&self) -> bool {
        if !self.supports_flag("--impersonate") {
            return false;
        }
        let launcher = self.effective_launcher();
        let fingerprint = BinaryFingerprint::of(&launcher);
        if let Some(available) = self
            .impersonation_available
            .lock()
            .ok()
            .and_then(|g| g.as_ref().and_then(|c| c.get(&fingerprint)))
        {
            return available;
        }
        let mut cmd = launcher.command();
        cmd.arg("--list-impersonate-targets");
        match run_command_with_deadline_polling(
            &mut cmd,
            None,
            Some(Instant::now() + Duration::from_secs(15)),
            Duration::from_millis(20),
        ) {
            Ok(CommandOutcome::Finished { status, stdout, .. }) if status.success() => {
                let available = chrome_impersonation_listed(&String::from_utf8_lossy(&stdout));
                if let Ok(mut guard) = self.impersonation_available.lock() {
                    *guard = Some(Cached {
                        binary: Some(fingerprint),
                        value: available,
                    });
                }
                available
            }
            _ => false,
        }
    }

    /// The JS runtime to pass to yt-dlp: the startup config's, else one found on PATH
    /// now. `status()` and probes both use this, so the panel and the probe agree.
    fn js_runtime(&self) -> Option<JsRuntime> {
        if let Some(runtime) = &self.config.js_runtime {
            return Some(runtime.clone());
        }
        if let Some(runtime) = self.discovered().and_then(|found| found.js_runtime) {
            return Some(runtime);
        }
        if let Some(runtime) = self.detected_js_runtime.lock().ok().and_then(|g| g.clone()) {
            return Some(runtime);
        }
        let found = detect_js_runtime(&path_dirs_from_env(), &|p| tool_version(p, &["--version"]));
        if let (Some(runtime), Ok(mut guard)) = (&found, self.detected_js_runtime.lock()) {
            *guard = Some(runtime.clone());
        }
        found
    }

    /// Runtime arguments shared by probe and download: JS runtime, remote
    /// components and browser impersonation (each only when supported).
    fn runtime_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        let flags = self.supported_flag_set();
        let js_runtime = self.js_runtime();
        if js_runtime.is_some() && flags.contains("--js-runtimes") {
            args.extend(ytdlp_js_runtime_args(
                js_runtime.as_ref(),
                self.config.remote_components && flags.contains("--remote-components"),
            ));
        } else if self.config.remote_components && flags.contains("--remote-components") {
            args.push("--remote-components".to_string());
            args.push("ejs:github".to_string());
        }
        if self.config.impersonate && self.impersonation_available() {
            args.push("--impersonate".to_string());
            args.push("chrome".to_string());
        }
        args
    }

    /// Current tool versions and gaps for the Toolchain panel.
    pub fn status(&self) -> ToolchainStatus {
        let launcher = self.effective_launcher();
        let path_dirs = path_dirs_from_env();
        let ytdlp_version = {
            let mut cmd = launcher.command();
            cmd.arg("--version");
            match run_command_with_deadline_polling(
                &mut cmd,
                None,
                Some(Instant::now() + Duration::from_secs(15)),
                Duration::from_millis(20),
            ) {
                Ok(CommandOutcome::Finished { status, stdout, .. }) if status.success() => {
                    Some(String::from_utf8_lossy(&stdout).into_owned())
                        .filter(|v| !v.trim().is_empty())
                        .map(|v| version_or_unknown(&v))
                }
                _ => None,
            }
        };
        let ytdlp = ytdlp_version.as_ref().map(|version| ToolInfo {
            path: launcher.describe(),
            version: Some(version.clone()),
        });
        let find_tool = |name: &str| -> Option<ToolInfo> {
            let path = self
                .config
                .ffmpeg_dir
                .as_ref()
                .map(|d| d.join(name))
                .filter(|p| p.is_file())
                .or_else(|| find_in_path(name, &path_dirs))?;
            Some(ToolInfo {
                version: tool_version(&path, &["-version"]),
                path: path.display().to_string(),
            })
        };
        let ejs = match (&launcher, ytdlp.is_some()) {
            (_, false) => EjsStatus::Unknown,
            (YtDlpLauncher::PythonModule { python }, true) => {
                if python_has_module(python, "yt_dlp_ejs") {
                    EjsStatus::PythonPackage
                } else {
                    EjsStatus::Missing
                }
            }
            (YtDlpLauncher::Binary { .. }, true) => {
                // Distro packages depend on python3; a sibling interpreter with
                // the module means the package is there. Standalone builds bundle it.
                match find_in_path("python3", &path_dirs) {
                    Some(python) if python_has_module(&python, "yt_dlp") => {
                        if python_has_module(&python, "yt_dlp_ejs") {
                            EjsStatus::PythonPackage
                        } else {
                            EjsStatus::Missing
                        }
                    }
                    _ => EjsStatus::Bundled,
                }
            }
        };
        ToolchainStatus {
            ytdlp_source: if ytdlp.is_some() {
                self.discovered()
                    .map_or(self.config.launcher_source, |found| found.launcher_source)
                    .or(Some(YtDlpSource::Path))
            } else {
                None
            },
            ytdlp_launcher: Some(launcher),
            ytdlp,
            ffmpeg: find_tool("ffmpeg"),
            ffprobe: find_tool("ffprobe"),
            js_runtime: self.js_runtime(),
            ejs,
            platform: PlatformInfo::current(),
            sidecar_root: None,
            plugin_dirs: self
                .effective_plugin_dirs()
                .iter()
                .map(|d| d.display().to_string())
                .collect(),
            remote_components_enabled: self.config.remote_components,
            impersonation_available: ytdlp_version.is_some() && self.impersonation_available(),
            issues: Vec::new(),
        }
        .with_issues()
    }

    fn effective_plugin_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = self.config.plugin_dirs.clone();
        if let Ok(extra) = self.sidecar_plugin_dirs.lock() {
            for dir in extra.iter() {
                if !dirs.iter().any(|d| d == dir) {
                    dirs.push(dir.clone());
                }
            }
        }
        dirs
    }

    /// Bind toolchain binary + plugin dirs from an installed sidecar root.
    pub fn bind_sidecar_root(&self, root: &Path) {
        // The same health check as discovery: a file without an execute bit would fail
        // every probe with EACCES, and a missing one clears the old binding.
        let healthy = match load_toolchain_manifest(root) {
            Ok(Some(manifest)) => {
                Some(toolchain_binary_path(root, &manifest)).filter(|p| is_executable_file(p))
            }
            _ => None,
        };
        if let Ok(mut slot) = self.sidecar_binary.lock() {
            *slot = healthy;
        }
        self.clear_capability_caches();
        if let Ok(mut dirs) = self.sidecar_plugin_dirs.lock() {
            dirs.clear();
            if let Some(plugin_dir) = crate::plugin_sidecar::plugin_dirs_path(root) {
                dirs.push(plugin_dir);
            }
        }
    }

    #[cfg(test)]
    fn sidecar_binary_for_test(&self) -> Option<PathBuf> {
        self.sidecar_binary.lock().ok().and_then(|g| g.clone())
    }

    #[cfg(test)]
    fn sidecar_plugin_dirs_for_test(&self) -> Vec<PathBuf> {
        self.sidecar_plugin_dirs
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Resolve host to expected extractor key prefixes.
    fn expected_extractor_for_url(&self, url: &str) -> Option<&'static [&'static str]> {
        let parsed = url::Url::parse(url).ok()?;
        let host = parsed.host_str()?;
        crate::launch_sites::find(host).map(|site| site.extractor_keys)
    }

    /// Check whether the resolved extractor key is expected for this URL.
    ///
    /// Returns `Ok(())` if the extractor matches, `Err` with a description
    /// if it falls through to generic or is unexpected.
    fn validate_extractor(
        &self,
        url: &str,
        extractor_key: &str,
        allow_generic: bool,
    ) -> Result<(), String> {
        // If the extractor_key is "Generic" or "GenericAlias", that means
        // yt-dlp had no dedicated extractor — hard-fail unless the caller
        // (Core's non-Launch-Site fallback chain) explicitly allows it.
        if is_generic_extractor_key(extractor_key) && !allow_generic {
            return Err(format!(
                "unsupported Launch Site: yt-dlp resolved '{url}' via generic \
                 extractor — no dedicated extractor available"
            ));
        }

        // If we have a known site, check it matches at least one expected key.
        if let Some(expected) = self.expected_extractor_for_url(url) {
            let matched = expected
                .iter()
                .any(|prefix| extractor_key.starts_with(prefix));
            if !matched {
                return Err(format!(
                    "extractor mismatch for '{url}': \
                     expected one of {:?}, got '{extractor_key}'",
                    expected
                ));
            }
        }

        Ok(())
    }

    /// Build the argv for a yt-dlp probe invocation (test seam).
    ///
    /// `--flat-playlist` lists playlist/channel/user entries without fully
    /// extracting each video. Multi-item probes keep standard quality presets
    /// from flat parse so the probe sheet opens without N+1 yt-dlp calls.
    /// `--plugin-dirs` is included when configured (Launch Site Extractor Plugins).
    pub(crate) fn probe_args(
        &self,
        url: &str,
        cookies: &CookieChannel,
        proxy: &ProxySettings,
    ) -> Vec<String> {
        self.probe_args_for(url, true, cookies, proxy)
    }

    fn probe_args_for(
        &self,
        url: &str,
        flat_playlist: bool,
        cookies: &CookieChannel,
        proxy: &ProxySettings,
    ) -> Vec<String> {
        let mut args = vec!["-J".to_string(), "--no-warnings".to_string()];
        if flat_playlist {
            args.push("--flat-playlist".to_string());
        } else {
            args.push("--no-playlist".to_string());
        }
        args.extend(ytdlp_cookie_args(cookies));
        for dir in self.effective_plugin_dirs() {
            args.push("--plugin-dirs".to_string());
            args.push(dir.display().to_string());
        }
        if let Some(ffmpeg_dir) = &self.config.ffmpeg_dir {
            args.push("--ffmpeg-location".to_string());
            args.push(ffmpeg_dir.display().to_string());
        }
        args.extend(self.runtime_args());
        // `--` ends option parsing so a URL that starts with `-` cannot become an option.
        args.push("--".to_string());
        args.push(url.to_string());
        args
    }

    /// Build the argv for a yt-dlp download invocation (test seam).
    fn download_args(
        &self,
        url: &str,
        format: &str,
        dest_dir: &std::path::Path,
        media_extras: &MediaExtrasOptions,
        cookies: &CookieChannel,
        proxy: &ProxySettings,
        output_template: &str,
        referer: Option<&str>,
    ) -> Vec<String> {
        let out_tmpl = dest_dir.join(output_template);
        let ffmpeg_present = ffmpeg_available(self.config.ffmpeg_dir.as_deref());
        let format_selector = quality_id_to_ytdlp_format(format, ffmpeg_present);

        let mut args = vec![
            "--no-warnings".to_string(),
            "--no-overwrites".to_string(),
            // Resume continues yt-dlp's own `.part` files; say so even if a user config
            // file turns `--no-continue` on.
            "--continue".to_string(),
            "-f".to_string(),
            format_selector,
            "-o".to_string(),
            out_tmpl.display().to_string(),
            "--print".to_string(),
            "after_move:filepath".to_string(),
            "--no-playlist".to_string(),
            // `--print` implies `--quiet`; `--progress` brings the progress lines back,
            // one per update, in a template Core can parse.
            "--progress".to_string(),
            "--newline".to_string(),
            "--progress-template".to_string(),
            PROGRESS_TEMPLATE.to_string(),
        ];
        for arg in ytdlp_media_extras_args(media_extras, ffmpeg_present) {
            args.push(arg);
        }
        args.extend(ytdlp_cookie_args(cookies));
        for dir in self.effective_plugin_dirs() {
            args.push("--plugin-dirs".to_string());
            args.push(dir.display().to_string());
        }
        if let Some(ffmpeg_dir) = &self.config.ffmpeg_dir {
            args.push("--ffmpeg-location".to_string());
            args.push(ffmpeg_dir.display().to_string());
        }
        if let Some(referer) = referer.filter(|r| !r.trim().is_empty()) {
            args.push("--referer".to_string());
            args.push(referer.to_string());
        }
        args.extend(self.runtime_args());
        args.push("--".to_string());
        args.push(url.to_string());
        args
    }

    /// Run yt-dlp with `-J` and return the raw JSON stdout.
    fn run_probe(
        &self,
        url: &str,
        cookies: &CookieChannel,
        proxy: &ProxySettings,
    ) -> Result<String, String> {
        self.run_probe_json(url, true, cookies, proxy, &ProbePolicy::default())
    }

    fn run_probe_json(
        &self,
        url: &str,
        flat_playlist: bool,
        cookies: &CookieChannel,
        proxy: &ProxySettings,
        policy: &ProbePolicy,
    ) -> Result<String, String> {
        let (cookies, _cookie_copy) = cookies.with_private_cookie_file()?;
        let args = self.probe_args_for(url, flat_playlist, &cookies, proxy);
        let launcher = self.effective_launcher();
        let (mut cmd, _doh_guard) =
            ytdlp_command_with_optional_doh_dns(&launcher, &args, url, proxy)?;

        let timeout = if policy.timeout.is_zero() {
            Duration::from_secs(self.config.probe_timeout_secs.max(5))
        } else {
            policy.timeout
        };
        let deadline = Instant::now() + timeout;
        let outcome = run_command_with_deadline_polling(
            &mut cmd,
            policy.cancel.as_deref(),
            Some(deadline),
            Duration::from_millis(50),
        )
        .map_err(|e| {
            // Keep a `[code]` (such as toolchain_missing) at the front.
            if error_code(&e).is_some() {
                e
            } else {
                format!("failed to execute yt-dlp: {e}")
            }
        })?;

        let (status, stdout, stderr) = match outcome {
            CommandOutcome::Finished {
                status,
                stdout,
                stderr,
            } => (status, stdout, stderr),
            CommandOutcome::Stopped => return Err("probe cancelled".to_string()),
            CommandOutcome::TimedOut => {
                return Err(tag_error(
                    ERROR_CODE_TIMEOUT,
                    format!("yt-dlp probe timed out after {}s", timeout.as_secs()),
                ))
            }
        };

        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr);
            let message = format!(
                "yt-dlp probe failed (exit {:?}): {}",
                status.code(),
                stderr.trim()
            );
            return Err(match classify_ytdlp_failure(&stderr) {
                Some(code) => tag_error(code, message),
                None => message,
            });
        }

        // A title with bytes from a legacy encoding must not fail the whole probe.
        let stdout = String::from_utf8_lossy(&stdout).into_owned();

        if stdout.trim().is_empty() {
            return Err("yt-dlp produced empty output for probe".into());
        }

        Ok(stdout)
    }

    /// Test-only alias for `parse_probe_result` — used by integration tests.
    pub fn parse_probe_result_for_test(
        &self,
        json: &str,
        url: &str,
    ) -> Result<ProbeResult, String> {
        self.parse_probe_result(json, url)
    }

    pub(crate) fn parse_probe_result(&self, json: &str, url: &str) -> Result<ProbeResult, String> {
        self.parse_probe_result_with_policy(json, url, false)
    }

    pub(crate) fn parse_probe_result_with_policy(
        &self,
        json: &str,
        url: &str,
        allow_generic: bool,
    ) -> Result<ProbeResult, String> {
        let info: YtDlpVideoInfo =
            serde_json::from_str(json).map_err(|e| format!("failed to parse yt-dlp JSON: {e}"))?;

        // Validate extractor key
        self.validate_extractor(url, &info.extractor_key, allow_generic)?;

        let is_playlist = info
            .type_
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case("playlist"))
            || info.entries.is_some();

        if is_playlist {
            return self.parse_playlist_probe(&info, url);
        }

        // Determine source name (None for single-video probes)
        let source_name = info.playlist_title.clone();

        // Map formats → qualities
        let qualities = self.formats_to_qualities(&info.formats.unwrap_or_default());

        let prefix = item_id_prefix(&info.extractor_key);
        let item = MediaItem {
            id: format!("{prefix}-{}", info.id),
            title: info.title.clone(),
            url: info.original_url.unwrap_or_else(|| url.to_string()),
            qualities,
            archived: false,
            uploader: info.uploader.clone(),
            upload_date: info.upload_date.clone(),
            fetch_kind: MediaFetchKind::Extractor,
            qualities_are_presets: false,
            referer: None,
        };

        Ok(ProbeResult {
            url: url.to_string(),
            items: vec![item],
            source_name,
            tier: tier_for_extractor_key(&info.extractor_key),
            extractor_key: Some(info.extractor_key.clone()),
            warnings: Vec::new(),
        })
    }

    /// Parse a multi-item playlist / channel / user probe result.
    fn parse_playlist_probe(
        &self,
        info: &YtDlpVideoInfo,
        url: &str,
    ) -> Result<ProbeResult, String> {
        let source_name = {
            let from_title = info.title.trim();
            if !from_title.is_empty() {
                Some(from_title.to_string())
            } else if !info.id.is_empty() {
                Some(info.id.clone())
            } else {
                info.playlist_title.clone()
            }
        };

        let prefix = item_id_prefix(&info.extractor_key);
        let mut items: Vec<MediaItem> = Vec::new();
        // Listings can repeat an entry (the live xHamster creator page lists its newest
        // video twice). Duplicate ids break selection counts and confirm, so keep the first.
        let mut seen_ids = HashSet::new();

        for entry in info.entries.iter().flatten().flatten() {
            let Some(item_url) = [&entry.url, &entry.webpage_url, &entry.original_url]
                .into_iter()
                .flatten()
                .find(|s| is_http_url(s))
                .cloned()
            else {
                continue;
            };

            let Some(id) = playlist_entry_id(entry, &item_url) else {
                continue;
            };
            if !seen_ids.insert(id.clone()) {
                continue;
            }
            let title = playlist_entry_title(entry, &id);

            let (qualities, presets) = match &entry.formats {
                Some(formats) if !formats.is_empty() => (self.formats_to_qualities(formats), false),
                _ => (launch_site_standard_qualities(), true),
            };

            items.push(MediaItem {
                id: format!("{prefix}-{id}"),
                title,
                url: item_url,
                qualities,
                archived: false,
                uploader: None,
                upload_date: None,
                fetch_kind: MediaFetchKind::Extractor,
                qualities_are_presets: presets,
                referer: None,
            });
        }

        if items.is_empty() {
            return Err(format!(
                "playlist probe for '{url}' returned no usable entries"
            ));
        }

        Ok(ProbeResult {
            url: url.to_string(),
            items,
            source_name,
            tier: tier_for_extractor_key(&info.extractor_key),
            extractor_key: Some(info.extractor_key.clone()),
            warnings: Vec::new(),
        })
    }

    /// Map yt-dlp format entries to Quality items.
    ///
    /// Strategy:
    /// - Video formats (has height or vcodec != "none"): use resolution
    /// - Audio-only formats: use bitrate label
    /// - Combined audio+video: prefer those
    /// - Deduplicate by quality label if possible
    fn formats_to_qualities(&self, formats: &[YtDlpFormat]) -> Vec<Quality> {
        // Group by resolution/quality label for deduplication
        let mut seen_labels: Vec<String> = Vec::new();
        let mut result: Vec<Quality> = Vec::new();

        let has_video =
            |f: &YtDlpFormat| -> bool { f.vcodec.as_deref().map(|c| c != "none").unwrap_or(true) };
        let has_audio =
            |f: &YtDlpFormat| -> bool { f.acodec.as_deref().map(|c| c != "none").unwrap_or(true) };

        for format in formats {
            let label = self.format_label(format);

            // Same label already kept: prefer mp4, upgrade missing size.
            if let Some(idx) = seen_labels.iter().position(|l| l == &label) {
                let incoming_mp4 = format
                    .ext
                    .as_deref()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"));
                let existing_mp4 = result[idx]
                    .container
                    .as_deref()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"));
                if incoming_mp4 && !existing_mp4 {
                    result[idx] = self.quality_from_format(format, &label);
                } else if result[idx].size_bytes.is_none() {
                    result[idx].size_bytes = usable_format_size(format);
                }
                continue;
            }

            // Determine quality id: use format_id, or a synthetic stable id
            let quality_id = Self::quality_id_for_format(format, has_video, has_audio);

            seen_labels.push(label.clone());
            result.push(Quality {
                id: quality_id,
                label,
                size_bytes: usable_format_size(format),
                container: format.ext.clone(),
            });
        }

        result
    }

    fn quality_id_for_format(
        format: &YtDlpFormat,
        has_video: impl Fn(&YtDlpFormat) -> bool,
        has_audio: impl Fn(&YtDlpFormat) -> bool,
    ) -> String {
        if has_video(format) && has_audio(format) {
            if let Some(h) = format.height {
                format!("{}p", h)
            } else if let Some(height) = format
                .resolution
                .as_deref()
                .and_then(height_from_resolution)
            {
                format!("{height}p")
            } else {
                format.format_id.clone()
            }
        } else if has_video(format) && !has_audio(format) {
            if let Some(h) = format.height {
                format!("{}p-videoonly", h)
            } else {
                format!("{}-videoonly", format.format_id)
            }
        } else if has_audio(format) {
            if let Some(tbr) = format.tbr {
                let kbps = tbr.round() as u64;
                format!("{}k-audio", kbps.max(1))
            } else {
                format!("{}-audio", format.format_id)
            }
        } else {
            format.format_id.clone()
        }
    }

    fn quality_from_format(&self, format: &YtDlpFormat, label: &str) -> Quality {
        let has_video =
            |f: &YtDlpFormat| -> bool { f.vcodec.as_deref().map(|c| c != "none").unwrap_or(true) };
        let has_audio =
            |f: &YtDlpFormat| -> bool { f.acodec.as_deref().map(|c| c != "none").unwrap_or(true) };
        Quality {
            id: Self::quality_id_for_format(format, has_video, has_audio),
            label: label.to_string(),
            size_bytes: usable_format_size(format),
            container: format.ext.clone(),
        }
    }

    /// Build a human-readable label for a format entry.
    fn format_label(&self, format: &YtDlpFormat) -> String {
        let has_video = format
            .vcodec
            .as_deref()
            .map(|c| c != "none")
            .unwrap_or(true);
        let has_audio = format
            .acodec
            .as_deref()
            .map(|c| c != "none")
            .unwrap_or(true);

        let resolution_part = if let Some(h) = format.height {
            if let Some(w) = format.width {
                format!("{}p ({w}×{h})", h)
            } else {
                format!("{}p", h)
            }
        } else if let Some(res) = &format.resolution {
            res.clone()
        } else if let Some(note) = &format.format_note {
            note.clone()
        } else if let Some(tbr) = format.tbr {
            let kbps = tbr.round() as u64;
            format!("{} kb/s", kbps.max(1))
        } else {
            format.format_id.clone()
        };

        let suffix = match (has_video, has_audio) {
            (true, true) => String::new(),
            // Downloaded with the best audio merged in, so not "video only" to the user.
            (true, false) => String::new(),
            (false, true) => " (audio only)".to_string(),
            (false, false) => String::new(),
        };

        format!("{resolution_part}{suffix}")
    }
}

impl ExtractorToolchain for YtDlpExtractorToolchain {
    fn ffmpeg_dir(&self) -> Option<&std::path::Path> {
        self.config.ffmpeg_dir.as_deref()
    }

    fn apply_sidecar_root(&self, root: &Path) {
        self.bind_sidecar_root(root);
    }

    fn apply_discovered_config(&self, config: YtDlpConfig) {
        YtDlpExtractorToolchain::apply_discovered_config(self, config);
    }

    /// Resume runs yt-dlp again in the same Job Destination with the same format and
    /// output template. yt-dlp continues its own `.part` files (`--continue` is its
    /// default) and `--no-overwrites` leaves finished files alone.
    fn supports_extractor_resume(&self) -> bool {
        true
    }

    fn probe(
        &self,
        url: &str,
        cookies: &CookieChannel,
        proxy: &ProxySettings,
    ) -> Result<ProbeResult, String> {
        // Validate URL first
        url::Url::parse(url).map_err(|e| format!("invalid URL '{url}': {e}"))?;

        // Run yt-dlp
        let json = self.run_probe(url, cookies, proxy)?;

        // Parse into ProbeResult — flat playlist entries already carry standard qualities.
        // Skip per-item enrich for multi-item channels so the probe sheet opens quickly.
        self.parse_probe_result(&json, url)
    }

    fn probe_with_policy(
        &self,
        url: &str,
        cookies: &CookieChannel,
        proxy: &ProxySettings,
        policy: &ProbePolicy,
    ) -> Result<ProbeResult, String> {
        url::Url::parse(url).map_err(|e| format!("invalid URL '{url}': {e}"))?;
        let json = self.run_probe_json(url, true, cookies, proxy, policy)?;
        self.parse_probe_result_with_policy(&json, url, policy.allow_generic)
    }

    fn toolchain_status(&self) -> ToolchainStatus {
        self.status()
    }

    fn probe_item_qualities(
        &self,
        item_url: &str,
        cookies: &CookieChannel,
        proxy: &ProxySettings,
    ) -> Result<Vec<Quality>, String> {
        ensure_http_url(item_url)?;
        let policy = ProbePolicy {
            allow_generic: true,
            ..ProbePolicy::default()
        };
        let json = self.run_probe_json(item_url, false, cookies, proxy, &policy)?;
        let result = self.parse_probe_result_with_policy(&json, item_url, true)?;
        result
            .items
            .into_iter()
            .next()
            .map(|item| item.qualities)
            .ok_or_else(|| format!("probe returned no items for {item_url}"))
    }

    fn download(
        &self,
        url: &str,
        format: &str,
        dest_dir: &std::path::Path,
        stop: &AtomicBool,
        media_extras: &MediaExtrasOptions,
        cookies: &CookieChannel,
        proxy: &ProxySettings,
        output_template: &str,
    ) -> Result<ExtractorDownloadOutcome, String> {
        self.download_with_referer(
            url,
            format,
            dest_dir,
            stop,
            media_extras,
            cookies,
            proxy,
            output_template,
            None,
        )
    }

    fn download_with_referer(
        &self,
        url: &str,
        format: &str,
        dest_dir: &std::path::Path,
        stop: &AtomicBool,
        media_extras: &MediaExtrasOptions,
        cookies: &CookieChannel,
        proxy: &ProxySettings,
        output_template: &str,
        referer: Option<&str>,
    ) -> Result<ExtractorDownloadOutcome, String> {
        self.download_with_progress(
            url,
            format,
            dest_dir,
            stop,
            media_extras,
            cookies,
            proxy,
            output_template,
            referer,
            &|_| {},
        )
    }

    fn download_with_progress(
        &self,
        url: &str,
        format: &str,
        dest_dir: &std::path::Path,
        stop: &AtomicBool,
        media_extras: &MediaExtrasOptions,
        cookies: &CookieChannel,
        proxy: &ProxySettings,
        output_template: &str,
        referer: Option<&str>,
        progress: &(dyn Fn(ExtractorProgress) + Send + Sync),
    ) -> Result<ExtractorDownloadOutcome, String> {
        ensure_http_url(url)?;
        std::fs::create_dir_all(dest_dir)
            .map_err(|e| format!("create download dir {}: {e}", dest_dir.display()))?;

        let (cookies, _cookie_copy) = cookies.with_private_cookie_file()?;
        let args = self.download_args(
            url,
            format,
            dest_dir,
            media_extras,
            &cookies,
            proxy,
            output_template,
            referer,
        );

        let launcher = self.effective_launcher();
        let (cmd, _doh_guard) = ytdlp_command_with_optional_doh_dns(&launcher, &args, url, proxy)?;
        let mut tracker = ProgressTracker::default();
        let mut observe = |bytes: &[u8]| {
            if let Some(p) = tracker.feed(bytes) {
                progress(p);
            }
        };
        let result = run_command_interruptible(
            cmd,
            stop,
            DOWNLOAD_STALL_TIMEOUT,
            Duration::from_millis(50),
            &mut observe,
        );

        match result? {
            InterruptibleOutput::Stopped => Ok(ExtractorDownloadOutcome::Stopped),
            InterruptibleOutput::Finished {
                status,
                stdout,
                stderr,
            } => {
                if !status.success() {
                    let stderr = String::from_utf8_lossy(&stderr);
                    let message = format!(
                        "yt-dlp download failed (exit {:?}): {}",
                        status.code(),
                        stderr.trim()
                    );
                    return Err(match classify_ytdlp_failure(&stderr) {
                        Some(code) => tag_error(code, message),
                        None => message,
                    });
                }

                let stdout = String::from_utf8_lossy(&stdout);
                let path_line = stdout
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .last()
                    .ok_or_else(|| "yt-dlp download produced no output path".to_string())?;

                let path = PathBuf::from(path_line);
                if !path.is_file() {
                    return Err(format!(
                        "yt-dlp reported path '{}' but file is missing",
                        path.display()
                    ));
                }
                Ok(ExtractorDownloadOutcome::Completed(path))
            }
        }
    }
}

/// yt-dlp `--progress-template` for downloads. Tab separated: marker, status, bytes
/// downloaded, exact total, estimated total, and the file being written (`NA` when absent).
const PROGRESS_TEMPLATE: &str = "download:PHLEGETHON_PROGRESS\t%(progress.status)s\t\
%(progress.downloaded_bytes)s\t%(progress.total_bytes)s\t%(progress.total_bytes_estimate)s\t\
%(progress.filename)s";
const PROGRESS_MARKER: &str = "PHLEGETHON_PROGRESS\t";

/// Sums yt-dlp's per-file progress lines into one figure for the job.
///
/// yt-dlp downloads the video and the audio of a merge as separate files and restarts
/// its counters for each. Counting per file and summing keeps `downloaded` from passing
/// `total` or going backwards when the second file starts.
#[derive(Default)]
struct ProgressTracker {
    /// Bytes of a line that has not ended yet.
    carry: Vec<u8>,
    /// Per file: (downloaded, total), in the order the files first appeared.
    files: Vec<(String, u64, Option<u64>)>,
    /// The last report, for duplicate suppression.
    last: Option<ExtractorProgress>,
    /// The last report that had a total. A report without one must not reset the share
    /// that later reports are held to.
    peak: Option<ExtractorProgress>,
}

impl ProgressTracker {
    /// Feed stdout bytes; returns the new aggregate when it changed.
    fn feed(&mut self, bytes: &[u8]) -> Option<ExtractorProgress> {
        self.carry.extend_from_slice(bytes);
        let mut changed = false;
        while let Some(end) = self.carry.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.carry.drain(..=end).collect();
            changed |= self.apply_line(&String::from_utf8_lossy(&line));
        }
        if !changed {
            return None;
        }
        let progress = self.monotone(self.aggregate());
        if self.last == Some(progress) {
            return None;
        }
        self.last = Some(progress);
        if progress.total_bytes.is_some() {
            self.peak = Some(progress);
        }
        Some(progress)
    }

    fn apply_line(&mut self, line: &str) -> bool {
        let Some(rest) = line.trim_end().strip_prefix(PROGRESS_MARKER) else {
            return false;
        };
        let fields: Vec<&str> = rest.split('\t').collect();
        let [status, downloaded, total, estimate, filename] = fields[..] else {
            return false;
        };
        let number = |s: &str| {
            s.trim()
                .parse::<f64>()
                .ok()
                .filter(|n| *n >= 0.0)
                .map(|n| n as u64)
        };
        let Some(downloaded) = number(downloaded) else {
            return false;
        };
        let total = number(total)
            .or_else(|| number(estimate))
            .filter(|t| *t > 0);
        let entry = match self
            .files
            .iter_mut()
            .position(|(name, ..)| name == filename)
        {
            Some(i) => &mut self.files[i],
            None => {
                self.files.push((filename.to_string(), 0, None));
                self.files.last_mut().expect("just pushed")
            }
        };
        entry.1 = entry.1.max(downloaded);
        entry.2 = match (entry.2, total) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        if status == "finished" {
            // A finished file is complete even when yt-dlp knew no total.
            entry.2 = Some(entry.2.map_or(entry.1, |t| t.max(entry.1)));
            entry.1 = entry.2.unwrap_or(entry.1);
        }
        true
    }

    fn aggregate(&self) -> ExtractorProgress {
        let downloaded: u64 = self.files.iter().map(|(_, d, _)| d).sum();
        let total = self.files.iter().map(|(_, _, t)| *t).sum::<Option<u64>>();
        ExtractorProgress {
            downloaded_bytes: total.map_or(downloaded, |t| downloaded.min(t)),
            total_bytes: total,
        }
    }

    /// Raise `progress` so its percentage is not below the last share reported with a total.
    ///
    /// The total grows when the second file's size becomes known, so the raw pair can
    /// fall from 1000/1000 (100 %) to 1100/1200 (92 %). The total stays the largest seen
    /// and the downloaded figure is raised to the share the last report had. The
    /// downloaded figure is not allowed past the total.
    fn monotone(&self, progress: ExtractorProgress) -> ExtractorProgress {
        let (Some(peak), Some(total)) = (self.peak, progress.total_bytes) else {
            return progress;
        };
        let Some(peak_total) = peak.total_bytes.filter(|t| *t > 0) else {
            return progress;
        };
        let total = total.max(peak_total);
        // The smallest downloaded figure whose share of `total` is at least `peak`'s share.
        let floor = (u128::from(peak.downloaded_bytes) * u128::from(total))
            .div_ceil(u128::from(peak_total));
        let floor = u64::try_from(floor).unwrap_or(u64::MAX);
        ExtractorProgress {
            downloaded_bytes: progress.downloaded_bytes.max(floor).min(total),
            total_bytes: Some(total),
        }
    }
}

/// Outcome of waiting on a child process that may be hard-killed on stop.
#[derive(Debug)]
enum InterruptibleOutput {
    Finished {
        status: std::process::ExitStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    Stopped,
}

/// Does `yt-dlp --list-impersonate-targets` list a usable Chrome target?
///
/// Without curl_cffi yt-dlp still prints the table, but every row says
/// `(unavailable)`, and passing `--impersonate chrome` then aborts the whole run.
fn chrome_impersonation_listed(listing: &str) -> bool {
    listing.lines().any(|line| {
        let line = line.to_ascii_lowercase();
        line.trim_start().starts_with("chrome") && !line.contains("unavailable")
    })
}

/// A download that prints nothing for this long is treated as hung and killed. yt-dlp
/// prints progress about once a second and retries a dead socket with messages, so only a
/// hang of the process itself is silent this long; a long merge still finishes in time.
const DOWNLOAD_STALL_TIMEOUT: Duration = Duration::from_secs(600);

/// Spawn `cmd`, poll `stop`, and hard-kill the child when stop is requested or `stall`
/// passes without output.
fn run_command_interruptible(
    mut cmd: Command,
    stop: &AtomicBool,
    stall: Duration,
    poll_interval: Duration,
    observer: &mut dyn FnMut(&[u8]),
) -> Result<InterruptibleOutput, String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    match run_command_watched(
        &mut cmd,
        Some(stop),
        None,
        Some(stall),
        poll_interval,
        Some(observer),
    )
    .map_err(|e| e.replace("failed to spawn", "failed to spawn yt-dlp download:"))?
    {
        CommandOutcome::Finished {
            status,
            stdout,
            stderr,
        } => Ok(InterruptibleOutput::Finished {
            status,
            stdout,
            stderr,
        }),
        CommandOutcome::Stopped => Ok(InterruptibleOutput::Stopped),
        // Only the stall limit can time this run out: there is no overall deadline.
        CommandOutcome::TimedOut => Err(tag_error(
            ERROR_CODE_TIMEOUT,
            format!(
                "yt-dlp download stalled: no output for {}s",
                stall.as_secs()
            ),
        )),
    }
}

/// Map a probe quality id to a yt-dlp `-f` selector.
///
/// Video-only qualities (DASH sites such as YouTube) are merged with the best
/// audio track, otherwise the file would have no sound. Merging needs ffmpeg;
/// without it the selector falls back to the best combined format under the height.
fn quality_id_to_ytdlp_format(quality_id: &str, can_merge: bool) -> String {
    match quality_id {
        "best" | "worst" => quality_id.to_string(),
        id if id.ends_with("-videoonly") => {
            let base = id.trim_end_matches("-videoonly");
            let height = base.trim_end_matches('p');
            let numeric = !height.is_empty() && height.chars().all(|c| c.is_ascii_digit());
            match (numeric, can_merge) {
                (true, true) => format!(
                    "bestvideo[height={height}]+bestaudio/bestvideo[height<={height}]+bestaudio\
                     /best[height<={height}]/best"
                ),
                (true, false) => format!("best[height<={height}]/best"),
                (false, true) => format!("{base}+bestaudio/{base}/best"),
                (false, false) => format!("{base}/best"),
            }
        }
        id if id.ends_with("-audio") || id.ends_with("k-audio") => "bestaudio/best".to_string(),
        id if id.ends_with('p') && id.trim_end_matches('p').chars().all(|c| c.is_ascii_digit()) => {
            let height = id.trim_end_matches('p');
            // Prefer HLS/combined streams; progressive xHamster formats are often unusable.
            format!("best[height={height}]/best[height<={height}]/best")
        }
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// yt-dlp JSON schema (deserialization structs)
// ---------------------------------------------------------------------------

/// Top-level yt-dlp JSON output for `-J` / `--dump-single-json`.
///
/// Covers both single-video and playlist (`_type: "playlist"`) responses.
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
struct YtDlpVideoInfo {
    /// YouTube video ID (or site-specific ID / playlist id).
    id: String,
    /// Video title or playlist/channel title.
    #[serde(default, deserialize_with = "null_as_empty")]
    title: String,
    /// Short extractor name (e.g. "youtube").
    #[serde(default, deserialize_with = "null_as_empty")]
    extractor: String,
    /// Capitalised extractor key (e.g. "Youtube", "YoutubePlaylist").
    #[serde(default, deserialize_with = "null_as_empty")]
    extractor_key: String,
    /// yt-dlp result type: `"video"`, `"playlist"`, `"url"`, etc.
    #[serde(default, rename = "_type")]
    type_: Option<String>,
    /// Available formats (may be absent for flat playlist entries).
    #[serde(default)]
    formats: Option<Vec<YtDlpFormat>>,
    /// Playlist / channel / user entries (null slots = deleted / private).
    #[serde(default)]
    entries: Option<Vec<Option<YtDlpEntry>>>,
    /// The webpage URL.
    #[serde(default, deserialize_with = "null_as_empty")]
    webpage_url: String,
    /// The originally requested URL.
    #[serde(default)]
    original_url: Option<String>,
    /// Playlist title if this came from a playlist/ channel.
    #[serde(default)]
    playlist_title: Option<String>,
    /// Playlist index within a playlist.
    #[serde(default, deserialize_with = "lenient_u64")]
    playlist_index: Option<u64>,
    /// Duration in seconds.
    #[serde(default, deserialize_with = "lenient_f64")]
    duration: Option<f64>,
    /// Thumbnail URL.
    #[serde(default)]
    thumbnail: Option<String>,
    /// Description text.
    #[serde(default)]
    description: Option<String>,
    /// Upload date (YYYYMMDD).
    #[serde(default)]
    upload_date: Option<String>,
    /// Uploader name.
    #[serde(default)]
    uploader: Option<String>,
}

/// A single entry inside a yt-dlp playlist JSON (`entries` array).
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
struct YtDlpEntry {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    webpage_url: Option<String>,
    #[serde(default)]
    original_url: Option<String>,
    #[serde(default)]
    formats: Option<Vec<YtDlpFormat>>,
    #[serde(default)]
    ie_key: Option<String>,
    #[serde(default, deserialize_with = "lenient_f64")]
    duration: Option<f64>,
}

/// Stable MediaItem id prefix for a Launch Site extractor family.
/// Tier reported for a yt-dlp result from its `extractor_key`.
fn tier_for_extractor_key(extractor_key: &str) -> ExtractionTier {
    if is_generic_extractor_key(extractor_key) {
        ExtractionTier::GenericExtractor
    } else {
        ExtractionTier::Dedicated
    }
}

fn item_id_prefix(extractor_key: &str) -> &'static str {
    if extractor_key.starts_with("XHamster") {
        "xh"
    } else if extractor_key.starts_with("Youtube") {
        "yt"
    } else if extractor_key.starts_with("LuxureTV") {
        "lux"
    } else if extractor_key.starts_with("XxxbpTV") {
        "xbp"
    } else {
        "item"
    }
}

/// Common resolution presets for Launch Sites when formats are not yet known.
fn launch_site_standard_qualities() -> Vec<Quality> {
    ["2160p", "1080p", "720p", "480p", "360p", "240p", "best"]
        .iter()
        .map(|id| Quality {
            id: (*id).to_string(),
            label: if *id == "best" {
                "Best available".to_string()
            } else {
                format!("{id}")
            },
            size_bytes: None,
            container: None,
        })
        .collect()
}

/// Prefer exact `filesize`, else positive `filesize_approx`; drop zero/absent.
fn usable_format_size(format: &YtDlpFormat) -> Option<u64> {
    format
        .filesize
        .filter(|&n| n > 0)
        .or_else(|| format.filesize_approx.filter(|&n| n > 0))
}

/// Derive a stable entry id when yt-dlp flat-playlist output omits `id`.
/// Height from a yt-dlp `resolution` string such as `1920x1080` (`None` for `audio only`).
fn height_from_resolution(resolution: &str) -> Option<u64> {
    let (width, height) = resolution.split_once('x')?;
    width.trim().parse::<u64>().ok()?;
    height.trim().parse().ok()
}

/// Is `s` an absolute http or https URL? Extractor output is untrusted, so listing entries
/// that fail this check are dropped before they can reach a yt-dlp argument vector.
fn is_http_url(s: &str) -> bool {
    url::Url::parse(s).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
}

fn ensure_http_url(s: &str) -> Result<(), String> {
    if is_http_url(s) {
        Ok(())
    } else {
        Err(format!("refusing to pass non-http(s) URL '{s}' to yt-dlp"))
    }
}

fn playlist_entry_id(entry: &YtDlpEntry, item_url: &str) -> Option<String> {
    if let Some(id) = entry.id.as_deref().filter(|s| !s.is_empty()) {
        return Some(id.to_string());
    }

    if let Ok(parsed) = url::Url::parse(item_url) {
        if let Some(video_id) = parsed
            .query_pairs()
            .find(|(key, _)| key == "v")
            .map(|(_, value)| value.into_owned())
            .filter(|s| !s.is_empty())
        {
            return Some(video_id);
        }

        if let Some(slug) = parsed
            .path_segments()
            .and_then(|segments| segments.filter(|segment| !segment.is_empty()).last())
            .map(|segment| segment.to_string())
            .filter(|s| !s.is_empty())
        {
            return Some(slug);
        }
    }

    None
}

/// Title for a flat playlist entry, falling back to a humanized id/slug.
fn playlist_entry_title(entry: &YtDlpEntry, id: &str) -> String {
    entry
        .title
        .as_deref()
        .filter(|title| !title.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| id.replace('-', " "))
}

/// Number fields that yt-dlp may print as an integer, a float, or `null`. Anything else
/// becomes `None` so one odd field does not fail the whole probe.
fn lenient_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    let value = <serde_json::Value as serde::Deserialize>::deserialize(d)?;
    Ok(match value {
        serde_json::Value::Number(n) => n.as_u64().or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite() && *f >= 0.0)
                .map(|f| f as u64)
        }),
        _ => None,
    })
}

/// Like [`lenient_u64`] for floats that must be finite.
fn lenient_f64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    let value = <serde_json::Value as serde::Deserialize>::deserialize(d)?;
    Ok(value.as_f64().filter(|f| f.is_finite()))
}

/// A string field that yt-dlp may print as `null`; `null` and non-strings become "".
fn null_as_empty<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = <serde_json::Value as serde::Deserialize>::deserialize(d)?;
    Ok(match value {
        serde_json::Value::String(s) => s,
        _ => String::new(),
    })
}

/// A single format entry from yt-dlp's format list.
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
struct YtDlpFormat {
    /// Canonical format identifier.
    format_id: String,
    /// Short note from yt-dlp (e.g. "1080p", "medium").
    #[serde(default)]
    format_note: Option<String>,
    /// File extension.
    #[serde(default)]
    ext: Option<String>,
    /// Video height in pixels (if video stream).
    #[serde(default, deserialize_with = "lenient_u64")]
    height: Option<u64>,
    /// Video width in pixels.
    #[serde(default, deserialize_with = "lenient_u64")]
    width: Option<u64>,
    /// Total bitrate in kbit/s (yt-dlp's `tbr` unit).
    #[serde(default, deserialize_with = "lenient_f64")]
    tbr: Option<f64>,
    /// Video codec name, "none" if no video.
    #[serde(default)]
    vcodec: Option<String>,
    /// Audio codec name, "none" if no audio.
    #[serde(default)]
    acodec: Option<String>,
    /// Resolution string (e.g. "1920x1080").
    #[serde(default)]
    resolution: Option<String>,
    /// Download URL for this format (may be None for some merged formats).
    #[serde(default)]
    url: Option<String>,
    /// Approximate filesize in bytes.
    #[serde(default, deserialize_with = "lenient_u64")]
    filesize: Option<u64>,
    /// Approximate filesize (when not known precisely).
    #[serde(default, deserialize_with = "lenient_u64")]
    filesize_approx: Option<u64>,
    /// Audio sampling rate.
    #[serde(default, deserialize_with = "lenient_u64")]
    asr: Option<u64>,
    /// Frames per second.
    #[serde(default, deserialize_with = "lenient_f64")]
    fps: Option<f64>,
    /// Number of audio channels.
    #[serde(default, deserialize_with = "lenient_u64")]
    audio_channels: Option<u64>,
}

// ---------------------------------------------------------------------------
// Fixture helpers (canned yt-dlp JSON for tests)
// ---------------------------------------------------------------------------

/// Returns a canned yt-dlp JSON response for a YouTube single video.
///
/// Matches what `yt-dlp -J https://www.youtube.com/watch?v=dQw4w9WgXcQ`
/// returns — a typical Rick Astley video with several quality options.
#[cfg(test)]
pub(crate) fn canned_youtube_probe_json() -> &'static str {
    r#"{
  "id": "dQw4w9WgXcQ",
  "title": "Rick Astley - Never Gonna Give You Up (Official Music Video)",
  "extractor": "youtube",
  "extractor_key": "Youtube",
  "webpage_url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
  "original_url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
  "duration": 212.0,
  "thumbnail": "https://i.ytimg.com/vi/dQw4w9WgXcQ/maxresdefault.jpg",
  "uploader": "Rick Astley",
  "upload_date": "20091025",
  "description": "The official video for 'Never Gonna Give You Up'.",
  "formats": [
    {
      "format_id": "18",
      "format_note": "360p",
      "ext": "mp4",
      "height": 360,
      "width": 640,
      "tbr": 250.0,
      "vcodec": "avc1.42001E",
      "acodec": "mp4a.40.2",
      "resolution": "640x360",
      "url": "https://r1.example.com/video18.mp4",
      "filesize": 7510000,
      "fps": 30.0,
      "audio_channels": 2
    },
    {
      "format_id": "22",
      "format_note": "720p",
      "ext": "mp4",
      "height": 720,
      "width": 1280,
      "tbr": 1150.0,
      "vcodec": "avc1.64001F",
      "acodec": "mp4a.40.2",
      "resolution": "1280x720",
      "url": "https://r1.example.com/video22.mp4",
      "filesize": 28100000,
      "fps": 30.0,
      "audio_channels": 2
    },
    {
      "format_id": "137",
      "format_note": "1080p",
      "ext": "mp4",
      "height": 1080,
      "width": 1920,
      "tbr": 4500.0,
      "vcodec": "avc1.640028",
      "acodec": "none",
      "resolution": "1920x1080",
      "url": "https://r1.example.com/video137.mp4",
      "filesize": 87000000,
      "fps": 30.0,
      "audio_channels": null
    },
    {
      "format_id": "140",
      "format_note": "medium",
      "ext": "m4a",
      "height": null,
      "width": null,
      "tbr": 130.0,
      "vcodec": "none",
      "acodec": "mp4a.40.2",
      "resolution": null,
      "url": "https://r1.example.com/audio140.m4a",
      "filesize": 3500000,
      "fps": null,
      "audio_channels": 2
    },
    {
      "format_id": "247",
      "format_note": "720p (60fps)",
      "ext": "webm",
      "height": 720,
      "width": 1280,
      "tbr": 2000.0,
      "vcodec": "vp9",
      "acodec": "none",
      "resolution": "1280x720",
      "url": "https://r1.example.com/video247.webm",
      "filesize": 42000000,
      "fps": 60.0,
      "audio_channels": null
    },
    {
      "format_id": "251",
      "format_note": "medium",
      "ext": "webm",
      "height": null,
      "width": null,
      "tbr": 160.0,
      "vcodec": "none",
      "acodec": "opus",
      "resolution": null,
      "url": "https://r1.example.com/audio251.webm",
      "filesize": 4100000,
      "fps": null,
      "audio_channels": 2
    }
  ]
}"#
}

/// Returns a canned yt-dlp JSON for a URL that falls through to generic extractor.
#[cfg(test)]
pub(crate) fn canned_generic_probe_json() -> &'static str {
    r#"{
  "id": "unknown",
  "title": "Generic Fallback Video",
  "extractor": "generic",
  "extractor_key": "Generic",
  "webpage_url": "https://example.com/video/12345",
  "original_url": "https://example.com/video/12345",
  "formats": [
    {
      "format_id": "default",
      "format_note": "default",
      "ext": "mp4",
      "height": 720,
      "width": 1280,
      "tbr": 1000.0,
      "vcodec": "avc1",
      "acodec": "mp4a",
      "resolution": "1280x720",
      "url": "https://cdn.example.com/video.mp4",
      "filesize": 50000000
    }
  ]
}"#
}

/// Canned yt-dlp JSON as produced by the LuxureTV Extractor Plugin.
#[cfg(test)]
pub(crate) fn canned_luxuretv_probe_json() -> &'static str {
    r#"{
  "id": "16982",
  "title": "Sample LuxureTV Video",
  "extractor": "LuxureTV",
  "extractor_key": "LuxureTV",
  "webpage_url": "https://www.luxuretv.com/videos/sample-video-16982.html",
  "original_url": "https://www.luxuretv.com/videos/sample-video-16982.html",
  "duration": 420.0,
  "formats": [
    {
      "format_id": "480p",
      "format_note": "480p",
      "ext": "mp4",
      "height": 480,
      "width": 854,
      "tbr": 800.0,
      "vcodec": "avc1",
      "acodec": "mp4a",
      "resolution": "854x480",
      "url": "https://cdn.luxuretv.com/videos/16982_480.mp4"
    },
    {
      "format_id": "720p",
      "format_note": "720p",
      "ext": "mp4",
      "height": 720,
      "width": 1280,
      "tbr": 1600.0,
      "vcodec": "avc1",
      "acodec": "mp4a",
      "resolution": "1280x720",
      "url": "https://cdn.luxuretv.com/videos/16982_720.mp4"
    },
    {
      "format_id": "1080p",
      "format_note": "1080p",
      "ext": "mp4",
      "height": 1080,
      "width": 1920,
      "tbr": 3200.0,
      "vcodec": "avc1",
      "acodec": "mp4a",
      "resolution": "1920x1080",
      "url": "https://cdn.luxuretv.com/videos/16982_1080.mp4"
    }
  ]
}"#
}

/// Canned yt-dlp JSON as produced by the XxxbpTV Extractor Plugin.
#[cfg(test)]
pub(crate) fn canned_xxxbp_probe_json() -> &'static str {
    r#"{
  "id": "12345",
  "title": "Sample XXXBP Video",
  "extractor": "XxxbpTV",
  "extractor_key": "XxxbpTV",
  "webpage_url": "https://www.xxxbp.tv/v/12345",
  "original_url": "https://www.xxxbp.tv/v/12345",
  "duration": 300.0,
  "formats": [
    {
      "format_id": "360p",
      "format_note": "360p",
      "ext": "mp4",
      "height": 360,
      "width": 640,
      "tbr": 500.0,
      "vcodec": "avc1",
      "acodec": "mp4a",
      "resolution": "640x360",
      "url": "https://cdn.xxxbp.tv/media/12345_360.mp4"
    },
    {
      "format_id": "720p",
      "format_note": "720p",
      "ext": "mp4",
      "height": 720,
      "width": 1280,
      "tbr": 1500.0,
      "vcodec": "avc1",
      "acodec": "mp4a",
      "resolution": "1280x720",
      "url": "https://cdn.xxxbp.tv/media/12345_720.mp4"
    }
  ]
}"#
}

/// Returns a canned yt-dlp JSON for a YouTube playlist entry (flat).
#[cfg(test)]
pub(crate) fn canned_youtube_playlist_item_json() -> &'static str {
    r#"{
  "id": "abc123def",
  "title": "Playlist Video Title",
  "extractor": "youtube",
  "extractor_key": "Youtube",
  "webpage_url": "https://www.youtube.com/watch?v=abc123def",
  "original_url": "https://www.youtube.com/watch?v=abc123def",
  "playlist_title": "My Awesome Playlist",
  "playlist_index": 1,
  "duration": 180.0,
  "formats": [
    {
      "format_id": "18",
      "format_note": "360p",
      "ext": "mp4",
      "height": 360,
      "width": 640,
      "tbr": 250.0,
      "vcodec": "avc1.42001E",
      "acodec": "mp4a.40.2",
      "resolution": "640x360",
      "url": "https://r1.example.com/playlist_video.mp4",
      "filesize": 6000000,
      "fps": 30.0,
      "audio_channels": 2
    }
  ]
}"#
}

/// Canned yt-dlp `-J --flat-playlist` output for a YouTube playlist URL.
#[cfg(test)]
pub(crate) fn canned_youtube_playlist_json() -> &'static str {
    r#"{
  "_type": "playlist",
  "id": "PLabc123",
  "title": "My Awesome Playlist",
  "extractor": "youtube:playlist",
  "extractor_key": "YoutubePlaylist",
  "webpage_url": "https://www.youtube.com/playlist?list=PLabc123",
  "original_url": "https://www.youtube.com/playlist?list=PLabc123",
  "entries": [
    {
      "_type": "url",
      "id": "vidOne111",
      "title": "First Playlist Video",
      "url": "https://www.youtube.com/watch?v=vidOne111",
      "ie_key": "Youtube",
      "duration": 120.0
    },
    {
      "_type": "url",
      "id": "vidTwo222",
      "title": "Second Playlist Video",
      "url": "https://www.youtube.com/watch?v=vidTwo222",
      "ie_key": "Youtube",
      "duration": 200.0
    },
    null
  ]
}"#
}

/// Canned yt-dlp output for a YouTube channel tab (multi-item).
#[cfg(test)]
pub(crate) fn canned_youtube_channel_json() -> &'static str {
    r#"{
  "_type": "playlist",
  "id": "UCchannel123",
  "title": "Example Channel",
  "extractor": "youtube:tab",
  "extractor_key": "YoutubeTab",
  "webpage_url": "https://www.youtube.com/@ExampleChannel/videos",
  "original_url": "https://www.youtube.com/@ExampleChannel/videos",
  "entries": [
    {
      "_type": "url",
      "id": "chanVid001",
      "title": "Channel Upload One",
      "url": "https://www.youtube.com/watch?v=chanVid001",
      "ie_key": "Youtube"
    },
    {
      "_type": "url",
      "id": "chanVid002",
      "title": "Channel Upload Two",
      "url": "https://www.youtube.com/watch?v=chanVid002",
      "ie_key": "Youtube"
    }
  ]
}"#
}

/// Canned yt-dlp output for a YouTube user URL (multi-item).
#[cfg(test)]
pub(crate) fn canned_youtube_user_json() -> &'static str {
    r#"{
  "_type": "playlist",
  "id": "UCUserUploads",
  "title": "ExampleUser",
  "extractor": "youtube:tab",
  "extractor_key": "YoutubeTab",
  "webpage_url": "https://www.youtube.com/user/ExampleUser",
  "original_url": "https://www.youtube.com/user/ExampleUser",
  "entries": [
    {
      "_type": "url",
      "id": "userVid001",
      "title": "User Video One",
      "url": "https://www.youtube.com/watch?v=userVid001",
      "ie_key": "Youtube"
    }
  ]
}"#
}

/// Canned yt-dlp JSON for an xHamster single video with qualities.
#[cfg(test)]
pub(crate) fn canned_xhamster_probe_json() -> &'static str {
    r#"{
  "id": "xh-video-12345",
  "title": "Example xHamster Video",
  "extractor": "xhamster",
  "extractor_key": "XHamster",
  "webpage_url": "https://xhamster.com/videos/example-xh-video-12345",
  "original_url": "https://xhamster.com/videos/example-xh-video-12345",
  "duration": 600.0,
  "formats": [
    {
      "format_id": "240p",
      "format_note": "240p",
      "ext": "mp4",
      "height": 240,
      "width": 426,
      "tbr": 300.0,
      "vcodec": "avc1",
      "acodec": "mp4a.40.2",
      "resolution": "426x240",
      "url": "https://cdn.xh.example/video_240.mp4"
    },
    {
      "format_id": "720p",
      "format_note": "720p",
      "ext": "mp4",
      "height": 720,
      "width": 1280,
      "tbr": 1500.0,
      "vcodec": "avc1",
      "acodec": "mp4a.40.2",
      "resolution": "1280x720",
      "url": "https://cdn.xh.example/video_720.mp4"
    },
    {
      "format_id": "hls-1080",
      "format_note": "1080p",
      "ext": "mp4",
      "height": 1080,
      "width": 1920,
      "tbr": 4000.0,
      "vcodec": "avc1",
      "acodec": "mp4a.40.2",
      "resolution": "1920x1080",
      "url": "https://cdn.xh.example/hls/1080.m3u8"
    }
  ]
}"#
}

/// Canned yt-dlp flat playlist for an xHamster user/creator list.
#[cfg(test)]
pub(crate) fn canned_xhamster_user_json() -> &'static str {
    r#"{
  "_type": "playlist",
  "id": "example-creator",
  "title": "example-creator",
  "extractor": "xhamster:user",
  "extractor_key": "XHamsterUser",
  "webpage_url": "https://xhamster.com/creators/example-creator",
  "original_url": "https://xhamster.com/creators/example-creator",
  "entries": [
    {
      "_type": "url",
      "id": "xh-item-aaa",
      "title": "Creator Video A",
      "url": "https://xhamster.com/videos/creator-video-a-xh-item-aaa",
      "ie_key": "XHamster"
    },
    {
      "_type": "url",
      "id": "xh-item-bbb",
      "title": "Creator Video B",
      "url": "https://xhamster.com/videos/creator-video-b-xh-item-bbb",
      "ie_key": "XHamster"
    }
  ]
}"#
}

/// Canned yt-dlp flat playlist for xHamster creator pages where entries
/// only include URLs (no `id` / `title`), matching live `--flat-playlist` output.
#[cfg(test)]
pub(crate) fn canned_xhamster_user_flat_urls_only_json() -> &'static str {
    r#"{
  "_type": "playlist",
  "id": "your-eshita",
  "title": "",
  "extractor": "xhamster:user",
  "extractor_key": "XHamsterUser",
  "webpage_url": "https://xhamster.com/creators/your-eshita",
  "original_url": "https://xhamster.com/creators/your-eshita",
  "entries": [
    {
      "_type": "url",
      "url": "https://xhamster.com/videos/creator-video-a-abc123",
      "ie_key": "XHamster"
    },
    {
      "_type": "url",
      "url": "https://xhamster.com/videos/creator-video-b-def456",
      "ie_key": "XHamster"
    }
  ]
}"#
}

// ---------------------------------------------------------------------------
// Fake YtDlpProcess (for tests that don't want to go through FakeExtractorToolchain)
// ---------------------------------------------------------------------------

/// A fake version of YtDlpExtractorToolchain that returns canned JSON
/// without invoking a real subprocess.
///
/// Used in unit tests to verify JSON parsing, extractor key validation,
/// and quality mapping without a live yt-dlp binary.
pub struct FakeYtDlpExtractor {
    /// URL → canned JSON response string
    responses: HashMap<String, String>,
    /// URLs that should simulate a subprocess failure
    failing_urls: Vec<String>,
}

impl FakeYtDlpExtractor {
    pub fn new() -> Self {
        Self {
            responses: HashMap::new(),
            failing_urls: Vec::new(),
        }
    }

    /// Register a canned JSON response for a URL.
    pub fn insert(&mut self, url: &str, json: &str) {
        self.responses.insert(url.to_string(), json.to_string());
    }

    /// Mark a URL as failing (simulates yt-dlp crash / non-zero exit).
    pub fn insert_failure(&mut self, url: &str) {
        self.failing_urls.push(url.to_string());
    }
}

impl ExtractorToolchain for FakeYtDlpExtractor {
    fn probe(
        &self,
        url: &str,
        _cookies: &CookieChannel,
        _proxy: &ProxySettings,
    ) -> Result<ProbeResult, String> {
        // Simulate subprocess failure
        if self.failing_urls.iter().any(|f| f == url) {
            return Err(format!("yt-dlp process error for {url}"));
        }

        let json = self
            .responses
            .get(url)
            .ok_or_else(|| format!("no canned response for {url}"))?;

        let config = YtDlpConfig::default();
        let extractor = YtDlpExtractorToolchain::new(config);
        extractor.parse_probe_result(json, url)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    // ---- Interruptible subprocess hard-kill ----

    #[test]
    fn run_command_interruptible_kills_long_running_child() {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_worker = stop.clone();
        let started = Instant::now();
        let worker = std::thread::spawn(move || {
            let mut cmd = Command::new("sleep");
            cmd.arg("30");
            run_command_interruptible(
                cmd,
                &stop_for_worker,
                Duration::from_secs(60),
                Duration::from_millis(20),
                &mut |_| {},
            )
        });

        std::thread::sleep(Duration::from_millis(80));
        stop.store(true, std::sync::atomic::Ordering::Release);

        let outcome = worker
            .join()
            .expect("worker join")
            .expect("interruptible wait");
        assert!(
            matches!(outcome, InterruptibleOutput::Stopped),
            "expected Stopped after kill, got {outcome:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "hard-kill must stop promptly, elapsed {:?}",
            started.elapsed()
        );
    }

    /// L-22: a download whose process hangs silently used to run forever.
    #[cfg(unix)]
    #[test]
    fn silent_download_is_killed_after_the_stall_limit() {
        let stop = AtomicBool::new(false);
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo started; sleep 30"]);
        let started = Instant::now();
        let err = run_command_interruptible(
            cmd,
            &stop,
            Duration::from_millis(400),
            Duration::from_millis(20),
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(error_code(&err), Some("timeout"), "{err}");
        assert!(err.contains("stalled"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn steady_output_keeps_a_download_alive_past_the_stall_limit() {
        let stop = AtomicBool::new(false);
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "for i in 1 2 3 4 5 6; do echo $i; sleep 0.15; done"]);
        let out = run_command_interruptible(
            cmd,
            &stop,
            Duration::from_millis(400),
            Duration::from_millis(20),
            &mut |_| {},
        )
        .unwrap();
        assert!(
            matches!(out, InterruptibleOutput::Finished { .. }),
            "{out:?}"
        );
    }

    /// L-23: one stray byte in a title must not fail the whole probe.
    #[cfg(unix)]
    #[test]
    fn probe_output_with_invalid_utf8_is_decoded_lossily() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("yt-dlp");
        write_executable(
            &script,
            "#!/bin/sh\ncase \"$*\" in\n  *--help*) echo '  --impersonate T' ;;\n  *) printf '{\"id\":\"a\",\"title\":\"caf\\351\",\"extractor_key\":\"Youtube\",\"extractor\":\"youtube\",\"webpage_url\":\"https://www.youtube.com/watch?v=a\",\"formats\":[{\"format_id\":\"18\",\"height\":360,\"ext\":\"mp4\",\"vcodec\":\"avc1\",\"acodec\":\"mp4a\"}]}' ;;\nesac\n",
        );
        let toolchain = YtDlpExtractorToolchain::new(YtDlpConfig {
            launcher: Some(YtDlpLauncher::Binary { path: script }),
            ..YtDlpConfig::default()
        });
        let result = toolchain
            .probe(
                "https://www.youtube.com/watch?v=a",
                &CookieChannel::default(),
                &ProxySettings::default(),
            )
            .expect("lossy decoding");
        assert!(result.items[0].title.starts_with("caf"));
    }

    /// M-26: a missing yt-dlp is reported with a code the shell can act on.
    #[test]
    fn missing_ytdlp_binary_probe_error_is_tagged() {
        let toolchain = YtDlpExtractorToolchain::new(YtDlpConfig {
            launcher: Some(YtDlpLauncher::Binary {
                path: PathBuf::from("/nonexistent/phlegethon/yt-dlp"),
            }),
            ..YtDlpConfig::default()
        });
        toolchain.set_supported_flags_for_test(&[]);
        let err = toolchain
            .probe(
                "https://www.youtube.com/watch?v=a",
                &CookieChannel::default(),
                &ProxySettings::default(),
            )
            .unwrap_err();
        assert_eq!(error_code(&err), Some("toolchain_missing"), "{err}");
    }

    // ---- YtDlpConfig ----

    #[test]
    fn probe_args_include_js_runtime_and_impersonation_only_when_supported() {
        let rt = JsRuntime {
            kind: crate::toolchain_discovery::JsRuntimeKind::Node,
            path: PathBuf::from("/usr/bin/node"),
            version: Some("22.22.0".into()),
            supported: true,
        };
        let config = YtDlpConfig {
            js_runtime: Some(rt),
            remote_components: true,
            ..YtDlpConfig::default()
        };
        let tc = YtDlpExtractorToolchain::new(config);
        // Old yt-dlp without the flags: nothing extra is passed.
        tc.set_supported_flags_for_test(&["-J"]);
        let args = tc.probe_args(
            "https://example.com/v",
            &CookieChannel::default(),
            &ProxySettings::default(),
        );
        assert!(!args.iter().any(|a| a == "--js-runtimes"), "{args:?}");
        assert!(!args.iter().any(|a| a == "--impersonate"), "{args:?}");
        // Current yt-dlp: runtime, remote components and impersonation.
        tc.set_supported_flags_for_test(&["--js-runtimes", "--remote-components", "--impersonate"]);
        let args = tc.probe_args(
            "https://example.com/v",
            &CookieChannel::default(),
            &ProxySettings::default(),
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["--js-runtimes", "node:/usr/bin/node"]),
            "{args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["--remote-components", "ejs:github"]),
            "{args:?}"
        );
        assert!(
            args.windows(2).any(|w| w == ["--impersonate", "chrome"]),
            "{args:?}"
        );
        assert_eq!(
            args.last().map(String::as_str),
            Some("https://example.com/v")
        );
    }

    #[test]
    fn python_module_launcher_is_used_when_configured() {
        let config = YtDlpConfig {
            launcher: Some(YtDlpLauncher::PythonModule {
                python: PathBuf::from("/usr/bin/python3"),
            }),
            ..YtDlpConfig::default()
        };
        let tc = YtDlpExtractorToolchain::new(config);
        assert_eq!(
            tc.effective_launcher(),
            YtDlpLauncher::PythonModule {
                python: PathBuf::from("/usr/bin/python3")
            }
        );
        let cmd = tc.effective_launcher().command();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["-m", "yt_dlp"]);
        assert_eq!(
            tc.effective_binary_path(),
            PathBuf::from("/usr/bin/python3")
        );
    }

    #[test]
    fn config_defaults_are_sensible() {
        let cfg = YtDlpConfig::default();
        assert_eq!(cfg.binary_path, PathBuf::from("yt-dlp"));
        assert!(!cfg.has_js_runtime);
        assert!(cfg.launcher.is_none());
        assert!(cfg.js_runtime.is_none());
        assert!(!cfg.remote_components);
        assert_eq!(cfg.probe_timeout_secs, 30);
        assert!(cfg.plugin_dirs.is_empty());
    }

    #[test]
    fn bind_sidecar_root_overrides_binary_when_file_present() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let manifest = crate::sidecar_updates::ToolchainManifest::stub("9.9.9");
        crate::sidecar_updates::write_toolchain_manifest(root, &manifest).unwrap();
        let binary = crate::sidecar_updates::toolchain_binary_path(root, &manifest);
        std::fs::write(&binary, b"fake-ytdlp").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert_eq!(extractor.effective_binary_path(), PathBuf::from("yt-dlp"));
        extractor.bind_sidecar_root(root);
        assert_eq!(extractor.sidecar_binary_for_test(), Some(binary.clone()));
        assert_eq!(extractor.effective_binary_path(), binary);
    }

    #[test]
    fn bind_sidecar_root_ignores_missing_binary_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let manifest = crate::sidecar_updates::ToolchainManifest::stub("9.9.9");
        crate::sidecar_updates::write_toolchain_manifest(root, &manifest).unwrap();

        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        extractor.bind_sidecar_root(root);
        assert!(extractor.sidecar_binary_for_test().is_none());
        assert_eq!(extractor.effective_binary_path(), PathBuf::from("yt-dlp"));
    }

    #[test]
    fn bind_sidecar_root_clears_stale_plugin_dirs_on_rebind() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let plugin_dirs = root.join("yt-dlp-plugins");
        std::fs::create_dir_all(&plugin_dirs).unwrap();

        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        extractor.bind_sidecar_root(root);
        extractor.bind_sidecar_root(root);
        assert_eq!(
            extractor.sidecar_plugin_dirs_for_test(),
            vec![plugin_dirs.clone()]
        );
        assert_eq!(extractor.effective_plugin_dirs(), vec![plugin_dirs]);
    }

    #[test]
    fn config_with_sidecar_plugins_attaches_plugin_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = tmp.path().join("yt-dlp-plugins");
        std::fs::create_dir_all(&dirs).unwrap();
        let cfg = YtDlpConfig::default().with_sidecar_plugins(tmp.path());
        assert_eq!(cfg.plugin_dirs, vec![dirs]);
    }

    // ---- Extractor key validation ----

    #[test]
    fn validate_youtube_extractor_passes() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(extractor
            .validate_extractor("https://www.youtube.com/watch?v=abc", "Youtube", false)
            .is_ok());
    }

    #[test]
    fn validate_youtube_extractor_with_playlist_key_passes() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(extractor
            .validate_extractor(
                "https://www.youtube.com/playlist?list=ABC",
                "YoutubePlaylist",
                false
            )
            .is_ok());
    }

    #[test]
    fn validate_generic_extractor_hard_fails() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .validate_extractor("https://example.com/video/12345", "Generic", false)
            .unwrap_err();
        assert!(
            err.contains("generic extractor"),
            "error should mention generic, got: {err}"
        );
    }

    #[test]
    fn validate_generic_alias_extractor_hard_fails() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .validate_extractor("https://unknown.example.com/video", "GenericAlias", false)
            .unwrap_err();
        assert!(
            err.contains("generic extractor"),
            "error should mention generic, got: {err}"
        );
    }

    #[test]
    fn validate_xhamster_extractor_passes() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(extractor
            .validate_extractor(
                "https://xhamster.com/videos/example-12345",
                "XHamster",
                false
            )
            .is_ok());
    }

    #[test]
    fn validate_unknown_extractor_for_known_site_fails() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .validate_extractor(
                "https://www.youtube.com/watch?v=abc",
                "SomeUnknownExtractor",
                false,
            )
            .unwrap_err();
        assert!(
            err.contains("extractor mismatch"),
            "error should mention mismatch, got: {err}"
        );
    }

    #[test]
    fn validate_unknown_site_with_unknown_extractor_passes() {
        // For sites we don't have in our expected map, any non-generic
        // extractor is accepted (it might be a plugin or custom extractor).
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(extractor
            .validate_extractor("https://example.com/video", "CustomExtractor", false)
            .is_ok());
    }

    #[test]
    fn validate_unknown_site_with_generic_fails() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .validate_extractor("https://example.com/video", "Generic", false)
            .unwrap_err();
        assert!(
            err.contains("generic extractor"),
            "error should mention generic, got: {err}"
        );
    }

    #[test]
    fn youtu_be_short_url_expected_youtube() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(extractor
            .validate_extractor("https://youtu.be/abc123", "Youtube", false)
            .is_ok());
    }

    // ---- YouTube JSON parsing ----

    #[test]
    fn parse_youtube_probe_returns_items_and_qualities() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = canned_youtube_probe_json();
        let result = extractor
            .parse_probe_result(json, "https://www.youtube.com/watch?v=dQw4w9WgXcQ")
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "yt-dQw4w9WgXcQ");
        assert_eq!(
            result.items[0].title,
            "Rick Astley - Never Gonna Give You Up (Official Music Video)"
        );
        assert_eq!(
            result.items[0].url,
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
        assert_eq!(result.source_name, None);
    }

    #[test]
    fn parse_youtube_probe_qualities_include_resolution_labels() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = canned_youtube_probe_json();
        let result = extractor
            .parse_probe_result(json, "https://www.youtube.com/watch?v=dQw4w9WgXcQ")
            .unwrap();

        let qualities = &result.items[0].qualities;

        // Should have combined audio+video formats (18=360p, 22=720p)
        let has_360p = qualities.iter().any(|q| q.id == "360p");
        let has_720p = qualities.iter().any(|q| q.id == "720p");
        assert!(has_360p, "should have 360p quality, got: {qualities:?}");
        assert!(has_720p, "should have 720p quality, got: {qualities:?}");

        // 137 (1080p video-only) stays: it is downloaded with the best audio merged in.
        // 247 (720p video-only) has the same label as combined 22 and collapses into it,
        // so the user sees one "720p" choice.
        let has_1080p_videoonly = qualities.iter().any(|q| q.id == "1080p-videoonly");
        assert!(
            has_1080p_videoonly,
            "should have 1080p video-only, got: {qualities:?}"
        );
        assert!(
            !qualities.iter().any(|q| q.id == "720p-videoonly"),
            "720p video-only should collapse into the combined 720p, got: {qualities:?}"
        );
        assert_eq!(
            qualities
                .iter()
                .filter(|q| q.label.starts_with("720p"))
                .count(),
            1,
            "{qualities:?}"
        );

        // Should have audio-only format (140=130k-audio).
        // Format 251 has the same note "medium" so it gets deduplicated.
        let has_audio = qualities.iter().any(|q| q.id.ends_with("-audio"));
        assert!(has_audio, "should have audio quality, got: {qualities:?}");

        // Verify quality labels are human-readable
        let q18 = qualities.iter().find(|q| q.id == "360p").unwrap();
        assert_eq!(q18.label, "360p (640×360)");

        let q137 = qualities
            .iter()
            .find(|q| q.id == "1080p-videoonly")
            .unwrap();
        assert_eq!(q137.label, "1080p (1920×1080)");

        let q140 = qualities.iter().find(|q| q.id == "130k-audio").unwrap();
        assert_eq!(q140.label, "medium (audio only)");
    }

    #[test]
    fn parse_youtube_probe_qualities_deduplicate_by_label() {
        // If two formats share the same label, only one quality should appear
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = canned_youtube_probe_json();
        let result = extractor
            .parse_probe_result(json, "https://www.youtube.com/watch?v=dQw4w9WgXcQ")
            .unwrap();

        let qualities = &result.items[0].qualities;

        // Each quality ID should be unique
        let mut ids: Vec<&str> = qualities.iter().map(|q| q.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(
            ids.len(),
            qualities.len(),
            "quality IDs should be unique, got: {ids:?}"
        );

        // Should have exactly 5 unique qualities from our 6 formats
        // (247 and 22 both resolve to the 720p label and are deduplicated)
        assert!(
            qualities.len() >= 4,
            "should have at least 4 unique quality entries, got {}: {qualities:?}",
            qualities.len()
        );
    }

    #[test]
    fn parse_youtube_probe_source_name_none_for_single_video() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = canned_youtube_probe_json();
        let result = extractor
            .parse_probe_result(json, "https://www.youtube.com/watch?v=dQw4w9WgXcQ")
            .unwrap();
        assert_eq!(result.source_name, None);
    }

    #[test]
    fn parse_playlist_item_includes_source_name() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = canned_youtube_playlist_item_json();
        let result = extractor
            .parse_probe_result(json, "https://www.youtube.com/watch?v=abc123def&list=PLabc")
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.source_name, Some("My Awesome Playlist".to_string()));
    }

    // ---- YouTube multi-item (playlist / channel / user) ----

    #[test]
    fn parse_youtube_playlist_returns_multi_item_probe() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .parse_probe_result(
                canned_youtube_playlist_json(),
                "https://www.youtube.com/playlist?list=PLabc123",
            )
            .unwrap();

        assert_eq!(result.items.len(), 2, "null entries must be skipped");
        assert_eq!(result.source_name, Some("My Awesome Playlist".to_string()));
        assert_eq!(result.items[0].id, "yt-vidOne111");
        assert_eq!(result.items[0].title, "First Playlist Video");
        assert_eq!(
            result.items[0].url,
            "https://www.youtube.com/watch?v=vidOne111"
        );
        assert_eq!(result.items[1].id, "yt-vidTwo222");
        // Flat playlist entries expose a selectable default quality
        assert!(
            result.items[0].qualities.iter().any(|q| q.id == "best"),
            "flat entries need a default quality, got: {:?}",
            result.items[0].qualities
        );
    }

    #[test]
    fn parse_youtube_channel_returns_multi_item_probe() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .parse_probe_result(
                canned_youtube_channel_json(),
                "https://www.youtube.com/@ExampleChannel/videos",
            )
            .unwrap();

        assert_eq!(result.items.len(), 2);
        assert_eq!(result.source_name, Some("Example Channel".to_string()));
        assert_eq!(result.items[0].title, "Channel Upload One");
        assert_eq!(result.items[1].title, "Channel Upload Two");
    }

    #[test]
    fn parse_youtube_user_returns_multi_item_probe() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .parse_probe_result(
                canned_youtube_user_json(),
                "https://www.youtube.com/user/ExampleUser",
            )
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.source_name, Some("ExampleUser".to_string()));
        assert_eq!(result.items[0].id, "yt-userVid001");
    }

    #[test]
    fn validate_youtube_tab_extractor_passes() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(extractor
            .validate_extractor(
                "https://www.youtube.com/@ExampleChannel/videos",
                "YoutubeTab",
                false
            )
            .is_ok());
    }

    // ---- xHamster ----

    #[test]
    fn parse_xhamster_single_video_returns_items_and_qualities() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .parse_probe_result(
                canned_xhamster_probe_json(),
                "https://xhamster.com/videos/example-xh-video-12345",
            )
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.source_name, None);
        assert_eq!(result.items[0].id, "xh-xh-video-12345");
        assert_eq!(result.items[0].title, "Example xHamster Video");
        let qualities = &result.items[0].qualities;
        assert!(
            qualities.iter().any(|q| q.id == "240p"),
            "expected 240p, got: {qualities:?}"
        );
        assert!(
            qualities.iter().any(|q| q.id == "720p"),
            "expected 720p, got: {qualities:?}"
        );
        assert!(
            qualities.iter().any(|q| q.id == "1080p"),
            "expected 1080p, got: {qualities:?}"
        );
    }

    #[test]
    fn parse_xhamster_user_list_returns_multi_item_probe() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .parse_probe_result(
                canned_xhamster_user_json(),
                "https://xhamster.com/creators/example-creator",
            )
            .unwrap();

        assert_eq!(result.items.len(), 2);
        assert_eq!(result.source_name, Some("example-creator".to_string()));
        assert_eq!(result.items[0].id, "xh-xh-item-aaa");
        assert_eq!(result.items[1].title, "Creator Video B");
        assert!(
            result.items[0].qualities.iter().any(|q| q.id == "best"),
            "flat user entries need default quality"
        );
    }

    #[test]
    fn multi_item_flat_playlist_uses_standard_qualities_without_enrich() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .parse_probe_result(
                canned_xhamster_user_json(),
                "https://xhamster.com/creators/example-creator",
            )
            .unwrap();

        assert!(result.items.len() > 1, "expected multi-item channel probe");
        for item in &result.items {
            assert!(
                item.qualities.iter().any(|q| q.id == "1080p"),
                "flat playlist should keep standard qualities without per-item enrich: {:?}",
                item.qualities
            );
        }
    }

    #[test]
    fn parse_xhamster_user_flat_urls_without_ids_returns_multi_item_probe() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .parse_probe_result(
                canned_xhamster_user_flat_urls_only_json(),
                "https://xhamster.com/creators/your-eshita",
            )
            .unwrap();

        assert_eq!(result.items.len(), 2);
        assert_eq!(result.source_name, Some("your-eshita".to_string()));
        assert_eq!(
            result.items[0].id, "xh-creator-video-a-abc123",
            "id should be derived from URL slug"
        );
        assert_eq!(
            result.items[0].title, "creator video a abc123",
            "title should be humanized from slug when missing"
        );
        assert_eq!(
            result.items[1].url,
            "https://xhamster.com/videos/creator-video-b-def456"
        );
    }

    #[test]
    fn validate_xhamster_user_extractor_passes() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(extractor
            .validate_extractor(
                "https://xhamster.com/creators/example-creator",
                "XHamsterUser",
                false
            )
            .is_ok());
    }

    #[test]
    fn parse_generic_extractor_hard_fails() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = canned_generic_probe_json();
        let err = extractor
            .parse_probe_result(json, "https://example.com/video/12345")
            .unwrap_err();
        assert!(
            err.contains("generic extractor") || err.contains("unsupported"),
            "generic probe should hard-fail, got: {err}"
        );
    }

    #[test]
    fn parse_luxuretv_plugin_probe_returns_items_and_qualities() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .parse_probe_result(
                canned_luxuretv_probe_json(),
                "https://www.luxuretv.com/videos/sample-video-16982.html",
            )
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "lux-16982");
        assert_eq!(result.items[0].title, "Sample LuxureTV Video");
        assert_eq!(result.source_name, None);

        let qualities = &result.items[0].qualities;
        assert!(
            qualities.iter().any(|q| q.id == "480p"),
            "expected 480p, got: {qualities:?}"
        );
        assert!(
            qualities.iter().any(|q| q.id == "720p"),
            "expected 720p, got: {qualities:?}"
        );
        assert!(
            qualities.iter().any(|q| q.id == "1080p"),
            "expected 1080p, got: {qualities:?}"
        );
    }

    #[test]
    fn parse_xxxbp_plugin_probe_returns_items_and_qualities() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .parse_probe_result(canned_xxxbp_probe_json(), "https://www.xxxbp.tv/v/12345")
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "xbp-12345");
        assert_eq!(result.items[0].title, "Sample XXXBP Video");

        let qualities = &result.items[0].qualities;
        assert!(
            qualities.iter().any(|q| q.id == "360p"),
            "expected 360p, got: {qualities:?}"
        );
        assert!(
            qualities.iter().any(|q| q.id == "720p"),
            "expected 720p, got: {qualities:?}"
        );
    }

    #[test]
    fn luxuretv_generic_fallback_hard_fails_even_with_plugin_site() {
        // Plugin miss: yt-dlp fell through to Generic — must not count as supported.
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .parse_probe_result(
                canned_generic_probe_json(),
                "https://www.luxuretv.com/videos/sample-video-16982.html",
            )
            .unwrap_err();
        assert!(
            err.contains("generic") || err.contains("unsupported"),
            "plugin miss must hard-fail, got: {err}"
        );
    }

    #[test]
    fn xxxbp_wrong_extractor_hard_fails() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .validate_extractor("https://www.xxxbp.tv/v/12345", "SomeOther", false)
            .unwrap_err();
        assert!(
            err.contains("extractor mismatch"),
            "wrong IE must hard-fail, got: {err}"
        );
    }

    #[test]
    fn probe_args_include_plugin_dirs() {
        let config = YtDlpConfig {
            plugin_dirs: vec![PathBuf::from("/app/sidecar/yt-dlp-plugins")],
            ..YtDlpConfig::default()
        };
        let extractor = YtDlpExtractorToolchain::new(config);
        let args = extractor.probe_args(
            "https://www.luxuretv.com/videos/x-1.html",
            &CookieChannel::default(),
            &ProxySettings::default(),
        );
        assert!(args.contains(&"--plugin-dirs".to_string()));
        assert!(args.contains(&"/app/sidecar/yt-dlp-plugins".to_string()));
        assert_eq!(
            args.last().map(String::as_str),
            Some("https://www.luxuretv.com/videos/x-1.html")
        );
    }

    #[test]
    fn probe_args_omit_plugin_dirs_when_empty() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let args = extractor.probe_args(
            "https://www.youtube.com/watch?v=abc",
            &CookieChannel::default(),
            &ProxySettings::default(),
        );
        assert!(!args.iter().any(|a| a == "--plugin-dirs"));
    }

    #[test]
    fn probe_args_include_cookies_from_browser() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let channel = CookieChannel {
            cookies_from_browser: Some("chrome".to_string()),
            cookie_file: None,
        };
        let args = extractor.probe_args(
            "https://www.youtube.com/watch?v=abc",
            &channel,
            &ProxySettings::default(),
        );
        assert!(args
            .windows(2)
            .any(|w| w == ["--cookies-from-browser", "chrome"]));
    }

    #[test]
    fn probe_args_include_cookie_file() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let channel = CookieChannel {
            cookies_from_browser: None,
            cookie_file: Some(PathBuf::from("C:\\cookies.txt")),
        };
        let args = extractor.probe_args(
            "https://www.youtube.com/watch?v=abc",
            &channel,
            &ProxySettings::default(),
        );
        assert!(args
            .windows(2)
            .any(|w| w == ["--cookies", "C:\\cookies.txt"]));
    }

    #[test]
    fn probe_args_leave_the_proxy_to_the_environment() {
        // M-24: the proxy URL can hold a password; argv is readable by every local user.
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let proxy = ProxySettings {
            url: Some("http://127.0.0.1:8080".to_string()),
        };
        let args = extractor.probe_args(
            "https://www.youtube.com/watch?v=abc",
            &CookieChannel::default(),
            &proxy,
        );
        assert!(!args
            .iter()
            .any(|a| a == "--proxy" || a.contains("127.0.0.1:8080")));
    }

    // ---- FakeYtDlpExtractor ----

    #[test]
    fn fake_ytdlp_extractor_returns_canned_probe() {
        let mut fake = FakeYtDlpExtractor::new();
        fake.insert(
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            canned_youtube_probe_json(),
        );

        let result = fake
            .probe(
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
                &CookieChannel::default(),
                &ProxySettings::default(),
            )
            .unwrap();
        assert_eq!(result.items.len(), 1);
        // 360p, 720p, 1080p (video-only, audio merged on download), audio.
        assert_eq!(result.items[0].qualities.len(), 4);
    }

    #[test]
    fn fake_ytdlp_extractor_fails_for_unknown_url() {
        let fake = FakeYtDlpExtractor::new();
        let err = fake
            .probe(
                "https://www.youtube.com/watch?v=unknown",
                &CookieChannel::default(),
                &ProxySettings::default(),
            )
            .unwrap_err();
        assert!(err.contains("no canned response"), "got: {err}");
    }

    #[test]
    fn fake_ytdlp_extractor_fails_for_configured_failure_url() {
        let mut fake = FakeYtDlpExtractor::new();
        fake.insert_failure("https://www.youtube.com/watch?v=fail");

        let err = fake
            .probe(
                "https://www.youtube.com/watch?v=fail",
                &CookieChannel::default(),
                &ProxySettings::default(),
            )
            .unwrap_err();
        assert!(
            err.contains("process error"),
            "should mention process error, got: {err}"
        );
    }

    // ---- Format label generation ----

    #[test]
    fn format_label_combined_audio_video() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let fmt = YtDlpFormat {
            format_id: "18".into(),
            format_note: Some("360p".into()),
            ext: Some("mp4".into()),
            height: Some(360),
            width: Some(640),
            tbr: Some(250.0),
            vcodec: Some("avc1".into()),
            acodec: Some("mp4a".into()),
            resolution: Some("640x360".into()),
            url: Some("https://example.com/v.mp4".into()),
            filesize: Some(1000),
            filesize_approx: None,
            asr: None,
            fps: Some(30.0),
            audio_channels: Some(2),
        };
        let label = extractor.format_label(&fmt);
        assert_eq!(label, "360p (640×360)");
    }

    #[test]
    fn format_label_video_only() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let fmt = YtDlpFormat {
            format_id: "137".into(),
            format_note: Some("1080p".into()),
            ext: Some("mp4".into()),
            height: Some(1080),
            width: Some(1920),
            tbr: Some(4500.0),
            vcodec: Some("avc1".into()),
            acodec: Some("none".into()),
            resolution: Some("1920x1080".into()),
            url: Some("https://example.com/v.mp4".into()),
            filesize: Some(87000000),
            filesize_approx: None,
            asr: None,
            fps: Some(30.0),
            audio_channels: None,
        };
        let label = extractor.format_label(&fmt);
        assert_eq!(label, "1080p (1920×1080)");
    }

    #[test]
    fn format_label_audio_only() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let fmt = YtDlpFormat {
            format_id: "140".into(),
            format_note: Some("medium".into()),
            ext: Some("m4a".into()),
            height: None,
            width: None,
            tbr: Some(130.0),
            vcodec: Some("none".into()),
            acodec: Some("mp4a".into()),
            resolution: None,
            url: Some("https://example.com/a.m4a".into()),
            filesize: Some(3500000),
            filesize_approx: None,
            asr: None,
            fps: None,
            audio_channels: Some(2),
        };
        let label = extractor.format_label(&fmt);
        assert_eq!(label, "medium (audio only)");
    }

    #[test]
    fn format_label_unknown_codec_falls_back_to_format_id() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let fmt = YtDlpFormat {
            format_id: "999".into(),
            format_note: None,
            ext: None,
            height: None,
            width: None,
            tbr: None,
            vcodec: None,
            acodec: None,
            resolution: None,
            url: None,
            filesize: None,
            filesize_approx: None,
            asr: None,
            fps: None,
            audio_channels: None,
        };
        let label = extractor.format_label(&fmt);
        // Both vcodec and acodec are None -> treated as having video and audio
        // No height/width -> falls back to format_note (None) -> tbr (None) -> format_id
        assert_eq!(label, "999");
    }

    // ---- Edge cases ----

    #[test]
    fn quality_id_maps_to_ytdlp_height_selector() {
        assert_eq!(
            quality_id_to_ytdlp_format("720p", true),
            "best[height=720]/best[height<=720]/best"
        );
        assert_eq!(quality_id_to_ytdlp_format("best", true), "best");
    }

    #[test]
    fn video_only_quality_merges_best_audio_when_ffmpeg_is_present() {
        let merged = quality_id_to_ytdlp_format("720p-videoonly", true);
        assert_eq!(
            merged,
            "bestvideo[height=720]+bestaudio/bestvideo[height<=720]+bestaudio/best[height<=720]/best"
        );
        // Without ffmpeg nothing can be merged: take a combined format instead.
        assert_eq!(
            quality_id_to_ytdlp_format("720p-videoonly", false),
            "best[height<=720]/best"
        );
        assert_eq!(
            quality_id_to_ytdlp_format("hls-1-videoonly", true),
            "hls-1+bestaudio/hls-1/best"
        );
        assert_eq!(
            quality_id_to_ytdlp_format("hls-1-videoonly", false),
            "hls-1/best"
        );
    }

    #[test]
    fn formats_to_qualities_empty_list() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let qualities = extractor.formats_to_qualities(&[]);
        assert!(qualities.is_empty());
    }

    #[test]
    fn formats_to_qualities_carries_filesize_as_size_bytes() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let formats = vec![YtDlpFormat {
            format_id: "18".into(),
            format_note: Some("360p".into()),
            ext: Some("mp4".into()),
            height: Some(360),
            width: Some(640),
            tbr: Some(250.0),
            vcodec: Some("avc1".into()),
            acodec: Some("mp4a".into()),
            resolution: Some("640x360".into()),
            url: Some("https://example.com/v.mp4".into()),
            filesize: Some(7_510_000),
            filesize_approx: None,
            asr: None,
            fps: Some(30.0),
            audio_channels: Some(2),
        }];
        let qualities = extractor.formats_to_qualities(&formats);
        assert_eq!(qualities.len(), 1);
        assert_eq!(qualities[0].id, "360p");
        assert_eq!(qualities[0].size_bytes, Some(7_510_000));
    }

    #[test]
    fn formats_to_qualities_falls_back_to_filesize_approx() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let formats = vec![YtDlpFormat {
            format_id: "22".into(),
            format_note: Some("720p".into()),
            ext: Some("mp4".into()),
            height: Some(720),
            width: Some(1280),
            tbr: Some(1000.0),
            vcodec: Some("avc1".into()),
            acodec: Some("mp4a".into()),
            resolution: Some("1280x720".into()),
            url: Some("https://example.com/v.mp4".into()),
            filesize: None,
            filesize_approx: Some(42_000_000),
            asr: None,
            fps: Some(30.0),
            audio_channels: Some(2),
        }];
        let qualities = extractor.formats_to_qualities(&formats);
        assert_eq!(qualities.len(), 1);
        assert_eq!(qualities[0].size_bytes, Some(42_000_000));
    }

    #[test]
    fn formats_to_qualities_ignores_zero_size() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let formats = vec![YtDlpFormat {
            format_id: "18".into(),
            format_note: Some("360p".into()),
            ext: Some("mp4".into()),
            height: Some(360),
            width: Some(640),
            tbr: Some(250.0),
            vcodec: Some("avc1".into()),
            acodec: Some("mp4a".into()),
            resolution: Some("640x360".into()),
            url: Some("https://example.com/v.mp4".into()),
            filesize: Some(0),
            filesize_approx: Some(0),
            asr: None,
            fps: Some(30.0),
            audio_channels: Some(2),
        }];
        let qualities = extractor.formats_to_qualities(&formats);
        assert_eq!(qualities.len(), 1);
        assert_eq!(qualities[0].size_bytes, None);
    }

    #[test]
    fn formats_to_qualities_deduplicates_duplicate_labels() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let formats = vec![
            YtDlpFormat {
                format_id: "a".into(),
                format_note: None,
                ext: Some("mp4".into()),
                height: Some(720),
                width: Some(1280),
                tbr: Some(1000.0),
                vcodec: Some("avc1".into()),
                acodec: Some("mp4a".into()),
                resolution: Some("1280x720".into()),
                url: Some("https://example.com/a.mp4".into()),
                filesize: Some(1000),
                filesize_approx: None,
                asr: None,
                fps: Some(30.0),
                audio_channels: Some(2),
            },
            YtDlpFormat {
                format_id: "b".into(),
                format_note: None,
                ext: Some("webm".into()),
                height: Some(720),
                width: Some(1280),
                tbr: Some(2000.0),
                vcodec: Some("vp9".into()),
                acodec: Some("opus".into()),
                resolution: Some("1280x720".into()),
                url: Some("https://example.com/b.webm".into()),
                filesize: Some(2000),
                filesize_approx: None,
                asr: None,
                fps: Some(60.0),
                audio_channels: Some(2),
            },
        ];
        let qualities = extractor.formats_to_qualities(&formats);
        // Both have the same label "720p (1280\u00d7720)" -> deduplicated to one entry
        assert_eq!(
            qualities.len(),
            1,
            "duplicate labels should deduplicate to 1, got {n}: {qualities:?}",
            n = qualities.len()
        );
        assert_eq!(qualities[0].id, "720p");
        assert_eq!(qualities[0].label, "720p (1280×720)");
    }

    #[test]
    fn formats_to_qualities_upgrades_size_on_duplicate_label() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let formats = vec![
            YtDlpFormat {
                format_id: "a".into(),
                format_note: None,
                ext: Some("mp4".into()),
                height: Some(720),
                width: Some(1280),
                tbr: Some(1000.0),
                vcodec: Some("avc1".into()),
                acodec: Some("mp4a".into()),
                resolution: Some("1280x720".into()),
                url: Some("https://example.com/a.mp4".into()),
                filesize: None,
                filesize_approx: None,
                asr: None,
                fps: Some(30.0),
                audio_channels: Some(2),
            },
            YtDlpFormat {
                format_id: "b".into(),
                format_note: None,
                ext: Some("webm".into()),
                height: Some(720),
                width: Some(1280),
                tbr: Some(2000.0),
                vcodec: Some("vp9".into()),
                acodec: Some("opus".into()),
                resolution: Some("1280x720".into()),
                url: Some("https://example.com/b.webm".into()),
                filesize: None,
                filesize_approx: Some(55_000_000),
                asr: None,
                fps: Some(60.0),
                audio_channels: Some(2),
            },
        ];
        let qualities = extractor.formats_to_qualities(&formats);
        assert_eq!(qualities.len(), 1);
        assert_eq!(qualities[0].size_bytes, Some(55_000_000));
    }

    #[test]
    fn invalid_url_returns_error() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .probe(
                "not-a-valid-url",
                &CookieChannel::default(),
                &ProxySettings::default(),
            )
            .unwrap_err();
        assert!(
            err.contains("invalid URL"),
            "should mention invalid URL, got: {err}"
        );
    }

    #[test]
    fn invalid_json_returns_parse_error() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .parse_probe_result("{invalid json}", "https://youtube.com/watch?v=abc")
            .unwrap_err();
        assert!(
            err.contains("failed to parse"),
            "should mention parse error, got: {err}"
        );
    }

    #[test]
    fn probe_on_unknown_site_with_generic_json_hard_fails() {
        // Even with a valid site we don't track, if the extractor is Generic
        // the probe should still fail.
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let err = extractor
            .parse_probe_result(
                canned_generic_probe_json(),
                "https://some-unknown-site.com/video/1",
            )
            .unwrap_err();
        assert!(
            err.contains("generic"),
            "generic extractor should always hard-fail, got: {err}"
        );
    }

    // ---- Expected extractor URL matching ----

    #[test]
    fn expected_extractor_for_youtube_com() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let keys = extractor
            .expected_extractor_for_url("https://www.youtube.com/watch?v=abc")
            .unwrap();
        assert!(keys.contains(&"Youtube"));
    }

    #[test]
    fn expected_extractor_for_youtu_be() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let keys = extractor
            .expected_extractor_for_url("https://youtu.be/abc")
            .unwrap();
        assert!(keys.contains(&"Youtube"));
    }

    #[test]
    fn expected_extractor_for_xhamster() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let keys = extractor
            .expected_extractor_for_url("https://xhamster.com/videos/example-12345")
            .unwrap();
        assert!(keys.contains(&"XHamster"));
    }

    #[test]
    fn expected_extractor_for_luxuretv_is_plugin() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let keys = extractor
            .expected_extractor_for_url("https://www.luxuretv.com/videos/sample-16982.html")
            .expect("luxuretv should map to plugin IE");
        assert!(keys.contains(&"LuxureTV"));
    }

    #[test]
    fn expected_extractor_for_xxxbp_tv_is_plugin() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let keys = extractor
            .expected_extractor_for_url("https://www.xxxbp.tv/v/12345")
            .expect("xxxbp.tv should map to plugin IE");
        assert!(keys.contains(&"XxxbpTV"));
    }

    #[test]
    fn validate_luxuretv_plugin_extractor_passes() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(extractor
            .validate_extractor(
                "https://en.luxuretv.com/videos/sample-16982.html",
                "LuxureTV",
                false
            )
            .is_ok());
    }

    #[test]
    fn validate_xxxbp_plugin_extractor_passes() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(extractor
            .validate_extractor("https://xxxbp.tv/v/12345", "XxxbpTV", false)
            .is_ok());
    }

    #[test]
    fn expected_extractor_for_unknown_site_returns_none() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let keys = extractor.expected_extractor_for_url("https://example.com/file.zip");
        assert!(keys.is_none());
    }

    #[test]
    fn expected_extractor_for_youtube_subdomain() {
        // Subdomains like music.youtube.com should still match
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let keys =
            extractor.expected_extractor_for_url("https://music.youtube.com/playlist?list=ABC");
        assert!(
            keys.is_some(),
            "music.youtube.com should match youtube.com extractors"
        );
        let keys = keys.unwrap();
        assert!(keys.contains(&"Youtube"));
    }

    #[test]
    fn doh_override_map_detects_xhamster_sinkhole_when_dns_diverges() {
        let host = "xhamster.com";
        let Ok(doh_ips) = crate::doh_dns::resolve_a_records(host) else {
            // Offline CI — skip.
            return;
        };
        let diverges = crate::doh_dns::system_dns_diverges_from_doh(host, &doh_ips);
        let map =
            crate::doh_proxy::doh_override_map_for_url("https://xhamster.com/creators/your-eshita")
                .expect("DoH map lookup");
        if diverges {
            let map = map.expect("expected DoH override when system DNS diverges");
            assert!(map.contains_key("xhamster.com"));
        } else {
            assert!(map.is_none(), "no override when DNS agrees");
        }
    }

    /// Live: creator channel must return multiple items when yt-dlp is on PATH.
    /// DoH CONNECT proxy unblocks ISP sinkhole DNS (browser works, bare yt-dlp RST).
    #[test]
    #[ignore = "live network + yt-dlp"]
    fn live_xhamster_creator_probe_lists_videos() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let result = extractor
            .probe(
                "https://xhamster.com/creators/your-eshita",
                &CookieChannel::default(),
                &ProxySettings::default(),
            )
            .expect("probe should succeed via DoH override when needed");
        assert!(
            result.items.len() > 1,
            "creator channel should list multiple videos, got {}",
            result.items.len()
        );
        assert!(result
            .items
            .iter()
            .all(|i| !i.url.is_empty() && !i.qualities.is_empty()));
    }

    /// Regression: the probe used to fail with "missing stdout pipe" on every real
    /// yt-dlp because the process runner did not capture output itself. This goes
    /// through an actual child process rather than the in-memory fake.
    #[cfg(unix)]
    #[test]
    fn probe_runs_a_real_child_process_and_parses_its_json() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("yt-dlp");
        std::fs::write(
            &script,
            r#"#!/bin/sh
case "$*" in
  *--help*) echo "  --impersonate TARGET   --js-runtimes RUNTIME" ;;
  *--list-impersonate-targets*) echo "no impersonate targets" ;;
  *--version*) echo "2099.01.01" ;;
  *) cat <<'JSON'
{"_type":"video","id":"abc123","title":"Real Process Video","extractor_key":"Youtube","extractor":"youtube",
 "webpage_url":"https://www.youtube.com/watch?v=abc123","duration":12,
 "formats":[{"format_id":"18","height":360,"ext":"mp4","vcodec":"avc1","acodec":"mp4a"},
            {"format_id":"22","height":720,"ext":"mp4","vcodec":"avc1","acodec":"mp4a"}]}
JSON
  ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let toolchain = YtDlpExtractorToolchain::new(YtDlpConfig {
            binary_path: script.clone(),
            launcher: Some(YtDlpLauncher::Binary { path: script }),
            ..YtDlpConfig::default()
        });
        let result = toolchain
            .probe(
                "https://www.youtube.com/watch?v=abc123",
                &CookieChannel::default(),
                &ProxySettings::default(),
            )
            .expect("probe through a real process succeeds");
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].title, "Real Process Video");
        assert!(
            result.items[0].qualities.len() >= 2,
            "{:?}",
            result.items[0].qualities
        );
        assert_eq!(result.extractor_key.as_deref(), Some("Youtube"));
    }

    #[test]
    fn impersonation_listing_distinguishes_available_from_unavailable() {
        let with_curl_cffi = "[info] Available impersonate targets\n\
            Client          OS           Source\n\
            --------------------------------------\n\
            Chrome-133      Macos-15     curl_cffi\n\
            Safari-17.2     Ios-17.2     curl_cffi\n";
        let without = "[info] Available impersonate targets\n\
            Client    OS   Source\n\
            --------------------------------------------\n\
            Tor       -    curl_cffi>=0.11 (unavailable)\n\
            Chrome    -    curl_cffi (unavailable)\n";
        assert!(chrome_impersonation_listed(with_curl_cffi));
        assert!(!chrome_impersonation_listed(without));
        assert!(!chrome_impersonation_listed(""));
        assert!(!chrome_impersonation_listed("Safari  -  curl_cffi\n"));
    }

    #[test]
    fn playlist_entries_repeated_by_the_site_become_one_item() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = r#"{"_type":"playlist","id":"your-eshita","title":"your-eshita",
            "extractor_key":"XHamsterUser","webpage_url":"https://xhamster.com/creators/your-eshita",
            "entries":[
              {"_type":"url","ie_key":"XHamster","url":"https://xhamster.com/videos/first-video-xhEt2mN"},
              {"_type":"url","ie_key":"XHamster","url":"https://xhamster.com/videos/first-video-xhEt2mN"},
              {"_type":"url","ie_key":"XHamster","url":"https://xhamster.com/videos/second-video-xhAb1cD"}
            ]}"#;
        let result = extractor
            .parse_probe_result(json, "https://xhamster.com/creators/your-eshita")
            .unwrap();
        let ids: Vec<&str> = result.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(result.items.len(), 2, "{ids:?}");
        let unique: HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len());
    }

    #[test]
    fn probe_args_put_double_dash_before_the_url() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        for url in ["https://example.com/v", "--exec=touch /tmp/pwned"] {
            let args =
                extractor.probe_args(url, &CookieChannel::default(), &ProxySettings::default());
            let n = args.len();
            assert_eq!(args[n - 2], "--", "{args:?}");
            assert_eq!(args[n - 1], url, "{args:?}");
        }
    }

    #[test]
    fn download_args_put_double_dash_before_the_url() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let args = extractor.download_args(
            "https://example.com/v",
            "best",
            Path::new("/tmp/dest"),
            &MediaExtrasOptions::default(),
            &CookieChannel::default(),
            &ProxySettings::default(),
            "%(title)s.%(ext)s",
            Some("https://example.com/"),
        );
        let n = args.len();
        assert_eq!(args[n - 2], "--", "{args:?}");
        assert_eq!(args[n - 1], "https://example.com/v", "{args:?}");
        assert_eq!(args.iter().filter(|a| a.as_str() == "--").count(), 1);
        assert!(args.iter().any(|a| a == "--continue"), "{args:?}");
    }

    #[test]
    fn download_refuses_a_url_that_is_not_http() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let stop = AtomicBool::new(false);
        let tmp = tempfile::tempdir().unwrap();
        let err = extractor
            .download(
                "--exec=touch /tmp/pwned",
                "best",
                tmp.path(),
                &stop,
                &MediaExtrasOptions::default(),
                &CookieChannel::default(),
                &ProxySettings::default(),
                "%(title)s.%(ext)s",
            )
            .unwrap_err();
        assert!(err.contains("non-http"), "{err}");
        let err = extractor
            .probe_item_qualities(
                "--plugin-dirs=/evil",
                &CookieChannel::default(),
                &ProxySettings::default(),
            )
            .unwrap_err();
        assert!(err.contains("non-http"), "{err}");
    }

    #[test]
    fn playlist_entries_with_non_http_urls_are_dropped() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = r#"{"_type":"playlist","id":"p","title":"p",
            "extractor_key":"XHamsterUser","webpage_url":"https://xhamster.com/creators/p",
            "entries":[
              {"_type":"url","id":"evil1","url":"--exec=sh -c 'touch /tmp/pwned'"},
              {"_type":"url","id":"evil2","url":"file:///etc/passwd"},
              {"_type":"url","id":"mixed","url":"--plugin-dirs=/evil",
               "webpage_url":"https://xhamster.com/videos/mixed-xh1"},
              {"_type":"url","id":"ok","url":"https://xhamster.com/videos/ok-xh2"}
            ]}"#;
        let result = extractor
            .parse_probe_result(json, "https://xhamster.com/creators/p")
            .unwrap();
        let urls: Vec<&str> = result.items.iter().map(|i| i.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://xhamster.com/videos/mixed-xh1",
                "https://xhamster.com/videos/ok-xh2"
            ]
        );
    }

    #[test]
    fn ytdlp_reports_resume_support() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert!(extractor.supports_extractor_resume());
    }

    fn progress_line(status: &str, downloaded: &str, total: &str, file: &str) -> String {
        format!("PHLEGETHON_PROGRESS\t{status}\t{downloaded}\t{total}\tNA\t{file}\n")
    }

    #[test]
    fn progress_sums_video_and_audio_files_and_never_exceeds_the_total() {
        let mut tracker = ProgressTracker::default();
        let mut seen: Vec<ExtractorProgress> = Vec::new();
        let lines = [
            progress_line("downloading", "400", "1000", "v.f137.mp4"),
            progress_line("finished", "1000", "1000", "v.f137.mp4"),
            progress_line("downloading", "100", "200", "v.f140.m4a"),
            progress_line("finished", "200", "200", "v.f140.m4a"),
            "[Merger] Merging formats into \"v.mp4\"\n".to_string(),
        ];
        for line in &lines {
            seen.extend(tracker.feed(line.as_bytes()));
        }
        assert!(seen
            .iter()
            .all(|p| p.total_bytes.is_none_or(|t| p.downloaded_bytes <= t)));
        assert!(
            seen.windows(2)
                .all(|w| w[0].downloaded_bytes <= w[1].downloaded_bytes),
            "bytes went backwards: {seen:?}"
        );
        assert_eq!(
            seen.last(),
            Some(&ExtractorProgress {
                downloaded_bytes: 1200,
                total_bytes: Some(1200)
            })
        );
    }

    #[test]
    fn reported_percent_never_drops_when_the_audio_file_adds_to_the_total() {
        let mut tracker = ProgressTracker::default();
        let mut seen: Vec<ExtractorProgress> = Vec::new();
        let lines = [
            progress_line("finished", "1000", "1000", "v.f137.mp4"),
            progress_line("downloading", "100", "200", "v.f140.m4a"),
            progress_line("downloading", "150", "200", "v.f140.m4a"),
            progress_line("finished", "200", "200", "v.f140.m4a"),
        ];
        for line in &lines {
            seen.extend(tracker.feed(line.as_bytes()));
        }
        let share = |p: &ExtractorProgress| {
            p.downloaded_bytes as f64 / p.total_bytes.expect("total known") as f64
        };
        assert_eq!(
            seen.first(),
            Some(&ExtractorProgress {
                downloaded_bytes: 1000,
                total_bytes: Some(1000)
            })
        );
        assert!(
            seen.windows(2).all(|w| share(&w[0]) <= share(&w[1])),
            "percent went backwards: {seen:?}"
        );
        assert!(seen
            .iter()
            .all(|p| p.downloaded_bytes <= p.total_bytes.unwrap()));
        assert_eq!(
            seen.last(),
            Some(&ExtractorProgress {
                downloaded_bytes: 1200,
                total_bytes: Some(1200)
            })
        );
    }

    #[test]
    fn reported_percent_holds_across_a_line_with_no_known_total() {
        let mut tracker = ProgressTracker::default();
        let mut seen: Vec<ExtractorProgress> = Vec::new();
        let lines = [
            progress_line("finished", "1000", "1000", "v.f137.mp4"),
            // yt-dlp gave no total for the audio file yet, so the total is unknown here.
            progress_line("downloading", "50", "NA", "v.f140.m4a"),
            progress_line("downloading", "100", "200", "v.f140.m4a"),
        ];
        for line in &lines {
            seen.extend(tracker.feed(line.as_bytes()));
        }
        let known: Vec<&ExtractorProgress> =
            seen.iter().filter(|p| p.total_bytes.is_some()).collect();
        assert!(
            known.windows(2).all(|w| {
                let (a, b) = (w[0], w[1]);
                a.downloaded_bytes as u128 * b.total_bytes.unwrap() as u128
                    <= b.downloaded_bytes as u128 * a.total_bytes.unwrap() as u128
            }),
            "percent went backwards: {seen:?}"
        );
        assert_eq!(
            seen.last(),
            Some(&ExtractorProgress {
                downloaded_bytes: 1200,
                total_bytes: Some(1200)
            })
        );
    }

    #[test]
    fn monotone_clamp_keeps_the_largest_total_and_never_exceeds_it() {
        let tracker = ProgressTracker {
            peak: Some(ExtractorProgress {
                downloaded_bytes: 300,
                total_bytes: Some(1000),
            }),
            ..Default::default()
        };
        // A smaller total later must not lower the reported total.
        let raised = tracker.monotone(ExtractorProgress {
            downloaded_bytes: 10,
            total_bytes: Some(500),
        });
        assert_eq!(
            raised,
            ExtractorProgress {
                downloaded_bytes: 300,
                total_bytes: Some(1000)
            }
        );
        // A raw figure above the total is capped at the total.
        let capped = tracker.monotone(ExtractorProgress {
            downloaded_bytes: 2000,
            total_bytes: Some(1000),
        });
        assert_eq!(capped.downloaded_bytes, 1000);
        // An unknown total is passed through, not turned into a percentage.
        let unknown = ExtractorProgress {
            downloaded_bytes: 42,
            total_bytes: None,
        };
        assert_eq!(tracker.monotone(unknown), unknown);
    }

    #[test]
    fn progress_lines_split_across_reads_and_noise_are_handled() {
        let mut tracker = ProgressTracker::default();
        let line = progress_line("downloading", "50", "100", "a.mp4");
        let (first, second) = line.as_bytes().split_at(17);
        assert_eq!(tracker.feed(b"[download] Destination: a.mp4\n"), None);
        assert_eq!(tracker.feed(first), None);
        assert_eq!(
            tracker.feed(second),
            Some(ExtractorProgress {
                downloaded_bytes: 50,
                total_bytes: Some(100)
            })
        );
        // Same figures again: no new report.
        assert_eq!(tracker.feed(line.as_bytes()), None);
        // Garbage in the numeric fields is ignored.
        assert_eq!(
            tracker.feed(progress_line("downloading", "NA", "NA", "a.mp4").as_bytes()),
            None
        );
    }

    #[test]
    fn progress_without_a_known_total_reports_bytes_only() {
        let mut tracker = ProgressTracker::default();
        assert_eq!(
            tracker.feed(progress_line("downloading", "123", "NA", "live.ts").as_bytes()),
            Some(ExtractorProgress {
                downloaded_bytes: 123,
                total_bytes: None
            })
        );
    }

    #[test]
    fn download_args_ask_yt_dlp_for_parseable_progress() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let args = extractor.download_args(
            "https://example.com/v",
            "best",
            Path::new("/tmp/dest"),
            &MediaExtrasOptions::default(),
            &CookieChannel::default(),
            &ProxySettings::default(),
            "%(title)s.%(ext)s",
            None,
        );
        assert!(args.iter().any(|a| a == "--progress"), "{args:?}");
        assert!(args.iter().any(|a| a == "--newline"), "{args:?}");
        let template = args
            .windows(2)
            .find(|w| w[0] == "--progress-template")
            .map(|w| w[1].as_str())
            .expect("progress template");
        assert!(
            template.starts_with("download:PHLEGETHON_PROGRESS\t"),
            "{template}"
        );
        assert_eq!(template.matches('\t').count(), 5);
    }

    #[cfg(unix)]
    #[test]
    fn download_reports_progress_while_the_child_runs() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("fake-yt-dlp");
        let out = tmp.path().join("out.mp4");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf 'PHLEGETHON_PROGRESS\\tdownloading\\t10\\t100\\tNA\\ta.mp4\\n'\n\
                 sleep 0.4\nprintf 'PHLEGETHON_PROGRESS\\tfinished\\t100\\t100\\tNA\\ta.mp4\\n'\n\
                 echo x > {out}\necho {out}\n",
                out = out.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig {
            binary_path: script,
            ..YtDlpConfig::default()
        });
        let seen = std::sync::Mutex::new(Vec::new());
        let stop = AtomicBool::new(false);
        let outcome = extractor
            .download_with_progress(
                "https://example.com/v",
                "best",
                tmp.path(),
                &stop,
                &MediaExtrasOptions::default(),
                &CookieChannel::default(),
                &ProxySettings::default(),
                "%(title)s.%(ext)s",
                None,
                &|p| seen.lock().unwrap().push((p, Instant::now())),
            )
            .unwrap();
        assert!(matches!(outcome, ExtractorDownloadOutcome::Completed(_)));
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(seen[0].0.downloaded_bytes, 10);
        assert_eq!(seen[1].0.downloaded_bytes, 100);
        assert!(
            seen[1].1.duration_since(seen[0].1) >= Duration::from_millis(200),
            "first report must arrive while the child still runs"
        );
    }

    // ---- Toolchain health and probe-output tolerance (H-24, M-40, M-28, L-20, L-21) ----

    #[cfg(unix)]
    fn write_executable(path: &Path, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A `--help` that fails (or times out) on a cold binary used to cache an empty flag
    /// set for the whole session, so `--impersonate` was never passed again.
    #[cfg(unix)]
    #[test]
    fn failed_help_is_not_cached_and_a_later_probe_sees_the_flags() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("yt-dlp");
        let marker = dir.path().join("warm");
        write_executable(
            &script,
            &format!(
                "#!/bin/sh\ncase \"$*\" in\n  *--help*) [ -f '{m}' ] || {{ touch '{m}'; exit 1; }}; echo '  --js-runtimes R  --impersonate T' ;;\n  *--list-impersonate-targets*) echo 'chrome  windows  curl_cffi' ;;\nesac\n",
                m = marker.display()
            ),
        );
        let toolchain = YtDlpExtractorToolchain::new(YtDlpConfig {
            launcher: Some(YtDlpLauncher::Binary { path: script }),
            ..YtDlpConfig::default()
        });
        assert!(
            !toolchain.supports_flag("--impersonate"),
            "cold start fails"
        );
        assert!(
            toolchain.supports_flag("--impersonate"),
            "the failure must not be cached"
        );
        assert!(toolchain
            .runtime_args()
            .contains(&"--impersonate".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn capability_caches_follow_the_binary_and_rebind() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("yt-dlp");
        write_executable(&script, "#!/bin/sh\necho '  --impersonate T'\n");
        let toolchain = YtDlpExtractorToolchain::new(YtDlpConfig {
            launcher: Some(YtDlpLauncher::Binary {
                path: script.clone(),
            }),
            ..YtDlpConfig::default()
        });
        assert!(toolchain.supports_flag("--impersonate"));
        assert!(!toolchain.supports_flag("--js-runtimes"));

        // The same path with new contents and a new mtime: an update from the panel.
        std::thread::sleep(Duration::from_millis(20));
        write_executable(
            &script,
            "#!/bin/sh\necho '  --impersonate T --js-runtimes R'\n",
        );
        let later = SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&script)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(
            toolchain.supports_flag("--js-runtimes"),
            "a changed binary must be probed again"
        );

        toolchain.clear_capability_caches();
        assert!(toolchain.supports_flag("--js-runtimes"));
    }

    /// M-55: the shell starts with the default config and publishes the discovery later.
    #[test]
    fn apply_discovered_config_replaces_launcher_and_js_runtime() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        assert_eq!(
            extractor.effective_binary_path(),
            PathBuf::from("yt-dlp"),
            "before discovery the bare yt-dlp name is used"
        );

        let runtime = JsRuntime {
            kind: crate::toolchain_discovery::JsRuntimeKind::Deno,
            path: PathBuf::from("/opt/deno"),
            version: Some("2.1.0".into()),
            supported: true,
        };
        extractor.apply_discovered_config(YtDlpConfig {
            launcher: Some(YtDlpLauncher::PythonModule {
                python: PathBuf::from("/usr/bin/python3"),
            }),
            launcher_source: Some(YtDlpSource::PythonModule),
            js_runtime: Some(runtime.clone()),
            ..YtDlpConfig::default()
        });

        assert_eq!(
            extractor.effective_binary_path(),
            PathBuf::from("/usr/bin/python3")
        );
        assert_eq!(extractor.js_runtime(), Some(runtime));
    }

    #[test]
    fn apply_discovered_config_keeps_the_startup_plugin_dirs_and_options() {
        let config = YtDlpConfig {
            plugin_dirs: vec![PathBuf::from("/data/yt-dlp-plugins")],
            probe_timeout_secs: 77,
            ..YtDlpConfig::default()
        };
        let extractor = YtDlpExtractorToolchain::new(config);
        extractor.apply_discovered_config(YtDlpConfig {
            binary_path: PathBuf::from("/usr/local/bin/yt-dlp"),
            plugin_dirs: vec![PathBuf::from("/elsewhere")],
            probe_timeout_secs: 5,
            ..YtDlpConfig::default()
        });
        assert_eq!(
            extractor.effective_binary_path(),
            PathBuf::from("/usr/local/bin/yt-dlp")
        );
        assert_eq!(
            extractor.effective_plugin_dirs(),
            vec![PathBuf::from("/data/yt-dlp-plugins")]
        );
    }

    /// An installed sidecar binary still wins over what discovery found.
    #[cfg(unix)]
    #[test]
    fn sidecar_binary_beats_a_discovered_launcher() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let manifest = crate::sidecar_updates::ToolchainManifest::stub("9.9.9");
        crate::sidecar_updates::write_toolchain_manifest(root, &manifest).unwrap();
        let binary = crate::sidecar_updates::toolchain_binary_path(root, &manifest);
        write_executable(&binary, "#!/bin/sh\n");

        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        extractor.bind_sidecar_root(root);
        extractor.apply_discovered_config(YtDlpConfig {
            binary_path: PathBuf::from("/usr/local/bin/yt-dlp"),
            ..YtDlpConfig::default()
        });
        assert_eq!(extractor.effective_binary_path(), binary);
    }

    /// `bind_sidecar_root` used to leave the caches of the previous binary in place.
    #[cfg(unix)]
    #[test]
    fn bind_sidecar_root_clears_the_capability_caches() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let manifest = crate::sidecar_updates::ToolchainManifest::stub("9.9.9");
        crate::sidecar_updates::write_toolchain_manifest(root, &manifest).unwrap();
        let binary = crate::sidecar_updates::toolchain_binary_path(root, &manifest);
        write_executable(&binary, "#!/bin/sh\necho '  --js-runtimes R'\n");

        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        extractor.set_supported_flags_for_test(&["--impersonate"]);
        assert!(extractor.supports_flag("--impersonate"));
        extractor.bind_sidecar_root(root);
        assert!(extractor.supports_flag("--js-runtimes"));
        assert!(!extractor.supports_flag("--impersonate"));
    }

    /// A slow `--help` must not hold the cache lock: other probes read it meanwhile.
    #[cfg(unix)]
    #[test]
    fn slow_help_does_not_block_concurrent_cache_readers() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("yt-dlp");
        write_executable(&script, "#!/bin/sh\nsleep 2\necho '  --impersonate T'\n");
        let toolchain = Arc::new(YtDlpExtractorToolchain::new(YtDlpConfig {
            launcher: Some(YtDlpLauncher::Binary { path: script }),
            ..YtDlpConfig::default()
        }));
        let slow = {
            let t = Arc::clone(&toolchain);
            std::thread::spawn(move || t.supports_flag("--impersonate"))
        };
        std::thread::sleep(Duration::from_millis(300));
        let started = Instant::now();
        let guard = toolchain.supported_flags.lock();
        assert!(guard.is_ok());
        drop(guard);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the cache lock was held while --help ran"
        );
        assert!(slow.join().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn bind_sidecar_root_ignores_a_non_executable_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let manifest = crate::sidecar_updates::ToolchainManifest::stub("9.9.9");
        crate::sidecar_updates::write_toolchain_manifest(root, &manifest).unwrap();
        let binary = crate::sidecar_updates::toolchain_binary_path(root, &manifest);
        std::fs::write(&binary, b"not executable").unwrap();

        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        extractor.bind_sidecar_root(root);
        assert!(extractor.sidecar_binary_for_test().is_none());
        assert_eq!(extractor.effective_binary_path(), PathBuf::from("yt-dlp"));
    }

    /// A binary deleted while the app runs must stop shadowing a working yt-dlp, and a
    /// rebind after the deletion drops the binding.
    #[cfg(unix)]
    #[test]
    fn deleted_sidecar_binary_is_unbound() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let manifest = crate::sidecar_updates::ToolchainManifest::stub("9.9.9");
        crate::sidecar_updates::write_toolchain_manifest(root, &manifest).unwrap();
        let binary = crate::sidecar_updates::toolchain_binary_path(root, &manifest);
        write_executable(&binary, "#!/bin/sh\n");

        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        extractor.bind_sidecar_root(root);
        assert_eq!(extractor.effective_binary_path(), binary);
        std::fs::remove_file(&binary).unwrap();
        assert_eq!(extractor.effective_binary_path(), PathBuf::from("yt-dlp"));
        extractor.bind_sidecar_root(root);
        assert!(extractor.sidecar_binary_for_test().is_none());
    }

    #[test]
    fn audio_ids_use_tbr_as_kbit_per_second() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = r#"{"id":"a","title":"t","extractor_key":"Youtube","extractor":"youtube",
            "webpage_url":"https://www.youtube.com/watch?v=a",
            "formats":[
              {"format_id":"139","format_note":"low","ext":"m4a","tbr":48.7,"vcodec":"none","acodec":"mp4a"},
              {"format_id":"140","format_note":"medium","ext":"m4a","tbr":129.5,"vcodec":"none","acodec":"mp4a"},
              {"format_id":"251","format_note":"high","ext":"webm","tbr":135.2,"vcodec":"none","acodec":"opus"}]}"#;
        let result = extractor
            .parse_probe_result(json, "https://www.youtube.com/watch?v=a")
            .unwrap();
        let ids: Vec<&str> = result.items[0]
            .qualities
            .iter()
            .map(|q| q.id.as_str())
            .collect();
        assert_eq!(ids, vec!["49k-audio", "130k-audio", "135k-audio"]);
    }

    #[test]
    fn resolution_string_becomes_a_height_selector() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = r#"{"id":"a","title":"t","extractor_key":"Youtube","extractor":"youtube",
            "webpage_url":"https://www.youtube.com/watch?v=a",
            "formats":[{"format_id":"x","ext":"mp4","resolution":"1920x1080","vcodec":"avc1","acodec":"mp4a"}]}"#;
        let result = extractor
            .parse_probe_result(json, "https://www.youtube.com/watch?v=a")
            .unwrap();
        let id = &result.items[0].qualities[0].id;
        assert_eq!(id, "1080p");
        assert_eq!(
            quality_id_to_ytdlp_format(id, true),
            "best[height=1080]/best[height<=1080]/best"
        );
        assert_eq!(height_from_resolution("audio only"), None);
    }

    #[test]
    fn probe_json_tolerates_float_and_null_fields() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        let json = r#"{"id":"a","title":null,"extractor_key":"Youtube","extractor":"youtube",
            "webpage_url":"https://www.youtube.com/watch?v=a","duration":null,
            "formats":[
              {"format_id":"22","height":720.0,"width":null,"filesize":1234567.0,"filesize_approx":null,
               "asr":44100.0,"fps":null,"tbr":null,"ext":"mp4","vcodec":"avc1","acodec":"mp4a"}]}"#;
        let result = extractor
            .parse_probe_result(json, "https://www.youtube.com/watch?v=a")
            .expect("one float or null field must not fail the whole probe");
        let quality = &result.items[0].qualities[0];
        assert_eq!(quality.id, "720p");
        assert_eq!(quality.size_bytes, Some(1_234_567));
    }

    #[test]
    fn expected_extractor_covers_launch_site_subdomains() {
        let extractor = YtDlpExtractorToolchain::new(YtDlpConfig::default());
        for (url, key) in [
            ("https://m.xhamster.com/videos/x-1", "XHamster"),
            ("https://de.xhamster.com/videos/x-1", "XHamster"),
            ("https://music.youtube.com/watch?v=a", "Youtube"),
            ("https://en.xxxbp.tv/v/1", "XxxbpTV"),
            ("https://de.luxuretv.com/videos/x-1.html", "LuxureTV"),
        ] {
            assert_eq!(
                extractor.expected_extractor_for_url(url),
                Some(&[key][..]),
                "{url}"
            );
        }
    }
}
