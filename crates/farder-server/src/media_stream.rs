// crates/farder-server/src/media_stream.rs
//
// Generalized media-stream routing. Replaces the voice-only `voice.rs`
// fanout machinery (deleted in MST-11) with a typed Audio+Video transport.
//
// Per spec (2026-05-25-media-stream-transport-design.md): server sees
// ciphertext only; routes by opaque session_id; per-(session, kind)
// token-bucket bandwidth caps.
//
// Wire-format layout constants (MEDIA_FRAME_VERSION, MEDIA_FRAME_TYPE_AUDIO,
// MEDIA_FRAME_TYPE_VIDEO, MEDIA_FRAME_HEADER_LEN), SessionId/SESSION_ID_LEN,
// MediaFrameError, and media_frame_header_aad now live in farder-crypto::media
// and are re-exported here so existing server-side consumers compile unchanged.

use farder_protocol::server::TrackKind;

// Re-export shared layout constants and types from farder-crypto.
pub use farder_crypto::media::{
    MEDIA_FRAME_VERSION,
    MEDIA_FRAME_TYPE_AUDIO,
    MEDIA_FRAME_TYPE_VIDEO,
    MEDIA_FRAME_HEADER_LEN,
    SESSION_ID_LEN,
    SessionId,
    MediaFrameError,
    media_frame_header_aad,
};

#[derive(Debug, PartialEq)]
pub struct MediaFrame<'a> {
    pub kind: TrackKind,
    pub seq: u64,
    pub session_id: SessionId,
    /// Opaque AEAD ciphertext (includes the 16-byte authenticator tag).
    /// The server NEVER decrypts this.
    pub ciphertext: &'a [u8],
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
        TrackKind::ScreenAudio => MEDIA_FRAME_TYPE_AUDIO, // inner frame == audio crypto
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

use std::collections::{HashMap, HashSet};
use farder_crypto::identity::PublicKey;

/// Per-channel state for the media-stream router. The server keeps one of
/// these per active voice/media channel.
pub struct StreamState {
    /// session_id → metadata for active streams
    pub sessions: HashMap<SessionId, ServerSession>,
    /// session_id → deafened flag (suppresses fanout TO this session)
    pub deafened: HashSet<SessionId>,
    /// session_id → self-muted flag (display-only; relayed to peers)
    pub muted: HashSet<SessionId>,
}

pub struct ServerSession {
    /// The QUIC connection that owns this session. Used to authenticate
    /// frames (a frame's session_id must match the receiving connection).
    pub connection_pk: [u8; 32],
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
    pub last_screen_audio_frame_ms: Option<u64>,
}

