# Role Display & Management — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Color member names by role, group the member list under hoisted-role section headers, allow self-assignment of roles, and let an admin toggle hoist + reorder roles.

**Architecture:** Add a `hoist` flag to roles (DB + protocol + types). The client colors names by the member's highest colored role, groups the member list by the member's highest hoisted role (with a catch-all "Members" section, ordered by role position), shows the Assign-Role menu on your own row, and adds a hoist toggle + reorder arrows to the role-management UI. Role changes already refresh live via the existing `PermissionsChanged` event.

**Tech Stack:** Rust (`farder-server`, `farder-protocol`), Tauri (`client/src-tauri`), TypeScript/React. Server via `cargo test -p farder-server`; client via `cargo build` + `npx tsc --noEmit`; the visual result is owner-verified on Windows.

## Global Constraints

- `hoist` is added with `#[serde(default)]` on `RoleInfo` and a guarded `ALTER TABLE … ADD COLUMN hoist INTEGER NOT NULL DEFAULT 0` migration (mirroring `members.profile_hash` in `db.rs`), so existing servers default to `hoist=0`. (spec §Section 1)
- Ordering everywhere is by role `position` DESCENDING (highest rank first) for display; the `roles` table stores raw positions. (spec §Section 2/3)
- Name color = the member's highest-`position` role with a non-null `color`; group = highest-`position` role with `hoist == true`. These are INDEPENDENT. (spec §Section 1/2)
- Online/offline is OUT OF SCOPE — every member appears in their section. (spec §"Out of scope")
- New Tauri command (`update_role`): the `invoke("update_role")` name ↔ `#[tauri::command] fn update_role` ↔ `generate_handler!` entry must agree. (CLAUDE.md seam)
- Any new `className` must be styled in every `client/src/themes/*/theme.css`; prefer reusing the existing member-group/section-header classes. (CLAUDE.md)

---

## File Structure

- `crates/farder-server/src/db.rs` — `roles.hoist` column + migration.
- `crates/farder-protocol/src/server.rs` — `RoleInfo.hoist`; `hoist` on `CreateRole`/`UpdateRole`.
- `crates/farder-server/src/members.rs` — `create_role`/`get_role`/`list_roles`/`update_role` thread `hoist`.
- `crates/farder-server/src/handlers.rs` — `CreateRole`/`UpdateRole` arms pass `hoist`.
- `client/src/lib/types.ts` — `RoleInfo.hoist`.
- `client/src-tauri/src/commands.rs` + `main.rs` — `update_role` command.
- `client/src/lib/tauri-bridge.ts` — `updateRole` wrapper.
- `client/src/components/MemberSidebar.tsx` — name colors + grouping.
- `client/src/components/MemberContextMenu.tsx` — self-assign.
- `client/src/components/ServerSettingsDialog.tsx` — hoist toggle + reorder.

---

## Task 1: Server — `hoist` on roles (DB + protocol + members + handlers)

**Files:**
- Modify: `crates/farder-server/src/db.rs` (roles CREATE ~22; migration near `profile_hash` ~294-308)
- Modify: `crates/farder-protocol/src/server.rs` (`RoleInfo` ~146; `CreateRole`/`UpdateRole` ~244-245)
- Modify: `crates/farder-server/src/members.rs` (`create_role` ~235, `get_role` ~257, `list_roles` ~281, `update_role` ~308)
- Modify: `crates/farder-server/src/handlers.rs` (`CreateRole` arm ~741, `UpdateRole` arm ~778)
- Test: `members.rs` / `handlers.rs` test module.

**Interfaces:**
- Produces: `RoleInfo { …, hoist: bool }`; `CreateRole { …, hoist: Option<bool> }`; `UpdateRole { …, hoist: Option<bool> }`; `members::create_role(…, hoist: bool)` / `update_role(…, hoist: Option<bool>)`; `list_roles`/`get_role` return `hoist`.

- [ ] **Step 1: Add the column + migration**

In `db.rs`, add `hoist INTEGER NOT NULL DEFAULT 0` to the `roles` `CREATE TABLE IF NOT EXISTS` (after `builtin`). Then add a guarded migration mirroring the `profile_hash` block (~294-308):
```rust
    let has_role_hoist = {
        let mut stmt = conn.prepare("PRAGMA table_info(roles)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols.iter().any(|c| c == "hoist")
    };
    if !has_role_hoist {
        conn.execute("ALTER TABLE roles ADD COLUMN hoist INTEGER NOT NULL DEFAULT 0", [])?;
    }
```

