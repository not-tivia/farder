# Server connection / transport layer

> **File(s):** `crates/farder-server/src/connection.rs`, `crates/farder-server/src/media_stream.rs`, `crates/farder-server/src/db.rs`
> **Layer:** Server crate
> **Last reviewed:** 2026-06-04

## Purpose

This group of three files owns everything from a raw QUIC connection arriving at
the server to a `ServerEvent` landing in a client's inbox, plus the voice/video
datagram relay between participants.

`connection.rs` handles the per-connection lifecycle: challenge-response
authentication, the `ServerRequest` / `ServerResponse` loop, subscription
management, broadcasting `ServerEvent`s, file upload/download over auxiliary QUIC
streams, and the datagram fan-out loop for media.

`media_stream.rs` is a pure routing library — no I/O. It defines the on-wire
media frame format, per-session state (`StreamState`), per-track token-bucket
bandwidth caps, and the `on_frame_ingress` decision function that says "forward
to these session IDs" or "drop, reason X". The datagram loop in `connection.rs`
calls it.

`db.rs` initialises the SQLite schema and opens the database. It does not run
queries itself; the business-logic modules (`members`, `channels`, `messages`,
etc.) do that using the connection `connection.rs` passes them via
`ServerState`.

---

## Connection lifecycle

### Phase 1: accept

The QUIC listener (in `main.rs`, not documented here) calls
`handle_connection(state, conn)` for each new incoming connection.
`handle_connection` opens a single bidirectional QUIC stream that it uses for
all framing for the lifetime of the connection. Every message on that stream is
length-prefixed (4-byte big-endian length, then payload), limited to 16 MB.

### Phase 2: authenticate

1. Server sends `ServerFrame::Challenge { nonce }` — a random byte sequence.
2. Client responds with `ClientFrame::Authenticate { public_key, signed_challenge, invite_code, setup_token }`.
3. Server verifies the Ed25519 signature over the nonce using `auth::verify_challenge`. Invalid signature → `ServerFrame::AuthError`, connection dropped.
4. Server checks the database:
   - **Known member:** `auth::authenticate_existing_member` — checks `banned` / `revoked` / `timeout_until` flags.
   - **Unknown member:** `auth::authenticate_new_member` — requires a valid invite code, OR a valid one-time `setup_token` (which grants ownership and is then cleared so it cannot be reused). The first member to connect when the server has no owner is auto-granted ownership.
5. On success, server sends `ServerFrame::Authenticated { session_token }` and logs the peer.

### Phase 3: register

After auth the server:
- Creates an `mpsc::channel::<ServerEvent>(64)` for the client. The sender half (`event_tx`) is inserted into `state.clients` keyed by the client's public-key bytes; the receiver half is held by the event branch of the main loop.
- Registers the `quinn::Connection` in `state.voice_connections` (same key) so the datagram fan-out loop can write back to this connection.
- Broadcasts `ServerEvent::MemberJoined` to all connected clients.
- Spawns two background tasks (next section).

### Phase 4: background tasks

**Auxiliary stream acceptor** — loops on `conn.accept_bi()` to accept additional bidirectional QUIC streams opened by the client. Each stream carries either a file upload or download request, identified by the first frame. Concurrency is limited to 10 simultaneous streams per connection via a semaphore.

**Datagram fan-out loop** — loops on `conn.read_datagram()` receiving raw QUIC
unreliable datagrams (voice/video frames). For each datagram it:
1. Reads the `session_id` out of bytes 12–28 of the raw frame.
2. Finds the matching channel by scanning `state.media.channels`.
3. Calls `media_stream::on_frame_ingress` (pure; see `media_stream.rs`) which validates the frame and returns `Forward { recipients }` or `Drop(reason)`.
4. On `Forward`: looks up each recipient `session_id` → `connection_pk` → `quinn::Connection` via `state.voice_connections`, then calls `conn.send_datagram(bytes)`. The bytes are forwarded verbatim — the server never decrypts them.

### Phase 5: main request loop

`main_loop` runs a `tokio::select!` with two branches:

