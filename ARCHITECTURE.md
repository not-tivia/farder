# Farder — Architecture Map

A one-page mental model of the whole system. Read this first, then dive into the
per-module docs under `docs/modules/`. (Documentation discipline is described in
`CLAUDE.md`.)

## What Farder is

A privacy-centric, self-hosted communication platform: Discord-style servers,
channels, roles; TeamSpeak-style self-hosted voice; Signal-style E2EE and
cryptographic identity; IRC-style lightness. Identity is an Ed25519 keypair (no
accounts). Traffic can route through relay nodes so a server never sees your IP.
DMs, private channels, and voice are end-to-end encrypted.

## The boundaries (why the code is split the way it is)

Data crosses several hard boundaries; each is a place bugs hide.

```
 React UI            Tauri (Rust)          Server (Rust)        Relay
 client/src   ──►   client/src-tauri  ──► crates/farder-   ──► crates/
 (TypeScript)  invoke()   bridge +      QUIC   server          farder-relay
              ◄──   commands +     ◄──  (handlers,      ◄──  (IP masking)
              events    voice engine        DB, events)
```

- **Language boundary** — TypeScript ↔ Rust, via Tauri `invoke()` (commands)
  and `emit()` (events). **Untyped:** `invoke("name")` is a plain string with no
  compile-time check that a `#[tauri::command]` of that name exists. This seam
  has already caused shipped regressions (voice-channel join). Treat it as
  high-risk; see `docs/modules/tauri-commands.md` and `tauri-bridge.md`.
- **Process boundary** — the UI/Tauri client is a separate process from the
  `farder-server` (which can run embedded as a sidecar for a local server, or
  remotely). They talk over QUIC.
- **Crate boundary** — Rust workspace crates compile independently; their public
  APIs are the contracts between them.
- **Network boundary** — server ↔ relay; the relay forwards traffic so the
  server learns the relay's address, not the client's.

## Directory map

| Path | What lives here |
|---|---|
| `client/src/` | React + TypeScript frontend. `components/`, `hooks/`, `context/` (the reducer/state), `lib/` (incl. `tauri-bridge.ts`, the `invoke()` wrappers, and `types.ts`). |
| `client/src-tauri/src/` | The Tauri (Rust) backend the UI talks to. `commands.rs` (every `#[tauri::command]`), `bridge.rs` (server-event → frontend-event bus + voice-event routing), `voice/` (the local voice engine), `audio_cpal.rs`/`audio.rs` (audio devices), `server_manager.rs` (spawns local server sidecars). |
| `crates/farder-server/` | The server: `handlers.rs` (request dispatch + permissions + DB writes + event broadcast), `channels.rs`, `members.rs`, `connection.rs`, `db.rs`, `media_stream.rs` (voice media relay), `polls.rs`/`giveaways.rs`/`channel_events.rs`/`reminders.rs`/`widgets.rs` (interactive message widgets — polls, giveaways, RSVP event cards — personal reminders, and the shared sweeper), `messages.rs` (**the single message-write choke point** — see below), `channel_class.rs` (fail-closed channel content class). |
| `crates/farder-protocol/` | The wire contract: `ServerRequest`, `ServerResponse`, `ServerEvent`, shared types. The single source of truth for client↔server messages. |
| `crates/farder-crypto/` | Ed25519 identity, X25519 key exchange, AES-GCM, E2EE DM + media key wrapping. |
| `crates/farder-mls/` | OpenMLS 0.8.1 wrapper for E2EE channel groups (mesh rung 2). Client-side only — the server never links it and never holds group keys. See `docs/modules/farder-mls.md`. |
| `crates/farder-node/` | Personal node embedded in the client (DMs). |
| `crates/farder-relay/` | Relay node (IP masking). |
| `crates/farder-notify/` | Desktop notifications helper. |

## Two end-to-end flows (the ones worth knowing)

**Sending a text message (legacy path):** UI calls `invoke("send_message")` → `commands.rs`
→ `bridge::send_request(ServerRequest::SendMessage)` over QUIC → server
`handlers.rs` checks permissions, writes to SQLite, broadcasts
`ServerEvent::NewMessage` to subscribers → each client's `bridge.rs` re-emits
the Tauri event `server:new_message` → `useServerEvents.ts` dispatches into
`ServerContext` → the UI re-renders.

