#!/usr/bin/env python3
"""Pure HTML parsers for Phlegethon Launch Site Extractor Plugins.

No yt-dlp import — CI tests these against HTML fixtures without network.
"""

from __future__ import annotations

import re
from typing import Any
from urllib.parse import urljoin


_HEIGHT_RE = re.compile(r'(\d{3,4})p', re.IGNORECASE)
_HEIGHT_IN_URL_RE = re.compile(
    r'(?:_|/|-)(?P<h>\d{3,4})(?:p)?\.(?:mp4|m3u8|webm)\b',
    re.IGNORECASE,
)
_SOURCE_TAG_RE = re.compile(
    r'<source[^>]+src=["\'](?P<url>[^"\']+)["\'][^>]*(?:type=["\'](?P<type>[^"\']+)["\'])?',
    re.IGNORECASE,
)
_VIDEO_SRC_RE = re.compile(
    r'<video[^>]+src=["\'](?P<url>[^"\']+)["\']',
    re.IGNORECASE,
)
_FLASHVARS_RE = re.compile(
    r'(?:flashvars|video_url|video_alt_url(?:\d+)?|file)\s*[:=]\s*["\'](?P<url>https?://[^"\']+\.(?:mp4|m3u8)[^"\']*)["\']',
    re.IGNORECASE,
)
_JW_FILE_RE = re.compile(
    r'''["']file["']\s*:\s*["'](?P<url>https?://[^"']+)["']''',
    re.IGNORECASE,
)
_LABEL_RE = re.compile(
    r'''["']label["']\s*:\s*["'](?P<label>[^"']+)["']''',
    re.IGNORECASE,
)
_OG_TITLE_RE = re.compile(
    r'''<meta[^>]+property=["']og:title["'][^>]+content=["'](?P<title>[^"']+)["']''',
    re.IGNORECASE,
)
_TITLE_TAG_RE = re.compile(r'<title[^>]*>(?P<title>[^<]+)</title>', re.IGNORECASE)
_OG_THUMB_RE = re.compile(
    r'''<meta[^>]+property=["']og:image["'][^>]+content=["'](?P<url>[^"']+)["']''',
    re.IGNORECASE,
)


def _abs(page_url: str, media_url: str) -> str:
    return urljoin(page_url, media_url)


def _height_from_label(label: str | None, url: str) -> int | None:
    for candidate in (label or '', url):
        m = _HEIGHT_RE.search(candidate)
        if m:
            return int(m.group(1))
    m = _HEIGHT_IN_URL_RE.search(url)
    if m:
        return int(m.group('h'))
    return None


def _format_entry(
    url: str,
    *,
    height: int | None = None,
    format_id: str | None = None,
    label: str | None = None,
) -> dict[str, Any]:
    h = height if height is not None else _height_from_label(label, url)
    fid = format_id
    if not fid:
        fid = f'{h}p' if h else 'default'
    note = label or (f'{h}p' if h else fid)
    entry: dict[str, Any] = {
        'url': url,
        'format_id': fid,
        'format_note': note,
        'ext': 'mp4' if '.m3u8' not in url.lower() else 'mp4',
        'vcodec': 'avc1',
        'acodec': 'mp4a',
        'http_headers': {},
    }
    if h is not None:
        entry['height'] = h
        entry['resolution'] = f'{int(h * 16 / 9)}x{h}' if h else None
    if '.m3u8' in url.lower():
        entry['protocol'] = 'm3u8_native'
        entry['format_id'] = f'hls-{fid}'
    return entry


def extract_formats(webpage: str, page_url: str) -> list[dict[str, Any]]:
    """Collect progressive / HLS media URLs from a video page HTML fixture."""
    formats: list[dict[str, Any]] = []
    seen: set[str] = set()

    def add(url: str, **kwargs: Any) -> None:
        abs_url = _abs(page_url, url.strip())
        if not abs_url.startswith('http') or abs_url in seen:
            return
        seen.add(abs_url)
        formats.append(_format_entry(abs_url, **kwargs))

    for m in _SOURCE_TAG_RE.finditer(webpage):
        add(m.group('url'))

    for m in _VIDEO_SRC_RE.finditer(webpage):
        add(m.group('url'))

    for m in _FLASHVARS_RE.finditer(webpage):
        add(m.group('url'))

    # JWPlayer-style sources: {file: "...", label: "720p"}
    for block in re.finditer(r'\{[^{}]*["\']file["\'][^{}]*\}', webpage, re.IGNORECASE):
        chunk = block.group(0)
        file_m = _JW_FILE_RE.search(chunk)
        if not file_m:
            continue
        label_m = _LABEL_RE.search(chunk)
        label = label_m.group('label') if label_m else None
        add(file_m.group('url'), label=label, height=_height_from_label(label, file_m.group('url')))

    # Bare m3u8 / mp4 URLs in scripts
    for m in re.finditer(
        r'''["'](https?://[^"']+\.(?:mp4|m3u8)[^"']*)["']''',
        webpage,
        re.IGNORECASE,
    ):
        add(m.group(1))

    return formats


def extract_title(webpage: str) -> str | None:
    m = _OG_TITLE_RE.search(webpage)
    if m:
        return m.group('title').strip()
    m = _TITLE_TAG_RE.search(webpage)
    if m:
        return m.group('title').strip()
    return None


def extract_thumbnail(webpage: str) -> str | None:
    m = _OG_THUMB_RE.search(webpage)
    return m.group('url').strip() if m else None


def extract_duration(webpage: str) -> int | None:
    m = re.search(
        r'''property=["']og:duration["'][^>]+content=["'](?P<dur>\d+)["']''',
        webpage,
        re.IGNORECASE,
    )
    if m:
        return int(m.group('dur'))
    m = re.search(
        r'''["']duration["']\s*:\s*(?P<dur>\d+)''',
        webpage,
        re.IGNORECASE,
    )
    if m:
        return int(m.group('dur'))
    return None


def build_info_dict(
    *,
    video_id: str,
    page_url: str,
    webpage: str,
    display_id: str | None = None,
) -> dict[str, Any]:
    formats = extract_formats(webpage, page_url)
    if not formats:
        raise ValueError(f'no media formats found for {page_url}')

    title = extract_title(webpage) or display_id or video_id
    info: dict[str, Any] = {
        'id': video_id,
        'title': title,
        'formats': formats,
        'age_limit': 18,
        'webpage_url': page_url,
    }
    if display_id:
        info['display_id'] = display_id
    thumb = extract_thumbnail(webpage)
    if thumb:
        info['thumbnail'] = thumb
    duration = extract_duration(webpage)
    if duration is not None:
        info['duration'] = duration
    # Prefer progressive formats; expose http referer for CDN checks.
    for fmt in info['formats']:
        fmt.setdefault('http_headers', {})['Referer'] = page_url
    return info
