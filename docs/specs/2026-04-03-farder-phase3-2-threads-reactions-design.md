# Farder Phase 3.2: Threads & Reactions — Design Spec

**Date:** 2026-04-03
**Status:** Draft
**Parent Spec:** `docs/specs/2026-04-01-privacy-chat-platform-design.md`
**Depends On:** Phase 2 (Servers & Text), Phase 3.1 (File Attachments)

## Goal

Add thread conversations and emoji reactions to Farder's server messaging. Threads are lightweight channels spawned from messages, inheriting parent channel permissions. Reactions are Unicode emoji attached to messages with per-user tracking and a 20-unique-emoji cap.

## Threads

### Data Model

A thread is a channel with `channel_type = "thread"`. The `channels` table gains a new column:

| Column | Type | Description |
|--------|------|-------------|
| thread_parent_message_id | INTEGER | FK to messages.id. NULL for non-thread channels. |

Thread channels inherit their parent channel's `category_id`. Permission resolution for a thread uses the **parent channel's** permissions — the thread itself has no overrides, and the thread's `category_id` matches the parent so category overrides also apply naturally.

### Thread Lifecycle

**Creation:** Any member with `SEND_MESSAGES` in the parent channel can create a thread via `CreateThread { message_id, name }`. The server:
1. Looks up the parent message to find its `channel_id`.
2. Verifies the member has `SEND_MESSAGES` in that channel.
3. Creates a new channel row with `channel_type = "thread"`, `thread_parent_message_id = message_id`, `category_id` copied from the parent channel, `name` from the request (or truncated parent message content if not provided, max 50 chars).
4. Returns the new thread's channel info and broadcasts `ChannelCreated`.

**Reading/Writing:** Thread access follows the parent channel's permissions. If a member has `VIEW_CHANNEL` + `READ_MESSAGES` on the parent, they can read the thread. If they have `SEND_MESSAGES` on the parent, they can post in the thread. The permission resolution algorithm already handles this because the thread channel shares the same `category_id` and has no channel-level overrides.

**Deletion:** Threads are deleted via the existing `DeleteChannel` request (requires `MANAGE_CHANNEL` on the parent channel). Deleting a thread deletes all messages within it (same as channel deletion).

**Parent message deletion:** Deleting the message that spawned a thread does NOT delete the thread. The thread persists as an orphaned conversation. The `thread_parent_message_id` becomes a dangling reference — the client displays "original message was deleted" (same pattern as `reply_to` dangling references from Phase 2).

### Thread Metadata on Messages

When a message has a thread spawned from it, the `MessageInfo` includes thread metadata so the client can show "N replies" and link to the thread:

- `thread_id: Option<u64>` — the channel ID of the thread (if one exists for this message)
- `thread_message_count: Option<u32>` — number of messages in the thread

These are computed on read (not stored) by querying the `channels` table for a thread with `thread_parent_message_id = message.id` and counting messages in that thread channel.

### Thread Display

Threads do not appear in the main channel list. The server includes them in `GetServerInfo` with `channel_type = "thread"`, but the client is expected to display them separately (e.g., in a sidebar or as expandable sections under their parent messages). The `Subscribe` mechanism works the same — clients subscribe to a thread's channel ID to receive live messages.

## Reactions

### Data Model

New table:

**`reactions`**

| Column | Type | Description |
|--------|------|-------------|
| message_id | INTEGER NOT NULL | FK to messages.id |
| user_key | BLOB NOT NULL | Public key of the member who reacted |
| emoji | TEXT NOT NULL | Unicode emoji string (e.g., "👍", "❤️") |
| created_at | INTEGER NOT NULL | Unix timestamp |
| PRIMARY KEY | (message_id, user_key, emoji) | One reaction per user per emoji per message |

Index: `idx_reactions_message ON reactions(message_id)`

### Reaction Rules

- A user can react with multiple different emoji on the same message.
- A user can only react with the same emoji once per message (idempotent — re-adding is a no-op).
- Maximum **20 unique emoji** per message. The 21st different emoji is rejected with an error. No limit on how many users react with the same emoji.
- Users can remove their own reactions freely.
- Members with `MANAGE_MESSAGES` permission can remove any user's reaction.

### Reaction Summary in Messages

`MessageInfo` gains:
```
pub reactions: Vec<ReactionGroup>
```

