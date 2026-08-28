//! C6 of the 5a lifecycle: multi-device self-add.
//!
//! The "I am adding a SECOND device to my own identity" path. The fold's
//! self-add rule (`event_log_state.rs:1136-1145`) means: once an identity holds
//! a confirmed leaf in a channel, only that identity itself may add its further
//! devices (`author == add.identity`). So the add-commit must be authored by an
//! **existing, confirmed device of the same identity**, while the *new* device
//! is the one being added.
//!
//! The sequence (all three steps over one [`E2eeTransport`]):
//!
//! 1. **The new device authorizes itself** — [`authorize_device`] submits
//!    `DeviceAuthorized { cert }` where `cert = DeviceCert::create(identity,
//!    &new_device_pubkey, now)`: the **identity** key signs the cert (binding
//!    the new device to the identity), and the **new device** key signs the
//!    event (`event_log_state.rs:781-802`).
//! 2. **The new device publishes a KeyPackage** — reused
//!    [`crate::channel::publish_key_package`], which stores the KeyPackage's
//!    private material in the new device's own store.
//! 3. **An existing confirmed device self-adds the new device** — reused
//!    [`crate::commit::add_member`], targeting the new device's
//!    `(identity, device_id)`. Because the authoring device's identity IS the
//!    added device's identity, the self-add rule is satisfied.
//!
//! # The device cap (8)
//!
//! The fold enforces the live-device cap at `DeviceAuthorized`
//! (`event_log_state.rs:840-849`): live = non-revoked + cert-unexpired, at most
//! [`MAX_LIVE_DEVICES_PER_IDENTITY`](farder_crypto::event_log_state::MAX_LIVE_DEVICES_PER_IDENTITY)
//! (= 8). This crate holds no fold `LogState`, so it cannot count live devices
//! client-side; instead [`authorize_device`] surfaces the fold's verbatim
//! rejection as [`E2eeError::DeviceCapReached`] (via
//! [`crate::transport::TransportError::is_device_cap_reached`]) rather than a
//! generic transport failure — never silently swallowed.

use farder_crypto::event_log::{device_id, DeviceCert, EventPayload};
use farder_crypto::identity::Keypair;
use farder_mls::group::{DeclaredMember, MlsChannelGroup};
use farder_mls::store::FarderMlsStore;

use crate::chain::{build_next_event, event_now_secs, Actor, ChainState};
use crate::channel::{publish_key_package, E2eeError};
use crate::commit::{add_member, StewardContext};
use crate::transport::E2eeTransport;

/// The result of a successful [`authorize_device`].
///
/// A `DeviceAuthorized` merges no MLS state locally, so — like
/// [`crate::revoke::RevokeOutcome`] — there is no `local_epoch` and no
/// divergence contract: acceptance is simply the server recording the event.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceAuthorizedOutcome {
    /// Server-assigned hash of the accepted `DeviceAuthorized` event.
    pub event_hash: String,
    /// The identity-signed cert that binds this device to its identity.
    pub cert: DeviceCert,
}

/// The fixed inputs for [`add_own_device`]: the identity, the NEW device, the
/// new device's own MLS store (its KeyPackage private material lives there —
/// see the store lifecycle contract in [`crate::join`]), and the existing
/// (confirmed) device's steward commit inputs. Bundled because the orchestration
/// spans two actors and two chains and would otherwise trip the clippy
/// argument-count bound.
pub struct OwnDeviceContext<'a> {
    /// The owning identity signing key — signs the `DeviceCert` and owns BOTH
    /// devices.
    pub identity: &'a Keypair,
    /// The NEW device's signing key (the device being added).
    pub new_device: &'a Keypair,
    /// The new device's MLS store — the one [`crate::channel::publish_key_package`]
    /// generates the KeyPackage FROM, and the one the new device will later
    /// join with.
    pub new_store: &'a FarderMlsStore,
    /// The new device's store instance hash.
    pub new_store_instance_hash: &'a [u8; 32],
    /// The existing (already-confirmed) device's steward commit inputs.
    pub steward: &'a StewardContext<'a>,
}

