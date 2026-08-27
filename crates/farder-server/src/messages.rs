use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use farder_crypto::identity::PublicKey;
use farder_protocol::server::{MessageInfo, MessageInfoV2, DELETED_USER_KEY};

use crate::db::now;

/// Shared SELECT column list for messages — must match `row_to_message_info` index order.
/// author_name_override is at index 8; author_badge at index 9; widget is appended last
/// (index 10) so all prior indices stay stable.
pub const MSG_SELECT: &str =
    "id, channel_id, author, content, timestamp, edited_at, reply_to, pinned, author_name_override, author_badge, widget";

pub fn row_to_message_info(row: &rusqlite::Row) -> rusqlite::Result<MessageInfo> {
    let id: i64 = row.get(0)?;
    let channel_id: i64 = row.get(1)?;
    let author_bytes: Vec<u8> = row.get(2)?;
    let content: String = row.get(3)?;
    let timestamp: i64 = row.get(4)?;
    let edited_at: Option<i64> = row.get(5)?;
    let reply_to: Option<i64> = row.get(6)?;
    let pinned: i64 = row.get(7)?;
    let author_name_override: Option<String> = row.get(8)?;
    let author_badge: Option<String> = row.get(9)?;
    let widget: Option<String> = row.get(10)?;

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
        author_name_override,
        author_badge,
        widget,
    })
}

// ---------------------------------------------------------------------------
// THE MESSAGE-WRITE CHOKE POINT (spec rev 2, C8/F1)
// ---------------------------------------------------------------------------
//
// Exactly ONE statement in the entire server inserts a `messages` row:
// `insert_row` below. Every other module reaches it through one of five doors:
//
//   | door                             | visibility  | class guard                     |
//   |----------------------------------|-------------|---------------------------------|
//   | `insert_message`                 | pub         | `require_plaintext`             |
//   | `insert_message_with_ts`         | pub         | `require_plaintext`             |
//   | `insert_message_with_author_name`| pub         | `require_plaintext`             |
//   | `edit_message` (UPDATE)          | pub         | `require_plaintext` (row's chan)|
//   | `insert_derived_row`             | pub(crate)  | `require_plaintext_for_derived` |
//   | `insert_sealed_row`              | pub(crate)  | `require_e2ee`                  |
//
// The guards live on the doors, not inside `insert_row`, so the one legitimate
// E2EE door can state its own (opposite) rule explicitly. Pinned by
// `no_insert_into_messages_sql_outside_the_choke_point`, which walks the crate
// source: a NEW writer added later trips a test, not production.

/// The ONE `INSERT INTO messages` statement in the server. Private on purpose.
/// `is_e2ee` rows carry `content = ''` plus opaque ciphertext in `sealed`, and
/// skip the FTS index entirely — nothing plaintext-shaped is ever written for
/// them, so there is nothing for a future `content`-reading feature to leak.
#[allow(clippy::too_many_arguments)]
fn insert_row(
    conn: &Connection,
    channel_id: u64,
    author: &PublicKey,
    content: &str,
    timestamp: u64,
    reply_to: Option<u64>,
    author_name_override: Option<&str>,
    author_badge: Option<&str>,
    event_hash: Option<&str>,
    sealed: Option<&[u8]>,
    is_e2ee: bool,
) -> Result<u64> {
    conn.execute(
        "INSERT INTO messages \
           (channel_id, author, content, timestamp, reply_to, author_name_override, author_badge, event_hash, sealed, is_e2ee) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            channel_id as i64,
            author.as_bytes().as_slice(),
            content,
            timestamp as i64,
            reply_to.map(|v| v as i64),
            author_name_override,
            author_badge,
            event_hash,
            sealed,
            if is_e2ee { 1i64 } else { 0i64 },
        ],
    )?;
    let id = conn.last_insert_rowid() as u64;

    // FTS5 is a plaintext index: sealed rows must never enter it.
    if !is_e2ee {
        conn.execute(
            "INSERT INTO messages_fts(rowid, content) VALUES (?1, ?2)",
            params![id as i64, content],
        )?;
    }

    Ok(id)
}

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
    crate::channel_class::require_plaintext(conn, channel_id)?;
    insert_row(
        conn, channel_id, author, content, timestamp, reply_to, None, None, None, None, false,
    )
}

