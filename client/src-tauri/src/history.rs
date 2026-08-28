//! Tauri wrappers over `farder-history` — the local decrypted-history store.
//!
//! Thin by design: every rule (what is sealed, what stays in the clear, the AAD
//! binding, the purge semantics) lives in the crate, which root tests can link.
//! This module only derives the key, resolves the path, and marshals types.
//!
//! # The key never becomes state
//!
//! `derive_keys` is an HKDF expand, so it is cheap enough to run per call from
//! the identity key that `AppState` already holds. Keeping no second copy of key
//! material is the point: the archive is readable exactly while the identity is
//! unlocked, and locking the app makes it unreadable without any extra
//! bookkeeping to get wrong.

use std::sync::Arc;

use farder_history::{derive_keys, HistoryRecord, HistoryStore};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::state::AppState;

/// The frontend's view of one stored message. `author` is raw public-key bytes
/// (the frontend already holds `PublicKey { bytes }` for every row).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HistoryRow {
    pub channel_id: u64,
    pub message_id: u64,
    pub event_hash: String,
    pub timestamp: u64,
    pub author: Vec<u8>,
    pub content: String,
    pub reply_to: Option<String>,
    pub attachments: Vec<String>,
}

impl From<HistoryRecord> for HistoryRow {
    fn from(r: HistoryRecord) -> Self {
        Self {
            channel_id: r.channel_id,
            message_id: r.message_id,
            event_hash: r.event_hash,
            timestamp: r.timestamp,
            author: r.author,
            content: r.content,
            reply_to: r.reply_to,
            attachments: r.attachments,
        }
    }
}

impl From<HistoryRow> for HistoryRecord {
    fn from(r: HistoryRow) -> Self {
        Self {
            channel_id: r.channel_id,
            message_id: r.message_id,
            event_hash: r.event_hash,
            timestamp: r.timestamp,
            author: r.author,
            content: r.content,
            reply_to: r.reply_to,
            attachments: r.attachments,
        }
    }
}

/// Open the store for the currently unlocked identity.
///
/// A locked identity is an ordinary refusal, not a panic: the caller renders the
/// fail-closed state it already has for "cannot read this yet".
fn store(state: &Arc<AppState>) -> Result<HistoryStore, String> {
    let seed = {
        let guard = state.signing_key_bytes.lock().map_err(|_| "identity lock poisoned")?;
        (*guard).ok_or_else(|| "identity is locked".to_string())?
    };
    let keys = derive_keys(&seed);
    let path = crate::commands::farder_data_dir().join("history.db");
    HistoryStore::open(&path, keys).map_err(|e| e.to_string())
}

/// Persist one decrypted message. Called exactly once per successful decrypt.
#[tauri::command]
pub fn history_put(state: State<'_, Arc<AppState>>, row: HistoryRow) -> Result<(), String> {
    store(&state)?
        .put(&row.into())
        .map_err(|e| e.to_string())
}

/// A page of stored history for one channel, newest-first.
#[tauri::command]
pub fn history_page(
    state: State<'_, Arc<AppState>>,
    channel_id: u64,
    before_id: Option<u64>,
    limit: u32,
) -> Result<Vec<HistoryRow>, String> {
    let rows = store(&state)?
        .page(channel_id, before_id, limit)
        .map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(HistoryRow::from).collect())
}

/// Client-side search over the stored history of one channel. E2EE rows never
/// enter the server's FTS index, so this is the ONLY way to search them.
#[tauri::command]
pub fn history_search(
    state: State<'_, Arc<AppState>>,
    channel_id: u64,
    query: String,
    limit: u32,
) -> Result<Vec<HistoryRow>, String> {
    let rows = store(&state)?
        .search(channel_id, &query, limit)
        .map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(HistoryRow::from).collect())
}

/// Fold a `MessageDeleted` tombstone into the local archive. The compliant-client
/// rule: server-side the delete works on ciphertext, so end to end it only means
/// anything if the client drops its own copy too.
#[tauri::command]
pub fn history_purge_message(
    state: State<'_, Arc<AppState>>,
    channel_id: u64,
    message_id: u64,
) -> Result<usize, String> {
    store(&state)?
        .purge_message(channel_id, message_id)
        .map_err(|e| e.to_string())
}

/// Retention expiry for one channel.
#[tauri::command]
pub fn history_purge_before(
    state: State<'_, Arc<AppState>>,
    channel_id: u64,
    before_ts: u64,
) -> Result<usize, String> {
    store(&state)?
        .purge_before(channel_id, before_ts)
        .map_err(|e| e.to_string())
}

