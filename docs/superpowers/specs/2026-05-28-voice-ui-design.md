# Voice UI — Design Spec

**Date:** 2026-05-28
**Status:** Approved (brainstorming)
**Depends on:** Voice Client Pipeline (`docs/superpowers/plans/2026-05-26-voice-client-pipeline.md`) — complete.

## Goal

Make voice calling usable from the UI. The audio engine (`VoiceController` + send/recv/mixer pipeline) is built and exposed via Tauri commands, but nothing in the UI drives it: clicking a voice channel only updates server-side *presence*, never starts audio, and there are no mute/deafen controls or speaking indicators.

This project wires the channel sidebar to the real audio engine, adds an in-call control bar, and shows live speaking + mute/deafen state for everyone in the call. It also adds the one missing backend piece — broadcasting a participant's mute/deafen state to the channel — so the indicators can be trusted for *other* people, not just yourself.

## Non-goals (deferred — each needs larger backend work)

- **Per-user volume** sliders
- **Mic / speaker device pickers**
- **Push-to-talk** (engine currently hardcodes open-mic via `GateMode::Open`)
- **DM call "ringing"** / incoming-call modal
- **Mute/deafen icons for non-participants** — someone merely *viewing* a voice channel without joining will not see speaking rings or mute/deafen icons. These render while you are in the call. Surfacing them on the always-visible roster for non-participants is deferred.

## Background: two layers, only one wired

The client has two parallel voice systems:

1. **Presence layer (older).** `joinVoice` / `leaveVoice` / `getVoiceState` Tauri commands + `server:voice_joined` events. The server tracks who is "in" a voice channel and broadcasts it to all channel viewers. `ChannelSidebar.renderVoiceChannel` uses this today to show a participant list under each voice channel. **This is the only thing wired to the UI, and it carries no audio.**

2. **Audio layer (new, unwired).** `VoiceController` driven by `voiceJoin` / `voiceLeave` / `voiceSetMute` / `voiceSetDeafen` / `voiceGetState` commands, emitting `voice://state-changed`, `voice://local-speaking`, `voice://peer-speaking`. This is the real pipeline: per-call key exchange, Opus, encrypted QUIC datagrams, mixing, playback. **No UI calls it.**

The audio layer is **end-to-end encrypted**: the per-call stream key is derived locally and wrapped per-peer (`wrap_stream_key_for_peer`), distributed via `OfferStreamKey`. The server only fans out sealed datagrams and "NEVER decrypts" them (`crates/farder-server/src/media_stream.rs`), and the relayed frame header does not contain the speaker's pubkey (sealed-sender invariant). This is stronger than the original Voice Calling v1 spec assumed (which said the server could decrypt), and the UI's privacy copy reflects the real guarantee.

## UX design

### Joining / leaving a voice channel

Clicking a voice channel in the sidebar:
- If not in it: calls the presence join (`joinVoice`, so all channel viewers see you in the roster) **and** `voiceJoin(serverId, channelId)` (starts the audio pipeline).
- If already in it: calls `voiceLeave()` then the presence leave (`leaveVoice`).

The disconnect button in the control bar does the same leave path. Joining a different voice channel while already in one auto-leaves the previous call (the controller already supports this — `double_join_auto_leaves_previous`).

### Participant list — ring style

Each person in a voice channel renders as a small circular **initial-avatar** (first letter of display name). Display names are resolved by matching each peer's public key against the server's existing member list (already loaded in `ServerContext`); no new backend lookup.

State shown per participant (live, while you are in the call):
- **Speaking:** a green glowing ring around the avatar (`box-shadow: 0 0 0 2px <green>`) and a brightened name. Driven by `voice://peer-speaking` (peers) and `voice://local-speaking` (you).
- **Muted:** a mic-off icon and a dimmed name.
- **Deafened:** a headphones-off icon (implies muted).

### In-call control bar (style C) — bottom of channel sidebar

Replaces the current thin `voice-status-bar`. Appears only while in a call, just above the user footer:

```
● Voice Connected · <channel name>
[Y] You                    speaking
[ 🎤 Mute ] [ 🎧 Deafen ] [ ✖ ]
🔒 End-to-end encrypted
```

- A "Voice Connected · <channel>" status line (green).
- A **self-preview** row: your avatar with a live speaking ring + your name, so you can confirm your mic is working.
- Three buttons: **Mute** (your mic), **Deafen** (silences all incoming + auto-mutes you), **Disconnect** (✖). Mute and Deafen render in a red/active style while engaged.
- An always-on **🔒 End-to-end encrypted** footer for the duration of the call.

Mute/deafen interactions call `voiceSetMute` / `voiceSetDeafen`. The bar's displayed state is driven by `voice://state-changed` (not local optimistic state), so it stays correct if state changes elsewhere. Deafen auto-mutes and restores the prior mute state on un-deafen (controller already implements `pre_deafen_muted`).

## Backend addition: broadcast peer mute/deafen

Today `SetDeafen` updates server-side state silently and there is no `SetMute` request and no broadcast — so other clients cannot know a peer's mute/deafen state. The fix mirrors the existing `EnableTrack` → `TrackEnabled` broadcast pattern (`crates/farder-server/src/handlers.rs`).

