# Frontend bridge (TS invoke wrappers + shared types)

> **File(s):** `client/src/lib/tauri-bridge.ts`, `client/src/lib/types.ts`
> **Layer:** Tauri bridge
> **Last reviewed:** 2026-06-04

## Purpose

These two files are the TypeScript side of the frontend ↔ Tauri seam. `tauri-bridge.ts` exports one typed async wrapper per Rust command, replacing raw `invoke("string")` calls with named functions that carry parameter and return types. `types.ts` owns all shared TS interfaces (the shapes Rust hands back as JSON) plus two helpers for working with public keys. Together they define the complete contract the UI code can rely on; nothing about actual IPC mechanics or Rust-side logic lives here.

---

## Serialization gotchas

Before reading the individual functions, internalize these two rules — they affect almost every interface:

1. **Public keys arrive as `{ bytes: number[] }`, not strings.** Rust's serde serializes `PublicKey` as a JSON object `{ "bytes": [0, 1, …, 31] }` (a 32-element byte array). The UI must call `publicKeyToString()` to turn this into the `"vk_<hex>"` string it uses as React keys and for string comparisons. Passing the raw `{ bytes }` object to equality checks or as a command argument will silently fail.

2. **`publicKeyToString()` is the single normalizer.** Every place in the UI that stores or compares a public key should go through this function. The Tauri bridge (Rust `bridge.rs`) emits keys in `"vk_<hex>"` form too, so event payloads and snapshot results are consistent as long as you normalize on both sides.

---

## Key interfaces (types.ts)

### `ChannelInfo`

Describes a channel as returned by the server. `channel_type` is one of `"Text" | "Announcement" | "Thread" | "Dm" | "Voice"`. `thread_parent_message_id` is non-null only for `Thread` channels — it identifies the message the thread was spawned from.

### `CategoryInfo`

A named group that channels are nested under. Only `id`, `name`, and `position`.

### `RoleInfo`

A server role. `permissions` is a bitmask (the Rust `Permissions` bitfield serialized as a plain integer). `color` is a CSS hex string or null.

### `MemberInfo`

A server member. `public_key` is a `{ bytes: number[] }` object — call `publicKeyToString()` before using it as a key or passing it to a command. `timeout_until` is a Unix-ms timestamp or null; non-null means the member is currently timed out.

### `MessageInfo`

A single message. `author` is also a `{ bytes: number[] }` public key. `reactions` is an array of `ReactionGroup` entries, each grouped by `emoji` plus an optional `file_id` for custom-emoji reactions. `thread_id` points to the `Thread` channel created for this message, if any.

### `BannedMember`

Returned by `listBanned()`. `public_key` is again `{ bytes: number[] }`. `ban_reason` is optional (the member may have been banned without a reason). `banned_at` is Unix ms.

### `ConnectResult`

Returned by `connectServer()` and `getServerInfo()`. Contains the initial snapshot: `channels`, `categories`, `roles`, and `owner_public_key` (also `{ bytes }`, may be null for servers whose owner is unknown to the client).

### `DmEntry`

An entry in the DM list. Bundles a `ChannelInfo` (the DM channel), a `MemberInfo` (the other participant), and optionally the most recent `MessageInfo`.

### `VoiceMember` (tauri-bridge.ts)

The roster entry returned by `getVoiceState()`. `public_key` is `{ bytes: number[] }` — this is the older roster command; the newer voice pipeline uses `VoicePeer` instead.

### `VoicePeer` (tauri-bridge.ts)

A peer inside an active voice call, returned as part of `VoiceState`. `pubkey` is also `{ bytes: number[] }`. `speaking`, `muted`, `deafened` are live state from the voice controller.

### `VoiceState` (tauri-bridge.ts)

