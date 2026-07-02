# Server request dispatcher

> **File(s):** `crates/farder-server/src/handlers.rs`
> **Layer:** Server crate
> **Last reviewed:** 2026-06-04

## Purpose

`handlers.rs` is the single entry point for every `ServerRequest` a connected
client can send. `handle_request` receives the authenticated caller's
`PublicKey`, whether they are the server owner, and the deserialized request
variant, then performs all permission checks, DB reads/writes, and assembles a
`HandleResult` containing the `ServerResponse` to return to the caller and a
list of `BroadcastEvent`s to fan out to other clients. It deliberately does no
I/O of its own — network, file, and media-relay work are handled by the caller
(`connection.rs`).

---

## Public interface

### `handle_request(conn, member, is_owner, request, storage_dir, state) -> Result<HandleResult>`

**What it does:** dispatches on the `ServerRequest` variant, runs the security
checks appropriate to that request, reads/mutates the SQLite DB, and returns a
`HandleResult`.

**Parameters:**
- `conn` — open `rusqlite::Connection` for the server's DB (not pooled; one per
  connection goroutine).
- `member` — the caller's `PublicKey` as authenticated by the QUIC handshake.
- `is_owner` — if `true` all permission checks are bypassed (the server owner
  has `ALL_PERMISSIONS` implicitly).
- `request` — the decoded `ServerRequest` variant from `farder_protocol`.
- `storage_dir` — path to the server's file-storage directory; currently
  threaded through but only used by attachment helpers called from this file.
- `state` — `Arc<ServerState>` giving access to the in-memory `media` map (voice
  session tracking in `state::MediaState`).

**Returns:** `Ok(HandleResult)` in almost every case — even permission failures
are returned as `HandleResult { response: ServerResponse::Error { reason }, ..
}` rather than a Rust `Err`. A genuine `Err` indicates a DB or logic panic
(unexpected missing row, etc.).

**Side effects:** may write to the SQLite DB; may mutate `state.media.channels`
(voice stream sessions, mute/deafen sets).

**Connects to:**
- `connection.rs` — calls `handle_request` and then fans out the returned
  `BroadcastEvent`s to the appropriate subscriber sets.
- `channels.rs`, `members.rs`, `messages.rs`, `reactions.rs`, `invites.rs`,
  `audit.rs` — all actual DB operations are delegated to these modules.
- `permissions::resolve` / `permissions::has` — permission evaluation.
- `state::ServerState` — media session map (in-memory, not persisted).

---

### `resolve_member_perms_pub(conn, member, channel_id, is_owner) -> Result<u64>`

A public re-export of the internal `resolve_member_perms` for use by
`connection.rs` (e.g. to gate channel subscription). Computes the member's
effective permission bitfield for a specific channel, taking into account
`@everyone`, all assigned roles, category overrides, and channel overrides.
Owners always receive `ALL_PERMISSIONS`.

---

### `resolve_member_server_perms(conn, member, is_owner) -> Result<u64>`

Like the above but ignores category/channel overrides — useful when the
operation is server-scoped rather than channel-scoped (e.g. checking
`MANAGE_SERVER` before a category edit).

---

## Dispatch model

Every arm of the `match request { .. }` block follows the same four-step
pattern:

1. **Timeout gate** (write-path only) — if the member has an active timeout,
   return `Error` immediately before touching the DB. Applied to `SendMessage`,
   `AddReaction`, `JoinChannelMedia`, and a few others that mutate visible state.

2. **Permission check** — either `require_base_perm` (server-level bits, no
   channel context) or a full `resolve_member_perms` call (channel + category
   overrides). A failed check returns `Error` without touching the DB. Owners
   always pass. Role-hierarchy checks (`require_role_hierarchy`,
   `require_member_hierarchy`) additionally prevent lower-ranked actors from
   managing higher-ranked roles/members.

3. **DB mutation** — delegates to the relevant module (`messages::insert_message`,
   `members::ban_member`, etc.). The handler never runs raw SQL itself.

