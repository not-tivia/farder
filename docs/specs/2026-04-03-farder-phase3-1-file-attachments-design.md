# Farder Phase 3.1: File Attachments & Media — Design Spec

**Date:** 2026-04-03
**Status:** Draft
**Parent Spec:** `docs/specs/2026-04-01-privacy-chat-platform-design.md`
**Depends On:** Phase 2 (Servers & Text)

## Goal

Add file attachment support to Farder. Server channels store files on the server's local filesystem with content-addressed deduplication. DM attachments are streamed peer-to-peer through the relay, encrypted end-to-end, with no size limit. A message can have up to 10 attachments. Uploads and downloads use dedicated QUIC streams so chat stays responsive during large transfers.

## Two Attachment Modes

### Server Attachments (Channels)

Files uploaded to the server, stored on the local filesystem, subject to a configurable size limit. The server sees file contents (channels are not E2EE in Phase 2). Content-addressed by SHA-256 — identical files are stored once and reference-counted.

### DM Attachments (Peer-to-Peer)

Files streamed directly between peers through the relay, encrypted with the shared DM secret (AES-256-GCM, same key as DM text messages). The relay sees only opaque ciphertext. No server storage, no size limit, no deduplication (E2EE makes identical plaintext produce different ciphertext). Both peers must be online — no offline file queuing.

## Server Attachment Architecture

### Storage Layout

Files stored on disk at `{storage_dir}/{hash[0:2]}/{hash[2:4]}/{hash}`, where `hash` is the lowercase hex-encoded SHA-256 digest. The two-level directory sharding prevents huge flat directories.

Example: a file with SHA-256 `a1b2c3d4e5...` is stored at `{storage_dir}/a1/b2/a1b2c3d4e5...`

Default storage directory: `./files` (configurable via `--storage-dir`).

### Database Tables

**`files`** — tracks stored files on disk.

| Column | Type | Description |
|--------|------|-------------|
| id | INTEGER PRIMARY KEY | Auto-increment file ID |
| hash | TEXT UNIQUE NOT NULL | SHA-256 hex digest |
| size | INTEGER NOT NULL | File size in bytes |
| mime_type | TEXT NOT NULL | MIME type (e.g., `image/png`) |
| original_name | TEXT NOT NULL | Original filename from first upload |
| uploaded_by | BLOB NOT NULL | Public key of first uploader |
| uploaded_at | INTEGER NOT NULL | Unix timestamp |
| ref_count | INTEGER NOT NULL DEFAULT 1 | Number of message_attachments referencing this file |

**`message_attachments`** — maps attachments to messages.

| Column | Type | Description |
|--------|------|-------------|
| id | INTEGER PRIMARY KEY | Auto-increment attachment ID |
| message_id | INTEGER NOT NULL | FK to messages.id |
| file_id | INTEGER NOT NULL | FK to files.id |
| position | INTEGER NOT NULL | Ordering within the message (0-indexed) |
| original_name | TEXT NOT NULL | Filename as provided by the uploader for this attachment |
| width | INTEGER | Image/video width in pixels (client-provided, nullable) |
| height | INTEGER | Image/video height in pixels (client-provided, nullable) |
| duration_secs | REAL | Audio/video duration in seconds (client-provided, nullable) |

Indexes:
- `idx_message_attachments_message ON message_attachments(message_id)`
- `idx_message_attachments_file ON message_attachments(file_id)`

### Content-Addressed Deduplication

When a client uploads a file, it sends the SHA-256 hash before sending any bytes. The server checks if a file with that hash already exists in the `files` table:

- **Hash exists:** Server responds immediately with the existing `file_id`. No bytes transferred. No ref_count change yet — that happens when the file is attached to a message via `SendMessage`.
- **Hash doesn't exist:** Server accepts the file bytes, writes to a temp file, verifies the hash matches, moves to the content-addressed path, and inserts a `files` row with `ref_count = 0`. The ref_count is incremented later when `SendMessage` attaches this file to a message.

### Reference Counting & Cleanup

Each `files` row has a `ref_count` tracking how many `message_attachments` rows reference it.

**ref_count lifecycle:**
- Upload creates a `files` row with `ref_count = 0`.
- `SendMessage` with `attachment_ids` increments `ref_count` by 1 for each referenced file and creates `message_attachments` rows.
- When a message is deleted: decrement `ref_count` for each attachment, delete the `message_attachments` rows.
- If `ref_count` reaches 0, delete the file from disk and remove the `files` row.

**Orphan cleanup:** A file uploaded but never attached to a message stays at `ref_count = 0`. A periodic cleanup task deletes `files` rows where `ref_count = 0` and `uploaded_at` is older than 1 hour. This gives clients time between uploading and sending the message.

