//! Media extras defaults — write/embed subtitles, chapters, thumbnails, metadata.
//!
//! Embed options require ffmpeg. Post-processing is remux + embed only (copy
//! streams, no re-encode).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

/// User-facing toggles for optional media sidecars and embedding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

impl Default for MediaExtrasOptions {
    fn default() -> Self {
        Self {
            write_subs: false,
            embed_subs: false,
            write_chapters: false,
            embed_chapters: false,
            write_thumbnail: false,
            embed_thumbnail: false,
            write_metadata: false,
            embed_metadata: false,
            remux: false,
        }
    }
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

    let status = Command::new(&ffmpeg)
        .arg("-y")
        .arg("-i")
        .arg(input)
        .arg("-c")
        .arg("copy")
        .arg("-movflags")
        .arg("+faststart")
        .arg(&output)
        .status();

    match status {
        Ok(s) if s.success() && output.is_file() => PostprocessOutcome::Remuxed(output),
        Ok(s) => PostprocessOutcome::Failed {
            error: format!("ffmpeg remux failed (exit {:?})", s.code()),
            pre_remux_path: input.to_path_buf(),
        },
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
}
