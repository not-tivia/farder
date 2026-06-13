# Screensharing Phase C2 — Share Lifecycle + Viewer (user-reachable)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make screen-sharing user-reachable end to end: a Share button starts capturing+encoding (Phase B) and sends the video over the call via the Phase C1 `VideoSender`; viewers' clients decode each peer's stream with WebCodecs and show it. One sharer at a time per channel.

**Architecture:** A `start_screen_share` controller method mirrors the audio join offer (derive a video stream key, wrap+offer it to channel members, `enable_track(Video)`) then spawns the Phase B capture→encode loop with a sink that drives a `VideoSender` (C1) into the call's `send_datagram`. A keyframe-on-viewer-join flag forces an IDR when a new peer joins, and the join also re-offers the video key (so late joiners can decrypt). The receive side already exists (C1 emits `voice://peer-video-frame` per decrypted frame); C2 adds the frontend: a Share button in the voice bar and a per-peer video tile that lazily spins up a WebCodecs decoder per sharing session. The audio path is untouched and regression-tested. The two-client end-to-end (sharer → viewer over relay + direct) is the owner's Windows verification.

**Tech Stack:** Rust (the voice controller, Phase B `H264Encoder`/`run_encode_loop`/`make_display_backend`, Phase C1 `VideoSender`, `farder-crypto` key wrap), Tauri commands/events, React/TypeScript + WebCodecs.

**Spec:** `docs/superpowers/specs/2026-06-12-screensharing-design.md` (Phase C; C2 is the share trigger + viewer UI portion. The polished OS picker, LIVE-badge styling, and game-audio slider are Phase E; C2 delivers a functional Share button + auto-shown video tile).

**Branch:** create `screenshare-phaseC2` from `main` before Task 1. Finish with ff-merge + push.

**Scope note:** C2 makes one-sharer screen-sharing work and verifiable. Game audio is Phase D; the polished share UI (OS source picker, LIVE badge, viewer pane + game-audio volume slider, click-to-watch) is Phase E. C2's viewer auto-shows a tile when a peer's video frames arrive (functional, minimal styling).

---

## Verified codebase facts (read 2026-06-13 — exact)

- **Audio join offer pattern** (`client/src-tauri/src/voice/mod.rs:566-631` `join_with_config`): `let session_id = server.join_stream(channel_id).await?;` → `let stream_key = farder_crypto::media::derive_stream_key();` → `let participants = server.get_media_state(channel_id).await?;` → `let keypair = server.my_keypair(); let my_sk = *keypair.signing_key_bytes(); let my_pk_bytes = *keypair.public_key().as_bytes();` → wrap per participant (skip self) via `farder_crypto::media::wrap_stream_key_for_peer(&stream_key, &my_sk, m.public_key.as_bytes())` into `Vec<(PublicKey, Vec<u8>)>` → `server.offer_stream_key(TrackKind::Audio, wrapped).await?` (only if non-empty) → spawn pipeline → `server.enable_track(TrackKind::Audio).await?`. The datagram sink is `Box::new(move |b: Bytes| { let _ = server_for_sink.send_datagram(b); })`.
- **`ServerSession` trait** (`voice/mod.rs`): `join_stream`, `leave_stream`, `get_media_state(channel_id) -> Result<Vec<VoiceMember>, String>`, `offer_stream_key(kind, Vec<(PublicKey, Vec<u8>)>)`, `enable_track(kind)`, `disable_track(kind)`, `set_mute`, `set_deafen`, `send_datagram(Bytes) -> Result<(), String>`, `my_keypair() -> Arc<Keypair>`, `dispatcher() -> Arc<MediaInboundDispatcher>`. `VoiceMember` has `public_key: PublicKey`.
- **ActiveCall** (`voice/mod.rs:411-...`): holds `server: Arc<dyn ServerSession>`, `peer_keys: HashMap<(SessionId,TrackKind),[u8;32]>`, `peer_pubkeys`, `video_peers: HashMap<SessionId, VideoPeerEntry>`, etc. **Does NOT currently store our own `session_id` or `channel_id`** — C2 adds them.
- **C1 video send** (`client/src-tauri/src/voice/send_video.rs`): `VideoSender::new(stream_key: [u8;32], session_id: SessionId, speaker_pk: [u8;32])`, `send(&mut self, frame: &EncodedFrame, sink: impl FnMut(Bytes))`.
- **Phase B capture** (`client/src-tauri/src/screenshare.rs`): `run_encode_loop(rx: Receiver<VideoFrame>, encoder: H264Encoder, stop: Arc<AtomicBool>, sink: impl FnMut(EncodedFrame))` — forces ONE keyframe at start then encodes until stop. `H264Encoder::new() -> Result<_, String>` (!Send), `force_keyframe(&mut self)`. `make_display_backend() -> Box<dyn DisplayBackend>`; `DisplayBackend::{enumerate_sources, start_capture(source_id, DisplayFormat{fps,max_width,max_height}) -> Result<mpsc::Receiver<VideoFrame>>, stop_capture}`. `EncodedFrame{data, is_keyframe, timestamp_ms}`.
- **C1 video recv emit** (`voice/mod.rs on_peer_video_track_enabled`): emits `voice://peer-video-frame` with `{session: hex(session_id), data: base64(h264), key: bool, seq}`. The handler has `_peer_pubkey: PublicKey` (currently unused) — C2 includes it in the event so the viewer can label/clean up the tile.
- **StreamJoined hook** (`voice/mod.rs:1090` `on_peer_stream_joined(session_id, muted, deafened)`): called from `bridge.rs:181` on the `StreamJoined` event. Does NOT carry the new peer's pubkey — C2's re-offer re-derives the member list via `get_media_state` instead.
- **Frontend voice** (`client/src/components/VoiceControlBar.tsx`): renders when `voice.inCall`; buttons live in `<div className="vcb-buttons">`. `client/src/hooks/useVoice.ts`: `UseVoice` interface + `listen("voice://...")` event wiring; `peers: VoiceUiPeer[]`. Tauri events listened via `import { listen } from "@tauri-apps/api/event"`. WebCodecs types resolve under the existing `lib.dom` (proven in Phase B `ScreensharePreview.tsx`).
- **Phase B preview component** (`client/src/components/ScreensharePreview.tsx`): the WebCodecs decode pattern (configure `avc1.42E01E` no description, key-gated, `EncodedVideoChunk`, draw to canvas) — C2's per-peer tile reuses this pattern keyed by session.
- **Voice command wiring**: `voice_join`/`voice_leave` etc. are Tauri commands that fetch the `VoiceController` from app state and call its methods (see `commands.rs`/`voice_bridge.rs`); registered in `generate_handler![]` in `main.rs`.

