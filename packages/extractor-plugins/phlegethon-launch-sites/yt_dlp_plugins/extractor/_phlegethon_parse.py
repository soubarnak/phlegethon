#!/usr/bin/env python3
"""Pure HTML parsers for Phlegethon Launch Site Extractor Plugins.

No yt-dlp import — CI tests these against HTML fixtures without network.

The module name is Phlegethon-specific on purpose: every yt-dlp plugin shares the
``yt_dlp_plugins.extractor`` namespace, so a generic name could collide.
"""

from __future__ import annotations

from html import unescape as html_unescape
import re
from typing import Any
from urllib.parse import urljoin


# Oldest yt-dlp the extractors work with. 2025.07.21 is the first release that
# honours the ``impersonate`` format key (downloader/common.py), which the luxuretv
# formats need to pass the Cloudflare check on the stream host. The request option
# ``impersonate=True`` and ``yt_dlp.networking.exceptions.HTTPError`` are older.
# Keep this equal to the ``yt-dlp>=`` requirement in pyproject.toml (a test checks it).
MIN_YT_DLP_VERSION = '2025.07.21'


def _version_tuple(version: str) -> tuple[int, ...]:
    parts: list[int] = []
    for piece in version.strip().split('.'):
        digits = re.match(r'\d+', piece)
        if not digits:
            break
        parts.append(int(digits.group(0)))
    return tuple(parts)


def yt_dlp_version_problem(current: str | None) -> str | None:
    """A message when ``current`` is older than :data:`MIN_YT_DLP_VERSION`, else ``None``."""
    have = _version_tuple(current or '')
    need = _version_tuple(MIN_YT_DLP_VERSION)
    if have and have >= need:
        return None
    return (
        f'these Launch Site plugins need yt-dlp {MIN_YT_DLP_VERSION} or newer '
        f'(found {current or "an unknown version"}); update yt-dlp'
    )


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


_PREVIEW_MARKERS = (
    '_preview.', '/preview/', '/previews/', '/preview_', '/thumb/', '/thumbs/',
    '/thumbnail/', '/thumbnails/', '_thumb.', '-preview.')


def looks_like_preview(url: str) -> bool:
    """Hover previews and thumbnails are never the main video."""
    lower = url.lower()
    return any(marker in lower for marker in _PREVIEW_MARKERS)


_IMAGE_URL_RE = re.compile(r'\.(?:jpe?g|png|gif|webp|avif|svg|ico|bmp)(?:[?#]|$)', re.IGNORECASE)


def looks_like_image(url: str) -> bool:
    """Thumbnail URLs such as ``abc.mp4/abc.mp4-7.jpg`` contain ``.mp4`` but are images."""
    return bool(_IMAGE_URL_RE.search(url))


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
        'ext': 'webm' if re.search(r'\.webm(?:[?#]|$)', url, re.IGNORECASE) else 'mp4',
        'vcodec': 'avc1',
        'acodec': 'mp4a',
        'http_headers': {},
    }
    if h is not None:
        entry['height'] = h
    if '.m3u8' in url.lower():
        entry['protocol'] = 'm3u8_native'
        entry['format_id'] = f'hls-{fid}'
    return entry


def url_has_video_id(url: str, video_id: str) -> bool:
    """True when ``video_id`` appears in ``url`` as a whole number, not as part of a longer one."""
    return bool(re.search(rf'(?<!\d){re.escape(video_id)}(?!\d)', url))


def extract_formats(webpage: str, page_url: str) -> list[dict[str, Any]]:
    """Collect progressive / HLS media URLs from a video page HTML fixture."""
    formats: list[dict[str, Any]] = []
    seen: set[str] = set()

    def add(url: str, **kwargs: Any) -> None:
        abs_url = _abs(page_url, url.strip())
        if (not abs_url.startswith('http') or abs_url in seen
                or looks_like_preview(abs_url) or looks_like_image(abs_url)):
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
        return html_unescape(m.group('title')).strip()
    m = _TITLE_TAG_RE.search(webpage)
    if m:
        return html_unescape(m.group('title')).strip()
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
    require_video_id_in_url: bool = False,
) -> dict[str, Any]:
    formats = extract_formats(webpage, page_url)
    if require_video_id_in_url:
        # Layouts we have not seen: a related clip must never stand in for the video.
        formats = [f for f in formats if url_has_video_id(f['url'], video_id)]
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


