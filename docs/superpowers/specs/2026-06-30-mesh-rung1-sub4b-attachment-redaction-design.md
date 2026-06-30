# Mesh Rung 1 — Sub-project 4b: Attachment redaction / takedown

**Date:** 2026-06-30
**Status:** design (awaiting owner review)
**Parent design:** `docs/superpowers/specs/2026-06-25-mesh-signed-log-foundation-design.md` (§"Attachments as revocable capabilities" — the redaction half)
**Predecessor:** sub-project 4a (`docs/superpowers/specs/2026-06-30-mesh-rung1-sub4a-attachment-caps-design.md`, shipped main `15dc1db`) — attachments now flow over the log as validated `AttachmentCap`s materialized into `message_attachments`.

## Problem

4a made attachments first-class in the signed event log but left them **permanent**: once a blob is referenced and materialized, there is no way to take it down. A privacy/communication platform needs takedown for two everyday reasons:

1. **Self-service** — a member wants to remove a file they posted ("oops, delete that").
2. **Moderation** — an owner/moderator must take down abusive or illegal content.

The red-teamed foundation spec already designed the model: a signed `AttachmentRedacted` event flips the blob's moderation state and authorizes garbage-collecting the bytes; the immutable reference event stays in the log (history honestly says "a file was here"), only the payload is removed. This is **compliant-host takedown, not Byzantine-proof** — a malicious host that already holds the bytes cannot be forced to forget them; we do not claim otherwise. 4a deliberately left the hooks: no `moderation_state`/`redacted` column on `files`, no GC-on-redact, no UI.

## What already exists (4a) that 4b builds on

- `files` table is content-addressed (`hash` UNIQUE, `uploaded_by` PublicKey, `ref_count`); blobs at the content path; `cleanup_orphaned_file` already deletes bytes + row by `file_id`.
- `message_attachments` join rows are materialized by `event_ingest::derive_attachments` only for the **first uploader** of a hash (4a validates `file.uploaded_by == cap.uploader`), so **only one identity per server can hold a materialized attachment of a given hash** — a second person posting identical bytes is quarantined. Consequence: a content-hash-keyed takedown can never touch another user's copy; the only multi-reference case is the same user posting the same file in two messages.
- Download path already returns a **uniform** `DownloadResponse::Error { reason: "not available" }` for missing/denied (existence-oracle hardening), gated on log membership + channel view.
- `LogState` (farder-crypto) exposes `is_owner`, `has_capability(pk, "kick")` (owner implicit), and folds events via `apply`. The moderation capability for kick/approve/remove is `"kick"`.
- `AttachmentInfo` (protocol read shape) = `{ id, file_id, name, size, mime_type, width, height, duration_secs }` — the client renders attachments from this.

## Goals

1. A member can take down a file **they uploaded**; an owner/moderator (holds `"kick"`) can take down **any** file — via a signed `AttachmentRedacted` event over the log.
2. Redaction **deletes the blob bytes** from the server and marks the blob redacted, recording **who** redacted it; the reference event and message stay (the message still renders).
3. A redacted attachment renders a **placeholder** distinguishing *removed by the uploader* from *removed by a moderator*; it is no longer downloadable (uniform "not available").
4. Redaction is **replay-deterministic** (the authz + redacted-set live in the log/`LogState`) and propagates **live** to connected clients.

## Non-goals (explicitly deferred / out of scope)

- **Full message edit/delete over the log** (`MessageEdited`/`MessageDeleted`) — a separate future event, not 4b. 4b removes an *attachment*, never the message.
- **Un-redaction / restore** — impossible by construction (bytes are deleted). Redaction is permanent.
- **Server-side byte content-sniffing / type allowlist** — the separate file-hardening track.
- **Richer moderation states** (quarantine/NSFW/age-gate) — 4b is a single redacted/not-redacted state.
- **Cross-host / replicated takedown guarantees** — Rung 3; 4b is single-host compliant takedown.

## Design

