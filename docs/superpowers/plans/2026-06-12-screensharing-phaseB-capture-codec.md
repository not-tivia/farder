# Screensharing Phase B — Capture + Codec (loopback)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Capture the screen, H.264-encode it in Rust, decode it in the webview with WebCodecs, and paint it to a canvas — all on one machine, NO networking. Proves every codec/capture/decode layer works before Phase C wires it over the network.

**Architecture:** A real Windows Graphics Capture backend implements the existing dormant `DisplayBackend` seam (`client/src-tauri/src/display.rs`), delivering RGBA frames; the mock backend stays for headless/Linux tests. An `H264Encoder` (the `openh264` crate, validated building on this machine) turns each `VideoFrame` into Annex-B H.264. A Tauri command runs a capture→encode loop and emits each encoded frame (base64) as a `screenshare:frame` event. A frontend component feeds those frames to a WebCodecs `VideoDecoder` and draws to a `<canvas>`. The whole Rust path (mock-capture → encode → openh264-decode round-trip) is headless-testable; the Windows-only capture and the webview decode are owner-verified at runtime.

**Tech Stack:** Rust (`openh264` 0.9.3, `windows-capture` 2.0.0), Tauri events, React/TypeScript + WebCodecs.

**Spec:** `docs/superpowers/specs/2026-06-12-screensharing-design.md` (Phase B).

**Branch:** create `screenshare-phaseB` from `main` before Task 1. Finish with ff-merge + push.