# ---------------------------------------------------------------------------
# Cloudflare
# ---------------------------------------------------------------------------

_CLOUDFLARE_MARKERS = (
    'just a moment...',
    'challenges.cloudflare.com',
    'cf-chl-',
    '_cf_chl_opt',
    'attention required! | cloudflare',
    'checking if the site connection is secure',
)


def is_cloudflare_challenge(webpage: str | None) -> bool:
    """True for Cloudflare's JS / managed-challenge interstitial."""
    if not webpage:
        return False
    head = webpage[:20000].lower()
    hits = sum(marker in head for marker in _CLOUDFLARE_MARKERS)
    return hits >= 2 or ('just a moment...' in head and 'cloudflare' in head)


CLOUDFLARE_HELP = (
    'is protected by a Cloudflare challenge. Use a yt-dlp build with browser impersonation '
    '(curl_cffi; check with --list-impersonate-targets), or open the site in your browser once '
    'and pass those cookies to yt-dlp (--cookies-from-browser <browser>, or Settings > Cookies in '
    'Phlegethon) and retry. The cf_clearance cookie is tied to the browser and network, so use '
    'the same machine.'
)


# ---------------------------------------------------------------------------
# xxxbp.tv
# ---------------------------------------------------------------------------

XXXBP_API_URL = 'https://api.xxxbp.tv/hls'
_XXXBP_TITLE_SUFFIX_RE = re.compile(r'\s*[-|–]\s*XXX\s?BP(?:[\s.]*TV)?\s*$', re.IGNORECASE)
_XXXBP_CARD_RE = re.compile(
    r'<a\s+[^>]*?href=["\'](?P<href>(?:https?://(?:[a-z0-9-]+\.)*xxxbp\.tv)?/video/(?P<id>\d+)/(?P<slug>[^"\'/?#]+))["\'][^>]*>',
    re.IGNORECASE,
)
_TITLE_ATTR_RE = re.compile(r'title=["\'](?P<title>[^"\']*)["\']', re.IGNORECASE)


def clean_xxxbp_title(title: str | None) -> str | None:
    if not title:
        return None
    cleaned = _XXXBP_TITLE_SUFFIX_RE.sub('', title).strip()
    return cleaned or None


def xxxbp_api_formats(payload: Any, referer: str) -> tuple[list[dict[str, Any]], str | None]:
    """Turn the ``api.xxxbp.tv/hls`` JSON into progressive formats and the HLS master URL.

    The payload is ``{"master": <m3u8>, "links": [{"src": <m3u8>, "title": "720p"}],
    "mp4": [{"src": <mp4>, "title": "720p"}]}``. Progressive mp4 files are preferred
    over HLS: they download without ffmpeg and resume cleanly.

    Raises ``ValueError`` when the body is not a JSON object.
    """
    if not isinstance(payload, dict):
        raise ValueError(f'unexpected source list from the xxxbp.tv API ({type(payload).__name__})')
    formats: list[dict[str, Any]] = []
    seen: set[str] = set()
    mp4_items = payload.get('mp4')
    for item in mp4_items if isinstance(mp4_items, list) else []:
        if not isinstance(item, dict) or not isinstance(item.get('src'), str):
            continue
        src = item['src'].strip()
        title = str(item.get('title') or '')
        if not src.startswith('http') or src in seen:
            continue
        seen.add(src)
        height = _height_from_label(title, src)
        fmt: dict[str, Any] = {
            'url': src,
            'format_id': f'mp4-{height}p' if height else 'mp4',
            'format_note': title or None,
            'ext': 'mp4',
            'protocol': 'https',
            'vcodec': 'avc1',
            'acodec': 'mp4a',
            'source_preference': 1,
            'http_headers': {'Referer': referer},
        }
        if height:
            fmt['height'] = height
        formats.append(fmt)
    raw_master = payload.get('master')
    master = raw_master.strip() or None if isinstance(raw_master, str) else None
    return formats, master if master and master.startswith('http') else None