**Sending a message with a file attachment (mesh/log path — mesh-mode servers only):**
1. UI calls `uploadFile` → `invoke("upload_file")` → server stores the blob and returns `UploadOutcome { fileId, contentHash, declaredType, size }`.
2. UI calls `submitEvent` → `invoke("submit_event", { ..., attachments: [{ contentHash, declaredType, size }] })` → `commands.rs` stamps `uploader` from the caller's identity and builds a signed `MessagePosted` event carrying `AttachmentCap`s.
3. Server `handlers.rs` (`SubmitEvent` arm) validates the event, then in a single SQLite transaction: stores the event body, derives the `messages` row, and calls `event_ingest::derive_attachments` to validate each cap (size/mime/uploader match + author-is-uploader-or-owner) and materialize `message_attachments` rows. Invalid caps are quarantined; the message still renders.
4. `NewMessage` is broadcast; downloaders go through the normal `download_file` path gated on `message_attachments` join rows. Both "not found" and "access denied" download responses are uniform (`"not available"`) so a `file_id` cannot be used as a *download-path* existence oracle. (The upload-dedup path still short-circuits on a known hash — a separate, pre-existing hash-existence oracle tracked under the file-hardening track, not closed by 4a.)

Note: URL-fetched images (via `fetchUrl`) and inline-emoji attachments have no client-known content hash and stay on the legacy `sendMessage` path even on mesh-mode servers.

**The message-write choke point (mesh rung 2):** every `messages` row in the
server — legacy `SendMessage`, edits, slash-command replies, webhooks, poll /
giveaway / event cards and their sweeper announcements, bot and system DMs, and
the log-derive path — goes through exactly one private statement,
`messages::insert_row`, reached via six named doors. Each door first resolves
the channel's content class from `channels.content_class` (mirrored from an
accepted `ChannelCreated`) via `channel_class::resolve`, which is **fail
closed**: a missing row, an unrecognised value, or a failed read all count as
encrypted and refuse. Only `messages::insert_sealed_row` may write into an E2EE
channel, it accepts only opaque ciphertext, and sealed rows never enter the FTS
index. A source-level test asserts no other file in the crate contains a raw
`INSERT INTO messages`, so a future writer fails a test rather than becoming a
plaintext door into a sealed channel. Details:
`docs/modules/server-handlers.md` § "Channel content class + the message write
choke point".

**Rung-2 ingest (`SubmitEvent`):** the arm's order is log-mode check →
`event_ingest::check_ingest_caps` (per-variant size/vector caps + a 300s
`core.timestamp` future bound, run **before** the `LogState` clone so a cap
breach cannot buy the allocation-heavy step) → the `stale-epoch` pre-check for
`MlsCommit` (a lost epoch CAS is an accepted no-op *in the fold*, so ingest
bounces it with that exact machine-readable code instead) → fold trial-apply →
one SQLite transaction (`store_event` → `materialize_channel_created` → derive)
→ in-memory `LogState` advance → broadcast. `ChannelCreated` materializes its
`channels` row **and its content class** inside that transaction, so the log and
its mirror cannot disagree across a crash; a refusal there rolls back the stored
event too. At startup `channel_class::reconcile_channel_classes` re-derives the
mirror from the log, repairing drift in the fail-closed direction only (it never
widens a channel the DB currently treats as sealed). Only a Plaintext-declared
channel is announced over the v1 `ServerEvent::ChannelCreated`; E2EE channels
wait for `ChannelInfoV2`.

**Attachment takedown (mesh-4b):** a member (uploader self-remove) or moderator (`KICK_MEMBERS`) right-clicks an attachment and chooses Remove / Take down. The UI calls `invoke("redact_attachment", { serverId, logServerId, contentHash })` → `commands.rs` builds a signed `AttachmentRedacted { content_hash }` event (same device-chain pattern as `submit_event`) and submits it via `ServerRequest::SubmitEvent`. On the server: `LogState::apply` validates authz (uploader OR `"kick"`, hash known, not already redacted), `attachments::redact_blob` sets `files.redacted_by` and deletes the on-disk bytes inside the persist TX, the in-memory `LogState` gains `content_hash` in `redacted_attachments`, and `ServerEvent::AttachmentRedacted { content_hash, by_moderator }` is broadcast to all clients. `bridge.rs` re-emits this as `server:attachment_redacted`; `useServerEvents.ts` dispatches `ATTACHMENT_REDACTED`; the `ServerContext` reducer walks `messages` and sets `redacted_by_moderator` on the matching `AttachmentInfo`. The UI replaces the attachment widget with a `Removed by the uploader` / `Removed by a moderator` placeholder. Download of a redacted blob returns uniform `"not available"` — same as not-found and access-denied — so the hash cannot be used as an existence oracle. At server startup, `event_ingest::sweep_redacted_bytes` deletes any remaining on-disk bytes for rows already marked redacted (crash-recovery).

**Joining voice:** there are TWO independent tracks. (1) **Presence/roster:**
`invoke("join_voice")` → `ServerRequest::JoinChannelMedia` → server adds you to
the channel's `voice_state` table and broadcasts `MediaJoined`/`MediaLeft` (the
participant list). (2) **Audio engine:** `invoke("voice_join")` →
`ServerRequest::JoinStream` → the Rust `VoiceController` (`voice/mod.rs`) opens
a QUIC media session, derives + wraps per-call stream keys, and spawns the
capture → encode → send and recv → decode → mix → playback pipeline
(`audio_cpal.rs` bridges real devices, resampling + channel-converting as
needed). The control bar is gated on the audio engine; the roster on presence.