---

### Task 1: Store my_session_id + channel_id on ActiveCall

**Files:** Modify `client/src-tauri/src/voice/mod.rs`

- [ ] **Step 1: Add the fields.** In `ActiveCall`, add:
```rust
    /// Our own stream session id (from JoinStream). Video share seals frames
    /// under this session, same as audio.
    my_session_id: SessionId,
    /// The channel we're in (for re-querying members on a late-joiner re-offer).
    channel_id: u64,
```
- [ ] **Step 2: Populate them at join.** In `join_with_config`, the `ActiveCall { ... }` construction (after the join obtained `session_id` and the fn has `channel_id`): add `my_session_id: session_id, channel_id,`. (Update any OTHER `ActiveCall { ... }` builder — e.g. a test builder — to set both; for tests use a fixed `[0u8;16]` / `0`.)
- [ ] **Step 3:** `cd client/src-tauri && cargo build && cargo test voice:: -- --test-threads=1` → clean + all voice tests pass (fields added, nothing reads them yet).
- [ ] **Step 4:** Commit:
```bash
git add client/src-tauri/src/voice/mod.rs
git commit -m "client: store my_session_id + channel_id on the active call (for video share)"
```

---

### Task 2: run_encode_loop honors a force-keyframe flag

**Files:** Modify `client/src-tauri/src/screenshare.rs`

- [ ] **Step 1: Add a test** (append to `mod tests`):
```rust
    #[test]
    fn force_keyframe_flag_injects_a_midstream_keyframe() {
        let backend = MockDisplayBackend::new();
        let rx = backend
            .start_capture("mock-display", DisplayFormat { fps: 30, max_width: 160, max_height: 120 })
            .unwrap();
        let encoder = H264Encoder::new().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let force = Arc::new(AtomicBool::new(false));

        let count = Arc::new(AtomicUsize::new(0));
        let keyframe_after_force = Arc::new(AtomicBool::new(false));
        let stop_s = stop.clone();
        let force_s = force.clone();
        let count_s = count.clone();
        let kaf = keyframe_after_force.clone();
        run_encode_loop(rx, encoder, stop.clone(), force.clone(), move |enc| {
            let n = count_s.fetch_add(1, Ordering::Relaxed);
            // After a few frames, request a keyframe; the NEXT frame must be one.
            if n == 3 { force_s.store(true, Ordering::Relaxed); }
            if n == 4 && enc.is_keyframe { kaf.store(true, Ordering::Relaxed); }
            if n + 1 >= 6 { stop_s.store(true, Ordering::Relaxed); }
        });
        backend.stop_capture().unwrap();
        assert!(keyframe_after_force.load(Ordering::Relaxed), "frame after force flag must be a keyframe");
    }
```
Also update the existing two tests that call `run_encode_loop(rx, encoder, stop.clone(), |enc| ...)` to pass a never-set force flag: insert `Arc::new(AtomicBool::new(false))` as the 4th arg.

