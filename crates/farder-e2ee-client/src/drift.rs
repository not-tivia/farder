//! C3 of the 5a lifecycle: drift discharge (remove dead leaves).
//!
//! When a member's device is revoked/expired, or a member is banned/kicked,
//! their leaf becomes *drift*: the fold's `pending_removals` set, which SEALS
//! the channel (`event_log_state.rs:576-587, 1445-1448`) until a remaining
//! confirmed member authors a commit that REMOVES those dead leaves. A rekey
//! (`self_update`) does NOT discharge drift — the fold's
//! `commit_discharges_drift` requires the commit's declared `removes` to
//! intersect `pending_removals` (`event_log_state.rs:636-646`). Drift discharge
//! is therefore a DISTINCT operation from rekey: it is a
//! [`MlsChannelGroup::remove_members`] commit listing the dead
//! `(identity, device)` leaves, submitted via [`discharge_drift`].
//!
//! # The client has no fold `LogState`
//!
//! This crate cannot query `pending_removals` directly (same problem as
//! `crate::rekey`). Drift is learned reactively from two signals:
//!
//! - the sealed-send rejection `"channel is sealed until a rekey discharges its
//!   pending removals"` (verbatim, wrapped as `"event rejected: …"`) — the
//!   [`crate::transport::TransportError::is_sealed_pending_removals`]
//!   / [`crate::channel::E2eeError::is_sealed_pending_removals`] predicate;
//! - the `DeviceRevoked` / `MembershipChanged` broadcasts (S1 made
//!   `DeviceRevoked` broadcast as a server-scoped `MlsControlEvent`; bans
//!   broadcast `MembershipChanged`).
//!
//! Neither signal carries the full dead-leaf set, so the CALLER supplies it,
//! derived from the revocation/ban event. [`dead_leaves_from_revocation`] does
//! the one non-trivial step: the `DeviceRevoked { device }` payload names only
//! the REVOKED device, not the identity that owned it, so the helper resolves
//! the identity half against the group's own member list.
//!
//! # The discharge race (idempotent, no spin)
//!
//! If two members discharge at once, one wins the fold's epoch CAS and the
//! other's submit is rejected with the bare `"stale-epoch"`.
//! [`discharge_drift`] surfaces that as [`E2eeError::StaleEpochDiverged`] —
//! `remove_members` already merged locally, so the loser's group is one epoch
//! ahead of the server and must NOT be reused. It deliberately does **not**
//! loop: the caller re-fetches and retries once via the normal resync
//! (`crate::resync`). Same divergence caveat as rekey — see finding F1.

use farder_crypto::event_log::{DeclaredRemove, EventPayload};
use farder_mls::credential::DeviceSigner;
use farder_mls::group::{DeclaredMember, MlsChannelGroup};
use farder_mls::store::FarderMlsStore;

use crate::chain::{build_next_event, event_now_secs, Actor, ChainState};
use crate::channel::E2eeError;
use crate::channel_key::ChannelKey;
use crate::transport::E2eeTransport;

/// The fixed inputs for one drift discharge: the channel, its generation, and
/// the MLS store plus its instance hash (which always travel together). Mirrors
/// [`crate::rekey::RekeyContext`] and [`crate::commit::StewardContext`]; named
/// for the operation it serves (a remove-commit, not a rekey or a steward add).
pub struct DriftDischargeContext<'a> {
    pub key: &'a ChannelKey,
    pub generation: u64,
    pub store: &'a FarderMlsStore,
    pub store_instance_hash: &'a [u8; 32],
}

/// The result of a successful [`discharge_drift`].
///
/// As with [`crate::channel::CommitSubmitted`] / [`crate::rekey::RekeyOutcome`],
/// `local_epoch` is the **local** group's post-merge epoch; acceptance is not
/// independent proof the server advanced (the ingest pre-check makes the
/// no-op path unreachable for a live submit, which instead returns the bare
/// `"stale-epoch"`).
#[derive(Debug)]
pub struct DriftDischargeOutcome {
    /// Server-assigned hash of the accepted `MlsCommit` event.
    pub event_hash: String,
    /// The epoch the LOCAL group is in after merging the removal (one past the
    /// authored epoch).
    pub local_epoch: u64,
    /// The group's epoch authenticator after the removal.
    pub post_epoch_authenticator: [u8; 32],
    /// The group's tree hash after the removal.
    pub post_tree_hash: [u8; 32],
}

