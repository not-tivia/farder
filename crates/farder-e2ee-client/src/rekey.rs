//! C2 of the 5a lifecycle: the rekey primitive and its cadence policy.
//!
//! A *rekey* is [`MlsChannelGroup::self_update`] — the author rotates its own
//! leaf secrets with an empty add/remove set — submitted as an ordinary
//! `MlsCommit`. [`rekey_channel`] is that primitive: run `self_update`, submit
//! the empty-adds/removes commit with the real chaining values, and advance the
//! chain on acceptance. It deliberately does **not** loop: a commit-rate
//! rejection is surfaced as [`E2eeError::RekeyRateLimited`] for the caller to
//! decide when (not whether) to retry.
//!
//! # The client has no fold `LogState`
//!
//! The commit-rate rule, `ceiling_demands_rekey`, and the pending-removals /
//! freshness-ceiling send gates are all server-side counters
//! (`event_log_state.rs:1187-1203, 267-270, 1443-1453`). This crate cannot
//! query them. The rekey therefore keys on two **reactive** signals the client
//! CAN observe — the exact fold REJECTION strings — plus a **local cadence**:
//!
//! - **Reactive:** a sealed send rejected with `"freshness ceiling reached"`
//!   is the fold's guarantee that the commit-rate rule now stands aside
//!   (`ceiling_demands_rekey()`), so a rekey is permitted. [`should_rekey`]
//!   maps that signal to an unconditional [`RekeyTrigger::CeilingSignalled`].
//! - **Proactive:** after [`REKEY_SEALED_SEND_INTERVAL`] of the device's own
//!   sealed sends, or [`REKEY_WALL_CLOCK_SECS`] since its last rekey,
//!   [`should_rekey`] attempts a rekey — but only when
//!   [`rekey_permitted_by_rate_rule`] says the commit-rate rule would accept
//!   it, otherwise it holds.
//!
//! [`should_rekey`] is a pure, loop-free decision function: it terminates under
//! every input and can never spin. The ceiling override is the anti-deadlock
//! property — no matter how far behind the local cadence is, a ceiling signal
//! always permits a rekey, so the policy can never reach the recurring
//! "over-conservative guard creates an unexitable state" bug class.

use farder_crypto::event_log::EventPayload;
use farder_crypto::event_log_state::COMMIT_RATE_MIN_EPOCH_GAP;
use farder_mls::credential::DeviceSigner;
use farder_mls::group::MlsChannelGroup;
use farder_mls::store::FarderMlsStore;

use crate::chain::{build_next_event, event_now_secs, Actor, ChainState};
use crate::channel::E2eeError;
use crate::channel_key::ChannelKey;
use crate::transport::E2eeTransport;

/// Proactive rekey cadence: attempt a rekey after this many of the device's
/// **own** sealed sends since its last own commit. Picked well below the
/// freshness ceiling (500 channel events, `event_log_state.rs:49`) so a
/// moderately-active member refreshes the channel's forward-secrecy budget
/// before it runs out, while staying far enough above zero that a rekey is not
/// attempted on every send. This is a local proxy for the server-side
/// `events_since_last_commit` the client cannot read; the `"freshness ceiling
/// reached"` rejection remains the authoritative backstop.
pub const REKEY_SEALED_SEND_INTERVAL: u64 = 100;

/// Proactive rekey cadence: attempt a rekey at most once per this many wall
/// clock seconds (one week), for low-traffic channels whose sealed-send counter
/// advances slowly. A caller passes `last_rekey_secs = 0` for "never rekeyed";
/// [`should_rekey`] treats that as immediately due.
pub const REKEY_WALL_CLOCK_SECS: u64 = 7 * 24 * 60 * 60;

/// The fixed inputs for one rekey: the channel, its generation, and the MLS
/// store plus its instance hash (which always travel together). Mirrors
/// [`crate::commit::StewardContext`], but a rekey is a **self** commit, not a
/// steward commit, so the type is named for the operation it serves.
pub struct RekeyContext<'a> {
    pub key: &'a ChannelKey,
    pub generation: u64,
    pub store: &'a FarderMlsStore,
    pub store_instance_hash: &'a [u8; 32],
}

