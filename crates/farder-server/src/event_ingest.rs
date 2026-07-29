//! Server-side glue for the mesh event log: persist the genesis, append events
//! to the source-of-truth `events` table, replay them into a `LogState`, and
//! derive the legacy `messages` read-view for `MessagePosted` events.

use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use farder_crypto::event_log::{
    ChannelClass, Event, EventHash, EventPayload, Genesis, E2EE_CHANNEL_ID_FLOOR,
    MAX_DECLARED_LEAVES_PER_COMMIT, MAX_E2EE_ATTACHMENTS, MAX_E2EE_CIPHERTEXT_BYTES,
    MAX_CHANNEL_NAME_BYTES, MAX_E2EE_EDIT_CIPHERTEXT_BYTES, MAX_EVENT_FUTURE_SKEW_SECS,
    MAX_KEY_PACKAGE_BYTES, MAX_MLS_MESSAGE_BYTES, MAX_MLS_WELCOME_BYTES, MAX_RESET_WELCOMES,
};
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
        // Rung-2 MLS/E2EE variants — DORMANT: LogState rejects them until the
        // fold rules land; ingest behavior for them is sub-3. Names only here.
        EventPayload::ChannelCreated { .. } => "ChannelCreated",
        EventPayload::MlsKeyPackagePublished { .. } => "MlsKeyPackagePublished",
        EventPayload::MlsCommit { .. } => "MlsCommit",
        EventPayload::MlsWelcome { .. } => "MlsWelcome",
        EventPayload::MlsLeafConfirmed { .. } => "MlsLeafConfirmed",
        EventPayload::MlsGroupReset { .. } => "MlsGroupReset",
        EventPayload::MessagePostedE2ee { .. } => "MessagePostedE2ee",
        EventPayload::MessageEditedE2ee { .. } => "MessageEditedE2ee",
        EventPayload::MessageDeleted { .. } => "MessageDeleted",
        EventPayload::DeviceRevoked { .. } => "DeviceRevoked",
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

// ---------------------------------------------------------------------------
// Rung-2 ingest duties the fold deliberately does NOT own
// (sub-2 resolved ambiguity #9 + `docs/modules/crypto.md`'s stated division).
// ---------------------------------------------------------------------------

/// Per-variant size caps + the `core.timestamp` upper bound, as of server time.
///
/// See [`check_ingest_caps_at`]; this is the production entry point and reads
/// the clock itself.
pub fn check_ingest_caps(event: &Event) -> Result<()> {
    check_ingest_caps_at(event, crate::db::now())
}

