# Events (RSVP cards) + personal reminders Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax. **Each task below is executed by ONE build agent.** Dependency order: **T1 → T2 → T3 → T4, T5** (T4 and T5 both depend on T3; T5 also documents T1/T2/T3/T4 surfaces, so run it last).

**Spec:** [[2026-07-28-events-reminders-design]]. **Precedent plan:** [[2026-07-27-poll-giveaway-widgets]] (the shipped widget substrate this extends).

**Goal:** (a) `📅` **event cards** — a member posts an event with a start time; everyone RSVPs Going / Maybe / Can't make it and sees *who* is coming; the server DMs at a reminder lead, flips the card + announces at start, and DMs on cancel. (b) `/remind 90m take the pizza out` — a **private** reminder, nothing posted, a DM when due. (c) The enabling piece both need: **one lazily-created server system identity** that can send DMs.

**Architecture:** Everything rides the shipped substrate — `messages.widget` JSON pointer, `commands.kind` → `RunCommand` dispatch, `widgets::sweep_once` (15 s tick, persist-then-notify), `handlers::widget_channel_visible` opaque visibility, `state.widget_limiter` (10/10 s), default-deny `request_requires_membership`, builder modals as the primary creation UX.

**Tech stack:** Rust (`farder-server`, `farder-protocol`, `farder-crypto`), rusqlite, Tauri, React/TS.

---

## Global constraints (every task ends green on the gates it touches)

Run everything from **`/home/deez/farder-events`** (isolated worktree, branch `events-reminders`). Never `git push`. Never switch branches.

- `cargo test -p farder-server 2>&1 | tail -30`
- `cargo build --workspace 2>&1 | tail -20`
- `cd /home/deez/farder-events/client/src-tauri && cargo build 2>&1 | tail -20` — **NOT a workspace member.** Required after ANY `farder-protocol` change (the MemberApproved-class regression).
- `cd /home/deez/farder-events/client && npx tsc --noEmit`
- **Seam audit:** every `invoke("X")` in `tauri-bridge.ts` ⇔ `#[tauri::command] fn X` in `client/src-tauri/src/commands.rs` ⇔ an entry in `generate_handler!` in `client/src-tauri/src/main.rs`. Zero drift; grep-audit before committing.
- **Themes:** every new `className` styled in ALL THREE `client/src/themes/{xp-luna-blue,discord-dark,hello-kitty}/theme.css`, colors only via `var(--xp-…)`. Verify: `grep -l "<class>" client/src/themes/*/theme.css` lists 3 files.
- **Lock discipline:** no DB `MutexGuard` held across any `.await` (the `bots::spawn_bot_poll_task` / `widgets::spawn_widget_sweeper` scoped-block idiom).
- **Timestamps:** unix seconds via `db::now() -> u64`; DB columns are `INTEGER` and code casts to `i64` at the rusqlite boundary (the `polls.rs` convention).
- **Docs-with-code:** doc updates land in the same commit as the surface they document (final sweep in T5).
- **Commits:** conventional (`feat(events):`, `feat(reminders):`, `fix(...)`). Commit locally only. **Never commit red.**

## Verified-against-source facts (do not re-derive; do not contradict)

| Claim | Verified |
|---|---|
| `MSG_SELECT` tail | `messages.rs:12-13` — `…, author_name_override, author_badge, widget`; `widget` at index **10**. Unchanged by this plan. |
| `widgets::sweep_once` | `fn sweep_once(conn: &rusqlite::Connection, now: u64) -> Vec<PendingBroadcast>`; two `#[cfg(test)] mod tests` fns (`sweep_once_closes_due_poll_once`, `sweep_once_draws_due_giveaway_exactly_once`) that use `pending.len()`, `pending[0].event`, `pending[..].target`, `.is_empty()`. |
| `spawn_widget_sweeper` | scoped `state.db.lock()` → `sweep_once` → guard dropped → `for pb in pending { broadcast_event(&state, pb.target, pb.event).await }` → `sleep(WIDGET_SWEEP_SECS)`. Spawned at `crates/farder-server/src/main.rs:144`. |
| `widget_limiter` | `state.rs:93` `pub widget_limiter: RateLimiter`, `state.rs:122` `RateLimiter::new(10, 10)`. Call form: `state.widget_limiter.allow(&caller_bytes)` where `caller_bytes: [u8; 32]`. |
| `ListActiveWidgets` handler | `handlers.rs:2592-2637`. `ACTIVE_WIDGETS_CAP = 20`. A **2-way merge loop** over `created_at` (`p.created_at <= g.created_at`), then `ok(ServerResponse::ActiveWidgets { polls, giveaways })`. Test helper `active_widgets(r)` at `handlers.rs:6845` destructures the 2-field variant. |
| `polls::list_open_in_channel` | `(conn, channel_id: i64, now: i64, limit: u32) -> Result<Vec<PollRow>>`. |
| `BotsTab` kind selector | `BotsTab.tsx:45` `useState<"text" \| "api" \| "poll" \| "giveaway">("text")`; `:136` `isWidgetKind`; `:531-537` the `<select>`; `:556`/`:562` hint blocks; `:619` the Add-button enable expression. `BotsTab.tsx:23` `const bots = (activeServer?.members ?? []).filter((m) => m.is_bot)` — **so one `GetMembers` filter removes the system identity from both the roster and BotsTab.** |
| `PollBuilderModal` duration control | `DURATIONS` array (incl. `{value:"custom",label:"Custom…"}`), `MIN_DURATION_SECS=60`, `MAX_DURATION_SECS=30*86_400`, `UNIT_SECS`, `resolveDurationToken(duration, amount, unit): string \| null`, custom row = `<input type="number" min={1} max={9999}>` + a minutes/hours/days `<select class="connect-input">`, `.error-text` on violation. `stripPipes(s)` replaces `|` with `/`. |
| `LinkedWidgetCard` | props `{ serverId, link: WidgetLink, messageChannelId }`; `WidgetLink { kind: "poll" \| "giveaway"; channelId; widgetId }`; computes `refetch = sameChannel ? "mount" : "interval"` itself; failure card `.linked-widget-unavailable`. |
| `Message.tsx` link machinery | `INVITE_REGEX` (`:33`), `WIDGET_LINK_REGEX` (`:40`), `INVITE_SPLIT_REGEX` (`:109`), `isWidgetSchemeLink` (`:113`, prefix test), `isWidgetLink` (`:118`, anchored full-token), `parseWidgetLink` (`:125`), `isInviteLink` (`:134`, delegates to `isWidgetSchemeLink`), `copyWidgetLink` (`:146`), pill render in `renderContent` (`:181-193`), invite-embeds IIFE (`:585-605`, filters with `isWidgetSchemeLink`), linked-widget IIFE (`:607-633`), widget slot `parsedWidget`/`widgetNode`/`showWidget` (`:355-394`). |
| `AlertSubscriptions.tsx` | `client/src/components/settings/AlertSubscriptions.tsx`. Shape: `useActiveServerId()` → `useEffect` fetch → `<div className="settings-panel"><h2 className="settings-panel-title">…` → `.error-text` → `<SettingsSection label="…">` → per-row `.organizer-row` > `.organizer-name` + `.organizer-actions` > `button.organizer-btn.organizer-delete`. All `.organizer-*` + `.settings-panel*` classes exist in all 3 themes. |
| `bots::send_bot_dm` | `bots.rs:491` `pub async fn send_bot_dm(state: &Arc<ServerState>, bot_pk: &PublicKey, recipient_pk: &PublicKey, text: &str) -> Result<()>`. Body: one scoped `state.db.lock()` doing `get_bot_secret` (missing → early `Ok(())`) → `channels::open_dm_channel` → `encrypt_bot_dm` → **`messages::insert_message`** → `messages::get_message` → `channels::get_channel` → `handlers::build_member_info(&conn, state, bot_pk)`; guard dropped; then `DmCreated` (if created) + `NewMessage` to `EventTarget::Members(vec![recipient])`. |
| `bots` table | `db.rs:326` `bots(public_key BLOB PK, secret_key BLOB NOT NULL, kind TEXT NOT NULL, coin_id TEXT NOT NULL, label TEXT NOT NULL, created_at INTEGER NOT NULL)` + migrated nullable `source_url`, `value_path`, `unit`. `register_bot` hardcodes `kind='crypto_ticker'`; `register_custom_bot` hardcodes `'custom_api'`. `list_bots` (`bots.rs:40`) selects with **no WHERE**. |
| Key material | `farder_crypto::identity::Keypair::generate()`; `kp.public_key()`; secret bytes via **`kp.signing_key_bytes()`** (the `AddBot` handler idiom, `handlers.rs:2069-2072`). |
| `members` | `register_bot_member(conn, pk, display_name)` inserts `is_bot = 1`. `list_members(conn)` = `SELECT … FROM members WHERE banned = 0 AND revoked = 0`. `get_member(conn, pk) -> Result<Option<MemberRecord>>` with `.display_name`. |
| `GetMembers` | `handlers.rs:1213-1222` — `members::list_members(conn)` then the mesh retain `all_members.retain(\|m\| m.is_bot \|\| ls.is_member(&m.public_key))`. |
| `RemoveBot` | `handlers.rs:2079-2093` — MANAGE_SERVER, then `bots::remove_bot` + `members::remove_member_row` + presence removal + `MemberLeft`. |
| Handler helpers | `ok(resp)`, `ok_with(resp, events)`, `err(&str)` (all `-> Result<HandleResult>`); `require_not_timed_out(conn, member) -> Result<Option<HandleResult>>`; `require_base_perm(conn, member, is_owner, perm, perm_name) -> Result<Option<HandleResult>>`; `widget_channel_visible(conn, member, channel_id: u64, is_owner) -> Result<bool>` (`handlers.rs:367`); `request_requires_membership` allow-list is 4 entries (`handlers.rs:393`) — **add nothing to it.** |
| Broadcast plumbing | `events::EventTarget::{All, Subscribers(u64), Members(Vec<PublicKey>), …}`; `events::BroadcastEvent { target, event }`. |
| Client `PublicKey` JSON | plain serde → `{ bytes: number[] }` (TS: `creator: { bytes: number[] }`, read with `publicKeyToString`). Only `Option<PublicKey>` needed the `giveaway_json` string mapping — **`EventInfo` has no optional pubkey, so it passes through raw like `PollInfo`.** |
| `bridge.rs` `dispatch_event` | ends with `_ => Ok(())`, so new `ServerEvent` variants never break the client crate build. |
| `client/src-tauri` `ActiveWidgets` | `commands.rs:4841` `struct ActiveWidgets { polls: Vec<PollInfo>, giveaways: Vec<serde_json::Value> }`; `list_active_widgets` destructures `ServerResponse::ActiveWidgets { polls, giveaways }`. |
| `run_command` | `client/src-tauri/src/commands.rs:2695` → `Result<(), String>`, arms `Ok`/`Error`/catch-all. `tauri-bridge.ts:977` `runCommand(...): Promise<void>`. Call sites: `MessageInput.tsx:236`, `PollBuilderModal`, `GiveawayBuilderModal` — all `await` and discard the result. |
| `MessageInput` builders | `builder` state `{ kind, trigger }`; kind-union checks at `:228` (handleSend), `:322` (insertCommand); modal renders at `:572` / `:581`; `handleBuilderCreated()` closes + clears. |
| `SettingsModal` | `SectionId` union at `:13`, `SECTIONS` array `:15-22`, render switch at `:60-66`. |
| Docs that exist | `docs/modules/server-widgets.md`, `frontend-state.md`, `tauri-commands.md`, `tauri-bridge.md`, `protocol.md`, `server-handlers.md`, `ARCHITECTURE.md`. |