`hard_delete_channel` must also handle this: decrement ref_counts for all files referenced by messages in that channel, delete orphaned files.

Message retention purges (from Phase 2) must also decrement ref_counts when deleting expired messages.

### Server Configuration

| Flag | Default | Description |
|------|---------|-------------|
| `--storage-dir` | `./files` | Directory for stored files |
| `--max-file-size` | `52428800` (50 MB) | Maximum upload size in bytes |

## Upload Protocol (Server Attachments)

Uploads happen on a dedicated QUIC stream, separate from the main bi-stream.

### Flow

1. Client opens a **new bi-directional QUIC stream** on the **same QUIC connection** used for chat. Since the connection is already authenticated, the server knows which member is making the request — no separate auth needed on the upload stream.
2. Client sends an `UploadRequest` frame (length-prefixed MessagePack, same framing as main protocol):

```
UploadRequest {
    channel_id: u64,           // target channel (for permission check)
    file_name: String,         // original filename
    file_size: u64,            // size in bytes
    hash: String,              // SHA-256 hex digest
    mime_type: String,         // MIME type
    width: Option<u32>,        // client-provided image/video width
    height: Option<u32>,       // client-provided image/video height
    duration_secs: Option<f64>,// client-provided audio/video duration
}
```

3. Server validates:
   - The QUIC connection is authenticated (member identity known from the main bi-stream auth).
   - Member has `SEND_MESSAGES` permission for the target channel.
   - `file_size` does not exceed `--max-file-size`.
   - `file_name` is non-empty and reasonable length (max 255 bytes).

4. Server checks if `hash` already exists in the `files` table:

   **Already exists:**
   - Server responds with `UploadResponse::Complete { file_id }` using the existing file's ID.
   - Stream closes. No bytes transferred. ref_count unchanged (incremented at SendMessage time).

   **Doesn't exist:**
   - Server responds with `UploadResponse::Ready`.
   - Client streams raw file bytes on the same stream until `file_size` bytes are sent, then closes its send side.
   - Server writes bytes to a temp file in `storage_dir`, computing SHA-256 as it goes.
   - Server verifies computed hash matches the declared hash. If mismatch: responds `UploadResponse::Error { reason: "hash mismatch" }`, deletes temp file.
   - Server moves temp file to content-addressed path.
   - Server inserts `files` row.
   - Server responds with `UploadResponse::Complete { file_id }`.

### Upload Response

```
enum UploadResponse {
    Ready,                           // server is ready to receive bytes
    Complete { file_id: u64 },       // upload finished, use this ID in SendMessage
    Error { reason: String },        // validation failed
}
```

### Attaching to a Message

After one or more uploads complete, the client sends a regular `SendMessage` request on the main bi-stream. The `SendMessage` request gains a new field:

```
SendMessage {
    channel_id: u64,
    content: String,
    reply_to: Option<u64>,
    attachment_ids: Vec<u64>,  // NEW: file_ids from UploadResponse::Complete (max 10)
}
```

The server validates:
- Each `file_id` exists in the `files` table.
- `attachment_ids.len() <= 10`.
- The uploading member owns these file_ids (or they were uploaded to the same channel — prevents cross-channel file theft).

The server creates `message_attachments` rows linking the message to the files, with `position` matching the order in `attachment_ids`.

## Download Protocol (Server Attachments)

Downloads also use dedicated QUIC streams.

### Flow

1. Client opens a **new bi-directional QUIC stream** on the **same QUIC connection** (already authenticated).
2. Client sends a `DownloadRequest` frame:

```
DownloadRequest {
    file_id: u64,
}
```

3. Server validates:
   - Connection is authenticated.
   - File exists.
   - Member has `VIEW_CHANNEL` and `READ_MESSAGES` for at least one channel containing a message that references this file.

4. Server responds with `DownloadResponse::Start`:

```
DownloadResponse::Start {
    file_name: String,
    file_size: u64,
    hash: String,
    mime_type: String,
}
```

5. Server streams the file bytes on the same stream, then closes its send side.
6. Client verifies SHA-256 hash matches.

### Download Response

```
enum DownloadResponse {
    Start { file_name: String, file_size: u64, hash: String, mime_type: String },
    Error { reason: String },
}
```

## DM Attachment Protocol

DM file attachments are streamed peer-to-peer through the relay, using the same E2EE as DM text messages.

### Flow

1. Sender and recipient must both be online with an active session (X25519 shared secret established).
2. Sender opens a **new QUIC stream** through the relay to the recipient.
3. Sender sends a `DmFileHeader` (encrypted with AES-256-GCM using the shared secret):

