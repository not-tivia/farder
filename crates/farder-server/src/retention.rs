use crate::{attachments, channels, db, members, messages, reactions};
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

/// Executes all data deletion requests whose grace period has expired.
/// For each expired request: removes attachments, anonymizes messages, deletes reactions,
/// removes the member record, and deletes the deletion request.
/// Returns the number of deletions executed.
pub fn execute_pending_deletions(conn: &Connection, storage_dir: &str) -> Result<u64> {
    let expired = members::list_expired_deletion_requests(conn)?;
    let mut count = 0u64;
    for req in &expired {
        info!(member = %req.member_key, "executing data deletion");
        // 1. Remove attachments
        let orphans = attachments::remove_attachments_for_author_messages(conn, &req.member_key)?;
        for fid in orphans {
            let _ = attachments::cleanup_orphaned_file(conn, storage_dir, fid);
        }
        // 2. Anonymize messages
        let msg_count = messages::anonymize_messages_by_author(conn, &req.member_key)?;
        info!(member = %req.member_key, messages = msg_count, "anonymized messages");
        // 3. Delete reactions by user
        let reaction_count = reactions::delete_reactions_by_user(conn, &req.member_key)?;
        info!(member = %req.member_key, reactions = reaction_count, "removed reactions");
        // 4. Remove member record (includes member_roles)
        members::remove_member(conn, &req.member_key)?;
        // 5. Delete the deletion request
        members::delete_deletion_request(conn, &req.member_key)?;
        count += 1;
    }
    Ok(count)
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
            {
                let conn = state.db.lock().unwrap();
                match execute_pending_deletions(&conn, &state.storage_dir) {
                    Ok(n) if n > 0 => info!(deletions = n, "executed pending data deletions"),
                    Err(e) => tracing::warn!("data deletion error: {}", e),
                    _ => {}
                }
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
    use crate::reactions;
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
        channels::update_channel(&conn, ch_id, None, None, None, None, Some(Some(3600)), None, None).unwrap();

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

    #[test]
    fn test_execute_pending_deletions() {
        let conn = db::open_in_memory().unwrap();

        // Set up two members
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();
        let pk1 = kp1.public_key();
        let pk2 = kp2.public_key();
        members::register_member(&conn, &pk1, "Alice").unwrap();
        members::register_member(&conn, &pk2, "Bob").unwrap();

        // Create a channel and insert messages + reactions for both
        let ch_id = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let msg_id1 = messages::insert_message(&conn, ch_id, &pk1, "Alice's message", None).unwrap();
        let _msg_id2 = messages::insert_message(&conn, ch_id, &pk2, "Bob's message", None).unwrap();
        reactions::add_reaction(&conn, msg_id1, &pk1, "👍").unwrap();
        reactions::add_reaction(&conn, msg_id1, &pk2, "❤️").unwrap();

        // Create an already-expired deletion request for Alice (pk1)
        members::create_deletion_request_with_expires(&conn, &pk1, 1000, 2000).unwrap();

        // Execute pending deletions
        let count = execute_pending_deletions(&conn, "/tmp").unwrap();
        assert_eq!(count, 1, "should have executed 1 deletion");

        // Alice's messages should be anonymized (content = '[deleted]')
        let history = messages::fetch_history(&conn, ch_id, None, 10, &pk2).unwrap();
        let alice_msg = history.iter().find(|m| m.id == msg_id1).unwrap();
        assert_eq!(alice_msg.content, "[deleted]", "Alice's message should be anonymized");

        // Alice's reactions should be removed; Bob's reaction should remain.
        // Use Bob as the requester: his ❤️ reaction should still appear (me=true).
        let reactions_for_msg = reactions::get_reactions_for_message(&conn, msg_id1, &pk2).unwrap();
        // Only Bob's ❤️ should remain; Alice's 👍 should be gone.
        let thumbs_up = reactions_for_msg.iter().find(|r| r.emoji == "👍");
        assert!(thumbs_up.is_none(), "Alice's 👍 reaction should be removed");
        let heart = reactions_for_msg.iter().find(|r| r.emoji == "❤️");
        assert!(heart.is_some(), "Bob's ❤️ reaction should remain");

        // Alice's member record should be gone
        assert!(members::get_member(&conn, &pk1).unwrap().is_none(), "Alice should be removed as member");

        // Alice's deletion request should be cleaned up
        assert!(members::get_deletion_request(&conn, &pk1).unwrap().is_none(), "Deletion request should be gone");

        // Bob should still exist and his data should be untouched
        assert!(members::get_member(&conn, &pk2).unwrap().is_some(), "Bob should still be a member");
        let bob_msg = history.iter().find(|m| m.content == "Bob's message");
        assert!(bob_msg.is_some(), "Bob's message should be intact");
    }
}
