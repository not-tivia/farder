# Server auth & permissions

> **File(s):** `crates/farder-server/src/auth.rs`, `crates/farder-server/src/permissions.rs`, `crates/farder-server/src/handlers.rs` (resolution helpers), `crates/farder-server/src/members.rs` (roles), `crates/farder-server/src/channels.rs` (overrides)
> **Layer:** Server crate
> **Last reviewed:** 2026-06-04

## Purpose

This group of modules answers two questions for every incoming request: (1) **Is this connection who it claims to be, and are they allowed on the server at all?** (`auth.rs`), and (2) **Does this authenticated member have the specific permission required to perform this action?** (`permissions.rs` + the resolution helpers in `handlers.rs`). `members.rs` is the source of truth for roles and their permission bits; `channels.rs` stores per-channel and per-category overrides. Neither auth nor permissions touches network I/O directly — they read SQLite and return plain `Result` values that the connection/handler layer acts on.

---

## Permission flag reference

All flags are `u64` bit constants defined in `permissions.rs`. Each is a distinct power-of-two bit; they are combined by OR and tested by AND.

| Constant | Bit | What it gates |
|---|---|---|
| `VIEW_CHANNEL` | 1 << 0 | Seeing that a channel exists in the channel list |
| `READ_MESSAGES` | 1 << 1 | Fetching message history, searching, adding reactions |
| `SEND_MESSAGES` | 1 << 2 | Posting messages, typing indicator, starting threads |
| `MANAGE_MESSAGES` | 1 << 3 | Deleting others' messages; pinning/unpinning messages |
| `CONNECT` | 1 << 4 | Joining a voice channel lobby (`JoinChannelMedia`) *(Phase 4)* |
| `SPEAK` | 1 << 5 | Reserved for future use — not yet checked in handlers *(Phase 4)* |
| `STREAM` | 1 << 6 | Reserved for future use — not yet checked in handlers *(Phase 4)* |
| `MANAGE_CHANNEL` | 1 << 7 | Creating, editing, deleting channels; setting channel overrides |
| `MANAGE_ROLES` | 1 << 8 | Creating, editing, deleting roles; assigning/removing roles from members |
| `MANAGE_SERVER` | 1 << 9 | Creating/editing/deleting categories; setting category overrides; viewing audit log |
| `KICK_MEMBERS` | 1 << 10 | Removing a member from the server (`KickMember`) |
| `BAN_MEMBERS` | 1 << 11 | Banning, unbanning, and listing the ban list |
| `ADMIN` | 1 << 12 | Grants all permissions; overrides cannot remove it from a member who has it via a role |
| `CREATE_INVITES` | 1 << 13 | Generating invite codes (`CreateInvite`) |
| `TIMEOUT_MEMBERS` | 1 << 14 | Silencing a member for up to 28 days (`TimeoutMember`/`RemoveTimeout`) |

`ALL_PERMISSIONS` is the OR of every flag above. `DEFAULT_EVERYONE` pre-grants `VIEW_CHANNEL | READ_MESSAGES | SEND_MESSAGES | CREATE_INVITES` — this is the initial permission set assigned to the built-in `@everyone` role on a fresh server.

---

## Public interface — `permissions.rs`

### `has(permissions: u64, permission: u64) -> bool`

**What it does:** returns `true` if every bit set in `permission` is also set in `permissions`. Works for single flags and for combined masks.
**Side effects:** none.
**Connects to:** called in nearly every `handle_request` branch in `handlers.rs` after a resolution call returns a `u64`.

---

### `resolve(ctx: ResolutionContext) -> u64`

**What it does:** computes the member's final effective permission set for a channel, following a layered algorithm (see "How permissions are resolved" below).
**Parameters:** `ctx` — a `ResolutionContext` struct containing all inputs needed for one resolution; see the struct fields below.
**Returns:** a `u64` bitmask of granted permissions.
**Side effects:** none — pure function over the context.
**Connects to:** called by the private `resolve_member_perms` function in `handlers.rs`, which builds the context from the database.

#### `ResolutionContext` fields

| Field | Type | Meaning |
|---|---|---|
| `everyone_permissions` | `u64` | Permission bits on the built-in `@everyone` role |
| `role_permissions` | `Vec<u64>` | One entry per non-@everyone role the member holds |
| `category_overrides` | `Vec<Override>` | Allow/deny pairs from the channel's parent category, filtered to the member's roles |
| `channel_overrides` | `Vec<Override>` | Allow/deny pairs from the channel itself, filtered to the member's roles |
| `is_owner` | `bool` | Whether the caller is the server owner |

