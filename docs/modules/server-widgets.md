# Widget substrate: polls, giveaways & reminders

> **File(s):** `crates/farder-server/src/widgets.rs`, `crates/farder-server/src/polls.rs`, `crates/farder-server/src/giveaways.rs`, `crates/farder-server/src/reminders.rs`
> **Layer:** Server crate
> **Last reviewed:** 2026-07-28

## Purpose

Interactive **message widgets**: a slash command of kind `"poll"` or `"giveaway"` (run via `RunCommand`) posts a normal card message whose `messages.widget` TEXT column carries `{"type":"poll"|"giveaway","id":<i64>}`; the feature's own table (`polls` / `giveaways` + their per-member vote/entry tables) holds the live state; interaction requests mutate it and broadcast a `PollUpdated` / `GiveawayUpdated` event; a single shared **sweeper task** (`widgets.rs`) retires timed widgets. These modules own storage and state transitions only — all authorization, rate limiting, and visibility checks live in `handlers.rs` dispatch; old clients that don't understand `widget` just render the card's plain-text content.

---

## `widgets.rs` — the shared sweeper

### `WIDGET_SWEEP_SECS: u64 = 15`

Fixed tick interval. A due poll closes / a due giveaway draws at most ~15 s late.

### `sweep_once(conn: &Connection, now: u64) -> SweepOutcome`

**What it does:** the sync tick body servicing every half: `polls::close_due` (every due timed poll → `PollUpdated`), then `giveaways::list_due` + `close_and_draw` per row (→ `GiveawayUpdated` + winner-announcement `NewMessage`), then `reminders::list_due` + `mark_sent` per row (→ one `PendingDm`, **zero broadcasts**).
**Returns / emits:** `SweepOutcome { broadcasts, dms }`; each half's errors are `tracing::warn!`ed per-item and never panic the sweeper.
**Side effects:** persists every state change (close / draw / announcement insert / reminder `sent` flip) BEFORE returning — i.e. under the caller's DB lock, before any broadcast or DM, and always behind a guarded `UPDATE` whose rows-affected decides whether the notification is produced at all. Persist-then-notify by construction: a crash in between can never re-close, redraw or re-fire. The accepted cost is **at-most-once** delivery (a crash in the persist→notify window loses that one notification; the durable state is still correct).
**Connects to:** `spawn_widget_sweeper` (the only production caller); tests call it directly without tokio.

### `spawn_widget_sweeper(state: Arc<ServerState>) -> JoinHandle<()>`

**What it does:** spawns the single background loop (started in `main.rs` next to the bot poller): every 15 s it takes a scoped `state.db` lock, runs `sweep_once`, drops the guard, then `broadcast_event`s each `PendingBroadcast` and `bots::send_system_dm`s each `PendingDm` (no DB MutexGuard is ever held across an `.await` — the bots.rs `spawn_bot_poll_task` lock discipline). DMs **must** happen after the guard drops: `send_system_dm` re-acquires the same mutex internally, which is exactly why they are returned as data rather than sent inline. A failed DM is logged and dropped.

### `PendingBroadcast { target: EventTarget, event: ServerEvent }`

A broadcast computed under the DB lock, sent after the guard drops.

### `PendingDm { recipient: PublicKey, text: String }` / `SweepOutcome { broadcasts, dms }`

A DM computed under the DB lock, sent after the guard drops, and the tick's whole output.

---

## `polls.rs` — poll storage & transitions

One vote per member (voting again moves it). `PollInfo` broadcasts carry counts only — voter identities never leave the server; a member's own vote is returned self-only via `ServerResponse::Poll { my_vote }`.

### `parse_poll_args(args: &str) -> Result<ParsedPoll, String>`

Parses the `RunCommand` arg string `Question | option A | option B [| 30m|2h|1d]` (pipe-separated; last segment is an optional duration, 1m–30d). Pure. Errors are the exact user-facing strings (`POLL_USAGE` const, "a poll needs 2-10 options", "question must be 1-256 characters", "options must be 1-100 characters", "duplicate options are not allowed", "duration must be between 1m and 30d").

