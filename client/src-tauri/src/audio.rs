// client/src-tauri/src/audio.rs
//
// AudioBackend trait + mock implementation.
//
// Voice (Phase 3) replaces the `_ => mock` arm in make_audio_backend with
// a real cpal/audiopus-backed implementation. Until then, the factory
// returns the mock so dev work in WSL (no audio hardware) isn't blocked.

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct AudioInputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone)]
pub struct AudioOutputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
    /// Total f32 samples per chunk, interleaved across channels.
    /// e.g. 48000 sample_rate * 1 channel * 20ms / 1000 = 960
    pub samples_per_chunk: usize,
}

/// A chunk of f32 PCM samples in [-1.0, 1.0], interleaved across channels.
pub struct PcmChunk {
    pub samples: Vec<f32>,
    pub timestamp_ms: u64,
}

pub trait AudioBackend: Send + Sync {
    fn enumerate_input_devices(&self) -> Result<Vec<AudioInputDevice>, String>;
    fn enumerate_output_devices(&self) -> Result<Vec<AudioOutputDevice>, String>;
    fn start_capture(
        &self,
        device_id: Option<&str>,
        format: AudioFormat,
    ) -> Result<mpsc::Receiver<PcmChunk>, String>;
    fn stop_capture(&self) -> Result<(), String>;
    fn start_playback(
        &self,
        device_id: Option<&str>,
        format: AudioFormat,
    ) -> Result<mpsc::SyncSender<PcmChunk>, String>;
    fn stop_playback(&self) -> Result<(), String>;
    fn backend_name(&self) -> &'static str;
}

pub struct MockAudioBackend {
    capture: Mutex<Option<JoinHandle<()>>>,
    capture_stop: Mutex<Option<Arc<AtomicBool>>>,
    playback: Mutex<Option<JoinHandle<()>>>,
    playback_stop: Mutex<Option<Arc<AtomicBool>>>,
}

/// Read FARDER_MOCK_AUDIO_HZ env var; clamp to [20, 20_000]; default 440.
fn mock_audio_hz() -> f32 {
    std::env::var("FARDER_MOCK_AUDIO_HZ")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .map(|hz| hz.clamp(20.0, 20_000.0))
        .unwrap_or(440.0)
}

impl MockAudioBackend {
    pub fn new() -> Self {
        Self {
            capture: Mutex::new(None),
            capture_stop: Mutex::new(None),
            playback: Mutex::new(None),
            playback_stop: Mutex::new(None),
        }
    }
}

impl Default for MockAudioBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioBackend for MockAudioBackend {
    fn enumerate_input_devices(&self) -> Result<Vec<AudioInputDevice>, String> {
        Ok(vec![AudioInputDevice {
            id: "mock-input".into(),
            name: "Mock Input (sine wave)".into(),
            is_default: true,
        }])
    }
    fn enumerate_output_devices(&self) -> Result<Vec<AudioOutputDevice>, String> {
        Ok(vec![AudioOutputDevice {
            id: "mock-output".into(),
            name: "Mock Output (discard)".into(),
            is_default: true,
        }])
    }
    fn start_capture(
        &self,
        _device_id: Option<&str>,
        format: AudioFormat,
    ) -> Result<mpsc::Receiver<PcmChunk>, String> {
        let mut capture_slot = self.capture.lock().map_err(|e| e.to_string())?;
        if capture_slot.is_some() {
            return Err("capture already active".into());
        }

        let hz = mock_audio_hz();
        let sample_rate = format.sample_rate as f32;
        let channels = format.channels as usize;
        let samples_per_chunk = format.samples_per_chunk;
        if channels == 0 || samples_per_chunk == 0 || sample_rate <= 0.0 {
            return Err(format!("invalid AudioFormat: {:?}", format));
        }
        let frames_per_chunk = samples_per_chunk / channels;
        let chunk_period =
            Duration::from_secs_f32(frames_per_chunk as f32 / sample_rate);

        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        // Bounded channel — backpressure if consumer is slow. 8 chunks ≈ 160ms
        // at 20ms chunks, which is plenty of slack without unbounded memory.
        let (tx, rx) = mpsc::sync_channel::<PcmChunk>(8);

        let started = Instant::now();
        let handle = std::thread::spawn(move || {
            let mut frame_index: u64 = 0;
            while !stop_clone.load(Ordering::Relaxed) {
                let chunk_start = Instant::now();
                let mut samples = Vec::with_capacity(samples_per_chunk);
                for f in 0..frames_per_chunk {
                    let t = (frame_index + f as u64) as f32 / sample_rate;
                    let v = (2.0 * std::f32::consts::PI * hz * t).sin() * 0.7;
                    for _ in 0..channels {
                        samples.push(v);
                    }
                }
                let chunk = PcmChunk {
                    samples,
                    timestamp_ms: started.elapsed().as_millis() as u64,
                };
                // If the consumer is gone, exit cleanly.
                if tx.send(chunk).is_err() {
                    break;
                }
                frame_index += frames_per_chunk as u64;

                // Sleep until next chunk boundary.
                let elapsed = chunk_start.elapsed();
                if elapsed < chunk_period {
                    std::thread::sleep(chunk_period - elapsed);
                }
            }
        });

        *capture_slot = Some(handle);
        *self.capture_stop.lock().map_err(|e| e.to_string())? = Some(stop);
        Ok(rx)
    }

