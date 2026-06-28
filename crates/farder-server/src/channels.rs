use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use farder_protocol::server::{CategoryInfo, ChannelInfo, ChannelType, OverrideInfo};

use crate::db::now;

pub fn channel_type_to_str(ct: &ChannelType) -> &'static str {
    match ct {
        ChannelType::Text => "text",
        ChannelType::Announcement => "announcement",
        ChannelType::Thread => "thread",
        ChannelType::Dm => "dm",
        ChannelType::Voice => "voice",
    }
}

pub fn str_to_channel_type(s: &str) -> ChannelType {
    match s {
        "announcement" => ChannelType::Announcement,
        "thread" => ChannelType::Thread,
        "dm" => ChannelType::Dm,
        "voice" => ChannelType::Voice,
        _ => ChannelType::Text,
    }
}

pub const CHANNEL_SELECT: &str =
    "id, name, channel_type, category_id, position, topic, nsfw, slow_mode_secs, retention_secs, thread_parent_message_id";

pub fn row_to_channel_info(row: &rusqlite::Row) -> rusqlite::Result<ChannelInfo> {
    let id: i64 = row.get(0)?;
    let name: String = row.get(1)?;
    let channel_type_str: String = row.get(2)?;
    let category_id: Option<i64> = row.get(3)?;
    let position: i64 = row.get(4)?;
    let topic: Option<String> = row.get(5)?;
    let nsfw: i64 = row.get(6)?;
    let slow_mode_secs: i64 = row.get(7)?;
    let retention_secs: Option<i64> = row.get(8)?;
    let thread_parent_message_id: Option<i64> = row.get(9)?;

    Ok(ChannelInfo {
        id: id as u64,
        name,
        channel_type: str_to_channel_type(&channel_type_str),
        category_id: category_id.map(|v| v as u64),
        position: position as u32,
        topic,
        nsfw: nsfw != 0,
        slow_mode_secs: slow_mode_secs as u32,
        retention_secs: retention_secs.map(|v| v as u64),
        thread_parent_message_id: thread_parent_message_id.map(|v| v as u64),
    })
}

// ---------------------------------------------------------------------------
// Category operations
// ---------------------------------------------------------------------------

pub fn create_category(conn: &Connection, name: &str, position: u32) -> Result<u64> {
    conn.execute(
        "INSERT INTO categories (name, position) VALUES (?1, ?2)",
        params![name, position as i64],
    )?;
    Ok(conn.last_insert_rowid() as u64)
}

pub fn get_category(conn: &Connection, id: u64) -> Result<Option<CategoryInfo>> {
    let row = conn
        .query_row(
            "SELECT id, name, position FROM categories WHERE id = ?1 AND deleted = 0",
            params![id as i64],
            |row| {
                let id: i64 = row.get(0)?;
                let name: String = row.get(1)?;
                let position: i64 = row.get(2)?;
                Ok(CategoryInfo {
                    id: id as u64,
                    name,
                    position: position as u32,
                })
            },
        )
        .optional()?;
    Ok(row)
}

pub fn list_categories(conn: &Connection) -> Result<Vec<CategoryInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, position FROM categories WHERE deleted = 0 ORDER BY position ASC",
    )?;

    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let name: String = row.get(1)?;
        let position: i64 = row.get(2)?;
        Ok(CategoryInfo {
            id: id as u64,
            name,
            position: position as u32,
        })
    })?;

    let mut categories = Vec::new();
    for row in rows {
        categories.push(row?);
    }
    Ok(categories)
}

pub fn update_category(
    conn: &Connection,
    id: u64,
    name: Option<&str>,
    position: Option<u32>,
) -> Result<()> {
    if name.is_none() && position.is_none() {
        return Ok(());
    }

    let mut sets: Vec<String> = Vec::new();
    let mut param_idx: usize = 2; // ?1 is the id

    if name.is_some() {
        sets.push(format!("name = ?{}", param_idx));
        param_idx += 1;
    }
    if position.is_some() {
        sets.push(format!("position = ?{}", param_idx));
    }

    let sql = format!("UPDATE categories SET {} WHERE id = ?1", sets.join(", "));
    let mut stmt = conn.prepare(&sql)?;

    let mut idx: usize = 1;
    stmt.raw_bind_parameter(idx, id as i64)?;
    idx += 1;

    if let Some(n) = name {
        stmt.raw_bind_parameter(idx, n)?;
        idx += 1;
    }
    if let Some(p) = position {
        stmt.raw_bind_parameter(idx, p as i64)?;
    }

    stmt.raw_execute()?;
    Ok(())
}

