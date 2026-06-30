# Mesh Rung 1 — Sub-project 4b: Attachment redaction / takedown — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a member take down a file they uploaded, and a moderator take down any file, via a signed `AttachmentRedacted` log event that deletes the bytes and renders a "removed" placeholder.

**Architecture:** A new `AttachmentRedacted { content_hash }` event is authorized in `LogState` (author is the recorded uploader OR holds `"kick"`; hash known; not already redacted), submitted over the existing `SubmitEvent` path. The server marks `files.redacted_by`, deletes the blob bytes (row stays as a tombstone), blocks download, and broadcasts; the client renders a placeholder distinguishing uploader- vs moderator-removal.

**Tech Stack:** Rust (`farder-crypto`, `farder-server`, `farder-protocol`), rusqlite/SQLite, Tauri commands, React/TypeScript, QUIC.

## Global Constraints

- **Verify-before-done (CLAUDE.md):** compiling + unit-passing is NOT "done"; the frontend↔backend seam and "the security step is actually on the real path" need the owner's Windows runtime test. Mark client runtime UNVERIFIED until then.
- **Seam rule:** every `invoke("X")` has a matching `#[tauri::command] fn X` registered in `client/src-tauri/src/main.rs` `generate_handler!`. New command this plan: `redact_attachment`.
- **Tauri casing:** command-arg names are camelCase in JS, snake_case in Rust (auto-converted). Nested serde structs need `#[serde(rename_all="camelCase")]` only if sent as JS objects. The TS `AttachmentInfo` type deserializes Rust directly and uses **snake_case field names** (`file_id`, `content_hash`, `redacted_by_moderator`).
- **Authoritative gate is the log:** `LogState::apply` is the real authz; client gating is convenience. Moderation capability in the log is the string `"kick"` (owner holds it implicitly via `has_capability`). Client "Take down" gate mirrors the existing kick/ban buttons (`KICK_MEMBERS` bit / `canKick`); self "Remove" gate is `isOwnMessage`.
- **Redaction is permanent:** bytes are deleted; there is no un-redact.
- **Compliant-host takedown only** — not Byzantine-proof. Single-host Rung 1.
- **UI styling (CLAUDE.md):** any new className must be added to all three theme CSS files (`client/src/themes/{xp-luna-blue,discord-dark,hello-kitty}/theme.css`); prefer reusing existing classes (`attachment-image`, `attachment-item`, `context-menu-item`).
- **Docs discipline:** a changed public surface updates its `docs/modules/*.md` (Task 7).
- **Build/test:** `cargo test -p farder-crypto`, `cargo test -p farder-server`, client crate `cd client/src-tauri && cargo build`, frontend `cd client && npx tsc --noEmit`. A new `EventPayload` variant breaks exhaustive matches across the **workspace** — build with `cargo build --workspace`, not just `-p`.

---

### Task 1: Crypto — `AttachmentRedacted` event + LogState authz

**Files:**
- Modify: `crates/farder-crypto/src/event_log.rs` (add the `EventPayload` variant)
- Modify: `crates/farder-crypto/src/event_log_state.rs` (LogState fields, authz, effect, query, tests)
- Modify: `crates/farder-server/src/event_ingest.rs:30-42` (`payload_type` arm — keeps the server crate compiling)

**Interfaces:**
- Produces: `EventPayload::AttachmentRedacted { content_hash: String }`; `LogState::is_attachment_redacted(&self, hash: &str) -> bool`; `LogState::attachment_uploader(&self, hash: &str) -> Option<&PublicKey>`.
- Consumes: existing `LogState` authz pattern (`has_capability`, `is_owner`), `AttachmentCap`.

- [ ] **Step 1: Add the variant**

In `crates/farder-crypto/src/event_log.rs`, in `enum EventPayload` (after `PermissionGranted`):

```rust
    /// Take down an attachment: delete its bytes + mark it redacted. Authorized by
    /// the original uploader or a moderator (holds "kick"). Content-hash-keyed so it
    /// is meaningful across hosts/replication (file_ids are server-local).
    AttachmentRedacted { content_hash: String },
```

- [ ] **Step 2: Add the failing LogState tests**

