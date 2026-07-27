# /giveaway — timed entries + winner draw (interactive command kind) — design

**Date:** 2026-07-27
**Status:** design (awaiting owner review)
**Context:** follow-on sub-project of the slash-command framework (see [[2026-07-04-slash-commands-design]]), which shipped kinds `text` and `api` and explicitly reserved the interactive kinds. This adds kind **`giveaway`**: `/<trigger> <duration> <prize>` posts a live card with an Enter/Leave button and a countdown; at the deadline the server draws one winner uniformly, updates the card, and announces. A sibling spec (`/poll`) is being written in parallel against the same **shared substrate contract** — widget messages, per-feature tables, one shared sweeper (`widgets::spawn_widget_sweeper`), read-request state recovery. Where this spec says "shared substrate", the poll spec uses the identical mechanism; the implementation builds each piece once.

## Problem

Commands so far are fire-and-forget: the bot posts a static or fetched answer and is done. A giveaway is a **stateful, interactive** message: it accumulates entries over time, every viewer sees the live entry count, and the server itself must act later (draw at the deadline) with no client online. This needs: a message that renders as a widget bound to server-side state, interaction requests, live update events, and a background sweeper — the substrate the framework spec promised for interactive kinds.

## Shared substrate (cross-reference)

Sibling spec: [[undefined-poll-command-design]] (`/poll`), written in parallel against the same contract. Shared pieces, built ONCE and identical in both specs: the `messages.widget` TEXT column + `MessageInfo.widget: Option<String>` (`#[serde(default)]`, `MSG_SELECT` index 10) + `messages::set_widget` helper (insert-then-set-widget idiom); the single `widgets::spawn_widget_sweeper` task (`widgets.rs`, `WIDGET_SWEEP_SECS = 15`, sync `widgets::sweep_once(&conn, now) -> Vec<PendingBroadcast>` tick body servicing BOTH halves — `polls::close_due` and `giveaways::list_due`/`close_and_draw`); the shared `widget_limiter` (`RateLimiter::new(10, 10)` on `ServerState`); the `Message.tsx` widget parse/render slot; the BotsTab kind selector widened to `"text" | "api" | "poll" | "giveaway"` and `takes_arg = matches!(kind, "api" | "poll" | "giveaway")`. All timestamps are **unix seconds** via `db::now()` (same unit as `messages.timestamp`). Whichever plan lands first creates the shared pieces; the second wires into them.

## What already exists (reused)

- **Command registry + dispatch:** `commands` table (`kind TEXT`), `find_by_trigger`, and the connection-level `RunCommand` interception (`connection.rs:935-1121`) with its content-block gate, `command_limiter` (5/10s), and `check_run_command_channel_auth`. Kind `giveaway` is a new arm in the step-5 content-build match.
- **Add Command form:** `BotsTab.tsx` kind selector + `AddCommand` validation (`handlers.rs:2187`) — gains a `giveaway` option.
- **Message insert + broadcast:** `messages::insert_message_with_author_name`, `get_message`, `broadcast_event(EventTarget::Subscribers(channel_id), ServerEvent::NewMessage{..})`.
- **Sweeper template:** `bots::spawn_bot_poll_task` (`bots.rs:364-444`) — snapshot under a scoped db lock, drop the guard before any await, live loop. The shared widget sweeper mirrors it exactly.
- **Permission plumbing:** `resolve_member_server_perms` / `resolve_member_perms_pub`, `permissions::MANAGE_SERVER` / `VIEW_CHANNEL`, `members::is_timed_out`, `members::get_member` (carries `banned` + `revoked` — used for draw-time eligibility), `content_block_reason` + default-deny `request_requires_membership` (new request variants are membership-gated automatically).
- **RNG:** `rand = "0.8"` is already a direct dependency of farder-server (`crates/farder-server/Cargo.toml:31`) — `rand::thread_rng().gen_range(..)` for the draw; **no new dependency**.
- **Migration idiom:** guarded `PRAGMA table_info` ALTER + `CREATE TABLE IF NOT EXISTS` in `db::init_schema`.

## Goals

1. A mod (MANAGE_SERVER) configures a `giveaway`-kind command once; then `/<trigger> 24h Steam key` posts a 🎉 card: prize, live entry count, time remaining, Enter/Leave toggle.
2. One entry per member; entering/leaving updates every subscriber's card live.
3. At the deadline the **server** (sweeper) draws one winner uniformly among still-eligible entrants, persists the result **before** broadcasting (a crash can never redraw), flips the card to its ended state (winner shown, or "no entries"), and posts a follow-up announcement message in the channel.
4. Cancel (creator or MANAGE_SERVER) voids an open giveaway; Reroll (creator or MANAGE_SERVER, only after ended-with-winner) redraws among existing eligible entries and announces again.
5. Late joiners and reconnecting clients recover full widget state via a read request keyed off the widget JSON — **no change to the history-fetch wire format**.