pub fn delete_category(conn: &Connection, id: u64) -> Result<()> {
    // Soft-delete the category.
    conn.execute(
        "UPDATE categories SET deleted = 1 WHERE id = ?1",
        params![id as i64],
    )?;
    // Unset category_id on channels that belonged to this category.
    conn.execute(
        "UPDATE channels SET category_id = NULL WHERE category_id = ?1",
        params![id as i64],
    )?;
    // Delete category overrides.
    conn.execute(
        "DELETE FROM category_overrides WHERE category_id = ?1",
        params![id as i64],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Channel operations
// ---------------------------------------------------------------------------

pub fn create_channel(
    conn: &Connection,
    name: &str,
    channel_type: ChannelType,
    category_id: Option<u64>,
    position: u32,
) -> Result<u64> {
    conn.execute(
        "INSERT INTO channels (name, channel_type, category_id, position) VALUES (?1, ?2, ?3, ?4)",
        params![
            name,
            channel_type_to_str(&channel_type),
            category_id.map(|v| v as i64),
            position as i64,
        ],
    )?;
    Ok(conn.last_insert_rowid() as u64)
}

/// Create a thread channel linked to a specific parent message.
/// The thread inherits the parent channel's category_id and gets position=0.
pub fn create_thread(
    conn: &Connection,
    name: &str,
    parent_channel_id: u64,
    parent_message_id: u64,
) -> Result<u64> {
    // Retrieve the parent channel to inherit category_id.
    let category_id: Option<i64> = conn
        .query_row(
            "SELECT category_id FROM channels WHERE id = ?1",
            params![parent_channel_id as i64],
            |row| row.get(0),
        )
        .optional()?
        .flatten();

    conn.execute(
        "INSERT INTO channels \
         (name, channel_type, category_id, position, thread_parent_message_id) \
         VALUES (?1, 'thread', ?2, 0, ?3)",
        params![name, category_id, parent_message_id as i64],
    )?;
    Ok(conn.last_insert_rowid() as u64)
}

/// Get the thread channel for a given parent message, if one exists.
pub fn get_thread_for_message(
    conn: &Connection,
    message_id: u64,
) -> Result<Option<ChannelInfo>> {
    let sql = format!(
        "SELECT {} FROM channels \
         WHERE thread_parent_message_id = ?1 AND deleted = 0",
        CHANNEL_SELECT
    );
    let row = conn
        .query_row(&sql, params![message_id as i64], row_to_channel_info)
        .optional()?;
    Ok(row)
}

pub fn get_channel(conn: &Connection, id: u64) -> Result<Option<ChannelInfo>> {
    let sql = format!(
        "SELECT {} FROM channels WHERE id = ?1 AND deleted = 0",
        CHANNEL_SELECT
    );
    let row = conn
        .query_row(&sql, params![id as i64], row_to_channel_info)
        .optional()?;
    Ok(row)
}

pub fn get_channel_including_deleted(conn: &Connection, id: u64) -> Result<Option<ChannelInfo>> {
    let sql = format!("SELECT {} FROM channels WHERE id = ?1", CHANNEL_SELECT);
    let row = conn
        .query_row(&sql, params![id as i64], row_to_channel_info)
        .optional()?;
    Ok(row)
}

pub fn list_channels(conn: &Connection) -> Result<Vec<ChannelInfo>> {
    let sql = format!(
        "SELECT {} FROM channels WHERE deleted = 0 AND channel_type != 'thread' AND channel_type != 'dm' ORDER BY position ASC",
        CHANNEL_SELECT
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_channel_info)?;
    let mut channels = Vec::new();
    for row in rows {
        channels.push(row?);
    }
    Ok(channels)
}

#[allow(clippy::too_many_arguments)]
pub fn update_channel(
    conn: &Connection,
    id: u64,
    name: Option<&str>,
    topic: Option<&str>,
    nsfw: Option<bool>,
    slow_mode_secs: Option<u32>,
    retention_secs: Option<Option<u64>>,
    category_id: Option<Option<u64>>,
    position: Option<u32>,
) -> Result<()> {
    if name.is_none()
        && topic.is_none()
        && nsfw.is_none()
        && slow_mode_secs.is_none()
        && retention_secs.is_none()
        && category_id.is_none()
        && position.is_none()
    {
        return Ok(());
    }

    let mut sets: Vec<String> = Vec::new();
    let mut param_idx: usize = 2; // ?1 is the id

    if name.is_some() {
        sets.push(format!("name = ?{}", param_idx));
        param_idx += 1;
    }
    if topic.is_some() {
        sets.push(format!("topic = ?{}", param_idx));
        param_idx += 1;
    }
    if nsfw.is_some() {
        sets.push(format!("nsfw = ?{}", param_idx));
        param_idx += 1;
    }
    if slow_mode_secs.is_some() {
        sets.push(format!("slow_mode_secs = ?{}", param_idx));
        param_idx += 1;
    }
    if retention_secs.is_some() {
        sets.push(format!("retention_secs = ?{}", param_idx));
        param_idx += 1;
    }
    if category_id.is_some() {
        sets.push(format!("category_id = ?{}", param_idx));
        param_idx += 1;
    }
    if position.is_some() {
        sets.push(format!("position = ?{}", param_idx));
    }

    let sql = format!("UPDATE channels SET {} WHERE id = ?1", sets.join(", "));
    let mut stmt = conn.prepare(&sql)?;

    let mut idx: usize = 1;
    stmt.raw_bind_parameter(idx, id as i64)?;
    idx += 1;

    if let Some(n) = name {
        stmt.raw_bind_parameter(idx, n)?;
        idx += 1;
    }
    if let Some(t) = topic {
        stmt.raw_bind_parameter(idx, t)?;
        idx += 1;
    }
    if let Some(n) = nsfw {
        stmt.raw_bind_parameter(idx, n as i64)?;
        idx += 1;
    }
    if let Some(s) = slow_mode_secs {
        stmt.raw_bind_parameter(idx, s as i64)?;
        idx += 1;
    }
    if let Some(r) = retention_secs {
        match r {
            Some(v) => stmt.raw_bind_parameter(idx, v as i64)?,
            None => stmt.raw_bind_parameter(idx, rusqlite::types::Null)?,
        }
        idx += 1;
    }
    if let Some(cat) = category_id {
        match cat {
            Some(v) => stmt.raw_bind_parameter(idx, v as i64)?,
            None => stmt.raw_bind_parameter(idx, rusqlite::types::Null)?,
        }
        idx += 1;
    }
    if let Some(p) = position {
        stmt.raw_bind_parameter(idx, p as i64)?;
    }

    stmt.raw_execute()?;
    Ok(())
}

pub fn soft_delete_channel(conn: &Connection, id: u64) -> Result<()> {
    conn.execute(
        "UPDATE channels SET deleted = 1, deleted_at = ?2 WHERE id = ?1",
        params![id as i64, now() as i64],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// DM channel operations
// ---------------------------------------------------------------------------

/// Create a new DM channel between two users, insert both dm_participants rows,
/// and return the new channel id.
pub fn create_dm_channel(conn: &Connection, user_a: &farder_crypto::identity::PublicKey, user_b: &farder_crypto::identity::PublicKey) -> Result<u64> {
    conn.execute(
        "INSERT INTO channels (name, channel_type, position) VALUES ('', 'dm', 0)",
        [],
    )?;
    let channel_id = conn.last_insert_rowid() as u64;
    conn.execute(
        "INSERT INTO dm_participants (channel_id, user_key) VALUES (?1, ?2)",
        rusqlite::params![channel_id as i64, user_a.as_bytes().as_slice()],
    )?;
    conn.execute(
        "INSERT INTO dm_participants (channel_id, user_key) VALUES (?1, ?2)",
        rusqlite::params![channel_id as i64, user_b.as_bytes().as_slice()],
    )?;
    Ok(channel_id)
}

/// Find an existing DM channel between two users. Returns None if no such channel exists.
pub fn find_dm_channel(conn: &Connection, user_a: &farder_crypto::identity::PublicKey, user_b: &farder_crypto::identity::PublicKey) -> Result<Option<ChannelInfo>> {
    let sql = format!(
        "SELECT {} FROM channels WHERE id IN (\
            SELECT channel_id FROM dm_participants WHERE user_key = ?1 \
            INTERSECT \
            SELECT channel_id FROM dm_participants WHERE user_key = ?2\
        ) AND deleted = 0 LIMIT 1",
        CHANNEL_SELECT
    );
    let row = conn
        .query_row(&sql, rusqlite::params![user_a.as_bytes().as_slice(), user_b.as_bytes().as_slice()], row_to_channel_info)
        .optional()?;
    Ok(row)
}

/// Find or create a DM channel between two users.
/// Returns (channel_id, was_created).
pub fn open_dm_channel(conn: &Connection, user_a: &farder_crypto::identity::PublicKey, user_b: &farder_crypto::identity::PublicKey) -> Result<(u64, bool)> {
    if let Some(ch) = find_dm_channel(conn, user_a, user_b)? {
        return Ok((ch.id, false));
    }
    let id = create_dm_channel(conn, user_a, user_b)?;
    Ok((id, true))
}

/// List all DM channels for a user, returning (ChannelInfo, other_participant_key) pairs.
pub fn list_dm_channels(conn: &Connection, user: &farder_crypto::identity::PublicKey) -> Result<Vec<(ChannelInfo, farder_crypto::identity::PublicKey)>> {
    // Find all channel_ids the user participates in.
    let channel_sql = format!(
        "SELECT {} FROM channels WHERE id IN (\
            SELECT channel_id FROM dm_participants WHERE user_key = ?1\
        ) AND deleted = 0",
        CHANNEL_SELECT
    );
    let mut stmt = conn.prepare(&channel_sql)?;
    let channel_rows = stmt.query_map(rusqlite::params![user.as_bytes().as_slice()], row_to_channel_info)?;
    let mut channels = Vec::new();
    for row in channel_rows {
        channels.push(row?);
    }

    let mut result = Vec::new();
    for ch in channels {
        // Find the other participant.
        let other_bytes: Option<Vec<u8>> = conn.query_row(
            "SELECT user_key FROM dm_participants WHERE channel_id = ?1 AND user_key != ?2 LIMIT 1",
            rusqlite::params![ch.id as i64, user.as_bytes().as_slice()],
            |row| row.get(0),
        ).optional()?;

        if let Some(bytes) = other_bytes {
            let arr: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("user_key blob wrong length"))?;
            let other_key = farder_crypto::identity::PublicKey::from_bytes(arr);
            result.push((ch, other_key));
        }
    }
    Ok(result)
}

/// Check whether `user` is a participant in the given DM channel.
pub fn is_dm_participant(conn: &Connection, channel_id: u64, user: &farder_crypto::identity::PublicKey) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM dm_participants WHERE channel_id = ?1 AND user_key = ?2",
        rusqlite::params![channel_id as i64, user.as_bytes().as_slice()],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub fn hard_delete_channel(conn: &Connection, id: u64) -> Result<()> {
    conn.execute(
        "DELETE FROM channel_overrides WHERE channel_id = ?1",
        params![id as i64],
    )?;
    // Clean up FTS5 entries before deleting messages.
    let mut stmt = conn.prepare("SELECT id, content FROM messages WHERE channel_id = ?1")?;
    let rows: Vec<(i64, String)> = stmt
        .query_map(params![id as i64], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // Delete attachments for all messages in the channel.
    for (msg_id, _) in &rows {
        crate::reactions::delete_reactions_for_message(conn, *msg_id as u64)?;
        let _ = crate::attachments::delete_attachments_for_message(conn, *msg_id as u64)?;
    }

    for (msg_id, content) in &rows {
        conn.execute(
            "INSERT INTO messages_fts(messages_fts, rowid, content) VALUES ('delete', ?1, ?2)",
            params![msg_id, content],
        )?;
    }
    conn.execute(
        "DELETE FROM messages WHERE channel_id = ?1",
        params![id as i64],
    )?;
    conn.execute(
        "DELETE FROM channels WHERE id = ?1",
        params![id as i64],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Override operations
// ---------------------------------------------------------------------------

pub fn set_channel_override(
    conn: &Connection,
    channel_id: u64,
    role_id: u64,
    allow: u64,
    deny: u64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO channel_overrides (channel_id, role_id, allow_bits, deny_bits) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(channel_id, role_id) DO UPDATE SET allow_bits = ?3, deny_bits = ?4",
        params![channel_id as i64, role_id as i64, allow as i64, deny as i64],
    )?;
    Ok(())
}

pub fn get_channel_overrides(conn: &Connection, channel_id: u64) -> Result<Vec<OverrideInfo>> {
    let mut stmt = conn.prepare(
        "SELECT role_id, allow_bits, deny_bits FROM channel_overrides WHERE channel_id = ?1",
    )?;
    let rows = stmt.query_map(params![channel_id as i64], |row| {
        let role_id: i64 = row.get(0)?;
        let allow: i64 = row.get(1)?;
        let deny: i64 = row.get(2)?;
        Ok(OverrideInfo {
            role_id: role_id as u64,
            allow: allow as u64,
            deny: deny as u64,
        })
    })?;
    let mut overrides = Vec::new();
    for row in rows {
        overrides.push(row?);
    }
    Ok(overrides)
}

pub fn set_category_override(
    conn: &Connection,
    category_id: u64,
    role_id: u64,
    allow: u64,
    deny: u64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO category_overrides (category_id, role_id, allow_bits, deny_bits) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(category_id, role_id) DO UPDATE SET allow_bits = ?3, deny_bits = ?4",
        params![category_id as i64, role_id as i64, allow as i64, deny as i64],
    )?;
    Ok(())
}

pub fn get_category_overrides(conn: &Connection, category_id: u64) -> Result<Vec<OverrideInfo>> {
    let mut stmt = conn.prepare(
        "SELECT role_id, allow_bits, deny_bits FROM category_overrides WHERE category_id = ?1",
    )?;
    let rows = stmt.query_map(params![category_id as i64], |row| {
        let role_id: i64 = row.get(0)?;
        let allow: i64 = row.get(1)?;
        let deny: i64 = row.get(2)?;
        Ok(OverrideInfo {
            role_id: role_id as u64,
            allow: allow as u64,
            deny: deny as u64,
        })
    })?;
    let mut overrides = Vec::new();
    for row in rows {
        overrides.push(row?);
    }
    Ok(overrides)
}

pub fn get_channel_overrides_for_roles(
    conn: &Connection,
    channel_id: u64,
    role_ids: &[u64],
) -> Result<Vec<OverrideInfo>> {
    if role_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders: Vec<String> = role_ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 2))
        .collect();
    let sql = format!(
        "SELECT role_id, allow_bits, deny_bits FROM channel_overrides \
         WHERE channel_id = ?1 AND role_id IN ({})",
        placeholders.join(",")
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(channel_id as i64));
    for id in role_ids {
        param_values.push(Box::new(*id as i64));
    }
    let params_ref: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();

    let rows = stmt.query_map(params_ref.as_slice(), |row| {
        let role_id: i64 = row.get(0)?;
        let allow: i64 = row.get(1)?;
        let deny: i64 = row.get(2)?;
        Ok(OverrideInfo {
            role_id: role_id as u64,
            allow: allow as u64,
            deny: deny as u64,
        })
    })?;

    let mut overrides = Vec::new();
    for row in rows {
        overrides.push(row?);
    }
    Ok(overrides)
}

pub fn get_category_overrides_for_roles(
    conn: &Connection,
    category_id: u64,
    role_ids: &[u64],
) -> Result<Vec<OverrideInfo>> {
    if role_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders: Vec<String> = role_ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 2))
        .collect();
    let sql = format!(
        "SELECT role_id, allow_bits, deny_bits FROM category_overrides \
         WHERE category_id = ?1 AND role_id IN ({})",
        placeholders.join(",")
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(category_id as i64));
    for id in role_ids {
        param_values.push(Box::new(*id as i64));
    }
    let params_ref: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();

    let rows = stmt.query_map(params_ref.as_slice(), |row| {
        let role_id: i64 = row.get(0)?;
        let allow: i64 = row.get(1)?;
        let deny: i64 = row.get(2)?;
        Ok(OverrideInfo {
            role_id: role_id as u64,
            allow: allow as u64,
            deny: deny as u64,
        })
    })?;

    let mut overrides = Vec::new();
    for row in rows {
        overrides.push(row?);
    }
    Ok(overrides)
}

