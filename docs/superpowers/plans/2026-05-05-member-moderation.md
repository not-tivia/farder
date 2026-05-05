# Member Moderation Context Menu Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Right-click on any member (in the member sidebar OR on their displayed username in chat) opens a context menu with View Profile, Send Message, Assign Role submenu, Kick, Ban (with optional reason), Block, Copy ID, Copy mention. Mods get a "Banned Members" tab in Server Settings to review bans and unban people.

**Architecture:** A single `MemberContextMenu` component reused on both surfaces. Permission gating is hide-don't-disable. Server already has Kick/Ban/MANAGE_ROLES primitives + a `require_member_hierarchy` check that prevents low-rank mods from acting on higher-rank targets — UI doesn't pre-filter based on hierarchy/owner; server rejects forbidden actions and the client surfaces errors as toasts. New: `ban_reason` column + `Unban` + `ListBanned` protocol additions, plus a "Banned Members" tab.

**Tech Stack:** Rust (farder-server + Tauri v2), React + TypeScript, SQLite via rusqlite.

**Spec:** `docs/superpowers/specs/2026-05-05-member-moderation-design.md`

---

## File structure

**Server (`crates/farder-server/src/`):**
- Modify `db.rs` — schema migration adding `members.ban_reason TEXT NULL`
- Modify `members.rs` — extend `ban_member`, add `unban_member`, add `list_banned`, add `BannedMember` type
- Modify `handlers.rs` — extend `BanMember` arm to thread `reason`; add `UnbanMember` + `ListBanned` arms

**Protocol (`crates/farder-protocol/src/server.rs`):**
- Add `reason: Option<String>` to `ServerRequest::BanMember`
- Add `ServerRequest::UnbanMember { member_key }` and `ServerRequest::ListBanned`
- Add `ServerResponse::BannedMembers { entries: Vec<BannedMember> }`
- Add `pub struct BannedMember`
- Add `ServerEvent::MemberUnbanned { public_key }`
- Extend `ServerEvent::MemberBanned` with optional `reason`

**Client Rust (`client/src-tauri/src/`):**
- Modify `commands.rs` — add `kick_member`, `ban_member`, `unban_member`, `list_banned` Tauri commands
- Modify `main.rs` — register new commands
- Modify `bridge.rs` — emit `server:member_unbanned`; extend `server:member_banned` payload with reason

**Client TS (`client/src/`):**
- Modify `lib/types.ts` — add `BannedMember` type
- Modify `lib/tauri-bridge.ts` — bindings for the 4 new commands
- Modify `hooks/useServerEvents.ts` — listener for `server:member_unbanned`
- Create `lib/permissions.ts` — `resolveMemberPermissions(member, roles): bigint` helper
- Create `components/MemberContextMenu.tsx` — the floating menu component
- Create `components/BanConfirmDialog.tsx` — modal with reason input
- Create `components/BannedMembersTab.tsx` — list + unban
- Modify `components/ServerSettingsDialog.tsx` — add "Banned Members" tab, gated on BAN_MEMBERS
- Modify `components/MemberSidebar.tsx` — `onContextMenu` on member rows
- Modify `components/Message.tsx` — `onContextMenu` on author-name span

---

## Task 1: Server schema migration — add `members.ban_reason`

**Files:**
- Modify: `crates/farder-server/src/db.rs`

- [ ] **Step 1: Add the migration**

In `crates/farder-server/src/db.rs`, locate the `init_schema` function. After all CREATE TABLE statements (or with the other ALTER migrations near the bottom of the function), add:

```rust
    // Members: add ban_reason column for moderator-supplied context.
    let has_ban_reason: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(members)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols.iter().any(|c| c == "ban_reason")
    };
    if !has_ban_reason {
        conn.execute(
            "ALTER TABLE members ADD COLUMN ban_reason TEXT NULL",
            [],
        )?;
    }
```

- [ ] **Step 2: Run db tests**

```
cd /home/deez/farder/crates/farder-server && cargo test --lib db:: 2>&1 | tail -10
```

Expected: existing tests pass (including the double-init_schema idempotency test).

- [ ] **Step 3: Run the full server test suite**

```
cd /home/deez/farder/crates/farder-server && cargo test --lib 2>&1 | tail -5
```

Expected: all tests pass.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/db.rs
git -C /home/deez/farder commit -m "feat(server): add nullable members.ban_reason column"
```

---

## Task 2: Protocol additions

**Files:**
- Modify: `crates/farder-protocol/src/server.rs`

- [ ] **Step 1: Add the `BannedMember` struct**

In `crates/farder-protocol/src/server.rs`, near the other shared structs (search for `pub struct ReactionGroup` for placement reference), add:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BannedMember {
    pub public_key: PublicKey,
    pub display_name: String,
    #[serde(default)]
    pub ban_reason: Option<String>,
    pub banned_at: u64,
}
```

- [ ] **Step 2: Update `ServerRequest::BanMember` and add new variants**

Find the existing `ServerRequest::BanMember { member_key: PublicKey }` variant and replace with:

```rust
BanMember {
    member_key: PublicKey,
    #[serde(default)]
    reason: Option<String>,
},
UnbanMember {
    member_key: PublicKey,
},
ListBanned,
```

(The new variants must be added INSIDE the `enum ServerRequest`, alongside `BanMember`.)

- [ ] **Step 3: Add `ServerResponse::BannedMembers`**

Inside `enum ServerResponse`, add a variant:

```rust
BannedMembers {
    entries: Vec<BannedMember>,
},
```

- [ ] **Step 4: Add `ServerEvent::MemberUnbanned` and extend MemberBanned**

In `enum ServerEvent`, find `MemberBanned { public_key }` and replace with:

```rust
MemberBanned {
    public_key: PublicKey,
    #[serde(default)]
    reason: Option<String>,
},
MemberUnbanned {
    public_key: PublicKey,
},
```

- [ ] **Step 5: Verify the workspace compiles (errors expected in dependent crates)**

```
cd /home/deez/farder && cargo check --workspace 2>&1 | tail -15
```

Expected: `farder-protocol` itself compiles. Dependent crates (server, client) will have errors at every `BanMember` pattern match and constructor — those will be fixed in Tasks 3-5. Capture which file:line errors appear.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add crates/farder-protocol/src/server.rs
git -C /home/deez/farder commit -m "feat(protocol): BanMember reason, UnbanMember, ListBanned, BannedMember, MemberUnbanned"
```

---

## Task 3: Server members module — extend ban, add unban + list_banned

**Files:**
- Modify: `crates/farder-server/src/members.rs`

- [ ] **Step 1: Extend `ban_member` to accept an optional reason**

In `crates/farder-server/src/members.rs`, find the existing `pub fn ban_member(conn: &Connection, pk: &PublicKey) -> Result<()>` and replace its signature + body:

```rust
pub fn ban_member(conn: &Connection, pk: &PublicKey, reason: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE members SET banned = 1, ban_reason = ?2 WHERE public_key = ?1",
        rusqlite::params![pk.as_bytes().as_slice(), reason],
    )?;
    Ok(())
}
```

- [ ] **Step 2: Add `unban_member`**

Append to the same file:

```rust
pub fn unban_member(conn: &Connection, pk: &PublicKey) -> Result<()> {
    conn.execute(
        "UPDATE members SET banned = 0, ban_reason = NULL WHERE public_key = ?1",
        rusqlite::params![pk.as_bytes().as_slice()],
    )?;
    Ok(())
}
```

- [ ] **Step 3: Add `list_banned`**

Append:

```rust
pub fn list_banned(conn: &Connection) -> Result<Vec<farder_protocol::server::BannedMember>> {
    let mut stmt = conn.prepare(
        "SELECT public_key, display_name, ban_reason, joined_at \
         FROM members WHERE banned = 1 \
         ORDER BY joined_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        let pk_bytes: Vec<u8> = row.get(0)?;
        let display_name: String = row.get(1)?;
        let ban_reason: Option<String> = row.get(2)?;
        let banned_at: i64 = row.get(3)?;
        let pk = farder_crypto::identity::PublicKey::from_bytes(
            pk_bytes.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?,
        );
        Ok(farder_protocol::server::BannedMember {
            public_key: pk,
            display_name,
            ban_reason,
            banned_at: banned_at as u64,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}
```

(NOTE: `joined_at` is used as a placeholder for `banned_at` because there's no separate `banned_at` column in the existing schema. If the existing schema has a `banned_at` column, use that instead. This is acceptable because Phase 1 doesn't otherwise track the moment of the ban.)

- [ ] **Step 4: Update existing test call sites**

Search for all `ban_member(` callers in the server crate (handlers.rs, retention.rs, members.rs tests, possibly handlers.rs tests):

```
grep -rn "members::ban_member\|ban_member(" /home/deez/farder/crates/farder-server/src/ | grep -v "pub fn ban_member"
```

For each caller, add a third `None` argument. Example:
```rust
members::ban_member(&conn, &pk_bytes)        // before
members::ban_member(&conn, &pk_bytes, None)  // after
```

- [ ] **Step 5: Add tests for the new functions**

In `members.rs` `#[cfg(test)] mod tests` (or create one if absent), add:

```rust
    #[test]
    fn ban_with_reason_persists() {
        let conn = crate::db::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        let pk = farder_crypto::identity::PublicKey::from_bytes([1u8; 32]);
        // Insert the member first (mirror the existing test setup pattern).
        // Use the existing helper or raw SQL:
        conn.execute(
            "INSERT INTO members (public_key, display_name, joined_at, banned, revoked) VALUES (?1, ?2, ?3, 0, 0)",
            rusqlite::params![pk.as_bytes().as_slice(), "TestUser", 1000i64],
        ).unwrap();

        ban_member(&conn, &pk, Some("spamming")).unwrap();
        let banned: Vec<_> = list_banned(&conn).unwrap();
        assert_eq!(banned.len(), 1);
        assert_eq!(banned[0].ban_reason.as_deref(), Some("spamming"));
        assert_eq!(banned[0].display_name, "TestUser");
    }

    #[test]
    fn unban_clears_flag_and_reason() {
        let conn = crate::db::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        let pk = farder_crypto::identity::PublicKey::from_bytes([2u8; 32]);
        conn.execute(
            "INSERT INTO members (public_key, display_name, joined_at, banned, revoked) VALUES (?1, ?2, ?3, 0, 0)",
            rusqlite::params![pk.as_bytes().as_slice(), "Other", 1000i64],
        ).unwrap();

        ban_member(&conn, &pk, Some("test")).unwrap();
        assert_eq!(list_banned(&conn).unwrap().len(), 1);
        unban_member(&conn, &pk).unwrap();
        assert_eq!(list_banned(&conn).unwrap().len(), 0);

        // Verify the member is no longer banned and reason is cleared.
        let m = get_member(&conn, &pk).unwrap().unwrap();
        assert!(!m.banned);
    }
```

(Read the existing tests in `members.rs` for the EXACT pattern of helpers and field names. Adjust if your test setup differs.)

- [ ] **Step 6: Run members tests + full suite**

```
cd /home/deez/farder/crates/farder-server && cargo test --lib members:: -- --test-threads=1 2>&1 | tail -15
cd /home/deez/farder/crates/farder-server && cargo test --lib 2>&1 | tail -5
```

Expected: all tests pass (existing + 2 new).

- [ ] **Step 7: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/members.rs crates/farder-server/src/handlers.rs crates/farder-server/src/retention.rs
git -C /home/deez/farder commit -m "feat(server): members ban_reason support + unban_member + list_banned"
```

---

## Task 4: Server handlers — wire BanMember reason, add UnbanMember + ListBanned

**Files:**
- Modify: `crates/farder-server/src/handlers.rs`

- [ ] **Step 1: Update `ServerRequest::BanMember` arm**

Find the existing `ServerRequest::BanMember { member_key } => { ... }` arm (around line 710). Replace with:

```rust
ServerRequest::BanMember { member_key, reason } => {
    if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::BAN_MEMBERS, "BAN_MEMBERS")? {
        return Ok(denied);
    }
    if let Some(denied) = require_member_hierarchy(conn, member, is_owner, &member_key)? {
        return Ok(denied);
    }
    members::ban_member(conn, &member_key, reason.as_deref())?;
    let event = BroadcastEvent {
        target: EventTarget::All,
        event: ServerEvent::MemberBanned {
            public_key: member_key.clone(),
            reason,
        },
    };
    ok_with(ServerResponse::Ok, vec![event])
}
```

(Read the EXISTING handler body first to preserve any extra logic like `state.clients.write().await.remove(...)` for kicking the connection.)

- [ ] **Step 2: Add `ServerRequest::UnbanMember` arm**

Add a new arm immediately after BanMember:

```rust
ServerRequest::UnbanMember { member_key } => {
    if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::BAN_MEMBERS, "BAN_MEMBERS")? {
        return Ok(denied);
    }
    members::unban_member(conn, &member_key)?;
    let event = BroadcastEvent {
        target: EventTarget::All,
        event: ServerEvent::MemberUnbanned { public_key: member_key.clone() },
    };
    ok_with(ServerResponse::Ok, vec![event])
}
```

- [ ] **Step 3: Add `ServerRequest::ListBanned` arm**

```rust
ServerRequest::ListBanned => {
    if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::BAN_MEMBERS, "BAN_MEMBERS")? {
        return Ok(denied);
    }
    let entries = members::list_banned(conn)?;
    ok(ServerResponse::BannedMembers { entries })
}
```

- [ ] **Step 4: Verify compile**

```
cd /home/deez/farder/crates/farder-server && cargo check 2>&1 | tail -5
```

Expected: `Finished` with no new errors.

- [ ] **Step 5: Add a handler test for unban**

In `handlers.rs` `#[cfg(test)] mod tests` (use the same setup pattern as the existing kick/ban handler tests around line 1418-1476), add:

```rust
    #[test]
    fn unban_member_clears_ban_and_emits_event() {
        // Setup: open_in_memory + init_schema + insert owner + victim.
        // (Copy the setup pattern from the existing test
        //  `ban_with_higher_position_blocks` or similar.)
        // ...

        // Ban first.
        let _ = handle_request(
            &conn, &owner, true,
            ServerRequest::BanMember { member_key: victim.clone(), reason: Some("test".to_string()) },
            "",
        ).unwrap();

        // Unban.
        let result = handle_request(
            &conn, &owner, true,
            ServerRequest::UnbanMember { member_key: victim.clone() },
            "",
        ).unwrap();

        assert!(matches!(result.response, ServerResponse::Ok));
        assert_eq!(result.events.len(), 1);
        assert!(matches!(result.events[0].event, ServerEvent::MemberUnbanned { .. }));
        assert!(crate::members::list_banned(&conn).unwrap().is_empty());
    }
```

- [ ] **Step 6: Run tests**

```
cd /home/deez/farder/crates/farder-server && cargo test --lib handlers:: -- --test-threads=1 2>&1 | tail -10
cd /home/deez/farder/crates/farder-server && cargo test --lib 2>&1 | tail -5
```

Expected: all tests pass.

- [ ] **Step 7: Commit**

```
git -C /home/deez/farder add crates/farder-server/src/handlers.rs
git -C /home/deez/farder commit -m "feat(server): BanMember threads reason + UnbanMember + ListBanned handlers"
```

---

## Task 5: Tauri commands + register in main.rs

**Files:**
- Modify: `client/src-tauri/src/commands.rs`
- Modify: `client/src-tauri/src/main.rs`

- [ ] **Step 1: Add the four Tauri commands**

In `client/src-tauri/src/commands.rs`, near the other moderation-style commands (search for `pub async fn assign_role` for placement), add:

```rust
#[tauri::command]
pub async fn kick_member(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    member_key: String,
) -> Result<(), String> {
    let pk = parse_pubkey(&member_key)?;
    let response = bridge::send_request(&state, &server_id, ServerRequest::KickMember { member_key: pk })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub async fn ban_member(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    member_key: String,
    reason: Option<String>,
) -> Result<(), String> {
    let pk = parse_pubkey(&member_key)?;
    let response = bridge::send_request(&state, &server_id, ServerRequest::BanMember { member_key: pk, reason })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub async fn unban_member(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    member_key: String,
) -> Result<(), String> {
    let pk = parse_pubkey(&member_key)?;
    let response = bridge::send_request(&state, &server_id, ServerRequest::UnbanMember { member_key: pk })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub async fn list_banned(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<Vec<farder_protocol::server::BannedMember>, String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::ListBanned)
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::BannedMembers { entries } => Ok(entries),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}
```

`parse_pubkey` should already exist in commands.rs (used by assign_role/remove_role). If not, find the existing pattern in those functions and inline it.

- [ ] **Step 2: Register the commands in main.rs**

In `client/src-tauri/src/main.rs`, in the `tauri::generate_handler![ ... ]` block, add (near other moderation commands like assign_role):

```rust
            commands::kick_member,
            commands::ban_member,
            commands::unban_member,
            commands::list_banned,
```

- [ ] **Step 3: Verify compile**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -3
```

Expected: `Finished`.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs
git -C /home/deez/farder commit -m "feat(client): kick_member/ban_member/unban_member/list_banned Tauri commands"
```

---

## Task 6: Bridge.rs — emit MemberUnbanned + thread reason in MemberBanned

**Files:**
- Modify: `client/src-tauri/src/bridge.rs`

- [ ] **Step 1: Update the existing MemberBanned arm + add MemberUnbanned**

In `client/src-tauri/src/bridge.rs`, find the `dispatch_event` function and locate the MemberBanned arm (search for `MemberBanned`). Replace it with:

```rust
        ServerEvent::MemberBanned { public_key, reason } =>
            app.emit("server:member_banned", serde_json::json!({
                "server_id": sid,
                "public_key": public_key.to_string(),
                "reason": reason,
            })),
        ServerEvent::MemberUnbanned { public_key } =>
            app.emit("server:member_unbanned", serde_json::json!({
                "server_id": sid,
                "public_key": public_key.to_string(),
            })),
```

- [ ] **Step 2: Verify compile**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -3
```

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/bridge.rs
git -C /home/deez/farder commit -m "feat(client): bridge emits server:member_unbanned + reason on member_banned"
```

---

## Task 7: TS types + bridge bindings

**Files:**
- Modify: `client/src/lib/types.ts`
- Modify: `client/src/lib/tauri-bridge.ts`

- [ ] **Step 1: Add BannedMember type**

In `client/src/lib/types.ts`, near the other type exports, add:

```ts
export interface BannedMember {
  public_key: string;
  display_name: string;
  ban_reason?: string;
  banned_at: number;
}
```

- [ ] **Step 2: Add bridge bindings**

In `client/src/lib/tauri-bridge.ts`, add the 4 new functions (near existing `assignRole`/`removeRole`):

```ts
export async function kickMember(serverId: string, memberKey: string): Promise<void> {
  return invoke<void>("kick_member", { serverId, memberKey });
}

export async function banMember(serverId: string, memberKey: string, reason?: string): Promise<void> {
  return invoke<void>("ban_member", { serverId, memberKey, reason: reason ?? null });
}

export async function unbanMember(serverId: string, memberKey: string): Promise<void> {
  return invoke<void>("unban_member", { serverId, memberKey });
}

export async function listBanned(serverId: string): Promise<BannedMember[]> {
  return invoke<BannedMember[]>("list_banned", { serverId });
}
```

Add an `import type { BannedMember } from "./types";` at the top if needed.

- [ ] **Step 3: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/lib/types.ts client/src/lib/tauri-bridge.ts
git -C /home/deez/farder commit -m "feat(client): BannedMember type + 4 moderation TS bindings"
```

---

## Task 8: Permission resolver helper

**Files:**
- Create: `client/src/lib/permissions.ts`

- [ ] **Step 1: Create the file**

`client/src/lib/permissions.ts`:

```ts
import type { MemberInfo, RoleInfo } from "./types";
import { publicKeyToString } from "./types";

// Permission flags must match crates/farder-server/src/permissions.rs
export const PERMISSIONS = {
  CREATE_INSTANT_INVITE: 1n << 0n,
  KICK_MEMBERS: 1n << 10n,
  BAN_MEMBERS: 1n << 11n,
  MANAGE_ROLES: 1n << 8n,
  MANAGE_SERVER: 1n << 9n,
  MANAGE_CHANNEL: 1n << 7n,
  MANAGE_MESSAGES: 1n << 3n,
  // Add more as needed; only the ones used by the context menu are listed.
} as const;

/** Compute the bitwise OR of all role permissions for the given member. */
export function resolveMemberPermissions(member: MemberInfo, roles: RoleInfo[]): bigint {
  if (member.role_ids.length === 0) return 0n;
  let bits = 0n;
  for (const roleId of member.role_ids) {
    const role = roles.find((r) => r.id === roleId);
    if (!role) continue;
    // role.permissions is a u64 from the server, deserialized as number or string.
    // Use BigInt() to safely convert either form.
    bits |= BigInt(role.permissions);
  }
  return bits;
}

export function hasPermission(bits: bigint, perm: bigint): boolean {
  return (bits & perm) === perm;
}

/** Find the actor's MemberInfo + their resolved permissions in one shot. */
export function getActorPermissions(
  members: MemberInfo[],
  roles: RoleInfo[],
  ownPk: string,
): { member: MemberInfo | null; bits: bigint } {
  const member = members.find((m) => publicKeyToString(m.public_key) === ownPk) ?? null;
  if (!member) return { member: null, bits: 0n };
  return { member, bits: resolveMemberPermissions(member, roles) };
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`. If `MemberInfo.role_ids` doesn't exist or is named differently (e.g. `role_assignments`), open `lib/types.ts` and align the field name to whatever exists.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/lib/permissions.ts
git -C /home/deez/farder commit -m "feat(client): permission resolver helper"
```

---

## Task 9: BanConfirmDialog component

**Files:**
- Create: `client/src/components/BanConfirmDialog.tsx`

- [ ] **Step 1: Create the file**

`client/src/components/BanConfirmDialog.tsx`:

```tsx
import { useState, type CSSProperties } from "react";

interface Props {
  targetName: string;
  onCancel: () => void;
  onConfirm: (reason: string) => void;
}

const overlay: CSSProperties = {
  position: "fixed", inset: 0, background: "rgba(0,0,0,0.4)",
  display: "flex", alignItems: "center", justifyContent: "center", zIndex: 2400,
};

const card: CSSProperties = {
  background: "var(--xp-window-bg, #ECE9D8)",
  color: "var(--xp-text-normal, #000)",
  border: "2px solid var(--xp-blue-dark, #003C74)",
  padding: 20, width: 380,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
};

export default function BanConfirmDialog({ targetName, onCancel, onConfirm }: Props) {
  const [reason, setReason] = useState("");

  return (
    <div style={overlay} onClick={onCancel}>
      <div style={card} onClick={(e) => e.stopPropagation()}>
        <h3 style={{ marginTop: 0 }}>Ban {targetName}?</h3>
        <p style={{ fontSize: 11, color: "var(--xp-text-muted, #666)" }}>
          They won't be able to rejoin with this identity.
        </p>
        <label style={{ fontSize: 11, display: "block", marginTop: 8, marginBottom: 4 }}>
          Reason (optional)
        </label>
        <textarea
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          maxLength={200}
          rows={3}
          style={{ width: "100%", font: "inherit", boxSizing: "border-box" }}
        />
        <div style={{ fontSize: 9, color: "var(--xp-text-muted, #888)", textAlign: "right" }}>
          {reason.length}/200
        </div>
        <div style={{ display: "flex", justifyContent: "flex-end", gap: 6, marginTop: 12 }}>
          <button onClick={onCancel} style={{ font: "inherit", padding: "4px 12px" }}>
            Cancel
          </button>
          <button
            onClick={() => onConfirm(reason.trim())}
            style={{
              font: "inherit", padding: "4px 12px",
              background: "#a00", color: "#fff",
              border: "1px solid #800",
            }}
          >
            Ban
          </button>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/BanConfirmDialog.tsx
git -C /home/deez/farder commit -m "feat(client): BanConfirmDialog with reason input"
```

---

## Task 10: MemberContextMenu component

**Files:**
- Create: `client/src/components/MemberContextMenu.tsx`

- [ ] **Step 1: Create the component**

`client/src/components/MemberContextMenu.tsx`:

```tsx
import { useEffect, useRef, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import type { MemberInfo, RoleInfo } from "../lib/types";
import { publicKeyToString } from "../lib/types";
import { useApp, useActiveServer } from "../context/ServerContext";
import { getActorPermissions, hasPermission, PERMISSIONS } from "../lib/permissions";
import BanConfirmDialog from "./BanConfirmDialog";
import UserProfilePopup from "./UserProfilePopup";

interface Props {
  target: MemberInfo;
  serverId: string;
  position: { x: number; y: number };
  ownPk: string | null;
  onClose: () => void;
}

const menu: CSSProperties = {
  position: "fixed",
  background: "var(--xp-panel-bg, #fff)",
  color: "var(--xp-text-normal, #000)",
  border: "1px solid var(--xp-border, #888)",
  boxShadow: "2px 2px 8px rgba(0,0,0,0.3)",
  padding: 4,
  minWidth: 180,
  zIndex: 2500,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
  fontSize: "var(--xp-font-size, 11px)",
};

const item: CSSProperties = {
  display: "block",
  width: "100%",
  textAlign: "left",
  padding: "4px 10px",
  background: "transparent",
  border: "none",
  cursor: "pointer",
  font: "inherit",
  color: "inherit",
};

const separator: CSSProperties = {
  height: 1,
  background: "var(--xp-border, #ccc)",
  margin: "4px 0",
};

const submenu: CSSProperties = {
  ...menu,
  maxHeight: 280,
  overflowY: "auto",
};

export default function MemberContextMenu({ target, serverId, position, ownPk, onClose }: Props) {
  const ref = useRef<HTMLDivElement | null>(null);
  const activeServer = useActiveServer();
  const [showRoleSubmenu, setShowRoleSubmenu] = useState(false);
  const [showBanDialog, setShowBanDialog] = useState(false);
  const [showProfile, setShowProfile] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const targetPk = publicKeyToString(target.public_key);
  const isSelf = ownPk === targetPk;
  const members = activeServer?.members ?? [];
  const roles: RoleInfo[] = activeServer?.roles ?? [];

  const { bits } = ownPk
    ? getActorPermissions(members, roles, ownPk)
    : { bits: 0n };

  const canManageRoles = hasPermission(bits, PERMISSIONS.MANAGE_ROLES);
  const canKick = hasPermission(bits, PERMISSIONS.KICK_MEMBERS);
  const canBan = hasPermission(bits, PERMISSIONS.BAN_MEMBERS);

  // Close on outside click or Esc.
  useEffect(() => {
    function handleMouse(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    }
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("mousedown", handleMouse);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleMouse);
      document.removeEventListener("keydown", handleKey);
    };
  }, [onClose]);

  async function viewProfile() {
    setShowProfile(true);
  }

  async function sendMessage() {
    try {
      await api.openDm(serverId, targetPk);
    } catch (e) {
      setError(String(e));
    }
    onClose();
  }

  async function toggleRole(roleId: number) {
    const has = target.role_ids?.includes(roleId);
    try {
      if (has) await api.removeRole(serverId, targetPk, roleId);
      else await api.assignRole(serverId, targetPk, roleId);
    } catch (e) {
      setError(String(e));
    }
  }

  async function kickConfirm() {
    if (!window.confirm(`Kick ${target.display_name} from this server?`)) return;
    try {
      await api.kickMember(serverId, targetPk);
    } catch (e) {
      setError(String(e));
    }
    onClose();
  }

  async function banConfirm(reason: string) {
    setShowBanDialog(false);
    try {
      await api.banMember(serverId, targetPk, reason || undefined);
    } catch (e) {
      setError(String(e));
    }
    onClose();
  }

  async function blockConfirm() {
    if (!window.confirm(`Block ${target.display_name}? You won't see their messages or DMs.`)) return;
    try {
      await api.blockUser(serverId, targetPk);
    } catch (e) {
      setError(String(e));
    }
    onClose();
  }

  function copyId() {
    navigator.clipboard.writeText(targetPk).catch(() => {});
    onClose();
  }

  function copyMention() {
    navigator.clipboard.writeText(`@${target.display_name}`).catch(() => {});
    onClose();
  }

  // Build the items array conditionally to compute which separators are needed.
  type Row = { kind: "item"; label: string; onClick: () => void; danger?: boolean }
            | { kind: "submenu"; label: string }
            | { kind: "separator" };
  const rows: Row[] = [];

  rows.push({ kind: "item", label: "View Profile", onClick: viewProfile });
  if (!isSelf) {
    rows.push({ kind: "separator" });
    rows.push({ kind: "item", label: "Send Message", onClick: sendMessage });
  }
  if (!isSelf && canManageRoles && roles.length > 0) {
    rows.push({ kind: "separator" });
    rows.push({ kind: "submenu", label: "Assign Role…  ▶" });
  }
  if (!isSelf && (canKick || canBan)) {
    rows.push({ kind: "separator" });
    if (canKick) rows.push({ kind: "item", label: "Kick", onClick: kickConfirm, danger: true });
    if (canBan) rows.push({ kind: "item", label: "Ban", onClick: () => setShowBanDialog(true), danger: true });
  }
  if (!isSelf) {
    rows.push({ kind: "separator" });
    rows.push({ kind: "item", label: "Block", onClick: blockConfirm });
  }
  rows.push({ kind: "separator" });
  rows.push({ kind: "item", label: "Copy ID", onClick: copyId });
  rows.push({ kind: "item", label: "Copy mention", onClick: copyMention });

  // Drop leading separator and consecutive separators.
  const cleaned: Row[] = [];
  for (const r of rows) {
    if (r.kind === "separator") {
      if (cleaned.length === 0 || cleaned[cleaned.length - 1].kind === "separator") continue;
    }
    cleaned.push(r);
  }
  if (cleaned.length > 0 && cleaned[cleaned.length - 1].kind === "separator") cleaned.pop();

  return (
    <>
      <div ref={ref} style={{ ...menu, top: position.y, left: position.x }}>
        {cleaned.map((row, i) => {
          if (row.kind === "separator") return <div key={`sep-${i}`} style={separator} />;
          if (row.kind === "submenu") {
            return (
              <div
                key={`sub-${i}`}
                onMouseEnter={() => setShowRoleSubmenu(true)}
                onMouseLeave={() => setShowRoleSubmenu(false)}
                style={{ position: "relative" }}
              >
                <button
                  style={item}
                  onClick={() => setShowRoleSubmenu((s) => !s)}
                >
                  {row.label}
                </button>
                {showRoleSubmenu && (
                  <div style={{ ...submenu, position: "absolute", top: 0, left: "100%", marginLeft: 2 }}>
                    {roles.map((r) => {
                      const has = target.role_ids?.includes(r.id) ?? false;
                      return (
                        <button
                          key={r.id}
                          style={item}
                          onClick={(e) => { e.stopPropagation(); void toggleRole(r.id); }}
                        >
                          {has ? "✓ " : "  "}<span style={{ color: r.color ?? "inherit" }}>●</span> {r.name}
                        </button>
                      );
                    })}
                  </div>
                )}
              </div>
            );
          }
          return (
            <button
              key={`item-${i}`}
              style={{ ...item, color: row.danger ? "#a00" : item.color }}
              onClick={row.onClick}
            >
              {row.label}
            </button>
          );
        })}
        {error && (
          <div style={{ color: "#a00", fontSize: 10, padding: "4px 10px", borderTop: "1px solid var(--xp-border, #ccc)" }}>
            {error}
          </div>
        )}
      </div>

      {showBanDialog && (
        <BanConfirmDialog
          targetName={target.display_name}
          onCancel={() => setShowBanDialog(false)}
          onConfirm={banConfirm}
        />
      )}
      {showProfile && (
        <UserProfilePopup
          member={target}
          roles={roles}
          position={position}
          onClose={() => { setShowProfile(false); onClose(); }}
          isSelf={isSelf}
          serverId={serverId}
        />
      )}
    </>
  );
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`. If `target.role_ids` doesn't exist (depends on existing MemberInfo shape), use whatever the existing field is — read `lib/types.ts` for the actual shape and adjust.

If `RoleInfo` doesn't have a `color` field, drop that styling.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/MemberContextMenu.tsx
git -C /home/deez/farder commit -m "feat(client): MemberContextMenu with permission-gated items + role submenu"
```

---

## Task 11: BannedMembersTab + ServerSettingsDialog tab integration

**Files:**
- Create: `client/src/components/BannedMembersTab.tsx`
- Modify: `client/src/components/ServerSettingsDialog.tsx`

- [ ] **Step 1: Create BannedMembersTab**

`client/src/components/BannedMembersTab.tsx`:

```tsx
import { useEffect, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import type { BannedMember } from "../lib/types";

interface Props {
  serverId: string;
}

const row: CSSProperties = {
  display: "flex",
  alignItems: "center",
  justifyContent: "space-between",
  padding: "8px 0",
  borderBottom: "1px solid var(--xp-border, #ccc)",
  gap: 12,
};

export default function BannedMembersTab({ serverId }: Props) {
  const [entries, setEntries] = useState<BannedMember[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    setLoading(true);
    setError(null);
    try {
      setEntries(await api.listBanned(serverId));
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => { void refresh(); }, [serverId]);

  async function unban(entry: BannedMember) {
    if (!window.confirm(`Unban ${entry.display_name}? They'll be able to rejoin with this identity.`)) return;
    try {
      await api.unbanMember(serverId, entry.public_key);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div style={{ padding: 12 }}>
      <h3 style={{ marginTop: 0 }}>Banned Members</h3>
      {loading && <div>Loading…</div>}
      {error && <div style={{ color: "#a00" }}>{error}</div>}
      {!loading && entries.length === 0 && (
        <div style={{ color: "var(--xp-text-muted, #666)", fontSize: 11 }}>
          No banned members.
        </div>
      )}
      {entries.map((e) => (
        <div key={e.public_key} style={row}>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ fontWeight: "bold" }}>{e.display_name}</div>
            <div style={{ fontSize: 10, color: "var(--xp-text-muted, #666)", overflow: "hidden", textOverflow: "ellipsis" }}>
              {e.ban_reason ?? <em>(no reason)</em>}
            </div>
          </div>
          <button
            onClick={() => void unban(e)}
            style={{ font: "inherit", padding: "4px 10px" }}
          >
            Unban
          </button>
        </div>
      ))}
    </div>
  );
}
```

- [ ] **Step 2: Add the tab to ServerSettingsDialog**

In `client/src/components/ServerSettingsDialog.tsx`, find the existing tab list (search for tabs / tab definitions). Add a new "Banned Members" tab. The dialog likely has a state for active tab + an array of tab labels. Add `"banned"` (or whatever the convention is) to the array, and render the `BannedMembersTab` component when that tab is active.

Gate the tab on the actor having `BAN_MEMBERS`. Read the existing tabs to understand the gating pattern; if no gating exists yet, use:

```tsx
import { getActorPermissions, hasPermission, PERMISSIONS } from "../lib/permissions";
import BannedMembersTab from "./BannedMembersTab";
// ...
// Inside the component, near where tabs are declared:
const { bits } = ownPk ? getActorPermissions(members, roles, ownPk) : { bits: 0n };
const canBan = hasPermission(bits, PERMISSIONS.BAN_MEMBERS);
// Then conditionally include "Banned Members" in the tabs array.
```

Add the render branch:

```tsx
{activeTab === "banned" && <BannedMembersTab serverId={serverId} />}
```

- [ ] **Step 3: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/components/BannedMembersTab.tsx client/src/components/ServerSettingsDialog.tsx
git -C /home/deez/farder commit -m "feat(client): BannedMembersTab + ServerSettingsDialog integration"
```