**Media datagram transport (Phase A):** media travels as fragmentable QUIC
datagrams behind a unified 26-byte cleartext outer header
(`farder-protocol::media_datagram`). Audio is always a single fragment; video
(introduced in later screensharing phases) spans several. The relay and server
route on the cleartext outer header and never decrypt — the AEAD-sealed inner
frame (`farder-crypto::media`) is the unchanged security boundary carried as the
payload. See `docs/modules/media-datagram.md` for the full reference.

**Screensharing (Phase B — local loopback):** Windows Graphics Capture (`display_wgc.rs`,
`windows-capture` 2.0.0) grabs RGBA frames from the selected monitor; `H264Encoder`
(`video_encoder.rs`, openh264) converts them to Annex-B H.264 at 3 Mbps / 30 fps;
the encode loop (`screenshare.rs`) emits each encoded frame as a base64 Tauri event
(`screenshare:frame`); and `ScreensharePreview.tsx` decodes them via the WebCodecs
`VideoDecoder` API and paints a canvas. Phase B is a local loopback — no networking.
See `docs/modules/screenshare-capture-codec.md` for the full reference.

**Screensharing (Phase C1 — video transport):** inbound media datagrams now route per
`(session_id, track_kind)` so a single peer session can carry independent audio and
video frame streams. Audio and video are independently keyed (separate AEAD keys per
`TrackKind`), sealed, fragmented, and reassembled using the same Phase A
`media_datagram` layer. On the receiver, each decrypted video frame is forwarded to the
webview as the `voice://peer-video-frame` Tauri event (`{ session, data: base64 Annex-B
H.264, key, seq }`), where Phase C2's per-peer video tile will decode it via WebCodecs.
The server is unchanged — it routes video datagrams identically to audio. The audio
path is unaffected. Phase C2 (the share trigger, video-key offer, per-peer video tile,
keyframe-on-join, late-joiner re-offer) is not yet implemented.
See `docs/modules/voice-video-transport.md` for the full transport reference.

**Screensharing (Phase C2 — end-to-end):** screen-sharing is now wired end-to-end
(capture -> H.264 encode -> C1 video transport -> peer decode -> per-peer WebCodecs
tile), one sharer per call: the Share button drives `voice_start_screen_share`, which
derives + offers the video key, enables the Video track, and drives the C1 `VideoSender`
from the capture loop; viewers decode each `voice://peer-video-frame` in `PeerVideoTiles.tsx`
(keyframe-on-join + late-joiner re-offer keep mid-share joiners in sync). Game audio
capture and the polished share UI are later phases (D/E).

**Screensharing (Phase D — game/screen audio):** game audio now flows as a third media
track (`ScreenAudio`, own outer byte `0x03`, inner frame reusing the audio E2EE seal) over
the same E2EE/datagram path, captured best-effort via WASAPI loopback of a selectable output
device (`list_audio_output_devices`) and mixed at its own (independent) volume on the viewer;
the volume slider + polished UI are Phase E.

**Screensharing (Phase E — share UI, feature-complete):** screensharing is now feature-complete
across all five phases (A-E): a user picks a monitor (`list_display_sources`) and a game-audio
device, shares into a voice channel, and peers see a LIVE badge and click to watch with an
independent per-peer game-audio volume (`voice_set_screen_audio_gain`) — all E2EE.

**Screensharing (UX pass):** sources now include single **windows** (`window:` ids) as well as
whole screens; the sharer gets a **self-preview** of their own stream (`voice://self-video-frame`);
and the viewer moved from cramped sidebar tiles to a **main-area `ScreenShareStage`** with a
sidebar **Join** flow (single-watch), replacing the retired `PeerVideoTiles`.

## Incoming webhooks

Webhooks let an external caller (CI system, bot, script) POST a message into a Farder channel without a Farder identity. The data path spans three processes.

**Full path — external POST → message in channel:**

