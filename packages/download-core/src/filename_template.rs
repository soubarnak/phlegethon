//! Filename Template — settings-backed naming for extractor job outputs.
//!
//! Supports a yt-dlp-like variable subset: `title`, `id`, `uploader`,
//! `upload_date`, `ext`. Plain HTTP jobs keep URL-derived names.

use std::path::Path;

/// Default output template (matches legacy yt-dlp behavior).
pub const DEFAULT_FILENAME_TEMPLATE: &str = "%(title).200B [%(id)s].%(ext)s";

/// Variables available when rendering a filename template for preview / collision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateVars<'a> {
    pub title: &'a str,
    pub id: &'a str,
    pub uploader: Option<&'a str>,
    pub upload_date: Option<&'a str>,
    pub ext: &'a str,
}

/// Strip the probe prefix (`yt-`, `xh-`, …) from a MediaItem id for `%(id)s`.
pub fn media_item_raw_id(prefixed_id: &str) -> &str {
    prefixed_id
        .split_once('-')
        .map(|(_, raw)| raw)
        .unwrap_or(prefixed_id)
}

/// Render a template with known variable values (for collision checks and tests).
///
/// Supports `%(name)s` and `%(name).Nb` truncation (byte limit on `title`).
pub fn render_template(template: &str, vars: &TemplateVars<'_>) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 1 < bytes.len() && bytes[i + 1] == b'(' {
            if let Some((value, next)) = parse_field(&template[i..], vars) {
                out.push_str(&value);
                i += next;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    sanitize_filename(&out)
}

/// Replace characters unsafe on common filesystems.
pub fn sanitize_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c == '/' || c == '\\' || c == ':' || c == '*' || c == '?' || c == '"' || c == '<'
            || c == '>' || c == '|'
        {
            out.push('_');
        } else if c.is_control() {
            out.push('_');
        } else {
            out.push(c);
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        "download".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Pick a non-colliding filename under `dest_dir`.
///
/// Completed files are never overwritten; partial/temp siblings may be replaced.
/// When `stem_guard` is true, any completed file sharing the same stem (any
/// extension) counts as a collision — used when the real extension is chosen
/// later by yt-dlp.
pub fn resolve_collision(dest_dir: &Path, filename: &str) -> String {
    resolve_collision_inner(dest_dir, filename, false)
}

fn resolve_collision_inner(dest_dir: &Path, filename: &str, stem_guard: bool) -> String {
    let initial = dest_dir.join(filename);
    if !initial.exists() || may_replace_existing(&initial) {
        if stem_guard {
            let (stem, _) = split_extension(filename);
            if completed_stem_exists(dest_dir, stem) {
                return next_suffixed_name(dest_dir, filename);
            }
        }
        return filename.to_string();
    }

    next_suffixed_name(dest_dir, filename)
}

fn next_suffixed_name(dest_dir: &Path, filename: &str) -> String {
    let (stem, ext) = split_extension(filename);
    for n in 1..10_000 {
        let candidate = if ext.is_empty() {
            format!("{stem} ({n})")
        } else {
            format!("{stem} ({n}).{ext}")
        };
        let path = dest_dir.join(&candidate);
        if !path.exists() || may_replace_existing(&path) {
            return candidate;
        }
    }
    format!("{stem} ({})", uuid::Uuid::new_v4())
}

fn completed_stem_exists(dest_dir: &Path, stem: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dest_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if may_replace_existing(&path) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let file_stem = split_extension(name).0;
        if stems_match(file_stem, stem) || file_stem.starts_with(&format!("{stem} (")) {
            return true;
        }
    }
    false
}

fn stems_match(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Prepare yt-dlp `-o` output (relative to `dest_dir`) and expected basename.
///
/// When no collision occurs, returns the user template so yt-dlp can resolve
/// `%(ext)s` from the downloaded format. When a completed file would be
/// overwritten, returns a fixed basename with a numeric suffix.
pub fn prepare_extractor_output(
    dest_dir: &Path,
    template: &str,
    vars: &TemplateVars<'_>,
) -> (String, String) {
    let rendered = render_template(template, vars);
    let stem_guard = template.contains("%(ext)");
    let final_name = resolve_collision_inner(dest_dir, &rendered, stem_guard);
    if final_name == rendered {
        (template.to_string(), final_name)
    } else {
        (final_name.clone(), final_name)
    }
}

fn may_replace_existing(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return true;
    };
    if !meta.is_file() {
        return false;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    name.ends_with(".part")
        || name.ends_with(".ytdl")
        || name.ends_with(".temp")
        || name.ends_with(".tmp")
}

fn split_extension(filename: &str) -> (&str, &str) {
    match filename.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && !ext.contains('/') => {
            (stem, ext)
        }
        _ => (filename, ""),
    }
}

fn parse_field(input: &str, vars: &TemplateVars<'_>) -> Option<(String, usize)> {
    let rest = input.strip_prefix("%(")?;
    let (field_name, after_paren) = rest.split_once(')')?;
    let mut consumed = 2 + field_name.len() + 1;

    let mut truncate_bytes: Option<usize> = None;
    let mut tail = after_paren;
    if let Some(stripped) = tail.strip_prefix('.') {
        if let Some((num, remainder)) = stripped.split_once('B') {
            if let Ok(n) = num.parse::<usize>() {
                truncate_bytes = Some(n);
                consumed += 1 + num.len() + 1;
                tail = remainder;
            }
        }
    }

    let has_s_suffix = tail.starts_with('s');
    if has_s_suffix {
        consumed += 1;
    } else if truncate_bytes.is_none() {
        return None;
    }

    let value = match field_name {
        "title" => match truncate_bytes {
            Some(n) => truncate_bytes_str(vars.title, n),
            None => vars.title.to_string(),
        },
        "id" => vars.id.to_string(),
        "uploader" => vars
            .uploader
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown")
            .to_string(),
        "upload_date" => vars
            .upload_date
            .filter(|s| !s.is_empty())
            .unwrap_or("00000000")
            .to_string(),
        "ext" => vars.ext.to_string(),
        other => {
            let mut placeholder = format!("%({other})");
            if let Some(n) = truncate_bytes {
                placeholder.push_str(&format!(".{n}B"));
            }
            if has_s_suffix {
                placeholder.push('s');
            }
            return Some((placeholder, consumed));
        }
    };

    Some((value, consumed))
}

fn truncate_bytes_str(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sample_vars<'a>() -> TemplateVars<'a> {
        TemplateVars {
            title: "Never Gonna Give You Up",
            id: "dQw4w9WgXcQ",
            uploader: Some("Rick Astley"),
            upload_date: Some("20091025"),
            ext: "mp4",
        }
    }

    #[test]
    fn render_default_template() {
        let vars = sample_vars();
        let out = render_template(DEFAULT_FILENAME_TEMPLATE, &vars);
        assert!(out.contains("Never Gonna Give You Up"));
        assert!(out.ends_with("[dQw4w9WgXcQ].mp4"));
    }

    #[test]
    fn render_all_variables() {
        let vars = sample_vars();
        let out = render_template(
            "%(title)s-%(id)s-%(uploader)s-%(upload_date)s.%(ext)s",
            &vars,
        );
        assert_eq!(
            out,
            "Never Gonna Give You Up-dQw4w9WgXcQ-Rick Astley-20091025.mp4"
        );
    }

    #[test]
    fn render_truncates_title_bytes() {
        let vars = TemplateVars {
            title: "abcdefghij",
            id: "id1",
            uploader: None,
            upload_date: None,
            ext: "mp4",
        };
        let out = render_template("%(title).5B [%(id)s].%(ext)s", &vars);
        assert_eq!(out, "abcde [id1].mp4");
    }

    #[test]
    fn sanitize_replaces_path_separators() {
        assert_eq!(sanitize_filename("a/b:c"), "a_b_c");
    }

    #[test]
    fn media_item_raw_id_strips_prefix() {
        assert_eq!(media_item_raw_id("yt-dQw4w9WgXcQ"), "dQw4w9WgXcQ");
        assert_eq!(media_item_raw_id("plain"), "plain");
    }

    #[test]
    fn collision_adds_numeric_suffix() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path();
        fs::write(dest.join("clip.mp4"), b"done").unwrap();

        let resolved = resolve_collision(dest, "clip.mp4");
        assert_eq!(resolved, "clip (1).mp4");
    }

    #[test]
    fn collision_skips_completed_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path();
        fs::write(dest.join("clip.mp4"), b"done").unwrap();
        fs::write(dest.join("clip (1).mp4"), b"also done").unwrap();

        let resolved = resolve_collision(dest, "clip.mp4");
        assert_eq!(resolved, "clip (2).mp4");
    }

    #[test]
    fn collision_may_replace_part_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path();
        fs::write(dest.join("clip.mp4.part"), b"partial").unwrap();

        let resolved = resolve_collision(dest, "clip.mp4");
        assert_eq!(resolved, "clip.mp4");
    }

    #[test]
    fn prepare_extractor_output_uses_template_when_no_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let vars = sample_vars();
        let (ytdlp, basename) =
            prepare_extractor_output(tmp.path(), DEFAULT_FILENAME_TEMPLATE, &vars);
        assert_eq!(ytdlp, DEFAULT_FILENAME_TEMPLATE);
        assert!(basename.contains("dQw4w9WgXcQ"));
    }

    #[test]
    fn prepare_extractor_output_fixes_name_on_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let vars = sample_vars();
        let rendered = render_template(DEFAULT_FILENAME_TEMPLATE, &vars);
        fs::write(tmp.path().join(&rendered), b"existing").unwrap();

        let (ytdlp, basename) =
            prepare_extractor_output(tmp.path(), DEFAULT_FILENAME_TEMPLATE, &vars);
        assert_eq!(ytdlp, basename);
        assert!(basename.contains("(1)"));
    }

    #[test]
    fn stem_guard_detects_different_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path();
        fs::write(dest.join("clip.webm"), b"done").unwrap();

        let vars = TemplateVars {
            title: "Clip",
            id: "abc",
            uploader: None,
            upload_date: None,
            ext: "mp4",
        };
        let (_, basename) = prepare_extractor_output(dest, "%(title)s.%(ext)s", &vars);
        assert_eq!(basename, "Clip (1).mp4");
    }
}
