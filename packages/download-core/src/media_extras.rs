//! Media extras defaults — write/embed subtitles, chapters, thumbnails, metadata.
//!
//! Embed options require ffmpeg. Post-processing is remux + embed only (copy
//! streams, no re-encode).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::toolchain_discovery::{run_command_with_deadline_polling, CommandOutcome};

/// User-facing toggles for optional media sidecars and embedding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaExtrasOptions {
    pub write_subs: bool,
    pub embed_subs: bool,
    pub write_chapters: bool,
    pub embed_chapters: bool,
    pub write_thumbnail: bool,
    pub embed_thumbnail: bool,
    pub write_metadata: bool,
    pub embed_metadata: bool,
    /// Remux to mp4 without re-encoding when the container differs.
    pub remux: bool,
}

impl MediaExtrasOptions {
    /// Whether any write-sidecar option is enabled.
    pub fn any_write(&self) -> bool {
        self.write_subs || self.write_chapters || self.write_thumbnail || self.write_metadata
    }

    /// Whether any embed/remux post-process option is enabled.
    pub fn any_embed_or_remux(&self) -> bool {
        self.embed_subs
            || self.embed_chapters
            || self.embed_thumbnail
            || self.embed_metadata
            || self.remux
    }
}

/// Capabilities exposed to shells for disabling embed toggles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MediaExtrasCapabilities {
    pub ffmpeg_available: bool,
    /// When embed options are unavailable, a short reason for the UI.
    pub embed_disabled_reason: Option<String>,
}

impl MediaExtrasCapabilities {
    pub fn unavailable() -> Self {
        Self {
            ffmpeg_available: false,
            embed_disabled_reason: Some(
                "ffmpeg not found — embed and remux options are disabled".into(),
            ),
        }
    }
}

/// Strip embed/remux flags when ffmpeg is unavailable; write-sidecar stays.
pub fn effective_options(
    options: &MediaExtrasOptions,
    caps: &MediaExtrasCapabilities,
) -> MediaExtrasOptions {
    if caps.ffmpeg_available {
        return *options;
    }
    MediaExtrasOptions {
        embed_subs: false,
        embed_chapters: false,
        embed_thumbnail: false,
        embed_metadata: false,
        remux: false,
        ..*options
    }
}

