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

/// A deliberate act by someone with power: kick, ban, role change, channel
/// delete. Rare, and kept forever — "who did this to my server, and when" has
/// no expiry date.
pub const CATEGORY_MODERATION: &str = "moderation";

/// Who came and went: sessions, server joins, voice channels. Written by the
/// ordinary use of the server rather than by an act of authority, so it arrives
/// in volume and is pruned on a window (see [`prune_activity`]).
pub const CATEGORY_ACTIVITY: &str = "activity";

/// Insert a moderation audit row and return the populated AuditEvent struct.
pub fn insert(
    conn: &Connection,
    actor: &PublicKey,
    target: Option<&PublicKey>,
    action: &str,
    metadata: Value,
) -> Result<AuditEvent> {
    insert_in(conn, CATEGORY_MODERATION, actor, target, action, metadata)
}

/// Insert an activity row: a join or a leave, not an act of authority.
///
/// Separate from [`insert`] only by the category it writes, but the category is
/// the whole point — it keeps these out of the moderation log's query and puts
/// them inside the retention sweep's.
pub fn insert_activity(
    conn: &Connection,
    actor: &PublicKey,
    action: &str,
    metadata: Value,
) -> Result<AuditEvent> {
    insert_in(conn, CATEGORY_ACTIVITY, actor, None, action, metadata)
}

fn insert_in(
    conn: &Connection,
    category: &str,
    actor: &PublicKey,
    target: Option<&PublicKey>,
    action: &str,
    metadata: Value,
) -> Result<AuditEvent> {
    let timestamp_ms = current_unix_ms();
    let metadata_str = metadata.to_string();
    conn.execute(
        "INSERT INTO audit_events (actor_pk, target_pk, action, metadata, timestamp_ms, category) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            actor.as_bytes().as_slice(),
            target.map(|t| t.as_bytes().to_vec()),
            action,
            metadata_str,
            timestamp_ms as i64,
            category,
        ],
    )?;
    let id = conn.last_insert_rowid() as u64;
    Ok(AuditEvent {
        id,
        actor: actor.clone(),
        target: target.cloned(),
        action: action.to_string(),
        metadata,
        timestamp_ms,
    })
}

/// List moderation events newest-first. `before_id` is exclusive.
/// `limit` is server-clamped to 100.
///
/// **Activity rows are excluded**, which is a behaviour change for any caller
/// written before the split — and the intended one. This is the log a moderator
/// opens to ask who banned whom; a thousand voice joins in front of the answer
/// is a regression, not extra information. Activity has its own reader below.
pub fn list(conn: &Connection, before_id: Option<u64>, limit: u32) -> Result<Vec<AuditEvent>> {
    list_category(conn, CATEGORY_MODERATION, before_id, limit)
}

/// List activity events (joins and leaves) newest-first, same cursor contract.
pub fn list_activity(
    conn: &Connection,
    before_id: Option<u64>,
    limit: u32,
) -> Result<Vec<AuditEvent>> {
    list_category(conn, CATEGORY_ACTIVITY, before_id, limit)
}

fn list_category(
    conn: &Connection,
    category: &str,
    before_id: Option<u64>,
    limit: u32,
) -> Result<Vec<AuditEvent>> {
    let limit = (limit as i64).min(100);
    let rows: Vec<AuditEvent> = match before_id {
        Some(bid) => {
            let mut stmt = conn.prepare(
                "SELECT id, actor_pk, target_pk, action, metadata, timestamp_ms
                 FROM audit_events WHERE category = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3",
            )?;
            let out: Vec<AuditEvent> = stmt
                .query_map(rusqlite::params![category, bid as i64, limit], row_to_event)?
                .filter_map(|r| r.ok())
                .collect();
            out
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT id, actor_pk, target_pk, action, metadata, timestamp_ms
                 FROM audit_events WHERE category = ?1 ORDER BY id DESC LIMIT ?2",
            )?;
            let out: Vec<AuditEvent> = stmt
                .query_map(rusqlite::params![category, limit], row_to_event)?
                .filter_map(|r| r.ok())
                .collect();
            out
        }
    };
    Ok(rows)
}

