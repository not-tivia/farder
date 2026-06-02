// client/src-tauri/src/voice/mixer.rs
//
// Mixer task: drains each registered PeerPcmRing once per tick, sums into
// one mono frame, soft-clips, sends to AudioBackend playback, and stores
// the mixed frame as the AEC render reference for the SendTask.
//
// Pacing is provided by the AudioBackend's SyncSender backpressure — when
// playback is consuming at audio rate, send() blocks at the right cadence.
//
// See docs/superpowers/specs/2026-05-26-voice-client-pipeline-design.md.

use crate::audio::PcmChunk;
use crate::opus_codec::OPUS_FRAME_SAMPLES_MONO;
use crate::voice::recv::PeerPcmRing;
use crate::voice::SessionId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};

/// Shared registry of active peer rings paired with a per-peer gain atomic
/// (holds `f32::to_bits`). VoiceController inserts/removes as `TrackEnabled` /
/// `TrackDisabled` events arrive; the command layer updates the gain live.
pub type PeerRings = Arc<Mutex<HashMap<SessionId, (Arc<PeerPcmRing>, Arc<AtomicU32>)>>>;

pub struct MixerTaskConfig {
    pub peer_rings: PeerRings,
    pub playback_tx: mpsc::SyncSender<PcmChunk>,
    /// Most-recent mixed PCM, fed back to SendTask's APM as AEC render reference.
    pub aec_ref: Arc<Mutex<Vec<f32>>>,
}

/// Run the mixer. Returns when the playback SyncSender is disconnected.
pub fn run(cfg: MixerTaskConfig) {
    loop {
        let mixed = mix_one_frame(&cfg.peer_rings);
        {
            let mut guard = cfg.aec_ref.lock().expect("aec_ref poisoned");
            *guard = mixed.clone();
        }
        let chunk = PcmChunk { samples: mixed, timestamp_ms: 0 };
        if cfg.playback_tx.send(chunk).is_err() {
            break; // playback consumer gone
        }
    }
}

fn mix_one_frame(peer_rings: &PeerRings) -> Vec<f32> {
    let rings = peer_rings.lock().expect("peer_rings poisoned");
    let mut acc = vec![0.0f32; OPUS_FRAME_SAMPLES_MONO];
    for (ring, gain_bits) in rings.values() {
        let gain = f32::from_bits(gain_bits.load(Ordering::Acquire));
        let frame = ring.pop_frame();
        for (i, s) in frame.iter().enumerate() {
            if i < acc.len() {
                acc[i] += *s * gain;
            }
        }
    }
    for s in acc.iter_mut() {
        *s = soft_clip(*s);
    }
    acc
}

