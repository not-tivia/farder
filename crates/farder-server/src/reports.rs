//! Message reports: the only way a moderator learns about content in an
//! encrypted channel.
//!
//! The server cannot read an encrypted message, so it cannot find anything by
//! itself — moderation there begins with a person pointing at something. That is
//! what a report is: a pointer (channel, message, and the log event hash so the
//! message can be deleted content-blind even after a restart re-derives the row)
//! plus a reason, plus OPTIONALLY the reporter's own decrypted copy.
//!
//! **The evidence column is the sharp edge.** Storing it means plaintext from an
//! encrypted channel now sits in the server's database, readable by whoever runs
//! it. That is a real cost, so it is never collected silently: the client asks,
//! per report, and `None` — "act on my word" — is a legitimate answer and the
//! default.

use anyhow::Result;
use farder_crypto::identity::PublicKey;
use farder_protocol::server::ReportInfo;
use rusqlite::{params, Connection, OptionalExtension};

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// File a report. Returns the stored row as a moderator will see it.
pub fn create(
    conn: &Connection,
    reporter: &PublicKey,
    channel_id: u64,
    message_id: u64,
    event_hash: Option<&str>,
    reason: &str,
    evidence: Option<&str>,
) -> Result<ReportInfo> {
    let created_at = now_secs();
    conn.execute(
        "INSERT INTO message_reports \
         (reporter_pk, channel_id, message_id, event_hash, reason, evidence, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            reporter.as_bytes().as_slice(),
            channel_id as i64,
            message_id as i64,
            event_hash,
            reason,
            evidence,
            created_at as i64,
        ],
    )?;
    let id = conn.last_insert_rowid() as u64;
    get(conn, id)?.ok_or_else(|| anyhow::anyhow!("report vanished immediately after insert"))
}

/// One report by id.
pub fn get(conn: &Connection, id: u64) -> Result<Option<ReportInfo>> {
    let row = conn
        .query_row(
            "SELECT id, reporter_pk, channel_id, message_id, event_hash, reason, evidence, \
                    created_at, outcome, resolved_by \
             FROM message_reports WHERE id = ?1",
            params![id as i64],
            row_to_report,
        )
        .optional()?;
    match row {
        None => Ok(None),
        Some(mut r) => {
            hydrate(conn, &mut r)?;
            Ok(Some(r))
        }
    }
}

/// The moderator queue, newest first.
pub fn list(conn: &Connection, before_id: Option<u64>, limit: u32) -> Result<Vec<ReportInfo>> {
    let limit = (limit as i64).min(100);
    let mut out = Vec::new();
    {
        let sql = "SELECT id, reporter_pk, channel_id, message_id, event_hash, reason, evidence, \
                          created_at, outcome, resolved_by \
                   FROM message_reports";
        let (sql, args): (String, Vec<i64>) = match before_id {
            Some(b) => (format!("{sql} WHERE id < ?1 ORDER BY id DESC LIMIT ?2"), vec![b as i64, limit]),
            None => (format!("{sql} ORDER BY id DESC LIMIT ?1"), vec![limit]),
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args), row_to_report)?;
        for r in rows {
            out.push(r?);
        }
    }
    for r in out.iter_mut() {
        hydrate(conn, r)?;
    }
    Ok(out)
}

/// Record how a report was handled. The row is KEPT: "looked at it, did nothing"
/// is an outcome a moderation log has to be able to show, and deleting the
/// record would make a quiet dismissal indistinguishable from never having seen
/// it.
pub fn resolve(conn: &Connection, id: u64, outcome: &str, resolver: &PublicKey) -> Result<bool> {
    let n = conn.execute(
        "UPDATE message_reports SET outcome = ?2, resolved_by = ?3 WHERE id = ?1",
        params![id as i64, outcome, resolver.as_bytes().as_slice()],
    )?;
    Ok(n > 0)
}

fn row_to_report(row: &rusqlite::Row) -> rusqlite::Result<ReportInfo> {
    let reporter: Vec<u8> = row.get(1)?;
    let resolved_by: Option<Vec<u8>> = row.get(9)?;
    Ok(ReportInfo {
        id: row.get::<_, i64>(0)? as u64,
        reporter: pk(reporter)?,
        reporter_name: None,
        channel_id: row.get::<_, i64>(2)? as u64,
        message_id: row.get::<_, i64>(3)? as u64,
        event_hash: row.get(4)?,
        author: None,
        author_name: None,
        reason: row.get(5)?,
        evidence: row.get(6)?,
        created_at: row.get::<_, i64>(7)?.max(0) as u64,
        outcome: row.get(8)?,
        resolved_by: match resolved_by {
            Some(b) => Some(pk(b)?),
            None => None,
        },
    })
}

fn pk(bytes: Vec<u8>) -> rusqlite::Result<PublicKey> {
    let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        rusqlite::Error::InvalidColumnType(0, "public_key".into(), rusqlite::types::Type::Blob)
    })?;
    Ok(PublicKey::from_bytes(arr))
}

/// Fill in the names and the reported message's author.
///
/// Looked up at READ time, not stored on the report: a display name changes, a
/// member leaves, and a moderator should see who these people are now rather
/// than who they were when someone clicked report. Absent rows stay `None` — a
/// report about a message that has since been deleted is still a report.
fn hydrate(conn: &Connection, r: &mut ReportInfo) -> Result<()> {
    r.reporter_name = crate::members::get_member(conn, &r.reporter)?.map(|m| m.display_name);

    let author: Option<Vec<u8>> = conn
        .query_row(
            "SELECT author FROM messages WHERE id = ?1",
            params![r.message_id as i64],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(bytes) = author {
        if let Ok(arr) = <[u8; 32]>::try_from(bytes.as_slice()) {
            let key = PublicKey::from_bytes(arr);
            r.author_name = crate::members::get_member(conn, &key)?.map(|m| m.display_name);
            r.author = Some(key);
        }
    }
    Ok(())
}
