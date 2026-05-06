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

/// Insert an audit event row and return the populated AuditEvent struct.
pub fn insert(
    conn: &Connection,
    actor: &PublicKey,
    target: Option<&PublicKey>,
    action: &str,
    metadata: Value,
) -> Result<AuditEvent> {
    let timestamp_ms = current_unix_ms();
    let metadata_str = metadata.to_string();
    conn.execute(
        "INSERT INTO audit_events (actor_pk, target_pk, action, metadata, timestamp_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            actor.as_bytes().as_slice(),
            target.map(|t| t.as_bytes().to_vec()),
            action,
            metadata_str,
            timestamp_ms as i64,
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

/// List audit events newest-first. `before_id` is exclusive.
/// `limit` is server-clamped to 100.
pub fn list(conn: &Connection, before_id: Option<u64>, limit: u32) -> Result<Vec<AuditEvent>> {
    let limit = (limit as i64).min(100);
    let rows: Vec<AuditEvent> = match before_id {
        Some(bid) => {
            let mut stmt = conn.prepare(
                "SELECT id, actor_pk, target_pk, action, metadata, timestamp_ms
                 FROM audit_events WHERE id < ?1 ORDER BY id DESC LIMIT ?2",
            )?;
            let out: Vec<AuditEvent> = stmt
                .query_map(rusqlite::params![bid as i64, limit], row_to_event)?
                .filter_map(|r| r.ok())
                .collect();
            out
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT id, actor_pk, target_pk, action, metadata, timestamp_ms
                 FROM audit_events ORDER BY id DESC LIMIT ?1",
            )?;
            let out: Vec<AuditEvent> = stmt
                .query_map(rusqlite::params![limit], row_to_event)?
                .filter_map(|r| r.ok())
                .collect();
            out
        }
    };
    Ok(rows)
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