1. **Relay HTTP ingress** — the relay runs an HTTP server (`webhook.rs`) on `webhook_bind` (default `0.0.0.0:8080`). An external caller sends `POST /webhook/<server_id_hex>/<token>` with a JSON body (`{"content": "...", "username": "..."}`, Discord-compatible). The relay: (a) rate-limits by IP, (b) enforces a 64 KiB `DefaultBodyLimit`, (c) looks up the server by `server_id_hex` in its QUIC registry. The relay stores **no tokens** — it never reads the token value.
2. **QUIC forward** — on a hit, the relay opens a QUIC bi-stream on the server's existing control connection, prefixes it with handle `0u32` (relay-originated sentinel), and writes `RelayStreamRole::Webhook { token, body }`. The relay then reads a 2-byte big-endian status code from the server and returns it as the HTTP response.
3. **Server delivery** (`webhooks::deliver`) — the server's `serve_via_relay` dispatches the `Webhook` arm to `run_relay_webhook`, which calls `webhooks::deliver`. Delivery: (a) looks up the webhook by token (`find_by_token`) — 401 if not found; (b) parses the body (`parse_webhook_payload`) — 400 on bad JSON or empty content; (c) inserts the message via `insert_message_with_author_name` with the webhook's synthetic Ed25519 public key as author and `author_name_override` carrying the display name; (d) broadcasts `ServerEvent::NewMessage` to channel subscribers; (e) returns 204.
4. **Client render** — `bridge.rs` receives `NewMessage`; `MessageInfo.author_name_override` is non-null; `Message.tsx` renders the display name and a `WEBHOOK` badge instead of a member-roster lookup.

**Management:** channel admins (MANAGE_SERVER) create/list/delete/rotate webhooks via the Webhooks tab in Channel Settings. `create_webhook` and `regenerate_webhook_token` return the token once (write-only in the DB after that) along with `server_id_hex` so the client can build the ingest URL: `<RELAY_WEBHOOK_BASE>/webhook/<server_id_hex>/<token>`.

**Security invariants:** the webhook author is a per-webhook synthetic Ed25519 key that is never a roster member; `author_name_override` cannot be set by any member request (only by `deliver`). Deleting a webhook instantly invalidates its token (next POST returns 401). The relay never reads or stores tokens.

---

## Slash commands

Slash commands let a server owner configure `/trigger` shortcuts that post a bot-authored message in a channel. Commands are **not roster members** — each command has a synthetic Ed25519 keypair generated at creation time (stored in the `commands` table, never in `members`). Their messages carry `author_badge = "BOT"` and `author_name_override = cmd.name`.

Six command kinds exist: **text** (posts a fixed body string), **api** (fetches a remote JSON endpoint and formats a numeric value), the interactive **poll**, **giveaway** and **event** kinds (each posts a live widget card — see the "Poll, giveaway & event widgets" section below), and **reminder**, the odd one out: it posts **nothing at all** and answers the invoker with a private `ServerResponse::Notice`. The `kind` string is the extension point: every interactive kind landed as a new dispatch arm in `connection.rs` with no schema change.

**Data path — user types `/rules` → bot message appears:**

1. On server connect, `MessageInput` calls `invoke("list_commands", { serverId })` → `commands.rs::list_commands` → `ServerRequest::ListCommands {}`. The server calls `commands::list_infos(conn)` and returns `ServerResponse::Commands { commands: Vec<CommandInfo> }`. The result is stored in component state. Only safe fields (`id`, `trigger`, `description`, `takes_arg`) are returned; `url_template` and `body_text` are never sent to clients.
2. The user types `/` in the message input. `MessageInput` filters the command list by prefix and renders an autocomplete dropdown.
3. The user sends the message. `handleSend` matches the trigger against the known command list. If matched, calls `invoke("run_command", { serverId, trigger, channelId, args })` → `commands.rs::run_command` → `ServerRequest::RunCommand { trigger, channel_id, args }`. If the trigger is **not** in the local list, the message is sent as a normal user message — no round-trip to the server.
4. `connection.rs` handles `RunCommand` asynchronously (same pattern as `FetchUrl` — not dispatched through `handlers.rs`):
   - **Content gate:** `content_block_reason` blocks pending-approval or non-log members.
   - **Rate limit:** `command_limiter` (5 runs / 10 s per user).
   - **Lookup:** `commands::find_by_trigger` (DB lock released before any `.await`).
   - **Content resolution:** for `"text"` commands, uses `body_text` directly. For `"api"` commands, calls `commands::build_command_url` (percent-encodes `args`, substitutes into `{arg}` in `url_template`), then `bots::fetch_json` (SSRF-guarded), then `bots::extract_dot_path` (dot-path into the JSON), then `commands::format_response` (formats with optional template, unit, thousands separator).
   - **Failure → Error, no post:** any fetch error, non-2xx, dot-path miss, or unknown `kind` returns `ServerResponse::Error { reason }` and does NOT post a message to the channel. The client shows the reason string locally.
5. On success: `messages::insert_message_with_author_name(conn, channel_id, &cmd.public_key, &content, None, Some(&cmd.name), Some("BOT"))` inserts the message; `broadcast_event(EventTarget::Subscribers(channel_id), NewMessage { message })` fans it out. `ServerResponse::Ok` is returned to the invoker.
6. `bridge.rs` re-emits `server:new_message`; `useServerEvents.ts` dispatches `NEW_MESSAGE`; `ServerContext` prepends the message. The `Message` component renders the `author_badge` field as a `"BOT"` label next to the author name (falling back to `"WEBHOOK"` for webhook-posted messages — the badge is data-driven from the DB field, not from any client-side type check).

