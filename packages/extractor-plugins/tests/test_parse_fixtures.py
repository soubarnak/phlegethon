#!/usr/bin/env python3
"""Fixture tests for Launch Site HTML parsers (no yt-dlp / no network)."""

from __future__ import annotations

import importlib.util
import json
import re
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PARSE_PATH = (
    ROOT
    / "phlegethon-launch-sites"
    / "yt_dlp_plugins"
    / "extractor"
    / "_phlegethon_parse.py"
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


class LuxureLivePageTests(unittest.TestCase):
    URL = "https://en.luxuretv.com/videos/sample-clip-100200.html"

    def setUp(self):
        self.html = (FIXTURES / "luxuretv_live.html").read_text(encoding="utf-8")

    def test_only_the_main_player_source_is_used(self):
        sources = parse.luxure_player_sources(self.html, self.URL)
        self.assertEqual(len(sources), 1)
        self.assertIn("/cf-stream/100200/", sources[0]["url"])
        self.assertIn("md5=TOKEN&expires=", sources[0]["url"], "&amp; must be unescaped")

    def test_info_ignores_thumbnails_ads_and_related_clips(self):
        info = parse.luxure_info(video_id="100200", page_url=self.URL, webpage=self.html)
        self.assertEqual(len(info["formats"]), 1)
        fmt = info["formats"][0]
        self.assertEqual(fmt["format_id"], "default")
        self.assertTrue(fmt["impersonate"])
        self.assertEqual(fmt["http_headers"]["Referer"], self.URL)
        self.assertEqual(info["title"], "Sample Clip")
        self.assertEqual(info["duration"], 96)
        self.assertTrue(info["thumbnail"].endswith(".jpg"))
        self.assertTrue(info["upload_date_raw"].startswith("2026-01-02"))

    def test_json_ld_content_url_is_the_fallback(self):
        no_player = self.html.replace('id="thisPlayer"', 'id="other"')
        info = parse.luxure_info(video_id="100200", page_url=self.URL, webpage=no_player)
        self.assertEqual(
            [f["url"] for f in info["formats"]],
            ["https://en.luxuretv.com/cf-stream/100200/a/b/c/d/e/abcde0002.mp4"],
        )

    def test_generic_scraper_never_returns_images(self):
        formats = parse.extract_formats(self.html, self.URL)
        self.assertFalse([f for f in formats if parse.looks_like_image(f["url"])])
        self.assertFalse([f for f in formats if "preview_" in f["url"]])

    def test_image_detection(self):
        self.assertTrue(parse.looks_like_image("https://x/abc.mp4/abc.mp4-7b.jpg"))
        self.assertTrue(parse.looks_like_image("https://x/a.PNG?v=1"))
        self.assertFalse(parse.looks_like_image("https://x/abc.mp4?md5=1"))

    def test_iso_durations(self):
        self.assertEqual(parse.parse_iso_duration("PT1M36S"), 96)
        self.assertEqual(parse.parse_iso_duration("PT1H2M3S"), 3723)
        self.assertEqual(parse.parse_iso_duration("PT45S"), 45)
        self.assertEqual(parse.parse_iso_duration(120), 120)
        self.assertEqual(parse.parse_iso_duration("90"), 90)
        self.assertIsNone(parse.parse_iso_duration("garbage"))
        self.assertIsNone(parse.parse_iso_duration(None))

    def test_page_without_any_source_still_raises(self):
        with self.assertRaises(ValueError):
            parse.luxure_info(
                video_id="1", page_url=self.URL, webpage="<html><body>nothing</body></html>")


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


class LuxureFallbackTests(unittest.TestCase):
    """M-45: the generic scraper must never offer a related clip as the video."""

    URL = "https://en.luxuretv.com/videos/sample-clip-100200.html"

    def test_fallback_accepts_only_urls_with_the_video_id(self):
        html = (FIXTURES / "luxuretv_layout_changed.html").read_text(encoding="utf-8")
        info = parse.luxure_info(video_id="100200", page_url=self.URL, webpage=html)
        self.assertEqual(
            [f["url"] for f in info["formats"]],
            ["https://en.luxuretv.com/cf-stream/100200/a/b/c/d/e/abcde0002.mp4?md5=TOKEN&expires=1790000000"],
        )
        self.assertTrue(all(f["impersonate"] for f in info["formats"]))

    def test_fallback_fails_when_only_related_clips_remain(self):
        html = (FIXTURES / "luxuretv_related_only.html").read_text(encoding="utf-8")
        with self.assertRaises(ValueError):
            parse.luxure_info(video_id="100200", page_url=self.URL, webpage=html)

    def test_video_id_must_match_as_a_whole_number(self):
        self.assertTrue(parse.url_has_video_id("https://x/100200/a.mp4", "100200"))
        self.assertTrue(parse.url_has_video_id("https://x/a_100200.mp4", "100200"))
        self.assertFalse(parse.url_has_video_id("https://x/1002001/a.mp4", "100200"))
        self.assertFalse(parse.url_has_video_id("https://x/2100200/a.mp4", "100200"))

    def test_listing_links_accept_any_subdomain(self):
        html = '<a href="https://fr.luxuretv.com/videos/premier-300.html">x</a>'
        links = parse.luxure_listing_links(html, "https://fr.luxuretv.com/videos/")
        self.assertEqual([l["id"] for l in links], ["300"])


class XxxbpSubdomainAndPayloadTests(unittest.TestCase):
    def test_listing_cards_accept_any_subdomain(self):
        html = '<a href="https://en.xxxbp.tv/video/42/some-clip" title="Some Clip">x</a>'
        entries = parse.xxxbp_listing_entries(html)
        self.assertEqual([e["id"] for e in entries], ["42"])
        self.assertEqual(entries[0]["url"], "https://xxxbp.tv/video/42/some-clip")

    def test_non_object_payload_is_a_value_error(self):
        for payload in ([], [{"src": "https://x/a.mp4"}], "text", 3, None):
            with self.assertRaises(ValueError, msg=repr(payload)):
                parse.xxxbp_api_formats(payload, "https://xxxbp.tv/embed/1")

    def test_malformed_members_are_skipped(self):
        formats, master = parse.xxxbp_api_formats(
            {"mp4": [{"src": 5}, {"src": None}, "x", {"src": "https://x/a_720.mp4", "title": "720p"}],
             "master": 7},
            "https://xxxbp.tv/embed/1")
        self.assertEqual([f["height"] for f in formats], [720])
        self.assertIsNone(master)
        formats, master = parse.xxxbp_api_formats({"mp4": "oops"}, "https://xxxbp.tv/embed/1")
        self.assertEqual(formats, [])


class FormatDetailTests(unittest.TestCase):
    def test_thumb_filter_keeps_media_paths_that_start_with_thumb(self):
        self.assertFalse(parse.looks_like_preview("https://cdn.example/thumbnailer-cdn/a/video.mp4"))
        self.assertFalse(parse.looks_like_preview("https://cdn.example/thumbs-and-more/video.mp4"))
        self.assertTrue(parse.looks_like_preview("https://cdn.example/thumbs/a.mp4"))
        self.assertTrue(parse.looks_like_preview("https://cdn.example/a/thumb/a.mp4"))

    def test_titles_are_html_unescaped(self):
        html = '<title>Tom &amp; Jerry &#39;s &quot;Clip&quot;</title>'
        self.assertEqual(parse.extract_title(html), 'Tom & Jerry \'s "Clip"')
        og = '<meta property="og:title" content="A &amp; B">'
        self.assertEqual(parse.extract_title(og), "A & B")
        card = '<a href="/video/7/a-b" title="A &amp; B">x</a>'
        self.assertEqual(parse.xxxbp_listing_entries(card)[0]["title"], "A & B")

    def test_formats_do_not_guess_resolution_or_width(self):
        fmt = parse.extract_formats('<source src="https://cdn.example/clip_1080.mp4">', "https://x/")[0]
        self.assertEqual(fmt["height"], 1080)
        self.assertNotIn("resolution", fmt)
        api, _ = parse.xxxbp_api_formats(
            {"mp4": [{"src": "https://cdn.example/v_720.mp4", "title": "720p"}]}, "https://xxxbp.tv/")
        self.assertNotIn("width", api[0])

    def test_ext_follows_the_url(self):
        webm = parse.extract_formats('<source src="https://cdn.example/clip_480.webm">', "https://x/")[0]
        mp4 = parse.extract_formats('<source src="https://cdn.example/clip_480.mp4">', "https://x/")[0]
        hls = parse.extract_formats('<source src="https://cdn.example/clip_480.m3u8">', "https://x/")[0]
        self.assertEqual((webm["ext"], mp4["ext"], hls["ext"]), ("webm", "mp4", "mp4"))


class YtDlpVersionTests(unittest.TestCase):
    def test_minimum_version_gate(self):
        floor = parse.MIN_YT_DLP_VERSION
        self.assertIsNone(parse.yt_dlp_version_problem(floor))
        self.assertIsNone(parse.yt_dlp_version_problem("2026.08.19"))
        self.assertIsNone(parse.yt_dlp_version_problem(floor + ".232754"))
        self.assertIn(floor, parse.yt_dlp_version_problem("2025.06.30"))
        self.assertIsNotNone(parse.yt_dlp_version_problem("2024.12.23"))
        self.assertIsNotNone(parse.yt_dlp_version_problem("unknown"))
        self.assertIsNotNone(parse.yt_dlp_version_problem(None))

    def test_package_metadata_agrees_with_the_code_and_manifest(self):
        pyproject = (ROOT / "phlegethon-launch-sites" / "pyproject.toml").read_text(encoding="utf-8")
        self.assertIn("[project]", pyproject)
        requirement = re.search(r'dependencies\s*=\s*\[\s*"yt-dlp>=([0-9.]+)"\s*\]', pyproject)
        self.assertIsNotNone(requirement, "pyproject.toml must require yt-dlp>=<version>")
        self.assertEqual(requirement.group(1), parse.MIN_YT_DLP_VERSION)
        version = re.search(r'^version\s*=\s*"([^"]+)"', pyproject, re.MULTILINE)
        manifest = json.loads((ROOT / "plugins-manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(version.group(1), manifest["version"])
        self.assertFalse((ROOT / "phlegethon-launch-sites" / "setup.cfg").exists(),
                         "setup.cfg would drift from pyproject.toml")


if __name__ == "__main__":
    suite = unittest.defaultTestLoader.loadTestsFromModule(sys.modules[__name__])
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    sys.exit(0 if result.wasSuccessful() else 1)