4. **Event assembly** — constructs zero or more `BroadcastEvent` structs (each
   with an `EventTarget` and a `ServerEvent` payload) and returns them in
   `HandleResult.events`. The caller fans them out; the handler never touches the
   network.

Audit rows are a side-effect of certain write operations (see the **Security
model** section). They are inserted via `audit_emit`, which also creates an
`AuditEventCreated` broadcast targeted at `PermissionHolders(MANAGE_SERVER)`.
`audit_emit` is non-fatal: if the insert fails it logs and returns `None`, so
the primary mutation still completes.

---

## Request table

### Messaging

| `ServerRequest` variant | What it does | Permission checked | DB effect | Events broadcast (target) |
|---|---|---|---|---|
| `SendMessage` | Insert a message into a channel or DM, attach files (legacy path) | `SEND_MESSAGES` (channel); DM checks participation and block list | `messages::insert_message`, `attachments::create_message_attachment` | `NewMessage` → `Subscribers(channel_id)` |
| `SubmitEvent` | Accept a signed event from the mesh event log (log-mode servers only). **For `MessagePosted`:** (1) validates the event against the in-memory `LogState`; (2) checks the channel exists and content length (max 8 000 chars); (3) persists the event body, derives the `messages` row, and materializes validated `AttachmentCap`s into `message_attachments` — all inside a single SQLite transaction (atomic); (4) advances `LogState`. Each cap is validated by `event_ingest::derive_attachments`: a cap is valid iff the stored blob's `size`/`mime_type`/`uploaded_by` match AND the event author is the cap's uploader or the server owner. Invalid caps are quarantined (logged + skipped — the message still renders). **For `AttachmentRedacted`:** (1) trial-applies to verify authz (uploader OR `"kick"`, hash known, not already redacted); (2) calls `attachments::redact_blob` inside the persist TX, which sets `files.redacted_by` to the requester's public-key bytes and deletes the on-disk file bytes (tombstone row); (3) advances `LogState` (`redacted_attachments` set gains the hash); (4) broadcasts `ServerEvent::AttachmentRedacted { content_hash, by_moderator }` to all clients. `by_moderator` is derived before the state advance by checking whether the requester matches the recorded uploader (`LogState::attachment_uploader`). | Log membership + `LogState::apply` authorization | `event_ingest::store_event`; `attachments::redact_blob` (all in one TX) | `NewMessage` → `Subscribers(channel_id)` (when a `MessagePosted` is derived); `AttachmentRedacted { content_hash, by_moderator }` → `All` (when an `AttachmentRedacted` is ingested); `EventAccepted` returned to caller |
| `EditMessage` | Replace a message's content | Author-only (no permission bit) | `messages::edit_message` | `MessageEdited` → `Subscribers(channel_id)` |
| `DeleteMessage` | Soft-delete a message | Author-only, OR `MANAGE_MESSAGES` | `messages::delete_message`; orphaned file IDs returned to caller | `MessageDeleted` → `Subscribers(channel_id)` |
| `FetchHistory` | Paginated message fetch (cursor by `before_id`, max 500) | `READ_MESSAGES` | Read only | None |
| `PinMessage` | Mark a message as pinned | `MANAGE_MESSAGES` | `messages::pin_message` | `MessagePinned` → `Subscribers(channel_id)` |
| `UnpinMessage` | Remove a pin | `MANAGE_MESSAGES` | `messages::unpin_message` | `MessageUnpinned` → `Subscribers(channel_id)` |
| `Search` | Full-text search, optionally scoped to a channel (max 500) | `READ_MESSAGES` on the target channel; cross-channel results filtered by `READ_MESSAGES` + `VIEW_CHANNEL` per result | Read only | None |
| `Typing` | Notify channel subscribers that the caller is typing | `SEND_MESSAGES` | None | `TypingStarted` → `Subscribers(channel_id)` |

### Channels and categories

