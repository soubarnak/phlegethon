# ⚠ Don't use relative imports
import itertools

from yt_dlp.extractor.common import InfoExtractor
from yt_dlp.networking.exceptions import HTTPError
from yt_dlp.utils import (
    ExtractorError,
    int_or_none,
    parse_duration,
    traverse_obj,
    unified_timestamp,
    urlencode_postdata,
)
from yt_dlp.version import __version__ as YT_DLP_VERSION

from yt_dlp_plugins.extractor._phlegethon_parse import (
    CLOUDFLARE_HELP,
    XXXBP_API_URL,
    clean_xxxbp_title,
    is_cloudflare_challenge,
    split_page_number,
    xxxbp_api_formats,
    xxxbp_listing_entries,
    yt_dlp_version_problem,
)

_BASE = 'https://xxxbp.tv'


def _require_supported_yt_dlp():
    problem = yt_dlp_version_problem(YT_DLP_VERSION)
    if problem:
        raise ExtractorError(f'xxxbp.tv: {problem}', expected=True)


class XxxbpTVIE(InfoExtractor):
    """Phlegethon Extractor Plugin: a single xxxbp.tv video.

    The page only carries hover previews. The real sources come from
    ``POST https://api.xxxbp.tv/hls`` (``id=<video id>``): progressive mp4 files
    plus an HLS master playlist. Links are signed and short-lived, so they are
    resolved at download time, never cached.
    """

    IE_NAME = 'XxxbpTV'
    _VALID_URL = r'''(?x)
        https?://(?:[a-z0-9-]+\.)*xxxbp\.tv/
        (?:video|v|videos|embed)/(?P<id>[0-9]+)(?:/(?P<display_id>[^/?\#]+))?/?
    '''
    _TESTS = [{
        'url': 'https://xxxbp.tv/video/793979/first-time-sex-with-old-couple',
        'only_matching': True,
    }, {
        'url': 'https://www.xxxbp.tv/v/12345',
        'only_matching': True,
    }, {
        'url': 'https://xxxbp.tv/embed/975074',
        'only_matching': True,
    }]

    def _real_extract(self, url):
        _require_supported_yt_dlp()
        mobj = self._match_valid_url(url)
        video_id = mobj.group('id')
        display_id = mobj.group('display_id') or video_id
        page_url = f'{_BASE}/video/{video_id}' + (f'/{display_id}' if display_id != video_id else '')

        webpage = self._download_webpage(page_url, video_id, note='Downloading video page', fatal=False) or ''
        if is_cloudflare_challenge(webpage):
            raise ExtractorError(f'xxxbp.tv {CLOUDFLARE_HELP}', expected=True)

        referer = f'{_BASE}/embed/{video_id}'
        payload = self._download_json(
            XXXBP_API_URL, video_id, note='Downloading source list',
            data=urlencode_postdata({'id': video_id}),
            headers={
                'Content-Type': 'application/x-www-form-urlencoded',
                'Referer': referer,
                'Origin': _BASE,
                'X-Requested-With': 'XMLHttpRequest',
            })
        try:
            formats, master = xxxbp_api_formats(payload, referer)
        except ValueError as e:
            raise ExtractorError(f'xxxbp.tv: {e}', expected=True) from e
        if master:
            hls_formats = self._extract_m3u8_formats(
                master, video_id, 'mp4', m3u8_id='hls', headers={'Referer': referer}, fatal=False)
            for fmt in hls_formats:
                fmt.setdefault('http_headers', {})['Referer'] = referer
            formats.extend(hls_formats)
        if not formats:
            raise ExtractorError(
                f'xxxbp.tv returned no sources for video {video_id}; it may have been removed',
                expected=True)

        ld = self._search_json_ld(webpage, video_id, default={}) if webpage else {}
        title = (
            clean_xxxbp_title(self._og_search_title(webpage, default=None))
            or clean_xxxbp_title(ld.get('title'))
            or display_id.replace('-', ' ').strip().title()
            or video_id)
        return {
            'id': video_id,
            'display_id': display_id,
            'title': title,
            'formats': formats,
            'thumbnail': self._og_search_thumbnail(webpage, default=None) or ld.get('thumbnail'),
            'duration': ld.get('duration') or parse_duration(self._html_search_meta('video:duration', webpage, default=None)),
            'timestamp': ld.get('timestamp') or unified_timestamp(traverse_obj(ld, ('upload_date',))),
            'age_limit': 18,
            'webpage_url': page_url,
        }


