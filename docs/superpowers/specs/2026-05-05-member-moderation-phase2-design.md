# Member Moderation Phase 2 — Design Spec

**Date:** 2026-05-05
**Phase 1 reference:** `docs/superpowers/specs/2026-05-05-member-moderation-design.md`

## Goal

Add the three deferred capabilities from Phase 1:

1. **Timeout** — server-enforced silence for a configurable duration. Discord-style: blocks send-message, add-reaction, join-voice, edit-nickname. Reads stay open. Timed-out user sees a yellow banner above the message input counting down the remaining time.
2. **Audit log** — `audit_events` table tracking who-did-what-to-whom-when across moderation and structural actions (~14 event types). Viewable by anyone with `MANAGE_SERVER`. Forever retention.
3. **Kicked/banned-user notification** — server sends a pre-disconnect frame so the client shows a clean dialog ("You were kicked from <server>" / "You were banned: <reason>") instead of the generic "Connection lost" flash.

All three are server-authoritative and backwards-compatible (`#[serde(default)]` on new fields, new event variants tolerated by old clients).

## Non-goals

- **Bulk select moderation** — defer (no demand).
- **Audit log filters/search** — Phase 3 if needed; v2 is just paginated chronological view.
- **Audit log retention/pruning** — forever, no cap.
- **Kick reason** — kicks stay reasonless; adding a reason field is a one-liner future addition if requested.
- **DM blocking on timeout** — timeout is server-scoped; DMs remain functional.
- **VIEW_AUDIT_LOG separate permission bit** — `MANAGE_SERVER` gates audit access.

## Architecture

Three coordinated additions, all following Phase 1's established patterns:
- New permission bit (`TIMEOUT_MEMBERS = 1 << 14`) added next to `KICK_MEMBERS`/`BAN_MEMBERS`.
- New columns on `members` (`timeout_until`, `timeout_reason`) via the established idempotent `pragma table_info` migration pattern.
- New `audit_events` table.
- New protocol additions (requests, responses, events, error variant) — all backwards-compatible.
- New context-menu items (timeout/untimeout) following Phase 1's hide-don't-disable rows-array builder.
- New tab in `ServerSettingsDialog` (Audit Log) using the same tab pattern Phase 1 established for Banned Members.
- Timeout state propagates via the existing `MembersChanged` event; no new events for member updates needed.
- Audit events broadcast live via a new `AuditEventCreated` event, filtered to `MANAGE_SERVER` holders.

## Permission bit

```rust
// crates/farder-server/src/permissions.rs
pub const TIMEOUT_MEMBERS: u64 = 1 << 14;
```

