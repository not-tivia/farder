//! Task 5 of the 4a vertical: sealed send and receive.
//!
//! [`send_sealed`] builds a [`MessageEnvelope`] (attachments are sub-6, so the
//! per-file keys/filenames/MIMEs travel as empty vecs), enforces the client-side
//! caps **before** sealing, seals against the group's *current* epoch, and
//! submits `MessagePostedE2ee`. [`receive_sealed`] opens exactly one ciphertext
//! and returns a typed [`SealedOutcome`] — never a plaintext fallback and, by
//! construction, never a retry.
//!
//! # Why a second `open` on the same ciphertext is structurally impossible
//!
//! [`MlsChannelGroup::open_message`] consumes that generation's decryption key
//! **including on failure** (forward secrecy — see the farder-mls gotcha and the
//! 4a plan's fact A2.4). [`receive_sealed`] therefore takes the ciphertext
//! **by value** (`Vec<u8>`, not `&[u8]`) and returns a [`SealedOutcome`] that
//! carries the ciphertext back out in **neither** variant: `Decrypted` carries
//! only the [`MessageEnvelope`], and `Undecryptable` carries only a
//! `reason: String`. Once the bytes have been moved in, they no longer exist at
//! the call site, and the outcome cannot be fed back into the function — so the
//! correct path has no way to express "open the same bytes again". The only way
//! a caller could re-arm a retry is to have kept a separate clone *before* the
//! call; that is a deliberate act, and it is still pointless, because the key
//! is already consumed and the second attempt deterministically returns
//! [`SealedOutcome::Undecryptable`].
//!
//! This module does **not** touch commit processing, so the F4 "poisoned group"
//! contract (`LeafBindingFailure` ⇒ terminal for the group instance) does not
//! arise here; it remains Task 6's (resync/abort) to own.

use farder_crypto::event_log::{EventPayload, EventRef, MAX_E2EE_CIPHERTEXT_BYTES};
use farder_mls::credential::DeviceSigner;
use farder_mls::envelope::{check_preseal_limits, MessageEnvelope};
use farder_mls::group::MlsChannelGroup;
use farder_mls::store::FarderMlsStore;

use crate::chain::{build_next_event, event_now_secs, Actor, ChainState};
use crate::channel::E2eeError;
use crate::channel_key::ChannelKey;
use crate::join::SendEligibility;
use crate::transport::E2eeTransport;

/// The outcome of opening one sealed ciphertext with [`receive_sealed`].
///
/// Neither variant carries the ciphertext back out (see the module doc for why
/// that makes a retry structurally impossible).
#[derive(Debug, PartialEq, Eq)]
pub enum SealedOutcome {
    /// The ciphertext opened and decoded to a valid [`MessageEnvelope`].
    Decrypted(MessageEnvelope),
    /// The ciphertext could not be opened — tampered, wrong group, wrong
    /// epoch, a non-application message, or any other OpenMLS rejection. The
    /// `reason` is a human-readable description.
    ///
    /// There is **no** plaintext fallback here, and the generation's decryption
    /// key is already consumed: a retry on the same bytes is impossible (module
    /// doc), not merely ill-advised.
    Undecryptable { reason: String },
}

/// The fixed inputs for one [`send_sealed`] call: the channel being posted to,
/// its generation, the MLS store to seal against, the message text, and the
/// optional event it replies to. Bundled (like [`crate::commit::StewardContext`])
/// so `send_sealed` stays under the 7-argument clippy bound.
pub struct SealContext<'a> {
    pub key: &'a ChannelKey,
    pub generation: u64,
    pub store: &'a FarderMlsStore,
    pub content: &'a str,
    pub reply_to: Option<EventRef>,
}

/// The result of a successful [`send_sealed`].
#[derive(Debug)]
pub struct SealedSendOutcome {
    /// Server-assigned hash of the accepted `MessagePostedE2ee` event.
    pub event_hash: String,
    /// The epoch the ciphertext was sealed in (the group's current epoch at
    /// seal time — sealing never advances the epoch, only commits do).
    pub epoch: u64,
}

