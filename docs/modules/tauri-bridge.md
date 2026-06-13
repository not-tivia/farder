# Tauri bridge (server ↔ frontend)

> **File(s):** `client/src-tauri/src/bridge.rs`
> **Layer:** Tauri bridge
> **Last reviewed:** 2026-06-04

## Purpose

`bridge.rs` is the seam between the `farder-server` (over QUIC) and the React
UI. It does two things: (1) **`send_request`** sends a `ServerRequest` to a
connected server and awaits its `ServerResponse` (used by almost every
`#[tauri::command]`); (2) **`dispatch_event`** receives each `ServerEvent`
pushed by a server and either re-emits it to the webview as a `server:*` Tauri
event, or routes it into the local `VoiceController`. It is the single source of
truth for the server-event → UI-event mapping.

---

## Public interface

### `send_request(state, server_id, request) -> Result<ServerResponse>`

**What it does:** looks up the connected server by `server_id`, registers a
pending-response slot keyed by a request id, sends the `ServerRequest` over the
QUIC connection, and awaits the matching `ServerResponse`.
**Returns:** the `ServerResponse`, or an error if the server isn't connected /
the response channel closed.
**Side effects:** inserts/removes an entry in the connection's
`pending_requests` map; network I/O.
**Connects to:** called by ~all command handlers in `commands.rs` (e.g.
`get_members`, `send_message`, `join_voice`). The `pending_requests` map is a
`std::sync::Mutex` — the lock is held only for the insert/remove, never across
an `.await`.

### `dispatch_event(app, server_id, event)`

**What it does:** matches a `ServerEvent` and emits the corresponding `server:*`
Tauri event (with `server_id` added to the payload), OR — for voice media
events — spawns a task that calls the `VoiceController`. Unmapped variants fall
through to `Ok(())` (no-op).
**Side effects:** emits a webview event, or mutates `VoiceController` state.
**Connects to:** the per-server event-reader loop (top of `bridge.rs`); the
frontend listeners in `client/src/hooks/useServerEvents.ts`; the
`VoiceController` in `voice/mod.rs`.

---

## Events emitted (the catalog)

Every row is a contract: a server event → a Tauri event name → a JSON payload →
a frontend listener. Public keys are emitted as their `.to_string()` form
(`"vk_<hex>"`), which matches `publicKeyToString()` on the TS side.

| `ServerEvent` | Tauri event | Payload (besides `server_id`) | Frontend listener |
|---|---|---|---|
| `NewMessage` | `server:new_message` | `message` | `useServerEvents` → `NEW_MESSAGE` |
| `MessageEdited` | `server:message_edited` | `message_id, channel_id, new_content, edited_at` | `MESSAGE_EDITED` |
| `MessageDeleted` | `server:message_deleted` | `message_id, channel_id` | `MESSAGE_DELETED` |
| `ReactionAdded` | `server:reaction_added` | `message_id, channel_id, emoji, public_key, file_id` | `REACTION_ADDED` |
| `ReactionRemoved` | `server:reaction_removed` | (same) | `REACTION_REMOVED` |
| `MemberJoined` | `server:member_joined` | `public_key, display_name` | `getMembers` refresh |
| `MemberLeft` | `server:member_left` | `public_key` | `MEMBER_LEFT` |
| `MemberBanned` / `MemberUnbanned` | `server:member_banned` / `_unbanned` | `public_key[, reason]` | banned-list refresh |
| `MemberTimeoutChanged` | `server:member_timeout_changed` | `public_key, until_ms, reason` | `MEMBER_TIMEOUT_CHANGED` |
| `YouWereKicked` / `YouWereBanned` | `server:you_were_kicked` / `_banned` | `[reason]` | `YOU_WERE_KICKED` / `_BANNED` |
| `AuditEventCreated` | `server:audit_event_created` | `event` | audit log tab |
| `ChannelCreated/Updated/Deleted` | `server:channel_*` | `channel` / `channel_id` | channel reducers |
| `CategoryCreated/Updated/Deleted` | `server:category_*` | `category` / `category_id` | category reducers |
| `RoleCreated/Updated/Deleted` | `server:role_*` | `role` / `role_id` | role reducers |
| `TypingStarted` | `server:typing` | `channel_id, public_key` | `TYPING_STARTED` (8s expiry) |
| `DmCreated` | `server:dm_created` | `channel, participant` | DM list |
| `MemberProfileUpdated` | `server:member_profile_updated` | `public_key, profile_hash` | `useServerEvents` → `getMembers` refresh (roster re-fetch so new `profile_hash` propagates to all member list consumers) |
| `MediaJoined` | `server:voice_joined` | `channel_id, public_key, display_name` | `VOICE_JOINED` (roster) |
| `MediaLeft` | `server:voice_left` | `channel_id, public_key` | `VOICE_LEFT` (roster) |
| `StreamCallIncoming` / `StreamCallEnded` | — (no-op) | — | DM-call signaling; no UI yet |

