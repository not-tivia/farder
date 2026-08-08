# Server request dispatcher

> **File(s):** `crates/farder-server/src/handlers.rs`
> **Layer:** Server crate
> **Last reviewed:** 2026-07-04

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

### `build_member_info(conn, state, pk) -> Result<MemberInfo>`

Constructs a `MemberInfo` for the given public key, suitable for embedding in
`DmCreated`, `DmOpened`, and similar events. Reads the `members` row (via
`members::get_member`), fetches role ids (`members::get_member_role_ids`),
resolves any active timeout (`members::is_timed_out`), and reads the live
`Presence` from `state.presences`.

**Parameters:**
- `conn` — open `rusqlite::Connection`.
- `state` — `&ServerState` (read-only access to `state.presences`).
- `pk` — the member's `PublicKey`.

**Returns:** `Err` if no `members` row exists for `pk` (not just `None` — callers
must ensure the member is present before calling).

**Why extracted:** the `OpenDm` handler and `bots::send_bot_dm` both need a
fully populated `MemberInfo` for the `DmCreated { participant }` event. Extracting
removes duplication and guarantees consistent field population.

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
| `SubmitEvent` | Accept a signed event from the mesh event log (log-mode servers only). **Arm order (Rung 2):** log-mode check → `event_ingest::check_ingest_caps` (size/vector caps + the `core.timestamp` future bound, *before* the `LogState` clone) → `stale-epoch` pre-check for `MlsCommit` → fold trial-apply → persist transaction (`store_event` → `materialize_channel_created` → derive) → in-memory `LogState` advance → broadcast. **For `MessagePosted`:** (1) validates the event against the in-memory `LogState`; (2) checks the channel exists and content length (max 8 000 chars); (3) persists the event body, derives the `messages` row, and materializes validated `AttachmentCap`s into `message_attachments` — all inside a single SQLite transaction (atomic); (4) advances `LogState`. Each cap is validated by `event_ingest::derive_attachments`: a cap is valid iff the stored blob's `size`/`mime_type`/`uploaded_by` match AND the event author is the cap's uploader or the server owner. Invalid caps are quarantined (logged + skipped — the message still renders). **For `AttachmentRedacted`:** (1) trial-applies to verify authz (uploader OR `"kick"`, hash known, not already redacted); (2) calls `attachments::redact_blob` inside the persist TX, which sets `files.redacted_by` to the requester's public-key bytes and deletes the on-disk file bytes (tombstone row); (3) advances `LogState` (`redacted_attachments` set gains the hash); (4) broadcasts `ServerEvent::AttachmentRedacted { content_hash, by_moderator }` to all clients. `by_moderator` is derived before the state advance by checking whether the requester matches the recorded uploader (`LogState::attachment_uploader`). | Log membership + `LogState::apply` authorization | `event_ingest::store_event`; `attachments::redact_blob` (all in one TX) | `NewMessage` → `Subscribers(channel_id)` (when a `MessagePosted` is derived); `AttachmentRedacted { content_hash, by_moderator }` → `All` (when an `AttachmentRedacted` is ingested); `EventAccepted` returned to caller |
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
| `KickMember` | Remove a member from the server. **Refuses the system identity** (`bots::is_system_identity` → `Error { "that identity can't be removed" }`) after the perm + hierarchy checks — hierarchy cannot protect it, since it holds no roles, and `members::remove_member` is an unfiltered `DELETE FROM members` that would durably kill every reminder/event DM. | `KICK_MEMBERS` (base) + member-hierarchy + not-the-system-identity | `bots::is_system_identity`, `members::remove_member` | `YouWereKicked` → `Members([target])`; `MemberLeft` → `All`; `AuditEventCreated` → `PermissionHolders(MANAGE_SERVER)` |
| `BanMember` | Ban a member (optionally with reason). Same `bots::is_system_identity` refusal as `KickMember`, same error string. | `BAN_MEMBERS` (base) + member-hierarchy + not-the-system-identity | `bots::is_system_identity`, `members::ban_member` | `YouWereBanned { reason }` → `Members([target])`; `MemberBanned` → `All`; `AuditEventCreated` → `PermissionHolders(MANAGE_SERVER)` |
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
| `GetMembers` | Return full `MemberInfo` list including role IDs and active timeout data. Sourced from `members::list_members_visible` — the server's own system identity is filtered out in SQL, **before** the mesh `is_bot ||` whitelist below can re-admit it. No permission check. `MemberInfo` now includes `profile_hash: Option<String>` (the SHA-256 hex of the member's last pushed profile, or `null` if none). |
| `Subscribe` | No-op at this layer; channel subscription is managed by `connection.rs`. |
| `FetchUrl` | Returns an immediate `Error`; this variant must be intercepted and handled asynchronously by `connection.rs` before reaching this function. |

### Bots

| `ServerRequest` variant | What it does | Permission checked | DB effect | Events broadcast (target) |
|---|---|---|---|---|
| `AddBot` | Trims and lowercases `coin_id`; validates charset `[a-z0-9-]` and length ≤64 (returns `Error` on violation). Trims `label`; validates 1–64 chars (returns `Error` on violation). Generates a fresh Ed25519 keypair; inserts a `bots` row (stores the coin id and secret key) and a `members` row with `is_bot=1`; `label` becomes the bot's `display_name`. The price-poller starts broadcasting its presence within the next poll cycle. | Owner only (`is_owner` gate; `MANAGE_SERVER` check reused) | `members::register_bot_member`, `bots::register_bot` | `MemberJoined { public_key, display_name: label }` → `All` |
| `AddCustomBot` | Trims `name` (1–64 chars), `source_url` (must start with `http://` or `https://`, ≤2048 chars), `value_path` (1–256 chars). Trims and caps `unit` at 24 chars; `None`/empty is stored as `NULL`. Validates each field and returns `Error` on violation. Generates a fresh Ed25519 keypair; calls `members::register_bot_member` and `bots::register_custom_bot`. `name` becomes the bot's `display_name`. The poll loop begins fetching the API on the next cycle. | MANAGE_SERVER | `members::register_bot_member`, `bots::register_custom_bot` | `MemberJoined { public_key, display_name: name }` → `All` |
| `RemoveBot` | **Refuses the system identity** (`bots::is_system_identity` → `Error { "that identity can't be removed" }`) before doing anything — defense in depth, since the key is never listed but a modified client could name it. Otherwise deletes the bot from `bots` and `members`; evicts the in-memory `Presence` from `state.presences`. Cascade: `bots::remove_bot` first deletes all `bot_alerts` and `bot_subscriptions` for this bot. | Owner only | `bots::remove_bot`, `members::remove_member_row` | `MemberLeft { public_key }` → `All` |
| `SetBotPollInterval` | Clamps `secs` to ≥30, then writes the value to `server_settings` via `bots::set_poll_interval`. The poll loop reads the interval live each cycle, so the change takes effect immediately after the current sleep expires (no restart). | MANAGE_SERVER (owner or role holders) | `bots::set_poll_interval` → `server_settings` KV (`key="bot_poll_interval"`) | — |
| `GetBotPollInterval` | Reads the poll interval via `bots::get_poll_interval` and returns `BotPollInterval { secs }`. No permission gate — any connected member may query this. | None | `bots::get_poll_interval` (read-only) | — |
| `AddBotAlert` | Validates `metric` (`"price_usd"`, `"change_24h"`, or `"value"`) and `comparator` (`"above"` or `"below"`); rejects invalid values with `Error`. Inserts a `bot_alerts` row with `armed=1`. Returns `Ok`. | MANAGE_SERVER | `bots::add_alert` → `bot_alerts` | — |
| `RemoveBotAlert` | Deletes the `bot_alerts` row by `alert_id`. No-op if id not found. Returns `Ok`. | MANAGE_SERVER | `bots::remove_alert` | — |
| `ListBotAlerts` | Returns all alerts for `bot_public_key` as `BotAlerts { alerts: Vec<BotAlertInfo> }`. The internal `armed` flag is not included in `BotAlertInfo`. | MANAGE_SERVER | `bots::list_alerts_for_bot` (read-only) | — |
| `SubscribeBot` | Idempotent subscribe: `INSERT OR IGNORE` into `bot_subscriptions` with the authenticated caller as subscriber (client cannot supply a different key). Returns `Ok`. | None (any authenticated member) | `bots::subscribe` → `bot_subscriptions` | — |
| `UnsubscribeBot` | Removes the `(bot_public_key, caller)` row from `bot_subscriptions`. No-op if not subscribed. Returns `Ok`. | None (any authenticated member) | `bots::unsubscribe` | — |
| `ListMySubscriptions` | Returns `MySubscriptions { bot_public_keys }` — the bot public keys the authenticated caller is subscribed to. | None (any authenticated member) | `bots::list_subscriptions_for_user` (read-only) | — |