In `crates/farder-crypto/src/event_log_state.rs` `mod tests`, add (use the module's existing test helpers — find how other tests build a genesis + authorize a device + post; mirror them exactly, e.g. the `MessagePosted`/`MemberApproved` tests already in this file):

```rust
    #[test]
    fn uploader_can_redact_own_attachment() {
        // owner posts a message with an attachment cap (hash "h"), then redacts it.
        let (mut st, owner, dev, mut seq, mut prev, mut lamport) = setup_owner_log();
        let post = next_event(&dev, &owner, st.server_id(), &mut prev, &mut seq, &mut lamport,
            EP::MessagePosted { channel_id: 1, content: "x".into(), reply_to: None,
                attachments: vec![AttachmentCap { content_hash: "h".into(), declared_type: "image/png".into(), size: 1, uploader: owner.public_key() }] });
        st.apply(&post).unwrap();
        let redact = next_event(&dev, &owner, st.server_id(), &mut prev, &mut seq, &mut lamport,
            EP::AttachmentRedacted { content_hash: "h".into() });
        assert!(st.apply(&redact).is_ok());
        assert!(st.is_attachment_redacted("h"));
    }

    #[test]
    fn non_uploader_non_mod_cannot_redact() {
        // owner posts (uploader=owner); a member without "kick" tries to redact → rejected.
        // (build a second member via invite+join as the file's adjacent tests do, then attempt redact)
        // ... assert st.apply(&redact_by_member).is_err()
    }

    #[test]
    fn redacting_unknown_hash_is_rejected() {
        let (mut st, owner, dev, mut seq, mut prev, mut lamport) = setup_owner_log();
        let redact = next_event(&dev, &owner, st.server_id(), &mut prev, &mut seq, &mut lamport,
            EP::AttachmentRedacted { content_hash: "never-posted".into() });
        assert!(st.apply(&redact).is_err());
    }

    #[test]
    fn double_redact_is_rejected() {
        let (mut st, owner, dev, mut seq, mut prev, mut lamport) = setup_owner_log();
        let post = next_event(&dev, &owner, st.server_id(), &mut prev, &mut seq, &mut lamport,
            EP::MessagePosted { channel_id: 1, content: "x".into(), reply_to: None,
                attachments: vec![AttachmentCap { content_hash: "h".into(), declared_type: "image/png".into(), size: 1, uploader: owner.public_key() }] });
        st.apply(&post).unwrap();
        let r1 = next_event(&dev, &owner, st.server_id(), &mut prev, &mut seq, &mut lamport,
            EP::AttachmentRedacted { content_hash: "h".into() });
        st.apply(&r1).unwrap();
        let r2 = next_event(&dev, &owner, st.server_id(), &mut prev, &mut seq, &mut lamport,
            EP::AttachmentRedacted { content_hash: "h".into() });
        assert!(st.apply(&r2).is_err());
    }
```

> Implementer note: this file's tests already construct events. Do NOT invent `setup_owner_log`/`next_event` if they don't exist under those names — use whatever helper pattern the existing `mod tests` uses to (a) make a genesis + owner-as-member state, (b) chain-build a signed event from a device. Match the existing tests verbatim; the four assertions above (uploader-ok, non-mod-rejected, unknown-rejected, double-rejected) are the contract. For the moderator-can-redact case, add a fifth test granting a member `"kick"` via `PermissionGranted` and asserting they can redact another's hash, mirroring how the existing `MemberApproved`/`PermissionGranted` tests set capabilities.

- [ ] **Step 3: Run tests — verify they fail**

Run: `cargo test -p farder-crypto attachment 2>&1 | tail -20`
Expected: FAIL — `no variant named AttachmentRedacted` / `no method is_attachment_redacted`.

- [ ] **Step 4: Add LogState fields**

In `event_log_state.rs`, add to `struct LogState` (after `chains`):

```rust
    /// content_hash -> first uploader seen (from MessagePosted caps); authz basis for self-takedown.
    attachment_uploaders: HashMap<String, PublicKey>,
    /// content hashes that have been redacted.
    redacted_attachments: HashSet<String>,
```

In `from_genesis`, initialize both:

```rust
            attachment_uploaders: HashMap::new(),
            redacted_attachments: HashSet::new(),
```

- [ ] **Step 5: Add the queries**

After `has_capability` (around line 95):

```rust
    /// Whether this attachment (by content hash) has been redacted.
    pub fn is_attachment_redacted(&self, hash: &str) -> bool {
        self.redacted_attachments.contains(hash)
    }
    /// The recorded (first) uploader of an attachment hash, if any MessagePosted cited it.
    pub fn attachment_uploader(&self, hash: &str) -> Option<&PublicKey> {
        self.attachment_uploaders.get(hash)
    }
```

- [ ] **Step 6: Record uploaders on MessagePosted (effect) + add the redaction authz + effect**

In `apply_payload_effect`, replace the MessagePosted arm:

```rust
            EventPayload::MessagePosted { .. } => {} // no authz-state change
```

with:

```rust
            EventPayload::MessagePosted { attachments, .. } => {
                for cap in attachments {
                    self.attachment_uploaders
                        .entry(cap.content_hash.clone())
                        .or_insert_with(|| cap.uploader.clone());
                }
            }
```

In `check_payload_authz`, add an arm (before the closing `}` of the match):

```rust
            EventPayload::AttachmentRedacted { content_hash } => {
                let uploader = self
                    .attachment_uploaders
                    .get(content_hash)
                    .context("redaction cites an unknown attachment")?;
                ensure!(
                    author == uploader || self.has_capability(author, "kick"),
                    "must be the uploader or hold 'kick'"
                );
                ensure!(
                    !self.redacted_attachments.contains(content_hash),
                    "attachment already redacted"
                );
                Ok(())
            }
```

In `apply_payload_effect`, add an arm:

```rust
            EventPayload::AttachmentRedacted { content_hash } => {
                self.redacted_attachments.insert(content_hash.clone());
            }
```

- [ ] **Step 7: Keep the server crate compiling — `payload_type` arm**

In `crates/farder-server/src/event_ingest.rs`, in `fn payload_type`, add:

```rust
        EventPayload::AttachmentRedacted { .. } => "AttachmentRedacted",
```

(`store_event`'s `channel_id` match has a `_ => None` arm, so it needs no change.)

- [ ] **Step 8: Run tests — verify they pass + workspace builds**

Run: `cargo test -p farder-crypto attachment 2>&1 | tail -20` → PASS (all five redaction tests).
Run: `cargo build --workspace 2>&1 | tail -5` → builds (exhaustive matches updated).

- [ ] **Step 9: Commit**

```bash
git add crates/farder-crypto/src/event_log.rs crates/farder-crypto/src/event_log_state.rs crates/farder-server/src/event_ingest.rs
git commit -m "feat(mesh-4b): AttachmentRedacted event + LogState redaction authz"
```

---

### Task 2: Server — `files.redacted_by`, blob redaction, read-model fields

**Files:**
- Modify: `crates/farder-server/src/db.rs:~356` (migration: add `files.redacted_by`)
- Modify: `crates/farder-protocol/src/server.rs` (`AttachmentInfo` gains `content_hash` + `redacted_by_moderator`)
- Modify: `crates/farder-server/src/attachments.rs` (add `redact_blob`; populate the two new fields in both `get_attachments_for_*`)

**Interfaces:**
- Produces: `attachments::redact_blob(conn, storage_dir, content_hash, redactor) -> Result<bool>` (marks `redacted_by`, deletes bytes, returns whether a row was found); `AttachmentInfo { ..., content_hash: String, redacted_by_moderator: Option<bool> }`.
- Consumes: `FileRecord`, `content_path`.

- [ ] **Step 1: Migration — add `files.redacted_by`**

In `crates/farder-server/src/db.rs`, after the `messages.event_hash` migration block (~line 356), add:

```rust
    // Migration: attachment redaction — who took the blob down (NULL = live).
    let has_redacted_by: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(files)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols.iter().any(|c| c == "redacted_by")
    };
    if !has_redacted_by {
        conn.execute("ALTER TABLE files ADD COLUMN redacted_by BLOB", [])?;
    }
```

- [ ] **Step 2: Extend `AttachmentInfo` (protocol)**

In `crates/farder-protocol/src/server.rs`, add two fields to `struct AttachmentInfo` (after `duration_secs`):

```rust
    /// Hex SHA-256 of the bytes — lets the client build an AttachmentRedacted event.
    #[serde(default)]
    pub content_hash: String,
    /// Redaction state: None = live; Some(false) = removed by the uploader;
    /// Some(true) = removed by a moderator (redactor != original uploader).
    #[serde(default)]
    pub redacted_by_moderator: Option<bool>,
```

- [ ] **Step 3: Add the failing test for `redact_blob`**

In `crates/farder-server/src/attachments.rs` `mod tests` (mirror existing helpers that insert a file + write its bytes), add:

```rust
    #[test]
    fn redact_blob_marks_and_deletes_bytes() {
        let (conn, dir) = test_db_and_dir(); // use whatever the file's tests use to get a conn + storage_dir
        let uploader = farder_crypto::identity::Keypair::generate().public_key();
        // store a real blob so bytes exist on disk
        let fid = store_file(&conn, dir.path().to_str().unwrap(), &uploader, "f.png", b"hello", "image/png", None, None, None).unwrap();
        let rec = get_file(&conn, fid).unwrap().unwrap();
        assert!(content_path(dir.path().to_str().unwrap(), &rec.hash).exists());
        let mod_pk = farder_crypto::identity::Keypair::generate().public_key();
        assert!(redact_blob(&conn, dir.path().to_str().unwrap(), &rec.hash, &mod_pk).unwrap());
        // bytes gone; row stays with redacted_by set
        assert!(!content_path(dir.path().to_str().unwrap(), &rec.hash).exists());
        let redacted_by: Option<Vec<u8>> = conn.query_row(
            "SELECT redacted_by FROM files WHERE hash = ?1", params![rec.hash], |r| r.get(0)).unwrap();
        assert_eq!(redacted_by.as_deref(), Some(mod_pk.as_bytes().as_slice()));
    }
```

> Implementer note: use the exact blob-insert helper the existing `attachments.rs` tests use (e.g. `store_file`/`store_or_reuse_from_temp_file`) and the same `tempfile`/dir helper pattern. The contract: after `redact_blob`, the on-disk bytes are gone and `files.redacted_by` is the redactor's key, while the row remains.

- [ ] **Step 4: Run — verify it fails**

Run: `cargo test -p farder-server redact_blob 2>&1 | tail -15`
Expected: FAIL — `cannot find function redact_blob`.

- [ ] **Step 5: Implement `redact_blob`**

In `attachments.rs` (near `cleanup_orphaned_file`):

```rust
/// Mark a blob redacted (records who) and delete its bytes from disk. The `files`
/// row stays as a tombstone so message_attachments joins still resolve (and render
/// the "removed" placeholder). Returns true if a matching row existed. Idempotent:
/// re-running after the bytes are already gone just re-asserts redacted_by.
pub fn redact_blob(
    conn: &Connection,
    storage_dir: &str,
    content_hash: &str,
    redactor: &PublicKey,
) -> Result<bool> {
    let updated = conn.execute(
        "UPDATE files SET redacted_by = ?1 WHERE hash = ?2",
        params![redactor.as_bytes().as_slice(), content_hash],
    )?;
    if updated == 0 {
        return Ok(false);
    }
    let path = content_path(storage_dir, content_hash);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(true)
}
```

- [ ] **Step 6: Populate the new `AttachmentInfo` fields in both queries**

In `get_attachments_for_message`, change the SELECT to add `f.hash`, `f.redacted_by`, `f.uploaded_by`:

```rust
        "SELECT ma.id, ma.file_id, ma.original_name, f.size, f.mime_type, \
                ma.width, ma.height, ma.duration_secs, f.hash, f.redacted_by, f.uploaded_by \
         FROM message_attachments ma \
         JOIN files f ON f.id = ma.file_id \
         WHERE ma.message_id = ?1 \
         ORDER BY ma.position ASC",
```

and in the row closure, read them and compute the flag:

```rust
        let duration_secs: Option<f64> = row.get(7)?;
        let content_hash: String = row.get(8)?;
        let redacted_by: Option<Vec<u8>> = row.get(9)?;
        let uploaded_by: Vec<u8> = row.get(10)?;
        let redacted_by_moderator = redacted_by.map(|r| r != uploaded_by);
        Ok(AttachmentInfo {
            id: id as u64,
            file_id: file_id as u64,
            name,
            size: size as u64,
            mime_type,
            width: width.map(|v| v as u32),
            height: height.map(|v| v as u32),
            duration_secs,
            content_hash,
            redacted_by_moderator,
        })
```

Apply the identical change to `get_attachments_for_messages` (its SELECT adds the same three columns shifted by one index because it also selects `ma.message_id` first; read `content_hash` at index 9, `redacted_by` at 10, `uploaded_by` at 11, and add the same two fields to the `AttachmentInfo`).

- [ ] **Step 7: Run — verify pass + workspace builds**

Run: `cargo test -p farder-server redact_blob 2>&1 | tail -10` → PASS.
Run: `cargo build --workspace 2>&1 | tail -5` → builds (every `AttachmentInfo { .. }` construction now needs the two new fields — the `#[serde(default)]` does NOT help struct literals, so fix any other construction site the compiler flags by adding `content_hash`/`redacted_by_moderator`; grep `AttachmentInfo {` to find them).

- [ ] **Step 8: Commit**

```bash
git add crates/farder-server/src/db.rs crates/farder-protocol/src/server.rs crates/farder-server/src/attachments.rs
git commit -m "feat(mesh-4b): files.redacted_by + redact_blob + AttachmentInfo redaction fields"
```

---

### Task 3: Server — ingest redaction in SubmitEvent, download guard, broadcast, sweep

**Files:**
- Modify: `crates/farder-protocol/src/server.rs` (`ServerEvent::AttachmentRedacted`)
- Modify: `crates/farder-server/src/handlers.rs:1814-1888` (SubmitEvent: ingest + broadcast)
- Modify: `crates/farder-server/src/connection.rs:213-234` (download guard)
- Modify: `crates/farder-server/src/event_ingest.rs` (startup sweep `sweep_redacted_bytes`)
- Modify: `crates/farder-server/src/main.rs:~116` (call the sweep)

**Interfaces:**
- Consumes: `attachments::redact_blob` (Task 2); `LogState::attachment_uploader` (Task 1); the existing SubmitEvent tx + broadcast.
- Produces: `ServerEvent::AttachmentRedacted { content_hash: String, by_moderator: bool }`; `event_ingest::sweep_redacted_bytes(conn, storage_dir) -> Result<usize>`.

- [ ] **Step 1: Add the broadcast event variant**

In `crates/farder-protocol/src/server.rs`, in `enum ServerEvent`, add:

```rust
    /// An attachment was taken down (bytes gone). Clients flip its placeholder.
    AttachmentRedacted { content_hash: String, by_moderator: bool },
```

- [ ] **Step 2: Add the failing integration test**

In `handlers.rs` SubmitEvent test section (copy the harness from the nearest `submit_event_*` test, same as 4a Task 2 did — `state.genesis` + `state.log_state` both set, owner device authorized, channel 1, a blob inserted, a MessagePosted with a matching cap already accepted so the message_attachments row + `attachment_uploaders` entry exist):

```rust
    #[test]
    fn submit_event_attachment_redacted_takes_down_blob() {
        // ... harness: genesis+owner device auth; insert blob hash 'cafef00d' uploaded_by owner;
        //     submit a MessagePosted citing that cap (materializes the attachment + records uploader) ...
        // Now submit AttachmentRedacted{ content_hash: "cafef00d" } by the owner:
        let redact = Event::next(&dev, owner.public_key(), genesis.server_id(), Some(&prev), seq, lamport,
            EP::AttachmentRedacted { content_hash: "cafef00d".into() });
        let result = handle_request(&state, &owner.public_key(), true,
            ServerRequest::SubmitEvent { event: redact }).await;
        assert!(matches!(result.response, ServerResponse::EventAccepted { .. }));
        // files.redacted_by set, message_attachments row still present.
        let db = state.db.lock().unwrap();
        let redacted_by: Option<Vec<u8>> = db.query_row(
            "SELECT redacted_by FROM files WHERE hash = 'cafef00d'", [], |r| r.get(0)).unwrap();
        assert!(redacted_by.is_some());
        let att_count: i64 = db.query_row(
            "SELECT COUNT(*) FROM message_attachments ma JOIN files f ON f.id=ma.file_id WHERE f.hash='cafef00d'",
            [], |r| r.get(0)).unwrap();
        assert_eq!(att_count, 1);
    }
```

> Implementer note: reuse the exact `handle_request`/harness names from the adjacent `submit_event_*` tests (4a's `submit_event_with_valid_attachment_materializes_row` is the closest template — it already sets up genesis+blob+MessagePosted). Chain the redaction event from the message's chain head.

- [ ] **Step 3: Run — verify it fails**

Run: `cargo test -p farder-server submit_event_attachment_redacted 2>&1 | tail -15`
Expected: FAIL — `redacted_by` is None (handler doesn't ingest redaction yet).

- [ ] **Step 4: Ingest redaction in the SubmitEvent handler**

In `handlers.rs`, the SubmitEvent arm: the `LogState::apply` on the `trial` clone already authorizes `AttachmentRedacted` (Task 1). After the existing persist `tx` block (which does `store_event` + message derivation) and BEFORE/at the broadcast section, add redaction handling. Inside the existing transaction block, after `store_event`, add (so the event row + the `redacted_by` mark commit together):

```rust
                if let EventPayload::AttachmentRedacted { content_hash } = &event.core.payload {
                    crate::attachments::redact_blob(&tx, &state.storage_dir, content_hash, &event.core.author)
                        .map_err(|e| anyhow::anyhow!("failed to redact attachment: {}", e))?;
                }
```

Then in the broadcast section (where `membership_pk` / `NewMessage` events are pushed), add an `AttachmentRedacted` broadcast. Compute `by_moderator` from the trial state's recorded uploader:

```rust
            if let EventPayload::AttachmentRedacted { content_hash } = &event.core.payload {
                let by_moderator = trial
                    .attachment_uploader(content_hash)
                    .map(|up| up != &event.core.author)
                    .unwrap_or(true);
                events.push(BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::AttachmentRedacted { content_hash: content_hash.clone(), by_moderator },
                });
            }
```

> Note: `trial` is moved into `*ls_guard = Some(trial)` in the existing code. Read `attachment_uploader` from `trial` BEFORE that move (compute `by_moderator` earlier and keep it in a local), or read from `ls_guard.as_ref()` after the commit. Pick whichever keeps the borrow checker happy; the value is a bool.

- [ ] **Step 5: Download guard**

In `connection.rs` `handle_download_stream`, right after the file record is loaded (the `let file = match file { Some(f) => f, None => { ...uniform "not available"... } };` block, ~line 234), add a redaction check. `FileRecord` does not currently carry `redacted_by`; the simplest is a direct query here:

```rust
    // Redacted blobs behave exactly like absent ones (uniform "not available").
    let is_redacted: bool = {
        let db = state.db.lock().unwrap();
        db.query_row(
            "SELECT redacted_by IS NOT NULL FROM files WHERE id = ?1",
            rusqlite::params![req.file_id as i64],
            |r| r.get::<_, bool>(0),
        ).optional()?.unwrap_or(false)
    };
    if is_redacted {
        let resp = codec::encode(&DownloadResponse::Error { reason: "not available".to_string() })?;
        write_frame(&mut send, &resp).await?;
        return Ok(());
    }
```

(Place it before the permission query so a redacted file is uniformly "not available" regardless of permission. Confirm `OptionalExtension` is in scope in connection.rs; if not, use `.optional()` via `rusqlite::OptionalExtension` import or restructure with `query_row` + match.)

- [ ] **Step 6: Startup sweep**

In `event_ingest.rs`, add:

```rust
/// Delete on-disk bytes for any blob already marked redacted (heals a crash between
/// the redacted_by mark and the byte delete). Idempotent. Returns bytes-deleted count.
pub fn sweep_redacted_bytes(conn: &Connection, storage_dir: &str) -> Result<usize> {
    let hashes: Vec<String> = {
        let mut stmt = conn.prepare("SELECT hash FROM files WHERE redacted_by IS NOT NULL")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut v = Vec::new();
        for row in rows { v.push(row?); }
        v
    };
    let mut deleted = 0;
    for hash in hashes {
        let path = crate::attachments::content_path(storage_dir, &hash);
        if path.exists() {
            std::fs::remove_file(&path)?;
            deleted += 1;
        }
    }
    Ok(deleted)
}
```

In `main.rs`, after the `reconcile_attachments` call (~line 116), add:

```rust
            let swept = farder_server::event_ingest::sweep_redacted_bytes(&conn, &server_state.storage_dir).unwrap_or(0);
            if swept > 0 { tracing::info!(count = swept, "swept bytes of already-redacted attachments"); }
```

(Use the correct storage-dir accessor in scope at that point — match how `storage_dir` is referenced elsewhere in `main.rs` startup; if it's on `server_state`, use that.)

- [ ] **Step 7: Run — verify pass + workspace builds**

Run: `cargo test -p farder-server submit_event_attachment_redacted 2>&1 | tail -10` → PASS.
Run: `cargo build --workspace 2>&1 | tail -5` → builds (the new `ServerEvent` variant: check `client/src-tauri/src/bridge.rs` match is non-exhaustive-safe or add the arm in Task 5; if the server-side bridge match over `ServerEvent` is exhaustive it must get the arm now — grep `ServerEvent::` in `crates/farder-server` for any exhaustive match and add a no-op/translate arm to compile).

- [ ] **Step 8: Commit**

```bash
git add crates/farder-protocol/src/server.rs crates/farder-server/src/handlers.rs crates/farder-server/src/connection.rs crates/farder-server/src/event_ingest.rs crates/farder-server/src/main.rs
git commit -m "feat(mesh-4b): SubmitEvent redaction ingest + download guard + broadcast + sweep"
```

---

### Task 4: Client (Rust + TS bridge) — `redact_attachment` command + types

**Files:**
- Modify: `client/src-tauri/src/commands.rs` (add `redact_attachment`, mirror `submit_event`)
- Modify: `client/src-tauri/src/main.rs` (register in `generate_handler!`)
- Modify: `client/src/lib/tauri-bridge.ts` (`redactAttachment`)
- Modify: `client/src/lib/types.ts:43-52` (`AttachmentInfo` gains `content_hash` + `redacted_by_moderator`)

**Interfaces:**
- Produces: Tauri `redact_attachment(server_id, log_server_id, content_hash)`; bridge `redactAttachment(serverId, logServerId, contentHash)`; TS `AttachmentInfo.content_hash`, `.redacted_by_moderator`.
- Consumes: the `event_build_next` + `event_send_submit` + `device_chain_lock` pattern from `submit_event` (4a, commands.rs ~3795).

- [ ] **Step 1: Add the `redact_attachment` command**

In `client/src-tauri/src/commands.rs`, add a command mirroring `submit_event` (the chain load → ensure DeviceAuthorized → build+sign → SubmitEvent → advance-on-accept pattern). Reuse the `event_build_next`, `event_send_submit`, and `device_chain_lock` helpers `submit_event` uses:

```rust
#[tauri::command]
pub async fn redact_attachment(
    state: State<'_, Arc<AppState>>,
    server_id: String,     // connection key (address) — routes the request
    log_server_id: String, // genesis hash — stamps EventCore.server_id + keys the device chain
    content_hash: String,
) -> Result<EventAcceptedResult, String> {
    // ... identical preamble to submit_event: acquire device_chain_lock, load identity (err if locked),
    //     load device key + DeviceState, ensure DeviceAuthorized (advance on accept) ...
    let redact = event_build_next(
        &device, &identity, &log_server_id,
        ds.last_event_hash.clone(), ds.next_seq, ds.lamport,
        EventPayload::AttachmentRedacted { content_hash },
    );
    let result = event_send_submit(&state, &server_id, &redact).await?;
    ds.next_seq = redact.core.seq + 1;
    ds.last_event_hash = Some(redact.hash());
    ds.lamport = redact.core.lamport;
    ds.save(&log_server_id)?;
    Ok(result)
}
```

> Implementer note: factor out or copy `submit_event`'s exact preamble (the lock acquisition, identity/device load, and first-time `DeviceAuthorized` submission). Do NOT diverge from its chain-advance discipline (advance + persist DeviceState ONLY on `EventAccepted`). If `submit_event`'s body is large, extract the shared chain-submit preamble into a helper and call it from both — but keep behavior identical.

- [ ] **Step 2: Register in `generate_handler!`**

In `client/src-tauri/src/main.rs`, add `commands::redact_attachment,` to the `generate_handler!` list (next to `commands::submit_event,`).

- [ ] **Step 3: Bridge function**

In `client/src/lib/tauri-bridge.ts`, add (near `submitEvent`):

```ts
export async function redactAttachment(serverId: string, logServerId: string, contentHash: string): Promise<{ event_hash: string; timestamp: number }> {
  return invoke("redact_attachment", { serverId, logServerId, contentHash });
}
```

- [ ] **Step 4: Extend the TS `AttachmentInfo` type**

In `client/src/lib/types.ts`, in `interface AttachmentInfo` (after `duration_secs`):

```ts
  content_hash?: string;
  redacted_by_moderator?: boolean | null;
```

- [ ] **Step 5: Build + seam check**

Run: `cd client/src-tauri && cargo build 2>&1 | tail -5` → builds.
Run: `grep -n 'redact_attachment' client/src-tauri/src/main.rs` → appears in `generate_handler!`.
Run: `cd client && npx tsc --noEmit 2>&1 | tail -5` → clean.

- [ ] **Step 6: Commit**

```bash
git add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs client/src/lib/tauri-bridge.ts client/src/lib/types.ts
git commit -m "feat(mesh-4b): redact_attachment command + bridge + AttachmentInfo TS fields"
```

---

### Task 5: Client — live update plumbing (`server:attachment_redacted`)

**Files:**
- Modify: `client/src-tauri/src/bridge.rs:~70` (emit `server:attachment_redacted`)
- Modify: `client/src/hooks/useServerEvents.ts:~188` (listen)
- Modify: `client/src/context/ServerContext.tsx:~96,~224` (action + reducer)

**Interfaces:**
- Consumes: `ServerEvent::AttachmentRedacted { content_hash, by_moderator }` (Task 3).
- Produces: an `ATTACHMENT_REDACTED` reducer action that sets `redacted_by_moderator` on every attachment whose `content_hash` matches, across all loaded messages.

- [ ] **Step 1: Emit the Tauri event (Rust bridge)**

In `client/src-tauri/src/bridge.rs`, in the `ServerEvent` → Tauri-emit match (where `server:message_deleted` etc. are emitted, ~line 70), add:

```rust
        ServerEvent::AttachmentRedacted { content_hash, by_moderator } =>
            app.emit("server:attachment_redacted", serde_json::json!({ "server_id": sid, "content_hash": content_hash, "by_moderator": by_moderator })),
```

- [ ] **Step 2: Listen (frontend hook)**

In `client/src/hooks/useServerEvents.ts`, mirror the `server:message_deleted` listener (~line 188):

```ts
    listen("server:attachment_redacted", (e) => {
      const data = e.payload as { server_id: string; content_hash: string; by_moderator: boolean };
      if (data.server_id !== activeRef.current) return;
      dispatch({ type: "ATTACHMENT_REDACTED", serverId: data.server_id, payload: { contentHash: data.content_hash, byModerator: data.by_moderator } });
    }).then(safePush);
```

- [ ] **Step 3: Reducer action + case**

In `client/src/context/ServerContext.tsx`, add the action type (near line 96, with the other action types):

```ts
  | { type: "ATTACHMENT_REDACTED"; serverId: string; payload: { contentHash: string; byModerator: boolean } }
```

and a reducer case (near the `MESSAGE_EDITED`/`MESSAGE_DELETED` cases, ~line 224) that maps in place over the active server's messages, flipping the flag on matching attachments:

```ts
    case "ATTACHMENT_REDACTED": {
      const srv = state.servers[action.serverId];
      if (!srv) return state;
      const messages: typeof srv.messages = {};
      for (const [chId, msgs] of Object.entries(srv.messages)) {
        messages[chId] = msgs.map((m) => {
          if (!m.attachments?.some((a) => a.content_hash === action.payload.contentHash)) return m;
          return { ...m, attachments: m.attachments.map((a) =>
            a.content_hash === action.payload.contentHash
              ? { ...a, redacted_by_moderator: action.payload.byModerator }
              : a) };
        });
      }
      return { ...state, servers: { ...state.servers, [action.serverId]: { ...srv, messages } } };
    }
```

> Implementer note: match the EXACT shape of per-server state in this reducer (how `messages` is keyed — `state.servers[id].messages[channelId]` per 4a memory). If the message/attachment field names differ, adapt to the real types; the contract is "set `redacted_by_moderator` on every attachment whose `content_hash` matches, leave everything else untouched." Mirror `MESSAGE_EDITED`'s in-place map (ServerContext.tsx ~213-223).

- [ ] **Step 4: Type-check + build**

Run: `cd client && npx tsc --noEmit 2>&1 | tail -5` → clean.
Run: `cd client/src-tauri && cargo build 2>&1 | tail -5` → builds (bridge emit arm added).

- [ ] **Step 5: Commit**

```bash
git add client/src-tauri/src/bridge.rs client/src/hooks/useServerEvents.ts client/src/context/ServerContext.tsx
git commit -m "feat(mesh-4b): live server:attachment_redacted update plumbing"
```

---

### Task 6: Client — placeholder render + take-down action + themes

**Files:**
- Modify: `client/src/components/Message.tsx` (`AttachmentDisplay` ~635-799: placeholder branch + context-menu item; pass mod/own props from the call site ~453)
- Modify: `client/src/themes/{xp-luna-blue,discord-dark,hello-kitty}/theme.css` (placeholder class if a new one is needed)

**Interfaces:**
- Consumes: `redactAttachment` (Task 4), `AttachmentInfo.redacted_by_moderator`/`content_hash` (Task 4), `PERMISSIONS`/`hasPermission` (`client/src/lib/permissions.ts`), `activeServer.logServerId`.

- [ ] **Step 1: Placeholder render**

In `AttachmentDisplay` (`Message.tsx` ~635), at the very top of the render (before the `isImage`/gated/loading branches), add:

```tsx
  if (attachment.redacted_by_moderator != null) {
    const who = attachment.redacted_by_moderator ? "a moderator" : "the uploader";
    return (
      <div className="attachment-item">
        <span>🚫</span>
        <span>Removed by {who}</span>
      </div>
    );
  }
```

(Reuses the existing `.attachment-item` chip class — already styled in every theme, so no new CSS is strictly required. If the design wants a distinct muted look, add an `.attachment-redacted` class to all three theme files driven by `var(--xp-text-muted)`/`var(--xp-border)`; otherwise reuse `.attachment-item`.)

- [ ] **Step 2: Add the take-down / remove context-menu item**

`AttachmentDisplay` needs to know: is this a mesh server (`logServerId`), can the viewer moderate (`canTakeDown`), is it the viewer's own message (`isOwnMessage`). These are computed in `Message.tsx` already (`viewerBits` at ~282, `isOwnMessage` at ~242, `activeServer?.logServerId`). Pass them into `AttachmentDisplay` as props (the component is rendered at ~453). Compute in the parent:

```tsx
  const canTakeDown = hasPermission(viewerBits, PERMISSIONS.KICK_MEMBERS); // mirrors kick/ban gate -> log "kick"
  const logServerId = activeServer?.logServerId ?? null;
```

(Import `PERMISSIONS` and `hasPermission` from `../lib/permissions` — `getActorPermissions`/`isModerator` are already imported.)

In `AttachmentDisplay`'s existing context menu (the `.context-menu` rendered ~763-787), add an item — shown only on a mesh server, for a not-yet-redacted attachment, when the viewer is the uploader (own message) or can take down:

```tsx
  {logServerId && attachment.redacted_by_moderator == null && attachment.content_hash && (isOwnMessage || canTakeDown) && (
    <div className="context-menu-item" onClick={async () => {
      try { await api.redactAttachment(serverId, logServerId, attachment.content_hash!); }
      catch (e) { console.error("[attachment:redact]", e); }
      setMenu(null);
    }}>
      {isOwnMessage ? "Remove" : "Take down"}
    </div>
  )}
```

> Implementer note: wire the new props (`logServerId`, `canTakeDown`, `isOwnMessage`) through `AttachmentDisplay`'s prop type and the call site at `Message.tsx:453-459`. Match the existing context-menu item style (`.context-menu-item`, the `.context-menu-divider` separators already there). `serverId` is already available in `AttachmentDisplay` (used for download/save). Use the same `setMenu(null)` close used by the sibling items.

- [ ] **Step 3: Theme check (only if a new class was added)**

If you added `.attachment-redacted`: Run `grep -l "attachment-redacted" client/src/themes/*/theme.css` → must list all three. If you reused `.attachment-item`, skip.

- [ ] **Step 4: Type-check**

Run: `cd client && npx tsc --noEmit 2>&1 | tail -5` → clean.

- [ ] **Step 5: Commit**

```bash
git add client/src/components/Message.tsx client/src/themes/
git commit -m "feat(mesh-4b): redacted-attachment placeholder + take-down action"
```

---

### Task 7: Docs

**Files:**
- Modify: `docs/modules/tauri-commands.md` (`redact_attachment`)
- Modify: the frontend bridge doc (`redactAttachment` + `AttachmentInfo.content_hash`/`redacted_by_moderator`)
- Modify: a server doc (`server-handlers.md` AttachmentRedacted ingest + broadcast; `server-connection.md` download guard)
- Modify: the crypto/event-log doc (the new event + authz + LogState maps)
- Modify: `ARCHITECTURE.md` (the redaction flow)

- [ ] **Step 1: Document the command + bridge + type**

`tauri-commands.md`: add `redact_attachment(server_id, log_server_id, content_hash) -> EventAcceptedResult` (builds + submits a signed `AttachmentRedacted` event). Bridge doc: `redactAttachment(serverId, logServerId, contentHash)` and the two new `AttachmentInfo` fields (`content_hash`, `redacted_by_moderator: None=live / false=uploader / true=moderator`).

- [ ] **Step 2: Document server + crypto + architecture**

Server doc: `AttachmentRedacted` ingest sets `files.redacted_by` + deletes bytes (tombstone row), download of a redacted blob returns uniform "not available", `ServerEvent::AttachmentRedacted{content_hash,by_moderator}` broadcast, startup `sweep_redacted_bytes`. Crypto/event-log doc: the new event, authz (uploader OR `"kick"`, hash known, not already redacted), and the `attachment_uploaders`/`redacted_attachments` LogState maps. `ARCHITECTURE.md`: add the takedown flow (uploader/mod → signed AttachmentRedacted → bytes deleted → placeholder).

- [ ] **Step 3: Commit**

```bash
git add docs/
git commit -m "docs(mesh-4b): attachment redaction command/event/flow"
```

---

## Owner runtime verification (REQUIRED before "done" — server changed → full rebuild incl. sidecar)

`git pull` → `cargo build -p farder-server` → STOP app → `.\client\src-tauri\binaries\copy-sidecar.ps1` (from repo root) → `cd client; npm run tauri dev` → `Ctrl+Shift+R`. On a **mesh** server:

1. Post an image → right-click it → **Remove** → it becomes "🚫 Removed by the uploader", no longer downloads; a 2nd client sees the same live; restart the app → still redacted (bytes gone).
2. A 2nd identity posts an image; you (owner) right-click → **Take down** → it shows "🚫 Removed by a moderator" for everyone.
3. A regular member sees no Remove/Take-down item on someone else's attachment.

## Self-review notes (coverage vs spec)

- Spec "AttachmentRedacted event + authz (uploader OR kick, hash known, not already redacted)" → Task 1.
- Spec "LogState folds content_hash→uploader + redacted set; replay-deterministic" → Task 1 (effect on MessagePosted + apply + tests incl. implicit replay via apply sequence).
- Spec "files.redacted_by column; delete bytes; tombstone row; record who" → Task 2 (`redact_blob`) + Task 3 (ingest).
- Spec "download of redacted → uniform not available" → Task 3 download guard.
- Spec "AttachmentInfo.redacted_by_moderator (uploader vs mod)" → Task 2; plus `content_hash` added (needed client-side to build the event — refinement over the spec, same intent).
- Spec "broadcast live update" → Task 3 (`ServerEvent::AttachmentRedacted`) + Task 5 (plumbing).
- Spec "client redact command + on-attachment action gated (uploader / mod), placeholder, themed" → Tasks 4/6.
- Spec "startup sweep" → Task 3 (`sweep_redacted_bytes`).
- Out of scope (no tasks): message edit/delete, un-redaction, byte-sniffing, richer moderation states. Correct.
- Refinement vs spec, recorded: redaction reuses `ServerRequest::SubmitEvent` (no new request); client moderation gate is `KICK_MEMBERS`/`canKick` (aligns with the log `"kick"` authz), not `MANAGE_MESSAGES`.
