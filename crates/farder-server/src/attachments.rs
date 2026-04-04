use anyhow::{bail, Result};
use farder_crypto::identity::PublicKey;
use farder_protocol::server::AttachmentInfo;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::db::now;

// ---------------------------------------------------------------------------
// SHA-256 utility
// ---------------------------------------------------------------------------

pub fn compute_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

// ---------------------------------------------------------------------------
// Content-addressed path
// ---------------------------------------------------------------------------

/// Returns `{storage_dir}/{hash[0:2]}/{hash[2:4]}/{hash}`
pub fn content_path(storage_dir: &str, hash: &str) -> PathBuf {
    assert!(hash.len() >= 4, "hash must be at least 4 characters");
    PathBuf::from(storage_dir)
        .join(&hash[0..2])
        .join(&hash[2..4])
        .join(hash)
}

// ---------------------------------------------------------------------------
// File record type
// ---------------------------------------------------------------------------

pub struct FileRecord {
    pub id: u64,
    pub hash: String,
    pub size: u64,
    pub mime_type: String,
    pub original_name: String,
    pub uploaded_by: PublicKey,
    pub ref_count: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_secs: Option<f64>,
}

// ---------------------------------------------------------------------------
// File storage
// ---------------------------------------------------------------------------

/// Verify declared_hash matches actual data hash, write file to disk, INSERT
/// into `files` with ref_count=0, and return the new file_id.
pub fn store_file(
    conn: &Connection,
    storage_dir: &str,
    uploaded_by: &PublicKey,
    original_name: &str,
    data: &[u8],
    declared_hash: &str,
    mime_type: &str,
    width: Option<u32>,
    height: Option<u32>,
    duration_secs: Option<f64>,
) -> Result<u64> {
    // Verify hash.
    let actual_hash = compute_sha256(data);
    if actual_hash != declared_hash {
        bail!(
            "hash mismatch: declared {} but computed {}",
            declared_hash,
            actual_hash
        );
    }

    // Write to content-addressed path.
    let path = content_path(storage_dir, &actual_hash);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, data)?;

    // Insert DB record.
    conn.execute(
        "INSERT INTO files (hash, size, mime_type, original_name, uploaded_by, uploaded_at, ref_count, width, height, duration_secs) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9)",
        params![
            actual_hash,
            data.len() as i64,
            mime_type,
            original_name,
            uploaded_by.as_bytes().as_slice(),
            now() as i64,
            width.map(|v| v as i64),
            height.map(|v| v as i64),
            duration_secs,
        ],
    )?;

    Ok(conn.last_insert_rowid() as u64)
}

/// Store a file from an already-written temp file. Computes SHA-256, verifies
/// against `declared_hash`, moves to content-addressed path, inserts DB record.
/// If a file with the same hash already exists, deletes the temp file and returns
/// the existing file_id.
pub fn store_or_reuse_from_temp_file(
    conn: &Connection,
    storage_dir: &str,
    uploaded_by: &PublicKey,
    original_name: &str,
    temp_path: &std::path::Path,
    declared_hash: &str,
    mime_type: &str,
    file_size: u64,
    width: Option<u32>,
    height: Option<u32>,
    duration_secs: Option<f64>,
) -> Result<u64> {
    // Check for existing file first (dedup — inside the same DB lock).
    if let Some(rec) = get_file_by_hash(conn, declared_hash)? {
        // Temp file is no longer needed.
        let _ = std::fs::remove_file(temp_path);
        return Ok(rec.id);
    }

    // Compute SHA-256 of the temp file and verify.
    let actual_hash = {
        use std::io::Read;
        let mut hasher = Sha256::new();
        let mut f = std::fs::File::open(temp_path)?;
        let mut buf = [0u8; 65536];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        hex::encode(hasher.finalize())
    };

    if actual_hash != declared_hash {
        let _ = std::fs::remove_file(temp_path);
        bail!(
            "hash mismatch: declared {} but computed {}",
            declared_hash,
            actual_hash
        );
    }

    // Move temp file to content-addressed path.
    let dest = content_path(storage_dir, &actual_hash);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(temp_path, &dest)?;

    // Insert DB record.
    conn.execute(
        "INSERT INTO files (hash, size, mime_type, original_name, uploaded_by, uploaded_at, ref_count, width, height, duration_secs) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9)",
        params![
            actual_hash,
            file_size as i64,
            mime_type,
            original_name,
            uploaded_by.as_bytes().as_slice(),
            now() as i64,
            width.map(|v| v as i64),
            height.map(|v| v as i64),
            duration_secs,
        ],
    )?;

    Ok(conn.last_insert_rowid() as u64)
}

