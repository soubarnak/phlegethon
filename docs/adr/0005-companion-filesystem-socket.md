# ADR 0005: Companion hand-off uses a private filesystem socket

- Status: accepted
- Date: 2026-10-16
- Findings: H-18, M-48, M-49, M-50 (docs/bugs.md)

## Context

The native-messaging host (NMH) forwards each Chrome Companion hand-off to the running
Platform Shell over a local socket. The socket used `interprocess` `GenericNamespaced`, which
on Linux is the abstract namespace. Abstract sockets have no filesystem permissions and no
owner. Any process in the network namespace, including another user's, could connect and
enqueue a download with a session, and while the shell was closed any process could bind the
name and receive the NMH's frame, cookies included.

## Decision

1. The socket is a filesystem socket (`GenericFilePath`) at
   `$XDG_RUNTIME_DIR/phlegethon/companion.sock`. When `$XDG_RUNTIME_DIR` is unset or not
   absolute, it is `<data dir>/phlegethon/run/companion.sock`, where the data dir is
   `$XDG_DATA_HOME` or `$HOME/.local/share`.
2. The directory is mode 0700 and the socket mode 0600. A directory that exists with looser
   permissions is tightened; a directory owned by another uid is an error.
3. On bind, a stale socket file (a socket owned by us that refuses connections) is removed. A
   live socket means another instance is running and is an error. A path that is not a socket
   is never removed.
4. The NMH refuses to connect unless the socket file is a socket (not a symlink) whose owner
   uid equals its own. It reads its own uid from `std::fs::metadata("/proc/self")`, so the NMH
   crate needs no libc binding.
5. The receiver serves each connection on its own thread with read and write timeouts, caps
   concurrent connections, and logs and continues on accept errors.
6. The NMH rejects a caller whose `argv[1]` origin is not a `chrome-extension://` origin listed
   in an installed `com.phlegethon.bridge.json`.

## Consequences

- Other users cannot reach the socket. A process of the same user still can; the threat model
  does not cover a hostile process running as the user, who could read the cookie files and
  `jobs.json` anyway.
- Tests bind a temporary path, so they no longer collide with a running app.
- A hand-off with no `$XDG_RUNTIME_DIR` (for example a minimal session) still works through the
  data directory fallback. Both sides compute the path from the same environment variables.
- Not done: a per-install token. The owner-uid check and 0700 directory give the same
  guarantee against other users without a second secret to store and rotate.
