# ⚠ Don't use relative imports
from yt_dlp.extractor.common import InfoExtractor
from yt_dlp.utils import ExtractorError, unified_strdate
from yt_dlp.version import __version__ as YT_DLP_VERSION

from yt_dlp_plugins.extractor._phlegethon_parse import (
    CLOUDFLARE_HELP,
    is_cloudflare_challenge,
    luxure_info,
    luxure_listing_links,
    next_page_link,
    yt_dlp_version_problem,
)

# 403 and 503 carry the Cloudflare challenge page, which is detected below.
_EXPECTED_STATUS = (403, 429, 503)


class _LuxureTVBase(InfoExtractor):
    """Shared page fetch for the luxuretv extractors."""

    def _fetch_page(self, url, video_id, note=None):
        problem = yt_dlp_version_problem(YT_DLP_VERSION)
        if problem:
            raise ExtractorError(f'luxuretv: {problem}', expected=True)
        kwargs = {'note': note} if note else {}
        webpage, urlh = self._download_webpage_handle(
            url, video_id, expected_status=_EXPECTED_STATUS, impersonate=True, **kwargs)
        if is_cloudflare_challenge(webpage):
            raise ExtractorError(f'luxuretv {CLOUDFLARE_HELP}', expected=True)
        status = getattr(urlh, 'status', 200)
        if status == 429:
            raise ExtractorError(
                'luxuretv is rate limiting requests (HTTP 429); wait a while and retry', expected=True)
        if status >= 400:
            raise ExtractorError(f'luxuretv answered HTTP {status} for {url}', expected=True)
        return webpage


class LuxureTVIE(_LuxureTVBase):
    """Phlegethon Extractor Plugin for a single luxuretv video.

    luxuretv serves a Cloudflare challenge to plain HTTP clients. The extractor
    recognises it and fails with an actionable message instead of a bare 403:
    pass browser cookies (``--cookies-from-browser``) and, where available,
    browser impersonation (``--impersonate chrome``, needs curl_cffi).
    """

    IE_NAME = 'LuxureTV'
    _VALID_URL = r'''(?x)
        https?://(?:[a-z0-9-]+\.)*luxuretv\.com/
        (?:videos?|video)/(?:(?P<display_id>[^/?\#]+?)-)?(?P<id>\d+)(?:\.html)?/?
    '''
    _TESTS = [{
        'url': 'https://en.luxuretv.com/videos/music-sucks-but-love-the-one-on-the-left-511995.html',
        'only_matching': True,
    }, {
        'url': 'https://www.luxuretv.com/videos/sample-video-16982.html',
        'only_matching': True,
    }, {
        'url': 'https://luxuretv.com/video/16982',
        'only_matching': True,
    }]

    def _real_extract(self, url):
        video_id = self._match_id(url)
        display_id = self._match_valid_url(url).group('display_id') or video_id
        webpage = self._fetch_page(url, video_id)
        try:
            info = luxure_info(
                video_id=video_id,
                page_url=url,
                webpage=webpage,
                display_id=display_id,
            )
        except ValueError as e:
            raise ExtractorError(str(e), expected=True) from e
        info['upload_date'] = unified_strdate(info.pop('upload_date_raw', None))
        return info


class LuxureTVListIE(_LuxureTVBase):
    """Best-effort luxuretv listing pages (channels, categories, search).

    Collects video links on the page and follows ``rel="next"``. Not verified
    against the live site, which is behind a Cloudflare challenge; single videos
    (LuxureTVIE) are the supported path.
    """

    IE_NAME = 'LuxureTVList'
    _VALID_URL = r'''(?x)
        https?://(?:[a-z0-9-]+\.)*luxuretv\.com/
        (?!videos?/(?:[^/?\#]+?-)?\d+(?:\.html)?/?(?:[?\#]|$))
        (?:channels?|categories|category|pornstars?|models?|tags?|search|videos)(?:/[^/?\#.]+)*/?(?:\?[^\#]*)?$
    '''
    _TESTS = [{
        'url': 'https://en.luxuretv.com/channels/sample/',
        'only_matching': True,
    }]
    _MAX_PAGES = 5

    def _entries(self, url, playlist_id):
        seen = set()
        page_url = url
        for page in range(1, self._MAX_PAGES + 1):
            webpage = self._fetch_page(page_url, playlist_id, note=f'Downloading page {page}')
            fresh = [v for v in luxure_listing_links(webpage, page_url) if v['id'] not in seen]
            if not fresh:
                return
            for video in fresh:
                seen.add(video['id'])
                yield self.url_result(video['url'], LuxureTVIE, video['id'])
            page_url = next_page_link(webpage, page_url)
            if not page_url:
                return

    def _real_extract(self, url):
        playlist_id = url.rstrip('/').rsplit('/', 1)[-1].split('?')[0] or 'luxuretv'
        entries = list(self._entries(url, playlist_id))
        if not entries:
            raise ExtractorError(f'No videos found at {url}', expected=True)
        return self.playlist_result(entries, playlist_id, playlist_id.replace('-', ' ').title())
