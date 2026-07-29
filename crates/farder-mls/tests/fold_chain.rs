//! Real-OpenMLS ↔ fold integration (mesh rung 2, sub-project 2, Task 5).
//!
//! `farder-crypto`'s fold is blind: it never runs MLS, it only chains the
//! values commits DECLARE. These tests prove the fold's chain variables and
//! epoch conventions match values a REAL `MlsChannelGroup` produces — not just
//! each other. Every `MlsCommit` event here carries real `CommitOutcome`
//! bytes/authenticators/tree hashes, every `MlsLeafConfirmed` cites a real
//! `JoinInfo`, and every KeyPackage is the real TLS-serialized one, routed
//! through `decode_key_package` exactly as a server-read log entry would be.
//!
//! Scope note: nothing here touches the server — the fold is pure, and the
//! server never links this crate.

use std::collections::HashSet;

use farder_crypto::event_log::{
    device_id, ChannelClass, DeclaredAdd, DeviceCert, DeviceId, Event, EventPayload as EP, Genesis,
};
use farder_crypto::event_log_state::LogState;
use farder_crypto::identity::{Keypair, PublicKey};
use farder_mls::credential::{credential_with_key, generate_key_package, DeviceSigner};
use farder_mls::group::{decode_key_package, DeclaredMember, MlsChannelGroup};
use openmls::prelude::tls_codec::Serialize as TlsSerialize;
use openmls_rust_crypto::OpenMlsRustCrypto;

/// The E2ee channel every test uses, and the MLS group id derived from it.
const CH: u64 = 7;
const GROUP_ID: &[u8] = b"server-1/channel-7/generation-0";
/// Per-device MLS store-instance hashes (the in-memory test provider has no
/// persisted instance id; the fold only requires per-device stability).
const OWNER_STORE: [u8; 32] = [1u8; 32];
const ALICE_STORE: [u8; 32] = [2u8; 32];
const BOB_STORE: [u8; 32] = [3u8; 32];
/// Every MLS-plane event claims this timestamp (the fold's device-liveness clock).
const TS: u64 = 500;

/// One participant: log identity + device keypairs, its own MLS provider, and
/// the head of its event chain.
struct Party {
    id: Keypair,
    dev: Keypair,
    prov: OpenMlsRustCrypto,
    prev: Event,
}

impl Party {
    fn pk(&self) -> PublicKey {
        self.id.public_key()
    }
    fn did(&self) -> DeviceId {
        device_id(&self.dev.public_key())
    }
    fn signer(&self) -> DeviceSigner<'_> {
        DeviceSigner(&self.dev)
    }
    fn member(&self) -> DeclaredMember {
        DeclaredMember {
            identity: self.pk(),
            device: self.did(),
        }
    }
}

fn genesis_of(owner: &Keypair) -> Genesis {
    Genesis {
        version: 1,
        name: "mls-fold".to_string(),
        owner: owner.public_key(),
        created_at: 1,
        nonce: [0u8; 16],
    }
}

/// Build + sign the next event on `party`'s chain and fold it, updating the
/// chain head. Panics with the fold's own message if the event is rejected.
fn fold(st: &mut LogState, party: &mut Party, sid: &str, ts: u64, payload: EP) -> Event {
    let e = Event::next(
        &party.dev,
        party.pk(),
        sid.to_string(),
        Some(&party.prev),
        party.prev.core.lamport,
        ts,
        payload,
    );
    st.apply(&e).unwrap_or_else(|err| panic!("event rejected by the fold: {err}"));
    party.prev = e.clone();
    e
}

