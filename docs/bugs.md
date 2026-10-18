# Fixes

Bug, gap and UI report for Phlegethon at commit `110a1ff` on `main`, compiled on 2026-10-15.

This report covers the whole repository except `prototypes/`. It lists defects, a UI layout pass aimed at niri and other tiling Wayland compositors, functional improvements, and gaps between the code, the glossary in `CONTEXT.md`, the tests and CI. It does not change any code. Section 11 records which pull request fixed each finding; the rest of the report describes the code as it was at `110a1ff`. Terms such as Download Core, Platform Shell, Job Destination, Library Root, Discard, Launch Site and Resumable Transfer are used as `CONTEXT.md` defines them.

## Contents

1. [How to read this report](#1-how-to-read-this-report)
2. [Summary and fix order](#2-summary-and-fix-order)
3. [Test, build and lint results](#3-test-build-and-lint-results)
4. [Bugs](#4-bugs)
5. [UI pass for niri and other tiling compositors](#5-ui-pass-for-niri-and-other-tiling-compositors)
6. [Functional improvement opportunities](#6-functional-improvement-opportunities)
7. [Gaps](#7-gaps)
8. [CODE_REVIEW.md reconciliation](#8-code_reviewmd-reconciliation)
9. [Method and limitations](#9-method-and-limitations)
10. [Not inspected](#10-not-inspected)
11. [Status](#11-status)

## 1. How to read this report

Every location is a repo-relative `path:line` or `path:start-end` at commit `110a1ff`. Line numbers drift as soon as code changes, so search for the quoted symbol if a line no longer matches.

**Severity**

| Level | Meaning |
| --- | --- |
| Blocker | Silent data corruption, or a crash or permanent failure on a normal path. Fix before the next release. |
| High | Wrong results, a security exposure, or a frozen or stuck app under plausible conditions. |
| Medium | Wrong behaviour in edge cases, or degraded use that has a workaround. |
| Low | Latent defects, hygiene, minor UX. |

**Confidence**

| Label | Meaning |
| --- | --- |
| Verified | The cited lines were re-read while this report was assembled, and the mechanism holds. |
| Confirmed | A reviewer read and quoted the code path. The mechanism follows from the code. Nothing was executed. |
| Suspected | The outcome depends on runtime, server, yt-dlp, Carbon, GTK or niri behaviour that was not checked. |

No defect in this report was reproduced at runtime. The test suite passes, so most of these defects are in paths the tests do not cover (see [7.4](#74-test-coverage-gaps)).

## 2. Summary and fix order

| Section | Count |
| --- | --- |
| Blockers | 2 |
| High | 25 |
| Medium | 71 |
| Low | 61 |
| UI layout findings | 18 |
| Gaps (CI, tests, docs, spec, hygiene) | see section 7 |

The suggested fix order puts data integrity first, then problems users hit every day, then exposure:

1. **B-1** Transfer finish uses a stale queue index. One Discard or reorder during a download can poison the Core mutex or mark the wrong job Completed.
2. **B-2** Range replies are never checked. Pause and Resume against a server without Range support produce a corrupt file that is marked Completed.
3. **H-2, H-3** The Bandwidth Cap does not cap. A throttled write is treated as end of body, which can also complete a truncated file.
4. **H-1** A Chrome Companion hand-off is enqueued twice and can sit hidden and idle.
5. **H-9** Network, ffmpeg and subprocess work runs while the Core mutex is held, so the window freezes.
6. **UI-1, UI-2, UI-4** The queue needs about 1050 px of window width. On a 1080p niri output only the 2/3 or full-width column shows it without sideways scrolling, and 480 px `minWidth` breaks the 1/3 preset on smaller outputs.
7. **H-11** yt-dlp gets URLs with no `--` separator, so a crafted playlist entry can inject options such as `--exec`.
8. **H-12** Launch Site subdomains (`m.xhamster.com`, `en.xxxbp.tv`) fall through to the silent generic fallback that `CONTEXT.md` forbids.
9. **H-18, H-21** The companion socket is unauthenticated, and session cookie files are written with default permissions and never deleted.
10. **H-22** The Extractor Toolchain install skips checksum verification whenever the SUMS file is missing or fails to download.
11. **G-1** No CI runs on pull requests or pushes, and `cargo fmt --check` already fails on 49 files.

## 3. Test, build and lint results

These commands were run on the reviewing machine against the clean tree.

| Command | Result |
| --- | --- |
| `cargo test --workspace --exclude phlegethon-tauri --locked` | Pass. download-core lib 215 (2 ignored), `core_api` 160, ytdlp integration 14, shell-support 18, companion-protocol 6, `plugin_sidecar_install` 2, `reference_urls_live` 6 (1 ignored), native-messaging-host 1. |
| `cargo test -p phlegethon-tauri` | Pass, 5 tests. webkit2gtk 4.1 is present on the machine. |
| `cargo fmt --check` | **Fail.** 49 files differ. The largest diffs are `packages/download-core/tests/core_api.rs` (146 hunks), `packages/download-core/src/ytdlp_extractor.rs` (53) and `packages/download-core/src/lib.rs` (36). |
| `cargo clippy --workspace --all-targets` | Warnings only, no errors. See [7.6](#76-code-hygiene). |
| `python3 packages/extractor-plugins/tests/test_parse_fixtures.py` | Pass, 23 tests. |
| `python3 packages/extractor-plugins/tests/test_extractors_offline.py` | All 9 tests **skipped**, because `yt_dlp` is not importable. The script exits 0 while checking nothing. |
| `npm ci` | Pass, 0 vulnerabilities. |
| `npm run typecheck` and `npm test` | Pass, 89 tests. |
| `npm run build` | Pass. JS bundle 658.98 kB (193.75 kB gzip), over Vite's 500 kB warning. CSS 1,228 kB. |
| Version check (six places plus README) | All 0.1.0 and consistent. A seventh location, `packages/extractor-plugins/phlegethon-launch-sites/setup.cfg:3`, says 0.1.0 while the plugin sidecar is 0.2.0. |
| Plugin manifest against `packages/download-core/src/plugin_sidecar.rs:49-53` | Version and extractor keys match. |
| i18n key check | No missing keys. 9 unused keys (see [7.6](#76-code-hygiene)). |
| `cargo tree --duplicates` | 60 duplicate crates. |
| `cargo audit` | Not installed, so not run. |

`make e2e-ui` was not run. The UI pass in section 5 is a static review of CSS, markup, Carbon source and window configuration.

## 4. Bugs

### 4.1 Blockers

#### B-1. Transfer finish uses a stale queue index

- **Location:** `packages/download-core/src/lib.rs:2627` (`job_index: i`), `packages/download-core/src/lib.rs:2781` (`self.jobs[job_index]`), `packages/download-core/src/lib.rs:3049-3066`, `packages/download-core/src/lib.rs:1392` (`self.jobs.remove(pos)` in `finalize_discard`), `packages/download-core/src/lib.rs:1572` (`self.jobs.remove(from_index)` in `reorder_job`).
- **Problem:** The transfer plan records the job's position in `Core::jobs` when the transfer starts. The transfer then runs with the Core mutex released. Discard and reorder shift positions in the vector during that time. `finish_unlocked_transfer` indexes `self.jobs[job_index]` without resolving the job by id. The segmented path (`packages/download-core/src/lib.rs:2829`), the extractor path (`packages/download-core/src/lib.rs:2864`) and `fail_or_failover` do the same.
- **Failure:** The queue is [A Pending, B Downloading]. The user discards A. When B finishes, the code indexes position 1 of a one-element vector and panics while the Core mutex guard is live. The mutex is poisoned and every later command fails until restart. With a reorder instead of a discard, there is no panic: the wrong job is marked Completed and its filename is overwritten.
- **Fix:** Carry the job id in the transfer outcome and resolve the index by id at finish time, as `sync_job_progress` already does. Assert that `jobs[idx].id() == job_id` before any mutation.
- **Confidence:** Verified. The reorder test at `packages/download-core/tests/core_api.rs:5487-5549` stops before the transfer completes, so it cannot catch this.

#### B-2. Range replies are never checked, so a server that ignores Range produces a corrupt file marked Completed

- **Location:** `packages/download-core/src/http.rs:183-232` (`fetch` never checks for status 206 or `Content-Range`), `packages/download-core/src/transfer.rs:152` (`OffsetWriter` writes with no length cap), `packages/download-core/src/lib.rs:2730-2745` (resume sends `Range` and opens the `.part` in append mode), `packages/download-core/src/lib.rs:2801` and `packages/download-core/src/lib.rs:2849` (completion tests `on_disk >= total`).
- **Problem:** A single-connection resume always sends `Range: bytes=N-` and appends whatever body comes back. The segmented path writes each response at its segment offset. Neither path checks that the server honoured the range, and completion accepts any file at least as long as the expected total.
- **Failure (resume):** A job starts as a single fetch because HEAD did not advertise Range support, which is common for dynamic origins. The user pauses at 400 of 1000 bytes and resumes. The server replies 200 with the whole body. The `.part` becomes 1400 bytes, `1400 >= 1000` passes, and the job is Completed with the first 400 bytes duplicated at the start. Resumable Transfer after a crash takes the same path.
- **Failure (segmented):** HEAD returns `Accept-Ranges: bytes`, but the GETs ignore Range. Each of four segments writes the full body at its own offset, the file grows past the total, and the job completes with scrambled content.
- **Related:** On a 206 reply, `Content-Length` is the remaining length, not the object size (`packages/download-core/src/http.rs:199-202`, consumed at `packages/download-core/src/lib.rs:2779-2787`). When HEAD gave no size, a resumed fetch takes the remainder as the total and completes early.
- **Fix:** Require status 206 and a `Content-Range` whose start equals the requested offset. On a 200 reply, truncate the `.part` and restart as a single fetch. Cap each segment writer at its range length. Compare the final size with `==`. Take the total from `Content-Range`. Store `ETag` or `Last-Modified` and send `If-Range` on resume.
- **Confidence:** Verified for the code path. ureq 2.12.1 returns `Ok` for every status below 400. The server behaviour is the scenario, not an observation. An Integrity Check catches the corruption only when an expected hash exists.

### 4.2 High

#### H-1. A Chrome Companion hand-off is enqueued twice and can sit hidden and idle

- **Location:** `packages/tauri-shell/src-tauri/src/companion.rs:58` (`core.enqueue(&url, None)`), `packages/tauri-shell/src-tauri/src/companion.rs:73` (emits `companion-intent` afterwards), `packages/tauri-shell/frontend/src/App.tsx:155-157` (`if (intent.url) void addUrls(intent.url);`), `packages/tauri-shell/frontend/src/tauri/types.ts:233-236`.
- **Problem:** The shell enqueues a direct URL and then emits the intent. The frontend treats the intent as a new URL, runs the duplicate check, and finds the job the shell just created. Enqueue emits no `core-event`, and the queue only dispatches through the `process_pending` command.
- **Failure:** The user sends a file link from the browser. `DuplicateUrlModal` reports "already queued". If the user picks Cancel or Show existing, nothing calls `refresh()` or `kick()`. The polling tick runs only while a job is active, so on an idle queue the new job is missing from the list and does not start until some unrelated action. Add anyway queues a second copy. If a small file completes before the event is handled, a second download runs.
- **Fix:** Read `intent.result`. For `enqueue_direct`, call `refresh()` and `kick()` without `addUrls`. For `probe_then_confirm`, push the URL onto the probe queue. Make the TypeScript type match the Rust `CompanionIntentEvent { url, result }`.
- **Confidence:** Verified. Rated high, not blocker, because any later UI action recovers the job.

#### H-2. A throttled write is treated as end of body

- **Location:** `packages/download-core/src/bandwidth.rs:250-253` (returns `Ok(0)` when the bucket is empty), `packages/download-core/src/http.rs:219-227` (breaks on a zero or short write and returns `Ok`), `packages/download-core/src/lib.rs:2807-2810` (unknown size: a successful fetch is complete).
- **Problem:** The rate-limited writer returns `Ok(0)` instead of waiting for tokens. The HTTP body loop reads that as a full sink and ends the fetch successfully. Nothing in `http.rs`, `bandwidth.rs` or `transfer.rs` sleeps.
- **Failure:** With a per-job cap and a server that sends no `Content-Length`, the first burst is written and the job is marked Completed with a truncated file. With a known size, the job returns to Pending and the next 500 ms frontend tick re-plans it, so throughput is one burst per tick instead of the cap, with a new GET each time.
- **Fix:** Block in the writer until tokens are available. Treat a short write without a pause request as an error. Keep the job bucket on the job instead of rebuilding it per plan (`packages/download-core/src/lib.rs:2648`).
- **Confidence:** Verified.

#### H-3. The global Bandwidth Cap is rebuilt on every dispatch round

- **Location:** `packages/download-core/src/lib.rs:2478-2480` (`shared_token_bucket` built inside `dispatch_pending`), `packages/download-core/src/lib.rs:2448-2450` (same pattern in `process_pending`).
- **Problem:** Each dispatch round creates a new global token bucket with a full one-second burst. Jobs started in different rounds draw from different buckets.
- **Failure:** With a 1 MB/s global cap, job A starts in round 1 and job B in round 2. Both run at 1 MB/s. Together with H-2, every tick grants another burst.
- **Fix:** Keep one global bucket on `Core`, created when the cap is set, and pass the same `Arc` to every plan.
- **Confidence:** Verified.

#### H-4. Segmented transfers accept holes, and crash recovery resumes past them

- **Location:** `packages/download-core/src/lib.rs:2828` (the segment byte count `_bytes_written` is discarded), `packages/download-core/src/transfer.rs:129` (count not compared with the range length), `packages/download-core/src/storage.rs:410-417` (`reconcile_job` sets `bytes_received` to the `.part` length).
- **Problem:** Segments write at independent offsets, so the `.part` length is the furthest offset written, not a contiguous prefix. After a crash, recovery treats that length as resumable progress and appends from there. Unwritten regions read back as zeros. A segment that ends early with a clean EOF is not detected either. The pause path already handles this by deleting the `.part` (`packages/download-core/src/lib.rs:2830-2836`); the crash path does not.
- **Failure:** The app is killed during a segmented download. On relaunch the job resumes after the holes and completes, without a hash, with zero-filled regions.
- **Fix:** Persist per-segment progress, or truncate and restart a segmented `.part` on relaunch. Check each segment's byte count against its range.
- **Confidence:** Verified for recovery. The sparse-file outcome is standard POSIX behaviour.

#### H-5. Mirror failover appends at a stale offset and mixes mirrors

- **Location:** `packages/download-core/src/lib.rs:2814` (single-fetch error goes to `fail_or_failover` without `sync_job_progress` and without removing the `.part`), `packages/download-core/src/lib.rs:2582`, `packages/download-core/src/lib.rs:2744` (`append: bytes_received > 0`), `packages/download-core/src/job.rs:401-402` (`advance_mirror` does not reset bytes).
- **Problem:** Bytes written by the failed attempt are not added to `bytes_received`, so the next mirror is asked for the old offset and its body is appended after the newer bytes. Nothing checks that the mirrors serve identical content. The segmented branch removes the partial on error (`packages/download-core/src/lib.rs:2825`), so the two paths disagree.
- **Failure:** A resumed job on a Mirror Set writes more bytes, then errors. Failover appends from the old offset and duplicates content, and `>=` completes it. With mirrors that serve different builds, the file is a mix of both.
- **Fix:** Sync progress before failover, or derive the offset from the `.part` length. Record validators on the first response and restart from zero when the next mirror's validators differ.
- **Confidence:** Confirmed.

#### H-6. A panic in a transfer thread leaves the job active forever

- **Location:** `packages/download-core/src/lib.rs:627` and `packages/download-core/src/lib.rs:670` (`transfer_active.store(false, …)` is not in a drop guard), `packages/download-core/src/lib.rs:826` (`h.join().unwrap()` on segment threads).
- **Problem:** A panic inside a transfer skips clearing `transfer_active`. A panic in a segment thread propagates through `unwrap` into a caller that may hold the Core lock.
- **Failure:** The job shows Downloading forever, it keeps one Active Job Limit slot, Discard returns Ok without finalizing, and sidecar updates stay blocked as "mid-transfer".
- **Fix:** Clear the flag and report a failed outcome from an RAII guard. Map a join error to a segment error.
- **Confidence:** Confirmed. No panic path on network data was found, so this is a robustness gap.

#### H-7. Changing the Library Root during a transfer starts a second transfer or orphans the job

- **Location:** `packages/tauri-shell/src-tauri/src/lib.rs:515-534` (no active-transfer check), `packages/download-core/src/lib.rs:1008-1011` (`self.jobs = storage::load_jobs(&root)?` then `register_job_signals`), `packages/download-core/src/job.rs:227-229` (fresh `transfer_active` flag), `packages/download-core/src/job_signals.rs:28-33` (`insert` overwrites the running thread's signals).
- **Problem:** `set_library_root` replaces the live `Job` objects and their signal flags while transfer threads still run.
- **Failure:** The user re-picks the same root during a download. The job reloads as Pending with `transfer_active == false`, and the next tick starts a second transfer that writes the same `.part`. With a different root, the job disappears, its thread finishes with "job not found", and partials stay under the old root.
- **Fix:** Refuse a root change while transfers are active, or pause them and persist the old queue first.
- **Confidence:** Confirmed.

#### H-8. A corrupt `jobs.json` makes the Library Root unusable while the shell records it as set

- **Location:** `packages/download-core/src/storage.rs:173-185` (all-or-nothing parse, one bad row fails the load), `packages/tauri-shell/src-tauri/src/lib.rs:1142-1150` (logs the error, then sets the stored root anyway), `packages/tauri-shell/src-tauri/src/lib.rs:246-251` (`ensure_library_root` returns early when a root is stored).
- **Problem:** Core's root stays unset, but the shell believes a root is set and re-persists it on every launch. The bad file is never moved aside.
- **Failure:** After a crash truncates `jobs.json`, every enqueue, probe and companion request fails with "Library Root not set" until the user re-picks a folder. Because release builds have no logger (M-47), nothing records why.
- **Fix:** Move a corrupt file to `jobs.json.corrupt-<timestamp>`, start with an empty queue, and show the error. Skip bad rows one at a time. Store the shell's root only after Core accepts it.
- **Confidence:** Verified for the shell side.

#### H-9. Network, ffmpeg and subprocess work runs while the Core mutex is held

- **Location:**
  - HEAD requests in transfer planning: `packages/download-core/src/lib.rs:2651` (`supports_range`), `packages/download-core/src/lib.rs:2659` and `packages/download-core/src/lib.rs:2695` (`file_size`), `packages/download-core/src/lib.rs:1098` (disk-space guard).
  - ffmpeg remux in `finish_unlocked_transfer`: `packages/download-core/src/media_extras.rs:176-185` (`Command::status()` with no timeout and no kill), `packages/download-core/src/lib.rs:2870`.
  - Sidecar work: `packages/tauri-shell/src-tauri/src/lib.rs:1158-1167` (on-launch apply under the lock), `packages/tauri-shell/src-tauri/src/lib.rs:350-352` (the comment says "snapshot the extractor handle, not the lock", but the code calls `toolchain_status()` under the lock), `packages/tauri-shell/src-tauri/src/lib.rs:911-913` (non-async `apply_sidecar_updates` command).
- **Problem:** The rest of the design releases the lock for network work, but these paths hold it across HEAD requests (10 s connect and 30 s read timeouts, `packages/download-core/src/http.rs:131-132`), multi-minute ffmpeg remuxes, GitHub downloads and 15 s `--version` probes. Non-async Tauri commands run on the main thread and block on the same mutex.
- **Failure:** A slow origin, a long remux, or a hung yt-dlp during the Toolchain panel freezes pause, discard, list and enqueue, and with them the window. A Discard during a remux does not stop ffmpeg, which contradicts the Discard definition ("stops the transfer").
- **Fix:** Do HEAD before taking the lock and re-validate under it. Run remux on a worker outside the lock and kill it on stop. Use the snapshot pattern of `install_toolchain_latest` (`packages/download-core/src/lib.rs:1752-1768`) for the on-launch apply and `toolchain_status`. Make I/O commands `async`.
- **Confidence:** Confirmed. Freeze duration is an estimate.

#### H-10. Pausing an extractor download makes Resume fail

- **Location:** `packages/download-core/src/lib.rs:2602-2609` (a job with partial bytes and no extractor resume support is failed with "use Retry"), `packages/download-core/src/extractor.rs:279-281` (`supports_extractor_resume` returns false; yt-dlp does not override it).
- **Problem:** Pause stops yt-dlp and leaves partial files. Resume returns the job to Pending, and the next plan fails it. Retry starts from zero.
- **Failure:** Every paused YouTube or other Launch Site job becomes Failed on Resume. This contradicts Resumable Transfer.
- **Fix:** Use yt-dlp's own `--continue` behaviour for resume, or hide Pause for jobs that cannot resume.
- **Confidence:** Verified.

#### H-11. yt-dlp gets untrusted URLs as bare positional arguments

- **Location:** `packages/download-core/src/ytdlp_extractor.rs:561` (probe), `packages/download-core/src/ytdlp_extractor.rs:1045` (download, `args.push(url.to_string())`), `packages/download-core/src/ytdlp_extractor.rs:724-750` (playlist entry URL taken from JSON without validation), `packages/download-core/src/ytdlp_extractor.rs:953-963` (`probe_item_qualities` skips the `Url::parse` gate).
- **Problem:** There is no `--` before the URL. Playlist and channel entry URLs come from extractor JSON and are stored as is.
- **Failure:** A listing returns an entry whose `url` is `--exec=sh -c '…'`. After the user confirms, yt-dlp parses it as an option and runs the command. `--plugin-dirs=/path` works the same way. This needs attacker-controlled extractor output, for example from a hostile page handled by a generic extractor.
- **Fix:** Insert `--` before the URL in both argument builders. Parse each entry URL with `url::Url::parse` and require http or https.
- **Confidence:** Verified for the missing separator and validation. The argparse behaviour is standard but was not executed.

#### H-12. Launch Site subdomains fall through to the generic fallback

- **Location:** `packages/download-core/src/classification.rs:58-75` (exact host match against `LAUNCH_SITES`), `packages/download-core/src/ytdlp_extractor.rs:471-487` (suffix match for the expected extractor), `packages/download-core/src/lib.rs:240` (`is_launch_site` in `ProbePlan`), `packages/download-core/src/lib.rs:276` (`allow_generic`).
- **Problem:** The two host lists are identical, but one matches exact hosts and the other matches subdomains. `ProbePlan` uses the exact match.
- **Failure:** `m.xhamster.com`, `de.xhamster.com`, `music.youtube.com` and `en.xxxbp.tv` are probed as ordinary sites: HEAD, then yt-dlp with the generic extractor allowed, then page sniffing. The user can get page-sniffed items with only a warning, which is the "silent generic fallback as supported" that `CONTEXT.md` lists under _Avoid_. The xxxbp and luxuretv plugin regexes accept only `www.` or `en.` hosts (`packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/xxxbp.py:39`, `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/luxuretv.py:25`), so other subdomains fail there too.
- **Fix:** Use one shared Launch Site table with exact-or-dot-suffix matching in `classification.rs`, `ProbePlan` and the extractor map. Add subdomain tests.
- **Confidence:** Verified. One reviewer claimed `is_launch_site` has no runtime caller; that claim is wrong (`packages/download-core/src/lib.rs:240` and `packages/download-core/src/lib.rs:1212`).

#### H-13. HLS and DASH manifests are downloaded as plain files

- **Location:** `packages/download-core/src/page_sniffer.rs:565-582` (`is_binary_content_type` treats every non-text type as binary), `packages/download-core/src/lib.rs:309-325` (tier 0 returns a DirectFile result before yt-dlp runs).
- **Failure:** The user pastes `https://host/live/index.m3u8`. HEAD returns `application/vnd.apple.mpegurl`, the probe offers one "Original file" item, and the job downloads the playlist text instead of the stream. `.mpd` behaves the same.
- **Fix:** Send manifest content types and extensions to yt-dlp. Add a regression test.
- **Confidence:** Verified for the content-type rule. The header value is typical, not checked against a live server.

#### H-14. The yt-dlp process tree is not owned, so children outlive Pause, Discard and app exit

- **Location:** `packages/download-core/src/toolchain_discovery.rs:583-592` (kills the direct child only, then joins the pipe readers), `packages/download-core/src/ytdlp_extractor.rs:1121` (downloads run with no deadline), `packages/download-core/src/lib.rs:3188-3191` (`Drop for Core` only flushes progress). No process group, `killpg` or `PR_SET_PDEATHSIG` exists, and the shell has no exit hook.
- **Failure:** Pause during the ffmpeg merge kills yt-dlp, but ffmpeg keeps writing into the Job Destination. Quitting mid-download leaves yt-dlp running. If a grandchild keeps the pipes open, the reader `join()` blocks until it exits, so Pause and Discard can stall for the whole post-processing phase (suspected).
- **Fix:** Spawn with `process_group(0)`, kill the group on stop, deadline and exit, set `PR_SET_PDEATHSIG` as a backstop, and do not block on reader joins after a kill.
- **Confidence:** Verified for the missing group kill. The stall is suspected.

#### H-15. The Metalink hash parser drops or misassigns hashes

- **Location:** `packages/download-core/src/integrity.rs:78-108`.
- **Problem:** `tag.find("type=")` searches the rest of the document, not the current tag. The `?` inside the loop returns `None` for the whole function. Digest length is not checked against the algorithm.
- **Failure:** A Metalink 4 file with a file-level `sha-256` hash followed by `<pieces>` hashes and no later `type=` returns no hash, so the job completes unverified. When a later `type=` exists, a piece digest can be stored under the wrong algorithm and every good download fails its Integrity Check.
- **Fix:** Use an XML parser. Read only direct `<file>/<hash>` children, skip entries without a type, and check digest length per algorithm.
- **Confidence:** Verified.

#### H-16. HTTPS and `socks5h` proxies pass validation but the HTTP client silently ignores them

- **Location:** `packages/download-core/src/proxy_settings.rs:28` (accepts `https` and `socks5h`), `packages/download-core/src/http.rs:150-160` and `packages/download-core/src/http.rs:175-182` (both discard the ureq build error and keep an environment-proxy or previous agent).
- **Problem:** ureq 2.12.1 rejects those schemes. The Download Core then sends segmented, HEAD and page-sniff requests direct or through an environment proxy, while yt-dlp honours the configured proxy. ureq has no `NO_PROXY` support, so environment proxies also apply to localhost and intranet hosts.
- **Failure:** A user behind `https://corp-proxy:8443` sees yt-dlp jobs work and direct downloads fail or leak around the proxy, with no error.
- **Fix:** Validate against what the HTTP client supports, return build errors to the caller, and honour `NO_PROXY`.
- **Confidence:** Verified for the discard of the error. ureq scheme support was read from its source.

#### H-17. The DoH tunnel uses `write_all` on non-blocking sockets

- **Location:** `packages/download-core/src/doh_proxy.rs:127-153`.
- **Problem:** Both sockets are set non-blocking. When the send buffer fills, `write_all` returns `WouldBlock`, and `?` tears down the tunnel mid-body.
- **Failure:** A yt-dlp download through the DoH proxy truncates whenever the reader is slower than the origin.
- **Fix:** Keep the tunnel sockets blocking with two threads, or buffer per direction and retry on `WouldBlock`.
- **Confidence:** Verified.

#### H-18. The companion socket is unauthenticated

- **Location:** `packages/companion-protocol/src/ipc.rs:12-18` (`GenericNamespaced` resolves to the Linux abstract namespace), `packages/tauri-shell/src-tauri/src/companion.rs:34-58`.
- **Problem:** Abstract sockets have no filesystem permissions, and neither side checks peer credentials. Any process in the network namespace, including other local users, can send an enqueue frame with a session.
- **Failure:** A local process sends `{"v":1,"op":"enqueue",…,"session":{…}}`. A known file extension is enqueued and started without review, and the user's persisted cookie setting is replaced (M-51).
- **Fix:** Use a filesystem socket under `$XDG_RUNTIME_DIR` with mode 0600, or check `SO_PEERCRED` against the shell's uid. Add a per-install token. See also M-49 and M-50.
- **Confidence:** Verified for the abstract namespace and the absence of peer checks.

#### H-19. There is no single-instance guard

- **Location:** `packages/tauri-shell/src-tauri/src/lib.rs:1118-1124` (no single-instance plugin), `packages/download-core/src/storage.rs:151` (Downloading jobs reload as Pending), `packages/tauri-shell/frontend/src/state/useJobs.ts:62`.
- **Failure:** A second launch loads the same `jobs.json`, maps Downloading jobs to Pending, and starts them while the first instance is still writing the same files. Both instances persist their own queue, so the last writer wins. Both also rewrite the plugin sidecar at startup (M-34), and the second companion listener fails silently.
- **Fix:** Add `tauri-plugin-single-instance` and forward argv URLs to the running instance, or take an exclusive lock file before creating `Core`.
- **Confidence:** Verified for the missing guard. The duplicate writes were not run.

#### H-20. A missing FileChooser portal looks like a cancelled folder picker

- **Location:** `packages/shell-support/src/portal.rs:14-26` (checks only `NameHasOwner` for `org.freedesktop.portal.Desktop`), `packages/tauri-shell/src-tauri/src/lib.rs:562-604`.
- **Problem:** "Portal available" means only that the portal bus name has an owner. When the FileChooser call fails, rfd falls back to its own zenity and then returns `None`. The shell reports `path: None` with `method: Portal`, which the UI treats as a user cancel. The shell's own zenity, kdialog and manual-path fallbacks are never tried.
- **Failure:** This is a common niri setup: `xdg-desktop-portal` runs, but no backend that implements FileChooser (such as `xdg-desktop-portal-gnome` or `-gtk`) is installed, and zenity is absent. "Choose folder" does nothing and shows no error, so the user cannot set a Library Root from the picker.
- **Fix:** Check for the `org.freedesktop.portal.FileChooser` interface. Treat a portal `None` that did not come from the user as a failure and continue to zenity, kdialog, then manual entry.
- **Confidence:** Verified for the shell check. The niri outcome is suspected.

#### H-21. Companion cookie files are left on disk with default permissions

- **Location:** `packages/companion-protocol/src/cookies.rs:64-69` (`File::create` with the default umask, one new file per hand-off), `packages/companion-protocol/src/ipc.rs:148`.
- **Problem:** The full cookie jar for the page, including HttpOnly and Secure session cookies, is written in plain text, usually mode 0644. Nothing ever deletes these files.
- **Failure:** After 100 "send with cookies" actions, 100 login sessions sit under `~/.local/share/phlegethon/companion-cookies/`.
- **Fix:** Create with mode 0600, reuse or delete the file when the job ends or the channel changes, and delete it on Discard.
- **Confidence:** Verified. Exposure to other users depends on the home directory's mode.

#### H-22. Extractor Toolchain checksum verification fails open

- **Location:** `packages/download-core/src/github_release_source.rs:195-200` (a failed `SHA2-256SUMS` fetch becomes `None` through `.ok()`), `packages/download-core/src/github_release_source.rs:236-245` (an existing download is reused with no check when `sha256` is `None`), `packages/download-core/src/github_release_source.rs:256` (verification only `if let Some(expected)`).
- **Problem:** The module documentation says the download is verified against SUMS before install. A transient SUMS error, or a SUMS file without the selected asset, installs an unverified binary that then runs on every probe. No signature is checked anywhere.
- **Fix:** Make a missing or unparseable SUMS entry a hard error for toolchain installs. Remove the unverified reuse path. Verify a detached signature if upstream publishes one.
- **Confidence:** Verified.

#### H-23. Sidecar installs are not atomic, and the "no swap mid-transfer" guard ignores probes

- **Location:** `packages/download-core/src/lib.rs:1779-1781` (`transfers_active` counts only body-writing jobs), `packages/download-core/src/sidecar_updates.rs:429-440` (`fs::copy` over the live binary), `packages/download-core/src/plugin_sidecar.rs:128-145` (`remove_dir_all` of the plugin package, then copy, then manifest write).
- **Problem:** yt-dlp probes and quality lookups run outside the Core lock and are invisible to the guard. Both installers replace live files in place.
- **Failure:** An install during a probe truncates the yt-dlp file (an executing ELF returns ETXTBSY; a zipimport script is not locked and can be read half-written). A probe that starts between the plugin delete and copy sees no LuxureTV or XxxbpTV extractor and fails as generic. A crash between the two steps leaves no plugin package until the next launch.
- **Fix:** Write to a temporary path in the same directory and `rename`. Count in-flight probes in `JobSignals`, or take a read lock in probes and a write lock in installs.
- **Confidence:** Verified for the code. The ETXTBSY and zipimport effects are suspected.

#### H-24. yt-dlp capability caches are never refreshed, and a failed check is cached

- **Location:** `packages/download-core/src/ytdlp_extractor.rs:258-276` (a failed or timed-out `--help` caches an empty flag set), `packages/download-core/src/ytdlp_extractor.rs:310` (impersonation `false` cached the same way), `packages/download-core/src/ytdlp_extractor.rs:440-455` (`bind_sidecar_root` clears neither cache), `packages/download-core/src/ytdlp_extractor.rs:321` (JS runtime taken from the startup config, while `status()` re-detects at `packages/download-core/src/ytdlp_extractor.rs:411`).
- **Failure:** A cold PyInstaller binary misses the 15 s `--help` deadline on the first probe. For the rest of the session `--impersonate` (needed by both plugins for Cloudflare) and `--js-runtimes` are never passed. Installing yt-dlp or deno from the panel has no effect until restart.
- **Fix:** Do not cache failures. Key the caches on binary path and mtime, clear them on rebind, and use the same JS runtime detection as `status()`. Run `--help` outside the cache mutex, which today blocks concurrent probes for up to 15 s.
- **Confidence:** Verified for the caching. The timeout trigger is a plausible scenario, not measured.

#### H-25. Every 8 KB of progress becomes an IPC event and a full React render

- **Location:** `packages/download-core/src/bandwidth.rs:260` (`emit_progress` per write), `packages/download-core/src/http.rs:206` (8 KB buffer), `packages/download-core/src/lib.rs:888` (unbounded channel), `packages/tauri-shell/src-tauri/src/lib.rs:205-219` (forwards every event, and also copies it into an unused buffer), `packages/tauri-shell/frontend/src/state/useJobs.ts:73-93` (`setLastEvent` and `setJobs` per event).
- **Failure:** One job at 10 MB/s sends about 1,200 events per second. Each one re-renders the shell, re-sorts the queue view and rebuilds every row. On a tiling compositor this competes with resize layout, so resizing or switching columns during downloads feels slow (suspected). If the webview stalls, the unbounded channel grows without limit.
- **Fix:** Coalesce progress per job to about 4 to 10 Hz in Rust and in the frontend, keep progress out of `lastEvent`, bound the channel, and memoize table rows.
- **Confidence:** Confirmed for the emission rate. The jank is suspected.

### 4.3 Medium

#### Download Core: queue, storage and archive

| ID | Problem | Location | Fix | Confidence |
| --- | --- | --- | --- | --- |
| M-1 | Discard removes the row and signals before deleting files. A failed `remove_dir_all` returns early without `JobDiscarded` or a persist, so the job vanishes from memory and comes back from `jobs.json` on restart. | `packages/download-core/src/lib.rs:1392-1398` | Persist the removal first, or delete first and remove the row on success. | Confirmed |
| M-2 | A Discard request is lost when the transfer finishes first. The completion paths never check `is_discard_requested`, so the row stays as Completed after `discard_job` returned Ok. | `packages/download-core/src/lib.rs:2801`, `packages/download-core/src/lib.rs:2867`, `packages/download-core/src/lib.rs:1420` | After any finish, honour a pending discard with Completed-job semantics. | Confirmed |
| M-3 | Transfer planning ignores pause and discard flags and the Active Job Limit. The limit is a snapshot in `dispatch_pending`, and overlapping rounds can exceed it. | `packages/download-core/src/lib.rs:2563-2578`, `packages/download-core/src/lib.rs:2472-2474` | Reserve slots under the lock, and check flags and the limit in `plan_unlocked_transfer`. | Confirmed; overshoot suspected |
| M-4 | `set_library_root` swaps jobs and settings before loading the archive. A corrupt or newer-version `archive.json` leaves the new jobs in memory with the old root, and later writes go to the wrong `jobs.json`. | `packages/download-core/src/lib.rs:1008-1028`, `packages/download-core/src/download_archive.rs:63-72` | Load into locals and commit at the end. Treat a bad archive as a warning and move it aside. | Confirmed |
| M-5 | Terminal events are emitted before the state is persisted, and errors in the dispatch path are dropped. A failed archive save after `JobCompleted` leaves `jobs.json` saying Downloading, and reconcile then marks the job Failed with no message. | `packages/download-core/src/lib.rs:3055-3068`, `packages/download-core/src/lib.rs:2502-2526` | Persist before emitting. Make the archive save best-effort with a logged error. | Confirmed |
| M-6 | Confirm and enqueue push jobs before every item is validated. An invalid second item leaves the first job in memory, and confirming again duplicates it. | `packages/download-core/src/lib.rs:2366-2430`, `packages/download-core/src/lib.rs:1230-1231` | Validate the whole selection first. Roll back on a persist failure. | Confirmed |
| M-7 | Reconcile demotes a user-Paused job to Failed, with no `last_error`, when its `.part` is missing. `CONTEXT.md` says Paused jobs stay Paused. | `packages/download-core/src/storage.rs:419-421` | Demote only Downloading jobs. For Paused jobs, reset bytes and set an explanation. | Verified |
| M-8 | Restore trusts persisted values: `active_job_limit` 0 stops all dispatch, a relative `destination` makes Discard delete a path relative to the working directory, and a `filename` with separators writes outside the Job Destination. Needs a hand-edited or legacy file. | `packages/download-core/src/storage.rs:215`, `packages/download-core/src/job.rs:216-232`, `packages/download-core/src/storage.rs:114` | Validate on load: limit at least 1, absolute job-owned destination, single-component filename. | Confirmed |
| M-9 | No `fsync` anywhere. `jobs.json`, `archive.json` and completed files are renamed without syncing data or the directory, so power loss can leave empty or truncated files that then trigger H-8. | `packages/download-core/src/storage.rs:357-361`, `packages/download-core/src/download_archive.rs:86-94`, `packages/download-core/src/transfer.rs:79`, `packages/download-core/src/lib.rs:3035` | `sync_all` before rename, then sync the parent directory. | Confirmed by absence |
| M-10 | A zero Bandwidth Cap is accepted. The bucket never refills, so with unknown size a zero-byte file is marked Completed (H-2) and with known size the job re-plans forever. The setters also swallow persist errors. | `packages/download-core/src/lib.rs:1482-1490`, `packages/download-core/src/lib.rs:1501-1510`, `packages/download-core/src/bandwidth.rs:97-103` | Reject 0 or treat it as uncapped. Return `Result` from the setters. | Confirmed |
| M-11 | The raw Filename Template is passed to yt-dlp `-o`, joined onto the Job Destination. An absolute template replaces the destination and `..` escapes it, so files land outside the job folder, Discard cannot remove them, and History points at a missing file. | `packages/download-core/src/filename_template.rs:150-151`, `packages/download-core/src/ytdlp_extractor.rs:1012`, `packages/download-core/src/lib.rs:1878-1885` | Reject absolute paths, `..` and separators in `set_filename_template`. Pass the sanitized relative spec. | Confirmed |
| M-12 | A failed retry overwrites the Completed Download Archive entry for the same media id. The probe then stops marking the item as downloaded and History loses the Completed row. | `packages/download-core/src/download_archive.rs:115-117`, `packages/download-core/src/lib.rs:1147`, `packages/download-core/src/lib.rs:3067` | Keep a Completed entry when a Failed record arrives, or key entries by media id and job id. | Confirmed |

#### Transfers and network

| ID | Problem | Location | Fix | Confidence |
| --- | --- | --- | --- | --- |
| M-13 | Segmented Transfer is chosen when HEAD has no usable size, and then fails with "no Content-Length", although a single GET would work. | `packages/download-core/src/lib.rs:773`, `packages/download-core/src/lib.rs:2653` | Fall back to a single fetch when the size is unknown. | Confirmed |
| M-14 | A crash between the last byte and the rename leaves a complete `.part`. On relaunch the plan requests `bytes=total-`, gets 416, and fails the job. Retry starts from zero. | `packages/download-core/src/lib.rs:2730-2734` | When `bytes_received >= total`, complete the file without a request. | Confirmed |
| M-15 | Pausing a Segmented Transfer deletes the `.part` and resets progress, by design (`// Resume restarts the transfer from zero`). This contradicts Resumable Transfer. | `packages/download-core/src/lib.rs:2829-2836` | Persist per-segment progress so finished segments survive a pause. | Verified |
| M-16 | The Mirror Set parser does not decode XML entities (`&amp;` stays literal), accepts `ftp` and `bittorrent` mirrors as HTTP, and mis-reads self-closing `<url/>` tags. | `packages/download-core/src/mirror_set.rs:51-60` | Use an XML reader, decode entities, filter to http and https. | Confirmed |
| M-17 | A checksum sidecar uses its first hash line regardless of the filename column, so a multi-file `SHA256SUMS` checks the job against another file's digest. | `packages/download-core/src/integrity.rs:53`, `packages/download-core/src/lib.rs:2036` | Match the filename column and error when no entry matches. | Confirmed |
| M-18 | Filenames are not capped at 255 bytes. A long title or Content-Disposition name fails with "File name too long" after the probe. | `packages/download-core/src/filename_template.rs:51-70`, `packages/download-core/src/lib.rs:2413` | Cap the stem on a char boundary, leaving room for `.part` and the extension. | Confirmed |
| M-19 | The URL splitter ends a URL at `,` or `;`. `https://example.com/dl?ids=1,2,3` becomes `…ids=1` and the rest is dropped without notice. | `packages/download-core/src/url_input.rs:35` | Split on whitespace only and report rejected tokens. | Confirmed |
| M-20 | Audio-only mode returns the first quality when no entry is tagged as audio, which can be a 2160p video stream. | `packages/download-core/src/quality_policy.rs:66-70` | Return `None` and tell the user no audio option exists. | Confirmed |
| M-21 | The DoH proxy listens on loopback with no authentication and no target allowlist, so any local process can CONNECT to any host and port, including loopback-only services. Idle clients hold a thread and file descriptor forever. | `packages/download-core/src/doh_proxy.rs:42`, `packages/download-core/src/doh_proxy.rs:64-66`, `packages/download-core/src/doh_proxy.rs:164-199` | Require a per-run token, restrict targets, set read timeouts and cap connections. | Confirmed |
| M-22 | The DoH override map covers only the page host and its `www.` twin, so sinkholed CDN hosts still fail. Any DoH error silently falls back to the plain command, and the DoH request ignores the configured proxy. | `packages/download-core/src/doh_proxy.rs:226-232`, `packages/download-core/src/doh_ytdlp.rs:47-49`, `packages/download-core/src/doh_dns.rs:15` | Route every CONNECT through DoH, emit a diagnostic event, and build the DoH agent from the proxy settings. | Confirmed; sinkhole effect suspected |
| M-23 | The DoH fallback script is `/tmp/phlegethon-doh-ytdlp-<pid>.py`, shared by concurrent jobs. `python <script>` puts `/tmp` first on `sys.path`, so a planted `/tmp/socket.py` is imported. Rust `{:?}` escapes are not valid Python literals. | `packages/download-core/src/doh_ytdlp.rs:80-86`, `packages/download-core/src/doh_ytdlp.rs:134-148` | Use a private per-run directory, run Python with `-I`, and pass data as JSON. | Confirmed |
| M-24 | Proxy credentials are stored in plain text in settings and passed on the yt-dlp command line, readable from `/proc/<pid>/cmdline`. | `packages/download-core/src/proxy_settings.rs:6-10`, `packages/download-core/src/proxy_settings.rs:48-52` | Pass credentials through the environment and store the password separately. | Confirmed |

#### Extractor Toolchain and probing

| ID | Problem | Location | Fix | Confidence |
| --- | --- | --- | --- | --- |
| M-25 | The tier-3 error wrapper puts the tagged yt-dlp error mid-string, so the `[code]` prefix is lost. For non-Launch-Site pages the UI never shows the cookie, timeout, JS-runtime or geo hint. | `packages/download-core/src/lib.rs:390-393`, `packages/download-core/src/toolchain_discovery.rs:685-687`, `packages/tauri-shell/frontend/src/lib/errors.ts:20` | Re-tag the wrapper with the inner code, or return a structured error. | Confirmed |
| M-26 | A missing yt-dlp gives "failed to execute yt-dlp: failed to spawn yt-dlp: No such file or directory" with no error code and no pointer to Toolchain › Install. | `packages/download-core/src/ytdlp_extractor.rs:594`, `packages/download-core/src/toolchain_discovery.rs:557-558` | Map a spawn ENOENT to a `toolchain_missing` code with an install hint. | Confirmed |
| M-27 | Percent-decoding slices a `str` by byte index and panics inside a multi-byte character, for example `filename="50%€.zip"`. The probe returns "probe task failed", no `ProbeFailed` event fires, and a stale cancel entry stays. | `packages/download-core/src/page_sniffer.rs:611-613` | Decode from bytes or use `str::get`. | Confirmed by running a copy of the function |
| M-28 | `tbr` is divided by 1000, but yt-dlp reports it in kbit/s. Typical audio formats all become `1k-audio`, share one id, and the user cannot pick the better track. The test fixtures use bit/s and hide this. | `packages/download-core/src/ytdlp_extractor.rs:849-851`, `packages/download-core/src/ytdlp_extractor.rs:890-892` | Use `tbr` as kbit/s and fix the fixtures. | Verified; unit from yt-dlp documentation |
| M-29 | Extractor progress is a recursive size walk of the Job Destination every 50 ms, and the total is one format's size. With video plus audio, progress passes 100 % during the merge and can go backwards. | `packages/download-core/src/lib.rs:742`, `packages/download-core/src/lib.rs:2420-2421` | Parse `--progress-template` output, or sum the selected formats. Show post-processing as its own phase. | Confirmed |
| M-30 | Page sniffing ignores `<base href>`, so relative media URLs resolve against the document and 404. | `packages/download-core/src/page_sniffer.rs:179-198` | Read the first `<base href>` and use it as the base. | Confirmed by absence |

#### Extractor sidecar and Extractor Plugins

| ID | Problem | Location | Fix | Confidence |
| --- | --- | --- | --- | --- |
| M-31 | A toolchain pin to any version other than the latest can never be applied. The GitHub source puts only the latest tag in the catalog, so Install fails with "catalog missing toolchain version" and the panel always says an update is available. | `packages/download-core/src/github_release_source.rs:121-122`, `packages/download-core/src/sidecar_updates.rs:370`, `packages/download-core/src/sidecar_updates.rs:584-585` | Fetch the pinned tag's release, or reject pins not in the catalog. | Confirmed |
| M-32 | Rollback does not pin the restored version, so with `check_on_launch` on, the next launch re-applies the release the user rolled back from. | `packages/download-core/src/sidecar_updates.rs:771-792`, `packages/download-core/src/sidecar_updates.rs:717-724` | Set a pin or a skip marker on rollback. | Confirmed |
| M-33 | A plugin pin or rollback to a version older than the bundled plugin is overwritten by `ensure_bundled_plugins` on every launch. | `packages/download-core/src/plugin_sidecar.rs:254-259`, `packages/tauri-shell/src-tauri/src/lib.rs:48` | Respect `plugin_pin` in `ensure_bundled_plugins`. | Confirmed |
| M-34 | Two app instances remove and rewrite the same plugin package and manifest at startup with no lock, so either can read a partial package or a half-written manifest. | `packages/download-core/src/plugin_sidecar.rs:134-142`, `packages/download-core/src/plugin_sidecar.rs:269` | Single-instance guard (H-19), or `flock` on the sidecar root, plus temp-and-rename. | Confirmed |
| M-35 | A partial apply (toolchain succeeds, plugin fails) returns early before saving runtime state. The new toolchain stays installed, its snapshot lives only in memory, and the old backup was already deleted. | `packages/download-core/src/sidecar_updates.rs:457-458`, `packages/download-core/src/sidecar_updates.rs:660-682` | Persist after each component and restore the toolchain on a later failure. | Confirmed |
| M-36 | An interrupted catalog plugin install leaves the old manifest beside a partial package, and the next launch reports it UpToDate. Needs a catalog plugin update, which has no producer today. | `packages/download-core/src/plugin_sidecar.rs:134-142`, `packages/download-core/src/plugin_sidecar.rs:239-263` | Stage beside the target, rename into place, then write the manifest. | Confirmed |
| M-37 | Manifest strings are joined into paths without validation, and `remove_dir_all` runs on the result. A tampered manifest with `"package": ".."` deletes the sidecar root, and an absolute path deletes that directory. Needs a modified local file. | `packages/download-core/src/plugin_sidecar.rs:131-136`, `packages/download-core/src/sidecar_updates.rs:251-253`, `packages/download-core/src/sidecar_updates.rs:527-529`, `packages/download-core/src/sidecar_updates.rs:753-766` | Accept only single normal path components for `package` and `binary_name`. | Confirmed |
| M-38 | A corrupt `sidecar-state.json` fails `set_sidecar_root` before the root is set, so every update, pin and rollback command says "sidecar root not configured" for the session. The file is written without temp-and-rename. | `packages/download-core/src/lib.rs:1709-1712`, `packages/tauri-shell/src-tauri/src/lib.rs:1139-1141`, `packages/download-core/src/sidecar_updates.rs:291`, `packages/download-core/src/sidecar_updates.rs:303` | Reset to defaults on parse failure, keep the root, write atomically. | Verified |
| M-39 | On musl, the zipimport `yt-dlp` asset is installed without checking for `python3`, and discovery prefers the sidecar binary, so a working `yt-dlp` on PATH is shadowed by one that cannot start. | `packages/download-core/src/github_release_source.rs:56-67`, `packages/download-core/src/toolchain_discovery.rs:98-101` | Run `--version` with a deadline before accepting a sidecar binary, and check for `python3` before choosing that asset. | Confirmed |
| M-40 | Binding the sidecar binary checks `is_file()` but not the execute bit that discovery requires, and a binary deleted while the app runs stays bound. Probes fail with EACCES or "failed to spawn" even when PATH has a working yt-dlp. | `packages/download-core/src/toolchain_discovery.rs:100`, `packages/download-core/src/ytdlp_extractor.rs:440-455` | Share one health check between discovery and bind, and clear the binding when the file is gone. | Confirmed |
| M-41 | "Installed" is read from the manifest only. Deleted or damaged sidecar files are reported up to date and never repaired, although the doc comment says a damaged install is refreshed. | `packages/download-core/src/sidecar_updates.rs:353-358`, `packages/download-core/src/sidecar_updates.rs:369`, `packages/download-core/src/plugin_sidecar.rs:40-42`, `packages/download-core/src/plugin_sidecar.rs:259-261` | Store per-file SHA-256 values and verify them before reporting up to date. | Confirmed |
| M-42 | yt-dlp is never updated by default (`check_on_launch: false`), and the nightly channel that `docs/research/extractor-toolchain.md:94` recommends is defined but never used. Launch Site extractors go stale without user action. | `packages/download-core/src/sidecar_updates.rs:33`, `packages/download-core/src/sidecar_updates.rs:45`, `packages/download-core/src/github_release_source.rs:39`, `packages/download-core/src/github_release_source.rs:151` | Check on launch by default (apply only when idle) and add a channel setting. | Confirmed |
| M-43 | The plugin catalog path installs Python code from `update-catalog.json` with no hash, signature or provenance check, and every yt-dlp process loads it. `CONTEXT.md` lists "user-installed untrusted plugins" under _Avoid_. No catalog file ships today. | `packages/download-core/src/sidecar_updates.rs:192-212`, `packages/download-core/src/sidecar_updates.rs:556-567` | Verify `sha256` or a signature for plugin entries, or remove the catalog path for plugins. | Confirmed |
| M-44 | The xxxbp listing uses `fatal=page == 1`, so any error on pages 2 to 5 returns an empty page and silently ends the listing. A 429 on page 3 yields a "complete" two-page channel. | `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/xxxbp.py:139-155` | Keep pages fatal and handle 404 explicitly. | Confirmed |
| M-45 | When the luxuretv player markup and JSON-LD are both missing, the fallback scrapes every media URL on the page, so a layout change can offer a related clip as the video. | `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/_parse.py:436-437`, `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/_parse.py:473-475` | Accept only sources that contain the numeric video id, or fail. | Confirmed; live markup suspected |
| M-46 | The plugins depend on yt-dlp APIs that were not checked (`impersonate=True` as a request option, the `'impersonate'` format key, `yt_dlp.networking.exceptions`) and declare no minimum yt-dlp version. The offline tests mock exactly these calls, and they are skipped anyway. | `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/luxuretv.py:42-43`, `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/_parse.py:489`, `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/xxxbp.py:5` | Declare a minimum version and run the offline tests against a real yt-dlp in CI. | Suspected |

#### Platform Shell and Chrome Companion

| ID | Problem | Location | Fix | Confidence |
| --- | --- | --- | --- | --- |
| M-47 | Release builds install no logger, so every `log::warn!` and `log::error!` is dropped, including companion receiver death, plugin install failure and the H-8 root error. | `packages/tauri-shell/src-tauri/src/lib.rs:1126-1132` | Always log to a file under the data directory. | Verified |
| M-48 | The companion receiver handles one connection at a time with no read timeout and stops for good on the first accept error. One idle client blocks every hand-off, and Chrome's `sendNativeMessage` never settles. | `packages/companion-protocol/src/ipc.rs:37`, `packages/companion-protocol/src/ipc.rs:52-58` | Per-connection timeouts and threads; log and continue on accept errors. | Confirmed |
| M-49 | The native-messaging host does not authenticate the server. While Phlegethon is closed, any process that binds the abstract name receives the frame, including cookies. | `packages/companion-protocol/src/ipc.rs:31-38`, `packages/native-messaging-host/src/main.rs:24` | Filesystem socket with an owner check, or a token challenge. | Confirmed |
| M-50 | The native-messaging host ignores the browser origin in `argv[1]`, so `allowed_origins` is enforced only by the browser. | `packages/native-messaging-host/src/main.rs:16-30` | Reject unknown `chrome-extension://` origins. | Confirmed |
| M-51 | A hand-off with cookies replaces the user's persisted cookie setting (for example `cookies_from_browser = firefox`), with no notice. The Settings page keeps showing the old value, and Apply in that section writes the stale value back. | `packages/tauri-shell/src-tauri/src/companion.rs:38-41`, `packages/download-core/src/lib.rs:1665-1670`, `packages/tauri-shell/frontend/src/components/SettingsPage.tsx:82-117` | Use the session for that job only, emit a change event, and show the active source. | Confirmed |
| M-52 | The `process_pending` busy flag is not cleared when dispatch panics, so no job ever starts again until restart. | `packages/tauri-shell/src-tauri/src/lib.rs:669-692` | Clear the flag from a drop guard. | Confirmed |
| M-53 | Discard runs `remove_dir_all` synchronously on the main thread under the Core lock. A failed HLS job with thousands of fragments freezes the window during the delete. | `packages/tauri-shell/src-tauri/src/lib.rs:755-763`, `packages/download-core/src/lib.rs:1380-1400` | Remove the row, then delete on a worker. | Confirmed |
| M-54 | Reveal and Open are synchronous. Reveal blocks on D-Bus on the main thread (and stalls when no FileManager1 service exists, which is common on niri). Open accepts any regular file path. | `packages/tauri-shell/src-tauri/src/lib.rs:956-975` | Make both async and limit paths to Job Destinations. Fall back to `xdg-open` on the folder. | Confirmed; timeout suspected |
| M-55 | Startup runs `python3 -c "import yt_dlp"`, JS-runtime `--version` and `ldd --version` on the main thread before the window exists, with deadlines of 5 to 10 s each. | `packages/tauri-shell/src-tauri/src/lib.rs:43-56`, `packages/download-core/src/toolchain_discovery.rs:164-170` | Discover on a background thread and publish the result. | Confirmed; delay suspected |
| M-56 | The Chrome extension reports every failure, including "desktop app is not running", only to the service-worker console. | `packages/chrome-companion/background.js:76-84`, `packages/chrome-companion/background.js:107-110` | Set a badge or show a notification. | Confirmed |
| M-57 | The extension requests `cookies` with `<all_urls>` for a feature used on explicit clicks only. | `packages/chrome-companion/manifest.json:6-14` | Use `optional_host_permissions` and request on demand. | Confirmed |
| M-58 | A release build without the `custom-protocol` feature loads `http://localhost:1420` into a webview with full IPC, so anything listening on that port controls the app. Nothing guards against this build mistake. | `packages/tauri-shell/src-tauri/Cargo.toml:19-23`, `packages/tauri-shell/src-tauri/tauri.conf.json:9`, `packaging/void/build-in-container.sh:107-115` | `compile_error!` in release builds without the feature, and check the packaged binary for embedded assets. | Confirmed; the shipped 0.1.0 binary is correct |

#### Frontend logic

| ID | Problem | Location | Fix | Confidence |
| --- | --- | --- | --- | --- |
| M-59 | A failed load in the Toolchain panel leaves default settings in state, and the next toggle writes them back, which clears the pins. | `packages/tauri-shell/frontend/src/components/ToolchainPanel.tsx:86-101`, `packages/tauri-shell/frontend/src/components/ToolchainPanel.tsx:285` | Track a loaded flag and disable saves until load succeeds. | Confirmed |
| M-60 | `SettingsPage` uses `Promise.allSettled` and substitutes defaults on rejection. Apply then overwrites configured values, for example the cookie channel, with defaults. | `packages/tauri-shell/frontend/src/components/SettingsPage.tsx:85-108` | Keep per-section error state and disable Apply for sections that failed to load. | Confirmed |
| M-61 | The clipboard watcher reads the whole clipboard every 1.5 s while enabled, sends it over IPC without a prefilter, ignores focus and visibility, and re-offers URLs the app itself copied. | `packages/tauri-shell/frontend/src/lib/clipboardWatcher.ts:33-57`, `packages/tauri-shell/frontend/src/components/UrlBar.tsx:75-87`, `packages/tauri-shell/frontend/src/App.tsx:260-264` | Prefilter for `://`, pause while hidden or unfocused, and ignore the app's own writes. | Confirmed |
| M-62 | Closing the probe sheet clears the pending probe without waiting for the probe to finish. A late result is stored anyway, and the URL is reported as "probing" with an empty job id until restart. | `packages/tauri-shell/frontend/src/components/ProbeModal.tsx:192-196`, `packages/download-core/src/lib.rs:2110`, `packages/download-core/src/lib.rs:1273-1280` | Clear after the probe settles, or drop results whose cancel flag is set. | Confirmed; timing suspected |
| M-63 | Cancel stays enabled during Confirm, and both close and confirm advance the probe queue by dropping its head. Cancelling mid-confirm silently skips the next queued URL. | `packages/tauri-shell/frontend/src/components/ProbeModal.tsx:482`, `packages/tauri-shell/frontend/src/App.tsx:177`, `packages/tauri-shell/frontend/src/App.tsx:389` | Remove by URL and disable Cancel while confirming. | Confirmed |
| M-64 | A second URL submission that hits a duplicate replaces the single duplicate resolver. The first batch then waits forever and its remaining URLs are never handled. Paste, drop and the clipboard action have no busy check. | `packages/tauri-shell/frontend/src/App.tsx:109-115`, `packages/tauri-shell/frontend/src/components/UrlBar.tsx:49-72` | Serialize `addUrls` and gate input while a modal is open. | Confirmed |
| M-65 | `lastEvent` is a single slot. Two completions in one render produce one notification and one History reload, and toggling notifications re-fires the last one. | `packages/tauri-shell/frontend/src/state/useJobs.ts:24`, `packages/tauri-shell/frontend/src/state/useJobs.ts:73`, `packages/tauri-shell/frontend/src/App.tsx:77-94` | Deliver terminal events through a queue and key effects on an event id. | Confirmed; loss suspected |
| M-66 | User-facing strings bypass `packages/shell-strings/en.json`: error hints, relative times, raw match status (`probing`, `downloading`), and Core English such as the ffmpeg notice that overrides `probe.media.noFfmpeg`. The i18n test does not catch literals. | `packages/tauri-shell/frontend/src/lib/errors.ts:43-53`, `packages/tauri-shell/frontend/src/lib/history.ts:16-27`, `packages/tauri-shell/frontend/src/components/DuplicateUrlModal.tsx:19`, `packages/download-core/src/media_extras.rs:74-75` | Route everything through `t()` and map Core codes to keys. | Confirmed |
| M-67 | One failing URL rejects the whole paste batch. Later URLs are never enqueued, and the toast does not say which failed. | `packages/tauri-shell/frontend/src/lib/urlInput.ts:51-59`, `packages/tauri-shell/frontend/src/App.tsx:142-143` | Catch per URL and report counts. | Confirmed |
| M-68 | Batch Discard uses every selected id, while Pause, Resume and Retry use only visible rows. A job hidden by a filter can be discarded and its files deleted. | `packages/tauri-shell/frontend/src/components/QueueTable.tsx:197`, `packages/tauri-shell/frontend/src/components/QueueTable.tsx:230` | Use the visible selection for every batch action, or clear selection on filter change. | Confirmed; Carbon selection retention suspected |
| M-69 | A companion intent sent while the webview is still loading is dropped, although the extension was told it succeeded. | `packages/tauri-shell/src-tauri/src/lib.rs:1169`, `packages/tauri-shell/src-tauri/src/companion.rs:73`, `packages/tauri-shell/frontend/src/App.tsx:152-167` | Buffer intents in Rust until the frontend pulls them on mount. | Suspected |
| M-70 | `useJobs` tracks `error` and `loading`, but the app never renders them. A failed `list_jobs` shows the empty-queue tile, and the empty hint flashes on startup. | `packages/tauri-shell/frontend/src/state/useJobs.ts:31`, `packages/tauri-shell/frontend/src/state/useJobs.ts:53`, `packages/tauri-shell/frontend/src/App.tsx:54` | Render an error banner and a loading state. | Confirmed |
| M-71 | IPC contract drift. The TypeScript `CompanionIntent` is `{url, source?}` while Rust sends `{url, result}`. `CompanionResult` tags are snake_case but `enqueue_url` results are PascalCase. `ActiveJobMatch.status` can be `"probing"`, which is not a job status. Error hint text exists in both Rust and TypeScript with different wording. | `packages/tauri-shell/frontend/src/tauri/types.ts:27-32`, `packages/tauri-shell/frontend/src/tauri/types.ts:233-236`, `packages/tauri-shell/src-tauri/src/companion.rs:14-17`, `packages/companion-protocol/src/envelope.rs:113-114`, `packages/tauri-shell/src-tauri/src/lib.rs:88`, `packages/download-core/src/lib.rs:1277`, `packages/download-core/src/toolchain_discovery.rs:696-713` | Generate the TypeScript types from Rust (for example with `ts-rs` or `specta`) and add a contract test. | Confirmed |

### 4.4 Low

| ID | Area | Problem | Location |
| --- | --- | --- | --- |
| L-1 | Core | Probe cancel flags are keyed by URL, so concurrent probes of one URL share a flag. A failed re-probe leaves the earlier result confirmable. | `packages/download-core/src/lib.rs:2095-2111`, `packages/download-core/src/lib.rs:2121-2129` |
| L-2 | Core | Template literal bytes are pushed as Latin-1 characters, so `é` becomes `Ã©` and `job.filename` differs from the real file. | `packages/download-core/src/filename_template.rs:44` |
| L-3 | Core | Names `.` and `..` pass `sanitize_filename`, and the final rename fails after the whole download. | `packages/download-core/src/lib.rs:2413-2416` |
| L-4 | Core | Probe and pending-probe maps use the URL as key, and `reorder_job` emits no event, so shells must poll. | `packages/download-core/src/lib.rs:1560-1576` |
| L-5 | Core | Persisted progress is written only at transfer end, so `jobs.json` byte counts are stale until reconcile. | `packages/download-core/src/lib.rs:1074-1086` |
| L-6 | Core | `filename_from_url` keeps percent-encoding, so `My%20File.zip` is written verbatim. | `packages/download-core/src/job.rs:32-35` |
| L-7 | Core | `mark_completed` and `mark_failed` have no state-transition guards. | `packages/download-core/src/job.rs:523-525`, `packages/download-core/src/job.rs:559-561` |
| L-8 | Core | `set_library_root` re-parses `jobs.json` about ten times, once per `load_*` helper. | `packages/download-core/src/storage.rs:193-304` |
| L-9 | Core | Old job signal ids are never unregistered on a root change and stay in the active count. | `packages/download-core/src/lib.rs:1009-1011` |
| L-10 | Core | Duplicate job ids in `jobs.json` are not rejected. | `packages/download-core/src/job.rs:216-248` |
| L-11 | Core | A relative `cookie_file` in a loaded queue file is passed to yt-dlp, which resolves it against its own working directory. `validate()` runs only before saving. | `packages/download-core/src/storage.rs:264`, `packages/download-core/src/cookie_channel.rs:35-41` |
| L-12 | Core | The archive uses one fixed temp name and no lock, so two writers lose updates. | `packages/download-core/src/download_archive.rs:86` |
| L-13 | Transfer | Open-ended ranges are sent as `bytes=N-18446744073709551614` instead of `bytes=N-`. | `packages/download-core/src/http.rs:192` |
| L-14 | Transfer | The disk-space check is skipped when the size is unknown, and nothing preallocates, so ENOSPC arrives mid-transfer. | `packages/download-core/src/lib.rs:1159-1161` |
| L-15 | Transfer | The DoH header reader rescans the whole buffer after every byte. | `packages/download-core/src/doh_proxy.rs:188-196` |
| L-16 | Probe | Tier-3 HEAD and GET have no overall deadline and ignore the cancel flag, so a slow-drip server keeps a probe running for minutes. | `packages/download-core/src/page_sniffer.rs:663-691`, `packages/download-core/src/http.rs:131-132` |
| L-17 | Probe | `html_unescape` decodes `&amp;` first (so `&amp;lt;` becomes `<`) and skips numeric entities. | `packages/download-core/src/page_sniffer.rs:147-154` |
| L-18 | Probe | `.ts` counts as media for any link (so `app.ts` is offered), ads are not filtered, and variants group by file stem across hosts. | `packages/download-core/src/page_sniffer.rs:99-102`, `packages/download-core/src/page_sniffer.rs:448-507` |
| L-19 | Probe | Failure classification is substring-based: any 403 becomes "needs cookies", any mention of "deno" becomes "needs JS runtime", and common geo wording is missed. | `packages/download-core/src/toolchain_discovery.rs:642-659` |
| L-20 | Probe | Strict `u64` fields and a non-null `title` make one float or null fail the whole probe JSON. | `packages/download-core/src/ytdlp_extractor.rs:1183`, `packages/download-core/src/ytdlp_extractor.rs:1359-1392` (suspected) |
| L-21 | Probe | A resolution fallback turns `1920x1080` into the selector `1920p1080`, which yt-dlp rejects. | `packages/download-core/src/ytdlp_extractor.rs:837-838` |
| L-22 | Probe | Downloads have no stall deadline, and passing the user's own cookie file to `--cookies` lets yt-dlp rewrite it. | `packages/download-core/src/ytdlp_extractor.rs:1121`, `packages/download-core/src/cookie_channel.rs:74-77` |
| L-23 | Probe | Non-UTF-8 yt-dlp output is not decoded lossily, and `PYTHONUTF8` is not set for the child. | `packages/download-core/src/ytdlp_extractor.rs:625-626` |
| L-24 | Sidecar | A failed toolchain download leaves `name.part`, and `downloads/<version>/` is never pruned. | `packages/download-core/src/github_release_source.rs:232`, `packages/download-core/src/github_release_source.rs:247-255` |
| L-25 | Sidecar | `invalidate()` has no caller, so the panel's Check button can return a catalog up to an hour old. | `packages/download-core/src/github_release_source.rs:174` |
| L-26 | Sidecar | "Apply when idle" does not exist. `DeferredUntilIdle` is never constructed, and a blocked install does not set the deferred flag. | `packages/download-core/src/sidecar_updates.rs:3-5`, `packages/download-core/src/sidecar_updates.rs:82`, `packages/download-core/src/lib.rs:1755-1760` |
| L-27 | Sidecar | Sidecar update settings are stored per Library Root although the sidecar is app-wide, so pins change when the root changes. | `packages/download-core/src/lib.rs:1024`, `packages/download-core/src/lib.rs:1051-1063` |
| L-28 | Sidecar | Relative PATH entries (`.` or empty) are searched for yt-dlp, zenity, kdialog, dbus-send and notify-send. | `packages/download-core/src/toolchain_discovery.rs:141-143`, `packages/shell-support/src/lib.rs:17-24` |
| L-29 | Sidecar | Version strings fall back to the first stdout line, so warnings can show as the version. | `packages/download-core/src/toolchain_discovery.rs:315-319`, `packages/download-core/src/ytdlp_extractor.rs:341-349` |
| L-30 | Sidecar | Toolchain versions compare as strings, so an older catalog latest is applied as a downgrade. Plugin compare drops pre-release suffixes. | `packages/download-core/src/sidecar_updates.rs:371`, `packages/download-core/src/plugin_sidecar.rs:225-237` |
| L-31 | Sidecar | The network `tag_name` becomes a path component without validation, a malformed local catalog is silently ignored, and a GitHub outage also fails plugin status. | `packages/download-core/src/github_release_source.rs:211-212`, `packages/download-core/src/github_release_source.rs:232` |
| L-32 | Sidecar | Every GitHub error, including a 403 rate limit, becomes "GitHub release lookup failed", with no retry and no negative cache. | `packages/download-core/src/github_release_source.rs:188-191` |
| L-33 | Sidecar | Each crash leaves a `.bundled-plugins-<pid>` staging directory. | `packages/download-core/src/plugin_sidecar.rs:269-273` |
| L-34 | Sidecar | The manifest `extractors` list is never validated, and catalog installs rebuild the manifest from defaults. | `packages/download-core/src/plugin_sidecar.rs:38-39`, `packages/download-core/src/sidecar_updates.rs:563` |
| L-35 | Plugins | `expected_status=(403, 429, 503)` makes a rate-limit page parse as "no media formats found". | `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/luxuretv.py:43`, `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/_parse.py:197-198` |
| L-36 | Plugins | `/thumb` filtering can drop real media paths, and `extract_title` does not unescape HTML entities. | `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/_parse.py:55`, `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/_parse.py:156-163` |
| L-37 | Plugins | The xxxbp list regex is case-sensitive, so `/Updates` is "Unsupported URL". | `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/xxxbp.py:117` |
| L-38 | Plugins | A non-dict JSON body raises `AttributeError` instead of `ExtractorError`. | `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/_parse.py:283`, `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/_parse.py:307` |
| L-39 | Plugins | Both luxuretv patterns match `/videos/16982`, so a list match can win and report "No videos found" (suspected). | `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/luxuretv.py:26`, `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/luxuretv.py:70` |
| L-40 | Plugins | `'ext'` is `mp4` in both branches of a conditional, and resolution assumes 16:9. | `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/_parse.py:99`, `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/_parse.py:106` |
| L-41 | Plugins | `setup.cfg` says 0.1.0 while the sidecar is 0.2.0, and `pyproject.toml` has no `[project]` table or yt-dlp requirement. | `packages/extractor-plugins/phlegethon-launch-sites/setup.cfg:3`, `packages/extractor-plugins/plugins-manifest.json:2` |
| L-42 | Plugins | `_parse.py` is a generic module name in the shared `yt_dlp_plugins.extractor` namespace and can collide with another plugin (suspected). | `packages/extractor-plugins/phlegethon-launch-sites/yt_dlp_plugins/extractor/_parse.py:1` |
| L-43 | Companion | Netscape cookie rows are written without escaping tabs or newlines, `http_only` is parsed and dropped, and cookie domains are not checked against the page URL. | `packages/companion-protocol/src/cookies.rs:29-41`, `packages/companion-protocol/src/cookies.rs:84-95` |
| L-44 | Companion | The cookie file is checked for an absolute path only after it is written, and the data directory falls back to `.` when no home exists. | `packages/companion-protocol/src/cookies.rs:98-104`, `packages/tauri-shell/src-tauri/src/lib.rs:1077-1081` |
| L-45 | Companion | The cookie channel is changed before the enqueue is validated, and skipped silently on a poisoned lock. | `packages/tauri-shell/src-tauri/src/companion.rs:34-50` |
| L-46 | Companion | An error reply that echoes a near-1 MiB URL exceeds the native-messaging cap, and the host exits with no reply. | `packages/companion-protocol/src/wire.rs:31-37`, `packages/companion-protocol/src/envelope.rs:59-60` |
| L-47 | Companion | Context menus are created in `onInstalled` without `removeAll()` first (suspected duplicate-id errors on update). | `packages/chrome-companion/background.js:9-30` |
| L-48 | Companion | The extension ignores `intent`, `referrer` and `force_direct` on the shell side, and collects cookies for the page origin even for link menu items. | `packages/tauri-shell/src-tauri/src/companion.rs:52-67`, `packages/chrome-companion/background.js:94-95` |
| L-49 | Shell | Core mutex poisoning is permanent, and setup uses `lock().unwrap()`. | `packages/tauri-shell/src-tauri/src/lib.rs:1138`, `packages/tauri-shell/src-tauri/src/lib.rs:1147`, `packages/tauri-shell/src-tauri/src/lib.rs:1153` |
| L-50 | Shell | Capabilities grant unused `dialog:default` and `opener:default`, and app commands are not gated by an app manifest. | `packages/tauri-shell/src-tauri/capabilities/default.json:10-11`, `packages/tauri-shell/src-tauri/build.rs:1-3` |
| L-51 | Shell | The CSP has no `base-uri` and allows unused `asset:` sources. | `packages/tauri-shell/src-tauri/tauri.conf.json:28-35` |
| L-52 | Shell | `focus_main_window` calls `show()` only when `set_focus()` succeeds. | `packages/tauri-shell/src-tauri/src/lib.rs:985-994` |
| L-53 | Shell | Every event is copied into a 2048-entry `event_buffer` that nothing reads, and a full buffer shifts about 2047 elements per event. | `packages/tauri-shell/src-tauri/src/lib.rs:213-219`, `packages/tauri-shell/src-tauri/src/lib.rs:998-1002` |
| L-54 | Frontend | The speed-estimator map keeps entries for jobs discarded mid-download. | `packages/tauri-shell/frontend/src/state/useJobs.ts:34`, `packages/tauri-shell/frontend/src/state/useJobs.ts:46-47` |
| L-55 | Frontend | The companion listener re-subscribes whenever the Save-to folder changes, leaving a short gap. | `packages/tauri-shell/frontend/src/App.tsx:97-107`, `packages/tauri-shell/frontend/src/App.tsx:152-167` |
| L-56 | Frontend | Enter that confirms an IME composition also submits the URL box. | `packages/tauri-shell/frontend/src/components/UrlBar.tsx:42-47` |
| L-57 | Frontend | The template preview has no sequence guard, so an older reply can overwrite a newer one. | `packages/tauri-shell/frontend/src/components/SettingsPage.tsx:126-137` |
| L-58 | Frontend | The audio-only switch flips before the save and does not revert on failure. | `packages/tauri-shell/frontend/src/components/SettingsPage.tsx:242-245` |
| L-59 | Frontend | Module-scope `t()` calls run before `initLocale()`, so those labels freeze in the default locale. | `packages/tauri-shell/frontend/src/components/QueueTable.tsx:64-70`, `packages/tauri-shell/frontend/src/components/HistoryTable.tsx:30-34`, `packages/tauri-shell/frontend/src/main.tsx:7` |
| L-60 | Frontend | The Discard confirmation text is built at click time and can over-warn when the job completes first. | `packages/tauri-shell/frontend/src/App.tsx:241-251` |
| L-61 | Frontend | The folder chooser has no timeout or cancel, so a dialog that never returns keeps the button disabled. | `packages/tauri-shell/frontend/src/components/FolderPicker.tsx:36-55` |

## 5. UI pass for niri and other tiling compositors

### 5.1 Method

This pass reads the SCSS, the React markup and the Carbon source in `node_modules` (`@carbon/react` 1.118.0, `@carbon/styles` 1.117.0), plus the Tauri window configuration. Nothing was rendered. Pixel figures are estimates from CSS constants, with IBM Plex at about 6.2 px per character at 12 px and 7.5 px at 14 px. Verify them in a real niri session before changing breakpoints.

The frontend has no `@media` or container queries anywhere in `packages/tauri-shell/frontend/src`. Every layout uses fixed minimums with fluid widths, so it does not adapt when niri snaps the window to a preset column width.

### 5.2 Window configuration

| Setting | Value | Source |
| --- | --- | --- |
| Default size | 960 × 640 | `packages/tauri-shell/src-tauri/tauri.conf.json:17-24` |
| Minimum size | 480 × 320 | `packages/tauri-shell/src-tauri/tauri.conf.json:20` |
| Decorations | Not set, so the default `true` applies | `packages/tauri-shell/src-tauri/tauri.conf.json:22` |
| Identifier | `com.phlegethon.app` | `packages/tauri-shell/src-tauri/tauri.conf.json:5` |
| GTK app id | Not set (`enableGTKAppId` defaults to false), so the Wayland app_id is probably the binary name `phlegethon` | Tauri 2.12.4 `app.rs:2446-2448` |
| Desktop file | `phlegethon.desktop`, no `StartupWMClass` | `packaging/linux/phlegethon.desktop:1-11` |
| Drag and drop | `dragDropEnabled: false`, correct because the URL bar uses HTML5 drop | `packages/tauri-shell/frontend/src/components/UrlBar.tsx:49-56` |

### 5.3 Width budget at niri column presets

niri's default preset column widths are 1/3, 1/2 and 2/3 of the output, with 16 px gaps. The table gives approximate tile widths in logical pixels, computed as (output width − gap) × proportion − gap. Borders, struts and custom gaps change the numbers slightly.

| Output (logical) | 1/3 | 1/2 | 2/3 | Notes |
| --- | --- | --- | --- | --- |
| 1280 (1920 at 1.5× scale) | about 405 | about 616 | about 827 | 1/3 is below `minWidth` 480. |
| 1366 | about 434 | about 659 | about 884 | 1/3 is below `minWidth` 480. |
| 1920 | about 619 | about 936 | about 1253 | Only 2/3 and full width fit the queue table. |
| 2560 | about 832 | about 1256 | about 1680 | 1/2 and wider fit. |

The main content area is the window width minus 48 px (`packages/tauri-shell/frontend/src/styles/index.scss:63`). Estimated minimum widths:

| Element | Needs about | Fits without sideways scroll at window width |
| --- | --- | --- |
| Queue table (UI-1) | 1000 px of content | about 1050 px and wider |
| Batch-action bar (UI-4) | 600 to 650 px | about 700 px and wider |
| Queue toolbar (UI-5) | 332 px of dropdowns plus search | about 520 px and wider |
| Probe results table (UI-6) | title plus 208 px quality plus 92 px size | about 950 px for typical titles |

On a 1920 × 1080 output, the queue table scrolls sideways in the 1/3 and 1/2 columns, and the batch bar clips Discard in the 1/3 column. Only the 2/3 column or a maximized column shows the queue without scrolling.

Height budget: niri stacks windows in a column. At 1080 logical pixels, two stacked windows get about 516 px each. The chrome above the queue rows is estimated at about 470 to 520 px at narrow widths (UI-7), so a stacked, narrow Phlegethon window shows almost no queue rows. At 768 px, three stacked windows get about 235 px each, which is below `minHeight` 320.

### 5.4 Findings

#### UI-1 (high). The queue table needs about 1000 px and scrolls sideways at most column widths

- **Location:** `packages/tauri-shell/frontend/src/styles/index.scss:205-219` (title and URL `white-space: nowrap` with `max-width: 34rem`), `packages/tauri-shell/frontend/src/styles/index.scss:224` (`.ph-queue__progress { min-width: 14rem; }`), `packages/tauri-shell/frontend/src/styles/index.scss:186-190` (scroll wrapper), `packages/tauri-shell/frontend/src/components/HistoryTable.tsx:121-161` (same pattern).
- **Problem:** The nowrap title and URL (up to 544 px) feed the table's min-content width, and the progress cell has a 224 px floor. Carbon's table content is `overflow-x: auto`, so the table scrolls inside the panel.
- **Effect:** At 440, 540, 620 and 950 px windows the pause, retry and overflow-menu column sits off-screen. Users must scroll sideways to act on a row. History behaves the same.
- **Fix:** Give the title and URL `max-width: 100%` with `min-width: 0` on the cell wrapper, so they ellipsize. Lower the progress minimum to about 10rem. Below about 800 px, hide the size column or merge it into the progress helper text.
- **Confidence:** Verified for the CSS. Widths are estimates.

#### UI-2 (high). `minWidth` 480 is wider than the 1/3 column on 1280 and 1366 px outputs

- **Location:** `packages/tauri-shell/src-tauri/tauri.conf.json:20`.
- **Problem:** niri cannot shrink a window below its minimum size, so the 1/3 preset cannot hold the window there. niri is expected to widen the column to 480 px, which breaks the proportion when cycling presets and pushes neighbouring columns along the strip.
- **Fix:** Lower `minWidth` to about 360 and make the layout work at that width (UI-1, UI-4, UI-5 and UI-6 are required anyway).
- **Confidence:** Verified for the config. The niri behaviour is expected but was not tested.

#### UI-3 (medium). Toasts cover modals and the row actions

- **Location:** `packages/tauri-shell/frontend/src/styles/index.scss:461-469` (`position: fixed; z-index: 9000; max-width: 28rem`), `packages/tauri-shell/frontend/src/state/useToasts.tsx:69`.
- **Problem:** The toast stack is a later DOM sibling with the same z-index as Carbon modals (9000), so toasts paint over open modals. Carbon toasts are 288 px wide below the largest breakpoint, so the 28rem rule has no effect.
- **Effect:** At 440 px a toast covers about 65 % of the width. Up to five stacked toasts cover the bottom 400 px, which hides the row Actions column and the probe sheet's "Download N selected" button. Error toasts stay for 12 s.
- **Fix:** Anchor the stack at `top: 3.5rem; right: 1rem`, cap it at three, and set its z-index below the modal.
- **Confidence:** Verified for CSS and DOM order. Click interception is suspected.

#### UI-4 (medium). The batch-action bar clips Discard below about 700 px

- **Location:** `packages/tauri-shell/frontend/src/components/QueueTable.tsx:202-234`. Carbon positions the bar absolutely with `white-space: nowrap`.
- **Effect:** With rows selected, the four labelled buttons need 600 to 650 px. At 620 px Discard is partly cut off. At 440 and 540 px Resume, Retry and Discard are off-screen.
- **Fix:** Below about 700 px, collapse the batch actions into an `OverflowMenu` or icon-only buttons with tooltips.
- **Confidence:** Verified for the CSS. Widths are estimates.

#### UI-5 (medium). The queue toolbar does not wrap

- **Location:** `packages/tauri-shell/frontend/src/styles/index.scss:252-259` (two dropdowns with `min-width: 10rem`), `packages/tauri-shell/frontend/src/components/QueueTable.tsx:243-266`.
- **Effect:** At 440 px the dropdowns take about 332 of 392 px, leaving about 60 px for search, and the Sort dropdown can be clipped at the window edge.
- **Fix:** Add `flex-wrap: wrap` to the toolbar below about 700 px, or move the dropdowns to a second row.
- **Confidence:** Verified for the non-wrapping flex row.

#### UI-6 (medium). Probe sheet titles overflow sideways and hide the quality picker

- **Location:** `packages/tauri-shell/frontend/src/styles/index.scss:314-330` (nowrap title with `max-width: 28rem`, quality `min-width: 11rem`), `packages/tauri-shell/frontend/src/components/ProbeModal.tsx:390`, `packages/tauri-shell/frontend/src/components/ProbeModal.tsx:402-429`.
- **Effect:** At 440 px the modal is full width with about 60 px left for the title, so almost every row scrolls sideways and hides the quality dropdown. At 620 px titles longer than about 28 characters overflow.
- **Fix:** Let titles wrap (`white-space: normal; overflow-wrap: anywhere`) and lower the quality minimum to about 8rem below 700 px.
- **Confidence:** Verified for the CSS. Thresholds are estimates.

#### UI-7 (medium). The URL panel is tall at narrow widths and short heights

- **Location:** `packages/tauri-shell/frontend/src/styles/index.scss:106-110` (`grid-template-columns: minmax(0, 1fr) auto`), `packages/tauri-shell/frontend/src/styles/index.scss:135`, `packages/tauri-shell/frontend/src/components/UrlBar.tsx:100-116`, `packages/shell-strings/en.json:86` (127-character helper text).
- **Effect:** The helper text sits in the column narrowed by the Add button. At 440 px it wraps to about four lines and the panel reaches about 215 px. With the header, tabs and toolbar, about 520 px of chrome sits above the first queue row, which leaves no full row at 590 px height and none in a stacked niri column.
- **Fix:** Move the helper into a tooltip or shorten it, put Add on its own row below about 700 px, or collapse the panel after the first submit.
- **Confidence:** Verified for the CSS and string. Heights are estimates.

#### UI-8 (medium). The "Type a path" dialog is nested inside the probe sheet and is mispositioned

- **Location:** `packages/tauri-shell/frontend/src/components/FolderPicker.tsx:82-83`, rendered inside `packages/tauri-shell/frontend/src/components/ProbeModal.tsx:461-467`.
- **Problem:** The open Carbon modal container keeps a `transform`, which makes it the containing block for `position: fixed` descendants. The nested modal's 100vw × 100vh backdrop is sized and placed relative to the probe box, not the window.
- **Effect:** At widths of 672 px and more, the nested backdrop covers only part of the window and the dialog sits off-centre over the probe content.
- **Fix:** Lift the dialog to `App` state, render it with `createPortal(…, document.body)`, or use an inline `TextInput`.
- **Confidence:** Suspected. The transform rule is confirmed; the rendering is not.

#### UI-9 (medium). Prompt dialogs open with focus on the close button, and Enter does nothing

- **Location:** `packages/tauri-shell/frontend/src/components/PromptModal.tsx:26-53`, `packages/tauri-shell/frontend/src/components/FolderPicker.tsx:83-96`.
- **Problem:** Neither dialog passes `shouldSubmitOnEnter` or marks the input with `data-modal-primary-focus`, so Carbon focuses the × button.
- **Effect:** Keyboard-driven users, common on tiling compositors, need several Tab presses to reach the field in "Add mirror" or "Speed limit", then more to reach the primary button.
- **Fix:** Add `shouldSubmitOnEnter` and `data-modal-primary-focus` on the `TextInput` in both dialogs.
- **Confidence:** Confirmed against Carbon source.

#### UI-10 (low). Focus is not returned to the trigger after a modal closes

- **Location:** `packages/tauri-shell/frontend/src/components/ProbeModal.tsx:261`. No modal passes a launcher ref.
- **Effect:** After cancelling the probe sheet, focus probably drops to `body`, and the next Tab starts at the top of the page.
- **Fix:** Pass the URL field or originating button as the launcher ref to the probe, Discard and duplicate modals.
- **Confidence:** Suspected.

#### UI-11 (low). Dark theme flashes light on startup

- **Location:** `packages/tauri-shell/frontend/src/styles/index.scss:15-19` (`:root` defaults to light), `packages/tauri-shell/frontend/src/state/useTheme.ts:31-38` (theme applied in `useEffect`).
- **Effect:** Dark-theme users see a white frame when a niri column opens the window.
- **Fix:** Set `data-theme` from an inline script in `index.html` before the module loads (allowed by the CSP only if hashed), or add a `prefers-color-scheme: dark` default to `:root`.
- **Confidence:** Confirmed. Flash duration suspected.

#### UI-12 (low). The Wayland app_id is not the documented identifier

- **Location:** `packages/tauri-shell/src-tauri/tauri.conf.json:5`, `packaging/linux/phlegethon.desktop:1-11`, `packaging/linux/com.phlegethon.app.metainfo.xml:3`.
- **Problem:** With `enableGTKAppId` off, GTK sets no application id and the app_id falls back to the program name, probably `phlegethon`. That matches `phlegethon.desktop`, but the metainfo id is `com.phlegethon.app`.
- **Effect:** A niri window rule written as `match app-id="com.phlegethon.app"` does not match. Turning on `enableGTKAppId` would fix the rule but break the desktop-file match unless the desktop file is renamed.
- **Fix:** Confirm the real value with `niri msg windows`, then document it. If the reverse-DNS id is wanted, set `enableGTKAppId: true` and rename the desktop file to `com.phlegethon.app.desktop` together with the metainfo `launchable`.
- **Confidence:** Confirmed for the configuration. The exact app_id string is suspected.

#### UI-13 (low). Window decorations are left at the default

- **Location:** `packages/tauri-shell/src-tauri/tauri.conf.json:17-24` (no `decorations` key).
- **Problem:** GTK3 does not implement `xdg-decoration`, so niri's `prefer-no-csd` probably cannot remove client-side decorations. Depending on GTK behaviour, the window shows either a GTK title bar or only a shadow and border margin.
- **Effect:** In a tiled column a title bar wastes about 37 px of height, and its maximize and minimize buttons do nothing useful in niri. Floating windows may have a shadow margin.
- **Fix:** Take a screenshot in niri first. If a title bar is drawn, set `"decorations": false` and add `data-tauri-drag-region` to the Carbon header so floating windows can still be moved.
- **Confidence:** Suspected.

#### UI-14 (low). Dead CSS would double the header offset if the inline style were removed

- **Location:** `packages/tauri-shell/frontend/src/styles/index.scss:49-52` (`padding-top: 3rem`), overridden at `packages/tauri-shell/frontend/src/App.tsx:340` (`style={{ padding: 0 }}`).
- **Fix:** Delete the `padding-top` rule.
- **Confidence:** Confirmed. No visible effect today.

#### UI-15 (low). Row quality menus in the probe sheet open inside the scroll area

- **Location:** `packages/tauri-shell/frontend/src/components/ProbeModal.tsx:411-426` (Dropdown without `autoAlign`).
- **Effect:** A menu opened near the bottom of the sheet extends the scroll area instead of opening upward, which matters most in short stacked windows.
- **Fix:** Pass `autoAlign`.
- **Confidence:** Suspected.

#### UI-16 (low). Two action columns have no header label

- **Location:** `packages/tauri-shell/frontend/src/components/QueueTable.tsx:69`, `packages/tauri-shell/frontend/src/components/HistoryTable.tsx:128`.
- **Fix:** Add a visually hidden "Actions" label.
- **Confidence:** Confirmed.

#### UI-17 (low). External picker dialogs open as new niri columns

- **Location:** `packages/shell-support/src/picker.rs` (zenity and kdialog are spawned as separate processes).
- **Problem:** zenity and kdialog run as separate top-level windows with no parent relation to Phlegethon. niri tiles such windows as a new column unless a window rule floats them.
- **Effect:** "Choose folder" scrolls the strip to a new column instead of showing a dialog over the app.
- **Fix:** Prefer the portal path (fix H-20 first), and document a niri window rule that floats the zenity picker.
- **Confidence:** Suspected.

#### UI-18 (low). Long clipboard URLs may overflow the clipboard hint

- **Location:** `packages/tauri-shell/frontend/src/components/UrlBar.tsx:148` (raw URL as the notification subtitle).
- **Effect:** A long URL without spaces may push past the inline notification at narrow widths.
- **Fix:** Apply `overflow-wrap: anywhere` to the subtitle, or truncate it with the full URL in a tooltip.
- **Confidence:** Suspected; Carbon's wrapping was not checked.

### 5.5 Overlapping and clipped text

No case was found where two text runs paint on top of each other inside normal flow. Long paths and URLs wrap with `break-all` (`packages/tauri-shell/frontend/src/styles/index.scss:348`, `packages/tauri-shell/frontend/src/styles/index.scss:361`, `packages/tauri-shell/frontend/src/styles/index.scss:420`, `packages/tauri-shell/frontend/src/styles/index.scss:434`), the destination chip truncates, and the Settings grid collapses to one column at 440 px. The overlap and clipping cases are all layered or overflow cases:

- Toasts paint over open modals and over the table's Actions column (UI-3).
- The nested "Type a path" backdrop and dialog are placed relative to the probe sheet and overlay part of it (UI-8).
- The batch-action bar clips Discard and, at narrow widths, Resume and Retry (UI-4).
- The toolbar squeezes search to about 60 px and can clip the Sort dropdown (UI-5).
- Queue, History and probe titles run under the panel edge and need sideways scrolling instead of ellipsizing (UI-1, UI-6).
- The progress helper text wraps inside the 224 px progress cell and makes rows taller (`packages/tauri-shell/frontend/src/styles/index.scss:224`).
- Clipboard hint subtitles with long URLs may overflow the notification (UI-18).

### 5.6 Snapping and resizing behaviour

- **Preset widths:** The layout has no breakpoints, so each niri preset lands on an arbitrary state of the fixed-minimum layout. Breakpoints or container queries at about 480, 700 and 1050 px would match the 1/3, 1/2 and 2/3 presets on common outputs.
- **Minimum size:** `minWidth` 480 blocks the 1/3 preset on 1280 and 1366 px outputs (UI-2). `minHeight` 320 blocks three-high stacks on 768 px outputs.
- **Resize cost:** Every progress chunk triggers a full React render (H-25). During downloads this competes with the relayout that each niri resize and column switch triggers, so snapping is likely to feel slow. Coalescing progress fixes both.
- **Keyboard:** Tiling users drive the app from the keyboard, but there are no shortcuts beyond Enter in the URL box and Ctrl+V outside inputs (`packages/tauri-shell/frontend/src/components/UrlBar.tsx:42-72`), and the prompt dialogs do not focus their fields (UI-9).
- **Window identity:** Window rules need the real app_id (UI-12).
- **Dialogs:** Folder picking depends on a working FileChooser portal (H-20), and the fallbacks open as new columns (UI-17).

### 5.7 Checked and fine

- The Carbon `Tabs` chain keeps bounded heights down to `.ph-scroll`, so Settings and History scroll correctly.
- `body` is `overflow: hidden`, and every scroll region is bounded.
- Carbon modal sizes adapt (`lg` is 96 % at medium widths, full width below), and the 92vh override keeps the footer visible.
- The duplicate-URL dialog's three buttons fit at 440 px.
- Escape closes only the topmost modal, and the danger-styled Discard dialog focuses Cancel.
- Icon-only buttons carry labels and tooltips. Focus rings are not overridden.

### 5.8 Suggested niri configuration for users

Until UI-1, UI-2 and UI-12 are fixed, a user can open Phlegethon in a wide column and float external pickers. This snippet is untested; check the app_id with `niri msg windows` first.

```kdl
window-rule {
    match app-id="^phlegethon$"
    default-column-width { proportion 0.66667; }
}

window-rule {
    match app-id="^zenity$"
    open-floating true
}
```

## 6. Functional improvement opportunities

### Downloads and the queue

- Store `ETag`, `Last-Modified` and size on the first response and send `If-Range` on resume (B-2, H-5).
- Persist per-segment progress so Pause and crash recovery keep finished segments (H-4, M-15).
- Keep one global token bucket on `Core` and make the throttle block instead of returning `Ok(0)` (H-2, H-3).
- Implement extractor resume through yt-dlp's `--continue`, or hide Pause for jobs that cannot resume (H-10).
- Parse yt-dlp `--progress-template` output instead of walking the disk every 50 ms (M-29).
- Let Retry resume from the `.part` when validators still match, instead of always starting from zero (`packages/download-core/src/job.rs:563-571`).
- Emit an event from `reorder_job` and from segmented Pause so the UI need not poll.
- Report the failing probe tier in `ProbeFailed`.
- Batch `select_default_quality` into one IPC call for large playlists (`packages/tauri-shell/frontend/src/components/ProbeModal.tsx:142-147`).
- Make long single-tier probes cancellable by passing the cancel flag into the HTTP read loop.

### Features that exist in the backend but have no UI

- Plugin updates can be checked but not applied. `applySidecarUpdates` (`packages/tauri-shell/frontend/src/tauri/api.ts:117`) has no caller.
- Integrity Check has no entry point. `setJobExpectedHash` (`packages/tauri-shell/frontend/src/tauri/api.ts:71-72`) has no caller, and the row menu has no "Set expected hash" item.
- The Mirror Set is invisible. `jobMirrors` (`packages/tauri-shell/frontend/src/tauri/api.ts:67`) has no caller, and failovers are not shown.
- The speed-limit prompt does not show the current cap. `getJobBandwidthCap` (`packages/tauri-shell/frontend/src/tauri/api.ts:70`) has no caller.
- History has an outcome filter but no text search, sort or re-download (`packages/tauri-shell/frontend/src/components/HistoryTable.tsx:30-33`), and it loads the whole archive without pagination.

### UI and UX

- Add responsive breakpoints or container queries for the niri presets (section 5.6).
- Add keyboard shortcuts: focus the URL box, act on selected rows, switch tabs.
- Make the header "Toolchain" icon scroll to the Toolchain section, as the cookie link already does (`packages/tauri-shell/frontend/src/App.tsx:320-322`).
- Put the full path in the "Save to" tooltip instead of "Clear Save to folder" (`packages/tauri-shell/frontend/src/components/UrlBar.tsx:125`).
- Make the queue table header sticky.
- Replace the `href="#"` header link (`packages/tauri-shell/frontend/src/App.tsx:305`).
- Make "Show existing" scroll to and highlight the row.
- Report skipped duplicates instead of only the enqueued count (`packages/tauri-shell/frontend/src/App.tsx:137-141`).
- Confirm Toolchain rollback before running it (`packages/tauri-shell/frontend/src/components/ToolchainPanel.tsx:299`).
- Show a summary from `parseCoreError` in failure notifications instead of raw error text (`packages/tauri-shell/frontend/src/App.tsx:90`).
- Show the active cookie source in Settings.

### Platform Shell, sidecar and companion

- Forward URLs from a second launch to the running instance (H-19).
- Route reveal, open and folder picking through one file-manager helper with `xdg-open` and copy-path fallbacks.
- Health-check every installed sidecar binary with `--version` and roll back on failure.
- Add an opt-in nightly yt-dlp channel (M-42).
- Move sidecar settings to app level (L-27).
- Surface yt-dlp plugin-load failures, which today look like "unsupported Launch Site".
- Show a preview in the shell of what a companion hand-off will do before it starts.
- Pass the referrer from the extension through to yt-dlp.

## 7. Gaps

### 7.1 Behaviour that contradicts CONTEXT.md

| Glossary term | What the glossary says | What the code does | Findings |
| --- | --- | --- | --- |
| Resumable Transfer | Continue from the last successful byte; avoid restart-from-zero. | Extractor jobs fail on Resume, segmented Pause restarts from zero, a complete `.part` fails with 416, and resume can duplicate bytes. | B-2, H-10, M-14, M-15 |
| Launch Site | Avoid silent generic fallback as "supported". | Subdomains take the generic path. | H-12 |
| Discard | Stops the transfer and deletes the job's files; no orphaned partials. | ffmpeg remux and yt-dlp children keep running, a discard during completion is lost, and template output outside the Job Destination survives. | H-9, H-14, M-2, M-11 |
| Job Destination | Always a job-owned directory. | Templates and persisted values can write or delete outside it. | M-8, M-11 |
| Bandwidth Cap | Optional global and per-job limit. | Neither limit is enforced correctly. | H-2, H-3, M-10 |
| Active Job Limit | Maximum concurrent transfers. | Planning does not recheck the limit, and overlapping rounds can exceed it. | M-3 |
| Mirror Set | Equivalent source URLs. | Failover appends one mirror's bytes onto another's. | H-5, M-16 |
| Integrity Check | Verify against a known hash when available. | Metalink and multi-file sidecar hashes are dropped or mismatched. | H-15, M-17 |
| Download Archive | Avoid a History that disagrees with the archive. | A failed retry replaces the Completed entry. | M-12 |
| Extractor Plugin | Avoid user-installed untrusted plugins. | The catalog path installs unverified Python. | M-43 |
| Chrome Companion | Use the same cookie channel as desktop settings. | The hand-off replaces the user's persisted choice instead of applying to one job. | M-51 |
| Default Quality Policy | Audio-only overrides the video policy. | Audio-only can pick a video stream. | M-20 |
| Probe-then-Confirm | Avoid auto-start without review. | Any local process can enqueue and start a direct download through the companion socket. | H-18 |

### 7.2 Spec features not implemented or partly implemented

- Apply-when-idle for sidecar updates (L-26).
- Toolchain pins to versions other than the latest (M-31).
- History search, sort and re-download (`CODE_REVIEW.md` roadmap item 14).
- Locales other than English. `SUPPORTED_LOCALES` is `["en"]` (`packages/tauri-shell/frontend/src/lib/i18n.ts:10`), and module-scope labels would not translate (L-59).
- Time-based scheduling is out of v1 by `CONTEXT.md`, so it is not a defect.

### 7.3 CI and release gaps

| ID | Severity | Gap | Location |
| --- | --- | --- | --- |
| G-1 | High | No workflow runs on pull requests or pushes. `release.yml` triggers only on tags and manual runs, so `cargo fmt`, clippy, typecheck and the shell crate tests are never gated. `cargo fmt --check` already fails. | `.github/workflows/release.yml:11-14` |
| G-2 | Medium | A manual run with an existing version rebuilds and overwrites the published assets while the tag stays on the old commit. | `.github/workflows/release.yml:125-132` |
| G-3 | Medium | `make desktop` always exits 0, because the `\|\|` branch also runs when validation fails. | `Makefile:73-74` |
| G-4 | Medium | `make dist` includes untracked files, today `CLAUDE.md` and `graphify-out/` with local `/home` paths. `graphify-out/` is not in `.gitignore`. | `scripts/make-dist-tarball.sh:57`, `.gitignore` |
| G-5 | Medium | The release version check covers two of the six version locations, plus the undocumented seventh in `setup.cfg`. | `.github/workflows/release.yml:62-66`, `scripts/make-dist-tarball.sh:39-45` |
| G-6 | Medium | CI runs only one of the two Python plugin tests, and the other skips itself without yt-dlp. | `.github/workflows/release.yml:45-46`, `Makefile:62-63` |
| G-7 | Medium | Nothing checks that the packaged binary embeds the frontend (M-58). | `packaging/void/build-in-container.sh:107-115` |
| G-8 | Low | Workflow-level `contents: write`, and actions pinned by tag, not SHA. | `.github/workflows/release.yml:24-25`, `.github/workflows/release.yml:36-37` |
| G-9 | Low | void-packages and the container image are not pinned, so a rebuild of one tag can differ. | `packaging/void/build-in-container.sh:21`, `scripts/xbps-build.sh:44`, `.github/workflows/release.yml:102` |
| G-10 | Low | Container scripts and README instructions are docker-only. A `CONTAINER_ENGINE` variable would allow podman; rootless podman with `--privileged` and uchroot is untested. | `README.md:32`, `packaging/void/build-in-container.sh:5`, `.github/workflows/release.yml:101` |
| G-11 | Low | `make test` runs cargo without `--locked`, `TARGET` ignores `CARGO_TARGET_DIR`, and `make install` does not install `LICENSE`. | `Makefile:18`, `Makefile:35-45`, `Makefile:59` |
| G-12 | Low | The shell crate repeats workspace metadata instead of inheriting it, which is why its version is a separate location, and `authors` is the placeholder `["you"]`. | `packages/tauri-shell/src-tauri/Cargo.toml:3-8` |
| G-13 | Low | Script `--help` output ends with a printed `set -euo pipefail`, and `setup-void.sh --force` is undocumented. | `scripts/setup-void.sh:8`, `scripts/setup-void.sh:16`, `scripts/install-nmh-linux.sh:9`, `scripts/make-dist-tarball.sh:10` |
| G-14 | Low | `install-nmh-linux.sh` writes the host path into JSON unescaped and does not check for a missing `--extension-id` value. | `scripts/install-nmh-linux.sh:16`, `scripts/install-nmh-linux.sh:116-124` |
| G-15 | Low | The NMH manifest template is read by nothing. The script writes its own JSON, and `render_manifest` in shell-support is used only by tests, so three definitions can drift. | `packages/native-messaging-host/manifests/com.phlegethon.bridge.json:4-7`, `packages/shell-support/src/nmh.rs:79-105` |
| G-16 | Low | Windows and macOS icons remain although the bundler is disabled. | `packages/tauri-shell/src-tauri/icons/icon.icns` |
| G-17 | Low | The metainfo has no `<developer>` or `<releases>` entry. | `packaging/linux/com.phlegethon.app.metainfo.xml:3-16` |
| G-18 | Low | The frontend `package.json` has no `engines` field, although the toolchain needs Node 22.12 or newer. | `packages/tauri-shell/frontend/package.json:43` |
| G-19 | Low | The e2e harness silently disables the WebKit sandbox. | `packages/tauri-shell/e2e/reference_ui.py:78` |

### 7.4 Test coverage gaps

The suite passes, but it does not exercise the paths behind most findings:

- **Transfers:** no test for a 200 reply to a Range request (B-2), a throttled fetch without `Content-Length` (H-2), the global cap across dispatch rounds (H-3), crash recovery of a segmented `.part` (H-4), failover after a partial body (H-5), a 416 on a complete `.part` (M-14), unsupported proxy schemes (H-16), or DoH back-pressure (H-17).
- **Queue:** no test that discards or reorders another job during a transfer and checks which job completes (B-1). The test at `packages/download-core/tests/core_api.rs:5487-5549` stops early. Nothing covers `set_library_root` during a transfer (H-7), a corrupt or partial `jobs.json` (H-8), a panic in a transfer thread (H-6), a failed `remove_dir_all` on Discard (M-1), or a discard during completion (M-2).
- **Probing:** no test for Launch Site subdomains (H-12), manifest content types (H-13), playlist entries that start with `-` (H-11), the download argument vector, a `[code]` surviving the tier-3 wrapper (M-25), non-ASCII `Content-Disposition` (M-27), or kbit/s `tbr` values (the fixtures use bit/s).
- **Sidecar:** no test for a SUMS failure (H-22), a non-latest pin (M-31), rollback followed by a launch check (M-32), a plugin pin older than bundled (M-33), a partial apply (M-35), manifest strings with `..` (M-37), cache refresh after rebind (H-24), or a non-executable sidecar binary. The test at `packages/download-core/src/ytdlp_extractor.rs:2016-2030` asserts that a non-executable file binds. The cache test at `packages/download-core/src/github_release_source.rs:383-388` never asserts caching.
- **Python plugins:** `packages/extractor-plugins/tests/test_extractors_offline.py:19-28` skips every test without yt-dlp, and its mocks at lines 40 to 45 replace the yt-dlp calls whose signatures are in question (M-46). No test covers the xxxbp page-2 failure (M-44) or the luxuretv fallback (M-45). Nothing loads the installed plugin package through yt-dlp.
- **Companion:** the only IPC test (`packages/companion-protocol/src/ipc.rs:161-188`) uses the real socket name, so it collides with a running app, and it covers no idle, oversize or malformed frames. Cookie tests cover one happy-path row. The extension and `install-nmh-linux.sh` have no tests.
- **Frontend:** `App.test.tsx` mocks the event module entirely and hard-wires `find_active_job_for_url` to `null`, so the companion flow (H-1), the duplicate flow, progress merging and notifications never run. There are no tests for `useJobs`, `useToasts`, `useTheme`, `api.ts`, `events.ts`, `platform.ts`, `DuplicateUrlModal` or `PromptModal`, and none for load failures in Settings or the Toolchain panel. The mock `split_urls_cmd` splits on whitespace, while the real one also splits on commas and semicolons, so tests pass against behaviour the app does not have.
- **i18n:** the test checks only static keys. It does not report unused keys, placeholder mismatches, or literals in `history.ts` and `errors.ts`.
- **Contract:** no test pins the TypeScript types to the Rust serde shapes (M-71).
- **UI:** no layout test at narrow widths. `make e2e-ui` exists but is not in CI.

### 7.5 Documentation gaps

- `CLAUDE.md` says the NMH template placeholders are filled by the install script; they are not (G-15).
- `CLAUDE.md` says `connect-src` is only `ipc:`, but `packages/tauri-shell/src-tauri/tauri.conf.json:34` also allows `http://ipc.localhost`.
- `CLAUDE.md` lists six version locations; `setup.cfg` is a seventh.
- `CLAUDE.md` calls `CODE_REVIEW.md` partly stale. Every item in it is now fixed or stale (section 8).
- `README.md:28` says releases publish on a `v*` tag; manual runs with a version publish too.
- `docs/companion-setup.md:31` builds without `--release` and without `--features custom-protocol`, the exact build the README warns against. `docs/companion-setup.md:12` says "named pipe", a Windows term. The heading at `docs/companion-setup.md:55` lists Brave, but the table covers only Chrome and Chromium.
- `docs/agents/caveman.md:5` holds an absolute path into another user's home directory. `docs/agents/triage-labels.md:15` is unedited template text ("Edit the right-hand column to match whatever vocabulary you actually use").
- `docs/research/chrome-companion-bridge.md` and `docs/research/extractor-toolchain.md` still describe Windows 11, WinUI and `yt-dlp.exe` with no superseded banner (for example `docs/research/extractor-toolchain.md:161`).
- `docs/spec/hand-off.md:131-132` describes the Job Destination without Save-to overrides, which `CONTEXT.md` now defines.
- Stale "C ABI" comments remain at `packages/download-core/src/lib.rs:864` and `packages/download-core/src/lib.rs:953`.
- No document tells niri or other tiling-compositor users which app_id to match, which portal backend the folder picker needs, or that a notification daemon is required.

### 7.6 Code hygiene

- `cargo fmt --check` fails on 49 files.
- Clippy dead code: `packages/download-core/src/bandwidth.rs:79` (`effective_rate_bytes_per_sec`), `packages/download-core/src/bandwidth.rs:107` (`rate`), `packages/download-core/src/doh_ytdlp.rs:17` (`proxy_guard`), `packages/download-core/src/job.rs:343` (`ytdlp_output_spec`, `clear_expected_hash`), `packages/download-core/src/transfer.rs:27` (`bytes_written`, which is the unchecked count behind H-4), `packages/download-core/src/ytdlp_extractor.rs:533` (`probe_args`), `packages/download-core/src/sidecar_updates.rs:799` (`write_minimal_plugin_package`).
- Other clippy warnings: `if_same_then_else` at `packages/download-core/src/filename_template.rs:56`, unused import `PROTOCOL_VERSION` at `packages/native-messaging-host/src/main.rs:5`, `enum_variant_names` at `packages/tauri-shell/src-tauri/src/lib.rs:118`, plus `derivable_impls`, `double_ended_iterator_last` and `type_complexity`.
- Dead or test-only code: `NIGHTLY_REPO` and `asset_needs_python` in `github_release_source.rs`, `needs_update` in `plugin_sidecar.rs`, `DeferredUntilIdle` in `sidecar_updates.rs`, `poll_events` and `event_buffer` in the shell (L-53), and frontend wrappers with no caller in `packages/tauri-shell/frontend/src/tauri/api.ts` (`normalizeUrl`, `pollEvents`, `jobMirrors`, `getJobBandwidthCap`, `setJobExpectedHash`, `getPlatformCapabilities`, `focusMainWindow`, `applySidecarUpdates`, `listHistoryByOutcome`, `historyLen`) and `openExternal` in `platform.ts`.
- Unused string keys in `packages/shell-strings/en.json`: `probe.close`, `probe.noSelection`, `probe.openUrl`, `probe.probed`, `probe.sourceAndUrl`, `queue.empty`, `queue.footer.apply`, `queue.footer.ready`, `url.enqueue`.
- The JS bundle is 659 kB, over Vite's 500 kB warning, and the CSS is 1.2 MB. Splitting Carbon imports or code-splitting Settings would reduce startup cost.
- 60 duplicate crates in the dependency tree. `cargo audit` is not part of the workflow.
- `FakeSidecarUpdateSource` is `pub` in a production module and contains `unwrap` calls (`packages/download-core/src/sidecar_updates.rs:237`, `packages/download-core/src/sidecar_updates.rs:243`).

## 8. CODE_REVIEW.md reconciliation

Every finding in `CODE_REVIEW.md` is now fixed or stale. Three roadmap items are partial. The file can be archived, and the "partly stale" note in `CLAUDE.md` removed.

| Item | Status | Evidence |
| --- | --- | --- |
| Discarding a single-item Save-to job deletes the chosen folder | Fixed | `packages/download-core/src/job.rs:164-178` nests a unique subfolder; tests at `packages/download-core/tests/core_api.rs:5054` and `packages/download-core/tests/core_api.rs:5102` |
| Extractor temp cleanup deletes browser `.part` files | Fixed by the ownership change | The sweep runs only inside the job-owned folder |
| `destination_byte_total` inflated by existing content | Fixed by the ownership change | Walks job-owned folders only |
| Tests used fresh paths only | Fixed | `packages/download-core/tests/core_api.rs:4971` and the two tests above |
| Progress can exceed 100 % | Fixed for HTTP jobs | `packages/tauri-shell/frontend/src/lib/progress.ts:10-11` clamps the display. Extractor progress can still pass the total internally (M-29). |
| C# indentation, C ABI export, old `app.js` scope | Stale | WinUI shell and C ABI removed by ADR 0001; frontend replaced by React (ADR 0002) |
| Poll loop depends on `stop` being honoured | Fixed | Contract documented at `packages/download-core/src/extractor.rs:6-14` |
| Roadmap 1 to 13, 15 to 18 | Fixed | Discard confirmation, restart resume, integrity, concurrency and reorder, disk check, media extras, quality policy, archive, remux, cookies, reveal and open, notifications, multi-URL paste, duplicate detection, templates, companion, toolchain updates |
| Roadmap 14: search, filter, sort, History re-download | Partial | Outcome filter only |
| Roadmap 19: proxy and scheduling | Partial | Proxy done (but see H-16); scheduling out of v1 |
| Roadmap 20: accessibility and i18n | Partial | English only; see UI-9, UI-10, UI-16 and L-59 |

Several "fixed" roadmap items still carry defects listed here: resume (B-2, H-10), integrity (H-15, M-17), concurrency (M-3), templates (M-11, L-2) and the companion (H-1, H-18).

## 9. Method and limitations

- Eight parallel reviewers covered: Download Core queue and storage, transfers and network, the Extractor Toolchain and probing, sidecar and Extractor Plugins, the Tauri backend and Chrome Companion, frontend logic, UI layout, and build, CI and docs. A separate run executed the tests and linters in section 3.
- Findings were deduplicated across reviewers. Blocker findings and most high findings were re-read at the cited lines while this report was assembled (marked Verified).
- One reviewer claimed that `classification::is_launch_site` has no runtime caller. That is wrong; it is called at `packages/download-core/src/lib.rs:240` and `packages/download-core/src/lib.rs:1212`, which is why H-12 has user-visible effect.
- The transfer and network review had no second review, so its medium findings are marked Confirmed, not Verified.
- yt-dlp is not installed on the reviewing machine. Claims about yt-dlp behaviour (`tbr` units, `--cookies` rewriting, plugin API signatures, external-downloader pipes) come from its documentation and are marked Suspected where they matter.
- The UI pass is static. No window was rendered, niri was not run, and pixel figures are estimates. Run `make e2e-ui` and a manual niri session at the preset widths before acting on the thresholds.
- Locked crate versions differ slightly from some registry sources that were read (tauri 2.12.4 against 2.12.1, tauri-plugin-opener 2.5.4 against 2.7.0). Findings that depend on those sources are marked Suspected.

## 10. Not inspected

- `prototypes/`, which is not product code.
- Runtime behaviour under niri: the real app_id, decoration rendering, min-size handling, and portal and notification daemon interaction.
- yt-dlp source, and live behaviour of the Launch Sites.
- `make e2e-ui`, `make probe-check` (needs network) and `scripts/install-nmh-linux.sh` beyond `--help`.
- The Carbon internals for toast auto-dismiss, DataTable selection retention and inline-notification wrapping.
- Fixture HTML contents beyond targeted searches, and the bodies of most `core_api.rs` tests (names only).
- The musl `.xbps` package contents, and whether `openssl-devel` and `librsvg-devel` build dependencies are needed.
- `cargo audit`, which is not installed.

## 11. Status

Status of each finding after the fix workstreams (GitHub issues #68 to #80). The PR column names the pull requests that carry the fix.

| ID | Status | PR |
| --- | --- | --- |
| B-1 | Fixed | #85 |
| B-2 | Fixed | #91, #99 |
| H-1 | Fixed | #84 |
| H-2 | Fixed | #94, #99 |
| H-3 | Fixed | #94, #99 |
| H-4 | Fixed | #95, #99 |
| H-5 | Fixed | #91, #99 |
| H-6 | Fixed | #96, #99 |
| H-7 | Fixed | #113 |
| H-8 | Fixed | #113, #133 |
| H-9 | Fixed | #96, #108, #115, #128, #133 |
| H-10 | Fixed | #114, #126 |
| H-11 | Fixed | #98 |
| H-12 | Fixed | #102 |
| H-13 | Fixed | #118 |
| H-14 | Fixed | #108 |
| H-15 | Fixed | #88 |
| H-16 | Fixed | #103 |
| H-17 | Fixed | #109 |
| H-18 | Fixed | #104 |
| H-19 | Fixed | #110 |
| H-20 | Fixed | #128 |
| H-21 | Fixed | #105, #125 |
| H-22 | Fixed | #107 |
| H-23 | Fixed | #129 |
| H-24 | Fixed | #123 |
| H-25 | Fixed | #86, #100, #130 |
| M-1 | Fixed | #113 |
| M-2 | Fixed | #113 |
| M-3 | Fixed | #96 / #99 |
| M-4 | Fixed | #113 |
| M-5 | Fixed | #113 |
| M-6 | Fixed | #113 |
| M-7 | Fixed | #113, #133 |
| M-8 | Fixed | #113 |
| M-9 | Fixed | #97 / #99 |
| M-10 | Fixed | #94 / #99 |
| M-11 | Fixed | #113 |
| M-12 | Fixed | #113 |
| M-13 | Fixed | #96 / #99 |
| M-14 | Fixed | #91 / #99 |
| M-15 | Fixed | #95 / #99 |
| M-16 | Fixed | #88 |
| M-17 | Fixed | #88 |
| M-18 | Fixed | #113 |
| M-19 | Fixed | #113 |
| M-20 | Fixed | #118 |
| M-21 | Fixed | #109 |
| M-22 | Fixed | #127 |
| M-23 | Fixed | #127 |
| M-24 | Fixed | #WAVE4 (secrets.json, ADR 0007) |
| M-25 | Fixed | #123 / #126 |
| M-26 | Fixed | #123, #126, #133 |
| M-27 | Fixed | #118 |
| M-28 | Fixed | #123 / #126 |
| M-29 | Partial | #114, #126, #WAVE4 (percentage is monotone; the total is not known up front) |
| M-30 | Fixed | #118 |
| M-31 | Fixed | #129 |
| M-32 | Fixed | #129 |
| M-33 | Fixed | #129 |
| M-34 | Fixed | #129 |
| M-35 | Fixed | #129 |
| M-36 | Fixed | #129 |
| M-37 | Fixed | #129 |
| M-38 | Fixed | #129 |
| M-39 | Fixed | #107 |
| M-40 | Fixed | #123 |
| M-41 | Fixed | #129 |
| M-42 | Fixed | #129 |
| M-43 | Fixed | #129 |
| M-44 | Fixed | #89 |
| M-45 | Fixed | #89 |
| M-46 | Fixed | #89 |
| M-47 | Fixed | #110 |
| M-48 | Fixed | #104 |
| M-49 | Fixed | #104 |
| M-50 | Fixed | #104 |
| M-51 | Fixed | #101, #105, #125 |
| M-52 | Fixed | #110 |
| M-53 | Fixed | #115, #133 (Discard guard regression) |
| M-54 | Fixed | #115 |
| M-55 | Fixed | #132 |
| M-56 | Fixed | #106 |
| M-57 | Fixed | #106 |
| M-58 | Fixed | #110 |
| M-59 | Fixed | #90, #100 |
| M-60 | Fixed | #90, #100 |
| M-61 | Fixed | #92, #100 |
| M-62 | Fixed | #120 |
| M-63 | Fixed | #92, #100 |
| M-64 | Fixed | #92, #100 |
| M-65 | Fixed | #86, #100 |
| M-66 | Fixed | #93, #100 |
| M-67 | Fixed | #92, #100 |
| M-68 | Fixed | #120 |
| M-69 | Fixed | #110 |
| M-70 | Fixed | #86, #100 |
| M-71 | Fixed | #130 |
| L-1 | Fixed | #113 |
| L-2 | Fixed | #113 |
| L-3 | Fixed | #113 |
| L-4 | Fixed | #113 |
| L-5 | Fixed | #97 |
| L-6 | Fixed | #113 |
| L-7 | Fixed | #97 |
| L-8 | Fixed | #113 |
| L-9 | Fixed | #113 |
| L-10 | Fixed | #113 |
| L-11 | Fixed | #113 |
| L-12 | Fixed | #113 |
| L-13 | Fixed | #91 |
| L-14 | Fixed | #97 |
| L-15 | Fixed | #109 |
| L-16 | Fixed | #118, #133 |
| L-17 | Fixed | #118 |
| L-18 | Fixed | #118, #WAVE4 |
| L-19 | Fixed | #123 |
| L-20 | Fixed | #123 |
| L-21 | Fixed | #123 |
| L-22 | Fixed | #123 |
| L-23 | Fixed | #123 |
| L-24 | Fixed | #107 |
| L-25 | Fixed | #107 |
| L-26 | Fixed | #129 |
| L-27 | Fixed | #129 |
| L-28 | Fixed | #123, #128 |
| L-29 | Fixed | #133 |
| L-30 | Fixed | #129 |
| L-31 | Fixed | #107 |
| L-32 | Fixed | #107 |
| L-33 | Fixed | #111 |
| L-34 | Fixed | #112 |
| L-35 | Fixed | #89 |
| L-36 | Fixed | #89 |
| L-37 | Fixed | #89 |
| L-38 | Fixed | #89 |
| L-39 | Fixed | #89 |
| L-40 | Fixed | #89 |
| L-41 | Fixed | #89 |
| L-42 | Fixed | #89 |
| L-43 | Fixed | #105 |
| L-44 | Fixed | #105, #133 |
| L-45 | Fixed | #101 / #125 |
| L-46 | Fixed | #104 |
| L-47 | Fixed | #106 |
| L-48 | Fixed | #106 |
| L-49 | Fixed | #132 |
| L-50 | Fixed | #110, #WAVE4 |
| L-51 | Fixed | #110 |
| L-52 | Fixed | #110 |
| L-53 | Fixed | #119 |
| L-54 | Fixed | #100 |
| L-55 | Fixed | #100 |
| L-56 | Fixed | #100 |
| L-57 | Fixed | #100 |
| L-58 | Fixed | #100 |
| L-59 | Fixed | #130 |
| L-60 | Fixed | #100 |
| L-61 | Fixed | #100 |
| UI-1 | Fixed | #87 |
| UI-2 | Fixed | #87 |
| UI-3 | Fixed | #120 |
| UI-4 | Fixed | #87 |
| UI-5 | Fixed | #87 |
| UI-6 | Fixed | #120 |
| UI-7 | Fixed | #120 |
| UI-8 | Fixed | #120 |
| UI-9 | Fixed | #120 |
| UI-10 | Fixed | #120 |
| UI-11 | Fixed | #120 |
| UI-12 | Open | needs a niri session |
| UI-13 | Open | needs a niri session |
| UI-14 | Fixed | #120 |
| UI-15 | Fixed | #120 |
| UI-16 | Fixed | #120 |
| UI-17 | Open | needs a niri session |
| UI-18 | Fixed | #120 |
| G-1 | Fixed | #83 |
| G-2 | Fixed | #131 |
| G-3 | Fixed | #131 |
| G-4 | Fixed | #131 |
| G-5 | Fixed | #131, #WAVE4 |
| G-6 | Fixed | #83 |
| G-7 | Fixed | #131 |
| G-8 | Fixed | #131, #WAVE4 |
| G-9 | Fixed | #131 |
| G-10 | Fixed | #131, #WAVE4 |
| G-11 | Fixed | #131 |
| G-12 | Fixed | #131 |
| G-13 | Fixed | #131 |
| G-14 | Fixed | #131 |
| G-15 | Fixed | #131 |
| G-16 | Fixed | #131 |
| G-17 | Fixed | #131 |
| G-18 | Fixed | #131 |
| G-19 | Fixed | #131 |
