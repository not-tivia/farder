# Real CpalAudioBackend — Design

**Status:** Drafted 2026-05-25
**Scope:** Farder client (`client/src-tauri`). New module `audio_cpal.rs` implementing the existing `AudioBackend` trait (from sub-project #1 `audio.rs`) against `cpal` device APIs. Updates the factory in `audio.rs` to prefer the real backend when devices exist. No protocol, server, or Cargo changes — `cpal = "0.15"` is already a client dep.
**Position in roadmap:** Sub-project #3.1 of the voice-and-screensharing track. The dependency-root of the voice feature. After this lands, sub-project #3.2 (Opus codec layer) and #3.3 (voice client pipeline) build on top.

## Goal

Provide a real `AudioBackend` implementation backed by `cpal` so voice (#3.3) can capture from microphones and play to speakers on Linux / macOS / Windows. The mock backend (sub-project #1) remains available for WSL development and explicit `FARDER_AUDIO_BACKEND=mock` use. The factory function auto-detects: if there are zero input devices on this host, fall back to mock with a one-time warning; otherwise use real.

## Non-Goals

- **Audio capture/playback validation in WSL.** WSL2 has no audio devices, so the real path can be COMPILED and the enumeration path TESTED (returns empty list, no crash) but actual capture/playback needs native hardware. Smoke testing happens on real OS.
- **Opus encoding / decoding.** Ships in #3.2 as a separate layer between `AudioBackend` and the network. The `AudioBackend` trait emits / consumes raw f32 PCM.
- **Resampling.** v1 requires the caller's `AudioFormat.sample_rate` to be natively supported by the device. If a USB headset only does 44.1 kHz and the caller asks for 48 kHz, we return `Err`. Resampling decorator is a future concern.
- **Stereo capture or stereo playback** for voice. v1 voice will use mono. The trait supports stereo via `AudioFormat.channels = 2`, but cpal stereo→mono downmix is the only conversion we ship; we do NOT mono→stereo upmix.
- **Per-device latency tuning / buffer-size customization.** cpal's default buffer size is used. Future work could expose this.
- **Hot-plug detection.** If a USB mic is plugged in mid-session, the user has to leave and rejoin the voice channel to pick it up. v1.
- **Echo cancellation / noise suppression / AGC.** Pure pass-through from cpal to the trait. v1.

## Architecture

```
┌─── Client (client/src-tauri) ──────────────────────────────────────────┐
│                                                                         │
│  audio.rs                                                              │
│    └─ make_audio_backend() ──┐                                         │
│         │                    │ no devices → fall back                  │
│         │                    └─→ MockAudioBackend (existing)           │
│         └─→ CpalAudioBackend (new)                                     │
│                                                                         │
│  audio_cpal.rs  (NEW)                                                  │
│    ├─ struct CpalAudioBackend                                          │
│    │     - capture_stream:  Mutex<Option<cpal::Stream>>                │
│    │     - playback_stream: Mutex<Option<cpal::Stream>>                │
│    │                                                                    │
│    ├─ enumerate_input_devices  → cpal::Host::input_devices             │
│    ├─ enumerate_output_devices → cpal::Host::output_devices            │
│    ├─ start_capture(device_id, format)                                 │
│    │     1. Open device by id (or default)                             │
│    │     2. Find supported config matching `format`                    │
│    │     3. Build cpal::Stream with on-data callback                   │
│    │     4. Callback: pack incoming samples into PcmChunk + send       │
│    │        via bounded mpsc::sync_channel(8)                          │
│    ├─ start_playback(device_id, format)                                │
│    │     Symmetric: caller pushes PcmChunk into sync_channel; cpal     │
│    │     callback pulls and writes to device                           │
│    └─ stop_*  → drop the Mutex's contained Stream (cpal auto-stops)    │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

### Why a new file (not extending audio.rs)

`audio.rs` is currently 280 lines including the AudioBackend trait, types, MockAudioBackend (~150 lines), factory, log_once. Adding CpalAudioBackend would push it past 600 lines with two distinct backend impls in one file. A separate `audio_cpal.rs` keeps responsibilities focused. The factory in `audio.rs` switches between them.

### cpal data-flow model

cpal's `Stream` API is callback-based:

```rust
let stream = device.build_input_stream(
    &config,
    move |data: &[f32], _info: &cpal::InputCallbackInfo| {
        // called by cpal on the real-time audio thread, ~every chunk_period_ms
        // do work here — DO NOT BLOCK
    },
    err_callback,
    None, // timeout
)?;
```

Bridging to our trait's `mpsc::Receiver<PcmChunk>`: the callback writes samples into a `PcmChunk` and pushes via `sync_sender.try_send(chunk)`. On a full channel, the callback DROPS the chunk (not the alternative of blocking — blocking the audio thread is catastrophic; the standard behavior is to drop and log).

For playback: caller pushes `PcmChunk`s into `mpsc::SyncSender`; the cpal output callback pulls via `receiver.try_recv()` and writes to `output: &mut [f32]`. Underrun (empty channel) means we write zeros for that frame — same dropped-frame model.

### Threading model

| Thread | What it does |
|---|---|
| cpal's audio thread (real-time, OS-owned) | Captures / plays samples. Pushes / pulls via mpsc. NEVER blocks. NEVER allocates. |
| Caller's async runtime task | Drains `Receiver<PcmChunk>` (capture) or pushes to `SyncSender<PcmChunk>` (playback). |
| `CpalAudioBackend::stop_*` caller | Locks the Mutex, takes the Stream out, drops it. cpal blocks until the audio thread exits cleanly (typically a few ms). |

The Mutex over `Option<cpal::Stream>` is the gate that lets a foreground caller cleanly tear down an active stream.

### WSL fallback

The factory probes cpal at construction time:

```rust
let host = cpal::default_host();
let has_input = host.input_devices().map(|i| i.count() > 0).unwrap_or(false);
if !has_input {
    log_once("audio.real_no_devices",
        "[audio] no input devices found; falling back to mock");
    return Box::new(MockAudioBackend::new());
}
```

If at least one input device exists, return `CpalAudioBackend::new()`. Honors the explicit `FARDER_AUDIO_BACKEND` env var: `"mock"` always picks mock; `"real"` always picks real (will fail at capture-time on WSL, with a clearer error than the silent fallback).

## CpalAudioBackend

### Construction + state

```rust
use send_wrapper::SendWrapper;

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
```

(Both `cpal::Host` and `cpal::Stream` are not `Send` on every platform — Windows specifically. `SendWrapper` provides `Send + Sync` by panic-on-cross-thread-access, which is the safe behavior given our usage pattern: same thread allocates and drops both objects.)

### Send + Sync

`cpal::Stream` is NOT `Send` or `Sync` on every platform (Windows specifically). `Mutex<Option<cpal::Stream>>` alone won't make `CpalAudioBackend` satisfy the `AudioBackend: Send + Sync` bound. Two workarounds:

1. Wrap the stream in `SendWrapper<cpal::Stream>` (from the `send_wrapper` crate) — pretends Stream is `Send`, panics if accessed off the original thread. Works if construction + drop happen on the same thread.
2. Spawn a dedicated owning thread per stream; use crossbeam channels for control + data.

v1 picks (1) — adds the `send_wrapper = "0.6"` dep (small, well-trusted). The Tauri command thread that calls `start_capture` is the same thread that calls `stop_capture`, so this is safe in practice. A `Drop` impl on the wrapper handles the panic on accidental cross-thread drop.

### enumerate_input_devices / enumerate_output_devices

```rust
fn enumerate_input_devices(&self) -> Result<Vec<AudioInputDevice>, String> {
    let devices = self.host.input_devices()
        .map_err(|e| format!("input_devices: {e}"))?;
    let default = self.host.default_input_device()
        .map(|d| d.name().unwrap_or_default());
    let mut out = Vec::new();
    for (i, dev) in devices.enumerate() {
        let name = dev.name().unwrap_or_else(|_| format!("device-{i}"));
        out.push(AudioInputDevice {
            id: name.clone(),
            name: name.clone(),
            is_default: default.as_deref() == Some(name.as_str()),
        });
    }
    Ok(out)
}
```

Output mirror.

Note: cpal uses the device's NAME as its stable identifier. There's no separate device-id concept. We use `name` for both `id` and `name` fields — works for current Farder needs (the UI picks a device, passes the name back to `start_capture`).

### start_capture

```rust
fn start_capture(
    &self,
    device_id: Option<&str>,
    format: AudioFormat,
) -> Result<mpsc::Receiver<PcmChunk>, String> {
    let mut slot = self.capture_stream.lock().map_err(|e| e.to_string())?;
    if slot.is_some() {
        return Err("capture already active".into());
    }

    let device = pick_input_device(&self.host, device_id)?;
    let cpal_config = build_input_config(&device, &format)?;
    let (tx, rx) = mpsc::sync_channel::<PcmChunk>(8);

    let frames_per_chunk = format.samples_per_chunk / format.channels as usize;
    let want_channels = format.channels as usize;
    let dev_channels = cpal_config.channels as usize;
    let started = Instant::now();
    let mut buffered_samples: Vec<f32> = Vec::with_capacity(format.samples_per_chunk);

    let stream = device.build_input_stream(
        &cpal_config,
        move |raw: &[f32], _info| {
            // Convert cpal's interleaved samples (possibly stereo) into
            // mono (or whatever was requested), accumulate to a chunk,
            // emit when full.
            let mut i = 0;
            while i < raw.len() {
                let sample = if dev_channels == want_channels {
                    raw[i]
                } else if dev_channels == 2 && want_channels == 1 {
                    // stereo → mono: average
                    (raw[i] + raw[i + 1]) / 2.0
                } else {
                    // unsupported channel mismatch — write zero, will
                    // be obvious in output
                    0.0
                };
                buffered_samples.push(sample);
                i += dev_channels;

                if buffered_samples.len() >= format.samples_per_chunk {
                    let chunk = PcmChunk {
                        samples: std::mem::take(&mut buffered_samples),
                        timestamp_ms: started.elapsed().as_millis() as u64,
                    };
                    buffered_samples.reserve(format.samples_per_chunk);
                    let _ = tx.try_send(chunk);
                    // Drop on full channel; do NOT block the audio thread.
                }
            }
        },
        |err| eprintln!("[audio] cpal capture error: {err}"),
        None,
    ).map_err(|e| format!("build_input_stream: {e}"))?;

    stream.play().map_err(|e| format!("stream.play: {e}"))?;
    *slot = Some(stream);
    Ok(rx)
}
```

`pick_input_device` looks up by name or returns the default. `build_input_config` finds a `SupportedStreamConfig` whose `sample_rate` matches `format.sample_rate` and whose channel count is `want_channels` or 2 (we'll downmix). Returns `Err` if no match.

### start_playback

Symmetric. Caller pushes chunks; output callback drains them. Empty queue → output zero samples (silence).

```rust
fn start_playback(
    &self,
    device_id: Option<&str>,
    format: AudioFormat,
) -> Result<mpsc::SyncSender<PcmChunk>, String> {
    let mut slot = self.playback_stream.lock().map_err(|e| e.to_string())?;
    if slot.is_some() {
        return Err("playback already active".into());
    }

    let device = pick_output_device(&self.host, device_id)?;
    let cpal_config = build_output_config(&device, &format)?;
    // Buffer sized for ~500ms (matches mock's behavior).
    let chunks_per_500ms = ((format.sample_rate as f32 * 0.5)
        / (format.samples_per_chunk / format.channels as usize).max(1) as f32)
        .ceil() as usize;
    let buf = chunks_per_500ms.max(2);
    let (tx, rx) = mpsc::sync_channel::<PcmChunk>(buf);
    let rx = std::sync::Mutex::new(rx);
    let mut pending: Vec<f32> = Vec::new();
    let want_channels = format.channels as usize;
    let dev_channels = cpal_config.channels as usize;

    let stream = device.build_output_stream(
        &cpal_config,
        move |out: &mut [f32], _info| {
            // Refill pending from the channel as needed.
            while pending.len() < out.len() {
                let rx_g = rx.lock().unwrap();
                match rx_g.try_recv() {
                    Ok(chunk) => {
                        // Adapt channel count: if device is stereo and chunk is mono,
                        // duplicate each sample.
                        if dev_channels == want_channels {
                            pending.extend_from_slice(&chunk.samples);
                        } else if want_channels == 1 && dev_channels == 2 {
                            for s in &chunk.samples {
                                pending.push(*s);
                                pending.push(*s);
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
            // Underrun → silence for the remainder
            for s in &mut out[take..] {
                *s = 0.0;
            }
        },
        |err| eprintln!("[audio] cpal playback error: {err}"),
        None,
    ).map_err(|e| format!("build_output_stream: {e}"))?;

    stream.play().map_err(|e| format!("stream.play: {e}"))?;
    *slot = Some(stream);
    Ok(tx)
}
```

### stop_capture / stop_playback

```rust
fn stop_capture(&self) -> Result<(), String> {
    let mut slot = self.capture_stream.lock().map_err(|e| e.to_string())?;
    slot.take(); // drop the Stream; cpal joins its audio thread
    Ok(())
}
```

Same shape for playback.

### backend_name

Returns `"cpal"`.

## Factory update in audio.rs

```rust
pub fn make_audio_backend() -> Box<dyn AudioBackend> {
    match std::env::var("FARDER_AUDIO_BACKEND").as_deref() {
        Ok("mock") => Box::new(MockAudioBackend::new()),
        Ok("real") => Box::new(CpalAudioBackend::new()),
        _ => {
            // Auto-detect. Probe for input devices; fall back to mock if none.
            let host = cpal::default_host();
            let has_input = host.input_devices()
                .map(|mut it| it.next().is_some())
                .unwrap_or(false);
            if has_input {
                Box::new(CpalAudioBackend::new())
            } else {
                log_once(
                    "audio.no_devices",
                    "[audio] no input devices found; falling back to mock",
                );
                Box::new(MockAudioBackend::new())
            }
        }
    }
}
```

`use crate::audio_cpal::CpalAudioBackend;` at the top of `audio.rs`.

## Cargo dependency additions

```toml
# client/src-tauri/Cargo.toml
send_wrapper = "0.6"
```

`cpal = "0.15"` is already there. `send_wrapper` is small (~200 lines), no transitive deps, well-trusted (used by web-sys ecosystem).

## Testing

### Unit tests (`#[cfg(test)] mod tests` in `audio_cpal.rs`)

- `cpal_backend_constructs_without_panicking` — `CpalAudioBackend::new()` returns Ok even with no devices. Sanity check.
- `cpal_backend_name` — `backend_name()` returns `"cpal"`.
- `cpal_enumerate_input_devices_returns_vec` — calls enumerate, asserts result is Ok (could be empty Vec on WSL, non-empty on a workstation; either is fine).
- `cpal_enumerate_output_devices_returns_vec` — same.
- `cpal_start_capture_with_no_devices_errors` — on systems with zero devices (or via host mocking, harder), the call should return Err. Skipped if the test environment has devices.

Capture/playback themselves can't be unit-tested in CI — no audio devices in the test runner. Manual smoke is required.

### Factory integration test (in `audio.rs::tests`)

- `make_audio_backend_mock_env_returns_mock` — set `FARDER_AUDIO_BACKEND=mock`, call factory, assert `backend_name() == "mock"`.
- `make_audio_backend_default_falls_back_to_mock_in_wsl` — UNSET env var, call factory, assert it returns SOMETHING (either mock or cpal depending on environment). Not assertion-heavy because of environmental variability.

### What's NOT tested in 3.1

- End-to-end capture or playback. Defers to #3.3 manual smoke on a real OS.
- Real device opening (no devices in CI).
- Format negotiation against unusual hardware.

## Migration / rollout

Pure addition. The factory's default arm changes from "mock-with-warn" to "real if devices, mock otherwise." For WSL users: behavior is identical to today (mock). For real-OS users: they suddenly get real audio whenever they invoke voice. But voice client (#3.3) doesn't exist yet, so no visible UX change until #3.3 lands.

## Future considerations

- **Resampling** when device doesn't natively support 48 kHz.
- **Hot-plug detection** via `cpal::Host`'s device-change events.
- **Per-device latency tuning** — expose `cpal::BufferSize::Fixed(n)` as a config knob.
- **Echo cancellation / noise suppression** — decorator backend that wraps `CpalAudioBackend`.
- **WSL with PulseAudio** — `cpal` can use ALSA on Linux; routing through WSL2's PulseAudio (via WSLg) might Just Work for some users. Worth documenting if it does.

---

This spec covers exactly the real-audio backend implementation for sub-project #3.1. Opus codec (#3.2) and voice client pipeline (#3.3) build on top of this and the existing `AudioBackend` trait.
