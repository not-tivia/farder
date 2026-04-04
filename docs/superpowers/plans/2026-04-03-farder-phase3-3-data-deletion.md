# Farder Phase 3.3: Data Deletion Rights — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give server members the right to request deletion of all their data with a 72-hour cancellable grace period, after which messages are anonymized, attachments removed, reactions cleared, and the member record deleted.

**Architecture:** A `deletion_requests` table tracks pending requests. The existing retention background task checks for expired requests each cycle and executes the purge. Messages are anonymized (author → sentinel key, content → `[deleted]`) rather than removed, preserving conversation structure. New protocol types for request/cancel/status. No new modules — logic distributed across existing modules.

**Tech Stack:** Existing — Rust, rusqlite, farder-protocol, farder-server.

**Spec:** `docs/specs/2026-04-03-farder-phase3-3-data-deletion-design.md`

---

## File Structure

### Modified Files

```
crates/farder-protocol/src/server.rs      # DELETED_USER_KEY constant, DeletionStatus struct, new requests/events
crates/farder-server/src/db.rs            # Add deletion_requests table
crates/farder-server/src/members.rs       # Deletion request CRUD functions
crates/farder-server/src/messages.rs      # anonymize_messages_by_author function
crates/farder-server/src/reactions.rs     # delete_reactions_by_user function
crates/farder-server/src/attachments.rs   # remove_attachments_for_author_messages function
crates/farder-server/src/handlers.rs      # RequestDeletion, CancelDeletion, GetDeletionStatus handlers
crates/farder-server/src/retention.rs     # execute_pending_deletions in the retention cycle
tests/e2e_server.rs                       # Deletion request + verify anonymization test
```

---

## Task 1: Protocol Types

**Files:**
- Modify: `crates/farder-protocol/src/server.rs`

- [ ] **Step 1: Add DELETED_USER_KEY constant and DeletionStatus struct**

Add near the top of `server.rs` (after the imports):

```rust
/// Sentinel public key for anonymized/deleted users. All zeros — not a valid Ed25519 key.
pub const DELETED_USER_KEY: [u8; 32] = [0u8; 32];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DeletionStatus {
    pub pending: bool,
    pub requested_at: Option<u64>,
    pub expires_at: Option<u64>,
}
```

- [ ] **Step 2: Add new request variants**

Add to `ServerRequest`:

```rust
RequestDeletion,
CancelDeletion,
GetDeletionStatus,
```

- [ ] **Step 3: Add new response variant**

Add to `ServerResponse`:

```rust
DeletionStatusResp { status: DeletionStatus },
```

- [ ] **Step 4: Add new event variants**

Add to `ServerEvent`:

```rust
DeletionRequested { public_key: PublicKey },
DeletionCancelled { public_key: PublicKey },
DeletionExecuted { public_key: PublicKey },
```

- [ ] **Step 5: Add roundtrip tests**

```rust
#[test]
fn test_roundtrip_deletion_status() {
    let status = DeletionStatus { pending: true, requested_at: Some(1000), expires_at: Some(1000 + 72 * 3600) };
    let bytes = codec::encode(&status).unwrap();
    let decoded: DeletionStatus = codec::decode(&bytes).unwrap();
    assert!(decoded.pending);
    assert_eq!(decoded.expires_at, Some(1000 + 72 * 3600));
}

#[test]
fn test_roundtrip_request_deletion() {
    let frame = ClientFrame::Request { id: 1, body: ServerRequest::RequestDeletion };
    let bytes = codec::encode(&frame).unwrap();
    let _: ClientFrame = codec::decode(&bytes).unwrap();
}
```

Add `RequestDeletion`, `CancelDeletion`, `GetDeletionStatus` to the existing `test_roundtrip_all_request_variants` test.

- [ ] **Step 6: Fix compilation — add stub handlers**

In `handlers.rs`, add match arms for the 3 new request types returning `err("not yet implemented")`.

- [ ] **Step 7: Verify all tests pass**

Run: `cargo test --workspace`

- [ ] **Step 8: Commit**

```bash
git add crates/farder-protocol/src/server.rs crates/farder-server/src/handlers.rs
git commit -m "feat(protocol): add data deletion request/response/event types and DELETED_USER_KEY"
```

---

## Task 2: DB Schema & Deletion Request CRUD

