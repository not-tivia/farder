//! Server-side glue for the mesh event log: persist the genesis, append events
//! to the source-of-truth `events` table, replay them into a `LogState`, and
//! derive the legacy `messages` read-view for `MessagePosted` events.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use farder_crypto::event_log::{Event, EventHash, EventPayload, Genesis};
use farder_crypto::event_log_state::LogState;
use farder_crypto::identity::PublicKey;

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
        EventPayload::AttachmentRedacted { .. } => "AttachmentRedacted",
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
    use farder_crypto::event_log::{AttachmentCap, DeviceCert, EventPayload as EP};
    use farder_crypto::identity::Keypair;

    fn genesis(owner: &Keypair) -> Genesis {
        Genesis { version: 1, name: "t".into(), owner: owner.public_key(), created_at: 1, nonce: [0u8; 16] }
    }

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
}
