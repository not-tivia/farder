//! Channel subscriptions — the fan-out set behind `EventTarget::Subscribers`.
//!
//! # The invariant
//!
//! **Subscribe is a permission boundary: the subscription set never contains a
//! member who cannot see the channel.**
//!
//! `state.subscriptions` maps `channel_id -> {member public key bytes}`, and
//! [`crate::connection::broadcast_event`] fans `EventTarget::Subscribers(id)`
//! out to that raw set with no further checks. Everything that rides that
//! target — `NewMessage` (plaintext for normal channels, ciphertext plus full
//! metadata for DMs), `MessageEdited`, `MessageDeleted`, `ReactionAdded`,
//! `ReactionRemoved`, and the poll/giveaway/event widget events — is therefore
//! only as private as this set is correct. `Subscribe` is client-supplied, so
//! the set is only correct if the server filters it.
//!
//! Two mechanisms keep the invariant:
//!
//! 1. **Admission** — [`apply_subscribe`] keeps only the channel ids the caller
//!    can actually see ([`visible`]) and silently drops the rest. Silently is
//!    deliberate: reporting which ids were dropped would turn `Subscribe` into
//!    an existence oracle for invisible channels, and erroring would break a
//!    legitimate client holding a slightly stale channel list.
//! 2. **Revocation** — admission alone is not enough, because access can be
//!    taken away *after* a legitimate subscribe. [`revalidate`] re-checks every
//!    entry in the map and drops the ones that no longer hold. It is driven
//!    from the single choke point every server event already passes through:
//!    `broadcast_event` calls it whenever it sees an event that signals an
//!    access change ([`event_changes_access`]). Hooking the broadcast rather
//!    than each handler arm means any current or future emitter of those events
//!    — the request handlers, the mesh event-log ingest path, bots — is covered
//!    without a new call site.
//!
//! # Residual gaps (deliberately documented, not hidden)
//!
//! - **Access change with no event.** Revocation is only as complete as the
//!   event stream. A future code path that removes access *without* emitting
//!   one of the events in [`event_changes_access`] would not prune. Any such
//!   path must either emit one of those events or call [`revalidate`] directly.
//! - **Direct DB edits** (an operator running SQL against the server database)
//!   emit nothing and so do not prune until the next access-change event.
//! - **Timeouts** are intentionally *not* an access change: a timeout removes
//!   the ability to send, not the ability to see, so a timed-out member keeps
//!   their subscriptions exactly as the read-side permission model says they
//!   should.
//! - **Racing subscribe.** [`revalidate`] holds the subscription write lock for
//!   the whole re-check, so a concurrent [`apply_subscribe`] either lands fully
//!   before or fully after it; there is no window where an entry is evaluated
//!   against stale state.

use std::collections::HashSet;

use farder_crypto::identity::PublicKey;
use farder_protocol::server::ServerEvent;
use rusqlite::Connection;

use crate::state::ServerState;

/// Is `member` allowed to hold a subscription to `channel_id`?
///
/// Three gates, all of which must pass:
/// 1. **Mesh membership** — on a server with an event log, the log is
///    authoritative; a removed/banned/pending key is not a member
///    (`handlers::content_block_reason`, the same gate `Subscribe` already
///    applied to the request as a whole).
/// 2. **Local membership** — a `members` row that is neither banned nor
///    revoked. Every authenticated connection has one (the handshake creates it
///    or rejects), so requiring it costs a legitimate member nothing while
///    making a kick/ban/revoke actually revoke.
/// 3. **Channel visibility** — `handlers::channel_visible`: DM ⇒ participant,
///    everything else ⇒ `VIEW_CHANNEL`, missing or soft-deleted ⇒ false.
///
/// Fails closed: any DB error is treated as "not allowed".
pub fn visible(
    state: &ServerState,
    conn: &Connection,
    member: &PublicKey,
    is_owner: bool,
    channel_id: u64,
) -> bool {
    if crate::handlers::content_block_reason(state, member).is_some() {
        return false;
    }
    match crate::members::get_member(conn, member) {
        Ok(Some(m)) if !m.banned && !m.revoked => {}
        _ => return false,
    }
    crate::handlers::channel_visible(conn, member, channel_id, is_owner).unwrap_or(false)
}

