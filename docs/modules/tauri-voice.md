# Voice engine (client-side)

> **File(s):** `client/src-tauri/src/voice/mod.rs`, `voice/send.rs`, `voice/recv.rs`, `voice/mixer.rs`, `voice/gate.rs`, `voice/jitter.rs`, `voice/apm.rs`, `client/src-tauri/src/audio_cpal.rs`, `client/src-tauri/src/audio.rs`
> **Layer:** Voice engine
> **Last reviewed:** 2026-06-04

## Purpose

The voice engine is the client-side subsystem that moves audio between the
user's microphone/speakers and a voice channel on the Farder server.
`VoiceController` (in `mod.rs`) owns the call lifecycle and all shared state;
the supporting files implement the audio pipeline as independent, composable
pieces. The engine standardises internally on **48 kHz mono Opus** (960 samples
= 20 ms per frame); the `audio_cpal.rs` layer adapts to whatever sample format,
channel count, and rate the real device supports. The engine does NOT handle
network transport (QUIC), server-side routing, or the Tauri command layer — those
belong to `bridge.rs` and `commands.rs`.

---

## Data types

### `VoiceState`

The snapshot that is serialised into every `voice://state-changed` payload. It
tells the UI everything it needs to render the voice HUD: which channel the
local user is in, local mute/deafen/transmit flags, and the list of peers with
their per-peer mute/deafen/speaking flags.

| Field | Type | Meaning |
|---|---|---|
| `channel_id` | `Option<[u8; 16]>` | `None` when not in a call; the 16-byte channel ID otherwise |
| `muted` | `bool` | Local mic is muted (no audio sent) |
| `deafened` | `bool` | Local playback is muted; also implies `muted` |
| `transmitting` | `bool` | PTT key is currently held (ignored in Open Mic mode) |
| `peers` | `Vec<VoicePeer>` | Live peers in the channel |

### `VoicePeer`

One remote participant as seen by the local client.

| Field | Type | Meaning |
|---|---|---|
| `pubkey` | `PublicKey` | The peer's long-lived identity key (`"vk_<hex>"` form in JSON) |
| `speaking` | `bool` | Server sent a `TrackActivityChanged(active=true)` for this peer |
| `muted` | `bool` | Peer self-muted (relayed via `StreamStateChanged`) |
| `deafened` | `bool` | Peer self-deafened (relayed via `StreamStateChanged`) |

### `VoiceMode`

Selects the send-path gate at join time.

| Variant | Behaviour |
|---|---|
| `OpenMic` | Every captured frame is forwarded to the encoder (gate is always open) |
| `PushToTalk` | Frames are only forwarded while the `transmit` `AtomicBool` is `true` |

### `JoinConfig`

Per-join settings resolved from the user's preferences by the command layer and
injected into `join_with_config`. Keeping it separate from `join`'s signature
makes tests straightforward — they construct a `JoinConfig::default()` and do
not need to touch the settings file.

| Field | Type | Meaning |
|---|---|---|
| `mode` | `VoiceMode` | `OpenMic` or `PushToTalk` |
| `peer_volumes` | `HashMap<String, f32>` | Saved per-peer gains (pubkey hex → `0.0`–`2.0`); absent peer defaults to `1.0` |
| `connection` | `Option<quinn::Connection>` | Live QUIC connection used to poll path stats for `voice://connection-quality`; `None` in tests |

---

## Public interface — `VoiceController`

### `VoiceController::new(app: tauri::AppHandle) -> Self`

**What it does:** production constructor; wraps the Tauri app handle in a
`TauriEmitter` and uses the real `AudioPipelineFactory` (cpal + Opus).
**Side effects:** none at construction time; audio threads start on `join`.
**Connects to:** called once by `main.rs` to create the controller stored in
Tauri's managed state.

---

### `join(channel_id: u64, server: Arc<dyn ServerSession>) -> Result<(), String>`

**What it does:** convenience wrapper around `join_with_config` with
`JoinConfig::default()` (Open Mic, no saved volumes, no quality poller). Kept
for backward-compatibility with tests; real code from the command layer calls
`join_with_config`.

---

