# Poll + Giveaway Widgets (interactive command kinds, v1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. **Each task below is executed by one build agent; tasks are ordered by dependency — T1 before T2/T3, T2+T3 before T4, T4 before T5/T6, everything before T7.**

**Specs:** [[2026-07-27-poll-command-design]] + [[2026-07-27-giveaway-command-design]] — two sibling specs against ONE shared widget substrate. This plan interleaves them: the substrate is built once (T1), each feature's server half lands whole (T2, T3), the client plumbing for both lands together (T4), then one widget UI each (T5, T6), then config UI + docs (T7).

**Goal:** Two interactive slash-command kinds on the shipped framework: `/poll Question | A | B [| 2h]` posts a live-voting poll card; `/giveaway 24h Steam key` (mod-only) posts an Enter/Leave card whose winner the server draws at the deadline.

**Architecture:** `messages.widget` TEXT column holds a JSON pointer (`{"type":"poll","id":N}` / `{"type":"giveaway","id":N}`) to per-feature tables (`polls`+`poll_votes`, `giveaways`+`giveaway_entries`). Interaction = new membership-gated ServerRequests acting as the authenticated connection key; live state = `PollUpdated`/`GiveawayUpdated` events to `Subscribers(channel_id)`; recovery = `GetPoll`/`GetGiveaway` read requests (history wire format untouched). One shared sweeper task (`widgets::spawn_widget_sweeper`, 15 s tick) closes due polls and draws due giveaways, persist-then-broadcast.

**Tech Stack:** Rust (`farder-server`, `farder-protocol`), rusqlite, `rand 0.8` (already a dep), Tauri, React/TS.

## Global Constraints (standard gates — every task ends green on the ones it touches)

- **Rust:** `cargo test -p farder-server 2>&1 | tail` and `cargo build --workspace 2>&1 | tail` clean.
- **Client crate (NON-workspace — the MemberApproved-class regression):** after ANY `farder-protocol` change: `cd client/src-tauri && cargo build`. `cargo build --workspace` does NOT cover it.
- **Frontend:** `cd client && npx tsc --noEmit` clean.
- **Seam:** every `invoke("X")` name = `#[tauri::command] fn X` = an entry in `generate_handler!` in `client/src-tauri/src/main.rs`. Zero drift; grep-audit before committing.
- **Themes:** every new className styled in ALL THREE `client/src/themes/*/theme.css` (`xp-luna-blue`, `discord-dark`, `hello-kitty`), colors only via `var(--xp-…)`. Verify with `grep -l "<class>" client/src/themes/*/theme.css` → 3 files.
- **Lock discipline:** no DB `MutexGuard` held across any `.await`, anywhere (bots.rs:364 pattern: scoped lock block, collect, drop guard, then broadcast).
- **Timestamps:** unix seconds via `db::now()` everywhere (same unit as `messages.timestamp`).
- **Docs-with-code:** tauri-commands.md / tauri-bridge.md / frontend-context.md entries land in the same commit as the surface they document (final sweep in T7).
- **Verify-before-done:** unit tests + builds gate each task; full runtime behavior is the owner's Windows verification (both specs' "Owner runtime verification" sections), since WSL can't run the GUI.

---

### Task 1: SUBSTRATE (server) — widget column, MessageInfo.widget, set_widget, empty sweeper

**Files:** `crates/farder-server/src/db.rs`; `crates/farder-server/src/messages.rs`; `crates/farder-protocol/src/server.rs`; `crates/farder-server/src/widgets.rs` (new); `crates/farder-server/src/lib.rs`; `crates/farder-server/src/main.rs`.

**Interfaces produced:** `MessageInfo.widget: Option<String>`; `messages::set_widget`; `widgets::{WIDGET_SWEEP_SECS, PendingBroadcast, sweep_once, spawn_widget_sweeper}`.

- [ ] **Step 1: Migration.** `db.rs` `init_schema`: guarded `ALTER TABLE messages ADD COLUMN widget TEXT` using the existing PRAGMA-table_info idiom (db.rs:317-324, same as `author_badge`). Nullable, no default — old rows read `None`.

- [ ] **Step 2: MSG_SELECT + MessageInfo.** VERIFY FIRST: `crates/farder-server/src/messages.rs:12-13` — `MSG_SELECT` currently ends `..., author_name_override, author_badge` (author_badge at index 9). Append `, widget` LAST → index 10; update the doc comment. `row_to_message_info`: `let widget: Option<String> = row.get(10)?;` and set it on the struct. `farder-protocol/src/server.rs`: add to `MessageInfo`, after `author_badge`:
```rust
    #[serde(default)]
    pub widget: Option<String>,
```
Fix every `MessageInfo { .. }` struct-literal site the compiler flags (`widget: None` for all non-widget constructors, incl. tests).

- [ ] **Step 3: set_widget helper.** `messages.rs`:
```rust
/// Stamps the widget JSON on an already-inserted message (insert-then-set-widget idiom:
/// resolves the message-id <-> feature-row-id circularity without touching insert signatures).
pub fn set_widget(conn: &Connection, message_id: i64, widget_json: &str) -> Result<()> {
    conn.execute("UPDATE messages SET widget = ?1 WHERE id = ?2", params![widget_json, message_id])?;
    Ok(())
}
```

