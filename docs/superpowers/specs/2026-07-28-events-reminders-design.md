# Events (RSVP cards) + personal reminders — design

**Date:** 2026-07-28
**Status:** design (awaiting owner review)
**Context:** two owner-requested features built on the **shipped** widget substrate ([[2026-07-27-poll-command-design]], [[2026-07-27-giveaway-command-design]], [[2026-07-27-widget-ux2-design]]). Feature 1 is an **event card**: "planning an event. like oh party on my house x a day and you can enter or leave to show that youre coming" — a card with a start time and Going / Maybe / Can't make it buttons, showing **who** is coming. Feature 2 is **personal reminders**: `/remind 2h take the pizza out`, no card, a DM when it's due. Both need a thing the server has never had: an identity the **server itself** can speak as. That enabling piece (§1) is specified here and retroactively unblocks the giveaway winner DM that v1 deferred.

## Problem

1. **Events.** Nothing in Farder answers "who's coming?". A poll can approximate it (`Going | Maybe | No`) but polls hide voters by design, have no start time, no local-time rendering, no reminder, and no automatic "it's happening now". The whole point of an event card is the **roster**.
2. **Reminders.** There is no way to ask the server to nudge you later. Every existing "the server acts later" path (bot alerts, giveaway draws) is server-configured and channel-visible; a reminder is per-member, private, and posts nothing.
3. **Nobody to send from.** Both features must deliver DMs. `bots::send_bot_dm` (bots.rs:491) needs a **server-held secret key** from the `bots` table — which is exactly why the giveaway winner DM was skipped in v1: "the card author is a user whose key the server does not and must not hold". Without a server-owned identity, neither feature can notify anyone.

## Shared substrate (reused verbatim — no parallel mechanism)

Everything below is **shipped code**; this spec extends it, it does not re-invent it.