### `create_poll_card(conn: &mut Connection, channel_id: u64, invoker: &PublicKey, parsed: &ParsedPoll, now: u64) -> Result<(MessageInfo, PollInfo)>`

The whole poll-kind `RunCommand` transaction: inserts the fallback card message ("📊 Poll: …" + option lines), the `polls` row, and `messages::set_widget` with `{"type":"poll","id":…}`, in one transaction. The dispatch arm broadcasts `NewMessage` then `PollUpdated` from its return.

### Row-level fns

- `create(conn, channel_id: i64, message_id: i64, creator, question, options, closes_at: Option<i64>) -> Result<i64>` — bare row insert (used by `create_poll_card`).
- `get(conn, id: i64) -> Result<Option<PollRow>>` — load one row.
- `build_info(conn, row) -> Result<PollInfo>` — assemble the protocol struct (per-option counts, total, closed flag).
- `vote(conn, poll_id, voter, option_index)` — upsert my vote.
- `retract(conn, poll_id, voter) -> Result<bool>` — delete my vote; `false` if I had none.
- `my_vote(conn, poll_id, voter) -> Result<Option<u32>>` — self-only read.
- `close(conn, poll_id, now: i64)` — idempotent (`AND closed_at IS NULL`).
- `close_due(conn, now: i64) -> Result<Vec<PollInfo>>` — sweeper half: close every open timed poll past `closes_at`, return their terminal infos.
- `list_open_in_channel(conn, channel_id: i64, now: i64, limit: u32) -> Result<Vec<PollRow>>` — the channel's open polls, oldest-first (`id ASC`). "Open" is exact: past-`closes_at`-but-unswept polls are excluded (`closes_at > now`), matching the `VotePoll` closed-check; untimed open polls are included. For `ListActiveWidgets`.

---

## `giveaways.rs` — giveaway storage, draw & announcements

Timed only; `status` is `open` → `ended` (drawn) or `cancelled`. Broadcasts carry `entry_count` / `status` / `winner` — never an entrant list. The winner draw is server-side `rand::thread_rng()` over eligible entrants (banned/revoked members filtered at draw time).

### `parse_giveaway_args(args: &str) -> Result<(u64, String), String>`

Parses `<duration> <prize>` (duration first token, 1m–30d; rest is the prize, 1–200 chars). Pure. `GIVEAWAY_USAGE` const is the usage error string. `parse_giveaway_duration(s) -> Option<u64>` is the shared duration token parser.

### `create_giveaway_card(conn: &mut Connection, channel_id: u64, invoker: &PublicKey, prize: &str, duration_secs: u64, now: u64) -> Result<(MessageInfo, GiveawayInfo)>`

The whole giveaway-kind `RunCommand` transaction (creation is **MANAGE_SERVER-gated at dispatch**): inserts the fallback card ("🎉 Giveaway: <prize> — ends <UTC time>"), the `giveaways` row, and the widget JSON. Dispatch broadcasts `NewMessage` then `GiveawayUpdated`.

### `close_and_draw(conn: &Connection, row: &GiveawayRow) -> Result<(GiveawayInfo, MessageInfo)>`

Sweeper half for one due giveaway, in one transaction (`unchecked_transaction`): flip `status='open'` → `ended` (bails and rolls back if the guarded UPDATE hits 0 rows — never double-draws), draw a random eligible winner (or end winnerless), insert the announcement message. Announcement author is a **fresh throwaway keypair** (secret discarded, can never authenticate), display override "Giveaway", badge "BOT", reply-to the card.

### `reroll_and_announce(conn: &Connection, row: &GiveawayRow) -> Result<Option<(GiveawayInfo, MessageInfo)>>`

Redraw for an `ended` giveaway with a winner; `None` when the eligible set is empty (handler maps it to "no eligible entries to reroll"). Same transaction + announcement shape as `close_and_draw`.

### Row-level fns

