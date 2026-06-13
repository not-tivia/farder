# Screenshare capture + codec (Phase B loopback)

> **File(s):** `client/src-tauri/src/display.rs`, `client/src-tauri/src/display_wgc.rs`, `client/src-tauri/src/video_encoder.rs`, `client/src-tauri/src/screenshare.rs`, `client/src/components/ScreensharePreview.tsx`
> **Layer:** Tauri command / Frontend component
> **Last reviewed:** 2026-06-13

## Purpose

Phase B proves the local capture → H.264 encode → WebCodecs decode path end to
end on one machine. It owns: the `DisplayBackend` seam (mock vs. real WGC), the
`H264Encoder` (RGBA → YUV → Annex-B), the encode loop and two Tauri preview
commands, and the React WebCodecs viewer component. It deliberately does NOT do
any networking — frames are emitted as Tauri events and consumed in the same
webview. Networking is a Phase C concern.

**Phase B scope:** local loopback only. One preview at a time. No screen audio.
No real share UI (the viewer is wired into VoiceSettings as a developer section).

**Roadmap:**
- Phase C — carry the encoded video over the Phase A media-datagram transport
  (fragment/reassemble already built; see `docs/modules/media-datagram.md`).
- Phase D — capture and mix screen audio into the stream.
- Phase E — build the real in-call screen-share UI (participant view, share
  toggle, permission request).

---

## The DisplayBackend seam

`display.rs` defines the `DisplayBackend` trait — the abstraction between the
capture pipeline and the platform-specific screen-grab implementation.

### `DisplayBackend` trait

```
fn enumerate_sources() -> Result<Vec<DisplaySource>, String>
fn start_capture(source_id, format) -> Result<Receiver<VideoFrame>, String>
fn stop_capture() -> Result<(), String>
fn backend_name() -> &'static str
```

**`VideoFrame` contract:** RGBA8888, row-major, packed — `stride == width * 4`
(no row padding). `pixels.len() >= height * width * 4`. `timestamp_ms` is
milliseconds since capture started (monotonic, not wall clock).

**`DisplayFormat`:** `fps: u32`, `max_width: u32`, `max_height: u32`.
In Phase B the WGC backend captures at the monitor's native resolution; the
format dimensions are advisory (a bounded channel bounds throughput; downscaling
is Phase C).

### `make_display_backend() -> Box<dyn DisplayBackend>`

Returns the backend for the current environment, selected at runtime:

1. `FARDER_DISPLAY_BACKEND=mock` (or any non-Windows host) → `MockDisplayBackend`
2. `FARDER_DISPLAY_BACKEND` unset + Windows → `WgcDisplayBackend`
3. `FARDER_DISPLAY_BACKEND=wgc` + Windows → `WgcDisplayBackend`

The mock delivers synthetic gradient frames in a background thread; it is what
all tests and CI run (Linux/WSL has no display capture).

### Mock backend (`MockDisplayBackend`)

`display.rs`. Generates frames with a scrolling color gradient plus a
5×7-pixel digit counter overlay so each frame is visually distinct. Frames are
sent into a `mpsc::sync_channel(4)`. Stops when `stop_capture()` is called
(sets an `AtomicBool` that the generator thread checks after each frame, then
joins the thread).

### Real backend — Windows Graphics Capture (`WgcDisplayBackend`)

`display_wgc.rs` (`#![cfg(windows)]`). Uses the `windows-capture` 2.0.0 crate.

**Capture flow:**
1. `enumerate_sources()` calls `Monitor::enumerate()` and returns one
   `DisplaySource` per monitor (id: `"monitor:<1-based-index>"`).
2. `start_capture()` parses the `"monitor:<N>"` id, calls
   `Monitor::from_index(N)` (1-based), builds `Settings` with
   `ColorFormat::Rgba8`, and calls `FrameHandler::start_free_threaded(settings)`
   which spawns the WGC session on its own thread and returns a `CaptureControl`
   handle.
3. `FrameHandler::on_frame_arrived()` calls `frame.buffer()` then
   `buffer.as_raw_nopadding_buffer()` to get a packed RGBA slice (stride =
   width * 4), copies it into a `VideoFrame`, and does a non-blocking `try_send`
   to the channel (drops the frame if the encoder is behind — live policy).
