# Screensharing Phase C1 — Video Transport in the Voice Controller

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generalize the client voice controller so a peer's `(session, TrackKind)` audio AND video frame streams route, key, seal, and reassemble independently — adding the video send + video receive primitives — without disturbing the proven audio path.

**Architecture:** Today the inbound dispatcher and the per-peer key map are keyed by `session_id` alone, and `TrackKind` is discarded from the key-offer flow. But one peer's session carries both audio and video frames (distinguished by the Phase A outer header's `track_kind`), so video would wrongly land in the audio recv task and fail to decrypt. C1 re-keys the dispatcher and key map by `(SessionId, TrackKind)`, threads `kind` through the key offer, adds H.264 video seal/open + a video send function + a video receive task (which reassembles, decrypts, and forwards encoded frames to the frontend). The server is already fully TrackKind-agnostic (no server changes). The end-user "share" trigger + the per-peer video UI are **Phase C2** — C1 builds and unit-tests the transport primitives; the audio path stays fully working and regression-tested.

**Tech Stack:** Rust (the existing voice pipeline, `farder-crypto` media seal/open, `farder-protocol::media_datagram` fragment/reassemble from Phase A, `openh264` from Phase B).

**Spec:** `docs/superpowers/specs/2026-06-12-screensharing-design.md` (Phase C).

**Branch:** create `screenshare-phaseC1` from `main` before Task 1. Finish with ff-merge + push.

**Scope note:** C1 is the transport foundation; it produces mergeable, tested software (audio still works; video send/recv primitives proven headlessly) but does NOT yet make screen-sharing reachable by a user — that's Phase C2 (the start/stop-share commands that derive+offer the video key, enable the Video track, drive the video send from the Phase B capture loop, keyframe-on-join, late-joiner re-offer, and the frontend per-peer video tile). No networking-runtime test here; the two-client end-to-end is verified in C2 on Windows.

---

## Verified codebase facts (read 2026-06-13 — exact)

- **Inbound dispatcher** (`client/src-tauri/src/voice/mod.rs` `MediaInboundDispatcher`): `routes: Mutex<HashMap<SessionId, mpsc::UnboundedSender<Bytes>>>`; `register(session_id, tx)`, `unregister(&session_id)`, `dispatch(bytes)` parses `OuterHeader::parse(&bytes)` → `header.session_id` → routes. `OuterHeader` (Phase A, `farder_protocol::media_datagram`) already exposes both `session_id` and `track_kind`.
- **ActiveCall state** (`voice/mod.rs:411-429`): `peer_keys: HashMap<SessionId, ([u8;32], PublicKey)>` (one key per session — audio only), `peers: HashMap<SessionId, PeerEntry>`, `peer_rings`, `peer_status`, etc. `PeerEntry { pubkey, recv_handle: JoinHandle<()>, datagram_tx }`.
- **Key offer** (`voice/mod.rs:816` `on_stream_key_offer(session_id, sender_pubkey, wrapped_key)`): unwraps via `farder_crypto::media::unwrap_stream_key(&wrapped, &my_sk, sender.as_bytes())` → `call.peer_keys.insert(session_id, (key, sender_pubkey))`. **Does NOT receive `kind`.** `peer_pubkey_for(&session_id)` reads `peer_keys[session].1`.
- **Bridge dispatch** (`client/src-tauri/src/bridge.rs`): `StreamKeyOffer { session_id, sender, wrapped_key, .. }` (line 115) — **`kind` discarded by `..`** → `on_stream_key_offer(session_id, sender, wrapped_key)`. `TrackEnabled { session_id, kind, .. }` (124) and `TrackDisabled` (148) each have a hardcoded `if !matches!(kind, TrackKind::Audio) { return; }` filter. `TrackActivityChanged` (169) passes `kind` through unfiltered.
- **on_peer_track_enabled** (`voice/mod.rs:846`): filters non-Audio (852); looks up `call.peer_keys.get(&session_id)` for the key; builds a `PeerPcmRing`, registers a mixer ring, `dispatcher.register(session_id, tx)`, spawns `recv::run(RecvTaskConfig{session_id, stream_key, deafened, datagram_rx, pcm_ring})`, inserts `PeerEntry`, pushes a `VoicePeer`, emits `voice://state-changed`. **on_peer_track_disabled** (~922) removes the peer (audio-only).
- **Audio seal/open** (`crates/farder-crypto/src/media.rs`): `seal_audio_packet_to_wire(key, seq, session_id, speaker_pk, opus) -> Vec<u8>` builds `build_audio_header` (28 bytes: `MEDIA_FRAME_VERSION 0x02 | MEDIA_FRAME_TYPE_AUDIO 0x01 | track_id 0 | codec_id 0 | seq(8 BE) | session_id(16)`) then `seal_media_frame(key,seq,session,&hdr,speaker,opus)`; `open_audio_wire_frame(key, wire) -> (seq, speaker_pk, opus)` checks `wire[1]==MEDIA_FRAME_TYPE_AUDIO`. `MEDIA_FRAME_TYPE_VIDEO=0x02`, `MEDIA_FRAME_HEADER_LEN=28`, `SessionId=[u8;16]` are exported. `seal_media_frame`/`open_media_frame` are the generic AEAD primitives (header is the AAD).
- **Audio send** (`voice/send.rs`): after sealing, `for dgram in fragment(TrackKind::Audio, &session_id, frame_id, &frame_bytes, DEFAULT_MAX_DGRAM_PAYLOAD) { (datagram_sink)(Bytes::from(dgram)); }`. The sink is `Box<dyn Fn(Bytes)+Send+Sync>` built in `voice/mod.rs:588` as `move |b| { let _ = server_for_sink.send_datagram(b); }`. `ServerSession::send_datagram(&self, Bytes) -> Result<(), String>` → `quinn::Connection::send_datagram`.
- **Audio recv** (`voice/recv.rs`): `RecvTaskConfig{session_id, stream_key, deafened, datagram_rx, pcm_ring}`; `run(cfg)` loops: deafen-check → `OuterHeader::parse(&bytes)` → `reassembler.accept(&header, payload)` → `open_audio_wire_frame(&cfg.stream_key, &sealed)` → jitter/opus-decode → `pcm_ring.push_frame`. `Reassembler::new()` from `farder_protocol::media_datagram`.
- **Phase B** (`client/src-tauri/src/video_encoder.rs`): `EncodedFrame { data: Vec<u8> (Annex-B H.264), is_keyframe: bool, timestamp_ms: u64 }`.
- **Emitter** (`voice/mod.rs`): `trait VoiceEventEmitter { fn emit(&self, event: &str, payload: serde_json::Value); }`; the controller holds `self.emitter: Arc<dyn VoiceEventEmitter>`.
- **Server is TrackKind-agnostic** (`crates/farder-server` handlers + `media_stream::on_frame_ingress`): EnableTrack/DisableTrack/OfferStreamKey carry `kind`; ingress routes by `(session, track_kind)` + per-kind bandwidth cap. **No server changes in C1.**