/// Delete activity rows older than `cutoff_ms`. Returns how many went.
///
/// **Only activity.** A retention sweep that could reach a moderation row would
/// mean a server operator's own record of who they banned quietly expiring, and
/// an admin who wanted their ban history gone could get it by setting a short
/// window. The category in the WHERE clause is that guarantee.
pub fn prune_activity(conn: &Connection, cutoff_ms: u64) -> Result<u64> {
    // SQLite integers are signed, and `u64 as i64` wraps: a cutoff past
    // i64::MAX would arrive as a negative number and match nothing, so the
    // sweep would silently delete NOTHING exactly when it was told to delete
    // everything. Saturate instead of wrapping.
    let cutoff = i64::try_from(cutoff_ms).unwrap_or(i64::MAX);
    let n = conn.execute(
        "DELETE FROM audit_events WHERE category = ?1 AND timestamp_ms < ?2",
        rusqlite::params![CATEGORY_ACTIVITY, cutoff],
    )?;
    Ok(n as u64)
}

// ---------------------------------------------------------------------------
// Activity log settings
// ---------------------------------------------------------------------------

const KEY_ENABLED: &str = "activity_log_enabled";
const KEY_RETENTION_DAYS: &str = "activity_log_retention_days";

/// Default window. Long enough to answer "who was in that call last month",
/// short enough that a Farder server is not quietly building a permanent
/// record of when everybody sleeps.
pub const DEFAULT_RETENTION_DAYS: u32 = 30;
/// Floor of 1 day and a ceiling of ~2 years. The ceiling is not a storage
/// limit; it is the product saying that "forever" is not on the menu for this
/// half of the log.
pub const MIN_RETENTION_DAYS: u32 = 1;
pub const MAX_RETENTION_DAYS: u32 = 730;

/// Whether new activity rows are written at all.
///
/// Defaults to ON: an admin who opens the tab on a fresh server should find the
/// feature working rather than an empty list with no explanation. It is
/// MANAGE_SERVER-only to read, bounded by [`DEFAULT_RETENTION_DAYS`], and the
/// switch is one click away in server settings.
pub fn logging_enabled(conn: &Connection) -> bool {
    match crate::db::get_setting(conn, KEY_ENABLED) {
        Ok(Some(v)) => v != "0",
        _ => true,
    }
}

pub fn retention_days(conn: &Connection) -> u32 {
    crate::db::get_setting(conn, KEY_RETENTION_DAYS)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u32>().ok())
        .map(clamp_retention_days)
        .unwrap_or(DEFAULT_RETENTION_DAYS)
}

pub fn clamp_retention_days(days: u32) -> u32 {
    days.clamp(MIN_RETENTION_DAYS, MAX_RETENTION_DAYS)
}