/// Seal `content` as an MLS application message and submit it as
/// `MessagePostedE2ee` citing the group's **current** epoch.
///
/// Order of gates (each returns a typed [`E2eeError`], never a panic, and none
/// of the pre-submit failures round-trips):
/// 1. [`SendEligibility::ensure_can_send`] — a pre-confirmation send is refused
///    locally with [`E2eeError::NotConfirmed`] (fact A2.6).
/// 2. [`check_preseal_limits`] — content ≤ [`farder_mls::MAX_CONTENT_CHARS`] AND
///    encoded envelope ≤ [`farder_mls::MAX_PRESEAL_BYTES`], enforced **before**
///    sealing so an over-cap message fails here as [`E2eeError::SealedOverCap`].
/// 3. `seal_message` — encode → (re-check limits) → pad → encrypt.
/// 4. [`MAX_E2EE_CIPHERTEXT_BYTES`] — a cheap pre-submit check of the server's
///    ciphertext cap (unreachable for any envelope that passed step 2, since
///    the 32 KiB pre-seal cap is strictly tighter than the 40 KiB top bucket
///    plus framing; kept as insurance against a framing-cost regression).
///
/// `authz_head` is this device's own folded chain head, carried opaque (the
/// fold neither reads nor validates it — peers compare it against their own
/// folded history). A rejection is NOT handled here: a sealed send is not a
/// commit and does not merge anything locally, so there is no divergence to
/// unwind, and resync is Task 6's job. Any rejection surfaces as
/// [`E2eeError::Transport`], never swallowed. Since finding F6, the bare
/// `"stale-epoch"` reason — `TransportError::is_stale_epoch` — is emitted by
/// the server for `MessagePostedE2ee` / `MessageEditedE2ee` too (not just
/// `MlsCommit`), so [`crate::resync::send_sealed_resync`] keys on it here.
pub async fn send_sealed<T: E2eeTransport + Sync>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    ctx: &SealContext<'_>,
    group: &mut MlsChannelGroup,
    eligibility: &SendEligibility,
) -> Result<SealedSendOutcome, E2eeError> {
    // 1. Local refusal first: a pre-confirmation send is doomed (the fold
    //    rejects "sealed content author does not hold a confirmed leaf"), so
    //    refuse without a round-trip.
    eligibility.ensure_can_send()?;

    // 2. `authz_head`: this device's own folded chain head, read before any
    //    sealing so a missing head fails without touching the sender ratchet.
    //    It must exist for any confirmed member (it has already published a
    //    KeyPackage and confirmed its leaf).
    let authz_head = chain.last_event_hash.clone().ok_or_else(|| {
        E2eeError::chain("sealed send needs a prior event to attest its folded head")
    })?;

    // 3. Build the envelope. Attachments are sub-6: empty in-band vecs.
    let envelope = MessageEnvelope {
        content: ctx.content.to_string(),
        attachment_keys: vec![],
        filenames: vec![],
        mimes: vec![],
    };

    // 4. Enforce the client-side caps BEFORE sealing, so an over-cap message
    //    fails here as a typed error rather than deep inside seal_message or as
    //    a server bounce. check_preseal_limits covers both MAX_CONTENT_CHARS
    //    and MAX_PRESEAL_BYTES.
    let encoded_len = envelope
        .encode()
        .map_err(|e| E2eeError::Mls(e.context("encode message envelope")))?
        .len();
    check_preseal_limits(&envelope, encoded_len)
        .map_err(|e| E2eeError::SealedOverCap { reason: e.to_string() })?;

    // 5. Seal against the group's CURRENT epoch. Sealing never advances the
    //    epoch (only commits do), so the epoch read here is what we cite.
    let epoch = group.epoch();
    let ciphertext = group
        .seal_message(ctx.store, &DeviceSigner(actor.device), &envelope)
        .map_err(|e| E2eeError::Mls(e.context("seal message")))?;

    // 6. Cheap pre-submit check of the server's ciphertext cap (event_log.rs
    //    MAX_E2EE_CIPHERTEXT_BYTES): refuse with a typed error rather than
    //    letting the server bounce it.
    if ciphertext.len() > MAX_E2EE_CIPHERTEXT_BYTES {
        return Err(E2eeError::SealedOverCap {
            reason: format!(
                "sealed ciphertext is {} bytes, over the {}-byte ciphertext cap",
                ciphertext.len(),
                MAX_E2EE_CIPHERTEXT_BYTES
            ),
        });
    }

    // 7. Build + submit.
    let event = build_next_event(
        actor.device,
        actor.identity,
        &ctx.key.log_server_id,
        chain,
        event_now_secs(),
        EventPayload::MessagePostedE2ee {
            channel_id: ctx.key.channel_id,
            generation: ctx.generation,
            epoch,
            ciphertext,
            reply_to: ctx.reply_to.clone(),
            attachments: vec![],
            authz_head,
        },
    );
    let accepted = transport.submit_event(&event).await?;
    chain.advance(&event);

    Ok(SealedSendOutcome {
        event_hash: accepted.event_hash,
        epoch,
    })
}

