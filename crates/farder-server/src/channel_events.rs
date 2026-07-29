//! Event cards — RSVP widgets with a start time, a visible roster, a lead-time
//! reminder, an automatic "it's starting now" announcement and a cancel path.
//!
//! NAMING: the `events` table is the mesh signed log and `events.rs` is
//! `EventTarget`/`BroadcastEvent`, so the product's event cards live in
//! `channel_events` / `channel_event_rsvps` and in THIS module. The protocol and
//! client names stay product-facing (`EventInfo`, `GetEvent`, `EventUpdated`).
//!
//! One RSVP per member (`channel_event_rsvps` PK, upsert = change of mind);
//! `clear_rsvp` is a DELETE (the `polls::retract` idiom). Counts are full totals
//! computed live; the broadcast carries at most `ATTENDEE_NAME_CAP` display
//! NAMES per option — attendee public keys never leave the server.
//!
//! `starts_at` is an ABSOLUTE unix second: nothing timezone-shaped is stored or
//! transmitted, and every client renders it in its own local time. All
//! timestamps are unix seconds (`db::now()`), cast to `i64` at the rusqlite
//! boundary (the `polls.rs` convention).
//!
//! Sweeper crash-safety: `mark_reminded`, `mark_cancel_notified` and the
//! `status='upcoming'` flip inside `start_and_announce` are guarded UPDATEs whose
//! rows-affected decides whether the notification is produced at all — persist
//! before notify, at-most-once, never twice.

use anyhow::Result;
use farder_crypto::identity::PublicKey;
use farder_protocol::server::{EventInfo, MessageInfo};
use rusqlite::{params, Connection, OptionalExtension};

/// Max display names carried per RSVP option; the client renders "and N more"
/// from `count - names.len()`. Bounds the broadcast at 30 short strings.
pub const ATTENDEE_NAME_CAP: usize = 10;
pub const MAX_TITLE: usize = 120;
pub const MAX_LOCATION: usize = 120;
pub const MAX_DESCRIPTION: usize = 500;
/// `starts_at` must be at least this far ahead of `now`.
pub const MIN_LEAD_SECS: u64 = 60;
/// …and at most this far ahead.
pub const MAX_AHEAD_SECS: u64 = 365 * 86_400;
/// Allowed reminder leads, in seconds (15 minutes / 1 hour / 1 day).
pub const REMIND_LEADS: [u64; 3] = [900, 3_600, 86_400];
/// Max event rows one sweeper tick will pick up per pass — the sibling of
/// `reminders::REMINDER_DUE_BATCH` (same value, same reason). After downtime the
/// overdue backlog can be arbitrarily large, and every row in a pass costs a
/// write (plus, for the start pass, an announcement INSERT) while the caller
/// holds the single `state.db` mutex. Bounding the pass keeps one tick's
/// lock-hold bounded; the remainder drains on the next tick, and the guarded
/// UPDATEs (`reminded_at IS NULL`, `status='upcoming'`,
/// `cancel_notified_at IS NULL`) mean a row carried over can never fire twice.
pub const EVENT_DUE_BATCH: usize = 200;

pub const EVENT_USAGE: &str =
    "usage: /<trigger> Title | 3d [| location] [| description] [| remind 1h]";
/// The server cannot know the invoker's timezone, so a wall-clock string is
/// REFUSED rather than silently assumed to be UTC (the bug class that lands an
/// event 8 hours off). The builder — which does know the browser's zone — sends
/// an absolute `@<unix>`.
pub const WHEN_HINT: &str =
    "use the event builder for a date and time, or a relative time like `3d`";

// ---------------------------------------------------------------------------
// Parse (pure — unit-tested without any DB)
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub enum WhenSpec {
    /// Seconds from `now`.
    Relative(u64),
    /// Absolute unix seconds (what the builder emits).
    Absolute(u64),
}

#[derive(Debug, PartialEq)]
pub struct ParsedEvent {
    pub title: String,
    pub when: WhenSpec,
    pub location: Option<String>,
    pub description: Option<String>,
    pub remind_lead: Option<u64>,
}

/// `^remind\s+(15m|1h|1d|none)$` case-insensitive → `Some(lead)`, where the inner
/// `Option` is the lead itself (`none` → `Some(None)`). Anything else → `None`
/// (the segment is then a normal description/location).
fn remind_token(seg: &str) -> Option<Option<u64>> {
    let low = seg.to_ascii_lowercase();
    let rest = low.strip_prefix("remind")?;
    if !rest.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    match rest.trim() {
        "15m" => Some(Some(900)),
        "1h" => Some(Some(3_600)),
        "1d" => Some(Some(86_400)),
        "none" => Some(None),
        _ => None,
    }
}

/// `^(\d{1,4})([mhd])$` case-insensitive → seconds. Purely syntactic; the range
/// is enforced by `resolve_start` so an out-of-range value still gets the bounds
/// message rather than the usage string.
fn duration_token_secs(tok: &str) -> Option<u64> {
    if tok.len() < 2 {
        return None;
    }
    let (digits, unit) = tok.split_at(tok.len() - 1);
    if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    let mult = match unit.chars().next()?.to_ascii_lowercase() {
        'm' => 60,
        'h' => 3_600,
        'd' => 86_400,
        _ => return None,
    };
    Some(n * mult)
}

