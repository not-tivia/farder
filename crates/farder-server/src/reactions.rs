use anyhow::{bail, Result};
use farder_crypto::identity::PublicKey;
use farder_protocol::server::ReactionGroup;
use rusqlite::{params, Connection};
use std::collections::HashMap;

use crate::db::now;

pub fn add_reaction(
    conn: &Connection,
    message_id: u64,
    user_key: &PublicKey,
    emoji: &str,
) -> Result<()> {
    // Check if this emoji already exists for this message (regardless of user).
    let emoji_exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM reactions WHERE message_id = ?1 AND emoji = ?2",
        params![message_id as i64, emoji],
        |row| row.get::<_, i64>(0),
    )? > 0;

    // If it's a new emoji, check the distinct emoji count for the message.
    if !emoji_exists {
        let distinct_count: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT emoji) FROM reactions WHERE message_id = ?1",
            params![message_id as i64],
            |row| row.get(0),
        )?;
        if distinct_count >= 20 {
            bail!("maximum 20 unique emoji per message");
        }
    }

    conn.execute(
        "INSERT OR IGNORE INTO reactions (message_id, user_key, emoji, created_at) \
         VALUES (?1, ?2, ?3, ?4)",
        params![
            message_id as i64,
            user_key.as_bytes().as_slice(),
            emoji,
            now() as i64,
        ],
    )?;

    Ok(())
}

pub fn remove_reaction(
    conn: &Connection,
    message_id: u64,
    user_key: &PublicKey,
    emoji: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM reactions WHERE message_id = ?1 AND user_key = ?2 AND emoji = ?3",
        params![
            message_id as i64,
            user_key.as_bytes().as_slice(),
            emoji,
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

pub fn get_reactions_for_message(
    conn: &Connection,
    message_id: u64,
    requester: &PublicKey,
) -> Result<Vec<ReactionGroup>> {
    let mut stmt = conn.prepare(
        "SELECT emoji, COUNT(*) as cnt, \
                MAX(CASE WHEN user_key = ?2 THEN 1 ELSE 0 END) as me \
         FROM reactions \
         WHERE message_id = ?1 \
         GROUP BY emoji \
         ORDER BY MIN(created_at) ASC",
    )?;

    let rows = stmt.query_map(
        params![message_id as i64, requester.as_bytes().as_slice()],
        |row| {
            let emoji: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            let me: i64 = row.get(2)?;
            Ok(ReactionGroup {
                emoji,
                count: count as u32,
                me: me != 0,
            })
        },
    )?;

    let mut groups = Vec::new();
    for row in rows {
        groups.push(row?);
    }
    Ok(groups)
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
        "SELECT message_id, emoji, COUNT(*) as cnt, \
                MAX(CASE WHEN user_key = ?1 THEN 1 ELSE 0 END) as me \
         FROM reactions \
         WHERE message_id IN ({}) \
         GROUP BY message_id, emoji \
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
        let count: i64 = row.get(2)?;
        let me: i64 = row.get(3)?;
        Ok((
            message_id as u64,
            ReactionGroup {
                emoji,
                count: count as u32,
                me: me != 0,
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

        add_reaction(&conn, msg_id, &user1, "👍").unwrap();
        add_reaction(&conn, msg_id, &user2, "👍").unwrap();

        let groups = get_reactions_for_message(&conn, msg_id, &user1).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].emoji, "👍");
        assert_eq!(groups[0].count, 2);
    }

    #[test]
    fn test_me_field() {
        let (conn, msg_id, user1, user2) = setup();

        add_reaction(&conn, msg_id, &user1, "❤️").unwrap();

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

        add_reaction(&conn, msg_id, &user1, "🔥").unwrap();
        // Adding the same reaction again should not error and not increment count.
        add_reaction(&conn, msg_id, &user1, "🔥").unwrap();

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
            add_reaction(&conn, msg_id, &user1, e).unwrap();
        }

        // 21st unique emoji should fail.
        let result = add_reaction(&conn, msg_id, &user1, "🦀");
        assert!(result.is_err(), "should reject 21st unique emoji");
        assert!(result.unwrap_err().to_string().contains("maximum 20 unique emoji"));
    }

    #[test]
    fn test_remove() {
        let (conn, msg_id, user1, user2) = setup();

        add_reaction(&conn, msg_id, &user1, "👍").unwrap();
        add_reaction(&conn, msg_id, &user2, "👍").unwrap();

        remove_reaction(&conn, msg_id, &user1, "👍").unwrap();

        let groups = get_reactions_for_message(&conn, msg_id, &user1).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count, 1);
        assert!(!groups[0].me, "user1 removed their reaction so me=false");
    }

    #[test]
    fn test_delete_for_message() {
        let (conn, msg_id, user1, user2) = setup();

        add_reaction(&conn, msg_id, &user1, "👍").unwrap();
        add_reaction(&conn, msg_id, &user2, "❤️").unwrap();

        delete_reactions_for_message(&conn, msg_id).unwrap();

        let groups = get_reactions_for_message(&conn, msg_id, &user1).unwrap();
        assert!(groups.is_empty());
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

        add_reaction(&conn, msg1, &user1, "👍").unwrap();
        add_reaction(&conn, msg1, &user2, "👍").unwrap();
        add_reaction(&conn, msg2, &user2, "❤️").unwrap();
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
}