---

### Personal reminders

Owner is **always** the authenticated connection key — neither variant carries an owner field, and both scope by `owner = ?` in SQL. Neither is added to `request_requires_membership`'s allow-list, so mesh log-membership gating is automatic. Creation is not a request at all: it is the `"reminder"` `RunCommand` kind (connection.rs), which runs after every existing dispatch gate and replies with an invoker-only `Notice` — nothing is posted or broadcast.

| `ServerRequest` variant | What it does | Permission checked | DB effect | Events broadcast (target) |
|---|---|---|---|---|
| `ListMyReminders` | Returns `MyReminders { reminders }` — the caller's own pending reminders, soonest first (`LIMIT 20`). Read: no timeout gate, not rate-limited, no channel visibility (a reminder is not channel content). | Membership only (default-deny) | `reminders::list_pending_for` (read-only) | — |
| `CancelReminder` | Cancels one of the caller's own pending reminders. Rows-affected 0 → `Error { "reminder not found" }`, byte-identical for a foreign id, an already-fired one and a nonexistent one (no existence oracle). No timeout gate — managing your own private nudges is not channel content. | Membership + `widget_limiter` (10/10 s) | `reminders::cancel` → `reminders.status='cancelled'` | — |

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

## Channel content class + the message write choke point

**Files:** `crates/farder-server/src/channel_class.rs`, `messages.rs`.
**Spec:** `docs/superpowers/specs/2026-07-27-mesh-rung2-e2ee-design.md` (rev 2), C8/F1.

An E2EE channel's promise is that the server cannot read it. That only holds if
**no** server-side path can author a plaintext row there. Rung 2 makes that
structural rather than a per-call-site convention.

### Class resolution (`channel_class.rs`)

The class is a property of the channel's identity **in the log**
(`EventPayload::ChannelCreated { class }`). It is mirrored into
`channels.content_class` inside the same transaction that accepts the event, so
any writer holding only a `&Connection` — including the widget sweeper, which
has no `ServerState` — can resolve it with a plain DB read and no second mutex.

| stored `content_class` | `ChannelWriteClass` | server-authored content |
|---|---|---|
| `'plaintext'` (also the column default, so every legacy channel) | `Plaintext` | allowed |
| `'e2ee'` | `E2ee` | **refused** |
| row missing, value unrecognised, or the read **errors** | `Unresolvable` | **refused** |

`ChannelWriteClass::refuses_server_authored_content()` answers `true` for
anything that is not a definite `Plaintext`. **Fail closed: absence of
information never yields a plaintext write.**

Public surface:

- `resolve(conn, channel_id) -> ChannelWriteClass` — infallible; a failed read
  is `Unresolvable`, never `Plaintext`.
- `require_plaintext(conn, channel_id) -> Result<()>` — the strict guard used by
  every server-authored door.
- `require_plaintext_for_derived(conn, channel_id) -> Result<()>` — the guard
  for the log-derived `MessagePosted` door. Identical to the strict guard except
  that a channel with **no `channels` row at all** is allowed, mirroring the
  fold's Rung-1 carve-out (`event_log_state.rs:873-886` gates only channels the
  log knows). A row that exists with an unreadable class is still refused.
- `require_e2ee(conn, channel_id) -> Result<()>` — the sealed door's guard; the
  sealed door is not a general bypass, so it refuses plaintext and unresolvable
  channels alike.
- `set_class(conn, channel_id, ChannelClass)` — writes the mirror. Called only
  from `reconcile_channel_classes` (the ingest path writes the class in the same
  `INSERT` that creates the row, so there is no window between the two).
- `reconcile_channel_classes(conn) -> Result<usize>` — startup re-derivation,
  called from `main.rs` beside `reconcile_messages`. Replays every stored
  `ChannelCreated` and re-asserts the mirror. The repair rule is **one-way**,
  because "the log is the authority" and "fail closed" only ever point the same
  direction: a mirror that is anything other than `'e2ee'` is repaired to the
  log's declared value (which can only ever refuse *more*), but a mirror that
  says `'e2ee'` while the log disagrees is **left alone** and logged at `error!`
  — widening a channel the DB currently treats as sealed is the one move that
  could expose content, so a disagreement there is refused rather than resolved.
  A declared channel with **no `channels` row** is logged and skipped, never
  re-created: absence resolves `Unresolvable` ⇒ refuse, whereas re-materializing
  could resurrect a deleted channel. Returns the number of rows repaired.
- `E2EE_REFUSED` — the single refusal string every class rejection shares, so a
  channel id is never an existence oracle.

### The choke point (`messages.rs`)

Exactly one statement in the server inserts a `messages` row: the private
`messages::insert_row`. Every module reaches it through one of these doors, and
the class guard lives on the door (not inside `insert_row`) so the one
legitimate E2EE door can state its own, opposite rule:

| door | visibility | guard |
|---|---|---|
| `insert_message` | `pub` | `require_plaintext` |
| `insert_message_with_ts` | `pub` | `require_plaintext` |
| `insert_message_with_author_name` | `pub` | `require_plaintext` |
| `edit_message` (the `UPDATE`) | `pub` | `require_plaintext`, resolved from the row's own `channel_id` |
| `insert_derived_row` | `pub(crate)` | `require_plaintext_for_derived` — the `MessagePosted` read-view door, used by `event_ingest` |
| `insert_sealed_row` | `pub(crate)` | `require_e2ee` — the **only** door into an E2EE channel, used by `event_ingest`'s sealed derive path |
| `update_sealed_row` (the `UPDATE`) | `pub(crate)` | `require_e2ee` resolved from the row's own `channel_id`, **plus** the row itself must be sealed — the sealed-edit door, used by `event_ingest::apply_sealed_edit`. Never touches `content` or `messages_fts` |

The three `insert_message*` signatures are **unchanged**, so all ~18 existing
writers (legacy `SendMessage`, slash-command replies, webhooks, poll/giveaway/
event cards and their sweeper announcements, bot and system DMs) are gated by
construction without being edited.

Sealed rows carry `content = ''`, the opaque ciphertext in `messages.sealed`,
`messages.is_e2ee = 1`, and **skip the FTS insert entirely** — nothing
plaintext-shaped is written, so there is nothing for a future `content`-reading
feature to leak.

**Structural guard:** `no_insert_into_messages_sql_outside_the_choke_point`
walks `crates/farder-server/src` and asserts no file other than `messages.rs`
contains a raw `INSERT INTO messages`. A new writer added later fails a test
instead of silently becoming a plaintext door into a sealed channel.

### Rung-2 ingest: caps, `stale-epoch`, and atomic channel creation

The `SubmitEvent` arm gained three things, in this order, all **before** the
persist transaction:

1. **`check_ingest_caps`** — see "Event ingest helpers" below. Runs before the
   `LogState` clone.
2. **The `stale-epoch` pre-check.** An `MlsCommit` that lost the epoch CAS is an
   *accepted no-op* in the fold (Rung-3 replay determinism needs every replica to
   fold a converged event set identically), but the author's client must not
   believe it landed. Ingest consults `LogState::mls_current_epoch(channel_id)`
   and, if either the declared generation or the declared epoch is not current,
   returns the **exact string `"stale-epoch"`** — a distinct, machine-readable
   code the client's resync loop keys on (process winner → rebuild → resubmit).
   The event is never stored, so the fold's no-op path is unreachable through
   ingest. A channel with no MLS group falls through to the fold, which refuses
   it. *This runs before signature/authz, so an unauthenticated submitter can
   distinguish "current epoch" from "not current" — which leaks nothing: the
   whole commit/Welcome stream is public server-wide by this rung's design.*