/// Open exactly one sealed ciphertext and return a typed [`SealedOutcome`].
///
/// Takes the ciphertext **by value** and returns an outcome that cannot be fed
/// back in, so the same bytes can never be opened twice (module doc). Never a
/// plaintext fallback, never a retry. The AEAD-failure `debug_assert` panic in
/// OpenMLS is already contained to a clean `Err` by `farder-mls`'s
/// `process_message_contained` in both build profiles.
pub fn receive_sealed(
    store: &FarderMlsStore,
    group: &mut MlsChannelGroup,
    ciphertext: Vec<u8>,
) -> SealedOutcome {
    match group.open_message(store, &ciphertext) {
        Ok(envelope) => SealedOutcome::Decrypted(envelope),
        Err(e) => SealedOutcome::Undecryptable {
            reason: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::event_log::EventPayload;
    use farder_crypto::identity::Keypair;
    use farder_mls::credential::{credential_with_key, generate_key_package, DeviceSigner};
    use farder_mls::group::decode_key_package;
    use farder_mls::MAX_CONTENT_CHARS;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tls_codec::Serialize as TlsSerialize;

    use crate::channel::channel_group_id;
    use crate::testing::FakeTransport;

    const SERVER_ID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let n = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "farder-e2ee-client-sealed-{name}-{}-{n}",
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

    /// A two-member group on disk: alice creates the group and adds bob (the
    /// honest add), bob joins. Both end at epoch 1, each on its own store.
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

        let bob_bundle = generate_key_package(&bob_store, &bob_dev, &bob_id.public_key()).unwrap();
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

    /// A mid-life chain state with a non-empty head, so `send_sealed`'s
    /// `authz_head` requirement is satisfiable without driving the full
    /// create/bootstrap/add lifecycle (which Tasks 2/4 already cover).
    fn mid_chain() -> ChainState {
        ChainState {
            next_seq: 5,
            last_event_hash: Some("0f".repeat(32)),
            lamport: 5,
        }
    }

    /// Extract the `(ciphertext, reply_to, attachments, authz_head, epoch,
    /// generation, channel_id)` of the last submitted event, which must be a
    /// `MessagePostedE2ee`.
    #[allow(clippy::type_complexity)]
    fn last_sealed_payload(transport: &FakeTransport) -> (Vec<u8>, Option<String>, Vec<String>, String, u64, u64, u64) {
        let event = transport.submitted().into_iter().last().expect("one submitted event");
        match event.core.payload {
            EventPayload::MessagePostedE2ee {
                channel_id,
                generation,
                epoch,
                ciphertext,
                reply_to,
                attachments,
                authz_head,
            } => {
                assert!(attachments.is_empty(), "attachments are empty in Task 5");
                (
                    ciphertext,
                    reply_to,
                    attachments.into_iter().map(|a| a.content_hash).collect(),
                    authz_head,
                    epoch,
                    generation,
                    channel_id,
                )
            }
            other => panic!("expected MessagePostedE2ee, got {other:?}"),
        }
    }

    fn assert_decrypted(outcome: SealedOutcome, expected_content: &str) {
        match outcome {
            SealedOutcome::Decrypted(env) => assert_eq!(env.content, expected_content),
            SealedOutcome::Undecryptable { reason } => {
                panic!("expected decrypted content, got Undecryptable: {reason}")
            }
        }
    }

    #[tokio::test]
    async fn sealed_send_then_receive_roundtrips_between_two_real_groups() {
        let mut f = two_member(1 << 60);
        let k = key(1 << 60);
        let transport = FakeTransport::new();

        // Alice -> Bob.
        let alice_actor = actor(&f.alice_id, &f.alice_dev);
        let mut alice_chain = mid_chain();
        let ctx = SealContext {
            key: &k,
            generation: 0,
            store: &f.alice_store,
            content: "hello over the sealed channel",
            reply_to: None,
        };
        let sent = send_sealed(
            &transport,
            &alice_actor,
            &mut alice_chain,
            &ctx,
            &mut f.alice_group,
            &SendEligibility::confirmed(),
        )
        .await
        .unwrap();
        assert_eq!(sent.epoch, 1);

        let (ciphertext, reply_to, attachments, _authz_head, epoch, generation, channel_id) =
            last_sealed_payload(&transport);
        assert_eq!(reply_to, None);
        assert!(attachments.is_empty());
        assert_eq!(epoch, 1);
        assert_eq!(generation, 0);
        assert_eq!(channel_id, k.channel_id);

        let opened = receive_sealed(&f.bob_store, &mut f.bob_group, ciphertext);
        assert_decrypted(opened, "hello over the sealed channel");

        // Bob -> Alice (the reply direction).
        let bob_actor = actor(&f.bob_id, &f.bob_dev);
        let mut bob_chain = mid_chain();
        let reply_ctx = SealContext {
            key: &k,
            generation: 0,
            store: &f.bob_store,
            content: "roger that",
            reply_to: None,
        };
        send_sealed(
            &transport,
            &bob_actor,
            &mut bob_chain,
            &reply_ctx,
            &mut f.bob_group,
            &SendEligibility::confirmed(),
        )
        .await
        .unwrap();

        let (reply_ciphertext, _, _, _, reply_epoch, _, _) = last_sealed_payload(&transport);
        assert_eq!(reply_epoch, 1);
        let reply_opened = receive_sealed(&f.alice_store, &mut f.alice_group, reply_ciphertext);
        assert_decrypted(reply_opened, "roger that");
    }

    #[test]
    fn tampered_ciphertext_is_undecryptable_without_panicking() {
        let mut f = two_member(1 << 61);

        let envelope = MessageEnvelope {
            content: "integrity matters".to_string(),
            attachment_keys: vec![],
            filenames: vec![],
            mimes: vec![],
        };
        let sealed = f
            .alice_group
            .seal_message(&f.alice_store, &DeviceSigner(&f.alice_dev), &envelope)
            .unwrap();

        let mut tampered = sealed.clone();
        let mid = tampered.len() - 10;
        tampered[mid] ^= 0x01;

        // Bit-flip inside the ciphertext body: AEAD rejects with a clean
        // Undecryptable (the OpenMLS debug_assert panic is contained by
        // farder-mls in BOTH build profiles). No panic here, no fallback.
        match receive_sealed(&f.bob_store, &mut f.bob_group, tampered) {
            SealedOutcome::Undecryptable { reason } => assert!(!reason.is_empty()),
            SealedOutcome::Decrypted(_) => panic!("tampered ciphertext must not decrypt"),
        }

        // Bob's ratchet is intact: a fresh message still opens. (The tampered
        // attempt consumed that generation's decryption key — forward secrecy,
        // not damage — so the sanity check uses a NEW message, not `sealed`.)
        let follow_up = MessageEnvelope {
            content: "still sealed, still working".to_string(),
            attachment_keys: vec![],
            filenames: vec![],
            mimes: vec![],
        };
        let sealed_follow_up = f
            .alice_group
            .seal_message(&f.alice_store, &DeviceSigner(&f.alice_dev), &follow_up)
            .unwrap();
        let opened = receive_sealed(&f.bob_store, &mut f.bob_group, sealed_follow_up);
        assert_decrypted(opened, "still sealed, still working");
    }

    #[tokio::test]
    async fn over_cap_content_is_refused_with_a_typed_error_and_nothing_submitted() {
        let mut f = two_member(1 << 62);
        let k = key(1 << 62);
        let transport = FakeTransport::new();

        let alice_actor = actor(&f.alice_id, &f.alice_dev);
        let mut alice_chain = mid_chain();
        let too_long = "a".repeat(MAX_CONTENT_CHARS + 1);
        let ctx = SealContext {
            key: &k,
            generation: 0,
            store: &f.alice_store,
            content: &too_long,
            reply_to: None,
        };
        let err = send_sealed(
            &transport,
            &alice_actor,
            &mut alice_chain,
            &ctx,
            &mut f.alice_group,
            &SendEligibility::confirmed(),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err, E2eeError::SealedOverCap { .. }),
            "expected SealedOverCap, got {err:?}"
        );
        assert_eq!(transport.submit_count(), 0, "nothing submitted for over-cap content");
    }

    #[tokio::test]
    async fn unconfirmed_sender_is_refused_locally_and_nothing_submitted() {
        let mut f = two_member(1 << 63);
        let k = key(1 << 63);
        let transport = FakeTransport::new();

        let alice_actor = actor(&f.alice_id, &f.alice_dev);
        let mut alice_chain = mid_chain();
        let ctx = SealContext {
            key: &k,
            generation: 0,
            store: &f.alice_store,
            content: "this must not go out",
            reply_to: None,
        };
        let err = send_sealed(
            &transport,
            &alice_actor,
            &mut alice_chain,
            &ctx,
            &mut f.alice_group,
            &SendEligibility::not_confirmed(),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, E2eeError::NotConfirmed), "expected NotConfirmed, got {err:?}");
        assert_eq!(transport.submit_count(), 0, "nothing submitted pre-confirmation");
    }

    #[tokio::test]
    async fn reply_to_is_carried_through_verbatim() {
        let mut f = two_member((1 << 60) + 5);
        let k = key((1 << 60) + 5);
        let transport = FakeTransport::new();
        let alice_actor = actor(&f.alice_id, &f.alice_dev);
        let mut alice_chain = mid_chain();

        let target = "ab".repeat(32); // a 64-hex-char event hash
        let ctx = SealContext {
            key: &k,
            generation: 0,
            store: &f.alice_store,
            content: "replying",
            reply_to: Some(target.clone()),
        };
        send_sealed(
            &transport,
            &alice_actor,
            &mut alice_chain,
            &ctx,
            &mut f.alice_group,
            &SendEligibility::confirmed(),
        )
        .await
        .unwrap();

        let (_, reply_to, _, _, _, _, _) = last_sealed_payload(&transport);
        assert_eq!(reply_to.as_deref(), Some(target.as_str()));

        // And the None case stays None (already covered in the round-trip test;
        // re-assert here for locality).
        let none_ctx = SealContext {
            key: &k,
            generation: 0,
            store: &f.alice_store,
            content: "no reply target",
            reply_to: None,
        };
        send_sealed(
            &transport,
            &alice_actor,
            &mut alice_chain,
            &none_ctx,
            &mut f.alice_group,
            &SendEligibility::confirmed(),
        )
        .await
        .unwrap();
        let (_, none_reply_to, _, _, _, _, _) = last_sealed_payload(&transport);
        assert_eq!(none_reply_to, None);
    }
}
