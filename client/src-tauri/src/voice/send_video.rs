//! Video send path: take encoded H.264 frames (from the Phase B encoder),
//! seal them with the peer-shared VIDEO stream key, fragment (Phase A), and
//! push each datagram to the connection. Mirrors voice::send but for video:
//! no Opus/APM — the frames are already H.264.
//!
//! The keyframe flag travels INSIDE the encrypted payload (1 leading byte) so
//! the receiver can tag the WebCodecs chunk key/delta without a side channel.

use crate::video_encoder::EncodedFrame;
use crate::voice::SessionId;
use bytes::Bytes;
use farder_crypto::media::seal_video_frame_to_wire;
use farder_protocol::media_datagram::{fragment, DEFAULT_MAX_DGRAM_PAYLOAD};
use farder_protocol::server::TrackKind;

/// Build the sealed+fragmented datagrams for one encoded video frame. Pure
/// (no I/O) so it's unit-testable; the caller sends each datagram.
/// `seq` is monotonic per stream (AEAD nonce); `frame_id` groups fragments.
pub fn build_video_datagrams(
    stream_key: &[u8; 32],
    session_id: &SessionId,
    speaker_pk: &[u8; 32],
    seq: u64,
    frame_id: u32,
    frame: &EncodedFrame,
) -> Result<Vec<Vec<u8>>, String> {
    // Payload = [keyframe:1][H.264 Annex-B...] (encrypted together).
    let mut payload = Vec::with_capacity(1 + frame.data.len());
    payload.push(frame.is_keyframe as u8);
    payload.extend_from_slice(&frame.data);
    let wire = seal_video_frame_to_wire(stream_key, seq, session_id, speaker_pk, &payload)
        .map_err(|e| format!("seal video: {e}"))?;
    Ok(fragment(TrackKind::Video, session_id, frame_id, &wire, DEFAULT_MAX_DGRAM_PAYLOAD))
}

/// A reusable sender that seals+fragments each frame and pushes datagrams to a
/// sink, advancing its own seq/frame_id counters.
pub struct VideoSender {
    stream_key: [u8; 32],
    session_id: SessionId,
    speaker_pk: [u8; 32],
    seq: u64,
    frame_id: u32,
}

impl VideoSender {
    pub fn new(stream_key: [u8; 32], session_id: SessionId, speaker_pk: [u8; 32]) -> Self {
        Self { stream_key, session_id, speaker_pk, seq: 0, frame_id: 0 }
    }

    /// Seal+fragment `frame` and push each datagram via `sink`.
    pub fn send(&mut self, frame: &EncodedFrame, mut sink: impl FnMut(Bytes)) {
        match build_video_datagrams(&self.stream_key, &self.session_id, &self.speaker_pk, self.seq, self.frame_id, frame) {
            Ok(dgrams) => {
                for d in dgrams {
                    sink(Bytes::from(d));
                }
            }
            Err(e) => eprintln!("[voice::send_video] dropped a frame: {e}"),
        }
        self.seq = self.seq.wrapping_add(1);
        self.frame_id = self.frame_id.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::media::open_video_wire_frame;
    use farder_protocol::media_datagram::{OuterHeader, Reassembler};

    fn enc_frame(key: bool, body: &[u8]) -> EncodedFrame {
        EncodedFrame { data: body.to_vec(), is_keyframe: key, timestamp_ms: 0 }
    }

    #[test]
    fn small_frame_seals_fragments_and_roundtrips_with_keyframe_flag() {
        let key = [0x22u8; 32];
        let session: SessionId = [0x33u8; 16];
        let speaker = [0x44u8; 32];
        let frame = enc_frame(true, &[0, 0, 0, 1, 0x67, 9, 8, 7]);
        let dgrams = build_video_datagrams(&key, &session, &speaker, 3, 3, &frame).unwrap();
        assert_eq!(dgrams.len(), 1, "small frame is one datagram");

        let mut reasm = Reassembler::new();
        let (header, payload) = OuterHeader::parse(&dgrams[0]).unwrap();
        assert_eq!(header.track_kind, TrackKind::Video);
        let sealed = reasm.accept(&header, payload).unwrap();
        let (seq, got_speaker, plaintext) = open_video_wire_frame(&key, &sealed).unwrap();
        assert_eq!(seq, 3);
        assert_eq!(got_speaker, speaker);
        assert_eq!(plaintext[0], 1, "keyframe byte == 1");
        assert_eq!(&plaintext[1..], &frame.data[..]);
    }

    #[test]
    fn large_frame_fragments_and_reassembles() {
        let key = [1u8; 32];
        let session: SessionId = [2u8; 16];
        let speaker = [3u8; 32];
        let big: Vec<u8> = (0..6000u32).map(|i| (i % 251) as u8).collect();
        let frame = enc_frame(false, &big);
        let dgrams = build_video_datagrams(&key, &session, &speaker, 0, 0, &frame).unwrap();
        assert!(dgrams.len() > 1, "a 6KB frame must span multiple datagrams");

        let mut reasm = Reassembler::new();
        let mut sealed = None;
        for d in dgrams.iter().rev() {
            let (h, p) = OuterHeader::parse(d).unwrap();
            if let Some(s) = reasm.accept(&h, p) {
                sealed = Some(s);
            }
        }
        let (_, _, plaintext) = open_video_wire_frame(&key, &sealed.unwrap()).unwrap();
        assert_eq!(plaintext[0], 0, "keyframe byte == 0");
        assert_eq!(&plaintext[1..], &big[..]);
    }

    #[test]
    fn sender_advances_seq_and_pushes() {
        let mut s = VideoSender::new([7u8; 32], [8u8; 16], [9u8; 32]);
        let collected = std::cell::RefCell::new(Vec::new());
        s.send(&enc_frame(true, b"a"), |b| collected.borrow_mut().push(b));
        s.send(&enc_frame(false, b"b"), |b| collected.borrow_mut().push(b));
        assert_eq!(collected.borrow().len(), 2, "two small frames -> two datagrams");
        assert_ne!(collected.borrow()[0], collected.borrow()[1]);
    }
}