    fn stop_capture(&self) -> Result<(), String> {
        if let Some(stop) = self.capture_stop.lock().map_err(|e| e.to_string())?.take() {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(handle) = self.capture.lock().map_err(|e| e.to_string())?.take() {
            // Use a thread that times out the join after 200ms. If join doesn't
            // complete, we detach (let the thread die on next stop-flag check).
            let (done_tx, done_rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = handle.join();
                let _ = done_tx.send(());
            });
            match done_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(()) => {}
                Err(_) => eprintln!("[audio] mock capture thread did not join within 200ms"),
            }
        }
        Ok(())
    }

    fn start_playback(
        &self,
        _device_id: Option<&str>,
        _format: AudioFormat,
    ) -> Result<mpsc::SyncSender<PcmChunk>, String> {
        Err("not yet implemented".into())
    }
    fn stop_playback(&self) -> Result<(), String> {
        Err("not yet implemented".into())
    }
    fn backend_name(&self) -> &'static str {
        "mock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_enumerate_returns_one_input_one_output() {
        let backend = MockAudioBackend::new();

        let inputs = backend.enumerate_input_devices().unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].id, "mock-input");
        assert!(inputs[0].is_default);

        let outputs = backend.enumerate_output_devices().unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].id, "mock-output");
        assert!(outputs[0].is_default);
    }

    #[test]
    fn mock_capture_emits_chunks_at_expected_cadence() {
        let backend = MockAudioBackend::new();
        let format = AudioFormat {
            sample_rate: 48_000,
            channels: 1,
            samples_per_chunk: 960, // 20ms
        };
        let rx = backend.start_capture(None, format).unwrap();

        let start = Instant::now();
        for _ in 0..5 {
            rx.recv_timeout(Duration::from_millis(200)).unwrap();
        }
        let elapsed = start.elapsed();
        backend.stop_capture().unwrap();

        assert!(
            elapsed >= Duration::from_millis(80),
            "5 chunks should take at least ~80ms, got {elapsed:?}",
        );
        assert!(
            elapsed <= Duration::from_millis(200),
            "5 chunks should take no more than ~200ms, got {elapsed:?}",
        );
    }

    #[test]
    fn mock_capture_samples_are_nonzero() {
        let backend = MockAudioBackend::new();
        let format = AudioFormat {
            sample_rate: 48_000,
            channels: 1,
            samples_per_chunk: 960,
        };
        let rx = backend.start_capture(None, format).unwrap();
        let chunk = rx.recv_timeout(Duration::from_millis(200)).unwrap();
        backend.stop_capture().unwrap();

        let above_floor = chunk
            .samples
            .iter()
            .filter(|&&s| s.abs() > 0.01)
            .count();
        let frac = above_floor as f32 / chunk.samples.len() as f32;
        assert!(
            frac > 0.5,
            "expected >50% of samples > 0.01 abs (sine isn't silent); got {frac}",
        );
    }

    #[test]
    fn mock_stop_capture_terminates_within_200ms() {
        let backend = MockAudioBackend::new();
        let format = AudioFormat {
            sample_rate: 48_000,
            channels: 1,
            samples_per_chunk: 960,
        };
        let _rx = backend.start_capture(None, format).unwrap();
        // Let it run briefly.
        std::thread::sleep(Duration::from_millis(40));

        let stop_start = Instant::now();
        backend.stop_capture().unwrap();
        let stop_elapsed = stop_start.elapsed();
        assert!(
            stop_elapsed < Duration::from_millis(200),
            "stop_capture took {stop_elapsed:?}",
        );
    }

    #[test]
    fn mock_double_start_capture_returns_err() {
        let backend = MockAudioBackend::new();
        let format = AudioFormat {
            sample_rate: 48_000,
            channels: 1,
            samples_per_chunk: 960,
        };
        let _rx = backend.start_capture(None, format).unwrap();
        let result = backend.start_capture(None, format);
        backend.stop_capture().unwrap();
        assert!(result.is_err(), "second start_capture should be Err");
    }

    #[test]
    fn mock_env_var_overrides_frequency() {
        // Set FARDER_MOCK_AUDIO_HZ=880, capture 1 second of mono 48kHz audio,
        // count zero crossings, divide by 2 → measured Hz. Assert ±10% of 880.
        //
        // NOTE: env vars are process-global. If other tests in this module
        // ever read FARDER_MOCK_AUDIO_HZ, this test must serialize with them
        // (currently it's the only reader).
        let prev = std::env::var("FARDER_MOCK_AUDIO_HZ").ok();
        std::env::set_var("FARDER_MOCK_AUDIO_HZ", "880");

        let backend = MockAudioBackend::new();
        let format = AudioFormat {
            sample_rate: 48_000,
            channels: 1,
            samples_per_chunk: 4800, // 100ms
        };
        let rx = backend.start_capture(None, format).unwrap();

        let mut samples = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(1500);
        while samples.len() < 48_000 && Instant::now() < deadline {
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
                samples.extend(chunk.samples);
            }
        }
        backend.stop_capture().unwrap();

        // Restore env var.
        match prev {
            Some(v) => std::env::set_var("FARDER_MOCK_AUDIO_HZ", v),
            None => std::env::remove_var("FARDER_MOCK_AUDIO_HZ"),
        }

        assert!(samples.len() >= 48_000, "did not collect 1s of samples");
        let truncated = &samples[..48_000];
        let mut crossings = 0usize;
        for w in truncated.windows(2) {
            if w[0].is_sign_negative() != w[1].is_sign_negative() {
                crossings += 1;
            }
        }
        let measured_hz = crossings as f32 / 2.0;
        let lo = 880.0 * 0.9;
        let hi = 880.0 * 1.1;
        assert!(
            measured_hz >= lo && measured_hz <= hi,
            "measured Hz {measured_hz} outside [{lo}, {hi}]",
        );
    }
}