**Files:**
- Modify: `crates/farder-server/src/db.rs`
- Modify: `crates/farder-server/src/members.rs`

- [ ] **Step 1: Add deletion_requests table**

In `db.rs` `init_schema`, add:

```sql
CREATE TABLE IF NOT EXISTS deletion_requests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    member_key BLOB UNIQUE NOT NULL,
    requested_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
```

- [ ] **Step 2: Write tests for deletion request CRUD**

Add to `members.rs` tests:

```rust
#[test]
fn test_create_and_get_deletion_request() {
    let conn = setup();
    let pk = make_key();
    register_member(&conn, &pk, "Alice").unwrap();
    create_deletion_request(&conn, &pk).unwrap();
    let req = get_deletion_request(&conn, &pk).unwrap().unwrap();
    assert!(req.expires_at > req.requested_at);
    assert_eq!(req.expires_at - req.requested_at, 72 * 3600);
}

#[test]
fn test_duplicate_deletion_request_fails() {
    let conn = setup();
    let pk = make_key();
    register_member(&conn, &pk, "Alice").unwrap();
    create_deletion_request(&conn, &pk).unwrap();
    let result = create_deletion_request(&conn, &pk);
    assert!(result.is_err());
}

#[test]
fn test_cancel_deletion_request() {
    let conn = setup();
    let pk = make_key();
    register_member(&conn, &pk, "Alice").unwrap();
    create_deletion_request(&conn, &pk).unwrap();
    cancel_deletion_request(&conn, &pk).unwrap();
    assert!(get_deletion_request(&conn, &pk).unwrap().is_none());
}

#[test]
fn test_list_expired_deletion_requests() {
    let conn = setup();
    let pk1 = make_key();
    let pk2 = make_key();
    register_member(&conn, &pk1, "Alice").unwrap();
    register_member(&conn, &pk2, "Bob").unwrap();
    // Create with already-expired timestamps for testing
    create_deletion_request_with_expires(&conn, &pk1, 100, 200).unwrap();
    create_deletion_request_with_expires(&conn, &pk2, 100, u64::MAX / 2).unwrap();
    let expired = list_expired_deletion_requests(&conn).unwrap();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].member_key, pk1);
}
```

- [ ] **Step 3: Implement deletion request functions**

Add to `members.rs`:

```rust
pub struct DeletionRequest {
    pub member_key: PublicKey,
    pub requested_at: u64,
    pub expires_at: u64,
}

const DELETION_GRACE_PERIOD_SECS: u64 = 72 * 3600; // 72 hours

pub fn create_deletion_request(conn: &Connection, pk: &PublicKey) -> Result<DeletionRequest> {
    let requested_at = now();
    let expires_at = requested_at + DELETION_GRACE_PERIOD_SECS;
    create_deletion_request_with_expires(conn, pk, requested_at, expires_at)
}

pub fn create_deletion_request_with_expires(
    conn: &Connection,
    pk: &PublicKey,
    requested_at: u64,
    expires_at: u64,
) -> Result<DeletionRequest> {
    conn.execute(
        "INSERT INTO deletion_requests (member_key, requested_at, expires_at) VALUES (?1, ?2, ?3)",
        params![pk.as_bytes().as_slice(), requested_at as i64, expires_at as i64],
    )?;
    Ok(DeletionRequest { member_key: pk.clone(), requested_at, expires_at })
}

pub fn get_deletion_request(conn: &Connection, pk: &PublicKey) -> Result<Option<DeletionRequest>> {
    let mut stmt = conn.prepare(
        "SELECT member_key, requested_at, expires_at FROM deletion_requests WHERE member_key = ?1"
    )?;
    let mut rows = stmt.query_map(params![pk.as_bytes().as_slice()], |row| {
        let key_bytes: Vec<u8> = row.get(0)?;
        let requested_at: i64 = row.get(1)?;
        let expires_at: i64 = row.get(2)?;
        Ok((key_bytes, requested_at as u64, expires_at as u64))
    })?;
    match rows.next() {
        Some(Ok((key_bytes, requested_at, expires_at))) => {
            let arr: [u8; 32] = key_bytes.try_into().map_err(|_| anyhow::anyhow!("bad key"))?;
            Ok(Some(DeletionRequest {
                member_key: PublicKey::from_bytes(arr),
                requested_at,
                expires_at,
            }))
        }
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

pub fn cancel_deletion_request(conn: &Connection, pk: &PublicKey) -> Result<()> {
    conn.execute(
        "DELETE FROM deletion_requests WHERE member_key = ?1",
        params![pk.as_bytes().as_slice()],
    )?;
    Ok(())
}

pub fn list_expired_deletion_requests(conn: &Connection) -> Result<Vec<DeletionRequest>> {
    let mut stmt = conn.prepare(
        "SELECT member_key, requested_at, expires_at FROM deletion_requests WHERE expires_at < ?1"
    )?;
    let rows = stmt.query_map(params![now() as i64], |row| {
        let key_bytes: Vec<u8> = row.get(0)?;
        let requested_at: i64 = row.get(1)?;
        let expires_at: i64 = row.get(2)?;
        Ok((key_bytes, requested_at as u64, expires_at as u64))
    })?;
    let mut results = Vec::new();
    for row in rows {
        let (key_bytes, requested_at, expires_at) = row?;
        let arr: [u8; 32] = key_bytes.try_into().map_err(|_| anyhow::anyhow!("bad key"))?;
        results.push(DeletionRequest {
            member_key: PublicKey::from_bytes(arr),
            requested_at,
            expires_at,
        });
    }
    Ok(results)
}

pub fn delete_deletion_request(conn: &Connection, pk: &PublicKey) -> Result<()> {
    conn.execute(
        "DELETE FROM deletion_requests WHERE member_key = ?1",
        params![pk.as_bytes().as_slice()],
    )?;
    Ok(())
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p farder-server -- members`

