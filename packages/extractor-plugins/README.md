# Phlegethon Extractor Plugins

App-managed yt-dlp Extractor Plugins for the Launch Sites that upstream yt-dlp
does not cover: **luxuretv** and **xxxbp.tv**. xHamster and YouTube use yt-dlp's
own extractors (xHamster creator pages are covered by yt-dlp's `XHamsterUser`).

## Extractors (manifest 0.2.1)

| IE key | Matches | How it works |
|---|---|---|
| `XxxbpTV` | `xxxbp.tv/video/<id>[/slug]`, `/v/<id>`, `/embed/<id>` on `xxxbp.tv` or any subdomain | `POST api.xxxbp.tv/hls` with `id=<id>`: progressive mp4 per quality plus the HLS master. Title, thumbnail and duration come from og tags and JSON-LD. The page itself only holds hover previews, which are never offered. |
| `XxxbpTVList` | `/updates`, `/top-rated`, `/model/<slug>`, `/site/<slug>`, `/categories/...` (any non-video path) | Pages are `/<section>/<N>`. Lists the first 5 pages; raise with `--extractor-args xxxbptv:max_pages=20` or cap with `--playlist-end`. Every page is fetched as fatal, so an error on page 2 or later (a 429, say) fails the listing instead of ending it early. Only a 404 past the first page ends it. Paths match case-insensitively. |
| `LuxureTV` | `luxuretv.com` or any subdomain, `/videos/<slug>-<id>.html` or `/videos/<id>` | Reads the main player, then JSON-LD. As a last resort it scans the page, but only for media URLs that contain the video id, and fails otherwise. |
| `LuxureTVList` | luxuretv listing pages | Best effort: follows `rel="next"`. Not verified live. |

luxuretv sits behind a Cloudflare managed challenge. Both luxuretv and xxxbp
detect the challenge page and fail with a message that tells the user to supply
browser cookies (Settings, Cookies in Phlegethon, or `--cookies-from-browser`).
`cf_clearance` is bound to the browser and network, so use the same machine.

## Layout

```text
packages/extractor-plugins/
  plugins-manifest.json              # version + extractor keys (must match plugin_sidecar.rs)
  fixtures/                          # HTML / JSON fixtures (no live network in CI)
  tests/
    test_parse_fixtures.py           # stdlib only
    test_extractors_offline.py       # needs `pip install yt-dlp`, skipped otherwise
  phlegethon-launch-sites/           # installable plugin package (pyproject.toml declares yt-dlp>=2025.07.21)
    yt_dlp_plugins/extractor/
      luxuretv.py                    # LuxureTVIE, LuxureTVListIE
      xxxbp.py                       # XxxbpTVIE, XxxbpTVListIE
      _phlegethon_parse.py           # shared parsing (no yt-dlp import)
```

## Minimum yt-dlp version

The plugins need yt-dlp 2025.07.21 or newer: that release is the first to honour the
`impersonate` format key, which the luxuretv streams need to pass the Cloudflare check.
The version is declared in `pyproject.toml` (`dependencies`) and as `MIN_YT_DLP_VERSION`
in `_phlegethon_parse.py`; a test keeps the two equal. The app installs the plugins by
copying files, so pip never enforces the requirement. Each extractor therefore checks the
running yt-dlp and fails with an "update yt-dlp" message when it is older.

## Sidecar install path

Download Core installs this package under the Extractor Toolchain sidecar:

```text
<sidecar_root>/
  yt-dlp-plugins/phlegethon-launch-sites/yt_dlp_plugins/...
  plugins-manifest.json
```

yt-dlp is invoked with `--plugin-dirs <sidecar_root>/yt-dlp-plugins`. Plugins are
updateable independently of an app release via
`download_core::plugin_sidecar::install_plugin_package`. A Rust test asserts
`plugins-manifest.json` equals `PluginManifest::launch_sites_default()`, so bump
both together.

## Tests

```bash
python3 packages/extractor-plugins/tests/test_parse_fixtures.py
python3 packages/extractor-plugins/tests/test_extractors_offline.py   # with yt-dlp installed
```

Try a live extraction without installing anything into the app:

```bash
yt-dlp --plugin-dirs packages/extractor-plugins -F 'https://xxxbp.tv/video/793979/first-time-sex-with-old-couple'
```