### `join_with_config(channel_id, server, config: JoinConfig) -> Result<(), String>`

**What it does:** performs the full call-setup sequence for a voice channel:

1. Auto-leaves any existing active call (so double-join is safe).
2. Calls `server.join_stream(channel_id)` to get a `session_id` from the server.
3. Derives a fresh per-call `stream_key` via `farder_crypto::media::derive_stream_key`, fetches current channel participants from `server.get_media_state`, wraps the stream key for each peer, and calls `server.offer_stream_key`.
4. Spawns the audio pipeline via `AudioPipelineFactory::spawn` (capture + APM + gate → Opus encode → send; mixer → playback).
5. Calls `server.enable_track(Audio)` to signal readiness to peers.
6. Spawns a speaking-event forwarder task that watches a `tokio::watch` channel and emits `voice://local-speaking` whenever the local RMS crosses the threshold.
7. If `config.connection` is `Some`, spawns a 1-second-interval poller that reads QUIC path stats and emits `voice://connection-quality`.
8. Commits the new `ActiveCall` to `Inner.active` and emits `voice://state-changed`.

**Returns:** `Ok(())` on success, or a `String` error if any step fails (the
error string propagates to the frontend via the command layer).
**Side effects:** starts audio capture and playback threads; spawns several
tokio tasks; emits `voice://state-changed`.
**Connects to:** `commands.rs` → `join_voice`; `AudioPipelineFactory::spawn`;
`farder_crypto::media`; `bridge.rs` routes `ServerEvent`s to the `on_*` methods
while the call is live.

---

### `leave() -> Result<(), String>`

**What it does:** tears down the active call in two phases. Phase 1 takes the
`ActiveCall` out of `inner.active` under the lock (so concurrent callers see no
active call). Phase 2 (outside the lock) calls `server.disable_track(Audio)`
and `server.leave_stream`, stops the audio pipeline (which closes the channels
causing the send and mixer threads to exit), aborts the quality poller, aborts
all peer recv tasks, and unregisters their dispatcher routes. Phase 3
re-acquires the lock to reset `VoiceState` to idle and clears the mute/deafen/
transmit atomics, then emits `voice://state-changed`. Calling `leave` when no
call is active is a safe no-op that still emits the cleared state.
**Side effects:** sends network requests (best-effort, errors ignored); stops
audio; emits `voice://state-changed`.
**Connects to:** called by `commands.rs` → `leave_voice`; also called
implicitly by `join_with_config` when joining a second channel.

---

### `set_mute(muted: bool) -> Result<(), String>`

**What it does:** stores the new mute value into the `muted` `AtomicBool` (read
lock-free by the send task on every frame), updates `VoiceState.muted`, notifies
the server via `server.set_mute` (best-effort; errors are logged but not
returned), and emits `voice://state-changed`.
**Side effects:** atomic write; one server round-trip; emits `voice://state-changed`.
**Connects to:** `commands.rs` → `voice_set_mute`.

---

### `set_deafen(deafened: bool) -> Result<(), String>`

**What it does:** mirrors `set_mute` but with deafen semantics: deafening also
forces `muted = true` (and saves the pre-deafen mute state in
`inner.pre_deafen_muted`); un-deafening restores the saved mute state rather
than hard-setting it to `false`. Notifies the server via `server.set_deafen`.
Emits `voice://state-changed`.
**Side effects:** atomic writes to both `muted` and `deafened`; one server
round-trip; emits `voice://state-changed`.
**Connects to:** `commands.rs` → `voice_set_deafen`.

---

### `toggle_transmit() -> bool`

**What it does:** flips the `transmit` `AtomicBool` (the PTT key state) and
re-emits `voice://state-changed`. Returns the new value (`true` = transmitting).
In Open Mic mode the gate ignores this flag entirely, so the call is harmless.
**Side effects:** atomic write; emits `voice://state-changed`.
**Connects to:** `commands.rs` → `voice_toggle_transmit` (bound to a hot key).

---

### `set_peer_volume(pubkey_hex: String, volume: f32) -> Result<(), String>`

