# Farder Phase 2: Servers & Text — Design Spec

**Date:** 2026-04-02
**Status:** Draft
**Parent Spec:** `docs/specs/2026-04-01-privacy-chat-platform-design.md`

## Goal

Build the community server binary — the thing people run to host a Farder server. A single Rust binary that provides channels, categories, roles, permissions, real-time text messaging, invite links, and server templates. Two or more users should be able to connect, join channels, and chat in real time with full permission enforcement.

## Architecture

### New Crate: farder-server

A single Rust binary added to the workspace. Internal modules:

| Module | Responsibility |
|--------|---------------|
| auth | Ed25519 signature verification, session tokens, member registration |
| permissions | Role CRUD, bitfield permission storage, resolution algorithm, server-side response filtering |
| channels | Channel and category CRUD, settings (retention, slow mode, NSFW, topic) |
| messages | Message storage/retrieval (SQLite), FTS5 search, real-time delivery |
| invites | Invite link generation, validation, expiry, usage tracking |
| templates | TOML-based server templates, loaded from filesystem or embedded defaults |

### Transport

QUIC (via Quinn), same as Phase 1. The parent spec mentions "WebSocket" for message delivery — this is superseded for Phase 2. QUIC is used for all server-client communication because the desktop client (Tauri) supports it natively and it's already proven in Phase 1. WebSocket support for browser clients will be added in Phase 5 as a compatibility layer. The server code is transport-agnostic (it operates on streams, not protocols), so adding WebSocket later requires no server changes.

Clients connect directly to the server for Phase 2 (relay routing deferred — wiring through the relay is a deployment change, not a code change). One persistent bi-directional stream per client for request/response and server-pushed events.

### Storage

SQLite via rusqlite. Single database file. FTS5 extension for full-text message search. Tables:

- `members` — public key, display name, avatar, join timestamp, ban status
- `roles` — id, name, permissions (u64 bitfield), color, position
- `member_roles` — maps members to roles (many-to-many)
- `categories` — id, name, position
- `channels` — id, name, type, category_id, position, settings (JSON)
- `channel_overrides` — channel_id, role_id, allow (u64), deny (u64)
- `category_overrides` — category_id, role_id, allow (u64), deny (u64)
- `messages` — id, channel_id, author, content, timestamp, edited_at, reply_to, pinned
- `messages_fts` — FTS5 virtual table indexing message content
- `invites` — code, created_by, max_uses, use_count, expires_at, target_channel

## Authentication & Membership

### Server Bootstrapping (First Run)

On first run, the server generates a one-time **setup token** (random 32 bytes, displayed in the server console). The first user to connect with this setup token becomes the Owner. The setup token is invalidated after use and never stored. After the owner is established, all subsequent joins require invite codes.

### Connection Flow

1. Client opens QUIC connection to server.
2. Server sends a random challenge (32 bytes) on the bi-stream.
3. Client signs the challenge with their Ed25519 private key.
4. Client sends: signed challenge + public key + invite code (if first time) or setup token (if claiming ownership).
5. Server verifies signature against the public key.
6. If setup token provided and no owner exists: register as Owner with all permissions.
7. If new user: validates invite code, registers member with @everyone role.
8. If existing member: looks up member by public key, verifies not banned.
9. Server issues a session token (random 32 bytes, valid for 24h).
10. Client includes session token in subsequent requests (avoids re-signing).

### Member Data

- Public key (primary identity)
- Display name and avatar (from signed profile, updated on connect)
- Assigned roles
- Join timestamp
- Ban status (banned keys cannot reconnect)

### Key Revocation

If a revocation notice arrives for a public key, the server invalidates that key's active session and marks the member record as revoked. Revoked keys cannot authenticate.

## Channels & Categories

### Categories

- Name, position (sort order)
- Permission overrides (apply to all channels within unless overridden)
- "Uncategorized" is a virtual category for channels not assigned to any category

### Channel Types (Phase 2)

| Type | Description |
|------|-------------|
| Text | Messages, replies, pins, search |
| Announcement | Only roles with SEND_MESSAGES + MANAGE_CHANNEL can post, everyone else reads |