Top-level snapshot of the local voice pipeline. `channel_id` is a raw `number[] | null` (16 bytes, the controller's `ChannelId`), not a server channel integer. It is null when no call is active.

### `AuditEvent` (tauri-bridge.ts)

`actor` and `target` are both `{ bytes: number[] }` public keys. `target` may be null (e.g. a server-settings change with no individual target). `metadata` is a freeform JSON object whose shape depends on `action`.

### `NotificationPrefs` (tauri-bridge.ts)

User notification settings persisted locally (not on-server). `dmAllowedUsers` and `mutedUsers` hold `"vk_<hex>"` strings — they are already normalized.

### `InviteResult` (tauri-bridge.ts)

Returned by `createInvite()`. `code` is the raw invite token; `link` and `deep_link` are ready-to-share URL strings.

### `DownloadResult` (tauri-bridge.ts)

Returned by `downloadFile()`. `data_url` is a base64 data URL for in-app preview (may be null if the file is too large or binary-only). `saved_path` is non-null if the file was also written to disk.

### `FavoriteEntry` (tauri-bridge.ts)

A locally-saved file favorite. `favorited_at` is Unix ms. `data_url` is always present (favorites are stored with inline data). `source_server` is the server ID the file came from.

### `ThemeMeta` / `ActiveTheme` (tauri-bridge.ts)

Theme catalog entry vs the currently-applied theme. `source: "builtin" | "user"` distinguishes shipped themes from user-created ones. `ActiveTheme.css` is the full stylesheet text.

### `ManagedServer` / `TemplateInfo` (tauri-bridge.ts)

Metadata for a locally-hosted server (port, data dir, template, privacy mode) and for a server template available at creation time.

### `EmbedKind` / `EmbedMedia` / `LinkEmbed` / `EmbedOutcome` (`lib/linkEmbed.ts`)

Mirror types for `farder_protocol::messages` embed types. `EmbedKind` is a string
union: `"Tweet" | "Video" | "Image" | "Audio" | "Article"`. `EmbedMedia` carries
`url`, `mime`, `width`/`height` (nullable), and `playable_inline: boolean`.
`LinkEmbed` carries `provider`, `kind`, `url`, and nullable `title`, `author`,
`description`, `thumbnail`, `media` (an `EmbedMedia`), and `duration_secs`.

`EmbedOutcome` mirrors serde's external tagging:
- `{ Embed: LinkEmbed }` — a successful embed.
- `"Unsupported"` — allowlisted host, but the URL shape isn't handled by an adapter.
- `"Unavailable"` — non-allowlisted, rate-limited, timed out, or no relay configured.

---

## Helper functions (types.ts)

### `publicKeyToString(pk: { bytes: number[] }): string`

**What it does:** converts a serde-deserialized public key object into the canonical `"vk_<hex>"` string used everywhere in the UI as an identifier.
**Returns:** `"vk_" + hex` where each byte is zero-padded to two hex digits.
**Use whenever:** comparing two public keys, using one as a React key, logging, or passing one as a string argument to a Tauri command.

### `isDeletedUser(pk: { bytes: number[] }): boolean`

**What it does:** returns true if the public key is all-zero bytes (the sentinel value `DELETED_USER_KEY`). The server uses this placeholder for messages whose author has deleted their account.
**Returns:** `true` / `false`.
**Use whenever:** rendering an author field — show a "Deleted user" label instead of looking up the key.

---

## invoke() wrappers by area

All functions are `async` and throw on Tauri/Rust error. Every `serverId` is the opaque string ID assigned at connect time.

---

### Server management

| Function | Rust command | What it does |
|---|---|---|
| `connectServer(address, inviteCode?, setupToken?)` | `connect_server` | Establishes a QUIC connection to a server. Returns `ConnectResult` with the initial channel/category/role snapshot. `inviteCode` is required for private servers; `setupToken` is used for first-time owner setup. |
| `disconnectServer(serverId)` | `disconnect_server` | Drops the QUIC connection and cleans up local state. |
| `listServers()` | `list_servers` | Returns all currently-connected servers as `{ id, name }[]`. |
| `getSavedServers()` | `get_saved_servers` | Returns servers saved to disk (auto-reconnect list). |
| `restartLocalServers()` | `restart_local_servers` | Restarts all locally-managed Farder server processes and returns the updated list. |
| `getServerInfo(serverId)` | `get_server_info` | Re-fetches the server snapshot (same shape as `ConnectResult`) without re-connecting. |

---

### Identity and profile

| Function | Rust command | What it does |
|---|---|---|
| `generateKeypair()` | `generate_keypair` | Generates a new Ed25519 keypair, persists it, and returns the public key as a `"vk_<hex>"` string. |
| `loadIdentity()` | `load_identity` | Loads the existing keypair from disk. Returns the public key string or null if none is saved. |
| `getPublicKey()` | `get_public_key` | Returns the current public key string, or null. |
| `setDisplayName(name)` | `set_display_name` | Persists the display name locally; the server learns it on next connect or profile update. |
| `getDisplayName()` | `get_display_name` | Returns the stored display name or null. |
| `setBio(bio)` | `set_bio` | Persists the profile bio. |
| `getBio()` | `get_bio` | Returns the stored bio or null. |
| `setProfileColor(color)` | `set_profile_color` | Persists a CSS hex color used as the user's accent color in the UI. |
| `getProfileColor()` | `get_profile_color` | Returns the stored color or null. |
| `getLastServer()` | `get_last_server` | Returns the ID of the last-connected server (for auto-select on startup). |
| `pickFile()` | `pick_file` | Opens the OS file-picker dialog. Returns the selected file path or null if the user cancelled. |
| `setAvatar(filePath)` | `set_avatar` | Encodes the file at `filePath` and stores it as the local user avatar. Returns a data URL. |
| `getAvatar()` | `get_avatar` | Returns the stored avatar data URL, or null. |
| `setServerAvatar(serverId, filePath)` | `set_server_avatar` | Same as `setAvatar` but scoped to a server (server-icon override). Returns a data URL. |
| `getServerAvatar(serverId)` | `get_server_avatar` | Returns the server icon data URL, or null. |

---

### Channels and categories

| Function | Rust command | What it does |
|---|---|---|
| `createChannel(serverId, name, channelType, categoryId?)` | `create_channel` | Creates a channel. `channelType` is a string matching the Rust enum variant, e.g. `"Text"` or `"Voice"`. |
| `updateChannel(serverId, channelId, opts)` | `update_channel` | Updates any combination of name, topic, nsfw, slow-mode, category assignment, and position. The `setCategory` flag is set internally when `opts.categoryId` is provided, to distinguish "move to category" from "leave category unchanged". |
| `deleteChannel(serverId, channelId)` | `delete_channel` | Deletes the channel and all its messages. |
| `createCategory(serverId, name)` | `create_category` | Creates a new category group. |
| `updateCategory(serverId, categoryId, opts)` | `update_category` | Updates category name and/or position. |
| `deleteCategory(serverId, categoryId)` | `delete_category` | Deletes the category (channels inside are moved to uncategorized). |
| `subscribeChannels(serverId, channelIds)` | `subscribe_channels` | Tells the server which channel IDs the client wants `NewMessage` events for. Must be called after connect and after navigating to a channel. |
| `setChannelOverride(serverId, channelId, roleId, allow, deny)` | `set_channel_override` | Sets a role-based permission override on a channel. `allow` and `deny` are bitmasks. |

---

### Messages

| Function | Rust command | What it does |
|---|---|---|
| `sendMessage(serverId, channelId, content, replyTo?, attachmentIds?)` | `send_message` | Posts a message. `replyTo` is a message ID to thread-reply to; `attachmentIds` are file IDs previously uploaded with `uploadFile`. Returns `SendMessageResult` with the assigned `id` and `timestamp`. |
| `fetchHistory(serverId, channelId, beforeId?, limit?)` | `fetch_history` | Returns messages older than `beforeId` (null = latest), up to `limit` (null = server default). Used for infinite-scroll pagination. |
| `editMessage(serverId, messageId, newContent)` | `edit_message` | Replaces message content. Server records `edited_at`. |
| `deleteMessage(serverId, messageId)` | `delete_message` | Permanently deletes a message. |
| `searchMessages(serverId, query, channelId?, limit?)` | `search_messages` | Full-text search. `channelId` scopes the search to one channel; null searches all subscribed channels. |
| `sendTyping(serverId, channelId)` | `send_typing` | Sends a typing indicator. Call at most once per ~3 s; the server broadcasts `TypingStarted` which expires after 8 s on the client. |

---

### Files and attachments

| Function | Rust command | What it does |
|---|---|---|
| `uploadFile(serverId, channelId, filePath)` | `upload_file` | Uploads a local file and returns a `file_id` integer. Pass to `sendMessage` as an attachment. |
| `fetchUrl(serverId, url, channelId)` | `fetch_url` | Asks the server to fetch a URL and store it as a file, returning a `file_id`. Used for link-preview attachments. |
| `downloadFile(serverId, fileId)` | `download_file` | Downloads a file by ID. Returns `DownloadResult` with an inline data URL and optional saved-to-disk path. |
| `addFavorite(serverId, fileId, originalUrl?)` | `add_favorite` | Saves a file to the local favorites store. Returns `FavoriteEntry`. |
| `listFavorites()` | `list_favorites` | Returns all locally-saved favorites (cross-server). |
| `removeFavorite(id)` | `remove_favorite` | Removes a favorite by its UUID. |

---

### Reactions and threads

| Function | Rust command | What it does |
|---|---|---|
| `addReaction(serverId, messageId, emoji, fileId?)` | `add_reaction` | Adds an emoji reaction to a message. `fileId` is provided for custom-emoji reactions (the emoji name alone is not enough to render them). |
| `removeReaction(serverId, messageId, emoji, fileId?)` | `remove_reaction` | Removes the current user's reaction. Same `fileId` rule applies. |
| `createThread(serverId, messageId, name?)` | `create_thread` | Creates a `Thread` channel parented to `messageId`. `name` defaults to a server-chosen label if omitted. |

---

### Members and moderation

| Function | Rust command | What it does |
|---|---|---|
| `getMembers(serverId)` | `get_members` | Returns the full member roster as `MemberInfo[]`. |
| `kickMember(serverId, memberKey)` | `kick_member` | Kicks a member (they can rejoin). `memberKey` is a `"vk_<hex>"` string. |
| `banMember(serverId, memberKey, reason?)` | `ban_member` | Bans a member with an optional reason. |
| `unbanMember(serverId, memberKey)` | `unban_member` | Removes a ban. |
| `listBanned(serverId)` | `list_banned` | Returns `BannedMember[]`. Public keys are `{ bytes }` objects — normalize before use. |
| `timeoutMember(serverId, memberKey, untilMs, reason)` | `timeout_member` | Mutes a member until `untilMs` (Unix ms). `reason` may be null. |
| `removeTimeout(serverId, memberKey)` | `remove_timeout` | Clears an active timeout immediately. |
| `listAuditEvents(serverId, beforeId, limit)` | `list_audit_events` | Paginates the server audit log. `beforeId` null = newest first. `actor`/`target` in each `AuditEvent` are `{ bytes }` objects. |
| `assignRole(serverId, memberKey, roleId)` | `assign_role` | Adds a role to a member. |
| `removeRole(serverId, memberKey, roleId)` | `remove_role` | Removes a role from a member. |
| `createRole(serverId, name, permissions, color?)` | `create_role` | Creates a new role. `permissions` is an integer bitmask. |
| `deleteRole(serverId, roleId)` | `delete_role` | Deletes a role and strips it from all members. |

---

### Direct messages

| Function | Rust command | What it does |
|---|---|---|
| `openDm(serverId, targetKey)` | `open_dm` | Opens or retrieves an existing DM channel with `targetKey` (`"vk_<hex>"`). Returns the `ChannelInfo` and the `MemberInfo` for the other participant. |
| `listDms(serverId)` | `list_dms` | Returns all DM conversations as `DmEntry[]`, each including the last message for preview. |
| `blockUser(serverId, targetKey)` | `block_user` | Blocks a user; they can no longer send DMs. |
| `unblockUser(serverId, targetKey)` | `unblock_user` | Removes a block. |

---

### DM end-to-end encryption

| Function | Rust command | What it does |
|---|---|---|
| `dmEncrypt(theirPublicKey, plaintext)` | `dm_encrypt` | Encrypts a plaintext string for a DM peer using AES-256-GCM with a key derived from the X25519 handshake. Returns a hex-encoded ciphertext with the nonce prepended. |
| `dmDecrypt(theirPublicKey, ciphertextHex)` | `dm_decrypt` | Decrypts a hex-encoded DM ciphertext. Throws if decryption fails (wrong key, tampered data, or if the message was sent in plaintext). |

---

### Voice presence (channel roster)

These are the older, lighter commands that track who is in a voice channel according to the server. They do not drive the audio pipeline.

| Function | Rust command | What it does |
|---|---|---|
| `joinVoice(serverId, channelId)` | `join_voice` | Registers the client in the server's voice-channel roster (broadcasts `MediaJoined`). Does not open any audio stream. |
| `leaveVoice(serverId, channelId)` | `leave_voice` | Removes the client from the roster (broadcasts `MediaLeft`). |
| `getVoiceState(serverId, channelId)` | `get_voice_state` | Returns the current channel roster as `VoiceMember[]`. Keys are `{ bytes }` objects. |

---

### Voice engine (local audio pipeline)

These commands drive the `VoiceController` (the local Opus/QUIC audio subsystem). They are distinct from the roster commands above — calling `voiceJoin` also calls `joinVoice` internally; calling `leaveVoice` without `voiceLeave` leaves the pipeline running.

| Function | Rust command | What it does |
|---|---|---|
| `voiceJoin(serverId, channelId)` | `voice_join` | Opens the QUIC stream session, derives and wraps the per-call key, and starts the audio send/mix/recv tasks. Also registers in the server roster. |
| `voiceLeave()` | `voice_leave` | Tears down the local pipeline and unregisters from the roster. No `serverId` needed — only one call can be active at a time. |
| `voiceSetMute(muted)` | `voice_set_mute` | Mutes/unmutes the local microphone on the audio pipeline. |
| `voiceSetDeafen(deafened)` | `voice_set_deafen` | Deafens/undeafens (also mutes mic while deafened). |
| `voiceGetState()` | `voice_get_state` | Returns the full `VoiceState` snapshot: active channel, mute/deafen flags, transmit state, and the `VoicePeer[]` roster. |
| `voiceToggleTransmit()` | `voice_toggle_transmit` | Toggles PTT transmit on/off. Returns the new transmit state as a boolean. |
| `voiceSetPeerVolume(pubkeyHex, volume)` | `voice_set_peer_volume` | Sets per-peer playback volume (0.0–2.0). `pubkeyHex` is a `"vk_<hex>"` string. |

---

### Voice settings

| Function | Rust command | What it does |
|---|---|---|
| `getVoiceMode()` | `get_voice_mode` | Returns the current voice activation mode: `"vad"` (voice-activity detection) or `"ptt"` (push-to-talk). |
| `setVoiceMode(mode)` | `set_voice_mode` | Persists the voice mode. |
| `getPttKey()` | `get_ptt_key` | Returns the key binding for push-to-talk (e.g. `"Space"`). |
| `setPttKey(key)` | `set_ptt_key` | Persists the PTT key binding. |
| `getPeerVolumes()` | `get_peer_volumes` | Returns a map of `"vk_<hex>" → volume` for all peers with a custom volume. |

---

### Audio recording / playback helpers

| Function | Rust command | What it does |
|---|---|---|
| `saveTempAudio(data)` | `save_temp_audio` | Writes a base64-encoded audio blob to a temp file. Returns the file path (used by the voice-message UI). |
| `startRecording()` | `start_recording` | Begins local microphone recording (independent of the voice pipeline). |
| `stopRecording()` | `stop_recording` | Stops recording and returns the path to the recorded file. |

---

### Notifications

| Function | Rust command | What it does |
|---|---|---|
| `showNotification(title, body)` | `show_notification` | Triggers a native OS notification. |
| `getNotificationPrefs()` | `get_notification_prefs` | Returns the locally-persisted `NotificationPrefs`. |
| `saveNotificationPrefs(prefs)` | `save_notification_prefs` | Persists the full prefs object. |

---

### Local server management

| Function | Rust command | What it does |
|---|---|---|
| `createLocalServer(name, template, privacy, iconPath?)` | `create_local_server` | Spawns a new local Farder server process and returns its connection result. `template` selects the channel/role preset; `privacy` is `"public"` or `"private"`. |
| `stopLocalServer(port)` | `stop_local_server` | Stops the local server running on `port`. |
| `getLocalServers()` | `get_local_servers` | Returns metadata for all locally-managed servers. |
| `listTemplates()` | `list_templates` | Returns available server templates (`TemplateInfo[]`). |

---

### Account deletion

| Function | Rust command | What it does |
|---|---|---|
| `requestDeletion(serverId)` | `request_deletion` | Initiates a deletion request on the server (may enter a grace period). |
| `cancelDeletion(serverId)` | `cancel_deletion` | Cancels a pending deletion request within the grace period. |
| `getDeletionStatus(serverId)` | `get_deletion_status` | Returns the current deletion status. Return type is `any` (untyped — the shape is server-defined). |

---

### Invites

| Function | Rust command | What it does |
|---|---|---|
| `createInvite(serverId, logServerId, maxUses?, requiresApproval?)` | `create_invite` | Creates an invite link. `logServerId` is the genesis hash for log/mesh-mode servers, or `null` for legacy servers. `maxUses` null = unlimited. `requiresApproval` (default `false`) — when `true`, emits `InviteCreated` with `requires_approval: true` so joiners must be approved. Returns `InviteResult` with the code and share-ready URLs. |
| `joinLogServer(serverId, logServerId, inviteCode)` | `join_log_server` | Emits a self-signed `MemberJoined` event so the joiner becomes a recognized log member who can post. `serverId` is the connection address (routes the request); `logServerId` is the genesis hash (stamps events and keys the device chain). Idempotent — returns immediately if already joined. Called automatically after a successful `connectServer` when an invite code was used and the server is mesh-mode (`result.server_id` is present). |
| `approveMember(serverId, logServerId, member)` | `approve_member` | Emit a signed `MemberApproved` event for the target member (hex pubkey). Requires approver identity to be unlocked. `serverId` routes the request; `logServerId` is the genesis hash for chain stamping. |
| `denyMember(serverId, logServerId, member)` | `deny_member` | Emit a signed `MemberRemoved` event to deny or remove a pending member. Same ID convention as `approveMember`. |
| `getMembershipStatus(serverId)` | `get_membership_status` | Returns `"member"` \| `"pending"` \| `"none"` for the caller's status on this server. Allowed for non-members so a pending joiner can poll. |
| `getPendingMembers(serverId)` | `get_pending_members` | Returns `MemberInfo[]` of members awaiting approval. Gated server-side to approvers (KICK\_MEMBERS) and the owner. |

---

### Rich link embeds (Phase 6)

These four functions speak to the relay's embed fetch proxy (phase two). No
server session or identity is required — each opens a throwaway QUIC connection
to the default relay. See `docs/modules/relay-embed.md` for the relay-side
protocol reference.

| Function | Rust command | What it does |
|---|---|---|
| `getLinkEmbed(url)` | `get_link_embed` | Asks the relay to resolve an external URL and return `EmbedOutcome`. Hits a 5-minute client-side cache before opening a connection. Returns `{ Embed: LinkEmbed }`, `"Unsupported"`, or `"Unavailable"`. |
| `getProxiedMedia(url)` | `get_proxied_media` | Streams a media asset (image or direct video) through the relay, returning `{ content_type, data_base64 }`. The caller wraps `data_base64` in a `Blob` URL for rendering; no CDN is ever contacted from the client. |
| `getDataSaverEmbeds()` | `get_data_saver_embeds` | Returns `true` if data-saver mode is enabled (embeds do not auto-load). |
| `setDataSaverEmbeds(enabled)` | `set_data_saver_embeds` | Persists the data-saver embed setting to `settings.json`. |

---

### Themes

| Function | Rust command | What it does |
|---|---|---|
| `listThemes()` | `list_themes` | Returns all available themes (built-in and user). |
| `loadThemeCss(id)` | `load_theme_css` | Returns the raw CSS text for a theme by ID. |
| `getActiveTheme()` | `get_active_theme` | Returns the currently-applied theme as `ActiveTheme` (id + CSS). |
| `setActiveTheme(id)` | `set_active_theme` | Persists and applies a theme by ID. |
| `openThemesFolder()` | `open_themes_folder` | Opens the user themes directory in the OS file manager. |
| `forkTheme(baseId, newId, name)` | `fork_theme` | Copies a built-in or user theme to a new editable user theme. Returns the initial CSS. |
| `saveUserTheme(id, css)` | `save_user_theme` | Overwrites the CSS for a user-owned theme. |
| `addThemeAsset(themeId, sourcePath, targetFilename)` | `add_theme_asset` | Copies a file (image, font) into the theme's asset directory. Returns the relative asset URL for use in CSS. |
| `deleteUserTheme(id)` | `delete_user_theme` | Permanently removes a user theme. |
| `renameUserTheme(id, newName)` | `rename_user_theme` | Renames a user theme. |
| `getThemeOrder()` | `get_theme_order` | Returns the display order of theme IDs. |
| `setThemeOrder(ids)` | `set_theme_order` | Persists a new display order. |

---

## State it owns

Neither file owns runtime state. They are pure function/type modules; all mutable state lives in the Rust commands or in React context.

---

## Integration map

- **`client/src-tauri/src/commands.rs`** — every Rust `#[tauri::command]` named in the tables above. The `invoke("command_name")` string must match exactly.
- **`client/src-tauri/src/bridge.rs`** — receives `ServerEvent`s and re-emits them as `"server:*"` Tauri events; this file is the source of the pubkey-as-string convention (`"vk_<hex>"`) that `publicKeyToString()` produces.
- **`client/src/hooks/useServerEvents.ts`** — registers `listen("server:*", …)` listeners for the events bridged from Rust; relies on `publicKeyToString()` matching the form bridge.rs emits.
- **`client/src/contexts/ServerContext.tsx`** (and related) — imports and calls these wrapper functions; receives the typed return values.

---

## Known gotchas

- **`invoke()` is untyped at runtime.** TypeScript types here are best-effort; if a Rust command is renamed or its parameter names change (Tauri maps camelCase JS names to snake_case Rust names automatically), the call silently returns an error at runtime. There is no compile-time check that the command is registered in `generate_handler!`.
- **All `{ bytes }` public keys must be normalized.** Forgetting `publicKeyToString()` before a string comparison or React key is the most common class of bug in this layer. `BannedMember.public_key`, `MemberInfo.public_key`, `MessageInfo.author`, `AuditEvent.actor/target`, `VoiceMember.public_key`, and `VoicePeer.pubkey` all arrive as byte objects.
- **`VoiceMember` vs `VoicePeer`:** `VoiceMember` is from `getVoiceState()` (server roster, older path); `VoicePeer` is from `voiceGetState()` (local audio pipeline). They look similar but carry different fields and are not interchangeable.
- **`updateChannel` and `setCategory`:** the `setCategory` boolean is derived internally from whether `opts.categoryId` was provided. Passing `categoryId: null` explicitly removes a channel from its category only if `setCategory` is true — which it will be. Omitting `categoryId` from `opts` entirely leaves the category unchanged.
- **`getDeletionStatus` returns `any`.** The return type has never been typed. Do not destructure it without a guard.
- **`voiceLeave` takes no arguments.** Only one call session can be active at a time; the controller tracks the active session internally.