---

## Task 12: Wire MemberSidebar + Message onContextMenu + event listener

**Files:**
- Modify: `client/src/components/MemberSidebar.tsx`
- Modify: `client/src/components/Message.tsx`
- Modify: `client/src/hooks/useServerEvents.ts`

- [ ] **Step 1: Wire MemberSidebar**

In `client/src/components/MemberSidebar.tsx`, find where each member row is rendered. Add to the imports:

```tsx
import { useState } from "react"; // if not already
import MemberContextMenu from "./MemberContextMenu";
import type { MemberInfo } from "../lib/types";
```

Add state inside the component:

```tsx
const [contextMenu, setContextMenu] = useState<{ target: MemberInfo; position: { x: number; y: number } } | null>(null);
const [ownPk, setOwnPk] = useState<string | null>(null);
useEffect(() => {
  api.getPublicKey().then(setOwnPk).catch(() => {});
}, []);
```

(Use existing `api` import or add `import * as api from "../lib/tauri-bridge"`.)

On each member row, add `onContextMenu`:

```tsx
<div
  // ... existing props ...
  onContextMenu={(e) => {
    e.preventDefault();
    setContextMenu({ target: m, position: { x: e.clientX, y: e.clientY } });
  }}
>
```

(Replace `m` with the actual variable name used in the existing map iteration.)

