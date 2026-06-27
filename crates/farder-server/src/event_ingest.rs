//! Server-side glue for the mesh event log: persist the genesis, append events
//! to the source-of-truth `events` table, replay them into a `LogState`, and
//! derive the legacy `messages` read-view for `MessagePosted` events.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use farder_crypto::event_log::{Event, EventHash, EventPayload, Genesis};
use farder_crypto::event_log_state::LogState;

pub fn save_genesis(conn: &Connection, g: &Genesis) -> Result<()> {
    let body = g.to_bytes();
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
        Some(body) => Ok(Some(Genesis::from_bytes(&body).context("decode genesis")?)),
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
        EventPayload::MemberApproved { .. } => "MemberApproved",
    }
}

pub fn store_event(conn: &Connection, event: &Event) -> Result<()> {
    let channel_id: Option<i64> = match &event.core.payload {
        EventPayload::MessagePosted { channel_id, .. } => Some(*channel_id as i64),
        _ => None,
    };
    let body = event.to_bytes();
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
        out.push(Event::from_bytes(&body).context("decode stored event")?);
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

/// Repair drift: derive a `messages` row for any stored `MessagePosted` event
/// whose `event_hash` has no corresponding `messages` row (e.g. a crash between
/// store_event and derive_message_row). The event log is the source of truth.
/// Returns the number of rows repaired.
pub fn reconcile_messages(conn: &Connection) -> Result<usize> {
    // Collect message-events that lack a derived row.
    let missing: Vec<Vec<u8>> = {
        let mut stmt = conn.prepare(
            "SELECT e.event_body FROM events e \
             LEFT JOIN messages m ON m.event_hash = e.event_hash \
             WHERE e.payload_type = 'MessagePosted' AND m.event_hash IS NULL \
             ORDER BY e.accept_seq ASC",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
        let mut v = Vec::new();
        for row in rows { v.push(row?); }
        v
    };
    let mut repaired = 0;
    for body in missing {
        let event: Event = Event::from_bytes(&body).context("decode event for reconcile")?;
        if derive_message_row(conn, &event)?.is_some() {
            repaired += 1;
        }
    }
    Ok(repaired)
}

/// Resolve a presented invite code to the hash of its `InviteCreated` event, by
/// matching `invite_code_hash(code)` against stored events. Returns `None` if no
/// invite matches (unknown/typo code). The raw code is never stored — only its hash.
pub fn find_invite_event_by_code(conn: &Connection, code: &str) -> Result<Option<EventHash>> {
    let target = farder_crypto::event_log::invite_code_hash(code);
    for event in load_events_in_order(conn)? {
        if let farder_crypto::event_log::EventPayload::InviteCreated { code_hash, .. } =
            &event.core.payload
        {
            if code_hash == &target {
                return Ok(Some(event.hash()));
            }
        }
    }
    Ok(None)
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

    #[test]
    fn find_invite_event_by_code_matches_on_hash() {
        use farder_crypto::event_log::{DeviceCert, invite_code_hash};

        let conn = crate::db::open_in_memory().unwrap();
        let owner = Keypair::generate();
        let dev = Keypair::generate();
        let g = genesis(&owner);
        save_genesis(&conn, &g).unwrap();

        // Owner authorizes their device, then creates an invite for code "JOINME12".
        let da = Event::next(
            &dev, owner.public_key(), g.server_id(), None, 0, 1,
            EP::DeviceAuthorized { cert: DeviceCert::create(&owner, &dev.public_key(), 1) },
        );
        store_event(&conn, &da).unwrap();

        let code = "JOINME12";
        let inv = Event::next(
            &dev, owner.public_key(), g.server_id(), Some(&da), 1, 2,
            EP::InviteCreated {
                code_hash: invite_code_hash(code),
                max_uses: 5,
                expires_at: 9_999_999_999,
                requires_approval: false,
            },
        );
        store_event(&conn, &inv).unwrap();

        // The right code resolves to the invite's event hash; a wrong code resolves to None.
        assert_eq!(find_invite_event_by_code(&conn, code).unwrap().as_deref(), Some(inv.hash().as_str()));
        assert_eq!(find_invite_event_by_code(&conn, "WRONGcode").unwrap(), None);
    }

    #[test]
    fn reconcile_derives_missing_message_rows() {
        let conn = crate::db::open_in_memory().unwrap();
        let owner = Keypair::generate();
        let dev = Keypair::generate();
        let g = genesis(&owner);
        save_genesis(&conn, &g).unwrap();
        let da = Event::next(&dev, owner.public_key(), g.server_id(), None, 0, 1,
            EP::DeviceAuthorized { cert: DeviceCert::create(&owner, &dev.public_key(), 1) });
        let msg = Event::next(&dev, owner.public_key(), g.server_id(), Some(&da), 1, 2,
            EP::MessagePosted { channel_id: 1, content: "drifted".into(), reply_to: None, attachments: vec![] });
        // Store events but DO NOT derive the message row (simulate the crash window).
        store_event(&conn, &da).unwrap();
        store_event(&conn, &msg).unwrap();
        let before: i64 = conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap();
        assert_eq!(before, 0);
        // Reconcile derives the missing row, and is idempotent (a second run repairs nothing).
        assert_eq!(reconcile_messages(&conn).unwrap(), 1);
        assert_eq!(reconcile_messages(&conn).unwrap(), 0);
        let (content, eh): (String, String) = conn.query_row(
            "SELECT content, event_hash FROM messages", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(content, "drifted");
        assert_eq!(eh, msg.hash());
    }
}