- [ ] **Step 2: Add `hoist` to the protocol**

In `server.rs`: add `pub hoist: bool` to `RoleInfo` (after `position`); since `RoleInfo` is constructed server-side and sent to clients, also add `#[serde(default)]` above the `hoist` field so older serialized roles deserialize as `false`. Add `hoist: Option<bool>` to `CreateRole` and `UpdateRole`.

- [ ] **Step 3: Write the failing test**

In the `members.rs` test module (reuse the existing role-test setup), assert a created role round-trips `hoist` and that `update_role` flips it:
```rust
    #[test]
    fn role_hoist_round_trips() {
        let conn = test_db(); // existing helper that runs init_schema
        let id = create_role(&conn, "Mods", 0, None, 5, true).unwrap(); // hoist = true
        let r = get_role(&conn, id).unwrap().unwrap();
        assert!(r.hoist, "create_role persists hoist");
        update_role(&conn, id, None, None, None, None, Some(false)).unwrap(); // hoist -> false
        assert!(!get_role(&conn, id).unwrap().unwrap().hoist, "update_role flips hoist");
    }
```
(Adapt the helper names + the exact `create_role`/`update_role` arg lists to what you define in Step 4.)

- [ ] **Step 4: Run — expect FAIL, then thread `hoist` through `members.rs`**

Run: `cargo test -p farder-server role_hoist_round_trips` → FAIL (compile: signatures lack `hoist`).

Then:
- `create_role`: add a `hoist: bool` param; include `hoist` in the INSERT columns/values (`hoist as i64` → store `0`/`1`).
- `get_role` + `list_roles`: add `hoist` to the SELECT (`SELECT id, name, permissions, color, position, hoist FROM roles …`), read it (`let hoist: bool = row.get(5)?;` — rusqlite maps INTEGER 0/1 to bool), and set it in the `RoleInfo { … }` construction.
- `update_role`: add a `hoist: Option<bool>` param; when `Some`, include `hoist = ?` in the UPDATE (follow the existing optional-field update pattern this fn uses for `color`/`position`).

- [ ] **Step 5: Pass `hoist` through the handler arms**

In `handlers.rs`: the `CreateRole` arm passes `hoist.unwrap_or(false)` to `members::create_role`; the `UpdateRole` arm passes `hoist` (the `Option<bool>`) to `members::update_role`. (Read both arms; thread the new field from the destructured request.)

- [ ] **Step 6: Run the test + full suite**

Run: `cargo test -p farder-server role_hoist_round_trips && cargo test -p farder-server && cargo build --workspace`
Expected: PASS; workspace builds (the protocol change compiles server + client).

- [ ] **Step 7: Commit**
```bash
git add crates/farder-server/src/db.rs crates/farder-protocol/src/server.rs crates/farder-server/src/members.rs crates/farder-server/src/handlers.rs
git commit -m "feat(server): add hoist flag to roles (DB + protocol + create/update/list)"
```

---

## Task 2: Client — `RoleInfo.hoist` + `update_role` command + `updateRole` bridge

**Files:**
- Modify: `client/src/lib/types.ts` (`RoleInfo`)
- Modify: `client/src-tauri/src/commands.rs` (new `update_role`); `client/src-tauri/src/main.rs` (register)
- Modify: `client/src/lib/tauri-bridge.ts` (`updateRole`)
- Doc: `docs/modules/tauri-commands.md` + `frontend-bridge.md`

**Interfaces:**
- Consumes: `ServerRequest::UpdateRole { role_id, name, permissions, color, position, hoist }` (Task 1).
- Produces: `RoleInfo.hoist: boolean`; `update_role` command; `updateRole(serverId, roleId, patch)` bridge.

- [ ] **Step 1: Add `hoist` to the client `RoleInfo` type**

In `client/src/lib/types.ts`, add `hoist: boolean;` to `RoleInfo`.

- [ ] **Step 2: Add the `update_role` Tauri command**

In `commands.rs` (model on `create_role`/`delete_role` — read them):
```rust
#[tauri::command]
pub async fn update_role(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    role_id: u64,
    name: Option<String>,
    permissions: Option<u64>,
    color: Option<String>,
    position: Option<u32>,
    hoist: Option<bool>,
) -> Result<(), String> {
    let response = bridge::send_request(&state, &server_id,
        ServerRequest::UpdateRole { role_id, name, permissions, color, position, hoist })
        .await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}
```
Register `commands::update_role` in `main.rs` `generate_handler!`.