/// A folded server with the owner + two members (each with one authorized
/// device) and one `ChannelCreated { class: E2ee }` for channel `CH`.
fn bootstrap() -> (LogState, String, Party, Party, Party) {
    let owner_id = Keypair::generate();
    let owner_dev = Keypair::generate();
    let g = genesis_of(&owner_id);
    let sid = g.server_id();
    let mut st = LogState::from_genesis(&g);

    let da = Event::next(
        &owner_dev,
        owner_id.public_key(),
        sid.clone(),
        None,
        0,
        1,
        EP::DeviceAuthorized {
            cert: DeviceCert::create(&owner_id, &owner_dev.public_key(), 1),
        },
    );
    st.apply(&da).expect("owner device authorization folds");
    let mut owner = Party {
        id: owner_id,
        dev: owner_dev,
        prov: OpenMlsRustCrypto::default(),
        prev: da,
    };

    let invite = fold(
        &mut st,
        &mut owner,
        &sid,
        100,
        EP::InviteCreated {
            code_hash: "c".to_string(),
            max_uses: 10,
            expires_at: 9999,
            requires_approval: false,
        },
    );

    let join = |st: &mut LogState| {
        let id = Keypair::generate();
        let dev = Keypair::generate();
        let da = Event::next(
            &dev,
            id.public_key(),
            sid.clone(),
            None,
            0,
            1,
            EP::DeviceAuthorized {
                cert: DeviceCert::create(&id, &dev.public_key(), 1),
            },
        );
        st.apply(&da).expect("member device authorization folds");
        let mut p = Party {
            id,
            dev,
            prov: OpenMlsRustCrypto::default(),
            prev: da,
        };
        let member = p.pk();
        fold(
            st,
            &mut p,
            &sid,
            2,
            EP::MemberJoined {
                member,
                invite: invite.hash(),
            },
        );
        p
    };
    let alice = join(&mut st);
    let bob = join(&mut st);

    fold(
        &mut st,
        &mut owner,
        &sid,
        10,
        EP::ChannelCreated {
            channel_id: CH,
            name: "sealed".to_string(),
            kind: "text".to_string(),
            class: ChannelClass::E2ee,
            parent: None,
        },
    );
    (st, sid, owner, alice, bob)
}

/// Generate a real KeyPackage for `party`, publish its real TLS bytes to the
/// log, and return `(event ref, decoded KeyPackage ready for `add_members`)`.
fn publish_key_package(
    st: &mut LogState,
    party: &mut Party,
    sid: &str,
    store: [u8; 32],
    adder_prov: &OpenMlsRustCrypto,
) -> (String, openmls::prelude::KeyPackage) {
    let bundle = generate_key_package(&party.prov, &party.dev, &party.pk())
        .expect("key package generation");
    let bytes = bundle
        .key_package()
        .tls_serialize_detached()
        .expect("serialize key package");
    let e = fold(
        st,
        party,
        sid,
        TS,
        EP::MlsKeyPackagePublished {
            key_package: bytes.clone(),
            store_instance_hash: store,
            expires_at_log_pos: 1_000_000,
        },
    );
    // The adder reads the package back from the log exactly as a client would.
    let kp = decode_key_package(adder_prov, &bytes).expect("log-carried key package decodes");
    (e.hash(), kp)
}

/// The `MlsCommit` payload for a real `CommitOutcome`, with the fold's three
/// chain variables filled from real MLS values: `prev_epoch_authenticator`
/// captured pre-merge by the outcome, `post_epoch_authenticator` read from the
/// group AFTER merging, and the real post-merge tree hash.
fn commit_payload(
    outcome: &farder_mls::group::CommitOutcome,
    post_epoch_authenticator: [u8; 32],
    adds: Vec<DeclaredAdd>,
    prev_epoch_authenticator: [u8; 32],
    store: [u8; 32],
) -> EP {
    EP::MlsCommit {
        channel_id: CH,
        generation: 0,
        epoch: outcome.epoch,
        mls_message: outcome.commit_bytes.clone(),
        adds,
        removes: vec![],
        prev_epoch_authenticator,
        post_epoch_authenticator,
        post_tree_hash: outcome.post_tree_hash,
        authz_head: "0".repeat(64),
        store_instance_hash: store,
    }
}

