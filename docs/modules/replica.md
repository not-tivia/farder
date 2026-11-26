# farder-replica (crates/farder-replica)

Keeps a verified copy of a Farder server's database on another machine, so friends can read the history while the
host's computer is off. Design and status: `docs/mesh-hosting-status.md`.

## Program

`farder-replica [--server host:port] [--invite LINK] [--dir DIR] [--interval SECS] [--once] [--cert-fp HEX]`

- First run needs `--invite`: the link from the app's invite dialog (`https://farder.xyz/join/...`), a
  `farder://host:port/CODE` link, or a bare code. When the link carries the address, `--server` can be left out.
  Relay links are refused (a replica connects directly).
- Prints `replica key: vk_...`; the owner allows that key in Server Settings > Backup copy (the list shows each member's
  short key). Until then `--once` exits with code 3.
- Each pull writes `latest.db` (and keeps `previous.db`) only after SHA-256, SQLite integrity and history replay checks.
- Exit codes with `--once`: 0 copied, 1 copy failed (old copy kept), 2 could not connect / not joined, 3 not allowed yet.

Serve the copy with `farder-server --read-only --db DIR/latest.db --bind 0.0.0.0:PORT` (no `--name` needed: the name
the host saved is used).

## Library (`farder_replica`)

| Item | What it does |
|---|---|
| `Session::connect(addr, cert_fp, key, invite, setup_token)` | QUIC connect + authenticate. |
| `Session::join_history(key, invite)` | Become a member of the signed history (same steps as the browser guest). |
| `pull_once(session, out_dir) -> CopyInfo` | One verified snapshot pull; `PullError::NotAllowed` until the owner allows it. |
| `verify_copy(path, size, sha)` | The checks a copy must pass before it replaces `latest.db`. |
| `load_or_create_key(path)` | The replica's own identity key (file mode 0600 on Unix). |
| `invite_from_link(s) -> (Option<SocketAddr>, code)` | Parse the three invite forms above. |
| `state_path(dir)` | Where the "joined" flag is kept. |

## Tests

`cargo test -p farder-replica` (unit tests for the link forms; integration tests against a real server, including the
read-only copy keeping the host's name). End to end with the app: `bash scripts/selftest/ubuntu-suite.sh failover`.