- [ ] **Step 3: Add the `updateRole` bridge wrapper**

In `tauri-bridge.ts` (near `createRole`):
```ts
export async function updateRole(serverId: string, roleId: number, patch: { name?: string; permissions?: number; color?: string; position?: number; hoist?: boolean }): Promise<void> {
  return invoke<void>("update_role", { serverId, roleId, name: patch.name ?? null, permissions: patch.permissions ?? null, color: patch.color ?? null, position: patch.position ?? null, hoist: patch.hoist ?? null });
}
```

- [ ] **Step 4: Docs** — add `update_role` to `tauri-commands.md` + `updateRole` to `frontend-bridge.md`.

- [ ] **Step 5: Compile-check + commit**

Run: `cd client/src-tauri && cargo build` and `cd client && npx tsc --noEmit` — both clean.
```bash
git commit -am "feat(client): RoleInfo.hoist + update_role command/bridge"
```

---

## Task 3: Client — color member names by role

**Files:**
- Modify: `client/src/components/MemberSidebar.tsx` (the `MemberRow` `.member-name` span ~39 + the roles available to it)
- Test: tsc.

**Interfaces:**
- Consumes: `activeServer.roles` (each `RoleInfo` with `color`/`position`), `member.role_ids`.

- [ ] **Step 1: Compute + apply the highest-colored-role color**

`MemberRow` needs the server roles. If it doesn't already receive them, pass `roles: RoleInfo[]` as a prop from `MemberSidebar` (which has `activeServer.roles`). Add a helper (module-level in the file):
```ts
function nameColor(member: MemberInfo, roles: RoleInfo[]): string | undefined {
  const mine = roles
    .filter(r => r.name !== "@everyone" && r.color && member.role_ids.includes(r.id))
    .sort((a, b) => b.position - a.position);
  return mine[0]?.color ?? undefined;
}
```
Apply to the name span:
```tsx
<span className="member-name" style={{ color: nameColor(member, roles) }}>{memberDisplayName(member.display_name)}</span>
```
(`style={{ color: undefined }}` leaves the theme default — correct fallback.)

- [ ] **Step 2: Compile-check + commit**

Run: `cd client && npx tsc --noEmit` — clean.
```bash
git commit -am "feat(client): color member names by their highest colored role"
```

---

## Task 4: Client — allow self role-assignment

**Files:**
- Modify: `client/src/components/MemberContextMenu.tsx` (the Assign-Role submenu gate ~215)
- Test: tsc.

- [ ] **Step 1: Show the Assign-Role submenu on your own row**

In `MemberContextMenu`, the Assign-Role row is gated `if (!isSelf && canManageRoles && roles.length > 0)`. Change it to drop the `!isSelf` for THIS row only:
```tsx
  if (canManageRoles && roles.length > 0) {
    rows.push({ kind: "separator" });
    rows.push({ kind: "submenu", label: "Assign Role…  ▶" });
  }
```
Leave kick/ban/timeout (the `!isSelf && (canKick || …)` block) unchanged. The server already permits self-assign (owner bypasses hierarchy; non-owner mods bounded by it).

- [ ] **Step 2: Compile-check + commit**

Run: `cd client && npx tsc --noEmit` — clean.
```bash
git commit -am "feat(client): allow assigning roles to yourself"
```

---

## Task 5: Client — group the member list by hoisted role

**Files:**
- Modify: `client/src/components/MemberSidebar.tsx` (the member list render)
- Possibly modify: theme CSS only if a NEW header class is introduced (prefer reusing the existing group/section-header class).

**Interfaces:**
- Consumes: `activeServer.roles` (with `hoist`/`position`), `member.role_ids`, the existing within-group name sort.

- [ ] **Step 1: Build the grouping**

Replace the flat member render with sections. Add a helper:
```ts
// Returns the member's highest-position hoisted role, or null.
function hoistGroup(member: MemberInfo, roles: RoleInfo[]): RoleInfo | null {
  const hoisted = roles
    .filter(r => r.hoist && r.name !== "@everyone" && member.role_ids.includes(r.id))
    .sort((a, b) => b.position - a.position);
  return hoisted[0] ?? null;
}
```
Then, from the (already name-sorted) members:
- the hoisted roles present, sorted by `position` desc → for each, a section `{ role, members: those whose hoistGroup is that role }`;
- a final catch-all section `{ role: null, members: those with hoistGroup === null }`.
Render each section with a header (the role name, or "Members" for the catch-all) above its rows. REUSE the existing member-group/section-header class already in this sidebar (read the file — it already renders a `member-role-group` or similar header for role/category grouping); do NOT invent a new class. Preserve `MemberRow` for each member and the within-section name sort.

