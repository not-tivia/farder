//! Personal reminders — private, per-owner nudges. Nothing is ever posted in a
//! channel: the only artifact is a DM from the server system identity
//! (`bots::send_system_dm`) when the reminder comes due.
//!
//! Ownership is structural: every read and every mutation is scoped by
//! `owner = ?` in SQL, and the owner is always the authenticated connection key
//! (no request carries an owner field). Delivery is at-most-once — the sweeper
//! flips `status` under a `status = 'pending'` guard BEFORE the DM is attempted,
//! so a crash can lose at most one nudge and can never duplicate one.
//!
//! All timestamps are unix seconds (`db::now()` at the caller's boundary), cast
//! to `i64` at the rusqlite boundary (the `polls.rs` convention).

use anyhow::Result;
use farder_crypto::identity::PublicKey;
use farder_protocol::server::ReminderInfo;
use rusqlite::{params, Connection};

/// Server-enforced upper bound on reminder text (the client mirrors it for UX).
pub const MAX_REMINDER_TEXT: usize = 500;
/// Per-user cap on outstanding (`pending`) reminders.
pub const MAX_PENDING_PER_USER: i64 = 20;
/// Bounded batch per sweeper tick; the remainder drains on the next tick.
pub const REMINDER_DUE_BATCH: usize = 200;

const MIN_DURATION_SECS: u64 = 60; // 1m
const MAX_DURATION_SECS: u64 = 30 * 86_400; // 30d

pub const REMINDER_USAGE: &str =
    "usage: /<trigger> <duration> <text> — e.g. /remind 90m take the pizza out (1m–30d)";

// ---------------------------------------------------------------------------
// Parse (pure — unit-tested without any DB)
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub struct ParsedReminder {
    pub delay_secs: u64,
    pub text: String,
}

/// Syntactic duration token: `^(\d{1,4})(m|h|d)$` case-insensitive → seconds.
/// Purely syntactic; range enforcement happens in `parse_reminder_args` so a
/// matching-but-out-of-range token (`0m`, `31d`) still yields the range error.
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

/// Parse `/remind` args: `<duration> <text>`. The grammar has no delimiter past
/// the first whitespace run, so the text is preserved VERBATIM (pipes and all).
pub fn parse_reminder_args(args: &str) -> Result<ParsedReminder, String> {
    let trimmed = args.trim();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let token = parts.next().unwrap_or("");
    let delay_secs = match duration_token_secs(token) {
        Some(s) => s,
        None => return Err(REMINDER_USAGE.to_string()),
    };
    if delay_secs < MIN_DURATION_SECS || delay_secs > MAX_DURATION_SECS {
        return Err("duration must be between 1m and 30d".to_string());
    }
    let text = parts.next().unwrap_or("").trim().to_string();
    if text.is_empty() {
        return Err(REMINDER_USAGE.to_string());
    }
    if text.chars().count() > MAX_REMINDER_TEXT {
        return Err("reminder text must be 1-500 characters".to_string());
    }
    Ok(ParsedReminder { delay_secs, text })
}

/// Human phrasing for a delay, used in the invoker-only `Notice` confirmation.
pub fn humanize_delay(secs: u64) -> String {
    if secs < 3_600 {
        let m = secs / 60;
        return format!("{m}m");
    }
    if secs < 86_400 {
        let h = secs / 3_600;
        let m = (secs % 3_600) / 60;
        return if m == 0 { format!("{h}h") } else { format!("{h}h {m}m") };
    }
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let days = if d == 1 { "1 day".to_string() } else { format!("{d} days") };
    if h == 0 {
        days
    } else {
        format!("{days} {h}h")
    }
}

// ---------------------------------------------------------------------------
// Rows + CRUD
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ReminderRow {
    pub id: i64,
    pub owner: PublicKey,
    pub channel_id: i64,
    pub text: String,
    pub created_at: i64,
    pub due_at: i64,
    pub status: String,
    pub sent_at: Option<i64>,
}

const REMINDER_SELECT: &str =
    "SELECT id, owner, channel_id, text, created_at, due_at, status, sent_at FROM reminders";

fn row_to_reminder(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReminderRow> {
    let owner_bytes: Vec<u8> = row.get(1)?;
    let arr: [u8; 32] = owner_bytes.try_into().map_err(|_| {
        rusqlite::Error::InvalidColumnType(1, "owner".into(), rusqlite::types::Type::Blob)
    })?;
    Ok(ReminderRow {
        id: row.get(0)?,
        owner: PublicKey::from_bytes(arr),
        channel_id: row.get(2)?,
        text: row.get(3)?,
        created_at: row.get(4)?,
        due_at: row.get(5)?,
        status: row.get(6)?,
        sent_at: row.get(7)?,
    })
}

/// Insert a pending reminder. Returns the new id.
pub fn create(
    conn: &Connection,
    owner: &PublicKey,
    channel_id: i64,
    text: &str,
    due_at: i64,
    now: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO reminders (owner, channel_id, text, created_at, due_at, status) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
        params![owner.as_bytes().as_slice(), channel_id, text, now, due_at],
    )?;
    Ok(conn.last_insert_rowid())
}

