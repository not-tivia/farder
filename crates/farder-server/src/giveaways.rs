//! Giveaway widgets — parse, CRUD, enter/leave/cancel, and the sweeper's draw.
//!
//! One entry per member (`giveaway_entries` PK; `INSERT OR IGNORE` = idempotent
//! enter); `entry_count` is computed live via COUNT(*) — entrant identities never
//! leave the server in v1. The draw runs only server-side (`rand::thread_rng()`,
//! OS-seeded) under the `status='open'` guard: persist-then-broadcast means a
//! crash can never redraw. All timestamps are unix seconds (`db::now()`).

use anyhow::Result;
use farder_crypto::identity::{Keypair, PublicKey};
use farder_protocol::server::{GiveawayInfo, MessageInfo};
use rand::Rng;
use rusqlite::{params, Connection, OptionalExtension};

pub const GIVEAWAY_USAGE: &str =
    "usage: /<trigger> <duration> <prize> — duration 1m–30d (e.g. 30m, 24h, 7d)";

const MIN_DURATION_SECS: u64 = 60; // 1m
const MAX_DURATION_SECS: u64 = 30 * 86_400; // 30d

// ---------------------------------------------------------------------------
// Parse
// ---------------------------------------------------------------------------

/// `^([0-9]+)([mhd])$` case-insensitive → seconds, bounded 1m..=30d inclusive.
/// Anything else (`0m`, `31d`, `5w`, `banana`, empty) → `None`.
pub fn parse_giveaway_duration(s: &str) -> Option<u64> {
    if s.len() < 2 {
        return None;
    }
    let (digits, unit) = s.split_at(s.len() - 1);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    let mult = match unit.chars().next()?.to_ascii_lowercase() {
        'm' => 60,
        'h' => 3_600,
        'd' => 86_400,
        _ => return None,
    };
    let secs = n.checked_mul(mult)?;
    if !(MIN_DURATION_SECS..=MAX_DURATION_SECS).contains(&secs) {
        return None;
    }
    Some(secs)
}

/// Parse `/giveaway` args: `<duration> <prize>` → (duration_secs, prize).
/// Pure — unit-tested without any DB.
pub fn parse_giveaway_args(args: &str) -> Result<(u64, String), String> {
    let trimmed = args.trim();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let duration_secs = parse_giveaway_duration(parts.next().unwrap_or(""))
        .ok_or_else(|| GIVEAWAY_USAGE.to_string())?;
    let prize = parts.next().unwrap_or("").trim();
    if prize.is_empty() || prize.chars().count() > 200 {
        return Err("prize must be 1–200 characters".to_string());
    }
    Ok((duration_secs, prize.to_string()))
}

// ---------------------------------------------------------------------------
// Rows / CRUD
// ---------------------------------------------------------------------------

pub struct GiveawayRow {
    pub id: i64,
    pub channel_id: i64,
    pub message_id: i64,
    pub creator: PublicKey,
    pub prize: String,
    pub ends_at: i64,
    pub status: String,
    pub winner: Option<PublicKey>,
    pub created_at: i64,
}

fn pk_from_blob(b: Vec<u8>) -> rusqlite::Result<PublicKey> {
    let arr: [u8; 32] = b.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(PublicKey::from_bytes(arr))
}

fn row_to_giveaway(r: &rusqlite::Row) -> rusqlite::Result<GiveawayRow> {
    let creator = pk_from_blob(r.get::<_, Vec<u8>>(3)?)?;
    let winner = match r.get::<_, Option<Vec<u8>>>(7)? {
        Some(b) => Some(pk_from_blob(b)?),
        None => None,
    };
    Ok(GiveawayRow {
        id: r.get(0)?,
        channel_id: r.get(1)?,
        message_id: r.get(2)?,
        creator,
        prize: r.get(4)?,
        ends_at: r.get(5)?,
        status: r.get(6)?,
        winner,
        created_at: r.get(8)?,
    })
}

