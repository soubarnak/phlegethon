# Phlegethon

A desktop download manager with first-class video intelligence, targeting Void Linux (glibc and musl) and released as an xbps package.

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
A browser extension that hands URLs, cookies, or download intents to the desktop Download Manager; optional for the first shippable desktop slice. Cookie hand-off reuses the Core cookie channel's shape (`cookies-from-browser` / cookie file) but applies it to the handed-off job only, as a per-job cookie session; it never replaces the user's persisted cookie settings.
_Avoid_: required for basic downloads, browser-only downloader, a second unrelated cookie store

**Launch Site**:
One of the four equal v1 must-have sources: YouTube, xxxbp.tv, luxuretv, and xHamster — each must meet the hand-off spec’s probe/download acceptance bar (at minimum single-video probe and quality list; multi-item list pages where the site supports them).
_Avoid_: nice-to-have site, phased secondary extractor (for these four), silent generic fallback as “supported”

**Download Core**:
The Rust library that probes URLs, manages the download queue, resumes transfers, and writes files — linked directly into the Tauri shell as a Rust crate (no C ABI).
_Avoid_: UI layer, per-platform download logic, FFI layers for shells that no longer exist

**Platform Shell**:
The single UI host: a Tauri 2 webview shell on Linux (Void Linux is the release target) that talks to the Download Core and renders the IBM Carbon Design System UI. The former WinUI 3 shell was removed (see `docs/adr/0001-drop-windows-shell.md`).
_Avoid_: a second shell per OS, Electron, Geist tokens (replaced by Carbon)

**Extractor Toolchain**:
The mature external media-extraction capability (e.g. yt-dlp-class) wrapped by the Download Core for probing and fetching Launch Site media; gaps are filled by Extractor Plugins first, with Download Core native handlers only when a site cannot be expressed as a plugin.
_Avoid_: from-scratch extractors for every site, UI-owned scraping, silent generic extractor as Launch Site coverage

**Extractor Plugin**:
A Phlegethon-owned yt-dlp plugin shipped as an app-managed sidecar asset (updateable independently of a full app install) that provides Launch Site coverage the upstream toolchain lacks.
_Avoid_: relying on upstream merge for gap sites, user-installed untrusted plugins

**Probe-then-Confirm**:
The default flow for multi-item sources (channels, playlists, batches): discover items and qualities, present a selectable list (all selected by default), start downloads only after explicit user confirmation.
_Avoid_: silent full-channel download, auto-start without review

**Default Quality Policy**:
Settings-backed rule for pre-selecting probe qualities (default: best ≤ 1080p, prefer mp4). The probe sheet still lets the user override per item; audio-only mode, when enabled, overrides this video policy.
_Avoid_: forcing best-available with no height cap as the only option, hiding overrides, remembering-per-host as a substitute for an explicit default

**Download Archive**:
A Library Root–scoped record of completed media identities used to detect “already downloaded” items during probe and to back History. Probe marks archived items; confirm skips them by default unless the user re-selects for re-download. A failed retry never replaces a Completed entry for the same media id.
_Avoid_: filename-only matching as the primary detector, a separate History store that disagrees with the archive

**Filename Template**:
A settings-backed naming pattern for extractor job outputs using a small yt-dlp-like variable set (title, id, uploader, upload_date, ext). Plain HTTP jobs keep URL-derived names unless overridden. The template names one file inside the Job Destination: path separators, absolute paths and `.`/`..` are rejected, and names are capped below the 255-byte filesystem limit. Collisions get a numeric suffix; completed files are never overwritten silently.
_Avoid_: arbitrary full yt-dlp template passthrough as the only mode, fixed non-configurable names only

**Library Root**:
The user-chosen base directory that is the default parent for Job Destinations; set in settings, not hardcoded by the app.
_Avoid_: hardcoded save path, app-managed-only storage, required parent of every override path

**Job Destination**:
The folder where a specific download job writes its files — always a job-owned directory (unique under Library Root by default, or a unique subfolder under a user-picked absolute parent when Save-to overrides). The job never treats a shared user folder as its own destination.
_Avoid_: scattering batch items across unrelated folders by default, writing single-item overrides into a shared folder verbatim, requiring every job to stay under Library Root

**Discard**:
User action that removes a job from the queue; for in-flight, Paused, or Failed jobs it also stops the transfer and deletes that job’s files under its Job Destination (including partials); for Completed jobs it removes the queue row only and leaves files on disk. If the files cannot be deleted, the job stays in the queue so Discard can be retried. A Discard that arrives as a transfer finishes is honoured with the Completed-job behaviour (row removed, file kept).
_Avoid_: Cancel (as the product name for this action), Pause, deleting Completed outputs, leaving orphaned partials after removing an unfinished job

**Resumable Transfer**:
A download that can continue from the last successful byte after interruption (including app exit or crash), with automatic retry/backoff and explicit Paused / Failed / Retry states until complete or Discarded. On relaunch, jobs that were Downloading auto-resume; user-Paused stay Paused; Failed stay Failed until Retry. A single-connection job resumes at the `.part` length. A Segmented Transfer resumes from its persisted segment table (see ADR 0004): Pause, failure and crash keep every byte already written, and only the unfinished part of each segment is requested again.
_Avoid_: restart-from-zero on every failure, silent delete of partials, requiring manual Resume after every crash for in-flight jobs, treating a segmented `.part` length as progress

**Segmented Transfer**:
A general HTTP(S) download that uses multiple concurrent Range connections when the server allows, falling back to a single connection otherwise.
_Avoid_: always-single-connection only, requiring Range for every host

**Mirror Set**:
A set of equivalent source URLs for one job, discovered from structured sources (Metalink / `.meta4` and page-local mirror lists) or added by the user — not from web-wide hash/filename search.
_Avoid_: web-wide mirror search engines as v1 discovery

**Integrity Check**:
Post-completion verification against a known hash when one is available (metalink, sidecar, or user-supplied); if no expected hash exists, the job completes without verification. Mismatch marks the job Failed and leaves the file on disk for the user to inspect, Retry, or Discard.
_Avoid_: failing jobs that have no hash, auto-deleting mismatched files, requiring hashes for Launch Site media

**Active Job Limit**:
The maximum number of jobs allowed to transfer at once (default 3, user-configurable). Queue order controls which waiting jobs start next; an already-active job is not preempted when the queue is reordered.
_Avoid_: unlimited simultaneous jobs as the default, preempting in-flight transfers on reorder

**Bandwidth Cap**:
An optional global and/or per-job throughput limit; default is uncapped. Schedule-based caps (e.g. nights-only) are out of v1.
_Avoid_: always-throttled default, schedule-only controls as the v1 mechanism
