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

pub(crate) fn payload_type(p: &EventPayload) -> &'static str {
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
        // Rung-2 MLS/E2EE variants — LIVE since 4a: `LogState` folds them, and
        // these `payload_type` names now feed the `MlsControlEvent` broadcast
        // (Task 7) and the fetch surfaces (`fetch_welcomes_for`,
        // `fetch_mls_control`, `fetch_key_packages`, `fetch_device_certs`).
        // Names only here.
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

/// Map an `EventRef` reply target onto the numeric `messages.id` the client
/// renders threading from (spec F9 / coexistence row 17).
///
/// E2EE channels are **log-only** — there is no legacy `SendMessage` fallback
/// for them — so without this mapping every reply in a sealed channel is
/// silently dropped, which is exactly the shipped `MessageInput.tsx:283` TODO.
///
/// An UNRESOLVABLE target derives `NULL` rather than failing the event: events
/// can arrive before the row they cite exists (crash mid-derive, or a future
/// replicated log), and losing the message to save the edge is the wrong trade.
/// The edge is not lost either — [`repair_reply_links`] re-resolves every NULL
/// edge whose event actually cited a target, at every startup.
///
/// The lookup is exact-match on `messages.event_hash`, which
/// `idx_messages_event_hash` makes unique and O(log n).
///
/// **Scoped to `channel_id`**, matching `sealed_edit_target` and
/// `apply_tombstone`, which both refuse a target in another channel. Without the
/// scope a sealed post could cite a plaintext message's hash (or the reverse)
/// and the derived `reply_to` would point ACROSS channels — `fetch_history`
/// would then hand clients a reply edge into a conversation the reader may not
/// even be able to see, and in a sealed channel it would point at a body the
/// channel's whole purpose is to keep the server out of.
///
/// An out-of-channel (or not-yet-derived) target resolves to `None` rather than
/// failing the event; `repair_reply_links` re-checks later under the same scope.
fn resolve_reply_target(
    conn: &Connection,
    channel_id: u64,
    reply_to: Option<&str>,
) -> Result<Option<u64>> {
    let Some(target) = reply_to else { return Ok(None) };
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM messages WHERE event_hash = ?1 AND channel_id = ?2",
            params![target, channel_id as i64],
            |r| r.get(0),
        )
        .optional()?;
    Ok(id.map(|v| v as u64))
}

/// Derive the `messages` read-view row for a content event and return its id.
/// Non-content payloads return `None`.
///
/// Two arms, one per content class, and the class gate lives in the door each
/// one calls (`messages.rs`'s choke point), never here:
///
/// - `MessagePosted` → `insert_derived_row`: plaintext body, FTS-indexed.
/// - `MessagePostedE2ee` → `insert_sealed_row`: the ciphertext lands in `sealed`
///   verbatim, `content` is `''`, and the row never enters `messages_fts`
///   (coexistence row 7a). The server stores opaque bytes and reads none of them.
///
/// Attachments are derived separately (`derive_attachments`, `MessagePosted`
/// only this slice).
pub fn derive_message_row(conn: &Connection, event: &Event) -> Result<Option<u64>> {
    match &event.core.payload {
        EventPayload::MessagePosted { channel_id, content, reply_to, .. } => {
            let reply = resolve_reply_target(conn, *channel_id, reply_to.as_deref())?;
            // Through the choke point (`messages::insert_derived_row`), never raw
            // SQL — so this path is class-gated like every other writer.
            let id = crate::messages::insert_derived_row(
                conn,
                *channel_id,
                &event.core.author,
                content,
                event.core.timestamp,
                reply,
                &event.hash(),
            )?;
            Ok(Some(id))
        }
        EventPayload::MessagePostedE2ee { channel_id, ciphertext, reply_to, .. } => {
            let reply = resolve_reply_target(conn, *channel_id, reply_to.as_deref())?;
            let id = crate::messages::insert_sealed_row(
                conn,
                *channel_id,
                &event.core.author,
                ciphertext,
                event.core.timestamp,
                reply,
                &event.hash(),
            )?;
            Ok(Some(id))
        }
        _ => Ok(None),
    }
}

