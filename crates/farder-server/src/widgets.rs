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
///
/// Takes `&mut Connection` because the event start pass opens a transaction
/// (`channel_events::start_and_announce`) — the flip and its announcement must
/// commit together or not at all.
pub fn sweep_once(conn: &mut rusqlite::Connection, now: u64) -> SweepOutcome {
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
                out.dms.push(PendingDm {
                    recipient: row.owner.clone(),
                    text: reminder_dm_text(&row.text, row.channel_id, chan.as_ref()),
                });
            }
        }
        Err(e) => tracing::warn!("widget sweeper: reminder list_due failed: {e}"),
    }
    sweep_events(conn, now, &mut out);
    out
}

/// Event half: lead-time DMs, the start flip + announcement, and the
/// cancellation DMs. Each pass persists its guard column BEFORE producing any
/// notification, so a crash between the two loses at most one nudge and can
/// never double-fire.
fn sweep_events(conn: &mut rusqlite::Connection, now: u64, out: &mut SweepOutcome) {
    let now_i = now as i64;
    // 1. Lead-time reminder pass — Going + Maybe (a "Maybe" is undecided and the
    //    nudge is what converts it; "Can't make it" gets nothing).
    match crate::channel_events::list_reminder_due(conn, now_i) {
        Ok(rows) => {
            for row in rows {
                match crate::channel_events::mark_reminded(conn, row.id, now_i) {
                    Ok(false) => continue, // already reminded — never DM twice
                    Err(e) => {
                        tracing::warn!("widget sweeper: event {} mark_reminded failed: {e}", row.id);
                        continue;
                    }
                    Ok(true) => {}
                }
                let text = event_dm_text(&format!("⏰ \"{}\" starts soon.", row.title), &row);
                push_event_dms(conn, out, &row, &["going", "maybe"], &text);
            }
        }
        Err(e) => tracing::warn!("widget sweeper: event list_reminder_due failed: {e}"),
    }

    // 2. Start pass — flip + announce (one transaction, single-shot) + DM Going.
    match crate::channel_events::list_start_due(conn, now_i) {
        Ok(rows) => {
            // Resolved ONCE per tick, and only when something actually starts —
            // a server that never starts an event (and never fires a reminder)
            // never mints a system identity at all.
            let system_pk = if rows.is_empty() {
                None
            } else {
                match crate::bots::get_or_create_system_identity(conn) {
                    Ok(pk) => Some(pk),
                    Err(e) => {
                        tracing::warn!("widget sweeper: system identity unavailable: {e}");
                        None
                    }
                }
            };
            for row in rows {
                let Some(system_pk) = system_pk.as_ref() else { break };
                match crate::channel_events::start_and_announce(conn, &row, system_pk, now_i) {
                    // A Cancel (or a card delete) won the guard — announce nothing.
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::warn!("widget sweeper: event {} start failed: {e}", row.id);
                        continue;
                    }
                    Ok(Some((info, msg))) => {
                        let channel_id = info.channel_id;
                        out.broadcasts.push(PendingBroadcast {
                            target: crate::events::EventTarget::Subscribers(channel_id),
                            event: farder_protocol::server::ServerEvent::EventUpdated {
                                event: info,
                            },
                        });
                        out.broadcasts.push(PendingBroadcast {
                            target: crate::events::EventTarget::Subscribers(channel_id),
                            event: farder_protocol::server::ServerEvent::NewMessage {
                                message: msg,
                            },
                        });
                        let text =
                            event_dm_text(&format!("📅 \"{}\" is starting now.", row.title), &row);
                        push_event_dms(conn, out, &row, &["going"], &text);
                    }
                }
            }
        }
        Err(e) => tracing::warn!("widget sweeper: event list_start_due failed: {e}"),
    }

    // 3. Cancel-notify pass — Going only, no channel message (the card flip is
    //    the public record, the CancelGiveaway precedent).
    match crate::channel_events::list_cancel_unnotified(conn) {
        Ok(rows) => {
            for row in rows {
                match crate::channel_events::mark_cancel_notified(conn, row.id, now_i) {
                    Ok(false) => continue,
                    Err(e) => {
                        tracing::warn!(
                            "widget sweeper: event {} mark_cancel_notified failed: {e}",
                            row.id
                        );
                        continue;
                    }
                    Ok(true) => {}
                }
                let text = format!("❌ \"{}\" was cancelled.", row.title);
                push_event_dms(conn, out, &row, &["going"], &text);
            }
        }
        Err(e) => tracing::warn!("widget sweeper: event list_cancel_unnotified failed: {e}"),
    }
}

