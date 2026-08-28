//! The local decrypted-history store for E2EE channels (spec sub-project 7).
//!
//! # Why this crate exists
//!
//! Opening a sealed message **consumes that generation's ratchet key in the
//! persisted MLS store**, so the same ciphertext can never be opened twice
//! (pinned by `farder-e2ee-client`'s
//! `opening_a_sealed_message_twice_is_impossible_so_history_needs_a_local_store`).
//! The client held decrypted content in memory only, so restarting the app made
//! every previously-read message render as "couldn't decrypt". Server ciphertext
//! is not a fallback: MLS deletes decryption keys aggressively and by design.
//!
//! So a local store is not an optimization — it is the only place an E2EE
//! channel's history can live at all.
//!
//! # Why every row is sealed
//!
//! Persisting decrypted text is a privacy regression unless it is encrypted at
//! rest: the whole point of the rung is that a member's device is the only place
//! the plaintext exists, and a seized laptop must not hand it over. Both halves
//! ship together — this crate has no "plaintext mode", not even for tests.
//!
//! **What is sealed:** author, content, reply-to and attachment refs, in one
//! AES-256-GCM blob per row.
//!
//! **What is deliberately NOT sealed**, and why that is honest rather than lazy:
//! `channel_id`, `message_id`, `event_hash` and `timestamp` stay in the clear so
//! ordering, pagination, retention sweeps and tombstone purges are index
//! operations that never decrypt anything. Those four columns are **exactly what
//! the server host already stores for every sealed row**, so they tell an
//! attacker holding this file nothing they could not get from the host's
//! database. Content and authorship — the part the host genuinely cannot see —
//! never touch the disk unsealed. The author is still queryable through a blind
//! index ([`author_tag`]) so an anonymize-on-leave purge runs without decryption
//! and without storing the author.
//!
//! # Key derivation
//!
//! [`derive_keys`] takes the unlocked identity signing key and HKDF-expands two
//! domain-separated subkeys: one for row AEAD, one for the author blind index.
//! The identity key is already PIN-wrapped at rest (Argon2id + AES-256-GCM) and
//! is already unlocked at every launch, so the archive is protected by the PIN
//! the user already types — with no second prompt and no second Argon2 pass.
//! Deriving an encryption key from the identity signing key follows the
//! precedent already set by `farder_crypto::key_exchange::ed25519_sk_to_x25519`.
//!
//! A separate AEAD key per purpose matters: the tag key MUST NOT be the content
//! key, or a blind index would be forgeable from a leaked row key.

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use anyhow::{Context, Result};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::path::Path;
use zeroize::ZeroizeOnDrop;

/// Current on-disk schema version. Bump only with a migration.
const SCHEMA_VERSION: i64 = 1;

/// HKDF info strings. Changing one is a key rotation — every existing row
/// becomes unreadable — so they are versioned and never edited in place.
const INFO_ROW_KEY: &[u8] = b"farder-history-store-v1";
const INFO_TAG_KEY: &[u8] = b"farder-history-author-tag-v1";

/// The two derived subkeys, zeroized on drop. Never serialized, never logged.
#[derive(Clone, ZeroizeOnDrop)]
pub struct HistoryKeys {
    row: [u8; 32],
    tag: [u8; 32],
}

impl std::fmt::Debug for HistoryKeys {
    /// Redacted on purpose: a key that prints is a key that reaches a log file.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HistoryKeys(redacted)")
    }
}

/// Derive one 32-byte at-rest subkey from the unlocked identity signing key.
///
/// This crate is the single owner of "keys for local files protected by the
/// identity", so every at-rest key in the client comes from here with its own
/// `info` string: the history rows and author tags below, the device signing
/// key's wrapper, and the MLS store's value encryption. Distinct `info` strings
/// are what keep those keys independent — reusing one would let a leak in any of
/// them compromise the others.
///
/// The input is already a uniformly random 32-byte seed, so HKDF-Expand is the
/// whole derivation (no extract step is needed).
pub fn derive_local_key(identity_signing_key: &[u8; 32], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, identity_signing_key);
    let mut out = [0u8; 32];
    hk.expand(info, &mut out).expect("32 is a valid HKDF length");
    out
}

