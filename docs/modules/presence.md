# Rich Presence / Activity Status

> **File(s):**
> - `client/src-tauri/src/presence/mod.rs` — `PresenceSource` trait, `PresenceManager`, `push_presence_everywhere`
> - `client/src-tauri/src/presence/music_windows.rs` — Windows GSMTC music source (`#[cfg(windows)]`)
> - `client/src-tauri/src/commands.rs` — presence settings commands
> - `client/src-tauri/src/main.rs` — 5s poller setup, `generate_handler!` registration
> - `crates/farder-protocol/src/server.rs` — `Presence`, `PresenceKind`, `UpdatePresence`, `MemberPresenceUpdated`, `MemberInfo.presence`
> - `crates/farder-server/src/state.rs` — `ServerState.presences` in-memory map
> - `crates/farder-server/src/handlers.rs` — `UpdatePresence` handler, rate limiter, disconnect cleanup
> - `client/src/lib/types.ts` — `Presence`, `PresenceKind`, `MemberInfo.presence`
> - `client/src/lib/presence.ts` — `formatPresence`
> - `client/src/lib/tauri-bridge.ts` — presence settings wrappers
> - `client/src/hooks/useServerEvents.ts` — `MemberPresenceUpdated` listener
> - `client/src/context/ServerContext.tsx` — `UPDATE_MEMBER_PRESENCE` reducer
> - `client/src/components/MemberSidebar.tsx`, `UserProfilePopup.tsx` — rendering
> - `client/src/components/VoiceSettings.tsx` — opt-in toggles
>
> **Layer:** Protocol + Server + Client (Rust) + Client (TypeScript/React)
> **Last reviewed:** 2026-06-23
> **Verification status:** UNVERIFIED at runtime (requires owner's Windows 2-client run + server sidecar rebuild)

## Purpose

Shows what a member is currently doing — automatically — in the member list: "🎵 Listening to Song – Artist" (music phase), later "🎮 Playing Game" (games phase). Privacy-first: opt-in, off by default, no persistence.

The architecture is source-agnostic: the `PresenceSource` trait is the only contract a new source must satisfy. Adding game detection requires no protocol or UI changes.

---

## The `Presence` model

Defined in `crates/farder-protocol/src/server.rs`:

```rust
pub enum PresenceKind { Music, Game }

pub struct Presence {
    pub kind: PresenceKind,
    pub details: String,      // music: track title; game: game name
    pub state: Option<String>, // music: artist name; game: None for now
}
```

Validation: `details` and `state` are each capped at 128 characters (the server rejects longer values with an error response). `Presence` derives `PartialEq` and `Clone` — both are required by the per-server dedup logic in `PresenceManager`.

The TypeScript mirror (`client/src/lib/types.ts`):

```typescript
export type PresenceKind = "Music" | "Game";
export interface Presence { kind: PresenceKind; details: string; state?: string | null }
```

---

## Transport — ephemeral, unsigned channel

Presence uses its own lightweight channel, separate from the signed profile pipeline, because presence values change frequently and re-signing the full profile (avatar included) on every track change would be wrong.

### Protocol messages

- `ServerRequest::UpdatePresence { presence: Option<Presence> }` — the client sends this to set or clear its presence. `None` clears it.
- `ServerEvent::MemberPresenceUpdated { public_key: PublicKey, presence: Option<Presence> }` — the server broadcasts this to all members when any member's presence changes (including clears). `public_key` is always the authenticated sender's key, never a value supplied by the client.
- `MemberInfo.presence: Option<Presence>` — added to the roster struct (`#[serde(default)]`, backward-compatible). `GetMembers` includes each member's current presence so late joiners see the full picture immediately.

### Server storage

`ServerState.presences: StdRwLock<HashMap<[u8; 32], Presence>>` — an **in-memory map**, keyed by the sender's 32-byte public key. **No database writes; no migrations; cleared on process restart.**

On `UpdatePresence` from an authenticated session:

1. The server stamps the sender's own authenticated public key (a client cannot forge another member's presence).
2. Field lengths are validated (max 128 chars each); excess is rejected with `ServerResponse::Error`.
3. The rate limiter is checked: `presence_limiter` is `RateLimiter::new(2, 1)` — at most **2 updates per second per member**. Excess is dropped (`ServerResponse::Error`).
4. The map entry is set (Some) or removed (None).
5. `MemberPresenceUpdated` is broadcast to all members.