/// Like `insert_message` but also sets `author_name_override` and `author_badge`
/// (for webhook/bot-posted messages). Pass `None` for either to leave it NULL.
pub fn insert_message_with_author_name(
    conn: &Connection,
    channel_id: u64,
    author: &PublicKey,
    content: &str,
    reply_to: Option<u64>,
    author_name_override: Option<&str>,
    author_badge: Option<&str>,
) -> Result<u64> {
    crate::channel_class::require_plaintext(conn, channel_id)?;
    insert_row(
        conn,
        channel_id,
        author,
        content,
        now(),
        reply_to,
        author_name_override,
        author_badge,
        None,
        None,
        false,
    )
}

/// The LOG-DERIVED plaintext door: a `messages` read-view row for a
/// signature-verified `MessagePosted` the fold accepted, carrying its
/// `event_hash`. Callable only from `event_ingest`'s derive/reconcile path.
/// See `channel_class::require_plaintext_for_derived` for why this door mirrors
/// the fold's unknown-channel carve-out instead of the strict rule.
pub(crate) fn insert_derived_row(
    conn: &Connection,
    channel_id: u64,
    author: &PublicKey,
    content: &str,
    timestamp: u64,
    reply_to: Option<u64>,
    event_hash: &str,
) -> Result<u64> {
    crate::channel_class::require_plaintext_for_derived(conn, channel_id)?;
    insert_row(
        conn,
        channel_id,
        author,
        content,
        timestamp,
        reply_to,
        None,
        None,
        Some(event_hash),
        None,
        false,
    )
}

/// The ONLY door into an E2EE channel. Callable solely from `event_ingest`'s
/// derive path, i.e. only for a row derived from a signature-verified
/// `MessagePostedE2ee` the fold accepted. Refuses a PLAINTEXT (or unresolvable)
/// channel too — it is the sealed door, not a general bypass.
///
/// `content` is stored as `''` and the FTS insert is skipped entirely.
pub(crate) fn insert_sealed_row(
    conn: &Connection,
    channel_id: u64,
    author: &PublicKey,
    sealed: &[u8],
    timestamp: u64,
    reply_to: Option<u64>,
    event_hash: &str,
) -> Result<u64> {
    crate::channel_class::require_e2ee(conn, channel_id)?;
    insert_row(
        conn,
        channel_id,
        author,
        "",
        timestamp,
        reply_to,
        None,
        None,
        Some(event_hash),
        Some(sealed),
        true,
    )
}

