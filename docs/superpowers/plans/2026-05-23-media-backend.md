# MediaBackend Abstraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `AudioBackend` + `DisplayBackend` traits with mock implementations and env-var-selected factories, so voice (Phase 3) and screensharing can be developed in parallel on WSL where real hardware is unavailable.

**Architecture:** Two new Rust modules in `client/src-tauri/src/`. Each owns its trait, mock impl, and factory. Mocks emit synthetic data (sine wave for audio, animated test pattern for display) through real `std::sync::mpsc` channels — same plumbing the real backends will use. Selection via `FARDER_AUDIO_BACKEND` / `FARDER_DISPLAY_BACKEND` env vars (default: mock-with-warn until real impls ship with the consumer sub-projects).

**Tech Stack:** Rust (Tauri 2 project). Stdlib only — `std::sync::{mpsc, atomic, Arc, Mutex}`, `std::thread`, `std::time`. No new `Cargo.toml` entries.

**Spec:** `docs/superpowers/specs/2026-05-23-media-backend-design.md`

---

## File structure

**Created:**
- `client/src-tauri/src/audio.rs` — types + `AudioBackend` trait + `MockAudioBackend` + `make_audio_backend()` factory + `log_once` helper + tests
- `client/src-tauri/src/display.rs` — types + `DisplayBackend` trait + 5×7 bitmap font constants + `MockDisplayBackend` + `make_display_backend()` factory + tests

**Modified:**
- `client/src-tauri/src/main.rs` — add `mod audio; mod display;` alongside existing module declarations

**Not touched:**
- `client/src-tauri/Cargo.toml` — no new deps
- No Tauri command wiring (commands ship with voice / screensharing sub-projects)
- No TS / protocol / server changes

---

## Phase 1: Audio module

## Task 1: audio.rs scaffold — types + AudioBackend trait + MockAudioBackend skeleton

**Files:**
- Create: `client/src-tauri/src/audio.rs`

- [ ] **Step 1: Create the file with types, trait, and an empty mock struct**

```rust
// client/src-tauri/src/audio.rs
//
// AudioBackend trait + mock implementation.
//
// Voice (Phase 3) replaces the `_ => mock` arm in make_audio_backend with
// a real cpal/audiopus-backed implementation. Until then, the factory
// returns the mock so dev work in WSL (no audio hardware) isn't blocked.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

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
        Err("not yet implemented".into())
    }
    fn enumerate_output_devices(&self) -> Result<Vec<AudioOutputDevice>, String> {
        Err("not yet implemented".into())
    }
    fn start_capture(
        &self,
        _device_id: Option<&str>,
        _format: AudioFormat,
    ) -> Result<mpsc::Receiver<PcmChunk>, String> {
        Err("not yet implemented".into())
    }
    fn stop_capture(&self) -> Result<(), String> {
        Err("not yet implemented".into())
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
```

- [ ] **Step 2: Wire the module into main.rs so it compiles**

In `client/src-tauri/src/main.rs`, find the cluster of `mod xxx;` declarations near the top. Add:

```rust
mod audio;
```

(Place it alphabetically with the others. There is no need to add anything to `tauri::generate_handler!` — no commands yet.)

- [ ] **Step 3: Verify cargo check**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -10
```

Expected: `Finished` (warnings about unused fields are fine — they get used by later tasks).

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/audio.rs client/src-tauri/src/main.rs
git -C /home/deez/farder commit -m "feat(client): audio.rs scaffold — AudioBackend trait + MockAudioBackend stub"
```

Use a HEREDOC and append the `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` trailer matching prior commits.

---

## Task 2: MockAudioBackend enumerate methods + tests

**Files:**
- Modify: `client/src-tauri/src/audio.rs`

- [ ] **Step 1: Implement enumerate_input_devices and enumerate_output_devices**

Replace the two `enumerate_*` stub methods with:

```rust
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
```

- [ ] **Step 2: Add tests module at the bottom of audio.rs**

Append to the end of the file:

```rust
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
}
```

- [ ] **Step 3: Run the test**

```
cd /home/deez/farder/client/src-tauri && cargo test audio::tests 2>&1 | tail -10
```

