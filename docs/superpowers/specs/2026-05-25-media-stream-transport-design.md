# Media Stream Transport — Design

**Status:** Drafted 2026-05-25
**Scope:** Farder protocol (`crates/farder-protocol`), server media routing (`crates/farder-server`), and client IPC bridge stubs (`client/src-tauri`). No client-side capture/playback or codec work — that ships with sub-project #3 (voice) and #4 (screensharing).
**Position in roadmap:** Sub-project #2 of the audio + screensharing track. Generalizes the existing voice-only protocol into a typed media-stream layer so voice and screensharing both consume one transport surface.

## Goal

Replace the voice-only `VC.*` protocol arms (`StartVoice` / `StopVoice` / `SetVoiceMute` / `SetVoiceDeafen` + the matching events) with a stream-of-tracks model: one logical media stream per user per channel, carrying independently-controlled `Audio` and `Video` tracks. Server stays a dumb forwarder but gains per-(user, channel, track) token-bucket bandwidth caps so a single misbehaving client can't saturate everyone. Frame wire format extends from 42 bytes to 44 bytes by reserving two bytes (`track_id`, `codec_id`) for future multi-track / multi-codec needs without breaking the wire format.

## Non-Goals

- **Codec implementations.** Opus encode/decode (audio) ships with sub-project #3 (voice Phase 3). VP8 encode/decode (video) ships with sub-project #4 (screensharing). v1 of the transport just routes opaque byte payloads tagged by type.
- **Client capture/playback wiring.** The MediaBackend traits from sub-project #1 get consumed by #3 and #4; #2 just exposes Tauri command surface that those sub-projects will call.
- **Reverse-mute** (this client doesn't want to receive a specific peer's audio). Out of scope; deafen is the only client-side receive control.
- **Codec negotiation.** Audio is always Opus, video is always VP8 in v1. `codec_id` byte is reserved and ignored on receive.
- **Multi-track per kind.** A user can have one audio track and one video track per channel; not two cameras or camera+screen. `track_id` byte is reserved and ignored on receive (clients always send `0`).
- **Per-peer bandwidth feedback / SFU-style adaptive routing.** Server caps are static per-server config; no per-peer downscaling.
- **Wire compatibility with the existing voice frame format** (version `0x01`). Voice client (Phase 3) never shipped, so no deployed clients can be using the old format. Server rejects `0x01` frames with a clear error after the upgrade.
- **A separate "video deafen" or "stop receiving video".** Deafen drops all incoming media of all tracks; there's no per-kind receive toggle in v1.

## Architecture

```
┌─── Client ──────────────────────────────────┐    ┌─── Server ────────────────────────────────┐
│                                              │    │                                            │
│  Tauri commands (renamed to match new arms): │    │  crates/farder-server/src/                 │
│    join_stream(channel_id)                   │    │    media_stream.rs   ← new, replaces       │
│    leave_stream()                            │    │                       voice.rs's routing   │
│    enable_track(kind)                        │    │      - parse_media_frame / build_media_   │
│    disable_track(kind)                       │    │        frame (44-byte header)              │
│    set_deafen(deafened)                      │    │      - token bucket per                    │
│    get_stream_state(channel_id)              │    │        (user, channel, track_kind)         │
│                                              │    │      - fanout: for each peer in channel    │
│  These are thin pass-throughs that emit      │    │        with stream open AND not deafened,  │
│  the corresponding ServerRequest::*Stream*   │    │        forward the frame (if cap allows)   │
│  arms via the existing request bridge.       │    │      - lifecycle events (StreamJoined,     │
│                                              │    │        StreamLeft, TrackEnabled, etc.)     │
│  No capture / no playback / no UI here.      │    │                                            │
│                                              │    │    handlers.rs - new arms for              │
│                                              │    │      JoinStream / LeaveStream /            │
│                                              │    │      EnableTrack / DisableTrack /          │
│                                              │    │      SetDeafen                             │
│                                              │    │                                            │
│                                              │    │  Datagram receive loop (existing) routes   │
│                                              │    │  to media_stream::on_frame() instead of    │
│                                              │    │  voice::on_frame()                         │
│                                              │    │                                            │
└──────────────────────────────────────────────┘    └────────────────────────────────────────────┘
```

