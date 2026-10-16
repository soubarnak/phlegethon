# Chrome Companion setup

Chrome Companion hands the current tab URL (and optionally same-origin cookies) to the running Phlegethon desktop app. It does **not** intercept browser downloads.

## Architecture

```
Chrome Companion (MV3)
        │  chrome.runtime.sendNativeMessage
        ▼
phlegethon-nmh  (native messaging host, stdio)
        │  filesystem socket: $XDG_RUNTIME_DIR/phlegethon/companion.sock
        ▼
Platform Shell + Download Core
```

| Component | Path |
| --- | --- |
| Extension | `packages/chrome-companion/` |
| Native messaging host | `packages/native-messaging-host/` (`phlegethon-nmh`) |
| Protocol types | `packages/companion-protocol/` |
| Tauri receiver | `packages/tauri-shell/src-tauri/src/companion.rs` |

## Build

### Build (Linux)

```bash
scripts/build-linux.sh
```

This builds the frontend, the shell (which includes the companion receiver) and `phlegethon-nmh` with `--release --features phlegethon-tauri/custom-protocol`. A plain `cargo build -p phlegethon-tauri` produces a shell that loads the dev server at `http://localhost:1420` instead of the embedded frontend, so do not use it for a usable app.


## Install native messaging host

Host name: **`com.phlegethon.bridge`**

Quick path on Linux (writes the manifest for Chrome, Chromium, Brave, Vivaldi and Edge, using the installed or freshly built `phlegethon-nmh`):

```bash
scripts/install-nmh-linux.sh --extension-id <32-letter extension id>
# --browsers chrome,chromium  limit to some browsers;  --system  /etc for Chrome/Chromium;  --uninstall
```

The manual steps below do the same thing by hand.

Template manifest: `packages/native-messaging-host/manifests/com.phlegethon.bridge.json` (a reference for the manual steps; `scripts/install-nmh-linux.sh` does not read it, and a test checks that both have the same fields).

Replace:

- `path` — absolute path to `phlegethon-nmh`
- `allowed_origins` — your unpacked or store extension ID (`chrome-extension://<id>/`)

### Linux (Chrome, Chromium and other Chromium browsers)

| Scope | Chrome | Chromium |
| --- | --- | --- |
| User | `~/.config/google-chrome/NativeMessagingHosts/com.phlegethon.bridge.json` | `~/.config/chromium/NativeMessagingHosts/com.phlegethon.bridge.json` |
| System | `/etc/opt/chrome/native-messaging-hosts/com.phlegethon.bridge.json` | `/etc/chromium/native-messaging-hosts/com.phlegethon.bridge.json` |

The install script also writes the per-user manifest for other Chromium browsers:

| Browser | User manifest directory |
| --- | --- |
| Brave | `~/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts/` |
| Vivaldi | `~/.config/vivaldi/NativeMessagingHosts/` |
| Edge | `~/.config/microsoft-edge/NativeMessagingHosts/` |

`path` in the manifest must be an **absolute** path to `phlegethon-nmh` on Linux.

## Load the extension

1. Open `chrome://extensions`
2. Enable **Developer mode**
3. **Load unpacked** → select `packages/chrome-companion/`
4. Copy the extension ID into the NMH manifest `allowed_origins`
5. Re-install or update the NMH manifest if the ID changes

## Usage

With Phlegethon desktop running:

| Gesture | Action |
| --- | --- |
| Toolbar icon | Send current tab URL |
| Context menu → Send page/link to Phlegethon | Send URL |
| Context menu → …(with cookies) | Send URL + a Netscape cookie file used by that download only (per-job cookie session) |

Launch Sites still use **Probe-then-Confirm** — the companion never auto-confirms a probe sheet.

## Receiver paths (acceptance)

| Platform | Receiver | Socket |
| --- | --- | --- |
| Tauri shell | Background thread in `companion.rs` | `$XDG_RUNTIME_DIR/phlegethon/companion.sock` (see ADR 0005) |

Both call the same Download Core enqueue path as paste/drop. Optional cookies are attached to that one enqueue as a per-job cookie session. They never replace the cookie setting in Settings. The shell deletes the cookie file when the job Completes or is discarded, or when the probe sheet is dismissed.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Native messaging connection failed | NMH manifest path, `allowed_origins` extension ID, host binary exists |
| Phlegethon desktop app is not running | Start the Phlegethon app first |
| Probe sheet does not appear | Shell event loop running; check shell logs |
| Cookies ignored | Use explicit “with cookies” menu item; verify cookie file under app data `companion-cookies/` |

## Security notes

- Session material travels only over native messaging + a private local socket (directory 0700, socket 0600, owner checked by the host) — never custom URI schemes. See ADR 0005.
- The host rejects callers whose `chrome-extension://` origin is not in an installed manifest, so re-run `scripts/install-nmh-linux.sh` after changing the extension id.
- Cookie export requires an explicit per-send user gesture. The extension holds no cookie or site access at install time: choosing a "with cookies" menu item asks Chrome for the `cookies` permission and access to that one origin, and cookies are collected for the URL being sent (the link target for a link).
- A failed hand-off, including "Phlegethon desktop app is not running", shows a red `!` on the toolbar icon with the reason in its tooltip. The next successful hand-off clears it.
- Download Core classifies all URLs; Launch Sites cannot skip Probe-then-Confirm.