| `ServerRequest` variant | What it does | Permission checked | DB effect | Events broadcast (target) |
|---|---|---|---|---|
| `CreateChannel` | Create a text, voice, or thread channel | `MANAGE_CHANNEL` (base) | `channels::create_channel` | `ChannelCreated` → `All`; `AuditEventCreated` → `PermissionHolders(MANAGE_SERVER)` |
| `UpdateChannel` | Rename, change topic/NSFW/slow-mode/retention/category/position | `MANAGE_CHANNEL` (channel) | `channels::update_channel` | `ChannelUpdated` → `All`; `AuditEventCreated` on actual rename → `PermissionHolders(MANAGE_SERVER)` |
| `DeleteChannel` | Soft-delete a channel | `MANAGE_CHANNEL` (channel) | `channels::soft_delete_channel` | `ChannelDeleted` → `All`; `AuditEventCreated` → `PermissionHolders(MANAGE_SERVER)` |
| `CreateCategory` | Create a channel category | `MANAGE_SERVER` (base) | `channels::create_category` | `CategoryCreated` → `All` |
| `UpdateCategory` | Rename/reposition a category | `MANAGE_SERVER` (base) | `channels::update_category` | `CategoryUpdated` → `All` |
| `DeleteCategory` | Delete a category | `MANAGE_SERVER` (base) | `channels::delete_category` | `CategoryDeleted` → `All` |
| `SetChannelOverride` | Set per-role allow/deny bits for a channel | `MANAGE_CHANNEL` (channel) | `channels::set_channel_override` | `PermissionsChanged` → `All`; `AuditEventCreated` → `PermissionHolders(MANAGE_SERVER)` |
| `SetCategoryOverride` | Set per-role allow/deny bits for a category | `MANAGE_SERVER` (base) | `channels::set_category_override` | `PermissionsChanged` → `All` |
| `CreateThread` | Create a thread channel from a parent message | `SEND_MESSAGES` (parent channel); threads cannot be nested | `channels::create_thread` | `ChannelCreated` → `All` |

### Roles

| `ServerRequest` variant | What it does | Permission checked | DB effect | Events broadcast (target) |
|---|---|---|---|---|
| `CreateRole` | Create a new role with a permissions bitfield, color, and position | `MANAGE_ROLES` (base) + role-hierarchy check on the target position | `members::create_role` | `RoleCreated` → `All`; `AuditEventCreated` → `PermissionHolders(MANAGE_SERVER)` |
| `UpdateRole` | Rename/recolor/reposition/change permissions of a role | `MANAGE_ROLES` (base) + role-hierarchy on both current and new position | `members::update_role` | `RoleUpdated` → `All`; `AuditEventCreated` only when permissions actually changed → `PermissionHolders(MANAGE_SERVER)` |
| `DeleteRole` | Delete a role (cannot delete builtin `@everyone`) | `MANAGE_ROLES` (base) + role-hierarchy | `members::delete_role` | `RoleDeleted` → `All`; `AuditEventCreated` → `PermissionHolders(MANAGE_SERVER)` |
| `AssignRole` | Give a role to a member | `MANAGE_ROLES` (base) + role-hierarchy | `members::assign_role` | `PermissionsChanged` → `All`; `AuditEventCreated` → `PermissionHolders(MANAGE_SERVER)` |
| `RemoveRole` | Remove a role from a member | `MANAGE_ROLES` (base) + role-hierarchy | `members::unassign_role` | `PermissionsChanged` → `All`; `AuditEventCreated` → `PermissionHolders(MANAGE_SERVER)` |

### Members / moderation / ban / timeout

