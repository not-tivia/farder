# Real CpalAudioBackend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a real `AudioBackend` implementation backed by `cpal` so voice (#3.3) can capture from microphones and play to speakers, with a transparent WSL fallback to the existing mock.

**Architecture:** New module `client/src-tauri/src/audio_cpal.rs` exposing `CpalAudioBackend` (implements the `AudioBackend` trait from `audio.rs`). cpal's audio-thread callback bridges into the trait's `mpsc::sync_channel`; full-channel drops on capture, underrun-silence on playback. `SendWrapper` provides Send + Sync over cpal's platform-variable Send-ness. Factory `make_audio_backend()` auto-detects: env var explicit wins, otherwise real-if-devices-else-mock.

**Tech Stack:** Rust (Tauri 2). Existing `cpal = "0.15"`. New: `send_wrapper = "0.6"` (single small RustCrypto-ecosystem dep). No protocol/server/TS changes.

**Spec:** `docs/superpowers/specs/2026-05-25-cpal-audio-backend-design.md`

---

## File structure

**Created:**
- `client/src-tauri/src/audio_cpal.rs` — `CpalAudioBackend` struct + all 7 trait method impls + unit tests

**Modified:**
- `client/src-tauri/Cargo.toml` — add `send_wrapper = "0.6"`
- `client/src-tauri/src/main.rs` — add `mod audio_cpal;`
- `client/src-tauri/src/audio.rs` — update factory `make_audio_backend()` to prefer real when devices exist

---

## Phase 1: Backend scaffold

## Task 1: audio_cpal.rs scaffold — types + struct + AudioBackend stubs

**Files:**
- Create: `client/src-tauri/src/audio_cpal.rs`
- Modify: `client/src-tauri/Cargo.toml`
- Modify: `client/src-tauri/src/main.rs`

- [ ] **Step 1: Add `send_wrapper` dep**

In `client/src-tauri/Cargo.toml`, add to `[dependencies]` (alphabetically after `serde` or wherever fits):
```toml
send_wrapper = "0.6"
```

- [ ] **Step 2: Add `mod audio_cpal;` to main.rs**

In `client/src-tauri/src/main.rs`, find the cluster of `mod xxx;` declarations near the top. Insert alphabetically between `mod audio;` and `mod book;`:
```rust
mod audio_cpal;
```

- [ ] **Step 3: Create audio_cpal.rs with the struct + Err-returning trait stubs**

```rust
// client/src-tauri/src/audio_cpal.rs
//
// Real AudioBackend backed by cpal. Bridges cpal's callback-based audio
// API into the AudioBackend trait's `mpsc` channel model.
//
// See docs/superpowers/specs/2026-05-25-cpal-audio-backend-design.md.

use crate::audio::{
    AudioBackend, AudioFormat, AudioInputDevice, AudioOutputDevice, PcmChunk,
};
use cpal::traits::{DeviceTrait, HostTrait};
use send_wrapper::SendWrapper;
use std::sync::mpsc;
use std::sync::Mutex;

pub struct CpalAudioBackend {
    host: SendWrapper<cpal::Host>,
    capture_stream: Mutex<Option<SendWrapper<cpal::Stream>>>,
    playback_stream: Mutex<Option<SendWrapper<cpal::Stream>>>,
}

impl CpalAudioBackend {
    pub fn new() -> Self {
        Self {
            host: SendWrapper::new(cpal::default_host()),
            capture_stream: Mutex::new(None),
            playback_stream: Mutex::new(None),
        }
    }
}

impl Default for CpalAudioBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioBackend for CpalAudioBackend {
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
        "cpal"
    }
}
```

