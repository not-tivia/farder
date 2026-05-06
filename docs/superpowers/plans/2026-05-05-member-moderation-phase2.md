# Member Moderation Phase 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add timeout (server-enforced silence with banner), audit log (forever-retention table viewable by MANAGE_SERVER holders), and kicked/banned pre-disconnect notifications to the existing Phase 1 moderation system.

**Architecture:** Server-authoritative. New `TIMEOUT_MEMBERS = 1 << 14` permission bit gates the action. Timeout state lives on the existing `members` table (two new columns), broadcast via existing `MembersChanged`-style updates. New `audit_events` table with a single `audit::emit(...)` helper called at the bottom of 14 mutating handlers. Kick/ban handlers send a new `YouWereKicked` / `YouWereBanned` event to the target before tear-down via a new `EventTarget::Members(Vec<PublicKey>)` broadcast variant.

**Tech Stack:** Rust (server, protocol, Tauri client crate), TypeScript + React (renderer). SQLite via rusqlite. No new external deps.

**Spec:** `docs/superpowers/specs/2026-05-05-member-moderation-phase2-design.md`

---

## File structure

**New:**
- Server: `crates/farder-server/src/audit.rs`
- Client TS: `client/src/components/TimeoutDialog.tsx`, `client/src/components/TimeoutBanner.tsx`, `client/src/components/AuditLogTab.tsx`, `client/src/components/KickedBannedDialog.tsx`

**Modified:**
- Protocol: `crates/farder-protocol/src/server.rs`
- Server: `crates/farder-server/src/{permissions,db,members,handlers,events,connection,lib}.rs`
- Client crate: `client/src-tauri/src/{commands,bridge,main}.rs`
- Client TS: `client/src/lib/{permissions,tauri-bridge}.ts`, `client/src/hooks/useServerEvents.ts`, `client/src/components/{MemberContextMenu,MessageInput,ServerSettingsDialog,AppShell}.tsx`
- Docs: `CHANGELOG.md`

---

## Task 1: Protocol additions + EventTarget variant

The wire-format foundation. Everything else depends on this compiling.

**Files:**
- Modify: `crates/farder-protocol/src/server.rs`
- Modify: `crates/farder-server/src/events.rs`

- [ ] **Step 1: Add new types and variants to `crates/farder-protocol/src/server.rs`**

After the existing `BannedMember` struct (around line 148), add:

```rust
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AuditEvent {
    pub id: u64,
    pub actor: PublicKey,
    #[serde(default)]
    pub target: Option<PublicKey>,
    pub action: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub timestamp_ms: u64,
}
```

In the `MemberInfo` struct, add two new fields (with `#[serde(default)]` for backwards-compatibility):

```rust
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MemberInfo {
    pub public_key: PublicKey,
    pub display_name: String,
    pub joined_at: u64,
    pub role_ids: Vec<u64>,
    #[serde(default)]
    pub timeout_until: Option<u64>,
    #[serde(default)]
    pub timeout_reason: Option<String>,
}
```

In `enum ServerRequest { ... }`, add three new variants alongside the existing moderation requests:

```rust
TimeoutMember { member_key: PublicKey, until_ms: u64, reason: Option<String> },
RemoveTimeout { member_key: PublicKey },
ListAuditEvents { before_id: Option<u64>, limit: u32 },
```

In `enum ServerResponse { ... }`, add:

```rust
AuditEventsList { events: Vec<AuditEvent> },
```

In `enum ServerEvent { ... }`, add:

```rust
MemberTimeoutChanged {
    public_key: PublicKey,
    #[serde(default)]
    until_ms: Option<u64>,
    #[serde(default)]
    reason: Option<String>,
},
YouWereKicked,
YouWereBanned {
    #[serde(default)]
    reason: Option<String>,
},
AuditEventCreated { event: AuditEvent },
```

(There is no separate `ServerError` enum — errors flow through `ServerResponse::Error { reason: String }`. The `TimedOut` error from the spec becomes a `ServerResponse::Error { reason: format!("timed out until {}{}", until_ms, reason_str) }` returned from handlers; the client will parse the prefix to render its banner appropriately. See Task 5 for the exact format.)

- [ ] **Step 2: Add the new EventTarget variant**

In `crates/farder-server/src/events.rs`, change the enum to:

```rust
use farder_crypto::identity::PublicKey;
use farder_protocol::server::ServerEvent;

#[derive(Debug)]
pub enum EventTarget {
    All,
    Subscribers(u64),                   // clients subscribed to this channel
    Members(Vec<PublicKey>),            // specific clients by public key
    PermissionHolders(u64),             // clients whose resolved server perms include this bit
}

#[derive(Debug)]
pub struct BroadcastEvent {
    pub target: EventTarget,
    pub event: ServerEvent,
}
```

- [ ] **Step 3: Verify the workspace compiles**

```
cd /home/deez/farder && cargo check --workspace 2>&1 | tail -10
```

Expected: `Finished` (broadcast_event in connection.rs will warn about unused match arms; that's fixed in Task 9).

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add crates/farder-protocol/src/server.rs crates/farder-server/src/events.rs
git -C /home/deez/farder commit -m "feat(protocol): Phase 2 moderation additions (timeout, audit, kicked/banned events)"
```

---

## Task 2: Database schema (members columns + audit_events table)

**Files:**
- Modify: `crates/farder-server/src/db.rs`

- [ ] **Step 1: Add the schema migrations**

In `crates/farder-server/src/db.rs::init_schema`, after the existing `ban_reason` migration block (around line 211), add:

```rust
    // Members: add timeout_until + timeout_reason columns (Phase 2 moderation).
    let has_timeout_until: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(members)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols.iter().any(|c| c == "timeout_until")
    };
    if !has_timeout_until {
        conn.execute(
            "ALTER TABLE members ADD COLUMN timeout_until INTEGER NULL",
            [],
        )?;
        conn.execute(
            "ALTER TABLE members ADD COLUMN timeout_reason TEXT NULL",
            [],
        )?;
    }

    // Audit events: forever-retention moderator action log (Phase 2).
    conn.execute(
        "CREATE TABLE IF NOT EXISTS audit_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            actor_pk BLOB NOT NULL,
            target_pk BLOB,
            action TEXT NOT NULL,
            metadata TEXT NOT NULL DEFAULT '{}',
            timestamp_ms INTEGER NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_audit_events_timestamp ON audit_events(timestamp_ms DESC)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_audit_events_actor ON audit_events(actor_pk)",
        [],
    )?;
```

- [ ] **Step 2: Verify it compiles**

```
cd /home/deez/farder && cargo check -p farder-server 2>&1 | tail -3
```

Expected: `Finished`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/db.rs
git -C /home/deez/farder commit -m "feat(server): schema for timeout columns + audit_events table"
```

---

## Task 3: Permission bit + members.rs timeout helpers

**Files:**
- Modify: `crates/farder-server/src/permissions.rs`
- Modify: `crates/farder-server/src/members.rs`

- [ ] **Step 1: Add the permission constant**

In `crates/farder-server/src/permissions.rs`, after the existing `BAN_MEMBERS` line:

```rust
pub const TIMEOUT_MEMBERS: u64 = 1 << 14;
```

In `ALL_PERMISSIONS`, add `| TIMEOUT_MEMBERS`. In the test module's `ALL_INDIVIDUAL` slice, add `TIMEOUT_MEMBERS` to the list.

(Note: `CREATE_INVITES` is already 1 << 13 — `TIMEOUT_MEMBERS = 1 << 14` is the next free bit.)

- [ ] **Step 2: Write the failing test for timeout helpers**

In `crates/farder-server/src/members.rs`, near the bottom inside `#[cfg(test)] mod tests { ... }`, add:

```rust
    #[test]
    fn test_set_and_get_timeout() {
        let conn = test_conn();
        let kp = farder_crypto::identity::Keypair::generate();
        let pk = kp.public_key();
        register_member(&conn, &pk, "alice").unwrap();

        // No timeout initially.
        assert_eq!(is_timed_out(&conn, &pk, 1000).unwrap(), None);

        // Set a timeout that hasn't expired.
        set_timeout(&conn, &pk, 5000, Some("warning")).unwrap();
        let active = is_timed_out(&conn, &pk, 1000).unwrap();
        assert_eq!(active, Some((5000, Some("warning".to_string()))));

        // Past `until_ms` → returns None and lazily clears the column.
        let active = is_timed_out(&conn, &pk, 6000).unwrap();
        assert_eq!(active, None);
        // Re-checking confirms the column is cleared.
        let active = is_timed_out(&conn, &pk, 1000).unwrap();
        assert_eq!(active, None);
    }

    #[test]
    fn test_clear_timeout() {
        let conn = test_conn();
        let kp = farder_crypto::identity::Keypair::generate();
        let pk = kp.public_key();
        register_member(&conn, &pk, "bob").unwrap();
        set_timeout(&conn, &pk, 9999, None).unwrap();

        clear_timeout(&conn, &pk).unwrap();
        assert_eq!(is_timed_out(&conn, &pk, 1000).unwrap(), None);
    }
```

- [ ] **Step 3: Run the failing tests**

```
cd /home/deez/farder && cargo test -p farder-server members::tests::test_set_and_get_timeout members::tests::test_clear_timeout 2>&1 | tail -20
```

Expected: both fail with "function not found" or similar (helpers not implemented yet).

- [ ] **Step 4: Implement the helpers**

In `crates/farder-server/src/members.rs`, after the existing `unban_member` function (around line 113):

```rust
pub fn set_timeout(conn: &Connection, pk: &PublicKey, until_ms: u64, reason: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE members SET timeout_until = ?1, timeout_reason = ?2 WHERE public_key = ?3",
        rusqlite::params![until_ms as i64, reason, pk.as_bytes().as_slice()],
    )?;
    Ok(())
}

pub fn clear_timeout(conn: &Connection, pk: &PublicKey) -> Result<()> {
    conn.execute(
        "UPDATE members SET timeout_until = NULL, timeout_reason = NULL WHERE public_key = ?1",
        rusqlite::params![pk.as_bytes().as_slice()],
    )?;
    Ok(())
}

/// Returns the active timeout details if `now_ms < timeout_until`. If the timeout has
/// expired, lazily clears the column so future reads are clean and returns None.
pub fn is_timed_out(conn: &Connection, pk: &PublicKey, now_ms: u64) -> Result<Option<(u64, Option<String>)>> {
    let row: Option<(Option<i64>, Option<String>)> = conn
        .query_row(
            "SELECT timeout_until, timeout_reason FROM members WHERE public_key = ?1",
            rusqlite::params![pk.as_bytes().as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    match row {
        Some((Some(until_i64), reason)) => {
            let until_ms = until_i64 as u64;
            if now_ms < until_ms {
                Ok(Some((until_ms, reason)))
            } else {
                // Expired — clear lazily.
                clear_timeout(conn, pk)?;
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}
```

