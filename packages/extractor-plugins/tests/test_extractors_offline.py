#!/usr/bin/env python3
"""Extractor-level tests with the network mocked out.

Needs the ``yt_dlp`` package (``pip install yt-dlp``); skipped otherwise so the
parse-level tests stay dependency free.
"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "fixtures"
sys.path.insert(0, str(ROOT / "phlegethon-launch-sites"))

try:
    import yt_dlp
    from yt_dlp.utils import ExtractorError

    from yt_dlp_plugins.extractor.luxuretv import LuxureTVIE, LuxureTVListIE
    from yt_dlp_plugins.extractor.xxxbp import XxxbpTVIE, XxxbpTVListIE

    HAVE_YTDLP = True
except Exception:  # pragma: no cover - environment dependent
    HAVE_YTDLP = False


def _read(name: str) -> str:
    return (FIXTURES / name).read_text(encoding="utf-8")


@unittest.skipUnless(HAVE_YTDLP, "yt-dlp is not installed")
class XxxbpExtractorTests(unittest.TestCase):
    def _ie(self, cls, webpage, api=None):
        ydl = yt_dlp.YoutubeDL({"quiet": True, "no_warnings": True})
        ie = cls(ydl)
        ie._download_webpage = lambda *a, **k: webpage
        if api is not None:
            ie._download_json = lambda *a, **k: api
        ie._extract_m3u8_formats = lambda *a, **k: [
            {"format_id": "hls-720", "url": "https://cdn.example/x.m3u8", "ext": "mp4", "height": 720}
        ]
        return ie

    def test_matches_reference_url(self):
        url = "https://xxxbp.tv/video/793979/first-time-sex-with-old-couple"
        self.assertTrue(XxxbpTVIE.suitable(url))
        self.assertFalse(XxxbpTVListIE.suitable(url), "video pages are not listings")
        self.assertTrue(XxxbpTVListIE.suitable("https://xxxbp.tv/updates/2"))
        self.assertFalse(XxxbpTVIE.suitable("https://xxxbp.tv/updates"))

    def test_single_video_uses_api_not_page_previews(self):
        api = json.loads(_read("xxxbp_api_hls.json"))
        ie = self._ie(XxxbpTVIE, _read("xxxbp_video_live.html"), api)
        info = ie._real_extract("https://xxxbp.tv/video/975074/sample-clip")
        self.assertEqual(info["id"], "975074")
        self.assertEqual(info["title"], "Sample Clip")
        self.assertEqual(info["duration"], 288)
        self.assertEqual(info["age_limit"], 18)
        urls = [f["url"] for f in info["formats"]]
        self.assertFalse([u for u in urls if "_preview" in u])
        heights = {f.get("height") for f in info["formats"]}
        self.assertTrue({144, 240, 360, 480, 720} <= heights)

    def test_empty_api_payload_is_an_expected_error(self):
        ie = self._ie(XxxbpTVIE, _read("xxxbp_video_live.html"), {})
        ie._extract_m3u8_formats = lambda *a, **k: []
        with self.assertRaises(ExtractorError) as ctx:
            ie._real_extract("https://xxxbp.tv/video/1/gone")
        self.assertIn("no sources", str(ctx.exception))

    def test_cloudflare_page_gives_cookie_hint(self):
        ie = self._ie(XxxbpTVIE, _read("cloudflare_challenge.html"), {})
        with self.assertRaises(ExtractorError) as ctx:
            ie._real_extract("https://xxxbp.tv/video/1/x")
        self.assertIn("cookies", str(ctx.exception).lower())

    def test_listing_yields_video_urls_and_stops_on_repeat_page(self):
        ie = self._ie(XxxbpTVListIE, _read("xxxbp_listing.html"))
        result = ie._real_extract("https://xxxbp.tv/updates")
        entries = list(result["entries"])
        self.assertEqual(len(entries), 6, "page 2+ repeats page 1 so paging stops")
        self.assertTrue(all(e["ie_key"] == "XxxbpTV" for e in entries))
        self.assertTrue(entries[0]["url"].startswith("https://xxxbp.tv/video/"))


@unittest.skipUnless(HAVE_YTDLP, "yt-dlp is not installed")
class LuxureExtractorTests(unittest.TestCase):
    def _ie(self, cls, webpage):
        ie = cls(yt_dlp.YoutubeDL({"quiet": True, "no_warnings": True}))
        ie._download_webpage = lambda *a, **k: webpage
        return ie

    def test_matches_reference_hosts(self):
        for host in ("en.luxuretv.com", "www.luxuretv.com", "luxuretv.com"):
            url = f"https://{host}/videos/music-sucks-but-love-the-one-on-the-left-511995.html"
            self.assertTrue(LuxureTVIE.suitable(url), url)

    def test_cloudflare_page_gives_cookie_hint(self):
        ie = self._ie(LuxureTVIE, _read("cloudflare_challenge.html"))
        with self.assertRaises(ExtractorError) as ctx:
            ie._real_extract("https://en.luxuretv.com/videos/some-video-100.html")
        self.assertIn("cookies", str(ctx.exception).lower())

    def test_single_video_page_extracts(self):
        ie = self._ie(LuxureTVIE, _read("luxuretv_single_video.html"))
        info = ie._real_extract("https://www.luxuretv.com/videos/sample-video-16982.html")
        self.assertEqual(info["id"], "16982")
        self.assertTrue(info["formats"])


if __name__ == "__main__":
    suite = unittest.defaultTestLoader.loadTestsFromModule(sys.modules[__name__])
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    sys.exit(0 if result.wasSuccessful() else 1)
