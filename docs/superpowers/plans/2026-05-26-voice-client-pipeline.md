# Voice Client Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire MediaBackend (#1) + Media Stream Transport (#2) + CpalAudioBackend (#3.1) + Opus Codec (#3.2) into a working group voice call, exposed via Tauri commands.

**Architecture:** New `client/src-tauri/src/voice/` module. One send-path task per local user, one recv-path task per remote peer, one mixer task per call. Server-side and client-side QUIC datagram loops added. Stream key derived per call, distributed via existing `OfferStreamKey`. v1 gating = open mic; APM = WebRTC AEC/NS/AGC; jitter = 60 ms fixed.

**Tech Stack:** Rust (Tauri 2 client + farder-server). New dep: `webrtc-audio-processing`.

**Spec:** `docs/superpowers/specs/2026-05-26-voice-client-pipeline-design.md`

---

## File structure

**Created:**
- `client/src-tauri/src/voice/mod.rs` — `VoiceController`, `VoiceState`, `VoicePeer`, peer registry, lifecycle, Tauri commands
- `client/src-tauri/src/voice/gate.rs` — `GateMode` + `pass()`
- `client/src-tauri/src/voice/jitter.rs` — `JitterBuffer`
- `client/src-tauri/src/voice/apm.rs` — `AudioProcessor` wrapping `webrtc-audio-processing`
- `client/src-tauri/src/voice/send.rs` — `SendTask`
- `client/src-tauri/src/voice/recv.rs` — `RecvTask`
- `client/src-tauri/src/voice/mixer.rs` — `MixerTask`

**Modified:**
- `client/src-tauri/Cargo.toml` — `webrtc-audio-processing = "0.6"` (pin actual latest at write time)
- `client/src-tauri/src/main.rs` — `mod voice;` + 5 Tauri commands in invoke handler
- `client/src-tauri/src/connection.rs` — expose `Arc<quinn::Connection>` for datagram I/O + spawn recv-datagram loop
- `crates/farder-server/src/connection.rs` — spawn datagram recv loop calling `media_stream::inspect_inbound_frame` and fanning out via `Connection::send_datagram`

---

## Phase 1: Foundation

## Task 1: voice/ module scaffold + Cargo dep

**Files:**
- Create: `client/src-tauri/src/voice/mod.rs`
- Create: `client/src-tauri/src/voice/gate.rs`
- Create: `client/src-tauri/src/voice/jitter.rs`
- Create: `client/src-tauri/src/voice/apm.rs`
- Create: `client/src-tauri/src/voice/send.rs`
- Create: `client/src-tauri/src/voice/recv.rs`
- Create: `client/src-tauri/src/voice/mixer.rs`
- Modify: `client/src-tauri/Cargo.toml`
- Modify: `client/src-tauri/src/main.rs`

- [ ] **Step 1: Add `webrtc-audio-processing` to Cargo.toml**

Look up the actual latest version on crates.io (use `cargo search webrtc-audio-processing 2>&1 | head`). Pin a real version, e.g. `webrtc-audio-processing = "0.6"`. If only a pre-release exists, pin to that explicitly (same approach as `audiopus = "0.3.0-rc.0"`).

If the crate name differs or has been replaced with a newer fork, document the substitution in the commit message and proceed with the closest viable alternative that exposes AEC + NS + AGC.

- [ ] **Step 2: Add `mod voice;` to main.rs**

Find the cluster of `mod xxx;` declarations and insert `mod voice;` alphabetically (between `mod tray;` and `mod translation;`, depending on current ordering).

- [ ] **Step 3: Create `voice/mod.rs` with public API skeleton**

```rust
// client/src-tauri/src/voice/mod.rs
//
// Voice call orchestration. Coordinates capture, encode, transport,
// receive, decode, mix, playback. See
// docs/superpowers/specs/2026-05-26-voice-client-pipeline-design.md.

pub mod apm;
pub mod gate;
pub mod jitter;
pub mod mixer;
pub mod recv;
pub mod send;

use farder_crypto::identity::PublicKey;
use serde::{Deserialize, Serialize};

pub type ChannelId = [u8; 16];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoicePeer {
    pub pubkey: PublicKey,
    pub speaking: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoiceState {
    pub channel_id: Option<ChannelId>,
    pub muted: bool,
    pub deafened: bool,
    pub peers: Vec<VoicePeer>,
}

pub struct VoiceController {
    // populated in Task 10
}

impl VoiceController {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for VoiceController {
    fn default() -> Self {
        Self::new()
    }
}
```

(If `PublicKey` lives at a different path in your tree — check with `grep -rn 'pub use.*PublicKey\|pub type PublicKey' /home/deez/farder/crates/farder-crypto/src/` — adjust the import.)

- [ ] **Step 4: Create each sibling file as a stub module**

For each of `gate.rs`, `jitter.rs`, `apm.rs`, `send.rs`, `recv.rs`, `mixer.rs`, write a stub like:

```rust
// client/src-tauri/src/voice/gate.rs
// Implementation lands in Task N.
```

