# Rich Presence / Activity Status Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Broadcast a member's automatic activity ("🎵 Listening to <song> – <artist>") next to their name — opt-in, ephemeral, source-agnostic so a game source plugs in later.

**Architecture:** A source-agnostic `Presence` value syncs over a new, ephemeral, in-memory (no-DB) channel separate from the signed profile. The Tauri Rust client polls Windows' media-session API (GSMTC) behind a `PresenceSource`/`PresenceManager` seam, debounces, and pushes `UpdatePresence`; the server stamps the sender's authenticated key, stores it in memory, includes it in the roster, and broadcasts `MemberPresenceUpdated`; the frontend renders activity over the manual status in the member list and both in the profile popup.

**Tech Stack:** Rust (farder-protocol/server/client-tauri), the `windows` crate (GSMTC), React/TS frontend.

## Global Constraints

- **Deps LOCKED by the 2026-06-22 probe:** in `client/src-tauri/Cargo.toml` under `[target.'cfg(windows)'.dependencies]`, add `windows = { version = "0.58", features = ["Media_Control", "Foundation"] }`. (Probe confirmed GSMTC reads title/artist/app/status with these. If `cargo tree` shows `windows-capture`/`wasapi` already pull a *different* `windows` major, still add this explicit `0.58` dep — Cargo coexists multiple majors; do not downgrade.)
- **Ephemeral + unsigned.** Presence is NOT persisted (no DB) and NOT signed. The server stores it in an in-memory map keyed by the **sender's authenticated public key** (a client can never set another member's presence), clears it on disconnect.
- **Opt-in, OFF by default.** `presence_enabled` (master) + `presence_music` both default false. The poller is idle and pushes a single clear when disabled.
- **Validation:** `Presence.details` ≤ 128 chars, `Presence.state` ≤ 128 chars (mirrors the existing status cap). Server rejects/over-cap drops.
- **Display:** member list shows activity when present, else the manual status (one line). Profile popup shows both.
- **No JS test runner.** Frontend "tests" = `cd client && npx tsc --noEmit` clean + pure helpers with inline test-notes. Rust = `cargo test`.
- **Cross-platform build:** the GSMTC source is `#[cfg(windows)]`; a non-Windows stub returns `None` so Linux/CI builds. The `PresenceSource` trait + `PresenceManager` are cross-platform and unit-tested on Linux with a mock source.
- **Server + protocol change → needs a sidecar rebuild to test** (not client-only).
- **Theming:** any new CSS class in all three themes (`discord-dark`, `hello-kitty`, `xp-luna-blue`), variable-driven, no hard-coded colors.
- **Spec:** `docs/superpowers/specs/2026-06-22-rich-presence-design.md`. Delete `presence-probe/` (on main) when this ships.

## File Structure
- `crates/farder-protocol/src/server.rs` — `PresenceKind`, `Presence`, `UpdatePresence`, `MemberPresenceUpdated`, `MemberInfo.presence`.
- `crates/farder-server/src/state.rs` — `presences` map + `presence_limiter`.
- `crates/farder-server/src/handlers.rs` — `UpdatePresence` handler + presence in `GetMembers`.
- `crates/farder-server/src/connection.rs` — clear presence in `cleanup_session`.
- `client/src-tauri/src/presence/mod.rs` (new) — `PresenceSource` trait + `PresenceManager` + push fns + a non-Windows mock/stub source.
- `client/src-tauri/src/presence/music_windows.rs` (new) — `#[cfg(windows)]` GSMTC source.
- `client/src-tauri/src/commands.rs` — presence settings get/set commands.
- `client/src-tauri/src/bridge.rs` — emit `server:member_presence_updated`.
- `client/src-tauri/src/main.rs` — register commands + spawn poller; `Cargo.toml` dep.
- `client/src/lib/types.ts` / `lib/presence.ts` / `hooks/useServerEvents.ts` / `context/ServerContext.tsx` / `components/MemberSidebar.tsx` / `components/UserProfilePopup.tsx` / `components/VoiceSettings.tsx` / `lib/tauri-bridge.ts` + themes.
- Docs: `docs/modules/` presence doc (+ GameSource contract), protocol/command/bridge docs.