- [ ] **Step 2:** Run `cd client/src-tauri && cargo test screenshare::` → compile FAIL (arity).

- [ ] **Step 3: Add the flag to run_encode_loop:**
```rust
pub fn run_encode_loop(
    rx: Receiver<VideoFrame>,
    mut encoder: H264Encoder,
    stop: Arc<AtomicBool>,
    force_keyframe: Arc<AtomicBool>,
    mut sink: impl FnMut(EncodedFrame),
) {
    encoder.force_keyframe(); // first frame is always a keyframe
    while !stop.load(Ordering::Relaxed) {
        let frame = match rx.recv() {
            Ok(f) => f,
            Err(_) => break,
        };
        // A viewer joined mid-stream: emit a fresh IDR so they can start.
        if force_keyframe.swap(false, Ordering::Relaxed) {
            encoder.force_keyframe();
        }
        match encoder.encode(&frame) {
            Ok(encoded) => sink(encoded),
            Err(e) => eprintln!("[screenshare] encode dropped a frame: {e}"),
        }
    }
}
```
Update the Phase B `start_screenshare_preview` caller to pass `Arc::new(AtomicBool::new(false))` as the force flag (the local preview never needs a mid-stream keyframe).

- [ ] **Step 4:** `cd client/src-tauri && cargo test screenshare::` → all pass (the 3 existing + the new force test).

- [ ] **Step 5:** Commit:
```bash
git add client/src-tauri/src/screenshare.rs
git commit -m "client: run_encode_loop honors a mid-stream force-keyframe flag"
```

---

### Task 3: Controller start_screen_share / stop_screen_share

**Files:** Modify `client/src-tauri/src/voice/mod.rs`

- [ ] **Step 1: Add the share state to ActiveCall + a struct.** Add to `ActiveCall`:
```rust
    /// Active outbound screen share (None = not sharing). One per call.
    video_share: Option<VideoShareState>,
```
and (next to `VideoPeerEntry`):
```rust
struct VideoShareState {
    stop: Arc<AtomicBool>,
    force_keyframe: Arc<AtomicBool>,
    backend: Box<dyn crate::display::DisplayBackend>,
    /// The video stream key (for re-offering to late joiners).
    video_key: [u8; 32],
    #[allow(dead_code)]
    thread: std::thread::JoinHandle<()>,
}
```
Initialize `video_share: None` at every `ActiveCall { ... }` construction.

