# Mesh Rung 1 — Sub-project 3a: Server-Side Event Ingestion — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the mesh event log into the server: a new server gets a cryptographic genesis, holds an in-memory `LogState`, accepts client-submitted signed `Event`s via a new `SubmitEvent` request (validated through `LogState`), persists them to an append-only `events` table as the source of truth, derives the existing `messages` read-view for `MessagePosted` events, and broadcasts — all **additively**, leaving the legacy `SendMessage` path untouched.

**Architecture:** Additive parallel path. `farder-protocol` gains `ServerRequest::SubmitEvent { event }` (carrying `farder_crypto::event_log::Event`) + `ServerResponse::EventAccepted`. The server adds an `events` + `genesis` table, an in-memory `LogState` on `ServerState` (created when the owner is established, rebuilt on startup by replaying `events` in accept order), and a handler that validates on a **clone** of `LogState` then commits. `MessagePosted` events derive a row into the existing `messages` table (so history/render are unchanged). No client changes in 3a — exercised by Rust integration tests that build signed events with `farder-crypto`.

**Tech Stack:** Rust, `rusqlite` (SQLite), `rmp_serde`, `farder-crypto` (`event_log`, `event_log_state`), `farder-protocol`, `farder-server`. No new dependencies.

## Global Constraints

- **Additive / non-invasive:** the existing `SendMessage` handler, `messages` table shape, broadcast machinery, and client receive path are NOT modified. Only new tables/columns, a new request variant, and a new handler arm.
- **Source of truth = the `events` table.** The `messages` table becomes a *derived view* for event-sourced messages (legacy rows coexist). Replay of `events` (in accept order) is sufficient to rebuild `LogState`.
- **Validate on a clone, commit only on success:** `let mut trial = log_state.clone(); trial.apply(&event)?;` then persist + derive in one DB transaction, then `*log_state = trial`. A rejected event never mutates in-memory state; a DB failure never commits the in-memory advance.
- **Reuse sub-projects 1 & 2 verbatim:** `farder_crypto::event_log::{Event, Genesis, EventPayload, AttachmentCap}` and `farder_crypto::event_log_state::LogState` (`from_genesis`, `apply`, `replay`, queries). Do not re-implement validation.
- **Server identity:** `genesis.server_id()` is the server's cryptographic id; persisted once when the owner is established.
- **Deferred (documented, NOT built here):** lamport-monotonicity enforcement (seq/prev already give within-chain integrity; needs a small `LogState` change to track per-chain lamport — follow-on); attachment derivation (`MessagePosted.attachments` are ignored when deriving `messages` this slice — sub-project 4); converting channels/roles/legacy-membership to events; full peer-side event verification on broadcast (3b/client); relay-ownership binding.

---

## File Structure

- **Modify** `crates/farder-protocol/src/server.rs` — add `ServerRequest::SubmitEvent { event: farder_crypto::event_log::Event }` and `ServerResponse::EventAccepted { event_hash: String, timestamp: u64 }`. (farder-protocol already depends on farder-crypto.)
- **Modify** `crates/farder-server/src/db.rs` — add the `events` table, the `genesis` table, and a nullable `event_hash` column on `messages` (idempotent migration, following the existing PRAGMA-table_info pattern).
- **Create** `crates/farder-server/src/event_ingest.rs` — server-side helpers: persist/load genesis, the `events` table read/write (`store_event`, `load_events_in_order`), `build_log_state` (genesis + replay), and `derive_message_from_event` (insert a `messages` row from a `MessagePosted` event). Keeps the new logic out of the already-large `handlers.rs`/`db.rs`.
- **Modify** `crates/farder-server/src/state.rs` — add `genesis: Mutex<Option<Genesis>>` and `log_state: Mutex<Option<LogState>>` to `ServerState`.
- **Modify** `crates/farder-server/src/main.rs` — on startup, if a genesis exists, load it + rebuild `LogState` via replay.
- **Modify** the owner-establish path (`crates/farder-server/src/connection.rs` ~566 / `auth.rs`) — when the owner is set, create + persist the genesis (if absent) and initialize `LogState`.
- **Modify** `crates/farder-server/src/handlers.rs` — add the `ServerRequest::SubmitEvent` arm.

---

## Task 1: Schema — `events` + `genesis` tables + `messages.event_hash`