- [ ] **Step 4: Verify cargo check**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -10
```

Expected: `Finished`. May take 30-90 seconds the first time as `send_wrapper` downloads + compiles. Subsequent runs are fast.

If you see `error[E0277]: the trait bound \`SendWrapper<cpal::Stream>: Sync\` is not satisfied` — `SendWrapper` provides `Send` but not `Sync` until accessed; the `Mutex<Option<SendWrapper<...>>>` wrap is what gives us `Sync` for the outer struct. If TS-style errors appear that look related to thread bounds, double-check the `Mutex` wrapping.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/audio_cpal.rs client/src-tauri/src/main.rs client/src-tauri/Cargo.toml client/src-tauri/Cargo.lock
git -C /home/deez/farder commit -m "feat(client): audio_cpal.rs scaffold — CpalAudioBackend stub"
```

Use a HEREDOC + the `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` trailer (see `git log -1` for format).

---

## Task 2: enumerate_input_devices + enumerate_output_devices + tests

**Files:**
- Modify: `client/src-tauri/src/audio_cpal.rs`

- [ ] **Step 1: Implement both enumerate methods**

Replace the two stub methods in the `impl AudioBackend for CpalAudioBackend` block:

```rust
    fn enumerate_input_devices(&self) -> Result<Vec<AudioInputDevice>, String> {
        let devices = self.host.input_devices()
            .map_err(|e| format!("input_devices: {e}"))?;
        let default = self.host.default_input_device()
            .and_then(|d| d.name().ok());
        let mut out = Vec::new();
        for (i, dev) in devices.enumerate() {
            let name = dev.name().unwrap_or_else(|_| format!("device-{i}"));
            let is_default = default.as_deref() == Some(name.as_str());
            out.push(AudioInputDevice {
                id: name.clone(),
                name: name.clone(),
                is_default,
            });
        }
        Ok(out)
    }

    fn enumerate_output_devices(&self) -> Result<Vec<AudioOutputDevice>, String> {
        let devices = self.host.output_devices()
            .map_err(|e| format!("output_devices: {e}"))?;
        let default = self.host.default_output_device()
            .and_then(|d| d.name().ok());
        let mut out = Vec::new();
        for (i, dev) in devices.enumerate() {
            let name = dev.name().unwrap_or_else(|_| format!("device-{i}"));
            let is_default = default.as_deref() == Some(name.as_str());
            out.push(AudioOutputDevice {
                id: name.clone(),
                name: name.clone(),
                is_default,
            });
        }
        Ok(out)
    }
```

- [ ] **Step 2: Add the tests module**

Append at the end of `audio_cpal.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpal_backend_constructs_without_panicking() {
        // Smoke: succeeds even when zero audio devices are present (WSL).
        let _backend = CpalAudioBackend::new();
    }

    #[test]
    fn cpal_backend_name_is_cpal() {
        let backend = CpalAudioBackend::new();
        assert_eq!(backend.backend_name(), "cpal");
    }

    #[test]
    fn cpal_enumerate_input_devices_returns_vec() {
        let backend = CpalAudioBackend::new();
        // Result is Ok regardless of whether devices exist. Empty Vec on
        // WSL; non-empty on workstations. Either is fine.
        let _devices = backend.enumerate_input_devices().expect("enumerate input");
    }

    #[test]
    fn cpal_enumerate_output_devices_returns_vec() {
        let backend = CpalAudioBackend::new();
        let _devices = backend.enumerate_output_devices().expect("enumerate output");
    }
}
```

- [ ] **Step 3: Run tests**

```
cd /home/deez/farder/client/src-tauri && cargo test audio_cpal::tests 2>&1 | tail -10
```

Expected: 4 passed.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/audio_cpal.rs
git -C /home/deez/farder commit -m "feat(client): CpalAudioBackend enumerate input/output + tests"
```

---

## Phase 2: Capture path

## Task 3: start_capture + stop_capture (real cpal stream)

**Files:**
- Modify: `client/src-tauri/src/audio_cpal.rs`

- [ ] **Step 1: Add the capture-path imports + helpers above the impl**

At the top of `audio_cpal.rs`, after the existing `use` block, add:

```rust
use std::time::Instant;
```

Above the `impl AudioBackend for CpalAudioBackend` block (after the `Default` impl), add these helper functions:

