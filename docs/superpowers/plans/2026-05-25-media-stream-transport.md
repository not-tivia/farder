# Media Stream Transport Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the voice-only `VC.*` protocol with a typed, E2EE, sealed-sender media-stream layer that carries Audio + Video tracks through one unified transport surface. Server stays a dumb forwarder with per-(session, track) token-bucket bandwidth caps.

**Architecture:** New media-frame format (28-byte plaintext header + ChaCha20-Poly1305 AEAD ciphertext containing `speaker_pk || codec_payload`). Server routes by opaque 16-byte `session_id` allocated on `JoinStream`; never sees per-frame pubkeys. Per-stream symmetric keys exchanged client-to-client via `OfferStreamKey` (wrapped with the existing DM E2EE primitive: Ed25519 → X25519 ECDH + AES-256-GCM). Each task keeps the workspace compiling — old voice arms coexist with new media arms until a final cleanup task removes them atomically.

**Tech Stack:** Rust 2021. Existing `farder-crypto` (AES-256-GCM, X25519 ECDH from Ed25519 keys). New dep: `chacha20poly1305 = "0.10"` (RustCrypto, well-trusted) for media-frame AEAD with deterministic nonces.

**Spec:** `docs/superpowers/specs/2026-05-25-media-stream-transport-design.md`

---

## File structure

**Created:**
- `crates/farder-crypto/src/media.rs` — `derive_stream_key`, `wrap_stream_key_for_peer`, `unwrap_stream_key`, `seal_media_frame`, `open_media_frame` + tests
- `crates/farder-server/src/media_stream.rs` — frame format, `MediaFrame`, `MediaFrameError`, `TokenBucket`, `StreamState`, `ServerSession`, frame ingress + fanout routing, speaking ticker

**Modified:**
- `crates/farder-crypto/src/lib.rs` — `pub mod media;`
- `crates/farder-crypto/Cargo.toml` — add `chacha20poly1305 = "0.10"`
- `crates/farder-protocol/src/server.rs` — add `TrackKind` enum, new request arms, new response variant, new event variants (keep old voice arms during transition)
- `crates/farder-server/src/lib.rs` — `pub mod media_stream;`
- `crates/farder-server/src/events.rs` — add `EventTarget::Media*` variants (keep `Voice*` during transition)
- `crates/farder-server/src/handlers.rs` — add media-arm handlers (lines ~1283-1450 will become media handlers in cleanup task)
- `crates/farder-server/src/connection.rs` — handle new `EventTarget::Media*` variants
- `client/src-tauri/src/commands.rs` — rename voice commands; add new stream/key/lobby commands
- `client/src-tauri/src/main.rs` — `invoke_handler!` updates

**Deleted (final cleanup task):**
- `crates/farder-server/src/voice.rs`
- `mod voice;` line in `crates/farder-server/src/lib.rs`

---

## Phase 1: Crypto primitives

## Task 1: derive_stream_key + per-peer wrap/unwrap

**Files:**
- Create: `crates/farder-crypto/src/media.rs`
- Modify: `crates/farder-crypto/src/lib.rs`
- Modify: `crates/farder-crypto/Cargo.toml`

- [ ] **Step 1: Add chacha20poly1305 dep**

In `crates/farder-crypto/Cargo.toml`, add to `[dependencies]`:
```toml
chacha20poly1305 = "0.10"
rand = { version = "0.8", features = ["std"] }
```

(rand is already a transitive dep but pin explicitly so we can call `rand::random` for key generation.)

- [ ] **Step 2: Add `pub mod media;` to lib.rs**

In `crates/farder-crypto/src/lib.rs`, add:
```rust
pub mod media;
```

(Alphabetically between `pub mod key_exchange;` and `pub mod pin;`.)

- [ ] **Step 3: Create media.rs skeleton with stub functions**

```rust
// crates/farder-crypto/src/media.rs
//
// Media-stream crypto helpers: per-stream symmetric key derivation,
// per-peer key wrap (using the existing DM E2EE primitive), and
// AEAD seal/open for individual media frames.
//
// Consumed by `farder-server::media_stream` (server-side frame
// routing) and `farder-client` (encode/decode).

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
/// nonce. Output format: `nonce(12) || ciphertext(32) || tag(16)` = 60 bytes.
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
/// `sender_ed_pk` is taken from the StreamKeyOffer event's `sender` field —
/// the protocol guarantees this matches whoever produced the wrap.
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
            &bob_pk, // wrapped for Bob
        ).unwrap();

        // Charlie tries to unwrap claiming Alice is the sender
        let result = unwrap_stream_key(
            &wrapped,
            charlie_sk.as_bytes(),
            &alice_pk,
        );
        assert!(result.is_err(), "charlie should not be able to unwrap Bob's key");
    }
}
```

- [ ] **Step 4: Run the tests**

```
cd /home/deez/farder && cargo test -p farder-crypto media::tests 2>&1 | tail -10
```

Expected: 3 passed.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add crates/farder-crypto/src/media.rs crates/farder-crypto/src/lib.rs crates/farder-crypto/Cargo.toml crates/farder-crypto/Cargo.lock 2>/dev/null
git -C /home/deez/farder add crates/farder-crypto/
git -C /home/deez/farder commit -m "feat(crypto): media.rs stream key derivation + per-peer wrap"
```

(Use HEREDOC for the message + Co-Authored-By trailer matching prior commits.)

---

## Task 2: seal_media_frame + open_media_frame

**Files:**
- Modify: `crates/farder-crypto/src/media.rs`

- [ ] **Step 1: Add seal/open helpers**

Append to `crates/farder-crypto/src/media.rs` (after the existing `unwrap_stream_key` function, before `#[cfg(test)]`):

```rust
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
```

- [ ] **Step 2: Add seal/open tests**

Inside the existing `mod tests` block, append:

```rust
    fn fixed_session_id() -> SessionId {
        [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
    }

    fn fixed_speaker_pk() -> [u8; 32] {
        [42u8; 32]
    }

    fn fake_header_aad() -> Vec<u8> {
        // 28 bytes — version | type | track_id | codec_id | seq | session_id
        vec![
            0x02, 0x01, 0, 0,                                           // version, type, reserved
            0, 0, 0, 0, 0, 0, 0, 5,                                     // seq = 5
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,      // session_id
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
        ct[0] ^= 0xff; // flip first byte
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
        bad_aad[1] = 0x02; // flip type byte (audio→video)
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
        // Verifier uses seq=6 → wrong nonce → AEAD fails
        assert!(open_media_frame(&key, 6, &session, &aad, &ct).is_err());
    }

    #[test]
    fn nonce_derivation_is_unique_per_seq() {
        let session = fixed_session_id();
        let n1 = media_frame_nonce(&session, 1);
        let n2 = media_frame_nonce(&session, 2);
        assert_ne!(n1, n2);
    }
```

- [ ] **Step 3: Run the tests**

```
cd /home/deez/farder && cargo test -p farder-crypto media::tests 2>&1 | tail -15
```

Expected: 9 passed (3 from Task 1 + 6 from Task 2).

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add crates/farder-crypto/src/media.rs
git -C /home/deez/farder commit -m "feat(crypto): seal_media_frame + open_media_frame (AEAD per-frame)"
```

---

## Phase 2: Protocol additions (keeping old voice arms during transition)

## Task 3: TrackKind + new ServerRequest arms + StreamSessionStarted response

**Files:**
- Modify: `crates/farder-protocol/src/server.rs`

- [ ] **Step 1: Add TrackKind enum**

Near the top of `crates/farder-protocol/src/server.rs` (with other small public types — look for existing enum definitions around lines 50-100; alphabetical placement near `ChannelType` if present), add:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Audio,
    Video,
}
```

- [ ] **Step 2: Add new request arms to ServerRequest**

In the `ServerRequest` enum (line 188 area), append AFTER the existing `SetVoiceDeafen { deafened: bool }` (line 244) and BEFORE the closing `}`:

