# Media Stream Transport — Design

**Status:** Drafted 2026-05-25 · Revised 2026-05-25 to add E2EE media + sealed-sender frame format
**Scope:** Farder protocol (`crates/farder-protocol`), server media routing (`crates/farder-server`), and client IPC bridge stubs (`client/src-tauri`). No client-side capture/playback or codec work — that ships with sub-project #3 (voice) and #4 (screensharing).
**Position in roadmap:** Sub-project #2 of the audio + screensharing track. Generalizes the existing voice-only protocol into a typed, end-to-end encrypted media-stream layer.

## Goal

Replace the voice-only `VC.*` protocol arms with a stream-of-tracks model: one logical media stream per user per channel, carrying independently-controlled `Audio` and `Video` tracks. The transport is **end-to-end encrypted** (the server sees ciphertext only) and **sealed-sender** at the frame level (the server routes by an opaque per-stream session token, not by the sender's pubkey). Server stays a dumb forwarder but gains per-(session, track) token-bucket bandwidth caps so a single misbehaving client can't saturate everyone.

## Non-Goals

- **Identity-blind QUIC handshake.** The server still knows which long-term pubkey opened each connection (auth via Ed25519, as today). A determined server operator who actively correlates "connection X auth'd as pubkey Y, session ABC belongs to connection X" can still link sessions to identities. Defending against that requires identity-blind connection auth — deferred to a future hardening sub-project. The frame-level sealed-sender we ship in v1 protects against:
  - Passive log inspection (logs never contain per-frame pubkeys)
  - Side-channel observers (process memory dumps, packet captures outside the server) who don't have the QUIC auth ledger
  - Accidental disclosure (no `for log in logs.grep("pubkey:")` works for media)
- **Forward secrecy for old captured frames.** Per-stream symmetric keys are ephemeral (regenerated on stream start), but we don't ratchet within a stream — if a key leaks, the whole stream is exposed retroactively. Adding double-ratchet here is overkill for real-time media; v1 accepts this.
- **Metadata anonymity.** The server sees "channel X has N transmitting sessions at rate R" — counts, timing, and channel membership are visible. Hiding those needs onion routing — way out of scope.
- **Codec implementations.** Opus encode/decode ships with sub-project #3 (voice). VP8 encode/decode ships with sub-project #4 (screensharing). v1 of the transport routes opaque ciphertext payloads.
- **Client capture/playback wiring.** The MediaBackend traits from sub-project #1 get consumed by #3 and #4; #2 just exposes the Tauri command surface those sub-projects call.
- **Reverse-mute** (this client doesn't want to receive a specific peer's audio). Out of scope; deafen is the only client-side receive control.
- **Codec negotiation.** Audio always Opus, video always VP8 in v1. `codec_id` byte is reserved.
- **Multi-track per kind.** One audio + one video per user per channel. `track_id` byte is reserved.
- **Per-peer bandwidth feedback / SFU-style adaptive routing.** Static per-server config caps.
- **Wire compatibility with the existing voice frame format** (version `0x01`). No deployed clients use Phase 3.

## Privacy model (what the server sees / doesn't see)

| Server sees | Server does NOT see |
|---|---|
| Which users are members of which channels | The plaintext audio (Opus) or video (VP8) |
| Which connections currently have a stream open in a channel | Per-frame: who actually sent it (frame carries an opaque `session_id`, not a pubkey) |
| Per-(connection, track) bandwidth / frame timing | Whether two sessions belong to the same user (different randomized session_ids) |
| Aggregate "someone is transmitting in channel X" presence | Identifying content if its logs are inspected without joining the auth ledger to the routing ledger |
| Each connection's authenticated pubkey at connection-setup time | (See non-goals: linking session → connection → pubkey still possible with active correlation) |

Key derivation and key exchange happen entirely client-to-client through the existing Farder E2EE message channel (used today for DMs). The server is never trusted with media-payload keys.

## Architecture

```
┌─── Client ──────────────────────────────────┐    ┌─── Server ────────────────────────────────┐
│                                              │    │                                            │
│  Tauri commands:                             │    │  crates/farder-server/src/                 │
│    join_stream(channel_id) -> session_id     │    │    media_stream.rs                         │
│    leave_stream()                            │    │                                            │
│    enable_track(kind)                        │    │      - parse_media_frame /                 │
│    disable_track(kind)                       │    │        build_media_frame                   │
│    set_deafen(deafened)                      │    │        (28-byte header + AEAD payload)     │
│    offer_stream_key(...)                     │    │      - session_id → connection map         │
│    get_stream_state(channel_id)              │    │      - token bucket per (session, kind)    │
│                                              │    │      - fanout: route by session_id to all  │
│  Crypto helpers (client-only):               │    │        OTHER sessions in same channel      │
│    derive_stream_key() -> ChaCha20Key        │    │        that are joined and not deafened    │
│    wrap_key_for_peer(key, peer_pk)           │    │      - lifecycle events                    │
│    unwrap_key_for_self(wrapped)              │    │                                            │
│    seal_frame(key, plaintext, seq, sess_id)  │    │    handlers.rs                             │
│    open_frame(key, ciphertext)               │    │      - new arms: JoinStream, LeaveStream,  │
│                                              │    │        EnableTrack, DisableTrack,          │
│  Per-stream state:                           │    │        SetDeafen, OfferStreamKey           │
│    - session_id (from server)                │    │      - StreamKeyOffer is server-fanned-out │
│    - own ChaCha20 key (if transmitting)      │    │        to the specific recipient(s) the    │
│    - peers' keys (received via               │    │        sender targeted                     │
│      StreamKeyOffer events, decrypted        │    │                                            │
│      with own identity key)                  │    │  Server has NO access to ChaCha20 keys —   │
│                                              │    │  they're exchanged peer-to-peer via the    │
│                                              │    │  existing DM E2EE channel (X25519 ECDH +   │
│                                              │    │  AES-GCM, same as Farder's message E2EE)   │
└──────────────────────────────────────────────┘    └────────────────────────────────────────────┘
```

### Why one stream per user per channel

User confirmed during brainstorming: "voice and video are pretty much always used together." A stream-of-tracks model lets `JoinStream` claim the slot once, then `EnableTrack` toggles each track independently. One join, one leave, one source of truth.

### Why E2EE + sealed sender now (not later)

We're about to bake in the frame format. Doing crypto + session_id now is one wire break (already breaking `0x01 → 0x02`); deferring means another wire break later. The implementation cost is ~30% on top of plain transport — well worth it for a self-hosted "privacy-centric" product where the operator may not be the user.

### Why ChaCha20-Poly1305

Same AEAD primitive that's used pervasively for real-time media (WireGuard, QUIC's default cipher). Fast in software (no hardware AES dependency), 256-bit key, 16-byte tag, ~50 ns/frame on modern CPUs. The `ring` crate is already a Farder dep via rustls.

