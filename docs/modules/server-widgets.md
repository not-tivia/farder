# Widget substrate: polls & giveaways

> **File(s):** `crates/farder-server/src/widgets.rs`, `crates/farder-server/src/polls.rs`, `crates/farder-server/src/giveaways.rs`
> **Layer:** Server crate
> **Last reviewed:** 2026-07-27

## Purpose

Interactive **message widgets**: a slash command of kind `"poll"` or `"giveaway"` (run via `RunCommand`) posts a normal card message whose `messages.widget` TEXT column carries `{"type":"poll"|"giveaway","id":<i64>}`; the feature's own table (`polls` / `giveaways` + their per-member vote/entry tables) holds the live state; interaction requests mutate it and broadcast a `PollUpdated` / `GiveawayUpdated` event; a single shared **sweeper task** (`widgets.rs`) retires timed widgets. These modules own storage and state transitions only — all authorization, rate limiting, and visibility checks live in `handlers.rs` dispatch; old clients that don't understand `widget` just render the card's plain-text content.

---

## `widgets.rs` — the shared sweeper

### `WIDGET_SWEEP_SECS: u64 = 15`

Fixed tick interval. A due poll closes / a due giveaway draws at most ~15 s late.

### `sweep_once(conn: &Connection, now: u64) -> Vec<PendingBroadcast>`

**What it does:** the sync tick body servicing BOTH halves: `polls::close_due` (every due timed poll → `PollUpdated`), then `giveaways::list_due` + `close_and_draw` per row (→ `GiveawayUpdated` + winner-announcement `NewMessage`).
**Returns / emits:** the broadcasts to send; each half's errors are `tracing::warn!`ed per-item and never panic the sweeper.
**Side effects:** persists every state change (close / draw / announcement insert) BEFORE returning — i.e. under the caller's DB lock, before any broadcast. Persist-then-broadcast by construction: a crash between persist and broadcast can never re-close or redraw.
**Connects to:** `spawn_widget_sweeper` (the only production caller); tests call it directly without tokio.

### `spawn_widget_sweeper(state: Arc<ServerState>) -> JoinHandle<()>`

**What it does:** spawns the single background loop (started in `main.rs` next to the bot poller): every 15 s it takes a scoped `state.db` lock, runs `sweep_once`, drops the guard, then `broadcast_event`s each `PendingBroadcast` (no DB MutexGuard is ever held across an `.await` — the bots.rs `spawn_bot_poll_task` lock discipline).

### `PendingBroadcast { target: EventTarget, event: ServerEvent }`

A broadcast computed under the DB lock, sent after the guard drops.

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
- `eligible_entrants(conn, giveaway_id) -> Result<Vec<PublicKey>>` — entrants still in the roster and not banned.

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
| `DeleteMessage` on a widget card | handlers.rs hook | open poll → `close`; open giveaway → `cancel` (no draw, no announcement) |
| 15 s tick | `spawn_widget_sweeper` | `sweep_once` |

## Integration map

- **`messages.rs`** — `insert_message` + `set_widget(conn, message_id: u64, widget_json)` write the card; `MSG_SELECT` returns `widget` at index 10 into `MessageInfo.widget: Option<String>` (`#[serde(default)]` — old peers deserialize fine).
- **`handlers.rs`** — owns ALL gates: default-deny membership, `widget_channel_visible` (VIEW_CHANNEL / DM-participant with opaque not-found), `is_timed_out` on mutations, `widget_limiter` (`RateLimiter::new(10, 10)` on `ServerState`) for vote/retract/enter/leave, MANAGE_SERVER for giveaway creation, creator-or-mod for close/cancel/reroll.
- **`connection.rs` / `events.rs`** — `broadcast_event(state, target, event)` fans out the `PendingBroadcast`s after the DB guard drops.
- **Client** — `PollWidget.tsx` / `GiveawayWidget.tsx` render from the `polls`/`giveaways` context slices (see `frontend-state.md`); Group 26 of `tauri-commands.md` lists the nine interaction commands.

## Known gotchas

- `sweep_once` takes `now: u64` (from `db::now()`) but the tables store `i64` — cast at the boundary, everywhere.
- `close_and_draw` / `reroll_and_announce` take `&Connection` (`unchecked_transaction`) because the sweeper and handlers only hold `&Connection`; the two `create_*_card` fns take `&mut Connection` (`transaction()`).
- Never broadcast from inside the DB lock scope: collect `PendingBroadcast`s, drop the guard, then await.
- The card's `widget` JSON is server-written only; the client still parses it as untrusted (try/catch + numeric-id check in `Message.tsx`).