---

### Task 1: Protocol — Presence types, messages, roster field

**Files:** Modify `crates/farder-protocol/src/server.rs`

**Interfaces — Produces:** `enum PresenceKind { Music, Game }`; `struct Presence { kind: PresenceKind, details: String, state: Option<String> }`; `ServerRequest::UpdatePresence { presence: Option<Presence> }`; `ServerEvent::MemberPresenceUpdated { public_key: PublicKey, presence: Option<Presence> }`; `MemberInfo.presence: Option<Presence>`.

- [ ] **Step 1: Add the types** (place near `TrackKind`, ~line 22):

```rust
/// What a member is doing right now (ephemeral activity). Source-agnostic so a
/// future game source produces the same shape as the music source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PresenceKind { Music, Game }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Presence {
    pub kind: PresenceKind,
    /// Primary line: music = track title; game = game name.
    pub details: String,
    /// Secondary line: music = artist; game = None (for now).
    pub state: Option<String>,
}
```

- [ ] **Step 2: Add the request variant** to `ServerRequest` (after `UpdateProfile`):

```rust
    /// Set or clear the sender's ephemeral presence (None clears it).
    UpdatePresence { presence: Option<Presence> },
```

- [ ] **Step 3: Add the event variant** to `ServerEvent` (after `MemberProfileUpdated`):

```rust
    /// A member's ephemeral presence changed (None = cleared/offline).
    MemberPresenceUpdated { public_key: PublicKey, presence: Option<Presence> },
```

- [ ] **Step 4: Add the roster field** to `MemberInfo` (after `profile_hash`):

```rust
    #[serde(default)]
    pub presence: Option<Presence>,
```

- [ ] **Step 5: Add a serde round-trip test** at the bottom of the file's `#[cfg(test)] mod tests` (or create one):

