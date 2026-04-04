# Farder Phase 3.3: Data Deletion Rights (Server) — Design Spec

**Date:** 2026-04-03
**Status:** Draft
**Parent Spec:** `docs/specs/2026-04-01-privacy-chat-platform-design.md`
**Depends On:** Phase 2 (Servers & Text), Phase 3.1 (File Attachments), Phase 3.2 (Threads & Reactions)

## Goal

Give server members the right to request complete deletion of their data from a server. After a 72-hour cancellable grace period, the server purges the member's identity and content — anonymizing messages, removing attachments, clearing reactions, and deleting the member record. Conversation structure is preserved with "Deleted User" / `[deleted]` placeholders.

## Deletion Request Flow

### Step 1: User Requests Deletion

The user sends a `RequestDeletion` request on the main bi-stream. The server:

1. Verifies the user is an authenticated member of the server.
2. Verifies no pending deletion request already exists for this member.
3. Creates a `deletion_requests` row with `expires_at = now() + 72 hours`.
4. Broadcasts a `DeletionRequested` event to all connected members (so admins are aware).
5. Returns `ServerResponse::Ok`.

The user cannot be the server owner — owners must transfer ownership first (or the server is abandoned). If the owner requests deletion, the server rejects with an error.

### Step 2: Grace Period (72 Hours)

During the grace period:
- The user can continue using the server normally.
- The user can check their deletion status via `GetDeletionStatus`.
- The user can cancel the request via `CancelDeletion`.
- Admins see the pending request but cannot block or delay it.
- No data is modified during the grace period.

### Step 3: User Cancels (Optional)

If the user sends `CancelDeletion` during the grace period:
1. The `deletion_requests` row is deleted.
2. A `DeletionCancelled` event is broadcast.
3. Returns `ServerResponse::Ok`.

If no pending request exists, returns an error.

### Step 4: Purge Execution (After 72 Hours)

The existing retention background task checks for expired deletion requests each cycle. When `expires_at < now()`:

1. **Anonymize messages:** For every message authored by the user, set `author` to the sentinel key (`[0u8; 32]`) and `content` to `[deleted]`. Update FTS5 entries to `[deleted]`.
2. **Remove attachments:** For every message by the user that has attachments, delete the `message_attachments` rows, decrement `ref_count` on referenced files, clean up orphaned files from disk.
3. **Remove reactions:** Delete all rows from `reactions` where `user_key` matches the deleted user.
4. **Remove member data:** Delete from `member_roles`, then delete from `members`.
5. **Clean up deletion request:** Delete the `deletion_requests` row.
6. **Broadcast:** Send `DeletionExecuted { public_key }` event to all connected members.

### Ordering Constraint

Attachments must be cleaned up before messages are anonymized, because `message_attachments` references `messages.id` and we need the original `author` to identify which messages to process. The purge processes in this order:

1. Identify all message IDs authored by the user
2. For each message: remove attachments (decrement ref_counts)
3. For each message: update FTS5 entry to `[deleted]`
4. For each message: anonymize (set author + content)
5. Remove user's reactions across all messages
6. Remove member roles and member record
7. Delete the deletion request row

## Sentinel Key

The deleted user's messages are attributed to a sentinel public key: `[0u8; 32]` (32 zero bytes). This is not a valid Ed25519 public key and can never collide with a real user's identity.

Constant: `pub const DELETED_USER_KEY: [u8; 32] = [0u8; 32];`

The client is responsible for detecting this sentinel key and rendering "Deleted User" as the display name. The server does not store a member record for the sentinel key.

## Database Changes

### New Table: `deletion_requests`

| Column | Type | Description |
|--------|------|-------------|
| id | INTEGER PRIMARY KEY | Auto-increment |
| member_key | BLOB UNIQUE NOT NULL | Public key of the requesting member |
| requested_at | INTEGER NOT NULL | Unix timestamp of the request |
| expires_at | INTEGER NOT NULL | Unix timestamp when purge should execute (requested_at + 72h) |

The `UNIQUE` constraint on `member_key` ensures only one pending request per member.

## Protocol Changes

### New Requests

```
RequestDeletion          — no fields, applies to the authenticated member
CancelDeletion           — no fields
GetDeletionStatus        — no fields
```

### New Response Variant

```
DeletionStatus {
    pending: bool,
    requested_at: Option<u64>,
    expires_at: Option<u64>,
}
```

### New Events

```
DeletionRequested { public_key: PublicKey }          — broadcast to All
DeletionCancelled { public_key: PublicKey }           — broadcast to All
DeletionExecuted { public_key: PublicKey }            — broadcast to All
```

### New Constant

```
pub const DELETED_USER_KEY: [u8; 32] = [0u8; 32];
```

Added to `farder-protocol/src/server.rs` so both server and client can reference it.

## Server Module Changes

### Modified Modules

- **`db.rs`** — add `deletion_requests` table to schema
- **`farder-protocol/src/server.rs`** — add `DELETED_USER_KEY` constant, new request/response/event variants, `DeletionStatus` struct
- **`handlers.rs`** — add `RequestDeletion`, `CancelDeletion`, `GetDeletionStatus` handlers
- **`retention.rs`** — add `execute_pending_deletions` function called each cycle; performs the full purge for expired requests
- **`members.rs`** — add deletion request CRUD: `create_deletion_request`, `get_deletion_request`, `cancel_deletion_request`, `list_expired_deletion_requests`
- **`messages.rs`** — add `anonymize_messages_by_author` function that updates author + content + FTS5 for all messages by a given public key
- **`attachments.rs`** — add `get_attachment_file_ids_for_author_messages` to find all files attached to a user's messages for cleanup
- **`reactions.rs`** — add `delete_reactions_by_user` to remove all reactions by a given user across all messages

## What's NOT in Phase 3.3

- DM deletion (requires client/node changes, separate scope)
- Admin override or delay of deletion requests
- Pre-deletion data export
- Ownership transfer (prerequisite for owner deletion, separate feature)
- Notification to the deleted user's DM contacts