## Protocol — `ServerRequest`

### Removed

```rust
StartVoice { channel_id: u64 }
StopVoice
SetVoiceMute { muted: bool }
SetVoiceDeafen { deafened: bool }
```

### Added

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Audio,
    Video,
}

/// Claim a media-stream slot in `channel_id`. The server responds with
/// `ServerResponse::StreamSessionStarted { session_id: [u8; 16] }` carrying
/// a fresh random session_id that the client uses in subsequent media frames.
/// The session_id is opaque — the server does NOT derive it from the user's
/// pubkey, and successive JoinStream calls by the same user yield fresh ids.
JoinStream { channel_id: u64 }

/// Release the stream. Server disables all tracks and emits StreamLeft to peers.
LeaveStream

/// Mark a track as active. Server begins fanning out frames of this kind from
/// this session to peers (subject to bandwidth caps). Frames sent BEFORE the
/// corresponding EnableTrack are silently dropped.
EnableTrack { kind: TrackKind }

/// Mark a track as inactive. Stops fanout of this kind from this session.
DisableTrack { kind: TrackKind }

/// Receive-side: while deafened, server suppresses fanout TO this session.
SetDeafen { deafened: bool }

/// Distribute the ChaCha20 key for this session's track to the specified peers.
/// The server forwards each (peer_pk, wrapped_key) pair as a `StreamKeyOffer`
/// event to that peer ONLY. Server never sees the unwrapped key.
///
/// `wrapped_keys` is keyed by recipient pubkey; each value is the symmetric
/// stream-key encrypted to that peer using the existing Farder DM E2EE wrap
/// (X25519 ECDH + AES-GCM, with the existing `dm_encrypt` Tauri command's
/// underlying primitive).
OfferStreamKey {
    kind: TrackKind,
    wrapped_keys: Vec<(PublicKey, Vec<u8>)>,
}
```

### Lobby-presence arms (renamed)

`JoinVoice` / `LeaveVoice` / `GetVoiceState` / `VoiceStateResp` are the "Alice is in the voice channel but hasn't started transmitting yet" arms — distinct from the transmission arms above. We rename them for consistency:

```rust
JoinChannelMedia { channel_id: u64 }    // was JoinVoice
LeaveChannelMedia { channel_id: u64 }   // was LeaveVoice
GetMediaState { channel_id: u64 }       // was GetVoiceState
// Response variant renames to MediaStateResp
```

Semantics unchanged.

### Mappings from old → new

| Old | New |
|---|---|
| `JoinVoice { ch }` | `JoinChannelMedia { ch }` |
| `StartVoice { ch }` | `JoinStream { ch }` → returns `session_id` → client calls `OfferStreamKey { Audio, … }` then `EnableTrack { Audio }` |
| `StopVoice` | `DisableTrack { Audio }` (keep stream open) OR `LeaveStream` (close everything) |
| `SetVoiceMute { true }` | `DisableTrack { Audio }` |
| `SetVoiceMute { false }` | `EnableTrack { Audio }` |
| `SetVoiceDeafen { d }` | `SetDeafen { d }` |
| `LeaveVoice { ch }` | `LeaveChannelMedia { ch }` |

## Protocol — `ServerEvent`

### Removed

```rust
VoiceJoined / VoiceLeft / VoiceSpeakingChanged
VoiceCallIncoming / VoiceCallEnded
```

### Added

```rust
/// Lobby presence (renamed from VoiceJoined/Left).
MediaJoined { channel_id: u64, public_key: PublicKey, display_name: String }
MediaLeft   { channel_id: u64, public_key: PublicKey }