/// The reminder DM's body + its origin footer.
///
/// `/remind` is reachable inside a DM, so `channel_id` can be a **DM** channel
/// id — and a DM channel is not in the client's `activeServer.channels` (it has
/// no name either: `create_dm_channel` stores `''`). A `farder://channel/<id>`
/// pill for one would render as a nameless "Open channel" that drops the main
/// view onto an id it cannot resolve. So a DM origin gets a plain, link-free
/// footer. (The event DMs' `farder://widget/event/...` links are unaffected —
/// those render an inline card client-side, they never switch channel.)
fn reminder_dm_text(
    text: &str,
    channel_id: i64,
    chan: Option<&farder_protocol::server::ChannelInfo>,
) -> String {
    let is_dm = chan
        .map(|c| c.channel_type == farder_protocol::server::ChannelType::Dm)
        .unwrap_or(false);
    if is_dm {
        return format!("⏰ {text}\n— set in a direct message");
    }
    let name = chan.map(|c| c.name.as_str()).unwrap_or("a channel");
    format!("⏰ {text}\n— set in #{name} · farder://channel/{channel_id}")
}

/// Body + optional location + the widget deep link.
fn event_dm_text(head: &str, row: &crate::channel_events::EventRow) -> String {
    let mut text = head.to_string();
    if let Some(loc) = &row.location {
        text.push_str(&format!("\n📍 {loc}"));
    }
    text.push_str(&format!(
        "\nfarder://widget/event/{}/{}",
        row.channel_id, row.id
    ));
    text
}

