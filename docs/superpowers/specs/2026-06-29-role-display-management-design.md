# Role Display & Management — Design Spec

**Status:** Approved (brainstorm 2026-06-29)

## Goal

Make roles visible and manageable the way users expect from Discord: members'
names show their role color, the member list groups members under "hoisted" role
section headers, you can assign roles to yourself, and you can control which roles
appear (and in what order).

## Problem / context

Roles exist (each has a `name`, `permissions`, `color`, `position`) and can be
created/deleted, and role assignment now works (it refreshes via the
`PermissionsChanged` event). But the role *display* is missing:

- Member names render with no color (`<span className="member-name">…</span>`) —
  the role `color` is never applied.
- The member list is a flat, name-sorted list — there is no role grouping.
- There is no "hoist" concept (a per-role "show its members as their own group").
- The "Assign Role" menu is hidden on your own row, so you can't self-assign.
- Role management (in `ServerSettingsDialog`) is minimal: create (name + color) and
  delete only — no per-role editing, no reordering.

Farder does **not** currently track per-member online/offline status (the green
dot is cosmetic; `presence` is *rich* presence — Music/Game). So an online/offline
split in the grouping is **out of scope** here and deferred to a follow-on that
first builds presence tracking.

## Out of scope (deferred follow-on)

Online/offline member tracking, and the resulting "role sections show only online
members; offline members drop to a greyed Offline group." Requires a new presence
system (server broadcasts member connect/disconnect → client tracks an online set).
Not part of this spec. For now every member appears in their role section.

## Section 1 — Role model + name colors + self-assign

### `hoist` flag on roles
Add a `hoist` boolean to roles (default `false`), meaning "display this role's
members as their own group in the member list."

- **DB** (`crates/farder-server/src/db.rs`, `roles` table ~line 22): add a
  `hoist INTEGER NOT NULL DEFAULT 0` column. Use `CREATE TABLE IF NOT EXISTS` +
  an idempotent `ALTER TABLE … ADD COLUMN` migration guard for existing DBs (mirror
  how other added columns are migrated), so existing servers get `hoist=0`.
- **Protocol** (`crates/farder-protocol/src/server.rs`): add `hoist: bool` to
  `RoleInfo` (with `#[serde(default)]` for backward compat); add
  `hoist: Option<bool>` to `ServerRequest::CreateRole` and
  `ServerRequest::UpdateRole`.
- **Server** (`crates/farder-server/src/members.rs` / handlers): persist + read
  `hoist` in the role create/update/list paths.
- **Client types** (`client/src/lib/types.ts`): add `hoist: boolean` to `RoleInfo`.

### Name colors
In `MemberSidebar`'s `MemberRow`, render the member's name in the color of their
**highest-positioned role that has a non-null `color`**, falling back to the
theme's normal name color when they have no colored role.

- Compute: from `activeServer.roles` filtered to `member.role_ids`, exclude
  `@everyone`, sort by `position` descending, take the first with a non-null
  `color`. Apply as an inline `style={{ color: roleColor }}` on the `.member-name`
  span (inline color is acceptable per CLAUDE.md — it's a dynamic per-member value,
  not a hard-coded theme color). No new CSS class.

This is independent of hoist: a colored non-hoisted role still tints the name.

### Self-assignment
In `MemberContextMenu`, show the "Assign Role" submenu on your **own** row too
(currently gated behind `!isSelf`). Remove only that `!isSelf` condition for the
role submenu — leave kick/ban/timeout self-gated. The server already permits
self-assign: `AssignRole` checks `MANAGE_ROLES` + `require_role_hierarchy`, and the
owner bypasses hierarchy (`is_owner`), while a non-owner mod remains bounded by it
(can't grant a role at/above their own position). No server change needed.

## Section 2 — Member-list grouping

Replace the flat member list in `MemberSidebar` with grouped sections:

- **Bucketing:** for each member, find their highest-positioned role with
  `hoist == true` (their "hoist group"). Members with no hoisted role go to a
  catch-all bucket.
- **Sections rendered, in order:** one section per hoisted role that has at least
  one member, ordered by role `position` **descending** (highest rank first), each
  with a header (the role name) listing its members (name-sorted within). Then a
  final **"Members"** section for the catch-all bucket.
- Every member appears in exactly one section (their highest hoisted role, else the
  catch-all). Online/offline is not considered (deferred).
- Section headers reuse the existing member-group / category-header styling
  (`member-role-group` or equivalent already used in the sidebar) so they inherit
  every theme; no new color CSS. The existing within-group name sort
  (`memberDisplayName(...).localeCompare`) is preserved inside each section.

## Section 3 — Role management UI (Server Settings)

In `ServerSettingsDialog`'s Roles section (~line 282), upgrade each role row from
"name + delete" to include:

- A **"Display separately" (hoist) checkbox** bound to the role's `hoist`, calling
  `api.updateRole(serverId, roleId, { hoist })` on change.
- **Up/down reorder arrows** that swap the role's `position` with its neighbor,
  mirroring the existing channel/category reordering in the same dialog
  (`api.updateRole(serverId, roleId, { position })`). Roles are listed sorted by
  `position` descending (highest at top).

This lets the user control which roles are hoisted and the order in which the
member-list sections appear (sections follow `position`).

The `updateRole` bridge/command + `UpdateRole` request must carry `hoist` and
`position` (position already exists on `UpdateRole`; add `hoist`).

## Data flow / refresh

Role create/update/delete and assignment all broadcast `PermissionsChanged`
(already wired as of the recent fix), which the client consumes to refetch
`getServerInfo` (roles, now incl. `hoist`/`position`) and `getMembers` (role_ids).
So hoist/reorder/color/assign changes reflect live with no extra plumbing.

## Error handling

- Reordering at the top/bottom is a no-op (no neighbor to swap).
- `updateRole` failures surface via the existing settings-dialog error path.
- A member whose only colored/hoisted role was just deleted falls back to the
  default name color / catch-all "Members" group on the next `PermissionsChanged`
  refresh.

## Testing

- **Server (unit):** `roles` table round-trips `hoist`; `CreateRole`/`UpdateRole`
  set/return `hoist` and `position`; `list_roles` includes `hoist`.
- **Client (compile-gated, tsc):** the grouping helper buckets members by highest
  hoisted role with a catch-all; the name-color helper picks the highest colored
  role; the role-row hoist toggle + reorder call `updateRole`; the self-assign
  submenu shows on the own row.
- **Owner runtime (Windows):** set a role to "display separately" → its members
  appear under a section header; reorder roles → sections reorder; a colored role
  tints member names; assign a role to yourself.

## Decomposition

Single sub-project (one plan). Natural task order: (1) server — `hoist` column +
protocol + create/update/list; (2) client types + `updateRole` bridge carrying
`hoist`; (3) name colors; (4) self-assign; (5) member-list grouping; (6) role
management UI (hoist toggle + reorder); (7) owner runtime verification.
