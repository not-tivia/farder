# farder-replica (crates/farder-replica)

Keeps a verified copy of a Farder server's database on another machine, so friends can read the history while the
host's computer is off. Design and status: `docs/mesh-hosting-status.md`.

## Program

`farder-replica [--server host:port] [--invite LINK] [--dir DIR] [--name NAME] [--interval SECS] [--once] [--announce HOST:PORT] [--cert-fp HEX]`

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
- `--announce HOST:PORT`: the address where a read-only copy server for this folder can be reached. After each successful
  pull (and file sync) the replica sends `AnnounceCopy { address }` to the host (`Session::announce_copy`), which keeps the
  newest 8 in `server_settings.known_copies`. A host that does not know the request, or refuses it, makes the replica log
  "could not announce ..." ONCE and carry on: the pull never fails because of it. Only the owner or an allowed replica may announce.
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
The owner's app builds the term from `GetPromotionInfo` (the copy's host key and log head), signs it, and sends `PromoteToHost` to the copy.

Step 2 (server side): `NotifyHostTerm { term }` (any member, also accepted by a read-only copy) lets a REPLACED host learn the
newer term: same signature, `server_id` and "higher term" checks, but `host_key` must NOT be its own identity. It stores the term,
goes read-only at runtime (and stays so after a restart), logs `REPLACED:`, and refuses writes with "this server moved to a new
host (<addresses>)". The app sending it is separate (app side). Test: `a_replaced_host_learns_the_newer_term_from_a_member_and_goes_read_only`.

Step 4 ("asks its copies"): the host also asks the copies by itself. Copies announce their address (`--announce`, above); on
start and every 10 minutes while it is the host (not read-only) the server (`promotion::spawn_copy_probe`) asks each known copy for
its term (each in parallel, 5 s limit; an offline copy is skipped quietly; start-up never waits). A copy has no login for the
host, so the question is the pre-auth `ClientFrame::GetHostTerm`, answered with `ServerFrame::HostTerm`. A term that passes
`notify`'s checks and names ANOTHER host key replaces the host exactly like `NotifyHostTerm` (term stored, read-only, `REPLACED`
in the log). Tests: `a_copy_announces_itself_and_a_restarted_old_host_asks_it_and_learns_it_was_replaced`,
`a_host_whose_copy_is_offline_starts_normally_and_stays_writable`, `a_pull_still_finishes_when_the_host_refuses_the_announcement`.

The copy accepts only if ALL hold (each refusal is a plain reason and the copy stays read-only):

1. it is read-only (a writable host refuses: "already the writable host");
2. the term verifies against the CURRENT owner (genesis owner, or the owner after a completed recovery takeover, as the fold knows it);
3. `server_id` is this server's;
4. `term` is higher than the stored term (none stored = 0);
5. `host_key` is this server's own identity: the 32 bytes in `--data-dir/server_id` (the TLS certificate is regenerated each start, so it cannot be the identity). Give each copy its own `--data-dir`; a restored backup carries the original's `server_id`;
6. `after_event` is the hash of the newest event in the copy's log.

On accept the term is saved in `server_settings` (`host_term`), SQLite `query_only` is lifted, the server turns writable, the
retention/bot/widget jobs start (plus automatic backups, if `--backup-dir` was given: one at once, then every
`--backup-interval`; a copy never promoted writes none), and one `PROMOTED:` line is logged. Not restarted by
promotion: `--relay` registration begins at the next start.

Start-up: a database with a stored term whose `host_key` is not this machine's identity starts READ-ONLY even without
`--read-only` (it was replaced). A promoted copy restarted from its `--db` with the same `--data-dir` stays writable with the
same term. Since built: asking the copies for a higher term (step 4, above), the owner's UI (Server Settings > Backup copy,
which learns the copy's `host_key` from `GetPromotionInfo`), and the apps following the move (`ubuntu-suite.sh promote`).
Not yet: `--relay` registration on promotion (the relay routes by the
old host's own id, so a promoted copy cannot take over relay invite links).

Test: `cargo test -p farder-replica --test replica promotes` (real server binary: promote, every refusal, restart).

## Tests

`cargo test -p farder-replica` (unit tests for the link forms; integration tests against a real server, including the
read-only copy keeping the host's name). End to end with the app: `bash scripts/selftest/ubuntu-suite.sh failover`.

## Promotion, app side (2026-11-27)

The owner uses Server Settings > Backup copy > "Make a copy the host" (`promote_copy`). Every app keeps the highest
verified hosting term per server and fences on it: the new host first, and a replaced host only read from. See
`get_moved_host` in `docs/modules/tauri-commands.md`. After promotion, stop `farder-replica` for that folder: the
promoted server owns `latest.db`. The replica refuses anyway (#28): if `latest.db` holds a hosting term
(`held_term`), `pull_once` returns `PullError::Promoted(term)` before downloading anything (and again at install
time), and the program exits with code **4** (`--once`) or stops.

## The old host follows the new host

A replaced host (it logged `REPLACED:`) keeps its own database untouched as an ARCHIVE: it holds the messages the copy never
got (its "tail"), and serving or promoting it would fork the history. To follow the new host instead, on the old machine:

1. stop the old `farder-server`;
2. `farder-replica --server <new host> --invite <link from the new host> --dir FOLLOW` (the owner allows its key in
   Server Settings > Backup copy, like any copy), then keep it pulling (`--interval`);
3. serve `FOLLOW/latest.db` read-only with the old machine's OWN `--data-dir` (its identity):
   `farder-server --read-only --db FOLLOW/latest.db --storage-dir FOLLOW/files --data-dir <old data dir>`.

If the new host is lost later, the owner can make the old machine the host again from that copy (term 2): the new host's
messages are kept and the old tail does not come back. Test: `a_replaced_old_host_follows_the_new_host_and_can_be_made_the_host_again_without_forking`
(real binaries: archive hash unchanged, followed copy read-only, promote-back accepted, no tail). A running copy server does
not yet pick up the replica's later pulls without a restart (issue #46).