#### `Override` struct

Each `Override` carries two `u64` fields: `allow` (bits to force-grant) and `deny` (bits to force-revoke). Stored in `channel_overrides` and `category_overrides` tables; read via `channels::get_channel_overrides_for_roles` / `get_category_overrides_for_roles`.

---

## How permissions are resolved (step-by-step)

`permissions::resolve` implements this algorithm exactly:

1. **Owner shortcut** — if `is_owner` is true, return `ALL_PERMISSIONS` immediately. No further checks.
2. **Base = @everyone** — start with the `@everyone` role's permission bits.
3. **OR in all role bits** — for each role the member holds, OR its permission bits into the base. Roles are additive; there is no concept of a role "removing" permissions at this stage.
4. **Record ADMIN** — note whether `ADMIN` is set at this point (before overrides). This protects admin members from having ADMIN stripped by a deny override.
5. **Apply category overrides** — union all allow bits across every applicable category override; union all deny bits. Then: `perms &= !combined_deny; perms |= combined_allow`. Category overrides narrow or expand permissions for an entire category of channels.
6. **Apply channel overrides** — same union-then-apply logic as step 5, but scoped to the specific channel. Channel overrides take final precedence over category overrides for the same bit.
7. **Restore ADMIN if it was earned from roles** — if ADMIN was set before overrides, restore it now. This ensures a deny override cannot revoke ADMIN from an actual admin.
8. **ADMIN grants all** — if ADMIN is still set after the override steps, return `ALL_PERMISSIONS`.
9. **Return** the resulting `perms`.

**Implication of the union approach:** within the same override level (e.g. channel), if a bit appears in both allow and deny, allow wins (deny is applied first, then allow re-sets the bit). To actually deny a bit at that level, only set deny.

---

## Public interface — `handlers.rs` (resolution helpers)

### `resolve_member_perms_pub(conn, member, channel_id, is_owner) -> Result<u64>`

**What it does:** public-visibility wrapper around the private `resolve_member_perms`. Fetches the member's roles, the `@everyone` permissions, category overrides, and channel overrides from SQLite, then calls `permissions::resolve`.
**Parameters:** `conn` — open SQLite connection; `member` — the requesting member's public key; `channel_id` — channel being accessed; `is_owner` — passed in by the connection handler.
**Returns:** resolved permission bits for that member in that channel, or an `Err` on database failure.
**Side effects:** multiple read queries against `roles`, `member_roles`, `channel_overrides`, `category_overrides`.
**Connects to:** exposed so tests or other crates can resolve permissions without going through the full request handler.

---

### `resolve_member_server_perms(conn, member, is_owner) -> Result<u64>`

**What it does:** like `resolve_member_perms_pub` but with empty override slices — computes server-wide permissions only (no channel or category override applied). Used when a check is conceptually a server-level operation rather than a channel-level one.
**Connects to:** not currently called from `handle_request` (which uses the private `resolve_member_perms`); available for external callers and tests.

---

### `handle_request(conn, member, is_owner, request, ...) -> Result<HandleResult>`

**What it does:** the main dispatch function. Receives an authenticated `(member, is_owner)` pair and a `ServerRequest`, performs the required permission checks, executes the operation, and returns a `HandleResult` (response + broadcast events + orphaned file IDs).
**Parameters:** `member` — the caller's `PublicKey` as established by the auth handshake; `is_owner` — true if the connection-level code identified this public key as the server owner; all permission checks short-circuit to allow when this is true.
**Side effects:** database reads/writes; may append `BroadcastEvent` entries for the connection layer to fan-out to subscribers.
**Connects to:** called once per `ServerRequest` by the connection handler. Auth has already been verified before this is called — `member` is trusted.

#### Per-request permission pattern

Every request branch follows one of two patterns:

- **Channel-scoped check** — calls the private `resolve_member_perms(conn, member, channel_id, is_owner)` to get the full permission set including category and channel overrides, then calls `permissions::has(perms, permissions::FLAG)`. Used for message and channel operations where the specific channel matters.
- **Base/server-scoped check** — calls `require_base_perm(conn, member, is_owner, permissions::FLAG, "FLAG")`. This resolves server-level permissions only (no overrides), and returns an early `Error` response if the bit is missing. Used for operations that are not tied to a specific channel (create category, manage roles, etc.).

Additionally:
- **`require_not_timed_out`** is called before any write that produces visible output (send message, add reaction, join voice). Timed-out members are blocked from these actions regardless of permission bits.
- **`require_role_hierarchy`** is called for role create/update/delete and assign/remove to prevent members from managing roles at or above their own highest role position.
- **`require_member_hierarchy`** is called for kick, ban, and timeout to prevent acting on members at or above the actor's own role level.

