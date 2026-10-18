> **Partly superseded (2026-10-16).** This note was written when Phlegethon targeted Windows 11 and Debian with a WinUI shell. [ADR 0001](../adr/0001-drop-windows-shell.md) dropped Windows: the only shell is Tauri 2 on Void Linux. The native-messaging recommendation still holds. The Windows registry, `--parent-window`, WinUI and Debian installer sections do not apply. The implemented design, including the filesystem socket between the host and the shell, is in [ADR 0005](../adr/0005-companion-filesystem-socket.md) and [docs/companion-setup.md](../companion-setup.md).

# Research: Chrome Companion → desktop bridge

**Ticket:** [#4](https://github.com/soubarnak/phlegethon/issues/4)  
**Question:** How should a Chrome Companion hand URLs, download intents, and optional session material to the desktop Download Manager on Windows 11 and Debian (native messaging vs local loopback API), including security boundaries — for a companion designed in the hand-off spec but deferred from desktop MVP?

**Constraint:** No Google-owned documentation. Sources below are Chromium open-source docs/source, MDN / Mozilla WebExtensions, WICG, Microsoft Learn (Edge / Windows URI schemes), and freedesktop.org.

---

## Verdict

**Recommend native messaging as the companion↔desktop transport**, with a small OS-installed host that speaks the length-prefixed JSON stdio protocol and forwards intents into the running Download Manager (or cold-starts it).

**Do not make a loopback HTTP API the extension-facing bridge.** Local Network Access / CSRF pressure and the need for bearer secrets make it a poor public surface for a browser extension. If the desktop needs an internal local API later, keep it **behind** the native host (named pipe / Unix socket), not reachable from arbitrary web origins.

**Custom URI schemes are optional and secondary** — useful only for “open / focus Phlegethon with a URL,” never for cookies or other session material.

Desktop MVP remains paste/drop URLs only; this note locks the **designed** companion bridge so the hand-off spec does not reopen the transport choice later.

---

## Options compared

| Bridge | Fit for Companion | Security boundary | Win11 + Debian |
| --- | --- | --- | --- |
| **Native messaging (stdio host)** | Purpose-built extension↔native IPC; bidirectional allowlist | Host allowlists exact extension origin(s); browser launches host; no LAN exposure | Registry (Windows) + fixed dirs (Linux); installer must cover Chrome **and** Chromium product paths |
| **Local loopback HTTP/WS** | Easy to prototype; works from any client that can `fetch` | Public→loopback is a known CSRF / fingerprinting class; LNA gates web origins; still needs auth if anything on the box can connect | Same code both OSes, but you own binding, CORS, tokens, and LNA edge cases |
| **Custom URI scheme** | Good for launch + short intent | Command-line is untrusted input; Microsoft documents injection / length hazards; argv and logs leak secrets | Win: `URL Protocol` registry; Debian: `x-scheme-handler/*` desktop MIME |

---

## Native messaging (recommended)

### Protocol (Chromium / Mozilla / Edge — same wire format)

Documented consistently by Mozilla (WebExtensions), Chromium source articles, and Microsoft Edge:

1. Extension declares `nativeMessaging` and calls `runtime.connectNative(name)` (persistent port) or `runtime.sendNativeMessage(name, msg)` (one-shot host process).
2. Browser starts the registered host; messages are **UTF-8 JSON**, each preceded by a **native-endian uint32 length**.
3. Limits (same across these sources): host→browser max **1 MiB**; browser→host max **4 GiB**.
4. Host receives the caller origin as a CLI argument (typically `chrome-extension://<id>/`), so the host can distinguish among multiple allowlisted extensions.
5. On Windows, hosts may also receive `--parent-window=<hwnd>` for parenting native UI.

Sources: [MDN Native messaging](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/Native_messaging), [Chromium `nativeMessaging.html` (src)](https://chromium.googlesource.com/chromium/src.git/+/62.0.3178.1/chrome/common/extensions/docs/templates/articles/nativeMessaging.html), [Microsoft Edge native messaging](https://learn.microsoft.com/en-us/microsoft-edge/extensions/developer-guide/native-messaging).

### Host manifest (allowlist)

Chromium-family hosts use:

```json
{
  "name": "com.phlegethon.bridge",
  "description": "Phlegethon Download Manager bridge",
  "path": "/absolute/or/windows/relative/path/to/phlegethon-nmh",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://<stable-extension-id>/"]
}
```

- `type` is only `"stdio"`.
- **No wildcards** in `allowed_origins` (Chromium article).
- Mozilla’s parallel model uses `allowed_extensions` (add-on IDs) instead of `allowed_origins`; useful as a design check, not required for a Chrome Companion. ([MDN Native manifests](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/Native_manifests))

### Registration locations (Win11 + Debian)

Installer must register **product-specific** locations. Chromium source docs distinguish Google Chrome vs Chromium paths; Edge documents Chrome/Chromium registry fallbacks.

**Windows (manifest path in registry default value):**

| Product | Registry key family |
| --- | --- |
| Chrome | `…\SOFTWARE\Google\Chrome\NativeMessagingHosts\<name>` |
| Chromium | `…\SOFTWARE\Chromium\NativeMessagingHosts\<name>` |
| Edge (optional later) | `…\SOFTWARE\Microsoft\Edge\NativeMessagingHosts\<name>` |

Prefer `HKEY_CURRENT_USER` for per-user installs; also support `HKEY_LOCAL_MACHINE` for machine-wide. Browsers typically search 32-bit then 64-bit views. (Chromium article; [Edge native messaging](https://learn.microsoft.com/en-us/microsoft-edge/extensions/developer-guide/native-messaging))

**Linux / Debian (fixed paths; absolute `path` in JSON):**

| Scope | Chrome | Chromium |
| --- | --- | --- |
| System | `/etc/opt/chrome/native-messaging-hosts/<name>.json` | `/etc/chromium/native-messaging-hosts/<name>.json` |
| User | `~/.config/google-chrome/NativeMessagingHosts/<name>.json` | `~/.config/chromium/NativeMessagingHosts/<name>.json` |

User-level dirs live under the browser user-data tree (`NativeMessagingHosts/`). ([Chromium article](https://chromium.googlesource.com/chromium/src.git/+/62.0.3178.1/chrome/common/extensions/docs/templates/articles/nativeMessaging.html); [chromium.org user-data pointer](https://www.chromium.org/user-experience/user-data-directory/))

**Implication for Phlegethon:** the WinUI and Debian installers (or a shared post-install step) must write **both** Chrome and Chromium registrations on each OS so packaged Chromium on Debian and Chrome on Windows both work without a special build.

### Recommended runtime shape

```
Chrome Companion (service worker)
        │  connectNative / sendNativeMessage
        ▼
phlegethon-nmh  (stdio host; short-lived or port-lifetime)
        │  local IPC (named pipe on Win11 / Unix socket on Debian)
        ▼
Platform Shell + Download Core
```

- Prefer **`connectNative`** when the companion may send a burst of intents or needs status acks; use **`sendNativeMessage`** for single-shot enqueue.
- The NMH should **not** embed Download Core logic; it validates the envelope and hands off to the already-running shell (or starts it).
- Content scripts must not call native messaging directly (MDN / Edge): route via the extension service worker.

### Message envelope (spec sketch — lock in hand-off)

Versioned JSON, small by default:

```json
{
  "v": 1,
  "op": "enqueue",
  "request_id": "uuid",
  "page_url": "https://…",
  "media_url": "https://…|null",
  "intent": "probe_then_confirm",
  "referrer": "https://…|null",
  "session": null
}
```

- `intent`: at least `probe_then_confirm` (default product flow) and `enqueue_direct` only if the spec later allows single-file HTTP downloads without probe.
- `session`: optional opaque object (see security). Absent for MVP companion design is fine; the field should exist so auth work does not invent a second transport.
- Host replies with `{ "v": 1, "request_id": "…", "ok": true|false, "error": "…" }` within the 1 MiB limit.

---

## Why not extension-facing loopback HTTP

Web sites reaching `localhost` / private IPs is the attack class Local Network Access (LNA) targets: CSRF against routers and local services, fingerprinting, etc. ([MDN Local network access](https://developer.mozilla.org/en-US/docs/Web/Security/Defenses/Local_network_access); [WICG Local Network Access](https://wicg.github.io/local-network-access/))

LNA facts that matter for a bridge:

- Requests from **public** address space into **loopback** / **local** are gated by `loopback-network` / `local-network` permissions (secure contexts).
- Even with permission, a desktop HTTP server on `127.0.0.1` is reachable by **any** local process and historically by any page that can induce a request — so you still need strong auth, CSRF defenses, and careful CORS.
- Extension pages are not the same as random websites, but a loopback API invites “drive-by” clients and future footguns (other browsers, bookmarks, malware).

**Allowed internal use:** after the NMH authenticates the extension, the shell may expose a **localhost-only** control channel for the NMH/UI only (token in a user-private file mode `0600`, bind `127.0.0.1`, no CORS for arbitrary origins). That channel is **not** the Companion contract.

---

## Custom URI schemes (secondary)

### Windows 11

Register under `HKEY_CLASSES_ROOT` (or HKCU/HKLM `Software\Classes`) with `URL Protocol` and `shell\open\command` quoting `"%1"`. Microsoft warns: treat argv as hostile; spaces/quotes/backslashes can split arguments; do not trust percent-decoding consistency across launchers. ([Registering an Application to a URI Scheme](https://learn.microsoft.com/en-us/previous-versions/windows/internet-explorer/ie-developer/platform-apis/aa767914(v=vs.85)))

### Debian

Advertise `MimeType=x-scheme-handler/phlegethon;` on a `.desktop` entry and associate via mime apps. ([freedesktop Shared MIME-info — URI scheme handlers](https://specifications.freedesktop.org/shared-mime-info/latest-single/); [Desktop Entry keys](https://specifications.freedesktop.org/desktop-entry/latest/recognized-keys.html); [mime-apps](https://specifications.freedesktop.org/mime-apps/latest-single/))

### Role in Phlegethon

Use only for:

- Cold-start / focus the desktop app with a **short** `page_url` (or opaque job id that the running app already knows).

**Never** put cookies, Authorization headers, or full `Cookie` jars in a custom URI. They appear in process lists, shell history, crash dumps, and OS “open with” UI.

---

## Security model (companion designed, deferred)

| Boundary | Rule |
| --- | --- |
| Extension identity | Pin `allowed_origins` to the published Companion ID(s); update installer when store ID stabilizes |
| Host identity | Only OS installers write NMH manifests/registry; treat user-writable host paths as trusted-code equivalent |
| Privilege | Companion may propose URLs + optional session material; **Download Core still runs Probe-then-Confirm** — companion must not silently start bulk channel downloads |
| Session material | Optional, explicit user gesture (“Send with cookies”); scope to the Launch Site origin; TTL; never log; never URI-scheme; prefer cookie names/values needed for extractor, not full browser profile dump |
| Payload size | Keep companion→host messages well under 1 MiB responses; reject oversized `session` blobs |
| Least transport | Native messaging for data; custom scheme only for launch; no public loopback |
| Cross-browser | Installer registers Chrome + Chromium paths; Edge later via Microsoft registry keys if needed |

**Limitation note:** Store distribution policy, exact MV3 permission strings for cookie access, and Chrome Web Store packaging rules are primarily documented on Google-owned pages. This research deliberately does **not** cite those. The hand-off spec should treat “Companion obtains cookies for origin X under user consent” as a product requirement and verify packaging against the store’s then-current policy outside this note.

---

## Put in the hand-off spec vs defer

### Lock in the hand-off spec

- Companion is **designed** but **not required** for desktop MVP (paste/drop remains sufficient) — already charted; keep.
- **Transport:** native messaging (`stdio` host) as the sole Companion↔desktop data path.
- **Host name** convention (e.g. `com.phlegethon.bridge`) and **v1 message envelope** (`enqueue` / ack).
- **Installer obligations:** register NMH for Chrome **and** Chromium on Win11 + Debian; host binary path absolute on Linux.
- **Security:** exact-origin allowlist; no session in custom URIs; Probe-then-Confirm still mandatory after companion hand-off.
- Optional: `phlegethon:` (or similar) scheme for launch/focus only.

### Defer (do not block desktop MVP or reopen transport)

- Shipping the Companion extension itself.
- Exact cookie / header export schema and Launch Site auth UX (map fog: “Auth/cookie handling when a Launch Site needs a logged-in session”).
- Extension-facing loopback HTTP/WebSocket API.
- Firefox / Edge companions (same wire protocol is portable; product decision later).
- Whether NMH is a thin forwarder vs embeds a tiny queue when the shell is offline (implementation detail; prefer thin forwarder + durable intent file if needed).
- Store IDs, signing, and dual-store `allowed_origins` lists until distribution is chosen.

---

## Sources (allowed)

| Source | Use |
| --- | --- |
| [MDN: Native messaging](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/Native_messaging) | Parallel WebExtensions model, protocol, permissions, connect vs send |
| [MDN: Native manifests](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/Native_manifests) | Manifest fields, Linux/Windows locations (Mozilla) |
| [Chromium src: nativeMessaging.html](https://chromium.googlesource.com/chromium/src.git/+/62.0.3178.1/chrome/common/extensions/docs/templates/articles/nativeMessaging.html) | Host manifest, `allowed_origins`, Chrome vs Chromium paths, wire protocol |
| [Microsoft Edge: Native messaging](https://learn.microsoft.com/en-us/microsoft-edge/extensions/developer-guide/native-messaging) | Same protocol; Windows registry search order including Chromium/Chrome fallbacks |
| [MDN: Local network access](https://developer.mozilla.org/en-US/docs/Web/Security/Defenses/Local_network_access) | Loopback/local permission model vs public sites |
| [WICG: Local Network Access](https://wicg.github.io/local-network-access/) | Spec for address spaces and permissions |
| [Microsoft: Registering a URI scheme](https://learn.microsoft.com/en-us/previous-versions/windows/internet-explorer/ie-developer/platform-apis/aa767914(v=vs.85)) | Win custom protocol registration + security warnings |
| [freedesktop: Shared MIME-info](https://specifications.freedesktop.org/shared-mime-info/latest-single/) | `x-scheme-handler/*` on Linux |

Google Chrome developer docs / policy pages were **not** used. Where a fact is only published there (e.g. Chrome Web Store distribution rules), this note states the gap instead of citing Google.
