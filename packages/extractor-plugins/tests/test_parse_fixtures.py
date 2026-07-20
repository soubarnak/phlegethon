#!/usr/bin/env python3
"""Fixture tests for Launch Site HTML parsers (no yt-dlp / no network)."""

from __future__ import annotations

import importlib.util
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


if __name__ == "__main__":
    suite = unittest.defaultTestLoader.loadTestsFromModule(sys.modules[__name__])
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    sys.exit(0 if result.wasSuccessful() else 1)