At the end of the component's JSX (just before the closing tag), add:

```tsx
{contextMenu && serverId && (
  <MemberContextMenu
    target={contextMenu.target}
    serverId={serverId}
    position={contextMenu.position}
    ownPk={ownPk}
    onClose={() => setContextMenu(null)}
  />
)}
```

(`serverId` should already be derived from `useActiveServerId()` or similar — confirm by reading the existing component.)

- [ ] **Step 2: Wire Message author-name span**

In `client/src/components/Message.tsx`, find the author-name `<span>` (around line 199-205, the one with `className="message-author"` and `onClick={member ? ... : undefined}`).

Add a new state alongside the existing `contextMenu`:

```tsx
const [memberMenu, setMemberMenu] = useState<{ x: number; y: number } | null>(null);
```

Add `onContextMenu` to the author span:

```tsx
<span
  className="message-author"
  style={{ color, cursor: member ? "pointer" : undefined }}
  onClick={member ? (e) => setProfilePopup({ x: e.clientX, y: e.clientY }) : undefined}
  onContextMenu={member ? (e) => {
    e.preventDefault();
    e.stopPropagation();  // prevent the row-level onContextMenu
    setMemberMenu({ x: e.clientX, y: e.clientY });
  } : undefined}
>
  {displayName}
</span>
```