## Spec corrections applied by this plan (build agents: follow the plan, not the spec, where they differ)

1. **`docs/modules/frontend-context.md` does not exist.** The ServerContext/useServerEvents doc is **`docs/modules/frontend-state.md`**. (`frontend-hooks.md` likewise does not exist.)
2. **E2EE rung-2 is NOT implemented in `farder-server`** — no `e2ee` column, no "encrypted channels" refusal anywhere in the crate. Spec §9 therefore requires **zero code**; the only deliverable is the feature-matrix rows in the rung-2 *design spec*, which T5 adds. Do not invent an E2EE gate.
3. **`ListActiveWidgets` needs a 3-way merge**, not "unchanged merge discipline": the shipped loop is a 2-way `created_at` merge. T2 rewrites it as a 3-way merge (see T2 Step 7).
4. **The `sweep_once` test change is ~6 lines, not one**: both shipped tests bind `let pending = sweep_once(...)` and then index/`len()`/`is_empty()` it. T1 changes them to `let out = sweep_once(...); let pending = out.broadcasts;` (+ `assert!(out.dms.is_empty())`).
5. **`send_bot_dm_as` must keep `state: &Arc<ServerState>`** — `handlers::build_member_info(conn, state, pk)` requires it.
6. **`AlertSubscriptions` row markup includes a `.organizer-actions` wrapper** around the button; `MyReminders` must mirror it (still zero new CSS).
7. `MIN_DURATION_SECS`/`MAX_DURATION_SECS` in `polls.rs`/`giveaways.rs` are **private** consts — `reminders.rs` defines its own.
8. `GetMembers` is at `handlers.rs:1213` (spec said 1212). Cosmetic.

---

### Task 1: SYSTEM IDENTITY + REMINDERS (server)

**Files:** `crates/farder-server/src/db.rs`; `crates/farder-server/src/bots.rs`; `crates/farder-server/src/members.rs`; `crates/farder-server/src/reminders.rs` (**new**); `crates/farder-server/src/lib.rs`; `crates/farder-server/src/widgets.rs`; `crates/farder-server/src/handlers.rs`; `crates/farder-server/src/connection.rs`; `crates/farder-server/src/commands.rs`; `crates/farder-protocol/src/server.rs`.

**Interfaces produced:** `bots::{SYSTEM_BOT_KIND, SYSTEM_BOT_LABEL, get_or_create_system_identity, send_bot_dm_as, send_system_dm}`; `members::list_members_visible`; the `reminders` module; `ReminderInfo`; `ServerRequest::{ListMyReminders, CancelReminder}`; `ServerResponse::{Notice, MyReminders}`; `widgets::{PendingDm, SweepOutcome}` + the new `sweep_once` signature; RunCommand kind `"reminder"`.

- [ ] **Step 1: DDL + the system-identity index.** `db.rs` `init_schema`, immediately **after** the `giveaway_entries` block (`db.rs:~525-532`), guarded `CREATE TABLE IF NOT EXISTS` (covered by the existing `test_schema_init_idempotent` by construction):

```sql
CREATE TABLE IF NOT EXISTS reminders (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    owner      BLOB    NOT NULL,
    channel_id INTEGER NOT NULL,
    text       TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    due_at     INTEGER NOT NULL,
    status     TEXT    NOT NULL DEFAULT 'pending',
    sent_at    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_reminders_due   ON reminders(status, due_at);
CREATE INDEX IF NOT EXISTS idx_reminders_owner ON reminders(owner, status, due_at);
```

Also, next to the `bots` DDL (`db.rs:~326`):
```sql
CREATE UNIQUE INDEX IF NOT EXISTS idx_bots_system ON bots(kind) WHERE kind = 'system';
```
(Belt-and-braces: the single `state.db` mutex already serializes lookup-then-insert.)

- [ ] **Step 2: `bots.rs` — the system identity.** Add near the top of the file:

```rust
/// `bots.kind` discriminator for the server's own identity (exactly one row).
pub const SYSTEM_BOT_KIND: &str = "system";
/// Display name + `author_name_override` used for every system-sent DM.
pub const SYSTEM_BOT_LABEL: &str = "Farder";

/// The server's own identity: lazily created on first use, then reused forever.
/// Never created at boot / in `init_schema` — a server that never fires a
/// reminder or starts an event never mints one.
pub fn get_or_create_system_identity(conn: &Connection) -> Result<PublicKey>
```
Body: `SELECT public_key FROM bots WHERE kind = 'system' LIMIT 1` (`.optional()?`) → present ⇒ `PublicKey::from_bytes(arr)`. Absent ⇒ `let kp = Keypair::generate(); let pk = kp.public_key();` then **both** inserts:
```rust
members::register_bot_member(conn, &pk, SYSTEM_BOT_LABEL)?;   // is_bot = 1; REQUIRED (build_member_info errors without it)
conn.execute(
    "INSERT INTO bots (public_key, secret_key, kind, coin_id, label, created_at) \
     VALUES (?1, ?2, 'system', '', ?3, ?4)",
    params![pk.as_bytes().as_slice(), kp.signing_key_bytes().as_slice(), SYSTEM_BOT_LABEL, crate::db::now() as i64],
)?;
```
Return `pk`.

- [ ] **Step 3: `bots.rs` — DRY DM refactor (no second copy of the plumbing).**

```rust
pub async fn send_bot_dm(state: &Arc<ServerState>, bot_pk: &PublicKey,
                         recipient_pk: &PublicKey, text: &str) -> Result<()> {
    send_bot_dm_as(state, bot_pk, recipient_pk, text, None, None).await
}

/// The former `send_bot_dm` body, with `insert_message` swapped for
/// `insert_message_with_author_name` so a non-roster sender still renders.
pub async fn send_bot_dm_as(state: &Arc<ServerState>, bot_pk: &PublicKey,
                            recipient_pk: &PublicKey, text: &str,
                            name_override: Option<&str>, badge: Option<&str>) -> Result<()>

/// Send a DM as the server itself (lazily minting the identity on first use).
pub async fn send_system_dm(state: &Arc<ServerState>, recipient_pk: &PublicKey,
                            text: &str) -> Result<()>
```
`send_bot_dm_as`: identical to the shipped body except the insert becomes
`crate::messages::insert_message_with_author_name(&conn, channel_id, bot_pk, &hex_ct, None, name_override, badge)?`.
**Lock discipline unchanged** — all DB + crypto inside the scoped block; the `MutexGuard` is dropped before the first `broadcast_event`; the doc-comment at `bots.rs:485` stays true.
`send_system_dm`: `let pk = { let conn = state.db.lock().unwrap(); get_or_create_system_identity(&conn)? };` (guard dropped) then `send_bot_dm_as(state, &pk, recipient_pk, text, Some(SYSTEM_BOT_LABEL), Some("BOT")).await`. **Badge `"BOT"` is reused deliberately** — a `"SYSTEM"` badge would need CSS in three themes for no product gain.

- [ ] **Step 4: `bots.rs` + `members.rs` + `handlers.rs` — exclusion from every list.**
  1. `bots::list_bots` → add `WHERE kind != 'system'` to its SELECT (the ticker poller must never poll it; its `coin_id` is empty).
  2. `members.rs`, new fn beside `list_members`:
```rust
/// `list_members` minus the server's own system identity — the roster query used
/// by `GetMembers`. The exclusion runs BEFORE the mesh `is_bot ||` whitelist so
/// that clause (which keeps ticker bots visible on mesh servers) can never
/// re-admit it.
pub fn list_members_visible(conn: &Connection) -> Result<Vec<MemberRecord>>
```
Same SQL as `list_members` plus `AND public_key NOT IN (SELECT public_key FROM bots WHERE kind = 'system')`.
  3. `handlers.rs:1214` — `GetMembers` calls `members::list_members_visible(conn)?` instead of `members::list_members(conn)?`. Nothing else in that arm changes.
  4. `handlers.rs:2079` `RemoveBot` — **before** `bots::remove_bot`, add:
```rust
let kind: Option<String> = conn.query_row(
    "SELECT kind FROM bots WHERE public_key = ?1",
    rusqlite::params![bot_public_key.as_bytes().as_slice()], |r| r.get(0)).optional()?;
if kind.as_deref() == Some(crate::bots::SYSTEM_BOT_KIND) {
    return err("that identity can't be removed");
}
```

- [ ] **Step 5: `reminders.rs` (new module; `pub mod reminders;` in `lib.rs`, alphabetically after `reactions`).**

```rust
pub const MAX_REMINDER_TEXT: usize = 500;
pub const MAX_PENDING_PER_USER: i64 = 20;
const MIN_DURATION_SECS: u64 = 60;             // 1m
const MAX_DURATION_SECS: u64 = 30 * 86_400;    // 30d
const REMINDER_USAGE: &str =
    "usage: /<trigger> <duration> <text> — e.g. /remind 90m take the pizza out (1m–30d)";
pub const REMINDER_DUE_BATCH: usize = 200;

pub struct ParsedReminder { pub delay_secs: u64, pub text: String }
pub struct ReminderRow {
    pub id: i64, pub owner: PublicKey, pub channel_id: i64, pub text: String,
    pub created_at: i64, pub due_at: i64, pub status: String, pub sent_at: Option<i64>,
}

pub fn parse_reminder_args(args: &str) -> Result<ParsedReminder, String>;
pub fn create(conn: &Connection, owner: &PublicKey, channel_id: i64, text: &str,
              due_at: i64, now: i64) -> Result<i64>;
pub fn count_pending(conn: &Connection, owner: &PublicKey) -> Result<i64>;
pub fn list_pending_for(conn: &Connection, owner: &PublicKey) -> Result<Vec<ReminderRow>>;
pub fn cancel(conn: &Connection, id: i64, owner: &PublicKey) -> Result<bool>;
pub fn list_due(conn: &Connection, now: i64) -> Result<Vec<ReminderRow>>;
pub fn mark_sent(conn: &Connection, id: i64, now: i64) -> Result<bool>;
pub fn to_info(row: &ReminderRow) -> ReminderInfo;
```

Semantics (exact):
- `parse_reminder_args`: `args.trim().splitn(2, char::is_whitespace)`. Token 1 must match `^(\d{1,4})([mhd])$` **case-insensitively** (`m`=60, `h`=3600, `d`=86400); no match / missing → `Err(REMINDER_USAGE.into())`. Out of `[MIN_DURATION_SECS, MAX_DURATION_SECS]` → `Err("duration must be between 1m and 30d".into())`. Text = remainder `.trim()`; empty → `Err(REMINDER_USAGE.into())`; `> MAX_REMINDER_TEXT` chars → `Err("reminder text must be 1-500 characters".into())`. Text is preserved **verbatim** (pipes and all — the grammar has no delimiter past the first space).
- `create`: plain insert, `status` defaulted `'pending'`.
- `count_pending`: `SELECT COUNT(*) FROM reminders WHERE owner = ?1 AND status = 'pending'`.
- `list_pending_for`: `WHERE owner = ?1 AND status = 'pending' ORDER BY due_at ASC LIMIT 20`.
- `cancel`: `UPDATE reminders SET status='cancelled' WHERE id=?1 AND owner=?2 AND status='pending'` → rows-affected > 0.
- `list_due`: `WHERE status='pending' AND due_at <= ?1 ORDER BY due_at ASC LIMIT 200` (bounded batch; the remainder drains next tick).
- `mark_sent`: `UPDATE reminders SET status='sent', sent_at=?2 WHERE id=?1 AND status='pending'` → rows-affected > 0 (**single-shot guard**).

- [ ] **Step 6: Protocol (`crates/farder-protocol/src/server.rs`, appended).**