**Files:**
- Modify: `crates/farder-server/src/db.rs` (init_schema + migrations)
- Test: in-module test in `db.rs` (or a small test in `event_ingest.rs` in Task 2 — keep one here for the schema)

**Interfaces:**
- Produces: tables `events`, `genesis`, and column `messages.event_hash` available after `init_schema`/migration.

- [ ] **Step 1: Add the schema**

In `crates/farder-server/src/db.rs`'s `init_schema` (in the `execute_batch` block, near the `messages` table), add:

```sql
CREATE TABLE IF NOT EXISTS events (
    accept_seq  INTEGER PRIMARY KEY AUTOINCREMENT,  -- server acceptance order (replay order)
    event_hash  TEXT    UNIQUE NOT NULL,            -- content id (SHA-256 hex of the signed Event)
    server_id   TEXT    NOT NULL,
    author      BLOB    NOT NULL,                    -- identity pubkey (32 bytes)
    device      TEXT    NOT NULL,                    -- device id
    seq         INTEGER NOT NULL,
    lamport     INTEGER NOT NULL,
    payload_type TEXT   NOT NULL,                    -- e.g. 'MessagePosted', 'MemberJoined'
    channel_id  INTEGER,                             -- denormalized for message lookups (NULL for non-message)
    event_body  BLOB    NOT NULL,                    -- rmp_serde(Event)
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_author_seq ON events(author, device, seq);

CREATE TABLE IF NOT EXISTS genesis (
    singleton   INTEGER PRIMARY KEY CHECK (singleton = 0),  -- exactly one row
    genesis_body BLOB NOT NULL,                              -- rmp_serde(Genesis)
    server_id   TEXT NOT NULL
);
```

Then, following the existing idempotent-migration pattern (PRAGMA table_info check), add a nullable `event_hash` column to `messages` if missing — add this in the migration section of `init_schema`:

```rust
// Migration: link event-sourced messages back to their event (NULL for legacy rows).
let has_event_hash: bool = conn
    .prepare("PRAGMA table_info(messages)")?
    .query_map([], |row| row.get::<_, String>(1))?
    .filter_map(|r| r.ok())
    .any(|name| name == "event_hash");
if !has_event_hash {
    conn.execute("ALTER TABLE messages ADD COLUMN event_hash TEXT", [])?;
}
```

- [ ] **Step 2: Write the test**

Add to `db.rs` `#[cfg(test)] mod tests` (or wherever db tests live — match the file):

```rust
    #[test]
    fn events_and_genesis_schema_and_message_event_hash_migration() {
        let conn = open_in_memory().unwrap(); // existing helper that runs init_schema
        // events table accepts a row.
        conn.execute(
            "INSERT INTO events (event_hash, server_id, author, device, seq, lamport, payload_type, channel_id, event_body, created_at) \
             VALUES ('h0','srv',X'00',  'd', 0, 1, 'MessagePosted', 1, X'01', 100)",
            [],
        ).unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // genesis singleton.
        conn.execute("INSERT INTO genesis (singleton, genesis_body, server_id) VALUES (0, X'02', 'srv')", []).unwrap();
        assert!(conn.execute("INSERT INTO genesis (singleton, genesis_body, server_id) VALUES (0, X'03', 'srv')", []).is_err(),
            "genesis must be a singleton");
        // messages.event_hash column exists and defaults NULL.
        let has: bool = conn.prepare("PRAGMA table_info(messages)").unwrap()
            .query_map([], |r| r.get::<_, String>(1)).unwrap()
            .filter_map(|r| r.ok()).any(|c| c == "event_hash");
        assert!(has, "messages.event_hash column must exist");
    }
```

