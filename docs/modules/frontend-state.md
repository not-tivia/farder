# Frontend state layer (ServerContext + useServerEvents + useVoice)

> **File(s):** `client/src/context/ServerContext.tsx`, `client/src/hooks/useServerEvents.ts`, `client/src/hooks/useVoice.ts`
> **Layer:** Frontend context / Frontend hook
> **Last reviewed:** 2026-06-14

## Purpose

These three files together are the entire client-side state layer. `ServerContext.tsx` owns the React context and reducer that holds state for every connected server. `useServerEvents.ts` subscribes to every `server:*` Tauri event emitted by `bridge.rs` and translates them into reducer actions. `useVoice.ts` is the hook the UI uses to read and control voice calls; it subscribes to the separate `voice://*` family of events emitted by the `VoiceController`. None of the three files talks directly to the Tauri command layer — they only receive events and dispatch actions (or, in `useVoice`, call `api.*` functions from `tauri-bridge.ts`).

---

## State it owns

### `AppState` (top-level context value)

| Field | Type | What it tracks |
|---|---|---|
| `hasIdentity` | `boolean` | Whether a local identity (keypair) exists; set once on startup |
| `activeServerId` | `string \| null` | Which server the UI is currently showing |
| `serverList` | `ServerListEntry[]` | Ordered list used to render the server sidebar (name, connected flag, unread count) |
| `servers` | `Record<string, PerServerState>` | Full state keyed by server id |
| `kickedBanned` | `{ kind, serverId, reason } \| null` | Set when the local user is kicked or banned; consumed by the modal and cleared with `CLEAR_KICKED_BANNED` |

### `PerServerState` (one entry per connected server)

