//! Task 4 of the 4a vertical: the steward add path and the two receive-side
//! gates.
//!
//! [`add_member`] is the steward side: fetch a member's KeyPackage from the
//! log, add them to the group, and submit the `MlsCommit` + `MlsWelcome` pair
//! (the Welcome cites the commit's event hash). [`process_incoming_commit`] is
//! the member side: apply someone else's commit **only** after it passes two
//! independent gates — the declared-vs-actual check (Gate 1, via
//! `process_commit_checked`) and the leaf-binding check (Gate 2, via
//! `credential::verify_leaf_binding`) — and **only** in epoch order.
//!
//! # The two gates (the security core)
//!
//! **Gate 1** — [`MlsChannelGroup::process_commit_checked`] and ONLY that.
//! Never [`MlsChannelGroup::process_commit`]. The checked variant stages the
//! commit, compares the staged (pre-merge) commit against the *declared*
//! adds/removes/post-tree-hash carried on the `MlsCommit` event, and merges
//! **only** on match; on any mismatch the staged commit is discarded unmerged
//! and the group stays in its current epoch.
//!
//! **Gate 2** — `process_commit_checked` is not sufficient by itself. It
//! compares *declared* against *actual*, but it cannot tell a genuine leaf
//! from an **impostor leaf that cloned a real member's credential bytes**:
//! such a commit passes Gate 1 (declared identity/device == actual claimed
//! identity/device) while carrying attacker-owned signature keys. So every
//! entry of `ProcessedCommit::actual_adds` must ALSO pass
//! `credential::verify_leaf_binding(credential, leaf_signature_key, cert)`
//! against a [`DeviceCert`] that the [`DeviceCertResolver`] attests is valid in
//! the log for that exact `(identity, device)`. A failure rejects the commit
//! with [`IncomingCommitOutcome::LeafBindingFailure`] — never silently
//! accepted, never merged-and-warned.
//!
//! # Ordering
//!
//! Commits are applied in epoch order. `process_incoming_commit` compares the
//! event's declared `epoch` (the epoch the commit was *authored* in, which
//! `MlsCommit.epoch` carries) against the group's current epoch, and a gap
//! (or a replay) returns [`IncomingCommitOutcome::OutOfOrder`] **without
//! merging**. Nothing is silently reordered or dropped.

use farder_crypto::event_log::{DeclaredAdd, DeclaredRemove, DeviceCert, Event, EventPayload};
use farder_crypto::identity::PublicKey;
use farder_mls::credential::{decode_credential_identity, verify_leaf_binding, DeviceSigner};
use farder_mls::group::{decode_key_package, DeclaredMember, MlsChannelGroup};
use farder_mls::store::FarderMlsStore;

use crate::chain::{build_next_event, event_now_secs, Actor, ChainState};
use crate::channel::E2eeError;
use crate::channel_key::ChannelKey;
use crate::transport::E2eeTransport;

/// Resolves the log-valid [`DeviceCert`] for one `(identity, device)`.
///
/// This is the *fold-status* half of the second receive-side gate. The
/// resolver must return `Some(cert)` ONLY when the log currently holds a live,
/// unrevoked, unexpired `DeviceCert` binding `device` to `identity`; the
/// cryptographic binding (credential ↔ leaf signature key ↔ cert) is checked
/// separately by `credential::verify_leaf_binding`. The caller owns the log
/// state (Task 9's fetch surface will have the certs at hand); this trait just
/// names the trust anchor `process_incoming_commit` needs.
pub trait DeviceCertResolver {
    fn device_cert(&self, identity: &PublicKey, device: &str) -> Option<DeviceCert>;
}

/// The declared fields of an `MlsCommit` event, as received from the log:
/// the epoch the commit was authored in (what `MlsCommit.epoch` carries), the
/// declared adds/removes, and the declared post-tree-hash.
/// [`process_incoming_commit`] checks the staged commit against the adds /
/// removes / post-tree-hash (Gate 1), the actually-added leaves against the
/// log-valid `DeviceCert`s (Gate 2), and applies only in `epoch` order.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredCommit {
    pub epoch: u64,
    pub adds: Vec<DeclaredAdd>,
    pub removes: Vec<DeclaredRemove>,
    pub post_tree_hash: [u8; 32],
}