/// A peer has claimed a transmission slot. `display_name` lets the
/// recipient cache (session_id → display_name) so subsequent track-activity
/// events can be shown without re-lookup.
StreamJoined {
    channel_id: u64,
    public_key: PublicKey,
    display_name: String,
    session_id: [u8; 16],
    active_tracks: Vec<TrackKind>,
}

/// Peer's stream is closed.
StreamLeft {
    channel_id: u64,
    session_id: [u8; 16],
}

/// Track-level lifecycle (peer toggled their audio/video on or off).
TrackEnabled  { channel_id: u64, session_id: [u8; 16], kind: TrackKind }
TrackDisabled { channel_id: u64, session_id: [u8; 16], kind: TrackKind }

/// Activity indicator (speaking for audio; frames-flowing for video).
/// Emitted by the 5 Hz ticker on transitions only.
TrackActivityChanged {
    channel_id: u64,
    session_id: [u8; 16],
    kind: TrackKind,
    active: bool,
}

/// Direct message ring (DM voice/video call).
StreamCallIncoming { channel_id: u64, caller: PublicKey, caller_name: String }
StreamCallEnded    { channel_id: u64 }

/// Targeted: delivered to the recipient peer only. Carries one wrapped
/// stream key from `OfferStreamKey`'s `wrapped_keys` map. The recipient
/// unwraps with their own identity key.
StreamKeyOffer {
    channel_id: u64,
    sender: PublicKey,          // who's offering — peer knows whose track to apply this to
    session_id: [u8; 16],       // which stream this key opens
    kind: TrackKind,
    wrapped_key: Vec<u8>,       // X25519 + AES-GCM ciphertext of the ChaCha20 key
}
```

Activity events reference `session_id` rather than `public_key`. The client maps `session_id → public_key` from the previously-received `StreamJoined` event. This means a server log of `TrackActivityChanged` events shows `session_id ABC is now speaking` — opaque to anyone reading the log without the StreamJoined index.

## Frame format (28-byte header + AEAD ciphertext)

```
Offset | Field         | Size  | Notes
-------+---------------+-------+--------------------------------------------------------------
 0     | version       | 1 B   | 0x02
 1     | type          | 1 B   | 0x01 = Opus audio, 0x02 = VP8 video (plaintext: server needs it for routing decisions like bandwidth-cap kind)
 2     | track_id      | 1 B   | RESERVED — always 0 in v1
 3     | codec_id      | 1 B   | RESERVED — always 0 in v1
 4-11  | seq           | 8 B   | u64 big-endian, monotonic per (session, kind), plaintext
 12-27 | session_id    | 16 B  | Opaque, server-allocated per stream
 28+   | ciphertext    | var   | ChaCha20-Poly1305 over (speaker_pk || codec_payload); 16-byte auth tag appended by AEAD