- [ ] **Step 5: Commit**

```bash
git add crates/farder-server/src/db.rs crates/farder-server/src/members.rs
git commit -m "feat(server): add deletion_requests table and CRUD functions"
```

---

## Task 3: Purge Functions

**Files:**
- Modify: `crates/farder-server/src/messages.rs`
- Modify: `crates/farder-server/src/reactions.rs`
- Modify: `crates/farder-server/src/attachments.rs`

- [ ] **Step 1: Write tests for anonymize and purge functions**

In `messages.rs` tests:

```rust
#[test]
fn test_anonymize_messages_by_author() {
    let (conn, ch_id, pk) = setup();
    let other = Keypair::generate().public_key();
    crate::members::register_member(&conn, &other, "Bob").unwrap();
    insert_message(&conn, ch_id, &pk, "alice msg 1", None).unwrap();
    insert_message(&conn, ch_id, &pk, "alice msg 2", None).unwrap();
    insert_message(&conn, ch_id, &other, "bob msg", None).unwrap();

    let count = anonymize_messages_by_author(&conn, &pk).unwrap();
    assert_eq!(count, 2);

    let history = fetch_history(&conn, ch_id, None, 50, &other).unwrap();
    assert_eq!(history.len(), 3);

    let sentinel = PublicKey::from_bytes(farder_protocol::server::DELETED_USER_KEY);
    let anon_msgs: Vec<_> = history.iter().filter(|m| m.author == sentinel).collect();
    assert_eq!(anon_msgs.len(), 2);
    assert!(anon_msgs.iter().all(|m| m.content == "[deleted]"));

    let bob_msgs: Vec<_> = history.iter().filter(|m| m.author == other).collect();
    assert_eq!(bob_msgs.len(), 1);
    assert_eq!(bob_msgs[0].content, "bob msg");
}
```

In `reactions.rs` tests:

```rust
#[test]
fn test_delete_reactions_by_user() {
    let (conn, msg, alice, bob) = setup();
    let ch = crate::channels::list_channels(&conn).unwrap()[0].id;
    let msg2 = crate::messages::insert_message(&conn, ch, &bob, "msg2", None).unwrap();
    add_reaction(&conn, msg, &alice, "👍").unwrap();
    add_reaction(&conn, msg, &bob, "👍").unwrap();
    add_reaction(&conn, msg2, &alice, "❤️").unwrap();
    let count = delete_reactions_by_user(&conn, &alice).unwrap();
    assert_eq!(count, 2);
    // Bob's reaction remains
    let groups = get_reactions_for_message(&conn, msg, &bob).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].count, 1);
    // Alice's reaction on msg2 gone
    let groups2 = get_reactions_for_message(&conn, msg2, &bob).unwrap();
    assert!(groups2.is_empty());
}
```

