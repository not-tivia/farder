# Screensharing (Game Streaming) — Design

**Date:** 2026-06-12
**Status:** Approved by owner (design conversation 2026-06-12)
**Owner priority:** #1 post-privacy feature.

## Problem & framing

Farder has voice channels with an end-to-end-encrypted media pipeline, but no
way to share a screen. The owner wants **game streaming** — the most demanding
target: smooth motion, low latency, and higher resolution matter more than
still-image crispness. v1 deliberately proves the entire pipeline end to end at
a usable-but-modest quality (software-encoded ~720p/30) so every layer is real
and verified; the high-end push (hardware encoding, 1080p60) is phase 2 on the
same architecture.

The media pipeline was built video-ready: `TrackKind::Video` already exists
(`crates/farder-protocol/src/server.rs:23-26`), the encrypted frame format has
a video type byte (`crates/farder-crypto/src/media.rs:69-77`), the per-stream
key wrapping is codec-agnostic (`media.rs:21-58`), the relay forwards frames
blind (`crates/farder-relay/src/datagram.rs:16-55`), and the server fan-out
routes by session id regardless of content (`crates/farder-server/src/media_stream.rs:219-275`).
These are reused unchanged. The genuinely new work is: a fragment/reassemble
transport for large frames, real screen + system-audio capture, H.264
encode, video decode in the webview, removing a hardcoded audio-only check on
the receive path, and the UI.

## Decisions (owner)

- **Use case:** game streaming — optimize for motion + low latency.
- **v1 quality:** prove the full real pipeline at ~720p / ~30fps, **software**
  H.264 encoding. NVENC hardware encoding and 1080p60 are phase 2 (encoder swap
  only — see below).
- **Screen audio IS in v1:** the game's own sound is captured (Windows loopback)
  and streamed as its own track, with an independent volume slider separate
  from people talking.
- **Watching is opt-in:** a sharer shows a LIVE badge; viewers click to open the
  stream (Discord-style), not auto-popped.
- **One sharer per channel** in v1; whole-monitor or single-window capture; must
  be in the voice channel to share.

## Architecture

### Transport: fragment / reassemble over datagrams

Video must ride the **unreliable QUIC datagram** path (not reliable streams) —
reliable delivery would head-of-line-block on every lost packet, which is fatal
for live interactive content. But a datagram only carries ~1200 bytes and an
encoded video picture is routinely 1–5 KB, so frames are fragmented.

**Unified media datagram format (v1 — supersedes the implicit audio-only
layout).** Every media datagram (mic audio, video, screen audio) gets a
cleartext routing/fragmentation header, then a slice of the sealed frame:

```
[ver:1 = 0x03]
[track_kind:1]        // 0x01 mic-audio, 0x02 video, 0x03 screen-audio
[session_id:16]       // sender's stream session — the server routes by this
[frame_id:4]          // u32, monotonic per (session, track)
[frag_index:2]        // u16, 0-based
[frag_count:2]        // u16, total fragments for this frame
[payload...]          // slice of the sealed frame (see crypto below)
```

26-byte cleartext header. The **relay** forwards blind (it only reads its own
4-byte handle prefix — `datagram.rs`). The **server** reads `session_id`
(routing) and `track_kind` (bandwidth accounting) from this header — it never
needs keys. The **receiver** buffers payload slices keyed by
`(session_id, track_kind, frame_id)`; once `frag_count` slices have arrived it
concatenates them into the sealed frame and decrypts once.

- **Audio frames are `frag_count = 1`** — the existing small Opus frames wrap in
  one datagram. The seal/open crypto is unchanged; only the outer header is new.
- **Drop-late / drop-incomplete policy:** the reassembly buffer holds only the
  few most-recent in-progress `frame_id`s per sender-track (e.g. a ring of 2–3).
  When a newer `frame_id`'s fragments arrive, older incomplete frames are
  abandoned — a frozen-but-current picture beats a smooth-but-delayed one, and a
  single lost fragment costs one frame, not a stall.
- **Fragment sizing:** query the connection's `max_datagram_size`; fragment the
  sealed frame so each datagram (26-byte header + payload + 4-byte relay handle +
  QUIC overhead) stays under it, with a conservative default (~1100-byte
  payload) when the size is unknown.

**Interop note (prominent):** this changes the media datagram format, so an old
client and a new client cannot exchange voice/video — both sides must run the
new build (the usual "rebuild the server sidecar + client" Farder media rule).
Because media rides throwaway per-session datagrams, version skew degrades to
"no audio/video between mismatched peers," never a crash or reconnect loop.
Phase A re-verifies existing mic voice (direct AND over relay) still works after
the format change — folding in the long-deferred voice-over-relay verification.

### Crypto / E2EE (reused, codec-agnostic)

