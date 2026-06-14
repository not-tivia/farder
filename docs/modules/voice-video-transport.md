# Voice video transport (Phase C1 + C2)

> **File(s):** `client/src-tauri/src/voice/mod.rs` (dispatcher + controller + share lifecycle),
> `client/src-tauri/src/voice/send_video.rs`,
> `client/src-tauri/src/voice/recv_video.rs`,
> `client/src-tauri/src/screenshare.rs` (`run_encode_loop`),
> `client/src/components/PeerVideoTiles.tsx` (viewer),
> `crates/farder-crypto/src/media.rs` (`seal_video_frame_to_wire` / `open_video_wire_frame`)
> **Layer:** Voice engine
> **Last reviewed:** 2026-06-13

## Purpose

Phase C1 adds the encrypted video transport layer to the voice engine. It wires
the Phase A media-datagram transport (fragment / reassemble) and the Phase B
H.264 encoder to a full send-and-receive pipeline for video: seal, fragment,
dispatch, reassemble, open, and forward decrypted H.264 frames to the webview
for WebCodecs decoding.

**C1 scope: transport primitives only.** The following are NOT in scope for C1:

- The user-facing share trigger (start/stop-share commands, the UI share button)
- Deriving and offering the video stream key at join or share-start time
- Enabling the Video track (the `enable_track(Video)` call)
- Driving `VideoSender` from the Phase B capture loop
- Keyframe-on-join (forcing an IDR when a new peer joins)
- Late-joiner re-offer of the video key
- The per-peer video tile in the frontend UI

All of the above are **Phase C2** — now implemented; see the
"Phase C2 — share lifecycle + viewer" section below. Phase C1 delivered only the
primitives; this section documents how C2 wires them to the capture loop and UI.

---

## Why `(session_id, TrackKind)` — the dispatcher re-key

Before C1, `MediaInboundDispatcher` routed inbound datagrams by `session_id`
alone. That was sufficient when each peer session carried only one track (Audio).
With screensharing, a peer's same QUIC session sends BOTH audio datagrams and
video datagrams on the same connection. The cleartext outer header already
carries a `track_kind` field (Audio `0x01`, Video `0x02`); the dispatcher
therefore needed a compound key `(session_id, TrackKind)` to deliver each
datagram to the correct recv task.

### `MediaInboundDispatcher`

Routes every inbound QUIC media datagram to the right receiver channel by
parsing the cleartext outer header's `(session_id, track_kind)`.

```rust
dispatcher.register(session_id, TrackKind::Audio, audio_tx).await;
dispatcher.register(session_id, TrackKind::Video, video_tx).await;
// Each track gets its own unbounded channel; both live under the same session.
```

**`register(session_id, kind, tx)`** — inserts the route. Calling it again for
the same `(session_id, kind)` replaces the old sender.

**`unregister(session_id, kind)`** — removes the route. Called from `leave()` and
`on_peer_track_disabled`.

**`dispatch(bytes)`** — parses the outer header, looks up `(session_id, kind)`,
and sends the bytes into the matching channel. Datagrams with no matching route
are silently dropped (not an error — the peer may have disabled the track
between when the datagram was sent and when it arrived).

---

## `peer_keys` generalisation

`ActiveCall.peer_keys` was re-keyed from `HashMap<SessionId, [u8; 32]>` to
`HashMap<(SessionId, TrackKind), [u8; 32]>`. A peer's session now carries
**independent** stream keys for audio and video — the same session_id yields
two separate AEAD keys, derived and wrapped independently at stream-key-offer
time. The companion `peer_pubkeys: HashMap<SessionId, PublicKey>` stores the
long-lived identity key (populated by the first key offer for the session,
regardless of `TrackKind`).

`on_stream_key_offer(session_id, kind, sender_pubkey, wrapped_key)` unwraps the
key and stores it at `(session_id, kind)`. The `kind` parameter was added in C1;
prior to C1 the method had no `kind` param and used `SessionId` only.

---

## Crypto helpers (`farder-crypto::media`)

### `seal_video_frame_to_wire(key, seq, session_id, speaker_pk, payload) -> Result<Vec<u8>>`

Builds a 28-byte inner header (type byte `0x02` = VIDEO at offset 1; `seq` at
offsets 2-9; `session_id` at 10-25; `speaker_pk` at 26-57) then AES-256-GCM
seals `payload` with the header as AAD. Returns the complete wire frame:
`header || ciphertext || tag`.