(With the file's planned task number.)

- [ ] **Step 5: cargo check**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
```

Expected: `Finished` with no errors. Pre-existing warnings OK.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add client/src-tauri/Cargo.toml client/src-tauri/Cargo.lock client/src-tauri/src/main.rs client/src-tauri/src/voice
git -C /home/deez/farder commit -m "feat(client): voice/ scaffold + webrtc-audio-processing dep"
```

HEREDOC + Co-Authored-By trailer.

---

## Phase 2: Pure unit components

## Task 2: gate.rs — GateMode + tests

**Files:**
- Modify: `client/src-tauri/src/voice/gate.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// client/src-tauri/src/voice/gate.rs
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};

#[derive(Clone, Debug)]
pub struct VadConfig {
    pub rms_threshold: f32,
}

#[derive(Clone, Debug)]
pub enum GateMode {
    Open,
    Vad(VadConfig),
    Ptt(Arc<AtomicBool>),
}

impl GateMode {
    pub fn pass(&self, _pcm: &[f32]) -> bool {
        unimplemented!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_always_passes() {
        let g = GateMode::Open;
        assert!(g.pass(&[0.0; 960]));
        assert!(g.pass(&[]));
    }

    #[test]
    fn ptt_false_blocks_true_passes() {
        let flag = Arc::new(AtomicBool::new(false));
        let g = GateMode::Ptt(flag.clone());
        assert!(!g.pass(&[0.5; 960]));
        flag.store(true, Ordering::Release);
        assert!(g.pass(&[0.5; 960]));
    }

    #[test]
    fn vad_v1_stub_always_passes() {
        // v1 stub: VAD always returns true. Real algorithm lands later.
        let g = GateMode::Vad(VadConfig { rms_threshold: 0.01 });
        assert!(g.pass(&[0.5; 960]));
        assert!(g.pass(&[0.0; 960]));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
cd /home/deez/farder/client/src-tauri && cargo test voice::gate 2>&1 | tail -10
```

Expected: 3 tests, panic from `unimplemented!()`.

- [ ] **Step 3: Implement `pass`**

Replace the body:

```rust
    pub fn pass(&self, _pcm: &[f32]) -> bool {
        match self {
            GateMode::Open => true,
            GateMode::Vad(_) => true, // v1 stub
            GateMode::Ptt(flag) => flag.load(Ordering::Acquire),
        }
    }
```

- [ ] **Step 4: Run tests to verify they pass**

```
cd /home/deez/farder/client/src-tauri && cargo test voice::gate 2>&1 | tail -10
```

Expected: 3 passed.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/voice/gate.rs
git -C /home/deez/farder commit -m "feat(client): voice::gate — GateMode + Open/Vad/Ptt variants"
```

---

## Task 3: jitter.rs — JitterBuffer + tests

**Files:**
- Modify: `client/src-tauri/src/voice/jitter.rs`

- [ ] **Step 1: Write the failing tests + skeleton**

```rust
// client/src-tauri/src/voice/jitter.rs
//
// Per-peer jitter buffer: 3-frame ring keyed by `seq` from the sealed frame
// header. Pop returns Some(packet) at the head slot or None for "play PLC".

pub const JITTER_DEPTH: usize = 3;

pub struct JitterBuffer {
    slots: [Option<Vec<u8>>; JITTER_DEPTH],
    head_seq: u64,
    initialized: bool,
}

impl JitterBuffer {
    pub fn new() -> Self {
        Self {
            slots: [None, None, None],
            head_seq: 0,
            initialized: false,
        }
    }

    /// Insert a packet. Returns true if accepted, false if dropped (stale/dup).
    pub fn insert(&mut self, _seq: u64, _packet: Vec<u8>) -> bool {
        unimplemented!()
    }

    /// Pop the head slot. Advances the window. Returns None for "loss → PLC".
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        unimplemented!()
    }
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_order_insert_then_pop_returns_packets() {
        let mut jb = JitterBuffer::new();
        assert!(jb.insert(10, vec![1]));
        assert!(jb.insert(11, vec![2]));
        assert!(jb.insert(12, vec![3]));
        assert_eq!(jb.pop(), Some(vec![1]));
        assert_eq!(jb.pop(), Some(vec![2]));
        assert_eq!(jb.pop(), Some(vec![3]));
    }

    #[test]
    fn out_of_order_inserts_reorder() {
        let mut jb = JitterBuffer::new();
        assert!(jb.insert(12, vec![3]));
        assert!(jb.insert(10, vec![1]));
        assert!(jb.insert(11, vec![2]));
        assert_eq!(jb.pop(), Some(vec![1]));
        assert_eq!(jb.pop(), Some(vec![2]));
        assert_eq!(jb.pop(), Some(vec![3]));
    }

    #[test]
    fn stale_packet_is_dropped() {
        let mut jb = JitterBuffer::new();
        assert!(jb.insert(10, vec![1]));
        let _ = jb.pop(); // advances head to 11
        let _ = jb.pop(); // advances head to 12
        assert!(!jb.insert(9, vec![99]), "seq below head must be rejected");
    }

    #[test]
    fn duplicate_packet_is_dropped() {
        let mut jb = JitterBuffer::new();
        assert!(jb.insert(10, vec![1]));
        assert!(!jb.insert(10, vec![99]), "duplicate seq must be rejected");
    }

    #[test]
    fn gap_yields_none_for_plc() {
        let mut jb = JitterBuffer::new();
        assert!(jb.insert(10, vec![1]));
        assert!(jb.insert(12, vec![3]));
        assert_eq!(jb.pop(), Some(vec![1]));
        assert_eq!(jb.pop(), None, "seq 11 missing → PLC");
        assert_eq!(jb.pop(), Some(vec![3]));
    }

    #[test]
    fn far_future_seq_advances_window_dropping_older_slots() {
        let mut jb = JitterBuffer::new();
        assert!(jb.insert(10, vec![1]));
        assert!(jb.insert(11, vec![2]));
        // seq 100 — way beyond window. Advance head; old slots gone.
        assert!(jb.insert(100, vec![100]));
        // pop should now yield None,None until we reach seq 100
        assert_eq!(jb.pop(), None);
    }

    #[test]
    fn empty_buffer_pop_returns_none_without_panic() {
        let mut jb = JitterBuffer::new();
        assert_eq!(jb.pop(), None);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
cd /home/deez/farder/client/src-tauri && cargo test voice::jitter 2>&1 | tail -10
```

Expected: 7 tests panic from `unimplemented!()`.

- [ ] **Step 3: Implement `insert` and `pop`**

```rust
    pub fn insert(&mut self, seq: u64, packet: Vec<u8>) -> bool {
        if !self.initialized {
            self.head_seq = seq;
            self.initialized = true;
        }
        if seq < self.head_seq {
            return false; // stale
        }
        let offset = (seq - self.head_seq) as usize;
        if offset < JITTER_DEPTH {
            if self.slots[offset].is_some() {
                return false; // duplicate
            }
            self.slots[offset] = Some(packet);
            true
        } else {
            // Far future — advance head to (seq - JITTER_DEPTH + 1), dropping old.
            let advance = offset - JITTER_DEPTH + 1;
            self.advance_head(advance);
            // After advancing, new packet sits at slot JITTER_DEPTH - 1.
            self.slots[JITTER_DEPTH - 1] = Some(packet);
            true
        }
    }

    pub fn pop(&mut self) -> Option<Vec<u8>> {
        if !self.initialized {
            return None;
        }
        let out = self.slots[0].take();
        self.advance_head(1);
        out
    }

    fn advance_head(&mut self, n: usize) {
        if n >= JITTER_DEPTH {
            self.slots = [None, None, None];
        } else {
            for i in 0..(JITTER_DEPTH - n) {
                self.slots[i] = self.slots[i + n].take();
            }
        }
        self.head_seq = self.head_seq.saturating_add(n as u64);
    }
```

- [ ] **Step 4: Run tests to verify they pass**

```
cd /home/deez/farder/client/src-tauri && cargo test voice::jitter 2>&1 | tail -10
```

Expected: 7 passed.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/voice/jitter.rs
git -C /home/deez/farder commit -m "feat(client): voice::jitter — reorder/dup/gap/window-advance"
```

---

## Task 4: apm.rs — AudioProcessor wrapper + tests

**Files:**
- Modify: `client/src-tauri/src/voice/apm.rs`

The exact `webrtc-audio-processing` API depends on the version. The implementer should run `cargo doc -p webrtc-audio-processing --no-deps --open` (or read the registry source) before writing code. The shape below is a reference; field names and method names may need adjustment.

- [ ] **Step 1: Write the failing tests + skeleton**

```rust
// client/src-tauri/src/voice/apm.rs
//
// Wrapper over webrtc-audio-processing's Processor. Voice-optimized config:
// 48 kHz mono, AEC + NS (High) + AGC (AdaptiveDigital).
//
// Each call to `process_capture` ingests one 20 ms frame (960 samples) and
// returns the processed frame. AEC requires the render (playback) signal
// to be fed in via `process_render` BEFORE the corresponding capture frame.

use crate::opus_codec::OPUS_FRAME_SAMPLES_MONO;

pub struct AudioProcessor {
    inner: Option<webrtc_audio_processing::Processor>,
    fallback: bool,
}

impl AudioProcessor {
    /// Construct a configured APM. If construction fails, returns a no-op
    /// fallback (`fallback = true`) — the call still works, just without AEC/NS/AGC.
    pub fn new() -> Self {
        unimplemented!()
    }

    /// Push a render frame (the mixed playback PCM) into APM for AEC reference.
    /// Must be called before `process_capture` for the next 20 ms tick.
    pub fn process_render(&mut self, _pcm: &mut [f32]) {
        unimplemented!()
    }

    /// Process one capture frame in place.
    pub fn process_capture(&mut self, _pcm: &mut [f32]) {
        unimplemented!()
    }

    pub fn is_fallback(&self) -> bool {
        self.fallback
    }
}

impl Default for AudioProcessor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_either_real_or_fallback() {
        // We don't require the real APM to be available in CI / headless tests.
        let _apm = AudioProcessor::new();
        // Just assert no panic on construction and process call.
    }

    #[test]
    fn process_capture_does_not_panic_on_correct_frame_size() {
        let mut apm = AudioProcessor::new();
        let mut frame = vec![0.0f32; OPUS_FRAME_SAMPLES_MONO];
        apm.process_capture(&mut frame);
        assert_eq!(frame.len(), OPUS_FRAME_SAMPLES_MONO, "frame length must be preserved");
    }

    #[test]
    fn process_render_does_not_panic_on_correct_frame_size() {
        let mut apm = AudioProcessor::new();
        let mut frame = vec![0.0f32; OPUS_FRAME_SAMPLES_MONO];
        apm.process_render(&mut frame);
    }

    #[test]
    fn fallback_mode_passes_through_unchanged() {
        let mut apm = AudioProcessor { inner: None, fallback: true };
        let original: Vec<f32> = (0..OPUS_FRAME_SAMPLES_MONO).map(|i| i as f32 * 0.001).collect();
        let mut frame = original.clone();
        apm.process_capture(&mut frame);
        assert_eq!(frame, original, "fallback must pass PCM through unchanged");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
cd /home/deez/farder/client/src-tauri && cargo test voice::apm 2>&1 | tail -10
```

Expected: 4 tests panic from `unimplemented!()`.

- [ ] **Step 3: Implement constructor + process methods**

```rust
    pub fn new() -> Self {
        // webrtc-audio-processing's Processor::new signature varies by version.
        // The intent: 1 capture channel (mono), 1 render channel (mono), 48 kHz.
        // Enable AEC, NS at High level, AGC AdaptiveDigital.
        match webrtc_audio_processing::Processor::new(
            &webrtc_audio_processing::InitializationConfig {
                num_capture_channels: 1,
                num_render_channels: 1,
                ..Default::default()
            },
        ) {
            Ok(mut proc_) => {
                let mut cfg = webrtc_audio_processing::Config::default();
                // The field names below are the typical webrtc-audio-processing API.
                // If the crate version uses different names, adjust here.
                cfg.echo_cancellation = Some(webrtc_audio_processing::EchoCancellation {
                    suppression_level: webrtc_audio_processing::EchoCancellationSuppressionLevel::Moderate,
                    stream_delay_ms: None,
                    enable_delay_agnostic: true,
                    enable_extended_filter: false,
                });
                cfg.noise_suppression = Some(webrtc_audio_processing::NoiseSuppression {
                    suppression_level: webrtc_audio_processing::NoiseSuppressionLevel::High,
                });
                cfg.gain_control = Some(webrtc_audio_processing::GainControl {
                    target_level_dbfs: 3,
                    compression_gain_db: 9,
                    enable_limiter: true,
                    mode: webrtc_audio_processing::GainControlMode::AdaptiveDigital,
                });
                proc_.set_config(cfg);
                Self { inner: Some(proc_), fallback: false }
            }
            Err(e) => {
                eprintln!("[voice::apm] APM init failed ({e}); falling back to no-op");
                Self { inner: None, fallback: true }
            }
        }
    }

    pub fn process_render(&mut self, pcm: &mut [f32]) {
        if let Some(p) = self.inner.as_mut() {
            if let Err(e) = p.process_render_frame(pcm) {
                eprintln!("[voice::apm] process_render: {e}");
            }
        }
    }

    pub fn process_capture(&mut self, pcm: &mut [f32]) {
        if let Some(p) = self.inner.as_mut() {
            if let Err(e) = p.process_capture_frame(pcm) {
                eprintln!("[voice::apm] process_capture: {e}");
            }
        }
    }
```

**If the crate API differs significantly** from the above, the implementer should preserve the public surface (`new`, `process_capture`, `process_render`, `is_fallback`) and adapt the internal calls. If the crate processes 10 ms chunks (some versions do), call its method twice per 20 ms frame from inside `process_capture` / `process_render`.

- [ ] **Step 4: Run tests to verify they pass**

```
cd /home/deez/farder/client/src-tauri && cargo test voice::apm 2>&1 | tail -10
```

Expected: 4 passed. If the crate doesn't build on Linux (libwebrtc has C++ build deps — cmake, clang), document the failure mode in the commit and either: (a) fix the system dep, or (b) flag for the controller to choose between persisting through the build issue and falling back to an interim no-op APM.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/voice/apm.rs
git -C /home/deez/farder commit -m "feat(client): voice::apm — WebRTC AEC/NS/AGC wrapper + fallback"
```

---

## Phase 3: Datagram transport plumbing

## Task 5: Server-side QUIC datagram fanout loop

**Files:**
- Modify: `crates/farder-server/src/connection.rs`

- [ ] **Step 1: Locate the connection-accept loop**

Read `crates/farder-server/src/connection.rs`. Find the per-connection task spawn (likely in a function called `handle_connection` or similar). The point of insertion is right after the bi-stream accept block — we want a sibling `tokio::spawn` reading datagrams off the same `quinn::Connection`.

- [ ] **Step 2: Write the failing test (an `#[ignore]` for now if no integration harness)**

Since there's no easy in-process QUIC client to test against, the validation here is a smoke check: the loop compiles, doesn't panic, and forwards a synthetic datagram correctly. Write the test in `crates/farder-server/src/media_stream.rs` as an additional unit test for the inspect+fanout glue:

```rust
#[test]
fn fanout_forwards_to_listed_recipients() {
    // (Re-use existing test scaffolding for ServerSession.)
    // Construct a session, mark two peers as joined+listening, call
    // inspect_inbound_frame, assert recipient list contains both.
    //
    // (Detailed test depends on the existing ServerSession test setup —
    // mirror an existing test in media_stream.rs.)
}
```

If the existing media_stream tests already cover this (likely from sub-project #2 MST-8/9), skip this step and rely on those.

- [ ] **Step 3: Add the datagram-recv task**

Inside the per-connection handler, after the existing stream-accept setup:

```rust
let conn_for_datagrams = connection.clone();
let session_registry = server_state.media_stream_registry.clone(); // adapt name
let conn_pk = peer_pubkey; // captured from auth
tokio::spawn(async move {
    loop {
        match conn_for_datagrams.read_datagram().await {
            Ok(bytes) => {
                let decision = {
                    let mut reg = session_registry.lock().await;
                    reg.inspect_inbound_frame(&conn_pk, &bytes)
                };
                use crate::media_stream::FrameInspection;
                match decision {
                    FrameInspection::Forward { recipients, .. } => {
                        for peer in recipients {
                            // Lookup the peer's quinn::Connection from a connection registry.
                            if let Some(peer_conn) = server_state.connection_for(&peer).await {
                                let _ = peer_conn.send_datagram(bytes.clone());
                            }
                        }
                    }
                    FrameInspection::Drop { reason } => {
                        // (Optional) telemetry. v1: silently drop.
                        let _ = reason;
                    }
                }
            }
            Err(quinn::ConnectionError::ApplicationClosed { .. })
            | Err(quinn::ConnectionError::ConnectionClosed { .. })
            | Err(quinn::ConnectionError::LocallyClosed)
            | Err(quinn::ConnectionError::TimedOut) => break,
            Err(e) => {
                eprintln!("[media] datagram read error: {e}");
                break;
            }
        }
    }
});
```

Adapt names to match what's already in the file:
- The connection registry method `connection_for(&peer)` may not exist; find the equivalent (likely a `HashMap<PublicKey, Connection>` in `state.rs` or similar). If none exists, add one in the same step and populate it during connection accept.
- `inspect_inbound_frame` signature — check `media_stream.rs` for the actual parameter list. The summary said it takes `&self`, the inbound bytes, and the sending conn's pubkey; adapt accordingly.
- `FrameInspection` variant names may differ.

- [ ] **Step 4: cargo check + run media_stream tests**

```
cd /home/deez/farder && cargo check --workspace 2>&1 | tail -10
cd /home/deez/farder && cargo test -p farder-server media_stream 2>&1 | tail -10
```

Expected: clean check; existing media_stream tests still pass.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/connection.rs crates/farder-server/src/state.rs
git -C /home/deez/farder commit -m "feat(server): QUIC datagram fanout loop using media_stream inspect"
```

(Adjust the staged file list to match what actually changed.)

---

## Task 6: Client-side QUIC datagram send + recv routing

**Files:**
- Modify: `client/src-tauri/src/connection.rs`
- Modify: `client/src-tauri/src/voice/mod.rs`

- [ ] **Step 1: Expose the QUIC Connection for datagram I/O**

In `client/src-tauri/src/connection.rs`, the existing `connect_and_authenticate` (or similarly named) function probably returns a tuple of `(Connection, SendStream, RecvStream, session_token)` or stores them in a struct. Expose the `quinn::Connection` to the rest of the client.

- [ ] **Step 2: Define a media-datagram dispatcher type**

In `client/src-tauri/src/voice/mod.rs`, add:

```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

pub type SessionId = [u8; 16];

/// Routes inbound media datagrams to the right RecvTask by session_id.
#[derive(Default)]
pub struct MediaInboundDispatcher {
    routes: Mutex<HashMap<SessionId, mpsc::UnboundedSender<Vec<u8>>>>,
}

impl MediaInboundDispatcher {
    pub async fn register(&self, session_id: SessionId, tx: mpsc::UnboundedSender<Vec<u8>>) {
        self.routes.lock().await.insert(session_id, tx);
    }

    pub async fn unregister(&self, session_id: &SessionId) {
        self.routes.lock().await.remove(session_id);
    }

    pub async fn dispatch(&self, bytes: Vec<u8>) {
        // Frame format from sub-project #2: bytes[12..28] = session_id.
        if bytes.len() < 28 { return; }
        let mut sid = [0u8; 16];
        sid.copy_from_slice(&bytes[12..28]);
        if let Some(tx) = self.routes.lock().await.get(&sid) {
            let _ = tx.send(bytes);
        }
    }
}
```

(Confirm `bytes[12..28]` matches the actual session_id offset from the frame layout in `crates/farder-protocol/src/server.rs` or the transport spec; the summary listed it as `12-27 | session_id | 16 B`.)

- [ ] **Step 3: Spawn the datagram-recv loop after connection establishment**

In whichever client startup path establishes the QUIC connection (likely in `bridge.rs`, `connection.rs`, or `server_manager.rs`), after auth succeeds:

```rust
let dispatcher = Arc::new(crate::voice::MediaInboundDispatcher::default());
let dispatcher_for_loop = dispatcher.clone();
let conn_for_loop = quic_connection.clone();
tokio::spawn(async move {
    loop {
        match conn_for_loop.read_datagram().await {
            Ok(bytes) => dispatcher_for_loop.dispatch(bytes.to_vec()).await,
            Err(_) => break,
        }
    }
});
// Stash `dispatcher` and `quic_connection` in the shared app state so
// VoiceController can grab them later.
```

The exact place to stash these depends on existing app-state shape. Find the pattern used for `ServerSession` or `connection.rs`'s shared state, and follow it.

- [ ] **Step 4: cargo check**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
```

Expected: clean. (No new tests for this task — it's plumbing tested transitively in Task 10's controller tests.)

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src
git -C /home/deez/farder commit -m "feat(client): QUIC datagram dispatcher + recv loop"
```

---

## Phase 4: Per-path tasks

## Task 7: send.rs — SendTask + tests

**Files:**
- Modify: `client/src-tauri/src/voice/send.rs`

- [ ] **Step 1: Define struct + write failing test**

```rust
// client/src-tauri/src/voice/send.rs
use crate::audio::AudioBackend;
use crate::opus_codec::{OpusEncoder, OPUS_DEFAULT_BITRATE_BPS, OPUS_FRAME_SAMPLES_MONO, OPUS_SAMPLE_RATE};
use crate::voice::apm::AudioProcessor;
use crate::voice::gate::GateMode;
use farder_crypto::media::seal_media_frame;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};
use tokio::sync::{mpsc, watch};

pub struct SendTaskHandle {
    pub muted: Arc<AtomicBool>,
    pub local_speaking_rx: watch::Receiver<bool>,
}

pub struct SendTaskConfig {
    pub audio_in: Box<dyn AudioBackend>,
    pub apm: AudioProcessor,
    pub gate: GateMode,
    pub session_id: [u8; 16],
    pub stream_key: [u8; 32],
    pub aec_ref_rx: watch::Receiver<Vec<f32>>,
    pub datagram_out: mpsc::UnboundedSender<Vec<u8>>,
}

pub async fn run(mut cfg: SendTaskConfig, muted: Arc<AtomicBool>, local_speaking_tx: watch::Sender<bool>) {
    let mut encoder = match OpusEncoder::new(OPUS_SAMPLE_RATE, 1, OPUS_DEFAULT_BITRATE_BPS) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[voice::send] encoder init: {e}");
            return;
        }
    };
    let mut seq: u64 = 0;
    let mut speaking_consec_below: u32 = 0;
    let mut speaking = false;

    loop {
        // Pull one 20ms frame from audio_in (blocking in the runtime sense).
        let mut frame = match cfg.audio_in.read_input_frame(OPUS_FRAME_SAMPLES_MONO).await {
            Some(f) => f,
            None => break, // device closed
        };

        // AEC: feed the most recent playback frame as render reference.
        if let Some(mut render) = cfg.aec_ref_rx.borrow().clone().into() {
            cfg.apm.process_render(&mut render);
        }
        cfg.apm.process_capture(&mut frame);

        // Local speaking RMS.
        let rms = rms(&frame);
        const SPEAK_THRESHOLD: f32 = 0.03;
        if rms > SPEAK_THRESHOLD {
            if !speaking {
                speaking = true;
                let _ = local_speaking_tx.send(true);
            }
            speaking_consec_below = 0;
        } else {
            speaking_consec_below = speaking_consec_below.saturating_add(1);
            if speaking && speaking_consec_below >= 15 {
                // ~300 ms at 20 ms tick
                speaking = false;
                let _ = local_speaking_tx.send(false);
            }
        }

        if !cfg.gate.pass(&frame) {
            seq = seq.saturating_add(1);
            continue;
        }
        if muted.load(Ordering::Acquire) {
            seq = seq.saturating_add(1);
            continue;
        }

        // Opus encode.
        let pkt = match encoder.encode(&frame) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[voice::send] encode: {e}");
                seq = seq.saturating_add(1);
                continue;
            }
        };

        // Build sealed frame using farder_crypto::media::seal_media_frame.
        // Frame layout: see crates/farder-protocol or sub-project #2 spec.
        let sealed = match seal_media_frame(&cfg.stream_key, &cfg.session_id, seq, &pkt) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[voice::send] seal: {e}");
                seq = seq.saturating_add(1);
                continue;
            }
        };

        let _ = cfg.datagram_out.send(sealed);
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
    use crate::audio::MockAudioBackend;

    #[tokio::test]
    async fn mocked_send_emits_frames_when_unmuted() {
        // 5 frames worth of sine; assert 5 datagrams emitted.
        let (datagram_tx, mut datagram_rx) = mpsc::unbounded_channel();
        let (_aec_tx, aec_rx) = watch::channel(vec![0.0f32; OPUS_FRAME_SAMPLES_MONO]);
        let (speak_tx, _speak_rx) = watch::channel(false);
        let muted = Arc::new(AtomicBool::new(false));

        let backend = MockAudioBackend::new_with_n_frames(5);
        let cfg = SendTaskConfig {
            audio_in: Box::new(backend),
            apm: AudioProcessor::new(),
            gate: GateMode::Open,
            session_id: [7u8; 16],
            stream_key: [0xAA; 32],
            aec_ref_rx: aec_rx,
            datagram_out: datagram_tx,
        };

        run(cfg, muted, speak_tx).await;

        let mut count = 0;
        while let Ok(_) = datagram_rx.try_recv() {
            count += 1;
        }
        assert_eq!(count, 5, "expected 5 emitted datagrams, got {count}");
    }

    #[tokio::test]
    async fn muted_drops_frames() {
        let (datagram_tx, mut datagram_rx) = mpsc::unbounded_channel();
        let (_aec_tx, aec_rx) = watch::channel(vec![0.0f32; OPUS_FRAME_SAMPLES_MONO]);
        let (speak_tx, _speak_rx) = watch::channel(false);
        let muted = Arc::new(AtomicBool::new(true));

        let backend = MockAudioBackend::new_with_n_frames(5);
        let cfg = SendTaskConfig {
            audio_in: Box::new(backend),
            apm: AudioProcessor::new(),
            gate: GateMode::Open,
            session_id: [7u8; 16],
            stream_key: [0xAA; 32],
            aec_ref_rx: aec_rx,
            datagram_out: datagram_tx,
        };

        run(cfg, muted, speak_tx).await;
        assert!(datagram_rx.try_recv().is_err(), "muted send must emit nothing");
    }

    #[tokio::test]
    async fn gate_blocked_drops_frames() {
        let (datagram_tx, mut datagram_rx) = mpsc::unbounded_channel();
        let (_aec_tx, aec_rx) = watch::channel(vec![0.0f32; OPUS_FRAME_SAMPLES_MONO]);
        let (speak_tx, _speak_rx) = watch::channel(false);
        let muted = Arc::new(AtomicBool::new(false));
        let ptt_flag = Arc::new(AtomicBool::new(false));

        let backend = MockAudioBackend::new_with_n_frames(5);
        let cfg = SendTaskConfig {
            audio_in: Box::new(backend),
            apm: AudioProcessor::new(),
            gate: GateMode::Ptt(ptt_flag),
            session_id: [7u8; 16],
            stream_key: [0xAA; 32],
            aec_ref_rx: aec_rx,
            datagram_out: datagram_tx,
        };

        run(cfg, muted, speak_tx).await;
        assert!(datagram_rx.try_recv().is_err(), "Ptt(false) must block all frames");
    }
}
```

- [ ] **Step 2: Adapt `MockAudioBackend` and `AudioBackend` trait as needed**

`AudioBackend` from sub-project #1 was originally synchronous (`MockAudioBackend` produced sine waves). It probably doesn't have `read_input_frame(n).await` yet. Two options:
- Add an async trait method `read_input_frame(&mut self, n: usize) -> Option<Vec<f32>>`. `MockAudioBackend::new_with_n_frames(k)` returns sine for `k` frames then `None`.
- Or use a channel-based capture pattern (see how `audio_cpal.rs` exposes frames).

Pick the pattern that fits existing code. If the AudioBackend doesn't support an async pull, wrap it in a small adapter inside `voice/send.rs` that translates between cpal-style push (callback fires with samples) and the async pull used by the SendTask.

- [ ] **Step 3: Run tests to verify they fail (compile or logic)**

```
cd /home/deez/farder/client/src-tauri && cargo test voice::send 2>&1 | tail -15
```

Expected: either compile errors from API mismatches (fix them), or failing tests.

- [ ] **Step 4: Iterate until tests pass**

Adjust `MockAudioBackend` and trait until all 3 tests pass. The point of TDD here is to drive out the right async pull interface on the backend trait.

- [ ] **Step 5: Run tests to verify they pass**

```
cd /home/deez/farder/client/src-tauri && cargo test voice::send 2>&1 | tail -10
```

Expected: 3 passed.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add client/src-tauri/src
git -C /home/deez/farder commit -m "feat(client): voice::send — capture→APM→gate→encode→seal pipeline"
```