**Protocol (`crates/farder-protocol/src/server.rs`):**
- Add `ServerRequest::SetMute { muted: bool }` (alongside existing `SetDeafen`).
- Add `ServerEvent::StreamStateChanged { channel_id, session_id, muted, deafened }`.
- Add `muted: bool` and `deafened: bool` fields to `ServerEvent::StreamJoined` so a client joining a call sees existing participants' current state immediately.

**Server (`handlers.rs`, `media_stream.rs`):**
- Track per-session `muted` (mirror the existing `deafened` set on `StreamState`, or store both as fields on `ServerSession`).
- `SetMute` and `SetDeafen` handlers update the stored flag(s) and broadcast `StreamStateChanged { ..., muted, deafened }` to `EventTarget::All` for the channel — exactly like `EnableTrack` broadcasts `TrackEnabled`.
- Populate the new `muted`/`deafened` fields when emitting `StreamJoined`.

**Client controller (`client/src-tauri/src/voice/mod.rs`):**
- `VoicePeer` gains `muted: bool` and `deafened: bool` (currently `{ pubkey, speaking }`).
- `set_mute` / `set_deafen` additionally send `SetMute` / `SetDeafen` to the server via the retained server-session handle (today `set_mute` only flips a local atomic and emits). The local atomics still gate the send/recv tasks; the server notification exists so the change can be relayed.
- New handler `on_peer_stream_state(session_id, muted, deafened)`: update the matching peer in `VoiceState.peers`, then emit the usual `voice://state-changed`. The inbound `StreamStateChanged` server event is routed to this handler in the connection/bridge event loop, alongside the existing `TrackActivityChanged` → `on_peer_activity` routing.
- Apply `StreamJoined`'s `muted`/`deafened` when registering a peer.

**Client bridge (`client/src/lib/tauri-bridge.ts`):**
- `VoicePeer` interface gains `muted` and `deafened`.
- No new TS event listener needed for mute/deafen: the controller absorbs `StreamStateChanged` and re-emits `voice://state-changed`, which the UI already consumes.

## Frontend architecture

A small **`useVoice` hook** (`client/src/hooks/useVoice.ts`) owns all voice-call UI state:
- Subscribes to `voice://state-changed`, `voice://local-speaking`, `voice://peer-speaking` (via `@tauri-apps/api/event` `listen`, following the cleanup pattern in `useServerEvents.ts`).
- Exposes `{ state: VoiceState, localSpeaking: boolean, join, leave, setMute, setDeafen }`.
- Maps `peer.pubkey` → display name via the active server's member list.

Components:
- **`VoiceControlBar`** (`client/src/components/VoiceControlBar.tsx`): style-C bar rendered at the bottom of `ChannelSidebar` when `state.channel_id` is set. Reads from `useVoice`.
- **`ChannelSidebar` changes:** voice-channel click handler also calls `useVoice().join` / `leave`; `renderVoiceChannel`'s participant list gains avatar + ring + mute/deafen icons, merging the speaking/mute/deafen state from `useVoice` onto the roster (matched by pubkey). The existing `voice-status-bar` block is replaced by `VoiceControlBar`.

Speaking indicator state can update at a high rate; the hook debounces re-renders only if needed (start simple — the controller's speaking events are already edge-triggered on/off, not per-frame).

## Styling

New/updated CSS in each theme (`client/src/themes/*/theme.css`), following the existing `.voice-*` classes:
- `.voice-avatar` (circular initial), `.voice-avatar.speaking` (green ring).
- `.voice-control-bar` and children (header, self-preview, button row, `.voice-e2e-footer`).
- Mute/deafen button active state; mic-off / headphones-off status icons.

Avatars and ring color use theme tokens so the three shipped themes (discord-dark, hello-kitty, xp-luna-blue) each render coherently.

## Testing

- **Backend (Rust):** unit tests in `handlers.rs` mirroring the existing `EnableTrack` test — `SetMute` / `SetDeafen` produce a `StreamStateChanged` broadcast to All with correct flags; `StreamJoined` carries current mute/deafen.
- **Controller (Rust):** extend `voice::controller_tests` — `set_mute` now sends to the (fake) server and emits state; `on_peer_stream_state` updates the right peer and emits `voice://state-changed`; `StreamJoined` with muted=true registers a muted peer.
- **Frontend:** `useVoice` hook test (mock the three events → assert exposed state, including pubkey→name mapping and peer mute/deafen). Component render test for `VoiceControlBar` (mute/deafen active styles; disconnect calls leave). Manual end-to-end check (`/run` or two clients) that two participants hear each other, rings light on speech, and a mute on one shows on the other.

## Decisions captured from brainstorming

- **Scope:** core set only — join/leave, mute, deafen, speaking indicators — **plus** peer mute/deafen broadcast (folded in at user request). Volume / device pickers / PTT / DM ringing deferred.
- **Control bar:** style C (self-preview panel) at the bottom of the channel sidebar.
- **Indicators:** ring-around-avatar style.
- **Privacy:** always-on "End-to-end encrypted" footer (no first-call modal this round).
