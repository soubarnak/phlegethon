# Running Phlegethon on niri and other tiling compositors

Phlegethon is a GTK/WebKit window. It needs no desktop environment, but three things in a bare compositor session decide whether the folder chooser and notifications work. The window rules at the end are optional.

The app_id, the decorations and the folder dialog below were checked on niri 26.04 with the packaged 0.1.1 (2026-10-20). The rest, including the window rules, is not verified against a running session. Check each item with the command given.

## Window class (app_id)

Window rules match the Wayland `app_id`. Tauri does not set a GTK application id for this app (`enableGTKAppId` is off), so the app_id is the binary name, `phlegethon` (seen with `niri msg windows` on niri 26.04). Confirm it on your setup:

```sh
niri msg windows        # niri
swaymsg -t get_tree     # sway
hyprctl clients         # Hyprland
```

The reverse-DNS identifier `com.phlegethon.app` is the Tauri bundle identifier and the AppStream id. It is not the app_id. If your compositor shows a different value, use that value in your rules.

## Folder chooser: a portal backend

**Settings, Library folder** and **Save to...** open a native folder chooser through `xdg-desktop-portal` (the `FileChooser` interface). The portal needs a backend for your session, and a bare compositor does not install one.

1. Install `xdg-desktop-portal` (an xbps dependency of Phlegethon) and a backend that implements `FileChooser`: `xdg-desktop-portal-gtk` for most setups, `xdg-desktop-portal-kde` on KDE, or `xdg-desktop-portal-gnome` (which needs `nautilus` for the chooser).
2. Check which backend the portal picks. niri ships a `niri-portals.conf` with `default=gnome;gtk;`: the GNOME backend first, then GTK. If `xdg-desktop-portal-gnome` is installed without `nautilus`, its chooser does not work, and the portal does not fall back. To force the GTK chooser, put this in `~/.config/xdg-desktop-portal/niri-portals.conf` (optional when only the GTK backend is installed):

   ```ini
   [preferred]
   default=gtk
   org.freedesktop.impl.portal.FileChooser=gtk
   ```

   Other compositors use `<XDG_CURRENT_DESKTOP>-portals.conf`, or `portals.conf`, in the same directory. See `portals.conf(5)`.
3. Make sure the portal runs in the session. Most compositors start it through D-Bus activation. If the chooser never appears, list the session bus names with `dbus-send --session --print-reply --dest=org.freedesktop.DBus / org.freedesktop.DBus.ListNames | grep portal`, or start `/usr/libexec/xdg-desktop-portal -r` by hand and read its log.

Without a working portal Phlegethon falls back to `zenity`, then `kdialog`, and finally asks you to type the path. The portal dialog and the fallback both open as separate windows; see the floating rules below.

## Notifications: a notification daemon

Phlegethon posts desktop notifications over D-Bus (`org.freedesktop.Notifications`). A tiling compositor does not provide that service. Run a notification daemon in your session, for example `mako`, `dunst`, `fnott` or `swaync`, and start it from your compositor config. When the D-Bus service is missing the app falls back to the `notify-send` command (xbps package `libnotify`), which also needs a daemon to show anything.

Check the daemon with:

```sh
notify-send "test" "hello"
```

If nothing appears, Phlegethon notifications will not appear either.

## Suggested niri rules

Phlegethon's layout was written for a window of about 960 × 640 and has a minimum of 360 × 320. The queue table is wide and scrolls sideways in narrow columns, so give it a wide column by default and float the external pickers.

```kdl
window-rule {
    match app-id="^phlegethon$"
    default-column-width { proportion 0.66667; }
}

window-rule {
    match app-id="^xdg-desktop-portal-gtk$" title="^Choose folder"
    open-floating true
}

window-rule {
    match app-id="^zenity$"
    open-floating true
}
```

The second rule matches the `Choose folder` dialog of `xdg-desktop-portal-gtk`, which is what the folder button opens when the GTK portal backend is installed (checked on niri 26.04). For sway, the equivalent floating rules (not checked) are `for_window [app_id="xdg-desktop-portal-gtk" title="^Choose folder"] floating enable` and `for_window [app_id="zenity"] floating enable`. The KDL above passes `niri validate` on niri 26.04. That the rules float the dialog and widen the column was not checked in a live session. On niri, also set `prefer-no-csd` in your config: with it Phlegethon draws no title bar or shadow, and niri's shipped default config leaves that line commented out, so without it you probably get a GTK title bar (not checked). Keep a Phlegethon window at least 700 px wide for the batch bar and toolbar.