- [ ] **Step 4: widgets.rs skeleton.** New module (`pub mod widgets;` in `lib.rs`):
```rust
pub const WIDGET_SWEEP_SECS: u64 = 15;

/// A broadcast computed under the DB lock, to be sent after the guard drops.
pub struct PendingBroadcast {
    pub target: crate::events::EventTarget,
    pub event: farder_protocol::server::ServerEvent,
}

/// Sync tick body servicing BOTH widget halves (polls: T2; giveaways: T3).
/// Extracted so tests run it without tokio. T1 skeleton: nothing due, ever.
pub fn sweep_once(_conn: &rusqlite::Connection, _now: u64) -> Vec<PendingBroadcast> {
    Vec::new()
}

pub fn spawn_widget_sweeper(state: std::sync::Arc<crate::state::ServerState>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let pending: Vec<PendingBroadcast> = {
                let conn = state.db.lock().unwrap();
                sweep_once(&conn, crate::db::now())
            }; // MutexGuard dropped here
            for pb in pending {
                crate::connection::broadcast_event(&state, pb.target, pb.event).await;
            }
            tokio::time::sleep(std::time::Duration::from_secs(WIDGET_SWEEP_SECS)).await;
        }
    })
}
```
Match the real `broadcast_event` signature / `EventTarget` path / `ServerState` field names against `bots::spawn_bot_poll_task` (bots.rs:364) — copy its lock discipline verbatim. Sweep-immediately-then-sleep ordering as shown (specs: "poll-then-sleep").

