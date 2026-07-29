# Mesh Rung 2 — Sub-project 3: Server Ingest + Delivery Duties + Legacy-Path Lockdown — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the server the blind Delivery Service the spec describes: **exactly one** server-side path may write a message row into an E2EE channel, and it accepts only sealed content derived from a signature-verified `MessagePostedE2ee`/`MessageEditedE2ee`. Every other writer — legacy `SendMessage`, edits, reactions, threads, slash commands (all six kinds), webhooks, polls, giveaways, **event cards and their sweeper announcements**, reminders, bot/system DMs, `FetchUrl` auto-attach — is refused at the request layer *and* backstopped at the choke point. On top of that: ingest of the ten Rung-2 log variants with per-variant size caps, atomic channel creation from `ChannelCreated`, a distinct `stale-epoch` bounce, a tombstone-aware derive/reconcile so deletions cannot resurrect, reply `EventRef` → derived-row-id mapping, FTS skip, the new fetch surfaces, a fail-closed protocol version gate, and **one observation test per content-producing path** proving the bytes that actually land are ciphertext.

**Architecture:** Additive, fail-closed, compile-forced. `crates/farder-server/src/channel_class.rs` (new) owns class resolution from a `channels.content_class` column written **inside the same transaction that accepts `ChannelCreated`** — so every writer can resolve the class from the `&Connection` it already holds, with no `ServerState` threading and no nested lock. `messages.rs` becomes the choke point: the raw `INSERT INTO messages` SQL moves into one private `insert_row`, every public plaintext door calls `require_plaintext_channel` first, and the one sealed door (`insert_sealed_row`) is `pub(crate)` and reachable only from `event_ingest`'s derive path. `handlers.rs`/`connection.rs` gain class-aware request-layer refusals. `event_ingest.rs` grows the Rung-2 derive/reconcile. `farder-protocol` gains new (dormant) request/response/event variants — new variants only, never a mutated struct.

**Tech Stack:** Rust, `rusqlite`, `rmp_serde`, `farder-crypto` (`event_log`, `event_log_state`), `farder-protocol`, `farder-server`. **No new dependencies, and the server never links OpenMLS** — its MLS knowledge stays opaque bytes plus fold-validated declared fields.