#[test]
fn real_openmls_commits_chain_through_the_fold() {
    let (mut st, sid, mut owner, mut alice, mut bob) = bootstrap();

    // Key packages: real bytes on the log, decoded by the adder's provider.
    let (alice_kp_ref, alice_kp) =
        publish_key_package(&mut st, &mut alice, &sid, ALICE_STORE, &owner.prov);
    let (bob_kp_ref, bob_kp) = publish_key_package(&mut st, &mut bob, &sid, BOB_STORE, &owner.prov);

    // --- The real group, created by the owner's device. ---
    let mut owner_group = MlsChannelGroup::create(
        &owner.prov,
        &owner.signer(),
        credential_with_key(&owner.dev, &owner.pk()),
        GROUP_ID,
    )
    .expect("create the channel group");
    assert_eq!(owner_group.epoch(), 0, "a fresh group is in epoch 0");
    assert_eq!(
        st.mls_current_epoch(CH),
        Some((0, 0)),
        "the fold's group starts where OpenMLS does"
    );

    // Commit 0 — the generation's bootstrap commit (a self-update): nothing to
    // chain onto, so the fold exempts it and confirms the author's leaf.
    let out0 = owner_group
        .self_update(&owner.prov, &owner.signer())
        .expect("bootstrap self-update commit");
    let auth0 = owner_group.epoch_authenticator();
    fold(
        &mut st,
        &mut owner,
        &sid,
        TS,
        commit_payload(&out0, auth0, vec![], out0.prev_epoch_authenticator, OWNER_STORE),
    );
    assert_eq!(owner_group.epoch(), 1);
    assert_eq!(
        st.mls_current_epoch(CH),
        Some((0, owner_group.epoch())),
        "declared epoch + 1 == the real post-merge epoch"
    );
    assert_eq!(
        st.leaves_confirmed(CH),
        HashSet::from([(owner.pk(), owner.did())]),
        "the bootstrap commit's author IS the tree"
    );

    // Commit 1 — add alice. Its REAL pre-merge authenticator must equal what
    // commit 0 declared: that identity is the fold's whole chain rule.
    let out1 = owner_group
        .add_members(&owner.prov, &owner.signer(), &[alice_kp])
        .expect("add alice");
    assert_eq!(
        out1.prev_epoch_authenticator, auth0,
        "OpenMLS's pre-merge authenticator == the previous commit's declared post value"
    );
    let auth1 = owner_group.epoch_authenticator();
    let welcome1 = out1.welcome_bytes.clone().expect("an add produces a Welcome");
    let commit1 = fold(
        &mut st,
        &mut owner,
        &sid,
        TS,
        commit_payload(
            &out1,
            auth1,
            vec![DeclaredAdd {
                identity: alice.pk(),
                device: alice.did(),
                key_package: alice_kp_ref,
            }],
            out1.prev_epoch_authenticator,
            OWNER_STORE,
        ),
    );

    // Welcome + the joiner's own confirmation, citing a real `JoinInfo`.
    let alice_leaf = (alice.pk(), alice.did());
    fold(
        &mut st,
        &mut owner,
        &sid,
        TS,
        EP::MlsWelcome {
            channel_id: CH,
            generation: 0,
            commit: commit1.hash(),
            for_member: alice.pk(),
            for_device: alice.did(),
            welcome: welcome1.clone(),
        },
    );
    let (mut alice_group, alice_join) =
        MlsChannelGroup::join_from_welcome(&alice.prov, &welcome1).expect("alice joins");
    assert_eq!(
        alice_join.epoch,
        out1.epoch + 1,
        "a joiner starts in the epoch the adding commit CREATED"
    );
    assert_eq!(alice_join.tree_hash, out1.post_tree_hash);
    assert!(
        st.pending_adds(CH, TS).contains(&(bob.pk(), bob.did())),
        "bob is a member with no leaf: outstanding drift"
    );
    fold(
        &mut st,
        &mut alice,
        &sid,
        TS,
        EP::MlsLeafConfirmed {
            channel_id: CH,
            generation: 0,
            epoch: alice_join.epoch,
            tree_hash: alice_join.tree_hash,
            store_instance_hash: ALICE_STORE,
        },
    );
    assert!(
        st.leaves_confirmed(CH).contains(&alice_leaf),
        "a real JoinInfo promotes the leaf to confirmed"
    );

    // Commit 2 — add bob, chaining onto commit 1's declared authenticator.
    let out2 = owner_group
        .add_members(&owner.prov, &owner.signer(), &[bob_kp])
        .expect("add bob");
    assert_eq!(out2.prev_epoch_authenticator, auth1, "the chain continues");
    let auth2 = owner_group.epoch_authenticator();
    let welcome2 = out2.welcome_bytes.clone().expect("an add produces a Welcome");
    let commit2 = fold(
        &mut st,
        &mut owner,
        &sid,
        TS,
        commit_payload(
            &out2,
            auth2,
            vec![DeclaredAdd {
                identity: bob.pk(),
                device: bob.did(),
                key_package: bob_kp_ref,
            }],
            out2.prev_epoch_authenticator,
            OWNER_STORE,
        ),
    );
    // Alice keeps up via the misuse-resistant path, using the DECLARED values
    // the log event carries — real bytes, real declared metadata.
    let processed = alice_group
        .process_commit_checked(
            &alice.prov,
            &out2.commit_bytes,
            &[bob.member()],
            &[],
            &out2.post_tree_hash,
        )
        .expect("alice processes the add-bob commit");
    assert_eq!(processed.epoch, out2.epoch);

    fold(
        &mut st,
        &mut owner,
        &sid,
        TS,
        EP::MlsWelcome {
            channel_id: CH,
            generation: 0,
            commit: commit2.hash(),
            for_member: bob.pk(),
            for_device: bob.did(),
            welcome: welcome2.clone(),
        },
    );
    let (bob_group, bob_join) =
        MlsChannelGroup::join_from_welcome(&bob.prov, &welcome2).expect("bob joins");
    fold(
        &mut st,
        &mut bob,
        &sid,
        TS,
        EP::MlsLeafConfirmed {
            channel_id: CH,
            generation: 0,
            epoch: bob_join.epoch,
            tree_hash: bob_join.tree_hash,
            store_instance_hash: BOB_STORE,
        },
    );

    // --- The fold's view converged on MLS reality. ---
    let real: HashSet<(PublicKey, DeviceId)> = owner_group
        .members()
        .expect("real members")
        .into_iter()
        .map(|m| (m.identity, m.device))
        .collect();
    assert_eq!(real.len(), 3);
    assert_eq!(
        st.leaves_confirmed(CH),
        real,
        "leaves_confirmed converges to the real tree membership"
    );
    assert_eq!(
        st.mls_current_epoch(CH),
        Some((0, owner_group.epoch())),
        "fold epoch == real epoch after the whole chain"
    );
    assert_eq!(owner_group.epoch(), alice_group.epoch());
    assert_eq!(bob_group.epoch(), owner_group.epoch());
    assert!(
        st.pending_adds(CH, TS).is_empty() && st.pending_removals(CH, TS).is_empty(),
        "no drift: the fold's member set and the real tree agree"
    );

    // With zero drift and a fresh epoch, sealed content is now sendable — the
    // whole point of the control plane the fold just validated.
    let epoch = st.mls_current_epoch(CH).unwrap().1;
    fold(
        &mut st,
        &mut alice,
        &sid,
        TS,
        EP::MessagePostedE2ee {
            channel_id: CH,
            generation: 0,
            epoch,
            ciphertext: vec![0xF0; 32],
            reply_to: None,
            attachments: vec![],
            authz_head: "0".repeat(64),
        },
    );
}