/// How many outstanding reminders this owner has (the per-user cap).
pub fn count_pending(conn: &Connection, owner: &PublicKey) -> Result<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM reminders WHERE owner = ?1 AND status = 'pending'",
        params![owner.as_bytes().as_slice()],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// The owner's pending reminders, soonest first. Owner-scoped in SQL — this can
/// never return another member's rows.
pub fn list_pending_for(conn: &Connection, owner: &PublicKey) -> Result<Vec<ReminderRow>> {
    let sql = format!(
        "{REMINDER_SELECT} WHERE owner = ?1 AND status = 'pending' ORDER BY due_at ASC LIMIT {}",
        MAX_PENDING_PER_USER
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![owner.as_bytes().as_slice()], row_to_reminder)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Cancel one of the owner's pending reminders. `false` when there was nothing
/// to cancel — a foreign id, an already-fired one and a nonexistent one are
/// indistinguishable by design (no oracle).
pub fn cancel(conn: &Connection, id: i64, owner: &PublicKey) -> Result<bool> {
    let n = conn.execute(
        "UPDATE reminders SET status = 'cancelled' \
         WHERE id = ?1 AND owner = ?2 AND status = 'pending'",
        params![id, owner.as_bytes().as_slice()],
    )?;
    Ok(n > 0)
}

/// Reminders that have come due, oldest first, bounded to `REMINDER_DUE_BATCH`.
pub fn list_due(conn: &Connection, now: i64) -> Result<Vec<ReminderRow>> {
    let sql = format!(
        "{REMINDER_SELECT} WHERE status = 'pending' AND due_at <= ?1 \
         ORDER BY due_at ASC LIMIT {REMINDER_DUE_BATCH}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![now], row_to_reminder)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Single-shot guard: flips `pending` → `sent`. `false` means someone else
/// already sent (or the owner cancelled) it — the caller must NOT then DM.
pub fn mark_sent(conn: &Connection, id: i64, now: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE reminders SET status = 'sent', sent_at = ?2 WHERE id = ?1 AND status = 'pending'",
        params![id, now],
    )?;
    Ok(n > 0)
}

pub fn to_info(row: &ReminderRow) -> ReminderInfo {
    ReminderInfo {
        id: row.id,
        text: row.text.clone(),
        due_at: row.due_at as u64,
        created_at: row.created_at as u64,
        channel_id: row.channel_id as u64,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::identity::Keypair;

    // --- pure parse ---------------------------------------------------------

    #[test]
    fn parse_happy_90m_taco() {
        let p = parse_reminder_args("90m take the pizza out").unwrap();
        assert_eq!(p.delay_secs, 90 * 60);
        assert_eq!(p.text, "take the pizza out");
    }

    #[test]
    fn parse_units_case_insensitive() {
        assert_eq!(parse_reminder_args("2H stretch").unwrap().delay_secs, 2 * 3_600);
        assert_eq!(parse_reminder_args("3D renew").unwrap().delay_secs, 3 * 86_400);
        assert_eq!(parse_reminder_args("15M stand up").unwrap().delay_secs, 15 * 60);
    }

    #[test]
    fn parse_rejects_zero_and_over_bounds() {
        assert_eq!(
            parse_reminder_args("0m nope").unwrap_err(),
            "duration must be between 1m and 30d"
        );
        assert_eq!(
            parse_reminder_args("31d nope").unwrap_err(),
            "duration must be between 1m and 30d"
        );
        // 30d exactly is in range.
        assert!(parse_reminder_args("30d ok").is_ok());
    }

    #[test]
    fn parse_rejects_garbage_and_missing_text() {
        assert_eq!(parse_reminder_args("banana x").unwrap_err(), REMINDER_USAGE);
        assert_eq!(parse_reminder_args("90m").unwrap_err(), REMINDER_USAGE);
        assert_eq!(parse_reminder_args("90m    ").unwrap_err(), REMINDER_USAGE);
        assert_eq!(parse_reminder_args("").unwrap_err(), REMINDER_USAGE);
    }

    #[test]
    fn parse_rejects_501_char_text() {
        let text = "a".repeat(501);
        assert_eq!(
            parse_reminder_args(&format!("5m {text}")).unwrap_err(),
            "reminder text must be 1-500 characters"
        );
        let ok = "a".repeat(500);
        assert!(parse_reminder_args(&format!("5m {ok}")).is_ok());
    }

    #[test]
    fn parse_preserves_pipes_verbatim() {
        let p = parse_reminder_args("5m a | b | c").unwrap();
        assert_eq!(p.text, "a | b | c");
    }

    #[test]
    fn humanize_delay_forms() {
        assert_eq!(humanize_delay(60), "1m");
        assert_eq!(humanize_delay(45 * 60), "45m");
        assert_eq!(humanize_delay(3_600), "1h");
        assert_eq!(humanize_delay(90 * 60), "1h 30m");
        assert_eq!(humanize_delay(86_400), "1 day");
        assert_eq!(humanize_delay(3 * 86_400), "3 days");
        assert_eq!(humanize_delay(3 * 86_400 + 4 * 3_600), "3 days 4h");
    }

    // --- DB -----------------------------------------------------------------

    fn owner() -> PublicKey {
        Keypair::generate().public_key()
    }

    #[test]
    fn create_and_count_pending_is_per_owner() {
        let conn = crate::db::open_in_memory().unwrap();
        let a = owner();
        let b = owner();
        create(&conn, &a, 1, "one", 100, 0).unwrap();
        create(&conn, &a, 1, "two", 200, 0).unwrap();
        create(&conn, &b, 1, "theirs", 300, 0).unwrap();
        assert_eq!(count_pending(&conn, &a).unwrap(), 2);
        assert_eq!(count_pending(&conn, &b).unwrap(), 1);
        // Cancelled rows stop counting against the cap.
        let id = create(&conn, &b, 1, "gone", 400, 0).unwrap();
        assert_eq!(count_pending(&conn, &b).unwrap(), 2);
        assert!(cancel(&conn, id, &b).unwrap());
        assert_eq!(count_pending(&conn, &b).unwrap(), 1);
    }

    #[test]
    fn list_pending_for_never_returns_another_owner() {
        let conn = crate::db::open_in_memory().unwrap();
        let a = owner();
        let b = owner();
        create(&conn, &a, 7, "mine-late", 900, 0).unwrap();
        create(&conn, &a, 7, "mine-soon", 100, 0).unwrap();
        create(&conn, &b, 7, "theirs", 50, 0).unwrap();

        let rows = list_pending_for(&conn, &a).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.owner == a));
        assert_eq!(rows[0].text, "mine-soon", "soonest first");
        assert_eq!(rows[1].text, "mine-late");
        // Info projection keeps the channel link-back.
        assert_eq!(to_info(&rows[0]).channel_id, 7);
    }

    #[test]
    fn cancel_guarded_by_owner() {
        let conn = crate::db::open_in_memory().unwrap();
        let a = owner();
        let b = owner();
        let id = create(&conn, &a, 1, "mine", 100, 0).unwrap();
        assert!(!cancel(&conn, id, &b).unwrap(), "foreign owner cannot cancel");
        let status: String = conn
            .query_row("SELECT status FROM reminders WHERE id = ?1", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "pending", "row untouched");
        assert!(cancel(&conn, id, &a).unwrap());
        assert!(!cancel(&conn, id, &a).unwrap(), "second cancel is a no-op");
        assert!(!cancel(&conn, 9_999, &a).unwrap(), "nonexistent id");
    }

    #[test]
    fn list_due_respects_status_due_at_and_batch_cap() {
        let conn = crate::db::open_in_memory().unwrap();
        let a = owner();
        let future = create(&conn, &a, 1, "later", 5_000, 0).unwrap();
        let cancelled = create(&conn, &a, 1, "cancelled", 10, 0).unwrap();
        cancel(&conn, cancelled, &a).unwrap();
        for i in 0..205 {
            create(&conn, &a, 1, &format!("due {i}"), 100 + i, 0).unwrap();
        }

        let due = list_due(&conn, 1_000).unwrap();
        assert_eq!(due.len(), REMINDER_DUE_BATCH, "batch cap");
        assert!(!due.iter().any(|r| r.id == future), "future excluded");
        assert!(!due.iter().any(|r| r.id == cancelled), "cancelled excluded");
        assert!(due.windows(2).all(|w| w[0].due_at <= w[1].due_at), "soonest first");
    }

    #[test]
    fn mark_sent_is_single_shot() {
        let conn = crate::db::open_in_memory().unwrap();
        let a = owner();
        let id = create(&conn, &a, 1, "ping", 10, 0).unwrap();
        assert!(mark_sent(&conn, id, 20).unwrap());
        assert!(!mark_sent(&conn, id, 30).unwrap(), "never fires twice");
        let (status, sent_at): (String, Option<i64>) = conn
            .query_row("SELECT status, sent_at FROM reminders WHERE id = ?1", params![id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(status, "sent");
        assert_eq!(sent_at, Some(20), "first send's timestamp is kept");
    }
}