| `ServerRequest` variant | What it does | Permission checked | DB effect | Events broadcast (target) |
|---|---|---|---|---|
| `KickMember` | Remove a member from the server | `KICK_MEMBERS` (base) + member-hierarchy | `members::remove_member` | `YouWereKicked` → `Members([target])`; `MemberLeft` → `All`; `AuditEventCreated` → `PermissionHolders(MANAGE_SERVER)` |
| `BanMember` | Ban a member (optionally with reason) | `BAN_MEMBERS` (base) + member-hierarchy | `members::ban_member` | `YouWereBanned { reason }` → `Members([target])`; `MemberBanned` → `All`; `AuditEventCreated` → `PermissionHolders(MANAGE_SERVER)` |
| `UnbanMember` | Lift a ban | `BAN_MEMBERS` (base) | `members::unban_member` | `MemberUnbanned` → `All`; `AuditEventCreated` → `PermissionHolders(MANAGE_SERVER)` |
| `ListBanned` | Fetch the ban list | `BAN_MEMBERS` (base) | Read only | None |
| `TimeoutMember` | Mute a member until `until_ms` (max 28 days) | `TIMEOUT_MEMBERS` (base) + member-hierarchy; `until_ms` must be in the future | `members::set_timeout` | `MemberTimeoutChanged { until_ms: Some(..) }` → `All`; `AuditEventCreated` → `PermissionHolders(MANAGE_SERVER)` |
| `RemoveTimeout` | Clear a timeout early | `TIMEOUT_MEMBERS` (base) + member-hierarchy | `members::clear_timeout` | `MemberTimeoutChanged { until_ms: None }` → `All`; `AuditEventCreated` → `PermissionHolders(MANAGE_SERVER)` |
| `BlockUser` | Block another member (personal; hides DM sends) | None | `members::block_user` | None |
| `UnblockUser` | Remove a block | None | `members::unblock_user` | None |

### Reactions and threads

| `ServerRequest` variant | What it does | Permission checked | DB effect | Events broadcast (target) |
|---|---|---|---|---|
| `AddReaction` | Add an emoji (or custom file) reaction to a message | `READ_MESSAGES` (channel); timeout gate | `reactions::add_reaction` | `ReactionAdded` → `Subscribers(channel_id)` |
| `RemoveReaction` | Remove the caller's reaction | None (own reaction only) | `reactions::remove_reaction` | `ReactionRemoved` → `Subscribers(channel_id)` |

### Voice presence — lobby layer (`JoinChannelMedia` / `LeaveChannelMedia` / `GetMediaState`)

These track who is *present* in a voice channel at the lobby level (shown in the
member panel). They do not start media streams; that is `JoinStream`.

| `ServerRequest` variant | What it does | Permission checked | DB effect | Events broadcast (target) |
|---|---|---|---|---|
| `JoinChannelMedia` | Atomically leaves all other voice channels, then joins `channel_id` | `CONNECT` (channel); timeout gate; must be a `Voice` channel type | `channels::leave_all_voice`, `channels::join_voice` | `MediaLeft { channel_id }` → `All` for each prior channel; `MediaJoined` → `All` |
| `LeaveChannelMedia` | Remove the caller from a voice channel's presence list | None | `channels::leave_voice` | `MediaLeft` → `All`; if a DM call and no participants remain, `StreamCallEnded` → `Members([other_participant])` |
| `GetMediaState` | List current voice-channel participants with joined timestamps | None | Read only | None |

### Media stream — WebRTC session layer

These manage the in-memory `state.media` map (no DB writes). A member must call
`JoinChannelMedia` first to appear in the lobby, then `JoinStream` to allocate a
WebRTC session.

| `ServerRequest` variant | What it does | Permission checked | DB effect | Events broadcast (target) |
|---|---|---|---|---|
| `JoinStream` | Allocate a new `ServerSession` in `state.media`; returns a `session_id` | None | None | `StreamJoined { session_id, .. }` → `All` |
| `LeaveStream` | Remove all of the caller's sessions from `state.media` | None | None | `StreamLeft { session_id }` → `All` per removed session |
| `EnableTrack` | Mark a track kind (Audio/Video) as active for the caller's session | None | None | `TrackEnabled { session_id, kind }` → `All` |
| `DisableTrack` | Mark a track kind as inactive | None | None | `TrackDisabled { session_id, kind }` → `All` |
| `SetMute` | Update the muted flag in `state.media` for the caller's session(s) | None | None | `StreamStateChanged { muted, deafened }` → `All` |
| `SetDeafen` | Update the deafened flag | None | None | `StreamStateChanged { muted, deafened }` → `All` |
| `OfferStreamKey` | Route per-peer wrapped media keys to specific recipients | Caller must have an active session (via `JoinStream`); returns error otherwise | None | `StreamKeyOffer` → `Members([peer_pk])` per entry in `wrapped_keys` |