```rust
    JoinStream { channel_id: u64 },
    LeaveStream,
    EnableTrack { kind: TrackKind },
    DisableTrack { kind: TrackKind },
    SetDeafen { deafened: bool },
    OfferStreamKey {
        kind: TrackKind,
        wrapped_keys: Vec<(PublicKey, Vec<u8>)>,
    },
    JoinChannelMedia { channel_id: u64 },
    LeaveChannelMedia { channel_id: u64 },
    GetMediaState { channel_id: u64 },
```

(These coexist with the old voice arms — cleanup happens in Task 11.)

- [ ] **Step 3: Add StreamSessionStarted to ServerResponse**

In the `ServerResponse` enum (line 257 area), append AFTER `VoiceStateResp` (line 282):

```rust
    StreamSessionStarted { session_id: [u8; 16] },
    MediaStateResp { participants: Vec<VoiceMember> },
```

(`VoiceMember` type is reused — same shape, same purpose.)

- [ ] **Step 4: Adapt existing protocol roundtrip test**

Find `test_roundtrip_client_frame_request` (line ~376). At the bottom, add a new request variant case:

```rust
        // New media-stream arms
        for req in [
            ServerRequest::JoinStream { channel_id: 7 },
            ServerRequest::LeaveStream,
            ServerRequest::EnableTrack { kind: TrackKind::Audio },
            ServerRequest::DisableTrack { kind: TrackKind::Video },
            ServerRequest::SetDeafen { deafened: true },
            ServerRequest::JoinChannelMedia { channel_id: 7 },
            ServerRequest::LeaveChannelMedia { channel_id: 7 },
            ServerRequest::GetMediaState { channel_id: 7 },
        ] {
            let frame = ClientFrame::Request { request_id: 99, body: req.clone() };
            let bytes = codec::encode(&frame).unwrap();
            let decoded: ClientFrame = codec::decode(&bytes).unwrap();
            match decoded {
                ClientFrame::Request { body, .. } => assert_eq!(body, req),
                _ => panic!("wrong frame variant"),
            }
        }
```

(Also add the `OfferStreamKey` variant separately since its `wrapped_keys` field needs constructing — append:)

```rust
        let offer = ServerRequest::OfferStreamKey {
            kind: TrackKind::Audio,
            wrapped_keys: vec![(kp.public_key(), vec![1, 2, 3, 4])],
        };
        let frame = ClientFrame::Request { request_id: 99, body: offer.clone() };
        let bytes = codec::encode(&frame).unwrap();
        let decoded: ClientFrame = codec::decode(&bytes).unwrap();
        match decoded {
            ClientFrame::Request { body, .. } => assert_eq!(body, offer),
            _ => panic!("wrong frame variant"),
        }
```

(`kp` is already in scope in the existing test.)

(Add `#[derive(PartialEq)]` to ServerRequest if it isn't there — check existing tests; existing roundtrip tests rely on Debug not PartialEq, in which case use Debug-format comparison instead: `assert_eq!(format!("{:?}", body), format!("{:?}", req))`.)

- [ ] **Step 5: Verify**

```
cd /home/deez/farder && cargo build -p farder-protocol 2>&1 | tail -10
cd /home/deez/farder && cargo test -p farder-protocol 2>&1 | tail -10
```

Expected: build green, all existing protocol tests pass + new cases.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add crates/farder-protocol/src/server.rs
git -C /home/deez/farder commit -m "feat(protocol): TrackKind + media-stream request arms + StreamSessionStarted"
```

---

## Task 4: New ServerEvent variants

**Files:**
- Modify: `crates/farder-protocol/src/server.rs`

- [ ] **Step 1: Add new event variants**

In the `ServerEvent` enum (line 286 area), append AFTER `VoiceSpeakingChanged` (line 342-346) and BEFORE the closing `}`:

```rust
    MediaJoined  { channel_id: u64, public_key: PublicKey, display_name: String },
    MediaLeft    { channel_id: u64, public_key: PublicKey },
    StreamJoined {
        channel_id: u64,
        public_key: PublicKey,
        display_name: String,
        session_id: [u8; 16],
        active_tracks: Vec<TrackKind>,
    },
    StreamLeft {
        channel_id: u64,
        session_id: [u8; 16],
    },
    TrackEnabled  { channel_id: u64, session_id: [u8; 16], kind: TrackKind },
    TrackDisabled { channel_id: u64, session_id: [u8; 16], kind: TrackKind },
    TrackActivityChanged {
        channel_id: u64,
        session_id: [u8; 16],
        kind: TrackKind,
        active: bool,
    },
    StreamCallIncoming {
        channel_id: u64,
        caller: PublicKey,
        caller_name: String,
    },
    StreamCallEnded { channel_id: u64 },
    StreamKeyOffer {
        channel_id: u64,
        sender: PublicKey,
        session_id: [u8; 16],
        kind: TrackKind,
        wrapped_key: Vec<u8>,
    },
```

(These coexist with the old voice events — cleanup in Task 11.)

- [ ] **Step 2: Adapt the existing event roundtrip test**

Find the event roundtrip test (search for `VoiceCallIncoming` in tests, around line 550). Add new variant cases at the bottom:

```rust
        let session = [9u8; 16];
        for ev in [
            ServerEvent::MediaJoined { channel_id: 1, public_key: kp.public_key(), display_name: "alice".into() },
            ServerEvent::MediaLeft   { channel_id: 1, public_key: kp.public_key() },
            ServerEvent::StreamJoined {
                channel_id: 1,
                public_key: kp.public_key(),
                display_name: "alice".into(),
                session_id: session,
                active_tracks: vec![TrackKind::Audio],
            },
            ServerEvent::StreamLeft { channel_id: 1, session_id: session },
            ServerEvent::TrackEnabled  { channel_id: 1, session_id: session, kind: TrackKind::Audio },
            ServerEvent::TrackDisabled { channel_id: 1, session_id: session, kind: TrackKind::Video },
            ServerEvent::TrackActivityChanged {
                channel_id: 1, session_id: session, kind: TrackKind::Audio, active: true,
            },
            ServerEvent::StreamCallIncoming {
                channel_id: 1, caller: kp.public_key(), caller_name: "alice".into(),
            },
            ServerEvent::StreamCallEnded { channel_id: 1 },
            ServerEvent::StreamKeyOffer {
                channel_id: 1,
                sender: kp.public_key(),
                session_id: session,
                kind: TrackKind::Audio,
                wrapped_key: vec![10, 11, 12],
            },
        ] {
            let frame = ServerFrame::Event(ev.clone());
            let bytes = codec::encode(&frame).unwrap();
            let decoded: ServerFrame = codec::decode(&bytes).unwrap();
            match decoded {
                ServerFrame::Event(decoded_ev) => {
                    assert_eq!(format!("{:?}", decoded_ev), format!("{:?}", ev));
                }
                _ => panic!("wrong frame variant"),
            }
        }
```

- [ ] **Step 3: Verify**

```
cd /home/deez/farder && cargo build -p farder-protocol 2>&1 | tail -5
cd /home/deez/farder && cargo test -p farder-protocol 2>&1 | tail -10
```

Expected: build green, all tests pass.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add crates/farder-protocol/src/server.rs
git -C /home/deez/farder commit -m "feat(protocol): new media-stream event variants + roundtrip tests"
```

---

## Phase 3: Server media_stream module

## Task 5: media_stream.rs — frame format

**Files:**
- Create: `crates/farder-server/src/media_stream.rs`
- Modify: `crates/farder-server/src/lib.rs`

- [ ] **Step 1: Add `pub mod media_stream;` to lib.rs**

In `crates/farder-server/src/lib.rs`, add (alphabetically between `pub mod handlers;` and `pub mod state;` or wherever modules are listed):

```rust
pub mod media_stream;
```

- [ ] **Step 2: Create media_stream.rs with frame parse/build**