/// Derive the row-AEAD and author-tag subkeys from the unlocked identity signing
/// key. They must differ: sharing them would let a leaked row key forge tags.
pub fn derive_keys(identity_signing_key: &[u8; 32]) -> HistoryKeys {
    HistoryKeys {
        row: derive_local_key(identity_signing_key, INFO_ROW_KEY),
        tag: derive_local_key(identity_signing_key, INFO_TAG_KEY),
    }
}

/// The blind index for one author: `HMAC-SHA256(tag_key, author_pk)`.
///
/// Lets `purge_author` and per-author queries run as index lookups without ever
/// storing the author in the clear. Keyed, so an attacker holding the file
/// cannot test a guessed author against it without the key.
pub fn author_tag(keys: &HistoryKeys, author: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&keys.tag)
        .expect("HMAC accepts any key length");
    mac.update(author);
    mac.finalize().into_bytes().to_vec()
}

/// One message as the UI needs it. `author` is raw public-key bytes (the caller
/// maps it to a display name); `content` is the decrypted text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRecord {
    pub channel_id: u64,
    pub message_id: u64,
    pub event_hash: String,
    pub timestamp: u64,
    pub author: Vec<u8>,
    pub content: String,
    pub reply_to: Option<String>,
    pub attachments: Vec<String>,
}

/// Exactly the fields that get sealed. Split from [`HistoryRecord`] so the
/// compiler — not a comment — decides what may touch the disk in the clear:
/// adding a field here seals it, adding one to the table does not.
#[derive(Serialize, Deserialize)]
struct SealedPayload {
    author: Vec<u8>,
    content: String,
    reply_to: Option<String>,
    attachments: Vec<String>,
}

/// The local history database for one identity (all servers, all channels).
pub struct HistoryStore {
    conn: Connection,
    keys: HistoryKeys,
}

