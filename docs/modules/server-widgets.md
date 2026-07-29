# Widget substrate: polls, giveaways, events & reminders

> **File(s):** `crates/farder-server/src/widgets.rs`, `crates/farder-server/src/polls.rs`, `crates/farder-server/src/giveaways.rs`, `crates/farder-server/src/channel_events.rs`, `crates/farder-server/src/reminders.rs`
> **Layer:** Server crate
> **Last reviewed:** 2026-07-28

## Purpose

Interactive **message widgets**: a slash command of kind `"poll"`, `"giveaway"` or `"event"` (run via `RunCommand`) posts a normal card message whose `messages.widget` TEXT column carries `{"type":"poll"|"giveaway"|"event","id":<i64>}`; the feature's own table (`polls` / `giveaways` + their per-member vote/entry tables) holds the live state; interaction requests mutate it and broadcast a `PollUpdated` / `GiveawayUpdated` event; a single shared **sweeper task** (`widgets.rs`) retires timed widgets. These modules own storage and state transitions only — all authorization, rate limiting, and visibility checks live in `handlers.rs` dispatch; old clients that don't understand `widget` just render the card's plain-text content.

---

## `widgets.rs` — the shared sweeper

### `WIDGET_SWEEP_SECS: u64 = 15`

Fixed tick interval. A due poll closes / a due giveaway draws at most ~15 s late.

### `sweep_once(conn: &mut Connection, now: u64) -> SweepOutcome`