### Why one stream per user per channel (not separate audio + video streams)

The user confirmed during brainstorming: "voice and video are pretty much always used together" — screensharing without audio is rare. A stream-of-tracks model lets `JoinStream` claim the slot once, and then `EnableTrack` toggles each track independently. One join, one leave, single source of truth for "is Alice transmitting any media right now."

### Why server-side bandwidth caps (not pure dumb forwarder)

A misbehaving screenshare client uploading 50 Mbps would saturate every other peer in the channel. Token-bucket caps per (user, track_kind) provide cheap insurance — a few hundred lines of server code that prevents an entire category of denial-of-service. The server stays a dumb forwarder in every other respect (no transcoding, no per-peer downscaling).

## Protocol — `ServerRequest`

### Removed

```rust
StartVoice { channel_id: u64 }
StopVoice
SetVoiceMute { muted: bool }
SetVoiceDeafen { deafened: bool }
```

### Added

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Audio,
    Video,
}

JoinStream { channel_id: u64 }
LeaveStream
EnableTrack { kind: TrackKind }
DisableTrack { kind: TrackKind }
SetDeafen { deafened: bool }
```

### Mappings from old → new (for documenting the migration in commits)

| Old | New |
|---|---|
| `StartVoice { channel_id }` | `JoinStream { channel_id }` then `EnableTrack { kind: Audio }` |
| `StopVoice` | `LeaveStream` (also disables every active track implicitly) |
| `SetVoiceMute { muted: true }` | `DisableTrack { kind: Audio }` |
| `SetVoiceMute { muted: false }` | `EnableTrack { kind: Audio }` |
| `SetVoiceDeafen { deafened }` | `SetDeafen { deafened }` |

Note: existing `JoinVoice` / `LeaveVoice` / `GetVoiceState` / `VoiceStateResp` (the lobby-style presence arms, separate from the transmit-state arms above) are **preserved**. They handle "Alice is in the voice channel but hasn't started transmitting yet" which is a different concept. We rename them to `JoinChannelMedia` / `LeaveChannelMedia` / `GetMediaState` / `MediaStateResp` for consistency, but the semantics don't change. (Detail in the implementation plan.)

## Protocol — `ServerEvent`

### Removed

```rust
VoiceJoined / VoiceLeft / VoiceSpeakingChanged
VoiceCallIncoming / VoiceCallEnded
```

### Added

```rust
StreamJoined {
    channel_id: u64,
    public_key: PublicKey,
    display_name: String,
    active_tracks: Vec<TrackKind>,   // empty when first joined; populated on TrackEnabled
}
StreamLeft {
    channel_id: u64,
    public_key: PublicKey,
}
TrackEnabled {
    channel_id: u64,
    public_key: PublicKey,
    kind: TrackKind,
}
TrackDisabled {
    channel_id: u64,
    public_key: PublicKey,
    kind: TrackKind,
}
TrackActivityChanged {
    channel_id: u64,
    public_key: PublicKey,
    kind: TrackKind,
    active: bool,    // for Audio: speaking indicator; for Video: "frames are flowing"
}
StreamCallIncoming {
    channel_id: u64,
    caller: PublicKey,
    caller_name: String,
}
StreamCallEnded {
    channel_id: u64,
}
```

`TrackActivityChanged` replaces `VoiceSpeakingChanged`. For audio, the server runs the same 5 Hz speaking-state ticker as today, just emitting a track-aware event. For video, the ticker observes whether VP8 frames are flowing and emits `active: true/false` on transitions (mostly cosmetic — UI can decorate the user as "currently sharing screen").

### Renames

| Old event | New event |
|---|---|
| `VoiceJoined` | `MediaJoined` (preserved as renamed event for "Alice entered the voice channel") |
| `VoiceLeft` | `MediaLeft` |

These are the lobby-style presence events (paired with `JoinChannelMedia` / `LeaveChannelMedia`), distinct from `StreamJoined` / `StreamLeft` which signal active transmission.

## Frame format

```
Offset | Field         | Size  | Notes
-------+---------------+-------+--------------------------------------------------------------
 0     | version       | 1 B   | 0x02 (incremented from voice 0x01 — wire break)
 1     | type          | 1 B   | 0x01 = Opus audio, 0x02 = VP8 video
 2     | track_id      | 1 B   | RESERVED — always 0 in v1; receivers ignore
 3     | codec_id      | 1 B   | RESERVED — always 0 in v1 (= default codec for type)
 4-11  | seq           | 8 B   | u64 big-endian, monotonic per (sender, type)
 12-43 | speaker_pk    | 32 B  | Sender's public key
 44+   | payload       | var   | Opus packet (for type=0x01) or VP8 packet (for type=0x02)
