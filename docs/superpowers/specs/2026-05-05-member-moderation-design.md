# Member Moderation Context Menu — Design

**Status:** Approved 2026-05-05
**Scope:** Farder client (Tauri + React) and server (farder-server crate). Mostly client UI; small server additions for ban-reason + unban.

## Goal

Give server moderators a right-click context menu on members (in the member sidebar AND on usernames in chat) for the standard moderation actions: assign roles, kick, ban (with reason), block, and the universally-useful affordances (View Profile, Send Message, Copy ID, Copy mention). Plus a Banned Members management panel so bans aren't permanent footguns.

## Non-Goals (v1)

- **Timeout / Mute** (mute-for-duration, server-side message suppression). Distinct design conversation; deferred to a follow-up spec.
- **Audit log** of moderation actions (who-modded-whom-when). Server-side accountability feature; deferred.
- **Bulk actions** (select N members → kick all). Overkill for v1; no real demand yet.
- **Kicked-user notification dialog.** Currently kicked users see "Connection lost" with no explanation. A `KickedFromServer { reason? }` frame sent before connection termination would fix this. Worth queuing as follow-up; not in v1.

## Architecture

### Two surfaces, one component

A single `MemberContextMenu` React component, rendered from two places:
1. **Member sidebar** — `MemberSidebar.tsx`: each member row gets `onContextMenu`. Right-click → menu opens at cursor.
2. **In-chat author name** — `Message.tsx`: the existing author name span (which already handles left-click → open profile popup) gains an `onContextMenu`. Both behaviors coexist.

Both surfaces pass the target's `MemberInfo` and a screen-coordinates position; the component handles permission gating, action dispatch, and dismiss-on-outside-click internally.

### Permission gating model — hide, don't disable

A menu item only appears if the actor has the relevant permission AND the action is meaningful for the target. So:
- A non-mod sees just "View Profile", "Send Message", "Block", "Copy ID", "Copy mention" on someone else.
- A full admin sees everything (except Kick/Ban on themselves or on the server owner).

Cleaner than disabled-but-visible (no clutter, no "why is this greyed out").

### Self & owner protection

- **Self:** Kick / Ban / Block / Send Message / Assign Role hidden. View Profile + Copy ID + Copy mention stay (Copy actions are universally useful).
- **Server owner (target):** Kick / Ban hidden when target is the server owner — no one boots the owner. Assign Role still visible (owners can have cosmetic roles).

## Action set (v1)

### View Profile
Opens existing `UserProfilePopup` at cursor position. No new code beyond wiring.

### Send Message
Calls existing `api.openDm(serverId, targetKey)`. On success, the existing DM dispatch flow opens the conversation in the DM panel.
- Hidden when target = self.
- No new server work.

### Assign Role… (submenu)
Hover or click → child submenu pops out to the right. Lists all server roles in display order, each row showing `[✓ if assigned] role-name (color dot)`. Click a row toggles: calls existing `assign_role` if currently absent, `remove_role` if present. Server broadcasts membership change; existing event flow refreshes the UI.
- Hidden unless actor has `MANAGE_ROLES`.

### Kick
Confirm prompt: `"Kick {name} from this server?"` → on confirm, calls new Tauri command `kick_member(server_id, member_key)` which dispatches the existing `ServerRequest::KickMember`. Server-side handler already exists, gates on `KICK_MEMBERS`, broadcasts `MemberLeft`.
- Hidden when target = self OR target = owner OR actor lacks `KICK_MEMBERS`.

### Ban (with optional reason)
Custom modal `BanConfirmDialog` (not `window.confirm`) with:
- Target name display
- Textarea for "Reason (optional, max 200 chars)"
- Cancel / Ban buttons

On confirm, calls new Tauri command `ban_member(server_id, member_key, reason)` which dispatches `ServerRequest::BanMember { member_key, reason }`. Server stores the reason on the member row, broadcasts `MemberBanned`.
- Hidden when target = self OR target = owner OR actor lacks `BAN_MEMBERS`.