class XxxbpTVListIE(InfoExtractor):
    """Phlegethon Extractor Plugin: xxxbp.tv listing pages (``/updates``, ``/model/<name>``, ...).

    Pages are numbered ``/<section>/<n>``. By default the first five pages are
    listed (listings run to thousands of pages); raise it with
    ``--extractor-args xxxbptv:max_pages=20`` or limit with ``--playlist-end``.
    """

    IE_NAME = 'XxxbpTVList'
    _VALID_URL = r'''(?ix)
        https?://(?:[a-z0-9-]+\.)*xxxbp\.tv/
        (?P<path>
            (?!(?:video|v|videos|embed|api|member|legal|feedback|history|static|categories/a-z)(?:/|$|\?))
            [a-z0-9][a-z0-9_-]*(?:/[a-z0-9][a-z0-9_-]*)*?
        )/?(?:\?[^\#]*)?$
    '''
    _TESTS = [{
        'url': 'https://xxxbp.tv/updates',
        'only_matching': True,
    }, {
        'url': 'https://xxxbp.tv/updates/2',
        'only_matching': True,
    }, {
        'url': 'https://xxxbp.tv/model/some-name',
        'only_matching': True,
    }]
    _DEFAULT_MAX_PAGES = 5

    def _max_pages(self):
        raw = traverse_obj(self._configuration_arg('max_pages', ie_key='XxxbpTV'), 0)
        return max(1, int_or_none(raw, default=self._DEFAULT_MAX_PAGES))

    def _fetch_page(self, base, page, playlist_id):
        page_url = base if page == 1 else f'{base}/{page}'
        try:
            # Always fatal: a 429 or 5xx on page 3 must fail the listing, not end it early.
            # Only a 404 past the first page means the listing ran out.
            webpage = self._download_webpage(
                page_url, playlist_id, note=f'Downloading page {page}')
        except ExtractorError as e:
            if page > 1 and isinstance(e.cause, HTTPError) and e.cause.status == 404:
                return []
            raise
        if is_cloudflare_challenge(webpage):
            raise ExtractorError(f'xxxbp.tv {CLOUDFLARE_HELP}', expected=True)
        return xxxbp_listing_entries(webpage or '')

    def _entries(self, base, start_page, playlist_id, first_page_entries):
        seen = set()
        for page in itertools.islice(itertools.count(start_page), self._max_pages()):
            items = first_page_entries if page == start_page else self._fetch_page(base, page, playlist_id)
            fresh = [i for i in items if i['id'] not in seen]
            if not fresh:
                return
            for item in fresh:
                seen.add(item['id'])
                yield self.url_result(
                    item['url'], XxxbpTVIE, item['id'], video_title=item['title'])

    def _real_extract(self, url):
        _require_supported_yt_dlp()
        path = self._match_valid_url(url).group('path').lower()
        base_path, start_page = split_page_number(path)
        base = f'{_BASE}/{base_path}'
        playlist_id = base_path.replace('/', '-')
        first = self._fetch_page(base, start_page, playlist_id)
        if not first:
            raise ExtractorError(f'No videos are listed at {url}', expected=True)
        title = playlist_id.replace('-', ' ').title()
        return self.playlist_result(
            self._entries(base, start_page, playlist_id, first), playlist_id, title)