fn soft_clip(x: f32) -> f32 {
    x / (1.0 + x.abs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opus_codec::OPUS_SAMPLE_RATE;

    fn gain_bits(g: f32) -> Arc<AtomicU32> {
        Arc::new(AtomicU32::new(g.to_bits()))
    }

    fn make_ring_with_sine(hz: f32, n_frames: usize) -> Arc<PeerPcmRing> {
        let ring = Arc::new(PeerPcmRing::new(n_frames + 2));
        for f in 0..n_frames {
            let frame: Vec<f32> = (0..OPUS_FRAME_SAMPLES_MONO)
                .map(|i| {
                    let t = (f * OPUS_FRAME_SAMPLES_MONO + i) as f32 / OPUS_SAMPLE_RATE as f32;
                    (2.0 * std::f32::consts::PI * hz * t).sin() * 0.5
                })
                .collect();
            ring.push_frame(&frame);
        }
        ring
    }

    /// Test harness: run mixer for `n` frames then close playback to stop it.
    fn run_for_n_frames(cfg: MixerTaskConfig, n: usize) -> Vec<PcmChunk> {
        let (capture_tx, capture_rx) = mpsc::sync_channel::<PcmChunk>(n + 4);
        let cfg_with_capture = MixerTaskConfig {
            peer_rings: cfg.peer_rings,
            playback_tx: capture_tx,
            aec_ref: cfg.aec_ref,
        };
        let handle = std::thread::spawn(move || run(cfg_with_capture));
        let mut chunks = Vec::with_capacity(n);
        for _ in 0..n {
            match capture_rx.recv_timeout(std::time::Duration::from_millis(500)) {
                Ok(c) => chunks.push(c),
                Err(_) => break,
            }
        }
        // Drop receiver to unblock mixer's next send -> run exits.
        drop(capture_rx);
        let _ = handle.join();
        chunks
    }

    #[test]
    fn empty_registry_emits_silence() {
        let peer_rings: PeerRings = Default::default();
        let (tx, rx) = mpsc::sync_channel::<PcmChunk>(4);
        let cfg = MixerTaskConfig {
            peer_rings,
            playback_tx: tx,
            aec_ref: Arc::new(Mutex::new(vec![])),
        };
        let chunks = run_for_n_frames(cfg, 3);
        assert!(!chunks.is_empty(), "mixer should emit chunks even with no peers");
        for chunk in &chunks {
            assert_eq!(chunk.samples.len(), OPUS_FRAME_SAMPLES_MONO);
            assert!(chunk.samples.iter().all(|&s| s == 0.0), "no peers -> silence");
        }
        let _ = rx;
    }

    #[test]
    fn single_peer_passes_audible_signal_through() {
        let peer_rings: PeerRings = Default::default();
        peer_rings.lock().unwrap().insert([1u8; 16], (make_ring_with_sine(440.0, 5), gain_bits(1.0)));
        let (tx, rx) = mpsc::sync_channel::<PcmChunk>(4);
        let cfg = MixerTaskConfig {
            peer_rings,
            playback_tx: tx,
            aec_ref: Arc::new(Mutex::new(vec![])),
        };
        let chunks = run_for_n_frames(cfg, 3);
        let first = chunks.first().expect("expected at least one chunk");
        let energy: f32 = first.samples.iter().map(|x| x * x).sum();
        assert!(energy > 0.0, "single-peer chunk must have non-zero energy");
        let _ = rx;
    }

    #[test]
    fn two_peers_sum_stays_within_soft_clip_bounds() {
        let peer_rings: PeerRings = Default::default();
        peer_rings.lock().unwrap().insert([1u8; 16], (make_ring_with_sine(440.0, 5), gain_bits(1.0)));
        peer_rings.lock().unwrap().insert([2u8; 16], (make_ring_with_sine(880.0, 5), gain_bits(1.0)));
        let (tx, rx) = mpsc::sync_channel::<PcmChunk>(4);
        let cfg = MixerTaskConfig {
            peer_rings,
            playback_tx: tx,
            aec_ref: Arc::new(Mutex::new(vec![])),
        };
        let chunks = run_for_n_frames(cfg, 3);
        for chunk in &chunks {
            for &s in &chunk.samples {
                assert!(s.abs() < 1.0, "soft clip must keep samples in (-1, 1), got {s}");
            }
        }
        let _ = rx;
    }

    #[test]
    fn zero_gain_silences_peer() {
        let peer_rings: PeerRings = Default::default();
        peer_rings
            .lock()
            .unwrap()
            .insert([1u8; 16], (make_ring_with_sine(440.0, 5), gain_bits(0.0)));
        let mixed = mix_one_frame(&peer_rings);
        assert!(mixed.iter().all(|&s| s == 0.0), "gain 0.0 must silence the peer");
    }

    #[test]
    fn double_gain_doubles_pre_clip_amplitude() {
        // Compare a single peer at gain 1.0 vs 2.0 on a low-amplitude sine
        // (well inside the soft-clip linear-ish region) sample-by-sample.
        let unity: PeerRings = Default::default();
        unity
            .lock()
            .unwrap()
            .insert([1u8; 16], (make_ring_with_sine(440.0, 5), gain_bits(1.0)));
        let doubled: PeerRings = Default::default();
        doubled
            .lock()
            .unwrap()
            .insert([1u8; 16], (make_ring_with_sine(440.0, 5), gain_bits(2.0)));
        let a = mix_one_frame(&unity);
        let b = mix_one_frame(&doubled);
        // For at least one clearly non-zero sample, doubled magnitude > unity.
        let idx = a.iter().position(|&s| s.abs() > 0.05).expect("some signal");
        assert!(b[idx].abs() > a[idx].abs(), "gain 2.0 must increase amplitude");
    }

    #[test]
    fn soft_clip_bounds() {
        assert!(soft_clip(0.5) > 0.0 && soft_clip(0.5) < 0.5);
        assert!(soft_clip(100.0) < 1.0);
        assert!(soft_clip(-100.0) > -1.0);
        assert_eq!(soft_clip(0.0), 0.0);
    }
}