/// Resolve a sealed edit's target to a derived row id, verifying everything the
/// fold deliberately cannot.
///
/// `LogState` keeps no per-message index by design (it would grow without bound
/// and buy the fold nothing), so it authorizes `MessageEditedE2ee` on the send
/// gates plus "not tombstoned" and leaves TARGET AUTHORSHIP to ingest — see
/// `event_log_state.rs`'s comment on the variant. This is that check, made
/// against the derived view, which is the only per-message index that exists.
///
/// - `Ok(None)` — no row carries that hash. Ingest treats it as a refusal; the
///   reconcile pass treats it as "already deleted / not yet derived" and skips.
/// - `Err` — a row exists but the edit has no business touching it: wrong
///   channel, not a sealed row, or a different author. Only the author may edit
///   their own message; moderators delete, they do not rewrite.
///
/// `strict` splits the two callers, and the split is load-bearing:
///
/// - INGEST (`strict = true`) is judging a live event and must refuse a
///   mismatch — that refusal is the authorship check itself.
/// - RECONCILE (`strict = false`) is replaying events ingest ALREADY accepted,
///   so a mismatch is not an attack, it is drift the server itself created.
///   `anonymize_messages_by_author` (the account-deletion path) rewrites
///   `messages.author` to `DELETED_USER_KEY` on sealed rows too, so after any
///   member exercises data deletion, their old sealed edits no longer match
///   their rows. Erroring there would abort `reconcile_messages` before its
///   reply-link pass on EVERY subsequent boot, permanently — a deletion request
///   would silently disable log reconciliation for the whole server. Skip
///   instead: the edit was authorized when it was accepted, and the row it
///   describes is already anonymized.
fn sealed_edit_target(
    conn: &Connection,
    channel_id: u64,
    target: &str,
    author: &PublicKey,
    strict: bool,
) -> Result<Option<u64>> {
    let row: Option<(i64, i64, Vec<u8>, i64)> = conn
        .query_row(
            "SELECT id, channel_id, author, is_e2ee FROM messages WHERE event_hash = ?1",
            params![target],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((id, row_channel, row_author, is_e2ee)) = row else {
        return Ok(None);
    };
    ensure!(
        row_channel as u64 == channel_id,
        "sealed edit cites a target in a different channel"
    );
    ensure!(is_e2ee != 0, "sealed edit targets a plaintext message");
    if row_author.as_slice() != author.as_bytes().as_slice() {
        ensure!(!strict, "only a message's own author may edit it");
        return Ok(None);
    }
    Ok(Some(id as u64))
}

/// Apply an accepted `MessageEditedE2ee`: replace the target row's ciphertext in
/// place. Returns the edited row id, or `None` for any other payload.
///
/// An UNKNOWN target is an `Err`, which rolls the whole ingest transaction back
/// (event not stored, fold not advanced). That refusal is what bounds the
/// fold's state: the fold cannot tell a real target from a fabricated one, so if
/// ingest accepted edits citing nothing, `MessageEditedE2ee` would be a free
/// write into an E2EE channel's history.
///
/// The row keeps `content = ''` and stays out of `messages_fts`
/// (`messages::update_sealed_row`); only `sealed` and `edited_at` change.
pub fn apply_sealed_edit(conn: &Connection, event: &Event) -> Result<Option<u64>> {
    let EventPayload::MessageEditedE2ee { channel_id, target, ciphertext, .. } =
        &event.core.payload
    else {
        return Ok(None);
    };
    let id = sealed_edit_target(conn, *channel_id, target, &event.core.author, true)?
        .context("sealed edit cites a message this server has not derived")?;
    crate::messages::update_sealed_row(conn, id, ciphertext, event.core.timestamp)?;
    Ok(Some(id))
}

/// What an accepted tombstone actually removed.
///
/// The plan's shape was `Option<u64>`; the orphaned blob ids ride along because
/// the handler must hand them to the same file-GC path the legacy
/// `DeleteMessage` arm uses — dropping them would leak blob bytes for every
/// log-deleted message.
pub struct TombstoneApplied {
    pub message_id: u64,
    pub channel_id: u64,
    pub orphaned_file_ids: Vec<u64>,
}

/// Apply an accepted `MessageDeleted`: HARD-DELETE the derived row (spec F2).
/// Returns what was removed, or `None` for any other payload.
///
/// Content-blind delete is the ONLY moderation mechanism in an E2EE channel, so
/// this path is load-bearing rather than cleanup. Both halves matter:
///
/// - the row is deleted here, and
/// - `reconcile_messages` consults the tombstone set at every startup so the
///   next boot does not re-derive it from the still-stored event. Without that,
///   deletion silently undoes itself on restart.
///
/// Authorship is split exactly where the fold splits it. `DeleteReason::Moderation`
/// is fully authorized by the fold (it checks the `kick` capability itself), so
/// ingest adds only target existence and channel agreement. `DeleteReason::Author`
/// needs the per-message index the fold omits, so ingest checks it here: the
/// deleter must be the row's author.
///
/// An unknown target is an `Err` (transaction rolled back) for the same reason
/// as sealed edits: it is what bounds the fold's `tombstones` set to targets
/// that exist.
pub fn apply_tombstone(conn: &Connection, event: &Event) -> Result<Option<TombstoneApplied>> {
    use farder_crypto::event_log::DeleteReason;

    let EventPayload::MessageDeleted { channel_id, target, reason } = &event.core.payload else {
        return Ok(None);
    };
    let row: Option<(i64, i64, Vec<u8>)> = conn
        .query_row(
            "SELECT id, channel_id, author FROM messages WHERE event_hash = ?1",
            params![target],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (id, row_channel, row_author) =
        row.context("tombstone cites a message this server has not derived")?;
    ensure!(
        row_channel as u64 == *channel_id,
        "tombstone cites a target in a different channel"
    );
    if *reason == DeleteReason::Author {
        ensure!(
            row_author.as_slice() == event.core.author.as_bytes().as_slice(),
            "an author-reason delete must be authored by the message's author"
        );
    }
    let orphaned_file_ids = crate::messages::delete_message(conn, id as u64)?;
    Ok(Some(TombstoneApplied {
        message_id: id as u64,
        channel_id: *channel_id,
        orphaned_file_ids,
    }))
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

/// Every target an accepted `MessageDeleted` has ever named.
///
/// Two independent sources, unioned, because the union is the FAIL-CLOSED
/// combination — more tombstones can only mean fewer resurrections:
///
/// 1. the stored `MessageDeleted` events. Ingest stores an event only AFTER the
///    fold accepted it, so this set equals the fold's by construction, and it is
///    the only source available when no `LogState` is in hand;
/// 2. the fold's own `LogState::is_tombstoned`, consulted per candidate below.
///
/// The fold's set is not enumerable (`tombstones` is private, deliberately), so
/// this returns source 1 and callers cross-check source 2 per event hash.
fn stored_tombstone_targets(conn: &Connection) -> Result<std::collections::HashSet<String>> {
    let bodies: Vec<Vec<u8>> = {
        let mut stmt = conn.prepare(
            "SELECT event_body FROM events WHERE payload_type = 'MessageDeleted' \
             ORDER BY accept_seq ASC",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
        let mut v = Vec::new();
        for row in rows { v.push(row?); }
        v
    };
    let mut set = std::collections::HashSet::new();
    for body in bodies {
        let event = Event::from_bytes(&body).context("decode tombstone for reconcile")?;
        if let EventPayload::MessageDeleted { target, .. } = &event.core.payload {
            set.insert(target.clone());
        }
    }
    Ok(set)
}

/// Repair drift between the log (source of truth) and the derived `messages`
/// view, at every startup. Returns the number of rows DERIVED (the other passes
/// log their own counts).
///
/// `log_state` is the point of the signature: without it this function
/// RESURRECTS DELETED MESSAGES. It re-derives any log message lacking a row, and
/// a content-blind delete removes the row while the `MessageDeleted` event stays
/// stored forever — so unless deletion is consulted here, the only moderation
/// mechanism an E2EE channel has silently undoes itself on the next restart
/// (spec F2).
///
/// Four passes, in order:
///
/// 1. **derive** every `MessagePosted` / `MessagePostedE2ee` with no row, EXCEPT
///    tombstoned ones;
/// 2. **sweep** any row whose event is tombstoned but which is somehow still
///    present (a restored snapshot, an operator with `sqlite3`) — the tombstone
///    is durable, so it wins;
/// 3. **replay sealed edits** in accept order, so a rebuilt row carries the
///    LATEST ciphertext rather than the original;
/// 4. **repair reply links** whose target had not been derived yet at the time.
///
/// Idempotent: a second run in a row repairs nothing.
pub fn reconcile_messages(conn: &Connection, log_state: Option<&LogState>) -> Result<usize> {
    let stored_tombstones = stored_tombstone_targets(conn)?;
    let is_tombstoned = |hash: &str| -> bool {
        stored_tombstones.contains(hash)
            || log_state.map(|ls| ls.is_tombstoned(hash)).unwrap_or(false)
    };

    // --- 1. Derive missing content rows (both classes). ---
    let missing: Vec<Vec<u8>> = {
        let mut stmt = conn.prepare(
            "SELECT e.event_body FROM events e \
             LEFT JOIN messages m ON m.event_hash = e.event_hash \
             WHERE e.payload_type IN ('MessagePosted', 'MessagePostedE2ee') \
               AND m.event_hash IS NULL \
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
        if is_tombstoned(&event.hash()) {
            continue;
        }
        if derive_message_row(conn, &event)?.is_some() {
            repaired += 1;
        }
    }

    // --- 2. Sweep rows the log says are deleted. ---
    let mut swept = 0usize;
    for target in &stored_tombstones {
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM messages WHERE event_hash = ?1",
                params![target],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            crate::messages::delete_message(conn, id as u64)?;
            swept += 1;
        }
    }
    if swept > 0 {
        tracing::warn!(count = swept, "swept derived rows the log had tombstoned");
    }

    // --- 3. Replay sealed edits (accept order; last edit wins). ---
    let edits = replay_sealed_edits(conn, &is_tombstoned)?;

    // --- 4. Repair reply edges that were unresolvable at derive time. ---
    let links = repair_reply_links(conn)?;
    if edits > 0 || links > 0 {
        tracing::info!(edits, links, "reconciled sealed edits and reply links");
    }

    Ok(repaired)
}

/// Re-apply every stored `MessageEditedE2ee` to its target row, in accept order.
///
/// This is what makes a from-events REBUILD equal the live view: derivation
/// alone gives every row its ORIGINAL ciphertext, so a wiped-and-rebuilt view
/// would silently roll back every edit ever made. Replaying in accept order
/// makes the last edit win, which is exactly what the live path produced.
///
/// Idempotent by construction (an UPDATE to the same bytes). A target that is
/// absent — tombstoned, or not yet derived — is SKIPPED, not an error: the live
/// path already refused unknown targets, so the only way to reach one here is a
/// message that was legitimately deleted afterwards.
fn replay_sealed_edits(
    conn: &Connection,
    is_tombstoned: &dyn Fn(&str) -> bool,
) -> Result<usize> {
    let bodies: Vec<Vec<u8>> = {
        let mut stmt = conn.prepare(
            "SELECT event_body FROM events WHERE payload_type = 'MessageEditedE2ee' \
             ORDER BY accept_seq ASC",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
        let mut v = Vec::new();
        for row in rows { v.push(row?); }
        v
    };
    let mut applied = 0usize;
    for body in bodies {
        let event = Event::from_bytes(&body).context("decode sealed edit for reconcile")?;
        let EventPayload::MessageEditedE2ee { channel_id, target, ciphertext, .. } =
            &event.core.payload
        else {
            continue;
        };
        if is_tombstoned(target) {
            continue;
        }
        let Some(id) = sealed_edit_target(conn, *channel_id, target, &event.core.author, false)? else {
            continue;
        };
        crate::messages::update_sealed_row(conn, id, ciphertext, event.core.timestamp)?;
        applied += 1;
    }
    Ok(applied)
}

/// Re-resolve reply edges that were `NULL` at derive time because the cited
/// message had not been derived yet (out-of-order arrival, a crash mid-derive,
/// or a future replicated log). Returns the number of edges restored.
///
/// The pairing with [`resolve_reply_target`] is the whole design: derivation
/// never fails an event over an unresolvable reply, and this pass guarantees the
/// edge is not lost permanently. Idempotent — it only ever looks at rows whose
/// `reply_to` is still NULL while their event cited a target.
pub fn repair_reply_links(conn: &Connection) -> Result<usize> {
    let rows: Vec<(Vec<u8>, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT e.event_body, m.id FROM events e \
             JOIN messages m ON m.event_hash = e.event_hash \
             WHERE e.payload_type IN ('MessagePosted', 'MessagePostedE2ee') \
               AND m.reply_to IS NULL \
             ORDER BY e.accept_seq ASC",
        )?;
        let mapped = stmt.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)))?;
        let mut v = Vec::new();
        for row in mapped { v.push(row?); }
        v
    };
    let mut repaired = 0usize;
    for (body, id) in rows {
        let event = Event::from_bytes(&body).context("decode event for reply repair")?;
        // The citing event's OWN channel scopes the lookup, exactly as it does at
        // derive time — repair must not create an edge derive would have refused.
        let cited = match &event.core.payload {
            EventPayload::MessagePosted { channel_id, reply_to, .. } => {
                reply_to.clone().map(|r| (*channel_id, r))
            }
            EventPayload::MessagePostedE2ee { channel_id, reply_to, .. } => {
                reply_to.clone().map(|r| (*channel_id, r))
            }
            _ => None,
        };
        let Some((cited_channel, cited)) = cited else { continue };
        if let Some(target_id) = resolve_reply_target(conn, cited_channel, Some(&cited))? {
            conn.execute(
                "UPDATE messages SET reply_to = ?2 WHERE id = ?1",
                params![id, target_id as i64],
            )?;
            repaired += 1;
        }
    }
    Ok(repaired)
}

/// The most raw events one fetch may return. A resync after a long absence is
/// paged with `since_accept_seq`, not served in one unbounded response.
pub const MAX_FETCH_EVENTS: usize = 500;

/// Raw signed-`Event` bytes for the MLS Welcomes addressed to `recipient`,
/// oldest-first, starting after `since_accept_seq`.
///
/// **`recipient` is the authenticated connection key at every call site** — the
/// request's own fields can narrow this result (by channel) but can never widen
/// it, so `FetchWelcomes` is not a "fetch anyone's Welcomes" oracle. The
/// recipient test is against the SIGNED payload's `for_member`, not against
/// anything the fetcher said.
///
/// The bytes are handed back opaque. The server stores and orders MLS traffic;
/// it does not interpret it, and a Welcome is useless to anyone but its holder.
///
/// Returns `(events, next_accept_seq, more)`. The cursor advances past every row
/// EXAMINED, not just the ones returned — a page that matched nothing still
/// makes progress, so a recipient whose Welcomes sit behind thousands of other
/// members' is not stuck re-scanning the same prefix forever.
pub fn fetch_welcomes_for(
    conn: &Connection,
    recipient: &PublicKey,
    channel_filter: Option<u64>,
    since_accept_seq: u64,
) -> Result<(Vec<Vec<u8>>, u64, bool)> {
    let mut stmt = conn.prepare(
        "SELECT accept_seq, event_body FROM events \
         WHERE payload_type = 'MlsWelcome' AND accept_seq > ?1 \
         ORDER BY accept_seq ASC",
    )?;
    let rows = stmt.query_map(params![since_accept_seq as i64], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    let mut out = Vec::new();
    let mut cursor = since_accept_seq;
    let mut more = false;
    let mut scanned = 0usize;
    for row in rows {
        let (seq, body) = row?;
        // Cap on ROWS SCANNED, not rows matched: bounding only the matches would
        // let one request walk the entire table when nothing matches.
        //
        // The bound is a COUNT of examined rows, never a distance in accept_seq.
        // A range cap (`seq > cursor + MAX`) breaks BEFORE `cursor = seq`
        // whenever the next matching row sits more than MAX positions past the
        // cursor -- i.e. behind a gap of rows this query's WHERE clause excludes,
        // which is the normal case on any busy server. It then returns
        // `more = true` with an UNMOVED cursor, and a client feeding
        // next_accept_seq back asks the identical question forever and never
        // reaches its event. Counting examined rows always advances the cursor
        // past what was examined, which is the invariant promised above.
        if out.len() >= MAX_FETCH_EVENTS || scanned >= MAX_FETCH_EVENTS {
            more = true;
            break;
        }
        scanned += 1;
        cursor = seq as u64;
        let event = Event::from_bytes(&body).context("decode welcome event")?;
        let EventPayload::MlsWelcome { channel_id, for_member, .. } = &event.core.payload else {
            continue;
        };
        if for_member.as_bytes() != recipient.as_bytes() {
            continue;
        }
        if let Some(want) = channel_filter {
            if *channel_id != want {
                continue;
            }
        }
        out.push(body);
    }
    Ok((out, cursor, more))
}

/// Raw signed-`Event` bytes for one channel's MLS control plane, oldest-first,
/// starting after `since_accept_seq`.
///
/// The control plane is the four channel-scoped MLS payloads a member needs to
/// advance (or rebuild) its group state when *another* member commits:
/// `MlsCommit`, `MlsWelcome`, `MlsLeafConfirmed` and `MlsGroupReset`.
/// `MlsKeyPackagePublished` is deliberately NOT here — it carries no
/// `channel_id` (it is server-scoped, published before any group exists) and is
/// already served by [`fetch_key_packages`].
///
/// `channel_id` is matched against the SIGNED payload, never against the
/// `events.channel_id` column: ingest only denormalizes that column for
/// `MessagePosted`, so for the MLS variants the body is the only source of
/// truth. The bytes are handed back opaque — the server stores and orders MLS
/// traffic, it does not interpret it.
///
/// Returns `(events, next_accept_seq, more)` with the SAME cursor semantics as
/// [`fetch_welcomes_for`]: the cursor advances past every row EXAMINED, not just
/// the ones returned, so a channel whose control events sit behind thousands of
/// other channels' is not stuck re-scanning the same prefix forever.
pub fn fetch_mls_control(
    conn: &Connection,
    channel_id: u64,
    since_accept_seq: u64,
) -> Result<(Vec<Vec<u8>>, u64, bool)> {
    let mut stmt = conn.prepare(
        "SELECT accept_seq, event_body FROM events \
         WHERE payload_type IN ('MlsCommit', 'MlsWelcome', 'MlsLeafConfirmed', 'MlsGroupReset') \
           AND accept_seq > ?1 \
         ORDER BY accept_seq ASC",
    )?;
    let rows = stmt.query_map(params![since_accept_seq as i64], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    let mut out = Vec::new();
    let mut cursor = since_accept_seq;
    let mut more = false;
    let mut scanned = 0usize;
    for row in rows {
        let (seq, body) = row?;
        // Cap on ROWS SCANNED, not rows matched: bounding only the matches would
        // let one request walk the entire table when nothing matches. Identical
        // to `fetch_welcomes_for`.
        //
        // The bound is a COUNT of examined rows, never a distance in accept_seq.
        // A range cap (`seq > cursor + MAX`) breaks BEFORE `cursor = seq`
        // whenever the next matching row sits more than MAX positions past the
        // cursor -- i.e. behind a gap of rows this query's WHERE clause excludes,
        // which is the normal case on any busy server. It then returns
        // `more = true` with an UNMOVED cursor, and a client feeding
        // next_accept_seq back asks the identical question forever and never
        // reaches its event. Counting examined rows always advances the cursor
        // past what was examined, which is the invariant promised above.
        if out.len() >= MAX_FETCH_EVENTS || scanned >= MAX_FETCH_EVENTS {
            more = true;
            break;
        }
        scanned += 1;
        cursor = seq as u64;
        let event = Event::from_bytes(&body).context("decode mls control event")?;
        let want = match &event.core.payload {
            EventPayload::MlsCommit { channel_id, .. }
            | EventPayload::MlsWelcome { channel_id, .. }
            | EventPayload::MlsLeafConfirmed { channel_id, .. }
            | EventPayload::MlsGroupReset { channel_id, .. } => *channel_id,
            _ => continue,
        };
        if want != channel_id {
            continue;
        }
        out.push(body);
    }
    Ok((out, cursor, more))
}

/// Raw signed-`Event` bytes for the KeyPackages one `(member, device)` published,
/// oldest-first.
///
/// KeyPackages are public by design — a committer must be able to fetch the
/// packages of a member it is adding — so this one really is keyed by the
/// request's fields. Membership gating still applies at the request layer:
/// public *within the server* is not public to the world.
///
/// Both `author` and `device` are matched against the columns the ingest path
/// denormalized from the SIGNED event core, not against anything the publisher
/// asserted separately.
pub fn fetch_key_packages(
    conn: &Connection,
    member: &PublicKey,
    device: &str,
) -> Result<Vec<Vec<u8>>> {
    let mut stmt = conn.prepare(
        "SELECT event_body FROM events \
         WHERE payload_type = 'MlsKeyPackagePublished' AND author = ?1 AND device = ?2 \
         ORDER BY accept_seq ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        params![member.as_bytes().as_slice(), device, MAX_FETCH_EVENTS as i64],
        |r| r.get::<_, Vec<u8>>(0),
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Raw signed-`Event` bytes for the `DeviceAuthorized` events of one identity,
/// oldest-first.
///
/// `DeviceAuthorized` carries a `DeviceCert` — the identity-signed
/// authorization of one device subkey. This is the ONLY production source of
/// the certs the receive-side leaf-binding gate (`verify_leaf_binding`)
/// verifies against, so the certs must be fetched from HERE (the log) and
/// **never** taken from the commit under validation.
///
/// Keyed by the SIGNED `author` column (== the identity), never by anything the
/// fetcher asserted. Membership gating still applies at the request layer:
/// public *within* the server is not public to the world.
///
/// Un-paginated, like [`fetch_key_packages`], and for the same reason: a
/// cert-per-device set is tiny (a device re-authorization is a rare, deliberate
/// act), so there is no long backlog to page through; `MAX_FETCH_EVENTS` is the
/// bound.
pub fn fetch_device_certs(
    conn: &Connection,
    identity: &PublicKey,
) -> Result<Vec<Vec<u8>>> {
    let mut stmt = conn.prepare(
        "SELECT event_body FROM events \
         WHERE payload_type = 'DeviceAuthorized' AND author = ?1 \
         ORDER BY accept_seq ASC LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        params![identity.as_bytes().as_slice(), MAX_FETCH_EVENTS as i64],
        |r| r.get::<_, Vec<u8>>(0),
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
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
    fn fetch_device_certs_returns_only_the_requested_identitys_certs() {
        let conn = crate::db::open_in_memory().unwrap();
        let alice = Keypair::generate();
        let alice_dev = Keypair::generate();
        let bob = Keypair::generate();
        let bob_dev = Keypair::generate();
        let g = genesis(&alice);
        save_genesis(&conn, &g).unwrap();

        // Alice and bob each authorize one device.
        let alice_da = Event::next(
            &alice_dev,
            alice.public_key(),
            g.server_id(),
            None,
            0,
            1,
            EP::DeviceAuthorized { cert: DeviceCert::create(&alice, &alice_dev.public_key(), 1) },
        );
        store_event(&conn, &alice_da).unwrap();
        let bob_da = Event::next(
            &bob_dev,
            bob.public_key(),
            g.server_id(),
            None,
            0,
            1,
            EP::DeviceAuthorized { cert: DeviceCert::create(&bob, &bob_dev.public_key(), 1) },
        );
        store_event(&conn, &bob_da).unwrap();

        // Alice's identity returns HER cert (and only hers).
        let alice_events = fetch_device_certs(&conn, &alice.public_key()).unwrap();
        assert_eq!(alice_events.len(), 1);
        let event = Event::from_bytes(&alice_events[0]).unwrap();
        let EP::DeviceAuthorized { cert } = &event.core.payload else {
            panic!("expected DeviceAuthorized");
        };
        assert_eq!(cert.core.identity, alice.public_key());
        assert_eq!(cert.core.device_pubkey, alice_dev.public_key());

        // Bob's identity returns his own; a stranger's returns nothing.
        let bob_events = fetch_device_certs(&conn, &bob.public_key()).unwrap();
        assert_eq!(bob_events.len(), 1);
        let stranger = Keypair::generate();
        assert!(fetch_device_certs(&conn, &stranger.public_key()).unwrap().is_empty());
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
        assert_eq!(reconcile_messages(&conn, None).unwrap(), 1);
        assert_eq!(reconcile_messages(&conn, None).unwrap(), 0);
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

    // -----------------------------------------------------------------------
    // Rung 2 — DERIVATION: sealed rows, FTS skip, reply mapping, sealed edits,
    // tombstones, rebuild parity (spec "Derivation" + F2 + F9 + rows 7a/7b).
    //
    // Every event below is folded by a REAL `LogState` before it is stored, in
    // the same order the `SubmitEvent` arm uses, so nothing here can pass on an
    // event the fold would have refused. Where ingest is supposed to refuse
    // something the FOLD accepts (target authorship — the fold keeps no
    // per-message index by design), the test proves the fold accepted it first.
    // -----------------------------------------------------------------------

    use farder_crypto::event_log::DeleteReason;

    /// Two LOG channels above the reserved floor: one sealed, one plaintext.
    /// The plaintext one exists because `MessageDeleted` is only valid in a
    /// channel the log knows, so a legacy channel cannot host the moderation
    /// half of these tests.
    const SEALED_CH: u64 = E2EE_CHANNEL_ID_FLOOR + 7;
    const PLAIN_CH: u64 = E2EE_CHANNEL_ID_FLOOR + 8;
    const BOOT_AUTH: [u8; 32] = [11u8; 32];
    const BOOT_TREE: [u8; 32] = [21u8; 32];
    const ADD_AUTH: [u8; 32] = [12u8; 32];
    const ADD_TREE: [u8; 32] = [22u8; 32];

    /// `(content, sealed, is_e2ee, edited_at, author)` — the five columns every
    /// assertion below reads off a derived row.
    type DerivedRow = (String, Option<Vec<u8>>, i64, Option<i64>, Vec<u8>);

    struct SealedFix {
        conn: Connection,
        st: LogState,
        sid: String,
        owner: Keypair,
        owner_dev: Keypair,
        prev: Event,
        alice: Option<(Keypair, Keypair, Event)>,
    }

    impl SealedFix {
        /// Owner + authorized device, a plaintext log channel, an E2EE log
        /// channel, and that channel's bootstrap commit (which confirms the
        /// creator's own leaf, so sealed content is actually foldable).
        fn new() -> Self {
            let conn = crate::db::open_in_memory().unwrap();
            let owner = Keypair::generate();
            let owner_dev = Keypair::generate();
            let g = genesis(&owner);
            save_genesis(&conn, &g).unwrap();
            let st = LogState::from_genesis(&g);
            let sid = g.server_id();
            let da = Event::next(
                &owner_dev, owner.public_key(), sid.clone(), None, 0, 100,
                EP::DeviceAuthorized { cert: DeviceCert::create(&owner, &owner_dev.public_key(), 1) },
            );
            let mut f = Self { conn, st, sid, owner, owner_dev, prev: da.clone(), alice: None };
            f.ingest(&da).expect("the owner's device authorizes");
            f.prev = da;
            f.own(EP::ChannelCreated {
                channel_id: PLAIN_CH, name: "town".into(), kind: "text".into(),
                class: ChannelClass::Plaintext, parent: None,
            });
            f.own(EP::ChannelCreated {
                channel_id: SEALED_CH, name: "sealed".into(), kind: "text".into(),
                class: ChannelClass::E2ee, parent: None,
            });
            f.own(EP::MlsCommit {
                channel_id: SEALED_CH, generation: 0, epoch: 0, mls_message: vec![0xC0],
                adds: vec![], removes: vec![],
                prev_epoch_authenticator: [0u8; 32], post_epoch_authenticator: BOOT_AUTH,
                post_tree_hash: BOOT_TREE, authz_head: "a".repeat(64),
                store_instance_hash: [3u8; 32],
            });
            f
        }

        fn sign_on(&self, dev: &Keypair, author: &Keypair, prev: &Event, payload: EP) -> Event {
            Event::next(dev, author.public_key(), self.sid.clone(), Some(prev),
                prev.core.lamport, 500, payload)
        }

        fn sign(&self, payload: EP) -> Event {
            self.sign_on(&self.owner_dev, &self.owner, &self.prev, payload)
        }

        /// EXACTLY the ingest transaction's sequence, in the same order, under
        /// one transaction — so a refusal leaves no trace, as in production.
        fn ingest(&mut self, e: &Event) -> Result<Option<u64>> {
            let mut trial = self.st.clone();
            trial.apply(e).context("the fold refused this event")?;
            let id = {
                let tx = self.conn.unchecked_transaction()?;
                store_event(&tx, e)?;
                materialize_channel_created(&tx, e)?;
                let id = derive_message_row(&tx, e)?;
                apply_sealed_edit(&tx, e)?;
                apply_tombstone(&tx, e)?;
                tx.commit()?;
                id
            };
            self.st = trial;
            Ok(id)
        }

        /// Sign on the OWNER's chain, ingest, advance the chain head.
        fn own(&mut self, payload: EP) -> (Event, Option<u64>) {
            let e = self.sign(payload);
            let id = self
                .ingest(&e)
                .unwrap_or_else(|err| panic!("owner event must ingest: {err:#}"));
            self.prev = e.clone();
            (e, id)
        }

        fn epoch(&self) -> u64 {
            self.st.mls_current_epoch(SEALED_CH).expect("the group exists").1
        }

        fn sealed_post(&mut self, ciphertext: &[u8], reply_to: Option<String>) -> (Event, u64) {
            let epoch = self.epoch();
            let (e, id) = self.own(EP::MessagePostedE2ee {
                channel_id: SEALED_CH, generation: 0, epoch,
                ciphertext: ciphertext.to_vec(), reply_to, attachments: vec![],
                authz_head: "a".repeat(64),
            });
            (e, id.expect("a sealed post derives a row"))
        }

        fn sealed_edit_of(&self, target: &str, ciphertext: &[u8]) -> EP {
            EP::MessageEditedE2ee {
                channel_id: SEALED_CH, target: target.to_string(), generation: 0,
                epoch: self.epoch(), ciphertext: ciphertext.to_vec(),
                authz_head: "a".repeat(64),
            }
        }

        /// Add a second full member (no capabilities, no MLS leaf).
        fn add_alice(&mut self) {
            let (inv, _) = self.own(EP::InviteCreated {
                code_hash: "c".repeat(64), max_uses: 10, expires_at: 9_999_999,
                requires_approval: false,
            });
            let alice = Keypair::generate();
            let alice_dev = Keypair::generate();
            let da = Event::next(
                &alice_dev, alice.public_key(), self.sid.clone(), None, 0, 100,
                EP::DeviceAuthorized { cert: DeviceCert::create(&alice, &alice_dev.public_key(), 1) },
            );
            self.ingest(&da).expect("alice's device authorizes");
            let join = self.sign_on(&alice_dev, &alice, &da,
                EP::MemberJoined { member: alice.public_key(), invite: inv.hash() });
            self.ingest(&join).expect("alice joins");
            self.alice = Some((alice, alice_dev, join));
        }

        /// Give alice a CONFIRMED MLS leaf, so she can author sealed content —
        /// the only way to reach ingest's target-authorship rule for edits (an
        /// unconfirmed author is stopped by the fold long before ingest).
        fn give_alice_a_leaf(&mut self) {
            let (alice, alice_dev, alice_prev) = self.alice.take().expect("add_alice first");
            let kp = self.sign_on(&alice_dev, &alice, &alice_prev,
                EP::MlsKeyPackagePublished {
                    key_package: vec![0xAB], store_instance_hash: [4u8; 32],
                    expires_at_log_pos: 1_000_000,
                });
            self.ingest(&kp).expect("alice publishes a key package");
            let epoch = self.epoch();
            self.own(EP::MlsCommit {
                channel_id: SEALED_CH, generation: 0, epoch, mls_message: vec![0xC1],
                adds: vec![farder_crypto::event_log::DeclaredAdd {
                    identity: alice.public_key(),
                    device: farder_crypto::event_log::device_id(&alice_dev.public_key()),
                    key_package: kp.hash(),
                }],
                removes: vec![],
                prev_epoch_authenticator: BOOT_AUTH, post_epoch_authenticator: ADD_AUTH,
                post_tree_hash: ADD_TREE, authz_head: "a".repeat(64),
                store_instance_hash: [3u8; 32],
            });
            let confirm = self.sign_on(&alice_dev, &alice, &kp, EP::MlsLeafConfirmed {
                channel_id: SEALED_CH, generation: 0, epoch: self.epoch(),
                tree_hash: ADD_TREE, store_instance_hash: [4u8; 32],
            });
            self.ingest(&confirm).expect("alice confirms her leaf");
            self.alice = Some((alice, alice_dev, confirm));
        }

        fn alice_signs(&self, payload: EP) -> Event {
            let (a, ad, prev) = self.alice.as_ref().expect("add_alice first");
            self.sign_on(ad, a, prev, payload)
        }

        fn row(&self, id: u64) -> Option<DerivedRow> {
            self.conn
                .query_row(
                    "SELECT content, sealed, is_e2ee, edited_at, author FROM messages WHERE id = ?1",
                    params![id as i64],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()
                .unwrap()
        }

        fn reply_of(&self, id: u64) -> Option<i64> {
            self.conn
                .query_row("SELECT reply_to FROM messages WHERE id = ?1", params![id as i64], |r| r.get(0))
                .unwrap()
        }

        fn message_count(&self) -> i64 {
            self.conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap()
        }
    }

    /// Every derived row, in an ID-INDEPENDENT shape: reply edges are compared
    /// as the target's `event_hash`, so a rebuild that re-numbers rows still
    /// compares equal. Ordered by `event_hash` for determinism.
    type ViewRow = (i64, Vec<u8>, String, i64, Option<i64>, Option<String>, Option<Vec<u8>>, i64, Option<String>);
    fn snapshot_view(conn: &Connection) -> Vec<ViewRow> {
        let mut stmt = conn
            .prepare(
                "SELECT m.channel_id, m.author, m.content, m.timestamp, m.edited_at, \
                        (SELECT r.event_hash FROM messages r WHERE r.id = m.reply_to), \
                        m.sealed, m.is_e2ee, m.event_hash \
                 FROM messages m ORDER BY m.event_hash",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                    r.get(6)?, r.get(7)?, r.get(8)?,
                ))
            })
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    #[test]
    fn a_sealed_post_derives_a_row_carrying_ciphertext_and_no_plaintext_column() {
        let mut f = SealedFix::new();
        // The "ciphertext" carries a readable needle on purpose: if any of this
        // path ever copied the payload into `content`, the assertions below fail
        // on the BYTES rather than on a mock's call count.
        let ciphertext = b"NEEDLEsealedalpha \x00\x01\x02 not really encrypted".to_vec();
        let (event, id) = f.sealed_post(&ciphertext, None);

        let (content, sealed, is_e2ee, edited_at, author) = f.row(id).expect("the row exists");
        assert_eq!(content, "", "a sealed row carries no plaintext column");
        assert_eq!(sealed.as_deref(), Some(ciphertext.as_slice()), "ciphertext stored verbatim");
        assert_eq!(is_e2ee, 1);
        assert_eq!(edited_at, None);
        assert_eq!(author.as_slice(), f.owner.public_key().as_bytes().as_slice());

        // The row is addressable by the event that produced it — the handle every
        // reply, edit and tombstone below uses.
        let (eh, ch, ts): (String, i64, i64) = f.conn
            .query_row(
                "SELECT event_hash, channel_id, timestamp FROM messages WHERE id = ?1",
                params![id as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(eh, event.hash());
        assert_eq!(ch as u64, SEALED_CH);
        assert_eq!(ts as u64, event.core.timestamp);

        // ...and the unique index makes that handle unambiguous: no second row
        // may ever carry the same event hash, whatever tries to write it. Two
        // rows for one event would make "the message this event derived"
        // ambiguous for reply mapping, sealed edits and tombstones alike.
        let other = crate::messages::insert_sealed_row(
            &f.conn, SEALED_CH, &f.owner.public_key(), b"other", 1, None, &"9".repeat(64),
        )
        .unwrap();
        let dup = f.conn.execute(
            "UPDATE messages SET event_hash = ?2 WHERE id = ?1",
            params![other as i64, event.hash()],
        );
        assert!(dup.is_err(), "event_hash must be unique across the derived view");
    }

    #[test]
    fn a_sealed_row_never_enters_the_fts_index() {
        let mut f = SealedFix::new();
        let needle = "topsecretneedle";
        let ciphertext = format!("{needle} pretending to be ciphertext").into_bytes();
        let (_, sealed_id) = f.sealed_post(&ciphertext, None);

        // Control: the SAME needle posted as plaintext through the log IS
        // indexed, so a negative result below means the skip worked rather than
        // that the fixture never indexes anything.
        let (_, plain_id) = f.own(EP::MessagePosted {
            channel_id: PLAIN_CH, content: format!("{needle} in the clear"),
            reply_to: None, attachments: vec![],
        });
        let plain_id = plain_id.unwrap();

        let hits: Vec<i64> = {
            let mut stmt = f.conn
                .prepare("SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1")
                .unwrap();
            let rows = stmt.query_map(params![needle], |r| r.get(0)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        assert_eq!(hits, vec![plain_id as i64], "only the plaintext control is indexed");

        // `search_messages` cannot surface it either — scoped or global — which
        // is the `AND is_e2ee = 0` belt behind the FTS-skip braces.
        let pk = f.owner.public_key();
        assert!(crate::messages::search_messages(&f.conn, needle, Some(SEALED_CH), 50, &pk).unwrap().is_empty());
        let global = crate::messages::search_messages(&f.conn, needle, None, 50, &pk).unwrap();
        assert_eq!(global.len(), 1);
        assert!(global.iter().all(|m| m.id != sealed_id));
    }

    #[test]
    fn reply_event_hash_maps_to_the_derived_row_id() {
        let mut f = SealedFix::new();
        // Sealed: E2EE channels are LOG-ONLY, so without this mapping every reply
        // in them is silently dropped (spec F9 / the MessageInput.tsx:283 TODO).
        let (parent, parent_id) = f.sealed_post(b"parent ciphertext", None);
        let (_, child_id) = f.sealed_post(b"child ciphertext", Some(parent.hash()));
        assert_eq!(f.reply_of(child_id), Some(parent_id as i64));

        // The same mapping on the plaintext log path: one helper, both arms.
        let (p, p_id) = f.own(EP::MessagePosted {
            channel_id: PLAIN_CH, content: "root".into(), reply_to: None, attachments: vec![],
        });
        let (_, c_id) = f.own(EP::MessagePosted {
            channel_id: PLAIN_CH, content: "answer".into(),
            reply_to: Some(p.hash()), attachments: vec![],
        });
        assert_eq!(f.reply_of(c_id.unwrap()), Some(p_id.unwrap() as i64));
    }

    #[test]
    fn an_unresolvable_reply_target_derives_null_and_is_repaired_by_reconcile() {
        let mut f = SealedFix::new();
        // The out-of-order case: the parent's EVENT is stored but its row was
        // never derived (a crash inside the ingest window). The child must still
        // land — losing the message to save the edge is the wrong trade.
        let epoch = f.epoch();
        let parent = f.sign(EP::MessagePostedE2ee {
            channel_id: SEALED_CH, generation: 0, epoch, ciphertext: b"parent".to_vec(),
            reply_to: None, attachments: vec![], authz_head: "a".repeat(64),
        });
        f.st.apply(&parent).expect("the fold accepts the parent");
        store_event(&f.conn, &parent).unwrap();
        f.prev = parent.clone();

        let (_, child_id) = f.sealed_post(b"child", Some(parent.hash()));
        assert_eq!(f.reply_of(child_id), None, "an unresolvable target derives NULL");
        assert_eq!(f.message_count(), 1, "the child landed anyway");

        // Reconcile derives the parent and repairs the edge — permanently lost
        // is exactly what the repair pass exists to prevent.
        let st = f.st.clone();
        assert_eq!(reconcile_messages(&f.conn, Some(&st)).unwrap(), 1);
        let parent_id: i64 = f.conn
            .query_row("SELECT id FROM messages WHERE event_hash = ?1", params![parent.hash()], |r| r.get(0))
            .unwrap();
        assert_eq!(f.reply_of(child_id), Some(parent_id));
        // Idempotent.
        assert_eq!(reconcile_messages(&f.conn, Some(&st)).unwrap(), 0);
        assert_eq!(repair_reply_links(&f.conn).unwrap(), 0);
    }

    #[test]
    fn a_sealed_edit_updates_the_row_in_place_and_only_for_its_own_author() {
        let mut f = SealedFix::new();
        f.add_alice();
        f.give_alice_a_leaf();
        let (post, id) = f.sealed_post(b"v1 ciphertext", None);
        let before = f.message_count();

        // The author's own edit: same row, new bytes, `edited_at` stamped, and
        // `content` still empty — an edit can never leak a body into the column.
        let e = f.sealed_edit_of(&post.hash(), b"v2 ciphertext ALPHA");
        f.own(e);
        let (content, sealed, is_e2ee, edited_at, _) = f.row(id).expect("row still there");
        assert_eq!(content, "");
        assert_eq!(sealed.as_deref(), Some(&b"v2 ciphertext ALPHA"[..]));
        assert_eq!(is_e2ee, 1);
        assert!(edited_at.is_some(), "an edit stamps edited_at");
        assert_eq!(f.message_count(), before, "an edit adds no row");

        // Alice holds a CONFIRMED leaf, so the fold authorizes her edit of
        // someone else's message — it keeps no per-message index by design.
        // Ingest is the only thing standing between that and a rewrite.
        let hostile = f.alice_signs(f.sealed_edit_of(&post.hash(), b"alice rewrote this"));
        f.st.clone().apply(&hostile).expect("the FOLD accepts it: authorship is ingest's job");
        let err = format!("{:#}", f.ingest(&hostile).unwrap_err());
        assert!(err.contains("only a message's own author may edit it"), "{err}");
        assert_eq!(
            f.row(id).unwrap().1.as_deref(),
            Some(&b"v2 ciphertext ALPHA"[..]),
            "the refused edit left the ciphertext byte-identical"
        );

        // An edit citing a target nobody derived is refused outright — that
        // refusal is what stops MessageEditedE2ee being a free write.
        let ghost = f.sign(f.sealed_edit_of(&"f".repeat(64), b"nowhere"));
        let err = format!("{:#}", f.ingest(&ghost).unwrap_err());
        assert!(err.contains("has not derived"), "{err}");
        assert!(
            f.conn.query_row("SELECT COUNT(*) FROM events WHERE event_hash = ?1",
                params![ghost.hash()], |r| r.get::<_, i64>(0)).unwrap() == 0,
            "a refused edit rolls the whole ingest transaction back"
        );
    }

    /// REGRESSION: an account-deletion request must not permanently disable
    /// log reconciliation.
    ///
    /// `anonymize_messages_by_author` rewrites `messages.author` on sealed rows
    /// too, so a member who ever edited a sealed message no longer matches their
    /// own rows afterwards. When the reconcile path treated that mismatch as an
    /// authorship VIOLATION it returned `Err`, aborting `reconcile_messages`
    /// before its reply-link repair — on that boot and every boot after it,
    /// forever, and silently, because `main.rs` swallows the error.
    ///
    /// This is the recurring shape where an over-strict guard creates a state
    /// the system can never leave. Reconcile now SKIPS the anonymized row: the
    /// edit was authorized when ingest accepted it, and re-judging settled
    /// history against mutated rows was never the check's job.
    #[test]
    fn anonymizing_an_author_does_not_permanently_break_reconcile() {
        let mut f = SealedFix::new();
        f.add_alice();
        f.give_alice_a_leaf();
        let (post, id) = f.sealed_post(b"v1", None);
        let e = f.sealed_edit_of(&post.hash(), b"v2 edited");
        f.own(e);

        // Reconcile is healthy before the deletion request.
        reconcile_messages(&f.conn, Some(&f.st)).expect("healthy before");

        // The author exercises data deletion (the path `retention.rs` runs).
        let author = f.owner.public_key();
        crate::messages::anonymize_messages_by_author(&f.conn, &author).unwrap();

        // ...and reconcile still works, on this boot and the next.
        reconcile_messages(&f.conn, Some(&f.st))
            .expect("a deletion request must not disable reconciliation");
        reconcile_messages(&f.conn, Some(&f.st)).expect("still working on the next boot");

        // The anonymized row kept its latest ciphertext: replay skipped it
        // rather than rolling it back to the pre-edit bytes.
        let (_, sealed, _, _, _) = f.row(id).expect("row survived");
        assert_eq!(sealed.as_deref(), Some(&b"v2 edited"[..]));

        // And INGEST is still strict: a live edit of someone else's message is
        // refused exactly as before. Relaxing reconcile must not relax the gate.
        let hostile = f.alice_signs(f.sealed_edit_of(&post.hash(), b"alice rewrote this"));
        let err = format!("{:#}", f.ingest(&hostile).unwrap_err());
        assert!(err.contains("only a message's own author may edit it"), "{err}");
    }

    #[test]
    fn a_deleted_message_stays_deleted_across_restart_and_reconcile() {
        let mut f = SealedFix::new();
        let (keep, keep_id) = f.sealed_post(b"survivor", None);
        let (doomed, doomed_id) = f.sealed_post(b"NEEDLEdoomed", None);

        f.own(EP::MessageDeleted {
            channel_id: SEALED_CH, target: doomed.hash(), reason: DeleteReason::Author,
        });
        assert!(f.row(doomed_id).is_none(), "the tombstone hard-deletes the row");
        assert!(f.row(keep_id).is_some());
        // The EVENT is still stored — which is exactly why reconcile would
        // resurrect it without the tombstone check.
        assert_eq!(
            f.conn.query_row("SELECT COUNT(*) FROM events WHERE event_hash = ?1",
                params![doomed.hash()], |r| r.get::<_, i64>(0)).unwrap(),
            1
        );

        // THE F2 INVARIANT: a restart re-runs reconcile, which must not re-derive it.
        let st = f.st.clone();
        assert!(st.is_tombstoned(&doomed.hash()), "the fold holds the tombstone");
        assert_eq!(reconcile_messages(&f.conn, Some(&st)).unwrap(), 0);
        assert!(f.row(doomed_id).is_none(), "content-blind delete must not undo itself");
        assert_eq!(f.message_count(), 1);

        // ...and it holds with NO fold in hand either: the stored MessageDeleted
        // events are the second, independent source (fail-closed union).
        assert_eq!(reconcile_messages(&f.conn, None).unwrap(), 0);
        assert_eq!(f.message_count(), 1);

        // Drift the other way — a restored snapshot in which the row is back
        // (written through the legitimate sealed door, so this is exactly the
        // row derivation would have produced). The tombstone is durable, so
        // reconcile sweeps it away again.
        crate::messages::insert_sealed_row(
            &f.conn, SEALED_CH, &f.owner.public_key(), b"NEEDLEdoomed", 1, None, &doomed.hash(),
        )
        .unwrap();
        assert_eq!(f.message_count(), 2);
        reconcile_messages(&f.conn, Some(&st)).unwrap();
        assert_eq!(f.message_count(), 1, "a resurrected row is swept back out");
        assert_eq!(
            f.conn.query_row("SELECT event_hash FROM messages", [], |r| r.get::<_, String>(0)).unwrap(),
            keep.hash()
        );
    }

    #[test]
    fn a_moderation_delete_needs_kick_and_an_author_delete_needs_authorship() {
        let mut f = SealedFix::new();
        f.add_alice();
        let alice_pk = f.alice.as_ref().unwrap().0.public_key();

        // Alice posts plaintext into the log's plaintext channel (she has no MLS
        // leaf, so sealed content is not open to her — deletion authority is the
        // subject here, and it is class-agnostic).
        let (alice_msg, alice_msg_id) = {
            let e = f.alice_signs(EP::MessagePosted {
                channel_id: PLAIN_CH, content: "alice speaks".into(),
                reply_to: None, attachments: vec![],
            });
            let id = f.ingest(&e).unwrap().unwrap();
            f.alice = f.alice.take().map(|(a, d, _)| (a, d, e.clone()));
            (e, id)
        };
        let (owner_msg, owner_msg_id) = f.own(EP::MessagePosted {
            channel_id: PLAIN_CH, content: "owner speaks".into(),
            reply_to: None, attachments: vec![],
        });
        let owner_msg_id = owner_msg_id.unwrap();

        // (1) MODERATION without `kick` — refused by the FOLD, before ingest.
        let no_kick = f.alice_signs(EP::MessageDeleted {
            channel_id: PLAIN_CH, target: owner_msg.hash(), reason: DeleteReason::Moderation,
        });
        let err = format!("{:#}", f.ingest(&no_kick).unwrap_err());
        assert!(err.contains("kick"), "{err}");
        assert!(f.row(owner_msg_id).is_some());

        // (2) AUTHOR-reason delete of someone else's message. The fold ACCEPTS
        // (alice is a member; verifying the claim needs the per-message index the
        // fold omits) — so this is ingest's rule, and only ingest's.
        let impostor = f.alice_signs(EP::MessageDeleted {
            channel_id: PLAIN_CH, target: owner_msg.hash(), reason: DeleteReason::Author,
        });
        f.st.clone().apply(&impostor).expect("the FOLD accepts it: authorship is ingest's job");
        let err = format!("{:#}", f.ingest(&impostor).unwrap_err());
        assert!(err.contains("must be authored by the message's author"), "{err}");
        assert!(f.row(owner_msg_id).is_some(), "the refused delete removed nothing");
        assert!(!f.st.is_tombstoned(&owner_msg.hash()), "and wrote no tombstone");

        // (3) Alice deleting her OWN message: accepted.
        let mine = f.alice_signs(EP::MessageDeleted {
            channel_id: PLAIN_CH, target: alice_msg.hash(), reason: DeleteReason::Author,
        });
        f.ingest(&mine).expect("an author may delete their own message");
        f.alice = f.alice.take().map(|(a, d, _)| (a, d, mine));
        assert!(f.row(alice_msg_id).is_none());

        // (4) The owner holds `kick` implicitly, so MODERATION over another
        // member's message is accepted — mods keep the mechanics, not the read.
        let (bystander, bystander_id) = f.own(EP::MessagePosted {
            channel_id: PLAIN_CH, content: "third".into(), reply_to: None, attachments: vec![],
        });
        let _ = bystander_id;
        let alice_pk_still_member = f.st.is_member(&alice_pk);
        assert!(alice_pk_still_member);
        f.own(EP::MessageDeleted {
            channel_id: PLAIN_CH, target: bystander.hash(), reason: DeleteReason::Moderation,
        });

        // (5) A tombstone citing a target nobody derived is refused outright —
        // that refusal is what BOUNDS the fold's tombstone set.
        let ghost = f.sign(EP::MessageDeleted {
            channel_id: PLAIN_CH, target: "e".repeat(64), reason: DeleteReason::Moderation,
        });
        let err = format!("{:#}", f.ingest(&ghost).unwrap_err());
        assert!(err.contains("has not derived"), "{err}");
        assert!(!f.st.is_tombstoned(&"e".repeat(64)));
    }

    #[test]
    fn derived_view_rebuild_from_events_equals_the_live_view() {
        let mut f = SealedFix::new();
        let (a, _) = f.sealed_post(b"A ciphertext", None);
        let (_b, b_id) = f.sealed_post(b"B ciphertext", Some(a.hash()));
        let (c, c_id) = f.sealed_post(b"C ciphertext", None);
        f.own(f.sealed_edit_of(&a.hash(), b"A ciphertext v2"));
        f.own(EP::MessagePosted {
            channel_id: PLAIN_CH, content: "plaintext too".into(), reply_to: None, attachments: vec![],
        });
        f.own(EP::MessageDeleted {
            channel_id: SEALED_CH, target: c.hash(), reason: DeleteReason::Author,
        });
        assert!(f.row(c_id).is_none());
        let live = snapshot_view(&f.conn);
        assert_eq!(live.len(), 3, "A (edited), B (reply), and the plaintext row");

        // Wipe the whole derived view and rebuild it from the events alone.
        f.conn.execute("DELETE FROM messages", []).unwrap();
        f.conn
            .execute("INSERT INTO messages_fts(messages_fts) VALUES('delete-all')", [])
            .unwrap();
        assert_eq!(f.message_count(), 0);
        let st = f.st.clone();
        assert_eq!(reconcile_messages(&f.conn, Some(&st)).unwrap(), 3);

        let rebuilt = snapshot_view(&f.conn);
        assert_eq!(rebuilt, live, "a from-events rebuild must equal the live view");
        // Spelled out, because each is a distinct way the rebuild could lie:
        assert!(
            rebuilt.iter().all(|r| r.8.as_deref() != Some(c.hash().as_str())),
            "the tombstoned message stays absent"
        );
        assert!(
            rebuilt.iter().any(|r| r.5.as_deref() == Some(a.hash().as_str())),
            "the reply edge is restored"
        );
        assert!(
            rebuilt.iter().any(|r| r.6.as_deref() == Some(&b"A ciphertext v2"[..])),
            "the EDITED ciphertext is rebuilt, not the original"
        );
        let _ = b_id;
    }

    #[test]
    fn retention_redaction_and_anonymize_operate_on_ciphertext_rows() {
        let mut f = SealedFix::new();
        // Two sealed rows at different timestamps + a plaintext control that
        // proves the FTS index is alive throughout.
        let epoch = f.epoch();
        let old = f.sign(EP::MessagePostedE2ee {
            channel_id: SEALED_CH, generation: 0, epoch, ciphertext: b"OLD ciphertext".to_vec(),
            reply_to: None, attachments: vec![], authz_head: "a".repeat(64),
        });
        // Hand-signed so the timestamp can be old; `own` fixes it at 500.
        let old = Event::next(&f.owner_dev, f.owner.public_key(), f.sid.clone(),
            Some(&f.prev), f.prev.core.lamport, 100, old.core.payload.clone());
        let old_id = f.ingest(&old).unwrap().unwrap();
        f.prev = old;
        let (_, fresh_id) = f.sealed_post(b"FRESH ciphertext", None);
        let (_, plain_id) = f.own(EP::MessagePosted {
            channel_id: PLAIN_CH, content: "searchable control".into(),
            reply_to: None, attachments: vec![],
        });
        let plain_id = plain_id.unwrap();
        let pk = f.owner.public_key();

        // --- RETENTION GC: blind on timestamp, works on ciphertext. ---
        assert_eq!(crate::messages::delete_messages_before(&f.conn, SEALED_CH, 200).unwrap(), 1);
        assert!(f.row(old_id).is_none());
        assert_eq!(f.row(fresh_id).unwrap().1.as_deref(), Some(&b"FRESH ciphertext"[..]));
        // The FTS index survived the sealed deletion (a 'delete' command issued
        // for a row that was never indexed is the corruption hazard this skips).
        assert_eq!(
            crate::messages::search_messages(&f.conn, "searchable", None, 50, &pk).unwrap().len(),
            1
        );

        // --- ANONYMIZE: the author sentinel lands on ciphertext rows too. ---
        assert_eq!(crate::messages::anonymize_messages_by_author(&f.conn, &pk).unwrap(), 2);
        let (content, sealed, _, _, author) = f.row(fresh_id).unwrap();
        assert_eq!(author.as_slice(), farder_protocol::server::DELETED_USER_KEY.as_slice());
        assert_eq!(content, "", "a sealed row keeps its empty content column");
        assert_eq!(sealed.as_deref(), Some(&b"FRESH ciphertext"[..]), "ciphertext untouched");
        let (plain_content, _, _, _, plain_author) = f.row(plain_id).unwrap();
        assert_eq!(plain_content, "[deleted]", "the plaintext body is still redacted");
        assert_eq!(plain_author.as_slice(), farder_protocol::server::DELETED_USER_KEY.as_slice());
        // The sealed row did NOT get pulled into the index by the re-index step.
        let indexed: Vec<i64> = {
            let mut stmt = f.conn
                .prepare("SELECT rowid FROM messages_fts WHERE messages_fts MATCH 'deleted'")
                .unwrap();
            let rows = stmt.query_map([], |r| r.get(0)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        assert_eq!(indexed, vec![plain_id as i64], "only the plaintext row is re-indexed");

        // --- REDACTION: keyed on the CONTENT HASH, so it never reads a row. ---
        insert_file(&f.conn, "sealedblob", 9, "application/octet-stream", &pk);
        assert!(crate::attachments::redact_blob(&f.conn, "/nonexistent-storage", "sealedblob", &pk).unwrap());
        let redacted: Option<Vec<u8>> = f.conn
            .query_row("SELECT redacted_by FROM files WHERE hash = 'sealedblob'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(redacted.as_deref(), Some(pk.as_bytes().as_slice()));
        assert_eq!(
            f.row(fresh_id).unwrap().1.as_deref(),
            Some(&b"FRESH ciphertext"[..]),
            "redaction touched no message row at all"
        );
    }

    /// Store one `MlsLeafConfirmed` control event for `channel_id`. `marker`
    /// lands in `epoch`/`generation`/`tree_hash`, which also makes every event's
    /// content hash unique across the test.
    fn store_control(
        conn: &Connection,
        dev: &Keypair,
        author: &farder_crypto::identity::PublicKey,
        g: &Genesis,
        channel_id: u64,
        marker: u64,
    ) -> Event {
        let ev = Event::next(
            dev,
            author.clone(),
            g.server_id(),
            None,
            0,
            marker,
            EP::MlsLeafConfirmed {
                channel_id,
                generation: marker,
                epoch: marker,
                tree_hash: [marker as u8; 32],
                store_instance_hash: [0u8; 32],
            },
        );
        store_event(conn, &ev).unwrap();
        ev
    }

    fn control_markers(events: &[Vec<u8>]) -> Vec<(u64, u64)> {
        events
            .iter()
            .map(|b| {
                let e = Event::from_bytes(b).unwrap();
                match e.core.payload {
                    EP::MlsLeafConfirmed { channel_id, epoch, .. } => (channel_id, epoch),
                    other => panic!("expected MlsLeafConfirmed, got {other:?}"),
                }
            })
            .collect()
    }

    #[test]
    fn fetch_mls_control_returns_the_channels_events_oldest_first() {
        let conn = crate::db::open_in_memory().unwrap();
        let owner = Keypair::generate();
        let dev = Keypair::generate();
        let g = genesis(&owner);
        let a = 100u64;
        let b = 200u64;
        // Interleave A and B control events; A's are 1, 3, 5 by accept order.
        store_control(&conn, &dev, &owner.public_key(), &g, a, 1);
        store_control(&conn, &dev, &owner.public_key(), &g, b, 2);
        store_control(&conn, &dev, &owner.public_key(), &g, a, 3);
        store_control(&conn, &dev, &owner.public_key(), &g, b, 4);
        store_control(&conn, &dev, &owner.public_key(), &g, a, 5);

        let (events, cursor, more) = fetch_mls_control(&conn, a, 0).unwrap();
        assert_eq!(control_markers(&events), vec![(a, 1), (a, 3), (a, 5)]);
        assert_eq!(cursor, 5, "the cursor is the last accept_seq examined");
        assert!(!more);
    }

    #[test]
    fn fetch_mls_control_pages_matching_rows_and_returns_more() {
        let conn = crate::db::open_in_memory().unwrap();
        let owner = Keypair::generate();
        let dev = Keypair::generate();
        let g = genesis(&owner);
        let a = 100u64;
        for marker in 1..=(MAX_FETCH_EVENTS as u64 + 1) {
            store_control(&conn, &dev, &owner.public_key(), &g, a, marker);
        }

        let (first, cursor, more) = fetch_mls_control(&conn, a, 0).unwrap();
        assert_eq!(first.len(), MAX_FETCH_EVENTS);
        assert!(more);
        assert_eq!(cursor, MAX_FETCH_EVENTS as u64);

        let (second, cursor2, more2) = fetch_mls_control(&conn, a, cursor).unwrap();
        assert_eq!(second.len(), 1, "feeding next_accept_seq back reaches the tail");
        assert_eq!(control_markers(&second), vec![(a, MAX_FETCH_EVENTS as u64 + 1)]);
        assert_eq!(cursor2, MAX_FETCH_EVENTS as u64 + 1);
        assert!(!more2);
    }

    #[test]
    fn fetch_mls_control_reaches_a_target_behind_many_non_matching_rows() {
        let conn = crate::db::open_in_memory().unwrap();
        let owner = Keypair::generate();
        let dev = Keypair::generate();
        let g = genesis(&owner);
        let a = 100u64;
        let b = 200u64;
        // 600 control events for channel B, then the ONE for channel A.
        for marker in 1..=600u64 {
            store_control(&conn, &dev, &owner.public_key(), &g, b, marker);
        }
        store_control(&conn, &dev, &owner.public_key(), &g, a, 7);

        // Drive the documented client loop. 601 rows exceed one page, so this
        // takes two -- the property under test is that the target IS reached and
        // that every `more` page strictly advances the cursor, never that a
        // single call swallows an unbounded number of rows.
        let mut cursor = 0u64;
        let mut seen = Vec::new();
        let mut pages = 0;
        loop {
            let (events, next, more) = fetch_mls_control(&conn, a, cursor).unwrap();
            seen.extend(control_markers(&events));
            pages += 1;
            assert!(pages <= 8, "pagination should converge, not grind");
            if !more {
                cursor = next;
                break;
            }
            assert!(next > cursor, "a `more` page must advance the cursor past what it examined");
            cursor = next;
        }
        assert_eq!(seen, vec![(a, 7)], "the target behind 600 non-matching rows is reachable");
        assert_eq!(cursor, 601, "the cursor ends past every row examined");
    }

    #[test]
    fn fetch_mls_control_advances_the_cursor_past_non_matching_rows_with_no_matches() {
        let conn = crate::db::open_in_memory().unwrap();
        let owner = Keypair::generate();
        let dev = Keypair::generate();
        let g = genesis(&owner);
        let a = 100u64;
        let b = 200u64;
        for marker in 1..=600u64 {
            store_control(&conn, &dev, &owner.public_key(), &g, b, marker);
        }

        // Nothing for A at all: every page is empty but the cursor still moves,
        // so the caller is not stuck re-scanning B's 600 rows from 0 forever.
        let mut cursor = 0u64;
        let mut pages = 0;
        loop {
            let (events, next, more) = fetch_mls_control(&conn, a, cursor).unwrap();
            assert!(events.is_empty());
            pages += 1;
            assert!(pages <= 8, "pagination should converge, not grind");
            if !more {
                cursor = next;
                break;
            }
            assert!(next > cursor, "an empty `more` page must still advance the cursor");
            cursor = next;
        }
        assert_eq!(cursor, 600, "the cursor ends past every row examined");
    }
    /// A control event sitting behind a GAP of rows the SQL filter excludes.
    ///
    /// The dense case (600 rows of the same payload family) advances the cursor
    /// one row at a time and never trips the range cap. The real world is the
    /// gap case: 600 ordinary events, then the control event at accept_seq 601.
    /// The range cap `seq > cursor + MAX_FETCH_EVENTS` then fires on the FIRST
    /// row and breaks BEFORE `cursor = seq`, so the page is empty, `more` is
    /// true, and the cursor has not moved. A client feeding `next_accept_seq`
    /// back asks the identical question forever and never reaches its event.
    #[test]
    fn a_more_page_always_advances_the_cursor_across_a_filtered_out_gap() {
        let conn = crate::db::open_in_memory().unwrap();
        let owner = Keypair::generate();
        let dev = Keypair::generate();
        let g = genesis(&owner);
        let a = 100u64;

        // Noise of a payload type the SQL filter excludes, so these rows are
        // never examined and cannot advance the cursor one-by-one.
        for marker in 1..=600u64 {
            let ev = Event::next(
                &dev,
                owner.public_key(),
                g.server_id(),
                None,
                0,
                marker,
                EP::DeviceRevoked { device: format!("{marker:064x}") },
            );
            store_event(&conn, &ev).unwrap();
        }
        store_control(&conn, &dev, &owner.public_key(), &g, a, 7);

        // Drive the documented client loop: feed next_accept_seq back while `more`.
        let mut cursor = 0u64;
        let mut seen = Vec::new();
        for _ in 0..16 {
            let (events, next, more) = fetch_mls_control(&conn, a, cursor).unwrap();
            seen.extend(control_markers(&events));
            assert!(
                next > cursor || !more,
                "a `more` page MUST advance the cursor or the client loops \
                 forever; it stayed at {cursor}"
            );
            cursor = next;
            if !more {
                break;
            }
        }
        assert_eq!(seen, vec![(a, 7)], "the event behind the gap must be reachable");
    }
}