## Local events (not from the server — emitted directly by Tauri commands)

These events are emitted by Tauri commands (not routed through `dispatch_event`)
and consumed by frontend components rather than `useServerEvents.ts`.

| Event name | Emitted by | Payload | Consumer |
|---|---|---|---|
| `"screenshare:frame"` | `start_screenshare_preview` encode thread (`screenshare.rs`) | `{ data: string, key: boolean, ts: number }` | `ScreensharePreview.tsx` → WebCodecs `VideoDecoder` → `<canvas>` |

**`screenshare:frame` payload fields:**
- `data` — Base64-encoded Annex-B H.264 frame. Annex-B means start-code-prefixed NALs (`0x00 0x00 0x00 0x01`), with SPS/PPS inline before each IDR. The WebCodecs `VideoDecoder` must be configured WITHOUT a `description` to accept Annex-B input.
- `key` — `true` if the frame is an IDR or I keyframe. The consumer must not decode delta frames (`key: false`) until the first keyframe has been received.
- `ts` — Capture timestamp in **milliseconds** (monotonic since capture started). The WebCodecs API expects `EncodedVideoChunk.timestamp` in **microseconds**; multiply by 1000 before passing to the decoder.

---

## Events routed to the VoiceController (not emitted to the UI)

These spawn a task that calls a `VoiceController` method instead of emitting a
webview event. The controller then emits its own `voice://*` events.

| `ServerEvent` | Controller call | Effect |
|---|---|---|
| `StreamKeyOffer` | `on_stream_key_offer(session_id, sender, wrapped_key)` | stash the peer's wrapped per-call key |
| `TrackEnabled` (Audio) | `on_peer_track_enabled(session_id, pk, kind)` | register the peer's audio ring (pk looked up via `peer_pubkey_for`) |
| `TrackDisabled` (Audio) | `on_peer_track_disabled(session_id)` | drop the peer's ring |
| `StreamLeft` | `on_peer_stream_left(session_id)` | remove the peer |
| `TrackActivityChanged` | `on_peer_activity(session_id, kind, active)` | speaking indicator |
| `StreamStateChanged` | `on_peer_stream_state(session_id, muted, deafened)` | peer mute/deafen |
| `StreamJoined` | `on_peer_stream_joined(session_id, muted, deafened)` | seed a late-registered peer's mute/deafen |

## Integration map

- **`commands.rs`** — calls `send_request` for the request/response direction.
- **`useServerEvents.ts`** — registers a `listen("server:*", …)` for each event
  above and dispatches into `ServerContext`. If an event isn't emitted here, the
  listener is dead code.
- **`voice/mod.rs`** (`VoiceController`) — receives the media events.

## Known gotchas

- **Silent drops:** a `ServerEvent` matched to `=> Ok(())` (or caught by the
  final `_ => Ok(())`) is silently discarded — the frontend never hears it. This
  is exactly how the voice roster broke (`MediaJoined`/`MediaLeft` were dropped).
  When you add a server event the UI needs, you MUST add an `emit` arm here AND a
  `listen` in `useServerEvents.ts` AND a reducer case in `ServerContext`.
- **Public-key form:** emit pubkeys via `public_key.to_string()` so they match
  the `getVoiceState`/member snapshots the UI already stores (`"vk_<hex>"`).
  Emitting the raw serde `{bytes}` object would break the UI's string compares.
- **`pending_requests` is a `std::sync::Mutex`:** never hold its guard across an
  `.await`, or you'll deadlock the event-reader task.