```

Total header: **28 bytes**. The pre-encryption design had a 44-byte plaintext header (which included the 32-byte `speaker_pk`). In the encrypted design, `speaker_pk` moves INTO the ciphertext (still 32 bytes) and an AEAD tag adds 16 bytes. Net per-frame overhead vs the plain design: **+16 bytes** (the AEAD tag). For a typical 20 ms Opus frame (~80 bytes payload), this is a 12% size increase. For a video frame (~1–10 KB), it's negligible. Acceptable cost for the privacy guarantees.

### Nonce derivation

ChaCha20-Poly1305 requires 12-byte unique nonces per key. Derive deterministically:

```rust
nonce[0..4] = session_id[0..4]   // tying nonce to session prevents cross-session collisions
nonce[4..12] = seq.to_be_bytes() // monotonic per stream
```

Unique by construction as long as `seq` doesn't wrap (it's u64 — 18 quintillion frames per stream, never an issue).

### AEAD inputs

- **Key:** the per-stream ChaCha20 key (32 bytes), known to all peers who received a valid `StreamKeyOffer`.
- **Nonce:** the 12 bytes derived above.
- **Associated data:** the 28 header bytes themselves (`version | type | track_id | codec_id | seq | session_id`). Binds the header to the ciphertext so the type or session_id can't be tampered with.
- **Plaintext:** `speaker_pk (32 B) || codec_payload`. The `speaker_pk` inside the ciphertext lets recipients verify which peer sent the frame (cross-checked against the `StreamJoined` event's `session_id → public_key` mapping).

### Rust types

```rust
// crates/farder-server/src/media_stream.rs

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
    /// The server NEVER decrypts this. The fields above are everything the
    /// server needs for routing.
    pub ciphertext: &'a [u8],
}

pub fn parse_media_frame(buf: &[u8]) -> Result<MediaFrame<'_>, MediaFrameError> {
    if buf.len() < MEDIA_FRAME_HEADER_LEN { return Err(MediaFrameError::TooShort); }
    if buf[0] != MEDIA_FRAME_VERSION { return Err(MediaFrameError::BadVersion(buf[0])); }
    let kind = match buf[1] {
        MEDIA_FRAME_TYPE_AUDIO => TrackKind::Audio,
        MEDIA_FRAME_TYPE_VIDEO => TrackKind::Video,
        other => return Err(MediaFrameError::BadType(other)),
    };
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
    buf.push(0); // track_id
    buf.push(0); // codec_id
    buf.extend_from_slice(&seq.to_be_bytes());
    buf.extend_from_slice(session_id);
    buf.extend_from_slice(ciphertext);
    buf
}