On disconnect/session cleanup: the member's map entry is removed and `MemberPresenceUpdated { presence: None }` is broadcast.

### Why unsigned?

Unlike identity/display-name/avatar/status (signed to prevent a malicious server from forging your identity), presence is ephemeral and low-stakes. Only the server itself could forge it — the same trust boundary as community content the server already sees. The trade keeps the channel lightweight.

---

## Client: PresenceSource trait and PresenceManager

### `PresenceSource` trait

`client/src-tauri/src/presence/mod.rs`:

```rust
pub trait PresenceSource: Send + Sync {
    fn current(&self) -> Option<Presence>;
}
```

This is the only interface a new presence source must implement. The `MusicSource` is the sole implementation today.

### `PresenceManager`

```rust
pub struct PresenceManager {
    pub sources: Vec<Box<dyn PresenceSource>>,
}
```

`compute() -> Option<Presence>`: iterates the sources in order and returns the first `Some`. Before iterating, checks both settings gates (see below). The merge priority when multiple sources are active: **first source in the `sources` vec wins**. Today only one source exists; the documented order for when games arrives is **game > music** (game source placed first in `default_sources()`).

### `default_sources()`

Platform factory: on Windows, returns `[Box::new(MusicSource)]`; on non-Windows, returns an empty `Vec`. This is the only `cfg(windows)` branch in the manager — the trait, `PresenceManager`, and `push_presence_everywhere` are all cross-platform.

### 5-second poller

Spawned in `client/src-tauri/src/main.rs` setup:

```rust
let mgr = presence::PresenceManager::new(presence::default_sources());
// tokio::spawn loop, 5s interval:
let current = mgr.compute();
presence::push_presence_everywhere(&state, current).await;
```

### Settings gates

Two settings in `~/.farder/settings.json`:

- `presence_enabled` (bool, default `false`) — master "Share my activity" toggle.
- `presence_music` (bool, default `false`) — "Share music" per-source toggle.

`PresenceManager::compute()` returns `None` (and `push_presence_everywhere` sends a clear) when either is false. The poller does not stop when toggled off — it keeps running but the `compute()` output becomes `None`, which triggers a clear on the next tick.

### `push_presence_everywhere`

```rust
pub async fn push_presence_everywhere(state: &Arc<AppState>, presence: Option<Presence>)
```

Iterates all connected servers. Per-server dedup: if `presence == last_pushed_for(server_id)`, the request is skipped. On `ServerResponse::Ok`, the dedup entry is updated. On failure the entry is left unchanged so the next poll tick retries.

---

## Windows GSMTC music source

`client/src-tauri/src/presence/music_windows.rs` (`#[cfg(windows)]` — never compiled on Linux/macOS):

```rust
pub struct MusicSource;
impl PresenceSource for MusicSource { ... }
```

Uses the `windows` crate `0.58` (`Cargo.toml`: `[target.'cfg(windows)'.dependencies]`), features: `Media_Control`, `Foundation`.

API path:
`GlobalSystemMediaTransportControlsSessionManager::RequestAsync()` → `GetCurrentSession()` → `GetPlaybackInfo()?.PlaybackStatus()` → if `Playing`, read `TryGetMediaPropertiesAsync()?.Title()` and `Artist()`.

Returns `Presence { kind: Music, details: title, state: Some(artist) }` when a session is actively playing; `None` when paused, stopped, or no session exists. COM/API errors are logged at debug level and return `None` — the manager never crashes.

Works for Spotify, Chrome, Firefox, and any media app that registers a GSMTC session.

---

## Bridge event

The bridge (`client/src-tauri/src/bridge.rs`) maps the `ServerEvent::MemberPresenceUpdated` to the Tauri event `server:member_presence_updated`. See `docs/modules/tauri-bridge.md` for the complete payload specification.

`useServerEvents.ts` listens for this event and dispatches:

```typescript
{ type: "UPDATE_MEMBER_PRESENCE", serverId, payload: { publicKey, presence } }
```