/// The result of a successful [`rekey_channel`].
///
/// As with [`crate::channel::CommitSubmitted`], `local_epoch` is the **local**
/// group's post-merge epoch; acceptance is not independently proof that the
/// server advanced (a commit that lost the epoch race is accepted as a no-op —
/// but the ingest pre-check makes that no-op path unreachable for a live
/// submit, which instead returns the bare `"stale-epoch"`).
#[derive(Debug)]
pub struct RekeyOutcome {
    /// Server-assigned hash of the accepted `MlsCommit` event.
    pub event_hash: String,
    /// The epoch the LOCAL group is in after merging the rekey (one past the
    /// authored epoch).
    pub local_epoch: u64,
    /// The group's epoch authenticator after the rekey.
    pub post_epoch_authenticator: [u8; 32],
    /// The group's tree hash after the rekey.
    pub post_tree_hash: [u8; 32],
}

/// The client's local, persisted belief about its rekey cadence for one
/// channel — the inputs [`should_rekey`] needs, none of which require a fold
/// query. The caller owns persistence (this crate owns no storage).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RekeyCadence {
    /// Whether this device's identity has authored a commit in this channel
    /// before (the fold's "author's first commit" exemption). `true` from the
    /// bootstrap commit onward.
    pub has_committed: bool,
    /// The epoch of this device's most recent own commit (meaningful only when
    /// `has_committed`).
    pub last_commit_epoch: u64,
    /// The group's current epoch (`MlsChannelGroup::epoch()`).
    pub current_epoch: u64,
    /// The client's estimate of the fold's `committing_identities()` — the
    /// number of distinct identities holding confirmed leaves. Derived locally
    /// from `MlsChannelGroup::members()` (count distinct identities, `>= 1`).
    /// An underestimate only costs a [`E2eeError::RekeyRateLimited`] (surfaced,
    /// not looped); it never blocks a ceiling-driven rekey.
    pub committing_identities: u64,
    /// Sealed sends this device has made since its last own commit.
    pub sealed_sends_since_last_commit: u64,
    /// Unix seconds of this device's last rekey (`0` = never).
    pub last_rekey_secs: u64,
}

/// Why [`should_rekey`] decided to rekey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RekeyTrigger {
    /// A sealed send was rejected with `"freshness ceiling reached"`: the fold
    /// guarantees the commit-rate rule stands aside, so rekey now.
    CeilingSignalled,
    /// A proactive cadence threshold fired and the commit-rate rule stands
    /// aside.
    Proactive,
}

/// Why [`should_rekey`] decided to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldReason {
    /// No proactive threshold has fired yet and the ceiling is not signalled.
    Cadence,
    /// A proactive threshold fired, but the commit-rate rule would reject a
    /// rekey authored now (already committed, epoch gap not yet reached).
    RateRule,
}

/// The result of [`should_rekey`]: rekey now, or hold and for what reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RekeyDecision {
    Rekey(RekeyTrigger),
    Hold(HoldReason),
}

/// Does the commit-rate rule permit a rekey (a non-drift-discharging,
/// empty-adds/removes `MlsCommit`) authored by this device at `current_epoch`?
///
/// Mirrors `event_log_state.rs:1187-1203` (authority-note fact 1) exactly, but
/// re-derived from the client's local knowledge because this crate holds no
/// fold `LogState`. The rule permits the commit when the freshness ceiling
/// demands a rekey (`ceiling_signalled`), OR it is the author's first commit
/// (`!has_committed`), OR the declared epoch is at least `gap` epochs past the
/// author's previous commit, where `gap = min(COMMIT_RATE_MIN_EPOCH_GAP,
/// committing_identities)`.
///
/// This is total under every input: `gap` is clamped to `>= 1` and the epoch
/// comparison uses saturating arithmetic, so a caller feeding `u64::MAX`
/// epochs (or a bogus `committing_identities`) gets a decision, never a panic.
pub fn rekey_permitted_by_rate_rule(
    has_committed: bool,
    last_commit_epoch: u64,
    current_epoch: u64,
    committing_identities: u64,
    ceiling_signalled: bool,
) -> bool {
    // ceiling_demands_rekey(): the freshness ceiling is the fold's own "rekey
    // now" override, which stands the rate rule aside.
    if ceiling_signalled {
        return true;
    }
    // The author's first commit in this channel is exempt.
    if !has_committed {
        return true;
    }
    // gap = min(COMMIT_RATE_MIN_EPOCH_GAP, committing_identities), clamped to 1
    // so a zero/bogus estimate degrades to the freest (single-identity) gap.
    let gap = COMMIT_RATE_MIN_EPOCH_GAP.min(committing_identities.max(1));
    current_epoch >= last_commit_epoch.saturating_add(gap)
}