**Management:** server owners (MANAGE_SERVER) create and delete commands via the Bots tab → Slash Commands section. `addCommand` / `deleteCommand` → `ServerRequest::AddCommand` / `DeleteCommand`. The Add Command form in `BotsTab.tsx` accepts all fields (name, trigger, description, kind, and kind-specific fields).

**SSRF guard:** `"api"` command URL templates are fetched by `bots::fetch_json`, which calls `ssrf::resolves_to_global` before opening any connection. Private, loopback, and link-local IPs are rejected. The `url_template` is never logged or returned to clients.

---

## Poll, giveaway & event widgets (and personal reminders)

Interactive message widgets built on the slash-command substrate. Full reference: `docs/modules/server-widgets.md` (server: `polls.rs`, `giveaways.rs`, `channel_events.rs`, `reminders.rs`, `widgets.rs`), `docs/modules/server-system-identity.md` (the server's own DM-sending key), `protocol.md` (wire), `tauri-commands.md` Groups 26–27 (client seam).

**Data path — member runs `/poll Question | A | B | 2h` → live card everyone can vote on:**

1. **Creation** — `RunCommand` on a command of kind `"poll"`/`"giveaway"` (dispatch in `connection.rs`, like all RunCommand): parse the arg string (`polls::parse_poll_args` / `giveaways::parse_giveaway_args`), then in ONE transaction insert the fallback card message, the `polls`/`giveaways` feature row, and stamp the card's `messages.widget` column with `{"type":"poll"|"giveaway","id":…}` (`messages::set_widget`). Broadcasts `NewMessage` then `PollUpdated`/`GiveawayUpdated`. Poll creation is open to anyone who can send in the channel; **giveaway creation is MANAGE_SERVER-gated at dispatch**.
2. **Render** — `Message.tsx` parses `MessageInfo.widget` as untrusted JSON (try/catch + numeric-id check) and mounts `PollWidget.tsx` / `GiveawayWidget.tsx` in place of the plain content; old clients (or parse failures) fall back to the card's plain-text content. On mount the widget hydrates via `get_poll`/`get_giveaway` into the `polls`/`giveaways` slices of `ServerContext`.
3. **Interaction** — nine `ServerRequest` variants (`VotePoll`, `RetractVote`, `ClosePoll`, `GetPoll`, `EnterGiveaway`, `LeaveGiveaway`, `CancelGiveaway`, `RerollGiveaway`, `GetGiveaway`), all membership-gated (default-deny), visibility-checked with opaque not-found errors, timeout-gated on mutation, and rate-limited (`widget_limiter`, 10/10 s) on vote/retract/enter/leave. Every state change broadcasts the full shared struct as `PollUpdated`/`GiveawayUpdated` → `bridge.rs` → `server:poll_updated`/`server:giveaway_updated` → reducer → every visible card updates live.
4. **Retirement** — the single `widgets::spawn_widget_sweeper` task (15 s tick) closes due polls and draws due giveaways (`rand::thread_rng()` over eligible entrants; winner announcement posted by a fresh throwaway BOT-badged key). All sweeper work persists **before** broadcasting, so a crash can never re-close or redraw. Deleting a card closes/cancels its widget.

**Privacy invariants (polls/giveaways):** broadcasts and reads carry counts/status/winner only — voter and entrant identities never leave the server; a member's own `my_vote`/`my_entered` is returned only to them.

**Event cards (`📅`, `channel_events.rs`) follow the same four steps** with three deliberate differences: creation has **no MANAGE_SERVER gate** (anyone who can post can propose an event, and the card is authored **as the invoker** — no BOT badge); the widget JSON is `{"type":"event","id":…}` and mounts `EventWidget.tsx`; and the five interaction requests (`GetEvent`, `RsvpEvent`, `ClearRsvp`, `CancelEvent`, `EditEvent`) broadcast `EventUpdated` carrying **attendee display names** (≤10 per option, plus counts) — a **deliberate divergence** from the poll/giveaway anonymity rule, because the roster is the whole feature and an RSVP is an affirmative public act. The visibility boundary is still the channel (`channel_visible`, opaque `"event not found"`), and public keys are never in the payload. Retirement is three sweeper passes rather than one: a **lead-time** DM pass, a **start** pass (guarded `upcoming → started` flip + a threaded `📅 … is starting now!` announcement), and a **cancel-notify** DM pass — each behind its own single-shot column guard (`reminded_at`, `status`, `cancel_notified_at`) so a crash can never double-ping or double-announce.

**Personal reminders (`reminders.rs`) ride the sweeper but not the widget substrate.** `/remind 90m take the pizza out` posts no message, creates no widget, and broadcasts nothing; it inserts a `reminders` row (≤500 chars, ≤20 outstanding per member) and replies `ServerResponse::Notice { text }` on the invoker's own request id. A fourth sweeper pass flips due rows to `sent` and returns one DM each. `ListMyReminders`/`CancelReminder` back the Settings → Reminders panel and are **owner-scoped in SQL** by the authenticated connection key; a foreign or already-fired id gets the opaque `"reminder not found"`.

**The server system identity** is the enabling piece both features needed: one lazily-created keypair (`bots` row `kind='system'`, label `"Farder"`) that the sweeper uses to send DMs and author the event-start announcement. It is filtered out of `GetMembers` in SQL, excluded from `list_bots`, refused by `RemoveBot`, and cannot authenticate a connection. The sweeper returns DMs as **data** (`PendingDm`) precisely so `send_system_dm` re-acquires the DB mutex only after the sweeper's guard has been dropped. See `docs/modules/server-system-identity.md`.

---

## Relay as fetch proxy (invite previews + rich embeds)

The relay doubles as a **privacy fetch proxy**. All outbound HTTP(S) traffic
originates from the relay — the viewer's IP is never exposed to any third-party
host. There are two proxy modes:

**Phase 1 — Invite previews:** when a client hovers over an invite link before
joining, it sends `ProxyInvitePreview` to the link's relay (or the default relay
for direct links) instead of contacting the target server directly. The relay opens
a handle-0-stamped `RelayStreamRole::Primary` stream on the server control
connection, speaks the Challenge / GetInvitePreview / InvitePreview exchange, and
returns `ProxyInvitePreviewResult`. Guardrails: 30/min/IP rate bucket, 5 s fetch
timeout, 16 KB answer cap, 256-char code cap, SSRF guard, 60 s TTL cache.

