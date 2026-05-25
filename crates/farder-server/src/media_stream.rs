// crates/farder-server/src/media_stream.rs
//
// Generalized media-stream routing. Replaces the voice-only `voice.rs`
// fanout machinery (deleted in MST-11) with a typed Audio+Video transport.
//
// Per spec (2026-05-25-media-stream-transport-design.md): server sees
// ciphertext only; routes by opaque session_id; per-(session, kind)
// token-bucket bandwidth caps.

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

/// Extract just the 28-byte header (the AEAD AAD for the crypto helpers).
/// Caller must ensure `buf` is at least that long.
pub fn media_frame_header_aad(buf: &[u8]) -> &[u8] {
    &buf[..MEDIA_FRAME_HEADER_LEN]
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
}