**What it does:** clamps `volume` to `[0.0, 2.0]` and, if a peer matching
`pubkey_hex` is currently in the call, updates its live mixer gain atomically
(stored as `f32::to_bits` in an `AtomicU32` in `PeerRings`). The change takes
effect on the mixer's next frame tick without any lock beyond the existing
`PeerRings` mutex. Persistence (writing to the settings file) is handled by the
command layer; this method only updates the in-memory gain.
**Side effects:** atomic write to the peer's gain; no event emitted.
**Connects to:** `commands.rs` → `voice_set_peer_volume`.

---

### `peer_pubkey_for(session_id: &SessionId) -> Option<PublicKey>`

**What it does:** looks up the long-lived public key stored for a given
`session_id` from the `peer_keys` map (populated by `on_stream_key_offer`).
**Returns:** `Some(PublicKey)` if the key offer has been processed, `None`
otherwise.
**Connects to:** called by `bridge.rs` to resolve the pubkey before calling
`on_peer_track_enabled`, because `TrackEnabled` only carries `session_id`.

---

### `on_stream_key_offer(session_id, sender_pubkey, wrapped_key)`

**What it does:** unwraps the encrypted per-call stream key using the local
keypair and the sender's public key via
`farder_crypto::media::unwrap_stream_key`. On success, stores
`(stream_key, sender_pubkey)` in `ActiveCall.peer_keys` keyed by `session_id`.
On failure (auth error), logs and silently drops — the matching `TrackEnabled`
will later fail to find the key and log a warning.
**Side effects:** writes to `peer_keys`.
**Connects to:** `bridge.rs` routes `ServerEvent::StreamKeyOffer` here.

---

### `on_peer_track_enabled(session_id, peer_pubkey, kind: TrackKind)`

**What it does:** called when a remote peer enables their audio track. No-ops for
non-audio kinds. Looks up the peer's stream key in `peer_keys` (which must
already be present from `on_stream_key_offer`). Creates a `PeerPcmRing`, seeds
its gain from `peer_volumes` (or defaults to `1.0`), inserts the ring into
`PeerRings` (so the mixer starts pulling from it), spawns a `RecvTask` for this
peer, and registers the recv task's channel in the `MediaInboundDispatcher` so
inbound datagrams are routed to it. Adds a `VoicePeer` to `VoiceState.peers`
(seeding `muted`/`deafened` from `peer_status` if a `StreamJoined` arrived
earlier). Emits `voice://state-changed`.
**Side effects:** inserts ring into `PeerRings`; spawns async recv task; registers
dispatcher route; emits `voice://state-changed`.
**Connects to:** `bridge.rs`; `voice/recv.rs`; `MediaInboundDispatcher`.

---

### `on_peer_track_disabled(session_id)`

**What it does:** tears down a single peer's recv pipeline: aborts its recv
task, removes its ring from `PeerRings`, unregisters its dispatcher route, and
removes the corresponding `VoicePeer` from `VoiceState.peers`. Also clears any
stale `peer_status` seed for the session so it cannot leak into a future
re-registration on the same `session_id`. Emits `voice://state-changed`.
**Side effects:** task abort; ring and dispatcher cleanup; emits `voice://state-changed`.
**Connects to:** `bridge.rs` routes `ServerEvent::TrackDisabled(Audio)` here.

---

### `on_peer_stream_left(session_id)`

**What it does:** delegates directly to `on_peer_track_disabled`. Both
`TrackDisabled` and `StreamLeft` server events require the same controller-side
teardown.
**Connects to:** `bridge.rs` routes `ServerEvent::StreamLeft` here.

---

### `on_peer_stream_joined(session_id, muted, deafened)`

**What it does:** stores the peer's initial mute/deafen state into
`ActiveCall.peer_status` so it is available when the matching `TrackEnabled`
arrives and creates the `VoicePeer`. This is needed because `StreamJoined` (with
mute state) and `TrackEnabled` (which triggers peer registration) arrive as
separate server events and the order is not guaranteed.
**Side effects:** writes to `peer_status`.
**Connects to:** `bridge.rs` routes `ServerEvent::StreamJoined` here.

---

### `on_peer_stream_state(session_id, muted, deafened)`

