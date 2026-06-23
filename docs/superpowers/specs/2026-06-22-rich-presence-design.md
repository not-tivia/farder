# Rich Presence / Activity Status — Design Spec

**Date:** 2026-06-22
**Status:** Approved (brainstorm), pending dep-probe + implementation plan
**Builds on:** the profile-sync pipeline (manual `status` field, `MemberProfileUpdated`) and the member roster. Adds an automatic, ephemeral *activity* layer ("🎵 Listening to …", later "🎮 Playing …").

## Goal

Show what a member is doing — automatically — next to their name: music now ("Listening to <song> – <artist>"), games later. Privacy-first (opt-in, off by default) and source-agnostic so a game detector plugs in later without protocol/UI changes.

## Product decisions (owner, locked 2026-06-22)

1. **Sources:** music first; build a **source-agnostic** presence pipeline so **games** plug in later (documented as we go). Music auto-detect ships now; games is a follow-on.
2. **Opt-in:** off by default. A **master "Share my activity"** toggle + **per-source** toggles ("Share music"; later "Share games").
3. **Display:** in the member list, **activity takes priority over the manual status** (show activity when present, else the manual status — one line). The **profile popup shows both**. Full detail on click.
4. **Music detail:** **full track** — "🎵 Listening to <song> – <artist>" (it's opt-in, so full detail is the default; an app-only mode can come later).

## The Presence model (source-agnostic)

```rust
// farder-protocol (new)
pub enum PresenceKind { Music, Game }     // extensible
pub struct Presence {
    pub kind: PresenceKind,
    pub details: String,          // music: track title; game: game name
    pub state: Option<String>,    // music: artist; game: None (for now)
}
```
The client formats by `kind`: Music → "🎵 Listening to {details} – {state}"; Game → "🎮 Playing {details}". Adding games needs **no** protocol/UI change — only a new producer. Validation: `details` ≤ 128 chars, `state` ≤ 128 (mirrors the existing status cap).

## Transport — ephemeral, NOT the signed profile

Presence changes every few seconds, so re-uploading the whole signed profile (avatar included) is wrong. Presence gets its own lightweight, ephemeral channel:

- **Protocol (farder-protocol):**
  - `ServerRequest::UpdatePresence { presence: Option<Presence> }` — `None` clears.
  - `ServerEvent::MemberPresenceUpdated { public_key: PublicKey, presence: Option<Presence> }`.
  - `MemberInfo` gains `presence: Option<Presence>` (`#[serde(default)]`, backward-compatible) so the roster carries current presence to late joiners.
- **Server (farder-server):** keeps an **in-memory** map `public_key -> Presence` per server (NO database). On `UpdatePresence` from an authenticated session, it sets/clears the map entry **stamped with the sender's own authenticated public key** (a client can never set another member's presence), validates field lengths, then broadcasts `MemberPresenceUpdated`. On disconnect/session cleanup it removes the member's entry and broadcasts the cleared presence. `GetMembers` includes each member's current presence. A modest per-session rate limit (e.g., ≤ 1 update/sec) guards against abuse.
- **Not signed.** Unlike identity/display-name/avatar/status (signed to stop a malicious server forging your identity), presence is ephemeral and low-stakes; only the server itself could forge it (same trust boundary as community content, which the server already sees). This is the deliberate trade for keeping it lightweight. Documented in the security section.
- **Relay:** flows over the existing connection (relayed or direct); the relay forwards blind. No relay changes.

## Detection — the music source (Windows)

- A background **poller** in the Tauri Rust client reads Windows' **Global System Media Transport Controls (GSMTC)** — `Windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager` via the `windows` crate — every ~5s: get the current session, read `Title`/`Artist` and playback status. **Playing** → build `Presence{Music, title, artist}`; **paused/stopped/none** → clear. Works for Spotify, browsers, any media app that registers a session.
- **Debounced:** only pushes `UpdatePresence` when the (kind, details, state) actually changes.
- **Gated** by the opt-in settings: the poller is idle (and pushes a clear once) when "Share my activity" or "Share music" is off.

## Source-agnostic seam (for games later)

- A `PresenceSource` trait: `fn current(&self) -> Option<Presence>` (+ an `id`/`kind`). `MusicSource` implements it now. A future `GameSource` (foreground-window/process detection + a per-app allowlist) implements the same trait.
- A `PresenceManager` polls the **enabled** sources, merges them by priority (only music exists now; documented order for when games arrives, e.g. game > music), debounces, and pushes to all connected servers (mirrors `push_profile_everywhere`). On disable/quit it pushes a clear.
- **Module doc** (`docs/modules/`) will state exactly what a `GameSource` must provide (how it names a game, the allowlist contract, the merge priority) so the games phase is a drop-in.

## Settings

- Client settings (read by the Rust poller, so stored in the settings file via Tauri commands — same pattern as input-device / data-saver): `presence_enabled` (master, default **false**) and `presence_music` (default **false**). New commands `get_presence_settings` / `set_presence_settings` (registered in `generate_handler!`). Toggling on starts the poller; off stops it and pushes a clear.
- Frontend UI: a "Share my activity" master checkbox + "Share music" checkbox in Settings (Privacy & Data section of `VoiceSettings.tsx`, alongside the existing embed/data-saver toggles), with a one-line privacy note.

