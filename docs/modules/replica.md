# farder-replica (crates/farder-replica)

Keeps a verified copy of a Farder server's database on another machine, so friends can read the history while the
host's computer is off. Design and status: `docs/mesh-hosting-status.md`.

## Program

`farder-replica [--server host:port] [--invite LINK] [--dir DIR] [--name NAME] [--interval SECS] [--once] [--cert-fp HEX]`

- First run needs `--invite`: the link from the app's invite dialog (`https://farder.xyz/join/...`), a
  `farder://host:port/CODE` link, or a bare code. When the link carries the address, `--server` can be left out.
  Relay links are refused (a replica connects directly).
- Names itself `--name` (default "Backup copy") in the member list, so members do not see an unexplained "Anonymous".
- Prints `replica key: vk_...`; the owner allows that key in Server Settings > Backup copy (the list shows each member's
  short key). Until then `--once` exits with code 3.
- Each pull writes `latest.db` (and keeps `previous.db`) only after SHA-256, SQLite integrity and history replay checks.
- Then the attachments: `files/` is made to hold exactly the files `latest.db` lists (fetched ones are checked against
  their SHA-256; a file on disk that no longer matches is fetched again; files the host redacted or deleted are removed).
  An allowed replica may fetch every file, like the owner (server: `handle_download_stream` in `connection.rs`).
- Exit codes with `--once`: 0 copied, 1 copy failed (old copy kept), 2 could not connect / not joined, 3 not allowed yet.

Serve the copy with `farder-server --read-only --db DIR/latest.db --storage-dir DIR/files --bind 0.0.0.0:PORT` (no
`--name` needed: the name the host saved is used).

## Library (`farder_replica`)

| Item | What it does |
|---|---|
| `Session::connect(addr, cert_fp, key, invite, setup_token)` | QUIC connect + authenticate. |
| `Session::join_history(key, invite)` | Become a member of the signed history (same steps as the browser guest). |
| `pull_once(session, out_dir) -> CopyInfo` | One verified snapshot pull; `PullError::NotAllowed` until the owner allows it. |
| `verify_copy(path, size, sha)` | The checks a copy must pass before it replaces `latest.db`. |
| `load_or_create_key(path)` | The replica's own identity key (file mode 0600 on Unix). |
| `sync_files(session, out_dir) -> FileSync` | Make `out_dir/files` match the attachments `latest.db` lists (fetch, check, remove). |
| `Session::set_name(key, name)` | Signed profile with that name (what the member list shows). |
| `Session::download(file_id)` | One file on its own stream; `None` when the server says "not available". |
| `invite_from_link(s) -> (Option<SocketAddr>, code)` | Parse the three invite forms above. |
| `state_path(dir)` | Where the "joined" flag is kept. |

## Promotion (a copy becomes the host)

Design: `docs/specs/2026-11-27-promotion-design.md`. Step 1 (server side only) is built: the owner signs a `HostTerm`
(`farder-crypto::host_term`) and sends `PromoteToHost { term }` to a running read-only copy, which becomes writable
**without a restart**. `GetHostTerm` returns the stored term on both host and copy (`None` = original host).

The copy accepts only if ALL hold (each refusal is a plain reason and the copy stays read-only):

1. it is read-only (a writable host refuses: "already the writable host");
2. the term verifies against the CURRENT owner (genesis owner, or the owner after a completed recovery takeover, as the fold knows it);
3. `server_id` is this server's;
4. `term` is higher than the stored term (none stored = 0);
5. `host_key` is this server's own identity: the 32 bytes in `--data-dir/server_id` (the TLS certificate is regenerated each start, so it cannot be the identity). Give each copy its own `--data-dir`; a restored backup carries the original's `server_id`;
6. `after_event` is the hash of the newest event in the copy's log.

On accept the term is saved in `server_settings` (`host_term`), SQLite `query_only` is lifted, the server turns writable, the
retention/bot/widget jobs start, and one `PROMOTED:` line is logged. Not restarted by promotion: automatic backups
(`--backup-dir`) and `--relay` registration begin at the next start.

Start-up: a database with a stored term whose `host_key` is not this machine's identity starts READ-ONLY even without
`--read-only` (it was replaced). A promoted copy restarted from its `--db` with the same `--data-dir` stays writable with the
same term. Not yet: asking peers for a higher term, client-side fencing, the UI, and a way for the app to learn the copy's `host_key`.

Test: `cargo test -p farder-replica --test replica promotes` (real server binary: promote, every refusal, restart).

## Tests

`cargo test -p farder-replica` (unit tests for the link forms; integration tests against a real server, including the
read-only copy keeping the host's name). End to end with the app: `bash scripts/selftest/ubuntu-suite.sh failover`.
