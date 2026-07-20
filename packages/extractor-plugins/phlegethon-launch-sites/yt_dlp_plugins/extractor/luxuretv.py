# ⚠ Don't use relative imports
from yt_dlp.extractor.common import InfoExtractor
from yt_dlp.utils import ExtractorError

from yt_dlp_plugins.extractor._parse import build_info_dict


class LuxureTVIE(InfoExtractor):
    """Phlegethon Extractor Plugin for luxuretv Launch Site single-video probe."""

    IE_NAME = 'LuxureTV'
    _VALID_URL = r'''(?x)
        https?://(?:www\.|en\.)?luxuretv\.com/
        (?:videos?|video)/(?:(?P<display_id>[^/?#]+?)-)?(?P<id>\d+)(?:\.html)?/?
    '''
    _TESTS = [{
        'url': 'https://www.luxuretv.com/videos/sample-video-16982.html',
        'only_matching': True,
    }, {
        'url': 'https://en.luxuretv.com/videos/sample-16982.html',
        'only_matching': True,
    }, {
        'url': 'https://luxuretv.com/video/16982',
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