// ---------------------------------------------------------------------------
// Voice state operations
// ---------------------------------------------------------------------------

pub fn join_voice(conn: &Connection, channel_id: u64, user_key: &farder_crypto::identity::PublicKey) -> Result<()> {
    // First, leave any existing voice channel
    leave_all_voice(conn, user_key)?;
    // Then join
    conn.execute(
        "INSERT OR REPLACE INTO voice_state (channel_id, user_key, joined_at) VALUES (?1, ?2, ?3)",
        params![channel_id as i64, user_key.as_bytes().as_slice(), crate::db::now() as i64],
    )?;
    Ok(())
}

pub fn leave_voice(conn: &Connection, channel_id: u64, user_key: &farder_crypto::identity::PublicKey) -> Result<()> {
    conn.execute(
        "DELETE FROM voice_state WHERE channel_id = ?1 AND user_key = ?2",
        params![channel_id as i64, user_key.as_bytes().as_slice()],
    )?;
    Ok(())
}

pub fn leave_all_voice(conn: &Connection, user_key: &farder_crypto::identity::PublicKey) -> Result<Vec<u64>> {
    // Returns channel_ids the user was in (removes all voice_state rows for this user)
    let mut stmt = conn.prepare("SELECT channel_id FROM voice_state WHERE user_key = ?1")?;
    let ids: Vec<u64> = stmt.query_map(params![user_key.as_bytes().as_slice()], |row| {
        Ok(row.get::<_, i64>(0)? as u64)
    })?.filter_map(|r| r.ok()).collect();
    conn.execute(
        "DELETE FROM voice_state WHERE user_key = ?1",
        params![user_key.as_bytes().as_slice()],
    )?;
    Ok(ids)
}