/// If a file with `declared_hash` already exists in the DB, return its id.
/// Otherwise call `store_file` and return the new id.
pub fn store_or_reuse(
    conn: &Connection,
    storage_dir: &str,
    uploaded_by: &PublicKey,
    original_name: &str,
    data: &[u8],
    declared_hash: &str,
    mime_type: &str,
    width: Option<u32>,
    height: Option<u32>,
    duration_secs: Option<f64>,
) -> Result<u64> {
    if let Some(rec) = get_file_by_hash(conn, declared_hash)? {
        return Ok(rec.id);
    }
    store_file(conn, storage_dir, uploaded_by, original_name, data, declared_hash, mime_type, width, height, duration_secs)
}

// ---------------------------------------------------------------------------
// File lookups
// ---------------------------------------------------------------------------

fn row_to_file_record(row: &rusqlite::Row) -> rusqlite::Result<FileRecord> {
    let id: i64 = row.get(0)?;
    let hash: String = row.get(1)?;
    let size: i64 = row.get(2)?;
    let mime_type: String = row.get(3)?;
    let original_name: String = row.get(4)?;
    let uploaded_by_bytes: Vec<u8> = row.get(5)?;
    let ref_count: i64 = row.get(6)?;
    let width: Option<i64> = row.get(7)?;
    let height: Option<i64> = row.get(8)?;
    let duration_secs: Option<f64> = row.get(9)?;
    let uploaded_by = PublicKey::from_bytes(
        uploaded_by_bytes
            .as_slice()
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidColumnType(5, "uploaded_by".to_string(), rusqlite::types::Type::Blob))?,
    );
    Ok(FileRecord {
        id: id as u64,
        hash,
        size: size as u64,
        mime_type,
        original_name,
        uploaded_by,
        ref_count: ref_count as u64,
        width: width.map(|v| v as u32),
        height: height.map(|v| v as u32),
        duration_secs,
    })
}

pub fn get_file(conn: &Connection, id: u64) -> Result<Option<FileRecord>> {
    let row = conn
        .query_row(
            "SELECT id, hash, size, mime_type, original_name, uploaded_by, ref_count, width, height, duration_secs \
             FROM files WHERE id = ?1",
            params![id as i64],
            row_to_file_record,
        )
        .optional()?;
    Ok(row)
}

pub fn get_file_by_hash(conn: &Connection, hash: &str) -> Result<Option<FileRecord>> {
    let row = conn
        .query_row(
            "SELECT id, hash, size, mime_type, original_name, uploaded_by, ref_count, width, height, duration_secs \
             FROM files WHERE hash = ?1",
            params![hash],
            row_to_file_record,
        )
        .optional()?;
    Ok(row)
}

// ---------------------------------------------------------------------------
// Ref counting
// ---------------------------------------------------------------------------

pub fn increment_ref_count(conn: &Connection, file_id: u64) -> Result<()> {
    conn.execute(
        "UPDATE files SET ref_count = ref_count + 1 WHERE id = ?1",
        params![file_id as i64],
    )?;
    Ok(())
}

pub fn decrement_ref_count(conn: &Connection, file_id: u64) -> Result<()> {
    conn.execute(
        "UPDATE files SET ref_count = MAX(ref_count - 1, 0) WHERE id = ?1",
        params![file_id as i64],
    )?;
    Ok(())
}

/// If the file has ref_count=0, delete it from disk and from the DB.
/// Returns true if the file was deleted.
pub fn cleanup_orphaned_file(
    conn: &Connection,
    storage_dir: &str,
    file_id: u64,
) -> Result<bool> {
    let row = conn
        .query_row(
            "SELECT hash, ref_count FROM files WHERE id = ?1",
            params![file_id as i64],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;

    let (hash, ref_count) = match row {
        Some(r) => r,
        None => return Ok(false),
    };

    if ref_count != 0 {
        return Ok(false);
    }

    // Delete from disk.
    let path = content_path(storage_dir, &hash);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }

    // Delete from DB.
    conn.execute("DELETE FROM files WHERE id = ?1", params![file_id as i64])?;

    Ok(true)
}