Expected: `1 passed`.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/audio.rs
git -C /home/deez/farder commit -m "feat(client): MockAudioBackend enumerate methods + test"
```

---

## Task 3: MockAudioBackend capture (sine wave thread)

**Files:**
- Modify: `client/src-tauri/src/audio.rs`

This is the meatiest audio task. We add the capture-thread machinery and 5 tests.

- [ ] **Step 1: Add stdlib imports + sine-wave generation helper**

At the top of the file (after the existing imports), add:

```rust
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
```

Add a free helper function (above the `impl MockAudioBackend` block):

```rust
/// Read FARDER_MOCK_AUDIO_HZ env var; clamp to [20, 20_000]; default 440.
fn mock_audio_hz() -> f32 {
    std::env::var("FARDER_MOCK_AUDIO_HZ")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .map(|hz| hz.clamp(20.0, 20_000.0))
        .unwrap_or(440.0)
}
```

- [ ] **Step 2: Implement start_capture and stop_capture**

Replace the two stub methods:

```rust
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
```

- [ ] **Step 3: Add capture tests to the tests module**

Append inside the existing `mod tests` block:

```rust
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
```

- [ ] **Step 4: Run all audio tests**

```
cd /home/deez/farder/client/src-tauri && cargo test audio::tests 2>&1 | tail -15
```

Expected: `6 passed` (1 from Task 2 + 5 added here).

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/audio.rs
git -C /home/deez/farder commit -m "feat(client): MockAudioBackend capture (sine wave) + 5 tests"
```

---

## Task 4: MockAudioBackend playback

**Files:**
- Modify: `client/src-tauri/src/audio.rs`

Playback consumes and discards. No test in the spec — the consumer-side behavior matters for voice (Phase 3) but the mock is just a sink.

- [ ] **Step 1: Implement start_playback and stop_playback**

Replace the two stub methods:

```rust
    fn start_playback(
        &self,
        _device_id: Option<&str>,
        format: AudioFormat,
    ) -> Result<mpsc::SyncSender<PcmChunk>, String> {
        let mut playback_slot = self.playback.lock().map_err(|e| e.to_string())?;
        if playback_slot.is_some() {
            return Err("playback already active".into());
        }

        if format.channels == 0 || format.samples_per_chunk == 0 {
            return Err(format!("invalid AudioFormat: {:?}", format));
        }
        // SyncSender buffer sized for ~500ms of audio at the requested format.
        // chunks_per_500ms = (sample_rate * 0.5) / (samples_per_chunk / channels)
        let frames_per_chunk = format.samples_per_chunk / format.channels as usize;
        let chunks_per_500ms = ((format.sample_rate as f32 * 0.5)
            / frames_per_chunk.max(1) as f32)
            .ceil() as usize;
        let buffer = chunks_per_500ms.max(2);

        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let (tx, rx) = mpsc::sync_channel::<PcmChunk>(buffer);

        let handle = std::thread::spawn(move || {
            while !stop_clone.load(Ordering::Relaxed) {
                // Drain with a short timeout so the stop flag is honoured promptly.
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(_chunk) => {
                        // Mock discards. Future: increment a counter for stats.
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });

        *playback_slot = Some(handle);
        *self.playback_stop.lock().map_err(|e| e.to_string())? = Some(stop);
        Ok(tx)
    }

    fn stop_playback(&self) -> Result<(), String> {
        if let Some(stop) = self.playback_stop.lock().map_err(|e| e.to_string())?.take() {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(handle) = self.playback.lock().map_err(|e| e.to_string())?.take() {
            let (done_tx, done_rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = handle.join();
                let _ = done_tx.send(());
            });
            match done_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(()) => {}
                Err(_) => eprintln!("[audio] mock playback thread did not join within 200ms"),
            }
        }
        Ok(())
    }
```

- [ ] **Step 2: Verify cargo check**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
```

Expected: `Finished`.

- [ ] **Step 3: Verify all existing audio tests still pass**

```
cd /home/deez/farder/client/src-tauri && cargo test audio::tests 2>&1 | tail -10
```

Expected: `6 passed`.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/audio.rs
git -C /home/deez/farder commit -m "feat(client): MockAudioBackend playback (consume + discard)"
```

---

## Task 5: make_audio_backend factory + log_once helper

**Files:**
- Modify: `client/src-tauri/src/audio.rs`

- [ ] **Step 1: Add the log_once helper at the top of audio.rs (after imports)**

