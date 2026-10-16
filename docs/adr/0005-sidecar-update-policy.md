# ADR 0005: Update policy for the Extractor Toolchain and Extractor Plugins

- Status: accepted
- Date: 2026-10-16
- Finding IDs: M-31, M-32, M-33, M-35, M-42, L-26, L-27, L-30 (see `docs/bugs.md`)

## Context

Launch Site extractors break whenever a site changes, so a yt-dlp that is never updated
stops working. The update manager existed, but `check_on_launch` defaulted to off, the
nightly channel was defined and never used, and "apply when idle" was only a doc comment:
`DeferredUntilIdle` was never produced and nothing applied a deferred update later.
Pins, rollbacks and settings also interacted badly. A pin to any version but the latest
could not install, a rollback was undone by the next launch check, a plugin pin older than
the bundled plugin was overwritten at every start, and the settings lived in the Library
Root queue although the sidecar is shared by every Library Root.

## Decision

1. **Check on launch is on by default.** The launch check runs on its own thread and
   holds the Core lock only to read and to swap, never during the network transfer.
2. **Updates apply only when idle.** When a transfer, probe or quality lookup is running,
   the update is not applied. Core stores a deferred flag in the sidecar state and returns
   `DeferredUntilIdle`. The shell calls `apply_deferred_sidecar_updates` every 30 seconds,
   which does nothing unless the flag is set and the app is idle. A manual Install that is
   blocked sets the same flag. A failed deferred apply clears the flag, so a broken source
   is not retried on every poll. `DeferredUntilNextLaunch` is removed.
3. **Channel setting.** `stable` (default) follows `yt-dlp/yt-dlp`; `nightly` follows
   `yt-dlp/yt-dlp-nightly-builds`. Stable stays the default because a nightly build has
   had less testing. Users who want faster fixes opt in.
4. **A pin names one release.** The GitHub source resolves a toolchain pin from
   `releases/tags/<tag>`, so a pin to an older version installs. A pin or latest version
   that the source does not list, or a plugin entry with no valid `sha256`, is reported as
   a `lookup_error` and not offered as an update.
5. **Versions compare numerically.** Dot-separated components compare as numbers, a
   pre-release sorts below its release, and an older catalog "latest" is not an update.
   An explicit pin may still move to a lower version.
6. **Rollback pins the restored version**, so the next launch check does not reinstall
   the release the user just left. `ensure_bundled_plugins` keeps an installed plugin
   whose version equals `plugin_pin`, even when it is older than the bundled one.
7. **A partial apply restores the toolchain.** The toolchain snapshot is saved to disk as
   soon as the toolchain is swapped; if the plugin install then fails, the previous
   toolchain is put back and the error says so.
8. **Settings are app-wide.** They live in `<sidecar_root>/.phlegethon/sidecar-settings.json`
   and no longer in the Library Root `jobs.json`. A file that does not parse is moved to
   `*.corrupt` and defaults apply.

## Consequences

- A user who had turned checking off in an earlier build gets the new default once,
  because the old value stored in `jobs.json` is not migrated. Pins set in an earlier build
  are likewise dropped once. Both are easy to set again in the Toolchain panel.
- Existing queues still load: the old `sidecar_update_settings` field is ignored.
- Update checks use the unauthenticated GitHub API (60 requests an hour). One lookup per
  launch, plus the manual Check button, stays well below that.