const GIVEAWAY_SELECT: &str =
    "id, channel_id, message_id, creator, prize, ends_at, status, winner, created_at";

/// Insert a giveaway row (status 'open'). Returns the new giveaway id.
pub fn create(
    conn: &Connection,
    channel_id: i64,
    message_id: i64,
    creator: &PublicKey,
    prize: &str,
    ends_at: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO giveaways (channel_id, message_id, creator, prize, ends_at, status, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6)",
        params![
            channel_id,
            message_id,
            creator.as_bytes().as_slice(),
            prize,
            ends_at,
            crate::db::now() as i64,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<GiveawayRow>> {
    conn.query_row(
        &format!("SELECT {GIVEAWAY_SELECT} FROM giveaways WHERE id = ?1"),
        params![id],
        row_to_giveaway,
    )
    .optional()
    .map_err(Into::into)
}

/// Build the wire `GiveawayInfo` for a row: `entry_count` via COUNT(*);
/// `winner_name` server-resolved via `members::get_member` (None when the winner
/// left the roster — clients fall back to the short key form).
pub fn build_info(conn: &Connection, row: &GiveawayRow) -> Result<GiveawayInfo> {
    let entry_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM giveaway_entries WHERE giveaway_id = ?1",
        params![row.id],
        |r| r.get(0),
    )?;
    let winner_name = match &row.winner {
        Some(pk) => crate::members::get_member(conn, pk)?.map(|m| m.display_name),
        None => None,
    };
    Ok(GiveawayInfo {
        id: row.id,
        channel_id: row.channel_id as u64,
        message_id: row.message_id as u64,
        creator: row.creator.clone(),
        prize: row.prize.clone(),
        ends_at: row.ends_at as u64,
        status: row.status.clone(),
        entry_count: entry_count as u32,
        winner: row.winner.clone(),
        winner_name,
    })
}

/// Enter (INSERT OR IGNORE — one entry per member). Returns whether a row was
/// actually inserted (false = already entered, idempotent no-op).
pub fn enter(conn: &Connection, giveaway_id: i64, member: &PublicKey, now: i64) -> Result<bool> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO giveaway_entries (giveaway_id, member, entered_at) VALUES (?1, ?2, ?3)",
        params![giveaway_id, member.as_bytes().as_slice(), now],
    )?;
    Ok(n > 0)
}

/// Withdraw an entry. Returns whether one existed (rows-affected > 0).
pub fn leave(conn: &Connection, giveaway_id: i64, member: &PublicKey) -> Result<bool> {
    let n = conn.execute(
        "DELETE FROM giveaway_entries WHERE giveaway_id = ?1 AND member = ?2",
        params![giveaway_id, member.as_bytes().as_slice()],
    )?;
    Ok(n > 0)
}

/// Void an open giveaway (single-shot: only flips `status='open'`).
pub fn cancel(conn: &Connection, giveaway_id: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE giveaways SET status = 'cancelled' WHERE id = ?1 AND status = 'open'",
        params![giveaway_id],
    )?;
    Ok(n > 0)
}

/// Open giveaways whose deadline has passed (the sweeper's work list).
pub fn list_due(conn: &Connection, now: i64) -> Result<Vec<GiveawayRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {GIVEAWAY_SELECT} FROM giveaways WHERE status = 'open' AND ends_at <= ?1"
    ))?;
    let rows = stmt.query_map(params![now], row_to_giveaway)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Update the winner of an already-ended giveaway (reroll — status stays 'ended').
pub fn reroll(conn: &Connection, giveaway_id: i64, winner: &PublicKey) -> Result<()> {
    conn.execute(
        "UPDATE giveaways SET winner = ?2 WHERE id = ?1 AND status = 'ended'",
        params![giveaway_id, winner.as_bytes().as_slice()],
    )?;
    Ok(())
}

