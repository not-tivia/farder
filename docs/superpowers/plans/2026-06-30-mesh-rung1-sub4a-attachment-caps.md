# Mesh Rung 1 — Sub-project 4a: Attachments over the event log — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a message with attachments post as a signed `MessagePosted` event over the mesh log, validating each `AttachmentCap` against the actual stored blob before the attachment is materialized or downloadable.

**Architecture:** The crypto `MessagePosted.attachments: Vec<AttachmentCap>` already exists but is unused. Server-side, the `SubmitEvent` ingest derives a `message_attachments` row only for caps whose `content_hash` resolves to a stored blob whose `size`/`mime_type`/`uploaded_by` match and whose poster is the uploader or owner — inside the existing ingest transaction. Client-side, the upload path surfaces the cap fields, `submit_event` builds caps into the event, and `MessageInput` routes staged-file mesh messages over the log. Download gating already exists (channel-view + mesh-member); this hardens its error responses to be uniform.

**Tech Stack:** Rust (`farder-server`, `farder-crypto`), rusqlite/SQLite, Tauri commands, React/TypeScript, QUIC (quinn).

## Global Constraints

- **Verify-before-done (CLAUDE.md):** code that compiles + unit-passes is NOT "done". The frontend↔backend seam and "is the security step actually on the real path" must be verified at runtime by the owner (Windows). Mark client runtime behavior UNVERIFIED until then.
- **Frontend↔backend seam:** every `invoke("X")` name must have a matching `#[tauri::command] fn X` registered in `client/src-tauri/src/main.rs` `generate_handler!`. `submit_event` (main.rs:258) and `upload_file` (main.rs:126) are already registered; this plan changes their signatures, not their names.
- **Tauri arg casing:** command-arg names convert snake_case↔camelCase automatically, but nested serde structs do NOT — `UploadOutcome` and `AttachmentCapInput` carry `#[serde(rename_all = "camelCase")]` so TS uses `fileId`/`contentHash`/`declaredType`/`size`.
- **Docs discipline (CLAUDE.md):** a changed public surface updates its `docs/modules/*.md` in the same commit (Task 7).
- **Single-host Rung 1:** the blob is always uploaded before the event is submitted, so a missing/mismatched blob means a misbehaving client; the response is to quarantine (not materialize), never to reject the signed event.
- **Build/test:** server tests `cargo test -p farder-server`; crypto unchanged; client crate `cd client/src-tauri && cargo build`; frontend `cd client && npx tsc --noEmit`.

---

### Task 1: Server — `derive_attachments` (validate caps + materialize rows)

**Files:**
- Modify: `crates/farder-server/src/event_ingest.rs` (add import; add `derive_attachments`; add tests)

**Interfaces:**
- Consumes: `farder_crypto::event_log::{Event, EventPayload, AttachmentCap}`; `farder_crypto::identity::PublicKey`; `crate::attachments::{get_file_by_hash, create_message_attachment, FileRecord}`.
- Produces: `pub fn derive_attachments(conn: &Connection, message_id: u64, event: &Event, owner: &PublicKey) -> Result<usize>` — materializes a `message_attachments` row per VALID cap, returns the count newly created; idempotent.

- [ ] **Step 1: Add the failing tests**

In `crates/farder-server/src/event_ingest.rs`, inside `mod tests`, add a file-insert helper and the validation tests. Place the helper after the existing `genesis(...)` helper:

