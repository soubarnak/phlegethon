#!/usr/bin/env python3
"""Extractor-level tests with the network mocked out.

Needs the ``yt_dlp`` package (``pip install yt-dlp``); skipped otherwise so the
parse-level tests stay dependency free.
"""

from __future__ import annotations

import io
import json
import sys
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "fixtures"
sys.path.insert(0, str(ROOT / "phlegethon-launch-sites"))

try:
    import yt_dlp
    from yt_dlp.networking.common import Response
    from yt_dlp.networking.exceptions import HTTPError
    from yt_dlp.utils import ExtractorError

    from yt_dlp_plugins.extractor.luxuretv import LuxureTVIE, LuxureTVListIE
    from yt_dlp_plugins.extractor.xxxbp import XxxbpTVIE, XxxbpTVListIE

    HAVE_YTDLP = True
except Exception:  # pragma: no cover - environment dependent
    HAVE_YTDLP = False


def _read(name: str) -> str:
    return (FIXTURES / name).read_text(encoding="utf-8")


def _http_error(status: int) -> "ExtractorError":
    """What ``_download_webpage(fatal=True)`` raises for an HTTP error status."""
    response = Response(io.BytesIO(b""), "https://xxxbp.tv/updates/2", {}, status=status)
    return ExtractorError(f"HTTP Error {status}", cause=HTTPError(response))


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

    def test_matches_any_subdomain_and_listing_case(self):
        for host in ("en.xxxbp.tv", "m.xxxbp.tv", "www.xxxbp.tv", "xxxbp.tv"):
            self.assertTrue(XxxbpTVIE.suitable(f"https://{host}/video/793979/clip"), host)
            self.assertTrue(XxxbpTVListIE.suitable(f"https://{host}/updates"), host)
        self.assertTrue(XxxbpTVListIE.suitable("https://xxxbp.tv/Updates"))
        self.assertFalse(XxxbpTVListIE.suitable("https://xxxbp.tv/Video/793979/clip"))
        for lookalike in ("https://notxxxbp.tv/video/1/x", "https://xxxbp.tv.example.com/video/1/x"):
            self.assertFalse(XxxbpTVIE.suitable(lookalike), lookalike)
            self.assertFalse(XxxbpTVListIE.suitable(lookalike), lookalike)

    def test_listing_path_is_lowercased(self):
        seen = []
        ie = self._ie(XxxbpTVListIE, _read("xxxbp_listing.html"))
        ie._download_webpage = lambda url, *a, **k: seen.append(url) or _read("xxxbp_listing.html")
        ie._real_extract("https://xxxbp.tv/Updates")
        self.assertEqual(seen, ["https://xxxbp.tv/updates"])

    def test_non_object_api_payload_is_an_expected_error(self):
        ie = self._ie(XxxbpTVIE, _read("xxxbp_video_live.html"), ["not", "an", "object"])
        with self.assertRaises(ExtractorError) as ctx:
            ie._real_extract("https://xxxbp.tv/video/1/odd")
        self.assertTrue(ctx.exception.expected)

    def test_old_yt_dlp_is_rejected_with_a_clear_message(self):
        ie = self._ie(XxxbpTVIE, _read("xxxbp_video_live.html"), {})
        # Patch the globals of the loaded class: yt-dlp's plugin loader may have replaced
        # the module in sys.modules, so patching by module name could miss this one.
        with mock.patch.dict(XxxbpTVIE._real_extract.__globals__, {"YT_DLP_VERSION": "2024.12.23"}):
            with self.assertRaises(ExtractorError) as ctx:
                ie._real_extract("https://xxxbp.tv/video/1/x")
        self.assertIn("update yt-dlp", str(ctx.exception))

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

    def _paged_ie(self, page2_error):
        """A listing whose page 1 is real and whose page 2 fails with ``page2_error``."""
        ie = self._ie(XxxbpTVListIE, _read("xxxbp_listing.html"))

        def download(url, *args, **kwargs):
            if url.endswith("/2"):
                if not kwargs.get("fatal", True):
                    return False  # yt-dlp reports the error and returns False when not fatal
                raise page2_error
            return _read("xxxbp_listing.html")

        ie._download_webpage = download
        return ie

    def test_error_on_a_later_page_fails_the_listing(self):
        # M-44: a 429 on page 2 used to end the listing quietly as a "complete" channel.
        for status in (429, 500, 503):
            ie = self._paged_ie(_http_error(status))
            result = ie._real_extract("https://xxxbp.tv/updates")
            with self.assertRaises(ExtractorError, msg=f"HTTP {status}"):
                list(result["entries"])

    def test_later_pages_are_downloaded_as_fatal(self):
        calls = []
        ie = self._ie(XxxbpTVListIE, _read("xxxbp_listing.html"))
        ie._download_webpage = lambda url, *a, **k: calls.append((url, k.get("fatal", True))) or _read("xxxbp_listing.html")
        list(ie._real_extract("https://xxxbp.tv/updates")["entries"])
        self.assertTrue(len(calls) >= 2)
        self.assertTrue(all(fatal for _, fatal in calls), calls)

    def test_404_past_the_last_page_ends_the_listing(self):
        ie = self._paged_ie(_http_error(404))
        entries = list(ie._real_extract("https://xxxbp.tv/updates")["entries"])
        self.assertEqual(len(entries), 6)

    def test_first_page_404_is_still_an_error(self):
        ie = self._ie(XxxbpTVListIE, "")
        def download(url, *args, **kwargs):
            raise _http_error(404)
        ie._download_webpage = download
        with self.assertRaises(ExtractorError):
            ie._real_extract("https://xxxbp.tv/updates")