**Dependency validation (DONE 2026-06-12, on the owner's Windows machine):** `openh264 0.9.3` builds (bundled C/C++ via `cc`, **nasm NOT required**) and the encoder constructs; `windows-capture 2.0.0` builds and enumerates monitors; WebCodecs `VideoDecoder` in WebView2 decodes `avc1.42E01E` (Constrained Baseline — what OpenH264 emits) and even `avc1.640028` (the phase-2 1080p target). No native-build unknowns remain.

---

## Verified API facts (researched 2026-06-12 — these are exact)

- **`openh264` 0.9.3 encode path:**
  - `openh264::formats::RgbaSliceU8::new(data: &[u8], dimensions: (usize, usize))` wraps packed `[R G B A …]` bytes (matches `VideoFrame`'s RGBA8888 contract).
  - `openh264::formats::YUVBuffer::from_rgb_source(src)` converts any RGB source (incl. `RgbaSliceU8`) to I420; the result implements `YUVSource`.
  - `openh264::encoder::Encoder::new() -> Result<Encoder, Error>` (default config; `source` feature) **OR** `Encoder::with_api_config(OpenH264API::from_source(), config) -> Result<Encoder, Error>`.
  - `Encoder::encode<T: YUVSource>(&mut self, &T) -> Result<EncodedBitStream, Error>`; `Encoder::force_intra_frame(&mut self)` forces the next frame to be an IDR keyframe.
  - `EncodedBitStream::to_vec(&self) -> Vec<u8>` (Annex-B NAL bytes); `EncodedBitStream::frame_type(&self) -> openh264::encoder::FrameType`.
  - `FrameType` variants: `Invalid, IDR, I, P, Skip, IPMixed` — `IDR` (and `I`) are keyframes.
  - `openh264::encoder::{EncoderConfig, BitRate, FrameRate, IntraFramePeriod}` builder: `EncoderConfig::new().bitrate(BitRate::from_bps(3_000_000)).max_frame_rate(FrameRate::new(30.0)).intra_frame_period(IntraFramePeriod::new(60))` (confirm `FrameRate::new` takes f32 vs u32 at impl time — the compiler will say).
  - `openh264::decoder::{Decoder, nal_units}` exist (used in tests to prove the encoder emits valid H.264); `Decoder::new() -> Result<Decoder, Error>`, `decoder.decode(nal) -> Result<Option<DecodedYUV>, Error>`.
- **`windows-capture` 2.0.0:**
  - Trait `GraphicsCaptureApiHandler` with `type Flags; type Error;`, `fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error>`, `fn on_frame_arrived(&mut self, frame: &mut Frame, ctl: InternalCaptureControl) -> Result<(), Self::Error>`, `fn on_closed(&mut self) -> Result<(), Self::Error>`.
  - `Settings::new(item, CursorCaptureSettings::Default, DrawBorderSettings::Default, SecondaryWindowSettings::Default, MinimumUpdateIntervalSettings::Default, DirtyRegionSettings::Default, ColorFormat::Rgba8, flags)` — **`ColorFormat::Rgba8` makes WGC deliver RGBA directly**, matching `VideoFrame`.
  - `monitor::Monitor::{primary(), enumerate(), from_index()}` (all return `Result`).
  - Start: `Handler::start(settings)` blocks the calling thread (run it on a spawned thread); `Handler::start_free_threaded(settings)` returns a control handle.
  - **CONFIRM AT IMPL TIME (Windows, the one unpinned call):** the frame-buffer accessor — inside `on_frame_arrived`, `frame.width()`, `frame.height()`, and `frame.buffer()` then a packed-rows accessor. The candidates are `buffer.as_raw_buffer()` (may be row-padded: stride ≥ width*4) and `buffer.as_raw_nopadding_buffer()` (tightly packed, stride = width*4). **We want packed RGBA** (stride = width*4) to match `RgbaSliceU8`. Use the nopadding accessor; if padded is all that's available, copy row-by-row stripping the pad. The implementer is on Windows and the compiler + docs.rs/windows-capture/2.0.0 settle the exact name.
- **The existing seam (`client/src-tauri/src/display.rs`):** `pub trait DisplayBackend { enumerate_sources, start_capture(source_id, DisplayFormat) -> Result<mpsc::Receiver<VideoFrame>, String>, stop_capture, backend_name }`; `pub struct VideoFrame { width, height, stride: usize, pixels: Vec<u8> (RGBA8888 packed), timestamp_ms }`; `DisplayFormat { fps, max_width, max_height }`; `MockDisplayBackend` (gradient + frame-counter, used by tests); `make_display_backend()` currently returns the mock for the non-"mock" arm too (the "real backend not yet shipped" fallback) — Phase B fills that arm on Windows. `make_display_backend` is **not wired into any command yet** — Phase B is its first consumer.
- **Client crate:** `client/src-tauri/Cargo.toml` has no `[target.'cfg(windows)']` section yet; deps are added per Task 1. Tauri v2 `AppHandle::emit("event", payload)` is the event mechanism (see `bridge.rs`); the frontend listens via `listen(...)` (see `useServerEvents.ts`). Commands are registered in `generate_handler![...]` in `main.rs`.
- **WebCodecs note:** OpenH264 emits **Annex-B** byte-stream (start codes `00 00 00 01`), with SPS/PPS inline before each IDR. Chromium's `VideoDecoder` accepts Annex-B when `configure()` is called WITHOUT a `description` field. The first decoded chunk MUST be `type: 'key'` (guaranteed by forcing an IDR as frame 0). Codec string `avc1.42E01E` (owner-validated).

---

### Task 1: Add the native dependencies

**Files:**
- Modify: `client/src-tauri/Cargo.toml`

- [ ] **Step 1: Add the deps.** In `client/src-tauri/Cargo.toml`, add to `[dependencies]`:

```toml
# H.264 software encoder/decoder for screensharing. Default `source` feature
# builds the bundled OpenH264 C/C++ via cc (nasm optional). Builds on all
# platforms (validated on Linux + Windows 2026-06-12).
openh264 = "0.9.3"
```

And add a new target section (after `[dependencies]`, before `[dev-dependencies]`):

```toml
# Windows Graphics Capture — screen/window/fullscreen capture. Windows-only;
# on other hosts the mock display backend is used and this isn't compiled.
[target.'cfg(windows)'.dependencies]
windows-capture = "2.0.0"
```

- [ ] **Step 2: Verify it resolves + builds.**

Run: `cd client/src-tauri && cargo build 2>&1 | tail -3`
Expected: clean build (on Linux, openh264 compiles its bundled source; windows-capture is skipped). First build is slow (OpenH264 C compile).

- [ ] **Step 3: Commit.**

```bash
git add client/src-tauri/Cargo.toml client/src-tauri/Cargo.lock
git commit -m "client: add openh264 + windows-capture (screenshare Phase B deps)"
```

---

### Task 2: H.264 encoder wrapper

**Files:**
- Create: `client/src-tauri/src/video_encoder.rs`
- Modify: `client/src-tauri/src/main.rs` (add `mod video_encoder;`)

- [ ] **Step 1: Create the module with tests.** Create `client/src-tauri/src/video_encoder.rs`:

```rust
//! H.264 software encoder wrapping the `openh264` crate. Turns RGBA
//! `VideoFrame`s (from the DisplayBackend) into Annex-B H.264 frames for the
//! screenshare pipeline. Decode happens in the webview (WebCodecs); the
//! openh264 decoder is used here only in tests to prove the output is valid.

use crate::display::VideoFrame;
use openh264::encoder::{Encoder, FrameType};
use openh264::formats::{RgbaSliceU8, YUVBuffer};

/// One encoded H.264 frame.
pub struct EncodedFrame {
    /// Annex-B NAL byte stream (start codes; SPS/PPS inline before each IDR).
    pub data: Vec<u8>,
    /// True for an IDR/I keyframe — the frontend tags the WebCodecs chunk
    /// `'key'` vs `'delta'` from this.
    pub is_keyframe: bool,
    pub timestamp_ms: u64,
}

pub struct H264Encoder {
    enc: Encoder,
}

impl H264Encoder {
    pub fn new() -> Result<Self, String> {
        let enc = Encoder::new().map_err(|e| format!("openh264 encoder init: {e}"))?;
        Ok(Self { enc })
    }

    /// Force the NEXT encoded frame to be an IDR keyframe (call once before the
    /// first frame so a fresh decoder can start; later phases call it again
    /// when a new viewer attaches).
    pub fn force_keyframe(&mut self) {
        self.enc.force_intra_frame();
    }

    /// Encode one RGBA frame. The encoder derives width/height from the frame.
    pub fn encode(&mut self, frame: &VideoFrame) -> Result<EncodedFrame, String> {
        let w = frame.width as usize;
        let h = frame.height as usize;
        // RgbaSliceU8 expects packed rows (stride == width*4). VideoFrame's
        // contract is packed RGBA8888; assert it so a padded frame fails loudly
        // rather than producing garbage.
        if frame.stride != w * 4 || frame.pixels.len() < h * w * 4 {
            return Err(format!(
                "frame not packed RGBA: stride={} expected={} pixels={} expected>={}",
                frame.stride, w * 4, frame.pixels.len(), h * w * 4
            ));
        }
        let src = RgbaSliceU8::new(&frame.pixels[..h * w * 4], (w, h));
        let yuv = YUVBuffer::from_rgb_source(src);
        let bitstream = self.enc.encode(&yuv).map_err(|e| format!("openh264 encode: {e}"))?;
        let is_keyframe = matches!(bitstream.frame_type(), FrameType::IDR | FrameType::I);
        Ok(EncodedFrame {
            data: bitstream.to_vec(),
            is_keyframe,
            timestamp_ms: frame.timestamp_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A packed RGBA test frame with a simple gradient (gives the encoder real
    /// content, not a flat color that could degenerate).
    fn gradient_frame(w: u32, h: u32, t: u64) -> VideoFrame {
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                pixels[i] = (x.wrapping_add(t as u32) % 256) as u8;
                pixels[i + 1] = (y % 256) as u8;
                pixels[i + 2] = ((x + y) % 256) as u8;
                pixels[i + 3] = 255;
            }
        }
        VideoFrame { width: w, height: h, stride: (w * 4) as usize, pixels, timestamp_ms: t }
    }

    #[test]
    fn first_forced_frame_is_keyframe_and_nonempty() {
        let mut enc = H264Encoder::new().unwrap();
        enc.force_keyframe();
        let out = enc.encode(&gradient_frame(320, 240, 0)).unwrap();
        assert!(out.is_keyframe, "first forced frame must be a keyframe");
        assert!(!out.data.is_empty(), "encoded data must be non-empty");
        // Annex-B start code at the front.
        assert_eq!(&out.data[..4], &[0x00, 0x00, 0x00, 0x01], "expected Annex-B start code");
    }

    #[test]
    fn rejects_unpacked_frame() {
        let mut enc = H264Encoder::new().unwrap();
        let mut f = gradient_frame(16, 16, 0);
        f.stride = 999; // not width*4
        assert!(enc.encode(&f).is_err());
    }

    #[test]
    fn encoder_output_decodes_back_with_openh264() {
        // Proves the encoder emits valid H.264 a real decoder can consume.
        use openh264::decoder::Decoder;
        use openh264::nal_units;

        let mut enc = H264Encoder::new().unwrap();
        let mut dec = Decoder::new().unwrap();
        enc.force_keyframe();

        let mut decoded_any = false;
        for t in 0..5u64 {
            let out = enc.encode(&gradient_frame(320, 240, t)).unwrap();
            for nal in nal_units(&out.data) {
                if let Ok(Some(yuv)) = dec.decode(nal) {
                    let (dw, dh) = yuv.dimensions();
                    assert_eq!((dw, dh), (320, 240), "decoded dims must match");
                    decoded_any = true;
                }
            }
        }
        assert!(decoded_any, "openh264 decoder must decode at least one frame from the encoder output");
    }
}
```

(`DecodedYUV::dimensions()` returns `(usize, usize)` in 0.9.x; if the method name differs the test compiler will say — adjust to the 0.9.3 accessor. The round-trip assertion is the point, not the exact accessor.)

- [ ] **Step 2: Register the module.** In `client/src-tauri/src/main.rs`, add `mod video_encoder;` with the other `mod` lines.

- [ ] **Step 3: Run the tests.**

Run: `cd client/src-tauri && cargo test video_encoder::`
Expected: 3 tests PASS (encode produces a keyframe Annex-B frame; unpacked rejected; output round-trips through the openh264 decoder).

- [ ] **Step 4: Commit.**

```bash
git add client/src-tauri/src/video_encoder.rs client/src-tauri/src/main.rs
git commit -m "client: H.264 encoder wrapper (openh264) with decode round-trip test"
```

---

### Task 3: Encoder bitrate/framerate config

**Files:**
- Modify: `client/src-tauri/src/video_encoder.rs`

- [ ] **Step 1: Add a config-aware constructor.** Replace `H264Encoder::new` with a configured version targeting ~3 Mbps / 30 fps / 2 s keyframe period (720p30 game-streaming starting point per the spec). In `video_encoder.rs`:

Change the imports line to:

```rust
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod};
use openh264::OpenH264API;
```

Replace `new`:

```rust
    pub fn new() -> Result<Self, String> {
        // 3 Mbps, 30 fps, keyframe every ~60 frames (~2 s). These are starting
        // values; Phase C/quality tuning revisits them (and NVENC in phase 2).
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(3_000_000))
            .max_frame_rate(FrameRate::new(30.0))
            .intra_frame_period(IntraFramePeriod::new(60));
        let enc = Encoder::with_api_config(OpenH264API::from_source(), config)
            .map_err(|e| format!("openh264 encoder init: {e}"))?;
        Ok(Self { enc })
    }
```

**Fallback (only if `OpenH264API::from_source()` or a builder name doesn't resolve):** keep `Encoder::new()` (default config) — the loopback works either way; bitrate is a tuning detail, not a correctness requirement. Note the fallback in the commit message if used. (`FrameRate::new` may take `u32` rather than `f32` — match the compiler.)

- [ ] **Step 2: Run the tests.**

Run: `cd client/src-tauri && cargo test video_encoder::`
Expected: the same 3 tests still PASS (config doesn't change the contract — keyframe-first, valid H.264).

- [ ] **Step 3: Commit.**

```bash
git add client/src-tauri/src/video_encoder.rs
git commit -m "client: configure H.264 encoder to 720p30 ~3Mbps target"
```

---

### Task 4: Real Windows Graphics Capture backend

**Files:**
- Create: `client/src-tauri/src/display_wgc.rs`
- Modify: `client/src-tauri/src/display.rs` (`make_display_backend` returns the WGC backend on Windows)
- Modify: `client/src-tauri/src/main.rs` (`mod display_wgc;` gated to windows)

This task is **Windows-only and owner-verified at runtime** — it cannot run on this Linux host. The headless gate is: the mock backend + its tests stay green, the Linux build is unaffected (cfg-gated), and the owner confirms `cargo build` on Windows compiles it.

- [ ] **Step 1: Create the WGC backend.** Create `client/src-tauri/src/display_wgc.rs` (the whole file is `#![cfg(windows)]`):

```rust
//! Real screen capture via Windows Graphics Capture (`windows-capture`).
//! Implements the DisplayBackend seam; delivers packed RGBA VideoFrames.
//! Windows-only — the mock backend covers every other host (incl. Linux CI).
#![cfg(windows)]

use crate::display::{DisplayBackend, DisplayFormat, DisplaySource, DisplaySourceKind, VideoFrame};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

/// Flags handed to the capture handler: the frame sink + a stop flag + the
/// capture start instant (for monotonic timestamps).
struct CaptureFlags {
    sink: SyncSender<VideoFrame>,
    stop: Arc<AtomicBool>,
    started: Instant,
}

struct FrameHandler {
    sink: SyncSender<VideoFrame>,
    stop: Arc<AtomicBool>,
    started: Instant,
}

impl GraphicsCaptureApiHandler for FrameHandler {
    type Flags = CaptureFlags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { sink: ctx.flags.sink, stop: ctx.flags.stop, started: ctx.flags.started })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if self.stop.load(Ordering::Relaxed) {
            capture_control.stop();
            return Ok(());
        }
        let width = frame.width();
        let height = frame.height();
        // CONFIRM the exact buffer accessor against windows-capture 2.0.0 docs:
        // we need PACKED RGBA (stride == width*4). The nopadding accessor gives
        // that; if only a padded buffer is available, copy row-by-row dropping
        // the pad. `as_raw_nopadding_buffer()` is the expected name.
        let mut buffer = frame.buffer()?;
        let raw: &[u8] = buffer.as_raw_nopadding_buffer()?;
        let packed_len = (width * height * 4) as usize;
        if raw.len() < packed_len {
            return Ok(()); // short buffer — skip this frame defensively
        }
        let vf = VideoFrame {
            width,
            height,
            stride: (width * 4) as usize,
            pixels: raw[..packed_len].to_vec(),
            timestamp_ms: self.started.elapsed().as_millis() as u64,
        };
        // Non-blocking: if the encoder is behind, drop the frame (live policy).
        let _ = self.sink.try_send(vf);
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub struct WgcDisplayBackend {
    stop: Mutex<Option<Arc<AtomicBool>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl WgcDisplayBackend {
    pub fn new() -> Self {
        Self { stop: Mutex::new(None), thread: Mutex::new(None) }
    }
}

impl Default for WgcDisplayBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayBackend for WgcDisplayBackend {
    fn enumerate_sources(&self) -> Result<Vec<DisplaySource>, String> {
        let monitors = Monitor::enumerate().map_err(|e| format!("enumerate monitors: {e}"))?;
        let mut out = Vec::new();
        for (i, m) in monitors.into_iter().enumerate() {
            // name()/width()/height() are best-effort; fall back to an index label.
            let label = m
                .name()
                .map(|n| format!("Display {}: {}", i + 1, n))
                .unwrap_or_else(|_| format!("Display {}", i + 1));
            let width = m.width().unwrap_or(0);
            let height = m.height().unwrap_or(0);
            out.push(DisplaySource {
                id: format!("monitor:{}", i + 1), // 1-based index → Monitor::from_index
                kind: DisplaySourceKind::Screen,
                label,
                width,
                height,
            });
        }
        Ok(out)
    }

    fn start_capture(
        &self,
        source_id: &str,
        format: DisplayFormat,
    ) -> Result<mpsc::Receiver<VideoFrame>, String> {
        if format.fps == 0 {
            return Err("invalid DisplayFormat: fps=0".into());
        }
        let mut thread_slot = self.thread.lock().map_err(|e| e.to_string())?;
        if thread_slot.is_some() {
            return Err("capture already active".into());
        }
        // Parse "monitor:N" → 1-based index.
        let idx: u32 = source_id
            .strip_prefix("monitor:")
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("bad source_id: {source_id}"))?;
        let monitor = Monitor::from_index(idx as usize).map_err(|e| format!("monitor {idx}: {e}"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel::<VideoFrame>(4); // 4 frames of slack
        let flags = CaptureFlags { sink: tx, stop: stop.clone(), started: Instant::now() };

        let settings = Settings::new(
            monitor,
            CursorCaptureSettings::Default,
            DrawBorderSettings::Default,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Rgba8,
            flags,
        );

        // FrameHandler::start blocks; run it on a dedicated thread.
        let handle = std::thread::spawn(move || {
            if let Err(e) = FrameHandler::start(settings) {
                eprintln!("[display_wgc] capture ended: {e:?}");
            }
        });

        *thread_slot = Some(handle);
        *self.stop.lock().map_err(|e| e.to_string())? = Some(stop);
        Ok(rx)
    }

    fn stop_capture(&self) -> Result<(), String> {
        if let Some(stop) = self.stop.lock().map_err(|e| e.to_string())?.take() {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(handle) = self.thread.lock().map_err(|e| e.to_string())?.take() {
            let _ = handle.join();
        }
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "wgc"
    }
}
```

(API names to CONFIRM on Windows at impl time, flagged inline: `buffer.as_raw_nopadding_buffer()`, `Monitor::{name,width,height,from_index}` exact signatures, `FrameHandler::start`. The structure — handler trait, RGBA via `ColorFormat::Rgba8`, channel sink, stop flag, blocking-start-on-a-thread — is correct; the compiler on Windows pins the leaf names.)

- [ ] **Step 2: Register the module + wire the factory.** In `main.rs` add:

```rust
#[cfg(windows)]
mod display_wgc;
```

In `display.rs` `make_display_backend`, replace the fallback arm:

```rust
pub fn make_display_backend() -> Box<dyn DisplayBackend> {
    match std::env::var("FARDER_DISPLAY_BACKEND").as_deref() {
        Ok("mock") => Box::new(MockDisplayBackend::new()),
        _ => {
            #[cfg(windows)]
            {
                return Box::new(crate::display_wgc::WgcDisplayBackend::new());
            }
            #[allow(unreachable_code)]
            {
                crate::audio::log_once(
                    "display.real_not_shipped",
                    "[display] real backend only on Windows; using mock",
                );
                Box::new(MockDisplayBackend::new())
            }
        }
    }
}
```

- [ ] **Step 3: Headless gate.**

Run: `cd client/src-tauri && cargo test display:: -- --test-threads=1`
Expected: the mock display tests still PASS, and the Linux build (no windows-capture) is unaffected.

- [ ] **Step 4: Commit.**

```bash
git add client/src-tauri/src/display_wgc.rs client/src-tauri/src/display.rs client/src-tauri/src/main.rs
git commit -m "client: real Windows Graphics Capture display backend (RGBA, mock stays for non-Windows)"
```

- [ ] **Step 5: Owner Windows compile check (report, not code).** Note in the task report: the owner runs `cargo build -p farder-client` on Windows to confirm `display_wgc.rs` compiles against windows-capture 2.0.0; any leaf-API mismatch (`as_raw_nopadding_buffer`, `Monitor` accessors, `start`) is a one-line fix the compiler points at. The real runtime test happens in Task 7's loopback.

---

### Task 5: Capture→encode loop + Tauri commands

**Files:**
- Create: `client/src-tauri/src/screenshare.rs`
- Modify: `client/src-tauri/src/main.rs` (`mod screenshare;` + register the two commands)
- Modify: `client/src/lib/tauri-bridge.ts` (the two command wrappers)

- [ ] **Step 1: Create the module with the testable loop.** Create `client/src-tauri/src/screenshare.rs`:

```rust
//! Local screenshare PREVIEW (Phase B loopback): capture → H.264 encode →
//! emit each encoded frame to the webview, which decodes it via WebCodecs and
//! paints a canvas. No networking — this proves the capture/codec/decode path
//! end to end on one machine.

use crate::display::{make_display_backend, DisplayBackend, DisplayFormat, VideoFrame};
use crate::video_encoder::{EncodedFrame, H264Encoder};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, Emitter, State};

/// The capture→encode loop, factored to take a sink callback so it's testable
/// without a Tauri runtime. Forces a keyframe first, then encodes every frame
/// the backend delivers until `stop` is set or the channel closes. Encode
/// errors drop that frame (live policy) and keep going.
pub fn run_encode_loop(
    rx: Receiver<VideoFrame>,
    mut encoder: H264Encoder,
    stop: Arc<AtomicBool>,
    mut sink: impl FnMut(EncodedFrame),
) {
    encoder.force_keyframe();
    while !stop.load(Ordering::Relaxed) {
        let frame = match rx.recv() {
            Ok(f) => f,
            Err(_) => break, // capture ended
        };
        match encoder.encode(&frame) {
            Ok(encoded) => sink(encoded),
            Err(e) => eprintln!("[screenshare] encode dropped a frame: {e}"),
        }
    }
}

// --- live preview state (one at a time) -------------------------------------

struct ActivePreview {
    stop: Arc<AtomicBool>,
    backend: Box<dyn DisplayBackend>,
}

fn active() -> &'static Mutex<Option<ActivePreview>> {
    static ACTIVE: OnceLock<Mutex<Option<ActivePreview>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(None))
}

/// Tauri-side AppState slot is not needed — preview state is process-global
/// (only one local preview at a time in Phase B).

#[tauri::command]
pub async fn start_screenshare_preview(app: AppHandle, fps: u32, max_width: u32, max_height: u32) -> Result<(), String> {
    {
        let guard = active().lock().map_err(|e| e.to_string())?;
        if guard.is_some() {
            return Err("a screenshare preview is already running".into());
        }
    }
    let backend = make_display_backend();
    let sources = backend.enumerate_sources()?;
    let source_id = sources.first().map(|s| s.id.clone()).ok_or("no capture source")?;
    let format = DisplayFormat { fps, max_width, max_height };
    let rx = backend.start_capture(&source_id, format)?;
    let encoder = H264Encoder::new()?;
    let stop = Arc::new(AtomicBool::new(false));

    {
        let mut guard = active().lock().map_err(|e| e.to_string())?;
        *guard = Some(ActivePreview { stop: stop.clone(), backend });
    }

    let app_for_loop = app.clone();
    std::thread::spawn(move || {
        run_encode_loop(rx, encoder, stop, move |enc| {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&enc.data);
            let _ = app_for_loop.emit(
                "screenshare:frame",
                serde_json::json!({ "data": b64, "key": enc.is_keyframe, "ts": enc.timestamp_ms }),
            );
        });
    });
    Ok(())
}

#[tauri::command]
pub async fn stop_screenshare_preview() -> Result<(), String> {
    let preview = {
        let mut guard = active().lock().map_err(|e| e.to_string())?;
        guard.take()
    };
    if let Some(p) = preview {
        p.stop.store(true, Ordering::Relaxed);
        p.backend.stop_capture()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::MockDisplayBackend;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn mock_capture_through_encoder_yields_decodable_keyframe_first() {
        use openh264::decoder::Decoder;
        use openh264::nal_units;

        // Mock backend → real encoder → collect encoded frames via the sink.
        let backend = MockDisplayBackend::new();
        let rx = backend
            .start_capture("mock-display", DisplayFormat { fps: 30, max_width: 320, max_height: 240 })
            .unwrap();
        let encoder = H264Encoder::new().unwrap();
        let stop = Arc::new(AtomicBool::new(false));

        // Stop after 5 frames so the loop terminates.
        let count = Arc::new(AtomicUsize::new(0));
        let collected = Arc::new(Mutex::new(Vec::<EncodedFrame>::new()));
        let stop_for_sink = stop.clone();
        let count_for_sink = count.clone();
        let collected_for_sink = collected.clone();
        run_encode_loop(rx, encoder, stop.clone(), move |enc| {
            collected_for_sink.lock().unwrap().push(enc);
            if count_for_sink.fetch_add(1, Ordering::Relaxed) + 1 >= 5 {
                stop_for_sink.store(true, Ordering::Relaxed);
            }
        });
        backend.stop_capture().unwrap();

        let frames = collected.lock().unwrap();
        assert!(frames.len() >= 5, "expected >=5 encoded frames, got {}", frames.len());
        assert!(frames[0].is_keyframe, "first frame must be a keyframe");

        // Every frame is valid H.264 (decodes through openh264).
        let mut dec = Decoder::new().unwrap();
        let mut decoded_any = false;
        for f in frames.iter() {
            for nal in nal_units(&f.data) {
                if let Ok(Some(_)) = dec.decode(nal) {
                    decoded_any = true;
                }
            }
        }
        assert!(decoded_any, "encoded preview frames must decode");
    }
}
```

(`base64` and `serde_json` are already client deps. `tauri::Emitter` is the v2 trait providing `emit`.)

- [ ] **Step 2: Register.** In `main.rs`: add `mod screenshare;` and add `screenshare::start_screenshare_preview, screenshare::stop_screenshare_preview` to `generate_handler![...]`.

- [ ] **Step 3: Bridge wrappers.** In `client/src/lib/tauri-bridge.ts`:

```ts
export async function startScreensharePreview(fps: number, maxWidth: number, maxHeight: number): Promise<void> {
  return invoke<void>("start_screenshare_preview", { fps, maxWidth, maxHeight });
}

export async function stopScreensharePreview(): Promise<void> {
  return invoke<void>("stop_screenshare_preview");
}
```

- [ ] **Step 4: Run the tests + seam check.**

Run: `cd client/src-tauri && cargo test screenshare:: -- --test-threads=1`
Expected: the loopback test PASSES (mock capture → encode → ≥5 frames, first is a keyframe, all decode).
Seam: `grep -c "start_screenshare_preview\|stop_screenshare_preview" src/main.rs` ≥ 2.

- [ ] **Step 5: Commit.**

```bash
git add client/src-tauri/src/screenshare.rs client/src-tauri/src/main.rs client/src/lib/tauri-bridge.ts
git commit -m "client: local screenshare preview loop (capture -> encode -> emit), mock-loopback tested"
```

---

### Task 6: Frontend WebCodecs viewer

**Files:**
- Create: `client/src/components/ScreensharePreview.tsx`
- Modify: a dev-reachable mount point — `client/src/components/VoiceSettings.tsx` (add a "Screen Share (preview)" section)

- [ ] **Step 1: Create the viewer component.** Create `client/src/components/ScreensharePreview.tsx`:

```tsx
import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import * as api from "../lib/tauri-bridge";

// OpenH264 emits Annex-B (start codes); Chromium's VideoDecoder accepts it when
// configure() is called WITHOUT a `description`. Constrained Baseline 3.0.
const H264_CODEC = "avc1.42E01E";

function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export default function ScreensharePreview() {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const decoderRef = useRef<VideoDecoder | null>(null);
  const gotKeyRef = useRef(false);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!("VideoDecoder" in window)) {
      setError("WebCodecs VideoDecoder is not available in this webview.");
    }
  }, []);

  useEffect(() => {
    const unlistenPromise = listen<{ data: string; key: boolean; ts: number }>(
      "screenshare:frame",
      (e) => {
        const dec = decoderRef.current;
        if (!dec) return;
        const { data, key, ts } = e.payload;
        // The decoder must start on a keyframe; ignore deltas until the first key.
        if (!gotKeyRef.current && !key) return;
        if (key) gotKeyRef.current = true;
        try {
          const chunk = new EncodedVideoChunk({
            type: key ? "key" : "delta",
            timestamp: ts * 1000, // ms → µs
            data: b64ToBytes(data),
          });
          dec.decode(chunk);
        } catch (err) {
          setError(String(err));
        }
      },
    );
    return () => { unlistenPromise.then((u) => u()); };
  }, []);

  async function start() {
    setError(null);
    gotKeyRef.current = false;
    const canvas = canvasRef.current!;
    const ctx = canvas.getContext("2d")!;
    const decoder = new VideoDecoder({
      output: (frame) => {
        canvas.width = frame.displayWidth;
        canvas.height = frame.displayHeight;
        ctx.drawImage(frame, 0, 0, canvas.width, canvas.height);
        frame.close();
      },
      error: (err) => setError(String(err)),
    });
    decoder.configure({ codec: H264_CODEC, optimizeForLatency: true });
    decoderRef.current = decoder;
    try {
      await api.startScreensharePreview(30, 1280, 720);
      setRunning(true);
    } catch (err) {
      setError(String(err));
    }
  }

  async function stop() {
    try { await api.stopScreensharePreview(); } catch {}
    setRunning(false);
    const dec = decoderRef.current;
    if (dec && dec.state !== "closed") dec.close();
    decoderRef.current = null;
    gotKeyRef.current = false;
  }

  return (
    <div className="screenshare-preview">
      <div className="screenshare-preview-controls">
        {running ? (
          <button className="xp-button" onClick={stop}>Stop preview</button>
        ) : (
          <button className="xp-button" onClick={start}>Start screen preview</button>
        )}
      </div>
      {error && <div className="screenshare-preview-error">{error}</div>}
      <canvas ref={canvasRef} className="screenshare-preview-canvas" />
    </div>
  );
}
```

- [ ] **Step 2: Type support for WebCodecs.** WebCodecs DOM types (`VideoDecoder`, `EncodedVideoChunk`) ship with TypeScript's `lib.dom` in current versions. If `npx tsc --noEmit` reports them as unknown, add `"dom"` is already present — instead add a one-line ambient fallback at the top of the file is NOT needed; modern `@types`/tsconfig include them. If tsc errors on the WebCodecs types, add `"lib": ["...","DOM","DOM.Iterable","ESNext"]` is already set — verify `tsconfig.json` and, only if needed, add `// @ts-expect-error` is NOT acceptable; instead install/enable the WebCodecs lib. (Check first; most likely it just works.)

- [ ] **Step 3: Mount it for the loopback test.** In `client/src/components/VoiceSettings.tsx`, import and render `<ScreensharePreview />` inside a clearly-labeled dev section:

```tsx
import ScreensharePreview from "./ScreensharePreview";
// ... inside the settings JSX, add a section:
        <div className="settings-section">
          <h3>Screen Share (preview — Phase B)</h3>
          <ScreensharePreview />
        </div>
```

(Match the file's existing section markup; if VoiceSettings uses different section classes, mirror them.)

- [ ] **Step 4: Theme CSS.** Add minimal styles for `.screenshare-preview-canvas` (max-width 100%, a border via `var(--xp-border)`, black background), `.screenshare-preview-controls` (margin), `.screenshare-preview-error` (`color: var(--xp-text-muted)` or an error color via theme var) to ALL THREE theme files (`client/src/themes/*/theme.css`), colors via theme vars only (CLAUDE.md). It's a dev surface, so keep it minimal.

- [ ] **Step 5: Type-check.**

Run: `cd client && npx tsc --noEmit`
Expected: clean.

- [ ] **Step 6: Commit.**

```bash
git add client/src/components/ScreensharePreview.tsx client/src/components/VoiceSettings.tsx client/src/themes/*/theme.css
git commit -m "client ui: WebCodecs screenshare preview canvas (Phase B loopback viewer)"
```

---

### Task 7: Docs + verification

**Files:**
- Create: `docs/modules/screenshare-capture-codec.md`
- Modify: `ARCHITECTURE.md`
- Modify: `docs/modules/tauri-commands.md` (the two new commands) + `docs/modules/tauri-bridge.md` (the `screenshare:frame` event)

- [ ] **Step 1: Write the docs.** `screenshare-capture-codec.md` (from `_TEMPLATE.md`): the DisplayBackend seam (mock vs WGC), `H264Encoder` (RGBA→YUV→Annex-B, keyframe forcing), `run_encode_loop` + the preview commands, the `screenshare:frame` event payload (`{data: base64, key, ts}`), and the WebCodecs viewer (Annex-B, no description, key-first). Note the Phase-B scope: loopback only, no networking, one preview at a time; Phase C carries video over the relay/datagram transport (Phase A) and Phase E builds the real UI. `tauri-commands.md`: `start_screenshare_preview(fps, maxWidth, maxHeight)` / `stop_screenshare_preview()` (params, side effects, matching `invoke` names). `tauri-bridge.md`: the `screenshare:frame` event. `ARCHITECTURE.md`: one line — screensharing capture/encode (openh264 + Windows Graphics Capture) → WebCodecs decode in the webview; Phase B is a local loopback.

- [ ] **Step 2: Headless gate.**

```bash
cd /home/deez/farder/client/src-tauri && cargo build && cargo test -- --test-threads=1 2>&1 | grep -E "^test result"
cd /home/deez/farder/client && npx tsc --noEmit && echo TSC_OK
cd /home/deez/farder && for c in start_screenshare_preview stop_screenshare_preview; do grep -q "$c" client/src-tauri/src/main.rs && grep -q "\"$c\"" client/src/lib/tauri-bridge.ts && echo "OK $c" || echo "MISSING $c"; done
```

Expected: all green; both seam lines `OK`. (Single-threaded for the known `FARDER_DATA` env race.)

- [ ] **Step 3: Commit.**

```bash
git add docs/ ARCHITECTURE.md
git commit -m "docs: screenshare capture+codec (Phase B loopback)"
```

- [ ] **Step 4: Owner runtime verification (report, not code).** Per CLAUDE.md, UNVERIFIED until the owner's Windows run. Steps: rebuild the client on Windows (`cargo build -p farder-server` for the sidecar is NOT needed — Phase B touches only the client; just rebuild + restart `npm run tauri dev`). Then: (a) **Codec/webview isolation test** — set `FARDER_DISPLAY_BACKEND=mock`, open Settings → "Screen Share (preview)", click Start → the animated gradient + frame counter should appear on the canvas (proves openh264 encode → WebCodecs decode → canvas, independent of WGC). (b) **Real capture test** — unset the env var (real WGC backend), Start → your actual screen should appear on the canvas. Confirm Stop ends it cleanly and a second Start works. This is the whole Phase B deliverable: your screen, captured → H.264 → decoded in the webview → painted, locally.

---

## Self-review notes (done at plan time)

- **Spec coverage (Phase B scope):** real WGC capture behind the DisplayBackend seam, mock retained (Task 4); openh264 encode + BGRA/RGBA→I420 (Tasks 2–3, using `ColorFormat::Rgba8` so no swap); WebCodecs decode + canvas (Task 6); captured-then-decoded loopback proof (Task 5 headless via openh264 decoder + Task 7 runtime via WebCodecs); no networking (everything is local emit). Phases C/D/E explicitly out of scope.
- **Testability split:** the codec wrapper and the capture→encode loop are fully tested on Linux (openh264 builds + runs there) including an encode→decode round-trip; the Windows-only WGC backend and the webview WebCodecs decode are owner-verified at runtime, with the headless gate keeping the mock path + Linux build green.
- **Type consistency:** `VideoFrame` (RGBA8888 packed, stride=width*4) is the seam between capture and encode; `EncodedFrame{data,is_keyframe,timestamp_ms}` flows capture-loop → event → WebCodecs `EncodedVideoChunk{type,timestamp,data}`; the `screenshare:frame` payload `{data:base64,key,ts}` matches on both sides.
- **Flagged uncertainties (honest):** the openh264 config call (`OpenH264API::from_source()` / `FrameRate::new` arg type) has a stated `Encoder::new()` fallback; the windows-capture leaf calls (`as_raw_nopadding_buffer`, `Monitor` accessors, `FrameHandler::start`) are confirmed-at-compile-on-Windows — structure correct, leaf names pinned by the compiler the owner runs. Both are isolated so a miss is a one-line fix, not a redesign.
- **Known follow-ups (Phase C+, not here):** carry video over the Phase A datagram transport (fragment is already built); keyframe-on-viewer-join; a Tauri `Channel` instead of base64 events for lower overhead; the real Share-button/picker/LIVE-badge/viewer UI (Phase E); the WGC capture picker for window/fullscreen selection (Phase B uses the first/primary monitor).