/// Delete all files with ref_count=0 that are older than max_age_secs.
/// Returns the count of deleted files.
pub fn cleanup_all_orphans(
    conn: &Connection,
    storage_dir: &str,
    max_age_secs: u64,
) -> Result<u64> {
    let cutoff = now().saturating_sub(max_age_secs);

    let mut stmt = conn.prepare(
        "SELECT id, hash FROM files WHERE ref_count = 0 AND uploaded_at < ?1",
    )?;
    let rows: Vec<(i64, String)> = stmt
        .query_map(params![cutoff as i64], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let count = rows.len() as u64;

    for (id, hash) in &rows {
        let path = content_path(storage_dir, hash);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        conn.execute("DELETE FROM files WHERE id = ?1", params![id])?;
    }

    Ok(count)
}

// ---------------------------------------------------------------------------
// Message attachments
// ---------------------------------------------------------------------------

pub fn create_message_attachment(
    conn: &Connection,
    message_id: u64,
    file_id: u64,
    position: u32,
    original_name: &str,
    width: Option<u32>,
    height: Option<u32>,
    duration_secs: Option<f64>,
) -> Result<u64> {
    conn.execute(
        "INSERT INTO message_attachments \
         (message_id, file_id, position, original_name, width, height, duration_secs) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            message_id as i64,
            file_id as i64,
            position as i64,
            original_name,
            width.map(|v| v as i64),
            height.map(|v| v as i64),
            duration_secs,
        ],
    )?;
    increment_ref_count(conn, file_id)?;
    Ok(conn.last_insert_rowid() as u64)
}

/// Load all attachments for a single message, ordered by position.
pub fn get_attachments_for_message(
    conn: &Connection,
    message_id: u64,
) -> Result<Vec<AttachmentInfo>> {
    let mut stmt = conn.prepare(
        "SELECT ma.id, ma.file_id, ma.original_name, f.size, f.mime_type, \
                ma.width, ma.height, ma.duration_secs \
         FROM message_attachments ma \
         JOIN files f ON f.id = ma.file_id \
         WHERE ma.message_id = ?1 \
         ORDER BY ma.position ASC",
    )?;

    let rows = stmt.query_map(params![message_id as i64], |row| {
        let id: i64 = row.get(0)?;
        let file_id: i64 = row.get(1)?;
        let name: String = row.get(2)?;
        let size: i64 = row.get(3)?;
        let mime_type: String = row.get(4)?;
        let width: Option<i64> = row.get(5)?;
        let height: Option<i64> = row.get(6)?;
        let duration_secs: Option<f64> = row.get(7)?;
        Ok(AttachmentInfo {
            id: id as u64,
            file_id: file_id as u64,
            name,
            size: size as u64,
            mime_type,
            width: width.map(|v| v as u32),
            height: height.map(|v| v as u32),
            duration_secs,
        })
    })?;

    let mut attachments = Vec::new();
    for row in rows {
        attachments.push(row?);
    }
    Ok(attachments)
}

/// Batch-load attachments for multiple messages. Returns a map from message_id
/// to its ordered list of attachments.
pub fn get_attachments_for_messages(
    conn: &Connection,
    message_ids: &[u64],
) -> Result<HashMap<u64, Vec<AttachmentInfo>>> {
    if message_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let placeholders: Vec<String> = message_ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect();
    let sql = format!(
        "SELECT ma.message_id, ma.id, ma.file_id, ma.original_name, f.size, f.mime_type, \
                ma.width, ma.height, ma.duration_secs \
         FROM message_attachments ma \
         JOIN files f ON f.id = ma.file_id \
         WHERE ma.message_id IN ({}) \
         ORDER BY ma.message_id ASC, ma.position ASC",
        placeholders.join(",")
    );

    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    for id in message_ids {
        param_values.push(Box::new(*id as i64));
    }
    let params_ref: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_ref.as_slice(), |row| {
        let message_id: i64 = row.get(0)?;
        let id: i64 = row.get(1)?;
        let file_id: i64 = row.get(2)?;
        let name: String = row.get(3)?;
        let size: i64 = row.get(4)?;
        let mime_type: String = row.get(5)?;
        let width: Option<i64> = row.get(6)?;
        let height: Option<i64> = row.get(7)?;
        let duration_secs: Option<f64> = row.get(8)?;
        Ok((
            message_id as u64,
            AttachmentInfo {
                id: id as u64,
                file_id: file_id as u64,
                name,
                size: size as u64,
                mime_type,
                width: width.map(|v| v as u32),
                height: height.map(|v| v as u32),
                duration_secs,
            },
        ))
    })?;

    let mut map: HashMap<u64, Vec<AttachmentInfo>> = HashMap::new();
    for row in rows {
        let (message_id, info) = row?;
        map.entry(message_id).or_default().push(info);
    }
    Ok(map)
}