```rust
/// Pick an input device by name, or the host's default if `device_id` is None.
fn pick_input_device(host: &cpal::Host, device_id: Option<&str>) -> Result<cpal::Device, String> {
    match device_id {
        None => host.default_input_device()
            .ok_or_else(|| "no default input device".to_string()),
        Some(name) => host.input_devices()
            .map_err(|e| format!("input_devices: {e}"))?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| format!("input device not found: {name}")),
    }
}

/// Find a SupportedStreamConfig on `device` whose sample rate matches
/// `format.sample_rate` exactly and whose channel count is either
/// `format.channels` (preferred) or 2 (we'll downmix to mono).
fn build_input_config(
    device: &cpal::Device,
    format: &AudioFormat,
) -> Result<cpal::StreamConfig, String> {
    use cpal::SampleRate;
    let want_sr = SampleRate(format.sample_rate);
    let want_channels = format.channels;
    let configs = device.supported_input_configs()
        .map_err(|e| format!("supported_input_configs: {e}"))?;
    // Prefer exact channel match, then fall back to stereo (downmix path).
    let mut exact_match: Option<cpal::SupportedStreamConfigRange> = None;
    let mut stereo_match: Option<cpal::SupportedStreamConfigRange> = None;
    for cfg in configs {
        if cfg.sample_format() != cpal::SampleFormat::F32 {
            continue;
        }
        if cfg.min_sample_rate() <= want_sr && want_sr <= cfg.max_sample_rate() {
            if cfg.channels() == want_channels {
                exact_match = Some(cfg);
                break;
            }
            if want_channels == 1 && cfg.channels() == 2 && stereo_match.is_none() {
                stereo_match = Some(cfg);
            }
        }
    }
    let chosen = exact_match.or(stereo_match)
        .ok_or_else(|| format!("no supported input config for {format:?}"))?;
    Ok(chosen.with_sample_rate(want_sr).config())
}
```

- [ ] **Step 2: Replace `start_capture` + `stop_capture` stubs**

In the `impl AudioBackend for CpalAudioBackend` block, replace the two stubs:

```rust
    fn start_capture(
        &self,
        device_id: Option<&str>,
        format: AudioFormat,
    ) -> Result<mpsc::Receiver<PcmChunk>, String> {
        let mut slot = self.capture_stream.lock()
            .map_err(|e| format!("capture lock: {e}"))?;
        if slot.is_some() {
            return Err("capture already active".into());
        }
        if format.channels == 0 || format.samples_per_chunk == 0 {
            return Err(format!("invalid AudioFormat: {format:?}"));
        }

        let device = pick_input_device(&self.host, device_id)?;
        let cpal_config = build_input_config(&device, &format)?;
        let dev_channels = cpal_config.channels as usize;
        let want_channels = format.channels as usize;
        let samples_per_chunk = format.samples_per_chunk;
        let (tx, rx) = mpsc::sync_channel::<PcmChunk>(8);

        let started = Instant::now();
        let mut buffered: Vec<f32> = Vec::with_capacity(samples_per_chunk);

        let stream = device.build_input_stream(
            &cpal_config,
            move |raw: &[f32], _info: &cpal::InputCallbackInfo| {
                // Walk cpal's interleaved samples one frame at a time. Downmix
                // stereo→mono if needed. Emit a PcmChunk each time we've
                // accumulated `samples_per_chunk` samples worth of output.
                let mut i = 0;
                while i + dev_channels <= raw.len() {
                    let sample = if dev_channels == want_channels {
                        raw[i]
                    } else if dev_channels == 2 && want_channels == 1 {
                        (raw[i] + raw[i + 1]) / 2.0
                    } else {
                        0.0 // unsupported channel mismatch
                    };
                    buffered.push(sample);
                    i += dev_channels;

                    if buffered.len() >= samples_per_chunk {
                        let chunk = PcmChunk {
                            samples: std::mem::take(&mut buffered),
                            timestamp_ms: started.elapsed().as_millis() as u64,
                        };
                        buffered.reserve(samples_per_chunk);
                        // Drop on full channel; never block the audio thread.
                        let _ = tx.try_send(chunk);
                    }
                }
            },
            |err| eprintln!("[audio] cpal capture error: {err}"),
            None,
        ).map_err(|e| format!("build_input_stream: {e}"))?;

        use cpal::traits::StreamTrait;
        stream.play().map_err(|e| format!("stream.play: {e}"))?;
        *slot = Some(SendWrapper::new(stream));
        Ok(rx)
    }

    fn stop_capture(&self) -> Result<(), String> {
        let mut slot = self.capture_stream.lock()
            .map_err(|e| format!("capture lock: {e}"))?;
        slot.take(); // drop the Stream; cpal joins its audio thread
        Ok(())
    }
```