```rust
    #[test]
    fn presence_roundtrips() {
        let p = Presence { kind: PresenceKind::Music, details: "Song".into(), state: Some("Artist".into()) };
        let bytes = bincode::serialize(&p).unwrap();
        let back: Presence = bincode::deserialize(&bytes).unwrap();
        assert_eq!(p, back);
        // None clears
        let req = ServerRequest::UpdatePresence { presence: None };
        let b = bincode::serialize(&req).unwrap();
        let _back: ServerRequest = bincode::deserialize(&b).unwrap();
    }
```
(Use whatever serializer the crate's other tests use — match the existing test style/imports; if tests use `serde_json`, use that instead of bincode.)

- [ ] **Step 6: Build + test** — `cargo test -p farder-protocol` → passes (incl. the new test). `cargo build -p farder-protocol` clean.

- [ ] **Step 7: Commit**
```bash
git add crates/farder-protocol/src/server.rs
git commit -m "feat(presence): protocol types — Presence, UpdatePresence, MemberPresenceUpdated, MemberInfo.presence"
```

---

### Task 2: Server — store, validate, broadcast, roster, cleanup

**Files:** Modify `crates/farder-server/src/state.rs`, `handlers.rs`, `connection.rs`

**Interfaces — Consumes:** Task 1 types. **Produces:** server honors `UpdatePresence`, includes presence in `GetMembers`, clears on disconnect.

- [ ] **Step 1: Add presence state** to `ServerState` (`state.rs`, in the struct):

```rust
    pub presences: RwLock<HashMap<[u8; 32], farder_protocol::server::Presence>>,
    pub presence_limiter: RateLimiter,
```
And initialize them in `ServerState`'s constructor alongside the other fields:
```rust
            presences: RwLock::new(HashMap::new()),
            presence_limiter: RateLimiter::new(2, 1), // ≤2 presence updates/sec/user
```
(Match the constructor's existing init style; `RateLimiter::new(max_per_window, window_secs)` per `state.rs:33-66`.)

- [ ] **Step 2: Handle `UpdatePresence`** in `handlers.rs` (add an arm next to `UpdateProfile`, ~line 1615). The enclosing handler fn has the authenticated `member: &PublicKey` and the server `state` (the same scope that returns `BroadcastEvent`s). Use `member.as_bytes()` for the map key.

```rust
        ServerRequest::UpdatePresence { presence } => {
            let pk_bytes = *member.as_bytes();
            // Rate-limit; silently accept (no error) if over the cap.
            if !state.presence_limiter.allow(&pk_bytes) {
                return ok(ServerResponse::Ok);
            }
            // Validate field lengths.
            if let Some(p) = &presence {
                if p.details.chars().count() > 128
                    || p.state.as_ref().map_or(false, |s| s.chars().count() > 128)
                {
                    return Err(/* match the file's error type, e.g. */ "presence too long".into());
                }
            }
            {
                let mut map = state.presences.write().await; // or .unwrap() if std RwLock — match the field's lock type
                match &presence {
                    Some(p) => { map.insert(pk_bytes, p.clone()); }
                    None => { map.remove(&pk_bytes); }
                }
            }
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::MemberPresenceUpdated { public_key: member.clone(), presence },
                }],
            )
        }
```
NOTE: match the file's exact `RwLock` flavor (tokio `.write().await` vs std `.write().unwrap()`) and error-return convention — copy how `UpdateProfile` returns errors and how other `state.*.write()` sites lock. If the handler fn lacks `state` in scope, thread it in (most arms here have it).

- [ ] **Step 3: Include presence in `GetMembers`** (`handlers.rs:994-1018`). After the `profile_hash` line, read the map once and attach per member:

```rust
            let presences = state.presences.read().await; // match lock flavor
            // ... inside the per-member loop, when building MemberInfo:
                presence: presences.get(m.public_key.as_bytes()).cloned(),
```
Add `presence: ...` to the `MemberInfo { ... }` literal. (Read the map once before the loop; clone per member.)

- [ ] **Step 4: Clear presence on disconnect** (`connection.rs` `cleanup_session`, ~line 613). Before/after the existing `MemberLeft` broadcast, remove the entry and broadcast a clear:

```rust
    { state.presences.write().await.remove(&pk_bytes); } // match lock flavor; pk_bytes already in scope
    broadcast_event(
        state,
        EventTarget::All,
        ServerEvent::MemberPresenceUpdated { public_key: public_key.clone(), presence: None },
    ).await;
```
(Use the existing `broadcast_event` helper the same way `MemberLeft` is broadcast.)

- [ ] **Step 5: Tests** (`handlers.rs`/server test module). Add unit tests mirroring existing handler tests: (a) `UpdatePresence{Some}` stores it under the sender's key + returns a `MemberPresenceUpdated{All}` broadcast; (b) `UpdatePresence{None}` removes it; (c) over-128 details → error; (d) `GetMembers` includes a stored presence; (e) a presence set by member A is keyed to A (can't be set for B — the handler always uses the authenticated `member`). Use the file's existing test harness/fixtures.

- [ ] **Step 6: Build + test** — `cargo test -p farder-server` passes; `cargo build -p farder-server` clean.

- [ ] **Step 7: Commit**
```bash
git add crates/farder-server/src/state.rs crates/farder-server/src/handlers.rs crates/farder-server/src/connection.rs
git commit -m "feat(presence): server stores/validates/broadcasts presence, roster + disconnect-clear"
```

---

### Task 3: Client Rust — PresenceManager core + push + settings (cross-platform)

**Files:** Create `client/src-tauri/src/presence/mod.rs`; modify `commands.rs`; declare the module in `main.rs` (or `lib.rs`).

**Interfaces — Produces:** `trait PresenceSource { fn current(&self) -> Option<Presence>; }`; `struct PresenceManager`; `async fn push_presence_everywhere(state: &Arc<AppState>, presence: Option<Presence>)`; settings fns `read_presence_enabled() -> bool`, `read_presence_music() -> bool` + commands `get_presence_enabled`/`set_presence_enabled`/`get_presence_music`/`set_presence_music`.

This task is fully Linux-testable (no Windows API): the manager logic + a mock source + the push/dedup + settings.

- [ ] **Step 1: Settings commands** (`commands.rs`, mirroring `data_saver_embeds` at ~643):

```rust
pub(crate) fn read_presence_enabled() -> bool {
    settings_get("presence_enabled").and_then(|v| v.as_bool()).unwrap_or(false)
}
pub(crate) fn read_presence_music() -> bool {
    settings_get("presence_music").and_then(|v| v.as_bool()).unwrap_or(false)
}
#[tauri::command]
pub fn get_presence_enabled() -> bool { read_presence_enabled() }
#[tauri::command]
pub fn set_presence_enabled(enabled: bool) -> Result<(), String> { settings_set("presence_enabled", serde_json::json!(enabled)) }
#[tauri::command]
pub fn get_presence_music() -> bool { read_presence_music() }
#[tauri::command]
pub fn set_presence_music(enabled: bool) -> Result<(), String> { settings_set("presence_music", serde_json::json!(enabled)) }
```
(Match the actual `settings_get` return type — the Explore showed `settings_get` returns `Option<serde_json::Value>`; adapt the `.as_bool()` accordingly.)

- [ ] **Step 2: The presence module** — create `client/src-tauri/src/presence/mod.rs`:

```rust
use std::sync::Arc;
use farder_protocol::server::{Presence, ServerRequest, ServerResponse};
use crate::AppState;

/// A producer of the local user's current activity. Music now; a GameSource
/// (foreground app + per-app allowlist) plugs in later by implementing this.
pub trait PresenceSource: Send + Sync {
    /// The current activity, or None if nothing to report.
    fn current(&self) -> Option<Presence>;
}

/// Push presence to every connected server, de-duplicating per server so an
/// unchanged value is never re-sent (mirrors profile_sync's per-server dedup;
/// a newly-connected server receives the current value on the next poll tick).
pub async fn push_presence_everywhere(state: &Arc<AppState>, presence: Option<Presence>) {
    let ids: Vec<String> = state.servers.lock().unwrap().keys().cloned().collect();
    for id in ids {
        if last_pushed(&id) == presence { continue; }
        match crate::bridge::send_request(state, &id, ServerRequest::UpdatePresence { presence: presence.clone() }).await {
            Ok(ServerResponse::Ok) => record_pushed(&id, presence.clone()),
            _ => { /* transport/non-ok: leave dedup unset so we retry next tick */ }
        }
    }
}

// Per-server last-pushed presence (in-memory dedup). Use a Mutex<HashMap<String, Option<Presence>>>.
use std::sync::Mutex;
use std::collections::HashMap;
static PUSHED: Mutex<Option<HashMap<String, Option<Presence>>>> = Mutex::new(None);
fn last_pushed(server_id: &str) -> Option<Presence> {
    PUSHED.lock().unwrap().as_ref().and_then(|m| m.get(server_id).cloned()).flatten()
}
fn record_pushed(server_id: &str, presence: Option<Presence>) {
    PUSHED.lock().unwrap().get_or_insert_with(HashMap::new).insert(server_id.to_string(), presence);
}

/// Polls the enabled sources, merges by priority (only music today; game would
/// take priority when added), and pushes on change. Run from a background task.
pub struct PresenceManager {
    sources: Vec<Box<dyn PresenceSource>>,
}
impl PresenceManager {
    pub fn new(sources: Vec<Box<dyn PresenceSource>>) -> Self { Self { sources } }
    /// Compute the current presence honoring the settings gates.
    pub fn compute(&self) -> Option<Presence> {
        if !crate::commands::read_presence_enabled() { return None; }
        if !crate::commands::read_presence_music() { return None; } // only music source exists; when games arrive, gate per source
        for s in &self.sources {
            if let Some(p) = s.current() { return Some(p); }
        }
        None
    }
}
```
NOTE: `PUSHED` as a `Mutex<Option<HashMap>>` avoids needing `once_cell`/`lazy_static`; if the crate already uses `once_cell`/`std::sync::OnceLock`, prefer that. Match `send_request`'s real signature/return from `bridge.rs`/`profile_sync.rs`.

- [ ] **Step 3: Mock source + manager tests** (in `presence/mod.rs` `#[cfg(test)]`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    struct Mock(Option<Presence>);
    impl PresenceSource for Mock { fn current(&self) -> Option<Presence> { self.0.clone() } }

    #[test]
    fn compute_returns_first_some() {
        let p = Presence { kind: farder_protocol::server::PresenceKind::Music, details: "x".into(), state: None };
        let m = PresenceManager::new(vec![Box::new(Mock(None)), Box::new(Mock(Some(p.clone())))]);
        // settings gate is read from disk; this test documents source-merge order.
        // (Gating is covered by the settings unit; here assert the first Some wins among sources.)
        let first = m.sources.iter().find_map(|s| s.current());
        assert_eq!(first, Some(p));
    }
}
```
(Keep tests Linux-runnable: don't call `compute()` if it depends on settings-on-disk in CI; test the source-merge directly as shown. Add a `dedup` reasoning note in a comment.)

- [ ] **Step 4: Declare the module** — add `mod presence;` where the other `mod` declarations live (`main.rs`/`lib.rs`).

- [ ] **Step 5: Build + test** — `cd client/src-tauri && cargo test presence::` passes; `cargo build` clean (Linux).

- [ ] **Step 6: Commit**
```bash
git add client/src-tauri/src/presence/mod.rs client/src-tauri/src/commands.rs client/src-tauri/src/main.rs
git commit -m "feat(presence): client PresenceManager + push/dedup + settings (cross-platform core)"
```

---

### Task 4: Client Rust — Windows GSMTC source, poller, command registration, bridge event, Cargo dep

**Files:** Create `client/src-tauri/src/presence/music_windows.rs`; modify `presence/mod.rs` (source factory), `main.rs` (deps already?), `Cargo.toml`, `bridge.rs`, `main.rs` (generate_handler! + setup poller).

**Interfaces — Consumes:** Task 3 manager/push; Task 1 types. **Produces:** live presence on Windows.

- [ ] **Step 1: Cargo dep** — in `client/src-tauri/Cargo.toml` `[target.'cfg(windows)'.dependencies]` add:
```toml
windows = { version = "0.58", features = ["Media_Control", "Foundation"] }
```

- [ ] **Step 2: GSMTC source** — create `client/src-tauri/src/presence/music_windows.rs` (mirrors the proven probe):

```rust
#![cfg(windows)]
use farder_protocol::server::{Presence, PresenceKind};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as Mgr,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};