/// The result of a successful [`add_own_device`]: the three staged event hashes
/// plus the add outcome's MLS facts.
#[derive(Debug)]
pub struct AddOwnDeviceOutcome {
    /// Server-assigned hash of the accepted `DeviceAuthorized` event.
    pub device_authorized_hash: String,
    /// Server-assigned hash of the accepted `MlsKeyPackagePublished` event.
    pub key_package_hash: String,
    /// Server-assigned hash of the accepted `MlsCommit` add event.
    pub commit_event_hash: String,
    /// Server-assigned hash of the accepted `MlsWelcome` event.
    pub welcome_event_hash: String,
    /// The epoch the LOCAL group is in after merging the add (see the
    /// divergence contract on [`E2eeError`]).
    pub local_epoch: u64,
    pub post_epoch_authenticator: [u8; 32],
    pub post_tree_hash: [u8; 32],
}

/// Authorize one device under its identity: submit
/// `DeviceAuthorized { cert }` where the cert is `DeviceCert::create(identity,
/// &device_pubkey, now)` — signed by the **identity** key, binding the device to
/// the identity — and the event is authored (signed) by the **device** key
/// (`event_log_state.rs:781-802`).
///
/// The `Actor` names both: `actor.identity` signs the cert, `actor.device`
/// signs the event. This is the first step of [`add_own_device`], and the exact
/// shape the new device performs on itself.
///
/// # Device cap
///
/// The fold refuses a `DeviceAuthorized` when the identity already holds
/// [`MAX_LIVE_DEVICES_PER_IDENTITY`](farder_crypto::event_log_state::MAX_LIVE_DEVICES_PER_IDENTITY)
/// live devices. That rejection surfaces here as [`E2eeError::DeviceCapReached`]
/// with the fold's reason preserved verbatim — never as a generic transport
/// error and never silently swallowed.
pub async fn authorize_device<T: E2eeTransport + Sync>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
) -> Result<DeviceAuthorizedOutcome, E2eeError> {
    let now = event_now_secs();
    let cert = DeviceCert::create(actor.identity, &actor.device.public_key(), now);

    let event = build_next_event(
        actor.device,
        actor.identity,
        actor.log_server_id,
        chain,
        now,
        EventPayload::DeviceAuthorized { cert: cert.clone() },
    );

    // Submit; the fold authorizes (or rejects — notably the live-device cap)
    // by the cert's identity and the event's device. Advance only on accept.
    let accepted = match transport.submit_event(&event).await {
        Ok(a) => a,
        Err(e) if e.is_device_cap_reached() => {
            return Err(E2eeError::DeviceCapReached {
                reason: e.rejection_reason().to_string(),
            });
        }
        Err(e) => return Err(e.into()),
    };
    chain.advance(&event);

    Ok(DeviceAuthorizedOutcome {
        event_hash: accepted.event_hash,
        cert,
    })
}

