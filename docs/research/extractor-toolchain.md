# Research: Extractor Toolchain packaging and Launch Site coverage

**Ticket:** [#2](https://github.com/soubarnak/phlegethon/issues/2)  
**Date:** 2026-07-20  
**Scope:** Hand-off SPEC input only (no downloader implementation).  
**Constraint:** Primary sources from the toolchain project and non-Google technical material; no Google-owned documentation.

## Recommendation (one line)

**Wrap official [yt-dlp](https://github.com/yt-dlp/yt-dlp) release binaries as a managed sidecar** (CLI + JSON probe), ship/update **ffmpeg/ffprobe** beside it, prefer the **nightly** channel for extractor freshness, and plan **custom extractors (yt-dlp plugins or Download Core handlers)** for **luxuretv** and **xxxbp.tv** — neither has a first-party extractor today.

## Why yt-dlp (not you-get / gallery-dl)

| Tool | Fit for Phlegethon | Evidence |
| --- | --- | --- |
| **yt-dlp** | Best match: mature probe (`-j`/`-J`), format listing, playlists/channels, cookies, resume, plugins, Win/Linux binaries | [README](https://github.com/yt-dlp/yt-dlp/blob/master/README.md), [supportedsites.md](https://github.com/yt-dlp/yt-dlp/blob/master/supportedsites.md) |
| **you-get** | Among the four Launch Sites, first-party focus is effectively YouTube; no luxuretv/xxxbp/xHamster extractors in its extractors tree | [you-get README supported sites](https://github.com/soimort/you-get/blob/develop/README.md); extractors listing via GitHub API (`youtube.py` only among Launch Site names) |
| **gallery-dl** | Gallery/image-oriented; lists xHamster, not luxuretv/xxxbp; not a video probe/quality toolchain for Download Core | [gallery-dl supportedsites](https://github.com/mikf/gallery-dl/blob/master/docs/supportedsites.md) |

**Alternatives verdict:** Use yt-dlp as the Extractor Toolchain. Do not dual-stack you-get or gallery-dl for Launch Site v1.

---

## Packaging options (Win11 + Debian)

### Option A — Sidecar official binaries (recommended)

Ship platform-appropriate release assets next to the app (or under an app-managed tools dir) and invoke them from the Rust Download Core via subprocess.

| OS | Primary binary | Notes |
| --- | --- | --- |
| Windows 11 (x64) | [`yt-dlp.exe`](https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe) | Documented as recommended Windows standalone x64 binary (Win8+) |
| Windows 11 (ARM64) | [`yt-dlp_arm64.exe`](https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_arm64.exe) | Win10+ ARM64 standalone |
| Debian (glibc x86_64) | [`yt-dlp`](https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp) (zipimport; needs system Python) **or** [`yt-dlp_linux`](https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_linux) (standalone) | README recommends zipimport for Linux/BSD; standalone linux build also published |

Source: [Release files table in README](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#release-files).

**Also ship:**

- **ffmpeg + ffprobe** binaries (strongly recommended for merge/post-process). yt-dlp points at [yt-dlp/FFmpeg-Builds](https://github.com/yt-dlp/FFmpeg-Builds) and [ffmpeg.org](https://www.ffmpeg.org). ([Dependencies](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#dependencies))
- For **full YouTube** support: **yt-dlp-ejs** plus a JS runtime (deno recommended; also node, bun, QuickJS). ([Dependencies](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#dependencies), [EJS wiki](https://github.com/yt-dlp/yt-dlp/wiki/EJS))
- Optional but useful for some adult/generic TLS fingerprinting cases: builds that include **curl_cffi** impersonation. ([Impersonation](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#impersonation); xHamster tests use `generic:impersonate`)

**Probe contract for Download Core:** `-J` / `--dump-single-json` (or `-j` per item) for Probe-then-Confirm; `-F` for human format listing; `-c` / `--continue` for resumable fragments. ([CLI options](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#general-options))

**Why sidecar over in-process embed for a Rust core:**

1. Matches a non-Python shell (WinUI 3 / Tauri) without bundling a Python interpreter in-process.
2. Official binaries support self-update (`yt-dlp -U` / `--update-to`). ([UPDATE](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#update))
3. Process isolation: extractor crashes/timeouts do not take down the Download Core.
4. Clear licensing boundary: app ships/executes a third-party binary rather than statically linking Python embedding.

### Option B — Embed yt-dlp as a Python library

yt-dlp documents an embedding API (`from yt_dlp import YoutubeDL`). ([Embedding yt-dlp](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#embedding-yt-dlp))

| Pros | Cons for Phlegethon |
| --- | --- |
| In-process callbacks (`progress_hooks`), no JSON parse | Requires shipping/managing a Python 3.10+ (CPython) or PyPy 3.11+ runtime on both OSes ([Dependencies](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#dependencies)) |
| Same feature surface as CLI | Update story is pip/venv rebuild, not `-U` |
| PyPI wheel is Unlicense-only source/wheel | Still need ffmpeg + ejs/JS runtime beside the embed |

**Verdict:** Viable only if Phlegethon already ships a managed Python environment. Not preferred for WinUI/Tauri + Rust Download Core.

### Option C — Distro / third-party package managers (not sole strategy)

| Path | Reality |
| --- | --- |
| Debian `apt` package `yt-dlp` | Exists in Debian suites, but versions lag by suite (e.g. bookworm `2023.03.04-1` vs forky/sid `2026.07.04-1`). ([Debian sources API](https://sources.debian.org/api/src/yt-dlp/)) |
| Ubuntu PPA / Snap / Scoop / Chocolatey / winget | Documented as **third-party**, may not be up-to-date. ([Installation wiki](https://github.com/yt-dlp/yt-dlp/wiki/Installation)) |
| pip `yt-dlp[default]` | Good for developers; update via reinstall. ([Installation wiki](https://github.com/yt-dlp/yt-dlp/wiki/Installation)) |

**Verdict:** Fine as a power-user override; **do not** rely on Debian stable apt alone for equal Launch Site coverage (extractor rot).

### Licensing note (packaging decision input)

- Project / PyPI sdist+wheel: **Unlicense**. ([Licensing](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#licensing))
- **PyInstaller-bundled** executables (e.g. `yt-dlp.exe`, `yt-dlp_linux`): combined work **GPLv3+**. ([Licensing](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#licensing))
- Zipimport Unix `yt-dlp`: Unlicense plus noted ISC/MIT third-party bits. ([Licensing](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#licensing))

SPEC should record which binary family is redistributed and how that interacts with Phlegethon’s license.

---

## Update story

| Mechanism | When to use |
| --- | --- |
| **`yt-dlp -U` / `--update-to CHANNEL[@TAG]`** on managed release binaries | Default app-managed update; channels: `stable`, `nightly` (recommended for regular use), `master`. ([UPDATE](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#update), [update channels](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#update-channels)) |
| **App downloads GitHub release assets** (stable or [nightly-builds](https://github.com/yt-dlp/yt-dlp-nightly-builds/releases)) with checksum verification | Prefer when the app must control trust, rollback, and concurrent jobs |
| **pip reinstall** | Only if Option B / developer installs |
| **apt / winget / scoop / etc.** | Optional; not the primary Launch Site freshness path |

**Channel guidance for Phlegethon:** Prefer **nightly** for the sidecar (README: stable is often “stale” and prone to external site breakage; nightly is recommended for regular users). ([Update channels](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#update-channels))

**Unpackaged zips** (`yt-dlp_win.zip`, etc.) are explicitly **no auto-update** — avoid as the managed install form. ([Release files](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#release-files))

**ffmpeg/ejs/JS runtime:** update on a separate cadence from yt-dlp; they are not covered by `yt-dlp -U`.

**Plugins for gap sites:** ship Phlegethon-owned extractor plugins under the documented plugin paths (e.g. next to the binary under `yt-dlp-plugins/`, or `%APPDATA%/yt-dlp/plugins/` / XDG config). ([Plugins](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#plugins), [sample plugins](https://github.com/yt-dlp/yt-dlp-sample-plugins))

---

## Launch Site coverage

Checked against yt-dlp **2026.07.04** (`--list-extractors`), [supportedsites.md](https://github.com/yt-dlp/yt-dlp/blob/master/supportedsites.md), and extractor sources. Absence of `luxure` / `xxxbp` confirmed in the extractor tree (recursive git tree listing).

| Launch Site | Probe (metadata + item list) | Quality list | Channel / playlist / user | Gaps / realism |
| --- | --- | --- | --- | --- |
| **YouTube** | **Strong.** Dedicated `youtube` IE family; `-J`/`-j` JSON probe. | **Strong** if ffmpeg + yt-dlp-ejs + JS runtime present; adaptive formats need merge (`bv*+ba`-class selection). | **Strong:** `youtube:playlist`, `youtube:tab`, `youtube:user`, channel URLs (README: channel URLs download all uploads including shorts/live), plus feeds (`:ytfav`, etc.). | Age-gate / some formats need cookies and/or PO tokens; without JS runtime, formats may be missing. ([supportedsites](https://github.com/yt-dlp/yt-dlp/blob/master/supportedsites.md), [YouTube extractor args](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#youtube), [Dependencies](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#dependencies), [CHANGES: YouTube improvements](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#new-features)) |
| **xHamster** | **Strong** for single video URLs matching `XHamsterIE` domains (`xhamster.com` and documented mirrors). | **Good:** progressive + HLS (`m3u8`) formats with height/quality ids; source prefers m3u8 sort because **HTTP progressive formats can return “Wrong key”**. | **User/creator playlists:** `XHamsterUser` for `/users/...` and `/creators/...` with pagination. Embed: `XHamsterEmbed`. | Not a “channel” model like YouTube; friends-only videos need cookies; some tests require browser impersonation. ([xhamster.py](https://github.com/yt-dlp/yt-dlp/blob/master/yt_dlp/extractor/xhamster.py), [supportedsites](https://github.com/yt-dlp/yt-dlp/blob/master/supportedsites.md)) |
| **luxuretv** / **en.luxuretv.com** | **No dedicated extractor** in supportedsites or extractor tree. Falls through to **`generic`** only (“works on some sites”). | **Unreliable** from watch-page URL alone; realistic path is custom plugin that resolves player/CDN MP4 or HLS, *or* companion-assisted direct media URL then generic/ffmpeg. | **None** first-party (no channel/user IE). | Treat as **custom-handler gap** for equal v1 bar. Upstream may reject some adult-site PRs depending on piracy/fakes policy — plan **in-tree Phlegethon plugin**, not relying on upstream merge. ([supportedsites intro](https://github.com/yt-dlp/yt-dlp/blob/master/supportedsites.md), [Plugins](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#plugins), [CONTRIBUTING piracy policy](https://github.com/yt-dlp/yt-dlp/blob/master/CONTRIBUTING.md#is-the-website-primarily-used-for-piracy)) |
| **xxxbp.tv** | **No dedicated extractor** (not in supportedsites; no extractor path hits). **`generic` only.** | **Unknown / likely gap** until a custom extractor exists. | **None** first-party. | Same as luxuretv: **custom plugin or Download Core handler** required for equal Launch Site acceptance. |

### Coverage summary for SPEC acceptance

- **Ready via yt-dlp alone (with deps):** YouTube, xHamster (video + user/creator lists).
- **Require Phlegethon custom extractors/plugins (or Companion-fed direct media URLs):** luxuretv, xxxbp.tv.
- **CONTEXT alignment:** “custom handlers only for gaps” — luxuretv and xxxbp.tv **are** those gaps.

---

## Risks

1. **Extractor rot:** Adult and large sites break often; `stable` channel lags; need nightly + in-app update UX. ([Update channels](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#update-channels))
2. **YouTube completeness cost:** Must ship/maintain ffmpeg, ejs, and a JS runtime; cookies/PO tokens for restricted formats. ([Dependencies](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#dependencies))
3. **GPLv3+ redistribution** if shipping PyInstaller binaries. ([Licensing](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#licensing))
4. **Two Launch Sites unsupported upstream** — equal v1 bar is blocked until custom plugins ship; generic is not a substitute for Probe-then-Confirm quality lists.
5. **xHamster HTTP format breakage** — prefer HLS; surface quality carefully. ([xhamster.py](https://github.com/yt-dlp/yt-dlp/blob/master/yt_dlp/extractor/xhamster.py))
6. **Plugin trust:** yt-dlp loads plugins with no code checks — only ship Phlegethon-signed/owned plugins. ([Plugins](https://github.com/yt-dlp/yt-dlp/blob/master/README.md#plugins))
7. **Debian apt lag** if users or packaging mistakenly depend on suite packages. ([Debian yt-dlp versions](https://sources.debian.org/api/src/yt-dlp/))
8. **Auth/cookies** for gated xHamster (and YouTube age) content — separate open ticket in map (“Not yet specified”).

---

## Sources (primary)

- yt-dlp README (install, release files, update, dependencies, embedding, plugins, CLI): https://github.com/yt-dlp/yt-dlp/blob/master/README.md  
- yt-dlp supported sites: https://github.com/yt-dlp/yt-dlp/blob/master/supportedsites.md  
- yt-dlp Installation wiki: https://github.com/yt-dlp/yt-dlp/wiki/Installation  
- yt-dlp EJS wiki: https://github.com/yt-dlp/yt-dlp/wiki/EJS  
- xHamster extractors: https://github.com/yt-dlp/yt-dlp/blob/master/yt_dlp/extractor/xhamster.py  
- YouTube extractor package: https://github.com/yt-dlp/yt-dlp/tree/master/yt_dlp/extractor/youtube  
- Generic extractor: https://github.com/yt-dlp/yt-dlp/blob/master/yt_dlp/extractor/generic.py  
- CONTRIBUTING (site policy): https://github.com/yt-dlp/yt-dlp/blob/master/CONTRIBUTING.md  
- Sample plugins: https://github.com/yt-dlp/yt-dlp-sample-plugins  
- Nightly builds: https://github.com/yt-dlp/yt-dlp-nightly-builds/releases  
- FFmpeg builds for yt-dlp: https://github.com/yt-dlp/FFmpeg-Builds  
- FFmpeg project: https://www.ffmpeg.org  
- Debian package versions: https://sources.debian.org/api/src/yt-dlp/  
- PyPI yt-dlp: https://pypi.org/project/yt-dlp/  
- gallery-dl supported sites: https://github.com/mikf/gallery-dl/blob/master/docs/supportedsites.md  
- you-get README: https://github.com/soimort/you-get/blob/develop/README.md  
- Local verification: yt-dlp release binary `2026.07.04`, `--list-extractors` (no luxuretv/xxxbp entries)

---

## SPEC hand-off bullets

1. **Toolchain = yt-dlp sidecar** (not embed, not you-get/gallery-dl).
2. **Win11:** ship `yt-dlp.exe` (+ ARM64 variant if needed) + ffmpeg/ffprobe + ejs/JS runtime; update via `-U`/`--update-to nightly` or app-fetched assets.
3. **Debian:** ship `yt-dlp_linux` **or** zipimport `yt-dlp` + system Python; do not depend on apt alone.
4. **Launch Sites:** YouTube + xHamster covered; **luxuretv + xxxbp.tv need custom plugins/handlers** before equal v1 acceptance.