#### Quick per-action permission map

| Request | Check type | Required flag |
|---|---|---|
| `SendMessage` (normal channel) | channel-scoped | `SEND_MESSAGES` |
| `DeleteMessage` (others' message) | channel-scoped | `MANAGE_MESSAGES` |
| `FetchHistory` | channel-scoped | `READ_MESSAGES` |
| `PinMessage` / `UnpinMessage` | channel-scoped | `MANAGE_MESSAGES` |
| `Search` (channel-scoped) | channel-scoped | `READ_MESSAGES` |
| `Typing` | channel-scoped | `SEND_MESSAGES` |
| `CreateChannel` | base | `MANAGE_CHANNEL` |
| `UpdateChannel` / `DeleteChannel` | channel-scoped | `MANAGE_CHANNEL` |
| `SetChannelOverride` | channel-scoped | `MANAGE_CHANNEL` |
| `CreateCategory` / `UpdateCategory` / `DeleteCategory` | base | `MANAGE_SERVER` |
| `SetCategoryOverride` | base | `MANAGE_SERVER` |
| `CreateRole` / `UpdateRole` / `DeleteRole` | base | `MANAGE_ROLES` + hierarchy |
| `AssignRole` / `RemoveRole` | base | `MANAGE_ROLES` + hierarchy |
| `KickMember` | base | `KICK_MEMBERS` + member hierarchy |
| `BanMember` / `UnbanMember` / `ListBanned` | base | `BAN_MEMBERS` (+ hierarchy for ban) |
| `CreateInvite` | base | `CREATE_INVITES` |
| `TimeoutMember` / `RemoveTimeout` | base | `TIMEOUT_MEMBERS` + member hierarchy |
| `ListAuditEvents` | base | `MANAGE_SERVER` |
| `JoinChannelMedia` | channel-scoped | `CONNECT` |
| `CreateThread` | channel-scoped | `SEND_MESSAGES` |
| `AddReaction` | channel-scoped | `READ_MESSAGES` |

---

## Public interface — `auth.rs`

### `generate_challenge() -> [u8; 32]`

**What it does:** returns 32 cryptographically random bytes to be sent to a connecting client as the auth challenge.
**Side effects:** none.

### `generate_session_token() -> [u8; 32]`

**What it does:** returns 32 cryptographically random bytes to use as a session token after authentication succeeds.
**Side effects:** none.

### `generate_setup_token() -> [u8; 32]`

**What it does:** returns 32 cryptographically random bytes to use as the one-time setup token for the first owner claim. The server holds this token in memory (not persisted to DB); it is cleared once the first owner registers.
**Side effects:** none.

---

### `verify_challenge(public_key, challenge, signature) -> Result<()>`

**What it does:** verifies that `signature` is a valid Ed25519 signature of `challenge` by `public_key`. This is the core identity proof step — the client must sign the challenge with the private key that corresponds to its claimed public key.
**Parameters:** `challenge` — the 32 bytes the server sent; `signature` — the bytes the client returned.
**Returns:** `Ok(())` on success; `Err` if the signature is invalid (wrong key, tampered challenge, wrong format).
**Side effects:** none.
**Connects to:** `farder_crypto::identity::PublicKey::verify`; called by the connection handshake code before any `handle_request` call.

---

### `authenticate_new_member(conn, public_key, display_name, invite_code, setup_token_hex, active_setup_token) -> Result<Result<bool, String>>`

**What it does:** attempts to register a new member via one of three paths, in priority order:

1. **Setup-token path** — if `setup_token_hex` is provided, compare against `active_setup_token`. On match, register the member and assign `@everyone`. Returns `Ok(Ok(false))`.
2. **Invite-code path** — if `invite_code` is provided, call `invites::use_invite`. On success, register and assign `@everyone`. Returns `Ok(Ok(false))`.
3. **Auto-claim path** — if the `members` table is empty (zero members), register without a token or invite. Returns `Ok(Ok(true))` to signal the caller that this member is the auto-claimed first owner.

If none of the above apply (server has members and no valid credential was provided), returns `Ok(Err("no invite code or setup token provided"))`.

**Returns (inner Result):**
- `Ok(true)` — registered as the auto-claimed first owner.
- `Ok(false)` — registered via setup token or invite.
- `Err(reason)` — rejected (wrong token, expired invite, server already has an owner, etc.).
**Returns (outer Result):** `Err` only on unexpected database failures.
**Side effects:** writes to `members` and `member_roles` tables; calls `invites::use_invite` which may decrement a use counter or mark an invite consumed.
**Connects to:** `members::register_member`, `members::assign_role`, `invites::use_invite`.

---

### `authenticate_existing_member(conn, public_key) -> Result<Result<(), String>>`

**What it does:** checks that a member who has already registered is still in good standing. Looks up the member record and rejects if they are banned or revoked.
**Returns (inner Result):** `Ok(())` on success; `Err(reason)` if the member is banned, revoked, or not found.
**Returns (outer Result):** `Err` only on unexpected database failures.
**Side effects:** one read query against `members`.
**Connects to:** `members::get_member`; called on every reconnect from a known public key.

---

## How auth fits into the connection flow

The connection handler (outside these modules) drives the handshake:

1. On new QUIC connection, the server calls `auth::generate_challenge()` and sends it to the client.
2. The client signs the challenge with its identity private key and sends back its public key + signature.
3. The server calls `auth::verify_challenge(public_key, challenge, signature)`. On failure, the connection is dropped.
4. The server checks whether this public key is already registered:
   - **Known member** → calls `auth::authenticate_existing_member`. Rejected if banned or revoked.
   - **Unknown public key** → calls `auth::authenticate_new_member` with whatever credential the client provided. Rejected if no valid path.
5. On success, the connection is marked as authenticated with `(member: PublicKey, is_owner: bool)`. Every subsequent `ServerRequest` is passed directly to `handle_request` with these two values.

---

## Integration map

- **`farder_crypto::identity`** (`PublicKey`, `Keypair`) — `verify_challenge` delegates signature verification to `public_key.verify(challenge, signature)`. The public key IS the member's identity; there is no separate username/password.
- **`members.rs`** — source of role data (`get_member_role_ids`, `get_member_role_permissions`, `get_highest_role_position`) consumed by the resolution helpers in `handlers.rs`. Also exposes `get_member` (used by `authenticate_existing_member`) and role CRUD used by `handle_request`.
- **`channels.rs`** — provides `get_channel_overrides_for_roles` and `get_category_overrides_for_roles`, which are the only channel-specific inputs to `permissions::resolve`. `set_channel_override` / `set_category_override` are the write side, gated behind `MANAGE_CHANNEL` / `MANAGE_SERVER`.
- **`invites.rs`** — `authenticate_new_member` calls `invites::use_invite` for the invite-code path. Invite lifecycle (expiry, use counts) lives entirely in `invites.rs`.
- **`handlers.rs`** — the single consumer of both `auth.rs` (indirectly, via the connection layer) and `permissions.rs` (directly). The resolution functions `resolve_member_perms`, `resolve_member_server_perms`, `require_base_perm`, `require_role_hierarchy`, and `require_member_hierarchy` all live in `handlers.rs`, not in `permissions.rs`.

---

## Known gotchas

- **`is_owner` is a runtime flag, not a permission bit.** The server owner is not identified by the `ADMIN` bit in a role — they are identified by comparing the authenticated public key to the owner key stored at the connection level. `is_owner = true` bypasses every permission check, including all override logic, before any bit is even looked at.
- **ADMIN cannot be stripped by channel/category overrides** — if a member earns ADMIN through their roles (step 3 of resolution), the code records that fact before applying overrides and restores it afterward. A deny override on the ADMIN bit for a specific channel has no effect on an actual admin.
- **`require_base_perm` vs. `resolve_member_perms`** — base-perm checks skip all overrides. This means a deny override on (e.g.) `MANAGE_ROLES` set at the channel level does NOT prevent the member from managing roles — that check never sees the channel context. This is intentional: server-scoped operations should not be blockable per-channel.
- **`SPEAK` and `STREAM` flags exist but are not yet enforced** — they are defined in `permissions.rs` and included in `ALL_PERMISSIONS`, but no handler branch currently calls `permissions::has(perms, SPEAK)` or similar. Checking for them will require a Phase 4 handler update.
- **`@everyone` role is builtin = 1** — the resolution helpers fetch it with `WHERE name = '@everyone' AND builtin = 1`. If this row is missing (shouldn't happen on a properly initialized server), the permission query returns 0 and all members silently lose their base permissions. Initialization code must create this role.
- **Overrides are role-scoped, not member-scoped** — there are no per-member overrides, only per-role. A member's overrides are the union of overrides for all roles they hold. If two roles have conflicting overrides at the same level, allow always wins over deny within that level (the union of allows is applied after the union of denies).
