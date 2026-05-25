# MediaBackend Abstraction — Design

**Status:** Drafted 2026-05-23
**Scope:** Farder client (Tauri 2 / Rust). Two new modules: `audio.rs` and `display.rs`. No protocol or server changes.
**Position in roadmap:** Sub-project #1 of the audio-and-screensharing track. Foundational layer that unblocks parallel development of voice (Phase 3) and screensharing in WSL where real audio/display hardware is unavailable.

## Goal

Define hardware-facing Rust traits — `AudioBackend` for microphone/speaker, `DisplayBackend` for screen/window capture — and ship mock implementations that emit synthetic but pipeline-real data (sine wave audio, animated test-pattern video). A runtime env-var switch picks between mock and real. Downstream sub-projects (voice, screensharing) consume these traits without caring about the backend, and provide their own real implementations when ready.

## Non-Goals

- **Real implementations** — no `cpal`/`audiopus` audio capture, no `scrap`/`screencapturekit` display capture in this sub-project. The factory functions default to mock with a one-time warn log; real backends land with voice (Phase 3) and screensharing respectively.
- **Tauri commands / IPC** — no `#[tauri::command]` here. The traits are pure Rust consumed by other Rust modules (voice, screensharing). IPC commands ship with those features.
- **Codec or transport layer** — the trait emits raw PCM / RGBA pixels. Opus encoding, VP8/AV1 encoding, packetization, network transport are all downstream.
- **Cross-backend mixing** — one `AudioBackend` instance owns capture and playback. We do not mix multiple backends in one process.
- **Audio effects / DSP** — no noise suppression, AGC, echo cancellation here. The trait is a thin device adapter; effects live above it.
- **Recording-to-file** — `hound` and `cpal` already exist in `Cargo.toml` for the existing recording features (`save_temp_audio`, `start_recording`); those stay as-is and are not refactored to use this new abstraction. This is forward-looking infrastructure for *real-time* media.

## Architecture

### File layout

Two new sibling modules in `client/src-tauri/src/`:

```
client/src-tauri/src/
├── audio.rs        — AudioBackend trait + MockAudioBackend + make_audio_backend()
├── display.rs      — DisplayBackend trait + MockDisplayBackend + make_display_backend()
└── main.rs         — `mod audio; mod display;` (no Tauri command wiring yet)
```

Both modules sit alongside existing client-only modules (`tenor.rs`, `book.rs`, `translation.rs`, `themes.rs`). No new workspace crate — the other crates (`farder-protocol`, `farder-server`, etc.) exist because they're shared between client and server; this is client-only.

### Why two separate traits

Audio and display have genuinely different concepts:
- Audio: devices (input + output), sample rate, channel count, sample chunk size.
- Display: sources (screens + windows), frame rate, resolution, raw pixel format.

A unified `MediaBackend<T>` trait would leak concepts (audio has no "resolution"; displays have no "sample rate") and force generic gymnastics on consumers. Downstream code that wants both calls both factories.

### Why no real impls here

Sub-project #1 unlocks parallel development. Mixing the real `cpal`/`audiopus` wiring into this spec would (a) double the surface area, (b) couple sub-project #1 to voice-specific Opus framing, (c) make the spec harder to keep tight. Voice (Phase 3) will own its real audio backend; screensharing will own its real display backend.

## AudioBackend trait