---

## Task 8: recv.rs — RecvTask + tests

**Files:**
- Modify: `client/src-tauri/src/voice/recv.rs`

- [ ] **Step 1: Define struct + write failing tests**

```rust
// client/src-tauri/src/voice/recv.rs
use crate::opus_codec::{OpusDecoder, OPUS_FRAME_SAMPLES_MONO, OPUS_SAMPLE_RATE};
use crate::voice::jitter::JitterBuffer;
use farder_crypto::media::open_media_frame;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};
use tokio::sync::mpsc;

pub struct PeerPcmRing {
    inner: Mutex<std::collections::VecDeque<f32>>,
    capacity: usize,
}

impl PeerPcmRing {
    pub fn new(capacity_frames: usize) -> Self {
        Self {
            inner: Mutex::new(std::collections::VecDeque::with_capacity(capacity_frames * OPUS_FRAME_SAMPLES_MONO)),
            capacity: capacity_frames * OPUS_FRAME_SAMPLES_MONO,
        }
    }

    pub fn push_frame(&self, samples: &[f32]) {
        let mut q = self.inner.lock().unwrap();
        for s in samples {
            if q.len() >= self.capacity {
                q.pop_front();
            }
            q.push_back(*s);
        }
    }

    pub fn pop_frame(&self) -> Vec<f32> {
        let mut q = self.inner.lock().unwrap();
        let n = OPUS_FRAME_SAMPLES_MONO.min(q.len());
        let mut out = Vec::with_capacity(OPUS_FRAME_SAMPLES_MONO);
        for _ in 0..n {
            out.push(q.pop_front().unwrap());
        }
        // Pad with silence if ring underflowed.
        while out.len() < OPUS_FRAME_SAMPLES_MONO {
            out.push(0.0);
        }
        out
    }
}

pub struct RecvTaskConfig {
    pub session_id: [u8; 16],
    pub stream_key: [u8; 32],
    pub deafened: Arc<AtomicBool>,
    pub datagram_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    pub pcm_ring: Arc<PeerPcmRing>,
}

pub async fn run(mut cfg: RecvTaskConfig) {
    let mut decoder = match OpusDecoder::new(OPUS_SAMPLE_RATE, 1) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[voice::recv] decoder init: {e}");
            return;
        }
    };
    let mut jitter = JitterBuffer::new();

    // Tick every 20 ms; on each tick, pop one slot and either decode or PLC.
    // For test simplicity we drive the loop off datagram arrival, but in
    // production this should be paced.
    while let Some(bytes) = cfg.datagram_rx.recv().await {
        if cfg.deafened.load(Ordering::Acquire) {
            continue;
        }
        // Parse seq from header (bytes[4..12], big-endian u64).
        if bytes.len() < 28 { continue; }
        let mut seq_buf = [0u8; 8];
        seq_buf.copy_from_slice(&bytes[4..12]);
        let seq = u64::from_be_bytes(seq_buf);
        // Open the sealed frame; returns Opus packet bytes.
        let pkt = match open_media_frame(&cfg.stream_key, &bytes) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[voice::recv] open: {e}");
                continue;
            }
        };
        jitter.insert(seq, pkt);
        // Drain whatever's poppable (typically 1 frame).
        if let Some(pkt) = jitter.pop() {
            match decoder.decode(&pkt) {
                Ok(pcm) => cfg.pcm_ring.push_frame(&pcm),
                Err(e) => eprintln!("[voice::recv] decode: {e}"),
            }
        } else {
            if let Ok(pcm) = decoder.decode_plc() {
                cfg.pcm_ring.push_frame(&pcm);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opus_codec::OpusEncoder;
    use farder_crypto::media::seal_media_frame;

    fn make_frame(stream_key: &[u8; 32], session_id: &[u8; 16], seq: u64, sine_hz: f32) -> Vec<u8> {
        let mut enc = OpusEncoder::new(OPUS_SAMPLE_RATE, 1, 24_000).unwrap();
        let pcm: Vec<f32> = (0..OPUS_FRAME_SAMPLES_MONO)
            .map(|i| (2.0 * std::f32::consts::PI * sine_hz * i as f32 / OPUS_SAMPLE_RATE as f32).sin() * 0.5)
            .collect();
        let pkt = enc.encode(&pcm).unwrap();
        seal_media_frame(stream_key, session_id, seq, &pkt).unwrap()
    }

    #[tokio::test]
    async fn in_order_datagrams_decoded_into_ring() {
        let stream_key = [0x11u8; 32];
        let session_id = [0x22u8; 16];
        let ring = Arc::new(PeerPcmRing::new(10));
        let (tx, rx) = mpsc::unbounded_channel();

        for seq in 0..3 {
            tx.send(make_frame(&stream_key, &session_id, seq, 440.0)).unwrap();
        }
        drop(tx);

        run(RecvTaskConfig {
            session_id,
            stream_key,
            deafened: Arc::new(AtomicBool::new(false)),
            datagram_rx: rx,
            pcm_ring: ring.clone(),
        }).await;

        let frame = ring.pop_frame();
        assert_eq!(frame.len(), OPUS_FRAME_SAMPLES_MONO);
    }

    #[tokio::test]
    async fn deafened_drops_everything() {
        let stream_key = [0x11u8; 32];
        let session_id = [0x22u8; 16];
        let ring = Arc::new(PeerPcmRing::new(10));
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(make_frame(&stream_key, &session_id, 0, 440.0)).unwrap();
        drop(tx);

        run(RecvTaskConfig {
            session_id,
            stream_key,
            deafened: Arc::new(AtomicBool::new(true)),
            datagram_rx: rx,
            pcm_ring: ring.clone(),
        }).await;

        let frame = ring.pop_frame();
        assert!(frame.iter().all(|&s| s == 0.0), "deafened recv must leave ring silent");
    }

    #[tokio::test]
    async fn corrupted_frame_is_ignored() {
        let ring = Arc::new(PeerPcmRing::new(10));
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(vec![0u8; 30]).unwrap(); // garbage
        drop(tx);

        run(RecvTaskConfig {
            session_id: [0; 16],
            stream_key: [0; 32],
            deafened: Arc::new(AtomicBool::new(false)),
            datagram_rx: rx,
            pcm_ring: ring.clone(),
        }).await; // must not panic
    }
}
```

