# Voice Client Pipeline Design

## Overview

**Position in roadmap:** Sub-project #3.3 of the audio + screensharing track. Wires the previously-shipped infrastructure (MediaBackend #1, Media Stream Transport #2, CpalAudioBackend #3.1, Opus Codec #3.2) into the first working voice call. UI lands in #3.4.

## Goals

Ship a working group voice call in a Farder channel:

- Local mic captured by cpal at 48 kHz / 20 ms / mono, run through WebRTC APM (echo cancel, noise suppress, AGC), gated, Opus-encoded, sealed-frame-encrypted, and shipped over QUIC datagrams.
- Remote peers' frames received, unsealed, jitter-buffered (~60 ms), Opus-decoded (with PLC on loss), per-peer ring-buffered, and software-mixed into one cpal output stream.
- Mute, deafen, local speaking indicator, remote speaking indicator (via server's `TrackActivity` event).
- All state controlled from React via Tauri commands; React renders from snapshot events.

## Non-goals (v1)

- Push-to-talk and VAD gating: gate trait is built, but only `Open` is implemented. `Ptt` + `Vad` are trait variants with stubs.
- Video tracks: `TrackKind::Video` is ignored by `VoiceController`.
- Automatic reconnect after QUIC drop.
- Mixing polish (limiter, ducking, per-peer volume sliders).
- Speaker-test / mic-test diagnostics.
- Multiple voice channels at once.

## Pipeline

```
SEND PATH (one task per local user in a call)
┌────────┐   ┌─────────┐   ┌─────────────┐   ┌────────┐   ┌──────────┐
│  cpal  │──▶│  APM    │──▶│ GateMode    │──▶│ Opus   │──▶│  Sealed  │──▶ QUIC datagram
│ input  │   │ AEC/NS/ │   │ (Open in v1)│   │ encode │   │  frame   │
│  20ms  │   │  AGC    │   │             │   │        │   │  build   │
└────────┘   └─────────┘   └─────────────┘   └────────┘   └──────────┘

RECV PATH (one task per remote peer)
QUIC datagram ──▶ ┌──────────┐   ┌──────────┐   ┌────────┐   ┌─────────┐
                  │  Sealed  │──▶│  Jitter  │──▶│ Opus   │──▶│ Per-peer│
                  │  frame   │   │  buffer  │   │ decode │   │  PCM    │
                  │  open    │   │ (~60ms)  │   │ +PLC   │   │  ring   │
                  └──────────┘   └──────────┘   └────────┘   └─────────┘
                                                                  │
                            ┌─────────────────────────────────────┘
                            ▼
                     ┌─────────┐   ┌────────┐
                     │  Mixer  │──▶│  cpal  │  (one task; sums all peer rings, soft-clips)
                     └─────────┘   └────────┘
```

## Module layout

New files in `client/src-tauri/src/voice/`:

| File | Responsibility |
|---|---|
| `mod.rs` | `VoiceController` — public surface, call state machine, registers Tauri commands |
| `send.rs` | Send-path task: owns `AudioBackend` input + APM + gate + encoder + sealed-frame builder |
| `recv.rs` | Recv-path task spawned per peer: owns jitter buffer + decoder + per-peer PCM ring |
| `mixer.rs` | Mixer task: owns `AudioBackend` output; drains all peer rings, sums, soft-clips |
| `apm.rs` | Thin wrapper over `webrtc-audio-processing` configured for 48 kHz / 20 ms / mono |
| `gate.rs` | `enum GateMode { Open, Vad(VadConfig), Ptt(Arc<AtomicBool>) }`, `fn pass(&mut self, pcm: &[f32]) -> bool` |
| `jitter.rs` | Per-peer reorder + smoothing buffer (3 frames deep, by `seq`) |

Modified files:

- `client/src-tauri/Cargo.toml` — add `webrtc-audio-processing = "0.6"` (or current stable).
- `client/src-tauri/src/main.rs` — `mod voice;` and register the 5 Tauri commands.

## Call lifecycle

### Joining a channel

1. UI invokes `voice_join(channel_id)`.
2. `VoiceController` sends `ServerRequest::JoinStream { channel_id }` → receives `session_id`.
3. `derive_stream_key()` produces a fresh 32-byte ChaCha20 key.
4. Controller calls `get_stream_state(channel_id)` to learn current participants, builds a `HashMap<peer_pk, ciphertext>` via `wrap_stream_key_for_peer`, and sends `OfferStreamKey { session_id, wrapped_keys }`.
5. Controller spawns send-path task + mixer task. Mixer is idle (no peer rings yet).
6. Controller sends `EnableTrack { kind: Audio }`. Send task begins capturing and emitting frames.
7. Controller emits `voice://state-changed` with the new `VoiceState`.

### Peer arrival

- Event `StreamKeyOffer { sender, session_id, wrapped_keys }` arrives. Controller unwraps its own entry → stores `(session_id, sender_pk) → ChaCha20Key` in a peer table.
- Event `TrackEnabled { session_id, kind: Audio }` arrives. Controller spawns a recv-path task for `(sender_pk, session_id)`, allocates a PCM ring, registers it with the mixer.
- Inbound QUIC datagrams matching the session_id are routed to that recv task; the task drains them through jitter buffer → opus decoder → PCM ring.

### Peer departure

- `TrackDisabled { Audio }` or `StreamLeft { session_id }`: recv task drains remaining buffered frames, emits 1-2 PLC frames for the tail to avoid an audible click, then exits and is unregistered from the mixer.

### Leaving

- `voice_leave` → controller sends `DisableTrack { Audio }`, then `LeaveStream`. Drops send task, mixer task, all recv tasks, and the stream key.

### Reconnects

QUIC connection drops during a call are treated as an implicit `LeaveStream`. The user has to rejoin manually. (Auto-reconnect deferred.)

### One-channel invariant

If `voice_join` is called while already in a channel, the controller calls `voice_leave` first, then proceeds.

## Mute, deafen, speaking indicators

### Mute

- `voice_set_mute(true)`: send-path task **drops encoded frames silently** — captures, runs APM, encodes, then throws the output away.
- Local-only gag. No `DisableTrack` round-trip; peers don't see your speaker icon flicker on every mute toggle.
- Trade-off: wasted CPU on the dropped-frame path. Acceptable for v1; can short-circuit earlier later.

### Deafen

- `voice_set_deafen(true)`: recv tasks drop QUIC datagrams at intake (no decode). Mixer emits silence. Implicitly forces `muted = true` (because talking to a void is rude).
- `voice_set_deafen(false)`: recv flow restored. Mute is restored to whatever the user's setting was before deafen flipped on.

### Local speaking indicator

- Send-path task tracks RMS of the most recent post-APM PCM frame.
- When RMS exceeds a fixed threshold for 2 consecutive frames (40 ms), emit `voice://local-speaking { speaking: true }`.
- After 300 ms below threshold, emit `voice://local-speaking { speaking: false }`.

### Remote speaking indicator

- Server (sub-project #2) already emits `ServerEvent::TrackActivity { session_id, kind, active }` based on token-bucket usage.
- Controller subscribes to that event, maps `session_id → peer_pk`, re-emits as `voice://peer-speaking { pubkey, speaking }`.
- Free signal — no decode-side audio analysis required.

## Tauri surface

### Commands

```rust
#[tauri::command] async fn voice_join(channel_id: ChannelId) -> Result<(), String>;
#[tauri::command] async fn voice_leave() -> Result<(), String>;
#[tauri::command] async fn voice_set_mute(muted: bool) -> Result<(), String>;
#[tauri::command] async fn voice_set_deafen(deafened: bool) -> Result<(), String>;
#[tauri::command] async fn voice_get_state() -> Result<VoiceState, String>;
```

### State payload

```rust
pub struct VoiceState {
    pub channel_id: Option<ChannelId>,
    pub muted: bool,
    pub deafened: bool,
    pub peers: Vec<VoicePeer>,
}
pub struct VoicePeer {
    pub pubkey: PublicKey,
    pub speaking: bool,
}
```

### Events

| Event | Payload | Fires when |
|---|---|---|
| `voice://state-changed` | `VoiceState` | join/leave/mute/deafen/peer add/peer remove |
| `voice://local-speaking` | `{ speaking: bool }` | local RMS crosses threshold |
| `voice://peer-speaking` | `{ pubkey, speaking: bool }` | `TrackActivity` from server |
| `voice://error` | `{ message: string }` | recoverable failure (audio device disappeared, encode error, ...) |

Snapshot-style `state-changed` matches how `ServerContext` already consumes server state in React. Sub-project #3.4 reads the snapshot and renders.

## APM configuration

```rust
// voice/apm.rs
let mut apm = webrtc_audio_processing::Processor::new(...)?;
apm.set_config(Config {
    echo_cancellation: Some(EchoCancellation { enable_extended_filter: false, ... }),
    noise_suppression: Some(NoiseSuppression { level: High }),
    gain_controller: Some(GainController { mode: AdaptiveDigital, target_level_dbfs: 3 }),
    ..
});
```

- Sample rate: 48 kHz, mono, 20 ms (160 internal 10 ms passes — APM operates on 10 ms blocks; we feed two blocks per Opus frame).
- AEC requires the playback signal to be fed back to APM as the reference. Mixer task forwards its pre-output PCM to the send task's APM via a single-element `tokio::sync::watch` (or equivalent) before writing to cpal.
- If APM init fails (rare — usually missing OS audio config), we fall back to a no-op identity processor and surface a `voice://error` warning. The call still works, just without echo cancellation.

## Gate trait

```rust
pub enum GateMode {
    Open,
    Vad(VadConfig),                  // stub variant; algorithm in a follow-up
    Ptt(Arc<AtomicBool>),            // stub variant; key binding in a follow-up
}

impl GateMode {
    pub fn pass(&mut self, _pcm: &[f32]) -> bool {
        match self {
            GateMode::Open => true,
            GateMode::Vad(_) => true,                 // stub: always passes for now
            GateMode::Ptt(flag) => flag.load(Acquire),
        }
    }
}
```

v1 wires `GateMode::Open`. The `Ptt` arm is functionally complete (just needs a key binding upstream); `Vad` is a stub returning `true`.

## Jitter buffer

Per-peer:

- Fixed depth: 3 frames (60 ms).
- Indexed by `seq` from the sealed frame header.
- On insert: if `seq` is older than the head, drop. If `seq` is within window, place at the right slot. If `seq` is ahead of the window, advance the window (possibly skipping frames; those become PLC at decode time).
- Output: pop the oldest slot once per 20 ms tick. If empty → call `decode_plc()`.

## Mixer

- Drains one frame from each peer ring every 20 ms.
- Sums sample-by-sample. After sum, soft-clip via `tanh(x)` (or `x / (1 + |x|)` for cheaper clipping).
- Writes the mixed frame to cpal output.
- For AEC: before mixing the next frame, push the previous output frame back into a `tokio::sync::watch<Vec<f32>>` that the send task pulls as the AEC reference.

## Test strategy

### Tests that run on WSL2 (no audio hardware)

| Module | Tests |
|---|---|
| `jitter.rs` | in-order insert/pop; out-of-order reorder; gap → PLC marker; duplicate drop; window advance; underflow → PLC |
| `mixer.rs` | sum N synthetic sines, all freq components present; no clip under small N; soft-clip kicks in at large N; empty registry → silence; peer add/remove mid-stream |
| `gate.rs` | `Open` passes; `Ptt(false)` blocks; `Ptt(true)` passes |
| `send.rs` | with `MockAudioBackend` feeding a sine, drives end-to-end to a captured QUIC writer; assert correct seq monotonicity, frame count, encode success |
| `recv.rs` | feed crafted sealed frames through, assert decoded PCM length per frame and PLC on gaps |
| `mod.rs` (controller) | `voice_join` → emits `JoinStream`, derives key, calls `OfferStreamKey`, `EnableTrack`; `voice_leave` → emits `DisableTrack` + `LeaveStream`; mute/deafen toggle state and event emission; one-channel invariant (join while in a call → leave then join) |

Target: ~20-25 unit tests, all green on `cargo test voice::`.

### Tests deferred to native-OS smoke (manual)

- cpal mic → speakers round trip.
- APM echo cancellation quality.
- Two real Farder clients on a LAN join the same channel and hear each other.

Sub-project #3.4 (UI) ships a manual smoke checklist that exercises these on macOS / Windows / Linux.

## Crate dependencies

Add to `client/src-tauri/Cargo.toml`:

```toml
webrtc-audio-processing = "0.6"   # pin to latest stable at plan-write time
```

`audiopus`, `cpal`, `send_wrapper`, `tokio`, `chacha20poly1305` are already present from earlier sub-projects.

## Migration / rollout

- Pure addition. No existing protocol or backend code is modified.
- No CHANGELOG entry from this sub-project. #3.4 (UI) ships the user-visible aggregate entry once the call is operable end-to-end through a real button.

## Future considerations

- VAD algorithm (energy + zero-crossing heuristic; ML model later).
- PTT global hotkey hook (`tauri-plugin-global-shortcut`).
- Automatic QUIC reconnect with stream-key re-derive.
- Per-peer volume sliders + ducking.
- Speaker-test diagnostic tool.
- Multi-channel concurrent voice (transport supports it; controller would need to track multiple sessions).
- Video track support (#4 / screensharing).
