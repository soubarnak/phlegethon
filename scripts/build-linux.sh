#!/usr/bin/env bash
# Build the frontend and the release binaries (no Tauri bundler, no deb/AppImage).
#
#   scripts/build-linux.sh                   full build
#   SKIP_FRONTEND=1 scripts/build-linux.sh   reuse an existing packages/tauri-shell/frontend/dist
#   CARGO_FLAGS="--offline --locked" scripts/build-linux.sh
#
# Afterwards it checks that the binary embeds the frontend (scripts/check-embedded-frontend.sh).
# Honors CARGO_TARGET_DIR.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
frontend="$root/packages/tauri-shell/frontend"

if [ "${SKIP_FRONTEND:-0}" != 1 ]; then
	command -v npm >/dev/null 2>&1 || {
		echo "npm not found: install nodejs (scripts/setup-void.sh) or set SKIP_FRONTEND=1 with a prebuilt dist." >&2
		exit 1
	}
	(
		cd "$frontend"
		if [ -f package-lock.json ]; then npm ci --no-audit --no-fund; else npm install --no-audit --no-fund; fi
		npm run build
	)
elif [ ! -f "$frontend/dist/index.html" ]; then
	echo "SKIP_FRONTEND=1 but $frontend/dist/index.html is missing." >&2
	exit 1
fi

cd "$root"
# custom-protocol embeds frontend/dist; without it the window would load the dev server URL.
# shellcheck disable=SC2086
cargo build --release ${CARGO_FLAGS:---locked} \
	-p phlegethon-tauri -p phlegethon-nmh \
	--features phlegethon-tauri/custom-protocol

target="${CARGO_TARGET_DIR:-$root/target}/release"
"$root/scripts/check-embedded-frontend.sh" "$target/phlegethon" "$frontend/dist/index.html"

echo
echo "Built:"
ls -lh "$target/phlegethon" "$target/phlegethon-nmh"
