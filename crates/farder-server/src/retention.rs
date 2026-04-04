use crate::{channels, db, messages};
use crate::state::ServerState;
use anyhow::Result;
use rusqlite::Connection;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

/// Purges messages in channels that have a `retention_secs` set, deleting
/// any messages older than the retention window. Also cleans up orphaned files
/// older than 1 hour. Returns `(messages_purged, files_cleaned)`.
pub fn purge_expired_messages(conn: &Connection, storage_dir: &str) -> Result<(u64, u64)> {
    let all_channels = channels::list_channels(conn)?;
    let mut total_purged: u64 = 0;

    for ch in all_channels {
        if let Some(secs) = ch.retention_secs {
            let cutoff = db::now().saturating_sub(secs);
            let deleted = messages::delete_messages_before(conn, ch.id, cutoff)?;
            if deleted > 0 {
                info!(
                    channel_id = ch.id,
                    channel_name = %ch.name,
                    deleted,
                    "purged expired messages"
                );
            }
            total_purged += deleted;
        }
    }

    let files_cleaned = crate::attachments::cleanup_all_orphans(conn, storage_dir, 3600)?;

    Ok((total_purged, files_cleaned))
}

/// Spawns a background Tokio task that periodically runs `purge_expired_messages`.
/// The task ticks every `interval_secs` seconds.
pub fn spawn_retention_task(
    state: Arc<ServerState>,
    interval_secs: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(Duration::from_secs(interval_secs));
        loop {
            interval.tick().await;
            let (messages_purged, files_cleaned) = {
                let conn = state.db.lock().unwrap();
                match purge_expired_messages(&conn, &state.storage_dir) {
                    Ok(counts) => counts,
                    Err(e) => {
                        tracing::warn!(error = %e, "retention task error");
                        (0, 0)
                    }
                }
            };
            if messages_purged > 0 || files_cleaned > 0 {
                info!(messages_purged, files_cleaned, "retention task completed");
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels;
    use crate::db;
    use crate::members;
    use crate::messages;
    use farder_crypto::identity::Keypair;
    use farder_protocol::server::ChannelType;

    #[test]
    fn test_purge_expired_messages() {
        let conn = db::open_in_memory().unwrap();
        let pk = Keypair::generate().public_key();
        members::register_member(&conn, &pk, "Alice").unwrap();

        // Channel with 1-hour retention (3600 seconds).
        let ch_id =
            channels::create_channel(&conn, "ephemeral", ChannelType::Text, None, 0).unwrap();
        channels::update_channel(&conn, ch_id, None, None, None, None, Some(Some(3600))).unwrap();

        // Channel with no retention.
        let ch_id2 =
            channels::create_channel(&conn, "permanent", ChannelType::Text, None, 1).unwrap();

        // Insert old messages (timestamps far in the past).
        messages::insert_message_with_ts(&conn, ch_id, &pk, "old msg", None, 1000).unwrap();
        messages::insert_message_with_ts(&conn, ch_id, &pk, "also old", None, 2000).unwrap();
        messages::insert_message_with_ts(&conn, ch_id2, &pk, "permanent old", None, 1000).unwrap();

        // Insert a recent message (far-future timestamp — will never be purged).
        messages::insert_message_with_ts(&conn, ch_id, &pk, "recent", None, u64::MAX / 2).unwrap();

        let (purged, _files_cleaned) = purge_expired_messages(&conn, "/tmp").unwrap();
        assert_eq!(purged, 2);

        let history = messages::fetch_history(&conn, ch_id, None, 10, &pk).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content, "recent");

        let history2 = messages::fetch_history(&conn, ch_id2, None, 10, &pk).unwrap();
        assert_eq!(history2.len(), 1);
    }
}
