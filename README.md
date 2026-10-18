# Phlegethon

A download manager for Void Linux. Paste or drop links, review what was found, pick the quality, download. Playlists and creator pages are listed first so you choose which items to keep.

It downloads from almost any link:

| Link is... | What happens |
| --- | --- |
| A file (video, audio, archive) | Plain HTTP with resume and segmented transfer |
| A site yt-dlp knows (YouTube, xHamster, ...) | yt-dlp's own extractor, with a quality list |
| A site yt-dlp only handles generically | Generic extraction, shown with a warning |
| A page that embeds a video | The page is scanned for the media URL |
| Nothing works | A clear error and a "Try plain download" button |

luxuretv.com and xxxbp.tv have their own extractor plugins in this repo.

## Install on Void Linux

Each [GitHub release](https://github.com/soubarnak/phlegethon/releases) is a signed xbps repository. Add it once, then install:

```sh
echo 'repository=https://github.com/soubarnak/phlegethon/releases/latest/download' | sudo tee /etc/xbps.d/20-phlegethon.conf
sudo xbps-install -S phlegethon
```

xbps picks the glibc or the musl index for your system. The first `xbps-install -S` shows the signing key fingerprint and asks whether to import the key. Import it only when the fingerprint is:

```
fa:d1:00:32:ce:b1:b9:fd:73:05:05:76:75:54:37:25
```

Do not pass `-y` on that first install, because it skips the fingerprint check. Later updates come from `sudo xbps-install -Su`.

Releases are built by `.github/workflows/release.yml` when a `phlegethon-v*` tag is pushed, or when the workflow is run by hand with a `version` (it creates the tag on the commit it built, and refuses a version whose tag already exists). The same build runs locally in a pinned Void container. Podman is the default container engine; set `CONTAINER_ENGINE=docker` to use Docker instead:

```sh
make dist              # frontend + source tarball
make xbps-container    # podman run --privileged on the Void image pinned in packaging/void/images.env
make xbps-container LIBC=musl
ls out/glibc/          # the .xbps
```

The container needs `--privileged` for xbps-src's `uchroot` build chroot. GitHub Actions uses Docker. Rootless podman runs `binary-bootstrap` but then fails in the package step with `/dev/null: Read-only file system`, so a local rootless podman build does not finish yet. Rootful podman and Docker are untested here; on a Void host use `scripts/xbps-build.sh`.

Or build it with xbps-src on your Void machine:

```sh
scripts/setup-void.sh          # build and runtime dependencies (xbps-install)
make dist                      # builds the frontend and the source tarball
scripts/xbps-build.sh          # clones void-packages, runs xbps-src
sudo xbps-install --repository=.void-packages/hostdir/binpkgs phlegethon
```

Or install straight from a checkout, without xbps-src:

```sh
scripts/setup-void.sh
make
sudo make install PREFIX=/usr
```

Install a portal backend for your desktop so the folder chooser works: `xdg-desktop-portal-gtk` or `xdg-desktop-portal-kde` (`xdg-desktop-portal-wlr` only does screen capture, not file choosing). Without one the app falls back to `zenity`, `kdialog`, or a typed path. On niri, sway and other tiling compositors you also need a notification daemon; see [docs/tiling-compositors.md](docs/tiling-compositors.md).

### glibc and musl

| libc | yt-dlp | Notes |
| --- | --- | --- |
| glibc | xbps `yt-dlp`, or the standalone binary the Toolchain panel installs | Nothing else needed |
| musl | xbps `yt-dlp` (needs `python3`) | The standalone Linux binaries are glibc-only; the panel installs the universal zipapp instead |

## How it finds yt-dlp

In order: the app's sidecar directory, `YTDLP_PATH`, `yt-dlp` on `PATH`, then `python3 -m yt_dlp`. **Settings, Toolchain** shows what was found (yt-dlp, ffmpeg, ffprobe, JavaScript runtime, platform) and can install the latest yt-dlp release, checked against its published SHA2-256 sums.

YouTube needs a JavaScript runtime. The app uses deno (2.3 or newer), node (22 or newer), quickjs or bun, whichever it finds. `nodejs` is an xbps dependency for this reason.

## Using it

| Do this | To get this |
| --- | --- |
| Paste (Ctrl+V), drop, or type URLs in the box | One or many URLs queued; sites that need a decision open a review sheet |
| **Save to...** | Choose a destination for this batch |
| Review sheet | Tick items, choose quality per item, choose media extras (subtitles, thumbnail, ...), then **Download** |
| Queue tab | Pause, resume, retry, reorder, reveal in folder, open file, add a mirror, discard |
| History tab | Re-download or remove finished and failed items |
| Settings | Library folder, job limit, default quality, filename template, cookies, proxy, bandwidth cap, extraction tiers, Toolchain |

luxuretv sits behind a Cloudflare check. It works when yt-dlp has browser impersonation (**Settings, Toolchain** shows *Browser impersonation: Found*, which needs `curl_cffi`), or when you set **Settings, Cookies** to your browser. The cookie is tied to your browser and network, so use the same machine. Reference URLs and their latest results are in [docs/reference-urls.md](docs/reference-urls.md).

## Browser companion (optional)

A Chrome extension in `packages/chrome-companion` sends the current tab to the running app. Register the native messaging host with your extension id:

```sh
scripts/install-nmh-linux.sh --extension-id <32 letters a-p>
```

Details: [docs/companion-setup.md](docs/companion-setup.md).

## Develop

```sh
make test             # Rust (no WebKit needed), plugin tests, frontend tests
make test-shell       # the Tauri crate (needs libwebkit2gtk41-devel, gtk+3-devel, libsoup3-devel)
cd packages/tauri-shell/frontend && npm run dev     # UI in a browser, Tauri calls unavailable
cargo tauri dev       # full app with hot reload (cargo install tauri-cli)
make probe-check      # probe the reference URLs against the real toolchain
make e2e-ui           # drive the real app window on every reference URL (WebDriver)
```

A plain `cargo build --release` of the shell needs `--features phlegethon-tauri/custom-protocol`; without it the window loads the dev server URL. `scripts/build-linux.sh` and the xbps template pass it.

### Layout

| Path | What |
| --- | --- |
| `packages/download-core` | Rust Download Core: queue, probe, tiers, HTTP and yt-dlp transfers |
| `packages/tauri-shell` | Tauri 2 shell (`src-tauri`) and the React + IBM Carbon UI (`frontend`) |
| `packages/shell-support` | Folder picker, notification and native-messaging helpers (no Tauri dependency) |
| `packages/extractor-plugins` | yt-dlp plugins for luxuretv and xxxbp.tv |
| `packages/native-messaging-host`, `companion-protocol`, `chrome-companion` | Browser companion |
| `packages/shell-strings` | User-facing strings |
| `packaging/` | `.desktop`, icons, AppStream, the xbps template |
| `docs/` | Spec, ADRs, reference URLs |

## Troubleshooting

| Symptom | Fix |
| --- | --- |
| "yt-dlp not found" | Install the `yt-dlp` package, or use Settings, Toolchain, Install |
| YouTube lists only low qualities or fails | Install `nodejs`; check Toolchain shows a JavaScript runtime |
| "needs browser cookies" | Use a yt-dlp with `curl_cffi` (Toolchain panel), or Settings, Cookies, pick your browser; retry |
| Folder button asks for a typed path | Install `xdg-desktop-portal` and a backend, or `zenity` |
| Window shows a connection error | The binary was built without `custom-protocol` |
| No desktop notifications | Install `libnotify` (`notify-send`) and run a notification daemon |