#[derive(Debug, PartialEq)]
pub enum MediaFrameError { TooShort, BadVersion(u8), BadType(u8) }
```

### Client-side crypto helpers

Live in `crates/farder-crypto` (where existing primitives sit) or a new `crates/farder-media-crypto` module. Decided in the implementation plan.

```rust
/// Generate a fresh 32-byte ChaCha20-Poly1305 key.
pub fn derive_stream_key() -> [u8; 32];

/// Wrap (encrypt-to-peer) the stream key for delivery via OfferStreamKey.
/// Uses the existing DM E2EE wrap: X25519 ECDH(my_kp, peer_pk) → AES-GCM key.
pub fn wrap_stream_key_for_peer(
    stream_key: &[u8; 32],
    my_kp: &Keypair,
    peer_pk: &PublicKey,
) -> Vec<u8>;

/// Unwrap a key delivered to me via StreamKeyOffer.
pub fn unwrap_stream_key(
    wrapped: &[u8],
    my_kp: &Keypair,
    sender_pk: &PublicKey,
) -> Result<[u8; 32], CryptoError>;

/// Encrypt a media frame plaintext (speaker_pk || codec_payload) under the
/// stream key, with nonce derived from (session_id, seq) and AAD = header bytes.
/// Returns ciphertext (includes 16-byte AEAD tag).
pub fn seal_media_frame(
    key: &[u8; 32],
    seq: u64,
    session_id: &SessionId,
    header_aad: &[u8],   // the 28 header bytes
    speaker_pk: &PublicKey,
    codec_payload: &[u8],
) -> Vec<u8>;

/// Decrypt and verify. Returns (speaker_pk, codec_payload).
pub fn open_media_frame(
    key: &[u8; 32],
    seq: u64,
    session_id: &SessionId,
    header_aad: &[u8],
    ciphertext: &[u8],
) -> Result<(PublicKey, Vec<u8>), CryptoError>;
```

These helpers are called by sub-projects #3 and #4 (the actual encoders/decoders). #2 just defines and tests them.

## Server routing — `media_stream.rs`

### State per channel

```rust
pub struct StreamState {
    // session_id → metadata for active streams
    sessions: HashMap<SessionId, ServerSession>,
    // connection_id → set of (channel_id, session_id) so we can clean up on disconnect
    by_connection: HashMap<ConnectionId, HashSet<SessionId>>,
    // session_id → deafened flag (deafen blocks RECEIVE fanout to this session)
    deafened: HashSet<SessionId>,
}

