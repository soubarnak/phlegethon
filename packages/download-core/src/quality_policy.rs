//! Default Quality Policy — settings-backed pre-selection for probe qualities.
//!
//! Default: best resolution ≤ 1080p, prefer mp4 container. Audio-only mode
//! overrides the video policy and picks the best audio format instead.

use crate::extractor::Quality;
use serde::{Deserialize, Serialize};

/// Default maximum video height for the Default Quality Policy.
pub const DEFAULT_MAX_HEIGHT: u32 = 1080;

/// Settings-backed rule for pre-selecting probe qualities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultQualityPolicy {
    /// Maximum video height in pixels (default 1080).
    pub max_height: u32,
    /// Prefer mp4 container when multiple formats share the same resolution.
    pub prefer_mp4: bool,
}

impl Default for DefaultQualityPolicy {
    fn default() -> Self {
        Self {
            max_height: DEFAULT_MAX_HEIGHT,
            prefer_mp4: true,
        }
    }
}

/// Select the default quality id from `qualities` using the active policy.
///
/// Returns `None` when `qualities` is empty.
pub fn select_default_quality(
    qualities: &[Quality],
    policy: &DefaultQualityPolicy,
    audio_only_mode: bool,
) -> Option<String> {
    if qualities.is_empty() {
        return None;
    }

    if audio_only_mode {
        return select_best_audio(qualities);
    }

    select_best_video(qualities, policy)
}

fn select_best_audio(qualities: &[Quality]) -> Option<String> {
    let mut best: Option<(u64, &Quality)> = None;

    for quality in qualities {
        if !is_audio_quality(quality) {
            continue;
        }
        let score = audio_bitrate_score(quality);
        match &best {
            None => best = Some((score, quality)),
            Some((best_score, _)) if score > *best_score => {
                best = Some((score, quality));
            }
            _ => {}
        }
    }

    if let Some((_, quality)) = best {
        return Some(quality.id.clone());
    }

    qualities.first().map(|q| q.id.clone())
}

fn select_best_video(qualities: &[Quality], policy: &DefaultQualityPolicy) -> Option<String> {
    let mut best: Option<(u32, bool, &Quality)> = None;

    for quality in qualities {
        if is_audio_quality(quality) || is_video_only_quality(quality) {
            continue;
        }

        let Some(height) = parse_video_height(&quality.id) else {
            continue;
        };

        if height > policy.max_height {
            continue;
        }

        let is_mp4 = is_mp4_container(quality);
        let replace = match &best {
            None => true,
            Some((best_height, best_mp4, _)) => {
                height > *best_height
                    || (height == *best_height
                        && policy.prefer_mp4
                        && is_mp4
                        && !*best_mp4)
            }
        };

        if replace {
            best = Some((height, is_mp4, quality));
        }
    }

    if let Some((_, _, quality)) = best {
        return Some(quality.id.clone());
    }

    // Fallback: "best" or first non-audio quality at or below cap when height is unknown.
    for quality in qualities {
        if is_audio_quality(quality) {
            continue;
        }
        if quality.id.eq_ignore_ascii_case("best") {
            return Some(quality.id.clone());
        }
    }

    qualities
        .iter()
        .find(|q| !is_audio_quality(q))
        .map(|q| q.id.clone())
        .or_else(|| qualities.first().map(|q| q.id.clone()))
}

fn is_audio_quality(quality: &Quality) -> bool {
    quality.id.ends_with("-audio")
        || quality.id.contains("k-audio")
        || quality.label.contains("(audio only)")
}

fn is_video_only_quality(quality: &Quality) -> bool {
    quality.id.ends_with("-videoonly") || quality.label.contains("(video only)")
}

fn is_mp4_container(quality: &Quality) -> bool {
    quality
        .container
        .as_deref()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"))
}

fn parse_video_height(id: &str) -> Option<u32> {
    let base = id.strip_suffix("-videoonly").unwrap_or(id);
    let digits = base.strip_suffix('p')?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn audio_bitrate_score(quality: &Quality) -> u64 {
    if let Some(stem) = quality.id.strip_suffix("-audio") {
        if let Some(kbps) = stem.strip_suffix('k').and_then(|n| n.parse::<u64>().ok()) {
            return kbps;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(id: &str, label: &str) -> Quality {
        Quality {
            id: id.to_string(),
            label: label.to_string(),
            size_bytes: None,
            container: None,
        }
    }

    fn q_mp4(id: &str, label: &str) -> Quality {
        Quality {
            id: id.to_string(),
            label: label.to_string(),
            size_bytes: None,
            container: Some("mp4".to_string()),
        }
    }

    fn q_webm(id: &str, label: &str) -> Quality {
        Quality {
            id: id.to_string(),
            label: label.to_string(),
            size_bytes: None,
            container: Some("webm".to_string()),
        }
    }

    #[test]
    fn picks_best_at_or_below_max_height() {
        let qualities = vec![
            q("2160p", "2160p"),
            q("1080p", "1080p"),
            q("720p", "720p"),
        ];
        let selected = select_default_quality(&qualities, &DefaultQualityPolicy::default(), false);
        assert_eq!(selected.as_deref(), Some("1080p"));
    }

    #[test]
    fn prefers_mp4_at_same_resolution() {
        let qualities = vec![q_webm("1080p", "1080p"), q_mp4("1080p", "1080p")];
        let selected = select_default_quality(&qualities, &DefaultQualityPolicy::default(), false);
        assert_eq!(selected.as_deref(), Some("1080p"));
        assert!(qualities
            .iter()
            .find(|q| q.id == "1080p" && q.container.as_deref() == Some("mp4"))
            .is_some());
    }

    #[test]
    fn audio_only_mode_picks_best_audio() {
        let qualities = vec![
            q("1080p", "1080p"),
            q("130k-audio", "medium (audio only)"),
            q("256k-audio", "high (audio only)"),
        ];
        let selected = select_default_quality(&qualities, &DefaultQualityPolicy::default(), true);
        assert_eq!(selected.as_deref(), Some("256k-audio"));
    }

    #[test]
    fn skips_video_only_for_default_video_policy() {
        let qualities = vec![
            q("1080p-videoonly", "1080p (video only)"),
            q("720p", "720p"),
        ];
        let selected = select_default_quality(&qualities, &DefaultQualityPolicy::default(), false);
        assert_eq!(selected.as_deref(), Some("720p"));
    }

    #[test]
    fn empty_qualities_returns_none() {
        assert_eq!(
            select_default_quality(&[], &DefaultQualityPolicy::default(), false),
            None
        );
    }
}
