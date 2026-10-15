# ADR 0001: Drop the Windows shell; Void Linux xbps is the only release target

- Status: accepted
- Date: 2026-10-14
- Supersedes: `docs/spec/hand-off.md` §3.1, §4.2, §4.3, §9.2 (Windows 11 / WinUI 3 / C ABI / installers)

## Context

The hand-off spec targeted Windows 11 (WinUI 3, C# shell over a Rust `cdylib` C ABI
generated with csbindgen) and Debian (Tauri 2). The product owner now runs Void Linux
exclusively and asked for the Windows UI to be dropped and for the application to be
released only as an xbps package.

Keeping the C ABI alive without a consumer would have doubled the cost of every Core
API change (every new field needed a manual `Serialize` impl and a C export) for a
shell nobody builds.

## Decision

1. `packages/winui-shell`, `scripts/build-core.ps1`, `design/geist/GeistTheme.xaml` and
   the WinUI research notes are deleted.
2. `download-core` is a plain Rust library crate (`rlib`). `src/cabi.rs`, `build.rs`
   (csbindgen) and the `cdylib` crate type are removed. Serialisable Core types derive
   `serde::Serialize` directly.
3. The Tauri 2 shell (`packages/tauri-shell`) is the single Platform Shell. It links
   `download-core` as a Rust dependency.
4. The release artifact is an xbps package built from `packaging/void/srcpkgs/phlegethon/template`.
   Tauri's bundler (deb / AppImage / rpm) is disabled.
5. Debian is no longer a named target. Building from source on other Linux distributions
   is expected to keep working (the code has no Void-specific assumptions) but is not
   packaged or tested.

## Consequences

- Positive: one shell, one design system, no FFI maintenance, Core API changes are
  additive serde fields.
- Negative: Windows users have no supported build. Re-adding Windows would mean a
  Tauri Windows build (WebView2), not a return of WinUI.
