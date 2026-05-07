# Voice Calling v1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** End-to-end real-time voice calls (DMs and voice channels) over QUIC datagrams with server-side fanout. Anonymity-first: peer IPs never exposed. Tauri-Rust audio stack with cpal + Opus.

**Architecture:** Audio captured/encoded/played in Tauri Rust (never crosses JS boundary). Opus 32 kbps mono 20 ms frames sent as QUIC datagrams over the existing connection. Server validates speaker pubkey, fans out to other listeners in the channel (skipping deafened members). Speaking state derived server-side at 5 Hz and broadcast to all viewers. Control plane (start/stop, mute, deafen, ringing) on the existing reliable request/event channel.

**Tech Stack:** Rust (Tauri client + Quinn server), `cpal` 0.15 (audio I/O, already present), new dep `audiopus` 0.3 (libopus binding), TypeScript + React (UI). No protocol crate beyond serde additions.

**Spec:** `docs/superpowers/specs/2026-05-07-voice-calling-v1-design.md`

---

## File structure

**New (server):**
- `crates/farder-server/src/voice.rs` — VoiceState struct, datagram parser/validator, fanout helper, speaking-state ticker

**Modified (server):**
- `crates/farder-server/src/state.rs` — add `voice: VoiceState` field
- `crates/farder-server/src/connection.rs` — datagram receive loop spawned per connection; enable datagrams in QUIC TransportConfig
- `crates/farder-server/src/handlers.rs` — StartVoice/StopVoice/SetVoiceMute/SetVoiceDeafen arms, DM ringing logic
- `crates/farder-server/src/main.rs` — spawn speaking-state ticker on startup; enable datagrams in endpoint config
- `crates/farder-server/src/lib.rs` — `pub mod voice;`

**Modified (protocol):**
- `crates/farder-protocol/src/server.rs` — 4 new requests, 3 new events

**New (client Rust):**
- `client/src-tauri/src/voice.rs` — audio engine (capture/encode/recv/playback threads, VAD/PTT gate, mixer, device pickers)

**Modified (client Rust):**
- `client/src-tauri/src/commands.rs` — 13 Tauri commands wrapping voice::*
- `client/src-tauri/src/connection.rs` — enable datagrams in client QUIC config; expose connection handle for datagram send
- `client/src-tauri/src/main.rs` — register voice commands
- `client/src-tauri/src/bridge.rs` — emit 3 new events
- `client/src-tauri/Cargo.toml` — add `audiopus` 0.3

**New (client TS):**
- `client/src/components/VoiceControlBar.tsx`
- `client/src/components/IncomingCallModal.tsx`
- `client/src/components/VoiceSettings.tsx`
- `client/public/sounds/ringtone.wav` (bundled, ~10–50 KB)

**Modified (client TS):**
- `client/src/lib/tauri-bridge.ts` — 13 new function exports + event types
- `client/src/components/ChannelSidebar.tsx` — speaking ring + mic/headphone icons on voice members; render VoiceControlBar
- `client/src/components/MemberContextMenu.tsx` — Volume submenu when target shares voice channel
- `client/src/components/AppearanceSettings.tsx` — add Voice tab
- `client/src/components/AppShell.tsx` — render IncomingCallModal
- `client/src/hooks/useServerEvents.ts` — 3 new event listeners
- `client/src/context/ServerContext.tsx` — voiceSpeakingPks Set + voiceCallIncoming reducer field

**Modified (docs):**
- `CHANGELOG.md`

---

## Phase 1: Server foundation

## Task 1: Protocol additions

The wire-format foundation. Everything else depends on this compiling.

**Files:**
- Modify: `crates/farder-protocol/src/server.rs`

- [ ] **Step 1: Add 4 new ServerRequest variants**

In `enum ServerRequest`, alongside the existing `JoinVoice` / `LeaveVoice` variants, add:

```rust
StartVoice { channel_id: u64 },
StopVoice,
SetVoiceMute { muted: bool },
SetVoiceDeafen { deafened: bool },
```

- [ ] **Step 2: Add 3 new ServerEvent variants**

In `enum ServerEvent`, alongside the existing `VoiceJoined` / `VoiceLeft`, add:

```rust
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

- [ ] **Step 3: Update the existing roundtrip serde test list (if present)**

Find the test that exhaustively serializes ServerRequest variants (around line 440-490 in `server.rs`). Add to the list:

```rust
ServerRequest::StartVoice { channel_id: 1 },
ServerRequest::StopVoice,
ServerRequest::SetVoiceMute { muted: true },
ServerRequest::SetVoiceDeafen { deafened: false },
```

If a similar list exists for ServerEvent, append the 3 new variants there too with reasonable example values.

- [ ] **Step 4: Verify workspace compiles**

```
cd /home/deez/farder && cargo check --workspace 2>&1 | tail -10
```

Expected: `Finished`. Non-exhaustive match warnings in `bridge.rs` and `connection.rs` are expected — they get filled in by Tasks 5 and 14.

If non-exhaustive matches are HARD ERRORS (Rust treats them as such), add stub no-op arms with a `// TODO(P1.5)` comment:

In `crates/farder-server/src/handlers.rs`, find any catch-all match-arms in `handle_request`. If the handlers exhaustively match, add stub arms returning "not yet implemented":

```rust
ServerRequest::StartVoice { .. }
| ServerRequest::StopVoice
| ServerRequest::SetVoiceMute { .. }
| ServerRequest::SetVoiceDeafen { .. } => {
    err("voice not yet implemented")  // TODO(P1.6)
}
```

In `crates/farder-server/src/connection.rs::broadcast_event`'s match (or wherever ServerEvent is exhaustively matched), the new variants will be picked up by `EventTarget::All` etc. since they don't add new EventTarget variants — but if a separate match on the event itself exists (e.g. for emit logging), add no-op arms.

In `client/src-tauri/src/bridge.rs`, the match block over ServerEvent is exhaustive. Add no-op arms (Task 14 fills them with real emit calls):

```rust
ServerEvent::VoiceCallIncoming { .. }
| ServerEvent::VoiceCallEnded { .. }
| ServerEvent::VoiceSpeakingChanged { .. } => Ok(()),  // TODO(P4.14)
```

- [ ] **Step 5: Run protocol tests**

```
cd /home/deez/farder && cargo test -p farder-protocol 2>&1 | tail -10
```

Expected: pass.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add crates/farder-protocol/src/server.rs crates/farder-server/src/handlers.rs crates/farder-server/src/connection.rs client/src-tauri/src/bridge.rs
git -C /home/deez/farder commit -m "feat(protocol): voice v1 requests + events"
```

---

## Task 2: Enable QUIC datagrams in server + client

Quinn supports datagrams but the feature is off by default. Both server endpoint and client connection must enable it.

**Files:**
- Modify: `crates/farder-server/src/main.rs`
- Modify: `client/src-tauri/src/connection.rs`

- [ ] **Step 1: Server: enable datagrams in TransportConfig**

In `crates/farder-server/src/main.rs::make_server_endpoint`, after constructing the `quinn::ServerConfig`, configure transport. Find:

```rust
let server_config = quinn::ServerConfig::with_crypto(Arc::new(
    quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
));
Ok(Endpoint::server(server_config, bind_addr)?)
```

Replace with:

```rust
let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(
    quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
));
// Enable QUIC datagrams (used by voice calls in v1).
let mut transport = quinn::TransportConfig::default();
transport.datagram_receive_buffer_size(Some(1 << 20));  // 1 MiB
transport.datagram_send_buffer_size(1 << 20);
server_config.transport_config(Arc::new(transport));
Ok(Endpoint::server(server_config, bind_addr)?)
```

- [ ] **Step 2: Client: enable datagrams**

Open `client/src-tauri/src/connection.rs`. Find where the `quinn::ClientConfig` (or equivalent) is built. Apply the same transport config:

```rust
let mut transport = quinn::TransportConfig::default();
transport.datagram_receive_buffer_size(Some(1 << 20));
transport.datagram_send_buffer_size(1 << 20);
let mut client_config = quinn::ClientConfig::new(Arc::new(crypto));  // existing line
client_config.transport_config(Arc::new(transport));
```

The exact line for `ClientConfig::new` may differ — adapt to the existing pattern. Search for `ClientConfig` in the file.

- [ ] **Step 3: Verify compile**

```
cd /home/deez/farder && cargo check --workspace 2>&1 | tail -5
```

Expected: `Finished`.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/main.rs client/src-tauri/src/connection.rs
git -C /home/deez/farder commit -m "feat: enable QUIC datagrams in server + client transport"
```

---

## Task 3: VoiceState struct in server state

**Files:**
- Create: `crates/farder-server/src/voice.rs` (initial skeleton)
- Modify: `crates/farder-server/src/state.rs`
- Modify: `crates/farder-server/src/lib.rs`

- [ ] **Step 1: Create the module file with the state struct**

`crates/farder-server/src/voice.rs`:

```rust
use farder_crypto::identity::PublicKey;
use std::collections::{HashMap, HashSet};
use tokio::sync::RwLock;

/// Ephemeral voice state. Not persisted to DB.
pub struct VoiceState {
    /// Per-channel: members currently transmitting audio (after StartVoice).
    pub channels: RwLock<HashMap<u64, HashSet<[u8; 32]>>>,
    /// Members who self-deafened — server skips forwarding audio to them.
    pub deafened: RwLock<HashSet<[u8; 32]>>,
    /// Members who self-muted — server expects no frames from them; if frames
    /// arrive, they're forwarded normally (mute is enforced client-side, but
    /// tracked here so other clients can render the mic-muted icon).
    pub muted: RwLock<HashSet<[u8; 32]>>,
    /// Per-speaker timestamp of last frame received (UNIX ms). Used by the
    /// 5 Hz speaking-state ticker.
    pub speaking_last_frame_ms: RwLock<HashMap<[u8; 32], u64>>,
    /// Currently-broadcast speaking state (deduplicated). Used to avoid
    /// re-broadcasting VoiceSpeakingChanged when state hasn't flipped.
    pub speaking_now: RwLock<HashSet<[u8; 32]>>,
    /// Reverse lookup: pubkey → channel they're transmitting in. Used by the
    /// datagram handler to find the right fanout list quickly.
    pub speaker_channel: RwLock<HashMap<[u8; 32], u64>>,
}

impl VoiceState {
    pub fn new() -> Self {
        Self {
            channels: RwLock::new(HashMap::new()),
            deafened: RwLock::new(HashSet::new()),
            muted: RwLock::new(HashSet::new()),
            speaking_last_frame_ms: RwLock::new(HashMap::new()),
            speaking_now: RwLock::new(HashSet::new()),
            speaker_channel: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for VoiceState {
    fn default() -> Self { Self::new() }
}

/// Add `pk` to `channel_id`'s active-speaker set. Updates speaker_channel reverse map.
/// If `pk` was previously transmitting in another channel, removes them from there first.
pub async fn start_transmit(state: &VoiceState, pk: [u8; 32], channel_id: u64) {
    let mut speaker_channel = state.speaker_channel.write().await;
    let mut channels = state.channels.write().await;
    if let Some(prev) = speaker_channel.get(&pk) {
        if let Some(prev_set) = channels.get_mut(prev) {
            prev_set.remove(&pk);
        }
    }
    channels.entry(channel_id).or_insert_with(HashSet::new).insert(pk);
    speaker_channel.insert(pk, channel_id);
}

/// Remove `pk` from any active-speaker set.
pub async fn stop_transmit(state: &VoiceState, pk: [u8; 32]) {
    let mut speaker_channel = state.speaker_channel.write().await;
    if let Some(channel_id) = speaker_channel.remove(&pk) {
        let mut channels = state.channels.write().await;
        if let Some(set) = channels.get_mut(&channel_id) {
            set.remove(&pk);
            if set.is_empty() {
                channels.remove(&channel_id);
            }
        }
    }
    state.muted.write().await.remove(&pk);
    state.speaking_last_frame_ms.write().await.remove(&pk);
    state.speaking_now.write().await.remove(&pk);
}
```

- [ ] **Step 2: Register the module**

In `crates/farder-server/src/lib.rs`, add `pub mod voice;` alongside other module declarations.