```rust
    fn insert_file(conn: &Connection, hash: &str, size: u64, mime: &str, uploader: &farder_crypto::identity::PublicKey) -> u64 {
        conn.execute(
            "INSERT INTO files (hash, size, mime_type, original_name, uploaded_by, uploaded_at, ref_count) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
            params![hash, size as i64, mime, "f.png", uploader.as_bytes().as_slice(), 1i64],
        ).unwrap();
        conn.last_insert_rowid() as u64
    }

    /// Build genesis + a derived message row, returning (conn, owner, message_id, msg_event).
    fn setup_message(conn: &Connection, owner: &Keypair, dev: &Keypair, author: &Keypair, caps: Vec<AttachmentCap>) -> (u64, Event) {
        let g = genesis(owner);
        save_genesis(conn, &g).unwrap();
        let msg = Event::next(dev, author.public_key(), g.server_id(), None, 0, 1,
            EP::MessagePosted { channel_id: 1, content: "hi".into(), reply_to: None, attachments: caps });
        store_event(conn, &msg).unwrap();
        let mid = derive_message_row(conn, &msg).unwrap().unwrap();
        (mid, msg)
    }

    fn attachment_count(conn: &Connection, message_id: u64) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM message_attachments WHERE message_id = ?1",
            params![message_id as i64], |r| r.get(0)).unwrap()
    }

    #[test]
    fn valid_cap_is_materialized() {
        let conn = crate::db::open_in_memory().unwrap();
        let owner = Keypair::generate();
        let dev = Keypair::generate();
        insert_file(&conn, "abc123", 42, "image/png", &owner.public_key());
        let cap = AttachmentCap { content_hash: "abc123".into(), declared_type: "image/png".into(), size: 42, uploader: owner.public_key() };
        let (mid, msg) = setup_message(&conn, &owner, &dev, &owner, vec![cap]);
        assert_eq!(derive_attachments(&conn, mid, &msg, &owner.public_key()).unwrap(), 1);
        assert_eq!(attachment_count(&conn, mid), 1);
    }

    #[test]
    fn missing_blob_is_quarantined() {
        let conn = crate::db::open_in_memory().unwrap();
        let owner = Keypair::generate();
        let dev = Keypair::generate();
        let cap = AttachmentCap { content_hash: "nope".into(), declared_type: "image/png".into(), size: 42, uploader: owner.public_key() };
        let (mid, msg) = setup_message(&conn, &owner, &dev, &owner, vec![cap]);
        assert_eq!(derive_attachments(&conn, mid, &msg, &owner.public_key()).unwrap(), 0);
        assert_eq!(attachment_count(&conn, mid), 0);
    }

    #[test]
    fn size_mime_uploader_mismatch_is_quarantined() {
        let conn = crate::db::open_in_memory().unwrap();
        let owner = Keypair::generate();
        let dev = Keypair::generate();
        let stranger = Keypair::generate();
        insert_file(&conn, "h1", 42, "image/png", &owner.public_key());
        // size mismatch
        let c_size = AttachmentCap { content_hash: "h1".into(), declared_type: "image/png".into(), size: 99, uploader: owner.public_key() };
        let (m1, e1) = setup_message(&conn, &owner, &dev, &owner, vec![c_size]);
        assert_eq!(derive_attachments(&conn, m1, &e1, &owner.public_key()).unwrap(), 0);
        // type mismatch
        let conn2 = crate::db::open_in_memory().unwrap();
        insert_file(&conn2, "h1", 42, "image/png", &owner.public_key());
        let c_type = AttachmentCap { content_hash: "h1".into(), declared_type: "image/jpeg".into(), size: 42, uploader: owner.public_key() };
        let (m2, e2) = setup_message(&conn2, &owner, &dev, &owner, vec![c_type]);
        assert_eq!(derive_attachments(&conn2, m2, &e2, &owner.public_key()).unwrap(), 0);
        // uploader mismatch (cap claims stranger but blob uploaded_by owner)
        let conn3 = crate::db::open_in_memory().unwrap();
        insert_file(&conn3, "h1", 42, "image/png", &owner.public_key());
        let c_up = AttachmentCap { content_hash: "h1".into(), declared_type: "image/png".into(), size: 42, uploader: stranger.public_key() };
        let (m3, e3) = setup_message(&conn3, &owner, &dev, &owner, vec![c_up]);
        assert_eq!(derive_attachments(&conn3, m3, &e3, &owner.public_key()).unwrap(), 0);
    }

    #[test]
    fn non_owner_cannot_attach_others_upload_but_owner_can() {
        // author is a stranger, cap+blob uploaded_by a *different* stranger → invalid (author != uploader, != owner).
        let conn = crate::db::open_in_memory().unwrap();
        let owner = Keypair::generate();
        let dev = Keypair::generate();
        let author = Keypair::generate();
        let uploader = Keypair::generate();
        insert_file(&conn, "h2", 7, "image/png", &uploader.public_key());
        let cap = AttachmentCap { content_hash: "h2".into(), declared_type: "image/png".into(), size: 7, uploader: uploader.public_key() };
        let (m, e) = setup_message(&conn, &owner, &dev, &author, vec![cap]);
        assert_eq!(derive_attachments(&conn, m, &e, &owner.public_key()).unwrap(), 0);

        // Same blob, but the OWNER posts it → owner exception allows attaching another's upload.
        let conn2 = crate::db::open_in_memory().unwrap();
        insert_file(&conn2, "h2", 7, "image/png", &uploader.public_key());
        let cap2 = AttachmentCap { content_hash: "h2".into(), declared_type: "image/png".into(), size: 7, uploader: uploader.public_key() };
        let (m2, e2) = setup_message(&conn2, &owner, &dev, &owner, vec![cap2]);
        assert_eq!(derive_attachments(&conn2, m2, &e2, &owner.public_key()).unwrap(), 1);
    }

    #[test]
    fn derive_attachments_is_idempotent() {
        let conn = crate::db::open_in_memory().unwrap();
        let owner = Keypair::generate();
        let dev = Keypair::generate();
        insert_file(&conn, "h3", 1, "image/png", &owner.public_key());
        let cap = AttachmentCap { content_hash: "h3".into(), declared_type: "image/png".into(), size: 1, uploader: owner.public_key() };
        let (mid, msg) = setup_message(&conn, &owner, &dev, &owner, vec![cap]);
        assert_eq!(derive_attachments(&conn, mid, &msg, &owner.public_key()).unwrap(), 1);
        assert_eq!(derive_attachments(&conn, mid, &msg, &owner.public_key()).unwrap(), 0);
        assert_eq!(attachment_count(&conn, mid), 1);
    }
```