```rust
use std::collections::HashSet;
use std::sync::OnceLock;

/// Log a message at most once per process, keyed by `tag`. Used by factory
/// functions to warn-on-fallback without spamming. When display.rs lands
/// (Task 9), promote this to a shared utility module.
fn log_once(tag: &'static str, message: &str) {
    static SEEN: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = seen.lock().expect("log_once mutex poisoned");
    if guard.insert(tag) {
        eprintln!("{message}");
    }
}
```

- [ ] **Step 2: Add the factory function at the bottom of audio.rs (before the tests module)**

```rust
/// Construct an AudioBackend based on the FARDER_AUDIO_BACKEND env var.
/// - "mock" → MockAudioBackend
/// - anything else (or unset) → real backend if shipped, else mock-with-warn
///
/// The voice (Phase 3) sub-project replaces the fallback arm with a real
/// cpal/audiopus-backed implementation.
pub fn make_audio_backend() -> Box<dyn AudioBackend> {
    match std::env::var("FARDER_AUDIO_BACKEND").as_deref() {
        Ok("mock") => Box::new(MockAudioBackend::new()),
        _ => {
            log_once(
                "audio.real_not_shipped",
                "[audio] real backend not yet shipped; using mock",
            );
            Box::new(MockAudioBackend::new())
        }
    }
}
```

- [ ] **Step 3: Verify cargo check + tests still pass**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
cd /home/deez/farder/client/src-tauri && cargo test audio::tests 2>&1 | tail -5
```

Expected: `Finished` and `6 passed`.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/audio.rs
git -C /home/deez/farder commit -m "feat(client): make_audio_backend factory + log_once helper"
```

---

## Phase 2: Display module

## Task 6: display.rs scaffold — types + DisplayBackend trait + 5×7 font + MockDisplayBackend skeleton

**Files:**
- Create: `client/src-tauri/src/display.rs`
- Modify: `client/src-tauri/src/main.rs`

- [ ] **Step 1: Create display.rs with types, trait, font, and an empty mock**

```rust
// client/src-tauri/src/display.rs
//
// DisplayBackend trait + mock implementation.
//
// Screensharing replaces the `_ => mock` arm in make_display_backend with
// a real scrap/native-backed implementation. Until then, the factory
// returns the mock so dev work in WSL (no display capture) isn't blocked.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub enum DisplaySourceKind {
    Screen,
    Window,
}

#[derive(Debug, Clone)]
pub struct DisplaySource {
    pub id: String,
    pub kind: DisplaySourceKind,
    pub label: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct DisplayFormat {
    pub fps: u32,
    pub max_width: u32,
    pub max_height: u32,
}

/// A captured frame in RGBA8888, row-major, packed (stride = width * 4).
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub pixels: Vec<u8>,
    pub timestamp_ms: u64,
}

pub trait DisplayBackend: Send + Sync {
    fn enumerate_sources(&self) -> Result<Vec<DisplaySource>, String>;
    fn start_capture(
        &self,
        source_id: &str,
        format: DisplayFormat,
    ) -> Result<mpsc::Receiver<VideoFrame>, String>;
    fn stop_capture(&self) -> Result<(), String>;
    fn backend_name(&self) -> &'static str;
}

// ---------------------------------------------------------------------------
// 5×7 bitmap font for digits 0–9. Each row is a u8 where the low 5 bits map
// to pixels left-to-right (bit 4 = leftmost). Used by the mock display to
// render a frame counter overlay; that's all — no full font system.
// ---------------------------------------------------------------------------
const DIGIT_FONT: [[u8; 7]; 10] = [
    // 0
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
    // 1
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
    // 2
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
    // 3
    [0b01110, 0b10001, 0b00001, 0b00110, 0b00001, 0b10001, 0b01110],
    // 4
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
    // 5
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
    // 6
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
    // 7
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
    // 8
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
    // 9
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
];

pub struct MockDisplayBackend {
    capture: Mutex<Option<JoinHandle<()>>>,
    capture_stop: Mutex<Option<Arc<AtomicBool>>>,
}

impl MockDisplayBackend {
    pub fn new() -> Self {
        Self {
            capture: Mutex::new(None),
            capture_stop: Mutex::new(None),
        }
    }
}

impl Default for MockDisplayBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayBackend for MockDisplayBackend {
    fn enumerate_sources(&self) -> Result<Vec<DisplaySource>, String> {
        Err("not yet implemented".into())
    }
    fn start_capture(
        &self,
        _source_id: &str,
        _format: DisplayFormat,
    ) -> Result<mpsc::Receiver<VideoFrame>, String> {
        Err("not yet implemented".into())
    }
    fn stop_capture(&self) -> Result<(), String> {
        Err("not yet implemented".into())
    }
    fn backend_name(&self) -> &'static str {
        "mock"
    }
}
```