- [ ] **Step 3: Verify cargo check**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
```

Expected: `Finished`. Warnings about unused fields on the playback path are fine (Task 4 lights them up).

- [ ] **Step 4: Run all audio_cpal tests**

```
cd /home/deez/farder/client/src-tauri && cargo test audio_cpal::tests 2>&1 | tail -10
```

Expected: 4 passed (the existing tests; no new tests this task — real capture/playback can't be tested without real audio hardware).

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/audio_cpal.rs
git -C /home/deez/farder commit -m "feat(client): CpalAudioBackend start_capture/stop_capture (real cpal stream)"
```

---

## Phase 3: Playback path

## Task 4: start_playback + stop_playback

**Files:**
- Modify: `client/src-tauri/src/audio_cpal.rs`

- [ ] **Step 1: Add output-path helpers**

After the existing `pick_input_device` / `build_input_config` helpers, add:

```rust
fn pick_output_device(host: &cpal::Host, device_id: Option<&str>) -> Result<cpal::Device, String> {
    match device_id {
        None => host.default_output_device()
            .ok_or_else(|| "no default output device".to_string()),
        Some(name) => host.output_devices()
            .map_err(|e| format!("output_devices: {e}"))?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| format!("output device not found: {name}")),
    }
}

fn build_output_config(
    device: &cpal::Device,
    format: &AudioFormat,
) -> Result<cpal::StreamConfig, String> {
    use cpal::SampleRate;
    let want_sr = SampleRate(format.sample_rate);
    let want_channels = format.channels;
    let configs = device.supported_output_configs()
        .map_err(|e| format!("supported_output_configs: {e}"))?;
    // Prefer exact channel match; otherwise stereo (we'll duplicate mono → stereo).
    let mut exact_match: Option<cpal::SupportedStreamConfigRange> = None;
    let mut stereo_match: Option<cpal::SupportedStreamConfigRange> = None;
    for cfg in configs {
        if cfg.sample_format() != cpal::SampleFormat::F32 {
            continue;
        }
        if cfg.min_sample_rate() <= want_sr && want_sr <= cfg.max_sample_rate() {
            if cfg.channels() == want_channels {
                exact_match = Some(cfg);
                break;
            }
            if want_channels == 1 && cfg.channels() == 2 && stereo_match.is_none() {
                stereo_match = Some(cfg);
            }
        }
    }
    let chosen = exact_match.or(stereo_match)
        .ok_or_else(|| format!("no supported output config for {format:?}"))?;
    Ok(chosen.with_sample_rate(want_sr).config())
}
```

- [ ] **Step 2: Replace `start_playback` + `stop_playback` stubs**