- [ ] **Step 5: Update `list_members` and `get_member` to populate the new MemberInfo fields**

Find the `SELECT public_key, display_name, joined_at FROM members ...` queries in `members.rs` that build `MemberRecord` / `MemberInfo` structs (search for `display_name`). Adjust the SELECT to also return `timeout_until, timeout_reason` and the row mapping to populate the new fields on `MemberInfo`.

For example, the existing `list_members` returns `MemberRecord` (different struct). The MemberInfo construction happens elsewhere — find it via:

```
grep -n "MemberInfo {" /home/deez/farder/crates/farder-server/src/
```

Wherever a `MemberInfo { public_key, display_name, joined_at, role_ids }` is being constructed, change it to also include `timeout_until: ..., timeout_reason: ...`. The values come from the same row as joined_at — extend the SELECT to include the two new columns and pass them through.

If MemberInfo is constructed in only one place that doesn't currently fetch timeout data, fetch it inline:

```rust
let (timeout_until, timeout_reason) = members::is_timed_out(conn, &pk, current_unix_ms())
    .ok()
    .flatten()
    .map(|(t, r)| (Some(t), r))
    .unwrap_or((None, None));
```

(Define `current_unix_ms()` inline if needed: `std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64`.)

- [ ] **Step 6: Run the tests**

```
cd /home/deez/farder && cargo test -p farder-server members::tests::test_set_and_get_timeout members::tests::test_clear_timeout 2>&1 | tail -15
```

Expected: both pass.

- [ ] **Step 7: Verify the full test suite still passes**

```
cd /home/deez/farder && cargo test -p farder-server 2>&1 | tail -10
```

Expected: all pre-existing tests still pass; no new failures.

- [ ] **Step 8: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/permissions.rs crates/farder-server/src/members.rs
git -C /home/deez/farder commit -m "feat(server): TIMEOUT_MEMBERS perm + members.rs timeout helpers"
```

---

## Task 4: Timeout enforcement at handler tops

Insert the `is_timed_out` check at the top of the four affected handler arms.

**Files:**
- Modify: `crates/farder-server/src/handlers.rs`

- [ ] **Step 1: Write failing tests**

In `crates/farder-server/src/handlers.rs` test module (near the bottom — find `#[cfg(test)] mod tests { ... }`), add:

```rust
    #[test]
    fn test_timed_out_send_message_rejected() {
        let conn = setup_test_db();
        let alice = test_member(&conn, "alice");
        let now = 1000u64;
        let until = 5000u64;
        members::set_timeout(&conn, &alice, until, Some("spam")).unwrap();

        let result = handle_request(&conn, &alice, false, ServerRequest::SendMessage {
            channel_id: 1,
            content: "hello".into(),
            reply_to: None,
            attachment_ids: vec![],
        }, "/tmp").unwrap();
        match result.response {
            ServerResponse::Error { reason } => assert!(reason.contains("timed out"), "expected timed-out error, got: {reason}"),
            other => panic!("expected error, got {other:?}"),
        }
    }
```

(`setup_test_db` and `test_member` are existing helpers — search for `fn setup_test_db` near the top of the test module to confirm. If the test helper has different names, mirror the pattern of `test_handle_ban_member` at line 1434.)

- [ ] **Step 2: Run failing test**

```
cd /home/deez/farder && cargo test -p farder-server handlers::tests::test_timed_out_send_message_rejected 2>&1 | tail -15
```