```

Total header: **44 bytes**.

### Rust types

```rust
// crates/farder-server/src/media_stream.rs  (replaces voice.rs)

pub const MEDIA_FRAME_VERSION: u8 = 0x02;
pub const MEDIA_FRAME_TYPE_AUDIO: u8 = 0x01;
pub const MEDIA_FRAME_TYPE_VIDEO: u8 = 0x02;
pub const MEDIA_FRAME_HEADER_LEN: usize = 44;

#[derive(Debug, PartialEq)]
pub struct MediaFrame<'a> {
    pub kind: TrackKind,
    pub seq: u64,
    pub speaker_pk: [u8; 32],
    pub payload: &'a [u8],
}

pub fn parse_media_frame(buf: &[u8]) -> Result<MediaFrame<'_>, MediaFrameError> {
    if buf.len() < MEDIA_FRAME_HEADER_LEN { return Err(MediaFrameError::TooShort); }
    if buf[0] != MEDIA_FRAME_VERSION { return Err(MediaFrameError::BadVersion(buf[0])); }
    let kind = match buf[1] {
        MEDIA_FRAME_TYPE_AUDIO => TrackKind::Audio,
        MEDIA_FRAME_TYPE_VIDEO => TrackKind::Video,
        other => return Err(MediaFrameError::BadType(other)),
    };
    // bytes 2 (track_id) and 3 (codec_id) reserved — ignored in v1
    let seq = u64::from_be_bytes(buf[4..12].try_into().unwrap());
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&buf[12..44]);
    Ok(MediaFrame { kind, seq, speaker_pk: pk, payload: &buf[44..] })
}

pub fn build_media_frame(
    kind: TrackKind,
    seq: u64,
    speaker_pk: &[u8; 32],
    payload: &[u8],
) -> Vec<u8> {
    let type_byte = match kind { TrackKind::Audio => MEDIA_FRAME_TYPE_AUDIO,
                                  TrackKind::Video => MEDIA_FRAME_TYPE_VIDEO };
    let mut buf = Vec::with_capacity(MEDIA_FRAME_HEADER_LEN + payload.len());
    buf.push(MEDIA_FRAME_VERSION);
    buf.push(type_byte);
    buf.push(0); // track_id reserved
    buf.push(0); // codec_id reserved
    buf.extend_from_slice(&seq.to_be_bytes());
    buf.extend_from_slice(speaker_pk);
    buf.extend_from_slice(payload);
    buf
}

#[derive(Debug, PartialEq)]
pub enum MediaFrameError {
    TooShort,
    BadVersion(u8),
    BadType(u8),
}
```

### Why version `0x02` (wire break, not extension)

Voice's existing `0x01` had the 42-byte header. Adding the two reserved bytes changes byte offsets. A v1 client built against `0x02` could not parse a `0x01` frame and vice versa. Since no client has actually shipped Phase 3 (voice client transmit/receive), there are no deployed `0x01` parsers to maintain. Cleaner to break and start fresh at `0x02`.

## Server routing — `media_stream.rs`

### State per channel

```rust
pub struct StreamState {
    // user → their active stream in this channel (None if not Joined)
    streams: HashMap<PublicKey, UserStream>,
    // user → are they deafened? (server stops forwarding TO them when true)
    deafened: HashSet<PublicKey>,
}