- `create(conn, channel_id: i64, message_id: i64, creator, prize, ends_at: i64) -> Result<i64>`; `get(conn, id) -> Result<Option<GiveawayRow>>`; `build_info(conn, row) -> Result<GiveawayInfo>` (`winner_name` = roster display name or `None`).
- `enter(conn, giveaway_id, member, now) -> Result<bool>` / `leave(conn, giveaway_id, member) -> Result<bool>` — idempotent; `false` = no-op.
- `my_entered(conn, giveaway_id, member) -> Result<bool>` — self-only read.
- `cancel(conn, giveaway_id) -> Result<bool>` — `open` → `cancelled`; no draw, no announcement.
- `list_due(conn, now: i64) -> Result<Vec<GiveawayRow>>` — open rows past `ends_at`, for the sweeper.
- `list_open_in_channel(conn, channel_id: i64, limit: u32) -> Result<Vec<GiveawayRow>>` — the channel's `status='open'` giveaways, oldest-first (`id ASC`), for `ListActiveWidgets`. A due-but-unswept giveaway may appear for ≤15 s; its Enter is still rejected by the handler's `ends_at` check.
- `eligible_entrants(conn, giveaway_id) -> Result<Vec<PublicKey>>` — entrants still in the roster and not banned.

---

## `reminders.rs` — personal reminders

**Private by construction.** A reminder is never posted in a channel and is never visible to anyone but its owner: the only artifacts are an invoker-only `Notice` at creation and a DM from the server system identity when it comes due. Every read and mutation is scoped by `owner = ?` in SQL, and the owner is always the authenticated connection key (no request carries an owner field).

Bounds: `MAX_REMINDER_TEXT = 500`, `MAX_PENDING_PER_USER = 20`, duration 1 m–30 d, `REMINDER_DUE_BATCH = 200` rows per tick (the remainder drains next tick).

### `parse_reminder_args(args: &str) -> Result<ParsedReminder, String>`

Parses the `RunCommand` arg string `<duration> <text>` — `args.trim().splitn(2, whitespace)`; the first token must match `^(\d{1,4})(m|h|d)$` case-insensitively. Pure. Text is preserved **verbatim** (pipes and all — the grammar has no delimiter past the first space). Errors are the exact user-facing strings (`REMINDER_USAGE` const, `"duration must be between 1m and 30d"`, `"reminder text must be 1-500 characters"`).

### `humanize_delay(secs: u64) -> String`

`"45m"` / `"1h 30m"` / `"3 days 4h"` — used in the creation `Notice`.

### Row-level fns

- `create(conn, owner, channel_id: i64, text, due_at: i64, now: i64) -> Result<i64>` — insert, `status='pending'`.
- `count_pending(conn, owner) -> Result<i64>` — the per-user cap check.
- `list_pending_for(conn, owner) -> Result<Vec<ReminderRow>>` — owner's pending rows, `due_at ASC`, `LIMIT 20`. For `ListMyReminders`.
- `cancel(conn, id: i64, owner) -> Result<bool>` — `UPDATE … WHERE id=? AND owner=? AND status='pending'`; `false` = nothing to cancel (foreign / already-fired / nonexistent are indistinguishable — the handler maps all three to the same opaque `"reminder not found"`).
- `list_due(conn, now: i64) -> Result<Vec<ReminderRow>>` — `status='pending' AND due_at <= now`, `due_at ASC`, batch-capped. Sweeper half.
- `mark_sent(conn, id: i64, now: i64) -> Result<bool>` — single-shot guard, `pending` → `sent`; `false` ⇒ the caller must NOT DM.
- `to_info(row) -> ReminderInfo` — protocol projection.

---

## Events emitted

| Event name | Payload shape | Who listens |
|---|---|---|
| `ServerEvent::PollUpdated` | `{ poll: PollInfo }` | bridge.rs → `server:poll_updated` → `useServerEvents` → `POLL_UPDATED` |
| `ServerEvent::GiveawayUpdated` | `{ giveaway: GiveawayInfo }` | bridge.rs → `server:giveaway_updated` (winner mapped to `"vk_<hex>"` via `commands::giveaway_json`) → `GIVEAWAY_UPDATED` |
| `ServerEvent::NewMessage` | winner announcement / card message | normal message flow |