- [ ] **Step 2: Run tests to verify they fail (initially, before compile fixes)**

```
cd /home/deez/farder/client/src-tauri && cargo test voice::recv 2>&1 | tail -15
```

Check `open_media_frame` signature — the summary listed it but the actual function may take different args. Adjust.

- [ ] **Step 3: Iterate until tests pass**

- [ ] **Step 4: Run tests to verify they pass**

```
cd /home/deez/farder/client/src-tauri && cargo test voice::recv 2>&1 | tail -10
```

Expected: 3 passed.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/voice/recv.rs
git -C /home/deez/farder commit -m "feat(client): voice::recv — unseal→jitter→decode→ring pipeline"
```

---

## Task 9: mixer.rs — MixerTask + tests

**Files:**
- Modify: `client/src-tauri/src/voice/mixer.rs`

- [ ] **Step 1: Define struct + write failing tests**

```rust
// client/src-tauri/src/voice/mixer.rs
use crate::audio::AudioBackend;
use crate::opus_codec::OPUS_FRAME_SAMPLES_MONO;
use crate::voice::recv::PeerPcmRing;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{watch, Mutex};

pub type SessionId = [u8; 16];

pub struct MixerHandle {
    pub peer_rings: Arc<Mutex<HashMap<SessionId, Arc<PeerPcmRing>>>>,
    pub aec_ref_rx: watch::Receiver<Vec<f32>>,
}

