# Mesh Rung 1 — Sub-project 4a: Attachments over the signed event log

**Date:** 2026-06-30
**Status:** design (awaiting owner review)
**Parent design:** `docs/superpowers/specs/2026-06-25-mesh-signed-log-foundation-design.md` (§"Attachments as revocable capabilities")
**Predecessors:** mesh Rung 1 sub-projects 1 (event crypto), 2 (authz state), 3a (server ingest), 3b (client posting); mesh invite/join 1/2/3a/3b.

## Problem

Today a message **with attachments** never goes over the mesh signed event log. The
client deliberately routes it to the legacy `SendMessage` path:

```ts
// client/src/components/MessageInput.tsx:244
if (logServerId && finalAttachments.length === 0 && !dm) {
    await api.submitEvent(...);          // mesh signed-event path
} else {
    await api.sendMessage(... finalAttachments ...);  // legacy
}
```

The mesh `MessagePosted` event already *carries* an attachment descriptor
(`EventPayload::MessagePosted { ..., attachments: Vec<AttachmentCap> }`,
`crates/farder-crypto/src/event_log.rs:115-120`), and `AttachmentCap`
(`content_hash`, `declared_type`, `size`, `uploader`) is content-hash-addressed to
line up with the `files.hash` column. But the pipe is unconnected at both ends:

- **Client:** `submit_event` hard-codes `attachments: vec![]` (`commands.rs:3850`);
  the `submitEvent` bridge has no attachments parameter.
- **Server:** `derive_message_row` (`crates/farder-server/src/event_ingest.rs:90-91`)
  explicitly does NOT derive attachments — "Attachments are NOT derived in this
  slice (sub-project 4)."

Sub-project 4a connects that pipe: attachments flow over the log, validated against
the actual stored bytes, with downloads gated so a content hash is not an existence
oracle.

## What already exists (so 4a does not rebuild it)

The code map (2026-06-30) confirmed substantial machinery is already in place:

- **Content-addressed storage.** `files` table keyed on `hash` (UNIQUE) with `size`,
  `mime_type`, `original_name`, `uploaded_by`, `ref_count`
  (`crates/farder-server/src/db.rs:122-134`); bytes on disk at the content path
  (`attachments.rs:26-32`).
- **Server-verified hashing.** On upload the server **re-hashes the received bytes**
  and rejects a hash mismatch (`store_or_reuse_from_temp_file`, `attachments.rs:138-161`).
  So "the bytes match `content_hash`" is already guaranteed for any blob in the store
  — cap validation does NOT need to re-check that, only `size` / `mime_type` / `uploader`.
- **Message↔attachment linking** via the `message_attachments` join table
  (`db.rs:136-150`) + `create_message_attachment` (which also bumps `ref_count`,
  `attachments.rs:362-388`) + `get_attachments_for_message(s)`.
- **Download authorization gate.** `handle_download_stream` already requires (unless
  owner) `VIEW_CHANNEL | READ_MESSAGES` on a channel the file is attached to, resolved
  through `message_attachments → messages` (`connection.rs:236-275`); and the mesh
  `content_block_reason` gate sits at the top of `handle_auxiliary_stream`
  (`connection.rs:426-428`) denying non-log-members entirely.
- **Upload is over an aux QUIC stream, separate from the control channel**, returns a
  `file_id`, dedupes by hash. The client already computes the SHA-256 itself
  (`commands.rs:1434`).

**Consequence:** existence-oracle download gating mostly falls out for free once mesh
attachments create `message_attachments` rows (the gate is reached via that join).
The real new work is (1) server: validate caps + materialize join rows in ingest;
(2) client: build caps + route attachment-bearing messages over the log.

## Goals

1. A message with attachments, sent on a mesh (log-mode) server, is posted as a signed
   `MessagePosted` event whose `attachments` carry honest `AttachmentCap`s.
2. The server, on ingesting that event, validates each cap **against the actual stored
   blob** and materializes a `message_attachments` row only for valid caps; the message
   itself always ingests (a bad cap never rejects the signed event).
3. Attachment **download** on a mesh server is gated so a content hash cannot be used as
   an existence oracle: only authorized viewers of the referencing message can fetch,
   and absent / unauthorized / unmaterialized responses are uniform.

## Non-goals (explicitly deferred)