In `attachments.rs` tests:

```rust
#[test]
fn test_remove_attachments_for_author_messages() {
    let (conn, dir) = setup();
    let alice = Keypair::generate().public_key();
    let bob = Keypair::generate().public_key();
    crate::members::register_member(&conn, &alice, "Alice").unwrap();
    crate::members::register_member(&conn, &bob, "Bob").unwrap();
    let ch = crate::channels::create_channel(&conn, "gen", farder_protocol::server::ChannelType::Text, None, 0).unwrap();

    let data = b"file content";
    let hash = compute_sha256(data);
    let file_id = store_file(&conn, &dir, &alice, "f.txt", data, &hash, "text/plain", None, None, None).unwrap();

    let msg_alice = crate::messages::insert_message(&conn, ch, &alice, "alice", None).unwrap();
    create_message_attachment(&conn, msg_alice, file_id, 0, "f.txt", None, None, None).unwrap();

    let msg_bob = crate::messages::insert_message(&conn, ch, &bob, "bob", None).unwrap();

    let orphans = remove_attachments_for_author_messages(&conn, &alice).unwrap();
    assert!(orphans.contains(&file_id));

    // Alice's message has no attachments now
    let attachments = get_attachments_for_message(&conn, msg_alice).unwrap();
    assert!(attachments.is_empty());

    cleanup(&dir);
}
```

- [ ] **Step 2: Implement anonymize_messages_by_author**

Add to `messages.rs`:

```rust
use farder_protocol::server::DELETED_USER_KEY;

/// Anonymize all messages by a given author: set author to sentinel key, content to "[deleted]".
/// Updates FTS5 entries. Returns number of messages anonymized.
pub fn anonymize_messages_by_author(conn: &Connection, author: &PublicKey) -> Result<u64> {
    // Get all message IDs by this author
    let mut stmt = conn.prepare("SELECT id, content FROM messages WHERE author = ?1")?;
    let rows: Vec<(i64, String)> = stmt.query_map(
        params![author.as_bytes().as_slice()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?.filter_map(|r| r.ok()).collect();

    let count = rows.len() as u64;
    let sentinel = DELETED_USER_KEY.as_slice();

    for (msg_id, old_content) in &rows {
        // Update FTS5 (content-backed: delete old, insert new)
        let _ = conn.execute(
            "INSERT INTO messages_fts(messages_fts, rowid, content) VALUES ('delete', ?1, ?2)",
            params![msg_id, old_content],
        );
        conn.execute(
            "INSERT INTO messages_fts(rowid, content) VALUES (?1, ?2)",
            params![msg_id, "[deleted]"],
        )?;
    }

    // Bulk update messages
    conn.execute(
        "UPDATE messages SET author = ?1, content = '[deleted]' WHERE author = ?2",
        params![sentinel, author.as_bytes().as_slice()],
    )?;

    Ok(count)
}
```

- [ ] **Step 3: Implement delete_reactions_by_user**

Add to `reactions.rs`:

```rust
/// Delete all reactions by a specific user across all messages. Returns count deleted.
pub fn delete_reactions_by_user(conn: &Connection, user_key: &PublicKey) -> Result<u64> {
    let count = conn.execute(
        "DELETE FROM reactions WHERE user_key = ?1",
        params![user_key.as_bytes().as_slice()],
    )?;
    Ok(count as u64)
}
```

- [ ] **Step 4: Implement remove_attachments_for_author_messages**

Add to `attachments.rs`:

```rust
/// Remove all attachments from messages authored by a given user.
/// Decrements ref_counts and returns file_ids that reached 0.
pub fn remove_attachments_for_author_messages(conn: &Connection, author: &PublicKey) -> Result<Vec<u64>> {
    // Get all message IDs by this author
    let msg_ids: Vec<u64> = {
        let mut stmt = conn.prepare("SELECT id FROM messages WHERE author = ?1")?;
        stmt.query_map(params![author.as_bytes().as_slice()], |row| {
            Ok(row.get::<_, i64>(0)? as u64)
        })?.filter_map(|r| r.ok()).collect()
    };

    let mut all_orphans = Vec::new();
    for msg_id in msg_ids {
        let orphans = delete_attachments_for_message(conn, msg_id)?;
        all_orphans.extend(orphans);
    }
    Ok(all_orphans)
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test --workspace`

