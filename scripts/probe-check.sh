#!/usr/bin/env bash
# Probe the reference URLs (docs/reference-urls.json) with the real toolchain.
#
#   scripts/probe-check.sh                       all entries
#   scripts/probe-check.sh --site xhamster       one site (xhamster, xxxbp, luxuretv, youtube, generic)
#   scripts/probe-check.sh --cookies-from-browser firefox   pass your browser's cookies
#   scripts/probe-check.sh --install-ytdlp       if yt-dlp is missing, install it into a private venv
#
# Other options are passed to the probe example (--timeout, --proxy, --cookie-file, ...).
# Prints a markdown table. Exit status is non-zero only when an entry FAILs; BLOCKED
# (for example luxuretv without browser cookies) is the documented outcome and passes.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
install_ytdlp=0 pass=()
while [ $# -gt 0 ]; do
	case "$1" in
	--install-ytdlp)
		install_ytdlp=1
		shift
		;;
	-h | --help)
		sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
		exit 0
		;;
	*)
		pass+=("$1")
		shift
		;;
	esac
done

have_ytdlp() {
	[ -n "${YTDLP_PATH:-}" ] && [ -x "$YTDLP_PATH" ] && return 0
	command -v yt-dlp >/dev/null 2>&1 && return 0
	python3 -c 'import yt_dlp' >/dev/null 2>&1
}

if ! have_ytdlp; then
	if [ "$install_ytdlp" -eq 1 ]; then
		venv="${XDG_CACHE_HOME:-$HOME/.cache}/phlegethon/probe-venv"
		[ -x "$venv/bin/yt-dlp" ] || {
			python3 -m venv "$venv"
			"$venv/bin/pip" install --quiet --upgrade yt-dlp
		}
		export YTDLP_PATH="$venv/bin/yt-dlp"
		echo "using $YTDLP_PATH" >&2
	else
		echo "yt-dlp not found. Install it (xbps-install yt-dlp) or re-run with --install-ytdlp." >&2
		exit 1
	fi
fi

cd "$root"
exec cargo run --quiet -p download-core --example probe -- \
	--check docs/reference-urls.json "${pass[@]}"