pub struct UserStream {
    active_tracks: HashSet<TrackKind>,   // which tracks have been EnableTrack'd
    buckets: HashMap<TrackKind, TokenBucket>,
    last_audio_frame_ms: Option<u64>,    // for speaking ticker
    last_video_frame_ms: Option<u64>,    // for video-activity ticker
}

pub struct TokenBucket {
    cap_bps: u64,           // server config
    tokens: u64,            // current bytes available
    last_refill_ms: u64,
}
```

### Frame ingress flow

1. Datagram arrives, parsed into `MediaFrame`.
2. Look up sender by `speaker_pk` in this channel.
3. If sender doesn't have a stream open in this channel → drop.
4. If `kind` is not in `active_tracks` for this user → drop.
5. Refill the user's `(kind)` token bucket based on `now - last_refill_ms`.
6. If `tokens < frame.len()` → drop (silent; increment a counter for ops logging).
7. Subtract `frame.len()` from `tokens`.
8. Update `last_(audio|video)_frame_ms`.
9. Fanout: for every peer in this channel with a stream open AND not in `deafened` AND not the sender themselves → write the frame to their QUIC datagram path.

### Speaking / activity ticker

Existing 5 Hz ticker (`speaking_state_ticker` from voice work) generalizes to walk every user's `last_(audio|video)_frame_ms` and emit `TrackActivityChanged` events on transitions. Same logic, two tracks now.

### Bandwidth defaults

```toml
# server-side config (additions to existing server.toml or equivalent)
[media]
audio_max_bps = 64000      # 64 Kbps — enough for Opus at typical voice quality
video_max_bps = 2_000_000  # 2 Mbps — enough for VP8 720p30 medium quality
```

These are reasonable defaults that match what the client will actually produce in sub-projects #3 and #4. The client encoders will be configured with target bitrates well under these caps so the buckets only ever drop in pathological cases (misconfigured client, hostile actor).

## Client-side bridge stubs

`client/src-tauri/src/commands.rs` currently exposes voice-related `#[tauri::command]` functions. We rename / restructure to match the new protocol but DO NOT implement capture/playback (that's #3 / #4):

```rust
// REMOVED:  join_voice, leave_voice, get_voice_state, start_recording, stop_recording (the voice-recording ones)
// ADDED:
#[tauri::command]
pub async fn join_stream(serverId: String, channelId: u64) -> Result<(), String> { /* request bridge */ }
pub async fn leave_stream(serverId: String) -> Result<(), String> { ... }
pub async fn enable_track(serverId: String, kind: String /* "audio" | "video" */) -> Result<(), String> { ... }
pub async fn disable_track(serverId: String, kind: String) -> Result<(), String> { ... }
pub async fn set_deafen(serverId: String, deafened: bool) -> Result<(), String> { ... }
pub async fn get_stream_state(serverId: String, channelId: u64) -> Result<StreamStateResp, String> { ... }
```

These are pure pass-throughs to the existing `bridge::send_request` machinery. No new capture, no playback, no codec. The actual `transmit_audio_frame` / `transmit_video_frame` Tauri commands (which would call `build_media_frame` and dispatch via QUIC datagrams) are deferred to sub-projects #3 / #4.

The existing voice-related commands that REMAIN: `save_temp_audio`, `start_recording`, `stop_recording` (these are for the existing local-recording feature, unrelated to the real-time media transport — keep as-is).

## Testing

### Unit (`crates/farder-server/src/media_stream.rs`)