- [ ] **Step 6: Commit**

```bash
git add crates/farder-server/src/messages.rs crates/farder-server/src/reactions.rs crates/farder-server/src/attachments.rs
git commit -m "feat(server): add message anonymization, reaction purge by user, and attachment cleanup by author"
```

---

## Task 4: Request Handlers

**Files:**
- Modify: `crates/farder-server/src/handlers.rs`

- [ ] **Step 1: Write tests**

```rust
#[test]
fn test_handle_request_deletion() {
    let (conn, _owner) = setup();
    let user = add_member(&conn, "Alice");
    let result = handle_request(&conn, &user, false, ServerRequest::RequestDeletion).unwrap();
    match result.response {
        ServerResponse::Ok => {}
        other => panic!("expected Ok, got {:?}", other),
    }
    assert!(!result.events.is_empty()); // DeletionRequested event
}

#[test]
fn test_handle_request_deletion_owner_rejected() {
    let (conn, owner) = setup();
    let result = handle_request(&conn, &owner, true, ServerRequest::RequestDeletion).unwrap();
    match result.response {
        ServerResponse::Error { .. } => {}
        other => panic!("expected Error, got {:?}", other),
    }
}

#[test]
fn test_handle_cancel_deletion() {
    let (conn, _owner) = setup();
    let user = add_member(&conn, "Alice");
    handle_request(&conn, &user, false, ServerRequest::RequestDeletion).unwrap();
    let result = handle_request(&conn, &user, false, ServerRequest::CancelDeletion).unwrap();
    match result.response {
        ServerResponse::Ok => {}
        other => panic!("expected Ok, got {:?}", other),
    }
}

#[test]
fn test_handle_get_deletion_status() {
    let (conn, _owner) = setup();
    let user = add_member(&conn, "Alice");
    // No pending request
    let result = handle_request(&conn, &user, false, ServerRequest::GetDeletionStatus).unwrap();
    match result.response {
        ServerResponse::DeletionStatusResp { status } => {
            assert!(!status.pending);
            assert!(status.requested_at.is_none());
        }
        other => panic!("expected DeletionStatusResp, got {:?}", other),
    }
    // Create request
    handle_request(&conn, &user, false, ServerRequest::RequestDeletion).unwrap();
    let result = handle_request(&conn, &user, false, ServerRequest::GetDeletionStatus).unwrap();
    match result.response {
        ServerResponse::DeletionStatusResp { status } => {
            assert!(status.pending);
            assert!(status.requested_at.is_some());
            assert!(status.expires_at.is_some());
        }
        other => panic!("expected DeletionStatusResp, got {:?}", other),
    }
}
```

- [ ] **Step 2: Implement handlers**

Replace the stub handlers:

```rust
ServerRequest::RequestDeletion => {
    if is_owner {
        return err("server owner cannot request deletion — transfer ownership first");
    }
    // Check no existing request
    if members::get_deletion_request(conn, member)?.is_some() {
        return err("deletion request already pending");
    }
    members::create_deletion_request(conn, member)?;
    ok_with(
        ServerResponse::Ok,
        vec![BroadcastEvent {
            target: EventTarget::All,
            event: ServerEvent::DeletionRequested { public_key: member.clone() },
        }],
    )
}

ServerRequest::CancelDeletion => {
    if members::get_deletion_request(conn, member)?.is_none() {
        return err("no pending deletion request");
    }
    members::cancel_deletion_request(conn, member)?;
    ok_with(
        ServerResponse::Ok,
        vec![BroadcastEvent {
            target: EventTarget::All,
            event: ServerEvent::DeletionCancelled { public_key: member.clone() },
        }],
    )
}

ServerRequest::GetDeletionStatus => {
    let status = match members::get_deletion_request(conn, member)? {
        Some(req) => DeletionStatus {
            pending: true,
            requested_at: Some(req.requested_at),
            expires_at: Some(req.expires_at),
        },
        None => DeletionStatus {
            pending: false,
            requested_at: None,
            expires_at: None,
        },
    };
    ok(ServerResponse::DeletionStatusResp { status })
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p farder-server -- handlers`

- [ ] **Step 4: Commit**

```bash
git add crates/farder-server/src/handlers.rs
git commit -m "feat(server): implement RequestDeletion, CancelDeletion, GetDeletionStatus handlers"
```