**What it does:** updates the live `VoicePeer`'s mute/deafen fields and also
updates the `peer_status` seed map (so a peer that hasn't been registered yet
still picks up the latest state). Emits `voice://state-changed`.
**Side effects:** mutates `VoiceState.peers` and `peer_status`; emits `voice://state-changed`.
**Connects to:** `bridge.rs` routes `ServerEvent::StreamStateChanged` here.

---

### `on_peer_activity(session_id, kind: TrackKind, active: bool)`

**What it does:** no-ops for non-audio kinds. Flips the `speaking` flag on the
matching `VoicePeer` in `VoiceState.peers`. Emits `voice://peer-speaking` with
the peer's pubkey hex and the new activity value (does NOT emit
`voice://state-changed` — speaking is high-frequency and the UI has a separate
listener for it).
**Side effects:** mutates `VoiceState.peers`; emits `voice://peer-speaking`.
**Connects to:** `bridge.rs` routes `ServerEvent::TrackActivityChanged(Audio)` here.

---

## The audio pipeline in detail

```
Microphone
  │ (cpal callback)
  ▼
audio_cpal: downmix → resample to 48 kHz → PcmChunk(960 f32 mono)
  │ std::mpsc::Receiver<PcmChunk>
  ▼
send.rs: AEC render reference feed → APM (no-op v1) → gate check →
         mute check → Opus encode → seal_audio_packet_to_wire →
         datagram_sink → QUIC (server.send_datagram)

──── network (per peer) ────────────────────────────────────────────

QUIC datagram
  │ (MediaInboundDispatcher routes by session_id)
  ▼
recv.rs: open_audio_wire_frame (authenticate + decrypt) →
         JitterBuffer (3-slot, seq-keyed) → OpusDecode or PLC →
         PeerPcmRing.push_frame

  │ (mixer drains on its own cadence)
  ▼
mixer.rs: pop each PeerPcmRing × per-peer gain → sum → soft_clip →
          PcmChunk → SyncSender<PcmChunk> + AEC render reference write

  │ std::mpsc::SyncSender<PcmChunk>
  ▼
audio_cpal: resample from 48 kHz → upmix → device sample format →
            cpal playback callback → speakers
```

### `send.rs` — `SendTaskConfig` / `run`

Runs on a `spawn_blocking` thread. On each 960-sample frame arriving from the
capture channel it:

1. Feeds the most recent mixed playback buffer (`aec_ref`) to `apm.process_render` as the echo cancellation reference.
2. Passes the capture frame through `apm.process_capture` (no-op in v1).
3. Computes RMS; emits `speaking=true/false` on the `watch::Sender` with a 15-frame (~300 ms) hangover so the indicator does not flicker.
4. Checks the `GateMode` — Open always passes; PTT only passes while the `transmit` atomic is `true`.
5. Checks the `muted` atomic — muted frames are dropped post-gate (wasted APM/gate CPU, acknowledged in the design spec).
6. Encodes with `OpusEncoder` at 48 kHz mono.
7. Calls `farder_crypto::media::seal_audio_packet_to_wire` (authenticates + encrypts the Opus packet, embeds `seq`, `session_id`, `speaker_pk`).
8. Calls the `datagram_sink` closure (which calls `server.send_datagram`).

The loop exits when the `pcm_rx` channel closes (capture stopped on leave).

### `recv.rs` — `RecvTaskConfig` / `run` + `PeerPcmRing`

One async task per remote peer. On each datagram arriving from `MediaInboundDispatcher`:

1. Skips if `deafened` is `true`.
2. Calls `farder_crypto::media::open_audio_wire_frame` to authenticate and decrypt.
3. Inserts the Opus packet into a `JitterBuffer`; pops one slot (may return `None` for a gap).
4. Decodes with `OpusDecoder`; falls back to PLC (`decode_plc`) on a gap.
5. Pushes the decoded PCM into the peer's `PeerPcmRing`.

`PeerPcmRing` is a fixed-capacity circular buffer (10 frames = 200 ms). `push_frame` drops the oldest sample on overflow; `pop_frame` pads with silence on underflow. The mixer calls `pop_frame` on its own cadence.