Expected: fails (the message gets sent because we haven't added the check).

- [ ] **Step 3: Add the timeout-check helper**

In `crates/farder-server/src/handlers.rs`, alongside `require_base_perm` and `require_member_hierarchy` (around line 188), add:

```rust
fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn require_not_timed_out(conn: &Connection, member: &PublicKey) -> Result<Option<HandleResult>> {
    if let Some((until_ms, reason)) = members::is_timed_out(conn, member, current_unix_ms())? {
        let reason_part = reason.map(|r| format!(": {r}")).unwrap_or_default();
        return Ok(Some(HandleResult {
            response: ServerResponse::Error {
                reason: format!("timed out until {until_ms}{reason_part}"),
            },
            events: Vec::new(),
            orphaned_file_ids: vec![],
        }));
    }
    Ok(None)
}
```

The error format is `timed out until <unix_ms>[: <reason>]`. The client parses this prefix to render the banner if it ever sees it (defense-in-depth — primarily the banner is driven by the `MemberTimeoutChanged` event, not error parsing).

- [ ] **Step 4: Insert the check at four handler arm tops**

Inside `handle_request`, at the very top of each of these arms (BEFORE any other validation):

`ServerRequest::SendMessage { ... } =>` — add as the first statement:
```rust
            if let Some(denied) = require_not_timed_out(conn, member)? {
                return Ok(denied);
            }
```

Same for `ServerRequest::AddReaction { ... } =>`, `ServerRequest::JoinVoice { ... } =>`, `ServerRequest::SetDisplayName { ... } =>`.

(For SetDisplayName: find it via `grep -n "SetDisplayName" /home/deez/farder/crates/farder-server/src/handlers.rs`. If the request variant doesn't exist yet — Phase 2 spec says edit-nickname should be blocked but the codebase may not have this request yet — search for whatever request handles display_name updates. If display_name is currently set only at registration time and there's no request for changing it, skip blocking it for this phase and add a note: "edit-nickname blocking deferred — no request variant exists". This is YAGNI-compatible with the spec since the spec says "block edit-nickname" but if the feature doesn't exist, blocking it is moot.)

- [ ] **Step 5: Run the test**

```
cd /home/deez/farder && cargo test -p farder-server handlers::tests::test_timed_out_send_message_rejected 2>&1 | tail -10
```

Expected: pass.

- [ ] **Step 6: Run the full server test suite**

```
cd /home/deez/farder && cargo test -p farder-server 2>&1 | tail -10
```

Expected: all pass.

- [ ] **Step 7: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/handlers.rs
git -C /home/deez/farder commit -m "feat(server): block send/react/voice/nickname while timed out"
```

---

## Task 5: TimeoutMember + RemoveTimeout handlers

**Files:**
- Modify: `crates/farder-server/src/handlers.rs`

- [ ] **Step 1: Write failing tests**

In the same test module as Task 4:

```rust
    #[test]
    fn test_timeout_member_requires_perm() {
        let conn = setup_test_db();
        let actor = test_member(&conn, "actor");
        let target = test_member(&conn, "target");

        // Actor has no TIMEOUT_MEMBERS perm.
        let result = handle_request(&conn, &actor, false, ServerRequest::TimeoutMember {
            member_key: target.clone(),
            until_ms: current_unix_ms() + 60_000,
            reason: None,
        }, "/tmp").unwrap();
        match result.response {
            ServerResponse::Error { reason } => assert!(reason.contains("TIMEOUT_MEMBERS"), "got: {reason}"),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn test_timeout_member_rejects_invalid_duration() {
        let conn = setup_test_db();
        let owner = test_owner(&conn);  // owner bypasses perms
        let target = test_member(&conn, "target");
        let now = current_unix_ms();

        // until_ms in the past.
        let result = handle_request(&conn, &owner, true, ServerRequest::TimeoutMember {
            member_key: target.clone(),
            until_ms: now.saturating_sub(1000),
            reason: None,
        }, "/tmp").unwrap();
        assert!(matches!(result.response, ServerResponse::Error { ref reason } if reason.contains("out of range")));

        // until_ms > now + 28d.
        let too_far = now + 29 * 24 * 60 * 60 * 1000;
        let result = handle_request(&conn, &owner, true, ServerRequest::TimeoutMember {
            member_key: target.clone(),
            until_ms: too_far,
            reason: None,
        }, "/tmp").unwrap();
        assert!(matches!(result.response, ServerResponse::Error { ref reason } if reason.contains("out of range")));
    }

    #[test]
    fn test_timeout_member_happy_path() {
        let conn = setup_test_db();
        let owner = test_owner(&conn);
        let target = test_member(&conn, "target");
        let until = current_unix_ms() + 60_000;

        let result = handle_request(&conn, &owner, true, ServerRequest::TimeoutMember {
            member_key: target.clone(),
            until_ms: until,
            reason: Some("warning".into()),
        }, "/tmp").unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_eq!(result.events.len(), 1);
        match &result.events[0].event {
            ServerEvent::MemberTimeoutChanged { until_ms: Some(u), reason: Some(r), .. } => {
                assert_eq!(*u, until);
                assert_eq!(r, "warning");
            }
            other => panic!("expected MemberTimeoutChanged, got {other:?}"),
        }
    }

    #[test]
    fn test_remove_timeout_clears() {
        let conn = setup_test_db();
        let owner = test_owner(&conn);
        let target = test_member(&conn, "target");
        members::set_timeout(&conn, &target, current_unix_ms() + 60_000, Some("oops")).unwrap();

        let result = handle_request(&conn, &owner, true, ServerRequest::RemoveTimeout {
            member_key: target.clone(),
        }, "/tmp").unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        assert_eq!(members::is_timed_out(&conn, &target, current_unix_ms()).unwrap(), None);
    }
```

(`test_owner` may not exist yet — if so, add it near the test helpers: `fn test_owner(conn: &Connection) -> PublicKey { let kp = Keypair::generate(); let pk = kp.public_key(); members::register_member(conn, &pk, "owner").unwrap(); pk }`. The handler uses the `is_owner: bool` flag passed in — owner identity is by-flag, not by-DB-row.)

- [ ] **Step 2: Run failing tests**

```
cd /home/deez/farder && cargo test -p farder-server handlers::tests::test_timeout 2>&1 | tail -20
```

Expected: all four fail with `unknown variant TimeoutMember` or "match arm not exhaustive".

- [ ] **Step 3: Implement the handler arms**

In `crates/farder-server/src/handlers.rs`, in the `handle_request` match block, after the existing `BanMember` / `UnbanMember` arms (around line 740):

```rust
        ServerRequest::TimeoutMember { member_key, until_ms, reason } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::TIMEOUT_MEMBERS, "TIMEOUT_MEMBERS")? {
                return Ok(denied);
            }
            if let Some(denied) = require_member_hierarchy(conn, member, is_owner, &member_key)? {
                return Ok(denied);
            }
            let now = current_unix_ms();
            const MAX_TIMEOUT_MS: u64 = 28 * 24 * 60 * 60 * 1000;
            if until_ms <= now || until_ms > now + MAX_TIMEOUT_MS {
                return err("timeout duration out of range (must be in the future, max 28 days)");
            }
            members::set_timeout(conn, &member_key, until_ms, reason.as_deref())?;
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberTimeoutChanged {
                    public_key: member_key,
                    until_ms: Some(until_ms),
                    reason: reason.clone(),
                },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        ServerRequest::RemoveTimeout { member_key } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::TIMEOUT_MEMBERS, "TIMEOUT_MEMBERS")? {
                return Ok(denied);
            }
            if let Some(denied) = require_member_hierarchy(conn, member, is_owner, &member_key)? {
                return Ok(denied);
            }
            members::clear_timeout(conn, &member_key)?;
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberTimeoutChanged {
                    public_key: member_key,
                    until_ms: None,
                    reason: None,
                },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }
```

- [ ] **Step 4: Run tests**

```
cd /home/deez/farder && cargo test -p farder-server handlers::tests::test_timeout handlers::tests::test_remove_timeout 2>&1 | tail -15
```

Expected: all four pass.

- [ ] **Step 5: Full test suite**

```
cd /home/deez/farder && cargo test -p farder-server 2>&1 | tail -10
```

Expected: all pass.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/handlers.rs
git -C /home/deez/farder commit -m "feat(server): TimeoutMember + RemoveTimeout handlers"
```

---

## Task 6: audit.rs module

The audit-event helper, list query, and a new EventTarget broadcast resolver.

**Files:**
- Create: `crates/farder-server/src/audit.rs`
- Modify: `crates/farder-server/src/lib.rs`
- Modify: `crates/farder-server/src/connection.rs` (broadcast_event for new EventTarget variants)

- [ ] **Step 1: Create the module file with tests baked in**

`crates/farder-server/src/audit.rs`:

```rust
use anyhow::Result;
use farder_crypto::identity::PublicKey;
use farder_protocol::server::AuditEvent;
use rusqlite::Connection;
use serde_json::Value;

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Insert an audit event row and return the populated AuditEvent struct.
/// Caller is responsible for broadcasting the AuditEventCreated event.
pub fn insert(
    conn: &Connection,
    actor: &PublicKey,
    target: Option<&PublicKey>,
    action: &str,
    metadata: Value,
) -> Result<AuditEvent> {
    let timestamp_ms = current_unix_ms();
    let metadata_str = metadata.to_string();
    conn.execute(
        "INSERT INTO audit_events (actor_pk, target_pk, action, metadata, timestamp_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            actor.as_bytes().as_slice(),
            target.map(|t| t.as_bytes().to_vec()),
            action,
            metadata_str,
            timestamp_ms as i64,
        ],
    )?;
    let id = conn.last_insert_rowid() as u64;
    Ok(AuditEvent {
        id,
        actor: *actor,
        target: target.copied(),
        action: action.to_string(),
        metadata,
        timestamp_ms,
    })
}

/// List audit events newest-first. `before_id` is exclusive (events with id < before_id).
/// `limit` is server-clamped to 100.
pub fn list(conn: &Connection, before_id: Option<u64>, limit: u32) -> Result<Vec<AuditEvent>> {
    let limit = (limit as i64).min(100);
    let (sql, rows) = match before_id {
        Some(bid) => {
            let sql = "SELECT id, actor_pk, target_pk, action, metadata, timestamp_ms
                       FROM audit_events WHERE id < ?1 ORDER BY id DESC LIMIT ?2";
            let mut stmt = conn.prepare(sql)?;
            let rows: Vec<AuditEvent> = stmt
                .query_map(rusqlite::params![bid as i64, limit], row_to_event)?
                .filter_map(|r| r.ok())
                .collect();
            (sql, rows)
        }
        None => {
            let sql = "SELECT id, actor_pk, target_pk, action, metadata, timestamp_ms
                       FROM audit_events ORDER BY id DESC LIMIT ?1";
            let mut stmt = conn.prepare(sql)?;
            let rows: Vec<AuditEvent> = stmt
                .query_map(rusqlite::params![limit], row_to_event)?
                .filter_map(|r| r.ok())
                .collect();
            (sql, rows)
        }
    };
    let _ = sql; // silence unused warning when both branches share format
    Ok(rows)
}

fn row_to_event(row: &rusqlite::Row) -> rusqlite::Result<AuditEvent> {
    let id: i64 = row.get(0)?;
    let actor_bytes: Vec<u8> = row.get(1)?;
    let target_bytes: Option<Vec<u8>> = row.get(2)?;
    let action: String = row.get(3)?;
    let metadata_str: String = row.get(4)?;
    let timestamp_ms: i64 = row.get(5)?;
    let actor = bytes_to_pk(&actor_bytes)?;
    let target = match target_bytes {
        Some(b) => Some(bytes_to_pk(&b)?),
        None => None,
    };
    let metadata: Value = serde_json::from_str(&metadata_str).unwrap_or(Value::Object(Default::default()));
    Ok(AuditEvent {
        id: id as u64,
        actor,
        target,
        action,
        metadata,
        timestamp_ms: timestamp_ms as u64,
    })
}

fn bytes_to_pk(bytes: &[u8]) -> rusqlite::Result<PublicKey> {
    let arr: [u8; 32] = bytes.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, "expected 32-byte pubkey".into())
    })?;
    Ok(PublicKey::from_bytes(&arr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use farder_crypto::identity::Keypair;
    use serde_json::json;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn test_insert_and_list() {
        let conn = test_conn();
        let actor = Keypair::generate().public_key();
        let target = Keypair::generate().public_key();

        let e1 = insert(&conn, &actor, Some(&target), "kick", json!({})).unwrap();
        let e2 = insert(&conn, &actor, Some(&target), "ban", json!({"reason": "spam"})).unwrap();

        let events = list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 2);
        // Newest first.
        assert_eq!(events[0].id, e2.id);
        assert_eq!(events[0].action, "ban");
        assert_eq!(events[1].id, e1.id);
        assert_eq!(events[1].action, "kick");
    }

    #[test]
    fn test_list_pagination() {
        let conn = test_conn();
        let actor = Keypair::generate().public_key();
        for i in 0..5 {
            insert(&conn, &actor, None, "channel_created", json!({"i": i})).unwrap();
        }
        let first = list(&conn, None, 2).unwrap();
        assert_eq!(first.len(), 2);
        let next = list(&conn, Some(first[1].id), 10).unwrap();
        assert_eq!(next.len(), 3);
        // Ensure no overlap.
        assert!(next.iter().all(|e| e.id < first[1].id));
    }

    #[test]
    fn test_list_clamps_limit() {
        let conn = test_conn();
        let actor = Keypair::generate().public_key();
        for i in 0..150 {
            insert(&conn, &actor, None, "test", json!({"i": i})).unwrap();
        }
        let events = list(&conn, None, 200).unwrap();
        assert_eq!(events.len(), 100);
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/farder-server/src/lib.rs`, add `pub mod audit;` alongside the other `pub mod` declarations.

- [ ] **Step 3: Run audit tests**

```
cd /home/deez/farder && cargo test -p farder-server audit::tests 2>&1 | tail -10
```

Expected: 3 pass.

- [ ] **Step 4: Update `broadcast_event` for the new EventTarget variants**

In `crates/farder-server/src/connection.rs`, find `pub async fn broadcast_event` (around line 803). Replace it with:

```rust
pub async fn broadcast_event(state: &ServerState, target: EventTarget, event: ServerEvent) {
    match target {
        EventTarget::All => {
            let clients = state.clients.read().await;
            for sender in clients.values() {
                let _ = sender.try_send(event.clone());
            }
        }
        EventTarget::Subscribers(channel_id) => {
            let subs = state.subscriptions.read().await;
            if let Some(subscriber_keys) = subs.get(&channel_id) {
                let clients = state.clients.read().await;
                for pk_bytes in subscriber_keys {
                    if let Some(sender) = clients.get(pk_bytes) {
                        let _ = sender.try_send(event.clone());
                    }
                }
            }
        }
        EventTarget::Members(pks) => {
            let clients = state.clients.read().await;
            for pk in pks {
                if let Some(sender) = clients.get(pk.as_bytes()) {
                    let _ = sender.try_send(event.clone());
                }
            }
        }
        EventTarget::PermissionHolders(perm_bit) => {
            let clients = state.clients.read().await;
            let conn = state.db.lock().unwrap();
            // Iterate connected clients, check each one's resolved server perms.
            for (pk_bytes, sender) in clients.iter() {
                if let Ok(pk_arr) = <[u8; 32]>::try_from(pk_bytes.as_slice()) {
                    let pk = farder_crypto::identity::PublicKey::from_bytes(&pk_arr);
                    let is_owner = state.owner_public_key.as_ref().map(|o| o.as_bytes() == pk.as_bytes()).unwrap_or(false);
                    if let Ok(perms) = crate::handlers::resolve_member_server_perms(&conn, &pk, is_owner) {
                        if crate::permissions::has(perms, perm_bit) {
                            let _ = sender.try_send(event.clone());
                        }
                    }
                }
            }
        }
    }
}
```

This requires a helper `resolve_member_server_perms` to exist in `handlers.rs` that resolves a member's server-level (not channel-level) permissions. Search:

```
grep -n "fn resolve_member_perms\|server_perms" /home/deez/farder/crates/farder-server/src/handlers.rs | head
```

If only `resolve_member_perms(conn, member, channel_id, is_owner)` exists (channel-scoped), add a sibling that omits the channel and returns just the member's role-based server perms. Add this above `handle_request`:

```rust
/// Server-level permissions only (no channel/category overrides applied).
pub fn resolve_member_server_perms(
    conn: &Connection,
    member: &PublicKey,
    is_owner: bool,
) -> Result<u64> {
    if is_owner {
        return Ok(permissions::ALL_PERMISSIONS);
    }
    let role_perms = members::get_member_role_permissions(conn, member)?;
    let everyone = members::get_everyone_role_permissions(conn).unwrap_or(permissions::DEFAULT_EVERYONE);
    let ctx = permissions::ResolutionContext {
        everyone_permissions: everyone,
        role_permissions: role_perms,
        category_overrides: vec![],
        channel_overrides: vec![],
        is_owner,
    };
    Ok(permissions::resolve(ctx))
}
```

(If `get_everyone_role_permissions` doesn't exist, look at how `resolve_member_perms` reads everyone's perms and mirror that approach.)

Also, `state.owner_public_key` — verify this field exists on `ServerState`:

```
grep -n "owner_public_key\|owner_pk" /home/deez/farder/crates/farder-server/src/state.rs
```

If the field is named differently, use the correct name. If owner is tracked some other way (e.g. via DB query), substitute that lookup.

- [ ] **Step 5: Verify compile**

```
cd /home/deez/farder && cargo check -p farder-server 2>&1 | tail -10
```

Expected: `Finished` (no errors). Warnings about `Members` and `PermissionHolders` being unused will go away in Tasks 7-9.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/audit.rs crates/farder-server/src/lib.rs crates/farder-server/src/connection.rs crates/farder-server/src/handlers.rs
git -C /home/deez/farder commit -m "feat(server): audit module + broadcast EventTarget::Members/PermissionHolders"
```

---

## Task 7: Audit emission at 14 mutating handler call sites

Wire the audit::insert call + AuditEventCreated broadcast at the bottom of each successful mutating handler. Each call site is a small, mechanical change.

**Files:**
- Modify: `crates/farder-server/src/handlers.rs`

- [ ] **Step 1: Write a single representative failing test**

In the handlers test module:

```rust
    #[test]
    fn test_kick_emits_audit_event() {
        let conn = setup_test_db();
        let owner = test_owner(&conn);
        let target = test_member(&conn, "target");

        let result = handle_request(&conn, &owner, true, ServerRequest::KickMember {
            member_key: target.clone(),
        }, "/tmp").unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        // Should emit MemberLeft AND AuditEventCreated.
        assert_eq!(result.events.len(), 2, "expected 2 events (MemberLeft + AuditEventCreated)");
        let has_audit = result.events.iter().any(|e| matches!(e.event, ServerEvent::AuditEventCreated { .. }));
        assert!(has_audit, "missing AuditEventCreated event");

        // And the audit_events table has a row.
        let events = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "kick");
    }