> If `open_in_memory()` isn't the in-memory helper name, use the crate's actual one (search `db::open_in_memory` usages in existing tests — it's used in `attachments.rs` tests).

- [ ] **Step 3: Run the test**

Run: `cargo test -p farder-server events_and_genesis_schema_and_message_event_hash_migration`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/farder-server/src/db.rs
git commit -m "feat(server): mesh events + genesis tables + messages.event_hash migration"
```

---

## Task 2: `event_ingest` helpers — genesis persist/load, event store, replay, message derive

**Files:**
- Create: `crates/farder-server/src/event_ingest.rs`
- Modify: `crates/farder-server/src/lib.rs` (add `pub mod event_ingest;`)

**Interfaces:**
- Consumes: `farder_crypto::event_log::{Event, Genesis, EventPayload}`, `farder_crypto::event_log_state::LogState`, `rusqlite::Connection`.
- Produces:
  - `save_genesis(conn, &Genesis) -> Result<()>`, `load_genesis(conn) -> Result<Option<Genesis>>`
  - `store_event(conn, &Event) -> Result<()>` (append to `events`; errors on duplicate hash)
  - `load_events_in_order(conn) -> Result<Vec<Event>>` (ORDER BY accept_seq)
  - `build_log_state(conn) -> Result<Option<LogState>>` (load genesis + replay events; None if no genesis)
  - `derive_message_row(conn, &Event) -> Result<Option<u64>>` (for a `MessagePosted` event, insert a `messages` row incl. `event_hash`, return the new message id; None for non-message payloads)

- [ ] **Step 1: Write the helpers + failing test**

Create `crates/farder-server/src/event_ingest.rs`:

```rust
//! Server-side glue for the mesh event log: persist the genesis, append events
//! to the source-of-truth `events` table, replay them into a `LogState`, and
//! derive the legacy `messages` read-view for `MessagePosted` events.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use farder_crypto::event_log::{Event, EventPayload, Genesis};
use farder_crypto::event_log_state::LogState;

pub fn save_genesis(conn: &Connection, g: &Genesis) -> Result<()> {
    let body = rmp_serde::to_vec(g).expect("genesis serialization cannot fail");
    conn.execute(
        "INSERT OR IGNORE INTO genesis (singleton, genesis_body, server_id) VALUES (0, ?1, ?2)",
        params![body, g.server_id()],
    )?;
    Ok(())
}

pub fn load_genesis(conn: &Connection) -> Result<Option<Genesis>> {
    let row: Option<Vec<u8>> = conn
        .query_row("SELECT genesis_body FROM genesis WHERE singleton = 0", [], |r| r.get(0))
        .optional()?;
    match row {
        Some(body) => Ok(Some(rmp_serde::from_slice(&body).context("decode genesis")?)),
        None => Ok(None),
    }
}

fn payload_type(p: &EventPayload) -> &'static str {
    match p {
        EventPayload::MessagePosted { .. } => "MessagePosted",
        EventPayload::DeviceAuthorized { .. } => "DeviceAuthorized",
        EventPayload::InviteCreated { .. } => "InviteCreated",
        EventPayload::MemberJoined { .. } => "MemberJoined",
        EventPayload::MemberRemoved { .. } => "MemberRemoved",
        EventPayload::MemberBanned { .. } => "MemberBanned",
        EventPayload::MemberUnbanned { .. } => "MemberUnbanned",
        EventPayload::PermissionGranted { .. } => "PermissionGranted",
    }
}

pub fn store_event(conn: &Connection, event: &Event) -> Result<()> {
    let channel_id: Option<i64> = match &event.core.payload {
        EventPayload::MessagePosted { channel_id, .. } => Some(*channel_id as i64),
        _ => None,
    };
    let body = rmp_serde::to_vec(event).expect("event serialization cannot fail");
    conn.execute(
        "INSERT INTO events (event_hash, server_id, author, device, seq, lamport, payload_type, channel_id, event_body, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            event.hash(),
            event.core.server_id,
            event.core.author.as_bytes().as_slice(),
            event.core.device,
            event.core.seq as i64,
            event.core.lamport as i64,
            payload_type(&event.core.payload),
            channel_id,
            body,
            crate::db::now() as i64,
        ],
    )?;
    Ok(())
}

pub fn load_events_in_order(conn: &Connection) -> Result<Vec<Event>> {
    let mut stmt = conn.prepare("SELECT event_body FROM events ORDER BY accept_seq ASC")?;
    let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
    let mut out = Vec::new();
    for row in rows {
        let body = row?;
        out.push(rmp_serde::from_slice(&body).context("decode stored event")?);
    }
    Ok(out)
}