- [ ] **Step 2: Wire the module into main.rs**

In `client/src-tauri/src/main.rs`, find the cluster of `mod xxx;` declarations. Add (alphabetically with the others):

```rust
mod display;
```

- [ ] **Step 3: Verify cargo check**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
```

Expected: `Finished` (warnings about unused constants/fields are fine — they get used by Task 8).

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/display.rs client/src-tauri/src/main.rs
git -C /home/deez/farder commit -m "feat(client): display.rs scaffold — DisplayBackend + 5x7 digit font + mock stub"
```

---

## Task 7: MockDisplayBackend enumerate + test

**Files:**
- Modify: `client/src-tauri/src/display.rs`

- [ ] **Step 1: Implement enumerate_sources**

Replace the stub method:

```rust
    fn enumerate_sources(&self) -> Result<Vec<DisplaySource>, String> {
        Ok(vec![DisplaySource {
            id: "mock-display".into(),
            kind: DisplaySourceKind::Screen,
            label: "Mock Display 1280×720".into(),
            width: 1280,
            height: 720,
        }])
    }
```

- [ ] **Step 2: Add tests module at the bottom of display.rs**

Append:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_enumerate_returns_one_source() {
        let backend = MockDisplayBackend::new();
        let sources = backend.enumerate_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id, "mock-display");
        assert_eq!(sources[0].width, 1280);
        assert_eq!(sources[0].height, 720);
        assert!(matches!(sources[0].kind, DisplaySourceKind::Screen));
    }
}
```

- [ ] **Step 3: Run the test**

```
cd /home/deez/farder/client/src-tauri && cargo test display::tests 2>&1 | tail -5
```

Expected: `1 passed`.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/display.rs
git -C /home/deez/farder commit -m "feat(client): MockDisplayBackend enumerate + test"
```

---

## Task 8: MockDisplayBackend capture (gradient + frame counter) + tests

**Files:**
- Modify: `client/src-tauri/src/display.rs`

This is the meatiest display task — gradient generation, font-rendered frame counter, and 5 tests.

- [ ] **Step 1: Add HSV-to-RGB helper above the impl blocks**

```rust
/// HSV→RGB conversion. h in [0, 360), s+v in [0, 1]. Returns (r, g, b) as u8.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let h_prime = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (h_prime % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match h_prime as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let to_u8 = |f: f32| ((f + m) * 255.0).clamp(0.0, 255.0) as u8;
    (to_u8(r1), to_u8(g1), to_u8(b1))
}

/// Draw `digit` (0..=9) at position (x, y) into a packed RGBA8888 buffer of
/// `stride` bytes per row. Each lit pixel becomes black on white background
/// (digit is 5×7 with a 1px white margin on all sides → 7×9 footprint).
fn draw_digit(buf: &mut [u8], stride: usize, x: usize, y: usize, digit: u32) {
    let glyph_idx = (digit % 10) as usize;
    let glyph = &DIGIT_FONT[glyph_idx];
    // 7x9 white background.
    for dy in 0..9 {
        for dx in 0..7 {
            let px = (y + dy) * stride + (x + dx) * 4;
            if px + 3 < buf.len() {
                buf[px] = 255;
                buf[px + 1] = 255;
                buf[px + 2] = 255;
                buf[px + 3] = 255;
            }
        }
    }
    // 5x7 glyph in the inner area (offset by 1, 1).
    for (row_idx, row_bits) in glyph.iter().enumerate() {
        for col_idx in 0..5 {
            let lit = (row_bits >> (4 - col_idx)) & 1 == 1;
            if lit {
                let px = (y + 1 + row_idx) * stride + (x + 1 + col_idx) * 4;
                if px + 3 < buf.len() {
                    buf[px] = 0;
                    buf[px + 1] = 0;
                    buf[px + 2] = 0;
                    buf[px + 3] = 255;
                }
            }
        }
    }
}
```