Unchanged from today (`crates/farder-crypto/src/media.rs`): each
`(session, TrackKind)` has a random 32-byte stream key
(`derive_stream_key`), wrapped per recipient via the existing identity-key DH +
AES-256-GCM (`wrap_stream_key_for_peer`, 60 bytes/peer) and distributed through
the existing `OfferStreamKey` / `StreamKeyOffer` protocol messages. The sealed
frame is `seal_media_frame()` exactly as today — it operates on an opaque
codec payload, so an H.264 picture seals identically to an Opus packet. The
reassembled bytes (above) ARE a sealed frame; `open_media_frame()` decrypts it.
The relay and server never hold keys and never see plaintext — confirmed
unchanged. A new `TrackKind::ScreenAudio` gets its own stream key, so game
sound is independently encrypted, keyed, and volume-controlled.

### Codec & capture stack

- **Video encode — H.264, software, in Rust** (`openh264` crate: prebuilt
  binary, permissive, real-time, Constrained Baseline). Chosen over VP8/VP9
  because NVIDIA's hardware encoder (phase 2) speaks H.264 — the phase-2 upgrade
  swaps **only the encoder**; the decode path and everything else are untouched.
  Target ~3 Mbps at 720p30 with periodic keyframes (e.g. every ~2s and on
  demand when a new viewer attaches).
- **Video decode — in the webview via WebCodecs.** WebView2 (Tauri's Windows
  webview, Chromium-based) supports the `VideoDecoder` API. The Rust receiver
  hands the small **encoded** H.264 frames (~tens of KB) to the frontend over
  IPC; the webview decodes on the GPU and paints to a `<canvas>`. This keeps IPC
  tiny (encoded, not raw 720p frames) and means **no Rust video decoder is
  needed for the product path** (openh264 decode is used only in headless
  tests). It also makes the phase-2 NVENC swap purely an encoder change.
- **Screen capture — Windows Graphics Capture** (the modern WGC API, via the
  `windows-capture` crate). It captures monitors and individual windows and —
  critically for games — exclusive-fullscreen content that older capture methods
  cannot. Yields BGRA frames; a color-convert step produces I420 (YUV420) for
  the H.264 encoder. Implements the existing `DisplayBackend` trait
  (`client/src-tauri/src/display.rs`), so the mock backend stays for tests.
- **Screen audio — Windows WASAPI loopback** capture of the default output,
  downmixed to mono 48 kHz, fed into the **existing** Opus encode + seal + send
  path as the `ScreenAudio` track (small frames, no fragmentation). (Stereo
  screen audio is a phase-2 nicety.)

### Server routing & bandwidth (mostly reuse)

The server's ingress (`media_stream.rs on_frame_ingress`) already fans a frame
to other sessions in the same channel by session id — it now reads session id
and track kind from the new cleartext header instead of from inside the sealed
frame, and is otherwise unchanged (it still forwards opaque bytes via
`VoiceSink::{Direct,Relayed}`, `state.rs:14-31`). The per-track token bucket
(`media_stream.rs:81-125`) keeps its audio cap; the **video cap is raised from 2
Mbps to ~8 Mbps** to leave headroom for keyframe bursts at the 3 Mbps target.
`ScreenAudio` uses the audio cap. No new server fan-out code — fragments are
just more datagrams routed by the same logic.

### Relay (reused)

The relay's handle-stamped forward/route (`datagram.rs`) carries fragments with
zero changes — it never inspects the media header. Voice-over-relay being
code-complete means video-over-relay rides the identical path; its end-to-end
verification happens naturally during this feature's two-client test.

### Client pipeline

- **Send (sharer):** a new video pipeline runs alongside the audio one —
  WGC capture → BGRA→I420 → openh264 encode → `seal_media_frame` → fragment →
  datagram sink (the existing QUIC datagram path). The screen-audio pipeline is
  the existing audio send path sourced from loopback capture under the
  `ScreenAudio` track. Both are gated on the sharer having enabled the
  respective track (`EnableTrack`) and having offered stream keys to current
  viewers.
- **Receive (viewer):** the hardcoded audio-only early-returns
  (`client/src-tauri/src/voice/mod.rs:648-651`, `bridge.rs:640-662`) are
  removed and the receive path is generalized to spawn a handler per
  `(peer, TrackKind)`. Audio tracks (mic + screen) flow through the existing
  reassemble(trivial)→open→Opus-decode→mixer path (screen audio mixed with its
  own gain). Video tracks flow through reassemble→open→**hand encoded H.264 to
  the webview** (a Tauri event/channel carrying the encoded frame + metadata);
  the frontend WebCodecs decoder renders it.

### UI

Screensharing lives inside a voice channel.