pub fn get_voice_participants(conn: &Connection, channel_id: u64) -> Result<Vec<(farder_crypto::identity::PublicKey, u64)>> {
    let mut stmt = conn.prepare(
        "SELECT user_key, joined_at FROM voice_state WHERE channel_id = ?1 ORDER BY joined_at ASC"
    )?;
    let rows = stmt.query_map(params![channel_id as i64], |row| {
        let key_bytes: Vec<u8> = row.get(0)?;
        let joined_at: i64 = row.get(1)?;
        Ok((key_bytes, joined_at as u64))
    })?;
    let mut result = Vec::new();
    for row in rows {
        let (key_bytes, joined_at) = row?;
        let arr: [u8; 32] = key_bytes.try_into().map_err(|_| anyhow::anyhow!("bad key"))?;
        result.push((farder_crypto::identity::PublicKey::from_bytes(arr), joined_at));
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::members::create_role;

    // -----------------------------------------------------------------------
    // Category tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_create_and_get_category() {
        let conn = db::open_in_memory().unwrap();
        let id = create_category(&conn, "General", 0).unwrap();
        let cat = get_category(&conn, id).unwrap().expect("category should exist");
        assert_eq!(cat.id, id);
        assert_eq!(cat.name, "General");
        assert_eq!(cat.position, 0);
    }

    #[test]
    fn test_list_categories() {
        let conn = db::open_in_memory().unwrap();
        create_category(&conn, "C", 2).unwrap();
        create_category(&conn, "A", 0).unwrap();
        create_category(&conn, "B", 1).unwrap();

        let cats = list_categories(&conn).unwrap();
        assert_eq!(cats.len(), 3);
        assert_eq!(cats[0].name, "A");
        assert_eq!(cats[1].name, "B");
        assert_eq!(cats[2].name, "C");
    }

    #[test]
    fn test_update_category() {
        let conn = db::open_in_memory().unwrap();
        let id = create_category(&conn, "Old Name", 5).unwrap();

        update_category(&conn, id, Some("New Name"), None).unwrap();
        let cat = get_category(&conn, id).unwrap().unwrap();
        assert_eq!(cat.name, "New Name");
        assert_eq!(cat.position, 5); // unchanged

        update_category(&conn, id, None, Some(10)).unwrap();
        let cat = get_category(&conn, id).unwrap().unwrap();
        assert_eq!(cat.position, 10);
        assert_eq!(cat.name, "New Name"); // unchanged
    }

    #[test]
    fn test_delete_category() {
        let conn = db::open_in_memory().unwrap();
        let cat_id = create_category(&conn, "ToDelete", 0).unwrap();
        let ch_id = create_channel(&conn, "general", ChannelType::Text, Some(cat_id), 0).unwrap();

        // Add a category override so we can verify deletion.
        let role_id = create_role(&conn, "Mod", 0xFF, None, 1, false).unwrap();
        set_category_override(&conn, cat_id, role_id, 0x01, 0x02).unwrap();

        delete_category(&conn, cat_id).unwrap();

        // Category no longer visible.
        assert!(get_category(&conn, cat_id).unwrap().is_none());
        // Overrides deleted.
        let overrides = get_category_overrides(&conn, cat_id).unwrap();
        assert!(overrides.is_empty());
        // Channel still exists but category_id cleared.
        let ch = get_channel(&conn, ch_id).unwrap().unwrap();
        assert!(ch.category_id.is_none());
    }

    // -----------------------------------------------------------------------
    // Channel tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_create_and_get_channel() {
        let conn = db::open_in_memory().unwrap();
        let id = create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let ch = get_channel(&conn, id).unwrap().expect("channel should exist");
        assert_eq!(ch.id, id);
        assert_eq!(ch.name, "general");
        assert_eq!(ch.channel_type, ChannelType::Text);
        assert!(ch.category_id.is_none());
        assert_eq!(ch.position, 0);
        assert!(!ch.nsfw);
        assert_eq!(ch.slow_mode_secs, 0);
        assert!(ch.retention_secs.is_none());
    }

    #[test]
    fn test_create_announcement_channel() {
        let conn = db::open_in_memory().unwrap();
        let id = create_channel(&conn, "announcements", ChannelType::Announcement, None, 1).unwrap();
        let ch = get_channel(&conn, id).unwrap().unwrap();
        assert_eq!(ch.channel_type, ChannelType::Announcement);
    }

    #[test]
    fn test_list_channels() {
        let conn = db::open_in_memory().unwrap();
        create_channel(&conn, "c", ChannelType::Text, None, 2).unwrap();
        create_channel(&conn, "a", ChannelType::Text, None, 0).unwrap();
        create_channel(&conn, "b", ChannelType::Text, None, 1).unwrap();

        let channels = list_channels(&conn).unwrap();
        assert_eq!(channels.len(), 3);
        assert_eq!(channels[0].name, "a");
        assert_eq!(channels[1].name, "b");
        assert_eq!(channels[2].name, "c");
    }

    #[test]
    fn test_update_channel() {
        let conn = db::open_in_memory().unwrap();
        let id = create_channel(&conn, "chat", ChannelType::Text, None, 0).unwrap();

        update_channel(&conn, id, Some("renamed"), Some("A topic"), Some(true), Some(5), None, None, None).unwrap();
        let ch = get_channel(&conn, id).unwrap().unwrap();
        assert_eq!(ch.name, "renamed");
        assert_eq!(ch.topic, Some("A topic".to_string()));
        assert!(ch.nsfw);
        assert_eq!(ch.slow_mode_secs, 5);
        assert!(ch.retention_secs.is_none());

        // Now set retention_secs.
        update_channel(&conn, id, None, None, None, None, Some(Some(3600)), None, None).unwrap();
        let ch = get_channel(&conn, id).unwrap().unwrap();
        assert_eq!(ch.retention_secs, Some(3600));

        // Clear retention_secs.
        update_channel(&conn, id, None, None, None, None, Some(None), None, None).unwrap();
        let ch = get_channel(&conn, id).unwrap().unwrap();
        assert!(ch.retention_secs.is_none());
    }

    #[test]
    fn test_soft_delete_channel() {
        let conn = db::open_in_memory().unwrap();
        let id = create_channel(&conn, "temp", ChannelType::Text, None, 0).unwrap();

        soft_delete_channel(&conn, id).unwrap();

        // Does not appear in get_channel or list_channels.
        assert!(get_channel(&conn, id).unwrap().is_none());
        let channels = list_channels(&conn).unwrap();
        assert!(channels.iter().all(|ch| ch.id != id));

        // Still retrievable via get_channel_including_deleted.
        let ch = get_channel_including_deleted(&conn, id).unwrap();
        assert!(ch.is_some());
    }

    #[test]
    fn test_hard_delete_channel() {
        let conn = db::open_in_memory().unwrap();
        let id = create_channel(&conn, "gone", ChannelType::Text, None, 0).unwrap();

        // Add an override to ensure cascade delete.
        let role_id = create_role(&conn, "Mod", 0xFF, None, 1, false).unwrap();
        set_channel_override(&conn, id, role_id, 0x01, 0x02).unwrap();

        hard_delete_channel(&conn, id).unwrap();

        // Gone completely.
        assert!(get_channel_including_deleted(&conn, id).unwrap().is_none());
        // Overrides also gone.
        let overrides = get_channel_overrides(&conn, id).unwrap();
        assert!(overrides.is_empty());
    }

    // -----------------------------------------------------------------------
    // Override tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_set_and_get_channel_overrides() {
        let conn = db::open_in_memory().unwrap();
        let ch_id = create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let role_id = create_role(&conn, "Mod", 0xFF, None, 1, false).unwrap();

        set_channel_override(&conn, ch_id, role_id, 0xAA, 0xBB).unwrap();

        let overrides = get_channel_overrides(&conn, ch_id).unwrap();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].role_id, role_id);
        assert_eq!(overrides[0].allow, 0xAA);
        assert_eq!(overrides[0].deny, 0xBB);
    }

    #[test]
    fn test_set_and_get_category_overrides() {
        let conn = db::open_in_memory().unwrap();
        let cat_id = create_category(&conn, "General", 0).unwrap();
        let role_id = create_role(&conn, "Mod", 0xFF, None, 1, false).unwrap();

        set_category_override(&conn, cat_id, role_id, 0x11, 0x22).unwrap();

        let overrides = get_category_overrides(&conn, cat_id).unwrap();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].role_id, role_id);
        assert_eq!(overrides[0].allow, 0x11);
        assert_eq!(overrides[0].deny, 0x22);
    }

    #[test]
    fn test_override_upsert() {
        let conn = db::open_in_memory().unwrap();
        let ch_id = create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let role_id = create_role(&conn, "Mod", 0xFF, None, 1, false).unwrap();

        // Set once.
        set_channel_override(&conn, ch_id, role_id, 0x01, 0x02).unwrap();
        // Set again with different values — should update, not insert a second row.
        set_channel_override(&conn, ch_id, role_id, 0xFF, 0x00).unwrap();

        let overrides = get_channel_overrides(&conn, ch_id).unwrap();
        assert_eq!(overrides.len(), 1, "should have exactly 1 row after upsert");
        assert_eq!(overrides[0].allow, 0xFF);
        assert_eq!(overrides[0].deny, 0x00);
    }

    #[test]
    fn test_get_channel_overrides_for_roles() {
        let conn = db::open_in_memory().unwrap();
        let ch_id = create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let r1 = create_role(&conn, "Mod", 0xFF, None, 1, false).unwrap();
        let r2 = create_role(&conn, "VIP", 0x0F, None, 2, false).unwrap();
        let r3 = create_role(&conn, "Other", 0x01, None, 3, false).unwrap();

        set_channel_override(&conn, ch_id, r1, 0xAA, 0x00).unwrap();
        set_channel_override(&conn, ch_id, r2, 0xBB, 0x00).unwrap();
        set_channel_override(&conn, ch_id, r3, 0xCC, 0x00).unwrap();

        // Ask for r1 and r2 only.
        let overrides = get_channel_overrides_for_roles(&conn, ch_id, &[r1, r2]).unwrap();
        assert_eq!(overrides.len(), 2);
        let allows: Vec<u64> = overrides.iter().map(|o| o.allow).collect();
        assert!(allows.contains(&0xAA));
        assert!(allows.contains(&0xBB));

        // Empty role_ids returns empty vec.
        let empty = get_channel_overrides_for_roles(&conn, ch_id, &[]).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn test_get_category_overrides_for_roles() {
        let conn = db::open_in_memory().unwrap();
        let cat_id = create_category(&conn, "General", 0).unwrap();
        let r1 = create_role(&conn, "Mod", 0xFF, None, 1, false).unwrap();
        let r2 = create_role(&conn, "VIP", 0x0F, None, 2, false).unwrap();
        let r3 = create_role(&conn, "Other", 0x01, None, 3, false).unwrap();

        set_category_override(&conn, cat_id, r1, 0x11, 0x00).unwrap();
        set_category_override(&conn, cat_id, r2, 0x22, 0x00).unwrap();
        set_category_override(&conn, cat_id, r3, 0x33, 0x00).unwrap();

        let overrides = get_category_overrides_for_roles(&conn, cat_id, &[r1, r3]).unwrap();
        assert_eq!(overrides.len(), 2);
        let allows: Vec<u64> = overrides.iter().map(|o| o.allow).collect();
        assert!(allows.contains(&0x11));
        assert!(allows.contains(&0x33));

        // Empty role_ids returns empty vec.
        let empty = get_category_overrides_for_roles(&conn, cat_id, &[]).unwrap();
        assert!(empty.is_empty());
    }

    // -----------------------------------------------------------------------
    // Thread channel tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_create_thread_channel() {
        use crate::messages::insert_message;
        use crate::members::register_member;
        use farder_crypto::identity::Keypair;

        let conn = db::open_in_memory().unwrap();
        let pk = Keypair::generate().public_key();
        register_member(&conn, &pk, "Alice").unwrap();

        let cat_id = create_category(&conn, "General", 0).unwrap();
        let parent_id = create_channel(&conn, "chat", ChannelType::Text, Some(cat_id), 0).unwrap();
        let msg_id = insert_message(&conn, parent_id, &pk, "start a thread", None).unwrap();

        let thread_id = create_thread(&conn, "my thread", parent_id, msg_id).unwrap();

        let thread = get_channel(&conn, thread_id).unwrap().expect("thread channel should exist");
        assert_eq!(thread.id, thread_id);
        assert_eq!(thread.name, "my thread");
        assert_eq!(thread.channel_type, ChannelType::Thread);
        assert_eq!(thread.category_id, Some(cat_id));
        assert_eq!(thread.thread_parent_message_id, Some(msg_id));
    }

    #[test]
    fn test_get_thread_for_message() {
        use crate::messages::insert_message;
        use crate::members::register_member;
        use farder_crypto::identity::Keypair;

        let conn = db::open_in_memory().unwrap();
        let pk = Keypair::generate().public_key();
        register_member(&conn, &pk, "Alice").unwrap();

        let parent_id = create_channel(&conn, "chat", ChannelType::Text, None, 0).unwrap();
        let msg_id = insert_message(&conn, parent_id, &pk, "start a thread", None).unwrap();
        let other_msg_id = insert_message(&conn, parent_id, &pk, "another", None).unwrap();

        // No thread yet.
        assert!(get_thread_for_message(&conn, msg_id).unwrap().is_none());

        create_thread(&conn, "thread-1", parent_id, msg_id).unwrap();

        // Now found.
        let thread = get_thread_for_message(&conn, msg_id)
            .unwrap()
            .expect("should find thread");
        assert_eq!(thread.thread_parent_message_id, Some(msg_id));
        assert_eq!(thread.channel_type, ChannelType::Thread);

        // Other message still has no thread.
        assert!(get_thread_for_message(&conn, other_msg_id).unwrap().is_none());
    }

    #[test]
    fn test_list_channels_excludes_threads() {
        use crate::messages::insert_message;
        use crate::members::register_member;
        use farder_crypto::identity::Keypair;

        let conn = db::open_in_memory().unwrap();
        let pk = Keypair::generate().public_key();
        register_member(&conn, &pk, "Alice").unwrap();

        let ch_id = create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let msg_id = insert_message(&conn, ch_id, &pk, "hello", None).unwrap();
        create_thread(&conn, "my-thread", ch_id, msg_id).unwrap();

        let channels = list_channels(&conn).unwrap();
        assert_eq!(channels.len(), 1, "list_channels should exclude thread channels");
        assert_eq!(channels[0].name, "general");
    }

    // -----------------------------------------------------------------------
    // DM channel tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_create_dm_channel() {
        use farder_crypto::identity::Keypair;

        let conn = db::open_in_memory().unwrap();
        let pk_a = Keypair::generate().public_key();
        let pk_b = Keypair::generate().public_key();

        let ch_id = create_dm_channel(&conn, &pk_a, &pk_b).unwrap();
        let ch = get_channel(&conn, ch_id).unwrap().expect("DM channel should exist");
        assert_eq!(ch.channel_type, ChannelType::Dm);

        // Both participants should be present.
        assert!(is_dm_participant(&conn, ch_id, &pk_a).unwrap());
        assert!(is_dm_participant(&conn, ch_id, &pk_b).unwrap());
    }

    #[test]
    fn test_find_dm_channel() {
        use farder_crypto::identity::Keypair;

        let conn = db::open_in_memory().unwrap();
        let pk_a = Keypair::generate().public_key();
        let pk_b = Keypair::generate().public_key();
        let pk_c = Keypair::generate().public_key();

        // No channel yet.
        assert!(find_dm_channel(&conn, &pk_a, &pk_b).unwrap().is_none());

        let ch_id = create_dm_channel(&conn, &pk_a, &pk_b).unwrap();

        // Found in both directions.
        let found_ab = find_dm_channel(&conn, &pk_a, &pk_b).unwrap().expect("should find a-b");
        assert_eq!(found_ab.id, ch_id);
        let found_ba = find_dm_channel(&conn, &pk_b, &pk_a).unwrap().expect("should find b-a");
        assert_eq!(found_ba.id, ch_id);

        // Different pair returns None.
        assert!(find_dm_channel(&conn, &pk_a, &pk_c).unwrap().is_none());
    }

    #[test]
    fn test_open_dm_channel_idempotent() {
        use farder_crypto::identity::Keypair;

        let conn = db::open_in_memory().unwrap();
        let pk_a = Keypair::generate().public_key();
        let pk_b = Keypair::generate().public_key();

        let (id1, created1) = open_dm_channel(&conn, &pk_a, &pk_b).unwrap();
        assert!(created1, "first open should create");

        let (id2, created2) = open_dm_channel(&conn, &pk_a, &pk_b).unwrap();
        assert!(!created2, "second open should not create");
        assert_eq!(id1, id2, "should return same channel id");

        // Also idempotent when called in reverse order.
        let (id3, created3) = open_dm_channel(&conn, &pk_b, &pk_a).unwrap();
        assert!(!created3);
        assert_eq!(id1, id3);
    }

    #[test]
    fn test_list_dm_channels() {
        use farder_crypto::identity::Keypair;

        let conn = db::open_in_memory().unwrap();
        let pk_a = Keypair::generate().public_key();
        let pk_b = Keypair::generate().public_key();
        let pk_c = Keypair::generate().public_key();

        create_dm_channel(&conn, &pk_a, &pk_b).unwrap();
        create_dm_channel(&conn, &pk_a, &pk_c).unwrap();

        let dms_a = list_dm_channels(&conn, &pk_a).unwrap();
        assert_eq!(dms_a.len(), 2);

        // Each entry's other key should be b or c.
        let other_keys: Vec<_> = dms_a.iter().map(|(_, k)| k.clone()).collect();
        assert!(other_keys.contains(&pk_b));
        assert!(other_keys.contains(&pk_c));

        // b only has one DM (with a).
        let dms_b = list_dm_channels(&conn, &pk_b).unwrap();
        assert_eq!(dms_b.len(), 1);
        assert_eq!(dms_b[0].1, pk_a);
    }

    #[test]
    fn test_is_dm_participant() {
        use farder_crypto::identity::Keypair;

        let conn = db::open_in_memory().unwrap();
        let pk_a = Keypair::generate().public_key();
        let pk_b = Keypair::generate().public_key();
        let pk_c = Keypair::generate().public_key();

        let ch_id = create_dm_channel(&conn, &pk_a, &pk_b).unwrap();

        assert!(is_dm_participant(&conn, ch_id, &pk_a).unwrap());
        assert!(is_dm_participant(&conn, ch_id, &pk_b).unwrap());
        assert!(!is_dm_participant(&conn, ch_id, &pk_c).unwrap());
    }

    #[test]
    fn test_list_channels_excludes_dms() {
        use farder_crypto::identity::Keypair;

        let conn = db::open_in_memory().unwrap();
        let pk_a = Keypair::generate().public_key();
        let pk_b = Keypair::generate().public_key();

        create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        create_dm_channel(&conn, &pk_a, &pk_b).unwrap();

        let channels = list_channels(&conn).unwrap();
        assert_eq!(channels.len(), 1, "list_channels should exclude DM channels");
        assert_eq!(channels[0].name, "general");
    }
}