- **`messages.widget` TEXT** holding `{"type":"poll"|"giveaway","id":N}` → gains `{"type":"event","id":N}`. `MessageInfo.widget` (`#[serde(default)]`, `MSG_SELECT` index 10) is unchanged; `messages::set_widget` is the insert-then-set-widget idiom (`polls::create_poll_card` polls.rs:314 is the exact template: one `conn.transaction()`, insert card → create row → `set_widget` → `get_message` + `build_info`).
- **`commands.kind`** plugs new interactive kinds into the `RunCommand` dispatch (connection.rs:935+, the `if cmd.kind.as_str() == "poll"` / `"giveaway"` arms) → adds **`"event"`** and **`"reminder"`**. The owner enables each in BotsTab → Add Command with a trigger of their choice, exactly like poll/giveaway. `commands::list_infos` → `takes_arg = matches!(kind, "api" | "poll" | "giveaway" | "event" | "reminder")`.
- **One shared sweeper** `widgets::spawn_widget_sweeper` (widgets.rs, `WIDGET_SWEEP_SECS = 15`, poll-then-sleep, bots.rs lock discipline) already closes polls + draws giveaways → it gains the **event half** and the **reminder half**. `sweep_once(&conn, now)` stays the sync, tokio-free, lock-scoped tick body (one signature change, §5.1).
- **`ServerEvent::PollUpdated` / `GiveawayUpdated`** pattern → **`EventUpdated { event: EventInfo }`** to `EventTarget::Subscribers(channel_id)`; terminal states fold into the same shape via `status` (no separate `EventStarted`/`EventCancelled` events).
- **Read requests** `GetPoll`/`GetGiveaway` (per-viewer `my_vote`/`my_entered` live **only** in the response, never in the broadcast) → **`GetEvent { event_id }` → `Event { event, my_rsvp: Option<String> }`**.
- **`ListActiveWidgets { channel_id }` → `ActiveWidgets { polls, giveaways }`** gains **`events`** (upcoming only) so event chips appear in the active-widgets bar under the channel header.
- **Widget links** `farder://widget/(poll|giveaway)/<channel_id>/<id>` → the scheme, `Message.tsx` detection (`WIDGET_LINK_REGEX`, the `isInviteLink` exclusion guard, `.widget-link-pill`) and `LinkedWidgetCard` all extend to `event`, with the shipped refetch discipline (`refetch?: "mount" | "interval"`: mount + after own interaction + 20 s interval while mounted, cleared on unmount).
- **Builder modals are the primary creation UX** (owner's explicit call: forms, not typed syntax). `PollBuilderModal`/`GiveawayBuilderModal` are the precedent, including the **"Custom…" duration control** (number 1–9999 + unit minutes/hours/days, clamped to 1 m–30 d against `MIN_DURATION_SECS`/`MAX_DURATION_SECS`, `.error-text` on violation). Typed args remain the power-user path.
- **`handlers::widget_channel_visible`** (handlers.rs:367 — DM → `is_dm_participant`, else `resolve_member_perms_pub` + `VIEW_CHANNEL`, channel-gone → `false`) with **opaque** not-found errors; the default-deny **`request_requires_membership`** (handlers.rs:393, a 4-entry allow-list) membership-gates every new request automatically; the shared **`widget_limiter`** (`RateLimiter::new(10, 10)`) bounds mutations.
- **E2EE:** these are server-side-state widgets → **refused in E2EE channels exactly like polls/giveaways** ([[2026-07-27-mesh-rung2-e2ee-design]] feature matrix row 5). No new behavior beyond matching the existing refusal (§9).

All timestamps are **unix seconds** via `db::now()`, the same unit as `messages.timestamp`, `polls.closes_at`, `giveaways.ends_at`.

## Goals

1. Any member who can post in a channel opens the **event builder** (📅 from the `/` autocomplete), fills in title / date / time / optional location / optional description / optional reminder lead, and an event card posts **as them**.
2. Every viewer sees the start time **in their own local time**, live RSVP counts, and **the names** under Going / Maybe / Can't make it.
3. One RSVP per member, changeable and clearable any time until the event starts.
4. At the reminder lead time the server DMs everyone who said **Going or Maybe**; at start time it flips the card to "happening now", posts a short channel announcement, and DMs the **Going** list; after start the card shows a past state and drops out of the active-widgets bar.
5. Cancel (creator or MANAGE_SERVER) flips the card to cancelled and DMs the Going list. Editing the time re-arms the reminder.
6. `/remind 90m text` sets a **private** reminder: nothing posts, the invoker gets a confirmation only they see, and at the due moment the server DMs them the text plus a link back to where they set it.
7. A **My reminders** section in Settings lists upcoming reminders with a Cancel button.
8. The server can speak as itself: **one lazily-created system identity** sends every DM in 4/6/7 — invisible in the roster, unlistable and unremovable in BotsTab.

## Non-goals (v1) — explicit

- **No recurring events** (no RRULE, no "every Friday"). One event = one instant.
- **No calendar export or integration** (no `.ics`, no Google/Apple Calendar sync, no webhook-out).
- **No timezone picker.** Storage is an absolute unix second; every client renders it in **its own** local time. No per-event tz field, no "shown in host's timezone" label beyond the rendered local string.
- **No per-attendee +1s / guest counts** ("Going, +2"). One member = one RSVP.
- **No reminder snooze / repeat / edit.** A reminder is set once, cancellable, fires once.
- No event *invites* to non-members, no RSVP deadlines/capacity limits, no waitlist.
- No attendee-list UI for polls/giveaways (their privacy rules are unchanged by this spec — see §8).
- No new notification surface: reminder/event delivery is a DM, using the existing DM path.
- No embedding widget state in `FetchHistory` (rejected in the substrate contract; state recovery is the read request).

---

## Design

### §1 — The server system identity (the enabling piece)

**Why.** `bots::send_bot_dm` opens a DM channel from a key, E2EE-encrypts with that key's **secret** (`get_bot_secret` → `encrypt_bot_dm`, bots.rs:452/474) and inserts the message. Every existing sender is either a real member (secret not held by the server — correctly) or a registered ticker bot. Reminder and event DMs come from *the server*, so the server needs one identity of its own.

**Shape.** A single row in the existing `bots` table with a **new kind**:

```sql
-- no DDL change: bots(public_key, secret_key, kind, coin_id, label, created_at, source_url, value_path, unit)
INSERT INTO bots (public_key, secret_key, kind, coin_id, label, created_at)
VALUES (?pk, ?sk, 'system', '', 'Farder', ?now)
```

plus `members::register_bot_member(conn, &pk, "Farder")` — a `members` row with `is_bot = 1` is **required**, not optional: `send_bot_dm` builds `DmCreated.participant` via `handlers::build_member_info`, which errors on a missing member row, and `DmEntry.participant` needs it too. Visibility is handled by exclusion (below), not by omitting the row.

**Creation / lookup (`bots.rs`):**

```rust
/// The server's own identity: lazily created on first use, then reused forever.
pub fn get_or_create_system_identity(conn: &Connection) -> Result<PublicKey>
```

`SELECT public_key FROM bots WHERE kind='system' LIMIT 1` → if present, return it. If absent: `Keypair::generate()`, insert the bot row + the member row, return. **Lazy** — a server that never uses reminders or events never mints one (nothing at boot, nothing in `init_schema`). **Idempotent** — every caller holds the single `state.db` mutex, which serializes lookup-then-insert; a partial-unique invariant (`CREATE UNIQUE INDEX IF NOT EXISTS idx_bots_system ON bots(kind) WHERE kind='system'`) is added as belt-and-braces so a future concurrent path can't mint two.

**Sending (`bots.rs`, DRY refactor — no second copy of the DM plumbing):**

```rust
pub async fn send_bot_dm(state, bot_pk, recipient_pk, text) -> Result<()>            // unchanged public API
    => send_bot_dm_as(state, bot_pk, recipient_pk, text, None, None).await
pub async fn send_bot_dm_as(state, bot_pk, recipient_pk, text,
                            name_override: Option<&str>, badge: Option<&str>) -> Result<()>
pub async fn send_system_dm(state, recipient_pk, text) -> Result<()>
    // resolves/creates the system identity inside the existing scoped lock block,
    // then delegates to send_bot_dm_as(.., Some("Farder"), Some("BOT"))
```

`send_bot_dm_as` is the current body with `insert_message` swapped for `insert_message_with_author_name` (both already exist); the lock discipline is untouched — **all DB + crypto work inside the scoped block, `MutexGuard` dropped before the first `broadcast_event`** (the comment block at bots.rs:485 stays true). The `author_name_override = "Farder"` + `author_badge = "BOT"` means the DM renders correctly even though the sender is not in the client's member map. Badge `"BOT"` is reused deliberately — a new `"SYSTEM"` badge would need CSS in all three themes for no product gain.

**Exclusion from the roster (must not leak into member lists):**

- `members::list_members_visible(conn)` — `list_members`'s query with `WHERE public_key NOT IN (SELECT public_key FROM bots WHERE kind='system')`. `GetMembers` (handlers.rs:1212) calls **this** instead of `list_members`. The filter runs **before** the mesh whitelist `all_members.retain(|m| m.is_bot || ls.is_member(&m.public_key))` (handlers.rs:1220), so the `is_bot ||` clause — which exists to keep ticker bots visible on mesh servers — can never re-admit the system identity.
- Because `BotsTab` derives its bot list from `activeServer.members` (BotsTab.tsx:23) and `MemberSidebar` from the same slice, **one filter removes it from both the roster and the BotsTab list**; no client change is needed for either.
- `bots::list_bots` gains `WHERE kind != 'system'` so the ticker poller never treats it as a bot to poll (it has an empty `coin_id`) and no future bot UI enumerates it.
- **`RemoveBot`** (handlers.rs:2079) gains a guard before `remove_bot`: if the target row's `kind == 'system'` → `err("that identity can't be removed")`. Defense in depth — the key is never listed, so a stock client cannot name it, but a modified client could.
- Nothing else changes: it holds no roles, `resolve_member_perms` gives it nothing, it can never authenticate a connection (the secret exists only server-side and no login path reads the `bots` table), and it is not a log member on mesh servers (it posts only via server-side `insert_message*`, the same path ticker bots and webhooks already use).

**Follow-on (called out, NOT built here):** with a durable server-held key in place, the deferred **giveaway winner DM** becomes a two-line addition to `giveaways`' sweeper half (`send_system_dm(state, &winner, "🎉 You won: <prize>")` after the broadcast step). It is deliberately out of scope for this spec's tasks.

---

### §2 — Events: data model

**Naming collision (important):** the table `events` **already exists** — it is the mesh signed log (db.rs:92, `accept_seq`/`event_hash`/`payload_type`). The new tables are therefore `channel_events` / `channel_event_rsvps`, and the new server module is **`crates/farder-server/src/channel_events.rs`** (`events.rs` is `EventTarget`/`BroadcastEvent`). The *protocol* and *client* names stay product-facing: `EventInfo`, `GetEvent`, `EventUpdated`, `EventWidget`.

Guarded `CREATE TABLE IF NOT EXISTS` in `db::init_schema`, after the giveaway block (covered by `test_schema_init_idempotent` by construction):

```sql
CREATE TABLE IF NOT EXISTS channel_events (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id          INTEGER NOT NULL,
    message_id          INTEGER NOT NULL,        -- the card message
    creator             BLOB    NOT NULL,        -- invoking member's public key (32 bytes)
    title               TEXT    NOT NULL,        -- 1..120 chars
    description         TEXT,                    -- NULL or 1..500 chars
    location            TEXT,                    -- NULL or 1..120 chars
    starts_at           INTEGER NOT NULL,        -- ABSOLUTE unix secs (no timezone stored)
    remind_lead         INTEGER,                 -- secs before start: 900 | 3600 | 86400; NULL = no reminder
    reminded_at         INTEGER,                 -- NULL until the lead-time DM batch fired  (single-shot guard)
    status              TEXT    NOT NULL DEFAULT 'upcoming',  -- 'upcoming' | 'started' | 'cancelled'
    started_at          INTEGER,                 -- set when the sweeper announces          (single-shot guard)
    cancelled_at        INTEGER,
    cancel_notified_at  INTEGER,                 -- NULL until the cancellation DMs fired    (single-shot guard)
    created_at          INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_channel_events_due ON channel_events(status, starts_at);

CREATE TABLE IF NOT EXISTS channel_event_rsvps (
    event_id   INTEGER NOT NULL,
    member     BLOB    NOT NULL,
    response   TEXT    NOT NULL,          -- 'going' | 'maybe' | 'no'
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (event_id, member)        -- one RSVP per member; upsert = change of mind
);
```

The composite PK gives one-RSVP-per-member at the schema level (the `poll_votes` idiom); clearing an RSVP is a `DELETE` (the `polls::retract` idiom). No FK to `messages` — deleting the card **cancels** the event via the DeleteMessage hook (§4), rows retained for audit. The three nullable `*_at` guard columns are what make each sweeper action **exactly-once** (§5).

**Module `channel_events.rs`** (mirrors `polls.rs`/`giveaways.rs`):

```rust
pub struct EventRow { /* the columns above */ }
pub struct ParsedEvent { title, when: WhenSpec, location: Option<String>,
                         description: Option<String>, remind_lead: Option<u64> }

pub fn parse_event_args(args: &str) -> Result<ParsedEvent, String>   // pure, no DB
pub fn resolve_start(when: &WhenSpec, now: u64) -> Result<u64, String>
pub fn create_event_card(conn: &mut Connection, channel_id: u64, invoker: &PublicKey,
                         parsed: &ParsedEvent, now: u64) -> Result<(MessageInfo, EventInfo)>
pub fn get(conn, id) -> Result<Option<EventRow>>
pub fn build_info(conn, &EventRow) -> Result<EventInfo>              // counts + capped name lists
pub fn rsvp(conn, event_id, member, response, now) -> Result<()>     // INSERT .. ON CONFLICT DO UPDATE
pub fn clear_rsvp(conn, event_id, member) -> Result<bool>            // rows-affected
pub fn my_rsvp(conn, event_id, member) -> Result<Option<String>>
pub fn responders(conn, event_id, responses: &[&str]) -> Result<Vec<PublicKey>>
pub fn cancel(conn, event_id, now) -> Result<bool>
pub fn edit(conn, event_id, &ParsedEvent, starts_at, now) -> Result<()>   // re-arms reminded_at
pub fn list_reminder_due(conn, now) -> Result<Vec<EventRow>>
pub fn list_start_due(conn, now) -> Result<Vec<EventRow>>
pub fn list_cancel_unnotified(conn) -> Result<Vec<EventRow>>
pub fn mark_reminded(conn, id, now) -> Result<bool>                  // guarded, single-shot
pub fn start_and_announce(conn, &EventRow, system_pk, now) -> Result<(EventInfo, MessageInfo)>
pub fn mark_cancel_notified(conn, id, now) -> Result<bool>           // guarded, single-shot
pub fn list_upcoming_in_channel(conn, channel_id, now, limit) -> Result<Vec<EventRow>>
```

### §3 — Events: protocol

`crates/farder-protocol/src/server.rs`, **appended** variants/fields (MessagePack externally-tagged enums):

```rust
pub struct EventInfo {
    pub id: i64,
    pub channel_id: u64,
    pub message_id: u64,
    pub creator: PublicKey,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub starts_at: u64,                 // absolute unix secs
    pub remind_lead: Option<u64>,       // secs before start
    pub status: String,                 // "upcoming" | "started" | "cancelled"
    pub going_count: u32,
    pub maybe_count: u32,
    pub no_count: u32,
    pub going_names: Vec<String>,       // server-resolved display names, CAPPED at 10
    pub maybe_names: Vec<String>,       // capped at 10
    pub no_names: Vec<String>,          // capped at 10
}
```

- **Names, not keys.** The roster carries **display names only** (server-resolved via `members::get_member`, the `GiveawayInfo.winner_name` precedent), capped at `ATTENDEE_NAME_CAP = 10` per option. The client renders "and N more" from `count - names.len()`. Public keys of attendees are **not** broadcast: the product needs "who is coming", not "here are 40 pubkeys", and this keeps the broadcast payload bounded (worst case 30 short strings) regardless of RSVP volume.
- **Per-viewer state stays out of the broadcast**, exactly as for polls/giveaways: `my_rsvp` rides only in the `GetEvent` response.

```rust
// ServerRequest (appended; none of them are added to request_requires_membership's allow-list)
GetEvent    { event_id: i64 },
RsvpEvent   { event_id: i64, response: String },   // "going" | "maybe" | "no"
ClearRsvp   { event_id: i64 },
CancelEvent { event_id: i64 },
EditEvent   { event_id: i64, title: String, description: Option<String>,
              location: Option<String>, starts_at: u64, remind_lead: Option<u64> },

// ServerResponse (appended)
Event { event: EventInfo, my_rsvp: Option<String> },
Notice { text: String },                            // §6: invoker-only confirmation, posts nothing
MyReminders { reminders: Vec<ReminderInfo> },        // §6

// ServerResponse::ActiveWidgets gains a third field
ActiveWidgets { polls: Vec<PollInfo>, giveaways: Vec<GiveawayInfo>,
                #[serde(default)] events: Vec<EventInfo> },

// ServerEvent (appended)
EventUpdated { event: EventInfo },                   // -> EventTarget::Subscribers(channel_id)
```

`EditEvent` is a **full replace** of the editable fields (same validation as creation) — no partial-patch semantics, so there is no "did they mean to clear the location?" ambiguity. The actor is **always** the authenticated connection key; every request carries ids/content only, never an actor field. `#[serde(default)]` on the new `ActiveWidgets` field keeps old *frames* decodable; as with every prior protocol addition, an old **client binary** cannot decode the new variants — client+server ship together (unchanged practice). After landing: `cargo build --workspace` **plus** `cd client/src-tauri && cargo build` (the non-workspace client crate — the MemberApproved-class regression, project memory `reference_farder_client_crate_build`).

### §4 — Events: creation, edit, interaction

#### 4.1 Typed grammar (power-user path) — `channel_events::parse_event_args`

Segments split on `|`, each trimmed (the `/poll` idiom):

```
/<trigger> <title> | <when> [| <location>] [| <description>] [| remind 15m|1h|1d]
```

1. If the **final** trimmed segment matches `^remind\s+(15m|1h|1d|none)$` (case-insensitive) it is **always** consumed as the reminder lead (`none` → `None`). Deterministic, like the poll duration rule.
2. Segment 1 = **title**, 1–120 chars. Segment 2 = **when** (required). Segment 3 (optional, may be empty to skip) = **location**, ≤120. Segment 4 (optional) = **description**, ≤500. Positional — no guessing.
3. **`<when>` accepts exactly two forms:**
   - relative: `^(\d{1,4})(m|h|d)$` → `starts_at = now + delta` (`in 3d` is accepted as a synonym: a leading `in ` is stripped);
   - absolute: `^@(\d{9,12})$` → an **absolute unix-seconds** timestamp. This is what the builder emits.
   A wall-clock string like `2026-08-01 20:00` is **rejected with an explicit reason** ("use the event builder for a date and time, or a relative time like `3d`"): the server cannot know the invoker's timezone, and silently assuming UTC or server-local is exactly the class of bug that makes an event land 8 hours off. The builder — which *does* know the browser's timezone — does the conversion (§4.2).
4. **Bounds:** `starts_at >= now + 60` ("the event must start at least a minute from now") and `starts_at <= now + 365 * 86_400` ("at most a year out").
5. Any violation → `ServerResponse::Error { reason }` to the **invoker only**; nothing posts. Usage string: `usage: /<trigger> Title | 3d [| location] [| description] [| remind 1h]`.

#### 4.2 The builder (primary UX) — `EventBuilderModal.tsx`

Opened from the `/` autocomplete when `cmd.kind === "event"` (MessageInput.tsx:320 `insertCommand` gains `"event"`; the modal builds the args and calls `runCommand` itself, exactly like `PollBuilderModal`). Fields:

| Field | Control | Validation (client, mirrors the server) |
|---|---|---|
| Title | `<input class="connect-input">` | required, ≤120, pipes replaced by `/` (the `stripPipes` idiom) |
| Date | `<input type="date" class="connect-input">` | required |
| Time | `<input type="time" class="connect-input">` | required |
| Location | `<input class="connect-input">` | optional, ≤120, `stripPipes` |
| Description | `<textarea class="connect-input">` | optional, ≤500, `stripPipes` |
| Reminder | `<select class="connect-input">` | None (default) / 15 minutes / 1 hour / 1 day |

**Timezone handling (explicit).** `const startsAt = Math.floor(new Date(`${date}T${time}`).getTime() / 1000)` — a date-time string **without** a `Z`/offset is parsed by JS as **local time**, which is precisely the intent ("8pm my time"). That absolute second is what travels (`@${startsAt}`) and what is stored. **Nothing timezone-shaped is stored or transmitted.** Every viewer renders it with `new Date(starts_at * 1000).toLocaleString(undefined, {...})`, so a member in another timezone sees *their* 3am. Client-side checks: `Number.isFinite(startsAt)`, `startsAt > now + 60` ("Pick a time in the future"), `startsAt <= now + 365d` ("Events can be at most a year out") — inline `.error-text`, submit blocked, server re-validates from scratch.

Args assembled: `` `${title} | @${startsAt}${location||description ? ` | ${location}` : ""}${description ? ` | ${description}` : ""}${lead ? ` | remind ${lead}` : ""}` ``.

#### 4.3 Dispatch — `"event"` kind in `RunCommand` (connection.rs)

New arm beside the `"poll"`/`"giveaway"` arms, **after** every existing gate runs unchanged: `content_block_reason` → `command_limiter` (5/10 s) → `check_run_command_channel_auth` (not-timed-out + DM-participant/blocked + `SEND_MESSAGES`). **That is the creation permission — no MANAGE_SERVER gate.** Events are social: anyone who can post in the channel can plan one. (Deliberate divergence from `/giveaway`, whose arm resolves MANAGE_SERVER at dispatch; giveaways hand out prizes, events do not.)

1. `channel_events::parse_event_args(&args)` — pure, no lock. `Err(reason)` → `Error { reason }` to the invoker, nothing posts.
2. One scoped `state.db.lock()` + `conn.transaction()` (the `create_poll_card` shape, guard dropped before any await):
   - fallback `content` for old clients: `📅 <title> — <RFC-3339 UTC of starts_at>` + optional `\n📍 <location>` + `\n<description>`;
   - `mid = messages::insert_message(&tx, channel_id, &member_key, &content, None)` — **plain invoker authorship**, no name override, no badge;
   - `eid = channel_events::create(&tx, …)`;
   - `messages::set_widget(&tx, mid, r#"{"type":"event","id":<eid>}"#)`;
   - `get_message` + `build_info`.
3. After the guard drops: `broadcast_event(Subscribers(channel_id), NewMessage { message })`, then `broadcast_event(Subscribers(channel_id), EventUpdated { event })` (pre-seeds connected reducers so the widget mounts with state, no `GetEvent` round-trip), then `ServerResponse::Ok`.

`AddCommand` validation (handlers.rs:2187) accepts `kind: "event"` with no kind-specific fields; the error text becomes "kind must be 'text', 'api', 'poll', 'giveaway', 'event' or 'reminder'".

#### 4.4 Handlers (`handlers.rs`, sync arms — exact authz sequences)

All five are membership-gated automatically (**not** added to `request_requires_membership`'s allow-list) and use the shared preamble: load the row (`None` → `err("event not found")`) → `if !widget_channel_visible(conn, member, row.channel_id as u64, is_owner)? { return err("event not found") }` — the **byte-identical** string for "no such event", "channel gone", "not a DM participant", and "no VIEW_CHANNEL", so an event id is never an existence oracle.

- **`GetEvent`** — preamble only (no timeout gate; reads are allowed while timed out, matching `GetPoll`) → `ok(Event { event: build_info(..), my_rsvp: my_rsvp(conn, id, member)? })`.
- **`RsvpEvent { event_id, response }`** — `state.widget_limiter.allow(caller)` (each RSVP fans out a broadcast) → `require_not_timed_out` → preamble (**no `SEND_MESSAGES` check — deliberate, see §8**) → `matches!(response.as_str(), "going"|"maybe"|"no")` else `err("invalid RSVP")` → **still open?** `status == "upcoming" && now < starts_at` else `err("this event has already started")` / `err("this event was cancelled")` (the `starts_at` half makes the cutoff exact even before the sweeper ticks) → `rsvp(..)` (upsert; identical response = same row rewritten, still emits, harmless) → `ok_with(Ok, [EventUpdated { event: build_info } → Subscribers(channel_id)])`.
- **`ClearRsvp`** — same gates minus the response check → `clear_rsvp` → **no row deleted → `Ok` with no event** (idempotent, no broadcast noise — the `RetractVote` rule); else `ok_with(Ok, [EventUpdated])`.
- **`CancelEvent`** — `require_not_timed_out` → preamble → `status == "upcoming"` else `err("event already ended or cancelled")` → **authz: creator OR MANAGE_SERVER** (`if row.creator != *member { require_base_perm(conn, member, is_owner, MANAGE_SERVER, "MANAGE_SERVER")? }`) → `UPDATE … SET status='cancelled', cancelled_at=?now WHERE id=?1 AND status='upcoming'` → `ok_with(Ok, [EventUpdated])`. **The DMs to the Going list are not sent here** — a sync handler cannot `.await`; the sweeper's cancel pass (§5.4) drains `cancel_notified_at IS NULL` within ≤15 s. That keeps every DM on one code path with one crash-safety guard.
- **`EditEvent`** — `require_not_timed_out` → preamble → `status == "upcoming"` else `err("only an upcoming event can be edited")` → creator-or-MANAGE_SERVER (same expression as Cancel) → re-run the **same field validation as creation** (title 1–120, description ≤500, location ≤120, `now + 60 <= starts_at <= now + 365d`, `remind_lead ∈ {None, 900, 3600, 86400}`) → `UPDATE …` and, **if `starts_at` changed, `reminded_at = NULL`** (re-arms the reminder; if the new lead moment is already past but the start is still future, the next sweep fires it immediately — documented, and better than silently skipping) → `ok_with(Ok, [EventUpdated])`.

**`DeleteMessage` hook** (extends the shipped poll/giveaway hook): after the existing authz, if `msg.widget` parses to `{"type":"event","id":N}` and that event is `status='upcoming'` → cancel it (`status='cancelled', cancelled_at=now`) and push `EventUpdated` alongside `MessageDeleted`. Rows retained (audit). The cancel-notify pass then DMs the Going list — deleting the card is a cancellation, and attendees are told.

**`ListActiveWidgets`** gains a third query: `channel_events::list_upcoming_in_channel(conn, channel_id, now, 20)` = `WHERE channel_id=?1 AND status='upcoming' AND starts_at > ?now ORDER BY id ASC LIMIT ?3` (the `starts_at > now` half excludes due-but-unswept events, matching the RSVP cutoff's exactness). The existing merge/cap discipline is unchanged: each list capped at 20, merged by `created_at`, truncated to 20 combined. No per-viewer fields. No new rate limit (read, `GetPoll` class), no `.await` in the arm.

### §5 — The sweeper (both halves)

#### 5.1 One signature change, justified

Reminder and event DMs are **async** (`send_system_dm` opens a DM channel, encrypts, inserts, and does a targeted broadcast) and therefore cannot happen inside the lock. `sweep_once` stays sync/tokio-free; it returns the DMs as **data**:

```rust
pub struct PendingDm { pub recipient: PublicKey, pub text: String }
pub struct SweepOutcome { pub broadcasts: Vec<PendingBroadcast>, pub dms: Vec<PendingDm> }

pub fn sweep_once(conn: &Connection, now: u64) -> SweepOutcome    // was -> Vec<PendingBroadcast>
```

The task loop is otherwise verbatim: scoped lock → `sweep_once` → **guard dropped** → `for pb in out.broadcasts { broadcast_event(&state, pb.target, pb.event).await }` → `for dm in out.dms { let _ = bots::send_system_dm(&state, &dm.recipient, &dm.text).await; }` → `sleep(WIDGET_SWEEP_SECS)`. `send_system_dm` re-acquires the mutex internally, which is safe precisely because the sweeper's guard is gone. Existing poll/giveaway halves are unchanged apart from pushing into `out.broadcasts`; the two shipped `sweep_once` tests get a one-line accessor change.

**Delivery semantics (stated plainly):** state is persisted **before** the DM is attempted, under a status guard. So a crash between persist and send loses **at most one** notification (the reminder is marked `sent`, the event `reminded`), and **never** duplicates one. **At-most-once is the deliberate choice**: a reminder that fires twice, or an event announced twice, is worse than a rare missed nudge after a server crash. Same rule the giveaway draw already follows.

#### 5.2 Event reminder pass (lead-time DMs)

`list_reminder_due` = `WHERE status='upcoming' AND remind_lead IS NOT NULL AND reminded_at IS NULL AND starts_at - remind_lead <= ?now AND starts_at > ?now` (the last clause means an event whose start also came due in the same tick skips the lead DM and gets only the start DM — no double-ping). Per row: `mark_reminded` = `UPDATE … SET reminded_at=?now WHERE id=?1 AND reminded_at IS NULL`; **rows-affected 0 → skip** (someone else already did it). Then `responders(conn, id, &["going", "maybe"])` → one `PendingDm` each:
`⏰ "<title>" starts <in 1 hour> — <local time is rendered client-side from the card; the DM carries an absolute phrasing>` + optional `\n📍 <location>` + `\nfarder://widget/event/<channel_id>/<event_id>`.

**Who gets the lead-time DM: Going + Maybe.** Recommended and adopted: a "Maybe" is an undecided person, and the nudge is exactly what converts it to a decision; a "Going" person wants the logistics reminder. "Can't make it" is a decision already made and gets nothing — DMing them would be spam. (The *start* and *cancel* DMs go to **Going only**: at that point "Maybe" has effectively not committed, and "it's starting now" to a maybe-attendee is noise.)

#### 5.3 Event start pass (flip + announce + DM)

`list_start_due` = `WHERE status='upcoming' AND starts_at <= ?now`. Per row, `start_and_announce` inside a `BEGIN`/`COMMIT`:

1. `UPDATE channel_events SET status='started', started_at=?now WHERE id=?1 AND status='upcoming'` — rows-affected 0 (a Cancel won the mutex first) → **abort this row, announce nothing**. This guard is what makes the announcement exactly-once across crashes and restarts.
2. Insert the announcement **in the same transaction**: `insert_message_with_author_name(&tx, channel_id, &system_pk, "📅 <title> is starting now!", Some(row.message_id), Some("Events"), Some("BOT"))` — authored by the **system identity** (§1) with `reply_to` = the card message, so it threads under the event. (Giveaways keep their throwaway-key announcement; no change there.)
3. Build `EventInfo` + `MessageInfo` for the broadcasts.

Sweeper output: `EventUpdated` (status `started` → the card flips to "Happening now" and the chip drops out of the active bar) + `NewMessage` (announcement), both `Subscribers(channel_id)`, plus one `PendingDm` per **Going** responder: `📅 "<title>" is starting now.` + optional location + the widget link.

The system identity is resolved **once per tick** at the top of the event half (`get_or_create_system_identity`) — lazily minting it the first time any server actually starts an event or fires a reminder.

#### 5.4 Event cancel-notify pass

`list_cancel_unnotified` = `WHERE status='cancelled' AND cancel_notified_at IS NULL`. Per row: `mark_cancel_notified` guarded update (rows-affected 0 → skip), then one `PendingDm` per **Going** responder: `❌ "<title>" was cancelled.` No channel message is posted (the card flip is the public record — the `CancelGiveaway` precedent).

#### 5.5 Reminder pass (feature 2)

`reminders::list_due` = `WHERE status='pending' AND due_at <= ?now ORDER BY due_at ASC LIMIT 200` (a bounded batch keeps one tick cheap after downtime; the remainder drains on the next tick). Per row: `UPDATE reminders SET status='sent', sent_at=?now WHERE id=?1 AND status='pending'` — rows-affected 0 → skip (already sent/cancelled). Then one `PendingDm`:

```
⏰ <text>
— set in #<channel name> · farder://channel/<channel_id>
```

**DM origins get no link-back.** `/remind` is reachable inside a DM, so `channel_id` can be a **DM** channel id — which is not in the client's `activeServer.channels` (and has no name: `create_dm_channel` stores `''`). A `farder://channel/<id>` pill for one would render as a nameless "Open channel" that drops the main view onto an unresolvable id. So when the origin channel is `ChannelType::Dm` the footer is the link-free `— set in a direct message` instead (suppressed **server-side**, in `widgets::reminder_dm_text`, so no client needs to know). The event DMs' `farder://widget/event/...` links are unaffected: those render an inline card and never switch channel.

**No broadcasts at all** — a reminder produces zero `PendingBroadcast`s. The only artifact anyone sees is a DM to one person.

### §6 — Personal reminders

#### 6.1 Table

```sql
CREATE TABLE IF NOT EXISTS reminders (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    owner      BLOB    NOT NULL,          -- the invoker; the ONLY recipient and the only reader
    channel_id INTEGER NOT NULL,          -- where it was set (link-back context)
    text       TEXT    NOT NULL,          -- 1..500 chars, server-enforced
    created_at INTEGER NOT NULL,
    due_at     INTEGER NOT NULL,          -- absolute unix secs
    status     TEXT    NOT NULL DEFAULT 'pending',  -- 'pending' | 'sent' | 'cancelled'
    sent_at    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_reminders_due   ON reminders(status, due_at);
CREATE INDEX IF NOT EXISTS idx_reminders_owner ON reminders(owner, status, due_at);
```

Module `crates/farder-server/src/reminders.rs`: `parse_reminder_args`, `create`, `count_pending`, `list_pending_for`, `cancel`, `list_due`, `mark_sent`. Bounds as constants: `MAX_REMINDER_TEXT = 500`, `MAX_PENDING_PER_USER = 20`, duration reuses the shipped 1 m–30 d range.

#### 6.2 `/remind` — dispatch kind `"reminder"`

Grammar: `/<trigger> <duration> <text>` — `args.trim().splitn(2, whitespace)`; duration `^(\d{1,4})(m|h|d)$` case-insensitive, **1 m–30 d**; text trimmed, 1–500 chars. Errors: `usage: /<trigger> <duration> <text> — e.g. /remind 90m take the pizza out (1m–30d)`.

New arm in `RunCommand`, running **after every existing gate unchanged** (`content_block_reason` → `command_limiter` → `check_run_command_channel_auth`; so a timed-out, blocked, non-SEND_MESSAGES or non-log member cannot set one, and it inherits DM handling for free):

1. Pure parse; failure → `Error { reason }`, nothing posts.
2. One scoped lock: `count_pending(conn, &member_key)? >= MAX_PENDING_PER_USER` → `Error { reason: "you already have 20 reminders pending — cancel one first" }`; else `create(conn, &member_key, channel_id, &text, now + secs, now)`.
3. Guard dropped → reply **`ServerResponse::Notice { text: "⏰ Reminder set for <humanized delta> — I'll DM you." }`**.

**How a success confirmation reaches only the invoker without posting a message (the mechanism, explicitly).** Today the dispatch can only *post a message* or *return `Error`* — `run_command` (client/src-tauri/src/commands.rs:2695) maps `ServerResponse::Ok → Ok(())` and `Error{reason} → Err(reason)`; the `Ok` case is silent by design because a message appeared. Abusing `Error` for a success would be a lie (red toast, "stay open" behavior in the builder modals). So the **minimal** mechanism: **one new response variant `ServerResponse::Notice { text: String }`**, returned on the request's own `request_id` — i.e. delivered over the existing per-request reply channel, to the one connection that asked, with **no broadcast and no message row**. Client side: `run_command` widens to `Result<Option<String>, String>` (`Ok → Ok(None)`, `Notice{text} → Ok(Some(text))`, `Error → Err`), `runCommand` in `tauri-bridge.ts` returns `Promise<string | null>`, and the two call sites that care toast it (`toast.success(notice)` — the shipped toast idiom, Message.tsx:106-111). The existing `await api.runCommand(...)` call sites (MessageInput, PollBuilderModal, GiveawayBuilderModal) compile unchanged.

#### 6.3 Requests

- **`ListMyReminders`** → `ok(MyReminders { reminders })`. **Owner-scoped by the connection key, always** — the request carries no owner field, so there is nothing to forge. `list_pending_for(conn, member)` = `WHERE owner=?1 AND status='pending' ORDER BY due_at ASC LIMIT 20`. Membership-gated by default-deny; no channel visibility involved (a reminder is not channel content); no timeout gate (read).
- **`CancelReminder { reminder_id }`** → `UPDATE reminders SET status='cancelled' WHERE id=?1 AND owner=?2 AND status='pending'`; **rows-affected 0 → `err("reminder not found")`** — the same opaque string for someone else's reminder, an already-fired one, and a nonexistent id (no oracle for other members' reminder ids). `state.widget_limiter.allow(caller)` for consistency with other mutations. No timeout gate: cancelling your own private nudge is not channel content and a timed-out member is not silenced from managing their own state.

```rust
pub struct ReminderInfo { pub id: i64, pub text: String, pub due_at: u64,
                          pub created_at: u64, pub channel_id: u64 }
```

### §7 — Client

**Types (`types.ts`):** `EventInfo`, `ReminderInfo`, `EventState { event: EventInfo; my_rsvp: string | null }`.

**Tauri commands** (`client/src-tauri/src/commands.rs`, standard 3-arm mapping, **each registered in `generate_handler!` in `main.rs`** — the untyped seam, plus `docs/modules/tauri-commands.md` entries):
`get_event(server_id, event_id) -> EventState`, `rsvp_event(server_id, event_id, response)`, `clear_rsvp(server_id, event_id)`, `cancel_event(server_id, event_id)`, `edit_event(server_id, event_id, title, description, location, starts_at, remind_lead)`, `list_my_reminders(server_id) -> Vec<ReminderInfo>`, `cancel_reminder(server_id, reminder_id)`; `run_command` return widened (§6.2); `list_active_widgets` return struct gains `events`.

**Bridge (`tauri-bridge.ts`):** `getEvent`, `rsvpEvent`, `clearRsvp`, `cancelEvent`, `editEvent`, `listMyReminders`, `cancelReminder`.

**Event plumbing (`bridge.rs` + `docs/modules/tauri-bridge.md`):** `ServerEvent::EventUpdated { event }` → `emit("server:event_updated", json!({ "server_id": sid, "event": event }))`; `useServerEvents.ts` listener drops non-active servers (the shipped rule) then dispatches `EVENT_UPDATED`.

**Reducer (`ServerContext.tsx`):** `PerServerState` gains `events: Record<number, { event: EventInfo; myRsvp: string | null }>`. Actions (SCREAMING_SNAKE + serverId + payload):
- `EVENT_UPDATED { serverId, payload: EventInfo }` — upsert, **preserving** existing `myRsvp` (broadcasts never carry it), default `null`;
- `EVENT_STATE { serverId, payload: { event; myRsvp } }` — from `getEvent`;
- `EVENT_MY_RSVP { serverId, payload: { eventId; myRsvp: string | null } }` — dispatched by the widget after its own successful ack.
- `ACTIVE_WIDGETS` gains `events: EventInfo[]` (ids into `activeWidgets.events`, infos upserted into the slice); `EVENT_UPDATED` maintains the bar the way `POLL_UPDATED` does — append when `status === "upcoming"` and the id is missing for the current channel, remove when it becomes `started`/`cancelled`.

**`EventWidget.tsx`** — props `{ serverId, eventId, refetch?: "mount" | "interval", onUnavailable? }` (identical contract to `PollWidget`/`GiveawayWidget`, so `LinkedWidgetCard` can host it):
- Mount fetch via `api.getEvent` behind the shipped `fetchedRef` guard (+ the `refetch` discipline for linked cards).
- **Upcoming card:** `.event-widget` → `.event-title` (📅 + title) → `.event-when` = `new Date(starts_at*1000).toLocaleString()` + a relative hint ("in 3 days", recomputed on a 30 s interval like the poll footer) → optional `.event-location` (📍) and `.event-description` → `.event-rsvp-row` with three `.event-rsvp-btn` (`Going` / `Maybe` / `Can't make it`), the current one carrying `.event-rsvp-btn--mine`; clicking a different one calls `rsvpEvent`, clicking **your own** calls `clearRsvp` (the poll retract idiom) → `.event-attendees` with three `.event-attendee-group`s: "Going · 4" then up to ten `.event-attendee-name`s then `.event-more` "and 2 more".
- **Started card:** header shows `.event-happening` "Happening now", then after the `HAPPENING_WINDOW_SECS` window the same block reads as a past event ("Started"). The local time itself stays on the `.event-when` line below and is **never** repeated in the happening block (that would print the same timestamp on two consecutive lines); RSVP buttons `disabled`.
- **Cancelled card:** muted `.event-cancelled` "Event cancelled" + strikethrough title; buttons inert.
- **Creator/mod controls:** `Cancel` and `Edit` (opens `EventBuilderModal` in edit mode, prefilled, submitting via `editEvent`) rendered when `ownPk === creator || hasPermission(MANAGE_SERVER)` — the server re-checks regardless. `.widget-copy-link` 🔗 copies `farder://widget/event/<channel_id>/<id>` (the shipped clipboard idiom).
- Errors surface inline in `.error-text`; the next `EventUpdated`/refetch self-corrects.

**`Message.tsx`:** the shipped widget slot gains `type === "event"` → `<EventWidget/>` in place of `.message-content`; `WIDGET_LINK_REGEX` becomes `/farder:\/\/widget\/(poll|giveaway|event)\/(\d+)\/(\d+)/gi` and `parseWidgetLink`'s kind union widens; `.widget-link-pill` label "📅 Event link". **New `CHANNEL_LINK_REGEX = /farder:\/\/channel\/(\d+)/gi`** for the reminder DM's link-back, rendering a `.widget-link-pill` "Go to #name" (name from the client's channel list; unknown → "Open channel") that selects the channel via the existing action. **Both new schemes MUST be added to the shipped `isInviteLink()` exclusion guard and the invite-embeds IIFE filter** — `INVITE_REGEX`'s `farder:\/\/[^\s]+` alternative matches them, and without the guard a reminder DM would render a bogus join card (this is the exact bug the widget-link spec already had to fix; the same fix extends).

**`LinkedWidgetCard.tsx`:** `kind: "event"` → `api.getEvent` → `EVENT_STATE` → `<EventWidget refetch={sameChannel ? "mount" : "interval"}/>`; the channel-id consistency check and the opaque "Event not available" card are unchanged in shape.

**`ActiveWidgetsBar.tsx`:** event chips 📅 + truncated title + `.widget-chip-time` "in 3h"/"tomorrow"; dropdown hosts `<EventWidget refetch="mount"/>`.

**`ReminderBuilderModal.tsx`** — opened from `/` autocomplete when `cmd.kind === "reminder"`:

| Field | Control | Validation |
|---|---|---|
| Remind me in | duration `<select>` (15m / 30m / 1h / 3h / 1d / 3d / 7d / **Custom…**) + the shared custom row (`<input type="number" min=1 max=9999>` + minutes/hours/days) | resolves to `\d{1,4}(m\|h\|d)`, clamped to [60, 2 592 000] via the shipped `resolveDurationToken` logic; violation → `.error-text` "Duration must be between 1 minute and 30 days" |
| Reminder text | `<textarea class="connect-input">` | required, ≤500 chars, live counter |

Submits `runCommand(serverId, trigger, channelId, `${token} ${text}`)` → on success `toast.success(notice)` and close. No pipe-stripping needed (the reminder grammar has no delimiter beyond the first space).

**`MyReminders.tsx`** (`client/src/components/settings/`, mirroring `AlertSubscriptions.tsx` exactly): `useActiveServerId` → `listMyReminders` in a `useEffect`, `SettingsSection label="Upcoming reminders"`, one `.organizer-row` per reminder (`.organizer-name` = text + due time via `toLocaleString`, `.organizer-btn .organizer-delete` = Cancel → `cancelReminder` → drop from local state), `.error-text` for failures, muted empty/disconnected states. **Reuses the shipped `.organizer-*` classes → zero new CSS.** `SettingsModal.tsx` gains `SectionId "reminders"` + a `{ id: "reminders", label: "Reminders" }` nav entry.

**`BotsTab.tsx`:** `cmdKind` widens to `"text" | "api" | "poll" | "giveaway" | "event" | "reminder"`; two `<option>`s; `isWidgetKind` includes them (no extra fields); hint lines — Event: `Members run /<trigger> Title | 3d [| location] [| description] [| remind 1h]` — or just pick it from "/" to open the form; Reminder: `Members run /<trigger> 90m take the pizza out — private, nothing is posted`.

**Theme CSS (CLAUDE.md rule)** — new classes, all added to **every** `client/src/themes/*/theme.css`, colors only via `var(--xp-…)`, modeled on `.link-embed`/`.poll-*`:
`.event-widget`, `.event-title`, `.event-when`, `.event-when-rel`, `.event-location`, `.event-description`, `.event-rsvp-row`, `.event-rsvp-btn`, `.event-rsvp-btn--mine`, `.event-attendees`, `.event-attendee-group`, `.event-attendee-name`, `.event-more`, `.event-happening`, `.event-cancelled`.
Done-check: `grep -l "event-rsvp-btn" client/src/themes/*/theme.css` lists all three.

### §8 — Privacy: why attendee names are visible (deliberate divergence)

Polls hide voters and giveaways hide entrants: their broadcasts carry counts only, and the single per-viewer bit (`my_vote`/`my_entered`) is self-only. **Events deliberately break that rule**, and this is a product decision, not an oversight:

- **The roster is the feature.** "You can enter or leave to show that you're coming" is the owner's requirement verbatim. An event card that showed "7 going" without saying *who* would not answer the only question anyone asks about a party. A poll can already do anonymous counting; an event exists precisely to be non-anonymous.
- **RSVPing is an affirmative, public act.** Nobody's data is exposed by anyone else's action: your name appears under an option **only** because you pressed that option, and pressing it again (or clearing it) removes you immediately. There is no passive collection.

**Stated plainly, because it must not be discovered later:**

- The host **and every member who can see the channel** sees the full breakdown, including who said "Can't make it". There is no anonymous RSVP and no "only the host sees it" mode in v1.
- The card exposes **display names only** — never public keys — and at most 10 per option in the payload, with counts for the rest. That bounds both the leak and the frame size.
- The visibility boundary is the **channel**: `widget_channel_visible` gates `GetEvent`, so a member without `VIEW_CHANNEL` (or a non-participant of the DM) learns nothing — not the roster, not the title, not that the event exists.
- Attendee names ride in `EventUpdated`, which targets `Subscribers(channel_id)` only — the same audience that can read the channel's messages.
- **Like every widget, events are unavailable in E2EE channels** (§9): the roster is server-side state, so an event in a sealed channel would mean handing the server the very content the channel promises to hide. Plan the party in a normal channel.
- Reminders are the opposite: `reminders.text` is readable only by its owner (`ListMyReminders` is key-scoped, `CancelReminder` is opaque), it is never broadcast, and it produces no channel artifact. It **is** stored server-side in plaintext, which is why `/remind` is refused in E2EE channels too.

**DECIDED: RSVP requires `VIEW_CHANNEL`, NOT `SEND_MESSAGES` — keep it that way.**

`RsvpEvent` gates on channel visibility (`widget_channel_visible`) + `widget_limiter` + not-timed-out, byte-identical to the shipped `VotePoll` arm. It deliberately does **not** require `SEND_MESSAGES`. The consequence, stated plainly: in an announcements-style channel where `@everyone` has `VIEW_CHANNEL` but not `SEND_MESSAGES`, a member who cannot post can still attach their display name to an event card.

That is the intended behavior, not an oversight:

- **An announcements channel is exactly where this feature lives.** "Party at my place Saturday" is posted where everyone will see it and where ordinary members cannot reply-spam. Requiring `SEND_MESSAGES` would make the event unRSVPable **by the very people it is for** — it would break the canonical use case to defend a boundary that is not actually being crossed.
- **Nothing is disclosed that the channel did not already disclose.** The name added to the roster is the member's display name, already visible to every member in the member roster. No public key, no new identifier.
- **The write is bounded and self-owned.** One row per member (PK-enforced upsert), three fixed values, rate-limited at 10/10 s, clearable by its author at any time, and the rendered list is capped at `ATTENDEE_NAME_CAP` names per option. It is not free-form content and cannot be used to say anything.
- **The real boundary still holds.** `VIEW_CHANNEL` (or DM participation) is the line: a member who cannot see the channel cannot RSVP, cannot read the roster, and cannot learn the event exists.

**Accepted trade-off:** a server owner who wants a strictly read-only announcements channel gets one exception — members can register attendance on event cards there (and, identically, vote in polls and enter giveaways: this is the existing widget rule, not a new one). A "RSVP needs SEND_MESSAGES" mode is a **non-goal for v1**. Do not "fix" this by adding a `SEND_MESSAGES` check to `RsvpEvent`; if it is ever wanted, it belongs behind a per-channel setting, not a hardcoded gate.

### §9 — E2EE (matching the existing refusal, no new behavior)

Per [[2026-07-27-mesh-rung2-e2ee-design]] the rung-2 gate rejects `RunCommand` **and** the widget interaction requests for E2EE channels at the request layer, and `insert_message*` refuses non-derived writes into E2EE channels (the choke point). Consequences here, requiring **no new mechanism**:

- Creating an event or setting a reminder in an E2EE channel is refused with the existing "not available in encrypted channels" error, because both are `RunCommand` kinds.
- `RsvpEvent`/`ClearRsvp`/`CancelEvent`/`EditEvent` inherit the same class-aware rejection as `VotePoll`/`EnterGiveaway` (unreachable in practice — creation was already refused).
- The sweeper cannot announce into an E2EE channel: the choke point rejects the write, and no event can exist there anyway.
- The rung-2 feature matrix gains: **events → server-features-channel-only** (row 5's rule extended) and **personal reminders → server-features-channel-only** (server-side plaintext text). Reminder **DMs** are the existing bot-DM class: encrypted with the system identity's key, which the server holds — the same honest caveat that already applies to ticker-bot alert DMs, not a new one.

### §10 — Security (explicit verification points)

- **Default-deny membership.** `GetEvent`, `RsvpEvent`, `ClearRsvp`, `CancelEvent`, `EditEvent`, `ListMyReminders`, `CancelReminder` are **not** added to `request_requires_membership`'s 4-entry allow-list, so mesh log-membership gating is automatic. **Verify by test**, not by inspection (one test asserts a non-member is refused for each).
- **Actor identity is always the authenticated connection key.** No request carries an RSVP-er, creator, or reminder-owner field. `ListMyReminders`/`CancelReminder` scope by `owner = caller` in SQL.
- **Opaque, oracle-free errors.** Every channel-scoped action funnels through `handlers::widget_channel_visible` and returns the byte-identical `"event not found"` for missing/invisible/forbidden. `CancelReminder` returns `"reminder not found"` for someone else's id. `ListActiveWidgets` keeps its `"channel not found"`.
- **Creation gates are the RunCommand gates, unchanged:** `content_block_reason` (mesh content gate) → `command_limiter` → `check_run_command_channel_auth` = not-timed-out + `SEND_MESSAGES` + DM-participant/`is_blocked`. Events add **no** MANAGE_SERVER gate (product decision); reminders add a per-user cap.
- **Timeout gating on state-mutating interactions:** `require_not_timed_out` on RSVP/clear/cancel/edit; reads (`GetEvent`, `ListMyReminders`) exempt, matching `GetPoll`. Rate limiting: RSVP/clear/cancel/edit/cancel-reminder all take `state.widget_limiter` (10/10 s); creation is bounded by `command_limiter` (5/10 s).
- **No DB mutex across any `.await`.** All handlers are sync `handle_request` arms. The dispatch arms snapshot under one scoped lock and broadcast after the guard drops. The sweeper persists under the lock, drops the guard, then broadcasts **and** DMs (`send_system_dm` re-acquires the mutex only after the sweeper's guard is gone — asserted by construction, and the reason DMs are returned as data rather than sent inline).
- **Persist-before-notify, single-shot guards.** `reminded_at IS NULL`, `status='upcoming'` (start), `cancel_notified_at IS NULL`, `status='pending'` (reminder) — every state flip is a guarded `UPDATE` whose rows-affected decides whether the notification is produced. A crash can therefore never double-fire a reminder or double-announce an event; the accepted cost is at-most-once (§5.1).
- **Length bounds enforced server-side** (the client mirrors them for UX only): title ≤120, location ≤120, description ≤500, reminder text ≤500, RSVP response ∈ {going, maybe, no}, `remind_lead` ∈ {900, 3600, 86400}, `starts_at` ∈ [now+60, now+365 d], duration ∈ [1 m, 30 d]. Per-user outstanding reminders ≤ 20.
- **The `widget` JSON is server-written only** and parsed defensively client-side (try/catch, numeric id).
- **The system identity** holds no roles, is filtered out of `GetMembers` before the mesh `is_bot` whitelist, is excluded from `list_bots`, cannot be removed via `RemoveBot`, and cannot authenticate a connection (no auth path reads `bots.secret_key`). Its secret is the same trust class as the existing ticker-bot secrets — the server is the bot's authority (bots.rs:1-3), stated openly rather than implied.

### §11 — Edge cases

- **RSVP racing the start:** the handler's `now < starts_at` check under the same mutex as the sweeper's guarded flip; the loser gets "this event has already started" and the UI self-corrects on the next `EventUpdated`.
- **Event card deleted:** DeleteMessage hook cancels it + broadcasts; the cancel-notify pass DMs the Going list; no announcement ever posts (`status='upcoming'` guard fails).
- **Editing the time backwards into the past:** rejected (`starts_at >= now + 60`). Editing forward re-arms `reminded_at`; editing a *started* or *cancelled* event is rejected.
- **Reminder lead longer than the time until start** (e.g. "1 day" on an event 2 hours out): `starts_at - remind_lead <= now` is already true, so the DM goes out on the **next tick** — an immediate "starts in 2 hours" nudge. Deliberate; better than silently dropping it.
- **Member leaves/is banned after RSVPing:** the RSVP row stands (a snapshot of who said yes while a member); their name still appears until they clear it, which they can no longer do. Their DM attempt is a no-op if the DM channel can't be opened. Documented; not recounted.
- **Nobody RSVPs:** start still flips + announces; zero DMs. Counts render "0 going".
- **20-reminder cap hit:** clear `Error` naming the cap; My reminders is where you cancel one.
- **Server down when a reminder comes due:** the next boot's first sweep fires it late (once). A reminder due during a >30 d outage still fires on boot — deliberately no expiry window.
- **Two events due in the same tick:** each row is independently guarded; announcements post in `id` order.
- **Reconnect / late joiner / scroll-back:** the card's `widget` JSON + `GetEvent` restores counts, roster and `my_rsvp` — no reliance on having seen an event.
- **Old clients:** render the plain-text fallback content (title + UTC time + location) and cannot RSVP; reminder DMs render as text with a bare `farder://channel/...` string.

---

## Testing

**Pure/unit (no DB):** `parse_event_args` — title/when/location/description positions; the `remind …` final-segment determinism; relative `3d`/`in 3d`/`2h` forms; absolute `@<unix>`; wall-clock string rejected with the builder-hint reason; bounds (`now+30` rejected, `now+400d` rejected, `now+60` accepted); over-length title/location/description; empty third segment = no location. `parse_reminder_args` — `90m taco`, case-insensitive units, `0m`/`31d`/`banana` rejected, missing text rejected, 501-char text rejected, text containing `|` preserved verbatim.

**`channel_events` module (in-memory conn):** `create_event_card` cross-links message ↔ event ↔ widget JSON with plain invoker authorship (the `create_poll_card_links_message_poll_and_widget` template); `rsvp` upsert moves a member between options without changing the total; `clear_rsvp` returns false when there was none; `build_info` counts are full totals while `*_names` cap at 10 (create 13 going, assert `going_count == 13 && going_names.len() == 10`); `responders(&["going","maybe"])` excludes "no"; `edit` with a changed `starts_at` nulls `reminded_at`, with an unchanged one does not; `list_upcoming_in_channel` excludes started/cancelled/past-but-unswept and other channels, `id ASC`, respects the limit.

**`reminders` module:** `create` + `count_pending` per owner; `list_pending_for` never returns another owner's rows; `cancel` guarded by owner (rows-affected 0 for a foreign id); `list_due` respects status + `due_at` + the 200 batch cap; `mark_sent` is single-shot.

**System identity (`bots`):** `get_or_create_system_identity` creates exactly one row on repeat calls (assert `COUNT(*) WHERE kind='system' == 1` after 3 calls) and registers a member row; `list_bots` excludes it; `members::list_members_visible` excludes it while `list_members` includes it; a `GetMembers` handler test asserts the system pk is absent **on both a legacy and a mesh-log server** (the `is_bot ||` whitelist path); `RemoveBot` on the system pk → error and the row survives.

**Handlers (`handlers.rs mod tests` fixtures — `setup()`/`add_member`/`make_channel`/`fake_state`):** RSVP happy path emits `EventUpdated` to `Subscribers`; RSVP with a bogus response string → err; RSVP after `starts_at` (unswept) → err; RSVP on cancelled → err; RSVP without `VIEW_CHANNEL` → **byte-identical** "event not found" as a nonexistent id (oracle test); timed-out member → denied on RSVP/clear/cancel/edit, allowed on `GetEvent`; `ClearRsvp` with no RSVP → `Ok` with **no** event; Cancel by a rando → MANAGE_SERVER error, by creator → cancelled, by a MANAGE_SERVER holder → cancelled, twice → err; Edit validation mirror + re-arm assertion; `GetEvent` returns the right `my_rsvp` per requester; `DeleteMessage` on an event card cancels + emits both events; `ListActiveWidgets` includes upcoming events, excludes started/cancelled, and still caps at 20 combined; `AddCommand` accepts kinds `event` + `reminder` and `list_infos` reports `takes_arg: true`; `ListMyReminders` returns only the caller's rows; `CancelReminder` on a foreign id → "reminder not found" and the row is untouched.

**Sweeper (`widgets::sweep_once`, sync, no tokio):** a due reminder produces exactly one `PendingDm` and flips to `sent`; a **second** `sweep_once` at the same `now` produces **zero** (crash-safety idempotence); a cancelled reminder produces none; an event at lead time produces DMs for going+maybe **only**, marks `reminded_at`, and a second sweep produces none; an event whose start and lead came due together produces the start batch only; the start pass flips `status='started'`, inserts **one** announcement authored by the system identity with badge `BOT` and `reply_to` = the card, DMs going-only, and a second sweep inserts **zero** further messages (assert `COUNT(*) FROM messages` unchanged); a cancelled event produces one DM batch then none; the shipped poll/giveaway assertions still pass through the new `SweepOutcome` shape.

**Builds / seams:** `cargo test --workspace`; `cd client/src-tauri && cargo build` (protocol change — the workspace build alone is NOT sufficient); `cd client && npx tsc --noEmit`; every new `invoke("…")` name present in `generate_handler!`; `grep -l "event-rsvp-btn" client/src/themes/*/theme.css` lists all three.

**Not unit-tested (runtime-verified):** DM delivery/rendering, sweeper timing, local-time rendering across timezones, dropdown anchoring, toast confirmation.

## Owner runtime verification (server changed → sidecar rebuild; two clients ideal)

1. Bots → Add Command: kind **Event**, trigger `event`; kind **Reminder**, trigger `remind`. Both appear in `/` autocomplete.
2. Type `/` → pick **event** → the builder opens. Title "Party at my house", date = today, time = 10 minutes out, location "my place", reminder **15 minutes**. Create → a 📅 card posts **as you** (no BOT badge) showing your local time.
3. Second account: **Going** → both cards show "Going · 1" with that member's **name**; switch to **Maybe** → the name moves; click Maybe again → the RSVP clears and the name disappears. Your own account RSVPs Going too.
4. Within ~15 s of the 15-minutes-out mark (use a short lead by editing the time), both Going and Maybe accounts receive a **DM from "Farder"** with the title and a widget link. Click the link → the interactive card opens.
5. At the start time (±15 s): the card flips to **"Happening now"**, a `📅 … is starting now!` message replies to the card, the Going account gets a DM, and the event's **chip disappears** from the active-widgets bar.
6. Create another event, then **Cancel** it from the card → both clients show cancelled; the Going account gets a cancellation DM; no channel message is posted.
7. Create one more, **delete its card message** → it is cancelled on both clients and no announcement ever appears.
8. Edit an event's time (creator) → the card updates live on both clients; the reminder fires again relative to the new time.
9. `/remind 2m water the plants` from the `/` autocomplete → the **reminder form** opens; submit → a toast confirms and **nothing is posted in the channel** (verify on the second client: no message at all). ~2 minutes later a DM from "Farder" arrives with the text and a "Go to #general" pill that jumps to the channel.
10. Settings → **Reminders**: set three more, see them listed with due times, Cancel one → it disappears and never fires. Set 20 → the 21st errors with the cap message.
11. Member sidebar and Bots tab: **"Farder" appears in neither**, and there is no way to remove it. It does appear as the DM sender in your DM list.
12. Paste an event link into another channel → interactive card; RSVP from there; the origin card catches up within ~20 s. Paste an invite link in the same message → the join card still renders (no regression).
13. Flip through all themes → the event card, RSVP buttons, attendee rows, chips and pills are all styled.

## Decomposition — exactly 5 build tasks

1. **T1 — System identity + reminders (server).** `bots::get_or_create_system_identity` + the unique partial index + `send_bot_dm_as`/`send_system_dm` refactor; `members::list_members_visible` + `GetMembers` swap + `list_bots` exclusion + `RemoveBot` guard; `reminders` table + `reminders.rs` (parse/create/count/list/cancel/list_due/mark_sent); `ServerResponse::Notice` + `MyReminders` + `ReminderInfo` + `ListMyReminders`/`CancelReminder` requests and handler arms; the `"reminder"` RunCommand kind + `AddCommand` validation + `takes_arg`; `SweepOutcome`/`PendingDm` signature change + the reminder pass in `sweep_once` + the DM loop in `spawn_widget_sweeper`. Unit tests per §Testing. Client crate rebuild.
2. **T2 — Events (server).** `channel_events`/`channel_event_rsvps` DDL + `channel_events.rs` (parse/create_event_card/build_info/rsvp/clear/responders/cancel/edit/list_* /start_and_announce/mark_*); `EventInfo` + 5 requests + `Event` response + `EventUpdated` event + `ActiveWidgets.events`; the `"event"` RunCommand kind (no MANAGE_SERVER gate) + `AddCommand` + `takes_arg`; the 5 handler arms + DeleteMessage hook + `ListActiveWidgets` third query; the sweeper's reminder/start/cancel-notify passes. Unit tests per §Testing.
3. **T3 — Client plumbing.** `EventInfo`/`ReminderInfo`/`EventState` types; 7 new Tauri commands + `generate_handler!` registration + bridge fns; `run_command` → `Result<Option<String>, String>` and `runCommand` → `Promise<string | null>`; `list_active_widgets` events field; `server:event_updated` emit + `useServerEvents` listener; `events` slice + `EVENT_UPDATED`/`EVENT_STATE`/`EVENT_MY_RSVP` + `ACTIVE_WIDGETS`/bar maintenance. `npx tsc --noEmit`.
4. **T4 — Event UI.** `EventWidget.tsx` (upcoming/started/cancelled states, RSVP buttons, capped attendee lists, local-time + relative rendering, creator/mod Cancel+Edit, copy-link, `refetch` prop); `EventBuilderModal.tsx` (create **and** edit mode, date+time→unix conversion, reminder select, bounds); `Message.tsx` widget slot + `farder://widget/event/...` detection + `farder://channel/...` pill + the `isInviteLink`/embeds-IIFE exclusion guards; `LinkedWidgetCard` event branch; `ActiveWidgetsBar` event chips; the 15 new classes in all 3 theme files.
5. **T5 — Reminder UI + docs.** `ReminderBuilderModal.tsx` (shared duration control + text field + toast on `Notice`); `MessageInput` builder wiring for `event`/`reminder`; `MyReminders.tsx` settings section (reusing `.organizer-*`) + `SettingsModal` nav entry; `BotsTab` kind selector entries + hint lines; docs — `tauri-commands.md`, `tauri-bridge.md`, `frontend-context.md`, module docs for `channel_events.rs`/`reminders.rs` + the `bots.rs` system-identity section + the `widgets.rs` sweeper update, `ARCHITECTURE.md` module list, and the rung-2 E2EE matrix rows.

## Carry-forward / known limitations

- **Giveaway winner DM is now unblocked** — with the system identity durable, `send_system_dm(state, &winner, …)` after the draw's broadcast step is a two-line follow-on. **Deliberately not built here.**
- Attendee lists are capped at 10 rendered names per option; a "see all attendees" expansion would need a new (channel-visibility-gated) read request.
- No recurring events / calendar export / timezone picker / +1s / reminder snooze (§Non-goals) — all additive: recurrence is a `recurrence TEXT` column plus a sweeper re-seed, snooze is a `CancelReminder`-style re-create.
- 15 s sweep granularity: reminders and start announcements land up to 15 s late (invisible at real durations).
- At-most-once delivery: a crash in the window between the guarded persist and the DM loses that one notification (§5.1) — the durable state (card flipped, reminder `sent`) is still correct.
- Reminder DMs and event notifications are encrypted with a **server-held** key (the existing bot-DM trust class), not with the sender's own device key.
- Old client binaries cannot decode the appended protocol variants (project-wide property of every protocol addition; client+server ship together).