- **Redaction / takedown / GC / moderation state** (`AttachmentRedacted`, a
  `moderation_state` column, mod UI) — that is **sub-project 4b**.
- **Server-side magic-byte content sniffing / type allowlist** — the separate
  file-hardening track. 4a validates that a cap's `declared_type` is *consistent* with
  the blob's recorded `mime_type`; it does not sniff the bytes.
- **Visible "pending blob" placeholders + late-arrival reconciliation** (a cap whose
  blob shows up later). In single-host Rung 1 the blob is always uploaded before the
  event is submitted, so a missing blob means a misbehaving/buggy client. The
  placeholder + late-arrival heal is a **replication-era (Rung 3) refinement**;
  validation logic is written now so it composes, but no new UI state is added.
- Populating image `width`/`height`/`duration` — already always `None` on the upload
  path today; no regression, out of scope.

## Design

### Crypto layer — no change

`AttachmentCap` and `MessagePosted.attachments` already exist and round-trip. Sub-project
1 explicitly reserved cap validation for 4a. `AttachmentCap` has **no `original_name`**;
4a derives the display name from the blob's `files.original_name` rather than adding a
field to the signed struct (the name is cosmetic; the hash identifies the bytes and the
uploader is fixed by validation, so the stored blob's name is the right source).

### Server — cap validation + materialization in ingest

When the `SubmitEvent` handler accepts a `MessagePosted` event (authz already validated
by `LogState::apply`, which is the real gate — cap validity is a **derived-view**
concern and never rejects the event):

1. Derive the `messages` row as today (existing `derive_message_row`).
2. For each `AttachmentCap` in `attachments`, in order, compute its validity:
   - Look up `get_file_by_hash(cap.content_hash)`.
   - The cap is **valid** iff a blob exists AND `blob.size == cap.size` AND
     `blob.mime_type == cap.declared_type` AND `blob.uploaded_by == cap.uploader` AND
     the poster is allowed to attach it: `event.author == cap.uploader` **or**
     `event.author` is the server owner (mirrors the legacy rule at
     `handlers.rs:390-392`).
   - **Valid** → `create_message_attachment(message_id, file_id, position, blob.original_name, …)`
     (bumps `ref_count`); the file is now downloadable through the existing gate.
   - **Invalid** (missing blob, or size/type/uploader/author mismatch) → **not
     materialized**; `tracing::warn!` with the reason; the message renders without that
     attachment. (Single-host: only reachable via a lying/buggy client.)
3. Wrap the message-row + attachment-row writes in the **existing ingest DB transaction**
   (sub-project 3b already wraps `store_event` + `derive_message_row`,
   `event_ingest.rs`) so a crash cannot leave a message row without its attachments.

Validation is a **pure function of (cap, blob metadata)**, so a replay/reconcile reaches
the same verdict.

### Server — reconciliation

Extend the startup `reconcile_messages` repair (3b) so it also (idempotently) materializes
attachment rows for any `MessagePosted` event whose message row exists but whose valid
caps have no `message_attachments` rows. Idempotent = never double-insert (guard on the
existing join rows). Legacy `MessagePosted` events carry empty `attachments`, so this is
a no-op for them; it exists for forward-compat and crash-recovery.

### Server — uniform download responses (existence-oracle hardening)

The existing download path already denies a non-authorized fetch. 4a ensures the failure
responses are **uniform** so a caller cannot distinguish "blob absent" from "exists but
you can't see it" from "unmaterialized cap": all map to the same `DownloadResponse::Error`
shape/reason ("not available"), with no timing or message that distinguishes the cases.
(Implementation reviews `handle_download_stream`'s current error reasons and collapses the
distinguishable ones; the plan pins the exact reasons.)

### Client — build caps and route over the log

1. **Surface cap fields from the upload path.** Extend the upload result so the frontend
   has each attachment's `{ content_hash, size, mime_type }` (the Rust upload command
   already has all three internally; today it returns only `file_id`). Apply the same to
   the other attachment sources the send path uses — server-side URL fetch (`fetch_url`)
   and inline-emoji resolution — so every attachment, whatever its source, can produce a
   cap. (All are backed by the same `files` store with a `hash`; the plan decides whether
   each returns the hash directly or a small `file_id → {hash,size,mime}` lookup is added.
   A source that cannot surface a hash falls back to legacy for that message, logged.)
