#!/usr/bin/env bash
# Build the xbps package with xbps-src from a void-packages checkout.
#
#   scripts/xbps-build.sh [--void-packages DIR] [--vendor]
#
# Needs a Void Linux host, git, and a non-root user (xbps-src refuses root).
# The checkout defaults to $VOID_PACKAGES or ./.void-packages and is cloned if absent,
# at the commit pinned in packaging/void/images.env (override with VOID_PACKAGES_REF).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
vp=${VOID_PACKAGES:-$root/.void-packages} vendor=()
while [ $# -gt 0 ]; do
	case "$1" in
	--void-packages)
		vp=$2
		shift 2
		;;
	--vendor)
		vendor=(--vendor)
		shift
		;;
	-h | --help)
		sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'
		exit 0
		;;
	*)
		echo "unknown option: $1" >&2
		exit 2
		;;
	esac
done

[ "$(id -u)" -ne 0 ] || {
	echo "run as a normal user: xbps-src refuses to build as root." >&2
	exit 1
}

# 1. Source tarball with the checksum written into the template.
"$root/scripts/make-dist-tarball.sh" --update-template "${vendor[@]}"
version=$(sed -n 's/^version=//p' "$root/packaging/void/srcpkgs/phlegethon/template")
tarball="$root/dist/phlegethon-$version.tar.gz"

# 2. void-packages checkout.
if [ ! -d "$vp/.git" ]; then
	# shellcheck source=../packaging/void/images.env
	ref=${VOID_PACKAGES_REF:-}
	[ -n "$ref" ] || {
		. "$root/packaging/void/images.env"
		ref=$VOID_PACKAGES_REF
	}
	git init -q "$vp"
	git -C "$vp" fetch -q --depth 1 https://github.com/void-linux/void-packages "$ref"
	git -C "$vp" checkout -q FETCH_HEAD
fi
cd "$vp"
[ -d masterdir ] || ./xbps-src binary-bootstrap

# 3. Template, and the tarball in the source cache so no download is attempted.
rm -rf "${vp:?}/srcpkgs/phlegethon"
cp -r "$root/packaging/void/srcpkgs/phlegethon" srcpkgs/phlegethon
mkdir -p "hostdir/sources/phlegethon-$version"
cp "$tarball" "hostdir/sources/phlegethon-$version/"

./xbps-src -f pkg phlegethon

echo
echo "Done. Install with:"
echo "  sudo xbps-install --repository=$vp/hostdir/binpkgs phlegethon"