- [ ] **Step 3: Add field to ServerState**

In `crates/farder-server/src/state.rs`, find `pub struct ServerState { ... }`. Add a field:

```rust
pub voice: voice::VoiceState,
```

In the `impl ServerState::new(...)` constructor, initialize it:

```rust
voice: voice::VoiceState::new(),
```

Add `use crate::voice;` at the top of `state.rs` if not already imported.

- [ ] **Step 4: Verify compile**

```
cd /home/deez/farder && cargo check -p farder-server 2>&1 | tail -5
```

Expected: `Finished`.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/voice.rs crates/farder-server/src/state.rs crates/farder-server/src/lib.rs
git -C /home/deez/farder commit -m "feat(server): VoiceState struct + start_transmit/stop_transmit helpers"
```

---

## Task 4: Datagram parser + validator

The wire-format codec for voice frames. Pure functions, easy to test.

**Files:**
- Modify: `crates/farder-server/src/voice.rs`

- [ ] **Step 1: Write failing tests**

In `crates/farder-server/src/voice.rs`, add at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_frame() {
        let mut buf = vec![0x01, 0x01]; // version, type
        buf.extend_from_slice(&42u64.to_be_bytes()); // seq
        buf.extend_from_slice(&[0xab; 32]); // pk
        buf.extend_from_slice(&[0xcc; 80]); // opus payload
        let parsed = parse_voice_frame(&buf).unwrap();
        assert_eq!(parsed.seq, 42);
        assert_eq!(parsed.speaker_pk, [0xab; 32]);
        assert_eq!(parsed.opus_payload.len(), 80);
    }

    #[test]
    fn parse_rejects_short() {
        let buf = vec![0x01, 0x01, 0, 0, 0, 0]; // 6 bytes — too short for header
        assert!(parse_voice_frame(&buf).is_err());
    }

    #[test]
    fn parse_rejects_wrong_version() {
        let mut buf = vec![0x02, 0x01]; // wrong version
        buf.extend_from_slice(&[0u8; 40]);
        assert!(parse_voice_frame(&buf).is_err());
    }

    #[test]
    fn parse_rejects_wrong_type() {
        let mut buf = vec![0x01, 0x02]; // wrong type
        buf.extend_from_slice(&[0u8; 40]);
        assert!(parse_voice_frame(&buf).is_err());
    }

    #[test]
    fn build_round_trips() {
        let pk = [0xab; 32];
        let opus = vec![0xcc; 100];
        let buf = build_voice_frame(123, &pk, &opus);
        let parsed = parse_voice_frame(&buf).unwrap();
        assert_eq!(parsed.seq, 123);
        assert_eq!(parsed.speaker_pk, pk);
        assert_eq!(parsed.opus_payload, opus.as_slice());
    }
}
```

Run failing:
```
cd /home/deez/farder && cargo test -p farder-server voice::tests 2>&1 | tail -15
```
Expected: all 5 fail with "function not found".

- [ ] **Step 2: Implement the parser + builder**

Add to `voice.rs` above the `tests` module:

```rust
pub const VOICE_FRAME_VERSION: u8 = 0x01;
pub const VOICE_FRAME_TYPE_AUDIO: u8 = 0x01;
pub const VOICE_FRAME_HEADER_LEN: usize = 1 + 1 + 8 + 32;

#[derive(Debug, PartialEq)]
pub struct VoiceFrame<'a> {
    pub seq: u64,
    pub speaker_pk: [u8; 32],
    pub opus_payload: &'a [u8],
}

pub fn parse_voice_frame(buf: &[u8]) -> Result<VoiceFrame<'_>, &'static str> {
    if buf.len() < VOICE_FRAME_HEADER_LEN {
        return Err("frame too short");
    }
    if buf[0] != VOICE_FRAME_VERSION { return Err("bad version"); }
    if buf[1] != VOICE_FRAME_TYPE_AUDIO { return Err("bad type"); }
    let seq = u64::from_be_bytes(buf[2..10].try_into().unwrap());
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&buf[10..42]);
    Ok(VoiceFrame { seq, speaker_pk: pk, opus_payload: &buf[42..] })
}

pub fn build_voice_frame(seq: u64, speaker_pk: &[u8; 32], opus: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(VOICE_FRAME_HEADER_LEN + opus.len());
    buf.push(VOICE_FRAME_VERSION);
    buf.push(VOICE_FRAME_TYPE_AUDIO);
    buf.extend_from_slice(&seq.to_be_bytes());
    buf.extend_from_slice(speaker_pk);
    buf.extend_from_slice(opus);
    buf
}

/// Replace the speaker_pk field in an existing frame buffer in place.
/// Used by the server to ensure forwarded frames carry the validated pubkey.
pub fn rewrite_speaker_pk(buf: &mut [u8], pk: &[u8; 32]) -> Result<(), &'static str> {
    if buf.len() < VOICE_FRAME_HEADER_LEN { return Err("frame too short"); }
    buf[10..42].copy_from_slice(pk);
    Ok(())
}
```

- [ ] **Step 3: Run tests**

```
cd /home/deez/farder && cargo test -p farder-server voice::tests 2>&1 | tail -10
```

Expected: 5 pass.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/voice.rs
git -C /home/deez/farder commit -m "feat(server): voice frame parser + builder + rewrite helper"
```

---

## Task 5: Datagram receive loop in connection.rs

The server-side fanout. Each authenticated connection spawns a task that reads datagrams, validates, and forwards.

**Files:**
- Modify: `crates/farder-server/src/connection.rs`

- [ ] **Step 1: Add the fanout function**

In `crates/farder-server/src/connection.rs`, alongside `broadcast_event`, add:

```rust
/// Validate an inbound voice frame and fan it out to other channel members.
/// Drops invalid frames silently (anti-DoS — never log on bad input).
async fn handle_voice_datagram(
    state: &ServerState,
    authenticated_pk: [u8; 32],
    mut datagram: bytes::Bytes,
) {
    let frame = match crate::voice::parse_voice_frame(&datagram) {
        Ok(f) => f,
        Err(_) => return,
    };
    // Anti-spoof: speaker_pk must equal the connection's authenticated identity.
    if frame.speaker_pk != authenticated_pk { return; }

    let channel_id = match state.voice.speaker_channel.read().await.get(&authenticated_pk) {
        Some(c) => *c,
        None => return,  // not in StartVoice state
    };

    // Update last-frame timestamp for the speaking ticker.
    let now_ms = crate::voice::now_ms();
    state.voice.speaking_last_frame_ms.write().await.insert(authenticated_pk, now_ms);

    // Fan out to other listeners. Skip self, skip deafened.
    let listeners = state.voice.channels.read().await
        .get(&channel_id).cloned().unwrap_or_default();
    let deafened = state.voice.deafened.read().await.clone();

    // We need to forward to ALL listeners in the channel — both transmitters
    // (in `channels`) and pure listeners (joined via JoinVoice but not StartVoice).
    // Pure listeners aren't in `channels`; we fetch them from the DB.
    let pure_listeners: Vec<[u8; 32]> = {
        let conn = state.db.lock().unwrap();
        crate::channels::get_voice_participants(&conn, channel_id)
            .unwrap_or_default()
            .into_iter()
            .map(|(pk, _)| *pk.as_bytes())
            .collect()
    };
    let mut all_recipients: HashSet<[u8; 32]> = listeners;
    all_recipients.extend(pure_listeners);
    all_recipients.remove(&authenticated_pk);

    let clients = state.clients.read().await;
    // Reuse the same datagram bytes for all sends (Bytes is cheaply cloneable).
    for listener_pk in all_recipients {
        if deafened.contains(&listener_pk) { continue; }
        if let Some(_sender) = clients.get(&listener_pk) {
            // Direct datagram send needs the Quinn Connection, not the event sender.
            // We need to extend ServerState's clients map to also hold the Connection
            // (or expose a separate voice_clients map).
            // See the implementation in step 2 below.
        }
    }
}

pub fn now_ms_helper() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
```

Wait — the existing `state.clients` map holds `EventSender` (mpsc channels for ServerEvent), not Quinn `Connection`s. We need a parallel map of `Connection` handles for direct datagram send.

- [ ] **Step 2: Add a voice_connections map to ServerState**

In `crates/farder-server/src/state.rs`, add a field:

```rust
pub voice_connections: RwLock<HashMap<[u8; 32], quinn::Connection>>,
```

In the constructor initializer:

```rust
voice_connections: RwLock::new(HashMap::new()),
```

Import `quinn` at the top if not already.

- [ ] **Step 3: Register and unregister voice_connections per connection**

In `crates/farder-server/src/connection.rs::handle_connection`, find where `state.clients` is populated (around the existing "Register client in state.clients" comment, line ~516-520). Right after registering the EventSender, also register the connection:

```rust
{
    let mut voice_conns = state.voice_connections.write().await;
    voice_conns.insert(*public_key.as_bytes(), conn.clone());
}
```

(`conn` is the `quinn::Connection`. It's `Clone` — clones share the underlying connection.)

In the connection-close cleanup path, remove it:

```rust
state.voice_connections.write().await.remove(&pk_bytes);
```

(Find the existing `state.clients.write().await.remove(...)` line; mirror it for voice_connections.)

- [ ] **Step 4: Add `crate::voice::now_ms()` and complete the handle_voice_datagram fanout**

In `crates/farder-server/src/voice.rs`, add at the bottom (above tests):

```rust
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
```

Replace the `// See the implementation in step 2 below.` comment block in `handle_voice_datagram` (from Step 1) with:

```rust
    let voice_conns = state.voice_connections.read().await;
    for listener_pk in all_recipients {
        if deafened.contains(&listener_pk) { continue; }
        if let Some(conn) = voice_conns.get(&listener_pk) {
            // Best-effort send. send_datagram takes Bytes by value; clone is cheap.
            let _ = conn.send_datagram(datagram.clone());
        }
    }
```