**Client request branch** — reads `ClientFrame::Request { id, body }` from the stream.
- `Subscribe { channel_ids }` is handled inline by `subscriptions::apply_subscribe`: the client's public-key bytes are removed from all existing subscription sets, then added to **the requested channels the caller can actually see**. `channel_ids` is client-supplied, so this filter is a **permission boundary**, not bookkeeping — see the invariant below. Ids the caller cannot see are dropped *silently* (no error, nothing named in the response) so the reply is never an existence oracle and a client with a stale channel list keeps its valid subscriptions. Replies `ServerResponse::Ok` regardless.
- `FetchUrl { url, channel_id }` is handled inline with an async HTTP fetch (permission checked first, no DB lock held during the fetch).
- All other `ServerRequest` variants are dispatched to `handlers::handle_request` which takes a short-lived `state.db` lock, does the work synchronously, and returns a `HandleResult { response, events, orphaned_file_ids }`. The lock is dropped before any `.await`. After sending the response the loop broadcasts each event in `result.events` and cleans up orphaned attachment files from disk.
- Reactions are rate-limited to 60/min per user before being dispatched.
- `AddReaction` is rate-limited (60 per minute per user) before dispatch.

**Event push branch** — receives `ServerEvent`s from `event_rx` and writes
them to the client as `ServerFrame::Event(ev)`.

### Phase 6: disconnect

When the main loop exits (clean EOF or error), `handle_connection`:
1. Aborts the auxiliary stream acceptor and datagram tasks.
2. Removes the client from `state.clients` only if the registered sender is still ours (guards against a reconnect from the same identity having already replaced it).
3. Removes the connection from `state.voice_connections`.
4. Removes the client from all channel subscription sets.
5. Broadcasts `ServerEvent::MemberLeft` to all remaining clients.

---

## Public interface

### `handle_connection(state: Arc<ServerState>, conn: quinn::Connection) -> Result<()>`

**What it does:** runs the full lifecycle described above for one QUIC connection, from challenge to disconnect cleanup.
**Returns:** `Ok(())` on clean disconnect; `Err` if a fatal error occurs (the caller logs it and drops the connection).
**Side effects:** inserts/removes entries in `state.clients`, `state.voice_connections`, and `state.subscriptions`; writes to the DB (via `handlers`); broadcasts `MemberJoined` / `MemberLeft` events; may set `state.owner` on first-member auto-claim.
**Connects to:** `handlers::handle_request` (request dispatch); `auth::*` (challenge/verify); `members::*`, `attachments::*` (DB access); `broadcast_event` (event fan-out); `media_stream::on_frame_ingress` (datagram routing).

---

### `broadcast_event(state: &ServerState, target: EventTarget, event: ServerEvent)`

**What it does:** delivers a `ServerEvent` clone to every client matched by `target`, using `try_send` on the per-client `mpsc::Sender`. Slow consumers that have let their 64-entry channel fill will silently drop the event.