Where:
```
pub struct ReactionGroup {
    pub emoji: String,
    pub count: u32,
    pub me: bool,
}
```

- `emoji` — the Unicode emoji string
- `count` — total number of users who reacted with this emoji
- `me` — whether the requesting member has reacted with this emoji

The `me` field requires the requesting member's identity. Message query functions (`get_message`, `fetch_history`, `search_messages`) need to accept the requester's `PublicKey` to compute this field.

Reactions are loaded alongside messages. For `fetch_history`, reactions are batch-loaded (similar to how attachments are batch-loaded) to avoid N+1 queries.

### Cascade Deletion

When a message is deleted, all its reactions are deleted: `DELETE FROM reactions WHERE message_id = ?`.

This is handled in `messages::delete_message` and `messages::delete_messages_before`, alongside the existing FTS5 and attachment cleanup.

## Protocol Changes

### New Types

```rust
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReactionGroup {
    pub emoji: String,
    pub count: u32,
    pub me: bool,
}
```

### Modified Types

**`ChannelType`** gains:
```rust
Thread
```

**`ChannelInfo`** gains:
```rust
pub thread_parent_message_id: Option<u64>,
```

**`MessageInfo`** gains:
```rust
pub reactions: Vec<ReactionGroup>,
pub thread_id: Option<u64>,
pub thread_message_count: Option<u32>,
```

### New Requests

```rust
CreateThread { message_id: u64, name: Option<String> }
AddReaction { message_id: u64, emoji: String }
RemoveReaction { message_id: u64, emoji: String }
```

`CreateThread` requires `SEND_MESSAGES` in the parent channel.
`AddReaction` requires `READ_MESSAGES` in the message's channel.
`RemoveReaction` — own reactions always allowed; others' reactions require `MANAGE_MESSAGES`.

### New Responses

No new response types needed. `CreateThread` returns `ServerResponse::Ok` (the thread channel is broadcast as `ChannelCreated`). `AddReaction` and `RemoveReaction` return `ServerResponse::Ok`.

### New Events

```rust
ReactionAdded { message_id: u64, channel_id: u64, emoji: String, public_key: PublicKey }
ReactionRemoved { message_id: u64, channel_id: u64, emoji: String, public_key: PublicKey }
```

Both sent to `EventTarget::Subscribers(channel_id)`.

## Server Module Changes

### New Module: `reactions.rs`

Handles:
- `add_reaction(conn, message_id, user_key, emoji) -> Result<()>` — INSERT OR IGNORE, enforce 20-emoji cap
- `remove_reaction(conn, message_id, user_key, emoji) -> Result<()>` — DELETE
- `remove_reaction_any_user(conn, message_id, emoji, user_key) -> Result<()>` — for MANAGE_MESSAGES
- `get_reactions_for_message(conn, message_id, requester: &PublicKey) -> Result<Vec<ReactionGroup>>` — GROUP BY emoji with COUNT and `me` check
- `get_reactions_for_messages(conn, message_ids, requester: &PublicKey) -> Result<HashMap<u64, Vec<ReactionGroup>>>` — batch load
- `delete_reactions_for_message(conn, message_id) -> Result<()>` — cascade delete

### Modified Modules

- **`db.rs`** — add `reactions` table and `thread_parent_message_id` column to `channels` table schema
- **`channels.rs`** — `create_channel` and `row_to_channel_info` updated for thread fields; new helper `get_thread_for_message(conn, message_id)` returns the thread channel if one exists
- **`messages.rs`** — `get_message` and `fetch_history` accept requester PublicKey, load reactions (batch) and thread metadata; `delete_message` and `delete_messages_before` cascade-delete reactions
- **`handlers.rs`** — add `CreateThread`, `AddReaction`, `RemoveReaction` handlers; update `SendMessage`/`FetchHistory`/`Search`/`GetServerInfo` handlers to pass requester
- **`connection.rs`** — pass member public key to handlers that need it for reaction `me` computation
- **`farder-protocol/src/server.rs`** — new types, modified types as listed above

## What's NOT in Phase 3.2

- Custom emoji / stickers / giphy (future — account-level feature)
- Thread auto-archiving or lifecycle management (deferred for UX refinement)
- Thread-specific permissions or overrides
- Mentions (@user, @role, @everyone)
- Reaction animations or rich emoji rendering (client-side concern)
- Thread nesting (threads of threads)