/// The clock-injected form, so the skew bound is testable without racing a
/// real second boundary.
///
/// Two jobs, both **blind** — every rule below is a byte count, a vector
/// length, or a clock comparison; no payload field is inspected for meaning:
///
/// 1. **Per-variant size caps** (spec "Size caps", M4/F8). `LogState::apply`
///    checks none of them by design, so unbounded is unbounded until here.
/// 2. **The `core.timestamp` upper bound** (`crypto.md`'s stated sub-3
///    residual). `core.timestamp` is an untrusted device claim that the fold
///    uses as its device-liveness and cert-expiry clock; a forward-dated event
///    walks that clock into the future and keeps a dead cert alive. Only the
///    FUTURE is bounded — a past claim is already handled by the fold's
///    monotonicity rules, and legacy events carry small timestamps.
///
/// Called before the fold's `LogState` clone: that clone is the
/// allocation-heavy step of ingest, and a cap breach must not be able to pay
/// for it. Pinned by
/// `oversized_sealed_ciphertext_is_refused_before_the_fold_runs`.
pub fn check_ingest_caps_at(event: &Event, now: u64) -> Result<()> {
    ensure!(
        event.core.timestamp <= now.saturating_add(MAX_EVENT_FUTURE_SKEW_SECS),
        "event timestamp is more than {MAX_EVENT_FUTURE_SKEW_SECS}s ahead of server time"
    );

    /// One cap, reported the same way every time.
    fn cap(what: &str, len: usize, max: usize) -> Result<()> {
        ensure!(len <= max, "{what} is {len} bytes, over the {max}-byte cap");
        Ok(())
    }
    fn cap_len(what: &str, len: usize, max: usize) -> Result<()> {
        ensure!(len <= max, "{what} has {len} entries, over the cap of {max}");
        Ok(())
    }

    match &event.core.payload {
        EventPayload::ChannelCreated { name, kind, .. } => {
            cap("ChannelCreated.name", name.len(), MAX_CHANNEL_NAME_BYTES)?;
            cap("ChannelCreated.kind", kind.len(), MAX_CHANNEL_NAME_BYTES)?;
        }
        EventPayload::MlsKeyPackagePublished { key_package, .. } => {
            cap(
                "MlsKeyPackagePublished.key_package",
                key_package.len(),
                MAX_KEY_PACKAGE_BYTES,
            )?;
        }
        EventPayload::MlsCommit { mls_message, adds, removes, .. } => {
            cap("MlsCommit.mls_message", mls_message.len(), MAX_MLS_MESSAGE_BYTES)?;
            cap_len("MlsCommit.adds", adds.len(), MAX_DECLARED_LEAVES_PER_COMMIT)?;
            cap_len("MlsCommit.removes", removes.len(), MAX_DECLARED_LEAVES_PER_COMMIT)?;
        }
        EventPayload::MlsWelcome { welcome, .. } => {
            cap("MlsWelcome.welcome", welcome.len(), MAX_MLS_WELCOME_BYTES)?;
        }
        EventPayload::MlsGroupReset { welcomes, .. } => {
            cap_len("MlsGroupReset.welcomes", welcomes.len(), MAX_RESET_WELCOMES)?;
        }
        EventPayload::MessagePostedE2ee { ciphertext, attachments, .. } => {
            cap(
                "MessagePostedE2ee.ciphertext",
                ciphertext.len(),
                MAX_E2EE_CIPHERTEXT_BYTES,
            )?;
            cap_len(
                "MessagePostedE2ee.attachments",
                attachments.len(),
                MAX_E2EE_ATTACHMENTS,
            )?;
        }
        EventPayload::MessageEditedE2ee { ciphertext, .. } => {
            cap(
                "MessageEditedE2ee.ciphertext",
                ciphertext.len(),
                MAX_E2EE_EDIT_CIPHERTEXT_BYTES,
            )?;
        }
        // Rung-1 variants: their bounds are request-layer rules that predate
        // this pass (e.g. `MessagePosted`'s 8000-char check in the SubmitEvent
        // arm) and are deliberately left where they are.
        _ => {}
    }
    Ok(())
}

