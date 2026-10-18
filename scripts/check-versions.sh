#!/usr/bin/env bash
# Check that every place that carries the application version agrees (G-5).
#
#   scripts/check-versions.sh              all locations must agree with each other
#   scripts/check-versions.sh <version>    all locations must equal <version> (a tag's v prefix is stripped)
#
# Locations: Cargo.toml [workspace.package], the shell crate, tauri.conf.json, the
# frontend package.json and package-lock.json (two entries), the Chrome extension
# manifest.json, the xbps template, the AppStream metainfo and the release URL in
# README.md. The yt-dlp plugin sidecar has its own version (see
# packages/extractor-plugins/plugins-manifest.json).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
want=${1:-}
want=${want#v}

json() { # file, python expression over the parsed document `d`
	python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(eval(sys.argv[2]))' "$1" "$2"
}
# First `version = "..."` in [package] of a manifest, resolved through `version.workspace = true`.
crate_version() {
	local f=$1 v
	if grep -Eq '^version\.workspace[[:space:]]*=[[:space:]]*true' "$f"; then
		echo "workspace"
		return
	fi
	v=$(awk -F'"' '/^\[package\]/{f=1; next} /^\[/{f=0} f&&/^version[[:space:]]*=/{print $2; exit}' "$f")
	echo "$v"
}

names=() values=()
add() {
	names+=("$1")
	values+=("$2")
}

add "Cargo.toml [workspace.package]" "$(awk -F'"' '/^\[workspace.package\]/{f=1; next} /^\[/{f=0} f&&/^version[[:space:]]*=/{print $2; exit}' Cargo.toml)"
shell_v=$(crate_version packages/tauri-shell/src-tauri/Cargo.toml)
# A shell crate that inherits the workspace version cannot drift.
[ "$shell_v" = workspace ] || add "packages/tauri-shell/src-tauri/Cargo.toml" "$shell_v"
add "packages/tauri-shell/src-tauri/tauri.conf.json" "$(json packages/tauri-shell/src-tauri/tauri.conf.json "d['version']")"
add "packages/tauri-shell/frontend/package.json" "$(json packages/tauri-shell/frontend/package.json "d['version']")"
add "packages/tauri-shell/frontend/package-lock.json (root)" "$(json packages/tauri-shell/frontend/package-lock.json "d['version']")"
add "packages/tauri-shell/frontend/package-lock.json (packages[''])" "$(json packages/tauri-shell/frontend/package-lock.json "d['packages']['']['version']")"
add "packages/chrome-companion/manifest.json" "$(json packages/chrome-companion/manifest.json "d['version']")"
add "packaging/void/srcpkgs/phlegethon/template" "$(sed -n 's/^version=//p' packaging/void/srcpkgs/phlegethon/template)"
add "packaging/linux/com.phlegethon.app.metainfo.xml (newest release)" "$(sed -n 's/^[[:space:]]*<release version="\([^"]*\)".*/\1/p' packaging/linux/com.phlegethon.app.metainfo.xml | head -1)"
readme=$(sed -n 's#.*/releases/download/v\([0-9][0-9.]*\)/phlegethon-.*#\1#p' README.md | sort -u | paste -sd,)
add "README.md (release URL)" "$readme"

[ -n "$want" ] || want=${values[0]}
bad=0
for i in "${!names[@]}"; do
	if [ "${values[$i]}" = "$want" ]; then
		printf 'ok       %-58s %s\n' "${names[$i]}" "${values[$i]}"
	else
		printf 'MISMATCH %-58s %s (want %s)\n' "${names[$i]}" "${values[$i]:-<missing>}" "$want"
		bad=1
	fi
done
if [ "$bad" -ne 0 ]; then
	echo "version locations disagree." >&2
	exit 1
fi
