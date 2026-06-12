# Profile sync

> **File(s):** `client/src-tauri/src/profile_sync.rs`
> **Layer:** Tauri command (support module — no `#[tauri::command]` directly; called by `commands.rs`)
> **Last reviewed:** 2026-06-11

## Purpose

`profile_sync.rs` owns the client side of identity-signed profile replication.
It builds per-server **effective profiles** (avatar + status + display name),
signs them with the local Ed25519 key, and pushes them to connected servers via
`ServerRequest::UpdateProfile`. It also validates incoming avatar bytes on the
client before they are stored, so a bad image cannot poison future push attempts.
What it deliberately does NOT do: it never contacts a server from the UI thread;
all network calls are spawned or awaited by the caller in `commands.rs`.

---

## Public interface

### `effective_avatar_bytes(server_id) -> Option<Vec<u8>>`

**What it does:** returns the raw image bytes to use as the avatar for
`server_id`. Resolution order: per-server override file first (if it exists),
then the global `avatar.png`, then `None`.
**Side effects:** none (read-only disk access).
**Connects to:** `override_path(server_id)` — `~/.farder/profile_overrides/<safe_server_id>.img`; `farder_data_dir()/avatar.png`.

---

### `build_signed_profile(keypair, server_id) -> SignedProfile`

**What it does:** assembles the effective profile for `server_id` — reads
`display_name` and `status` from `~/.farder/profile.json`, calls
`effective_avatar_bytes`, and creates a fresh `SignedProfile` (the keypair signs
a canonical serialization of the profile fields).
**Returns:** a `SignedProfile` ready to serialize with `.to_bytes()`.
**Side effects:** read-only disk access.
**Connects to:** `farder_crypto::profile::SignedProfile::create`.

---

### `validate_avatar_bytes(data) -> Result<(), String>`

**What it does:** client-side pre-check that mirrors the server's avatar rules.
Rejects bytes larger than 2 MB and rejects images that don't start with a
recognized magic signature (PNG `\x89PNG`, JPEG `\xFF\xD8\xFF`, GIF `GIF8`,
WebP `RIFF…WEBP`). A bad avatar would otherwise be stored locally and poison
every subsequent profile push (the server rejects the entire signed blob).
**Side effects:** none (in-memory inspection only).
**Connects to:** called by `commands::set_avatar` and
`commands::set_server_avatar_override` before writing to disk.

---

### `push_profile(state, server_id) -> Result<(), String>` (async)

**What it does:** builds the effective signed profile for `server_id`, computes
its SHA-256 hash, and compares it against the last successfully pushed hash
stored in `pushed_profiles.json`. If the profile is unchanged the function
returns immediately (no network I/O). If the profile changed, it sends
`ServerRequest::UpdateProfile { profile: bytes }` to the server and, on
`ServerResponse::Ok`, records the new hash in `pushed_profiles.json`.

**Special case:** if the identity is currently locked (`signing_key_bytes` is
`None`) the function returns `Ok(())` immediately without touching the network
or the pushed map.
**Side effects:** may write to `~/.farder/pushed_profiles.json`; may send one
`UpdateProfile` request (network I/O).

---

### `push_profile_everywhere(state)` (async)

**What it does:** iterates every server id currently in `AppState::servers` and
calls `push_profile` on each. Each server gets its own effective profile (with
its own avatar override), so per-server overrides are respected. Errors per
server are logged to stderr but do not abort the remaining servers.
**Side effects:** calls `push_profile` for each connected server.
**Connects to:** called by `commands::set_avatar` and
`commands::set_profile_status` (spawned as background tokio tasks).

---

### `push_profile_on_connect(state, server_id) -> Result<(), String>` (async)

**What it does:** a connect-time variant of `push_profile` that uses the
**server's own stored hash** as the ground truth instead of the local
`pushed_profiles.json`. This defends against a server whose DB was wiped: the
local pushed map would still show the old hash (indicating "already synced") and
the profile would never be re-sent; `push_profile_on_connect` closes that gap.

Procedure:
1. Fetches `ServerRequest::GetMembers` and looks up our own `profile_hash` in
   the roster.