### Block (client-side)
Confirm prompt: `"Block {name}? You won't see their messages or DMs."` → calls existing `api.blockUser(serverId, targetKey)`. Pure client-side hide.
- Hidden when target = self.

### Copy ID
`navigator.clipboard.writeText(targetKey)` — the public key hex. Always shown.

### Copy mention
`navigator.clipboard.writeText("@" + targetDisplayName)` — for pasting into another channel. Always shown.

### Final menu shape

```
View Profile
─────────────
Send Message            (hidden if target = self)
─────────────
Assign Role…  ▶         (hidden unless actor has MANAGE_ROLES)
─────────────
Kick                    (hidden if self / owner / no KICK_MEMBERS)
Ban                     (hidden if self / owner / no BAN_MEMBERS)
─────────────
Block                   (hidden if target = self)
─────────────
Copy ID
Copy mention
```

Hidden items don't leave gaps; the separator above a fully-hidden section is hidden too.

## Server changes (small)

### Schema migration
Add `ban_reason TEXT NULL` to `members` table. Idempotent ALTER guarded by `pragma table_info` (same pattern as the recent `reactions.file_id` migration).

### Members module
- `members::ban_member(conn, pk, reason: Option<&str>)` — extends the existing fn, stores the reason if provided
- New `members::unban_member(conn, pk)` — sets `banned = 0`, clears `ban_reason`
- New `members::list_banned(conn) -> Vec<BannedMember>` returning `{ public_key, display_name, ban_reason, banned_at }`

### Protocol additions

```rust
ServerRequest::BanMember {
    member_key: PublicKey,
    #[serde(default)]
    reason: Option<String>,        // <-- new
}
ServerRequest::UnbanMember { member_key: PublicKey }                     // <-- new
ServerRequest::ListBanned                                                 // <-- new
ServerResponse::BannedMembers { entries: Vec<BannedMember> }              // <-- new

pub struct BannedMember {
    pub public_key: PublicKey,
    pub display_name: String,
    pub ban_reason: Option<String>,
    pub banned_at: u64,
}

ServerEvent::MemberUnbanned { public_key: PublicKey }                     // <-- new
```

### Server handlers
- `ServerRequest::BanMember` extends to thread `reason` through to `members::ban_member`. Permission check (`BAN_MEMBERS`) unchanged.
- `ServerRequest::UnbanMember` — gates on `BAN_MEMBERS`; calls `members::unban_member`; broadcasts `ServerEvent::MemberUnbanned`.
- `ServerRequest::ListBanned` — gates on `BAN_MEMBERS`; calls `members::list_banned`; returns `BannedMembers` response.

### Backwards compatibility
`reason: Option<String>` with `#[serde(default)]` — old clients sending `BanMember` without the field still work (deserializes as `None`).

## Client changes

### New Tauri commands (Rust, in `commands.rs`)

```rust
#[tauri::command] pub async fn kick_member(state, server_id, member_key) -> Result<(), String>
#[tauri::command] pub async fn ban_member(state, server_id, member_key, reason: Option<String>) -> Result<(), String>
#[tauri::command] pub async fn unban_member(state, server_id, member_key) -> Result<(), String>
#[tauri::command] pub async fn list_banned(state, server_id) -> Result<Vec<BannedMember>, String>
```

Each parses hex `member_key` → `PublicKey`, dispatches via `bridge::send_request`, returns Ok/Err. Same shape as existing `assign_role` / `remove_role`.

### TypeScript bindings (in `tauri-bridge.ts`)

```ts
kickMember(serverId, memberKey): Promise<void>
banMember(serverId, memberKey, reason?: string): Promise<void>
unbanMember(serverId, memberKey): Promise<void>
listBanned(serverId): Promise<BannedMember[]>
```

Plus `BannedMember` type in `lib/types.ts`.

### New components

