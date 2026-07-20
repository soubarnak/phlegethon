# ⚠ Don't use relative imports
from yt_dlp.extractor.common import InfoExtractor
from yt_dlp.utils import ExtractorError

from yt_dlp_plugins.extractor._parse import build_info_dict


class XxxbpTVIE(InfoExtractor):
    """Phlegethon Extractor Plugin for xxxbp.tv Launch Site single-video probe."""

    IE_NAME = 'XxxbpTV'
    _VALID_URL = r'''(?x)
        https?://(?:www\.)?xxxbp\.tv/
        (?:v|video|videos)/(?P<id>[0-9]+)(?:/(?P<display_id>[^/?#]+))?/?
    '''
    _TESTS = [{
        'url': 'https://www.xxxbp.tv/v/12345',
        'only_matching': True,
    }, {
        'url': 'https://xxxbp.tv/video/12345/sample-clip',
        'only_matching': True,
    }]

    def _real_extract(self, url):
        video_id = self._match_id(url)
        display_id = self._match_valid_url(url).group('display_id') or video_id
        webpage = self._download_webpage(url, video_id)
        try:
            info = build_info_dict(
                video_id=video_id,
                page_url=url,
                webpage=webpage,
                display_id=display_id,
            )
        except ValueError as e:
            raise ExtractorError(str(e), expected=True) from e
        return info
