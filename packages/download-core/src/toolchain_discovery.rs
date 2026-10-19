//! Extractor Toolchain discovery: where yt-dlp, ffmpeg and a JS runtime live.
//!
//! Resolution order for yt-dlp (first hit wins):
//!
//! 1. the app-managed sidecar (`<sidecar_root>/toolchain-manifest.json`),
//! 2. the `YTDLP_PATH` environment variable,
//! 3. a `yt-dlp` executable on `PATH` (the distro package on Void),
//! 4. `python3 -m yt_dlp` when the Python module is importable.
//!
//! Current yt-dlp needs a JavaScript runtime (deno ≥ 2.3 or node ≥ 22) plus
//! the `yt-dlp-ejs` challenge solver for full YouTube support; this module
//! detects one and produces the matching `--js-runtimes` argument.
//!
//! Everything that touches the process environment takes injectable inputs so
//! the ordering rules are unit-tested without spawning real tools.

use serde::Serialize;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Launcher
// ---------------------------------------------------------------------------

/// How to start yt-dlp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum YtDlpLauncher {
    /// A standalone executable (PyInstaller build, distro package or zipimport script).
    Binary { path: PathBuf },
    /// `python -m yt_dlp` (musl systems without a standalone build, or pip installs).
    PythonModule { python: PathBuf },
}

impl YtDlpLauncher {
    pub fn binary(path: impl Into<PathBuf>) -> Self {
        Self::Binary { path: path.into() }
    }

    /// A [`Command`] with no arguments yet.
    pub fn command(&self) -> Command {
        let mut cmd = match self {
            Self::Binary { path } => Command::new(path),
            Self::PythonModule { python } => {
                let mut cmd = Command::new(python);
                cmd.args(["-m", "yt_dlp"]);
                cmd
            }
        };
        // Python picks its stdout encoding from the locale; under a C locale yt-dlp would
        // fail on, or mangle, non-ASCII titles. Force UTF-8 for the child.
        cmd.env("PYTHONUTF8", "1").env("PYTHONIOENCODING", "utf-8");
        cmd
    }

    /// Human-readable location for the Toolchain panel.
    pub fn describe(&self) -> String {
        match self {
            Self::Binary { path } => path.display().to_string(),
            Self::PythonModule { python } => format!("{} -m yt_dlp", python.display()),
        }
    }

    /// The executable path for [`Self::Binary`]; `None` for the Python module.
    pub fn binary_path(&self) -> Option<&Path> {
        match self {
            Self::Binary { path } => Some(path),
            Self::PythonModule { .. } => None,
        }
    }
}

/// Where the active yt-dlp came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum YtDlpSource {
    SidecarManifest,
    EnvVar,
    Path,
    PythonModule,
}

/// Injectable environment for discovery (tests stub every probe).
pub struct DiscoveryEnv<'a> {
    /// `YTDLP_PATH`, when set.
    pub env_ytdlp_path: Option<OsString>,
    /// Directories from `PATH`.
    pub path_dirs: &'a [PathBuf],
    /// Does `python -c "import yt_dlp"` succeed for this interpreter?
    pub python_has_ytdlp: &'a dyn Fn(&Path) -> bool,
}

/// Find yt-dlp following the documented order.
pub fn discover_ytdlp(
    sidecar_root: Option<&Path>,
    env: &DiscoveryEnv<'_>,
) -> Option<(YtDlpLauncher, YtDlpSource)> {
    if let Some(root) = sidecar_root {
        if let Ok(Some(manifest)) = crate::sidecar_updates::load_toolchain_manifest(root) {
            let path = crate::sidecar_updates::toolchain_binary_path(root, &manifest);
            if is_executable_file(&path) {
                return Some((YtDlpLauncher::binary(path), YtDlpSource::SidecarManifest));
            }
        }
    }
    if let Some(raw) = env.env_ytdlp_path.as_ref().filter(|v| !v.is_empty()) {
        let path = PathBuf::from(raw);
        if is_executable_file(&path) {
            return Some((YtDlpLauncher::binary(path), YtDlpSource::EnvVar));
        }
        // A bare name on PATH is also accepted.
        if let Some(found) = find_in_path(&raw.to_string_lossy(), env.path_dirs) {
            return Some((YtDlpLauncher::binary(found), YtDlpSource::EnvVar));
        }
    }
    if let Some(found) = find_in_path("yt-dlp", env.path_dirs) {
        return Some((YtDlpLauncher::binary(found), YtDlpSource::Path));
    }
    for name in ["python3", "python"] {
        if let Some(python) = find_in_path(name, env.path_dirs) {
            if (env.python_has_ytdlp)(&python) {
                return Some((
                    YtDlpLauncher::PythonModule { python },
                    YtDlpSource::PythonModule,
                ));
            }
        }
    }
    None
}

/// `PATH` split into directories.
///
/// Relative and empty entries are dropped: they mean "the current directory", which
/// would let a file in whatever folder the app started from stand in for a tool.
pub fn path_dirs_from_env() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .filter(|dir| dir.is_absolute())
                .collect()
        })
        .unwrap_or_default()
}

/// First `dir/name` on `dirs` that is an executable file.
pub fn find_in_path(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    if name.contains(std::path::MAIN_SEPARATOR) {
        let p = PathBuf::from(name);
        return is_executable_file(&p).then_some(p);
    }
    dirs.iter()
        .filter(|d| d.is_absolute())
        .map(|d| d.join(name))
        .find(|p| is_executable_file(p))
}