---

### Task 1: Re-key the inbound dispatcher by (session, kind)

**Files:**
- Modify: `client/src-tauri/src/voice/mod.rs` (`MediaInboundDispatcher` + its tests + the one `register` caller)

- [ ] **Step 1: Update the dispatcher tests to the new key.** In `voice/mod.rs` `mod dispatcher_tests`, the helper `outer_audio_dgram` already builds a `TrackKind::Audio` outer datagram. Change every `dispatcher.register(sid, tx)` to `dispatcher.register(sid, TrackKind::Audio, tx)` and every `dispatcher.unregister(&sid)` to `dispatcher.unregister(&sid, TrackKind::Audio)`. Add one new test proving audio and video for the SAME session route to DIFFERENT receivers:

```rust
    #[tokio::test]
    async fn dispatch_separates_audio_and_video_for_same_session() {
        use farder_protocol::media_datagram::OuterHeader;
        use farder_protocol::server::TrackKind;
        let dispatcher = MediaInboundDispatcher::default();
        let sid: SessionId = [5u8; 16];
        let (atx, mut arx) = mpsc::unbounded_channel();
        let (vtx, mut vrx) = mpsc::unbounded_channel();
        dispatcher.register(sid, TrackKind::Audio, atx).await;
        dispatcher.register(sid, TrackKind::Video, vtx).await;

        fn dgram(sid: &SessionId, kind: TrackKind) -> Bytes {
            let mut v = Vec::new();
            OuterHeader { track_kind: kind, session_id: *sid, frame_id: 0, frag_index: 0, frag_count: 1 }
                .write_to(&mut v);
            v.extend_from_slice(b"payload");
            Bytes::from(v)
        }
        dispatcher.dispatch(dgram(&sid, TrackKind::Video)).await;
        dispatcher.dispatch(dgram(&sid, TrackKind::Audio)).await;
        assert!(arx.try_recv().is_ok(), "audio route gets the audio datagram");
        assert!(vrx.try_recv().is_ok(), "video route gets the video datagram");
        assert!(arx.try_recv().is_err(), "audio route does NOT get the video datagram");
    }
```

- [ ] **Step 2: Run to verify failure.**

Run: `cd client/src-tauri && cargo test voice::dispatcher_tests`
Expected: compile FAILURE (`register`/`unregister` arity changed).

- [ ] **Step 3: Re-key the dispatcher.** Replace the `MediaInboundDispatcher` impl:

```rust
/// Routes inbound media datagrams to the right RecvTask by (session_id, track_kind).
/// A peer's session carries BOTH audio and video frame streams (distinguished by
/// the cleartext outer header's track_kind); each track gets its own recv task.
#[derive(Default)]
pub struct MediaInboundDispatcher {
    routes: Mutex<HashMap<(SessionId, TrackKind), mpsc::UnboundedSender<Bytes>>>,
}

impl MediaInboundDispatcher {
    pub async fn register(&self, session_id: SessionId, kind: TrackKind, tx: mpsc::UnboundedSender<Bytes>) {
        self.routes.lock().await.insert((session_id, kind), tx);
    }

    pub async fn unregister(&self, session_id: &SessionId, kind: TrackKind) {
        self.routes.lock().await.remove(&(*session_id, kind));
    }

    pub async fn dispatch(&self, bytes: Bytes) {
        use farder_protocol::media_datagram::OuterHeader;
        let (session_id, track_kind) = match OuterHeader::parse(&bytes) {
            Ok((header, _payload)) => (header.session_id, header.track_kind),
            Err(_) => return,
        };
        let routes = self.routes.lock().await;
        if let Some(tx) = routes.get(&(session_id, track_kind)) {
            let _ = tx.send(bytes);
        }
    }
}
```

(`TrackKind` is already imported in mod.rs via `use farder_protocol::server::{TrackKind, VoiceMember};`.)