### `mixer.rs` — `MixerTaskConfig` / `run`

Runs on a `spawn_blocking` thread. Loops forever, each iteration:

1. Locks `PeerRings`, pops one frame from each registered `PeerPcmRing`, multiplies by the per-peer gain (`f32::from_bits(AtomicU32.load(...))`).
2. Sums all peers into one 960-sample mono accumulator.
3. Applies `soft_clip(x) = x / (1 + |x|)` to keep summed audio in `(-1, 1)`.
4. Writes the mixed buffer into `aec_ref` (shared with the send task for AEC).
5. Sends the `PcmChunk` to the playback `SyncSender`; the backpressure of the bounded channel provides pacing. The loop exits when the sender returns an error (playback stopped on leave).

With no peers registered, `mix_one_frame` emits a silence frame — the mixer keeps running throughout the call whether or not anyone is speaking.

### `gate.rs` — `GateMode`

A lightweight enum consulted by the send task before encoding.

| Variant | `pass()` returns |
|---|---|
| `Open` | Always `true` |
| `Vad(VadConfig)` | Always `true` (v1 stub; VAD logic not yet implemented) |
| `Ptt(Arc<AtomicBool>)` | The current value of the shared transmit atomic |

`gate_for_mode` in `mod.rs` constructs the right variant from the join-time `VoiceMode` and the shared `transmit` atomic.

### `jitter.rs` — `JitterBuffer`

A 3-slot (`JITTER_DEPTH = 3`) fixed-depth jitter buffer keyed by `seq` (a `u64`
from the wire frame header). Insert: places the packet at `seq - head_seq`; drops
stale (below head after first pop), duplicate, or overflow (far-future — advances
the window, discarding older slots). Pop: returns the front slot (`Some(pkt)`)
or `None` (gap → caller invokes PLC). The 3-slot depth is enough to reorder
within ~60 ms of network jitter.

### `apm.rs` — `AudioProcessor`

A pluggable capture/render processing stage. v1 always constructs the no-op
fallback (`fallback: true`). `process_render` and `process_capture` are called
by the send task on every frame but do nothing yet. The interface is designed so
a future WebRTC-APM integration (blocked on `libwebrtc-audio-processing-2`) can
be dropped in behind the same API without touching the send task.

---

## The cpal backend (`audio_cpal.rs` + `audio.rs`)

### `AudioBackend` trait (`audio.rs`)

Defines the interface used by `AudioPipelineFactory`: `start_capture(device_id, format)` returns a `std::mpsc::Receiver<PcmChunk>`; `start_playback(device_id, format)` returns a `std::mpsc::SyncSender<PcmChunk>`. Both take an `AudioFormat` specifying sample rate, channel count, and chunk size. `stop_capture` / `stop_playback` stop the streams. `enumerate_input/output_devices` list available hardware.

`make_audio_backend()` (in `audio.rs`) selects the implementation at startup:
- `FARDER_AUDIO_BACKEND=mock` → `MockAudioBackend` (sine-wave source, /dev/null sink; used in CI and WSL).
- `FARDER_AUDIO_BACKEND=real` → `CpalAudioBackend` (forced even without hardware).
- Unset → `CpalAudioBackend` if at least one input device exists; otherwise `MockAudioBackend` with a one-time warning.

### `CpalAudioBackend` (`audio_cpal.rs`)

The real backend. All cpal objects (`Host`, `Stream`) live on a **dedicated OS thread** named `"farder-audio"`. This is required because cpal streams are `!Send` — they must be created and dropped on the same thread, and under tokio's multi-threaded scheduler there is no guarantee that a struct stored in shared state is accessed from the same thread each time. The backend's public methods send `AudioCommand` variants (enum with a one-shot reply channel) to that thread and block for the reply.

**Capture pipeline on the audio thread:**