```

Add `use crate::audit;` near the top of the test module if not already present.

- [ ] **Step 2: Run failing test**

```
cd /home/deez/farder && cargo test -p farder-server handlers::tests::test_kick_emits_audit_event 2>&1 | tail -10
```

Expected: fails (only MemberLeft is emitted; no audit row written).

- [ ] **Step 3: Add a helper for emitting audit events**

In `crates/farder-server/src/handlers.rs`, near `require_base_perm`:

```rust
/// Insert an audit event row and produce the BroadcastEvent for the AuditEventCreated event.
/// Failures here are swallowed (logged) — the parent action has already succeeded.
fn audit_emit(
    conn: &Connection,
    actor: &PublicKey,
    target: Option<&PublicKey>,
    action: &str,
    metadata: serde_json::Value,
) -> Option<BroadcastEvent> {
    match audit::insert(conn, actor, target, action, metadata) {
        Ok(event) => Some(BroadcastEvent {
            target: EventTarget::PermissionHolders(permissions::MANAGE_SERVER),
            event: ServerEvent::AuditEventCreated { event },
        }),
        Err(e) => {
            eprintln!("[audit] insert failed: {e}");
            None
        }
    }
}
```

Add `use crate::audit;` and `use serde_json::json;` at the top of the file.

- [ ] **Step 4: Add audit emission to KickMember**

Find the `ServerRequest::KickMember { member_key } => { ... }` arm. Change the trailing `ok_with(...)` from:

```rust
            let event = BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberLeft { public_key: member_key },
            };
            ok_with(ServerResponse::Ok, vec![event])
```

to:

```rust
            let mut events = vec![BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberLeft { public_key: member_key.clone() },
            }];
            if let Some(audit_evt) = audit_emit(conn, member, Some(&member_key), "kick", json!({})) {
                events.push(audit_evt);
            }
            ok_with(ServerResponse::Ok, events)
```

(Add the `.clone()` on member_key in the MemberLeft event if needed — the audit_emit call passes a reference.)

- [ ] **Step 5: Run the test**

```
cd /home/deez/farder && cargo test -p farder-server handlers::tests::test_kick_emits_audit_event 2>&1 | tail -10
```

Expected: pass.

- [ ] **Step 6: Apply the same pattern to the remaining 13 call sites**

For each handler arm below, after the existing broadcast `vec![event]` is built, push an audit event. Use the metadata schema from the spec.

Locate each arm via `grep -n "ServerRequest::" /home/deez/farder/crates/farder-server/src/handlers.rs` and find the corresponding arm. Then before `ok_with(...)`, insert the audit_emit call.

| Handler arm | action string | metadata json |
|---|---|---|
| `BanMember` | `"ban"` | `json!({"reason": reason})` (capture before move) |
| `UnbanMember` | `"unban"` | `json!({})` |
| `TimeoutMember` | `"timeout"` | `json!({"until_ms": until_ms, "reason": reason})` |
| `RemoveTimeout` | `"untimeout"` | `json!({})` |
| `AssignRole` | `"role_assigned"` | `json!({"role_id": role_id, "role_name": members::get_role(conn, role_id)?.map(|r| r.name).unwrap_or_default()})` |
| `RemoveRole` | `"role_removed"` | `json!({"role_id": role_id, "role_name": ...})` |
| `CreateChannel` | `"channel_created"` | `json!({"channel_id": new_channel.id, "channel_name": new_channel.name, "channel_type": format!("{:?}", new_channel.channel_type)})` |
| `DeleteChannel` | `"channel_deleted"` | `json!({"channel_id": channel_id, "channel_name": prev_channel.name})` (capture before delete) |
| `UpdateChannel` (rename only — when `name.is_some() && name != prev_name`) | `"channel_renamed"` | `json!({"channel_id": channel_id, "old_name": prev_name, "new_name": new_name})` |
| `CreateRole` | `"role_created"` | `json!({"role_id": new_role.id, "role_name": new_role.name, "permissions": new_role.permissions.to_string()})` |
| `DeleteRole` | `"role_deleted"` | `json!({"role_id": role_id, "role_name": prev_name})` (capture before delete) |
| `UpdateRole` (only when permissions changed) | `"role_perms_changed"` | `json!({"role_id": role_id, "old_permissions": prev_perms.to_string(), "new_permissions": new_perms.to_string()})` |
| `SetChannelOverride` | `"channel_overrides_changed"` | `json!({"channel_id": channel_id, "role_id": role_id, "allow": allow.to_string(), "deny": deny.to_string()})` |

Notes for tricky arms:
- **BanMember**: clone reason before consuming it (`let reason_for_audit = reason.clone();`), then use `reason_for_audit` in the json.
- **UpdateChannel**: only emit `"channel_renamed"` when the name changed. Other field updates (topic, slow_mode, etc.) don't get audit events in this phase — that's a YAGNI tradeoff matching the spec's "structural changes" scope.
- **UpdateRole**: only emit when `permissions` was changed. Name-only renames don't count for v2.
- **DeleteChannel** / **DeleteRole**: read the existing record BEFORE deletion to capture name for metadata.
- For permissions in JSON: use `.to_string()` because u64 > 53 bits is unsafe in JS Number. The client will parse strings back to BigInt.

For each call site:
1. Read enough context to capture metadata correctly (e.g. read prev name before delete).
2. Push `audit_emit(conn, member, Some(&target_pk), "action", json!({...}))` (or `None` for target if no clear target like channel_created).
3. Verify `cargo check -p farder-server` after each addition (catches typos cheaper than batch).

- [ ] **Step 7: Add one passing test per audit-emitting action**

For each of the 13 additional arms, add a test mirroring `test_kick_emits_audit_event` that:
1. Performs the action successfully.
2. Asserts an `AuditEventCreated` event is emitted.
3. Asserts `audit::list` returns a row with the expected `action` string and key metadata fields.

Example for ban:
```rust
    #[test]
    fn test_ban_emits_audit_event() {
        let conn = setup_test_db();
        let owner = test_owner(&conn);
        let target = test_member(&conn, "target");
        let result = handle_request(&conn, &owner, true, ServerRequest::BanMember {
            member_key: target.clone(),
            reason: Some("spam".into()),
        }, "/tmp").unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
        let events = audit::list(&conn, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "ban");
        assert_eq!(events[0].metadata["reason"], "spam");
    }
```

Mirror this pattern for the remaining 12. Keep each test small and focused.

- [ ] **Step 8: Run all audit tests**

```
cd /home/deez/farder && cargo test -p farder-server _emits_audit_event 2>&1 | tail -20
```

Expected: 14 pass.

- [ ] **Step 9: Run the full server suite**

```
cd /home/deez/farder && cargo test -p farder-server 2>&1 | tail -10
```

Expected: all pass.

- [ ] **Step 10: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/handlers.rs
git -C /home/deez/farder commit -m "feat(server): emit audit events at 14 mutating handler call sites"
```

---

## Task 8: ListAuditEvents handler

**Files:**
- Modify: `crates/farder-server/src/handlers.rs`

- [ ] **Step 1: Write failing tests**

```rust
    #[test]
    fn test_list_audit_events_requires_manage_server() {
        let conn = setup_test_db();
        let actor = test_member(&conn, "actor");  // no perms
        let result = handle_request(&conn, &actor, false, ServerRequest::ListAuditEvents {
            before_id: None,
            limit: 10,
        }, "/tmp").unwrap();
        match result.response {
            ServerResponse::Error { reason } => assert!(reason.contains("MANAGE_SERVER")),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn test_list_audit_events_returns_paginated() {
        let conn = setup_test_db();
        let owner = test_owner(&conn);
        // Insert 3 events directly.
        for _ in 0..3 {
            audit::insert(&conn, &owner, None, "test", json!({})).unwrap();
        }
        let result = handle_request(&conn, &owner, true, ServerRequest::ListAuditEvents {
            before_id: None,
            limit: 2,
        }, "/tmp").unwrap();
        match result.response {
            ServerResponse::AuditEventsList { events } => {
                assert_eq!(events.len(), 2);
            }
            other => panic!("expected AuditEventsList, got {other:?}"),
        }
    }
```

(Add `use serde_json::json;` to test imports if missing.)

- [ ] **Step 2: Run failing tests**

```
cd /home/deez/farder && cargo test -p farder-server handlers::tests::test_list_audit_events 2>&1 | tail -10
```

Expected: fails — match arm doesn't exist.

- [ ] **Step 3: Implement the handler**

In `handle_request`, near the other moderation arms:

```rust
        ServerRequest::ListAuditEvents { before_id, limit } => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? {
                return Ok(denied);
            }
            let events = audit::list(conn, before_id, limit)?;
            ok(ServerResponse::AuditEventsList { events })
        }
```

- [ ] **Step 4: Run tests**

```
cd /home/deez/farder && cargo test -p farder-server handlers::tests::test_list_audit_events 2>&1 | tail -10
```

