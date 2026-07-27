# /poll — interactive poll command (v1) — design

**Date:** 2026-07-27
**Status:** design (awaiting owner review)
**Context:** first **interactive** command kind on the slash-command framework (see [[2026-07-04-slash-commands-design]], shipped: kinds `text` + `api`). A member types `/<trigger> Question | A | B [| 1h]`, a poll card posts in the channel, everyone votes on it live, and it closes on a timer or by hand. Built against the **shared widget substrate contract** with the sibling `/giveaway` spec (written in parallel): widget messages, per-feature tables, RunCommand kind dispatch, per-feature interaction requests, `PollUpdated`-style live events, and **one** shared `widgets::spawn_widget_sweeper` task that services both features.

## Problem

Commands can only *answer* (canned text, api lookup). There is no message a member can *interact with*. Polls are the canonical first interactive widget: a message whose state (votes) changes after posting and must update live on every subscriber's screen, survive reconnects, and close deterministically.

## Shared substrate (cross-reference)

Sibling spec: [[undefined-giveaway-command-design]] (`/giveaway`), written in parallel against the same contract. Shared pieces, built ONCE and identical in both specs: the `messages.widget` TEXT column + `MessageInfo.widget: Option<String>` (`#[serde(default)]`, `MSG_SELECT` index 10) + `messages::set_widget` helper (insert-then-set-widget idiom); the single `widgets::spawn_widget_sweeper` task (`widgets.rs`, `WIDGET_SWEEP_SECS = 15`, sync `widgets::sweep_once(&conn, now) -> Vec<PendingBroadcast>` tick body servicing BOTH halves — `polls::close_due` and `giveaways::list_due`/`close_and_draw`); the shared `widget_limiter` (`RateLimiter::new(10, 10)` on `ServerState`); the `Message.tsx` widget parse/render slot; the BotsTab kind selector widened to `"text" | "api" | "poll" | "giveaway"` and `takes_arg = matches!(kind, "api" | "poll" | "giveaway")`. All timestamps are **unix seconds** via `db::now()` (same unit as `messages.timestamp`). Whichever plan lands first creates the shared pieces; the second wires into them.

## What already exists (reused)

- **Slash-command framework:** `commands` table + `commands.rs` CRUD (`crates/farder-server/src/commands.rs`), `RunCommand` dispatch at the connection level (`connection.rs:935+`) with content-block gate → `command_limiter` (5/10 s) → `check_run_command_channel_auth` (timeout + DM-participant/blocked + `SEND_MESSAGES`) → trigger lookup → kind match. The `poll` kind slots into that kind match; **every gate before it is reused unchanged** — which is exactly the contract's poll-creation permission (any member with SEND_MESSAGES in that channel).
- **Message plumbing:** `messages::insert_message` (plain member authorship — per the substrate contract the poll card is authored by the **invoking user**, `author_name_override`/`author_badge` stay NULL), `MSG_SELECT` append-last column discipline (`messages.rs:12`, `widget` becomes index 10), `broadcast_event` + `EventTarget::Subscribers(channel_id)`.
- **Sweeper template:** `bots::spawn_bot_poll_task` (`bots.rs:364`) — snapshot under a scoped `state.db` lock, drop the guard **before** any `.await`, poll-then-sleep. `widgets::spawn_widget_sweeper` copies this discipline.
- **Guarded migrations:** `db::init_schema` PRAGMA-table_info ALTER idiom (db.rs:317-324) + `CREATE TABLE IF NOT EXISTS` for new tables (idempotency covered by `test_schema_init_idempotent`).
- **Gates:** `content_block_reason` via the default-deny `request_requires_membership` (handlers.rs:371 — new request variants are membership-gated *automatically*), `require_not_timed_out`, `resolve_member_perms_pub`, `channels::is_dm_participant`, `require_base_perm(MANAGE_SERVER)`, `RateLimiter`.
- **Client:** `MessageInput` `/` autocomplete + `runCommand` send path (works for polls with zero changes — `takes_arg` just needs to include the new kind), `BotsTab` Add Command form, `Message.tsx` render slots, `useServerEvents` → reducer event flow, `.link-embed`-family card styling.

## Goals

