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
One of the four equal v1 must-have sources: YouTube, xxxbp.tv, luxuretv, and xHamster — each must meet the hand-off spec’s probe/download acceptance bar (at minimum single-video probe and quality list; multi-item list pages where the site supports them).
_Avoid_: nice-to-have site, phased secondary extractor (for these four), silent generic fallback as “supported”

**Download Core**:
The shared Rust library that probes URLs, manages the download queue, resumes transfers, and writes files — used by both platform shells.
_Avoid_: UI layer, per-platform download logic

**Platform Shell**:
The OS-native UI host: WinUI 3 on Windows 11, Tauri 2 on Debian — both talk to the Download Core and follow the Geist-inspired design system.
_Avoid_: single cross-platform UI framework (for this product), Electron

**Extractor Toolchain**:
The mature external media-extraction capability (e.g. yt-dlp-class) wrapped by the Download Core for probing and fetching Launch Site media; gaps are filled by Extractor Plugins first, with Download Core native handlers only when a site cannot be expressed as a plugin.
_Avoid_: from-scratch extractors for every site, UI-owned scraping, silent generic extractor as Launch Site coverage

**Extractor Plugin**:
A Phlegethon-owned yt-dlp plugin shipped as an app-managed sidecar asset (updateable independently of a full app install) that provides Launch Site coverage the upstream toolchain lacks.
_Avoid_: relying on upstream merge for gap sites, user-installed untrusted plugins

**Probe-then-Confirm**:
The default flow for multi-item sources (channels, playlists, batches): discover items and qualities, present a selectable list (all selected by default), start downloads only after explicit user confirmation.
_Avoid_: silent full-channel download, auto-start without review

**Library Root**:
The user-chosen base directory that is the default parent for Job Destinations; set in settings, not hardcoded by the app.
_Avoid_: hardcoded save path, app-managed-only storage, required parent of every override path

**Job Destination**:
The folder where a specific download job writes its files — by default a path under the Library Root (batch/channel jobs get a dedicated subfolder derived from the source); the user may override it to any absolute folder on disk for that job.
_Avoid_: scattering batch items across unrelated folders by default, requiring every job to stay under Library Root

**Discard**:
User action that removes a job from the queue; for in-flight, Paused, or Failed jobs it also stops the transfer and deletes that job’s files under its Job Destination (including partials); for Completed jobs it removes the queue row only and leaves files on disk.
_Avoid_: Cancel (as the product name for this action), Pause, deleting Completed outputs, leaving orphaned partials after removing an unfinished job

**Resumable Transfer**:
A download that can continue from the last successful byte after interruption, with automatic retry/backoff and explicit Paused / Failed / Retry states until complete or Discarded.
_Avoid_: restart-from-zero on every failure, silent delete of partials

**Segmented Transfer**:
A general HTTP(S) download that uses multiple concurrent Range connections when the server allows, falling back to a single connection otherwise.
_Avoid_: always-single-connection only, requiring Range for every host

**Mirror Set**:
A set of equivalent source URLs for one job, discovered from structured sources (Metalink / `.meta4` and page-local mirror lists) or added by the user — not from web-wide hash/filename search.
_Avoid_: web-wide mirror search engines as v1 discovery

**Bandwidth Cap**:
An optional global and/or per-job throughput limit; default is uncapped. Schedule-based caps (e.g. nights-only) are out of v1.
_Avoid_: always-throttled default, schedule-only controls as the v1 mechanism