1. `pick_input_device` selects the device by name or the OS default.
2. `choose_stream_config` ranks the device's supported configs: prefers exact channel match (then stereo, then any), prefers a config whose rate range covers 48 kHz (no resampling needed), prefers F32 sample format (then I16/I32/U16). Falls back to the device's lowest native rate if 48 kHz is not supported.
3. A typed `run_input_stream<T>` builds the cpal input stream; its callback converts device samples to f32 via `cpal::FromSample`.
4. Inside the callback: multi-channel frames are downmixed to mono (`downmix_frame_to_mono` averages all channels); if the device rate differs from 48 kHz, `LinearResampler` resamples to 48 kHz; samples accumulate into 960-sample chunks which are sent via a bounded `sync_channel` (backpressure capacity 8 chunks = 160 ms).

**Playback pipeline on the audio thread:**

1. `pick_output_device` and `choose_stream_config` mirror the capture side.
2. `run_output_stream<T>` builds the cpal output stream; its callback pulls from a `SyncSender` (provided to the mixer), resamples from 48 kHz to device rate if needed, upmixes mono to the device channel count (`upmix_mono_into` replicates the sample), and converts to the device sample type. Underrun fills with silence.

**`LinearResampler`:** stateful linear interpolation, mono only. Maintains phase continuity across chunk boundaries (keeps one `prev` sample between calls). Adequate for voice (not studio-grade). A 1-sample streaming delay is inherent; identity rate produces exactly the input.

---

## State it owns (`VoiceController`)

| Field | Type | What it tracks |
|---|---|---|
| `inner` | `Arc<Mutex<Inner>>` | All mutable voice state; lock is never held across `.await` beyond the minimum needed |
| `Inner.state` | `VoiceState` | Serialisable snapshot emitted on every state-changing event |
| `Inner.muted` | `Arc<AtomicBool>` | Consulted lock-free by the send task every frame |
| `Inner.deafened` | `Arc<AtomicBool>` | Consulted lock-free by each recv task every datagram |
| `Inner.transmit` | `Arc<AtomicBool>` | PTT flag; consulted lock-free by `GateMode::Ptt` in the send task |
| `Inner.pre_deafen_muted` | `bool` | Mute state saved when deafening, restored on un-deafen |
| `Inner.active` | `Option<ActiveCall>` | `None` when idle; `Some` during a call |
| `ActiveCall.server` | `Arc<dyn ServerSession>` | The server I/O interface for this call |
| `ActiveCall.pipeline` | `Option<Box<dyn VoicePipelineHandle>>` | The live audio pipeline; dropping it stops send + mixer |
| `ActiveCall.peer_rings` | `PeerRings` | `Arc<Mutex<HashMap<SessionId, (Arc<PeerPcmRing>, Arc<AtomicU32>)>>>` — mixer reads this |
| `ActiveCall.peers` | `HashMap<SessionId, PeerEntry>` | Live recv tasks + their datagram senders |
| `ActiveCall.peer_keys` | `HashMap<SessionId, ([u8; 32], PublicKey)>` | Unwrapped stream keys waiting for `TrackEnabled` |
| `ActiveCall.peer_status` | `HashMap<SessionId, (bool, bool)>` | Mute/deafen seed; used when `StreamJoined` arrives before `TrackEnabled` |
| `ActiveCall.peer_volumes` | `HashMap<String, f32>` | Saved per-peer gains from settings; seeds new peer rings |
| `ActiveCall.quality_poller` | `Option<JoinHandle<()>>` | Aborted on leave; `None` if no QUIC connection supplied |

---

## Events emitted

| Event name | Payload keys | When | Who listens |
|---|---|---|---|
| `voice://state-changed` | Full `VoiceState` JSON (`channel_id`, `muted`, `deafened`, `transmitting`, `peers`) | Join, leave, set_mute, set_deafen, toggle_transmit, peer register/unregister/state-change | `useVoice.ts` |
| `voice://local-speaking` | `{ "speaking": bool }` | Local RMS crosses the (adjustable) sensitivity threshold, with 300 ms hangover; muted/deafened never speaks | `useVoice.ts` |
| `voice://input-level` | `{ "level": f32 }` | Raw mic RMS, ~10x/sec while a call is active | `VoiceSettings.tsx` (mic-sensitivity meter) |
| `voice://peer-speaking` | `{ "session_id": [u8;16], "pubkey": String, "active": bool }` | `TrackActivityChanged` arrives for an audio track | `useVoice.ts` |
| `voice://connection-quality` | `{ "rtt_ms": f64, "loss_pct": f64 }` | Every ~1 s while a call is active (only if `JoinConfig.connection` is `Some`) | `useVoice.ts` |