/// Regular file with an execute bit (any file on non-Unix).
pub fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Production check for `python -c "import yt_dlp"`.
pub fn python_has_module(python: &Path, module: &str) -> bool {
    let mut cmd = Command::new(python);
    cmd.args(["-c", &format!("import {module}")]);
    matches!(
        run_command_with_deadline(cmd, None, Some(Instant::now() + Duration::from_secs(10))),
        Ok(CommandOutcome::Finished { status, .. }) if status.success()
    )
}

// ---------------------------------------------------------------------------
// JS runtime
// ---------------------------------------------------------------------------

/// JavaScript runtimes yt-dlp can drive for its challenge solver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JsRuntimeKind {
    Deno,
    Node,
    QuickJs,
    Bun,
}

impl JsRuntimeKind {
    /// Name yt-dlp expects in `--js-runtimes`.
    pub fn ytdlp_name(self) -> &'static str {
        match self {
            Self::Deno => "deno",
            Self::Node => "node",
            Self::QuickJs => "quickjs",
            Self::Bun => "bun",
        }
    }

    fn executable_names(self) -> &'static [&'static str] {
        match self {
            Self::Deno => &["deno"],
            Self::Node => &["node", "nodejs"],
            Self::QuickJs => &["qjs", "quickjs"],
            Self::Bun => &["bun"],
        }
    }

    /// Minimum version yt-dlp supports (`None` = any).
    fn minimum(self) -> Option<(u64, u64, u64)> {
        match self {
            Self::Deno => Some((2, 3, 0)),
            Self::Node => Some((22, 0, 0)),
            Self::QuickJs => None,
            Self::Bun => None,
        }
    }

    /// Detection order: deno is yt-dlp's default, bun is deprecated upstream.
    const ORDER: [JsRuntimeKind; 4] = [Self::Deno, Self::Node, Self::QuickJs, Self::Bun];
}

/// A detected JS runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JsRuntime {
    pub kind: JsRuntimeKind,
    pub path: PathBuf,
    pub version: Option<String>,
    /// Meets yt-dlp's minimum version (unknown versions are assumed fine).
    pub supported: bool,
}

/// Detect the preferred JS runtime on `path_dirs`.
///
/// `version_of(path)` runs `<path> --version` in production; tests stub it.
pub fn detect_js_runtime(
    path_dirs: &[PathBuf],
    version_of: &dyn Fn(&Path) -> Option<String>,
) -> Option<JsRuntime> {
    let mut fallback: Option<JsRuntime> = None;
    for kind in JsRuntimeKind::ORDER {
        for name in kind.executable_names() {
            let Some(path) = find_in_path(name, path_dirs) else {
                continue;
            };
            let version = version_of(&path).and_then(|raw| parse_version_string(&raw));
            let supported = match (kind.minimum(), version.as_deref().and_then(parse_semver)) {
                (Some(min), Some(found)) => found >= min,
                _ => true,
            };
            let runtime = JsRuntime {
                kind,
                path,
                version,
                supported,
            };
            if supported && kind != JsRuntimeKind::Bun {
                return Some(runtime);
            }
            if fallback.is_none() {
                fallback = Some(runtime);
            }
        }
    }
    fallback
}

/// `deno 2.3.1 (stable, ...)`, `v22.1.0`, `QuickJS-ng version 0.10.1` → `2.3.1` etc.
pub fn parse_version_string(raw: &str) -> Option<String> {
    let first_line = raw.lines().find(|l| !l.trim().is_empty())?;
    let mut best: Option<&str> = None;
    for token in first_line.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',') {
        let token = token.trim_start_matches('v').trim_start_matches('V');
        let dotted = token.chars().filter(|c| *c == '.').count() >= 1;
        if dotted && token.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            best = Some(token);
            break;
        }
    }
    best.map(|s| s.trim_end_matches('.').to_string())
}

/// `2.3.1` → `(2, 3, 1)`; missing components are zero.
pub fn parse_semver(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next().flatten().unwrap_or(0);
    let patch = parts.next().flatten().unwrap_or(0);
    Some((major, minor, patch))
}

/// `--js-runtimes` (+ optional `--remote-components`) arguments for yt-dlp.
///
/// Only emitted for a supported runtime. `remote_components` lets yt-dlp fetch
/// the `yt-dlp-ejs` solver from GitHub when it is not bundled — opt-in because
/// it is network access the user did not ask for.
pub fn ytdlp_js_runtime_args(runtime: Option<&JsRuntime>, remote_components: bool) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(rt) = runtime.filter(|rt| rt.supported) {
        args.push("--js-runtimes".to_string());
        args.push(format!("{}:{}", rt.kind.ytdlp_name(), rt.path.display()));
    }
    if remote_components {
        args.push("--remote-components".to_string());
        args.push("ejs:github".to_string());
    }
    args
}

/// The dotted version in a tool's `--version` output, or `unknown`: arbitrary output
/// (a banner, an error line) is never shown as a version.
pub fn version_or_unknown(raw: &str) -> String {
    parse_version_string(raw).unwrap_or_else(|| "unknown".to_string())
}