**Spec:** `docs/superpowers/specs/2026-07-27-mesh-rung2-e2ee-design.md` (rev 2), sub-project 3. Precedent for style: `docs/superpowers/plans/2026-06-25-mesh-rung1-sub3a-server-ingest.md` (Rung 1's equivalent).

**Baselines measured on `main` @ `c3f531e` (2026-07-29):** `cargo test -p farder-crypto` = **123 passed**; `cargo test -p farder-server` = **409 passed** (+5 in `tests/relay_mode.rs`); `cargo test --workspace` = **20/20 binaries**.

---

## Authority note: plan against the code, not the spec's prose

The spec predates both sub-2's landed fold and the events/reminders feature that
merged 2026-07-28. Where they disagree, **the code wins**. Specifically:

1. **The fold's real API** (`crates/farder-crypto/src/event_log_state.rs`) — this
   sub-project consumes, and must not re-implement:
   `LogState::{apply, replay, from_genesis, channel_class, is_tombstoned,
   is_device_revoked, log_pos, mls_current_epoch, pending_removals, pending_adds,
   pending_confirmations, leaves_confirmed, commit_discharges_drift,
   compare_same_epoch_commits, live_devices, attachment_uploader,
   is_attachment_redacted}`. `apply` keeps its `Result<()>` signature.
2. **The real variant shapes** (`event_log.rs`) — note the fields the spec's
   listing does **not** have: `MlsCommit` carries a **third** chaining field
   `post_epoch_authenticator: [u8;32]` (sub-2 resolved ambiguity #1), and
   `MlsGroupReset` carries `post_tree_hash: [u8;32]` (round-3 minor (c)).
   `AuthzBeacon` is **not** an `EventPayload` — it is a sealed application
   message, sub-4/5's business. There are exactly **10** new variants:
   `ChannelCreated`, `MlsKeyPackagePublished`, `MlsCommit`, `MlsWelcome`,
   `MlsLeafConfirmed`, `MlsGroupReset`, `MessagePostedE2ee`,
   `MessageEditedE2ee`, `MessageDeleted`, `DeviceRevoked`.
3. **Late semantic changes the fold already enforces** (`docs/modules/crypto.md`
   + sub-2 plan rounds 3/4) that ingest must NOT duplicate or contradict:
   *derived* reset completeness (`reset_incomplete()` is a pure predicate, not a
   latch); reset obligations pruned **by voidness**, never by absence; the
   **scaled** `commit_rate_gap() = min(4, distinct confirmed-leaf identities)`
   with the `COMMIT_RATE_CEILING_GRACE_EVENTS = 50` **ceiling override**; the
   reset tree-hash anchor scoped to `reset_welcomed` **and** the declared
   anchor. Ingest adds *only* what the fold deliberately omits (below).
4. **What ingest owns, per `crypto.md`'s stated division** (sub-2 ambiguity #9 +
   rounds 1–4 residuals):
   - per-variant **byte size caps** (the fold checks none);
   - the **`stale-epoch` bounce** (the fold accepts a stale commit as a no-op);
   - **target authorship + existence** for `MessageEditedE2ee` and
     `MessageDeleted { reason: Author }`, verified against the derived
     `messages` view (the fold has no per-message index by design; without this
     the fold's `tombstones` set is bounded only by ingest);
   - a **bound on `core.timestamp` against server time** — round-2's explicitly
     stated residual ("closing those needs a bound on `core.timestamp` against
     server time, which is ingest's (sub-3) job", `crypto.md:515-529`);
   - the `messages`-table emptiness belt-and-braces on `ChannelCreated`.

## Q8 is decided: FRESH SERVERS ONLY

No migration path is built. A pre-Rung-1 server has no genesis, `log_state` is
`None`, and `SubmitEvent`'s existing "server is not running the event log (no
genesis yet)" rejection (`handlers.rs:2017`) stands as the only answer — so
E2EE is structurally unavailable there and nothing in this plan changes that.
Legacy channels (DB rows with no `ChannelCreated`) are **permanently
plaintext-class** per sub-2's carve-out, and the fold already refuses a
`ChannelCreated` for any channel it has seen plaintext in
(`plaintext_history_channels`).

## THE COMPLETE WRITER INVENTORY (enumerated from the code, not from the spec)

Every non-test call site that can put a row in `messages`, as of `c3f531e`.
The spec's C8 list is **stale**: it predates event cards, reminders and the
system identity. Each row names its gate. This table is the acceptance
criterion for Tasks 1–2 and the observation suite in Task 6.

| # | Writer | Source | Gate |
|---|---|---|---|
| 1 | legacy `SendMessage` | `handlers.rs:507` | request-layer refusal (T2) + choke point (T1) |
| 2 | `EditMessage` → `messages::edit_message` | `handlers.rs:537,554` | request-layer refusal (T2) + choke point on the UPDATE (T1) |
| 3 | slash command `text`/`api` reply | `connection.rs:1359` | request-layer refusal on `RunCommand` (T2) + choke point |
| 4 | incoming webhook | `webhooks.rs:185` (`deliver`) | webhook-create refusal + delivery refusal (T2) + choke point |
| 5 | poll card | `polls.rs:326` (`create_poll_card`) | `RunCommand` kind `poll` refusal (T2) + choke point |
| 6 | giveaway card | `giveaways.rs:398` (`create_giveaway_card`) | `RunCommand` kind `giveaway` refusal (T2) + choke point |
| 7 | giveaway **sweeper** announcement | `giveaways.rs:289` (`insert_announcement` ← `close_and_draw`) | **no request layer exists** — choke point only (T1), sweeper skips gracefully (T2) |
| 8 | **event card** (NEW since the spec) | `channel_events.rs:604` (`create_event_card`) | `RunCommand` kind `event` refusal (T2) + choke point |
| 9 | **event start announcement** (NEW) | `channel_events.rs:646` (`start_and_announce`) | **no request layer** — choke point only (T1), sweeper skips gracefully (T2) |
| 10 | **event cancel-notify** (NEW) | `widgets.rs:190-210` | writes **no** message row (DM-only) — proven by test, not assumed (T6) |
| 11 | **personal reminders** (NEW) | `reminders.rs` + `widgets.rs:88-111` | writes **no** message row; `/remind` refused in E2EE channels because `reminders.text` is server-side plaintext (T2) |
| 12 | bot alert DM | `bots.rs:571` (`send_bot_dm`) | DM channels only; DM channels can never be E2EE-class — proven (T6) + choke point |
| 13 | **system identity DM** (NEW) | `bots.rs:587,658` (`send_bot_dm_as` / `send_system_dm`) | same as #12 |
| 14 | `FetchUrl` auto-attach | `connection.rs:907` → `handle_fetch_url:317` | request-layer refusal (T2); the fetched blob never becomes a row, but the *client* fallback that would attach it is refused server-side |
| 15 | `CreateThread` | `handlers.rs:1404` | request-layer refusal under an E2EE parent (T2) |
| 16 | reactions | `handlers.rs:1431,1457` | request-layer refusal (T2) — `ReactionAdded` would tell the host who reacted with what to which sealed message |
| 17 | **the log derive path** | `event_ingest.rs:106` (`derive_message_row`) | **THE choke point's one permitted E2EE door** — extended in T4 |
| 18 | `retention`/`anonymize`/`delete` | `messages.rs:375,421,259` | not writers of new content; must keep working **on ciphertext** (T4) |

**Also gated, though they write no row:** every widget interaction request
(`VotePoll`, `RetractVote`, `ClosePoll`, `EnterGiveaway`, `LeaveGiveaway`,
`CancelGiveaway`, `RerollGiveaway`, `RsvpEvent`, `ClearRsvp`, `CancelEvent`,
`EditEvent`, `GetPoll`, `GetGiveaway`, `GetEvent`, `ListActiveWidgets`). No
widget can exist in an E2EE channel once #5/#6/#8 are refused, so these are
unreachable in practice — they are gated anyway (defence in depth) and MUST
keep returning the existing **opaque** not-found errors so an id is never an
existence oracle (`EVENT_NOT_FOUND`, `handlers.rs:388`).

## Resolved ambiguities (decisions for this sub-project — source of truth)

1. **Class resolution reads a DB column, not the in-memory fold.** The writers
   above hold a `&Connection` (often a `&Transaction` inside the DB mutex) and
   have no access to `state.log_state`; reaching for it would nest two mutexes
   in an order nothing else uses, and the sweeper (`widgets::sweep_once(conn,
   now)`) has no `state` at all. So `ChannelCreated` ingest writes
   `channels.content_class` **in the same transaction** that accepts the event,
   and `channel_class::resolve(conn, channel_id)` is a pure DB read. The log
   stays the authority: the column is *derived* from an accepted event, and
   startup re-derives/verifies it (`reconcile_channel_classes`). Any
   disagreement between the column and the fold is **unresolvable ⇒ refuse**.
2. **Fail-closed resolution, exactly.** `resolve` returns
   `Plaintext | E2ee | Unresolvable`, and every writer treats `Unresolvable`
   **as E2ee** (refuse). The mapping:
   - `content_class = 'e2ee'` ⇒ `E2ee`.
   - `content_class = 'plaintext'` ⇒ `Plaintext` (this covers both a declared
     plaintext channel and every legacy channel, which default to `'plaintext'`
     — Q8's carve-out).
   - channel row **missing**, `content_class` unrecognised, or the query
     **errors** ⇒ `Unresolvable` ⇒ refuse. There is no branch in which absence
     of information yields a plaintext write.
3. **The choke point is one private function.** All raw `INSERT INTO messages`
   SQL collapses into `messages::insert_row` (private). The public doors are
   `insert_message`, `insert_message_with_ts`, `insert_message_with_author_name`
   (all call `require_plaintext_channel` first) and `insert_sealed_row`
   (`pub(crate)`, the only E2EE door, callable only from `event_ingest`).
   `edit_message` gets the same guard. A source-level test asserts no other file
   contains `INSERT INTO messages` — so a *new* writer added later trips a test,
   not production.
4. **Ingest size caps live in `farder-crypto::event_log`.** They are wire
   constants both halves need: the client must not exceed them, the server
   enforces them, and the cross-crate observation test that measures real
   OpenMLS framing overhead lives in `crates/farder-mls/tests/` — and
   `farder-crypto` is the only crate both `farder-mls` and `farder-server`
   depend on. They are **constants, not fold rules**: `LogState::apply` never
   reads them (sub-2 ambiguity #9 stands).
5. **The 40 KiB ciphertext cap is measured, not assumed.** `PADDING_BUCKETS`'
   top entry is 40960 bytes of **plaintext**; the MLS `PrivateMessage` that
   seals it is larger. A literal 40960-byte ciphertext cap would hard-bounce a
   legal maximum message — the exact bug rev 2 fixed when it raised the cap
   from 16 KiB. So `MAX_E2EE_CIPHERTEXT_BYTES` is set with framing headroom and
   **pinned by a real-OpenMLS test** that seals a top-bucket envelope and
   asserts the sealed bytes fit (Task 3, Step 1).
6. **`ChannelCreated` shape accepted this rung: `kind == "text"`, `parent:
   None`.** Threads under a sealed parent are refused by the spec (row 12) and
   categories are legacy DB state with no log representation, so any other
   shape is refused at ingest with a clear error. The fold's parent-class
   inheritance rule stays live for a later rung.
7. **`ChannelCreated.channel_id` comes from a reserved range.** It is
   client-chosen and must not collide with the `channels` AUTOINCREMENT space.
   Ingest refuses `channel_id < E2EE_CHANNEL_ID_FLOOR = 1 << 32` and refuses any
   id already present in `channels` (or in `plaintext_history_channels` — the
   fold refuses that one first). Ids are `u64` end to end; the resulting
   `sqlite_sequence` bump is harmless and documented.
8. **Old-client behavior is omission, not "listed but not enterable".** The
   spec asks for E2EE channels to be *listed* to old clients with upgrade copy —
   but `ChannelInfo` cannot gain a class field without breaking every
   un-updated client's decode of **plaintext** channels too (the M2 rule).
   Listing an E2EE channel to a v1 client therefore hands it a normal-looking
   channel with a working composer, which is worse than hiding it. Fail-closed
   choice: **v1 connections never see E2EE channels** in `ServerInfo`, and every
   v1 request naming one is refused with "this channel requires a newer client".
   v2 connections get the full picture via `ChannelInfoV2`. The "listed with
   upgrade copy" affordance is a client-side (sub-4) concern once both ends
   speak v2.
9. **Everything protocol-shaped lands DORMANT.** Per spec F15, sub-3 ships all
   remaining protocol churn (new request/response/event variants, the version
   handshake) so sub-4/5/6/7 are behavior-only. No client code is written here;
   `cargo test --workspace` staying green **is** the dormancy evidence.

## Global constraints

- **Content-blind server.** Ingest validates signatures, authz (via the fold),
  caps, ordering and cap-vs-blob metadata. It never inspects plaintext.
  Moderation works on event hashes and content hashes only.
- **Fail closed, everywhere.** Unresolvable class ⇒ refuse. Unknown protocol
  version ⇒ no E2EE. Unknown target ⇒ refuse. Missing tombstone knowledge at
  reconcile ⇒ do not re-derive.
- **Default-deny request gating stays.** No new `ServerRequest` is added to
  `request_requires_membership`'s bootstrap allow-list except
  `NegotiateProtocol` (a deliberate, tested act — an unauthenticated-but-
  connected client must be able to state its version before anything else).
  The actor is always the authenticated connection key, never a request field.
- **Opaque errors.** Class refusals return one byte-identical string per family
  so a channel id is not an existence oracle; widget arms keep their existing
  opaque not-found strings.
- **No DB `Mutex` across `.await`.** Every new async touch point follows the
  established pattern: DB work in a scoped block, guard dropped, then broadcast.
- **Persist before broadcast; ingest is atomic.** All new derive work rides
  inside the existing `conn.unchecked_transaction()` in the `SubmitEvent` arm
  (`handlers.rs:2054-2071`), and the in-memory `LogState` advance commits only
  after `tx.commit()`.
- **Never mutate a shipped struct or variant** (spec M2). `MessageInfo`,
  `ChannelInfo`, `ServerRequest`'s existing variants, `ServerEvent`'s existing
  variants are frozen. New data rides new variants.
- **Docs in the same commit** (CLAUDE.md): `docs/modules/server-handlers.md`,
  `server-connection.md`, `server-widgets.md`, `server-system-identity.md`,
  `protocol.md`, `crypto.md`, and `ARCHITECTURE.md` as each surface changes.

## Gates — ALL green before EVERY commit; never commit red

```bash
cargo test -p farder-crypto          # 123 baseline
cargo test -p farder-server          # 409 baseline (+5 relay_mode)
cargo test --workspace               # 20/20 binaries — also the DORMANCY evidence
cargo build --workspace
cd client/src-tauri && cargo build   # ONLY if farder-protocol changed (NOT a workspace member)
cargo clippy -p farder-mls -- -D warnings
```

Conventional commits: `feat(e2ee)` / `fix(e2ee)`. Commit locally on
`mesh-rung2-sub3-server-ingest`. **NEVER push.**

## File structure

- **Create** `crates/farder-server/src/channel_class.rs` — class resolution +
  the `ChannelCreated` → `channels` row materialization + startup reconcile.
- **Modify** `crates/farder-server/src/messages.rs` — the choke point.
- **Modify** `crates/farder-server/src/db.rs` — `channels.content_class`,
  `messages.is_e2ee`, `messages.sealed`, unique index on `messages.event_hash`.
- **Modify** `crates/farder-server/src/handlers.rs` — request-layer refusals,
  `SubmitEvent` arm extension, caps, `stale-epoch`, new fetch surfaces.
- **Modify** `crates/farder-server/src/connection.rs` — `RunCommand` (all six
  kinds) + `FetchUrl` refusals, per-connection protocol version, broadcast filter.
- **Modify** `crates/farder-server/src/webhooks.rs` — `deliver` refusal.
- **Modify** `crates/farder-server/src/widgets.rs` — sweeper skips E2EE channels.
- **Modify** `crates/farder-server/src/event_ingest.rs` — Rung-2 derive/reconcile.
- **Modify** `crates/farder-server/src/state.rs` — `client_protocol` map.
- **Modify** `crates/farder-crypto/src/event_log.rs` — ingest cap constants.
- **Modify** `crates/farder-protocol/src/server.rs` — new dormant variants.
- **Create** `crates/farder-server/tests/e2ee_observation.rs` — Task 6.
- **Create** `crates/farder-mls/tests/ciphertext_cap.rs` — Task 3 Step 1.

---

## Task 1: The choke point + fail-closed class resolution

**Spec:** C8/F1 requirement 1 — "One server-side choke point. … Not the config
UI. Not the handler. The function every path funnels through."

**Files:** create `crates/farder-server/src/channel_class.rs`; modify
`crates/farder-server/src/{db.rs,messages.rs,lib.rs}`;
`docs/modules/server-handlers.md`.

**Interfaces produced:**
- `channel_class::{ChannelWriteClass, resolve, require_plaintext, set_class, reconcile_channel_classes}`
- `messages::insert_sealed_row(conn, …) -> Result<u64>` (`pub(crate)`)
- every existing `messages::insert_*` / `edit_message` signature **unchanged**,
  now hard-erroring for E2EE channels.

- [x] **Step 1: Write the failing tests**

In `crates/farder-server/src/channel_class.rs`'s `#[cfg(test)] mod tests` and
`messages.rs`'s test module. Spec invariants as names:

- `a_channel_whose_class_cannot_be_resolved_is_treated_as_encrypted` — a
  `channel_id` with no `channels` row, and one with a garbage
  `content_class` value, both resolve `Unresolvable`, and `require_plaintext`
  errors for both.
- `a_legacy_channel_absent_from_the_log_is_plaintext_class` — a channel created
  by `channels::create_channel` (no `ChannelCreated` anywhere) resolves
  `Plaintext`; this is Q8's carve-out and MUST NOT regress.
- `insert_message_hard_errors_in_an_e2ee_channel`
- `insert_message_with_ts_hard_errors_in_an_e2ee_channel`
- `insert_message_with_author_name_hard_errors_in_an_e2ee_channel`
- `edit_message_hard_errors_in_an_e2ee_channel`
- `insert_sealed_row_is_the_only_writer_that_reaches_an_e2ee_channel` — the
  sealed door succeeds where all four plaintext doors error, and *itself* errors
  in a plaintext channel (the door is not a general bypass).
- `no_insert_into_messages_sql_outside_the_choke_point` — walks
  `crates/farder-server/src` (via `std::fs`, skipping `messages.rs` and any
  `#[cfg(test)]`-only fixture files) and asserts no other file contains the
  string `INSERT INTO messages`. This is what makes a *future* writer a test
  failure instead of a breach.

- [x] **Step 2: Schema — `channels.content_class`**

In `db.rs`, add to the `channels` CREATE (harmless for existing DBs) and add an
idempotent migration in the same PRAGMA-table_info style used for
`messages.event_hash` (`db.rs:392-400`):

```rust
// Rung 2: the channel's content class, DERIVED from an accepted `ChannelCreated`
// inside the same transaction. Legacy rows default to 'plaintext' (Q8 carve-out).
// A missing row or an unrecognised value is UNRESOLVABLE => refuse (fail closed).
let has_content_class: bool = {
    let mut stmt = conn.prepare("PRAGMA table_info(channels)")?;
    let cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();
    cols.iter().any(|c| c == "content_class")
};
if !has_content_class {
    conn.execute(
        "ALTER TABLE channels ADD COLUMN content_class TEXT NOT NULL DEFAULT 'plaintext'",
        [],
    )?;
}
```

- [x] **Step 3: `channel_class.rs`**

```rust
//! The channel content class as the SERVER sees it, and the fail-closed rule
//! every message writer funnels through (spec rev 2, C8/F1).
//!
//! The class is a property of the channel's identity in the LOG
//! (`EventPayload::ChannelCreated { class }`). It is mirrored into
//! `channels.content_class` inside the SAME transaction that accepts the event,
//! so a writer holding only a `&Connection` (or a `&Transaction`, or the
//! sweeper, which has no `ServerState` at all) can resolve it without reaching
//! across the log-state mutex. Startup re-derives and verifies the mirror.
//!
//! FAIL CLOSED: anything that is not a definite 'plaintext' is refused. There is
//! no branch in which missing information yields a plaintext write.

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};

use farder_crypto::event_log::ChannelClass;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelWriteClass {
    /// Declared plaintext, or a legacy channel the log never knew (Q8 carve-out).
    Plaintext,
    /// Declared `ChannelClass::E2ee` — server-authored content is forbidden.
    E2ee,
    /// No row, unrecognised value, or a failed read. TREATED AS ENCRYPTED.
    Unresolvable,
}

impl ChannelWriteClass {
    /// The single predicate every writer asks. `Unresolvable` answers `true`:
    /// a class we cannot determine is encrypted, never plaintext.
    pub fn refuses_server_authored_content(self) -> bool {
        !matches!(self, ChannelWriteClass::Plaintext)
    }
}

/// The ONE refusal string for every class-based rejection, byte-identical so a
/// channel id never becomes an existence oracle.
pub const E2EE_REFUSED: &str = "not available in encrypted channels";

pub fn resolve(conn: &Connection, channel_id: u64) -> ChannelWriteClass {
    let row: Option<String> = conn
        .query_row(
            "SELECT content_class FROM channels WHERE id = ?1",
            params![channel_id as i64],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None); // a failed read is UNRESOLVABLE, never plaintext
    match row.as_deref() {
        Some("plaintext") => ChannelWriteClass::Plaintext,
        Some("e2ee") => ChannelWriteClass::E2ee,
        _ => ChannelWriteClass::Unresolvable,
    }
}

/// The choke point's guard: `Ok(())` only for a definitely-plaintext channel.
pub fn require_plaintext(conn: &Connection, channel_id: u64) -> Result<()> {
    if resolve(conn, channel_id).refuses_server_authored_content() {
        bail!("{E2EE_REFUSED} (channel {channel_id})");
    }
    Ok(())
}

/// Mirror an accepted `ChannelCreated` class onto the channel row. Called ONLY
/// from `event_ingest` inside the ingest transaction (Task 3).
pub fn set_class(conn: &Connection, channel_id: u64, class: ChannelClass) -> Result<()> {
    let s = match class {
        ChannelClass::Plaintext => "plaintext",
        ChannelClass::E2ee => "e2ee",
    };
    conn.execute(
        "UPDATE channels SET content_class = ?2 WHERE id = ?1",
        params![channel_id as i64, s],
    )?;
    Ok(())
}
```

`reconcile_channel_classes` lands in Task 3 (it needs the events table read).
Register the module in `lib.rs`.

- [x] **Step 4: The choke point in `messages.rs`**

Collapse the three raw inserts into one private `insert_row`, then guard:

```rust
/// The ONE statement in the entire server that inserts a `messages` row.
/// Every public door funnels through here; the class guard lives on the doors so
/// the one legitimate E2EE door (`insert_sealed_row`) can bypass it explicitly.
/// Pinned by `no_insert_into_messages_sql_outside_the_choke_point`.
#[allow(clippy::too_many_arguments)]
fn insert_row(
    conn: &Connection,
    channel_id: u64,
    author: &PublicKey,
    content: &str,
    timestamp: u64,
    reply_to: Option<u64>,
    author_name_override: Option<&str>,
    author_badge: Option<&str>,
    event_hash: Option<&str>,
    sealed: Option<&[u8]>,
    is_e2ee: bool,
) -> Result<u64> { /* single INSERT + conditional FTS insert (skipped when is_e2ee) */ }
```

- `insert_message` / `insert_message_with_ts` / `insert_message_with_author_name`
  each begin with `crate::channel_class::require_plaintext(conn, channel_id)?;`
  — **signatures unchanged**, so none of the 409 existing tests or the ~18
  writers need editing, and every one of them is gated by construction.
- `edit_message` resolves the channel from the row it is about to update and
  applies the same guard.
- New, `pub(crate)`:

```rust
/// The ONLY door into an E2EE channel. Callable solely from `event_ingest`'s
/// derive path, i.e. only for a row derived from a signature-verified
/// `MessagePostedE2ee` the fold accepted. Refuses a PLAINTEXT channel too — it
/// is the sealed door, not a general bypass.
pub(crate) fn insert_sealed_row(
    conn: &Connection,
    channel_id: u64,
    author: &PublicKey,
    sealed: &[u8],
    timestamp: u64,
    reply_to: Option<u64>,
    event_hash: &str,
) -> Result<u64>
```

`content` is stored as `''` for sealed rows and the FTS insert is skipped
entirely — nothing plaintext-shaped is ever written, so there is nothing for a
future `content`-reading feature to leak. (`messages.sealed` / `is_e2ee` columns
land in Task 4; for this task `insert_sealed_row` can be written against them
by adding the two columns here — do that, it keeps Task 4 to behavior.)

- [x] **Step 5: Run the gates**

```bash
cargo test -p farder-server        # 409 + the new tests, all green
cargo test --workspace
cargo build --workspace
cargo clippy -p farder-mls -- -D warnings
```

- [x] **Step 6: Docs + commit**

`docs/modules/server-handlers.md`: a new "Channel content class + the message
choke point" section (the resolution table, `E2EE_REFUSED`, the four doors, the
source-level guard test). `ARCHITECTURE.md`: note that `messages.rs` is the
single message-write choke point.

```bash
git add -A && git commit -m "feat(e2ee): single message-write choke point + fail-closed channel-class resolution"
```

---

## Task 2: Request-layer refusals for every enumerated writer

**Spec:** C8/F1 requirement 2 + Coexistence rows 3, 4, 5, 6b, 10, 11, 12, 15,
**19 (event cards)** and **20 (reminders)**. Every "refused" verdict names a
**server-side** enforcement point, never a UI one.

**Files:** `handlers.rs`, `connection.rs`, `webhooks.rs`, `widgets.rs`;
`docs/modules/{server-handlers,server-connection,server-widgets,server-system-identity}.md`.

- [ ] **Step 1: Write the failing tests** (all in `handlers.rs` tests except
  where noted; each drives the REAL request path against an E2EE channel and
  asserts the refusal **and** that `SELECT COUNT(*) FROM messages WHERE
  channel_id = ?` is still 0):

- `send_message_is_refused_in_an_e2ee_channel`
- `edit_message_is_refused_in_an_e2ee_channel`
- `add_and_remove_reaction_are_refused_in_an_e2ee_channel`
- `thread_create_is_refused_under_an_e2ee_parent`
- `fetch_url_is_refused_in_an_e2ee_channel` (in `connection.rs` tests, against
  `handle_fetch_url`, which returns `Result<u64, String>`)
- `every_run_command_kind_is_refused_in_an_e2ee_channel` — a table test over all
  six kinds: `text`, `api`, `poll`, `giveaway`, **`event`**, **`reminder`**.
  The `reminder` case is the one the spec's C8 list has no idea exists:
  `reminders.text` is stored server-side in plaintext, so a `/remind` set inside
  a sealed channel hands the host content the channel promises to hide — refused
  even though it posts nothing.
- `webhook_create_is_refused_for_an_e2ee_channel`
- `webhook_delivery_into_an_e2ee_channel_is_refused_and_writes_nothing` (in
  `webhooks.rs` tests, driving `deliver`; asserts the ack is a refusal, not a
  silent success)
- `widget_interactions_in_an_e2ee_channel_refuse_without_an_existence_oracle` —
  every widget request returns the **existing opaque** not-found string, not a
  distinguishable "encrypted channel" message.
- `the_giveaway_sweeper_cannot_announce_into_an_e2ee_channel` (in `widgets.rs`
  tests, via `sweep_once`)
- `the_event_sweeper_cannot_announce_into_an_e2ee_channel` (ditto — the start
  pass; assert the status flip either does not happen or happens with no message
  row, and that the sweeper **continues** to the next row rather than aborting
  the tick)
- `no_new_request_variant_escapes_default_deny_request_requires_membership` —
  asserts `request_requires_membership` is `true` for every new variant except
  the one deliberate exception (`NegotiateProtocol`, Task 5). Write it now with
  the variants that exist; extend in Task 5.

- [ ] **Step 2: Implement the refusals**

A shared helper in `handlers.rs`:

```rust
/// Class gate for every request that would produce server-readable content in a
/// channel. Returns `Some(denied)` to be propagated, `None` to proceed.
/// FAIL CLOSED: an unresolvable class refuses.
fn require_plaintext_channel(conn: &Connection, channel_id: u64) -> Option<Result<HandleResult>> {
    if crate::channel_class::resolve(conn, channel_id).refuses_server_authored_content() {
        return Some(err(crate::channel_class::E2EE_REFUSED));
    }
    None
}
```

Applied at:
- `SendMessage` (`handlers.rs:469`) — immediately after the timeout check, before
  any allocation-heavy work.
- `EditMessage` (`:537`) — resolve via `msg.channel_id`.
- `AddReaction` (`:1431`) / `RemoveReaction` (`:1457`) — via `msg.channel_id`.
- `CreateThread` (`:1404`) — via `msg.channel_id` (the parent). A thread under a
  sealed parent would be a plaintext thread beneath a sealed message.
- `CreateWebhook` (`:2233`).
- Every widget request arm — but returning that arm's **existing** opaque
  not-found string, not `E2EE_REFUSED`.
- `RunCommand` in `connection.rs:935` — one check right after
  `check_run_command_channel_auth` (so it covers all six kinds at once, before
  any parse, fetch or DB write).
- `FetchUrl` in `handle_fetch_url` (`connection.rs:317`) — beside the existing
  `SEND_MESSAGES` check, before the outbound HTTP fetch (so an E2EE channel
  cannot even be used to make the server fetch a URL).
- `webhooks::deliver` (`:185`) — after `find_by_token`, returning
  `WebhookAck::Unauthorized` (the existing opaque ack; a distinct "encrypted"
  ack would let an external prober classify channels).
- `widgets::sweep_once` — both announcement paths check the class before the
  guarded UPDATE and `continue` on refusal, logging at `warn!`. **The tick must
  not abort**: an E2EE channel that somehow held a widget must not stop the
  sweeper servicing every other channel.

- [ ] **Step 3: Gates + docs + commit**

Update the four module docs (`server-handlers.md` request table gains a "class"
column note; `server-widgets.md` records the sweeper skip; `server-connection.md`
records the `RunCommand`/`FetchUrl` gates; `server-system-identity.md` records
that the system identity can only ever write into DM channels, which are never
E2EE-class).

```bash
git commit -m "feat(e2ee): class-aware request-layer refusals for every server-authored write path"
```

---

## Task 3: Ingest — `ChannelCreated` atomicity, per-variant size caps, `stale-epoch`, timestamp bound

**Spec:** "Server changes" + "Size caps" (M4/F8) + Ordering ("stale-epoch as a
distinct error code") + the `messages`-table emptiness check + `crypto.md`'s
stated sub-3 residual (bound `core.timestamp` against server time).

**Files:** `crates/farder-crypto/src/event_log.rs` (constants),
`crates/farder-mls/tests/ciphertext_cap.rs` (new), `event_ingest.rs`,
`handlers.rs` (`SubmitEvent` arm), `channel_class.rs`, `main.rs`;
`docs/modules/{crypto,server-handlers}.md`.

- [ ] **Step 1: Measure the real ciphertext cap, then set it**

Create `crates/farder-mls/tests/ciphertext_cap.rs`: build a real two-member
group with `MlsChannelGroup`, seal a `MessageEnvelope` whose encoded length sits
just under `MAX_PRESEAL_BYTES` (so `pad_to_bucket` lands on the top 40960-byte
bucket), and assert:

```rust
assert!(sealed.len() <= farder_crypto::event_log::MAX_E2EE_CIPHERTEXT_BYTES,
    "a legal maximum-size message must fit the ingest cap: {} > {}",
    sealed.len(), farder_crypto::event_log::MAX_E2EE_CIPHERTEXT_BYTES);
assert!(sealed.len() > farder_mls::PADDING_BUCKETS[4],
    "the cap must include MLS framing overhead, not just the padding bucket");
```

Then add to `crates/farder-crypto/src/event_log.rs`:

```rust
// ---- Rung-2 INGEST caps (spec "Size caps", M4/F8) ------------------------
// These are wire constants, NOT fold rules: `LogState::apply` never reads them
// (sub-2 resolved ambiguity #9). The server enforces them at ingest before any
// allocation-heavy work; the client must not exceed them.

/// Sealed application-message cap. The spec's "40 KiB" is the top PADDING
/// bucket, which is PLAINTEXT; the MLS PrivateMessage that seals it is larger,
/// so a literal 40960 cap would hard-bounce a legal maximum message (exactly the
/// bug rev 2 fixed when it raised the cap from 16 KiB). Framing headroom is
/// included and pinned by `farder-mls/tests/ciphertext_cap.rs`.
pub const MAX_E2EE_CIPHERTEXT_BYTES: usize = 45 * 1024;
pub const MAX_MLS_MESSAGE_BYTES: usize = 256 * 1024;
pub const MAX_MLS_WELCOME_BYTES: usize = 256 * 1024;
pub const MAX_KEY_PACKAGE_BYTES: usize = 8 * 1024;
/// Bounds a commit's declared-leaf vectors and a reset's welcome list, so a
/// single event cannot force an unbounded fold walk before it is rejected.
pub const MAX_DECLARED_LEAVES_PER_COMMIT: usize = 256;
pub const MAX_RESET_WELCOMES: usize = 256;
pub const MAX_E2EE_ATTACHMENTS: usize = 10;
/// `ChannelCreated.channel_id` is client-chosen; this floor keeps it clear of
/// the `channels` AUTOINCREMENT space.
pub const E2EE_CHANNEL_ID_FLOOR: u64 = 1 << 32;
/// Ingest refuses an event claiming a timestamp more than this far ahead of
/// server time — the bound `crypto.md` names as sub-3's job, closing round 2's
/// stated residual (a forward-dated claim feeding `corroborated_clock`).
pub const MAX_EVENT_FUTURE_SKEW_SECS: u64 = 300;
```

- [ ] **Step 2: Write the failing ingest tests** (`handlers.rs` tests, driving
  the real `SubmitEvent` arm):

- `oversized_sealed_ciphertext_is_refused_before_the_fold_runs` — a
  `MessagePostedE2ee` one byte over the cap is refused, **and** the in-memory
  `LogState.log_pos()` is unchanged (proving the cap ran before `apply`).
- `oversized_commit_welcome_and_key_package_are_refused` — table test over the
  three byte caps + the two vector caps.
- `a_stale_epoch_commit_is_bounced_with_the_stale_epoch_code` — asserts the
  error string is exactly `"stale-epoch"` (a distinct, machine-readable code the
  client's resync loop keys on) and that the event is **not** stored, so the
  fold's accepted-no-op path is never reached through ingest.
- `an_event_dated_far_in_the_future_is_refused_at_ingest`
- `channel_created_materializes_the_channel_row_atomically_with_its_class` —
  after acceptance, the `channels` row exists with `content_class='e2ee'`, and
  `channel_class::resolve` agrees with `LogState::channel_class`.
- `channel_created_is_refused_for_a_channel_that_already_has_messages` — the
  belt-and-braces `messages`-table emptiness check (the fold's
  `plaintext_history_channels` refuses the fresh-replay case; this catches a
  legacy DB channel the log never saw).
- `channel_created_is_refused_below_the_reserved_id_floor_or_on_a_collision`
- `channel_created_is_refused_for_an_unsupported_shape` — `kind != "text"` or
  `parent: Some(_)`.
- `a_failed_ingest_transaction_leaves_no_channel_row_and_no_log_advance` —
  atomicity: the derived write and the `LogState` advance stand or fall together.

- [ ] **Step 3: Implement**

In `handlers.rs`'s `SubmitEvent` arm, **before** step 2's `trial.apply`, insert a
`check_ingest_caps(&event)?` pass (a free function in `event_ingest.rs`) that
matches on the payload and enforces every constant above plus
`MAX_EVENT_FUTURE_SKEW_SECS` against `crate::db::now()`. Caps first, fold second
— the fold clones `LogState`, which is the allocation-heavy step.

Then, still before `apply`, the `stale-epoch` pre-check:

```rust
// A commit that lost the epoch CAS is an accepted no-op IN THE FOLD (Rung-3
// determinism) — but ingest must bounce it with a distinct code so the author's
// client resyncs (process winner → rebuild → resubmit) instead of believing it
// landed. Spec Ordering; served by `LogState::mls_current_epoch`.
if let EventPayload::MlsCommit { channel_id, generation, epoch, .. } = &event.core.payload {
    if let Some((gen, cur)) = ls.mls_current_epoch(*channel_id) {
        if *generation != gen || *epoch != cur {
            return err("stale-epoch");
        }
    }
}
```

`ChannelCreated` handling inside the existing transaction (after `store_event`):
refuse on `channel_id < E2EE_CHANNEL_ID_FLOOR`, on an existing `channels` row,
on `SELECT 1 FROM messages WHERE channel_id = ?`, and on an unsupported shape;
otherwise `INSERT INTO channels (id, name, channel_type, position, content_class)`
with the declared id, then broadcast `ServerEvent::ChannelCreated` **only for a
Plaintext-declared channel** (the E2EE announcement needs `ChannelInfoV2`, Task
5 — until then an E2EE channel is simply not announced, which is the fail-closed
side).

`channel_class::reconcile_channel_classes(conn) -> Result<usize>`: replay
`payload_type = 'ChannelCreated'` rows from `events`, re-assert each channel's
`content_class`, and **refuse to serve** (log `error!` and mark the channel
`deleted = 1`? no — leave the row and let `resolve` return `E2ee`) on any
mismatch. Call it from `main.rs` beside `reconcile_messages`.

- [ ] **Step 4: Gates + docs + commit**

`crypto.md` gains an "Ingest caps" section (constants + the explicit note that
the fold never reads them). `server-handlers.md` documents the `SubmitEvent`
arm's new order: caps → stale-epoch → fold → transaction → broadcast.

```bash
git commit -m "feat(e2ee): ingest caps, stale-epoch bounce, timestamp bound, atomic ChannelCreated materialization"
```

---

## Task 4: Derivation — sealed rows, FTS skip, reply id mapping, tombstone-aware derive + reconcile

**Spec:** "Derivation" bullet + F2 (tombstones) + F9 (reply mapping) + row 7a
(FTS) + row 7b (retention/redaction/anonymize on ciphertext).

**Files:** `db.rs`, `event_ingest.rs`, `messages.rs`, `main.rs`;
`docs/modules/server-handlers.md`.

- [ ] **Step 1: Write the failing tests** (`event_ingest.rs` tests + `handlers.rs`
  integration tests):

- `a_sealed_post_derives_a_row_carrying_ciphertext_and_no_plaintext_column`
- `a_sealed_row_never_enters_the_fts_index` — assert `messages_fts` has no row
  for the derived id and that `search_messages` cannot surface it.
- `reply_event_hash_maps_to_the_derived_row_id` — `reply_to: Some(EventRef)`
  resolves through `messages.event_hash` to the numeric id the client renders
  threading from. This is the F9 prerequisite: E2EE channels are log-only, so
  without it replies are silently dropped (the shipped `MessageInput.tsx:283`
  TODO).
- `an_unresolvable_reply_target_derives_null_and_is_repaired_by_reconcile` —
  out-of-order arrival must not lose the edge permanently.
- `a_sealed_edit_updates_the_row_in_place_and_only_for_its_own_author` — target
  authorship is verified against the derived view (the fold has no per-message
  index by design).
- `a_deleted_message_stays_deleted_across_restart_and_reconcile` — **the F2
  invariant**: `MessageDeleted` hard-deletes the row, and `reconcile_messages`
  consults the fold's `is_tombstoned` so the next startup does not resurrect it.
  Without this, content-blind delete — the *only* moderation mechanism in an
  E2EE channel — silently undoes itself.
- `a_moderation_delete_needs_kick_and_an_author_delete_needs_authorship`
- `derived_view_rebuild_from_events_equals_the_live_view` — wipe `messages`,
  re-run `reconcile_messages`, and deep-compare against the pre-wipe view
  (including tombstoned rows staying absent and reply edges restored).
- `retention_redaction_and_anonymize_operate_on_ciphertext_rows` — the three
  existing mechanisms work unchanged against sealed rows (row 7b's
  "works-on-ciphertext server-side" claim, verified rather than asserted).

- [ ] **Step 2: Schema**

`messages.is_e2ee INTEGER NOT NULL DEFAULT 0`, `messages.sealed BLOB` (both via
the idempotent PRAGMA migration pattern; add in Task 1 if convenient), plus:

```sql
CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_event_hash ON messages(event_hash);
```

(SQLite permits many NULLs in a unique index, so legacy rows are unaffected —
and the index is what makes reply mapping and edit/delete targeting O(log n).)

- [ ] **Step 3: Implement**

- `derive_message_row` gains a `MessagePostedE2ee` arm: resolve `reply_to`
  (`SELECT id FROM messages WHERE event_hash = ?`), call
  `messages::insert_sealed_row`, skip FTS. Its existing `MessagePosted` arm is
  untouched.
- New `apply_sealed_edit(conn, event) -> Result<Option<u64>>` and
  `apply_tombstone(conn, event) -> Result<Option<u64>>`, both called from the
  ingest transaction; both resolve the target through `messages.event_hash` and
  **refuse** (rolling the transaction back) when the target is unknown — which
  is what bounds the fold's `tombstones` set.
- `reconcile_messages(conn, log_state: Option<&LogState>)` — the signature
  change is the point: it now (a) covers `MessagePostedE2ee` events, (b) skips
  any event whose hash `log_state.is_tombstoned()`, and (c) runs a
  `repair_reply_links` pass for rows whose `reply_to` was unresolvable at derive
  time. Update the one call site in `main.rs:116` (the `LogState` is built two
  lines above).
- `search_messages` adds `AND m.is_e2ee = 0` — belt-and-braces behind the FTS
  skip.

- [ ] **Step 4: Gates + docs + commit**

```bash
git commit -m "feat(e2ee): sealed derivation, FTS skip, reply id mapping, tombstone-aware derive and reconcile"
```

---

## Task 5: Fetch surfaces + fail-closed protocol version gate (all dormant)

**Spec:** "Protocol compatibility and rollout" (F3/M2) + "New fetch surfaces".
Per F15 this is the **last** protocol upheaval — one workspace build + one
separate client-crate build, not five.

**Files:** `crates/farder-protocol/src/server.rs`, `state.rs`, `connection.rs`,
`handlers.rs`; `docs/modules/protocol.md`, `server-connection.md`.

- [ ] **Step 1: Write the failing tests**

- `a_v1_connection_never_sees_an_e2ee_channel_or_a_v2_only_event` — the
  broadcast filter drops v2-only events for un-negotiated connections, and
  `GetServerInfo` omits E2EE channels for them. **This is the M2 invariant**: an
  old client that cannot decode a frame fails the *whole* frame, including in
  plaintext channels, so it must never be sent one.
- `a_v1_request_naming_an_e2ee_channel_gets_the_upgrade_error`
- `negotiate_protocol_is_the_only_new_variant_in_the_bootstrap_allow_list` —
  extends Task 2's default-deny test over every variant added here.
- `new_fetch_surfaces_are_membership_gated_and_actor_is_the_connection_key` — a
  non-member cannot fetch Welcomes/KeyPackages, and `FetchWelcomes` serves only
  the **authenticated connection key's** own Welcomes regardless of any field in
  the request (no "fetch anyone's" oracle).
- `existing_variants_are_byte_stable_after_the_additions` — round-trip the
  shipped `ServerRequest`/`ServerResponse`/`ServerEvent` variants and assert the
  encoded bytes are unchanged (the M2 append-only rule, enforced not assumed).

- [ ] **Step 2: Protocol additions (append-only, dormant)**

`ServerRequest`: `NegotiateProtocol { client_version: u32 }`,
`FetchWelcomes { channel_id: Option<u64>, since_accept_seq: u64 }`,
`FetchKeyPackages { member: PublicKey, device: String }`,
`GetServerInfoV2`, `FetchHistoryV2 { channel_id, before_id: Option<u64>, limit: u32 }`.

`ServerResponse`: `ProtocolVersion { server_version: u32, min_client_version_for_e2ee: u32 }`,
`Welcomes { events: Vec<Vec<u8>> }` (raw signed `Event` bytes — the server hands
out opaque bytes, it does not interpret MLS),
`KeyPackages { events: Vec<Vec<u8>> }`,
`ServerInfoV2 { channels: Vec<ChannelInfoV2>, … }`,
`HistoryV2 { messages: Vec<MessageInfoV2> }`.

New structs `ChannelInfoV2` (the `ChannelInfo` fields **plus** `class`) and
`MessageInfoV2` (the `MessageInfo` fields **plus** `is_e2ee`, `sealed:
Option<Vec<u8>>`, `event_hash: Option<String>`). New structs, never mutations.

`ServerEvent`: `SealedMessage { channel_id, message: MessageInfoV2 }`,
`SealedMessageEdited { … }`, `MessageTombstoned { channel_id, message_id }`,
`MlsControlEvent { channel_id: Option<u64>, event_hash: String, payload_type: String }`,
`ChannelCreatedV2 { channel: ChannelInfoV2 }`.

- [ ] **Step 3: The version gate**

`ServerState` gains `client_protocol: RwLock<HashMap<[u8;32], u32>>` (default
absent ⇒ v1). `NegotiateProtocol` records it; the broadcast path
(`connection::broadcast_event`) filters v2-only events by that map; `ServerInfo`
filters E2EE channels for v1; every request naming an E2EE channel from a v1
connection returns `"this channel requires a newer client"`.

Fail closed both directions: an *unknown/newer* client version is treated as v2
only for events it explicitly negotiated; an un-negotiated connection is v1.

- [ ] **Step 4: Gates (including the client crate — protocol changed)**

```bash
cargo test -p farder-crypto && cargo test -p farder-server && cargo test --workspace
cargo build --workspace
cd client/src-tauri && cargo build      # NOT a workspace member — required here
cargo clippy -p farder-mls -- -D warnings
```

- [ ] **Step 5: Docs + commit**

`protocol.md` gains the new catalogs and an explicit "never mutate a shipped
struct" banner; `server-connection.md` documents the per-connection version map
and the broadcast filter; `ARCHITECTURE.md` notes the v1/v2 split.

```bash
git commit -m "feat(e2ee): V2 fetch surfaces, sealed delivery events, and a fail-closed protocol version gate"
```

---

## Task 6: One observation test per content-producing path

**Spec:** the Global-constraints "Verify by observation" rule + C8/F1
requirement 5 + CLAUDE.md ("For security/privacy features, verify by
observation, not by reading code… Capture the bytes that actually leave the
process and assert they are ciphertext"). **A test that asserts a function was
called does not count.**

**Files:** create `crates/farder-server/tests/e2ee_observation.rs`.

- [ ] **Step 1: The shared observer**

```rust
/// Scan every place a byte could land and assert the plaintext needle is in
/// NONE of them: `messages.content`, `messages.sealed`, `messages_fts`,
/// `message_attachments.file_name`, `channel_events.*`, `polls.*`,
/// `giveaways.*`, `reminders.text`, and the raw `events.event_body` blobs.
/// Byte-level, not column-typed — a needle hidden in a serialized blob still trips.
fn assert_no_plaintext_anywhere(conn: &Connection, needle: &str);

/// Assert the bytes a subscriber would actually receive for this channel are
/// ciphertext: drives the real broadcast assembly and inspects the encoded frame.
fn assert_broadcast_is_ciphertext(frames: &[ServerEvent], needle: &str);
```

- [ ] **Step 2: One test per path** (each drives the REAL path, then observes):

| Path | Test |
|---|---|
| sealed send | `sealed_send_persists_and_broadcasts_only_ciphertext` |
| sealed edit | `sealed_edit_persists_and_broadcasts_only_ciphertext` |
| sealed reply | `sealed_reply_threads_by_id_without_leaking_content` |
| legacy `SendMessage` | `legacy_send_writes_nothing_into_an_e2ee_channel` |
| `EditMessage` | `legacy_edit_writes_nothing_into_an_e2ee_channel` |
| reactions | `reaction_attempt_writes_nothing_and_reveals_no_reactor` |
| threads | `thread_create_writes_no_plaintext_child_under_a_sealed_parent` |
| slash `text`/`api` | `slash_command_reply_writes_nothing_into_an_e2ee_channel` |
| webhook | `webhook_delivery_writes_nothing_into_an_e2ee_channel` |
| poll | `poll_card_writes_nothing_into_an_e2ee_channel` |
| giveaway create | `giveaway_card_writes_nothing_into_an_e2ee_channel` |
| giveaway sweeper | `giveaway_draw_announcement_writes_nothing_into_an_e2ee_channel` |
| **event card** | `event_card_writes_nothing_into_an_e2ee_channel` |
| **event start announcement** | `event_start_announcement_writes_nothing_into_an_e2ee_channel` |
| **event cancel** | `event_cancel_notify_writes_no_channel_row_at_all` |
| **reminder set** | `remind_in_an_e2ee_channel_stores_no_plaintext_reminder_text` |
| **reminder DM** | `a_due_reminder_dm_never_lands_in_the_originating_e2ee_channel` |
| bot alert DM | `bot_alert_dms_never_reach_an_e2ee_channel` |
| **system identity DM** | `the_system_identity_can_only_write_into_dm_channels` |
| `FetchUrl` | `fetch_url_stores_no_blob_for_an_e2ee_channel` |
| search / FTS | `an_e2ee_channels_content_is_absent_from_the_search_index` |
| retention / redaction | `retention_and_redaction_operate_without_ever_reading_plaintext` |
| host injection | `a_row_inserted_outside_the_choke_point_is_impossible` (the source-level guard from Task 1, re-asserted at the integration level by proving all four public doors refuse) |

The four rows in **bold** are paths the spec's C8 enumeration does not contain
at all — they shipped 2026-07-28, after the spec was written. They are the
reason this task enumerates from the code.

- [ ] **Step 3: Full gate run + final docs sweep**

Run every gate. Then confirm the documentation checklist: `server-handlers.md`,
`server-connection.md`, `server-widgets.md`, `server-system-identity.md`,
`protocol.md`, `crypto.md`, `ARCHITECTURE.md` all reflect the shipped surface.

```bash
git commit -m "feat(e2ee): observation tests proving no plaintext reaches an E2EE channel on any path"
```

---

## Self-Review

**Spec coverage (sub-project 3):**
- Accept/validate/store/broadcast the new variants → Tasks 3, 4, 5. ✅
- Per-variant size caps, enforced blind, before allocation-heavy work → Task 3. ✅
- Class enforcement at ingest → Task 3 (fold already does the log half). ✅
- The `insert_message*` choke point + request-layer refusals for **every**
  non-log write path → Tasks 1, 2 (writer inventory table is the checklist). ✅
- Tombstone-aware derive/reconcile → Task 4. ✅
- FTS skip + `is_e2ee` → Tasks 1 (columns), 4 (behavior). ✅
- Reply event-hash ↔ id mapping → Task 4. ✅
- Welcome/KeyPackage/V2 fetch surfaces → Task 5. ✅
- `stale-epoch` error → Task 3. ✅
- Protocol version gate → Task 5. ✅
- Validation matrix (plaintext-in-E2EE, stale epoch, consumed/expired
  KeyPackage, non-member Add, good-standing Remove, incomplete reset, exceeded
  caps, pending-removals gate, staleness ceiling) → the fold owns all but caps
  and stale-epoch; Task 3 covers those two and Task 6's sealed-send tests
  exercise the fold's gates through the real ingest path. ✅
- Delete survives restart/reconcile → Task 4. ✅
- Derived-view rebuild parity → Task 4. ✅
- Retention/redaction on ciphertext → Task 4, observed in Task 6. ✅
- One observation test per content-producing path → Task 6. ✅

**Security checklist, each verified against source:**
- **Choke point** — every writer enumerated from the code with a line cite
  (writer inventory table); the raw SQL collapses into `messages::insert_row`
  and the source-level test `no_insert_into_messages_sql_outside_the_choke_point`
  keeps it that way.
- **Fail closed** — `ChannelWriteClass::Unresolvable` is treated as `E2ee` by
  `refuses_server_authored_content`; a failed read maps to `Unresolvable`, never
  `Plaintext` (ambiguity #2).
- **Content-blind** — no ingest path reads plaintext; edit/delete target
  verification is by `event_hash`, moderation by hash, cap validation by
  content hash/size/uploader.
- **Default-deny** — `no_new_request_variant_escapes_default_deny_request_requires_membership`
  (Task 2, extended in Task 5); actor is the authenticated connection key;
  widget arms keep their opaque not-found strings.
- **No DB Mutex across `.await`** — the two async touch points (`webhooks::deliver`,
  the sweeper) already scope their guards; the class check is a synchronous DB
  read inside those scopes.
- **Persist before broadcast; atomic ingest** — all new derive work rides the
  existing `unchecked_transaction` and the `LogState` advance follows
  `tx.commit()`.
- **Size caps before allocation** — `check_ingest_caps` runs before
  `ls.clone()`, pinned by `oversized_sealed_ciphertext_is_refused_before_the_fold_runs`
  asserting `log_pos()` is unchanged.

**Placeholder scan:** Tasks 1 and 3 contain complete code for the load-bearing
new functions. Task 2's refusals are one-line insertions at named line numbers.
Tasks 4–6 give exact test names, exact SQL and exact signatures but leave the
test bodies to the implementer, because they must be written against the crate's
real harness (`ServerState::new_for_test()`, `handlers.rs:3008`'s `setup()`, the
`widgets::sweep_once` fixture style). Flag for the reviewer: confirm Task 6's
tests **observe stored/broadcast bytes**, not call counts.

**Type consistency:** `ChannelClass::{Plaintext, E2ee}`, `EventPayload`'s ten new
variants with their **real** fields (`MlsCommit.post_epoch_authenticator` and
`MlsGroupReset.post_tree_hash` included), `LogState::{mls_current_epoch,
is_tombstoned, channel_class, log_pos}`, `messages::{insert_row,
insert_sealed_row}`, `channel_class::{resolve, require_plaintext, set_class}`,
and `event_ingest::{check_ingest_caps, derive_message_row, apply_sealed_edit,
apply_tombstone, reconcile_messages}` are used consistently across tasks.

**Integration caveats for the implementer:**
- `reconcile_messages`'s signature change has exactly one call site
  (`main.rs:116`), two lines below where the `LogState` is built.
- `widgets::sweep_once(conn, now)` has no `ServerState` — this is why the class
  lives in a DB column (ambiguity #1). Do not "fix" it by passing state.
- `client/src-tauri` depends on `farder-protocol` by path
  (`client/src-tauri/Cargo.toml:15`) but is **not** a workspace member, so
  `cargo build --workspace` does not compile it. After Task 5,
  `cd client/src-tauri && cargo build` is mandatory.
- The sweeper must `continue`, never abort a tick, on a class refusal.
