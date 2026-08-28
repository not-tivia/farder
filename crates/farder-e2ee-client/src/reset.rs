//! C5 of the 5a lifecycle: `MlsGroupReset` emission (owner) and handling
//! (member) — the owner's "big hammer" to recover a broken/diverged channel.
//!
//! A reset tears the MLS group down and rebuilds it at `generation + 1`. The
//! fold's rules (`event_log_state.rs:1342-1394`, `:1239-1246`, `:1284-1316`)
//! force this exact sequence:
//!
//! 1. The owner mints a FRESH one-member group at the new generation (LOCAL —
//!    the fold knows nothing of it yet).
//! 2. The owner adds every current member in ONE commit, so every welcomed leaf
//!    lands at the SAME post-tree-hash (the confirmation wall checks each
//!    staged leaf against the reset's single declared hash; one add per member
//!    would give each a different tree hash and only the last could confirm).
//! 3. The owner stages one next-generation `MlsWelcome` per member (owner-only,
//!    `event_log_state.rs:1243-1246`), recorded by the fold keyed by event hash.
//! 4. The owner submits `MlsGroupReset { new_generation, welcomes, post_tree_hash }`.
//! 5. Each member fetches their staged Welcome, joins at the new generation, and
//!    confirms with `tree_hash == the reset's declared post_tree_hash`.
//!
//! # Why this is NOT [`crate::commit::add_member`]
//!
//! [`crate::commit::add_member`] submits an `MlsCommit` + `MlsWelcome` at the
//! CURRENT generation. A reset stages Welcomes WITHOUT an accepted commit first:
//! the new generation has no group in the fold until the reset lands. So
//! [`reset_group`] builds the fresh-group → add → staged-welcome → reset
//! sequence itself from the lower-level [`MlsChannelGroup`] methods, and never
//! emits a commit.
//!
//! # The exact-cover contract (caller's responsibility)
//!
//! The fold accepts a reset only if the staged Welcomes cover EXACTLY
//! `member_leaf_set(timestamp) − the resetter's own device` — no more, no
//! fewer (`event_log_state.rs:1387-1392`). [`reset_group`] stages exactly one
//! Welcome per entry of `members`, so the CALLER must pass the complete current
//! member × live-device set minus the owner's authoring device (including the
//! owner's OWN other devices, which are also welcomed). [`member_live_leaves`]
//! enumerates one identity's live (authorized, un-revoked, un-expired) devices
//! to help build that set; the caller still decides which identities are
//! current members.

use std::collections::{HashMap, HashSet};

use farder_crypto::event_log::{DeviceId, Event, EventPayload};
use farder_crypto::identity::PublicKey;
use farder_mls::credential::{credential_with_key, decode_credential_identity, DeviceSigner};
use farder_mls::group::{decode_key_package, DeclaredMember, MlsChannelGroup};
use farder_mls::store::FarderMlsStore;

use crate::chain::{build_next_event, event_now_secs, Actor, ChainState};
use crate::channel::{channel_group_id, E2eeError};
use crate::channel_key::ChannelKey;
use crate::join::{join_channel, LeafConfirmation, PendingWelcome, SendEligibility};
use crate::transport::E2eeTransport;

/// The `commit` ref carried by a next-generation (reset-staging) `MlsWelcome`.
///
/// The reset generation's add-commit is NEVER a log event, so there is no
/// event hash to cite. The fold's next-generation Welcome arm
/// (`event_log_state.rs:1239-1246`) checks only `is_owner(author)` — it never
/// reads the `commit` field — so this is a well-formed placeholder, documented
/// rather than a magic inline string.
const RESET_WELCOME_COMMIT_SENTINEL: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// The fixed inputs for a reset emit or a reset join: the channel and the MLS
/// store plus its instance hash (which always travel together). Mirrors
/// [`crate::commit::StewardContext`] / [`crate::rekey::RekeyContext`]; named for
/// the operation it serves. Shared by [`reset_group`] (the resetter) and
/// [`join_reset`] (a welcomed member): both operate on the same channel's store.
pub struct ResetContext<'a> {
    pub key: &'a ChannelKey,
    pub store: &'a FarderMlsStore,
    pub store_instance_hash: &'a [u8; 32],
}

/// The result of a successful [`reset_group`].
///
/// Unlike the own-commit outcomes (`CommitSubmitted` / `RekeyOutcome` /
/// `DriftDischargeOutcome`), there is no `local_epoch` and no divergence caveat:
/// the fresh generation's group is LOCAL-ONLY — it is never submitted as a
/// commit, so there is no epoch CAS to lose. A rejection (a wrong `generation`,
/// a wrong member set, or the reset rate limit) surfaces as a plain
/// [`E2eeError::Transport`] with the fold's reason preserved verbatim.
#[derive(Debug)]
pub struct ResetOutcome {
    /// Server-assigned hash of the accepted `MlsGroupReset` event.
    pub event_hash: String,
    pub new_generation: u64,
    /// The fresh generation's tree hash after welcoming everyone — the value
    /// every post-reset confirmation is validated against.
    pub post_tree_hash: [u8; 32],
}

