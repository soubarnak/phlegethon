#!/bin/sh
# Build the phlegethon xbps package with xbps-src inside a Void Linux container.
# Used by the release workflow and for local builds. From a checkout, `make dist` and then
# `make xbps-container` run it on the pinned image with $CONTAINER_ENGINE (default podman;
# the release workflow sets docker). The explicit command is:
#
#   . packaging/void/images.env
#   ${CONTAINER_ENGINE:-podman} run --rm --privileged -v "$PWD:/src" "$VOID_IMAGE_GLIBC" \
#       /src/packaging/void/build-in-container.sh
#
# The container needs --privileged for the chroot style (uchroot) and the /dev/shm mounts.
#
# Expects /src/dist/phlegethon-<version>.tar.gz (scripts/make-dist-tarball.sh).
# Writes the package(s) to /src/out/<libc>/. Follows void-packages' own CI: binary-bootstrap,
# then pkg. The chroot style depends on the engine (see XBPS_CHROOT_CMD below).
#
# Environment:
#   SRC=/src                     repository checkout
#   OUT=$SRC/out                 where packages are copied
#   VOID_PACKAGES_REF=<commit>   void-packages commit to build against (default: the pin in
#                                packaging/void/images.env; a branch name also works)
#   XBPS_CHROOT_CMD=uchroot|ethereal  chroot style (default: ethereal in a rootless container,
#                                uchroot otherwise)
#   EXTRA_CA_BUNDLE=<file>       extra CA certificates to trust (TLS-inspecting proxies)
set -eu

SRC=${SRC:-/src}
OUT=${OUT:-$SRC/out}
REF=${VOID_PACKAGES_REF:-}
if [ -z "$REF" ]; then
	# shellcheck disable=SC1091
	. "$SRC/packaging/void/images.env"
	REF=$VOID_PACKAGES_REF
fi
WORK=/build

libc=glibc
[ -e /lib/ld-musl-x86_64.so.1 ] || [ -n "$(ls /lib/ld-musl-*.so.1 2>/dev/null)" ] && libc=musl

# Chroot style. uchroot (xbps-uchroot) bind-mounts /dev read-only inside the chroot. In a
# rootless container that makes /dev/null read-only too, and xbps-src then stops at its first
# redirect to /dev/null with "/dev/null: Read-only file system". A rootless engine therefore uses
# ethereal: the container root is the build root (the masterdir is a symlink to /), so no
# mounts are made and the container is the chroot. Rootful Docker keeps uchroot.
if [ -z "${XBPS_CHROOT_CMD:-}" ]; then
	# uid_map: "<inside> <outside> <count>". Root outside (0) means rootful Docker.
	read -r _ outer _ </proc/self/uid_map
	if [ "$outer" = 0 ]; then XBPS_CHROOT_CMD=uchroot; else XBPS_CHROOT_CMD=ethereal; fi
fi
case $XBPS_CHROOT_CMD in
uchroot | ethereal) ;;
*)
	echo "XBPS_CHROOT_CMD must be uchroot or ethereal, not $XBPS_CHROOT_CMD" >&2
	exit 1
	;;
esac
echo ">> chroot style: $XBPS_CHROOT_CMD"

trust_extra_ca() { # $1 = root directory
	[ -n "${EXTRA_CA_BUNDLE:-}" ] && [ -f "$EXTRA_CA_BUNDLE" ] || return 0
	bundle="$1/etc/ssl/certs/ca-certificates.crt"
	[ -f "$bundle" ] || return 0
	if ! grep -q "phlegethon-extra-ca" "$bundle"; then
		{ echo "# phlegethon-extra-ca"; cat "$EXTRA_CA_BUNDLE"; } >>"$bundle"
	fi
	# Tools inside the xbps-src chroot (xbps, cargo, git) run with a cleared environment,
	# so SSL_CA_CERT_FILE does not reach them. OpenSSL always checks the hashed
	# directory, so install each extra certificate there as <subject-hash>.<n>.
	split=$(mktemp -d)
	awk -v dir="$split" '/BEGIN CERTIFICATE/{n++} n{print > (dir "/c" n ".pem")}' "$EXTRA_CA_BUNDLE"
	for cert in "$split"/c*.pem; do
		[ -f "$cert" ] || continue
		hash=$(openssl x509 -hash -noout -in "$cert" 2>/dev/null) || continue
		i=0
		while [ -e "$1/etc/ssl/certs/$hash.$i" ] && ! cmp -s "$cert" "$1/etc/ssl/certs/$hash.$i"; do
			i=$((i + 1))
		done
		cp "$cert" "$1/etc/ssl/certs/$hash.$i"
	done
	rm -rf "$split"
}