impl HistoryStore {
    /// Open (creating if absent) the store at `path`.
    pub fn open(path: &Path, keys: HistoryKeys) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open history store at {}", path.display()))?;
        Self::init(&conn)?;
        Ok(Self { conn, keys })
    }

    /// An in-memory store — for tests that do not exercise the file itself.
    /// The observation test deliberately does NOT use this: it must scan a real
    /// file on disk.
    pub fn open_in_memory(keys: HistoryKeys) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init(&conn)?;
        Ok(Self { conn, keys })
    }

    fn init(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS messages (
                 channel_id INTEGER NOT NULL,
                 message_id INTEGER NOT NULL,
                 event_hash TEXT NOT NULL,
                 timestamp  INTEGER NOT NULL,
                 author_tag BLOB NOT NULL,
                 nonce      BLOB NOT NULL,
                 sealed     BLOB NOT NULL,
                 PRIMARY KEY (channel_id, message_id, event_hash)
             ) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS idx_history_channel_ts
                 ON messages(channel_id, timestamp);
             CREATE INDEX IF NOT EXISTS idx_history_author ON messages(author_tag);",
        )?;
        let existing: Option<i64> = conn
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |r| r.get(0))
            .optional()?;
        match existing {
            None => {
                conn.execute("INSERT INTO schema_version (version) VALUES (?1)", params![SCHEMA_VERSION])?;
            }
            Some(v) if v == SCHEMA_VERSION => {}
            Some(v) => anyhow::bail!("history store schema version {v} is not supported (expected {SCHEMA_VERSION})"),
        }
        Ok(())
    }

    /// The AEAD associated data: binds a row to its identity coordinates so a
    /// sealed blob copied to another channel, message id or event hash fails to
    /// open instead of silently decrypting somewhere it does not belong.
    fn aad(channel_id: u64, message_id: u64, event_hash: &str) -> Vec<u8> {
        let mut aad = Vec::with_capacity(16 + event_hash.len());
        aad.extend_from_slice(&channel_id.to_be_bytes());
        aad.extend_from_slice(&message_id.to_be_bytes());
        aad.extend_from_slice(event_hash.as_bytes());
        aad
    }

    fn seal(&self, rec: &HistoryRecord) -> Result<(Vec<u8>, Vec<u8>)> {
        let payload = SealedPayload {
            author: rec.author.clone(),
            content: rec.content.clone(),
            reply_to: rec.reply_to.clone(),
            attachments: rec.attachments.clone(),
        };
        let plain = rmp_serde::to_vec(&payload).context("serialize history payload")?;
        let nonce_bytes: [u8; 12] = rand::random();
        let cipher = Aes256Gcm::new_from_slice(&self.keys.row).expect("32-byte key");
        let aad = Self::aad(rec.channel_id, rec.message_id, &rec.event_hash);
        let sealed = cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), Payload { msg: &plain, aad: &aad })
            .map_err(|_| anyhow::anyhow!("seal history row"))?;
        Ok((nonce_bytes.to_vec(), sealed))
    }

    fn unseal(
        &self,
        channel_id: u64,
        message_id: u64,
        event_hash: &str,
        nonce: &[u8],
        sealed: &[u8],
    ) -> Result<SealedPayload> {
        let cipher = Aes256Gcm::new_from_slice(&self.keys.row).expect("32-byte key");
        let aad = Self::aad(channel_id, message_id, event_hash);
        let plain = cipher
            .decrypt(Nonce::from_slice(nonce), Payload { msg: sealed, aad: &aad })
            .map_err(|_| anyhow::anyhow!("unseal history row (wrong key or moved row)"))?;
        rmp_serde::from_slice(&plain).context("deserialize history payload")
    }

    /// Store one decrypted message. Idempotent on `(channel_id, message_id,
    /// event_hash)`: re-storing the same message re-seals it (fresh nonce) and
    /// replaces the row, so a duplicate write can never produce two rows.
    pub fn put(&self, rec: &HistoryRecord) -> Result<()> {
        let (nonce, sealed) = self.seal(rec)?;
        let tag = author_tag(&self.keys, &rec.author);
        self.conn.execute(
            "INSERT OR REPLACE INTO messages
             (channel_id, message_id, event_hash, timestamp, author_tag, nonce, sealed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                rec.channel_id as i64,
                rec.message_id as i64,
                rec.event_hash,
                rec.timestamp as i64,
                tag,
                nonce,
                sealed
            ],
        )?;
        Ok(())
    }

    /// One message, if this store has it.
    pub fn get(&self, channel_id: u64, message_id: u64) -> Result<Option<HistoryRecord>> {
        let row = self
            .conn
            .query_row(
                "SELECT event_hash, timestamp, nonce, sealed FROM messages
                 WHERE channel_id = ?1 AND message_id = ?2",
                params![channel_id as i64, message_id as i64],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                        r.get::<_, Vec<u8>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((event_hash, ts, nonce, sealed)) = row else {
            return Ok(None);
        };
        let p = self.unseal(channel_id, message_id, &event_hash, &nonce, &sealed)?;
        Ok(Some(HistoryRecord {
            channel_id,
            message_id,
            event_hash,
            timestamp: ts as u64,
            author: p.author,
            content: p.content,
            reply_to: p.reply_to,
            attachments: p.attachments,
        }))
    }

    /// A page of history, newest-first, mirroring `fetch_history_v2`'s shape so
    /// the frontend merge is a drop-in: `before_id = None` starts at the newest.
    pub fn page(&self, channel_id: u64, before_id: Option<u64>, limit: u32) -> Result<Vec<HistoryRecord>> {
        let before = before_id.map(|b| b as i64).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(
            "SELECT message_id, event_hash, timestamp, nonce, sealed FROM messages
             WHERE channel_id = ?1 AND message_id < ?2
             ORDER BY message_id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![channel_id as i64, before, limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Vec<u8>>(3)?,
                r.get::<_, Vec<u8>>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (mid, event_hash, ts, nonce, sealed) = row?;
            let p = self.unseal(channel_id, mid as u64, &event_hash, &nonce, &sealed)?;
            out.push(HistoryRecord {
                channel_id,
                message_id: mid as u64,
                event_hash,
                timestamp: ts as u64,
                author: p.author,
                content: p.content,
                reply_to: p.reply_to,
                attachments: p.attachments,
            });
        }
        Ok(out)
    }

    /// Case-insensitive substring search within one channel, newest-first
    /// (decrypt-and-scan: personal-scale data, and a blind token index would
    /// leak equality patterns for throughput we do not need).
    pub fn search(&self, channel_id: u64, query: &str, limit: u32) -> Result<Vec<HistoryRecord>> {
        let needle = query.to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut before = None;
        loop {
            let page = self.page(channel_id, before, 500)?;
            if page.is_empty() {
                break;
            }
            before = page.last().map(|r| r.message_id);
            for rec in page {
                if rec.content.to_lowercase().contains(&needle) {
                    out.push(rec);
                    if out.len() as u32 >= limit {
                        return Ok(out);
                    }
                }
            }
        }
        Ok(out)
    }

    // -- Purge obligations (the compliant-client rule, spec line 364) ---------
    //
    // Every one of these is a DELETE by index: no decryption, no scan. A
    // tombstone that arrives while the client is running must not survive in the
    // local archive, and the purge path must never need the key to do its job.

    /// Fold a `MessageDeleted` tombstone: drop every row for that message.
    pub fn purge_message(&self, channel_id: u64, message_id: u64) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM messages WHERE channel_id = ?1 AND message_id = ?2",
            params![channel_id as i64, message_id as i64],
        )?)
    }

    /// Retention expiry: drop everything older than `before_ts` in one channel.
    pub fn purge_before(&self, channel_id: u64, before_ts: u64) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM messages WHERE channel_id = ?1 AND timestamp < ?2",
            params![channel_id as i64, before_ts as i64],
        )?)
    }

    /// Anonymize-on-leave: drop everything one author wrote, found through the
    /// blind index so the author is never stored or compared in the clear.
    pub fn purge_author(&self, author: &[u8]) -> Result<usize> {
        let tag = author_tag(&self.keys, author);
        Ok(self
            .conn
            .execute("DELETE FROM messages WHERE author_tag = ?1", params![tag])?)
    }

    /// Attachment redaction: re-seal the row without that attachment ref. The
    /// only purge that needs the key, because attachments live inside the blob.
    pub fn redact_attachment(&self, channel_id: u64, message_id: u64, attachment: &str) -> Result<bool> {
        let Some(mut rec) = self.get(channel_id, message_id)? else {
            return Ok(false);
        };
        let before = rec.attachments.len();
        rec.attachments.retain(|a| a != attachment);
        if rec.attachments.len() == before {
            return Ok(false);
        }
        self.put(&rec)?;
        Ok(true)
    }

    /// Row count for one channel (diagnostics + tests).
    pub fn count(&self, channel_id: u64) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE channel_id = ?1",
            params![channel_id as i64],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_path(name: &str) -> PathBuf {
        let n = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "farder-history-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("history.db")
    }

    fn keys(seed: u8) -> HistoryKeys {
        derive_keys(&[seed; 32])
    }

    fn rec(channel_id: u64, message_id: u64, content: &str) -> HistoryRecord {
        HistoryRecord {
            channel_id,
            message_id,
            event_hash: format!("hash-{message_id}"),
            timestamp: 1_000 + message_id,
            author: vec![7u8; 32],
            content: content.to_string(),
            reply_to: None,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn a_stored_message_round_trips() {
        let s = HistoryStore::open_in_memory(keys(1)).unwrap();
        let r = rec(9, 1, "the quick brown fox");
        s.put(&r).unwrap();
        assert_eq!(s.get(9, 1).unwrap().as_ref(), Some(&r));
    }

    #[test]
    fn a_different_key_cannot_open_the_rows() {
        let path = temp_path("wrong-key");
        {
            let s = HistoryStore::open(&path, keys(1)).unwrap();
            s.put(&rec(9, 1, "secret")).unwrap();
        }
        let other = HistoryStore::open(&path, keys(2)).unwrap();
        let err = other.get(9, 1).unwrap_err();
        assert!(format!("{err}").contains("unseal"), "got {err}");
    }

    /// The AAD binding: a sealed blob moved to another message id must NOT open.
    /// Without it, a row could be relocated inside the file and still decrypt,
    /// so a tampered database could attribute one message's content to another.
    #[test]
    fn a_row_moved_to_another_message_id_fails_to_open() {
        let path = temp_path("moved-row");
        let s = HistoryStore::open(&path, keys(1)).unwrap();
        s.put(&rec(9, 1, "belongs to message 1")).unwrap();

        // Relocate the sealed blob onto message id 2, as a tamperer would.
        s.conn
            .execute(
                "INSERT INTO messages (channel_id, message_id, event_hash, timestamp, author_tag, nonce, sealed)
                 SELECT channel_id, 2, event_hash, timestamp, author_tag, nonce, sealed
                 FROM messages WHERE channel_id = 9 AND message_id = 1",
                [],
            )
            .unwrap();

        let err = s.get(9, 2).unwrap_err();
        assert!(format!("{err}").contains("unseal"), "got {err}");
        // The original still opens, so the failure is the binding, not corruption.
        assert_eq!(s.get(9, 1).unwrap().unwrap().content, "belongs to message 1");
    }

    #[test]
    fn put_is_idempotent_on_the_message_identity() {
        let s = HistoryStore::open_in_memory(keys(1)).unwrap();
        s.put(&rec(9, 1, "once")).unwrap();
        s.put(&rec(9, 1, "once")).unwrap();
        assert_eq!(s.count(9).unwrap(), 1);
    }

    #[test]
    fn pages_come_back_newest_first_and_paginate() {
        let s = HistoryStore::open_in_memory(keys(1)).unwrap();
        for i in 1..=5 {
            s.put(&rec(9, i, &format!("message {i}"))).unwrap();
        }
        let first = s.page(9, None, 2).unwrap();
        assert_eq!(first.iter().map(|r| r.message_id).collect::<Vec<_>>(), vec![5, 4]);
        let next = s.page(9, Some(4), 2).unwrap();
        assert_eq!(next.iter().map(|r| r.message_id).collect::<Vec<_>>(), vec![3, 2]);
    }

    #[test]
    fn search_finds_content_case_insensitively_within_one_channel() {
        let s = HistoryStore::open_in_memory(keys(1)).unwrap();
        s.put(&rec(9, 1, "Meet me at the Bridge")).unwrap();
        s.put(&rec(9, 2, "nothing to see")).unwrap();
        s.put(&rec(10, 3, "bridge in another channel")).unwrap();
        let hits = s.search(9, "bridge", 10).unwrap();
        assert_eq!(hits.len(), 1, "only the matching row in THIS channel");
        assert_eq!(hits[0].message_id, 1);
    }

    #[test]
    fn a_tombstone_purges_the_row() {
        let s = HistoryStore::open_in_memory(keys(1)).unwrap();
        s.put(&rec(9, 1, "delete me")).unwrap();
        assert_eq!(s.purge_message(9, 1).unwrap(), 1);
        assert!(s.get(9, 1).unwrap().is_none());
    }

    #[test]
    fn retention_expiry_purges_only_older_rows() {
        let s = HistoryStore::open_in_memory(keys(1)).unwrap();
        for i in 1..=4 {
            s.put(&rec(9, i, "x")).unwrap();
        }
        // timestamps are 1001..=1004; drop everything before 1003.
        assert_eq!(s.purge_before(9, 1003).unwrap(), 2);
        assert_eq!(s.count(9).unwrap(), 2);
    }

    #[test]
    fn anonymize_purges_one_authors_rows_through_the_blind_index() {
        let s = HistoryStore::open_in_memory(keys(1)).unwrap();
        let mut mine = rec(9, 1, "mine");
        mine.author = vec![1u8; 32];
        let mut theirs = rec(9, 2, "theirs");
        theirs.author = vec![2u8; 32];
        s.put(&mine).unwrap();
        s.put(&theirs).unwrap();

        assert_eq!(s.purge_author(&[1u8; 32]).unwrap(), 1);
        assert!(s.get(9, 1).unwrap().is_none());
        assert!(s.get(9, 2).unwrap().is_some(), "the other author is untouched");
    }

    #[test]
    fn the_author_tag_is_keyed_so_it_is_not_a_bare_hash_of_the_author() {
        let author = vec![3u8; 32];
        assert_ne!(
            author_tag(&keys(1), &author),
            author_tag(&keys(2), &author),
            "two identities must not produce the same tag for the same author"
        );
    }

    #[test]
    fn redacting_an_attachment_reseals_without_it() {
        let s = HistoryStore::open_in_memory(keys(1)).unwrap();
        let mut r = rec(9, 1, "with files");
        r.attachments = vec!["a.png".into(), "b.png".into()];
        s.put(&r).unwrap();

        assert!(s.redact_attachment(9, 1, "a.png").unwrap());
        assert_eq!(s.get(9, 1).unwrap().unwrap().attachments, vec!["b.png".to_string()]);
        assert!(!s.redact_attachment(9, 1, "a.png").unwrap(), "already gone");
    }

    // -- T5: the observation test ------------------------------------------

    /// Scan every column of every table in a CLOSED database file for `needle`.
    /// Mirrors the server harness's `assert_no_plaintext_anywhere`: it must look
    /// everywhere, not only where we expect to have written.
    fn file_contains(path: &Path, needle: &str) -> bool {
        let conn = Connection::open(path).unwrap();
        let tables: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table'")
                .unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        for table in tables {
            let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
            let cols = stmt.column_count();
            let mut rows = stmt.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                for i in 0..cols {
                    let as_text: Option<String> = row.get(i).ok();
                    if as_text.is_some_and(|v| v.contains(needle)) {
                        return true;
                    }
                    let as_blob: Option<Vec<u8>> = row.get(i).ok();
                    if as_blob.is_some_and(|v| {
                        v.windows(needle.len()).any(|w| w == needle.as_bytes())
                    }) {
                        return true;
                    }
                }
            }
        }
        // Also scan the raw bytes of the file itself, so anything sqlite keeps
        // outside a readable row (freelist pages, stale WAL frames) still counts.
        let raw = std::fs::read(path).unwrap_or_default();
        raw.windows(needle.len().max(1)).any(|w| w == needle.as_bytes())
    }

    #[test]
    fn the_stored_file_contains_no_plaintext_content_or_author() {
        let path = temp_path("observation");
        let needle = "sensitive-needle-do-not-leak";
        {
            let s = HistoryStore::open(&path, keys(1)).unwrap();
            let mut r = rec(9, 1, needle);
            r.author = b"author-needle-do-not-leak".to_vec();
            s.put(&r).unwrap();
            // Prove it is really in there, through the real read path.
            assert_eq!(s.get(9, 1).unwrap().unwrap().content, needle);
        } // closed: WAL checkpointed into the file

        assert!(
            !file_contains(&path, needle),
            "the message content reached the disk in the clear"
        );
        assert!(
            !file_contains(&path, "author-needle-do-not-leak"),
            "the author reached the disk in the clear"
        );
    }

    /// The positive control for the scanner above. Without this, a scanner that
    /// silently looked in the wrong place would make the observation test pass
    /// while the store leaked everything.
    #[test]
    fn the_observation_scanner_finds_a_needle_that_is_really_there() {
        let path = temp_path("observation-control");
        let needle = "definitely-present-needle";
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE leaky (v TEXT);").unwrap();
            conn.execute("INSERT INTO leaky (v) VALUES (?1)", params![needle])
                .unwrap();
        }
        assert!(
            file_contains(&path, needle),
            "the scanner cannot see plaintext it was pointed straight at"
        );
    }
}