## Client UI (rendering others' presence)

- `MemberInfo.presence` flows in via the roster (`SET_MEMBERS`); `MemberPresenceUpdated` events update it live. Store per-server presence (e.g. `PerServerState.presences: Record<pubkeyHex, Presence>`, seeded from the roster, updated by the event, cleared on `null`).
- A `formatPresence(p)` helper → `{ icon, text }`. **Member list:** show the member's activity line when present, else their manual status. **Profile popup (`UserProfilePopup.tsx`):** show both the activity and the manual status. (Chat avatars unchanged for now.)
- `useServerEvents.ts` gains a `MemberPresenceUpdated` listener.

## Dep-validation probe (GATING — before the implementation plan locks)

Like the screenshare/audio native features, the Windows GSMTC API via the `windows` crate is a native unknown WSL can't test. Ship a throwaway `presence-probe/` (detached cargo workspace, `#![cfg(windows)]`) that opens the GSMTC session manager and prints the current now-playing **title / artist / playback-status**. **Owner runs `cd presence-probe && cargo run --release`** with music playing and pastes the output. This confirms the API + the exact `windows` crate **features** needed (e.g. `Media_Control`, `Foundation`), which the plan then locks (version + feature set). Delete the probe when the music source ships.

## Components / files

- `crates/farder-protocol`: `Presence`/`PresenceKind`, `UpdatePresence`, `MemberPresenceUpdated`, `MemberInfo.presence`.
- `crates/farder-server`: in-memory presence map + `UpdatePresence` handler (validate, stamp sender pk, rate-limit, broadcast), disconnect cleanup, roster inclusion.
- `client/src-tauri/src/presence/`: `mod.rs` (`PresenceSource` trait + `PresenceManager`), `music_windows.rs` (`#[cfg(windows)]` GSMTC source) + a non-Windows stub returning `None` (so it builds on Linux/CI), push integration, settings-gated start/stop; settings get/set commands; registration in `main.rs`.
- `client/src`: `lib/types.ts` (`Presence`, `MemberInfo.presence`), `lib/tauri-bridge.ts` (presence settings + event), `useServerEvents.ts` (`MemberPresenceUpdated`), `context/ServerContext.tsx` (`presences` state + reducer), `components/MemberSidebar`/member list + `UserProfilePopup.tsx` rendering, `VoiceSettings.tsx` toggles, a `lib/presence.ts` `formatPresence` helper.
- Docs: `docs/modules/` presence module doc (incl. the game-source contract) + `docs/modules/tauri-commands.md` / protocol docs / `frontend-*` updates per the doc-discipline checklist.

## Data flow (music)

1. User enables "Share my activity" + "Share music" → `PresenceManager` starts.
2. Poller reads GSMTC every ~5s → playing → `Presence{Music,title,artist}`; debounce vs last.
3. On change → `UpdatePresence` to each connected server.
4. Server stamps sender pk, validates, stores in-memory, broadcasts `MemberPresenceUpdated`.
5. Other clients update `presences[pk]`; member list shows the activity (over manual status); popup shows both.
6. Pause/stop or toggle-off → `UpdatePresence{None}` → cleared everywhere. Disconnect → server clears + broadcasts.

## Error handling & edge cases

- **No media playing / GSMTC empty:** presence is `None` (cleared). 
- **GSMTC/COM error:** the poll is skipped (logged at debug); the manager keeps running; never crashes the client.
- **Settings off:** poller idle; one clear pushed on transition to off.
- **Non-Windows build:** `MusicSource` stub returns `None` (feature simply inactive); everything compiles on Linux/CI.
- **Late joiner:** sees current presences via the roster.
- **Rate-limit hit:** server drops the excess update (client debounce should keep it under the limit anyway).

## Testing

- **Probe** (owner, Windows) — gates the plan.
- **Headless:** protocol serde round-trip for `Presence`/messages; server unit tests (set/clear/roster-inclusion/disconnect-clear/own-pk-stamping/rate-limit); client `PresenceManager` debounce + merge + settings-gating with a mock `PresenceSource` (the Windows source is `#[cfg(windows)]`, but the manager/trait are cross-platform and testable on Linux); `formatPresence` cases. `cargo test --workspace` + client crate + `tsc`.
- **Runtime (owner, Windows, 2 clients — UNVERIFIED until run; needs server sidecar rebuild + both clients):** enable share-music on A, play a song → B sees "🎵 Listening to <song> – <artist>" in the member list and (with manual status) both in A's popup; pause → clears; switch songs → updates; toggle off → clears; works relayed + direct.

## Out of scope (this phase)

- **Game detection** (foreground-app/process source) + the **per-app allowlist** — designed-for but implemented in the games phase.
- App-only (hide-track) music mode — possible later toggle.
- Non-Windows presence detection (stub only).
- Signed/authenticated presence; persisting presence across sessions.
- Chat-message author-line presence (member list + popup only for now).

## Documentation (same-commit discipline)

New protocol messages → protocol docs; new Tauri commands → `tauri-commands.md` + `tauri-bridge.md`; new event → `tauri-bridge.md` + `useServerEvents` note; new presence module → a `docs/modules/` doc that ALSO specifies the future `GameSource` contract; `ARCHITECTURE.md` if a new data-flow path warrants it.
