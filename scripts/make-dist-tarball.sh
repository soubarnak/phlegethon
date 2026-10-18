#!/usr/bin/env bash
# Create phlegethon-<version>.tar.gz for xbps-src: the git-tracked sources as they are in the
# working tree (untracked and ignored files are left out) plus the prebuilt frontend/dist,
# so the package build needs no npm.
#
#   scripts/make-dist-tarball.sh [--out DIR] [--vendor] [--update-template]
#
#   --out DIR           where to write the tarball (default: ./dist)
#   --vendor            also vendor all crates (offline xbps-src builds; adds ~100 MB)
#   --update-template   write the sha256 into packaging/void/srcpkgs/phlegethon/template
#
# Fails when the version differs between Cargo.toml, the xbps template, tauri.conf.json,
# package.json, package-lock.json, the extension manifest and the AppStream metainfo (scripts/check-versions.sh).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
out="$root/dist" vendor=0 update_template=0
while [ $# -gt 0 ]; do
	case "$1" in
	--out)
		out=$2
		shift 2
		;;
	--vendor)
		vendor=1
		shift
		;;
	--update-template)
		update_template=1
		shift
		;;
	-h | --help)
		sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
		exit 0
		;;
	*)
		echo "unknown option: $1" >&2
		exit 2
		;;
	esac
done

cd "$root"
version=$(awk -F'"' '/^\[workspace.package\]/{f=1} f&&/^version/{print $2; exit}' Cargo.toml)
template="$root/packaging/void/srcpkgs/phlegethon/template"
"$root/scripts/check-versions.sh" "$version" >&2
[ -f packages/tauri-shell/frontend/dist/index.html ] || {
	echo "frontend/dist is missing: run 'npm run build' in packages/tauri-shell/frontend first (make frontend)." >&2
	exit 1
}

name="phlegethon-$version"
stage=$(mktemp -d)
trap 'rm -rf "${stage:?}"' EXIT
mkdir -p "$stage/$name"

# Tracked files only, with the working-tree contents. Untracked files (local notes,
# tool output such as graphify-out/) must not end up in a release.
if ! git diff --quiet HEAD -- 2>/dev/null; then
	echo "warning: tracked files have uncommitted changes; the tarball contains them." >&2
fi
git ls-files -z | tar --null -T - --ignore-failed-read -cf - | tar -xf - -C "$stage/$name"
mkdir -p "$stage/$name/packages/tauri-shell/frontend"
cp -r packages/tauri-shell/frontend/dist "$stage/$name/packages/tauri-shell/frontend/dist"

if [ "$vendor" -eq 1 ]; then
	(
		cd "$stage/$name"
		mkdir -p .cargo
		cargo vendor --locked vendor >.cargo/config.toml
	)
fi

mkdir -p "$out"
tarball="$out/$name.tar.gz"
# Reproducible: sorted names, fixed owner, mtime from the last commit.
mtime=$(git log -1 --format=%ct 2>/dev/null || date +%s)
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$mtime" \
	-C "$stage" -cf - "$name" | gzip -n -9 >"$tarball"

sum=$(sha256sum "$tarball" | awk '{print $1}')
echo "Wrote $tarball"
echo "sha256 $sum"
if [ "$update_template" -eq 1 ]; then
	sed -i "s/^checksum=.*/checksum=$sum/" "$template"
	echo "Updated checksum in $template"
else
	echo "Put this in the template:  checksum=$sum"
fi