- **`MemberContextMenu.tsx`** — the floating menu component. Props: `target: MemberInfo`, `serverId: string`, `position: { x, y }`, `onClose: () => void`. Renders the action list with permission gating; manages submenu state for "Assign Role"; dismisses on outside click or Esc.
- **`BanConfirmDialog.tsx`** — modal with target name + reason textarea + Cancel/Ban buttons. Returns the (possibly empty) reason on confirm.
- **`BannedMembersTab.tsx`** — content for a new tab in `ServerSettingsDialog`. Loads `listBanned` on mount; renders rows of `{name, reason, banned_at}` with an "Unban" button (with confirm) per row.

### Modified components

- **`MemberSidebar.tsx`** — each member row gets `onContextMenu={(e) => { e.preventDefault(); setMenu({ target: m, position: { x: e.clientX, y: e.clientY } }); }}`. Renders `<MemberContextMenu>` when `menu` state is set.
- **`Message.tsx`** — author-name span gains `onContextMenu` (preserving existing left-click → profile popup). New state `memberMenu` separate from existing `contextMenu` (which handles message-level actions like edit/delete).
- **`ServerSettingsDialog.tsx`** — add a "Banned Members" tab alongside existing tabs. Only visible to users with `BAN_MEMBERS`.

### State + permission resolution

The menu component reads from `useApp()` to access:
- `state.servers[serverId].members` — to find the actor's `MemberInfo`
- `state.servers[serverId].roles` — for the Assign Role submenu
- The actor's resolved permission bits (computed by walking their roles' `permissions` field — utility function probably already exists; if not, write a small `resolvePermissions(member, roles)` helper)
- The server owner's public key (in the existing per-server state)

## Out of Scope / Deferred

- **Timeout / Mute** — server-enforced silence for a duration. Needs a `timeout_until` column and per-action-time check in handlers. Separate spec.
- **Mute (alternative semantics)** — server-side suppression of a member's broadcasts vs client-side hide. Needs design conversation about platform impact.
- **Audit log** — `audit_events` table tracking who did what to whom and when. Server-side concept; defer.
- **Kicked-user notification frame** — server sends a `YouWereKicked { reason? }` frame before tearing down the connection so the kicked client can show a clean dialog instead of "Connection lost". Half-day server work; queued.
- **Bulk select** — multi-select members in the sidebar for bulk kick/ban. No demand yet.
- **Reason on kick** — currently only Ban has a reason field. Could symmetrically add to Kick. Deferred unless asked.

## Success criteria

- Right-clicking a member (sidebar or in-chat name) opens the menu at the cursor.
- A non-mod right-clicking a non-self member sees: View Profile · Send Message · Block · Copy ID · Copy mention.
- A mod with KICK_MEMBERS additionally sees Kick. With BAN_MEMBERS, also Ban.
- Banning a member with a reason persists the reason; the Banned Members tab shows it correctly.
- Unbanning a member removes them from the Banned list and they can rejoin via the same identity.
- Hidden items don't leave visual gaps; section separators hide when all items in the section are hidden.
- Right-click on yourself shows: View Profile · Copy ID · Copy mention (everything else hidden).
- Right-click on the server owner shows everything except Kick/Ban (hidden by owner-protection rule).
- Copy ID puts the hex public key on the clipboard. Copy mention puts `@displayname`.
- Old clients calling `BanMember` without the new `reason` field still work.

## Implementation notes (non-binding, for the planner)

- **Permission resolution helper** — if no existing `resolvePermissions(member, roles)` function exists in the client, create a small one. The server has equivalent logic (`resolve_member_perms` in handlers.rs); the client version is a pure function over the role list.
- **Menu z-index** — use a high z-index (e.g. 2500) so it floats above modals and reaction bars but below intro-style overlays.
- **Submenu positioning** — for "Assign Role" submenu, position relative to the parent menu item. Right side by default; flip to left if it would overflow viewport.
- **Reason length cap** — enforce 200 chars in the BanConfirmDialog textarea (`maxLength={200}`); server-side gate as a sanity check (reject if longer).
- **Owner detection** — check if `state.servers[serverId].ownerPk === target.public_key`. If owner pk isn't already in client state, fetch it via `get_server_info` (already exists).
