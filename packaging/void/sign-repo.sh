#!/bin/sh
# Index and sign the release packages as an xbps repository. Run by the release workflow in the
# pinned void-glibc image (packaging/void/images.env, VOID_IMAGE_SIGN):
#
#   sign-repo.sh DIR KEYFILE
#
# DIR holds one phlegethon-*.x86_64.xbps and one phlegethon-*.x86_64-musl.xbps. The script
# writes into DIR:
#   x86_64-repodata, x86_64-musl-repodata   signed package index, one per libc
#   <package>.sig2                          signature of each package
# Each index lists only the packages of its own arch, so each arch is indexed on its own.
# KEYFILE is the RSA private key (PEM) for xbps-rindex. Packages are signed as
# "Bongbetic <soubarna@live.in>".
set -eu

dir=$1
key=$2
signedby="Bongbetic <soubarna@live.in>"

[ -s "$key" ] || {
	echo "sign-repo: signing key $key is missing or empty" >&2
	exit 1
}
command -v xbps-rindex >/dev/null || {
	echo "sign-repo: xbps-rindex not found; this must run in the void-glibc image" >&2
	exit 1
}
cd "$dir"

for arch in x86_64 x86_64-musl; do
	# glibc packages end in .x86_64.xbps; musl packages end in .x86_64-musl.xbps.
	set -- phlegethon-*."$arch".xbps
	[ -f "$1" ] || {
		echo "sign-repo: no $arch package in $dir" >&2
		exit 1
	}
	[ $# -eq 1 ] || {
		echo "sign-repo: more than one $arch package in $dir" >&2
		exit 1
	}
	pkg=$1

	echo ">> indexing $pkg as $arch"
	XBPS_ARCH=$arch xbps-rindex -a "$pkg"
	XBPS_ARCH=$arch xbps-rindex --privkey "$key" --signedby "$signedby" -s .
	echo ">> signing $pkg"
	xbps-rindex --privkey "$key" --signedby "$signedby" -S "$pkg"
done

# Every output must exist and be non-empty before anything is published.
for repodata in x86_64-repodata x86_64-musl-repodata; do
	[ -s "$repodata" ] || {
		echo "sign-repo: $repodata was not written" >&2
		exit 1
	}
done
for pkg in phlegethon-*.xbps; do
	[ -s "$pkg.sig2" ] || {
		echo "sign-repo: $pkg.sig2 was not written" >&2
		exit 1
	}
done
ls -l