1. Owner adds a command of kind **poll** (trigger of their choice, e.g. `/poll`) in BotsTab; no other config.
2. Any member with SEND_MESSAGES runs `/<trigger> Question | option A | option B [| ...] [| 1h]` (2–10 options, optional duration) → a poll card posts as **their** message.
3. Card shows question, clickable option rows, **live** counts + percentages + total, time remaining if timed; all subscribers see votes land in real time.
4. One vote per member; re-vote replaces; retractable; all until close.
5. No duration → open until closed manually; close-early by creator or MANAGE_SERVER; timed polls closed by the shared sweeper. Closed card shows final results with the winning option(s) highlighted.
6. Reconnects/late joiners recover full poll state via a read request (`GetPoll`) — **history wire format is not extended with poll state** (substrate contract §5: read-request option chosen).

## Non-goals (v1)

- **Voter-list UI.** Votes are stored by public key server-side (deliberately not anonymous), but v1 UI shows only counts/percentages. "Who voted for what" is an explicit future feature, not an oversight.
- **Anonymous-mode polls**, **multi-select** (vote for N options), **editing** a poll's question/options after creation.
- **Per-command permissions** on the poll command (framework carry-forward applies unchanged).
- **Notifications** on poll close; **pinning/summary reposts**.
- Embedding widget state in `FetchHistory` responses (rejected per contract §5 to avoid history-format churn).

## Design

### Invocation parse rules (exact)

Args string (everything after the trigger, as sent by the existing `MessageInput` path) is split on `|`; each segment is trimmed.

1. **Duration detection is deterministic, not heuristic:** if the *final* trimmed segment matches `^(\d{1,4})(m|h|d)$` (case-insensitive), it is **always** consumed as the duration — never as an option. `30m`/`2h`/`7d` → minutes/hours/days. Range after conversion: **1 minute – 30 days**, else `Error("duration must be between 1m and 30d")`.
2. Remaining segments: first = **question** (1–256 chars), rest = **options** (each 1–100 chars, 2–10 of them). Case-insensitive duplicate options rejected. Empty segments rejected.
3. Any violation → `ServerResponse::Error { reason: "usage: /<trigger> Question | option A | option B [| 30m|2h|1d]" }` (specific reason where known, e.g. "a poll needs 2-10 options"). Error goes to the invoker only; **nothing posts**.

Consequence stated openly: an option that is *literally* a bare duration token can't be expressed (`/poll Best? | 1h | 2h` parses `2h` as duration, leaves one option → usage error). Workaround: phrase it (`1 hour`). This determinism is the "explicit syntax" — no guessing between option and duration.

Implemented as pure `polls::parse_poll_args(args: &str) -> Result<ParsedPoll, String>` where `ParsedPoll { question: String, options: Vec<String>, duration_secs: Option<u64> }` — unit-tested without any DB.

### Data model

Guarded migrations in `db::init_schema` (existing idioms; timestamps are **unix seconds** via `db::now()`, same unit as `messages.timestamp`):

- `messages` gains nullable `widget TEXT` via the PRAGMA-table_info ALTER idiom (shared with `/giveaway` — whichever lands first adds it; the guard makes the second a no-op). Holds JSON exactly `{"type":"poll","id":<poll_id>}`. `MSG_SELECT` appends `, widget` **last** (index 10); `row_to_message_info` reads it; `MessageInfo` gains `#[serde(default)] pub widget: Option<String>`.
- New tables (after the `commands` table block):

```sql
CREATE TABLE IF NOT EXISTS polls (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id  INTEGER NOT NULL,
    message_id  INTEGER NOT NULL,
    creator     BLOB    NOT NULL,          -- invoking member's public key (32 bytes)
    question    TEXT    NOT NULL,
    options     TEXT    NOT NULL,          -- JSON array of option strings (2-10)
    created_at  INTEGER NOT NULL,          -- unix secs
    closes_at   INTEGER,                   -- unix secs; NULL = manual close only
    closed_at   INTEGER                    -- unix secs; NULL = open
);

CREATE TABLE IF NOT EXISTS poll_votes (
    poll_id      INTEGER NOT NULL,
    voter        BLOB    NOT NULL,         -- member public key
    option_index INTEGER NOT NULL,
    voted_at     INTEGER NOT NULL,
    PRIMARY KEY (poll_id, voter)           -- one vote per member; upsert = re-vote
);
```

Options as a JSON column (not a third table): options are immutable after creation and always read whole; counts come from `SELECT option_index, COUNT(*) FROM poll_votes WHERE poll_id=?1 GROUP BY option_index`. No shared generic "widgets" table (contract §2).