#[test]
fn fold_rejects_a_commit_that_does_not_chain_onto_the_declared_authenticator() {
    let (mut st, sid, mut owner, mut alice, _bob) = bootstrap();
    let (alice_kp_ref, alice_kp) =
        publish_key_package(&mut st, &mut alice, &sid, ALICE_STORE, &owner.prov);

    let mut owner_group = MlsChannelGroup::create(
        &owner.prov,
        &owner.signer(),
        credential_with_key(&owner.dev, &owner.pk()),
        GROUP_ID,
    )
    .expect("create the channel group");

    // Commit k: the bootstrap commit, declaring a real post-merge authenticator.
    let out0 = owner_group
        .self_update(&owner.prov, &owner.signer())
        .expect("bootstrap self-update commit");
    let auth0 = owner_group.epoch_authenticator();
    fold(
        &mut st,
        &mut owner,
        &sid,
        TS,
        commit_payload(&out0, auth0, vec![], out0.prev_epoch_authenticator, OWNER_STORE),
    );

    // Commit k+1: a REAL commit (same bytes either way) — only the declared
    // `prev_epoch_authenticator` differs.
    let out1 = owner_group
        .add_members(&owner.prov, &owner.signer(), &[alice_kp])
        .expect("add alice");
    let auth1 = owner_group.epoch_authenticator();
    let adds = vec![DeclaredAdd {
        identity: alice.pk(),
        device: alice.did(),
        key_package: alice_kp_ref,
    }];
    assert_eq!(
        out1.prev_epoch_authenticator, auth0,
        "the honest declaration IS the real pre-merge authenticator"
    );

    let mut tampered = out1.prev_epoch_authenticator;
    tampered[0] ^= 0xFF;
    let bad = Event::next(
        &owner.dev,
        owner.pk(),
        sid.clone(),
        Some(&owner.prev),
        owner.prev.core.lamport,
        TS,
        commit_payload(&out1, auth1, adds.clone(), tampered, OWNER_STORE),
    );
    let err = st
        .clone()
        .apply(&bad)
        .expect_err("a commit that does not chain must be rejected");
    assert!(
        err.to_string().contains("does not chain onto"),
        "unexpected rejection: {err}"
    );

    // The honest declaration of the very same MLS commit is accepted.
    fold(
        &mut st,
        &mut owner,
        &sid,
        TS,
        commit_payload(&out1, auth1, adds, out1.prev_epoch_authenticator, OWNER_STORE),
    );
    assert_eq!(
        st.mls_current_epoch(CH),
        Some((0, owner_group.epoch())),
        "the accepted chain tracks the real group's epoch"
    );
}