### Crypto layer (`farder-crypto`) — new event + authz

- New `EventPayload::AttachmentRedacted { content_hash: String }` (signed like every event).
- `LogState` gains two folded fields:
  - `attachment_uploaders: HashMap<ContentHash, PublicKey>` — the **first** `cap.uploader` seen for each content hash across `MessagePosted` events (first-writer-wins). This is who "owns" the attachment for self-takedown authz.
  - `redacted_attachments: HashSet<ContentHash>` — hashes that have been redacted.
- `apply` rules:
  - On `MessagePosted`: for each `AttachmentCap`, record `attachment_uploaders.entry(content_hash).or_insert(cap.uploader)` (effects step, after existing authz).
  - On `AttachmentRedacted { content_hash }`: authorize iff the hash is **known** (`attachment_uploaders` has it) AND (`author == attachment_uploaders[hash]` **or** `has_capability(author, "kick")`) AND it is **not already redacted**. Effect: insert into `redacted_attachments`.
- New `LogState` query `is_attachment_redacted(&self, hash) -> bool` and (for the server) access to the recorded uploader so the placeholder can name the redactor relative to the uploader.
- Map sizes are bounded by distinct attached file hashes (far fewer than messages); this keeps `LogState` checkpoint-friendly.
- **Known residual (documented, accepted for Rung 1):** because `attachment_uploaders` records the *claimed* `cap.uploader` of the first `MessagePosted` citing a hash, an attacker who knows a file's SHA-256 *before* the real owner posts it could pre-claim it and gain self-takedown rights over the later real post. This requires precognition of the exact bytes' hash and only yields takedown (not read) of that one file; the moderator path is unaffected. Tracked, not solved in 4b.

### Server (`farder-server`) — column, GC, broadcast, download guard