```rust
// client/src-tauri/src/audio.rs

use std::sync::mpsc;

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
    pub sample_rate: u32,        // e.g. 48000
    pub channels: u16,           // 1 = mono, 2 = stereo
    pub samples_per_chunk: usize, // total f32 samples per chunk
                                 //   = (sample_rate * channels * chunk_ms) / 1000
                                 //   e.g. 48000 * 1 * 20 / 1000 = 960 for 20ms mono
}

/// A chunk of f32 PCM samples in [-1.0, 1.0], interleaved across channels.
pub struct PcmChunk {
    pub samples: Vec<f32>,
    pub timestamp_ms: u64, // monotonic since backend start
}

pub trait AudioBackend: Send + Sync {
    fn enumerate_input_devices(&self) -> Result<Vec<AudioInputDevice>, String>;
    fn enumerate_output_devices(&self) -> Result<Vec<AudioOutputDevice>, String>;

    /// Start capture from `device_id` (or backend default if None).
    /// Returns a receiver yielding chunks at `format`'s natural cadence.
    /// Subsequent calls to start_capture without an intervening stop_capture
    /// return Err.
    fn start_capture(
        &self,
        device_id: Option<&str>,
        format: AudioFormat,
    ) -> Result<mpsc::Receiver<PcmChunk>, String>;

    /// Stop the active capture. Returns Ok even if no capture is active.
    fn stop_capture(&self) -> Result<(), String>;

    /// Start playback to `device_id` (or backend default if None).
    /// Returns a SyncSender — push chunks to it as you'd like them played.
    /// Backpressure: SyncSender buffer is sized to hold ~500ms of audio.
    fn start_playback(
        &self,
        device_id: Option<&str>,
        format: AudioFormat,
    ) -> Result<mpsc::SyncSender<PcmChunk>, String>;

    /// Stop the active playback. Returns Ok even if no playback is active.
    fn stop_playback(&self) -> Result<(), String>;

    /// Identifier for logging / UI ("mock", "cpal", future others).
    fn backend_name(&self) -> &'static str;
}
```

### Concurrency model

`Send + Sync` is required because backend instances live behind `Arc` and capture/playback threads call methods concurrently with the consumer. Implementations use interior mutability (`Mutex<Option<JoinHandle>>` etc.) to track active capture/playback state.

### Format negotiation

The consumer specifies the format. The backend either honours it exactly or returns Err. No "best effort" resampling here — that's a downstream concern (e.g., voice will probably standardize on 48 kHz mono and only ever request that). This keeps the trait predictable.

## DisplayBackend trait

```rust
// client/src-tauri/src/display.rs

use std::sync::mpsc;

#[derive(Debug, Clone, Copy)]
pub enum DisplaySourceKind {
    Screen, // a whole monitor
    Window, // an application window
}

#[derive(Debug, Clone)]
pub struct DisplaySource {
    pub id: String,
    pub kind: DisplaySourceKind,
    pub label: String, // human-readable: "Display 1 (2560×1440)", "Firefox - Farder", etc.
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct DisplayFormat {
    pub fps: u32,        // target frame rate; backend may produce fewer
    pub max_width: u32,  // backend downscales if source larger
    pub max_height: u32,
}

/// A captured frame in RGBA8888, row-major, packed (stride = width * 4).
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub stride: usize, // bytes per row; typically width * 4
    pub pixels: Vec<u8>,
    pub timestamp_ms: u64, // monotonic since backend start
}

pub trait DisplayBackend: Send + Sync {
    fn enumerate_sources(&self) -> Result<Vec<DisplaySource>, String>;

    /// Start capture of `source_id`. Returns a receiver yielding frames at
    /// roughly `format.fps`. Subsequent calls without stop_capture return Err.
    fn start_capture(
        &self,
        source_id: &str,
        format: DisplayFormat,
    ) -> Result<mpsc::Receiver<VideoFrame>, String>;

    /// Stop the active capture. Returns Ok even if no capture is active.
    fn stop_capture(&self) -> Result<(), String>;

    /// Identifier for logging / UI ("mock", "scrap", future others).
    fn backend_name(&self) -> &'static str;
}
```

### Frame pixel format

RGBA8888 is the WebView's natural format and works directly with `<canvas>` `putImageData()` or WebGL textures. Backends that capture in BGRA (Windows) or YUV (some Linux paths) convert before emitting. Conversion cost is acceptable — we're not optimizing for the absolute minimum copy here.

## Mock implementations

### `MockAudioBackend`

```rust
pub struct MockAudioBackend {
    capture: Mutex<Option<JoinHandle<()>>>,
    playback: Mutex<Option<JoinHandle<()>>>,
    capture_stop: Mutex<Option<Arc<AtomicBool>>>,
    playback_stop: Mutex<Option<Arc<AtomicBool>>>,
}
```

