// client/src-tauri/src/voice/send.rs
//
// Send-path pipeline: capture -> APM -> gate -> encode -> seal -> datagram.
// Runs as a sync function inside spawn_blocking. PcmChunks arrive on a
// std::mpsc channel from AudioBackend::start_capture.
//
// Local speaking indicator emits via a tokio::sync::watch sender.
//
// See docs/superpowers/specs/2026-05-26-voice-client-pipeline-design.md.

use crate::opus_codec::{OpusEncoder, OPUS_DEFAULT_BITRATE_BPS, OPUS_FRAME_SAMPLES_MONO, OPUS_SAMPLE_RATE};
use crate::voice::apm::AudioProcessor;
use crate::voice::gate::GateMode;
use crate::voice::SessionId;
use bytes::Bytes;
use farder_crypto::media::seal_audio_packet_to_wire;
use std::sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc, Mutex};
use tokio::sync::watch;

pub struct SendTaskConfig {
    pub pcm_rx: mpsc::Receiver<crate::audio::PcmChunk>,
    pub apm: AudioProcessor,
    pub gate: GateMode,
    pub session_id: SessionId,
    pub stream_key: [u8; 32],
    pub speaker_pk: [u8; 32],
    /// Most-recent mixed playback PCM, fed back as APM render reference.
    pub aec_ref: Arc<Mutex<Vec<f32>>>,
    /// Datagram sink. Implementations: a closure that calls
    /// `quinn::Connection::send_datagram` or a test-friendly sink for unit tests.
    pub datagram_sink: Box<dyn Fn(Bytes) + Send + Sync + 'static>,
}

/// Run the send task. Blocks until the PcmChunk channel is closed.
pub fn run(
    cfg: SendTaskConfig,
    muted: Arc<AtomicBool>,
    local_speaking_tx: watch::Sender<bool>,
) {
    let mut apm = cfg.apm;
    let mut encoder = match OpusEncoder::new(OPUS_SAMPLE_RATE, 1, OPUS_DEFAULT_BITRATE_BPS) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[voice::send] encoder init: {e}");
            return;
        }
    };
    let mut seq: u64 = 0;
    let mut speaking = false;
    let mut consec_below: u32 = 0;
    const SPEAK_THRESHOLD: f32 = 0.03;
    const SPEAK_HANGOVER_FRAMES: u32 = 15; // ~300 ms at 20 ms/frame

    while let Ok(chunk) = cfg.pcm_rx.recv() {
        let mut frame = chunk.samples;
        if frame.len() != OPUS_FRAME_SAMPLES_MONO {
            // The backend is supposed to deliver exact 20 ms frames; if it
            // didn't, skip rather than mis-encode.
            seq = seq.saturating_add(1);
            continue;
        }

        // AEC: feed last playback frame as render reference.
        {
            let guard = cfg.aec_ref.lock().expect("aec_ref poisoned");
            if guard.len() == OPUS_FRAME_SAMPLES_MONO {
                let mut render = guard.clone();
                drop(guard);
                apm.process_render(&mut render);
            }
        }
        apm.process_capture(&mut frame);

        // Local speaking RMS.
        let rms = rms(&frame);
        if rms > SPEAK_THRESHOLD {
            if !speaking {
                speaking = true;
                let _ = local_speaking_tx.send(true);
            }
            consec_below = 0;
        } else {
            consec_below = consec_below.saturating_add(1);
            if speaking && consec_below >= SPEAK_HANGOVER_FRAMES {
                speaking = false;
                let _ = local_speaking_tx.send(false);
            }
        }

        // Gate.
        if !cfg.gate.pass(&frame) {
            seq = seq.saturating_add(1);
            continue;
        }
        // Mute (post-gate, post-APM -- wasted CPU acknowledged in spec).
        if muted.load(Ordering::Acquire) {
            seq = seq.saturating_add(1);
            continue;
        }

        // Encode.
        let pkt = match encoder.encode(&frame) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[voice::send] encode: {e}");
                seq = seq.saturating_add(1);
                continue;
            }
        };

        // Seal + wire.
        let frame_bytes = match seal_audio_packet_to_wire(
            &cfg.stream_key,
            seq,
            &cfg.session_id,
            &cfg.speaker_pk,
            &pkt,
        ) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[voice::send] seal: {e}");
                seq = seq.saturating_add(1);
                continue;
            }
        };

        (cfg.datagram_sink)(Bytes::from(frame_bytes));
        seq = seq.saturating_add(1);
    }
}