- [ ] **Step 4: Update the one production caller.** In `on_peer_track_enabled` (~line 891), change `dispatcher.register(session_id, tx_for_register).await;` to `dispatcher.register(session_id, TrackKind::Audio, tx_for_register).await;`. In `on_peer_track_disabled` (find the `unregister` call, if any — if it doesn't unregister, leave it; Task 6 adds (session,kind) teardown), and any other `register`/`unregister` calls, add `TrackKind::Audio`. Grep: `grep -n "\.register(\|\.unregister(" client/src-tauri/src/voice/mod.rs` and fix each to pass `TrackKind::Audio`.

- [ ] **Step 5: Run the tests.**

Run: `cd client/src-tauri && cargo test voice:: -- --test-threads=1`
Expected: all voice tests PASS (dispatcher routes by (session,kind); the audio path registers (session, Audio); the new separation test passes).

- [ ] **Step 6: Commit.**

```bash
git add client/src-tauri/src/voice/mod.rs
git commit -m "client: route inbound media datagrams by (session, track_kind)"
```

---

### Task 2: Re-key peer_keys + thread `kind` through the key offer

**Files:**
- Modify: `client/src-tauri/src/voice/mod.rs` (`ActiveCall`, `on_stream_key_offer`, `on_peer_track_enabled`, `peer_pubkey_for`)
- Modify: `client/src-tauri/src/bridge.rs` (pass `kind` to `on_stream_key_offer`)

- [ ] **Step 1: Re-key the state + thread kind.** In `voice/mod.rs` `ActiveCall`, replace the `peer_keys` field:

```rust
    /// Per-(peer-session, track) stream keys delivered via StreamKeyOffer events.
    /// A peer's session has independent audio + video keys.
    peer_keys: HashMap<(SessionId, TrackKind), [u8; 32]>,
    /// Per-session peer public key (set on the first key offer for the session).
    peer_pubkeys: HashMap<SessionId, PublicKey>,
```

Update every `ActiveCall { ... }` construction site (grep `peer_keys:` in mod.rs — the join flow ~line 645) to initialize both `peer_keys: HashMap::new(), peer_pubkeys: HashMap::new(),`.

Update `on_stream_key_offer` to take `kind` and store by `(session, kind)`:

```rust
    pub async fn on_stream_key_offer(
        &self,
        session_id: SessionId,
        kind: TrackKind,
        sender_pubkey: PublicKey,
        wrapped_key: Vec<u8>,
    ) {
        let mut inner = self.inner.lock().await;
        let call = match inner.active.as_mut() {
            Some(c) => c,
            None => return,
        };
        let keypair = call.server.my_keypair();
        let my_sk = *keypair.signing_key_bytes();
        match farder_crypto::media::unwrap_stream_key(&wrapped_key, &my_sk, sender_pubkey.as_bytes()) {
            Ok(key) => {
                call.peer_keys.insert((session_id, kind), key);
                call.peer_pubkeys.insert(session_id, sender_pubkey);
            }
            Err(e) => eprintln!("[voice] unwrap_stream_key: {e}"),
        }
    }
```

Update `peer_pubkey_for` to read `peer_pubkeys`:

```rust
    pub async fn peer_pubkey_for(&self, session_id: &SessionId) -> Option<PublicKey> {
        let inner = self.inner.lock().await;
        inner.active.as_ref().and_then(|c| c.peer_pubkeys.get(session_id).cloned())
    }
```

In `on_peer_track_enabled`, change the key lookup from `call.peer_keys.get(&session_id)` to `call.peer_keys.get(&(session_id, TrackKind::Audio))`:

```rust
        let stream_key = match call.peer_keys.get(&(session_id, TrackKind::Audio)) {
            Some(k) => *k,
            None => {
                eprintln!("[voice] TrackEnabled(Audio) for unknown session; missing StreamKeyOffer?");
                return;
            }
        };
```

(If `on_stream_key_offer`/`peer_pubkey_for` have unit tests in mod.rs that call them with the old signature, update those calls to pass `TrackKind::Audio`.)

- [ ] **Step 2: Thread `kind` in the bridge.** In `bridge.rs` the `StreamKeyOffer` arm (line 115), capture `kind` and pass it:

```rust
        ServerEvent::StreamKeyOffer { session_id, sender, kind, wrapped_key, .. } => {
            if let Some(ctrl) = app.try_state::<Arc<crate::voice::VoiceController>>() {
                let ctrl = (*ctrl).clone();
                tokio::spawn(async move {
                    ctrl.on_stream_key_offer(session_id, kind, sender, wrapped_key).await;
                });
            }
            Ok(())
        }
```

- [ ] **Step 3: Build + test.**

Run: `cd client/src-tauri && cargo build && cargo test voice:: -- --test-threads=1`
Expected: clean build, all voice tests pass (the audio key-offer → track-enabled flow now uses `(session, Audio)` and still works).

- [ ] **Step 4: Commit.**

```bash
git add client/src-tauri/src/voice/mod.rs client/src-tauri/src/bridge.rs
git commit -m "client: key stream keys by (session, track_kind); thread kind through the key offer"
```

---

### Task 3: H.264 video seal/open helpers (farder-crypto)

**Files:**
- Modify: `crates/farder-crypto/src/media.rs`

- [ ] **Step 1: Write the failing tests.** Append inside `mod tests` in `media.rs`:

```rust
    #[test]
    fn seal_open_video_wire_roundtrip() {
        let key = derive_stream_key();
        let session = fixed_session_id();
        let speaker = fixed_speaker_pk();
        let h264 = vec![0u8, 0, 0, 1, 0x67, 1, 2, 3]; // stand-in Annex-B bytes
        let wire = seal_video_frame_to_wire(&key, 9, &session, &speaker, &h264).unwrap();
        // Inner header type byte must be VIDEO (0x02 at offset 1).
        assert_eq!(wire[1], MEDIA_FRAME_TYPE_VIDEO);
        let (seq, got_speaker, got_h264) = open_video_wire_frame(&key, &wire).unwrap();
        assert_eq!(seq, 9);
        assert_eq!(got_speaker, speaker);
        assert_eq!(got_h264, h264);
    }

    #[test]
    fn open_video_rejects_audio_frame() {
        let key = derive_stream_key();
        let session = fixed_session_id();
        let speaker = fixed_speaker_pk();
        let audio_wire = seal_audio_packet_to_wire(&key, 1, &session, &speaker, b"opus").unwrap();
        assert!(open_video_wire_frame(&key, &audio_wire).is_err(), "video open must reject an audio frame");
    }
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p farder-crypto seal_open_video_wire_roundtrip`
Expected: compile FAILURE (`seal_video_frame_to_wire`/`open_video_wire_frame` not found).

- [ ] **Step 3: Implement** — add to `media.rs` (next to the audio wire helpers, mirroring them with the VIDEO type byte):

```rust
/// Build a 28-byte VIDEO media-frame header (mirrors build_audio_header).
fn build_video_header(seq: u64, session_id: &SessionId) -> [u8; MEDIA_FRAME_HEADER_LEN] {
    let mut hdr = [0u8; MEDIA_FRAME_HEADER_LEN];
    hdr[0] = MEDIA_FRAME_VERSION;
    hdr[1] = MEDIA_FRAME_TYPE_VIDEO;
    hdr[4..12].copy_from_slice(&seq.to_be_bytes());
    hdr[12..28].copy_from_slice(session_id);
    hdr
}

/// One-shot: seal an H.264 frame payload into the complete video wire frame
/// `header(28) || ciphertext+tag`. The header is the AEAD AAD.
pub fn seal_video_frame_to_wire(
    key: &[u8; 32],
    seq: u64,
    session_id: &SessionId,
    speaker_pk: &[u8; 32],
    h264_payload: &[u8],
) -> Result<Vec<u8>> {
    let hdr = build_video_header(seq, session_id);
    let ciphertext = seal_media_frame(key, seq, session_id, &hdr, speaker_pk, h264_payload)?;
    let mut wire = Vec::with_capacity(MEDIA_FRAME_HEADER_LEN + ciphertext.len());
    wire.extend_from_slice(&hdr);
    wire.extend_from_slice(&ciphertext);
    Ok(wire)
}

/// One-shot: open + verify a full video wire frame. Returns `(seq, speaker_pk, h264_payload)`.
pub fn open_video_wire_frame(key: &[u8; 32], wire: &[u8]) -> Result<(u64, [u8; 32], Vec<u8>)> {
    if wire.len() < MEDIA_FRAME_HEADER_LEN {
        return Err(anyhow!("video wire frame too short: {} bytes", wire.len()));
    }
    if wire[0] != MEDIA_FRAME_VERSION {
        return Err(anyhow!("bad media frame version: 0x{:02x}", wire[0]));
    }
    if wire[1] != MEDIA_FRAME_TYPE_VIDEO {
        return Err(anyhow!("expected video frame type 0x{:02x}, got 0x{:02x}", MEDIA_FRAME_TYPE_VIDEO, wire[1]));
    }
    let seq = u64::from_be_bytes(wire[4..12].try_into().unwrap());
    let mut session_id = [0u8; SESSION_ID_LEN];
    session_id.copy_from_slice(&wire[12..28]);
    let header_aad = &wire[..MEDIA_FRAME_HEADER_LEN];
    let ciphertext = &wire[MEDIA_FRAME_HEADER_LEN..];
    let (speaker_pk, h264) = open_media_frame(key, seq, &session_id, header_aad, ciphertext)?;
    Ok((seq, speaker_pk, h264))
}
```

- [ ] **Step 4: Run the tests.**

Run: `cargo test -p farder-crypto`
Expected: all crypto tests PASS (2 new + existing).

- [ ] **Step 5: Commit.**

```bash
git add crates/farder-crypto/src/media.rs
git commit -m "crypto: H.264 video frame seal/open wire helpers"
```

---

### Task 4: Video send function

**Files:**
- Create: `client/src-tauri/src/voice/send_video.rs`
- Modify: `client/src-tauri/src/voice/mod.rs` (add `pub mod send_video;` under the existing voice submodule declarations)

- [ ] **Step 1: Create the module with tests.** Create `client/src-tauri/src/voice/send_video.rs`:

```rust
//! Video send path: take encoded H.264 frames (from the Phase B encoder),
//! seal them with the peer-shared VIDEO stream key, fragment (Phase A), and
//! push each datagram to the connection. Mirrors voice::send but for video:
//! no Opus/APM — the frames are already H.264.
//!
//! The keyframe flag travels INSIDE the encrypted payload (1 leading byte) so
//! the receiver can tag the WebCodecs chunk key/delta without a side channel.

use crate::video_encoder::EncodedFrame;
use crate::voice::SessionId;
use bytes::Bytes;
use farder_crypto::media::seal_video_frame_to_wire;
use farder_protocol::media_datagram::{fragment, DEFAULT_MAX_DGRAM_PAYLOAD};
use farder_protocol::server::TrackKind;

/// Build the sealed+fragmented datagrams for one encoded video frame. Pure
/// (no I/O) so it's unit-testable; the caller sends each datagram.
/// `seq` is monotonic per stream (AEAD nonce); `frame_id` groups fragments.
pub fn build_video_datagrams(
    stream_key: &[u8; 32],
    session_id: &SessionId,
    speaker_pk: &[u8; 32],
    seq: u64,
    frame_id: u32,
    frame: &EncodedFrame,
) -> Result<Vec<Vec<u8>>, String> {
    // Payload = [keyframe:1][H.264 Annex-B...] (encrypted together).
    let mut payload = Vec::with_capacity(1 + frame.data.len());
    payload.push(frame.is_keyframe as u8);
    payload.extend_from_slice(&frame.data);
    let wire = seal_video_frame_to_wire(stream_key, seq, session_id, speaker_pk, &payload)
        .map_err(|e| format!("seal video: {e}"))?;
    Ok(fragment(TrackKind::Video, session_id, frame_id, &wire, DEFAULT_MAX_DGRAM_PAYLOAD))
}

/// A reusable sender that seals+fragments each frame and pushes datagrams to a
/// sink, advancing its own seq/frame_id counters.
pub struct VideoSender {
    stream_key: [u8; 32],
    session_id: SessionId,
    speaker_pk: [u8; 32],
    seq: u64,
    frame_id: u32,
}

impl VideoSender {
    pub fn new(stream_key: [u8; 32], session_id: SessionId, speaker_pk: [u8; 32]) -> Self {
        Self { stream_key, session_id, speaker_pk, seq: 0, frame_id: 0 }
    }

    /// Seal+fragment `frame` and push each datagram via `sink`.
    pub fn send(&mut self, frame: &EncodedFrame, mut sink: impl FnMut(Bytes)) {
        match build_video_datagrams(&self.stream_key, &self.session_id, &self.speaker_pk, self.seq, self.frame_id, frame) {
            Ok(dgrams) => {
                for d in dgrams {
                    sink(Bytes::from(d));
                }
            }
            Err(e) => eprintln!("[voice::send_video] dropped a frame: {e}"),
        }
        self.seq = self.seq.wrapping_add(1);
        self.frame_id = self.frame_id.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::media::open_video_wire_frame;
    use farder_protocol::media_datagram::{OuterHeader, Reassembler};

    fn enc_frame(key: bool, body: &[u8]) -> EncodedFrame {
        EncodedFrame { data: body.to_vec(), is_keyframe: key, timestamp_ms: 0 }
    }

    #[test]
    fn small_frame_seals_fragments_and_roundtrips_with_keyframe_flag() {
        let key = [0x22u8; 32];
        let session: SessionId = [0x33u8; 16];
        let speaker = [0x44u8; 32];
        let frame = enc_frame(true, &[0, 0, 0, 1, 0x67, 9, 8, 7]);
        let dgrams = build_video_datagrams(&key, &session, &speaker, 3, 3, &frame).unwrap();
        assert_eq!(dgrams.len(), 1, "small frame is one datagram");

        // Receiver: parse outer header, reassemble, open, split keyframe byte.
        let mut reasm = Reassembler::new();
        let (header, payload) = OuterHeader::parse(&dgrams[0]).unwrap();
        assert_eq!(header.track_kind, TrackKind::Video);
        let sealed = reasm.accept(&header, payload).unwrap();
        let (seq, got_speaker, plaintext) = open_video_wire_frame(&key, &sealed).unwrap();
        assert_eq!(seq, 3);
        assert_eq!(got_speaker, speaker);
        assert_eq!(plaintext[0], 1, "keyframe byte == 1");
        assert_eq!(&plaintext[1..], &frame.data[..]);
    }

    #[test]
    fn large_frame_fragments_and_reassembles() {
        let key = [1u8; 32];
        let session: SessionId = [2u8; 16];
        let speaker = [3u8; 32];
        let big: Vec<u8> = (0..6000u32).map(|i| (i % 251) as u8).collect();
        let frame = enc_frame(false, &big);
        let dgrams = build_video_datagrams(&key, &session, &speaker, 0, 0, &frame).unwrap();
        assert!(dgrams.len() > 1, "a 6KB frame must span multiple datagrams");

        let mut reasm = Reassembler::new();
        let mut sealed = None;
        for d in dgrams.iter().rev() {
            let (h, p) = OuterHeader::parse(d).unwrap();
            if let Some(s) = reasm.accept(&h, p) {
                sealed = Some(s);
            }
        }
        let (_, _, plaintext) = open_video_wire_frame(&key, &sealed.unwrap()).unwrap();
        assert_eq!(plaintext[0], 0, "keyframe byte == 0");
        assert_eq!(&plaintext[1..], &big[..]);
    }

    #[test]
    fn sender_advances_seq_and_pushes() {
        let mut s = VideoSender::new([7u8; 32], [8u8; 16], [9u8; 32]);
        let collected = std::cell::RefCell::new(Vec::new());
        s.send(&enc_frame(true, b"a"), |b| collected.borrow_mut().push(b));
        s.send(&enc_frame(false, b"b"), |b| collected.borrow_mut().push(b));
        assert_eq!(collected.borrow().len(), 2, "two small frames -> two datagrams");
        // seq advanced so the two frames have distinct nonces (different bytes).
        assert_ne!(collected.borrow()[0], collected.borrow()[1]);
    }
}
```

- [ ] **Step 2: Register the submodule.** In `voice/mod.rs`, find the `pub mod send;` / `pub mod recv;` lines and add `pub mod send_video;`.

- [ ] **Step 3: Run the tests.**

Run: `cd client/src-tauri && cargo test voice::send_video::`
Expected: 3 tests PASS (seal+fragment+reassemble+open roundtrip incl. the keyframe byte; large-frame fragmentation; sender advances).

- [ ] **Step 4: Commit.**

```bash
git add client/src-tauri/src/voice/send_video.rs client/src-tauri/src/voice/mod.rs
git commit -m "client: video send path (seal H.264 + fragment + datagram sink)"
```

---

### Task 5: Video receive task

**Files:**
- Create: `client/src-tauri/src/voice/recv_video.rs`
- Modify: `client/src-tauri/src/voice/mod.rs` (`pub mod recv_video;`)

- [ ] **Step 1: Create the module with tests.** Create `client/src-tauri/src/voice/recv_video.rs`:

```rust
//! Video receive task (one per remote peer's video session): inbound datagram
//! -> reassemble -> open with the peer's video stream key -> split the keyframe
//! byte -> hand the encoded H.264 to a sink (the controller forwards it to the
//! webview, which decodes via WebCodecs). No jitter buffer / no decode here —
//! video timing is handled by the WebCodecs decoder on the frontend.

use crate::voice::SessionId;
use bytes::Bytes;
use farder_crypto::media::open_video_wire_frame;
use farder_protocol::media_datagram::{OuterHeader, Reassembler};
use tokio::sync::mpsc;

/// One decoded-from-the-wire (still H.264-encoded) video frame for the frontend.
pub struct VideoOut {
    pub data: Vec<u8>,     // H.264 Annex-B
    pub is_keyframe: bool,
    pub seq: u64,
}

pub struct RecvVideoConfig {
    pub session_id: SessionId,
    pub stream_key: [u8; 32],
    pub datagram_rx: mpsc::UnboundedReceiver<Bytes>,
}

/// Run the video recv task. Calls `sink` for each fully-received, decrypted
/// frame. Returns when the datagram channel closes.
pub async fn run(mut cfg: RecvVideoConfig, mut sink: impl FnMut(VideoOut)) {
    let mut reassembler = Reassembler::new();
    while let Some(bytes) = cfg.datagram_rx.recv().await {
        let (header, payload) = match OuterHeader::parse(&bytes) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let sealed = match reassembler.accept(&header, payload) {
            Some(s) => s,
            None => continue,
        };
        let (seq, _speaker_pk, plaintext) = match open_video_wire_frame(&cfg.stream_key, &sealed) {
            Ok(t) => t,
            Err(_) => continue, // wrong key / not yet keyed / corrupt -> drop
        };
        if plaintext.is_empty() {
            continue;
        }
        let is_keyframe = plaintext[0] == 1;
        sink(VideoOut { data: plaintext[1..].to_vec(), is_keyframe, seq });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video_encoder::EncodedFrame;
    use crate::voice::send_video::build_video_datagrams;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn receives_and_emits_a_keyframe() {
        let key = [0x55u8; 32];
        let session: SessionId = [0x66u8; 16];
        let speaker = [0x77u8; 32];
        let frame = EncodedFrame { data: vec![0, 0, 0, 1, 0x65, 1, 2], is_keyframe: true, timestamp_ms: 0 };
        let dgrams = build_video_datagrams(&key, &session, &speaker, 0, 0, &frame).unwrap();

        let (tx, rx) = mpsc::unbounded_channel();
        for d in dgrams { tx.send(Bytes::from(d)).unwrap(); }
        drop(tx);

        let out = Arc::new(Mutex::new(Vec::<VideoOut>::new()));
        let out2 = out.clone();
        run(RecvVideoConfig { session_id: session, stream_key: key, datagram_rx: rx }, move |v| {
            out2.lock().unwrap().push(v);
        }).await;

        let frames = out.lock().unwrap();
        assert_eq!(frames.len(), 1);
        assert!(frames[0].is_keyframe);
        assert_eq!(frames[0].data, frame.data);
    }

    #[tokio::test]
    async fn wrong_key_drops_frame() {
        let key = [1u8; 32];
        let session: SessionId = [2u8; 16];
        let frame = EncodedFrame { data: vec![9, 9, 9], is_keyframe: false, timestamp_ms: 0 };
        let dgrams = build_video_datagrams(&key, &session, &[3u8; 32], 0, 0, &frame).unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        for d in dgrams { tx.send(Bytes::from(d)).unwrap(); }
        drop(tx);

        let out = Arc::new(Mutex::new(Vec::<VideoOut>::new()));
        let out2 = out.clone();
        // Decrypt with the WRONG key.
        run(RecvVideoConfig { session_id: session, stream_key: [0xFFu8; 32], datagram_rx: rx }, move |v| {
            out2.lock().unwrap().push(v);
        }).await;
        assert!(out.lock().unwrap().is_empty(), "a frame under the wrong key must be dropped, not panic");
    }
}
```

- [ ] **Step 2: Register the submodule.** In `voice/mod.rs` add `pub mod recv_video;` near `pub mod recv;`.

- [ ] **Step 3: Run the tests.**

Run: `cd client/src-tauri && cargo test voice::recv_video::`
Expected: 2 tests PASS (receives + emits a keyframe with the correct H.264; wrong key drops silently).

- [ ] **Step 4: Commit.**

```bash
git add client/src-tauri/src/voice/recv_video.rs client/src-tauri/src/voice/mod.rs
git commit -m "client: video recv task (reassemble + open + emit encoded frames)"
```

---

### Task 6: Wire video recv into the controller; remove the audio-only filters

**Files:**
- Modify: `client/src-tauri/src/voice/mod.rs` (`on_peer_track_enabled`, `on_peer_track_disabled`, a new `on_peer_video_track_enabled`)
- Modify: `client/src-tauri/src/bridge.rs` (remove the `TrackEnabled`/`TrackDisabled` audio-only filters)

- [ ] **Step 1: Add a video-recv setup path on the controller.** In `voice/mod.rs`, generalize `on_peer_track_enabled` to branch by kind, and add the video handler. Replace the early `if !matches!(kind, TrackKind::Audio) { return; }` (line 852) with a dispatch:

```rust
    pub async fn on_peer_track_enabled(
        &self,
        session_id: SessionId,
        peer_pubkey: PublicKey,
        kind: TrackKind,
    ) {
        match kind {
            TrackKind::Audio => self.on_peer_audio_track_enabled(session_id, peer_pubkey).await,
            TrackKind::Video => self.on_peer_video_track_enabled(session_id, peer_pubkey).await,
        }
    }
```

Rename the existing body (everything after the old filter, lines 855-end of the function) into a new `async fn on_peer_audio_track_enabled(&self, session_id, peer_pubkey)` (drop the `kind` param; it's always Audio). Keep its logic identical (it already registers `(session, Audio)` from Task 1).

Add the video handler. It mirrors the audio recv-setup but spawns `recv_video::run` with an emit sink, and tracks the peer's video recv handle. Because `PeerEntry` currently holds one `recv_handle` (the audio one), and a peer may have BOTH audio and video recv tasks, add a parallel map on `ActiveCall`:

```rust
    /// Per-session video recv tasks (independent of the audio `peers` map).
    video_peers: HashMap<SessionId, VideoPeerEntry>,
```

with

```rust
struct VideoPeerEntry {
    recv_handle: JoinHandle<()>,
    #[allow(dead_code)]
    datagram_tx: mpsc::UnboundedSender<Bytes>,
}
```

(initialize `video_peers: HashMap::new()` at every `ActiveCall { ... }` construction). The handler:

```rust
    async fn on_peer_video_track_enabled(&self, session_id: SessionId, _peer_pubkey: PublicKey) {
        let mut inner = self.inner.lock().await;
        let call = match inner.active.as_mut() {
            Some(c) => c,
            None => return,
        };
        if call.video_peers.contains_key(&session_id) {
            return; // already running
        }
        let stream_key = match call.peer_keys.get(&(session_id, TrackKind::Video)) {
            Some(k) => *k,
            None => {
                eprintln!("[voice] TrackEnabled(Video) for session with no video key; missing StreamKeyOffer(Video)?");
                return;
            }
        };
        let (tx, rx) = mpsc::unbounded_channel::<Bytes>();
        let dispatcher = call.server.dispatcher();
        let tx_for_register = tx.clone();
        tokio::spawn(async move {
            dispatcher.register(session_id, TrackKind::Video, tx_for_register).await;
        });

        let emitter = self.emitter.clone();
        let session_hex = hex::encode(session_id);
        let recv_handle = tokio::spawn(async move {
            crate::voice::recv_video::run(
                crate::voice::recv_video::RecvVideoConfig { session_id, stream_key, datagram_rx: rx },
                move |v| {
                    use base64::Engine;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&v.data);
                    emitter.emit(
                        "voice://peer-video-frame",
                        serde_json::json!({ "session": session_hex, "data": b64, "key": v.is_keyframe, "seq": v.seq }),
                    );
                },
            )
            .await;
        });

        call.video_peers.insert(session_id, VideoPeerEntry { recv_handle, datagram_tx: tx });
    }
```

(`hex` and `base64` are client deps. `self.emitter` is `Arc<dyn VoiceEventEmitter>` — clone it into the task.)

- [ ] **Step 2: Generalize track-disabled teardown.** `on_peer_track_disabled` is currently audio-only. Make it kind-aware. The bridge passes `kind` (Step 3). Change its signature to `on_peer_track_disabled(&self, session_id: SessionId, kind: TrackKind)` and branch: for Audio, the existing audio teardown (remove from `peers`, `peer_rings`, abort the recv handle, unregister `(session, Audio)` if it unregisters); for Video, remove from `video_peers`, abort its handle, and `dispatcher.unregister(&session_id, TrackKind::Video)`. Concretely add at the top:

```rust
    pub async fn on_peer_track_disabled(&self, session_id: SessionId, kind: TrackKind) {
        if matches!(kind, TrackKind::Video) {
            let mut inner = self.inner.lock().await;
            if let Some(call) = inner.active.as_mut() {
                if let Some(entry) = call.video_peers.remove(&session_id) {
                    entry.recv_handle.abort();
                    let dispatcher = call.server.dispatcher();
                    tokio::spawn(async move { dispatcher.unregister(&session_id, TrackKind::Video).await; });
                }
            }
            return;
        }
        // ... existing audio teardown unchanged ...
    }
```

(Keep the existing audio teardown body below, unchanged.)

- [ ] **Step 3: Remove the bridge audio-only filters.** In `bridge.rs`, the `TrackEnabled` arm (lines 124-147): delete the `if !matches!(kind, TrackKind::Audio) { return; }` guard so Video events reach the controller. The peer-pubkey lookup still works (peer_pubkey_for reads peer_pubkeys, set by the key offer for any kind). The `TrackDisabled` arm (148-159): delete its filter AND pass `kind`:

```rust
        ServerEvent::TrackDisabled { session_id, kind, .. } => {
            if let Some(ctrl) = app.try_state::<Arc<crate::voice::VoiceController>>() {
                let ctrl = (*ctrl).clone();
                tokio::spawn(async move {
                    ctrl.on_peer_track_disabled(session_id, kind).await;
                });
            }
            Ok(())
        }
```

(The `TrackEnabled` arm already calls `on_peer_track_enabled(session_id, pk, kind)` which now dispatches by kind — just remove its filter.)

- [ ] **Step 4: Fix any other `on_peer_track_disabled` call sites.** Grep `grep -n "on_peer_track_disabled" client/src-tauri/src` — update each caller (and any mod.rs tests) to pass a `TrackKind`. The leave-call/cleanup path that disables our own tracks may call it; pass the right kind.

- [ ] **Step 5: Build + test.**

Run: `cd client/src-tauri && cargo build && cargo test voice:: -- --test-threads=1`
Expected: clean build; all voice tests pass. The audio path is unchanged in behavior (audio TrackEnabled → `on_peer_audio_track_enabled`); video TrackEnabled now sets up a video recv task. Add a controller-level test if feasible (a `FakeServerSession` + a captured emitter): simulate `on_stream_key_offer(session, Video, peer, wrapped)` then `on_peer_track_enabled(session, peer, Video)` and assert a `video_peers` entry exists — OR, if wiring a fake is heavy, rely on the send_video/recv_video unit tests (Tasks 4-5) plus the build/compile proof and defer the full controller integration to C2's runtime test. State which in the commit.

- [ ] **Step 6: Commit.**

```bash
git add client/src-tauri/src/voice/mod.rs client/src-tauri/src/bridge.rs
git commit -m "client: set up a per-peer video recv task on TrackEnabled(Video); drop audio-only filters"
```

---

### Task 7: Docs + verification gate

**Files:**
- Modify: `docs/modules/screenshare-capture-codec.md` (add the C1 transport section) OR create `docs/modules/voice-video-transport.md`
- Modify: `docs/modules/tauri-bridge.md` (the `voice://peer-video-frame` event)
- Modify: `ARCHITECTURE.md`

- [ ] **Step 1: Write the docs.** Document: the `(session, TrackKind)` dispatcher + key map generalization; the H.264 video seal/open helpers; `send_video` (build_video_datagrams / VideoSender — keyframe byte inside the encrypted payload) and `recv_video` (reassemble → open → emit); `on_peer_video_track_enabled` + the `voice://peer-video-frame` event (`{session: hex, data: base64 H.264, key: bool, seq}`, consumed by the C2 frontend per-peer decoder). State the C1 SCOPE: transport primitives only; the share trigger (start/stop-share commands, video-key derive+offer, keyframe-on-join, late-joiner re-offer) and the frontend per-peer video tile are **Phase C2**. In `tauri-bridge.md`, add the `voice://peer-video-frame` event. In `ARCHITECTURE.md`, one line: media datagrams now route per `(session, track_kind)`; a peer's audio and video frame streams are keyed, sealed, and reassembled independently.

- [ ] **Step 2: Full verification gate.**

```bash
cd /home/deez/farder && cargo test --workspace 2>&1 | grep -E "^test result" | tail -20
cd /home/deez/farder/client/src-tauri && cargo build && cargo test -- --test-threads=1 2>&1 | grep -E "^test result"
cd /home/deez/farder/client && npx tsc --noEmit && echo TSC_OK
```

Expected: all green (the client crate single-threaded for the known `FARDER_DATA` env race; the `mock_capture_emits_frames_at_expected_fps` timing flake may need a re-run — confirm it passes alone, it's pre-existing). If any voice/media test FAILS, STOP and report — the audio-path generalization regressed.

- [ ] **Step 3: Commit.**

```bash
git add docs/ ARCHITECTURE.md
git commit -m "docs: per-(session,track) video transport (Phase C1)"
```

- [ ] **Step 4: Report — C1 is transport only.** Note clearly: C1 generalizes the controller for video and proves the send/recv primitives headlessly, but screen-sharing is NOT yet user-reachable. Phase C2 wires the start/stop-share commands (derive+offer the video key, enable the Video track, drive `VideoSender` from the Phase B capture/encode loop's sink, keyframe-on-join + late-joiner re-offer) and the frontend per-peer video tile (a WebCodecs decoder fed by `voice://peer-video-frame`, mirroring the Phase B preview). The two-client end-to-end (sharer → viewer over relay + direct) is verified at the end of C2 on Windows. The audio path is unchanged + regression-tested here.

---

## Self-review notes (done at plan time)

- **Spec coverage (Phase C, C1 portion):** "remove the audio-only checks" (Task 6 + bridge); "generalize the send/recv pipeline ... for the Video track" (Tasks 4-6: send_video, recv_video, the controller wiring); "per-video-track stream keys" (Task 2: peer_keys keyed by (session, kind) + kind-threaded offer; Task 3: video seal/open). "Server routing for Video" — already done (Phase A); "keyframe-on-join" and the "end-to-end one-sharer-one-viewer" share flow + UI are **Phase C2** (explicitly deferred). The dispatcher re-key (Task 1) is the load-bearing correctness fix the spec implied but didn't name.
- **Type consistency:** `(SessionId, TrackKind)` keys the dispatcher AND peer_keys; `seal_video_frame_to_wire`/`open_video_wire_frame` mirror the audio pair; `EncodedFrame{data,is_keyframe,timestamp_ms}` (Phase B) feeds `build_video_datagrams`/`VideoSender`; the encrypted payload is `[keyframe:1][h264]`; `recv_video` emits `VideoOut{data,is_keyframe,seq}` → `voice://peer-video-frame {session,data,key,seq}`.
- **Proven-path risk:** Tasks 1, 2, and 6 touch the audio path's core (dispatcher, key map, on_peer_track_enabled). Each keeps the audio behavior identical (audio registers/looks-up under `TrackKind::Audio`) and the full voice test suite is the regression gate after every such task. The video additions (Tasks 3-5) are isolated new modules, fully unit-tested headlessly (seal→fragment→reassemble→open roundtrips, wrong-key drop).
- **Known judgment call (Task 6):** a full controller-level integration test needs a `FakeServerSession` + captured emitter; if that harness is heavy, C1 leans on the send_video/recv_video unit tests + compile proof and defers the end-to-end controller test to C2's runtime. Flagged in the task.