pub fn my_entered(conn: &Connection, giveaway_id: i64, member: &PublicKey) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM giveaway_entries WHERE giveaway_id = ?1 AND member = ?2",
        params![giveaway_id, member.as_bytes().as_slice()],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

// ---------------------------------------------------------------------------
// Draw (sweeper core) + reroll draw
// ---------------------------------------------------------------------------

/// Entrants still eligible to win: member row exists and is neither banned nor
/// revoked (membership is re-checked at draw time, not entry time).
pub fn eligible_entrants(conn: &Connection, giveaway_id: i64) -> Result<Vec<PublicKey>> {
    let mut stmt = conn.prepare(
        "SELECT member FROM giveaway_entries WHERE giveaway_id = ?1 ORDER BY entered_at, member",
    )?;
    let blobs = stmt.query_map(params![giveaway_id], |r| r.get::<_, Vec<u8>>(0))?;
    let mut eligible = Vec::new();
    for b in blobs {
        let arr: [u8; 32] = match b?.try_into() {
            Ok(a) => a,
            Err(_) => continue, // malformed blob — never eligible
        };
        let pk = PublicKey::from_bytes(arr);
        if let Some(m) = crate::members::get_member(conn, &pk)? {
            if !m.banned && !m.revoked {
                eligible.push(pk);
            }
        }
    }
    Ok(eligible)
}

/// Winner's display name for announcement text: roster display name, falling
/// back to the short key form ("vk_" + first 8 hex chars).
fn display_name_for(conn: &Connection, pk: &PublicKey) -> String {
    match crate::members::get_member(conn, pk) {
        Ok(Some(m)) => m.display_name,
        _ => pk.to_string().chars().take(11).collect(),
    }
}

/// Insert a BOT-badged "Giveaway" announcement replying to the card. The author
/// is a freshly generated non-member throwaway key (secret discarded at insert —
/// webhook precedent): it can never authenticate and never appears in the roster.
fn insert_announcement(
    conn: &Connection,
    channel_id: u64,
    reply_to: u64,
    text: &str,
) -> Result<MessageInfo> {
    let announce_key = Keypair::generate().public_key();
    let mid = crate::messages::insert_message_with_author_name(
        conn,
        channel_id,
        &announce_key,
        text,
        Some(reply_to),
        Some("Giveaway"),
        Some("BOT"),
    )?;
    crate::messages::get_message(conn, mid, &announce_key)?
        .ok_or_else(|| anyhow::anyhow!("giveaway announcement vanished after insert"))
}

/// Sweeper draw: inside one transaction, pick a winner uniformly among still-
/// eligible entrants (None when empty), flip open→ended under the
/// `AND status='open'` single-shot guard, and insert the announcement message.
/// Everything commits BEFORE the caller broadcasts — a crash after commit never
/// redraws; a crash before commit leaves the row open for the next tick.
pub fn close_and_draw(conn: &Connection, row: &GiveawayRow) -> Result<(GiveawayInfo, MessageInfo)> {
    let tx = conn.unchecked_transaction()?;
    let eligible = eligible_entrants(&tx, row.id)?;
    let winner = if eligible.is_empty() {
        None
    } else {
        Some(eligible[rand::thread_rng().gen_range(0..eligible.len())].clone())
    };
    let n = tx.execute(
        "UPDATE giveaways SET status = 'ended', winner = ?2 WHERE id = ?1 AND status = 'open'",
        params![row.id, winner.as_ref().map(|w| w.as_bytes().to_vec())],
    )?;
    if n == 0 {
        // Lost the open→ended transition (e.g. concurrent cancel won the lock
        // first): nothing to draw. Transaction drops → rollback, no announcement.
        anyhow::bail!("giveaway {} is no longer open", row.id);
    }
    let text = match &winner {
        Some(w) => format!("🎉 {} won: {}", display_name_for(&tx, w), row.prize),
        None => format!("🎉 Giveaway ended — no entries: {}", row.prize),
    };
    let msg = insert_announcement(&tx, row.channel_id as u64, row.message_id as u64, &text)?;
    let updated = get(&tx, row.id)?
        .ok_or_else(|| anyhow::anyhow!("giveaway row vanished during draw"))?;
    let info = build_info(&tx, &updated)?;
    tx.commit()?;
    Ok((info, msg))
}