/// Production `<path> --version` with a short timeout.
pub fn tool_version(path: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new(path);
    cmd.args(args);
    match run_command_with_deadline(cmd, None, Some(Instant::now() + Duration::from_secs(10))) {
        Ok(CommandOutcome::Finished {
            status,
            stdout,
            stderr,
        }) if status.success() => {
            let text = if stdout.is_empty() { stderr } else { stdout };
            let text = String::from_utf8_lossy(&text);
            Some(version_or_unknown(&text))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Platform
// ---------------------------------------------------------------------------

/// C library flavour (decides which yt-dlp release asset can run).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Libc {
    Glibc,
    Musl,
    Unknown,
}

/// Host platform summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlatformInfo {
    pub os: String,
    pub arch: String,
    pub libc: Libc,
}

impl PlatformInfo {
    /// The host platform. Detected once: the libc check can run `ldd --version`, and
    /// every Toolchain status asks for this.
    pub fn current() -> Self {
        static CURRENT: std::sync::OnceLock<PlatformInfo> = std::sync::OnceLock::new();
        CURRENT
            .get_or_init(|| Self {
                os: std::env::consts::OS.to_string(),
                arch: std::env::consts::ARCH.to_string(),
                libc: detect_libc(),
            })
            .clone()
    }
}

/// glibc vs musl: compile-time target first, then the dynamic loader on disk,
/// then `ldd --version` (musl's prints "musl libc").
pub fn detect_libc() -> Libc {
    if !cfg!(target_os = "linux") {
        return Libc::Unknown;
    }
    if cfg!(target_env = "musl") {
        return Libc::Musl;
    }
    if let Ok(entries) = std::fs::read_dir("/lib") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("ld-musl-") {
                return Libc::Musl;
            }
        }
    }
    if Path::new("/lib64/ld-linux-x86-64.so.2").exists()
        || Path::new("/lib/ld-linux-aarch64.so.1").exists()
        || Path::new("/lib/ld-linux-armhf.so.3").exists()
    {
        return Libc::Glibc;
    }
    let mut cmd = Command::new("ldd");
    cmd.arg("--version");
    if let Ok(CommandOutcome::Finished { stdout, stderr, .. }) =
        run_command_with_deadline(cmd, None, Some(Instant::now() + Duration::from_secs(5)))
    {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        )
        .to_ascii_lowercase();
        if text.contains("musl") {
            return Libc::Musl;
        }
        if text.contains("glibc") || text.contains("gnu libc") {
            return Libc::Glibc;
        }
    }
    Libc::Unknown
}

// ---------------------------------------------------------------------------
// Status report
// ---------------------------------------------------------------------------

/// One detected tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolInfo {
    pub path: String,
    pub version: Option<String>,
}

/// Is the `yt-dlp-ejs` challenge solver available?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EjsStatus {
    /// Standalone yt-dlp builds bundle it.
    Bundled,
    /// `yt_dlp_ejs` Python package importable (Void: `python3-yt-dlp-ejs`).
    PythonPackage,
    Missing,
    Unknown,
}

/// Snapshot for the Toolchain panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolchainStatus {
    pub ytdlp: Option<ToolInfo>,
    pub ytdlp_source: Option<YtDlpSource>,
    pub ytdlp_launcher: Option<YtDlpLauncher>,
    pub ffmpeg: Option<ToolInfo>,
    pub ffprobe: Option<ToolInfo>,
    pub js_runtime: Option<JsRuntime>,
    pub ejs: EjsStatus,
    pub platform: PlatformInfo,
    pub sidecar_root: Option<String>,
    pub plugin_dirs: Vec<String>,
    pub remote_components_enabled: bool,
    /// yt-dlp can impersonate a browser TLS fingerprint (curl_cffi present).
    pub impersonation_available: bool,
    /// Human hints: what is missing and how to fix it.
    pub issues: Vec<String>,
}

impl ToolchainStatus {
    /// Report for toolchains that cannot introspect (fakes).
    pub fn unavailable() -> Self {
        Self {
            ytdlp: None,
            ytdlp_source: None,
            ytdlp_launcher: None,
            ffmpeg: None,
            ffprobe: None,
            js_runtime: None,
            ejs: EjsStatus::Unknown,
            platform: PlatformInfo::current(),
            sidecar_root: None,
            plugin_dirs: Vec::new(),
            remote_components_enabled: false,
            impersonation_available: false,
            issues: vec!["Extractor Toolchain status is not available.".to_string()],
        }
    }

    /// Fill `issues` from the detected state.
    pub fn with_issues(mut self) -> Self {
        let mut issues = Vec::new();
        if self.ytdlp.is_none() {
            issues.push(match self.platform.libc {
                Libc::Musl => "yt-dlp was not found. On musl Void install the `yt-dlp` package \
                    (`xbps-install yt-dlp`) or use Install below to fetch the universal \
                    script (needs python3)."
                    .to_string(),
                _ => "yt-dlp was not found. Install the `yt-dlp` package or use Install below \
                    to download the latest release into the app sidecar."
                    .to_string(),
            });
        }
        if self.ffmpeg.is_none() {
            issues.push(
                "ffmpeg was not found: HLS/DASH downloads, merging and remux need it \
                 (`xbps-install ffmpeg`)."
                    .to_string(),
            );
        }
        match &self.js_runtime {
            None => issues.push(
                "No JavaScript runtime found: YouTube needs deno (≥ 2.3) or node (≥ 22) \
                 for yt-dlp's challenge solver (`xbps-install deno` or `nodejs`)."
                    .to_string(),
            ),
            Some(rt) if !rt.supported => issues.push(format!(
                "{} {} is too old for yt-dlp; install deno ≥ 2.3 or node ≥ 22.",
                rt.kind.ytdlp_name(),
                rt.version.as_deref().unwrap_or("(unknown version)")
            )),
            Some(_) => {}
        }
        if self.ejs == EjsStatus::Missing {
            issues.push(
                "yt-dlp-ejs is not installed: install `python3-yt-dlp-ejs` or enable remote \
                 components so yt-dlp fetches it from GitHub."
                    .to_string(),
            );
        }
        self.issues = issues;
        self
    }
}