Expected: both pass.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/handlers.rs
git -C /home/deez/farder commit -m "feat(server): ListAuditEvents handler (gated by MANAGE_SERVER)"
```

---

## Task 9: YouWereKicked / YouWereBanned pre-disconnect notifications

**Files:**
- Modify: `crates/farder-server/src/handlers.rs`

- [ ] **Step 1: Write a failing test**

```rust
    #[test]
    fn test_kick_emits_you_were_kicked_to_target() {
        let conn = setup_test_db();
        let owner = test_owner(&conn);
        let target = test_member(&conn, "target");

        let result = handle_request(&conn, &owner, true, ServerRequest::KickMember {
            member_key: target.clone(),
        }, "/tmp").unwrap();
        let has_targeted_kick = result.events.iter().any(|e| {
            matches!(e.event, ServerEvent::YouWereKicked) &&
            matches!(&e.target, EventTarget::Members(pks) if pks.contains(&target))
        });
        assert!(has_targeted_kick, "expected YouWereKicked targeted at the kicked member");
    }

    #[test]
    fn test_ban_emits_you_were_banned_to_target() {
        let conn = setup_test_db();
        let owner = test_owner(&conn);
        let target = test_member(&conn, "target");

        let result = handle_request(&conn, &owner, true, ServerRequest::BanMember {
            member_key: target.clone(),
            reason: Some("spam".into()),
        }, "/tmp").unwrap();
        let has_targeted_ban = result.events.iter().any(|e| {
            matches!(&e.event, ServerEvent::YouWereBanned { reason: Some(r) } if r == "spam") &&
            matches!(&e.target, EventTarget::Members(pks) if pks.contains(&target))
        });
        assert!(has_targeted_ban, "expected YouWereBanned targeted at banned member");
    }
```

- [ ] **Step 2: Run failing tests**

```
cd /home/deez/farder && cargo test -p farder-server handlers::tests::test_kick_emits_you_were_kicked test_ban_emits_you_were_banned 2>&1 | tail -10
```

Expected: both fail.

- [ ] **Step 3: Add the events to KickMember and BanMember arms**

In KickMember, before the existing `MemberLeft` event, push:

```rust
            let mut events = vec![BroadcastEvent {
                target: EventTarget::Members(vec![member_key.clone()]),
                event: ServerEvent::YouWereKicked,
            }];
            events.push(BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberLeft { public_key: member_key.clone() },
            });
            if let Some(audit_evt) = audit_emit(conn, member, Some(&member_key), "kick", json!({})) {
                events.push(audit_evt);
            }
            ok_with(ServerResponse::Ok, events)
```

(Replace whatever the prior Task-7 version of this arm produced — this is the final form for KickMember.)

In BanMember, similarly:

```rust
            let reason_for_audit = reason.clone();
            let reason_for_event = reason.clone();
            members::ban_member(conn, &member_key, reason.as_deref())?;
            let mut events = vec![BroadcastEvent {
                target: EventTarget::Members(vec![member_key.clone()]),
                event: ServerEvent::YouWereBanned { reason: reason_for_event },
            }];
            events.push(BroadcastEvent {
                target: EventTarget::All,
                event: ServerEvent::MemberBanned { public_key: member_key.clone(), reason },
            });
            if let Some(audit_evt) = audit_emit(conn, member, Some(&member_key), "ban", json!({"reason": reason_for_audit})) {
                events.push(audit_evt);
            }
            ok_with(ServerResponse::Ok, events)
```

- [ ] **Step 4: Run tests**

```
cd /home/deez/farder && cargo test -p farder-server handlers::tests::test_kick_emits handlers::tests::test_ban_emits 2>&1 | tail -10
```

Expected: all pass.

- [ ] **Step 5: Full server suite + 50ms sleep before disconnect**

The handler-level event is queued; the broadcast machinery sends it before any teardown. Phase 1's kick already lets the connection die naturally on the next failed auth check. Verify no extra sleep is needed by running the full suite:

```
cd /home/deez/farder && cargo test -p farder-server 2>&1 | tail -10
```

Expected: all pass.

(If smoke testing in Task 20 reveals the YouWereKicked frame doesn't reach the client before disconnect, add a 50ms `tokio::time::sleep` after the broadcast inside `connection.rs::broadcast_event` for `EventTarget::Members` only. Don't pre-optimize.)

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/handlers.rs
git -C /home/deez/farder commit -m "feat(server): YouWereKicked/Banned pre-disconnect notifications"
```

---

## Task 10: Tauri commands for timeout/untimeout/list_audit_events

**Files:**
- Modify: `client/src-tauri/src/commands.rs`
- Modify: `client/src-tauri/src/main.rs`

- [ ] **Step 1: Find the existing commands pattern**

```
grep -n "pub async fn ban_member\|pub async fn unban_member" /home/deez/farder/client/src-tauri/src/commands.rs
```

Read 30 lines around the result to mirror style.

- [ ] **Step 2: Add three new commands to `commands.rs`**

After the existing `unban_member` / `list_banned` commands, add:

```rust
#[tauri::command]
pub async fn timeout_member(
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    server_id: String,
    member_pk: String,
    until_ms: u64,
    reason: Option<String>,
) -> Result<(), String> {
    let pk_bytes = hex::decode(&member_pk).map_err(|e| format!("bad pubkey hex: {e}"))?;
    let pk_arr: [u8; 32] = pk_bytes.try_into().map_err(|_| "pubkey must be 32 bytes".to_string())?;
    let pk = farder_crypto::identity::PublicKey::from_bytes(&pk_arr);
    let conn = state.connection_for(&server_id).await.ok_or("not connected")?;
    let req = farder_protocol::server::ServerRequest::TimeoutMember { member_key: pk, until_ms, reason };
    match conn.send_request(req).await.map_err(|e| e.to_string())? {
        farder_protocol::server::ServerResponse::Ok => Ok(()),
        farder_protocol::server::ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {other:?}")),
    }
}

#[tauri::command]
pub async fn remove_timeout(
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    server_id: String,
    member_pk: String,
) -> Result<(), String> {
    let pk_bytes = hex::decode(&member_pk).map_err(|e| format!("bad pubkey hex: {e}"))?;
    let pk_arr: [u8; 32] = pk_bytes.try_into().map_err(|_| "pubkey must be 32 bytes".to_string())?;
    let pk = farder_crypto::identity::PublicKey::from_bytes(&pk_arr);
    let conn = state.connection_for(&server_id).await.ok_or("not connected")?;
    let req = farder_protocol::server::ServerRequest::RemoveTimeout { member_key: pk };
    match conn.send_request(req).await.map_err(|e| e.to_string())? {
        farder_protocol::server::ServerResponse::Ok => Ok(()),
        farder_protocol::server::ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {other:?}")),
    }
}

#[tauri::command]
pub async fn list_audit_events(
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    server_id: String,
    before_id: Option<u64>,
    limit: u32,
) -> Result<Vec<farder_protocol::server::AuditEvent>, String> {
    let conn = state.connection_for(&server_id).await.ok_or("not connected")?;
    let req = farder_protocol::server::ServerRequest::ListAuditEvents { before_id, limit };
    match conn.send_request(req).await.map_err(|e| e.to_string())? {
        farder_protocol::server::ServerResponse::AuditEventsList { events } => Ok(events),
        farder_protocol::server::ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {other:?}")),
    }
}
```

(If `state.connection_for` has a different name in this codebase — e.g. `get_conn` — adjust to match. Verify by reading the existing `ban_member` command body.)

- [ ] **Step 3: Register the commands in `main.rs`**

In `client/src-tauri/src/main.rs`, in the `tauri::generate_handler![...]` block, after the existing moderation commands:

```rust
            commands::timeout_member,
            commands::remove_timeout,
            commands::list_audit_events,
```

- [ ] **Step 4: Verify cargo check**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -3
```

Expected: `Finished`.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs
git -C /home/deez/farder commit -m "feat(client): Tauri commands for timeout/untimeout/list_audit_events"
```

---

## Task 11: bridge.rs event emissions

**Files:**
- Modify: `client/src-tauri/src/bridge.rs`

- [ ] **Step 1: Add four new event emissions**

In `client/src-tauri/src/bridge.rs`, find the existing `app.emit("server:member_banned", ...)` / `app.emit("server:member_unbanned", ...)` calls. After them, add:

```rust
        ServerEvent::MemberTimeoutChanged { public_key, until_ms, reason } =>
            app.emit("server:member_timeout_changed", serde_json::json!({
                "server_id": sid,
                "public_key": public_key.to_string(),
                "until_ms": until_ms,
                "reason": reason
            })),
        ServerEvent::YouWereKicked =>
            app.emit("server:you_were_kicked", serde_json::json!({
                "server_id": sid
            })),
        ServerEvent::YouWereBanned { reason } =>
            app.emit("server:you_were_banned", serde_json::json!({
                "server_id": sid,
                "reason": reason
            })),
        ServerEvent::AuditEventCreated { event } =>
            app.emit("server:audit_event_created", serde_json::json!({
                "server_id": sid,
                "event": event
            })),
```

(Wherever the match block is — look for the block that emits `server:member_banned`. The four new arms go alongside the existing variants, all returning `app.emit(...)` of the same shape.)

- [ ] **Step 2: Verify cargo check**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -3
```

Expected: `Finished`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/bridge.rs
git -C /home/deez/farder commit -m "feat(client): emit timeout-changed, you-were-kicked/banned, audit-event-created"
```

---

## Task 12: Client TS — permissions + tauri-bridge bindings

**Files:**
- Modify: `client/src/lib/permissions.ts`
- Modify: `client/src/lib/tauri-bridge.ts`

- [ ] **Step 1: Add the permission constant**

In `client/src/lib/permissions.ts`, after the existing `BAN_MEMBERS` entry:

```ts
  TIMEOUT_MEMBERS: 1n << 14n,
```

- [ ] **Step 2: Add tauri bridge bindings + types**

In `client/src/lib/tauri-bridge.ts`, after the existing `unbanMember` / `listBanned` exports:

```ts
export interface AuditEvent {
  id: number;
  actor: PublicKey;
  target: PublicKey | null;
  action: string;
  metadata: Record<string, unknown>;
  timestamp_ms: number;
}

export async function timeoutMember(
  serverId: string,
  memberPk: string,
  untilMs: number,
  reason: string | null,
): Promise<void> {
  return invoke<void>("timeout_member", { serverId, memberPk, untilMs, reason });
}

export async function removeTimeout(serverId: string, memberPk: string): Promise<void> {
  return invoke<void>("remove_timeout", { serverId, memberPk });
}

export async function listAuditEvents(
  serverId: string,
  beforeId: number | null,
  limit: number,
): Promise<AuditEvent[]> {
  return invoke<AuditEvent[]>("list_audit_events", { serverId, beforeId, limit });
}
```

If `PublicKey` is imported from elsewhere (`./types`), match the existing import. Add `import` at the top if missing.

Also, in the existing `MemberInfo` TS type definition (likely in `lib/types.ts`):

```ts
export interface MemberInfo {
  public_key: PublicKey;
  display_name: string;
  joined_at: number;
  role_ids: number[];
  timeout_until?: number | null;
  timeout_reason?: string | null;
}
```

(Both new fields are optional to mirror the Rust `#[serde(default)]`.)

