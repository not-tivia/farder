//! Screen-audio send loop: PcmChunk -> Opus -> seal (audio crypto) -> fragment
//! under TrackKind::ScreenAudio -> datagram sink. No APM/gate/mute (game audio
//! is sent unconditionally while sharing). Mirrors voice::send's encode tail.

use crate::audio::PcmChunk;
use crate::opus_codec::{OpusEncoder, OPUS_DEFAULT_BITRATE_BPS, OPUS_FRAME_SAMPLES_MONO, OPUS_SAMPLE_RATE};
use crate::voice::SessionId;
use bytes::Bytes;
use farder_crypto::media::seal_audio_packet_to_wire;
use farder_protocol::media_datagram::{fragment, DEFAULT_MAX_DGRAM_PAYLOAD};
use farder_protocol::server::TrackKind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

pub struct ScreenAudioSendConfig {
    pub pcm_rx: mpsc::Receiver<PcmChunk>,
    pub session_id: SessionId,
    pub stream_key: [u8; 32],
    pub speaker_pk: [u8; 32],
    pub datagram_sink: Box<dyn Fn(Bytes) + Send + Sync + 'static>,
}

/// Run until `stop` is set or the PcmChunk channel closes.
pub fn run(cfg: ScreenAudioSendConfig, stop: Arc<AtomicBool>) {
    let mut encoder = match OpusEncoder::new(OPUS_SAMPLE_RATE, 1, OPUS_DEFAULT_BITRATE_BPS) {
        Ok(e) => e,
        Err(e) => { eprintln!("[screen_audio::send] encoder init: {e}"); return; }
    };
    let mut seq: u64 = 0;
    let mut frame_id: u32 = 0;
    while !stop.load(Ordering::Relaxed) {
        let chunk = match cfg.pcm_rx.recv() {
            Ok(c) => c,
            Err(_) => break,
        };
        if chunk.samples.len() != OPUS_FRAME_SAMPLES_MONO {
            seq = seq.saturating_add(1);
            continue;
        }
        let pkt = match encoder.encode(&chunk.samples) {
            Ok(p) => p,
            Err(e) => { eprintln!("[screen_audio::send] encode: {e}"); seq = seq.saturating_add(1); continue; }
        };
        let frame_bytes = match seal_audio_packet_to_wire(&cfg.stream_key, seq, &cfg.session_id, &cfg.speaker_pk, &pkt) {
            Ok(b) => b,
            Err(e) => { eprintln!("[screen_audio::send] seal: {e}"); seq = seq.saturating_add(1); continue; }
        };
        for dgram in fragment(TrackKind::ScreenAudio, &cfg.session_id, frame_id, &frame_bytes, DEFAULT_MAX_DGRAM_PAYLOAD) {
            (cfg.datagram_sink)(Bytes::from(dgram));
        }
        seq = seq.saturating_add(1);
        frame_id = frame_id.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_protocol::media_datagram::OuterHeader;
    use std::sync::Mutex;

    fn sine(samples: usize) -> PcmChunk {
        let pcm: Vec<f32> = (0..samples)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / OPUS_SAMPLE_RATE as f32).sin() * 0.5)
            .collect();
        PcmChunk { samples: pcm, timestamp_ms: 0 }
    }

    #[test]
    fn emits_one_screen_audio_datagram_per_chunk() {
        let (tx, rx) = mpsc::channel::<PcmChunk>();
        let sink: Arc<Mutex<Vec<Bytes>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_c = sink.clone();
        let cfg = ScreenAudioSendConfig {
            pcm_rx: rx,
            session_id: [7u8; 16],
            stream_key: [0xAA; 32],
            speaker_pk: [0xBB; 32],
            datagram_sink: Box::new(move |b| sink_c.lock().unwrap().push(b)),
        };
        for _ in 0..4 { tx.send(sine(OPUS_FRAME_SAMPLES_MONO)).unwrap(); }
        drop(tx);
        run(cfg, Arc::new(AtomicBool::new(false)));
        let got = sink.lock().unwrap();
        assert_eq!(got.len(), 4, "one datagram per chunk");
        let (hdr, _) = OuterHeader::parse(&got[0]).expect("valid header");
        assert_eq!(hdr.track_kind, TrackKind::ScreenAudio, "must route as ScreenAudio");
        assert_eq!(hdr.session_id, [7u8; 16]);
    }
}