3. **`materialize_channel_created`**, inside the persist transaction — see
   "Event ingest helpers" below.

**Broadcast rule for `ChannelCreated`:** only a **Plaintext**-declared channel is
announced, via the existing `ServerEvent::ChannelCreated { channel: ChannelInfo }`.
`ChannelInfo` has no class field and cannot gain one without breaking every
un-updated client's decode of *plaintext* channels too (spec M2), so announcing a
sealed channel through it would hand a v1 client a normal-looking channel with a
working composer — worse than not seeing it. The E2EE announcement rides
`ChannelInfoV2` (sub-3 Task 5); until then an E2EE channel is simply not
announced, which is the fail-closed side.

**Atomicity, observed:** the "fold accepts, ingest refuses" case (a thread child
whose class the fold happily inherits, which ingest refuses on shape) is the
hardest one, and it is the one the test drives — `store_event` has already run
inside the transaction, and the assertion is that the `events` count, the
`channels` count and `LogState::log_pos()` are all unchanged afterwards, and that
the next well-shaped event on the same chain slot is still accepted.

### Rung-2 derivation: sealed rows, reply mapping, edits, tombstones

Every Rung-2 read-view write addresses its row through `messages.event_hash`,
which a `UNIQUE` index makes a real key. That is a correctness property, not a
performance one: reply mapping, sealed edits and content-blind deletes all mean
"the row this event derived", and a second row carrying the same hash would make
that target ambiguous — on the moderation path, ambiguity is a bug. SQLite
treats `NULL`s in a unique index as distinct, so every legacy row keeps its
`NULL` hash and is unaffected.

**Reply mapping (spec F9).** A log reply cites an event *hash*; the client
renders threading off a numeric row *id*. `resolve_reply_target` bridges the two
by looking the hash up in `messages.event_hash`. An **unresolvable** target
derives `reply_to = NULL` rather than failing the event — the edge is repaired
later by `repair_reply_links`, so out-of-order arrival (or a future replicated
log) costs a render, never the message. Without this, replies in an E2EE
channel would be silently dropped, since such channels are log-only.

**Sealed edits.** `apply_sealed_edit` runs inside the persist transaction.
`LogState` keeps no per-message index by design, so the fold authorizes
`MessageEditedE2ee` on the send gates plus "not tombstoned" and leaves **target
authorship** to ingest, which owns the only per-message index there is. The
target must exist, sit in the cited channel, be sealed, and belong to the same
author: only an author rewrites their own message — moderators delete, they do
not rewrite. `update_sealed_row` touches `sealed` and `edited_at` only, so
`content` stays `''` and the row never reaches `messages_fts` through the back
door `edit_message`'s re-index would open.

**Tombstones (spec F2).** `apply_tombstone` **hard-deletes** the derived row and
returns its orphaned blob ids so the handler can hand them to the same file-GC
path the legacy `DeleteMessage` arm uses. Authorship splits exactly where the
fold splits it: `DeleteReason::Moderation` is fully authorized by the fold (it
checks the `kick` capability), while `DeleteReason::Author` needs the index the
fold omits, so ingest requires the deleter to be the row's author. The delete
takes effect live through the **shipped** `ServerEvent::MessageDeleted` — no new
variant, so v1 clients act on it too, and its payload (an id and a channel) is
public metadata, so it stays content-blind in a sealed channel. Content-blind
delete is the *only* moderation mechanism in an E2EE channel, so this path is
load-bearing rather than cleanup.

**Unknown targets are refused, and that is what bounds the fold.** Both
`apply_sealed_edit` and `apply_tombstone` return `Err` for a target no row
carries, which rolls the persist transaction back: nothing stored, no log
advance. The fold cannot tell a real target from a fabricated one, so without
this refusal `MessageEditedE2ee` would be a free write into a sealed channel's
history and the fold's `tombstones` set would accept anything.

**Sealed posts and sealed edits are deliberately NOT broadcast.**
`NewMessage`/`MessageEdited` carry a `String` body; stuffing ciphertext into one
would ship garbage to every v1 client. Their delivery rides the v2 surfaces
(sub-3 Task 5).

**`reconcile_messages(conn, log_state: Option<&LogState>)`** — the signature
change is the point. At startup it now runs four passes, and is idempotent (a
second run in a row repairs nothing):

1. **Derive missing rows** for both `MessagePosted` and `MessagePostedE2ee`,
   in `accept_seq` order, **skipping any event the log has tombstoned**. Without
   that skip, deletion would silently undo itself on the next boot — the event
   is still stored, so re-derivation would resurrect the row.
2. **Sweep** any row a stored tombstone still targets (the crash-window heal),
   logged at `warn!`.
3. **Replay sealed edits** in accept order, last edit wins. Derivation alone
   gives every row its *original* ciphertext, so a wiped-and-rebuilt view would
   otherwise roll back every edit ever made. An absent target — tombstoned, or
   not yet derived — is skipped, not an error.
4. **Repair reply links** left `NULL` at derive time.

Together these are what make `derived_view_rebuild_from_events` equal the live
view: wipe `messages`, re-run reconcile, and the result matches row for row,
with tombstoned rows staying absent and reply edges restored.

**Search.** `search_messages` adds `AND is_e2ee = 0` as belt-and-braces behind
the FTS skip (coexistence row 7a). A sealed row never enters `messages_fts` so
it cannot match — but the index is a mutable artifact and search is the one
surface whose whole job is reading content, so the filter is stated in the query
too. Retention, redaction and anonymization already operate on the ciphertext
row without reading it (row 7b), which the tests verify rather than assert.

### Schema (`db.rs`)

- `channels.content_class TEXT NOT NULL DEFAULT 'plaintext'` — the class mirror.
- `messages.is_e2ee INTEGER NOT NULL DEFAULT 0`, `messages.sealed BLOB`.
- `idx_messages_event_hash` — a `UNIQUE` index on `messages.event_hash`,
  enforcing exactly one derived row per log event (see "Rung-2 derivation"
  above). Created unconditionally, not via the column migration.