/// Reset a channel (owner): mint the fresh next-generation group, add `members`,
/// stage one next-generation Welcome per member, and submit `MlsGroupReset`.
///
/// `generation` is the group's CURRENT generation (the fold requires
/// `new_generation == generation + 1`); `members` is the complete current
/// member × live-device set minus the owner's own authoring device (see the
/// module doc — this is the caller's responsibility). The fresh group is
/// created in `ctx.store` under the new generation's group id and is persisted
/// there, so the caller can later `MlsChannelGroup::load` it.
pub async fn reset_group<T: E2eeTransport + Sync>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    ctx: &ResetContext<'_>,
    generation: u64,
    members: &[DeclaredMember],
) -> Result<ResetOutcome, E2eeError> {
    let new_generation = generation
        .checked_add(1)
        .ok_or_else(|| E2eeError::chain("reset generation overflow: cannot advance past u64::MAX"))?;

    // 1. Mint the fresh next-generation group. This is LOCAL only: the creation
    //    and the single add-commit below are never log events, and the fold
    //    learns the new generation only when the MlsGroupReset lands. The
    //    resetting owner's authoring device is the tree by construction.
    let mut fresh = MlsChannelGroup::create(
        ctx.store,
        &DeviceSigner(actor.device),
        credential_with_key(actor.device, &actor.identity.public_key()),
        channel_group_id(&ctx.key.log_server_id, ctx.key.channel_id, new_generation).as_bytes(),
    )
    .map_err(|e| E2eeError::Mls(e.context("create the reset generation's group")))?;

    // 2. Add every member in ONE commit (see the module doc for why one-per-
    //    member would break the confirmation wall), then stage one Welcome per
    //    member.
    let mut welcome_refs: Vec<String> = Vec::with_capacity(members.len());
    if !members.is_empty() {
        let mut key_packages = Vec::with_capacity(members.len());
        for member in members {
            let kp_event_bytes = transport
                .fetch_key_packages(&member.identity, &member.device)
                .await?
                .into_iter()
                .last()
                .ok_or_else(|| {
                    E2eeError::chain(format!(
                        "member {} has no published key packages to reset into the new generation",
                        member.device
                    ))
                })?;
            let kp_event = Event::from_bytes(&kp_event_bytes).map_err(|e| {
                E2eeError::Mls(anyhow::anyhow!("decode key package event bytes: {e}"))
            })?;
            let EventPayload::MlsKeyPackagePublished { key_package, .. } = &kp_event.core.payload else {
                return Err(E2eeError::chain(
                    "fetch_key_packages returned a non-MlsKeyPackagePublished event",
                ));
            };
            let key_package = decode_key_package(ctx.store, key_package).map_err(|e| {
                E2eeError::Mls(
                    e.context("decode fetched key package (non-farder credential fails closed)"),
                )
            })?;
            let (kp_identity, kp_device) = decode_credential_identity(
                key_package.leaf_node().credential().serialized_content(),
            )
            .map_err(|e| E2eeError::Mls(e.context("decode fetched key package credential")))?;
            if kp_identity != member.identity || kp_device != member.device {
                return Err(E2eeError::chain(
                    "fetched key package credential does not match the member being reset",
                ));
            }
            key_packages.push(key_package);
        }

        let outcome = fresh
            .add_members(ctx.store, &DeviceSigner(actor.device), &key_packages)
            .map_err(|e| E2eeError::Mls(e.context("add members to the reset generation's group")))?;
        debug_assert_eq!(outcome.adds.as_slice(), members);
        let welcome_bytes = outcome.welcome_bytes.clone().ok_or_else(|| {
            E2eeError::Mls(anyhow::anyhow!("add_members produced no welcome for a non-empty member set"))
        })?;

        // 3. Stage one next-generation Welcome per member — the SAME Welcome
        //    bytes, one event per member so the fold's exact-cover check can
        //    count them (owner-only, `event_log_state.rs:1243-1246`).
        for member in members {
            let welcome_event = build_next_event(
                actor.device,
                actor.identity,
                actor.log_server_id,
                chain,
                event_now_secs(),
                EventPayload::MlsWelcome {
                    channel_id: ctx.key.channel_id,
                    generation: new_generation,
                    commit: RESET_WELCOME_COMMIT_SENTINEL.to_string(),
                    for_member: member.identity.clone(),
                    for_device: member.device.clone(),
                    welcome: welcome_bytes.clone(),
                },
            );
            let accepted = transport.submit_event(&welcome_event).await?;
            chain.advance(&welcome_event);
            welcome_refs.push(accepted.event_hash);
        }
    }

    // 4. The reset's declared post-tree-hash: the fresh group's tree hash after
    //    the single add-commit (or, for an owner-only channel, at creation).
    let post_tree_hash = fresh.tree_hash();

    // 5. Submit the reset citing exactly the staged Welcomes. The fold's
    //    exact-cover rule (`event_log_state.rs:1387-1392`) checks the refs cover
    //    member_leaf_set minus the resetter's own device.
    let reset_event = build_next_event(
        actor.device,
        actor.identity,
        actor.log_server_id,
        chain,
        event_now_secs(),
        EventPayload::MlsGroupReset {
            channel_id: ctx.key.channel_id,
            new_generation,
            welcomes: welcome_refs,
            post_tree_hash,
        },
    );
    let accepted = transport.submit_event(&reset_event).await?;
    chain.advance(&reset_event);

    Ok(ResetOutcome {
        event_hash: accepted.event_hash,
        new_generation,
        post_tree_hash,
    })
}

/// Handle a reset (member): join from the staged Welcome at the new generation
/// and confirm the leaf against the reset's declared `post_tree_hash`.
///
/// This is [`crate::join::confirm_leaf`]'s reset twin. The difference is the
/// `tree_hash` it submits: an ordinary join cites the adding commit's
/// `post_tree_hash` (from [`crate::join::JoinInfo`]), but a reset generation's
/// add-commit is never a log event, so the confirmation wall's anchor is the
/// tree hash the RESETTER declared on `MlsGroupReset` (`event_log_state.rs:1284-1316`).
/// The two values must agree — the resetter created the group — and this fn
/// checks that locally, failing closed before emitting a doomed confirmation.
pub async fn join_reset<T: E2eeTransport + Sync>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    ctx: &ResetContext<'_>,
    welcome: &PendingWelcome,
    reset_post_tree_hash: [u8; 32],
) -> Result<LeafConfirmation, E2eeError> {
    if ctx.key.channel_id != welcome.channel_id {
        return Err(E2eeError::chain(format!(
            "joining reset for channel {} but the Welcome is for channel {}",
            ctx.key.channel_id, welcome.channel_id
        )));
    }

    // Join from the staged Welcome (LOCAL: no event is submitted by the join).
    let (_group, join_info) = join_channel(ctx.store, &welcome.welcome)?;

    // The resetter created the group, so the tree we landed in must be exactly
    // the hash it declared. A mismatch means a foreign Welcome or a wrong
    // declared hash: fail closed before emitting a confirmation the fold would
    // reject anyway.
    if join_info.tree_hash != reset_post_tree_hash {
        return Err(E2eeError::chain(
            "the reset Welcome's tree hash does not match the reset's declared post_tree_hash",
        ));
    }

    let event = build_next_event(
        actor.device,
        actor.identity,
        actor.log_server_id,
        chain,
        event_now_secs(),
        EventPayload::MlsLeafConfirmed {
            channel_id: ctx.key.channel_id,
            generation: welcome.generation,
            epoch: join_info.epoch,
            tree_hash: reset_post_tree_hash,
            store_instance_hash: *ctx.store_instance_hash,
        },
    );
    let accepted = transport.submit_event(&event).await?;
    chain.advance(&event);

    Ok(LeafConfirmation {
        event_hash: accepted.event_hash,
        epoch: join_info.epoch,
        eligibility: SendEligibility::confirmed(),
    })
}