4. `stop_capture()` calls `CaptureControl::stop()` to tear down the WGC session
   and join its thread.

**External-stop design (key invariant):** WGC only calls `on_frame_arrived` when
the screen content changes. On a static or idle screen, no frames arrive, so a
stop flag checked only inside the callback would never be seen — `stop_capture`
would block indefinitely (and the capture session would keep running, a privacy
bug). `start_free_threaded` solves this: it returns a `CaptureControl` that can
tear down the session from OUTSIDE the callback, regardless of frame delivery.
The `AtomicBool` flag is kept only as belt-and-braces for in-flight frames that
arrive after `control.stop()` is called.

**Leaf API names to confirm on Windows (owner-verification required):**
- `CaptureControl<FrameHandler, <FrameHandler as GraphicsCaptureApiHandler>::Error>` — exact generic type.
- `FrameHandler::start_free_threaded(settings) -> Result<CaptureControl<...>, Error>`.
- `CaptureControl::stop() -> Result<(), Error>` — stops the session and joins the capture thread.
- `frame.buffer() -> Result<FrameBuffer, Error>` — frame accessor.
- `buffer.as_raw_nopadding_buffer() -> Result<&[u8], Error>` — packed RGBA accessor (stride = width * 4).
- `Monitor::enumerate() -> Result<Vec<Monitor>, Error>`.
- `Monitor::from_index(usize) -> Result<Monitor, Error>` — 1-based index.
- `Monitor::name() -> Result<String, Error>`, `Monitor::width() -> Result<u32, Error>`, `Monitor::height() -> Result<u32, Error>`.

---

## H264Encoder (`video_encoder.rs`)

Wraps `openh264::encoder::Encoder`. Converts packed RGBA frames to planar YUV
(via `RgbaSliceU8` / `YUVBuffer::from_rgb_source`) then encodes to Annex-B
H.264. The output byte stream has H.264 start codes (`0x00 0x00 0x00 0x01`)
with SPS/PPS inline before each IDR.

**`!Send` constraint:** the `openh264` `Encoder` is not `Send`. The encoder must
be constructed on and used from the same thread — the dedicated capture/encode
thread spawned by `start_screenshare_preview`. It must never be passed across a
thread boundary.

### `H264Encoder::new() -> Result<Self, String>`

Constructs the encoder with fixed `EncoderConfig`:
- Bitrate: 3 Mbps (`BitRate::from_bps(3_000_000)`)
- Max frame rate: 30 fps (`FrameRate::from_hz(30.0)`)
- Intra-frame period: 60 frames (~2 s keyframe interval) (`IntraFramePeriod::from_num_frames(60)`)

These are Phase B starting values; Phase C quality tuning (and NVENC hardware
acceleration) will revisit them.

### `H264Encoder::force_keyframe(&mut self)`

Forces the NEXT encoded frame to be an IDR keyframe. Called once before the
first frame so a fresh decoder can start decoding immediately. Should be called
again whenever a new viewer attaches mid-stream (not yet exposed to Phase B
callers but the mechanism is in place).

### `H264Encoder::encode(&mut self, frame: &VideoFrame) -> Result<EncodedFrame, String>`

Converts the RGBA frame to YUV via `RgbaSliceU8::new` + `YUVBuffer::from_rgb_source`,
then calls `enc.encode(&yuv)` and returns the resulting bitstream. Checks that
`stride == width * 4` and `pixels.len() >= height * width * 4` (fails loudly on
a padded frame rather than producing garbage output). A keyframe is detected by
matching `FrameType::IDR | FrameType::I` on the bitstream result.

**`EncodedFrame`:**
```
pub struct EncodedFrame {
    pub data: Vec<u8>,          // Annex-B NAL byte stream; SPS/PPS inline before IDR
    pub is_keyframe: bool,      // true for IDR/I frames
    pub timestamp_ms: u64,      // from the input VideoFrame
}
```

---

## Encode loop and Tauri commands (`screenshare.rs`)

### `run_encode_loop(rx, encoder, stop, sink)`

