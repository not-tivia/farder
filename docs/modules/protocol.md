# Wire Protocol

> **File(s):** `crates/farder-protocol/src/server.rs`, `crates/farder-protocol/src/messages.rs`, `crates/farder-protocol/src/codec.rs`
> **Layer:** Protocol
> **Last reviewed:** 2026-06-04

## Purpose

`farder-protocol` is the single source of truth for every message exchanged
between a Farder client and a Farder server. It defines the full type system for
requests, responses, and push events, plus the shared data structs they carry.
It does NOT implement networking, connection management, or any business logic —
those live in the server crate and `bridge.rs` respectively.

The crate has three sub-modules:

- **`server.rs`** — the main protocol types: `ClientFrame`, `ServerFrame`,
  `ServerRequest`, `ServerResponse`, `ServerEvent`, and all shared data structs.
- **`messages.rs`** — the relay and DM signaling protocol (`Message` enum) used
  between clients via the relay node, separate from the server request/response
  layer.
- **`codec.rs`** — a thin wrapper around `rmp_serde` (MessagePack); `encode` and
  `decode` are the only serialization entry points used by the rest of the
  codebase.

---

## Serialization format

All frames are serialized with **MessagePack** via `rmp_serde`. Callers use:

```rust
farder_protocol::codec::encode(&value)   // -> Result<Vec<u8>>
farder_protocol::codec::decode(&bytes)   // -> Result<T>
```

There is no versioning or magic header in the byte stream; both sides must agree
on the same compiled type definitions.

### `PublicKey` serialization quirk

`PublicKey` (from `farder_crypto::identity`) derives `Serialize`/`Deserialize`
with its single private field `bytes: [u8; 32]`. Under MessagePack (and under
`serde_json` for logging) it therefore serializes as a **map with a `bytes`
key** — i.e. `{"bytes": <32-byte array>}`, not as a bare byte array.

When emitting public keys to the webview, `bridge.rs` always calls
`public_key.to_string()` instead (which produces `"vk_<hex64>"`) so the
frontend's string comparisons work. Never pass a raw `PublicKey` value through
Tauri's JSON boundary — the `{bytes: [...]}` form would break every UI lookup.

---

## Frame envelopes

### `ClientFrame` (client → server)

The outer envelope for everything the client sends after the QUIC stream opens.

| Variant | Fields | When sent |
|---|---|---|
| `Authenticate` | `public_key: PublicKey`, `signed_challenge: Vec<u8>`, `invite_code: Option<String>`, `setup_token: Option<String>` | Sent once in response to a `Challenge` frame; proves identity by signing the server's nonce. `invite_code` is required on private servers; `setup_token` is used for first-run server setup. |
| `Request` | `id: u32`, `body: ServerRequest` | Every subsequent operation. The `id` is echoed back in the matching `ServerFrame::Response` so the client can correlate async responses. |

### `ServerFrame` (server → client)

The outer envelope for everything the server sends.

| Variant | Fields | When sent |
|---|---|---|
| `Challenge` | `nonce: [u8; 32]` | Immediately on connection; client must sign this nonce and reply with `ClientFrame::Authenticate`. |
| `Authenticated` | `session_token: Vec<u8>` | Sent after the server accepts the authentication; the session token may be used for resumption. |
| `AuthError` | `reason: String` | Sent if authentication fails (bad signature, not invited, etc.). |
| `Response` | `request_id: u32`, `body: ServerResponse` | Carries the `ServerResponse` matching the `ClientFrame::Request` with the same `id`. |
| `Event` | `(ServerEvent)` | Server-initiated push; not correlated to any request. Handled in `bridge.rs` by `dispatch_event`. |

---

## `ServerRequest` catalog

Wrapped in `ClientFrame::Request { id, body: ServerRequest::... }`. The server
replies with a matching `ServerFrame::Response { request_id: id, body: ServerResponse::... }`.

### Messaging

| Variant | Fields | What it asks the server to do |
|---|---|---|
| `Subscribe` | `channel_ids: Vec<u64>` | Start receiving `NewMessage` / `TypingStarted` events for the listed channels. |
| `SendMessage` | `channel_id: u64`, `content: String`, `reply_to: Option<u64>`, `attachment_ids: Vec<u64>` | Post a new message; attach pre-uploaded file IDs; optionally quote another message. |
| `EditMessage` | `message_id: u64`, `new_content: String` | Replace the content of an existing message the caller authored. |
| `DeleteMessage` | `message_id: u64` | Permanently delete a message (author or moderator). |
| `FetchHistory` | `channel_id: u64`, `before_id: Option<u64>`, `limit: u32` | Page backwards through channel history; omit `before_id` for the most recent messages. |
| `PinMessage` | `message_id: u64` | Mark a message as pinned (requires permission). |
| `UnpinMessage` | `message_id: u64` | Remove a message's pinned status. |
| `Search` | `query: String`, `channel_id: Option<u64>`, `limit: u32` | Full-text search; scope to a channel or search server-wide. |
| `Typing` | `channel_id: u64` | Notify other subscribers that the caller is typing (fire-and-forget; no meaningful response). |
| `AddReaction` | `message_id: u64`, `emoji: String`, `file_id: Option<u64>` | Add an emoji or custom-sticker reaction to a message. |
| `RemoveReaction` | `message_id: u64`, `emoji: String`, `file_id: Option<u64>` | Remove the caller's reaction from a message. |
| `CreateThread` | `message_id: u64`, `name: Option<String>` | Create a thread channel branching from an existing message. |

