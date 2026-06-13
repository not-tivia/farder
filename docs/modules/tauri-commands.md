# Tauri command layer

> **File(s):** `client/src-tauri/src/commands.rs`, `client/src-tauri/src/themes.rs`, `client/src-tauri/src/tenor.rs`, `client/src-tauri/src/translation.rs`, `client/src-tauri/src/book.rs`
> **Layer:** Tauri command
> **Last reviewed:** 2026-06-04

## Purpose

This layer exposes all Rust functionality to the React frontend as Tauri
commands (`#[tauri::command]`). Every command is registered in
`generate_handler!` in `client/src-tauri/src/main.rs` and called from the
frontend via `invoke("command_name", { ...args })`, almost always through the
typed wrappers in `client/src/lib/tauri-bridge.ts`. Commands that talk to a
server always route through `bridge::send_request`, which sends a
`ServerRequest` over the QUIC connection and returns a `ServerResponse`.
Commands that are purely local (identity, settings, audio) never touch the
network.

---

## Command ↔ invoke seam — gotchas

> This seam has shipped broken features before (voice-channel join). Read this
> section before touching any command.

There are **three places** that must all agree on the same snake_case name:

1. The `invoke("X")` call in `tauri-bridge.ts` (plain string — not type-checked).
2. The `#[tauri::command] pub fn X(...)` in the relevant `.rs` file.
3. The `commands::X` (or `themes::X`, `book::X`, etc.) entry in the
   `generate_handler!` list in `main.rs`.

If any one of the three is missing or misspelled, the command silently fails at
runtime with a "command not found" error. The Rust compiler does **not** catch
this.

**How to audit:**

```bash
# All invoke names used in TypeScript:
grep -h 'invoke(' client/src/lib/tauri-bridge.ts | grep -oP '(?<=invoke[(<][^"]*")[^"]+' | sort -u

# All names registered in generate_handler!:
grep -oP '(?<=commands::|themes::|tenor::|translation::|book::)\w+' \
  client/src-tauri/src/main.rs | sort -u

# Diff the two lists. Every invoke name must appear in the handler list.
```

A zero-diff means no drift. Add this check to any CI that touches commands.

**Naming rule:** Rust function names use `snake_case`; Tauri converts them to
the same string for `invoke`. The TypeScript wrappers use `camelCase` function
names that call `invoke("snake_case_name")`. These two namespaces are
independent — what matters for the seam is only the string inside `invoke(...)`.

---

## Group 1 — Identity

These commands manage the local cryptographic identity keypair stored in
`<data dir>/identity.key`. They never contact a server. All five live in
`client/src-tauri/src/identity.rs` (logic on `IdentityStore`). The private
key never crosses the Tauri boundary — only the public key and recovery
phrase are returned to the frontend.

---

### `identity_status() -> IdentityStatus`

**What it does:** classifies `<data dir>/identity.key` by file length without
decrypting: `"none"` (file absent), `"plaintext"` (legacy 32-byte key that
needs migration), or `"encrypted"` (current format). Drives which screen the
IdentityGate shows.
**Side effects:** none (read-only disk check).
**invoke name:** `"identity_status"` → `identityStatus()`.

---

### `create_identity(pin) -> { public_key, recovery_phrase }`

**What it does:** generates a fresh Ed25519 identity, encrypts it under a
4-digit PIN (Argon2id + AES-256-GCM), writes the blob to
`<data dir>/identity.key`, loads the key into `AppState`, and returns the
public key plus a 24-word BIP39 recovery phrase.
**Parameters:** `pin` — 4-digit PIN string.
**Returns:** `{ public_key: "vk_<hex>", recovery_phrase: "<24 words>" }`.
**Side effects:** writes `identity.key`; mutates `AppState::signing_key_bytes`.
Async — crypto runs off the UI thread.
**invoke name:** `"create_identity"` → `createIdentity(pin)`.

---

### `unlock_identity(pin) -> public_key`

**What it does:** decrypts the stored identity blob with the PIN, loads the
key into `AppState`, and returns the public key.
**Parameters:** `pin` — 4-digit PIN string.
**Returns:** `"vk_<hex>"` on success; errors with `IncorrectPin` on a wrong PIN
(no lockout).
**Side effects:** mutates `AppState::signing_key_bytes`.
**invoke name:** `"unlock_identity"` → `unlockIdentity(pin)`.

---

### `migrate_plaintext_identity(pin) -> { public_key, recovery_phrase }`

**What it does:** one-time migration — reads the legacy plaintext 32-byte key,
re-stores it encrypted under the PIN (key value preserved), loads it into
`AppState`, and returns the public key plus a 24-word BIP39 recovery phrase.
**Parameters:** `pin` — 4-digit PIN string.
**Returns:** `{ public_key: "vk_<hex>", recovery_phrase: "<24 words>" }`.
**Side effects:** overwrites `identity.key` with the encrypted blob; mutates
`AppState::signing_key_bytes`.
**invoke name:** `"migrate_plaintext_identity"` → `migratePlaintextIdentity(pin)`.

---

### `restore_identity(phrase, pin) -> public_key`

**What it does:** rebuilds the Ed25519 key from a 24-word BIP39 recovery
phrase, re-stores it encrypted under a new PIN, and loads it into `AppState`.
**Parameters:** `phrase` — 24-word BIP39 phrase; `pin` — new 4-digit PIN.
**Returns:** `"vk_<hex>"` on success; errors with `InvalidPhrase` on a bad
checksum or unknown words.
**Side effects:** writes `identity.key`; mutates `AppState::signing_key_bytes`.
**invoke name:** `"restore_identity"` → `restoreIdentity(phrase, pin)`.

---

### `get_public_key(state) -> Option<String>`

**What it does:** derives the public key from the currently loaded signing key
bytes without touching disk.
**Returns:** `"vk_<hex>"` or `null` if no key is loaded.
**Side effects:** none.
**invoke name:** `"get_public_key"` → `getPublicKey()`.

---

## Group 2 — Local profile (display name, bio, color, avatar)

All commands in this group read/write `~/.farder/profile.json` (JSON blob) or
`~/.farder/avatar.png`. None contact a server.

---

### `set_display_name(name) -> Result<(), String>` / `get_display_name() -> Option<String>`

**What it does:** persists / reads the `display_name` field in `profile.json`.
**invoke names:** `"set_display_name"` / `"get_display_name"`.

---

### `set_bio(bio) -> Result<(), String>` / `get_bio() -> Option<String>`

**What it does:** persists / reads the `bio` field in `profile.json`.
**invoke names:** `"set_bio"` / `"get_bio"`.

---

### `set_profile_color(color) -> Result<(), String>` / `get_profile_color() -> Option<String>`

**What it does:** persists / reads the `banner_color` field (stored as
`"banner_color"` internally despite the command name `profile_color`).
**Gotcha:** the JSON key is `"banner_color"`, not `"profile_color"`. Both sides
agree on the command name; only the storage key differs.
**invoke names:** `"set_profile_color"` / `"get_profile_color"`.

---

### `set_avatar(file_path) -> Result<String, String>` / `get_avatar() -> Option<String>`

**What it does:** `set_avatar` reads an image from `file_path`, validates it
(magic-byte sniff: PNG/JPEG/GIF/WebP only; max 2 MB via `profile_sync::validate_avatar_bytes`),
writes it to `~/.farder/avatar.png`, then spawns a background task that calls
`push_profile_everywhere` to push the updated profile to every connected server.
Returns a magic-sniffed data URL. `get_avatar` reads the stored file and returns
a data URL, or `null` if none exists.
**Returns (`set_avatar`):** `"data:<mime>;base64,<b64>"` — MIME type inferred
from magic bytes (PNG/JPEG/GIF/WebP; falls back to `application/octet-stream`).
**Side effects:** disk write to `~/.farder/avatar.png`; spawns a tokio task that
calls `push_profile_everywhere` (network I/O to every connected server).
**invoke names:** `"set_avatar"` / `"get_avatar"`.

---

### `set_server_avatar(server_id, file_path) -> Result<String, String>` / `get_server_avatar(server_id) -> Option<String>`

**What it does:** stores / retrieves a per-server icon in
`~/.farder/server_avatars/<safe_name>.png`, where `safe_name` replaces `:`, `.`,
and `/` with `_` to produce a safe filename.
**Returns (`set_server_avatar`):** `"data:image/png;base64,<b64>"`.
**invoke names:** `"set_server_avatar"` / `"get_server_avatar"`.

