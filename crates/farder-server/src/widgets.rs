//! Shared widget substrate: the single sweeper task servicing every interactive
//! widget kind (polls close on their deadline; giveaways draw a winner; personal
//! reminders DM their owner). One task, fixed 15 s tick, sync tick body
//! (`sweep_once`) so tests run it without tokio.
//!
//! Lock discipline (bots.rs `spawn_bot_poll_task` pattern): all DB work happens in a
//! scoped `state.db` lock block that PERSISTS state changes and merely COLLECTS the
//! broadcasts and DMs; the guard drops before any `.await`. Persist-then-notify by
//! construction — a crash between persist and notify never re-closes, redraws or
//! re-fires (at-most-once delivery, deliberately chosen over at-least-once).

pub const WIDGET_SWEEP_SECS: u64 = 15;

/// A broadcast computed under the DB lock, to be sent after the guard drops.
pub struct PendingBroadcast {
    pub target: crate::events::EventTarget,
    pub event: farder_protocol::server::ServerEvent,
}

/// A DM computed under the DB lock, sent after the guard drops (`send_system_dm`
/// re-acquires the mutex, which is safe only once the sweeper's guard is gone —
/// the reason DMs are returned as data rather than sent inline).
pub struct PendingDm {
    pub recipient: farder_crypto::identity::PublicKey,
    pub text: String,
}

/// Everything one tick produced: broadcasts to fan out, DMs to deliver.
pub struct SweepOutcome {
    pub broadcasts: Vec<PendingBroadcast>,
    pub dms: Vec<PendingDm>,
}