New module `crates/farder-server/src/polls.rs` (mirrors `commands.rs`): `parse_poll_args`, `create(conn, channel_id, message_id, creator, question, &options, closes_at) -> Result<i64>`, `get(conn, id) -> Result<Option<PollRow>>`, `build_info(conn, &PollRow) -> Result<PollInfo>` (computes counts/total/`closed`), `vote(conn, poll_id, voter, option_index)` (INSERT … ON CONFLICT(poll_id, voter) DO UPDATE), `retract(conn, poll_id, voter) -> Result<bool>` (rows-affected), `close(conn, poll_id, now)`, `close_due(conn, now) -> Result<Vec<PollInfo>>` (see sweeper), `my_vote(conn, poll_id, voter) -> Result<Option<u32>>`.

### Protocol (crates/farder-protocol/src/server.rs)

```rust
pub struct PollInfo {
    pub id: i64,
    pub channel_id: u64,
    pub message_id: u64,
    pub creator: PublicKey,
    pub question: String,
    pub options: Vec<String>,
    pub counts: Vec<u32>,      // aligned with options
    pub total_votes: u32,
    pub created_at: u64,       // unix secs
    pub closes_at: Option<u64>,
    pub closed: bool,
}
```

- `ServerRequest::GetPoll { poll_id: i64 }` → `ServerResponse::Poll { poll: PollInfo, my_vote: Option<u32> }` (`my_vote` is requester-specific, so it lives only in this response — the broadcast event below carries shared state only).
- `ServerRequest::VotePoll { poll_id: i64, option_index: u32 }` → `Ok`.
- `ServerRequest::RetractVote { poll_id: i64 }` → `Ok`.
- `ServerRequest::ClosePoll { poll_id: i64 }` → `Ok`.
- `ServerEvent::PollUpdated { poll: PollInfo }` → `EventTarget::Subscribers(channel_id)`. Terminal close **folds into the same shape** with `closed: true` (contract §5) — there is no separate PollClosed event.

The **actor is always the authenticated connection key** — no request carries a voter/creator field (contract §4). Winner is **not** a server field: the client derives winner(s) = argmax(counts) when `closed && total_votes > 0` (ties highlight all tied options).

Compat notes: `MessageInfo.widget` uses `#[serde(default)]` (old rows/frames decode fine). New enum variants require rebuilding everything: `cargo build --workspace` **plus** `cd client/src-tauri && cargo build` — the client crate is not a workspace member (the MemberApproved-class regression; contract §10). As with every prior ServerEvent addition, an old client binary cannot decode frames containing the new variant — client+server ship together, unchanged practice.

### Creation — `poll` kind in RunCommand dispatch (connection.rs)

`AddCommand` validation (handlers.rs) accepts `kind: "poll"` with **no** extra required fields (`body_text`/`url_template`/etc. stay NULL; the sibling spec adds `"giveaway"` to the same arm). `commands::list_infos` changes `takes_arg` to `matches!(kind, "api" | "poll" | "giveaway")` (identical expression in the giveaway spec — new kinds opt in explicitly) so autocomplete inserts the trailing space. The framework still generates a per-command `public_key`; the poll kind never uses it (invoker authorship) — harmless, noted.

The existing RunCommand arm's gates run first, unchanged: content-block → `command_limiter` → `check_run_command_channel_auth` (this IS the SEND_MESSAGES creation gate, DM-aware) → trigger lookup. Then in the kind match:

`"poll"` branch:
1. `polls::parse_poll_args(&args)` — `Err(reason)` → `Error { reason }` to the invoker, no post. (Pure, no lock held.)
2. One scoped `state.db.lock()` block, wrapped in a SQLite transaction (three writes must be atomic), guard dropped before any await:
   - `content` fallback = `"📊 Poll: {question}\n"` + options as `"- {option}"` lines (old clients see the whole poll read-only; new clients render the widget instead).
   - `mid = messages::insert_message(&conn, channel_id, &member_key, &content, None)` — **plain invoker authorship**, no name override, no badge (contract §7).
   - `poll_id = polls::create(&conn, channel_id, mid, &member_key, question, &options, duration_secs.map(|d| now + d))`.
   - `UPDATE messages SET widget = '{"type":"poll","id":<poll_id>}' WHERE id = mid` (small `messages::set_widget` helper; insert-then-update resolves the message-id↔poll-id circularity without touching `insert_message` signatures).
   - `msg = get_message(...)`; `info = polls::build_info(...)`.
