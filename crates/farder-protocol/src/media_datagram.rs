//! Unified media-datagram transport: a 26-byte cleartext outer header plus
//! fragment/reassemble, so large (video) frames ride the QUIC datagram path.
//!
//! The outer header is what the relay/server/receiver route on WITHOUT keys.
//! The payload it carries is the existing `farder-crypto` sealed frame (the
//! AEAD-bound security boundary) — possibly split across several datagrams.
//! Audio frames are a single fragment, so they gain only the 26-byte header.

use crate::server::TrackKind;
use farder_crypto::media::{
    MEDIA_FRAME_TYPE_AUDIO, MEDIA_FRAME_TYPE_VIDEO, SessionId, SESSION_ID_LEN,
};

/// Version byte for the outer media datagram header (distinct from the inner
/// sealed-frame version 0x02).
pub const MEDIA_DGRAM_VERSION: u8 = 0x03;

/// version(1) | track_kind(1) | session_id(16) | frame_id(4) | frag_index(2) | frag_count(2)
pub const MEDIA_DGRAM_HEADER_LEN: usize = 1 + 1 + SESSION_ID_LEN + 4 + 2 + 2; // 26

/// A conservative per-datagram payload cap when the connection's
/// `max_datagram_size` is unknown. Audio frames are far below this so they
/// never fragment; Phase C derives the real value from the connection.
pub const DEFAULT_MAX_DGRAM_PAYLOAD: usize = 1100;

#[derive(Debug, PartialEq, Eq)]
pub enum MediaDgramError {
    TooShort,
    BadVersion(u8),
    BadTrackKind(u8),
    BadFragmentation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OuterHeader {
    pub track_kind: TrackKind,
    pub session_id: SessionId,
    pub frame_id: u32,
    pub frag_index: u16,
    pub frag_count: u16,
}

fn track_kind_to_byte(k: TrackKind) -> u8 {
    match k {
        TrackKind::Audio => MEDIA_FRAME_TYPE_AUDIO,
        TrackKind::Video => MEDIA_FRAME_TYPE_VIDEO,
    }
}

fn byte_to_track_kind(b: u8) -> Option<TrackKind> {
    match b {
        MEDIA_FRAME_TYPE_AUDIO => Some(TrackKind::Audio),
        MEDIA_FRAME_TYPE_VIDEO => Some(TrackKind::Video),
        _ => None,
    }
}

impl OuterHeader {
    /// Append the 26-byte header to `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.push(MEDIA_DGRAM_VERSION);
        out.push(track_kind_to_byte(self.track_kind));
        out.extend_from_slice(&self.session_id);
        out.extend_from_slice(&self.frame_id.to_be_bytes());
        out.extend_from_slice(&self.frag_index.to_be_bytes());
        out.extend_from_slice(&self.frag_count.to_be_bytes());
    }

    /// Parse the header off the front of `buf`, returning it and the remaining
    /// payload slice. Validates version, track kind, and `frag_index < frag_count`.
    pub fn parse(buf: &[u8]) -> Result<(OuterHeader, &[u8]), MediaDgramError> {
        if buf.len() < MEDIA_DGRAM_HEADER_LEN {
            return Err(MediaDgramError::TooShort);
        }
        if buf[0] != MEDIA_DGRAM_VERSION {
            return Err(MediaDgramError::BadVersion(buf[0]));
        }
        let track_kind = byte_to_track_kind(buf[1]).ok_or(MediaDgramError::BadTrackKind(buf[1]))?;
        let mut session_id = [0u8; SESSION_ID_LEN];
        session_id.copy_from_slice(&buf[2..2 + SESSION_ID_LEN]);
        let frame_id = u32::from_be_bytes(buf[18..22].try_into().unwrap());
        let frag_index = u16::from_be_bytes(buf[22..24].try_into().unwrap());
        let frag_count = u16::from_be_bytes(buf[24..26].try_into().unwrap());
        if frag_count == 0 || frag_index >= frag_count {
            return Err(MediaDgramError::BadFragmentation);
        }
        Ok((
            OuterHeader { track_kind, session_id, frame_id, frag_index, frag_count },
            &buf[MEDIA_DGRAM_HEADER_LEN..],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid() -> SessionId {
        [9u8, 8, 7, 6, 5, 4, 3, 2, 1, 0, 1, 2, 3, 4, 5, 6]
    }

    #[test]
    fn header_roundtrip() {
        let h = OuterHeader {
            track_kind: TrackKind::Video,
            session_id: sid(),
            frame_id: 0xDEAD_BEEF,
            frag_index: 3,
            frag_count: 7,
        };
        let mut buf = Vec::new();
        h.write_to(&mut buf);
        buf.extend_from_slice(b"payload-bytes");
        assert_eq!(buf.len(), MEDIA_DGRAM_HEADER_LEN + 13);
        let (got, payload) = OuterHeader::parse(&buf).unwrap();
        assert_eq!(got, h);
        assert_eq!(payload, b"payload-bytes");
    }

    #[test]
    fn parse_rejects_short() {
        let buf = vec![0u8; MEDIA_DGRAM_HEADER_LEN - 1];
        assert_eq!(OuterHeader::parse(&buf), Err(MediaDgramError::TooShort));
    }

    #[test]
    fn parse_rejects_bad_version() {
        let mut buf = vec![0u8; MEDIA_DGRAM_HEADER_LEN];
        buf[0] = 0x02; // inner-frame version, not the outer one
        buf[1] = MEDIA_FRAME_TYPE_AUDIO;
        buf[25] = 1; // frag_count = 1 so only the version check fires
        assert_eq!(OuterHeader::parse(&buf), Err(MediaDgramError::BadVersion(0x02)));
    }

    #[test]
    fn parse_rejects_bad_track_kind() {
        let mut buf = vec![0u8; MEDIA_DGRAM_HEADER_LEN];
        buf[0] = MEDIA_DGRAM_VERSION;
        buf[1] = 0x7f;
        buf[25] = 1;
        assert_eq!(OuterHeader::parse(&buf), Err(MediaDgramError::BadTrackKind(0x7f)));
    }

    #[test]
    fn parse_rejects_bad_fragmentation() {
        // frag_index >= frag_count
        let h_bytes = {
            let mut v = Vec::new();
            OuterHeader {
                track_kind: TrackKind::Audio,
                session_id: sid(),
                frame_id: 1,
                frag_index: 2,
                frag_count: 2,
            }
            .write_to(&mut v);
            v
        };
        assert_eq!(OuterHeader::parse(&h_bytes), Err(MediaDgramError::BadFragmentation));

        // frag_count == 0
        let mut zero = h_bytes.clone();
        zero[22..24].copy_from_slice(&0u16.to_be_bytes()); // frag_index = 0
        zero[24..26].copy_from_slice(&0u16.to_be_bytes()); // frag_count = 0
        assert_eq!(OuterHeader::parse(&zero), Err(MediaDgramError::BadFragmentation));
    }
}