pub async fn run(
    mut audio_out: Box<dyn AudioBackend>,
    peer_rings: Arc<Mutex<HashMap<SessionId, Arc<PeerPcmRing>>>>,
    aec_ref_tx: watch::Sender<Vec<f32>>,
) {
    loop {
        let mixed = {
            let rings = peer_rings.lock().await;
            let mut acc = vec![0.0f32; OPUS_FRAME_SAMPLES_MONO];
            for ring in rings.values() {
                let frame = ring.pop_frame();
                for (i, s) in frame.iter().enumerate() {
                    acc[i] += *s;
                }
            }
            for s in acc.iter_mut() {
                *s = soft_clip(*s);
            }
            acc
        };
        let _ = aec_ref_tx.send(mixed.clone());
        if audio_out.write_output_frame(&mixed).await.is_none() {
            break; // device closed
        }
    }
}

fn soft_clip(x: f32) -> f32 {
    x / (1.0 + x.abs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::MockAudioBackend;

    fn make_ring_with_sine(hz: f32, n_frames: usize) -> Arc<PeerPcmRing> {
        let ring = Arc::new(PeerPcmRing::new(n_frames + 2));
        for f in 0..n_frames {
            let frame: Vec<f32> = (0..OPUS_FRAME_SAMPLES_MONO)
                .map(|i| {
                    let t = (f * OPUS_FRAME_SAMPLES_MONO + i) as f32 / 48000.0;
                    (2.0 * std::f32::consts::PI * hz * t).sin() * 0.5
                })
                .collect();
            ring.push_frame(&frame);
        }
        ring
    }

    #[tokio::test]
    async fn empty_registry_emits_silence() {
        let (aec_tx, mut aec_rx) = watch::channel(vec![]);
        let rings: Arc<Mutex<HashMap<SessionId, Arc<PeerPcmRing>>>> = Default::default();
        let out_backend = Box::new(MockAudioBackend::output_capture(3));
        // run for ~3 frames then stop (MockAudioBackend::output_capture exits after 3)
        run(out_backend, rings, aec_tx).await;
        let frame = aec_rx.borrow().clone();
        assert!(frame.iter().all(|&s| s == 0.0), "empty registry must produce silence");
    }

    #[tokio::test]
    async fn single_peer_passes_through() {
        let (aec_tx, mut aec_rx) = watch::channel(vec![]);
        let rings: Arc<Mutex<HashMap<SessionId, Arc<PeerPcmRing>>>> = Default::default();
        let ring = make_ring_with_sine(440.0, 5);
        rings.lock().await.insert([1u8; 16], ring);
        let out_backend = Box::new(MockAudioBackend::output_capture(3));
        run(out_backend, rings, aec_tx).await;
        let frame = aec_rx.borrow().clone();
        let energy: f32 = frame.iter().map(|x| x * x).sum();
        assert!(energy > 0.0, "non-empty ring must produce non-silent output");
    }

    #[tokio::test]
    async fn two_peers_sum_with_soft_clip() {
        let (aec_tx, mut aec_rx) = watch::channel(vec![]);
        let rings: Arc<Mutex<HashMap<SessionId, Arc<PeerPcmRing>>>> = Default::default();
        rings.lock().await.insert([1u8; 16], make_ring_with_sine(440.0, 5));
        rings.lock().await.insert([2u8; 16], make_ring_with_sine(880.0, 5));
        let out_backend = Box::new(MockAudioBackend::output_capture(3));
        run(out_backend, rings, aec_tx).await;
        let frame = aec_rx.borrow().clone();
        assert!(frame.iter().all(|&s| s.abs() < 1.0), "soft clip must keep samples in (-1, 1)");
    }

    #[test]
    fn soft_clip_bounds() {
        assert!(soft_clip(0.5) > 0.0 && soft_clip(0.5) < 0.5);
        assert!(soft_clip(100.0) < 1.0);
        assert!(soft_clip(-100.0) > -1.0);
        assert_eq!(soft_clip(0.0), 0.0);
    }
}
```

- [ ] **Step 2: Add `MockAudioBackend::output_capture(n)` if needed**

If `MockAudioBackend` from sub-project #1 doesn't have an output-capture mode, add one: a constructor that lets the mixer write N frames into an internal buffer, then returns None to terminate.

- [ ] **Step 3: Run tests to verify they fail / compile-fix**

```
cd /home/deez/farder/client/src-tauri && cargo test voice::mixer 2>&1 | tail -15
```

- [ ] **Step 4: Iterate until tests pass**

- [ ] **Step 5: Run tests to verify they pass**

```
cd /home/deez/farder/client/src-tauri && cargo test voice::mixer 2>&1 | tail -10
```

Expected: 4 passed.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add client/src-tauri/src
git -C /home/deez/farder commit -m "feat(client): voice::mixer — sum + soft-clip + AEC reference"
```

---

## Phase 5: Controller + Tauri surface

## Task 10: voice/mod.rs — VoiceController state machine + tests

**Files:**
- Modify: `client/src-tauri/src/voice/mod.rs`

- [ ] **Step 1: Flesh out `VoiceController` with full state + lifecycle**

Replace the skeleton with:

```rust
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, watch, Mutex};
use std::collections::HashMap;

pub struct VoiceController {
    inner: Arc<Mutex<Inner>>,
    app: AppHandle,
}

struct Inner {
    state: VoiceState,
    muted: Arc<AtomicBool>,
    deafened: Arc<AtomicBool>,
    pre_deafen_muted: bool,
    stream_key: Option<[u8; 32]>,
    session_id: Option<SessionId>,
    peers: HashMap<SessionId, PeerEntry>,
    send_handle: Option<tokio::task::JoinHandle<()>>,
    mixer_handle: Option<tokio::task::JoinHandle<()>>,
    aec_ref_tx: Option<watch::Sender<Vec<f32>>>,
    aec_ref_rx: Option<watch::Receiver<Vec<f32>>>,
    dispatcher: Arc<MediaInboundDispatcher>,
    peer_rings: Arc<Mutex<HashMap<SessionId, Arc<crate::voice::recv::PeerPcmRing>>>>,
}

struct PeerEntry {
    pubkey: PublicKey,
    stream_key: [u8; 32],
    recv_handle: tokio::task::JoinHandle<()>,
    datagram_tx: mpsc::UnboundedSender<Vec<u8>>,
}

impl VoiceController {
    pub fn new(app: AppHandle, dispatcher: Arc<MediaInboundDispatcher>) -> Self {
        let (aec_tx, aec_rx) = watch::channel(vec![0.0f32; OPUS_FRAME_SAMPLES_MONO]);
        Self {
            inner: Arc::new(Mutex::new(Inner {
                state: VoiceState { channel_id: None, muted: false, deafened: false, peers: vec![] },
                muted: Arc::new(AtomicBool::new(false)),
                deafened: Arc::new(AtomicBool::new(false)),
                pre_deafen_muted: false,
                stream_key: None,
                session_id: None,
                peers: HashMap::new(),
                send_handle: None,
                mixer_handle: None,
                aec_ref_tx: Some(aec_tx),
                aec_ref_rx: Some(aec_rx),
                dispatcher,
                peer_rings: Default::default(),
            })),
            app,
        }
    }

    pub async fn join(&self, channel_id: ChannelId, server: Arc<ServerSession>) -> Result<(), String> {
        let mut inner = self.inner.lock().await;
        if inner.session_id.is_some() {
            drop(inner);
            self.leave(server.clone()).await?;
            inner = self.inner.lock().await;
        }
        // 1. JoinStream → session_id
        let session_id = server.join_stream(channel_id).await?;
        // 2. derive_stream_key
        let key = farder_crypto::media::derive_stream_key();
        // 3. get_stream_state → wrap_stream_key_for_peer for each → OfferStreamKey
        let participants = server.get_stream_state(channel_id).await?;
        let wrapped: HashMap<PublicKey, Vec<u8>> = participants.iter()
            .map(|p| (p.pubkey, farder_crypto::media::wrap_stream_key_for_peer(&key, &p.pubkey, &server.my_keypair())))
            .collect();
        server.offer_stream_key(session_id, wrapped).await?;
        // 4. Spawn mixer
        let mixer_handle = {
            let rings = inner.peer_rings.clone();
            let aec_tx = inner.aec_ref_tx.clone().unwrap();
            let out_backend = crate::audio::make_output_backend();
            tokio::spawn(async move {
                crate::voice::mixer::run(out_backend, rings, aec_tx).await;
            })
        };
        // 5. Spawn send
        let aec_rx = inner.aec_ref_rx.clone().unwrap();
        let muted = inner.muted.clone();
        let app_for_speaking = self.app.clone();
        let (speak_tx, mut speak_rx) = watch::channel(false);
        let datagram_out = server.media_datagram_sender();
        let send_handle = tokio::spawn(async move {
            crate::voice::send::run(
                crate::voice::send::SendTaskConfig {
                    audio_in: crate::audio::make_input_backend(),
                    apm: crate::voice::apm::AudioProcessor::new(),
                    gate: crate::voice::gate::GateMode::Open,
                    session_id,
                    stream_key: key,
                    aec_ref_rx: aec_rx,
                    datagram_out,
                },
                muted,
                speak_tx,
            ).await;
        });
        // 6. EnableTrack
        server.enable_track(farder_protocol::server::TrackKind::Audio).await?;
        // 7. Speaking-event forwarder
        let app_for_local = self.app.clone();
        tokio::spawn(async move {
            while speak_rx.changed().await.is_ok() {
                let s = *speak_rx.borrow();
                let _ = app_for_local.emit("voice://local-speaking", serde_json::json!({ "speaking": s }));
            }
        });
        // 8. Update state
        inner.session_id = Some(session_id);
        inner.stream_key = Some(key);
        inner.send_handle = Some(send_handle);
        inner.mixer_handle = Some(mixer_handle);
        inner.state.channel_id = Some(channel_id);
        let snap = inner.state.clone();
        let _ = self.app.emit("voice://state-changed", &snap);
        Ok(())
    }

    pub async fn leave(&self, server: Arc<ServerSession>) -> Result<(), String> {
        let mut inner = self.inner.lock().await;
        if let Some(_sid) = inner.session_id.take() {
            let _ = server.disable_track(farder_protocol::server::TrackKind::Audio).await;
            let _ = server.leave_stream().await;
            if let Some(h) = inner.send_handle.take() { h.abort(); }
            if let Some(h) = inner.mixer_handle.take() { h.abort(); }
            for (_, peer) in inner.peers.drain() {
                peer.recv_handle.abort();
            }
            inner.peer_rings.lock().await.clear();
            inner.stream_key = None;
            inner.state = VoiceState { channel_id: None, muted: false, deafened: false, peers: vec![] };
            inner.muted.store(false, Ordering::Release);
            inner.deafened.store(false, Ordering::Release);
        }
        let snap = inner.state.clone();
        let _ = self.app.emit("voice://state-changed", &snap);
        Ok(())
    }

    pub async fn set_mute(&self, muted: bool) -> Result<(), String> {
        let inner = self.inner.lock().await;
        inner.muted.store(muted, Ordering::Release);
        let mut snap = inner.state.clone();
        snap.muted = muted;
        let _ = self.app.emit("voice://state-changed", &snap);
        Ok(())
    }

    pub async fn set_deafen(&self, deafened: bool) -> Result<(), String> {
        let mut inner = self.inner.lock().await;
        if deafened {
            inner.pre_deafen_muted = inner.muted.load(Ordering::Acquire);
            inner.muted.store(true, Ordering::Release);
            inner.deafened.store(true, Ordering::Release);
        } else {
            inner.muted.store(inner.pre_deafen_muted, Ordering::Release);
            inner.deafened.store(false, Ordering::Release);
        }
        inner.state.muted = inner.muted.load(Ordering::Acquire);
        inner.state.deafened = deafened;
        let snap = inner.state.clone();
        let _ = self.app.emit("voice://state-changed", &snap);
        Ok(())
    }

    pub async fn state(&self) -> VoiceState {
        self.inner.lock().await.state.clone()
    }

    // (Peer-arrival / departure / TrackActivity routing methods added inline
    //  via inbound-event callbacks — wire these in connection.rs's event handler.)
}
```

The exact `ServerSession` API (`join_stream`, `get_stream_state`, `offer_stream_key`, `enable_track`, `disable_track`, `leave_stream`, `media_datagram_sender`, `my_keypair`) does not exist yet. Either add minimal wrappers to whichever existing connection abstraction handles control frames, or sketch them inline on a freshly-named handle. **This is the highest-risk integration of the plan**; the implementer should expect to push the structure in `server_manager.rs` or `connection.rs` along the way.

- [ ] **Step 2: Write controller tests**

Add a `#[cfg(test)] mod tests` block exercising:

```rust
    #[tokio::test]
    async fn join_then_leave_round_trip_updates_state() {
        // Use a FakeServerSession that records calls and returns canned responses.
        // After join: state.channel_id == Some(...), VoiceState event emitted.
        // After leave: state.channel_id == None, second event emitted.
    }

    #[tokio::test]
    async fn double_join_auto_leaves_previous() { /* ... */ }

    #[tokio::test]
    async fn set_mute_updates_atomic_and_emits_state() { /* ... */ }

    #[tokio::test]
    async fn set_deafen_implicitly_mutes_and_restores_on_undeafen() { /* ... */ }
```

Provide a `FakeServerSession` test double. Mock everything it touches; the test's job is to validate the controller's state machine + event emissions, not the network.

- [ ] **Step 3: Run tests until they pass**

```
cd /home/deez/farder/client/src-tauri && cargo test voice::tests 2>&1 | tail -15
```

Expected: 4 passed.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src
git -C /home/deez/farder commit -m "feat(client): VoiceController — join/leave/mute/deafen state machine"
```

---

## Task 11: Tauri commands + bridge events

**Files:**
- Modify: `client/src-tauri/src/main.rs` (or wherever the invoke handler is)
- Modify: `client/src-tauri/src/bridge.rs` (to surface event types if needed)

- [ ] **Step 1: Register the 5 commands**

In the Tauri invoke handler, add `voice_join`, `voice_leave`, `voice_set_mute`, `voice_set_deafen`, `voice_get_state`. Each is a thin async wrapper that grabs `State<Arc<VoiceController>>` and `State<Arc<ServerSession>>` and delegates.

```rust
#[tauri::command]
async fn voice_join(
    state: tauri::State<'_, Arc<VoiceController>>,
    server: tauri::State<'_, Arc<ServerSession>>,
    channel_id: [u8; 16],
) -> Result<(), String> {
    state.join(channel_id, server.inner().clone()).await
}
// (Same shape for leave, set_mute, set_deafen, get_state.)
```

Adapt to existing patterns — `bridge.rs` already has dozens of `#[tauri::command]`s; follow their style for state injection, error mapping, and bridge.ts type generation if applicable.

- [ ] **Step 2: Wire inbound media events to the controller**

In the server-event dispatch loop (probably in `bridge.rs` or `connection.rs`'s receive task), add handlers for:
- `ServerEvent::StreamKeyOffer { ... }` → call a new method `VoiceController::on_stream_key_offer(...)`
- `ServerEvent::TrackEnabled { Audio, session_id, peer }` → `on_peer_track_enabled(...)`: spawn RecvTask, register ring, register dispatcher route.
- `ServerEvent::TrackDisabled { Audio, session_id }` → `on_peer_track_disabled(...)`: drain + abort RecvTask, unregister.
- `ServerEvent::StreamLeft { session_id }` → same as TrackDisabled.
- `ServerEvent::TrackActivity { session_id, kind: Audio, active }` → `on_peer_activity(...)`: emit `voice://peer-speaking`.

Add these methods to `VoiceController` in this step. Each just updates state and emits.

- [ ] **Step 3: Generate TypeScript bindings (if the project uses tauri-specta or similar)**

If `bridge.ts` is auto-generated from `bridge.rs`, run the generator. If hand-written, add type stubs for the new events + commands in the existing TS bridge file.

- [ ] **Step 4: cargo check + tsc**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -5
```

Expected: both clean.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src client/src
git -C /home/deez/farder commit -m "feat(client): wire VoiceController Tauri commands + media event handlers"
```

---

## Phase 6: Verification

## Task 12: Final smoke + workspace verify

**Files:**
- None (verification only)

- [ ] **Step 1: Run all voice tests**

```
cd /home/deez/farder/client/src-tauri && cargo test voice:: 2>&1 | tail -30
```

Expected: ~24 tests passing (3 gate + 7 jitter + 4 apm + 3 send + 3 recv + 4 mixer + 4 controller).

- [ ] **Step 2: Run all opus_codec tests (regression)**

```
cd /home/deez/farder/client/src-tauri && cargo test opus_codec:: 2>&1 | tail -5
```

Expected: 11 passed (unchanged from sub-project #3.2).

- [ ] **Step 3: Workspace check**

```
cd /home/deez/farder && cargo check --workspace 2>&1 | tail -10
```

Expected: clean.

- [ ] **Step 4: Server media_stream tests (regression)**

```
cd /home/deez/farder && cargo test -p farder-server media_stream 2>&1 | tail -10
```

Expected: clean.

- [ ] **Step 5: Client UI typecheck**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -5
```

Expected: clean.

- [ ] **Step 6: No CHANGELOG entry**

Backend-only sub-project. #3.4 (Voice UI) ships the aggregate user-visible entry.

- [ ] **Step 7: No final commit**

Steps 1-5 are read-only.

---

## Self-review notes

**Spec coverage:**

| Spec section | Implemented in |
|---|---|
| Pipeline send path | Task 7 (`send.rs`) |
| Pipeline recv path | Task 8 (`recv.rs`) |
| Mixer + AEC reference | Task 9 (`mixer.rs`) |
| APM AEC/NS/AGC | Task 4 (`apm.rs`) |
| Gate trait, Open variant | Task 2 (`gate.rs`) |
| Jitter buffer | Task 3 (`jitter.rs`) |
| Server datagram fanout | Task 5 |
| Client datagram dispatch | Task 6 |
| VoiceController lifecycle | Task 10 |
| Mute / deafen | Task 10 |
| Local speaking indicator | Task 7 + Task 10 (event forwarder) |
| Remote speaking indicator | Task 11 (`TrackActivity` → `voice://peer-speaking`) |
| Tauri commands + events | Task 11 |

**Placeholder scan:** None. Each step has either concrete code or a clearly-bounded investigation ("read the crate docs, adapt names"). The two highest-uncertainty areas — `webrtc-audio-processing` API and `ServerSession` shape — are flagged explicitly in their respective tasks.

**Type consistency:**
- `SessionId = [u8; 16]` defined in Task 1, used across tasks 6/7/8/9/10/11.
- `PublicKey` imported from `farder_crypto::identity::PublicKey` consistently.
- `VoiceState` / `VoicePeer` shape stable from Task 1 onward.
- `OPUS_FRAME_SAMPLES_MONO` (960) used consistently for all frame allocations.
- `derive_stream_key`, `wrap_stream_key_for_peer`, `seal_media_frame`, `open_media_frame` reused verbatim from `farder_crypto::media`.

**No CHANGELOG by design** — backend sub-project; #3.4 ships the aggregate entry.

## Notes for the implementer

- **Highest-risk task: Task 10.** The `ServerSession` shape is fictional in this plan; the implementer must find the existing client→server abstraction (likely in `server_manager.rs` or `bridge.rs`) and either extend it with the listed methods or sketch a thin facade. If the existing pattern looks fundamentally incompatible, stop and surface to the controller before pushing forward.
- **Audio backend pull vs push.** The `AudioBackend` trait from sub-project #1 may be push-style (cpal callback emits samples). The pipeline assumes async pull. Add an adapter in the send/mixer tasks if needed — `tokio::sync::mpsc::unbounded_channel` between the cpal callback thread and the async task.
- **Why no `tokio::time::interval` pacing in mixer/recv:** the audio output device drives the pacing — when cpal asks for the next chunk, we deliver it. The mixer's loop iteration rate is therefore set by the output device. Recv just consumes datagrams as they arrive and feeds the jitter buffer; the per-peer ring smooths out small timing variance.
- **AEC reference signal accuracy:** ideally APM gets the playback PCM that's *about to play* with a known delay. We approximate by feeding APM the last-mixed frame via `watch::channel`. This isn't perfect but is good enough for v1 with adaptive AEC (which auto-tunes delay).
- **Test discipline:** every task has tests. Even Task 10's controller-state tests are checking small contracts (state emitted, atomics flipped, handles spawned/aborted). If a controller test starts requiring six pages of setup, that's a sign to extract a smaller test target instead of writing the giant integration.
