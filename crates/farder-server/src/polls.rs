//! Poll widgets — parse, CRUD, vote/retract/close, and the sweeper's `close_due`.
//!
//! One vote per member (`poll_votes` PK, upsert = re-vote); options are an
//! immutable JSON array TEXT column; counts are computed live via GROUP BY.
//! All timestamps are unix seconds (`db::now()` at the caller's boundary).

use anyhow::Result;
use farder_crypto::identity::PublicKey;
use farder_protocol::server::{MessageInfo, PollInfo};
use rusqlite::{params, Connection, OptionalExtension};

pub const POLL_USAGE: &str = "usage: /<trigger> Question | option A | option B [| 30m|2h|1d]";

const MIN_DURATION_SECS: u64 = 60; // 1m
const MAX_DURATION_SECS: u64 = 30 * 86_400; // 30d

// ---------------------------------------------------------------------------
// Parse
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ParsedPoll {
    pub question: String,
    pub options: Vec<String>,
    pub duration_secs: Option<u64>,
}

/// Syntactic duration token: `^(\d{1,4})(m|h|d)$` case-insensitive → seconds.
/// Purely syntactic — range enforcement happens in `parse_poll_args` so that a
/// matching-but-out-of-range token (e.g. `0m`, `31d`) is still consumed as a
/// duration and rejected with the range error (deterministic, never an option).
fn duration_token_secs(seg: &str) -> Option<u64> {
    if seg.len() < 2 {
        return None;
    }
    let (digits, unit) = seg.split_at(seg.len() - 1);
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

/// Parse `/poll` args: `Question | option A | option B [| 30m|2h|1d]`.
/// Split on `|`, trim; a final segment matching the duration token pattern is
/// ALWAYS the duration (1m–30d after conversion); then question 1–256 chars,
/// 2–10 options each 1–100 chars, case-insensitive dups and empty segments
/// rejected. Pure — unit-tested without any DB.
pub fn parse_poll_args(args: &str) -> Result<ParsedPoll, String> {
    let mut segments: Vec<String> = args.split('|').map(|s| s.trim().to_string()).collect();

    // Deterministic duration detection: the final segment, if it is a bare
    // duration token, is always consumed as the duration — never as an option.
    let mut duration_secs: Option<u64> = None;
    if let Some(last) = segments.last() {
        if let Some(secs) = duration_token_secs(last) {
            if !(MIN_DURATION_SECS..=MAX_DURATION_SECS).contains(&secs) {
                return Err("duration must be between 1m and 30d".to_string());
            }
            duration_secs = Some(secs);
            segments.pop();
        }
    }

    if segments.iter().any(|s| s.is_empty()) {
        return Err(POLL_USAGE.to_string());
    }
    if segments.len() < 3 {
        return Err(if segments.len() == 2 {
            "a poll needs 2-10 options".to_string()
        } else {
            POLL_USAGE.to_string()
        });
    }

    let question = segments.remove(0);
    if question.is_empty() || question.chars().count() > 256 {
        return Err("question must be 1-256 characters".to_string());
    }
    let options = segments;
    if options.len() < 2 || options.len() > 10 {
        return Err("a poll needs 2-10 options".to_string());
    }
    for opt in &options {
        if opt.is_empty() || opt.chars().count() > 100 {
            return Err("options must be 1-100 characters".to_string());
        }
    }
    let mut seen: Vec<String> = Vec::new();
    for opt in &options {
        let low = opt.to_lowercase();
        if seen.contains(&low) {
            return Err("duplicate options are not allowed".to_string());
        }
        seen.push(low);
    }

    Ok(ParsedPoll { question, options, duration_secs })
}

// ---------------------------------------------------------------------------
// Rows / CRUD
// ---------------------------------------------------------------------------

pub struct PollRow {
    pub id: i64,
    pub channel_id: i64,
    pub message_id: i64,
    pub creator: PublicKey,
    pub question: String,
    pub options: Vec<String>,
    pub created_at: i64,
    pub closes_at: Option<i64>,
    pub closed_at: Option<i64>,
}

fn row_to_poll(r: &rusqlite::Row) -> rusqlite::Result<PollRow> {
    let creator_b: Vec<u8> = r.get(3)?;
    let arr: [u8; 32] = creator_b
        .try_into()
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let options_json: String = r.get(5)?;
    let options: Vec<String> = serde_json::from_str(&options_json)
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(PollRow {
        id: r.get(0)?,
        channel_id: r.get(1)?,
        message_id: r.get(2)?,
        creator: PublicKey::from_bytes(arr),
        question: r.get(4)?,
        options,
        created_at: r.get(6)?,
        closes_at: r.get(7)?,
        closed_at: r.get(8)?,
    })
}

const POLL_SELECT: &str =
    "id, channel_id, message_id, creator, question, options, created_at, closes_at, closed_at";

/// Insert a poll row. Returns the new poll id.
pub fn create(
    conn: &Connection,
    channel_id: i64,
    message_id: i64,
    creator: &PublicKey,
    question: &str,
    options: &[String],
    closes_at: Option<i64>,
) -> Result<i64> {
    let options_json = serde_json::to_string(options)?;
    conn.execute(
        "INSERT INTO polls (channel_id, message_id, creator, question, options, created_at, closes_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            channel_id,
            message_id,
            creator.as_bytes().as_slice(),
            question,
            options_json,
            crate::db::now() as i64,
            closes_at,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<PollRow>> {
    conn.query_row(
        &format!("SELECT {POLL_SELECT} FROM polls WHERE id = ?1"),
        params![id],
        row_to_poll,
    )
    .optional()
    .map_err(Into::into)
}

/// Build the wire `PollInfo` for a row: counts via GROUP BY (aligned with
/// options), `closed = closed_at.is_some()`.
pub fn build_info(conn: &Connection, row: &PollRow) -> Result<PollInfo> {
    let mut counts = vec![0u32; row.options.len()];
    let mut stmt = conn.prepare(
        "SELECT option_index, COUNT(*) FROM poll_votes WHERE poll_id = ?1 GROUP BY option_index",
    )?;
    let pairs = stmt.query_map(params![row.id], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    })?;
    for pair in pairs {
        let (idx, n) = pair?;
        if let Some(slot) = counts.get_mut(idx as usize) {
            *slot = n as u32;
        }
    }
    let total_votes = counts.iter().sum();
    Ok(PollInfo {
        id: row.id,
        channel_id: row.channel_id as u64,
        message_id: row.message_id as u64,
        creator: row.creator.clone(),
        question: row.question.clone(),
        options: row.options.clone(),
        counts,
        total_votes,
        created_at: row.created_at as u64,
        closes_at: row.closes_at.map(|v| v as u64),
        closed: row.closed_at.is_some(),
    })
}

/// Cast or replace a vote (upsert on the (poll_id, voter) PK).
pub fn vote(conn: &Connection, poll_id: i64, voter: &PublicKey, option_index: u32) -> Result<()> {
    conn.execute(
        "INSERT INTO poll_votes (poll_id, voter, option_index, voted_at) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(poll_id, voter) DO UPDATE SET option_index = excluded.option_index, voted_at = excluded.voted_at",
        params![poll_id, voter.as_bytes().as_slice(), option_index as i64, crate::db::now() as i64],
    )?;
    Ok(())
}

/// Remove the caller's vote. Returns whether a vote existed (rows-affected > 0).
pub fn retract(conn: &Connection, poll_id: i64, voter: &PublicKey) -> Result<bool> {
    let n = conn.execute(
        "DELETE FROM poll_votes WHERE poll_id = ?1 AND voter = ?2",
        params![poll_id, voter.as_bytes().as_slice()],
    )?;
    Ok(n > 0)
}

/// Close a poll (idempotent — only flips an open poll).
pub fn close(conn: &Connection, poll_id: i64, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE polls SET closed_at = ?2 WHERE id = ?1 AND closed_at IS NULL",
        params![poll_id, now],
    )?;
    Ok(())
}

/// Sweeper poll half: persist `closed_at = now` on every due timed poll and
/// return their built infos (`closed: true`). Persist happens under the
/// caller's lock BEFORE any broadcast — a crash after this call never re-opens.
pub fn close_due(conn: &Connection, now: i64) -> Result<Vec<PollInfo>> {
    let mut due: Vec<PollRow> = Vec::new();
    {
        let mut stmt = conn.prepare(&format!(
            "SELECT {POLL_SELECT} FROM polls \
             WHERE closed_at IS NULL AND closes_at IS NOT NULL AND closes_at <= ?1"
        ))?;
        let rows = stmt.query_map(params![now], row_to_poll)?;
        for r in rows {
            due.push(r?);
        }
    }
    let mut out = Vec::with_capacity(due.len());
    for mut row in due {
        conn.execute(
            "UPDATE polls SET closed_at = ?2 WHERE id = ?1 AND closed_at IS NULL",
            params![row.id, now],
        )?;
        row.closed_at = Some(now);
        out.push(build_info(conn, &row)?);
    }
    Ok(out)
}

/// Open polls of one channel, oldest-first (`id ASC` = creation order,
/// AUTOINCREMENT). "Open" is exact even before the sweeper ticks: a past-
/// `closes_at`-but-unswept poll is excluded (`closes_at > now`), matching the
/// VotePoll closed-check exactness. Untimed open polls are included.
pub fn list_open_in_channel(
    conn: &Connection,
    channel_id: i64,
    now: i64,
    limit: u32,
) -> Result<Vec<PollRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {POLL_SELECT} FROM polls \
         WHERE channel_id = ?1 AND closed_at IS NULL \
           AND (closes_at IS NULL OR closes_at > ?2) \
         ORDER BY id ASC LIMIT ?3"
    ))?;
    let rows = stmt.query_map(params![channel_id, now, limit as i64], row_to_poll)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn my_vote(conn: &Connection, poll_id: i64, voter: &PublicKey) -> Result<Option<u32>> {
    Ok(conn
        .query_row(
            "SELECT option_index FROM poll_votes WHERE poll_id = ?1 AND voter = ?2",
            params![poll_id, voter.as_bytes().as_slice()],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .map(|v| v as u32))
}

// ---------------------------------------------------------------------------
// Creation (RunCommand `poll` kind body — extracted so tests run it sync)
// ---------------------------------------------------------------------------

/// Post a poll card: one SQLite transaction inserting the fallback-content
/// message (PLAIN invoker authorship — no override, no badge), the poll row,
/// and the widget JSON cross-link. Returns the built message + poll info for
/// the caller to broadcast AFTER the DB guard drops.
pub fn create_poll_card(
    conn: &mut Connection,
    channel_id: u64,
    invoker: &PublicKey,
    parsed: &ParsedPoll,
    now: u64,
) -> Result<(MessageInfo, PollInfo)> {
    let tx = conn.transaction()?;
    let mut content = format!("📊 Poll: {}", parsed.question);
    for opt in &parsed.options {
        content.push_str(&format!("\n- {opt}"));
    }
    let mid = crate::messages::insert_message(&tx, channel_id, invoker, &content, None)?;
    let poll_id = create(
        &tx,
        channel_id as i64,
        mid as i64,
        invoker,
        &parsed.question,
        &parsed.options,
        parsed.duration_secs.map(|d| (now + d) as i64),
    )?;
    crate::messages::set_widget(&tx, mid, &format!(r#"{{"type":"poll","id":{poll_id}}}"#))?;
    let msg = crate::messages::get_message(&tx, mid, invoker)?
        .ok_or_else(|| anyhow::anyhow!("poll card message vanished after insert"))?;
    let row = get(&tx, poll_id)?.ok_or_else(|| anyhow::anyhow!("poll row vanished after insert"))?;
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
    use farder_crypto::identity::Keypair;

    // ------------------------------ parse ---------------------------------

    fn opts(p: &ParsedPoll) -> Vec<&str> {
        p.options.iter().map(|s| s.as_str()).collect()
    }

    #[test]
    fn parse_happy_two_and_ten_options() {
        let p = parse_poll_args("Best pizza? | Margherita | Pepperoni").unwrap();
        assert_eq!(p.question, "Best pizza?");
        assert_eq!(opts(&p), vec!["Margherita", "Pepperoni"]);
        assert_eq!(p.duration_secs, None);

        let ten = (1..=10).map(|i| format!("opt{i}")).collect::<Vec<_>>().join(" | ");
        let p = parse_poll_args(&format!("Q | {ten}")).unwrap();
        assert_eq!(p.options.len(), 10);
    }

    #[test]
    fn parse_trims_segments() {
        let p = parse_poll_args("  Q?  |  a  |  b  ").unwrap();
        assert_eq!(p.question, "Q?");
        assert_eq!(opts(&p), vec!["a", "b"]);
    }

    #[test]
    fn parse_duration_forms_and_bounds() {
        assert_eq!(parse_poll_args("Q | a | b | 30m").unwrap().duration_secs, Some(30 * 60));
        assert_eq!(parse_poll_args("Q | a | b | 2H").unwrap().duration_secs, Some(2 * 3600));
        assert_eq!(parse_poll_args("Q | a | b | 7d").unwrap().duration_secs, Some(7 * 86400));
        // Inclusive bounds.
        assert_eq!(parse_poll_args("Q | a | b | 1m").unwrap().duration_secs, Some(60));
        assert_eq!(parse_poll_args("Q | a | b | 30d").unwrap().duration_secs, Some(30 * 86400));
        // Out of range — still consumed as duration, rejected.
        assert_eq!(
            parse_poll_args("Q | a | b | 0m").unwrap_err(),
            "duration must be between 1m and 30d"
        );
        assert_eq!(
            parse_poll_args("Q | a | b | 31d").unwrap_err(),
            "duration must be between 1m and 30d"
        );
    }

    #[test]
    fn parse_duration_is_deterministic() {
        // Final `2h` is ALWAYS the duration → one option left → error.
        assert!(parse_poll_args("q | 1h | 2h").is_err());
        // But a duration-looking middle segment is a normal option.
        let p = parse_poll_args("q | 1h | other | 2h").unwrap();
        assert_eq!(opts(&p), vec!["1h", "other"]);
        assert_eq!(p.duration_secs, Some(2 * 3600));
    }

    #[test]
    fn parse_option_count_bounds() {
        assert!(parse_poll_args("Q | only").is_err());
        let eleven = (1..=11).map(|i| format!("o{i}")).collect::<Vec<_>>().join(" | ");
        assert_eq!(
            parse_poll_args(&format!("Q | {eleven}")).unwrap_err(),
            "a poll needs 2-10 options"
        );
    }

    #[test]
    fn parse_rejects_case_insensitive_dups_and_empty_segments() {
        assert_eq!(
            parse_poll_args("Q | Yes | yes").unwrap_err(),
            "duplicate options are not allowed"
        );
        assert_eq!(parse_poll_args("Q | a || b").unwrap_err(), POLL_USAGE);
        assert_eq!(parse_poll_args("").unwrap_err(), POLL_USAGE);
    }

    #[test]
    fn parse_rejects_over_length() {
        let long_q = "q".repeat(257);
        assert_eq!(
            parse_poll_args(&format!("{long_q} | a | b")).unwrap_err(),
            "question must be 1-256 characters"
        );
        let long_o = "o".repeat(101);
        assert_eq!(
            parse_poll_args(&format!("Q | a | {long_o}")).unwrap_err(),
            "options must be 1-100 characters"
        );
    }

    // ------------------------------ module --------------------------------

    fn setup() -> (rusqlite::Connection, i64, PublicKey) {
        let conn = crate::db::open_in_memory().unwrap();
        let channel_id =
            crate::channels::create_channel(&conn, "general", farder_protocol::server::ChannelType::Text, None, 0)
                .unwrap();
        let pk = Keypair::generate().public_key();
        crate::members::register_member(&conn, &pk, "Alice").unwrap();
        (conn, channel_id as i64, pk)
    }

    fn two_opts() -> Vec<String> {
        vec!["a".to_string(), "b".to_string()]
    }

    #[test]
    fn create_and_build_info_counts() {
        let (conn, cid, pk) = setup();
        let id = create(&conn, cid, 1, &pk, "Q?", &two_opts(), None).unwrap();
        let row = get(&conn, id).unwrap().unwrap();
        assert_eq!(row.question, "Q?");
        assert_eq!(row.options, two_opts());
        assert_eq!(row.creator, pk);
        assert!(row.closed_at.is_none());

        let v1 = Keypair::generate().public_key();
        let v2 = Keypair::generate().public_key();
        vote(&conn, id, &v1, 0).unwrap();
        vote(&conn, id, &v2, 1).unwrap();
        let info = build_info(&conn, &row).unwrap();
        assert_eq!(info.counts, vec![1, 1]);
        assert_eq!(info.total_votes, 2);
        assert!(!info.closed);
    }

    #[test]
    fn vote_upsert_moves_counts_total_stable() {
        let (conn, cid, pk) = setup();
        let id = create(&conn, cid, 1, &pk, "Q?", &two_opts(), None).unwrap();
        let row = get(&conn, id).unwrap().unwrap();
        vote(&conn, id, &pk, 0).unwrap();
        assert_eq!(build_info(&conn, &row).unwrap().counts, vec![1, 0]);
        assert_eq!(my_vote(&conn, id, &pk).unwrap(), Some(0));
        // Re-vote replaces: counts move between indices, total stays 1.
        vote(&conn, id, &pk, 1).unwrap();
        let info = build_info(&conn, &row).unwrap();
        assert_eq!(info.counts, vec![0, 1]);
        assert_eq!(info.total_votes, 1);
        assert_eq!(my_vote(&conn, id, &pk).unwrap(), Some(1));
    }

    #[test]
    fn retract_false_on_no_vote() {
        let (conn, cid, pk) = setup();
        let id = create(&conn, cid, 1, &pk, "Q?", &two_opts(), None).unwrap();
        assert!(!retract(&conn, id, &pk).unwrap());
        vote(&conn, id, &pk, 0).unwrap();
        assert!(retract(&conn, id, &pk).unwrap());
        assert_eq!(my_vote(&conn, id, &pk).unwrap(), None);
    }

    #[test]
    fn close_due_closes_only_due_timed_polls_and_persists() {
        let (conn, cid, pk) = setup();
        let now = 1_000_000i64;
        let due = create(&conn, cid, 1, &pk, "due", &two_opts(), Some(now - 5)).unwrap();
        let future = create(&conn, cid, 2, &pk, "future", &two_opts(), Some(now + 500)).unwrap();
        let untimed = create(&conn, cid, 3, &pk, "untimed", &two_opts(), None).unwrap();
        let already = create(&conn, cid, 4, &pk, "already", &two_opts(), Some(now - 50)).unwrap();
        close(&conn, already, now - 40).unwrap();

        let infos = close_due(&conn, now).unwrap();
        assert_eq!(infos.len(), 1, "only the due open timed poll closes");
        assert_eq!(infos[0].id, due);
        assert!(infos[0].closed, "returned info reports closed: true");
        drop(infos); // rows persist closed even when the return is dropped
        assert!(get(&conn, due).unwrap().unwrap().closed_at.is_some());
        assert!(get(&conn, future).unwrap().unwrap().closed_at.is_none());
        assert!(get(&conn, untimed).unwrap().unwrap().closed_at.is_none());
        assert_eq!(get(&conn, already).unwrap().unwrap().closed_at, Some(now - 40));
        // Idempotent: a second sweep finds nothing.
        assert!(close_due(&conn, now).unwrap().is_empty());
    }

    #[test]
    fn list_open_in_channel_filters_orders_and_limits() {
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
        let untimed = create(&conn, cid, 1, &pk, "untimed", &two_opts(), None).unwrap();
        let future = create(&conn, cid, 2, &pk, "future", &two_opts(), Some(now + 500)).unwrap();
        let closed = create(&conn, cid, 3, &pk, "closed", &two_opts(), None).unwrap();
        close(&conn, closed, now - 1).unwrap();
        // Past closes_at but unswept: excluded (open-check is exact, like VotePoll).
        let _due_unswept = create(&conn, cid, 4, &pk, "due", &two_opts(), Some(now - 5)).unwrap();
        // closes_at == now boundary: excluded (strict `closes_at > now`).
        let _at_now = create(&conn, cid, 5, &pk, "at-now", &two_opts(), Some(now)).unwrap();
        let elsewhere = create(&conn, other, 6, &pk, "elsewhere", &two_opts(), None).unwrap();

        let ids: Vec<i64> = list_open_in_channel(&conn, cid, now, 20)
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, vec![untimed, future], "open-only, this channel only, id ASC");

        // Limit respected (keeps the oldest).
        let ids: Vec<i64> = list_open_in_channel(&conn, cid, now, 1)
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, vec![untimed]);

        // Other channel sees only its own poll.
        let ids: Vec<i64> = list_open_in_channel(&conn, other, now, 20)
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, vec![elsewhere]);
    }

    #[test]
    fn create_poll_card_links_message_poll_and_widget() {
        let (mut conn, cid, pk) = setup();
        let parsed = parse_poll_args("Best? | a | b | 2h").unwrap();
        let (msg, info) = create_poll_card(&mut conn, cid as u64, &pk, &parsed, 1_000).unwrap();
        // Plain invoker authorship — no override, no badge.
        assert_eq!(msg.author, pk);
        assert_eq!(msg.author_name_override, None);
        assert_eq!(msg.author_badge, None);
        assert!(msg.content.contains("Best?") && msg.content.contains("- a"));
        // Cross-linked.
        assert_eq!(msg.widget.as_deref(), Some(format!(r#"{{"type":"poll","id":{}}}"#, info.id).as_str()));
        assert_eq!(info.message_id, msg.id);
        assert_eq!(info.channel_id, cid as u64);
        assert_eq!(info.closes_at, Some(1_000 + 2 * 3600));
        let row = get(&conn, info.id).unwrap().unwrap();
        assert_eq!(row.message_id as u64, msg.id);
    }
}