---

## Group 2b — Profile sync (status, per-server avatar override, member profile fetch)

These commands are part of the profile-sync feature. They interact with
`profile_sync.rs`, which builds signed profiles and pushes them to servers.

---

### `get_profile_status() -> Option<String>`

**What it does:** reads the `"status"` field from `~/.farder/profile.json` and
returns it, or `null` if unset.
**Side effects:** none (read-only disk access).
**invoke name:** `"get_profile_status"` → `getProfileStatus()`.

---

### `set_profile_status(status) -> Result<(), String>`

**What it does:** writes `status` (trimmed; `null` or empty string clears it) to
the `"status"` field in `~/.farder/profile.json`, then spawns a background task
that calls `push_profile_everywhere` to propagate the change to every connected
server.
**Parameters:** `status` — optional string, max 128 characters (after trimming).
**Returns:** `Ok(())` on success; errors if the string exceeds 128 chars or the
file write fails.
**Side effects:** disk write to `~/.farder/profile.json`; spawns a tokio task
for `push_profile_everywhere` (network I/O).
**invoke name:** `"set_profile_status"` → `setProfileStatus(status)`.

---

### `set_server_avatar_override(server_id, file_path) -> Result<String, String>`

**What it does:** reads the image at `file_path`, validates it (PNG/JPEG/GIF/WebP
by magic bytes, max 2 MB), writes it to
`~/.farder/profile_overrides/<safe_server_id>.img`, then synchronously calls
`push_profile` to push the updated profile to that server. The per-server
override takes precedence over the global avatar for that server only.
**Parameters:** `server_id` — server address string; `file_path` — local image path.
**Returns:** magic-sniffed data URL of the saved image; errors if validation
fails or the server sync fails (override is still saved locally in the latter
case — the error message says so).
**Side effects:** disk write to `profile_overrides/`; synchronous
`push_profile` call (network I/O).
**invoke name:** `"set_server_avatar_override"` → `setServerAvatarOverride(serverId, filePath)`.

---

### `clear_server_avatar_override(server_id) -> Result<(), String>`

**What it does:** deletes `~/.farder/profile_overrides/<safe_server_id>.img`
(no-op if absent) and calls `push_profile` to push the profile without an
override (i.e. the global avatar or no avatar) to that server.
**Side effects:** disk delete; synchronous `push_profile` (network I/O).
**invoke name:** `"clear_server_avatar_override"` → `clearServerAvatarOverride(serverId)`.

---

### `get_server_avatar_override(server_id) -> Option<String>`

**What it does:** reads `~/.farder/profile_overrides/<safe_server_id>.img` and
returns a magic-sniffed data URL, or `null` if no override is set.
**Side effects:** none (read-only disk access).
**invoke name:** `"get_server_avatar_override"` → `getServerAvatarOverride(serverId)`.

---

### `get_member_profile(server_id, public_key, profile_hash) -> Result<Option<MemberProfileView>, String>`

**What it does:** resolves a member's profile by its hash. Checks the on-disk
cache (`~/.farder/profile_cache/<hash>`) first; on a hit it re-verifies the
signature and public-key binding before returning (corrupt or wrong-key entries
are deleted and re-fetched). On a miss, sends
`ServerRequest::GetMemberProfile { member_key }` to the server, then verifies
the returned blob (signature, key match, hash match) before writing it to the
cache and returning.
**Parameters:**
- `server_id` — server to query if the cache misses.
- `public_key` — the member's public key string (`"vk_<hex>"`).
- `profile_hash` — 64-char lowercase hex SHA-256 hash; `null` returns `null`
  immediately.
**Returns:** `{ avatar_data_url: string | null, status: string | null }` or
`null` if the server has no profile for that member. Errors on signature or hash
mismatch.
**Side effects:** may write to `~/.farder/profile_cache/` (on network fetch) or
delete a corrupt entry; sends one `GetMemberProfile` request (network I/O) on
cache miss.
**Connects to:** `farder_crypto::profile::SignedProfile::from_bytes` + `verify()`;
the in-process JS cache in `useMemberProfile.ts` (module-level `Map`) which
deduplicates concurrent requests for the same hash.
**invoke name:** `"get_member_profile"` → `getMemberProfile(serverId, publicKey, profileHash)`.

---

## Group 3 — Server connection

Commands that establish, query, and tear down connections to Farder servers.

---

### `connect_server(app, state, address, invite_code, setup_token) -> Result<ConnectResult, String>`

**What it does:** establishes a QUIC connection to `address`, authenticates
with the stored keypair (optionally with `invite_code` or `setup_token`),
spawns the event-reader loop and the media-datagram loop, inserts the
`ServerConnection` into `AppState::servers`, and sends `ServerRequest::GetServerInfo`
to get initial channel/category/role data.
**Parameters:**
- `address` — `"<ip>:<port>"` string, parsed to `SocketAddr`.
- `invite_code` — required when joining for the first time; `null` if already a member.
- `setup_token` — first-run claim token used to become owner of a fresh server.

**Returns:** `ConnectResult { server_name, member_count, channels, categories, roles, owner_public_key }`.
**Side effects:** registers `ServerConnection` in `AppState::servers`; spawns two
`tokio` tasks (event reader + datagram loop); saves address to `settings.json`
(`address` key) and to `servers.json`; starts event relay to the webview via
`bridge::spawn_event_reader`.
**ServerRequest sent:** `GetServerInfo`.
**invoke name:** `"connect_server"` → `connectServer()`.

---

### `disconnect_server(state, server_id) -> Result<(), String>`

**What it does:** removes the `ServerConnection` from `AppState::servers`,
aborts the event-reader task, and removes the entry from `servers.json`.
**Side effects:** aborts the event-reader `JoinHandle`; mutates `servers.json`.
**invoke name:** `"disconnect_server"` → `disconnectServer()`.

---

### `list_servers(state) -> Vec<ServerEntry>`

**What it does:** returns all currently connected servers (live in-memory map,
not the disk list).
**Returns:** `[{ id: string, name: string }]`.
**invoke name:** `"list_servers"` → `listServers()`.

---

### `get_saved_servers() -> Vec<ServerEntry>`

**What it does:** reads `servers.json` from disk and returns the persisted list.
**invoke name:** `"get_saved_servers"` → `getSavedServers()`.

---

### `get_server_info(state, server_id) -> Result<ConnectResult, String>`

**What it does:** re-fetches server metadata for an already-connected server.
**ServerRequest sent:** `GetServerInfo`.
**invoke name:** `"get_server_info"` → `getServerInfo()`.

---

### `save_last_server(address) -> Result<(), String>` / `get_last_server() -> Option<String>`

**What it does:** persists / reads the `"address"` key in `settings.json`, used
to pre-fill the connect dialog on next launch. `connect_server` calls
`save_last_server` automatically.
**invoke names:** `"save_last_server"` (no TS wrapper — called internally) /
`"get_last_server"` → `getLastServer()`.

---

## Group 4 — Channels and categories

Admin commands that create, update, delete, and reorder channels and categories.
All send a `ServerRequest` and expect `ServerResponse::Ok` or an error.

---

### `create_channel(state, server_id, name, channel_type, category_id) -> Result<(), String>`

**What it does:** creates a new channel of type `"Text"`, `"Announcement"`, or
`"Voice"`. Unknown types default to `Text`.
**Parameters:** `channel_type` — one of `"Text"`, `"Announcement"`, `"Voice"`.
**ServerRequest:** `CreateChannel { name, channel_type, category_id, position: None }`.
**invoke name:** `"create_channel"` → `createChannel()`.

---

### `create_category(state, server_id, name) -> Result<(), String>`

**ServerRequest:** `CreateCategory { name, position: None }`.
**invoke name:** `"create_category"` → `createCategory()`.

---

### `delete_channel(state, server_id, channel_id) -> Result<(), String>`

**ServerRequest:** `DeleteChannel { channel_id }`.
**invoke name:** `"delete_channel"` → `deleteChannel()`.

---

### `delete_category(state, server_id, category_id) -> Result<(), String>`

**ServerRequest:** `DeleteCategory { category_id }`.
**invoke name:** `"delete_category"` → `deleteCategory()`.

---

### `update_channel(state, server_id, channel_id, name, topic, nsfw, slow_mode_secs, category_id, set_category, position) -> Result<(), String>`

