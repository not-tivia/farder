use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use farder_crypto::identity::PublicKey;
use farder_protocol::server::{MessageInfo, DELETED_USER_KEY};

use crate::db::now;

/// Shared SELECT column list for messages — must match `row_to_message_info` index order.
pub const MSG_SELECT: &str =
    "id, channel_id, author, content, timestamp, edited_at, reply_to, pinned";

pub fn row_to_message_info(row: &rusqlite::Row) -> rusqlite::Result<MessageInfo> {
    let id: i64 = row.get(0)?;
    let channel_id: i64 = row.get(1)?;
    let author_bytes: Vec<u8> = row.get(2)?;
    let content: String = row.get(3)?;
    let timestamp: i64 = row.get(4)?;
    let edited_at: Option<i64> = row.get(5)?;
    let reply_to: Option<i64> = row.get(6)?;
    let pinned: i64 = row.get(7)?;

    let arr: [u8; 32] = author_bytes
        .try_into()
        .map_err(|_| rusqlite::Error::InvalidColumnType(2, "author".into(), rusqlite::types::Type::Blob))?;

    Ok(MessageInfo {
        id: id as u64,
        channel_id: channel_id as u64,
        author: PublicKey::from_bytes(arr),
        content,
        timestamp: timestamp as u64,
        edited_at: edited_at.map(|v| v as u64),
        reply_to: reply_to.map(|v| v as u64),
        pinned: pinned != 0,
        attachments: vec![],
        reactions: vec![],
        thread_id: None,
        thread_message_count: None,
    })
}

// ---------------------------------------------------------------------------
// Message operations
// ---------------------------------------------------------------------------

pub fn insert_message(
    conn: &Connection,
    channel_id: u64,
    author: &PublicKey,
    content: &str,
    reply_to: Option<u64>,
) -> Result<u64> {
    insert_message_with_ts(conn, channel_id, author, content, reply_to, now())
}

pub fn insert_message_with_ts(
    conn: &Connection,
    channel_id: u64,
    author: &PublicKey,
    content: &str,
    reply_to: Option<u64>,
    timestamp: u64,
) -> Result<u64> {
    conn.execute(
        "INSERT INTO messages (channel_id, author, content, timestamp, reply_to) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            channel_id as i64,
            author.as_bytes().as_slice(),
            content,
            timestamp as i64,
            reply_to.map(|v| v as i64),
        ],
    )?;
    let id = conn.last_insert_rowid() as u64;

    // Insert into FTS5.
    conn.execute(
        "INSERT INTO messages_fts(rowid, content) VALUES (?1, ?2)",
        params![id as i64, content],
    )?;

    Ok(id)
}

pub fn get_message(conn: &Connection, id: u64, requester: &PublicKey) -> Result<Option<MessageInfo>> {
    let sql = format!("SELECT {} FROM messages WHERE id = ?1", MSG_SELECT);
    let mut msg = match conn.query_row(&sql, params![id as i64], row_to_message_info).optional()? {
        Some(m) => m,
        None => return Ok(None),
    };
    msg.attachments = crate::attachments::get_attachments_for_message(conn, msg.id)?;
    msg.reactions = crate::reactions::get_reactions_for_message(conn, msg.id, requester)?;
    if let Some(thread) = crate::channels::get_thread_for_message(conn, msg.id)? {
        msg.thread_id = Some(thread.id);
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE channel_id = ?1",
            rusqlite::params![thread.id as i64],
            |row| row.get(0),
        )?;
        msg.thread_message_count = Some(count as u32);
    }
    Ok(Some(msg))
}