/// Add a second device to the caller's own identity, end-to-end:
///
/// 1. the new device authorizes itself ([`authorize_device`]),
/// 2. the new device publishes its KeyPackage ([`crate::channel::publish_key_package`]),
/// 3. the existing confirmed device self-adds the new device
///    ([`crate::commit::add_member`], with `author == add.identity`, satisfying
///    the self-add rule).
///
/// `steward` is the *existing* device (whose `identity` must equal
/// [`OwnDeviceContext::identity`], guarded up front); `steward_chain` is its
/// per-(server, device) chain. `new_chain` is the NEW device's chain, advanced
/// past its `DeviceAuthorized` + `MlsKeyPackagePublished`. `group` is the
/// existing device's already-loaded group (this crate's convention: load, then
/// pass `&mut` in).
///
/// # Divergence contract
///
/// Step 3 is an own-commit: `MlsChannelGroup::add_members` merges locally and
/// immediately, so a `stale-epoch` rejection of the add surfaces
/// [`E2eeError::StaleEpochDiverged`] and the local group is one epoch ahead of
/// the server — the caller must resync, never keep using the group (see
/// [`crate::commit::add_member`]).
pub async fn add_own_device<T: E2eeTransport + Sync>(
    transport: &T,
    ctx: &OwnDeviceContext<'_>,
    new_chain: &mut ChainState,
    steward: &Actor<'_>,
    steward_chain: &mut ChainState,
    group: &mut MlsChannelGroup,
) -> Result<AddOwnDeviceOutcome, E2eeError> {
    // The self-add rule requires the authoring device to BE this identity. A
    // mis-wired caller (a different identity's device authoring the add) must
    // fail here, before any doomed event is submitted.
    if steward.identity.public_key() != ctx.identity.public_key() {
        return Err(E2eeError::chain(
            "add_own_device: the authoring device belongs to a different identity",
        ));
    }

    // 1. The new device authorizes itself (identity-signed cert, device-signed
    //    event).
    let new_actor = Actor {
        device: ctx.new_device,
        identity: ctx.identity,
        log_server_id: ctx.steward.key.log_server_id.as_str(),
    };
    let authorized = authorize_device(transport, &new_actor, new_chain).await?;

    // 2. The new device publishes its KeyPackage from its own store.
    let published = publish_key_package(
        transport,
        &new_actor,
        new_chain,
        ctx.new_store,
        ctx.new_store_instance_hash,
    )
    .await?;

    // 3. The existing confirmed device self-adds the new device. `member` is
    //    the new device's (identity, device_id); the steward's identity equals
    //    `member.identity`, so `author == add.identity` holds.
    let member = DeclaredMember {
        identity: ctx.identity.public_key(),
        device: device_id(&ctx.new_device.public_key()),
    };
    let added = add_member(transport, steward, steward_chain, ctx.steward, group, &member).await?;

    Ok(AddOwnDeviceOutcome {
        device_authorized_hash: authorized.event_hash,
        key_package_hash: published.event_hash,
        commit_event_hash: added.commit_event_hash,
        welcome_event_hash: added.welcome_event_hash,
        local_epoch: added.local_epoch,
        post_epoch_authenticator: added.post_epoch_authenticator,
        post_tree_hash: added.post_tree_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::event_log::{device_id, E2EE_CHANNEL_ID_FLOOR};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::channel::{bootstrap_group, create_e2ee_channel, ChannelSpec};
    use crate::chain::Actor;
    use crate::join::create_joiner_store;
    use crate::testing::FakeTransport;

    const SERVER_ID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let n = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "farder-e2ee-client-device-{name}-{}-{n}",
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

    /// A channel created + bootstrapped by one identity/device, at epoch 1 with
    /// a confirmed leaf — ready to self-add a second device.
    async fn stewarded_channel(
        transport: &FakeTransport,
        channel_id: u64,
    ) -> (
        Keypair,
        Keypair,
        crate::channel_key::ChannelKey,
        MlsChannelGroup,
        FarderMlsStore,
        [u8; 32],
        ChainState,
    ) {
        let dir = temp_dir("steward");
        let k = key(channel_id);
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let a = actor(&identity, &device);
        let mut chain = ChainState::default();
        let spec = ChannelSpec {
            key: k.clone(),
            name: "vault".to_string(),
            kind: "text".to_string(),
            parent: None,
        };
        let created = create_e2ee_channel(transport, &a, &mut chain, &spec, &dir)
            .await
            .unwrap();
        let mut group = created.group;
        let store = created.store;
        let hash = created.store_instance_hash;
        bootstrap_group(transport, &a, &mut chain, &k, &mut group, &store, &hash)
            .await
            .unwrap();
        (identity, device, k, group, store, hash, chain)
    }

    #[test]
    fn the_device_cap_is_eight() {
        assert_eq!(
            farder_crypto::event_log_state::MAX_LIVE_DEVICES_PER_IDENTITY,
            8
        );
    }

    #[tokio::test]
    async fn authorize_device_emits_a_self_authorization_bound_to_identity_and_signed_by_the_device() {
        let transport = FakeTransport::new();
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let a = actor(&identity, &device);
        let mut chain = ChainState::default();

        let outcome = authorize_device(&transport, &a, &mut chain).await.unwrap();

        assert_eq!(transport.submit_count(), 1);
        let last = transport.submitted().into_iter().last().expect("one event");

        // The event is signed by the NEW device key and authored by the identity.
        assert!(last.verify(&device.public_key()).is_ok());
        assert_eq!(last.core.author, identity.public_key());
        assert_eq!(last.core.device, device_id(&device.public_key()));

        // The cert binds this device to the identity.
        match &last.core.payload {
            EventPayload::DeviceAuthorized { cert } => {
                assert_eq!(cert.core.identity, identity.public_key());
                assert_eq!(cert.core.device_id, device_id(&device.public_key()));
                assert_eq!(cert.core.device_pubkey, device.public_key());
                assert!(cert.verify().is_ok());
                assert_eq!(cert, &outcome.cert);
            }
            other => panic!("expected DeviceAuthorized, got {other:?}"),
        }

        // The chain advanced past the accepted event.
        assert_eq!(chain.next_seq, 1);
        assert_eq!(chain.last_event_hash.as_deref(), Some(outcome.event_hash.as_str()));
        assert_eq!(outcome.event_hash, last.hash());
    }

    #[tokio::test]
    async fn add_own_device_submits_authorize_then_keypackage_then_add_targeting_the_new_device() {
        let transport = FakeTransport::new();
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 101;
        let (identity, device, k, mut group, store, hash, mut steward_chain) =
            stewarded_channel(&transport, channel_id).await;

        // The SECOND device of the same identity, with its own store.
        let new_device = Keypair::generate();
        let new_dir = temp_dir("new-device");
        let (new_store, new_hash) = create_joiner_store(&new_dir, &k).unwrap();

        let steward_ctx = StewardContext {
            key: &k,
            generation: 0,
            store: &store,
            store_instance_hash: &hash,
        };
        let ctx = OwnDeviceContext {
            identity: &identity,
            new_device: &new_device,
            new_store: &new_store,
            new_store_instance_hash: &new_hash,
            steward: &steward_ctx,
        };
        let steward_actor = actor(&identity, &device);
        let mut new_chain = ChainState::default();

        let before = transport.submit_count();
        let outcome = add_own_device(
            &transport,
            &ctx,
            &mut new_chain,
            &steward_actor,
            &mut steward_chain,
            &mut group,
        )
        .await
        .unwrap();

        let submitted = transport.submitted();
        assert_eq!(submitted.len(), before + 4, "authorize + keypackage + commit + welcome");

        // 1. DeviceAuthorized: cert binds the new device to the identity, event
        //    signed by the new device key.
        let authorized = &submitted[before];
        assert!(authorized.verify(&new_device.public_key()).is_ok());
        assert_eq!(authorized.core.author, identity.public_key());
        assert_eq!(authorized.core.device, device_id(&new_device.public_key()));
        match &authorized.core.payload {
            EventPayload::DeviceAuthorized { cert } => {
                assert_eq!(cert.core.identity, identity.public_key());
                assert_eq!(cert.core.device_id, device_id(&new_device.public_key()));
            }
            other => panic!("expected DeviceAuthorized first, got {other:?}"),
        }

        // 2. MlsKeyPackagePublished (the new device's own package).
        let published = &submitted[before + 1];
        match &published.core.payload {
            EventPayload::MlsKeyPackagePublished { .. } => {}
            other => panic!("expected MlsKeyPackagePublished second, got {other:?}"),
        }
        assert_eq!(published.core.device, device_id(&new_device.public_key()));

        // 3. MlsCommit add targeting the new device, citing the package above.
        let commit = &submitted[before + 2];
        match &commit.core.payload {
            EventPayload::MlsCommit { adds, removes, .. } => {
                assert!(removes.is_empty());
                assert_eq!(adds.len(), 1);
                assert_eq!(adds[0].identity, identity.public_key());
                assert_eq!(adds[0].device, device_id(&new_device.public_key()));
                assert_eq!(adds[0].key_package, published.hash());
            }
            other => panic!("expected MlsCommit third, got {other:?}"),
        }

        // 4. MlsWelcome addressed to the new device, citing the commit.
        let welcome = &submitted[before + 3];
        match &welcome.core.payload {
            EventPayload::MlsWelcome {
                for_member,
                for_device,
                commit: cited,
                ..
            } => {
                assert_eq!(for_member, &identity.public_key());
                assert_eq!(for_device, &device_id(&new_device.public_key()));
                assert_eq!(cited, &commit.hash());
            }
            other => panic!("expected MlsWelcome fourth, got {other:?}"),
        }

        // Chains: the new device advanced past authorize + keypackage; the
        // steward advanced past channel-created + bootstrap + commit + welcome.
        assert_eq!(new_chain.next_seq, 2);
        assert_eq!(steward_chain.next_seq, 4);

        // Outcome hashes line up with the submitted events.
        assert_eq!(outcome.device_authorized_hash, authorized.hash());
        assert_eq!(outcome.key_package_hash, published.hash());
        assert_eq!(outcome.commit_event_hash, commit.hash());
        assert_eq!(outcome.welcome_event_hash, welcome.hash());
        assert_eq!(outcome.local_epoch, 2);

        // The local group now holds BOTH leaves of the identity.
        let members = group.members().unwrap();
        assert!(members.iter().any(|m| {
            m.identity == identity.public_key() && m.device == device_id(&device.public_key())
        }));
        assert!(members.iter().any(|m| {
            m.identity == identity.public_key() && m.device == device_id(&new_device.public_key())
        }));
    }

    #[tokio::test]
    async fn a_device_cap_rejection_surfaces_the_fold_reason_clearly() {
        let transport = FakeTransport::new();
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let a = actor(&identity, &device);
        let mut chain = ChainState::default();

        transport.reject_next("event rejected: identity already has the maximum number of live devices");

        let err = authorize_device(&transport, &a, &mut chain).await.unwrap_err();

        match err {
            E2eeError::DeviceCapReached { reason } => {
                assert_eq!(
                    reason,
                    "event rejected: identity already has the maximum number of live devices"
                );
            }
            other => panic!("expected DeviceCapReached, got {other:?}"),
        }
        // A rejection advances nothing.
        assert_eq!(chain.next_seq, 0);
        assert_eq!(chain.last_event_hash, None);
    }

    #[tokio::test]
    async fn add_own_device_refuses_a_steward_from_a_different_identity() {
        let transport = FakeTransport::new();
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 102;
        let (identity, _device, k, mut group, store, hash, mut steward_chain) =
            stewarded_channel(&transport, channel_id).await;

        let new_device = Keypair::generate();
        let new_dir = temp_dir("new-device-misattributed");
        let (new_store, new_hash) = create_joiner_store(&new_dir, &k).unwrap();

        let steward_ctx = StewardContext {
            key: &k,
            generation: 0,
            store: &store,
            store_instance_hash: &hash,
        };
        let ctx = OwnDeviceContext {
            identity: &identity,
            new_device: &new_device,
            new_store: &new_store,
            new_store_instance_hash: &new_hash,
            steward: &steward_ctx,
        };

        // A DIFFERENT identity's device authors the add: the self-add rule
        // would reject it server-side, so we refuse before any submit.
        let other_identity = Keypair::generate();
        let other_device = Keypair::generate();
        let wrong_steward = actor(&other_identity, &other_device);
        let mut new_chain = ChainState::default();

        let before = transport.submit_count();
        let err = add_own_device(
            &transport,
            &ctx,
            &mut new_chain,
            &wrong_steward,
            &mut steward_chain,
            &mut group,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, E2eeError::Chain(_)), "got {err:?}");
        assert_eq!(transport.submit_count(), before, "nothing submitted for a mis-wired steward");
    }

    #[tokio::test]
    async fn add_own_device_surfaces_the_cap_rejection_from_within_the_orchestration() {
        let transport = FakeTransport::new();
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 103;
        let (identity, device, k, mut group, store, hash, mut steward_chain) =
            stewarded_channel(&transport, channel_id).await;

        let new_device = Keypair::generate();
        let new_dir = temp_dir("new-device-cap");
        let (new_store, new_hash) = create_joiner_store(&new_dir, &k).unwrap();

        let steward_ctx = StewardContext {
            key: &k,
            generation: 0,
            store: &store,
            store_instance_hash: &hash,
        };
        let ctx = OwnDeviceContext {
            identity: &identity,
            new_device: &new_device,
            new_store: &new_store,
            new_store_instance_hash: &new_hash,
            steward: &steward_ctx,
        };
        let steward_actor = actor(&identity, &device);
        let mut new_chain = ChainState::default();

        // The FIRST submit of the orchestration (the DeviceAuthorized) hits the
        // cap; the fold's reason must propagate out verbatim.
        transport.reject_next("event rejected: identity already has the maximum number of live devices");

        let before = transport.submit_count();
        let err = add_own_device(
            &transport,
            &ctx,
            &mut new_chain,
            &steward_actor,
            &mut steward_chain,
            &mut group,
        )
        .await
        .unwrap_err();

        match err {
            E2eeError::DeviceCapReached { reason } => {
                assert_eq!(
                    reason,
                    "event rejected: identity already has the maximum number of live devices"
                );
            }
            other => panic!("expected DeviceCapReached, got {other:?}"),
        }
        // Only the doomed DeviceAuthorized was submitted.
        assert_eq!(transport.submit_count(), before + 1);
        assert_eq!(new_chain.next_seq, 0);
    }
}