(Quinn's `Connection::send_datagram` is sync and returns `Result<(), SendDatagramError>` — best-effort.)

Also remove the orphaned `now_ms_helper` from connection.rs — it duplicates `voice::now_ms`.

Add `use std::collections::HashSet;` at the top of `connection.rs` if not already imported.

- [ ] **Step 5: Spawn the datagram receive loop per connection**

In `connection.rs::handle_connection`, near the existing "Spawn auxiliary stream acceptor" (around line 548-553), add another spawn for datagrams. Place AFTER the `state.clients` and `state.voice_connections` registration:

```rust
// Voice datagram receive loop. Best-effort, drops invalid frames silently.
let voice_state = Arc::clone(&state);
let voice_conn = conn.clone();
let voice_pk = *public_key.as_bytes();
let voice_acceptor = tokio::spawn(async move {
    while let Ok(datagram) = voice_conn.read_datagram().await {
        handle_voice_datagram(&voice_state, voice_pk, datagram).await;
    }
});
```

Make sure `voice_acceptor` is awaited or aborted on connection close (mirror how `stream_acceptor` is handled — search for that name in the file).

- [ ] **Step 6: Verify compile**

```
cd /home/deez/farder && cargo check -p farder-server 2>&1 | tail -10
```

Expected: `Finished`.

- [ ] **Step 7: Run server tests**

```
cd /home/deez/farder && cargo test -p farder-server 2>&1 | tail -10
```

Expected: all pass (we haven't added behavioral tests for the fanout yet — those come in later tasks).

- [ ] **Step 8: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/connection.rs crates/farder-server/src/state.rs crates/farder-server/src/voice.rs
git -C /home/deez/farder commit -m "feat(server): voice datagram receive loop + per-connection fanout"
```

---

## Phase 2: Server handlers

## Task 6: StartVoice / StopVoice / SetVoiceMute / SetVoiceDeafen handlers

**Files:**
- Modify: `crates/farder-server/src/handlers.rs`

- [ ] **Step 1: Failing tests**

In `crates/farder-server/src/handlers.rs` test module:

```rust
    #[test]
    fn test_start_voice_marks_active() {
        let conn = setup_test_db();
        let owner = test_owner(&conn);
        // Create a voice channel
        let ch_id = channels::create_channel(&conn, "voice", ChannelType::Voice, None, 0).unwrap();
        // Need to also JoinVoice first (presence)
        handle_request(&conn, &owner, true, ServerRequest::JoinVoice { channel_id: ch_id }, "/tmp").unwrap();
        let result = handle_request(&conn, &owner, true, ServerRequest::StartVoice { channel_id: ch_id }, "/tmp").unwrap();
        assert!(matches!(result.response, ServerResponse::Ok));
    }

    #[test]
    fn test_stop_voice_when_not_started() {
        let conn = setup_test_db();
        let owner = test_owner(&conn);
        let result = handle_request(&conn, &owner, true, ServerRequest::StopVoice, "/tmp").unwrap();
        // StopVoice when not in StartVoice state is a no-op success.
        assert!(matches!(result.response, ServerResponse::Ok));
    }
```

(`channels::create_channel` signature may differ — adapt by reading the existing `CreateChannel` handler arm. The test setup just needs a Voice channel that the owner can JoinVoice on.)

Run failing:
```
cd /home/deez/farder && cargo test -p farder-server handlers::tests::test_start_voice handlers::tests::test_stop_voice 2>&1 | tail -15
```
Expected: fails ("voice not yet implemented" stub from Task 1).

- [ ] **Step 2: Implement the four handler arms**

Find the stub from Task 1:
```rust
ServerRequest::StartVoice { .. }
| ServerRequest::StopVoice
| ServerRequest::SetVoiceMute { .. }
| ServerRequest::SetVoiceDeafen { .. } => {
    err("voice not yet implemented")
}
```

Replace with four real arms:

```rust
        ServerRequest::StartVoice { channel_id } => {
            if let Some(denied) = require_not_timed_out(conn, member)? {
                return Ok(denied);
            }
            let channel = channels::get_channel(conn, channel_id)?
                .ok_or_else(|| anyhow::anyhow!("channel not found"))?;
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            // SPEAK is the existing-but-unused permission for voice transmit.
            if !permissions::has(perms, permissions::SPEAK) {
                return err("missing SPEAK permission");
            }
            // Voice channels and DMs are valid; reject text channels.
            if channel.channel_type != ChannelType::Voice && channel.channel_type != ChannelType::Dm {
                return err("not a voice or DM channel");
            }
            // We can't run async here (handle_request is sync). The actual VoiceState
            // mutation happens in connection.rs after the response. Emit a flag in
            // metadata so the connection.rs layer can call voice::start_transmit.
            // Simpler approach: emit a special internal event that the connection
            // dispatcher catches.
            //
            // Concrete implementation: add a new BroadcastEvent target type,
            // EventTarget::VoiceStartTransmit { pk, channel_id }, that the dispatcher
            // intercepts to mutate state.voice. See Step 3.
            let event = BroadcastEvent {
                target: EventTarget::VoiceStartTransmit {
                    pk: *member.as_bytes(),
                    channel_id,
                },
                // No actual ServerEvent emitted to clients; this is a state mutation signal.
                event: ServerEvent::VoiceJoined {
                    channel_id, public_key: member.clone(), display_name: String::new(),
                },
            };
            // Also emit DM ringing if applicable:
            let mut events = vec![event];
            if channel.channel_type == ChannelType::Dm {
                // Was the channel empty before this StartVoice?
                let active = channels::get_voice_participants(conn, channel_id)?;
                let is_first = active.iter().filter(|(pk, _)| pk != member).count() == 0;
                if is_first {
                    if let Some((other_pk, _)) = channels::list_dm_channels(conn, member)?
                        .into_iter().find(|(ch, _)| ch.id == channel_id) {
                        let display_name = members::get_member(conn, member)?
                            .map(|m| m.display_name).unwrap_or_default();
                        events.push(BroadcastEvent {
                            target: EventTarget::Members(vec![other_pk]),
                            event: ServerEvent::VoiceCallIncoming {
                                channel_id,
                                caller: member.clone(),
                                caller_name: display_name,
                            },
                        });
                    }
                }
            }
            ok_with(ServerResponse::Ok, events)
        }

        ServerRequest::StopVoice => {
            // Same async-state-mutation issue: signal via BroadcastEvent target.
            let event = BroadcastEvent {
                target: EventTarget::VoiceStopTransmit { pk: *member.as_bytes() },
                event: ServerEvent::VoiceLeft { channel_id: 0, public_key: member.clone() },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        ServerRequest::SetVoiceMute { muted } => {
            let event = BroadcastEvent {
                target: EventTarget::VoiceSetMute { pk: *member.as_bytes(), muted },
                event: ServerEvent::VoiceLeft { channel_id: 0, public_key: member.clone() },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }

        ServerRequest::SetVoiceDeafen { deafened } => {
            let event = BroadcastEvent {
                target: EventTarget::VoiceSetDeafen { pk: *member.as_bytes(), deafened },
                event: ServerEvent::VoiceLeft { channel_id: 0, public_key: member.clone() },
            };
            ok_with(ServerResponse::Ok, vec![event])
        }
```

- [ ] **Step 3: Add new EventTarget variants for voice state mutations**

The handlers above signal voice state mutations via `BroadcastEvent` because `handle_request` is sync (no async access to RwLocks). The actual mutation happens in the dispatcher loop, which IS async.

In `crates/farder-server/src/events.rs`:

```rust
#[derive(Debug)]
pub enum EventTarget {
    All,
    Subscribers(u64),
    Members(Vec<PublicKey>),
    PermissionHolders(u64),
    /// Internal signal: mutate voice state. The `event` field on the
    /// containing BroadcastEvent is ignored for these — they don't emit to clients.
    VoiceStartTransmit { pk: [u8; 32], channel_id: u64 },
    VoiceStopTransmit { pk: [u8; 32] },
    VoiceSetMute { pk: [u8; 32], muted: bool },
    VoiceSetDeafen { pk: [u8; 32], deafened: bool },
}
```

In `connection.rs::broadcast_event`, add arms:

```rust
        EventTarget::VoiceStartTransmit { pk, channel_id } => {
            crate::voice::start_transmit(&state.voice, pk, channel_id).await;
        }
        EventTarget::VoiceStopTransmit { pk } => {
            crate::voice::stop_transmit(&state.voice, pk).await;
        }
        EventTarget::VoiceSetMute { pk, muted } => {
            let mut muted_set = state.voice.muted.write().await;
            if muted { muted_set.insert(pk); } else { muted_set.remove(&pk); }
        }
        EventTarget::VoiceSetDeafen { pk, deafened } => {
            let mut deaf_set = state.voice.deafened.write().await;
            if deafened { deaf_set.insert(pk); } else { deaf_set.remove(&pk); }
        }
```

(For these arms, do NOT call `sender.try_send(event.clone())` — they're state-mutation signals, not event broadcasts.)

- [ ] **Step 4: Run tests**

```
cd /home/deez/farder && cargo test -p farder-server handlers::tests::test_start_voice handlers::tests::test_stop_voice 2>&1 | tail -10
```

Expected: pass. The state mutation is verified in subsequent integration tasks.

- [ ] **Step 5: Run full server suite**

```
cd /home/deez/farder && cargo test -p farder-server 2>&1 | tail -10
```

Expected: all pass.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/handlers.rs crates/farder-server/src/events.rs crates/farder-server/src/connection.rs
git -C /home/deez/farder commit -m "feat(server): StartVoice/StopVoice/SetVoiceMute/SetVoiceDeafen + DM ringing"
```

---

## Task 7: Speaking-state ticker

5Hz tokio task that derives speaking state from last-frame timestamps and broadcasts `VoiceSpeakingChanged` on transitions.

**Files:**
- Modify: `crates/farder-server/src/voice.rs`
- Modify: `crates/farder-server/src/main.rs`

- [ ] **Step 1: Add the ticker function**

Append to `crates/farder-server/src/voice.rs`:

```rust
use std::sync::Arc;

/// Tokio task: every 200 ms, scan speaking_last_frame_ms; for each pk
/// whose state has flipped (was-speaking vs is-speaking based on 300 ms
/// inactivity threshold), broadcast VoiceSpeakingChanged.
pub async fn speaking_state_ticker(state: Arc<crate::state::ServerState>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
    loop {
        interval.tick().await;
        let now = now_ms();
        // Snapshot last-frames; release the lock before broadcasting.
        let last_frames: Vec<([u8; 32], u64)> = state.voice.speaking_last_frame_ms
            .read().await.iter().map(|(k, v)| (*k, *v)).collect();
        let mut changes: Vec<([u8; 32], bool)> = Vec::new();
        {
            let mut speaking_now = state.voice.speaking_now.write().await;
            for (pk, last) in &last_frames {
                let is_speaking = now.saturating_sub(*last) < 300;
                let was_speaking = speaking_now.contains(pk);
                if is_speaking != was_speaking {
                    if is_speaking { speaking_now.insert(*pk); }
                    else { speaking_now.remove(pk); }
                    changes.push((*pk, is_speaking));
                }
            }
        }
        // Broadcast each change.
        let speaker_channel = state.voice.speaker_channel.read().await;
        for (pk_bytes, speaking) in changes {
            let channel_id = match speaker_channel.get(&pk_bytes) {
                Some(c) => *c,
                // If the speaker stopped transmitting, channel_id may be gone —
                // fall back to broadcasting to All (clients that don't care will ignore).
                None => 0,
            };
            let pk = farder_crypto::identity::PublicKey::from_bytes(pk_bytes);
            let event = farder_protocol::server::ServerEvent::VoiceSpeakingChanged {
                channel_id, public_key: pk, speaking,
            };
            // Broadcast to All (low-volume event — 5Hz max per speaker, voice channels are small).
            crate::connection::broadcast_event(&state, crate::events::EventTarget::All, event).await;
        }
    }
}
```

- [ ] **Step 2: Spawn the ticker on server startup**

In `crates/farder-server/src/main.rs`, after `let _retention = retention::spawn_retention_task(...)`, add:

```rust
let _voice_ticker = tokio::spawn(crate::voice::speaking_state_ticker(Arc::clone(&state)));
```

(Or `farder_server::voice::speaking_state_ticker` depending on whether main.rs imports the crate path.)

- [ ] **Step 3: Verify compile**

```
cd /home/deez/farder && cargo check -p farder-server 2>&1 | tail -5
```

Expected: `Finished`.

- [ ] **Step 4: Smoke test the ticker** (no automated test — too timing-dependent)

Will be exercised in the manual smoke test (Task 20).

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/voice.rs crates/farder-server/src/main.rs
git -C /home/deez/farder commit -m "feat(server): 5Hz speaking-state ticker emits VoiceSpeakingChanged"
```

---

## Task 8: VoiceCallEnded on empty-DM during ring

When a DM caller leaves the voice channel before the callee accepts, the callee's incoming-call modal should auto-close. This requires emitting `VoiceCallEnded` when a DM voice channel becomes empty.

**Files:**
- Modify: `crates/farder-server/src/handlers.rs`

- [ ] **Step 1: Add VoiceCallEnded emission to LeaveVoice**

Find the `ServerRequest::LeaveVoice { channel_id } =>` arm. Currently:

```rust
ServerRequest::LeaveVoice { channel_id } => {
    channels::leave_voice(conn, channel_id, member)?;
    ok_with(ServerResponse::Ok, vec![BroadcastEvent {
        target: EventTarget::All,
        event: ServerEvent::VoiceLeft { channel_id, public_key: member.clone() },
    }])
}
```

Replace with:

```rust
ServerRequest::LeaveVoice { channel_id } => {
    let channel = channels::get_channel(conn, channel_id)?;
    channels::leave_voice(conn, channel_id, member)?;
    let mut events = vec![BroadcastEvent {
        target: EventTarget::All,
        event: ServerEvent::VoiceLeft { channel_id, public_key: member.clone() },
    }];
    // Also stop transmit (sync-side via VoiceStopTransmit signal)
    events.push(BroadcastEvent {
        target: EventTarget::VoiceStopTransmit { pk: *member.as_bytes() },
        event: ServerEvent::VoiceLeft { channel_id, public_key: member.clone() },
    });
    // If this was a DM and the channel is now empty, emit VoiceCallEnded to the other party.
    if let Some(ch) = channel {
        if ch.channel_type == ChannelType::Dm {
            let remaining = channels::get_voice_participants(conn, channel_id)?;
            if remaining.is_empty() {
                if let Some((other_pk, _)) = channels::list_dm_channels(conn, member)?
                    .into_iter().find(|(c, _)| c.id == channel_id) {
                    events.push(BroadcastEvent {
                        target: EventTarget::Members(vec![other_pk]),
                        event: ServerEvent::VoiceCallEnded { channel_id },
                    });
                }
            }
        }
    }
    ok_with(ServerResponse::Ok, events)
}
```

- [ ] **Step 2: Add a similar emission to StopVoice**

In the `StopVoice` arm, expand to:

```rust
ServerRequest::StopVoice => {
    // Look up the channel they're transmitting in (may be None).
    // We can't read state.voice.speaker_channel from sync handle_request,
    // so we rely on connection.rs to emit VoiceCallEnded after processing
    // the VoiceStopTransmit signal — see Step 3.
    let event = BroadcastEvent {
        target: EventTarget::VoiceStopTransmit { pk: *member.as_bytes() },
        event: ServerEvent::VoiceLeft { channel_id: 0, public_key: member.clone() },
    };
    ok_with(ServerResponse::Ok, vec![event])
}
```

- [ ] **Step 3: Make VoiceStopTransmit also check for empty-DM and emit VoiceCallEnded**

In `connection.rs::broadcast_event`'s `EventTarget::VoiceStopTransmit` arm, expand:

```rust
        EventTarget::VoiceStopTransmit { pk } => {
            // Look up which channel they were transmitting in BEFORE removal.
            let prev_channel = state.voice.speaker_channel.read().await.get(&pk).copied();
            crate::voice::stop_transmit(&state.voice, pk).await;
            // If this was a DM and now nobody's transmitting, emit VoiceCallEnded
            // to the other party.
            if let Some(channel_id) = prev_channel {
                let now_empty = state.voice.channels.read().await
                    .get(&channel_id).map(|s| s.is_empty()).unwrap_or(true);
                if now_empty {
                    let conn_lock = state.db.lock().unwrap();
                    if let Ok(Some(ch)) = crate::channels::get_channel(&conn_lock, channel_id) {
                        if ch.channel_type == farder_protocol::server::ChannelType::Dm {
                            // Find the other DM participant.
                            let pk_obj = farder_crypto::identity::PublicKey::from_bytes(pk);
                            if let Ok(dms) = crate::channels::list_dm_channels(&conn_lock, &pk_obj) {
                                if let Some((other_pk, _)) = dms.into_iter().find(|(c, _)| c.id == channel_id) {
                                    drop(conn_lock);
                                    crate::connection::broadcast_event(
                                        &state,
                                        crate::events::EventTarget::Members(vec![other_pk]),
                                        farder_protocol::server::ServerEvent::VoiceCallEnded { channel_id },
                                    ).await;
                                }
                            }
                        }
                    }
                }
            }
        }
```

(Note: `broadcast_event` calling itself recursively — Rust doesn't mind this, but watch for infinite recursion. The recursive call uses `EventTarget::Members` which doesn't loop back through VoiceStopTransmit. Safe.)

- [ ] **Step 4: Verify compile**

```
cd /home/deez/farder && cargo check -p farder-server 2>&1 | tail -5
```

Expected: `Finished`.

- [ ] **Step 5: Run tests**

```
cd /home/deez/farder && cargo test -p farder-server 2>&1 | tail -10
```

Expected: all pass.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/handlers.rs crates/farder-server/src/connection.rs
git -C /home/deez/farder commit -m "feat(server): emit VoiceCallEnded when DM voice empties during ring"
```

---

## Phase 3: Client Rust audio engine

## Task 9: cpal capture thread + ring buffer

Build the input side: cpal opens the default input device, samples flow into a ring buffer that the encode thread will drain.

**Files:**
- Modify: `client/src-tauri/Cargo.toml`
- Create: `client/src-tauri/src/voice.rs`

- [ ] **Step 1: Add audiopus dep**

In `client/src-tauri/Cargo.toml`, under `[dependencies]`:

```toml
audiopus = "0.3"
```

- [ ] **Step 2: Create voice.rs with config + capture skeleton**

`client/src-tauri/src/voice.rs`:

```rust
use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 1;
const FRAME_SAMPLES: usize = 960; // 20 ms at 48 kHz mono
const RING_CAPACITY: usize = FRAME_SAMPLES * 50; // ~1 second

#[derive(Clone)]
pub struct VoiceConfig {
    pub muted: bool,
    pub deafened: bool,
    pub input_volume: f32,
    pub output_volume: f32,
    pub per_user_volume: HashMap<[u8; 32], f32>,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub ptt_enabled: bool,
    pub ptt_active: bool,
    pub vad_threshold: f32,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            muted: false, deafened: false,
            input_volume: 1.0, output_volume: 1.0,
            per_user_volume: HashMap::new(),
            input_device: None, output_device: None,
            ptt_enabled: false, ptt_active: false,
            vad_threshold: 0.02,
        }
    }
}

/// Simple lock-protected ring buffer of f32 samples. Single producer, single consumer.
pub struct AudioRing {
    buf: std::collections::VecDeque<f32>,
    capacity: usize,
}

impl AudioRing {
    pub fn new(capacity: usize) -> Self {
        Self { buf: std::collections::VecDeque::with_capacity(capacity), capacity }
    }
    pub fn push_slice(&mut self, samples: &[f32]) {
        for &s in samples {
            if self.buf.len() >= self.capacity { self.buf.pop_front(); }  // overflow: drop oldest
            self.buf.push_back(s);
        }
    }
    pub fn pop_n(&mut self, n: usize) -> Option<Vec<f32>> {
        if self.buf.len() < n { return None; }
        Some((0..n).map(|_| self.buf.pop_front().unwrap()).collect())
    }
    pub fn len(&self) -> usize { self.buf.len() }
}

pub struct VoiceSession {
    pub server_id: String,
    pub channel_id: u64,
    pub config: Arc<Mutex<VoiceConfig>>,
    pub capture_ring: Arc<Mutex<AudioRing>>,
    // Holds the cpal input stream so it stays alive. Stream dropped = capture stopped.
    _input_stream: cpal::Stream,
    pub shutdown_tx: Option<oneshot::Sender<()>>,
}

unsafe impl Send for VoiceSession {}

/// Start cpal input capture. Returns a stream handle (must be kept alive).
pub fn build_input_stream(
    device_name: Option<&str>,
    capture_ring: Arc<Mutex<AudioRing>>,
) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = match device_name {
        Some(name) => host.input_devices()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| anyhow!("input device {} not found", name))?,
        None => host.default_input_device()
            .ok_or_else(|| anyhow!("no default input device"))?,
    };
    let config = device.default_input_config()?;
    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();
    let err_fn = |e| eprintln!("[voice] input stream error: {e}");
    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mut ring = capture_ring.lock().unwrap();
                ring.push_slice(data);
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let f32_samples: Vec<f32> = data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                let mut ring = capture_ring.lock().unwrap();
                ring.push_slice(&f32_samples);
            },
            err_fn,
            None,
        )?,
        _ => return Err(anyhow!("unsupported input sample format")),
    };
    stream.play()?;
    Ok(stream)
}
```

(For cpal's stream-config sample-rate conversion: `default_input_config()` may not be 48kHz. Resampling is handled in the encode thread — Task 10. For simplicity in v1, the encode thread asserts the sample rate is 48kHz; if not, it logs a warning and skips. Resampling to 48kHz is a v1.5 polish.)

- [ ] **Step 3: Add a global session holder**

```rust
static VOICE_SESSION: once_cell::sync::Lazy<Mutex<Option<VoiceSession>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(None));

pub fn current_session() -> std::sync::MutexGuard<'static, Option<VoiceSession>> {
    VOICE_SESSION.lock().unwrap()
}
```

(`once_cell` is already a transitive dep. If it's not directly available, `std::sync::OnceLock` works too.)

- [ ] **Step 4: Verify compile (just the new module + dep)**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -10
```

Expected: `Finished`. May warn about unused functions.

Add `pub mod voice;` to `client/src-tauri/src/main.rs` (above `fn main()`):

```rust
mod voice;
```

Run check again, expect Finished.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/Cargo.toml client/src-tauri/Cargo.lock client/src-tauri/src/voice.rs client/src-tauri/src/main.rs
git -C /home/deez/farder commit -m "feat(client): voice.rs skeleton + cpal input stream + audiopus dep"
```

---

## Task 10: Opus encode + VAD/PTT gate + datagram send

Encode thread: every 20 ms, drain a frame from the capture ring, apply VAD/PTT, encode, and send.

**Files:**
- Modify: `client/src-tauri/src/voice.rs`

- [ ] **Step 1: Add the encode thread**

Append to `voice.rs`:

```rust
pub fn spawn_encode_thread(
    config: Arc<Mutex<VoiceConfig>>,
    capture_ring: Arc<Mutex<AudioRing>>,
    own_pk: [u8; 32],
    quic_conn: quinn::Connection,
    mut shutdown_rx: oneshot::Receiver<()>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut encoder = match audiopus::coder::Encoder::new(
            audiopus::SampleRate::Hz48000,
            audiopus::Channels::Mono,
            audiopus::Application::Voip,
        ) {
            Ok(e) => e,
            Err(err) => { eprintln!("[voice] opus encoder init failed: {err}"); return; }
        };
        if encoder.set_bitrate(audiopus::Bitrate::BitsPerSecond(32_000)).is_err() {
            eprintln!("[voice] opus set_bitrate failed");
        }

        let mut seq: u64 = 0;
        let mut opus_out = vec![0u8; 256];
        let mut last_above_threshold_ms: u64 = 0;
        let frame_duration = std::time::Duration::from_millis(20);
        let mut next_tick = std::time::Instant::now();

        loop {
            // Check shutdown
            if shutdown_rx.try_recv().is_ok() { break; }

            // Try drain a 960-sample frame
            let pcm = {
                let mut ring = capture_ring.lock().unwrap();
                ring.pop_n(FRAME_SAMPLES)
            };
            if pcm.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(2));
                continue;
            }
            let mut pcm = pcm.unwrap();

            // Pace to ~50Hz (don't burst)
            next_tick += frame_duration;
            let now = std::time::Instant::now();
            if next_tick > now {
                std::thread::sleep(next_tick - now);
            } else if now > next_tick + frame_duration * 5 {
                next_tick = now + frame_duration; // re-sync if we drifted >100ms
            }

            // Read config
            let cfg = config.lock().unwrap().clone();

            // Apply input volume
            if cfg.input_volume != 1.0 {
                for s in pcm.iter_mut() { *s *= cfg.input_volume; }
            }

            // VAD/PTT gate
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
            let rms = (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32).sqrt();
            let gate_open = if cfg.muted {
                false
            } else if cfg.ptt_enabled {
                cfg.ptt_active
            } else {
                if rms > cfg.vad_threshold {
                    last_above_threshold_ms = now_ms;
                    true
                } else {
                    now_ms.saturating_sub(last_above_threshold_ms) < 200  // 200ms hangover
                }
            };

            if !gate_open { continue; }

            // Encode
            let encoded_len = match encoder.encode_float(&pcm, &mut opus_out) {
                Ok(n) => n,
                Err(e) => { eprintln!("[voice] opus encode err: {e}"); continue; }
            };

            // Build frame
            let frame = build_voice_frame(seq, &own_pk, &opus_out[..encoded_len]);
            seq = seq.wrapping_add(1);

            // Send (best-effort)
            let _ = quic_conn.send_datagram(bytes::Bytes::from(frame));
        }
    })
}

fn build_voice_frame(seq: u64, speaker_pk: &[u8; 32], opus: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(42 + opus.len());
    buf.push(0x01); // version
    buf.push(0x01); // type: audio
    buf.extend_from_slice(&seq.to_be_bytes());
    buf.extend_from_slice(speaker_pk);
    buf.extend_from_slice(opus);
    buf
}
```

- [ ] **Step 2: Verify compile**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
```

Expected: `Finished` (will warn about unused fn — wired up in Task 13).

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/voice.rs
git -C /home/deez/farder commit -m "feat(client): voice encode thread (Opus + VAD/PTT gate + datagram send)"
```

---

## Task 11: Datagram receive + per-speaker decoder

Receive thread reads datagrams from the QUIC connection, parses, and feeds the right per-speaker buffer.

**Files:**
- Modify: `client/src-tauri/src/voice.rs`

- [ ] **Step 1: Add the receive thread (async, runs on tokio)**

Append to `voice.rs`:

```rust
pub fn spawn_recv_task(
    config: Arc<Mutex<VoiceConfig>>,
    speaker_buffers: Arc<Mutex<HashMap<[u8; 32], AudioRing>>>,
    decoders: Arc<Mutex<HashMap<[u8; 32], audiopus::coder::Decoder>>>,
    quic_conn: quinn::Connection,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let datagram = match quic_conn.read_datagram().await {
                Ok(d) => d,
                Err(_) => return, // connection closed
            };
            if datagram.len() < 42 { continue; }
            let buf = datagram.as_ref();
            if buf[0] != 0x01 || buf[1] != 0x01 { continue; }
            let mut speaker_pk = [0u8; 32];
            speaker_pk.copy_from_slice(&buf[10..42]);
            let opus_payload = &buf[42..];

            // If deafened, drop everything.
            if config.lock().unwrap().deafened { continue; }

            // Get-or-create decoder for this speaker.
            let mut decoders_g = decoders.lock().unwrap();
            let decoder = decoders_g.entry(speaker_pk).or_insert_with(|| {
                audiopus::coder::Decoder::new(
                    audiopus::SampleRate::Hz48000,
                    audiopus::Channels::Mono,
                ).expect("opus decoder init")
            });
            let mut pcm = vec![0.0f32; FRAME_SAMPLES];
            let n = match decoder.decode_float(Some(opus_payload), &mut pcm[..], false) {
                Ok(n) => n,
                Err(_) => { continue; }
            };
            pcm.truncate(n);
            drop(decoders_g);

            // Push into the per-speaker playback buffer.
            let mut speakers_g = speaker_buffers.lock().unwrap();
            let buf = speakers_g.entry(speaker_pk).or_insert_with(|| AudioRing::new(RING_CAPACITY));
            buf.push_slice(&pcm);
        }
    })
}
```

- [ ] **Step 2: Verify compile**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
```

Expected: `Finished`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/voice.rs
git -C /home/deez/farder commit -m "feat(client): voice recv task (datagram parse + Opus decode + per-speaker buffers)"
```

---

## Task 12: Mixer + cpal playback thread + start/stop wiring

Playback thread mixes per-speaker buffers and writes to cpal output. Plus `start()` / `stop()` glue functions that spawn/join threads.

**Files:**
- Modify: `client/src-tauri/src/voice.rs`

- [ ] **Step 1: Add the playback stream builder**

Append to `voice.rs`:

```rust
pub fn build_output_stream(
    device_name: Option<&str>,
    config_arc: Arc<Mutex<VoiceConfig>>,
    speaker_buffers: Arc<Mutex<HashMap<[u8; 32], AudioRing>>>,
) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = match device_name {
        Some(name) => host.output_devices()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| anyhow!("output device {} not found", name))?,
        None => host.default_output_device()
            .ok_or_else(|| anyhow!("no default output device"))?,
    };
    let config = device.default_output_config()?;
    let stream_config: cpal::StreamConfig = config.into();
    let err_fn = |e| eprintln!("[voice] output stream error: {e}");

    let stream = device.build_output_stream(
        &stream_config,
        move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let cfg = config_arc.lock().unwrap().clone();
            let mut speakers_g = speaker_buffers.lock().unwrap();

            for sample_slot in out.iter_mut() {
                let mut mix = 0.0f32;
                for (pk, ring) in speakers_g.iter_mut() {
                    if let Some(s) = ring.buf.pop_front() {
                        let per_user = cfg.per_user_volume.get(pk).copied().unwrap_or(1.0);
                        mix += s * per_user;
                    }
                }
                mix *= cfg.output_volume;
                // Soft clip
                *sample_slot = mix.clamp(-1.0, 1.0);
            }
        },
        err_fn,
        None,
    )?;
    stream.play()?;
    Ok(stream)
}
```

- [ ] **Step 2: Add the `start()` and `stop()` orchestration functions**

Append:

```rust
pub async fn start(
    server_id: String,
    channel_id: u64,
    own_pk: [u8; 32],
    quic_conn: quinn::Connection,
    initial_config: VoiceConfig,
) -> Result<()> {
    // Stop any existing session first.
    stop().await?;

    let config = Arc::new(Mutex::new(initial_config.clone()));
    let capture_ring = Arc::new(Mutex::new(AudioRing::new(RING_CAPACITY)));
    let speaker_buffers = Arc::new(Mutex::new(HashMap::new()));
    let decoders = Arc::new(Mutex::new(HashMap::new()));

    let input_stream = build_input_stream(initial_config.input_device.as_deref(), Arc::clone(&capture_ring))?;
    let output_stream = build_output_stream(
        initial_config.output_device.as_deref(),
        Arc::clone(&config),
        Arc::clone(&speaker_buffers),
    )?;

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let _encode_handle = spawn_encode_thread(
        Arc::clone(&config),
        Arc::clone(&capture_ring),
        own_pk,
        quic_conn.clone(),
        shutdown_rx,
    );
    let _recv_handle = spawn_recv_task(
        Arc::clone(&config),
        Arc::clone(&speaker_buffers),
        Arc::clone(&decoders),
        quic_conn.clone(),
    );

    let session = VoiceSession {
        server_id, channel_id,
        config, capture_ring,
        _input_stream: input_stream,
        shutdown_tx: Some(shutdown_tx),
    };
    // Drop output_stream by binding so it's stored. Otherwise dropped here = silence.
    // Move it into the session via a hack: since VoiceSession only stores _input_stream,
    // store output via a static. Simplest: extend VoiceSession.
    drop(output_stream);  // FIX: extend VoiceSession with _output_stream
    *current_session() = Some(session);
    Ok(())
}

pub async fn stop() -> Result<()> {
    let mut sess = current_session();
    if let Some(mut s) = sess.take() {
        if let Some(tx) = s.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // Streams drop here, stopping cpal.
    }
    Ok(())
}

pub fn set_mute(muted: bool) {
    if let Some(s) = current_session().as_ref() {
        s.config.lock().unwrap().muted = muted;
    }
}

pub fn set_deafen(deafened: bool) {
    if let Some(s) = current_session().as_ref() {
        s.config.lock().unwrap().deafened = deafened;
    }
}

pub fn set_input_volume(v: f32) {
    if let Some(s) = current_session().as_ref() {
        s.config.lock().unwrap().input_volume = v.clamp(0.0, 2.0);
    }
}

pub fn set_output_volume(v: f32) {
    if let Some(s) = current_session().as_ref() {
        s.config.lock().unwrap().output_volume = v.clamp(0.0, 2.0);
    }
}

pub fn set_per_user_volume(pk: [u8; 32], v: f32) {
    if let Some(s) = current_session().as_ref() {
        s.config.lock().unwrap().per_user_volume.insert(pk, v.clamp(0.0, 2.0));
    }
}

pub fn set_ptt_enabled(enabled: bool) {
    if let Some(s) = current_session().as_ref() {
        s.config.lock().unwrap().ptt_enabled = enabled;
    }
}

pub fn set_ptt_active(active: bool) {
    if let Some(s) = current_session().as_ref() {
        s.config.lock().unwrap().ptt_active = active;
    }
}

pub fn set_vad_threshold(t: f32) {
    if let Some(s) = current_session().as_ref() {
        s.config.lock().unwrap().vad_threshold = t.clamp(0.0, 1.0);
    }
}

pub fn list_audio_devices() -> Result<(Vec<String>, Vec<String>)> {
    let host = cpal::default_host();
    let inputs: Vec<String> = host.input_devices()?
        .filter_map(|d| d.name().ok()).collect();
    let outputs: Vec<String> = host.output_devices()?
        .filter_map(|d| d.name().ok()).collect();
    Ok((inputs, outputs))
}
```

- [ ] **Step 3: Fix the dropped output_stream**

The skeleton above has a bug — `output_stream` is dropped immediately. Extend `VoiceSession` to hold both:

```rust
pub struct VoiceSession {
    pub server_id: String,
    pub channel_id: u64,
    pub config: Arc<Mutex<VoiceConfig>>,
    pub capture_ring: Arc<Mutex<AudioRing>>,
    _input_stream: cpal::Stream,
    _output_stream: cpal::Stream,
    pub shutdown_tx: Option<oneshot::Sender<()>>,
}
```

In `start()`, replace the `drop(output_stream)` line with the correct field assignment when constructing `VoiceSession`:

```rust
let session = VoiceSession {
    server_id, channel_id,
    config, capture_ring,
    _input_stream: input_stream,
    _output_stream: output_stream,
    shutdown_tx: Some(shutdown_tx),
};
*current_session() = Some(session);
Ok(())
```

- [ ] **Step 4: Verify compile**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -10
```

Expected: `Finished`. The `unsafe impl Send for VoiceSession` may need to also account for `_output_stream` — cpal::Stream is `!Send` on macOS. On Linux/Windows it's Send. Mark the impl with cfg gates if Mac support matters; for v1 in WSL/Linux dev, this is fine.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/voice.rs
git -C /home/deez/farder commit -m "feat(client): mixer playback + start/stop orchestration + session control"
```

---

## Phase 4: Client Tauri bridge

## Task 13: Tauri commands wrapping voice::*

13 commands. Each is a thin async wrapper.

**Files:**
- Modify: `client/src-tauri/src/commands.rs`
- Modify: `client/src-tauri/src/main.rs`

- [ ] **Step 1: Add all 13 commands**

Append to `client/src-tauri/src/commands.rs` (near other voice/audio commands or at the end):

```rust
#[tauri::command]
pub async fn start_voice(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
) -> Result<(), String> {
    // Send the StartVoice request to server first.
    bridge::send_request(&state, &server_id, ServerRequest::StartVoice { channel_id })
        .await.map_err(|e| e.to_string())?;
    // Get the QUIC connection handle for the audio engine.
    let conn = state.connections.lock().unwrap().get(&server_id).cloned()
        .ok_or_else(|| "not connected".to_string())?;
    let quic_conn = conn.quinn_connection.clone();  // see note below
    let own_pk = parse_public_key(&get_own_pk_string(&state)?)?;
    let initial_config = load_voice_config_from_settings();
    crate::voice::start(server_id, channel_id, *own_pk.as_bytes(), quic_conn, initial_config)
        .await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn stop_voice(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    crate::voice::stop().await.map_err(|e| e.to_string())?;
    if let Some(server_id) = crate::voice::current_session().as_ref().map(|s| s.server_id.clone()) {
        let _ = bridge::send_request(&state, &server_id, ServerRequest::StopVoice).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn set_voice_mute(state: State<'_, Arc<AppState>>, muted: bool) -> Result<(), String> {
    crate::voice::set_mute(muted);
    if let Some(server_id) = crate::voice::current_session().as_ref().map(|s| s.server_id.clone()) {
        bridge::send_request(&state, &server_id, ServerRequest::SetVoiceMute { muted })
            .await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn set_voice_deafen(state: State<'_, Arc<AppState>>, deafened: bool) -> Result<(), String> {
    crate::voice::set_deafen(deafened);
    if let Some(server_id) = crate::voice::current_session().as_ref().map(|s| s.server_id.clone()) {
        bridge::send_request(&state, &server_id, ServerRequest::SetVoiceDeafen { deafened })
            .await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn set_voice_input_volume(volume: f32) {
    crate::voice::set_input_volume(volume);
    let _ = settings_set("voice_input_volume", serde_json::json!(volume));
}

#[tauri::command]
pub fn set_voice_output_volume(volume: f32) {
    crate::voice::set_output_volume(volume);
    let _ = settings_set("voice_output_volume", serde_json::json!(volume));
}

#[tauri::command]
pub fn set_voice_per_user_volume(member_key: String, volume: f32) -> Result<(), String> {
    let pk = parse_public_key(&member_key)?;
    crate::voice::set_per_user_volume(*pk.as_bytes(), volume);
    // Persist the whole map under voice_per_user_volumes.
    let mut map: serde_json::Map<String, serde_json::Value> = settings_get("voice_per_user_volumes")
        .and_then(|v| v.as_object().cloned()).unwrap_or_default();
    map.insert(member_key, serde_json::json!(volume));
    let _ = settings_set("voice_per_user_volumes", serde_json::Value::Object(map));
    Ok(())
}

#[tauri::command]
pub fn set_voice_input_device(device: Option<String>) {
    let _ = settings_set("voice_input_device", match &device {
        Some(s) => serde_json::Value::String(s.clone()),
        None => serde_json::Value::Null,
    });
    // Note: device change requires session restart. UI should call stop_voice + start_voice.
}

#[tauri::command]
pub fn set_voice_output_device(device: Option<String>) {
    let _ = settings_set("voice_output_device", match &device {
        Some(s) => serde_json::Value::String(s.clone()),
        None => serde_json::Value::Null,
    });
}

#[tauri::command]
pub fn set_voice_ptt_enabled(enabled: bool) {
    crate::voice::set_ptt_enabled(enabled);
    let _ = settings_set("voice_use_ptt", serde_json::json!(enabled));
}

#[tauri::command]
pub fn set_voice_ptt_active(active: bool) {
    crate::voice::set_ptt_active(active);
}

#[tauri::command]
pub fn set_voice_vad_threshold(threshold: f32) {
    crate::voice::set_vad_threshold(threshold);
    let _ = settings_set("voice_vad_threshold", serde_json::json!(threshold));
}

#[tauri::command]
pub fn list_audio_devices() -> Result<serde_json::Value, String> {
    let (inputs, outputs) = crate::voice::list_audio_devices().map_err(|e| e.to_string())?;
    Ok(serde_json::json!({"inputs": inputs, "outputs": outputs}))
}

fn load_voice_config_from_settings() -> crate::voice::VoiceConfig {
    let mut cfg = crate::voice::VoiceConfig::default();
    if let Some(v) = settings_get("voice_input_device").and_then(|v| v.as_str().map(String::from)) {
        cfg.input_device = Some(v);
    }
    if let Some(v) = settings_get("voice_output_device").and_then(|v| v.as_str().map(String::from)) {
        cfg.output_device = Some(v);
    }
    if let Some(v) = settings_get("voice_input_volume").and_then(|v| v.as_f64()) {
        cfg.input_volume = v as f32;
    }
    if let Some(v) = settings_get("voice_output_volume").and_then(|v| v.as_f64()) {
        cfg.output_volume = v as f32;
    }
    if let Some(v) = settings_get("voice_use_ptt").and_then(|v| v.as_bool()) {
        cfg.ptt_enabled = v;
    }
    if let Some(v) = settings_get("voice_vad_threshold").and_then(|v| v.as_f64()) {
        cfg.vad_threshold = v as f32;
    }
    cfg
}

fn get_own_pk_string(_state: &Arc<AppState>) -> Result<String, String> {
    // Reuse the existing helper used by other commands; if it's named differently,
    // adapt. Common pattern: state.identity.public_key().to_string()
    crate::commands::get_public_key_sync().ok_or_else(|| "no identity".to_string())
}
```

(`get_own_pk_string` may need to be wired to your existing identity helper — search for `get_public_key` usage in the file. If commands.rs doesn't have a sync flavor, add one or call `crate::commands::get_public_key()` directly.)

(`state.connections` and `quinn_connection` — verify exact names by searching commands.rs for how `bridge::send_request` accesses the connection. Whatever it uses to dispatch a request, expose the `quinn::Connection` similarly. If the connection type is wrapped in `Arc`, use `.clone()` to get a fresh handle.)

- [ ] **Step 2: Register all 13 commands in main.rs**

In `client/src-tauri/src/main.rs`, in the `tauri::generate_handler![ ... ]` block, add:

```rust
            commands::start_voice,
            commands::stop_voice,
            commands::set_voice_mute,
            commands::set_voice_deafen,
            commands::set_voice_input_volume,
            commands::set_voice_output_volume,
            commands::set_voice_per_user_volume,
            commands::set_voice_input_device,
            commands::set_voice_output_device,
            commands::set_voice_ptt_enabled,
            commands::set_voice_ptt_active,
            commands::set_voice_vad_threshold,
            commands::list_audio_devices,
```

- [ ] **Step 3: Verify compile**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -10
```

Expected: `Finished`. There may be missing helpers (`get_public_key_sync`, `quinn_connection` field) that you need to add or rename — adapt to existing patterns in commands.rs.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs
git -C /home/deez/farder commit -m "feat(client): 13 Tauri commands wrapping voice engine"
```

---

## Task 14: bridge.rs voice event emissions

**Files:**
- Modify: `client/src-tauri/src/bridge.rs`

- [ ] **Step 1: Replace the no-op stub**

Find the no-op stub from Task 1 and replace with real emit arms:

```rust
        ServerEvent::VoiceCallIncoming { channel_id, caller, caller_name } =>
            app.emit("server:voice_call_incoming", serde_json::json!({
                "server_id": sid,
                "channel_id": channel_id,
                "caller": caller.to_string(),
                "caller_name": caller_name,
            })),
        ServerEvent::VoiceCallEnded { channel_id } =>
            app.emit("server:voice_call_ended", serde_json::json!({
                "server_id": sid,
                "channel_id": channel_id,
            })),
        ServerEvent::VoiceSpeakingChanged { channel_id, public_key, speaking } =>
            app.emit("server:voice_speaking_changed", serde_json::json!({
                "server_id": sid,
                "channel_id": channel_id,
                "public_key": public_key.to_string(),
                "speaking": speaking,
            })),
```

- [ ] **Step 2: Verify compile**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -3
```

Expected: `Finished`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/bridge.rs
git -C /home/deez/farder commit -m "feat(client): emit voice_call_incoming/ended + voice_speaking_changed"
```

---

## Phase 5: Client UI

## Task 15: TS bridge bindings + reducer state

**Files:**
- Modify: `client/src/lib/tauri-bridge.ts`
- Modify: `client/src/context/ServerContext.tsx`
- Modify: `client/src/hooks/useServerEvents.ts`

- [ ] **Step 1: Add 13 invoke wrappers + types in tauri-bridge.ts**

Append to `client/src/lib/tauri-bridge.ts`:

```ts
export async function startVoice(serverId: string, channelId: number): Promise<void> {
  return invoke<void>("start_voice", { serverId, channelId });
}
export async function stopVoice(): Promise<void> {
  return invoke<void>("stop_voice");
}
export async function setVoiceMute(muted: boolean): Promise<void> {
  return invoke<void>("set_voice_mute", { muted });
}
export async function setVoiceDeafen(deafened: boolean): Promise<void> {
  return invoke<void>("set_voice_deafen", { deafened });
}
export async function setVoiceInputVolume(volume: number): Promise<void> {
  return invoke<void>("set_voice_input_volume", { volume });
}
export async function setVoiceOutputVolume(volume: number): Promise<void> {
  return invoke<void>("set_voice_output_volume", { volume });
}
export async function setVoicePerUserVolume(memberKey: string, volume: number): Promise<void> {
  return invoke<void>("set_voice_per_user_volume", { memberKey, volume });
}
export async function setVoiceInputDevice(device: string | null): Promise<void> {
  return invoke<void>("set_voice_input_device", { device });
}
export async function setVoiceOutputDevice(device: string | null): Promise<void> {
  return invoke<void>("set_voice_output_device", { device });
}
export async function setVoicePttEnabled(enabled: boolean): Promise<void> {
  return invoke<void>("set_voice_ptt_enabled", { enabled });
}
export async function setVoicePttActive(active: boolean): Promise<void> {
  return invoke<void>("set_voice_ptt_active", { active });
}
export async function setVoiceVadThreshold(threshold: number): Promise<void> {
  return invoke<void>("set_voice_vad_threshold", { threshold });
}
export async function listAudioDevices(): Promise<{ inputs: string[]; outputs: string[] }> {
  return invoke<{ inputs: string[]; outputs: string[] }>("list_audio_devices");
}
```

- [ ] **Step 2: Extend AppState + actions in ServerContext.tsx**

In `client/src/context/ServerContext.tsx`:

In `PerServerState` interface, add:

```ts
voiceSpeakingPks: Set<string>;
```

In `initialPerServerState`:

```ts
voiceSpeakingPks: new Set(),
```

In `AppState` interface, add:

```ts
voiceCallIncoming: { serverId: string; channelId: number; callerPk: string; callerName: string } | null;
```

In `initialAppState`:

```ts
voiceCallIncoming: null,
```

In `AppAction` union, append:

```ts
| { type: "VOICE_SPEAKING_CHANGED"; serverId: string; payload: { publicKey: string; speaking: boolean } }
| { type: "VOICE_CALL_INCOMING"; serverId: string; channelId: number; callerPk: string; callerName: string }
| { type: "VOICE_CALL_ENDED"; serverId: string; channelId: number }
| { type: "CLEAR_VOICE_CALL_INCOMING" };
```

In `perServerReducer`, add:

```ts
case "VOICE_SPEAKING_CHANGED": {
  const next = new Set(state.voiceSpeakingPks);
  if (action.payload.speaking) next.add(action.payload.publicKey);
  else next.delete(action.payload.publicKey);
  return { ...state, voiceSpeakingPks: next };
}
```

In `appReducer`, near other top-level actions:

```ts
case "VOICE_CALL_INCOMING":
  return { ...state, voiceCallIncoming: {
    serverId: action.serverId, channelId: action.channelId,
    callerPk: action.callerPk, callerName: action.callerName,
  }};
case "VOICE_CALL_ENDED":
  return state.voiceCallIncoming &&
         state.voiceCallIncoming.serverId === action.serverId &&
         state.voiceCallIncoming.channelId === action.channelId
    ? { ...state, voiceCallIncoming: null }
    : state;
case "CLEAR_VOICE_CALL_INCOMING":
  return { ...state, voiceCallIncoming: null };
```

- [ ] **Step 3: Add 3 listeners in useServerEvents.ts**

After existing voice event listeners, add:

```ts
listen("server:voice_call_incoming", (e) => {
  const data = e.payload as { server_id: string; channel_id: number; caller: string; caller_name: string };
  dispatch({ type: "VOICE_CALL_INCOMING", serverId: data.server_id, channelId: data.channel_id,
    callerPk: data.caller, callerName: data.caller_name });
}).then(safePush);

listen("server:voice_call_ended", (e) => {
  const data = e.payload as { server_id: string; channel_id: number };
  dispatch({ type: "VOICE_CALL_ENDED", serverId: data.server_id, channelId: data.channel_id });
}).then(safePush);

listen("server:voice_speaking_changed", (e) => {
  const data = e.payload as { server_id: string; channel_id: number; public_key: string; speaking: boolean };
  dispatch({ type: "VOICE_SPEAKING_CHANGED", serverId: data.server_id,
    payload: { publicKey: data.public_key, speaking: data.speaking } });
}).then(safePush);
```

- [ ] **Step 4: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src/lib/tauri-bridge.ts client/src/context/ServerContext.tsx client/src/hooks/useServerEvents.ts
git -C /home/deez/farder commit -m "feat(client): voice TS bindings + reducer state for speaking/ringing"
```

---

## Task 16: ChannelSidebar speaking ring + mic/headphone icons

**Files:**
- Modify: `client/src/components/ChannelSidebar.tsx`

- [ ] **Step 1: Add a small VoiceMemberRow component within ChannelSidebar.tsx**

Find the existing voice-channel rendering (`renderVoiceChannel` function, around line 344). It iterates voice participants. Add per-row:
- Mic icon (red if muted; gray if not muted) — for now, mute state isn't propagated; render gray always for v1, with a TODO. (Optionally, add a server-side broadcast event for mute state in a follow-up.)
- Headphones icon if deafened — same caveat.
- A 2px green ring around the avatar when `voiceSpeakingPks.has(pk)`.

The mute/deafen indicators in v1 can use only the actor's own state (showing your own mute status) since there's no broadcast yet. For others, render gray always. Document the limitation.

Replace the existing rendered participant inside `renderVoiceChannel`:

```tsx
{participants.map((p) => {
  const pkStr = publicKeyToString(p.public_key);
  const speaking = activeServer?.voiceSpeakingPks.has(pkStr) ?? false;
  return (
    <div key={pkStr} className="voice-participant" style={{ display: "flex", alignItems: "center", gap: 6 }}>
      <span
        className="member-avatar-mini"
        style={{
          outline: speaking ? "2px solid #4ade80" : "none",
          transition: "outline 100ms ease",
        }}
      >
        {p.display_name.charAt(0).toUpperCase()}
      </span>
      <span>{p.display_name}</span>
    </div>
  );
})}
```

(If the existing participants render uses different prop shape, adapt — the key piece is the `voiceSpeakingPks.has(pkStr)` lookup driving the speaking ring.)

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -5; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/ChannelSidebar.tsx
git -C /home/deez/farder commit -m "feat(client): speaking ring around voice participants in ChannelSidebar"
```

---

## Task 17: VoiceControlBar + IncomingCallModal + AppShell wiring

**Files:**
- Create: `client/src/components/VoiceControlBar.tsx`
- Create: `client/src/components/IncomingCallModal.tsx`
- Create: `client/public/sounds/ringtone.wav`
- Modify: `client/src/components/AppShell.tsx`
- Modify: `client/src/components/ChannelSidebar.tsx`

- [ ] **Step 1: Create VoiceControlBar**

`client/src/components/VoiceControlBar.tsx`:

```tsx
import { useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";

interface Props {
  serverId: string;
  channelId: number;
  channelName: string;
}

const bar: CSSProperties = {
  borderTop: "1px solid var(--xp-border, #888)",
  padding: 8,
  background: "var(--xp-panel-bg, #f0ece0)",
  fontSize: 11,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
};

export default function VoiceControlBar({ serverId, channelId, channelName }: Props) {
  const [muted, setMuted] = useState(false);
  const [deafened, setDeafened] = useState(false);

  async function toggleMute() {
    const next = !muted;
    setMuted(next);
    try { await api.setVoiceMute(next); } catch {}
  }
  async function toggleDeafen() {
    const next = !deafened;
    setDeafened(next);
    try { await api.setVoiceDeafen(next); } catch {}
  }
  async function leave() {
    try { await api.stopVoice(); } catch {}
  }

  return (
    <div style={bar}>
      <div style={{ fontWeight: "bold" }}>Voice Connected</div>
      <div style={{ color: "var(--xp-text-muted, #666)" }}>~ {channelName}</div>
      <div style={{ fontSize: 10, color: "var(--xp-text-muted, #666)", marginTop: 2 }}>
        🔒 Server-relay (private)
      </div>
      <div style={{ display: "flex", gap: 4, marginTop: 6 }}>
        <button onClick={toggleMute} style={{ font: "inherit", padding: "2px 8px" }}>
          {muted ? "🔇" : "🎤"} {muted ? "Unmute" : "Mute"}
        </button>
        <button onClick={toggleDeafen} style={{ font: "inherit", padding: "2px 8px" }}>
          {deafened ? "🔇" : "🎧"} {deafened ? "Undeafen" : "Deafen"}
        </button>
        <button onClick={leave} style={{ font: "inherit", padding: "2px 8px", marginLeft: "auto", color: "#a00" }}>
          📞 Leave
        </button>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Create IncomingCallModal**

`client/src/components/IncomingCallModal.tsx`:

```tsx
import { useEffect, useRef, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";

interface Props {
  serverId: string;
  channelId: number;
  callerPk: string;
  callerName: string;
  onClose: () => void;
}

const overlay: CSSProperties = {
  position: "fixed", inset: 0, background: "rgba(0,0,0,0.6)",
  display: "flex", alignItems: "center", justifyContent: "center",
  zIndex: 3000,
};

const card: CSSProperties = {
  background: "var(--xp-window-bg, #ECE9D8)",
  color: "var(--xp-text-normal, #000)",
  border: "2px solid var(--xp-blue-dark, #003C74)",
  padding: 24, width: 320, textAlign: "center",
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
};

export default function IncomingCallModal({ serverId, channelId, callerPk: _callerPk, callerName, onClose }: Props) {
  const audioRef = useRef<HTMLAudioElement | null>(null);

  useEffect(() => {
    const a = new Audio("/sounds/ringtone.wav");
    a.loop = true;
    a.volume = 0.7;
    a.play().catch(() => {});
    audioRef.current = a;
    const timer = setTimeout(onClose, 30_000);  // auto-dismiss after 30s
    return () => {
      a.pause();
      a.src = "";
      clearTimeout(timer);
    };
  }, [onClose]);

  async function accept() {
    try { await api.startVoice(serverId, channelId); } catch (e) { console.error("[call] accept failed:", e); }
    onClose();
  }
  function decline() { onClose(); }

  return (
    <div style={overlay}>
      <div style={card}>
        <div style={{ fontSize: 14, fontWeight: "bold" }}>Incoming Call</div>
        <div style={{ fontSize: 24, margin: "12px 0" }}>📞</div>
        <div style={{ fontSize: 13 }}>{callerName}</div>
        <div style={{ fontSize: 10, color: "var(--xp-text-muted, #666)" }}>is calling…</div>
        <div style={{ display: "flex", gap: 8, justifyContent: "center", marginTop: 16 }}>
          <button onClick={decline} style={{
            font: "inherit", padding: "6px 16px",
            background: "#a00", color: "#fff", border: "1px solid #800",
          }}>Decline</button>
          <button onClick={accept} style={{
            font: "inherit", padding: "6px 16px",
            background: "#0a0", color: "#fff", border: "1px solid #060",
          }}>Accept</button>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Bundle a ringtone**

Place a short looping WAV at `client/public/sounds/ringtone.wav`. Source: download a CC0 phone-ring WAV from freesound.org or similar. Target ~3 seconds, ~50 KB. If you don't have one ready, generate a placeholder:

```
ffmpeg -f lavfi -i "sine=frequency=440:duration=0.4,sine=frequency=480:duration=0.4" -af "apad=pad_dur=2.2" client/public/sounds/ringtone.wav
```

(Generates a single ring with silence — acceptable placeholder.)

- [ ] **Step 4: Wire IncomingCallModal into AppShell**

In `client/src/components/AppShell.tsx`, after the existing KickedBannedDialog render (which already uses `useApp()` state), add:

```tsx
import IncomingCallModal from "./IncomingCallModal";
// ...
{state.voiceCallIncoming && (
  <IncomingCallModal
    serverId={state.voiceCallIncoming.serverId}
    channelId={state.voiceCallIncoming.channelId}
    callerPk={state.voiceCallIncoming.callerPk}
    callerName={state.voiceCallIncoming.callerName}
    onClose={() => dispatch({ type: "CLEAR_VOICE_CALL_INCOMING" })}
  />
)}
```

- [ ] **Step 5: Render VoiceControlBar in ChannelSidebar when in voice**

In `ChannelSidebar.tsx`, near the bottom of the rendered sidebar (after channel list, before whatever's at the bottom), add:

```tsx
{activeServer?.currentVoiceChannelId != null && (() => {
  const ch = activeServer.channels.find(c => c.id === activeServer.currentVoiceChannelId);
  if (!ch || !serverId) return null;
  return <VoiceControlBar serverId={serverId} channelId={ch.id} channelName={ch.name} />;
})()}
```

Add `import VoiceControlBar from "./VoiceControlBar";` at the top.

- [ ] **Step 6: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 7: Commit**

```
git -C /home/deez/farder add client/src/components/VoiceControlBar.tsx client/src/components/IncomingCallModal.tsx client/public/sounds/ringtone.wav client/src/components/AppShell.tsx client/src/components/ChannelSidebar.tsx
git -C /home/deez/farder commit -m "feat(client): VoiceControlBar + IncomingCallModal + AppShell/ChannelSidebar wiring"
```

---

## Task 18: MemberContextMenu Volume submenu

**Files:**
- Modify: `client/src/components/MemberContextMenu.tsx`

- [ ] **Step 1: Add Volume row**

In `MemberContextMenu.tsx`, find the rows-array builder. Add (somewhere reasonable — between View Profile and Send Message is fine):

```tsx
const targetIsInVoice = activeServer?.channels.some(
  ch => ch.channel_type === "Voice" && /* TODO: find a way to check participants */
       false  // placeholder — see note below
);
```

Volume control benefits from voice context but isn't strictly tied to it. Simpler approach: ALWAYS show the Volume submenu, since the volume preference applies whenever the user is in voice with that person. Skip the participant check.

```tsx
if (!isSelf) {
  rows.push({
    kind: "submenu",
    label: "Volume… ▶",
  });
}
```

The submenu logic in MemberContextMenu currently handles only the role submenu — extending it to handle a second submenu type adds complexity. Simpler: add the Volume submenu inline as a dedicated row that opens an inline slider on click.

Replace the simple row-push with a small popover. Add state:

```tsx
const [showVolume, setShowVolume] = useState(false);
const [volume, setVolume] = useState(1.0);
```

In the rows builder:

```tsx
if (!isSelf) {
  rows.push({
    kind: "item",
    label: "Volume…",
    onClick: () => setShowVolume(v => !v),
  });
}
```

In the rendered menu, after the cleaned rows render, before the closing `</div>`, add:

```tsx
{showVolume && (
  <div style={{
    padding: "4px 10px", borderTop: "1px solid var(--xp-border, #ccc)",
    display: "flex", flexDirection: "column", gap: 4,
  }}>
    <label style={{ fontSize: 10 }}>Volume: {Math.round(volume * 100)}%</label>
    <input
      type="range"
      min="0" max="2" step="0.05"
      value={volume}
      onChange={(e) => {
        const v = parseFloat(e.target.value);
        setVolume(v);
        api.setVoicePerUserVolume(targetPk, v).catch(() => {});
      }}
      style={{ width: "100%" }}
    />
  </div>
)}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -5; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/MemberContextMenu.tsx
git -C /home/deez/farder commit -m "feat(client): per-user volume slider in MemberContextMenu"
```

---

## Task 19: Voice settings tab in AppearanceSettings

**Files:**
- Create: `client/src/components/VoiceSettings.tsx`
- Modify: `client/src/components/AppearanceSettings.tsx`

- [ ] **Step 1: Create VoiceSettings.tsx**

`client/src/components/VoiceSettings.tsx`:

```tsx
import { useEffect, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";

const row: CSSProperties = { display: "flex", justifyContent: "space-between", alignItems: "center", padding: "8px 0", gap: 12 };

export default function VoiceSettings() {
  const [inputs, setInputs] = useState<string[]>([]);
  const [outputs, setOutputs] = useState<string[]>([]);
  const [inputDevice, setInputDevice] = useState<string>("");
  const [outputDevice, setOutputDevice] = useState<string>("");
  const [inputVolume, setInputVolume] = useState(1);
  const [outputVolume, setOutputVolume] = useState(1);
  const [usePtt, setUsePtt] = useState(false);
  const [vadThreshold, setVadThreshold] = useState(0.02);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.listAudioDevices().then(({ inputs, outputs }) => {
      setInputs(inputs); setOutputs(outputs);
    }).catch((e) => setError(String(e)));
  }, []);

  async function commitInputDevice(d: string) {
    setInputDevice(d);
    await api.setVoiceInputDevice(d || null);
  }
  async function commitOutputDevice(d: string) {
    setOutputDevice(d);
    await api.setVoiceOutputDevice(d || null);
  }

  return (
    <div style={{ padding: 12 }}>
      <h3 style={{ marginTop: 0 }}>Voice & Video</h3>
      {error && <div style={{ color: "#a00", marginBottom: 8 }}>{error}</div>}

      <div style={row}>
        <label>Input device</label>
        <select value={inputDevice} onChange={(e) => commitInputDevice(e.target.value)} style={{ font: "inherit" }}>
          <option value="">Default</option>
          {inputs.map(d => <option key={d} value={d}>{d}</option>)}
        </select>
      </div>

      <div style={row}>
        <label>Output device</label>
        <select value={outputDevice} onChange={(e) => commitOutputDevice(e.target.value)} style={{ font: "inherit" }}>
          <option value="">Default</option>
          {outputs.map(d => <option key={d} value={d}>{d}</option>)}
        </select>
      </div>

      <div style={row}>
        <label>Input volume: {Math.round(inputVolume * 100)}%</label>
        <input type="range" min="0" max="2" step="0.05" value={inputVolume}
          onChange={(e) => { const v = parseFloat(e.target.value); setInputVolume(v); api.setVoiceInputVolume(v); }}
          style={{ width: 200 }} />
      </div>
      <div style={row}>
        <label>Output volume: {Math.round(outputVolume * 100)}%</label>
        <input type="range" min="0" max="2" step="0.05" value={outputVolume}
          onChange={(e) => { const v = parseFloat(e.target.value); setOutputVolume(v); api.setVoiceOutputVolume(v); }}
          style={{ width: 200 }} />
      </div>

      <div style={row}>
        <label>Voice activation</label>
        <select value={usePtt ? "ptt" : "vad"} onChange={(e) => {
          const v = e.target.value === "ptt"; setUsePtt(v); api.setVoicePttEnabled(v);
        }} style={{ font: "inherit" }}>
          <option value="vad">Voice Activity</option>
          <option value="ptt">Push-to-Talk</option>
        </select>
      </div>

      {!usePtt && (
        <div style={row}>
          <label>VAD threshold: {(vadThreshold * 100).toFixed(1)}%</label>
          <input type="range" min="0" max="0.2" step="0.005" value={vadThreshold}
            onChange={(e) => { const v = parseFloat(e.target.value); setVadThreshold(v); api.setVoiceVadThreshold(v); }}
            style={{ width: 200 }} />
        </div>
      )}

      {usePtt && (
        <p style={{ fontSize: 10, color: "var(--xp-text-muted, #666)" }}>
          PTT key configuration deferred to v1.5 (currently uses default 'V' key, registered globally when voice starts).
        </p>
      )}

      <p style={{ fontSize: 10, color: "var(--xp-text-muted, #666)", marginTop: 16 }}>
        Voice is server-relayed (your IP is never shown to peers). Audio is encrypted in transit but decryptable on the server. For best quality use headphones — no echo cancellation in v1.
      </p>
    </div>
  );
}
```

- [ ] **Step 2: Add Voice tab to AppearanceSettings**

In `client/src/components/AppearanceSettings.tsx`:

Find the tab type (currently `"appearance" | "gif"`). Change to:

```tsx
const [activeTab, setActiveTab] = useState<"appearance" | "gif" | "voice">("appearance");
```

In the tab bar rendering (the `(["appearance", "gif"] as const).map(...)` block), change to:

```tsx
{(["appearance", "gif", "voice"] as const).map((tab) => (
  <button ... >
    {tab === "appearance" ? "Appearance" : tab === "gif" ? "GIF Search" : "Voice & Video"}
  </button>
))}
```

In the body, after `{activeTab === "gif" && <GifSearchSettings />}`, add:

```tsx
{activeTab === "voice" && <VoiceSettings />}
```

Add the import:

```tsx
import VoiceSettings from "./VoiceSettings";
```

- [ ] **Step 3: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -5; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/components/VoiceSettings.tsx client/src/components/AppearanceSettings.tsx
git -C /home/deez/farder commit -m "feat(client): Voice & Video settings tab (devices/volumes/VAD/PTT)"
```

---

## Phase 6: Smoke + CHANGELOG

## Task 20: Smoke test + CHANGELOG

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Restart and rebuild everything**

```
pkill -f farder-server
pkill -f farder-client
cd /home/deez/farder && cargo build -p farder-server && bash client/src-tauri/binaries/copy-sidecar.sh
cd /home/deez/farder/client && npm run build && cd src-tauri && cargo build --release  # rebuild Bob
cd /home/deez/farder/client && npm run tauri dev  # launch Alice
```

In a separate terminal:
```
FARDER_DATA=/tmp/farder-bob /home/deez/farder/client/src-tauri/target/release/farder-client
```

- [ ] **Step 2: Smoke checklist**

Two-client tests (Alice = owner, Bob in /tmp/farder-bob):

- [ ] Both clients connect and join the same voice channel.
- [ ] Alice clicks Voice control bar shows; "🔒 Server-relay (private)" footer visible.
- [ ] Alice speaks → her avatar gets a green ring on Bob's screen; Bob hears her audio.
- [ ] Alice's avatar gets a green ring on her own screen too.
- [ ] Self-mute on Alice → no ring, Bob hears nothing from her.
- [ ] Self-deafen on Bob → Bob hears nothing (even if Alice speaks); Alice's UI shows Bob still in the channel.
- [ ] Alice opens Bob's right-click menu → Volume… → slider; reducing it makes Alice hear Bob quieter.
- [ ] Settings → Voice & Video → input device picker populated; output device picker populated.
- [ ] Switch input device → restart voice (stop/start) → audio works on new device.
- [ ] Switch to PTT mode → no audio sent unless 'V' is held (TODO: PTT key registration in v1; if not wired, document limitation).
- [ ] VAD threshold slider visible in VAD mode; raising it suppresses faint speech.
- [ ] **DM ringing:** Alice opens a DM with Bob, clicks "Start Voice" (or however it's exposed) → Bob's screen shows the IncomingCallModal with ringtone playing. Bob accepts → both connected. Two-way audio.
- [ ] DM ringing decline: Alice rings Bob, Bob clicks Decline → modal closes; ringtone stops.
- [ ] Caller hangs up before answer: Alice rings, Alice leaves before Bob accepts → Bob's modal auto-closes (VoiceCallEnded received).
- [ ] No browser permission prompt for microphone (audio is via cpal, not getUserMedia).
- [ ] Server log doesn't contain audio data at any log level.

If any item fails, file a follow-up — don't fix in this commit.

- [ ] **Step 3: Add CHANGELOG entry**

In `CHANGELOG.md` under the most recent `### Added` block, prepend:

```
- (2026-05-07) Voice calling v1: real-time voice in DMs and voice channels. Server-relay architecture (peer IPs never exposed). Opus 32 kbps mono 20 ms frames over QUIC datagrams. Tauri-Rust audio stack — cpal capture + audiopus encode/decode + custom mixer; audio path never crosses the JS boundary. UI: voice control bar with mute/deafen/leave + "🔒 Server-relay (private)" footer; speaking indicator (green avatar ring) driven by 5 Hz server-side ticker; per-user volume slider via right-click; input/output device pickers + Voice Activity / Push-to-Talk modes in new "Voice & Video" Settings tab. DM 1:1 calls trigger an IncomingCallModal with bundled ringtone (auto-dismiss 30s); VoiceCallEnded auto-closes the modal if the caller hangs up before answer. Privacy disclaimer surfaces in the settings tab: server can decrypt audio; no AEC in v1; use headphones. P2P direct-connection toggle is v1.5; screensharing is v2; end-to-end encryption is a separate follow-up. New protocol additions: StartVoice/StopVoice/SetVoiceMute/SetVoiceDeafen requests, VoiceCallIncoming/VoiceCallEnded/VoiceSpeakingChanged events. New EventTarget variants: VoiceStartTransmit/VoiceStopTransmit/VoiceSetMute/VoiceSetDeafen for sync→async state mutation signaling. New deps: audiopus 0.3.
```

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add CHANGELOG.md
git -C /home/deez/farder commit -m "docs: changelog for voice calling v1"
```

---

## Self-review notes

**Spec coverage:**
- 4 new ServerRequest variants → Task 1
- 3 new ServerEvent variants → Task 1
- QUIC datagram enable on server + client → Task 2
- VoiceState struct → Task 3
- start_transmit / stop_transmit helpers → Task 3
- Datagram parser + builder + rewrite → Task 4
- Datagram receive loop on server → Task 5
- voice_connections map (Quinn handles) → Task 5
- Server fanout w/ deafen filtering + anti-spoof → Task 5
- StartVoice / StopVoice / SetVoiceMute / SetVoiceDeafen handlers → Task 6
- DM ringing (VoiceCallIncoming) → Task 6
- VoiceState mutation via new EventTarget variants → Task 6
- Speaking-state ticker (5Hz) → Task 7
- VoiceCallEnded on empty-DM-during-ring → Task 8
- audiopus dep + voice.rs skeleton → Task 9
- cpal input stream + AudioRing → Task 9
- Opus encode + VAD/PTT gate + datagram send → Task 10
- Datagram recv + per-speaker decoder → Task 11
- Mixer + cpal output stream → Task 12
- start() / stop() / per-knob setters → Task 12
- list_audio_devices → Task 12
- 13 Tauri commands → Task 13
- bridge.rs event emissions → Task 14
- TS bridge bindings → Task 15
- Reducer state additions → Task 15
- 3 new event listeners → Task 15
- Speaking ring on voice members → Task 16
- VoiceControlBar → Task 17
- IncomingCallModal + ringtone → Task 17
- AppShell wiring → Task 17
- Per-user volume submenu → Task 18
- Voice & Video settings tab → Task 19
- Smoke + changelog → Task 20

**Type/name consistency:**
- `VoiceConfig` shape matches between Rust commands and JS bridge (input_volume f32, vad_threshold f32, ptt_enabled bool, etc.).
- `voiceSpeakingPks: Set<string>` — used in reducer + ChannelSidebar.
- `voiceCallIncoming: { serverId, channelId, callerPk, callerName } | null` — used in reducer + AppShell + IncomingCallModal.
- Action types: `VOICE_SPEAKING_CHANGED`, `VOICE_CALL_INCOMING`, `VOICE_CALL_ENDED`, `CLEAR_VOICE_CALL_INCOMING` — consistent.
- Tauri command names: `start_voice`, `stop_voice`, `set_voice_mute`, etc. — snake_case server side, camelCase JS side, same root.

**Known compromises:**
- **Mute/deafen UI on OTHER users** (showing Bob's mute icon to Alice) requires a server broadcast event when SetVoiceMute/SetVoiceDeafen is received. Not in v1 — Task 16 documents the limitation with a TODO. Add `VoiceMuteChanged` / `VoiceDeafenChanged` events in v1.5 if needed.
- **PTT key configuration UI** is a stub — Tauri global shortcut API integration is its own ~4 hour task. v1 ships with a hardcoded 'V' key (or no PTT key registration at all if Tauri global shortcuts aren't wired). Documented as "deferred to v1.5" in the VoiceSettings tab.
- **Echo cancellation / noise suppression** — explicitly out of scope per the spec. Headphones-by-convention disclaimer surfaces in settings.
- **End-to-end encryption** — server has the keys to decrypt audio in v1. Disclaimer surfaces in settings.
- **Resampling** — encode thread asserts 48kHz; if cpal opens at a different rate, audio will be wrong-pitched. v1.5 adds rubato or speexdsp resampler. v1 just logs a warning if rate doesn't match.
- **Test coverage** — server fanout has minimal test coverage in this plan (the smoke covers the happy path). Adding integration tests for fanout, deafen-skip, anti-spoof is recommended in a follow-up if voice has churn.
- **Quinn `unsafe impl Send`** for VoiceSession — cpal::Stream is `!Send` on macOS. Linux/Windows OK. Mac support requires either a different threading approach or feature-gated `unsafe impl`.
- **No automated client-TS tests** — consistent with codebase.

**Phasing for review checkpoints (recommended):**

1. After Phase 1 (Tasks 1-5): Server can receive datagrams and the structure compiles. No actual fanout end-to-end yet.
2. After Phase 2 (Tasks 6-8): Server-side voice complete. Hand-test by sending crafted datagrams via a test client.
3. After Phase 3 (Tasks 9-12): Audio engine compiles and can be unit-tested without the network.
4. After Phase 4 (Tasks 13-14): Tauri commands callable from JS console for hand-testing.
5. After Phase 5 (Tasks 15-19): Full UI integrated.
6. Phase 6: Smoke + ship.