- `enumerate_input_devices` returns one entry: `{ id: "mock-input", name: "Mock Input (sine wave)", is_default: true }`.
- `enumerate_output_devices` returns one entry: `{ id: "mock-output", name: "Mock Output (discard)", is_default: true }`.
- `start_capture`:
  - Reads `FARDER_MOCK_AUDIO_HZ` env var (default `440`); clamps to `[20, 20_000]`.
  - Spawns a thread holding an `Arc<AtomicBool>` stop-flag.
  - Generates one chunk per `(samples_per_chunk / channels) / sample_rate` seconds, fills with `sin(2π · hz · t)` clamped to `[-0.7, 0.7]` (avoid clipping headroom).
  - Sends via `mpsc::sync_channel` (bounded at 8 chunks) — backpressure naturally rate-limits if consumer is slow.
  - Stop flag checked between chunks; thread exits cleanly within one chunk period.
- `start_playback`:
  - Spawns a consumer thread holding the stop flag.
  - Drains the SyncSender and discards (future: track total samples / chunk count for stats).
- `stop_capture` / `stop_playback`: set the stop flag, join the thread with a 200 ms timeout. If join times out, log a warning and detach — backend stays usable for next start.
- `backend_name`: `"mock"`.

### `MockDisplayBackend`

```rust
pub struct MockDisplayBackend {
    capture: Mutex<Option<JoinHandle<()>>>,
    capture_stop: Mutex<Option<Arc<AtomicBool>>>,
}
```

- `enumerate_sources` returns one entry:
  `{ id: "mock-display", kind: Screen, label: "Mock Display 1280×720", width: 1280, height: 720 }`.
- `start_capture`:
  - Spawns a thread; period = `1000 / format.fps` ms (default behavior: respect requested fps up to 60).
  - Each frame:
    - Resolution: `min(1280, format.max_width) × min(720, format.max_height)`.
    - Content: HSV rotating gradient (hue = `frame_count * 2 % 360`), with a small frame counter rendered in the top-left as black-on-white pixels via a simple 5×7 bitmap font (digits only — no full font system).
  - Pixel buffer is allocated once and reused (mutated in place) per frame to avoid heap churn.
- `stop_capture`: same stop-flag pattern as audio.
- `backend_name`: `"mock"`.

### Bitmap font

A 5×7 monospace pixel font covering `0`–`9`, included as a `[[u8; 5]; 7]` constant. Each row is a byte where bit positions encode pixels left-to-right. Rendering the counter is `for digit in counter.to_string().chars() { draw_digit(digit, x, y); x += 6; }`. Total code: ~40 lines.

## Backend selection

Both modules expose a single factory:

```rust
// audio.rs
pub fn make_audio_backend() -> Box<dyn AudioBackend> {
    match std::env::var("FARDER_AUDIO_BACKEND").as_deref() {
        Ok("mock") => Box::new(MockAudioBackend::new()),
        _ => {
            // Real backend ships with the voice sub-project. Until then, fall
            // back to mock so WSL development isn't blocked. Logged once.
            log_once("audio.real_not_shipped",
                "[audio] real backend not yet shipped; using mock");
            Box::new(MockAudioBackend::new())
        }
    }
}

// display.rs — same shape with FARDER_DISPLAY_BACKEND
```

`log_once` is a small helper that uses a `Mutex<HashSet<&'static str>>` to ensure each warning fires at most once per process. Lives in `audio.rs` initially; if `display.rs` needs the same pattern (it will), promote to a shared utility module — but only when the second user lands.

When voice (Phase 3) ships, it changes the `_ => mock` arm to `_ => Box::new(RealAudioBackend::new())` (and similarly for screensharing). The env var stays as the explicit dev override.

### Why env var, not feature flag

Env var means one binary, runtime toggle. WSL developers `export FARDER_AUDIO_BACKEND=mock` in their shell init; everyone else gets real automatically once real impls land. Feature flag would require separate builds — bad for the dev/prod cycle, and explicit selection is preferred over compile-time magic for hardware concerns.

### Why default `_` to mock (not error)

If a non-WSL user runs without setting the env var BEFORE the real backend ships, defaulting to "real" would crash on hardware init. Defaulting to "mock" + warn log keeps the app usable. Once real ships, the warn log goes away and the default becomes real.

## Testing

### Unit tests (Rust, in each module)