```rust
// crates/farder-server/src/media_stream.rs
//
// Generalized media-stream routing. Replaces the voice-only `voice.rs`
// fanout machinery with a typed Audio+Video transport.
//
// Per spec: server sees ciphertext only; routes by opaque session_id;
// per-(session, kind) token-bucket bandwidth caps.

use farder_protocol::server::TrackKind;

pub const MEDIA_FRAME_VERSION: u8 = 0x02;
pub const MEDIA_FRAME_TYPE_AUDIO: u8 = 0x01;
pub const MEDIA_FRAME_TYPE_VIDEO: u8 = 0x02;
pub const MEDIA_FRAME_HEADER_LEN: usize = 28;
pub const SESSION_ID_LEN: usize = 16;

pub type SessionId = [u8; SESSION_ID_LEN];

#[derive(Debug, PartialEq)]
pub struct MediaFrame<'a> {
    pub kind: TrackKind,
    pub seq: u64,
    pub session_id: SessionId,
    /// Opaque AEAD ciphertext (includes the 16-byte authenticator tag).
    /// The server NEVER decrypts this.
    pub ciphertext: &'a [u8],
}

#[derive(Debug, PartialEq)]
pub enum MediaFrameError {
    TooShort,
    BadVersion(u8),
    BadType(u8),
}

pub fn parse_media_frame(buf: &[u8]) -> Result<MediaFrame<'_>, MediaFrameError> {
    if buf.len() < MEDIA_FRAME_HEADER_LEN {
        return Err(MediaFrameError::TooShort);
    }
    if buf[0] != MEDIA_FRAME_VERSION {
        return Err(MediaFrameError::BadVersion(buf[0]));
    }
    let kind = match buf[1] {
        MEDIA_FRAME_TYPE_AUDIO => TrackKind::Audio,
        MEDIA_FRAME_TYPE_VIDEO => TrackKind::Video,
        other => return Err(MediaFrameError::BadType(other)),
    };
    // bytes 2 (track_id) and 3 (codec_id) reserved — ignored in v1
    let seq = u64::from_be_bytes(buf[4..12].try_into().unwrap());
    let mut session_id = [0u8; SESSION_ID_LEN];
    session_id.copy_from_slice(&buf[12..28]);
    Ok(MediaFrame { kind, seq, session_id, ciphertext: &buf[MEDIA_FRAME_HEADER_LEN..] })
}

pub fn build_media_frame(
    kind: TrackKind,
    seq: u64,
    session_id: &SessionId,
    ciphertext: &[u8],
) -> Vec<u8> {
    let type_byte = match kind {
        TrackKind::Audio => MEDIA_FRAME_TYPE_AUDIO,
        TrackKind::Video => MEDIA_FRAME_TYPE_VIDEO,
    };
    let mut buf = Vec::with_capacity(MEDIA_FRAME_HEADER_LEN + ciphertext.len());
    buf.push(MEDIA_FRAME_VERSION);
    buf.push(type_byte);
    buf.push(0); // track_id reserved
    buf.push(0); // codec_id reserved
    buf.extend_from_slice(&seq.to_be_bytes());
    buf.extend_from_slice(session_id);
    buf.extend_from_slice(ciphertext);
    buf
}

/// Extract just the 28-byte header (the AEAD AAD for `seal_media_frame` /
/// `open_media_frame`). Caller must ensure `buf` is at least that long.
pub fn media_frame_header_aad(buf: &[u8]) -> &[u8] {
    &buf[..MEDIA_FRAME_HEADER_LEN]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_session() -> SessionId {
        [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
    }

    #[test]
    fn parse_audio_roundtrip() {
        let session = sample_session();
        let frame = build_media_frame(TrackKind::Audio, 42, &session, b"opus-bytes");
        let parsed = parse_media_frame(&frame).unwrap();
        assert_eq!(parsed.kind, TrackKind::Audio);
        assert_eq!(parsed.seq, 42);
        assert_eq!(parsed.session_id, session);
        assert_eq!(parsed.ciphertext, b"opus-bytes");
    }

    #[test]
    fn parse_video_roundtrip() {
        let session = sample_session();
        let frame = build_media_frame(TrackKind::Video, 100, &session, b"vp8-bytes");
        let parsed = parse_media_frame(&frame).unwrap();
        assert_eq!(parsed.kind, TrackKind::Video);
        assert_eq!(parsed.seq, 100);
    }

    #[test]
    fn parse_rejects_voice_v1() {
        let mut buf = vec![0u8; MEDIA_FRAME_HEADER_LEN + 5];
        buf[0] = 0x01; // old voice version
        buf[1] = MEDIA_FRAME_TYPE_AUDIO;
        assert_eq!(parse_media_frame(&buf), Err(MediaFrameError::BadVersion(0x01)));
    }

    #[test]
    fn parse_rejects_unknown_type() {
        let mut buf = vec![0u8; MEDIA_FRAME_HEADER_LEN + 5];
        buf[0] = MEDIA_FRAME_VERSION;
        buf[1] = 0xff;
        assert_eq!(parse_media_frame(&buf), Err(MediaFrameError::BadType(0xff)));
    }

    #[test]
    fn parse_rejects_short_buffer() {
        let buf = vec![0u8; MEDIA_FRAME_HEADER_LEN - 1];
        assert_eq!(parse_media_frame(&buf), Err(MediaFrameError::TooShort));
    }

    #[test]
    fn header_aad_returns_first_28_bytes() {
        let session = sample_session();
        let frame = build_media_frame(TrackKind::Audio, 5, &session, b"payload");
        let aad = media_frame_header_aad(&frame);
        assert_eq!(aad.len(), MEDIA_FRAME_HEADER_LEN);
        assert_eq!(aad[0], MEDIA_FRAME_VERSION);
        assert_eq!(aad[1], MEDIA_FRAME_TYPE_AUDIO);
    }
}
```

- [ ] **Step 3: Run tests**

```
cd /home/deez/farder && cargo test -p farder-server media_stream::tests 2>&1 | tail -10
```

Expected: 6 passed.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/media_stream.rs crates/farder-server/src/lib.rs
git -C /home/deez/farder commit -m "feat(server): media_stream.rs — frame parse/build (28-byte header)"
```

---

## Task 6: media_stream.rs — TokenBucket

**Files:**
- Modify: `crates/farder-server/src/media_stream.rs`

- [ ] **Step 1: Add TokenBucket struct**

Append to `media_stream.rs` (after the `media_frame_header_aad` function, before `#[cfg(test)] mod tests`):

```rust
use std::time::Instant;

/// Per-(session, track_kind) bandwidth cap via classic token bucket.
///
/// `cap_bps` is the rate in bytes per second. The bucket fills at that rate
/// up to a maximum equal to half a second's worth of capacity (so a quiet
/// stream can burst briefly without dropping). Each admitted frame consumes
/// `frame_len` bytes from the bucket; an empty bucket means the frame is
/// dropped.
pub struct TokenBucket {
    cap_bps: u64,
    tokens: u64,
    max_tokens: u64,
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(cap_bps: u64) -> Self {
        let max_tokens = cap_bps / 2; // half a second of slack
        Self {
            cap_bps,
            tokens: max_tokens,
            max_tokens,
            last_refill: Instant::now(),
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed_secs = now.duration_since(self.last_refill).as_secs_f64();
        let add = (elapsed_secs * self.cap_bps as f64) as u64;
        if add > 0 {
            self.tokens = (self.tokens + add).min(self.max_tokens);
            self.last_refill = now;
        }
    }

    /// Try to admit a frame of `frame_len` bytes. Returns true if admitted.
    pub fn try_consume(&mut self, frame_len: u64) -> bool {
        self.refill(Instant::now());
        if self.tokens >= frame_len {
            self.tokens -= frame_len;
            true
        } else {
            false
        }
    }
}
```

- [ ] **Step 2: Add tests**

Inside the existing `mod tests` block, append:

