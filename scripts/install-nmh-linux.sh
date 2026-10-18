#!/usr/bin/env bash
# Register the Phlegethon native messaging host with Chrome-family browsers.
#
#   scripts/install-nmh-linux.sh --extension-id <32 letters a-p> [--extension-id ...]
#       [--host-path /usr/bin/phlegethon-nmh] [--browsers chrome,chromium,brave,vivaldi,edge]
#       [--system] [--uninstall]
#
# --system writes to /etc (Chrome and Chromium only) and needs root.
set -euo pipefail

host_name=com.phlegethon.bridge
need_value() { # $1 = option name, $2 = its value
	[ $# -ge 2 ] && [ -n "$2" ] || {
		echo "$1 needs a value" >&2
		exit 2
	}
}
ids=() host_path="" browsers=chrome,chromium,brave,vivaldi,edge system=0 uninstall=0
while [ $# -gt 0 ]; do
	case "$1" in
	--extension-id)
		need_value "$@"
		ids+=("$2")
		shift 2
		;;
	--host-path)
		need_value "$@"
		host_path=$2
		shift 2
		;;
	--browsers)
		need_value "$@"
		browsers=$2
		shift 2
		;;
	--system)
		system=1
		shift
		;;
	--uninstall)
		uninstall=1
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

dir_for() { # browser -> manifest directory (empty if none)
	case "$1:$system" in
	chrome:0) echo "$HOME/.config/google-chrome/NativeMessagingHosts" ;;
	chromium:0) echo "$HOME/.config/chromium/NativeMessagingHosts" ;;
	brave:0) echo "$HOME/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts" ;;
	vivaldi:0) echo "$HOME/.config/vivaldi/NativeMessagingHosts" ;;
	edge:0) echo "$HOME/.config/microsoft-edge/NativeMessagingHosts" ;;
	chrome:1) echo "/etc/opt/chrome/native-messaging-hosts" ;;
	chromium:1) echo "/etc/chromium/native-messaging-hosts" ;;
	*) echo "" ;;
	esac
}

IFS=, read -r -a browser_list <<<"$browsers"

if [ "$uninstall" -eq 1 ]; then
	for b in "${browser_list[@]}"; do
		d=$(dir_for "$b")
		if [ -n "$d" ] && [ -f "$d/$host_name.json" ]; then
			rm -v "${d:?}/${host_name:?}.json"
		fi
	done
	exit 0
fi

[ "${#ids[@]}" -gt 0 ] || {
	echo "at least one --extension-id is required (chrome://extensions, Developer mode)." >&2
	exit 2
}
for id in "${ids[@]}"; do
	[[ "$id" =~ ^[a-p]{32}$ ]] || {
		echo "not a valid Chrome extension id: $id" >&2
		exit 2
	}
done

if [ -z "$host_path" ]; then
	root=$(cd "$(dirname "$0")/.." && pwd)
	if command -v phlegethon-nmh >/dev/null 2>&1; then
		host_path=$(command -v phlegethon-nmh)
	elif [ -x "$root/target/release/phlegethon-nmh" ]; then
		host_path="$root/target/release/phlegethon-nmh"
	else
		echo "phlegethon-nmh not found: pass --host-path or build with scripts/build-linux.sh." >&2
		exit 1
	fi
fi
case "$host_path" in
/*) ;;
*)
	echo "--host-path must be absolute: $host_path" >&2
	exit 2
	;;
esac
[ -x "$host_path" ] || {
	echo "not an executable file: $host_path" >&2
	exit 1
}

# The manifest is JSON: escape what a path may contain.
json_string() {
	printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}
case "$host_path" in
*[[:cntrl:]]*)
	echo "--host-path must not contain control characters" >&2
	exit 2
	;;
esac
json_host_path=$(json_string "$host_path")

origins=""
for id in "${ids[@]}"; do origins+="${origins:+,}\"chrome-extension://$id/\""; done

wrote=0
for b in "${browser_list[@]}"; do
	d=$(dir_for "$b")
	if [ -z "$d" ]; then
		echo "skipping $b (no system-wide location)" >&2
		continue
	fi
	mkdir -p "$d"
	cat >"$d/$host_name.json" <<JSON
{
  "name": "$host_name",
  "description": "Phlegethon Download Manager bridge",
  "path": "$json_host_path",
  "type": "stdio",
  "allowed_origins": [$origins]
}
JSON
	chmod 644 "$d/$host_name.json"
	echo "wrote $d/$host_name.json"
	wrote=$((wrote + 1))
done
[ "$wrote" -gt 0 ] || exit 1