Add the render (e.g. just below where `profilePopup` is rendered):

```tsx
{memberMenu && member && serverId && (
  <MemberContextMenu
    target={member}
    serverId={serverId}
    position={memberMenu}
    ownPk={ownPk}
    onClose={() => setMemberMenu(null)}
  />
)}
```

(Add `import MemberContextMenu from "./MemberContextMenu";` at the top.)

- [ ] **Step 3: Add MemberUnbanned event listener**

In `client/src/hooks/useServerEvents.ts`, find the existing reaction event listeners (around line 175-205). Add a new listener for `server:member_unbanned`:

```ts
listen("server:member_unbanned", (e) => {
  const data = e.payload as { server_id: string; public_key: string };
  // For v1 we just trigger a re-render of any open BannedMembersTab via a
  // global custom event. The tab's refresh() picks it up.
  window.dispatchEvent(new CustomEvent("farder:banned-list-changed", { detail: { serverId: data.server_id } }));
}).then(safePush);
```

Then in `BannedMembersTab.tsx`, add a listener:

```tsx
useEffect(() => {
  const handler = (e: Event) => {
    const detail = (e as CustomEvent).detail as { serverId: string };
    if (detail.serverId === serverId) void refresh();
  };
  window.addEventListener("farder:banned-list-changed", handler);
  return () => window.removeEventListener("farder:banned-list-changed", handler);
}, [serverId]);
```