```rust
/// One of the caller's own pending reminders (owner-scoped by the connection key —
/// `ListMyReminders` carries no owner field, so there is nothing to forge).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReminderInfo {
    pub id: i64,
    pub text: String,
    pub due_at: u64,      // absolute unix secs
    pub created_at: u64,
    pub channel_id: u64,  // where it was set (link-back context)
}

// ServerRequest (appended; NOT added to request_requires_membership's allow-list)
ListMyReminders,
CancelReminder { reminder_id: i64 },

// ServerResponse (appended)
/// Invoker-only confirmation delivered on the request's own request_id: no
/// broadcast, no message row. The `Ok`-but-say-something case.
Notice { text: String },
MyReminders { reminders: Vec<ReminderInfo> },
```

- [ ] **Step 7: `widgets.rs` — `SweepOutcome` + the reminder pass.**

```rust
/// A DM computed under the DB lock, sent after the guard drops (send_system_dm
/// re-acquires the mutex, which is safe only once the sweeper's guard is gone).
pub struct PendingDm { pub recipient: farder_crypto::identity::PublicKey, pub text: String }

pub struct SweepOutcome { pub broadcasts: Vec<PendingBroadcast>, pub dms: Vec<PendingDm> }

pub fn sweep_once(conn: &rusqlite::Connection, now: u64) -> SweepOutcome   // was -> Vec<PendingBroadcast>
```
Existing poll/giveaway halves are byte-identical apart from pushing into `out.broadcasts`.
**Reminder pass** (append, after the giveaway half):
```rust
match crate::reminders::list_due(conn, now as i64) {
    Ok(rows) => for row in rows {
        match crate::reminders::mark_sent(conn, row.id, now as i64) {
            Ok(false) => continue,                       // already sent/cancelled — skip
            Err(e) => { tracing::warn!("widget sweeper: reminder {} mark_sent failed: {e}", row.id); continue }
            Ok(true) => {}
        }
        let chan = crate::channels::get_channel(conn, row.channel_id as u64).ok().flatten();
        let name = chan.map(|c| c.name).unwrap_or_else(|| "a channel".to_string());
        out.dms.push(PendingDm {
            recipient: row.owner.clone(),
            text: format!("⏰ {}\n— set in #{} · farder://channel/{}", row.text, name, row.channel_id),
        });
    },
    Err(e) => tracing::warn!("widget sweeper: reminder list_due failed: {e}"),
}
```
**A reminder produces ZERO broadcasts.** Persist-before-notify under the `status='pending'` guard ⇒ **at-most-once**: a crash between the guarded flip and the DM loses at most one nudge and can never duplicate one.

`spawn_widget_sweeper` loop body becomes:
```rust
let out: SweepOutcome = { let conn = state.db.lock().unwrap(); sweep_once(&conn, crate::db::now()) };
// MutexGuard dropped here — before any .await
for pb in out.broadcasts { crate::connection::broadcast_event(&state, pb.target, pb.event).await; }
for dm in out.dms { let _ = crate::bots::send_system_dm(&state, &dm.recipient, &dm.text).await; }
tokio::time::sleep(std::time::Duration::from_secs(WIDGET_SWEEP_SECS)).await;
```
Update both shipped tests: `let out = sweep_once(&conn, now); let pending = out.broadcasts;` + `assert!(out.dms.is_empty())`; the idempotence assertions become `sweep_once(&conn, now).broadcasts.is_empty()`.

- [ ] **Step 8: Dispatch kind `"reminder"` + `AddCommand` + `takes_arg`.**
  - `commands.rs:136` → `takes_arg: matches!(r.kind.as_str(), "api" | "poll" | "giveaway" | "event" | "reminder")` (**both new kinds now — T2 does not touch this line**).
  - `handlers.rs:2283` → `"poll" | "giveaway" | "reminder" => {}` and `:2284` error text → `"kind must be 'text', 'api', 'poll', 'giveaway', 'event' or 'reminder'"` (the `"event"` arm itself lands in T2; the text is widened once, here).
  - `connection.rs` `RunCommand` — a new `if cmd.kind.as_str() == "reminder" { … }` branch **beside** the `"poll"` branch (`connection.rs:1025`), i.e. **after every existing gate runs unchanged**: `content_block_reason` → `command_limiter` → `check_run_command_channel_auth` → trigger lookup.
    1. `reminders::parse_reminder_args(&args)` — pure, no lock. `Err(reason)` → `ServerResponse::Error { reason }` to the invoker; **nothing posts**.
    2. One scoped `state.db.lock()`: `count_pending(&conn, &member_key)? >= MAX_PENDING_PER_USER` → error `"you already have 20 reminders pending — cancel one first"`; else `create(&conn, &member_key, channel_id as i64, &parsed.text, (now + parsed.delay_secs) as i64, now as i64)`. Guard drops (no `.await` inside).
    3. Reply `ServerResponse::Notice { text: format!("⏰ Reminder set for {} — I'll DM you.", humanize(parsed.delay_secs)) }` where `humanize` is a small local helper producing `"90m"`-style → `"1h 30m"` / `"3 days"`; keep it in `reminders.rs` as `pub fn humanize_delay(secs: u64) -> String` so it is unit-testable.

- [ ] **Step 9: Handler arms (`handlers.rs`, sync).** Neither is added to `request_requires_membership`'s allow-list (default-deny does the mesh gating).
  - **`ListMyReminders`** — no timeout gate (read), no channel visibility (a reminder is not channel content). `ok(ServerResponse::MyReminders { reminders: reminders::list_pending_for(conn, member)?.iter().map(reminders::to_info).collect() })`.
  - **`CancelReminder { reminder_id }`** — `state.widget_limiter.allow(&caller_bytes)` (consistency with other mutations; over-limit → `err("slow down")`), **no** timeout gate (managing your own private state is not channel content) → `reminders::cancel(conn, reminder_id, member)?`; `false` → **`err("reminder not found")`** — the byte-identical string for a foreign id, an already-fired one, and a nonexistent one (no oracle) → `true` → `ok(ServerResponse::Ok)`.

- [ ] **Step 10: Tests.**
  `reminders.rs mod tests` (pure): `parse_happy_90m_taco`; `parse_units_case_insensitive` (`2H`, `3D`); `parse_rejects_zero_and_over_bounds` (`0m`, `31d` → the duration message); `parse_rejects_garbage_and_missing_text` (`banana x`, `90m`, `""` → `REMINDER_USAGE`); `parse_rejects_501_char_text`; `parse_preserves_pipes_verbatim`; `humanize_delay_forms`.
  `reminders.rs` (in-memory conn via `crate::db::open_in_memory()`): `create_and_count_pending_is_per_owner`; `list_pending_for_never_returns_another_owner`; `cancel_guarded_by_owner` (foreign id → `false`, row still `pending`); `list_due_respects_status_due_at_and_batch_cap` (create 205 due → 200 returned); `mark_sent_is_single_shot` (second call `false`).
  `bots.rs mod tests`: `get_or_create_system_identity_is_idempotent` (call 3× → same pk, `SELECT COUNT(*) FROM bots WHERE kind='system'` == 1, and a `members` row exists with `is_bot`); `list_bots_excludes_system_identity`; `list_members_visible_excludes_system_while_list_members_includes_it`.
  `handlers.rs mod tests`: `test_get_members_hides_system_identity_legacy_and_mesh` (assert absent on a legacy server **and** with a mesh `log_state` present — the `is_bot ||` whitelist path); `test_remove_bot_refuses_system_identity` (error + the `bots` row survives); `test_list_my_reminders_returns_only_callers_rows`; `test_cancel_reminder_foreign_id_is_opaque_not_found` (error string == the one for a nonexistent id, and the foreign row is untouched); `test_add_command_accepts_reminder_kind_and_takes_arg`.
  `widgets.rs mod tests`: `sweep_once_due_reminder_produces_one_dm_and_flips_sent` (exactly one `PendingDm`, zero broadcasts, row `status='sent'`); `sweep_once_reminder_is_idempotent` (second `sweep_once` at the same `now` → `dms.is_empty()`); `sweep_once_ignores_cancelled_reminder`; plus the two shipped tests updated to the `SweepOutcome` shape.

- [ ] **Step 11: Gates + commit.**
```bash
cd /home/deez/farder-events
cargo test -p farder-server 2>&1 | tail -30
cargo build --workspace 2>&1 | tail -20
cd /home/deez/farder-events/client/src-tauri && cargo build 2>&1 | tail -20   # protocol changed
cd /home/deez/farder-events
git add crates/farder-protocol/src/server.rs crates/farder-server/src/db.rs crates/farder-server/src/bots.rs crates/farder-server/src/members.rs crates/farder-server/src/reminders.rs crates/farder-server/src/lib.rs crates/farder-server/src/widgets.rs crates/farder-server/src/handlers.rs crates/farder-server/src/connection.rs crates/farder-server/src/commands.rs
git commit -m "feat(reminders): server system identity + reminders table/module + /remind kind + sweeper reminder pass"
```
> The client crate still maps `ServerResponse::Notice` into its catch-all `other => Err(...)` arm at this point — that is fine and expected; **T3 widens `run_command`.**

---

### Task 2: EVENTS (server)

**Files:** `crates/farder-server/src/db.rs`; `crates/farder-server/src/channel_events.rs` (**new**); `crates/farder-server/src/lib.rs`; `crates/farder-protocol/src/server.rs`; `crates/farder-server/src/connection.rs`; `crates/farder-server/src/handlers.rs`; `crates/farder-server/src/widgets.rs`; `crates/farder-server/src/commands.rs` (**verify only — T1 already widened `takes_arg`**).

**Naming (non-negotiable):** the table `events` **already exists** (mesh signed log, `db.rs:92`) and `events.rs` is `EventTarget`/`BroadcastEvent`. New tables are **`channel_events` / `channel_event_rsvps`**; the new module is **`crates/farder-server/src/channel_events.rs`**. Protocol/client names stay product-facing: `EventInfo`, `GetEvent`, `EventUpdated`.

**Interfaces produced:** the `channel_events` module; `EventInfo`; `ServerRequest::{GetEvent, RsvpEvent, ClearRsvp, CancelEvent, EditEvent}`; `ServerResponse::Event`; `ActiveWidgets.events`; `ServerEvent::EventUpdated`; RunCommand kind `"event"`; the sweeper's three event passes.

- [ ] **Step 1: DDL.** `db.rs` `init_schema`, after the T1 `reminders` block:

```sql
CREATE TABLE IF NOT EXISTS channel_events (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id          INTEGER NOT NULL,
    message_id          INTEGER NOT NULL,
    creator             BLOB    NOT NULL,
    title               TEXT    NOT NULL,
    description         TEXT,
    location            TEXT,
    starts_at           INTEGER NOT NULL,
    remind_lead         INTEGER,
    reminded_at         INTEGER,
    status              TEXT    NOT NULL DEFAULT 'upcoming',
    started_at          INTEGER,
    cancelled_at        INTEGER,
    cancel_notified_at  INTEGER,
    created_at          INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_channel_events_due ON channel_events(status, starts_at);

CREATE TABLE IF NOT EXISTS channel_event_rsvps (
    event_id   INTEGER NOT NULL,
    member     BLOB    NOT NULL,
    response   TEXT    NOT NULL,          -- 'going' | 'maybe' | 'no'
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (event_id, member)
);
```
No FK to `messages` (deleting the card **cancels**; rows retained for audit). The three nullable `*_at` guard columns are what make each sweeper action exactly-once.

- [ ] **Step 2: Protocol (`crates/farder-protocol/src/server.rs`, appended).**