- [ ] **Step 2: Implement start_capture and stop_capture**

Replace the two stub methods:

```rust
    fn start_capture(
        &self,
        _source_id: &str,
        format: DisplayFormat,
    ) -> Result<mpsc::Receiver<VideoFrame>, String> {
        let mut capture_slot = self.capture.lock().map_err(|e| e.to_string())?;
        if capture_slot.is_some() {
            return Err("capture already active".into());
        }
        if format.fps == 0 || format.max_width == 0 || format.max_height == 0 {
            return Err(format!("invalid DisplayFormat: {:?}", format));
        }

        let width = format.max_width.min(1280) as usize;
        let height = format.max_height.min(720) as usize;
        let stride = width * 4;
        let frame_period = Duration::from_secs_f32(1.0 / format.fps as f32);

        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        // Bounded channel — 4 frames of slack.
        let (tx, rx) = mpsc::sync_channel::<VideoFrame>(4);

        let started = Instant::now();
        let handle = std::thread::spawn(move || {
            // Reusable pixel buffer, cloned per frame for the channel.
            let mut buf = vec![0u8; height * stride];
            let mut frame_count: u32 = 0;
            while !stop_clone.load(Ordering::Relaxed) {
                let frame_start = Instant::now();

                // HSV gradient: hue rotates with frame_count; saturation 1.0;
                // value gradient diagonally across the frame.
                let hue_base = (frame_count as f32 * 2.0) % 360.0;
                for y in 0..height {
                    for x in 0..width {
                        let h = (hue_base + (x as f32 / width as f32) * 60.0) % 360.0;
                        let v = (x as f32 + y as f32) / (width as f32 + height as f32);
                        let (r, g, b) = hsv_to_rgb(h, 1.0, v);
                        let px = y * stride + x * 4;
                        buf[px] = r;
                        buf[px + 1] = g;
                        buf[px + 2] = b;
                        buf[px + 3] = 255;
                    }
                }

                // Render frame counter in top-left corner. Each digit is 7px
                // wide (font 5px + 1px margin each side); space them with 1
                // additional pixel of gap.
                let digits: Vec<u32> = {
                    let mut n = frame_count;
                    if n == 0 {
                        vec![0]
                    } else {
                        let mut d = Vec::new();
                        while n > 0 {
                            d.push(n % 10);
                            n /= 10;
                        }
                        d.reverse();
                        d
                    }
                };
                let mut text_x = 4usize;
                for digit in digits {
                    draw_digit(&mut buf, stride, text_x, 4, digit);
                    text_x += 8;
                    if text_x + 8 >= width {
                        break;
                    }
                }

                let frame = VideoFrame {
                    width: width as u32,
                    height: height as u32,
                    stride,
                    pixels: buf.clone(),
                    timestamp_ms: started.elapsed().as_millis() as u64,
                };
                if tx.send(frame).is_err() {
                    break;
                }
                frame_count = frame_count.wrapping_add(1);

                let elapsed = frame_start.elapsed();
                if elapsed < frame_period {
                    std::thread::sleep(frame_period - elapsed);
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
            let (done_tx, done_rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = handle.join();
                let _ = done_tx.send(());
            });
            match done_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(()) => {}
                Err(_) => eprintln!("[display] mock capture thread did not join within 200ms"),
            }
        }
        Ok(())
    }
```

- [ ] **Step 3: Add capture tests to the tests module**

Append inside the existing `mod tests` block:

```rust
    #[test]
    fn mock_capture_emits_frames_at_expected_fps() {
        let backend = MockDisplayBackend::new();
        let format = DisplayFormat {
            fps: 30,
            max_width: 1280,
            max_height: 720,
        };
        let rx = backend.start_capture("mock-display", format).unwrap();

        let start = Instant::now();
        for _ in 0..5 {
            rx.recv_timeout(Duration::from_millis(500)).unwrap();
        }
        let elapsed = start.elapsed();
        backend.stop_capture().unwrap();

        assert!(
            elapsed >= Duration::from_millis(100),
            "5 frames @30fps should take at least ~100ms, got {elapsed:?}",
        );
        assert!(
            elapsed <= Duration::from_millis(400),
            "5 frames @30fps should take no more than ~400ms (slack for rendering), got {elapsed:?}",
        );
    }

    #[test]
    fn mock_capture_frames_are_correct_size() {
        let backend = MockDisplayBackend::new();
        let format = DisplayFormat {
            fps: 30,
            max_width: 640,
            max_height: 360,
        };
        let rx = backend.start_capture("mock-display", format).unwrap();
        let frame = rx.recv_timeout(Duration::from_millis(500)).unwrap();
        backend.stop_capture().unwrap();

        assert_eq!(frame.width, 640);
        assert_eq!(frame.height, 360);
        assert_eq!(frame.stride, 640 * 4);
        assert_eq!(frame.pixels.len(), 640 * 360 * 4);
    }

    #[test]
    fn mock_capture_frames_have_visible_gradient() {
        let backend = MockDisplayBackend::new();
        let format = DisplayFormat {
            fps: 30,
            max_width: 1280,
            max_height: 720,
        };
        let rx = backend.start_capture("mock-display", format).unwrap();
        let frame = rx.recv_timeout(Duration::from_millis(500)).unwrap();
        backend.stop_capture().unwrap();

        // Sample 100 evenly-spaced pixels. Compute variance of R values; if
        // the frame is uniform black/white the variance is ~0. A gradient
        // produces variance well above 100 (R values span 0..255 ranges).
        let stride = frame.stride;
        let mut r_values: Vec<u8> = Vec::with_capacity(100);
        for i in 0..100 {
            let x = (i * frame.width as usize / 100).min(frame.width as usize - 1);
            let y = (i * frame.height as usize / 100).min(frame.height as usize - 1);
            r_values.push(frame.pixels[y * stride + x * 4]);
        }
        let mean = r_values.iter().map(|&v| v as f32).sum::<f32>() / 100.0;
        let variance = r_values
            .iter()
            .map(|&v| (v as f32 - mean).powi(2))
            .sum::<f32>()
            / 100.0;
        assert!(
            variance > 100.0,
            "expected gradient variance > 100; got {variance}",
        );
    }

    #[test]
    fn mock_stop_display_capture_terminates_within_200ms() {
        let backend = MockDisplayBackend::new();
        let format = DisplayFormat {
            fps: 30,
            max_width: 1280,
            max_height: 720,
        };
        let _rx = backend.start_capture("mock-display", format).unwrap();
        std::thread::sleep(Duration::from_millis(50));

        let stop_start = Instant::now();
        backend.stop_capture().unwrap();
        let stop_elapsed = stop_start.elapsed();
        assert!(
            stop_elapsed < Duration::from_millis(200),
            "stop_capture took {stop_elapsed:?}",
        );
    }

    #[test]
    fn mock_double_start_display_capture_returns_err() {
        let backend = MockDisplayBackend::new();
        let format = DisplayFormat {
            fps: 30,
            max_width: 1280,
            max_height: 720,
        };
        let _rx = backend.start_capture("mock-display", format).unwrap();
        let result = backend.start_capture("mock-display", format);
        backend.stop_capture().unwrap();
        assert!(result.is_err(), "second start_capture should be Err");
    }
```

- [ ] **Step 4: Run all display tests**

```
cd /home/deez/farder/client/src-tauri && cargo test display::tests 2>&1 | tail -15
```