**`audio.rs`** (`#[cfg(test)] mod tests`):
- `mock_enumerate_returns_one_input_one_output` — exact equality of id/name.
- `mock_capture_emits_chunks_at_expected_cadence` — start capture at 48 kHz mono 960-sample chunks, collect 5 chunks, assert elapsed time is within `[80 ms, 200 ms]` (5 chunks × 20 ms with slack).
- `mock_capture_samples_are_nonzero` — collect a chunk, assert at least 50% of samples are above 0.01 absolute value (sine isn't silent).
- `mock_stop_capture_terminates_within_200ms` — start, stop, assert thread joined in `< 200 ms`.
- `mock_double_start_capture_returns_err` — start, start-again, expect Err.
- `mock_env_var_overrides_frequency` — set `FARDER_MOCK_AUDIO_HZ=880`, start capture at 48 kHz mono, collect 1 second of samples, count zero crossings, divide by 2 to get measured Hz, assert within ±10% of 880. Uses zero crossings (not FFT) to avoid adding an FFT dep.

**`display.rs`** (`#[cfg(test)] mod tests`):
- `mock_enumerate_returns_one_source` — exact equality.
- `mock_capture_emits_frames_at_expected_fps` — start at 30 fps, collect 5 frames, assert elapsed time `[100 ms, 250 ms]`.
- `mock_capture_frames_are_correct_size` — start with `max_width=640, max_height=360`, assert returned frame is `640 × 360`.
- `mock_capture_frames_have_visible_gradient` — collect a frame, assert pixel variance across rows is above some floor (gradient isn't black).
- `mock_stop_capture_terminates_within_200ms` — same shape as audio.
- `mock_double_start_capture_returns_err` — same shape as audio.

### Smoke

No end-to-end smoke in this sub-project. The factories return mock backends; nothing wires them into UI yet. Voice/screensharing sub-projects exercise the integration.

### Manual verification

After landing:
```
cd ~/farder/client/src-tauri && cargo test audio::tests display::tests
```

Expect: 12 passed.

## File inventory

**Created:**
- `client/src-tauri/src/audio.rs` (~250 lines including trait, mock impl, factory, tests, 5×7 font is in display.rs).
- `client/src-tauri/src/display.rs` (~300 lines including font constant).

**Modified:**
- `client/src-tauri/src/main.rs` — add `mod audio; mod display;` declarations alongside existing `mod tenor;` etc.

**No changes:**
- `client/src-tauri/Cargo.toml` — the mock impls use only stdlib (`std::sync::mpsc`, `std::thread`, `std::sync::atomic`). No new deps.
- Existing `cpal` and `hound` deps stay in `Cargo.toml` for the existing recording features; not consumed by this sub-project.
- No Tauri commands added.
- No protocol changes.
- No TS changes.

## Rollout

This sub-project ships infrastructure with no user-visible behavior change. Nothing in the UI references the new modules; nothing in the protocol changes. After landing:
- Voice (Phase 3) imports `crate::audio::{make_audio_backend, AudioBackend, AudioFormat, PcmChunk}` and consumes the trait.
- Screensharing imports `crate::display::{...}` similarly.
- Real implementations replace the `_ => mock` factory arm when their sub-projects land. No coordination needed beyond merge order.

## Future considerations

- **Resampling layer above the trait** — if voice settles on 48 kHz but the user's device only supports 44.1 kHz, the real backend can opt to resample internally, OR a separate `ResamplingAudioBackend` decorator can wrap any backend.
- **Echo cancellation / noise suppression** — same pattern: decorator backends that wrap a base backend and process chunks in flight.
- **More than one mock pattern** — if we ever want voice tests to differentiate "speaker A" from "speaker B", we can add `MockAudioBackend::with_pattern(Pattern::Sine440)` / `Pattern::Sweep20to20k` / etc. Out of scope for v1.
- **Browser-side capture for screensharing** — the WebView's `getDisplayMedia()` could be a third `BrowserDisplayBackend` that uses an IPC bridge to push frames into Rust. Worth considering when the real display backend lands.

---

This spec covers exactly the trait surface, mock behavior, selection logic, and tests for sub-project #1. Real backends, codec layer, transport layer, and UI all ship in subsequent sub-projects with their own specs.