fn rms(pcm: &[f32]) -> f32 {
    let sum: f32 = pcm.iter().map(|x| x * x).sum();
    (sum / pcm.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::PcmChunk;
    use std::sync::Mutex as StdMutex;

    fn make_sine_chunk(hz: f32, samples: usize) -> PcmChunk {
        let pcm: Vec<f32> = (0..samples)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / OPUS_SAMPLE_RATE as f32).sin() * 0.5)
            .collect();
        PcmChunk { samples: pcm, timestamp_ms: 0 }
    }

    fn build_cfg() -> (SendTaskConfig, mpsc::Sender<PcmChunk>, Arc<StdMutex<Vec<Bytes>>>) {
        let (tx, rx) = mpsc::channel::<PcmChunk>();
        let sink: Arc<StdMutex<Vec<Bytes>>> = Arc::new(StdMutex::new(Vec::new()));
        let sink_clone = sink.clone();
        let cfg = SendTaskConfig {
            pcm_rx: rx,
            apm: AudioProcessor::new(),
            gate: GateMode::Open,
            session_id: [7u8; 16],
            stream_key: [0xAA; 32],
            speaker_pk: [0xBB; 32],
            aec_ref: Arc::new(Mutex::new(vec![0.0; OPUS_FRAME_SAMPLES_MONO])),
            datagram_sink: Box::new(move |b: Bytes| {
                sink_clone.lock().unwrap().push(b);
            }),
        };
        (cfg, tx, sink)
    }

    #[test]
    fn open_mic_emits_one_datagram_per_chunk() {
        let (cfg, tx, sink) = build_cfg();
        let muted = Arc::new(AtomicBool::new(false));
        let (speak_tx, _speak_rx) = watch::channel(false);

        for _ in 0..5 {
            tx.send(make_sine_chunk(440.0, OPUS_FRAME_SAMPLES_MONO)).unwrap();
        }
        drop(tx);
        run(cfg, muted, speak_tx);

        let emitted = sink.lock().unwrap().len();
        assert_eq!(emitted, 5, "expected 5 datagrams, got {emitted}");
    }

    #[test]
    fn muted_drops_all_frames() {
        let (cfg, tx, sink) = build_cfg();
        let muted = Arc::new(AtomicBool::new(true));
        let (speak_tx, _speak_rx) = watch::channel(false);

        for _ in 0..5 {
            tx.send(make_sine_chunk(440.0, OPUS_FRAME_SAMPLES_MONO)).unwrap();
        }
        drop(tx);
        run(cfg, muted, speak_tx);

        assert_eq!(sink.lock().unwrap().len(), 0, "muted send must emit nothing");
    }

    #[test]
    fn ptt_closed_drops_all_frames() {
        let (mut cfg, tx, sink) = build_cfg();
        let ptt = Arc::new(AtomicBool::new(false));
        cfg.gate = GateMode::Ptt(ptt);
        let muted = Arc::new(AtomicBool::new(false));
        let (speak_tx, _speak_rx) = watch::channel(false);

        for _ in 0..5 {
            tx.send(make_sine_chunk(440.0, OPUS_FRAME_SAMPLES_MONO)).unwrap();
        }
        drop(tx);
        run(cfg, muted, speak_tx);

        assert_eq!(sink.lock().unwrap().len(), 0, "Ptt(false) must block all frames");
    }
}