Expected: `6 passed` (1 from Task 7 + 5 added here).

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/display.rs
git -C /home/deez/farder commit -m "feat(client): MockDisplayBackend capture (gradient + frame counter) + 5 tests"
```

---

## Task 9: make_display_backend factory

**Files:**
- Modify: `client/src-tauri/src/display.rs`
- Modify: `client/src-tauri/src/audio.rs` (re-export `log_once`)

The spec said "promote `log_once` to a shared utility module when the second user lands." `display.rs` is that second user. Rather than creating a brand-new `util.rs` for a 10-line helper, re-export it from `audio.rs` — it stays in one place, and `display.rs` calls `crate::audio::log_once`. If a third user ever appears, then promote.

- [ ] **Step 1: Make `log_once` accessible from outside audio.rs**

In `client/src-tauri/src/audio.rs`, find the `fn log_once(...)` declaration (added in Task 5). Change the visibility to `pub(crate)`:

```rust
pub(crate) fn log_once(tag: &'static str, message: &str) {
```

- [ ] **Step 2: Add the factory at the bottom of display.rs (before the tests module)**

```rust
/// Construct a DisplayBackend based on the FARDER_DISPLAY_BACKEND env var.
/// - "mock" → MockDisplayBackend
/// - anything else (or unset) → real backend if shipped, else mock-with-warn
///
/// The screensharing sub-project replaces the fallback arm with a real
/// scrap/native-backed implementation.
pub fn make_display_backend() -> Box<dyn DisplayBackend> {
    match std::env::var("FARDER_DISPLAY_BACKEND").as_deref() {
        Ok("mock") => Box::new(MockDisplayBackend::new()),
        _ => {
            crate::audio::log_once(
                "display.real_not_shipped",
                "[display] real backend not yet shipped; using mock",
            );
            Box::new(MockDisplayBackend::new())
        }
    }
}
```

- [ ] **Step 3: Verify cargo check + all tests still pass**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
cd /home/deez/farder/client/src-tauri && cargo test audio::tests display::tests 2>&1 | tail -5
```

Expected: `Finished` and `12 passed`.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/display.rs client/src-tauri/src/audio.rs
git -C /home/deez/farder commit -m "feat(client): make_display_backend factory + share log_once with audio.rs"
```

---

## Task 10: Final smoke + verify against spec

**Files:**
- None (verification only)

- [ ] **Step 1: Final cargo check across the whole client crate**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
```

Expected: `Finished`. Pre-existing warnings are fine.

- [ ] **Step 2: Run all media tests together**

```
cd /home/deez/farder/client/src-tauri && cargo test audio::tests display::tests 2>&1 | tail -20
```

Expected: `12 passed` (6 audio + 6 display).

- [ ] **Step 3: Verify env var selection works**

```
cd /home/deez/farder/client/src-tauri && cargo build --bin farder-client 2>&1 | tail -5
```

Expected: `Finished`.

Optional manual check — confirm the factory respects the env var by checking the backend name string:

```
cd /home/deez/farder/client/src-tauri && cargo test --doc 2>&1 | tail -5
```

(Doc tests aren't required by the spec — skip if there aren't any. This step's purpose is to verify the cargo invocation paths still work after the new modules land.)

- [ ] **Step 4: No CHANGELOG entry**

This sub-project ships infrastructure with no user-visible behavior change. The CHANGELOG entry waits until voice (Phase 3) lands and exercises the abstraction end-to-end — at that point a single entry covers "voice calling Phase 3 + MediaBackend abstraction it builds on".

- [ ] **Step 5: No final commit**

Step 1 and Step 2 are read-only verifications; nothing to commit here. The plan ends.

---

## Self-review notes

- **Spec coverage:**
  - Architecture / file layout → Tasks 1, 6, 10 (main.rs wiring)
  - AudioBackend trait → Task 1
  - DisplayBackend trait → Task 6
  - MockAudioBackend (enumerate, capture, playback) → Tasks 2, 3, 4
  - MockDisplayBackend (enumerate, capture) → Tasks 7, 8
  - 5×7 bitmap font → Task 6 (declared) + Task 8 (used by draw_digit)
  - make_audio_backend + log_once → Task 5
  - make_display_backend + log_once promotion → Task 9
  - 6 audio tests → Tasks 2 (1) + 3 (5)
  - 6 display tests → Tasks 7 (1) + 8 (5)
  - Final smoke → Task 10
- **Placeholder scan:** searched the plan for "TBD", "TODO", "fill in", "add appropriate" — none.
- **Type consistency:** `AudioFormat { sample_rate, channels, samples_per_chunk }` used identically across Tasks 1-5. `DisplayFormat { fps, max_width, max_height }` used identically across Tasks 6-9. `PcmChunk { samples, timestamp_ms }` and `VideoFrame { width, height, stride, pixels, timestamp_ms }` consistent.
- **`log_once` location:** spec says "promote to shared utility module when the second user lands." Task 9 promotes via `pub(crate)` re-export rather than creating a new `util.rs` — same outcome, less ceremony, single source of truth. If a third user appears (e.g., a future input-method backend), THEN spin up `util.rs`.
- **`backend_name` not directly tested** — it's accessor-only and unlikely to drift. Acceptable to defer.
- **The "real backend default" warning logged in Task 5 / Task 9** will fire on every Tauri startup until voice/screensharing ship their real impls. `log_once` ensures it appears at most once per process run, so it's not noisy.