- [ ] **Step 5: Spawn.** `main.rs` (next to line 143's `let _bot_poller = ...`): `let _widget_sweeper = farder_server::widgets::spawn_widget_sweeper(Arc::clone(&state));`

- [ ] **Step 6: Tests + gates + commit.** Tests (messages.rs / db.rs `mod tests`): (a) existing `test_schema_init_idempotent` passes (covers the guarded ALTER + reruns); (b) `set_widget` then `get_message` → `MessageInfo.widget == Some(json)`; a plain insert reads `widget: None`; (c) serde roundtrip: a `MessageInfo` encoded WITHOUT `widget` decodes with `widget: None` (`#[serde(default)]` guard). Gates: `cargo test -p farder-server`, `cargo build --workspace`, `cd client/src-tauri && cargo build` (protocol struct changed).
```bash
git add crates/farder-protocol/src/server.rs crates/farder-server/src/db.rs crates/farder-server/src/messages.rs crates/farder-server/src/widgets.rs crates/farder-server/src/lib.rs crates/farder-server/src/main.rs
git commit -m "feat(widgets): message widget column + set_widget + shared sweeper skeleton"
```

---

### Task 2: POLL SERVER — polls module, protocol, dispatch, handlers, sweeper wiring

**Files:** `crates/farder-server/src/db.rs`; `crates/farder-server/src/polls.rs` (new); `crates/farder-server/src/lib.rs`; `crates/farder-protocol/src/server.rs`; `crates/farder-server/src/state.rs`; `crates/farder-server/src/commands.rs`; `crates/farder-server/src/connection.rs`; `crates/farder-server/src/handlers.rs`; `crates/farder-server/src/widgets.rs`.

**Interfaces produced:** `polls::{parse_poll_args, ParsedPoll, PollRow, create, get, build_info, vote, retract, close, close_due, my_vote}`; `PollInfo`; `ServerRequest::{GetPoll, VotePoll, RetractVote, ClosePoll}`; `ServerResponse::Poll`; `ServerEvent::PollUpdated`; `ServerState.widget_limiter`; RunCommand kind `"poll"`; sweeper poll half.

- [ ] **Step 1: DDL.** `db.rs` after the `commands` block, DDL exactly per the poll spec ("Data model"):
`polls (id INTEGER PRIMARY KEY AUTOINCREMENT, channel_id INTEGER NOT NULL, message_id INTEGER NOT NULL, creator BLOB NOT NULL, question TEXT NOT NULL, options TEXT NOT NULL, created_at INTEGER NOT NULL, closes_at INTEGER, closed_at INTEGER)`; `poll_votes (poll_id INTEGER NOT NULL, voter BLOB NOT NULL, option_index INTEGER NOT NULL, voted_at INTEGER NOT NULL, PRIMARY KEY (poll_id, voter))`. `CREATE TABLE IF NOT EXISTS` both.

- [ ] **Step 2: polls.rs (tests first).** New module (`pub mod polls;` in `lib.rs`), mirroring `commands.rs` style:
```rust
pub struct ParsedPoll { pub question: String, pub options: Vec<String>, pub duration_secs: Option<u64> }
pub fn parse_poll_args(args: &str) -> Result<ParsedPoll, String>;
pub struct PollRow { pub id: i64, pub channel_id: i64, pub message_id: i64, pub creator: PublicKey,
    pub question: String, pub options: Vec<String>, pub created_at: i64,
    pub closes_at: Option<i64>, pub closed_at: Option<i64> }
pub fn create(conn, channel_id: i64, message_id: i64, creator: &PublicKey, question: &str,
    options: &[String], closes_at: Option<i64>) -> Result<i64>;
pub fn get(conn, id: i64) -> Result<Option<PollRow>>;
pub fn build_info(conn, row: &PollRow) -> Result<PollInfo>;   // counts via GROUP BY; closed = closed_at.is_some()
pub fn vote(conn, poll_id: i64, voter: &PublicKey, option_index: u32) -> Result<()>;  // INSERT .. ON CONFLICT(poll_id,voter) DO UPDATE
pub fn retract(conn, poll_id: i64, voter: &PublicKey) -> Result<bool>;                // rows-affected > 0
pub fn close(conn, poll_id: i64, now: i64) -> Result<()>;
pub fn close_due(conn, now: i64) -> Result<Vec<PollInfo>>;    // WHERE closed_at IS NULL AND closes_at IS NOT NULL AND closes_at <= now; sets closed_at = now; returns built infos (closed: true)
pub fn my_vote(conn, poll_id: i64, voter: &PublicKey) -> Result<Option<u32>>;
```
Parse rules exactly per spec: split on `|`, trim; final segment matching `^(\d{1,4})(m|h|d)$` case-insensitive is ALWAYS the duration (1m-30d after conversion, else `Err("duration must be between 1m and 30d")`); then question 1-256 chars, 2-10 options each 1-100 chars, case-insensitive dup + empty-segment rejection; violations → usage-string `Err` (`"usage: /<trigger> Question | option A | option B [| 30m|2h|1d]"` or the specific reason). Options stored as a JSON array TEXT column.
**Tests (unit, per spec test plan):** parse — happy 2 and 10 options; trimming; `30m`/`2H`/`7d`; reject `0m`/`31d`; determinism `q | 1h | 2h` → usage error; 1 and 11 options; case-insensitive dups; empty segments; over-length question/option; no duration → `None`. Module (in-memory conn) — create + build_info counts; vote upsert moves counts between indices with stable total; retract false on no-vote; my_vote; close_due closes only due timed polls (untimed + already-closed untouched), returns `closed: true`, and rows persist closed even when the return is dropped.

- [ ] **Step 3: Protocol.** `farder-protocol/src/server.rs`: `PollInfo` exactly as specced (id/channel_id/message_id/creator/question/options/counts/total_votes/created_at/closes_at/closed — `counts: Vec<u32>` aligned with options). `ServerRequest::{GetPoll { poll_id: i64 }, VotePoll { poll_id: i64, option_index: u32 }, RetractVote { poll_id: i64 }, ClosePoll { poll_id: i64 }}`; `ServerResponse::Poll { poll: PollInfo, my_vote: Option<u32> }`; `ServerEvent::PollUpdated { poll: PollInfo }`. Appended variants (MessagePack externally-tagged). No separate PollClosed — close folds into `PollUpdated` with `closed: true`.

- [ ] **Step 4: widget_limiter.** `state.rs`: `pub widget_limiter: RateLimiter` on `ServerState`, init `RateLimiter::new(10, 10)` (match the real ctor/`allow`-vs-`check` naming used by `command_limiter`). Shared by T3.

- [ ] **Step 5: Dispatch — kind "poll".** `commands.rs` `list_infos`: `takes_arg: matches!(r.kind.as_str(), "api" | "poll" | "giveaway")` (both new kinds now — T3 needs no touch here). `handlers.rs` `AddCommand` kind match: `"poll"` arm accepting NO extra fields; error text → `"kind must be 'text', 'api', 'poll' or 'giveaway'"` (giveaway arm itself lands in T3). `connection.rs` RunCommand kind match (after the existing gates: content-block → `command_limiter` → `check_run_command_channel_auth` → trigger lookup), new `"poll"` branch:
  1. `polls::parse_poll_args(&args)` — `Err(reason)` → `Error { reason }` to invoker, nothing posts. (Pure; no lock.)
  2. One scoped `state.db.lock()` block wrapped in a SQLite transaction: fallback `content` = `"📊 Poll: {question}\n"` + `"- {option}"` lines; `mid = messages::insert_message(&conn, channel_id, &member_key, &content, None)` (PLAIN invoker authorship — no override, no badge); `poll_id = polls::create(&conn, channel_id, mid, &member_key, &question, &options, duration_secs.map(|d| now + d))`; `messages::set_widget(&conn, mid, &format!(r#"{{"type":"poll","id":{poll_id}}}"#))`; `msg = get_message`, `info = polls::build_info`. Guard drops.
  3. `broadcast_event(Subscribers(channel_id), NewMessage { message: msg }).await`, then `PollUpdated { poll: info }`, then `ServerResponse::Ok`.

- [ ] **Step 6: Handlers.** Four sync `handle_request` arms (all membership-gated automatically by default-deny `request_requires_membership` — do NOT add them to the bootstrap allow-list). Shared visibility check: `channels::get_channel` → DM ⇒ `is_dm_participant`, else `resolve_member_perms_pub` + VIEW_CHANNEL; ANY visibility failure → the same `err("poll not found")` (no existence oracle).
  - **GetPoll:** load → visibility → `ok(Poll { poll: build_info, my_vote })`. No timeout gate (reads allowed while timed out).
  - **VotePoll:** `widget_limiter.allow(caller)` → `require_not_timed_out` → load → closed check `closed_at.is_some() || closes_at.map_or(false, |t| now >= t)` → `err("poll is closed")` → visibility → `option_index < options.len()` else `err("invalid option")` → `polls::vote` → `ok_with(Ok, [PollUpdated → Subscribers(channel_id)])`.
  - **RetractVote:** same gates minus index check → `retract`; `false` → plain `Ok`, NO event (idempotent); `true` → `ok_with(Ok, [PollUpdated])`.
  - **ClosePoll:** `require_not_timed_out` → load → already closed → `err("poll already closed")` → creator OR `require_base_perm(MANAGE_SERVER)` → `close` → `ok_with(Ok, [PollUpdated])` (`closed: true`).
  - **DeleteMessage hook:** after existing authz, if the loaded `msg.widget` parses to `{"type":"poll","id"}` and the poll is open → `polls::close` + push `PollUpdated` (closed) alongside `MessageDeleted`. Rows retained.

- [ ] **Step 7: Sweeper poll half.** `widgets::sweep_once`: `for info in polls::close_due(conn, now as i64)? { out.push(PendingBroadcast { target: Subscribers(info.channel_id), event: PollUpdated { poll: info } }) }` (persist happens inside `close_due`, under the caller's lock — persist-before-broadcast by construction).

- [ ] **Step 8: Handler tests + gates + commit.** Tests (handlers.rs `mod tests` fixtures — `setup()`/`add_member`/`make_channel`/`fake_state`), per the spec test plan: VotePoll happy → `PollUpdated` to Subscribers; vote on closed → err; past-closes_at-unswept → err; bad index → err; no VIEW_CHANNEL → err ("poll not found"); timed-out → err; RetractVote idempotent (no event); ClosePoll non-creator non-mod → MANAGE_SERVER err, creator → closed, MANAGE_SERVER holder → closed, double-close → err; GetPoll `my_vote` correct per requester; DeleteMessage on a poll card → poll closed + both events; AddCommand accepts kind `poll` with no extra fields; `list_infos` `takes_arg: true` for poll; RunCommand poll-kind → message row (invoker author, NULL badge/override) + poll row + widget JSON cross-linked; parse failure posts nothing; `sweep_once` closes a due poll and returns one PendingBroadcast, second call returns none (idempotent). Gates: `cargo test -p farder-server`, `cargo build --workspace`, `cd client/src-tauri && cargo build` (new protocol variants).
```bash
git add crates/farder-protocol/src/server.rs crates/farder-server/src/db.rs crates/farder-server/src/polls.rs crates/farder-server/src/lib.rs crates/farder-server/src/state.rs crates/farder-server/src/commands.rs crates/farder-server/src/connection.rs crates/farder-server/src/handlers.rs crates/farder-server/src/widgets.rs
git commit -m "feat(polls): poll tables + module + protocol + dispatch + handlers + sweeper close"
```

---

### Task 3: GIVEAWAY SERVER — giveaways module, protocol, dispatch, handlers, draw

**Files:** `crates/farder-server/src/db.rs`; `crates/farder-server/src/giveaways.rs` (new); `crates/farder-server/src/lib.rs`; `crates/farder-protocol/src/server.rs`; `crates/farder-server/src/connection.rs`; `crates/farder-server/src/handlers.rs`; `crates/farder-server/src/widgets.rs`.

**Interfaces produced:** `giveaways::{parse_giveaway_duration, GiveawayRow, create, get, build_info, enter, leave, cancel, reroll, list_due, close_and_draw, my_entered}`; `GiveawayInfo`; `ServerRequest::{EnterGiveaway, LeaveGiveaway, CancelGiveaway, RerollGiveaway, GetGiveaway}`; `ServerResponse::Giveaway`; `ServerEvent::GiveawayUpdated`; RunCommand kind `"giveaway"`; sweeper giveaway half.

- [ ] **Step 1: DDL.** `db.rs`, per the giveaway spec: `giveaways (id, channel_id, message_id, creator BLOB, prize TEXT, ends_at INTEGER, status TEXT NOT NULL DEFAULT 'open', winner BLOB, created_at INTEGER)`; `giveaway_entries (giveaway_id, member BLOB, entered_at, PRIMARY KEY (giveaway_id, member))`. No FK to messages (delete = cancel via hook, rows retained).

- [ ] **Step 2: giveaways.rs (tests first).** `pub mod giveaways;` in `lib.rs`:
```rust
pub fn parse_giveaway_duration(s: &str) -> Option<u64>;   // ^([0-9]+)([mhd])$ ci; 1m..=30d in secs
pub struct GiveawayRow { pub id: i64, pub channel_id: i64, pub message_id: i64, pub creator: PublicKey,
    pub prize: String, pub ends_at: i64, pub status: String, pub winner: Option<PublicKey>, pub created_at: i64 }
pub fn create(conn, channel_id: i64, message_id: i64, creator: &PublicKey, prize: &str, ends_at: i64) -> Result<i64>;
pub fn get(conn, id: i64) -> Result<Option<GiveawayRow>>;
pub fn build_info(conn, row: &GiveawayRow) -> Result<GiveawayInfo>;  // entry_count COUNT(*); winner_name via members::get_member when ended
pub fn enter(conn, giveaway_id: i64, member: &PublicKey, now: i64) -> Result<bool>;  // INSERT OR IGNORE; rows-affected
pub fn leave(conn, giveaway_id: i64, member: &PublicKey) -> Result<bool>;
pub fn cancel(conn, giveaway_id: i64) -> Result<bool>;    // UPDATE .. SET status='cancelled' WHERE id=? AND status='open'
pub fn list_due(conn, now: i64) -> Result<Vec<GiveawayRow>>;  // WHERE status='open' AND ends_at <= now
pub fn close_and_draw(conn, row: &GiveawayRow) -> Result<(GiveawayInfo, MessageInfo)>;  // see Step 5
pub fn reroll(conn, giveaway_id: i64, winner: &PublicKey) -> Result<()>;
pub fn my_entered(conn, giveaway_id: i64, member: &PublicKey) -> Result<bool>;
```
**Tests:** duration parse — `30m`/`24h`/`7d`/case-insensitive → secs; `0m`, `31d`, `5w`, `banana`, empty → None; bounds 1m/30d inclusive. Module — enter idempotent (double-enter one row), leave on no-entry false, cancel single-shot, list_due respects ends_at/status.

- [ ] **Step 3: Protocol.** `GiveawayInfo` exactly as specced (id/channel_id/message_id/creator/prize/ends_at/status/entry_count/winner/winner_name — **`entry_count: u32`, NOT an entrant list**, per the spec's DECISION: entrant identities never leave the server in v1; the only per-viewer bit, `my_entered`, rides solely in the GetGiveaway response). Five request variants (each `{ giveaway_id: i64 }`), `ServerResponse::Giveaway { giveaway: GiveawayInfo, my_entered: bool }`, `ServerEvent::GiveawayUpdated { giveaway: GiveawayInfo }`. Appended variants; terminal states fold into `status` + `winner`.

- [ ] **Step 4: Dispatch — kind "giveaway".** `handlers.rs` `AddCommand`: `"giveaway"` arm, no kind-specific fields (error text already widened in T2). `connection.rs` RunCommand `"giveaway"` branch (after the shared gates):
  1. **MANAGE_SERVER gate at dispatch:** scoped lock → `resolve_member_server_perms(&conn, &member_key, is_owner)`; missing → `Error { "giveaways can only be started by moderators (missing MANAGE_SERVER)" }`.
  2. **Parse:** `args.trim().splitn(2, char::is_whitespace)` → duration token via `parse_giveaway_duration` (fail → `Error { "usage: /<trigger> <duration> <prize> — duration 1m–30d (e.g. 30m, 24h, 7d)" }`); prize trimmed 1-200 chars (fail → `Error { "prize must be 1–200 characters" }`).
  3. **Create (one scoped lock, one BEGIN/COMMIT):** fallback content `"🎉 Giveaway: <prize> — ends <ends_at>"`; `mid = insert_message(...)` plain invoker authorship; `gid = giveaways::create(&conn, channel_id, mid, &member_key, prize, now + duration_secs)`; `set_widget(&conn, mid, &format!(r#"{{"type":"giveaway","id":{gid}}}"#))`; build `msg` + `info`; guard drops (no `.await` inside the arm).
  4. `NewMessage` then `GiveawayUpdated` to `Subscribers(channel_id)` → `Ok`.

- [ ] **Step 5: Sweeper draw + announcement.** `giveaways::close_and_draw(conn, &row)` inside BEGIN/COMMIT: load entrants; filter eligible = `members::get_member(conn, pk)` is `Some(m)` with `!m.banned && !m.revoked` (draw-time re-check); winner = `eligible[rand::thread_rng().gen_range(0..eligible.len())]` when non-empty else `None`; `UPDATE giveaways SET status='ended', winner=?2 WHERE id=?1 AND status='open'` (single-shot guard — a concurrent Cancel that won the lock leaves nothing to draw); announcement in the SAME transaction: `insert_message_with_author_name(conn, channel_id, &announce_key, &text, Some(row.message_id), Some("Giveaway"), Some("BOT"))` where `announce_key` = freshly generated non-member `Keypair::generate().public_key()` (secret discarded — webhook precedent), text `"🎉 <display_name> won: <prize>"` (name via `members::get_member`, short-key fallback) or `"🎉 Giveaway ended — no entries: <prize>"`. Returns the built `GiveawayUpdated` + `NewMessage` payloads. `widgets::sweep_once` giveaway half: `for row in giveaways::list_due(conn, now)? { close_and_draw → push both PendingBroadcasts }`. **Persist-then-broadcast:** everything commits under the caller's lock before the sweeper's guard drops; crash after commit never redraws (`status='open'` guard).

- [ ] **Step 6: Handlers.** Five arms, same preamble as polls (load → `err("giveaway not found")`; visibility check with the same opaque-not-found rule):
  - **EnterGiveaway:** `widget_limiter.allow` → `require_not_timed_out` → `status == "open" && now < ends_at` else `err("this giveaway has ended")`/`("...was cancelled")` → `enter`; already-entered → plain `Ok` no event; else `ok_with(Ok, [GiveawayUpdated])`.
  - **LeaveGiveaway:** `widget_limiter.allow` → `require_not_timed_out` → `status == "open"` → `leave`; no-row → `Ok` no event; else `ok_with(Ok, [GiveawayUpdated])`.
  - **CancelGiveaway:** `require_not_timed_out` → creator OR MANAGE_SERVER else `err("only the creator or a moderator can cancel")` → `status == "open"` else `err("giveaway already ended")` → `cancel` → `ok_with(Ok, [GiveawayUpdated])`. No announcement.
  - **RerollGiveaway:** `require_not_timed_out` → creator-or-MANAGE_SERVER → `status == "ended" && winner.is_some()` else `err("can only reroll a finished giveaway with a winner")` → recompute eligible set (same filter as draw); empty → `err("no eligible entries to reroll")`, previous winner stands; else draw, `reroll` (winner update, still 'ended'), fresh announcement (`"🎉 Reroll — <display_name> won: <prize>"`, fresh throwaway key) → events `[GiveawayUpdated, NewMessage]`.
  - **GetGiveaway:** preamble only → `ok(Giveaway { giveaway, my_entered })`. No timeout gate.
  - **DeleteMessage hook:** widget parses to `{"type":"giveaway","id"}` and `status='open'` → cancel + `GiveawayUpdated` (cancelled) alongside `MessageDeleted`; ended/cancelled card delete changes nothing.

- [ ] **Step 7: Tests + gates + commit.** Per the spec test plan: dispatch — non-mod → MANAGE_SERVER error no rows; mod creates → giveaway row + card with correct widget JSON/fallback/invoker-author/no badge; bad args → usage error no rows. Handlers — enter/leave idempotence (no event on no-op); timed-out enter+leave denied; enter after ends_at / on cancelled → err; no VIEW_CHANNEL → denied; cancel rando/creator/mod matrix + double-cancel; reroll on open / no-winner / empty-eligible (winner unchanged); GetGiveaway `my_entered` per requester; DeleteMessage open → cancelled + both events + `list_due` excludes it; DeleteMessage ended → untouched, no event. Draw — no entries → ended + NULL winner + no-entries announcement; banned + revoked entrants never drawn (loop N times); winner ∈ entrants; announcement author matches NO member row, override "Giveaway", badge "BOT"; sweep pass twice → draws exactly once, zero new announcements. Gates: `cargo test -p farder-server`, `cargo build --workspace`, `cd client/src-tauri && cargo build`.
```bash
git add crates/farder-protocol/src/server.rs crates/farder-server/src/db.rs crates/farder-server/src/giveaways.rs crates/farder-server/src/lib.rs crates/farder-server/src/connection.rs crates/farder-server/src/handlers.rs crates/farder-server/src/widgets.rs
git commit -m "feat(giveaways): giveaway tables + module + protocol + dispatch + handlers + sweeper draw"
```

---

### Task 4: CLIENT PLUMBING — Tauri commands, bridge, types, events, reducer

**Files:** `client/src-tauri/src/commands.rs`; `client/src-tauri/src/main.rs`; `client/src-tauri/src/bridge.rs`; `client/src/lib/tauri-bridge.ts`; `client/src/lib/types.ts`; `client/src/hooks/useServerEvents.ts`; `client/src/context/ServerContext.tsx`.

- [ ] **Step 1: Tauri commands (9).** `client/src-tauri/src/commands.rs`, standard 3-arm response mapping (mirror existing command style):
  - `get_poll(server_id, poll_id) -> Result<PollState, String>` with `#[derive(Serialize)] struct PollState { poll: PollInfo, my_vote: Option<u32> }` ← `ServerResponse::Poll`.
  - `vote_poll(server_id, poll_id, option_index)`, `retract_vote(server_id, poll_id)`, `close_poll(server_id, poll_id)` ← `Ok`.
  - `get_giveaway(server_id, giveaway_id) -> Result<GiveawayState, String>` with `#[derive(Serialize)] struct GiveawayState { giveaway: GiveawayInfo, my_entered: bool }` ← `ServerResponse::Giveaway`.
  - `enter_giveaway`, `leave_giveaway`, `cancel_giveaway`, `reroll_giveaway` (each `(server_id, giveaway_id)`) ← `Ok`.
  ALL NINE registered in `generate_handler!` in `client/src-tauri/src/main.rs`. Audit: `grep -c 'get_poll\|vote_poll\|retract_vote\|close_poll\|get_giveaway\|enter_giveaway\|leave_giveaway\|cancel_giveaway\|reroll_giveaway' client/src-tauri/src/main.rs`.

- [ ] **Step 2: Event bridge.** `bridge.rs` `dispatch_event`: `ServerEvent::PollUpdated { poll } => emit("server:poll_updated", json!({ "server_id": sid, "poll": poll }))`; `ServerEvent::GiveawayUpdated { giveaway } => emit("server:giveaway_updated", json!({ "server_id": sid, "giveaway": giveaway }))`. PublicKey fields serialize to strings consistent with `MessageInfo.author` handling (follow the existing pk-to-string mapping in this file).

- [ ] **Step 3: Bridge fns + types.** `tauri-bridge.ts`: `getPoll(serverId, pollId)`, `votePoll(serverId, pollId, optionIndex)`, `retractVote(serverId, pollId)`, `closePoll(serverId, pollId)`, `getGiveaway(serverId, giveawayId)`, `enterGiveaway`, `leaveGiveaway`, `cancelGiveaway`, `rerollGiveaway` (camelCase invoke args, snake_case response types). `types.ts`: `MessageInfo.widget?: string | null`; `PollInfo` mirroring the protocol struct (creator same shape as `MessageInfo.author`); `GiveawayInfo` (`entry_count: number`, `winner: string | null`, `winner_name: string | null`); widen `cmdKind`-style unions to `"text" | "api" | "poll" | "giveaway"` where the kind union lives in types (BotsTab's local union is T7).

- [ ] **Step 4: Reducer + listeners.** `ServerContext.tsx` `PerServerState`: `polls: Record<number, { poll: PollInfo; myVote: number | null }>` and `giveaways: Record<number, { giveaway: GiveawayInfo; myEntered: boolean }>` (both init `{}`). Actions (immutable upsert, `ATTACHMENT_REDACTED` idiom):
  - `POLL_UPDATED { serverId, payload: PollInfo }` — upsert PRESERVING existing `myVote` (default `null`).
  - `POLL_STATE { serverId, payload: { poll, myVote } }` — from `getPoll`.
  - `POLL_MY_VOTE { serverId, payload: { pollId, myVote: number | null } }` — post-ack.
  - `GIVEAWAY_UPDATED { serverId, payload: GiveawayInfo }` — upsert PRESERVING `myEntered` (default `false`).
  - `GIVEAWAY_STATE { serverId, payload: { giveaway, myEntered } }`.
  - `GIVEAWAY_MY_ENTERED { serverId, payload: { giveawayId, myEntered: boolean } }`.
  `useServerEvents.ts`: `listen("server:poll_updated")` + `listen("server:giveaway_updated")` — drop if `serverId !== activeRef.current` (matching other message-adjacent events), else dispatch `POLL_UPDATED` / `GIVEAWAY_UPDATED`.

- [ ] **Step 5: Gates + commit.** `cd client/src-tauri && cargo build`; `cd client && npx tsc --noEmit`; the invoke-name ↔ `generate_handler!` grep audit (9/9).
```bash
git add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs client/src-tauri/src/bridge.rs client/src/lib/tauri-bridge.ts client/src/lib/types.ts client/src/hooks/useServerEvents.ts client/src/context/ServerContext.tsx
git commit -m "feat(widgets): client plumbing — 9 Tauri commands, events, reducer slices"
```
> Adjust the `useServerEvents.ts`/`ServerContext.tsx` paths to their real locations if they differ (grep for the files first); everything else is fixed.

---

### Task 5: POLL WIDGET UI — PollWidget.tsx + Message.tsx widget dispatch + themes

**Files:** `client/src/components/PollWidget.tsx` (new); `client/src/components/Message.tsx`; `client/src/themes/xp-luna-blue/theme.css`; `client/src/themes/discord-dark/theme.css`; `client/src/themes/hello-kitty/theme.css`.

- [ ] **Step 1: Message.tsx widget dispatch (build the GENERIC slot — T6 only adds a case).** Memoized `try/catch JSON.parse(message.widget)`; require `typeof parsed.id === "number"`. `switch (parsed.type)`: `"poll"` → `<PollWidget serverId={serverId} pollId={parsed.id} onUnavailable={...} />` rendered IN PLACE OF the `.message-content` text body (content is the old-client fallback), in the established slot beside `.link-embeds`; `default` → plain content as today. Parse failure or widget-unavailable callback → plain content. Reply/reactions/threads/context-menu untouched (normal member message).

- [ ] **Step 2: PollWidget.tsx.** Props `{ serverId: string; pollId: number; onUnavailable?: () => void }`. Reads `state.polls[pollId]`; absent → `api.getPoll` once on mount → dispatch `POLL_STATE`; fetch error → call `onUnavailable` (parent falls back to plain content). Render per spec: `.poll-widget` card (`.link-embed` family look) → `.poll-question` → option rows `<button class="poll-option">` with `.poll-option-bar` (width = percentage fill), `.poll-option-label`, `.poll-option-count` ("12 · 60%"); modifiers `.poll-option--mine`, `.poll-option--winner` (closed-state argmax, ALL tied winners; none when `total_votes === 0`). Footer `.poll-footer`: "{total} votes · closes in 2h 10m" (30 s interval countdown recomputed from `closes_at` while open+timed) / "{total} votes · final results". Close button = existing `.xp-button`, only while open AND (creator === me OR `hasPermission(MANAGE_SERVER)` via the existing `getActorPermissions` path); server re-checks regardless. Interactions: click option → `votePoll` then dispatch `POLL_MY_VOTE` (different option = re-vote); click MY OWN voted option → `retractVote` then `POLL_MY_VOTE null`; closed → rows `disabled`. Errors (lost race with close) → `.error-text` line inside the card; next `PollUpdated`/`getPoll` corrects.

- [ ] **Step 3: Theme CSS.** All 9 classes (`.poll-widget`, `.poll-question`, `.poll-option`, `.poll-option-bar`, `.poll-option-label`, `.poll-option-count`, `.poll-option--mine`, `.poll-option--winner`, `.poll-footer`) in ALL THREE theme.css files; colors only `var(--xp-…)` (`--xp-blue` fill bar, `--xp-panel-bg`/`--xp-border` card, `--xp-text-muted` counts).

- [ ] **Step 4: Gates + commit.** `cd client && npx tsc --noEmit`; `grep -l "poll-widget" client/src/themes/*/theme.css` → all three.
```bash
git add client/src/components/PollWidget.tsx client/src/components/Message.tsx client/src/themes/
git commit -m "feat(polls): PollWidget + Message widget slot + theme CSS"
```

---

### Task 6: GIVEAWAY WIDGET UI — GiveawayWidget.tsx + themes

**Files:** `client/src/components/GiveawayWidget.tsx` (new); `client/src/components/Message.tsx` (one added switch case only); the three `client/src/themes/*/theme.css`.

- [ ] **Step 1: Message.tsx.** Add the `"giveaway"` case to the T5 dispatch switch → `<GiveawayWidget serverId={serverId} giveawayId={parsed.id} onUnavailable={...} />`. Nothing else changes.

- [ ] **Step 2: GiveawayWidget.tsx.** Same skeleton as PollWidget (context read → `api.getGiveaway` on mount when absent → `GIVEAWAY_STATE`; failed fetch → `onUnavailable`). States per spec:
  - **Open:** 🎉 + `.giveaway-prize`; `.giveaway-meta` row = live countdown (1 s `setInterval` from `ends_at`, no server ticks) + `entry_count` entries; one toggle `.giveaway-enter-btn` **Enter ↔ Leave** driven by `myEntered` — on successful ack dispatch `GIVEAWAY_MY_ENTERED` (optimistic on the ack; broadcasts only refresh the count, they can never flip the button); on error the toggle stays put, error inline. **Cancel** link in `.giveaway-actions` when `ownPk === creator || canManageServer` (server re-checks).
  - **Ended:** `.giveaway-winner` — "🎉 Winner: {winner_name}" (fallback: short form of `winner` pk; "No entries." when winner null). **Reroll** link, same creator-or-mod visibility, only when `winner` set.
  - **Cancelled:** muted `.giveaway-cancelled` "Giveaway cancelled."
  - Errors inline via existing `.error-text`.

- [ ] **Step 3: Theme CSS.** All 7 classes (`.giveaway-widget`, `.giveaway-prize`, `.giveaway-meta`, `.giveaway-actions`, `.giveaway-enter-btn`, `.giveaway-winner`, `.giveaway-cancelled`) in ALL THREE theme files; card modeled on `.link-embed` + `.link-embed-chip`; `var(--xp-…)` only.

- [ ] **Step 4: Gates + commit.** `cd client && npx tsc --noEmit`; `grep -l "giveaway-widget" client/src/themes/*/theme.css` → all three.
```bash
git add client/src/components/GiveawayWidget.tsx client/src/components/Message.tsx client/src/themes/
git commit -m "feat(giveaways): GiveawayWidget + theme CSS"
```

---

### Task 7: CONFIG UI + DOCS — BotsTab kinds, autocomplete verify, documentation

**Files:** `client/src/components/BotsTab.tsx`; `client/src/components/MessageInput.tsx` (verify only — expect ZERO changes); `docs/modules/tauri-commands.md`; `docs/modules/tauri-bridge.md`; `docs/modules/frontend-context.md`; `docs/modules/` server module doc(s) for `polls.rs`/`giveaways.rs`/`widgets.rs` (use `_TEMPLATE.md`); `ARCHITECTURE.md`.

- [ ] **Step 1: BotsTab kind selector.** `cmdKind` union widened to `"text" | "api" | "poll" | "giveaway"`; `<option value="poll">Poll</option>` + `<option value="giveaway">Giveaway</option>`. Selecting either shows NO url/body/path/template/unit fields — only a muted per-kind hint line: poll → `Members run /<trigger> Question | option A | option B [| 30m|2h|1d]`; giveaway → `Usage: /<trigger> <duration> <prize> — e.g. /giveaway 24h Steam key (moderators only)`. Add button enabled on name+trigger+description alone for these kinds; `handleAddCommand` passes `null` for all kind-specific fields.

- [ ] **Step 2: Autocomplete verify.** Confirm `MessageInput.tsx` needs zero changes: the `/` menu lists poll/giveaway commands (from `listCommands` — server-side `takes_arg` already `true` for both since T2), inserts a trailing space, and the pipe-separated / space-separated args ride through `runCommand` unchanged (`/\s+/` split+rejoin only collapses whitespace, which both parsers trim). If anything DOES need changing, that is a T2 `takes_arg` bug — fix it there, not here.

- [ ] **Step 3: Docs.** `tauri-commands.md`: 9 new commands (name, params, return, matching `invoke` name). `tauri-bridge.md`: `server:poll_updated` + `server:giveaway_updated` (payload + `useServerEvents` listener). `frontend-context.md`: `polls`/`giveaways` slices + 6 actions. Module docs: `polls.rs`, `giveaways.rs`, `widgets.rs` (sweeper contract: 15 s tick, sweep_once sync body, persist-then-broadcast), plus `messages.rs` doc updated for `widget`/`set_widget` and `protocol.md` for the new variants. `ARCHITECTURE.md`: widget data path (`RunCommand` kind → card + feature row + widget JSON → interaction requests → `*Updated` events → sweeper) and the widgets/polls/giveaways modules in the module list.

- [ ] **Step 4: Gates + commit.** `cd client && npx tsc --noEmit`.
```bash
git add client/src/components/BotsTab.tsx docs/modules/ ARCHITECTURE.md
git commit -m "feat(widgets): BotsTab poll/giveaway kinds + module docs"
```

---

## Security checklist (verify before calling the feature done)

- [ ] All authorization server-side against the authenticated connection key; no request carries a voter/creator/entrant identity field — ids only.
- [ ] All 9 interaction variants membership-gated by default-deny `request_requires_membership` (NOT added to the bootstrap allow-list) — mesh log-mode gating automatic.
- [ ] Visibility failures on GetPoll/VotePoll/…/GetGiveaway return opaque `"... not found"` — widget ids are not an existence oracle for invisible channels.
- [ ] Giveaway creation MANAGE_SERVER-gated at dispatch (BotsTab gating is cosmetic); poll creation inherits every RunCommand gate (SEND_MESSAGES via `check_run_command_channel_auth`).
- [ ] `require_not_timed_out` on every mutating request (vote/retract/close/enter/leave/cancel/reroll); reads exempt.
- [ ] Fan-out bounded: shared `widget_limiter` 10/10 s on vote/enter/leave; creation bounded by the existing `command_limiter` 5/10 s.
- [ ] `messages.widget` JSON is server-written only; client parses it as untrusted (try/catch, numeric id check).
- [ ] Per-member data self-only: `my_vote`/`my_entered` only in the requester's own read response; broadcasts carry counts/status/winner only; entrant/voter identities never leave the server in v1.
- [ ] Draw is server-side `rand::thread_rng()` (OS-seeded), persist-before-broadcast under `status='open'` guard — unpredictable, never double-drawn; banned/revoked filtered at draw time.
- [ ] Announcement author = fresh throwaway non-member key (secret discarded, can never authenticate); never a real member's identity under a BOT badge.
- [ ] No DB MutexGuard across `.await` anywhere (dispatch arms, handlers, sweeper).

## Owner runtime verification (server changed → sidecar rebuild; two clients ideal)

Run BOTH specs' "Owner runtime verification" sections in full (poll spec steps 1-7; giveaway spec steps 1-8). Highlights: live vote counts on two screens; sweeper close within ~15 s; creator-vs-mod Close/Cancel/Reroll gating; mid-poll client restart re-hydrates via GetPoll/GetGiveaway; card delete closes the poll / cancels the giveaway (no draw, no announcement); server restart past a due giveaway draws exactly once.

## Self-review notes

- Substrate built once (T1) exactly per the shared contract both specs restate; T2/T3 only consume it (`sweep_once` gains one half each).
- Poll spec decomposition items 1-2 → T1+T2; giveaway items 1-3 → T1+T3; both client-plumbing items → T4; widget UIs → T5/T6; config+docs → T7.
- Giveaway DECISION 3 honored: `entry_count` broadcast, never an entrant list (T3 Step 3).
- Winner-DM explicitly skipped (spec decision, carry-forward); voter-list/entrant-list UIs deferred per specs.
- MSG_SELECT tail verified against source: `author_badge` at index 9 (messages.rs:12-13) → `widget` at 10.
- Known drift risks called out in-task: RateLimiter method naming (T2 Step 4), handler fixture names (T2/T3), hook/context file paths (T4 Step 5) — build agents match real code, plan gives the contract.
