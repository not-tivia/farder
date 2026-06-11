# Profile Sync: Avatars + Custom Status (Signed Profiles)

**Date:** 2026-06-11
**Status:** Approved by owner (design conversation 2026-06-11)
**Approach:** B — signed profile sync (owner picked over plain sync and file-attachment reuse)

## Problem

Changing your profile picture today only writes `avatar.png` locally
(`client/src-tauri/src/commands.rs` `set_avatar`) and returns a data URL for
your own profile popup. Nothing carries an avatar over the wire: no protocol
message exists, the `members.avatar` BLOB column (`crates/farder-server/src/db.rs`)
is dead, and the member list (`MemberSidebar.tsx`) and chat (`Message.tsx`)
render letter-initial circles for everyone. `farder_crypto::profile::SignedProfile`
(display name + avatar + status, signed, unit-tested) exists but is wired to
nothing.

## Decisions (owner)

- **Scope:** profile picture + custom status line. Live display-name rename is
  OUT (the `display_name` field inside `SignedProfile` is filled but unused this
  phase — servers keep using `members.display_name`).
- **Per-server pictures with a global default:** one main picture for your
  identity; any server can be given an override (Discord-Nitro model). Status is
  global (one status everywhere).
- **Signed:** the profile is signed by the member's identity key. Servers verify
  before storing; other clients verify before rendering. A malicious host can
  withhold a profile but cannot forge or tamper with one.
- **Animated GIF avatars allowed.** Same image rules as the custom-emoji book:
  PNG/JPG/GIF/WebP, 2 MB cap.
- **Current-picture semantics:** chat shows the member's current avatar on all
  their messages, old and new (avatar belongs to the person, not the message).

## Architecture

### Profile payload

The unit that travels and is cached is the serialized `SignedProfile`:
`ProfileData { display_name, avatar: Option<Vec<u8>>, status: Option<String> }`
plus an Ed25519 signature over the rmp-serialized data. The **profile hash** is
the SHA-256 hex of the serialized `SignedProfile`; it is the cache key and the
"did it change" comparator everywhere.

The client builds the *effective* profile per server at push time:
avatar = per-server override if set, else global `avatar.png`, else none;
status = global status; display_name = current display name. Effective profiles
differ between servers only when an override exists; each push is signed fresh.

### Wire protocol (crates/farder-protocol/src/server.rs)

All post-auth, riding the existing Primary stream (works over relay unchanged):

- `ServerRequest::UpdateProfile { profile: Vec<u8> }` — serialized
  `SignedProfile`. Response: existing `Ok` / `Error`.
- `ServerRequest::GetMemberProfile { public_key }` →
  `ServerResponse::MemberProfile { public_key, profile: Option<Vec<u8>> }`.
- Member entries in the `Members` response gain
  `#[serde(default)] profile_hash: Option<String>` (hash only — keeps the member
  list light; profiles are fetched once per hash and cached).
- `ServerEvent::MemberProfileUpdated { public_key, profile_hash: Option<String> }`
  broadcast on accepted update (None = profile cleared).

Compat: new enum variants follow the established rollout rule — update servers
before clients. Old servers answer the new request with a decode error; nothing
crashes. `serde(default)` keeps the Members change forward-compatible.

### Server (crates/farder-server)

- `members` table: reuse the existing `avatar BLOB` column to store the
  serialized `SignedProfile`; add `profile_hash TEXT` via the established
  idempotent-migration pattern. (Column keeps its name; it now holds the whole
  signed profile, documented in `docs/modules/`.)
- `UpdateProfile` handler validation, in order: signature verifies AND the
  profile's public key == the authenticated member's key; avatar (when present)
  passes `image_validation.rs` and is ≤ 2 MB; status ≤ 128 chars (chars, not
  bytes); serialized profile ≤ 2.5 MB. Reject with `Error` on any failure.
  On success: store blob + hash, broadcast `MemberProfileUpdated`.
- `GetMemberProfile` returns the stored blob for any member (requester must be
  authenticated; profiles are member-visible data like display names).

### Client — Rust (client/src-tauri)

