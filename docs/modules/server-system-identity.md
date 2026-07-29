# Server system identity ("Farder")

> **File(s):** `crates/farder-server/src/bots.rs` (identity + DM senders), `crates/farder-server/src/members.rs` (`list_members_visible`), `crates/farder-server/src/db.rs` (the `idx_bots_system` index), `crates/farder-server/src/handlers.rs` (`GetMembers` / `RemoveBot` exclusion points)
> **Layer:** Server crate
> **Last reviewed:** 2026-07-28

## Purpose

Some server-side machinery needs to **speak to one member privately** without a
human or a configured bot behind it: an event reminder ("your party starts in an
hour"), an event-cancelled notice, a `/remind` nudge. The DM path
(`bots::send_bot_dm`) already existed, but it needs a **keypair** to author and
seal the message — and every keypair the server had belonged to a user-created
ticker/custom bot.

This module is the answer: **one** keypair, owned by the server itself, created
**lazily on first use** and reused forever. It exists to send DMs and to author
the sweeper's event-start announcement. It deliberately does **not** appear in
any roster, cannot be managed by anyone, and cannot authenticate a connection —
it is machinery, not a member.

---

## Public interface

### `bots::get_or_create_system_identity(conn: &Connection) -> Result<PublicKey>`

**What it does:** returns the server's own public key, minting it on first call.
**Parameters:** `conn` — the caller already holds the single `state.db` mutex.
**Returns / emits:** the `PublicKey` of the `bots` row with `kind = 'system'`.
**Side effects:** on first call only — generates a `farder_crypto::identity::Keypair`,
inserts a `bots` row (`kind = 'system'`, `label = 'Farder'`, empty `coin_id`,
secret bytes from `kp.signing_key_bytes()`) **and** a `members` row via
`members::register_bot_member` (required: `handlers::build_member_info` errors
without one). Never called from `init_schema` or at boot, so a server that never
runs an event or a reminder never mints one.
**Connects to:** `send_system_dm` (its only automatic caller), the sweeper's
event-start pass (`widgets::sweep_once`, which resolves it once per tick and
**only** when `channel_events::list_start_due` actually returned rows).

**Idempotence** is doubly guaranteed: callers hold the one DB mutex, and
`db.rs` creates `CREATE UNIQUE INDEX idx_bots_system ON bots(kind) WHERE kind = 'system'`
as belt-and-braces — a second row is a hard SQL error, not a silent second identity.

### `bots::send_bot_dm_as(state: &Arc<ServerState>, bot_pk: &PublicKey, recipient_pk: &PublicKey, text: &str, name_override: Option<&str>, badge: Option<&str>) -> Result<()>`

**What it does:** the generalized DM sender — the shipped `send_bot_dm` body with
two extra display knobs. `send_bot_dm` is now a thin wrapper passing `None, None`,
so its callers and behavior are unchanged.
**Parameters:** `name_override` / `badge` stamp `messages.author_name_override` /
`author_badge` on the inserted row (how the DM renders in the client).
**Side effects:** one scoped `state.db` lock doing `get_bot_secret` (missing →
early `Ok(())`) → `channels::open_dm_channel` → `encrypt_bot_dm` →
`messages::insert_message` → the read-backs; the guard is **dropped** before the
`DmCreated`/`NewMessage` broadcasts to `EventTarget::Members(vec![recipient])`.
**Why `state: &Arc<ServerState>` and not just `&Connection`:**
`handlers::build_member_info(conn, state, pk)` requires it.

### `bots::send_system_dm(state: &Arc<ServerState>, recipient_pk: &PublicKey, text: &str) -> Result<()>`

**What it does:** sends a DM **as the server itself**.
**Side effects:** takes its own scoped lock to resolve/mint the identity, **drops
it**, then delegates to `send_bot_dm_as(.., Some("Farder"), Some("BOT"))`.
**The caller must NOT hold the DB mutex** — this is precisely why
`widgets::sweep_once` returns DMs as `PendingDm` **data** instead of sending them
inline. Badge `"BOT"` is reused deliberately: a `"SYSTEM"` badge would need CSS
in three themes for no product gain.

---

## The four exclusion points (all server-side)

The identity is invisible and unmanageable. Each point is enforced in the server,
not in the UI:

1. **`GetMembers`** — sources its list from `members::list_members_visible(conn)`
   (`SELECT … WHERE banned = 0 AND revoked = 0 AND public_key NOT IN (SELECT public_key FROM bots WHERE kind = 'system')`).
   The filter runs in SQL, i.e. **before**
   the mesh roster whitelist `all_members.retain(|m| m.is_bot || ls.is_member(&m.public_key))`
   can re-admit it on the `is_bot ||` clause. This single filter is what keeps it
   out of **both** client surfaces: `MemberSidebar` and `BotsTab` each derive from
   `activeServer.members`, so no client change was needed.
2. **`bots::list_bots`** — `WHERE kind != 'system'`. The bot poller must never
   poll it (its `coin_id` is empty) and no bot UI enumerates it.
3. **`RemoveBot`** — refuses `kind = 'system'` with
   `Error { "that identity can't be removed" }` **before doing anything**.
   Defense in depth: the key is never listed, but a modified client could name it.
4. **No auth path reads `bots.secret_key`.** Connection authentication verifies a
   challenge signature against the `members` table; bot secrets are read only by
   `get_bot_secret` inside the DM sender. Possessing the row does not make the
   identity loggable-in.

## Known gotchas

- **The secret is the same trust class as the existing ticker-bot secrets** —
  stated openly rather than implied. The server holds it in `bots.secret_key` and
  seals DMs with it, exactly as it already does for bot alert DMs. It is not E2E
  to the recipient, and the E2EE feature matrix classifies it accordingly
  (see [[2026-07-27-mesh-rung2-e2ee-design]] rows 2 and 20).
- **It has no roles.** `register_bot_member` inserts an `is_bot = 1` member with
  no role grants, so every permission check it could ever hit fails closed.
- **Laziness is load-bearing, not an optimization.** Minting at boot would put a
  phantom "Farder" row (and a keypair) on every server that never uses the
  feature; worse, an eager mint inside `init_schema` would run before the
  `members` table is guaranteed usable.

## Integration map

- **`widgets.rs`** — the sweeper's reminder pass and three event passes return
  `PendingDm { recipient, text }`; `spawn_widget_sweeper` calls `send_system_dm`
  for each **after** dropping the DB guard. See `server-widgets.md`.
- **`channel_events.rs`** — `start_and_announce` takes the system `PublicKey` and
  authors the `📅 <title> is starting now!` reply with it
  (`author_name_override = "Events"`, `author_badge = "BOT"`).
- **`handlers.rs`** — `GetMembers` / `RemoveBot` exclusion points above; see
  `server-handlers.md` for the per-arm tables.