2. If the server's hash matches the local effective profile — record the hash
   locally and return (no extra push needed).
3. If the server's hash is different (or our key isn't in the roster yet) —
   push unconditionally.
4. If `GetMembers` fails (roster unavailable) — fall back to the normal
   `push_profile` (local pushed-map logic).

**Side effects:** same as `push_profile` on a miss (writes `pushed_profiles.json`,
sends `UpdateProfile`); additionally sends one `GetMembers` request.
**Connects to:** called by `commands::connect_server` after the connection is established.

---

## State it owns

| Field / variable | Type | What it tracks, when it's mutated |
|---|---|---|
| `~/.farder/pushed_profiles.json` | disk (JSON object) | Map of `server_id → last-pushed profile hash (hex)`. Written by `record_pushed`; read by `push_profile` and `push_profile_on_connect`. |
| `~/.farder/profile_overrides/<safe_id>.img` | disk (raw bytes) | Per-server avatar override; written by `set_server_avatar_override` in `commands.rs`, deleted by `clear_server_avatar_override`. |

The module does NOT own `avatar.png`, `profile.json`, or the `profile_cache/`
directory (those belong to `commands.rs`).

---

## Events emitted

None. Profile pushes are responses to `UpdateProfile` requests; the server
broadcasts `MemberProfileUpdated` to all clients, which `bridge.rs` re-emits as
`server:member_profile_updated`. That event is not emitted from this module.

## Events / requests consumed

| Event / request | Source | What this module does with it |
|---|---|---|
| `ServerResponse::Ok` (reply to `UpdateProfile`) | server | `record_pushed` called to persist the new hash |
| `ServerResponse::Members` (reply to `GetMembers`) | server | `push_profile_on_connect` reads `profile_hash` for our own key |

---

## Integration map

- **`commands.rs`** — calls `validate_avatar_bytes` before storing images;
  calls `push_profile_everywhere` (spawned) after `set_avatar` or
  `set_profile_status`; calls `push_profile` (awaited) after
  `set_server_avatar_override` / `clear_server_avatar_override`; calls
  `push_profile_on_connect` after `connect_server`.
- **`farder_crypto::profile`** — `SignedProfile::create` / `to_bytes` /
  `from_bytes` / `verify`; `profile_hash_hex` (SHA-256 of the serialized blob).
- **`bridge.rs`** — receives `ServerResponse` from `send_request` calls made by
  `push_profile` / `push_profile_on_connect`.
- **`state::AppState`** — reads `signing_key_bytes` (to build the keypair) and
  `servers` (to enumerate server ids in `push_profile_everywhere`).

---

## Known gotchas

- **`pushed_profiles.json` is not ground truth.** It tracks what *we* last
  pushed successfully; the server's stored hash is authoritative. Always use
  `push_profile_on_connect` (not `push_profile`) at connect time. Using the
  wrong variant is exactly the class of bug that leads to "profile only shows
  after manually re-setting it".
- **safe_server_name sanitization:** `:`, `.`, and `/` are replaced with `_` to
  produce a safe filename for the override file. A `server_id` like
  `"192.168.1.1:7070"` becomes `"192_168_1_1_7070"`. Two distinct server ids
  that differ only in those characters would collide — in practice `server_id`
  is `"ip:port"` and collisions are not possible without deliberately crafted
  inputs.
- **Avatar validation is on the write path, not the display path.** `get_avatar`
  and `get_server_avatar_override` return whatever bytes are on disk; they do not
  re-validate. Only the set paths (`set_avatar`, `set_server_avatar_override`)
  call `validate_avatar_bytes`. Do not assume stored images are valid.
- **Profile hash is over the *serialized bytes*, not the fields.** The hash
  passed around is `profile_hash_hex(&signed.to_bytes())` — a hash of the
  signed blob including signature bytes. Two identical field values signed at
  different times produce different hashes. The client cache key in
  `useMemberProfile.ts` is the hash from the server's `MemberInfo.profile_hash`,
  which was stored at push time: the hash is stable as long as the profile is unchanged.
