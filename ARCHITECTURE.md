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
| `crates/farder-server/` | The server: `handlers.rs` (request dispatch + permissions + DB writes + event broadcast), `channels.rs`, `members.rs`, `connection.rs`, `db.rs`, `media_stream.rs` (voice media relay). |
| `crates/farder-protocol/` | The wire contract: `ServerRequest`, `ServerResponse`, `ServerEvent`, shared types. The single source of truth for client↔server messages. |
| `crates/farder-crypto/` | Ed25519 identity, X25519 key exchange, AES-GCM, E2EE DM + media key wrapping. |
| `crates/farder-node/` | Personal node embedded in the client (DMs). |
| `crates/farder-relay/` | Relay node (IP masking). |
| `crates/farder-notify/` | Desktop notifications helper. |

## Two end-to-end flows (the ones worth knowing)

**Sending a text message:** UI calls `invoke("send_message")` → `commands.rs`
→ `bridge::send_request(ServerRequest::SendMessage)` over QUIC → server
`handlers.rs` checks permissions, writes to SQLite, broadcasts
`ServerEvent::NewMessage` to subscribers → each client's `bridge.rs` re-emits
the Tauri event `server:new_message` → `useServerEvents.ts` dispatches into
`ServerContext` → the UI re-renders.

**Joining voice:** there are TWO independent tracks. (1) **Presence/roster:**
`invoke("join_voice")` → `ServerRequest::JoinChannelMedia` → server adds you to
the channel's `voice_state` table and broadcasts `MediaJoined`/`MediaLeft` (the
participant list). (2) **Audio engine:** `invoke("voice_join")` →
`ServerRequest::JoinStream` → the Rust `VoiceController` (`voice/mod.rs`) opens
a QUIC media session, derives + wraps per-call stream keys, and spawns the
capture → encode → send and recv → decode → mix → playback pipeline
(`audio_cpal.rs` bridges real devices, resampling + channel-converting as
needed). The control bar is gated on the audio engine; the roster on presence.

## Cross-cutting things that bite

- **Identity at rest:** `client/src-tauri/src/identity.rs` (`IdentityStore`)
  stores the Ed25519 key encrypted (Argon2id + AES-256-GCM) behind a 4-digit
  PIN; `farder-crypto::recovery` provides a BIP39 recovery phrase. See
  `docs/superpowers/audits/2026-06-05-privacy-security-wiring-audit.md` Gap #2.
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