@unittest.skipUnless(HAVE_YTDLP, "yt-dlp is not installed")
class LuxureExtractorTests(unittest.TestCase):
    def _ie(self, cls, webpage, status=200):
        ie = cls(yt_dlp.YoutubeDL({"quiet": True, "no_warnings": True}))
        handle = mock.Mock(status=status)
        ie._download_webpage_handle = lambda *a, **k: (webpage, handle)
        return ie

    def test_matches_reference_hosts(self):
        for host in ("en.luxuretv.com", "www.luxuretv.com", "luxuretv.com", "fr.luxuretv.com", "m.luxuretv.com"):
            url = f"https://{host}/videos/music-sucks-but-love-the-one-on-the-left-511995.html"
            self.assertTrue(LuxureTVIE.suitable(url), url)
            self.assertTrue(LuxureTVListIE.suitable(f"https://{host}/channels/sample/"), host)
        for lookalike in ("https://notluxuretv.com/videos/a-1.html", "https://luxuretv.com.example.com/videos/a-1.html"):
            self.assertFalse(LuxureTVIE.suitable(lookalike), lookalike)

    def test_bare_video_id_is_a_video_not_a_listing(self):
        # L-39: both patterns used to match /videos/<id>.
        for url in ("https://luxuretv.com/videos/16982", "https://en.luxuretv.com/video/16982",
                    "https://luxuretv.com/videos/some-slug-16982"):
            self.assertTrue(LuxureTVIE.suitable(url), url)
            self.assertFalse(LuxureTVListIE.suitable(url), url)
        for url in ("https://luxuretv.com/videos", "https://luxuretv.com/videos/", "https://luxuretv.com/search/term"):
            self.assertFalse(LuxureTVIE.suitable(url), url)
            self.assertTrue(LuxureTVListIE.suitable(url), url)

    def test_fallback_never_offers_a_related_clip(self):
        # M-45: no player, no JSON-LD; related clips must not stand in for the video.
        ie = self._ie(LuxureTVIE, _read("luxuretv_layout_changed.html"))
        info = ie._real_extract("https://en.luxuretv.com/videos/sample-clip-100200.html")
        self.assertEqual(len(info["formats"]), 1)
        self.assertIn("/cf-stream/100200/", info["formats"][0]["url"])
        ie = self._ie(LuxureTVIE, _read("luxuretv_related_only.html"))
        with self.assertRaises(ExtractorError) as ctx:
            ie._real_extract("https://en.luxuretv.com/videos/sample-clip-100200.html")
        self.assertIn("no media formats", str(ctx.exception))

    def test_rate_limit_page_is_reported_as_rate_limit(self):
        # L-35: a 429 body used to parse as "no media formats found".
        ie = self._ie(LuxureTVIE, "<html><body>Too many requests</body></html>", status=429)
        with self.assertRaises(ExtractorError) as ctx:
            ie._real_extract("https://en.luxuretv.com/videos/sample-clip-100200.html")
        self.assertIn("rate limiting", str(ctx.exception))
        ie = self._ie(LuxureTVListIE, "<html></html>", status=429)
        with self.assertRaises(ExtractorError) as ctx:
            ie._real_extract("https://en.luxuretv.com/channels/sample/")
        self.assertIn("rate limiting", str(ctx.exception))

    def test_other_error_status_is_reported(self):
        ie = self._ie(LuxureTVIE, "<html>Not here</html>", status=404)
        with self.assertRaises(ExtractorError) as ctx:
            ie._real_extract("https://en.luxuretv.com/videos/sample-clip-100200.html")
        self.assertIn("HTTP 404", str(ctx.exception))

    def test_old_yt_dlp_is_rejected_with_a_clear_message(self):
        ie = self._ie(LuxureTVIE, _read("luxuretv_live.html"))
        with mock.patch.dict(LuxureTVIE._fetch_page.__globals__, {"YT_DLP_VERSION": "2024.12.23"}):
            with self.assertRaises(ExtractorError) as ctx:
                ie._real_extract("https://en.luxuretv.com/videos/sample-clip-100200.html")
        self.assertIn("update yt-dlp", str(ctx.exception))

    def test_installed_yt_dlp_meets_the_declared_minimum(self):
        from yt_dlp.version import __version__
        from yt_dlp_plugins.extractor._phlegethon_parse import yt_dlp_version_problem
        self.assertIsNone(yt_dlp_version_problem(__version__), __version__)

    def test_yt_dlp_apis_the_plugins_rely_on_exist(self):
        import inspect
        from yt_dlp.extractor.common import InfoExtractor
        self.assertIn("impersonate", inspect.signature(InfoExtractor._download_webpage_handle).parameters)
        self.assertIn("expected_status", inspect.signature(InfoExtractor._download_webpage_handle).parameters)
        self.assertIn("_get_impersonate_target", dir(yt_dlp.downloader.common.FileDownloader))

    def test_cloudflare_page_gives_cookie_hint(self):
        ie = self._ie(LuxureTVIE, _read("cloudflare_challenge.html"))
        with self.assertRaises(ExtractorError) as ctx:
            ie._real_extract("https://en.luxuretv.com/videos/some-video-100.html")
        self.assertIn("cookies", str(ctx.exception).lower())

    def test_live_page_yields_exactly_the_main_stream(self):
        ie = self._ie(LuxureTVIE, _read("luxuretv_live.html"))
        info = ie._real_extract("https://en.luxuretv.com/videos/sample-clip-100200.html")
        self.assertEqual(info["id"], "100200")
        self.assertEqual(info["title"], "Sample Clip")
        self.assertEqual(info["duration"], 96)
        self.assertEqual(info["upload_date"], "20260102")
        self.assertEqual(len(info["formats"]), 1)
        self.assertIn("/cf-stream/100200/", info["formats"][0]["url"])

    def test_single_video_page_extracts(self):
        ie = self._ie(LuxureTVIE, _read("luxuretv_single_video.html"))
        info = ie._real_extract("https://www.luxuretv.com/videos/sample-video-16982.html")
        self.assertEqual(info["id"], "16982")
        self.assertTrue(info["formats"])


if __name__ == "__main__":
    suite = unittest.defaultTestLoader.loadTestsFromModule(sys.modules[__name__])
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    sys.exit(0 if result.wasSuccessful() else 1)