2. **`submit_event` command + `submitEvent` bridge** gain an `attachments` parameter:
   a list of `{ content_hash, declared_type, size }`. The command builds
   `AttachmentCap { content_hash, declared_type, size, uploader: <self identity> }` for
   each, fills `MessagePosted.attachments`, and signs as today (device-subkey signs,
   author = identity, advance-DeviceState-only-on-accept).
3. **`MessageInput.handleSend` routing:** on a mesh server (`logServerId` set, non-DM),
   route the message — attachments included — through `submitEvent` instead of legacy
   `sendMessage`. The `finalAttachments.length === 0` condition is removed for the mesh
   case (legacy path stays for non-mesh servers and DMs).

## Data flow (mesh server, message with one attachment)

```
user picks file
  client uploads bytes over aux stream  ──▶ server re-hashes, stores in files{hash,...}, returns {file_id, hash, size, mime}
  client builds AttachmentCap{content_hash, declared_type, size, uploader=self}
  client signs MessagePosted{channel_id, content, reply_to, attachments:[cap]}
  ServerRequest::SubmitEvent{event}     ──▶ LogState::apply (authz)  ──▶ persist event
                                              derive_message_row  +  per-cap validate vs files{hash}
                                                 valid  → create_message_attachment (downloadable)
                                                 invalid→ warn, skip
                                              (all in one DB transaction)
                                            broadcast NewMessage
  another member opens the message
  client DownloadRequest{file_id}        ──▶ content_block_reason gate (log member?)
                                            + channel-view gate (attached to a channel I can VIEW?)
                                              → stream bytes  (else uniform "not available")
```

## Testing

**Rust (`farder-server`)** — extend the existing single-threaded submit-event harness:

- A `MessagePosted` with a cap whose blob is present and whose `size`/`mime_type`/
  `uploaded_by` match → message ingests, a `message_attachments` row is created, file is
  downloadable by an authorized member.
- A cap referencing a **missing** hash → message ingests, **no** attachment row, not
  downloadable.
- A cap with mismatched `size`, mismatched `declared_type`, mismatched `uploader`, or a
  poster who is neither the uploader nor owner → message ingests, attachment quarantined
  (not materialized) in each case.
- Owner attaches a file uploaded by another member → materialized (owner exception).
- `reconcile_messages` re-materializes a missing-but-valid attachment row idempotently;
  running it twice does not double-insert.
- Replay from genesis reaches the same attachment materialization as the live path.
- Download of an unmaterialized / unauthorized / absent file returns the **same** uniform
  error.

**Client** — `cargo build` (client crate) + `npx tsc --noEmit`; the cap-building in
`submit_event` is a pure construction with an inline test-note. Runtime behavior is the
owner's Windows test (below) per the verify-before-done rule.

**Docs** (same commit): add the missing attachments/files module doc (or extend
`server-connection.md` / `server-handlers.md`), update `tauri-commands.md` +
`frontend-bridge.md` for the changed `submit_event`/`submitEvent`/upload signatures, and
note the cap-validation step in the mesh data path in `ARCHITECTURE.md`.

## Owner runtime verification (pending — server changed → full rebuild incl. sidecar)

`git pull` → `cargo build -p farder-server` → STOP app →
`.\client\src-tauri\binaries\copy-sidecar.ps1` (run from repo root) →
`cd client; npm run tauri dev` → `Ctrl+Shift+R`. Then on a **mesh** server:

1. Post a message with an image attachment → it sends, renders with the image, and the
   image **downloads/displays**.
2. Restart the app → the message **and its attachment survive** (it is a real
   `events`-row-derived attachment, not just a legacy `messages` row).
3. A second identity (FARDER_DATA instance) who is a log member of the server sees the
   message and can download the attachment; a non-member cannot (waiting/!member gate).

## Carry-forward / known limitations

- Single-host only: cap-vs-bytes validation assumes the blob is present at ingest (true
  because the client uploads before submitting). Late-arriving blobs + visible pending
  placeholders are Rung 3.
- No moderation/retention state yet → no takedown/GC (4b).
- `declared_type` is validated for *consistency* with the recorded type, not sniffed
  against the bytes (file-hardening track).
- DM attachments stay on the legacy path (mesh log is server-scoped).