pub struct ServerSession {
    connection_id: ConnectionId,
    channel_id: u64,
    public_key: PublicKey,   // bound from the JoinStream call; used for StreamJoined
                              // event payloads but NEVER written to media-frame logs
    display_name: String,
    active_tracks: HashSet<TrackKind>,
    buckets: HashMap<TrackKind, TokenBucket>,
    last_audio_frame_ms: Option<u64>,
    last_video_frame_ms: Option<u64>,
}
```

The server **does** know `session_id → public_key` internally (it has to, to emit `StreamJoined` events). But media-frame routing only references `session_id`. The pubkey doesn't appear in the per-frame hot path or in frame-rate-of logs.

### Frame ingress flow

1. Datagram arrives on connection `C`, parsed into `MediaFrame { kind, seq, session_id, ciphertext }`.
2. Look up `session_id` in `sessions`. If not found → drop silently.
3. **Authenticate**: confirm `sessions[session_id].connection_id == C`. Mismatch → drop (defends against a malicious peer spoofing someone else's `session_id`).
4. If `kind` not in `active_tracks` for this session → drop.
5. Refill the session's `(kind)` token bucket.
6. If bucket dry → drop, increment ops counter, log to ops.
7. Update `last_(audio|video)_frame_ms`.
8. **Fanout**: for every OTHER session in the same channel that is not deafened, write the frame bytes (verbatim — server doesn't modify ciphertext or header) to that session's connection.

### Key-distribution flow

1. Sender generates a per-stream key client-side: `let key = derive_stream_key();`
2. For each peer the sender wants to be able to decrypt (everyone else in the channel, looked up via the lobby presence list), wrap the key: `wrap_stream_key_for_peer(&key, &my_kp, &peer_pk)`.
3. Sender calls `OfferStreamKey { kind, wrapped_keys: Vec<(PublicKey, Vec<u8>)> }`.
4. Server looks up which session each `peer_pk` corresponds to in this channel. For each (peer_pk, wrapped) pair, emits a `StreamKeyOffer { channel_id, sender: my_pk, session_id: my_session, kind, wrapped_key }` event to that peer's connection.
5. Recipient receives `StreamKeyOffer`, unwraps with their identity key, stores `(sender_session_id, kind) → ChaCha20 key` in client-side state.
6. Sender can now `EnableTrack { kind }` and start emitting encrypted frames; recipients have the key.

### Key rotation

When a peer joins the lobby (`MediaJoined` event) AFTER a stream is open, the sender re-runs key generation + distribution. New key. Old key is retired.

When a peer leaves (`MediaLeft`), the sender SHOULD rotate to deprive the departing peer of decrypt capability on subsequent frames. This is the "forward secrecy on membership change" property. Implementation detail in the plan.

Within a stream, no rotation between membership changes — accepted limitation (cf. Non-Goals).

### Speaking / activity ticker

Same 5 Hz cadence as today. Walks each session's `last_(audio|video)_frame_ms`. On transitions, emits `TrackActivityChanged { session_id, kind, active }` to every other session in the channel. (Plus to the sender themselves, so they can show their own talk indicator — same as today.)

### Bandwidth defaults

```toml
[media]
audio_max_bps = 64000       # 64 Kbps
video_max_bps = 2_000_000   # 2 Mbps
```

Per-(session, kind) token bucket. Empty bucket → drop frame silently + increment ops counter.

## Client-side bridge stubs

`client/src-tauri/src/commands.rs` gains:

```rust
#[tauri::command] pub async fn join_stream(serverId, channelId) -> Result<SessionId, String>;
#[tauri::command] pub async fn leave_stream(serverId) -> Result<(), String>;
#[tauri::command] pub async fn enable_track(serverId, kind: String) -> Result<(), String>;
#[tauri::command] pub async fn disable_track(serverId, kind: String) -> Result<(), String>;
#[tauri::command] pub async fn set_deafen(serverId, deafened: bool) -> Result<(), String>;
#[tauri::command] pub async fn offer_stream_key(serverId, kind: String, wrapped_keys: Vec<(PublicKey, Vec<u8>)>) -> Result<(), String>;
#[tauri::command] pub async fn get_stream_state(serverId, channelId) -> Result<StreamStateResp, String>;
```

These are pure pass-throughs to the existing `bridge::send_request` machinery. The TS side doesn't change in this sub-project — the actual hookup to a `StreamControlPanel` UI lives in #3 / #4.

The existing `join_voice` / `leave_voice` Tauri commands rename to `join_channel_media` / `leave_channel_media` (lobby presence). `start_recording` / `stop_recording` / `save_temp_audio` are unrelated local-recording commands — preserved as-is.

## Testing

### Unit — `crates/farder-server/src/media_stream.rs`

- `parse_media_frame_audio_roundtrip` — build + parse, fields match.
- `parse_media_frame_video_roundtrip` — same for video.
- `parse_media_frame_rejects_voice_v1` — `version=0x01` returns `Err(BadVersion(0x01))`.
- `parse_media_frame_rejects_unknown_type` — `Err(BadType(...))`.
- `parse_media_frame_rejects_short_buffer` — `Err(TooShort)`.
- `token_bucket_passes_under_cap` — frames at 50% cap, all admitted.
- `token_bucket_drops_over_cap` — frames at 200% cap, ~50% dropped.
- `token_bucket_refills_over_time` — drain, wait 100 ms, refilled by `cap_bps * 0.1`.
- `session_spoof_rejected` — frame with mismatched session/connection is dropped.

### Unit — crypto helpers

- `seal_open_roundtrip_audio` — seal a plaintext, open with same key/nonce, plaintext matches.
- `open_rejects_tampered_ciphertext` — flip a byte in ciphertext, `open_media_frame` returns Err.
- `open_rejects_tampered_aad` — flip a byte in the header AAD, `open_media_frame` returns Err.
- `open_rejects_wrong_key` — random other key → Err.
- `nonce_derivation_unique` — `(session_id, seq) → nonce` collision-free under property test.
- `wrap_unwrap_roundtrip` — wrap key for peer with my_kp, unwrap with peer's perspective, key matches.
- `unwrap_rejects_wrong_recipient` — wrap for peer A, try unwrap with peer B's key → Err.

### Unit — `crates/farder-protocol/src/server.rs`

- Adapt existing `test_roundtrip_client_frame_request` to cover the new arms (`JoinStream`, `EnableTrack`, `OfferStreamKey`, etc.).
- Adapt event roundtrip to cover new events including `StreamKeyOffer`.

### Integration

- `stream_join_leave` — replaces existing voice_join_leave; lifecycle events fire correctly.
- `audio_only_to_audio_plus_video` — `JoinStream → OfferStreamKey(Audio) → EnableTrack(Audio) → OfferStreamKey(Video) → EnableTrack(Video) → DisableTrack(Video) → LeaveStream`. Events fire in the right order.
- `bandwidth_cap_drops_video_under_load` — verifies token-bucket drop path.
- `sealed_sender_no_pubkey_in_frame_log` — start a stream, capture server's media-frame log, assert no instance of any participant pubkey hex appears in the log (only `session_id` hex).
- `key_offer_targeted_delivery` — sender calls OfferStreamKey with peers A and B but not C. C does not receive a StreamKeyOffer event.

### What's NOT tested in #2

- Actual codec output — sub-project #3 (Opus) and #4 (VP8) bring their own tests against synthetic input from MediaBackend mocks.
- End-to-end media smoke between two clients with real audio/video.

## Migration

- **Server:** `voice.rs` renamed to `media_stream.rs` with full routing rewrite (now sealed-sender). Bandwidth-cap machinery is new. 5 Hz speaking ticker generalizes. Voice arm handlers in `handlers.rs` are replaced with media arm handlers. `EventTarget::Voice*` variants rename to `EventTarget::Media*` 1:1.
- **Protocol:** Voice arms and events removed. New media arms + events added (including the new `OfferStreamKey` request + `StreamKeyOffer` event).
- **Client (Rust):** Tauri commands renamed; new `offer_stream_key` command + crypto helpers in `crates/farder-crypto` (or new module).
- **Frame wire format:** `0x01` rejected; clients must speak `0x02`. No deployed clients use `0x01`.
- **Identity key wrap:** the X25519+AES-GCM primitive Farder already uses for DM messages (`dm_encrypt` / `dm_decrypt` Tauri commands) is reused via thin wrapper helpers. No new crypto primitives.

## Rollout

Infrastructure sub-project — no UI behavior change. After landing:
- Voice (#3) builds the Opus encode/decode + audio-track lifecycle UX (talk indicator, mute, deafen) on top of these arms.
- Screensharing (#4) builds VP8 + video-track lifecycle UX (share screen / stop sharing / viewer) on top.
- Both consume the same `JoinStream` / `EnableTrack` / `OfferStreamKey` / frame-transport machinery.

## Future considerations (deferred)

- **Identity-blind QUIC handshake** — closes the "server knows which connection is which pubkey" gap. Future hardening sub-project.
- **Per-stream forward-secrecy ratchet** — re-key every N frames within a single stream.
- **Multi-track per kind** (`track_id != 0`) — camera + screenshare at the same time.
- **Codec negotiation** (`codec_id != 0`) — Opus → AAC, VP8 → AV1.
- **Reverse-mute** — client-only UX, no protocol impact.
- **Per-peer bandwidth feedback** — proper SFU territory.
- **Recording-to-file of incoming streams** — separate spec.

---

This spec covers exactly the protocol shape, encrypted frame format, sealed-sender routing, key-exchange flow, server bandwidth caps, and bridge-stub renames for sub-project #2.