## Events / requests consumed

| Event / request | Source | What this module does with it |
|---|---|---|
| `RunCommand` (kind `poll`/`giveaway`) | handlers.rs dispatch | `create_poll_card` / `create_giveaway_card` |
| `GetPoll`/`VotePoll`/`RetractVote`/`ClosePoll` | handlers.rs | row fns above; visibility + timeout + rate-limit checks stay in handlers |
| `EnterGiveaway`/`LeaveGiveaway`/`CancelGiveaway`/`RerollGiveaway`/`GetGiveaway` | handlers.rs | ditto |
| `ListActiveWidgets` | handlers.rs | `polls::list_open_in_channel` + `giveaways::list_open_in_channel` (each `LIMIT 20`), merged by `created_at` asc, 20 combined; visibility on the requested `channel_id` with opaque `"channel not found"`; read — no limiter, no broadcasts, no per-viewer fields |
| `DeleteMessage` on a widget card | handlers.rs hook | open poll → `close`; open giveaway → `cancel` (no draw, no announcement) |
| `RunCommand` (kind `reminder`) | connection.rs dispatch | after every existing gate (`content_block_reason` → `command_limiter` → `check_run_command_channel_auth`): pure `parse_reminder_args`, one scoped lock doing `count_pending` (cap) + `create`, then an invoker-only `ServerResponse::Notice`. Nothing posts, nothing broadcasts. |
| `ListMyReminders` / `CancelReminder` | handlers.rs | `list_pending_for` / `cancel`, both owner-scoped in SQL; `CancelReminder` shares the `widget_limiter` |
| 15 s tick | `spawn_widget_sweeper` | `sweep_once` |

## Integration map

- **`messages.rs`** — `insert_message` + `set_widget(conn, message_id: u64, widget_json)` write the card; `MSG_SELECT` returns `widget` at index 10 into `MessageInfo.widget: Option<String>` (`#[serde(default)]` — old peers deserialize fine).
- **`handlers.rs`** — owns ALL gates: default-deny membership, `widget_channel_visible` (VIEW_CHANNEL / DM-participant with opaque not-found), `is_timed_out` on mutations, `widget_limiter` (`RateLimiter::new(10, 10)` on `ServerState`) for vote/retract/enter/leave, MANAGE_SERVER for giveaway creation, creator-or-mod for close/cancel/reroll.
- **`connection.rs` / `events.rs`** — `broadcast_event(state, target, event)` fans out the `PendingBroadcast`s after the DB guard drops.
- **Client** — `PollWidget.tsx` / `GiveawayWidget.tsx` render from the `polls`/`giveaways` context slices (see `frontend-state.md`); Group 26 of `tauri-commands.md` lists the nine interaction commands. `LinkedWidgetCard.tsx` re-mounts the same widgets under messages containing `farder://widget/...` links, resolving through the same `GetPoll`/`GetGiveaway` reads (opaque failures → a "not available" card; no new server surface — see `frontend-bridge.md` "Shareable widget links"). `ActiveWidgetsBar.tsx` lists the viewed channel's open widgets as chips under the channel header, fed by `ListActiveWidgets` (via the `list_active_widgets` command) and kept live by the `PollUpdated`/`GiveawayUpdated` broadcasts.

## Known gotchas

- `sweep_once` takes `now: u64` (from `db::now()`) but the tables store `i64` — cast at the boundary, everywhere.
- `close_and_draw` / `reroll_and_announce` take `&Connection` (`unchecked_transaction`) because the sweeper and handlers only hold `&Connection`; the two `create_*_card` fns take `&mut Connection` (`transaction()`).
- Never broadcast from inside the DB lock scope: collect `PendingBroadcast`s, drop the guard, then await.
- The card's `widget` JSON is server-written only; the client still parses it as untrusted (try/catch + numeric-id check in `Message.tsx`).