Mirrors `seal_audio_packet_to_wire` exactly; the only difference is the type
byte (`0x02` vs `0x01`).

### `open_video_wire_frame(key, wire) -> Result<(seq, speaker_pk, plaintext)>`

Parses the inner header, verifies the type byte is `0x02` (rejects audio frames
with an error), then AES-256-GCM opens the ciphertext. Returns `(seq, speaker_pk,
plaintext)` on success. Any AEAD auth failure or wrong type byte is returned as
an error; callers drop the datagram.

---

## Video send path (`voice/send_video.rs`)

### `build_video_datagrams(stream_key, session_id, speaker_pk, seq, frame_id, frame) -> Result<Vec<Vec<u8>>>`

Pure (no I/O) function: seals one `EncodedFrame` and returns it as a list of
wire datagrams.

**Payload layout (encrypted together as one AEAD plaintext):**

```
byte 0:   keyframe flag (0x00 = delta, 0x01 = IDR/keyframe)
bytes 1+: H.264 Annex-B NAL byte stream
```

The keyframe byte is the FIRST byte of the ENCRYPTED payload — it is NOT
cleartext. The receiver recovers it only after AEAD authentication. This means
the network (relay, server) cannot observe which frames are keyframes.

After sealing, the wire frame is passed to `farder_protocol::media_datagram::fragment`
with `TrackKind::Video`. Large frames (above `DEFAULT_MAX_DGRAM_PAYLOAD`) produce
multiple datagrams; small frames (typical H.264 P-frames at low motion) produce one.

**Parameters:**
- `stream_key` — the per-call VIDEO stream key (`[u8; 32]`).
- `session_id` — local QUIC session id (`[u8; 16]`), written into the inner header.
- `speaker_pk` — local long-lived public key bytes, written into the inner header.
- `seq` — monotonically increasing AEAD nonce; never reuse for the same key.
- `frame_id` — groups the fragments of one frame so the reassembler can reassemble them.
- `frame` — the `EncodedFrame` from the Phase B encoder (`data`, `is_keyframe`, `timestamp_ms`).

**Returns:** the sealed+fragmented datagrams, or a `String` error on seal failure.

### `VideoSender`

A stateful wrapper around `build_video_datagrams` that advances `seq` and
`frame_id` automatically on each call to `send`.

**`VideoSender::new(stream_key, session_id, speaker_pk) -> Self`** — constructs
with `seq = 0` and `frame_id = 0`.

**`send(&mut self, frame: &EncodedFrame, sink: impl FnMut(Bytes))`** — seals and
fragments the frame, calls `sink` for each datagram, then increments `seq` and
`frame_id`. Encode errors are logged and the frame is dropped (live policy — one
dropped frame is preferable to stalling the pipeline).

**Connects to:** Phase C2 will drive `VideoSender::send` from the Phase B encode
loop (`run_encode_loop` in `screenshare.rs`), calling `server.send_datagram` as
the sink.

---

## Video receive path (`voice/recv_video.rs`)

### `RecvVideoConfig`

```rust
pub struct RecvVideoConfig {
    pub session_id: SessionId,
    pub stream_key: [u8; 32],       // VIDEO key for this peer
    pub datagram_rx: mpsc::UnboundedReceiver<Bytes>,
}
```

### `VideoOut`

The decoded-from-wire (still H.264-encoded) frame handed to the sink:

```rust
pub struct VideoOut {
    pub data: Vec<u8>,      // H.264 Annex-B NAL byte stream
    pub is_keyframe: bool,
    pub seq: u64,           // from the inner header
}
```

### `run(cfg: RecvVideoConfig, sink: impl FnMut(VideoOut)) -> (async, no return value)`

The video receive task. Runs until the datagram channel closes (which happens
when `leave()` drops the `VideoPeerEntry` and aborts the task, or the sender
disconnects).

**Pipeline per datagram:**

1. Parse the outer header via `OuterHeader::parse`. Invalid datagrams are skipped.
2. Pass `(header, payload)` to `Reassembler::accept`. The reassembler accumulates
   fragments by `frame_id`; returns `Some(sealed)` only when all fragments of a
   frame have arrived. Partial frames wait for subsequent datagrams.
3. Call `open_video_wire_frame(stream_key, sealed)`. Auth failures (wrong key,
   corrupt ciphertext, wrong type byte) silently drop the frame.
4. Split the plaintext: `plaintext[0]` is the keyframe flag; `plaintext[1..]` is
   the H.264 Annex-B payload.