pub fn fetch_history(
    conn: &Connection,
    channel_id: u64,
    before_id: Option<u64>,
    limit: u32,
    requester: &PublicKey,
) -> Result<Vec<MessageInfo>> {
    let sql = match before_id {
        Some(_) => format!(
            "SELECT {} FROM messages WHERE channel_id = ?1 AND id < ?2 \
             ORDER BY id DESC LIMIT ?3",
            MSG_SELECT
        ),
        None => format!(
            "SELECT {} FROM messages WHERE channel_id = ?1 \
             ORDER BY id DESC LIMIT ?2",
            MSG_SELECT
        ),
    };

    let mut messages = Vec::new();

    if let Some(bid) = before_id {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![channel_id as i64, bid as i64, limit as i64],
            row_to_message_info,
        )?;
        for row in rows {
            messages.push(row?);
        }
    } else {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![channel_id as i64, limit as i64],
            row_to_message_info,
        )?;
        for row in rows {
            messages.push(row?);
        }
    }

    let msg_ids: Vec<u64> = messages.iter().map(|m| m.id).collect();
    if !msg_ids.is_empty() {
        let attach_map = crate::attachments::get_attachments_for_messages(conn, &msg_ids)?;
        for msg in &mut messages {
            if let Some(attachments) = attach_map.get(&msg.id) {
                msg.attachments = attachments.clone();
            }
        }
        // Batch-load reactions
        let reaction_map = crate::reactions::get_reactions_for_messages(conn, &msg_ids, requester)?;
        for msg in &mut messages {
            if let Some(reactions) = reaction_map.get(&msg.id) {
                msg.reactions = reactions.clone();
            }
        }
        // Load thread metadata
        for msg in &mut messages {
            if let Some(thread) = crate::channels::get_thread_for_message(conn, msg.id)? {
                msg.thread_id = Some(thread.id);
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM messages WHERE channel_id = ?1",
                    rusqlite::params![thread.id as i64],
                    |row| row.get(0),
                )?;
                msg.thread_message_count = Some(count as u32);
            }
        }
    }

    Ok(messages)
}

pub fn edit_message(conn: &Connection, id: u64, new_content: &str) -> Result<()> {
    // Fetch the old content before updating so we can remove it from FTS5.
    let old_content: String = conn.query_row(
        "SELECT content FROM messages WHERE id = ?1",
        params![id as i64],
        |row| row.get(0),
    )?;

    let edited_at = now();
    conn.execute(
        "UPDATE messages SET content = ?2, edited_at = ?3 WHERE id = ?1",
        params![id as i64, new_content, edited_at as i64],
    )?;

    // For a content-backed FTS5 table, use the special 'delete' command to
    // remove the old entry, then re-insert with the updated content.
    conn.execute(
        "INSERT INTO messages_fts(messages_fts, rowid, content) VALUES ('delete', ?1, ?2)",
        params![id as i64, old_content],
    )?;
    conn.execute(
        "INSERT INTO messages_fts(rowid, content) VALUES (?1, ?2)",
        params![id as i64, new_content],
    )?;

    Ok(())
}

pub fn delete_message(conn: &Connection, id: u64) -> Result<Vec<u64>> {
    // Delete attachments and get orphaned file_ids
    let orphans = crate::attachments::delete_attachments_for_message(conn, id)?;

    crate::reactions::delete_reactions_for_message(conn, id)?;

    // Fetch the content before deleting so we can remove it from FTS5.
    let content: Option<String> = conn
        .query_row(
            "SELECT content FROM messages WHERE id = ?1",
            params![id as i64],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(old_content) = content {
        // Use the 'delete' command for the content-backed FTS5 table.
        conn.execute(
            "INSERT INTO messages_fts(messages_fts, rowid, content) VALUES ('delete', ?1, ?2)",
            params![id as i64, old_content],
        )?;
    }

    conn.execute(
        "DELETE FROM messages WHERE id = ?1",
        params![id as i64],
    )?;
    Ok(orphans)
}

pub fn pin_message(conn: &Connection, id: u64) -> Result<()> {
    conn.execute(
        "UPDATE messages SET pinned = 1 WHERE id = ?1",
        params![id as i64],
    )?;
    Ok(())
}

pub fn unpin_message(conn: &Connection, id: u64) -> Result<()> {
    conn.execute(
        "UPDATE messages SET pinned = 0 WHERE id = ?1",
        params![id as i64],
    )?;
    Ok(())
}

pub fn search_messages(
    conn: &Connection,
    query: &str,
    channel_id: Option<u64>,
    limit: u32,
    requester: &PublicKey,
) -> Result<Vec<MessageInfo>> {
    let mut messages = Vec::new();

    if let Some(cid) = channel_id {
        let sql = format!(
            "SELECT {} FROM messages \
             WHERE channel_id = ?2 AND id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1) \
             ORDER BY id DESC LIMIT ?3",
            MSG_SELECT
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![query, cid as i64, limit as i64],
            row_to_message_info,
        )?;
        for row in rows {
            messages.push(row?);
        }
    } else {
        let sql = format!(
            "SELECT {} FROM messages \
             WHERE id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1) \
             ORDER BY id DESC LIMIT ?2",
            MSG_SELECT
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![query, limit as i64], row_to_message_info)?;
        for row in rows {
            messages.push(row?);
        }
    }

    let msg_ids: Vec<u64> = messages.iter().map(|m| m.id).collect();
    if !msg_ids.is_empty() {
        let attach_map = crate::attachments::get_attachments_for_messages(conn, &msg_ids)?;
        for msg in &mut messages {
            if let Some(attachments) = attach_map.get(&msg.id) {
                msg.attachments = attachments.clone();
            }
        }
        // Batch-load reactions
        let reaction_map = crate::reactions::get_reactions_for_messages(conn, &msg_ids, requester)?;
        for msg in &mut messages {
            if let Some(reactions) = reaction_map.get(&msg.id) {
                msg.reactions = reactions.clone();
            }
        }
        // Load thread metadata
        for msg in &mut messages {
            if let Some(thread) = crate::channels::get_thread_for_message(conn, msg.id)? {
                msg.thread_id = Some(thread.id);
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM messages WHERE channel_id = ?1",
                    rusqlite::params![thread.id as i64],
                    |row| row.get(0),
                )?;
                msg.thread_message_count = Some(count as u32);
            }
        }
    }

    Ok(messages)
}