/// Compute the dead-leaf set from a `DeviceRevoked { device }` event.
///
/// The revocation payload names only the REVOKED device (its `device_id`) in
/// its `device` field — NOT the identity that owned it (and NOT the revoker,
/// who is `EventCore.author` / `EventCore.device`). A [`DeclaredMember`] is the
/// pair `(identity, device)`, so the identity half must come from the group's
/// own member list: the caller passes `members` (e.g. `group.members()`), which
/// maps identity -> devices, and this fn returns every leaf whose `device`
/// matches the revoked device id. The result is exactly the set
/// [`discharge_drift`] removes.
///
/// This is deliberately minimal: it does NOT fetch anything, does NOT know
/// which identity owned the device, and returns an empty set when the revoked
/// device is not in the member list (e.g. the device never joined this
/// channel) — the caller decides whether that means "nothing to discharge".
pub fn dead_leaves_from_revocation(
    revoked_device: &str,
    members: &[DeclaredMember],
) -> Vec<DeclaredMember> {
    members
        .iter()
        .filter(|m| m.device.as_str() == revoked_device)
        .cloned()
        .collect()
}

/// Discharge drift: run [`MlsChannelGroup::remove_members`] over the dead
/// leaves and submit the resulting `MlsCommit` with `removes` = those dead
/// leaves, mirroring [`crate::channel::bootstrap_group`] /
/// [`crate::commit::add_member`] / [`crate::rekey::rekey_channel`]'s submit +
/// chain-advance exactly.
///
/// `group` is the caller's already-loaded group (this crate's convention: load
/// via [`MlsChannelGroup::load`] or resume, then pass `&mut` in). The author
/// must be a remaining confirmed member — never one of `dead_leaves`.
///
/// # Divergence contract (the race)
///
/// `remove_members` merges **locally and immediately**, so by the time this fn
/// submits, the local group is already one epoch ahead. If another member
/// discharged first, the submit is rejected with the bare `"stale-epoch"` and
/// this returns [`E2eeError::StaleEpochDiverged`] — the caller must NOT reuse
/// the advanced group; re-fetch and retry once via the normal resync
/// (`crate::resync`). It never loops.
///
/// # The commit-rate rule is a discharge-is-wrong signal
///
/// A genuine drift discharge is EXEMPT from the commit-rate rule
/// (`commit_discharges_drift`, `event_log_state.rs:1193-1199`). So if the fold
/// DOES return `"event rejected: commit-rate rule: …"`, it means the declared
/// `removes` did NOT intersect `pending_removals` — the dead-leaf set was wrong
/// (stale, already discharged, or never drift). That surfaces as
/// [`E2eeError::RekeyRateLimited`] (reusing C2's variant) with the fold's
/// verbatim reason, so the caller knows the removes discharged nothing. Same
/// divergence caveat as [`E2eeError::StaleEpochDiverged`] applies.
pub async fn discharge_drift<T: E2eeTransport + Sync>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    ctx: &DriftDischargeContext<'_>,
    group: &mut MlsChannelGroup,
    dead_leaves: &[DeclaredMember],
) -> Result<DriftDischargeOutcome, E2eeError> {
    // A drift-discharging commit attests this device's folded log head; a
    // device that has never committed has nothing to attest (it bootstraps
    // first).
    let authz_head = chain
        .last_event_hash
        .clone()
        .ok_or_else(|| E2eeError::chain("drift discharge needs a prior event to attest its folded head"))?;

    // Refuse an empty dead-leaf set up front: an empty-removes commit is a
    // rekey, not drift discharge — it would discharge nothing and trip the
    // commit-rate rule. Never emit that silently.
    if dead_leaves.is_empty() {
        return Err(E2eeError::chain(
            "drift discharge needs at least one dead leaf (derive it from the revocation/ban event + the group member list)",
        ));
    }

    // 1. Perform the removal locally. `remove_members` merges immediately and
    //    errors BEFORE any submit when a target leaf is not present (the "no
    //    silent no-op" guarantee — a wrong dead-leaf set never reaches the
    //    wire).
    let outcome = group
        .remove_members(ctx.store, &DeviceSigner(actor.device), dead_leaves)
        .map_err(|e| E2eeError::Mls(e.context("drift discharge remove-members")))?;
    debug_assert!(outcome.adds.is_empty());
    debug_assert!(!outcome.removes.is_empty());
    let post_epoch_authenticator = group.epoch_authenticator();
    debug_assert_eq!(group.epoch(), outcome.epoch + 1);

    // 2. Build the MlsCommit from the real CommitOutcome: empty adds, removes =
    //    the actually-removed leaves (the declared removes the fold checks
    //    against `pending_removals`).
    let removes: Vec<DeclaredRemove> = outcome
        .removes
        .iter()
        .map(|m| DeclaredRemove {
            identity: m.identity.clone(),
            device: m.device.clone(),
        })
        .collect();
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
            removes,
            prev_epoch_authenticator: outcome.prev_epoch_authenticator,
            post_epoch_authenticator,
            post_tree_hash: outcome.post_tree_hash,
            authz_head,
            store_instance_hash: *ctx.store_instance_hash,
        },
    );

    // 3. Submit; map the two load-bearing rejections distinctly and never loop.
    let accepted = match transport.submit_event(&event).await {
        Ok(a) => a,
        Err(e) if e.is_stale_epoch() => {
            return Err(E2eeError::StaleEpochDiverged {
                local_epoch: group.epoch(),
            });
        }
        Err(e) if e.is_commit_rate_limited() => {
            // A genuine drift discharge is commit-rate-exempt, so reaching here
            // means the removes did not discharge anything — the dead-leaf set
            // was wrong. Surface C2's variant with the fold's reason.
            return Err(E2eeError::RekeyRateLimited {
                reason: e.rejection_reason().to_string(),
            });
        }
        Err(e) => return Err(e.into()),
    };
    chain.advance(&event);

    Ok(DriftDischargeOutcome {
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
    use farder_crypto::event_log::{device_id, E2EE_CHANNEL_ID_FLOOR, EventPayload};
    use farder_crypto::identity::Keypair;
    use farder_mls::credential::generate_key_package;
    use farder_mls::group::decode_key_package;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tls_codec::Serialize as TlsSerialize;

    const SERVER_ID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let n = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "farder-e2ee-client-drift-{name}-{}-{n}",
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

    /// A channel created + bootstrapped (the author at epoch 1), plus `n` extra
    /// leaves added directly at the MLS level so `discharge_drift` has real
    /// leaves to remove. The author's chain carries the bootstrap commit, so a
    /// discharge has a folded head to attest.
    struct GroupWithOthers {
        transport: FakeTransport,
        identity: Keypair,
        device: Keypair,
        chain: ChainState,
        key: ChannelKey,
        store: FarderMlsStore,
        store_instance_hash: [u8; 32],
        group: MlsChannelGroup,
        bootstrap: crate::channel::CommitSubmitted,
        /// `(identity, device, declared)` for each added leaf, in add order.
        others: Vec<(Keypair, Keypair, DeclaredMember)>,
    }

    async fn group_with_others(dir_name: &str, channel_id: u64, n: usize) -> GroupWithOthers {
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

        let mut others = Vec::with_capacity(n);
        for i in 0..n {
            let other_id = Keypair::generate();
            let other_dev = Keypair::generate();

            let other_store_path = {
                let mut p = k.mls_store_path(&dir).unwrap();
                p.set_file_name(format!("{channel_id}.other{i}.sqlite"));
                p
            };
            std::fs::create_dir_all(other_store_path.parent().unwrap()).unwrap();
            let (other_store, _) = FarderMlsStore::create(&other_store_path).unwrap();
            let bundle = generate_key_package(&other_store, &other_dev, &other_id.public_key())
                .unwrap();
            let kp_bytes = bundle.key_package().tls_serialize_detached().unwrap();
            let kp = decode_key_package(&created.store, &kp_bytes).unwrap();
            created
                .group
                .add_members(&created.store, &DeviceSigner(&device), &[kp])
                .unwrap();

            let declared = DeclaredMember {
                identity: other_id.public_key(),
                device: device_id(&other_dev.public_key()),
            };
            others.push((other_id, other_dev, declared));
        }

        GroupWithOthers {
            transport,
            identity,
            device,
            chain,
            key: k,
            store: created.store,
            store_instance_hash: created.store_instance_hash,
            group: created.group,
            bootstrap,
            others,
        }
    }

    /// Build the discharge context from the three disjoint fields, so the
    /// borrow checker sees the context borrows `key` / `store` /
    /// `store_instance_hash` only — leaving `chain` / `group` free to be
    /// borrowed mutably in the same call.
    fn ctx<'a>(
        key: &'a ChannelKey,
        store: &'a FarderMlsStore,
        store_instance_hash: &'a [u8; 32],
    ) -> DriftDischargeContext<'a> {
        DriftDischargeContext {
            key,
            generation: 0,
            store,
            store_instance_hash,
        }
    }

    // ---- discharge_drift: the primitive ----

    #[tokio::test]
    async fn discharge_drift_removes_the_dead_leaf_and_advances_the_epoch() {
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 41;
        let mut g = group_with_others("ok", channel_id, 1).await;
        let a = actor(&g.identity, &g.device);
        let dead = g.others[0].2.clone();
        let c = ctx(&g.key, &g.store, &g.store_instance_hash);
        let pre_epoch = g.group.epoch(); // 2: bootstrap (1) + one unlogged add

        let outcome = discharge_drift(
            &g.transport,
            &a,
            &mut g.chain,
            &c,
            &mut g.group,
            std::slice::from_ref(&dead),
        )
        .await
        .unwrap();

        // Local group advanced (2 -> 3) and dropped the dead leaf.
        assert_eq!(pre_epoch, 2);
        assert_eq!(outcome.local_epoch, 3);
        assert_eq!(g.group.epoch(), 3);
        let remaining: Vec<_> = g.group.members().unwrap();
        assert_eq!(remaining.len(), 1, "the dead leaf is gone, the author remains");
        assert_eq!(remaining[0].identity, g.identity.public_key());
        assert_ne!(remaining[0].device, dead.device);

        // The submitted MlsCommit carries removes = the dead set, empty adds,
        // and real chaining values.
        let last = g.transport.submitted().into_iter().last().expect("one discharge commit");
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
                assert_eq!(*epoch, 2, "authored in the pre-merge epoch");
                assert!(adds.is_empty(), "drift discharge adds nothing");
                assert_eq!(removes.len(), 1, "removes = the dead set");
                assert_eq!(removes[0].identity, dead.identity);
                assert_eq!(removes[0].device, dead.device);
                assert_eq!(post_epoch_authenticator, &outcome.post_epoch_authenticator);
                assert_eq!(post_tree_hash, &outcome.post_tree_hash);
                assert_ne!(prev_epoch_authenticator, post_epoch_authenticator);
                assert_eq!(store_instance_hash, &g.store_instance_hash);
                assert_eq!(authz_head, &g.bootstrap.event_hash);
            }
            other => panic!("expected MlsCommit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn discharging_a_leaf_that_is_not_present_is_a_typed_error_with_no_submit() {
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 43;
        let mut g = group_with_others("absent", channel_id, 1).await;
        let a = actor(&g.identity, &g.device);
        let c = ctx(&g.key, &g.store, &g.store_instance_hash);
        let submits_before = g.transport.submit_count();

        // A leaf that was never added to the group.
        let ghost = DeclaredMember {
            identity: Keypair::generate().public_key(),
            device: device_id(&Keypair::generate().public_key()),
        };

        let err = discharge_drift(&g.transport, &a, &mut g.chain, &c, &mut g.group, &[ghost])
            .await
            .unwrap_err();

        assert!(matches!(err, E2eeError::Mls(_)), "typed error, not Ok, got {err}");
        // No silent no-op and no spin: `remove_members` failed before submit.
        assert_eq!(g.transport.submit_count(), submits_before);
    }

    #[tokio::test]
    async fn an_empty_dead_leaf_set_is_refused_before_any_submit() {
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 45;
        let mut g = group_with_others("empty", channel_id, 1).await;
        let a = actor(&g.identity, &g.device);
        let c = ctx(&g.key, &g.store, &g.store_instance_hash);
        let submits_before = g.transport.submit_count();

        let err = discharge_drift(&g.transport, &a, &mut g.chain, &c, &mut g.group, &[])
            .await
            .unwrap_err();

        assert!(matches!(err, E2eeError::Chain(_)), "refused up front, got {err}");
        assert_eq!(g.transport.submit_count(), submits_before);
    }

    #[tokio::test]
    async fn a_stale_epoch_discharge_surfaces_stale_epoch_diverged_and_submits_exactly_once() {
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 47;
        let mut g = group_with_others("stale", channel_id, 1).await;
        let a = actor(&g.identity, &g.device);
        let c = ctx(&g.key, &g.store, &g.store_instance_hash);
        let dead = g.others[0].2.clone();

        g.transport.reject_next("stale-epoch");
        let submits_before = g.transport.submit_count();

        let err = discharge_drift(
            &g.transport,
            &a,
            &mut g.chain,
            &c,
            &mut g.group,
            std::slice::from_ref(&dead),
        )
        .await
        .unwrap_err();

        assert!(err.is_stale_epoch_diverged(), "expected divergence, got {err}");
        match err {
            E2eeError::StaleEpochDiverged { local_epoch } => {
                // The local group already advanced to epoch 3 (ahead of the
                // server).
                assert_eq!(local_epoch, 3);
            }
            other => panic!("expected StaleEpochDiverged, got {other:?}"),
        }
        // Exactly ONE submit — no loop, no spin.
        assert_eq!(g.transport.submit_count(), submits_before + 1);
    }

    #[tokio::test]
    async fn a_second_discharge_on_a_now_stale_group_surfaces_stale_epoch_diverged_not_a_hang() {
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 49;
        let mut g = group_with_others("race", channel_id, 2).await;
        let a = actor(&g.identity, &g.device);
        let c = ctx(&g.key, &g.store, &g.store_instance_hash);
        let first = g.others[0].2.clone();
        let second = g.others[1].2.clone();
        let submits_before = g.transport.submit_count();

        // First discharge loses the epoch race: another member discharged first.
        g.transport.reject_next("stale-epoch");
        let err1 = discharge_drift(
            &g.transport,
            &a,
            &mut g.chain,
            &c,
            &mut g.group,
            std::slice::from_ref(&first),
        )
        .await
        .unwrap_err();
        assert!(err1.is_stale_epoch_diverged(), "first discharge diverges, got {err1}");

        // The group is now stale (one epoch ahead of the server). A SECOND
        // discharge on that stale group must surface the same divergence, not
        // hang or loop.
        g.transport.reject_next("stale-epoch");
        let err2 = discharge_drift(
            &g.transport,
            &a,
            &mut g.chain,
            &c,
            &mut g.group,
            std::slice::from_ref(&second),
        )
        .await
        .unwrap_err();
        assert!(err2.is_stale_epoch_diverged(), "second discharge also diverges, got {err2}");

        // Two attempts, two submits, no more.
        assert_eq!(g.transport.submit_count(), submits_before + 2);
    }

    #[tokio::test]
    async fn a_commit_rate_rejection_on_discharge_surfaces_rekey_rate_limited_and_does_not_loop() {
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 51;
        let mut g = group_with_others("rate", channel_id, 1).await;
        let a = actor(&g.identity, &g.device);
        let c = ctx(&g.key, &g.store, &g.store_instance_hash);
        let dead = g.others[0].2.clone();

        // A genuine drift discharge is commit-rate-exempt, so this rejection is
        // the "your removes did not discharge anything" signal.
        g.transport.reject_next(
            "event rejected: commit-rate rule: a non-drift-discharging commit \
             must be its author's first or at least 2 epochs past their previous one",
        );
        let submits_before = g.transport.submit_count();

        let err = discharge_drift(&g.transport, &a, &mut g.chain, &c, &mut g.group, &[dead])
            .await
            .unwrap_err();

        assert!(err.is_rekey_rate_limited(), "expected rate-limit, got {err}");
        match err {
            E2eeError::RekeyRateLimited { reason } => {
                assert!(reason.contains("commit-rate rule:"), "reason preserved: {reason}");
            }
            other => panic!("expected RekeyRateLimited, got {other:?}"),
        }
        // Exactly ONE submit — no loop.
        assert_eq!(g.transport.submit_count(), submits_before + 1);
    }

    #[tokio::test]
    async fn discharge_drift_requires_a_prior_event_to_attest_its_folded_head() {
        let transport = FakeTransport::new();
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let a = actor(&identity, &device);
        let mut chain = ChainState::default(); // empty: no prior event
        let k = key(E2EE_CHANNEL_ID_FLOOR + 53);
        let dir = temp_dir("no-prev");

        // Build a store + one-member group directly (bypassing create/bootstrap
        // so the chain stays empty).
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
        let c = DriftDischargeContext {
            key: &k,
            generation: 0,
            store: &store,
            store_instance_hash: &store.store_instance_hash(),
        };
        let dead = DeclaredMember {
            identity: Keypair::generate().public_key(),
            device: device_id(&Keypair::generate().public_key()),
        };

        let err = discharge_drift(&transport, &a, &mut chain, &c, &mut group, &[dead])
            .await
            .unwrap_err();

        assert!(matches!(err, E2eeError::Chain(_)));
        assert_eq!(transport.submit_count(), 0);
    }

    // ---- dead_leaves_from_revocation: the helper ----

    #[test]
    fn dead_leaves_from_revocation_maps_the_revoked_device_to_its_leaves() {
        let a_id = Keypair::generate();
        let a_dev = Keypair::generate();
        let b_id = Keypair::generate();
        let b_dev = Keypair::generate();

        let members = vec![
            DeclaredMember {
                identity: a_id.public_key(),
                device: device_id(&a_dev.public_key()),
            },
            DeclaredMember {
                identity: b_id.public_key(),
                device: device_id(&b_dev.public_key()),
            },
        ];

        // Revoking b's device yields exactly b's leaf — the identity half comes
        // from the member list, not from the `DeviceRevoked { device }` payload.
        let dead = dead_leaves_from_revocation(&device_id(&b_dev.public_key()), &members);
        assert_eq!(dead, vec![members[1].clone()]);

        // A device that is not in the group yields nothing to discharge.
        let stranger = device_id(&Keypair::generate().public_key());
        assert!(dead_leaves_from_revocation(&stranger, &members).is_empty());
    }

    // ---- the reactive predicate ----

    #[test]
    fn e2ee_error_sealed_pending_removals_predicate_keys_on_the_transport_reason() {
        let err = E2eeError::Transport(crate::transport::TransportError::rejected(
            "event rejected: channel is sealed until a rekey discharges its pending removals",
        ));
        assert!(err.is_sealed_pending_removals());
        assert!(!err.is_freshness_ceiling_reached());
        assert!(!err.is_rekey_rate_limited());
        assert!(!err.is_stale_epoch_diverged());
    }
}