/// Get the file_ids for all attachments on a message, delete those attachment
/// rows, decrement ref counts, and return the file_ids whose ref_count reached 0.
pub fn delete_attachments_for_message(
    conn: &Connection,
    message_id: u64,
) -> Result<Vec<u64>> {
    // Collect file_ids first.
    let mut stmt = conn.prepare(
        "SELECT file_id FROM message_attachments WHERE message_id = ?1",
    )?;
    let file_ids: Vec<u64> = stmt
        .query_map(params![message_id as i64], |row| {
            let id: i64 = row.get(0)?;
            Ok(id as u64)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // Delete the attachment rows.
    conn.execute(
        "DELETE FROM message_attachments WHERE message_id = ?1",
        params![message_id as i64],
    )?;

    // Decrement ref counts and collect file_ids that hit 0.
    let mut zero_refs = Vec::new();
    for &fid in &file_ids {
        decrement_ref_count(conn, fid)?;
        let ref_count: i64 = conn.query_row(
            "SELECT ref_count FROM files WHERE id = ?1",
            params![fid as i64],
            |row| row.get(0),
        )?;
        if ref_count == 0 {
            zero_refs.push(fid);
        }
    }

    Ok(zero_refs)
}

/// Remove all attachments for messages authored by `author`.
/// Returns accumulated orphan file_ids (those whose ref_count reached 0).
pub fn remove_attachments_for_author_messages(
    conn: &Connection,
    author: &PublicKey,
) -> Result<Vec<u64>> {
    // Get all message IDs authored by this user.
    let mut stmt = conn.prepare(
        "SELECT id FROM messages WHERE author = ?1",
    )?;
    let message_ids: Vec<u64> = stmt
        .query_map(params![author.as_bytes().as_slice()], |row| {
            let id: i64 = row.get(0)?;
            Ok(id as u64)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut all_orphans = Vec::new();
    for msg_id in message_ids {
        let orphans = delete_attachments_for_message(conn, msg_id)?;
        all_orphans.extend(orphans);
    }

    Ok(all_orphans)
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

    fn make_temp_dir() -> String {
        let dir = std::env::temp_dir().join(format!(
            "farder-attach-test-{}-{}",
            std::process::id(),
            rand::random::<u32>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn setup_db_channel_author() -> (rusqlite::Connection, u64, PublicKey) {
        let conn = db::open_in_memory().unwrap();
        let channel_id = create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let pk = gen_pk();
        register_member(&conn, &pk, "Alice").unwrap();
        (conn, channel_id, pk)
    }

    // -----------------------------------------------------------------------
    // Test 1: content_path format
    // -----------------------------------------------------------------------

    #[test]
    fn test_content_addressed_path() {
        let hash = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let path = content_path("/storage", hash);
        assert_eq!(
            path,
            PathBuf::from("/storage/ab/cd/abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890")
        );
    }

    // -----------------------------------------------------------------------
    // Test 2: store and retrieve file
    // -----------------------------------------------------------------------

    #[test]
    fn test_store_and_retrieve_file() {
        let conn = db::open_in_memory().unwrap();
        let storage = make_temp_dir();
        let pk = gen_pk();
        let data = b"hello world";
        let hash = compute_sha256(data);

        let file_id = store_file(&conn, &storage, &pk, "hello.txt", data, &hash, "text/plain", None, None, None).unwrap();

        // File exists on disk.
        let path = content_path(&storage, &hash);
        assert!(path.exists(), "file should be on disk");
        assert_eq!(std::fs::read(&path).unwrap(), data);

        // DB record has ref_count=0.
        let rec = get_file(&conn, file_id).unwrap().expect("file record should exist");
        assert_eq!(rec.id, file_id);
        assert_eq!(rec.hash, hash);
        assert_eq!(rec.size, data.len() as u64);
        assert_eq!(rec.mime_type, "text/plain");
        assert_eq!(rec.original_name, "hello.txt");
        assert_eq!(rec.ref_count, 0);

        std::fs::remove_dir_all(&storage).ok();
    }

    // -----------------------------------------------------------------------
    // Test 3: deduplication — same hash returns existing id
    // -----------------------------------------------------------------------

    #[test]
    fn test_dedup_same_hash() {
        let conn = db::open_in_memory().unwrap();
        let storage = make_temp_dir();
        let pk = gen_pk();
        let data = b"deduplicated content";
        let hash = compute_sha256(data);

        let id1 = store_or_reuse(&conn, &storage, &pk, "file.bin", data, &hash, "application/octet-stream", None, None, None).unwrap();
        let id2 = store_or_reuse(&conn, &storage, &pk, "file.bin", data, &hash, "application/octet-stream", None, None, None).unwrap();

        assert_eq!(id1, id2, "duplicate uploads must return the same file id");

        std::fs::remove_dir_all(&storage).ok();
    }

    // -----------------------------------------------------------------------
    // Test 4: wrong hash is rejected
    // -----------------------------------------------------------------------

    #[test]
    fn test_hash_mismatch_rejected() {
        let conn = db::open_in_memory().unwrap();
        let storage = make_temp_dir();
        let pk = gen_pk();
        let data = b"real content";
        let bad_hash = "0000000000000000000000000000000000000000000000000000000000000000";

        let result = store_file(&conn, &storage, &pk, "file.txt", data, bad_hash, "text/plain", None, None, None);
        assert!(result.is_err(), "store_file must fail on hash mismatch");

        std::fs::remove_dir_all(&storage).ok();
    }

    // -----------------------------------------------------------------------
    // Test 5: ref count lifecycle + cleanup
    // -----------------------------------------------------------------------

    #[test]
    fn test_ref_count_lifecycle() {
        let conn = db::open_in_memory().unwrap();
        let storage = make_temp_dir();
        let pk = gen_pk();
        let data = b"ref count test";
        let hash = compute_sha256(data);

        let file_id = store_file(&conn, &storage, &pk, "rc.bin", data, &hash, "application/octet-stream", None, None, None).unwrap();

        // Increment twice → ref_count = 2.
        increment_ref_count(&conn, file_id).unwrap();
        increment_ref_count(&conn, file_id).unwrap();
        let rec = get_file(&conn, file_id).unwrap().unwrap();
        assert_eq!(rec.ref_count, 2);

        // Decrement twice → ref_count = 0.
        decrement_ref_count(&conn, file_id).unwrap();
        decrement_ref_count(&conn, file_id).unwrap();
        let rec = get_file(&conn, file_id).unwrap().unwrap();
        assert_eq!(rec.ref_count, 0);

        // cleanup_orphaned_file should remove it.
        let deleted = cleanup_orphaned_file(&conn, &storage, file_id).unwrap();
        assert!(deleted, "file with ref_count=0 should be cleaned up");

        // No longer in DB.
        assert!(get_file(&conn, file_id).unwrap().is_none());
        // No longer on disk.
        assert!(!content_path(&storage, &hash).exists());

        std::fs::remove_dir_all(&storage).ok();
    }

    // -----------------------------------------------------------------------
    // Test 6: create and query message attachments
    // -----------------------------------------------------------------------

    #[test]
    fn test_create_and_get_message_attachments() {
        let (conn, channel_id, pk) = setup_db_channel_author();
        let storage = make_temp_dir();

        let data = b"image data";
        let hash = compute_sha256(data);
        let file_id = store_file(&conn, &storage, &pk, "img.png", data, &hash, "image/png", None, None, None).unwrap();

        let message_id = insert_message(&conn, channel_id, &pk, "check this out", None).unwrap();
        let att_id = create_message_attachment(
            &conn, message_id, file_id, 0, "img.png",
            Some(800), Some(600), None,
        ).unwrap();

        // ref_count should now be 1.
        let rec = get_file(&conn, file_id).unwrap().unwrap();
        assert_eq!(rec.ref_count, 1);

        let attachments = get_attachments_for_message(&conn, message_id).unwrap();
        assert_eq!(attachments.len(), 1);
        let a = &attachments[0];
        assert_eq!(a.id, att_id);
        assert_eq!(a.file_id, file_id);
        assert_eq!(a.name, "img.png");
        assert_eq!(a.size, data.len() as u64);
        assert_eq!(a.mime_type, "image/png");
        assert_eq!(a.width, Some(800));
        assert_eq!(a.height, Some(600));
        assert!(a.duration_secs.is_none());

        std::fs::remove_dir_all(&storage).ok();
    }

    // -----------------------------------------------------------------------
    // Test 7: batch query across multiple messages
    // -----------------------------------------------------------------------

    #[test]
    fn test_get_attachments_for_messages_batch() {
        let (conn, channel_id, pk) = setup_db_channel_author();
        let storage = make_temp_dir();

        // Create two files.
        let data1 = b"file one";
        let hash1 = compute_sha256(data1);
        let fid1 = store_file(&conn, &storage, &pk, "one.txt", data1, &hash1, "text/plain", None, None, None).unwrap();

        let data2 = b"file two";
        let hash2 = compute_sha256(data2);
        let fid2 = store_file(&conn, &storage, &pk, "two.txt", data2, &hash2, "text/plain", None, None, None).unwrap();

        // Create two messages, each with one attachment.
        let msg1 = insert_message(&conn, channel_id, &pk, "msg1", None).unwrap();
        let msg2 = insert_message(&conn, channel_id, &pk, "msg2", None).unwrap();
        let msg3 = insert_message(&conn, channel_id, &pk, "msg3 no attach", None).unwrap();

        create_message_attachment(&conn, msg1, fid1, 0, "one.txt", None, None, None).unwrap();
        create_message_attachment(&conn, msg2, fid2, 0, "two.txt", None, None, None).unwrap();

        // Batch load msg1, msg2, msg3.
        let map = get_attachments_for_messages(&conn, &[msg1, msg2, msg3]).unwrap();

        assert_eq!(map.get(&msg1).unwrap().len(), 1);
        assert_eq!(map.get(&msg1).unwrap()[0].file_id, fid1);

        assert_eq!(map.get(&msg2).unwrap().len(), 1);
        assert_eq!(map.get(&msg2).unwrap()[0].file_id, fid2);

        // msg3 has no attachments — may be absent or empty.
        assert!(map.get(&msg3).map(|v| v.is_empty()).unwrap_or(true));

        // Empty slice returns empty map.
        let empty = get_attachments_for_messages(&conn, &[]).unwrap();
        assert!(empty.is_empty());

        std::fs::remove_dir_all(&storage).ok();
    }

    // -----------------------------------------------------------------------
    // Test: remove_attachments_for_author_messages
    // -----------------------------------------------------------------------

    #[test]
    fn test_remove_attachments_for_author_messages() {
        let conn = db::open_in_memory().unwrap();
        let channel_id = create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let alice = gen_pk();
        register_member(&conn, &alice, "Alice").unwrap();
        let storage = make_temp_dir();

        // Create a message with an attachment by alice.
        let msg_id = insert_message(&conn, channel_id, &alice, "alice's message", None).unwrap();
        let data = b"attachment data";
        let hash = compute_sha256(data);
        let file_id = store_file(
            &conn, &storage, &alice, "file.txt", data, &hash, "text/plain", None, None, None,
        ).unwrap();
        create_message_attachment(&conn, msg_id, file_id, 0, "file.txt", None, None, None).unwrap();

        // Verify ref_count=1 before removal.
        assert_eq!(get_file(&conn, file_id).unwrap().unwrap().ref_count, 1);

        // Remove attachments for alice's messages.
        let orphans = remove_attachments_for_author_messages(&conn, &alice).unwrap();
        assert!(orphans.contains(&file_id), "file_id should be an orphan after removal");

        // The attachment should be gone.
        let attachments = get_attachments_for_message(&conn, msg_id).unwrap();
        assert!(attachments.is_empty(), "attachments should be gone after removal");

        // ref_count should now be 0.
        assert_eq!(get_file(&conn, file_id).unwrap().unwrap().ref_count, 0);

        std::fs::remove_dir_all(&storage).ok();
    }
}
