#!/usr/bin/env bash
# Tests for the shell scripts and Makefile targets that release builds rely on.
#
#   scripts/test-scripts.sh
#
# Covers check-versions.sh (G-5), install-nmh-linux.sh (G-14, G-15), make desktop (G-3),
# check-embedded-frontend.sh (G-7) and make-dist-tarball.sh (G-4; needs a built
# packages/tauri-shell/frontend/dist).
set -uo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root" || exit 1
tmp=$(mktemp -d)
trap 'rm -rf "${tmp:?}"' EXIT
fails=0

pass() { echo "ok   $1"; }
fail() {
	echo "FAIL $1" >&2
	fails=$((fails + 1))
}
check() { # name, then a command that must succeed
	local name=$1
	shift
	if "$@" >"$tmp/out" 2>&1; then pass "$name"; else
		fail "$name"
		sed 's/^/     /' "$tmp/out" >&2
	fi
}
check_fails() { # name, then a command that must fail
	local name=$1
	shift
	if "$@" >"$tmp/out" 2>&1; then
		fail "$name (expected a non-zero exit)"
		sed 's/^/     /' "$tmp/out" >&2
	else pass "$name"; fi
}

# --- check-versions.sh -------------------------------------------------------------
check "check-versions: the tree agrees" scripts/check-versions.sh
version=$(awk -F'"' '/^\[workspace.package\]/{f=1} f&&/^version/{print $2; exit}' Cargo.toml)
check "check-versions: accepts phlegethon-v<version>" scripts/check-versions.sh "phlegethon-v$version"
check_fails "check-versions: rejects another version" scripts/check-versions.sh 9.9.9
check_fails "check-versions: rejects phlegethon-v9.9.9" scripts/check-versions.sh phlegethon-v9.9.9

# A copy of the tree with one location changed must be rejected.
copy="$tmp/tree"
mkdir -p "$copy/scripts" "$copy/packages/tauri-shell/src-tauri" "$copy/packages/tauri-shell/frontend" \
	"$copy/packages/chrome-companion" "$copy/packaging/void/srcpkgs/phlegethon" "$copy/packaging/linux"
cp scripts/check-versions.sh "$copy/scripts/"
cp Cargo.toml "$copy/"
cp packages/tauri-shell/src-tauri/Cargo.toml packages/tauri-shell/src-tauri/tauri.conf.json "$copy/packages/tauri-shell/src-tauri/"
cp packages/tauri-shell/frontend/package.json packages/tauri-shell/frontend/package-lock.json "$copy/packages/tauri-shell/frontend/"
cp packages/chrome-companion/manifest.json "$copy/packages/chrome-companion/"
cp packaging/void/srcpkgs/phlegethon/template "$copy/packaging/void/srcpkgs/phlegethon/"
cp packaging/linux/com.phlegethon.app.metainfo.xml "$copy/packaging/linux/"
check "check-versions: copy of the tree agrees" "$copy/scripts/check-versions.sh"
sed -i 's/"version": "[^"]*"/"version": "7.7.7"/' "$copy/packages/chrome-companion/manifest.json"
check_fails "check-versions: a changed extension manifest is caught" "$copy/scripts/check-versions.sh"
cp packages/chrome-companion/manifest.json "$copy/packages/chrome-companion/"

sed -i 's/<release version="[^"]*"/<release version="7.7.7"/' "$copy/packaging/linux/com.phlegethon.app.metainfo.xml"
check_fails "check-versions: a stale metainfo release is caught" "$copy/scripts/check-versions.sh"

# --- sign-repo.sh ------------------------------------------------------------------
check "sign-repo: parses" sh -n packaging/void/sign-repo.sh
check_fails "sign-repo: refuses an empty signing key" sh packaging/void/sign-repo.sh "$tmp" /dev/null

# --- install-nmh-linux.sh ----------------------------------------------------------
home="$tmp/home"
mkdir -p "$home" "$tmp/bin"
good_id=abcdefghijklmnopabcdefghijklmnop
host="$tmp/bin/phlegethon-nmh"
printf '#!/bin/sh\n' >"$host"
chmod +x "$host"
nmh() { HOME=$home scripts/install-nmh-linux.sh "$@"; }
check_fails "install-nmh: --extension-id without a value" nmh --extension-id
check_fails "install-nmh: --host-path without a value" nmh --extension-id "$good_id" --host-path
check_fails "install-nmh: --browsers without a value" nmh --extension-id "$good_id" --browsers
check "install-nmh: writes the manifest" nmh --extension-id "$good_id" --host-path "$host" --browsers chrome
manifest="$home/.config/google-chrome/NativeMessagingHosts/com.phlegethon.bridge.json"
check "install-nmh: manifest is valid JSON with the host path" python3 -c '
import json,sys
d=json.load(open(sys.argv[1]))
assert d["path"]==sys.argv[2], d
assert d["allowed_origins"]==["chrome-extension://"+sys.argv[3]+"/"], d
' "$manifest" "$host" "$good_id"
# G-15: the checked-in template has the same keys and values as what the script writes,
# apart from the two placeholders.
check "install-nmh: template matches the script output" python3 -c '
import json,sys
want=json.load(open("packages/native-messaging-host/manifests/com.phlegethon.bridge.json"))
got=json.load(open(sys.argv[1]))
assert want.keys()==got.keys(), (want.keys(), got.keys())
for k in want:
    if k in ("path","allowed_origins"):
        continue
    assert want[k]==got[k], (k, want[k], got[k])