```rust
/// Live event state, broadcast whole on every change (`EventUpdated`) and returned
/// by `GetEvent`. Carries the roster as SERVER-RESOLVED DISPLAY NAMES ONLY, capped
/// at `ATTENDEE_NAME_CAP` per option — attendee public keys never leave the server,
/// and the payload stays bounded (worst case 30 short strings) at any RSVP volume.
/// The per-viewer `my_rsvp` rides solely in `ServerResponse::Event`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EventInfo {
    pub id: i64,
    pub channel_id: u64,
    pub message_id: u64,
    pub creator: PublicKey,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub starts_at: u64,            // absolute unix secs — no timezone stored anywhere
    pub remind_lead: Option<u64>,  // secs before start: 900 | 3600 | 86400
    pub status: String,            // "upcoming" | "started" | "cancelled"
    pub going_count: u32,
    pub maybe_count: u32,
    pub no_count: u32,
    pub going_names: Vec<String>,  // capped at 10
    pub maybe_names: Vec<String>,  // capped at 10
    pub no_names: Vec<String>,     // capped at 10
}

// ServerRequest (appended; NOT added to request_requires_membership's allow-list)
GetEvent    { event_id: i64 },
RsvpEvent   { event_id: i64, response: String },   // "going" | "maybe" | "no"
ClearRsvp   { event_id: i64 },
CancelEvent { event_id: i64 },
EditEvent   { event_id: i64, title: String, description: Option<String>,
              location: Option<String>, starts_at: u64, remind_lead: Option<u64> },

// ServerResponse (appended)
Event { event: EventInfo, my_rsvp: Option<String> },

// ServerResponse::ActiveWidgets gains a THIRD field (existing variant, edited)
ActiveWidgets {
    polls: Vec<PollInfo>,
    giveaways: Vec<GiveawayInfo>,
    #[serde(default)]
    events: Vec<EventInfo>,
},

// ServerEvent (appended)
EventUpdated { event: EventInfo },   // -> EventTarget::Subscribers(channel_id)
```
`EditEvent` is a **full replace** of the editable fields (same validation as creation) — no partial-patch ambiguity. No request ever carries an actor field; the actor is always the authenticated connection key. Terminal states fold into `status` (no `EventStarted`/`EventCancelled` variants).

- [ ] **Step 3: `channel_events.rs` (new module; `pub mod channel_events;` in `lib.rs`, after `channels`).** Mirrors `polls.rs`/`giveaways.rs` style, including a `const EVENT_SELECT: &str` + `fn row_to_event(row) -> rusqlite::Result<EventRow>` pair.

```rust
pub const ATTENDEE_NAME_CAP: usize = 10;
pub const MAX_TITLE: usize = 120;
pub const MAX_LOCATION: usize = 120;
pub const MAX_DESCRIPTION: usize = 500;
pub const MIN_LEAD_SECS: u64 = 60;                  // starts_at >= now + 60
pub const MAX_AHEAD_SECS: u64 = 365 * 86_400;       // starts_at <= now + 365d
pub const EVENT_USAGE: &str =
    "usage: /<trigger> Title | 3d [| location] [| description] [| remind 1h]";
/// Allowed reminder leads, in seconds.
pub const REMIND_LEADS: [u64; 3] = [900, 3600, 86_400];

pub struct EventRow {
    pub id: i64, pub channel_id: i64, pub message_id: i64, pub creator: PublicKey,
    pub title: String, pub description: Option<String>, pub location: Option<String>,
    pub starts_at: i64, pub remind_lead: Option<i64>, pub reminded_at: Option<i64>,
    pub status: String, pub started_at: Option<i64>, pub cancelled_at: Option<i64>,
    pub cancel_notified_at: Option<i64>, pub created_at: i64,
}

pub enum WhenSpec { Relative(u64), Absolute(u64) }
pub struct ParsedEvent {
    pub title: String, pub when: WhenSpec, pub location: Option<String>,
    pub description: Option<String>, pub remind_lead: Option<u64>,
}

pub fn parse_event_args(args: &str) -> Result<ParsedEvent, String>;                       // pure, no DB
pub fn resolve_start(when: &WhenSpec, now: u64) -> Result<u64, String>;                    // pure; applies the bounds
pub fn create_event_card(conn: &mut Connection, channel_id: u64, invoker: &PublicKey,
                         parsed: &ParsedEvent, now: u64) -> Result<(MessageInfo, EventInfo)>;
pub fn create(conn: &Connection, channel_id: i64, message_id: i64, creator: &PublicKey,
              parsed: &ParsedEvent, starts_at: i64, now: i64) -> Result<i64>;
pub fn get(conn: &Connection, id: i64) -> Result<Option<EventRow>>;
pub fn build_info(conn: &Connection, row: &EventRow) -> Result<EventInfo>;
pub fn rsvp(conn: &Connection, event_id: i64, member: &PublicKey, response: &str, now: i64) -> Result<()>;
pub fn clear_rsvp(conn: &Connection, event_id: i64, member: &PublicKey) -> Result<bool>;
pub fn my_rsvp(conn: &Connection, event_id: i64, member: &PublicKey) -> Result<Option<String>>;
pub fn responders(conn: &Connection, event_id: i64, responses: &[&str]) -> Result<Vec<PublicKey>>;
pub fn cancel(conn: &Connection, event_id: i64, now: i64) -> Result<bool>;
pub fn edit(conn: &Connection, event_id: i64, title: &str, description: Option<&str>,
            location: Option<&str>, starts_at: i64, remind_lead: Option<i64>,
            rearm_reminder: bool) -> Result<()>;
pub fn list_reminder_due(conn: &Connection, now: i64) -> Result<Vec<EventRow>>;
pub fn list_start_due(conn: &Connection, now: i64) -> Result<Vec<EventRow>>;
pub fn list_cancel_unnotified(conn: &Connection) -> Result<Vec<EventRow>>;
pub fn mark_reminded(conn: &Connection, id: i64, now: i64) -> Result<bool>;
pub fn mark_cancel_notified(conn: &Connection, id: i64, now: i64) -> Result<bool>;
pub fn start_and_announce(conn: &mut Connection, row: &EventRow, system_pk: &PublicKey,
                          now: i64) -> Result<Option<(EventInfo, MessageInfo)>>;
pub fn list_upcoming_in_channel(conn: &Connection, channel_id: i64, now: i64,
                                limit: u32) -> Result<Vec<EventRow>>;
```

Exact semantics:
- **`parse_event_args`** — split on `|`, trim each segment (the `/poll` idiom).
  1. If the **final** trimmed segment matches `^remind\s+(15m|1h|1d|none)$` **case-insensitively**, it is **always** consumed as the lead (`15m`→900, `1h`→3600, `1d`→86400, `none`→`None`) and removed. Deterministic, like the poll duration rule.
  2. Segment 1 = title (`1..=120`, else `Err("title must be 1-120 characters")`). Segment 2 = when (**required**; missing → `Err(EVENT_USAGE.into())`). Segment 3 (optional, may be empty ⇒ no location) = location (`<=120`, else `Err("location must be at most 120 characters")`). Segment 4 (optional) = description (`<=500`, else `Err("description must be at most 500 characters")`). More than 4 segments after lead removal → `Err(EVENT_USAGE.into())`. **Positional — no guessing.**
  3. `<when>` accepts exactly two forms: `^(\d{1,4})([mhd])$` (a leading `in ` is stripped first, so `in 3d` works) → `WhenSpec::Relative(secs)`; `^@(\d{9,12})$` → `WhenSpec::Absolute(unix_secs)` (what the builder emits). **Anything else** (including a wall-clock string like `2026-08-01 20:00`) → `Err("use the event builder for a date and time, or a relative time like `3d`".into())` — the server cannot know the invoker's timezone, and assuming UTC or server-local is exactly the bug class that lands an event 8 hours off.
- **`resolve_start`** — `Relative(d)` → `now + d`; `Absolute(t)` → `t`. Then bounds: `< now + MIN_LEAD_SECS` → `Err("the event must start at least a minute from now")`; `> now + MAX_AHEAD_SECS` → `Err("an event can be at most a year out")`.
- **`create_event_card`** — one `conn.transaction()` (the `polls::create_poll_card` template, `polls.rs:314`):
  fallback `content` for old clients = `format!("📅 {} — {}", title, utc_stamp)` + `\n📍 {location}` if present + `\n{description}` if present, where
```rust
/// UTC wall-clock stamp for the old-client fallback line. `farder-server` has NO
/// date-formatting dependency (no chrono/time in Cargo.toml — verified), so this
/// borrows SQLite's formatter rather than adding a crate or hand-rolling
/// civil-from-days. Falls back to the raw epoch string if the query ever fails.
fn utc_stamp(conn: &Connection, secs: i64) -> String {
    conn.query_row("SELECT strftime('%Y-%m-%d %H:%M UTC', ?1, 'unixepoch')", params![secs], |r| r.get::<_, String>(0))
        .unwrap_or_else(|_| format!("{secs}"))
}
```
  → `let mid = messages::insert_message(&tx, channel_id, invoker, &content, None)?` (**plain invoker authorship — no name override, no badge**) → `let eid = create(&tx, …)?` → `messages::set_widget(&tx, mid, &format!(r#"{{"type":"event","id":{eid}}}"#))?` → `messages::get_message(&tx, mid, invoker)?` + `get` + `build_info` → `tx.commit()`.
- **`build_info`** — one `SELECT response, member FROM channel_event_rsvps WHERE event_id = ?1 ORDER BY updated_at ASC, rowid ASC`; bucket by response; `*_count` = the **full** bucket length; `*_names` = the first `ATTENDEE_NAME_CAP` resolved via `members::get_member(conn, pk)?.map(|m| m.display_name)` (the `GiveawayInfo.winner_name` precedent), skipping members that no longer exist. The client renders "and N more" from `count - names.len()`.
- **`rsvp`** — `INSERT INTO channel_event_rsvps (event_id, member, response, updated_at) VALUES (?1,?2,?3,?4) ON CONFLICT(event_id, member) DO UPDATE SET response = excluded.response, updated_at = excluded.updated_at` (the `poll_votes` idiom).
- **`clear_rsvp`** — `DELETE … WHERE event_id=?1 AND member=?2` → rows-affected > 0 (the `polls::retract` idiom).
- **`responders`** — `SELECT member FROM channel_event_rsvps WHERE event_id = ?1 AND response IN (…)`, built with the right number of `?` placeholders.
- **`cancel`** — `UPDATE channel_events SET status='cancelled', cancelled_at=?2 WHERE id=?1 AND status='upcoming'` → rows-affected > 0.
- **`edit`** — `UPDATE channel_events SET title=?, description=?, location=?, starts_at=?, remind_lead=?` **plus `, reminded_at = NULL` when `rearm_reminder`** `WHERE id=? AND status='upcoming'`.
- **`list_reminder_due`** — `WHERE status='upcoming' AND remind_lead IS NOT NULL AND reminded_at IS NULL AND starts_at - remind_lead <= ?1 AND starts_at > ?1 ORDER BY id ASC` (the trailing clause means an event whose start also came due in the same tick skips the lead DM and gets only the start DM — **no double-ping**).
- **`list_start_due`** — `WHERE status='upcoming' AND starts_at <= ?1 ORDER BY id ASC`.
- **`list_cancel_unnotified`** — `WHERE status='cancelled' AND cancel_notified_at IS NULL ORDER BY id ASC`.
- **`mark_reminded` / `mark_cancel_notified`** — guarded `UPDATE … SET x=?2 WHERE id=?1 AND x IS NULL` → rows-affected > 0 (**skip on 0**).
- **`start_and_announce`** — one `conn.transaction()`:
  1. `UPDATE channel_events SET status='started', started_at=?2 WHERE id=?1 AND status='upcoming'` — **rows-affected 0** (a Cancel won the mutex first) ⇒ roll back and return `Ok(None)`, **announcing nothing**. This guard is what makes the announcement exactly-once across crashes and restarts.
  2. **In the same transaction:** `messages::insert_message_with_author_name(&tx, channel_id, system_pk, &format!("📅 {} is starting now!", title), Some(row.message_id as u64), Some("Events"), Some("BOT"))` — authored by the **system identity** (T1) with `reply_to` = the card, so it threads under the event.
  3. Build `EventInfo` + `MessageInfo`, commit, return `Ok(Some((info, msg)))`.
