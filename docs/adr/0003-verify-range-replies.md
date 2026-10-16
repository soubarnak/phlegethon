# ADR 0003: Verify Range replies and validate resumes with If-Range

- Status: accepted
- Date: 2026-10-16
- Finding IDs: B-2, H-5, M-14, L-13 (see `docs/bugs.md`)

## Context

A Resumable Transfer sends `Range: bytes=N-` and appends the reply to the `.part`
file. The HTTP backend returned `Ok` for every status below 400 and never looked at
`Content-Range`, so a server that ignored the Range header (status 200, whole body)
produced a corrupt file that completion accepted because it only tested
`on_disk >= total`. A resume could also continue an object that had changed on the
server, and mirror failover appended at an offset that lagged the bytes the failed
attempt had written.

## Decision

1. `FetchResult` reports the reply `status`, the parsed `Content-Range`, and the `ETag`
   and `Last-Modified` validators. `total_length` is the object size: the
   `Content-Range` total of a 206 reply, else the `Content-Length` of a 200 reply.
2. `HttpBackend::fetch_conditional` takes an optional `If-Range` validator. The
   production backend does not stream the body of a Range request that was answered
   with anything but 206; Core restarts from zero instead.
3. Core requires a 206 whose `Content-Range` starts at the requested offset. On any
   other reply it truncates the `.part`, clears the stored validators, and restarts
   with a plain request. When a Segmented Transfer meets a server that ignores Range,
   the job is marked `range_ignored` and falls back to one connection. Each segment
   writer is capped at its range length.
4. The `Job` stores `etag`, `last_modified` and `range_ignored` in `jobs.json` (all
   with serde defaults, so older queues still load). Resume sends the `ETag`, else the
   `Last-Modified`, as `If-Range`. Validators that differ from the stored ones also
   restart from zero.
5. The resume offset is the `.part` length, not the persisted counter. A failed
   attempt syncs its progress before mirror failover. A `.part` whose length equals
   the known total is completed without a request, and one longer than the total is
   truncated and restarted.
6. Completion compares sizes with `==`. A larger file fails the job instead of
   completing.

## Consequences

- A server that ignores Range costs one extra request on resume, not a corrupt file.
- Backends that do not report a status (test doubles) are trusted to have honoured the
  Range; the production backend always reports one.
- Segment progress is still not persisted; that is a separate decision (H-4, M-15).