**Phase 2 — Rich external embeds:** when the client encounters an allowlisted URL
in a message, it sends `ProxyLinkEmbed` on a fresh throwaway connection. The relay
resolves the URL through a provider adapter (Twitter/X via fxtwitter, YouTube and
Spotify via oEmbed, Reddit via `.json` API, direct images inline), enforces a
strict egress allowlist + SSRF guard on all resolved IPs + redirect re-validation,
and returns `ProxyLinkEmbedResult { outcome: EmbedOutcome }`. Media bytes are
fetched on a separate `ProxyMedia` throwaway connection; the relay validates the
content-type (images and `video/mp4` only) and caps bytes at 25 MB. Rate limits:
30 metadata / 60 media requests per minute per IP. 1 h relay-side TTL cache (2048
entries). Egress guardrails in `crates/farder-relay/src/embed.rs::SafeFetcher`.

The client-side commands are `get_invite_preview`, `get_link_embed`, and
`get_proxied_media`. See `docs/modules/relay-proxy.md`,
`docs/modules/relay-embed.md`, and `docs/modules/tauri-commands.md` for the full
reference.

## Profile sync

Avatars and status text travel as **identity-signed blobs** (`SignedProfile` from
`farder-crypto`). The client builds a per-server effective profile (per-server
avatar override ?? global `avatar.png`; global status; display name), signs it
fresh each time, and pushes it to the server via `ServerRequest::UpdateProfile`.
The server stores the raw blob in `members.avatar` and a SHA-256 hash in
`members.profile_hash`. Other clients fetch the blob on demand via
`ServerRequest::GetMemberProfile`, verify the signature and hash, and cache the
result in `~/.farder/profile_cache/<hash>`. The `MemberInfo.profile_hash` field
acts as a cache key: an unchanged hash means the local cache is still valid.

---

## Crypto ticker bots

Ticker bots are **server-managed members**: a synthetic roster entry whose Ed25519 keypair is generated and held by the server, not by any user. The server drives their presence autonomously.

**Data path — owner adds a bot → live price appears in the member list:**

1. Owner calls `invoke("add_bot", { serverId, coinId, label })` → `commands.rs::add_bot` → `ServerRequest::AddBot { coin_id, label }` over QUIC.
2. `handlers.rs` (`AddBot` arm, owner-gated): generates a fresh Ed25519 keypair, inserts a `bots` row (stores secret key + CoinGecko coin id), inserts a `members` row with `is_bot=1`; `label` is the `display_name`. Broadcasts `ServerEvent::MemberJoined { public_key, display_name: label }` → all connected clients.
3. The price-poller (`bots::spawn_bot_poll_task`, launched at server startup, ~60 s interval):
   - Snapshots the bot list (drops DB lock before any `.await`).
   - Deduplicates coin IDs and issues ONE SSRF-guarded CoinGecko `/simple/price` fetch for all bots.
   - For each bot in the response: calls `bots::ticker_presence` (formats `"$<price> <arrow><pct>%"`), stores the `Presence` in `state.presences` (RwLock), and broadcasts `ServerEvent::MemberPresenceUpdated` via `connection::broadcast_event`.