Add to the `use` line at the top of `mod tests`: change
`use farder_crypto::event_log::{DeviceCert, EventPayload as EP};`
to
`use farder_crypto::event_log::{AttachmentCap, DeviceCert, EventPayload as EP};`

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p farder-server event_ingest::tests::valid_cap_is_materialized 2>&1 | tail -20`
Expected: FAIL — `cannot find function 'derive_attachments'`.

- [ ] **Step 3: Add the import and implement `derive_attachments`**

At the top of `crates/farder-server/src/event_ingest.rs`, add to the crypto import:

```rust
use farder_crypto::event_log::{AttachmentCap, Event, EventHash, EventPayload, Genesis};
use farder_crypto::identity::PublicKey;
```

Add the function after `derive_message_row` (before `reconcile_messages`):

```rust
/// Validate each `AttachmentCap` on a `MessagePosted` event against the actual
/// stored blob and create a `message_attachments` row for each VALID cap. A cap is
/// valid iff a blob with its `content_hash` exists AND the blob's `size`,
/// `mime_type`, and `uploaded_by` match the cap AND the event author is the cap's
/// uploader OR the server `owner` (owner may attach another member's upload, mirroring
/// the legacy rule in handlers.rs). Invalid caps (missing blob or any mismatch) are
/// skipped + logged — the message still renders, the attachment is just not
/// materialized (and so not downloadable). Idempotent: a cap already materialized for
/// this message is skipped, so reconcile can re-run safely. Returns the count newly
/// created. Non-message payloads return `Ok(0)`.
pub fn derive_attachments(
    conn: &Connection,
    message_id: u64,
    event: &Event,
    owner: &PublicKey,
) -> Result<usize> {
    let EventPayload::MessagePosted { attachments, .. } = &event.core.payload else {
        return Ok(0);
    };
    let author = &event.core.author;
    let mut created = 0usize;
    for (position, cap) in attachments.iter().enumerate() {
        let Some(file) = crate::attachments::get_file_by_hash(conn, &cap.content_hash)? else {
            tracing::warn!(hash = %cap.content_hash, "attachment cap references unknown blob; quarantined");
            continue;
        };
        let valid = file.size == cap.size
            && file.mime_type == cap.declared_type
            && file.uploaded_by == cap.uploader
            && (author == &cap.uploader || author == owner);
        if !valid {
            tracing::warn!(hash = %cap.content_hash, "attachment cap mismatch vs stored blob; quarantined");
            continue;
        }
        let already: Option<bool> = conn
            .query_row(
                "SELECT 1 FROM message_attachments WHERE message_id = ?1 AND file_id = ?2 LIMIT 1",
                params![message_id as i64, file.id as i64],
                |_| Ok(true),
            )
            .optional()?;
        if already.is_some() {
            continue;
        }
        crate::attachments::create_message_attachment(
            conn,
            message_id,
            file.id,
            position as u32,
            &file.original_name,
            file.width,
            file.height,
            file.duration_secs,
        )?;
        created += 1;
    }
    Ok(created)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p farder-server event_ingest:: 2>&1 | tail -25`
Expected: PASS — all `valid_cap_is_materialized`, `missing_blob_is_quarantined`, `size_mime_uploader_mismatch_is_quarantined`, `non_owner_cannot_attach_others_upload_but_owner_can`, `derive_attachments_is_idempotent` (plus the pre-existing event_ingest tests) pass.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-server/src/event_ingest.rs
git commit -m "feat(mesh-4a): derive_attachments validates caps vs stored blobs"
```

---

### Task 2: Server — wire `derive_attachments` into the SubmitEvent ingest transaction

**Files:**
- Modify: `crates/farder-server/src/handlers.rs:1840-1850` (the SubmitEvent persist block) + add an integration test in the `mod tests` SubmitEvent section.

**Interfaces:**
- Consumes: `crate::event_ingest::derive_attachments` (Task 1); `state.genesis: Mutex<Option<Genesis>>` (state.rs:93); `Genesis.owner: PublicKey`.
- Produces: SubmitEvent now materializes attachments atomically with the message row.

- [ ] **Step 1: Add the failing integration test**

In `crates/farder-server/src/handlers.rs`, in the SubmitEvent test section (near line 3861), add a test. It mirrors the existing pattern that establishes genesis, authorizes the owner's device, then submits a message — but first inserts a blob and attaches it. Use the existing test helpers in that module for building the harness (`make_state`, `dispatch`, genesis/device setup — copy the exact setup from the adjacent `submit_event_*` test that already posts a MessagePosted, e.g. the one around line 3958, and extend it):

```rust
    #[tokio::test]
    async fn submit_event_with_valid_attachment_materializes_row() {
        // --- reuse the adjacent submit_event test's harness setup verbatim up to the
        //     point where the owner's device is authorized and `state`/`conn`/`owner`/
        //     `dev`/`ls`/genesis are in scope. (Copy from submit_event_message test.) ---
        // Insert a blob owned by the owner.
        {
            let db = state.db.lock().unwrap();
            db.execute(
                "INSERT INTO files (hash, size, mime_type, original_name, uploaded_by, uploaded_at, ref_count) \
                 VALUES ('cafef00d', 5, 'image/png', 'pic.png', ?1, 1, 0)",
                rusqlite::params![owner.public_key().as_bytes().as_slice()],
            ).unwrap();
        }
        // Build a MessagePosted carrying a matching cap, chained after the owner's device-auth head.
        let cap = farder_crypto::event_log::AttachmentCap {
            content_hash: "cafef00d".into(), declared_type: "image/png".into(), size: 5, uploader: owner.public_key(),
        };
        let msg = Event::next(&dev, owner.public_key(), genesis.server_id(), Some(&da), 1, 2,
            EP::MessagePosted { channel_id: 1, content: "look".into(), reply_to: None, attachments: vec![cap] });

        let result = dispatch(&state, &owner.public_key(), true,
            ServerRequest::SubmitEvent { event: msg.clone() }).await;
        assert!(matches!(result.response, ServerResponse::EventAccepted { .. }), "expected acceptance, got {:?}", result.response);

        // The message_attachments row exists and points at the blob.
        let db = state.db.lock().unwrap();
        let count: i64 = db.query_row(
            "SELECT COUNT(*) FROM message_attachments ma JOIN files f ON f.id = ma.file_id WHERE f.hash = 'cafef00d'",
            [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
    }
```

> Implementer note: the exact harness lines (channel 1 creation, genesis establish, `da` device-auth event, `dispatch` signature `(state, member, is_owner, req)`) must be copied from the nearest existing `submit_event_*` async test in this file so types line up. Do not invent helper names — use the ones already in `mod tests`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p farder-server submit_event_with_valid_attachment 2>&1 | tail -20`
Expected: FAIL — `assert_eq!(count, 1)` fails with `0` (handler doesn't derive attachments yet).

- [ ] **Step 3: Wire `derive_attachments` into the transaction**

In `crates/farder-server/src/handlers.rs`, replace the persist block (currently lines 1840-1850):

```rust
            // 4. Persist the event (source of truth) + derive the message row — atomically.
            let derived_id = {
                let tx = conn.unchecked_transaction()
                    .map_err(|e| anyhow::anyhow!("failed to begin tx: {}", e))?;
                crate::event_ingest::store_event(&tx, &event)
                    .map_err(|e| anyhow::anyhow!("failed to store event: {}", e))?;
                let id = crate::event_ingest::derive_message_row(&tx, &event)
                    .map_err(|e| anyhow::anyhow!("failed to derive message: {}", e))?;
                tx.commit().map_err(|e| anyhow::anyhow!("failed to commit event: {}", e))?;
                id
            };
```

with:

```rust
            // 4. Persist the event (source of truth) + derive the message row + its
            //    validated attachments — atomically, so a crash never leaves a message
            //    without its attachment rows.
            let owner_pk = state.genesis.lock().unwrap().as_ref().map(|g| g.owner.clone());
            let derived_id = {
                let tx = conn.unchecked_transaction()
                    .map_err(|e| anyhow::anyhow!("failed to begin tx: {}", e))?;
                crate::event_ingest::store_event(&tx, &event)
                    .map_err(|e| anyhow::anyhow!("failed to store event: {}", e))?;
                let id = crate::event_ingest::derive_message_row(&tx, &event)
                    .map_err(|e| anyhow::anyhow!("failed to derive message: {}", e))?;
                if let (Some(mid), Some(owner)) = (id, owner_pk.as_ref()) {
                    crate::event_ingest::derive_attachments(&tx, mid, &event, owner)
                        .map_err(|e| anyhow::anyhow!("failed to derive attachments: {}", e))?;
                }
                tx.commit().map_err(|e| anyhow::anyhow!("failed to commit event: {}", e))?;
                id
            };
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p farder-server submit_event 2>&1 | tail -25`
Expected: PASS — the new test plus all existing `submit_event_*` tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-server/src/handlers.rs
git commit -m "feat(mesh-4a): SubmitEvent materializes validated attachments in-tx"
```

---

### Task 3: Server — `reconcile_attachments` startup repair

**Files:**
- Modify: `crates/farder-server/src/event_ingest.rs` (add `reconcile_attachments` + test)
- Modify: `crates/farder-server/src/main.rs:116` (call it next to `reconcile_messages`)

**Interfaces:**
- Consumes: `derive_attachments` (Task 1); `load_genesis`; `Event::from_bytes`.
- Produces: `pub fn reconcile_attachments(conn: &Connection) -> Result<usize>` — idempotently materializes missing valid attachment rows for already-derived `MessagePosted` events.

- [ ] **Step 1: Add the failing test**

In `event_ingest.rs` `mod tests`:

```rust
    #[test]
    fn reconcile_attachments_repairs_missing_rows_idempotently() {
        let conn = crate::db::open_in_memory().unwrap();
        let owner = Keypair::generate();
        let dev = Keypair::generate();
        insert_file(&conn, "rr", 3, "image/png", &owner.public_key());
        let cap = AttachmentCap { content_hash: "rr".into(), declared_type: "image/png".into(), size: 3, uploader: owner.public_key() };
        // Store the event + derive the message row, but NOT the attachment (crash window).
        let (mid, _msg) = setup_message(&conn, &owner, &dev, &owner, vec![cap]);
        assert_eq!(attachment_count(&conn, mid), 0);
        // Reconcile materializes it once; a second run is a no-op.
        assert_eq!(reconcile_attachments(&conn).unwrap(), 1);
        assert_eq!(reconcile_attachments(&conn).unwrap(), 0);
        assert_eq!(attachment_count(&conn, mid), 1);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p farder-server reconcile_attachments 2>&1 | tail -20`
Expected: FAIL — `cannot find function 'reconcile_attachments'`.

- [ ] **Step 3: Implement `reconcile_attachments`**

In `event_ingest.rs`, after `reconcile_messages`:

```rust
/// Repair drift: for every stored `MessagePosted` event that already has a derived
/// `messages` row, (re)materialize any missing VALID attachment rows. Idempotent
/// (each cap is guarded inside `derive_attachments`). Returns the number of attachment
/// rows created. No-op if there is no genesis (legacy server) — and legacy
/// `MessagePosted` events carry empty `attachments`, so this only does work for
/// log-mode servers that crashed mid-derive or that replicate events (forward-compat).
pub fn reconcile_attachments(conn: &Connection) -> Result<usize> {
    let Some(g) = load_genesis(conn)? else { return Ok(0) };
    let owner = g.owner.clone();
    let rows: Vec<(Vec<u8>, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT e.event_body, m.id FROM events e \
             JOIN messages m ON m.event_hash = e.event_hash \
             WHERE e.payload_type = 'MessagePosted' ORDER BY e.accept_seq ASC",
        )?;
        let mapped = stmt.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)))?;
        let mut v = Vec::new();
        for row in mapped { v.push(row?); }
        v
    };
    let mut repaired = 0;
    for (body, mid) in rows {
        let event = Event::from_bytes(&body).context("decode event for reconcile_attachments")?;
        repaired += derive_attachments(conn, mid as u64, &event, &owner)?;
    }
    Ok(repaired)
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p farder-server reconcile 2>&1 | tail -20`
Expected: PASS — `reconcile_attachments_repairs_missing_rows_idempotently` and the existing `reconcile_derives_missing_message_rows`.

- [ ] **Step 5: Call it at startup**

In `crates/farder-server/src/main.rs`, after line 116:

```rust
            let repaired = farder_server::event_ingest::reconcile_messages(&conn).unwrap_or(0);
```

add:

```rust
            let repaired_att = farder_server::event_ingest::reconcile_attachments(&conn).unwrap_or(0);
            if repaired_att > 0 {
                tracing::info!(count = repaired_att, "reconciled missing attachment rows from the event log");
            }
```

(If the existing line logs `repaired`, keep that log; just add the attachment reconcile + its log beneath it.)

- [ ] **Step 6: Build + commit**

Run: `cargo build -p farder-server 2>&1 | tail -5`
Expected: builds clean.

```bash
git add crates/farder-server/src/event_ingest.rs crates/farder-server/src/main.rs
git commit -m "feat(mesh-4a): reconcile_attachments heals missing attachment rows on startup"
```

---

### Task 4: Server — uniform download error responses (existence-oracle hardening)

**Files:**
- Modify: `crates/farder-server/src/connection.rs:228-231` and `:270-273` (download error reasons)

**Interfaces:**
- Consumes/Produces: same `DownloadResponse::Error` shape; both the "file missing" and "access denied" branches now return the identical reason so a content hash / file_id cannot be probed as an existence oracle.

- [ ] **Step 1: Collapse the two distinguishable error reasons**

In `handle_download_stream`, change the "file not found" branch (line ~229):

```rust
            let resp = codec::encode(&DownloadResponse::Error {
                reason: "file not found".to_string(),
            })?;
```

to:

```rust
            let resp = codec::encode(&DownloadResponse::Error {
                reason: "not available".to_string(),
            })?;
```

and change the "access denied" branch (line ~271):

```rust
        let resp = codec::encode(&DownloadResponse::Error {
            reason: "access denied".to_string(),
        })?;
```

to:

```rust
        let resp = codec::encode(&DownloadResponse::Error {
            // Uniform with the "missing" case so a file_id / content hash can't be
            // used as an existence oracle (mesh design: existence-oracle gating).
            reason: "not available".to_string(),
        })?;
```

- [ ] **Step 2: Build to verify**

Run: `cargo build -p farder-server 2>&1 | tail -5`
Expected: builds clean.

- [ ] **Step 3: Commit**

```bash
git add crates/farder-server/src/connection.rs
git commit -m "feat(mesh-4a): uniform download error so file_id is not an existence oracle"
```

---

### Task 5: Client (Rust) — upload returns cap fields; `submit_event` builds caps

**Files:**
- Modify: `client/src-tauri/src/commands.rs` — add `UploadOutcome` + `AttachmentCapInput`; change `upload_file` and `upload_file_internal_with_channel` returns; keep `upload_file_internal` returning `u64`; add an `attachments` param to `submit_event` and build caps.

**Interfaces:**
- Produces (to TS, Task 6):
  - `upload_file(...) -> UploadOutcome { fileId: u64, contentHash, declaredType, size }`
  - `submit_event(..., attachments: Vec<AttachmentCapInput>)` where `AttachmentCapInput { contentHash, declaredType, size }`.
- Consumes: `farder_crypto::event_log::AttachmentCap`; existing `identity` in `submit_event`.

- [ ] **Step 1: Define the serde structs**

In `client/src-tauri/src/commands.rs`, near the other small DTO structs (top of file area), add:

```rust
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadOutcome {
    pub file_id: u64,
    pub content_hash: String,
    pub declared_type: String,
    pub size: u64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentCapInput {
    pub content_hash: String,
    pub declared_type: String,
    pub size: u64,
}
```

- [ ] **Step 2: Make `upload_file_internal_with_channel` return `UploadOutcome`**

Change its signature return from `Result<u64, String>` to `Result<UploadOutcome, String>`. The `hash`, `mime_type`, and `data.len()` are already computed locally (lines 1434/1436/1465). At each point that currently returns a `file_id` (the `Complete { file_id }` arms at ~1497 and ~1502, and any dedup short-circuit), build and return the outcome instead. To avoid moving `hash`/`mime_type` into the `UploadRequest` and then needing them again, clone them for the request:

In the `UploadRequest` construction (line ~1462), change `hash,` to `hash: hash.clone(),` and `mime_type,` to `mime_type: mime_type.clone(),`. Capture the size once before the match:

```rust
    let size = data.len() as u64;
```

Then replace each `Ok(file_id)` return inside this function's response match with:

```rust
            farder_protocol::server::UploadResponse::Complete { file_id } => Ok(UploadOutcome {
                file_id,
                content_hash: hash.clone(),
                declared_type: mime_type.clone(),
                size,
            }),
```

(Apply to both the post-`Ready` `Complete` arm and the dedup short-circuit `Complete` arm.)

- [ ] **Step 3: Update the two callers of the internal helper**

`upload_file` (the Tauri command, line ~1377) — change its return type and body:

```rust
#[tauri::command]
pub async fn upload_file(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
    file_path: String,
) -> Result<UploadOutcome, String> {
    upload_file_internal_with_channel(&state, &server_id, channel_id, &file_path).await
}
```

(Keep the `#[tauri::command]` attribute exactly as it currently is above the fn.)

`upload_file_internal` (line ~1388, used by book.rs) — keep it returning `u64` by extracting the field:

```rust
pub(crate) async fn upload_file_internal(
    state: &AppState,
    server_id: &str,
    file_path: &str,
) -> Result<u64, String> {
    upload_file_internal_with_channel(state, server_id, 0, file_path)
        .await
        .map(|o| o.file_id)
}
```

(book.rs:259 is unchanged — it still gets a `u64`.)

- [ ] **Step 4: Add `attachments` to `submit_event` and build caps**

In `submit_event` (line ~3795), add the parameter (after `reply_to`):

```rust
pub async fn submit_event(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    log_server_id: String,
    channel_id: u64,
    content: String,
    reply_to: Option<String>,
    attachments: Vec<AttachmentCapInput>,
) -> Result<EventAcceptedResult, String> {
```

Where the message event is built (line ~3850), replace `attachments: vec![]` by mapping the inputs to caps stamped with the caller's identity as uploader. Just before the `let msg = event_build_next(` call, add:

```rust
    let caps: Vec<farder_crypto::event_log::AttachmentCap> = attachments
        .into_iter()
        .map(|a| farder_crypto::event_log::AttachmentCap {
            content_hash: a.content_hash,
            declared_type: a.declared_type,
            size: a.size,
            uploader: identity.public_key(),
        })
        .collect();
```

and change the payload to:

```rust
        EventPayload::MessagePosted {
            channel_id,
            content,
            reply_to,
            attachments: caps,
        },
```

> Seam note (CLAUDE.md): `submit_event` and `upload_file` names are unchanged, so `generate_handler!` (main.rs:258, :126) needs no edit. Verify after building: `grep -n 'submit_event\|upload_file' client/src-tauri/src/main.rs` still lists both.

- [ ] **Step 5: Build the client crate**

Run: `cd client/src-tauri && cargo build 2>&1 | tail -15`
Expected: builds clean (no other internal Rust caller of `upload_file` exists besides the two handled in Step 3 — confirmed: only book.rs via `upload_file_internal`).

- [ ] **Step 6: Commit**

```bash
git add client/src-tauri/src/commands.rs
git commit -m "feat(mesh-4a): upload returns cap fields; submit_event carries AttachmentCaps"
```

---

### Task 6: Client (TS) — bridge types + route staged-file mesh messages over the log

**Files:**
- Modify: `client/src/lib/tauri-bridge.ts` — `UploadOutcome`/`AttachmentCapInput` types; `uploadFile` + `submitEvent` signatures.
- Modify: `client/src/components/MessageInput.tsx` — track the staged cap; route over the log when all attachments carry caps.

**Interfaces:**
- Consumes: `upload_file -> UploadOutcome`, `submit_event(attachments)` (Task 5).
- Produces: a mesh message with a staged-file attachment posts via `submitEvent` with caps; URL-fetched / inline-emoji attachments keep the legacy path (documented limitation).

- [ ] **Step 1: Bridge types + signatures**

In `client/src/lib/tauri-bridge.ts`, add near the top exports:

```ts
export interface UploadOutcome {
  fileId: number;
  contentHash: string;
  declaredType: string;
  size: number;
}

export interface AttachmentCapInput {
  contentHash: string;
  declaredType: string;
  size: number;
}
```

Change `uploadFile` (line ~283) to:

```ts
export async function uploadFile(serverId: string, channelId: number, filePath: string): Promise<UploadOutcome> {
  return invoke<UploadOutcome>("upload_file", { serverId, channelId, filePath });
}
```

(Preserve any existing body lines other than the return type — match the current `invoke` arg names.)

Change `submitEvent` (line ~165) to accept caps:

```ts
export async function submitEvent(
  serverId: string,    // connection key (address) — routes the request
  logServerId: string, // genesis hash — stamps the event + keys the device chain
  channelId: number,
  content: string,
  replyTo?: string | null,
  attachments: AttachmentCapInput[] = [],
): Promise<{ event_hash: string; timestamp: number }> {
  return invoke("submit_event", { serverId, logServerId, channelId, content, replyTo: replyTo ?? null, attachments });
}
```

- [ ] **Step 2: Track the staged cap in MessageInput**

In `client/src/components/MessageInput.tsx`, add a state alongside `attachedFileId`/`attachedFileName` (search for `attachedFileName` to find the `useState` block):

```ts
  const [attachedCap, setAttachedCap] = useState<AttachmentCapInput | null>(null);
```

Add `AttachmentCapInput` (and `UploadOutcome` if needed) to the existing `import ... from "../lib/tauri-bridge"` / `api` import. If the file imports the bridge as `api`, reference the type via a type import:

```ts
import type { AttachmentCapInput } from "../lib/tauri-bridge";
```

At the staging upload (line ~81), capture the cap:

```ts
      const outcome = await api.uploadFile(serverId, channelId, path);
      setAttachedFileId(outcome.fileId);
      setAttachedCap({ contentHash: outcome.contentHash, declaredType: outcome.declaredType, size: outcome.size });
```

(Adjust the surrounding lines that set `attachedFileName` to keep them; only the `fileId` extraction + the new `setAttachedCap` are added.)

At the voice-recorder upload (line ~188), it now gets an `UploadOutcome`:

```ts
      const outcome = await api.uploadFile(serverId, channelId, filePath);
      await api.sendMessage(serverId, channelId, "", undefined, [outcome.fileId]);
```

Wherever the staged file is cleared (the reset after send at ~257, and any "remove staged file" handler that calls `setAttachedFileId(null)`), also clear the cap:

```ts
      setAttachedFileId(null);
      setAttachedFileName(null);
      setAttachedCap(null);
```

- [ ] **Step 3: Route staged-file mesh messages over the log**

In `handleSend` (line ~201), the message currently routes to the log only when `finalAttachments.length === 0`. Change so a staged-file-only attachment goes over the log with its cap, while URL-fetched / inline-emoji attachments (which have no client-known hash) keep the legacy path.

Track whether any uncappable attachment is present. Replace the auto-fetch + emoji + routing region (lines ~209-255) with:

```ts
      // Auto-fetch image URLs found in the message text (legacy-only: no client-known hash).
      const urls = text.match(imageUrlRegex) || [];
      let hasUncappableAttachment = urls.length > 0;
      for (const url of urls) {
        try {
          const fileId = await api.fetchUrl(serverId, url, channelId);
          attachments.push(fileId);
        } catch {
          // Failed to fetch — leave the URL as plain text
        }
      }

      // Encrypt the message content if this is a DM channel
      let messageContent = text;
      const dm = activeServer?.dms.find(d => d.channel.id === channelId);
      if (dm) {
        const peerPk = publicKeyToString(dm.participant.public_key);
        try {
          messageContent = await api.dmEncrypt(peerPk, text);
        } catch {
          setError("Encryption failed — message not sent");
          return;
        }
      }

      // Resolve inline :name: tokens into book-item file_ids (legacy-only: no client cap).
      const beforeEmoji = attachments.length;
      const finalAttachments = await resolveInlineEmojiAttachments(text, attachments);
      if (finalAttachments.length > beforeEmoji) hasUncappableAttachment = true;

      const logServerId = activeServer?.logServerId ?? null;
      // Route over the mesh log when this is a log server, not a DM, and every
      // attachment carries a client-known cap (i.e. only the staged upload — URL
      // images and inline emoji have no client-side hash and stay on legacy).
      if (logServerId && !dm && !hasUncappableAttachment) {
        const caps = attachedCap ? [attachedCap] : [];
        // TODO(mesh): replies over the log need event-hash mapping; legacy replyTo is a numeric id, so drop it for now (top-level post).
        await api.submitEvent(serverId, logServerId, channelId, messageContent, null, caps);
      } else {
        await api.sendMessage(
          serverId,
          channelId,
          messageContent,
          replyTo,
          finalAttachments.length > 0 ? finalAttachments : undefined,
        );
      }
```

> Note: when `attachedCap` is set, `attachments` started as `[attachedFileId]`; the staged file is therefore NOT in `urls`/emoji, so `hasUncappableAttachment` stays false for a staged-file-only message and it routes over the log with `caps`.

- [ ] **Step 4: Type-check**

Run: `cd client && npx tsc --noEmit 2>&1 | tail -20`
Expected: no errors. (If `UploadOutcome`/`AttachmentCapInput` are reported unused in the bridge, they are used by MessageInput + the function signatures — confirm the import path.)

- [ ] **Step 5: Commit**

```bash
git add client/src/lib/tauri-bridge.ts client/src/components/MessageInput.tsx
git commit -m "feat(mesh-4a): route staged-file mesh messages over the log with caps"
```

---

### Task 7: Docs — record the changed surface

**Files:**
- Modify: `docs/modules/tauri-commands.md` (upload_file return, submit_event param)
- Modify: `docs/modules/frontend-bridge.md` (uploadFile/submitEvent signatures) — file name per the actual doc (`frontend-bridge.md` or `tauri-bridge.md`; check which exists)
- Modify: `docs/modules/server-handlers.md` or `server-connection.md` (note cap validation in SubmitEvent ingest + uniform download error)
- Modify: `ARCHITECTURE.md` (note attachments flow over the log via cap validation)

- [ ] **Step 1: Update the command docs**

In `docs/modules/tauri-commands.md`, update the `upload_file` entry: return is now `UploadOutcome { fileId, contentHash, declaredType, size }` (was `file_id`). Update the `submit_event` entry: add the `attachments: AttachmentCapInput[]` parameter ({ contentHash, declaredType, size }); the command stamps `uploader` from the caller's identity and posts a `MessagePosted` carrying the caps.

- [ ] **Step 2: Update the bridge doc**

In the frontend bridge doc, update `uploadFile` (returns `UploadOutcome`) and `submitEvent` (new `attachments` arg). Note that URL-fetched and inline-emoji attachments still use the legacy `sendMessage` path (no client-known content hash).

- [ ] **Step 3: Update server + architecture docs**

In the server handler/connection doc, add: on `SubmitEvent`, a `MessagePosted`'s `AttachmentCap`s are validated against the stored blob (`size`/`mime_type`/`uploaded_by` + author-is-uploader-or-owner) and materialized into `message_attachments` inside the ingest transaction; invalid caps are quarantined (not materialized). Note the uniform "not available" download error as existence-oracle hardening. In `ARCHITECTURE.md`, note attachments now flow over the event log (not just legacy `SendMessage`).

- [ ] **Step 4: Commit**

```bash
git add docs/
git commit -m "docs(mesh-4a): attachment caps over the log + changed command/bridge signatures"
```

---

## Owner runtime verification (REQUIRED before "done" — server changed → full rebuild incl. sidecar)

Per CLAUDE.md verify-before-done, this feature is UNVERIFIED until the owner runs it on Windows:

`git pull` → `cargo build -p farder-server` → STOP the app → `.\client\src-tauri\binaries\copy-sidecar.ps1` (run from repo root) → `cd client; npm run tauri dev` → `Ctrl+Shift+R`. Then on a **mesh** (log-mode) server:

1. Attach an image and send → the message sends, renders with the image, and the image **downloads/displays**.
2. Restart the app → the message **and its attachment survive** (it is derived from an `events` row, not a legacy-only `messages` row).
3. A second identity (FARDER_DATA instance) who is a log member sees the message and can download the attachment; a non-member cannot.

## Self-review notes (coverage vs spec)

- Spec "validate caps against stored blob; materialize only valid" → Task 1 (`derive_attachments`) + Task 2 (wired into ingest).
- Spec "atomic message+attachments in the ingest transaction" → Task 2.
- Spec "reconcile materializes missing attachment rows idempotently" → Task 3.
- Spec "existence-oracle gating + uniform responses" → existing channel-view/member gate is reused (Task 2 makes mesh attachments create the join rows the gate reads); Task 4 makes failure responses uniform.
- Spec "client builds caps + routes attachment-bearing mesh messages over the log" → Tasks 5–6.
- Spec "derive original_name from the blob row (no crypto change)" → Task 1 uses `file.original_name`.
- Spec "sources that cannot surface a hash fall back to legacy" → Task 6 keeps URL-fetched + inline-emoji on legacy (documented).
- Out of scope (4b / other tracks): redaction/GC/moderation state, byte sniffing, pending-blob placeholders + late-arrival reconciliation — none appear as tasks. Correct.
