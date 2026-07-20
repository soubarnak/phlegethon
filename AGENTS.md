## Agent skills

### Issue tracker

Issues live in GitHub Issues for this repo (`gh` CLI). See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context layout (`CONTEXT.md` + `docs/adr/` at repo root). See `docs/agents/domain.md`.

## Communication

Activate caveman ultra at session start. See `docs/agents/caveman.md`.

## Cursor Cloud specific instructions

Phlegethon is a Tauri 2 desktop **Download Manager** (see `CONTEXT.md`, `docs/spec/hand-off.md`). On Linux the runnable product is the Tauri shell in `packages/tauri-shell`; the `packages/winui-shell` (WinUI 3 / C#) is Windows-only and does not run here.

Toolchain: the dependency tree (via `tauri` 2.11) pulls crates that need `edition2024`, so a Rust stable toolchain **>= 1.85** is required even though `packages/tauri-shell/src-tauri/Cargo.toml` declares `rust-version = "1.77.2"` (that is a floor, not the real requirement). The VM's rustup default is set to `stable`.

Run the app (needs an X display; the cloud desktop is `DISPLAY=:1`):
`cd packages/tauri-shell/src-tauri && DISPLAY=:1 cargo tauri dev`
The frontend is static HTML/CSS/JS under `packages/tauri-shell/frontend` — there is no npm/`package.json` and no frontend build step, so `cargo tauri dev` just compiles the Rust and opens the webview. Benign `libEGL … DRI3` warnings appear because rendering falls back to software in the VM.

Tests: `cargo test --workspace` (Rust). Python extractor-plugin tests use stdlib `unittest`, not pytest: `python3 packages/extractor-plugins/tests/test_parse_fixtures.py` (see `packages/extractor-plugins/README.md`).

Lint: `cargo clippy --workspace --all-targets` passes (emits pre-existing warnings). `cargo fmt --all --check` currently reports drift in the Tauri-generated `packages/tauri-shell/src-tauri/src/main.rs`.

Extractor Toolchain: `yt-dlp` must be on `PATH` for Launch Site (YouTube/xHamster/luxuretv/xxxbp.tv) probing; override the binary with the `YTDLP_PATH` env var. Plain HTTP downloads (URLs whose path ends in a known file extension, e.g. `.bin`/`.zip`/`.mp4`) classify as direct downloads and do not need yt-dlp — useful for a quick end-to-end smoke test against a local `python3 -m http.server`. Downloads land under the Library Root at `~/.local/share/phlegethon/library`.