pub fn delete_messages_before(
    conn: &Connection,
    channel_id: u64,
    cutoff_timestamp: u64,
) -> Result<u64> {
    // For a content-backed FTS5 table we must use the 'delete' command for each
    // row individually (the bulk DELETE via subquery does not work for this FTS5
    // variant).  Collect the rows to delete first, then remove them one-by-one.
    let mut stmt = conn.prepare(
        "SELECT id, content FROM messages WHERE channel_id = ?1 AND timestamp < ?2",
    )?;
    let rows: Vec<(i64, String)> = stmt
        .query_map(
            params![channel_id as i64, cutoff_timestamp as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let count = rows.len() as u64;

    // Delete attachments for each message before deleting the messages.
    for (rid, _) in &rows {
        let _ = crate::attachments::delete_attachments_for_message(conn, *rid as u64)?;
        crate::reactions::delete_reactions_for_message(conn, *rid as u64)?;
    }

    for (rid, content) in &rows {
        conn.execute(
            "INSERT INTO messages_fts(messages_fts, rowid, content) VALUES ('delete', ?1, ?2)",
            params![rid, content],
        )?;
    }

    conn.execute(
        "DELETE FROM messages WHERE channel_id = ?1 AND timestamp < ?2",
        params![channel_id as i64, cutoff_timestamp as i64],
    )?;

    Ok(count)
}

/// Anonymize all messages by the given author:
/// - Removes old content from the FTS5 index and inserts "[deleted]"
/// - Sets author = DELETED_USER_KEY, content = '[deleted]' in the messages table
/// Returns the count of messages updated.
pub fn anonymize_messages_by_author(conn: &Connection, author: &PublicKey) -> Result<u64> {
    // Collect all (id, content) for this author.
    let mut stmt = conn.prepare(
        "SELECT id, content FROM messages WHERE author = ?1",
    )?;
    let rows: Vec<(i64, String)> = stmt
        .query_map(params![author.as_bytes().as_slice()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let count = rows.len() as u64;

    // Update FTS5 for each message: delete old content, insert "[deleted]".
    for (id, old_content) in &rows {
        conn.execute(
            "INSERT INTO messages_fts(messages_fts, rowid, content) VALUES ('delete', ?1, ?2)",
            params![id, old_content],
        )?;
        conn.execute(
            "INSERT INTO messages_fts(rowid, content) VALUES (?1, ?2)",
            params![id, "[deleted]"],
        )?;
    }

    // Bulk UPDATE: set author sentinel and content.
    conn.execute(
        "UPDATE messages SET author = ?1, content = '[deleted]' WHERE author = ?2",
        params![DELETED_USER_KEY.as_slice(), author.as_bytes().as_slice()],
    )?;

    Ok(count)
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
    use farder_crypto::identity::Keypair;
    use farder_protocol::server::ChannelType;

    fn setup() -> (rusqlite::Connection, u64, PublicKey) {
        let conn = db::open_in_memory().unwrap();
        let channel_id = create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let pk = Keypair::generate().public_key();
        register_member(&conn, &pk, "Alice").unwrap();
        (conn, channel_id, pk)
    }

    #[test]
    fn test_insert_and_get_message() {
        let (conn, channel_id, pk) = setup();
        let id = insert_message_with_ts(&conn, channel_id, &pk, "Hello, world!", None, 1_000_000).unwrap();
        let msg = get_message(&conn, id, &pk).unwrap().expect("message should exist");
        assert_eq!(msg.id, id);
        assert_eq!(msg.channel_id, channel_id);
        assert_eq!(msg.author, pk);
        assert_eq!(msg.content, "Hello, world!");
        assert_eq!(msg.timestamp, 1_000_000);
        assert!(msg.edited_at.is_none());
        assert!(msg.reply_to.is_none());
        assert!(!msg.pinned);
    }

    #[test]
    fn test_insert_reply() {
        let (conn, channel_id, pk) = setup();
        let parent_id = insert_message_with_ts(&conn, channel_id, &pk, "Parent message", None, 1_000).unwrap();
        let reply_id = insert_message_with_ts(&conn, channel_id, &pk, "Reply message", Some(parent_id), 2_000).unwrap();
        let reply = get_message(&conn, reply_id, &pk).unwrap().expect("reply should exist");
        assert_eq!(reply.reply_to, Some(parent_id));
    }

    #[test]
    fn test_fetch_history_paginated() {
        let (conn, channel_id, pk) = setup();

        // Insert 10 messages with distinct timestamps / ids.
        let mut ids = Vec::new();
        for i in 0..10u64 {
            let id = insert_message_with_ts(
                &conn,
                channel_id,
                &pk,
                &format!("message {}", i),
                None,
                1_000 + i,
            )
            .unwrap();
            ids.push(id);
        }

        // First page: 3 newest.
        let page1 = fetch_history(&conn, channel_id, None, 3, &pk).unwrap();
        assert_eq!(page1.len(), 3);
        // Newest first means ids[9], ids[8], ids[7].
        assert_eq!(page1[0].id, ids[9]);
        assert_eq!(page1[1].id, ids[8]);
        assert_eq!(page1[2].id, ids[7]);

        // Second page: 3 before ids[7].
        let page2 = fetch_history(&conn, channel_id, Some(ids[7]), 3, &pk).unwrap();
        assert_eq!(page2.len(), 3);
        assert_eq!(page2[0].id, ids[6]);
        assert_eq!(page2[1].id, ids[5]);
        assert_eq!(page2[2].id, ids[4]);
    }

    #[test]
    fn test_edit_message() {
        let (conn, channel_id, pk) = setup();
        let id = insert_message_with_ts(&conn, channel_id, &pk, "Original", None, 1_000).unwrap();

        edit_message(&conn, id, "Edited content").unwrap();

        let msg = get_message(&conn, id, &pk).unwrap().unwrap();
        assert_eq!(msg.content, "Edited content");
        assert!(msg.edited_at.is_some(), "edited_at should be set after edit");
    }

    #[test]
    fn test_delete_message() {
        let (conn, channel_id, pk) = setup();
        let id = insert_message_with_ts(&conn, channel_id, &pk, "To be deleted", None, 1_000).unwrap();

        delete_message(&conn, id).unwrap();

        let result = get_message(&conn, id, &pk).unwrap();
        assert!(result.is_none(), "message should not exist after deletion");
    }

    #[test]
    fn test_pin_unpin_message() {
        let (conn, channel_id, pk) = setup();
        let id = insert_message_with_ts(&conn, channel_id, &pk, "Pinnable", None, 1_000).unwrap();

        // Initially not pinned.
        let msg = get_message(&conn, id, &pk).unwrap().unwrap();
        assert!(!msg.pinned);

        // Pin it.
        pin_message(&conn, id).unwrap();
        let msg = get_message(&conn, id, &pk).unwrap().unwrap();
        assert!(msg.pinned);

        // Unpin it.
        unpin_message(&conn, id).unwrap();
        let msg = get_message(&conn, id, &pk).unwrap().unwrap();
        assert!(!msg.pinned);
    }

    #[test]
    fn test_fts5_search() {
        let (conn, channel_id, pk) = setup();

        // Insert messages with varying content.
        insert_message_with_ts(&conn, channel_id, &pk, "I love rust and systems programming", None, 1_000).unwrap();
        insert_message_with_ts(&conn, channel_id, &pk, "Python is great for scripting", None, 2_000).unwrap();
        insert_message_with_ts(&conn, channel_id, &pk, "rust and python are both popular languages", None, 3_000).unwrap();

        // Search for "rust" — should match messages 1 and 3.
        let results = search_messages(&conn, "rust", None, 50, &pk).unwrap();
        assert_eq!(results.len(), 2, "expected 2 results for 'rust'");

        // Search for "python" — should match messages 2 and 3.
        let results = search_messages(&conn, "python", None, 50, &pk).unwrap();
        assert_eq!(results.len(), 2, "expected 2 results for 'python'");

        // Search with channel filter.
        let other_channel_id = create_channel(&conn, "other", ChannelType::Text, None, 1).unwrap();
        let results = search_messages(&conn, "rust", Some(other_channel_id), 50, &pk).unwrap();
        assert_eq!(results.len(), 0, "no results from other channel");
    }

    #[test]
    fn test_delete_old_messages() {
        let (conn, channel_id, pk) = setup();

        // Insert messages at explicit timestamps.
        let _id1 = insert_message_with_ts(&conn, channel_id, &pk, "old message 1", None, 100).unwrap();
        let _id2 = insert_message_with_ts(&conn, channel_id, &pk, "old message 2", None, 200).unwrap();
        let id3 = insert_message_with_ts(&conn, channel_id, &pk, "new message 1", None, 1_000).unwrap();
        let id4 = insert_message_with_ts(&conn, channel_id, &pk, "new message 2", None, 2_000).unwrap();

        // Delete messages before timestamp 500 — should delete 2 messages.
        let deleted = delete_messages_before(&conn, channel_id, 500).unwrap();
        assert_eq!(deleted, 2, "should have deleted 2 old messages");

        // The newer messages should still exist.
        assert!(get_message(&conn, id3, &pk).unwrap().is_some());
        assert!(get_message(&conn, id4, &pk).unwrap().is_some());

        // The old messages should be gone.
        let remaining = fetch_history(&conn, channel_id, None, 100, &pk).unwrap();
        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn test_get_message_includes_attachments() {
        let (conn, ch_id, pk) = setup();
        let msg_id = insert_message(&conn, ch_id, &pk, "with attachment", None).unwrap();
        let hash = crate::attachments::compute_sha256(b"file data");
        let dir = std::env::temp_dir().join(format!("farder-msg-test-{}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_id = crate::attachments::store_file(
            &conn, &dir.to_string_lossy(), &pk, "photo.jpg", b"file data", &hash, "image/jpeg", None, None, None
        ).unwrap();
        crate::attachments::create_message_attachment(&conn, msg_id, file_id, 0, "photo.jpg", Some(800), Some(600), None).unwrap();
        let msg = get_message(&conn, msg_id, &pk).unwrap().unwrap();
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].name, "photo.jpg");
        assert_eq!(msg.attachments[0].width, Some(800));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_fetch_history_includes_attachments() {
        let (conn, ch_id, pk) = setup();
        let msg_id = insert_message(&conn, ch_id, &pk, "with file", None).unwrap();
        insert_message(&conn, ch_id, &pk, "no file", None).unwrap();
        let hash = crate::attachments::compute_sha256(b"data");
        let dir = std::env::temp_dir().join(format!("farder-hist-test-{}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_id = crate::attachments::store_file(
            &conn, &dir.to_string_lossy(), &pk, "doc.pdf", b"data", &hash, "application/pdf", None, None, None
        ).unwrap();
        crate::attachments::create_message_attachment(&conn, msg_id, file_id, 0, "doc.pdf", None, None, None).unwrap();
        let history = fetch_history(&conn, ch_id, None, 50, &pk).unwrap();
        assert_eq!(history.len(), 2);
        let with_attach = history.iter().find(|m| m.content == "with file").unwrap();
        let without = history.iter().find(|m| m.content == "no file").unwrap();
        assert_eq!(with_attach.attachments.len(), 1);
        assert!(without.attachments.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_delete_message_cleans_up_attachments() {
        let (conn, ch_id, pk) = setup();
        let msg_id = insert_message(&conn, ch_id, &pk, "will delete", None).unwrap();
        let dir = std::env::temp_dir().join(format!("farder-del-test-{}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        let hash = crate::attachments::compute_sha256(b"data");
        let file_id = crate::attachments::store_file(
            &conn, &dir.to_string_lossy(), &pk, "f.txt", b"data", &hash, "text/plain", None, None, None
        ).unwrap();
        crate::attachments::create_message_attachment(&conn, msg_id, file_id, 0, "f.txt", None, None, None).unwrap();
        assert_eq!(crate::attachments::get_file(&conn, file_id).unwrap().unwrap().ref_count, 1);

        let orphans = delete_message(&conn, msg_id).unwrap();
        assert!(orphans.contains(&file_id));
        assert_eq!(crate::attachments::get_file(&conn, file_id).unwrap().unwrap().ref_count, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_get_message_includes_reactions() {
        let (conn, ch_id, pk) = setup();
        let msg_id = insert_message(&conn, ch_id, &pk, "react", None).unwrap();
        crate::reactions::add_reaction(&conn, msg_id, &pk, "👍", None).unwrap();
        let msg = get_message(&conn, msg_id, &pk).unwrap().unwrap();
        assert_eq!(msg.reactions.len(), 1);
        assert_eq!(msg.reactions[0].emoji, "👍");
        assert!(msg.reactions[0].me);
    }

    #[test]
    fn test_get_message_includes_thread_metadata() {
        let (conn, ch_id, pk) = setup();
        let msg_id = insert_message(&conn, ch_id, &pk, "thread parent", None).unwrap();
        let thread_id = crate::channels::create_thread(&conn, "thread", ch_id, msg_id).unwrap();
        insert_message(&conn, thread_id, &pk, "reply 1", None).unwrap();
        insert_message(&conn, thread_id, &pk, "reply 2", None).unwrap();
        let msg = get_message(&conn, msg_id, &pk).unwrap().unwrap();
        assert_eq!(msg.thread_id, Some(thread_id));
        assert_eq!(msg.thread_message_count, Some(2));
    }

    #[test]
    fn test_fetch_history_includes_reactions() {
        let (conn, ch_id, pk) = setup();
        let msg_id = insert_message(&conn, ch_id, &pk, "msg", None).unwrap();
        crate::reactions::add_reaction(&conn, msg_id, &pk, "❤️", None).unwrap();
        let history = fetch_history(&conn, ch_id, None, 50, &pk).unwrap();
        assert_eq!(history[0].reactions.len(), 1);
    }

    #[test]
    fn test_anonymize_messages_by_author() {
        let conn = db::open_in_memory().unwrap();
        let channel_id = create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let alice = Keypair::generate().public_key();
        let bob = Keypair::generate().public_key();
        register_member(&conn, &alice, "Alice").unwrap();
        register_member(&conn, &bob, "Bob").unwrap();

        let a1 = insert_message_with_ts(&conn, channel_id, &alice, "Hello from Alice", None, 1000).unwrap();
        let a2 = insert_message_with_ts(&conn, channel_id, &alice, "Another Alice message", None, 2000).unwrap();
        let b1 = insert_message_with_ts(&conn, channel_id, &bob, "Bob says hi", None, 3000).unwrap();

        let count = anonymize_messages_by_author(&conn, &alice).unwrap();
        assert_eq!(count, 2, "should have anonymized 2 alice messages");

        // Alice's messages should have sentinel author and [deleted] content.
        let sentinel = farder_protocol::server::DELETED_USER_KEY;
        let check_a1: (Vec<u8>, String) = conn.query_row(
            "SELECT author, content FROM messages WHERE id = ?1",
            params![a1 as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(check_a1.0.as_slice(), sentinel.as_slice());
        assert_eq!(check_a1.1, "[deleted]");

        let check_a2: (Vec<u8>, String) = conn.query_row(
            "SELECT author, content FROM messages WHERE id = ?1",
            params![a2 as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(check_a2.0.as_slice(), sentinel.as_slice());
        assert_eq!(check_a2.1, "[deleted]");

        // Bob's message should be untouched.
        let check_b1: (Vec<u8>, String) = conn.query_row(
            "SELECT author, content FROM messages WHERE id = ?1",
            params![b1 as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(check_b1.0.as_slice(), bob.as_bytes().as_slice());
        assert_eq!(check_b1.1, "Bob says hi");
    }
}