impl StreamState {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            deafened: HashSet::new(),
            muted: HashSet::new(),
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

pub struct MediaConfig {
    pub audio_max_bps: u64,
    pub video_max_bps: u64,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            audio_max_bps: 64_000,
            video_max_bps: 8_000_000,
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
    sending_connection_pk: &[u8; 32],
    raw: &[u8],
    now_ms: u64,
) -> IngressDecision {
    let (header, _payload) = match farder_protocol::media_datagram::OuterHeader::parse(raw) {
        Ok(h) => h,
        Err(_e) => return IngressDecision::Drop(DropReason::ParseError(MediaFrameError::TooShort)),
    };

    let session = match state.sessions.get_mut(&header.session_id) {
        Some(s) => s,
        None => return IngressDecision::Drop(DropReason::UnknownSession),
    };

    if session.connection_pk != *sending_connection_pk {
        return IngressDecision::Drop(DropReason::SessionConnectionMismatch);
    }
    if !session.active_tracks.contains(&header.track_kind) {
        return IngressDecision::Drop(DropReason::TrackNotEnabled);
    }

    let cap = match header.track_kind {
        TrackKind::Audio => config.audio_max_bps,
        TrackKind::Video => config.video_max_bps,
        TrackKind::ScreenAudio => config.audio_max_bps,
    };
    let bucket = session.buckets.entry(header.track_kind).or_insert_with(|| TokenBucket::new(cap));

    if !bucket.try_consume(raw.len() as u64) {
        return IngressDecision::Drop(DropReason::BandwidthCap);
    }

    match header.track_kind {
        TrackKind::Audio => session.last_audio_frame_ms = Some(now_ms),
        TrackKind::Video => session.last_video_frame_ms = Some(now_ms),
        TrackKind::ScreenAudio => session.last_screen_audio_frame_ms = Some(now_ms),
    }

    let channel_id = session.channel_id;
    let sender_session = header.session_id;
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
        for kind in [TrackKind::Audio, TrackKind::Video, TrackKind::ScreenAudio] {
            if !session.active_tracks.contains(&kind) {
                continue;
            }
            let last_ms = match kind {
                TrackKind::Audio => session.last_audio_frame_ms,
                TrackKind::Video => session.last_video_frame_ms,
                TrackKind::ScreenAudio => session.last_screen_audio_frame_ms,
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

use std::sync::RwLock as StdRwLock;

/// Process-wide map of per-channel media stream state.
/// One `StreamState` per active voice/media channel.
pub struct MediaStateMap {
    pub channels: StdRwLock<HashMap<u64, StreamState>>,
}

impl MediaStateMap {
    pub fn new() -> Self {
        Self {
            channels: StdRwLock::new(HashMap::new()),
        }
    }
}

impl Default for MediaStateMap {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_session() -> SessionId {
        [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
    }

    use farder_protocol::media_datagram::OuterHeader;

    /// Build a single-fragment outer media datagram carrying `ciphertext`
    /// (the wire format the server now routes on).
    fn outer_dgram(kind: TrackKind, session: &SessionId, ciphertext: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        OuterHeader {
            track_kind: kind,
            session_id: *session,
            frame_id: 0,
            frag_index: 0,
            frag_count: 1,
        }
        .write_to(&mut v);
        v.extend_from_slice(ciphertext);
        v
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
        buf[0] = 0x01;
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

    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn bucket_admits_under_cap() {
        let mut b = TokenBucket::new(10_000); // 10 KB/s, max 5000 tokens
        assert!(b.try_consume(1000));
        assert!(b.try_consume(1000));
    }

    #[test]
    fn bucket_drops_when_drained() {
        let mut b = TokenBucket::new(1000); // 1 KB/s, max 500 tokens
        assert!(b.try_consume(500));
        assert!(!b.try_consume(1000), "second large consume should be denied");
    }

    #[test]
    fn bucket_refills_over_time() {
        let mut b = TokenBucket::new(10_000);
        while b.try_consume(100) {}
        sleep(Duration::from_millis(100));
        assert!(b.try_consume(500),
            "should have refilled at least 500 bytes after 100ms at 10KB/s");
    }

    use farder_crypto::identity::PublicKey;

    fn fake_pubkey(n: u8) -> PublicKey {
        PublicKey::from_bytes([n; 32])
    }

    fn install_session(
        state: &mut StreamState,
        session_id: SessionId,
        connection_pk: [u8; 32],
        channel_id: u64,
        with_audio: bool,
    ) {
        let mut tracks = HashSet::new();
        if with_audio { tracks.insert(TrackKind::Audio); }
        state.sessions.insert(session_id, ServerSession {
            connection_pk,
            channel_id,
            public_key: fake_pubkey(connection_pk[0]),
            display_name: format!("user{}", connection_pk[0]),
            active_tracks: tracks,
            buckets: HashMap::new(),
            last_audio_frame_ms: None,
            last_video_frame_ms: None,
            last_screen_audio_frame_ms: None,
        });
    }

    #[test]
    fn ingress_drops_unknown_session() {
        let mut state = StreamState::new();
        let config = MediaConfig::default();
        let bogus = [99u8; 16];
        let frame = outer_dgram(TrackKind::Audio, &bogus, b"x");
        match on_frame_ingress(&mut state, &config, &[1u8; 32], &frame, 0) {
            IngressDecision::Drop(DropReason::UnknownSession) => {}
            _ => panic!("expected UnknownSession drop"),
        }
    }

    #[test]
    fn ingress_drops_session_connection_mismatch() {
        let mut state = StreamState::new();
        let config = MediaConfig::default();
        let session = [1u8; 16];
        install_session(&mut state, session, [1u8; 32], 100, true);
        let frame = outer_dgram(TrackKind::Audio, &session, b"x");
        match on_frame_ingress(&mut state, &config, &[2u8; 32], &frame, 0) {
            IngressDecision::Drop(DropReason::SessionConnectionMismatch) => {}
            _ => panic!("expected SessionConnectionMismatch drop"),
        }
    }

    #[test]
    fn ingress_drops_track_not_enabled() {
        let mut state = StreamState::new();
        let config = MediaConfig::default();
        let session = [1u8; 16];
        install_session(&mut state, session, [1u8; 32], 100, false); // no audio
        let frame = outer_dgram(TrackKind::Audio, &session, b"x");
        match on_frame_ingress(&mut state, &config, &[1u8; 32], &frame, 0) {
            IngressDecision::Drop(DropReason::TrackNotEnabled) => {}
            _ => panic!("expected TrackNotEnabled drop"),
        }
    }

    #[test]
    fn ingress_forwards_to_other_sessions_in_channel() {
        let mut state = StreamState::new();
        let config = MediaConfig::default();
        let sender = [1u8; 16];
        let other = [2u8; 16];
        let other_channel = [3u8; 16];
        install_session(&mut state, sender, [1u8; 32], 100, true);
        install_session(&mut state, other, [2u8; 32], 100, true);
        install_session(&mut state, other_channel, [3u8; 32], 999, true);
        let frame = outer_dgram(TrackKind::Audio, &sender, b"x");
        match on_frame_ingress(&mut state, &config, &[1u8; 32], &frame, 0) {
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
        install_session(&mut state, sender, [1u8; 32], 100, true);
        install_session(&mut state, other, [2u8; 32], 100, true);
        state.deafened.insert(other);
        let frame = outer_dgram(TrackKind::Audio, &sender, b"x");
        match on_frame_ingress(&mut state, &config, &[1u8; 32], &frame, 0) {
            IngressDecision::Forward { recipients } => assert!(recipients.is_empty()),
            _ => panic!("expected Forward"),
        }
    }

    #[test]
    fn activity_transition_inactive_to_active() {
        let mut state = StreamState::new();
        let session = [1u8; 16];
        install_session(&mut state, session, [1u8; 32], 100, true);
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
        install_session(&mut state, session, [1u8; 32], 100, true);
        state.sessions.get_mut(&session).unwrap().last_audio_frame_ms = Some(0);
        let mut prev = HashMap::new();
        prev.insert((session, TrackKind::Audio), true);
        // 500ms after last frame — should be inactive (threshold 300ms)
        let (transitions, _new) = compute_activity_transitions(&state, &prev, 500);
        assert_eq!(transitions.len(), 1);
        assert!(!transitions[0].active);
    }

    #[test]
    fn activity_no_transition_when_state_stable() {
        let mut state = StreamState::new();
        let session = [1u8; 16];
        install_session(&mut state, session, [1u8; 32], 100, true);
        state.sessions.get_mut(&session).unwrap().last_audio_frame_ms = Some(1000);
        let mut prev = HashMap::new();
        prev.insert((session, TrackKind::Audio), true);
        let (transitions, _new) = compute_activity_transitions(&state, &prev, 1100);
        assert!(transitions.is_empty(), "no transition expected");
    }

    /// Sealed-sender invariant: a media frame's plaintext header bytes
    /// must NEVER contain any participant's pubkey.
    ///
    /// This is the key privacy guarantee — server logs at the frame-routing
    /// layer should never expose who-said-what. The speaker_pk is inside
    /// the encrypted ciphertext (verified via the AEAD), not in the routing
    /// header.
    #[test]
    fn sealed_sender_no_pubkey_in_frame_header() {
        let mut state = StreamState::new();
        let config = MediaConfig::default();
        let alice_pk = farder_crypto::identity::PublicKey::from_bytes([0xaa; 32]);
        let bob_pk = farder_crypto::identity::PublicKey::from_bytes([0xbb; 32]);
        let alice_session = [1u8; 16];
        let bob_session = [2u8; 16];

        // Install Alice and Bob in the same channel; both have Audio enabled.
        let alice_conn = [0xaa; 32];
        let bob_conn = [0xbb; 32];
        let mut alice_tracks = HashSet::new();
        alice_tracks.insert(TrackKind::Audio);
        state.sessions.insert(alice_session, ServerSession {
            connection_pk: alice_conn,
            channel_id: 99,
            public_key: alice_pk.clone(),
            display_name: "alice".into(),
            active_tracks: alice_tracks,
            buckets: HashMap::new(),
            last_audio_frame_ms: None,
            last_video_frame_ms: None,
            last_screen_audio_frame_ms: None,
        });
        let mut bob_tracks = HashSet::new();
        bob_tracks.insert(TrackKind::Audio);
        state.sessions.insert(bob_session, ServerSession {
            connection_pk: bob_conn,
            channel_id: 99,
            public_key: bob_pk.clone(),
            display_name: "bob".into(),
            active_tracks: bob_tracks,
            buckets: HashMap::new(),
            last_audio_frame_ms: None,
            last_video_frame_ms: None,
            last_screen_audio_frame_ms: None,
        });

        // Alice builds an audio frame. The ciphertext bytes here are opaque
        // (representing what the AEAD would produce in real flow).
        let frame = outer_dgram(TrackKind::Audio, &alice_session, b"opaque-ciphertext-bytes");

        // The outer header MUST NOT contain Alice's pubkey anywhere.
        use farder_protocol::media_datagram::MEDIA_DGRAM_HEADER_LEN;
        let header = &frame[..MEDIA_DGRAM_HEADER_LEN];
        let alice_pk_bytes = alice_pk.as_bytes();
        for window_start in 0..header.len().saturating_sub(32) {
            assert_ne!(
                &header[window_start..window_start + 32],
                alice_pk_bytes,
                "frame header must not contain alice's pubkey (sealed sender invariant)",
            );
        }
        let bob_pk_bytes = bob_pk.as_bytes();
        for window_start in 0..header.len().saturating_sub(32) {
            assert_ne!(
                &header[window_start..window_start + 32],
                bob_pk_bytes,
                "frame header must not contain bob's pubkey",
            );
        }

        // Routing decision: Alice's frame goes to Bob (and only Bob).
        let decision = on_frame_ingress(&mut state, &config, &alice_conn, &frame, 0);
        match decision {
            IngressDecision::Forward { recipients } => {
                assert_eq!(recipients.len(), 1);
                assert_eq!(recipients[0], bob_session);
            }
            _ => panic!("expected Forward, got Drop"),
        }
    }

    /// Multi-track lifecycle: same session can enable Audio first then Video.
    #[test]
    fn multi_track_lifecycle_audio_then_video() {
        let mut state = StreamState::new();
        let config = MediaConfig::default();
        let session = [1u8; 16];
        let conn = [0xaa; 32];
        // Install with NO active tracks yet.
        state.sessions.insert(session, ServerSession {
            connection_pk: conn,
            channel_id: 100,
            public_key: farder_crypto::identity::PublicKey::from_bytes([0xaa; 32]),
            display_name: "alice".into(),
            active_tracks: HashSet::new(),
            buckets: HashMap::new(),
            last_audio_frame_ms: None,
            last_video_frame_ms: None,
            last_screen_audio_frame_ms: None,
        });

        // Frame for audio -- dropped because Audio isn't enabled yet.
        let audio_frame = outer_dgram(TrackKind::Audio, &session, b"x");
        match on_frame_ingress(&mut state, &config, &conn, &audio_frame, 0) {
            IngressDecision::Drop(DropReason::TrackNotEnabled) => {}
            _ => panic!("expected TrackNotEnabled drop for audio"),
        }

        // Enable Audio.
        state.sessions.get_mut(&session).unwrap().active_tracks.insert(TrackKind::Audio);

        // Audio now admitted (and there are no other recipients in channel 100).
        match on_frame_ingress(&mut state, &config, &conn, &audio_frame, 0) {
            IngressDecision::Forward { recipients } => assert!(recipients.is_empty()),
            _ => panic!("expected Forward (empty) for audio"),
        }

        // Video frame still dropped -- Video not yet enabled.
        let video_frame = outer_dgram(TrackKind::Video, &session, b"y");
        match on_frame_ingress(&mut state, &config, &conn, &video_frame, 0) {
            IngressDecision::Drop(DropReason::TrackNotEnabled) => {}
            _ => panic!("expected TrackNotEnabled drop for video"),
        }

        // Enable Video. Both kinds now flow.
        state.sessions.get_mut(&session).unwrap().active_tracks.insert(TrackKind::Video);
        match on_frame_ingress(&mut state, &config, &conn, &video_frame, 0) {
            IngressDecision::Forward { .. } => {}
            _ => panic!("expected Forward for video"),
        }
    }

    /// ScreenAudio uses the AUDIO bandwidth cap, not the video cap.
    ///
    /// We configure audio_max_bps=500 and video_max_bps=8_000_000.
    /// A ScreenAudio frame of 1000 bytes should be denied with BandwidthCap
    /// (because 1000 > 500/2 initial tokens), proving the audio bucket — not
    /// the video bucket — gates the frame.
    #[test]
    fn screen_audio_uses_audio_cap() {
        let mut state = StreamState::new();
        // Deliberately tiny audio cap so a single medium frame exceeds it,
        // but video cap is enormous (proving we picked the right branch).
        let config = MediaConfig { audio_max_bps: 500, video_max_bps: 8_000_000 };
        let session = [7u8; 16];
        let conn = [0xcc; 32];
        let mut tracks = HashSet::new();
        tracks.insert(TrackKind::ScreenAudio);
        state.sessions.insert(session, ServerSession {
            connection_pk: conn,
            channel_id: 200,
            public_key: fake_pubkey(0xcc),
            display_name: "screensharer".into(),
            active_tracks: tracks,
            buckets: HashMap::new(),
            last_audio_frame_ms: None,
            last_video_frame_ms: None,
            last_screen_audio_frame_ms: None,
        });

        // Frame larger than audio_max_bps / 2 (initial tokens) — must be denied.
        let big_frame = outer_dgram(TrackKind::ScreenAudio, &session, &vec![0u8; 1000]);
        match on_frame_ingress(&mut state, &config, &conn, &big_frame, 0) {
            IngressDecision::Drop(DropReason::BandwidthCap) => {}
            _ => panic!("expected BandwidthCap drop for ScreenAudio over audio cap"),
        }
    }

    /// ScreenAudio activity stamp is tracked independently in last_screen_audio_frame_ms.
    #[test]
    fn screen_audio_activity_transition() {
        let mut state = StreamState::new();
        let session = [8u8; 16];
        let mut tracks = HashSet::new();
        tracks.insert(TrackKind::ScreenAudio);
        state.sessions.insert(session, ServerSession {
            connection_pk: [0u8; 32],
            channel_id: 300,
            public_key: fake_pubkey(0),
            display_name: "s".into(),
            active_tracks: tracks,
            buckets: HashMap::new(),
            last_audio_frame_ms: None,
            last_video_frame_ms: None,
            last_screen_audio_frame_ms: Some(1000),
        });
        let prev = HashMap::new();
        // At t=1100 (100ms after last frame) — within 300ms threshold, should fire active=true.
        let (transitions, _) = compute_activity_transitions(&state, &prev, 1100);
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].kind, TrackKind::ScreenAudio);
        assert!(transitions[0].active);
    }
}
