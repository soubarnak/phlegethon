#!/bin/sh
# Check that a built phlegethon binary embeds the frontend (G-7, M-58).
#
#   scripts/check-embedded-frontend.sh <phlegethon binary> <frontend index.html>
#
# A shell built without the `custom-protocol` feature loads http://localhost:1420
# instead and shows a blank window. Tauri stores the embedded assets under their
# paths, so every /assets/... file that index.html references must appear in the
# binary. A build without the feature contains none of them.
set -eu

[ $# -eq 2 ] || {
	sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//' >&2
	exit 2
}
bin=$1 index=$2
[ -f "$bin" ] || {
	echo "binary not found: $bin" >&2
	exit 1
}
[ -f "$index" ] || {
	echo "index.html not found: $index" >&2
	exit 1
}

assets=$(grep -o 'assets/[A-Za-z0-9._-]*\.\(js\|css\)' "$index" | sort -u)
[ -n "$assets" ] || {
	echo "no assets/*.js or assets/*.css referenced in $index" >&2
	exit 1
}
missing=0
for asset in $assets; do
	if grep -aqF "/$asset" "$bin"; then
		echo "embedded: $asset"
	else
		echo "NOT embedded: $asset" >&2
		missing=1
	fi
done
if [ "$missing" -ne 0 ]; then
	echo "$bin does not embed the frontend: build with --features phlegethon-tauri/custom-protocol." >&2
	exit 1
fi