pub struct MusicSource;
impl crate::presence::PresenceSource for MusicSource {
    fn current(&self) -> Option<Presence> {
        read_now_playing().ok().flatten()
    }
}

fn read_now_playing() -> windows::core::Result<Option<Presence>> {
    let mgr = Mgr::RequestAsync()?.get()?;
    let session = match mgr.GetCurrentSession() { Ok(s) => s, Err(_) => return Ok(None) };
    if session.GetPlaybackInfo()?.PlaybackStatus()? != Status::Playing { return Ok(None); }
    let props = session.TryGetMediaPropertiesAsync()?.get()?;
    let title = props.Title()?.to_string();
    if title.trim().is_empty() { return Ok(None); }
    let artist = props.Artist().ok().map(|h| h.to_string()).filter(|s| !s.trim().is_empty());
    Some(Presence { kind: PresenceKind::Music, details: title, state: artist }).pipe_ok()
}
trait PipeOk: Sized { fn pipe_ok(self) -> windows::core::Result<Option<Presence>>; }
impl PipeOk for Option<Presence> { fn pipe_ok(self) -> windows::core::Result<Option<Presence>> { Ok(self) } }
```
(If the `pipe_ok` helper feels awkward, just write `Ok(Some(Presence { ... }))` inline — the helper is only to keep the `?` flow tidy. Probe confirmed `Title()/Artist()` are `HSTRING` with `.to_string()`, and `PlaybackStatus::Playing`.)

- [ ] **Step 3: Source factory** — in `presence/mod.rs`, add a constructor that wires the platform source:
```rust
pub fn default_sources() -> Vec<Box<dyn PresenceSource>> {
    #[cfg(windows)]
    { vec![Box::new(crate::presence::music_windows::MusicSource)] }
    #[cfg(not(windows))]
    { Vec::new() } // no detection off Windows; manager yields None
}
```
And add `#[cfg(windows)] pub mod music_windows;` to `presence/mod.rs`.