Voice, Voice+Text, and Stage channel types are deferred to Phase 4.

### Per-Channel Settings

- **Permission overrides** — three-state (inherit/allow/deny) per role per permission
- **Message retention** — auto-purge duration (never, 1h, 24h, 7d, 30d, 90d, 1y, or custom seconds)
- **Slow mode** — minimum seconds between messages per user (0 = disabled)
- **NSFW flag** — client-side content warning
- **Topic** — short description text displayed in channel header

### Channel Lifecycle

- **Creation:** name, type, optional category, optional position
- **Soft-delete:** marks as deleted, stops appearing in channel list, messages preserved
- **Hard-delete:** after configurable grace period (default 30 days) or immediately by owner. All messages and FTS entries purged.

## Permissions & Roles

### Permission Bitfield

Each permission is a bit in a u64:

| Bit | Permission | Description |
|-----|-----------|-------------|
| 0 | VIEW_CHANNEL | Can see the channel exists |
| 1 | READ_MESSAGES | Can read message history |
| 2 | SEND_MESSAGES | Can post messages |
| 3 | MANAGE_MESSAGES | Can delete/pin others' messages |
| 4 | CONNECT | Can join voice channels (Phase 4) |
| 5 | SPEAK | Can unmute in voice (Phase 4) |
| 6 | STREAM | Can screenshare/video (Phase 4) |
| 7 | MANAGE_CHANNEL | Can edit channel settings |
| 8 | MANAGE_ROLES | Can create/edit roles below their highest role |
| 9 | MANAGE_SERVER | Can edit server settings, categories |
| 10 | KICK_MEMBERS | Can kick members |
| 11 | BAN_MEMBERS | Can ban members (by public key) |
| 12 | ADMIN | All permissions, only grantable by Owner |
| 13 | CREATE_INVITES | Can generate invite links |

### Roles

- **Owner** — built-in, all permissions, cannot be restricted, cannot be deleted. Assigned to whoever created the server.
- **@everyone** — built-in, default role for all members. Sets baseline permissions.
- **Custom roles** — created by users with MANAGE_ROLES. Have a name, color (for display), position (hierarchy), and permission bitfield.

Role hierarchy: higher position = higher rank. Users can only manage roles below their own highest role's position.

### Channel/Category Overrides

Each override stores two u64 bitfields: `allow` and `deny`.