/// The sealed-EDIT door: replace a sealed row's ciphertext in place, from a
/// signature-verified `MessageEditedE2ee` the fold accepted. Callable solely
/// from `event_ingest`.
///
/// Same shape as `edit_message`'s guard, mirrored: the channel is resolved from
/// the row about to be updated and must be definitely E2EE, and the row must
/// itself be sealed — a sealed edit landing on a plaintext row would replace a
/// server-readable body with opaque bytes nobody can render.
///
/// `content` is never touched (it stays `''`) and `messages_fts` is never
/// touched, so an edited sealed row cannot enter the plaintext index by the
/// back door the way `edit_message`'s re-index would.
pub(crate) fn update_sealed_row(
    conn: &Connection,
    id: u64,
    sealed: &[u8],
    edited_at: u64,
) -> Result<()> {
    let (channel_id, is_e2ee): (i64, i64) = conn.query_row(
        "SELECT channel_id, is_e2ee FROM messages WHERE id = ?1",
        params![id as i64],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    crate::channel_class::require_e2ee(conn, channel_id as u64)?;
    anyhow::ensure!(is_e2ee != 0, "sealed edits only apply to sealed rows");
    conn.execute(
        "UPDATE messages SET sealed = ?2, edited_at = ?3 WHERE id = ?1",
        params![id as i64, sealed, edited_at as i64],
    )?;
    Ok(())
}

/// Stamps the widget JSON on an already-inserted message (insert-then-set-widget idiom:
/// resolves the message-id <-> feature-row-id circularity without touching insert signatures).
pub fn set_widget(conn: &Connection, message_id: u64, widget_json: &str) -> Result<()> {
    conn.execute(
        "UPDATE messages SET widget = ?1 WHERE id = ?2",
        params![widget_json, message_id as i64],
    )?;
    Ok(())
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

/// `get_message` for the v2 surface: the same row plus the three v2-only
/// columns (`is_e2ee`, `sealed`, `event_hash`), enriched exactly like
/// `get_message`. Used to build the `SealedMessage` / `SealedMessageEdited`
/// broadcasts for a freshly derived or edited row — the server never has the
/// plaintext, so the only live shape it can announce is `MessageInfoV2`.
pub fn get_message_v2(
    conn: &Connection,
    id: u64,
    requester: &PublicKey,
) -> Result<Option<MessageInfoV2>> {
    let select = format!("{}, is_e2ee, sealed, event_hash", MSG_SELECT);
    let mut msg = match conn
        .query_row(
            &format!("SELECT {} FROM messages WHERE id = ?1", select),
            params![id as i64],
            |row| {
                let base = row_to_message_info(row)?;
                let n = 11; // MSG_SELECT's column count; the three v2 columns follow it.
                let is_e2ee: i64 = row.get(n)?;
                Ok(MessageInfoV2 {
                    base,
                    is_e2ee: is_e2ee != 0,
                    sealed: row.get(n + 1)?,
                    event_hash: row.get(n + 2)?,
                })
            },
        )
        .optional()?
    {
        Some(m) => m,
        None => return Ok(None),
    };
    msg.base.attachments = crate::attachments::get_attachments_for_message(conn, msg.base.id)?;
    msg.base.reactions = crate::reactions::get_reactions_for_message(conn, msg.base.id, requester)?;
    if let Some(thread) = crate::channels::get_thread_for_message(conn, msg.base.id)? {
        msg.base.thread_id = Some(thread.id);
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE channel_id = ?1",
            rusqlite::params![thread.id as i64],
            |row| row.get(0),
        )?;
        msg.base.thread_message_count = Some(count as u32);
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
    // `AND is_e2ee = 0`: this is the V1 read surface, and `MessageInfo` has no
    // way to carry ciphertext. Without the filter a sealed row would arrive as a
    // real message with an EMPTY body — indistinguishable from a blank post, and
    // silently wrong. A v2 client reads sealed rows through `fetch_history_v2`.
    let sql = match before_id {
        Some(_) => format!(
            "SELECT {} FROM messages WHERE channel_id = ?1 AND is_e2ee = 0 AND id < ?2 \
             ORDER BY id DESC LIMIT ?3",
            MSG_SELECT
        ),
        None => format!(
            "SELECT {} FROM messages WHERE channel_id = ?1 AND is_e2ee = 0 \
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

/// Replace a message's plaintext content. Part of the choke point: the channel
/// is resolved from the row about to be updated and must be definitely
/// plaintext — a server-readable edit body in a sealed channel is exactly the
/// leak `MessageEditedE2ee` exists to prevent (spec coexistence row 10).
pub fn edit_message(conn: &Connection, id: u64, new_content: &str) -> Result<()> {
    // Fetch the old content before updating so we can remove it from FTS5.
    let (channel_id, old_content): (i64, String) = conn.query_row(
        "SELECT channel_id, content FROM messages WHERE id = ?1",
        params![id as i64],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    crate::channel_class::require_plaintext(conn, channel_id as u64)?;

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

    // Fetch the content before deleting so we can remove it from FTS5. A SEALED
    // row was never indexed (`insert_row` skips the FTS insert for it), and an
    // FTS5 'delete' command for a row that was never inserted is an index
    // corruption hazard, so it is skipped rather than issued against `''`.
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT content, is_e2ee FROM messages WHERE id = ?1",
            params![id as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    if let Some((old_content, is_e2ee)) = row {
        if is_e2ee == 0 {
            // Use the 'delete' command for the content-backed FTS5 table.
            conn.execute(
                "INSERT INTO messages_fts(messages_fts, rowid, content) VALUES ('delete', ?1, ?2)",
                params![id as i64, old_content],
            )?;
        }
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

/// The v2 read surface: history that can carry sealed rows.
///
/// Same ordering, permission model and `limit` semantics as [`fetch_history`] —
/// the ONLY difference is that a sealed row is included, with its ciphertext in
/// `sealed`, `is_e2ee = true`, and `base.content` left as the `''` the server
/// stores (it holds ciphertext and has nothing else to put there).
///
/// `event_hash` rides along because a v2 client needs it to cite a message in a
/// reply, edit or delete over the log; the numeric id is server-local.
pub fn fetch_history_v2(
    conn: &Connection,
    channel_id: u64,
    before_id: Option<u64>,
    limit: u32,
    requester: &PublicKey,
) -> Result<Vec<MessageInfoV2>> {
    let select = format!("{}, is_e2ee, sealed, event_hash", MSG_SELECT);
    let sql = match before_id {
        Some(_) => format!(
            "SELECT {} FROM messages WHERE channel_id = ?1 AND id < ?2 \
             ORDER BY id DESC LIMIT ?3",
            select
        ),
        None => format!(
            "SELECT {} FROM messages WHERE channel_id = ?1 \
             ORDER BY id DESC LIMIT ?2",
            select
        ),
    };
    let row_to_v2 = |row: &rusqlite::Row| -> rusqlite::Result<MessageInfoV2> {
        let base = row_to_message_info(row)?;
        let n = 11; // MSG_SELECT's column count; the three v2 columns follow it.
        let is_e2ee: i64 = row.get(n)?;
        Ok(MessageInfoV2 {
            base,
            is_e2ee: is_e2ee != 0,
            sealed: row.get(n + 1)?,
            event_hash: row.get(n + 2)?,
        })
    };

    let mut out: Vec<MessageInfoV2> = Vec::new();
    {
        let mut stmt = conn.prepare(&sql)?;
        let rows = match before_id {
            Some(bid) => stmt.query_map(
                params![channel_id as i64, bid as i64, limit as i64],
                row_to_v2,
            )?,
            None => stmt.query_map(params![channel_id as i64, limit as i64], row_to_v2)?,
        };
        for row in rows {
            out.push(row?);
        }
    }
    // Enrich exactly as `fetch_history` does, through the SAME batch loaders —
    // per-message lookups here would be an N+1 the v1 surface does not have.
    // Attachments, reactions and threads are class-independent: a sealed message
    // can still carry an encrypted attachment and still be reacted to.
    //
    // Ordering matches `fetch_history` exactly (newest-first, `ORDER BY id DESC`,
    // no reversal). A v2 client paginates identically to a v1 one.
    let msg_ids: Vec<u64> = out.iter().map(|m| m.base.id).collect();
    if !msg_ids.is_empty() {
        let attach_map = crate::attachments::get_attachments_for_messages(conn, &msg_ids)?;
        let reaction_map = crate::reactions::get_reactions_for_messages(conn, &msg_ids, requester)?;
        for m in out.iter_mut() {
            if let Some(a) = attach_map.get(&m.base.id) {
                m.base.attachments = a.clone();
            }
            if let Some(r) = reaction_map.get(&m.base.id) {
                m.base.reactions = r.clone();
            }
            if let Some(thread) = crate::channels::get_thread_for_message(conn, m.base.id)? {
                m.base.thread_id = Some(thread.id);
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM messages WHERE channel_id = ?1",
                    params![thread.id as i64],
                    |row| row.get(0),
                )?;
                m.base.thread_message_count = Some(count as u32);
            }
        }
    }
    Ok(out)
}

pub fn search_messages(
    conn: &Connection,
    query: &str,
    channel_id: Option<u64>,
    limit: u32,
    requester: &PublicKey,
) -> Result<Vec<MessageInfo>> {
    let mut messages = Vec::new();

    // `AND is_e2ee = 0` is belt-and-braces behind the FTS skip (coexistence row
    // 7a): a sealed row never enters `messages_fts`, so it cannot match — but the
    // index is a mutable artifact and search is the one surface whose whole job
    // is reading content, so the filter is stated in the query too. Client-side
    // search over the local decrypted store is sub-4's job.
    if let Some(cid) = channel_id {
        let sql = format!(
            "SELECT {} FROM messages \
             WHERE channel_id = ?2 AND is_e2ee = 0 \
               AND id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1) \
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
             WHERE is_e2ee = 0 \
               AND id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1) \
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
        "SELECT id, content, is_e2ee FROM messages WHERE channel_id = ?1 AND timestamp < ?2",
    )?;
    let rows: Vec<(i64, String, i64)> = stmt
        .query_map(
            params![channel_id as i64, cutoff_timestamp as i64],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let count = rows.len() as u64;

    // Delete attachments for each message before deleting the messages.
    for (rid, _, _) in &rows {
        let _ = crate::attachments::delete_attachments_for_message(conn, *rid as u64)?;
        crate::reactions::delete_reactions_for_message(conn, *rid as u64)?;
    }

    // Sealed rows were never indexed, so there is nothing to un-index for them
    // (see `delete_message`). Retention itself is content-blind: the DELETE below
    // is driven purely by channel + timestamp and covers ciphertext identically.
    for (rid, content, is_e2ee) in &rows {
        if *is_e2ee != 0 {
            continue;
        }
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
///
/// Returns the count of messages updated.
///
/// SEALED rows are anonymized too — the author sentinel is exactly the point of
/// the mechanism, and it is pure metadata, so it works on ciphertext unchanged.
/// Two things are deliberately NOT done to them: their `content` stays `''`
/// (writing `'[deleted]'` there would break the "a sealed row carries no
/// plaintext column" invariant every reader leans on), and they are kept out of
/// `messages_fts` (they were never in it; re-indexing them as `'[deleted]'`
/// would put sealed rows into the plaintext index by the back door, which
/// coexistence row 7a forbids). The ciphertext itself is left intact, matching
/// the plaintext behaviour where the body survives with the author erased.
pub fn anonymize_messages_by_author(conn: &Connection, author: &PublicKey) -> Result<u64> {
    // Collect all (id, content, is_e2ee) for this author.
    let mut stmt = conn.prepare(
        "SELECT id, content, is_e2ee FROM messages WHERE author = ?1",
    )?;
    let rows: Vec<(i64, String, i64)> = stmt
        .query_map(params![author.as_bytes().as_slice()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let count = rows.len() as u64;

    // Update FTS5 for each PLAINTEXT message: delete old content, insert "[deleted]".
    for (id, old_content, is_e2ee) in &rows {
        if *is_e2ee != 0 {
            continue;
        }
        conn.execute(
            "INSERT INTO messages_fts(messages_fts, rowid, content) VALUES ('delete', ?1, ?2)",
            params![id, old_content],
        )?;
        conn.execute(
            "INSERT INTO messages_fts(rowid, content) VALUES (?1, ?2)",
            params![id, "[deleted]"],
        )?;
    }

    // Bulk UPDATE: set the author sentinel on every row; rewrite `content` only
    // where there is a plaintext body to rewrite.
    conn.execute(
        "UPDATE messages SET author = ?1, \
                content = CASE WHEN is_e2ee = 0 THEN '[deleted]' ELSE content END \
         WHERE author = ?2",
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

    // -----------------------------------------------------------------------
    // The choke point (spec C8/F1)
    // -----------------------------------------------------------------------

    /// A channel declared E2EE by an accepted `ChannelCreated` (mirrored class).
    fn e2ee_channel(conn: &Connection) -> u64 {
        let id = create_channel(conn, "sealed", ChannelType::Text, None, 1).unwrap();
        crate::channel_class::set_class(conn, id, farder_crypto::event_log::ChannelClass::E2ee)
            .unwrap();
        id
    }

    fn message_count(conn: &Connection, channel_id: u64) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE channel_id = ?1",
            params![channel_id as i64],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn insert_message_hard_errors_in_an_e2ee_channel() {
        let (conn, _plain, pk) = setup();
        let sealed = e2ee_channel(&conn);
        let err = insert_message(&conn, sealed, &pk, "host-authored", None).unwrap_err();
        assert!(err.to_string().contains(crate::channel_class::E2EE_REFUSED), "{err}");
        assert_eq!(message_count(&conn, sealed), 0, "no row may land");
    }

    #[test]
    fn insert_message_with_ts_hard_errors_in_an_e2ee_channel() {
        let (conn, _plain, pk) = setup();
        let sealed = e2ee_channel(&conn);
        let err = insert_message_with_ts(&conn, sealed, &pk, "host-authored", None, 5).unwrap_err();
        assert!(err.to_string().contains(crate::channel_class::E2EE_REFUSED), "{err}");
        assert_eq!(message_count(&conn, sealed), 0);
    }

    #[test]
    fn insert_message_with_author_name_hard_errors_in_an_e2ee_channel() {
        let (conn, _plain, pk) = setup();
        let sealed = e2ee_channel(&conn);
        let err =
            insert_message_with_author_name(&conn, sealed, &pk, "webhook says hi", None, Some("Hook"), Some("BOT"))
                .unwrap_err();
        assert!(err.to_string().contains(crate::channel_class::E2EE_REFUSED), "{err}");
        assert_eq!(message_count(&conn, sealed), 0);
    }

    #[test]
    fn edit_message_hard_errors_in_an_e2ee_channel() {
        let (conn, _plain, pk) = setup();
        let sealed = e2ee_channel(&conn);
        // Get a row into the sealed channel the only legitimate way.
        let id = insert_sealed_row(&conn, sealed, &pk, b"ciphertext", 10, None, "hash-edit").unwrap();
        let err = edit_message(&conn, id, "plaintext edit body").unwrap_err();
        assert!(err.to_string().contains(crate::channel_class::E2EE_REFUSED), "{err}");
        // The row is untouched: still empty content, still sealed.
        let (content, edited_at): (String, Option<i64>) = conn
            .query_row(
                "SELECT content, edited_at FROM messages WHERE id = ?1",
                params![id as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(content, "");
        assert!(edited_at.is_none());
    }

    #[test]
    fn insert_sealed_row_is_the_only_writer_that_reaches_an_e2ee_channel() {
        let (conn, plain, pk) = setup();
        let sealed = e2ee_channel(&conn);

        // All four public plaintext doors refuse.
        assert!(insert_message(&conn, sealed, &pk, "x", None).is_err());
        assert!(insert_message_with_ts(&conn, sealed, &pk, "x", None, 1).is_err());
        assert!(insert_message_with_author_name(&conn, sealed, &pk, "x", None, None, None).is_err());
        // (edit_message is covered above; it needs an existing row.)
        // The log-derived plaintext door refuses too.
        assert!(insert_derived_row(&conn, sealed, &pk, "x", 1, None, "h1").is_err());
        assert_eq!(message_count(&conn, sealed), 0);

        // The sealed door succeeds — and ONLY there.
        let id = insert_sealed_row(&conn, sealed, &pk, b"\x00\x01ciphertext", 7, None, "h2").unwrap();
        assert_eq!(message_count(&conn, sealed), 1);

        // The sealed door is not a general bypass: it refuses a plaintext channel...
        assert!(insert_sealed_row(&conn, plain, &pk, b"ct", 7, None, "h3").is_err());
        assert_eq!(message_count(&conn, plain), 0);
        // ...and an unresolvable one.
        assert!(insert_sealed_row(&conn, 987_654, &pk, b"ct", 7, None, "h4").is_err());

        let _ = id;
    }

    /// OBSERVATION: inspect the bytes that actually landed for a sealed row.
    /// The blob is deliberately hostile — it contains the needle as readable
    /// text — so the assertions prove the row is kept out of the plaintext
    /// index and out of `content` structurally, not because the ciphertext
    /// happened to be unreadable.
    #[test]
    fn a_sealed_row_persists_ciphertext_only_and_never_enters_the_fts_index() {
        let (conn, plain, pk) = setup();
        let sealed_ch = e2ee_channel(&conn);
        let needle = "topsecretneedle";
        let ciphertext: Vec<u8> = format!("{needle} pretending to be ciphertext").into_bytes();
        let id = insert_sealed_row(&conn, sealed_ch, &pk, &ciphertext, 42, None, "hash-obs").unwrap();

        // Control: the same needle in a PLAINTEXT channel IS indexed, so a
        // negative result below means the skip worked, not that FTS is broken.
        insert_message(&conn, plain, &pk, "topsecretneedle in the clear", None).unwrap();

        let (content, stored, is_e2ee): (String, Option<Vec<u8>>, i64) = conn
            .query_row(
                "SELECT content, sealed, is_e2ee FROM messages WHERE id = ?1",
                params![id as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(content, "", "sealed rows store no plaintext content");
        assert_eq!(stored.as_deref(), Some(ciphertext.as_slice()), "sealed bytes stored verbatim");
        assert_eq!(is_e2ee, 1);

        // The FTS index contains exactly the plaintext row — never the sealed one.
        let hits: Vec<i64> = {
            let mut stmt = conn
                .prepare("SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1")
                .unwrap();
            let rows = stmt.query_map(params![needle], |r| r.get(0)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        assert!(
            !hits.contains(&(id as i64)),
            "a sealed row must never be reachable through the plaintext index: {hits:?}"
        );
        assert_eq!(hits.len(), 1, "only the plaintext control row is indexed");

        // Search cannot surface it either, in the sealed channel or globally.
        assert!(search_messages(&conn, needle, Some(sealed_ch), 50, &pk).unwrap().is_empty());
        let global = search_messages(&conn, needle, None, 50, &pk).unwrap();
        assert!(global.iter().all(|m| m.id != id));
    }

    /// The structural guard: exactly one file in the crate may contain the raw
    /// `messages` INSERT. A future writer added anywhere else fails HERE, in a
    /// test, instead of silently becoming a plaintext door into a sealed channel.
    #[test]
    fn no_insert_into_messages_sql_outside_the_choke_point() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                    out.push(path);
                }
            }
        }

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&src, &mut files);
        assert!(files.len() > 10, "source walk found suspiciously few files");

        // `INSERT INTO messages` NOT followed by `_` (so `messages_fts`, a
        // plaintext-only index the sealed door skips, does not count).
        let needle = concat!("INSERT INTO ", "messages");
        let mut offenders = Vec::new();
        for path in files {
            if path.file_name().map(|f| f == "messages.rs").unwrap_or(false) {
                continue; // the choke point itself
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let hit = text.match_indices(needle).any(|(i, _)| {
                !matches!(text.as_bytes().get(i + needle.len()), Some(b'_'))
            });
            if hit {
                offenders.push(path.display().to_string());
            }
        }
        assert!(
            offenders.is_empty(),
            "raw `messages` INSERT outside the choke point (messages.rs): {offenders:?} — \
             route it through messages::insert_message*/insert_derived_row/insert_sealed_row"
        );
    }

    #[test]
    fn test_set_widget_roundtrip() {
        let (conn, channel_id, pk) = setup();
        let id = insert_message(&conn, channel_id, &pk, "poll fallback", None).unwrap();
        // Plain insert reads widget: None.
        let msg = get_message(&conn, id, &pk).unwrap().unwrap();
        assert_eq!(msg.widget, None);
        // set_widget then get_message reads it back.
        let json = r#"{"type":"poll","id":7}"#;
        set_widget(&conn, id, json).unwrap();
        let msg = get_message(&conn, id, &pk).unwrap().unwrap();
        assert_eq!(msg.widget, Some(json.to_string()));
        // Other messages untouched.
        let other = insert_message(&conn, channel_id, &pk, "plain", None).unwrap();
        assert_eq!(get_message(&conn, other, &pk).unwrap().unwrap().widget, None);
    }

    #[test]
    fn test_message_info_decodes_without_widget_field() {
        // A MessageInfo encoded WITHOUT `widget` (old peer) must decode with
        // widget: None — the #[serde(default)] guard.
        let (conn, channel_id, pk) = setup();
        let id = insert_message(&conn, channel_id, &pk, "old frame", None).unwrap();
        let msg = get_message(&conn, id, &pk).unwrap().unwrap();
        // Simulate an old encoder: serialize to a JSON map and strip the field.
        let mut v = serde_json::to_value(&msg).unwrap();
        v.as_object_mut().unwrap().remove("widget").expect("widget field present");
        let decoded: MessageInfo = serde_json::from_value(v).unwrap();
        assert_eq!(decoded.widget, None);
        assert_eq!(decoded.content, "old frame");
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
            &conn, &dir.to_string_lossy(), &pk, "photo.jpg", b"file data", &hash, "application/octet-stream", None, None, None
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