### Channels and categories

| Variant | Fields | What it asks the server to do |
|---|---|---|
| `CreateChannel` | `name: String`, `channel_type: ChannelType`, `category_id: Option<u64>`, `position: Option<u32>` | Create a new channel of the given type, optionally inside a category. |
| `UpdateChannel` | `channel_id: u64`, `name: Option<String>`, `topic: Option<String>`, `nsfw: Option<bool>`, `slow_mode_secs: Option<u32>`, `retention_secs: Option<Option<u64>>`, `category_id: Option<Option<u64>>`, `position: Option<u32>` | Patch any subset of a channel's settings; fields that are `None` are left unchanged. `retention_secs` and `category_id` use `Option<Option<T>>` so the caller can explicitly clear them. |
| `DeleteChannel` | `channel_id: u64` | Permanently delete a channel and all its messages. |
| `CreateCategory` | `name: String`, `position: Option<u32>` | Create a new category for grouping channels. |
| `UpdateCategory` | `category_id: u64`, `name: Option<String>`, `position: Option<u32>` | Rename or reposition a category. |
| `DeleteCategory` | `category_id: u64` | Delete a category (channels are not deleted with it). |
| `SetChannelOverride` | `channel_id: u64`, `role_id: u64`, `allow: u64`, `deny: u64` | Set a per-role permission override on a channel (bitmask). |
| `SetCategoryOverride` | `category_id: u64`, `role_id: u64`, `allow: u64`, `deny: u64` | Set a per-role permission override on a category. |

### Roles and moderation

| Variant | Fields | What it asks the server to do |
|---|---|---|
| `CreateRole` | `name: String`, `permissions: u64`, `color: Option<String>`, `position: Option<u32>` | Create a new role with a permission bitmask and optional display color. |
| `UpdateRole` | `role_id: u64`, `name: Option<String>`, `permissions: Option<u64>`, `color: Option<String>`, `position: Option<u32>` | Patch any subset of a role's properties. |
| `DeleteRole` | `role_id: u64` | Delete a role and strip it from all members. |
| `AssignRole` | `member_key: PublicKey`, `role_id: u64` | Grant a role to a member. |
| `RemoveRole` | `member_key: PublicKey`, `role_id: u64` | Revoke a role from a member. |
| `KickMember` | `member_key: PublicKey` | Remove a member from the server (they can rejoin with an invite). |
| `BanMember` | `member_key: PublicKey`, `reason: Option<String>` | Permanently ban a member; they cannot rejoin even with an invite. |
| `UnbanMember` | `member_key: PublicKey` | Lift a ban. |
| `TimeoutMember` | `member_key: PublicKey`, `until_ms: u64`, `reason: Option<String>` | Mute a member until the given Unix-ms timestamp. |
| `RemoveTimeout` | `member_key: PublicKey` | Clear an active timeout immediately. |
| `BlockUser` | `target_key: PublicKey` | Block a user (server-side; affects DM delivery). |
| `UnblockUser` | `target_key: PublicKey` | Remove a block. |

### Server info and membership

| Variant | Fields | What it asks the server to do |
|---|---|---|
| `GetServerInfo` | — | Fetch the server's name, member count, channel list, category list, and role list. |
| `GetMembers` | — | Fetch the full member roster with roles and timeout state. |
| `ListBanned` | — | Fetch the list of banned members. |
| `ListAuditEvents` | `before_id: Option<u64>`, `limit: u32` | Page through the audit log. |
| `CreateInvite` | `max_uses: Option<u32>`, `expires_in_secs: Option<u64>`, `target_channel: Option<u64>` | Generate an invite code with optional use cap and expiry. |

### Account / deletion

| Variant | Fields | What it asks the server to do |
|---|---|---|
| `RequestDeletion` | — | Begin account deletion; starts a grace-period countdown. |
| `CancelDeletion` | — | Cancel a pending deletion request during the grace period. |
| `GetDeletionStatus` | — | Check whether a deletion is pending and when it expires. |

### Direct messages

| Variant | Fields | What it asks the server to do |
|---|---|---|
| `OpenDm` | `target_key: PublicKey` | Open (or retrieve) the DM channel with another member. |
| `ListDms` | — | Fetch all of the caller's DM channels with their participants and last message. |

### File transfer