/// Enumerate one identity's LIVE devices — every `(identity, device)` whose
/// device is authorized, not revoked, and not expired — from the log's
/// device-lifecycle stream. This is the helper a reset caller uses to build the
/// exact-cover member set ([`reset_group`]'s `members`).
///
/// Revocation- and expiry-aware, exactly like [`crate::cert::resolve_device_cert`]
/// (sub-5 C1): a device named by any `DeviceRevoked` is dropped, and a device
/// whose NEWEST verifying cert has an `expires_at` already past the client's
/// local clock is dropped. The result is sorted by device id for determinism.
pub async fn member_live_leaves<T: E2eeTransport + Sync>(
    transport: &T,
    identity: &PublicKey,
) -> Result<Vec<DeclaredMember>, E2eeError> {
    let events = transport.fetch_device_certs(identity).await?;

    // First pass: every device this identity has revoked. `DeviceRevoked` names
    // the VICTIM in its payload (not the revoker), so collect the revoked ids
    // before judging any cert.
    let mut revoked: HashSet<DeviceId> = HashSet::new();
    for bytes in &events {
        let event = Event::from_bytes(bytes)
            .map_err(|e| E2eeError::Mls(anyhow::anyhow!("decode device-lifecycle event: {e}")))?;
        if let EventPayload::DeviceRevoked { device } = &event.core.payload {
            revoked.insert(device.clone());
        }
    }

    // Second pass: the newest verifying, un-revoked cert per device (events are
    // oldest-first, so the last write wins), keeping its expiry for the final
    // judgement.
    let mut newest_expiry: HashMap<DeviceId, Option<u64>> = HashMap::new();
    for bytes in events {
        let event = Event::from_bytes(&bytes)
            .map_err(|e| E2eeError::Mls(anyhow::anyhow!("decode device-lifecycle event: {e}")))?;
        let EventPayload::DeviceAuthorized { cert } = &event.core.payload else {
            continue;
        };
        if &cert.core.identity != identity {
            continue;
        }
        if cert.verify().is_err() {
            continue;
        }
        if revoked.contains(&cert.core.device_id) {
            continue;
        }
        newest_expiry.insert(cert.core.device_id.clone(), cert.core.expires_at);
    }

    let mut leaves: Vec<DeclaredMember> = newest_expiry
        .into_iter()
        .filter_map(|(device, expires_at)| {
            if let Some(expires_at) = expires_at {
                if expires_at < event_now_secs() {
                    return None;
                }
            }
            Some(DeclaredMember {
                identity: identity.clone(),
                device,
            })
        })
        .collect();
    leaves.sort_by(|a, b| a.device.cmp(&b.device));
    Ok(leaves)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::join::fetch_pending_welcomes;
    use crate::testing::FakeTransport;
    use crate::transport::{EventAccepted, MlsControl, TransportError, Welcomes};
    use farder_crypto::event_log::{
        device_id, ChannelClass, DeclaredAdd, DeviceCert, EventPayload as EP, Genesis,
        E2EE_CHANNEL_ID_FLOOR,
    };
    use farder_crypto::event_log_state::LogState;
    use farder_crypto::identity::Keypair;
    use farder_mls::credential::generate_key_package;
    use farder_mls::group::{decode_key_package, CommitOutcome};
    use openmls::prelude::KeyPackage;
    use std::future::Future;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;
    use tls_codec::Serialize as TlsSerialize;

    const SERVER_ID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
    const TS: u64 = 500;

    static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let n = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "farder-e2ee-client-reset-{name}-{}-{n}",
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

    fn mk_store(dir: &Path, file: &str) -> FarderMlsStore {
        let path = dir.join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let (store, _) = FarderMlsStore::create(&path).unwrap();
        store
    }

    fn member_of(id: &Keypair, dev: &Keypair) -> DeclaredMember {
        DeclaredMember {
            identity: id.public_key(),
            device: device_id(&dev.public_key()),
        }
    }

    /// Serve one member's published KeyPackage on a [`FakeTransport`] and return
    /// their [`DeclaredMember`] (the KP is minted from `owner_store` so it is
    /// decodable by the owner during `reset_group`).
    fn serve_member_kp(
        transport: &FakeTransport,
        owner_store: &FarderMlsStore,
        id: &Keypair,
        dev: &Keypair,
    ) -> DeclaredMember {
        let bundle = generate_key_package(owner_store, dev, &id.public_key()).unwrap();
        let kp_bytes = bundle.key_package().tls_serialize_detached().unwrap();
        let kp_event = build_next_event(
            dev,
            id,
            SERVER_ID,
            &ChainState::default(),
            event_now_secs(),
            EP::MlsKeyPackagePublished {
                key_package: kp_bytes,
                store_instance_hash: [0u8; 32],
                expires_at_log_pos: u64::MAX,
            },
        );
        transport.serve_key_packages(
            &id.public_key(),
            &device_id(&dev.public_key()),
            vec![kp_event.to_bytes()],
        );
        member_of(id, dev)
    }

    /// A (joiner store, joiner identity/device, store hash, staged Welcome,
    /// declared post-tree-hash) bundle: the owner mints a fresh generation-1
    /// group and adds the joiner, producing a real Welcome.
    fn reset_welcome_fixture(
        channel_id: u64,
    ) -> (FarderMlsStore, Keypair, Keypair, [u8; 32], PendingWelcome, [u8; 32]) {
        let dir = temp_dir("join-reset-fixture");
        let k = key(channel_id);

        let owner_id = Keypair::generate();
        let owner_dev = Keypair::generate();
        let joiner_id = Keypair::generate();
        let joiner_dev = Keypair::generate();

        let owner_store_path = {
            let mut p = k.mls_store_path(&dir).unwrap();
            p.set_file_name(format!("{channel_id}.owner.sqlite"));
            p
        };
        std::fs::create_dir_all(owner_store_path.parent().unwrap()).unwrap();
        let (owner_store, _) = FarderMlsStore::create(&owner_store_path).unwrap();
        let mut fresh = MlsChannelGroup::create(
            &owner_store,
            &DeviceSigner(&owner_dev),
            credential_with_key(&owner_dev, &owner_id.public_key()),
            channel_group_id(SERVER_ID, channel_id, 1).as_bytes(),
        )
        .unwrap();

        let joiner_store_path = {
            let mut p = k.mls_store_path(&dir).unwrap();
            p.set_file_name(format!("{channel_id}.joiner.sqlite"));
            p
        };
        std::fs::create_dir_all(joiner_store_path.parent().unwrap()).unwrap();
        let (joiner_store, _) = FarderMlsStore::create(&joiner_store_path).unwrap();
        let joiner_hash = joiner_store.store_instance_hash();

        let bundle = generate_key_package(&joiner_store, &joiner_dev, &joiner_id.public_key())
            .unwrap();
        let kp_bytes = bundle.key_package().tls_serialize_detached().unwrap();
        let kp = decode_key_package(&owner_store, &kp_bytes).unwrap();
        let outcome = fresh
            .add_members(&owner_store, &DeviceSigner(&owner_dev), &[kp])
            .unwrap();
        let welcome = outcome.welcome_bytes.clone().unwrap();

        let pending = PendingWelcome {
            channel_id,
            generation: 1,
            welcome,
        };
        (joiner_store, joiner_id, joiner_dev, joiner_hash, pending, outcome.post_tree_hash)
    }

    fn device_authorized_bytes(identity: &Keypair, device: &Keypair, created_at: u64) -> Vec<u8> {
        let cert = DeviceCert::create(identity, &device.public_key(), created_at);
        build_next_event(
            device,
            identity,
            SERVER_ID,
            &ChainState::default(),
            created_at,
            EP::DeviceAuthorized { cert },
        )
        .to_bytes()
    }

    fn device_authorized_expiring_bytes(
        identity: &Keypair,
        device: &Keypair,
        created_at: u64,
        expires_at: u64,
    ) -> Vec<u8> {
        let cert = DeviceCert::create_expiring(identity, &device.public_key(), created_at, expires_at);
        build_next_event(
            device,
            identity,
            SERVER_ID,
            &ChainState::default(),
            created_at,
            EP::DeviceAuthorized { cert },
        )
        .to_bytes()
    }

    fn device_revoked_bytes(
        identity: &Keypair,
        revoker: &Keypair,
        revoked_device: &str,
        timestamp: u64,
    ) -> Vec<u8> {
        build_next_event(
            revoker,
            identity,
            SERVER_ID,
            &ChainState::default(),
            timestamp,
            EP::DeviceRevoked {
                device: revoked_device.to_string(),
            },
        )
        .to_bytes()
    }

    // ---- reset_group (owner emit) unit tests ----

    #[tokio::test]
    async fn reset_group_stages_one_welcome_per_member_then_submits_the_reset() {
        let transport = FakeTransport::new();
        let owner_id = Keypair::generate();
        let owner_dev = Keypair::generate();
        let a = actor(&owner_id, &owner_dev);
        let mut chain = ChainState::default();
        let dir = temp_dir("reset-emit");
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 100;
        let k = key(channel_id);

        let store_path = k.mls_store_path(&dir).unwrap();
        std::fs::create_dir_all(store_path.parent().unwrap()).unwrap();
        let (store, _) = FarderMlsStore::create(&store_path).unwrap();
        let store_hash = store.store_instance_hash();

        let alice_id = Keypair::generate();
        let alice_dev = Keypair::generate();
        let bob_id = Keypair::generate();
        let bob_dev = Keypair::generate();
        let alice = serve_member_kp(&transport, &store, &alice_id, &alice_dev);
        let bob = serve_member_kp(&transport, &store, &bob_id, &bob_dev);
        let members = vec![alice.clone(), bob.clone()];

        let ctx = ResetContext {
            key: &k,
            store: &store,
            store_instance_hash: &store_hash,
        };
        let outcome = reset_group(&transport, &a, &mut chain, &ctx, 0, &members)
            .await
            .unwrap();

        assert_eq!(outcome.new_generation, 1);

        // Exactly two staged Welcomes then the reset.
        let events = transport.submitted();
        assert_eq!(events.len(), 3);
        let welcome0_hash = events[0].hash();
        let welcome1_hash = events[1].hash();

        for (i, expected) in [&alice, &bob].into_iter().enumerate() {
            match &events[i].core.payload {
                EP::MlsWelcome {
                    channel_id: cid,
                    generation,
                    commit,
                    for_member,
                    for_device,
                    welcome,
                } => {
                    assert_eq!(*cid, channel_id);
                    assert_eq!(*generation, 1, "a reset-staging welcome is for the NEXT generation");
                    assert_eq!(commit, RESET_WELCOME_COMMIT_SENTINEL);
                    assert_eq!(for_member, &expected.identity);
                    assert_eq!(for_device, &expected.device);
                    assert!(!welcome.is_empty());
                }
                other => panic!("expected MlsWelcome, got {other:?}"),
            }
        }
        // Both members carry the SAME Welcome bytes (one add-commit welcomes all).
        match (&events[0].core.payload, &events[1].core.payload) {
            (EP::MlsWelcome { welcome: w0, .. }, EP::MlsWelcome { welcome: w1, .. }) => {
                assert_eq!(w0, w1);
            }
            _ => unreachable!("both first events are welcomes"),
        }

        // The reset cites exactly the two staged welcome hashes and declares the
        // fresh group's post-tree-hash.
        match &events[2].core.payload {
            EP::MlsGroupReset {
                channel_id: cid,
                new_generation,
                welcomes,
                post_tree_hash,
            } => {
                assert_eq!(*cid, channel_id);
                assert_eq!(*new_generation, 1);
                assert_eq!(welcomes, &vec![welcome0_hash, welcome1_hash]);
                assert_eq!(post_tree_hash, &outcome.post_tree_hash);
            }
            other => panic!("expected MlsGroupReset, got {other:?}"),
        }

        // The declared hash is the real tree hash of the fresh group the owner
        // minted in the store at generation 1.
        let fresh = MlsChannelGroup::load(
            &store,
            &DeviceSigner(&owner_dev),
            channel_group_id(SERVER_ID, channel_id, 1).as_bytes(),
        )
        .unwrap()
        .expect("the fresh generation group is persisted in the owner's store");
        assert_eq!(fresh.tree_hash(), outcome.post_tree_hash);
        let fresh_members = fresh.members().unwrap();
        assert_eq!(fresh_members.len(), 3, "owner + both members");
        assert!(fresh_members.contains(&alice));
        assert!(fresh_members.contains(&bob));

        // The chain advanced past all three events.
        assert_eq!(chain.next_seq, 3);
        assert_eq!(chain.last_event_hash.as_deref(), Some(outcome.event_hash.as_str()));
    }

    #[tokio::test]
    async fn reset_group_refuses_a_key_package_that_claims_a_different_member() {
        let transport = FakeTransport::new();
        let owner_id = Keypair::generate();
        let owner_dev = Keypair::generate();
        let a = actor(&owner_id, &owner_dev);
        let mut chain = ChainState::default();
        let dir = temp_dir("reset-bad-kp");
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 102;
        let k = key(channel_id);

        let store_path = k.mls_store_path(&dir).unwrap();
        std::fs::create_dir_all(store_path.parent().unwrap()).unwrap();
        let (store, _) = FarderMlsStore::create(&store_path).unwrap();
        let store_hash = store.store_instance_hash();

        // Serve a key package for CHARLIE, but ask to reset BOB.
        let charlie_id = Keypair::generate();
        let charlie_dev = Keypair::generate();
        let _charlie = serve_member_kp(&transport, &store, &charlie_id, &charlie_dev);
        let bob_id = Keypair::generate();
        let bob_dev = Keypair::generate();
        let bob = member_of(&bob_id, &bob_dev);

        let ctx = ResetContext {
            key: &k,
            store: &store,
            store_instance_hash: &store_hash,
        };
        let before = transport.submit_count();
        let err = reset_group(&transport, &a, &mut chain, &ctx, 0, &[bob])
            .await
            .unwrap_err();

        assert!(matches!(err, E2eeError::Chain(_)), "got {err:?}");
        assert_eq!(transport.submit_count(), before, "nothing submitted for a mismatch");
    }

    // ---- join_reset (member handle) unit tests ----

    #[tokio::test]
    async fn join_reset_confirms_against_the_declared_tree_hash() {
        let transport = FakeTransport::new();
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 200;
        let (joiner_store, joiner_id, joiner_dev, joiner_hash, pending, post_tree_hash) =
            reset_welcome_fixture(channel_id);
        let k = key(channel_id);
        let a = actor(&joiner_id, &joiner_dev);
        let mut chain = ChainState::default();
        let ctx = ResetContext {
            key: &k,
            store: &joiner_store,
            store_instance_hash: &joiner_hash,
        };

        let confirmation = join_reset(&transport, &a, &mut chain, &ctx, &pending, post_tree_hash)
            .await
            .unwrap();

        assert!(confirmation.can_send());
        assert_eq!(confirmation.epoch, 1);

        let events = transport.submitted();
        assert_eq!(events.len(), 1);
        let event = &events[0];
        // Authored BY the joining device.
        assert_eq!(event.core.author, joiner_id.public_key());
        assert_eq!(event.core.device, device_id(&joiner_dev.public_key()));
        match &event.core.payload {
            EP::MlsLeafConfirmed {
                channel_id: cid,
                generation,
                epoch,
                tree_hash,
                store_instance_hash,
            } => {
                assert_eq!(*cid, channel_id);
                assert_eq!(*generation, pending.generation);
                assert_eq!(*epoch, 1);
                assert_eq!(*tree_hash, post_tree_hash);
                assert_eq!(store_instance_hash, &joiner_hash);
            }
            other => panic!("expected MlsLeafConfirmed, got {other:?}"),
        }
        assert_eq!(chain.next_seq, 1);
        assert_eq!(chain.last_event_hash.as_deref(), Some(confirmation.event_hash.as_str()));
    }

    #[tokio::test]
    async fn join_reset_fails_closed_when_the_welcome_does_not_match_the_declared_hash() {
        let transport = FakeTransport::new();
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 202;
        let (joiner_store, joiner_id, joiner_dev, joiner_hash, pending, _real_hash) =
            reset_welcome_fixture(channel_id);
        let k = key(channel_id);
        let a = actor(&joiner_id, &joiner_dev);
        let mut chain = ChainState::default();
        let ctx = ResetContext {
            key: &k,
            store: &joiner_store,
            store_instance_hash: &joiner_hash,
        };

        let err = join_reset(&transport, &a, &mut chain, &ctx, &pending, [0xFFu8; 32])
            .await
            .unwrap_err();

        assert!(matches!(err, E2eeError::Chain(_)), "got {err:?}");
        assert_eq!(transport.submit_count(), 0, "nothing submitted for a mismatched Welcome");
    }

    #[tokio::test]
    async fn join_reset_rejects_a_welcome_for_a_different_channel() {
        let transport = FakeTransport::new();
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 204;
        let (joiner_store, joiner_id, joiner_dev, joiner_hash, pending, post_tree_hash) =
            reset_welcome_fixture(channel_id);
        let wrong_key = key(E2EE_CHANNEL_ID_FLOOR + 205);
        let a = actor(&joiner_id, &joiner_dev);
        let mut chain = ChainState::default();
        let ctx = ResetContext {
            key: &wrong_key,
            store: &joiner_store,
            store_instance_hash: &joiner_hash,
        };

        let err = join_reset(&transport, &a, &mut chain, &ctx, &pending, post_tree_hash)
            .await
            .unwrap_err();
        assert!(matches!(err, E2eeError::Chain(_)), "got {err:?}");
        assert_eq!(transport.submit_count(), 0);
    }

    // ---- member_live_leaves unit test ----

    #[tokio::test]
    async fn member_live_leaves_drops_revoked_and_expired_devices() {
        let transport = FakeTransport::new();
        let identity = Keypair::generate();
        let dev_a = Keypair::generate();
        let dev_b = Keypair::generate();
        let dev_c = Keypair::generate();

        let auth_a = device_authorized_bytes(&identity, &dev_a, 1);
        let auth_b = device_authorized_bytes(&identity, &dev_b, 2);
        let auth_c = device_authorized_expiring_bytes(&identity, &dev_c, 3, 100); // expired
        let revoke_b = device_revoked_bytes(&identity, &dev_a, &device_id(&dev_b.public_key()), 4);
        transport.serve_device_certs(
            &identity.public_key(),
            vec![auth_a, auth_b, auth_c, revoke_b],
        );

        let leaves = member_live_leaves(&transport, &identity.public_key())
            .await
            .unwrap();

        assert_eq!(leaves.len(), 1, "only dev_a is live, revoked, and un-expired");
        assert_eq!(leaves[0].identity, identity.public_key());
        assert_eq!(leaves[0].device, device_id(&dev_a.public_key()));
    }

    // ---- real LogState fold replay ----

    /// An [`E2eeTransport`] whose `submit_event` applies each event to a REAL
    /// [`LogState`] (rejecting with the fold's own message), and which serves
    /// key packages / welcomes / device certs back from the events it accepted.
    struct FoldTransport {
        st: Mutex<LogState>,
        key_packages: Mutex<HashMap<(PublicKey, String), Vec<Vec<u8>>>>,
        welcomes: Mutex<Vec<Vec<u8>>>,
        device_certs: Mutex<HashMap<PublicKey, Vec<Vec<u8>>>>,
    }

    impl FoldTransport {
        fn new(st: LogState) -> Self {
            Self {
                st: Mutex::new(st),
                key_packages: Mutex::new(HashMap::new()),
                welcomes: Mutex::new(Vec::new()),
                device_certs: Mutex::new(HashMap::new()),
            }
        }

        fn lock_state(&self) -> std::sync::MutexGuard<'_, LogState> {
            self.st.lock().unwrap()
        }
    }

    impl E2eeTransport for FoldTransport {
        fn submit_event(
            &self,
            event: &Event,
        ) -> impl Future<Output = Result<EventAccepted, TransportError>> + Send {
            let event = event.clone();
            let result = {
                let mut st = self.st.lock().unwrap();
                st.apply(&event).map(|_| EventAccepted {
                    event_hash: event.hash(),
                    timestamp: event.core.timestamp,
                })
                .map_err(|e| TransportError::rejected(format!("event rejected: {e}")))
            };
            if result.is_ok() {
                match &event.core.payload {
                    EP::MlsKeyPackagePublished { .. } => {
                        self.key_packages
                            .lock()
                            .unwrap()
                            .entry((event.core.author.clone(), event.core.device.clone()))
                            .or_default()
                            .push(event.to_bytes());
                    }
                    EP::MlsWelcome { .. } => {
                        self.welcomes.lock().unwrap().push(event.to_bytes());
                    }
                    EP::DeviceAuthorized { .. } | EP::DeviceRevoked { .. } => {
                        self.device_certs
                            .lock()
                            .unwrap()
                            .entry(event.core.author.clone())
                            .or_default()
                            .push(event.to_bytes());
                    }
                    _ => {}
                }
            }
            async move { result }
        }

        fn fetch_welcomes(
            &self,
            channel_id: Option<u64>,
            _since_accept_seq: u64,
        ) -> impl Future<Output = Result<Welcomes, TransportError>> + Send {
            let all = self.welcomes.lock().unwrap().clone();
            let events = all
                .into_iter()
                .filter(|bytes| match Event::from_bytes(bytes) {
                    Ok(e) => match &e.core.payload {
                        EP::MlsWelcome { channel_id: cid, .. } => {
                            channel_id.is_none_or(|c| c == *cid)
                        }
                        _ => false,
                    },
                    Err(_) => false,
                })
                .collect();
            async move {
                Ok(Welcomes {
                    events,
                    next_accept_seq: 0,
                    more: false,
                })
            }
        }

        fn fetch_mls_control(
            &self,
            _channel_id: u64,
            _since_accept_seq: u64,
        ) -> impl Future<Output = Result<MlsControl, TransportError>> + Send {
            async move {
                Ok(MlsControl {
                    events: vec![],
                    next_accept_seq: 0,
                    more: false,
                })
            }
        }

        fn fetch_key_packages(
            &self,
            member: &PublicKey,
            device: &str,
        ) -> impl Future<Output = Result<Vec<Vec<u8>>, TransportError>> + Send {
            let events = self
                .key_packages
                .lock()
                .unwrap()
                .get(&(member.clone(), device.to_string()))
                .cloned()
                .unwrap_or_default();
            async move { Ok(events) }
        }

        fn fetch_device_certs(
            &self,
            identity: &PublicKey,
        ) -> impl Future<Output = Result<Vec<Vec<u8>>, TransportError>> + Send {
            let events = self
                .device_certs
                .lock()
                .unwrap()
                .get(identity)
                .cloned()
                .unwrap_or_default();
            async move { Ok(events) }
        }

        fn fetch_history_v2(
            &self,
            _channel_id: u64,
            _before_id: Option<u64>,
            _limit: u32,
        ) -> impl Future<
            Output = Result<Vec<farder_protocol::server::MessageInfoV2>, TransportError>,
        > + Send {
            async move { Ok(Vec::new()) }
        }
    }

    /// A participant holding a real on-disk MLS store (used by the full
    /// two-member-channel replay).
    struct FParty {
        id: Keypair,
        dev: Keypair,
        store: FarderMlsStore,
        store_hash: [u8; 32],
        chain: ChainState,
    }

    impl FParty {
        fn member(&self) -> DeclaredMember {
            member_of(&self.id, &self.dev)
        }
    }

    /// A chain-only participant (no MLS store) for the rejection tests, where
    /// real MLS is not needed — only the fold's membership bookkeeping is.
    struct CParty {
        id: Keypair,
        dev: Keypair,
        chain: ChainState,
    }

    impl CParty {
        fn member(&self) -> DeclaredMember {
            member_of(&self.id, &self.dev)
        }
    }

    async fn submit(
        t: &FoldTransport,
        chain: &mut ChainState,
        dev: &Keypair,
        id: &Keypair,
        sid: &str,
        ts: u64,
        payload: EP,
    ) -> Event {
        let ev = build_next_event(dev, id, sid, chain, ts, payload);
        let accepted = t
            .submit_event(&ev)
            .await
            .unwrap_or_else(|e| panic!("fold rejected a setup event: {e}"));
        assert_eq!(accepted.event_hash, ev.hash());
        chain.advance(&ev);
        ev
    }

    fn mls_commit(
        channel_id: u64,
        generation: u64,
        outcome: &CommitOutcome,
        post_epoch_authenticator: [u8; 32],
        adds: Vec<DeclaredAdd>,
        store_hash: [u8; 32],
    ) -> EP {
        EP::MlsCommit {
            channel_id,
            generation,
            epoch: outcome.epoch,
            mls_message: outcome.commit_bytes.clone(),
            adds,
            removes: vec![],
            prev_epoch_authenticator: outcome.prev_epoch_authenticator,
            post_epoch_authenticator,
            post_tree_hash: outcome.post_tree_hash,
            authz_head: "0".repeat(64),
            store_instance_hash: store_hash,
        }
    }

    /// Publish a REAL KeyPackage for `party` from their own store, and return
    /// `(event ref, decoded KeyPackage ready for add_members)`.
    async fn publish_kp(
        t: &FoldTransport,
        party: &mut FParty,
        sid: &str,
        adder_store: &FarderMlsStore,
    ) -> (String, KeyPackage) {
        let bundle = generate_key_package(&party.store, &party.dev, &party.id.public_key()).unwrap();
        let bytes = bundle.key_package().tls_serialize_detached().unwrap();
        let ev = submit(
            t,
            &mut party.chain,
            &party.dev,
            &party.id,
            sid,
            TS,
            EP::MlsKeyPackagePublished {
                key_package: bytes.clone(),
                store_instance_hash: party.store_hash,
                expires_at_log_pos: 1_000_000,
            },
        )
        .await;
        let kp = decode_key_package(adder_store, &bytes).unwrap();
        (ev.hash(), kp)
    }

    async fn join_member_only(t: &FoldTransport, sid: &str, invite: &Event) -> CParty {
        let id = Keypair::generate();
        let dev = Keypair::generate();
        let mut chain = ChainState::default();
        submit(
            t,
            &mut chain,
            &dev,
            &id,
            sid,
            1,
            EP::DeviceAuthorized {
                cert: DeviceCert::create(&id, &dev.public_key(), 1),
            },
        )
        .await;
        submit(
            t,
            &mut chain,
            &dev,
            &id,
            sid,
            2,
            EP::MemberJoined {
                member: id.public_key(),
                invite: invite.hash(),
            },
        )
        .await;
        CParty { id, dev, chain }
    }

    /// Build a REAL two-member channel at generation 0: owner authorized +
    /// channel created + alice/bob authorized and joined, then the owner
    /// bootstraps and adds both members, who join and confirm. This is exactly
    /// the fold-replay shape of `farder-mls/tests/fold_chain.rs`, driven through
    /// the transport so every event is both folded AND recorded for later fetches.
    async fn setup_folded(channel_id: u64, dir: &Path) -> Folded {
        let owner_id = Keypair::generate();
        let owner_dev = Keypair::generate();
        let g = Genesis {
            version: 1,
            name: "t".to_string(),
            owner: owner_id.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        };
        let sid = g.server_id();
        let t = FoldTransport::new(LogState::from_genesis(&g));

        let owner_store = mk_store(dir, &format!("{channel_id}.owner.sqlite"));
        let owner_store_hash = owner_store.store_instance_hash();
        let mut owner = FParty {
            id: owner_id,
            dev: owner_dev,
            store: owner_store,
            store_hash: owner_store_hash,
            chain: ChainState::default(),
        };

        submit(
            &t,
            &mut owner.chain,
            &owner.dev,
            &owner.id,
            &sid,
            1,
            EP::DeviceAuthorized {
                cert: DeviceCert::create(&owner.id, &owner.dev.public_key(), 1),
            },
        )
        .await;
        let invite = submit(
            &t,
            &mut owner.chain,
            &owner.dev,
            &owner.id,
            &sid,
            100,
            EP::InviteCreated {
                code_hash: "c".to_string(),
                max_uses: 10,
                expires_at: 9999,
                requires_approval: false,
            },
        )
        .await;

        let alice_only = join_member_only(&t, &sid, &invite).await;
        let bob_only = join_member_only(&t, &sid, &invite).await;
        let mut alice = FParty {
            id: alice_only.id,
            dev: alice_only.dev,
            store: mk_store(dir, &format!("{channel_id}.alice.sqlite")),
            store_hash: [0u8; 32],
            chain: alice_only.chain,
        };
        alice.store_hash = alice.store.store_instance_hash();
        let mut bob = FParty {
            id: bob_only.id,
            dev: bob_only.dev,
            store: mk_store(dir, &format!("{channel_id}.bob.sqlite")),
            store_hash: [0u8; 32],
            chain: bob_only.chain,
        };
        bob.store_hash = bob.store.store_instance_hash();

        submit(
            &t,
            &mut owner.chain,
            &owner.dev,
            &owner.id,
            &sid,
            10,
            EP::ChannelCreated {
                channel_id,
                name: "sealed".to_string(),
                kind: "text".to_string(),
                class: ChannelClass::E2ee,
                parent: None,
            },
        )
        .await;

        // The real generation-0 group + bootstrap commit.
        let mut owner_group = MlsChannelGroup::create(
            &owner.store,
            &DeviceSigner(&owner.dev),
            credential_with_key(&owner.dev, &owner.id.public_key()),
            channel_group_id(&sid, channel_id, 0).as_bytes(),
        )
        .unwrap();
        let out0 = owner_group.self_update(&owner.store, &DeviceSigner(&owner.dev)).unwrap();
        let auth0 = owner_group.epoch_authenticator();
        submit(
            &t,
            &mut owner.chain,
            &owner.dev,
            &owner.id,
            &sid,
            TS,
            mls_commit(channel_id, 0, &out0, auth0, vec![], owner.store_hash),
        )
        .await;

        // Add alice (commit + welcome + join + confirm).
        let (alice_kp_ref, alice_kp) = publish_kp(&t, &mut alice, &sid, &owner.store).await;
        let out1 = owner_group
            .add_members(&owner.store, &DeviceSigner(&owner.dev), &[alice_kp])
            .unwrap();
        let auth1 = owner_group.epoch_authenticator();
        let welcome1 = out1.welcome_bytes.clone().unwrap();
        let commit1 = submit(
            &t,
            &mut owner.chain,
            &owner.dev,
            &owner.id,
            &sid,
            TS,
            mls_commit(
                channel_id,
                0,
                &out1,
                auth1,
                vec![DeclaredAdd {
                    identity: alice.id.public_key(),
                    device: device_id(&alice.dev.public_key()),
                    key_package: alice_kp_ref,
                }],
                owner.store_hash,
            ),
        )
        .await;
        submit(
            &t,
            &mut owner.chain,
            &owner.dev,
            &owner.id,
            &sid,
            TS,
            EP::MlsWelcome {
                channel_id,
                generation: 0,
                commit: commit1.hash(),
                for_member: alice.id.public_key(),
                for_device: device_id(&alice.dev.public_key()),
                welcome: welcome1.clone(),
            },
        )
        .await;
        let (_alice_group, alice_join) =
            MlsChannelGroup::join_from_welcome(&alice.store, &welcome1).unwrap();
        submit(
            &t,
            &mut alice.chain,
            &alice.dev,
            &alice.id,
            &sid,
            TS,
            EP::MlsLeafConfirmed {
                channel_id,
                generation: 0,
                epoch: alice_join.epoch,
                tree_hash: alice_join.tree_hash,
                store_instance_hash: alice.store_hash,
            },
        )
        .await;

        // Add bob (commit + welcome + join + confirm).
        let (bob_kp_ref, bob_kp) = publish_kp(&t, &mut bob, &sid, &owner.store).await;
        let out2 = owner_group
            .add_members(&owner.store, &DeviceSigner(&owner.dev), &[bob_kp])
            .unwrap();
        let auth2 = owner_group.epoch_authenticator();
        let welcome2 = out2.welcome_bytes.clone().unwrap();
        let commit2 = submit(
            &t,
            &mut owner.chain,
            &owner.dev,
            &owner.id,
            &sid,
            TS,
            mls_commit(
                channel_id,
                0,
                &out2,
                auth2,
                vec![DeclaredAdd {
                    identity: bob.id.public_key(),
                    device: device_id(&bob.dev.public_key()),
                    key_package: bob_kp_ref,
                }],
                owner.store_hash,
            ),
        )
        .await;
        submit(
            &t,
            &mut owner.chain,
            &owner.dev,
            &owner.id,
            &sid,
            TS,
            EP::MlsWelcome {
                channel_id,
                generation: 0,
                commit: commit2.hash(),
                for_member: bob.id.public_key(),
                for_device: device_id(&bob.dev.public_key()),
                welcome: welcome2.clone(),
            },
        )
        .await;
        let (_bob_group, bob_join) =
            MlsChannelGroup::join_from_welcome(&bob.store, &welcome2).unwrap();
        submit(
            &t,
            &mut bob.chain,
            &bob.dev,
            &bob.id,
            &sid,
            TS,
            EP::MlsLeafConfirmed {
                channel_id,
                generation: 0,
                epoch: bob_join.epoch,
                tree_hash: bob_join.tree_hash,
                store_instance_hash: bob.store_hash,
            },
        )
        .await;

        let key = ChannelKey::new(sid.clone(), channel_id).unwrap();
        Folded {
            t,
            sid,
            key,
            owner,
            alice,
            bob,
        }
    }

    struct Folded {
        t: FoldTransport,
        sid: String,
        key: ChannelKey,
        owner: FParty,
        alice: FParty,
        bob: FParty,
    }

    /// A reset-ready fold WITHOUT the generation-0 MLS group: owner authorized,
    /// channel created, two members joined. Enough for the fold's exact-cover
    /// and confirmation-wall checks, which never inspect the MLS bytes.
    async fn setup_members_only(channel_id: u64) -> (FoldTransport, String, CParty, CParty, CParty) {
        let owner_id = Keypair::generate();
        let owner_dev = Keypair::generate();
        let g = Genesis {
            version: 1,
            name: "t".to_string(),
            owner: owner_id.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        };
        let sid = g.server_id();
        let t = FoldTransport::new(LogState::from_genesis(&g));

        let mut owner = CParty {
            id: owner_id,
            dev: owner_dev,
            chain: ChainState::default(),
        };
        submit(
            &t,
            &mut owner.chain,
            &owner.dev,
            &owner.id,
            &sid,
            1,
            EP::DeviceAuthorized {
                cert: DeviceCert::create(&owner.id, &owner.dev.public_key(), 1),
            },
        )
        .await;
        let invite = submit(
            &t,
            &mut owner.chain,
            &owner.dev,
            &owner.id,
            &sid,
            100,
            EP::InviteCreated {
                code_hash: "c".to_string(),
                max_uses: 10,
                expires_at: 9999,
                requires_approval: false,
            },
        )
        .await;
        let alice = join_member_only(&t, &sid, &invite).await;
        let bob = join_member_only(&t, &sid, &invite).await;
        submit(
            &t,
            &mut owner.chain,
            &owner.dev,
            &owner.id,
            &sid,
            10,
            EP::ChannelCreated {
                channel_id,
                name: "sealed".to_string(),
                kind: "text".to_string(),
                class: ChannelClass::E2ee,
                parent: None,
            },
        )
        .await;
        (t, sid, owner, alice, bob)
    }

    /// Stage a FAKE next-generation Welcome for one member (the fold does not
    /// inspect the Welcome bytes — only `for_member` / `for_device` /
    /// `generation`), and return its event hash.
    async fn stage_fake_welcome(
        t: &FoldTransport,
        chain: &mut ChainState,
        dev: &Keypair,
        id: &Keypair,
        sid: &str,
        channel_id: u64,
        member: &DeclaredMember,
    ) -> String {
        let ev = build_next_event(
            dev,
            id,
            sid,
            chain,
            TS,
            EP::MlsWelcome {
                channel_id,
                generation: 1,
                commit: RESET_WELCOME_COMMIT_SENTINEL.to_string(),
                for_member: member.identity.clone(),
                for_device: member.device.clone(),
                welcome: vec![0xAA; 32],
            },
        );
        let accepted = t
            .submit_event(&ev)
            .await
            .expect("the fold must accept a staged next-generation welcome");
        chain.advance(&ev);
        accepted.event_hash
    }

    async fn member_joins_reset(
        t: &FoldTransport,
        sid: &str,
        key: &ChannelKey,
        member: &mut FParty,
        new_generation: u64,
        post_tree_hash: [u8; 32],
    ) {
        let a = Actor {
            device: &member.dev,
            identity: &member.id,
            log_server_id: sid,
        };
        let welcomes = fetch_pending_welcomes(t, &a, Some(key.channel_id), 0)
            .await
            .unwrap();
        let staged: Vec<_> = welcomes
            .iter()
            .filter(|w| w.generation == new_generation)
            .collect();
        assert_eq!(
            staged.len(),
            1,
            "the member should have exactly one staged Welcome for the new generation"
        );
        let store_hash = member.store_hash;
        let ctx = ResetContext {
            key,
            store: &member.store,
            store_instance_hash: &store_hash,
        };
        join_reset(t, &a, &mut member.chain, &ctx, staged[0], post_tree_hash)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn the_fold_accepts_the_full_reset_sequence() {
        let channel_id = 1 << 55;
        let dir = temp_dir("fold-reset-ok");
        let mut f = setup_folded(channel_id, &dir).await;

        // The owner's generation-0 group is a real 2-member channel before the
        // reset.
        {
            let st = f.t.lock_state();
            assert_eq!(st.mls_current_epoch(channel_id), Some((0, 3)));
            assert_eq!(st.leaves_confirmed(channel_id).len(), 3);
        }

        // Members publish FRESH key packages for the reset (their generation-0
        // ones were consumed by the generation-0 adds).
        publish_kp(&f.t, &mut f.alice, &f.sid, &f.owner.store).await;
        publish_kp(&f.t, &mut f.bob, &f.sid, &f.owner.store).await;

        let members = vec![f.alice.member(), f.bob.member()];
        let owner_actor = Actor {
            device: &f.owner.dev,
            identity: &f.owner.id,
            log_server_id: &f.sid,
        };
        let owner_hash = f.owner.store_hash;
        let ctx = ResetContext {
            key: &f.key,
            store: &f.owner.store,
            store_instance_hash: &owner_hash,
        };
        let outcome = reset_group(&f.t, &owner_actor, &mut f.owner.chain, &ctx, 0, &members)
            .await
            .expect("the reset must be accepted by the fold");

        assert_eq!(outcome.new_generation, 1);

        // Each member fetches their staged Welcome, joins, and confirms.
        member_joins_reset(&f.t, &f.sid, &f.key, &mut f.alice, outcome.new_generation, outcome.post_tree_hash).await;
        member_joins_reset(&f.t, &f.sid, &f.key, &mut f.bob, outcome.new_generation, outcome.post_tree_hash).await;

        // The fold converged on generation 1, epoch 1, with all three leaves
        // confirmed and no outstanding obligations.
        {
            let st = f.t.lock_state();
            assert_eq!(st.mls_current_epoch(channel_id), Some((1, 1)));
            let confirmed = st.leaves_confirmed(channel_id);
            assert_eq!(confirmed.len(), 3);
            assert!(confirmed.contains(&(f.owner.id.public_key(), device_id(&f.owner.dev.public_key()))));
            assert!(confirmed.contains(&(f.alice.id.public_key(), device_id(&f.alice.dev.public_key()))));
            assert!(confirmed.contains(&(f.bob.id.public_key(), device_id(&f.bob.dev.public_key()))));
            assert!(st.pending_confirmations(channel_id).is_empty());
        }
    }

    #[tokio::test]
    async fn the_fold_refuses_a_partial_reset_with_the_exact_cover_error() {
        let channel_id = 1 << 56;
        let (t, sid, mut owner, alice, bob) = setup_members_only(channel_id).await;

        // Stage BOTH members' welcomes, then cite only alice's in the reset.
        let alice_ref = stage_fake_welcome(&t, &mut owner.chain, &owner.dev, &owner.id, &sid, channel_id, &alice.member()).await;
        let _bob_ref = stage_fake_welcome(&t, &mut owner.chain, &owner.dev, &owner.id, &sid, channel_id, &bob.member()).await;

        let reset = build_next_event(
            &owner.dev,
            &owner.id,
            &sid,
            &owner.chain,
            TS,
            EP::MlsGroupReset {
                channel_id,
                new_generation: 1,
                welcomes: vec![alice_ref],
                post_tree_hash: [0x11u8; 32],
            },
        );

        let err = {
            let mut st = t.lock_state();
            st.apply(&reset).expect_err("a partial reset must be refused")
        };
        assert!(
            err.to_string().contains("non-selective reset"),
            "unexpected rejection: {err}"
        );
    }

    #[tokio::test]
    async fn the_fold_refuses_a_wrong_tree_hash_confirmation() {
        let channel_id = 1 << 57;
        let (t, sid, mut owner, alice, bob) = setup_members_only(channel_id).await;

        let alice_ref = stage_fake_welcome(&t, &mut owner.chain, &owner.dev, &owner.id, &sid, channel_id, &alice.member()).await;
        let bob_ref = stage_fake_welcome(&t, &mut owner.chain, &owner.dev, &owner.id, &sid, channel_id, &bob.member()).await;

        let post_tree_hash = [0x22u8; 32];
        let reset = build_next_event(
            &owner.dev,
            &owner.id,
            &sid,
            &owner.chain,
            TS,
            EP::MlsGroupReset {
                channel_id,
                new_generation: 1,
                welcomes: vec![alice_ref, bob_ref],
                post_tree_hash,
            },
        );
        let accepted = t
            .submit_event(&reset)
            .await
            .expect("the complete reset must be accepted");
        let mut owner_chain = owner.chain;
        owner_chain.advance(&reset);
        let _ = accepted;

        // Alice confirms with the WRONG tree hash: the confirmation wall must
        // refuse it (the reset generation's add-commit is never a log event, so
        // the anchor is the reset's declared post_tree_hash).
        let wrong = build_next_event(
            &alice.dev,
            &alice.id,
            &sid,
            &alice.chain,
            TS,
            EP::MlsLeafConfirmed {
                channel_id,
                generation: 1,
                epoch: 1,
                tree_hash: [0xFFu8; 32],
                store_instance_hash: [0u8; 32],
            },
        );
        let err = {
            let mut st = t.lock_state();
            st.apply(&wrong).expect_err("a wrong-tree-hash confirmation must be refused")
        };
        assert!(
            err.to_string().contains("does not match the tree hash the reset declared"),
            "unexpected rejection: {err}"
        );
    }
}