/// Reroll draw (handler core): recompute the eligible set; `Ok(None)` when it is
/// empty (previous winner stands, nothing written). Otherwise draw uniformly,
/// update the winner (status stays 'ended') and insert a fresh announcement —
/// one transaction, committed before the caller broadcasts.
pub fn reroll_and_announce(
    conn: &Connection,
    row: &GiveawayRow,
) -> Result<Option<(GiveawayInfo, MessageInfo)>> {
    let tx = conn.unchecked_transaction()?;
    let eligible = eligible_entrants(&tx, row.id)?;
    if eligible.is_empty() {
        return Ok(None); // tx drops → rollback (nothing was written)
    }
    let winner = eligible[rand::thread_rng().gen_range(0..eligible.len())].clone();
    reroll(&tx, row.id, &winner)?;
    let text = format!("🎉 Reroll — {} won: {}", display_name_for(&tx, &winner), row.prize);
    let msg = insert_announcement(&tx, row.channel_id as u64, row.message_id as u64, &text)?;
    let updated = get(&tx, row.id)?
        .ok_or_else(|| anyhow::anyhow!("giveaway row vanished during reroll"))?;
    let info = build_info(&tx, &updated)?;
    tx.commit()?;
    Ok(Some((info, msg)))
}

// ---------------------------------------------------------------------------
// Creation (RunCommand `giveaway` kind body — extracted so tests run it sync)
// ---------------------------------------------------------------------------

/// Render a unix-seconds timestamp as "YYYY-MM-DD HH:MM UTC" for the plain-text
/// fallback card (no chrono dep; civil-from-days algorithm).
fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hh, mm) = (rem / 3_600, (rem % 3_600) / 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02} UTC")
}

