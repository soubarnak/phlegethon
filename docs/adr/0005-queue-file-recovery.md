# Recover from a bad queue file instead of refusing the Library Root

Status: accepted

`jobs.json` (and `archive.json`) are written atomically, but a crash on an older build, a manual edit or a downgrade can still leave a file Core cannot read. Before this decision a single bad byte made `set_library_root` fail, and the shell then kept a Library Root that Core had rejected (finding H-8).

## Decision

- `set_library_root` loads everything into locals and commits only when the load succeeds. It never leaves a half-swapped queue (M-4) and refuses while any transfer is active (H-7).
- A queue or archive file that cannot be parsed, or has an unsupported version, is moved to `<name>.corrupt-<unix seconds>`. Core starts with an empty queue or archive and returns the reason in `LibraryRootReport::warnings`. The root is set.
- A single queue row that fails to parse or validate is skipped with a warning. The original file is copied to `jobs.json.corrupt-<unix seconds>` first, because the next save rewrites `jobs.json` without that row.
- Restore validates each row. The destination must be absolute, free of `.` and `..`, not the Library Root or one of its ancestors, and end in a folder named after the job id. The filename must be one path component. Duplicate job ids and relative cookie files are dropped. An Active Job Limit below 1 becomes the default (M-8, L-10, L-11).
- Only an I/O error that leaves a file's state unknown (for example, a directory where `archive.json` should be) is returned as an error.

## Why

The destination check is what keeps Discard safe: `finalize_discard` deletes a job's destination recursively, so a hand-edited or legacy row must not be able to name a shared folder. Every destination Core creates ends in the job id, so the rule never rejects a row Core wrote.

Moving the file aside, not deleting it, keeps the user's data available for inspection or manual repair.

## Consequences

Shells must show `LibraryRootReport::warnings` to the user. Downgrading to an older build moves a newer queue file aside instead of failing.
