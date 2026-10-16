# ADR 0004: Persist the segment table of a Segmented Transfer

- Status: accepted
- Date: 2026-10-16
- Finding IDs: H-4, M-15 (see `docs/bugs.md`)

## Context

A Segmented Transfer writes four byte ranges of one `.part` file at independent
offsets. The file length is therefore the furthest offset written, not a contiguous
prefix. Crash recovery (`reconcile_job`) took that length as resumable progress and
appended from there, leaving zero-filled holes. A segment that ended early with a clean
EOF was not detected either. Pause avoided the problem by deleting the `.part` and
restarting from zero, which contradicts Resumable Transfer.

## Decision

1. `jobs.json` stores a `segments` table per Segmented Transfer: `{start, end, written}`
   for each range, where `written` counts the contiguous bytes from `start` that are
   on disk. The field has a serde default and is omitted when empty, so older queues
   load and single-connection jobs are unchanged.
2. A new segmented transfer persists its table before it creates the `.part`, then
   preallocates the `.part` to the object size. A `.part` of the full size without a
   table cannot occur.
3. Segment workers advance a per-segment counter only after `sync_data`, every 4 MiB
   and when the fetch ends, so a saved table never claims bytes that a power loss can
   take back. The dispatch tick saves the table at most once a second; pause, failure
   and completion save it immediately.
4. A segment that ends before its range is complete fails with an error. The worker
   keeps what it wrote.
5. Pause, failure and Mirror Set failover keep the `.part` and the table. The next
   plan requests `start + written..end` for each unfinished segment, with the stored
   validator as `If-Range`. If every segment is complete the job is finished without a
   request.
6. `reconcile_job` takes progress from the table. If the table is not a gap-free cover
   of the object or the `.part` length differs from the object size, the table is
   dropped and the job restarts from zero.
7. A resumed segment that gets the whole body, or whose reply carries different
   validators, means the object changed: the `.part` and table are dropped and the job
   restarts. Only a fresh segmented request that gets a whole body marks the job
   `range_ignored`.
8. Retry clears the table, as it clears `bytes_received`.

## Consequences

- Pause no longer discards progress, and a crash loses at most the unsynced tail of
  each segment.
- The `.part` is sparse until the transfer ends. Free-space checks still use the object
  size.
- A job whose servers never send a validator resumes without `If-Range`. This matches
  single-connection resume (ADR 0003).