/// The typed outcome of [`process_incoming_commit`].
///
/// The security-relevant rejections are values here, not errors, so the caller
/// must consciously match them rather than `?`-ing past them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncomingCommitOutcome {
    /// Both gates passed and the commit was merged; the group is now at
    /// `epoch` with `post_tree_hash`.
    Applied {
        /// The group's new (post-merge) epoch.
        epoch: u64,
        /// The group's new tree hash (== the commit's declared post tree hash,
        /// which Gate 1 checked).
        post_tree_hash: [u8; 32],
    },
    /// The commit's declared `epoch` does not match the group's current epoch:
    /// a gap ahead (missing commits) or a replay behind (already seen). The
    /// commit was **not** merged — nothing is reordered or skipped.
    OutOfOrder {
        current_epoch: u64,
        received_epoch: u64,
    },
    /// Gate 2 failure: an added leaf failed leaf-binding against the log-valid
    /// `DeviceCert` for its claimed `(identity, device)`. Equivocation-class:
    /// the commit matched its own declaration (Gate 1 passed) yet carries an
    /// impostor leaf with cloned credential bytes over attacker-owned keys.
    ///
    /// Because Gate 1 already merged the commit, the local group now contains
    /// the impostor leaf; the caller must treat the group as poisoned
    /// (resync/abort — Task 6), never continue using it.
    LeafBindingFailure {
        /// The `(identity, device)` the impostor leaf *claimed*.
        member: DeclaredMember,
        /// Why the binding failed (no log-valid cert, or a mismatch).
        reason: String,
    },
}

/// The fixed "where am I committing" inputs for a steward commit
/// ([`add_member`]): the channel, its generation, and the MLS store plus its
/// instance hash (which always travel together). Bundled so [`add_member`]
/// stays under the 7-argument clippy bound.
pub struct StewardContext<'a> {
    pub key: &'a ChannelKey,
    pub generation: u64,
    pub store: &'a FarderMlsStore,
    pub store_instance_hash: &'a [u8; 32],
}

/// The result of [`add_member`]: the steward's own commit and the Welcome that
/// followed it.
#[derive(Debug)]
pub struct AddMemberOutcome {
    /// Server-assigned hash of the accepted `MlsCommit` event.
    pub commit_event_hash: String,
    /// Server-assigned hash of the accepted `MlsWelcome` event.
    pub welcome_event_hash: String,
    /// The epoch the LOCAL group is in after merging the add (one past the
    /// authored epoch). See the divergence contract on [`E2eeError`].
    pub local_epoch: u64,
    /// The group's epoch authenticator after the add (read immediately after
    /// the merge — it is NOT a field on `CommitOutcome`, finding F1).
    pub post_epoch_authenticator: [u8; 32],
    pub post_tree_hash: [u8; 32],
}

