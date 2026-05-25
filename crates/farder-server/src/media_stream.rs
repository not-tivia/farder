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
}
