# Screensharing Phase A — Media Datagram Transport (fragment / reassemble)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a unified, cleartext media-datagram header with fragment/reassemble so large (video) frames can ride the QUIC datagram path, and migrate the existing voice path onto it without regressing voice.

**Architecture:** A new pure module `farder-protocol::media_datagram` defines a 26-byte outer header (`version | track_kind | session_id | frame_id | frag_index | frag_count`) plus `fragment()` and a `Reassembler`. The sealed media frame (the existing `farder-crypto` seal/open unit, the AEAD-bound security boundary) becomes the *payload* that gets split across datagrams. The server and the client inbound dispatcher route by reading the cleartext outer header (never decrypting); the client receiver reassembles fragments back into a sealed frame before opening it. Audio frames are a single fragment, so behavior is identical — only a 26-byte routing header is added in front.

**Tech Stack:** Rust (existing quinn datagrams, `farder-crypto` ChaCha20-Poly1305 seal/open, `farder-protocol` enums). No new dependencies. No native libs (those arrive in Phases B–E).

**Spec:** `docs/superpowers/specs/2026-06-12-screensharing-design.md` (Phase A).

**Branch:** create `screenshare-phaseA` from `main` before Task 1. Finish with ff-merge + push per project workflow.

**Scope note:** This is Phase A only — the transport. Phases B (capture+codec), C (video track wiring), D (screen audio), E (UI) get their own plans once the native-dependency APIs (`windows-capture`, `openh264`, WebCodecs) are validated. No relay changes are needed in any phase: the relay forwards datagrams blind by its 4-byte handle prefix and never reads the media header (`crates/farder-relay/src/datagram.rs`).

**Interop warning (state in the final report):** this changes the media datagram wire format. An old client/server cannot exchange voice with a new one — both sides must rebuild (the standard Farder "rebuild the server sidecar + client" media rule). Because media rides throwaway per-session datagrams, skew degrades to "no audio between mismatched peers," never a crash or loop. The owner's two-client Windows run is the real verification and **also finally verifies voice-over-relay end to end** (the long-deferred item).

---

## Verified codebase facts (read before implementing — these are exact)

- **Inner sealed frame** (`crates/farder-crypto/src/media.rs`): `seal_audio_packet_to_wire(key, seq, session_id, speaker_pk, opus_packet) -> Vec<u8>` produces `[28-byte header][AEAD ciphertext]`; the 28-byte header (`MEDIA_FRAME_VERSION=0x02 | type | track_id | codec_id | seq(8 BE) | session_id(16)`, `MEDIA_FRAME_HEADER_LEN=28`) is the AEAD AAD. `open_audio_wire_frame(key, wire) -> (seq, speaker_pk, opus)` reverses it. Constants `MEDIA_FRAME_TYPE_AUDIO=0x01`, `MEDIA_FRAME_TYPE_VIDEO=0x02`, `SESSION_ID_LEN=16`, `pub type SessionId=[u8;16]` are exported here. **This module is NOT changed by Phase A** — the sealed frame stays the inner unit.
- **Server routing** (`crates/farder-server/src/media_stream.rs`): `on_frame_ingress(state, config, sending_pk, raw, now_ms) -> IngressDecision::{Forward{recipients}, Drop(reason)}` currently calls `parse_media_frame(raw)` which reads the 28-byte inner header (`session_id` at `raw[12..28]`, `kind` at `raw[1]`). `MediaConfig::default()` has `audio_max_bps: 64_000, video_max_bps: 2_000_000`. Token bucket consumes `raw.len()`.
- **Server ingress glue** (`crates/farder-server/src/connection.rs:987` `process_inbound_voice_frame`): reads `raw[12..28]` as session_id (gated on `raw.len() >= MEDIA_FRAME_HEADER_LEN`) to find the channel, then calls `on_frame_ingress`, then fans `bytes.clone()` to each recipient's `VoiceSink`. Forwards the **whole datagram** unchanged.
- **Client send** (`client/src-tauri/src/voice/send.rs`): after `seal_audio_packet_to_wire(...)` it does `(cfg.datagram_sink)(Bytes::from(frame_bytes))`, one datagram per 20 ms frame; `seq: u64` counter.
- **Client inbound dispatch** (`client/src-tauri/src/voice/mod.rs:107` `MediaInboundDispatcher::dispatch`): reads `bytes[12..28]` as session_id (gated on `bytes.len() < 28`) to route to the per-peer `RecvTask` channel.
- **Client recv** (`client/src-tauri/src/voice/recv.rs`): `open_audio_wire_frame(&cfg.stream_key, &bytes)` directly on the datagram, then jitter/decode.
- **`TrackKind`** (`crates/farder-protocol/src/server.rs`): enum `{ Audio, Video }`, derives include `Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug`.
- **farder-protocol depends on farder-crypto** (uses `PublicKey`), so the new module can import `farder_crypto::media::{SessionId, SESSION_ID_LEN, MEDIA_FRAME_TYPE_AUDIO, MEDIA_FRAME_TYPE_VIDEO}` and `crate::server::TrackKind`.

---

### Task 1: Outer header — encode / parse

**Files:**
- Create: `crates/farder-protocol/src/media_datagram.rs`
- Modify: `crates/farder-protocol/src/lib.rs` (add `pub mod media_datagram;`)

- [ ] **Step 1: Add the module declaration.** In `crates/farder-protocol/src/lib.rs`, add alongside the existing `pub mod` lines:

```rust
pub mod media_datagram;
```

- [ ] **Step 2: Write the failing tests.** Create `crates/farder-protocol/src/media_datagram.rs` with ONLY this content for now (types + tests; the impl follows):