/// Add one member `(identity, device)` to the group: fetch their KeyPackage
/// from the log, decode it strictly (failing closed on a non-farder
/// credential), add them, then submit the `MlsCommit` (carrying the declared
/// add and the real chaining values) followed by the `MlsWelcome` that cites
/// the commit's event hash.
///
/// # Divergence contract
///
/// `MlsChannelGroup::add_members` merges **locally and immediately**, so if the
/// `MlsCommit` submit is rejected with the bare `"stale-epoch"`, this returns
/// [`E2eeError::StaleEpochDiverged`] and the local group is one epoch ahead of
/// the server — the caller must resync from the log, never keep using the
/// group. This is never silently swallowed and never reported as success.
pub async fn add_member<T: E2eeTransport + Sync>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    ctx: &StewardContext<'_>,
    group: &mut MlsChannelGroup,
    member: &DeclaredMember,
) -> Result<AddMemberOutcome, E2eeError> {
    // 1. Fetch the member's published KeyPackage (the transport returns the
    //    raw signed `MlsKeyPackagePublished` event bytes, oldest-first) and
    //    decode the newest one strictly. `decode_key_package` fails closed on
    //    a non-farder credential, and we additionally refuse a package whose
    //    credential does not claim exactly the member we are adding (so the
    //    steward can never emit a commit whose declared add diverges from what
    //    the key package actually claims — Gate 1 for our own commits).
    let kp_event_bytes = transport
        .fetch_key_packages(&member.identity, &member.device)
        .await?
        .into_iter()
        .last()
        .ok_or_else(|| E2eeError::chain("member has no published key packages"))?;
    let kp_event = Event::from_bytes(&kp_event_bytes)
        .map_err(|e| E2eeError::Mls(anyhow::anyhow!("decode key package event bytes: {e}")))?;
    let EventPayload::MlsKeyPackagePublished { key_package, .. } = &kp_event.core.payload else {
        return Err(E2eeError::chain(
            "fetch_key_packages returned a non-MlsKeyPackagePublished event",
        ));
    };
    let key_package = decode_key_package(ctx.store, key_package).map_err(|e| {
        E2eeError::Mls(e.context("decode fetched key package (non-farder credential fails closed)"))
    })?;
    let (kp_identity, kp_device) = decode_credential_identity(
        key_package.leaf_node().credential().serialized_content(),
    )
    .map_err(|e| E2eeError::Mls(e.context("decode fetched key package credential")))?;
    if kp_identity != member.identity || kp_device != member.device {
        return Err(E2eeError::chain(
            "fetched key package credential does not match the member being added",
        ));
    }
    let key_package_event_hash = kp_event.hash();

    // 2. Add locally (merges immediately).
    let outcome = group
        .add_members(ctx.store, &DeviceSigner(actor.device), &[key_package])
        .map_err(|e| E2eeError::Mls(e.context("add member")))?;
    debug_assert_eq!(outcome.adds.as_slice(), std::slice::from_ref(member));
    let welcome_bytes = outcome.welcome_bytes.clone().ok_or_else(|| {
        E2eeError::Mls(anyhow::anyhow!("add_members produced no welcome for a single add"))
    })?;
    let post_epoch_authenticator = group.epoch_authenticator();
    debug_assert_eq!(group.epoch(), outcome.epoch + 1);

    // 3. Build the MlsCommit from the real CommitOutcome. The declared add
    //    cites the key-package event we consumed; removes are the actual
    //    removes (empty for a single add).
    let authz_head = chain.last_event_hash.clone().ok_or_else(|| {
        E2eeError::chain("add_member needs a prior event to attest its folded head")
    })?;
    let adds = vec![DeclaredAdd {
        identity: member.identity.clone(),
        device: member.device.clone(),
        key_package: key_package_event_hash,
    }];
    let removes: Vec<DeclaredRemove> = outcome
        .removes
        .iter()
        .map(|m| DeclaredRemove {
            identity: m.identity.clone(),
            device: m.device.clone(),
        })
        .collect();
    let commit_event = build_next_event(
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
            adds,
            removes,
            prev_epoch_authenticator: outcome.prev_epoch_authenticator,
            post_epoch_authenticator,
            post_tree_hash: outcome.post_tree_hash,
            authz_head,
            store_instance_hash: *ctx.store_instance_hash,
        },
    );

    // 4. Submit the commit; a stale-epoch rejection is the divergence error.
    let accepted = match transport.submit_event(&commit_event).await {
        Ok(a) => a,
        Err(e) if e.is_stale_epoch() => {
            return Err(E2eeError::StaleEpochDiverged {
                local_epoch: group.epoch(),
            });
        }
        Err(e) => return Err(e.into()),
    };
    chain.advance(&commit_event);

    // 5. Submit the Welcome citing the accepted commit's event hash. The fold
    //    rejects a Welcome that cites an unrecorded commit, so this MUST come
    //    after a successful commit submit and MUST cite `accepted.event_hash`.
    let welcome_event = build_next_event(
        actor.device,
        actor.identity,
        &ctx.key.log_server_id,
        chain,
        event_now_secs(),
        EventPayload::MlsWelcome {
            channel_id: ctx.key.channel_id,
            generation: ctx.generation,
            commit: accepted.event_hash.clone(),
            for_member: member.identity.clone(),
            for_device: member.device.clone(),
            welcome: welcome_bytes,
        },
    );
    let welcome_accepted = transport.submit_event(&welcome_event).await?;
    chain.advance(&welcome_event);

    Ok(AddMemberOutcome {
        commit_event_hash: accepted.event_hash,
        welcome_event_hash: welcome_accepted.event_hash,
        local_epoch: group.epoch(),
        post_epoch_authenticator,
        post_tree_hash: outcome.post_tree_hash,
    })
}