- Local state: global `avatar.png` (existing), global status in `profile.json`
  (existing profile store), per-server overrides under
  `<data_dir>/profile_overrides/<safe-server-id>.png`.
- Push logic: track `last_pushed_hash` per server (in the existing per-server
  local store). After auth on connect, and immediately on any profile change
  while connected, rebuild the effective profile; if its hash differs from
  `last_pushed_hash`, send `UpdateProfile` and record the new hash on `Ok`.
  A global-picture or status change pushes to every connected server without a
  conflicting override; an override change pushes to that server only.
- Profile cache: disk cache keyed by profile hash
  (`<data_dir>/profile_cache/<hash>`), shared across servers. On fetch,
  **verify the signature against the member's public key before caching**;
  discard on mismatch (render fallback). Cache entries are immutable by
  construction (hash-keyed).
- New/changed Tauri commands (each registered in `generate_handler!`, mirrored
  in `tauri-bridge.ts`, documented in `docs/modules/tauri-commands.md`):
  - `set_avatar` (existing, unchanged semantics) + new
    `set_server_avatar_override(server_id, file_path)` /
    `clear_server_avatar_override(server_id)`
  - `set_profile_status(status: Option<String>)`
  - `get_member_profile(server_id, public_key) -> { avatarDataUrl?, status? }`
    — serves from cache by the member's current hash, fetching over the wire on
    miss. Lazy: NEVER called at module load (PIN-lock lesson — eb1511d).
- Bridge event for `MemberProfileUpdated` → frontend hash update.

### Client — frontend (client/src)

- Member state gains `profile_hash`; `useServerEvents.ts` applies
  `MemberProfileUpdated`.
- A small `useMemberProfile(serverId, publicKey, profileHash)` hook resolves
  hash → `{ avatarUrl, status }` via `get_member_profile`, with an in-memory map
  so a hash resolves once per session.
- Rendering: `MemberSidebar.tsx` (avatar image + status line under the name),
  `Message.tsx` (avatar image), `UserProfilePopup.tsx` (others: their picture +
  status; own: main picture, per-this-server picture + clear-override, status
  text input with 128-char counter). Letter-circle remains the fallback
  everywhere a profile/avatar is absent.
- **Theme CSS:** every new or restructured class styled in ALL theme files
  (`client/src/themes/*/theme.css`) using theme variables only — no hard-coded
  colors (CLAUDE.md rule; `xp-luna-blue` lacks some vars, needs fallbacks).

## Privacy notes

Profiles (picture + status) are visible to all members of a server and stored
by its host — the same trust level as display names and messages today, with
the same relay-can-see caveat for relayed servers (closed later by the
E2E-tunnel backlog item). The signature closes the *tamper* gap only. Per-server
overrides double as a privacy feature: present a different face per community.

## Limits and edge cases

- Removing your picture or status syncs too (profile with `avatar: None` /
  `status: None`; all-empty profiles are still valid signed payloads).
- A member's profile persists on the server if they go offline; it is removed
  with the member row if they are removed.
- Fetch failure or signature mismatch → silent fallback to letter-circle; no
  error UI.
- The 2 MB avatar cap is enforced server-side; the client pre-checks and shows
  a friendly error before uploading an oversized image.

## Out of scope (this phase)

- DM-list avatars (community servers only this pass).
- Live display-name rename.
- Rich presence / activity status (games, Spotify, listening-to) — owner wants
  this later; it extends the same status pipeline built here.
- Auto-download size caps / data-saving mode — separate feature, captured on the
  roadmap 2026-06-11.

## Verification plan

Headless here: unit tests for signature-reject (wrong key, tampered bytes),
hash stability, override-resolution (override ?? global ?? none), server
validation matrix (oversize avatar, long status, key mismatch), push-on-change
logic; `cargo test --workspace`, client crate build, `npx tsc --noEmit`, Tauri
seam check (every new `invoke` name registered). Real verification is the
owner's Windows run: change picture → second client sees it in member list +
chat; set override → differs per server; status visible; GIF animates. Per
CLAUDE.md this feature is UNVERIFIED until that run.