---

## Events / requests consumed

| Source event | Source | Controller call |
|---|---|---|
| `ServerEvent::StreamKeyOffer` | `bridge.rs` | `on_stream_key_offer` |
| `ServerEvent::TrackEnabled(Audio)` | `bridge.rs` | `on_peer_track_enabled` (pubkey resolved first via `peer_pubkey_for`) |
| `ServerEvent::TrackDisabled(Audio)` | `bridge.rs` | `on_peer_track_disabled` |
| `ServerEvent::StreamLeft` | `bridge.rs` | `on_peer_stream_left` |
| `ServerEvent::StreamJoined` | `bridge.rs` | `on_peer_stream_joined` |
| `ServerEvent::StreamStateChanged` | `bridge.rs` | `on_peer_stream_state` |
| `ServerEvent::TrackActivityChanged(Audio)` | `bridge.rs` | `on_peer_activity` |

---

## Integration map

- **`bridge.rs`** — routes all seven media `ServerEvent` variants to the `on_*` methods above. Also provides the `ServerSession` concrete impl (`bridge::ServerSessionImpl`) that backs the production `VoiceController`.
- **`commands.rs`** — calls `join_with_config`, `leave`, `set_mute`, `set_deafen`, `toggle_transmit`, `set_peer_volume` via Tauri commands. Resolves `JoinConfig` from settings before calling `join_with_config`.
- **`useVoice.ts` (frontend)** — listens on all four `voice://*` events and maintains local UI state (call status, peer list, speaking indicators, connection quality badge).
- **`farder_crypto::media`** — `derive_stream_key`, `wrap_stream_key_for_peer`, `unwrap_stream_key`, `seal_audio_packet_to_wire`, `open_audio_wire_frame` — all crypto for the media path lives here, not in the engine.
- **`audio.rs` / `audio_cpal.rs`** — `AudioPipelineFactory::spawn` calls `make_audio_backend()` to get a backend, then calls `start_capture` and `start_playback` on it to get the channels that the send and mixer tasks use.

---

## Known gotchas

- **`StreamKeyOffer` must precede `TrackEnabled`:** `on_peer_track_enabled` looks up the stream key in `peer_keys`. If `TrackEnabled` arrives first (the server should never do this, but defensive code matters), the peer is silently skipped and a warning is logged. The peer will never be registered for that session.

- **Deafen restores pre-deafen mute, not `false`:** calling `set_deafen(false)` reads `inner.pre_deafen_muted` — it does NOT unconditionally unmute. If the user was muted before they deafened, they remain muted after un-deafening. This is intentional (Discord-style behaviour) but surprises newcomers.

- **`leave` is two-phase to avoid holding the lock over network I/O:** `inner.active.take()` is done under the lock; the server calls (`disable_track`, `leave_stream`) and audio teardown happen outside it. A window exists between the two phases where `inner.active` is `None` but the audio is still running — do not rely on `active.is_some()` as a proxy for "audio is running".

- **The cpal audio thread is `!Send`:** never attempt to move a `cpal::Stream` out of `audio_cpal.rs` or store one in Tauri managed state. The entire point of the dedicated thread + command channel design is to guarantee the stream lives and dies on one thread.

- **`muted` is checked after gate and APM in the send task:** when muted, the capture frame has already gone through APM and gate processing (wasted CPU). This is noted in the design spec as a known inefficiency acceptable for v1.

- **Mixer paces itself via backpressure, not a timer:** the `playback_tx.send()` call blocks when the `SyncSender` buffer fills. If the cpal playback callback stops pulling (e.g. a device error), the mixer stalls and the send task will also stall because `pcm_rx` accumulates. This avoids an explicit sleep loop but means a broken output device eventually blocks capture too.

- **`VoiceMode::Vad` is a stub:** `GateMode::Vad` always returns `true` from `pass()`. It exists so the enum variants are wired; real VAD logic is a future task.