4. `bridge.rs` re-emits `server:member_presence_updated`; `useServerEvents.ts` dispatches `UPDATE_MEMBER_PRESENCE`; `ServerContext` updates `member.presence` for the bot's public key.
5. The client renders the bot with a **BOT badge** (`is_bot: true`) and the inline ticker price (`member.presence.details`) in the member list.

**Removal:** `invoke("remove_bot", { serverId, botPublicKey })` → `ServerRequest::RemoveBot` → deletes `bots` + `members` rows, evicts presence from `state.presences`, broadcasts `MemberLeft`. Cascade: `bots::remove_bot` first deletes all `bot_alerts` and `bot_subscriptions` rows for that bot.

**Persistence:** bots survive server restarts; the poller re-fetches prices within one tick after startup.

**SSRF guard:** the CoinGecko URL is pre-validated by `ssrf::resolves_to_global` before any network call; HTTP redirects are disabled on the reqwest client.

**Mesh roster:** for log-mode servers, `GetMembers` filters the roster to `m.is_bot || ls.is_member(...)` so bots always appear alongside human log-space members.

### Price alerts → E2EE bot DMs

The alert feature lets server owners define conditions on a bot's price data and members opt in to receive a DM when a condition fires.

**Setup (owner):** Server Settings → Bots → select bot → Alerts sub-section → Add alert (`metric`, `comparator`, `threshold`). Each alert is stored in `bot_alerts` (armed=1). `invoke("add_bot_alert", ...)` → `ServerRequest::AddBotAlert` → MANAGE_SERVER-gated.

**Opt-in (any member):** Right-click a bot in the member list → Notify me (🔔 toggle). `invoke("subscribe_bot", ...)` → `ServerRequest::SubscribeBot` → INSERT OR IGNORE into `bot_subscriptions`. The authenticated caller's own key is always used as the subscriber; the client cannot specify another user.

**Data path — alert fires → DM delivered:**

1. Poll loop receives a successful `fetch_prices` response and processes each bot that has a real `PriceInfo`.
2. **Under DB lock:** calls `bots::evaluate_alert(value, comparator, threshold, armed)` for each alert — fire-once with hysteresis:
   - Armed + condition met → `(true, false)`: fire and disarm.
   - Disarmed + condition cleared → `(false, true)`: re-arm only.
   - Otherwise no change.
   Persists armed-state changes via `bots::set_alert_armed`. Collects fired alerts into a `Vec<(metric, comparator, threshold)>`. Drops the DB lock.
3. **Under a fresh DB lock:** loads subscribers from `bot_subscriptions`. Drops the lock.
4. **No lock held (async):** for each fired alert × each subscriber, calls `bots::send_bot_dm`:
   - Loads the bot's Ed25519 secret key (`bots::get_bot_secret`).
   - Opens or reuses the DM channel (`channels::open_dm_channel`).
   - Encrypts the alert text via `bots::encrypt_bot_dm` → `farder_crypto::key_exchange::derive_dm_shared_secret` (X25519 ECDH) + `farder_crypto::encryption::encrypt` (AES-256-GCM). The nonce is prepended: the ciphertext is `nonce(12)||ct+tag`, hex-encoded.
   - Persists the ciphertext as a `messages` row.
   - Broadcasts `DmCreated` (if new channel) and `NewMessage` **targeted at `EventTarget::Members([recipient])`** — only the recipient receives the events, not all connected clients.
5. The recipient's client receives the `DmCreated` / `NewMessage` events via `bridge.rs` → `server:dm_created` / `server:new_message`. The UI calls `dmDecrypt(botPublicKey, ciphertextHex)` to decrypt — symmetric because the shared secret is the same from either direction.

**Re-arm:** after the condition is disarmed, the alert re-arms only once the condition clears. A BTC "above $70 000" alert that fires at $71 000 will not re-fire on the next poll cycle; it re-arms when BTC drops below $70 000, then fires again if/when it crosses above again.

**My subscriptions:** Settings → Alerts section lists all bots the user is subscribed to, populated by `invoke("list_my_subscriptions", ...)` → `ServerRequest::ListMySubscriptions` → `bot_subscriptions` read. The user can unsubscribe per-bot from this view.

---

## Custom monitor bots

Custom monitor bots let an owner point a bot at any public JSON API; the server polls it and broadcasts the extracted numeric value (with an optional unit label) as the bot's presence.

**Data path — owner adds a custom monitor → value appears in the member list:**