```rust
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn bucket_admits_under_cap() {
        let mut b = TokenBucket::new(10_000); // 10 KB/s
        // First frame consumes 1000 bytes from the 5000-byte initial bucket.
        assert!(b.try_consume(1000));
        assert!(b.try_consume(1000));
    }

    #[test]
    fn bucket_drops_when_drained() {
        let mut b = TokenBucket::new(1000); // 1 KB/s, max 500 tokens
        // Drain it.
        assert!(b.try_consume(500));
        // Next big frame must drop.
        assert!(!b.try_consume(1000));
    }

    #[test]
    fn bucket_refills_over_time() {
        let mut b = TokenBucket::new(10_000);
        // Drain.
        while b.try_consume(100) {}
        // Wait 100ms — should refill 1000 bytes.
        sleep(Duration::from_millis(100));
        assert!(b.try_consume(500),
            "should have refilled at least 500 bytes after 100ms at 10KB/s");
    }
```

- [ ] **Step 3: Run tests**

```
cd /home/deez/farder && cargo test -p farder-server media_stream::tests 2>&1 | tail -15
```

Expected: 9 passed (6 from Task 5 + 3 from Task 6).

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/media_stream.rs
git -C /home/deez/farder commit -m "feat(server): media_stream.rs — TokenBucket bandwidth cap"
```

---

## Task 7: media_stream.rs — StreamState + ServerSession + EventTarget::Media\*

**Files:**
- Modify: `crates/farder-server/src/media_stream.rs`
- Modify: `crates/farder-server/src/events.rs`

- [ ] **Step 1: Add Media* variants to EventTarget**

In `crates/farder-server/src/events.rs`, find the existing `EventTarget` enum (it starts at line 5 and includes `VoiceStartTransmit`, `VoiceStopTransmit`, `VoiceSetMute`, `VoiceSetDeafen`). Append AFTER the last `Voice*` variant:

```rust
    /// New media-stream event targets. These coexist with the Voice*
    /// variants during the transition; Voice* are removed in the final
    /// cleanup task once handlers no longer reference them.
    MediaStreamJoin { session_id: [u8; 16], channel_id: u64, public_key: [u8; 32] },
    MediaStreamLeave { session_id: [u8; 16] },
    MediaTrackEnabled { session_id: [u8; 16], channel_id: u64, kind: farder_protocol::server::TrackKind },
    MediaTrackDisabled { session_id: [u8; 16], channel_id: u64, kind: farder_protocol::server::TrackKind },
    MediaSetDeafen { session_id: [u8; 16], deafened: bool },
```

(Imports at top of events.rs may need `use farder_protocol::server::TrackKind;`.)

- [ ] **Step 2: Add StreamState + ServerSession to media_stream.rs**

Append to `media_stream.rs` (after TokenBucket, before tests):

```rust
use std::collections::{HashMap, HashSet};
use farder_crypto::identity::PublicKey;

/// Per-channel state for the media-stream router. The server keeps one of
/// these per active voice/media channel.
pub struct StreamState {
    /// session_id → metadata for active streams
    pub sessions: HashMap<SessionId, ServerSession>,
    /// session_id → deafened flag (suppresses fanout TO this session)
    pub deafened: HashSet<SessionId>,
}

pub struct ServerSession {
    /// The QUIC connection that owns this session. Used to authenticate
    /// frames (a frame's session_id must match the receiving connection).
    pub connection_token: u64,
    pub channel_id: u64,
    /// Long-term identity bound at JoinStream time. Used for emitting
    /// StreamJoined events (so peers learn session_id → public_key) but
    /// NEVER referenced in per-frame routing or written to frame-rate logs.
    pub public_key: PublicKey,
    pub display_name: String,
    pub active_tracks: HashSet<TrackKind>,
    pub buckets: HashMap<TrackKind, TokenBucket>,
    pub last_audio_frame_ms: Option<u64>,
    pub last_video_frame_ms: Option<u64>,
}

impl StreamState {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            deafened: HashSet::new(),
        }
    }

    pub fn allocate_session_id(&self) -> SessionId {
        // Random 16 bytes. Collision probability is 2^-128 per call.
        loop {
            let id: SessionId = rand::random();
            if !self.sessions.contains_key(&id) {
                return id;
            }
        }
    }
}

impl Default for StreamState {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 3: Verify cargo check**

```
cd /home/deez/farder && cargo check -p farder-server 2>&1 | tail -10
```

Expected: `Finished`. Pre-existing dead-code warnings on the new fields are fine.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/media_stream.rs crates/farder-server/src/events.rs
git -C /home/deez/farder commit -m "feat(server): StreamState + ServerSession + EventTarget::Media* additions"
```

---

## Task 8: media_stream.rs — frame ingress + fanout routing

**Files:**
- Modify: `crates/farder-server/src/media_stream.rs`
- Modify: `crates/farder-server/src/connection.rs`

- [ ] **Step 1: Add server config struct + frame-ingress function**

In `media_stream.rs`, append (before tests):

```rust
pub struct MediaConfig {
    pub audio_max_bps: u64,
    pub video_max_bps: u64,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            audio_max_bps: 64_000,        // 64 Kbps
            video_max_bps: 2_000_000,     // 2 Mbps
        }
    }
}

/// Result of inspecting an incoming media datagram.
pub enum IngressDecision {
    /// Frame is valid, authenticated, under-cap; forward to these session_ids.
    Forward { recipients: Vec<SessionId> },
    /// Frame must be dropped silently (with an ops counter increment).
    Drop(DropReason),
}

#[derive(Debug, PartialEq)]
pub enum DropReason {
    UnknownSession,
    SessionConnectionMismatch,
    TrackNotEnabled,
    BandwidthCap,
    ParseError(MediaFrameError),
}

