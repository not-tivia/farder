//! C7 of the 5a lifecycle: store re-provisioning and diverged-group recovery.
//!
//! This is the recovery primitive that makes the terminal
//! [`E2eeError::StoreResumeTerminal`] and the diverged-group error class
//! (finding F1) non-fatal.
//!
//! # The two recoveries (finding F1)
//!
//! 1. **Store re-provisioning.** When [`crate::join::resume_store`] fails
//!    terminally, the device cannot use its old store — `FarderMlsStore::create`
//!    refuses an existing path and `resume` never recreates
//!    (`farder-mls/src/store.rs:79-90, 144-205`). The only forward path is to
//!    self-`DeviceRevoked` the old device, then mint a FRESH device key + store
//!    + cert + KeyPackage and self-add that fresh device.
//! 2. **Diverged-group recovery.** Every own-commit primitive
//!    (`self_update` / `add_members` / `remove_members`) merges **locally
//!    before the submit is accepted**, so a rejected own-commit
//!    (`StaleEpochDiverged` / `RekeyRateLimited`) leaves the local group one
//!    epoch ahead of the server with no rollback. The recovery has the SAME
//!    shape as (1): the old leaf is dead/drift, so the channel is rejoined by a
//!    NEW leaf for the same identity — never by resurrecting the old one.
//!
//! # The honest limitation (MLS-state-loss-without-device-loss)
//!
//! MLS ratchet state lives in the store, so there is no way to "rebuild the
//! group from the log" client-side (finding F1). Recovery is therefore a NEW
//! leaf for the same identity, NOT a resurrection of the old leaf. That new
//! leaf must be authored into the group by an **existing confirmed device of
//! the same identity** (the self-add rule, `event_log_state.rs:1136-1145`) —
//! see [`crate::device::add_own_device`]. A single-device identity whose only
//! store is lost therefore has no healthy device to author the self-add; that
//! case needs an owner `MlsGroupReset` (C5) or another device, and is out of
//! C7's scope — H1 wires the harness around the multi-device (or
//! still-keyed-old-device) case.
//!
//! # The seam: the caller supplies the fresh device key + store
//!
//! [`reprovision_device`] does **not** generate the fresh device [`Keypair`] or
//! mint the fresh store itself. The caller supplies both (via
//! [`ReprovisionContext::fresh`], an [`OwnDeviceContext`]). This is the cleaner
//! seam because:
//!
//! - the caller owns persistence: it must write the fresh device key to
//!   `device_state.json` and remember the fresh store's path + instance hash,
//!   and only the caller knows where those files go (this crate owns no
//!   storage, matching [`crate::chain`]'s contract);
//! - the fresh store MUST live at a **new path** — the old path is poison
//!   (`FarderMlsStore::create` refuses to create over it, and deleting it
//!   in-place would destroy the evidence and risk reusing a cloned store). Only
//!   the caller can choose the new path (a fresh per-device `data_dir` or a
//!   suffixed filename);
//! - it keeps [`reprovision_device`] a pure orchestration over
//!   [`crate::revoke::revoke_device`] + [`crate::device::add_own_device`], with
//!   no filesystem coupling of its own.
//!
//! A generator would hide all three of those decisions behind a single call and
//! would still have to hand the caller back the generated key/path so it could
//! persist them — strictly more surface for no gain.

use farder_crypto::event_log::device_id;
use farder_crypto::identity::Keypair;
use farder_mls::group::MlsChannelGroup;

use crate::chain::{Actor, ChainState};
use crate::channel::E2eeError;
use crate::device::{add_own_device, AddOwnDeviceOutcome, OwnDeviceContext};
use crate::revoke::revoke_device;
use crate::transport::E2eeTransport;

/// The fixed inputs for [`reprovision_device`]: the OLD device (which signs its
/// own `DeviceRevoked`) plus the fresh device's orchestration inputs (the exact
/// [`OwnDeviceContext`] that [`crate::device::add_own_device`] consumes).
///
/// The fresh device's store MUST already be minted at a NEW path by the caller
/// (see the module doc): the old store path is poison and is never reused or
/// deleted in place.
pub struct ReprovisionContext<'a> {
    /// The OLD device's signing key. It signs the self-`DeviceRevoked`
    /// (`revoke.rs`'s self-revoke shape — the victim IS the signer, so the
    /// victim id is `device_id(&old_device.public_key())`).
    pub old_device: &'a Keypair,
    /// The fresh device's orchestration inputs: `identity` (the same identity),
    /// the fresh device key, the fresh store + its instance hash, and the
    /// healthy steward's [`crate::commit::StewardContext`].
    pub fresh: OwnDeviceContext<'a>,
}