**What it does:** updates any combination of channel fields. Category
membership uses a two-flag encoding to distinguish "don't change category"
from "explicitly uncategorize":

- `set_category=true` + `category_id=Some(x)` → move to category `x`.
- `set_category=true` + `category_id=None` → remove from any category.
- `set_category=false` (or absent) → leave category unchanged.

The TypeScript wrapper (`updateChannel`) handles this automatically by setting
`setCategory = opts.categoryId !== undefined`.
**ServerRequest:** `UpdateChannel { ... }` (note: `retention_secs` is always `None`).
**invoke name:** `"update_channel"` → `updateChannel()`.

---

### `update_category(state, server_id, category_id, name, position) -> Result<(), String>`

**ServerRequest:** `UpdateCategory { category_id, name, position }`.
**invoke name:** `"update_category"` → `updateCategory()`.

---

### `set_channel_override(state, server_id, channel_id, role_id, allow, deny) -> Result<(), String>`

**What it does:** sets a per-role permission override on a channel. `allow` and
`deny` are bitmasks.
**ServerRequest:** `SetChannelOverride { channel_id, role_id, allow, deny }`.
**invoke name:** `"set_channel_override"` → `setChannelOverride()`.

---

## Group 5 — Messages

---

### `send_message(state, server_id, channel_id, content, reply_to, attachment_ids) -> Result<SendMessageResult, String>`

**What it does:** posts a chat message to a channel, optionally replying to
`reply_to` (message id) and attaching previously-uploaded file ids.
**Returns:** `{ id: u64, timestamp: u64 }`.
**ServerRequest:** `SendMessage { channel_id, content, reply_to, attachment_ids }`.
**invoke name:** `"send_message"` → `sendMessage()`.

---

### `fetch_history(state, server_id, channel_id, before_id, limit) -> Result<Vec<MessageInfo>, String>`

**What it does:** returns up to `limit` (default 50) messages from `channel_id`
older than `before_id` (cursor-based pagination).
**ServerRequest:** `FetchHistory { channel_id, before_id, limit }`.
**invoke name:** `"fetch_history"` → `fetchHistory()`.

---

### `subscribe_channels(state, server_id, channel_ids) -> Result<(), String>`

**What it does:** tells the server to begin pushing events for the listed
channels to this connection. Must be called before events like `NewMessage`
are received for those channels.
**ServerRequest:** `Subscribe { channel_ids }`.
**invoke name:** `"subscribe_channels"` → `subscribeChannels()`.

---

### `edit_message(state, server_id, message_id, new_content) -> Result<(), String>`

**ServerRequest:** `EditMessage { message_id, new_content }`.
**invoke name:** `"edit_message"` → `editMessage()`.

---

### `delete_message(state, server_id, message_id) -> Result<(), String>`

**ServerRequest:** `DeleteMessage { message_id }`.
**invoke name:** `"delete_message"` → `deleteMessage()`.

---

### `search_messages(state, server_id, query, channel_id, limit) -> Result<Vec<MessageInfo>, String>`

**What it does:** full-text search across message history. Optionally scoped to
a single channel; default limit 20.
**ServerRequest:** `Search { query, channel_id, limit }`.
**invoke name:** `"search_messages"` → `searchMessages()`.

---

### `send_typing(state, server_id, channel_id) -> Result<(), String>`

**What it does:** fires a `Typing` notification to all channel subscribers.
Fire-and-forget — errors are discarded so a missed typing indicator never
blocks the caller.
**ServerRequest:** `Typing { channel_id }`.
**invoke name:** `"send_typing"` → `sendTyping()`.

---

## Group 6 — Members and moderation

Public-key parameters accept either `"vk_<hex>"` or bare hex; the internal
`parse_public_key` helper strips the `"vk_"` prefix.

---

### `get_members(state, server_id) -> Result<Vec<MemberInfo>, String>`

**ServerRequest:** `GetMembers`.
**invoke name:** `"get_members"` → `getMembers()`.

---

### `kick_member(state, server_id, member_key) -> Result<(), String>`

**ServerRequest:** `KickMember { member_key }`.
**invoke name:** `"kick_member"` → `kickMember()`.

---

### `ban_member(state, server_id, member_key, reason) -> Result<(), String>`

**ServerRequest:** `BanMember { member_key, reason }`.
**invoke name:** `"ban_member"` → `banMember()`.

---

### `unban_member(state, server_id, member_key) -> Result<(), String>`

**ServerRequest:** `UnbanMember { member_key }`.
**invoke name:** `"unban_member"` → `unbanMember()`.

---

### `list_banned(state, server_id) -> Result<Vec<BannedMember>, String>`

**ServerRequest:** `ListBanned`.
**invoke name:** `"list_banned"` → `listBanned()`.

---

### `timeout_member(state, server_id, member_key, until_ms, reason) -> Result<(), String>`

**What it does:** temporarily restricts a member until `until_ms` (Unix ms).
**ServerRequest:** `TimeoutMember { member_key, until_ms, reason }`.
**invoke name:** `"timeout_member"` → `timeoutMember()`.

---

### `remove_timeout(state, server_id, member_key) -> Result<(), String>`

**ServerRequest:** `RemoveTimeout { member_key }`.
**invoke name:** `"remove_timeout"` → `removeTimeout()`.

---

### `list_audit_events(state, server_id, before_id, limit) -> Result<Vec<AuditEvent>, String>`

**What it does:** fetches the server audit log, cursor-paged by `before_id`.
**ServerRequest:** `ListAuditEvents { before_id, limit }`.
**invoke name:** `"list_audit_events"` → `listAuditEvents()`.

---

### `create_role(state, server_id, name, permissions, color) -> Result<(), String>`

**Parameters:** `permissions` — bitmask of allowed permissions; `color` — optional hex string.
**ServerRequest:** `CreateRole { name, permissions, color, position: None }`.
**invoke name:** `"create_role"` → `createRole()`.

---

### `delete_role(state, server_id, role_id) -> Result<(), String>`

**ServerRequest:** `DeleteRole { role_id }`.
**invoke name:** `"delete_role"` → `deleteRole()`.

---

### `assign_role(state, server_id, member_key, role_id) -> Result<(), String>`

**ServerRequest:** `AssignRole { member_key, role_id }`.
**invoke name:** `"assign_role"` → `assignRole()`.

---

### `remove_role(state, server_id, member_key, role_id) -> Result<(), String>`

**ServerRequest:** `RemoveRole { member_key, role_id }`.
**invoke name:** `"remove_role"` → `removeRole()`.

---

## Group 7 — Reactions and threads

---

### `add_reaction(state, server_id, message_id, emoji, file_id) -> Result<(), String>`

**What it does:** adds a reaction to a message. `emoji` is a Unicode string for
standard emoji; `file_id` is set for custom (book) emoji.
**ServerRequest:** `AddReaction { message_id, emoji, file_id }`.
**invoke name:** `"add_reaction"` → `addReaction()`.

---

### `remove_reaction(state, server_id, message_id, emoji, file_id) -> Result<(), String>`

**ServerRequest:** `RemoveReaction { message_id, emoji, file_id }`.
**invoke name:** `"remove_reaction"` → `removeReaction()`.

---

### `create_thread(state, server_id, message_id, name) -> Result<(), String>`

**What it does:** creates a thread from an existing message, optionally with
a custom name.
**ServerRequest:** `CreateThread { message_id, name }`.
**invoke name:** `"create_thread"` → `createThread()`.

---

## Group 8 — File transfer

---

### `pick_file() -> Result<Option<String>, String>`

**What it does:** opens a native OS file picker dialog (blocking; runs in
`spawn_blocking`) and returns the selected file path as a string, or `null` if
the user cancels.
**Side effects:** spawns a blocking thread; opens a native OS dialog.
**invoke name:** `"pick_file"` → `pickFile()`.

---

### `upload_file(state, server_id, channel_id, file_path) -> Result<u64, String>`

**What it does:** reads the file, computes SHA-256, opens a new QUIC bi-stream
on the existing connection (separate from the main request stream), sends an
`UploadRequest` frame, streams the raw bytes, waits for `UploadResponse::Complete`.
If the server already has this hash it responds with `Complete` immediately
(deduplication).
**Returns:** `file_id` (`u64`).
**Side effects:** opens a new QUIC bi-stream on the live server connection; network I/O.
**Connects to:** `upload_file_internal_with_channel` (shared with `book.rs`'s
internal uploads). Uses its own stream — does NOT use `bridge::send_request`.
**invoke name:** `"upload_file"` → `uploadFile()`.

