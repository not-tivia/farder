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

/// Sync tick body servicing BOTH widget halves (polls: close due; giveaways: draw due).
/// Extracted so tests run it without tokio. Skeleton: nothing due, ever.
pub fn sweep_once(_conn: &rusqlite::Connection, _now: u64) -> Vec<PendingBroadcast> {
    Vec::new()
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