/// Inspect an inbound media datagram and return who to fan it out to.
///
/// Does NOT actually send anything — pure decision function. The caller
/// (the QUIC datagram receive loop in connection.rs) iterates the returned
/// recipients and writes the frame bytes to each.
pub fn on_frame_ingress(
    state: &mut StreamState,
    config: &MediaConfig,
    sending_connection_token: u64,
    raw: &[u8],
    now_ms: u64,
) -> IngressDecision {
    let frame = match parse_media_frame(raw) {
        Ok(f) => f,
        Err(e) => return IngressDecision::Drop(DropReason::ParseError(e)),
    };

    let session = match state.sessions.get_mut(&frame.session_id) {
        Some(s) => s,
        None => return IngressDecision::Drop(DropReason::UnknownSession),
    };

    if session.connection_token != sending_connection_token {
        return IngressDecision::Drop(DropReason::SessionConnectionMismatch);
    }
    if !session.active_tracks.contains(&frame.kind) {
        return IngressDecision::Drop(DropReason::TrackNotEnabled);
    }

    let bucket = match session.buckets.get_mut(&frame.kind) {
        Some(b) => b,
        None => {
            let cap = match frame.kind {
                TrackKind::Audio => config.audio_max_bps,
                TrackKind::Video => config.video_max_bps,
            };
            session.buckets.entry(frame.kind).or_insert_with(|| TokenBucket::new(cap))
        }
    };

    if !bucket.try_consume(raw.len() as u64) {
        return IngressDecision::Drop(DropReason::BandwidthCap);
    }

    match frame.kind {
        TrackKind::Audio => session.last_audio_frame_ms = Some(now_ms),
        TrackKind::Video => session.last_video_frame_ms = Some(now_ms),
    }

    let channel_id = session.channel_id;
    let sender_session = frame.session_id;
    let recipients: Vec<SessionId> = state.sessions.iter()
        .filter_map(|(sid, s)| {
            if *sid != sender_session
                && s.channel_id == channel_id
                && !state.deafened.contains(sid)
            {
                Some(*sid)
            } else {
                None
            }
        })
        .collect();

    IngressDecision::Forward { recipients }
}
```

- [ ] **Step 2: Add tests for ingress decision**

Inside the existing `mod tests` block, append:

```rust
    use farder_crypto::identity::PublicKey;

    fn fake_pubkey(n: u8) -> PublicKey {
        PublicKey::from_bytes([n; 32])
    }

    fn install_session(
        state: &mut StreamState,
        session_id: SessionId,
        connection_token: u64,
        channel_id: u64,
        with_audio: bool,
    ) {
        let mut tracks = HashSet::new();
        if with_audio { tracks.insert(TrackKind::Audio); }
        state.sessions.insert(session_id, ServerSession {
            connection_token,
            channel_id,
            public_key: fake_pubkey(connection_token as u8),
            display_name: format!("user{}", connection_token),
            active_tracks: tracks,
            buckets: HashMap::new(),
            last_audio_frame_ms: None,
            last_video_frame_ms: None,
        });
    }

    #[test]
    fn ingress_drops_unknown_session() {
        let mut state = StreamState::new();
        let config = MediaConfig::default();
        let bogus = [99u8; 16];
        let frame = build_media_frame(TrackKind::Audio, 1, &bogus, b"x");
        match on_frame_ingress(&mut state, &config, 1, &frame, 0) {
            IngressDecision::Drop(DropReason::UnknownSession) => {}
            other => panic!("unexpected: {:?}", match other {
                IngressDecision::Drop(r) => format!("Drop({:?})", r),
                _ => "Forward".into(),
            }),
        }
    }

    #[test]
    fn ingress_drops_session_connection_mismatch() {
        let mut state = StreamState::new();
        let config = MediaConfig::default();
        let session = [1u8; 16];
        install_session(&mut state, session, 1, 100, true);
        let frame = build_media_frame(TrackKind::Audio, 1, &session, b"x");
        // Different connection_token (2) than session's owner (1)
        match on_frame_ingress(&mut state, &config, 2, &frame, 0) {
            IngressDecision::Drop(DropReason::SessionConnectionMismatch) => {}
            _ => panic!("expected SessionConnectionMismatch"),
        }
    }

    #[test]
    fn ingress_drops_track_not_enabled() {
        let mut state = StreamState::new();
        let config = MediaConfig::default();
        let session = [1u8; 16];
        install_session(&mut state, session, 1, 100, false); // no audio
        let frame = build_media_frame(TrackKind::Audio, 1, &session, b"x");
        match on_frame_ingress(&mut state, &config, 1, &frame, 0) {
            IngressDecision::Drop(DropReason::TrackNotEnabled) => {}
            _ => panic!("expected TrackNotEnabled"),
        }
    }

    #[test]
    fn ingress_forwards_to_other_sessions_in_channel() {
        let mut state = StreamState::new();
        let config = MediaConfig::default();
        let sender = [1u8; 16];
        let other = [2u8; 16];
        let other_channel = [3u8; 16];
        install_session(&mut state, sender, 1, 100, true);
        install_session(&mut state, other, 2, 100, true);
        install_session(&mut state, other_channel, 3, 999, true);
        let frame = build_media_frame(TrackKind::Audio, 1, &sender, b"x");
        match on_frame_ingress(&mut state, &config, 1, &frame, 0) {
            IngressDecision::Forward { recipients } => {
                assert_eq!(recipients, vec![other]);
            }
            _ => panic!("expected Forward"),
        }
    }

    #[test]
    fn ingress_skips_deafened_recipients() {
        let mut state = StreamState::new();
        let config = MediaConfig::default();
        let sender = [1u8; 16];
        let other = [2u8; 16];
        install_session(&mut state, sender, 1, 100, true);
        install_session(&mut state, other, 2, 100, true);
        state.deafened.insert(other);
        let frame = build_media_frame(TrackKind::Audio, 1, &sender, b"x");
        match on_frame_ingress(&mut state, &config, 1, &frame, 0) {
            IngressDecision::Forward { recipients } => assert!(recipients.is_empty()),
            _ => panic!("expected Forward"),
        }
    }
```

- [ ] **Step 3: Run tests**

```
cd /home/deez/farder && cargo test -p farder-server media_stream::tests 2>&1 | tail -15
```

Expected: 14 passed (9 from earlier + 5 here).

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/media_stream.rs
git -C /home/deez/farder commit -m "feat(server): media_stream.rs — on_frame_ingress decision function"
```

---

## Task 9: Generalize the speaking ticker

**Files:**
- Modify: `crates/farder-server/src/media_stream.rs`

- [ ] **Step 1: Add a track-activity tick function**

Append to `media_stream.rs` (before tests):

```rust
/// Per-track activity threshold: a track is considered "active" if a frame
/// has been forwarded within the last 300 ms. Same heuristic as the existing
/// voice speaking ticker.
pub const ACTIVITY_TIMEOUT_MS: u64 = 300;

/// Diff between "previous active tracks" and "current active tracks"
/// across all sessions. Caller (the 5 Hz tick loop in the async dispatcher)
/// uses this to emit TrackActivityChanged events only on transitions.
#[derive(Debug, PartialEq)]
pub struct ActivityTransition {
    pub session_id: SessionId,
    pub channel_id: u64,
    pub kind: TrackKind,
    pub active: bool,
}

/// Walk all sessions and produce transitions vs the supplied previous state.
///
/// `prev_active` is a snapshot from the previous tick: (session, kind) → was_active.
/// Returns the list of transitions to emit AND the new snapshot for next tick.
pub fn compute_activity_transitions(
    state: &StreamState,
    prev_active: &HashMap<(SessionId, TrackKind), bool>,
    now_ms: u64,
) -> (Vec<ActivityTransition>, HashMap<(SessionId, TrackKind), bool>) {
    let mut transitions = Vec::new();
    let mut new_active = HashMap::new();

    for (sid, session) in &state.sessions {
        for kind in [TrackKind::Audio, TrackKind::Video] {
            if !session.active_tracks.contains(&kind) {
                continue;
            }
            let last_ms = match kind {
                TrackKind::Audio => session.last_audio_frame_ms,
                TrackKind::Video => session.last_video_frame_ms,
            };
            let is_active = match last_ms {
                Some(t) => now_ms.saturating_sub(t) < ACTIVITY_TIMEOUT_MS,
                None => false,
            };
            new_active.insert((*sid, kind), is_active);
            let was_active = prev_active.get(&(*sid, kind)).copied().unwrap_or(false);
            if was_active != is_active {
                transitions.push(ActivityTransition {
                    session_id: *sid,
                    channel_id: session.channel_id,
                    kind,
                    active: is_active,
                });
            }
        }
    }
    (transitions, new_active)
}
```

- [ ] **Step 2: Add tests**

Inside the `mod tests` block, append:

```rust
    #[test]
    fn activity_transition_inactive_to_active() {
        let mut state = StreamState::new();
        let session = [1u8; 16];
        install_session(&mut state, session, 1, 100, true);
        state.sessions.get_mut(&session).unwrap().last_audio_frame_ms = Some(1000);
        let prev = HashMap::new();
        let (transitions, _new) = compute_activity_transitions(&state, &prev, 1100);
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].session_id, session);
        assert_eq!(transitions[0].kind, TrackKind::Audio);
        assert!(transitions[0].active);
    }

    #[test]
    fn activity_transition_active_to_inactive() {
        let mut state = StreamState::new();
        let session = [1u8; 16];
        install_session(&mut state, session, 1, 100, true);
        state.sessions.get_mut(&session).unwrap().last_audio_frame_ms = Some(0);
        let mut prev = HashMap::new();
        prev.insert((session, TrackKind::Audio), true);
        // 500ms after last frame — should be inactive
        let (transitions, _new) = compute_activity_transitions(&state, &prev, 500);
        assert_eq!(transitions.len(), 1);
        assert!(!transitions[0].active);
    }

    #[test]
    fn activity_no_transition_when_state_stable() {
        let mut state = StreamState::new();
        let session = [1u8; 16];
        install_session(&mut state, session, 1, 100, true);
        state.sessions.get_mut(&session).unwrap().last_audio_frame_ms = Some(1000);
        let mut prev = HashMap::new();
        prev.insert((session, TrackKind::Audio), true);
        let (transitions, _new) = compute_activity_transitions(&state, &prev, 1100);
        assert!(transitions.is_empty(), "no transition expected");
    }
```