---

### `download_file(state, server_id, file_id) -> Result<DownloadResult, String>`

**What it does:** sends a `DownloadRequest` on a new QUIC bi-stream and reads
the file data. For images (`mime_type` starts with `"image/"`) returns a base64
data URL in `data_url`. For other types saves to the OS downloads directory and
returns the path in `saved_path`.
**Returns:** `DownloadResult { data_url, file_name, mime_type, saved_path }`.
**Side effects:** opens a new QUIC bi-stream; may write a file to the OS downloads
directory.
**invoke name:** `"download_file"` → `downloadFile()`.

---

### `fetch_url(state, server_id, url, channel_id) -> Result<u64, String>`

**What it does:** asks the server to fetch an external URL and store it as an
attachment, returning the resulting `file_id`. Used for GIF embeds (the server
fetches from Tenor so the client's IP is never exposed).
**ServerRequest:** `FetchUrl { url, channel_id }`.
**invoke name:** `"fetch_url"` → `fetchUrl()`.

---

## Group 9 — Favorites

Favorites are locally stored copies of server images (not dependent on the
server remaining connected). Backed by `~/.farder/favorites.json` (index) and
`~/.farder/favorites/<sha256>` (raw bytes per file).

---

### `add_favorite(state, server_id, file_id, original_url) -> Result<FavoriteEntry, String>`

**What it does:** downloads the file from the server via a raw QUIC bi-stream,
stores the raw bytes under its SHA-256 hash in `~/.farder/favorites/`, appends
an entry to `favorites.json`, and returns the `FavoriteEntry`. Idempotent — a
second call for the same hash is a no-op.
**Returns:** `FavoriteEntry { id, file_name, mime_type, data_url, source_server, original_url, favorited_at }`.
**Side effects:** disk write; network I/O.
**invoke name:** `"add_favorite"` → `addFavorite()`.

---

### `list_favorites() -> Result<Vec<FavoriteEntry>, String>`

**What it does:** reads `favorites.json` and returns all entries. No network.
**invoke name:** `"list_favorites"` → `listFavorites()`.

---

### `remove_favorite(id) -> Result<(), String>`

**What it does:** removes the entry from `favorites.json` and deletes the
corresponding raw file from `~/.farder/favorites/<id>`.
**Side effects:** disk delete.
**invoke name:** `"remove_favorite"` → `removeFavorite()`.

---

## Group 10 — Direct messages

---

### `open_dm(state, server_id, target_key) -> Result<Value, String>`

**What it does:** creates or retrieves a DM channel with `target_key`.
**Returns:** `{ channel: ChannelInfo, participant: MemberInfo }`.
**ServerRequest:** `OpenDm { target_key }`.
**invoke name:** `"open_dm"` → `openDm()`.

---

### `list_dms(state, server_id) -> Result<Vec<Value>, String>`

**ServerRequest:** `ListDms`.
**invoke name:** `"list_dms"` → `listDms()`.

---

### `block_user(state, server_id, target_key) -> Result<(), String>`

**ServerRequest:** `BlockUser { target_key }`.
**invoke name:** `"block_user"` → `blockUser()`.

---

### `unblock_user(state, server_id, target_key) -> Result<(), String>`

**ServerRequest:** `UnblockUser { target_key }`.
**invoke name:** `"unblock_user"` → `unblockUser()`.

---

## Group 11 — DM end-to-end encryption

These are synchronous (no async, no server round-trip). The shared secret is
derived via X25519 ECDH over the two parties' Ed25519 keys (convert-then-DH
inside `farder-crypto`), then used as an AES-256-GCM key.

---

### `dm_encrypt(state, their_public_key, plaintext) -> Result<String, String>`

**What it does:** derives the DM shared secret from our signing key and
`their_public_key`, encrypts `plaintext` with AES-256-GCM (nonce prepended),
and returns the ciphertext as a lowercase hex string.
**Returns:** hex-encoded ciphertext.
**Side effects:** none (in-memory only).
**Connects to:** `farder_crypto::key_exchange::derive_dm_shared_secret` +
`farder_crypto::encryption::encrypt`.
**invoke name:** `"dm_encrypt"` → `dmEncrypt()`.

---

### `dm_decrypt(state, their_public_key, ciphertext_hex) -> Result<String, String>`

**What it does:** inverse of `dm_encrypt` — decodes hex, derives the same shared
secret, decrypts, and returns plaintext UTF-8.
**Returns:** plaintext string.
**Side effects:** none.
**invoke name:** `"dm_decrypt"` → `dmDecrypt()`.

---

## Group 12 — Voice presence (server roster)

These commands update the server-side voice roster (who the server lists as
present in a voice channel) and broadcast `MediaJoined`/`MediaLeft` events to
all members. They are **distinct from the audio pipeline** commands in Group 13.
The frontend calls both `join_voice` (roster) AND `voice_join` (audio) when
entering a voice channel.

---

### `join_voice(state, server_id, channel_id) -> Result<(), String>`

**What it does:** registers the local user's presence in `channel_id`'s
server-side voice roster.
**ServerRequest:** `JoinChannelMedia { channel_id }`.
**Side effects:** server broadcasts `MediaJoined` event to all members
(re-emitted by `bridge.rs` as `server:voice_joined`).
**invoke name:** `"join_voice"` → `joinVoice()`.

---

### `leave_voice(state, server_id, channel_id) -> Result<(), String>`

**ServerRequest:** `LeaveChannelMedia { channel_id }`.
**Side effects:** server broadcasts `MediaLeft` → `server:voice_left`.
**invoke name:** `"leave_voice"` → `leaveVoice()`.

---

### `get_voice_state(state, server_id, channel_id) -> Result<Vec<VoiceMember>, String>`

**What it does:** returns the current voice roster snapshot for `channel_id`.
**ServerRequest:** `GetMediaState { channel_id }`.
**invoke name:** `"get_voice_state"` → `getVoiceState()`.

---

### `join_channel_media` / `leave_channel_media` / `get_media_state`

These are **duplicate commands** that send the same `ServerRequest` variants as
`join_voice`, `leave_voice`, and `get_voice_state` respectively, but return
slightly different types (`get_media_state` returns `serde_json::Value` instead
of typed `Vec<VoiceMember>`). Both sets are registered in `generate_handler!`.
Prefer the `join_voice`/`leave_voice`/`get_voice_state` variants for new code;
the `*_channel_media`/`get_media_state` variants are legacy scaffolding.
**invoke names:** `"join_channel_media"`, `"leave_channel_media"`, `"get_media_state"`.

---

## Group 13 — Voice stream protocol (media signaling)

These commands are the server-protocol layer of the voice audio pipeline —
they manage stream sessions, track enable/disable, deafen state, and E2EE key
distribution. The local audio pipeline itself is managed by Group 14
(`voice_*` commands).

---

### `join_stream(state, server_id, channel_id) -> Result<Vec<u8>, String>`

**What it does:** joins the voice stream session for `channel_id`.
**Returns:** `session_id` as a raw byte vector (16 bytes).
**ServerRequest:** `JoinStream { channel_id }`.
**invoke name:** `"join_stream"`. (No TS wrapper in `tauri-bridge.ts` — called
directly if needed.)

---

### `leave_stream(state, server_id) -> Result<(), String>`

**ServerRequest:** `LeaveStream`.
**invoke name:** `"leave_stream"`.

---

### `enable_track(state, server_id, kind) -> Result<(), String>`

**What it does:** tells the server the local client is now transmitting a track
of the given kind (`"audio"` or `"video"`).
**ServerRequest:** `EnableTrack { kind }`.
**invoke name:** `"enable_track"`.

---

### `disable_track(state, server_id, kind) -> Result<(), String>`

**ServerRequest:** `DisableTrack { kind }`.
**invoke name:** `"disable_track"`.

---

### `set_deafen(state, server_id, deafened) -> Result<(), String>`

**What it does:** updates the deafen state on the server for the current stream
session (distinct from the local `voice_set_deafen` which mutes playback).
**ServerRequest:** `SetDeafen { deafened }`.
**invoke name:** `"set_deafen"`.

---

### `offer_stream_key(state, server_id, kind, wrapped_keys) -> Result<(), String>`