- [ ] **Step 4: Spawn the poller** — in `main.rs` `.setup()` (where the deep-link task spawns), start a 5s loop:
```rust
    {
        let state = state.clone(); // the Arc<AppState> managed by Tauri (match how state is obtained in setup)
        tauri::async_runtime::spawn(async move {
            let mgr = presence::PresenceManager::new(presence::default_sources());
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let current = mgr.compute();
                presence::push_presence_everywhere(&state, current).await;
            }
        });
    }
```
(Match how `setup` accesses the `Arc<AppState>` — use the same handle other spawned tasks use.)

- [ ] **Step 5: Register commands** — add to `generate_handler![ ... ]` in `main.rs`:
```rust
    commands::get_presence_enabled,
    commands::set_presence_enabled,
    commands::get_presence_music,
    commands::set_presence_music,
```

- [ ] **Step 6: Bridge event emit** — in `client/src-tauri/src/bridge.rs`, find where `ServerEvent::MemberProfileUpdated` is translated into a Tauri emit (event name `"server:member_profile_updated"`). Add an arm for the new event emitting `"server:member_presence_updated"` with a payload carrying the data the frontend needs:
```rust
        ServerEvent::MemberPresenceUpdated { public_key, presence } => {
            let _ = emit(/* same emitter/window the others use */, "server:member_presence_updated",
                serde_json::json!({
                    "server_id": server_id,                 // match how other arms pass the server id
                    "public_key": public_key,               // serialize like other arms (e.g. {bytes:[...]})
                    "presence": presence,
                }));
        }
```
(Mirror EXACTLY how the neighboring arms serialize `public_key` and pass `server_id`/the emitter — copy that arm's shape.)

- [ ] **Step 7: Build** — `cd client/src-tauri && cargo build` clean on Linux (windows code cfg-gated, uncompiled). Confirm `mod presence` + `default_sources` + commands compile. Seam check: each new `commands::*` in `generate_handler!` exists as a `#[tauri::command]`.

- [ ] **Step 8: Commit**
```bash
git add client/src-tauri/src/presence/ client/src-tauri/src/main.rs client/src-tauri/src/bridge.rs client/src-tauri/Cargo.toml client/src-tauri/Cargo.lock
git commit -m "feat(presence): Windows GSMTC music source + 5s poller + command/bridge wiring"
```

---

### Task 5: Frontend — types, state, event listener, formatter

**Files:** Modify `client/src/lib/types.ts`, create `client/src/lib/presence.ts`, modify `client/src/lib/tauri-bridge.ts`, `client/src/hooks/useServerEvents.ts`, `client/src/context/ServerContext.tsx`.

**Interfaces — Produces:** `Presence` TS type + `MemberInfo.presence`; `formatPresence(p)`; an `UPDATE_MEMBER_PRESENCE` reducer action; the presence settings bridge fns.

- [ ] **Step 1: Types** — in `lib/types.ts`:
```ts
export type PresenceKind = "Music" | "Game";
export interface Presence { kind: PresenceKind; details: string; state?: string | null }
```
And add to `MemberInfo`: `presence?: Presence | null;`

- [ ] **Step 2: Formatter** — create `client/src/lib/presence.ts`:
```ts
import type { Presence } from "./types";
/** Render a presence as a single line, e.g. "🎵 Listening to Song – Artist".
 * Test-notes (by inspection):
 *   {Music,"S","A"} -> "🎵 Listening to S – A"
 *   {Music,"S",null} -> "🎵 Listening to S"
 *   {Game,"Valorant",null} -> "🎮 Playing Valorant"
 */
export function formatPresence(p: Presence): string {
  if (p.kind === "Music") {
    return p.state ? `🎵 Listening to ${p.details} – ${p.state}` : `🎵 Listening to ${p.details}`;
  }
  return `🎮 Playing ${p.details}`;
}
```

- [ ] **Step 3: Bridge settings fns** — in `lib/tauri-bridge.ts`, add (mirror existing `getDataSaverEmbeds`/`setDataSaverEmbeds`):
```ts
export const getPresenceEnabled = () => invoke<boolean>("get_presence_enabled");
export const setPresenceEnabled = (enabled: boolean) => invoke<void>("set_presence_enabled", { enabled });
export const getPresenceMusic = () => invoke<boolean>("get_presence_music");
export const setPresenceMusic = (enabled: boolean) => invoke<void>("set_presence_music", { enabled });
```

- [ ] **Step 4: Reducer action** — in `ServerContext.tsx`, add to the action union and a case:
```ts
  | { type: "UPDATE_MEMBER_PRESENCE"; serverId: string; payload: { publicKey: string; presence: Presence | null } }
```
```ts
    case "UPDATE_MEMBER_PRESENCE":
      return {
        ...state,
        members: state.members.map((m) =>
          publicKeyToString(m.public_key) === action.payload.publicKey
            ? { ...m, presence: action.payload.presence }
            : m,
        ),
      };
```
(This is in the per-server reducer where `SET_MEMBERS` lives; use the existing `publicKeyToString` helper. Import `Presence` type.)

- [ ] **Step 5: Event listener** — in `useServerEvents.ts`, next to the `member_profile_updated` listener, add:
```ts
listen("server:member_presence_updated", (e) => {
  const data = e.payload as { server_id: string; public_key: { bytes: number[] }; presence: Presence | null };
  dispatch({
    type: "UPDATE_MEMBER_PRESENCE",
    serverId: data.server_id,
    payload: { publicKey: publicKeyToString(data.public_key), presence: data.presence },
  });
}).then(safePush);
```
(Match how the file converts `public_key` to a string — reuse the same `publicKeyToString`/helper the other listeners use; match the payload shape the bridge emits in Task 4 Step 6.)

- [ ] **Step 6: Type-check** — `cd client && npx tsc --noEmit` clean.

- [ ] **Step 7: Commit**
```bash
git add client/src/lib/types.ts client/src/lib/presence.ts client/src/lib/tauri-bridge.ts client/src/hooks/useServerEvents.ts client/src/context/ServerContext.tsx
git commit -m "feat(presence): frontend types, formatter, presence event listener + reducer"
```

---

### Task 6: Frontend UI — member list, profile popup, settings toggles, theming

**Files:** Modify `client/src/components/MemberSidebar.tsx`, `UserProfilePopup.tsx`, `VoiceSettings.tsx`, themes ×3.

**Interfaces — Consumes:** Task 5 `formatPresence`, `presence` field, bridge settings fns.

- [ ] **Step 1: Member row** — in `MemberSidebar.tsx` `MemberRow`, show activity OVER status. Import `formatPresence`. Replace the status line:
```tsx
        {member.presence
          ? <span className="member-presence">{formatPresence(member.presence)}</span>
          : status && <span className="member-status">{status}</span>}
```

- [ ] **Step 2: Profile popup** — in `UserProfilePopup.tsx`, show BOTH (activity + manual status). Where the status renders, add above/below it:
```tsx
        {member.presence && <div className="profile-presence">{formatPresence(member.presence)}</div>}
```
(Import `formatPresence`; keep the existing status display.)

- [ ] **Step 3: Settings toggles** — in `VoiceSettings.tsx` Privacy & Data section, mirror the embed toggles. Add state + handlers (load via `getPresenceEnabled`/`getPresenceMusic` in the mount effect; save via `setPresenceEnabled`/`setPresenceMusic`), then:
```tsx
        <label className="settings-row">
          <input type="checkbox" checked={presenceEnabled} onChange={(e) => choosePresenceEnabled(e.target.checked)} />
          Share my activity (let others see what you're doing)
        </label>
        <label className="settings-row">
          <input type="checkbox" checked={presenceMusic} disabled={!presenceEnabled} onChange={(e) => choosePresenceMusic(e.target.checked)} />
          Share music I'm playing
        </label>
        <p className="settings-help">
          Off by default. When on, members on your servers see your current activity
          (e.g. the song you're playing). Turn off any time.
        </p>
```

- [ ] **Step 4: Theming** — add to ALL THREE `client/src/themes/*/theme.css`:
```css
.member-presence { display: block; font-size: 0.75em; color: var(--xp-text-secondary); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.profile-presence { font-size: 0.85em; color: var(--xp-text-normal, var(--xp-text-secondary)); margin: 2px 0; }
```
(Match the existing `.member-status` rule's look in each theme — copy its sizing/color so presence reads as the same kind of subtext.)

- [ ] **Step 5: Verify** — `cd client && npx tsc --noEmit` clean; `grep -l "member-presence" client/src/themes/*/theme.css` lists all three.

- [ ] **Step 6: Commit**
```bash
git add client/src/components/MemberSidebar.tsx client/src/components/UserProfilePopup.tsx client/src/components/VoiceSettings.tsx client/src/themes/*/theme.css
git commit -m "feat(presence): member-list + popup activity rendering, settings toggles, theming ×3"
```

---

### Task 7: Documentation

**Files:** Create `docs/modules/presence.md`; update `docs/modules/tauri-commands.md`, `tauri-bridge.md`, the protocol module doc, `ARCHITECTURE.md` if warranted.

- [ ] **Step 1: Module doc** — create `docs/modules/presence.md` describing: the `Presence` model; the ephemeral unsigned channel (UpdatePresence/MemberPresenceUpdated/MemberInfo.presence + in-memory server map, no DB, disconnect-clear); the client `PresenceSource`/`PresenceManager` seam + the 5s poller + settings gates; **the GameSource contract** (a future game source implements `PresenceSource::current()` returning `Presence{kind:Game,details:game name}`, gated by a `presence_games` setting + a per-app allowlist, merged with priority game > music); the Windows-only `windows`=0.58 GSMTC dep; UNVERIFIED-at-runtime status.

- [ ] **Step 2: Command/bridge/protocol docs** — add the 4 presence commands to `tauri-commands.md` (+ their `invoke` names in `tauri-bridge.ts`), the `server:member_presence_updated` event + payload to `tauri-bridge.md` and its `useServerEvents` listener, and the new protocol messages/fields to the protocol doc.

- [ ] **Step 3: Commit**
```bash
git add docs/
git commit -m "docs(presence): module doc (+ GameSource contract), command/bridge/protocol updates"
```

---

## Final verification (before declaring done in code)
- [ ] `cargo build --workspace` + `cargo test -p farder-protocol -p farder-server` pass; `cd client/src-tauri && cargo build` (Linux) clean; `cd client && npx tsc --noEmit` clean.
- [ ] Seam: every new `commands::*` in `generate_handler!` has a matching `#[tauri::command]`; the bridge emits `server:member_presence_updated` and `useServerEvents` listens for it.
- [ ] `grep -l "member-presence" client/src/themes/*/theme.css` → all three.
- [ ] Presence is never written to the DB (no schema/migration touched); server stores it only in `state.presences`.
- [ ] Spec coverage walk: source-agnostic Presence (T1) ✓; ephemeral unsigned channel + roster + disconnect-clear + rate-limit + own-pk stamp (T1/T2) ✓; PresenceSource/Manager seam + push/dedup + settings (T3) ✓; Windows GSMTC music source + poller + bridge event + dep lock (T4) ✓; opt-in toggles off-by-default (T3/T6) ✓; activity-over-status list + both in popup (T6) ✓; theming ×3 (T6) ✓; docs + GameSource contract (T7) ✓.
- [ ] **Runtime (owner, Windows, 2 clients — UNVERIFIED until run; REBUILD THE SERVER SIDECAR + both clients):** enable "Share my activity" + "Share music" on A, play a song → B sees "🎵 Listening to <song> – <artist>" in A's member-list row (over A's manual status) and both lines in A's profile popup; pause/stop → clears within ~5s; change song → updates; toggle off → clears; A disconnect → clears on B; works relayed + direct. Then delete `presence-probe/`.
```