### Audit

| `ServerRequest` variant | What it does | Permission checked | DB effect | Events broadcast (target) |
|---|---|---|---|---|
| `ListAuditEvents` | Paginated read of the audit log (cursor by `before_id`) | `MANAGE_SERVER` (base) | Read only | None |

### Data deletion

| `ServerRequest` variant | What it does | Permission checked | DB effect | Events broadcast (target) |
|---|---|---|---|---|
| `RequestDeletion` | Queue the caller's own data for deletion (not available to owner) | None; owner is blocked | `members::create_deletion_request` | `DeletionRequested` → `All` |
| `CancelDeletion` | Cancel a pending deletion request | None | `members::cancel_deletion_request` | `DeletionCancelled` → `All` |
| `GetDeletionStatus` | Read deletion-request status for the caller | None | Read only | None |

### Direct messages

| `ServerRequest` variant | What it does | Permission checked | DB effect | Events broadcast (target) |
|---|---|---|---|---|
| `OpenDm` | Find or create a DM channel with `target_key`; checks both sides for blocks | Target must be a member; block check | `channels::open_dm_channel` | `DmCreated { channel, participant }` → `All` (only when newly created) |
| `ListDms` | Return the caller's DM list sorted by most-recent message | None | Read only | None |

### Profile sync

| `ServerRequest` variant | What it does | Permission checked | DB effect | Events broadcast (target) |
|---|---|---|---|---|
| `UpdateProfile` | Store the caller's signed profile blob. Validates: the blob deserializes as a `SignedProfile`, the embedded signature is valid, the embedded public key matches the authenticated caller, and the avatar (if present) passes the same PNG/JPEG/GIF/WebP + 2 MB rules as the client. Stores raw bytes in `members.avatar` and the SHA-256 hash in `members.profile_hash`. | None (caller's own profile; authentication is the permission) | `members::set_member_profile` | `MemberProfileUpdated { public_key, profile_hash }` → `All` |
| `GetMemberProfile` | Fetch the stored signed profile blob for `member_key`. Returns `ServerResponse::MemberProfile { profile: Some(bytes) }` or `None` if the member has no profile yet. | None | Read only | None |

### Misc

