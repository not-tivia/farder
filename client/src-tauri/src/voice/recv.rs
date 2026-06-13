// client/src-tauri/src/voice/recv.rs
//
// Recv-path pipeline (one task per remote peer's audio session):
// inbound datagram -> open_audio_wire_frame -> jitter buffer -> opus decode (or PLC)
// -> per-peer PCM ring. The mixer drains the ring on its own cadence.
//
// See docs/superpowers/specs/2026-05-26-voice-client-pipeline-design.md.

use crate::opus_codec::{OpusDecoder, OPUS_FRAME_SAMPLES_MONO, OPUS_SAMPLE_RATE};
use crate::voice::jitter::JitterBuffer;
use crate::voice::SessionId;
use bytes::Bytes;
use farder_crypto::media::open_audio_wire_frame;
use farder_protocol::media_datagram::{OuterHeader, Reassembler};
use std::collections::VecDeque;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};
use tokio::sync::mpsc;

/// Per-peer PCM ring. Mixer drains one frame's worth on each tick.
pub struct PeerPcmRing {
    inner: Mutex<VecDeque<f32>>,
    capacity: usize,
}

impl PeerPcmRing {
    pub fn new(capacity_frames: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(
                capacity_frames * OPUS_FRAME_SAMPLES_MONO,
            )),
            capacity: capacity_frames * OPUS_FRAME_SAMPLES_MONO,
        }
    }

    pub fn push_frame(&self, samples: &[f32]) {
        let mut q = self.inner.lock().expect("ring poisoned");
        for s in samples {
            if q.len() >= self.capacity {
                q.pop_front();
            }
            q.push_back(*s);
        }
    }

    /// Pop one 20 ms frame. Pads with silence if the ring underflowed.
    pub fn pop_frame(&self) -> Vec<f32> {
        let mut q = self.inner.lock().expect("ring poisoned");
        let mut out = Vec::with_capacity(OPUS_FRAME_SAMPLES_MONO);
        for _ in 0..OPUS_FRAME_SAMPLES_MONO {
            out.push(q.pop_front().unwrap_or(0.0));
        }
        out
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

pub struct RecvTaskConfig {
    pub session_id: SessionId,
    pub stream_key: [u8; 32],
    pub deafened: Arc<AtomicBool>,
    pub datagram_rx: mpsc::UnboundedReceiver<Bytes>,
    pub pcm_ring: Arc<PeerPcmRing>,
}

/// Run the recv task. Returns when the datagram channel is closed.
pub async fn run(cfg: RecvTaskConfig) {
    let mut decoder = match OpusDecoder::new(OPUS_SAMPLE_RATE, 1) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[voice::recv] decoder init: {e}");
            return;
        }
    };
    let mut jitter = JitterBuffer::new();
    let mut reassembler = Reassembler::new();
    let mut datagram_rx = cfg.datagram_rx;

    while let Some(bytes) = datagram_rx.recv().await {
        if cfg.deafened.load(Ordering::Acquire) {
            continue;
        }
        let (header, payload) = match OuterHeader::parse(&bytes) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let sealed = match reassembler.accept(&header, payload) {
            Some(s) => s,
            None => continue, // fragment buffered; frame not complete yet
        };
        let (seq, _speaker_pk, opus_pkt) = match open_audio_wire_frame(&cfg.stream_key, &sealed) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[voice::recv] open: {e}");
                continue;
            }
        };
        // Insert into jitter; pop one slot per inbound frame for simple v1
        // pacing (the mixer's own cadence smooths out timing).
        jitter.insert(seq, opus_pkt);
        match jitter.pop() {
            Some(pkt) => match decoder.decode(&pkt) {
                Ok(pcm) => cfg.pcm_ring.push_frame(&pcm),
                Err(e) => eprintln!("[voice::recv] decode: {e}"),
            },
            None => {
                if let Ok(pcm) = decoder.decode_plc() {
                    cfg.pcm_ring.push_frame(&pcm);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opus_codec::OpusEncoder;
    use farder_crypto::media::seal_audio_packet_to_wire;

    fn make_wire_frame(stream_key: &[u8; 32], session_id: &SessionId, seq: u64) -> Bytes {
        let mut enc = OpusEncoder::new(OPUS_SAMPLE_RATE, 1, 24_000).unwrap();
        let pcm: Vec<f32> = (0..OPUS_FRAME_SAMPLES_MONO)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / OPUS_SAMPLE_RATE as f32).sin() * 0.5)
            .collect();
        let pkt = enc.encode(&pcm).unwrap();
        let speaker_pk = [0xCCu8; 32];
        let wire = seal_audio_packet_to_wire(stream_key, seq, session_id, &speaker_pk, &pkt).unwrap();
        use farder_protocol::media_datagram::OuterHeader;
        use farder_protocol::server::TrackKind;
        let mut dgram = Vec::new();
        OuterHeader {
            track_kind: TrackKind::Audio,
            session_id: *session_id,
            frame_id: seq as u32,
            frag_index: 0,
            frag_count: 1,
        }
        .write_to(&mut dgram);
        dgram.extend_from_slice(&wire);
        Bytes::from(dgram)
    }

    #[tokio::test]
    async fn in_order_datagrams_populate_ring() {
        let stream_key = [0x11u8; 32];
        let session_id: SessionId = [0x22u8; 16];
        let ring = Arc::new(PeerPcmRing::new(10));
        let (tx, rx) = mpsc::unbounded_channel();

        for seq in 0..3 {
            tx.send(make_wire_frame(&stream_key, &session_id, seq)).unwrap();
        }
        drop(tx);

        run(RecvTaskConfig {
            session_id,
            stream_key,
            deafened: Arc::new(AtomicBool::new(false)),
            datagram_rx: rx,
            pcm_ring: ring.clone(),
        })
        .await;

        // At least one frame's worth of samples should be in the ring.
        assert!(ring.len() >= OPUS_FRAME_SAMPLES_MONO,
                "ring should have >= 1 frame; got {}", ring.len());
    }

    #[tokio::test]
    async fn deafened_drops_everything() {
        let stream_key = [0x11u8; 32];
        let session_id: SessionId = [0x22u8; 16];
        let ring = Arc::new(PeerPcmRing::new(10));
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(make_wire_frame(&stream_key, &session_id, 0)).unwrap();
        tx.send(make_wire_frame(&stream_key, &session_id, 1)).unwrap();
        drop(tx);

        run(RecvTaskConfig {
            session_id,
            stream_key,
            deafened: Arc::new(AtomicBool::new(true)),
            datagram_rx: rx,
            pcm_ring: ring.clone(),
        })
        .await;

        assert_eq!(ring.len(), 0, "deafened recv must leave the ring empty");
    }

    #[tokio::test]
    async fn corrupted_frame_is_ignored() {
        let ring = Arc::new(PeerPcmRing::new(10));
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(Bytes::from(vec![0u8; 30])).unwrap(); // wrong version byte / garbage
        drop(tx);

        run(RecvTaskConfig {
            session_id: [0; 16],
            stream_key: [0; 32],
            deafened: Arc::new(AtomicBool::new(false)),
            datagram_rx: rx,
            pcm_ring: ring.clone(),
        })
        .await; // must not panic

        assert_eq!(ring.len(), 0, "garbage frame must not populate the ring");
    }
}
