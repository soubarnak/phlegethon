# Phlegethon Extractor Plugins

App-managed yt-dlp Extractor Plugins for Launch Sites that upstream yt-dlp does
not cover: **luxuretv** and **xxxbp.tv**.

## Layout

```text
packages/extractor-plugins/
  plugins-manifest.json              # independent update metadata
  fixtures/                          # HTML fixtures (CI default — no live net)
  tests/                             # stdlib unittest against fixtures
  phlegethon-launch-sites/           # installable plugin package
    yt_dlp_plugins/extractor/
      luxuretv.py                    # LuxureTVIE
      xxxbp.py                       # XxxbpTVIE
      _parse.py                      # shared HTML → formats (no yt-dlp import)
```

## Sidecar install path

Download Core installs this package under the Extractor Toolchain sidecar:

```text
<sidecar_root>/
  yt-dlp-plugins/phlegethon-launch-sites/yt_dlp_plugins/...
  plugins-manifest.json
```

yt-dlp is invoked with `--plugin-dirs <sidecar_root>/yt-dlp-plugins`.

Plugins are updateable independently of a full Win/Debian app release via
`download_core::plugin_sidecar::install_plugin_package` (replace package + bump
manifest version).

## CI

```bash
python3 packages/extractor-plugins/tests/test_parse_fixtures.py
```

No live Launch Site network. Live yt-dlp smoke is optional and gated outside CI.