- **`list_upcoming_in_channel`** — `WHERE channel_id=?1 AND status='upcoming' AND starts_at > ?2 ORDER BY id ASC LIMIT ?3` (the `starts_at > now` half excludes due-but-unswept events, matching the RSVP cutoff's exactness).

- [ ] **Step 4: Dispatch — kind `"event"` (`connection.rs`).** New `if cmd.kind.as_str() == "event" { … }` branch beside `"poll"`/`"giveaway"`/`"reminder"`, **after every existing gate runs unchanged**: `content_block_reason` → `command_limiter` (5/10 s) → `check_run_command_channel_auth` (not-timed-out + DM-participant/blocked + `SEND_MESSAGES`). **That is the creation permission — NO MANAGE_SERVER gate.** Events are social: anyone who can post in the channel can plan one. (Deliberate divergence from `/giveaway`, which resolves MANAGE_SERVER at dispatch; giveaways hand out prizes, events do not.)
  1. `channel_events::parse_event_args(&args)` — pure, no lock. `Err(reason)` → `Error { reason }` to the invoker; nothing posts.
  2. One scoped `state.db.lock()` calling `channel_events::create_event_card(&mut conn, channel_id, &member_key, &parsed, crate::db::now())` (which resolves the start time internally via `resolve_start`, returning its `Err(reason)` as a user-facing error — mirror the `"poll"` branch's error mapping so a bounds violation is a plain `Error`, not `internal error:`). **Guard drops before any `.await`.**
  3. `broadcast_event(Subscribers(channel_id), NewMessage { message }).await`, then `broadcast_event(Subscribers(channel_id), EventUpdated { event }).await` (pre-seeds connected reducers so the widget mounts with state, no `GetEvent` round-trip), then `ServerResponse::Ok`.
  - `handlers.rs` `AddCommand`: add `"event"` to the no-extra-fields arm (T1 already widened the error text and `takes_arg`).

- [ ] **Step 5: Handler arms (`handlers.rs`, sync).** All five are membership-gated automatically — **do not touch `request_requires_membership`.**
  **Shared preamble** (write it once as a local closure/helper and use it in all five):
```rust
let row = match crate::channel_events::get(conn, event_id)? { Some(r) => r, None => return err("event not found") };
if !widget_channel_visible(conn, member, row.channel_id as u64, is_owner)? { return err("event not found"); }
```
  The string `"event not found"` is **byte-identical** for "no such event", "channel gone", "not a DM participant" and "no VIEW_CHANNEL" — an event id is never an existence oracle.
  - **`GetEvent`** — preamble only (**no** timeout gate; reads are allowed while timed out, matching `GetPoll`) → `ok(ServerResponse::Event { event: channel_events::build_info(conn, &row)?, my_rsvp: channel_events::my_rsvp(conn, event_id, member)? })`.
  - **`RsvpEvent { event_id, response }`** — `state.widget_limiter.allow(&caller_bytes)` (each RSVP fans out a broadcast) → `require_not_timed_out` → preamble → `matches!(response.as_str(), "going" | "maybe" | "no")` else `err("invalid RSVP")` → still open? `row.status == "upcoming"` else `err("this event was cancelled")` / (started) `err("this event has already started")`, **and** `(now as i64) < row.starts_at` else `err("this event has already started")` (makes the cutoff exact even before the sweeper ticks) → `channel_events::rsvp(...)` (upsert; an identical response rewrites the same row and still emits — harmless) → `ok_with(ServerResponse::Ok, vec![BroadcastEvent { target: Subscribers(row.channel_id as u64), event: EventUpdated { event: build_info(...) } }])`.
  - **`ClearRsvp { event_id }`** — same gates minus the response check → `clear_rsvp`; **no row deleted → `ok(ServerResponse::Ok)` with NO event** (idempotent, no broadcast noise — the `RetractVote` rule); else `ok_with(Ok, [EventUpdated])`.
  - **`CancelEvent { event_id }`** — `require_not_timed_out` → preamble → `row.status == "upcoming"` else `err("event already ended or cancelled")` → **authz: creator OR MANAGE_SERVER**: `if row.creator != *member { if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? { return Ok(denied); } }` → `channel_events::cancel(conn, event_id, now)` → `ok_with(Ok, [EventUpdated])`. **The DMs to the Going list are NOT sent here** — a sync handler cannot `.await`; the sweeper's cancel-notify pass drains `cancel_notified_at IS NULL` within ≤15 s. One DM code path, one crash-safety guard.
  - **`EditEvent { … }`** — `require_not_timed_out` → preamble → `row.status == "upcoming"` else `err("only an upcoming event can be edited")` → creator-or-MANAGE_SERVER (same expression as Cancel) → re-run **the same field validation as creation**: title `1..=120`, location `<=120`, description `<=500`, `now + 60 <= starts_at <= now + 365d`, `remind_lead` ∈ `{None} ∪ REMIND_LEADS` else `err("invalid reminder lead")` → `channel_events::edit(..., rearm_reminder = (starts_at as i64 != row.starts_at))` → `ok_with(Ok, [EventUpdated])`. If the new lead moment is already past but the start is still future, the next sweep fires the reminder immediately — **documented and deliberate**, better than silently skipping.

- [ ] **Step 6: `DeleteMessage` hook (`handlers.rs`, extend the shipped `match v.get("type")`).** Add:
```rust
Some("event") => {
    if let Some(eid) = v.get("id").and_then(|i| i.as_i64()) {
        if let Some(row) = crate::channel_events::get(conn, eid)? {
            if row.status == "upcoming" {
                crate::channel_events::cancel(conn, eid, crate::db::now() as i64)?;
                let updated = crate::channel_events::get(conn, eid)?
                    .ok_or_else(|| anyhow::anyhow!("event row vanished during cancel"))?;
                let info = crate::channel_events::build_info(conn, &updated)?;
                events.push(BroadcastEvent {
                    target: EventTarget::Subscribers(channel_id),
                    event: ServerEvent::EventUpdated { event: info },
                });
            }
        }
    }
}
```
Rows retained (audit). The cancel-notify pass then DMs the Going list — **deleting the card is a cancellation, and attendees are told.** No announcement can ever post (the `status='upcoming'` guard in `start_and_announce` fails).

- [ ] **Step 7: `ListActiveWidgets` — third query + 3-WAY merge (`handlers.rs:2592-2637`).** Add
`let event_rows = crate::channel_events::list_upcoming_in_channel(conn, channel_id as i64, now, ACTIVE_WIDGETS_CAP as u32)?;`
and **rewrite the 2-way loop as a 3-way merge** over `created_at` (each list is already `id ASC` = creation order). Deterministic tie-break order: **poll, then giveaway, then event**. Keep `ACTIVE_WIDGETS_CAP = 20` **combined**, then `ok(ServerResponse::ActiveWidgets { polls, giveaways, events })`. No per-viewer fields, no rate limit (read, `GetPoll` class), no `.await`. **Also update the test helper `active_widgets(r)` (`handlers.rs:6845`) to destructure three fields and return a 3-tuple**, and fix its existing call sites.

- [ ] **Step 8: Sweeper — the three event passes (`widgets.rs::sweep_once`).** Resolve the system identity **once per tick**, at the top of the event section:
```rust
let system_pk = match crate::bots::get_or_create_system_identity(conn) {
    Ok(pk) => Some(pk),
    Err(e) => { tracing::warn!("widget sweeper: system identity unavailable: {e}"); None }
};
```
(Lazy: minted the first time any server actually starts an event or fires a reminder.)
  - **Reminder pass (§5.2):** `for row in channel_events::list_reminder_due(conn, now)?` → `mark_reminded(conn, row.id, now)?` false ⇒ skip → `responders(conn, row.id, &["going", "maybe"])?` → one `PendingDm` each:
    `format!("⏰ \"{}\" starts soon.", title)` + `\n📍 {location}` if present + `\nfarder://widget/event/{channel_id}/{id}`.
    **Recipients: Going + Maybe.** A "Maybe" is undecided and the nudge is what converts it; a "Going" wants the logistics reminder. "Can't make it" gets nothing — DMing them is spam.
  - **Start pass (§5.3):** `for row in channel_events::list_start_due(conn, now)?` → `start_and_announce(...)` → `Ok(None)` ⇒ skip (a Cancel won) → `Ok(Some((info, msg)))` ⇒ push `EventUpdated { event: info }` **and** `NewMessage { message: msg }`, both `Subscribers(channel_id)`, plus one `PendingDm` per **Going** responder: `format!("📅 \"{}\" is starting now.", title)` + optional location + the widget link.
  - **Cancel-notify pass (§5.4):** `for row in channel_events::list_cancel_unnotified(conn)?` → `mark_cancel_notified` false ⇒ skip → one `PendingDm` per **Going** responder: `format!("❌ \"{}\" was cancelled.", title)`. **No channel message** — the card flip is the public record (the `CancelGiveaway` precedent).
  Each pass wraps its top-level call in `match … { Err(e) => tracing::warn!(...), Ok(rows) => … }` — one bad row never panics the sweeper. `start_and_announce` needs `&mut Connection`; `sweep_once` takes `&Connection`, so either (a) change `sweep_once` to take `&mut rusqlite::Connection` and let the sweeper pass `&mut *conn` from its `MutexGuard`, **or** (b) have `start_and_announce` use explicit `conn.execute("BEGIN")` / `COMMIT` / `ROLLBACK` on `&Connection`. **Pick (a)** — it is the smaller, type-checked change; update both shipped `sweep_once` tests to `let mut conn = …`.

- [ ] **Step 9: Tests.**
  `channel_events.rs mod tests` (pure parse): `parse_positional_title_when_location_description`; `parse_remind_final_segment_is_deterministic` (`… | remind 1h` and `… | remind none`, case-insensitive); `parse_relative_forms` (`3d`, `in 3d`, `2h`); `parse_absolute_at_unix`; `parse_rejects_wall_clock_with_builder_hint` (assert the exact hint string); `parse_empty_third_segment_means_no_location`; `parse_rejects_over_length_title_location_description`; `resolve_start_bounds` (`now+30` rejected, `now+400d` rejected, `now+60` **accepted**).
  `channel_events.rs` (in-memory conn): `create_event_card_links_message_event_and_widget` (widget JSON `{"type":"event","id":N}`, message author == invoker, `author_badge` NULL, `author_name_override` NULL — the `create_poll_card_links_message_poll_and_widget` template); `rsvp_upsert_moves_member_between_options_without_changing_total`; `clear_rsvp_returns_false_when_none`; `build_info_counts_are_full_totals_while_names_cap_at_ten` (13 going ⇒ `going_count == 13 && going_names.len() == 10`); `responders_going_and_maybe_excludes_no`; `edit_changed_start_nulls_reminded_at_unchanged_does_not`; `list_upcoming_in_channel_excludes_started_cancelled_past_and_other_channels_id_asc_limit`.
  `handlers.rs mod tests` (fixtures `setup()` / `add_member` / `make_channel` / `fake_state`): `test_rsvp_event_happy_emits_event_updated_to_subscribers`; `test_rsvp_event_bogus_response_errors`; `test_rsvp_event_after_starts_at_unswept_errors`; `test_rsvp_event_on_cancelled_errors`; `test_rsvp_event_without_view_channel_is_opaque_not_found` (**assert the reason is byte-identical to the nonexistent-id reason**); `test_event_timeout_gating` (denied on RSVP/clear/cancel/edit, **allowed** on `GetEvent`); `test_clear_rsvp_with_no_rsvp_is_ok_with_no_event`; `test_cancel_event_authz_matrix` (rando → MANAGE_SERVER error, creator → cancelled, MANAGE_SERVER holder → cancelled, twice → err); `test_edit_event_validation_and_reminder_rearm`; `test_get_event_my_rsvp_is_per_requester`; `test_delete_message_on_event_card_cancels_and_emits_both`; `test_list_active_widgets_includes_upcoming_events_excludes_started_cancelled_and_caps_at_20_combined`; `test_add_command_accepts_event_kind_and_takes_arg`; `test_run_command_event_kind_creates_card_with_invoker_authorship`.
  `widgets.rs mod tests`: `sweep_once_event_lead_dms_going_and_maybe_only_then_never_again` (marks `reminded_at`; second sweep → zero); `sweep_once_event_start_and_lead_same_tick_sends_start_batch_only`; `sweep_once_event_start_flips_announces_once_and_dms_going_only` (assert `status='started'`, **exactly one** announcement with `author_badge == Some("BOT")`, `author_name_override == Some("Events")`, `reply_to == Some(card_id)`, then a second sweep leaves `SELECT COUNT(*) FROM messages` **unchanged**); `sweep_once_cancelled_event_dms_going_once_then_none`; `sweep_once_start_pass_skips_event_cancelled_first` (no announcement).

- [ ] **Step 10: Gates + commit.**
```bash
cd /home/deez/farder-events
cargo test -p farder-server 2>&1 | tail -30
cargo build --workspace 2>&1 | tail -20
cd /home/deez/farder-events/client/src-tauri && cargo build 2>&1 | tail -20   # protocol changed (ActiveWidgets gained a field — fix the destructure in commands.rs:4859 to `{ polls, giveaways, events: _ }` so this task builds; T3 wires it properly)
cd /home/deez/farder-events
git add crates/farder-protocol/src/server.rs crates/farder-server/src/db.rs crates/farder-server/src/channel_events.rs crates/farder-server/src/lib.rs crates/farder-server/src/connection.rs crates/farder-server/src/handlers.rs crates/farder-server/src/widgets.rs client/src-tauri/src/commands.rs
git commit -m "feat(events): channel_events tables + module + protocol + dispatch + handlers + sweeper passes"
```

---

### Task 3: CLIENT PLUMBING — Tauri commands, bridge, types, events, reducer

**Files:** `client/src-tauri/src/commands.rs`; `client/src-tauri/src/main.rs`; `client/src-tauri/src/bridge.rs`; `client/src/lib/tauri-bridge.ts`; `client/src/lib/types.ts`; `client/src/hooks/useServerEvents.ts`; `client/src/context/ServerContext.tsx`.

- [ ] **Step 1: Seven new Tauri commands + one widened.** In `client/src-tauri/src/commands.rs`, standard 3-arm mapping (`ServerResponse::X => Ok(..)`, `Error { reason } => Err(reason)`, `other => Err(format!("unexpected response: {:?}", other))`). **`EventInfo` passes through raw** — it has no `Option<PublicKey>`, so no `giveaway_json`-style mapping is needed.
```rust
#[derive(serde::Serialize)] pub struct EventState { pub event: EventInfo, pub my_rsvp: Option<String> }

get_event(server_id: String, event_id: i64) -> Result<EventState, String>            // <- ServerResponse::Event
rsvp_event(server_id: String, event_id: i64, response: String) -> Result<(), String>
clear_rsvp(server_id: String, event_id: i64) -> Result<(), String>
cancel_event(server_id: String, event_id: i64) -> Result<(), String>
edit_event(server_id: String, event_id: i64, title: String, description: Option<String>,
           location: Option<String>, starts_at: u64, remind_lead: Option<u64>) -> Result<(), String>
list_my_reminders(server_id: String) -> Result<Vec<ReminderInfo>, String>            // <- ServerResponse::MyReminders
cancel_reminder(server_id: String, reminder_id: i64) -> Result<(), String>
```
  Widen the existing `run_command` (`commands.rs:2695`) to `Result<Option<String>, String>`:
```rust
ServerResponse::Ok => Ok(None),
ServerResponse::Notice { text } => Ok(Some(text)),
ServerResponse::Error { reason } => Err(reason),
other => Err(format!("unexpected response: {:?}", other)),
```
  Widen `struct ActiveWidgets` (`commands.rs:4841`) with `pub events: Vec<EventInfo>` and destructure/populate it in `list_active_widgets` (replacing T2's temporary `events: _`).
  **Register all seven in `generate_handler!` in `client/src-tauri/src/main.rs`.** Audit:
```bash
cd /home/deez/farder-events/client/src-tauri
for f in get_event rsvp_event clear_rsvp cancel_event edit_event list_my_reminders cancel_reminder; do
  printf "%s: " "$f"; grep -c "\b$f\b" src/main.rs; done   # each must be >= 1
```

- [ ] **Step 2: Event bridge (`client/src-tauri/src/bridge.rs`).** Beside the `PollUpdated`/`GiveawayUpdated` arms (`:212-215`), **before** the `_ => Ok(())` catch-all:
```rust
ServerEvent::EventUpdated { event } =>
    app.emit("server:event_updated", serde_json::json!({ "server_id": sid, "event": event })),
```
(No pubkey mapping — `EventInfo.creator` serializes as `{ bytes }`, the `PollInfo.creator` shape.)

- [ ] **Step 3: TS types (`client/src/lib/types.ts`).**
```ts
export interface EventInfo {
  id: number; channel_id: number; message_id: number;
  /** Serde-serialized PublicKey (same shape as MessageInfo.author); use publicKeyToString(). */
  creator: { bytes: number[] };
  title: string; description: string | null; location: string | null;
  starts_at: number;               // absolute unix secs — render with toLocaleString()
  remind_lead: number | null;
  status: "upcoming" | "started" | "cancelled";
  going_count: number; maybe_count: number; no_count: number;
  going_names: string[]; maybe_names: string[]; no_names: string[];   // capped at 10 each
}
export interface EventState { event: EventInfo; my_rsvp: string | null }
export interface ReminderInfo { id: number; text: string; due_at: number; created_at: number; channel_id: number }
```
Update `CommandInfo.kind`'s doc comment to list `event` and `reminder`.

- [ ] **Step 4: Bridge fns (`client/src/lib/tauri-bridge.ts`).**
```ts
getEvent(serverId: string, eventId: number): Promise<EventState>
rsvpEvent(serverId: string, eventId: number, response: string): Promise<void>
clearRsvp(serverId: string, eventId: number): Promise<void>
cancelEvent(serverId: string, eventId: number): Promise<void>
editEvent(serverId, eventId, title, description, location, startsAt, remindLead): Promise<void>
listMyReminders(serverId: string): Promise<ReminderInfo[]>
cancelReminder(serverId: string, reminderId: number): Promise<void>
```
camelCase invoke args (`{ serverId, eventId, … }` — Tauri maps to snake_case params), snake_case response fields. Widen `runCommand(...)` (`:977`) to `Promise<string | null>` and `listActiveWidgets(...)` (`:1046`) to `Promise<{ polls: PollInfo[]; giveaways: GiveawayInfo[]; events: EventInfo[] }>`. **The three existing `await api.runCommand(...)` call sites compile unchanged** (they discard the result).

- [ ] **Step 5: Reducer + listener (`client/src/context/ServerContext.tsx`, `client/src/hooks/useServerEvents.ts`).**
  `PerServerState` (beside `polls`/`giveaways` at `:28-36`):
```ts
events: Record<number, { event: EventInfo; myRsvp: string | null }>;   // init {}
activeWidgets: { channelId: number; polls: number[]; giveaways: number[]; events: number[] } | null;
```
  Actions (SCREAMING_SNAKE + `serverId` + `payload`, immutable upsert):
  - `EVENT_UPDATED { serverId, payload: EventInfo }` — upsert **preserving** the existing `myRsvp` (broadcasts never carry it), default `null`. **Plus active-bar maintenance mirroring `POLL_UPDATED` (`:412-431`):** when `event.channel_id === activeWidgets.channelId` — append the id when `status === "upcoming"` and it is missing and the combined length is `< ACTIVE_WIDGETS_CAP`; remove it when `status` is `"started"` or `"cancelled"`.
  - `EVENT_STATE { serverId, payload: { event: EventInfo; myRsvp: string | null } }` — from `getEvent`.
  - `EVENT_MY_RSVP { serverId, payload: { eventId: number; myRsvp: string | null } }` — dispatched by the widget after its own successful ack.
  - `ACTIVE_WIDGETS` payload gains `events: EventInfo[]` — ids into `activeWidgets.events`, infos upserted into the `events` slice (same treatment as polls/giveaways at `:474-489`). **The combined 20-cap applies across all three lists.**
  `useServerEvents.ts`, beside the two widget listeners (`:454-466`):
```ts
listen("server:event_updated", (e) => {
  const data = e.payload as { server_id: string; event: EventInfo };
  if (data.server_id !== activeRef.current) return;   // background servers dropped, like the other widget events
  dispatch({ type: "EVENT_UPDATED", serverId: data.server_id, payload: data.event });
}).then(safePush);
```

- [ ] **Step 6: Gates + commit.**
```bash
cd /home/deez/farder-events/client/src-tauri && cargo build 2>&1 | tail -20
cd /home/deez/farder-events/client && npx tsc --noEmit
cd /home/deez/farder-events
git add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs client/src-tauri/src/bridge.rs client/src/lib/tauri-bridge.ts client/src/lib/types.ts client/src/hooks/useServerEvents.ts client/src/context/ServerContext.tsx
git commit -m "feat(events): client plumbing — 7 Tauri commands, Notice-aware run_command, event slice"
```

---

### Task 4: EVENT UI — EventWidget, EventBuilderModal, links, chips, theme CSS

**Files:** `client/src/components/EventWidget.tsx` (**new**); `client/src/components/EventBuilderModal.tsx` (**new**); `client/src/components/Message.tsx`; `client/src/components/LinkedWidgetCard.tsx`; `client/src/components/ActiveWidgetsBar.tsx`; the three `client/src/themes/*/theme.css`.

- [ ] **Step 1: `EventWidget.tsx`.** Props **identical in contract** to `PollWidget`/`GiveawayWidget` so `LinkedWidgetCard` and `ActiveWidgetsBar` can host it:
```ts
interface EventWidgetProps {
  serverId: string;
  eventId: number;
  onUnavailable?: () => void;
  refetch?: "mount" | "interval";
}
```
  Copy `PollWidget`'s skeleton verbatim: module-level `cachedOwnPk`, `fetchedRef` mount guard, `refetch === "interval"` ⇒ 20 s `setInterval` cleared on unmount + a refetch after each own successful ack, `api.getEvent` → `dispatch({ type: "EVENT_STATE", … })`, fetch error → `onUnavailable?.()`.
  Render:
  - `.event-widget` → `.event-title` (📅 + title) → `.event-when` = `new Date(starts_at * 1000).toLocaleString()` plus a `.event-when-rel` relative hint ("in 3 days"), recomputed on a 30 s interval like the poll footer → optional `.event-location` (📍) and `.event-description`.
  - `.event-rsvp-row` with three `<button class="event-rsvp-btn">`: **Going / Maybe / Can't make it**; the current one also carries `.event-rsvp-btn--mine`. Clicking a **different** one calls `api.rsvpEvent(serverId, eventId, "going"|"maybe"|"no")` then dispatches `EVENT_MY_RSVP`; clicking **your own** calls `api.clearRsvp(...)` then `EVENT_MY_RSVP { myRsvp: null }` (the poll-retract idiom).
  - `.event-attendees` with three `.event-attendee-group`s: a header ("Going · 4"), then up to ten `.event-attendee-name`s, then `.event-more` reading `and {count - names.length} more` when positive.
  - **Started:** header `.event-happening` "Happening now"; after the start it reads "Started {local time}"; RSVP buttons `disabled`.
  - **Cancelled:** muted `.event-cancelled` "Event cancelled" + strikethrough title; buttons inert.
  - **Creator/mod controls:** `Cancel` and `Edit` (existing `.xp-button`) when `ownPk === publicKeyToString(event.creator) || hasPermission(getActorPermissions(...), PERMISSIONS.MANAGE_SERVER)` (the `PollWidget` Close-button expression) — **the server re-checks regardless**. Edit opens `EventBuilderModal` in edit mode, prefilled, submitting via `api.editEvent`.
  - `.widget-copy-link` 🔗 copies `farder://widget/event/${event.channel_id}/${event.id}` (the shipped clipboard + `toast.success` idiom).
  - Errors surface inline in `.error-text`; the next `EventUpdated`/refetch self-corrects.

- [ ] **Step 2: `EventBuilderModal.tsx` (create **and** edit mode).** Modeled on `PollBuilderModal` (same `.modal-overlay` / `.modal-dialog` / `.modal-titlebar` / `.modal-body` / `.connect-section` / `.connect-label` / `.connect-input` / `.connect-actions` / `.error-text` scaffolding — **zero new CSS**).
```ts
interface Props {
  serverId: string; channelId: number;
  /** Create mode: the event command's trigger (without "/"). */
  trigger?: string;
  /** Edit mode: the event being edited (mutually exclusive with `trigger`). */
  editing?: EventInfo;
  onClose: () => void;
  onCreated: () => void;
}
```
  Fields: **Title** `<input class="connect-input">` required ≤120 `stripPipes`; **Date** `<input type="date" class="connect-input">` required; **Time** `<input type="time" class="connect-input">` required; **Location** `<input class="connect-input">` optional ≤120 `stripPipes`; **Description** `<textarea class="connect-input">` optional ≤500 `stripPipes`; **Reminder** `<select class="connect-input">` — None (default) / 15 minutes (`15m`) / 1 hour (`1h`) / 1 day (`1d`).
  **Timezone handling (explicit):** `const startsAt = Math.floor(new Date(`${date}T${time}`).getTime() / 1000)` — a date-time string **without** a `Z`/offset is parsed by JS as **local** time, which is precisely the intent ("8pm my time"). That absolute second is what travels and what is stored. **Nothing timezone-shaped is stored or transmitted**; every viewer renders it with `toLocaleString`. Client checks (inline `.error-text`, submit blocked, server re-validates from scratch): `Number.isFinite(startsAt)`; `startsAt > now + 60` → "Pick a time in the future"; `startsAt <= now + 365 * 86400` → "Events can be at most a year out".
  **Create mode** assembles the exact server syntax and calls `api.runCommand(serverId, trigger, channelId, args)`:
```ts
const args = [title, `@${startsAt}`,
  ...(location || description ? [location] : []),
  ...(description ? [description] : []),
  ...(lead ? [`remind ${lead}`] : []),
].join(" | ");
```
  (The location segment is emitted — possibly empty — whenever a description exists, because the grammar is positional.)
  **Edit mode** calls `api.editEvent(serverId, editing.id, title, description || null, location || null, startsAt, leadSecs)` where `leadSecs` ∈ `{null, 900, 3600, 86400}`; prefill from `editing` (`new Date(editing.starts_at * 1000)` split into local date/time strings). On success `onCreated()`; on rejection stay open with the error.

- [ ] **Step 3: `Message.tsx` — widget slot, event links, and the NEW channel link.**
  1. Widget slot (`:372-392`): add `case "event": return <EventWidget serverId={serverId} eventId={parsedWidget.id} onUnavailable={() => setWidgetUnavailable(true)} />;`.
  2. Widen **all four** widget-link places to the three-kind union: `WIDGET_LINK_REGEX` (`:40`) → `/farder:\/\/widget\/(poll|giveaway|event)\/(\d+)\/(\d+)/gi`; `isWidgetLink` (`:118`) → `/^farder:\/\/widget\/(poll|giveaway|event)\/\d+\/\d+$/i`; `parseWidgetLink` (`:125`) regex + its `kind` cast → `"poll" | "giveaway" | "event"`; the pill label in `renderContent` (`:191`) → a small `widgetPillLabel(seg)` helper returning `"📊 Poll link"` / `"🎉 Giveaway link"` / `"📅 Event link"`. (`isWidgetSchemeLink`'s `farder://widget/` prefix test already covers `event` — no change.)
  3. **New channel link** for the reminder DM's link-back:
```ts
const CHANNEL_LINK_REGEX = /farder:\/\/channel\/(\d+)/gi;
function isChannelSchemeLink(s: string): boolean { return /^farder:\/\/channel\//i.test(s); }
function isChannelLink(s: string): boolean { return /^farder:\/\/channel\/\d+$/i.test(s); }
```
   In `renderContent`, **before** the `isInviteLink` branch, render matching segments as a `.widget-link-pill` labelled `Go to #{name}` (name from the client's channel list; unknown → `"Open channel"`), whose `onClick` selects the channel via the existing channel-select action.
  4. **CRITICAL — both exclusion guards.** `INVITE_REGEX`'s `farder:\/\/[^\s]+` alternative matches `farder://channel/...`, so without the guard a reminder DM renders a bogus join card (this is the exact bug the widget-link work already had to fix; the same fix extends):
     - `isInviteLink` (`:134`): `if (isWidgetSchemeLink(s) || isChannelSchemeLink(s)) return false;`
     - the invite-embeds IIFE (`:593`): `if (isWidgetSchemeLink(m) || isChannelSchemeLink(m)) continue;`

- [ ] **Step 4: `LinkedWidgetCard.tsx`.** `WidgetLink.kind` → `"poll" | "giveaway" | "event"`; the `info` lookup gains `server?.events[link.widgetId]?.event ?? null`; the unavailable label gains `"Event not available"`; the render gains an `EventWidget` branch with the **unchanged** `refetch={sameChannel ? "mount" : "interval"}` discipline and `onUnavailable={() => setUnavailable(true)}`. The channel-id consistency check is unchanged in shape.

- [ ] **Step 5: `ActiveWidgetsBar.tsx`.** `Chip.kind` → `"poll" | "giveaway" | "event"`; `open` state kind likewise; build event chips from `activeWidgets.events` looking each info up in `server.events[id]?.event` — `label = title`, `endsAt = starts_at`, `order = message_id` (message ids are one monotonically increasing sequence, so `message_id` ASC IS creation order across all three kinds); the chip prefix is 📅 and `.widget-chip-time` shows `formatChipTime(starts_at, nowSecs)` ("in 3h"). The dropdown hosts `<EventWidget serverId={serverId} eventId={id} refetch="mount" />`. Both `ACTIVE_WIDGETS` dispatches in the fetch effect gain `events: res.events` / `events: []`.

- [ ] **Step 6: Theme CSS (CLAUDE.md rule — ALL THREE files).** 15 new classes, modeled on the shipped `.poll-*` / `.link-embed` families, **colors only via `var(--xp-…)`** (`--xp-panel-bg`/`--xp-border` card, `--xp-blue` for `.event-rsvp-btn--mine`, `--xp-text-muted` for counts/names, `--xp-text-normal` for the title):
`.event-widget`, `.event-title`, `.event-when`, `.event-when-rel`, `.event-location`, `.event-description`, `.event-rsvp-row`, `.event-rsvp-btn`, `.event-rsvp-btn--mine`, `.event-attendees`, `.event-attendee-group`, `.event-attendee-name`, `.event-more`, `.event-happening`, `.event-cancelled`.
**Verified: none of these names currently exist in any theme file** (no collisions). Everything else reuses shipped classes (`.widget-link-pill`, `.widget-copy-link`, `.widget-chip-time`, `.linked-widget-unavailable`, `.error-text`, `.xp-button`, the `.modal-*`/`.connect-*` family).

- [ ] **Step 7: Gates + commit.**
```bash
cd /home/deez/farder-events/client && npx tsc --noEmit
grep -l "event-rsvp-btn" /home/deez/farder-events/client/src/themes/*/theme.css   # must list all 3
grep -l "event-widget"   /home/deez/farder-events/client/src/themes/*/theme.css   # must list all 3
cd /home/deez/farder-events
git add client/src/components/EventWidget.tsx client/src/components/EventBuilderModal.tsx client/src/components/Message.tsx client/src/components/LinkedWidgetCard.tsx client/src/components/ActiveWidgetsBar.tsx client/src/themes/
git commit -m "feat(events): EventWidget + EventBuilderModal + widget/channel links + chips + theme CSS"
```

---

### Task 5: REMINDER UI + CONFIG + DOCS

**Files:** `client/src/components/ReminderBuilderModal.tsx` (**new**); `client/src/components/settings/MyReminders.tsx` (**new**); `client/src/components/MessageInput.tsx`; `client/src/components/settings/SettingsModal.tsx`; `client/src/components/BotsTab.tsx`; `docs/modules/server-widgets.md`; `docs/modules/tauri-commands.md`; `docs/modules/tauri-bridge.md`; `docs/modules/frontend-state.md`; `docs/modules/protocol.md`; `docs/modules/server-handlers.md`; `ARCHITECTURE.md`; `docs/superpowers/specs/2026-07-27-mesh-rung2-e2ee-design.md`.

- [ ] **Step 1: `ReminderBuilderModal.tsx`.** Same modal scaffolding as `PollBuilderModal` (**zero new CSS**).
```ts
interface Props { serverId: string; channelId: number; trigger: string;
                  onClose: () => void; onCreated: (notice: string | null) => void }
```
  Fields: **Remind me in** — a duration `<select class="connect-input">` (15 minutes `15m` / 30 minutes `30m` / 1 hour `1h` / 3 hours `3h` / 1 day `1d` / 3 days `3d` / 7 days `7d` / **Custom…**) plus the shared custom row copied from `PollBuilderModal` (`<input type="number" min={1} max={9999}>` + a minutes/hours/days `<select>`), resolved through a local copy of `resolveDurationToken` with `MIN_DURATION_SECS = 60` / `MAX_DURATION_SECS = 30 * 86_400`; `null` → `.error-text` "Duration must be between 1 minute and 30 days". **Reminder text** — `<textarea class="connect-input">` required, ≤500, live `{n}/500` counter.
  Submit: `const notice = await api.runCommand(serverId, trigger, channelId, `${token} ${text}`)` → `onCreated(notice)`. **No pipe-stripping** (the reminder grammar has no delimiter past the first space).

- [ ] **Step 2: `MessageInput.tsx` builder wiring.** Widen the `builder` state kind to `"poll" | "giveaway" | "event" | "reminder"` and the two kind checks (`:228` in `handleSend`, `:322` in `insertCommand`) to include `"event"` and `"reminder"`. Render `<EventBuilderModal … trigger={builder.trigger} />` for `builder?.kind === "event"` and `<ReminderBuilderModal … />` for `"reminder"`, both beside the existing two (`:572`/`:581`). `handleBuilderCreated` gains an optional `notice` parameter: `if (notice) toast.success(notice);` then the existing close+clear (the shipped `toast` import already exists in this file family — add it if absent). Wire `onCreated={handleBuilderCreated}` for all four (poll/giveaway pass nothing → `notice` undefined → no toast).

- [ ] **Step 3: `MyReminders.tsx` (`client/src/components/settings/`).** Mirror `AlertSubscriptions.tsx` **exactly** — `useActiveServerId()`, a `useEffect` calling `api.listMyReminders(serverId)` into local state, `<div className="settings-panel"><h2 className="settings-panel-title">Reminders</h2>`, an `.error-text` line, `<SettingsSection label="Upcoming reminders">`, one row per reminder:
```tsx
<div key={r.id} className="organizer-row">
  <span className="organizer-name">{r.text} · {new Date(r.due_at * 1000).toLocaleString()}</span>
  <div className="organizer-actions">
    <button className="organizer-btn organizer-delete" title="Cancel reminder"
            onClick={() => void handleCancel(r.id)}>Cancel</button>
  </div>
</div>
```
  `handleCancel` → `api.cancelReminder(serverId, id)` → drop from local state; failure → `.error-text`. Muted `var(--xp-text-muted)` empty state ("You have no upcoming reminders.") and disconnected state ("Connect to a server to manage reminders."). **Reuses the shipped `.organizer-*` / `.settings-panel*` classes → ZERO new CSS.**
  `SettingsModal.tsx`: `SectionId` (`:13`) gains `| "reminders"`; `SECTIONS` (`:15`) gains `{ id: "reminders", label: "Reminders" }`; the render switch (`:66`) gains `{active === "reminders" && <MyReminders />}`.

- [ ] **Step 4: `BotsTab.tsx` kind selector.** `cmdKind` union (`:45`) → `"text" | "api" | "poll" | "giveaway" | "event" | "reminder"`; `isWidgetKind` (`:136`) → `cmdKind === "poll" || cmdKind === "giveaway" || cmdKind === "event" || cmdKind === "reminder"` (no extra fields for any of them); two `<option>`s after `giveaway` (`:537`): `<option value="event">Event</option>`, `<option value="reminder">Reminder</option>`; the Add-button enable expression (`:619`) treats them like poll/giveaway (name + trigger + description only). Muted hint lines beside the existing poll/giveaway ones:
  - Event: `Members run /<trigger> Title | 3d [| location] [| description] [| remind 1h]` — or just pick it from "/" to open the form.
  - Reminder: `Members run /<trigger> 90m take the pizza out` — private, nothing is posted.

- [ ] **Step 5: Docs (same commit — docs are treated like tests).**
  - **`docs/modules/server-widgets.md`** — update the `widgets.rs` section for the new `sweep_once(conn: &mut Connection, now: u64) -> SweepOutcome` signature, `SweepOutcome`/`PendingDm`, the DM loop in `spawn_widget_sweeper`, and the three event passes + the reminder pass (state persisted under a guarded UPDATE **before** the DM ⇒ **at-most-once**, never duplicated). Add full sections for **`channel_events.rs`** and **`reminders.rs`** using `docs/modules/_TEMPLATE.md` (every `pub fn` from T1 Step 5 and T2 Step 3, with its one-line contract).
  - **New file `docs/modules/server-system-identity.md`** (or a clearly-titled section appended to `server-widgets.md` — pick one and link it from `ARCHITECTURE.md`): `bots::get_or_create_system_identity` / `send_bot_dm_as` / `send_system_dm`, the `bots.kind='system'` row + unique partial index, and the **four** exclusion points (`GetMembers` via `members::list_members_visible` **before** the mesh `is_bot ||` whitelist, `bots::list_bots`, `RemoveBot`, and "no auth path reads `bots.secret_key`").
  - **`docs/modules/tauri-commands.md`** — the 7 new commands (name, params, return, side effects, matching `invoke("…")`), plus the **changed** `run_command` return (`Result<Option<String>, String>`; `Notice` → `Some(text)`) and the `ActiveWidgets.events` field.
  - **`docs/modules/tauri-bridge.md`** — `server:event_updated` (payload `{ server_id, event }`) and the `useServerEvents.ts` listener that consumes it.
  - **`docs/modules/frontend-state.md`** (**not** `frontend-context.md` — that file does not exist) — the `events` slice, `activeWidgets.events`, and the three new actions `EVENT_UPDATED` / `EVENT_STATE` / `EVENT_MY_RSVP`.
  - **`docs/modules/protocol.md`** — `EventInfo`, `ReminderInfo`, the 7 new requests, `Event`/`Notice`/`MyReminders` responses, the `ActiveWidgets` third field (`#[serde(default)]`), `EventUpdated`.
  - **`docs/modules/server-handlers.md`** — the 7 new arms with their exact gate order, the opaque `"event not found"` / `"reminder not found"` rule, and the note that **none** were added to `request_requires_membership`'s allow-list.
  - **`ARCHITECTURE.md`** — add `channel_events.rs` / `reminders.rs` to the `crates/farder-server/` module line (`:46`), and extend the widget data-path paragraph (`:180`) with the event/reminder flow + the system identity.
  - **`docs/superpowers/specs/2026-07-27-mesh-rung2-e2ee-design.md`** — add the two feature-matrix rows (**events → server-features-channel-only**, **personal reminders → server-features-channel-only**). **This is the ONLY §9 deliverable: the rung-2 gate is not implemented in `farder-server` today (verified — no `e2ee` column, no "encrypted channels" refusal anywhere in the crate), so there is no code to write here. Do not invent one.**

- [ ] **Step 6: Gates + commit.**
```bash
cd /home/deez/farder-events/client && npx tsc --noEmit
cd /home/deez/farder-events && cargo test -p farder-server 2>&1 | tail -20
git add client/src/components/ReminderBuilderModal.tsx client/src/components/settings/MyReminders.tsx client/src/components/MessageInput.tsx client/src/components/settings/SettingsModal.tsx client/src/components/BotsTab.tsx docs/ ARCHITECTURE.md
git commit -m "feat(reminders): ReminderBuilderModal + MyReminders settings + BotsTab kinds + module docs"
```

---

## Security checklist (verify before calling the feature done)

- [ ] All authorization is server-side against the **authenticated connection key**; no request carries an RSVP-er, creator, or reminder-owner field — ids and content only.
- [ ] All 7 new requests are membership-gated by **default-deny** `request_requires_membership` (**nothing added to its 4-entry allow-list**) — mesh log-membership gating is automatic. One test per request asserts a non-member is refused.
- [ ] Every channel-scoped action funnels through `handlers::widget_channel_visible` and returns the **byte-identical** `"event not found"` for missing / channel-gone / non-DM-participant / no-`VIEW_CHANNEL`. `CancelReminder` returns `"reminder not found"` for a foreign id. `ListActiveWidgets` keeps `"channel not found"`.
- [ ] Creation gates are the RunCommand gates, unchanged: `content_block_reason` → `command_limiter` (5/10 s) → `check_run_command_channel_auth`. Events add **no** MANAGE_SERVER gate (product decision); reminders add a **20-per-user** cap.
- [ ] `require_not_timed_out` on every mutating interaction (RSVP / clear / cancel / edit); reads (`GetEvent`, `ListMyReminders`) exempt, matching `GetPoll`. `CancelReminder` is deliberately exempt (managing your own private state is not channel content).
- [ ] `state.widget_limiter` (10/10 s) on RSVP / clear / cancel-reminder; creation bounded by `command_limiter`.
- [ ] **No DB `MutexGuard` across any `.await`** — dispatch arms, handlers (all sync), and the sweeper (DMs returned as **data** precisely so `send_system_dm` re-acquires the mutex only after the sweeper's guard is gone).
- [ ] **Persist-before-notify with single-shot guards:** `reminded_at IS NULL`, `status='upcoming'` (start), `cancel_notified_at IS NULL`, `status='pending'` (reminder). A crash can never double-fire or double-announce; the accepted cost is **at-most-once**.
- [ ] Length/enum bounds enforced **server-side** (client mirrors for UX only): title ≤120, location ≤120, description ≤500, reminder text ≤500, `response ∈ {going,maybe,no}`, `remind_lead ∈ {900,3600,86400}`, `starts_at ∈ [now+60, now+365d]`, duration ∈ [1 m, 30 d], ≤20 pending reminders per user.
- [ ] **Attendee privacy is a deliberate divergence, not an oversight** (spec §8): the roster IS the feature; RSVPing is an affirmative public act; **display names only, never public keys**, capped at 10 per option; the visibility boundary is the channel; `EventUpdated` targets `Subscribers(channel_id)` only.
- [ ] `reminders.text` is readable **only by its owner** (`ListMyReminders` is key-scoped in SQL, `CancelReminder` is opaque), never broadcast, and produces no channel artifact.
- [ ] The `messages.widget` JSON is **server-written only** and parsed defensively client-side (try/catch, numeric id).
- [ ] The system identity holds no roles, is filtered out of `GetMembers` **before** the mesh `is_bot` whitelist, is excluded from `list_bots`, cannot be removed via `RemoveBot`, and cannot authenticate a connection (no auth path reads `bots.secret_key`). Its secret is the **same trust class as the existing ticker-bot secrets** — stated openly, not implied.

## Owner runtime verification (server changed → sidecar rebuild; two clients ideal)

Run the spec's "Owner runtime verification" section in full (steps 1–13). Highlights: the builder posts a 📅 card **as you** (no BOT badge) in your local time; a second account's Going/Maybe/clear moves their **name** live on both screens; a DM from **"Farder"** at the lead time (Going + Maybe) with a clickable widget link; the card flips to "Happening now" with a threaded `📅 … is starting now!` reply and the chip drops out of the bar; cancel and card-deletion both DM the Going list with no announcement; `/remind 2m …` posts **nothing** (verify on the second client) and DMs with a "Go to #general" pill; Settings → Reminders lists and cancels; **"Farder" appears in neither the member sidebar nor the Bots tab and cannot be removed**; pasting an event link renders an interactive card while an invite link in the same message still renders a join card (no regression); every theme styles the card.

## Self-review notes

- Task boundaries match the spec's 5-task decomposition exactly; T1 ships the enabling identity **and** the whole reminder feature (server), so T2 can rely on `send_system_dm` and `SweepOutcome` existing.
- `sweep_once`'s signature changes **twice** (T1 adds `SweepOutcome`, T2 makes the conn `&mut`). Both shipped tests are updated in the task that breaks them; T2 must not leave T1's tests red.
- The `ActiveWidgets` variant gains a field in T2, which breaks the client crate's destructure — T2 patches it minimally (`events: _`) to keep its own gate green, and T3 wires it properly. Both are called out in the task text so no agent is surprised.
- Deliberate divergences from the poll/giveaway precedent, each justified in-spec: attendee **names** are broadcast (§8), event creation has **no MANAGE_SERVER gate** (§4.3), `Notice` is a new response variant rather than abusing `Error` (§6.2), and DMs are returned from the sweeper as data rather than sent inline (§5.1).
- Deliberately NOT built: the giveaway winner DM (now a two-line follow-on: `send_system_dm(state, &winner, …)` after the draw's broadcast step), recurring events, calendar export, timezone picker, +1s, reminder snooze, a "see all attendees" expansion.
- `farder-server` has **no date-formatting dependency** (verified against `crates/farder-server/Cargo.toml`), so the old-client fallback stamp uses SQLite's `strftime` (T2 Step 3). **Do not add a crate for it.**
- Remaining drift risk, flagged rather than guessed: the exact `RateLimiter` call form and the `handlers.rs mod tests` fixture names — build agents match the real code; this plan fixes the contract and the names that must not vary.