```rust
    fn start_playback(
        &self,
        device_id: Option<&str>,
        format: AudioFormat,
    ) -> Result<mpsc::SyncSender<PcmChunk>, String> {
        let mut slot = self.playback_stream.lock()
            .map_err(|e| format!("playback lock: {e}"))?;
        if slot.is_some() {
            return Err("playback already active".into());
        }
        if format.channels == 0 || format.samples_per_chunk == 0 {
            return Err(format!("invalid AudioFormat: {format:?}"));
        }

        let device = pick_output_device(&self.host, device_id)?;
        let cpal_config = build_output_config(&device, &format)?;
        let dev_channels = cpal_config.channels as usize;
        let want_channels = format.channels as usize;

        // Buffer sized for ~500ms of audio (matches the mock's behavior).
        let frames_per_chunk = (format.samples_per_chunk / want_channels).max(1);
        let chunks_per_500ms = ((format.sample_rate as f32 * 0.5)
            / frames_per_chunk as f32).ceil() as usize;
        let buf = chunks_per_500ms.max(2);
        let (tx, rx) = mpsc::sync_channel::<PcmChunk>(buf);

        // The receiver moves into the audio-thread closure. It is NOT Sync,
        // but the closure owns it singly — that's fine.
        let mut pending: Vec<f32> = Vec::new();

        let stream = device.build_output_stream(
            &cpal_config,
            move |out: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                // Refill `pending` from the channel as needed.
                while pending.len() < out.len() {
                    match rx.try_recv() {
                        Ok(chunk) => {
                            if dev_channels == want_channels {
                                pending.extend_from_slice(&chunk.samples);
                            } else if want_channels == 1 && dev_channels == 2 {
                                // Mono → stereo: duplicate each sample.
                                for &s in &chunk.samples {
                                    pending.push(s);
                                    pending.push(s);
                                }
                            } else {
                                pending.extend_from_slice(&chunk.samples);
                            }
                        }
                        Err(_) => break,
                    }
                }
                let take = pending.len().min(out.len());
                out[..take].copy_from_slice(&pending[..take]);
                pending.drain(..take);
                // Underrun → silence.
                for s in &mut out[take..] {
                    *s = 0.0;
                }
            },
            |err| eprintln!("[audio] cpal playback error: {err}"),
            None,
        ).map_err(|e| format!("build_output_stream: {e}"))?;

        use cpal::traits::StreamTrait;
        stream.play().map_err(|e| format!("stream.play: {e}"))?;
        *slot = Some(SendWrapper::new(stream));
        Ok(tx)
    }

    fn stop_playback(&self) -> Result<(), String> {
        let mut slot = self.playback_stream.lock()
            .map_err(|e| format!("playback lock: {e}"))?;
        slot.take();
        Ok(())
    }
```

- [ ] **Step 3: Verify cargo check + tests**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
cd /home/deez/farder/client/src-tauri && cargo test audio_cpal::tests 2>&1 | tail -10
```

Expected: `Finished` + 4 passed.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/audio_cpal.rs
git -C /home/deez/farder commit -m "feat(client): CpalAudioBackend start_playback/stop_playback"
```

---

## Phase 4: Factory integration

## Task 5: Update make_audio_backend to prefer real when devices exist

**Files:**
- Modify: `client/src-tauri/src/audio.rs`

- [ ] **Step 1: Update the factory function**

In `client/src-tauri/src/audio.rs`, find:

```rust
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

Replace with:

```rust
pub fn make_audio_backend() -> Box<dyn AudioBackend> {
    use cpal::traits::HostTrait;
    match std::env::var("FARDER_AUDIO_BACKEND").as_deref() {
        Ok("mock") => Box::new(MockAudioBackend::new()),
        Ok("real") => Box::new(crate::audio_cpal::CpalAudioBackend::new()),
        _ => {
            // Auto-detect: real if there's at least one input device, else mock.
            let host = cpal::default_host();
            let has_input = host.input_devices()
                .map(|mut it| it.next().is_some())
                .unwrap_or(false);
            if has_input {
                Box::new(crate::audio_cpal::CpalAudioBackend::new())
            } else {
                log_once(
                    "audio.no_input_devices",
                    "[audio] no input devices found; falling back to mock",
                );
                Box::new(MockAudioBackend::new())
            }
        }
    }
}
```

- [ ] **Step 2: Add a test asserting the env-var override works**

Append inside the existing `mod tests` block in `audio.rs`:

```rust
    #[test]
    fn make_audio_backend_mock_env_returns_mock() {
        let prev = std::env::var("FARDER_AUDIO_BACKEND").ok();
        std::env::set_var("FARDER_AUDIO_BACKEND", "mock");
        let backend = make_audio_backend();
        match prev {
            Some(v) => std::env::set_var("FARDER_AUDIO_BACKEND", v),
            None => std::env::remove_var("FARDER_AUDIO_BACKEND"),
        }
        assert_eq!(backend.backend_name(), "mock");
    }