- **Share Screen button** in `VoiceControlBar.tsx` next to mute/deafen (only
  when in a call). Clicking opens the OS picker (WGC's monitor/window chooser);
  on selection the client enables the Video track (+ ScreenAudio), offers keys
  to present viewers, and starts capturing. A **Stop Sharing** button replaces it
  while live.
- **LIVE badge** next to a sharing member's name in the voice participant list
  (`ChannelSidebar.tsx` / `useVoice.ts` peer state gains an `isSharing` flag from
  the sharer's `TrackEnabled { Video }`). Clicking the badge opens the viewer.
- **Viewer pane:** a `<canvas>` fed by the WebCodecs decoder, showing the
  sharer's name, enlargeable / fullscreen, with a **game-audio volume slider**
  (drives the `ScreenAudio` track's per-peer gain via the existing
  `setPeerVolume` mechanism). Closing the pane stops decoding but the sharer
  keeps broadcasting for others.
- New classes styled in **all three themes** (CLAUDE.md rule).

## Privacy notes

Screen video and game audio are E2EE exactly like voice — sealed per-track,
keys wrapped per recipient with identity-key DH, relay and server forward blind.
A relay/server operator cannot see a shared screen. The only metadata exposed is
the same as voice today (who is in the channel, that a video track is active,
frame timing/size). The sharer explicitly chooses what to capture via the OS
picker; nothing is shared until they pick a source and go live.

## Limits & edge cases

- **Keyframe on join:** when a viewer attaches mid-stream, the sharer forces a
  keyframe so the decoder can start without waiting for the periodic one.
- **Loss:** a lost fragment drops that one frame; the next keyframe (or the next
  fully-received frame) recovers. No retransmission.
- **No source / capture failure / permission denied:** the share button surfaces
  a friendly error and stays in the not-sharing state; voice is unaffected.
- **Sharer leaves voice:** the Video + ScreenAudio tracks tear down with the
  session (existing `StreamLeft` path); viewer panes close.
- **Backpressure:** if encode/network can't keep up, frames are dropped at the
  source (the token bucket + a bounded send queue), never queued unboundedly.
- **One sharer per channel** is enforced client-side in v1 (the Share button is
  disabled when someone else is already live); a server-side guard is a small
  add but not required for v1 since starting a second share just means a second
  video track the UI chooses not to surface.

## Scope & phasing

The implementation plan will phase this; each phase is independently testable
and the risky network/codec layers are proven before any UI:

- **Phase A — Transport.** The unified media datagram header + fragment/reassemble
  layer (client send + recv) and the server/relay reading session id from the new
  header. Headless-testable with synthetic frames (fragmentation round-trips,
  drop-incomplete, drop-late, audio `frag_count=1`). **Regression gate: existing
  mic voice still works, direct and over relay** (the deferred voice-over-relay
  verification lands here).
- **Phase B — Capture + codec.** Real WGC screen capture and openh264 encode
  behind the `DisplayBackend` seam (mock stays); BGRA→I420; encoded-frame output.
  Frontend WebCodecs decode + canvas render, proven with a captured-then-decoded
  loopback. No networking yet.
- **Phase C — Video track wiring.** Remove the audio-only checks; generalize the
  send/recv pipeline and server routing for the Video track; per-video-track
  stream keys; keyframe-on-join. End-to-end one-sharer-one-viewer video over the
  datagram path (direct + relay).
- **Phase D — Screen audio.** WASAPI loopback capture → existing Opus path as the
  `ScreenAudio` track; independent decode/mix with its own gain.
- **Phase E — UI.** Share button + OS picker, Stop Sharing, LIVE badge,
  click-to-watch viewer pane with the game-audio slider, themed in all three.

## Out of scope (phase 2+, noted not built)

- NVENC / hardware encoding; 1080p60; in-call bitrate/quality sliders.
- Multiple simultaneous sharers per channel.
- Region/cropped capture; per-application audio capture; stereo screen audio.
- macOS/Linux capture backends (the `DisplayBackend` seam keeps this open).
- Recording; viewer-side annotations; remote control.

## Verification plan

Headless on this machine: fragmentation/reassembly unit + integration tests
(round-trip, drop-incomplete, drop-late, MTU sizing, audio `frag_count=1`);
the existing voice tests must stay green after the format change; openh264
encode→decode round-trip; server routing of fragmented frames (extend the
`relay_mode` / media tests); WebCodecs decode is exercised via a frontend test
harness where feasible. Real verification is the owner's two-client Windows run
(per CLAUDE.md, UNVERIFIED until then): one machine shares a game (fullscreen),
the second joins voice and clicks LIVE → sees the gameplay with acceptable
latency and hears the game audio with an independent volume from voice; confirm
both direct and over the deployed relay. This run also finally verifies
voice-over-relay end to end.
