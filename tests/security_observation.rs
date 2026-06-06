// Observation-based security tests.
//
// CLAUDE.md requires that privacy/security guarantees be verified by OBSERVING
// the bytes that actually leave the process, not by reading code. These tests
// drive the real send paths and assert on the wire-format bytes:
//
//   * DM content placed on the wire is ciphertext — the plaintext never appears
//     in the bytes the client hands to `send_message`, nor in the encoded
//     protocol Message that the node path produces.
//   * The ciphertext is genuinely reversible by the intended peer and only the
//     intended peer (wrong key fails), proving it is real AEAD, not obfuscation.
//
// These mirror, byte-for-byte, the crypto the `dm_encrypt` Tauri command runs
// (derive_dm_shared_secret -> encryption::encrypt) and the node `prepare_dm`
// -> codec::encode path used for relayed DMs.

use farder_crypto::identity::Keypair;
use farder_crypto::{encryption, key_exchange};
use farder_node::node::PersonalNode;
use farder_protocol::{codec, messages::Message};

/// True if `needle` appears as a contiguous byte run inside `haystack`.
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Drives the EXACT path `dm_encrypt` runs: derive the X25519/Ed25519 shared
/// secret, AES-256-GCM encrypt, and observe the bytes that become the message
/// `content`. Asserts the plaintext is absent from those bytes, the intended
/// peer recovers it, and a third party with the wrong key cannot.
#[test]
fn dm_content_on_wire_is_ciphertext_not_plaintext() {
    let alice = Keypair::generate();
    let bob = Keypair::generate();
    let mallory = Keypair::generate(); // eavesdropper with their own identity

    // A distinctive plaintext we can search the wire bytes for. Long and unique
    // so a chance collision in 40-ish ciphertext bytes is effectively zero.
    let plaintext = b"FARDER-SECRET-CANARY meet me at the docks at midnight, bring the documents";

    // --- sender side: identical to dm_encrypt ---
    let shared_ab = key_exchange::derive_dm_shared_secret(
        alice.signing_key_bytes(),
        bob.public_key().as_bytes(),
    )
    .expect("alice derives shared secret with bob");
    let ciphertext = encryption::encrypt(&shared_ab, plaintext).expect("encrypt");

    // The client sends hex::encode(ciphertext) as the `content` string, so the
    // bytes that actually traverse the network are these. Observe both the raw
    // ciphertext and the hex-string form that goes in the protocol field.
    let wire_content_string = hex::encode(&ciphertext);
    let wire_bytes_raw = ciphertext.clone();
    let wire_bytes_hex = wire_content_string.as_bytes();

    // OBSERVATION 1: the plaintext does not appear on the wire, in either form.
    assert!(
        !contains_subslice(&wire_bytes_raw, plaintext),
        "plaintext leaked into raw ciphertext bytes on the wire"
    );
    assert!(
        !contains_subslice(wire_bytes_hex, plaintext),
        "plaintext leaked into the hex content string on the wire"
    );
    // Also assert no long printable run of the plaintext survives: check that
    // each whitespace-delimited word of the secret is absent from the wire.
    for word in plaintext.split(|b| *b == b' ') {
        if word.len() >= 4 {
            assert!(
                !contains_subslice(&wire_bytes_raw, word),
                "a plaintext word leaked onto the wire: {:?}",
                String::from_utf8_lossy(word)
            );
        }
    }

    // OBSERVATION 2: the intended peer recovers the exact plaintext (proves the
    // ciphertext is a real, reversible AEAD payload — not lossy obfuscation).
    let shared_ba = key_exchange::derive_dm_shared_secret(
        bob.signing_key_bytes(),
        alice.public_key().as_bytes(),
    )
    .expect("bob derives the same shared secret");
    assert_eq!(shared_ab, shared_ba, "ECDH must be symmetric");
    let recovered = encryption::decrypt(&shared_ba, &ciphertext).expect("bob decrypts");
    assert_eq!(recovered, plaintext, "bob must recover the exact plaintext");

    // OBSERVATION 3: an eavesdropper with the wrong key cannot read it. The
    // server, the relay, and any other member fall in this bucket.
    let shared_am = key_exchange::derive_dm_shared_secret(
        mallory.signing_key_bytes(),
        alice.public_key().as_bytes(),
    )
    .expect("mallory can still attempt a derivation");
    assert_ne!(shared_ab, shared_am, "an outsider derives a different secret");
    assert!(
        encryption::decrypt(&shared_am, &ciphertext).is_err(),
        "GCM auth must reject decryption with the wrong key"
    );
}

/// Drives the node relay path: `prepare_dm` -> `codec::encode`, and asserts the
/// plaintext is absent from the FULL encoded protocol frame (not just the
/// ciphertext field), so no envelope/metadata field leaks the message body.
#[test]
fn encoded_dm_protocol_frame_never_contains_plaintext() {
    let mut alice = PersonalNode::new_in_memory(Keypair::generate()).unwrap();
    let mut bob = PersonalNode::new_in_memory(Keypair::generate()).unwrap();

    let alice_session = alice.session_public_key_bytes();
    let bob_session = bob.session_public_key_bytes();
    alice.complete_key_exchange(&bob.public_key(), &bob_session);
    bob.complete_key_exchange(&alice.public_key(), &alice_session);

    let plaintext = b"FARDER-SECRET-CANARY-2 the password is hunter2 do not tell anyone";
    let msg = alice.prepare_dm(&bob.public_key(), plaintext).unwrap();

    // This is the actual wire frame: the serialized protocol Message.
    let frame = codec::encode(&msg).unwrap();

    assert!(
        !contains_subslice(&frame, plaintext),
        "plaintext leaked into the encoded protocol frame"
    );
    for word in plaintext.split(|b| *b == b' ') {
        if word.len() >= 4 {
            assert!(
                !contains_subslice(&frame, word),
                "a plaintext word leaked into the encoded frame: {:?}",
                String::from_utf8_lossy(word)
            );
        }
    }

    // And confirm the receiver still recovers it, so we know we tested the real
    // (reversible) frame and not a corrupted one.
    let decoded: Message = codec::decode(&frame).unwrap();
    match decoded {
        Message::EncryptedDm { sender, ciphertext, timestamp } => {
            let recovered = bob.receive_dm(&sender, &ciphertext, timestamp).unwrap();
            assert_eq!(recovered, plaintext);
        }
        other => panic!("expected EncryptedDm, got {other:?}"),
    }
}

/// The audit (2026-06-05) found the identity key was stored as raw plaintext.
/// This is the standing regression guard for the fix: the encrypted blob the
/// real key-wrapping path produces must neither equal nor contain the raw
/// private key, must reopen under the right PIN, and must reject the wrong one.
#[test]
fn encrypted_identity_blob_is_not_the_raw_private_key() {
    let kp = Keypair::generate();
    let raw = *kp.signing_key_bytes();
    let blob = kp.export_encrypted("1234").expect("encrypt identity");

    assert_ne!(blob.as_slice(), &raw[..], "blob equals the raw private key");
    assert!(
        !contains_subslice(&blob, &raw),
        "raw private key bytes appear verbatim inside the encrypted blob"
    );

    let back = Keypair::import_encrypted(&blob, "1234").expect("decrypt identity");
    assert_eq!(back.signing_key_bytes(), &raw);
    assert!(
        Keypair::import_encrypted(&blob, "9999").is_err(),
        "wrong PIN must fail"
    );
}