// ---------------------------------------------------------------------------
// Process runner with stop + deadline
// ---------------------------------------------------------------------------

/// Outcome of waiting on a child process that may be killed on stop or deadline.
#[derive(Debug)]
pub enum CommandOutcome {
    Finished {
        status: std::process::ExitStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    /// `stop` was set; the child was killed.
    Stopped,
    /// `deadline` passed; the child was killed.
    TimedOut,
}

/// Spawn `cmd`, poll `stop` and `deadline`, and hard-kill the child on either.
pub fn run_command_with_deadline(
    mut cmd: Command,
    stop: Option<&AtomicBool>,
    deadline: Option<Instant>,
) -> Result<CommandOutcome, String> {
    run_command_with_deadline_polling(
        cmd_prepare(&mut cmd),
        stop,
        deadline,
        Duration::from_millis(50),
    )
}

fn cmd_prepare(cmd: &mut Command) -> &mut Command {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
}

/// How long to wait for the output readers once the child has exited or been killed.
const READER_GRACE: Duration = Duration::from_millis(1500);

/// Output captured from one pipe by a detached reader thread.
struct PipeCapture {
    buf: Arc<Mutex<Vec<u8>>>,
    done: Arc<AtomicBool>,
}

impl PipeCapture {
    fn spawn<R: std::io::Read + Send + 'static>(mut pipe: R) -> Self {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::new(AtomicBool::new(false));
        let (thread_buf, thread_done) = (Arc::clone(&buf), Arc::clone(&done));
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut b) = thread_buf.lock() {
                            b.extend_from_slice(&chunk[..n]);
                        }
                    }
                }
            }
            thread_done.store(true, Ordering::Release);
        });
        Self { buf, done }
    }

    fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    /// Bytes captured so far (or taken so far, never decreasing within one run).
    fn len(&self) -> usize {
        self.buf.lock().map(|b| b.len()).unwrap_or_default()
    }

    /// Copy the bytes captured after the first `from` bytes.
    fn read_from(&self, from: usize) -> Vec<u8> {
        self.buf
            .lock()
            .map(|b| b.get(from..).map(<[u8]>::to_vec).unwrap_or_default())
            .unwrap_or_default()
    }

    /// Take what was captured so far.
    fn take(&self) -> Vec<u8> {
        self.buf
            .lock()
            .map(|mut b| std::mem::take(&mut *b))
            .unwrap_or_default()
    }
}

/// Wait up to [`READER_GRACE`] for both readers to reach EOF. Returns false when a
/// descendant still holds a pipe open.
fn wait_for_readers(readers: [&PipeCapture; 2]) -> bool {
    let end = Instant::now() + READER_GRACE;
    while !readers.iter().all(|r| r.is_done()) {
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

/// Put the child in its own process group and ask the kernel to kill it when the
/// spawning thread dies, so it cannot outlive the app.
fn own_the_process_tree(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        #[cfg(target_os = "linux")]
        // SAFETY: the hook only calls `prctl`, which is async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong);
                Ok(())
            });
        }
    }
}

/// Kill the child's whole process group, then the child itself. Call it only while the
/// child is not yet reaped, or when the group is known to still have members, so the
/// group id cannot belong to an unrelated process.
fn kill_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: plain syscall; `pid` is the group id set by `process_group(0)`.
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// [`run_command_with_deadline`] with an explicit poll interval.
///
/// The child leads its own process group. `stop`, `deadline` and a wait error kill the
/// whole group (yt-dlp's ffmpeg included) and never block on the output readers: a
/// descendant that escaped the group and holds a pipe open cannot stall the caller.
pub fn run_command_with_deadline_polling(
    cmd: &mut Command,
    stop: Option<&AtomicBool>,
    deadline: Option<Instant>,
    poll_interval: Duration,
) -> Result<CommandOutcome, String> {
    run_command_observing(cmd, stop, deadline, poll_interval, None)
}

/// Callback that receives each stdout chunk while a child runs.
type ChunkObserver<'a> = &'a mut dyn FnMut(&[u8]);

/// [`run_command_with_deadline_polling`] that also hands every new chunk of stdout to
/// `observer` while the child runs (called from the polling thread, once per poll).
/// The full output is still returned at the end.
pub fn run_command_observing(
    cmd: &mut Command,
    stop: Option<&AtomicBool>,
    deadline: Option<Instant>,
    poll_interval: Duration,
    observer: Option<ChunkObserver<'_>>,
) -> Result<CommandOutcome, String> {
    run_command_watched(cmd, stop, deadline, None, poll_interval, observer)
}