- [ ] **Step 2: Theme coverage (only if a new class was unavoidable)**

If you added any new className, add it to all `client/src/themes/*/theme.css` (var-driven) and confirm with `grep -l`. Otherwise skip.

- [ ] **Step 3: Compile-check + commit**

Run: `cd client && npx tsc --noEmit` — clean.
```bash
git commit -am "feat(client): group the member list under hoisted-role sections"
```

---

## Task 6: Client — role management UI (hoist toggle + reorder)

**Files:**
- Modify: `client/src/components/ServerSettingsDialog.tsx` (Roles section ~282-313)
- Possibly modify: theme CSS only if a new class is introduced (prefer existing).

**Interfaces:**
- Consumes: `api.updateRole(serverId, roleId, patch)` (Task 2); `activeServer.roles`.

- [ ] **Step 1: Per-role hoist toggle + reorder**

In the Roles section, the role list currently maps `(activeServer?.roles ?? []).filter(r => r.name !== "@everyone")` to a row with a delete button. Sort that list by `position` DESC and, in each row, add:
- a **"Display separately"** checkbox bound to `r.hoist`, `onChange` → `await api.updateRole(serverId, r.id, { hoist: e.target.checked })`;
- **up/down** buttons that swap `position` with the adjacent role, mirroring the channel/category reorder already in this dialog (those use `api.updateChannel(..., { position })` with an index-swap — do the same with `api.updateRole(..., { position })`). Disable up on the first row, down on the last.

Reuse the dialog's existing row/button classes (`.connect-section`, `.xp-button`, the same controls the channel reorder uses) — no new CSS unless unavoidable.

The live refresh is automatic: `UpdateRole` broadcasts `PermissionsChanged`, which the client already consumes to refetch roles + members.

- [ ] **Step 2: Compile-check (+ theme grep if a new class was added) + commit**

Run: `cd client && npx tsc --noEmit` — clean.
```bash
git commit -am "feat(client): role hoist toggle + reorder in server settings"
```

---

## Task 7: Owner runtime verification (Windows)

**Files:** none. Server changed → full rebuild incl. sidecar.

- [ ] **Step 1: Full rebuild** — `git pull` → `cargo build -p farder-server` → stop app → `.\client\src-tauri\binaries\copy-sidecar.ps1` → `cd client; npm run tauri dev` → `Ctrl+Shift+R`.

- [ ] **Step 2: Verify**
- Create a role with a color, assign it to a member (and **to yourself**) → the name shows the **role color**.
- In Server Settings → Roles, check **"Display separately"** on that role → its members appear under a **section header** in the member list; uncheck → they fall back to "Members".
- **Reorder** roles (up/down) → the member-list sections reorder to match.
- A member with multiple colored/hoisted roles uses the **highest** one for color/group.

- [ ] **Step 3: Report** what works / what doesn't (name colors, hoist grouping, reorder, self-assign). Console (F12) on any failure.

---

## Self-Review (completed by plan author)

**Spec coverage** (against `2026-06-29-role-display-management-design.md`):
- `hoist` column + protocol + create/update/list → Task 1. ✓
- Name colors (highest colored role) → Task 3. ✓
- Self-assign → Task 4. ✓
- Member-list grouping (hoisted sections by position + catch-all) → Task 5. ✓
- Role management UI (hoist toggle + reorder) → Task 6 (needs the `update_role` command from Task 2). ✓
- Live refresh via `PermissionsChanged` → already wired; no task needed. ✓
- Online/offline → correctly excluded. ✓

**Placeholder scan:** none — every step has concrete code or concrete edits; "adapt to existing helper/arg names" notes carry binding behavior (the test asserts hoist round-trip; the grouping/color helpers are given in full).

**Type consistency:** `hoist: bool`/`boolean` is consistent across the DB, `RoleInfo` (server + client), `CreateRole`/`UpdateRole`, `update_role` command, and `updateRole` bridge; `nameColor` (highest colored role) and `hoistGroup` (highest hoisted role) are distinct helpers used in Tasks 3 and 5; ordering is `position` DESC everywhere display-facing.