---

## Task 5: Purge Execution in Retention Task

**Files:**
- Modify: `crates/farder-server/src/retention.rs`

- [ ] **Step 1: Write test**

Add to retention tests:

```rust
#[test]
fn test_execute_pending_deletions() {
    let conn = db::open_in_memory().unwrap();
    let alice = Keypair::generate().public_key();
    let bob = Keypair::generate().public_key();
    members::register_member(&conn, &alice, "Alice").unwrap();
    members::register_member(&conn, &bob, "Bob").unwrap();

    let ch = channels::create_channel(&conn, "gen", ChannelType::Text, None, 0).unwrap();
    messages::insert_message(&conn, ch, &alice, "alice msg 1", None).unwrap();
    messages::insert_message(&conn, ch, &alice, "alice msg 2", None).unwrap();
    messages::insert_message(&conn, ch, &bob, "bob msg", None).unwrap();

    // Alice adds a reaction
    crate::reactions::add_reaction(&conn, 1, &alice, "👍").unwrap();
    crate::reactions::add_reaction(&conn, 3, &bob, "❤️").unwrap();

    // Create an already-expired deletion request for Alice
    members::create_deletion_request_with_expires(&conn, &alice, 100, 200).unwrap();

    let storage_dir = std::env::temp_dir().join(format!("farder-ret-del-{}", rand::random::<u32>()));
    std::fs::create_dir_all(&storage_dir).unwrap();

    let executed = execute_pending_deletions(&conn, &storage_dir.to_string_lossy()).unwrap();
    assert_eq!(executed, 1);

    // Alice's messages are anonymized
    let sentinel = PublicKey::from_bytes(farder_protocol::server::DELETED_USER_KEY);
    let history = messages::fetch_history(&conn, ch, None, 50, &bob).unwrap();
    let anon: Vec<_> = history.iter().filter(|m| m.author == sentinel).collect();
    assert_eq!(anon.len(), 2);
    assert!(anon.iter().all(|m| m.content == "[deleted]"));

    // Bob's message is untouched
    let bob_msgs: Vec<_> = history.iter().filter(|m| m.author == bob).collect();
    assert_eq!(bob_msgs.len(), 1);

    // Alice's reaction is gone, Bob's remains
    let r1 = crate::reactions::get_reactions_for_message(&conn, 1, &bob).unwrap();
    assert!(r1.is_empty()); // alice's reaction removed
    let r3 = crate::reactions::get_reactions_for_message(&conn, 3, &bob).unwrap();
    assert_eq!(r3.len(), 1); // bob's reaction stays

    // Alice's member record is gone
    assert!(members::get_member(&conn, &alice).unwrap().is_none());

    // Deletion request is gone
    assert!(members::get_deletion_request(&conn, &alice).unwrap().is_none());

    let _ = std::fs::remove_dir_all(&storage_dir);
}
```

- [ ] **Step 2: Implement execute_pending_deletions**

Add to `retention.rs`:

```rust
use crate::{attachments, members, messages, reactions};

/// Execute all expired deletion requests. Returns count of members purged.
pub fn execute_pending_deletions(conn: &Connection, storage_dir: &str) -> Result<u64> {
    let expired = members::list_expired_deletion_requests(conn)?;
    let mut count = 0u64;

    for req in &expired {
        info!(member = %req.member_key, "executing data deletion");

        // 1. Remove attachments from this user's messages (decrement ref_counts)
        let orphans = attachments::remove_attachments_for_author_messages(conn, &req.member_key)?;
        for fid in orphans {
            let _ = attachments::cleanup_orphaned_file(conn, storage_dir, fid);
        }

        // 2. Anonymize messages (author → sentinel, content → [deleted], FTS updated)
        let msg_count = messages::anonymize_messages_by_author(conn, &req.member_key)?;
        info!(member = %req.member_key, messages = msg_count, "anonymized messages");

        // 3. Delete all reactions by this user
        let reaction_count = reactions::delete_reactions_by_user(conn, &req.member_key)?;
        info!(member = %req.member_key, reactions = reaction_count, "removed reactions");

        // 4. Remove member roles and member record
        members::remove_member(conn, &req.member_key)?;

        // 5. Delete the deletion request
        members::delete_deletion_request(conn, &req.member_key)?;

        count += 1;
    }

    Ok(count)
}
```