All three are added by the idempotent `PRAGMA table_info` migration pattern, so
existing databases pick them up on the next open with legacy rows defaulting to
plaintext (Q8's carve-out: a channel the log never knew stays plaintext forever).

### Request-layer refusals

The choke point hard-**errors**, which is the right answer for a programming
mistake but the wrong answer to a user request. So every request that would
produce (or act on) server-readable content in a channel is refused *first*, at
the request layer, with a clean message. The choke point remains as the
backstop; neither is load-bearing alone.

`handlers::require_plaintext_channel(conn, channel_id) -> Option<Result<HandleResult>>`
is the shared gate: `Some(denied)` to propagate, `None` to proceed. It returns
the byte-identical `E2EE_REFUSED` and fails closed on `Unresolvable`, so a
sealed channel and a nonexistent one are indistinguishable.

| request | where the gate sits | refusal |
|---|---|---|
| `SendMessage` | after the timeout check, before any attachment/FTS work | `E2EE_REFUSED` |
| `EditMessage` | after the authorship check, via `msg.channel_id` | `E2EE_REFUSED` |
| `AddReaction` / `RemoveReaction` | after the permission check, via `msg.channel_id` | `E2EE_REFUSED` |
| `CreateThread` | after the permission check, via the parent's `channel_id` | `E2EE_REFUSED` |
| `CreateWebhook` | after the channel-exists check | `E2EE_REFUSED` |
| `RunCommand` (all six kinds) | inside `check_run_command_channel_auth`, before the trigger is even looked up | `E2EE_REFUSED` |
| `FetchUrl` | `connection::handle_fetch_url`, before the outbound HTTP fetch | `E2EE_REFUSED` |
| every widget request | inside `widget_channel_visible`, which now answers `false` for any non-plaintext class | that arm's **existing opaque** string (`"poll not found"`, `"giveaway not found"`, `EVENT_NOT_FOUND`, `"channel not found"`) |
| incoming webhook delivery | `webhooks::deliver`, after `find_by_token` | `WebhookAck::Unauthorized` (the existing opaque ack) |
| giveaway draw + event start announcements | `widgets::sweep_once`, before the guarded UPDATE | skip and `continue`, logged at `warn!` |

Two deliberate choices in that table:

- **Widget arms keep their own opaque errors.** A distinguishable "encrypted
  channel" answer would let a widget id classify a channel, so the gate is
  expressed as *invisibility* — the same answer the arm gives for a widget that
  does not exist. No widget can exist in an E2EE channel anyway (create is
  refused at `RunCommand`); this is defence in depth.
- **`RunCommand`'s gate is kind-agnostic by construction.** It sits in
  `check_run_command_channel_auth`, which `connection.rs` calls before the
  command lookup, so `text`, `api`, `poll`, `giveaway`, `event` and `reminder`
  are all covered by one check. `reminder` matters even though it posts nothing:
  `reminders.text` is stored server-side in plaintext.

`request_requires_membership` keeps its default-deny shape — the bootstrap
allow-list is still exactly `SubmitEvent`, `ResolveInvite`, `GetServerInfo`,
`GetMembershipStatus`, pinned by an exhaustive-`match` test that fails to
compile when a new `ServerRequest` variant is added without being classified.

---

## Event ingest helpers (`crates/farder-server/src/event_ingest.rs`)

These public functions are the source of truth for deriving persistent rows from
the immutable event log. They are called inside the `SubmitEvent` handler and
also by the startup reconciliation path in `main.rs`.

### `check_ingest_caps(event) -> Result<()>` / `check_ingest_caps_at(event, now) -> Result<()>`

The bounds the **fold deliberately does not own** (sub-2 resolved ambiguity #9).
Two jobs, both blind — every rule is a byte count, a vector length or a clock
comparison, and no payload field is inspected for meaning:

1. **Per-variant size caps.** The constants live in
   `farder_crypto::event_log` (see `docs/modules/crypto.md` → "Ingest caps");
   `LogState::apply` reads none of them, so unbounded is unbounded until here.
2. **The `core.timestamp` upper bound** (`MAX_EVENT_FUTURE_SKEW_SECS`, 300s).
   Applies to **every** variant, not only the Rung-2 ones — `core.timestamp` is
   the fold's device-liveness and cert-expiry clock for all of them.

`check_ingest_caps` reads the clock itself; `check_ingest_caps_at` takes it as a
parameter so the skew bound is testable without racing a real second boundary.

**Ordering is load-bearing.** This runs at the very top of the `SubmitEvent`
arm, before the `LogState` clone that `apply` validates against — that clone is
the allocation-heavy step of ingest, and a cap breach must not be able to buy
it. Observed, not asserted: an oversized payload signed by a device the log has
never authorized still returns the *cap* error, not the fold's device error
(`oversized_sealed_ciphertext_is_refused_before_the_fold_runs`).

---

### `materialize_channel_created(conn, event) -> Result<Option<ChannelClass>>`

Creates the `channels` row for an accepted `ChannelCreated`, **with its
`content_class`**, inside the same transaction that stores the event — so the
log and its mirror cannot disagree across a crash, and any refusal here rolls
back the stored event too (no channel row, no log advance, nothing broadcast).
Returns `None` for every other payload.

The fold has already authorized the event (owner-authored, id never seen in the
log, no plaintext history in the log, parent class inherited). What ingest adds
is everything the fold cannot see because it lives in the **legacy DB**, plus
the shape this rung supports:

| refusal | why |
|---|---|
| `channel_id < E2EE_CHANNEL_ID_FLOOR` (`1 << 32`) | the id is **client-chosen**; the floor keeps it clear of the `channels` AUTOINCREMENT space so it can never adopt a legacy channel |
| the id already exists in `channels` | the log's own immutability rule cannot see legacy DB rows |
| the id already has rows in `messages` | belt-and-braces for the case the fold's `plaintext_history_channels` cannot catch — a legacy DB channel carrying plaintext the log never saw. Declaring E2ee over it would put a lock icon on messages every host already read |
| `kind != "text"` or `parent: Some(_)` | this rung accepts text channels only; threads under a sealed parent are refused by the spec (coexistence row 12) and categories are legacy DB state with no log representation. The fold's parent-class inheritance rule stays live for a later rung |

---

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

A server-managed bot is represented in two tables:

- **`bots`**: `(public_key, secret_key, kind, coin_id, label, source_url, value_path, unit, created_at)` — the server owns and holds the bot's Ed25519 secret key. `kind` is `'crypto_ticker'` for CoinGecko-price bots and `'custom_api'` for custom-monitor bots. `source_url`, `value_path`, and `unit` are `NULL` for `crypto_ticker` bots and populated for `custom_api` bots.
- **`members`**: a normal members row with `is_bot=1`; the display name (label or name) is the `display_name`; `joined_at` is set at registration time.

The bot has no human operator and cannot authenticate a client connection — it exists only so it appears in the member roster (`GetMembers`) and can have a `Presence` broadcast on its behalf by the poller.

### Data storage: `server_settings` KV table

Bot configuration is stored in the `server_settings` table (schema: `key TEXT PRIMARY KEY, value TEXT NOT NULL`). Currently the only bot-related key is:

| Key | Value | Default |
|---|---|---|
| `"bot_poll_interval"` | Poll interval in seconds (stored as a decimal string). Always ≥30 when read back. | 60 (used when the key is absent) |

Read via `db::get_setting`; written via `db::set_setting` (upsert semantics).

### DB helpers

| Function | Signature | What it does |
|---|---|---|
| `register_bot` | `(conn, pk, secret, coin_id, label) -> Result<()>` | Inserts a `crypto_ticker` row into `bots` (`source_url`/`value_path`/`unit` left `NULL`). |
| `register_custom_bot` | `(conn, pk, secret, name, source_url, value_path, unit: Option<&str>) -> Result<()>` | Inserts a `custom_api` row into `bots` with `coin_id=''` and the custom-monitor fields populated. |
| `list_bots` | `(conn) -> Result<Vec<BotRecord>>` | Returns every `BotRecord { public_key, coin_id, label, kind, source_url, value_path, unit }` row **except the system identity** (`WHERE kind != 'system'`) — the poller must never poll it (empty `coin_id`) and no bot UI should enumerate it. |
| `get_or_create_system_identity` | `(conn) -> Result<PublicKey>` | The server's OWN identity, **lazily** created on first use (never at boot / in `init_schema`) and reused forever. Inserts a `bots` row with `kind='system'`, `label='Farder'` **and** a `members` row via `register_bot_member` (required — `build_member_info` errors without it). Idempotent: callers hold the single `state.db` mutex, plus the `idx_bots_system` partial unique index as belt-and-braces. **Self-heals**: on the already-exists branch it re-registers the `members` row if it has gone missing (two un-transacted INSERTs, and the pk is public via `DmCreated.participant`) — otherwise one delete would silently kill every system DM forever. |
| `is_system_identity` | `(conn, pk) -> Result<bool>` | `true` iff `pk` is the `bots` row with `kind = 'system'`. The shared moderation predicate behind the `RemoveBot` / `KickMember` / `BanMember` refusals. |
| `remove_bot` | `(conn, pk) -> Result<()>` | Deletes all `bot_alerts` and `bot_subscriptions` rows for `pk` (cascade), then deletes from `bots`. The `members` row is deleted separately by `handlers.rs` via `members::remove_member_row`. |
| `get_poll_interval` | `(conn) -> u64` | Reads `"bot_poll_interval"` from `server_settings`; floors at `POLL_INTERVAL_FLOOR` (30); returns `POLL_INTERVAL_DEFAULT` (60) when unset. |
| `set_poll_interval` | `(conn, secs: u64) -> Result<()>` | Writes `secs.max(POLL_INTERVAL_FLOOR)` to `server_settings` under `"bot_poll_interval"`. |
| `add_alert` | `(conn, bot: &PublicKey, metric, comparator, threshold) -> Result<i64>` | Inserts a `bot_alerts` row with `armed=1`; returns the new row id. |
| `remove_alert` | `(conn, id: i64) -> Result<()>` | Deletes a single `bot_alerts` row by id. |
| `list_alerts_for_bot` | `(conn, bot: &PublicKey) -> Result<Vec<AlertRow>>` | Returns all `AlertRow` structs for a bot (`id`, `metric`, `comparator`, `threshold`, `armed`). |
| `set_alert_armed` | `(conn, id: i64, armed: bool) -> Result<()>` | Updates the `armed` column for a single alert (called by the poll loop after a fire or re-arm). |
| `subscribe` | `(conn, bot: &PublicKey, subscriber: &PublicKey) -> Result<()>` | `INSERT OR IGNORE` into `bot_subscriptions` — idempotent. |
| `unsubscribe` | `(conn, bot: &PublicKey, subscriber: &PublicKey) -> Result<()>` | Deletes the `(bot, subscriber)` row; no-op if absent. |
| `list_subscribers_for_bot` | `(conn, bot: &PublicKey) -> Result<Vec<PublicKey>>` | Returns all subscriber public keys for a bot; used by the poll loop to fan out alert DMs. |
| `list_subscriptions_for_user` | `(conn, subscriber: &PublicKey) -> Result<Vec<PublicKey>>` | Returns all bot public keys a user is subscribed to; used by `ListMySubscriptions`. |

### `unknown_coin_presence() -> Presence`

Returns `Presence { kind: Ticker, details: "unknown coin", state: None }`. The poller calls this for any bot whose `coin_id` was absent from a **successful** CoinGecko response — indicating a bad or misspelled id rather than a transient network failure. The `"unknown coin"` string is rendered by the client's `formatPresence` path (same path as a normal ticker) so no client-side change was needed.

### `unavailable_presence() -> Presence`

Returns `Presence { kind: Ticker, details: "unavailable", state: None }`. The poller calls this for any `custom_api` bot whose JSON fetch failed or whose dot-path extraction returned `None` (missing key, non-numeric leaf). The bot continues showing `"unavailable"` until the next successful poll cycle.

### `ticker_presence(p: &PriceInfo) -> Presence`

Composes a `Presence { kind: Ticker, details: "$<price> <arrow><pct>%", state: Some("24h") }`. The arrow glyph is `\u{25B2}` (▲) for non-negative 24h change, `\u{25BC}` (▼) for negative. Used both by the poller and by unit tests.

### `fetch_prices(coin_ids: &[String]) -> Result<HashMap<String, PriceInfo>>`

Fetches USD price and 24h percentage change for all given CoinGecko ids in a single `GET /api/v3/simple/price` call. SSRF-guarded: the constructed URL is pre-validated by `ssrf::resolves_to_global` before any network I/O. 10-second timeout; redirects disabled. Returns only entries where a valid `usd` f64 was present in the JSON response. Non-2xx HTTP status is surfaced as an error (so rate-limits/blocks are not silently treated as empty responses).

### `spawn_bot_poll_task(state: Arc<ServerState>) -> JoinHandle<()>`

Spawns a background `tokio::spawn`. The loop is **poll-then-sleep**: it executes one poll cycle immediately on startup, then reads the current interval from `server_settings` and sleeps for that duration before the next cycle. Because the interval is re-read at the start of each sleep, changes made via `SetBotPollInterval` take effect after the current sleep expires — no server restart is needed.

On each poll cycle:

1. **Snapshot** — reads the bot list from SQLite, releasing the `Mutex<Connection>` lock before any `.await`.
2. **Coalesce** — deduplicates `coin_id` values for `crypto_ticker` bots only and issues ONE `fetch_prices` network call for all of them.
3. **Branch by kind** — for each bot:

   **`custom_api` bots:**
   - Calls `fetch_json(source_url)` (SSRF-guarded, 10 s timeout, 256 KiB cap, redirects disabled).
   - Calls `extract_dot_path(&json, value_path)` to walk the dot-separated key chain and coerce the leaf to `f64`.
   - On success: calls `custom_value_presence(value, unit)` (formats `"<value> <unit>"` or just `"<value>"`), stores + broadcasts the `Presence` via `broadcast_presence`, then calls `eval_and_notify_alerts` with `metrics=[("value", value)]`.
   - On any fetch or extract failure: calls `unavailable_presence()` and broadcasts it. No alert evaluation.

   **`crypto_ticker` bots:**
   - On a **network error** for the batch fetch (prices = `None`): all crypto bots **skip this cycle**, retaining their last-known presence. Nothing is broadcast. The error is logged at `WARN` level.
   - On a **successful** batch fetch: for each bot, if the coin ID appeared in the response calls `ticker_presence`; if absent (unknown/misspelled id) calls `unknown_coin_presence()`. Stores + broadcasts via `broadcast_presence`, then calls `eval_and_notify_alerts` with `metrics=[("price_usd", usd), ("change_24h", change_24h)]`.

   The updated `Presence` is stored in `state.presences` (RwLock) and broadcast as `ServerEvent::MemberPresenceUpdated` to all connected clients via `connection::broadcast_event`.

**Note on fetch-failure behavior:** crypto bots keep their last presence on a network error (no `"unknown coin"` flip); `"unknown coin"` only appears when the batch call succeeded but a specific coin id was absent from the response. Custom bots show `"unavailable"` on any per-bot fetch or extract failure.

**Roster whitelist for mesh servers:** when `GetMembers` is handled for a log-mode server, the member list is filtered to `m.is_bot || ls.is_member(&m.public_key)` so bots always appear alongside confirmed log-space members. The system identity is already gone by then (`list_members_visible`), so the `is_bot ||` clause cannot leak it.

### Alert engine

#### `evaluate_alert(value: f64, comparator: &str, threshold: f64, armed: bool) -> (bool, bool)`

Fire-once with hysteresis. Returns `(did_fire, new_armed)`:

| State | Condition | Result |
|---|---|---|
| Armed | Met (`value > threshold` or `value < threshold`) | `(true, false)` — fire and disarm |
| Disarmed | Not met (condition cleared) | `(false, true)` — re-arm |
| Otherwise | — | `(false, armed)` — no change |

This ensures each alert fires **exactly once** per crossing, then re-arms only after the condition clears. The poll loop calls `set_alert_armed` to persist the new state after evaluation.

#### `metric_value(p: &PriceInfo, metric: &str) -> Option<f64>`

Maps a metric name to a f64 value from a `PriceInfo` snapshot:

| `metric` | Returns |
|---|---|
| `"price_usd"` | `Some(p.usd)` |
| `"change_24h"` | `Some(p.change_24h)` |
| Any other | `None` |

#### `format_alert_message(label, metric, comparator, threshold, p) -> String`

Formats a human-readable alert body for a fired alert. Examples:
- `price_usd above 70000` at $71234: `"🔔 BTC crossed above $70000.00 — now $71234.00"`
- `change_24h below -5` at -6.1%: `"🔔 BTC 24h change below -5.0% — now -6.1% ($65000.00)"`

#### Alert evaluation inside the poll loop

Alert evaluation is performed by the shared `eval_and_notify_alerts` helper for both `crypto_ticker` and `custom_api` bots. The per-bot sequence is:

1. **Under the DB lock:** load alerts for the bot, call `evaluate_alert` for each metric that matches an alert's `metric` string, persist armed-state changes via `set_alert_armed`, collect fired alerts. Drop the lock.
2. **Under a fresh DB lock:** load subscribers for the bot. Drop the lock.
3. **Without any DB lock held:** for each fired alert × each subscriber, call `send_bot_dm` (which is `async`). Failures are logged at `WARN` level and do not abort the remaining DMs.

This three-step pattern ensures the `Mutex<Connection>` is never held across an `.await`.

**Metric names by bot kind:** `crypto_ticker` bots pass `metrics=[("price_usd", usd), ("change_24h", chg)]`; `custom_api` bots pass `metrics=[("value", v)]`. `AddBotAlert` accepts `"value"` (alongside `"price_usd"`/`"change_24h"`), and the client alert form exposes a **Value (custom bots)** option, so custom-bot alerts are configurable end-to-end. (The metric dropdown shows all three options regardless of bot kind; the owner picks the one matching the bot — a per-kind filtered dropdown is a possible refinement.)

#### `eval_and_notify_alerts(state, bot, metrics, make_message) -> (async)`

Shared helper called by both the `crypto_ticker` and `custom_api` branches after presence is broadcast. Evaluates all armed alerts for `bot` against the supplied `metrics` slice (a `&[(&str, f64)]` of `(metric_name, value)` pairs), persists armed-state changes, collects fires, then for each fire × each subscriber calls `send_bot_dm` with the message produced by `make_message(metric, comparator, threshold) -> String`. The `MutexGuard<Connection>` is dropped before any `.await` (two separate lock scopes for alerts + subscribers).

#### `broadcast_presence(state, bot, presence) -> (async)`

Shared helper: inserts `presence` into `state.presences` (RwLock) and broadcasts `ServerEvent::MemberPresenceUpdated { public_key: bot.public_key, presence: Some(presence) }` to all connected clients via `connection::broadcast_event`. Called by both `crypto_ticker` and `custom_api` branches.

### Custom monitor bot helpers

#### `fetch_json(url: &str) -> Result<serde_json::Value>` (async)

Fetches the given URL as JSON. SSRF-guarded: the URL is pre-validated by `ssrf::resolves_to_global` before any network I/O — requests resolving to private/loopback/link-local addresses are rejected (returns `Err`). Request settings: 10-second timeout, redirects disabled, `User-Agent: farder-bot/1.0`. Non-2xx responses return `Err`. Responses larger than 256 KiB return `Err` (soft cap applied post-read). On any error the caller treats the bot as `"unavailable"` for this cycle.

Note: only server owners (MANAGE_SERVER-gated `AddCustomBot`) can configure the URL, so the security perimeter is owner trust, not arbitrary-user trust.

#### `extract_dot_path(v: &serde_json::Value, path: &str) -> Option<f64>`

Walks a dot-separated key path into a JSON value and coerces the leaf to `f64`. Returns `None` on any of: empty path, missing key at any segment, non-numeric and non-numeric-string leaf. String leaves are coerced via `str::parse::<f64>()`. Examples: `"data.online.count"` on `{"data":{"online":{"count":102433}}}` → `Some(102433.0)`; `"data.online.count"` on `{"data":{"online":{"name":"foo"}}}` → `None`.

#### `custom_value_presence(value: f64, unit: Option<&str>) -> Presence`

Formats the inline display for a custom monitor bot: `"<value> <unit>"` or just `"<value>"` when `unit` is `None` or empty. Integer values (zero fractional part, absolute value < 1e15) are formatted with thousands separators (e.g. `"102,433"`); non-integer values are formatted as `"{v:.2}"`. Returns `Presence { kind: Ticker, details, state: None }`.

#### `format_custom_alert_message(label, comparator, threshold, value, unit) -> String`

Formats a human-readable alert body for a fired custom-api alert. Example: `"🔔 RuneScape crossed above 100,000 players — now 102,433 players"`. Both the threshold and current value are formatted by the same thousands-separator logic as `custom_value_presence`.

### E2EE DM delivery (bot → user)

#### `get_bot_secret(conn: &Connection, pk: &PublicKey) -> Result<Option<[u8; 32]>>`

Reads the raw 32-byte Ed25519 secret key for the given bot from `bots.secret_key`. Returns `Ok(None)` if the bot does not exist.

#### `encrypt_bot_dm(bot_ed_sk: &[u8; 32], recipient_ed_pk: &[u8; 32], text: &str) -> Result<String>`

Encrypts `text` as a DM from the bot to the recipient:
1. Calls `farder_crypto::key_exchange::derive_dm_shared_secret(bot_ed_sk, recipient_ed_pk)` — symmetric X25519 ECDH, same shared secret the recipient derives from their own key + the bot's public key.
2. Calls `farder_crypto::encryption::encrypt(&shared, text.as_bytes())` — AES-256-GCM with a random 12-byte nonce prepended.
3. Returns the result as a lowercase hex string (`nonce(12) || ciphertext+tag`).

The recipient decrypts with their normal `dm_decrypt` Tauri command (passing the bot's public key as `their_public_key`).

#### `send_bot_dm(state: &Arc<ServerState>, bot_pk: &PublicKey, recipient_pk: &PublicKey, text: &str) -> Result<()>`

Sends a full E2EE DM from a server-managed bot to a recipient member. All DB and crypto work is scoped inside a block that drops the `Mutex<Connection>` guard before any `.await`:

1. **Under the DB lock:**
   - `get_bot_secret` — load the bot's Ed25519 secret key.
   - `channels::open_dm_channel` — find or create the DM channel between bot and recipient.
   - `encrypt_bot_dm` — encrypt the text (X25519 + AES-256-GCM).
   - `messages::insert_message` — persist the ciphertext as a message in the DM channel.
   - `messages::get_message` — read the persisted `MessageInfo` (so the recipient gets a proper timestamp and id).
   - `channels::get_channel` — read the `ChannelInfo` for the DM channel.
   - `build_member_info` — build a `MemberInfo` for the bot (for `DmCreated`).
   - Drop the lock.
2. **Without any lock held (async):**
   - If the DM channel was just created: `broadcast_event(EventTarget::Members([recipient]), DmCreated { channel, participant: bot_member })`.
   - Always: `broadcast_event(EventTarget::Members([recipient]), NewMessage { message })`.

`EventTarget::Members([recipient])` ensures the DM is delivered only to the intended recipient, not broadcast to all connected clients. The message persists in the DM channel so the recipient sees it even if they were offline during the poll cycle.

Since the system identity landed, `send_bot_dm` is a thin wrapper: it delegates to `send_bot_dm_as(state, bot_pk, recipient_pk, text, None, None)`.

#### `send_bot_dm_as(state, bot_pk, recipient_pk, text, name_override: Option<&str>, badge: Option<&str>) -> Result<()>`

The body described above, with `messages::insert_message` swapped for `messages::insert_message_with_author_name` so a sender that is deliberately **absent from the client's member map** still renders with a name and a badge. Lock discipline is unchanged.

#### `send_system_dm(state, recipient_pk, text) -> Result<()>`

Sends a DM as the server itself. Takes its own scoped lock to resolve/mint the system identity (`get_or_create_system_identity`), **drops it**, then delegates to `send_bot_dm_as(.., Some("Farder"), Some("BOT"))`. The caller must NOT hold the DB mutex — this is why `widgets::sweep_once` returns DMs as `PendingDm` data instead of sending them inline. Badge `"BOT"` is reused deliberately: a `"SYSTEM"` badge would need CSS in three themes for no product gain.

**Roster invisibility.** The system identity holds no roles, is excluded from `GetMembers` (which calls `members::list_members_visible`, filtering in SQL **before** the mesh `is_bot ||` whitelist can re-admit it), is excluded from `bots::list_bots`, cannot be removed via `RemoveBot`, and cannot authenticate a connection (no auth path reads `bots.secret_key`). Because `BotsTab` and `MemberSidebar` both derive from `activeServer.members`, that one filter removes it from both — no client change needed.

---

## `webhooks.rs` — incoming webhook delivery

> **File:** `crates/farder-server/src/webhooks.rs`

Implements the full lifecycle of incoming webhooks: CRUD (create / list / delete / regenerate token), payload parsing, and message delivery.

### Data model

Two schema additions (see `db.rs`):

- **`webhooks` table** — `(id, channel_id, token, name, public_key, created_at)`. `token` is a 64-hex (256-bit) random string, UNIQUE. `public_key` is a fresh Ed25519 key generated at creation time (the webhook's author identity — never a roster member). Tokens are never returned by list/read operations.
- **`messages.author_name_override` column** — `TEXT`, nullable. Stores the display name for webhook-posted messages (per-delivery `username` overrides the registered webhook `name`). `NULL` for all normal member messages.

### `parse_webhook_payload(body: &[u8]) -> Result<WebhookPayload>`

Parses a Discord-compatible JSON body. Accepts `{"content": "...", "username": "..."}` and ignores all other fields (embeds, avatar_url, etc.). Rules:
- `content` must be present, non-empty after trim. Missing or whitespace-only → error.
- `username` is optional; capped at 80 chars (Discord's limit).
- Non-JSON input → error.

Returns `WebhookPayload { content: String, username: Option<String> }`.

### CRUD helpers

| Function | Signature | What it does |
|---|---|---|
| `create` | `(conn, channel_id, name) -> Result<(i64, String)>` | Inserts a webhook row. Returns `(id, token)` — token shown once. |
| `regenerate_token` | `(conn, id) -> Result<Option<String>>` | Updates `token` to a fresh random value. Returns `Some(new_token)` or `None` if id not found. |
| `delete` | `(conn, id) -> Result<()>` | Deletes the webhook row. |
| `list_for_channel` | `(conn, channel_id) -> Result<Vec<WebhookRow>>` | Lists all webhooks for the channel (no `token` field in `WebhookRow`). |
| `find_by_token` | `(conn, token) -> Result<Option<WebhookRow>>` | Used during delivery to look up the webhook by its secret token. |

### `deliver(state, token, body) -> WebhookAck`

Called by `serve_via_relay`'s `Webhook` dispatch arm when the relay forwards an inbound HTTP POST. Full delivery sequence:

1. Body size guard: returns `WebhookAck::TooLarge` if `body.len() > 64 KiB` (secondary guard; the relay-side HTTP layer enforces the same limit earlier).
2. **Under the DB lock** (synchronous block so no `Mutex<Connection>` guard crosses an `.await`):
   a. `find_by_token` — if token not found, returns `WebhookAck::Unauthorized`.
   b. `parse_webhook_payload` — if content is missing/empty/invalid JSON, returns `WebhookAck::BadRequest`.
   c. Content is capped at 8 000 chars (Discord's message limit).
   d. Display name: `payload.username` (per-delivery override) takes priority over the webhook's registered `name`.
   e. `messages::insert_message_with_author_name(conn, channel_id, &wh.public_key, &content, None, Some(&display))` — inserts the message with the webhook's synthetic public key as author and the display name in `author_name_override`.
   f. `messages::get_message` — reads the full `MessageInfo` (with populated `author_name_override`).
   g. DB lock is released.
3. **Without any lock held** (async): `broadcast_event(EventTarget::Subscribers(channel_id), ServerEvent::NewMessage { message })`.

Returns `WebhookAck::Ok` on success. The ack is mapped to HTTP status by `run_relay_webhook` in `relay.rs`: Ok → 204, Unauthorized → 401, BadRequest → 400, TooLarge → 413.

### `ServerState.relay_server_id`

`Mutex<Option<String>>` set at startup when the server registers with a relay. Contains the hex id the relay uses to route webhook POSTs. `handlers.rs` reads this when returning `ServerResponse::WebhookToken` so the client can build the ingest URL. `None` for direct (non-relay) servers.

### Webhook handler arms in `handlers.rs`

| `ServerRequest` | Permission | Action |
|---|---|---|
| `CreateWebhook { channel_id, name }` | `MANAGE_SERVER` | Validates channel exists; trims `name` (1–64 chars); calls `webhooks::create`; reads `relay_server_id`; returns `WebhookToken { id, token, server_id_hex }`. |
| `RegenerateWebhookToken { id }` | `MANAGE_SERVER` | Calls `webhooks::regenerate_token`; returns `WebhookToken { id, new_token, server_id_hex }` or `Error` if not found. |
| `DeleteWebhook { id }` | `MANAGE_SERVER` | Calls `webhooks::delete`; returns `Ok`. |
| `ListWebhooks { channel_id }` | `MANAGE_SERVER` | Calls `webhooks::list_for_channel`; maps to `WebhookInfo` structs; returns `Webhooks { webhooks }`. |

---

## `commands.rs` — slash-command CRUD and utilities

> **File:** `crates/farder-server/src/commands.rs`

Implements CRUD for server-configured `/trigger` slash commands plus the utility functions used by the async `RunCommand` dispatch in `connection.rs`.

### Data model

- **`commands` table** — `(id, trigger, name, description, kind, body_text, url_template, value_path, response_template, unit, public_key, created_at)`. `trigger` is UNIQUE and stored lowercase. `public_key` is a fresh Ed25519 key generated at creation time — the command's author identity, **never a roster member**. `url_template` and `body_text` are server-only; they are never sent to clients.
- **`CommandRow`** (internal) — full DB row including secrets. Used by `find_by_trigger` in the `RunCommand` dispatch.
- **`CommandInfo`** (protocol) — safe-fields-only view (`id`, `trigger`, `description`, `takes_arg`, `kind`). `kind` is not sensitive (it names the command's behavior class, used by the client's builder UIs); `url_template`/`body_text` remain excluded. Returned by `list_infos` and sent to clients.

### CRUD helpers

| Function | Signature | What it does |
|---|---|---|
| `create` | `(conn, name, trigger, description, kind, body_text, url_template, value_path, response_template, unit) -> Result<i64>` | Inserts a command row. Generates a fresh Ed25519 keypair; stores the public key as the command's author identity. Returns the new row id. |
| `delete` | `(conn, id) -> Result<()>` | Deletes a command row by id. |
| `list_rows` | `(conn) -> Result<Vec<CommandRow>>` | Full rows ordered by trigger. Includes secrets — server-internal only. |
| `list_infos` | `(conn) -> Result<Vec<CommandInfo>>` | Safe-fields-only list for clients. `takes_arg` is `true` for kinds `"api"`, `"poll"`, `"giveaway"`, `"event"`, `"reminder"`; `kind` is passed through verbatim. |
| `find_by_trigger` | `(conn, trigger) -> Result<Option<CommandRow>>` | Look up a command by its trigger string. `None` if not found. Used by the `RunCommand` dispatch in `connection.rs`. |

### Dispatch utilities

| Function | Signature | What it does |
|---|---|---|
| `build_command_url` | `(template: &str, args: &str) -> String` | Percent-encodes `args` (preserving path chars `/`, `-`, `.`, `_`, `~`; encoding space, `&`, `?`, `=`, `#`, `%`, etc.) and substitutes into `{arg}` in `template`. Path args like `owner/repo` pass through unchanged; injection attempts like `a b&c=x` are encoded. If `template` contains no `{arg}`, returns it unchanged. |
| `format_response` | `(template: Option<&str>, args: &str, value: f64, unit: Option<&str>) -> String` | Formats the bot reply. If `template` is non-empty, substitutes `{arg}` and `{value}` (thousands-formatted via `bots::format_thousands`). Falls back to `"<value> <unit>"` or just `"<value>"` if template and unit are absent. |

### Slash command handler arms in `handlers.rs`

CRUD arms run synchronously inside the standard `handle_request` dispatch. `RunCommand` is deliberately absent from that dispatch (it returns an `Error` stub) because it requires an async HTTP fetch.

| `ServerRequest` | Permission | Action |
|---|---|---|
| `ListCommands {}` | none (any member) | Calls `commands::list_infos(conn)`; returns `Commands { commands }`. `takes_arg` is `true` for kinds `"api"`, `"poll"`, `"giveaway"`, `"event"`, `"reminder"`. |
| `AddCommand { ... }` | `MANAGE_SERVER` | Validates all fields; checks trigger uniqueness via `find_by_trigger`; calls `commands::create`; returns `Ok`. Interactive kinds (`"poll"`, `"giveaway"`, `"event"`, `"reminder"`) accept no kind-specific fields — the arg string is parsed at dispatch ("kind must be 'text', 'api', 'poll', 'giveaway', 'event' or 'reminder'" otherwise). |
| `DeleteCommand { id }` | `MANAGE_SERVER` | Calls `commands::delete(conn, id)`; returns `Ok`. |
| `RunCommand { .. }` | — | Stub arm — returns `Error("RunCommand must be handled at the connection level")`. See the `RunCommand` dispatch note below. |

### Widget interaction arms (polls, giveaways & events)

Fifteen synchronous arms in `handle_request` servicing the message widgets — see `docs/modules/server-widgets.md` for the storage modules and `protocol.md` for the full variant table. Shared shape of every arm: load the row → **channel visibility** via `widget_channel_visible(conn, member, channel_id, is_owner)` (DM ⇒ participant, else `VIEW_CHANNEL`; missing channel ⇒ false) with an **opaque** `Error { "poll not found" }` / `"giveaway not found"` / `"event not found"` on any failure (no existence oracle; the event arms share the `visible_event(conn, member, is_owner, event_id)` preamble + the `EVENT_NOT_FOUND` const so the string cannot drift) → then status/authz checks. Mutating arms are `is_timed_out`-gated (**`CancelReminder` excepted** — a private nudge is not channel content; see its row above); `VotePoll`/`RetractVote`/`EnterGiveaway`/`LeaveGiveaway`/`RsvpEvent`/`ClearRsvp`/`CancelEvent`/`EditEvent`/`CancelReminder` also pass through `state.widget_limiter` (10 / 10 s per member, "slow down — too many interactions") — spec §10. `EditEvent` is the one that genuinely needs it: it is repeatable, reachable by any ordinary member who created the event, and each call is a DB write plus up to 30 member lookups under the db mutex plus a full `EventInfo` broadcast to every channel subscriber.

| `ServerRequest` | Permission | Action |
|---|---|---|
| `GetPoll` / `GetGiveaway` | visibility only (read; not rate-limited) | Returns `Poll { poll, my_vote }` / `Giveaway { giveaway, my_entered }` — the `my_*` field is the caller's own state only. |
| `VotePoll` / `RetractVote` | visibility + not timed out + limiter | `polls::vote` / `polls::retract` on an open poll; broadcasts `PollUpdated` → `Subscribers(channel_id)`. |
| `ClosePoll` | creator or `MANAGE_SERVER` | `polls::close` (idempotent); broadcasts `PollUpdated`. |
| `EnterGiveaway` / `LeaveGiveaway` | visibility + not timed out + limiter | `giveaways::enter` / `leave` (idempotent) on an open giveaway; broadcasts `GiveawayUpdated`. |
| `CancelGiveaway` | creator or `MANAGE_SERVER` | `giveaways::cancel` — no draw, no announcement; broadcasts `GiveawayUpdated`. |
| `RerollGiveaway` | creator or `MANAGE_SERVER` | `giveaways::reroll_and_announce` on an `"ended"` giveaway with a winner; broadcasts `GiveawayUpdated` then the announcement `NewMessage`. |
| `ListActiveWidgets` | visibility only (read; not rate-limited; allowed while timed out) | Visibility is checked on the requested `channel_id` itself (the only client-supplied field) with an opaque `Error { "channel not found" }` for missing AND invisible channels. Calls `polls::list_open_in_channel` + `giveaways::list_open_in_channel` + `channel_events::list_upcoming_in_channel` (each `LIMIT 20`), **three-way** merges by `created_at` ascending (ties ordered poll → giveaway → event), truncates to 20 combined, `build_info`s each; returns `ActiveWidgets { polls, giveaways, events }`. No per-viewer fields, no broadcasts. |
| `GetEvent` | visibility only (read; not rate-limited; allowed while timed out) | Returns `Event { event, my_rsvp }` — `my_rsvp` is the caller's own RSVP only and never rides in the broadcast. |
| `RsvpEvent` | visibility + not timed out + limiter | Response must be `"going"`/`"maybe"`/`"no"` (else `"invalid RSVP"`); rejected on a cancelled event and once `now >= starts_at` (exact before the sweeper ticks). `channel_events::rsvp` upsert; broadcasts `EventUpdated` → `Subscribers(channel_id)`. |
| `ClearRsvp` | visibility + not timed out + limiter | `channel_events::clear_rsvp`; **no row deleted → plain `Ok` with no event** (the `RetractVote` rule). |
| `CancelEvent` | creator or `MANAGE_SERVER` (+ not timed out + limiter) | Upcoming only (`"event already ended or cancelled"`); `channel_events::cancel`; broadcasts `EventUpdated`. **No DMs here** — a sync arm cannot `.await`; the sweeper's cancel-notify pass DMs the Going list within one tick. |
| `EditEvent` | creator or `MANAGE_SERVER` (+ not timed out + limiter) | Upcoming only; full replace re-running the creation validation (`channel_events::validate_event_fields` + `resolve_start` + `REMIND_LEADS` membership). A changed `starts_at` passes `rearm_reminder = true`, NULLing `reminded_at`. Broadcasts `EventUpdated`. Unlike cancel it does **not** self-limit (it is repeatable and open to any member who created the event, and each call costs a write + up to 30 member lookups + a full `EventInfo` fan-out), so `state.widget_limiter` is the only bound. |

The `DeleteMessage` arm additionally parses the deleted message's `widget` JSON: an open poll is closed (+`PollUpdated`), an open giveaway is cancelled (+`GiveawayUpdated`), an **upcoming event is cancelled** (+`EventUpdated`; the row is retained for audit, the Going list gets the sweeper's cancellation DM, and no start announcement can ever post afterwards) — deleting an already-ended/cancelled/started card is a no-op.

### `RunCommand` dispatch in `connection.rs`

`RunCommand` is handled asynchronously in the connection read-loop (mirroring `FetchUrl`) because `"api"` commands require an outbound HTTP fetch. The dispatch sequence:

1. **Content gate** — `handlers::content_block_reason(&state, &member_key)` returns `Some(reason)` if the member is pending approval or not in the member log; returns `Error { reason }` immediately.
2. **Rate limit** — `state.command_limiter.allow(&caller_bytes)` (5 runs / 10 s per user); returns `Error { reason: "slow down..." }` on excess.
3. **Command lookup** — `commands::find_by_trigger(&conn, &trigger.trim().to_lowercase())` under a scoped DB lock (released before any `.await`). Returns `Error` if not found.
4. **Content resolution** (no lock held):
   - `"text"` commands: use `cmd.body_text` directly.
   - `"api"` commands: call `commands::build_command_url(url_template, args)`, then `bots::fetch_json(&url)` (SSRF-guarded via `ssrf::resolves_to_global`; rejects private/loopback IPs, 10 s timeout, redirects disabled, 256 KiB cap); on success call `bots::extract_dot_path(&json, value_path)` to get the numeric leaf, then `commands::format_response(response_template, args, value, unit)`.
   - `"poll"` commands: `polls::parse_poll_args(args)` (pure), then `polls::create_poll_card` under one scoped DB lock (card message + `polls` row + widget JSON in one transaction); broadcasts `NewMessage` then `PollUpdated`.
   - `"giveaway"` commands: **MANAGE_SERVER re-checked at dispatch** ("giveaways can only be started by moderators (missing MANAGE_SERVER)"), then `giveaways::parse_giveaway_args(args)` and `giveaways::create_giveaway_card`; broadcasts `NewMessage` then `GiveawayUpdated`.
   - `"event"` commands: **no MANAGE_SERVER gate** (events are social — the creation permission is exactly the gates above). `channel_events::parse_event_args(args)` (pure), then `channel_events::resolve_start` BEFORE the lock so a bounds violation is a plain user-facing `Error` rather than an `internal error:`, then `channel_events::create_event_card` under one scoped DB lock (card message + `channel_events` row + widget JSON in one transaction, **plain invoker authorship**); broadcasts `NewMessage` then `EventUpdated`.
   - `"reminder"` commands: **private** — `reminders::parse_reminder_args(args)` (pure), then one scoped DB lock doing `count_pending` (20/user cap) + `create`. Replies `ServerResponse::Notice { text: "\u23f0 Reminder set for <humanized> \u2014 I'll DM you." }` to the invoker on the request's own `request_id`. **Nothing is posted, nothing is broadcast**; the nudge arrives later as a DM from the system identity via the widget sweeper.
   - Unknown `kind`: returns `Error { reason: "command misconfigured" }`.
   - Any fetch/extract/parse failure: returns `Error { reason }`. No message is posted.
5. **Message insert and broadcast** (DB lock scoped off the broadcast `.await`): for `"text"`/`"api"`, calls `messages::insert_message_with_author_name(conn, channel_id, &cmd.public_key, &content, None, Some(&cmd.name), Some("BOT"))` — `author_name_override = cmd.name`, `author_badge = "BOT"`. Broadcasts `ServerEvent::NewMessage { message }` to `EventTarget::Subscribers(channel_id)`. Returns `Ok`.

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
- **`messages.rs`** — message insert/edit/delete/fetch, pin/unpin; `set_widget(conn, message_id: u64, widget_json)` stamps a card message's `widget` column (insert-then-set-widget idiom used by the poll/giveaway creation transactions).
- **`polls.rs` / `giveaways.rs` / `widgets.rs`** — interactive widget storage, state transitions, and the shared 15 s sweeper. See `docs/modules/server-widgets.md`.
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
