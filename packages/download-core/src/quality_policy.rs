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
    // Video-only qualities count: the downloader merges the best audio into them.
    // Ranking at equal height: combined first (no merge needed), then mp4.
    let rank = |q: &Quality| {
        (
            !is_video_only_quality(q),
            policy.prefer_mp4 && is_mp4_container(q),
        )
    };
    let candidates: Vec<(u32, &Quality)> = qualities
        .iter()
        .filter(|q| !is_audio_quality(q))
        .filter_map(|q| parse_video_height(&q.id).map(|h| (h, q)))
        .collect();

    let under_cap = candidates
        .iter()
        .filter(|(h, _)| *h <= policy.max_height)
        .max_by(|(ha, a), (hb, b)| ha.cmp(hb).then(rank(a).cmp(&rank(b))));
    if let Some((_, quality)) = under_cap {
        return Some(quality.id.clone());
    }

    // Everything is above the cap: take the smallest one rather than an arbitrary first entry.
    if let Some((_, quality)) = candidates
        .iter()
        .min_by(|(ha, a), (hb, b)| ha.cmp(hb).then(rank(b).cmp(&rank(a))))
    {
        return Some(quality.id.clone());
    }

    // Heights unknown: "best", then the first non-audio quality.
    for quality in qualities {
        if !is_audio_quality(quality) && quality.id.eq_ignore_ascii_case("best") {
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
        let qualities = vec![q("2160p", "2160p"), q("1080p", "1080p"), q("720p", "720p")];
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
    fn video_only_counts_because_audio_is_merged_in() {
        let qualities = vec![q("1080p-videoonly", "1080p"), q("720p", "720p")];
        let selected = select_default_quality(&qualities, &DefaultQualityPolicy::default(), false);
        assert_eq!(selected.as_deref(), Some("1080p-videoonly"));
    }

    #[test]
    fn combined_wins_over_video_only_at_equal_height() {
        let qualities = vec![q("720p-videoonly", "720p"), q("720p", "720p")];
        let selected = select_default_quality(&qualities, &DefaultQualityPolicy::default(), false);
        assert_eq!(selected.as_deref(), Some("720p"));
    }

    /// The live YouTube result: combined only at 360p, the rest video-only.
    #[test]
    fn youtube_like_set_under_a_low_cap_picks_the_tallest_fitting_stream() {
        let qualities = vec![
            q("144p-videoonly", "144p"),
            q("240p-videoonly", "240p"),
            q("360p", "360p"),
            q("130k-audio", "medium (audio only)"),
        ];
        let policy = DefaultQualityPolicy {
            max_height: 240,
            prefer_mp4: true,
        };
        assert_eq!(
            select_default_quality(&qualities, &policy, false).as_deref(),
            Some("240p-videoonly")
        );
    }

    #[test]
    fn everything_above_the_cap_picks_the_smallest() {
        let qualities = vec![
            q("1080p", "1080p"),
            q("480p", "480p"),
            q("720p-videoonly", "720p"),
        ];
        let policy = DefaultQualityPolicy {
            max_height: 240,
            prefer_mp4: true,
        };
        assert_eq!(
            select_default_quality(&qualities, &policy, false).as_deref(),
            Some("480p")
        );
    }

    #[test]
    fn empty_qualities_returns_none() {
        assert_eq!(
            select_default_quality(&[], &DefaultQualityPolicy::default(), false),
            None
        );
    }
}