pub fn set_logging(conn: &Connection, enabled: bool, days: u32) -> Result<()> {
    crate::db::set_setting(conn, KEY_ENABLED, if enabled { "1" } else { "0" })?;
    crate::db::set_setting(
        conn,
        KEY_RETENTION_DAYS,
        &clamp_retention_days(days).to_string(),
    )?;
    Ok(())
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
    let metadata: Value =
        serde_json::from_str(&metadata_str).unwrap_or(Value::Object(Default::default()));
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
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Blob,
            "expected 32-byte pubkey".into(),
        )
    })?;
    Ok(PublicKey::from_bytes(arr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use farder_crypto::identity::Keypair;
    use serde_json::json;

    fn test_conn() -> Connection {
        db::open_in_memory().unwrap()
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
        assert!(next.iter().all(|e| e.id < first[1].id));
    }

    #[test]
    fn activity_and_moderation_are_separate_lists() {
        let conn = test_conn();
        let actor = Keypair::generate().public_key();

        insert(&conn, &actor, None, "ban", json!({})).unwrap();
        for i in 0..40 {
            insert_activity(&conn, &actor, "voice_joined", json!({ "i": i })).unwrap();
        }

        // The whole reason for the split: 40 joins do not bury one ban.
        let moderation = list(&conn, None, 100).unwrap();
        assert_eq!(moderation.len(), 1);
        assert_eq!(moderation[0].action, "ban");

        let activity = list_activity(&conn, None, 100).unwrap();
        assert_eq!(activity.len(), 40);
        assert!(activity.iter().all(|e| e.action == "voice_joined"));
    }

    #[test]
    fn the_prune_cannot_reach_a_moderation_row() {
        let conn = test_conn();
        let actor = Keypair::generate().public_key();

        let ban = insert(&conn, &actor, None, "ban", json!({})).unwrap();
        let join = insert_activity(&conn, &actor, "voice_joined", json!({})).unwrap();

        // A cutoff far in the future: everything is "old". Only activity goes.
        let pruned = prune_activity(&conn, u64::MAX).unwrap();
        assert_eq!(pruned, 1, "exactly the one activity row");

        let moderation = list(&conn, None, 100).unwrap();
        assert_eq!(moderation.len(), 1);
        assert_eq!(moderation[0].id, ban.id, "the ban survives any window");
        assert!(list_activity(&conn, None, 100).unwrap().is_empty());
        assert_ne!(ban.id, join.id);
    }

    #[test]
    fn the_prune_keeps_rows_inside_the_window() {
        let conn = test_conn();
        let actor = Keypair::generate().public_key();
        let recent = insert_activity(&conn, &actor, "session_started", json!({})).unwrap();

        // Cutoff one second before the row was written.
        let pruned = prune_activity(&conn, recent.timestamp_ms - 1000).unwrap();
        assert_eq!(pruned, 0);
        assert_eq!(list_activity(&conn, None, 10).unwrap().len(), 1);
    }

    #[test]
    fn rows_written_before_the_split_read_as_moderation() {
        // A pre-migration database has rows with no category. The column default
        // has to backfill them into the moderation log, or every ban this server
        // ever recorded silently disappears from the tab that shows them.
        let conn = test_conn();
        let actor = Keypair::generate().public_key();
        conn.execute(
            "INSERT INTO audit_events (actor_pk, target_pk, action, metadata, timestamp_ms)
             VALUES (?1, NULL, 'ban', '{}', 1000)",
            rusqlite::params![actor.as_bytes().as_slice()],
        )
        .unwrap();

        let moderation = list(&conn, None, 10).unwrap();
        assert_eq!(moderation.len(), 1, "a legacy row must still be listed");
        assert_eq!(moderation[0].action, "ban");
        assert!(list_activity(&conn, None, 10).unwrap().is_empty());
    }

    #[test]
    fn activity_logging_defaults_to_on_with_a_bounded_window() {
        let conn = test_conn();
        assert!(logging_enabled(&conn), "a fresh server records activity");
        assert_eq!(retention_days(&conn), DEFAULT_RETENTION_DAYS);
    }

    #[test]
    fn the_retention_window_is_clamped_both_ways() {
        let conn = test_conn();

        // "Keep it forever" is not on the menu for this half of the log.
        set_logging(&conn, true, 100_000).unwrap();
        assert_eq!(retention_days(&conn), MAX_RETENTION_DAYS);

        // Nor is zero, which would make the sweep delete rows as fast as they
        // are written and look exactly like a broken feature.
        set_logging(&conn, true, 0).unwrap();
        assert_eq!(retention_days(&conn), MIN_RETENTION_DAYS);
    }

    #[test]
    fn turning_logging_off_is_remembered() {
        let conn = test_conn();
        set_logging(&conn, false, 30).unwrap();
        assert!(!logging_enabled(&conn));
        set_logging(&conn, true, 30).unwrap();
        assert!(logging_enabled(&conn));
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
