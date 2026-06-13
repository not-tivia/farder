//! Video receive task (one per remote peer's video session): inbound datagram
//! -> reassemble -> open with the peer's video stream key -> split the keyframe
//! byte -> hand the encoded H.264 to a sink (the controller forwards it to the
//! webview, which decodes via WebCodecs). No jitter buffer / no decode here —
//! video timing is handled by the WebCodecs decoder on the frontend.

use crate::voice::SessionId;
use bytes::Bytes;
use farder_crypto::media::open_video_wire_frame;
use farder_protocol::media_datagram::{OuterHeader, Reassembler};
use tokio::sync::mpsc;

/// One decoded-from-the-wire (still H.264-encoded) video frame for the frontend.
pub struct VideoOut {
    pub data: Vec<u8>,     // H.264 Annex-B
    pub is_keyframe: bool,
    pub seq: u64,
}

pub struct RecvVideoConfig {
    pub session_id: SessionId,
    pub stream_key: [u8; 32],
    pub datagram_rx: mpsc::UnboundedReceiver<Bytes>,
}

/// Run the video recv task. Calls `sink` for each fully-received, decrypted
/// frame. Returns when the datagram channel closes.
pub async fn run(mut cfg: RecvVideoConfig, mut sink: impl FnMut(VideoOut)) {
    let _session_id = cfg.session_id; // retained for controller bookkeeping symmetry
    let mut reassembler = Reassembler::new();
    while let Some(bytes) = cfg.datagram_rx.recv().await {
        let (header, payload) = match OuterHeader::parse(&bytes) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let sealed = match reassembler.accept(&header, payload) {
            Some(s) => s,
            None => continue,
        };
        let (seq, _speaker_pk, plaintext) = match open_video_wire_frame(&cfg.stream_key, &sealed) {
            Ok(t) => t,
            Err(_) => continue, // wrong key / not yet keyed / corrupt -> drop
        };
        if plaintext.is_empty() {
            continue;
        }
        let is_keyframe = plaintext[0] == 1;
        sink(VideoOut { data: plaintext[1..].to_vec(), is_keyframe, seq });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video_encoder::EncodedFrame;
    use crate::voice::send_video::build_video_datagrams;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn receives_and_emits_a_keyframe() {
        let key = [0x55u8; 32];
        let session: SessionId = [0x66u8; 16];
        let speaker = [0x77u8; 32];
        let frame = EncodedFrame { data: vec![0, 0, 0, 1, 0x65, 1, 2], is_keyframe: true, timestamp_ms: 0 };
        let dgrams = build_video_datagrams(&key, &session, &speaker, 0, 0, &frame).unwrap();

        let (tx, rx) = mpsc::unbounded_channel();
        for d in dgrams { tx.send(Bytes::from(d)).unwrap(); }
        drop(tx);

        let out = Arc::new(Mutex::new(Vec::<VideoOut>::new()));
        let out2 = out.clone();
        run(RecvVideoConfig { session_id: session, stream_key: key, datagram_rx: rx }, move |v| {
            out2.lock().unwrap().push(v);
        }).await;

        let frames = out.lock().unwrap();
        assert_eq!(frames.len(), 1);
        assert!(frames[0].is_keyframe);
        assert_eq!(frames[0].data, frame.data);
    }

    #[tokio::test]
    async fn wrong_key_drops_frame() {
        let key = [1u8; 32];
        let session: SessionId = [2u8; 16];
        let frame = EncodedFrame { data: vec![9, 9, 9], is_keyframe: false, timestamp_ms: 0 };
        let dgrams = build_video_datagrams(&key, &session, &[3u8; 32], 0, 0, &frame).unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        for d in dgrams { tx.send(Bytes::from(d)).unwrap(); }
        drop(tx);

        let out = Arc::new(Mutex::new(Vec::<VideoOut>::new()));
        let out2 = out.clone();
        // Decrypt with the WRONG key.
        run(RecvVideoConfig { session_id: session, stream_key: [0xFFu8; 32], datagram_rx: rx }, move |v| {
            out2.lock().unwrap().push(v);
        }).await;
        assert!(out.lock().unwrap().is_empty(), "a frame under the wrong key must be dropped, not panic");
    }
}
