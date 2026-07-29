# Wire Protocol

> **File(s):** `crates/farder-protocol/src/server.rs`, `crates/farder-protocol/src/messages.rs`, `crates/farder-protocol/src/codec.rs`
> **Layer:** Protocol
> **Last reviewed:** 2026-07-03

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
| `UpdatePresence` | `presence: Option<Presence>` | Set or clear the sender's ephemeral activity presence. `None` clears it. The server stamps the **sender's own authenticated public key** — the client cannot supply a key. Rate-limited to 2 updates/sec per member (excess silently returns `Ok`, no broadcast). Responds with `Ok` on success; `Error` only on validation failure (a field over 128 chars). See `docs/modules/presence.md`. |

### Incoming webhooks

| Variant | Fields | What it asks the server to do |
|---|---|---|
| `CreateWebhook` | `channel_id: u64`, `name: String` | Create an incoming webhook for a channel. **MANAGE_SERVER-gated.** `name` is trimmed and must be 1–64 chars. Generates a 64-hex (256-bit) random token and a per-webhook Ed25519 public key (the webhook's author identity, not a roster member). Returns `WebhookToken`. |
| `ListWebhooks` | `channel_id: u64` | List all webhooks for the channel. Tokens are never returned. Returns `Webhooks`. **MANAGE_SERVER-gated.** |
| `DeleteWebhook` | `id: i64` | Delete a webhook by id. Subsequent POST calls to the ingest URL immediately return 401. Returns `Ok`. **MANAGE_SERVER-gated.** |
| `RegenerateWebhookToken` | `id: i64` | Rotate the secret token for a webhook. The old token is immediately invalidated. Returns `WebhookToken` with the new token. **MANAGE_SERVER-gated.** |

### Bots

| Variant | Fields | What it asks the server to do |
|---|---|---|
| `AddBot` | `coin_id: String`, `label: String` | Register a new server-managed crypto-ticker bot. **Owner-gated.** The server trims and lowercases `coin_id`, then validates it against `[a-z0-9-]` (≤64 chars); `label` is trimmed and must be 1–64 chars — both are rejected with `Error` on violation. On success, generates a fresh Ed25519 keypair, inserts a `bots` row (stores the secret key and CoinGecko coin id), and inserts a `members` row with `is_bot=1`; `label` becomes the bot's display name. Returns `Ok`. The bot appears in the member roster and the price-poller starts broadcasting its presence within one poll cycle. |
| `AddCustomBot` | `name: String`, `source_url: String`, `value_path: String`, `unit: Option<String>` | Register a new custom-monitor bot that polls an arbitrary JSON API endpoint and broadcasts the extracted numeric value as its presence. **MANAGE_SERVER-gated.** `name` is trimmed and must be 1–64 chars; `source_url` must start with `http://` or `https://` and be ≤2048 chars; `value_path` is 1–256 chars. `unit` is optional — trimmed and capped at 24 chars server-side; `None`/empty means no unit label. On success, generates a fresh Ed25519 keypair, inserts a `bots` row (`kind='custom_api'`, `coin_id=''`, `source_url`/`value_path`/`unit` populated), and inserts a `members` row with `is_bot=1`; `name` becomes the bot's `display_name`. Returns `Ok`. The bot appears in the member roster; the price-poller fetches the API and broadcasts the extracted value as presence within one poll cycle. On fetch or extract failure the presence shows `"unavailable"`. |
| `RemoveBot` | `bot_public_key: PublicKey` | Remove a bot by its public key. **Owner-gated.** Deletes the `bots` and `members` rows, evicts the in-memory presence entry from `state.presences`, and broadcasts `MemberLeft`. Cascades: all `bot_alerts` and `bot_subscriptions` rows for this bot are deleted first. Returns `Ok`. |
| `SetBotPollInterval` | `secs: u64` | Set the bot price-poll interval. **MANAGE_SERVER-gated.** Values below 30 are clamped to 30 server-side. Stored in the `server_settings` KV table under key `"bot_poll_interval"`. The poll loop reads this value live each cycle, so the change takes effect without a server restart. Returns `Ok`. |
| `GetBotPollInterval` | — | Query the current bot price-poll interval. No permission gate (any member may call this). Returns `BotPollInterval { secs }` — the stored value floored at 30, or the default of 60 if unset. |
| `AddBotAlert` | `bot_public_key: PublicKey`, `metric: String`, `comparator: String`, `threshold: f64` | Add an alert for a bot. **MANAGE_SERVER-gated.** `metric` must be `"price_usd"`, `"change_24h"` (crypto ticker bots), or `"value"` (custom monitor bots); `comparator` must be `"above"` or `"below"` — both are rejected with `Error` on violation. Inserts a `bot_alerts` row with `armed=1`. Returns `Ok`. |
| `RemoveBotAlert` | `alert_id: i64` | Delete a price alert by its id. **MANAGE_SERVER-gated.** No-op if the id does not exist. Returns `Ok`. |
| `ListBotAlerts` | `bot_public_key: PublicKey` | List all price alerts for the given bot. **MANAGE_SERVER-gated.** Returns `BotAlerts { alerts: Vec<BotAlertInfo> }`. The `armed` field is internal and not exposed to clients. |
| `SubscribeBot` | `bot_public_key: PublicKey` | Subscribe the authenticated member to alert DMs for the given bot. No permission gate — any connected member may subscribe. Idempotent (INSERT OR IGNORE). Returns `Ok`. |
| `UnsubscribeBot` | `bot_public_key: PublicKey` | Unsubscribe the authenticated member from alert DMs for the given bot. No permission gate. No-op if not subscribed. Returns `Ok`. |
| `ListMySubscriptions` | — | List the public keys of all bots the authenticated member is subscribed to. No permission gate. Returns `MySubscriptions { bot_public_keys: Vec<PublicKey> }`. |

### Slash commands

| Variant | Fields | What it asks the server to do |
|---|---|---|
| `ListCommands {}` | — | List all slash commands registered on the server. **No permission gate** — any connected member may call this. Returns `Commands { commands: Vec<CommandInfo> }`. Only safe fields are returned (`id`, `trigger`, `description`, `takes_arg`); `url_template` and `body_text` are never exposed. |
| `AddCommand` | `name: String`, `trigger: String`, `description: String`, `kind: String`, `body_text: Option<String>`, `url_template: Option<String>`, `value_path: Option<String>`, `response_template: Option<String>`, `unit: Option<String>` | Register a new slash command. **MANAGE_SERVER-gated.** `name` trimmed, 1–48 chars; `trigger` trimmed/lowercased, 1–32 chars of `[a-z0-9_-]`, must be unique; `description` ≤160 chars. `kind` must be `"text"` (requires `body_text`), `"api"` (requires `url_template` starting with `http(s)://` and ≤2048 chars, and non-empty `value_path`), `"poll"`, or `"giveaway"` (the last two take **no** kind-specific fields). A fresh Ed25519 keypair is generated per command — that key is never a roster member; it is only used to author the command's response messages with `author_badge = "BOT"`. Returns `Ok`. |
| `DeleteCommand` | `id: i64` | Delete a slash command by id. **MANAGE_SERVER-gated.** No-op if the id does not exist. Returns `Ok`. |
| `RunCommand` | `trigger: String`, `channel_id: u64`, `args: String` | Invoke a slash command. **No create-gate** for `"text"`/`"api"`/`"poll"` — any connected member may call these; `"giveaway"` is **MANAGE_SERVER-gated at dispatch**. Subject to content-block check (`content_block_reason`) and a per-user rate limit of 5 runs / 10 s (`command_limiter`). Handled asynchronously at the connection level (not in `handlers.rs`) because `"api"` commands require an HTTP fetch. On success: `"text"`/`"api"` insert a message authored by the command's synthetic public key with `author_name_override = cmd.name` and `author_badge = "BOT"` and broadcast `ServerEvent::NewMessage`; `"poll"`/`"giveaway"` parse `args`, create the widget card + feature row in one transaction (`polls::create_poll_card` / `giveaways::create_giveaway_card` — see `server-widgets.md`), and broadcast `NewMessage` then `PollUpdated`/`GiveawayUpdated`. On any failure (unknown trigger, rate-limited, `"api"` fetch error, parse error, missing permission): returns `Error { reason }` and does NOT post a message to the channel. |

### Polls & giveaways (widget interactions)

A poll/giveaway is **created** by `RunCommand` on a command of kind `"poll"` / `"giveaway"` (poll: any member with SEND_MESSAGES; giveaway: **MANAGE_SERVER-gated at dispatch**). The variants below only interact with an existing widget by id. All are membership-gated (default-deny `request_requires_membership`); every variant re-checks channel visibility (VIEW_CHANNEL, or DM participant) and returns an opaque `Error { "poll not found" }` / `"giveaway not found"` on any visibility failure — widget ids are not an existence oracle. Mutating variants are timeout-gated; `VotePoll`/`RetractVote`/`EnterGiveaway`/`LeaveGiveaway` share the `widget_limiter` (10 interactions / 10 s per member).

| Variant | Fields | What it asks the server to do |
|---|---|---|
| `GetPoll` | `poll_id: i64` | Fetch full poll state. Returns `Poll { poll: PollInfo, my_vote: Option<u32> }` — `my_vote` is the caller's own vote only. |
| `VotePoll` | `poll_id: i64`, `option_index: u32` | Cast or move the caller's single vote. Fails on closed poll / bad index / rate limit. Broadcasts `PollUpdated` to channel subscribers. |
| `RetractVote` | `poll_id: i64` | Remove the caller's vote on an open poll. Broadcasts `PollUpdated`. |
| `ClosePoll` | `poll_id: i64` | Close early. **Creator-or-MANAGE_SERVER.** Idempotent. Broadcasts `PollUpdated`. |
| `GetGiveaway` | `giveaway_id: i64` | Fetch full giveaway state. Returns `Giveaway { giveaway: GiveawayInfo, my_entered: bool }` — `my_entered` is caller-only. |
| `EnterGiveaway` | `giveaway_id: i64` | Enter an open giveaway (idempotent). Broadcasts `GiveawayUpdated` with the new `entry_count`. |
| `LeaveGiveaway` | `giveaway_id: i64` | Leave an open giveaway (idempotent). Broadcasts `GiveawayUpdated`. |
| `CancelGiveaway` | `giveaway_id: i64` | Cancel an open giveaway — no draw, no announcement. **Creator-or-MANAGE_SERVER.** Broadcasts `GiveawayUpdated` (`status = "cancelled"`). |
| `RerollGiveaway` | `giveaway_id: i64` | Redraw the winner of an `"ended"` giveaway that has one, from the still-eligible entrants. **Creator-or-MANAGE_SERVER.** Broadcasts `GiveawayUpdated` then a winner-announcement `NewMessage`. |
| `ListActiveWidgets` | `channel_id: u64` | List a channel's OPEN widgets (read; not rate-limited; allowed while timed out). Visibility-checked on `channel_id` itself with an opaque `Error { "channel not found" }` for both a missing channel and one the caller cannot see. Returns `ActiveWidgets { polls, giveaways }` — each list oldest-first, 20 combined by `created_at`; no per-viewer fields. No broadcasts. |

### Personal reminders

A reminder is **created** by `RunCommand` on a command of kind `"reminder"` (`/<trigger> <duration> <text>`, 1 m–30 d, text 1–500 chars, ≤20 outstanding per member). Nothing is posted in the channel: the dispatch replies `Notice { text }` to the invoker only, and the due reminder arrives as a DM from the server system identity. Both variants below are membership-gated (default-deny `request_requires_membership`) and **owner-scoped by the authenticated connection key** — neither carries an owner field, so there is nothing to forge.

| Variant | Fields | What it asks the server to do |
|---|---|---|
| `ListMyReminders` | — | List the CALLER's own pending reminders, soonest first (`LIMIT 20`). Read: no timeout gate, not rate-limited. Returns `MyReminders { reminders }`. No broadcasts. |
| `CancelReminder` | `reminder_id: i64` | Cancel one of the caller's own pending reminders. Shares the `widget_limiter` (10/10 s). No timeout gate — managing your own private nudges is not channel content. A foreign id, an already-fired one and a nonexistent one all return the byte-identical `Error { "reminder not found" }` (no existence oracle). No broadcasts. |

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
| `BotPollInterval` | `secs: u64` | Current bot price-poll interval in response to `GetBotPollInterval`. Value is floored at 30 and defaults to 60 when unset. |
| `BotAlerts` | `alerts: Vec<BotAlertInfo>` | List of price alerts for a bot, in response to `ListBotAlerts`. Each entry is a `BotAlertInfo` (see below). |
| `MySubscriptions` | `bot_public_keys: Vec<PublicKey>` | The bot public keys the authenticated member is subscribed to, in response to `ListMySubscriptions`. |
| `WebhookToken` | `id: i64`, `token: String`, `server_id_hex: Option<String>` | The new/rotated webhook token in response to `CreateWebhook` or `RegenerateWebhookToken`. `server_id_hex` is the relay's routing id (used to build the ingest URL `POST /webhook/<server_id_hex>/<token>`); `None` for direct (non-relay) servers. **Shown once** — the token is write-only in the DB after this response. |
| `Webhooks` | `webhooks: Vec<WebhookInfo>` | The webhook list in response to `ListWebhooks`. No tokens. |
| `Commands` | `commands: Vec<CommandInfo>` | The slash-command list in response to `ListCommands`. Safe fields only — no `url_template` or `body_text`. |
| `Poll` | `poll: PollInfo`, `my_vote: Option<u32>` | Full poll state in response to `GetPoll`. `my_vote` is the requester's own vote index (self-only — voter identities are never sent to anyone else). |
| `Giveaway` | `giveaway: GiveawayInfo`, `my_entered: bool` | Full giveaway state in response to `GetGiveaway`. `my_entered` is self-only; entrant identities never leave the server. |
| `ActiveWidgets` | `polls: Vec<PollInfo>`, `giveaways: Vec<GiveawayInfo>` | The channel's OPEN widgets in response to `ListActiveWidgets`. Each list oldest-first (creation order); at most 20 combined, chosen by `created_at` ascending. Shared state only — `my_vote`/`my_entered` stay exclusive to `Poll`/`Giveaway`. |
| `Notice` | `text: String` | Invoker-only confirmation delivered on the request's own `request_id` — no broadcast, no message row. The `Ok`-but-say-something case (used by the `"reminder"` `RunCommand` kind, which deliberately posts nothing). |
| `MyReminders` | `reminders: Vec<ReminderInfo>` | The caller's own pending reminders in response to `ListMyReminders`, soonest first. Owner-scoped in SQL — another member's reminder can never appear here. |

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

### Widget events (polls & giveaways)

Broadcast to the widget's channel subscribers on every state change (vote, retract, close, enter, leave, cancel, draw, reroll — including sweeper-driven closes/draws). Both carry the full shared state struct, never per-member data.

| Variant | Fields | When the server broadcasts it |
|---|---|---|
| `PollUpdated` | `poll: PollInfo` | A poll's counts or closed state changed. Maps to `server:poll_updated`. |
| `GiveawayUpdated` | `giveaway: GiveawayInfo` | A giveaway's entry count, status, or winner changed. Maps to `server:giveaway_updated` (bridge maps `winner` to its `"vk_<hex>"` string form). A draw/reroll is followed by a winner-announcement `NewMessage`. |

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
| `author_name_override` | `Option<String>` | Display-name override for webhook-posted messages. `None` for all normal member messages. Decorated with `#[serde(default)]` so it deserializes as `None` from older frames. When present, clients must use this name instead of a roster lookup — the author public key is a per-webhook synthetic key, not a roster member. |
| `author_badge` | `Option<String>` | Badge label shown next to the author name for non-member posts. `"WEBHOOK"` for messages posted by an incoming webhook; `"BOT"` for messages posted by a slash command. `None` / absent on all normal member messages. Decorated with `#[serde(default)]` for schema evolution. The badge is data-driven: it is stored in `messages.author_badge` and returned verbatim; adding new values here (e.g. `"POLL"`) does not require a client code change. |
| `widget` | `Option<String>` | Server-written JSON marking this message as an interactive widget card: `{"type":"poll"\|"giveaway","id":<i64>}`. `None` on all normal messages. Decorated with `#[serde(default)]` — old peers deserialize frames without it, and old clients that ignore it still render the card's plain-text `content` fallback. Written only via `messages::set_widget`; clients parse it as untrusted input. |

### `WebhookInfo`

A webhook summary as returned by `ListWebhooks`. Tokens are never included.

| Field | Type | Notes |
|---|---|---|
| `id` | `i64` | Server-assigned webhook id. |
| `channel_id` | `u64` | The channel this webhook posts into. |
| `name` | `String` | Display name registered at creation time (1–64 chars). |

### `CommandInfo`

A slash-command summary as returned by `ListCommands`. **Safe-fields-only view** — `url_template` and `body_text` are never included, so API keys are never exposed to members. Defined in `farder-protocol/src/server.rs`.

| Field | Type | Notes |
|---|---|---|
| `id` | `i64` | Server-assigned command id. Use for `DeleteCommand`. |
| `trigger` | `String` | The trigger word (without `/`). Always stored and returned lowercase. |
| `description` | `String` | Short human-readable description (≤160 chars). |
| `takes_arg` | `bool` | `true` for `"api"`, `"poll"`, and `"giveaway"` commands; `false` for `"text"` commands. The client UI uses this to hint that trailing input is expected. |
| `kind` | `String` | The command kind (`"text"` \| `"api"` \| `"poll"` \| `"giveaway"`). `#[serde(default)]` — empty string when talking to an old server that omits it. Not sensitive (unlike `url_template`/`body_text`); the client uses it to open builder forms for structured kinds (poll/giveaway) instead of raw text entry. |

### `PollInfo`

Live poll state, broadcast whole on every change (`PollUpdated`) and returned by `GetPoll`. Shared state only — voter identities never leave the server (a member's own vote arrives separately as `Poll.my_vote`).

| Field | Type | Notes |
|---|---|---|
| `id` | `i64` | Poll id (also in the card message's `widget` JSON). |
| `channel_id` | `u64` | Channel of the card message. |
| `message_id` | `u64` | The card message's id. |
| `creator` | `PublicKey` | The member who ran the command (`{bytes}` shape, like `MessageInfo.author`). |
| `question` | `String` | 1–256 chars. |
| `options` | `Vec<String>` | 2–10 options, 1–100 chars each, no duplicates. |
| `counts` | `Vec<u32>` | Vote counts aligned index-for-index with `options`. |
| `total_votes` | `u32` | Sum of `counts`. |
| `created_at` | `u64` | Unix **seconds** (`db::now()`, same unit as `messages.timestamp`). |
| `closes_at` | `Option<u64>` | Unix-secs deadline, or `None` for an untimed poll (closes only manually / on card delete). |
| `closed` | `bool` | Terminal flag; set by `ClosePoll`, the sweeper, or card deletion. |

### `GiveawayInfo`

Live giveaway state, broadcast whole on every change (`GiveawayUpdated`) and returned by `GetGiveaway`. Shared state only — `entry_count`, never an entrant list; a member's own entry arrives separately as `Giveaway.my_entered`.

| Field | Type | Notes |
|---|---|---|
| `id` | `i64` | Giveaway id (also in the card message's `widget` JSON). |
| `channel_id` | `u64` | Channel of the card message. |
| `message_id` | `u64` | The card message's id. |
| `creator` | `PublicKey` | The moderator who ran the command. |
| `prize` | `String` | 1–200 chars. |
| `ends_at` | `u64` | Unix-secs draw deadline (giveaways are always timed, 1m–30d). |
| `status` | `String` | `"open"` \| `"ended"` \| `"cancelled"`. |
| `entry_count` | `u32` | Live entry count; identities stay server-side. |
| `winner` | `Option<PublicKey>` | Set on a drawn `"ended"` giveaway; `None` if winnerless/cancelled/open. The Tauri bridge converts it to a `"vk_<hex>"` string for the frontend. |
| `winner_name` | `Option<String>` | Server-resolved display name, set when ended with a winner still on the roster (`None` → clients fall back to the short key form). |

### `ReminderInfo`

One of the caller's **own** pending reminders, returned only inside `MyReminders`. Never broadcast, never sent to anyone but the owner.

| Field | Type | Notes |
|---|---|---|
| `id` | `i64` | Reminder id — used by `CancelReminder`. Not an oracle: a foreign id is indistinguishable from a nonexistent one. |
| `text` | `String` | 1–500 chars, preserved verbatim (the grammar has no delimiter past the first space). |
| `due_at` | `u64` | Absolute unix **seconds**; no timezone is stored anywhere (clients render locally). |
| `created_at` | `u64` | Unix secs. |
| `channel_id` | `u64` | Where it was set — link-back context only; the reminder is not channel content. |

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
| `presence` | `Option<Presence>` | The member's current ephemeral activity (music, game, or ticker price), or `None`. Defaults to `None` via `#[serde(default)]` — backward-compatible. Populated from `ServerState.presences` at the time `GetMembers` is handled so late joiners see the full picture. For bots with `kind=Ticker`, the price poller keeps this field live. |
| `is_bot` | `bool` | `true` for server-managed crypto-ticker bots (rows with `is_bot=1` in the `members` table). Defaults to `false` via `#[serde(default)]`. The client uses this flag to render the BOT badge and to suppress human-only context-menu actions on bot members. |

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

**`PresenceKind`** (enum): `Music | Game | Ticker`.

**`Presence`** (struct):

| Field | Type | Notes |
|---|---|---|
| `kind` | `PresenceKind` | `Music`, `Game`, or `Ticker`. Determines the display format on the client. `Ticker` is used by both bot kinds: crypto-ticker bots write a formatted price string into `details` with `state="24h"`; custom-monitor bots write the extracted value (with optional unit) into `details` with `state=None`. |
| `details` | `String` | Primary text: track title (Music) / game name (Game) / for `crypto_ticker` bots: `"$<price> <arrow><pct>%"` (e.g. `"$67432.00 ▲2.10%"`); for `custom_api` bots: `"<value> <unit>"` (e.g. `"102,433 players"`) or just `"<value>"` when no unit. `"unavailable"` when a custom bot's fetch or extract fails. Max 128 chars for user-supplied presence; bot presence is not rate-limited by this path. |
| `state` | `Option<String>` | Secondary text: artist name (Music) / `None` (Game) / `"24h"` (crypto-ticker bots only — the change-window label) / `None` (custom-monitor bots). Max 128 chars. |

Field-length limits (128 chars each) are enforced server-side for user-supplied presence (`UpdatePresence`); bot presence written by the server's poller is not rate-limited by this path. `Presence` derives `PartialEq` and `Clone` (required by the client's per-server dedup logic).

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

### `BotAlertInfo`

A price alert as returned to clients by `ListBotAlerts` / `BotAlerts`. The internal `armed` flag is not exposed.

| Field | Type | Notes |
|---|---|---|
| `id` | `i64` | Server-assigned alert id (primary key in `bot_alerts`). Pass this back to `RemoveBotAlert`. |
| `metric` | `String` | `"price_usd"`, `"change_24h"`, or `"value"` (custom monitor bots). |
| `comparator` | `String` | `"above"` or `"below"`. |
| `threshold` | `f64` | The threshold value the metric is compared against. |

**TypeScript equivalent** (in `client/src/lib/types.ts`):
```ts
interface BotAlertInfo { id: number; metric: string; comparator: string; threshold: number; }
```

### `bots` table

The `bots` table stores server-managed bot keypairs and configuration. It is in the server's SQLite database.

| Column | Type | Notes |
|---|---|---|
| `public_key` | `BLOB PRIMARY KEY` | 32-byte Ed25519 public key generated by the server. |
| `secret_key` | `BLOB NOT NULL` | 32-byte Ed25519 secret key held by the server (never sent to clients). |
| `kind` | `TEXT NOT NULL` | `'crypto_ticker'` for CoinGecko-price bots; `'custom_api'` for custom-monitor bots. |
| `coin_id` | `TEXT NOT NULL` | CoinGecko coin id for `crypto_ticker` bots (e.g. `"bitcoin"`). Empty string `""` for `custom_api` bots. |
| `label` | `TEXT NOT NULL` | Display name — matches the `members.display_name` set at registration time. |
| `source_url` | `TEXT` | For `custom_api` only: the JSON API endpoint URL. `NULL` for `crypto_ticker` bots. |
| `value_path` | `TEXT` | For `custom_api` only: dot-separated key path into the JSON response (e.g. `"data.players"`). `NULL` for `crypto_ticker` bots. |
| `unit` | `TEXT` | For `custom_api` only: optional unit label appended to the displayed value (e.g. `"players"`). `NULL` for `crypto_ticker` bots or when no unit was supplied. |
| `created_at` | `INTEGER NOT NULL` | Unix-ms insert timestamp. |

The `source_url`, `value_path`, and `unit` columns are added as nullable `ALTER TABLE` migrations so existing `bots` tables from before `custom_api` support are upgraded transparently.

### Alert DB tables (`bot_alerts`, `bot_subscriptions`)

These tables live in the server's SQLite database and back the price-alert feature.

**`bot_alerts`** — one row per configured alert:

| Column | Type | Notes |
|---|---|---|
| `id` | `INTEGER PRIMARY KEY` | Auto-increment. Returned in `BotAlertInfo.id`. |
| `bot_public_key` | `BLOB` | 32-byte Ed25519 public key of the bot that owns this alert. FK → `bots.public_key`. |
| `metric` | `TEXT` | `"price_usd"`, `"change_24h"`, or `"value"` (custom monitor bots). |
| `comparator` | `TEXT` | `"above"` or `"below"`. |
| `threshold` | `REAL` | Threshold value. |
| `armed` | `INTEGER` | `1` = alert may fire; `0` = disarmed (fired; waiting for condition to clear before re-arming). |
| `created_at` | `INTEGER` | Unix-ms insert timestamp. |

**`bot_subscriptions`** — one row per member/bot subscription pair:

| Column | Type | Notes |
|---|---|---|
| `bot_public_key` | `BLOB` | 32-byte public key of the bot. |
| `subscriber_public_key` | `BLOB` | 32-byte public key of the subscribing member. |
| `created_at` | `INTEGER` | Unix-ms insert timestamp. |

Unique constraint: `(bot_public_key, subscriber_public_key)` — subscriptions are idempotent (`INSERT OR IGNORE`).

**Cascade on bot removal:** `bots::remove_bot` deletes all `bot_alerts` and `bot_subscriptions` rows for the bot before deleting the `bots` row, so no orphan rows are left.

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

## `RelayStreamRole` (relay-mode stream classifier)

`RelayStreamRole` is the **first frame on every QUIC bi-stream opened by the relay to the server** (relay-mode only; direct connections never use it). It identifies what kind of stream this is. The relay prefixes each stream with a 4-byte big-endian routing handle before this frame.

| Variant | Fields | Meaning |
|---|---|---|
| `Primary` | — | A new client session; the server runs the auth handshake. |
| `Session` | `token: Vec<u8>` | A file-transfer stream for an already-authenticated session (identified by the 32-byte session token). |
| `Webhook` | `token: String`, `body: Vec<u8>` | An incoming webhook delivery: the relay passes the raw HTTP body and the webhook secret token. The server validates, parses (Discord-compatible `{content, username?}` JSON), and posts the message. After processing the server writes a 2-byte big-endian HTTP-ish status code (204 ok, 400 bad request, 401 unauthorized, 413 too large) and closes the stream. The relay forwards this code as the HTTP response status to the external caller. |

The relay always uses handle `0` for relay-originated streams (invite previews and webhook deliveries); client streams get handles ≥ 1.

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

## DB schema additions (webhooks)

Two additions to the server's SQLite schema relate to this protocol:

- **`webhooks` table** — `CREATE TABLE IF NOT EXISTS webhooks (id INTEGER PRIMARY KEY AUTOINCREMENT, channel_id INTEGER NOT NULL, token TEXT NOT NULL UNIQUE, name TEXT NOT NULL, public_key BLOB NOT NULL, created_at INTEGER NOT NULL)`. `token` is the 64-hex secret (write-only after creation/rotation). `public_key` is the webhook author's synthetic Ed25519 key (never a roster member). There is no `token` field in `WebhookInfo` — tokens are intentionally excluded from list/read responses.

- **`messages.author_name_override` column** — `ALTER TABLE messages ADD COLUMN author_name_override TEXT` (applied as a migration; defaults to `NULL`). Stores the per-delivery `username` or the webhook's registered `name` for webhook-posted messages. All normal member messages have `NULL` here.

## DB schema additions (slash commands)

Two additions to the server's SQLite schema relate to slash commands:

- **`commands` table** — `CREATE TABLE IF NOT EXISTS commands (id INTEGER PRIMARY KEY AUTOINCREMENT, trigger TEXT NOT NULL UNIQUE, name TEXT NOT NULL, description TEXT NOT NULL, kind TEXT NOT NULL, body_text TEXT, url_template TEXT, value_path TEXT, response_template TEXT, unit TEXT, public_key BLOB NOT NULL, created_at INTEGER NOT NULL)`. `trigger` is unique and stored lowercase. `public_key` is a fresh Ed25519 key generated at creation time (the command's author identity — **never a roster member**). `url_template` and `body_text` are server-only secrets; they are intentionally excluded from the `CommandInfo` wire type. `kind` is the extension point: `"text"`, `"api"`, `"poll"`, and `"giveaway"` — the interactive kinds landed exactly this way, as new dispatch arms in `connection.rs` with no schema change (their per-widget state lives in the `polls`/`giveaways` tables, see `server-widgets.md`).

- **`messages.author_badge` column** — `ALTER TABLE messages ADD COLUMN author_badge TEXT` (applied as a migration; defaults to `NULL`). Data-driven badge label stored verbatim and returned in `MessageInfo`. Current values: `"WEBHOOK"` (set by `webhooks::deliver`) and `"BOT"` (set by `RunCommand` dispatch in `connection.rs`). Extending the badge vocabulary requires no schema change — add a new string value in the relevant dispatch site.

---

## Known gotchas

- **`PublicKey` wire form vs. display form:** on the wire (MessagePack / JSON) `PublicKey` is `{"bytes": [32 bytes]}`. When crossing the Tauri webview boundary it must be converted to its `Display` string (`"vk_<hex64>"`) via `.to_string()`. Mixing these two forms is a silent bug — the UI's string equality checks will always fail.
- **`#[serde(default)]` fields:** several optional fields on wire types (`ban_reason`, `timeout_until`, `timeout_reason`, `file_id` on `ReactionGroup`/`ReactionAdded`/`ReactionRemoved`, `target` and `metadata` on `AuditEvent`, `owner_public_key` on `ServerInfo`, `presence` and `is_bot` on `MemberInfo`) use `#[serde(default)]` so they can be omitted from older serialized frames. Adding a new required field without `#[serde(default)]` will break deserialization of any frames produced by an older server.
- **Session ID is `[u8; 16]`:** `session_id` in all `Stream*` events is a 16-byte opaque array, not a UUID string. The `VoiceController` keys its peer map on this value; always compare by byte equality, not string form.
- **`Option<Option<T>>` in `UpdateChannel`:** `retention_secs` and `category_id` use `Option<Option<T>>` — the outer `None` means "do not change this field", the inner `None` means "clear it to null". This is intentional and necessary for partial-update semantics but is easy to conflate.
- **`PermissionsChanged` carries no payload:** the event only signals that the client's permissions may have changed. The client must issue a fresh `GetServerInfo` to know what changed.
- **`MessagePinned` / `MessageUnpinned` have no bridge mapping:** these two `ServerEvent` variants exist in the protocol but are not handled in `bridge.rs` (they fall through to the silent `_ => Ok(())` arm). If the UI needs pin change notifications, a bridge arm and a `useServerEvents` listener must both be added.
