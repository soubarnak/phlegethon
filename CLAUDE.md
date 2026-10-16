# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Phlegethon is a download manager for Void Linux: a Rust workspace with a Tauri 2 shell, a React/Carbon frontend, yt-dlp extractor plugins, and a Chrome companion extension. Read `AGENTS.md` first (issue tracker is GitHub Issues via `gh`; triage labels; domain docs). Read `CONTEXT.md` (domain glossary with `_Avoid_` synonyms) and `docs/adr/` before changing behavior, and use the glossary terms in code and issues. `docs/bugs.md` is the audit report: each finding (B-, H-, M-, L-, UI-, G-) has Location, Problem, Failure and Fix, and section 8 maps the archived `docs/archive/CODE_REVIEW.md` to the code. CI runs on every pull request and push to `main` (`.github/workflows/ci.yml`); releases are built by `release.yml`.

## Commands

Top-level `Makefile` drives everything:

- `make` / `make build`: runs `scripts/build-linux.sh` (`npm ci`, `npm run build`, then `cargo build --release --locked -p phlegethon-tauri -p phlegethon-nmh --features phlegethon-tauri/custom-protocol`). `SKIP_FRONTEND=1` reuses an existing `dist`.
- `make test`: Rust (`cargo test --workspace --exclude phlegethon-tauri --locked`), the two Python plugin tests, the extension test, `scripts/test-scripts.sh` (version check, NMH installer, `make desktop`, tarball contents) and the frontend (`npm run typecheck && npm test`).
- `make test-shell`: `cargo test -p phlegethon-tauri`. Needs webkit2gtk 4.1, gtk3, and libsoup3 dev packages, which is why the shell crate is excluded from `make test` and CI.
- `make dist`, `make xbps`, `make xbps-container` (xbps package in the pinned Void container; `CONTAINER_ENGINE` defaults to `podman`, `LIBC=musl` for musl), `make check-versions`, `make install PREFIX=/usr`, `make probe-check` (live URLs, needs network), `make e2e-ui` (WebDriver under xvfb), `make nmh-install EXT=<id>`.

Frontend (`packages/tauri-shell/frontend`): `npm run dev` (Vite, strict port 1420, HMR on 1421), `npm run typecheck`, `npm test` (`vitest run`).

Single tests:

- Rust: `cargo test -p download-core --test core_api <name>` or `cargo test -p download-core <name>`.
- Frontend: `npx vitest run src/lib/errors.test.ts` or `npx vitest run -t "<name>"`.
- Python plugin tests are plain `unittest` scripts: `python3 packages/extractor-plugins/tests/test_parse_fixtures.py`. They use offline fixtures in `packages/extractor-plugins/fixtures/`; CI has no live network.

## Architecture

Per ADR 0001, the Tauri shell is the only Platform Shell and Void xbps is the only release target. Tauri's bundler is disabled.

- **`packages/download-core`**: plain Rust library (`rlib`, serde types) holding the queue and all download logic. `lib.rs` is the public API and queue. Transfer lives in `http.rs`/`transfer.rs` (resume, segmented). `ytdlp_extractor.rs` wraps yt-dlp. `plugin_sidecar.rs` and `sidecar_updates.rs` install and update extractor plugins. `download_archive.rs`, `filename_template.rs`, `quality_policy.rs`, `mirror_set.rs`, and `integrity.rs` implement the glossary concepts. No database: state is in memory plus files.
- **`packages/tauri-shell/src-tauri`** (crate `phlegethon-tauri`, lib `app_lib`): IPC commands and state in `lib.rs`; `companion.rs` listens for URLs from the browser companion and enqueues them. Without the `custom-protocol` feature the window loads `http://localhost:1420` instead of the embedded frontend.
- **`packages/tauri-shell/frontend`**: React 19 + `@carbon/react` (ADR 0002). All IPC goes through `src/tauri/api.ts`; components never call `invoke` directly. Core events arrive as `core-event`. Pure logic and its tests live in `src/lib/`; hooks in `src/state/`. CSP is strict (`default-src 'self'`, `connect-src` is `ipc:` and `http://ipc.localhost`; see `tauri.conf.json`), so fonts and assets must be bundled, not loaded from a CDN.
- **`packages/shell-strings/en.json`**: single user-facing string table. A test fails when a `t("key")` in source is missing from it.
- **`packages/shell-support`**: folder picker (portal/zenity/kdialog), notifications, native-messaging install. No Tauri dependency.
- **Browser companion path**: `packages/chrome-companion` (MV3) talks over stdio to `phlegethon-nmh` (`packages/native-messaging-host`), which forwards over a named local socket (`packages/companion-protocol/src/ipc.rs`) to the shell's `companion.rs`. The native-messaging manifest template (`packages/native-messaging-host/manifests/com.phlegethon.bridge.json`) has `REPLACE_WITH_...` placeholders for manual installs only. `scripts/install-nmh-linux.sh --extension-id <id>` does not read it; it writes its own JSON, and `scripts/test-scripts.sh` checks that the two have the same fields.
- **`packages/extractor-plugins`**: yt-dlp plugins installed as an app-managed sidecar. `plugins-manifest.json` version and extractor keys must match `packages/download-core/src/plugin_sidecar.rs`.
- `prototypes/` is static mockup HTML/JS, not part of the build. Do not edit it as product code.

## Gotchas

- **Version lives in seven places** (the shell crate inherits the workspace version): `Cargo.toml` `[workspace.package]`, `tauri.conf.json`, `frontend/package.json` and `package-lock.json` (two entries), `packages/chrome-companion/manifest.json`, `version=` in `packaging/void/srcpkgs/phlegethon/template`, and the newest `<release>` in `packaging/linux/com.phlegethon.app.metainfo.xml`. The README release URL hard-codes it too. `scripts/check-versions.sh [version]` checks all of them; the release workflow, `scripts/make-dist-tarball.sh`, CI and `make test` run it. The yt-dlp plugin sidecar has its own version (`plugins-manifest.json`, `pyproject.toml`, `plugin_sidecar.rs`).
- **Frontend lockfile**: `package-lock.json` must carry the same `license` as `package.json` (MIT). Use `npm ci`, not `npm install`.
- **`Cargo.lock` is committed** and builds use `--locked`.
- **xbps template**: `checksum=` stays all zeros until `make dist` / `xbps-build.sh --update-template` writes the real sha256. The WebKit build dependency is `libwebkit2gtk41-devel`.
- **Release**: `.github/workflows/release.yml` publishes on a `v*` tag or a manual run with a `version` input (which tags the built commit). A manual run for a version whose tag already exists fails in the preflight job. A manual run with an empty `version` builds without publishing. Actions are pinned by SHA (Dependabot bumps them); the Void images and the void-packages commit are pinned in `packaging/void/images.env`.
- **Discard semantics** (see `CONTEXT.md`): a job must never treat a shared user folder as its own Job Destination. `finalize_discard` in `packages/download-core/src/lib.rs` calls `remove_dir_all` on the destination of non-Completed jobs, so changes to destination handling must preserve that invariant.
- `release.yml` runs the container build with `CONTAINER_ENGINE=docker` on GitHub runners. Locally use `make xbps-container`, which defaults to `podman`; do not run docker.
- The container build and `scripts/build-linux.sh` check that the binary embeds the frontend (`scripts/check-embedded-frontend.sh`). A shell built without `--features phlegethon-tauri/custom-protocol` opens a blank window.