`ServerContext.tsx` reducer (`UPDATE_MEMBER_PRESENCE`): finds the matching member in `state.servers[serverId].members` by `publicKey` (`"vk_<hex>"` string) and replaces their `presence` field. `null` presence clears the activity display.

---

## Frontend rendering

### `formatPresence` (`client/src/lib/presence.ts`)

```typescript
export function formatPresence(p: Presence): string
```

- `{ kind: "Music", details: "Song", state: "Artist" }` → `"🎵 Listening to Song – Artist"`
- `{ kind: "Music", details: "Song", state: null }` → `"🎵 Listening to Song"`
- `{ kind: "Game", details: "Valorant", state: null }` → `"🎮 Playing Valorant"`

### Member list (`MemberSidebar.tsx`)

Activity takes priority over the manual status: if `member.presence` is set, `formatPresence(member.presence)` is shown instead of `member.status`. One line only.

### Profile popup (`UserProfilePopup.tsx`)

Both are shown: the activity line (if set) and the manual status line. Allows the user to see both at once.

---

## GameSource contract (future games phase)

The game detection source is **not implemented**; it is designed for as a drop-in. The exact contract a future `GameSource` must satisfy:

1. **Trait implementation:** `GameSource` must implement `PresenceSource`:
   ```rust
   fn current(&self) -> Option<Presence>
   ```
   Return `Presence { kind: PresenceKind::Game, details: <game name as String>, state: None }` when a game from the allowlist is in the foreground; `None` otherwise.

2. **Detection method:** a Windows foreground-app detector (e.g. `GetForegroundWindow` / `GetWindowText` + process name via `QueryFullProcessImageName`) reads the active window/process name every poll tick and matches it against the allowlist.

3. **Allowlist:** a per-app allowlist (`presence_game_allowlist: Vec<String>`) stored in `settings.json` (or a separate file). The user must explicitly add an executable name or window title pattern. No game is shared without the user adding it. The Settings UI (Privacy & Data) must include an allowlist editor.

4. **Settings gate:** a new boolean setting `presence_games` (default `false`) in `settings.json`. `PresenceManager::compute()` must check this gate before calling `GameSource::current()`.

5. **Merge priority:** `default_sources()` must return `[Box::new(GameSource), Box::new(MusicSource)]` — game source first so game activity takes priority over music when both are active.

6. **No protocol or UI changes required:** the `Presence { kind: Game, ... }` value already round-trips through the protocol and `formatPresence` already handles `kind == "Game"`. The only additions needed are the source implementation, the allowlist setting, and the `presence_games` gate.

---

## Data flow summary

```
User enables "Share my activity" + "Share music"
  -> PresenceManager starts (poller already running; first non-None tick)
  -> MusicSource::current() reads GSMTC every 5s
  -> PresenceManager::compute() returns Some(Presence{Music, title, artist})
  -> push_presence_everywhere sends UpdatePresence to each server
  -> Server: stamp pk, validate, store in presences map, broadcast MemberPresenceUpdated
  -> Other clients: bridge emits server:member_presence_updated
  -> useServerEvents dispatches UPDATE_MEMBER_PRESENCE
  -> ServerContext updates member.presence
  -> MemberSidebar shows activity over manual status; popup shows both

Pause / toggle off / disconnect
  -> compute() returns None -> UpdatePresence{None} sent
  -> Server removes map entry, broadcasts MemberPresenceUpdated{presence: None}
  -> Other clients clear the activity display
```

---

## Testing

- **Protocol serde:** `presence_roundtrips()` in `crates/farder-protocol/src/server.rs`.
- **Server unit tests** (`crates/farder-server/src/handlers.rs`): `test_update_presence_some_stores_and_broadcasts`, `test_update_presence_none_removes_and_broadcasts`, `test_update_presence_details_too_long_returns_error`, `test_update_presence_state_too_long_returns_error`, `test_get_members_includes_stored_presence`, `test_update_presence_keyed_to_authenticated_sender`.
- **Client dedup tests** (`client/src-tauri/src/presence/mod.rs`): `compute_returns_first_some`, `presence_equality_for_dedup`, `dedup_record_and_read`.
- **Runtime (UNVERIFIED):** requires owner, Windows, 2 clients, server sidecar rebuild. See task-7-brief for the full runtime checklist.
