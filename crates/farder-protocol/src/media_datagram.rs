//! Unified media-datagram transport: a 26-byte cleartext outer header plus
//! fragment/reassemble, so large (video) frames ride the QUIC datagram path.
//!
//! The outer header is what the relay/server/receiver route on WITHOUT keys.
//! The payload it carries is the existing `farder-crypto` sealed frame (the
//! AEAD-bound security boundary) — possibly split across several datagrams.
//! Audio frames are a single fragment, so they gain only the 26-byte header.

use crate::server::TrackKind;
use farder_crypto::media::{
    MEDIA_FRAME_TYPE_AUDIO, MEDIA_FRAME_TYPE_SCREEN_AUDIO, MEDIA_FRAME_TYPE_VIDEO, SessionId,
    SESSION_ID_LEN,
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
        TrackKind::ScreenAudio => MEDIA_FRAME_TYPE_SCREEN_AUDIO,
    }
}

fn byte_to_track_kind(b: u8) -> Option<TrackKind> {
    match b {
        MEDIA_FRAME_TYPE_AUDIO => Some(TrackKind::Audio),
        MEDIA_FRAME_TYPE_VIDEO => Some(TrackKind::Video),
        MEDIA_FRAME_TYPE_SCREEN_AUDIO => Some(TrackKind::ScreenAudio),
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

use std::collections::HashMap;

/// Split a sealed frame into one or more datagrams (each = outer header +
/// payload slice). `max_payload` must be >= 1. A frame that fits in one
/// datagram becomes a single `frag_count = 1` datagram.
///
/// Precondition: `sealed.len() <= max_payload * u16::MAX` (i.e. at most 65535
/// fragments). Real media frames are far below this; violating it is a bug
/// (debug-asserted) and yields aliased fragment indices in release.
pub fn fragment(
    track_kind: TrackKind,
    session_id: &SessionId,
    frame_id: u32,
    sealed: &[u8],
    max_payload: usize,
) -> Vec<Vec<u8>> {
    let max_payload = max_payload.max(1);
    let frag_count = sealed.len().div_ceil(max_payload).max(1);
    debug_assert!(
        frag_count <= u16::MAX as usize,
        "sealed frame too large to fragment: {} fragments exceeds u16::MAX (frame {} bytes, max_payload {})",
        frag_count, sealed.len(), max_payload
    );
    let frag_count_u16 = frag_count.min(u16::MAX as usize) as u16;
    let mut out = Vec::with_capacity(frag_count);
    for (i, chunk) in sealed.chunks(max_payload).enumerate() {
        let mut buf = Vec::with_capacity(MEDIA_DGRAM_HEADER_LEN + chunk.len());
        OuterHeader {
            track_kind,
            session_id: *session_id,
            frame_id,
            frag_index: i as u16,
            frag_count: frag_count_u16,
        }
        .write_to(&mut buf);
        buf.extend_from_slice(chunk);
        out.push(buf);
    }
    // An empty sealed frame still produces one empty-payload datagram so the
    // receiver completes it (chunks() yields nothing for an empty slice).
    if out.is_empty() {
        let mut buf = Vec::with_capacity(MEDIA_DGRAM_HEADER_LEN);
        OuterHeader { track_kind, session_id: *session_id, frame_id, frag_index: 0, frag_count: 1 }
            .write_to(&mut buf);
        out.push(buf);
    }
    out
}

struct Partial {
    frag_count: u16,
    parts: Vec<Option<Vec<u8>>>,
    received: u16,
    touch: u64,
}

/// Reassembles datagrams of one peer-track into complete sealed frames.
/// Drop-late / drop-incomplete: only the most-recently-touched `max_frames`
/// in-progress frames are kept; older incomplete frames are evicted.
pub struct Reassembler {
    frames: HashMap<u32, Partial>,
    max_frames: usize,
    clock: u64,
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::with_capacity(4)
    }
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(max_frames: usize) -> Self {
        Self { frames: HashMap::new(), max_frames: max_frames.max(1), clock: 0 }
    }

    /// Number of in-progress (incomplete) frames currently buffered.
    pub fn in_progress_len(&self) -> usize {
        self.frames.len()
    }

    /// Feed one parsed datagram. Returns the completed sealed frame if this
    /// datagram finished one. `header.frag_index < header.frag_count` is
    /// guaranteed by `OuterHeader::parse`.
    ///
    /// # Panics
    /// Panics if `header.frag_index >= header.frag_count`. Datagrams produced
    /// by `OuterHeader::parse` always satisfy this; only hand-constructed
    /// headers can violate it.
    pub fn accept(&mut self, header: &OuterHeader, payload: &[u8]) -> Option<Vec<u8>> {
        // Single-fragment fast path: no buffering.
        if header.frag_count == 1 {
            return Some(payload.to_vec());
        }
        self.clock += 1;
        let now = self.clock;
        let cap = header.frag_count as usize;

        let entry = self.frames.entry(header.frame_id).or_insert_with(|| Partial {
            frag_count: header.frag_count,
            parts: vec![None; cap],
            received: 0,
            touch: now,
        });

        // A frag_count mismatch for the same frame_id means corruption/reuse —
        // restart this frame's buffer.
        if entry.frag_count != header.frag_count {
            *entry = Partial { frag_count: header.frag_count, parts: vec![None; cap], received: 0, touch: now };
        }
        entry.touch = now;

        let slot = &mut entry.parts[header.frag_index as usize];
        if slot.is_none() {
            *slot = Some(payload.to_vec());
            entry.received += 1;
        }

        let done = entry.received == entry.frag_count;
        if done {
            let entry = self.frames.remove(&header.frame_id).unwrap();
            let mut sealed = Vec::new();
            for part in entry.parts {
                sealed.extend_from_slice(&part.expect("all parts present when received == frag_count"));
            }
            return Some(sealed);
        }

        // Evict the least-recently-touched frame if over capacity.
        if self.frames.len() > self.max_frames {
            if let Some((&oldest, _)) = self.frames.iter().min_by_key(|(_, p)| p.touch) {
                self.frames.remove(&oldest);
            }
        }
        None
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

    #[test]
    fn fragment_single_when_under_cap() {
        let dgrams = fragment(TrackKind::Audio, &sid(), 5, b"small-sealed-frame", 1100);
        assert_eq!(dgrams.len(), 1);
        let (h, payload) = OuterHeader::parse(&dgrams[0]).unwrap();
        assert_eq!(h.frag_count, 1);
        assert_eq!(h.frag_index, 0);
        assert_eq!(h.frame_id, 5);
        assert_eq!(payload, b"small-sealed-frame");
    }

    #[test]
    fn fragment_splits_when_over_cap() {
        let sealed: Vec<u8> = (0..2500u32).map(|i| i as u8).collect();
        let dgrams = fragment(TrackKind::Video, &sid(), 9, &sealed, 1000);
        assert_eq!(dgrams.len(), 3); // ceil(2500 / 1000)
        for (i, d) in dgrams.iter().enumerate() {
            let (h, _) = OuterHeader::parse(d).unwrap();
            assert_eq!(h.frag_count, 3);
            assert_eq!(h.frag_index as usize, i);
            assert_eq!(h.frame_id, 9);
            assert_eq!(h.track_kind, TrackKind::Video);
        }
    }

    fn feed(reasm: &mut Reassembler, dgram: &[u8]) -> Option<Vec<u8>> {
        let (h, payload) = OuterHeader::parse(dgram).unwrap();
        reasm.accept(&h, payload)
    }

    #[test]
    fn reassemble_single_fragment_completes_immediately() {
        let dgrams = fragment(TrackKind::Audio, &sid(), 1, b"abc", 1100);
        let mut r = Reassembler::new();
        assert_eq!(feed(&mut r, &dgrams[0]).as_deref(), Some(&b"abc"[..]));
    }

    #[test]
    fn reassemble_multi_in_order() {
        let sealed: Vec<u8> = (0..2500u32).map(|i| i as u8).collect();
        let dgrams = fragment(TrackKind::Video, &sid(), 1, &sealed, 1000);
        let mut r = Reassembler::new();
        assert!(feed(&mut r, &dgrams[0]).is_none());
        assert!(feed(&mut r, &dgrams[1]).is_none());
        assert_eq!(feed(&mut r, &dgrams[2]), Some(sealed));
    }

    #[test]
    fn reassemble_multi_out_of_order() {
        let sealed: Vec<u8> = (0..2500u32).map(|i| (i * 3) as u8).collect();
        let dgrams = fragment(TrackKind::Video, &sid(), 1, &sealed, 1000);
        let mut r = Reassembler::new();
        assert!(feed(&mut r, &dgrams[2]).is_none());
        assert!(feed(&mut r, &dgrams[0]).is_none());
        assert_eq!(feed(&mut r, &dgrams[1]), Some(sealed));
    }

    #[test]
    fn reassemble_duplicate_fragment_is_idempotent() {
        let sealed: Vec<u8> = (0..1500u32).map(|i| i as u8).collect();
        let dgrams = fragment(TrackKind::Video, &sid(), 1, &sealed, 1000);
        let mut r = Reassembler::new();
        assert!(feed(&mut r, &dgrams[0]).is_none());
        assert!(feed(&mut r, &dgrams[0]).is_none()); // duplicate, must not "complete"
        assert_eq!(feed(&mut r, &dgrams[1]), Some(sealed));
    }

    #[test]
    fn reassemble_incomplete_frame_is_evicted_not_leaked() {
        // Start many frames, each missing a fragment; the buffer must stay bounded.
        let mut r = Reassembler::with_capacity(4);
        for frame_id in 0..100u32 {
            let sealed: Vec<u8> = (0..1500u32).map(|i| i as u8).collect();
            let dgrams = fragment(TrackKind::Video, &sid(), frame_id, &sealed, 1000);
            // feed only fragment 0 — never completes
            assert!(feed(&mut r, &dgrams[0]).is_none());
        }
        assert!(r.in_progress_len() <= 4, "reassembly buffer must stay bounded");
    }

    #[test]
    fn fragment_empty_sealed_yields_one_empty_payload_datagram() {
        let dgrams = fragment(TrackKind::Audio, &sid(), 1, b"", 1100);
        assert_eq!(dgrams.len(), 1);
        let (h, payload) = OuterHeader::parse(&dgrams[0]).unwrap();
        assert_eq!(h.frag_count, 1);
        assert!(payload.is_empty());
        // And it round-trips through the reassembler to an empty frame.
        let mut r = Reassembler::new();
        assert_eq!(feed(&mut r, &dgrams[0]).as_deref(), Some(&b""[..]));
    }

    #[test]
    fn fragment_coerces_zero_max_payload_to_one() {
        // max_payload 0 must not divide-by-zero; it's coerced to 1 -> one datagram per byte.
        let dgrams = fragment(TrackKind::Video, &sid(), 1, b"abc", 0);
        assert_eq!(dgrams.len(), 3);
        for (i, d) in dgrams.iter().enumerate() {
            let (h, payload) = OuterHeader::parse(d).unwrap();
            assert_eq!(h.frag_count, 3);
            assert_eq!(h.frag_index as usize, i);
            assert_eq!(payload.len(), 1);
        }
    }

    #[test]
    fn screen_audio_track_kind_round_trips_through_outer_byte() {
        let sid = [9u8; 16];
        let dgrams = fragment(TrackKind::ScreenAudio, &sid, 0, b"opaque", DEFAULT_MAX_DGRAM_PAYLOAD);
        let (hdr, _payload) = OuterHeader::parse(&dgrams[0]).expect("valid header");
        assert_eq!(hdr.track_kind, TrackKind::ScreenAudio);
    }

    #[test]
    fn reassemble_frag_count_mismatch_restarts_cleanly() {
        // Same frame_id reused with a different frag_count must reset the buffer
        // (no panic, no stale completion).
        let sealed_a: Vec<u8> = (0..2500u32).map(|i| i as u8).collect();   // 3 frags @1000
        let sealed_b: Vec<u8> = (0..1500u32).map(|i| (i + 1) as u8).collect(); // 2 frags @1000
        let a = fragment(TrackKind::Video, &sid(), 7, &sealed_a, 1000);
        let b = fragment(TrackKind::Video, &sid(), 7, &sealed_b, 1000); // same frame_id, fewer frags
        let mut r = Reassembler::new();
        assert!(feed(&mut r, &a[0]).is_none());            // buffers under frag_count=3
        assert!(feed(&mut r, &b[0]).is_none());            // mismatch -> reset to frag_count=2
        assert_eq!(feed(&mut r, &b[1]), Some(sealed_b));   // completes the NEW frame cleanly
    }
}