/// The `channels` row an accepted `ChannelCreated` materializes, written inside
/// the SAME transaction that stores the event so the mirror and the log cannot
/// disagree across a crash (resolved ambiguity #1). Returns the declared class,
/// or `None` for any other payload.
///
/// The fold has already authorized the event (owner-authored, id never seen,
/// no plaintext history in the log, parent class inherited). What ingest adds
/// is everything the fold cannot see because it lives in the legacy DB, plus
/// the shape this rung supports:
///
/// - **id floor** — `channel_id` is CLIENT-chosen, so it must stay clear of the
///   `channels` AUTOINCREMENT space or a declared channel could land on a legacy
///   rowid (resolved ambiguity #7).
/// - **collision** — an id already in `channels` is refused outright; the log's
///   own immutability rule cannot see legacy rows.
/// - **`messages` emptiness** — belt-and-braces for the case the fold's
///   `plaintext_history_channels` cannot catch: a legacy DB channel that carries
///   plaintext the log never saw. Declaring E2ee over it would put a lock icon
///   on messages every host already read.
/// - **shape** — `kind == "text"` and `parent: None` this rung. Threads under a
///   sealed parent are refused by the spec (coexistence row 12) and categories
///   are legacy DB state with no log representation; the fold's parent-class
///   inheritance rule stays live for a later rung.
///
/// Every refusal is an `Err`, which rolls the ingest transaction back — so a
/// refused `ChannelCreated` leaves no channel row, no stored event and no log
/// advance.
pub fn materialize_channel_created(conn: &Connection, event: &Event) -> Result<Option<ChannelClass>> {
    let EventPayload::ChannelCreated { channel_id, name, kind, class, parent } =
        &event.core.payload
    else {
        return Ok(None);
    };

    ensure!(
        *channel_id >= E2EE_CHANNEL_ID_FLOOR,
        "ChannelCreated.channel_id must be at or above the reserved floor {E2EE_CHANNEL_ID_FLOOR}"
    );
    let collision: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM channels WHERE id = ?1",
            params![*channel_id as i64],
            |r| r.get(0),
        )
        .optional()?;
    ensure!(collision.is_none(), "channel id already exists on this server");
    let has_messages: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM messages WHERE channel_id = ?1 LIMIT 1",
            params![*channel_id as i64],
            |r| r.get(0),
        )
        .optional()?;
    ensure!(
        has_messages.is_none(),
        "channel id already carries message history on this server"
    );
    if kind != "text" || parent.is_some() {
        bail!("unsupported ChannelCreated shape (this rung accepts kind=\"text\" with no parent)");
    }

    let position: i64 = conn.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM channels",
        [],
        |r| r.get(0),
    )?;
    let class_str = match class {
        ChannelClass::Plaintext => "plaintext",
        ChannelClass::E2ee => "e2ee",
    };
    conn.execute(
        "INSERT INTO channels (id, name, channel_type, position, content_class) \
         VALUES (?1, ?2, 'text', ?3, ?4)",
        params![*channel_id as i64, name, position, class_str],
    )?;
    Ok(Some(*class))
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
    // Through the choke point (`messages::insert_derived_row`), never raw SQL —
    // so this path is class-gated like every other writer.
    let id = crate::messages::insert_derived_row(
        conn,
        *channel_id,
        &event.core.author,
        content,
        event.core.timestamp,
        None,
        &event.hash(),
    )?;
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

    // -----------------------------------------------------------------------
    // Rung 2 — ingest caps + the timestamp bound (spec "Size caps" M4/F8, and
    // `crypto.md`'s stated sub-3 residual). These are the checks the FOLD
    // deliberately does not make, so nothing else in the system bounds them.
    // Each is exercised at the limit AND one unit over: a cap that is merely
    // "somewhere near" the constant is not a cap.
    // -----------------------------------------------------------------------

    use farder_crypto::event_log::{
        DeclaredAdd, DeclaredRemove, MAX_CHANNEL_NAME_BYTES, MAX_DECLARED_LEAVES_PER_COMMIT,
        MAX_E2EE_ATTACHMENTS, MAX_E2EE_CIPHERTEXT_BYTES, MAX_EVENT_FUTURE_SKEW_SECS,
        MAX_KEY_PACKAGE_BYTES, MAX_MLS_MESSAGE_BYTES, MAX_MLS_WELCOME_BYTES, MAX_RESET_WELCOMES,
    };

    /// Sign a payload as a throwaway device. Caps run before every signature,
    /// authz and ordering check, so the identity behind the event is irrelevant
    /// here — which is the point: a cap breach is refused before the server does
    /// any work at all on the event.
    fn capped(payload: EP, timestamp: u64) -> Event {
        let kp = Keypair::generate();
        Event::next(&kp, kp.public_key(), "sid".into(), None, 0, timestamp, payload)
    }

    fn commit(mls_message: Vec<u8>, adds: usize, removes: usize) -> EP {
        let who = Keypair::generate().public_key();
        EP::MlsCommit {
            channel_id: 1,
            generation: 0,
            epoch: 0,
            mls_message,
            adds: (0..adds)
                .map(|_| DeclaredAdd {
                    identity: who.clone(),
                    device: "d".into(),
                    key_package: "k".into(),
                })
                .collect(),
            removes: (0..removes)
                .map(|_| DeclaredRemove { identity: who.clone(), device: "d".into() })
                .collect(),
            prev_epoch_authenticator: [0u8; 32],
            post_epoch_authenticator: [0u8; 32],
            post_tree_hash: [0u8; 32],
            authz_head: "h".into(),
            store_instance_hash: [0u8; 32],
        }
    }

    fn attachment_caps(n: usize) -> Vec<AttachmentCap> {
        (0..n)
            .map(|_| AttachmentCap {
                content_hash: "h".into(),
                declared_type: "image/png".into(),
                size: 1,
                uploader: Keypair::generate().public_key(),
            })
            .collect()
    }

    #[test]
    fn every_ingest_cap_accepts_exactly_at_the_limit_and_refuses_one_over() {
        let pk = Keypair::generate().public_key();
        // (label, at-the-limit payload, one-over payload)
        let cases: Vec<(&str, EP, EP)> = vec![
            (
                "MessagePostedE2ee.ciphertext",
                EP::MessagePostedE2ee {
                    channel_id: 1, generation: 0, epoch: 0,
                    ciphertext: vec![7u8; MAX_E2EE_CIPHERTEXT_BYTES],
                    reply_to: None, attachments: vec![], authz_head: "h".into(),
                },
                EP::MessagePostedE2ee {
                    channel_id: 1, generation: 0, epoch: 0,
                    ciphertext: vec![7u8; MAX_E2EE_CIPHERTEXT_BYTES + 1],
                    reply_to: None, attachments: vec![], authz_head: "h".into(),
                },
            ),
            (
                "MessagePostedE2ee.attachments",
                EP::MessagePostedE2ee {
                    channel_id: 1, generation: 0, epoch: 0, ciphertext: vec![],
                    reply_to: None, attachments: attachment_caps(MAX_E2EE_ATTACHMENTS),
                    authz_head: "h".into(),
                },
                EP::MessagePostedE2ee {
                    channel_id: 1, generation: 0, epoch: 0, ciphertext: vec![],
                    reply_to: None, attachments: attachment_caps(MAX_E2EE_ATTACHMENTS + 1),
                    authz_head: "h".into(),
                },
            ),
            (
                "MessageEditedE2ee.ciphertext",
                EP::MessageEditedE2ee {
                    channel_id: 1, target: "t".into(), generation: 0, epoch: 0,
                    ciphertext: vec![7u8; MAX_E2EE_CIPHERTEXT_BYTES], authz_head: "h".into(),
                },
                EP::MessageEditedE2ee {
                    channel_id: 1, target: "t".into(), generation: 0, epoch: 0,
                    ciphertext: vec![7u8; MAX_E2EE_CIPHERTEXT_BYTES + 1], authz_head: "h".into(),
                },
            ),
            (
                "MlsCommit.mls_message",
                commit(vec![1u8; MAX_MLS_MESSAGE_BYTES], 0, 0),
                commit(vec![1u8; MAX_MLS_MESSAGE_BYTES + 1], 0, 0),
            ),
            (
                "MlsCommit.adds",
                commit(vec![], MAX_DECLARED_LEAVES_PER_COMMIT, 0),
                commit(vec![], MAX_DECLARED_LEAVES_PER_COMMIT + 1, 0),
            ),
            (
                "MlsCommit.removes",
                commit(vec![], 0, MAX_DECLARED_LEAVES_PER_COMMIT),
                commit(vec![], 0, MAX_DECLARED_LEAVES_PER_COMMIT + 1),
            ),
            (
                "MlsWelcome.welcome",
                EP::MlsWelcome {
                    channel_id: 1, generation: 0, commit: "c".into(),
                    for_member: pk.clone(), for_device: "d".into(),
                    welcome: vec![2u8; MAX_MLS_WELCOME_BYTES],
                },
                EP::MlsWelcome {
                    channel_id: 1, generation: 0, commit: "c".into(),
                    for_member: pk.clone(), for_device: "d".into(),
                    welcome: vec![2u8; MAX_MLS_WELCOME_BYTES + 1],
                },
            ),
            (
                "MlsKeyPackagePublished.key_package",
                EP::MlsKeyPackagePublished {
                    key_package: vec![3u8; MAX_KEY_PACKAGE_BYTES],
                    store_instance_hash: [0u8; 32], expires_at_log_pos: 1,
                },
                EP::MlsKeyPackagePublished {
                    key_package: vec![3u8; MAX_KEY_PACKAGE_BYTES + 1],
                    store_instance_hash: [0u8; 32], expires_at_log_pos: 1,
                },
            ),
            (
                "MlsGroupReset.welcomes",
                EP::MlsGroupReset {
                    channel_id: 1, new_generation: 1,
                    welcomes: vec!["w".to_string(); MAX_RESET_WELCOMES],
                    post_tree_hash: [0u8; 32],
                },
                EP::MlsGroupReset {
                    channel_id: 1, new_generation: 1,
                    welcomes: vec!["w".to_string(); MAX_RESET_WELCOMES + 1],
                    post_tree_hash: [0u8; 32],
                },
            ),
            (
                "ChannelCreated.name",
                EP::ChannelCreated {
                    channel_id: 1, name: "n".repeat(MAX_CHANNEL_NAME_BYTES),
                    kind: "text".into(), class: ChannelClass::E2ee, parent: None,
                },
                EP::ChannelCreated {
                    channel_id: 1, name: "n".repeat(MAX_CHANNEL_NAME_BYTES + 1),
                    kind: "text".into(), class: ChannelClass::E2ee, parent: None,
                },
            ),
        ];

        for (label, at_limit, over) in cases {
            check_ingest_caps_at(&capped(at_limit, 100), 1_000)
                .unwrap_or_else(|e| panic!("{label} must be ACCEPTED exactly at the limit: {e}"));
            let err = check_ingest_caps_at(&capped(over, 100), 1_000)
                .unwrap_err()
                .to_string();
            assert!(err.contains(label), "{label}: unexpected refusal message {err:?}");
        }
    }

    #[test]
    fn the_future_skew_bound_is_exact_and_applies_to_every_variant() {
        let now = 1_000_000u64;
        // A Rung-1 payload: the bound is envelope-level, not an E2EE-only rule —
        // `core.timestamp` is the fold's device-liveness and cert-expiry clock
        // for EVERY variant, so a forward-dated legacy event is just as good a
        // way to keep a dead cert alive.
        let plain = || EP::MessagePosted {
            channel_id: 1, content: "x".into(), reply_to: None, attachments: vec![],
        };
        check_ingest_caps_at(&capped(plain(), 1), now).expect("a past timestamp is fine");
        check_ingest_caps_at(&capped(plain(), now), now).expect("now is fine");
        check_ingest_caps_at(&capped(plain(), now + MAX_EVENT_FUTURE_SKEW_SECS), now)
            .expect("exactly at the skew bound is accepted");
        let err = check_ingest_caps_at(&capped(plain(), now + MAX_EVENT_FUTURE_SKEW_SECS + 1), now)
            .unwrap_err()
            .to_string();
        assert!(err.contains("ahead of server time"), "{err}");

        // The bound saturates rather than wrapping: a `u64::MAX` claim against a
        // real clock is still refused. (Within 300s of `u64::MAX` the bound
        // degenerates to "accept" — stated, not hidden; server time is unix
        // seconds, so that is ~5.8e11 years away.)
        assert!(
            check_ingest_caps_at(&capped(plain(), u64::MAX), now).is_err(),
            "a u64::MAX timestamp claim must not wrap into acceptance"
        );
        check_ingest_caps_at(&capped(plain(), u64::MAX), u64::MAX - 1)
            .expect("saturating add, not a panic on overflow");
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