/// [`run_command_observing`] that also gives up when the child prints nothing on stdout
/// or stderr for `stall`: the outcome is [`CommandOutcome::TimedOut`] after the process
/// tree is killed. A hung network read then ends the run instead of blocking it forever.
pub fn run_command_watched(
    cmd: &mut Command,
    stop: Option<&AtomicBool>,
    deadline: Option<Instant>,
    stall: Option<Duration>,
    poll_interval: Duration,
    mut observer: Option<ChunkObserver<'_>>,
) -> Result<CommandOutcome, String> {
    if stop.is_some_and(|s| s.load(Ordering::Acquire)) {
        return Ok(CommandOutcome::Stopped);
    }
    let program = cmd.get_program().to_string_lossy().into_owned();
    // Output is always captured here; callers must not have to remember to pipe it.
    cmd_prepare(cmd);
    own_the_process_tree(cmd);
    let mut child = cmd.spawn().map_err(|e| {
        let message = format!("failed to spawn {program}: {e}");
        if e.kind() == std::io::ErrorKind::NotFound {
            tag_error(ERROR_CODE_TOOLCHAIN_MISSING, message)
        } else {
            message
        }
    })?;

    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| format!("{program}: missing stdout pipe"))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| format!("{program}: missing stderr pipe"))?;
    let stdout = PipeCapture::spawn(stdout_pipe);
    let stderr = PipeCapture::spawn(stderr_pipe);

    let mut observed = 0usize;
    let mut observe = |stdout: &PipeCapture, observed: &mut usize| {
        if let Some(observer) = observer.as_deref_mut() {
            let chunk = stdout.read_from(*observed);
            if !chunk.is_empty() {
                *observed += chunk.len();
                observer(&chunk);
            }
        }
    };

    let mut last_output = Instant::now();
    let mut seen = 0usize;
    let status = loop {
        observe(&stdout, &mut observed);
        let total = stdout.len() + stderr.len();
        if total != seen {
            seen = total;
            last_output = Instant::now();
        }
        if stall.is_some_and(|limit| last_output.elapsed() >= limit) {
            kill_process_tree(&mut child);
            wait_for_readers([&stdout, &stderr]);
            return Ok(CommandOutcome::TimedOut);
        }
        if stop.is_some_and(|s| s.load(Ordering::Acquire)) {
            kill_process_tree(&mut child);
            wait_for_readers([&stdout, &stderr]);
            return Ok(CommandOutcome::Stopped);
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            kill_process_tree(&mut child);
            wait_for_readers([&stdout, &stderr]);
            return Ok(CommandOutcome::TimedOut);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(poll_interval),
            Err(e) => {
                kill_process_tree(&mut child);
                wait_for_readers([&stdout, &stderr]);
                return Err(format!("failed while waiting for {program}: {e}"));
            }
        }
    };

    if !wait_for_readers([&stdout, &stderr]) {
        // The child is gone but a descendant keeps a pipe open. The group still has a
        // member, so its id is still ours: kill the stragglers.
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: plain syscall; the group is alive, so the id is not reusable.
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
        }
        wait_for_readers([&stdout, &stderr]);
    }
    observe(&stdout, &mut observed);
    Ok(CommandOutcome::Finished {
        status,
        stdout: stdout.take(),
        stderr: stderr.take(),
    })
}

/// Wait until `pid` is gone or a zombie. Test helper shared with other modules' tests.
#[cfg(all(test, unix))]
pub(crate) fn wait_until_process_gone(pid: i32, within: Duration) -> bool {
    let end = Instant::now() + within;
    loop {
        let gone = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit(')')
                .next()
                .is_some_and(|rest| rest.trim_start().starts_with('Z')),
        };
        if gone {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ---------------------------------------------------------------------------
// Failure classification
// ---------------------------------------------------------------------------

/// Machine-readable prefix for a toolchain error (`[needs_browser_cookies] ...`).
pub const ERROR_CODE_NEEDS_BROWSER_COOKIES: &str = "needs_browser_cookies";
pub const ERROR_CODE_NEEDS_JS_RUNTIME: &str = "needs_js_runtime";
pub const ERROR_CODE_TIMEOUT: &str = "timeout";
pub const ERROR_CODE_UNSUPPORTED_URL: &str = "unsupported_url";
pub const ERROR_CODE_GEO_BLOCKED: &str = "geo_blocked";
pub const ERROR_CODE_NOT_FOUND: &str = "not_found";
pub const ERROR_CODE_TOOLCHAIN_MISSING: &str = "toolchain_missing";

/// Classify yt-dlp stderr into a known failure code, when recognisable.
///
/// Each code needs wording that yt-dlp or the site really uses for that failure. A bare
/// `403` or a mention of `deno` is not enough: a 403 can be a hotlink or expired URL, and
/// `deno` can sit in a path.
pub fn classify_ytdlp_failure(stderr: &str) -> Option<&'static str> {
    let lower = stderr.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));

    // Geo wording is specific, so it wins over the generic blocked-request markers a
    // geo-block page often carries too.
    if has(&[
        "not available in your country",
        "available in your country",
        "not available in your region",
        "available in your region",
        "blocked in your country",
        "geo restricted",
        "geo-restricted",
        "geo restriction",
        "geo-restriction",
        "geoblocked",
        "geo-blocked",
        "geo bypass",
    ]) {
        return Some(ERROR_CODE_GEO_BLOCKED);
    }
    let cloudflare = has(&[
        "just a moment",
        "cloudflare",
        "cf-chl",
        "challenges.cloudflare.com",
        "captcha",
    ]);
    let login = has(&[
        "sign in to confirm",
        "login required",
        "log in to",
        "--cookies",
        "cookies-from-browser",
        "not a bot",
        "age-restricted",
        "confirm your age",
        "private video",
        "members-only",
    ]);
    if cloudflare || login {
        return Some(ERROR_CODE_NEEDS_BROWSER_COOKIES);
    }
    if has(&[
        "js runtime",
        "javascript runtime",
        "--js-runtimes",
        "n challenge solving failed",
        "nsig extraction failed",
        "yt-dlp-ejs",
    ]) {
        return Some(ERROR_CODE_NEEDS_JS_RUNTIME);
    }
    if has(&[
        "http error 404",
        "video unavailable",
        "has been removed",
        "no longer available",
    ]) {
        return Some(ERROR_CODE_NOT_FOUND);
    }
    if lower.contains("unsupported url") {
        return Some(ERROR_CODE_UNSUPPORTED_URL);
    }
    None
}