```rust
//! Unified media-datagram transport: a 26-byte cleartext outer header plus
//! fragment/reassemble, so large (video) frames ride the QUIC datagram path.
//!
//! The outer header is what the relay/server/receiver route on WITHOUT keys.
//! The payload it carries is the existing `farder-crypto` sealed frame (the
//! AEAD-bound security boundary) — possibly split across several datagrams.
//! Audio frames are a single fragment, so they gain only the 26-byte header.

use crate::server::TrackKind;
use farder_crypto::media::{
    MEDIA_FRAME_TYPE_AUDIO, MEDIA_FRAME_TYPE_VIDEO, SessionId, SESSION_ID_LEN,
};

/// Version byte for the outer media datagram header (distinct from the inner
/// sealed-frame version 0x02).
pub const MEDIA_DGRAM_VERSION: u8 = 0x03;

/// version(1) | track_kind(1) | session_id(16) | frame_id(4) | frag_index(2) | frag_count(2)
pub const MEDIA_DGRAM_HEADER_LEN: usize = 1 + 1 + SESSION_ID_LEN + 4 + 2 + 2; // 26

/// A conservative per-datagram payload cap when the connection's
/// `max_datagram_size` is unknown. Audio frames are far below this so they
/// never fragment; Phase C derives the real value from the connection.
pub const DEFAULT_MAX_DGRAM_PAYLOAD: usize = 1100;

#[derive(Debug, PartialEq, Eq)]
pub enum MediaDgramError {
    TooShort,
    BadVersion(u8),
    BadTrackKind(u8),
    BadFragmentation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OuterHeader {
    pub track_kind: TrackKind,
    pub session_id: SessionId,
    pub frame_id: u32,
    pub frag_index: u16,
    pub frag_count: u16,
}

fn track_kind_to_byte(k: TrackKind) -> u8 {
    match k {
        TrackKind::Audio => MEDIA_FRAME_TYPE_AUDIO,
        TrackKind::Video => MEDIA_FRAME_TYPE_VIDEO,
    }
}

fn byte_to_track_kind(b: u8) -> Option<TrackKind> {
    match b {
        MEDIA_FRAME_TYPE_AUDIO => Some(TrackKind::Audio),
        MEDIA_FRAME_TYPE_VIDEO => Some(TrackKind::Video),
        _ => None,
    }
}

impl OuterHeader {
    /// Append the 26-byte header to `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.push(MEDIA_DGRAM_VERSION);
        out.push(track_kind_to_byte(self.track_kind));
        out.extend_from_slice(&self.session_id);
        out.extend_from_slice(&self.frame_id.to_be_bytes());
        out.extend_from_slice(&self.frag_index.to_be_bytes());
        out.extend_from_slice(&self.frag_count.to_be_bytes());
    }

    /// Parse the header off the front of `buf`, returning it and the remaining
    /// payload slice. Validates version, track kind, and `frag_index < frag_count`.
    pub fn parse(buf: &[u8]) -> Result<(OuterHeader, &[u8]), MediaDgramError> {
        if buf.len() < MEDIA_DGRAM_HEADER_LEN {
            return Err(MediaDgramError::TooShort);
        }
        if buf[0] != MEDIA_DGRAM_VERSION {
            return Err(MediaDgramError::BadVersion(buf[0]));
        }
        let track_kind = byte_to_track_kind(buf[1]).ok_or(MediaDgramError::BadTrackKind(buf[1]))?;
        let mut session_id = [0u8; SESSION_ID_LEN];
        session_id.copy_from_slice(&buf[2..2 + SESSION_ID_LEN]);
        let frame_id = u32::from_be_bytes(buf[18..22].try_into().unwrap());
        let frag_index = u16::from_be_bytes(buf[22..24].try_into().unwrap());
        let frag_count = u16::from_be_bytes(buf[24..26].try_into().unwrap());
        if frag_count == 0 || frag_index >= frag_count {
            return Err(MediaDgramError::BadFragmentation);
        }
        Ok((
            OuterHeader { track_kind, session_id, frame_id, frag_index, frag_count },
            &buf[MEDIA_DGRAM_HEADER_LEN..],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid() -> SessionId {
        [9u8, 8, 7, 6, 5, 4, 3, 2, 1, 0, 1, 2, 3, 4, 5, 6]
    }

    #[test]
    fn header_roundtrip() {
        let h = OuterHeader {
            track_kind: TrackKind::Video,
            session_id: sid(),
            frame_id: 0xDEAD_BEEF,
            frag_index: 3,
            frag_count: 7,
        };
        let mut buf = Vec::new();
        h.write_to(&mut buf);
        buf.extend_from_slice(b"payload-bytes");
        assert_eq!(buf.len(), MEDIA_DGRAM_HEADER_LEN + 13);
        let (got, payload) = OuterHeader::parse(&buf).unwrap();
        assert_eq!(got, h);
        assert_eq!(payload, b"payload-bytes");
    }

    #[test]
    fn parse_rejects_short() {
        let buf = vec![0u8; MEDIA_DGRAM_HEADER_LEN - 1];
        assert_eq!(OuterHeader::parse(&buf), Err(MediaDgramError::TooShort));
    }

    #[test]
    fn parse_rejects_bad_version() {
        let mut buf = vec![0u8; MEDIA_DGRAM_HEADER_LEN];
        buf[0] = 0x02; // inner-frame version, not the outer one
        buf[1] = MEDIA_FRAME_TYPE_AUDIO;
        buf[25] = 1; // frag_count = 1 so only the version check fires
        assert_eq!(OuterHeader::parse(&buf), Err(MediaDgramError::BadVersion(0x02)));
    }

    #[test]
    fn parse_rejects_bad_track_kind() {
        let mut buf = vec![0u8; MEDIA_DGRAM_HEADER_LEN];
        buf[0] = MEDIA_DGRAM_VERSION;
        buf[1] = 0x7f;
        buf[25] = 1;
        assert_eq!(OuterHeader::parse(&buf), Err(MediaDgramError::BadTrackKind(0x7f)));
    }

    #[test]
    fn parse_rejects_bad_fragmentation() {
        // frag_index >= frag_count
        let h_bytes = {
            let mut v = Vec::new();
            OuterHeader {
                track_kind: TrackKind::Audio,
                session_id: sid(),
                frame_id: 1,
                frag_index: 2,
                frag_count: 2,
            }
            .write_to(&mut v);
            v
        };
        assert_eq!(OuterHeader::parse(&h_bytes), Err(MediaDgramError::BadFragmentation));

        // frag_count == 0
        let mut zero = h_bytes.clone();
        zero[22..24].copy_from_slice(&0u16.to_be_bytes()); // frag_index = 0
        zero[24..26].copy_from_slice(&0u16.to_be_bytes()); // frag_count = 0
        assert_eq!(OuterHeader::parse(&zero), Err(MediaDgramError::BadFragmentation));
    }
}
```

- [ ] **Step 3: Run the tests.**

Run: `cargo test -p farder-protocol media_datagram::`
Expected: 5 tests PASS.

- [ ] **Step 4: Commit.**

```bash
git add crates/farder-protocol/src/lib.rs crates/farder-protocol/src/media_datagram.rs
git commit -m "protocol: media-datagram outer header (encode/parse)"
```

---

### Task 2: `fragment()` + `Reassembler`

**Files:**
- Modify: `crates/farder-protocol/src/media_datagram.rs`

- [ ] **Step 1: Write the failing tests.** Append inside the `mod tests` block in `media_datagram.rs`:

```rust
    #[test]
    fn fragment_single_when_under_cap() {
        let dgrams = fragment(TrackKind::Audio, &sid(), 5, b"small-sealed-frame", 1100);
        assert_eq!(dgrams.len(), 1);
        let (h, payload) = OuterHeader::parse(&dgrams[0]).unwrap();
        assert_eq!(h.frag_count, 1);
        assert_eq!(h.frag_index, 0);
        assert_eq!(h.frame_id, 5);
        assert_eq!(payload, b"small-sealed-frame");
    }

    #[test]
    fn fragment_splits_when_over_cap() {
        let sealed: Vec<u8> = (0..2500u32).map(|i| i as u8).collect();
        let dgrams = fragment(TrackKind::Video, &sid(), 9, &sealed, 1000);
        assert_eq!(dgrams.len(), 3); // ceil(2500 / 1000)
        for (i, d) in dgrams.iter().enumerate() {
            let (h, _) = OuterHeader::parse(d).unwrap();
            assert_eq!(h.frag_count, 3);
            assert_eq!(h.frag_index as usize, i);
            assert_eq!(h.frame_id, 9);
            assert_eq!(h.track_kind, TrackKind::Video);
        }
    }

    fn feed(reasm: &mut Reassembler, dgram: &[u8]) -> Option<Vec<u8>> {
        let (h, payload) = OuterHeader::parse(dgram).unwrap();
        reasm.accept(&h, payload)
    }

    #[test]
    fn reassemble_single_fragment_completes_immediately() {
        let dgrams = fragment(TrackKind::Audio, &sid(), 1, b"abc", 1100);
        let mut r = Reassembler::new();
        assert_eq!(feed(&mut r, &dgrams[0]).as_deref(), Some(&b"abc"[..]));
    }

    #[test]
    fn reassemble_multi_in_order() {
        let sealed: Vec<u8> = (0..2500u32).map(|i| i as u8).collect();
        let dgrams = fragment(TrackKind::Video, &sid(), 1, &sealed, 1000);
        let mut r = Reassembler::new();
        assert!(feed(&mut r, &dgrams[0]).is_none());
        assert!(feed(&mut r, &dgrams[1]).is_none());
        assert_eq!(feed(&mut r, &dgrams[2]), Some(sealed));
    }

    #[test]
    fn reassemble_multi_out_of_order() {
        let sealed: Vec<u8> = (0..2500u32).map(|i| (i * 3) as u8).collect();
        let dgrams = fragment(TrackKind::Video, &sid(), 1, &sealed, 1000);
        let mut r = Reassembler::new();
        assert!(feed(&mut r, &dgrams[2]).is_none());
        assert!(feed(&mut r, &dgrams[0]).is_none());
        assert_eq!(feed(&mut r, &dgrams[1]), Some(sealed));
    }

    #[test]
    fn reassemble_duplicate_fragment_is_idempotent() {
        let sealed: Vec<u8> = (0..1500u32).map(|i| i as u8).collect();
        let dgrams = fragment(TrackKind::Video, &sid(), 1, &sealed, 1000);
        let mut r = Reassembler::new();
        assert!(feed(&mut r, &dgrams[0]).is_none());
        assert!(feed(&mut r, &dgrams[0]).is_none()); // duplicate, must not "complete"
        assert_eq!(feed(&mut r, &dgrams[1]), Some(sealed));
    }

    #[test]
    fn reassemble_incomplete_frame_is_evicted_not_leaked() {
        // Start many frames, each missing a fragment; the buffer must stay bounded.
        let mut r = Reassembler::with_capacity(4);
        for frame_id in 0..100u32 {
            let sealed: Vec<u8> = (0..1500u32).map(|i| i as u8).collect();
            let dgrams = fragment(TrackKind::Video, &sid(), frame_id, &sealed, 1000);
            // feed only fragment 0 — never completes
            assert!(feed(&mut r, &dgrams[0]).is_none());
        }
        assert!(r.in_progress_len() <= 4, "reassembly buffer must stay bounded");
    }
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p farder-protocol media_datagram::`
Expected: compile FAILURE (`fragment`, `Reassembler` not found).

- [ ] **Step 3: Implement.** Add to `media_datagram.rs` (above the `#[cfg(test)]` block):

```rust
use std::collections::HashMap;

/// Split a sealed frame into one or more datagrams (each = outer header +
/// payload slice). `max_payload` must be >= 1. A frame that fits in one
/// datagram becomes a single `frag_count = 1` datagram.
pub fn fragment(
    track_kind: TrackKind,
    session_id: &SessionId,
    frame_id: u32,
    sealed: &[u8],
    max_payload: usize,
) -> Vec<Vec<u8>> {
    let max_payload = max_payload.max(1);
    let frag_count = sealed.len().div_ceil(max_payload).max(1);
    let frag_count_u16 = frag_count.min(u16::MAX as usize) as u16;
    let mut out = Vec::with_capacity(frag_count);
    for (i, chunk) in sealed.chunks(max_payload).enumerate() {
        let mut buf = Vec::with_capacity(MEDIA_DGRAM_HEADER_LEN + chunk.len());
        OuterHeader {
            track_kind,
            session_id: *session_id,
            frame_id,
            frag_index: i as u16,
            frag_count: frag_count_u16,
        }
        .write_to(&mut buf);
        buf.extend_from_slice(chunk);
        out.push(buf);
    }
    // An empty sealed frame still produces one empty-payload datagram so the
    // receiver completes it (chunks() yields nothing for an empty slice).
    if out.is_empty() {
        let mut buf = Vec::with_capacity(MEDIA_DGRAM_HEADER_LEN);
        OuterHeader { track_kind, session_id: *session_id, frame_id, frag_index: 0, frag_count: 1 }
            .write_to(&mut buf);
        out.push(buf);
    }
    out
}

struct Partial {
    frag_count: u16,
    parts: Vec<Option<Vec<u8>>>,
    received: u16,
    touch: u64,
}

/// Reassembles datagrams of one peer-track into complete sealed frames.
/// Drop-late / drop-incomplete: only the most-recently-touched `max_frames`
/// in-progress frames are kept; older incomplete frames are evicted.
pub struct Reassembler {
    frames: HashMap<u32, Partial>,
    max_frames: usize,
    clock: u64,
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::with_capacity(4)
    }
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(max_frames: usize) -> Self {
        Self { frames: HashMap::new(), max_frames: max_frames.max(1), clock: 0 }
    }

    /// Number of in-progress (incomplete) frames currently buffered.
    pub fn in_progress_len(&self) -> usize {
        self.frames.len()
    }

    /// Feed one parsed datagram. Returns the completed sealed frame if this
    /// datagram finished one. `header.frag_index < header.frag_count` is
    /// guaranteed by `OuterHeader::parse`.
    pub fn accept(&mut self, header: &OuterHeader, payload: &[u8]) -> Option<Vec<u8>> {
        // Single-fragment fast path: no buffering.
        if header.frag_count == 1 {
            return Some(payload.to_vec());
        }
        self.clock += 1;
        let now = self.clock;
        let cap = header.frag_count as usize;

        let entry = self.frames.entry(header.frame_id).or_insert_with(|| Partial {
            frag_count: header.frag_count,
            parts: vec![None; cap],
            received: 0,
            touch: now,
        });

        // A frag_count mismatch for the same frame_id means corruption/reuse —
        // restart this frame's buffer.
        if entry.frag_count != header.frag_count {
            *entry = Partial { frag_count: header.frag_count, parts: vec![None; cap], received: 0, touch: now };
        }
        entry.touch = now;

        let slot = &mut entry.parts[header.frag_index as usize];
        if slot.is_none() {
            *slot = Some(payload.to_vec());
            entry.received += 1;
        }

        let done = entry.received == entry.frag_count;
        if done {
            let entry = self.frames.remove(&header.frame_id).unwrap();
            let mut sealed = Vec::new();
            for part in entry.parts {
                sealed.extend_from_slice(&part.expect("all parts present when received == frag_count"));
            }
            return Some(sealed);
        }

        // Evict the least-recently-touched frame if over capacity.
        if self.frames.len() > self.max_frames {
            if let Some((&oldest, _)) = self.frames.iter().min_by_key(|(_, p)| p.touch) {
                self.frames.remove(&oldest);
            }
        }
        None
    }
}
```

- [ ] **Step 4: Run the tests.**

Run: `cargo test -p farder-protocol media_datagram::`
Expected: all media_datagram tests PASS (Task 1's 5 + Task 2's 7 = 12).

- [ ] **Step 5: Commit.**

```bash
git add crates/farder-protocol/src/media_datagram.rs
git commit -m "protocol: media-datagram fragment() + Reassembler (drop-late, bounded)"
```

---

### Task 3: Server routes on the outer header

**Files:**
- Modify: `crates/farder-server/src/media_stream.rs` (`on_frame_ingress` + tests; bump video cap)
- Modify: `crates/farder-server/src/connection.rs` (`process_inbound_voice_frame` session lookup + its tests)

- [ ] **Step 1: Rewrite the ingress tests to the new wire format.** In `crates/farder-server/src/media_stream.rs` `mod tests`, add a helper near the top of the test module (after `fn sample_session()`):

```rust
    use farder_protocol::media_datagram::OuterHeader;

    /// Build a single-fragment outer media datagram carrying `ciphertext`
    /// (the wire format the server now routes on).
    fn outer_dgram(kind: TrackKind, session: &SessionId, ciphertext: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        OuterHeader {
            track_kind: kind,
            session_id: *session,
            frame_id: 0,
            frag_index: 0,
            frag_count: 1,
        }
        .write_to(&mut v);
        v.extend_from_slice(ciphertext);
        v
    }
```

Then replace every `build_media_frame(TrackKind::X, seq, &session, payload)` call **that is passed into `on_frame_ingress`** with `outer_dgram(TrackKind::X, &session, payload)`. The affected tests: `ingress_drops_unknown_session`, `ingress_drops_session_connection_mismatch`, `ingress_drops_track_not_enabled`, `ingress_forwards_to_other_sessions_in_channel`, `ingress_skips_deafened_recipients`, `multi_track_lifecycle_audio_then_video`, and `sealed_sender_no_pubkey_in_frame_header`. (Leave `parse_audio_roundtrip`, `parse_video_roundtrip`, `parse_rejects_*`, and `header_aad_returns_first_28_bytes` UNCHANGED — they test the inner-frame helpers, which still exist.)

For `sealed_sender_no_pubkey_in_frame_header`: build the frame with `outer_dgram(TrackKind::Audio, &alice_session, b"opaque-ciphertext-bytes")`, and change the header-scan to scan the **outer** header — replace `let header = &frame[..MEDIA_FRAME_HEADER_LEN];` with:

```rust
        use farder_protocol::media_datagram::MEDIA_DGRAM_HEADER_LEN;
        let header = &frame[..MEDIA_DGRAM_HEADER_LEN];
```

(The pubkey-absence assertions below it work unchanged against the 26-byte outer header.)

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p farder-server media_stream::tests::ingress_forwards_to_other_sessions_in_channel`
Expected: FAIL — `on_frame_ingress` still parses the 28-byte inner header, so an outer datagram is misread (UnknownSession / parse error).

- [ ] **Step 3: Switch `on_frame_ingress` to the outer header.** In `media_stream.rs`, replace the body of `on_frame_ingress` from the `let frame = match parse_media_frame(raw)` line through the `if !session.active_tracks.contains(&frame.kind)` block with outer-header parsing. The full replacement of the parse + lookup + checks region:

```rust
    use farder_protocol::media_datagram::OuterHeader;
    let (header, _payload) = match OuterHeader::parse(raw) {
        Ok(h) => h,
        Err(_e) => return IngressDecision::Drop(DropReason::ParseError(MediaFrameError::TooShort)),
    };

    let session = match state.sessions.get_mut(&header.session_id) {
        Some(s) => s,
        None => return IngressDecision::Drop(DropReason::UnknownSession),
    };

    if session.connection_pk != *sending_connection_pk {
        return IngressDecision::Drop(DropReason::SessionConnectionMismatch);
    }
    if !session.active_tracks.contains(&header.track_kind) {
        return IngressDecision::Drop(DropReason::TrackNotEnabled);
    }

    let cap = match header.track_kind {
        TrackKind::Audio => config.audio_max_bps,
        TrackKind::Video => config.video_max_bps,
    };
    let bucket = session.buckets.entry(header.track_kind).or_insert_with(|| TokenBucket::new(cap));

    if !bucket.try_consume(raw.len() as u64) {
        return IngressDecision::Drop(DropReason::BandwidthCap);
    }

    match header.track_kind {
        TrackKind::Audio => session.last_audio_frame_ms = Some(now_ms),
        TrackKind::Video => session.last_video_frame_ms = Some(now_ms),
    }

    let channel_id = session.channel_id;
    let sender_session = header.session_id;
```

(Everything from `let recipients: Vec<SessionId> = ...` to the end of the function is UNCHANGED.) The `ParseError(MediaFrameError)` drop reason is kept (mapping any outer-parse failure to `TooShort` so the existing `DropReason` enum is unchanged and the ops counter still fires).

- [ ] **Step 4: Bump the video bandwidth cap.** In `media_stream.rs` `MediaConfig::default`, change `video_max_bps: 2_000_000` to `video_max_bps: 8_000_000` (headroom for 720p30 H.264 keyframe bursts at the ~3 Mbps target — spec §"server routing").

- [ ] **Step 5: Fix the server ingress glue session lookup.** In `crates/farder-server/src/connection.rs` `process_inbound_voice_frame` (~line 998), replace the session-id extraction block:

```rust
    let raw: &[u8] = &bytes;
    let channel_id_opt: Option<u64> = match farder_protocol::media_datagram::OuterHeader::parse(raw) {
        Ok((header, _payload)) => {
            let channels = state.media.channels.read().unwrap();
            channels
                .iter()
                .find(|(_ch, st)| st.sessions.contains_key(&header.session_id))
                .map(|(ch, _)| *ch)
        }
        Err(_) => None,
    };
```

(The rest of `process_inbound_voice_frame` — the `on_frame_ingress` call and the fan-out of `bytes.clone()` — is UNCHANGED.)

- [ ] **Step 6: Update the connection voice_relay_tests to the new format.** In `connection.rs` `mod voice_relay_tests`, any test that builds a media datagram via `build_media_frame(...)` or a hand-rolled 28-byte frame and feeds it to `process_inbound_voice_frame` must build an outer datagram instead. Add this helper at the top of `mod voice_relay_tests`:

```rust
    fn outer_audio_dgram(session: &[u8; 16], ciphertext: &[u8]) -> bytes::Bytes {
        use farder_protocol::media_datagram::OuterHeader;
        use farder_protocol::server::TrackKind;
        let mut v = Vec::new();
        OuterHeader {
            track_kind: TrackKind::Audio,
            session_id: *session,
            frame_id: 0,
            frag_index: 0,
            frag_count: 1,
        }
        .write_to(&mut v);
        v.extend_from_slice(ciphertext);
        bytes::Bytes::from(v)
    }
```

Then replace the frame construction in the test at ~line 1125 (and any sibling) so the datagram passed to `process_inbound_voice_frame` is built via `outer_audio_dgram(&session_id, &ciphertext_bytes)`. (Read the test; the session id it installs must match the one in the datagram.)

- [ ] **Step 7: Run the server tests.**

Run: `cargo test -p farder-server`
Expected: ALL green (media_stream tests on the new format; connection voice tests on the new format; everything else unchanged).

- [ ] **Step 8: Commit.**

```bash
git add crates/farder-server/src/media_stream.rs crates/farder-server/src/connection.rs
git commit -m "server: route media datagrams on the unified outer header; raise video cap to 8Mbps"
```

---

### Task 4: Client send fragments after sealing

**Files:**
- Modify: `client/src-tauri/src/voice/send.rs`

- [ ] **Step 1: Add a test asserting the emitted datagram carries the outer header.** Append to `mod tests` in `send.rs`:

```rust
    #[test]
    fn emitted_datagrams_have_the_outer_header_and_route_to_session() {
        use farder_protocol::media_datagram::OuterHeader;
        use farder_protocol::server::TrackKind;
        let (cfg, tx, sink) = build_cfg();
        let session = cfg.session_id;
        let muted = Arc::new(AtomicBool::new(false));
        let (speak_tx, _speak_rx) = watch::channel(false);

        tx.send(make_sine_chunk(440.0, OPUS_FRAME_SAMPLES_MONO)).unwrap();
        drop(tx);
        run(cfg, muted, speak_tx);

        let emitted = sink.lock().unwrap();
        assert_eq!(emitted.len(), 1, "one chunk -> one datagram (audio is single-fragment)");
        let (header, _payload) = OuterHeader::parse(&emitted[0]).expect("valid outer header");
        assert_eq!(header.track_kind, TrackKind::Audio);
        assert_eq!(header.session_id, session);
        assert_eq!(header.frag_count, 1);
    }
```

- [ ] **Step 2: Run to verify failure.**

Run: `cd client/src-tauri && cargo test voice::send::tests::emitted_datagrams_have_the_outer_header_and_route_to_session`
Expected: FAIL — current send emits the bare sealed frame (`OuterHeader::parse` rejects it: bad version, since the sealed frame starts with 0x02).

- [ ] **Step 3: Wrap the seal output in `fragment()`.** In `send.rs`:

Add the import near the top (with the other `use` lines):

```rust
use farder_protocol::media_datagram::{fragment, DEFAULT_MAX_DGRAM_PAYLOAD};
use farder_protocol::server::TrackKind;
```

Add a `frame_id` counter next to `seq` (after `let mut seq: u64 = 0;`):

```rust
    let mut frame_id: u32 = 0;
```

Replace the final emit (`(cfg.datagram_sink)(Bytes::from(frame_bytes));` then `seq = seq.saturating_add(1);`) with:

```rust
        for dgram in fragment(
            TrackKind::Audio,
            &cfg.session_id,
            frame_id,
            &frame_bytes,
            DEFAULT_MAX_DGRAM_PAYLOAD,
        ) {
            (cfg.datagram_sink)(Bytes::from(dgram));
        }
        seq = seq.saturating_add(1);
        frame_id = frame_id.wrapping_add(1);
```

(`DEFAULT_MAX_DGRAM_PAYLOAD` = 1100; audio sealed frames are ~60–150 bytes, so this is always one datagram. Phase C replaces the constant with the connection's `max_datagram_size` and adds the Video track.)

- [ ] **Step 4: Run the send tests.**

Run: `cd client/src-tauri && cargo test voice::send::`
Expected: all send tests PASS (the count tests `open_mic_emits_one_datagram_per_chunk` etc. still hold — one fragment per frame).

- [ ] **Step 5: Commit.**

```bash
git add client/src-tauri/src/voice/send.rs
git commit -m "client: fragment sealed media frames before sending (audio = 1 fragment)"
```

---

### Task 5: Client dispatch + recv on the outer header

**Files:**
- Modify: `client/src-tauri/src/voice/mod.rs` (`MediaInboundDispatcher::dispatch` + its tests)
- Modify: `client/src-tauri/src/voice/recv.rs` (reassemble before opening + its tests)

- [ ] **Step 1: Update the dispatcher to read the outer header, and its tests.** In `client/src-tauri/src/voice/mod.rs`, replace `MediaInboundDispatcher::dispatch`:

```rust
    pub async fn dispatch(&self, bytes: Bytes) {
        use farder_protocol::media_datagram::OuterHeader;
        let sid = match OuterHeader::parse(&bytes) {
            Ok((header, _payload)) => header.session_id,
            Err(_) => return, // not a valid media datagram
        };
        let routes = self.routes.lock().await;
        if let Some(tx) = routes.get(&sid) {
            let _ = tx.send(bytes);
        }
    }
```

In `mod dispatcher_tests`, add a helper and update the frame-building tests to the outer format:

```rust
    fn outer_audio_dgram(session: &SessionId) -> Bytes {
        use farder_protocol::media_datagram::OuterHeader;
        use farder_protocol::server::TrackKind;
        let mut v = Vec::new();
        OuterHeader {
            track_kind: TrackKind::Audio,
            session_id: *session,
            frame_id: 0,
            frag_index: 0,
            frag_count: 1,
        }
        .write_to(&mut v);
        v.extend_from_slice(b"opaque-sealed-frame-bytes");
        Bytes::from(v)
    }
```

- In `dispatch_routes_to_registered_session`: replace the hand-built `frame` with `let frame = outer_audio_dgram(&sid);` and `dispatcher.dispatch(frame.clone()).await;` then assert `received.len() == frame.len()`.
- In `dispatch_drops_unknown_session`: `dispatcher.dispatch(outer_audio_dgram(&[9u8; 16])).await;`.
- In `dispatch_drops_too_short_frames`: unchanged (a 20-byte buffer is still too short for the 26-byte outer header → dropped).
- In `unregister_removes_route`: `dispatcher.dispatch(outer_audio_dgram(&sid)).await;`.

- [ ] **Step 2: Update recv to reassemble, and its tests.** In `client/src-tauri/src/voice/recv.rs`, add imports:

```rust
use farder_protocol::media_datagram::{OuterHeader, Reassembler};
```

In `run`, add a reassembler before the loop (after `let mut jitter = JitterBuffer::new();`):

```rust
    let mut reassembler = Reassembler::new();
```

Replace the open step inside the loop. The current:

```rust
        let (seq, _speaker_pk, opus_pkt) = match open_audio_wire_frame(&cfg.stream_key, &bytes) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[voice::recv] open: {e}");
                continue;
            }
        };