Update `spawn_retention_task` to call `execute_pending_deletions` each cycle alongside message purging:

In the tick handler, after `purge_expired_messages`, add:

```rust
let deletions = execute_pending_deletions(&conn, storage_dir)?;
if deletions > 0 {
    info!(deletions, "executed pending data deletions");
}
```

The `spawn_retention_task` needs access to `storage_dir` — it already has it via `state.storage_dir` from Phase 3.1.

- [ ] **Step 3: Run tests**

Run: `cargo test -p farder-server -- retention`

- [ ] **Step 4: Commit**

```bash
git add crates/farder-server/src/retention.rs
git commit -m "feat(server): execute pending data deletions in retention task with full purge"
```

---

## Task 6: E2E Integration Test

**Files:**
- Modify: `tests/e2e_server.rs`

- [ ] **Step 1: Add deletion flow to e2e test**

After the existing thread/reaction test flow, add:

```rust
// ---- DATA DELETION FLOW ----

// 19. User requests deletion
send_request(&mut user_send, 5, ServerRequest::RequestDeletion).await;
let (_, resp) = recv_response(&mut user_recv).await;
match resp {
    ServerResponse::Ok => {}
    other => panic!("expected Ok for RequestDeletion, got {:?}", other),
}

// 20. User checks deletion status
send_request(&mut user_send, 6, ServerRequest::GetDeletionStatus).await;
let (_, resp) = recv_response(&mut user_recv).await;
match resp {
    ServerResponse::DeletionStatusResp { status } => {
        assert!(status.pending);
        assert!(status.requested_at.is_some());
        assert!(status.expires_at.is_some());
    }
    other => panic!("expected DeletionStatusResp, got {:?}", other),
}

// 21. User cancels deletion
send_request(&mut user_send, 7, ServerRequest::CancelDeletion).await;
let (_, resp) = recv_response(&mut user_recv).await;
match resp {
    ServerResponse::Ok => {}
    other => panic!("expected Ok for CancelDeletion, got {:?}", other),
}

// 22. Verify status is no longer pending
send_request(&mut user_send, 8, ServerRequest::GetDeletionStatus).await;
let (_, resp) = recv_response(&mut user_recv).await;
match resp {
    ServerResponse::DeletionStatusResp { status } => {
        assert!(!status.pending);
    }
    other => panic!("expected DeletionStatusResp, got {:?}", other),
}
```

- [ ] **Step 2: Run e2e test**

Run: `cargo test e2e_server -- --nocapture`

- [ ] **Step 3: Run full suite**

Run: `cargo test --workspace`

- [ ] **Step 4: Commit**

```bash
git add tests/e2e_server.rs
git commit -m "feat(server): add e2e test for data deletion request, status check, and cancellation"
```

---

## Self-Review Results

**Spec coverage:**
- RequestDeletion handler ✅ Task 4
- CancelDeletion handler ✅ Task 4
- GetDeletionStatus handler ✅ Task 4
- DeletionStatus response type ✅ Task 1
- 72-hour grace period ✅ Task 2 (DELETION_GRACE_PERIOD_SECS)
- Owner cannot request deletion ✅ Task 4
- Duplicate request rejected ✅ Task 4
- DeletionRequested/Cancelled/Executed events ✅ Tasks 1, 4, 5
- Message anonymization (sentinel key + [deleted]) ✅ Task 3
- DELETED_USER_KEY constant ✅ Task 1
- Attachment removal with ref_count ✅ Task 3
- Reaction removal by user ✅ Task 3
- FTS5 update ✅ Task 3
- Member record deletion ✅ Task 5
- Member-role cleanup ✅ Task 5 (via remove_member)
- Retention task integration ✅ Task 5
- Ordering constraint (attachments before anonymize) ✅ Task 5
- Deletion request row cleanup ✅ Task 5
- E2E test ✅ Task 6

**Placeholder scan:** No placeholders found.

**Type consistency:** `DeletionRequest`, `DeletionStatus`, `create_deletion_request`, `get_deletion_request`, `cancel_deletion_request`, `list_expired_deletion_requests`, `delete_deletion_request`, `anonymize_messages_by_author`, `delete_reactions_by_user`, `remove_attachments_for_author_messages`, `execute_pending_deletions` — all consistent.