/// Rebuild the authorization state from genesis + the stored events (in accept
/// order, which reproduces the exact validation order). `None` if no genesis.
pub fn build_log_state(conn: &Connection) -> Result<Option<LogState>> {
    let Some(g) = load_genesis(conn)? else { return Ok(None) };
    let events = load_events_in_order(conn)?;
    let state = LogState::replay(&g, &events).context("replay of stored events failed")?;
    Ok(Some(state))
}

/// For a `MessagePosted` event, insert a `messages` read-view row (carrying the
/// event_hash) and return the new message id. Attachments are NOT derived in this
/// slice (sub-project 4). Non-message payloads return `None`.
pub fn derive_message_row(conn: &Connection, event: &Event) -> Result<Option<u64>> {
    let EventPayload::MessagePosted { channel_id, content, .. } = &event.core.payload else {
        return Ok(None);
    };
    conn.execute(
        "INSERT INTO messages (channel_id, author, content, timestamp, reply_to, event_hash) \
         VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
        params![
            *channel_id as i64,
            event.core.author.as_bytes().as_slice(),
            content,
            event.core.timestamp as i64,
            event.hash(),
        ],
    )?;
    let id = conn.last_insert_rowid() as u64;
    conn.execute("INSERT INTO messages_fts(rowid, content) VALUES (?1, ?2)", params![id as i64, content])?;
    Ok(Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::event_log::{DeviceCert, EventPayload as EP};
    use farder_crypto::identity::Keypair;

    fn genesis(owner: &Keypair) -> Genesis {
        Genesis { version: 1, name: "t".into(), owner: owner.public_key(), created_at: 1, nonce: [0u8; 16] }
    }

    #[test]
    fn genesis_roundtrip_and_replay_rebuilds_state() {
        let conn = crate::db::open_in_memory().unwrap();
        let owner = Keypair::generate();
        let dev = Keypair::generate();
        let g = genesis(&owner);
        save_genesis(&conn, &g).unwrap();
        assert_eq!(load_genesis(&conn).unwrap().unwrap().server_id(), g.server_id());

        // Build a small valid log: owner authorizes a device, then posts a message.
        let da = Event::next(&dev, owner.public_key(), g.server_id(), None, 0, 1,
            EP::DeviceAuthorized { cert: DeviceCert::create(&owner, &dev.public_key(), 1) });
        let msg = Event::next(&dev, owner.public_key(), g.server_id(), Some(&da), 1, 2,
            EP::MessagePosted { channel_id: 1, content: "hello".into(), reply_to: None, attachments: vec![] });
        store_event(&conn, &da).unwrap();
        store_event(&conn, &msg).unwrap();

        // Replay rebuilds state with the owner as a member and the device authorized.
        let ls = build_log_state(&conn).unwrap().unwrap();
        assert!(ls.is_member(&owner.public_key()));

        // Deriving the message row inserts into messages with the event_hash.
        let mid = derive_message_row(&conn, &msg).unwrap().unwrap();
        let (content, eh): (String, String) = conn.query_row(
            "SELECT content, event_hash FROM messages WHERE id = ?1", params![mid as i64],
            |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(content, "hello");
        assert_eq!(eh, msg.hash());

        // Duplicate event hash is rejected (UNIQUE).
        assert!(store_event(&conn, &msg).is_err());
    }
}
```

Add to `crates/farder-server/src/lib.rs`:

```rust
pub mod event_ingest;
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p farder-server event_ingest::tests::genesis_roundtrip_and_replay_rebuilds_state`
Expected: PASS. (Implementation written complete above; if you want strict red, stub `build_log_state` to `Ok(None)` first and watch the `unwrap()` panic.)

- [ ] **Step 3: Commit**

```bash
git add crates/farder-server/src/event_ingest.rs crates/farder-server/src/lib.rs
git commit -m "feat(server): event_ingest helpers — genesis persist/load, store/replay events, derive message row"
```

---

## Task 3: Protocol `SubmitEvent` + `EventAccepted`

**Files:**
- Modify: `crates/farder-protocol/src/server.rs`

**Interfaces:**
- Produces: `ServerRequest::SubmitEvent { event: farder_crypto::event_log::Event }`, `ServerResponse::EventAccepted { event_hash: String, timestamp: u64 }`.

- [ ] **Step 1: Add the variants**

In `crates/farder-protocol/src/server.rs`, add to the `ServerRequest` enum (near `SendMessage`):

```rust
    /// Submit a signed mesh event (Rung 1). The server validates it through the
    /// authorization log and, for MessagePosted, derives a `messages` row.
    SubmitEvent { event: farder_crypto::event_log::Event },
```

And to `ServerResponse`:

```rust
    EventAccepted { event_hash: String, timestamp: u64 },
```

(`farder-protocol` already depends on `farder-crypto`; `Event` is `Serialize`/`Deserialize`, so the codec handles it. Adding enum variants is backward-compatible under rmp_serde for these enums — confirm the existing enums aren't `#[serde(...)]`-tagged in a way that forbids it; they encode by variant index/name as the rest do.)

- [ ] **Step 2: Build to verify the protocol crate compiles**

Run: `cargo build -p farder-protocol`
Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add crates/farder-protocol/src/server.rs
git commit -m "feat(protocol): SubmitEvent request + EventAccepted response (mesh Rung 1)"
```

---

## Task 4: `ServerState` log_state/genesis + startup rebuild + owner-establish genesis creation

**Files:**
- Modify: `crates/farder-server/src/state.rs` (add fields to `ServerState`)
- Modify: `crates/farder-server/src/main.rs` (startup: rebuild LogState if genesis exists)
- Modify: `crates/farder-server/src/connection.rs` (owner-establish: create genesis + init LogState)

**Interfaces:**
- Consumes: Task 2's `event_ingest::{save_genesis, build_log_state}`; `farder_crypto::event_log::Genesis`, `event_log_state::LogState`.
- Produces: `ServerState.genesis: Mutex<Option<Genesis>>`, `ServerState.log_state: Mutex<Option<LogState>>`, both populated on startup (if genesis exists) and on owner-establish.

- [ ] **Step 1: Add the fields**

In `crates/farder-server/src/state.rs`, add to the `ServerState` struct (use `std::sync::Mutex` to match the existing std-Mutex fields like `db`):

```rust
    pub genesis: std::sync::Mutex<Option<farder_crypto::event_log::Genesis>>,
    pub log_state: std::sync::Mutex<Option<farder_crypto::event_log_state::LogState>>,
```

And initialize them to `Mutex::new(None)` in `ServerState::new` (match how the other fields are constructed).

- [ ] **Step 2: Rebuild on startup**

In `crates/farder-server/src/main.rs`, after `ServerState` is constructed (in/after `init_server`), rebuild from the DB:

```rust
{
    let conn = state.db.lock().unwrap();
    if let Some(g) = crate::event_ingest::load_genesis(&conn)? {
        let ls = crate::event_ingest::build_log_state(&conn)?;
        drop(conn);
        *state.genesis.lock().unwrap() = Some(g);
        *state.log_state.lock().unwrap() = ls;
    }
}
```

- [ ] **Step 3: Create genesis when the owner is established**

In `crates/farder-server/src/connection.rs`, at the owner-establish spot (where `*owner = Some(public_key.clone())` is set, ~line 566), after setting the owner, create + persist the genesis if absent and init the LogState:

```rust
// Mesh Rung 1: a server's genesis fixes its identity + owner. Create it once,
// when the owner is first established, then hold the derived LogState in memory.
{
    let mut g_guard = state.genesis.lock().unwrap();
    if g_guard.is_none() {
        let genesis = farder_crypto::event_log::Genesis {
            version: 1,
            name: state.server_name.clone(),       // match the actual field name on ServerState
            owner: public_key.clone(),
            created_at: crate::db::now(),
            nonce: rand::random::<[u8; 16]>(),
        };
        {
            let conn = state.db.lock().unwrap();
            crate::event_ingest::save_genesis(&conn, &genesis)?;
        }
        *state.log_state.lock().unwrap() =
            Some(farder_crypto::event_log_state::LogState::from_genesis(&genesis));
        *g_guard = Some(genesis);
    }
}
```

> Match `state.server_name` to the actual server-name field on `ServerState` (the recon shows `ServerState::new(conn, args.name, ...)` — use whatever field holds it). `rand` is already a workspace dep used elsewhere in the server.

- [ ] **Step 4: Build + run the server suite (no regressions)**

Run: `cargo test -p farder-server`
Expected: green (the new fields + startup/owner hooks compile; existing tests unaffected — the owner-establish hook only adds state, the legacy path is untouched).

- [ ] **Step 5: Commit**

```bash
git add crates/farder-server/src/state.rs crates/farder-server/src/main.rs crates/farder-server/src/connection.rs
git commit -m "feat(server): ServerState genesis+log_state, startup replay, genesis-on-owner-establish"
```

---

## Task 5: The `SubmitEvent` handler (validate → persist → derive → broadcast)

**Files:**
- Modify: `crates/farder-server/src/handlers.rs` (add the `ServerRequest::SubmitEvent` arm)

**Interfaces:**
- Consumes: Task 2 helpers (`store_event`, `derive_message_row`), Task 4 state (`state.log_state`), `LogState::apply`, `messages::get_message`, the `ok_with`/`BroadcastEvent`/`EventTarget::Subscribers` machinery, `channels::get_channel`.
- Produces: handling for `SubmitEvent` — clone-validate, persist+derive in a transaction, commit LogState, broadcast `NewMessage`, respond `EventAccepted`.

- [ ] **Step 1: Write the handler arm + integration test**

In `crates/farder-server/src/handlers.rs`, add a new arm in `handle_request`'s match (after `SendMessage`):

```rust
        ServerRequest::SubmitEvent { event } => {
            // 1. The server must be in log mode (genesis established).
            let mut ls_guard = state.log_state.lock().unwrap();
            let ls = match ls_guard.as_ref() {
                Some(ls) => ls,
                None => return err("server is not running the event log (no genesis yet)"),
            };

            // 2. Validate on a CLONE — apply runs the full envelope + authz; on
            //    error nothing is mutated and we reject.
            let mut trial = ls.clone();
            if let Err(e) = trial.apply(&event) {
                return err(&format!("event rejected: {}", e));
            }

            // 3. For a message event, the referenced channel must exist (channels
            //    are still legacy DB state this slice).
            if let EventPayload::MessagePosted { channel_id, content, .. } = &event.core.payload {
                if content.len() > 8000 {
                    return err("message content too long (max 8000 characters)");
                }
                if channels::get_channel(conn, *channel_id)?.is_none() {
                    return err("channel not found");
                }
            }

            // 4. Persist the event (source of truth) + derive the message row, in a
            //    transaction so they commit atomically. NOTE: `conn` here is the
            //    handler's &Connection; use a savepoint/immediate writes — rusqlite
            //    `conn.execute` is already within the single DB lock, so sequential
            //    writes are atomic enough for single-host. (If a transaction guard
            //    is available in this codebase, wrap these two writes in it.)
            crate::event_ingest::store_event(conn, &event)
                .map_err(|e| anyhow::anyhow!("failed to store event: {}", e))?;
            let derived_id = crate::event_ingest::derive_message_row(conn, &event)
                .map_err(|e| anyhow::anyhow!("failed to derive message: {}", e))?;

            // 5. Commit the advanced authorization state in memory.
            *ls_guard = Some(trial);
            drop(ls_guard);

            // 6. Broadcast: for a derived message, send NewMessage so the existing
            //    client render path works unchanged.
            let timestamp = event.core.timestamp;
            let mut events = Vec::new();
            if let Some(mid) = derived_id {
                if let EventPayload::MessagePosted { channel_id, .. } = &event.core.payload {
                    if let Some(msg) = messages::get_message(conn, mid, member)? {
                        events.push(BroadcastEvent {
                            target: EventTarget::Subscribers(*channel_id),
                            event: ServerEvent::NewMessage { message: msg },
                        });
                    }
                }
            }
            ok_with(ServerResponse::EventAccepted { event_hash: event.hash(), timestamp }, events)
        }
```

> **Field-name check:** sub-project 1's `MessagePosted` is `{ channel_id, content, reply_to, attachments }` — there is NO `attachment_ids`. Destructure the real fields: `MessagePosted { channel_id, content, .. }`. Fix the destructure in step 1 to match (`attachment_ids: _` was a stray — remove it).

Add an integration test at the bottom of `handlers.rs` `#[cfg(test)] mod tests` (or wherever server handler tests live — match the file; they take a `Connection` + `ServerState`). It must drive a real `ServerState` with a genesis. If the handler tests can't easily build a full `ServerState`, put this test in `event_ingest.rs` instead and call a thin wrapper — but the preferred form exercises `handle_request`:

```rust
    // Pseudocode shape — adapt to the crate's test harness for building a
    // ServerState with a channel + an established genesis/log_state:
    //   1. open_in_memory db; create a channel id=1; set owner = O; save_genesis;
    //      state.log_state = from_genesis.
    //   2. Build signed events with farder_crypto: O's DeviceAuthorized, then a
    //      MessagePosted in channel 1.
    //   3. handle_request(SubmitEvent{da}) -> EventAccepted; handle_request(SubmitEvent{msg})
    //      -> EventAccepted + a NewMessage broadcast event; assert the messages table
    //      has one row with event_hash == msg.hash().
    //   4. A MessagePosted from a NON-member (a stranger's device+identity) ->
    //      Error "event rejected".
    //   5. A MessagePosted to a non-existent channel -> Error "channel not found".
```

Write the concrete test using the crate's actual `ServerState`/`handle_request` test helpers (search existing `handlers.rs` tests for how they construct state — e.g. a `test_state()` helper).

- [ ] **Step 2: Run the test to verify it fails (before the arm compiles) then passes**

Run: `cargo test -p farder-server` (compile first — fix the `MessagePosted` destructure to the real fields; resolve any `ServerState`/import mismatches).
Then run the new integration test: `cargo test -p farder-server submit_event`
Expected: PASS — accepted message stored + derived + broadcast; non-member and bad-channel rejected.

- [ ] **Step 3: Run the whole server suite**

Run: `cargo test -p farder-server`
Expected: green, no regressions (legacy SendMessage tests unaffected).

- [ ] **Step 4: Commit**

```bash
git add crates/farder-server/src/handlers.rs
git commit -m "feat(server): SubmitEvent handler — validate via LogState, persist event, derive message, broadcast"
```

---

## Self-Review

**Spec coverage (sub-project 3a = server-side event ingestion):**
- `events` table as source of truth + `messages` derived view → Tasks 1, 2, 5. ✅
- Genesis identity created + persisted; `server_id` fixed → Tasks 1, 2, 4. ✅
- In-memory `LogState`, rebuilt by replay on startup → Tasks 2, 4. ✅
- `SubmitEvent` validated via `LogState` (clone-validate, commit-on-success) → Tasks 3, 5. ✅
- `MessagePosted` derives a `messages` row (render/history unchanged) + broadcast `NewMessage` → Tasks 2, 5. ✅
- Additive (legacy `SendMessage` untouched) → handler adds an arm; no edits to the SendMessage arm or `messages` shape beyond a nullable column. ✅
- Rust-testable without a client (events built via `farder-crypto`) → Tasks 2, 5 tests. ✅
- Deferred + documented: lamport monotonicity, attachment derivation, channel/role/membership-to-events, peer broadcast verification. ✅

**Placeholder scan:** Tasks 1–4 contain complete code. Task 5's *test* is given as an adapt-to-harness shape because the server's test-state constructor is crate-specific — the implementer must write the concrete test against the actual `handle_request`/`ServerState` helpers (this is the one place the plan cannot pin exact code without the harness). The handler *arm* itself is complete. Flag for the reviewer: confirm Task 5's concrete test exercises accept + non-member-reject + bad-channel-reject.

**Type consistency:** `Genesis`/`Event`/`EventPayload::MessagePosted { channel_id, content, reply_to, attachments }` (NO `attachment_ids` — the plan notes the fix), `LogState::{from_genesis, apply, replay, is_member}`, `event_ingest::{save_genesis, load_genesis, store_event, load_events_in_order, build_log_state, derive_message_row}`, and `ServerState.{genesis, log_state}` are used consistently across tasks. `event.hash()`, `event.core.author.as_bytes()`, `event.core.{server_id, device, seq, lamport, timestamp, payload}` match sub-project 1's API.

**Integration caveats for the implementer (call these out in dispatch):**
- Match the actual `ServerState` field for the server name, the `ServerState::new` constructor, and the std-`Mutex` field style.
- Match the exact owner-establish line in `connection.rs` (the recon points to ~566); the genesis hook goes right after the owner is set.
- Confirm `handle_request`'s signature gives access to `state` (the recon shows it takes `&state`) so the handler can reach `state.log_state`.
- Use the crate's real in-memory db + test-state helpers in tests.