## Non-goals (v1)

- **Winner DM.** See "Why no DM" below — announcement message + card highlight only.
- **Multiple winners / weighted entries / entry requirements** (role-gated entry, minimum account age). One winner, uniform, any visible member.
- **Editing a running giveaway** (prize/duration) — cancel and recreate.
- **Scheduled start** — a giveaway opens the moment it's posted.
- **Entrant-list UI.** Entries are stored by public key server-side, but v1 broadcasts only a count; "who entered" is a future mod-gated read (carry-forward), not an oversight.
- **Ephemeral errors beyond the existing invoker-only `RunCommand` Error path.**

## Design

### Shared substrate: widget messages

(Shared with `/poll` — built once.)

- `messages` gains a nullable `widget TEXT` column (guarded `PRAGMA table_info` ALTER, appended **last** so `MSG_SELECT` indices stay stable). It holds a small JSON tag: `{"type":"giveaway","id":<i64>}` (polls: `{"type":"poll","id":<i64>}`). No feature state lives in the message row — the tag is a pointer.
- `MessageInfo` gains `pub widget: Option<String>` with `#[serde(default)]` (append after `author_badge`); `MSG_SELECT` + `row_to_message_info` extended.
- New helper `messages::set_widget(conn, message_id, widget_json)` (small UPDATE, shared with `/poll` — built once): the card is inserted by plain `insert_message`, the feature row is created pointing at it, then `set_widget` stamps the JSON. Insert-then-update resolves the message-id↔widget-id circularity without touching any `insert_message` signature.
- `content` always carries a **plain-text fallback** (`🎉 Giveaway: <prize> — ends <local-format of ends_at>`) so an old client that doesn't know `widget` renders something sensible (serde-default = `None`, plain message).
- Client `Message.tsx`: parse `message.widget`; on `type === "giveaway"` render `<GiveawayWidget/>` **in place of** the plain `.message-content` body (the content is only the fallback); parse failure or unknown type → render content as today.

### Data model

Two new tables (guarded `CREATE TABLE IF NOT EXISTS` in `db::init_schema`, after the `commands` block):

```sql
CREATE TABLE IF NOT EXISTS giveaways (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,          -- the card message
    creator    BLOB    NOT NULL,          -- invoking user's PublicKey bytes
    prize      TEXT    NOT NULL,
    ends_at    INTEGER NOT NULL,          -- unix secs (db::now() unit, same as messages.timestamp and the poll tables)
    status     TEXT    NOT NULL DEFAULT 'open',   -- 'open' | 'ended' | 'cancelled'
    winner     BLOB,                       -- PublicKey bytes; NULL until drawn / when no entries
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS giveaway_entries (
    giveaway_id INTEGER NOT NULL,
    member      BLOB    NOT NULL,
    entered_at  INTEGER NOT NULL,
    PRIMARY KEY (giveaway_id, member)
);
```

The composite PK enforces one-entry-per-member at the schema level. No FK to `messages` — deleting the card **cancels** an open giveaway via the DeleteMessage hook (below), but the giveaway + entry rows are retained (audit), so no cascade delete either.

New module `crates/farder-server/src/giveaways.rs`: `GiveawayRow`, `create`, `get`, `build_info(conn, &row) -> GiveawayInfo` (computes `entry_count` via `COUNT(*)` and resolves `winner_name` when ended; same name convention as `polls::build_info`), `enter`, `leave`, `cancel`, `list_due(conn, now)`, `close_and_draw` (sweeper core), `reroll`, `my_entered(conn, giveaway_id, member) -> Result<bool>`.

### Protocol additions

All in `farder-protocol/src/server.rs`; MessagePack externally-tagged enums, so these are **appended** variants. **After any of these land, `cargo build --workspace` does NOT rebuild the Tauri client crate — `cd client/src-tauri && cargo build` separately** (the MemberApproved-class regression; also project memory `reference_farder_client_crate_build`).

```rust
pub struct GiveawayInfo {
    pub id: i64,
    pub channel_id: u64,
    pub message_id: u64,
    pub creator: PublicKey,
    pub prize: String,
    pub ends_at: u64,                 // unix secs
    pub status: String,               // "open" | "ended" | "cancelled"
    pub entry_count: u32,             // live count; identities stay server-side
    pub winner: Option<PublicKey>,
    pub winner_name: Option<String>,  // server-resolved display name, set when ended with a winner
}
```

