# Running Phlegethon on niri and other tiling compositors

Phlegethon is a GTK/WebKit window. It needs no desktop environment, but three things in a bare compositor session decide whether the folder chooser and notifications work. The window rules at the end are optional.

Nothing here has been run in a real niri session. Check each item with the command given.

## Window class (app_id)

Window rules match the Wayland `app_id`. Tauri does not set a GTK application id for this app (`enableGTKAppId` is off), so the app_id is expected to be the binary name, `phlegethon`. Confirm it:

```sh
niri msg windows        # niri
swaymsg -t get_tree     # sway
hyprctl clients         # Hyprland
```

The reverse-DNS identifier `com.phlegethon.app` is the Tauri bundle identifier and the AppStream id. It is not the app_id. If your compositor shows a different value, use that value in your rules.

## Folder chooser: a portal backend

**Settings, Library folder** and **Save to...** open a native folder chooser through `xdg-desktop-portal` (the `FileChooser` interface). The portal needs a backend for your session, and a bare compositor does not install one.

1. Install `xdg-desktop-portal` (an xbps dependency of Phlegethon) and a backend that implements `FileChooser`: `xdg-desktop-portal-gtk` for most setups, `xdg-desktop-portal-kde` on KDE, or `xdg-desktop-portal-gnome` (which needs `nautilus` for the chooser).
2. Tell the portal which backend to use. niri ships a `niri-portals.conf` that prefers the GNOME backend for most interfaces. Without GNOME that leaves `FileChooser` unanswered, so override it in `~/.config/xdg-desktop-portal/niri-portals.conf`:

   ```ini
   [preferred]
   default=gtk
   org.freedesktop.impl.portal.FileChooser=gtk
   ```

   Other compositors use `<XDG_CURRENT_DESKTOP>-portals.conf`, or `portals.conf`, in the same directory. See `portals.conf(5)`.
3. Start the portal from the session. Most compositors do this through D-Bus activation. If the chooser never appears, run `systemctl --user status xdg-desktop-portal` or start `/usr/libexec/xdg-desktop-portal -r` by hand and read its log.

Without a working portal Phlegethon falls back to `zenity`, then `kdialog`, and finally asks you to type the path. That fallback opens a separate dialog window; see the floating rule below.

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
    match app-id="^zenity$"
    open-floating true
}
```

For sway, the equivalent floating rule is `for_window [app_id="zenity"] floating enable`. Keep a Phlegethon window at least 700 px wide for the batch bar and toolbar.