5. Call `sink(VideoOut { data, is_keyframe, seq })`.

**No jitter buffer, no decode.** Video timing and decode are left to the WebCodecs
`VideoDecoder` on the frontend. The recv task is transport-only.

---

## Controller wiring (`voice/mod.rs`)

### `on_peer_video_track_enabled(session_id, _peer_pubkey)` (private)

Called by `on_peer_track_enabled` when `kind == TrackKind::Video`. Sets up the
full video recv pipeline for a peer:

1. Checks `call.video_peers` to avoid double-registering the same session.
2. Looks up `(session_id, TrackKind::Video)` in `call.peer_keys`. If absent
   (no video `StreamKeyOffer` arrived yet), logs a warning and returns — the
   track cannot be decrypted without the key.
3. Creates an `mpsc::UnboundedChannel`. Registers the sender with the dispatcher
   as `(session_id, TrackKind::Video)` so inbound video datagrams are routed here.
4. Spawns the video recv task (`recv_video::run`) with a sink closure that:
   - Base64-encodes `v.data` using the standard engine.
   - Emits `voice://peer-video-frame` with the payload described below.
5. Stores a `VideoPeerEntry { recv_handle, datagram_tx }` in `call.video_peers`.

### `voice://peer-video-frame` event

Emitted by the video recv task's sink closure, once per fully-decrypted,
reassembled H.264 frame.

| Field | Type | Description |
|---|---|---|
| `session` | `string` | Hex-encoded `session_id` of the sending peer (lower-case, 32 hex chars) |
| `pubkey` | `string` | Sending peer's public key as `vk_` + 64 lowercase hex chars of the sender's 32-byte public key (matches `publicKeyToString()` in the frontend) — lets the viewer label/clean up the tile by identity (Phase C2) |
| `data` | `string` | Base64-encoded H.264 Annex-B frame (SPS/PPS inline before IDR) |
| `key` | `boolean` | `true` if this is an IDR/keyframe; `false` for delta frames |
| `seq` | `number` | Frame sequence number from the inner header (u64, monotonically increasing) |

**Consumer (Phase C2):** the frontend per-peer video tile will listen for this
event, identify the peer by `session`, gate on `key` before the first delta
(key-first invariant), and feed each frame into a per-peer WebCodecs
`VideoDecoder`. The `seq` field can be used to detect gaps and request a keyframe.

### `on_peer_track_disabled(session_id, kind)` — video teardown

When `kind == TrackKind::Video`:
1. Removes the `VideoPeerEntry` from `call.video_peers`.
2. Aborts the recv task handle.
3. Spawns an async task to call `dispatcher.unregister(session_id, TrackKind::Video)`.

No `VoiceState` emission is needed for video disable (the roster only tracks
audio presence; the video UI state is Phase C2).

### `leave()` — full teardown

`leave()` drains both `call.peers` (audio) and `call.video_peers` (video),
aborting recv tasks and unregistering dispatcher routes for each. Audio teardown
emits `voice://state-changed`; video teardown does not (same reason as above).
`leave()` also takes any local `call.video_share` and calls
`shutdown_video_share` on it, so leaving a call while sharing tears the capture
loop down too (see Phase C2 below).

---

## Phase C2 — share lifecycle + viewer

Phase C2 makes screensharing end-to-end: it adds the user-facing share trigger,
drives the C1 `VideoSender` from the Phase B capture loop, and decodes each
peer's stream into a WebCodecs tile in the UI. The C1 transport primitives are
unchanged; C2 wires them to the capture/encode loop and the frontend.

### `start_screen_share(fps, max_width, max_height) -> Result<(), String>`

The local share entry point on `VoiceController`. One sharer per call.

1. Fail-fast: constructs and drops a throwaway `H264Encoder` so an unusable
   encoder errors before any state is touched.
2. Locks `inner`, requires an active call (else `"not in a voice channel"`), and
   reads whether a `video_share` already exists. If so, returns
   `"already sharing your screen"` — **the one-sharer-per-call guard**.
3. Derives a fresh per-call VIDEO stream key (`derive_stream_key`) and offers it
   to the current channel members via the `offer_video_key` helper (wraps the key
   per peer with `wrap_stream_key_for_peer`, skipping self, and calls
   `offer_stream_key(Video, …)`).