- `parse_media_frame_audio_roundtrip` — build with `TrackKind::Audio`, parse, fields match.
- `parse_media_frame_video_roundtrip` — same for video.
- `parse_media_frame_rejects_voice_v1` — bytes starting with `0x01` (the old voice version) parse to `Err(BadVersion(0x01))`.
- `parse_media_frame_rejects_unknown_type` — `version=0x02 type=0xff …` returns `Err(BadType(0xff))`.
- `parse_media_frame_rejects_short_buffer` — buffer < 44 bytes returns `Err(TooShort)`.
- `token_bucket_passes_under_cap` — produce frames at 50% of `cap_bps` for 1 second, assert all admitted.
- `token_bucket_drops_over_cap` — produce frames at 200% of `cap_bps` for 1 second, assert ~50% dropped.
- `token_bucket_refills_over_time` — drain bucket, wait 100 ms, assert refilled by `cap_bps * 0.1`.

### Unit (`crates/farder-protocol/src/server.rs`)

- Adapt the existing `test_roundtrip_client_frame_request` to cover the new arms (`JoinStream`, `EnableTrack`, etc.).
- Adapt the existing event roundtrip test to cover the new events.

### Integration

The existing server integration tests under `crates/farder-server/tests/` likely include voice-flow tests. We adapt them:
- The `voice_join_leave` scenario becomes `stream_join_leave`.
- A new `audio_only_to_audio_plus_video` scenario verifies multi-track lifecycle.
- A new `bandwidth_cap_drops_video_under_load` scenario verifies the token-bucket drop path.

(Sub-project #2 implementation plan will enumerate the exact existing tests that need adaptation.)

### What's NOT tested in #2

- Actual codec output — sub-project #3 (Opus) and #4 (VP8) bring their own tests against synthetic input from MediaBackend mocks.
- End-to-end media smoke between two clients — that's #3 / #4 territory.

## Migration

- **Server:** `voice.rs` is **renamed** to `media_stream.rs` with its routing updated to the new protocol. The bandwidth-cap machinery is new. The 5 Hz speaking ticker generalizes to walk both audio and video activity. Existing voice arm handlers in `handlers.rs` are replaced with media arm handlers. Existing `EventTarget::Voice*` variants (`EventTarget::VoiceStateUpdate`, etc., used by the async event dispatcher) are renamed `EventTarget::Media*` 1:1 — same fanout shape, different variant names. No structural change to the dispatcher.
- **Protocol:** the `ServerRequest::Start/Stop/SetVoice{Mute,Deafen}` arms are **removed**. `ServerEvent::Voice*` variants are removed. Because Phase 3 voice client never shipped, no deployed clients use these arms, so no version negotiation is needed.
- **Client (Rust):** voice-related `#[tauri::command]` functions are renamed to match. No JS/TS changes in this sub-project — those happen with the consumer sub-projects.
- **Frame wire format:** version `0x01` rejected; clients must speak `0x02`. Again, no deployed clients use `0x01`.

## Rollout

This sub-project ships infrastructure with no UI behavior change. After landing:
- Voice (Phase 3) implements the audio track of the stream — real Opus encode/decode + MediaBackend wiring + UI for the talk indicator, mute, deafen.
- Screensharing (#4) implements the video track — real VP8 encode/decode + DisplayBackend wiring + UI for "share screen" / "stop sharing" + viewer.
- Both consume the same `JoinStream` / `EnableTrack` / frame-transport machinery.

## Future considerations (deferred)

- **Multi-track per kind** (`track_id != 0`): the reserved byte unlocks "camera + screen at the same time" when there's product demand.
- **Codec negotiation** (`codec_id != 0`): swap Opus → AAC, VP8 → AV1 without a wire break.
- **Reverse-mute** (block a specific peer's audio): client-only UX, no protocol impact, ship when requested.
- **Per-peer bandwidth feedback** (the watching peer reports "I can only handle 500 Kbps video"): real SFU territory; major undertaking, only if product needs it.
- **Recording-to-file of incoming streams**: separate feature, separate spec.

---

This spec covers exactly the protocol shape, frame format, server routing, bandwidth caps, and bridge-stub renames for sub-project #2. Actual codec wiring, client capture/playback, and UI all ship in subsequent sub-projects.