fn parse_when(seg: &str) -> Result<WhenSpec, String> {
    let trimmed = seg.trim();
    // `in 3d` is a synonym for `3d`.
    let body = {
        let low = trimmed.to_ascii_lowercase();
        if low.starts_with("in ") {
            trimmed[3..].trim()
        } else {
            trimmed
        }
    };
    if let Some(digits) = body.strip_prefix('@') {
        if (9..=12).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(secs) = digits.parse::<u64>() {
                return Ok(WhenSpec::Absolute(secs));
            }
        }
        return Err(WHEN_HINT.to_string());
    }
    match duration_token_secs(body) {
        Some(secs) => Ok(WhenSpec::Relative(secs)),
        None => Err(WHEN_HINT.to_string()),
    }
}

/// Shared field validation: creation and `EditEvent` MUST agree, so both call
/// this one function.
pub fn validate_event_fields(
    title: &str,
    description: Option<&str>,
    location: Option<&str>,
) -> Result<(), String> {
    if title.is_empty() || title.chars().count() > MAX_TITLE {
        return Err("title must be 1-120 characters".to_string());
    }
    if location.map_or(false, |l| l.chars().count() > MAX_LOCATION) {
        return Err("location must be at most 120 characters".to_string());
    }
    if description.map_or(false, |d| d.chars().count() > MAX_DESCRIPTION) {
        return Err("description must be at most 500 characters".to_string());
    }
    Ok(())
}

/// Parse `/event` args: `Title | <when> [| location] [| description] [| remind 1h]`.
/// Split on `|`, trim each segment (the `/poll` idiom). A final `remind …`
/// segment is ALWAYS consumed as the lead (deterministic, like the poll duration
/// rule); the remaining segments are strictly POSITIONAL — no guessing.
pub fn parse_event_args(args: &str) -> Result<ParsedEvent, String> {
    let mut segments: Vec<String> = args.split('|').map(|s| s.trim().to_string()).collect();

    let mut remind_lead: Option<u64> = None;
    if let Some(last) = segments.last() {
        if let Some(lead) = remind_token(last) {
            remind_lead = lead;
            segments.pop();
        }
    }

    if segments.len() < 2 || segments.len() > 4 {
        return Err(EVENT_USAGE.to_string());
    }
    let title = segments[0].clone();
    if segments[1].is_empty() {
        return Err(EVENT_USAGE.to_string());
    }
    let when = parse_when(&segments[1])?;
    // An empty third segment deliberately means "no location" (it is how you
    // reach the description slot without one).
    let location = segments.get(2).filter(|s| !s.is_empty()).cloned();
    let description = segments.get(3).filter(|s| !s.is_empty()).cloned();
    validate_event_fields(&title, description.as_deref(), location.as_deref())?;

    Ok(ParsedEvent { title, when, location, description, remind_lead })
}

/// Resolve a parsed `<when>` against `now` and apply the bounds. Pure.
pub fn resolve_start(when: &WhenSpec, now: u64) -> Result<u64, String> {
    let starts_at = match when {
        WhenSpec::Relative(d) => now.saturating_add(*d),
        WhenSpec::Absolute(t) => *t,
    };
    if starts_at < now.saturating_add(MIN_LEAD_SECS) {
        return Err("the event must start at least a minute from now".to_string());
    }
    if starts_at > now.saturating_add(MAX_AHEAD_SECS) {
        return Err("an event can be at most a year out".to_string());
    }
    Ok(starts_at)
}

// ---------------------------------------------------------------------------
// Rows / CRUD
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct EventRow {
    pub id: i64,
    pub channel_id: i64,
    pub message_id: i64,
    pub creator: PublicKey,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub starts_at: i64,
    pub remind_lead: Option<i64>,
    pub reminded_at: Option<i64>,
    pub status: String,
    pub started_at: Option<i64>,
    pub cancelled_at: Option<i64>,
    pub cancel_notified_at: Option<i64>,
    pub created_at: i64,
}

fn pk_from_blob(b: Vec<u8>) -> rusqlite::Result<PublicKey> {
    let arr: [u8; 32] = b.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(PublicKey::from_bytes(arr))
}

fn row_to_event(r: &rusqlite::Row) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        id: r.get(0)?,
        channel_id: r.get(1)?,
        message_id: r.get(2)?,
        creator: pk_from_blob(r.get::<_, Vec<u8>>(3)?)?,
        title: r.get(4)?,
        description: r.get(5)?,
        location: r.get(6)?,
        starts_at: r.get(7)?,
        remind_lead: r.get(8)?,
        reminded_at: r.get(9)?,
        status: r.get(10)?,
        started_at: r.get(11)?,
        cancelled_at: r.get(12)?,
        cancel_notified_at: r.get(13)?,
        created_at: r.get(14)?,
    })
}

const EVENT_SELECT: &str = "id, channel_id, message_id, creator, title, description, location, \
     starts_at, remind_lead, reminded_at, status, started_at, cancelled_at, cancel_notified_at, \
     created_at";

