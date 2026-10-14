#!/usr/bin/env python3
"""Fixture tests for Launch Site HTML parsers (no yt-dlp / no network)."""

from __future__ import annotations

import importlib.util
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PARSE_PATH = (
    ROOT
    / "phlegethon-launch-sites"
    / "yt_dlp_plugins"
    / "extractor"
    / "_parse.py"
)
FIXTURES = ROOT / "fixtures"


def _load_parse():
    spec = importlib.util.spec_from_file_location("phlegethon_parse", PARSE_PATH)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(mod)
    return mod


parse = _load_parse()


class LuxureTvFixtureTests(unittest.TestCase):
    def test_single_video_yields_qualities(self):
        html = (FIXTURES / "luxuretv_single_video.html").read_text(encoding="utf-8")
        url = "https://www.luxuretv.com/videos/sample-video-16982.html"
        info = parse.build_info_dict(
            video_id="16982",
            page_url=url,
            webpage=html,
            display_id="sample-video",
        )
        self.assertEqual(info["id"], "16982")
        self.assertEqual(info["title"], "Sample LuxureTV Video")
        self.assertEqual(info["duration"], 420)
        heights = {f.get("height") for f in info["formats"] if f.get("height")}
        self.assertTrue({480, 720, 1080}.issubset(heights), heights)
        self.assertTrue(all(f["http_headers"].get("Referer") == url for f in info["formats"]))


class XxxbpFixtureTests(unittest.TestCase):
    def test_single_video_yields_qualities(self):
        html = (FIXTURES / "xxxbp_single_video.html").read_text(encoding="utf-8")
        url = "https://www.xxxbp.tv/v/12345"
        info = parse.build_info_dict(
            video_id="12345",
            page_url=url,
            webpage=html,
            display_id="12345",
        )
        self.assertEqual(info["id"], "12345")
        self.assertEqual(info["title"], "Sample XXXBP Video")
        heights = {f.get("height") for f in info["formats"] if f.get("height")}
        self.assertTrue({360, 720}.issubset(heights), heights)
        self.assertTrue(
            any(".m3u8" in f["url"] for f in info["formats"]),
            "expected HLS format",
        )


class HardFailFixtureTests(unittest.TestCase):
    def test_empty_page_raises(self):
        html = (FIXTURES / "empty_page.html").read_text(encoding="utf-8")
        with self.assertRaises(ValueError) as ctx:
            parse.build_info_dict(
                video_id="0",
                page_url="https://www.luxuretv.com/videos/missing-0.html",
                webpage=html,
            )
        self.assertIn("no media formats", str(ctx.exception))


class PreviewFilterTests(unittest.TestCase):
    def test_looks_like_preview(self):
        self.assertTrue(parse.looks_like_preview("https://cdn.example/a/123_preview.mp4"))
        self.assertTrue(parse.looks_like_preview("https://cdn.example/previews/123.mp4"))
        self.assertFalse(parse.looks_like_preview("https://cdn.example/mp4/123_720p.mp4"))

    def test_live_xxxbp_page_does_not_pick_hover_previews(self):
        html = (FIXTURES / "xxxbp_video_live.html").read_text(encoding="utf-8")
        formats = parse.extract_formats(html, "https://xxxbp.tv/video/975074/sample")
        self.assertFalse(
            [f for f in formats if parse.looks_like_preview(f["url"])],
            "hover preview clips must never be offered as formats",
        )