**What it does:** distributes the per-call stream encryption key, wrapped
separately for each peer's public key (forward-secure E2EE for voice).
`wrapped_keys` is a list of `(pubkey_bytes: [u8;32], wrapped_key: Vec<u8>)`
pairs.
**ServerRequest:** `OfferStreamKey { kind, wrapped_keys }`.
**invoke name:** `"offer_stream_key"`.

---

## Group 14 — Voice engine (local audio pipeline)

These commands wrap `VoiceController`, the single global audio pipeline
instance managed by Tauri state (`Arc<VoiceController>`). They do NOT
communicate with the server directly; the controller emits `voice://*` events
back to the UI.

---

### `voice_join(voice, state, server_id, channel_id) -> Result<(), String>`

**What it does:** the main entry point for starting a voice call. Looks up the
`ServerConnection` from `AppState::servers`, builds a `QuinnServerSession`
adapter (which holds the QUIC connection for sending stream datagrams), reads
the saved voice mode and per-peer volumes, then calls
`VoiceController::join_with_config`. This opens the encoder/decoder pipeline,
starts the send and recv tasks, and begins transmitting if in `OpenMic` mode.
**Side effects:** spawns audio I/O threads; mutates `VoiceController` state;
emits `voice://*` events.
**Connects to:** `voice_bridge::QuinnServerSession`; `VoiceController::join_with_config`.
**invoke name:** `"voice_join"` → `voiceJoin()`.

---

### `voice_leave(voice) -> Result<(), String>`

**What it does:** tears down the audio pipeline, stops send/recv tasks, and
releases audio devices.
**Side effects:** mutates `VoiceController` state; emits `voice://left` event.
**invoke name:** `"voice_leave"` → `voiceLeave()`.

---

### `voice_set_mute(voice, muted) -> Result<(), String>`

**What it does:** mutes or unmutes the local microphone (stops sending encoded
audio when muted). Does not affect deafen.
**Side effects:** mutates `VoiceController` mute state; emits `voice://mute_changed`.
**invoke name:** `"voice_set_mute"` → `voiceSetMute()`.

---

### `voice_set_deafen(voice, deafened) -> Result<(), String>`

**What it does:** deafens or undeafens local playback (silences all received
audio). Also mutes send when deafened.
**Side effects:** mutates `VoiceController` deafen state; emits `voice://deafen_changed`.
**invoke name:** `"voice_set_deafen"` → `voiceSetDeafen()`.

---

### `voice_get_state(voice) -> Result<VoiceState, String>`

**What it does:** returns a snapshot of the current audio pipeline state:
active channel id, muted/deafened flags, transmit-active flag, and the list of
known peers with their speaking/muted/deafened indicators.
**Returns:** `VoiceState { channel_id, muted, deafened, transmitting, peers }`.
**invoke name:** `"voice_get_state"` → `voiceGetState()`.

---

### `voice_toggle_transmit(voice) -> Result<bool, String>`

**What it does:** toggles push-to-talk transmit state; returns the new transmit
bool. Only meaningful in `PushToTalk` mode.
**invoke name:** `"voice_toggle_transmit"` → `voiceToggleTransmit()`.

---

### `voice_set_peer_volume(voice, pubkey_hex, volume) -> Result<(), String>`

**What it does:** sets the playback volume multiplier for a specific peer
(clamped to `[0.0, 2.0]`) and persists the value to `settings.json` so it
survives reconnects.
**Side effects:** calls `VoiceController::set_peer_volume`; writes `peer_volumes`
to `settings.json`.
**invoke name:** `"voice_set_peer_volume"` → `voiceSetPeerVolume()`.

---

## Group 15 — Voice settings (mic mode, PTT key, per-peer volumes)

Purely local settings stored under `~/.farder/settings.json`. Read back at
`voice_join` time to configure the `VoiceController`.

---

### `get_voice_mode() -> String` / `set_voice_mode(mode) -> Result<(), String>`

**What it does:** reads/writes the `"voice_mode"` key. Accepted values:
`"OpenMic"` (default) and `"PushToTalk"`. Any unknown value passed to
`set_voice_mode` is normalized to `"OpenMic"`.
**invoke names:** `"get_voice_mode"` / `"set_voice_mode"` → `getVoiceMode()` / `setVoiceMode()`.

---

### `get_ptt_key() -> String` / `set_ptt_key(key) -> Result<(), String>`

**What it does:** reads/writes the `"ptt_key"` key (a `KeyboardEvent.code` string,
e.g. `"Backquote"`). Default: `"Backquote"`.
**invoke names:** `"get_ptt_key"` / `"set_ptt_key"` → `getPttKey()` / `setPttKey()`.

---

### `get_peer_volumes() -> HashMap<String, f32>`

**What it does:** returns the full `peer_volumes` map (pubkey-hex → volume
multiplier). The map is loaded at call time from `settings.json`.
**invoke name:** `"get_peer_volumes"` → `getPeerVolumes()`.

### `get_voice_sensitivity() -> u32` / `set_voice_sensitivity(voice, value) -> Result<(), String>`

**What it does:** mic sensitivity (0-100, higher = more sensitive; default 85),
persisted as `voice_sensitivity`. `set_voice_sensitivity` persists it AND applies
it live to the active call's send task via `VoiceController::set_speak_threshold`
(mapping sensitivity to the speaking RMS threshold). `voice_join` applies the
saved value at join. Drives the live mic meter in Voice settings.
**invoke names:** `"get_voice_sensitivity"` / `"set_voice_sensitivity"` →
`getVoiceSensitivity()` / `setVoiceSensitivity()`.

---

## Group 16 — Audio device selection

---

### `list_input_devices() -> Result<Vec<AudioDeviceInfo>, String>`

**What it does:** enumerates all cpal audio input devices on the host. Returns
a list of `{ name: String, is_default: bool }`. The name can be passed to
`set_input_device` to persist the selection.
**invoke name:** `"list_input_devices"` -> `listInputDevices()`.

---

### `list_output_devices() -> Result<Vec<AudioDeviceInfo>, String>`

Same as `list_input_devices` but for output (playback) devices.
**invoke name:** `"list_output_devices"` -> `listOutputDevices()`.

---

### `get_input_device() -> Option<String>` / `set_input_device(name: Option<String>) -> Result<(), String>`

**What it does:** reads/writes the `"input_device"` key in `settings.json`.
`None` / absent = system default. The saved name is consumed by
`start_recording` (voice-message capture), `voice_join` (live call capture),
and the `Test Mic` flow.
**Side effects (set):** overwrites `settings.json`; removes the key when `name` is
`null` / `None` to restore system-default behaviour.
**invoke names:** `"get_input_device"` / `"set_input_device"` ->
`getInputDevice()` / `setInputDevice()`.

---

### `get_output_device() -> Option<String>` / `set_output_device(name: Option<String>) -> Result<(), String>`

Same pattern as `get/set_input_device` but for the `"output_device"` key.
Consumed by `play_audio_file` (Test Mic playback) and `voice_join`
(live call mixer playback).
**invoke names:** `"get_output_device"` / `"set_output_device"` ->
`getOutputDevice()` / `setOutputDevice()`.

---

## Group 17 — Recording

---

### `start_recording() -> Result<u64, String>`

**What it does:** opens the saved input device (falling back to system default)
via `cpal`, creates a WAV file in the OS temp directory (filename
`farder_voice_<ms>.wav`), and streams 16-bit samples into it. The blocking
cpal stream runs in `spawn_blocking`. The command awaits a oneshot channel
that reports success or failure of the stream setup before returning, so a
missing audio device surfaces immediately rather than silently.
**Returns:** the new recording's **session id** once recording has started (not
when it ends). Pass it to `stop_recording` so only the owner can stop this
recording (protects against stale stops from React StrictMode's dev
double-mount or late async cleanups).
**Side effects:** writes to a temp WAV file; acquires the audio input device.
Returns `Err("already recording")` if a recording is already live (the start
guard is a `compare_exchange`, so two concurrent starts cannot both win).
**invoke name:** `"start_recording"` -> `startRecording(): Promise<number>`.

---

### `stop_recording(session: Option<u64>) -> Result<String, String>`