The encode loop. Factored out so it is testable without a Tauri runtime.

**Must run on its own thread:** because `H264Encoder` is `!Send`, the encoder is
constructed inside the thread spawned by `start_screenshare_preview` and passed
directly to `run_encode_loop` — it never crosses a thread boundary.

Forces a keyframe first, then loops: receive a `VideoFrame` from `rx`, encode
it, pass the `EncodedFrame` to `sink`. Encode errors drop the frame (live
policy — a single frame loss is preferable to stalling the pipeline). Exits when
`stop` is set or the channel closes (capture ended).

**Parameters:**
- `rx: Receiver<VideoFrame>` — channel from the DisplayBackend.
- `encoder: H264Encoder` — takes ownership; must be constructed on the calling thread.
- `stop: Arc<AtomicBool>` — set externally by `stop_screenshare_preview`.
- `sink: impl FnMut(EncodedFrame)` — called for each successfully encoded frame.

---

### `start_screenshare_preview(app, fps, max_width, max_height) -> Result<(), String>`

**What it does:** starts a local capture→encode→emit loop. One preview at a time.

**Parameters:**
- `fps` — target frames per second (passed to `DisplayFormat`; advisory for WGC backend).
- `max_width`, `max_height` — advisory max dimensions (see DisplayFormat note).

**Returns:** `Ok(())` once the capture and encode thread are running. Errors if a
preview is already active (`"a screenshare preview is already running"`), if the
encoder fails to init (pre-flight check), or if the capture backend fails to
start.

**Side effects:**
1. Calls `make_display_backend()` and `backend.start_capture()`.
2. Stores the `ActivePreview` (stop flag + backend handle) in a `static Mutex`.
3. Spawns a `std::thread` that constructs `H264Encoder` and calls
   `run_encode_loop`. The sink base64-encodes each `EncodedFrame` and calls
   `app.emit("screenshare:frame", { data, key, ts })`.

**invoke name:** `"start_screenshare_preview"` → `startScreensharePreview(fps, maxWidth, maxHeight)`.

---

### `stop_screenshare_preview() -> Result<(), String>`

**What it does:** stops the active preview — sets the stop flag (which exits the
encode loop) and calls `backend.stop_capture()` (which tears down the WGC
session / joins the mock generator thread).

**Side effects:** takes `ActivePreview` from the static slot (idempotent — a
second call finds `None` and is a no-op); calls `stop.store(true)`;
calls `backend.stop_capture()`.

**invoke name:** `"stop_screenshare_preview"` → `stopScreensharePreview()`.

---

## `screenshare:frame` event

Emitted by `start_screenshare_preview`'s encode thread for each encoded frame.
See `docs/modules/tauri-bridge.md` for the full event catalog entry.

| Field | Type | Description |
|---|---|---|
| `data` | `string` | Base64-encoded Annex-B H.264 frame (SPS/PPS inline before IDR) |
| `key` | `boolean` | True if this is a keyframe (IDR/I) |
| `ts` | `number` | Capture timestamp in milliseconds (monotonic since capture started) |

---

## WebCodecs viewer (`ScreensharePreview.tsx`)

React component. Listens for `screenshare:frame` events, decodes via the
browser's `VideoDecoder` API (WebCodecs), and paints to a `<canvas>`.

**Codec string:** `"avc1.42E01E"` — Constrained Baseline profile 4.2, level 3.0.

**Annex-B mode:** `decoder.configure()` is called WITHOUT a `description` field.
This is correct for Annex-B input (start-code-prefixed NALs). Passing a
`description` (the AVCC/MP4 extradata form) would cause the decoder to expect a
length-prefixed stream and reject the Annex-B output from openh264.

**Key-first gate:** the decoder must start on a keyframe. The component tracks
`gotKeyRef` and silently drops all delta frames until the first keyframe is seen.
Once a keyframe arrives it is decoded and subsequent deltas are accepted.

**ms to µs conversion:** `EncodedVideoChunk.timestamp` is in microseconds;
`EncodedFrame.timestamp_ms` is in milliseconds. The component multiplies by 1000.

