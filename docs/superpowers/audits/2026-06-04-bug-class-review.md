# Bug-Class Review — 2026-06-04

Project-wide read-only audit triggered after a session of voice bugs. Five
parallel agents each hunted one bug class we'd actually hit. Findings below are
deduplicated and prioritized; where 2+ agents independently found the same
issue it's noted (strong signal).

Status legend: **OPEN** | **FIXING** | **FIXED** (this session).

---

## CRITICAL

### C1. Live voice events silently dropped — roster never updates **[FIXED]**
`client/src-tauri/src/bridge.rs:194`. The server broadcasts `MediaJoined` /
`MediaLeft` when anyone joins/leaves a voice channel, but `dispatch_event`
matched them to `=> Ok(())` and emitted nothing. So the frontend listeners
(`useServerEvents.ts:353-370` → `VOICE_JOINED`/`VOICE_LEFT` reducers) were dead
code, and `voiceStates` (the participant list) only ever held the snapshot from
when *you* joined. Root cause of: stale roster, **others not seeing you leave**,
and part of the **missing mute/deafen icons** (peers not tracked live).
*Found independently by the state-sync and identifier agents.* Fix: emit
`server:voice_joined` / `server:voice_left` (public_key as `.to_string()`).

### C2. Optimistic voice-join + control-bar double-gate = silent zombie state **[FIXED]**
`ChannelSidebar.tsx:362-364` dispatches `JOIN_VOICE_CHANNEL` *before* awaiting
`voice.join`, and the failure path only `console.error`s — no rollback, no
`LEAVE_VOICE_CHANNEL`. The control bar renders only when BOTH
`currentVoiceChannelId != null` (set optimistically) AND `voice.inCall`
(`VoiceControlBar.tsx:23`) are true. So if the audio engine fails, the user is
marked "in the channel" (on others' rosters) but the bar returns `null` — a
blank slot, no controls, no error. *Found by 2 agents.* Fix: commit join UI
state only after `voice.join` resolves; on failure, roll back + surface an error.

### C3. Unban is always broken (identifier mismatch) **[FIXED]**
`crates/farder-protocol/src/server.rs:153` serializes `BannedMember.public_key`
as serde object `{bytes:[…]}`, but `client/src/lib/types.ts:110` types it as
`string`. `BannedMembersTab.tsx:50` passes the object to `unbanMember` → arrives
as `"[object Object]"` → `parse_public_key` rejects it. React `key` also breaks.
Fix: TS should `publicKeyToString(entry.public_key)` and correct the type.

### C4. Audio backend still rejects non-F32 devices **[FIXED]**
`client/src-tauri/src/audio_cpal.rs` `choose_stream_config` filters to F32
configs only. A device exposing only I16/I32 (some WASAPI/ALSA drivers) yields
"no supported config" and the same silent join failure we just fixed for
rate/channels. The user's device offered F32 so it works, but this is the next
device that will fail. Fix: build an i16/i32 stream and convert, or widen format
handling.

### C5. Screen-recording repeats the cpal/thread bug we just fixed **[FIXED]**
`client/src-tauri/src/commands.rs:1797-1844` (`start_recording`): builds a cpal
stream inside `spawn_blocking`, with `.unwrap()` on `WavWriter::create`,
`build_input_stream`, and `stream.play()` (silent worker-thread panic on any
failure), plus `stop_recording` does `std::thread::sleep(500ms)` in a sync Tauri
command. Same class as the voice `SendWrapper`/unwrap issues. Fix: dedicated
thread + propagate errors instead of unwrap.

---

## IMPORTANT

### I1. Reconnect / connection-loss / kick-ban don't tear down voice **[FIXED]**
- `AppShell.tsx` reconnect path doesn't clear `currentVoiceChannelId` or stop the
  audio engine, so after a reconnect you appear "in" a call over a dead session.
- `CONNECTION_LOST` (`ServerContext.tsx:140`) preserves the voice roster;
  `DISCONNECTED` (which clears it) is **never dispatched** — dead code.
- Kick/ban (`AppShell.tsx:121-134`) disconnects but never calls `voice.leave()` —
  the mic can stay live and you stay on the roster.

### I2. Pervasive silent error-swallowing on user actions **[PARTIAL]**
(Fixed the worst: message edit/delete, create-thread, reactions now surface
errors. Still open: channel/server switch blank-on-failure, `subscribeChannels`,
`getMembers`-on-join, voice settings persist.)
Operations that fail with zero feedback (`try{}catch{}` / `.catch(()=>{})`):
- Edit message (`Message.tsx:252` closes editor as if saved), delete message &
  create thread (`Message.tsx:450-458`), reactions (`Message.tsx:203-230`).
- Channel switch (`ChannelSidebar.tsx:318` — blank channel on history-fetch
  failure), server switch (`ServerStrip.tsx:27`), channel/category move+delete.
- `subscribeChannels` (`AppShell.tsx:70` — channel silently stops getting
  messages), `getMembers` on member-join (stale member list).
- `setVoiceMode`/`setPttKey` (`VoiceSettings.tsx:19,31`) — wrong mic mode applied
  silently.
Fix incrementally: surface a toast/inline error on the ones that matter.

### I3. `std::Mutex` `.unwrap()` poisoning hazards **[FIXED]**
(server_manager child-process locks now recover from poison; the `bridge.rs`
`pending_requests` locks are not held across `.await` so are lower risk.)
- `server_manager.rs:30,38,184` — `children` mutex `.unwrap()`, including in the
  app-exit `stop_all` hook → a poisoned mutex panics at exit and orphans
  farder-server child processes (they then fight over the SQLite DB).
- `bridge.rs:17,49` — `pending_requests` mutex `.unwrap()` in the event-reader
  loop; a poison deadlocks all pending requests.

### I4. Stereo-capture left-channel-only path (conditional) **[OPEN/LOW]**
`audio_cpal.rs` capture takes `raw[i]` when `dev_channels == want_channels`.
Since `want_channels` is always 1 (mono), this only triggers for a mono device
and is correct today. Flagged as a latent trap if the engine ever requests
stereo. (One agent rated this Important assuming want_channels could be 2; it
can't currently — downgraded.)

### I5. Opus frame-size assumptions at non-integer resample ratios **[OPEN/LOW]**
`voice/send.rs` drops chunks whose length != 960. At exotic device rates (e.g.
44.1kHz-only) the resampler's per-callback output may not align to 960 cleanly,
risking silent frame drops. Standard rates (48/96k) are fine.

---

## MINOR
- Empty `display_name` → blank avatar initials (`charAt(0)` unguarded in
  `ChannelSidebar.tsx:492`, `MemberSidebar.tsx:67`, `Message.tsx:278`, etc.).
- Control bar shows literal "Voice" if the channel is deleted while you're in it
  (`ChannelSidebar.tsx:503`).
- `selfInitial={"Y"}` hardcoded placeholder in the control bar
  (`ChannelSidebar.tsx:504`) — doesn't use your real initial.
- `CHANNEL_DELETED` leaks the stale `voiceStates[channelId]` entry
  (`ServerContext.tsx:264`) — invisible (row gone) but accumulates.
- AEC render reference only fed when buffer length == 960 (`send.rs:66`) — a
  trap for the future WebRTC APM, not a current bug.
- `TauriEmitter::emit` / `bridge.rs:64` drop emit errors during webview teardown
  — benign except a late `voice://state-changed` can be lost.

---

## Confirmed OK (checked, not bugs)
- Public-key string form is consistent everywhere **except** `BannedMember`
  (C3): the bridge always `.to_string()`s before emitting and the frontend uses
  `publicKeyToString({bytes})` on serde structs — both yield `vk_<hex>`.
- Per-peer volume map key agrees across TS, Rust controller, and settings file
  (`vk_<hex>`).
- `voice://peer-speaking` pubkey matches `VoiceUiPeer.pubkey` (both `vk_<hex>`).
- `SendWrapper` is fully removed from the codebase (only a doc comment remains).
- `peer_rings` / ring / AEC `std::Mutex` locks are not held across `.await`
  (safe today; latent if a future `.await` is added inside the critical section).