**What it does:** stops a recording. With `Some(id)`, only stops the matching
session — a stale id returns `Err("stale recording session")` WITHOUT touching
a newer live recording (this is what makes stray/late stops harmless). With
`None`, stops whatever is recording (wedge recovery). The session check and the
path claim happen ATOMICALLY under one lock (check-then-sleep-then-take let a
stale stop steal a newer session's path). Only after claiming does it set the
`RECORDING` atomic to false (signals the cpal thread to stop), wait 500 ms for
WAV finalization, and return the path to the WAV file.
**Returns:** absolute path to the WAV file.
**Side effects:** finalizes the WAV; releases the audio device.
**invoke name:** `"stop_recording"` → `stopRecording(session?: number)`.

---

### `save_temp_audio(data) -> Result<String, String>`

**What it does:** decodes a base64 audio blob (e.g., from the browser's
`MediaRecorder`) and writes it to a temp file at `farder_voice_<ms>.webm`.
Returns the file path. Used as an alternative to `start_recording` when the
frontend captures audio itself.
**invoke name:** `"save_temp_audio"` → `saveTempAudio()`.

### `play_audio_file(path) -> Result<(), String>`

**What it does:** decodes a WAV file and plays it on the saved output device
(falling back to system default) via a cpal output stream. Runs in
`spawn_blocking`; the command returns once playback completes.
**Side effects:** acquires the audio output device for the duration.
Used by the `Test Mic` flow to play back the just-recorded WAV.
**invoke name:** `"play_audio_file"` -> `playAudioFile()`.

---

## Group 17b — Invite preview

---

### `get_invite_preview(link) -> Result<InvitePreviewResult, String>`

**What it does:** fetches an invite preview through the relay that owns the
link (for relay links) or through the build-configured default relay (for direct
links), without touching any session connection. The command is anonymous —
no identity is required and the caller's IP never reaches the target server.

**Parameters:**
- `link` — any Farder invite link form: a relay URL
  (`farder://relay/<server_id_hex>/<code>`), a direct deep link
  (`farder://<host:port>/<code>`), or a bare `<host:port>/<code>` string.
  Setup-token links and bare address links (no invite code segment) return
  `status: "none"` immediately.

**Returns:** `InvitePreviewResult { status, server_name, member_count, online_count }`.
`status` is one of:
- `"ok"` — valid code; `server_name` (truncated to 80 chars), `member_count`,
  `online_count` are populated.
- `"invalid"` — the relay confirmed the code is invalid, expired, or exhausted.
- `"unavailable"` — the relay timed out, the server was unreachable, or the
  relay refused the request (rate-limited, SSRF-blocked, etc.).
- `"none"` — the link carries no invite code (setup token, bare address, or
  unrecognised format).

**Relay-selection rule:**
- Relay links (`farder://relay/...`) use the relay embedded in the link.
- Direct links use the build-configured default relay
  (`crate::default_relay::default_relay()`). If no default relay is configured
  in this build, the result is `"none"`.

**Cache:** results are cached for 60 s in a session-scoped static
`PREVIEW_CACHE` (keyed by the raw link string). This mirrors the relay's own
60 s TTL, so a cached relay answer is double-cached and the relay is never hit
more than once per minute per distinct link.

**No identity needed:** the command opens a throwaway QUIC connection to the
relay, sends `ProxyInvitePreview`, reads `ProxyInvitePreviewResult`, and closes
the connection. It never touches `AppState::servers` or any authenticated
session.

**Timeout:** 8 s client-side budget (the relay's own budget is 5 s, so the
relay's result always arrives before this fires under normal conditions).

**Side effects:** none beyond the throwaway network connection and the session
cache write.

**invoke name:** `"get_invite_preview"` → `getInvitePreview(link)`.

**Seam note:** `parse_direct_invite(link)` in `client/src-tauri/src/connection.rs`
parses `<host:port>/<code>` and `farder://<host:port>/<code>` forms; it rejects
setup-token segments and multi-segment relay-style paths. `parse_relay_target`
handles relay links.

---

## Group 18 — Invites and account deletion

---

### `create_invite(state, server_id, max_uses) -> Result<InviteResult, String>`

**What it does:** creates a server invite. Builds two shareable URLs from the
returned invite code:
- `link`: `https://farder.gg/join/<base64url(address/code)>`
- `deep_link`: `farder://<address>/<code>`

**ServerRequest:** `CreateInvite { max_uses, expires_in_secs: None, target_channel: None }`.
**invoke name:** `"create_invite"` → `createInvite()`.

---

### `request_deletion(state, server_id) -> Result<(), String>`

**What it does:** starts a 30-day account deletion grace period on the server.
**ServerRequest:** `RequestDeletion`.
**invoke name:** `"request_deletion"` → `requestDeletion()`.

---

### `cancel_deletion(state, server_id) -> Result<(), String>`

**ServerRequest:** `CancelDeletion`.
**invoke name:** `"cancel_deletion"` → `cancelDeletion()`.

---

### `get_deletion_status(state, server_id) -> Result<DeletionStatusResult, String>`

**Returns:** `{ pending: bool, requested_at?: u64, expires_at?: u64 }`.
**ServerRequest:** `GetDeletionStatus`.
**invoke name:** `"get_deletion_status"` → `getDeletionStatus()`.

---

## Group 19 — Notifications

---

### `show_notification(title, body) -> Result<(), String>`

**What it does:** spawns a native OS desktop notification. Uses `notify-send`
on Linux, a PowerShell toast on Windows, and `osascript` on macOS. Fully
fire-and-forget — subprocess spawn errors are silently discarded.
**Side effects:** spawns a child process.
**invoke name:** `"show_notification"` → `showNotification()`.

---

### `get_notification_prefs() -> Result<Value, String>` / `save_notification_prefs(prefs) -> Result<(), String>`

**What it does:** reads/writes `~/.farder/notifications.json`. Default prefs
are inline (DM notifications: `"all"`, mentions: `true`, etc.).
**invoke names:** `"get_notification_prefs"` / `"save_notification_prefs"` →
`getNotificationPrefs()` / `saveNotificationPrefs()`.

---

## Group 19 — Themes (`themes.rs`)

All commands read from built-in themes (CSS embedded at compile time via
`include_str!`) or from user themes in `~/.farder/themes/`. Active theme
preference is stored in `settings.json` under `"active_theme"`.

---

### `list_themes() -> Vec<ThemeMeta>`

**What it does:** returns metadata for all built-in and user themes.
**invoke name:** `"list_themes"` → `listThemes()`.

---

### `load_theme_css(id) -> Result<String, String>`

**What it does:** returns the full CSS text for the given theme id.
**invoke name:** `"load_theme_css"` → `loadThemeCss()`.

---

### `get_active_theme() -> ActiveTheme` / `set_active_theme(id) -> Result<(), String>`

**What it does:** reads/writes the `"active_theme"` key in `settings.json`.
`get_active_theme` also returns the CSS for immediate application.
**invoke names:** `"get_active_theme"` / `"set_active_theme"` → `getActiveTheme()` / `setActiveTheme()`.

---

### `fork_theme(base_id, new_id, name) -> Result<String, String>`

**What it does:** copies the CSS of `base_id` to a new user theme at
`~/.farder/themes/<new_id>/`, writes `theme.json`, and returns the CSS.
**invoke name:** `"fork_theme"` → `forkTheme()`.

---

### `save_user_theme(id, css) -> Result<(), String>`

**What it does:** overwrites the CSS file of user theme `id`. Fails if `id`
belongs to a built-in theme.
**invoke name:** `"save_user_theme"` → `saveUserTheme()`.

---

### `add_theme_asset(theme_id, source_path, target_filename) -> Result<String, String>`

**What it does:** copies a file into a user theme's asset directory and returns
a relative `url(...)` string suitable for use in CSS.
**invoke name:** `"add_theme_asset"` → `addThemeAsset()`.

---

### `delete_user_theme(id) -> Result<(), String>` / `rename_user_theme(id, new_name) -> Result<(), String>`

**invoke names:** `"delete_user_theme"` / `"rename_user_theme"` →
`deleteUserTheme()` / `renameUserTheme()`.

---

### `get_theme_order() -> Vec<String>` / `set_theme_order(ids) -> Result<(), String>`

**What it does:** reads/writes the `"theme_order"` key (a JSON array of theme
ids) in `settings.json`, used to let users drag-reorder the theme list.
**invoke names:** `"get_theme_order"` / `"set_theme_order"` → `getThemeOrder()` / `setThemeOrder()`.

---

### `open_themes_folder() -> Result<(), String>`

**What it does:** opens `~/.farder/themes/` in the OS file manager.
**invoke name:** `"open_themes_folder"` → `openThemesFolder()`.

---

## Group 20 — GIF search / Tenor (`tenor.rs`)

---

### `tenor_search(query, limit, pos) -> Result<TenorSearchResult, String>`

**What it does:** queries the Tenor v2 API with the stored or default API key.
`pos` is the pagination cursor from a previous result's `next` field.
**Side effects:** outbound HTTPS request from the Tauri process (NOT proxied
through the server, unlike `fetch_url`).
**invoke name:** `"tenor_search"`. (No TS wrapper in `tauri-bridge.ts` — called
directly by the GIF picker component.)

---

### `tenor_trending(limit) -> Result<TenorSearchResult, String>`

**What it does:** fetches Tenor trending GIFs.
**invoke name:** `"tenor_trending"`.

---

### `get_gif_search_settings() -> GifSearchSettings` / `set_gif_search_settings(settings) -> Result<(), String>`

**What it does:** reads/writes GIF search settings from `settings.json` keys
`gif_search_enabled`, `gif_search_content_filter`, and `gif_search_user_key`.
**invoke names:** `"get_gif_search_settings"` / `"set_gif_search_settings"` →
`getGifSearchSettings()` / `setGifSearchSettings()`.

---

## Group 21 — Translation (`translation.rs`)

Translation is powered by locally-downloaded Bergamot/OPUS models.

---

### `get_translation_settings() -> TranslationSettings` / `set_translation_settings(settings) -> Result<(), String>`

**What it does:** reads/writes translation settings from `settings.json` (keys:
`translation_enabled`, `translation_default_target`, `translation_seen_first_run`,
`translation_user_overrides`).
**invoke names:** `"get_translation_settings"` / `"set_translation_settings"` →
`getTranslationSettings()` / `setTranslationSettings()`.

---

### `list_available_pairs() -> Vec<LangPair>`

**What it does:** returns the hard-coded list of downloadable language-pair models.
**invoke name:** `"list_available_pairs"`.

---

### `download_model(pair) -> Result<(), String>`

**What it does:** downloads model, vocab, and lexical shortlist files from the
Bergamot CDN for the given `{ src, trg }` language pair into
`~/.farder/translation-models/<src>-<trg>/`. Language code is validated (2–8
lowercase ASCII chars only).
**Side effects:** network I/O; writes model files to disk.
**invoke name:** `"download_model"`.

---

### `list_local_models() -> Vec<LocalModel>`

**What it does:** scans `~/.farder/translation-models/` and returns metadata
for any fully-downloaded model pairs.
**invoke name:** `"list_local_models"`.

---

### `delete_model(pair) -> Result<(), String>`

**What it does:** removes the model directory for `pair`.
**Side effects:** disk delete.
**invoke name:** `"delete_model"`.

---

### `get_model_paths(pair) -> Result<ModelPaths, String>`

**What it does:** returns the absolute file paths for model, vocab (or
split src/trg vocabs), and lex files for `pair`. Needed by the JS-side
translation worker to load the model from disk.
**Returns:** `ModelPaths { model, vocab?, src_vocab?, trg_vocab?, lex }`.
**invoke name:** `"get_model_paths"`.

---

## Group 22 — Book (sticker/emoji library) (`book.rs`)

The Book is a local image library (PNG/JPG/GIF/WebP, max 2 MB total) that
lives in `~/.farder/book/`. Items have a `server_files` map so that when an
item is used as a reaction on a specific server it can be re-used by `file_id`
without re-uploading.

---

### `book_list_items() -> Result<Vec<BookItem>, String>`

**What it does:** reads `~/.farder/book/items.json` and returns all items.
**invoke name:** `"book_list_items"`.

---

### `book_upload_item(file_path, name) -> Result<BookItem, String>`

**What it does:** copies the file into `~/.farder/book/files/` (enforces
allowed extensions and 2 MB cap), detects animation, records dimensions, and
appends the item to `items.json`.
**Side effects:** disk write; enforces size quota (rejects if over `MAX_BOOK_BYTES`).
**invoke name:** `"book_upload_item"`.

---

### `book_delete_item(id) -> Result<(), String>`

**What it does:** removes the item from `items.json` and deletes its file from
`~/.farder/book/files/`.
**Side effects:** disk delete.
**invoke name:** `"book_delete_item"`.

---

### `book_rename_item(id, name) -> Result<BookItem, String>`

**What it does:** updates the `name` field of the item in `items.json`.
**invoke name:** `"book_rename_item"`.

---

### `book_get_file_for_server(state, id, server_id) -> Result<u64, String>`

**What it does:** returns the cached `file_id` for this item on `server_id`, or
uploads the file to the server (via `upload_file_internal`) and caches the
returned `file_id` in `server_files` for future calls.
**Side effects:** may upload a file; writes updated `items.json` to disk.
**Connects to:** `upload_file_internal` in `commands.rs`.
**invoke name:** `"book_get_file_for_server"`.

---

### `book_migrate_legacy_favorites() -> Result<Vec<BookItem>, String>`

**What it does:** one-time migration — reads the old `favorites.json` index,
copies each image into the book, and returns the migrated items. Safe to call
repeatedly (already-migrated items are skipped by hash).
**invoke name:** `"book_migrate_legacy_favorites"`.

---

### `book_save_from_url(url, name) -> Result<BookItem, String>`

**What it does:** fetches an image URL via `reqwest`, validates extension and
size, saves it into the book, and returns the `BookItem`.
**Side effects:** outbound HTTP(S) request; disk write.
**invoke name:** `"book_save_from_url"`.

---

### `book_item_absolute_path(id) -> Result<String, String>`

**What it does:** returns the absolute filesystem path for a book item's image
file, for use in contexts where a file path is needed rather than a data URL.
**invoke name:** `"book_item_absolute_path"`.

---

## Group 23 — Local server management

These commands spawn and manage `farder-server` child processes on the local
machine. A "local server" is a full server process managed by the Tauri app.

---

### `create_local_server(app, state, procs, name, template, privacy, icon_path, relay_mode, relay_addr, relay_fp) -> Result<Value, String>`

**What it does:** spawns a `farder-server` child process using `server_manager`,
connects as owner (no invite needed -- first connection auto-claims ownership on
a fresh server), optionally saves a server avatar, fetches initial server info,
and returns the full connect result plus the assigned address.  Supports two
connectivity modes selected by `relay_mode`:
- `"direct"` -- direct local bind (legacy behaviour); polls for readiness up to 5 s.
- `"farder"` -- uses the built-in Farder default relay; retries the relay
  connection for up to 30 s while the server registers.
- `"selfhost"` -- connects through a caller-supplied relay at `relay_addr` with
  certificate fingerprint `relay_fp` (64 hex chars / 32 bytes).

**Parameters:**
- `template` -- one of `"blank"`, `"friend-group"`, `"gaming-community"`, `"organization"`, `"public-community"`.
- `privacy` -- passed through to `spawn_server`.
- `icon_path` -- optional local image path for the server avatar.
- `relay_mode` -- `"direct"` | `"farder"` | `"selfhost"`.
- `relay_addr` -- host:port of the relay (self-host mode only; ignored otherwise).
- `relay_fp` -- 64-char hex certificate fingerprint (self-host mode only).

**Returns:** `{ address, server_name, member_count, channels, categories, roles, owner_public_key, relayed }`.
`address` is `"127.0.0.1:<port>"` for direct or a `farder://` relay link for relayed servers.
**Side effects:** spawns a child OS process; establishes a connection; writes
`servers.json`; may write a server avatar file.
**Gotcha:** duplicate name check (same name as existing local server) returns an
error to prevent two processes from sharing one SQLite database.
**invoke name:** `"create_local_server"` → `createLocalServer()`.

---

### `stop_local_server(procs, port) -> Result<(), String>`

**What it does:** signals and waits for the server process on `port` to stop.
**invoke name:** `"stop_local_server"` → `stopLocalServer()`.

---

### `get_local_servers(procs) -> Vec<ManagedServer>`

**What it does:** returns all currently running locally-managed server processes.
**invoke name:** `"get_local_servers"` → `getLocalServers()`.

---

### `list_templates() -> Vec<Value>`

**What it does:** returns the hard-coded list of available server templates.
No disk or network I/O.
**invoke name:** `"list_templates"` → `listTemplates()`.

---

### `restart_local_servers(procs) -> Vec<ServerEntry>`

**What it does:** called at app startup. Reads `servers.json`, kills any orphan
`farder-server` processes whose `--db` flag matches each local server's data
directory (Unix only, via `pgrep -af`), respawns each local server, and
updates `servers.json` with new ports (ports may differ on restart). Remote
servers are passed through unchanged.
**Side effects:** may `kill -9` orphan OS processes; spawns child processes;
rewrites `servers.json`.
**invoke name:** `"restart_local_servers"` → `restartLocalServers()`.

---

## Group 24 — Screenshare preview (`screenshare.rs`)

These commands start and stop the Phase B local capture→encode→emit loopback.
No server connection is needed. See `docs/modules/screenshare-capture-codec.md`
for the full design reference.

---

### `start_screenshare_preview(app, fps, max_width, max_height) -> Result<(), String>`

**What it does:** starts a local screen-capture and H.264 encode loop. Picks the
first available display source from the platform backend (Windows Graphics
Capture on Windows; mock gradient frames on other platforms). Spawns a dedicated
thread that constructs an `H264Encoder` (which is `!Send` and must stay on its
own thread), then continuously captures frames, encodes them to Annex-B H.264,
and emits each as a `screenshare:frame` Tauri event with a base64 payload.
Forces a keyframe at loop start so a fresh WebCodecs decoder can begin decoding
immediately.

**Parameters:**
- `fps` — requested frame rate; passed to `DisplayFormat` (advisory for the WGC backend).
- `max_width`, `max_height` — advisory max frame dimensions; WGC captures at native resolution in Phase B; downscaling is Phase C.

**Returns:** `Ok(())` once the capture and encode thread are running.
**Errors:**
- `"a screenshare preview is already running"` — only one preview at a time.
- Encoder init failure (pre-flight `H264Encoder::new()` check).
- Backend `start_capture` failure (no capture sources, bad format, WGC API error).

**Side effects:** calls `make_display_backend()` and `backend.start_capture()`;
stores the `ActivePreview` in a process-global static slot; spawns one
`std::thread` (the encode loop); emits `screenshare:frame` events until stopped.

**Connects to:** `ScreensharePreview.tsx` via the `screenshare:frame` event.
**invoke name:** `"start_screenshare_preview"` → `startScreensharePreview(fps, maxWidth, maxHeight)`.

---

### `stop_screenshare_preview() -> Result<(), String>`

**What it does:** stops the active preview. Sets the encode loop's stop flag
(causing `run_encode_loop` to exit on its next iteration) and calls
`backend.stop_capture()` (which tears down the WGC session or joins the mock
generator thread). Idempotent — calling when no preview is running is a no-op.

**Returns:** `Ok(())` always (unless the internal mutex is poisoned).
**Side effects:** takes `ActivePreview` from the static slot; sets the `AtomicBool`
stop flag; calls `backend.stop_capture()`. After this returns, the encode thread
will exit and no further `screenshare:frame` events will be emitted.

**invoke name:** `"stop_screenshare_preview"` → `stopScreensharePreview()`.

---

## State it owns

| Field / variable | Type | What it tracks, when it's mutated |
|---|---|---|
| `AppState::signing_key_bytes` | `Mutex<Option<[u8;32]>>` | Loaded signing key; mutated by `create_identity` / `unlock_identity` / `migrate_plaintext_identity` / `restore_identity` |
| `AppState::servers` | `Mutex<HashMap<String, Arc<ServerConnection>>>` | Live server connections; mutated by `connect_server` / `disconnect_server` / `create_local_server` |
| `RECORDING` | `AtomicBool` (static) | Whether a recording is in progress |
| `RECORDING_PATH` | `Mutex<Option<String>>` (static) | Path to the in-progress WAV file |
| `~/.farder/identity.key` | disk | 32-byte Ed25519 signing key |
| `~/.farder/profile.json` | disk | Display name, bio, banner color |
| `~/.farder/avatar.png` | disk | Local user avatar |
| `~/.farder/settings.json` | disk | All app settings (voice mode, last server, PTT key, theme, etc.) |
| `~/.farder/servers.json` | disk | Persisted server list + local server configs |
| `~/.farder/favorites.json` | disk | Favorites index |
| `~/.farder/notifications.json` | disk | Notification preferences |
| `~/.farder/book/items.json` | disk | Book item index |
| `~/.farder/profile_overrides/<safe_server_id>.img` | disk | Per-server avatar override (raw image bytes); written by `set_server_avatar_override`, cleared by `clear_server_avatar_override` |
| `~/.farder/profile_cache/<hash>` | disk | Verified signed-profile blobs keyed by SHA-256 hash; written by `get_member_profile` on a network fetch; corrupt entries auto-deleted |
| `~/.farder/pushed_profiles.json` | disk | Map of `server_id → last successfully pushed profile hash`; owned by `profile_sync.rs` |
| `ACTIVE` (screenshare) | `OnceLock<Mutex<Option<ActivePreview>>>` (static in `screenshare.rs`) | The single active preview (stop flag + backend); set by `start_screenshare_preview`, cleared by `stop_screenshare_preview` |

## Integration map

- **`bridge.rs`** — `send_request` is called by every server-facing command;
  `spawn_event_reader` is called by `connect_server` and `create_local_server`.
- **`voice/mod.rs`** (`VoiceController`) — called directly by `voice_join`,
  `voice_leave`, `voice_set_mute`, `voice_set_deafen`, `voice_get_state`,
  `voice_toggle_transmit`, `voice_set_peer_volume`.
- **`voice_bridge.rs`** (`QuinnServerSession`) — constructed inside `voice_join`
  to give the controller a handle to send stream datagrams.
- **`server_manager.rs`** — called by `create_local_server`, `stop_local_server`,
  `get_local_servers`, `restart_local_servers`.
- **`farder_crypto`** — used by `dm_encrypt`/`dm_decrypt`, by the
  `identity.rs` `IdentityStore` commands (Argon2id + AES-256-GCM, BIP39
  recovery), and by `connect_and_authenticate` inside `connect_server`.
- **`profile_sync.rs`** — called by `set_avatar`, `set_profile_status`,
  `set_server_avatar_override`, `clear_server_avatar_override`, and
  `get_member_profile`. Owns the effective-profile logic (override priority,
  `pushed_profiles.json`, avatar validation, signed-profile build and push).
- **`tauri-bridge.ts`** — every command's typed TypeScript wrapper; the
  `invoke("X")` strings here must match the Rust function names and the
  `generate_handler!` entries in `main.rs`.
- **`screenshare.rs`** (`DisplayBackend`, `H264Encoder`, `run_encode_loop`) — called by
  `start_screenshare_preview` / `stop_screenshare_preview`; see
  `docs/modules/screenshare-capture-codec.md`.

## Known gotchas

- **Duplicate voice-join commands:** both `join_voice`/`leave_voice`/`get_voice_state`
  and `join_channel_media`/`leave_channel_media`/`get_media_state` send the same
  `ServerRequest` variants. They are both registered in `generate_handler!`. If
  you add roster-join logic, update both or clearly deprecate one set.
- **`set_category` encoding:** `update_channel` uses a two-value flag pair to
  distinguish "don't touch category" from "remove from category". This is
  invisible to the TS caller because `updateChannel()` in `tauri-bridge.ts`
  computes `setCategory` automatically. Do not pass `setCategory` manually from
  new call sites.
- **Tenor API key:** `tenor.rs` has `const TENOR_DEFAULT_KEY: &str = "REPLACE_WITH_REAL_KEY"`. This is intentionally left as a placeholder for local development; ship with a real key or users cannot search GIFs.
- **File upload uses its own QUIC stream:** `upload_file` and `add_favorite` open
  a fresh bi-directional QUIC stream instead of using the `bridge::send_request`
  path. Do not confuse the two code paths when debugging transfer failures.
- **`RECORDING` is process-global:** `start_recording` / `stop_recording` use a
  pair of `static` variables (`AtomicBool` + `Mutex<Option<String>>`). Only one
  recording can be active at a time across the whole app; calling `start_recording`
  twice returns an error.
- **`restart_local_servers` rewrites ports:** local servers get a new port on
  each restart. Any in-flight `server_id` strings (which are `"ip:port"`) from a
  previous session become stale after this call.
- **Public-key format in member/moderation commands:** `parse_public_key` strips
  a `"vk_"` prefix if present. Always pass keys in `"vk_<hex>"` format from the
  frontend (matching the form stored in `VoiceState::peers` and `MemberInfo`).
  Passing a raw JSON `{bytes}` object will silently fail the hex decode.