- [ ] **Step 2: Add the share methods** (on `impl VoiceController`):
```rust
    /// Start sharing our screen into the active call: derive a video key, offer
    /// it to current members, enable the Video track, and drive the Phase B
    /// capture→encode loop into the C1 VideoSender over this call's connection.
    pub async fn start_screen_share(&self, fps: u32, max_width: u32, max_height: u32) -> Result<(), String> {
        // Fail-fast: validate the encoder can init (it's built inside the thread).
        drop(crate::video_encoder::H264Encoder::new()?);

        let (server, my_session_id, channel_id, my_pk_bytes, already) = {
            let inner = self.inner.lock().await;
            match inner.active.as_ref() {
                Some(c) => {
                    let kp = c.server.my_keypair();
                    (c.server.clone(), c.my_session_id, c.channel_id, *kp.public_key().as_bytes(), c.video_share.is_some())
                }
                None => return Err("not in a voice channel".into()),
            }
        };
        if already {
            return Err("already sharing your screen".into());
        }

        // Derive + offer the video key to current members.
        let video_key = farder_crypto::media::derive_stream_key();
        offer_video_key(&server, channel_id, &video_key).await?;

        // Start capture.
        let backend = crate::display::make_display_backend();
        let sources = backend.enumerate_sources()?;
        let source_id = sources.first().map(|s| s.id.clone()).ok_or("no capture source")?;
        let rx = backend.start_capture(&source_id, crate::display::DisplayFormat { fps, max_width, max_height })?;

        let stop = Arc::new(AtomicBool::new(false));
        let force_keyframe = Arc::new(AtomicBool::new(false));

        // Spawn the capture→encode→send thread (encoder is !Send → built inside).
        let stop_t = stop.clone();
        let force_t = force_keyframe.clone();
        let server_t = server.clone();
        let thread = std::thread::spawn(move || {
            let encoder = match crate::video_encoder::H264Encoder::new() {
                Ok(e) => e,
                Err(e) => { eprintln!("[voice] video encoder init failed: {e}"); return; }
            };
            let mut sender = crate::voice::send_video::VideoSender::new(video_key, my_session_id, my_pk_bytes);
            crate::screenshare::run_encode_loop(rx, encoder, stop_t, force_t, move |enc| {
                sender.send(&enc, |b| { let _ = server_t.send_datagram(b); });
            });
        });

        // Enable the Video track (after capture is up).
        server.enable_track(TrackKind::Video).await?;

        // Record share state.
        let mut inner = self.inner.lock().await;
        if let Some(call) = inner.active.as_mut() {
            call.video_share = Some(VideoShareState { stop, force_keyframe, backend, video_key, thread });
        }
        Ok(())
    }

    /// Stop sharing: tear down capture, stop the send loop, disable the track.
    pub async fn stop_screen_share(&self) -> Result<(), String> {
        let (share, server) = {
            let mut inner = self.inner.lock().await;
            match inner.active.as_mut() {
                Some(c) => (c.video_share.take(), Some(c.server.clone())),
                None => (None, None),
            }
        };
        if let Some(s) = share {
            s.stop.store(true, Ordering::Relaxed);
            let _ = s.backend.stop_capture();
            if let Some(server) = server {
                let _ = server.disable_track(TrackKind::Video).await;
            }
        }
        Ok(())
    }
```
And a free helper (near the top of the impl block or as a module fn):
```rust
/// Wrap `video_key` for every current channel member (except self) and offer it.
async fn offer_video_key(server: &Arc<dyn ServerSession>, channel_id: u64, video_key: &[u8; 32]) -> Result<(), String> {
    let participants = server.get_media_state(channel_id).await?;
    let keypair = server.my_keypair();
    let my_sk = *keypair.signing_key_bytes();
    let my_pk = *keypair.public_key().as_bytes();
    let wrapped: Vec<(PublicKey, Vec<u8>)> = participants
        .iter()
        .filter(|m| m.public_key.as_bytes() != &my_pk)
        .filter_map(|m| {
            farder_crypto::media::wrap_stream_key_for_peer(video_key, &my_sk, m.public_key.as_bytes())
                .ok()
                .map(|w| (m.public_key.clone(), w))
        })
        .collect();
    if !wrapped.is_empty() {
        server.offer_stream_key(TrackKind::Video, wrapped).await?;
    }
    Ok(())
}
```

- [ ] **Step 3: Add a controller test** using the existing `FakeServerSession` test harness (find it in mod.rs tests — it records calls). Assert that `start_screen_share` offers a Video key + enables the Video track, and `stop_screen_share` disables it:
```rust
    #[tokio::test]
    async fn start_screen_share_offers_video_key_and_enables_track() {
        // Set FARDER_DISPLAY_BACKEND=mock so capture works headlessly.
        std::env::set_var("FARDER_DISPLAY_BACKEND", "mock");
        let ctrl = /* build controller with a FakeServerSession that has 1 peer in get_media_state */;
        ctrl.join(1, fake_server.clone()).await.unwrap();
        ctrl.start_screen_share(15, 320, 240).await.unwrap();
        // FakeServerSession should have recorded offer_stream_key(Video) and enable_track(Video).
        assert!(fake_server.offered_kinds().contains(&TrackKind::Video));
        assert!(fake_server.enabled_tracks().contains(&TrackKind::Video));
        ctrl.stop_screen_share().await.unwrap();
        assert!(fake_server.disabled_tracks().contains(&TrackKind::Video));
        std::env::remove_var("FARDER_DISPLAY_BACKEND");
    }
```
ADAPT to the actual `FakeServerSession` API: read the existing audio join test to see how it constructs the fake + what it records (offer/enable). If the fake doesn't record offered kinds / enabled / disabled tracks, EXTEND it minimally to record them (it's a test double). If a full controller test is too heavy given the fake's shape, assert the narrower observable (e.g. that start returns Ok with the mock backend and stop returns Ok) and rely on the offer_video_key unit + Task 4's re-offer test — NOTE which you did.

- [ ] **Step 4:** `cd client/src-tauri && cargo build && cargo test voice:: -- --test-threads=1` → clean + green. The audio path is untouched (no audio code modified).

