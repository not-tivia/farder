//! Shared widget substrate: the single sweeper task servicing every interactive
//! widget kind (polls close on their deadline; giveaways draw a winner). One task,
//! fixed 15 s tick, sync tick body (`sweep_once`) so tests run it without tokio.
//!
//! Lock discipline (bots.rs `spawn_bot_poll_task` pattern): all DB work happens in a
//! scoped `state.db` lock block that PERSISTS state changes and merely COLLECTS the
//! broadcasts; the guard drops before any `.await`. Persist-then-broadcast by
//! construction — a crash between persist and broadcast never re-closes or redraws.

pub const WIDGET_SWEEP_SECS: u64 = 15;

/// A broadcast computed under the DB lock, to be sent after the guard drops.
pub struct PendingBroadcast {
    pub target: crate::events::EventTarget,
    pub event: farder_protocol::server::ServerEvent,
}

/// Sync tick body servicing BOTH widget halves (polls: close due; giveaways: draw due —
/// lands with the giveaway feature). Extracted so tests run it without tokio.
/// State is persisted inside each half BEFORE this returns (i.e. under the caller's
/// lock, before any broadcast) — persist-then-broadcast by construction.
pub fn sweep_once(conn: &rusqlite::Connection, now: u64) -> Vec<PendingBroadcast> {
    let mut out = Vec::new();
    // Poll half: close every due timed poll; fold the terminal state into PollUpdated.
    match crate::polls::close_due(conn, now as i64) {
        Ok(infos) => {
            for info in infos {
                out.push(PendingBroadcast {
                    target: crate::events::EventTarget::Subscribers(info.channel_id),
                    event: farder_protocol::server::ServerEvent::PollUpdated { poll: info },
                });
            }
        }
        Err(e) => tracing::warn!("widget sweeper: poll close_due failed: {e}"),
    }
    // Giveaway half: draw every due open giveaway (persist + announcement commit
    // inside close_and_draw, under the caller's lock) → card flip + announcement.
    match crate::giveaways::list_due(conn, now as i64) {
        Ok(rows) => {
            for row in rows {
                match crate::giveaways::close_and_draw(conn, &row) {
                    Ok((info, msg)) => {
                        let channel_id = info.channel_id;
                        out.push(PendingBroadcast {
                            target: crate::events::EventTarget::Subscribers(channel_id),
                            event: farder_protocol::server::ServerEvent::GiveawayUpdated {
                                giveaway: info,
                            },
                        });
                        out.push(PendingBroadcast {
                            target: crate::events::EventTarget::Subscribers(channel_id),
                            event: farder_protocol::server::ServerEvent::NewMessage { message: msg },
                        });
                    }
                    Err(e) => {
                        tracing::warn!("widget sweeper: giveaway {} draw failed: {e}", row.id)
                    }
                }
            }
        }
        Err(e) => tracing::warn!("widget sweeper: giveaway list_due failed: {e}"),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_once_closes_due_poll_once() {
        let conn = crate::db::open_in_memory().unwrap();
        let channel_id = crate::channels::create_channel(
            &conn, "general", farder_protocol::server::ChannelType::Text, None, 0,
        )
        .unwrap();
        let pk = farder_crypto::identity::Keypair::generate().public_key();
        crate::members::register_member(&conn, &pk, "Alice").unwrap();
        let opts = vec!["a".to_string(), "b".to_string()];
        let now = 1_000_000u64;
        let poll_id = crate::polls::create(
            &conn, channel_id as i64, 1, &pk, "due?", &opts, Some(now as i64 - 10),
        )
        .unwrap();

        let pending = sweep_once(&conn, now);
        assert_eq!(pending.len(), 1, "one due poll → one PendingBroadcast");
        match &pending[0].event {
            farder_protocol::server::ServerEvent::PollUpdated { poll } => {
                assert_eq!(poll.id, poll_id);
                assert!(poll.closed);
            }
            other => panic!("expected PollUpdated, got {other:?}"),
        }
        assert!(matches!(pending[0].target, crate::events::EventTarget::Subscribers(c) if c == channel_id));
        // Persisted: closed even though the broadcasts are merely collected.
        assert!(crate::polls::get(&conn, poll_id).unwrap().unwrap().closed_at.is_some());
        // Idempotent: a second sweep returns nothing (never re-closes).
        assert!(sweep_once(&conn, now).is_empty());
    }

    #[test]
    fn sweep_once_draws_due_giveaway_exactly_once() {
        let conn = crate::db::open_in_memory().unwrap();
        let channel_id = crate::channels::create_channel(
            &conn, "general", farder_protocol::server::ChannelType::Text, None, 0,
        )
        .unwrap();
        let creator = farder_crypto::identity::Keypair::generate().public_key();
        crate::members::register_member(&conn, &creator, "Creator").unwrap();
        let entrant = farder_crypto::identity::Keypair::generate().public_key();
        crate::members::register_member(&conn, &entrant, "Entrant").unwrap();
        let now = 1_000_000u64;
        let gid = crate::giveaways::create(
            &conn, channel_id as i64, 1, &creator, "prize", now as i64 - 10,
        )
        .unwrap();
        crate::giveaways::enter(&conn, gid, &entrant, 1).unwrap();

        let pending = sweep_once(&conn, now);
        assert_eq!(pending.len(), 2, "draw → GiveawayUpdated + NewMessage announcement");
        match &pending[0].event {
            farder_protocol::server::ServerEvent::GiveawayUpdated { giveaway } => {
                assert_eq!(giveaway.id, gid);
                assert_eq!(giveaway.status, "ended");
                assert_eq!(giveaway.winner, Some(entrant.clone()));
            }
            other => panic!("expected GiveawayUpdated, got {other:?}"),
        }
        match &pending[1].event {
            farder_protocol::server::ServerEvent::NewMessage { message } => {
                assert_eq!(message.author_badge.as_deref(), Some("BOT"));
                assert!(message.content.contains("won: prize"));
            }
            other => panic!("expected NewMessage announcement, got {other:?}"),
        }
        // Persisted before broadcast; second sweep draws NOTHING (no double
        // announcement — crash-safety idempotence).
        let msg_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap();
        assert!(sweep_once(&conn, now).is_empty());
        let msg_count_after: i64 =
            conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap();
        assert_eq!(msg_count, msg_count_after, "sweep pass twice → zero new announcements");
    }
}

/// Spawns the widget sweeper. Sweeps immediately, then every `WIDGET_SWEEP_SECS`
/// (poll-then-sleep, matching `bots::spawn_bot_poll_task`).
pub fn spawn_widget_sweeper(state: std::sync::Arc<crate::state::ServerState>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tracing::info!("widget sweeper started");
        loop {
            let pending: Vec<PendingBroadcast> = {
                let conn = state.db.lock().unwrap();
                sweep_once(&conn, crate::db::now())
            }; // MutexGuard dropped here — before any .await
            for pb in pending {
                crate::connection::broadcast_event(&state, pb.target, pb.event).await;
            }
            tokio::time::sleep(std::time::Duration::from_secs(WIDGET_SWEEP_SECS)).await;
        }
    })
}
