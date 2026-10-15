#!/bin/sh
# Build the phlegethon xbps package with xbps-src inside a Void Linux container.
# Used by the release workflow and for local builds:
#
#   docker run --rm --privileged -v "$PWD:/src" ghcr.io/void-linux/void-glibc-full \
#       /src/packaging/void/build-in-container.sh
#
# Expects /src/dist/phlegethon-<version>.tar.gz (scripts/make-dist-tarball.sh).
# Writes the package(s) to /src/out/<libc>/. Mirrors void-packages' own CI: a non-root
# builder user, the uchroot chroot style, then binary-bootstrap and pkg.
#
# Environment:
#   SRC=/src                     repository checkout
#   OUT=$SRC/out                 where packages are copied
#   VOID_PACKAGES_REF=master     void-packages branch to build against
#   EXTRA_CA_BUNDLE=<file>       extra CA certificates to trust (TLS-inspecting proxies)
set -eu

SRC=${SRC:-/src}
OUT=${OUT:-$SRC/out}
REF=${VOID_PACKAGES_REF:-master}
WORK=/build

libc=glibc
[ -e /lib/ld-musl-x86_64.so.1 ] || [ -n "$(ls /lib/ld-musl-*.so.1 2>/dev/null)" ] && libc=musl

trust_extra_ca() { # $1 = root directory
	[ -n "${EXTRA_CA_BUNDLE:-}" ] && [ -f "$EXTRA_CA_BUNDLE" ] || return 0
	local f
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
	git clone --depth 1 --branch "$REF" https://github.com/void-linux/void-packages "$WORK/void-packages"
fi
cd "$WORK/void-packages"

rm -rf "${WORK:?}/void-packages/srcpkgs/phlegethon"
cp -r "$SRC/packaging/void/srcpkgs/phlegethon" srcpkgs/phlegethon
sum=$(sha256sum "$tarball" | cut -d' ' -f1)
sed -i "s/^checksum=.*/checksum=$sum/" srcpkgs/phlegethon/template
mkdir -p "hostdir/sources/phlegethon-$version"
cp "$tarball" "hostdir/sources/phlegethon-$version/"

grep -q '^XBPS_CHROOT_CMD=' etc/conf 2>/dev/null || {
	echo XBPS_CHROOT_CMD=uchroot >>etc/conf
	echo XBPS_ALLOW_RESTRICTED=yes >>etc/conf
}
chown -R builder:builder "$WORK"

echo ">> binary-bootstrap"
sudo -Eu builder ./xbps-src binary-bootstrap
trust_extra_ca "$WORK/void-packages/masterdir-$(xbps-uhelper arch)"
for d in masterdir*; do trust_extra_ca "$WORK/void-packages/$d"; done

echo ">> building phlegethon-$version"
sudo -Eu builder ./xbps-src -j"$(nproc)" pkg phlegethon

mkdir -p "$OUT/$libc"
cp hostdir/binpkgs/phlegethon-*.xbps "$OUT/$libc/"
pkg=$(ls "$OUT/$libc"/phlegethon-"$version"_*.xbps | head -1)

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
test -f /usr/share/applications/phlegethon.desktop
test -f /usr/share/licenses/phlegethon/LICENSE
rm -f "$OUT/$libc"/*-repodata

echo ">> done: $pkg"
ls -l "$OUT/$libc"