| `ServerRequest` variant | What it does |
|---|---|
| `CreateInvite` | Generate an invite code; requires `CREATE_INVITES` (base). Returns `InviteCreated { code }`. No events. |
| `GetServerInfo` | Return channel list, category list, roles, and member count. No permission check. `name` and `owner_public_key` fields are patched by `connection.rs` after this returns. |
| `GetMembers` | Return full `MemberInfo` list including role IDs and active timeout data. No permission check. `MemberInfo` now includes `profile_hash: Option<String>` (the SHA-256 hex of the member's last pushed profile, or `null` if none). |
| `Subscribe` | No-op at this layer; channel subscription is managed by `connection.rs`. |
| `FetchUrl` | Returns an immediate `Error`; this variant must be intercepted and handled asynchronously by `connection.rs` before reaching this function. |

### Bots

| `ServerRequest` variant | What it does | Permission checked | DB effect | Events broadcast (target) |
|---|---|---|---|---|
| `AddBot` | Generates a fresh Ed25519 keypair; inserts a `bots` row (stores the coin id and secret key) and a `members` row with `is_bot=1`; `label` becomes the bot's `display_name`. The price-poller will start broadcasting its presence within the next tick. | Owner only (`is_owner` gate; `MANAGE_SERVER` check reused) | `members::register_bot_member`, `bots::register_bot` | `MemberJoined { public_key, display_name: label }` → `All` |
| `RemoveBot` | Deletes the bot from `bots` and `members`; evicts the in-memory `Presence` from `state.presences`. | Owner only | `bots::remove_bot`, `members::remove_member_row` | `MemberLeft { public_key }` → `All` |

---

### Pre-auth: `GetInvitePreview` (relay fetch proxy)

`GetInvitePreview` is **not** a `ServerRequest` — it is a `ClientFrame` handled
inside `connection.rs::authenticate()` before the normal auth sequence completes.
It is documented here because it is part of the server's request surface.

**Who sends it:** the relay's preview proxy opens a new bi-stream on the
server's control connection (handle-0 stamped, `RelayStreamRole::Primary`), the
server sends a `Challenge`, and the relay replies with
`ClientFrame::GetInvitePreview { code }` instead of the usual `Authenticate`
frame.

**What the server does:**
1. Validates the invite code via `invites::validate_invite` — no DB lock is
   held beyond the validate call.
2. **Uniform `Invalid` answer:** expired, exhausted, and nonexistent codes all
   receive the same `ServerFrame::InvitePreviewError { reason: "invalid" }`
   response. The reason string is intentionally opaque — a relay or network
   observer cannot distinguish these cases.
3. On a valid code, reads the total member count via `members::list_members`
   and the current online count from `state.clients`, then sends
   `ServerFrame::InvitePreview { server_name, member_count, online_count }`.
4. After sending the preview frame, bails with an `Err` so `handle_connection`
   tears down the stream. This is a **throwaway connection**: no member entry
   is created, no session token is issued, no `MemberJoined` event is emitted.

**Security note:** the `Err` returned from `authenticate()` on this path is not
an auth failure — it is normal termination. `connection.rs` logs it at `debug`
level. The relay's `run_relay_primary` enforces that handle-0 streams never
progress to a full authenticated session (see `docs/modules/relay.md`).

---

## Event ingest helpers (`crates/farder-server/src/event_ingest.rs`)

These public functions are the source of truth for deriving persistent rows from
the immutable event log. They are called inside the `SubmitEvent` handler and
also by the startup reconciliation path in `main.rs`.

### `derive_attachments(conn, message_id, event, owner) -> Result<usize>`

Validates each `AttachmentCap` on a `MessagePosted` event against the stored
blob and materializes a `message_attachments` row for each valid cap. A cap is
valid iff:
- A blob with its `content_hash` exists in the `files` table.
- The blob's `size`, `mime_type`, and `uploaded_by` match the cap's `size`,
  `declared_type`, and `uploader` fields exactly.
- The event author equals the cap's uploader OR equals the server owner (mirrors
  the legacy `SendMessage` ownership rule).

Invalid caps are **quarantined**: logged at `WARN` level and skipped. The message
still renders; only the attachment is unavailable. Idempotent — a cap already
materialized for this `message_id` is skipped, so reconcile can re-run safely.
Non-`MessagePosted` payloads return `Ok(0)`.

**Returns:** the count of newly-created `message_attachments` rows.

---

### `reconcile_attachments(conn) -> Result<usize>`

Startup repair: for every stored `MessagePosted` event that already has a derived
`messages` row, (re)materializes any missing valid `message_attachments` rows by
calling `derive_attachments` for each event. Idempotent. No-op for legacy
(non-log-mode) servers — returns `Ok(0)` if no genesis row exists. Called once
at server startup after `reconcile_messages`.

**Returns:** the total count of attachment rows created across all repaired events.

---

### `sweep_redacted_bytes(conn, storage_dir) -> Result<usize>`

Startup sweep: finds every `files` row where `redacted_by IS NOT NULL` and
deletes the on-disk bytes at `attachments::content_path(storage_dir, hash)` for
each. Heals a crash that occurred between setting `redacted_by` (inside the
persist TX) and the file delete that follows it. Idempotent — missing on-disk
files are silently skipped. Called once at server startup after
`reconcile_attachments`.

**Returns:** the count of on-disk files actually deleted.

---

## `bots.rs` — ticker bot storage and poller

> **File:** `crates/farder-server/src/bots.rs`

### Data model

A crypto-ticker bot is represented in two tables:

- **`bots`**: `(public_key, secret_key, kind='crypto_ticker', coin_id, label, created_at)` — the server owns and holds the bot's Ed25519 secret key.
- **`members`**: a normal members row with `is_bot=1`; `label` is the `display_name`; `joined_at` is set at registration time.

The bot has no human operator and cannot authenticate a client connection — it exists only so it appears in the member roster (`GetMembers`) and can have a `Presence` broadcast on its behalf by the poller.