3. After guard drop: `broadcast_event(Subscribers(channel_id), NewMessage { message: msg }).await`, then `broadcast_event(Subscribers(channel_id), PollUpdated { poll: info }).await` (pre-seeds connected clients' reducers so the widget mounts with state, no GetPoll round-trip), then `ServerResponse::Ok`.

### Interaction handlers (handlers.rs, sync arms in `handle_request`)

All four new variants are membership-gated automatically by default-deny `request_requires_membership` (they are simply not added to the bootstrap allow-list) — the mesh log-mode gate from contract §4 costs zero new code. Shared per-request authz sequence:

**`GetPoll`** (read): load poll (`None` → `err("poll not found")`); **visibility check**: `channels::get_channel` → if `ChannelType::Dm` require `is_dm_participant` else `resolve_member_perms_pub` + `has(VIEW_CHANNEL)`. **Any visibility failure** (channel gone, not a DM participant, missing VIEW_CHANNEL) returns the same `err("poll not found")` — not a permission error — so poll ids are not an existence oracle for channels the caller can't see (same rule in the giveaway spec); → `ok(Poll { poll: build_info, my_vote })`. No timeout gate (reads are allowed while timed out).

**`VotePoll`**: `state.widget_limiter.allow(caller)` (new `RateLimiter::new(10, 10)` on `ServerState` — votes are cheap but each fans out a broadcast; separate from `command_limiter` so voting doesn't eat command budget) → `require_not_timed_out` → load poll → **closed check**: `closed_at.is_some() || closes_at.map_or(false, |t| now >= t)` → `err("poll is closed")` (the `closes_at` half makes closing exact even if the sweeper hasn't ticked yet) → visibility check as GetPoll → `option_index < options.len()` else `err("invalid option")` → `polls::vote` (upsert = re-vote replaces) → `ok_with(Ok, vec![BroadcastEvent { target: Subscribers(channel_id), event: PollUpdated { poll: build_info } }])`.

**`RetractVote`**: same gates as VotePoll minus the index check → `polls::retract`; if no row was deleted, return `Ok` with **no** event (idempotent, no broadcast noise); else `ok_with(Ok, [PollUpdated])`.

**`ClosePoll`**: `require_not_timed_out` → load poll → already closed → `err("poll already closed")` → **authz: creator OR MANAGE_SERVER** — `if poll.creator != *member { require_base_perm(conn, member, is_owner, MANAGE_SERVER, "MANAGE_SERVER")? }` (contract §8) → `polls::close(conn, id, now)` → `ok_with(Ok, [PollUpdated])` with `closed: true`.

**`DeleteMessage` hook**: messages are hard-deleted (`messages::delete_message` removes the row). After the existing authz + before returning, if the loaded `msg.widget` parses to `{"type":"poll","id"}` and that poll is open → `polls::close(conn, id, now)` and push a `PollUpdated` (closed) event alongside `MessageDeleted`. Poll + vote rows are retained (audit/history); the card is gone so the poll simply ends. No zombie open polls.

### Sweeper — `widgets::spawn_widget_sweeper` (shared with /giveaway)

New module `crates/farder-server/src/widgets.rs`; **one** task services both features (contract §6). Spawned in `main.rs` beside the bot poller: `let _widget_sweeper = farder_server::widgets::spawn_widget_sweeper(Arc::clone(&state));`

```rust
pub const WIDGET_SWEEP_SECS: u64 = 15;
pub fn spawn_widget_sweeper(state: Arc<ServerState>) -> tokio::task::JoinHandle<()>
```

Loop body (bots.rs lock discipline, verbatim pattern):
1. Scoped lock block: `let pending: Vec<PendingBroadcast> = { let conn = state.db.lock().unwrap(); widgets::sweep_once(&conn, db::now()) }` — `sweep_once` is the sync tick body servicing **both** halves (extracted so tests run it without tokio; `PendingBroadcast` = target + event). Poll half: `polls::close_due` selects `WHERE closed_at IS NULL AND closes_at IS NOT NULL AND closes_at <= ?now`, sets `closed_at = ?now` for each, and returns the built `PollInfo`s, which `sweep_once` wraps as `PollUpdated` → `Subscribers(channel_id)` broadcasts. **State is persisted before the guard drops, therefore before any broadcast** — a crash between persist and broadcast just means clients learn `closed: true` from their next `GetPoll`/reconnect; nothing re-opens or re-fires. (Giveaway half: `giveaways::list_due` + `close_and_draw` — that behavior, including the `rand`-based winner draw and the announcement message, is specified in the sibling spec; `rand 0.8` is already a farder-server dependency so no new crate either way.)
2. Guard dropped (`// MutexGuard dropped here`), then `for pb in pending { broadcast_event(&state, pb.target, pb.event).await; }`
3. `tokio::time::sleep(Duration::from_secs(WIDGET_SWEEP_SECS)).await` (fixed tick; no owner-tunable interval — 15 s lag on a poll close is imperceptible, and the VotePoll closed-check is exact regardless).

No DB mutex is ever held across an await anywhere in this feature (contract §10).

### Client

**Types (`types.ts`):** `MessageInfo` gains `widget?: string | null`; new `PollInfo` mirroring the protocol struct (`creator: { bytes: number[] }`, same shape as `MessageInfo.author`).

**Tauri commands (`client/src-tauri/src/commands.rs`)** — standard 3-arm response mapping, all four registered in `generate_handler!` in `main.rs` (the untyped seam — CLAUDE.md checklist applies, plus `docs/modules/tauri-commands.md` entries):
- `get_poll(server_id, poll_id) -> Result<PollState, String>` where `#[derive(Serialize)] struct PollState { poll: PollInfo, my_vote: Option<u32> }` ← `ServerResponse::Poll`.
- `vote_poll(server_id, poll_id, option_index)`, `retract_vote(server_id, poll_id)`, `close_poll(server_id, poll_id)` ← `Ok`.

**Bridge (`tauri-bridge.ts`):** `getPoll(serverId, pollId)`, `votePoll(serverId, pollId, optionIndex)`, `retractVote(serverId, pollId)`, `closePoll(serverId, pollId)`.

**Event (`bridge.rs`):** `ServerEvent::PollUpdated { poll }` → `app.emit("server:poll_updated", json!({ "server_id": sid, "poll": poll }))` (+ `docs/modules/tauri-bridge.md` entry).

**Reducer (`ServerContext.tsx`):** `PerServerState` gains `polls: Record<number, { poll: PollInfo; myVote: number | null }>` (keyed by poll id; per-server, so ids from different servers never collide). Actions (naming convention: SCREAMING_SNAKE + serverId + payload):
- `POLL_UPDATED { serverId, payload: PollInfo }` — upsert, **preserving** existing `myVote` (broadcast events don't carry it), default `null`.
- `POLL_STATE { serverId, payload: { poll: PollInfo; myVote: number | null } }` — from `getPoll`.
- `POLL_MY_VOTE { serverId, payload: { pollId: number; myVote: number | null } }` — dispatched by the widget after a successful `votePoll` (index) / `retractVote` (null).

**Listener (`useServerEvents.ts`):** `listen("server:poll_updated")` → drop if `serverId !== activeRef.current` (matching other message-adjacent events; background-server widgets refetch via `getPoll` on next mount) → `dispatch POLL_UPDATED`.

**`PollWidget.tsx`** (new, `client/src/components/`), props `{ serverId: string; pollId: number }`:
- Reads `state.polls[pollId]`; if absent, calls `api.getPoll` once on mount → `POLL_STATE`; on error (deleted/unknown poll) renders nothing and signals the parent to fall back to plain content.
- Card: `.poll-widget` (styled like the `.link-embed` card family) → `.poll-question` → option rows: `<button class="poll-option">` each containing `.poll-option-bar` (background fill, width = percentage), `.poll-option-label`, `.poll-option-count` ("12 · 60%"); modifier classes `.poll-option--mine` (my current vote) and `.poll-option--winner` (closed-state argmax highlight, all tied winners). Footer `.poll-footer`: "{total} votes · closes in 2h 10m" / "{total} votes · final results"; countdown re-derived from `closes_at` on a 30 s interval while open+timed. Close button = existing `.xp-button`, rendered only while open and (creator is me OR `hasPermission(MANAGE_SERVER)` via the existing `getActorPermissions` path).
- Interactions: click an option → `votePoll` (re-click a *different* option = re-vote); click **my own** voted option → `retractVote`; closed poll → rows inert (`disabled`). Errors (e.g. lost race with close) surface as a small `.error-text` line inside the card; the following `PollUpdated`/`getPoll` refresh corrects the display.
- **Theme rule (CLAUDE.md):** every new class (`.poll-widget`, `.poll-question`, `.poll-option`, `.poll-option-bar`, `.poll-option-label`, `.poll-option-count`, `.poll-option--mine`, `.poll-option--winner`, `.poll-footer`) added to **all three** `client/src/themes/*/theme.css`, colors only via `var(--xp-…)` (`--xp-blue` for the fill bar, `--xp-panel-bg`/`--xp-border` for the card, `--xp-text-muted` for counts).

**`Message.tsx`:** memoized try/catch `JSON.parse(message.widget)`; when `{type:"poll"}` → render `<PollWidget/>` **in place of** the `.message-content` text body (the content string is the old-client fallback; new clients hide it), in the established widget slot after `.message-content` / beside `.link-embeds`. Parse failure or PollWidget fallback signal → plain content renders as today. Reply/reactions/threads/context-menu on the card work untouched (it's a normal member message).

**`BotsTab.tsx`:** kind `<select>` gains `poll` (`cmdKind` type widens to `"text" | "api" | "poll" | "giveaway"` — the shared selector; whichever plan lands second adds only its `<option>`); selecting it shows no extra fields, just a muted hint line: `Members run /<trigger> Question | option A | option B [| 30m|2h|1d]`. Add button enabled on name+trigger+description alone; `handleAddCommand` passes `null` for all kind-specific fields. **`MessageInput.tsx`: zero changes** — the existing match-trigger → `runCommand(args)` path carries the pipe-separated string as-is (its `/\s+/` split+rejoin only collapses whitespace runs, which the parser trims anyway).

### Edge cases

- **Vote/close race with sweeper or manual close:** the closed check inside VotePoll (`closed_at` OR past `closes_at`, evaluated under the same DB lock) makes it exact; the loser gets `Error("poll is closed")` and the UI self-corrects on the next event.
- **Poll card message deleted:** DeleteMessage hook closes the poll + broadcasts; rows retained; widget disappears with the message.
- **Member leaves / is kicked / banned after voting:** their vote **stands** (counts don't recount — a snapshot of who voted while a member). They can't vote further: the membership default-deny (mesh) / visibility check (legacy) rejects them.
- **Timed-out member:** can read (GetPoll) but not vote/retract/close (`require_not_timed_out`), matching message-send semantics.
- **Mesh log-mode:** all four requests membership-gated by default-deny; creation gated by the existing RunCommand content-block. Pending-approval joiners get the standard "pending approval" reason.
- **DM polls:** creation already allowed by `check_run_command_channel_auth`; interaction visibility uses `is_dm_participant` for DM channels, so both parties (only) can vote.
- **Bare-duration option** (`| 1h` as a genuine option): unrepresentable by design; usage error tells the user; workaround is rephrasing.
- **Zero-vote close:** `total_votes == 0` → no winner highlight, footer reads "0 votes · final results".
- **Reconnect/late joiner:** history delivers the card with its `widget` JSON; PollWidget's `getPoll` fetch restores counts + `my_vote`. No reliance on having seen any event.

### Security

- All authorization is server-side against the connection key; requests carry only ids. Creation inherits every RunCommand gate; interactions add the visibility/timeout/creator-or-MANAGE_SERVER checks above.
- `widget` JSON is server-written only (never client-supplied); the client treats it as untrusted anyway (try/catch parse, id must be a number).
- Vote fan-out bounded by `widget_limiter` (10/10 s per user); creation bounded by the existing `command_limiter`.
- Counts are public by design; per-voter data never leaves the server in v1 (no request returns another member's vote — `my_vote` is self-only).

## Testing

- **`parse_poll_args` (unit, pure):** happy path 2 and 10 options; trimming; duration forms `30m`/`2H`/`7d` + bounds (reject `0m`, `31d`); determinism case `q | 1h | 2h` → usage error; 1 option / 11 options; duplicate options (case-insensitive); empty segments; over-length question/option; no duration → `duration_secs: None`.
- **polls module (unit, in-memory conn):** create + `build_info` counts; `vote` upsert replaces (counts move between indices, total stable); `retract` returns false on no-vote; `my_vote`; `close_due` closes only due timed polls (untimed and already-closed untouched) and returns them with `closed: true`, and rows are persisted closed even if the return value is dropped (crash-safety assertion).
- **Handlers (handlers.rs `mod tests` fixtures — `setup()`/`add_member`/`make_channel`/`fake_state`):** VotePoll happy path emits `PollUpdated` to `Subscribers`; vote on closed → err; past-`closes_at`-but-unswept → err; bad index → err; member without VIEW_CHANNEL → err; timed-out member → err; RetractVote idempotent (no event when no vote); ClosePoll by non-creator non-mod → "missing MANAGE_SERVER permission", by creator → closed, by MANAGE_SERVER holder → closed, double-close → err; GetPoll returns correct `my_vote` per requester; DeleteMessage on a poll card closes the poll and emits both events; AddCommand accepts kind `poll` with no extra fields; `list_infos` reports `takes_arg: true` for it. RunCommand poll-kind creation: message row (invoker author, NULL badge/override) + poll row + widget JSON all present and cross-linked; parse failure posts nothing.
- **Schema:** existing `test_schema_init_idempotent` covers the new DDL by construction.
- **Builds:** `cargo test --workspace`; `cd client/src-tauri && cargo build` (protocol change — the non-workspace client crate gotcha); `cd client && npx tsc --noEmit`; `grep -l "poll-widget" client/src/themes/*/theme.css` lists all three.
- Sweeper timing/broadcast and full UI are runtime-verified (below), not unit-tested.

## Owner runtime verification (server changed → sidecar rebuild)

1. Bots → Add Command: kind **poll**, trigger `poll`. Typing `/` shows it in autocomplete.
2. `/poll Best pizza? | Margherita | Pepperoni | 2m` → a poll card posts **as you** (no BOT badge), `/poll…` itself doesn't appear.
3. From a second account: vote — both screens update counts/percentages live without refresh. Change your vote (counts move), then click your own option (vote retracts).
4. Wait ~2 minutes: the sweeper closes it (within ~15 s of due); card shows final results with the winner highlighted; option rows go inert; voting from either account errors "poll is closed".
5. `/poll Untimed? | yes | no` → footer shows no countdown; second account has **no** Close button; your (creator) Close button closes it instantly on both screens.
6. `/poll onlyone | a` → usage error shown to you only, nothing posts. Restart the client mid-poll → card restores full state (counts + your vote).
7. Delete a poll card message → poll disappears and is closed (a pre-delete `GetPoll`-driven widget elsewhere would show closed).

## Decomposition (for the plan)

1. **Server: schema + polls module + protocol.** `widget` column + `MSG_SELECT`/`row_to_message_info`/`MessageInfo.widget`; `polls`/`poll_votes` DDL; `polls.rs` (parse/create/get/build_info/vote/retract/close/close_due/my_vote, unit-tested); `PollInfo` + 4 request variants + `Poll` response + `PollUpdated` event; `widget_limiter` on ServerState.
2. **Server: dispatch + handlers + sweeper.** `poll` kind in AddCommand validation + `takes_arg` + the RunCommand branch (insert/create/set_widget/broadcast); the four handler arms + DeleteMessage hook; `widgets.rs` `spawn_widget_sweeper` + main.rs spawn (coordinate with the /giveaway plan — the module and spawn line are shared, first lander creates them).
3. **Client: run path.** types (`PollInfo`, `MessageInfo.widget`) + 4 Tauri commands + `generate_handler!` registration + bridge fns + `server:poll_updated` emit/listener + `polls` state slice + 3 reducer actions.
4. **Client: UI.** `PollWidget.tsx` + `Message.tsx` widget slot/fallback + BotsTab `poll` kind option + the 9 new classes in all 3 theme.css files.
5. **Docs.** `tauri-commands.md`, `tauri-bridge.md`, `frontend-context.md`, module doc for `polls.rs`/`widgets.rs`, ARCHITECTURE.md if the module list is enumerated there.

## Carry-forward / known limitations

- **Voter-list UI:** the data (per-pk votes) already exists; a future `GetPollVoters` (creator/mod-gated or public — product call) + a hover/expand UI drops in without schema changes.
- **Multi-select, anonymous mode, editing:** all additive (`polls.multi INTEGER`, `polls.anonymous INTEGER`, edit request + `PollUpdated`); none require reshaping v1 rows.
- Bare-duration-literal options unrepresentable (documented above).
- Background-server widgets go stale until remount (`getPoll` on mount refreshes) — same staleness class as background-server messages today.
- The per-command permission carry-forward from the framework spec applies to the poll kind unchanged (e.g. restrict poll creation to a role later).
- Old client binaries can't decode frames containing `PollUpdated` (no `#[serde(other)]` — existing project-wide property of every ServerEvent addition).