/// Apply a client `Subscribe`: drop every previous subscription for `member`,
/// then subscribe them to exactly the requested ids they are allowed to see.
///
/// This is the whole of the `ServerRequest::Subscribe` state change — the
/// request arm in `connection::main_loop` does nothing else — so the admission
/// filter cannot be bypassed by another code path.
///
/// Lock discipline: the DB mutex is taken, used, and **dropped** before the
/// subscription write lock is awaited. The DB mutex is never held across an
/// `.await`.
pub async fn apply_subscribe(
    state: &ServerState,
    member: &PublicKey,
    is_owner: bool,
    channel_ids: Vec<u64>,
) {
    let allowed: Vec<u64> = {
        let conn = state.db.lock().unwrap();
        channel_ids
            .into_iter()
            .filter(|cid| visible(state, &conn, member, is_owner, *cid))
            .collect()
    };

    let pk_bytes = *member.as_bytes();
    let mut subs = state.subscriptions.write().await;
    for subscribers in subs.values_mut() {
        subscribers.remove(&pk_bytes);
    }
    for channel_id in allowed {
        subs.entry(channel_id)
            .or_insert_with(HashSet::new)
            .insert(pk_bytes);
    }
}

/// Does this event mean somebody's access may have just changed?
///
/// Kept deliberately broad — a false positive costs one re-check of a map that
/// is small and only walked on rare control-plane events, while a false
/// negative leaves a member receiving a channel they can no longer see.
///
/// | event | access change it stands for |
/// |---|---|
/// | `PermissionsChanged` | role assigned/removed, channel or category override set |
/// | `RoleUpdated` / `RoleDeleted` | a role's permission bits changed or vanished |
/// | `ChannelUpdated` / `ChannelDeleted` | channel moved between categories (new overrides apply), or deleted |
/// | `MemberLeft` / `MemberBanned` / `YouWereKicked` / `YouWereBanned` | kick, ban, bot removal |
/// | `MembershipChanged` | mesh event-log `MemberRemoved` / `MemberBanned` / join / approval |
pub fn event_changes_access(event: &ServerEvent) -> bool {
    matches!(
        event,
        ServerEvent::PermissionsChanged
            | ServerEvent::RoleUpdated { .. }
            | ServerEvent::RoleDeleted { .. }
            | ServerEvent::ChannelUpdated { .. }
            | ServerEvent::ChannelDeleted { .. }
            | ServerEvent::MemberLeft { .. }
            | ServerEvent::MemberBanned { .. }
            | ServerEvent::MembershipChanged { .. }
            | ServerEvent::YouWereKicked
            | ServerEvent::YouWereBanned { .. }
    )
}