- [ ] **Step 5:** Commit:
```bash
git add client/src-tauri/src/voice/mod.rs
git commit -m "client: VoiceController start/stop_screen_share (offer video key, drive VideoSender)"
```

---

### Task 4: Keyframe-on-join + late-joiner re-offer

**Files:** Modify `client/src-tauri/src/voice/mod.rs`

- [ ] **Step 1: Hook on_peer_stream_joined.** Extend it so that, when we're sharing, a new peer triggers a video-key re-offer + a forced keyframe:
```rust
    pub async fn on_peer_stream_joined(&self, session_id: SessionId, muted: bool, deafened: bool) {
        let reoffer = {
            let mut inner = self.inner.lock().await;
            match inner.active.as_mut() {
                Some(call) => {
                    call.peer_status.insert(session_id, (muted, deafened));
                    // If we're sharing, re-offer the video key to everyone (the
                    // new member is now in get_media_state) and force a keyframe
                    // so they can start decoding immediately.
                    call.video_share.as_ref().map(|s| {
                        s.force_keyframe.store(true, Ordering::Relaxed);
                        (call.server.clone(), call.channel_id, s.video_key)
                    })
                }
                None => None,
            }
        };
        if let Some((server, channel_id, video_key)) = reoffer {
            let _ = offer_video_key(&server, channel_id, &video_key).await;
        }
    }
```

- [ ] **Step 2: Add a test.** Mirror the existing `on_peer_stream_joined_seeds_late_registered_peer` test but with an active share: after `start_screen_share`, call `on_peer_stream_joined(new_sid, false, false)` and assert the fake recorded a SECOND `offer_stream_key(Video)` and that the force_keyframe flag is set. (Adapt to the FakeServerSession recording shape; if the fake counts offers, assert the Video offer count is 2. If heavy, assert the force_keyframe flag was set via a controller test accessor or rely on the offer count.)

- [ ] **Step 3:** `cd client/src-tauri && cargo build && cargo test voice:: -- --test-threads=1` → green. Audio `on_peer_stream_joined` behavior (seeding peer_status) is unchanged when not sharing.

- [ ] **Step 4:** Commit:
```bash
git add client/src-tauri/src/voice/mod.rs
git commit -m "client: re-offer video key + force keyframe when a viewer joins mid-share"
```

---

### Task 5: Tauri commands + include peer pubkey in the video event

**Files:** Modify `client/src-tauri/src/voice_bridge.rs` (or wherever voice commands live — find `voice_join`), `client/src-tauri/src/main.rs`, `client/src-tauri/src/voice/mod.rs`, `client/src/lib/tauri-bridge.ts`

- [ ] **Step 1: Add the commands.** Where `voice_join`/`voice_leave` are defined (fetch the `VoiceController` from app state), add:
```rust
#[tauri::command]
pub async fn voice_start_screen_share(
    ctrl: tauri::State<'_, std::sync::Arc<crate::voice::VoiceController>>,
    fps: u32,
    max_width: u32,
    max_height: u32,
) -> Result<(), String> {
    ctrl.start_screen_share(fps, max_width, max_height).await
}

#[tauri::command]
pub async fn voice_stop_screen_share(
    ctrl: tauri::State<'_, std::sync::Arc<crate::voice::VoiceController>>,
) -> Result<(), String> {
    ctrl.stop_screen_share().await
}
```
(Match the EXACT state-extraction pattern the existing `voice_join` command uses — `tauri::State<Arc<VoiceController>>` vs `app.state()` etc. Read voice_join first.)

- [ ] **Step 2: Register** both in `generate_handler![...]` in `main.rs`.

- [ ] **Step 3: Include the peer pubkey in the per-peer video event** so the viewer can label/clean up the tile. In `on_peer_video_track_enabled`, the handler has the peer pubkey — capture it for the emit. Change `async fn on_peer_video_track_enabled(&self, session_id: SessionId, _peer_pubkey: PublicKey)` to use the pubkey: add `let peer_hex = _peer_pubkey.to_string();` (rename `_peer_pubkey` → `peer_pubkey`), and add `"pubkey": peer_hex` to the emitted JSON:
```rust
                    emitter.emit(
                        "voice://peer-video-frame",
                        serde_json::json!({ "session": session_hex, "pubkey": peer_hex, "data": b64, "key": v.is_keyframe, "seq": v.seq }),
                    );
```
(Clone `peer_hex` into the recv closure alongside `session_hex`.)

