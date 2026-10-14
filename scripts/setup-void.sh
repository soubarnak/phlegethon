#!/usr/bin/env bash
# Install what Phlegethon needs on Void Linux (glibc or musl).
#
#   scripts/setup-void.sh                 build + runtime dependencies
#   scripts/setup-void.sh --runtime-only  only what a packaged install needs
#   scripts/setup-void.sh --dry-run       print the xbps-install command and exit
#   scripts/setup-void.sh -y              do not ask for confirmation
set -euo pipefail

runtime_only=0 dry_run=0 assume_yes=0 force=0
for arg in "$@"; do
	case "$arg" in
	--runtime-only) runtime_only=1 ;;
	--dry-run) dry_run=1 ;;
	-y | --yes) assume_yes=1 ;;
	--force) force=1 ;;
	-h | --help)
		sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'
		exit 0
		;;
	*)
		echo "unknown option: $arg" >&2
		exit 2
		;;
	esac
done

if ! command -v xbps-install >/dev/null 2>&1 && [ "$force" -eq 0 ] && [ "$dry_run" -eq 0 ]; then
	echo "xbps-install not found: this script is for Void Linux (use --dry-run to just see the package list)." >&2
	exit 1
fi

arch=$(xbps-uhelper arch 2>/dev/null || uname -m)
case "$arch" in
*-musl) libc=musl ;;
*) libc=glibc ;;
esac

# What the installed app needs.
runtime=(yt-dlp ffmpeg nodejs xdg-utils xdg-desktop-portal dbus)
# Optional native folder chooser used when no portal backend is running.
fallback_pickers=(zenity)
# What `scripts/build-linux.sh` needs on top.
build=(base-devel git pkg-config rust cargo nodejs webkit2gtk41-devel gtk+3-devel libsoup3-devel openssl-devel librsvg-devel)

packages=("${runtime[@]}" "${fallback_pickers[@]}")
[ "$runtime_only" -eq 0 ] && packages+=("${build[@]}")
# De-duplicate while keeping order.
mapfile -t packages < <(printf '%s\n' "${packages[@]}" | awk '!seen[$0]++')

as_root=()
if [ "$(id -u)" -ne 0 ] && [ "$dry_run" -eq 0 ]; then
	if command -v doas >/dev/null 2>&1; then
		as_root=(doas)
	elif command -v sudo >/dev/null 2>&1; then
		as_root=(sudo)
	else
		echo "need root: install sudo or doas, or run this script as root." >&2
		exit 1
	fi
fi

cmd=("${as_root[@]}" xbps-install -S)
[ "$assume_yes" -eq 1 ] && cmd+=(-y)
cmd+=("${packages[@]}")

echo "Platform: Void Linux ($arch, $libc)"
echo "Running:  ${cmd[*]}"
[ "$dry_run" -eq 1 ] && exit 0
"${cmd[@]}"

echo
echo "Installed. Also install a portal backend for your desktop so the folder chooser works:"
echo "  xdg-desktop-portal-gtk (GNOME/Xfce/most), xdg-desktop-portal-kde, xdg-desktop-portal-wlr"
if [ "$libc" = musl ]; then
	cat <<'NOTE'

musl notes:
  - Use the yt-dlp package from xbps (needs python3), not the prebuilt "yt-dlp_linux"
    binary, which is linked against glibc. Phlegethon's built-in "Install/Update yt-dlp"
    picks the universal zipapp on musl, which also needs python3.
  - Run "xbps-query -l | grep python3" and install python3 if it is missing.
NOTE
fi