### DB helpers

| Function | Signature | What it does |
|---|---|---|
| `register_bot` | `(conn, pk, secret, coin_id, label) -> Result<()>` | Inserts a row into `bots`. |
| `list_bots` | `(conn) -> Result<Vec<BotRecord>>` | Returns all `BotRecord { public_key, coin_id, label }` rows. |
| `remove_bot` | `(conn, pk) -> Result<()>` | Deletes from `bots` by public key. The `members` row is deleted separately by `handlers.rs` via `members::remove_member_row`. |

### `ticker_presence(p: &PriceInfo) -> Presence`

Composes a `Presence { kind: Ticker, details: "$<price> <arrow><pct>%", state: Some("24h") }`. The arrow glyph is `\u{25B2}` (▲) for non-negative 24h change, `\u{25BC}` (▼) for negative. Used both by the poller and by unit tests.

### `fetch_prices(coin_ids: &[String]) -> Result<HashMap<String, PriceInfo>>`

Fetches USD price and 24h percentage change for all given CoinGecko ids in a single `GET /api/v3/simple/price` call. SSRF-guarded: the constructed URL is pre-validated by `ssrf::resolves_to_global` before any network I/O. 10-second timeout; redirects disabled. Returns only entries where a valid `usd` f64 was present in the JSON response.

### `spawn_bot_poll_task(state: Arc<ServerState>, interval_secs: u64) -> JoinHandle<()>`

Spawns a background `tokio::spawn` that ticks every `interval_secs` (min 15 s, typically 60 s). On each tick:

1. **Snapshot** — reads the bot list from SQLite, releasing the `Mutex<Connection>` lock before any `.await`.
2. **Coalesce** — deduplicates coin IDs and issues ONE `fetch_prices` network call for all bots.
3. **Broadcast** — for each bot whose coin ID appeared in the response, calls `ticker_presence`, stores the result in `state.presences` (RwLock), and broadcasts `ServerEvent::MemberPresenceUpdated` to all connected clients via `connection::broadcast_event`.

Bots whose coin ID is absent from the API response retain their last-known presence (no eviction on partial failures). A network error is logged at `WARN` level and the poller continues running on the next tick.

**Roster whitelist for mesh servers:** when `GetMembers` is handled for a log-mode server, the member list is filtered to `m.is_bot || ls.is_member(&m.public_key)` so bots always appear alongside confirmed log-space members.

---

## Security model

### Who can call what

- **Owner** (`is_owner = true`): bypasses every permission and hierarchy check.
  Computed once when the connection authenticates and passed into every
  `handle_request` call.
- **Regular member**: effective permissions are the union of `@everyone` + all
  assigned roles, then modulated by category overrides and channel overrides (see
  `permissions::resolve`). The `ADMIN` bit short-circuits to `ALL_PERMISSIONS`
  after role union, before overrides.
- **Timeout**: a member with an active timeout is blocked from `SendMessage`,
  `AddReaction`, and `JoinChannelMedia` before any permission check. Moderators
  (with `TIMEOUT_MEMBERS`) can still call moderation actions while timed out
  because the timeout gate is applied selectively, not globally.

### Hierarchy rules

Two separate hierarchy checks prevent privilege escalation:

- **`require_role_hierarchy`** — used for role management (`CreateRole`,
  `UpdateRole`, `DeleteRole`, `AssignRole`, `RemoveRole`). The actor's highest
  role position must be strictly greater than the target role's position.
- **`require_member_hierarchy`** — used for member moderation (`KickMember`,
  `BanMember`, `TimeoutMember`, `RemoveTimeout`). The actor's highest role
  position must be strictly greater than the target member's highest role
  position.

Both checks are bypassed for the server owner.

### Audit trail

Destructive or permission-changing operations (channel create/rename/delete,
role create/update/delete, role assign/remove, ban, kick, timeout, channel
override changes) write a row via `audit::insert` and emit an
`AuditEventCreated` event targeted at `EventTarget::PermissionHolders(MANAGE_SERVER)`.
The audit write is non-fatal: if it fails the primary action still completes and
an error is logged.

