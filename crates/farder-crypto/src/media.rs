// crates/farder-crypto/src/media.rs
//
// Media-stream crypto helpers: per-stream symmetric key derivation,
// per-peer key wrap (using the existing DM E2EE primitive), and
// AEAD seal/open for individual media frames (added in MST-2).
//
// Consumed by `farder-server::media_stream` and the client.

use crate::key_exchange::derive_dm_shared_secret;
use crate::encryption;
use anyhow::{Result, anyhow};

/// 32-byte random ChaCha20-Poly1305 stream key. Generate ONCE per
/// (session, track) and distribute to all peers via `wrap_stream_key_for_peer`.
pub fn derive_stream_key() -> [u8; 32] {
    rand::random()
}

/// Encrypt `stream_key` for delivery to a single peer.
///
/// Reuses the existing DM E2EE primitive: derive an AES-256-GCM key from
/// `derive_dm_shared_secret(my_ed_sk, peer_ed_pk)`, then encrypt
/// `stream_key` (32 bytes plaintext) under that derived key with a random
/// nonce. Output: `nonce(12) || ciphertext(32) || tag(16)` = 60 bytes.
pub fn wrap_stream_key_for_peer(
    stream_key: &[u8; 32],
    my_ed_sk: &[u8; 32],
    peer_ed_pk: &[u8; 32],
) -> Result<Vec<u8>> {
    let shared = derive_dm_shared_secret(my_ed_sk, peer_ed_pk)
        .map_err(|e| anyhow!("derive_dm_shared_secret: {e}"))?;
    encryption::encrypt(&shared, stream_key)
}

/// Decrypt a `StreamKeyOffer.wrapped_key` delivered to us.
///
/// `sender_ed_pk` is taken from the StreamKeyOffer event's `sender` field.
pub fn unwrap_stream_key(
    wrapped: &[u8],
    my_ed_sk: &[u8; 32],
    sender_ed_pk: &[u8; 32],
) -> Result<[u8; 32]> {
    let shared = derive_dm_shared_secret(my_ed_sk, sender_ed_pk)
        .map_err(|e| anyhow!("derive_dm_shared_secret: {e}"))?;
    let plaintext = encryption::decrypt(&shared, wrapped)?;
    if plaintext.len() != 32 {
        return Err(anyhow!("unwrapped stream key is {} bytes; expected 32", plaintext.len()));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&plaintext);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    #[test]
    fn derive_stream_key_returns_32_bytes_random() {
        let k1 = derive_stream_key();
        let k2 = derive_stream_key();
        assert_eq!(k1.len(), 32);
        assert_ne!(k1, k2, "two derived keys should differ");
    }

    #[test]
    fn wrap_unwrap_roundtrip() {
        let alice_sk = SigningKey::generate(&mut OsRng);
        let bob_sk = SigningKey::generate(&mut OsRng);
        let alice_pk = alice_sk.verifying_key().to_bytes();
        let bob_pk = bob_sk.verifying_key().to_bytes();

        let stream_key = derive_stream_key();
        let wrapped = wrap_stream_key_for_peer(
            &stream_key,
            alice_sk.as_bytes(),
            &bob_pk,
        ).unwrap();
        let unwrapped = unwrap_stream_key(
            &wrapped,
            bob_sk.as_bytes(),
            &alice_pk,
        ).unwrap();
        assert_eq!(stream_key, unwrapped);
    }

    #[test]
    fn unwrap_rejects_wrong_recipient() {
        let alice_sk = SigningKey::generate(&mut OsRng);
        let bob_sk = SigningKey::generate(&mut OsRng);
        let charlie_sk = SigningKey::generate(&mut OsRng);
        let bob_pk = bob_sk.verifying_key().to_bytes();
        let alice_pk = alice_sk.verifying_key().to_bytes();

        let stream_key = derive_stream_key();
        let wrapped = wrap_stream_key_for_peer(
            &stream_key,
            alice_sk.as_bytes(),
            &bob_pk,
        ).unwrap();

        let result = unwrap_stream_key(
            &wrapped,
            charlie_sk.as_bytes(),
            &alice_pk,
        );
        assert!(result.is_err(), "charlie should not be able to unwrap Bob's key");
    }
}