def xxxbp_listing_entries(html: str) -> list[dict[str, str]]:
    """Video cards on a listing page, in page order, without duplicates."""
    entries: list[dict[str, str]] = []
    seen: set[str] = set()
    for m in _XXXBP_CARD_RE.finditer(html):
        video_id = m.group('id')
        if video_id in seen:
            continue
        seen.add(video_id)
        title_m = _TITLE_ATTR_RE.search(m.group(0))
        entries.append({
            'id': video_id,
            'slug': m.group('slug'),
            'url': f'https://xxxbp.tv/video/{video_id}/{m.group("slug")}',
            'title': (html_unescape(title_m.group('title')).strip() if title_m else '') or m.group('slug').replace('-', ' '),
        })
    return entries


def split_page_number(path: str) -> tuple[str, int]:
    """``updates/3`` -> (``updates``, 3); ``model/some-name`` -> (that, 1)."""
    m = re.fullmatch(r'(?P<base>.+?)/(?P<page>\d+)', path.strip('/'))
    if m:
        return m.group('base'), int(m.group('page'))
    return path.strip('/'), 1


# ---------------------------------------------------------------------------
# luxuretv
# ---------------------------------------------------------------------------

_LUXURE_VIDEO_LINK_RE = re.compile(
    r'''href=["'](?P<url>(?:https?://(?:[a-z0-9-]+\.)*luxuretv\.com)?/videos?/[^"'#?]*?-(?P<id>\d+)(?:\.html)?)["']''',
    re.IGNORECASE,
)
_REL_NEXT_RE = re.compile(
    r'''<(?:a|link)\b[^>]*\brel=["']next["'][^>]*\bhref=["'](?P<href>[^"']+)["']|'''
    r'''<(?:a|link)\b[^>]*\bhref=["'](?P<href2>[^"']+)["'][^>]*\brel=["']next["']''',
    re.IGNORECASE,
)


def luxure_listing_links(html: str, page_url: str) -> list[dict[str, str]]:
    """Video links on a luxuretv listing page (best effort, see LuxureTVListIE)."""
    out: list[dict[str, str]] = []
    seen: set[str] = set()
    for m in _LUXURE_VIDEO_LINK_RE.finditer(html):
        video_id = m.group('id')
        if video_id in seen:
            continue
        seen.add(video_id)
        out.append({'id': video_id, 'url': urljoin(page_url, m.group('url'))})
    return out


def next_page_link(html: str, page_url: str) -> str | None:
    m = _REL_NEXT_RE.search(html)
    if not m:
        return None
    href = m.group('href') or m.group('href2')
    return urljoin(page_url, href) if href else None


_LUXURE_PLAYER_RE = re.compile(
    r'<video\b[^>]*\bid=["\']thisPlayer["\'][^>]*>(?P<body>.*?)</video>',
    re.IGNORECASE | re.DOTALL,
)
_LUXURE_TITLE_SUFFIX_RE = re.compile(r'\s*[-|–]\s*LuxureTV\s*$', re.IGNORECASE)
_ISO_DURATION_RE = re.compile(
    r'^P(?:(?P<d>\d+)D)?T?(?:(?P<h>\d+)H)?(?:(?P<m>\d+)M)?(?:(?P<s>\d+(?:\.\d+)?)S)?$', re.IGNORECASE)
_JSON_LD_RE = re.compile(
    r'<script[^>]+type=["\']application/ld\+json["\'][^>]*>(?P<body>.*?)</script>',
    re.IGNORECASE | re.DOTALL,
)


def parse_iso_duration(value: Any) -> int | None:
    """``PT1M36S`` -> 96. Plain numbers are taken as seconds."""
    if isinstance(value, (int, float)):
        return int(value)
    if not isinstance(value, str) or not value.strip():
        return None
    text = value.strip()
    if text.isdigit():
        return int(text)
    m = _ISO_DURATION_RE.match(text)
    if not m or not any(m.groupdict().values()):
        return None
    total = (
        int(m.group('d') or 0) * 86400
        + int(m.group('h') or 0) * 3600
        + int(m.group('m') or 0) * 60
        + float(m.group('s') or 0)
    )
    return int(total)