/// Decide whether to rekey now, from local knowledge only (pure, no I/O).
///
/// - Reactive: `ceiling_signalled` (a sealed send bounced with `"freshness
///   ceiling reached"`) forces a rekey — the fold guarantees it is permitted.
/// - Proactive: when either [`REKEY_SEALED_SEND_INTERVAL`] sealed sends or
///   [`REKEY_WALL_CLOCK_SECS`] have elapsed since the last rekey, rekey — but
///   only if [`rekey_permitted_by_rate_rule`] says the commit-rate rule would
///   accept it; otherwise hold with [`HoldReason::RateRule`] rather than
///   round-trip a doomed commit.
///
/// This function has no loop and is total, so it terminates under every input.
/// The ceiling signal is the anti-deadlock backstop: it returns
/// [`RekeyTrigger::CeilingSignalled`] regardless of the cadence state.
pub fn should_rekey(ceiling_signalled: bool, cadence: &RekeyCadence, now_secs: u64) -> RekeyDecision {
    if ceiling_signalled {
        return RekeyDecision::Rekey(RekeyTrigger::CeilingSignalled);
    }

    let proactive_due = cadence.last_rekey_secs == 0
        || now_secs.saturating_sub(cadence.last_rekey_secs) >= REKEY_WALL_CLOCK_SECS
        || cadence.sealed_sends_since_last_commit >= REKEY_SEALED_SEND_INTERVAL;

    if !proactive_due {
        return RekeyDecision::Hold(HoldReason::Cadence);
    }

    if rekey_permitted_by_rate_rule(
        cadence.has_committed,
        cadence.last_commit_epoch,
        cadence.current_epoch,
        cadence.committing_identities,
        false,
    ) {
        RekeyDecision::Rekey(RekeyTrigger::Proactive)
    } else {
        RekeyDecision::Hold(HoldReason::RateRule)
    }
}