/// Prefix `message` with `[code]` so shells can branch on it.
pub fn tag_error(code: &str, message: impl AsRef<str>) -> String {
    format!("[{code}] {}", message.as_ref())
}

/// Read a `[code]` prefix back out of an error string.
pub fn error_code(message: &str) -> Option<&str> {
    let rest = message.strip_prefix('[')?;
    let end = rest.find(']')?;
    let code = &rest[..end];
    if code.is_empty() || !code.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
        return None;
    }
    Some(code)
}

/// User-facing explanation for a failure code.
pub fn error_code_hint(code: &str) -> Option<&'static str> {
    match code {
        ERROR_CODE_NEEDS_BROWSER_COOKIES => Some(
            "The site blocked the request (login wall, bot check or Cloudflare challenge). \
             Open the page in your browser once, then set Cookies → from browser in Settings \
             and retry.",
        ),
        ERROR_CODE_NEEDS_JS_RUNTIME => Some(
            "yt-dlp needs a JavaScript runtime for this site. Install deno (≥ 2.3) or node \
             (≥ 22) and make sure yt-dlp-ejs is available (see Toolchain).",
        ),
        ERROR_CODE_TIMEOUT => Some("The probe took too long. Check the network, or raise the probe timeout in Settings."),
        ERROR_CODE_GEO_BLOCKED => Some("The site reports this media is not available in your region. A proxy in Settings may help."),
        ERROR_CODE_NOT_FOUND => Some("The media could not be found; it may have been removed."),
        ERROR_CODE_UNSUPPORTED_URL => Some("yt-dlp does not support this URL; the page sniffer found nothing either."),
        ERROR_CODE_TOOLCHAIN_MISSING => Some(
            "A required tool was not found. Open Toolchain → Install to set up yt-dlp, or \
             install it with your package manager.",
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    #[test]
    fn discovery_prefers_sidecar_then_env_then_path_then_python() {
        let tmp = tempfile::tempdir().unwrap();
        let sidecar = tmp.path().join("sidecar");
        let bin_a = tmp.path().join("a");
        let bin_b = tmp.path().join("b");
        std::fs::create_dir_all(&sidecar).unwrap();
        std::fs::create_dir_all(&bin_a).unwrap();
        std::fs::create_dir_all(&bin_b).unwrap();
        let no_python = |_: &Path| false;
        let yes_python = |_: &Path| true;
        let path_dirs = vec![bin_a.clone(), bin_b.clone()];

        // Nothing anywhere.
        let env = DiscoveryEnv {
            env_ytdlp_path: None,
            path_dirs: &path_dirs,
            python_has_ytdlp: &no_python,
        };
        assert_eq!(discover_ytdlp(Some(&sidecar), &env), None);

        // python3 on PATH with the module → PythonModule.
        let py = exe(&bin_b, "python3");
        let env = DiscoveryEnv {
            env_ytdlp_path: None,
            path_dirs: &path_dirs,
            python_has_ytdlp: &yes_python,
        };
        assert_eq!(
            discover_ytdlp(Some(&sidecar), &env),
            Some((
                YtDlpLauncher::PythonModule { python: py.clone() },
                YtDlpSource::PythonModule
            ))
        );

        // yt-dlp on PATH beats python.
        let on_path = exe(&bin_b, "yt-dlp");
        assert_eq!(
            discover_ytdlp(Some(&sidecar), &env),
            Some((YtDlpLauncher::binary(on_path.clone()), YtDlpSource::Path))
        );

        // YTDLP_PATH beats PATH.
        let env_bin = exe(&bin_a, "yt-dlp-custom");
        let env = DiscoveryEnv {
            env_ytdlp_path: Some(env_bin.clone().into_os_string()),
            path_dirs: &path_dirs,
            python_has_ytdlp: &yes_python,
        };
        assert_eq!(
            discover_ytdlp(Some(&sidecar), &env),
            Some((YtDlpLauncher::binary(env_bin), YtDlpSource::EnvVar))
        );

        // A sidecar manifest with an executable beats everything.
        let manifest = crate::sidecar_updates::ToolchainManifest::stub("2026.08.19");
        crate::sidecar_updates::write_toolchain_manifest(&sidecar, &manifest).unwrap();
        exe(&sidecar, "yt-dlp");
        assert_eq!(
            discover_ytdlp(Some(&sidecar), &env).map(|(_, s)| s),
            Some(YtDlpSource::SidecarManifest)
        );

        // A manifest whose binary lacks the exec bit is skipped.
        std::fs::write(sidecar.join("yt-dlp"), "not executable").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                sidecar.join("yt-dlp"),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
            assert_eq!(
                discover_ytdlp(Some(&sidecar), &env).map(|(_, s)| s),
                Some(YtDlpSource::EnvVar)
            );
        }
    }

    #[test]
    fn js_runtime_detection_prefers_deno_and_checks_minimums() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().to_path_buf();
        let node = exe(&bin, "node");
        let dirs = vec![bin.clone()];

        let old_node = |_: &Path| Some("v18.19.0".to_string());
        let rt = detect_js_runtime(&dirs, &old_node).unwrap();
        assert_eq!(rt.kind, JsRuntimeKind::Node);
        assert!(!rt.supported, "node 18 is below yt-dlp's minimum");
        assert_eq!(rt.version.as_deref(), Some("18.19.0"));
        assert!(ytdlp_js_runtime_args(Some(&rt), false).is_empty());

        let new_node = |_: &Path| Some("v22.22.0".to_string());
        let rt = detect_js_runtime(&dirs, &new_node).unwrap();
        assert!(rt.supported);
        assert_eq!(
            ytdlp_js_runtime_args(Some(&rt), true),
            vec![
                "--js-runtimes".to_string(),
                format!("node:{}", node.display()),
                "--remote-components".to_string(),
                "ejs:github".to_string(),
            ]
        );

        let deno = exe(&bin, "deno");
        let versions = move |p: &Path| {
            if p.ends_with("deno") {
                Some("deno 2.4.0 (stable, release, x86_64-unknown-linux-gnu)".to_string())
            } else {
                Some("v22.22.0".to_string())
            }
        };
        let rt = detect_js_runtime(&dirs, &versions).unwrap();
        assert_eq!(rt.kind, JsRuntimeKind::Deno);
        assert_eq!(rt.path, deno);
        assert_eq!(rt.version.as_deref(), Some("2.4.0"));
    }

    #[test]
    fn version_strings_are_parsed() {
        assert_eq!(
            parse_version_string("deno 2.3.1 (stable)").as_deref(),
            Some("2.3.1")
        );
        assert_eq!(parse_version_string("v22.1.0\n").as_deref(), Some("22.1.0"));
        assert_eq!(
            parse_version_string("2026.08.19").as_deref(),
            Some("2026.08.19")
        );
        assert_eq!(
            parse_version_string("ffmpeg version 7.1.1 Copyright (c) 2000-2025").as_deref(),
            Some("7.1.1")
        );
        assert_eq!(version_or_unknown("2026.08.19\n"), "2026.08.19");
        assert_eq!(version_or_unknown("Usage: tool [options]\n"), "unknown");
        assert_eq!(version_or_unknown(""), "unknown");
        assert_eq!(parse_semver("22.1.0"), Some((22, 1, 0)));
        assert_eq!(parse_semver("2.3"), Some((2, 3, 0)));
        assert_eq!(parse_semver("nope"), None);
    }

    #[test]
    fn deadline_kills_long_running_child() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let started = Instant::now();
        let outcome =
            run_command_with_deadline(cmd, None, Some(Instant::now() + Duration::from_millis(200)))
                .unwrap();
        assert!(matches!(outcome, CommandOutcome::TimedOut), "{outcome:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn stop_flag_kills_child() {
        let stop = AtomicBool::new(false);
        let stop_ref = &stop;
        let outcome = std::thread::scope(|s| {
            s.spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                stop_ref.store(true, Ordering::Release);
            });
            let mut cmd = Command::new("sleep");
            cmd.arg("30");
            run_command_with_deadline(cmd, Some(stop_ref), None).unwrap()
        });
        assert!(matches!(outcome, CommandOutcome::Stopped), "{outcome:?}");
    }

    #[test]
    fn finished_child_returns_output() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo out; echo err 1>&2"]);
        match run_command_with_deadline(cmd, None, None).unwrap() {
            CommandOutcome::Finished {
                status,
                stdout,
                stderr,
            } => {
                assert!(status.success());
                assert_eq!(String::from_utf8_lossy(&stdout).trim(), "out");
                assert_eq!(String::from_utf8_lossy(&stderr).trim(), "err");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn failure_classification_and_error_codes() {
        assert_eq!(
            classify_ytdlp_failure(
                "ERROR: Unable to download webpage: HTTP Error 403: Forbidden (Cloudflare)"
            ),
            Some(ERROR_CODE_NEEDS_BROWSER_COOKIES)
        );
        assert_eq!(
            classify_ytdlp_failure("<title>Just a moment...</title>"),
            Some(ERROR_CODE_NEEDS_BROWSER_COOKIES)
        );
        assert_eq!(
            classify_ytdlp_failure("ERROR: [youtube] abc: Sign in to confirm you’re not a bot"),
            Some(ERROR_CODE_NEEDS_BROWSER_COOKIES)
        );
        assert_eq!(
            classify_ytdlp_failure(
                "WARNING: [youtube] No supported JS runtime found; n challenge solving failed"
            ),
            Some(ERROR_CODE_NEEDS_JS_RUNTIME)
        );
        assert_eq!(
            classify_ytdlp_failure("ERROR: Unsupported URL: https://x"),
            Some(ERROR_CODE_UNSUPPORTED_URL)
        );
        assert_eq!(classify_ytdlp_failure("something else"), None);
        let tagged = tag_error(ERROR_CODE_TIMEOUT, "yt-dlp probe timed out after 45s");
        assert_eq!(error_code(&tagged), Some(ERROR_CODE_TIMEOUT));
        assert_eq!(error_code("[Not A Code] x"), None);
        assert_eq!(error_code("plain"), None);
        assert!(error_code_hint(ERROR_CODE_NEEDS_BROWSER_COOKIES)
            .unwrap()
            .contains("Cookies"));
    }

    /// L-19: a bare 403 or a path that happens to contain `deno` is not evidence of a
    /// cookie wall or a missing JS runtime, and common geo wording was missed.
    #[test]
    fn failure_classification_needs_real_evidence() {
        assert_eq!(
            classify_ytdlp_failure(
                "ERROR: unable to download video data: HTTP Error 403: Forbidden"
            ),
            None
        );
        assert_eq!(
            classify_ytdlp_failure("ERROR: cannot write to /home/deno/Videos/clip.mp4"),
            None
        );
        for geo in [
            "ERROR: [youtube] x: The uploader has not made this video available in your country",
            "ERROR: This video is not available in your region",
            "ERROR: [site] This content is geo-restricted",
            "ERROR: Geo restriction detected; use --geo-bypass",
        ] {
            assert_eq!(
                classify_ytdlp_failure(geo),
                Some(ERROR_CODE_GEO_BLOCKED),
                "{geo}"
            );
        }
    }

    /// M-26: a missing binary used to be an untagged "failed to spawn ... No such file".
    #[test]
    fn missing_binary_is_tagged_toolchain_missing() {
        let mut cmd = Command::new("/nonexistent/phlegethon/yt-dlp");
        let err =
            run_command_with_deadline_polling(&mut cmd, None, None, Duration::from_millis(10))
                .unwrap_err();
        assert_eq!(
            error_code(&err),
            Some(ERROR_CODE_TOOLCHAIN_MISSING),
            "{err}"
        );
        assert!(error_code_hint(ERROR_CODE_TOOLCHAIN_MISSING)
            .unwrap()
            .contains("Toolchain"));
    }

    #[cfg(unix)]
    #[test]
    fn launcher_forces_utf8_for_python() {
        let cmd = YtDlpLauncher::binary(PathBuf::from("/bin/true")).command();
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs
            .iter()
            .any(|(k, v)| *k == "PYTHONUTF8" && *v == Some(std::ffi::OsStr::new("1"))));
    }

    /// L-28: `.` and empty PATH entries mean "the current directory".
    #[cfg(unix)]
    #[test]
    fn relative_path_entries_are_ignored() {
        use std::os::unix::fs::PermissionsExt;
        struct Remove(PathBuf);
        impl Drop for Remove {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let name = format!("phlegethon-path-test-{}", std::process::id());
        let file = std::env::current_dir().unwrap().join(&name);
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        let _cleanup = Remove(file.clone());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();

        let relative = [PathBuf::from("."), PathBuf::new()];
        assert_eq!(find_in_path(&name, &relative), None);
        let absolute = [std::env::current_dir().unwrap()];
        assert_eq!(find_in_path(&name, &absolute), Some(file));
    }

    #[test]
    fn status_issues_mention_missing_tools() {
        let status = ToolchainStatus::unavailable();
        let status = ToolchainStatus {
            issues: Vec::new(),
            ..status
        }
        .with_issues();
        assert!(status.issues.iter().any(|i| i.contains("yt-dlp")));
        assert!(status.issues.iter().any(|i| i.contains("ffmpeg")));
        assert!(status.issues.iter().any(|i| i.contains("JavaScript")));
    }

    #[test]
    fn libc_detection_returns_a_value_on_linux() {
        let libc = detect_libc();
        if cfg!(target_os = "linux") {
            assert_ne!(libc, Libc::Unknown);
        }
    }

    #[cfg(unix)]
    fn read_pid_file(path: &Path) -> i32 {
        let started = Instant::now();
        loop {
            if let Some(pid) = std::fs::read_to_string(path)
                .ok()
                .and_then(|s| s.trim().parse().ok())
            {
                return pid;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "no pid file");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(unix)]
    #[test]
    fn stop_kills_grandchildren_and_returns_promptly() {
        let tmp = tempfile::tempdir().unwrap();
        let pid_file = tmp.path().join("pid");
        let script = format!("sleep 60 &\necho $! > {}\nwait", pid_file.display());
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let worker = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", &script]);
                run_command_with_deadline(cmd, Some(&stop), None).unwrap()
            })
        };
        let grandchild = read_pid_file(&pid_file);
        stop.store(true, Ordering::Release);
        let begun = Instant::now();
        let outcome = worker.join().unwrap();
        assert!(matches!(outcome, CommandOutcome::Stopped), "{outcome:?}");
        assert!(begun.elapsed() < Duration::from_secs(5));
        assert!(wait_until_process_gone(grandchild, Duration::from_secs(5)));
    }

    #[cfg(unix)]
    #[test]
    fn deadline_kills_grandchildren() {
        let tmp = tempfile::tempdir().unwrap();
        let pid_file = tmp.path().join("pid");
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            &format!("sleep 60 &\necho $! > {}\nwait", pid_file.display()),
        ]);
        let deadline = Instant::now() + Duration::from_millis(500);
        let outcome = run_command_with_deadline(cmd, None, Some(deadline)).unwrap();
        assert!(matches!(outcome, CommandOutcome::TimedOut), "{outcome:?}");
        assert!(wait_until_process_gone(
            read_pid_file(&pid_file),
            Duration::from_secs(5)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_descendant_holding_the_pipes_does_not_stall_a_finished_command() {
        let tmp = tempfile::tempdir().unwrap();
        let pid_file = tmp.path().join("pid");
        let mut cmd = Command::new("sh");
        // The shell exits at once; the background sleep keeps stdout and stderr open.
        cmd.args([
            "-c",
            &format!("sleep 60 &\necho $! > {}\necho done", pid_file.display()),
        ]);
        let begun = Instant::now();
        let outcome = run_command_with_deadline(cmd, None, None).unwrap();
        assert!(
            begun.elapsed() < Duration::from_secs(10),
            "stalled on the pipes"
        );
        match outcome {
            CommandOutcome::Finished { stdout, .. } => {
                assert_eq!(String::from_utf8_lossy(&stdout).trim(), "done");
            }
            other => panic!("{other:?}"),
        }
        assert!(wait_until_process_gone(
            read_pid_file(&pid_file),
            Duration::from_secs(5)
        ));
    }
}