def json_ld_video(webpage: str) -> dict[str, Any]:
    """The first schema.org ``VideoObject`` in the page, or ``{}``."""
    import json

    def walk(node: Any):
        if isinstance(node, dict):
            kind = node.get('@type')
            if kind == 'VideoObject' or (isinstance(kind, list) and 'VideoObject' in kind):
                yield node
            for value in node.values():
                yield from walk(value)
        elif isinstance(node, list):
            for value in node:
                yield from walk(value)

    for m in _JSON_LD_RE.finditer(webpage):
        try:
            data = json.loads(m.group('body'))
        except ValueError:
            continue
        for video in walk(data):
            return video
    return {}


def luxure_player_sources(webpage: str, page_url: str) -> list[dict[str, str]]:
    """``<source>`` tags of the main player (``<video id="thisPlayer">``) only.

    The page also has an advertising modal player and dozens of related-video clips and
    thumbnails; none of those are the video being watched.
    """
    m = _LUXURE_PLAYER_RE.search(webpage)
    if not m:
        return []
    sources: list[dict[str, str]] = []
    seen: set[str] = set()
    for tag in re.finditer(r'<source\b[^>]*>', m.group('body'), re.IGNORECASE):
        src = re.search(r'\bsrc=["\'](?P<u>[^"\']+)["\']', tag.group(0), re.IGNORECASE)
        if not src:
            continue
        url = _abs(page_url, src.group('u').replace('&amp;', '&').strip())
        if not url.startswith('http') or url in seen or looks_like_image(url):
            continue
        seen.add(url)
        kind = re.search(r'\btype=["\'](?P<t>[^"\']+)["\']', tag.group(0), re.IGNORECASE)
        sources.append({'url': url, 'type': kind.group('t') if kind else 'video/mp4'})
    return sources


def luxure_info(
    *,
    video_id: str,
    page_url: str,
    webpage: str,
    display_id: str | None = None,
) -> dict[str, Any]:
    """Info dict for a luxuretv video page.

    Uses the main player's signed source, falling back to the JSON-LD ``contentUrl``,
    and only then to the generic scraper (for layouts we have not seen).
    """
    ld = json_ld_video(webpage)
    sources = luxure_player_sources(webpage, page_url)
    if not sources and isinstance(ld.get('contentUrl'), str) and not looks_like_image(ld['contentUrl']):
        sources = [{'url': ld['contentUrl'], 'type': 'video/mp4'}]
    if not sources:
        # Neither the player nor JSON-LD matched. Accept only media whose URL carries
        # this video's numeric id, so a related clip is never offered as the video.
        info = build_info_dict(
            video_id=video_id, page_url=page_url, webpage=webpage, display_id=display_id,
            require_video_id_in_url=True)
        for fmt in info['formats']:
            fmt['impersonate'] = True
        return info

    formats = []
    for index, source in enumerate(sources):
        is_hls = source['type'].lower().startswith('application/') or '.m3u8' in source['url']
        formats.append({
            'url': source['url'],
            'format_id': 'default' if len(sources) == 1 else f'default-{index}',
            'ext': 'mp4',
            'protocol': 'm3u8_native' if is_hls else 'https',
            'vcodec': 'avc1',
            'acodec': 'mp4a',
            'http_headers': {'Referer': page_url},
            # The stream sits behind the same Cloudflare check as the page.
            'impersonate': True,
        })
    title = _LUXURE_TITLE_SUFFIX_RE.sub(
        '', html_unescape(str(ld.get('name') or extract_title(webpage) or ''))).strip()
    info: dict[str, Any] = {
        'id': video_id,
        'title': title or display_id or video_id,
        'formats': formats,
        'age_limit': 18,
        'webpage_url': page_url,
    }
    if display_id:
        info['display_id'] = display_id
    thumb = ld.get('thumbnailUrl')
    if isinstance(thumb, list):
        thumb = thumb[0] if thumb else None
    if isinstance(thumb, str):
        info['thumbnail'] = thumb
    duration = parse_iso_duration(ld.get('duration'))
    if duration is not None:
        info['duration'] = duration
    if isinstance(ld.get('uploadDate'), str):
        info['upload_date_raw'] = ld['uploadDate']
    return info