/// Insert an event row (status 'upcoming'). Returns the new event id.
pub fn create(
    conn: &Connection,
    channel_id: i64,
    message_id: i64,
    creator: &PublicKey,
    parsed: &ParsedEvent,
    starts_at: i64,
    now: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO channel_events \
         (channel_id, message_id, creator, title, description, location, starts_at, remind_lead, \
          status, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'upcoming', ?9)",
        params![
            channel_id,
            message_id,
            creator.as_bytes().as_slice(),
            parsed.title,
            parsed.description,
            parsed.location,
            starts_at,
            parsed.remind_lead.map(|l| l as i64),
            now,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<EventRow>> {
    conn.query_row(
        &format!("SELECT {EVENT_SELECT} FROM channel_events WHERE id = ?1"),
        params![id],
        row_to_event,
    )
    .optional()
    .map_err(Into::into)
}

/// Build the wire `EventInfo`: full counts per option, plus the first
/// `ATTENDEE_NAME_CAP` display names of each (RSVP order). Members who left the
/// roster are skipped in the name lists but still counted (the giveaway
/// `winner_name` precedent for name resolution).
pub fn build_info(conn: &Connection, row: &EventRow) -> Result<EventInfo> {
    let mut stmt = conn.prepare(
        "SELECT response, member FROM channel_event_rsvps WHERE event_id = ?1 \
         ORDER BY updated_at ASC, rowid ASC",
    )?;
    let rows = stmt.query_map(params![row.id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;

    let (mut going_count, mut maybe_count, mut no_count) = (0u32, 0u32, 0u32);
    let mut going_names: Vec<String> = Vec::new();
    let mut maybe_names: Vec<String> = Vec::new();
    let mut no_names: Vec<String> = Vec::new();
    for entry in rows {
        let (response, blob) = entry?;
        let (count, names) = match response.as_str() {
            "going" => (&mut going_count, &mut going_names),
            "maybe" => (&mut maybe_count, &mut maybe_names),
            "no" => (&mut no_count, &mut no_names),
            _ => continue, // never written by the server; defensive
        };
        *count += 1;
        if names.len() >= ATTENDEE_NAME_CAP {
            continue;
        }
        let arr: [u8; 32] = match blob.try_into() {
            Ok(a) => a,
            Err(_) => continue,
        };
        if let Some(m) = crate::members::get_member(conn, &PublicKey::from_bytes(arr))? {
            names.push(m.display_name);
        }
    }

    Ok(EventInfo {
        id: row.id,
        channel_id: row.channel_id as u64,
        message_id: row.message_id as u64,
        creator: row.creator.clone(),
        title: row.title.clone(),
        description: row.description.clone(),
        location: row.location.clone(),
        starts_at: row.starts_at as u64,
        remind_lead: row.remind_lead.map(|l| l as u64),
        status: row.status.clone(),
        going_count,
        maybe_count,
        no_count,
        going_names,
        maybe_names,
        no_names,
    })
}

/// Set or change one member's RSVP (upsert on the (event_id, member) PK).
pub fn rsvp(
    conn: &Connection,
    event_id: i64,
    member: &PublicKey,
    response: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO channel_event_rsvps (event_id, member, response, updated_at) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(event_id, member) DO UPDATE SET \
           response = excluded.response, updated_at = excluded.updated_at",
        params![event_id, member.as_bytes().as_slice(), response, now],
    )?;
    Ok(())
}

/// Remove the caller's RSVP. Returns whether one existed (rows-affected > 0).
pub fn clear_rsvp(conn: &Connection, event_id: i64, member: &PublicKey) -> Result<bool> {
    let n = conn.execute(
        "DELETE FROM channel_event_rsvps WHERE event_id = ?1 AND member = ?2",
        params![event_id, member.as_bytes().as_slice()],
    )?;
    Ok(n > 0)
}

pub fn my_rsvp(conn: &Connection, event_id: i64, member: &PublicKey) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT response FROM channel_event_rsvps WHERE event_id = ?1 AND member = ?2",
            params![event_id, member.as_bytes().as_slice()],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
}

/// Public keys of everyone whose RSVP is in `responses` (the DM audiences:
/// going+maybe for the lead reminder, going only for start/cancel).
///
/// DELIBERATELY UNCAPPED, unlike the `EVENT_DUE_BATCH`-bounded due lists. A
/// truncated due list is *deferred* work (the row keeps its guard column and is
/// picked up next tick); a truncated responder list is *dropped* work — the
/// attendees past the cut would silently never be told their event started, with
/// no state left behind to retry from. The list is naturally bounded by the
/// members who affirmatively RSVPed to one event, and the DMs it produces are
/// returned as data and sent after the db guard is dropped.
pub fn responders(
    conn: &Connection,
    event_id: i64,
    responses: &[&str],
) -> Result<Vec<PublicKey>> {
    if responses.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders =
        (0..responses.len()).map(|i| format!("?{}", i + 2)).collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT member FROM channel_event_rsvps WHERE event_id = ?1 AND response IN ({placeholders}) \
         ORDER BY updated_at ASC, rowid ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut binds: Vec<&dyn rusqlite::ToSql> = vec![&event_id];
    for r in responses {
        binds.push(r);
    }
    let rows = stmt.query_map(binds.as_slice(), |r| r.get::<_, Vec<u8>>(0))?;
    let mut out = Vec::new();
    for blob in rows {
        if let Ok(arr) = <[u8; 32]>::try_from(blob?) {
            out.push(PublicKey::from_bytes(arr));
        }
    }
    Ok(out)
}

/// Cancel an upcoming event (single-shot: only flips `status='upcoming'`).
pub fn cancel(conn: &Connection, event_id: i64, now: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE channel_events SET status = 'cancelled', cancelled_at = ?2 \
         WHERE id = ?1 AND status = 'upcoming'",
        params![event_id, now],
    )?;
    Ok(n > 0)
}

/// Full replace of the editable fields (upcoming only). `rearm_reminder` NULLs
/// `reminded_at` so a moved start time fires its lead DM again.
#[allow(clippy::too_many_arguments)]
pub fn edit(
    conn: &Connection,
    event_id: i64,
    title: &str,
    description: Option<&str>,
    location: Option<&str>,
    starts_at: i64,
    remind_lead: Option<i64>,
    rearm_reminder: bool,
) -> Result<()> {
    let sql = if rearm_reminder {
        "UPDATE channel_events SET title = ?2, description = ?3, location = ?4, starts_at = ?5, \
         remind_lead = ?6, reminded_at = NULL WHERE id = ?1 AND status = 'upcoming'"
    } else {
        "UPDATE channel_events SET title = ?2, description = ?3, location = ?4, starts_at = ?5, \
         remind_lead = ?6 WHERE id = ?1 AND status = 'upcoming'"
    };
    conn.execute(
        sql,
        params![event_id, title, description, location, starts_at, remind_lead],
    )?;
    Ok(())
}

/// Every list query goes through here: one prepared `EVENT_SELECT` + the given
/// tail clause. (Keeping the `Statement` in a named binding is also what makes
/// the borrow live long enough to drain the rows.)
fn query_rows<P: rusqlite::Params>(conn: &Connection, tail: &str, p: P) -> Result<Vec<EventRow>> {
    let mut stmt = conn.prepare(&format!("SELECT {EVENT_SELECT} FROM channel_events {tail}"))?;
    let rows = stmt.query_map(p, row_to_event)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Events whose lead moment has arrived but which have not started yet. The
/// `starts_at > ?1` clause means an event whose START also came due in the same
/// tick skips the lead DM and gets only the start DM — no double-ping.
/// Bounded to `EVENT_DUE_BATCH` (oldest first); the rest drains next tick.
pub fn list_reminder_due(conn: &Connection, now: i64) -> Result<Vec<EventRow>> {
    query_rows(
        conn,
        &format!(
            "WHERE status = 'upcoming' AND remind_lead IS NOT NULL AND reminded_at IS NULL \
               AND starts_at - remind_lead <= ?1 AND starts_at > ?1 \
             ORDER BY id ASC LIMIT {EVENT_DUE_BATCH}"
        ),
        params![now],
    )
}

/// Upcoming events whose start time has arrived (the sweeper's flip list).
/// Bounded to `EVENT_DUE_BATCH` (oldest first); the rest drains next tick.
pub fn list_start_due(conn: &Connection, now: i64) -> Result<Vec<EventRow>> {
    query_rows(
        conn,
        &format!(
            "WHERE status = 'upcoming' AND starts_at <= ?1 ORDER BY id ASC LIMIT {EVENT_DUE_BATCH}"
        ),
        params![now],
    )
}

/// Cancelled events whose attendees have not been told yet.
/// Bounded to `EVENT_DUE_BATCH` (oldest first); the rest drains next tick.
pub fn list_cancel_unnotified(conn: &Connection) -> Result<Vec<EventRow>> {
    query_rows(
        conn,
        &format!(
            "WHERE status = 'cancelled' AND cancel_notified_at IS NULL \
             ORDER BY id ASC LIMIT {EVENT_DUE_BATCH}"
        ),
        [],
    )
}

/// Single-shot guard for the lead-time DM batch. `false` ⇒ someone already sent
/// it; the caller must NOT then DM.
pub fn mark_reminded(conn: &Connection, id: i64, now: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE channel_events SET reminded_at = ?2 WHERE id = ?1 AND reminded_at IS NULL",
        params![id, now],
    )?;
    Ok(n > 0)
}

/// Single-shot guard for the cancellation DM batch.
pub fn mark_cancel_notified(conn: &Connection, id: i64, now: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE channel_events SET cancel_notified_at = ?2 \
         WHERE id = ?1 AND cancel_notified_at IS NULL",
        params![id, now],
    )?;
    Ok(n > 0)
}