---

## `EventTarget` values

| Value | Who receives it |
|---|---|
| `All` | Every currently connected client on this server |
| `Subscribers(channel_id)` | Clients subscribed to that channel (managed by `connection.rs`) |
| `Members(Vec<PublicKey>)` | A specific set of members by public key |
| `PermissionHolders(perm_bit)` | Clients whose resolved server-level permissions include `perm_bit`; used exclusively for `AuditEventCreated` (gated on `MANAGE_SERVER`) |

---

## State it owns

`handlers.rs` itself is stateless between calls. All persistent state is in
SQLite (via the module helpers). The only in-memory state it touches is:

| Field | Type | What it tracks, when mutated |
|---|---|---|
| `state.media.channels` | `RwLock<HashMap<u64, StreamState>>` | Voice session map — mutated by `JoinStream`, `LeaveStream`, `EnableTrack`, `DisableTrack`, `SetMute`, `SetDeafen`, `OfferStreamKey` |

---

## Integration map

- **`connection.rs`** — calls `handle_request` for every received `ServerRequest`;
  fans out the returned `Vec<BroadcastEvent>` to connected clients; handles
  `FetchUrl` before calling this function.
- **`channels.rs`** — channel and category CRUD, voice-presence tables,
  override tables, DM channel helpers.
- **`members.rs`** — member CRUD, role management, ban/timeout tables,
  block-list, deletion requests.
- **`messages.rs`** — message insert/edit/delete/fetch, pin/unpin.
- **`reactions.rs`** — reaction add/remove.
- **`invites.rs`** — invite code creation.
- **`audit.rs`** — audit row insert and paginated list.
- **`permissions`** — `resolve`, `has`, and all permission bit constants.
- **`state::ServerState`** — in-memory media session map (`state.media`).
- **`farder_protocol::server`** — `ServerRequest`, `ServerResponse`, `ServerEvent`, `ChannelType`, `TrackKind` types.
- **`bridge.rs` (client)** — the client-side mirror: each `ServerEvent` emitted here must have a corresponding arm in `bridge::dispatch_event`, which re-emits it as a `server:*` Tauri event to the frontend.

---

## Known gotchas

- **`GetServerInfo` fields are stubs here.** The `name` and `owner_public_key`
  fields in the returned `ServerResponse::ServerInfo` are placeholder values
  (`""` and `None`). `connection.rs` patches them after the call. If you add
  more stub-patched fields, search for the patching site in `connection.rs`.

- **`FetchUrl` is a protocol landmine.** The variant is present in the
  `ServerRequest` enum and reaches this match, but calling it goes straight to
  an `Error` response. It must be intercepted by `connection.rs` before the
  synchronous `handle_request` call and handled asynchronously. If you forget
  this, URL-preview requests will silently error.

- **All errors are `ServerResponse::Error`, not Rust `Err`.** A genuine `Err`
  propagated out of `handle_request` is a server-level panic (unexpected DB
  state). Normal validation failures (wrong permission, duplicate request, etc.)
  return `Ok(HandleResult { response: Error { .. } })`. The connection handler
  must not confuse these two paths.

- **Attachment ownership is enforced here, not in the attachment module.**
  `SendMessage` checks `file.uploaded_by == *member` before creating the
  attachment record. There is no separate attachment-permission layer; if you
  route file access through a different code path, add the check there too.

- **Voice sessions are matched by `connection_pk` bytes, not by `session_id`.**
  `LeaveStream`, `EnableTrack`, `DisableTrack`, `SetMute`, and `SetDeafen` all
  iterate `state.media.channels` looking for sessions whose `connection_pk`
  matches the caller's public-key bytes. A member could have multiple sessions
  (e.g. two tabs); all are affected simultaneously. `OfferStreamKey` takes only
  the first matching session when looking up `sender_info`.

- **`require_not_timed_out` is not a global gate.** It is called only for
  specific request variants. Do not assume it is applied universally; always add
  it explicitly for new write paths where timed-out members should be blocked.