/// Rekey one channel: run [`MlsChannelGroup::self_update`] and submit the
/// resulting empty-adds/removes `MlsCommit`, mirroring
/// [`crate::channel::bootstrap_group`]'s submit + chain-advance.
///
/// `group` is the caller's already-loaded group (this crate's convention: load
/// via [`MlsChannelGroup::load`] or resume, then pass `&mut` in — see
/// `bootstrap_group` / [`crate::commit::add_member`]).
///
/// # Divergence contract
///
/// `self_update` merges **locally and immediately**, so by the time this fn
/// submits, the local group is already one epoch ahead. A `"stale-epoch"`
/// rejection therefore surfaces [`E2eeError::StaleEpochDiverged`]; a
/// `"commit-rate rule:"` rejection surfaces [`E2eeError::RekeyRateLimited`] —
/// the caller must not reuse either advanced group until it resyncs. Neither
/// rejection is retried here (no loop); the cadence policy decides the next
/// attempt.
pub async fn rekey_channel<T: E2eeTransport + Sync>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    ctx: &RekeyContext<'_>,
    group: &mut MlsChannelGroup,
) -> Result<RekeyOutcome, E2eeError> {
    // A rekey attests this device's folded log head; a device that has never
    // committed has nothing to attest and cannot rekey (it bootstraps first).
    let authz_head = chain
        .last_event_hash
        .clone()
        .ok_or_else(|| E2eeError::chain("rekey needs a prior event to attest its folded head"))?;

    // 1. Perform the commit locally. self_update merges immediately.
    let outcome = group
        .self_update(ctx.store, &DeviceSigner(actor.device))
        .map_err(|e| E2eeError::Mls(e.context("rekey self-update")))?;
    debug_assert!(outcome.adds.is_empty() && outcome.removes.is_empty());
    let post_epoch_authenticator = group.epoch_authenticator();
    debug_assert_eq!(group.epoch(), outcome.epoch + 1);

    // 2. Build the MlsCommit from the real CommitOutcome: a rekey is empty
    //    adds/removes by definition.
    let event = build_next_event(
        actor.device,
        actor.identity,
        &ctx.key.log_server_id,
        chain,
        event_now_secs(),
        EventPayload::MlsCommit {
            channel_id: ctx.key.channel_id,
            generation: ctx.generation,
            epoch: outcome.epoch,
            mls_message: outcome.commit_bytes,
            adds: vec![],
            removes: vec![],
            prev_epoch_authenticator: outcome.prev_epoch_authenticator,
            post_epoch_authenticator,
            post_tree_hash: outcome.post_tree_hash,
            authz_head,
            store_instance_hash: *ctx.store_instance_hash,
        },
    );

    // 3. Submit; surface the two load-bearing rejections distinctly, and never
    //    loop on either.
    let accepted = match transport.submit_event(&event).await {
        Ok(a) => a,
        Err(e) if e.is_stale_epoch() => {
            return Err(E2eeError::StaleEpochDiverged {
                local_epoch: group.epoch(),
            });
        }
        Err(e) if e.is_commit_rate_limited() => {
            return Err(E2eeError::RekeyRateLimited {
                reason: e.rejection_reason().to_string(),
            });
        }
        Err(e) => return Err(e.into()),
    };
    chain.advance(&event);

    Ok(RekeyOutcome {
        event_hash: accepted.event_hash,
        local_epoch: group.epoch(),
        post_epoch_authenticator,
        post_tree_hash: outcome.post_tree_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::Actor;
    use crate::channel::{bootstrap_group, create_e2ee_channel, ChannelSpec};
    use crate::testing::FakeTransport;
    use farder_crypto::event_log::{E2EE_CHANNEL_ID_FLOOR, EventPayload};
    use farder_crypto::identity::Keypair;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    const SERVER_ID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let n = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "farder-e2ee-client-rekey-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn key(channel_id: u64) -> ChannelKey {
        ChannelKey::new(SERVER_ID.to_string(), channel_id).unwrap()
    }

    fn actor<'a>(identity: &'a Keypair, device: &'a Keypair) -> Actor<'a> {
        Actor {
            device,
            identity,
            log_server_id: SERVER_ID,
        }
    }

    fn spec(k: ChannelKey) -> ChannelSpec {
        ChannelSpec {
            key: k,
            name: "vault".to_string(),
            kind: "text".to_string(),
            parent: None,
        }
    }

    /// A channel created + bootstrapped, ready to rekey (group at epoch 1).
    struct Bootstrapped {
        transport: FakeTransport,
        identity: Keypair,
        device: Keypair,
        chain: ChainState,
        key: ChannelKey,
        store: FarderMlsStore,
        store_instance_hash: [u8; 32],
        group: MlsChannelGroup,
        bootstrap: crate::channel::CommitSubmitted,
    }

    async fn bootstrapped(dir_name: &str, channel_id: u64) -> Bootstrapped {
        let transport = FakeTransport::new();
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let a = actor(&identity, &device);
        let mut chain = ChainState::default();
        let dir = temp_dir(dir_name);
        let k = key(channel_id);
        let s = spec(k.clone());

        let mut created = create_e2ee_channel(&transport, &a, &mut chain, &s, &dir)
            .await
            .unwrap();
        let bootstrap = bootstrap_group(
            &transport,
            &a,
            &mut chain,
            &k,
            &mut created.group,
            &created.store,
            &created.store_instance_hash,
        )
        .await
        .unwrap();

        Bootstrapped {
            transport,
            identity,
            device,
            chain,
            key: k,
            store: created.store,
            store_instance_hash: created.store_instance_hash,
            group: created.group,
            bootstrap,
        }
    }

    // ---- rekey_channel: the primitive ----

    #[tokio::test]
    async fn rekey_channel_advances_epoch_and_submits_an_empty_commit_with_real_chaining() {
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 21;
        let mut b = bootstrapped("ok", channel_id).await;
        let a = actor(&b.identity, &b.device);
        let ctx = RekeyContext {
            key: &b.key,
            generation: 0,
            store: &b.store,
            store_instance_hash: &b.store_instance_hash,
        };

        let outcome = rekey_channel(&b.transport, &a, &mut b.chain, &ctx, &mut b.group)
            .await
            .unwrap();

        // Local group advanced 1 -> 2.
        assert_eq!(outcome.local_epoch, 2);
        assert_eq!(b.group.epoch(), 2);

        let last = b.transport.submitted().into_iter().last().expect("one rekey commit");
        match &last.core.payload {
            EventPayload::MlsCommit {
                channel_id: cid,
                generation,
                epoch,
                adds,
                removes,
                prev_epoch_authenticator,
                post_epoch_authenticator,
                post_tree_hash,
                authz_head,
                store_instance_hash,
                ..
            } => {
                assert_eq!(*cid, channel_id);
                assert_eq!(*generation, 0);
                assert_eq!(*epoch, 1, "authored in the pre-merge epoch");
                assert!(adds.is_empty() && removes.is_empty(), "a rekey is empty adds/removes");
                // Chaining: the rekey's prev authenticator is the bootstrap's
                // post authenticator.
                assert_eq!(prev_epoch_authenticator, &b.bootstrap.post_epoch_authenticator);
                assert_eq!(post_epoch_authenticator, &outcome.post_epoch_authenticator);
                assert_eq!(post_tree_hash, &outcome.post_tree_hash);
                assert_ne!(prev_epoch_authenticator, post_epoch_authenticator);
                assert_eq!(store_instance_hash, &b.store_instance_hash);
                // The folded head is the bootstrap commit's event hash (the
                // chain head this device held when it rekeyed).
                assert_eq!(authz_head, &b.bootstrap.event_hash);
            }
            other => panic!("expected MlsCommit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_stale_epoch_rekey_rejection_surfaces_stale_epoch_diverged() {
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 23;
        let mut b = bootstrapped("stale", channel_id).await;
        let a = actor(&b.identity, &b.device);
        let ctx = RekeyContext {
            key: &b.key,
            generation: 0,
            store: &b.store,
            store_instance_hash: &b.store_instance_hash,
        };

        b.transport.reject_next("stale-epoch");

        let err = rekey_channel(&b.transport, &a, &mut b.chain, &ctx, &mut b.group)
            .await
            .unwrap_err();

        assert!(err.is_stale_epoch_diverged(), "expected divergence, got {err}");
        match err {
            E2eeError::StaleEpochDiverged { local_epoch } => {
                // The local group already advanced to epoch 2 (ahead of the
                // server).
                assert_eq!(local_epoch, 2);
            }
            other => panic!("expected StaleEpochDiverged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_commit_rate_rejection_surfaces_rekey_rate_limited_and_does_not_loop() {
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 25;
        let mut b = bootstrapped("rate", channel_id).await;
        let a = actor(&b.identity, &b.device);
        let ctx = RekeyContext {
            key: &b.key,
            generation: 0,
            store: &b.store,
            store_instance_hash: &b.store_instance_hash,
        };

        b.transport.reject_next(
            "event rejected: commit-rate rule: a non-drift-discharging commit \
             must be its author's first or at least 2 epochs past their previous one",
        );
        let submits_before = b.transport.submit_count();

        let err = rekey_channel(&b.transport, &a, &mut b.chain, &ctx, &mut b.group)
            .await
            .unwrap_err();

        assert!(err.is_rekey_rate_limited(), "expected rate-limit, got {err}");
        match err {
            E2eeError::RekeyRateLimited { reason } => {
                assert!(reason.contains("commit-rate rule:"), "reason preserved: {reason}");
            }
            other => panic!("expected RekeyRateLimited, got {other:?}"),
        }
        // Exactly ONE rekey submit happened — no loop, no spin.
        assert_eq!(b.transport.submit_count(), submits_before + 1);
    }

    #[tokio::test]
    async fn rekey_requires_a_prior_event_to_attest_its_folded_head() {
        let transport = FakeTransport::new();
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let a = actor(&identity, &device);
        let mut chain = ChainState::default(); // empty: no prior event
        let k = key(E2EE_CHANNEL_ID_FLOOR + 27);
        let dir = temp_dir("no-prev");

        // Build a store + group directly (bypassing create/bootstrap so the
        // chain stays empty).
        let store_path = k.mls_store_path(&dir).unwrap();
        std::fs::create_dir_all(store_path.parent().unwrap()).unwrap();
        let (store, _) = FarderMlsStore::create(&store_path).unwrap();
        let mut group = MlsChannelGroup::create(
            &store,
            &DeviceSigner(&device),
            farder_mls::credential::credential_with_key(&device, &identity.public_key()),
            crate::channel::channel_group_id(SERVER_ID, k.channel_id, 0).as_bytes(),
        )
        .unwrap();
        let ctx = RekeyContext {
            key: &k,
            generation: 0,
            store: &store,
            store_instance_hash: &store.store_instance_hash(),
        };

        let err = rekey_channel(&transport, &a, &mut chain, &ctx, &mut group)
            .await
            .unwrap_err();

        assert!(matches!(err, E2eeError::Chain(_)));
        assert_eq!(transport.submit_count(), 0);
    }

    // ---- cadence decision: pure, no I/O ----

    #[test]
    fn e2ee_error_freshness_ceiling_predicate_keys_on_the_transport_reason() {
        // The reactive trigger: a sealed send bounced with the ceiling reason is
        // the fold's guarantee a rekey is now permitted.
        let err = E2eeError::Transport(crate::transport::TransportError::rejected(
            "event rejected: freshness ceiling reached: the channel is sealed until somebody rekeys",
        ));
        assert!(err.is_freshness_ceiling_reached());
        assert!(!err.is_rekey_rate_limited());
        assert!(!err.is_stale_epoch_diverged());
    }

    fn cadence() -> RekeyCadence {
        RekeyCadence {
            has_committed: true,
            last_commit_epoch: 0,
            current_epoch: 1,
            committing_identities: 2,
            sealed_sends_since_last_commit: 0,
            last_rekey_secs: 0,
        }
    }

    #[test]
    fn should_rekey_rekeys_immediately_when_the_ceiling_is_signalled() {
        // The ceiling signal is the fold's guarantee the rate rule stands
        // aside: it must rekey even when every proactive gate says hold.
        let mut c = cadence();
        c.sealed_sends_since_last_commit = 0;
        c.current_epoch = 1; // rate rule would reject a proactive rekey here
        assert_eq!(
            should_rekey(true, &c, 1),
            RekeyDecision::Rekey(RekeyTrigger::CeilingSignalled)
        );
    }

    #[test]
    fn should_rekey_holds_when_the_rate_rule_would_reject() {
        // has_committed, last at epoch 0, now at epoch 1, gap = min(4, 2) = 2:
        // the proactive cadence is due but the rate rule would reject — it must
        // hold, never round-trip a doomed rekey.
        let mut c = cadence();
        c.sealed_sends_since_last_commit = REKEY_SEALED_SEND_INTERVAL;
        c.last_rekey_secs = 1_000_000;
        assert_eq!(
            should_rekey(false, &c, 1_000_000 + REKEY_WALL_CLOCK_SECS + 1),
            RekeyDecision::Hold(HoldReason::RateRule)
        );
    }

    #[test]
    fn should_rekey_rekeys_proactively_when_due_and_the_rate_rule_permits() {
        let mut c = cadence();
        c.current_epoch = 2; // 2 >= 0 + gap(2)
        c.sealed_sends_since_last_commit = REKEY_SEALED_SEND_INTERVAL;
        assert_eq!(
            should_rekey(false, &c, 1),
            RekeyDecision::Rekey(RekeyTrigger::Proactive)
        );
    }

    #[test]
    fn should_rekey_rekeys_proactively_on_wall_clock_alone() {
        let mut c = cadence();
        c.last_rekey_secs = 1_000_000;
        c.sealed_sends_since_last_commit = 0; // no send volume
        c.current_epoch = 4; // gap satisfied
        assert_eq!(
            should_rekey(false, &c, 1_000_000 + REKEY_WALL_CLOCK_SECS),
            RekeyDecision::Rekey(RekeyTrigger::Proactive)
        );
    }

    #[test]
    fn should_rekey_holds_on_cadence_when_no_trigger_fired() {
        let mut c = cadence();
        c.last_rekey_secs = 1_000_000;
        c.sealed_sends_since_last_commit = 0;
        c.current_epoch = 4; // rate rule would permit, but nothing is due
        assert_eq!(
            should_rekey(false, &c, 1_000_000 + REKEY_WALL_CLOCK_SECS - 1),
            RekeyDecision::Hold(HoldReason::Cadence)
        );
    }

    #[test]
    fn rekey_permitted_by_rate_rule_mirrors_the_fold() {
        // ceiling override, always permitted
        assert!(rekey_permitted_by_rate_rule(true, 0, 0, 2, true));
        // author's first commit, always permitted
        assert!(rekey_permitted_by_rate_rule(false, 0, 0, 2, false));
        // gap met: gap = min(4, 2) = 2, current 2 >= 0 + 2
        assert!(rekey_permitted_by_rate_rule(true, 0, 2, 2, false));
        // gap NOT met: current 1 < 0 + 2
        assert!(!rekey_permitted_by_rate_rule(true, 0, 1, 2, false));
        // single committing identity => gap 1 => epoch 1 permitted
        assert!(rekey_permitted_by_rate_rule(true, 0, 1, 1, false));
        // committing_identities capped at 4 => gap 4 => epoch 4 permitted, 3 not
        assert!(rekey_permitted_by_rate_rule(true, 0, 4, 100, false));
        assert!(!rekey_permitted_by_rate_rule(true, 0, 3, 100, false));
        // zero committing_identities is clamped to gap 1 (defensive)
        assert!(rekey_permitted_by_rate_rule(true, 0, 1, 0, false));
    }

    #[test]
    fn should_rekey_terminates_and_stays_decidable_under_every_input() {
        // The recurring "over-conservative guard creates an unexitable state"
        // bug class: assert the decision function is total (no overflow panic,
        // no hang) and that the ceiling signal always unblocks — no input can
        // wedge the policy into refusing to rekey forever.
        for ceiling in [false, true] {
            for has_committed in [false, true] {
                for last_commit_epoch in [0u64, 1, 5, u64::MAX - 1, u64::MAX] {
                    for current_epoch in [0u64, 1, 4, 5, u64::MAX - 1, u64::MAX] {
                        for committing_identities in [0u64, 1, 2, 4, 8] {
                            for sends in [0u64, REKEY_SEALED_SEND_INTERVAL - 1, REKEY_SEALED_SEND_INTERVAL] {
                                let c = RekeyCadence {
                                    has_committed,
                                    last_commit_epoch,
                                    current_epoch,
                                    committing_identities,
                                    sealed_sends_since_last_commit: sends,
                                    last_rekey_secs: 0,
                                };
                                let d = should_rekey(ceiling, &c, u64::MAX);
                                // The decision is a valid, finite value.
                                assert!(
                                    matches!(
                                        d,
                                        RekeyDecision::Rekey(_) | RekeyDecision::Hold(_)
                                    )
                                );
                                // Anti-deadlock: a ceiling signal rekeys no
                                // matter the rest of the state.
                                if ceiling {
                                    assert_eq!(
                                        d,
                                        RekeyDecision::Rekey(RekeyTrigger::CeilingSignalled)
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