```

becomes:

```rust
        let (header, payload) = match OuterHeader::parse(&bytes) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let sealed = match reassembler.accept(&header, payload) {
            Some(s) => s,
            None => continue, // fragment buffered; frame not complete yet
        };
        let (seq, _speaker_pk, opus_pkt) = match open_audio_wire_frame(&cfg.stream_key, &sealed) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[voice::recv] open: {e}");
                continue;
            }
        };
```

In `mod tests`, the `make_wire_frame` helper currently returns the bare sealed frame; wrap it in a single-fragment outer datagram so it matches what the network now delivers. Replace `make_wire_frame`'s final two lines:

```rust
        let wire = seal_audio_packet_to_wire(stream_key, seq, session_id, &speaker_pk, &pkt).unwrap();
        Bytes::from(wire)
```

with:

```rust
        let wire = seal_audio_packet_to_wire(stream_key, seq, session_id, &speaker_pk, &pkt).unwrap();
        use farder_protocol::media_datagram::OuterHeader;
        use farder_protocol::server::TrackKind;
        let mut dgram = Vec::new();
        OuterHeader {
            track_kind: TrackKind::Audio,
            session_id: *session_id,
            frame_id: seq as u32,
            frag_index: 0,
            frag_count: 1,
        }
        .write_to(&mut dgram);
        dgram.extend_from_slice(&wire);
        Bytes::from(dgram)
