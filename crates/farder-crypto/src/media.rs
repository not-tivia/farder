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

use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, KeyInit};
use chacha20poly1305::aead::{Aead, Payload};

pub const SESSION_ID_LEN: usize = 16;
pub type SessionId = [u8; SESSION_ID_LEN];

/// Derive the 12-byte AEAD nonce for a media frame.
///
/// `nonce[0..4]  = session_id[0..4]`  — ties nonce to session
/// `nonce[4..12] = seq.to_be_bytes()` — monotonic per stream
///
/// Unique by construction provided `seq` is monotonic per session (u64
/// wraps after 18 quintillion frames — practically never).
pub fn media_frame_nonce(session_id: &SessionId, seq: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[0..4].copy_from_slice(&session_id[0..4]);
    nonce[4..12].copy_from_slice(&seq.to_be_bytes());
    nonce
}

/// Seal one media frame.
///
/// Encrypts `speaker_pk || codec_payload` under `key` with deterministic
/// nonce and AAD = `header_aad` (the 28 header bytes — binds the header
/// to the ciphertext). Returns ciphertext including the 16-byte AEAD tag.
pub fn seal_media_frame(
    key: &[u8; 32],
    seq: u64,
    session_id: &SessionId,
    header_aad: &[u8],
    speaker_pk: &[u8; 32],
    codec_payload: &[u8],
) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce_bytes = media_frame_nonce(session_id, seq);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let mut plaintext = Vec::with_capacity(32 + codec_payload.len());
    plaintext.extend_from_slice(speaker_pk);
    plaintext.extend_from_slice(codec_payload);

    cipher.encrypt(nonce, Payload { msg: &plaintext, aad: header_aad })
        .map_err(|e| anyhow!("AEAD encrypt: {e}"))
}

/// Open and verify one media frame. Returns `(speaker_pk, codec_payload)`.
pub fn open_media_frame(
    key: &[u8; 32],
    seq: u64,
    session_id: &SessionId,
    header_aad: &[u8],
    ciphertext: &[u8],
) -> Result<([u8; 32], Vec<u8>)> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce_bytes = media_frame_nonce(session_id, seq);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let plaintext = cipher.decrypt(nonce, Payload { msg: ciphertext, aad: header_aad })
        .map_err(|_| anyhow!("AEAD decrypt failed (wrong key / nonce / aad / tampered ciphertext)"))?;

    if plaintext.len() < 32 {
        return Err(anyhow!("plaintext too short to contain speaker_pk"));
    }
    let mut speaker_pk = [0u8; 32];
    speaker_pk.copy_from_slice(&plaintext[..32]);
    let codec_payload = plaintext[32..].to_vec();
    Ok((speaker_pk, codec_payload))
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

    fn fixed_session_id() -> SessionId {
        [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
    }

    fn fixed_speaker_pk() -> [u8; 32] {
        [42u8; 32]
    }

    fn fake_header_aad() -> Vec<u8> {
        // 28 bytes — version | type | track_id | codec_id | seq | session_id
        vec![
            0x02, 0x01, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 5,
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
        ]
    }

    #[test]
    fn seal_open_roundtrip() {
        let key = derive_stream_key();
        let session = fixed_session_id();
        let speaker = fixed_speaker_pk();
        let aad = fake_header_aad();
        let payload = b"hello opus";

        let ct = seal_media_frame(&key, 5, &session, &aad, &speaker, payload).unwrap();
        let (got_speaker, got_payload) = open_media_frame(&key, 5, &session, &aad, &ct).unwrap();
        assert_eq!(got_speaker, speaker);
        assert_eq!(got_payload, payload);
    }

    #[test]
    fn open_rejects_tampered_ciphertext() {
        let key = derive_stream_key();
        let session = fixed_session_id();
        let speaker = fixed_speaker_pk();
        let aad = fake_header_aad();

        let mut ct = seal_media_frame(&key, 5, &session, &aad, &speaker, b"hi").unwrap();
        ct[0] ^= 0xff;
        assert!(open_media_frame(&key, 5, &session, &aad, &ct).is_err());
    }

    #[test]
    fn open_rejects_tampered_aad() {
        let key = derive_stream_key();
        let session = fixed_session_id();
        let speaker = fixed_speaker_pk();
        let aad = fake_header_aad();

        let ct = seal_media_frame(&key, 5, &session, &aad, &speaker, b"hi").unwrap();
        let mut bad_aad = aad.clone();
        bad_aad[1] = 0x02;
        assert!(open_media_frame(&key, 5, &session, &bad_aad, &ct).is_err());
    }

    #[test]
    fn open_rejects_wrong_key() {
        let key = derive_stream_key();
        let other_key = derive_stream_key();
        let session = fixed_session_id();
        let speaker = fixed_speaker_pk();
        let aad = fake_header_aad();

        let ct = seal_media_frame(&key, 5, &session, &aad, &speaker, b"hi").unwrap();
        assert!(open_media_frame(&other_key, 5, &session, &aad, &ct).is_err());
    }

    #[test]
    fn open_rejects_wrong_seq() {
        let key = derive_stream_key();
        let session = fixed_session_id();
        let speaker = fixed_speaker_pk();
        let aad = fake_header_aad();

        let ct = seal_media_frame(&key, 5, &session, &aad, &speaker, b"hi").unwrap();
        assert!(open_media_frame(&key, 6, &session, &aad, &ct).is_err());
    }

    #[test]
    fn nonce_derivation_is_unique_per_seq() {
        let session = fixed_session_id();
        let n1 = media_frame_nonce(&session, 1);
        let n2 = media_frame_nonce(&session, 2);
        assert_ne!(n1, n2);
    }
}