One shape everywhere — responses, events, reroll updates — and it carries **shared state only**: a count (not the entrant pk list), status, and the winner (pk + server-resolved display name once ended, so clients don't need the entrant identities or a roster lookup to render the ended card). The per-viewer bit — *did I enter?* — is deliberately **not** in the broadcast shape: it rides only in the `GetGiveaway` read response as `my_entered: bool`, exactly mirroring the poll spec's `my_vote`. Entrant identities never leave the server in v1 (bounded broadcast size falls out for free).

- `ServerRequest::EnterGiveaway { giveaway_id: i64 }` → `Ok` (idempotent: already-entered is `Ok`)
- `ServerRequest::LeaveGiveaway { giveaway_id: i64 }` → `Ok` (idempotent)
- `ServerRequest::CancelGiveaway { giveaway_id: i64 }` → `Ok`
- `ServerRequest::RerollGiveaway { giveaway_id: i64 }` → `Ok`
- `ServerRequest::GetGiveaway { giveaway_id: i64 }` → `ServerResponse::Giveaway { giveaway: GiveawayInfo, my_entered: bool }` (`my_entered` is requester-specific, so it lives only in this response — same rule as the poll spec's `my_vote`)
- `ServerEvent::GiveawayUpdated { giveaway: GiveawayInfo }` — broadcast to `Subscribers(channel_id)` on every state change (enter, leave, cancel, draw, reroll). Terminal states fold into the same shape via `status` + `winner` — no separate "ended" event.

The actor for every request is the **authenticated connection key** — `giveaway_id` is the only client-supplied field. All five variants fall under the default-deny `request_requires_membership` (they are not in the bootstrap allow-list), so mesh log-membership gating is automatic.

### Dispatch — kind `giveaway` in `RunCommand`

`AddCommand` validation gains the arm: `kind == "giveaway"` requires **no** kind-specific fields (`body_text`/`url_template`/`value_path` all ignored/null); anything-else error text becomes "kind must be 'text', 'api', 'poll' or 'giveaway'". `CommandInfo.takes_arg` becomes `matches!(kind, "api" | "poll" | "giveaway")` (identical expression in the poll spec — new kinds opt in explicitly). BotsTab's kind `<select>` gains a "Giveaway" option whose only extra UI is a hint line: `Usage: /<trigger> <duration> <prize> — e.g. /giveaway 24h Steam key`.

New arm in the connection-level content-build match (`connection.rs`, step 5), running after the existing gates (content-block, `command_limiter`, `check_run_command_channel_auth`):

1. **MANAGE_SERVER gate (server-side, dispatch-time):** scoped lock → `resolve_member_server_perms(&conn, &member_key, is_owner)`; missing MANAGE_SERVER → `Error { reason: "giveaways can only be started by moderators (missing MANAGE_SERVER)" }`. Owner short-circuits as usual. (`check_run_command_channel_auth` already covered timeout + SEND_MESSAGES.)
2. **Parse args:** `args.trim().splitn(2, whitespace)` → `<duration> <prize>`. Duration: `^([0-9]+)([mhd])$` case-insensitive → minutes/hours/days; bounds **1m ≤ duration ≤ 30d**; violations → `Error { "usage: /<trigger> <duration> <prize> — duration 1m–30d (e.g. 30m, 24h, 7d)" }`. Prize: trimmed, 1–200 chars → `Error { "prize must be 1–200 characters" }`.
3. **Create (one scoped lock, one transaction — same insert-then-set-widget idiom as `/poll`):** `mid = messages::insert_message(&conn, channel_id, &member_key, &fallback_content, None)` (plain invoker authorship), then `gid = giveaways::create(&conn, channel_id, mid, &member_key, prize, now + duration_secs)`, then `messages::set_widget(&conn, mid, &format!(r#"{{"type":"giveaway","id":{gid}}}"#))`. Wrapped in `BEGIN`/`COMMIT` so no torn card-without-giveaway or giveaway-without-card state exists. `get_message` + `giveaways::build_info` for the broadcast copies; guard dropped.
4. **Broadcast:** `NewMessage { message }` to `Subscribers(channel_id)` (the card arrives through the normal message path; the widget JSON rides in `MessageInfo.widget`), then `GiveawayUpdated { giveaway }` (pre-seeds connected clients' reducers so the widget mounts with state, no GetGiveaway round-trip — same creation sequence as `/poll`) → `ServerResponse::Ok`.

**Author identity (substrate contract §7):** the card is authored by the **invoking user** — plain `author = member_key`, `author_name_override`/`author_badge` NULL. It is the mod's giveaway, rendered under their name; the bot pseudo-key machinery is not used.

No `.await` occurs in the giveaway arm, so the whole arm runs inside one scoped lock block before the broadcast — the existing "guard dropped before any await" discipline is preserved.

### Interaction handlers (`handlers.rs`, sync, exhaustive-match arms)

Common preamble for all five (identical idiom to the poll spec's handlers): load the giveaway (`Ok(None)` → `err("giveaway not found")`); **visibility check**: `channels::get_channel` → if `ChannelType::Dm` require `is_dm_participant` else `resolve_member_perms_pub` + `has(VIEW_CHANNEL)`. **Any visibility failure** (channel gone, not a DM participant, missing VIEW_CHANNEL) returns the same `err("giveaway not found")` — not a permission error — so giveaway ids are not an existence oracle for channels the caller can't see (same rule in the poll spec). Then per-request:

- **EnterGiveaway:** `state.widget_limiter.allow(caller)` (the shared `RateLimiter::new(10, 10)` on `ServerState` introduced by the poll spec — one limiter bounds votes and entries alike, since each fans out a broadcast) → `require_not_timed_out`; `status == "open"` **and** `now < ends_at` (belt-and-braces against the sweeper tick lag) → else `err("this giveaway has ended")` / `("...was cancelled")`; `INSERT OR IGNORE` into `giveaway_entries`; if no row was inserted (already entered), return `Ok` with **no** event (idempotent, no broadcast noise — matching RetractVote in the poll spec); else `ok_with(Ok, [GiveawayUpdated → Subscribers(channel_id)])`.
- **LeaveGiveaway:** `state.widget_limiter.allow(caller)` → `require_not_timed_out` (contract: every mutating widget request is timeout-gated, matching RetractVote); `status == "open"` required (entries are frozen once ended — leaving after the draw is meaningless); `DELETE`; if no row was deleted, return `Ok` with **no** event; else `ok_with(Ok, [GiveawayUpdated])`.
- **CancelGiveaway:** `require_not_timed_out` → authorized iff `member == creator` **or** MANAGE_SERVER (`resolve_member_server_perms` + `has`) — else `err("only the creator or a moderator can cancel")`; `status == "open"` required → else `err("giveaway already ended")`; `UPDATE ... SET status='cancelled' WHERE id=? AND status='open'` → `ok_with(Ok, [GiveawayUpdated])`. No announcement message; the card flip is the record.
- **RerollGiveaway:** `require_not_timed_out` → same creator-or-MANAGE_SERVER authz; requires `status == "ended" && winner IS NOT NULL` → else `err("can only reroll a finished giveaway with a winner")`; recompute the eligible set (same filter as the draw, below); if empty → `err("no eligible entries to reroll")` and the previous winner stands; else draw uniformly, `UPDATE ... SET winner=?` (still `status='ended'`), insert a fresh announcement message (below) — all before returning; events: `[GiveawayUpdated, NewMessage(announcement)]`, both `Subscribers(channel_id)`.
- **GetGiveaway:** preamble only → `ok(ServerResponse::Giveaway { giveaway, my_entered })` (`my_entered` = `giveaways::my_entered(conn, id, caller)`). No timeout gate (reads are allowed while timed out, matching GetPoll).

**`DeleteMessage` hook** (exactly mirrors the poll spec's delete-closes-poll hook): messages are hard-deleted. After the existing authz + before returning, if the loaded `msg.widget` parses to `{"type":"giveaway","id"}` and that giveaway is `status='open'` → `UPDATE giveaways SET status='cancelled' WHERE id=? AND status='open'` and push a `GiveawayUpdated` (cancelled) event alongside `MessageDeleted`. **Deleting the card cancels the giveaway**: no draw ever happens (the sweeper's `WHERE status='open'` filter skips it), no announcement posts (matching Cancel), and giveaway + entry rows are retained (audit/history). Deleting the card of an already-ended/cancelled giveaway changes nothing. No zombie open giveaways — same guarantee as polls.

All handlers are synchronous `handle_request` arms returning `HandleResult` events — no locks across awaits by construction (the connection loop broadcasts after the guard is gone).

### Sweeper — `widgets::spawn_widget_sweeper`

(Shared substrate §6 — **one** task closes due polls AND draws due giveaways; this section specs the giveaway half.) New module `crates/farder-server/src/widgets.rs`; `pub fn spawn_widget_sweeper(state: Arc<ServerState>) -> JoinHandle<()>`, spawned in `main.rs` next to `spawn_bot_poll_task` (`let _widget_sweeper = ...` at main.rs:143's block). Loop: sweep immediately, then `sleep(Duration::from_secs(WIDGET_SWEEP_SECS))` — fixed `pub const WIDGET_SWEEP_SECS: u64 = 15` (the same const the poll half uses), not owner-configurable (a 15s draw latency is invisible against multi-hour giveaways). The lock-scoped tick body is the sync `widgets::sweep_once(&conn, now) -> Vec<PendingBroadcast>` shared with the poll half.

Per tick, giveaway half (mirrors `bots.rs` lock discipline):

1. **One scoped lock block** — snapshot **and** persist:
   a. `list_due`: `SELECT ... FROM giveaways WHERE status='open' AND ends_at <= ?now`.
   b. For each due row, `close_and_draw(conn, &row)` inside a `BEGIN`/`COMMIT`:
      - Load entrants; filter to **eligible**: `members::get_member(conn, pk)` is `Some(m)` with `!m.banned && !m.revoked` (banned/removed members are excluded **at draw time** — membership is re-checked here, not at entry time).
      - Winner: `eligible.get(rand::thread_rng().gen_range(0..eligible.len()))` when non-empty (uniform); `None` when empty.
      - `UPDATE giveaways SET status='ended', winner=? WHERE id=? AND status='open'` — the `AND status='open'` guard makes the open→ended transition single-shot; a concurrent Cancel that won the lock first leaves nothing to draw.
      - Insert the **announcement message in the same transaction**: `insert_message_with_author_name(conn, channel_id, &announce_key, &text, Some(row.message_id), Some("Giveaway"), Some("BOT"))` — text `🎉 <display_name> won: <prize>` (winner's display name resolved server-side via `members::get_member`, falling back to the short key form) or `🎉 Giveaway ended — no entries: <prize>`. `reply_to = card message id` links it back. `announce_key` is a **freshly generated non-member keypair** (generated per announcement, secret discarded immediately — the server signs nothing here; it's a DB row like any webhook post, and webhooks already insert messages under generated non-member keys, so this is established precedent). Deliberately **not** the creator's key: a BOT-badged automated message must never be attributed to a real member's identity. Server-side insert, so mesh content-gating is unaffected. `author_name_override = "Giveaway"` + `author_badge = "BOT"` make it render as an automated post. Reroll announcements (handler above) use the identical form (fresh key each time) with text `🎉 Reroll — <display_name> won: <prize>`.
      - Build the `GiveawayUpdated` + `NewMessage` event payloads while still holding the lock; push onto a local `Vec`.
   c. `// MutexGuard dropped here`.
2. **After the guard drops:** `broadcast_event(&state, Subscribers(channel_id), ev).await` for each collected event.

**Crash safety (persist-then-broadcast):** winner + announcement row commit atomically **before** any broadcast. Crash before commit → the row is still `status='open'` and the next tick redraws from scratch (no partial state existed). Crash after commit but before broadcast → the winner is durable, the `status='open'` guard means it is **never redrawn**; the lost live events are recovered by clients through history fetch (the announcement is a normal message) and `GetGiveaway`.

**Why no winner DM (decision, contract asks for the call):** `bots::send_bot_dm` E2EE-encrypts with a **bot secret key** the server stores (`get_bot_secret` → `encrypt_bot_dm`). The giveaway card is authored by the *creator's user key* (whose secret the server does not and must not hold) and the announcement by a throwaway generated key whose secret is discarded at insert — so there is no key that can legitimately sign/encrypt a DM "from the giveaway". Sending it would require minting a persistent server-side "Giveaway bot" identity with a stored secret key purely for this notification — real scope (bot registration, roster presence, key lifecycle) for marginal value when the winner already gets a channel announcement that mentions them plus the flipped card. **v1 skips the DM**; if wanted later, a registered giveaway-bot identity drops in and the sweeper calls `send_bot_dm` after the broadcast step (carry-forward).

### Client

**Types (`types.ts`):** `MessageInfo.widget?: string | null`; `GiveawayInfo` mirroring the Rust struct (`entry_count: number`, `winner: string | null` — pk string, matching the `to_string()` convention in `bridge.rs` — `winner_name: string | null`).

**Bridge + Tauri commands** (5 new, each: `tauri-bridge.ts` fn → `#[tauri::command]` in `client/src-tauri/src/commands.rs` → **registered in `generate_handler!` in `main.rs`** — the untyped seam, zero drift required; + `docs/modules/tauri-commands.md` entries):
`getGiveaway(serverId, id): Promise<GiveawayState>` where `#[derive(Serialize)] struct GiveawayState { giveaway: GiveawayInfo, my_entered: bool }` ← `ServerResponse::Giveaway` (same pattern as the poll spec's `PollState`); `enterGiveaway`, `leaveGiveaway`, `cancelGiveaway`, `rerollGiveaway` — the standard 3-arm response mapping (`Giveaway{..}`/`Ok` → `Ok`, `Error{reason}` → `Err(reason)`, catch-all).

**Event plumbing:** `bridge.rs` `dispatch_event` gains `ServerEvent::GiveawayUpdated { giveaway } => emit("server:giveaway_updated", json!({ "server_id": sid, "giveaway": giveaway }))` (PublicKeys serialize via the existing to_string path in the json! of GiveawayInfo — serialize the struct with pk-to-string mapping consistent with `MessageInfo` handling). `useServerEvents.ts` adds a listener: active-server check, then `dispatch({ type: "GIVEAWAY_UPDATED", serverId, payload: giveaway })`.

**Reducer (`ServerContext.tsx`):** `PerServerState` gains `giveaways: Record<number, { giveaway: GiveawayInfo; myEntered: boolean }>` (initialized `{}`; same slice shape as the poll spec's `polls`). Actions:
- `GIVEAWAY_UPDATED { serverId, payload: GiveawayInfo }` — upsert by `payload.id` (immutable rebuild, same idiom as `ATTACHMENT_REDACTED`), **preserving** existing `myEntered` (broadcast events don't carry it), default `false`.
- `GIVEAWAY_STATE { serverId, payload: { giveaway: GiveawayInfo; myEntered: boolean } }` — from `getGiveaway`.
- `GIVEAWAY_MY_ENTERED { serverId, payload: { giveawayId: number; myEntered: boolean } }` — dispatched by the widget after a successful `enterGiveaway` (true) / `leaveGiveaway` (false) ack.

Background-server events are dropped like other message events — state re-hydrates via `GetGiveaway` on widget mount after a server switch.

**`GiveawayWidget.tsx`** (new component, rendered by `Message.tsx` when `widget` parses to `{type:"giveaway",id}`):
- Reads `giveaways[id]` from context; if absent (late join, reconnect, server switch, scroll-back into history) → `api.getGiveaway(serverId, id)` once on mount → dispatch `GIVEAWAY_STATE` (delivers both the shared state and `myEntered`). **This read-request is the state-recovery path** (substrate §5 — chosen over embedding state in history fetch, so `fetch_history`'s wire shape is untouched and one code path serves history, reconnect, and late-subscribe alike) — and it is the **only** place the client learns whether it entered, mirroring the poll spec's `my_vote`. A failed fetch (deleted server-side, old export) renders the plain-content fallback.
- **Open card:** 🎉 + prize, live countdown (1s `setInterval` recomputing from `ends_at` — no server ticks), entry count (`entry_count`), and one toggle button: **Enter** ↔ **Leave** driven by `myEntered`; on a successful ack the widget dispatches `GIVEAWAY_MY_ENTERED` (optimistic local toggle on the ack — the `GiveawayUpdated` broadcast only refreshes the shared count, it cannot flip the button). On error the toggle stays put and the error surfaces inline. A **Cancel** link shown when `ownPk === creator || canManageServer` (server re-checks regardless).
- **Ended card:** "🎉 Winner: `<display name>`" using the server-resolved `winner_name` (fallback to the short form of `winner` when null — e.g. the winner left the roster; no client-side roster lookup needed) or "No entries." A **Reroll** link under the same creator-or-mod visibility, only when `winner` is set.
- **Cancelled card:** muted "Giveaway cancelled."
- Request errors surface inline in the card (reuse `.error-text`).

**MessageInput:** no changes — `/trigger` already dispatches through `runCommand` for any registered command; `takes_arg` keeps a trailing space in the autocomplete insert.

**CSS:** new classes `.giveaway-widget`, `.giveaway-prize`, `.giveaway-meta` (count + countdown row), `.giveaway-actions`, `.giveaway-enter-btn`, `.giveaway-winner`, `.giveaway-cancelled` — added to **all three** theme files (`xp-luna-blue`, `discord-dark`, `hello-kitty`), colors exclusively via `var(--xp-…)`, card look modeled on the existing `.link-embed` card + `.link-embed-chip` button chip. `grep -l giveaway-widget client/src/themes/*/theme.css` must list all three before done.

### Edge cases

- **Enter-after-end race:** three defenses that compose under the single DB mutex — the handler checks `ends_at` against now (clean UX even before the sweeper tick), the handler checks `status`, and the sweeper's `WHERE status='open'` update serializes with any in-flight enter (whichever takes the mutex second sees the other's committed state). An entry can land in the final ≤15s after `ends_at` but before the sweep only if the handler's `ends_at` check passed first — it can't, so no late entries.
- **Reroll after all entrants left/banned:** eligible set recomputed at reroll; empty → error, previous winner stands (documented behavior — an "unwin" would be more confusing than a stale winner).
- **Deleted card message:** the DeleteMessage hook **cancels** an open giveaway atomically with the delete (`status='cancelled'` + `GiveawayUpdated` alongside `MessageDeleted`) — no draw ever happens, no announcement posts, and the sweeper skips it (`WHERE status='open'`). Rows retained (audit). Deleting the card of an already-ended giveaway changes nothing: the winner stands and the announcement stays (its `reply_to` renders via the client's existing deleted-reply path).
- **Cancel vs. sweeper race:** both mutate under the mutex with `status='open'` guards; exactly one wins, the loser no-ops/errors.
- **Command deleted while giveaways run:** unaffected — giveaway rows reference nothing in `commands`.
- **Mesh log-mode:** creation is behind the existing connection-level `content_block_reason` gate on `RunCommand`; all five interaction requests are membership-gated by the default-deny `request_requires_membership` with no allow-list additions.
- **Winner leaves after the draw:** winner pk stays persisted; `build_info`'s `winner_name` resolution falls back to `None` → clients show the short key form; announcement text (snapshotted display name) is already in history.

### Security

- Giveaway **creation** is MANAGE_SERVER, enforced **server-side at dispatch** (the BotsTab form gating is cosmetic; a modified client hits the same wall).
- Actor identity is always the connection's authenticated key; requests carry only ids. Timeout gates every mutating request (enter/leave/cancel/reroll — reads exempt, matching the poll spec); channel visibility gates all interaction (opaque "not found" on failure); creator-or-MANAGE_SERVER gates cancel/reroll; enter/leave fan-out is bounded by the shared `widget_limiter`.
- The draw runs **only server-side** in the sweeper with `rand::thread_rng()` (OS-seeded CSPRNG via `rand` 0.8) — clients cannot influence or predict it; persist-before-broadcast means no crash/retry ever produces two winners. Draw-time eligibility re-check (banned/revoked filtered) is unchanged by any of this.
- **Entrant identities never leave the server in v1:** broadcasts and reads carry only `entry_count` + `status` + `winner`/`winner_name`; the sole per-viewer fact (`my_entered`) is self-only in the `GetGiveaway` response — no request returns another member's entry, mirroring the poll spec's `my_vote` rule. Only the drawn winner's identity becomes public.
- The announcement's author key is a server-generated non-member throwaway (webhook precedent); its secret is discarded at insert, it never appears in the roster, and it can never authenticate a connection — it exists only as a message-author row.

## Testing

- **Duration parse (unit, pure fn `parse_giveaway_duration(&str) -> Option<u64>`):** `30m`/`24h`/`7d`/case-insensitive → secs; `0m`, `31d`, `5w`, `banana`, empty → None; bounds 1m/30d inclusive.
- **Dispatch (unit, via the extracted sync create fn):** non-mod → MANAGE_SERVER error, no rows; mod creates → giveaway row + card message with correct `widget` JSON, fallback content, author = invoker, no badge; bad args → usage error, no rows; transaction leaves no orphan on forced mid-failure.
- **Handlers (unit, `setup()` + `fake_state()` fixtures):** enter idempotent (double-enter → one row, Ok both, second emits no event); leave idempotent (no-entry leave → Ok, no event); enter timed-out → denied; leave timed-out → denied; enter after `ends_at` / on cancelled → error; enter without VIEW_CHANNEL → denied; cancel by rando → denied, by creator → Ok + event, by MANAGE_SERVER member → Ok; cancel twice → second errors; reroll on open / on no-winner → error; reroll with empty eligible set → error + winner unchanged; GetGiveaway shape (`entry_count`, status) + correct `my_entered` per requester (entered account true, other account false); DeleteMessage on an open giveaway's card → status `cancelled` + both events (`MessageDeleted` + `GiveawayUpdated` cancelled) and `list_due` no longer returns it; DeleteMessage on an ended giveaway's card → status/winner untouched, no `GiveawayUpdated`.
- **Draw (unit, on `close_and_draw` with an in-memory db):** no entries → `status='ended'`, `winner NULL`, "no entries" announcement inserted; entries with one banned + one revoked member → winner never the excluded pks (loop the draw N times); winner ∈ entrants; announcement author is a freshly generated key that matches **no** member row, with `author_name_override = "Giveaway"` + `author_badge = "BOT"`; **idempotence/crash-safety**: calling the sweep pass twice draws exactly once (second pass sees `status='ended'`, zero new announcements).
- **Sweeper plumbing:** `list_due` respects `ends_at`/status; the tick fn is extracted as sync `widgets::sweep_once(conn, now) -> Vec<PendingBroadcast>` so tests run it without tokio.
- **Client:** `cd client/src-tauri && cargo build` (protocol change — the workspace build alone is NOT sufficient), `cd client && npx tsc --noEmit`; invoke-name ↔ `generate_handler!` audit for the 5 new commands; theme grep for the new classes across all 3 files.

## Owner runtime verification (server changed → sidecar rebuild; two clients ideal)

1. Bots → Add Command: kind **Giveaway**, trigger `giveaway`. As a non-mod, `/giveaway 5m test` → clean "moderators only" error, no post.
2. As owner: `/giveaway 2m Steam key` → 🎉 card under your name (no BOT badge), countdown ticking, "0 entries".
3. Second account: Enter → both clients' counts tick to 1 live; Leave → back to 0; Enter again.
4. Wait out the 2m (+≤15s sweep): card flips to the winner's display name AND a "Giveaway"-badged announcement message appears replying to the card.
5. Reroll from the card → winner re-announced. Start another and Cancel it → card shows cancelled on both clients.
6. Restart the client mid-giveaway → the card re-hydrates (count/countdown/your Enter↔Leave toggle correct) via `GetGiveaway`.
7. Restart the **server** with a giveaway past due → on boot the first sweep draws exactly once (no double announcement).
8. Start a giveaway, have the second account enter, then **delete the card message** → the giveaway is cancelled on both clients; waiting past its deadline produces no draw and no announcement.

## Decomposition (for the plan)

1. **Server: substrate + model.** `widget` column migration + `MessageInfo.widget` + `messages::set_widget` (shared with the poll plan — first lander creates them); `giveaways`/`giveaway_entries` tables + `giveaways.rs` module (create/get/build_info/enter/leave/cancel/reroll/list_due/close_and_draw + duration parser). Unit-tested.
2. **Server: protocol + dispatch + handlers.** `GiveawayInfo`, 5 requests + `Giveaway` response + `GiveawayUpdated` event; `giveaway` kind in AddCommand validation + `takes_arg`; the RunCommand dispatch arm; the 5 handler arms. Unit-tested. (Client crate rebuild after this step.)
3. **Server: sweeper.** `widgets.rs` `spawn_widget_sweeper` + `sweep_once` (poll half stubbed or landed by the sibling plan — coordinate: whichever plan lands second wires its half into the existing task) + main.rs spawn. Unit-tested via `sweep_once`.
4. **Client: plumbing.** Types, 5 bridge fns + Tauri commands + registration, `server:giveaway_updated` emit + listener + `GIVEAWAY_UPDATED` reducer + `giveaways` state slice.
5. **Client: widget UI.** `GiveawayWidget.tsx` + `Message.tsx` widget slot + BotsTab kind option + CSS in all 3 themes.
6. **Docs.** `tauri-commands.md`, `tauri-bridge.md`, module doc for `giveaways.rs`/`widgets.rs`, ARCHITECTURE.md sweeper mention.

## Carry-forward / known limitations

- **Winner E2EE DM** — needs a registered giveaway-bot identity (stored secret key) so `send_bot_dm` has a key to encrypt with; additive once bot identity minting is generalized.
- **Entrant-list UI:** per-pk entries already exist server-side; a future mod-gated `GetGiveawayEntrants` + expand UI drops in without schema changes (same shape as the poll spec's voter-list carry-forward).
- Entry requirements (role-gated entry); multiple winners; editing a live giveaway; a "giveaways" moderation list view (all currently-open giveaways per server).
- 15s sweep granularity means the draw lands up to 15s after the nominal deadline (invisible at real durations, documented for tests).
- Old clients (pre-`widget`) see only the plain-text fallback card and the announcement — degraded but coherent; they cannot enter.
