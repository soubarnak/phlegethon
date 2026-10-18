#!/usr/bin/env bash
# Regenerate the TypeScript types for everything that crosses the IPC boundary.
#
# The Rust serde types are the source of truth (ts-rs, behind the `ts` cargo feature). Output goes to
# packages/tauri-shell/frontend/src/tauri/generated/ (see .cargo/config.toml). CI runs this script
# and fails when the committed files differ. Needs the same system libraries as `make test-shell`.
set -euo pipefail
cd "$(dirname "$0")/.."

out=packages/tauri-shell/frontend/src/tauri/generated
rm -rf "$out"
mkdir -p "$out"

cargo test --locked \
  -p download-core -p companion-protocol -p phlegethon-tauri \
  --features download-core/ts,companion-protocol/ts,phlegethon-tauri/ts \
  -- export_bindings

ls "$out" > /dev/null