**Decoder lifecycle:** created in `start()`, configured, then stored in a `useRef`.
The event listener (wired in a separate `useEffect`) reads `decoderRef.current`
on each frame. `stop()` calls `dec.close()` and clears the ref.

**Mount location:** imported and rendered in `VoiceSettings.tsx` as a dev-only
section. Not part of the real share UI (Phase E).

**Bridge wrappers:** `startScreensharePreview(fps, maxWidth, maxHeight)` and
`stopScreensharePreview()` in `client/src/lib/tauri-bridge.ts`.

---

## State it owns

| Field | Type | What it tracks, when it's mutated |
|---|---|---|
| `ACTIVE` (static) | `OnceLock<Mutex<Option<ActivePreview>>>` | The single active preview (stop flag + backend); set by `start_screenshare_preview`, cleared by `stop_screenshare_preview` |
| `decoderRef` | `React.MutableRefObject<VideoDecoder \| null>` | The active WebCodecs decoder; set in `start()`, cleared in `stop()` |
| `gotKeyRef` | `React.MutableRefObject<boolean>` | True once a keyframe has been received; gates delta decoding |

## Events emitted

| Event name | Payload shape | Who listens |
|---|---|---|
| `"screenshare:frame"` | `{ data: string, key: boolean, ts: number }` | `ScreensharePreview.tsx` → WebCodecs `VideoDecoder` |

## Events / requests consumed

| Event | Source | What this module does with it |
|---|---|---|
| `"screenshare:frame"` | `start_screenshare_preview` encode thread | Decodes the Annex-B chunk via WebCodecs; paints the canvas |

## Integration map

- **`display.rs`** — `DisplayBackend` trait + `make_display_backend()` factory + `MockDisplayBackend`.
- **`display_wgc.rs`** — `WgcDisplayBackend`: Windows-only real capture; selected by `make_display_backend()` on Windows.
- **`video_encoder.rs`** — `H264Encoder`: RGBA → YUV → Annex-B; `EncodedFrame` output type.
- **`screenshare.rs`** — `run_encode_loop` + Tauri commands; owns the static `ACTIVE` preview slot.
- **`ScreensharePreview.tsx`** — WebCodecs decode + canvas paint; listens for `screenshare:frame`.
- **`VoiceSettings.tsx`** — mounts `<ScreensharePreview>` for Phase B development.
- **`tauri-bridge.ts`** — `startScreensharePreview` / `stopScreensharePreview` wrappers.
- **`media-datagram.md`** — Phase C will carry the encoded video over the Phase A
  datagram transport (fragment/reassemble already built for audio; video frame IDs
  extend the same wire format).

## Known gotchas

- **`H264Encoder` is `!Send`:** the encoder must be constructed and used on a
  single thread. In `start_screenshare_preview`, the encoder is built INSIDE the
  spawned thread and passed directly into `run_encode_loop`. Never move it across
  a thread boundary or store it in a `Mutex` for use from multiple threads.
- **Annex-B, no description:** `VideoDecoder.configure()` must be called without
  a `description` for Annex-B input. Adding a `description` would switch the
  decoder to AVCC (length-prefixed) mode and break all frames.
- **Key-first gating is mandatory:** if the decoder receives a delta before a
  keyframe, it will error or produce garbage. The `gotKeyRef` gate in
  `ScreensharePreview.tsx` and the `force_keyframe()` call at loop start together
  ensure the first emitted frame is always a keyframe.
- **WGC static-screen hang (external stop design):** WGC only delivers frames on
  screen change. The stop flag inside `on_frame_arrived` is never seen on a static
  screen. Always stop via `CaptureControl::stop()` (called from `stop_capture()`),
  not by waiting for the flag to propagate through a frame callback.
- **One preview at a time:** the `ACTIVE` static slot enforces this. Calling
  `start_screenshare_preview` while a preview is running returns an error; the
  caller must call `stop_screenshare_preview` first.
- **WGC leaf API names not CI-verified:** the `display_wgc.rs` code carries
  `OWNER-CONFIRM` comments for the exact API names on `windows-capture` 2.0.0
  that cannot be validated on Linux/WSL CI. See the leaf API list above; the
  owner must verify these compile and behave correctly on Windows.