4. Starts the Phase B capture backend, then spawns the capture→encode→send
   thread: it builds the `H264Encoder` (kept off the async runtime because the
   encoder is `!Send`), constructs a `VideoSender` (the C1 stateful sealer), and
   runs `run_encode_loop`, whose sink calls `VideoSender::send(frame, |b| server.send_datagram(b))`
   for every encoded frame. The loop also receives a `force_keyframe:
   Arc<AtomicBool>` flag (see below).
5. Enables the Video track (`enable_track(Video)`) so peers set up their recv
   pipeline. If that fails, the just-spawned thread is torn down before returning.
6. Stores `VideoShareState { stop, force_keyframe, backend, video_key, thread }`
   in `call.video_share`. A concurrent-start race that lost (another
   `video_share` already present) tears down the share it built without clobbering
   the winner; a call that ended mid-start tears down and disables the track.

### `stop_screen_share() -> Result<(), String>`

Takes `call.video_share` (if any), calls `shutdown_video_share` on it (sets the
`stop` flag AND `backend.stop_capture()` — both are required to break
`run_encode_loop`), then `disable_track(Video)` so peers tear down their video
tiles. No-op if not sharing or not in a call. Teardown also happens on `leave()`.

### `run_encode_loop` mid-stream `force_keyframe`

`run_encode_loop` (in `screenshare.rs`) gained a `force_keyframe:
Arc<AtomicBool>` parameter. When the flag is set, the encoder is told to emit a
fresh IDR on the next frame and the flag is cleared. This is how a late joiner
gets a keyframe without restarting the share.

### Keyframe-on-join + late-joiner re-offer (`on_peer_stream_joined`)

When a new peer joins the call (`on_peer_stream_joined`) and we are currently
sharing, the controller:

- sets the share's `force_keyframe` flag so the encoder emits a fresh IDR the new
  viewer can start decoding from immediately, and
- re-offers the video key via `offer_video_key` — the new member is now in
  `get_media_state`, so the re-offer wraps the existing `video_key` for the
  enlarged member set (the offer runs after the `inner` lock is released).

Without this, a peer joining mid-share would never receive the video key and
would wait indefinitely for a keyframe.

### Frontend viewer — `PeerVideoTiles.tsx`

`client/src/components/PeerVideoTiles.tsx`, mounted in `ChannelSidebar` above the
voice control bar, renders one tile per sharing peer. It listens for
`voice://peer-video-frame` (documented above, carries `session`, `pubkey`,
`data`, `key`, `seq`):

- **Lazy per-session decoder:** one `PeerDecoder` (WebCodecs `VideoDecoder` +
  canvas) is created lazily on the first frame for a `session`. Keyed by
  `session` so each H.264 stream gets its own decoder (mixing streams corrupts
  output — see the C1 gotcha). Configured for Annex-B (`configure()` with no
  `description`, `optimizeForLatency`).
- **Key-gated:** frames are dropped until the first keyframe (`p.key`) for that
  session arrives, enforcing the key-first invariant before feeding deltas.
- **Error self-heal:** on a decoder error (corrupt stream) the decoder is closed
  and dropped; the next frame for that session recreates it and re-gates on a
  keyframe.
- **3s idle reap:** a timer closes and drops any session's decoder when no frame
  has arrived for >3s, and removes its tile. All decoders are also closed on
  unmount so leaving the view never leaks `VideoDecoder`s.

The tile labels itself with the first 8 chars of the sharer's `pubkey`.

---

## State it owns

| Field | Type | What it tracks, when it's mutated |
|---|---|---|
| `ActiveCall.peer_keys` | `HashMap<(SessionId, TrackKind), [u8; 32]>` | Per-track stream keys; populated by `on_stream_key_offer`, read by `on_peer_*_track_enabled` |
| `ActiveCall.peer_pubkeys` | `HashMap<SessionId, PublicKey>` | Long-lived identity keys; populated by the first `on_stream_key_offer` for each session |
| `ActiveCall.video_peers` | `HashMap<SessionId, VideoPeerEntry>` | Live video recv tasks; populated by `on_peer_video_track_enabled`, drained by `leave` / `on_peer_track_disabled(Video)` |
| `ActiveCall.video_share` | `Option<VideoShareState>` | The LOCAL outbound share (one per call): `stop`/`force_keyframe` flags, capture `backend`, `video_key`, encode `thread`. Set by `start_screen_share`, taken by `stop_screen_share` / `leave` |
| `MediaInboundDispatcher.routes` | `Mutex<HashMap<(SessionId, TrackKind), UnboundedSender<Bytes>>>` | Dispatch table; one entry per registered (session, track) pair |

