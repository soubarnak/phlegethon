# Code Review — `claude/code-review-7h6bro`

**Reviewed range:** 7 commits since PR #40 (`65a236d..882d514`)
**Date:** 2026-07-21

## Scope

| Area | Files |
| --- | --- |
| Core (Rust) | `download-core/src/{lib,job,extractor,ytdlp_extractor,cabi}.rs` |
| Tauri shell | `tauri-shell/src-tauri/src/lib.rs`, `tauri-shell/frontend/{app.js,index.html,styles.css}` |
| WinUI shell | `winui-shell/Phlegethon/**` |
| Tests | `download-core/tests/{core_api,ytdlp_extractor_integration}.rs` |

Themes of the change: absolute **Save to… / Job Destination overrides**, **Library Root** settings + persistence, an **interruptible Extractor** (Pause/Discard hard-kills yt-dlp), **silent-progress** reporting from on-disk growth, and **filesize-based totals** for determinate progress.

**Build:** `cargo check -p download-core` passes (only pre-existing dead-code warnings).

**Overall:** the concurrency design is solid — draining stdout/stderr on separate threads before waiting avoids the classic pipe-buffer deadlock, routing both Pause and Discard through one `stop` flag is clean, and the stale-preview request-ID guard in the frontend is a nice touch. There is **one serious data-loss issue** that should block shipping, plus minor items.

---

## 🔴 High — Discarding a single-item "Save to…" job deletes the entire chosen folder

**Files:** `download-core/src/lib.rs:452`, `:321`, `:830`, `:1314`, `:1287`; `download-core/src/job.rs` (`new_at_destination`)

When a **single** item receives an absolute override, the job's destination becomes that folder **verbatim**, with no unique subfolder:

- `confirm_probe_selection` single-item override → `Job::new_at_destination(url, destination)` — `lib.rs:830`
- direct `enqueue(url, Some(dest))` → `Job::new_at_destination` — `lib.rs:321`

The pre-existing `finalize_discard` then runs:

```rust
// lib.rs:452
std::fs::remove_dir_all(destination)   // destination == the user's picked folder
```

### Reproduction (user-reachable in the Tauri shell)
1. Paste a Launch-Site URL.
2. Click **Save to… → `~/Videos`** (any existing folder).
3. Click the **✕ Discard** button that every queue row renders (`frontend/app.js:547`).

**Result:** `remove_dir_all("~/Videos")` — the whole folder and all unrelated contents are deleted.

Before this branch, every destination was a unique `jobs/<id>` subfolder, so `remove_dir_all` only ever touched files the job owned. `new_at_destination` breaks that invariant. This also **violates the `Discard` spec** in `CONTEXT.md`, which scopes deletion to *"that job's files under its Job Destination (including partials)"* — not a user folder it shares.

### Two more symptoms, same root cause (job treats a folder it doesn't own as job-owned)
- **`cleanup_extractor_temp_artifacts`** (`lib.rs:1314`) deletes any `.part` / `.ytdl` / `.temp` / `.tmp` file in the folder on completion — so a single-item Save-to `~/Downloads` can silently delete a **browser's** in-progress `*.part`.
- **`destination_byte_total`** (`lib.rs:1287`) sums **all** files under the folder, so progress/total for a populated override folder is inflated by pre-existing content, and the final completion byte count is wrong.

### Coverage gap
The override tests (`core_api.rs:4304`, `:4386`) use fresh, non-existent temp paths, so none of these three symptoms is exercised.

### Suggested fix
Never hand a job a caller-owned folder as its own destination. Options:
- Even for a single-item override, write into a unique subfolder under the picked folder (as multi-item already does via `new_with_source_at`), **or**
- Track the specific file(s) each job creates and, on discard/cleanup, delete only those — never `remove_dir_all` the directory or suffix-sweep it.

Whichever is chosen, `destination_byte_total` should then count only the job's own files. Add a Discard confirmation dialog as defense-in-depth.

---

## 🟡 Low — Determinate progress can exceed 100%

**File:** `tauri-shell/frontend/app.js:542`

`total_bytes` is set from the selected quality's single-format `filesize` / `filesize_approx` (`Quality.size_bytes`). When yt-dlp merges separate video+audio streams, on-disk bytes exceed that single-format size. The progress **bar** is clamped (`Math.min(pct, 100)`), but the row-meta text is not:

```js
// app.js:542
${hasTotal ? ` (${pct}%)` : ""}   // can render "(137%)"
```

Cosmetic. Clamp the percentage, or treat the format size as a floor rather than a hard total.

---

## Minor / non-blocking

- **`EventChannelService.ConfirmProbeSelection` (C#)** — the inner `try` block is over-indented (`Services/EventChannelService.cs`). Harmless, just untidy.
- **`preview_probe_destination` has no C-ABI export.** The WinUI shell therefore can't show the Core-resolved preview path the way the Tauri frontend does; it displays the raw picked path instead. Fine if intentional — noting the asymmetry.
- **Poll-loop dependency on `stop` honoring.** The `process_pending` disk-poll loop (`lib.rs`) only exits when the extractor thread returns; it relies entirely on the toolchain honoring `stop`. `YtDlpExtractorToolchain` and the fake both do, so this is safe today — worth a doc note on the trait so future implementations don't stall a Pause/Discard.

---

## What's good

- **`run_command_interruptible`** — spawns dedicated reader threads for stdout/stderr before the wait loop, so a chatty yt-dlp can't deadlock on a full pipe; hard-kills promptly on `stop` and is unit-tested with a `sleep 30` child (`ytdlp_extractor.rs`).
- **Unified stop signal** — Pause and Discard share `pause_requested`, and `finalize_stop_after_transfer` cleanly forks on `is_discard_requested()`. Kill/stop races that surface as toolchain errors are correctly re-routed to the finalize path when the user already requested stop.
- **Silent-progress polling** — emitting `JobProgress` from on-disk growth for otherwise-silent extractors is a good UX call, and it only emits on change (`last_emitted`).
- **Frontend stale-response guard** — `probePreviewRequestId` correctly discards out-of-order `preview_probe_destination` results.
- **Filesize plumbing** — `usable_format_size` (prefer exact, fall back to positive approx, drop zero) and the duplicate-label size upgrade are well-tested.

---

## Recommendation

Fix the destination-ownership issue (High) before this ships — accidental deletion of a user's folder is the worst possible outcome for a download manager, and it's reachable in two clicks. The Low/Minor items can follow.
