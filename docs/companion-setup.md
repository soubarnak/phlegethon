# Chrome Companion setup

Chrome Companion hands the current tab URL (and optionally same-origin cookies) to the running Phlegethon desktop app. It does **not** intercept browser downloads.

## Architecture

```
Chrome Companion (MV3)
        │  chrome.runtime.sendNativeMessage
        ▼
phlegethon-nmh  (native messaging host, stdio)
        │  local socket / named pipe: phlegethon-companion
        ▼
Platform Shell + Download Core
```

| Component | Path |
| --- | --- |
| Extension | `packages/chrome-companion/` |
| Native messaging host | `packages/native-messaging-host/` (`phlegethon-nmh`) |
| Protocol types | `packages/companion-protocol/` |
| Debian receiver | `packages/tauri-shell/src-tauri/src/companion.rs` |
| Win11 receiver | `packages/winui-shell/Phlegethon/Services/CompanionReceiverService.cs` |

## Build

### Prerequisites (Windows / MSVC)

`download-core` (and crates that pull in `ring`) need the **MSVC C++ toolchain** with Windows SDK headers/libs. A plain PowerShell without VC env fails with `vcruntime.h` / `LNK1104: msvcrt.lib` because `INCLUDE` and `LIB` are unset.

Install **Visual Studio 2022 Build Tools** (or Community) with component `Microsoft.VisualStudio.Component.VC.Tools.x86.x64`.

Then either:

1. Open **x64 Native Tools Command Prompt for VS 2022**, `cd` to the repo, and run `cargo`, or
2. Load DevShell in PowerShell, then build:

```powershell
$vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" `
  -latest -products * `
  -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
  -property installationPath
Import-Module "$vs\Common7\Tools\Microsoft.VisualStudio.DevShell.dll"
Enter-VsDevShell -VsInstallPath $vs -SkipAutomaticLocation -DevCmdArguments '-arch=x64' | Out-Null
cargo build -p download-core
```

Or use the helper script (auto-loads DevShell):

```powershell
./scripts/build-core.ps1
# optional: ./scripts/build-core.ps1 --release
```

### Packages

```bash
# Native messaging host
cargo build -p phlegethon-nmh --release

# Debian shell (includes companion receiver)
cargo build -p phlegethon-tauri
```

WinUI shell: build `packages/winui-shell/Phlegethon/Phlegethon.csproj` as usual (`-p:Platform=x64`).

## Install native messaging host

Host name: **`com.phlegethon.bridge`**

Template manifest: `packages/native-messaging-host/manifests/com.phlegethon.bridge.json`

Replace:

- `path` — absolute path to `phlegethon-nmh` (`.exe` on Windows)
- `allowed_origins` — your unpacked or store extension ID (`chrome-extension://<id>/`)

### Windows (Chrome + Chromium)

Per-user registry (repeat for each browser):

| Browser | Registry key |
| --- | --- |
| Chrome | `HKCU\Software\Google\Chrome\NativeMessagingHosts\com.phlegethon.bridge` |
| Chromium | `HKCU\Software\Chromium\NativeMessagingHosts\com.phlegethon.bridge` |

Default value = full path to the host manifest JSON file.

### Debian / Linux (Chrome + Chromium)

| Scope | Chrome | Chromium |
| --- | --- | --- |
| User | `~/.config/google-chrome/NativeMessagingHosts/com.phlegethon.bridge.json` | `~/.config/chromium/NativeMessagingHosts/com.phlegethon.bridge.json` |
| System | `/etc/opt/chrome/native-messaging-hosts/com.phlegethon.bridge.json` | `/etc/chromium/native-messaging-hosts/com.phlegethon.bridge.json` |

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
| Context menu → …(with cookies) | Send URL + write Netscape cookie file → Core `CookieChannel` |

Launch Sites still use **Probe-then-Confirm** — the companion never auto-confirms a probe sheet.

## Receiver paths (acceptance)

| Platform | Receiver | Socket / pipe |
| --- | --- | --- |
| Debian (Tauri) | Background thread in `companion.rs` | `phlegethon-companion` (interprocess local socket) |
| Win11 (WinUI) | `CompanionReceiverService` | Named pipe `\\.\pipe\phlegethon-companion` |

Both call the same Download Core enqueue path as paste/drop. Optional cookies are applied via `set_cookie_channel` before enqueue.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Native messaging connection failed | NMH manifest path, `allowed_origins` extension ID, host binary exists |
| Phlegethon desktop app is not running | Start Tauri or WinUI shell first |
| Probe sheet does not appear | Shell event loop running; check shell logs |
| Cookies ignored | Use explicit “with cookies” menu item; verify cookie file under app data `companion-cookies/` |

## Security notes

- Session material travels only over native messaging + local socket — never custom URI schemes.
- Cookie export requires an explicit per-send user gesture.
- Download Core classifies all URLs; Launch Sites cannot skip Probe-then-Confirm.