class XxxbpApiTests(unittest.TestCase):
    def setUp(self):
        self.payload = json.loads((FIXTURES / "xxxbp_api_hls.json").read_text(encoding="utf-8"))
        self.referer = "https://xxxbp.tv/embed/975074"

    def test_mp4_formats_sorted_by_height(self):
        formats, master = parse.xxxbp_api_formats(self.payload, self.referer)
        heights = sorted(f["height"] for f in formats)
        self.assertEqual(heights, [144, 240, 360, 480, 720])
        self.assertTrue(all(f["ext"] == "mp4" for f in formats))
        self.assertTrue(all(f["http_headers"]["Referer"] == self.referer for f in formats))
        self.assertIn("master.m3u8", master)

    def test_empty_payload_is_safe(self):
        formats, master = parse.xxxbp_api_formats({}, self.referer)
        self.assertEqual(formats, [])
        self.assertIsNone(master)

    def test_ignores_non_http_and_duplicate_sources(self):
        payload = {
            "mp4": [
                {"src": "javascript:alert(1)", "title": "720p"},
                {"src": "https://cdn.example/a_360p.mp4", "title": "360p"},
                {"src": "https://cdn.example/a_360p.mp4", "title": "360p"},
                "not-a-dict",
            ]
        }
        formats, _ = parse.xxxbp_api_formats(payload, self.referer)
        self.assertEqual(len(formats), 1)
        self.assertEqual(formats[0]["height"], 360)

    def test_title_suffix_is_cleaned(self):
        self.assertEqual(parse.clean_xxxbp_title("First Time - XXXBP.tv"), "First Time")
        self.assertEqual(parse.clean_xxxbp_title("Plain title"), "Plain title")
        self.assertIsNone(parse.clean_xxxbp_title(None))


class XxxbpListingTests(unittest.TestCase):
    def test_listing_cards_are_unique_and_ordered(self):
        html = (FIXTURES / "xxxbp_listing.html").read_text(encoding="utf-8")
        entries = parse.xxxbp_listing_entries(html)
        ids = [e["id"] for e in entries]
        self.assertEqual(ids, sorted(set(ids), key=ids.index), "no duplicates")
        self.assertEqual(ids[:3], ["994801", "994802", "994803"])
        self.assertTrue(all(e["url"].startswith("https://xxxbp.tv/video/") for e in entries))
        self.assertTrue(all(e["title"] for e in entries))

    def test_split_page_number(self):
        self.assertEqual(parse.split_page_number("updates/3"), ("updates", 3))
        self.assertEqual(parse.split_page_number("/updates/"), ("updates", 1))
        self.assertEqual(parse.split_page_number("model/some-name"), ("model/some-name", 1))
        self.assertEqual(parse.split_page_number("categories/big/12"), ("categories/big", 12))


class CloudflareDetectionTests(unittest.TestCase):
    def test_challenge_page_detected(self):
        html = (FIXTURES / "cloudflare_challenge.html").read_text(encoding="utf-8")
        self.assertTrue(parse.is_cloudflare_challenge(html))

    def test_regular_pages_not_flagged(self):
        for name in ("luxuretv_single_video.html", "xxxbp_single_video.html", "xxxbp_video_live.html"):
            html = (FIXTURES / name).read_text(encoding="utf-8")
            self.assertFalse(parse.is_cloudflare_challenge(html), name)
        self.assertFalse(parse.is_cloudflare_challenge(None))
        self.assertFalse(parse.is_cloudflare_challenge(""))

    def test_help_text_mentions_cookies(self):
        self.assertIn("cookies", parse.CLOUDFLARE_HELP.lower())


class LuxureListingTests(unittest.TestCase):
    HTML = """
    <html><head><link rel="next" href="/videos/page2.html"></head><body>
      <a href="/videos/first-video-100.html">one</a>
      <a href="https://en.luxuretv.com/videos/second-video-200.html">two</a>
      <a href="/videos/first-video-100.html">dup</a>
      <a href="/about">not a video</a>
    </body></html>
    """

    def test_links_are_absolute_unique(self):
        links = parse.luxure_listing_links(self.HTML, "https://en.luxuretv.com/videos/")
        self.assertEqual([l["id"] for l in links], ["100", "200"])
        self.assertTrue(all(l["url"].startswith("https://en.luxuretv.com/") for l in links))

    def test_next_page(self):
        nxt = parse.next_page_link(self.HTML, "https://en.luxuretv.com/videos/")
        self.assertEqual(nxt, "https://en.luxuretv.com/videos/page2.html")
        self.assertIsNone(parse.next_page_link("<html></html>", "https://en.luxuretv.com/"))


if __name__ == "__main__":
    suite = unittest.defaultTestLoader.loadTestsFromModule(sys.modules[__name__])
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    sys.exit(0 if result.wasSuccessful() else 1)