Before dispatching, it calls `subscriptions::event_changes_access(&event)`; if that is true (a kick, ban, role change, permission overwrite, channel delete/move, or mesh membership change) it first `await`s `subscriptions::revalidate(state)`, which re-checks every live subscription and drops the ones that no longer hold. This is the **revocation** half of the subscribe permission boundary: filtering at `Subscribe` time only covers admission, and a member who subscribed legitimately and later lost access would otherwise keep receiving until they re-subscribed. Hooking it here rather than at each handler arm means every emitter of those events — the request handlers, the mesh event-log ingest path, bots — is covered with no per-call-site hook. Cost is a discriminant test on the hot path; DB work happens only on those rare control-plane events.
**Parameters:** `target` — which clients to deliver to (see `EventTarget` below); `event` — the payload.
**Side effects:** network I/O (writes `ServerFrame::Event` to matched QUIC streams via each client's event loop).
**Connects to:** called after every successful `handle_request` (result events), and directly from `handle_connection` for `MemberJoined` / `MemberLeft`.

#### `EventTarget` variants

| Variant | Who receives the event |
|---|---|
| `All` | Every currently-connected client |
| `Subscribers(channel_id)` | Clients subscribed to a specific channel (opt-in via `Subscribe`, **filtered** — the set only ever contains members who can see the channel, so this target is safe to use for private-channel and DM traffic) |
| `Members(pks)` | An explicit list of public keys (e.g. DM participants) |
| `PermissionHolders(perm_bit)` | All connected clients whose resolved permissions include `perm_bit` (e.g. for audit events) |
| `MediaStreamJoin/Leave`, `MediaTrackEnabled/Disabled`, `MediaSetDeafen` | Media lifecycle targets — currently no-op stubs in `broadcast_event` (implementation pending MST-10) |

---

## File upload/download

Both are handled over auxiliary QUIC bidirectional streams, not over the primary control stream.

**Upload** (`handle_upload_stream`): validates file size (configurable cap from `state.max_file_size`), checks `SEND_MESSAGES` permission, applies a 10-upload-per-minute per-user rate limit, deduplicates by SHA-256 hash, streams bytes to a temp file with a 5-minute timeout, then atomically moves the temp file to a content-addressed path and records it in the DB via `attachments::store_or_reuse_from_temp_file`.

**Download** (`handle_download_stream`): looks up the file record; checks `files.redacted_by IS NOT NULL` — if set, returns `DownloadResponse::Error { reason: "not available" }` immediately (a redacted blob is treated the same as a missing one, regardless of permission); then verifies the requesting member has `VIEW_CHANNEL | READ_MESSAGES` on at least one channel the file was attached to; sends a `DownloadResponse::Start` metadata frame; and streams the raw file bytes. "File not found", "access denied", and "redacted" all return `DownloadResponse::Error { reason: "not available" }` — the uniform reason string ensures no case can be used as an existence oracle (mesh design: existence-oracle hardening). (Scope note: the upload-dedup path still returns `Complete` immediately for a known hash — a pre-existing hash-existence oracle left to the file-hardening track.) For mesh-mode servers, the `message_attachments` join rows that the permission gate reads are materialized by `event_ingest::derive_attachments` at ingest time, so a member's attachment download access automatically follows log membership.

**URL fetch** (`handle_fetch_url`): the server fetches a URL on the client's behalf (max 10 MB, 10-second timeout, HTTP/HTTPS only), stores it as an attachment, and returns the `file_id`. Permission is checked (requires `SEND_MESSAGES` on the target channel). The DB lock is held only for the brief store step — the HTTP fetch runs without any lock.

---

## Media stream routing (`media_stream.rs`)

### Wire format

Each QUIC datagram is a fixed 28-byte header followed by opaque AEAD ciphertext. The server **never decrypts the ciphertext** — it routes solely on the header.

| Bytes | Field |
|---|---|
| 0 | Version (`MEDIA_FRAME_VERSION`) |
| 1 | Track type: `0x01` = Audio, `0x02` = Video |
| 2–3 | `track_id`, `codec_id` (reserved, ignored in v1) |
| 4–11 | Sequence number (u64, big-endian) |
| 12–27 | `session_id` (16 bytes, opaque random; no pubkey in header — sealed-sender invariant) |
| 28+ | AEAD ciphertext (includes 16-byte auth tag) |

### `on_frame_ingress(state, config, sending_connection_pk, raw, now_ms) -> IngressDecision`

**What it does:** pure routing decision — no I/O, no locks taken. Parses the frame header, checks:
1. `session_id` exists in `state.sessions`.
2. The session's `connection_pk` matches `sending_connection_pk` (prevents session hijacking).
3. The session has the track kind enabled (`active_tracks`).
4. The per-track token bucket has capacity (`audio_max_bps = 64 KB/s`, `video_max_bps = 2 MB/s`).

On pass, returns `Forward { recipients }` — the `session_id`s of every other session in the same channel that is not deafened.
On fail, returns `Drop(DropReason)` — one of `UnknownSession`, `SessionConnectionMismatch`, `TrackNotEnabled`, `BandwidthCap`, `ParseError`.

**Connects to:** called by the datagram task in `handle_connection`; caller resolves recipients to `quinn::Connection`s and calls `send_datagram`.

### `StreamState` / `ServerSession`

The server keeps one `StreamState` per active voice/media channel in `state.media.channels` (a `std::sync::RwLock`). Each `StreamState` holds:
- `sessions: HashMap<SessionId, ServerSession>` — active streams; `ServerSession` carries `connection_pk` (for auth), `channel_id`, `public_key` / `display_name` (for event emission only, never inspected during per-frame routing), `active_tracks`, token `buckets`, and last-frame timestamps.
- `deafened: HashSet<SessionId>` — sessions that should not receive forwarded frames.
- `muted: HashSet<SessionId>` — display-only; does not affect routing.

### `compute_activity_transitions(state, prev_active, now_ms)`

Called by a 5 Hz tick loop (outside this file). Compares each session's last-frame timestamp against `ACTIVITY_TIMEOUT_MS` (300 ms) and returns the list of `(session_id, kind, active)` transitions since the previous tick. The caller emits `TrackActivityChanged` server events on transitions (speaking indicator).

---

## SQLite schema highlights (`db.rs`)

`db::open_file` opens the database with `PRAGMA journal_mode=WAL` and foreign-key enforcement, then calls `init_schema`. `db::open_in_memory` is used in tests. The schema is applied idempotently.

| Table | Purpose |
|---|---|
| `members` | One row per registered public key. Columns include `display_name`, `avatar`, `banned`, `revoked`, `ban_reason`, `timeout_until`, `timeout_reason`. |
| `roles` / `member_roles` | RBAC: named roles with a `permissions` bitmask; members may hold multiple roles. |
| `categories` / `channels` | Server structure. `channels.channel_type` is `'text'`, `'voice'`, or `'dm'`; supports slow mode, NSFW flag, retention, thread parent, and soft-delete. |
| `channel_overrides` / `category_overrides` | Per-(channel/category, role) permission allow/deny bitmasks. |
| `messages` | Text content, author pubkey, timestamp, optional `edited_at`, `reply_to`, `pinned`. Indexed on `(channel_id, timestamp)` and `(channel_id, id)` for pagination. |
| `messages_fts` | FTS5 virtual table mirroring `messages.content` for full-text search. |
| `invites` | Single-use or multi-use invite codes with optional expiry and use-count cap. |
| `files` / `message_attachments` | Content-addressed file store (SHA-256 keyed). `files.ref_count` tracks attachment references for orphan cleanup. |
| `reactions` | `(message_id, user_key, emoji)` primary key; optional `file_id` for custom-emoji reactions. |
| `voice_state` | Which users are currently in which voice channel. |
| `dm_participants` | Maps DM channel IDs to their two participant public keys. |
| `blocked_users` | Per-user block list. |
| `deletion_requests` | Scheduled account-deletion requests with an expiry. |
| `audit_events` | Append-only moderator action log (`actor_pk`, `target_pk`, `action`, `metadata`, `timestamp_ms`). Indexed by timestamp and actor. |

Schema migrations are applied inline in `init_schema` using `PRAGMA table_info` checks before `ALTER TABLE`, because SQLite does not support `IF NOT EXISTS` for column additions.

---

## State it owns

| Field | Type | What it tracks |
|---|---|---|
| `state.clients` | `RwLock<HashMap<[u8;32], mpsc::Sender<ServerEvent>>>` | One entry per live connection, keyed by public-key bytes. The sender is the inbound end of that client's event channel. Inserted on auth, removed on disconnect. |
| `state.voice_connections` | `RwLock<HashMap<[u8;32], quinn::Connection>>` | Same key as `clients`. Holds the raw QUIC connection for datagram fan-out. |
| `state.subscriptions` | `RwLock<HashMap<u64, HashSet<[u8;32]>>>` | `channel_id → set of subscribed public-key bytes`. Written only by `subscriptions::apply_subscribe` (admission-filtered) and `subscriptions::revalidate` (revocation); the entire entry for a client is removed on disconnect. **Invariant: the subscription set never contains a member who cannot see the channel.** |
| `state.media.channels` | `StdRwLock<HashMap<u64, StreamState>>` | Per-channel media routing state. Managed by the voice/stream handler (not directly in this file). |
| `state.owner` | `RwLock<Option<PublicKey>>` | The server's owner public key. Set on first-member auto-claim or when a `setup_token` is consumed. Never cleared. |
| `state.setup_token` | `Mutex<Option<[u8;32]>>` | One-time bootstrap token. Cleared to `None` atomically when claimed. |

---

## Integration map

- **`handlers::handle_request`** — receives every `ServerRequest` (except `Subscribe` and `FetchUrl`) and returns a `HandleResult` with the response, broadcast events, and any orphaned file IDs.
- **`auth`** — provides `generate_challenge`, `verify_challenge`, `authenticate_existing_member`, `authenticate_new_member`, `generate_session_token`.
- **`attachments`** — `store_or_reuse_from_temp_file`, `get_file`, `get_file_by_hash`, `cleanup_orphaned_file`, `content_path`.
- **`media_stream::on_frame_ingress`** — the per-datagram routing oracle called from the datagram fan-out task.
- **`bridge.rs` (client crate)** — the other end of the QUIC control stream: it reads `ServerFrame`s and dispatches `ServerEvent`s to the UI or `VoiceController`. See `docs/modules/tauri-bridge.md` for the full event catalog.
- **`farder_protocol::codec`** — `encode`/`decode` for all frames.

---

## Known gotchas

- **`Subscribe` is a permission boundary, not bookkeeping.** The subscription set never contains a member who cannot see the channel. `EventTarget::Subscribers(channel_id)` fans out to that raw set with no further check, and it carries `NewMessage` (plaintext for normal channels, ciphertext plus full metadata for DMs), `MessageEdited`, `MessageDeleted`, `ReactionAdded/Removed` and the poll/giveaway/event widget events — so the privacy of every private channel and DM rests on the set being correct. Two mechanisms hold it: admission (`subscriptions::apply_subscribe`) and revocation (`subscriptions::revalidate`, driven from `broadcast_event`). **Never insert into `state.subscriptions` from anywhere else**, and never add a code path that removes someone's access without emitting one of the events in `subscriptions::event_changes_access` (or calling `revalidate` directly). See `crates/farder-server/src/subscriptions.rs` for the full statement and its documented residual gaps.
- **`state.db` is a `std::sync::Mutex`** — the lock must never be held across an `.await`. All DB calls in `handle_connection` follow the pattern: acquire lock in a block, do the work, drop block, then `.await`. Violating this deadlocks the entire server because Tokio cannot yield across a sync mutex guard.
- **`state.clients` event channel capacity is 64.** `try_send` is used (not `.await`-send), so a slow client that fills its inbox silently loses events. This is intentional to prevent one lagging client from blocking all broadcasts, but it means clients may miss events if they can't keep up.
- **Same-identity reconnect race:** the disconnect cleanup only removes the client from `state.clients` if the stored sender still matches the disconnecting connection's sender (`same_channel` check). Without this, a fast reconnect from the same key would have its entry evicted by the old connection's cleanup.
- **Media frame routing is sealed-sender.** The 28-byte frame header contains only a random `session_id` — no public key. The server cannot tell from the header who is speaking; identity is bound to the session inside the AEAD ciphertext. Any logging at the frame-routing layer is therefore privacy-safe.
- **`MediaStreamJoin/Leave/TrackEnabled/Disabled/MediaSetDeafen` `EventTarget` variants are stubs** — they are matched as `=> {}` in `broadcast_event` and do nothing. Media lifecycle events (join/leave voice channel, track toggle) are dispatched via a separate path in `handlers.rs`, not through `broadcast_event`. Don't add event logic here expecting these targets to fire.
- **Auxiliary stream concurrency cap is 10 per connection.** A client requesting an 11th concurrent upload/download will have that stream silently dropped (no error frame is sent for the over-limit case, only for streams that are accepted but fail).
