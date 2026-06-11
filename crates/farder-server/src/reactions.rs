use anyhow::{bail, Result};
use farder_crypto::identity::PublicKey;
use farder_protocol::server::ReactionGroup;
use rusqlite::{params, Connection};
use std::collections::HashMap;

use crate::db::now;

/// Returns true if a NEW reaction row was inserted, false if it already
/// existed (INSERT OR IGNORE) — callers must not broadcast a ReactionAdded
/// event for an ignored duplicate.
pub fn add_reaction(
    conn: &Connection,
    message_id: u64,
    user_key: &PublicKey,
    emoji: &str,
    file_id: Option<u64>,
) -> Result<bool> {
    if emoji.is_empty() {
        bail!("emoji cannot be empty");
    }
    if emoji.len() > 32 {
        bail!("emoji too long (max 32 bytes)");
    }
    // Sentinel ":custom:" must include a file_id; non-sentinel must NOT.
    if emoji == ":custom:" && file_id.is_none() {
        bail!("custom emoji reaction requires file_id");
    }
    if emoji != ":custom:" && file_id.is_some() {
        bail!("non-custom emoji must not include file_id");
    }

    // Existing-row check: matched on (message_id, user_key, emoji, file_id).
    // Distinct-emoji-group check uses (emoji, file_id) pair.
    let emoji_exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM reactions WHERE message_id = ?1 AND emoji = ?2 AND \
         ((file_id IS NULL AND ?3 IS NULL) OR file_id = ?3)",
        params![message_id as i64, emoji, file_id.map(|v| v as i64)],
        |row| row.get::<_, i64>(0),
    )? > 0;

    if !emoji_exists {
        let distinct_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM (SELECT DISTINCT emoji, file_id FROM reactions WHERE message_id = ?1)",
            params![message_id as i64],
            |row| row.get(0),
        )?;
        if distinct_count >= 20 {
            bail!("maximum 20 unique reactions per message");
        }
    }

    let inserted = conn.execute(
        "INSERT OR IGNORE INTO reactions (message_id, user_key, emoji, file_id, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            message_id as i64,
            user_key.as_bytes().as_slice(),
            emoji,
            file_id.map(|v| v as i64),
            now() as i64,
        ],
    )?;
    Ok(inserted > 0)
}

pub fn remove_reaction(
    conn: &Connection,
    message_id: u64,
    user_key: &PublicKey,
    emoji: &str,
    file_id: Option<u64>,
) -> Result<()> {
    conn.execute(
        "DELETE FROM reactions WHERE message_id = ?1 AND user_key = ?2 AND emoji = ?3 AND \
         ((file_id IS NULL AND ?4 IS NULL) OR file_id = ?4)",
        params![
            message_id as i64,
            user_key.as_bytes().as_slice(),
            emoji,
            file_id.map(|v| v as i64),
        ],
    )?;
    Ok(())
}

pub fn delete_reactions_for_message(conn: &Connection, message_id: u64) -> Result<()> {
    conn.execute(
        "DELETE FROM reactions WHERE message_id = ?1",
        params![message_id as i64],
    )?;
    Ok(())
}

pub fn delete_reactions_by_user(conn: &Connection, user_key: &PublicKey) -> Result<u64> {
    let count = conn.execute(
        "DELETE FROM reactions WHERE user_key = ?1",
        params![user_key.as_bytes().as_slice()],
    )?;
    Ok(count as u64)
}

pub fn get_reactions_for_message(
    conn: &Connection,
    message_id: u64,
    requester: &PublicKey,
) -> Result<Vec<ReactionGroup>> {
    let mut stmt = conn.prepare(
        "SELECT emoji, file_id, COUNT(*) as cnt, \
                MAX(CASE WHEN user_key = ?2 THEN 1 ELSE 0 END) as me \
         FROM reactions \
         WHERE message_id = ?1 \
         GROUP BY emoji, file_id \
         ORDER BY MIN(created_at) ASC",
    )?;

    let rows = stmt.query_map(
        params![message_id as i64, requester.as_bytes().as_slice()],
        |row| {
            let emoji: String = row.get(0)?;
            let file_id: Option<i64> = row.get(1)?;
            let count: i64 = row.get(2)?;
            let me: i64 = row.get(3)?;
            Ok(ReactionGroup {
                emoji,
                count: count as u32,
                me: me != 0,
                file_id: file_id.map(|v| v as u64),
            })
        },
    )?;

    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}