```

(We deliberately do NOT add `make_audio_backend_real_env_returns_cpal` — that test would behave differently on a CI runner with no cpal default host. The auto-detect path is also not test-asserted for the same reason.)

- [ ] **Step 3: Run tests**

```
cd /home/deez/farder/client/src-tauri && cargo test audio::tests 2>&1 | tail -10
```

Expected: 7 passed (6 from sub-project #1 + 1 here).

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/audio.rs
git -C /home/deez/farder commit -m "feat(client): factory prefers real cpal backend when devices exist"
```

---

## Phase 5: Verification

## Task 6: Final smoke + workspace verification

**Files:**
- None (verification only)

- [ ] **Step 1: cargo check on the client**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
```

Expected: `Finished`. Pre-existing warnings are acceptable.

- [ ] **Step 2: Run all audio tests**

```
cd /home/deez/farder/client/src-tauri && cargo test audio_cpal::tests audio::tests 2>&1 | tail -15
```

Expected: total 11 passed (4 audio_cpal + 7 audio).

- [ ] **Step 3: Quick smoke — assert factory picks the right backend for WSL**

```
cd /home/deez/farder/client/src-tauri && FARDER_AUDIO_BACKEND= cargo test audio::tests::make_audio_backend_default 2>&1 | tail -5
```

(This test isn't added — but you can manually verify the factory by running a small bin or inspecting the test output.)

- [ ] **Step 4: TS check on the client UI**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -5
```

Expected: clean (no TS changes in this sub-project, but verify nothing leaked).

- [ ] **Step 5: No CHANGELOG entry**

This sub-project ships infrastructure with no user-visible behavior change. CHANGELOG entry waits for sub-project #3.3 (voice client pipeline) to exercise the abstraction end-to-end.

- [ ] **Step 6: No final commit**

Steps 1-4 are read-only verifications.

---

## Self-review notes

- **Spec coverage:**
  - CpalAudioBackend struct + state + Send/Sync via SendWrapper → Task 1
  - enumerate input/output → Task 2
  - start_capture (cpal callback → mpsc, downmix stereo→mono) → Task 3
  - start_playback (mpsc → cpal callback, mono→stereo upmix) → Task 4
  - Factory auto-detect with env var override + log_once + WSL fallback → Task 5
  - Tests (4 enumeration + 1 factory) → Tasks 2, 5
  - No CHANGELOG → Task 6 Step 5

- **Placeholder scan:** No "TBD" / "fill in details" / "add appropriate error handling" markers. Each step shows exact code.

- **Type consistency:**
  - `CpalAudioBackend` struct fields consistent across Tasks 1-5
  - `AudioFormat`, `PcmChunk`, `AudioInputDevice`, `AudioOutputDevice`, `AudioBackend` trait — all imported from `crate::audio` (the trait module from sub-project #1)
  - `pick_input_device` / `build_input_config` defined in Task 3, used by `start_capture`
  - `pick_output_device` / `build_output_config` defined in Task 4, used by `start_playback`
  - All `SendWrapper::new(stream)` wrapping happens at the `*slot = Some(...)` assignment site, consistent with the spec's "wrap on store, unwrap-via-Mutex-take on drop" pattern.

- **Real capture/playback validation deferred to manual smoke** on a native OS. Spec acknowledges this; tests don't pretend to cover it.

- **The `make_audio_backend_default_falls_back_to_mock_in_wsl` test in the spec is INTENTIONALLY OMITTED** from this plan. The factory's auto-detect branch can return either backend depending on the test environment — making the test brittle. Instead the env-var "mock" path is asserted; the real path is exercised in #3.3 manual smoke.