/// Upcoming events of one channel, oldest-first (`id ASC` = creation order).
/// "Upcoming" is exact even before the sweeper ticks: a due-but-unswept event is
/// excluded (`starts_at > now`), matching the RSVP cutoff's exactness.
pub fn list_upcoming_in_channel(
    conn: &Connection,
    channel_id: i64,
    now: i64,
    limit: u32,
) -> Result<Vec<EventRow>> {
    query_rows(
        conn,
        "WHERE channel_id = ?1 AND status = 'upcoming' AND starts_at > ?2 ORDER BY id ASC LIMIT ?3",
        params![channel_id, now, limit as i64],
    )
}

// ---------------------------------------------------------------------------
// Creation (RunCommand `event` kind body — extracted so tests run it sync)
// ---------------------------------------------------------------------------

/// UTC wall-clock stamp for the old-client fallback line. `farder-server` has no
/// date-formatting dependency, so this borrows SQLite's formatter rather than
/// adding a crate; it falls back to the raw epoch string if the query ever fails.
fn utc_stamp(conn: &Connection, secs: i64) -> String {
    conn.query_row(
        "SELECT strftime('%Y-%m-%d %H:%M UTC', ?1, 'unixepoch')",
        params![secs],
        |r| r.get::<_, String>(0),
    )
    .unwrap_or_else(|_| format!("{secs}"))
}