/// The transient, mutated inputs for one recovery: the three chains and the
/// healthy steward's commit surface. Bundled so [`reprovision_device`] and
/// [`recover_diverged_group`] stay under the clippy argument-count bound (the
/// same convention as [`OwnDeviceContext`] / [`crate::commit::StewardContext`]).
pub struct ReprovisionLive<'a> {
    /// The OLD device's per-(server, device) chain — advanced past the
    /// accepted `DeviceRevoked`.
    pub old_chain: &'a mut ChainState,
    /// The FRESH device's chain — advanced past its `DeviceAuthorized` +
    /// `MlsKeyPackagePublished`.
    pub new_chain: &'a mut ChainState,
    /// The healthy, already-confirmed steward device that authors the self-add
    /// (its `identity` must equal [`OwnDeviceContext::identity`]).
    pub steward: Actor<'a>,
    /// The steward device's chain — advanced past the add-commit + Welcome.
    pub steward_chain: &'a mut ChainState,
    /// The steward's already-loaded group (this crate's convention: load, then
    /// pass `&mut` in).
    pub group: &'a mut MlsChannelGroup,
}

/// The result of a successful [`reprovision_device`]: the revoked event's hash
/// plus the full [`AddOwnDeviceOutcome`] from the fresh device's self-add.
#[derive(Debug)]
pub struct ReprovisionOutcome {
    /// Server-assigned hash of the accepted `DeviceRevoked` (old device).
    pub device_revoked_hash: String,
    /// The fresh device's authorize + publish + add outcome (see
    /// [`AddOwnDeviceOutcome`]).
    pub add: AddOwnDeviceOutcome,
}

/// Re-provision this identity as a FRESH device after a terminal store loss or
/// a diverged group: self-`DeviceRevoked` the old device, then self-add a fresh
/// device (authorize + publish a KeyPackage + add) via C6's
/// [`crate::device::add_own_device`].
///
/// The sequence, in order:
///
/// 1. [`crate::revoke::revoke_device`] — the old device signs its own
///    `DeviceRevoked` (the victim id is `device_id(&old_device.public_key())`),
///    so the fold's "owning identity OR owner" arm authorizes it
///    (`event_log_state.rs:996-1010`).
/// 2. [`crate::device::add_own_device`] — the fresh device authorizes itself
///    (`DeviceAuthorized` with an identity-signed cert), publishes its
///    KeyPackage from its fresh store, and the healthy steward self-adds its
///    leaf.
///
/// The fresh device key + store are caller-supplied (see the module doc), so
/// the caller can persist the new key and store path to disk. The old store
/// path is never read, reused, or deleted here.
///
/// # Requirements on the caller
///
/// - `ctx.fresh.identity` is the SAME identity as `ctx.old_device`'s owner
///   (and as `live.steward.identity`) — `add_own_device` guards the
///   steward-identity half up front.
/// - `ctx.fresh.steward` names a healthy, already-confirmed device of that
///   identity whose group is in sync with the server; its `group` is passed as
///   `live.group`.
pub async fn reprovision_device<T: E2eeTransport + Sync>(
    transport: &T,
    ctx: &ReprovisionContext<'_>,
    live: ReprovisionLive<'_>,
) -> Result<ReprovisionOutcome, E2eeError> {
    let ReprovisionLive {
        old_chain,
        new_chain,
        steward,
        steward_chain,
        group,
    } = live;

    // 1. Self-revoke the OLD device (the device signs its own revocation; the
    //    victim id is the old device's id).
    let revoke_actor = Actor {
        device: ctx.old_device,
        identity: ctx.fresh.identity,
        log_server_id: ctx.fresh.steward.key.log_server_id.as_str(),
    };
    let old_device_id = device_id(&ctx.old_device.public_key());
    let revoked = revoke_device(transport, &revoke_actor, old_chain, old_device_id).await?;

    // 2. Self-add the FRESH device (authorize + publish + add), reusing C6.
    let added = add_own_device(transport, &ctx.fresh, new_chain, &steward, steward_chain, group)
        .await?;

    Ok(ReprovisionOutcome {
        device_revoked_hash: revoked.event_hash,
        add: added,
    })
}