/// Anonymize-on-leave: drop everything one author wrote, via the blind index.
#[tauri::command]
pub fn history_purge_author(
    state: State<'_, Arc<AppState>>,
    author: Vec<u8>,
) -> Result<usize, String> {
    store(&state)?
        .purge_author(&author)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(message_id: u64, content: &str) -> HistoryRow {
        HistoryRow {
            channel_id: 4242,
            message_id,
            event_hash: format!("hash-{message_id}"),
            timestamp: 1_700_000_000 + message_id,
            author: vec![9u8; 32],
            content: content.to_string(),
            reply_to: None,
            attachments: Vec::new(),
        }
    }

    /// One combined test: `FARDER_DATA` is process-global, so split tests would
    /// race (the same reason `profile_sync`'s filesystem tests are one test).
    ///
    /// This drives the REAL command bodies — the same `store()` helper the Tauri
    /// commands call — so key derivation from `AppState`, the store path, and the
    /// put/page round-trip are verified rather than merely compiled. What it
    /// cannot cover is the untyped `invoke()` seam; that needs the app running.
    #[test]
    fn history_commands_derive_a_key_round_trip_and_refuse_while_locked() {
        let tmp = std::env::temp_dir().join(format!("farder-history-cmd-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe { std::env::set_var("FARDER_DATA", &tmp) };

        let state = Arc::new(AppState::new());

        // Locked identity: an ordinary refusal, never a panic.
        let err = match store(&state) {
            Err(e) => e,
            Ok(_) => panic!("a locked identity must not open the archive"),
        };
        assert!(err.contains("locked"), "expected a locked-identity refusal, got {err}");

        // Unlock (as `unlock_identity` does) and the store becomes readable.
        *state.signing_key_bytes.lock().unwrap() = Some([3u8; 32]);

        let s = store(&state).unwrap();
        s.put(&row(1, "first message").into()).unwrap();
        s.put(&row(2, "second message").into()).unwrap();

        let page: Vec<HistoryRow> = s
            .page(4242, None, 10)
            .unwrap()
            .into_iter()
            .map(HistoryRow::from)
            .collect();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].message_id, 2, "newest first");
        assert_eq!(page[0].content, "second message");
        assert_eq!(page[1].author, vec![9u8; 32], "author survives the round trip");

        // Search reaches the same rows.
        let hits = s.search(4242, "SECOND", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message_id, 2);

        // A tombstone purge drops it.
        assert_eq!(s.purge_message(4242, 2).unwrap(), 1);
        assert_eq!(s.page(4242, None, 10).unwrap().len(), 1);

        // A DIFFERENT identity opens the same file and sees nothing readable —
        // the archive is bound to the identity that wrote it.
        *state.signing_key_bytes.lock().unwrap() = Some([4u8; 32]);
        let other = store(&state).unwrap();
        assert!(
            other.page(4242, None, 10).is_err(),
            "another identity must not be able to read these rows"
        );

        unsafe { std::env::remove_var("FARDER_DATA") };
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

/// One in-channel transparency notice (sub-5b G1): a device gained or lost the
/// ability to read this channel.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NoticeRow {
    pub channel_id: u64,
    pub id: String,
    pub timestamp: u64,
    pub kind: String,
    pub identity: String,
    pub device: String,
}

impl From<farder_history::NoticeRecord> for NoticeRow {
    fn from(n: farder_history::NoticeRecord) -> Self {
        Self {
            channel_id: n.channel_id,
            id: n.id,
            timestamp: n.timestamp,
            kind: n.kind,
            identity: n.identity,
            device: n.device,
        }
    }
}

impl From<NoticeRow> for farder_history::NoticeRecord {
    fn from(n: NoticeRow) -> Self {
        Self {
            channel_id: n.channel_id,
            id: n.id,
            timestamp: n.timestamp,
            kind: n.kind,
            identity: n.identity,
            device: n.device,
        }
    }
}

/// Record one leaf-change notice. Idempotent on its deterministic id, so the
/// cursor-based steward observing the same change twice cannot stack duplicates.
#[tauri::command]
pub fn history_put_notice(state: State<'_, Arc<AppState>>, notice: NoticeRow) -> Result<(), String> {
    store(&state)?
        .put_notice(&notice.into())
        .map_err(|e| e.to_string())
}

/// A channel's transparency notices, oldest-first (they render in the timeline).
#[tauri::command]
pub fn history_notices(
    state: State<'_, Arc<AppState>>,
    channel_id: u64,
    limit: u32,
) -> Result<Vec<NoticeRow>, String> {
    let rows = store(&state)?
        .notices(channel_id, limit)
        .map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(NoticeRow::from).collect())
}