```
DmFileHeader {
    file_name: String,
    file_size: u64,
    hash: String,          // SHA-256 of the plaintext file
    mime_type: String,
    width: Option<u32>,
    height: Option<u32>,
    duration_secs: Option<f64>,
}
```

4. Sender streams the file, encrypting each chunk with AES-256-GCM (same key, sequential nonces).
5. Recipient decrypts each chunk, writes to local storage, verifies SHA-256 hash of the reassembled plaintext.

### Chunked Encryption

Files are split into chunks (default 64 KB). Each chunk is encrypted independently:

- Chunk format: `[nonce (12 bytes)][ciphertext][GCM tag (16 bytes)]`
- Nonce: 12-byte counter starting at 0, incrementing per chunk. The first 4 bytes are a random prefix (sent in the header), the last 8 bytes are the counter.
- Final chunk may be smaller than 64 KB.
- After all chunks, sender sends a zero-length terminator frame.

This streaming encryption avoids buffering the entire file in memory.

### Offline Handling

If the recipient is offline, the file transfer cannot proceed. The client should:
- Notify the user that the recipient is offline.
- Queue the transfer locally and retry when the recipient comes online.
- The relay does NOT store file data.

## Protocol Changes Summary

### New Types in `farder-protocol`

```rust
// Attachment metadata returned with messages
pub struct AttachmentInfo {
    pub id: u64,
    pub file_id: u64,
    pub name: String,
    pub size: u64,
    pub mime_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_secs: Option<f64>,
}

// Upload stream protocol
pub enum UploadRequest { ... }    // as described above
pub enum UploadResponse { ... }   // as described above

// Download stream protocol
pub enum DownloadRequest { ... }  // as described above
pub enum DownloadResponse { ... } // as described above

// DM file transfer
pub struct DmFileHeader { ... }   // as described above
```

### Modified Types

**`MessageInfo`** gains:
```rust
pub attachments: Vec<AttachmentInfo>,  // empty for text-only messages
```

**`ServerRequest::SendMessage`** gains:
```rust
pub attachment_ids: Vec<u64>,  // empty for text-only, max 10
```

### New Server Events

```rust
ServerEvent::FileUploaded { file_id: u64, channel_id: u64 }  // informational, not critical
```

### New DM Protocol Message

```rust
Message::DmFileHeader { sender: PublicKey, encrypted_header: Vec<u8> }
Message::DmFileChunk { sender: PublicKey, encrypted_chunk: Vec<u8> }
Message::DmFileComplete { sender: PublicKey }
```

## Server Module Changes

### New Module: `attachments.rs`

Handles:
- File storage (write to disk, content-addressed path computation)
- Upload validation (size, permissions)
- Download authorization (permission check)
- Reference counting (increment, decrement, cleanup)
- Database operations on `files` and `message_attachments` tables

### Modified Modules

- **`db.rs`** — add `files` and `message_attachments` tables to schema init
- **`handlers.rs`** — `SendMessage` handler processes `attachment_ids`, creates `message_attachments` rows. `DeleteMessage` handler decrements ref_counts and cleans up orphaned files.
- **`messages.rs`** — `get_message` and `fetch_history` include attachment data via JOIN. `delete_message` and `delete_messages_before` handle ref_count decrement.
- **`channels.rs`** — `hard_delete_channel` handles attachment cleanup.
- **`connection.rs`** — after authenticating the main bi-stream, spawn a loop that accepts additional bi-streams from the client (`conn.accept_bi()`). Each new stream is identified as upload or download by its first frame. The QUIC connection object is shared (via `Arc<quinn::Connection>`) so the server can map any stream on the connection to the authenticated member.
- **`main.rs`** — new CLI flags (`--storage-dir`, `--max-file-size`), pass to ServerState.
- **`state.rs`** — add `storage_dir: String` and `max_file_size: u64` to ServerState.
- **`retention.rs`** — `delete_messages_before` already handles message deletion; ensure attachment cleanup is integrated.

## Client Rendering

The server stores metadata (mime type, dimensions, duration) but does not decide what renders inline. The client is responsible for:

- Displaying images inline (JPEG, PNG, GIF, WebP)
- Showing video players for video attachments
- Showing audio players for audio attachments
- Showing download links for everything else
- Respecting the `position` field for attachment ordering

No server-side image resizing, thumbnail generation, or format conversion. The client works with the original file.

## What's NOT in Phase 3.1

- Link previews / URL embeds (deferred)
- Server-side image resizing or thumbnail generation
- Server-side metadata extraction (client provides dimensions/duration)
- Offline DM file delivery (requires relay storage, deferred)
- Streaming video/audio playback (client downloads full file first)
- Virus/malware scanning
- File type restrictions (any file type allowed, server is type-agnostic)