**What it does:** the sync tick body servicing every half: `polls::close_due` (every due timed poll → `PollUpdated`), then `giveaways::list_due` + `close_and_draw` per row (→ `GiveawayUpdated` + winner-announcement `NewMessage`), then `reminders::list_due` + `mark_sent` per row (→ one `PendingDm`, **zero broadcasts**; its footer is `— set in #<channel> · farder://channel/<id>`, but a reminder set **inside a DM** gets the link-free `— set in a direct message` instead, because a DM channel id is not in the client's channel list and has no name — see `reminder_dm_text`), then the three **event passes** (`sweep_events`; the system identity is resolved via `bots::get_or_create_system_identity` once per tick and ONLY when the start pass actually has rows, so a server that never starts an event never mints one):

1. **Lead-time pass** — `channel_events::list_reminder_due` + `mark_reminded` (single-shot) → one `PendingDm` per **going + maybe** responder (`⏰ "<title>" starts soon.` + optional location + `farder://widget/event/<channel>/<id>`). An event whose start is also due this tick is excluded by the query, so it gets the start DM only — no double-ping.
2. **Start pass** — `channel_events::list_start_due` + `start_and_announce` (one transaction: guarded `upcoming → started` flip + the `📅 <title> is starting now!` announcement authored by the system identity, `author_name_override = "Events"`, `author_badge = "BOT"`, `reply_to` = the card). `Ok(None)` (a Cancel won the guard) ⇒ announce nothing. Emits `EventUpdated` + `NewMessage`, plus one `PendingDm` per **going** responder.
3. **Cancel-notify pass** — `channel_events::list_cancel_unnotified` + `mark_cancel_notified` (single-shot) → one `PendingDm` per **going** responder (`❌ "<title>" was cancelled.`). **No channel message** — the card flip is the public record.

**Rung-2 class gate (the two announcement paths).** The giveaway draw and the event start pass are the only two message writers in the server with **no request layer in front of them** — nobody asks the sweeper to announce. So the gate lives in the tick: before `close_and_draw` and before `start_and_announce`, `channel_class::resolve(conn, row.channel_id)` is consulted, and anything that is not definitely `Plaintext` is **skipped with a `continue`** and a `warn!`. Two properties are load-bearing:

- **Skip, never abort.** The sweeper is one task for the whole server, so a single sealed (or unresolvable) channel must never starve every other channel's widgets. The tick continues to the next row.
- **Skip the flip too.** For events the status flip and its announcement share one transaction, so neither happens — the row stays `upcoming` and is skipped again on every later tick rather than becoming a deferred leak.

No widget can exist in an E2EE channel anyway (`RunCommand` create is refused, see `server-connection.md`), so this is defence in depth behind the `messages.rs` choke point, which would otherwise surface a hard error mid-tick.

It takes `&mut Connection` (not `&Connection`) purely because the start pass opens a real `conn.transaction()`.
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

## `channel_events.rs` — event cards (RSVP)

**Naming:** the `events` table is the mesh signed log and `events.rs` is `EventTarget`/`BroadcastEvent`, so the product's event cards live in `channel_events` / `channel_event_rsvps` and in THIS module. Protocol/client names stay product-facing (`EventInfo`, `GetEvent`, `EventUpdated`).

`starts_at` is an ABSOLUTE unix second — nothing timezone-shaped is stored or transmitted; every client renders it locally. Bounds: `MAX_TITLE = 120`, `MAX_LOCATION = 120`, `MAX_DESCRIPTION = 500`, `MIN_LEAD_SECS = 60`, `MAX_AHEAD_SECS = 365 d`, `REMIND_LEADS = [900, 3600, 86400]`, `ATTENDEE_NAME_CAP = 10`, `EVENT_DUE_BATCH = 200` rows per sweeper pass (the remainder drains next tick).

### `parse_event_args(args: &str) -> Result<ParsedEvent, String>`

`Title | <when> [| location] [| description] [| remind 15m|1h|1d|none]` — split on `|`, each segment trimmed (the `/poll` idiom). A **final** `remind …` segment is always consumed as the lead (deterministic); the rest are strictly POSITIONAL (an empty third segment means "no location"). `<when>` accepts only `^(\d{1,4})(m|h|d)$` (a leading `in ` is stripped) → `WhenSpec::Relative`, or `^@(\d{9,12})$` → `WhenSpec::Absolute` (what the builder emits). A wall-clock string is **refused** with `WHEN_HINT` — the server cannot know the invoker's timezone, and assuming UTC is the bug class that lands an event 8 hours off. Pure, no DB.

### `resolve_start(when: &WhenSpec, now: u64) -> Result<u64, String>` / `validate_event_fields(title, description, location)`

The two pure validators. **Creation and `EditEvent` both call them**, so the two paths cannot drift.

### `create_event_card(conn: &mut Connection, channel_id: u64, invoker: &PublicKey, parsed: &ParsedEvent, now: u64) -> Result<(MessageInfo, EventInfo)>`

One transaction: fallback-content message (`📅 <title> — <UTC stamp>` + optional location/description; the stamp borrows SQLite's `strftime` since the crate has no date dependency) with **plain invoker authorship — no name override, no badge** → `create` → `messages::set_widget(… {"type":"event","id":N})` → `get_message` + `build_info`.

### `build_info(conn, &EventRow) -> Result<EventInfo>`

One RSVP query (`ORDER BY updated_at, rowid`), bucketed by response: `*_count` are the **full** totals, `*_names` the first `ATTENDEE_NAME_CAP` resolved via `members::get_member` (departed members are skipped in the names but still counted). Attendee public keys never leave the server.

### `start_and_announce(conn: &mut Connection, row, system_pk, now) -> Result<Option<(EventInfo, MessageInfo)>>`

Guarded `status='upcoming'` flip + the announcement insert in ONE transaction. Rows-affected 0 ⇒ rollback + `Ok(None)`, announcing nothing — this is what makes the announcement exactly-once across crashes and restarts.

### Row-level fns

- `create(conn, channel_id: i64, message_id: i64, creator, parsed, starts_at: i64, now: i64) -> Result<i64>`, `get(conn, id) -> Result<Option<EventRow>>`.
- `rsvp(conn, event_id, member, response, now)` — `INSERT … ON CONFLICT(event_id, member) DO UPDATE` (the `poll_votes` idiom); `clear_rsvp(…) -> Result<bool>` (rows-affected); `my_rsvp(…) -> Result<Option<String>>`; `responders(conn, event_id, responses: &[&str]) -> Result<Vec<PublicKey>>` (the DM audiences).
- `cancel(conn, id, now) -> Result<bool>` — guarded on `status='upcoming'`; `edit(conn, id, title, description, location, starts_at, remind_lead, rearm_reminder)` — upcoming only, NULLs `reminded_at` when `rearm_reminder`.
- `list_reminder_due(conn, now)` / `list_start_due(conn, now)` / `list_cancel_unnotified(conn)` — the three sweeper queries, each `ORDER BY id ASC LIMIT EVENT_DUE_BATCH` (200, the sibling of `REMINDER_DUE_BATCH`): after downtime the overdue backlog is unbounded, and every row in a pass is a write (plus, in the start pass, an announcement INSERT) held under the single `state.db` mutex. The remainder drains on the next tick, and the guarded UPDATEs make a carried-over row unable to fire twice. `responders` is **deliberately uncapped** — a truncated due list is deferred work, but a truncated responder list would silently drop attendees with no state left to retry from.
- `list_upcoming_in_channel(conn, channel_id, now, limit)` — the active-bar query (`starts_at > now` excludes a due-but-unswept event).
- `mark_reminded(conn, id, now)` / `mark_cancel_notified(conn, id, now)` — guarded single-shot `UPDATE … WHERE x IS NULL`; `false` ⇒ the caller must NOT DM.

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
| `ServerEvent::EventUpdated` | `{ event: EventInfo }` | bridge.rs → `server:event_updated` → `useServerEvents` → `EVENT_UPDATED` |
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
| `RunCommand` (kind `event`) | connection.rs dispatch | pure `parse_event_args` + `resolve_start`, then `create_event_card` under one scoped lock; broadcasts `NewMessage` then `EventUpdated`. No MANAGE_SERVER gate. |
| `GetEvent`/`RsvpEvent`/`ClearRsvp`/`CancelEvent`/`EditEvent` | handlers.rs | row fns above; visibility (opaque `"event not found"`), timeout and rate-limit checks stay in handlers |
| `DeleteMessage` on an event card | handlers.rs hook | upcoming event → `cancel` (+`EventUpdated`); rows retained, no announcement ever posts |
| 15 s tick | `spawn_widget_sweeper` | `sweep_once` |

## Integration map

- **`messages.rs`** — `insert_message` + `set_widget(conn, message_id: u64, widget_json)` write the card; `MSG_SELECT` returns `widget` at index 10 into `MessageInfo.widget: Option<String>` (`#[serde(default)]` — old peers deserialize fine).
- **`handlers.rs`** — owns ALL gates: default-deny membership, `widget_channel_visible` (VIEW_CHANNEL / DM-participant with opaque not-found), `is_timed_out` on mutations, `widget_limiter` (`RateLimiter::new(10, 10)` on `ServerState`) for vote/retract/enter/leave and rsvp/clear/cancel-event/edit-event/cancel-reminder, MANAGE_SERVER for giveaway creation, creator-or-mod for close/cancel/reroll.
- **`connection.rs` / `events.rs`** — `broadcast_event(state, target, event)` fans out the `PendingBroadcast`s after the DB guard drops.
- **Client** — `PollWidget.tsx` / `GiveawayWidget.tsx` render from the `polls`/`giveaways` context slices (see `frontend-state.md`); Group 26 of `tauri-commands.md` lists the nine interaction commands. `LinkedWidgetCard.tsx` re-mounts the same widgets under messages containing `farder://widget/...` links, resolving through the same `GetPoll`/`GetGiveaway` reads (opaque failures → a "not available" card; no new server surface — see `frontend-bridge.md` "Shareable widget links"). `ActiveWidgetsBar.tsx` lists the viewed channel's open widgets as chips under the channel header, fed by `ListActiveWidgets` (via the `list_active_widgets` command) and kept live by the `PollUpdated`/`GiveawayUpdated` broadcasts.

## Known gotchas

- `sweep_once` takes `now: u64` (from `db::now()`) but the tables store `i64` — cast at the boundary, everywhere.
- `sweep_once` takes `&mut Connection` (the event start pass opens a transaction); the sweeper passes `&mut *conn` from its `MutexGuard`, still inside the scoped lock block.
- `close_and_draw` / `reroll_and_announce` take `&Connection` (`unchecked_transaction`) because the sweeper and handlers only hold `&Connection`; the two `create_*_card` fns take `&mut Connection` (`transaction()`).
- Never broadcast from inside the DB lock scope: collect `PendingBroadcast`s, drop the guard, then await.
- The card's `widget` JSON is server-written only; the client still parses it as untrusted (try/catch + numeric-id check in `Message.tsx`).