echo ">> preparing container ($libc)"
trust_extra_ca ""
# xbps's fetch library reads its CA file from SSL_CA_CERT_FILE, not the bundle path.
if [ -n "${EXTRA_CA_BUNDLE:-}" ]; then
	export SSL_CA_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
	export CARGO_HTTP_CAINFO=/etc/ssl/certs/ca-certificates.crt
fi
xbps-install -Syu xbps >/dev/null
xbps-install -yu >/dev/null
xbps-install -y sudo bash git curl >/dev/null
trust_extra_ca ""
id builder >/dev/null 2>&1 || useradd -G xbuilder -M builder

version=$(sed -n 's/^version=//p' "$SRC/packaging/void/srcpkgs/phlegethon/template")
tarball="$SRC/dist/phlegethon-$version.tar.gz"
[ -f "$tarball" ] || {
	echo "missing $tarball: run scripts/make-dist-tarball.sh first" >&2
	exit 1
}

echo ">> void-packages ($REF)"
mkdir -p "$WORK"
if [ ! -d "$WORK/void-packages/.git" ]; then
	# A shallow fetch of one ref works for a commit as well as for a branch.
	git init -q "$WORK/void-packages"
	git -C "$WORK/void-packages" fetch -q --depth 1 https://github.com/void-linux/void-packages "$REF"
	git -C "$WORK/void-packages" checkout -q FETCH_HEAD
fi
cd "$WORK/void-packages"

rm -rf "${WORK:?}/void-packages/srcpkgs/phlegethon"
cp -r "$SRC/packaging/void/srcpkgs/phlegethon" srcpkgs/phlegethon
sum=$(sha256sum "$tarball" | cut -d' ' -f1)
sed -i "s/^checksum=.*/checksum=$sum/" srcpkgs/phlegethon/template
mkdir -p "hostdir/sources/phlegethon-$version"
cp "$tarball" "hostdir/sources/phlegethon-$version/"

touch etc/conf
sed -i -e '/^XBPS_CHROOT_CMD=/d' -e '/^XBPS_ALLOW_CHROOT_BREAKOUT=/d' etc/conf
echo "XBPS_CHROOT_CMD=$XBPS_CHROOT_CMD" >>etc/conf
if [ "$XBPS_CHROOT_CMD" = ethereal ]; then
	# The container is disposable and is the build root, so xbps-src may run as root.
	echo XBPS_ALLOW_CHROOT_BREAKOUT=yes >>etc/conf
	ln -sfn / masterdir
	xbps-uhelper arch >/.xbps_chroot_init
fi
grep -q '^XBPS_ALLOW_RESTRICTED=' etc/conf || echo XBPS_ALLOW_RESTRICTED=yes >>etc/conf
if [ "$XBPS_CHROOT_CMD" = uchroot ]; then
	chown -R builder:builder "$WORK"
fi

# xbps-src runs as the builder user under uchroot, and as root under ethereal.
xbs() {
	if [ "$XBPS_CHROOT_CMD" = ethereal ]; then
		./xbps-src "$@"
	else
		sudo -Eu builder ./xbps-src "$@"
	fi
}

echo ">> binary-bootstrap"
xbs binary-bootstrap
trust_extra_ca "$WORK/void-packages/masterdir-$(xbps-uhelper arch)"
for d in masterdir*; do trust_extra_ca "$WORK/void-packages/$d"; done

echo ">> building phlegethon-$version"
xbs -j"$(nproc)" pkg phlegethon

mkdir -p "$OUT/$libc"
cp hostdir/binpkgs/phlegethon-*.xbps "$OUT/$libc/"
set -- "$OUT/$libc/phlegethon-${version}_"*.xbps
pkg=$1

echo ">> install check"
xbps-rindex -a "$OUT/$libc"/*.xbps >/dev/null
xbps-install -y --repository="$OUT/$libc" phlegethon >/dev/null
for bin in /usr/bin/phlegethon /usr/bin/phlegethon-nmh; do
	missing=$(ldd "$bin" | grep "not found" || true)
	[ -z "$missing" ] || {
		echo "unresolved libraries in $bin: $missing" >&2
		exit 1
	}
done
# G-7: the packaged binary must embed the frontend (built with custom-protocol).
tar -xzOf "$tarball" "phlegethon-$version/packages/tauri-shell/frontend/dist/index.html" >"$WORK/index.html"
"$SRC/scripts/check-embedded-frontend.sh" /usr/bin/phlegethon "$WORK/index.html"
test -f /usr/share/applications/phlegethon.desktop
test -f /usr/share/licenses/phlegethon/LICENSE
rm -f "$OUT/$libc"/*-repodata

echo ">> done: $pkg"
ls -l "$OUT/$libc"