/// Sync tick body servicing every widget half (polls: close due; giveaways: draw
/// due; reminders: DM due). Extracted so tests run it without tokio.
/// State is persisted inside each half BEFORE this returns (i.e. under the caller's
/// lock, before any broadcast or DM) — persist-then-notify by construction, under a
/// status guard, so a crash can never double-close, double-draw or double-fire.
pub fn sweep_once(conn: &rusqlite::Connection, now: u64) -> SweepOutcome {
    let mut out = SweepOutcome { broadcasts: Vec::new(), dms: Vec::new() };
    // Poll half: close every due timed poll; fold the terminal state into PollUpdated.
    match crate::polls::close_due(conn, now as i64) {
        Ok(infos) => {
            for info in infos {
                out.broadcasts.push(PendingBroadcast {
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
                        out.broadcasts.push(PendingBroadcast {
                            target: crate::events::EventTarget::Subscribers(channel_id),
                            event: farder_protocol::server::ServerEvent::GiveawayUpdated {
                                giveaway: info,
                            },
                        });
                        out.broadcasts.push(PendingBroadcast {
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
    // Reminder half: DM every due personal reminder. ZERO broadcasts — the only
    // artifact anyone sees is a DM to one person. `mark_sent` is the single-shot
    // guard: it flips 'pending' → 'sent' BEFORE the DM is queued, so a crash in
    // between loses at most one nudge and can never duplicate one.
    match crate::reminders::list_due(conn, now as i64) {
        Ok(rows) => {
            for row in rows {
                match crate::reminders::mark_sent(conn, row.id, now as i64) {
                    Ok(false) => continue, // already sent / cancelled — never DM
                    Err(e) => {
                        tracing::warn!(
                            "widget sweeper: reminder {} mark_sent failed: {e}",
                            row.id
                        );
                        continue;
                    }
                    Ok(true) => {}
                }
                let chan = crate::channels::get_channel(conn, row.channel_id as u64)
                    .ok()
                    .flatten();
                let name = chan.map(|c| c.name).unwrap_or_else(|| "a channel".to_string());
                out.dms.push(PendingDm {
                    recipient: row.owner.clone(),
                    text: format!(
                        "⏰ {}\n— set in #{} · farder://channel/{}",
                        row.text, name, row.channel_id
                    ),
                });
            }
        }
        Err(e) => tracing::warn!("widget sweeper: reminder list_due failed: {e}"),
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

        let out = sweep_once(&conn, now);
        assert!(out.dms.is_empty(), "polls never DM");
        let pending = out.broadcasts;
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
        assert!(sweep_once(&conn, now).broadcasts.is_empty());
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

        let out = sweep_once(&conn, now);
        assert!(out.dms.is_empty(), "giveaway draws never DM in v1");
        let pending = out.broadcasts;
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
        assert!(sweep_once(&conn, now).broadcasts.is_empty());
        let msg_count_after: i64 =
            conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap();
        assert_eq!(msg_count, msg_count_after, "sweep pass twice → zero new announcements");
    }

    #[test]
    fn sweep_once_due_reminder_produces_one_dm_and_flips_sent() {
        let conn = crate::db::open_in_memory().unwrap();
        let channel_id = crate::channels::create_channel(
            &conn, "general", farder_protocol::server::ChannelType::Text, None, 0,
        )
        .unwrap();
        let owner = farder_crypto::identity::Keypair::generate().public_key();
        crate::members::register_member(&conn, &owner, "Alice").unwrap();
        let now = 1_000_000u64;
        let id = crate::reminders::create(
            &conn, &owner, channel_id as i64, "take the pizza out", now as i64 - 5, now as i64 - 900,
        )
        .unwrap();

        let out = sweep_once(&conn, now);
        assert!(out.broadcasts.is_empty(), "a reminder produces ZERO broadcasts");
        assert_eq!(out.dms.len(), 1, "one due reminder → one DM");
        assert_eq!(out.dms[0].recipient, owner);
        assert!(out.dms[0].text.contains("take the pizza out"));
        assert!(out.dms[0].text.contains("#general"), "channel link-back");
        assert!(out.dms[0].text.contains(&format!("farder://channel/{channel_id}")));
        // Persisted BEFORE the DM leaves the process.
        let status: String = conn
            .query_row("SELECT status FROM reminders WHERE id = ?1", rusqlite::params![id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "sent");
    }

    #[test]
    fn sweep_once_reminder_is_idempotent() {
        let conn = crate::db::open_in_memory().unwrap();
        let channel_id = crate::channels::create_channel(
            &conn, "general", farder_protocol::server::ChannelType::Text, None, 0,
        )
        .unwrap();
        let owner = farder_crypto::identity::Keypair::generate().public_key();
        crate::members::register_member(&conn, &owner, "Alice").unwrap();
        let now = 1_000_000u64;
        crate::reminders::create(&conn, &owner, channel_id as i64, "ping", now as i64 - 5, 0)
            .unwrap();

        assert_eq!(sweep_once(&conn, now).dms.len(), 1);
        // Crash-safety idempotence: a second sweep at the same `now` fires nothing.
        assert!(sweep_once(&conn, now).dms.is_empty());
    }

    #[test]
    fn sweep_once_ignores_cancelled_reminder() {
        let conn = crate::db::open_in_memory().unwrap();
        let channel_id = crate::channels::create_channel(
            &conn, "general", farder_protocol::server::ChannelType::Text, None, 0,
        )
        .unwrap();
        let owner = farder_crypto::identity::Keypair::generate().public_key();
        crate::members::register_member(&conn, &owner, "Alice").unwrap();
        let now = 1_000_000u64;
        let id = crate::reminders::create(
            &conn, &owner, channel_id as i64, "cancelled", now as i64 - 5, 0,
        )
        .unwrap();
        assert!(crate::reminders::cancel(&conn, id, &owner).unwrap());

        let out = sweep_once(&conn, now);
        assert!(out.dms.is_empty(), "a cancelled reminder never fires");
        let status: String = conn
            .query_row("SELECT status FROM reminders WHERE id = ?1", rusqlite::params![id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "cancelled", "sweeper does not touch it");
    }
}

/// Spawns the widget sweeper. Sweeps immediately, then every `WIDGET_SWEEP_SECS`
/// (poll-then-sleep, matching `bots::spawn_bot_poll_task`).
pub fn spawn_widget_sweeper(state: std::sync::Arc<crate::state::ServerState>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tracing::info!("widget sweeper started");
        loop {
            let out: SweepOutcome = {
                let conn = state.db.lock().unwrap();
                sweep_once(&conn, crate::db::now())
            }; // MutexGuard dropped here — before any .await
            for pb in out.broadcasts {
                crate::connection::broadcast_event(&state, pb.target, pb.event).await;
            }
            // DMs go out only after the guard is gone: send_system_dm re-acquires
            // the same mutex internally. A failed DM is logged and dropped — the
            // state flip already committed (at-most-once by design).
            for dm in out.dms {
                if let Err(e) = crate::bots::send_system_dm(&state, &dm.recipient, &dm.text).await {
                    tracing::warn!("widget sweeper: system dm failed: {e}");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(WIDGET_SWEEP_SECS)).await;
        }
    })
}
