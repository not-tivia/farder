# Screensharing Phase D — Game/Screen Audio Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Capture the sharer's system/game audio (Windows WASAPI loopback of a user-selectable output device), send it E2EE over the call as a new `TrackKind::ScreenAudio` track riding the existing Opus path, and have viewers decode + mix it with its own independent volume — wired into the C2 Share lifecycle.

**Architecture:** A new `TrackKind::ScreenAudio` variant gets its **own** outer-datagram routing byte (`0x03`) so a sharer's mic (`Audio`) and game audio (`ScreenAudio`) never collide on the same `(session, kind)` route — exactly the separation C1 introduced for video. The **inner** sealed frame reuses the audio crypto (`seal_audio_packet_to_wire` / `open_audio_wire_frame`, inner type byte `0x01`) and the audio bandwidth cap. Capture is a new Windows-only `screen_audio_wasapi.rs` (the `wasapi 0.23` loopback path validated by `phaseD-probe/`), autoconverting any native device format to 48 kHz then downmixing to mono for Opus. A trimmed send loop (`voice/send_screen_audio.rs`) encodes/seals/fragments it. The controller's `start_screen_share` (from C2) also starts screen audio under a separate stream key + device choice; viewers decode it through the existing reassemble→open→Opus→ring path but into a **separate** mixer ring with an **independent gain** (so game audio volume is distinct from that peer's voice). The volume *slider* and polished share UI remain Phase E; Phase D ships a minimal output-device picker because the owner's default device is a silent virtual Sonar endpoint.

**Tech Stack:** Rust (`wasapi 0.23` loopback capture, `audiopus`/OpusEncoder, `farder-crypto` audio seal, `farder-protocol` media-datagram), the existing voice mixer/recv path, Tauri commands/events, React/TypeScript.

**Spec:** `docs/superpowers/specs/2026-06-12-screensharing-design.md` (Phase D). Native dep validated by the `phaseD-probe/` run on the owner's Windows box (see "Verified facts").

**Branch:** create `screenshare-phaseD` from `main` before Task 1. Finish with ff-merge + push, then delete `phaseD-probe/`.

**Scope note:** Phase D = game audio flows end-to-end and mixes at its own independent gain, with a minimal device picker. The volume *slider*, LIVE badge, polished viewer pane, and OS *video*-source picker are Phase E. Mono screen audio only (stereo is a later nicety, per spec).

---

## Verified codebase facts (read 2026-06-13 — exact)

- **`TrackKind`** (`crates/farder-protocol/src/server.rs:23-26`): `#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)] pub enum TrackKind { Audio, Video }`.
- **Outer-datagram routing byte** (`crates/farder-protocol/src/media_datagram.rs`): `track_kind_to_byte(k)` and `byte_to_track_kind(b)` map `Audio<->MEDIA_FRAME_TYPE_AUDIO (0x01)`, `Video<->MEDIA_FRAME_TYPE_VIDEO (0x02)`. The OuterHeader (26 bytes) carries this byte; the server + client recv route on it. **This is the routing identity** — ScreenAudio needs its own byte here.
- **Crypto type-byte constants** (`crates/farder-crypto/src/media.rs:69-73`): `MEDIA_FRAME_VERSION=0x02`, `MEDIA_FRAME_TYPE_AUDIO=0x01`, `MEDIA_FRAME_TYPE_VIDEO=0x02`. `MEDIA_FRAME_HEADER_LEN=28`. The **inner** sealed frame's `buf[1]` is this type byte.
- **Audio seal/open** (`crates/farder-crypto/src/media.rs`): `seal_audio_packet_to_wire(key,&seq,&session_id,&speaker_pk,&opus_packet) -> Result<Vec<u8>>` writes inner type `0x01`; `open_audio_wire_frame(key,wire) -> Result<(u64,[u8;32],Vec<u8>)>` validates `wire[1]==0x01`. **ScreenAudio reuses BOTH** (its inner frame is byte-identical to a mic-audio frame; the OUTER 0x03 byte already separated routing).
- **Server ingress/cap/activity** (`crates/farder-server/src/media_stream.rs`): `on_frame_ingress` routes on the OUTER `header.track_kind`. Cap select (`~243`): `match header.track_kind { Audio => audio_max_bps, Video => video_max_bps }`. Activity stamp (`~253`): `match header.track_kind { Audio => last_audio_frame_ms, Video => last_video_frame_ms }`. `ServerSession` has `last_audio_frame_ms`, `last_video_frame_ms: Option<u64>`. Activity loop (`~305`): `for kind in [TrackKind::Audio, TrackKind::Video]` + nested `match kind`. Inner-frame helpers `build_media_frame`/`parse_media_frame` (`~46-67`) also `match` kind/byte (these are about the inner frame; ScreenAudio maps to the audio byte there).
- **Inner-frame match sites elsewhere**: `media_datagram.rs` `track_kind_to_byte`/`byte_to_track_kind`; `client/src-tauri/src/commands.rs:~2851` `parse_track_kind("audio"|"video")`; `client/src-tauri/src/voice/mod.rs:~927` `on_peer_track_enabled` dispatch.
- **Audio send pipeline** (`client/src-tauri/src/voice/send.rs`): `OpusEncoder::new(OPUS_SAMPLE_RATE, 1, OPUS_DEFAULT_BITRATE_BPS)`; per 960-sample `PcmChunk`: APM→gate→`encoder.encode(&frame)->Vec<u8>`→`seal_audio_packet_to_wire(...)`→`fragment(TrackKind::Audio,&session_id,frame_id,&frame_bytes,DEFAULT_MAX_DGRAM_PAYLOAD)`→`(cfg.datagram_sink)(Bytes)`. `seq`/`frame_id` increment per frame. Screen audio needs the encode→seal→fragment tail WITHOUT APM/gate/mute/speaking.
- **Opus consts** (`client/src-tauri/src/opus_codec.rs`): `OPUS_SAMPLE_RATE=48_000`, `OPUS_FRAME_SAMPLES_MONO=960`, `OPUS_DEFAULT_BITRATE_BPS=24_000`. `OpusEncoder::new(sample_rate,channels,bitrate)->Result<_,String>`, `encode(&mut self,&[f32])->Result<Vec<u8>,String>`. `OpusDecoder::new(sample_rate,channels)` (recv.rs uses it).
- **AudioBackend trait** (`client/src-tauri/src/audio.rs`): already has `enumerate_output_devices() -> Result<Vec<AudioOutputDevice>, String>`. `PcmChunk { samples: Vec<f32>, timestamp_ms: u64 }`. `AudioFormat` struct exists. `AudioOutputDevice` has `id`/`name`-style fields (verify exact field names when implementing Task 7).
- **Recv/mix** (`client/src-tauri/src/voice/recv.rs` + `voice/mixer.rs`): `recv::run(RecvTaskConfig{session_id,stream_key,deafened,datagram_rx,pcm_ring})` does reassemble→`open_audio_wire_frame`→jitter→Opus decode→`pcm_ring.push_frame`. `PeerPcmRing::new(10)`. Mixer (`mixer.rs`) iterates `peer_rings: HashMap<SessionId,(Arc<PeerPcmRing>,Arc<AtomicU32>)>`, pops one frame per ring, multiplies by gain (`f32::from_bits`), sums + soft-clips. **Screen audio needs a SEPARATE ring with its own gain** so it mixes independently of the peer's voice.
- **C2 share lifecycle** (`client/src-tauri/src/voice/mod.rs`): `start_screen_share(fps,max_width,max_height)` derives+offers a Video key (`offer_video_key`), `enable_track(Video)`, spawns capture→encode→VideoSender; `stop_screen_share` tears down; `on_peer_stream_joined` (when sharing) re-offers the video key + sets force_keyframe + re-`enable_track(Video)`; `leave()` calls `shutdown_video_share`. `ActiveCall` holds `video_share: Option<VideoShareState>`, `my_session_id`, `channel_id`, `peer_rings`, `peer_keys: HashMap<(SessionId,TrackKind),[u8;32]>`. `offer_video_key(server,channel_id,&key)` wraps+offers a key to members (generalizable to any kind).
- **Probe-locked capture facts** (`phaseD-probe/` run, owner's Windows): `wasapi 0.23` builds + captures real audio. Default RENDER device + `initialize_client(&desired,&Direction::Capture,&mode)` = loopback. `StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: min_time }` (from `get_device_period`). Request `WaveFormat::new(32,32,&SampleType::Float,48000,2,None)` → WASAPI autoconverts any native fmt (incl. 96 kHz/8-ch) to 48 kHz/2-ch f32. Read via `capture_client.read_from_device_to_deque(&mut VecDeque<u8>)` then interpret LE f32. **Device enumeration**: `DeviceEnumerator::new()` → `get_default_device(&Direction::Render)`, `get_device_collection(&Direction::Render)` → `get_nbr_devices()`, `get_device_at_index(i)` → `Device::get_friendlyname()`, `get_id()`. **Virtual endpoints (Steam/Sonar) WEDGE inside the WASAPI init/start/read calls** — capture must run so a wedged device can't hang the call (use a dedicated thread that the controller abandons on stop; enumeration for the picker only reads names/ids, which is safe). **The owner's Windows default is a silent virtual Sonar device → the picker is required.**

---

### Task 1: Add `TrackKind::ScreenAudio` + routing byte 0x03 (protocol + crypto)

**Files:** Modify `crates/farder-protocol/src/server.rs`, `crates/farder-protocol/src/media_datagram.rs`, `crates/farder-crypto/src/media.rs`

- [ ] **Step 1: Add the crypto type-byte constant.** In `crates/farder-crypto/src/media.rs`, after `MEDIA_FRAME_TYPE_VIDEO`:
```rust
/// Type byte for a screen-audio track in the OUTER datagram header (routing).
/// NOTE: the INNER sealed frame for screen audio reuses the AUDIO byte (0x01)
/// because it is encrypted/decrypted with seal_audio_packet_to_wire /
/// open_audio_wire_frame — only the outer routing identity differs.
pub const MEDIA_FRAME_TYPE_SCREEN_AUDIO: u8 = 0x03;
```

- [ ] **Step 2: Add the enum variant.** In `crates/farder-protocol/src/server.rs:23-26`:
```rust
pub enum TrackKind {
    Audio,
    Video,
    ScreenAudio,
}
```

- [ ] **Step 3: Write the failing round-trip test.** Append to the tests module in `crates/farder-protocol/src/media_datagram.rs` (find `#[cfg(test)] mod tests`):
```rust
    #[test]
    fn screen_audio_track_kind_round_trips_through_outer_byte() {
        use farder_protocol::server::TrackKind;
        let sid = [9u8; 16];
        let dgrams = super::fragment(TrackKind::ScreenAudio, &sid, 0, b"opaque", super::DEFAULT_MAX_DGRAM_PAYLOAD);
        let (hdr, _payload) = super::OuterHeader::parse(&dgrams[0]).expect("valid header");
        assert_eq!(hdr.track_kind, TrackKind::ScreenAudio);
    }
```
(Adapt `super::` paths to how the existing tests in that file call `fragment`/`OuterHeader::parse` — match them exactly.)

- [ ] **Step 4: Run it — expect compile failure** (non-exhaustive match) in `track_kind_to_byte`/`byte_to_track_kind`:
```
cargo test -p farder-protocol media_datagram 2>&1 | tail -20
```
Expected: `error[E0004]: non-exhaustive patterns: TrackKind::ScreenAudio not covered`.

- [ ] **Step 5: Add the outer-byte mapping.** In `crates/farder-protocol/src/media_datagram.rs`, import the new const (mirror how `MEDIA_FRAME_TYPE_AUDIO` is imported — likely `use farder_crypto::media::{..., MEDIA_FRAME_TYPE_SCREEN_AUDIO};` or a local re-declare; match the file's existing import) and extend both fns:
```rust
fn track_kind_to_byte(k: TrackKind) -> u8 {
    match k {
        TrackKind::Audio => MEDIA_FRAME_TYPE_AUDIO,
        TrackKind::Video => MEDIA_FRAME_TYPE_VIDEO,
        TrackKind::ScreenAudio => MEDIA_FRAME_TYPE_SCREEN_AUDIO,
    }
}

fn byte_to_track_kind(b: u8) -> Option<TrackKind> {
    match b {
        MEDIA_FRAME_TYPE_AUDIO => Some(TrackKind::Audio),
        MEDIA_FRAME_TYPE_VIDEO => Some(TrackKind::Video),
        MEDIA_FRAME_TYPE_SCREEN_AUDIO => Some(TrackKind::ScreenAudio),
        _ => None,
    }
}
```

- [ ] **Step 6: Run the test — expect PASS:**
```
cargo test -p farder-protocol media_datagram 2>&1 | grep "test result"
```
Expected: all pass incl. `screen_audio_track_kind_round_trips_through_outer_byte`.

- [ ] **Step 7: Commit:**
```bash
git add crates/farder-protocol/src/server.rs crates/farder-protocol/src/media_datagram.rs crates/farder-crypto/src/media.rs
git commit -m "protocol: add TrackKind::ScreenAudio with its own outer routing byte 0x03"
```

---

### Task 2: Server — cap, activity, and inner-frame match arms for ScreenAudio

**Files:** Modify `crates/farder-server/src/media_stream.rs`

- [ ] **Step 1: Add the activity field.** In `ServerSession` (where `last_audio_frame_ms`/`last_video_frame_ms` are declared, ~line 153):
```rust
    pub last_screen_audio_frame_ms: Option<u64>,
```
Initialize it (`last_screen_audio_frame_ms: None`) at EVERY `ServerSession { ... }` construction — there is one in `handlers.rs` (`JoinStream`) and several in tests. Grep `ServerSession {` across `crates/farder-server/` and add the field to each.

- [ ] **Step 2: Extend the cap match** (`on_frame_ingress`, ~243). Screen audio uses the audio cap:
```rust
    let cap = match header.track_kind {
        TrackKind::Audio => config.audio_max_bps,
        TrackKind::Video => config.video_max_bps,
        TrackKind::ScreenAudio => config.audio_max_bps,
    };
```

- [ ] **Step 3: Extend the activity-stamp match** (~253):
```rust
    match header.track_kind {
        TrackKind::Audio => session.last_audio_frame_ms = Some(now_ms),
        TrackKind::Video => session.last_video_frame_ms = Some(now_ms),
        TrackKind::ScreenAudio => session.last_screen_audio_frame_ms = Some(now_ms),
    }
```

- [ ] **Step 4: Extend the activity loop** (`compute_activity_transitions`, ~305):
```rust
        for kind in [TrackKind::Audio, TrackKind::Video, TrackKind::ScreenAudio] {
            if !session.active_tracks.contains(&kind) {
                continue;
            }
            let last_ms = match kind {
                TrackKind::Audio => session.last_audio_frame_ms,
                TrackKind::Video => session.last_video_frame_ms,
                TrackKind::ScreenAudio => session.last_screen_audio_frame_ms,
            };
```

- [ ] **Step 5: Extend the inner-frame helpers** (`build_media_frame` ~64, and the `parse_media_frame` match ~46). The inner screen-audio frame is byte-identical to audio, so:
```rust
    // build_media_frame:
    let type_byte = match kind {
        TrackKind::Audio => MEDIA_FRAME_TYPE_AUDIO,
        TrackKind::Video => MEDIA_FRAME_TYPE_VIDEO,
        TrackKind::ScreenAudio => MEDIA_FRAME_TYPE_AUDIO, // inner frame == audio
    };
```
`parse_media_frame` (`match buf[1]`) needs no new arm — `0x01 => Audio` already covers a screen-audio inner frame, and the server routes on the OUTER header, not this. If the compiler flags any OTHER exhaustive `match` on `TrackKind` in `crates/farder-server/`, add a `ScreenAudio` arm mirroring `Audio`.

- [ ] **Step 6: Build + test the server:**
```
cargo test -p farder-server media_stream 2>&1 | grep -E "test result|error\["
```
Expected: clean compile, all media_stream tests pass.

- [ ] **Step 7: Commit:**
```bash
git add crates/farder-server/src/media_stream.rs crates/farder-server/src/handlers.rs
git commit -m "server: route + cap + track ScreenAudio (reuses the audio cap)"
```

---

### Task 3: WASAPI loopback capture module (`screen_audio_wasapi.rs`)

**Files:** Create `client/src-tauri/src/screen_audio.rs` (cross-platform seam + mock), create `client/src-tauri/src/screen_audio_wasapi.rs` (cfg(windows) real backend), modify `client/src-tauri/src/main.rs` (or `lib.rs` — wherever modules are declared) to add `mod screen_audio; #[cfg(windows)] mod screen_audio_wasapi;`, modify `client/src-tauri/Cargo.toml`

This is the only native-unknown task; the real WASAPI path is **UNVERIFIED until the owner's Windows run** (the `phaseD-probe/` confirmed the exact API used here). Build on Linux exercises the seam + mock only.

- [ ] **Step 1: Add the dependency.** In `client/src-tauri/Cargo.toml`, under a Windows-only target table (mirror how `windows-capture` is gated for Phase B — find `[target.'cfg(windows)'.dependencies]`):
```toml
[target.'cfg(windows)'.dependencies]
# ...existing (windows-capture, etc.)...
wasapi = "0.23.0"
```

- [ ] **Step 2: Define the seam + an output-device list type.** Create `client/src-tauri/src/screen_audio.rs`:
```rust
//! Screen-audio (game audio) capture seam. The real implementation is WASAPI
//! output-device loopback on Windows (screen_audio_wasapi.rs); elsewhere a mock
//! tone so the pipeline is testable headlessly. Produces 48 kHz MONO PcmChunks
//! (exactly OPUS_FRAME_SAMPLES_MONO per chunk) on an mpsc channel, matching the
//! voice send path's contract.

use crate::audio::PcmChunk;
use std::sync::mpsc;

/// A selectable system output (render) device for loopback capture.
#[derive(Clone, Debug, serde::Serialize)]
pub struct OutputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// A running loopback capture. Dropping/`stop()`-ing it ends the capture thread.
pub trait ScreenAudioCapture: Send {
    /// Stop capturing (idempotent).
    fn stop(&self);
}

/// List output devices available for loopback capture (for the picker).
pub fn list_output_devices() -> Result<Vec<OutputDevice>, String> {
    #[cfg(windows)]
    {
        crate::screen_audio_wasapi::list_output_devices()
    }
    #[cfg(not(windows))]
    {
        Ok(vec![OutputDevice { id: "mock".into(), name: "Mock Output (no WASAPI)".into(), is_default: true }])
    }
}

/// Start loopback capture of `device_id` (None = system default render device).
/// Delivers 48 kHz mono PcmChunks of OPUS_FRAME_SAMPLES_MONO samples each.
pub fn start_capture(device_id: Option<String>) -> Result<(Box<dyn ScreenAudioCapture>, mpsc::Receiver<PcmChunk>), String> {
    #[cfg(windows)]
    {
        crate::screen_audio_wasapi::start_capture(device_id)
    }
    #[cfg(not(windows))]
    {
        let _ = device_id;
        start_mock_capture()
    }
}

#[cfg(not(windows))]
fn start_mock_capture() -> Result<(Box<dyn ScreenAudioCapture>, mpsc::Receiver<PcmChunk>), String> {
    use crate::opus_codec::{OPUS_FRAME_SAMPLES_MONO, OPUS_SAMPLE_RATE};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct MockCap { stop: Arc<AtomicBool> }
    impl ScreenAudioCapture for MockCap { fn stop(&self) { self.stop.store(true, Ordering::Relaxed); } }

    let (tx, rx) = mpsc::channel::<PcmChunk>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = stop.clone();
    std::thread::spawn(move || {
        let mut phase = 0.0f32;
        while !stop_t.load(Ordering::Relaxed) {
            let mut samples = Vec::with_capacity(OPUS_FRAME_SAMPLES_MONO);
            for _ in 0..OPUS_FRAME_SAMPLES_MONO {
                samples.push((phase).sin() * 0.3);
                phase += 2.0 * std::f32::consts::PI * 440.0 / OPUS_SAMPLE_RATE as f32;
            }
            if tx.send(PcmChunk { samples, timestamp_ms: 0 }).is_err() { break; }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });
    Ok((Box::new(MockCap { stop }), rx))
}
```

- [ ] **Step 3: Write the seam test** (append a `#[cfg(test)] mod tests` to `screen_audio.rs`):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::opus_codec::OPUS_FRAME_SAMPLES_MONO;
    use std::time::Duration;

    #[test]
    fn capture_delivers_mono_frames_of_the_right_size() {
        // On non-Windows this is the mock; on Windows it opens the default
        // render device. Either way the chunk contract must hold.
        let (cap, rx) = start_capture(None).expect("capture starts");
        let chunk = rx.recv_timeout(Duration::from_secs(2)).expect("a chunk arrives");
        assert_eq!(chunk.samples.len(), OPUS_FRAME_SAMPLES_MONO, "must be one 20ms mono frame");
        cap.stop();
    }

    #[test]
    fn lists_at_least_one_output_device() {
        let devices = list_output_devices().expect("enumerate");
        assert!(!devices.is_empty(), "must list at least one output device");
    }
}
```

- [ ] **Step 4: Implement the real WASAPI backend.** Create `client/src-tauri/src/screen_audio_wasapi.rs` (this is `cfg(windows)` only; it will NOT compile-check on Linux — it is gated, like `display_wgc.rs`). Use the probe-confirmed API verbatim:
```rust
//! Windows WASAPI output-device LOOPBACK capture for screen/game audio.
//! Confirmed against the phaseD-probe run (wasapi 0.23): default/selected RENDER
//! device, initialize_client in CAPTURE direction = loopback, autoconvert to
//! 48 kHz/2ch f32, then downmix to mono + chunk to OPUS_FRAME_SAMPLES_MONO.

use crate::audio::PcmChunk;
use crate::opus_codec::{OPUS_FRAME_SAMPLES_MONO, OPUS_SAMPLE_RATE};
use crate::screen_audio::{OutputDevice, ScreenAudioCapture};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use wasapi::*;

pub fn list_output_devices() -> Result<Vec<OutputDevice>, String> {
    initialize_mta().ok().map_err(|e| format!("COM init: {e}"))?;
    let enumerator = DeviceEnumerator::new().map_err(|e| e.to_string())?;
    let default_id = enumerator
        .get_default_device(&Direction::Render)
        .ok()
        .and_then(|d| d.get_id().ok());
    let collection = enumerator
        .get_device_collection(&Direction::Render)
        .map_err(|e| e.to_string())?;
    let n = collection.get_nbr_devices().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for i in 0..n {
        if let Ok(device) = collection.get_device_at_index(i) {
            let name = device.get_friendlyname().unwrap_or_else(|_| "<unknown>".into());
            if let Ok(id) = device.get_id() {
                let is_default = Some(&id) == default_id.as_ref();
                out.push(OutputDevice { id, name, is_default });
            }
        }
    }
    Ok(out)
}

struct WasapiCapture { stop: Arc<AtomicBool> }
impl ScreenAudioCapture for WasapiCapture {
    fn stop(&self) { self.stop.store(true, Ordering::Relaxed); }
}

pub fn start_capture(device_id: Option<String>) -> Result<(Box<dyn ScreenAudioCapture>, mpsc::Receiver<PcmChunk>), String> {
    let (tx, rx) = mpsc::channel::<PcmChunk>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = stop.clone();

    // The whole capture lives on its own thread (WASAPI COM objects are not Send,
    // and a wedged virtual device must never block the caller). The thread ends
    // when `stop` is set (checked each buffer) or the receiver is dropped.
    std::thread::spawn(move || {
        if let Err(e) = capture_loop(device_id, &tx, &stop_t) {
            eprintln!("[screen_audio] capture loop ended: {e}");
        }
    });
    Ok((Box::new(WasapiCapture { stop }), rx))
}

fn capture_loop(device_id: Option<String>, tx: &mpsc::Sender<PcmChunk>, stop: &AtomicBool) -> Result<(), String> {
    initialize_mta().ok().map_err(|e| format!("COM init: {e}"))?;
    let enumerator = DeviceEnumerator::new().map_err(|e| e.to_string())?;

    // Pick the requested device, else the default render device.
    let device = match device_id {
        Some(id) => {
            let collection = enumerator.get_device_collection(&Direction::Render).map_err(|e| e.to_string())?;
            let n = collection.get_nbr_devices().map_err(|e| e.to_string())?;
            let mut found = None;
            for i in 0..n {
                if let Ok(d) = collection.get_device_at_index(i) {
                    if d.get_id().ok().as_deref() == Some(id.as_str()) { found = Some(d); break; }
                }
            }
            found.ok_or_else(|| format!("output device not found: {id}"))?
        }
        None => enumerator.get_default_device(&Direction::Render).map_err(|e| e.to_string())?,
    };

    let mut audio_client = device.get_iaudioclient().map_err(|e| e.to_string())?;
    // Ask WASAPI to autoconvert whatever the device's native format is to
    // 48 kHz / 2ch / f32 (validated against a 96 kHz/8ch device in the probe).
    let desired = WaveFormat::new(32, 32, &SampleType::Float, OPUS_SAMPLE_RATE as usize, 2, None);
    let (_def, min_time) = audio_client.get_device_period().map_err(|e| e.to_string())?;
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: min_time };
    audio_client.initialize_client(&desired, &Direction::Capture, &mode).map_err(|e| e.to_string())?;

    let capture_client = audio_client.get_audiocaptureclient().map_err(|e| e.to_string())?;
    let h_event = audio_client.set_get_eventhandle().map_err(|e| e.to_string())?;
    audio_client.start_stream().map_err(|e| e.to_string())?;

    let mut bytes: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
    let mut mono: Vec<f32> = Vec::with_capacity(OPUS_FRAME_SAMPLES_MONO);
    while !stop.load(Ordering::Relaxed) {
        capture_client.read_from_device_to_deque(&mut bytes).map_err(|e| e.to_string())?;
        // Interpret as interleaved stereo f32; downmix L/R -> mono.
        while bytes.len() >= 8 {
            let mut l = [0u8; 4];
            let mut r = [0u8; 4];
            for b in l.iter_mut() { *b = bytes.pop_front().unwrap(); }
            for b in r.iter_mut() { *b = bytes.pop_front().unwrap(); }
            let s = (f32::from_le_bytes(l) + f32::from_le_bytes(r)) * 0.5;
            mono.push(s);
            if mono.len() == OPUS_FRAME_SAMPLES_MONO {
                if tx.send(PcmChunk { samples: std::mem::take(&mut mono), timestamp_ms: 0 }).is_err() {
                    return Ok(()); // receiver gone
                }
                mono = Vec::with_capacity(OPUS_FRAME_SAMPLES_MONO);
            }
        }
        let _ = h_event.wait_for_event(200_000); // ~20ms
    }
    let _ = audio_client.stop_stream();
    Ok(())
}
```

- [ ] **Step 5: Declare the modules** in the client crate root (`client/src-tauri/src/main.rs` or `lib.rs` — find where `mod display;`/`mod screenshare;` are declared) and add:
```rust
mod screen_audio;
#[cfg(windows)]
mod screen_audio_wasapi;
```

- [ ] **Step 6: Build + test (Linux exercises seam + mock):**
```
cd client/src-tauri && cargo build && cargo test screen_audio:: -- --test-threads=1 2>&1 | grep -E "test result|error\["
```
Expected: clean build (the `cfg(windows)` file is not compiled on Linux), both seam tests pass via the mock.

- [ ] **Step 7: Commit:**
```bash
git add client/src-tauri/Cargo.toml client/src-tauri/src/screen_audio.rs client/src-tauri/src/screen_audio_wasapi.rs client/src-tauri/src/main.rs
git commit -m "client: WASAPI screen-audio loopback capture (cfg-windows) + mock seam"
```

---

### Task 4: Screen-audio send loop (`voice/send_screen_audio.rs`)

**Files:** Create `client/src-tauri/src/voice/send_screen_audio.rs`, modify `client/src-tauri/src/voice/mod.rs` (add `pub mod send_screen_audio;`)

A trimmed copy of the mic send path: PcmChunk → Opus → seal (audio) → fragment under `TrackKind::ScreenAudio`. No APM/gate/mute/speaking — game audio is always sent while sharing.

- [ ] **Step 1: Write the failing test.** Create `client/src-tauri/src/voice/send_screen_audio.rs`:
```rust
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
```

- [ ] **Step 2: Declare the module.** In `client/src-tauri/src/voice/mod.rs`, near the other `pub mod` declarations (`pub mod send_video;`):
```rust
pub mod send_screen_audio;
```

- [ ] **Step 3: Build + test:**
```
cd client/src-tauri && cargo test send_screen_audio:: -- --test-threads=1 2>&1 | grep -E "test result|error\["
```
Expected: PASS (4 datagrams, routed as ScreenAudio).

- [ ] **Step 4: Commit:**
```bash
git add client/src-tauri/src/voice/send_screen_audio.rs client/src-tauri/src/voice/mod.rs
git commit -m "client: screen-audio send loop (Opus -> audio-seal -> ScreenAudio fragments)"
```

---

### Task 5: Controller — start/stop screen audio inside the C2 share lifecycle

**Files:** Modify `client/src-tauri/src/voice/mod.rs`

Extend `start_screen_share`/`stop_screen_share`/`on_peer_stream_joined`/`leave()` so the share also runs screen audio under `TrackKind::ScreenAudio`, with its own stream key + selected output device.

- [ ] **Step 1: Generalize the key-offer helper.** The C2 `offer_video_key(server, channel_id, &key)` hardcodes `TrackKind::Video`. Add a `kind` parameter (and update the two C2 call sites to pass `TrackKind::Video`):
```rust
/// Wrap `key` for every current channel member (except self) and offer it for `kind`.
async fn offer_track_key(server: &Arc<dyn ServerSession>, channel_id: u64, kind: TrackKind, key: &[u8; 32]) -> Result<(), String> {
    let participants = server.get_media_state(channel_id).await?;
    let keypair = server.my_keypair();
    let my_sk = *keypair.signing_key_bytes();
    let my_pk = *keypair.public_key().as_bytes();
    let wrapped: Vec<(PublicKey, Vec<u8>)> = participants
        .iter()
        .filter(|m| m.public_key.as_bytes() != &my_pk)
        .filter_map(|m| {
            match farder_crypto::media::wrap_stream_key_for_peer(key, &my_sk, m.public_key.as_bytes()) {
                Ok(w) => Some((m.public_key.clone(), w)),
                Err(e) => { eprintln!("[voice] failed to wrap key for a peer: {e}"); None }
            }
        })
        .collect();
    if !wrapped.is_empty() {
        server.offer_stream_key(kind, wrapped).await?;
    }
    Ok(())
}
```
Rename the old `offer_video_key(server, ch, key)` calls to `offer_track_key(server, ch, TrackKind::Video, key)` (in `start_screen_share` and `on_peer_stream_joined`). Delete the now-unused `offer_video_key` (or keep it as a thin wrapper — DRY: delete it and update callers).

- [ ] **Step 2: Extend `VideoShareState` with the screen-audio handles.** Add fields (next to the existing `stop`/`force_keyframe`/`backend`/`video_key`/`thread`):
```rust
    /// Screen-audio stream key (for late-joiner re-offer). None if audio capture failed to start.
    screen_audio_key: Option<[u8; 32]>,
    /// Stop flag for the screen-audio send loop.
    screen_audio_stop: Arc<AtomicBool>,
    /// The running WASAPI loopback capture (dropping it ends the capture thread).
    #[allow(dead_code)]
    screen_audio_capture: Option<Box<dyn crate::screen_audio::ScreenAudioCapture>>,
```

- [ ] **Step 3: Start screen audio in `start_screen_share`.** Change the signature to accept the device choice:
```rust
    pub async fn start_screen_share(&self, fps: u32, max_width: u32, max_height: u32, audio_device_id: Option<String>) -> Result<(), String> {
```
After the video capture/send thread is spawned and BEFORE `enable_track(Video)` (so both keys are offered together), add the screen-audio setup. The capture+send run on threads driving the same call `send_datagram` sink:
```rust
        // --- Screen audio (best-effort: a capture failure must not abort the
        // video share; we just log and continue without game audio). ---
        let screen_audio_stop = Arc::new(AtomicBool::new(false));
        let mut screen_audio_key: Option<[u8; 32]> = None;
        let mut screen_audio_capture: Option<Box<dyn crate::screen_audio::ScreenAudioCapture>> = None;
        match crate::screen_audio::start_capture(audio_device_id) {
            Ok((cap, pcm_rx)) => {
                let sa_key = farder_crypto::media::derive_stream_key();
                if let Err(e) = offer_track_key(&server, channel_id, TrackKind::ScreenAudio, &sa_key).await {
                    eprintln!("[voice] offer screen-audio key failed: {e}");
                }
                let sa_stop_t = screen_audio_stop.clone();
                let server_sa = server.clone();
                std::thread::spawn(move || {
                    crate::voice::send_screen_audio::run(
                        crate::voice::send_screen_audio::ScreenAudioSendConfig {
                            pcm_rx,
                            session_id: my_session_id,
                            stream_key: sa_key,
                            speaker_pk: my_pk_bytes,
                            datagram_sink: Box::new(move |b| { let _ = server_sa.send_datagram(b); }),
                        },
                        sa_stop_t,
                    );
                });
                screen_audio_key = Some(sa_key);
                screen_audio_capture = Some(cap);
                let _ = server.enable_track(TrackKind::ScreenAudio).await;
            }
            Err(e) => eprintln!("[voice] screen-audio capture failed to start (sharing video only): {e}"),
        }
```
Then store the three new fields when constructing `VideoShareState { ..., screen_audio_key, screen_audio_stop, screen_audio_capture }`. On the early-return/lost-race teardown paths added in C2, ALSO set `screen_audio_stop` and disable the ScreenAudio track:
```rust
            // (in every teardown/bail path that stops the just-built share:)
            screen_audio_stop.store(true, Ordering::Relaxed);
            let _ = server.disable_track(TrackKind::ScreenAudio).await; // best-effort
```

- [ ] **Step 4: Tear down screen audio.** Factor the C2 `shutdown_video_share(VideoShareState)` to also stop screen audio:
```rust
fn shutdown_video_share(s: VideoShareState) {
    s.stop.store(true, Ordering::Relaxed);
    let _ = s.backend.stop_capture();
    s.screen_audio_stop.store(true, Ordering::Relaxed);
    if let Some(cap) = s.screen_audio_capture.as_ref() { cap.stop(); }
    // (s.screen_audio_capture drops here, ending the capture thread.)
}
```
In `stop_screen_share`, after stopping the video pieces, also `let _ = server.disable_track(TrackKind::ScreenAudio).await;` alongside the existing `disable_track(Video)`.

- [ ] **Step 5: Late-joiner re-offer + re-enable for screen audio.** In `on_peer_stream_joined`, where C2 re-offers the video key + re-enables Video, also re-offer + re-enable ScreenAudio when present. Extend the captured tuple to include the screen-audio key, and after the video re-offer/re-enable:
```rust
        // (reoffer tuple now also carries Option<[u8;32]> screen_audio_key)
        if let Some(sa_key) = screen_audio_key {
            if let Err(e) = offer_track_key(&server, channel_id, TrackKind::ScreenAudio, &sa_key).await {
                eprintln!("[voice] re-offer screen-audio key on join failed: {e}");
            }
            if let Err(e) = server.enable_track(TrackKind::ScreenAudio).await {
                eprintln!("[voice] re-enable ScreenAudio on join failed: {e}");
            }
        }
```
(Capture `s.screen_audio_key` under the lock alongside `s.video_key`.)

- [ ] **Step 6: Add a controller test** using the FakeServerSession (extended in C2 to record `offered_kinds`/`enabled_kinds`/`disabled_kinds`). With `FARDER_DISPLAY_BACKEND=mock` (and the non-Windows mock screen-audio capture), assert that `start_screen_share(15,320,240,None)` records an `offer_stream_key(ScreenAudio)` AND `enable_track(ScreenAudio)`, and `stop_screen_share` records `disable_track(ScreenAudio)`:
```rust
    #[tokio::test]
    async fn start_screen_share_also_offers_and_enables_screen_audio() {
        std::env::set_var("FARDER_DISPLAY_BACKEND", "mock");
        // (build controller + FakeServerSession with 1 member, join — mirror the
        //  C2 test start_then_stop_screen_share_offers_enables_and_disables_video)
        // ctrl.start_screen_share(15, 320, 240, None).await.unwrap();
        assert!(fake.enabled_kinds.lock().unwrap().contains(&TrackKind::ScreenAudio));
        assert!(fake.offered_kinds.lock().unwrap().contains(&TrackKind::ScreenAudio));
        // ctrl.stop_screen_share().await.unwrap();
        assert!(fake.disabled_kinds.lock().unwrap().contains(&TrackKind::ScreenAudio));
        std::env::remove_var("FARDER_DISPLAY_BACKEND");
    }
```
Fill in the controller/fake construction by copying the exact shape of the C2 test `start_then_stop_screen_share_offers_enables_and_disables_video` in the same file. If the screen-audio mock capture thread makes the test flaky, assert the narrower observable (offer+enable recorded) and rely on the send-loop unit test.

- [ ] **Step 7: Build + test:**
```
cd client/src-tauri && cargo build && cargo test voice:: -- --test-threads=1 2>&1 | grep -E "test result|error\["
```
Expected: clean + green (audio/video paths unchanged; ScreenAudio offer/enable/disable recorded).

- [ ] **Step 8: Commit:**
```bash
git add client/src-tauri/src/voice/mod.rs
git commit -m "client: start/stop screen audio in the share lifecycle (own key + device + late re-offer)"
```

---

### Task 6: Viewer — receive ScreenAudio + mix at an independent gain

**Files:** Modify `client/src-tauri/src/voice/mod.rs`, possibly `client/src-tauri/src/voice/mixer.rs`

The viewer must decode a peer's ScreenAudio and mix it into output with a gain **separate** from that peer's voice gain (so game audio has its own volume). The mixer iterates `peer_rings` keyed by `SessionId`; screen audio from the same session needs a distinct ring. Add a parallel `screen_audio_rings` map that the mixer also sums.

- [ ] **Step 1: Give the mixer a second ring map.** In `mixer.rs`, find the mix loop that iterates `peer_rings`. Add a second map of the same value type for screen audio and sum it in the SAME per-frame loop (each ring contributes `pop_frame() * gain`). Concretely, change the mixer config to hold both `peer_rings` and `screen_audio_rings: PeerRings` (the existing `PeerRings` type alias), and in `mix_one_frame` iterate both maps identically before the soft-clip. (If `mix_one_frame` takes `peer_rings` as an argument, give it a second arg and call `.pop_frame()*gain` for each; if it reads from cfg, add the field.) Keep the soft-clip on the combined sum. Add a test asserting two rings (one "voice", one "screen") with different gains both contribute:
```rust
    #[test]
    fn screen_audio_ring_mixes_independently_of_voice_ring() {
        // Build a voice ring (gain 1.0) and a screen-audio ring (gain 0.5),
        // push a known frame into each, mix one frame, assert the output equals
        // voice + 0.5*screen (pre-clip, with small values to avoid the clip).
        // (Adapt to mixer.rs's actual mix_one_frame signature + PeerPcmRing API.)
    }
```
Write the assertion against the real `mix_one_frame`/`PeerPcmRing::push_frame`/`pop_frame` API in the file (use small sample values like 0.1 / 0.2 so `x/(1+|x|)` clipping is negligible, or compute the expected post-clip value).

- [ ] **Step 2: Thread the screen-audio ring map onto `ActiveCall`.** Add `screen_audio_rings: crate::voice::mixer::PeerRings` next to `peer_rings`, construct it `Default::default()` at the `ActiveCall { ... }` build, and pass it into the mixer spawn alongside `peer_rings` (find where `peer_rings` is handed to `mixer::run`/`PipelineParams` and add the second map).

- [ ] **Step 3: Dispatch ScreenAudio on the viewer.** In `on_peer_track_enabled` (mod.rs ~927), add the arm:
```rust
        match kind {
            TrackKind::Audio => self.on_peer_audio_track_enabled(session_id, peer_pubkey).await,
            TrackKind::Video => self.on_peer_video_track_enabled(session_id, peer_pubkey).await,
            TrackKind::ScreenAudio => self.on_peer_screen_audio_track_enabled(session_id, peer_pubkey).await,
        }
```

- [ ] **Step 4: Implement `on_peer_screen_audio_track_enabled`.** Model it on `on_peer_audio_track_enabled` but: look up the key at `(session_id, TrackKind::ScreenAudio)`; register the dispatcher route for `TrackKind::ScreenAudio`; create a ring + a FRESH independent gain (default 1.0 — the slider is Phase E) and insert it into `screen_audio_rings` (NOT `peer_rings`); spawn `recv::run` (the same audio recv task — it reassembles, `open_audio_wire_frame`s, Opus-decodes into the ring); track the recv handle in a new `screen_audio_peers: HashMap<SessionId, VideoPeerEntry-like>` so teardown can abort it. Do NOT add a UI `VoicePeer` for screen audio (it's not a participant). Mirror the audio handler's structure exactly minus the peer_status/VoicePeer/peer_volumes parts:
```rust
    async fn on_peer_screen_audio_track_enabled(&self, session_id: SessionId, _peer_pubkey: PublicKey) {
        let mut inner = self.inner.lock().await;
        let deafened_flag = inner.deafened.clone();
        let call = match inner.active.as_mut() { Some(c) => c, None => return };
        if call.screen_audio_peers.contains_key(&session_id) { return; }
        let stream_key = match call.peer_keys.get(&(session_id, TrackKind::ScreenAudio)) {
            Some(k) => *k,
            None => { eprintln!("[voice] ScreenAudio TrackEnabled with no key offer"); return; }
        };
        let ring = Arc::new(PeerPcmRing::new(10));
        let gain = Arc::new(std::sync::atomic::AtomicU32::new(1.0f32.to_bits()));
        call.screen_audio_rings.lock().expect("rings poisoned").insert(session_id, (ring.clone(), gain));
        let (tx, rx) = mpsc::unbounded_channel::<Bytes>();
        let dispatcher = call.server.dispatcher();
        let tx_reg = tx.clone();
        tokio::spawn(async move { dispatcher.register(session_id, TrackKind::ScreenAudio, tx_reg).await; });
        let recv_handle = tokio::spawn(async move {
            crate::voice::recv::run(crate::voice::recv::RecvTaskConfig {
                session_id, stream_key, deafened: deafened_flag, datagram_rx: rx, pcm_ring: ring,
            }).await;
        });
        call.screen_audio_peers.insert(session_id, ScreenAudioPeerEntry { recv_handle, datagram_tx: tx });
    }
```
Add the `screen_audio_peers: HashMap<SessionId, ScreenAudioPeerEntry>` field to `ActiveCall` (+ `struct ScreenAudioPeerEntry { recv_handle: tokio::task::JoinHandle<()>, #[allow(dead_code)] datagram_tx: mpsc::UnboundedSender<Bytes> }`), construct it empty.

- [ ] **Step 5: Tear down ScreenAudio.** In `on_peer_track_disabled`, add a `ScreenAudio` arm that aborts the recv handle, unregisters the `(session, ScreenAudio)` route, and removes the `screen_audio_rings` entry. In `leave()`, abort all `screen_audio_peers` handles + unregister their routes (mirror the audio/video teardown loop).

- [ ] **Step 6: Build + test:**
```
cd client/src-tauri && cargo build && cargo test voice:: -- --test-threads=1 2>&1 | grep -E "test result|error\["
```
Expected: clean + green; the mixer independence test passes.

- [ ] **Step 7: Commit:**
```bash
git add client/src-tauri/src/voice/mod.rs client/src-tauri/src/voice/mixer.rs
git commit -m "client: receive + independently mix peer ScreenAudio (own ring + gain)"
```

---

### Task 7: Commands — list output devices + device-aware start-share

**Files:** Modify `client/src-tauri/src/commands.rs`, `client/src-tauri/src/main.rs`, `client/src/lib/tauri-bridge.ts`

- [ ] **Step 1: Add `list_audio_output_devices` command.** Where voice commands live (`commands.rs`):
```rust
#[tauri::command]
pub async fn list_audio_output_devices() -> Result<Vec<crate::screen_audio::OutputDevice>, String> {
    crate::screen_audio::list_output_devices()
}
```

- [ ] **Step 2: Add `audio_device_id` to the start-share command.** Update the C2 `voice_start_screen_share` command to pass through the device:
```rust
#[tauri::command]
pub async fn voice_start_screen_share(
    voice: tauri::State<'_, std::sync::Arc<crate::voice::VoiceController>>,
    fps: u32,
    max_width: u32,
    max_height: u32,
    audio_device_id: Option<String>,
) -> Result<(), String> {
    voice.start_screen_share(fps, max_width, max_height, audio_device_id).await
}
```
(Match the exact state-extraction the C2 command uses.)

- [ ] **Step 3: Register** `list_audio_output_devices` in `generate_handler![...]` in `main.rs` (next to `voice_start_screen_share`).

- [ ] **Step 4: Bridge wrappers** in `client/src/lib/tauri-bridge.ts`:
```ts
export interface AudioOutputDevice { id: string; name: string; is_default: boolean; }
export async function listAudioOutputDevices(): Promise<AudioOutputDevice[]> {
  return invoke<AudioOutputDevice[]>("list_audio_output_devices");
}
export async function voiceStartScreenShare(fps: number, maxWidth: number, maxHeight: number, audioDeviceId: string | null): Promise<void> {
  return invoke<void>("voice_start_screen_share", { fps, maxWidth, maxHeight, audioDeviceId });
}
```
(Update the EXISTING `voiceStartScreenShare` wrapper from C2 to add the 4th arg — every caller must pass it; the C2 hook call becomes `voiceStartScreenShare(30,1280,720, deviceId)`.)

- [ ] **Step 5: Build + seam check:**
```
cd client/src-tauri && cargo build 2>&1 | tail -3
cd /home/deez/farder && grep -q "list_audio_output_devices" client/src-tauri/src/main.rs && grep -q '"list_audio_output_devices"' client/src/lib/tauri-bridge.ts && echo "SEAM OK" || echo "SEAM MISSING"
cd /home/deez/farder/client && npx tsc --noEmit && echo TSC_OK
```
Expected: clean, `SEAM OK`, `TSC_OK`.

- [ ] **Step 6: Commit:**
```bash
git add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs client/src/lib/tauri-bridge.ts
git commit -m "client: list_audio_output_devices command + device-aware start-share"
```

---

### Task 8: Frontend — output-device picker in the share flow

**Files:** Modify `client/src/hooks/useVoice.ts`, `client/src/components/VoiceControlBar.tsx`, theme CSS ×3

A minimal dropdown: when the user enables sharing, they pick which output device to capture (default = the system-default). Polished UI is Phase E; this is the functional minimum so the owner's Sonar setup is selectable.

- [ ] **Step 1: Load devices + selection state in `useVoice`.** Add to the hook:
```ts
  const [audioDevices, setAudioDevices] = useState<api.AudioOutputDevice[]>([]);
  const [audioDeviceId, setAudioDeviceId] = useState<string | null>(null);
  useEffect(() => {
    api.listAudioOutputDevices().then((d) => {
      setAudioDevices(d);
      const def = d.find((x) => x.is_default);
      setAudioDeviceId((cur) => cur ?? def?.id ?? d[0]?.id ?? null);
    }).catch(() => {});
  }, []);
```
Change `startShare` to pass the device, and expose the list + setter:
```ts
  const startShare = useCallback(async () => {
    await api.voiceStartScreenShare(30, 1280, 720, audioDeviceId);
    setIsSharing(true);
  }, [audioDeviceId]);
```
Add `audioDevices`, `audioDeviceId`, `setAudioDeviceId` to the `UseVoice` interface + returned object.

- [ ] **Step 2: Add the picker to the voice bar.** In `VoiceControlBar.tsx`, next to the Share button, render a `<select>` (only meaningful while not yet sharing; harmless if shown always). Reuse an existing select class if the codebase has one (grep `client/src/themes/*/theme.css` for an existing `select`/dropdown class such as `.connect-input` or a settings dropdown class) — prefer reuse:
```tsx
        <select
          className="vcb-audio-source"
          title="Game-audio source (output device to capture)"
          value={voice.audioDeviceId ?? ""}
          onChange={(e) => voice.setAudioDeviceId(e.target.value)}
          disabled={voice.isSharing}
        >
          {voice.audioDevices.map((d) => (
            <option key={d.id} value={d.id}>{d.name}{d.is_default ? " (default)" : ""}</option>
          ))}
        </select>
```

- [ ] **Step 3: Theme CSS.** If you used the new class `.vcb-audio-source`, add it to ALL THREE `client/src/themes/*/theme.css`, styled with theme vars (`background: var(--xp-panel-bg)`, `color: var(--xp-text-normal)`, `border: 1px solid var(--xp-border)`, small font, max-width ~160px with ellipsis). If you reused an existing select class, skip this step. Confirm:
```
grep -l "vcb-audio-source" client/src/themes/*/theme.css   # 3 files (only if you added the class)
```

- [ ] **Step 4: Type-check:**
```
cd /home/deez/farder/client && npx tsc --noEmit && echo TSC_OK
```

- [ ] **Step 5: Commit:**
```bash
git add client/src/hooks/useVoice.ts client/src/components/VoiceControlBar.tsx client/src/themes/*/theme.css
git commit -m "client ui: screen-audio output-device picker in the voice bar"
```

---

### Task 9: Docs + verification gate

**Files:** Modify `docs/modules/voice-video-transport.md`, `docs/modules/tauri-commands.md`, `ARCHITECTURE.md`; delete `phaseD-probe/`

- [ ] **Step 1: Docs.** In `voice-video-transport.md` add a "Phase D — screen/game audio" section: `TrackKind::ScreenAudio` (own outer byte `0x03`, inner frame reuses the audio seal, audio bandwidth cap); WASAPI loopback capture (`screen_audio.rs` seam + `screen_audio_wasapi.rs`, autoconvert→48k→mono, device-selectable, virtual-endpoint wedge caveat); the `send_screen_audio` loop; the controller integration (own key, `enable_track(ScreenAudio)`, late-joiner re-offer+re-enable, teardown in `stop`/`leave`); the viewer path (`on_peer_screen_audio_track_enabled` → separate mixer ring with independent gain). `tauri-commands.md`: `list_audio_output_devices` (returns `[{id,name,is_default}]`) + the new `audioDeviceId` param on `voice_start_screen_share` + the `voiceStartScreenShare`/`listAudioOutputDevices` bridge fns. `ARCHITECTURE.md`: one line — game audio now flows as a third media track (`ScreenAudio`) over the same E2EE/datagram path, mixed at its own volume; the volume slider + polished UI are Phase E.

- [ ] **Step 2: Full gate:**
```bash
cd /home/deez/farder && cargo test --workspace 2>&1 | grep -E "^test result" | tail -25
cd /home/deez/farder/client/src-tauri && cargo build && cargo test -- --test-threads=1 2>&1 | grep -E "^test result"
cd /home/deez/farder/client && npx tsc --noEmit && echo TSC_OK
cd /home/deez/farder && grep -q "list_audio_output_devices" client/src-tauri/src/main.rs && echo "SEAM OK" || echo "SEAM MISSING"
```
All green (client single-threaded for the FARDER_DATA race; the `mock_capture_emits_frames_at_expected_fps` timing flake re-runs alone). If any voice/media/screen_audio test fails for a real reason, STOP and report.

- [ ] **Step 3: Delete the throwaway probe** (its job is done — Phase D locks `wasapi 0.23`):
```bash
cd /home/deez/farder && git rm -r phaseD-probe && git commit -m "chore: remove phaseD-probe (WASAPI loopback validated, folded into Phase D)"
```

- [ ] **Step 4: Commit docs:**
```bash
git add docs/ ARCHITECTURE.md
git commit -m "docs: screen/game audio (Phase D)"
```

- [ ] **Step 5: Owner two-client runtime verification (report, not code).** UNVERIFIED until the owner's Windows run (CLAUDE.md). The WASAPI capture (`screen_audio_wasapi.rs`) is `cfg(windows)` and never compiled/run on Linux — the owner's build is the first real exercise. Steps: rebuild BOTH clients (and the server sidecar — the protocol enum changed, so old/new peers can't exchange media; both sides must be rebuilt). With two clients in the same voice channel: client A picks an output device in the share dropdown (IMPORTANT for A's Sonar setup — the *physical* "Speakers (USB Audio Device)", not the silent default) and clicks Share while a game/video plays; client B should HEAR A's game audio, mixed with (and independently of) A's voice. Test: B joins AFTER A started sharing → B still gets the ScreenAudio key + track (late re-offer/re-enable) and hears the game. Confirm Stop ends the game audio. Repeat over a DIRECT server. This run is the whole Phase D deliverable.

---

## Self-review notes (done at plan time)

- **Spec coverage (Phase D):** WASAPI loopback capture (Task 3) → existing Opus path as the ScreenAudio track (Task 4 send, reusing `seal_audio_packet_to_wire`); new `TrackKind::ScreenAudio` with its own routing byte + audio cap (Tasks 1–2); independent decode/mix with its own gain (Task 6, separate mixer ring); share-lifecycle integration incl. late-joiner re-offer (Task 5, mirrors C2's video). Added beyond the bare spec: the output-device picker (Tasks 7–8), justified by the probe finding that the owner's Windows default is a silent virtual Sonar device — without it the feature is un-demoable. The volume *slider*, LIVE badge, viewer pane, and OS *video*-source picker remain Phase E.
- **Type consistency:** `TrackKind::ScreenAudio` everywhere; outer byte `MEDIA_FRAME_TYPE_SCREEN_AUDIO=0x03`; inner frame reuses `seal_audio_packet_to_wire`/`open_audio_wire_frame` (inner `0x01`); `start_screen_share(fps,max_width,max_height,audio_device_id)` ↔ `voice_start_screen_share(...,audio_device_id)` ↔ `voiceStartScreenShare(30,1280,720,deviceId)`; `offer_track_key(server,channel_id,kind,key)` generalizes the C2 `offer_video_key`; `ScreenAudioSendConfig{pcm_rx,session_id,stream_key,speaker_pk,datagram_sink}`; `OutputDevice{id,name,is_default}` ↔ TS `AudioOutputDevice{id,name,is_default}`; separate `screen_audio_rings` mixer map + `screen_audio_peers` recv map keyed by `SessionId`.
- **Proven-path risk:** Tasks 1–2 add an enum variant → ~11 exhaustive match arms (compiler-guided; ScreenAudio mirrors Audio's cap/inner-byte). Task 5 only ADDS to `start/stop_screen_share`/`on_peer_stream_joined`/`leave` (the mic-audio and C2-video paths are unchanged; screen audio is best-effort so a capture failure can't break the video share). Task 6 adds a SECOND mixer ring map + a new dispatch arm — the existing `peer_rings` voice mixing is untouched. The full voice suite is the regression gate after Tasks 2, 5, 6.
- **Testability split:** headless (Linux) — the protocol byte round-trip (Task 1), server cap/activity (Task 2), the screen-audio seam+mock + send loop (Tasks 3–4), the controller offer/enable/disable + mixer independence (Tasks 5–6) all run via the mock screen-audio capture + FakeServerSession. Owner-runtime (Windows) — the real `screen_audio_wasapi.rs` loopback (cfg-gated, never built on Linux) and the two-client hear-the-game-audio test (Task 9). The `wasapi 0.23` API is locked by the `phaseD-probe` run.
- **Known judgment calls:** screen audio is best-effort within the share (a capture failure logs + shares video only, rather than failing the whole Share); the viewer mixes ScreenAudio at a fixed gain 1.0 (the per-source volume slider is Phase E — the independent ring is already in place for it); the device picker is a bare `<select>` (styling/placement polished in Phase E); the mixer's second ring map vs. re-keying `peer_rings` by `(SessionId,TrackKind)` — chose the second map to leave the proven voice mixing path byte-for-byte unchanged.