| Field | Type | What it tracks, when it's mutated |
|---|---|---|
| `serverName` | `string` | Display name; set on connect/refresh |
| `connected` | `boolean` | Whether the QUIC connection is up |
| `connectionLost` | `boolean` | True if the connection dropped unexpectedly (triggers reconnect UI) |
| `channels` | `ChannelInfo[]` | All text/voice channels; replaced on connect, patched by channel events. Each entry carries an optional Rung-2 `class` (`"Plaintext"` \| `"E2ee"`) flattened from `getServerInfoV2`; absent = plaintext |
| `categories` | `CategoryInfo[]` | Channel categories; same lifecycle as `channels` |
| `roles` | `RoleInfo[]` | Server roles; same lifecycle |
| `members` | `MemberInfo[]` | Full member list; re-fetched on `member_joined`, patched by `MEMBER_LEFT` / `MEMBER_TIMEOUT_CHANGED` |
| `currentChannelId` | `number \| null` | The text channel the user is viewing; set by `SELECT_CHANNEL` |
| `messages` | `Record<number, MessageInfo[]>` | Messages keyed by channel id; populated lazily as channels are opened. Rung-2 rows may carry optional `is_e2ee` / `sealed` / `event_hash` (a sealed row has empty `content` and ciphertext in `sealed`) |
| `threadChannelId` | `number \| null` | Threadpane channel; set by `VIEW_THREAD` |
| `readState` | `Record<number, number>` | Last-read message id per channel; updated on channel select and explicit `MARK_READ` |
| `dms` | `DmEntry[]` | Direct-message entries (channel + participant); set by `SET_DMS` / `DM_CREATED` |
| `dmPanelChannelId` | `number \| null` | Which DM is open in the side panel |
| `typingUsers` | `Record<number, { publicKey, displayName, expiresAt }[]>` | Active typers per channel; entries expire after 8 s |
| `voiceStates` | `Record<number, { publicKey, displayName }[]>` | **Roster** of who is in each voice channel, as reported by the server. Updated by `VOICE_JOINED` / `VOICE_LEFT` / `SET_VOICE_STATE`. This is NOT the audio state — see gotchas. |
| `currentVoiceChannelId` | `number \| null` | Which voice channel the local user has joined. Set by `JOIN_VOICE_CHANNEL`, cleared by `LEAVE_VOICE_CHANNEL`. |
| `ownerPublicKey` | `string \| null` | Server owner's pubkey in `"vk_<hex>"` form |
| `highlightMessageId` | `number \| null` | Message to scroll-to-and-highlight; cleared by the UI after use |
| `polls` | `Record<number, { poll: PollInfo; myVote: number \| null }>` | Live poll state keyed by poll id. Populated lazily by `PollWidget` (`POLL_STATE` after `getPoll` — also refreshed by the widget's `refetch="mount"/"interval"` prop when rendered as a linked card), patched by `POLL_UPDATED` broadcasts and `POLL_MY_VOTE` |
| `giveaways` | `Record<number, { giveaway: GiveawayInfo; myEntered: boolean }>` | Live giveaway state keyed by giveaway id. Populated lazily by `GiveawayWidget` (`GIVEAWAY_STATE` after `getGiveaway` — also refreshed by the widget's `refetch="mount"/"interval"` prop when rendered as a linked card), patched by `GIVEAWAY_UPDATED` broadcasts and `GIVEAWAY_MY_ENTERED` |
| `events` | `Record<number, { event: EventInfo; myRsvp: string \| null }>` | Live event-card state keyed by event id. Populated lazily by `EventWidget` (`EVENT_STATE` after `getEvent` — also refreshed by the widget's `refetch="mount"/"interval"` prop when rendered as a linked card), patched by `EVENT_UPDATED` broadcasts and `EVENT_MY_RSVP`. `myRsvp` is `"going"`/`"maybe"`/`"no"`/`null` and is **self-only** — it never arrives in a broadcast |
| `activeWidgets` | `{ channelId: number; polls: number[]; giveaways: number[]; events: number[] } \| null` | The viewed channel's open-widget **id lists** for the active-widgets bar (`ActiveWidgetsBar.tsx`); the infos are upserted into `polls`/`giveaways`/`events` so chips share one source of truth with the widgets. Replaced whole by `ACTIVE_WIDGETS` (fetched via `listActiveWidgets` on channel switch/reconnect), maintained live by `POLL_UPDATED`/`GIVEAWAY_UPDATED`/`EVENT_UPDATED` (append on open/upcoming-and-missing, remove on closed/ended/cancelled/started, 20 cap **combined across all three lists**). `null` until the first fetch |
| `mlsControlEvents` | `MlsControlEventInfo[]` | Pending MLS control events (KeyPackage / Commit / Welcome / LeafConfirmed / GroupReset) received via `server:mls_control_event`. Record-only in T4 — the steward (T9, 4b-2) drains the queue; deduplicated by `eventHash` |
| `sealedDecrypts` | `Record<number, SealedDecryptEntry>` | Per-message decrypt results for sealed rows, keyed by message id (T10, D2/D4). Each sealed row is decrypted **exactly once** (the ratchet is consumed on open) and the result cached here: `{ kind: "decrypted", content, eventHash }` or `{ kind: "undecryptable", reason, eventHash }`. `eventHash` is the opened ciphertext's log event hash so a sealed edit (same id, new ciphertext) triggers a fresh decrypt. Absent = not yet decrypted (the T5 placeholder renders) |

---

## Public interface

### `AppProvider` / `ServerProvider`

**What it does:** wraps the component tree with the `AppContext` value (state + dispatch). `ServerProvider` is an alias kept for backward compatibility.
**Side effects:** initializes `useReducer` with `initialAppState`.
**Connects to:** every hook and component that calls `useApp()`, `useServer()`, `useActiveServer()`, or `useActiveServerId()`.

### `useApp() / useServer()`

**What it does:** returns `{ state: AppState, dispatch }`. `useServer()` is an alias.
**Returns:** throws if called outside `AppProvider`.

### `useActiveServer()`

**What it does:** convenience selector — returns the `PerServerState` for `activeServerId`, or `null` if none.

### `useActiveServerId()`

**What it does:** returns the `activeServerId` string (or `null`) without subscribing to the full state.

---

## Reducer action catalog

All per-server actions carry a `serverId: string` field and are routed by `appReducer` to the matching `PerServerState` slice. App-level actions mutate `AppState` directly.

### App-level actions

| Action type | What it mutates |
|---|---|
| `SET_IDENTITY` | Sets `hasIdentity = true` |
| `SERVER_ADDED` | Upserts a `ServerListEntry` and seeds or replaces the `PerServerState` for that server with data from a `ConnectResult` |
| `SERVER_REMOVED` | Removes the server from `serverList` and `servers`; advances `activeServerId` to the next server if the removed one was active |
| `SET_ACTIVE_SERVER` | Updates `activeServerId` |
| `UPDATE_SERVER_LIST` | Replaces the entire `serverList` array (used for ordering changes) |
| `INCREMENT_UNREAD` | Increments `unreadCount` on the matching `ServerListEntry` |
| `CLEAR_UNREAD` | Resets `unreadCount` and `hasMention` on the matching entry |
| `YOU_WERE_KICKED` | Sets `kickedBanned = { kind: "kick", serverId, reason: null }` on `AppState` |
| `YOU_WERE_BANNED` | Sets `kickedBanned = { kind: "ban", serverId, reason }` on `AppState` |
| `CLEAR_KICKED_BANNED` | Clears `kickedBanned` |

### Connection actions (per-server)

| Action type | What it mutates |
|---|---|
| `CONNECTED` / `SERVER_REFRESHED` | Replaces channels, categories, roles, ownerPublicKey; sets `connected = true`, `connectionLost = false`. Also syncs the `ServerListEntry` name/connected flag. Payload is a `ServerInfoV2` (v2 channel list carries `class`); `SERVER_ADDED` stays on `ConnectResult` and defaults class to plaintext. |
| `DISCONNECTED` | Resets the server slice to `initialPerServerState` with `connected = false` |
| `CONNECTION_LOST` | Sets `connected = false, connectionLost = true`; marks the `ServerListEntry` disconnected |
| `RECONNECTED` | Sets `connected = true, connectionLost = false`; marks the `ServerListEntry` connected |

### Member actions (per-server)

| Action type | What it mutates |
|---|---|
| `SET_MEMBERS` | Replaces `members` array wholesale |
| `MEMBER_JOINED` | Appends a `MemberInfo` to `members` (note: `useServerEvents` refetches the full list instead, so this action is not currently dispatched by the event hook) |
| `MEMBER_LEFT` | Removes the member matching `publicKey` from `members` |
| `MEMBER_TIMEOUT_CHANGED` | Patches `timeout_until` and `timeout_reason` on the matching member in-place |

### Message actions (per-server)

| Action type | What it mutates |
|---|---|
| `SET_MESSAGES` | Replaces the message array for one channel |
| `PREPEND_MESSAGES` | Prepends older messages to the front of a channel's array (pagination / history load) |
| `NEW_MESSAGE` | Appends one message; deduplicates by id |
| `MESSAGE_EDITED` | Patches `content` and `edited_at` on the matching message |
| `MESSAGE_DELETED` | Removes the matching message |
| `ADD_OR_UPDATE_MESSAGE` | Whole-row upsert by id (append if absent, replace if present); used for sealed rows, where a sealed edit replaces the entire row (new ciphertext + `event_hash`) rather than patching `content`/`edited_at` |
| `ATTACHMENT_REDACTED` | Sets `redacted_by_moderator` on every `AttachmentInfo` whose `content_hash` matches, across all channels; leaves the message/attachment in place (placeholder stays) |
| `REACTION_ADDED` | Increments a reaction counter (or creates a new reaction entry); sets `me = true` if the reactor is the local user |
| `REACTION_REMOVED` | Decrements a reaction counter; removes the entry when count reaches 0 |
| `HIGHLIGHT_MESSAGE` | Sets `highlightMessageId` |

### Channel / category / role actions (per-server)

| Action type | What it mutates |
|---|---|
| `SELECT_CHANNEL` | Sets `currentChannelId`; clears `threadChannelId`; auto-advances `readState` for already-loaded messages |
| `CHANNEL_CREATED` | Appends to `channels` (deduplicates by id) |
| `CHANNEL_UPDATED` | Replaces the matching entry in `channels` |
| `CHANNEL_DELETED` | Removes from `channels` |
| `CATEGORY_CREATED` | Appends to `categories` |
| `CATEGORY_UPDATED` | Replaces the matching entry in `categories` |
| `CATEGORY_DELETED` | Removes from `categories` |
| `ROLE_CREATED` | Appends to `roles` |
| `ROLE_DELETED` | Removes from `roles` |
| `ROLE_UPDATED` | Replaces the matching role in `roles` by id; appends if not present |
| `VIEW_THREAD` | Sets `threadChannelId` (pass `null` to close) |
| `MARK_READ` | Updates `readState[channelId]` to `lastMessageId` |

### Rung-2 actions (per-server)

| Action type | What it mutates |
|---|---|
| `MLS_CONTROL_EVENT` | Appends an `MlsControlEventInfo` to `mlsControlEvents` (deduplicated by `eventHash`). Record-only — the steward (T9) drains the queue and processes each event via `fetch_mls_control` |
| `SEALED_DECRYPTED` | Upserts `sealedDecrypts[messageId] = { kind: "decrypted", content, eventHash }` |
| `SEALED_UNDECRYPTABLE` | Upserts `sealedDecrypts[messageId] = { kind: "undecryptable", reason, eventHash }` |

### DM actions (per-server)

| Action type | What it mutates |
|---|---|
| `SET_DMS` | Replaces the entire `dms` array |
| `DM_CREATED` | Appends a new `DmEntry` (deduplicates by channel id) |
| `OPEN_DM_PANEL` | Sets `dmPanelChannelId` |
| `CLOSE_DM_PANEL` | Clears `dmPanelChannelId` |

### Poll / giveaway / event widget actions (per-server)

Broadcast events (`POLL_UPDATED` / `GIVEAWAY_UPDATED` / `EVENT_UPDATED`) carry shared state only — the reducer preserves the local user's existing `myVote` / `myEntered` / `myRsvp` (defaulting to `null` / `false` / `null` for a slice not yet hydrated). The `MY_*` patch actions no-op when the id is not in state yet, so widgets must dispatch `POLL_STATE` / `GIVEAWAY_STATE` / `EVENT_STATE` (from the `getPoll` / `getGiveaway` / `getEvent` fetch) before relying on them.

| Action type | What it mutates |
|---|---|
| `POLL_UPDATED` | Upserts `polls[poll.id].poll` from a `server:poll_updated` broadcast; preserves existing `myVote`. Also maintains `activeWidgets` when set and the poll is in its channel: open-and-missing → append id (20 combined cap), closed → remove id |
| `POLL_STATE` | Sets `polls[poll.id]` to `{ poll, myVote }` (full self-inclusive state from `getPoll`) |
| `POLL_MY_VOTE` | Patches only `myVote` on an already-hydrated poll (optimistic local update after vote/retract) |
| `GIVEAWAY_UPDATED` | Upserts `giveaways[giveaway.id].giveaway` from a `server:giveaway_updated` broadcast; preserves existing `myEntered`. Also maintains `activeWidgets` when set and the giveaway is in its channel: open-and-missing → append id (20 combined cap), ended/cancelled → remove id |
| `GIVEAWAY_STATE` | Sets `giveaways[giveaway.id]` to `{ giveaway, myEntered }` (full self-inclusive state from `getGiveaway`) |
| `GIVEAWAY_MY_ENTERED` | Patches only `myEntered` on an already-hydrated giveaway (optimistic local update after enter/leave) |
| `EVENT_UPDATED` | Upserts `events[event.id].event` from a `server:event_updated` broadcast; preserves existing `myRsvp`. Also maintains `activeWidgets` when set and the event is in its channel: upcoming-and-missing → append id (20 combined cap), started/cancelled → remove id |
| `EVENT_STATE` | Sets `events[event.id]` to `{ event, myRsvp }` (full self-inclusive state from `getEvent`) |
| `EVENT_MY_RSVP` | Patches only `myRsvp` on an already-hydrated event (local update after a successful RSVP/clear ack) |
| `ACTIVE_WIDGETS` | Replaces `activeWidgets` whole with `{ channelId, polls: ids, giveaways: ids, events: ids }` (from `listActiveWidgets`; an empty payload hides the bar) and upserts every carried info into `polls`/`giveaways`/`events` with broadcast semantics (shared state only — existing `myVote`/`myEntered`/`myRsvp` preserved) |

### Typing actions (per-server)

| Action type | What it mutates |
|---|---|
| `TYPING_STARTED` | Upserts a typing entry for `(channelId, publicKey)` with an 8-second `expiresAt` timestamp |
| `TYPING_EXPIRED` | Removes the typing entry for `(channelId, publicKey)` |

### Voice roster actions (per-server)

These actions update `voiceStates` (who the server says is in each voice channel) and `currentVoiceChannelId` (which channel the local user has joined). They do NOT touch audio state — that lives in `useVoice`.

| Action type | What it mutates |
|---|---|
| `VOICE_JOINED` | Appends `{ publicKey, displayName }` to `voiceStates[channelId]`; deduplicates by `publicKey` |
| `VOICE_LEFT` | Removes the matching entry from `voiceStates[channelId]` by `publicKey` |
| `SET_VOICE_STATE` | Replaces `voiceStates[channelId]` with a full participant list (used for an authoritative snapshot) |
| `JOIN_VOICE_CHANNEL` | Sets `currentVoiceChannelId = channelId` (local UI intent; does not start audio) |
| `LEAVE_VOICE_CHANNEL` | Clears `currentVoiceChannelId` (local UI intent; does not stop audio) |

---

## `useServerEvents` — event-to-action mapping

`useServerEvents()` is called once at the app root. It registers one `listen()` call per `server:*` Tauri event inside a single `useEffect` (deps: `[dispatch]`). The cleanup correctly handles React StrictMode's double-mount via the `cancelled` flag + `safePush` pattern.

Two module-level caches are populated at import time (not inside the effect): `notifPrefs` (from `getNotificationPrefs`) and `cachedOwnPk` (from `getPublicKey`). These are used synchronously inside event callbacks.

**Active-server filter:** most per-server events are silently dropped if `data.server_id !== activeRef.current`. Exceptions are noted below. `activeRef` is a ref updated on every render so callbacks always see the current value without being listed as effect deps.

| Tauri event | Filter | Reducer action dispatched | Notes |
|---|---|---|---|
| `server:new_message` | none (DM decrypt path runs for all servers; unread increment runs for non-active) | `NEW_MESSAGE` (active server) or `INCREMENT_UNREAD` (background) | DM messages are decrypted via `api.dmDecrypt` before dispatch; on decryption failure the ciphertext is dispatched as-is. Triggers `api.showNotification` for background servers if `notifPrefs` allow it. |
| `server:message_edited` | active only | `MESSAGE_EDITED` | |
| `server:message_deleted` | active only | `MESSAGE_DELETED` | |
| `server:sealed_message` | active only | `ADD_OR_UPDATE_MESSAGE` | Flattens the `MessageInfoV2` payload (sealed row: `content` "", ciphertext in `sealed`, `is_e2ee` true) and upserts by id. No unread/notification — a sealed row has no plaintext to show (unread semantics are T5/T11's call) |
| `server:sealed_message_edited` | active only | `ADD_OR_UPDATE_MESSAGE` | Whole-row replace (new ciphertext + `event_hash`); `MESSAGE_EDITED` cannot express a sealed edit |
| `server:message_tombstoned` | active only | `MESSAGE_DELETED` | Content-blind delete in a sealed channel; same row-removal as the v1 delete |
| `server:channel_created_v2` | active only | `CHANNEL_CREATED` | Flattens the `ChannelInfoV2` payload onto `base` + `class` so a newly created E2EE channel appears live |
| `server:mls_control_event` | none (all servers) | `MLS_CONTROL_EVENT` | Record-only: appends to `mlsControlEvents` (deduped by `eventHash`). The steward (T9) drains the queue and processes each event via `fetch_mls_control` |
| `server:attachment_redacted` | active only | `ATTACHMENT_REDACTED` | Scans all channels' message lists for attachments matching `content_hash`; sets `redacted_by_moderator` in-place |
| `server:reaction_added` | active only | `REACTION_ADDED` | Sets `me: true` if `data.public_key === cachedOwnPk` |
| `server:reaction_removed` | active only | `REACTION_REMOVED` | |
| `server:member_banned` | none | dispatches a `farder:banned-list-changed` DOM `CustomEvent` | Consumed by the BannedList component directly via `window.addEventListener` |
| `server:member_unbanned` | none | dispatches `farder:banned-list-changed` DOM event | |
| `server:member_timeout_changed` | none (applies to all servers) | `MEMBER_TIMEOUT_CHANGED` | |
| `server:you_were_kicked` | none | `YOU_WERE_KICKED` | App-level action |
| `server:you_were_banned` | none | `YOU_WERE_BANNED` | App-level action |
| `server:audit_event_created` | none | dispatches `farder:audit-event-created` DOM `CustomEvent` | Consumed by `AuditLogTab` directly |
| `server:member_joined` | active only for reducer; background triggers notification | calls `api.getMembers` then `SET_MEMBERS` | Does a full refetch rather than an incremental insert, so `MEMBER_JOINED` action is never dispatched by this hook |
| `server:member_left` | active only for reducer; background triggers notification | `MEMBER_LEFT` | |
| `server:channel_created` | active only | `CHANNEL_CREATED` | |
| `server:channel_deleted` | active only | `CHANNEL_DELETED` | |
| `server:channel_updated` | active only | `CHANNEL_UPDATED` | |
| `server:category_created` | active only | `CATEGORY_CREATED` | |
| `server:category_deleted` | active only | `CATEGORY_DELETED` | |
| `server:category_updated` | active only | `CATEGORY_UPDATED` | |
| `server:role_created` | active only | `ROLE_CREATED` | |
| `server:role_deleted` | active only | `ROLE_DELETED` | |
| `server:role_updated` | active only | `ROLE_UPDATED` | Enables live refresh of hoist, reorder, color, and rename in member sidebar and role settings |
| `server:typing` | active only | `TYPING_STARTED` then `TYPING_EXPIRED` after 8 s via `setTimeout` | `displayName` is not available in the payload; `publicKey` is used as a fallback display name |
| `server:dm_created` | active only | `DM_CREATED` | |
| `server:disconnected` | none | `CONNECTION_LOST` | |
| `server:voice_joined` | **none** — dispatched for ALL servers | `VOICE_JOINED` | Voice roster must stay current even when the user is browsing a different server |
| `server:voice_left` | **none** — dispatched for ALL servers | `VOICE_LEFT` | Same rationale |

Cross-reference: every event in this table must have a corresponding emit arm in `bridge.rs` (documented in `tauri-bridge.md`). If `bridge.rs` drops an event, its listener here is dead code.

---

## `useSealedDecrypt` — decrypt-once hook (T10)

`useSealedDecrypt()` is mounted once at the app root (next to `useServerEvents` /
`useMlsSteward`). It scans the active server's `messages` for sealed rows
(`is_e2ee === true && sealed != null && content === ""`) and calls
`api.decryptSealedMessage` for each one that has no cached result. It reacts to
**state**, not to a single event, so it covers both arrival paths: a live
`server:sealed_message` (folded into `messages` by `useServerEvents`) and a
history load (`SET_MESSAGES` / `PREPEND_MESSAGES`).

**Decrypt-once invariant** is enforced in two layers, plus the backend:

1. **Terminal cache** — `sealedDecrypts[messageId]` holds the settled result;
   a row whose `(messageId, eventHash)` pair is already cached is skipped.
2. **In-flight dedupe** — a module-level `Map<"${serverId}:${messageId}", Promise>`
   keys each outstanding `invoke`; a re-render (or StrictMode double-mount) joins
   the existing promise rather than issuing a second `invoke`.
3. **Backend structure** — `decrypt_sealed_message` takes the ciphertext by value
   and calls `receive_sealed` exactly once; the returned `SealedOutcome` carries
   the bytes in neither variant, so a second open is structurally impossible.

On success it dispatches `SEALED_DECRYPTED`; on `undecryptable` (or a command
failure) it dispatches `SEALED_UNDECRYPTABLE`. Decrypted content lives only in
frontend memory (`sealedDecrypts`) — never persisted to disk in 4b (D4).

---

## `useVoice` — audio call hook

`useVoice()` is the single hook the UI uses for everything voice-call related. It owns its own local `useState` variables; it does NOT read from `ServerContext`. On mount it hydrates from `api.voiceGetState()` and subscribes to its `voice://*` Tauri events. On unmount it unlistens all of them.

**Ownership: one instance, in `AppShell`.** `useVoice()` is now called **once** in `AppShell` and the returned object is threaded down as a prop to both `ChannelSidebar` (sidebar JOIN button + self LIVE badge) and `ChatPanel` (the main-area `ScreenShareStage`). Because the hook holds local state (`isSharing`, `watching`, `displaySources`, …), calling it in two places would create two independent, divergent copies — the sidebar Join button and the main-area stage must share a single source of truth, so there is exactly one instance.

### Returned interface

| Field / method | Type | What it is |
|---|---|---|
| `inCall` | `boolean` | True when `channel_id` in the latest `VoiceState` is non-null |
| `muted` | `boolean` | Whether the local mic is muted |
| `deafened` | `boolean` | Whether the local user has deafened (can't hear peers) |
| `transmitting` | `boolean` | Whether push-to-talk is currently active (always `false` in voice-activated mode) |
| `localSpeaking` | `boolean` | Whether the VAD detects local speech |
| `peers` | `VoiceUiPeer[]` | List of connected audio peers; each has `pubkey`, `speaking`, `muted`, `deafened` |
| `connectionQuality` | `{ rttMs, lossPct } \| null` | Latest RTT and packet-loss sample; `null` when not in a call |
| `join(serverId, channelId)` | `() => Promise<void>` | Calls `api.voiceJoin`; state updates arrive via `voice://state-changed` |
| `leave()` | `() => Promise<void>` | Calls `api.voiceLeave` |
| `setMute(muted)` | `(boolean) => Promise<void>` | Calls `api.voiceSetMute` |
| `setDeafen(deafened)` | `(boolean) => Promise<void>` | Calls `api.voiceSetDeafen` |
| `toggleTransmit()` | `() => Promise<void>` | Calls `api.voiceToggleTransmit`; state refreshes via `voice://state-changed` |
| `peerVolume(pubkey)` | `(string) => number` | Returns the playback gain for a peer (0–2, default 1.0); loaded from persisted settings |
| `setPeerVolume(pubkey, v)` | `(string, number) => Promise<void>` | Optimistically updates the local gain map and persists via `api.voiceSetPeerVolume`; clamps to 0–2 |
| `isSharing` | `boolean` | Whether the local user is currently screen-sharing. Local `useState` only — there is no `voice://` event for share state, so it is set/cleared by `startShare`/`stopShare` and stays `true` until the user stops manually or leaves |
| `startShare()` | `() => Promise<void>` | Starts a local screen share via `api.voiceStartScreenShare(30, 1280, 720, sourceId, audioDeviceId)`, then sets `isSharing` |
| `stopShare()` | `() => Promise<void>` | Stops the local share via `api.voiceStopScreenShare()` (errors swallowed) and clears `isSharing` |
| `displaySources` | `api.DisplaySource[]` | Available capture sources, hydrated from `api.listDisplaySources()`; now a mix of `kind: "Screen"` (monitors) and `kind: "Window"` (single windows). Backs the grouped Screens/Windows list in `ShareSetupPopover` |
| `sourceId` / `setSourceId(id)` | `string \| null` / `(string \| null) => void` | Selected source id passed to `startShare` (`screen:{i}` or `window:{i}`); defaults to the first source. `null` = capture the first source |
| `refreshDisplaySources()` | `() => Promise<void>` | Re-fetches `api.listDisplaySources()` into `displaySources` and keeps `sourceId` valid (resets to the first source if the current selection vanished). Called by `ShareSetupPopover` when it opens so the source list is fresh |
| `sharingPeers` | `Set<string>` | Pubkeys of peers currently sharing video (Phase E), driven by the `voice://peer-video-sharing` event; backs the LIVE badge + JOIN button. Never contains the local client |
| `someoneElseSharing` | `boolean` | `sharingPeers.size > 0` — true when another peer is sharing; used to disable the local Share button (one sharer per channel) |
| `watching` / `toggleWatch(pubkey)` | `Set<string>` / `(string) => void` | **Single-watch** gating set: at most one peer at a time. `toggleWatch(pubkey)` watches that peer (clearing any other) or, if already watching them, stops. `ScreenShareStage` renders/decodes the watched peer's video. Backs the sidebar JOIN/WATCHING button |
| `setGameAudioVolume(pubkey, gain)` | `(string, number) => void` | Sets the per-peer game/screen-audio volume via `api.voiceSetScreenAudioGain` (Phase E); backs the stage's game-audio slider. Ephemeral (not persisted) |

(The viewer side — decoding the watched peer's shared video (and the sharer's own
self-preview) — lives in the main-area `client/src/components/ScreenShareStage.tsx`,
which listens for the `voice://peer-video-frame` and `voice://self-video-frame`
events directly rather than through `useVoice`. The old sidebar
`PeerVideoTiles.tsx` was retired. See `docs/modules/voice-video-transport.md` for
the share lifecycle + viewer.)

### `voice://*` events consumed

| Event | Payload type | Effect |
|---|---|---|
| `voice://state-changed` | `api.VoiceState` | Full state snapshot; runs through `applyState` which updates `inCall`, `muted`, `deafened`, `transmitting`, `peers`. When `inCall` becomes false, also clears `localSpeaking`, `transmitting`, and `connectionQuality`. |
| `voice://local-speaking` | `api.VoiceLocalSpeakingPayload` | Sets `localSpeaking` |
| `voice://peer-speaking` | `api.VoicePeerSpeakingPayload` | Patches `speaking` on the matching peer in `peers` by `pubkey`; no-op if the peer isn't in the list yet (next `state-changed` will fill it) |
| `voice://connection-quality` | `api.ConnectionQualityPayload` | Sets `connectionQuality` |
| `voice://peer-video-sharing` | `{ pubkey: string, sharing: boolean }` | Adds/removes the peer's pubkey in `sharingPeers` (Phase E); drives the LIVE badge and `someoneElseSharing` |

(The `voice://peer-video-frame` and `voice://self-video-frame` frame events are consumed by `ScreenShareStage` directly, not by `useVoice` — see the viewer note above.)

These events are emitted by the `VoiceController` in `client/src-tauri/src/voice/mod.rs`, not by `bridge.rs`. They are a separate event namespace from `server:*`.

---

## Integration map

- **`bridge.rs`** — emits every `server:*` event that `useServerEvents` listens for. If a `ServerEvent` is dropped in `bridge.rs` (falls through to `_ => Ok(())`), the corresponding listener in `useServerEvents` never fires. See `tauri-bridge.md` for the authoritative event catalog.
- **`voice/mod.rs`** (`VoiceController`) — emits the `voice://*` events that `useVoice` subscribes to. It is also the target of `api.voiceJoin`, `api.voiceLeave`, etc.
- **`tauri-bridge.ts`** (`client/src/lib/tauri-bridge.ts`) — the typed wrappers around `invoke()` that `useVoice` and `useServerEvents` call for the command direction (fetching members, decrypting DMs, controlling voice).
- **`lib/types.ts`** — defines `ChannelInfo`, `MemberInfo`, `MessageInfo`, etc. and the critical `publicKeyToString()` helper. All pubkey comparisons in the reducer and hooks use this function; mixing raw objects with string forms causes silent mismatches.

---

## Known gotchas

**Three separate representations of "who is in a voice channel"**

The voice roster (`PerServerState.voiceStates`), the audio peers (`useVoice().peers`), and `currentVoiceChannelId` are separate state sources that must stay consistent but are updated by different pipelines:

- `voiceStates` is driven by server-push events (`server:voice_joined` / `server:voice_left`) relayed by `bridge.rs`. It tracks everyone in the channel as the server sees them.
- `useVoice().peers` is driven by `VoiceController`-emitted `voice://state-changed` events. It tracks only peers with an active audio stream (WebRTC connected). A peer can be in `voiceStates` but not yet in `peers` if their audio stream hasn't connected, and vice versa if a `voice_left` event was dropped.
- `currentVoiceChannelId` is set by the UI dispatching `JOIN_VOICE_CHANNEL` / `LEAVE_VOICE_CHANNEL`. It is intentionally separate from `useVoice().inCall` — the UI sets it optimistically while the audio connection is being established.

If these three get out of sync (e.g. the user appears "in" a channel in the roster but `inCall` is false), the most common cause is a missed `server:voice_joined` / `server:voice_left` event in `bridge.rs` — check the `dispatch_event` match arms first.

**Voice events bypass the active-server filter**

`server:voice_joined` and `server:voice_left` are dispatched to ALL servers, not just the active one. This is intentional: the voice channel member counts in the sidebar must stay accurate even while the user is browsing another server. All other per-server events silently drop if `serverId !== activeRef.current`.

**`member_joined` does a full refetch, not an incremental push**

When `server:member_joined` fires, `useServerEvents` calls `api.getMembers()` and dispatches `SET_MEMBERS` with the full list. The `MEMBER_JOINED` action exists in the reducer but is never dispatched by the event hook. This is deliberate (the event payload is `{ public_key, display_name }`, not a full `MemberInfo`), but it means a member-join triggers a round-trip to the server.

**StrictMode double-mount leaks**

Both `useServerEvents` and `useVoice` use the `cancelled` / `safePush` pattern to handle React StrictMode's double-mount. If you add a new `listen()` call, wrap the resolved unlisten function with `safePush(u)`, not `unlisten.push(u)`, or the handler will leak on remount.

**Typing indicator display name**

The `server:typing` payload from `bridge.rs` carries `public_key` but not `display_name`. The hook falls back to using `publicKey` as the display name. The typing indicator component is responsible for resolving the display name from `members` if it needs a human-readable label.