- Add a nullable `redacted_by BLOB` column to `files` (NULL = live; set = redactor's PublicKey). Idempotent migration (`ADD COLUMN`).
- `SubmitEvent` ingest of `AttachmentRedacted` (after `LogState::apply` authorizes): in one transaction, set `files.redacted_by = event.author` for the hash, then **delete the blob bytes from disk** (reuse the content-path deletion logic; keep the `files` row as a tombstone so `message_attachments` joins still resolve and render the placeholder). Broadcast a new `ServerEvent::AttachmentRedacted { content_hash }` (or reuse a lightweight refresh signal) to all connected clients so live views update.
- Startup `reconcile`: a redacted blob whose bytes still exist on disk (crash between mark and delete) is swept — bytes deleted when `redacted_by IS NOT NULL`. (Mirrors 4a's reconcile discipline; idempotent.)
- **Download guard:** `handle_download_stream` checks `redacted_by` and returns the uniform `"not available"` *before* attempting to read the (deleted) bytes, so a redacted file behaves identically to absent/denied.
- **Read model:** `AttachmentInfo` gains a redaction indicator — `redacted_by_moderator: Option<bool>` (None = live; `Some(false)` = redactor == original `uploaded_by`, i.e. by the uploader; `Some(true)` = redactor != uploader, i.e. by a moderator). Computed server-side from `files.redacted_by` vs `files.uploaded_by`. `get_attachments_for_message(s)` populate it.

### Client — command, UI action, placeholder

- New `redact_attachment(server_id, log_server_id, content_hash)` Tauri command: builds + signs an `AttachmentRedacted` event (same `event_build_next` + `device_chain_lock` + advance-on-accept pattern as 4a's `submit_event`), submits via `SubmitEvent`. Bridge `redactAttachment`.
- **Action placement:** on the attachment itself — a hover/right-click control on a rendered attachment. Shown as **"Remove"** when the viewer is the uploader, **"Take down"** when the viewer holds the moderation capability (owner/`"kick"`) on any attachment. Hidden otherwise. (Gating mirrors the existing `canKick`/owner checks used by `MemberContextMenu`.)
- **Placeholder render:** when `AttachmentInfo.redacted_by_moderator` is set, render a themed placeholder in place of the image/file chip — "🚫 Removed by the uploader" (`Some(false)`) or "🚫 Removed by a moderator" (`Some(true)`) — reusing existing attachment-chip classes; themed in all three theme CSS files (`xp-luna-blue`, `discord-dark`, `hello-kitty`) per the project's no-unstyled-class rule.
- A `server:attachment_redacted` listener refetches/updates the affected message so the placeholder appears live.

## Data flow (member removes their own attachment)

```
user clicks Remove on an attachment they uploaded
  client redact_attachment(server, logServer, content_hash)
    builds + signs AttachmentRedacted{content_hash}
    ServerRequest::SubmitEvent{event}
      LogState::apply: hash known? author == uploader OR has "kick"? not already redacted? → accept
      persist event; set files.redacted_by=author; delete bytes from disk (one tx)
      broadcast AttachmentRedacted{content_hash}
  every client: refetch the message → AttachmentInfo.redacted_by_moderator = Some(false)
    → renders "🚫 Removed by the uploader"; download now returns uniform "not available"
```

## Testing

**Rust (`farder-crypto`)** — `LogState` unit tests: a `MessagePosted` records the uploader; the uploader can redact their own hash; a member with `"kick"` can redact any hash; a non-uploader non-mod is rejected; redacting an unknown hash is rejected; a second redaction of the same hash is rejected; `is_attachment_redacted` reflects state; replay from genesis reaches the same redacted set.

**Rust (`farder-server`)** — ingest tests: an authorized `AttachmentRedacted` sets `redacted_by`, deletes the bytes, keeps the `files` row + `message_attachments` rows; download of a redacted file returns `"not available"`; `AttachmentInfo.redacted_by_moderator` computes correctly for uploader vs moderator redactor; the startup sweep deletes lingering bytes idempotently; an unauthorized redaction is rejected with no byte deletion.

**Client** — `cargo build` (client crate) + `npx tsc --noEmit`; the event build is a pure construction (inline test-note). Runtime behavior is the owner's Windows test.

**Docs** (same commit per task): `tauri-commands.md` (`redact_attachment`), the bridge doc (`redactAttachment` + `AttachmentInfo.redacted_by_moderator`), the server doc (AttachmentRedacted ingest + GC + download guard), `crypto.md` / event-log doc (the new event + authz), and `ARCHITECTURE.md` (the redaction flow).

## Owner runtime verification (pending — server changed → full rebuild incl. sidecar)

`git pull` → `cargo build -p farder-server` → STOP app → `.\client\src-tauri\binaries\copy-sidecar.ps1` (from repo root) → `cd client; npm run tauri dev` → `Ctrl+Shift+R`. On a **mesh** server:

1. Post an image, then **Remove** it → it becomes "🚫 Removed by the uploader", no longer downloads; a second client sees the same live; restart → still redacted (bytes gone).
2. A second identity posts an image; you (owner/mod) **Take down** their attachment → it shows "🚫 Removed by a moderator" for everyone.
3. A regular member sees no Remove/Take-down control on someone else's attachment.

## Decomposition (for the implementation plan)

1. **4b-1 (crypto):** `AttachmentRedacted` variant + `LogState` uploader/redacted maps + authz + queries (pure, unit-tested).
2. **4b-2 (server):** `files.redacted_by` migration, ingest (mark + GC in-tx), startup sweep, download guard, `AttachmentInfo.redacted_by_moderator`, broadcast.
3. **4b-3 (client):** `redact_attachment` command + bridge, the on-attachment action (gated), the themed placeholder, the live-update listener.

## Carry-forward / known limitations

- Compliant-host takedown only; not Byzantine-proof (a host that kept the bytes can still serve them).
- The `attachment_uploaders` pre-claim residual (above) — niche, requires knowing the hash in advance, takedown-only.
- Same user posting the same file in two messages → redaction removes it from both (shared bytes; owner-accepted).
- Message edit/delete over the log remains future work (separate event).
