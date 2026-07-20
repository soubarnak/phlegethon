# Phlegethon

A desktop download manager with first-class video intelligence, targeting Windows 11 and Debian Linux.

## Language

**Download Manager**:
The product itself — an app that fetches files from arbitrary URLs, with video probing and batch selection as first-class capabilities.
_Avoid_: video downloader (as the primary name), downloader-only, FDM clone

**Video Intelligence**:
The ability to probe a URL, discover available media items and qualities, and let the user select what to fetch — especially for channels, playlists, and single videos.
_Avoid_: scraping (as the user-facing term), auto-download-everything (without selection)

**Hand-off Spec**:
The destination of the current wayfinding effort — a product and engineering specification clear enough that implementation can start without reopening locked decisions.
_Avoid_: MVP build, working prototype (as the map destination)

**Chrome Companion**:
A browser extension that hands URLs, cookies, or download intents to the desktop Download Manager; optional for the first shippable desktop slice.
_Avoid_: required for basic downloads, browser-only downloader

**Launch Site**:
One of the four equal v1 must-have sources: YouTube, xxxbp.tv, luxuretv, and xHamster — each must meet the same probe/download acceptance bar in the hand-off spec.
_Avoid_: nice-to-have site, phased secondary extractor (for these four)

**Download Core**:
The shared Rust library that probes URLs, manages the download queue, resumes transfers, and writes files — used by both platform shells.
_Avoid_: UI layer, per-platform download logic

**Platform Shell**:
The OS-native UI host: WinUI 3 on Windows 11, Tauri 2 on Debian — both talk to the Download Core and follow the Geist-inspired design system.
_Avoid_: single cross-platform UI framework (for this product), Electron

**Extractor Toolchain**:
The mature external media-extraction capability (e.g. yt-dlp-class) wrapped by the Download Core for probing and fetching Launch Site media; custom handlers only fill gaps.
_Avoid_: from-scratch extractors for every site, UI-owned scraping

**Probe-then-Confirm**:
The default flow for multi-item sources (channels, playlists, batches): discover items and qualities, present a selectable list (all selected by default), start downloads only after explicit user confirmation.
_Avoid_: silent full-channel download, auto-start without review

**Library Root**:
The user-chosen base directory under which all downloads are stored.
_Avoid_: hardcoded save path, app-managed-only storage

**Job Destination**:
The folder for a specific download job, under the Library Root; batch/channel jobs use a dedicated subfolder derived from the source.
_Avoid_: scattering batch items across unrelated folders by default

**Resumable Transfer**:
A download that can continue from the last successful byte after interruption, with automatic retry/backoff and explicit Paused / Failed / Retry states until complete or discarded.
_Avoid_: restart-from-zero on every failure, silent delete of partials