/// Post a giveaway card: one SQLite transaction inserting the fallback-content
/// message (PLAIN invoker authorship — no override, no badge), the giveaway row,
/// and the widget JSON cross-link. Returns the built message + giveaway info for
/// the caller to broadcast AFTER the DB guard drops. The MANAGE_SERVER gate is
/// the dispatch arm's job — this fn only creates.
pub fn create_giveaway_card(
    conn: &mut Connection,
    channel_id: u64,
    invoker: &PublicKey,
    prize: &str,
    duration_secs: u64,
    now: u64,
) -> Result<(MessageInfo, GiveawayInfo)> {
    let tx = conn.transaction()?;
    let ends_at = now + duration_secs;
    let content = format!("🎉 Giveaway: {prize} — ends {}", format_utc(ends_at));
    let mid = crate::messages::insert_message(&tx, channel_id, invoker, &content, None)?;
    let gid = create(&tx, channel_id as i64, mid as i64, invoker, prize, ends_at as i64)?;
    crate::messages::set_widget(&tx, mid, &format!(r#"{{"type":"giveaway","id":{gid}}}"#))?;
    let msg = crate::messages::get_message(&tx, mid, invoker)?
        .ok_or_else(|| anyhow::anyhow!("giveaway card message vanished after insert"))?;
    let row = get(&tx, gid)?
        .ok_or_else(|| anyhow::anyhow!("giveaway row vanished after insert"))?;
    let info = build_info(&tx, &row)?;
    tx.commit()?;
    Ok((msg, info))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------ parse ---------------------------------

    #[test]
    fn duration_parse_happy_and_case_insensitive() {
        assert_eq!(parse_giveaway_duration("30m"), Some(30 * 60));
        assert_eq!(parse_giveaway_duration("24h"), Some(24 * 3_600));
        assert_eq!(parse_giveaway_duration("7d"), Some(7 * 86_400));
        assert_eq!(parse_giveaway_duration("24H"), Some(24 * 3_600));
        assert_eq!(parse_giveaway_duration("7D"), Some(7 * 86_400));
    }

    #[test]
    fn duration_parse_bounds_inclusive() {
        assert_eq!(parse_giveaway_duration("1m"), Some(60));
        assert_eq!(parse_giveaway_duration("30d"), Some(30 * 86_400));
        assert_eq!(parse_giveaway_duration("0m"), None);
        assert_eq!(parse_giveaway_duration("31d"), None);
    }

    #[test]
    fn duration_parse_rejects_garbage() {
        assert_eq!(parse_giveaway_duration("5w"), None);
        assert_eq!(parse_giveaway_duration("banana"), None);
        assert_eq!(parse_giveaway_duration(""), None);
        assert_eq!(parse_giveaway_duration("m"), None);
        assert_eq!(parse_giveaway_duration("12"), None);
        assert_eq!(parse_giveaway_duration("1.5h"), None);
    }

    #[test]
    fn args_parse_duration_prize_split() {
        let (secs, prize) = parse_giveaway_args("24h Steam key").unwrap();
        assert_eq!(secs, 24 * 3_600);
        assert_eq!(prize, "Steam key");
        // Trimmed both sides; prize keeps internal whitespace.
        let (_, prize) = parse_giveaway_args("  30m   a big  prize  ").unwrap();
        assert_eq!(prize, "a big  prize");
    }

    #[test]
    fn args_parse_errors() {
        assert_eq!(parse_giveaway_args("banana prize").unwrap_err(), GIVEAWAY_USAGE);
        assert_eq!(parse_giveaway_args("").unwrap_err(), GIVEAWAY_USAGE);
        assert_eq!(parse_giveaway_args("0m prize").unwrap_err(), GIVEAWAY_USAGE);
        assert_eq!(
            parse_giveaway_args("24h").unwrap_err(),
            "prize must be 1–200 characters"
        );
        let long = "p".repeat(201);
        assert_eq!(
            parse_giveaway_args(&format!("24h {long}")).unwrap_err(),
            "prize must be 1–200 characters"
        );
    }

    // ------------------------------ module --------------------------------

    fn setup() -> (rusqlite::Connection, i64, PublicKey) {
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

    fn add_member(conn: &Connection, name: &str) -> PublicKey {
        let pk = Keypair::generate().public_key();
        crate::members::register_member(conn, &pk, name).unwrap();
        pk
    }

    #[test]
    fn enter_is_idempotent_one_row() {
        let (conn, cid, pk) = setup();
        let id = create(&conn, cid, 1, &pk, "prize", 10_000).unwrap();
        assert!(enter(&conn, id, &pk, 5).unwrap());
        assert!(!enter(&conn, id, &pk, 6).unwrap(), "double-enter inserts nothing");
        let row = get(&conn, id).unwrap().unwrap();
        assert_eq!(build_info(&conn, &row).unwrap().entry_count, 1);
        assert!(my_entered(&conn, id, &pk).unwrap());
    }

    #[test]
    fn leave_false_without_entry() {
        let (conn, cid, pk) = setup();
        let id = create(&conn, cid, 1, &pk, "prize", 10_000).unwrap();
        assert!(!leave(&conn, id, &pk).unwrap());
        enter(&conn, id, &pk, 5).unwrap();
        assert!(leave(&conn, id, &pk).unwrap());
        assert!(!my_entered(&conn, id, &pk).unwrap());
    }

    #[test]
    fn cancel_single_shot() {
        let (conn, cid, pk) = setup();
        let id = create(&conn, cid, 1, &pk, "prize", 10_000).unwrap();
        assert!(cancel(&conn, id).unwrap());
        assert_eq!(get(&conn, id).unwrap().unwrap().status, "cancelled");
        assert!(!cancel(&conn, id).unwrap(), "second cancel flips nothing");
    }

    #[test]
    fn list_due_respects_ends_at_and_status() {
        let (conn, cid, pk) = setup();
        let now = 1_000_000i64;
        let due = create(&conn, cid, 1, &pk, "due", now - 5).unwrap();
        let exact = create(&conn, cid, 2, &pk, "exact", now).unwrap();
        let future = create(&conn, cid, 3, &pk, "future", now + 500).unwrap();
        let cancelled = create(&conn, cid, 4, &pk, "cancelled", now - 50).unwrap();
        cancel(&conn, cancelled).unwrap();

        let ids: Vec<i64> = list_due(&conn, now).unwrap().iter().map(|r| r.id).collect();
        assert!(ids.contains(&due));
        assert!(ids.contains(&exact), "ends_at <= now is due");
        assert!(!ids.contains(&future));
        assert!(!ids.contains(&cancelled));
    }

    #[test]
    fn close_and_draw_no_entries() {
        let (conn, cid, pk) = setup();
        let id = create(&conn, cid, 1, &pk, "the prize", 10).unwrap();
        let row = get(&conn, id).unwrap().unwrap();
        let (info, msg) = close_and_draw(&conn, &row).unwrap();
        assert_eq!(info.status, "ended");
        assert_eq!(info.winner, None);
        assert_eq!(info.winner_name, None);
        assert!(msg.content.contains("no entries") && msg.content.contains("the prize"));
        let updated = get(&conn, id).unwrap().unwrap();
        assert_eq!(updated.status, "ended");
        assert!(updated.winner.is_none());
    }

    #[test]
    fn close_and_draw_never_picks_banned_or_revoked() {
        // Loop the draw N times on fresh giveaways: the excluded pks never win.
        let (conn, cid, creator) = setup();
        let good = add_member(&conn, "Good");
        let banned = add_member(&conn, "Banned");
        let revoked = add_member(&conn, "Revoked");
        crate::members::ban_member(&conn, &banned, None).unwrap();
        crate::members::revoke_member(&conn, &revoked).unwrap();
        for i in 0..25 {
            let id = create(&conn, cid, i + 10, &creator, "p", 10).unwrap();
            enter(&conn, id, &good, 1).unwrap();
            enter(&conn, id, &banned, 1).unwrap();
            enter(&conn, id, &revoked, 1).unwrap();
            let row = get(&conn, id).unwrap().unwrap();
            let (info, _msg) = close_and_draw(&conn, &row).unwrap();
            assert_eq!(info.winner.as_ref(), Some(&good), "only the eligible entrant can win");
            assert_eq!(info.winner_name.as_deref(), Some("Good"));
        }
    }

    #[test]
    fn close_and_draw_winner_among_entrants_and_announcement_shape() {
        let (conn, cid, creator) = setup();
        let a = add_member(&conn, "A");
        let b = add_member(&conn, "B");
        let id = create(&conn, cid, 42, &creator, "steam key", 10).unwrap();
        enter(&conn, id, &a, 1).unwrap();
        enter(&conn, id, &b, 1).unwrap();
        let row = get(&conn, id).unwrap().unwrap();
        let (info, msg) = close_and_draw(&conn, &row).unwrap();
        let winner = info.winner.clone().expect("winner drawn");
        assert!(winner == a || winner == b, "winner must be an entrant");
        // Announcement: BOT-badged "Giveaway" author that matches NO member row,
        // replying to the card.
        assert_eq!(msg.author_name_override.as_deref(), Some("Giveaway"));
        assert_eq!(msg.author_badge.as_deref(), Some("BOT"));
        assert!(crate::members::get_member(&conn, &msg.author).unwrap().is_none());
        assert_eq!(msg.reply_to, Some(42));
        assert!(msg.content.contains("won: steam key"));
    }

    #[test]
    fn close_and_draw_is_single_shot() {
        let (conn, cid, creator) = setup();
        let a = add_member(&conn, "A");
        let id = create(&conn, cid, 1, &creator, "p", 10).unwrap();
        enter(&conn, id, &a, 1).unwrap();
        let row = get(&conn, id).unwrap().unwrap();
        close_and_draw(&conn, &row).unwrap();
        let msgs_after_first: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        // Second pass over the same (stale) row errors on the status='open'
        // guard and inserts NOTHING (transaction rolls back).
        assert!(close_and_draw(&conn, &row).is_err());
        let msgs_after_second: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(msgs_after_first, msgs_after_second, "no second announcement");
        assert_eq!(get(&conn, id).unwrap().unwrap().status, "ended");
    }

    #[test]
    fn reroll_and_announce_empty_eligible_leaves_winner() {
        let (conn, cid, creator) = setup();
        let a = add_member(&conn, "A");
        let id = create(&conn, cid, 1, &creator, "p", 10).unwrap();
        enter(&conn, id, &a, 1).unwrap();
        let row = get(&conn, id).unwrap().unwrap();
        close_and_draw(&conn, &row).unwrap();
        // Sole entrant becomes ineligible → reroll finds nobody; winner stands.
        crate::members::ban_member(&conn, &a, None).unwrap();
        let ended = get(&conn, id).unwrap().unwrap();
        assert!(reroll_and_announce(&conn, &ended).unwrap().is_none());
        assert_eq!(get(&conn, id).unwrap().unwrap().winner, Some(a));
    }

    #[test]
    fn reroll_and_announce_draws_and_announces() {
        let (conn, cid, creator) = setup();
        let a = add_member(&conn, "A");
        let id = create(&conn, cid, 7, &creator, "prize!", 10).unwrap();
        enter(&conn, id, &a, 1).unwrap();
        let row = get(&conn, id).unwrap().unwrap();
        close_and_draw(&conn, &row).unwrap();
        let ended = get(&conn, id).unwrap().unwrap();
        let (info, msg) = reroll_and_announce(&conn, &ended).unwrap().expect("rerolled");
        assert_eq!(info.status, "ended");
        assert_eq!(info.winner, Some(a));
        assert!(msg.content.starts_with("🎉 Reroll — "));
        assert_eq!(msg.author_badge.as_deref(), Some("BOT"));
        assert_eq!(msg.reply_to, Some(7));
    }

    #[test]
    fn create_giveaway_card_links_message_giveaway_and_widget() {
        let (mut conn, cid, pk) = setup();
        let (msg, info) =
            create_giveaway_card(&mut conn, cid as u64, &pk, "Steam key", 24 * 3_600, 1_000_000)
                .unwrap();
        // Plain invoker authorship — no override, no badge.
        assert_eq!(msg.author, pk);
        assert_eq!(msg.author_name_override, None);
        assert_eq!(msg.author_badge, None);
        assert!(msg.content.contains("Giveaway: Steam key"));
        // Cross-linked.
        assert_eq!(
            msg.widget.as_deref(),
            Some(format!(r#"{{"type":"giveaway","id":{}}}"#, info.id).as_str())
        );
        assert_eq!(info.message_id, msg.id);
        assert_eq!(info.channel_id, cid as u64);
        assert_eq!(info.ends_at, 1_000_000 + 24 * 3_600);
        assert_eq!(info.status, "open");
        assert_eq!(info.entry_count, 0);
        let row = get(&conn, info.id).unwrap().unwrap();
        assert_eq!(row.message_id as u64, msg.id);
    }

    #[test]
    fn format_utc_renders_civil_date() {
        // 2026-07-27 00:00:00 UTC = 1785110400.
        assert_eq!(super::format_utc(1_785_110_400), "2026-07-27 00:00 UTC");
        assert_eq!(super::format_utc(0), "1970-01-01 00:00 UTC");
    }
}
