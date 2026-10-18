# ADR 0007: Keep proxy credentials in `secrets.json`, outside `jobs.json`

- Status: accepted
- Date: 2026-10-18
- Finding IDs: M-24 (see `docs/bugs.md`)

## Context

The global proxy is stored in `jobs.json` as one URL. A URL with `user:password@` in it was written in plain text, so any process that can read the user's data directory can read the password. The same file is copied with the Library Root and kept as a `.corrupt-<timestamp>` copy when it cannot be parsed. The command-line leak of the password was fixed earlier. The yt-dlp and HTTP paths pass the URL through the environment and the HTTP client, not through argv.

## Decision

1. `jobs.json` stores the proxy URL with `user[:password]@` removed.
2. The full URL is stored in `.phlegethon/secrets.json`, beside `jobs.json`, as `{"version": 1, "proxy_url": "..."}`. The file is written with `fsutil::write_atomic_private`, which creates the temp file with mode 0600 before any data is written, so the mode also holds after the rename.
3. `save_queue` writes `secrets.json` first and `jobs.json` second. When the proxy has no credentials, `secrets.json` is removed.
4. `load_queue` restores credentials only when the stored URL, with credentials removed, equals the URL in `jobs.json`. A credential is therefore never used for another proxy host, port or scheme.
5. A missing `secrets.json` loads the proxy without credentials and is not an error. An unreadable one adds a warning, with no content in it, and the proxy loads without credentials.
6. A `jobs.json` from before this change keeps its inline credentials when loaded. Its first save moves them to `secrets.json`.
7. A job's `last_error` that echoes the full proxy URL is saved with the credentials removed. The proxy error messages in `http.rs` and `doh_dns.rs` print the URL without credentials.

## Why

The OS keyring would give encryption at rest, but it adds a dependency and a service that is not always present on Void setups. A file the user owns with mode 0600 gives the same protection the user's cookie files and other config already get. Binding the credential to the exact proxy identity keeps a stale or edited file from sending a password to a host the user did not choose.

## Consequences

- The password is still plain text on disk. It is protected by the file mode only. Anyone who can read as the user, or who gets a copy of `.phlegethon/`, can read it.
- The legacy `.corrupt-<timestamp>` copy of a `jobs.json` that still had inline credentials keeps them.
- A crash between the two writes is safe. The secret is written first and matched by URL on load.
- The in-memory `ProxySettings` still holds the full URL, so the yt-dlp and HTTP paths are unchanged.
- A keyring-backed store could replace `secrets.json` later without changing `jobs.json`.