Add to `ALL_PERMISSIONS`. Hierarchy enforcement uses the existing `require_member_hierarchy` helper — same rule as kick/ban (can't act on equal-or-higher rank, including the owner).

## Protocol additions

`crates/farder-protocol/src/server.rs`:

```rust
// New request variants on ServerRequest
TimeoutMember { member: PublicKey, until_ms: u64, reason: Option<String> },
RemoveTimeout { member: PublicKey },
ListAuditEvents { before_id: Option<u64>, limit: u32 },

// New response variant
AuditEventsList { events: Vec<AuditEvent> },

// New broadcast events on ServerEvent
MemberTimeoutChanged { member: PublicKey, until_ms: Option<u64>, reason: Option<String> },
YouWereKicked,                              // sent to target only, before disconnect
YouWereBanned { reason: Option<String> },   // sent to target only, before disconnect
AuditEventCreated { event: AuditEvent },    // sent to MANAGE_SERVER holders only

// New error variant on ServerError
TimedOut { until_ms: u64, reason: Option<String> },

// New struct
pub struct AuditEvent {
    pub id: u64,
    pub actor: PublicKey,
    pub target: Option<PublicKey>,
    pub action: String,                  // "kick", "ban", "timeout", "channel_created", etc.
    pub metadata: serde_json::Value,     // free-form per-action context
    pub timestamp_ms: u64,
}
```

`MemberInfo` gains two `#[serde(default)]` fields:
```rust
#[serde(default)] pub timeout_until: Option<u64>,
#[serde(default)] pub timeout_reason: Option<String>,
```

**Why string + JSON for audit events instead of a tagged enum:** the audit list is a read-mostly UI render. Adding new event types in the future doesn't break the wire format if `action` is a string and `metadata` is `serde_json::Value`. Strict typing buys little.

## Database schema

Both via the established idempotent `pragma table_info` / `CREATE TABLE IF NOT EXISTS` pattern in `db.rs`:

```sql
-- Add to members
ALTER TABLE members ADD COLUMN timeout_until INTEGER;     -- UNIX ms; NULL = no timeout
ALTER TABLE members ADD COLUMN timeout_reason TEXT;       -- NULL when no timeout

-- New audit_events table
CREATE TABLE IF NOT EXISTS audit_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    actor_pk BLOB NOT NULL,
    target_pk BLOB,
    action TEXT NOT NULL,
    metadata TEXT NOT NULL DEFAULT '{}',  -- JSON object stored as text
    timestamp_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_events_timestamp ON audit_events(timestamp_ms DESC);
CREATE INDEX IF NOT EXISTS idx_audit_events_actor ON audit_events(actor_pk);
```

Indexes:
- `timestamp_ms DESC` — viewer pages newest-first
- `actor_pk` — supports a future "filter by user" UI without a migration
- No `target_pk` index — uncommon query, scan is fine at Farder scale

## Server implementation

### `members.rs` additions

```rust
pub fn set_timeout(conn: &Connection, pk: &PublicKey, until_ms: u64, reason: Option<&str>) -> Result<()>;
pub fn clear_timeout(conn: &Connection, pk: &PublicKey) -> Result<()>;

/// Returns active timeout details if the member is currently timed out.
/// Lazily clears the column if the timestamp has passed (so the next read is clean).
pub fn is_timed_out(conn: &Connection, pk: &PublicKey, now_ms: u64) -> Result<Option<(u64, Option<String>)>>;
```

### Timeout enforcement at handler tops

In `handlers.rs`, at the top of `SendMessage`, `AddReaction`, `JoinVoice`, `SetDisplayName`:

```rust
if let Some((until_ms, reason)) = members::is_timed_out(conn, &actor_pk, now_ms())? {
    return Err(ServerError::TimedOut { until_ms, reason });
}
```

DM sending is intentionally NOT blocked — timeout is server-scoped.

### `TimeoutMember` / `RemoveTimeout` handlers

```rust
ServerRequest::TimeoutMember { member, until_ms, reason } => {
    require_permission(perms, TIMEOUT_MEMBERS)?;
    if let Some(denied) = require_member_hierarchy(conn, member_acting, is_owner, &member)? {
        return Err(denied);
    }
    let now = now_ms();
    if until_ms <= now || until_ms > now + 28 * 24 * 60 * 60 * 1000 {
        return Err(ServerError::InvalidRequest("timeout duration out of range (1ms - 28d)".into()));
    }
    members::set_timeout(conn, &member, until_ms, reason.as_deref())?;
    broadcast(ServerEvent::MemberTimeoutChanged { member, until_ms: Some(until_ms), reason: reason.clone() });
    audit::emit(conn, &actor_pk, Some(&member), "timeout", json!({"until_ms": until_ms, "reason": reason}))?;
    Ok(ServerResponse::Ok)
}
```

`RemoveTimeout` mirrors this — clears timeout, broadcasts `MemberTimeoutChanged { until_ms: None, reason: None }`, emits `"untimeout"` audit event.

### Audit emission helper

New `crates/farder-server/src/audit.rs`:

```rust
pub fn emit(
    conn: &Connection,
    actor: &PublicKey,
    target: Option<&PublicKey>,
    action: &str,
    metadata: serde_json::Value,
) -> Result<AuditEvent> {
    let id = insert_row(conn, actor, target, action, &metadata)?;
    let event = AuditEvent { id, actor: *actor, target: target.copied(), action: action.into(), metadata, timestamp_ms: now_ms() };
    broadcast_to_manage_server_holders(ServerEvent::AuditEventCreated { event: event.clone() });
    Ok(event)
}
```

`audit::emit` is called at the bottom of each successful mutating handler (after the broadcast). 14 call sites:
- kick, ban, unban, timeout, untimeout
- assign_role, remove_role
- create_channel, delete_channel, update_channel (rename or reorder)
- create_role, delete_role, role-perms-changed
- channel/category overrides changed (set_channel_override)

Each call site is one line. If `audit::emit` fails (DB error), log the error but don't fail the parent action — the mutation has already succeeded and rolling it back for an audit-write failure would be a worse UX.

### Metadata schema per action

| action | metadata example |
|---|---|
| `kick` | `{}` |
| `ban` | `{"reason": "spam"}` |
| `unban` | `{}` |
| `timeout` | `{"until_ms": 1234567890, "reason": "warning"}` |
| `untimeout` | `{}` |
| `role_assigned` | `{"role_id": 5, "role_name": "Mod"}` |
| `role_removed` | `{"role_id": 5, "role_name": "Mod"}` |
| `channel_created` | `{"channel_id": 12, "channel_name": "general", "channel_type": "Text"}` |
| `channel_deleted` | `{"channel_id": 12, "channel_name": "general"}` |
| `channel_renamed` | `{"channel_id": 12, "old_name": "x", "new_name": "y"}` |
| `role_created` | `{"role_id": 5, "role_name": "Mod", "permissions": "..."}` |
| `role_deleted` | `{"role_id": 5, "role_name": "Mod"}` |
| `role_perms_changed` | `{"role_id": 5, "old_permissions": "...", "new_permissions": "..."}` |
| `channel_overrides_changed` | `{"channel_id": 12, "role_id": 5, "old_allow": "...", "old_deny": "...", "new_allow": "...", "new_deny": "..."}` |

### `ListAuditEvents` handler

```rust
ServerRequest::ListAuditEvents { before_id, limit } => {
    require_permission(perms, MANAGE_SERVER)?;
    let limit = limit.min(100);  // server-side cap, defense against client bug
    let events = audit::list(conn, before_id, limit)?;
    Ok(ServerResponse::AuditEventsList { events })
}
```

### Kicked/banned notification

In the `KickMember` / `BanMember` handler arms, after the DB mutation but before tearing down the target's connection:

```rust
if let Some(target_conn) = clients.get(&target_pk) {
    let _ = target_conn.send(ServerEvent::YouWereKicked).await;
    // give QUIC a moment to flush before drop
    tokio::time::sleep(Duration::from_millis(50)).await;
}
clients.remove(&target_pk);
// ... rest of existing tear-down
```

Best-effort — if the target is already disconnected, the send fails silently and we proceed. The 50ms sleep is the cost of the dignity-preserving notification.

## Client implementation

### Permission helper

`client/src/lib/permissions.ts`:
```ts
export const PERMISSIONS = {
  // ... existing
  TIMEOUT_MEMBERS: 1n << 14n,
};
```

### Tauri bridge bindings

`client/src/lib/tauri-bridge.ts` gets:
```ts
export async function timeoutMember(serverId: string, memberPk: string, untilMs: number, reason: string | null): Promise<void>;
export async function removeTimeout(serverId: string, memberPk: string): Promise<void>;
export async function listAuditEvents(serverId: string, beforeId: number | null, limit: number): Promise<AuditEvent[]>;
```

Plus the corresponding Rust commands in `client/src-tauri/src/commands.rs`.

### UI components

#### 1. `MemberContextMenu.tsx` — add timeout row

Between the existing "Kick" and "Ban" rows, conditionally push:

```ts
const isTimedOut = target.timeout_until && target.timeout_until > Date.now();
if (canTimeout && !isSelf) {
  if (isTimedOut) {
    rows.push({ kind: "item", label: "Remove timeout", onClick: () => removeTimeoutMember(target) });
  } else {
    rows.push({ kind: "item", label: "Timeout…", onClick: () => openTimeoutDialog(target) });
  }
}
```

`canTimeout` checks `(myPerms & PERMISSIONS.TIMEOUT_MEMBERS) === PERMISSIONS.TIMEOUT_MEMBERS`.

#### 2. `TimeoutDialog.tsx` (new)

Modal mirroring `BanConfirmDialog`'s look. Layout:
- Top: 6 preset radio buttons (60s, 5min, 10min, 1hr, 1day, 1wk)
- Below: a "Custom duration" toggle that swaps the radio group for a number input + unit dropdown (minutes / hours / days, max 28d enforced client-side and server-side)
- Optional reason textarea (200-char cap)
- "Until 5:42 PM" preview text
- "Time out" / "Cancel" buttons

#### 3. `TimeoutBanner.tsx` (new)

Rendered inside `MessageInput.tsx` above the input row. Reads `activeServer.me.timeout_until`. If non-null and in the future:
- Yellow strip with text: "You're timed out for 4m 23s. Reason: <reason>"
- Updates every second via `setInterval` (cleared on unmount or when timeout expires)
- Disables the textarea + send button while active (defense-in-depth — server is the truth)
- When the countdown hits 0, banner disappears and input re-enables locally; next message attempt confirms with the server

#### 4. `ServerSettingsDialog.tsx` — add Audit Log tab

Visible only with `MANAGE_SERVER`. New tab next to "Banned Members". Renders `AuditLogTab.tsx`.

#### 5. `AuditLogTab.tsx` (new)

- Initial load: `listAuditEvents(serverId, null, 50)`
- Render rows newest-first: `<actor avatar> <actor name> <action verb> <target avatar+name if present> · <relative time>`
  - Action verbs: "kicked", "banned", "unbanned", "timed out", "removed timeout from", "assigned role X to", "removed role X from", "created channel #x", "deleted channel #x", "renamed channel #x to #y", "created role X", "deleted role X", "changed permissions for role X", "changed channel overrides for #x"
- Click a row → expands a detail panel showing the metadata JSON pretty-printed
- Bottom: "Load older" button → `listAuditEvents(serverId, oldest_loaded_id, 50)` and append
- Live-update: subscribed to `AuditEventCreated` event in `useServerEvents.ts` → dispatches `farder:audit-event-created` window event → `AuditLogTab` listens and prepends new rows
- Empty state: "No moderation actions recorded yet"

#### 6. `KickedBannedDialog.tsx` (new — single component handling both)

When `useServerEvents.ts` receives `YouWereKicked` or `YouWereBanned`:
- Set a top-level `kickedBannedReason: { kind: "kick" | "ban", reason: string | null, serverName: string } | null` state
- Render the dialog via the existing modal pattern
- Single OK button routes to the server picker (clears the active server in the same way `disconnect_server` does)
- Triggered BEFORE the connection-lost reducer dispatch fires (so the user sees this dialog instead of the "Connection lost" flash)

### `useServerEvents.ts` changes

Add cases for the three new events:
- `MemberTimeoutChanged` → dispatch a member-update reducer action (timeout_until + timeout_reason are part of MemberInfo, so this is a normal `member_updated` style update)
- `YouWereKicked` / `YouWereBanned` → render `KickedBannedDialog` with reason
- `AuditEventCreated` → `window.dispatchEvent(new CustomEvent("farder:audit-event-created", { detail: event }))`

## Edge cases

| Case | Handling |
|---|---|
| Timeout expires while user connected | Banner countdown reaches 0 → state clears → input re-enables. `is_timed_out` returns None for past timestamps and lazily clears the DB column. |
| User sets timeout on themselves | Server `require_member_hierarchy` rejects (can't act on equal rank). |
| Target offline when timed out | `MembersChanged` broadcast updates everyone; on target reconnect, `MemberInfo.timeout_until` is in the initial member-list payload so the banner appears immediately. |
| `audit::emit` fails after a successful action | Log error, don't fail the parent action. |
| Audit list with no events | Empty state ("No moderation actions recorded yet"). |
| Old client + new server, receives `YouWereKicked` | Unknown variant ignored gracefully (existing `#[serde(other)]` on ServerEvent); falls through to existing connection-lost flow. |
| New client + old server, calls `TimeoutMember` | Server returns "unknown request" error → UI surfaces inline ("Server doesn't support timeouts"). |
| User with active timeout tries to send via DM | Allowed (timeout is server-scoped). |
| User with active timeout edits an existing message | Allowed (timeout blocks new sends, not edits — matches Discord). Document in plan. |
| Two mods race to timeout the same user | Last write wins; both audit events emit. |

## Testing

**Server unit tests** (in existing `members.rs` / `handlers.rs` test modules + new `audit.rs` test module):
- `set_timeout` + `is_timed_out` happy path
- `is_timed_out` lazy-clears expired column
- `is_timed_out` returns None when no timeout set
- `TimeoutMember` requires `TIMEOUT_MEMBERS` perm
- `TimeoutMember` enforces `require_member_hierarchy`
- `TimeoutMember` rejects `until_ms <= now` and `until_ms > now + 28d`
- `SendMessage` returns `TimedOut` error when actor is timed out
- `AddReaction` / `JoinVoice` / `SetDisplayName` likewise return `TimedOut`
- `RemoveTimeout` clears the column and broadcasts
- `audit::emit` writes the row + returns the event
- `audit::list` paginates correctly via `before_id`
- One test per audit call site asserting "action X emits audit event Y" (14 tests)
- `ListAuditEvents` requires `MANAGE_SERVER`
- `ListAuditEvents` clamps limit to 100

**Server integration test:**
- Two in-process clients: A kicks B → B receives `YouWereKicked` event before disconnect.

**Client tests:** none (no JS test infra in repo). Manual smoke list in the plan.

## Backwards compatibility

- New permission bit doesn't affect existing roles (default 0 for the new bit means no one has it until granted; owner still gets ALL via `is_owner` short-circuit).
- New columns on `members` are nullable.
- New protocol variants are unknown-tolerant on old clients via `#[serde(other)]` on ServerEvent / ServerResponse.
- New `MemberInfo` fields use `#[serde(default)]` so old wire frames decode.
- New `audit_events` table is created via `CREATE TABLE IF NOT EXISTS`.

## Files to create / modify

**New (server):**
- `crates/farder-server/src/audit.rs`

**Modified (server):**
- `crates/farder-server/src/permissions.rs` — add `TIMEOUT_MEMBERS`
- `crates/farder-server/src/db.rs` — schema additions (members columns, audit_events table)
- `crates/farder-server/src/members.rs` — `set_timeout`, `clear_timeout`, `is_timed_out`, plus including timeout fields in `MemberInfo` rows
- `crates/farder-server/src/handlers.rs` — 4 enforcement-check insertions (SendMessage, AddReaction, JoinVoice, SetDisplayName), 2 new handler arms (TimeoutMember, RemoveTimeout), 1 new handler arm (ListAuditEvents), 14 `audit::emit` call-site insertions, 2 pre-disconnect notification sends (kick/ban arms)
- `crates/farder-server/src/lib.rs` — `pub mod audit;`

**Modified (protocol):**
- `crates/farder-protocol/src/server.rs` — new request/response/event/error variants + `AuditEvent` struct + `MemberInfo` field additions

**Modified (client Rust):**
- `client/src-tauri/src/commands.rs` — 3 new Tauri commands
- `client/src-tauri/src/main.rs` — register new commands
- `client/src-tauri/src/bridge.rs` — emit MemberTimeoutChanged, YouWereKicked, YouWereBanned, AuditEventCreated to renderer

**New (client TS):**
- `client/src/components/TimeoutDialog.tsx`
- `client/src/components/TimeoutBanner.tsx`
- `client/src/components/AuditLogTab.tsx`
- `client/src/components/KickedBannedDialog.tsx`

**Modified (client TS):**
- `client/src/lib/permissions.ts` — add `TIMEOUT_MEMBERS`
- `client/src/lib/tauri-bridge.ts` — 3 new function exports + `AuditEvent` type
- `client/src/components/MemberContextMenu.tsx` — add timeout/untimeout rows
- `client/src/components/MessageInput.tsx` — render `<TimeoutBanner>` above input row
- `client/src/components/ServerSettingsDialog.tsx` — add Audit Log tab
- `client/src/components/useServerEvents.ts` — handle 3 new events

**Modified (docs):**
- `CHANGELOG.md` — entry for Phase 2