/// Process someone else's commit, delivery-agnostic: it takes the raw commit
/// bytes plus the declared fields carried on the `MlsCommit` event, resolves
/// `DeviceCert`s via [`DeviceCertResolver`], and returns a typed
/// [`IncomingCommitOutcome`]. It does NOT touch the transport (finding F3:
/// there is currently no surface that fetches an `MlsCommit`; Task 9 adds one).
///
/// Order: (0) the declared `epoch` must equal the group's current epoch, else
/// [`IncomingCommitOutcome::OutOfOrder`]; (1) Gate 1 via
/// [`MlsChannelGroup::process_commit_checked`] only; (2) Gate 2 via
/// `credential::verify_leaf_binding` on every actually-added leaf.
pub fn process_incoming_commit(
    store: &FarderMlsStore,
    group: &mut MlsChannelGroup,
    commit_bytes: &[u8],
    declared: &DeclaredCommit,
    certs: &impl DeviceCertResolver,
) -> Result<IncomingCommitOutcome, E2eeError> {
    // Ordering gate: the next commit for a group at epoch N is one authored at
    // epoch N (it moves the group to N+1). Anything else is a gap or a replay
    // and must block, not skip.
    let current_epoch = group.epoch();
    if declared.epoch != current_epoch {
        return Ok(IncomingCommitOutcome::OutOfOrder {
            current_epoch,
            received_epoch: declared.epoch,
        });
    }

    let declared_members: Vec<DeclaredMember> = declared
        .adds
        .iter()
        .map(|a| DeclaredMember {
            identity: a.identity.clone(),
            device: a.device.clone(),
        })
        .collect();
    let declared_remove_members: Vec<DeclaredMember> = declared
        .removes
        .iter()
        .map(|r| DeclaredMember {
            identity: r.identity.clone(),
            device: r.device.clone(),
        })
        .collect();

    // Gate 1: process_commit_checked ONLY. On a declared-vs-actual mismatch the
    // staged commit is discarded unmerged and the group stays put; the error is
    // surfaced, never swallowed.
    let processed = group
        .process_commit_checked(
            store,
            commit_bytes,
            &declared_members,
            &declared_remove_members,
            &declared.post_tree_hash,
        )
        .map_err(|e| {
            E2eeError::Mls(e.context(
                "process incoming commit (Gate 1: declared metadata did not match the staged commit)",
            ))
        })?;

    // Gate 2: every actually-added leaf must bind to a log-valid DeviceCert for
    // the exact (identity, device) its credential claims. A cloned-credential
    // impostor passes Gate 1 and is caught here.
    for leaf in &processed.actual_adds {
        let Some(cert) = certs.device_cert(&leaf.member.identity, &leaf.member.device) else {
            return Ok(IncomingCommitOutcome::LeafBindingFailure {
                member: leaf.member.clone(),
                reason: format!(
                    "no log-valid device cert for {} / {}",
                    leaf.member.identity, leaf.member.device
                ),
            });
        };
        if let Err(e) = verify_leaf_binding(&leaf.credential, &leaf.signature_key, &cert) {
            return Ok(IncomingCommitOutcome::LeafBindingFailure {
                member: leaf.member.clone(),
                reason: format!("leaf binding failed: {e}"),
            });
        }
    }

    Ok(IncomingCommitOutcome::Applied {
        epoch: group.epoch(),
        post_tree_hash: processed.post_tree_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::event_log::{device_id, EventPayload as EP};
    use farder_crypto::identity::Keypair;
    use farder_mls::credential::{credential_with_key, generate_key_package};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tls_codec::Serialize as TlsSerialize;

    use crate::channel::{bootstrap_group, channel_group_id, create_e2ee_channel, ChannelSpec};
    use crate::testing::FakeTransport;

    const SERVER_ID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let n = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "farder-e2ee-client-commit-{name}-{}-{n}",
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

    fn declared(identity: &Keypair, device: &Keypair) -> DeclaredMember {
        DeclaredMember {
            identity: identity.public_key(),
            device: device_id(&device.public_key()),
        }
    }

    /// A log-valid-cert resolver backed by an in-memory map, for the tests.
    struct MapCertResolver(HashMap<(PublicKey, String), DeviceCert>);

    impl DeviceCertResolver for MapCertResolver {
        fn device_cert(&self, identity: &PublicKey, device: &str) -> Option<DeviceCert> {
            self.0.get(&(identity.clone(), device.to_string())).cloned()
        }
    }

    /// A two-member group on disk: alice creates the group and adds bob (the
    /// honest add), bob joins. Both end at epoch 1.
    struct TwoMember {
        alice_id: Keypair,
        alice_dev: Keypair,
        alice_store: FarderMlsStore,
        alice_group: MlsChannelGroup,
        bob_id: Keypair,
        bob_dev: Keypair,
        bob_store: FarderMlsStore,
        bob_group: MlsChannelGroup,
    }

    fn two_member(channel_id: u64) -> TwoMember {
        let dir = temp_dir("two-member");
        let k = key(channel_id);

        let alice_id = Keypair::generate();
        let alice_dev = Keypair::generate();
        let bob_id = Keypair::generate();
        let bob_dev = Keypair::generate();

        let alice_store_path = {
            let mut p = k.mls_store_path(&dir).unwrap();
            p.set_file_name(format!("{}.alice.sqlite", channel_id));
            p
        };
        std::fs::create_dir_all(alice_store_path.parent().unwrap()).unwrap();
        let (alice_store, _) = FarderMlsStore::create(&alice_store_path).unwrap();
        let mut alice_group = MlsChannelGroup::create(
            &alice_store,
            &DeviceSigner(&alice_dev),
            credential_with_key(&alice_dev, &alice_id.public_key()),
            channel_group_id(SERVER_ID, channel_id, 0).as_bytes(),
        )
        .unwrap();

        let bob_store_path = {
            let mut p = k.mls_store_path(&dir).unwrap();
            p.set_file_name(format!("{}.bob.sqlite", channel_id));
            p
        };
        std::fs::create_dir_all(bob_store_path.parent().unwrap()).unwrap();
        let (bob_store, _) = FarderMlsStore::create(&bob_store_path).unwrap();

        let bob_bundle =
            generate_key_package(&bob_store, &bob_dev, &bob_id.public_key()).unwrap();
        let bob_kp_bytes = bob_bundle.key_package().tls_serialize_detached().unwrap();
        let bob_kp = decode_key_package(&alice_store, &bob_kp_bytes).unwrap();

        let add_outcome = alice_group
            .add_members(&alice_store, &DeviceSigner(&alice_dev), &[bob_kp])
            .unwrap();
        let welcome = add_outcome.welcome_bytes.clone().unwrap();
        let (bob_group, _) = MlsChannelGroup::join_from_welcome(&bob_store, &welcome).unwrap();

        TwoMember {
            alice_id,
            alice_dev,
            alice_store,
            alice_group,
            bob_id,
            bob_dev,
            bob_store,
            bob_group,
        }
    }

    /// A self-minted KeyPackage whose credential claims
    /// `(victim_identity, victim_device)` but whose signature/HPKE keys are
    /// attacker-owned — the cloned-credential impostor (mirrors the
    /// `farder-mls` test `impostor_add_passes_declared_check_but_fails_leaf_binding_on_actual_leaf`).
    fn impostor_key_package(
        provider: &FarderMlsStore,
        attacker_device: &Keypair,
        victim_identity: &Keypair,
        victim_device: &Keypair,
    ) -> openmls::prelude::KeyPackage {
        use openmls::prelude::{BasicCredential, Credential, CredentialWithKey};
        let credential: Credential = BasicCredential::new(
            farder_mls::credential::encode_credential_identity(
                &victim_identity.public_key(),
                &device_id(&victim_device.public_key()),
            ),
        )
        .into();
        let credential_with_key = CredentialWithKey {
            credential,
            signature_key: attacker_device.public_key().as_bytes().to_vec().into(),
        };
        openmls::prelude::KeyPackage::builder()
            .build(
                farder_mls::CIPHERSUITE,
                provider,
                &DeviceSigner(attacker_device),
                credential_with_key,
            )
            .unwrap()
            .key_package()
            .clone()
    }

    fn submitted_payloads(transport: &FakeTransport) -> Vec<EP> {
        transport
            .submitted()
            .into_iter()
            .map(|e| e.core.payload)
            .collect()
    }

    // ---- add_member (steward) tests ----

    /// Build a stewarded channel: alice creates the channel + bootstraps, so
    /// she is at epoch 1 with a confirmed leaf, ready to add a member.
    async fn stewarded_channel(
        transport: &FakeTransport,
        channel_id: u64,
    ) -> (
        Keypair,
        Keypair,
        ChannelKey,
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
        let created =
            create_e2ee_channel(transport, &a, &mut chain, &spec, &dir)
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

    #[tokio::test]
    async fn add_member_submits_a_commit_then_a_welcome_citing_the_commit_hash() {
        let transport = FakeTransport::new();
        let (alice_id, alice_dev, k, mut group, store, hash, mut chain) =
            stewarded_channel(&transport, 1 << 50).await;
        let a = actor(&alice_id, &alice_dev);

        let bob_id = Keypair::generate();
        let bob_dev = Keypair::generate();
        let bob_bundle =
            generate_key_package(&store, &bob_dev, &bob_id.public_key()).unwrap();
        let bob_kp_bytes = bob_bundle.key_package().tls_serialize_detached().unwrap();
        let kp_event = build_next_event(
            &bob_dev,
            &bob_id,
            SERVER_ID,
            &ChainState::default(),
            event_now_secs(),
            EP::MlsKeyPackagePublished {
                key_package: bob_kp_bytes,
                store_instance_hash: [0u8; 32],
                expires_at_log_pos: u64::MAX,
            },
        );
        transport.serve_key_packages(
            &bob_id.public_key(),
            &device_id(&bob_dev.public_key()),
            vec![kp_event.to_bytes()],
        );

        let member = declared(&bob_id, &bob_dev);
        let before = transport.submit_count();
        let ctx = StewardContext {
            key: &k,
            generation: 0,
            store: &store,
            store_instance_hash: &hash,
        };
        let outcome = add_member(&transport, &a, &mut chain, &ctx, &mut group, &member)
            .await
            .unwrap();

        assert_eq!(outcome.local_epoch, 2);
        // The commit advanced the group and added bob.
        assert_eq!(group.epoch(), 2);
        assert!(group
            .members()
            .unwrap()
            .contains(&member));

        let payloads = submitted_payloads(&transport);
        assert_eq!(payloads.len(), before + 2, "one commit + one welcome");

        // First of the pair: MlsCommit declaring the add + the key-package ref.
        let commit_hash = match &payloads[before] {
            EP::MlsCommit {
                channel_id,
                generation,
                epoch,
                adds,
                removes,
                post_epoch_authenticator,
                post_tree_hash,
                ..
            } => {
                assert_eq!(*channel_id, k.channel_id);
                assert_eq!(*generation, 0);
                assert_eq!(*epoch, 1, "the add is authored in epoch 1");
                assert!(removes.is_empty());
                assert_eq!(adds.len(), 1);
                assert_eq!(adds[0].identity, bob_id.public_key());
                assert_eq!(adds[0].device, device_id(&bob_dev.public_key()));
                assert_eq!(adds[0].key_package, kp_event.hash());
                assert_eq!(post_epoch_authenticator, &outcome.post_epoch_authenticator);
                assert_eq!(post_tree_hash, &outcome.post_tree_hash);
                transport.submitted()[before].hash()
            }
            other => panic!("expected MlsCommit, got {other:?}"),
        };

        // Second of the pair: MlsWelcome citing that exact commit hash.
        match &payloads[before + 1] {
            EP::MlsWelcome {
                channel_id,
                generation,
                commit,
                for_member,
                for_device,
                welcome,
            } => {
                assert_eq!(*channel_id, k.channel_id);
                assert_eq!(*generation, 0);
                assert_eq!(commit, &commit_hash);
                assert_eq!(for_member, &bob_id.public_key());
                assert_eq!(for_device, &device_id(&bob_dev.public_key()));
                assert!(!welcome.is_empty());
            }
            other => panic!("expected MlsWelcome, got {other:?}"),
        }

        assert_eq!(outcome.commit_event_hash, commit_hash);
    }

    #[tokio::test]
    async fn add_member_stale_commit_surfaces_as_diverged_not_success() {
        let transport = FakeTransport::new();
        let (alice_id, alice_dev, k, mut group, store, hash, mut chain) =
            stewarded_channel(&transport, 1 << 51).await;
        let a = actor(&alice_id, &alice_dev);

        let bob_id = Keypair::generate();
        let bob_dev = Keypair::generate();
        let bob_bundle =
            generate_key_package(&store, &bob_dev, &bob_id.public_key()).unwrap();
        let bob_kp_bytes = bob_bundle.key_package().tls_serialize_detached().unwrap();
        let kp_event = build_next_event(
            &bob_dev,
            &bob_id,
            SERVER_ID,
            &ChainState::default(),
            event_now_secs(),
            EP::MlsKeyPackagePublished {
                key_package: bob_kp_bytes,
                store_instance_hash: [0u8; 32],
                expires_at_log_pos: u64::MAX,
            },
        );
        transport.serve_key_packages(
            &bob_id.public_key(),
            &device_id(&bob_dev.public_key()),
            vec![kp_event.to_bytes()],
        );
        transport.reject_next("stale-epoch");

        let ctx = StewardContext {
            key: &k,
            generation: 0,
            store: &store,
            store_instance_hash: &hash,
        };
        let err = add_member(
            &transport,
            &a,
            &mut chain,
            &ctx,
            &mut group,
            &declared(&bob_id, &bob_dev),
        )
        .await
        .unwrap_err();

        assert!(err.is_stale_epoch_diverged(), "expected divergence, got {err}");
        match err {
            E2eeError::StaleEpochDiverged { local_epoch } => {
                // The add merged locally before the rejection.
                assert_eq!(local_epoch, 2);
            }
            other => panic!("expected StaleEpochDiverged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn add_member_fails_closed_on_a_non_farder_credential() {
        let transport = FakeTransport::new();
        let (alice_id, alice_dev, k, mut group, store, hash, mut chain) =
            stewarded_channel(&transport, 1 << 52).await;
        let a = actor(&alice_id, &alice_dev);

        // A TLS-valid, OpenMLS-valid KeyPackage whose credential is NOT a
        // farder credential — only decode_key_package's gate can refuse it.
        let bob_dev = Keypair::generate();
        let non_farder: openmls::prelude::KeyPackage = {
            use openmls::prelude::{BasicCredential, Credential, CredentialWithKey};
            let credential: Credential = BasicCredential::new(b"not-a-farder-credential".to_vec()).into();
            let cwk = CredentialWithKey {
                credential,
                signature_key: bob_dev.public_key().as_bytes().to_vec().into(),
            };
            openmls::prelude::KeyPackage::builder()
                .build(farder_mls::CIPHERSUITE, &store, &DeviceSigner(&bob_dev), cwk)
                .unwrap()
                .key_package()
                .clone()
        };
        let non_farder_bytes = non_farder.tls_serialize_detached().unwrap();
        let kp_event = build_next_event(
            &bob_dev,
            &alice_id,
            SERVER_ID,
            &ChainState::default(),
            event_now_secs(),
            EP::MlsKeyPackagePublished {
                key_package: non_farder_bytes,
                store_instance_hash: [0u8; 32],
                expires_at_log_pos: u64::MAX,
            },
        );
        transport.serve_key_packages(
            &alice_id.public_key(),
            &device_id(&bob_dev.public_key()),
            vec![kp_event.to_bytes()],
        );

        let before = transport.submit_count();
        let ctx = StewardContext {
            key: &k,
            generation: 0,
            store: &store,
            store_instance_hash: &hash,
        };
        let err = add_member(
            &transport,
            &a,
            &mut chain,
            &ctx,
            &mut group,
            &declared(&alice_id, &bob_dev),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, E2eeError::Mls(_)), "got {err:?}");
        // Nothing was submitted: the credential gate fired before add_members.
        assert_eq!(transport.submit_count(), before);
    }

    #[tokio::test]
    async fn add_member_refuses_a_key_package_that_claims_a_different_member() {
        let transport = FakeTransport::new();
        let (alice_id, alice_dev, k, mut group, store, hash, mut chain) =
            stewarded_channel(&transport, 1 << 53).await;
        let a = actor(&alice_id, &alice_dev);

        // A well-formed farder key package for CHARLIE, but we ask to add BOB:
        // the steward must not emit a commit whose declared add diverges from
        // what the key package actually claims.
        let charlie_id = Keypair::generate();
        let charlie_dev = Keypair::generate();
        let charlie_bundle =
            generate_key_package(&store, &charlie_dev, &charlie_id.public_key()).unwrap();
        let charlie_kp_bytes = charlie_bundle.key_package().tls_serialize_detached().unwrap();
        let kp_event = build_next_event(
            &charlie_dev,
            &charlie_id,
            SERVER_ID,
            &ChainState::default(),
            event_now_secs(),
            EP::MlsKeyPackagePublished {
                key_package: charlie_kp_bytes,
                store_instance_hash: [0u8; 32],
                expires_at_log_pos: u64::MAX,
            },
        );

        let bob_id = Keypair::generate();
        let bob_dev = Keypair::generate();
        transport.serve_key_packages(
            &bob_id.public_key(),
            &device_id(&bob_dev.public_key()),
            vec![kp_event.to_bytes()],
        );

        let before = transport.submit_count();
        let ctx = StewardContext {
            key: &k,
            generation: 0,
            store: &store,
            store_instance_hash: &hash,
        };
        let err = add_member(
            &transport,
            &a,
            &mut chain,
            &ctx,
            &mut group,
            &declared(&bob_id, &bob_dev),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, E2eeError::Chain(_)), "got {err:?}");
        assert_eq!(transport.submit_count(), before);
    }

    // ---- process_incoming_commit (receive-side) tests ----

    #[test]
    fn process_incoming_commit_applies_an_honest_commit() {
        let mut f = two_member(1 << 54);
        let certs = MapCertResolver(HashMap::new());

        let update = f
            .alice_group
            .self_update(&f.alice_store, &DeviceSigner(&f.alice_dev))
            .unwrap();

        let result = process_incoming_commit(
            &f.bob_store,
            &mut f.bob_group,
            &update.commit_bytes,
            &DeclaredCommit {
                epoch: update.epoch,
                adds: vec![],
                removes: vec![],
                post_tree_hash: update.post_tree_hash,
            },
            &certs,
        )
        .unwrap();

        assert_eq!(
            result,
            IncomingCommitOutcome::Applied {
                epoch: 2,
                post_tree_hash: update.post_tree_hash,
            }
        );
        assert_eq!(f.bob_group.epoch(), 2);
        assert_eq!(f.bob_group.tree_hash(), update.post_tree_hash);
        assert_eq!(f.bob_group.epoch_authenticator(), f.alice_group.epoch_authenticator());
    }

    #[test]
    fn process_incoming_commit_rejects_a_lying_commit_and_does_not_merge() {
        // Gate 1: a commit whose declared metadata lies (declares a remove that
        // did not happen) must be rejected WITHOUT merging.
        let mut f = two_member(1 << 55);
        let certs = MapCertResolver(HashMap::new());

        let update = f
            .alice_group
            .self_update(&f.alice_store, &DeviceSigner(&f.alice_dev))
            .unwrap();
        let epoch_before = f.bob_group.epoch();

        let lying_removes = vec![DeclaredRemove {
            identity: f.alice_id.public_key(),
            device: device_id(&f.alice_dev.public_key()),
        }];

        let err = process_incoming_commit(
            &f.bob_store,
            &mut f.bob_group,
            &update.commit_bytes,
            &DeclaredCommit {
                epoch: update.epoch,
                adds: vec![],
                removes: lying_removes,
                post_tree_hash: update.post_tree_hash,
            },
            &certs,
        )
        .unwrap_err();

        assert!(matches!(err, E2eeError::Mls(_)), "got {err:?}");
        // The lying commit was never merged: bob stays put.
        assert_eq!(f.bob_group.epoch(), epoch_before);
    }

    #[test]
    fn process_incoming_commit_rejects_an_impostor_leaf_that_passes_the_declared_check() {
        // THE test of the task. A hostile commit declares member bob but
        // actually adds an impostor leaf carrying bob's credential bytes over
        // attacker-owned keys. It passes Gate 1 (declared == actual claimed
        // identity/device) and must be caught by Gate 2 (leaf binding).
        let mut f = two_member(1 << 56);

        let attacker_dev = Keypair::generate();
        let impostor = impostor_key_package(
            &f.alice_store,
            &attacker_dev,
            &f.bob_id,
            &f.bob_dev,
        );
        let impostor_bytes = impostor.tls_serialize_detached().unwrap();
        let impostor = decode_key_package(&f.alice_store, &impostor_bytes).unwrap();

        let outcome = f
            .alice_group
            .add_members(&f.alice_store, &DeviceSigner(&f.alice_dev), &[impostor])
            .unwrap();

        // The declared add cites bob; the actual leaf claims bob too, so Gate 1
        // passes. Gate 2 resolves bob's real cert and finds the attacker's
        // signature key does not match.
        let mut certs = HashMap::new();
        certs.insert(
            (f.bob_id.public_key(), device_id(&f.bob_dev.public_key())),
            DeviceCert::create(&f.bob_id, &f.bob_dev.public_key(), 1_700_000_000),
        );
        let certs = MapCertResolver(certs);
        let bob_member = declared(&f.bob_id, &f.bob_dev);

        let result = process_incoming_commit(
            &f.bob_store,
            &mut f.bob_group,
            &outcome.commit_bytes,
            &DeclaredCommit {
                epoch: outcome.epoch,
                adds: vec![DeclaredAdd {
                    identity: bob_member.identity.clone(),
                    device: bob_member.device.clone(),
                    key_package: "0f".repeat(32),
                }],
                removes: vec![],
                post_tree_hash: outcome.post_tree_hash,
            },
            &certs,
        )
        .unwrap();

        match result {
            IncomingCommitOutcome::LeafBindingFailure { member, reason } => {
                assert_eq!(member, bob_member);
                assert!(!reason.is_empty());
            }
            other => panic!("expected LeafBindingFailure, got {other:?}"),
        }
    }

    #[test]
    fn process_incoming_commit_blocks_a_gap_without_merging() {
        let mut f = two_member(1 << 58);
        let certs = MapCertResolver(HashMap::new());

        let update = f
            .alice_group
            .self_update(&f.alice_store, &DeviceSigner(&f.alice_dev))
            .unwrap();
        let epoch_before = f.bob_group.epoch();

        // The commit bytes are for epoch 1, but the event declares epoch 7: a
        // gap (epochs 2..6 are missing). It must block, not skip.
        let result = process_incoming_commit(
            &f.bob_store,
            &mut f.bob_group,
            &update.commit_bytes,
            &DeclaredCommit {
                epoch: 7,
                adds: vec![],
                removes: vec![],
                post_tree_hash: update.post_tree_hash,
            },
            &certs,
        )
        .unwrap();

        assert_eq!(
            result,
            IncomingCommitOutcome::OutOfOrder {
                current_epoch: 1,
                received_epoch: 7,
            }
        );
        assert_eq!(f.bob_group.epoch(), epoch_before);
    }

    #[test]
    fn process_incoming_commit_blocks_a_replay_without_merging() {
        let mut f = two_member(1 << 59);
        let certs = MapCertResolver(HashMap::new());

        let update = f
            .alice_group
            .self_update(&f.alice_store, &DeviceSigner(&f.alice_dev))
            .unwrap();
        let epoch_before = f.bob_group.epoch();

        // declared_epoch < current: an already-seen commit must block, not
        // silently drop or reapply.
        let result = process_incoming_commit(
            &f.bob_store,
            &mut f.bob_group,
            &update.commit_bytes,
            &DeclaredCommit {
                epoch: 0,
                adds: vec![],
                removes: vec![],
                post_tree_hash: update.post_tree_hash,
            },
            &certs,
        )
        .unwrap();

        assert_eq!(
            result,
            IncomingCommitOutcome::OutOfOrder {
                current_epoch: 1,
                received_epoch: 0,
            }
        );
        assert_eq!(f.bob_group.epoch(), epoch_before);
    }
}