/// Post an event card: one SQLite transaction inserting the fallback-content
/// message (PLAIN invoker authorship — no override, no badge), the event row and
/// the widget JSON cross-link. Returns the built message + event info for the
/// caller to broadcast AFTER the DB guard drops.
///
/// The start time is resolved (and bounds-checked) internally from `parsed.when`;
/// callers that want the bounds violation as a plain user-facing error should
/// call `resolve_start` first — it is pure and needs no lock.
pub fn create_event_card(
    conn: &mut Connection,
    channel_id: u64,
    invoker: &PublicKey,
    parsed: &ParsedEvent,
    now: u64,
) -> Result<(MessageInfo, EventInfo)> {
    let starts_at = resolve_start(&parsed.when, now).map_err(|e| anyhow::anyhow!(e))?;
    let tx = conn.transaction()?;
    let mut content = format!("📅 {} — {}", parsed.title, utc_stamp(&tx, starts_at as i64));
    if let Some(loc) = &parsed.location {
        content.push_str(&format!("\n📍 {loc}"));
    }
    if let Some(desc) = &parsed.description {
        content.push_str(&format!("\n{desc}"));
    }
    let mid = crate::messages::insert_message(&tx, channel_id, invoker, &content, None)?;
    let eid = create(
        &tx,
        channel_id as i64,
        mid as i64,
        invoker,
        parsed,
        starts_at as i64,
        now as i64,
    )?;
    crate::messages::set_widget(&tx, mid, &format!(r#"{{"type":"event","id":{eid}}}"#))?;
    let msg = crate::messages::get_message(&tx, mid, invoker)?
        .ok_or_else(|| anyhow::anyhow!("event card message vanished after insert"))?;
    let row =
        get(&tx, eid)?.ok_or_else(|| anyhow::anyhow!("event row vanished after insert"))?;
    let info = build_info(&tx, &row)?;
    tx.commit()?;
    Ok((msg, info))
}

/// Sweeper start pass: inside one transaction, flip `upcoming` → `started` under
/// the single-shot guard and insert the "starting now" announcement authored by
/// the SYSTEM identity, replying to the card so it threads under the event.
///
/// `Ok(None)` means the guard lost (a Cancel or a delete-hook won first) — the
/// transaction rolls back and NOTHING is announced. That guard is what makes the
/// announcement exactly-once across crashes and restarts.
pub fn start_and_announce(
    conn: &mut Connection,
    row: &EventRow,
    system_pk: &PublicKey,
    now: i64,
) -> Result<Option<(EventInfo, MessageInfo)>> {
    let tx = conn.transaction()?;
    let n = tx.execute(
        "UPDATE channel_events SET status = 'started', started_at = ?2 \
         WHERE id = ?1 AND status = 'upcoming'",
        params![row.id, now],
    )?;
    if n == 0 {
        return Ok(None); // tx drops → rollback; no announcement, ever
    }
    let mid = crate::messages::insert_message_with_author_name(
        &tx,
        row.channel_id as u64,
        system_pk,
        &format!("📅 {} is starting now!", row.title),
        Some(row.message_id as u64),
        Some("Events"),
        Some("BOT"),
    )?;
    let msg = crate::messages::get_message(&tx, mid, system_pk)?
        .ok_or_else(|| anyhow::anyhow!("event announcement vanished after insert"))?;
    let updated =
        get(&tx, row.id)?.ok_or_else(|| anyhow::anyhow!("event row vanished during start"))?;
    let info = build_info(&tx, &updated)?;
    tx.commit()?;
    Ok(Some((info, msg)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::identity::Keypair;

    // ------------------------------ parse ---------------------------------

    #[test]
    fn parse_positional_title_when_location_description() {
        let p = parse_event_args("Party at my house | 3d | my place | bring snacks").unwrap();
        assert_eq!(p.title, "Party at my house");
        assert_eq!(p.when, WhenSpec::Relative(3 * 86_400));
        assert_eq!(p.location.as_deref(), Some("my place"));
        assert_eq!(p.description.as_deref(), Some("bring snacks"));
        assert_eq!(p.remind_lead, None);
        // Title + when only is enough.
        let p = parse_event_args("Standup | 2h").unwrap();
        assert_eq!(p.location, None);
        assert_eq!(p.description, None);
        // Five segments (after lead removal) is not positional — refuse.
        assert_eq!(parse_event_args("T | 3d | l | d | extra").unwrap_err(), EVENT_USAGE);
        // Missing `when`.
        assert_eq!(parse_event_args("Just a title").unwrap_err(), EVENT_USAGE);
        assert_eq!(parse_event_args("Title | ").unwrap_err(), EVENT_USAGE);
    }

    #[test]
    fn parse_remind_final_segment_is_deterministic() {
        let p = parse_event_args("T | 3d | here | why | remind 1h").unwrap();
        assert_eq!(p.remind_lead, Some(3_600));
        assert_eq!(p.description.as_deref(), Some("why"));
        assert_eq!(parse_event_args("T | 3d | REMIND 15M").unwrap().remind_lead, Some(900));
        assert_eq!(parse_event_args("T | 3d | remind 1d").unwrap().remind_lead, Some(86_400));
        let none = parse_event_args("T | 3d | remind none").unwrap();
        assert_eq!(none.remind_lead, None);
        assert_eq!(none.location, None, "the remind segment is consumed, not a location");
        // A non-matching `remind …` stays a normal segment.
        let p = parse_event_args("T | 3d | remind me later").unwrap();
        assert_eq!(p.location.as_deref(), Some("remind me later"));
        assert_eq!(p.remind_lead, None);
    }

    #[test]
    fn parse_relative_forms() {
        assert_eq!(parse_event_args("T | 3d").unwrap().when, WhenSpec::Relative(3 * 86_400));
        assert_eq!(parse_event_args("T | in 3d").unwrap().when, WhenSpec::Relative(3 * 86_400));
        assert_eq!(parse_event_args("T | 2h").unwrap().when, WhenSpec::Relative(2 * 3_600));
        assert_eq!(parse_event_args("T | 90M").unwrap().when, WhenSpec::Relative(90 * 60));
    }

    #[test]
    fn parse_absolute_at_unix() {
        assert_eq!(
            parse_event_args("T | @1800000000").unwrap().when,
            WhenSpec::Absolute(1_800_000_000)
        );
        // Too few / too many digits is not an absolute stamp.
        assert_eq!(parse_event_args("T | @12345").unwrap_err(), WHEN_HINT);
    }

    #[test]
    fn parse_rejects_wall_clock_with_builder_hint() {
        assert_eq!(parse_event_args("T | 2026-08-01 20:00").unwrap_err(), WHEN_HINT);
        assert_eq!(parse_event_args("T | tomorrow 8pm").unwrap_err(), WHEN_HINT);
        assert_eq!(
            WHEN_HINT,
            "use the event builder for a date and time, or a relative time like `3d`"
        );
    }

    #[test]
    fn parse_empty_third_segment_means_no_location() {
        let p = parse_event_args("T | 3d |  | just a description").unwrap();
        assert_eq!(p.location, None);
        assert_eq!(p.description.as_deref(), Some("just a description"));
    }

    #[test]
    fn parse_rejects_over_length_title_location_description() {
        let long = "x".repeat(121);
        assert_eq!(
            parse_event_args(&format!("{long} | 3d")).unwrap_err(),
            "title must be 1-120 characters"
        );
        assert_eq!(parse_event_args(" | 3d").unwrap_err(), "title must be 1-120 characters");
        assert_eq!(
            parse_event_args(&format!("T | 3d | {long}")).unwrap_err(),
            "location must be at most 120 characters"
        );
        let long_d = "y".repeat(501);
        assert_eq!(
            parse_event_args(&format!("T | 3d | here | {long_d}")).unwrap_err(),
            "description must be at most 500 characters"
        );
        // Exact bounds are accepted.
        assert!(parse_event_args(&format!("{} | 3d | {} | {}", "x".repeat(120), "l".repeat(120), "d".repeat(500))).is_ok());
    }

    #[test]
    fn resolve_start_bounds() {
        let now = 1_000_000u64;
        assert_eq!(
            resolve_start(&WhenSpec::Relative(30), now).unwrap_err(),
            "the event must start at least a minute from now"
        );
        assert_eq!(resolve_start(&WhenSpec::Relative(60), now).unwrap(), now + 60);
        assert_eq!(
            resolve_start(&WhenSpec::Relative(400 * 86_400), now).unwrap_err(),
            "an event can be at most a year out"
        );
        assert_eq!(
            resolve_start(&WhenSpec::Absolute(now + MAX_AHEAD_SECS), now).unwrap(),
            now + MAX_AHEAD_SECS
        );
        assert_eq!(
            resolve_start(&WhenSpec::Absolute(now - 10), now).unwrap_err(),
            "the event must start at least a minute from now"
        );
    }

    // ------------------------------ module --------------------------------

    fn setup() -> (Connection, i64, PublicKey) {
        let conn = crate::db::open_in_memory().unwrap();
        let channel_id = crate::channels::create_channel(
            &conn,
            "general",
            farder_protocol::server::ChannelType::Text,
            None,
            0,
        )
        .unwrap();
        let pk = Keypair::generate().public_key();
        crate::members::register_member(&conn, &pk, "Alice").unwrap();
        (conn, channel_id as i64, pk)
    }

    fn member(conn: &Connection, name: &str) -> PublicKey {
        let pk = Keypair::generate().public_key();
        crate::members::register_member(conn, &pk, name).unwrap();
        pk
    }

    fn simple(when: WhenSpec) -> ParsedEvent {
        ParsedEvent {
            title: "Party".to_string(),
            when,
            location: None,
            description: None,
            remind_lead: None,
        }
    }

    #[test]
    fn create_event_card_links_message_event_and_widget() {
        let (mut conn, cid, pk) = setup();
        let parsed =
            parse_event_args("Party at my house | 3d | my place | snacks | remind 1h").unwrap();
        let now = 1_800_000_000u64;
        let (msg, info) = create_event_card(&mut conn, cid as u64, &pk, &parsed, now).unwrap();
        // Plain invoker authorship — no override, no badge.
        assert_eq!(msg.author, pk);
        assert_eq!(msg.author_name_override, None);
        assert_eq!(msg.author_badge, None);
        assert!(msg.content.contains("Party at my house"));
        assert!(msg.content.contains("📍 my place"));
        assert!(msg.content.contains("UTC"), "old-client fallback carries a stamp");
        // Cross-linked.
        assert_eq!(
            msg.widget.as_deref(),
            Some(format!(r#"{{"type":"event","id":{}}}"#, info.id).as_str())
        );
        assert_eq!(info.message_id, msg.id);
        assert_eq!(info.channel_id, cid as u64);
        assert_eq!(info.starts_at, now + 3 * 86_400);
        assert_eq!(info.remind_lead, Some(3_600));
        assert_eq!(info.status, "upcoming");
        assert_eq!(info.going_count, 0);
        let row = get(&conn, info.id).unwrap().unwrap();
        assert_eq!(row.message_id as u64, msg.id);
        assert_eq!(row.creator, pk);
    }

    #[test]
    fn rsvp_upsert_moves_member_between_options_without_changing_total() {
        let (conn, cid, pk) = setup();
        let id = create(&conn, cid, 1, &pk, &simple(WhenSpec::Relative(60)), 5_000, 0).unwrap();
        let row = get(&conn, id).unwrap().unwrap();
        rsvp(&conn, id, &pk, "going", 10).unwrap();
        let info = build_info(&conn, &row).unwrap();
        assert_eq!((info.going_count, info.maybe_count, info.no_count), (1, 0, 0));
        assert_eq!(info.going_names, vec!["Alice".to_string()]);
        assert_eq!(my_rsvp(&conn, id, &pk).unwrap().as_deref(), Some("going"));

        rsvp(&conn, id, &pk, "maybe", 20).unwrap();
        let info = build_info(&conn, &row).unwrap();
        assert_eq!((info.going_count, info.maybe_count, info.no_count), (0, 1, 0));
        assert_eq!(info.maybe_names, vec!["Alice".to_string()]);
        assert_eq!(my_rsvp(&conn, id, &pk).unwrap().as_deref(), Some("maybe"));
    }

    #[test]
    fn clear_rsvp_returns_false_when_none() {
        let (conn, cid, pk) = setup();
        let id = create(&conn, cid, 1, &pk, &simple(WhenSpec::Relative(60)), 5_000, 0).unwrap();
        assert!(!clear_rsvp(&conn, id, &pk).unwrap());
        rsvp(&conn, id, &pk, "no", 1).unwrap();
        assert!(clear_rsvp(&conn, id, &pk).unwrap());
        assert_eq!(my_rsvp(&conn, id, &pk).unwrap(), None);
    }

    #[test]
    fn build_info_counts_are_full_totals_while_names_cap_at_ten() {
        let (conn, cid, pk) = setup();
        let id = create(&conn, cid, 1, &pk, &simple(WhenSpec::Relative(60)), 5_000, 0).unwrap();
        let row = get(&conn, id).unwrap().unwrap();
        for i in 0..13 {
            let m = member(&conn, &format!("M{i}"));
            rsvp(&conn, id, &m, "going", i as i64).unwrap();
        }
        let info = build_info(&conn, &row).unwrap();
        assert_eq!(info.going_count, 13, "count is the FULL total");
        assert_eq!(info.going_names.len(), ATTENDEE_NAME_CAP, "names are capped");
        assert_eq!(info.going_names[0], "M0", "RSVP order");
    }

    #[test]
    fn responders_going_and_maybe_excludes_no() {
        let (conn, cid, pk) = setup();
        let id = create(&conn, cid, 1, &pk, &simple(WhenSpec::Relative(60)), 5_000, 0).unwrap();
        let going = member(&conn, "Going");
        let maybe = member(&conn, "Maybe");
        let no = member(&conn, "No");
        rsvp(&conn, id, &going, "going", 1).unwrap();
        rsvp(&conn, id, &maybe, "maybe", 2).unwrap();
        rsvp(&conn, id, &no, "no", 3).unwrap();

        let both = responders(&conn, id, &["going", "maybe"]).unwrap();
        assert_eq!(both, vec![going.clone(), maybe]);
        assert_eq!(responders(&conn, id, &["going"]).unwrap(), vec![going]);
        assert!(responders(&conn, id, &[]).unwrap().is_empty());
    }

    #[test]
    fn edit_changed_start_nulls_reminded_at_unchanged_does_not() {
        let (conn, cid, pk) = setup();
        let mut parsed = simple(WhenSpec::Relative(60));
        parsed.remind_lead = Some(900);
        let id = create(&conn, cid, 1, &pk, &parsed, 5_000, 0).unwrap();
        assert!(mark_reminded(&conn, id, 100).unwrap());
        assert!(!mark_reminded(&conn, id, 200).unwrap(), "single-shot");

        // Same start time → the guard stays armed as-is.
        edit(&conn, id, "T2", None, None, 5_000, Some(900), false).unwrap();
        let row = get(&conn, id).unwrap().unwrap();
        assert_eq!(row.title, "T2");
        assert_eq!(row.reminded_at, Some(100));

        // Moved start time → re-armed.
        edit(&conn, id, "T3", Some("d"), Some("l"), 9_000, Some(3_600), true).unwrap();
        let row = get(&conn, id).unwrap().unwrap();
        assert_eq!(row.reminded_at, None);
        assert_eq!(row.starts_at, 9_000);
        assert_eq!(row.remind_lead, Some(3_600));
        assert_eq!(row.description.as_deref(), Some("d"));
        assert_eq!(row.location.as_deref(), Some("l"));

        // A cancelled event is not editable.
        assert!(cancel(&conn, id, 500).unwrap());
        edit(&conn, id, "nope", None, None, 12_000, None, false).unwrap();
        assert_eq!(get(&conn, id).unwrap().unwrap().title, "T3");
    }

    #[test]
    fn list_upcoming_in_channel_excludes_started_cancelled_past_and_other_channels_id_asc_limit() {
        let (conn, cid, pk) = setup();
        let other = crate::channels::create_channel(
            &conn,
            "other",
            farder_protocol::server::ChannelType::Text,
            None,
            1,
        )
        .unwrap() as i64;
        let now = 1_000_000i64;
        let a = create(&conn, cid, 1, &pk, &simple(WhenSpec::Relative(60)), now + 100, 0).unwrap();
        let b = create(&conn, cid, 2, &pk, &simple(WhenSpec::Relative(60)), now + 200, 0).unwrap();
        let cancelled =
            create(&conn, cid, 3, &pk, &simple(WhenSpec::Relative(60)), now + 300, 0).unwrap();
        cancel(&conn, cancelled, now).unwrap();
        // Due but unswept: excluded (the cutoff is exact).
        let _past = create(&conn, cid, 4, &pk, &simple(WhenSpec::Relative(60)), now - 5, 0).unwrap();
        let elsewhere =
            create(&conn, other, 5, &pk, &simple(WhenSpec::Relative(60)), now + 100, 0).unwrap();

        let ids: Vec<i64> =
            list_upcoming_in_channel(&conn, cid, now, 20).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![a, b]);
        let ids: Vec<i64> =
            list_upcoming_in_channel(&conn, cid, now, 1).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![a], "limit keeps the oldest");
        let ids: Vec<i64> =
            list_upcoming_in_channel(&conn, other, now, 20).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![elsewhere]);
    }

    #[test]
    fn list_reminder_due_skips_events_whose_start_is_also_due() {
        let (conn, cid, pk) = setup();
        let now = 1_000_000i64;
        let mut parsed = simple(WhenSpec::Relative(60));
        parsed.remind_lead = Some(900);
        // Lead has arrived, start has not → due.
        let soon = create(&conn, cid, 1, &pk, &parsed, now + 300, 0).unwrap();
        // Start also arrived → no lead DM (the start pass handles it).
        let started_too = create(&conn, cid, 2, &pk, &parsed, now - 1, 0).unwrap();
        // No lead configured → never in this list.
        let _no_lead =
            create(&conn, cid, 3, &pk, &simple(WhenSpec::Relative(60)), now + 300, 0).unwrap();

        let ids: Vec<i64> = list_reminder_due(&conn, now).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![soon]);
        assert!(list_start_due(&conn, now).unwrap().iter().any(|r| r.id == started_too));
        // Marking flips it out of the list.
        assert!(mark_reminded(&conn, soon, now).unwrap());
        assert!(list_reminder_due(&conn, now).unwrap().is_empty());
    }

    /// The three sweeper feeds are bounded exactly like `reminders::list_due`:
    /// one tick takes at most `EVENT_DUE_BATCH` rows, oldest (`id ASC`) first.
    #[test]
    fn sweeper_due_lists_cap_at_event_due_batch_oldest_first() {
        let (conn, cid, pk) = setup();
        let now = 1_000_000i64;
        let over = EVENT_DUE_BATCH + 5;
        let mut lead = simple(WhenSpec::Relative(60));
        lead.remind_lead = Some(900);

        let mut start_due = Vec::new();
        let mut reminder_due = Vec::new();
        let mut cancelled = Vec::new();
        for i in 0..over {
            start_due.push(create(&conn, cid, 1, &pk, &simple(WhenSpec::Relative(60)), now - 5, 0).unwrap());
            // Lead reached, start still ahead.
            reminder_due.push(create(&conn, cid, 2, &pk, &lead, now + 300 + i as i64, 0).unwrap());
            let c = create(&conn, cid, 3, &pk, &simple(WhenSpec::Relative(60)), now + 9_000, 0).unwrap();
            cancel(&conn, c, now).unwrap();
            cancelled.push(c);
        }

        let starts = list_start_due(&conn, now).unwrap();
        assert_eq!(starts.len(), EVENT_DUE_BATCH, "start pass is batched");
        assert_eq!(starts[0].id, start_due[0], "oldest id first");
        assert_eq!(list_reminder_due(&conn, now).unwrap().len(), EVENT_DUE_BATCH, "lead pass is batched");
        assert_eq!(
            list_cancel_unnotified(&conn).unwrap().len(),
            EVENT_DUE_BATCH,
            "cancel-notify pass is batched"
        );
        assert!(starts.windows(2).all(|w| w[0].id < w[1].id), "id ASC");
        // The remainder is not lost — clearing the first batch's guards surfaces it.
        for id in starts.iter().map(|r| r.id) {
            conn.execute("UPDATE channel_events SET status = 'started' WHERE id = ?1", params![id])
                .unwrap();
        }
        assert_eq!(list_start_due(&conn, now).unwrap().len(), over - EVENT_DUE_BATCH);
    }

    #[test]
    fn start_and_announce_is_guarded_by_status() {
        let (mut conn, cid, pk) = setup();
        let now = 1_000_000i64;
        let id = create(&conn, cid, 1, &pk, &simple(WhenSpec::Relative(60)), now - 5, 0).unwrap();
        let system = crate::bots::get_or_create_system_identity(&conn).unwrap();
        let row = get(&conn, id).unwrap().unwrap();

        let (info, msg) = start_and_announce(&mut conn, &row, &system, now).unwrap().unwrap();
        assert_eq!(info.status, "started");
        assert_eq!(msg.author_badge.as_deref(), Some("BOT"));
        assert_eq!(msg.author_name_override.as_deref(), Some("Events"));
        assert_eq!(msg.reply_to, Some(row.message_id as u64));
        // A second attempt on the stale row announces nothing (single-shot).
        assert!(start_and_announce(&mut conn, &row, &system, now).unwrap().is_none());

        // A cancelled event can never announce.
        let cid2 = create(&conn, cid, 2, &pk, &simple(WhenSpec::Relative(60)), now - 5, 0).unwrap();
        let row2 = get(&conn, cid2).unwrap().unwrap();
        assert!(cancel(&conn, cid2, now).unwrap());
        assert!(start_and_announce(&mut conn, &row2, &system, now).unwrap().is_none());
    }

    #[test]
    fn cancel_and_notify_guards_are_single_shot() {
        let (conn, cid, pk) = setup();
        let id = create(&conn, cid, 1, &pk, &simple(WhenSpec::Relative(60)), 5_000, 0).unwrap();
        assert!(cancel(&conn, id, 100).unwrap());
        assert!(!cancel(&conn, id, 200).unwrap(), "second cancel is a no-op");
        let ids: Vec<i64> =
            list_cancel_unnotified(&conn).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![id]);
        assert!(mark_cancel_notified(&conn, id, 300).unwrap());
        assert!(!mark_cancel_notified(&conn, id, 400).unwrap());
        assert!(list_cancel_unnotified(&conn).unwrap().is_empty());
    }
}