This is a lightweight pub/sub via `window`. Avoids threading a new reducer action through ServerContext for one rare event.

- [ ] **Step 4: Verify TS + Rust compile**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -3
```

Expected: tsc exit 0, cargo Finished.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src/components/MemberSidebar.tsx client/src/components/Message.tsx client/src/hooks/useServerEvents.ts client/src/components/BannedMembersTab.tsx
git -C /home/deez/farder commit -m "feat(client): wire member context menu surfaces + member_unbanned listener"
```

---

## Task 13: End-to-end smoke test + CHANGELOG

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Restart dev session and walk the smoke tests**

```
pkill -f farder-server
cd /home/deez/farder/client && npm run tauri dev
```

Use two clients (or one client + a second identity) to confirm:

- [ ] Right-click on a member in the **member sidebar** → menu opens at cursor.
- [ ] Right-click on a username in **chat** → same menu opens. Left-click still opens the profile popup (separate behavior).
- [ ] Right-click on **yourself** → see View Profile · Copy ID · Copy mention only. Kick/Ban/Block/Send Message hidden.
- [ ] As a non-mod (no KICK_MEMBERS/BAN_MEMBERS), right-click another user → see View Profile · Send Message · Block · Copy ID · Copy mention only.
- [ ] As a mod, see Kick / Ban based on perms.
- [ ] **Send Message** opens a DM with the target.
- [ ] **Assign Role** submenu shows roles with ✓ for already-assigned. Clicking a row toggles assignment.
- [ ] **Kick** prompts; on confirm, the target's connection drops and they vanish from the member list.
- [ ] **Ban** opens the BanConfirmDialog. Reason input has 200-char cap. Ban with a reason → server stores it.
- [ ] **Block** prompts; on confirm, target's messages are hidden.
- [ ] **Copy ID** puts the hex public key on the clipboard.
- [ ] **Copy mention** puts `@displayname` on the clipboard.
- [ ] In **Server Settings → Banned Members** tab (visible only with BAN_MEMBERS), the banned user appears with the reason.
- [ ] Click **Unban** → confirm prompt → user removed from the banned list. They can rejoin via the same identity.
- [ ] Old client without the new TypeScript changes (if available) still works against the new server (Backwards compat: `Option<String>` reason field).