```

(The `corrupted_frame_is_ignored` test sends `vec![0u8; 30]`; `OuterHeader::parse` rejects it on the version byte → `continue`, so the ring stays empty. It still passes — leave it.)

- [ ] **Step 3: Run the client voice tests.**

Run: `cd client/src-tauri && cargo test voice::`
Expected: all voice tests PASS (dispatcher routes on the outer header; recv reassembles single-fragment frames then decodes).

- [ ] **Step 4: Commit.**

```bash
git add client/src-tauri/src/voice/mod.rs client/src-tauri/src/voice/recv.rs
git commit -m "client: dispatch + reassemble media datagrams on the unified outer header"
```

---

### Task 6: End-to-end format round-trip + full regression gate

**Files:**
- Create: `crates/farder-protocol/tests/media_datagram_e2e.rs`

- [ ] **Step 1: Write the capstone integration test** proving the crypto seal/open composes with fragment/reassemble for both a real single-fragment audio frame and a large multi-fragment frame. Create `crates/farder-protocol/tests/media_datagram_e2e.rs`:

```rust
//! Capstone: the inner sealed frame (farder-crypto) survives a full
//! fragment -> (server forward, simulated) -> reassemble round-trip.

use farder_crypto::media::{open_audio_wire_frame, seal_audio_packet_to_wire};
use farder_protocol::media_datagram::{fragment, OuterHeader, Reassembler, DEFAULT_MAX_DGRAM_PAYLOAD};
use farder_protocol::server::TrackKind;