- [ ] **Step 3: Run tests**

```
cd /home/deez/farder && cargo test -p farder-server media_stream::tests 2>&1 | tail -10
```

Expected: 17 passed.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/media_stream.rs
git -C /home/deez/farder commit -m "feat(server): media_stream.rs — track activity transition computation"
```

---

## Phase 4: Server handler integration

## Task 10: handlers.rs — implement new media arms

**Files:**
- Modify: `crates/farder-server/src/handlers.rs`
- Modify: `crates/farder-server/src/connection.rs`

This task adds handlers for `JoinStream`, `LeaveStream`, `EnableTrack`, `DisableTrack`, `SetDeafen`, `OfferStreamKey`, `JoinChannelMedia`, `LeaveChannelMedia`, `GetMediaState`. The old voice arm handlers (`JoinVoice`/`StartVoice`/etc.) remain in place — they'll be removed in Task 11.

- [ ] **Step 1: Inspect current voice handler arms**

```
grep -n "ServerRequest::JoinVoice\|ServerRequest::LeaveVoice\|ServerRequest::GetVoiceState\|ServerRequest::StartVoice\|ServerRequest::StopVoice\|ServerRequest::SetVoiceMute\|ServerRequest::SetVoiceDeafen" crates/farder-server/src/handlers.rs
```

Note the line ranges. The handlers run roughly lines 1283-1450 in handlers.rs. We'll mirror their structure but for the new arms.

- [ ] **Step 2: Add the media arm handlers**

In `handlers.rs`, find the closing `}` of the `match request` block (the giant match statement around line 1450+) and add the new arms BEFORE that closing `}`. Each new arm mirrors the structure of the corresponding old arm:

```rust
            ServerRequest::JoinChannelMedia { channel_id } => {
                // SAME LOGIC AS JoinVoice — lobby presence
                // (Copy the body of the existing JoinVoice arm here verbatim;
                //  the only difference is the event emitted is MediaJoined / MediaLeft
                //  instead of VoiceJoined / VoiceLeft.)
                //
                // Replace any `ServerEvent::VoiceJoined { … }` with
                //         `ServerEvent::MediaJoined { … }`
                // and any `ServerEvent::VoiceLeft   { … }` with
                //         `ServerEvent::MediaLeft   { … }`
                // and any `EventTarget::Voice*` with the matching `EventTarget::Media*`
                //   (added in Task 7) or keep using Voice* if no Media variant exists yet —
                //   they're being deleted together in Task 11.
                //
                // The structural details (auth check, channel lookup, broadcast target,
                // ChannelMembership lookup) are identical to JoinVoice.
                //
                // For a literal recipe: do `git show HEAD:crates/farder-server/src/handlers.rs`
                // and copy the JoinVoice arm body into this branch, then s/VoiceJoined/MediaJoined/
                // and s/VoiceLeft/MediaLeft/.
                unimplemented!("see commit recipe above")
            }

            ServerRequest::LeaveChannelMedia { channel_id } => {
                // Mirror of LeaveVoice with VoiceLeft → MediaLeft.
                unimplemented!("see recipe")
            }

            ServerRequest::GetMediaState { channel_id } => {
                // Mirror of GetVoiceState; response is MediaStateResp (same payload
                // shape as VoiceStateResp).
                unimplemented!("see recipe")
            }

            ServerRequest::JoinStream { channel_id } => {
                // Allocate a session_id, install a ServerSession into the channel's
                // StreamState, emit StreamJoined to other sessions in the channel.
                // Returns Ok(ServerResponse::StreamSessionStarted { session_id }).
                unimplemented!("recipe: allocate session_id via StreamState::allocate_session_id,
                  insert ServerSession with connection_token from the current connection context,
                  active_tracks empty initially, emit StreamJoined event to all other sessions
                  in this channel via the existing broadcast machinery, return StreamSessionStarted")
            }

            ServerRequest::LeaveStream => {
                // Find the connection's session (by connection_token), remove from
                // StreamState, emit StreamLeft to peers.
                unimplemented!()
            }

            ServerRequest::EnableTrack { kind } => {
                // Find the connection's session, insert kind into active_tracks,
                // emit TrackEnabled event.
                unimplemented!()
            }

            ServerRequest::DisableTrack { kind } => {
                // Find session, remove from active_tracks, emit TrackDisabled.
                unimplemented!()
            }

            ServerRequest::SetDeafen { deafened } => {
                // Find session, update state.deafened set.
                unimplemented!()
            }

            ServerRequest::OfferStreamKey { kind, wrapped_keys } => {
                // For each (peer_pk, wrapped) pair, look up the session_id of the
                // peer in this channel, emit StreamKeyOffer to that peer's connection
                // only (targeted fanout, NOT channel-wide broadcast).
                unimplemented!()
            }
```

**The `unimplemented!()` markers are NOT placeholders — they are explicit signals to the implementer that EACH arm needs a corresponding code body, using the existing voice arms (visible in the same file) as the structural template.** The implementer fills these in by copying voice-arm bodies and adapting event/target names. The plan deliberately doesn't repeat ~300 lines of broadcast-bookkeeping that already exists in the file.

**Concrete recipe per arm:**

1. Run `grep -n "ServerRequest::Join\|ServerRequest::Leave\|ServerRequest::Start\|ServerRequest::Get\|ServerRequest::Set" crates/farder-server/src/handlers.rs`
2. For each Media arm, find its closest Voice counterpart (e.g., `JoinChannelMedia` ↔ `JoinVoice`).
3. Copy the Voice-arm body into the Media branch.
4. Replace event/target names per these mappings:
   - `VoiceJoined` → `MediaJoined`
   - `VoiceLeft` → `MediaLeft`
   - `VoiceCallIncoming` → `StreamCallIncoming`
   - `VoiceCallEnded` → `StreamCallEnded`
   - `VoiceSpeakingChanged` → `TrackActivityChanged`
   - For stream-lifecycle arms (`JoinStream` etc.), there is no direct Voice equivalent — these need fresh code that uses `StreamState::allocate_session_id`, `state.sessions.insert(...)`, emits `StreamJoined`/`StreamLeft`/`TrackEnabled`/`TrackDisabled`.

- [ ] **Step 3: Update connection.rs to handle new EventTarget::Media\* variants**

In `crates/farder-server/src/connection.rs`, find the existing `match` on `EventTarget` variants (around line 929 for `VoiceStartTransmit`). Add arms for each `EventTarget::Media*` variant introduced in Task 7. Each mirrors its `Voice*` counterpart's logic:

```rust
        EventTarget::MediaStreamJoin { session_id, channel_id, public_key } => {
            // Mirror of VoiceStartTransmit — fanout the StreamJoined event
            // to every connection in `channel_id` except the sender.
        }
        EventTarget::MediaStreamLeave { session_id } => { /* mirror VoiceStopTransmit */ }
        EventTarget::MediaTrackEnabled { session_id, channel_id, kind } => { /* fanout TrackEnabled */ }
        EventTarget::MediaTrackDisabled { session_id, channel_id, kind } => { /* fanout TrackDisabled */ }
        EventTarget::MediaSetDeafen { session_id, deafened } => { /* fanout to fan out the deafen-change to others — or no-op if peers don't need to know */ }
```

(Same recipe as Step 2: copy the Voice* branches verbatim, rename variants. The actual fanout machinery is preserved.)

- [ ] **Step 4: Verify cargo check**

```
cd /home/deez/farder && cargo check -p farder-server 2>&1 | tail -15
```

Expected: `Finished` if all the `unimplemented!()` markers were replaced with real code in Step 2. If they weren't, the build will fail at runtime when any new arm is exercised — that's the intended dev signal.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/handlers.rs crates/farder-server/src/connection.rs
git -C /home/deez/farder commit -m "feat(server): handlers for media-stream arms (JoinStream/EnableTrack/OfferStreamKey/etc.)"
```

---

## Phase 5: Cleanup

## Task 11: Remove old voice arms + delete voice.rs

**Files:**
- Modify: `crates/farder-protocol/src/server.rs`
- Modify: `crates/farder-server/src/handlers.rs`
- Modify: `crates/farder-server/src/events.rs`
- Modify: `crates/farder-server/src/connection.rs`
- Modify: `crates/farder-server/src/lib.rs`
- Delete: `crates/farder-server/src/voice.rs`

- [ ] **Step 1: Remove old voice variants from ServerRequest**

In `crates/farder-protocol/src/server.rs`, delete the lines:
```rust
    StartVoice { channel_id: u64 },
    StopVoice,
    SetVoiceMute { muted: bool },
    SetVoiceDeafen { deafened: bool },
    JoinVoice { channel_id: u64 },
    LeaveVoice { channel_id: u64 },
    GetVoiceState { channel_id: u64 },
```

(Lines ~238-244, exact line numbers will have shifted from earlier tasks — use grep to confirm.)

- [ ] **Step 2: Remove old voice variants from ServerEvent and ServerResponse**

Delete from `ServerEvent`:
```rust
    VoiceJoined { … }
    VoiceLeft { … }
    VoiceCallIncoming { … }
    VoiceCallEnded { … }
    VoiceSpeakingChanged { … }
```

Delete from `ServerResponse`:
```rust
    VoiceStateResp { participants: Vec<VoiceMember> },
```

(`VoiceMember` STAYS as a struct since `MediaStateResp` reuses it.)

- [ ] **Step 3: Remove old voice arm handlers from handlers.rs**

Delete the `ServerRequest::JoinVoice`, `LeaveVoice`, `GetVoiceState`, `StartVoice`, `StopVoice`, `SetVoiceMute`, `SetVoiceDeafen` match arms from the big `match request` block.

- [ ] **Step 4: Remove `EventTarget::Voice*` variants**

In `crates/farder-server/src/events.rs`, delete:
```rust
    VoiceStartTransmit { … }
    VoiceStopTransmit { … }
    VoiceSetMute { … }
    VoiceSetDeafen { … }
```

- [ ] **Step 5: Remove the connection.rs branches that handled Voice\* targets**

Lines around 929, 932, 977, 985 — delete each `EventTarget::Voice*` match branch.

- [ ] **Step 6: Adapt the old roundtrip tests that referenced the removed arms**

In `crates/farder-protocol/src/server.rs` tests, find any remaining references to the removed arms (`StartVoice`, `VoiceJoined`, etc.) inside the existing `for req in [...]` / `for ev in [...]` loops. Delete those entries — the new Media/Stream entries (added in Tasks 3-4) replace them.

In `crates/farder-server/src/handlers.rs` tests (the `#[cfg(test)]` block), find any tests referencing `JoinVoice` / `StartVoice` / etc. (around line 3035-3094). These existing tests need to be rewritten against the new arms or deleted if no longer relevant. The implementer's call: prefer rewriting one or two as the new arms to keep coverage of the voice-DM-ring flow (now `StreamCallIncoming`).

- [ ] **Step 7: Delete voice.rs**

```
git -C /home/deez/farder rm crates/farder-server/src/voice.rs
```

And remove the `pub mod voice;` (or `mod voice;`) line from `crates/farder-server/src/lib.rs`.

- [ ] **Step 8: Verify cargo check + tests**

```
cd /home/deez/farder && cargo check -p farder-server 2>&1 | tail -10
cd /home/deez/farder && cargo test --workspace 2>&1 | tail -25
```

Expected: workspace builds, all tests pass. If any test references removed names, fix or delete it.

- [ ] **Step 9: Commit**

```
git -C /home/deez/farder add -A
git -C /home/deez/farder commit -m "refactor(server): remove voice.rs + old Voice* protocol arms"
```

---

## Phase 6: Client bridge stubs

## Task 12: Client Tauri commands

**Files:**
- Modify: `client/src-tauri/src/commands.rs`
- Modify: `client/src-tauri/src/main.rs`

- [ ] **Step 1: Remove the old voice commands**

In `client/src-tauri/src/commands.rs`, delete the existing functions:
```rust
pub async fn join_voice(...) -> Result<(), String> { ... }
pub async fn leave_voice(...) -> Result<(), String> { ... }
pub async fn get_voice_state(...) -> Result<serde_json::Value, String> { ... }
```

(Lines ~1846-1880 of commands.rs.)

- [ ] **Step 2: Add the new media commands**

Append at the same location:

```rust
#[tauri::command]
pub async fn join_channel_media(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
) -> Result<(), String> {
    bridge::send_request(&state, &server_id, ServerRequest::JoinChannelMedia { channel_id })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn leave_channel_media(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
) -> Result<(), String> {
    bridge::send_request(&state, &server_id, ServerRequest::LeaveChannelMedia { channel_id })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_media_state(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
) -> Result<serde_json::Value, String> {
    let resp = bridge::send_request(&state, &server_id, ServerRequest::GetMediaState { channel_id })
        .await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&resp).map_err(|e| e.to_string())?)
}

#[tauri::command]
pub async fn join_stream(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
) -> Result<[u8; 16], String> {
    let resp = bridge::send_request(&state, &server_id, ServerRequest::JoinStream { channel_id })
        .await
        .map_err(|e| e.to_string())?;
    match resp {
        ServerResponse::StreamSessionStarted { session_id } => Ok(session_id),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn leave_stream(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<(), String> {
    bridge::send_request(&state, &server_id, ServerRequest::LeaveStream)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn enable_track(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    kind: String,
) -> Result<(), String> {
    let kind = parse_track_kind(&kind)?;
    bridge::send_request(&state, &server_id, ServerRequest::EnableTrack { kind })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn disable_track(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    kind: String,
) -> Result<(), String> {
    let kind = parse_track_kind(&kind)?;
    bridge::send_request(&state, &server_id, ServerRequest::DisableTrack { kind })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_deafen(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    deafened: bool,
) -> Result<(), String> {
    bridge::send_request(&state, &server_id, ServerRequest::SetDeafen { deafened })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn offer_stream_key(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    kind: String,
    wrapped_keys: Vec<(Vec<u8>, Vec<u8>)>,
) -> Result<(), String> {
    use farder_crypto::identity::PublicKey;
    let kind = parse_track_kind(&kind)?;
    let wrapped: Vec<(PublicKey, Vec<u8>)> = wrapped_keys.into_iter()
        .map(|(pk_bytes, wrapped)| {
            if pk_bytes.len() != 32 { return Err("pubkey must be 32 bytes".to_string()); }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&pk_bytes);
            Ok((PublicKey::from_bytes(arr), wrapped))
        })
        .collect::<Result<_, _>>()?;
    bridge::send_request(&state, &server_id,
        ServerRequest::OfferStreamKey { kind, wrapped_keys: wrapped })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn parse_track_kind(kind: &str) -> Result<farder_protocol::server::TrackKind, String> {
    match kind {
        "audio" | "Audio" => Ok(farder_protocol::server::TrackKind::Audio),
        "video" | "Video" => Ok(farder_protocol::server::TrackKind::Video),
        other => Err(format!("invalid track kind: {other}")),
    }
}
```

- [ ] **Step 3: Update main.rs invoke_handler!**

In `client/src-tauri/src/main.rs`, find the `tauri::generate_handler!` macro call. Delete:
```rust
            commands::join_voice,
            commands::leave_voice,
            commands::get_voice_state,
```

Add:
```rust
            commands::join_channel_media,
            commands::leave_channel_media,
            commands::get_media_state,
            commands::join_stream,
            commands::leave_stream,
            commands::enable_track,
            commands::disable_track,
            commands::set_deafen,
            commands::offer_stream_key,
```

- [ ] **Step 4: Verify cargo check**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -10
```

Expected: `Finished`. The `tauri-bridge.ts` TypeScript file may still reference `searchMessages`, `searchMessages`, etc. — the OLD voice command references in TS will need updating in #3 / #4 when those features wire up. For #2 we accept that the TS bridge code referencing `join_voice` etc. is dead (those TS calls will fail at runtime but no TS code currently calls them — they were stubs).

- [ ] **Step 5: Run tsc to spot any TS errors that reference removed commands**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: clean. The `invoke<...>("join_voice", ...)` calls in `tauri-bridge.ts`, if any, may have lost their Rust counterparts — if `tsc` flags them, leave them as-is (they're untyped string args; tsc accepts them). The runtime failure is acceptable for #2; #3 wires the new commands into actual usage.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs
git -C /home/deez/farder commit -m "feat(client): media-stream + key + lobby Tauri command stubs"
```

---

## Phase 7: Integration tests + smoke

## Task 13: Server integration tests

**Files:**
- Modify: existing test files under `crates/farder-server/tests/` (if any) OR `crates/farder-server/src/handlers.rs` test module

- [ ] **Step 1: Find existing voice integration tests**

```
grep -rn "JoinVoice\|StartVoice\|VoiceJoined\|VoiceCallIncoming" crates/farder-server/ --include="*.rs" | grep -v target
```

Note which tests need adaptation. The existing tests inside `handlers.rs::tests` mod (around lines 3030-3100) reference the old arms.

- [ ] **Step 2: Adapt or rewrite each affected test**

For each test that referenced the old voice arms, rewrite using the new media arms. Concretely:

- `JoinVoice { channel_id }` → `JoinChannelMedia { channel_id }`
- `StartVoice { channel_id }` → `JoinStream { channel_id }` (then `EnableTrack { Audio }`)
- The DM-ring test expecting `VoiceCallIncoming` → expect `StreamCallIncoming`

If the original tests covered:
- Lobby join/leave events → keep, adapted.
- DM-ring signal → keep, adapted.
- Voice transmit flow → adapt to JoinStream + EnableTrack + emitting a fake `MediaFrame` via `on_frame_ingress` and asserting the recipient list.

Write at least one NEW test:

```rust
#[test]
fn sealed_sender_no_pubkey_in_frame_routing() {
    // Build a server stream state with two sessions in the same channel.
    let mut state = crate::media_stream::StreamState::new();
    let config = crate::media_stream::MediaConfig::default();
    let alice_pk = farder_crypto::identity::PublicKey::from_bytes([0xaa; 32]);
    let bob_pk = farder_crypto::identity::PublicKey::from_bytes([0xbb; 32]);
    let alice_session = [1u8; 16];
    let bob_session = [2u8; 16];

    state.sessions.insert(alice_session, crate::media_stream::ServerSession {
        connection_token: 1, channel_id: 99,
        public_key: alice_pk.clone(),
        display_name: "alice".into(),
        active_tracks: [crate::media_stream::TrackKind::Audio].iter().cloned().collect(),
        buckets: std::collections::HashMap::new(),
        last_audio_frame_ms: None, last_video_frame_ms: None,
    });
    state.sessions.insert(bob_session, crate::media_stream::ServerSession {
        connection_token: 2, channel_id: 99,
        public_key: bob_pk.clone(),
        display_name: "bob".into(),
        active_tracks: [crate::media_stream::TrackKind::Audio].iter().cloned().collect(),
        buckets: std::collections::HashMap::new(),
        last_audio_frame_ms: None, last_video_frame_ms: None,
    });

    // Alice transmits an audio frame.
    let frame = crate::media_stream::build_media_frame(
        crate::media_stream::TrackKind::Audio,
        1, &alice_session, b"ciphertext-bytes",
    );

    // Crucially, the frame buffer is 28-byte-header + ciphertext.
    // The 28-byte header is: version | type | track | codec | seq | session_id.
    // It MUST NOT contain alice_pk's 32 bytes anywhere.
    let alice_pk_bytes = alice_pk.to_bytes();
    for window in frame.windows(32) {
        assert_ne!(window, &alice_pk_bytes,
            "frame must not contain alice's pubkey anywhere (sealed sender invariant)");
    }

    let decision = crate::media_stream::on_frame_ingress(&mut state, &config, 1, &frame, 0);
    match decision {
        crate::media_stream::IngressDecision::Forward { recipients } => {
            assert_eq!(recipients, vec![bob_session]);
        }
        _ => panic!("expected Forward"),
    }
}
```

- [ ] **Step 3: Run the workspace tests**

```
cd /home/deez/farder && cargo test --workspace 2>&1 | tail -25
```

Expected: green. If any pre-existing voice-related test was deleted in Task 11 and references a removed name, fix it here.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add -A
git -C /home/deez/farder commit -m "test(server): adapt voice integration tests + sealed-sender invariant"
```

---

## Task 14: Final smoke + workspace verification

**Files:**
- None (verification only)

- [ ] **Step 1: Full workspace cargo check**

```
cd /home/deez/farder && cargo check --workspace 2>&1 | tail -10
```

Expected: `Finished`. Warnings about dead code or unused fields are acceptable.

- [ ] **Step 2: Full workspace test**

```
cd /home/deez/farder && cargo test --workspace 2>&1 | tail -20
```

Expected: every test passes. The new test count vs pre-#2:
- +9 `farder-crypto::media::tests` (3 wrap/unwrap + 6 seal/open)
- +17 `farder-server::media_stream::tests` (6 frame + 3 bucket + 5 ingress + 3 activity)
- +10 protocol roundtrip variants
- Plus integration tests adapted in Task 13

- [ ] **Step 3: Client tsc clean**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -5
```

Expected: no errors.

- [ ] **Step 4: No CHANGELOG entry**

This sub-project ships infrastructure with no user-visible behavior change. CHANGELOG entry waits for #3 (voice client) to exercise the abstraction end-to-end.

- [ ] **Step 5: No final commit**

Steps 1-3 are read-only verifications.

---

## Self-review notes

- **Spec coverage:**
  - Protocol arm changes (removed + added) → Tasks 3 + 11
  - Lobby-arm renames → Tasks 3 (add Media variants) + 11 (remove Voice variants)
  - Event changes → Tasks 4 + 11
  - Frame format (28-byte header + AEAD) → Task 5 (parse/build) + Task 2 (seal/open helpers)
  - Token bucket → Task 6
  - StreamState / ServerSession → Task 7
  - Frame ingress + fanout routing → Task 8
  - Speaking ticker generalized → Task 9
  - Server handlers → Task 10 (add) + Task 11 (remove old)
  - Crypto helpers → Tasks 1 + 2
  - Client bridge stubs → Task 12
  - Integration tests → Task 13

- **Type consistency:**
  - `SessionId` is `[u8; 16]` — used identically in protocol crate (events), crypto (`seal_media_frame`), server (`media_stream.rs`), and client commands.
  - `TrackKind` lives in `farder-protocol::server`; used by both crates.
  - `MediaFrame.ciphertext` is opaque to the server — server NEVER calls `open_media_frame`. Only clients do.

- **Placeholder scan:** No "TBD" / "fill in details". Task 10 uses `unimplemented!()` MARKERS as deliberate signposts that the implementer must copy the corresponding voice-arm body and adapt — the plan provides the concrete recipe for each rather than ~300 lines of repeated bookkeeping code.

- **Workspace stays green commit-to-commit:** by adding new arms in Tasks 3-4 BEFORE removing old arms in Task 11, every intermediate commit compiles. Task 10 fills in the new handlers so by Task 11 they're available as the old arms get deleted.

- **No CHANGELOG entry intentional:** matches sub-project #1 (MediaBackend) pattern — infrastructure-only changes wait for the feature sub-project to write a single user-visible CHANGELOG entry.
