# Reference URLs

URLs the app is checked against. The machine-readable copy is [`reference-urls.json`](reference-urls.json); this page is the human version. The first four rows were supplied by the product owner.

| Site | URL | Kind | Expected |
| --- | --- | --- | --- |
| xHamster | `https://xhamster.com/videos/your-eshita-indian-bangla-beautiful-girl-with-office-boss-viral-sex-video-xhg2PUz` | Single video | Dedicated, several qualities |
| xHamster | `https://xhamster.com/creators/your-eshita` | Creator page | Dedicated, a list of videos |
| luxuretv | `https://en.luxuretv.com/videos/music-sucks-but-love-the-one-on-the-left-511995.html` | Single video | Dedicated, one stream. Needs browser impersonation or cookies |
| xxxbp.tv | `https://xxxbp.tv/video/793979/first-time-sex-with-old-couple` | Single video | Dedicated, 144p to 360p |
| xxxbp.tv | `https://xxxbp.tv/updates` | Listing | Dedicated, hundreds of videos |
| YouTube | `https://www.youtube.com/watch?v=jNQXAC9IVRw` | Single video | Dedicated (placeholder) |
| YouTube | `https://www.youtube.com/playlist?list=PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI` | Playlist | Dedicated (placeholder) |
| samplelib.com | `https://samplelib.com/sample-mp4.html` | Site with no dedicated extractor | Generic extraction or page scan, with a warning |
| MDN | `https://interactive-examples.mdn.mozilla.net/media/cc0-videos/flower.mp4` | Direct file | Plain HTTP, no yt-dlp |

The host typed as `en.luxure.tv` in the request does not resolve. The site is `luxuretv.com` (`en.luxuretv.com`), which is what the reference uses.

## Run the check

```sh
scripts/probe-check.sh                        # every row, prints a table
scripts/probe-check.sh --site xxxbp           # one site
scripts/probe-check.sh --install-ytdlp        # no yt-dlp yet: use a private venv
scripts/probe-check.sh --cookies-from-browser firefox
cargo run -p download-core --example probe -- <any url>    # one URL, full detail
cargo test -p download-core --test reference_urls_live -- --ignored --nocapture
```

A row is **PASS** when the tier, extractor, item count and quality count match. It is **BLOCKED** when it fails in the documented way, which for luxuretv is `needs_browser_cookies`. Anything else is **FAIL** and makes the command exit non-zero.

## Last result

Run on 2026-10-14 from a Linux container with yt-dlp 2026.08.19, curl_cffi, node 22 and ffmpeg.

| Row | Result | Tier | Extractor | Items | Best quality count |
| --- | --- | --- | --- | --- | --- |
| xhamster-video | PASS | dedicated | XHamster | 1 | 9 |
| xhamster-creator | PASS | dedicated | XHamsterUser | 46 | 7 |
| luxuretv-video | PASS | dedicated | LuxureTV | 1 | 1 |
| xxxbp-video | PASS | dedicated | XxxbpTV | 1 | 3 |
| xxxbp-listing | PASS | dedicated | XxxbpTVList | 271 | 7 |
| youtube-video | PASS | dedicated | Youtube | 1 | 3 |
| youtube-playlist | PASS | dedicated | YoutubeTab | 182 | 7 |
| generic-page | PASS | generic_extractor | Generic | 1 | 1 |
| direct-file | PASS | direct_file | | 1 | 1 |

## End-to-end downloads through the Core

`cargo run -p download-core --example download -- <url> --out DIR --max-height 240` runs the app's own flow: probe, pick a quality, confirm, transfer, then checks the file. Same machine and date as above. Every file was verified with ffprobe.

| URL | Result | File |
| --- | --- | --- |
| Direct file (MDN flower.mp4) | Completed | 1.1 MB, 5 s, plain HTTP |
| xxxbp.tv reference, 240p | Completed | 47.5 MB, 25:04, h264 + aac |
| luxuretv reference | Completed | 12.4 MB, 1:36, h264 + aac |
| xHamster reference, 240p (HLS, needs ffmpeg) | Completed | 507 s, h264 + aac |
| YouTube "Me at the zoo" | Probe passes, download fails with 403 | See below |

**YouTube from a datacenter address.** The probe lists formats, but the download is refused with HTTP 403, and plain `yt-dlp` fails the same way from this container. YouTube treats datacenter traffic as automated. The Core reports it as `needs_browser_cookies`, so the UI points at Settings, Cookies. On a home connection this is not expected; if it happens, supply browser cookies and keep yt-dlp current.

## What luxuretv needs

luxuretv sits behind a Cloudflare managed challenge. Without help the probe fails with `needs_browser_cookies`.

| Setup | Result |
| --- | --- |
| yt-dlp with curl_cffi (Settings, Toolchain shows **Browser impersonation: Found**) | Works without cookies |
| yt-dlp without curl_cffi | BLOCKED. Set **Settings, Cookies** to your browser, or install a yt-dlp that includes curl_cffi |

The Cloudflare cookie is tied to the browser and the network, so use the same machine.

## Sandbox notes

The container used for these runs reaches the internet only through an environment proxy. The Core's HTTP client now honours `HTTPS_PROXY`, `HTTP_PROXY` and `ALL_PROXY`, the same as yt-dlp.