#[test]
fn audio_frame_survives_fragment_reassemble_and_opens() {
    let key = [0x33u8; 32];
    let session = [0x44u8; 16];
    let speaker = [0x55u8; 32];
    let opus = vec![1u8, 2, 3, 4, 5, 6, 7, 8]; // stand-in for an Opus packet
    let sealed = seal_audio_packet_to_wire(&key, 7, &session, &speaker, &opus).unwrap();

    // Sender fragments (audio -> single datagram).
    let dgrams = fragment(TrackKind::Audio, &session, 7, &sealed, DEFAULT_MAX_DGRAM_PAYLOAD);
    assert_eq!(dgrams.len(), 1);

    // Receiver parses the outer header (as the dispatcher does), reassembles,
    // then opens the inner sealed frame.
    let mut reasm = Reassembler::new();
    let (header, payload) = OuterHeader::parse(&dgrams[0]).unwrap();
    assert_eq!(header.session_id, session);
    let reassembled = reasm.accept(&header, payload).expect("single fragment completes");
    let (seq, got_speaker, got_opus) = open_audio_wire_frame(&key, &reassembled).unwrap();
    assert_eq!(seq, 7);
    assert_eq!(got_speaker, speaker);
    assert_eq!(got_opus, opus);
}

#[test]
fn large_frame_survives_multi_fragment_reassemble() {
    // Stand-in for a big (video) sealed frame: must fragment and rejoin exactly.
    let session = [0x66u8; 16];
    let sealed: Vec<u8> = (0..5000u32).map(|i| (i * 7 + 1) as u8).collect();
    let dgrams = fragment(TrackKind::Video, &session, 42, &sealed, 1000);
    assert_eq!(dgrams.len(), 5);

    // Deliver out of order to prove ordering independence.
    let mut reasm = Reassembler::new();
    let order = [3usize, 0, 4, 1, 2];
    let mut completed = None;
    for &i in &order {
        let (header, payload) = OuterHeader::parse(&dgrams[i]).unwrap();
        if let Some(frame) = reasm.accept(&header, payload) {
            completed = Some(frame);
        }
    }
    assert_eq!(completed.as_deref(), Some(sealed.as_slice()));
}
```

- [ ] **Step 2: Run the capstone.**

Run: `cargo test -p farder-protocol --test media_datagram_e2e`
Expected: 2 tests PASS.

- [ ] **Step 3: Full regression gate.** The wire-format change must not break any existing media/voice tests anywhere.

```bash
cd /home/deez/farder && cargo test --workspace 2>&1 | grep -E "^test result"
cd /home/deez/farder/client/src-tauri && cargo test -- --test-threads=1 2>&1 | grep -E "^test result"
```

Expected: ALL green. (The client crate runs single-threaded due to the pre-existing `FARDER_DATA` env race — unrelated to this change.) If ANY media/voice test fails, STOP — the format migration is incomplete; do not proceed.

- [ ] **Step 4: Commit.**

```bash
git add crates/farder-protocol/tests/media_datagram_e2e.rs
git commit -m "protocol: capstone e2e test — sealed frame survives fragment/reassemble"
```

---

### Task 7: Docs

**Files:**
- Create: `docs/modules/media-datagram.md` (use `docs/modules/_TEMPLATE.md`)
- Modify: `docs/modules/media-stream.md` if it exists (the server now routes on the outer header) — else note its absence in the report
- Modify: `ARCHITECTURE.md` (one line: media datagrams carry a unified outer header with fragment/reassemble; video rides this in later phases)

- [ ] **Step 1: Write `docs/modules/media-datagram.md`** following `_TEMPLATE.md`: the 26-byte outer header layout and field meanings; the relationship to the inner sealed frame (outer = cleartext routing/fragmentation, inner = AEAD security boundary, unchanged); `fragment()` and `Reassembler` (drop-late/drop-incomplete, bounded buffer); who reads the outer header (server `on_frame_ingress`, client `MediaInboundDispatcher`, client `recv`); and the **security note**: the outer header is unauthenticated cleartext, but tampering it can only misroute/drop (the inner AAD binds session/seq/type to the ciphertext, so a frame opened under the wrong key/session fails — no content injection, same threat model as today).

- [ ] **Step 2: Update `ARCHITECTURE.md`** — in the media/voice section add one sentence: media now travels as fragmentable datagrams behind a unified 26-byte outer header (`farder-protocol::media_datagram`); audio is a single fragment, video (later phases) spans several; the relay/server route on the cleartext header and never decrypt.

- [ ] **Step 3: Commit.**

```bash
git add docs/ ARCHITECTURE.md
git commit -m "docs: media-datagram transport module"
```

- [ ] **Step 4: Owner verification note (report, not code).** Phase A is UNVERIFIED at runtime until the owner's Windows two-client run: rebuild the server sidecar + client on BOTH machines (the wire format changed — old↔new cannot exchange voice), then confirm mic voice still works **direct and over the relay** — this is simultaneously the Phase A regression check and the long-deferred voice-over-relay verification. No screen video exists yet (Phases B–E); this phase only proves the transport carries the existing voice unchanged and is ready for large frames.

---

## Self-review notes (done at plan time)

- **Spec coverage (Phase A scope):** unified outer header (Task 1), fragment/reassemble with drop-late/drop-incomplete + bounded buffer (Task 2), server routes on the header + video cap bump to 8 Mbps (Task 3), client send fragments (Task 4), client dispatch+recv reassemble (Task 5), the interop/version-skew degradation + voice regression gate + capstone (Tasks 3/5/6), security-of-cleartext-header note (Task 7). Phases B–E (capture, codec, video wiring, screen audio, UI) are explicitly out of this plan.
- **Type consistency:** `OuterHeader{track_kind,session_id,frame_id,frag_index,frag_count}`, `fragment(track_kind, session_id, frame_id, sealed, max_payload) -> Vec<Vec<u8>>`, `Reassembler::{new,with_capacity,accept,in_progress_len}` used identically in every task. `SessionId` and the track-kind bytes are imported from `farder_crypto::media`; `TrackKind` from `farder_protocol::server`. `MEDIA_DGRAM_HEADER_LEN=26`, `DEFAULT_MAX_DGRAM_PAYLOAD=1100`.
- **Known judgment calls:** `frame_id` on the send side is a local `u32` counter (audio never fragments, so its exact value is immaterial; the Reassembler groups by it only for multi-fragment frames). The `DropReason` enum is left unchanged — any outer-parse failure maps to `ParseError(MediaFrameError::TooShort)` to avoid widening the enum in Phase A. The inner sealed-frame helpers (`build_media_frame`/`parse_media_frame` in media_stream.rs) are retained unused-by-routing because they still correctly describe the inner frame and are exercised by their own tests; Phase C revisits them when it builds real video sealed frames.