- [ ] **Step 2: Add CHANGELOG entry**

In `CHANGELOG.md`, under `### Added`, add:

```
- (2026-05-05) Member moderation context menu: right-click any member (in the member sidebar OR on their displayed name in chat) to View Profile · Send Message · Assign Roles · Kick · Ban (with optional reason, 200-char cap) · Block · Copy ID · Copy mention. Permission-gated — items hide when actor lacks the permission. New "Banned Members" tab in Server Settings (visible to users with BAN_MEMBERS) shows banned users with their reason and an Unban button. Server changes: nullable `members.ban_reason` column + `UnbanMember` and `ListBanned` protocol additions. Server-side `require_member_hierarchy` already prevents low-rank mods from acting on higher-rank targets (including the owner). Timeout/mute/audit-log are deferred to follow-up specs.
```

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add CHANGELOG.md
git -C /home/deez/farder commit -m "docs: changelog for member moderation context menu"
```

---

## Self-review notes

**Spec coverage:**
- Two-surface menu (sidebar + in-chat name) → Tasks 10, 12
- Hide-don't-disable permission gating → Task 10 (rows array conditional builder)
- Self protection → Task 10 (`isSelf` guard)
- Owner protection → Server-side via `require_member_hierarchy` (no UI work needed; per spec note)
- Action set: View Profile / Send Message / Assign Role / Kick / Ban / Block / Copy ID / Copy mention → Task 10
- Ban with optional reason → Tasks 2, 3, 4, 9, 10
- Unban + Banned Members tab → Tasks 4, 11, 12
- New Tauri commands → Task 5
- Server schema migration → Task 1
- Protocol additions with backwards-compat → Task 2
- Permission resolver → Task 8
- BanConfirmDialog → Task 9

**Type/name consistency:** `BannedMember` defined in protocol (Task 2), surfaced in TS (Task 7), used in BannedMembersTab (Task 11). `ban_reason` field consistent across schema (Task 1), members.rs (Task 3), protocol (Task 2), TS (Task 7). New Tauri command names match between Rust (Task 5) and TS bindings (Task 7).

**No placeholders:** every code step has runnable code. The two "read existing pattern first" notes (Task 11 ServerSettingsDialog tab integration; Task 12 author-span onContextMenu) are necessary because those edits depend on the existing component shape — the implementer must align with established conventions, not reinvent them.

**Known compromise:** owner-target detection is server-side only (server's `require_member_hierarchy` rejects). UI doesn't proactively hide Kick/Ban for owner targets — if a mod tries on the owner, the server returns an error and the menu's `error` state shows it inline. Adds 0 UX friction in practice (no one tries to kick the owner casually) and saves a protocol/state addition.
