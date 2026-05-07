# Voice Calling v1 — Design Spec

**Date:** 2026-05-07
**Phase:** Voice v1 (server-relay Opus). Scoped to first end-to-end voice. P2P toggle is v1.5; screensharing is v2.

## Goal

Real-time voice for DMs and voice channels. Server-relayed (anonymity-first: peer IPs never exposed). Tauri-Rust audio stack. Opus codec over QUIC datagrams. UI: ringing modal for DMs, in-call control bar with mute/deafen, per-user volume, device pickers, and VAD/PTT modes.

## Non-goals

- **P2P direct connections** (deferred to v1.5; will be a per-call toggle)
- **Screensharing / video** (deferred to v2)
- **Echo cancellation, noise suppression, automatic gain control** (deferred to v1.5; ship with a "use headphones" tooltip)
- **End-to-end encryption** (server can decrypt audio in v1; deferred — separate ~1 week project)
- **Group call ringing** (only DM 1:1 calls ring; voice channels don't)
- **Recording / persistence of calls** (calls are ephemeral; nothing logged)
- **Multi-call queuing** (one active DM call at a time per user)

## Privacy positioning

Voice traffic is encrypted in transit by QUIC's TLS but the server process can decrypt it (no E2E in v1). Peer IPs are never exposed because all traffic flows through the server. Users see a "🔒 Server-relay (private)" footer in the voice control bar to make this concrete.

A first-call disclaimer educates users: "Audio is encrypted in transit but the server can decrypt it. For best quality use headphones — there's no echo cancellation in v1."

## Architecture

Three pieces:

1. **Audio engine** (Tauri Rust, new module `client/src-tauri/src/voice.rs`): `cpal` capture → Opus encode → QUIC datagram send. `cpal` playback ← mix ← Opus decode ← QUIC datagram recv. Four threads (capture, encode, recv, playback). All triggered via Tauri commands; audio frames never cross the Tauri/JS boundary.
2. **Server fanout** (Rust, additions to `crates/farder-server/src/{state,connection,voice}.rs`): per-channel listener map, server forwards each inbound datagram to other listeners (skipping deafened ones), validates speaker-pubkey to prevent spoofing.
3. **Client UI** (React/TS): voice channel sidebar with speaking indicators, voice control bar with mute/deafen/leave, member-volume right-click submenu, incoming-call modal for DMs, Voice settings tab.

The audio path is **datagram-only** (lossy, low-latency). The control plane (start/stop, mute, ringing, speaking-state broadcasts) flows through the existing reliable QUIC request/event channel.

## Codec parameters

- **Opus**, mono, 48 kHz sample rate
- 32 kbps bitrate
- 20 ms frame size (960 samples per frame)
- Frame payload: ~80–120 bytes after Opus encode
- VBR enabled (default Opus mode)
- Application: `Voip` (Opus's voice-tuned mode)

These choices are tunable via constants in `voice.rs`; not exposed to users in v1.

## Wire format (datagram payload)

Raw bytes (no serde — too hot for the per-frame path):

```
Offset  Bytes  Field
------  -----  -----
   0      1    Version (0x01)
   1      1    Type (0x01 = audio frame)
   2      8    Sequence number (u64 BE)
  10     32    Speaker public key
  42     N    Opus frame payload
```

Total: 42 + Opus payload bytes (~120–160 bytes typical).

**Send-side rules:**
- Client writes its own pubkey in the speaker field.
- Server validates `speaker_pk == authenticated_pk` on receive; mismatched frames are silently dropped (never logged — anti-DoS).
- On forwarding, server preserves the speaker pubkey (it's already correct after validation).

**Receive-side rules:**
- Drop frames where `version != 0x01` or `type != 0x01`.
- Drop frames where `seq <= last_seq[speaker]` (late or duplicate). No reorder buffer.
- Drop frames where Opus decode fails (return silence for that 20ms slot).

## Protocol additions

`crates/farder-protocol/src/server.rs` — additions to existing enums (all backwards-compatible via `#[serde(default)]` on new optional fields):

```rust
// New ServerRequest variants
StartVoice { channel_id: u64 },
StopVoice,
SetVoiceMute { muted: bool },
SetVoiceDeafen { deafened: bool },

// New ServerEvent variants
VoiceCallIncoming {
    channel_id: u64,
    caller: PublicKey,
    caller_name: String,
},
VoiceCallEnded {
    channel_id: u64,
},
VoiceSpeakingChanged {
    channel_id: u64,
    public_key: PublicKey,
    speaking: bool,
},
```

`JoinVoice` / `LeaveVoice` / `GetVoiceState` are unchanged — they're presence-only. `StartVoice` / `StopVoice` are the audio-stream lifecycle (you can join a channel without transmitting).

The `VoiceCallEnded` event handles the "caller hangs up before callee answers" case: when a caller leaves an empty DM voice channel before the callee accepts, the server emits this so the callee's incoming-call modal closes.

## Server implementation

### `state.rs` additions

```rust
pub struct VoiceState {
    /// Per-channel: members currently transmitting audio (in StartVoice state).
    pub channels: RwLock<HashMap<u64, HashSet<[u8; 32]>>>,
    /// Members who have set self-deafen — server skips forwarding to them.
    pub deafened: RwLock<HashSet<[u8; 32]>>,
    /// Last frame timestamp per speaker. Used to derive speaking state at 5Hz.
    pub speaking_last_frame_ms: RwLock<HashMap<[u8; 32], u64>>,
    /// Currently-broadcast speaking state per pubkey. Used to deduplicate
    /// VoiceSpeakingChanged broadcasts.
    pub speaking_now: RwLock<HashSet<[u8; 32]>>,
}
```

Added to `ServerState` as a `voice: VoiceState` field.

### Datagram receive loop

In `connection.rs`, alongside the existing reliable-stream loop, a new task per connection reads datagrams:

```rust
loop {
    let datagram = conn.read_datagram().await?;
    handle_voice_datagram(&state, authenticated_pk, datagram).await;
}
```

`handle_voice_datagram`:
1. Parse header (12 bytes minimum). Drop if too short / wrong version / wrong type.
2. Validate `speaker_pk == authenticated_pk`. Drop if not.
3. Look up speaker's active voice channel (from `voice.channels`). Drop if not in any.
4. Update `speaking_last_frame_ms[pk] = now_ms()`.
5. For each other listener in the channel:
   - Skip if `voice.deafened.contains(listener)`
   - Look up listener's connection in `state.clients`
   - `conn.send_datagram(datagram_bytes.clone())` — best-effort; ignore errors

### Speaking-state broadcaster

A single tokio task spawned at startup, runs at 5Hz:

```rust
async fn speaking_state_ticker(state: Arc<ServerState>) {
    let mut interval = tokio::time::interval(Duration::from_millis(200));
    loop {
        interval.tick().await;
        let now = unix_ms();
        let last_frames = state.voice.speaking_last_frame_ms.read().await.clone();
        let mut now_speaking = state.voice.speaking_now.write().await;
        for (pk, last_ms) in &last_frames {
            let is_speaking = now.saturating_sub(*last_ms) < 300;
            if is_speaking != now_speaking.contains(pk) {
                let channel_id = lookup_voice_channel(&state, pk).await;
                broadcast(state, channel_id, ServerEvent::VoiceSpeakingChanged {
                    channel_id, public_key: pk_from_bytes(*pk), speaking: is_speaking,
                });
                if is_speaking { now_speaking.insert(*pk); } else { now_speaking.remove(pk); }
            }
        }
    }
}
```

### Handler additions

`handlers.rs` gains four new arms (StartVoice, StopVoice, SetVoiceMute, SetVoiceDeafen). All require the actor to be a member with VIEW_CHANNEL on the target channel. StartVoice also implicitly does JoinVoice if not already joined; StopVoice does NOT auto-leave (you can stop transmitting but stay in the channel as a listener).

DM ringing logic in StartVoice:
1. If channel is a DM channel
2. AND the channel was empty before this StartVoice
3. THEN emit `VoiceCallIncoming` to the OTHER DM participant only (target = `EventTarget::Members(vec![other_pk])`)

Symmetric VoiceCallEnded logic in StopVoice/LeaveVoice when the channel becomes empty during a ring.

## Client Rust implementation

### Module: `client/src-tauri/src/voice.rs`

Public functions: `start(server_id, channel_id)`, `stop()`, `set_mute(bool)`, `set_deafen(bool)`, `set_input_device(Option<String>)`, `set_output_device(Option<String>)`, `set_input_volume(f32)`, `set_output_volume(f32)`, `set_per_user_volume(pk: String, f32)`, `set_ptt_enabled(bool)`, `set_ptt_active(bool)`, `set_vad_threshold(f32)`, `list_audio_devices() -> {inputs, outputs}`.

Internal state (singleton, behind `Mutex<Option<VoiceSession>>`):

```rust
struct VoiceSession {
    server_id: String,
    channel_id: u64,
    capture_handle: JoinHandle<()>,
    encode_handle: JoinHandle<()>,
    recv_handle: JoinHandle<()>,
    playback_handle: JoinHandle<()>,
    shutdown_tx: oneshot::Sender<()>,
    // Shared state read by threads:
    config: Arc<RwLock<VoiceConfig>>,  // mute, deafen, ptt_active, volumes
    decoders: Arc<Mutex<HashMap<[u8; 32], opus::Decoder>>>,
    speaker_buffers: Arc<Mutex<HashMap<[u8; 32], RingBuffer<f32>>>>,
    capture_ring: Arc<Mutex<RingBuffer<f32>>>,
}
```

Threads:

1. **Capture thread**: cpal input stream's data callback writes raw f32 PCM samples into `capture_ring`. Capture is at the device's native sample rate; resampling to 48kHz happens in the encode thread.
2. **Encode thread**: every 20ms, drains 960 samples from `capture_ring` (resampling if needed). Computes RMS energy. Applies VAD/PTT gate. If gate open and not muted, encodes via Opus, builds the datagram (header + Opus bytes), sends via the existing QUIC connection's `send_datagram` API. Increments local sequence counter.
3. **Receive thread**: reads datagrams from the connection. Parses header. Looks up the per-speaker decoder (lazily created). Decodes Opus to f32 PCM, writes to that speaker's playback buffer.
4. **Playback thread**: cpal output stream's data callback. Mixes all per-speaker buffers (sum, then clamp), applies per-user volume multipliers and master output volume, writes to the cpal output buffer.

**VAD logic:**
```rust
let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
let gate_open = if config.ptt_enabled {
    config.ptt_active
} else {
    if rms > config.vad_threshold {
        last_above_threshold_ms = now;
        true
    } else {
        now - last_above_threshold_ms < 200  // 200ms hangover to avoid choppy ends
    }
};
```

**Send path** uses Quinn's `Connection::send_datagram(Bytes)`. Connection accessor lives in the existing `client/src-tauri/src/state.rs` (already accessible per server_id).

**Settings persistence:** all voice settings (input device, output device, volumes, ptt mode/key, vad threshold) persist via existing `commands::settings_get/set` helpers under keys `voice_*`.

### `commands.rs` additions

Thin wrappers calling into `voice::*`. Each command corresponds to a function listed above.

```rust
#[tauri::command]
pub async fn start_voice(state: State<...>, server_id: String, channel_id: u64) -> Result<(), String> {
    voice::start(&state, &server_id, channel_id).await.map_err(|e| e.to_string())
}
// ... etc
```

Registered in `main.rs`.

### Bridge events

`bridge.rs` gains arms for the four new server events:
```rust
ServerEvent::VoiceCallIncoming { channel_id, caller, caller_name } =>
    app.emit("server:voice_call_incoming", json!({...})),
ServerEvent::VoiceCallEnded { channel_id } =>
    app.emit("server:voice_call_ended", json!({...})),
ServerEvent::VoiceSpeakingChanged { channel_id, public_key, speaking } =>
    app.emit("server:voice_speaking_changed", json!({...})),
```

(JoinVoice/LeaveVoice already emit; no change there.)

## Client UI

### `VoiceControlBar.tsx` (new)

Fixed at the bottom of `ChannelSidebar` while in voice. Shows:
- "Voice Connected • <channel name>"
- 🔒 Server-relay (private) footer text
- Mute / Deafen / Leave buttons

### Voice-channel members in `ChannelSidebar.tsx` (modified)

Existing voice-channel rendering shows participants. Add per-participant:
- Mic icon (red when muted, gray otherwise)
- Headphones icon when deafened
- Avatar gets a 2px green ring while speaking (driven by `VoiceSpeakingChanged` event into reducer state, with a 300ms timeout to clear if no further state change arrives).

### `MemberContextMenu.tsx` (modified)

Add a "Volume" item (with submenu — slider 0-200%) when the target is in the same voice channel as the actor. Stores per-user volume in a Map persisted to settings.json under `voice_per_user_volumes`.

### `IncomingCallModal.tsx` (new)

Full-screen overlay (zIndex 3000). Shows caller avatar, name, "is calling…", Accept / Decline buttons. Plays a short bundled WAV (`/sounds/ringtone.wav`, ~3 sec loop) for up to 30 seconds, then auto-dismisses with a missed-call toast. Triggered by `server:voice_call_incoming` event; dismissed by either user action OR `server:voice_call_ended`.

System notification fired in parallel (using existing `notification_*` infra) so the call is discoverable when the app's not focused.

### Voice settings tab (added to existing `AppearanceSettings.tsx`)

A third tab next to Appearance / GIF Search. Contents:
- Input device (dropdown, populated via `list_audio_devices`)
- Output device (dropdown)
- Input volume (slider 0-200%)
- Output volume (slider 0-200%)
- Voice activation mode: radio (Voice Activity / Push-to-Talk)
- VAD threshold slider + live RMS meter (visible when VAD selected)
- PTT key picker (visible when PTT selected) — "Click and press a key" UI

### Reducer/state additions

`ServerContext.tsx` additions to `PerServerState`:
```ts
voiceSpeakingPks: Set<string>;     // who's currently speaking in the active voice channel
voiceCallIncoming: { channelId: number; callerPk: string; callerName: string } | null;
```

Plus `AppState`-level voice config (mute, deafen, etc.) — actually that's UI-only state, can stay in AppShell or VoiceControlBar local state.

## Edge cases (full table reproduced from brainstorm Section 6)

| Case | Handling |
|---|---|
| User joins voice while already in another voice channel | Server auto-leaves previous channel before joining new one. |
| User disconnects from QUIC mid-call | Server's connection-close handler removes them from voice state, broadcasts VoiceLeft. |
| Datagram dropped on the network | Opus decoder concealment (<60ms); silent frame for longer gaps. No retransmission. |
| Datagram arrives out of order | Sequence-number check; drop late/dup. No reorder buffer. |
| Microphone permission denied / no device | `start_voice` returns Err with user-facing message; UI surfaces inline. |
| Mic produces silence (hardware mute) | VAD never opens; behaves identically to self-mute. |
| Two callers ring same DM peer simultaneously | First call's modal stays; second VoiceCallIncoming ignored via state flag. |
| Caller hangs up before callee answers | Server emits VoiceCallEnded → modal auto-closes. |
| Spoofed speaker pubkey in datagram | Server drops silently (anti-DoS). |
| Server send_datagram backpressure | Best-effort drop; never stalls audio thread. |
| Echo without headphones | Documented in disclaimer; v1.5 adds AEC. |
| No audio devices (WSL) | Empty device list; voice controls greyed out with explanatory text. |
| Voice settings file corrupted | Falls back to defaults per existing settings_get pattern. |
| User switches server / logs out while in voice | `stop_voice` called automatically before disconnect. |

## Testing

**Server unit tests** (in `crates/farder-server/src/voice.rs` test module + `connection.rs` for the datagram path):
- Datagram parser: valid, truncated, wrong-version, wrong-type
- Anti-spoof: forged speaker_pk dropped
- Fanout: 3-member channel, A speaks → B and C receive, A doesn't echo
- Deafen: deafened member skipped on fanout
- Speaking-state ticker: state flips correctly at 200ms granularity, broadcasts only on change
- DM ringing: StartVoice in empty DM channel emits VoiceCallIncoming to the other party only
- VoiceCallEnded: emitted when channel becomes empty during a ring

**Client Rust unit tests** (in `voice.rs` test module):
- VAD threshold (silent input → no frames; loud input → frames)
- PTT gate (PTT-active determines gate state)
- Opus encode/decode roundtrip preserves frame count and approximate energy
- Per-user volume scaling correctness
- Decoder graceful handling of malformed Opus payload (returns silence)

**Integration test** (Rust, in-process, in `crates/farder-server/tests/`):
- Two clients in a voice channel, A sends 100 frames, B receives same count (frame-by-frame byte equality not expected; sequence-number monotonicity confirmed)

**No automated client-TS tests** (consistent with codebase).

**Manual smoke test** (in plan):
- Two clients (Alice + Bob via FARDER_DATA), both join a voice channel, mic test confirms two-way audio
- Self-mute on Alice → Bob hears nothing from her
- Self-deafen on Bob → Alice can still hear herself? No — deafen is server-suppressed, Alice still transmits but Bob never receives
- DM call ringing: Alice starts voice in DM, Bob's modal pops with ringtone, Accept connects, Decline emits VoiceCallEnded
- PTT toggle, VAD threshold tuning, device picker
- Speaking indicators glow correctly across both clients
- Per-user volume slider attenuates one specific speaker

## Files to create / modify

**New (server):**
- `crates/farder-server/src/voice.rs` (state struct, datagram handler, speaking ticker, helpers)

**Modified (server):**
- `crates/farder-server/src/state.rs` (add `voice: VoiceState` field)
- `crates/farder-server/src/connection.rs` (datagram receive loop spawned per connection)
- `crates/farder-server/src/handlers.rs` (StartVoice, StopVoice, SetVoiceMute, SetVoiceDeafen arms; DM ringing logic)
- `crates/farder-server/src/main.rs` (spawn speaking-state ticker on startup)
- `crates/farder-server/src/lib.rs` (`pub mod voice;`)

**Modified (protocol):**
- `crates/farder-protocol/src/server.rs` (4 new requests, 3 new events)

**New (client Rust):**
- `client/src-tauri/src/voice.rs` (audio engine: capture/encode/recv/playback threads + VAD/PTT/volume/device control)

**Modified (client Rust):**
- `client/src-tauri/src/commands.rs` (~13 new Tauri commands wrapping `voice::*`)
- `client/src-tauri/src/main.rs` (register new commands)
- `client/src-tauri/src/bridge.rs` (3 new event arms)
- `client/src-tauri/Cargo.toml` (add `audiopus`)

**New (client TS):**
- `client/src/components/VoiceControlBar.tsx`
- `client/src/components/IncomingCallModal.tsx`
- `client/src/components/VoiceSettings.tsx`
- `client/public/sounds/ringtone.wav` (bundled, ~10KB)

**Modified (client TS):**
- `client/src/lib/tauri-bridge.ts` (~13 new function exports + types)
- `client/src/components/ChannelSidebar.tsx` (speaking ring, mic/headphones icons on voice members)
- `client/src/components/MemberContextMenu.tsx` (Volume submenu)
- `client/src/components/AppearanceSettings.tsx` (Voice tab)
- `client/src/components/AppShell.tsx` (render IncomingCallModal)
- `client/src/hooks/useServerEvents.ts` (3 new event listeners)
- `client/src/context/ServerContext.tsx` (voiceSpeakingPks set, voiceCallIncoming reducer)

**Modified (docs):**
- `CHANGELOG.md`

## Backwards compatibility

- New permission bits: none required (voice uses existing CONNECT/SPEAK perms which are already declared but unused).
- New protocol variants tolerated by old clients via existing `#[serde(other)]`-style fallthroughs.
- Datagrams are silently dropped by old servers that don't handle them (Quinn's default datagram handler is no-op if not consumed).
- Old client connecting to new server: works for chat. StartVoice request gets "unknown variant" error gracefully if old client sends it (won't happen — old clients don't have the request).

## Implementation phasing

The plan will sequence ~12-18 tasks. Suggested phases for review checkpoints:

1. **Server foundation** (4 tasks): protocol additions, voice state struct, datagram parser/validator, speaking-state ticker
2. **Server handlers** (3 tasks): StartVoice/StopVoice/SetVoiceMute/SetVoiceDeafen, DM ringing logic, fanout loop in connection.rs
3. **Client Rust audio engine** (4 tasks): cpal setup + capture thread, Opus encode/decode + VAD, datagram send/recv, mixer + playback
4. **Client Tauri commands + bridge** (2 tasks): wrappers + event emission
5. **Client UI** (5 tasks): voice control bar, channel sidebar speaking indicators, member volume menu, incoming call modal, voice settings tab
6. **Smoke + CHANGELOG** (1 task)

## Acceptance criteria

- Two clients in a voice channel can hear each other with sub-200ms perceived latency.
- Mute/deafen work: muted users emit no frames; deafened users receive no frames.
- DM 1:1 calls trigger an incoming-call modal with ringtone on the callee, dismissable by Accept (joins voice) or Decline (server stops ringing).
- Speaking indicator glows green around speakers' avatars across all viewers, synchronized within ~200ms.
- VAD and PTT modes are user-selectable per-server in Settings; PTT key is configurable.
- Per-user volume sliders attenuate individual speakers without affecting overall master volume.
- Input/output device pickers populate from cpal and persist across restart.
- No `getUserMedia` permission prompt — capture is via cpal, OS-level mic access only.
- Server logs do not contain audio data at any log level.
- Server CPU usage under 5% with 8 concurrent listeners on one speaker.