- Bit not in allow AND not in deny = **Inherit** (use role permissions)
- Bit in allow = **Allow** (grant even if role doesn't have it)
- Bit in deny = **Deny** (block even if role grants it; deny always wins)

### Permission Resolution Algorithm

```
fn resolve_permissions(member, channel) -> u64:
    // 1. Start with @everyone role
    perms = everyone_role.permissions

    // 2. OR in all other assigned roles
    for role in member.roles (excluding @everyone):
        perms |= role.permissions

    // 3. Apply category overrides (if channel is in a category)
    //    Union all allows and denies across all of the member's roles first,
    //    then apply once. This avoids order-dependent results.
    if channel.category:
        combined_allow = 0
        combined_deny = 0
        for override in category_overrides for member's roles:
            combined_allow |= override.allow
            combined_deny |= override.deny
        perms &= !combined_deny   // deny wins
        perms |= combined_allow

    // 4. Apply channel overrides (same union approach)
    combined_allow = 0
    combined_deny = 0
    for override in channel_overrides for member's roles:
        combined_allow |= override.allow
        combined_deny |= override.deny
    perms &= !combined_deny   // deny wins
    perms |= combined_allow

    // 5. Admin gets everything
    if perms & ADMIN:
        perms = ALL_PERMISSIONS

    // 6. Owner always gets everything
    if member is server owner:
        perms = ALL_PERMISSIONS

    return perms
```

### Server-Side Enforcement

Every handler filters its response through permission checks before serialization:

- **Channel list:** only channels where `VIEW_CHANNEL` is set
- **Message send:** requires `SEND_MESSAGES` for that channel
- **Message delete:** requires `MANAGE_MESSAGES` (others' messages) or own message
- **Message edit:** own messages only
- **Member list:** per-channel, only if requester has `VIEW_CHANNEL`
- **Role management:** requires `MANAGE_ROLES`, can only affect roles below own position
- **Channel management:** requires `MANAGE_CHANNEL`
- **Server settings:** requires `MANAGE_SERVER`
- **Kick/Ban:** requires respective permission, can only affect members with lower role position

No data for unauthorized resources is ever serialized or transmitted.

## Real-Time Messaging Protocol

### Message Structure

| Field | Type | Description |
|-------|------|-------------|
| id | u64 | Server-assigned, monotonically increasing per channel |
| channel_id | u64 | Which channel this message belongs to |
| author | PublicKey | Sender's public key |
| content | String | Markdown text |
| timestamp | u64 | Server-assigned Unix timestamp (seconds) |
| edited_at | Option<u64> | Null if never edited |
| reply_to | Option<u64> | Message ID being replied to, null if not a reply. If the replied-to message is deleted, this field is preserved as a dangling reference — the client displays "original message was deleted." |
| pinned | bool | Whether the message is pinned |

### Client Requests

Sent by the client on the persistent bi-stream:

| Request | Fields | Permission Required |
|---------|--------|-------------------|
| Authenticate | public_key, signed_challenge, invite_code? | None (pre-auth) |
| Subscribe | channel_ids: Vec<u64> | VIEW_CHANNEL per channel |
| SendMessage | channel_id, content, reply_to? | SEND_MESSAGES |
| EditMessage | message_id, new_content | Author only |
| DeleteMessage | message_id | Author or MANAGE_MESSAGES |
| FetchHistory | channel_id, before_id?, limit | READ_MESSAGES |
| PinMessage | message_id | MANAGE_MESSAGES |
| UnpinMessage | message_id | MANAGE_MESSAGES |
| Search | query, channel_id?, limit | READ_MESSAGES per channel |
| Typing | channel_id | SEND_MESSAGES (indicator expires after 8 seconds without renewal) |
| CreateChannel | name, type, category_id?, position? | MANAGE_CHANNEL |
| UpdateChannel | channel_id, settings | MANAGE_CHANNEL |
| DeleteChannel | channel_id | MANAGE_CHANNEL |
| CreateCategory | name, position? | MANAGE_SERVER |
| UpdateCategory | category_id, name?, position? | MANAGE_SERVER |
| DeleteCategory | category_id | MANAGE_SERVER |
| CreateRole | name, permissions, color?, position? | MANAGE_ROLES |
| UpdateRole | role_id, name?, permissions?, color?, position? | MANAGE_ROLES |
| DeleteRole | role_id | MANAGE_ROLES |
| AssignRole | member_key, role_id | MANAGE_ROLES |
| RemoveRole | member_key, role_id | MANAGE_ROLES |
| KickMember | member_key | KICK_MEMBERS |
| BanMember | member_key | BAN_MEMBERS |
| CreateInvite | max_uses?, expires_in?, target_channel? | CREATE_INVITES |
| GetServerInfo | | Any authenticated member |
| GetMembers | | Any authenticated member |
| SetChannelOverride | channel_id, role_id, allow, deny | MANAGE_CHANNEL |
| SetCategoryOverride | category_id, role_id, allow, deny | MANAGE_SERVER |

### Server Events

Pushed by the server on the persistent bi-stream:

| Event | Fields | Sent to |
|-------|--------|---------|
| NewMessage | full message struct | Members subscribed to that channel with READ_MESSAGES |
| MessageEdited | message_id, new_content, edited_at | Same as NewMessage |
| MessageDeleted | message_id, channel_id | Same as NewMessage |
| MessagePinned | message_id | Same as NewMessage |
| MessageUnpinned | message_id | Same as NewMessage |
| MemberJoined | public_key, profile | All connected members |
| MemberLeft | public_key | All connected members |
| MemberBanned | public_key | All connected members |
| TypingStarted | channel_id, public_key | Members subscribed to that channel |
| ChannelCreated | full channel struct | Members with VIEW_CHANNEL |
| ChannelUpdated | channel_id, changed fields | Members with VIEW_CHANNEL |
| ChannelDeleted | channel_id | Members who had VIEW_CHANNEL |
| CategoryCreated | full category struct | All connected members |
| CategoryUpdated | category_id, changed fields | All connected members |
| CategoryDeleted | category_id | All connected members |
| RoleCreated | full role struct | All connected members |
| RoleUpdated | role_id, changed fields | All connected members |
| RoleDeleted | role_id | All connected members |
| PermissionsChanged | | All connected members (tells client to re-fetch channel list) |

### Subscription Model

- Client sends `Subscribe { channel_ids }` to indicate which channels it wants live events for.
- Server only pushes NewMessage/Typing/etc. for subscribed channels.
- Client resubscribes when switching views.
- Structural events (channel/role/member changes) are always pushed regardless of subscription.

### Message Retention

A background task runs every hour (configurable):
1. Scans all channels with non-null retention policies.
2. Deletes messages older than the retention threshold.
3. Removes corresponding FTS5 entries.
4. Logs the purge count per channel.

## Invite System

### Invite Link Format

`farder://server-address/invite-code`

### Invite Properties

| Field | Type | Description |
|-------|------|-------------|
| code | String | Random 8-character alphanumeric |
| created_by | PublicKey | Who generated the invite |
| max_uses | Option<u32> | Null = unlimited |
| use_count | u32 | Current number of uses |
| expires_at | Option<u64> | Unix timestamp, null = never |
| target_channel | Option<u64> | Channel to land in after joining |

### Validation

1. Invite code exists in database.
2. Not expired (expires_at is null or in the future).
3. Use count < max_uses (or max_uses is null).
4. If valid: increment use_count, register member.
5. If invalid: reject with specific reason (expired, used up, not found).

## Server Templates

### Format

TOML files defining initial server configuration.

```toml
[template]
name = "Gaming Community"
description = "Voice lobbies, LFG, and game channels"

[[roles]]
name = "Admin"
permissions = 16383  # all bits set
color = "#FF0000"
position = 3

[[roles]]
name = "Moderator"
permissions = 2191  # view, read, send, manage_messages, manage_channel, kick, ban (bits 0-3,7,10,11)
color = "#00FF00"
position = 2

[[roles]]
name = "Member"
permissions = 8199  # view, read, send, create_invites
position = 1

[[categories]]
name = "General"

[[categories.channels]]
name = "welcome"
type = "announcement"

[[categories.channels]]
name = "chat"
type = "text"

[[categories]]
name = "Gaming"

[[categories.channels]]
name = "looking-for-group"
type = "text"

[[categories.channels]]
name = "game-night"
type = "text"
```

### Loading

1. Binary embeds 5 default templates via `include_str!`.
2. On startup, if a `templates/` directory exists next to the binary, load all `.toml` files from it.
3. Filesystem templates override embedded ones with the same name.
4. Users add custom templates by dropping `.toml` files in the `templates/` directory.
5. Template is applied once during initial server setup (first run).

### Built-in Templates

- **Gaming Community** — Admin, Mod, Member roles. General (#welcome, #chat), Gaming (#lfg, #game-night), Staff Only (hidden)
- **Friend Group** — No extra roles. #general, #media, #voice-lounge
- **Organization/Team** — Admin, Manager, Member. Departments as categories, #announcements
- **Public Community** — Verified role for posting. #rules (read-only), #general, #introductions
- **Blank** — Just #general text channel, @everyone with basic permissions

## What's NOT in Phase 2

- E2EE channels (Phase 3 — needs group key management)
- Voice/Stage channels (Phase 4)
- File attachments and embeds (Phase 3)
- Threads and reactions (Phase 3)
- Bulk attachment management (Phase 3)
- Data deletion rights (Phase 3)
- Relay routing (later — server code is transport-agnostic)
- WebSocket support for browsers (Phase 5)
- Moderation tooling / spam detection (Phase 5)
- Bot/plugin system (Phase 5)