1. Owner opens Server Settings → Bots → Add Custom Monitor. Fills in name, API URL, dot-path (e.g. `data.players`), and optional unit. The UI calls `invoke("add_custom_bot", { serverId, name, sourceUrl, valuePath, unit })` → `commands.rs::add_custom_bot` → `ServerRequest::AddCustomBot { name, source_url, value_path, unit }` over QUIC.
2. `handlers.rs` (`AddCustomBot` arm, MANAGE_SERVER-gated): validates all fields, generates a fresh Ed25519 keypair, calls `bots::register_custom_bot` (inserts a `bots` row with `kind='custom_api'`, `coin_id=''`, `source_url`, `value_path`, `unit`) and `members::register_bot_member` (inserts a `members` row with `is_bot=1`, `name` as `display_name`). Broadcasts `ServerEvent::MemberJoined { public_key, display_name: name }` → all connected clients.
3. The price-poller (`bots::spawn_bot_poll_task`) branches on `bot.kind`:
   - **`custom_api`:** calls `bots::fetch_json(source_url)` — SSRF-guarded (pre-validated by `ssrf::resolves_to_global`; rejects private/loopback addresses), 10 s timeout, redirects disabled, 256 KiB body cap. On success, calls `bots::extract_dot_path(&json, value_path)` to walk the dot-separated key chain and coerce the leaf to `f64`. On success: `bots::custom_value_presence(value, unit)` formats `"<value> <unit>"` (integers get thousands separators); `bots::broadcast_presence` stores the result in `state.presences` and broadcasts `ServerEvent::MemberPresenceUpdated`. On any fetch or extract failure: `bots::unavailable_presence()` broadcasts `"unavailable"`.
4. `bridge.rs` re-emits `server:member_presence_updated`; `useServerEvents.ts` dispatches `UPDATE_MEMBER_PRESENCE`; `ServerContext` updates `member.presence` for the bot.
5. The client renders the bot with a **BOT badge** and the inline value+unit string as presence.

**Alert reuse:** after broadcasting presence for a successful fetch, the poller calls `bots::eval_and_notify_alerts` with `metrics=[("value", v)]`, using the same fire-once-with-hysteresis engine, E2EE DM delivery, and subscription model as crypto ticker bots. See the "Price alerts → E2EE bot DMs" section above. `AddBotAlert` accepts metric `"value"` (alongside `"price_usd"`/`"change_24h"`), and the client alert form exposes a **Value (custom bots)** metric option, so custom-bot alerts are fully configurable end-to-end.

**SSRF guard:** `fetch_json` rejects any URL whose resolved IP is private, loopback, or link-local. Only the server owner can configure the URL (MANAGE_SERVER gate), so the attack surface is limited to owner-trusted actors. Redirects are disabled on the reqwest client.

**Degrade-to-unavailable:** custom bot fetch failures (SSRF rejection, timeout, non-2xx response, body too large, parse failure, missing key, non-numeric leaf) all result in `"unavailable"` presence for that cycle. The previous presence is overwritten — there is no "keep last" behavior for custom bots (unlike crypto ticker bots on a batch fetch failure).

---

## Cross-cutting things that bite

- **Identity at rest:** `client/src-tauri/src/identity.rs` (`IdentityStore`)
  stores the Ed25519 key encrypted (Argon2id + AES-256-GCM) behind a 4-digit
  PIN; `farder-crypto::recovery` provides a BIP39 recovery phrase. See
  `docs/superpowers/audits/2026-06-05-privacy-security-wiring-audit.md` Gap #2.
- **The wire is v1/v2 split, and old clients cannot ignore what they cannot
  parse.** MessagePack fails a decode on an unknown variant name or a
  wrong-length field array, and it fails the WHOLE frame — so one v2-only event
  sent to a v1 client breaks that client's stream in plaintext channels too.
  Consequences: never add or reorder a field on a shipped struct (add a new
  `...V2` struct instead — that is why `ChannelInfoV2` / `MessageInfoV2` exist);
  a connection that has not sent `NegotiateProtocol` is treated as v1; and
  v2-only events are filtered at the send in `connection::may_receive`. See
  `docs/modules/protocol.md` and `docs/modules/server-connection.md`.
- **The untyped command seam** (above). Keep `invoke("...")` names, the
  `#[tauri::command]`, and the `generate_handler!` list in `main.rs` in sync.
- **The event bus** (`bridge.rs`): every `ServerEvent` maps to exactly one
  `server:...` Tauri event with a specific JSON payload. If a variant is matched
  to `=> Ok(())`, the UI silently never hears it. See `docs/modules/tauri-bridge.md`.
- **Identifier serialization**: a Rust `PublicKey` is `{ bytes: [...] }` over
  serde IPC, but `"vk_<hex>"` when emitted via `.to_string()`. The TS helper
  `publicKeyToString()` normalizes the object form. Mismatches cause silent
  lookup/filter failures.
- **State has layers**: the React `ServerContext` roster (`voiceStates`), the
  audio engine peers (`voice.peers`), and the context's `currentVoiceChannelId`
  are separate sources that must be kept consistent.