- [ ] **Step 4: Bridge wrappers** in `client/src/lib/tauri-bridge.ts`:
```ts
export async function voiceStartScreenShare(fps: number, maxWidth: number, maxHeight: number): Promise<void> {
  return invoke<void>("voice_start_screen_share", { fps, maxWidth, maxHeight });
}
export async function voiceStopScreenShare(): Promise<void> {
  return invoke<void>("voice_stop_screen_share");
}
```

- [ ] **Step 5:** `cd client/src-tauri && cargo build && cargo test voice:: -- --test-threads=1` (green) and seam: `grep -c "voice_start_screen_share\|voice_stop_screen_share" src/main.rs` ≥ 2. `cd client && npx tsc --noEmit`.

- [ ] **Step 6:** Commit:
```bash
git add client/src-tauri/src/voice_bridge.rs client/src-tauri/src/main.rs client/src-tauri/src/voice/mod.rs client/src/lib/tauri-bridge.ts
git commit -m "client: voice_start/stop_screen_share commands; tag peer-video-frame with pubkey"
```

---

### Task 6: Frontend — Share button + useVoice share state

**Files:** Modify `client/src/hooks/useVoice.ts`, `client/src/components/VoiceControlBar.tsx`, theme CSS

- [ ] **Step 1: Add share state to useVoice.** Add `isSharing: boolean`, `startShare: () => Promise<void>`, `stopShare: () => Promise<void>` to the `UseVoice` interface. In the hook:
```ts
  const [isSharing, setIsSharing] = useState(false);
  const startShare = useCallback(async () => {
    await api.voiceStartScreenShare(30, 1280, 720);
    setIsSharing(true);
  }, []);
  const stopShare = useCallback(async () => {
    try { await api.voiceStopScreenShare(); } catch {}
    setIsSharing(false);
  }, []);
```
Reset `setIsSharing(false)` when a call ends (in `applyState`, inside the `if (!n.inCall)` block). Add the three to the returned object.