assert want["path"].startswith("REPLACE_WITH_") and len(want["allowed_origins"])==1
' "$manifest"
# G-14: a path with a quote and a backslash must still give valid JSON.
odd="$tmp/bin/odd\"dir\\x"
mkdir -p "$odd"
printf '#!/bin/sh\n' >"$odd/phlegethon-nmh"
chmod +x "$odd/phlegethon-nmh"
check "install-nmh: odd host path is escaped" nmh --extension-id "$good_id" --host-path "$odd/phlegethon-nmh" --browsers chromium
check "install-nmh: escaped manifest round-trips" python3 -c '
import json,sys
assert json.load(open(sys.argv[1]))["path"]==sys.argv[2]
' "$home/.config/chromium/NativeMessagingHosts/com.phlegethon.bridge.json" "$odd/phlegethon-nmh"

# --- help output (G-13) --------------------------------------------------------------
for s in setup-void install-nmh-linux make-dist-tarball xbps-build; do
	if scripts/$s.sh --help 2>&1 | grep -q 'set -euo'; then fail "$s --help prints its code"; else pass "$s --help is clean"; fi
done
check "setup-void --help documents --force" bash -c 'scripts/setup-void.sh --help | grep -q -- --force'

# --- check-embedded-frontend.sh (G-7) -----------------------------------------------------
mkdir -p "$tmp/fe"
printf '<script src="/assets/index-AbC123.js"></script><link href="/assets/index-XyZ.css">\n' >"$tmp/fe/index.html"
printf 'junk\0/assets/index-AbC123.js\0/assets/index-XyZ.css\0junk' >"$tmp/fe/with-frontend"
printf 'junk\0http://localhost:1420\0junk' >"$tmp/fe/dev-build"
check "check-embedded-frontend: accepts an embedding binary" scripts/check-embedded-frontend.sh "$tmp/fe/with-frontend" "$tmp/fe/index.html"
check_fails "check-embedded-frontend: rejects a dev build" scripts/check-embedded-frontend.sh "$tmp/fe/dev-build" "$tmp/fe/index.html"

# --- make desktop (G-3) ----------------------------------------------------------------
fake="$tmp/fakebin"
mkdir -p "$fake"
printf '#!/bin/sh\necho "fake validator failing" >&2\nexit 1\n' >"$fake/desktop-file-validate"
chmod +x "$fake/desktop-file-validate"
check_fails "make desktop: fails when desktop-file-validate fails" env PATH="$fake:$PATH" make desktop

# --- make-dist-tarball.sh (G-4) ----------------------------------------------------------
if [ -f packages/tauri-shell/frontend/dist/index.html ]; then
	untracked="$root/untracked-dist-test-file.txt"
	echo local >"$untracked"
	scripts/make-dist-tarball.sh --out "$tmp/dist" >"$tmp/out" 2>&1 || { fail "make-dist-tarball runs"; cat "$tmp/out" >&2; }
	rm -f "$untracked"
	tar -tzf "$tmp/dist/phlegethon-$version.tar.gz" >"$tmp/listing"
	if grep -q untracked-dist-test-file "$tmp/listing"; then
		fail "make-dist-tarball: untracked file leaked into the tarball"
	else pass "make-dist-tarball: untracked files are left out"; fi
	if grep -q 'frontend/dist/index.html' "$tmp/listing"; then
		pass "make-dist-tarball: frontend/dist is included"
	else fail "make-dist-tarball: frontend/dist missing"; fi
else
	echo "skip make-dist-tarball (build the frontend first)"
fi

# --- third-party notices ship with the package -------------------------------------------
if grep -q 'THIRD_PARTY_NOTICES.md' Makefile && grep -q 'vlicense THIRD_PARTY_NOTICES.md' packaging/void/srcpkgs/phlegethon/template; then
	pass "THIRD_PARTY_NOTICES.md is installed by make install and the xbps template"
else fail "THIRD_PARTY_NOTICES.md is not installed by make install or the xbps template"; fi
if grep -q 'SIL OPEN FONT LICENSE Version 1.1' THIRD_PARTY_NOTICES.md; then
	pass "THIRD_PARTY_NOTICES.md carries the OFL text"
else fail "THIRD_PARTY_NOTICES.md is missing the OFL text"; fi

[ "$fails" -eq 0 ] || {
	echo "$fails script test(s) failed" >&2
	exit 1
}
echo "all script tests passed"