- [ ] **Step 3: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/lib/permissions.ts client/src/lib/tauri-bridge.ts client/src/lib/types.ts
git -C /home/deez/farder commit -m "feat(client): TIMEOUT_MEMBERS perm + audit/timeout TS bindings + MemberInfo fields"
```

---

## Task 13: TimeoutDialog component

**Files:**
- Create: `client/src/components/TimeoutDialog.tsx`

- [ ] **Step 1: Write the component**

`client/src/components/TimeoutDialog.tsx`:

```tsx
import { useMemo, useState, type CSSProperties } from "react";

interface Props {
  targetName: string;
  onCancel: () => void;
  onConfirm: (untilMs: number, reason: string | null) => void;
}

const overlay: CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(0,0,0,0.4)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 2400,
};

const card: CSSProperties = {
  background: "var(--xp-window-bg, #ECE9D8)",
  color: "var(--xp-text-normal, #000)",
  border: "2px solid var(--xp-blue-dark, #003C74)",
  padding: 20,
  width: 420,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
};

const PRESETS: Array<{ label: string; ms: number }> = [
  { label: "60 seconds", ms: 60 * 1000 },
  { label: "5 minutes", ms: 5 * 60 * 1000 },
  { label: "10 minutes", ms: 10 * 60 * 1000 },
  { label: "1 hour", ms: 60 * 60 * 1000 },
  { label: "1 day", ms: 24 * 60 * 60 * 1000 },
  { label: "1 week", ms: 7 * 24 * 60 * 60 * 1000 },
];

const MAX_DURATION_MS = 28 * 24 * 60 * 60 * 1000;

export default function TimeoutDialog({ targetName, onCancel, onConfirm }: Props) {
  const [presetIdx, setPresetIdx] = useState<number>(1); // default 5 minutes
  const [useCustom, setUseCustom] = useState(false);
  const [customAmount, setCustomAmount] = useState<number>(30);
  const [customUnit, setCustomUnit] = useState<"minutes" | "hours" | "days">("minutes");
  const [reason, setReason] = useState("");

  const durationMs = useMemo(() => {
    if (!useCustom) return PRESETS[presetIdx].ms;
    const mult = customUnit === "minutes" ? 60_000 : customUnit === "hours" ? 3_600_000 : 86_400_000;
    return Math.max(1, customAmount) * mult;
  }, [useCustom, presetIdx, customAmount, customUnit]);

  const clampedMs = Math.min(durationMs, MAX_DURATION_MS);
  const untilMs = Date.now() + clampedMs;
  const untilDate = new Date(untilMs);

  return (
    <div style={overlay} onClick={onCancel}>
      <div style={card} onClick={(e) => e.stopPropagation()}>
        <h3 style={{ marginTop: 0 }}>Timeout {targetName}?</h3>
        <p style={{ fontSize: 11, color: "var(--xp-text-muted, #666)" }}>
          They won't be able to send messages, react, join voice, or change their nickname for the chosen duration.
        </p>

        {!useCustom && (
          <div style={{ display: "flex", flexDirection: "column", gap: 4, marginTop: 8 }}>
            {PRESETS.map((p, i) => (
              <label key={i} style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 11 }}>
                <input
                  type="radio"
                  name="timeout-preset"
                  checked={presetIdx === i}
                  onChange={() => setPresetIdx(i)}
                />
                {p.label}
              </label>
            ))}
          </div>
        )}

        {useCustom && (
          <div style={{ display: "flex", gap: 6, alignItems: "center", marginTop: 8 }}>
            <input
              type="number"
              min={1}
              value={customAmount}
              onChange={(e) => setCustomAmount(Math.max(1, parseInt(e.target.value || "1", 10)))}
              style={{ width: 80, font: "inherit" }}
            />
            <select
              value={customUnit}
              onChange={(e) => setCustomUnit(e.target.value as "minutes" | "hours" | "days")}
              style={{ font: "inherit" }}
            >
              <option value="minutes">minutes</option>
              <option value="hours">hours</option>
              <option value="days">days</option>
            </select>
          </div>
        )}

        <label style={{ fontSize: 11, display: "block", marginTop: 8 }}>
          <input
            type="checkbox"
            checked={useCustom}
            onChange={(e) => setUseCustom(e.target.checked)}
            style={{ marginRight: 6 }}
          />
          Custom duration (max 28 days)
        </label>

        <label style={{ fontSize: 11, display: "block", marginTop: 12, marginBottom: 4 }}>
          Reason (optional)
        </label>
        <textarea
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          maxLength={200}
          rows={2}
          style={{ width: "100%", font: "inherit", boxSizing: "border-box" }}
        />
        <div style={{ fontSize: 9, color: "var(--xp-text-muted, #888)", textAlign: "right" }}>
          {reason.length}/200
        </div>

        <p style={{ fontSize: 10, color: "var(--xp-text-muted, #666)", marginTop: 4 }}>
          Until {untilDate.toLocaleString()}
        </p>

        <div style={{ display: "flex", justifyContent: "flex-end", gap: 6, marginTop: 12 }}>
          <button onClick={onCancel} style={{ font: "inherit", padding: "4px 12px" }}>
            Cancel
          </button>
          <button
            onClick={() => onConfirm(untilMs, reason.trim() || null)}
            style={{
              font: "inherit",
              padding: "4px 12px",
              background: "#a60",
              color: "#fff",
              border: "1px solid #840",
            }}
          >
            Time out
          </button>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/TimeoutDialog.tsx
git -C /home/deez/farder commit -m "feat(client): TimeoutDialog (presets + custom + reason)"
```

---

## Task 14: TimeoutBanner + wire into MessageInput

**Files:**
- Create: `client/src/components/TimeoutBanner.tsx`
- Modify: `client/src/components/MessageInput.tsx`

- [ ] **Step 1: Create TimeoutBanner.tsx**

```tsx
import { useEffect, useState, type CSSProperties } from "react";

interface Props {
  untilMs: number;
  reason: string | null;
}

const banner: CSSProperties = {
  background: "#fff8e1",
  color: "#a60",
  border: "1px solid #e0c060",
  padding: "4px 8px",
  fontSize: 11,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
};