/// One `PendingDm` per responder in `responses` (never a broadcast — an RSVP
/// roster is not public mail).
fn push_event_dms(
    conn: &rusqlite::Connection,
    out: &mut SweepOutcome,
    row: &crate::channel_events::EventRow,
    responses: &[&str],
    text: &str,
) {
    match crate::channel_events::responders(conn, row.id, responses) {
        Ok(pks) => {
            for pk in pks {
                out.dms.push(PendingDm { recipient: pk, text: text.to_string() });
            }
        }
        Err(e) => tracing::warn!("widget sweeper: event {} responders failed: {e}", row.id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_once_closes_due_poll_once() {
        let mut conn = crate::db::open_in_memory().unwrap();
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

        let out = sweep_once(&mut conn, now);
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
        assert!(sweep_once(&mut conn, now).broadcasts.is_empty());
    }

    #[test]
    fn sweep_once_draws_due_giveaway_exactly_once() {
        let mut conn = crate::db::open_in_memory().unwrap();
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

        let out = sweep_once(&mut conn, now);
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
        assert!(sweep_once(&mut conn, now).broadcasts.is_empty());
        let msg_count_after: i64 =
            conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap();
        assert_eq!(msg_count, msg_count_after, "sweep pass twice → zero new announcements");
    }

    #[test]
    fn sweep_once_due_reminder_produces_one_dm_and_flips_sent() {
        let mut conn = crate::db::open_in_memory().unwrap();
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

        let out = sweep_once(&mut conn, now);
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
    fn sweep_once_reminder_set_in_dm_omits_the_channel_link_back() {
        let mut conn = crate::db::open_in_memory().unwrap();
        let owner = farder_crypto::identity::Keypair::generate().public_key();
        let peer = farder_crypto::identity::Keypair::generate().public_key();
        crate::members::register_member(&conn, &owner, "Alice").unwrap();
        crate::members::register_member(&conn, &peer, "Bob").unwrap();
        // /remind is reachable inside a DM — and a DM channel id is not in the
        // client's channel list, so the pill must not be emitted at all.
        let dm_id = crate::channels::create_dm_channel(&conn, &owner, &peer).unwrap();
        let now = 1_000_000u64;
        crate::reminders::create(&conn, &owner, dm_id as i64, "call mum", now as i64 - 5, 0)
            .unwrap();

        let out = sweep_once(&mut conn, now);
        assert_eq!(out.dms.len(), 1, "a DM-origin reminder still fires");
        assert_eq!(out.dms[0].recipient, owner);
        assert!(out.dms[0].text.contains("call mum"), "the nudge itself is unchanged");
        assert!(
            !out.dms[0].text.contains("farder://channel/"),
            "no channel pill for a DM origin: {}",
            out.dms[0].text
        );
        assert!(!out.dms[0].text.contains('#'), "no nameless #… either");
        assert!(out.dms[0].text.contains("direct message"), "but the origin is still stated");
    }

    #[test]
    fn sweep_once_reminder_is_idempotent() {
        let mut conn = crate::db::open_in_memory().unwrap();
        let channel_id = crate::channels::create_channel(
            &conn, "general", farder_protocol::server::ChannelType::Text, None, 0,
        )
        .unwrap();
        let owner = farder_crypto::identity::Keypair::generate().public_key();
        crate::members::register_member(&conn, &owner, "Alice").unwrap();
        let now = 1_000_000u64;
        crate::reminders::create(&conn, &owner, channel_id as i64, "ping", now as i64 - 5, 0)
            .unwrap();

        assert_eq!(sweep_once(&mut conn, now).dms.len(), 1);
        // Crash-safety idempotence: a second sweep at the same `now` fires nothing.
        assert!(sweep_once(&mut conn, now).dms.is_empty());
    }

    #[test]
    fn sweep_once_ignores_cancelled_reminder() {
        let mut conn = crate::db::open_in_memory().unwrap();
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

        let out = sweep_once(&mut conn, now);
        assert!(out.dms.is_empty(), "a cancelled reminder never fires");
        let status: String = conn
            .query_row("SELECT status FROM reminders WHERE id = ?1", rusqlite::params![id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "cancelled", "sweeper does not touch it");
    }
    // ---------------------------- events ---------------------------------

    /// conn + channel + a creator member.
    fn ev_setup() -> (rusqlite::Connection, u64, farder_crypto::identity::PublicKey) {
        let conn = crate::db::open_in_memory().unwrap();
        let channel_id = crate::channels::create_channel(
            &conn, "general", farder_protocol::server::ChannelType::Text, None, 0,
        )
        .unwrap();
        let creator = farder_crypto::identity::Keypair::generate().public_key();
        crate::members::register_member(&conn, &creator, "Creator").unwrap();
        (conn, channel_id, creator)
    }

    fn ev_member(conn: &rusqlite::Connection, name: &str) -> farder_crypto::identity::PublicKey {
        let pk = farder_crypto::identity::Keypair::generate().public_key();
        crate::members::register_member(conn, &pk, name).unwrap();
        pk
    }

    fn parsed(lead: Option<u64>, location: Option<&str>) -> crate::channel_events::ParsedEvent {
        crate::channel_events::ParsedEvent {
            title: "Party".to_string(),
            when: crate::channel_events::WhenSpec::Relative(3_600),
            location: location.map(str::to_string),
            description: None,
            remind_lead: lead,
        }
    }

    /// An event row with an explicit `starts_at` (message id 1 — tests needing a
    /// real card use `create_event_card`).
    fn make_ev(
        conn: &rusqlite::Connection,
        channel_id: u64,
        creator: &farder_crypto::identity::PublicKey,
        starts_at: i64,
        lead: Option<u64>,
        location: Option<&str>,
    ) -> i64 {
        crate::channel_events::create(
            conn, channel_id as i64, 1, creator, &parsed(lead, location), starts_at, 0,
        )
        .unwrap()
    }

    #[test]
    fn sweep_once_event_lead_dms_going_and_maybe_only_then_never_again() {
        let (mut conn, channel_id, creator) = ev_setup();
        let now = 1_000_000u64;
        // Lead moment reached (starts_at - 900 <= now), start still ahead.
        let id = make_ev(&conn, channel_id, &creator, now as i64 + 300, Some(900), Some("my place"));
        let going = ev_member(&conn, "Going");
        let maybe = ev_member(&conn, "Maybe");
        let nope = ev_member(&conn, "Nope");
        crate::channel_events::rsvp(&conn, id, &going, "going", 1).unwrap();
        crate::channel_events::rsvp(&conn, id, &maybe, "maybe", 2).unwrap();
        crate::channel_events::rsvp(&conn, id, &nope, "no", 3).unwrap();

        let out = sweep_once(&mut conn, now);
        assert!(out.broadcasts.is_empty(), "a lead-time nudge is DM-only");
        let recipients: Vec<_> = out.dms.iter().map(|d| d.recipient.clone()).collect();
        assert_eq!(recipients, vec![going, maybe], "Going + Maybe only — never 'no'");
        assert!(out.dms[0].text.contains("\"Party\" starts soon."));
        assert!(out.dms[0].text.contains("📍 my place"));
        assert!(out.dms[0]
            .text
            .contains(&format!("farder://widget/event/{channel_id}/{id}")));
        // Persisted BEFORE the DM leaves the process, under the single-shot guard.
        let row = crate::channel_events::get(&conn, id).unwrap().unwrap();
        assert_eq!(row.reminded_at, Some(now as i64));
        // Crash-safety idempotence: a second sweep at the same `now` nudges nobody.
        assert!(sweep_once(&mut conn, now).dms.is_empty());
    }

    #[test]
    fn sweep_once_event_start_and_lead_same_tick_sends_start_batch_only() {
        let (mut conn, channel_id, creator) = ev_setup();
        let now = 1_000_000u64;
        // Both the lead moment AND the start are due in this tick.
        let id = make_ev(&conn, channel_id, &creator, now as i64 - 1, Some(900), None);
        let going = ev_member(&conn, "Going");
        let maybe = ev_member(&conn, "Maybe");
        crate::channel_events::rsvp(&conn, id, &going, "going", 1).unwrap();
        crate::channel_events::rsvp(&conn, id, &maybe, "maybe", 2).unwrap();

        let out = sweep_once(&mut conn, now);
        // No "starts soon" nudge at all — only the start batch (no double-ping).
        assert!(out.dms.iter().all(|d| d.text.contains("is starting now.")));
        assert_eq!(
            out.dms.iter().map(|d| d.recipient.clone()).collect::<Vec<_>>(),
            vec![going],
            "the start DM is Going-only"
        );
        let row = crate::channel_events::get(&conn, id).unwrap().unwrap();
        assert_eq!(row.status, "started");
        assert_eq!(row.reminded_at, None, "the lead DM was skipped, not sent");
    }

    #[test]
    fn sweep_once_event_start_flips_announces_once_and_dms_going_only() {
        let (mut conn, channel_id, creator) = ev_setup();
        let now = 1_000_000u64;
        // A real card so the announcement can thread under it.
        let (card, info) = crate::channel_events::create_event_card(
            &mut conn, channel_id, &creator, &parsed(None, None), now,
        )
        .unwrap();
        conn.execute(
            "UPDATE channel_events SET starts_at = ?2 WHERE id = ?1",
            rusqlite::params![info.id, now as i64 - 5],
        )
        .unwrap();
        let going = ev_member(&conn, "Going");
        let maybe = ev_member(&conn, "Maybe");
        crate::channel_events::rsvp(&conn, info.id, &going, "going", 1).unwrap();
        crate::channel_events::rsvp(&conn, info.id, &maybe, "maybe", 2).unwrap();

        let out = sweep_once(&mut conn, now);
        assert_eq!(out.broadcasts.len(), 2, "EventUpdated + the announcement");
        match &out.broadcasts[0].event {
            farder_protocol::server::ServerEvent::EventUpdated { event } => {
                assert_eq!(event.id, info.id);
                assert_eq!(event.status, "started");
            }
            other => panic!("expected EventUpdated, got {other:?}"),
        }
        match &out.broadcasts[1].event {
            farder_protocol::server::ServerEvent::NewMessage { message } => {
                assert_eq!(message.author_badge.as_deref(), Some("BOT"));
                assert_eq!(message.author_name_override.as_deref(), Some("Events"));
                assert_eq!(message.reply_to, Some(card.id), "threads under the card");
                assert!(message.content.contains("Party is starting now!"));
            }
            other => panic!("expected NewMessage announcement, got {other:?}"),
        }
        assert_eq!(
            out.dms.iter().map(|d| d.recipient.clone()).collect::<Vec<_>>(),
            vec![going],
            "start DMs go to Going only — a Maybe never committed"
        );
        assert_eq!(
            crate::channel_events::get(&conn, info.id).unwrap().unwrap().status,
            "started"
        );
        // Persisted before broadcast: a second sweep announces NOTHING.
        let msg_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap();
        let out2 = sweep_once(&mut conn, now);
        assert!(out2.broadcasts.is_empty() && out2.dms.is_empty());
        let msg_count_after: i64 =
            conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap();
        assert_eq!(msg_count, msg_count_after, "sweep twice → zero new announcements");
    }

    #[test]
    fn sweep_once_cancelled_event_dms_going_once_then_none() {
        let (mut conn, channel_id, creator) = ev_setup();
        let now = 1_000_000u64;
        let id = make_ev(&conn, channel_id, &creator, now as i64 + 3_600, None, None);
        let going = ev_member(&conn, "Going");
        let nope = ev_member(&conn, "Nope");
        crate::channel_events::rsvp(&conn, id, &going, "going", 1).unwrap();
        crate::channel_events::rsvp(&conn, id, &nope, "no", 2).unwrap();
        assert!(crate::channel_events::cancel(&conn, id, now as i64).unwrap());

        let out = sweep_once(&mut conn, now);
        assert!(out.broadcasts.is_empty(), "no channel message — the card flip is the record");
        assert_eq!(out.dms.len(), 1);
        assert_eq!(out.dms[0].recipient, going);
        assert!(out.dms[0].text.contains("\"Party\" was cancelled."));
        let row = crate::channel_events::get(&conn, id).unwrap().unwrap();
        assert_eq!(row.cancel_notified_at, Some(now as i64));
        // Single-shot.
        assert!(sweep_once(&mut conn, now).dms.is_empty());
    }

    /// After downtime the overdue backlog can exceed `EVENT_DUE_BATCH`. One tick
    /// must take at most a batch (bounded lock-hold), and the carry-over must
    /// drain on later ticks — each event starting EXACTLY once.
    #[test]
    fn sweep_once_drains_oversized_start_backlog_across_ticks_without_duplication() {
        use crate::channel_events::EVENT_DUE_BATCH;
        let (mut conn, channel_id, creator) = ev_setup();
        let now = 1_000_000u64;
        let going = ev_member(&conn, "Going");
        let total = EVENT_DUE_BATCH + 5;
        let mut ids = Vec::new();
        for i in 0..total {
            let id = make_ev(&conn, channel_id, &creator, now as i64 - 5 - i as i64, None, None);
            crate::channel_events::rsvp(&conn, id, &going, "going", 1).unwrap();
            ids.push(id);
        }

        // Tick 1 — capped at exactly one batch.
        let t1 = sweep_once(&mut conn, now);
        assert_eq!(t1.broadcasts.len(), EVENT_DUE_BATCH * 2, "EventUpdated + announcement each");
        assert_eq!(t1.dms.len(), EVENT_DUE_BATCH, "one Going DM per started event");
        // Tick 2 — the carry-over, and only the carry-over.
        let t2 = sweep_once(&mut conn, now);
        assert_eq!(t2.broadcasts.len(), (total - EVENT_DUE_BATCH) * 2);
        assert_eq!(t2.dms.len(), total - EVENT_DUE_BATCH);
        // Tick 3 — backlog empty; the guarded UPDATEs mean nothing re-fires.
        let t3 = sweep_once(&mut conn, now);
        assert!(t3.broadcasts.is_empty() && t3.dms.is_empty(), "drained, and never twice");

        // Every event started exactly once, across all ticks.
        let mut started: Vec<i64> = t1
            .broadcasts
            .iter()
            .chain(t2.broadcasts.iter())
            .filter_map(|b| match &b.event {
                farder_protocol::server::ServerEvent::EventUpdated { event } => Some(event.id),
                _ => None,
            })
            .collect();
        assert_eq!(started.len(), total, "one EventUpdated per event, no duplicates");
        started.sort_unstable();
        let mut want = ids.clone();
        want.sort_unstable();
        assert_eq!(started, want, "the whole backlog drained, exactly once each");
        assert!(ids
            .iter()
            .all(|id| crate::channel_events::get(&conn, *id).unwrap().unwrap().status == "started"));
        // One announcement row per event — not one per tick it was visible in.
        let msgs: i64 =
            conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap();
        assert_eq!(msgs, total as i64, "exactly one announcement per event");
    }

    #[test]
    fn sweep_once_start_pass_skips_event_cancelled_first() {
        let (mut conn, channel_id, creator) = ev_setup();
        let now = 1_000_000u64;
        // Due to start, but cancelled first (a Cancel or a card delete won).
        let id = make_ev(&conn, channel_id, &creator, now as i64 - 5, None, None);
        let going = ev_member(&conn, "Going");
        crate::channel_events::rsvp(&conn, id, &going, "going", 1).unwrap();
        assert!(crate::channel_events::cancel(&conn, id, now as i64).unwrap());
        let msg_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap();

        let out = sweep_once(&mut conn, now);
        assert!(
            out.broadcasts.is_empty(),
            "a cancelled event can never announce (the status guard fails)"
        );
        // It still gets its cancellation DM from the cancel-notify pass.
        assert_eq!(out.dms.len(), 1);
        assert!(out.dms[0].text.contains("was cancelled."));
        let msg_count_after: i64 =
            conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap();
        assert_eq!(msg_count, msg_count_after, "no announcement row was written");
        assert_eq!(crate::channel_events::get(&conn, id).unwrap().unwrap().status, "cancelled");
    }
}

/// Spawns the widget sweeper. Sweeps immediately, then every `WIDGET_SWEEP_SECS`
/// (poll-then-sleep, matching `bots::spawn_bot_poll_task`).
pub fn spawn_widget_sweeper(state: std::sync::Arc<crate::state::ServerState>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tracing::info!("widget sweeper started");
        loop {
            let out: SweepOutcome = {
                let mut conn = state.db.lock().unwrap();
                sweep_once(&mut conn, crate::db::now())
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