pub fn get_reactions_for_messages(
    conn: &Connection,
    message_ids: &[u64],
    requester: &PublicKey,
) -> Result<HashMap<u64, Vec<ReactionGroup>>> {
    if message_ids.is_empty() {
        return Ok(HashMap::new());
    }

    // Build IN clause placeholders: requester is ?1, message_ids start at ?2.
    let placeholders: Vec<String> = message_ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 2))
        .collect();

    let sql = format!(
        "SELECT message_id, emoji, file_id, COUNT(*) as cnt, \
                MAX(CASE WHEN user_key = ?1 THEN 1 ELSE 0 END) as me \
         FROM reactions \
         WHERE message_id IN ({}) \
         GROUP BY message_id, emoji, file_id \
         ORDER BY message_id ASC, MIN(created_at) ASC",
        placeholders.join(",")
    );

    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(requester.as_bytes().to_vec()));
    for id in message_ids {
        param_values.push(Box::new(*id as i64));
    }
    let params_ref: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_ref.as_slice(), |row| {
        let message_id: i64 = row.get(0)?;
        let emoji: String = row.get(1)?;
        let file_id: Option<i64> = row.get(2)?;
        let count: i64 = row.get(3)?;
        let me: i64 = row.get(4)?;
        Ok((
            message_id as u64,
            ReactionGroup {
                emoji,
                count: count as u32,
                me: me != 0,
                file_id: file_id.map(|v| v as u64),
            },
        ))
    })?;

    let mut map: HashMap<u64, Vec<ReactionGroup>> = HashMap::new();
    for row in rows {
        let (message_id, group) = row?;
        map.entry(message_id).or_default().push(group);
    }
    Ok(map)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::create_channel;
    use crate::db;
    use crate::members::register_member;
    use crate::messages::insert_message;
    use farder_crypto::identity::Keypair;
    use farder_protocol::server::ChannelType;

    fn gen_pk() -> PublicKey {
        Keypair::generate().public_key()
    }

    /// Sets up an in-memory DB with two registered members, one channel, and one message.
    /// Returns (conn, message_id, user1, user2).
    fn setup() -> (rusqlite::Connection, u64, PublicKey, PublicKey) {
        let conn = db::open_in_memory().unwrap();
        let channel_id = create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let user1 = gen_pk();
        let user2 = gen_pk();
        register_member(&conn, &user1, "Alice").unwrap();
        register_member(&conn, &user2, "Bob").unwrap();
        let msg_id = insert_message(&conn, channel_id, &user1, "hello", None).unwrap();
        (conn, msg_id, user1, user2)
    }

    #[test]
    fn test_add_and_get() {
        let (conn, msg_id, user1, user2) = setup();

        add_reaction(&conn, msg_id, &user1, "👍", None).unwrap();
        add_reaction(&conn, msg_id, &user2, "👍", None).unwrap();

        let groups = get_reactions_for_message(&conn, msg_id, &user1).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].emoji, "👍");
        assert_eq!(groups[0].count, 2);
    }

    #[test]
    fn test_me_field() {
        let (conn, msg_id, user1, user2) = setup();

        add_reaction(&conn, msg_id, &user1, "❤️", None).unwrap();

        // user1 reacted — me should be true.
        let groups = get_reactions_for_message(&conn, msg_id, &user1).unwrap();
        assert!(groups[0].me);

        // user2 did not react — me should be false.
        let groups = get_reactions_for_message(&conn, msg_id, &user2).unwrap();
        assert!(!groups[0].me);
    }

    #[test]
    fn test_idempotent() {
        let (conn, msg_id, user1, _user2) = setup();

        add_reaction(&conn, msg_id, &user1, "🔥", None).unwrap();
        // Adding the same reaction again should not error and not increment count.
        add_reaction(&conn, msg_id, &user1, "🔥", None).unwrap();

        let groups = get_reactions_for_message(&conn, msg_id, &user1).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count, 1);
    }

    #[test]
    fn test_max_20() {
        let (conn, msg_id, user1, _user2) = setup();

        // Add 20 distinct emoji from user1.
        let emojis = [
            "😀", "😁", "😂", "🤣", "😃", "😄", "😅", "😆", "😉", "😊",
            "😋", "😎", "😍", "😘", "🥰", "😗", "😙", "😚", "🙂", "🤗",
        ];
        for e in &emojis {
            add_reaction(&conn, msg_id, &user1, e, None).unwrap();
        }

        // 21st unique emoji should fail.
        let result = add_reaction(&conn, msg_id, &user1, "🦀", None);
        assert!(result.is_err(), "should reject 21st unique emoji");
        assert!(result.unwrap_err().to_string().contains("maximum 20 unique reactions"));
    }

    #[test]
    fn test_remove() {
        let (conn, msg_id, user1, user2) = setup();

        add_reaction(&conn, msg_id, &user1, "👍", None).unwrap();
        add_reaction(&conn, msg_id, &user2, "👍", None).unwrap();

        remove_reaction(&conn, msg_id, &user1, "👍", None).unwrap();

        let groups = get_reactions_for_message(&conn, msg_id, &user1).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count, 1);
        assert!(!groups[0].me, "user1 removed their reaction so me=false");
    }

    #[test]
    fn test_delete_for_message() {
        let (conn, msg_id, user1, user2) = setup();

        add_reaction(&conn, msg_id, &user1, "👍", None).unwrap();
        add_reaction(&conn, msg_id, &user2, "❤️", None).unwrap();

        delete_reactions_for_message(&conn, msg_id).unwrap();

        let groups = get_reactions_for_message(&conn, msg_id, &user1).unwrap();
        assert!(groups.is_empty());
    }

    #[test]
    fn test_delete_reactions_by_user() {
        let (conn, msg_id, user1, user2) = setup();

        add_reaction(&conn, msg_id, &user1, "👍", None).unwrap();
        add_reaction(&conn, msg_id, &user1, "❤️", None).unwrap();
        add_reaction(&conn, msg_id, &user2, "👍", None).unwrap();

        let deleted = delete_reactions_by_user(&conn, &user1).unwrap();
        assert_eq!(deleted, 2, "should have deleted 2 reactions for user1");

        // user2's reaction should remain.
        let groups = get_reactions_for_message(&conn, msg_id, &user2).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].emoji, "👍");
        assert_eq!(groups[0].count, 1);
        assert!(groups[0].me, "user2 still has their reaction");
    }

    #[test]
    fn test_batch_load() {
        let conn = db::open_in_memory().unwrap();
        let channel_id = create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let user1 = gen_pk();
        let user2 = gen_pk();
        register_member(&conn, &user1, "Alice").unwrap();
        register_member(&conn, &user2, "Bob").unwrap();

        let msg1 = insert_message(&conn, channel_id, &user1, "msg1", None).unwrap();
        let msg2 = insert_message(&conn, channel_id, &user1, "msg2", None).unwrap();
        let msg3 = insert_message(&conn, channel_id, &user1, "msg3", None).unwrap();

        add_reaction(&conn, msg1, &user1, "👍", None).unwrap();
        add_reaction(&conn, msg1, &user2, "👍", None).unwrap();
        add_reaction(&conn, msg2, &user2, "❤️", None).unwrap();
        // msg3 has no reactions.

        let map = get_reactions_for_messages(&conn, &[msg1, msg2, msg3], &user1).unwrap();

        let g1 = map.get(&msg1).unwrap();
        assert_eq!(g1.len(), 1);
        assert_eq!(g1[0].emoji, "👍");
        assert_eq!(g1[0].count, 2);
        assert!(g1[0].me, "user1 reacted on msg1");

        let g2 = map.get(&msg2).unwrap();
        assert_eq!(g2.len(), 1);
        assert_eq!(g2[0].emoji, "❤️");
        assert_eq!(g2[0].count, 1);
        assert!(!g2[0].me, "user1 did not react on msg2");

        // msg3 absent or empty.
        assert!(map.get(&msg3).map(|v| v.is_empty()).unwrap_or(true));

        // Empty input.
        let empty = get_reactions_for_messages(&conn, &[], &user1).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn add_reaction_custom_requires_file_id() {
        let (conn, msg_id, user1, _user2) = setup();
        let result = add_reaction(&conn, msg_id, &user1, ":custom:", None);
        assert!(result.is_err(), "custom emoji without file_id must error");
        assert!(result.unwrap_err().to_string().contains("custom emoji reaction requires file_id"));
    }

    #[test]
    fn add_reaction_unicode_rejects_file_id() {
        let (conn, msg_id, user1, _user2) = setup();
        let result = add_reaction(&conn, msg_id, &user1, "👍", Some(42));
        assert!(result.is_err(), "non-custom emoji with file_id must error");
        assert!(result.unwrap_err().to_string().contains("non-custom emoji must not include file_id"));
    }

    #[test]
    fn custom_reactions_with_different_file_ids_are_distinct_groups() {
        let (conn, msg_id, user1, user2) = setup();

        // Insert two fake file rows so the FK is satisfied.
        let uploader_bytes = user1.as_bytes().to_vec();
        conn.execute(
            "INSERT INTO files (id, hash, size, mime_type, original_name, uploaded_by, uploaded_at, ref_count) \
             VALUES (1, 'hash_a', 100, 'image/png', 'a.png', ?1, 0, 0)",
            params![uploader_bytes.as_slice()],
        ).unwrap();
        conn.execute(
            "INSERT INTO files (id, hash, size, mime_type, original_name, uploaded_by, uploaded_at, ref_count) \
             VALUES (2, 'hash_b', 200, 'image/png', 'b.png', ?1, 0, 0)",
            params![uploader_bytes.as_slice()],
        ).unwrap();

        add_reaction(&conn, msg_id, &user1, ":custom:", Some(1)).unwrap();
        add_reaction(&conn, msg_id, &user2, ":custom:", Some(2)).unwrap();

        let groups = get_reactions_for_message(&conn, msg_id, &user1).unwrap();
        assert_eq!(groups.len(), 2, "two distinct custom emoji groups (different file_id)");
        assert!(groups.iter().all(|g| g.emoji == ":custom:"));
        let file_ids: Vec<Option<u64>> = groups.iter().map(|g| g.file_id).collect();
        assert!(file_ids.contains(&Some(1)));
        assert!(file_ids.contains(&Some(2)));
    }
}