/// Resolve ffmpeg binary path from an optional directory or PATH.
pub fn resolve_ffmpeg(ffmpeg_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = ffmpeg_dir {
        for name in ["ffmpeg", "ffmpeg.exe"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        return None;
    }
    if command_exists("ffmpeg") {
        Some(PathBuf::from("ffmpeg"))
    } else {
        None
    }
}

/// Whether ffmpeg is available for embed/remux post-processing.
pub fn ffmpeg_available(ffmpeg_dir: Option<&Path>) -> bool {
    resolve_ffmpeg(ffmpeg_dir).is_some()
}

fn command_exists(name: &str) -> bool {
    Command::new(name)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Outcome of remux/embed post-processing after the primary download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostprocessOutcome {
    /// No post-process ran; path unchanged.
    Unchanged(PathBuf),
    /// Remux succeeded; returns final path.
    Remuxed(PathBuf),
    /// The stop flag ended the remux (Pause or Discard); the partial output is removed and
    /// the pre-remux file stays where it was.
    Stopped,
    /// Remux failed; pre-remux file retained at `pre_remux_path`.
    Failed {
        error: String,
        pre_remux_path: PathBuf,
    },
}

/// Apply remux-only post-processing (copy streams, no re-encode).
///
/// Embed flags are handled by yt-dlp during download; this step only remuxes
/// when requested and the container is not already mp4.
pub fn apply_remux_postprocess(
    input: &Path,
    options: &MediaExtrasOptions,
    ffmpeg_dir: Option<&Path>,
    stop: &AtomicBool,
) -> PostprocessOutcome {
    if !options.remux {
        return PostprocessOutcome::Unchanged(input.to_path_buf());
    }

    if input
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"))
    {
        return PostprocessOutcome::Unchanged(input.to_path_buf());
    }

    let Some(ffmpeg) = resolve_ffmpeg(ffmpeg_dir) else {
        return PostprocessOutcome::Failed {
            error: "remux requested but ffmpeg is not available".into(),
            pre_remux_path: input.to_path_buf(),
        };
    };

    if stop.load(Ordering::Acquire) {
        return PostprocessOutcome::Unchanged(input.to_path_buf());
    }

    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let output = input.with_file_name(format!("{stem}.remux.mp4"));

    let mut cmd = Command::new(&ffmpeg);
    cmd.arg("-nostdin")
        .args(["-loglevel", "error", "-y", "-i"])
        .arg(input)
        .args(["-c", "copy", "-movflags", "+faststart"])
        .arg(&output);
    let outcome =
        run_command_with_deadline_polling(&mut cmd, Some(stop), None, Duration::from_millis(50));

    match outcome {
        Ok(CommandOutcome::Finished { status, .. }) if status.success() && output.is_file() => {
            PostprocessOutcome::Remuxed(output)
        }
        Ok(CommandOutcome::Finished { status, .. }) => {
            let _ = std::fs::remove_file(&output);
            PostprocessOutcome::Failed {
                error: format!("ffmpeg remux failed (exit {:?})", status.code()),
                pre_remux_path: input.to_path_buf(),
            }
        }
        Ok(CommandOutcome::Stopped | CommandOutcome::TimedOut) => {
            let _ = std::fs::remove_file(&output);
            PostprocessOutcome::Stopped
        }
        Err(e) => PostprocessOutcome::Failed {
            error: format!("failed to run ffmpeg remux: {e}"),
            pre_remux_path: input.to_path_buf(),
        },
    }
}

/// Build yt-dlp CLI args for media extras (write + embed when ffmpeg present).
pub fn ytdlp_media_extras_args(
    options: &MediaExtrasOptions,
    ffmpeg_available: bool,
) -> Vec<String> {
    let mut args = Vec::new();

    if options.write_subs {
        args.push("--write-subs".into());
        args.push("--write-auto-subs".into());
    }
    if options.write_thumbnail {
        args.push("--write-thumbnail".into());
    }
    if options.write_metadata {
        args.push("--write-info-json".into());
        args.push("--write-description".into());
    }
    if options.write_chapters {
        args.push("--write-info-json".into());
    }

    if ffmpeg_available {
        if options.embed_subs {
            args.push("--embed-subs".into());
        }
        if options.embed_thumbnail {
            args.push("--embed-thumbnail".into());
        }
        if options.embed_metadata {
            args.push("--embed-metadata".into());
        }
        if options.embed_chapters {
            args.push("--embed-chapters".into());
        }
        if options.remux {
            args.push("--remux-video".into());
            args.push("mp4".into());
        }
    }

    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_options_strips_embed_when_ffmpeg_missing() {
        let opts = MediaExtrasOptions {
            write_subs: true,
            embed_subs: true,
            remux: true,
            ..Default::default()
        };
        let effective = effective_options(&opts, &MediaExtrasCapabilities::unavailable());
        assert!(effective.write_subs);
        assert!(!effective.embed_subs);
        assert!(!effective.remux);
    }

    #[test]
    fn ytdlp_args_include_write_only_without_ffmpeg() {
        let opts = MediaExtrasOptions {
            write_subs: true,
            embed_subs: true,
            write_thumbnail: true,
            ..Default::default()
        };
        let args = ytdlp_media_extras_args(&opts, false);
        assert!(args.iter().any(|a| a == "--write-subs"));
        assert!(!args.iter().any(|a| a == "--embed-subs"));
    }

    #[test]
    fn remux_postprocess_fails_without_ffmpeg_and_retains_input() {
        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("clip.webm");
        std::fs::write(&input, b"fake webm").unwrap();
        let stop = std::sync::atomic::AtomicBool::new(false);
        let outcome = apply_remux_postprocess(
            &input,
            &MediaExtrasOptions {
                remux: true,
                ..Default::default()
            },
            Some(tmp.path()),
            &stop,
        );
        match outcome {
            PostprocessOutcome::Failed { pre_remux_path, .. } => {
                assert_eq!(pre_remux_path, input);
                assert!(input.is_file());
            }
            other => panic!("expected remux failure, got {other:?}"),
        }
    }

    #[test]
    fn ytdlp_args_include_embed_when_ffmpeg_present() {
        let opts = MediaExtrasOptions {
            embed_subs: true,
            remux: true,
            ..Default::default()
        };
        let args = ytdlp_media_extras_args(&opts, true);
        assert!(args.iter().any(|a| a == "--embed-subs"));
        assert!(args.iter().any(|a| a == "--remux-video"));
    }

    #[cfg(unix)]
    fn write_fake_ffmpeg(dir: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("ffmpeg");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    fn remux_options() -> MediaExtrasOptions {
        MediaExtrasOptions {
            remux: true,
            ..Default::default()
        }
    }

    #[cfg(unix)]
    #[test]
    fn remux_postprocess_runs_ffmpeg_and_returns_the_new_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_fake_ffmpeg(
            tmp.path(),
            "for a; do last=$a; done\necho remuxed > \"$last\"",
        );
        let input = tmp.path().join("clip.webm");
        std::fs::write(&input, b"fake webm").unwrap();
        let stop = AtomicBool::new(false);
        match apply_remux_postprocess(&input, &remux_options(), Some(tmp.path()), &stop) {
            PostprocessOutcome::Remuxed(p) => assert_eq!(p, tmp.path().join("clip.remux.mp4")),
            other => panic!("expected Remuxed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn stop_kills_the_whole_ffmpeg_process_tree_during_remux() {
        let tmp = tempfile::tempdir().unwrap();
        let pid_file = tmp.path().join("grandchild.pid");
        write_fake_ffmpeg(
            tmp.path(),
            &format!("sleep 60 &\necho $! > {}\nwait", pid_file.display()),
        );
        let input = tmp.path().join("clip.webm");
        std::fs::write(&input, b"fake webm").unwrap();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let worker = {
            let (dir, input, stop) = (tmp.path().to_path_buf(), input.clone(), stop.clone());
            std::thread::spawn(move || {
                apply_remux_postprocess(&input, &remux_options(), Some(&dir), &stop)
            })
        };
        let started = std::time::Instant::now();
        let grandchild: i32 = loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|s| s.trim().parse().ok())
            {
                break pid;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "fake ffmpeg never started"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        stop.store(true, Ordering::Release);
        let begun = std::time::Instant::now();
        assert_eq!(worker.join().unwrap(), PostprocessOutcome::Stopped);
        assert!(begun.elapsed() < Duration::from_secs(5));
        assert!(crate::toolchain_discovery::wait_until_process_gone(
            grandchild,
            Duration::from_secs(5)
        ));
        assert!(!tmp.path().join("clip.remux.mp4").exists());
        assert!(input.is_file());
    }
}
