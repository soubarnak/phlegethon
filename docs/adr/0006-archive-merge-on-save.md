# Save the Download Archive by merging, under a lock

Status: accepted

`archive.json` used to be written from the in-memory copy alone. Two writers on one Library Root (two app instances, or a second Core in a tool) each held a snapshot, so whichever saved last erased the other's entries (finding L-12). A failed retry could also overwrite the Completed entry for a media id, so the probe stopped marking the item as downloaded (finding M-12).

## Decision

- `DownloadArchive` journals its changes (entries recorded, ids removed) since the last load or save.
- `save` takes an exclusive lock on `.phlegethon/archive.lock`, re-reads `archive.json`, replays the journal onto what is on disk, writes the result atomically and adopts it as the in-memory state.
- A Failed record never replaces a Completed entry for the same media id. A later Completed record still replaces either.
- If `archive.json` is unreadable at save time, it is copied to `archive.json.corrupt-<unix seconds>` and the writer's own entries are written.

## Why

Merging by journal keeps removals (Remove history entry, Clear history) from being undone by a stale writer, which a plain union of the two files would do. The lock is advisory and local; it protects against Phlegethon processes, not against other programs editing the file.

`jobs.json` is not merged. Only one Core owns a Library Root's queue.

## Consequences

`DownloadArchive::save` takes `&mut self`. History shown by one instance picks up another instance's entries at its next save, not live.
