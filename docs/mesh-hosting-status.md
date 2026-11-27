# Where the "relay first, then mesh" goal stands (written 2026-11-15)

Plain words first, details after.

**The goal:** a server starts out on a relay (the helper machine that hides everybody's
address and passes messages along). Over time it turns into a *mesh*: the members' own
computers hold copies of the server, so it stays alive when the owner turns their PC off.

**Honest answer: the foundation is built. The mesh itself is not.**
Nothing in Farder copies a server onto a second computer yet. Today every server lives on
ONE computer (the owner's), and the relay only passes traffic through; it stores nothing.
If the owner's PC is off, the server is unreachable. (The two-client self-test uses exactly
this setup: A's PC hosts, B connects through the relay.)

## The ladder, and which rungs exist

| Step | What it means | State |
|---|---|---|
| Single host + relay | One computer hosts, the relay hides addresses | DONE and tested (self-test, 70 checks) |
| Signed log foundation ("Rung 1") | Everything that happens in a server (messages, joins, bans, invites, devices) is a signed, hash-chained event. Anyone holding the events can verify them, so a copy can't be forged | DONE (spec 2026-06-25, plus invites/approvals, attachments, takedowns) |
| Encrypted channels ("Rung 2") | A member's computer can hold a copy without being able to read encrypted channels (OpenMLS) | DONE as code, server-side tests green; the GUI path is only lightly exercised |
| **Copying the server to another computer** | A second machine follows the host and keeps a verified copy | **BUILT 2026-11-15 (steps 1-2): `FetchLog` + replicas + verified snapshots + the `farder-replica` tool; tested against the real server. Not yet: serving the copy while the host is down, client failover, promotion** |
| **Failover** | If the host is off, clients switch to a copy; a copy can take over | **NOT STARTED** |
| **Members hosting together (the real mesh)** | Several machines accept new messages and merge them | **NOT STARTED** (the log was designed so merging is possible: per-device chains, Lamport clock; never built) |
| **Relay turning into a mesh** | The server's address list grows from "relay" to "relay + member copies" | **NOT STARTED** |

Two parts that look related but are NOT the mesh: `farder-node` (an early peer-to-peer
experiment used only by a demo) and the relay (forwarding only).

## What I'd build next (in order, each small enough to test on its own)

1. **A way to ask a server for its log** ("send me every event after position N", checked
   for membership, paged). The only fetch requests that exist today are for encryption
   handshakes.
2. **A read-only backup** that follows the host: it pulls the log, checks every signature
   and chain link, keeps its own copy. Test with the two-client harness: stop A's server,
   confirm the backup still has all history and B can still read it.
3. **Failover for clients:** a server's entry can list more than one address; if the first
   is down, try the next.
4. **Promotion:** a backup can become the host (one writer at a time, so nothing has to be
   merged yet). Test: kill the host mid-chat, promote, keep chatting.
5. Only then, **several writers at once** (the true mesh), which is the hard part: merging,
   who may be a host, and abuse by a bad host. The spec already names these risks.

Steps 1-4 give "the server survives the owner turning their PC off" without needing the
hard merge problem. That is the useful milestone for a group of friends.

## Decisions I need from you before step 2

- **Who runs a backup?** My recommendation: any member can opt in with one switch ("keep a
  copy of this server on my computer"), and the owner can also name one machine as the
  always-on anchor (e.g. your home server). Not everyone, so a phone never has to.
- **Does a backup see everything?** In normal channels, yes (the host does too). In
  encrypted channels it only holds scrambled data it cannot read.
- **How much history?** Full history at first (simple). A "recent window plus archive on the
  anchor" scheme is already sketched in the spec for later.

## How it will be proven

Each step gets a scenario in `scripts/selftest/` (the two-client harness) that kills the
host and checks what the others can still do, plus Rust tests for the new log request.
Nothing here will be called working until that run passes.

## Status of steps 1-2 (built 2026-11-15)

**Step 1, `FetchLog`** (owner, or an allowed replica): pages the signed history (`after`, `limit`, with the genesis so a copy can check the
server's id). Owner/replica only, because the raw history holds every channel's messages including ones a member cannot open.

**What I found while planning step 2, and changed:** channels, roles and categories live only in the server's DATABASE, not in the
signed history, so a copy built from the log alone would lose them. So a replica copies the DATABASE as a verified snapshot, not the log.

**Step 2, replicas and `farder-replica`:**
- The owner allows a member as a replica (`SetReplica`; `ListReplicas`). Only the owner can; a replica cannot make more replicas.
- `SnapshotBegin` / `SnapshotChunk`: the server makes a consistent copy of its database (`VACUUM INTO`), announces its size and
  SHA-256, and hands it over in 1 MiB pieces. At most 4 live at once, deleted after 15 minutes.
- `farder-replica` (a program with its own key): joins with a normal invite, waits to be allowed (exit code 3 with its key printed),
  then pulls a copy every N seconds; checks the SHA-256, SQLite's integrity check and that the signed history inside replays cleanly
  and is the server it asked; only then replaces `latest.db` (keeping `previous.db`). A bad copy is thrown away and the old good one stays.
- **A replica sees everything the host sees**, including channels a member cannot open and invite codes. Run one only on a machine
  you trust as much as the host.

Tested (`cargo test -p farder-replica`, real server): refused until allowed; after allowing, the copy holds the messages and a channel
that exists only in the database; a second pull keeps `previous.db`; a corrupted file is rejected; revoking stops further copies.

**Step 3, reading the copy while the host is off (verified end to end 2026-11-26):** `farder-server --read-only --db latest.db` serves
the copy (it shows the host's name, saved in the database); each friend types the copy's address in Server Settings > Backup copy; the
owner ticks the copy machine there (shown by its short key, the one `farder-replica` prints). `bash scripts/selftest/ubuntu-suite.sh
failover` runs all of it on the real stack: host killed with `kill -9`, B on the copy after 36 s (banner, history fetched from the copy,
send refused with the plain "saved copy" reason), host restarted, B back on the host after 11 s and sending. `farder-replica --invite`
takes the link straight from the app's invite dialog.

**Attachments (2026-11-26):** after each database pull the replica fetches every file the copy lists (checked against its SHA-256)
and removes files the host redacted or deleted; the read-only copy serves them with `--storage-dir`. Tested in
`cargo test -p farder-replica` against the real server (and proven to fail without the server's replica download rule), and through the app in `ubuntu-suite.sh failover`: a picture
posted before the host is killed comes from the copy byte for byte and loads on screen.

Not done: promotion (a copy becomes the host), and running the replica plus
its read-only server as a service on the Beelink.