function formatRemaining(ms: number): string {
  if (ms <= 0) return "0s";
  const totalSec = Math.ceil(ms / 1000);
  const days = Math.floor(totalSec / 86400);
  const hours = Math.floor((totalSec % 86400) / 3600);
  const mins = Math.floor((totalSec % 3600) / 60);
  const secs = totalSec % 60;
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${mins}m`;
  if (mins > 0) return `${mins}m ${secs}s`;
  return `${secs}s`;
}

export default function TimeoutBanner({ untilMs, reason }: Props) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);

  const remaining = untilMs - now;
  if (remaining <= 0) return null;

  return (
    <div style={banner}>
      <strong>You're timed out for {formatRemaining(remaining)}.</strong>
      {reason && <> Reason: {reason}</>}
    </div>
  );
}
```

- [ ] **Step 2: Wire into MessageInput**

In `client/src/components/MessageInput.tsx`:

Add import:
```tsx
import TimeoutBanner from "./TimeoutBanner";
```

Read the component to find where `me` (active server's own member info) is available. Search:
```
grep -n "activeServer\.me\|members.find\|own_member\|publicKey" /home/deez/farder/client/src/components/MessageInput.tsx
```

If `activeServer.me` exists, use it directly. Otherwise resolve via:
```tsx
const ownPk = useOwnPublicKey();  // or however it's exposed
const me = activeServer?.members.find(m => publicKeyToString(m.public_key) === ownPk);
```

Inside the JSX, just before the existing `<div className="message-input-row">`, add:
```tsx
{me?.timeout_until != null && me.timeout_until > Date.now() && (
  <TimeoutBanner untilMs={me.timeout_until} reason={me.timeout_reason ?? null} />
)}
```

Also, when an active timeout exists, disable the textarea and Send button. Find the existing `<textarea ... disabled={sending}>` and change to `disabled={sending || isTimedOut}` where `const isTimedOut = me?.timeout_until != null && me.timeout_until > Date.now();`. Same for the Send button. (Note: this check re-evaluates on every render — that's fine since the TimeoutBanner causes parent re-renders via setNow each second.)

To make MessageInput re-render every second while timed out, lift the `now` ticker into MessageInput too, or have MessageInput observe a context that updates. Simplest: add the same `useEffect`+`setNow` ticker into MessageInput, gated on timed-out state. Add at top of MessageInput:

```tsx
const me = activeServer?.members.find(m => publicKeyToString(m.public_key) === ownPk);
const timeoutActive = me?.timeout_until != null && me.timeout_until > Date.now();
const [, setNowTick] = useState(0);
useEffect(() => {
  if (!timeoutActive) return;
  const t = setInterval(() => setNowTick(n => n + 1), 1000);
  return () => clearInterval(t);
}, [timeoutActive]);
```

(`me` and `timeoutActive` are recomputed every render; the ticker just nudges the render every second.)

- [ ] **Step 3: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/components/TimeoutBanner.tsx client/src/components/MessageInput.tsx
git -C /home/deez/farder commit -m "feat(client): TimeoutBanner + disable input when timed out"
```

---

## Task 15: MemberContextMenu — timeout/untimeout rows

**Files:**
- Modify: `client/src/components/MemberContextMenu.tsx`

- [ ] **Step 1: Add the rows**

Read the existing file to understand its structure:
```
grep -n "rows.push\|canKick\|canBan\|PERMISSIONS\." /home/deez/farder/client/src/components/MemberContextMenu.tsx | head -20
```

Find where the Kick row is pushed. After it (between Kick and Ban), insert:

```tsx
const canTimeout = (myPerms & PERMISSIONS.TIMEOUT_MEMBERS) === PERMISSIONS.TIMEOUT_MEMBERS;
const isTimedOut = target.timeout_until != null && target.timeout_until > Date.now();
if (canTimeout && !isSelf) {
  if (isTimedOut) {
    rows.push({
      kind: "item",
      label: "Remove timeout",
      onClick: async () => {
        try { await api.removeTimeout(serverId, publicKeyToString(target.public_key)); }
        catch (e) { setError(String(e)); }
        onClose();
      },
    });
  } else {
    rows.push({
      kind: "item",
      label: "Timeout…",
      onClick: () => {
        onClose();
        openTimeoutDialog(target);
      },
    });
  }
}
```

`openTimeoutDialog` should be a prop the parent supplies (MemberSidebar / Message). Simplest path: pass it as an `onTimeout?: (target: MemberInfo) => void` prop. The parent renders TimeoutDialog with state and calls `api.timeoutMember(...)` on confirm:

```tsx
// In MemberSidebar.tsx (and Message.tsx where MemberContextMenu is rendered)
const [timeoutTarget, setTimeoutTarget] = useState<MemberInfo | null>(null);

<MemberContextMenu
  ...
  onTimeout={setTimeoutTarget}
/>

{timeoutTarget && (
  <TimeoutDialog
    targetName={timeoutTarget.display_name}
    onCancel={() => setTimeoutTarget(null)}
    onConfirm={async (untilMs, reason) => {
      try { await api.timeoutMember(serverId, publicKeyToString(timeoutTarget.public_key), untilMs, reason); }
      catch (e) { setError(String(e)); }
      setTimeoutTarget(null);
    }}
  />
)}
```

Add the same wiring to both `MemberSidebar.tsx` and `Message.tsx` (the two surfaces using MemberContextMenu).

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/MemberContextMenu.tsx client/src/components/MemberSidebar.tsx client/src/components/Message.tsx
git -C /home/deez/farder commit -m "feat(client): timeout/untimeout rows in MemberContextMenu"
```

---

## Task 16: useServerEvents — handle three new events

**Files:**
- Modify: `client/src/hooks/useServerEvents.ts`

- [ ] **Step 1: Add listeners for three new events**

Find `listen("server:member_banned", ...)` and `listen("server:member_unbanned", ...)` (around lines 213, 218). After them, add:

```ts
    listen("server:member_timeout_changed", (e) => {
      const payload = e.payload as { server_id: string; public_key: string; until_ms: number | null; reason: string | null };
      dispatch({
        type: "MEMBER_TIMEOUT_CHANGED",
        serverId: payload.server_id,
        publicKey: payload.public_key,
        untilMs: payload.until_ms,
        reason: payload.reason,
      });
    }).then((u) => safePush(u));

    listen("server:you_were_kicked", (e) => {
      const payload = e.payload as { server_id: string };
      dispatch({ type: "YOU_WERE_KICKED", serverId: payload.server_id });
    }).then((u) => safePush(u));

    listen("server:you_were_banned", (e) => {
      const payload = e.payload as { server_id: string; reason: string | null };
      dispatch({ type: "YOU_WERE_BANNED", serverId: payload.server_id, reason: payload.reason });
    }).then((u) => safePush(u));

    listen("server:audit_event_created", (e) => {
      const payload = e.payload as { server_id: string; event: AuditEvent };
      // Cross-component pubsub — AuditLogTab listens for this directly.
      window.dispatchEvent(new CustomEvent("farder:audit-event-created", { detail: payload }));
    }).then((u) => safePush(u));
```

(Add `import type { AuditEvent } from "../lib/tauri-bridge";` at top.)

- [ ] **Step 2: Add reducer cases**

Find the reducer file (search for `MEMBER_BANNED` or similar):
```
grep -rn "MEMBER_BANNED\|case \"MEMBER\"" /home/deez/farder/client/src/
```

In the same reducer, add three new action types and cases:

```ts
case "MEMBER_TIMEOUT_CHANGED": {
  const server = state.servers[action.serverId];
  if (!server) return state;
  const members = server.members.map((m) =>
    publicKeyToString(m.public_key) === action.publicKey
      ? { ...m, timeout_until: action.untilMs, timeout_reason: action.reason }
      : m
  );
  return { ...state, servers: { ...state.servers, [action.serverId]: { ...server, members } } };
}

case "YOU_WERE_KICKED": {
  return { ...state, kickedBanned: { kind: "kick", serverId: action.serverId, reason: null } };
}

case "YOU_WERE_BANNED": {
  return { ...state, kickedBanned: { kind: "ban", serverId: action.serverId, reason: action.reason } };
}
```

Add the corresponding action types to the reducer's action union and the `kickedBanned` field to the state type:

```ts
kickedBanned: { kind: "kick" | "ban"; serverId: string; reason: string | null } | null;
```

Initialize `kickedBanned: null` in the initial state.

- [ ] **Step 3: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/hooks/useServerEvents.ts client/src/context/ServerContext.tsx
git -C /home/deez/farder commit -m "feat(client): handle MemberTimeoutChanged + YouWereKicked/Banned + AuditEventCreated"
```

(Adjust paths if the reducer lives elsewhere — search `grep -rn "function reducer\|useReducer" /home/deez/farder/client/src/` to locate it.)

---

## Task 17: KickedBannedDialog + wire into AppShell

**Files:**
- Create: `client/src/components/KickedBannedDialog.tsx`
- Modify: `client/src/components/AppShell.tsx`

- [ ] **Step 1: Create the dialog**

```tsx
import { type CSSProperties } from "react";

interface Props {
  kind: "kick" | "ban";
  serverName: string;
  reason: string | null;
  onClose: () => void;
}

const overlay: CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(0,0,0,0.55)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 3000,
};

const card: CSSProperties = {
  background: "var(--xp-window-bg, #ECE9D8)",
  color: "var(--xp-text-normal, #000)",
  border: "2px solid var(--xp-blue-dark, #003C74)",
  padding: 24,
  width: 380,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
  textAlign: "center",
};

export default function KickedBannedDialog({ kind, serverName, reason, onClose }: Props) {
  const verb = kind === "kick" ? "kicked from" : "banned from";
  return (
    <div style={overlay}>
      <div style={card}>
        <h3 style={{ marginTop: 0 }}>You were {verb} {serverName}</h3>
        {reason && (
          <p style={{ fontSize: 12, color: "var(--xp-text-muted, #666)" }}>
            Reason: {reason}
          </p>
        )}
        <button
          onClick={onClose}
          style={{
            font: "inherit",
            padding: "6px 20px",
            marginTop: 12,
            background: "var(--xp-blue, #0058E6)",
            color: "#fff",
            border: "1px solid var(--xp-blue-dark, #003C74)",
          }}
        >
          OK
        </button>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Render in AppShell**

In `client/src/components/AppShell.tsx`, read the existing top-level layout and add somewhere near other modals:

```tsx
import KickedBannedDialog from "./KickedBannedDialog";
// ... inside component
const { kickedBanned, dispatch, servers } = useApp();
// ...
{kickedBanned && (
  <KickedBannedDialog
    kind={kickedBanned.kind}
    serverName={servers[kickedBanned.serverId]?.serverInfo?.name ?? "the server"}
    reason={kickedBanned.reason}
    onClose={() => {
      dispatch({ type: "CLEAR_KICKED_BANNED" });
      // Also disconnect / route back to picker — leverage the existing disconnect path.
      // If a `disconnect` action is wired via tauri-bridge, call it here:
      // void api.disconnectServer(kickedBanned.serverId).catch(() => {});
    }}
  />
)}
```

Add a `CLEAR_KICKED_BANNED` reducer case that sets `kickedBanned: null`.

(If the existing app layout already routes to a server picker on connection-loss, just wiring `kickedBanned` here suffices. Verify by smoke-testing in Task 20.)

- [ ] **Step 3: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/components/KickedBannedDialog.tsx client/src/components/AppShell.tsx client/src/context/ServerContext.tsx
git -C /home/deez/farder commit -m "feat(client): KickedBannedDialog wired into AppShell"
```

---

## Task 18: AuditLogTab component

**Files:**
- Create: `client/src/components/AuditLogTab.tsx`

- [ ] **Step 1: Create the component**

```tsx
import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import type { AuditEvent } from "../lib/tauri-bridge";
import { useActiveServer } from "../context/ServerContext";
import { publicKeyToString } from "../lib/types";

interface Props {
  serverId: string;
}

const ACTION_VERBS: Record<string, (target: string | null, meta: Record<string, unknown>) => string> = {
  kick: (t) => `kicked ${t ?? ""}`,
  ban: (t) => `banned ${t ?? ""}`,
  unban: (t) => `unbanned ${t ?? ""}`,
  timeout: (t, m) => {
    const u = m["until_ms"] as number | undefined;
    return `timed out ${t ?? ""}${u ? ` until ${new Date(u).toLocaleString()}` : ""}`;
  },
  untimeout: (t) => `removed timeout from ${t ?? ""}`,
  role_assigned: (t, m) => `assigned role "${m["role_name"] ?? "?"}" to ${t ?? ""}`,
  role_removed: (t, m) => `removed role "${m["role_name"] ?? "?"}" from ${t ?? ""}`,
  channel_created: (_t, m) => `created channel #${m["channel_name"] ?? "?"}`,
  channel_deleted: (_t, m) => `deleted channel #${m["channel_name"] ?? "?"}`,
  channel_renamed: (_t, m) => `renamed channel #${m["old_name"] ?? "?"} → #${m["new_name"] ?? "?"}`,
  role_created: (_t, m) => `created role "${m["role_name"] ?? "?"}"`,
  role_deleted: (_t, m) => `deleted role "${m["role_name"] ?? "?"}"`,
  role_perms_changed: (_t, m) => `changed permissions for role id ${m["role_id"] ?? "?"}`,
  channel_overrides_changed: (_t, m) => `changed channel-overrides on channel ${m["channel_id"] ?? "?"} for role ${m["role_id"] ?? "?"}`,
};

function relativeTime(ms: number): string {
  const diff = Date.now() - ms;
  if (diff < 60_000) return "just now";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
  return `${Math.floor(diff / 86_400_000)}d ago`;
}

const row: CSSProperties = {
  padding: "8px 4px",
  borderBottom: "1px solid var(--xp-border, #ddd)",
  fontSize: 11,
  cursor: "pointer",
};

const detail: CSSProperties = {
  background: "var(--xp-panel-bg, #fafafa)",
  padding: 8,
  marginTop: 4,
  fontSize: 10,
  fontFamily: "monospace",
  whiteSpace: "pre-wrap",
};

export default function AuditLogTab({ serverId }: Props) {
  const activeServer = useActiveServer();
  const [events, setEvents] = useState<AuditEvent[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [hasMore, setHasMore] = useState(true);
  const [expandedId, setExpandedId] = useState<number | null>(null);
  const seenIds = useRef<Set<number>>(new Set());

  const memberByPk = useMemo(() => {
    const map = new Map<string, string>();
    activeServer?.members.forEach((m) => map.set(publicKeyToString(m.public_key), m.display_name));
    return map;
  }, [activeServer?.members]);

  function nameFor(pk: string | null): string | null {
    if (!pk) return null;
    return memberByPk.get(pk) ?? pk.slice(0, 8) + "…";
  }

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    api.listAuditEvents(serverId, null, 50)
      .then((evts) => {
        if (cancelled) return;
        evts.forEach((e) => seenIds.current.add(e.id));
        setEvents(evts);
        setHasMore(evts.length === 50);
      })
      .catch((e) => { if (!cancelled) setError(String(e)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [serverId]);

  // Live updates
  useEffect(() => {
    function onAudit(e: Event) {
      const detail = (e as CustomEvent).detail as { server_id: string; event: AuditEvent };
      if (detail.server_id !== serverId) return;
      if (seenIds.current.has(detail.event.id)) return;
      seenIds.current.add(detail.event.id);
      setEvents((prev) => [detail.event, ...prev]);
    }
    window.addEventListener("farder:audit-event-created", onAudit);
    return () => window.removeEventListener("farder:audit-event-created", onAudit);
  }, [serverId]);

  async function loadOlder() {
    if (!hasMore || events.length === 0) return;
    const oldest = events[events.length - 1].id;
    try {
      const more = await api.listAuditEvents(serverId, oldest, 50);
      more.forEach((e) => seenIds.current.add(e.id));
      setEvents((prev) => [...prev, ...more]);
      setHasMore(more.length === 50);
    } catch (e) {
      setError(String(e));
    }
  }

  if (loading) return <div style={{ padding: 16 }}>Loading audit log…</div>;
  if (error) return <div style={{ padding: 16, color: "#a00" }}>{error}</div>;
  if (events.length === 0) {
    return <div style={{ padding: 16, color: "var(--xp-text-muted, #666)" }}>No moderation actions recorded yet.</div>;
  }

  return (
    <div style={{ padding: 8 }}>
      {events.map((evt) => {
        const actorName = nameFor(publicKeyToString(evt.actor)) ?? "?";
        const targetName = evt.target ? nameFor(publicKeyToString(evt.target)) : null;
        const verb = ACTION_VERBS[evt.action]
          ? ACTION_VERBS[evt.action](targetName, evt.metadata)
          : `did "${evt.action}"`;
        const expanded = expandedId === evt.id;
        return (
          <div
            key={evt.id}
            style={row}
            onClick={() => setExpandedId(expanded ? null : evt.id)}
          >
            <div>
              <strong>{actorName}</strong> {verb}{" "}
              <span style={{ color: "var(--xp-text-muted, #666)" }}>· {relativeTime(evt.timestamp_ms)}</span>
            </div>
            {expanded && <pre style={detail}>{JSON.stringify(evt.metadata, null, 2)}</pre>}
          </div>
        );
      })}
      {hasMore && (
        <button onClick={loadOlder} style={{ marginTop: 8, font: "inherit", padding: "4px 12px" }}>
          Load older
        </button>
      )}
    </div>
  );
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/AuditLogTab.tsx
git -C /home/deez/farder commit -m "feat(client): AuditLogTab — paginated list + live updates"
```

---

## Task 19: ServerSettingsDialog — Audit Log tab

**Files:**
- Modify: `client/src/components/ServerSettingsDialog.tsx`

- [ ] **Step 1: Add the new tab**

In `ServerSettingsDialog.tsx`, find the existing tab state (line ~24):

```tsx
const [activeTab, setActiveTab] = useState<"general" | "banned">("general");
```

Change to:

```tsx
const [activeTab, setActiveTab] = useState<"general" | "banned" | "audit">("general");
```

Find the existing tab buttons (line ~169-185). After the `banned` tab button, add:

```tsx
{canManageServer && (
  <button
    className={`tab-btn${activeTab === "audit" ? " tab-btn--active" : ""}`}
    onClick={() => setActiveTab("audit")}
  >
    Audit Log
  </button>
)}
```

Where `canManageServer` is computed from the actor's resolved permissions. If a similar `canBan` check already exists for the Banned tab, mirror its computation:

```tsx
const canManageServer = (myPerms & PERMISSIONS.MANAGE_SERVER) === PERMISSIONS.MANAGE_SERVER;
```

After the existing `{activeTab === "banned" && ...}` block, add:

```tsx
{activeTab === "audit" && serverId && <AuditLogTab serverId={serverId} />}
```

Add the import at the top:
```tsx
import AuditLogTab from "./AuditLogTab";
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/ServerSettingsDialog.tsx
git -C /home/deez/farder commit -m "feat(client): Audit Log tab in ServerSettingsDialog"
```

---

## Task 20: Smoke test + CHANGELOG

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Restart and smoke-test**

```
pkill -f farder-server
cd /home/deez/farder/client && npm run tauri dev
```

Walk this checklist with two clients (Alice = owner / mod, Bob = target):

- [ ] Right-click Bob → menu shows "Timeout…" between Kick and Ban (with TIMEOUT_MEMBERS perm).
- [ ] Click Timeout → dialog appears with 5 minutes pre-selected, reason field.
- [ ] Pick "60 seconds", click Time out → no error.
- [ ] Bob's MessageInput shows yellow banner "You're timed out for 59s." Send button disabled.
- [ ] Bob tries to send anyway via fake API call (or just waits) → server rejects with `timed out until <ms>`.
- [ ] Banner counts down each second.
- [ ] Wait 60s → banner disappears, send re-enables locally; Bob can send.
- [ ] Right-click Bob (still timed-out) → menu shows "Remove timeout" instead of "Timeout…".
- [ ] Click → timeout cleared, banner gone immediately on Bob's client.
- [ ] Ban Bob with reason "test ban" → Bob immediately sees "You were banned from <server> · Reason: test ban" dialog (not "Connection lost").
- [ ] Re-add Bob (unban + re-invite). Kick Bob → Bob sees "You were kicked from <server>" dialog.
- [ ] Open Server Settings → tab bar shows General / Banned / Audit Log (with MANAGE_SERVER).
- [ ] Click Audit Log → see all moderator actions newest-first: ban, kick, timeout, untimeout. Click a row → metadata JSON expands.
- [ ] Perform another action (ban+unban a third user) → Audit Log updates live without refresh.
- [ ] Custom-duration: try 31 days → server rejects "out of range".
- [ ] Custom-duration: try 0 days → button disabled or server rejects.
- [ ] Non-mod user opens Server Settings → no Audit Log tab visible.
- [ ] Non-mod user manually invokes `list_audit_events` (via console) → server returns "missing MANAGE_SERVER".

If any item fails, file a follow-up task — don't fix in this commit.

- [ ] **Step 2: Add CHANGELOG entry**

In `CHANGELOG.md`, under `### Added`:

```
- (2026-05-05) Member Moderation Phase 2: timeout (server-enforced silence), audit log, kicked/banned dignity. Right-click any member with the new TIMEOUT_MEMBERS permission for "Timeout…" — preset durations (60s/5m/10m/1h/1d/1w) plus a custom duration up to 28 days, optional reason. Timed-out users see a yellow banner above the message input with a live countdown; their textarea + send button disable until the timeout expires. Server blocks send-message, add-reaction, and join-voice while timed out (DMs unaffected — timeout is server-scoped). New audit_events table tracks 14 mutating action types (kick, ban, unban, timeout, role/channel CRUD) with forever retention; viewable via a new Audit Log tab in Server Settings (gated by MANAGE_SERVER) with paginated newest-first list, click-to-expand metadata JSON, and live updates via a new AuditEventCreated broadcast (filtered server-side to MANAGE_SERVER holders). Kicked and banned users now receive a YouWereKicked / YouWereBanned event before tear-down so they see a clean "You were kicked/banned from <server>" dialog with the reason instead of the generic "Connection lost" flash.
```

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add CHANGELOG.md
git -C /home/deez/farder commit -m "docs: changelog for Member Moderation Phase 2"
```

---

## Self-review notes

**Spec coverage:**
- TIMEOUT_MEMBERS permission bit → Task 3
- members.timeout_until + timeout_reason columns → Task 2 (schema), Task 3 (helpers + MemberInfo wiring)
- audit_events table + indexes → Task 2
- TimeoutMember / RemoveTimeout / ListAuditEvents protocol additions → Task 1
- AuditEvent struct + MemberInfo additions → Task 1
- 4 new ServerEvent variants (MemberTimeoutChanged, YouWereKicked, YouWereBanned, AuditEventCreated) → Task 1
- New EventTarget::Members + EventTarget::PermissionHolders → Tasks 1, 6
- Timeout enforcement at 4 handler tops → Task 4
- TimeoutMember / RemoveTimeout handlers (perm + hierarchy + range) → Task 5
- audit::insert + audit::list helpers → Task 6
- 14 audit::emit call sites → Task 7
- ListAuditEvents handler → Task 8
- YouWereKicked / YouWereBanned emission → Task 9
- 3 Tauri commands → Task 10
- 4 bridge.rs event emissions → Task 11
- TIMEOUT_MEMBERS in TS, AuditEvent type, MemberInfo TS additions → Task 12
- TimeoutDialog (presets + custom + reason + 28d cap) → Task 13
- TimeoutBanner + MessageInput integration → Task 14
- MemberContextMenu rows → Task 15
- useServerEvents handlers + reducer cases → Task 16
- KickedBannedDialog + AppShell wiring → Task 17
- AuditLogTab (paginated + live updates + expandable detail) → Task 18
- ServerSettingsDialog Audit Log tab → Task 19

**Type/name consistency:**
- `TIMEOUT_MEMBERS = 1 << 14` (Rust) ↔ `TIMEOUT_MEMBERS: 1n << 14n` (TS) — match.
- `TimeoutMember`, `RemoveTimeout`, `ListAuditEvents` — same names everywhere (Rust enum variants → Tauri command snake_case → TS function camelCase).
- `MemberTimeoutChanged`, `YouWereKicked`, `YouWereBanned`, `AuditEventCreated` — same Rust enum variants emitted as `server:member_timeout_changed`, `server:you_were_kicked`, etc.
- `AuditEvent { id, actor, target, action, metadata, timestamp_ms }` — same shape Rust ↔ TS.
- `audit::insert(conn, actor, target, action, metadata)` — same signature in tests and call sites.
- `audit::list(conn, before_id, limit)` — same signature.
- `set_timeout(conn, pk, until_ms, reason)`, `clear_timeout(conn, pk)`, `is_timed_out(conn, pk, now_ms)` — same throughout.

**Known compromises (and rationales):**
- The `TimedOut` "error" rides on the existing `ServerResponse::Error { reason }` rather than getting its own variant. Keeping the response enum stable is more valuable than typed parsing; the client primarily learns about timeout via `MemberTimeoutChanged`, not error parsing.
- `EventTarget::PermissionHolders` resolution requires holding both the clients lock and the DB lock briefly. Acceptable for low-frequency audit broadcasts; revisit if perf becomes an issue.
- The `audit_emit` helper logs but doesn't propagate failures. The parent action has already succeeded; failing it for an audit insert is worse UX than a missing log row. (Spec mandates this.)
- Edit-nickname blocking depends on whether a `SetDisplayName` request actually exists in the codebase. If not, that single enforcement point is skipped — documented in Task 4 Step 4.
- `UpdateChannel` only emits an audit event for renames, not for topic/slow_mode/etc edits — matches the spec's "structural changes" scope but explicit in code: `if name.is_some() && name != prev_name { audit_emit(...) }`.
- `UpdateRole` only emits for permissions changes, not name-only renames.
- No automated client tests (consistent with codebase). Smoke list in Task 20 is the validation.