- [ ] **Step 2: Add the Share button** to `VoiceControlBar.tsx` in the `<div className="vcb-buttons">` group (before the leave button):
```tsx
        <button
          className={`vcb-btn${voice.isSharing ? " active" : ""}`}
          title={voice.isSharing ? "Stop sharing your screen" : "Share your screen"}
          aria-pressed={voice.isSharing}
          onClick={() => { void (voice.isSharing ? voice.stopShare() : voice.startShare()); }}
        ><span>&#x1F5A5;</span></button>
```
(🖥 = U+1F5A5 display screen. Use the `&#x...;` HTML-entity form, matching the file's existing mic/headphone icon style.)

- [ ] **Step 3:** Confirm `.vcb-btn` + `.vcb-btn.active` already style this (they style the mute/deafen buttons) — they do, in all 3 themes; no new class needed. If you want a distinct "live" tint when sharing, that's Phase E — skip for C2.

- [ ] **Step 4:** `cd client && npx tsc --noEmit` → clean.

- [ ] **Step 5:** Commit:
```bash
git add client/src/hooks/useVoice.ts client/src/components/VoiceControlBar.tsx
git commit -m "client ui: Share Screen button in the voice bar"
```

---

### Task 7: Frontend — per-peer video tiles (WebCodecs)

**Files:** Create `client/src/components/PeerVideoTiles.tsx`; modify a mount point (the chat/voice area — find where `VoiceControlBar` is rendered and mount alongside it) + theme CSS

- [ ] **Step 1: Create the per-peer video component.** Create `client/src/components/PeerVideoTiles.tsx`:
```tsx
import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

const H264_CODEC = "avc1.42E01E";

function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

interface FramePayload { session: string; pubkey: string; data: string; key: boolean; seq: number; }

/// One decoder+canvas per sharing session. Lazily created on the first frame
/// for a session; gated on the first keyframe.
class PeerDecoder {
  decoder: VideoDecoder;
  gotKey = false;
  constructor(private canvas: HTMLCanvasElement, onError: (e: string) => void) {
    const ctx = canvas.getContext("2d")!;
    this.decoder = new VideoDecoder({
      output: (frame) => {
        canvas.width = frame.displayWidth;
        canvas.height = frame.displayHeight;
        ctx.drawImage(frame, 0, 0, canvas.width, canvas.height);
        frame.close();
      },
      error: (e) => onError(String(e)),
    });
    this.decoder.configure({ codec: H264_CODEC, optimizeForLatency: true });
  }
  decode(p: FramePayload) {
    if (!this.gotKey && !p.key) return; // wait for a keyframe to start
    if (p.key) this.gotKey = true;
    try {
      this.decoder.decode(new EncodedVideoChunk({
        type: p.key ? "key" : "delta",
        timestamp: p.seq, // monotonic; advisory for a live stream
        data: b64ToBytes(p.data),
      }));
    } catch { /* drop */ }
  }
  close() { if (this.decoder.state !== "closed") this.decoder.close(); }
}

export default function PeerVideoTiles() {
  // session -> { pubkey, lastSeen }
  const [sessions, setSessions] = useState<Record<string, { pubkey: string }>>({});
  const canvasRefs = useRef<Record<string, HTMLCanvasElement | null>>({});
  const decoders = useRef<Record<string, PeerDecoder>>({});
  const lastSeen = useRef<Record<string, number>>({});

  useEffect(() => {
    const unlisten = listen<FramePayload>("voice://peer-video-frame", (e) => {
      const p = e.payload;
      lastSeen.current[p.session] = Date.now();
      setSessions((prev) => prev[p.session] ? prev : { ...prev, [p.session]: { pubkey: p.pubkey } });
      const dec = decoders.current[p.session];
      if (dec) dec.decode(p);
      // else: the canvas isn't mounted yet this tick; the next frame (or the
      // forced keyframe) will land once the decoder is created in the effect below.
    });
    return () => { unlisten.then((u) => u()); };
  }, []);

  // Create a decoder when a session's canvas mounts; reap sessions idle > 3s.
  useEffect(() => {
    for (const session of Object.keys(sessions)) {
      const canvas = canvasRefs.current[session];
      if (canvas && !decoders.current[session]) {
        decoders.current[session] = new PeerDecoder(canvas, () => {});
      }
    }
    const t = setInterval(() => {
      const now = Date.now();
      setSessions((prev) => {
        let changed = false;
        const next = { ...prev };
        for (const s of Object.keys(prev)) {
          if (now - (lastSeen.current[s] ?? 0) > 3000) {
            decoders.current[s]?.close();
            delete decoders.current[s];
            delete next[s];
            changed = true;
          }
        }
        return changed ? next : prev;
      });
    }, 1000);
    return () => clearInterval(t);
  }, [sessions]);

  const entries = Object.entries(sessions);
  if (entries.length === 0) return null;
  return (
    <div className="peer-video-tiles">
      {entries.map(([session, info]) => (
        <div key={session} className="peer-video-tile">
          <canvas ref={(el) => { canvasRefs.current[session] = el; }} className="peer-video-canvas" />
          <div className="peer-video-label">{info.pubkey.slice(0, 8)}&hellip; is sharing</div>
        </div>
      ))}
    </div>
  );
}
```

- [ ] **Step 2: Mount it.** Find where `<VoiceControlBar` is rendered (grep `client/src --include=*.tsx -l "VoiceControlBar"`); render `<PeerVideoTiles />` in the same view (e.g. above the message list or near the voice bar — somewhere visible while in a channel). Match the surrounding JSX.

- [ ] **Step 3: Theme CSS.** Add `.peer-video-tiles` (flex/grid, gap), `.peer-video-tile`, `.peer-video-canvas` (max-width 100%, black bg, `var(--xp-border)`), `.peer-video-label` (small, `var(--xp-text-muted)`) to ALL THREE `client/src/themes/*/theme.css`, colors via theme vars (CLAUDE.md). `background:#000` on the canvas is an acceptable letterbox literal.

- [ ] **Step 4:** `cd client && npx tsc --noEmit` → clean. `grep -l "peer-video-canvas" client/src/themes/*/theme.css` → 3 files.

- [ ] **Step 5:** Commit:
```bash
git add client/src/components/PeerVideoTiles.tsx client/src/components/<mount-file>.tsx client/src/themes/*/theme.css
git commit -m "client ui: per-peer screen-share video tiles (WebCodecs)"
```

---

### Task 8: Docs + verification gate

**Files:** Modify `docs/modules/voice-video-transport.md` (add the C2 share lifecycle), `docs/modules/tauri-commands.md` (the two commands), `ARCHITECTURE.md`

- [ ] **Step 1: Docs.** In `voice-video-transport.md` add a "Phase C2 — share lifecycle + viewer" section: `start_screen_share`/`stop_screen_share` (derive+offer video key, drive VideoSender from the capture loop, the keyframe-on-join + late-joiner re-offer via `on_peer_stream_joined`), the `voice_start/stop_screen_share` commands, the `voice://peer-video-frame` event now carrying `pubkey`, and the frontend `PeerVideoTiles` (lazy per-session WebCodecs decoder, key-gated, idle reap). `tauri-commands.md`: the two commands (params/returns/side effects, invoke names + bridge fns). `ARCHITECTURE.md`: one line — screen-sharing is now end-to-end (capture→encode→C1 transport→peer decode→WebCodecs tile); one sharer per call; game audio + polished UI are later phases.

- [ ] **Step 2: Full gate.**
```bash
cd /home/deez/farder && cargo test --workspace 2>&1 | grep -E "^test result" | tail -25
cd /home/deez/farder/client/src-tauri && cargo build && cargo test -- --test-threads=1 2>&1 | grep -E "^test result"
cd /home/deez/farder/client && npx tsc --noEmit && echo TSC_OK
cd /home/deez/farder && for c in voice_start_screen_share voice_stop_screen_share; do grep -q "$c" client/src-tauri/src/main.rs && grep -q "\"$c\"" client/src/lib/tauri-bridge.ts && echo "OK $c" || echo "MISSING $c"; done
```
All green (client single-threaded for the FARDER_DATA race; the `mock_capture_emits_frames_at_expected_fps` timing flake — re-run alone if it fails, pre-existing). If any voice/media test fails, STOP and report.

- [ ] **Step 3:** Commit:
```bash
git add docs/ ARCHITECTURE.md
git commit -m "docs: screen-share lifecycle + viewer (Phase C2)"
```

- [ ] **Step 4: Owner two-client runtime verification (report, not code).** UNVERIFIED until the owner's Windows run (CLAUDE.md). Steps: rebuild the client on BOTH machines (Phase C1 changed the media format earlier — both must be on the same build) and the server sidecar if the build changed it (C2 is client-only, but the C1 format is already in main). With two clients in the SAME voice channel on the deployed relay: client A clicks the Share button → client B should see A's screen appear as a video tile within a second or two (proves capture→encode→C1 transport→relay→decrypt→WebCodecs end to end). Test: B joins AFTER A is already sharing → B still gets a keyframe and sees the screen (keyframe-on-join + late re-offer). Repeat over a DIRECT server (no relay). Confirm Stop ends the tile. This run is the whole C2 deliverable: your screen, shared to another person, over the network.

---

## Self-review notes (done at plan time)

- **Spec coverage (Phase C, C2 portion):** the user-facing share trigger (Task 3/5/6 — Share button → start_screen_share → offer video key + enable track + drive VideoSender over the call); keyframe-on-join (Task 2 flag + Task 4 hook); late-joiner key re-offer (Task 4 via get_media_state); the viewer (Task 7 per-peer WebCodecs tile fed by the C1 `voice://peer-video-frame`); one-sharer-per-call (start rejects when already sharing); end-to-end over relay + direct (owner runtime, Task 8). The polished OS picker, LIVE-badge styling, and game-audio slider are Phase E; game audio is Phase D — both explicitly out of C2.
- **Type consistency:** `start_screen_share(fps, max_width, max_height)` ↔ `voice_start_screen_share` ↔ `voiceStartScreenShare(30,1280,720)`; `run_encode_loop(rx, encoder, stop, force_keyframe, sink)` 5-arg everywhere (Phase B caller updated); `VideoShareState{stop, force_keyframe, backend, video_key, thread}`; `offer_video_key(server, channel_id, &video_key)` reused by start + re-offer; the event `voice://peer-video-frame {session, pubkey, data, key, seq}` matches `PeerVideoTiles`' `FramePayload`.
- **Proven-path risk:** Tasks 1, 3, 4 modify the controller and `on_peer_stream_joined`, but only ADD fields/methods and an `if video_share.is_some()` branch — the audio join, send, recv, and the not-sharing `on_peer_stream_joined` path are unchanged. The full voice suite is the regression gate after each. Task 2 adds one flag-check per frame to `run_encode_loop` (Phase B preview passes a never-set flag — behavior identical).
- **Testability split:** headless — `offer_video_key` wrap/offer, the force-keyframe flag (Task 2 mock-capture test), the controller start/stop offer+enable+disable (Task 3, via FakeServerSession), the re-offer-on-join (Task 4). Owner-runtime — the real capture→encode→send thread and the two-client receive+decode (Windows, Task 8). The `!Send` encoder stays on its capture thread (same pattern as Phase B).
- **Known judgment calls:** the controller tests lean on the existing `FakeServerSession` recording shape — if it doesn't already record offered/enabled/disabled tracks, the implementer extends the test double minimally (flagged in Tasks 3-4). The video tile reaps a session after 3s idle (no explicit "stopped sharing" frontend signal in C2 — TrackDisabled stops the frames; a precise stop signal is a Phase E nicety). `PeerVideoTiles` mount point is found by grep at implement time.
