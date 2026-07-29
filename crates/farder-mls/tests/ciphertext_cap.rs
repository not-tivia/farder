//! The measured pin on `farder_crypto::event_log::MAX_E2EE_CIPHERTEXT_BYTES`
//! (mesh rung 2, sub-project 3, Task 3, Step 1).
//!
//! The spec's "40 KiB" ciphertext cap is the top `PADDING_BUCKETS` entry, which
//! is **plaintext**. The MLS `PrivateMessage` that seals it is larger. A literal
//! 40960-byte ingest cap would therefore hard-bounce a legal maximum-size
//! message — exactly the bug rev 2 fixed when it raised the cap from 16 KiB.
//!
//! So the cap is not asserted, it is MEASURED: this test builds a real
//! two-member `MlsChannelGroup`, seals a real maximum-size envelope through the
//! real `seal_message` path (encode → pre-seal limits → pad to the top bucket →
//! encrypt), and asserts the bytes that would go on the wire both fit the cap
//! and exceed the padding bucket. The second assertion is the one that keeps the
//! constant honest: if OpenMLS framing ever became free, the cap's headroom
//! would be unjustified and this test would say so.
//!
//! The server never links this crate; `farder-crypto` is the shared crate the
//! constant lives in, which is why this test can see both sides.

use farder_crypto::event_log::MAX_E2EE_CIPHERTEXT_BYTES;
use farder_crypto::identity::Keypair;
use farder_mls::credential::{credential_with_key, generate_key_package, DeviceSigner};
use farder_mls::envelope::{pad_to_bucket, MessageEnvelope};
use farder_mls::group::{decode_key_package, MlsChannelGroup};
use farder_mls::{MAX_CONTENT_CHARS, MAX_PRESEAL_BYTES, PADDING_BUCKETS};
use openmls::prelude::tls_codec::Serialize as TlsSerialize;
use openmls_rust_crypto::OpenMlsRustCrypto;

const GROUP_ID: &[u8] = b"server-1/channel-7/generation-0";

/// The largest envelope a compliant client can legally produce: `MAX_CONTENT_CHARS`
/// characters that are 4 bytes each in UTF-8, which is precisely the case the
/// spec's cap rationale names ("8000 chars is up to 32 KiB of UTF-8 (CJK/emoji)").
fn maximum_legal_envelope() -> MessageEnvelope {
    MessageEnvelope {
        content: "\u{1F600}".repeat(MAX_CONTENT_CHARS),
        attachment_keys: vec![],
        filenames: vec![],
        mimes: vec![],
    }
}

#[test]
fn a_legal_maximum_size_sealed_message_fits_the_ingest_cap() {
    let owner_id = Keypair::generate();
    let owner_dev = Keypair::generate();
    let alice_id = Keypair::generate();
    let alice_dev = Keypair::generate();
    let owner_prov = OpenMlsRustCrypto::default();
    let alice_prov = OpenMlsRustCrypto::default();

    // A real two-member group: the owner creates it, alice joins by KeyPackage.
    let mut group = MlsChannelGroup::create(
        &owner_prov,
        &DeviceSigner(&owner_dev),
        credential_with_key(&owner_dev, &owner_id.public_key()),
        GROUP_ID,
    )
    .expect("create the channel group");
    let bundle = generate_key_package(&alice_prov, &alice_dev, &alice_id.public_key())
        .expect("alice's key package");
    let kp_bytes = bundle
        .key_package()
        .tls_serialize_detached()
        .expect("serialize key package");
    let kp = decode_key_package(&owner_prov, &kp_bytes).expect("key package decodes");
    group
        .add_members(&owner_prov, &DeviceSigner(&owner_dev), &[kp])
        .expect("add alice");
    assert_eq!(group.members().expect("members").len(), 2, "two-member group");

    // The envelope really is at the ceiling of what a client may send, and it
    // really does land on the TOP padding bucket (otherwise this measures the
    // wrong case entirely).
    let envelope = maximum_legal_envelope();
    let encoded = envelope.encode().expect("encode envelope");
    assert!(
        encoded.len() <= MAX_PRESEAL_BYTES,
        "the fixture must be a LEGAL message: {} > {MAX_PRESEAL_BYTES}",
        encoded.len()
    );
    let top_bucket = *PADDING_BUCKETS.last().expect("non-empty ladder");
    assert_eq!(
        pad_to_bucket(&encoded).expect("pads").len(),
        top_bucket,
        "the fixture must land on the TOP padding bucket, or the cap is measured against the wrong size"
    );

    // The real seal path — this is the byte count ingest will see.
    let sealed = group
        .seal_message(&owner_prov, &DeviceSigner(&owner_dev), &envelope)
        .expect("seal the maximum-size envelope");

    // Measured 2026-07-29 on openmls 0.8.1: 41125 bytes for a 40960-byte
    // bucket — 165 bytes of framing, and 4955 bytes of headroom under the cap.
    assert!(
        sealed.len() <= MAX_E2EE_CIPHERTEXT_BYTES,
        "a legal maximum-size message must fit the ingest cap: {} > {}",
        sealed.len(),
        MAX_E2EE_CIPHERTEXT_BYTES
    );
    assert!(
        sealed.len() > top_bucket,
        "the cap must include MLS framing overhead, not just the padding bucket: {} <= {top_bucket}",
        sealed.len()
    );
}