/// Re-check every live subscription and drop the ones that no longer satisfy
/// [`visible`]. This is the revocation half of the invariant.
///
/// Lock discipline: the subscription write lock is taken first and the DB mutex
/// *inside* it, with no `.await` in between — so the DB mutex is still never
/// held across an await, and the re-check is atomic with respect to
/// [`apply_subscribe`].
pub async fn revalidate(state: &ServerState) {
    let owner = state.owner.read().await.clone();

    let mut subs = state.subscriptions.write().await;
    if subs.is_empty() {
        return;
    }
    let conn = state.db.lock().unwrap();
    for (channel_id, subscribers) in subs.iter_mut() {
        subscribers.retain(|pk_bytes| {
            let pk = PublicKey::from_bytes(*pk_bytes);
            let is_owner = owner.as_ref().map(|o| o == &pk).unwrap_or(false);
            visible(state, &conn, &pk, is_owner, *channel_id)
        });
    }
    subs.retain(|_, subscribers| !subscribers.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::broadcast_event;
    use crate::events::EventTarget;
    use crate::{channels, members, messages, permissions};
    use farder_crypto::identity::Keypair;
    use farder_protocol::server::ChannelType;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    /// A state with the builtin `@everyone` role and a registered owner, the
    /// same shape `db::init` + the handshake produce.
    async fn setup() -> (Arc<ServerState>, PublicKey) {
        let state = Arc::new(ServerState::new_for_test().unwrap());
        let owner = Keypair::generate().public_key();
        {
            let conn = state.db.lock().unwrap();
            members::create_role(
                &conn,
                "@everyone",
                permissions::DEFAULT_EVERYONE,
                None,
                0,
                true,
                false,
            )
            .unwrap();
            members::register_member(&conn, &owner, "Owner").unwrap();
            let everyone = everyone_role_id_with(&conn);
            members::assign_role(&conn, &owner, everyone).unwrap();
        }
        *state.owner.write().await = Some(owner.clone());
        (state, owner)
    }

    /// Register a member exactly the way the handshake does
    /// (`auth::authenticate_new_member`): a `members` row **plus** an explicit
    /// `@everyone` role assignment. The assignment is not cosmetic — channel
    /// overrides only apply to roles the member actually holds, so without it
    /// an `@everyone` deny would silently do nothing.
    fn add_member(state: &ServerState, name: &str) -> PublicKey {
        let pk = Keypair::generate().public_key();
        let conn = state.db.lock().unwrap();
        members::register_member(&conn, &pk, name).unwrap();
        let everyone = everyone_role_id_with(&conn);
        members::assign_role(&conn, &pk, everyone).unwrap();
        pk
    }

    fn text_channel(state: &ServerState, name: &str) -> u64 {
        let conn = state.db.lock().unwrap();
        channels::create_channel(&conn, name, ChannelType::Text, None, 0).unwrap()
    }

    /// MERGE-BOUNDARY REGRESSION: an E2EE channel must stay SUBSCRIBABLE.
    ///
    /// Two changes met here. `channel_visible` became the Subscribe permission
    /// boundary, and Rung 2 made a non-plaintext channel invisible to widget
    /// requests. Folding the class check into `channel_visible` would have
    /// satisfied both call sites and silently broken the feature: sealed
    /// messages are delivered to SUBSCRIBERS like any other broadcast, so a
    /// class-gated subscribe leaves every member of a sealed channel receiving
    /// nothing, with no error anywhere to say why.
    ///
    /// The class gate therefore lives in `widget_channel_visible` only. This
    /// test pins the half that has no other test: class does NOT gate subscribe.
    #[tokio::test]
    async fn an_e2ee_channel_is_still_subscribable() {
        let (state, owner) = setup().await;
        let sealed = text_channel(&state, "sealed");
        {
            let conn = state.db.lock().unwrap();
            crate::channel_class::set_class(
                &conn,
                sealed,
                farder_crypto::event_log::ChannelClass::E2ee,
            )
            .unwrap();
        }

        let allowed = {
            let conn = state.db.lock().unwrap();
            visible(&state, &conn, &owner, true, sealed)
        };
        assert!(
            allowed,
            "a sealed channel became unsubscribable — its members would receive \
             no sealed messages at all"
        );

        // ...and the widget surface still cannot see it, so the class gate did
        // not simply get deleted.
        let widget_sees = {
            let conn = state.db.lock().unwrap();
            crate::handlers::widget_channel_visible_for_test(&conn, &owner, sealed, true).unwrap()
        };
        assert!(!widget_sees, "widgets must not see a sealed channel");
    }

    fn everyone_role_id_with(conn: &Connection) -> u64 {
        members::list_roles(conn)
            .unwrap()
            .into_iter()
            .find(|r| r.name == "@everyone")
            .unwrap()
            .id
    }

    /// Deny `VIEW_CHANNEL` to `@everyone` on this channel — i.e. make it private.
    fn make_private(state: &ServerState, channel_id: u64) {
        let conn = state.db.lock().unwrap();
        let role_id = everyone_role_id_with(&conn);
        channels::set_channel_override(&conn, channel_id, role_id, 0, permissions::VIEW_CHANNEL)
            .unwrap();
    }

    async fn is_subscribed(state: &ServerState, channel_id: u64, pk: &PublicKey) -> bool {
        state
            .subscriptions
            .read()
            .await
            .get(&channel_id)
            .map(|s| s.contains(pk.as_bytes()))
            .unwrap_or(false)
    }

    /// Register a live client and return its event receiver, so a test can
    /// observe what the member's connection *actually* receives.
    async fn connect(state: &ServerState, pk: &PublicKey) -> mpsc::Receiver<ServerEvent> {
        let (tx, rx) = mpsc::channel::<ServerEvent>(64);
        state.clients.write().await.insert(*pk.as_bytes(), tx);
        rx
    }

    /// Post a real message to `channel_id` exactly the way the `SendMessage`
    /// handler does: insert the row, load the `MessageInfo`, and broadcast
    /// `NewMessage` to `EventTarget::Subscribers(channel_id)`.
    async fn post(state: &ServerState, channel_id: u64) {
        let author = Keypair::generate().public_key();
        let message = {
            let conn = state.db.lock().unwrap();
            let id =
                messages::insert_message(&conn, channel_id, &author, "secret plaintext", None)
                    .unwrap();
            messages::get_message(&conn, id, &author).unwrap().unwrap()
        };
        broadcast_event(
            state,
            EventTarget::Subscribers(channel_id),
            ServerEvent::NewMessage { message },
        )
        .await;
    }

    /// Did this connection actually receive a `NewMessage` for `channel_id`?
    /// Drains everything queued, so unrelated control events (the `MemberLeft`
    /// / `PermissionsChanged` a revocation test broadcasts to `All`) can never
    /// be mistaken for message delivery.
    fn received_post(rx: &mut mpsc::Receiver<ServerEvent>, channel_id: u64) -> bool {
        let mut seen = false;
        while let Ok(ev) = rx.try_recv() {
            if let ServerEvent::NewMessage { message } = ev {
                if message.channel_id == channel_id {
                    seen = true;
                }
            }
        }
        seen
    }

    // -----------------------------------------------------------------------
    // Admission
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn subscribe_to_private_channel_without_view_channel_is_dropped() {
        let (state, _owner) = setup().await;
        let secret = text_channel(&state, "secret");
        make_private(&state, secret);
        let snoop = add_member(&state, "Snoop");
        let mut rx = connect(&state, &snoop).await;

        apply_subscribe(&state, &snoop, false, vec![secret]).await;

        assert!(
            !is_subscribed(&state, secret, &snoop).await,
            "member without VIEW_CHANNEL must not be in the subscription set for a private channel"
        );
        post(&state, secret).await;
        assert!(
            !received_post(&mut rx, secret),
            "member without VIEW_CHANNEL received a broadcast for a private channel"
        );
    }

    #[tokio::test]
    async fn subscribe_to_someone_elses_dm_is_dropped() {
        let (state, _owner) = setup().await;
        let alice = add_member(&state, "Alice");
        let bob = add_member(&state, "Bob");
        let snoop = add_member(&state, "Snoop");
        let dm = {
            let conn = state.db.lock().unwrap();
            channels::create_dm_channel(&conn, &alice, &bob).unwrap()
        };
        let mut rx = connect(&state, &snoop).await;

        apply_subscribe(&state, &snoop, false, vec![dm]).await;

        assert!(
            !is_subscribed(&state, dm, &snoop).await,
            "non-participant must not be in the subscription set for a DM channel"
        );
        post(&state, dm).await;
        assert!(
            !received_post(&mut rx, dm),
            "non-participant received a broadcast for someone else's DM"
        );
    }

    #[tokio::test]
    async fn subscribe_to_unknown_channel_is_dropped_silently() {
        let (state, _owner) = setup().await;
        let snoop = add_member(&state, "Snoop");

        // No error, no panic — just nothing subscribed (no existence oracle).
        apply_subscribe(&state, &snoop, false, vec![9999]).await;

        assert!(!is_subscribed(&state, 9999, &snoop).await);
    }

    #[tokio::test]
    async fn mixed_request_keeps_the_allowed_ids_and_drops_the_rest() {
        let (state, _owner) = setup().await;
        let public = text_channel(&state, "general");
        let secret = text_channel(&state, "secret");
        make_private(&state, secret);
        let m = add_member(&state, "M");

        apply_subscribe(&state, &m, false, vec![public, secret]).await;

        assert!(
            is_subscribed(&state, public, &m).await,
            "a stale/greedy channel list must not cost the member their legitimate subscriptions"
        );
        assert!(!is_subscribed(&state, secret, &m).await);
    }

    // -----------------------------------------------------------------------
    // No regression for legitimate subscribers
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn ordinary_member_owner_and_dm_participants_still_receive() {
        let (state, owner) = setup().await;
        let general = text_channel(&state, "general");
        let alice = add_member(&state, "Alice");
        let bob = add_member(&state, "Bob");
        let dm = {
            let conn = state.db.lock().unwrap();
            channels::create_dm_channel(&conn, &alice, &bob).unwrap()
        };

        let mut rx_alice = connect(&state, &alice).await;
        let mut rx_owner = connect(&state, &owner).await;
        apply_subscribe(&state, &alice, false, vec![general]).await;
        apply_subscribe(&state, &owner, true, vec![general]).await;
        post(&state, general).await;
        assert!(received_post(&mut rx_alice, general), "ordinary member must receive");
        assert!(received_post(&mut rx_owner, general), "owner must receive");

        // DM: both participants.
        let mut rx_a = connect(&state, &alice).await;
        let mut rx_b = connect(&state, &bob).await;
        apply_subscribe(&state, &alice, false, vec![dm]).await;
        apply_subscribe(&state, &bob, false, vec![dm]).await;
        post(&state, dm).await;
        assert!(received_post(&mut rx_a, dm), "DM participant A must receive");
        assert!(received_post(&mut rx_b, dm), "DM participant B must receive");
    }

    #[tokio::test]
    async fn owner_may_subscribe_to_a_private_channel() {
        let (state, owner) = setup().await;
        let secret = text_channel(&state, "secret");
        make_private(&state, secret);
        let mut rx = connect(&state, &owner).await;

        apply_subscribe(&state, &owner, true, vec![secret]).await;

        assert!(is_subscribed(&state, secret, &owner).await);
        post(&state, secret).await;
        assert!(received_post(&mut rx, secret), "owner must still see private channels");
    }

    // -----------------------------------------------------------------------
    // Revocation — subscribe legitimately, then lose access
    // -----------------------------------------------------------------------

    /// Subscribe `m` to a fresh public channel and assert it works, so each
    /// revocation test starts from a genuinely live subscription.
    async fn live_subscription(
        state: &ServerState,
        m: &PublicKey,
    ) -> (u64, mpsc::Receiver<ServerEvent>) {
        let channel_id = text_channel(state, "general");
        let mut rx = connect(state, m).await;
        apply_subscribe(state, m, false, vec![channel_id]).await;
        post(state, channel_id).await;
        assert!(received_post(&mut rx, channel_id), "precondition: subscription must be live");
        (channel_id, rx)
    }

    #[tokio::test]
    async fn kick_revokes_the_subscription() {
        let (state, owner) = setup().await;
        let m = add_member(&state, "M");
        let (channel_id, mut rx) = live_subscription(&state, &m).await;

        {
            let conn = state.db.lock().unwrap();
            members::remove_member(&conn, &m).unwrap();
        }
        broadcast_event(
            &state,
            EventTarget::All,
            ServerEvent::MemberLeft { public_key: m.clone() },
        )
        .await;
        let _ = owner;

        assert!(!is_subscribed(&state, channel_id, &m).await);
        post(&state, channel_id).await;
        assert!(!received_post(&mut rx, channel_id), "a kicked member kept receiving channel traffic");
    }

    #[tokio::test]
    async fn ban_revokes_the_subscription() {
        let (state, _owner) = setup().await;
        let m = add_member(&state, "M");
        let (channel_id, mut rx) = live_subscription(&state, &m).await;

        {
            let conn = state.db.lock().unwrap();
            members::ban_member(&conn, &m, Some("spam")).unwrap();
        }
        broadcast_event(
            &state,
            EventTarget::All,
            ServerEvent::MemberBanned { public_key: m.clone(), reason: Some("spam".into()) },
        )
        .await;

        assert!(!is_subscribed(&state, channel_id, &m).await);
        post(&state, channel_id).await;
        assert!(!received_post(&mut rx, channel_id), "a banned member kept receiving channel traffic");
    }

    #[tokio::test]
    async fn channel_override_removing_view_channel_revokes_the_subscription() {
        let (state, _owner) = setup().await;
        let m = add_member(&state, "M");
        let (channel_id, mut rx) = live_subscription(&state, &m).await;

        make_private(&state, channel_id);
        broadcast_event(&state, EventTarget::All, ServerEvent::PermissionsChanged).await;

        assert!(!is_subscribed(&state, channel_id, &m).await);
        post(&state, channel_id).await;
        assert!(
            !received_post(&mut rx, channel_id),
            "member kept receiving after the channel was made private under them"
        );
    }

    #[tokio::test]
    async fn role_update_removing_view_channel_revokes_the_subscription() {
        let (state, _owner) = setup().await;
        let m = add_member(&state, "M");
        let channel_id = text_channel(&state, "staff");
        // Private to @everyone, visible only via the "Staff" role's override.
        make_private(&state, channel_id);
        let staff = {
            let conn = state.db.lock().unwrap();
            let staff = members::create_role(&conn, "Staff", 0, None, 1, false, false).unwrap();
            channels::set_channel_override(&conn, channel_id, staff, permissions::VIEW_CHANNEL, 0)
                .unwrap();
            members::assign_role(&conn, &m, staff).unwrap();
            staff
        };
        let mut rx = connect(&state, &m).await;
        apply_subscribe(&state, &m, false, vec![channel_id]).await;
        post(&state, channel_id).await;
        assert!(received_post(&mut rx, channel_id), "precondition: Staff can see the staff channel");

        // Take VIEW_CHANNEL away from the role's channel override.
        {
            let conn = state.db.lock().unwrap();
            channels::set_channel_override(&conn, channel_id, staff, 0, permissions::VIEW_CHANNEL)
                .unwrap();
        }
        let role = {
            let conn = state.db.lock().unwrap();
            members::get_role(&conn, staff).unwrap().unwrap()
        };
        broadcast_event(&state, EventTarget::All, ServerEvent::RoleUpdated { role }).await;

        assert!(!is_subscribed(&state, channel_id, &m).await);
        post(&state, channel_id).await;
        assert!(!received_post(&mut rx, channel_id), "member kept receiving after their role lost VIEW_CHANNEL");
    }

    #[tokio::test]
    async fn role_removal_revokes_the_subscription() {
        let (state, _owner) = setup().await;
        let m = add_member(&state, "M");
        let channel_id = text_channel(&state, "staff");
        make_private(&state, channel_id);
        let staff = {
            let conn = state.db.lock().unwrap();
            let staff = members::create_role(&conn, "Staff", 0, None, 1, false, false).unwrap();
            channels::set_channel_override(&conn, channel_id, staff, permissions::VIEW_CHANNEL, 0)
                .unwrap();
            members::assign_role(&conn, &m, staff).unwrap();
            staff
        };
        let mut rx = connect(&state, &m).await;
        apply_subscribe(&state, &m, false, vec![channel_id]).await;
        post(&state, channel_id).await;
        assert!(received_post(&mut rx, channel_id), "precondition: Staff can see the staff channel");

        {
            let conn = state.db.lock().unwrap();
            members::unassign_role(&conn, &m, staff).unwrap();
        }
        broadcast_event(&state, EventTarget::All, ServerEvent::PermissionsChanged).await;

        assert!(!is_subscribed(&state, channel_id, &m).await);
        post(&state, channel_id).await;
        assert!(!received_post(&mut rx, channel_id), "member kept receiving after losing the role that granted access");
    }

    #[tokio::test]
    async fn channel_delete_revokes_the_subscription() {
        let (state, _owner) = setup().await;
        let m = add_member(&state, "M");
        let (channel_id, mut rx) = live_subscription(&state, &m).await;

        {
            let conn = state.db.lock().unwrap();
            channels::soft_delete_channel(&conn, channel_id).unwrap();
        }
        broadcast_event(&state, EventTarget::All, ServerEvent::ChannelDeleted { channel_id }).await;

        assert!(!is_subscribed(&state, channel_id, &m).await);
        post(&state, channel_id).await;
        assert!(!received_post(&mut rx, channel_id), "member kept receiving from a deleted channel");
    }

    #[tokio::test]
    async fn mesh_member_removal_revokes_the_subscription() {
        use farder_crypto::event_log::Genesis;
        use farder_crypto::event_log_state::LogState;

        let (state, owner) = setup().await;
        let m = add_member(&state, "M");
        let (channel_id, mut rx) = live_subscription(&state, &m).await;

        // Turn this into a mesh server whose log knows the owner but not `m`:
        // exactly the state the log reaches after `MemberRemoved { m }`.
        let g = Genesis {
            version: 1,
            name: "mesh".into(),
            owner: owner.clone(),
            created_at: 1,
            nonce: [0u8; 16],
        };
        *state.log_state.lock().unwrap() = Some(LogState::from_genesis(&g));
        broadcast_event(
            &state,
            EventTarget::All,
            ServerEvent::MembershipChanged { public_key: m.clone() },
        )
        .await;

        assert!(!is_subscribed(&state, channel_id, &m).await);
        post(&state, channel_id).await;
        assert!(
            !received_post(&mut rx, channel_id),
            "a member removed from the mesh log kept receiving channel traffic"
        );
    }

    // -----------------------------------------------------------------------
    // Revocation must not over-prune
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn revalidate_keeps_members_who_still_have_access() {
        let (state, _owner) = setup().await;
        let m = add_member(&state, "M");
        let other = add_member(&state, "Other");
        let (channel_id, mut rx) = live_subscription(&state, &m).await;

        // Somebody *else* is kicked; `m` must be untouched.
        {
            let conn = state.db.lock().unwrap();
            members::remove_member(&conn, &other).unwrap();
        }
        broadcast_event(
            &state,
            EventTarget::All,
            ServerEvent::MemberLeft { public_key: other },
        )
        .await;

        assert!(is_subscribed(&state, channel_id, &m).await);
        post(&state, channel_id).await;
        assert!(received_post(&mut rx, channel_id), "an unrelated kick must not drop a valid subscription");
    }
}