/// A thin convenience over [`reprovision_device`] for the diverged-group class:
/// if `trigger` is a divergence signal (see [`E2eeError::is_diverged`]), run the
/// recovery; otherwise return the trigger unchanged.
///
/// The divergence variants (`StaleEpochDiverged`, `RekeyRateLimited`,
/// `ResyncEquivocation`, `ResyncPoisoned`) all leave the local group one epoch
/// ahead of the server with no rollback, so a caller that surfaces one of them
/// hands it here rather than keeping the group. `trigger` is taken by value so a
/// non-diverged error is returned exactly as-is (not reconstructed).
pub async fn recover_diverged_group<T: E2eeTransport + Sync>(
    transport: &T,
    ctx: &ReprovisionContext<'_>,
    trigger: E2eeError,
    live: ReprovisionLive<'_>,
) -> Result<ReprovisionOutcome, E2eeError> {
    if !trigger.is_diverged() {
        return Err(trigger);
    }
    reprovision_device(transport, ctx, live).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::event_log::{device_id, EventPayload, E2EE_CHANNEL_ID_FLOOR};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::channel::{bootstrap_group, create_e2ee_channel, ChannelSpec};
    use crate::commit::StewardContext;
    use crate::join::{create_joiner_store, resume_store};
    use crate::testing::FakeTransport;

    const SERVER_ID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let n = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "farder-e2ee-client-reprovision-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn key(channel_id: u64) -> crate::channel_key::ChannelKey {
        crate::channel_key::ChannelKey::new(SERVER_ID.to_string(), channel_id).unwrap()
    }

    fn actor<'a>(identity: &'a Keypair, device: &'a Keypair) -> Actor<'a> {
        Actor {
            device,
            identity,
            log_server_id: SERVER_ID,
        }
    }

    /// A channel created + bootstrapped by one identity/device — the healthy
    /// steward that will author the fresh device's self-add. `group` is at
    /// epoch 1 with a confirmed leaf.
    struct Stewarded {
        transport: FakeTransport,
        identity: Keypair,
        device: Keypair,
        key: crate::channel_key::ChannelKey,
        chain: ChainState,
        store: farder_mls::store::FarderMlsStore,
        store_instance_hash: [u8; 32],
        group: MlsChannelGroup,
    }

    async fn stewarded(channel_id: u64) -> Stewarded {
        let transport = FakeTransport::new();
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let a = actor(&identity, &device);
        let mut chain = ChainState::default();
        let dir = temp_dir("steward");
        let k = key(channel_id);
        let spec = ChannelSpec {
            key: k.clone(),
            name: "vault".to_string(),
            kind: "text".to_string(),
            parent: None,
        };

        let created = create_e2ee_channel(&transport, &a, &mut chain, &spec, &dir)
            .await
            .unwrap();
        let mut group = created.group;
        let store = created.store;
        let store_instance_hash = created.store_instance_hash;
        bootstrap_group(
            &transport,
            &a,
            &mut chain,
            &k,
            &mut group,
            &store,
            &store_instance_hash,
        )
        .await
        .unwrap();

        Stewarded {
            transport,
            identity,
            device,
            key: k,
            chain,
            store,
            store_instance_hash,
            group,
        }
    }

    /// A "lost" device whose MLS store file is gone but whose instance-hash
    /// file survives — the exact shape that makes `resume_store` terminal
    /// (`StoreResumeTerminal::Io`) rather than failing earlier.
    fn lose_device_store(dir: &std::path::Path, k: &crate::channel_key::ChannelKey) -> Keypair {
        let lost = Keypair::generate();
        let (store, _hash) = create_joiner_store(dir, k).unwrap();
        drop(store);
        let store_path = k.mls_store_path(dir).unwrap();
        std::fs::remove_file(&store_path).unwrap();
        lost
    }

    #[test]
    fn is_diverged_covers_the_divergence_variants_and_nothing_else() {
        assert!(E2eeError::StaleEpochDiverged { local_epoch: 2 }.is_diverged());
        assert!(E2eeError::RekeyRateLimited {
            reason: "commit-rate rule: nope".to_string()
        }
        .is_diverged());
        assert!(E2eeError::ResyncEquivocation {
            attempts: 3,
            last_epoch: 2,
        }
        .is_diverged());
        assert!(E2eeError::ResyncPoisoned {
            member: farder_mls::group::DeclaredMember {
                identity: Keypair::generate().public_key(),
                device: device_id(&Keypair::generate().public_key()),
            },
            reason: "impostor".to_string(),
        }
        .is_diverged());

        // Not divergence: transport/chain/MLS/cap/resume-terminal are all
        // different failure classes with their own recovery (or none).
        assert!(!E2eeError::NotConfirmed.is_diverged());
        assert!(!E2eeError::Chain("nope".to_string()).is_diverged());
        assert!(!E2eeError::DeviceCapReached {
            reason: "cap".to_string()
        }
        .is_diverged());
        assert!(!E2eeError::Transport(crate::transport::TransportError::rejected(
            "stale-epoch"
        ))
        .is_diverged());
    }

    #[tokio::test]
    async fn reprovision_device_recovers_a_terminal_store_resume_error() {
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 201;
        let mut s = stewarded(channel_id).await;

        // A "lost" device: its store file is gone, so resume is terminal.
        let lost_dir = temp_dir("lost");
        let lost_device = lose_device_store(&lost_dir, &s.key);
        match resume_store(&lost_dir, &s.key) {
            Ok(_) => panic!("resume of a missing store must be terminal"),
            Err(E2eeError::StoreResumeTerminal(_)) => {}
            Err(other) => panic!("expected StoreResumeTerminal, got {other}"),
        }

        // The FRESH device: a new key + a fresh store at a NEW path.
        let fresh_device = Keypair::generate();
        let fresh_dir = temp_dir("fresh");
        let (fresh_store, fresh_hash) = create_joiner_store(&fresh_dir, &s.key).unwrap();

        let steward_ctx = StewardContext {
            key: &s.key,
            generation: 0,
            store: &s.store,
            store_instance_hash: &s.store_instance_hash,
        };
        let fresh_ctx = OwnDeviceContext {
            identity: &s.identity,
            new_device: &fresh_device,
            new_store: &fresh_store,
            new_store_instance_hash: &fresh_hash,
            steward: &steward_ctx,
        };
        let ctx = ReprovisionContext {
            old_device: &lost_device,
            fresh: fresh_ctx,
        };

        let mut old_chain = ChainState::default();
        let mut new_chain = ChainState::default();
        let steward_actor = actor(&s.identity, &s.device);

        let before = s.transport.submit_count();
        let outcome = reprovision_device(
            &s.transport,
            &ctx,
            ReprovisionLive {
                old_chain: &mut old_chain,
                new_chain: &mut new_chain,
                steward: steward_actor,
                steward_chain: &mut s.chain,
                group: &mut s.group,
            },
        )
        .await
        .unwrap();

        // Five events: revoke(old) + authorize(fresh) + keypackage(fresh) +
        // commit(add fresh) + welcome(fresh).
        let submitted = s.transport.submitted();
        assert_eq!(submitted.len(), before + 5);
        let revoke = &submitted[before];
        let authorize = &submitted[before + 1];
        let published = &submitted[before + 2];
        let commit = &submitted[before + 3];
        let welcome = &submitted[before + 4];

        // DeviceRevoked names the OLD device, authored by the old device itself.
        assert_eq!(revoke.core.author, s.identity.public_key());
        assert_eq!(revoke.core.device, device_id(&lost_device.public_key()));
        match &revoke.core.payload {
            EventPayload::DeviceRevoked { device } => {
                assert_eq!(device, &device_id(&lost_device.public_key()));
            }
            other => panic!("expected DeviceRevoked first, got {other:?}"),
        }
        assert_eq!(outcome.device_revoked_hash, revoke.hash());

        // DeviceAuthorized binds the FRESH device to the identity, signed by it.
        assert!(authorize.verify(&fresh_device.public_key()).is_ok());
        match &authorize.core.payload {
            EventPayload::DeviceAuthorized { cert } => {
                assert_eq!(cert.core.identity, s.identity.public_key());
                assert_eq!(cert.core.device_id, device_id(&fresh_device.public_key()));
            }
            other => panic!("expected DeviceAuthorized second, got {other:?}"),
        }

        // MlsKeyPackagePublished is the FRESH device's own package.
        assert_eq!(published.core.device, device_id(&fresh_device.public_key()));
        assert!(matches!(
            &published.core.payload,
            EventPayload::MlsKeyPackagePublished { .. }
        ));

        // MlsCommit adds the FRESH device (self-add), citing the package above.
        match &commit.core.payload {
            EventPayload::MlsCommit { adds, removes, .. } => {
                assert!(removes.is_empty());
                assert_eq!(adds.len(), 1);
                assert_eq!(adds[0].identity, s.identity.public_key());
                assert_eq!(adds[0].device, device_id(&fresh_device.public_key()));
                assert_eq!(adds[0].key_package, published.hash());
            }
            other => panic!("expected MlsCommit fourth, got {other:?}"),
        }

        // MlsWelcome is addressed to the FRESH device, citing the commit.
        match &welcome.core.payload {
            EventPayload::MlsWelcome {
                for_member,
                for_device,
                commit: cited,
                ..
            } => {
                assert_eq!(for_member, &s.identity.public_key());
                assert_eq!(for_device, &device_id(&fresh_device.public_key()));
                assert_eq!(cited, &commit.hash());
            }
            other => panic!("expected MlsWelcome fifth, got {other:?}"),
        }

        // The fresh device is now a leaf alongside the steward.
        let members = s.group.members().unwrap();
        assert!(members.iter().any(|m| {
            m.identity == s.identity.public_key()
                && m.device == device_id(&fresh_device.public_key())
        }));

        // Chains advanced: old past the revoke, fresh past authorize+publish,
        // steward past create+bootstrap+commit+welcome.
        assert_eq!(old_chain.next_seq, 1);
        assert_eq!(new_chain.next_seq, 2);
        assert_eq!(s.chain.next_seq, 4);
    }

    #[tokio::test]
    async fn reprovision_device_emits_revoke_then_authorize_then_keypackage_then_add_in_order() {
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 203;
        let mut s = stewarded(channel_id).await;

        let old_device = Keypair::generate();
        let fresh_device = Keypair::generate();
        let fresh_dir = temp_dir("fresh-order");
        let (fresh_store, fresh_hash) = create_joiner_store(&fresh_dir, &s.key).unwrap();

        let steward_ctx = StewardContext {
            key: &s.key,
            generation: 0,
            store: &s.store,
            store_instance_hash: &s.store_instance_hash,
        };
        let ctx = ReprovisionContext {
            old_device: &old_device,
            fresh: OwnDeviceContext {
                identity: &s.identity,
                new_device: &fresh_device,
                new_store: &fresh_store,
                new_store_instance_hash: &fresh_hash,
                steward: &steward_ctx,
            },
        };

        let mut old_chain = ChainState::default();
        let mut new_chain = ChainState::default();
        let before = s.transport.submit_count();

        let outcome = reprovision_device(
            &s.transport,
            &ctx,
            ReprovisionLive {
                old_chain: &mut old_chain,
                new_chain: &mut new_chain,
                steward: actor(&s.identity, &s.device),
                steward_chain: &mut s.chain,
                group: &mut s.group,
            },
        )
        .await
        .unwrap();

        // The five submitted payloads, in order.
        let submitted = s.transport.submitted();
        let payloads: Vec<&EventPayload> = submitted
            .iter()
            .skip(before)
            .map(|e| &e.core.payload)
            .collect();
        assert_eq!(payloads.len(), 5);
        assert!(matches!(payloads[0], EventPayload::DeviceRevoked { .. }));
        assert!(matches!(payloads[1], EventPayload::DeviceAuthorized { .. }));
        assert!(matches!(payloads[2], EventPayload::MlsKeyPackagePublished { .. }));
        assert!(matches!(payloads[3], EventPayload::MlsCommit { .. }));
        assert!(matches!(payloads[4], EventPayload::MlsWelcome { .. }));

        // The outcome hashes line up with the submitted events.
        let submitted = s.transport.submitted();
        assert_eq!(outcome.device_revoked_hash, submitted[before].hash());
        assert_eq!(outcome.add.device_authorized_hash, submitted[before + 1].hash());
        assert_eq!(outcome.add.key_package_hash, submitted[before + 2].hash());
        assert_eq!(outcome.add.commit_event_hash, submitted[before + 3].hash());
        assert_eq!(outcome.add.welcome_event_hash, submitted[before + 4].hash());
    }

    #[tokio::test]
    async fn recover_diverged_group_maps_a_divergence_to_the_recovery_path() {
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 205;
        let mut s = stewarded(channel_id).await;

        let old_device = Keypair::generate();
        let fresh_device = Keypair::generate();
        let fresh_dir = temp_dir("fresh-diverged");
        let (fresh_store, fresh_hash) = create_joiner_store(&fresh_dir, &s.key).unwrap();

        let steward_ctx = StewardContext {
            key: &s.key,
            generation: 0,
            store: &s.store,
            store_instance_hash: &s.store_instance_hash,
        };
        let ctx = ReprovisionContext {
            old_device: &old_device,
            fresh: OwnDeviceContext {
                identity: &s.identity,
                new_device: &fresh_device,
                new_store: &fresh_store,
                new_store_instance_hash: &fresh_hash,
                steward: &steward_ctx,
            },
        };

        let mut old_chain = ChainState::default();
        let mut new_chain = ChainState::default();
        let before = s.transport.submit_count();

        // A divergence trigger maps to the recovery path.
        let trigger = E2eeError::StaleEpochDiverged { local_epoch: 7 };
        let outcome = recover_diverged_group(
            &s.transport,
            &ctx,
            trigger,
            ReprovisionLive {
                old_chain: &mut old_chain,
                new_chain: &mut new_chain,
                steward: actor(&s.identity, &s.device),
                steward_chain: &mut s.chain,
                group: &mut s.group,
            },
        )
        .await
        .unwrap();

        assert_eq!(s.transport.submit_count(), before + 5, "full recovery ran");
        assert_eq!(outcome.add.local_epoch, 2);

        // A NON-diverged error is returned unchanged and submits nothing.
        let mut old_chain2 = ChainState::default();
        let mut new_chain2 = ChainState::default();
        let not_diverged = E2eeError::NotConfirmed;
        let before2 = s.transport.submit_count();
        let err = recover_diverged_group(
            &s.transport,
            &ctx,
            not_diverged,
            ReprovisionLive {
                old_chain: &mut old_chain2,
                new_chain: &mut new_chain2,
                steward: actor(&s.identity, &s.device),
                steward_chain: &mut s.chain,
                group: &mut s.group,
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, E2eeError::NotConfirmed));
        assert_eq!(s.transport.submit_count(), before2, "nothing submitted");
    }

    #[test]
    fn reprovision_uses_a_distinct_fresh_store_path_and_never_recreates_in_place() {
        // The old store's path (per-device data_dir) and the fresh store's path
        // must be genuinely distinct, and the old path stays refused by create.
        let k = key(E2EE_CHANNEL_ID_FLOOR + 207);
        let old_dir = temp_dir("old-path");
        let new_dir = temp_dir("new-path");

        let (old_store, old_hash) = create_joiner_store(&old_dir, &k).unwrap();
        drop(old_store);

        let (new_store, new_hash) = create_joiner_store(&new_dir, &k).unwrap();

        // Distinct on-disk paths.
        assert_ne!(
            k.mls_store_path(&old_dir).unwrap(),
            k.mls_store_path(&new_dir).unwrap()
        );
        // Fresh instance id => fresh publishable hash (the recovery's whole point).
        assert_ne!(old_hash, new_hash);
        assert_eq!(new_store.store_instance_hash(), new_hash);

        // The old path is still poison: create refuses to recreate over it, so
        // re-provisioning can never silently reuse the old store.
        assert!(create_joiner_store(&old_dir, &k).is_err());
    }
}