File upload and download use separate side-channel protocols (`UploadRequest` / `UploadResponse` and `DownloadRequest` / `DownloadResponse`) outside the main `ServerRequest`/`ServerResponse` flow. See the [Shared structs](#file-transfer-side-channel) section.

| Variant | Fields | What it asks the server to do |
|---|---|---|
| `FetchUrl` | `url: String`, `channel_id: u64` | Ask the server to proxy-fetch a URL and store it as a server-side file; returns the `file_id`. |

### Rich presence

| Variant | Fields | What it asks the server to do |
|---|---|---|
| `UpdatePresence` | `presence: Option<Presence>` | Set or clear the sender's ephemeral activity presence. `None` clears it. The server stamps the **sender's own authenticated public key** — the client cannot supply a key. Rate-limited to 2 updates/sec per member. Responds with `Ok` or `Error` (validation failure or rate-limit exceeded). See `docs/modules/presence.md`. |

### Voice / media

| Variant | Fields | What it asks the server to do |
|---|---|---|
| `JoinChannelMedia` | `channel_id: u64` | Join the voice roster for a channel (appears in presence, triggers `MediaJoined`). |
| `LeaveChannelMedia` | `channel_id: u64` | Leave the voice roster (triggers `MediaLeft`). |
| `GetMediaState` | `channel_id: u64` | Fetch the current list of voice participants for a channel. |
| `JoinStream` | `channel_id: u64` | Open a media stream session within an already-joined voice channel (triggers `StreamJoined`). |
| `LeaveStream` | — | End the caller's stream session (triggers `StreamLeft`). |
| `EnableTrack` | `kind: TrackKind` | Signal that a track (Audio or Video) is now active. |
| `DisableTrack` | `kind: TrackKind` | Signal that a track is now inactive. |
| `SetMute` | `muted: bool` | Update the caller's muted state; broadcast as `StreamStateChanged`. |
| `SetDeafen` | `deafened: bool` | Update the caller's deafened state; broadcast as `StreamStateChanged`. |
| `OfferStreamKey` | `kind: TrackKind`, `wrapped_keys: Vec<(PublicKey, Vec<u8>)>` | Distribute a per-session encrypted media key to each listed peer (E2EE key exchange for voice). |

---

## `ServerResponse` catalog

Carried in `ServerFrame::Response { request_id, body: ServerResponse::... }`.
Every request receives exactly one response.

| Variant | Fields | Meaning |
|---|---|---|
| `Ok` | — | The request succeeded and there is no data to return. |
| `Error` | `reason: String` | The request failed; `reason` is a human-readable explanation (e.g. "permission denied", "not found"). |
| `MessageSent` | `id: u64`, `timestamp: u64` | Echoes back the server-assigned message ID and its canonical timestamp after `SendMessage`. |
| `History` | `messages: Vec<MessageInfo>` | Ordered (newest-first) slice of history in response to `FetchHistory`. |
| `SearchResults` | `messages: Vec<MessageInfo>` | Results of a `Search` request, ordered by relevance. |
| `ServerInfo` | `name: String`, `member_count: u32`, `channels: Vec<ChannelInfo>`, `categories: Vec<CategoryInfo>`, `roles: Vec<RoleInfo>`, `owner_public_key: Option<PublicKey>` | Full server metadata in response to `GetServerInfo`. `owner_public_key` defaults to `None` via `#[serde(default)]` for schema evolution. |
| `Members` | `members: Vec<MemberInfo>` | Full member roster in response to `GetMembers`. |
| `BannedMembers` | `entries: Vec<BannedMember>` | Ban list in response to `ListBanned`. |
| `AuditEventsList` | `events: Vec<AuditEvent>` | Audit log page in response to `ListAuditEvents`. |
| `InviteCreated` | `code: String` | The generated invite code in response to `CreateInvite`. |
| `DeletionStatusResp` | `status: DeletionStatus` | Current deletion state in response to `GetDeletionStatus`. |
| `UrlFetched` | `file_id: u64` | The server-assigned file ID for the proxied URL in response to `FetchUrl`. |
| `DmOpened` | `channel: ChannelInfo`, `participant: MemberInfo` | The DM channel and participant info in response to `OpenDm`. |
| `DmList` | `dms: Vec<DmEntry>` | All DM entries in response to `ListDms`. |
| `StreamSessionStarted` | `session_id: [u8; 16]` | The 16-byte session identifier assigned to this stream in response to `JoinStream`. Used to correlate all subsequent `Stream*` events. |
| `MediaStateResp` | `participants: Vec<VoiceMember>` | Current voice roster in response to `GetMediaState`. |

---

## `ServerEvent` catalog

Carried in `ServerFrame::Event(ServerEvent::...)`. Server-pushed; not
correlated to a request. The `bridge.rs` `dispatch_event` function maps each
event to a `server:*` Tauri event (or a `VoiceController` call). See
`docs/modules/tauri-bridge.md` for the complete server event → Tauri event →
frontend listener mapping.

### Message events

| Variant | Fields | When the server broadcasts it |
|---|---|---|
| `NewMessage` | `message: MessageInfo` | A new message was posted in any channel the client has subscribed to via `Subscribe`. |
| `MessageEdited` | `message_id: u64`, `channel_id: u64`, `new_content: String`, `edited_at: u64` | A message was edited; `edited_at` is the Unix-ms timestamp of the edit. |
| `MessageDeleted` | `message_id: u64`, `channel_id: u64` | A message was deleted. |
| `MessagePinned` | `message_id: u64`, `channel_id: u64` | A message was pinned. (Note: `tauri-bridge.md` does not list a Tauri mapping for this event — it is currently unhandled in the bridge.) |
| `MessageUnpinned` | `message_id: u64`, `channel_id: u64` | A message's pin was removed. (Same note: no current bridge mapping.) |
| `ReactionAdded` | `message_id: u64`, `channel_id: u64`, `emoji: String`, `public_key: PublicKey`, `file_id: Option<u64>` | A member added a reaction. `file_id` is present for custom sticker reactions. |
| `ReactionRemoved` | `message_id: u64`, `channel_id: u64`, `emoji: String`, `public_key: PublicKey`, `file_id: Option<u64>` | A member removed their reaction. |
| `TypingStarted` | `channel_id: u64`, `public_key: PublicKey` | A member began typing; the UI should show a typing indicator for ~8 seconds. |

### Member and moderation events

| Variant | Fields | When the server broadcasts it |
|---|---|---|
| `MemberJoined` | `public_key: PublicKey`, `display_name: String` | A new member joined the server. |
| `MemberLeft` | `public_key: PublicKey` | A member left (voluntary or kicked). |
| `MemberBanned` | `public_key: PublicKey`, `reason: Option<String>` | A member was banned. |
| `MemberUnbanned` | `public_key: PublicKey` | A ban was lifted. |
| `MemberTimeoutChanged` | `public_key: PublicKey`, `until_ms: Option<u64>`, `reason: Option<String>` | A timeout was set or cleared; `until_ms` is `None` when the timeout is removed. |
| `YouWereKicked` | — | Sent only to the kicked client; the client should disconnect. |
| `YouWereBanned` | `reason: Option<String>` | Sent only to the banned client; the client should disconnect. |
| `AuditEventCreated` | `event: AuditEvent` | A new entry was appended to the audit log; broadcast to members with audit-log permission. |
| `MemberPresenceUpdated` | `public_key: PublicKey`, `presence: Option<Presence>` | A member's ephemeral presence changed. `None` means cleared (paused, stopped, toggled off, or disconnected). Always keyed to the member's authenticated public key. Maps to `server:member_presence_updated` in `tauri-bridge.md`. |

### Channel, category, and role lifecycle events

| Variant | Fields | When the server broadcasts it |
|---|---|---|
| `ChannelCreated` | `channel: ChannelInfo` | A new channel was created. |
| `ChannelUpdated` | `channel: ChannelInfo` | A channel's settings were changed. |
| `ChannelDeleted` | `channel_id: u64` | A channel was deleted. |
| `CategoryCreated` | `category: CategoryInfo` | A new category was created. |
| `CategoryUpdated` | `category: CategoryInfo` | A category was renamed or repositioned. |
| `CategoryDeleted` | `category_id: u64` | A category was deleted. |
| `RoleCreated` | `role: RoleInfo` | A new role was created. |
| `RoleUpdated` | `role: RoleInfo` | A role's properties were changed. |
| `RoleDeleted` | `role_id: u64` | A role was deleted. |
| `PermissionsChanged` | — | The caller's effective permissions changed (role reassignment, override edit, etc.); the client should re-fetch `GetServerInfo`. |

### Account lifecycle events

| Variant | Fields | When the server broadcasts it |
|---|---|---|
| `DeletionRequested` | `public_key: PublicKey` | A member started the account-deletion grace period. |
| `DeletionCancelled` | `public_key: PublicKey` | A pending deletion was cancelled. |
| `DeletionExecuted` | `public_key: PublicKey` | The grace period expired and the account was deleted; the member is now represented by the `DELETED_USER_KEY` sentinel. |

### Direct message events

| Variant | Fields | When the server broadcasts it |
|---|---|---|
| `DmCreated` | `channel: ChannelInfo`, `participant: MemberInfo` | A DM channel was opened with this client as one of the participants. Cross-referenced to `server:dm_created` in `tauri-bridge.md`. |

### Voice and media events

These events fall into two categories in `bridge.rs`: some are re-emitted to the webview as `server:voice_*` Tauri events (roster presence), while others are routed directly to the `VoiceController` for media-engine handling. See `docs/modules/tauri-bridge.md` for the exact routing.

| Variant | Fields | When the server broadcasts it |
|---|---|---|
| `MediaJoined` | `channel_id: u64`, `public_key: PublicKey`, `display_name: String` | A member joined the voice roster (presence layer, before streaming). Maps to `server:voice_joined`. |
| `MediaLeft` | `channel_id: u64`, `public_key: PublicKey` | A member left the voice roster. Maps to `server:voice_left`. |
| `StreamJoined` | `channel_id: u64`, `public_key: PublicKey`, `display_name: String`, `session_id: [u8; 16]`, `active_tracks: Vec<TrackKind>`, `muted: bool`, `deafened: bool` | A peer opened a media stream session. Routed to `VoiceController::on_peer_stream_joined`. |
| `StreamLeft` | `channel_id: u64`, `session_id: [u8; 16]` | A peer's stream session ended. Routed to `VoiceController::on_peer_stream_left`. |
| `TrackEnabled` | `channel_id: u64`, `session_id: [u8; 16]`, `kind: TrackKind` | A peer activated a track (Audio or Video). Routed to `VoiceController::on_peer_track_enabled`. |
| `TrackDisabled` | `channel_id: u64`, `session_id: [u8; 16]`, `kind: TrackKind` | A peer deactivated a track. Routed to `VoiceController::on_peer_track_disabled`. |
| `TrackActivityChanged` | `channel_id: u64`, `session_id: [u8; 16]`, `kind: TrackKind`, `active: bool` | Voice activity detection fired for a peer's track (speaking indicator). Routed to `VoiceController::on_peer_activity`. |
| `StreamStateChanged` | `channel_id: u64`, `session_id: [u8; 16]`, `muted: bool`, `deafened: bool` | A peer changed their mute/deafen state. Routed to `VoiceController::on_peer_stream_state`. |
| `StreamCallIncoming` | `channel_id: u64`, `caller: PublicKey`, `caller_name: String` | A DM voice call is incoming. Currently a no-op in `bridge.rs` (no UI wired yet). |
| `StreamCallEnded` | `channel_id: u64` | An incoming DM call ended or was declined. Currently a no-op in `bridge.rs`. |
| `StreamKeyOffer` | `channel_id: u64`, `sender: PublicKey`, `session_id: [u8; 16]`, `kind: TrackKind`, `wrapped_key: Vec<u8>` | A peer is distributing their per-session E2EE media key, wrapped for this client. Routed to `VoiceController::on_stream_key_offer`. |

---

## Shared structs

### `MessageInfo`

The canonical representation of a message, used in `NewMessage`, `History`,
`SearchResults`, and `MessageSent`.

| Field | Type | Notes |
|---|---|---|
| `id` | `u64` | Server-assigned message ID, monotonically increasing per channel. |
| `channel_id` | `u64` | The channel this message belongs to. |
| `author` | `PublicKey` | Serialized as `{bytes: [u8; 32]}` in MessagePack. |
| `content` | `String` | Plaintext message body. |
| `timestamp` | `u64` | Unix-ms creation time. |
| `edited_at` | `Option<u64>` | Unix-ms time of last edit, or `None`. |
| `reply_to` | `Option<u64>` | The `id` of the message being quoted, or `None`. |
| `pinned` | `bool` | Whether the message is currently pinned. |
| `attachments` | `Vec<AttachmentInfo>` | File attachments; see `AttachmentInfo`. |
| `reactions` | `Vec<ReactionGroup>` | Aggregated reactions; see `ReactionGroup`. |
| `thread_id` | `Option<u64>` | The channel ID of the thread spawned from this message, if any. |
| `thread_message_count` | `Option<u32>` | Cached reply count for the thread, if any. |

### `AttachmentInfo`

A file attached to a message. The file was uploaded separately via the
`UploadRequest` side-channel before the message was sent.

| Field | Type | Notes |
|---|---|---|
| `id` | `u64` | The attachment record ID (distinct from `file_id`). |
| `file_id` | `u64` | The opaque file storage ID, used in `DownloadRequest`. |
| `name` | `String` | Original filename. |
| `size` | `u64` | File size in bytes. |
| `mime_type` | `String` | MIME type string. |
| `width` | `Option<u32>` | For images/video: pixel width. |
| `height` | `Option<u32>` | For images/video: pixel height. |
| `duration_secs` | `Option<f64>` | For audio/video: duration. |

### `ReactionGroup`

An aggregated emoji reaction count on a message.

| Field | Type | Notes |
|---|---|---|
| `emoji` | `String` | Unicode emoji or custom sticker identifier. |
| `count` | `u32` | Number of users who reacted with this emoji. |
| `me` | `bool` | Whether the requesting client has added this reaction. |
| `file_id` | `Option<u64>` | Present for custom sticker reactions; `None` for standard emoji. Defaults to `None` via `#[serde(default)]`. |

### `ChannelInfo`

A channel record, used in create/update events and `ServerInfo`.

| Field | Type | Notes |
|---|---|---|
| `id` | `u64` | Server-assigned channel ID. |
| `name` | `String` | Display name. |
| `channel_type` | `ChannelType` | One of `Text`, `Announcement`, `Thread`, `Dm`, `Voice`. |
| `category_id` | `Option<u64>` | Parent category, or `None` for uncategorized. |
| `position` | `u32` | Sort order within the category/server. |
| `topic` | `Option<String>` | Optional channel topic/description. |
| `nsfw` | `bool` | Whether the channel is marked NSFW. |
| `slow_mode_secs` | `u32` | Minimum seconds between messages per user; 0 = disabled. |
| `retention_secs` | `Option<u64>` | Auto-delete messages older than this many seconds; `None` = keep forever. |
| `thread_parent_message_id` | `Option<u64>` | For `Thread` channels only: the message ID that spawned this thread. |

### `CategoryInfo`

| Field | Type | Notes |
|---|---|---|
| `id` | `u64` | Server-assigned category ID. |
| `name` | `String` | Display name. |
| `position` | `u32` | Sort order. |

### `RoleInfo`

| Field | Type | Notes |
|---|---|---|
| `id` | `u64` | Server-assigned role ID. |
| `name` | `String` | Display name. |
| `permissions` | `u64` | Bitmask of granted permissions. |
| `color` | `Option<String>` | Hex color string (e.g. `"#FF5500"`) or `None`. |
| `position` | `u32` | Role hierarchy position; higher = more privileged. |

### `MemberInfo`

A server member, used in `Members` response and `MemberJoined` event.

| Field | Type | Notes |
|---|---|---|
| `public_key` | `PublicKey` | Serialized as `{bytes: [u8; 32]}` in MessagePack. |
| `display_name` | `String` | Member's chosen display name. |
| `joined_at` | `u64` | Unix-ms join timestamp. |
| `role_ids` | `Vec<u64>` | IDs of all roles currently held. |
| `timeout_until` | `Option<u64>` | Unix-ms timeout expiry, or `None`. Defaults to `None` via `#[serde(default)]`. |
| `timeout_reason` | `Option<String>` | Human-readable reason for the timeout. Defaults to `None` via `#[serde(default)]`. |
| `presence` | `Option<Presence>` | The member's current ephemeral activity (music or game), or `None`. Defaults to `None` via `#[serde(default)]` — backward-compatible. Populated from `ServerState.presences` at the time `GetMembers` is handled so late joiners see the full picture. |

### `BannedMember`

A ban list entry, returned in `BannedMembers`.

| Field | Type | Notes |
|---|---|---|
| `public_key` | `PublicKey` | Serialized as `{bytes: [u8; 32]}` in MessagePack. |
| `display_name` | `String` | Display name at time of ban. |
| `ban_reason` | `Option<String>` | Moderator-provided reason. Defaults to `None` via `#[serde(default)]`. |
| `banned_at` | `u64` | Unix-ms timestamp of the ban. |

### `VoiceMember`

A voice presence entry, used in `MediaStateResp` and the `MediaJoined`/`MediaLeft` events.

| Field | Type | Notes |
|---|---|---|
| `public_key` | `PublicKey` | Serialized as `{bytes: [u8; 32]}` in MessagePack. |
| `display_name` | `String` | Member's display name. |
| `joined_at` | `u64` | Unix-ms timestamp when the member joined the voice channel. |

### `Presence` and `PresenceKind`

The ephemeral activity payload carried by `UpdatePresence`, `MemberPresenceUpdated`, and `MemberInfo.presence`. Defined in `crates/farder-protocol/src/server.rs`.

**`PresenceKind`** (enum): `Music | Game`.

**`Presence`** (struct):

| Field | Type | Notes |
|---|---|---|
| `kind` | `PresenceKind` | `Music` or `Game`. Determines the display format on the client. |
| `details` | `String` | Primary text: track title (Music) or game name (Game). Max 128 chars. |
| `state` | `Option<String>` | Secondary text: artist name (Music) or `None` (Game, for now). Max 128 chars. |

Field-length limits (128 chars each) are enforced server-side; the server returns `ServerResponse::Error` on violation. `Presence` derives `PartialEq` and `Clone` (required by the client's per-server dedup logic).

### `AuditEvent`

A single entry in the server audit log, used in `AuditEventsList` and `AuditEventCreated`.

| Field | Type | Notes |
|---|---|---|
| `id` | `u64` | Monotonically increasing log entry ID. |
| `actor` | `PublicKey` | Who performed the action. |
| `target` | `Option<PublicKey>` | Who was affected (e.g. the banned member), or `None` for server-level actions. Defaults to `None` via `#[serde(default)]`. |
| `action` | `String` | A string key identifying the action type (e.g. `"ban_member"`, `"delete_channel"`). |
| `metadata` | `serde_json::Value` | Free-form JSON with action-specific detail (e.g. ban reason, old/new channel name). Defaults to `null` via `#[serde(default)]`. |
| `timestamp_ms` | `u64` | Unix-ms timestamp of the event. |

### `DeletionStatus`

Returned by `GetDeletionStatus` / `DeletionStatusResp`.

| Field | Type | Notes |
|---|---|---|
| `pending` | `bool` | Whether a deletion is in the grace period. |
| `requested_at` | `Option<u64>` | Unix-ms when deletion was requested; `None` if not pending. |
| `expires_at` | `Option<u64>` | Unix-ms when the grace period expires; `None` if not pending. |

### `OverrideInfo`

A per-role permission override on a channel or category (used internally).

| Field | Type | Notes |
|---|---|---|
| `role_id` | `u64` | The role this override applies to. |
| `allow` | `u64` | Permission bitmask of explicitly granted permissions. |
| `deny` | `u64` | Permission bitmask of explicitly denied permissions. |

### `DmEntry`

One item in the `DmList` response.

| Field | Type | Notes |
|---|---|---|
| `channel` | `ChannelInfo` | The DM channel (type will be `Dm`). |
| `participant` | `MemberInfo` | The other party in the conversation. |
| `last_message` | `Option<MessageInfo>` | The most recent message, or `None` for empty DMs. |

---

## File-transfer side-channel

File upload and download bypass the main `ServerRequest`/`ServerResponse`
request-response cycle and use their own standalone types, sent over a separate
QUIC stream.

### `UploadRequest` / `UploadResponse`

The client sends an `UploadRequest` to declare the file, then streams the bytes.

| Type | Fields | Notes |
|---|---|---|
| `UploadRequest` | `channel_id: u64`, `file_name: String`, `file_size: u64`, `hash: String`, `mime_type: String`, `width: Option<u32>`, `height: Option<u32>`, `duration_secs: Option<f64>` | Declares the file before streaming bytes. |
| `UploadResponse::Ready` | — | Server is ready to receive bytes. |
| `UploadResponse::Complete` | `file_id: u64` | Upload finished; use `file_id` in `attachment_ids` when calling `SendMessage`. |
| `UploadResponse::Error` | `reason: String` | Upload was rejected (too large, invalid type, etc.). |

### `DownloadRequest` / `DownloadResponse`

| Type | Fields | Notes |
|---|---|---|
| `DownloadRequest` | `file_id: u64` | Requests a file by its opaque server ID. |
| `DownloadResponse::Start` | `file_name: String`, `file_size: u64`, `hash: String`, `mime_type: String` | Server confirms the file and begins streaming bytes. |
| `DownloadResponse::Error` | `reason: String` | File not found or access denied. |

---

## Relay / DM signaling protocol (`messages.rs`)

`Message` is a separate enum used between two clients via the relay node —
distinct from the client-to-server protocol above. It handles key exchange and
asynchronous DM delivery, invite previews, and rich external-link embeds.

### Core relay routing and DM signaling

| Variant | Fields | Purpose |
|---|---|---|
| `RelayConnect` | `destination_id: Vec<u8>` | Ask the relay to route subsequent frames to this destination. |
| `RelayConnected` | — | Relay confirms the route is established. |
| `RelayError` | `reason: String` | Relay reports a routing failure. |
| `KeyExchange` | `sender: PublicKey`, `session_public_key: [u8; 32]` | Initiate a Diffie-Hellman key exchange; `session_public_key` is the ephemeral X25519 public key. |
| `KeyExchangeResponse` | `responder: PublicKey`, `session_public_key: [u8; 32]` | Reply to a key exchange offer with the responder's ephemeral key. |
| `EncryptedDm` | `sender: PublicKey`, `ciphertext: Vec<u8>`, `timestamp: u64` | An end-to-end encrypted DM payload, sent after key exchange. |
| `NotifyRegister` | `public_key: PublicKey` | Register with the relay's async notification queue. |
| `NotifyPending` | `count: u32` | Relay notifies a registered client of `count` queued messages. |
| `NotifyFetch` | — | Ask the relay to deliver all queued messages. |
| `NotifyMessages` | `messages: Vec<QueuedMessage>` | Relay delivers queued messages. |
| `NotifyDeliver` | `recipient: PublicKey`, `payload: Vec<u8>` | Ask the relay to queue a payload for offline delivery to a recipient. |
| `DmFileHeader` | `sender: PublicKey`, `encrypted_header: Vec<u8>` | Begin a DM file transfer; the header carries encrypted file metadata. |
| `DmFileChunk` | `sender: PublicKey`, `encrypted_chunk: Vec<u8>` | A subsequent chunk of a DM file transfer. |
| `DmFileComplete` | `sender: PublicKey` | Signal that the DM file transfer is finished. |

`QueuedMessage` carries `sender: PublicKey`, `payload: Vec<u8>`, and
`timestamp: u64`, and is used only inside `NotifyMessages`.

### Relay fetch proxy — invite previews

| Variant | Fields | Purpose |
|---|---|---|
| `ProxyInvitePreview` | `target: PreviewTarget`, `code: String` | First message on a fresh throwaway connection; asks the relay to fetch an invite preview on the requester's behalf. |
| `ProxyInvitePreviewResult` | `outcome: PreviewOutcome` | Relay's answer to `ProxyInvitePreview`. |

`PreviewTarget` variants: `Registered { server_id: Vec<u8> }` (a server registered with this relay) or `Direct { addr: String }` (relay dials an arbitrary address, subject to SSRF guard).

`PreviewOutcome` variants: `Preview { server_name: String, member_count: u32, online_count: u32 }`, `Invalid`, `Unavailable`.

### Relay fetch proxy — rich external embeds (Phase 6)

New message variants for the relay's embed proxy. Each is exchanged on its own
fresh throwaway QUIC connection (never on a session connection).

| Variant | Fields | Purpose |
|---|---|---|
| `ProxyLinkEmbed` | `url: String` | Client → relay: first message on a throwaway connection; asks the relay to resolve rich embed metadata for `url`. `url` must be host-allowlisted by the relay (see `docs/modules/relay-embed.md`). |
| `ProxyLinkEmbedResult` | `outcome: EmbedOutcome` | Relay → client: relay's normalized answer. |
| `ProxyMedia` | `url: String` | Client → relay: first message on a separate throwaway connection; asks the relay to stream a media asset (image or direct video). |
| `ProxyMediaHeader` | `content_type: String`, `total_len: u64` | Relay → client: sent before the raw bytes, confirms the validated content type and total byte count. |
| `ProxyMediaUnavailable` | — | Relay → client: sent instead of `ProxyMediaHeader` when the media cannot be served (non-allowlisted, SSRF refusal, over 25 MB cap, bad content-type, timeout, rate-limit). |

**Wire format for `ProxyMedia` exchange:** after `ProxyMediaHeader` the relay
writes raw bytes as a 4-byte-BE `u32` length-prefix + raw bytes directly on the
stream (NOT framed as a `Message`). The client reads this with `recv.read_exact`
for 4 + `total_len` bytes and verifies the u32 matches `total_len`.

### New embed types

These types are defined in `messages.rs` and carried by the embed proxy messages.
They mirror the TypeScript types in `client/src/lib/linkEmbed.ts`.

**`EmbedKind`** (enum): `Tweet | Video | Image | Audio | Article`. Coarse
classification used by the client to pick a card layout.

**`EmbedMedia`** (struct): a directly-fetchable media asset.

| Field | Type | Notes |
|---|---|---|
| `url` | `String` | The media URL; fetched via `ProxyMedia`. |
| `mime` | `String` | MIME type (e.g. `"video/mp4"`, `"image/jpeg"`). |
| `width` | `Option<u32>` | Pixel width, if known. |
| `height` | `Option<u32>` | Pixel height, if known. |
| `playable_inline` | `bool` | `true` for direct files playable in `<video>`/`<img>`; `false` for embeddable sources (YouTube, Spotify) that must open externally. |

**`LinkEmbed`** (struct): normalized metadata for one external link, produced by
an adapter in `embed.rs`. The client never sees raw HTML; only this struct.

| Field | Type | Notes |
|---|---|---|
| `provider` | `String` | Short provider name: `"twitter"`, `"youtube"`, `"spotify"`, `"reddit"`, `"image"`. |
| `kind` | `EmbedKind` | Card layout hint. |
| `url` | `String` | Canonical URL (may differ from the input, e.g. after Twitter adapter canonicalization). |
| `title` | `Option<String>` | Page or post title. |
| `author` | `Option<String>` | Creator name (e.g. `"@jack"` for Twitter, `"r/aww"` for Reddit). |
| `description` | `Option<String>` | Post body or article excerpt. |
| `thumbnail` | `Option<String>` | Thumbnail image URL, fetched via `ProxyMedia`. |
| `media` | `Option<EmbedMedia>` | Inline-playable media, if available. `None` for YouTube/Spotify (external only). |
| `duration_secs` | `Option<u32>` | Media duration in seconds. |

**`EmbedOutcome`** (enum): `Embed(LinkEmbed) | Unsupported | Unavailable`.
Uniform failure variants — `Unsupported` means the host is allowlisted but the
URL shape isn't handled by any adapter; `Unavailable` means everything else
(non-allowlisted, rate-limit, SSRF, timeout, parse failure). Uniform failure
leaks no information about why the relay refused.

---

## Sentinel values

| Constant | Value | Meaning |
|---|---|---|
| `DELETED_USER_KEY` | `[0u8; 32]` | The public-key bytes used to represent a deleted user. Any `PublicKey` whose `bytes` are all zero is a tombstone, not a real identity. |

---

## Integration map

- **`bridge.rs`** — calls `codec::decode` on every incoming byte slice, pattern-matches `ServerFrame`, and either routes `Response` bodies back to `pending_requests` or calls `dispatch_event` for `Event` payloads.
- **`commands.rs`** — constructs `ServerRequest` values and reads `ServerResponse` variants; never touches `codec` directly.
- **`farder-server`** (server crate) — the authoritative sender of `ServerResponse` and `ServerEvent`; the authoritative receiver of `ServerRequest`.
- **`useServerEvents.ts`** — the frontend listener for the `server:*` Tauri events that `bridge.rs` emits in response to `ServerEvent` payloads.
- **`farder-crypto`** — provides `PublicKey` and `Keypair`; the protocol crate depends on it only for type definitions.

## Known gotchas

- **`PublicKey` wire form vs. display form:** on the wire (MessagePack / JSON) `PublicKey` is `{"bytes": [32 bytes]}`. When crossing the Tauri webview boundary it must be converted to its `Display` string (`"vk_<hex64>"`) via `.to_string()`. Mixing these two forms is a silent bug — the UI's string equality checks will always fail.
- **`#[serde(default)]` fields:** several optional fields on wire types (`ban_reason`, `timeout_until`, `timeout_reason`, `file_id` on `ReactionGroup`/`ReactionAdded`/`ReactionRemoved`, `target` and `metadata` on `AuditEvent`, `owner_public_key` on `ServerInfo`, `presence` on `MemberInfo`) use `#[serde(default)]` so they can be omitted from older serialized frames. Adding a new required field without `#[serde(default)]` will break deserialization of any frames produced by an older server.
- **Session ID is `[u8; 16]`:** `session_id` in all `Stream*` events is a 16-byte opaque array, not a UUID string. The `VoiceController` keys its peer map on this value; always compare by byte equality, not string form.
- **`Option<Option<T>>` in `UpdateChannel`:** `retention_secs` and `category_id` use `Option<Option<T>>` — the outer `None` means "do not change this field", the inner `None` means "clear it to null". This is intentional and necessary for partial-update semantics but is easy to conflate.
- **`PermissionsChanged` carries no payload:** the event only signals that the client's permissions may have changed. The client must issue a fresh `GetServerInfo` to know what changed.
- **`MessagePinned` / `MessageUnpinned` have no bridge mapping:** these two `ServerEvent` variants exist in the protocol but are not handled in `bridge.rs` (they fall through to the silent `_ => Ok(())` arm). If the UI needs pin change notifications, a bridge arm and a `useServerEvents` listener must both be added.