## Events emitted

| Event name | Payload shape | When | Who listens |
|---|---|---|---|
| `"voice://peer-video-frame"` | `{ session: string, pubkey: string, data: string, key: boolean, seq: number }` | Per decrypted, reassembled video frame | Phase C2 frontend per-peer video tile (WebCodecs decoder) |

## Events / requests consumed

| Event / request | Source | What this module does with it |
|---|---|---|
| `ServerEvent::StreamKeyOffer { kind: Video, ... }` | `bridge.rs` | `on_stream_key_offer(session_id, TrackKind::Video, sender_pubkey, wrapped_key)` — unwraps and stores the video stream key |
| `ServerEvent::TrackEnabled { kind: Video, ... }` | `bridge.rs` | `on_peer_video_track_enabled` — registers dispatcher route and spawns video recv task |
| `ServerEvent::TrackDisabled { kind: Video, ... }` | `bridge.rs` | `on_peer_track_disabled(session_id, TrackKind::Video)` — tears down the recv task and unregisters the route |

## Integration map

- **`farder-crypto::media`** — `seal_video_frame_to_wire` / `open_video_wire_frame`: the AEAD
  seal+open primitives for the video wire frame.
- **`farder-protocol::media_datagram`** — `fragment` / `Reassembler` / `OuterHeader`: the
  fragmentation and reassembly layer, shared with audio. Video frames use `TrackKind::Video`
  in the outer header; the relay and server treat them identically to audio frames (no server
  changes for C1).
- **`voice/send_video.rs`** — `build_video_datagrams` / `VideoSender`: the send-side primitives.
  Phase C2 wires `VideoSender` from the Phase B encode loop.
- **`voice/recv_video.rs`** — `run(RecvVideoConfig, sink)`: the per-peer video recv task.
- **`voice/mod.rs`** (`VoiceController`) — owns the `peer_keys`, `video_peers`, and
  `MediaInboundDispatcher`; coordinates the full lifecycle.
- **`bridge.rs`** — routes `ServerEvent::StreamKeyOffer/TrackEnabled/TrackDisabled` with
  `kind == Video` to the controller's video methods. Routes `TrackEnabled/TrackDisabled`
  with `kind == Audio` to the existing audio methods (unchanged).
- **`screenshare-capture-codec.md`** — Phase B delivers the encoded `EncodedFrame` that
  `VideoSender` seals and sends. Phase C2 connects the two.
- **`media-datagram.md`** — full reference for the Phase A outer header format, fragment /
  reassemble, and `DEFAULT_MAX_DGRAM_PAYLOAD`.
- **`tauri-bridge.md`** — canonical event catalog entry for `voice://peer-video-frame`.

## Known gotchas

- **Video key must arrive before TrackEnabled(Video):** `on_peer_video_track_enabled`
  looks up `(session_id, TrackKind::Video)` in `peer_keys`. If the `StreamKeyOffer(Video)`
  has not yet arrived, the method logs a warning and returns without registering the peer.
  The video track will never be decrypted for that session. Phase C2 must ensure the video
  `StreamKeyOffer` is sent before (or atomically with) `TrackEnabled(Video)`.

- **Keyframe byte is inside the ciphertext, not cleartext:** `plaintext[0]` is only
  readable after AEAD authentication. Any code that tries to inspect the `is_keyframe`
  flag on the sealed wire bytes will read garbage or nothing. Always open the wire frame
  first, then read `plaintext[0]`.

- **No jitter buffer on the video recv path:** audio uses a 3-slot jitter buffer (`jitter.rs`)
  because Opus decoding must be paced at 48 kHz. Video has no such constraint — the WebCodecs
  decoder handles timing. Adding a jitter buffer on the video path would introduce latency
  with no benefit.

- **`voice://peer-video-frame` fires for every peer independently:** the `session` field
  identifies which peer the frame came from. A frontend decoder that mixes frames from
  different sessions into the same `VideoDecoder` will produce corrupt output (each
  H.264 stream has its own SPS/PPS and is not interleaved). One `VideoDecoder` instance
  per `session` value.

- **Audio path is unchanged:** the `MediaInboundDispatcher` re-key is fully backward-
  compatible. Audio peers are registered as `(session_id, TrackKind::Audio)` and receive
  exactly the same datagrams they did before C1. All 55 voice tests remained green
  after the re-key.
