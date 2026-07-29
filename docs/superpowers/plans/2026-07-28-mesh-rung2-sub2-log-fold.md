# Mesh Rung 2 — Sub-project 2: Log Schema + Fold (MLS Control Plane) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the full Rung-2 MLS control plane in the log — every new `EventPayload` variant (all **dormant**: the fold accepts and validates them, nothing emits them yet) and all the new blind-server fold rules: channel class gating (fail closed, with the legacy carve-out), epoch CAS + commit-authenticator chaining, pending/confirmed leaves, the `pending_removals` send gate, the freshness ceiling, KeyPackage/device caps + lifetimes, deletion tombstones, `DeviceRevoked` + `DeviceCert` expiry, and non-selective reset completeness — all in `farder-crypto`, pure, unit-testable with no server/runtime, and checkpoint-composable exactly like Rung 1.

**Architecture:** Extend `crates/farder-crypto/src/event_log.rs` (new payload variants + support types, append-only — existing variants keep their exact canonical bytes) and `crates/farder-crypto/src/event_log_state.rs` (`LogState` grows the spec's fold state; `apply` stays a pure check-then-mutate `(prior_state, event) -> new_state` step). A final cross-crate integration test in `crates/farder-mls/tests/` proves the fold's chaining rules hold against **real OpenMLS values** produced by sub-1's `MlsChannelGroup` (farder-mls already depends on farder-crypto, so no dependency cycle).

**Tech Stack:** Rust, `std::collections::{HashMap, HashSet}`, `anyhow`, `serde`/`rmp-serde` (canonical bytes, per Rung-1 idiom). No new dependencies in `farder-crypto`. The integration task uses `farder-mls`'s existing public surface only (see authority note below).

**Spec:** `docs/superpowers/specs/2026-07-27-mesh-rung2-e2ee-design.md` (rev 2), sub-project 2. Precedent for style and fold idioms: `docs/superpowers/plans/2026-06-25-mesh-rung1-sub2-authz-state.md`.

---

## Authority note: sub-1's real public surface

The spec's sub-1 interface prose is superseded in places. The authority on the
`farder-mls` surface is `docs/modules/farder-mls.md` + `crates/farder-mls/src`:

- `ProcessedCommit.actual_adds` is `Vec<ActualLeaf>` (NOT `Vec<DeclaredMember>`);
  `ActualLeaf` carries the real credential + leaf signature key.
- `process_commit_checked` exists and is the variant downstream code SHOULD
  consume (refuses to merge a lying commit pre-merge).
- `ActualLeaf`, `MlsChannelGroup::load`, `leaves()`, `credential_with_key`,
  `decode_key_package` are public.
- `CommitOutcome { commit_bytes, welcome_bytes, prev_epoch_authenticator, post_tree_hash, epoch, adds, removes }`
  — `prev_epoch_authenticator` is captured **before** merging; **epoch
  convention:** `epoch` is the epoch the commit was *authored in*; merging moves
  the group to `epoch + 1`. `MlsChannelGroup::epoch_authenticator()` (accessor,
  post-merge) yields the NEW epoch's authenticator.

## Resolved spec ambiguities (decisions for this sub-project — source of truth)

These are places where the spec's sub-2 prose is under- or mis-specified and the
plan must pick; each decision honors the spec's *contract* even where it adjusts
a literal shape:

1. **`MlsCommit` carries a third chaining field: `post_epoch_authenticator: [u8;32]`.**
   The spec's chain rule ("a commit is invalid unless its
   `prev_epoch_authenticator` equals the authenticator the previously accepted
   commit **declared**") is unimplementable from the spec's two declared fields
   alone: the fold can never compute MLS authenticators, and
   `prev_epoch_authenticator` of commit *k+1* is the authenticator of the epoch
   commit *k* **created** — a value commit *k* never declared in the spec's
   field list. The author reads it post-merge via
   `MlsChannelGroup::epoch_authenticator()` and declares it. The fold stores it
   as the group's `epoch_authenticator` and checks the NEXT commit's
   `prev_epoch_authenticator` against it — exactly the spec's own fold-state
   comment (`epoch_authenticator // chained`). A liar faking
   `post_epoch_authenticator` wedges the chain (next honest commit cannot
   match) → dead end → reset — the spec's accepted "first lie costs a reset."
2. **Legacy plaintext carve-out (replay compatibility — load-bearing).** The
   spec says "either message variant is invalid in a channel with no prior
   `ChannelCreated`" AND "legacy channels … are permanently plaintext-class."
   Read literally, the first rule would brick every existing post-Rung-1 server:
   `build_log_state` replays stored logs whose `MessagePosted` events predate
   `ChannelCreated`'s existence. Reconciliation (per the spec's own legacy
   line + Q8 fresh-servers-only): `MessagePosted` in a channel **unknown to the
   log** is a legacy plaintext channel and stays valid (exact Rung-1 behavior);
   `MessagePosted` in a channel with `class: E2ee` is invalid;
   `MessagePostedE2ee` is valid ONLY in a channel with a prior `ChannelCreated`
   of `class: E2ee` — fail closed where it matters, no replay regression.
3. **`AuthzBeacon` is NOT a log event.** The spec's variant listing shows it for
   completeness, but its own comment says "sent as a sealed MLS application
   message (NOT a log event)". Nothing lands in `EventPayload` for it; it is
   sub-4/5 client behavior.
4. **Clocks.** Two deterministic clocks, both pure inputs (never wall time in
   the fold): (a) `event.core.timestamp` (untrusted author claim — same
   acceptance Rung 1 already made for invite expiry) drives `DeviceCert`
   expiry; (b) a new `log_pos: u64` in `LogState` (count of accepted events)
   drives KeyPackage lifetimes (`expires_at_log_pos`). Both serialize into
   state, so checkpoint composition is unaffected.
5. **Generation/epoch bootstrap conventions.** Generation 0 begins at
   `ChannelCreated { class: E2ee }` with `epoch = 0`; the creator authors the
   first logged `MlsCommit` at epoch 0. The first accepted commit of any
   `(channel, generation)` is exempt from the `prev_epoch_authenticator` chain
   check (there is nothing to chain to) and its author's `(identity, device)`
   enters `leaves_confirmed` (the creator IS the tree by construction). After
   `MlsGroupReset`, the new generation starts at `epoch = 1` (creation + the
   single implicit add-commit that produced the staged Welcomes), with
   `leaves_confirmed = {resetter's device}` and `leaves_pending =` the welcomed
   set.
6. **Reset Welcome staging.** `MlsGroupReset.welcomes` is a `Vec<EventRef>` and
   refs are content-addressed, so the Welcomes must precede the reset in the
   log. Rule: an `MlsWelcome` with `generation == current + 1` is valid only
   owner-authored (reset staging) and is recorded; `generation == current`
   requires a confirmed-leaf author (normal join flow). The reset's
   completeness check resolves its refs against recorded Welcomes.
7. **Post-reset leaf confirmation.** The reset generation's add-commit is never
   a log event, so `commits_by_epoch` has no entry to check
   `MlsLeafConfirmed.tree_hash` against. Rule: the FIRST accepted
   `MlsLeafConfirmed` of a reset generation seeds the expected tree hash; every
   subsequent confirmation must match it (all welcomed members must land on the
   same tree). `reset_pending` clears only when `leaves_confirmed` equals the
   fold's `members × live_devices`.
8. **Stale commits are accepted no-ops, fold-side.** `apply` keeps its
   `Result<()>` signature (callers in `event_ingest.rs` / `handlers.rs` are
   untouched): an `MlsCommit` whose epoch CAS fails folds `Ok(())`, advances
   ONLY the author's chain head, and changes zero MLS state — the spec's
   Rung-3-deterministic no-op. Ingest's distinct `stale-epoch` bounce is sub-3's
   job, served by this sub-project's query helpers.
9. **What the fold does NOT validate (division with sub-3):** per-variant byte
   size caps (40 KiB ciphertext, 256 KiB commit/Welcome, 8 KiB KeyPackage) are
   ingest checks; `authz_head` is opaque to the fold (head attestation is a
   client-side mechanism); edit/delete **target ownership** needs a per-message
   index the spec's fold state deliberately omits — the fold records tombstones
   and gates by class/membership/reason, ingest verifies target authorship
   against the derived `messages` table. Each is documented at the rule site.

## Review round 1 — adjustments to the resolved ambiguities (implemented)

The code review of the landed sub-2 branch found six holes in the rules as
written above. Each is fixed in `fix(crypto): address sub2 review findings
(round 1)`; where the fix narrows a decision made above, the narrowing is the
authority from here on (and is documented in `docs/modules/crypto.md`):

1. **Ambiguity #5 is now enforced literally: the bootstrap commit is
   CREATOR-only.** "The creator authors the first logged `MlsCommit` at epoch 0"
   was prose, not a rule — the implemented exemption skipped the confirmed-leaf
   check for a generation's first commit with no author check at all, so any
   identity that could register a device could seize a fresh `ChannelCreated
   { E2ee }` group (or any post-reset generation, whose `epoch_authenticator` is
   `None` again), brick it for its real creator, and hold a confirmed leaf in
   it. `ChannelRecord` now stores `creator`; the confirmed-leaf exemption
   applies only while `leaves_confirmed` is EMPTY, and then only to the creator.
   The chain-check exemption stays keyed to `epoch_authenticator == None`.
2. **MLS control-plane authority is re-checked against the authz fold.**
   `MlsCommit`, `MlsWelcome` and `MlsLeafConfirmed` all require
   `is_member(author) && !is_pending(author)`, like `check_sealed_send`. Leaf
   membership is not standing authority: `MemberRemoved` does not touch leaf
   sets, so a kicked identity keeps its confirmed leaf until a Remove-commit
   lands and could otherwise still drive the group.
3. **Ambiguity #4 (clocks) gains a third, monotone clock.**
   `event.core.timestamp` is author-chosen, and three fold gates were
   load-bearing on it. `LogState.identity_clock` (per-identity max claimed
   timestamp) is now the floor `live_devices` judges liveness at, so the
   live-device cap cannot be pumped with a future timestamp and an identity
   cannot back-date past its own certs' expiry. Residual (cross-identity
   back-dating below a silent identity's expiry) is stated in `crypto.md` and
   is sub-3's to bound at ingest.
4. **Tombstones no longer spend freshness budget.** `MessageDeleted` targets are
   opaque to the fold (ambiguity #9), so spending the C4 ceiling on them let any
   member seal any E2ee channel on demand with 500 fabricated tombstones. They
   still advance `channel_events_since_reset`. Spec C4 only requires sealed
   content to spend the ceiling.
5. **An incomplete reset is exempt from the reset rate limit.** While
   `reset_pending` is set the channel accepts no sealed content, so its
   rate-limit clock cannot advance — a welcomed device that never confirms would
   otherwise lock the channel out of the only recovery hatch it has.
6. **The legacy carve-out (ambiguity #2) is one-way.** `LogState` records channel
   ids that carried plaintext under the carve-out and refuses `ChannelCreated`
   for them, so a channel with plaintext history can never be declared `E2ee`.
   The fold rule is again self-sufficient for Rung-3 fresh replay, as the spec
   claims; sub-3's `messages`-table check stays belt-and-braces.

## Review round 2 — adjustments (implemented)

Round 1's fixes were correct but two of them were only half the story, and one
spec claim turned out to contradict the spec's own fold-state formula. Each is
fixed in `fix(crypto): address sub2 review findings (round 2)`; where a fix
narrows or overrides an earlier decision, the narrowing is the authority from
here on (and is documented in `docs/modules/crypto.md`).

1. **Round 1's monotone clock only stopped BACK-dating; FORWARD-dating was the
   live C7 attack.** The per-identity floor is per-identity, so an attacker's
   claimed timestamp raised only their own floor. A commit author claiming a
   far-future `core.timestamp` had every OTHER member's expiring cert judged
   dead, `good_standing` collapsed, and the non-selective-removal rule
   authorized a **silent, unlogged eviction** — the exact spec C7 attack, without
   touching the reset hatch, available to any confirmed-leaf member. The same
   claim also made the eviction count as a drift discharge (bypassing
   `COMMIT_RATE_MIN_EPOCH_GAP`) and shrank `member_leaf_set` enough for a partial
   reset to pass the exact-cover check. Round 1's residual note pointed at "sub-3
   bounds `core.timestamp` at ingest", but round 1 itself rejected that standard
   (the FOLD must refuse it, so a Rung-3 replica replaying from genesis refuses
   it too). **Ambiguity #4 gains a fourth clock:** `LogState.corroborated_clock`
   — the greatest timestamp at least **two distinct identities** have claimed
   (the second-largest `identity_clock` value, recomputed after each accepted
   event, so a checkpoint carries it and it only moves forward). Liveness is now
   judged at two different points depending on who is asking:
   `self_liveness_ts = max(at_ts, floor)` for an identity's own claim (envelope
   expiry gate, public `live_devices`), and
   `judged_liveness_ts = max(floor, min(at_ts, corroborated_clock))` for every
   cross-identity derivation (`member_leaf_set` → drift sets →
   `commit_discharges_drift` → reset completeness, plus the declared add/remove
   checks). A *lone* author cannot move the ceiling at all, so forward-dating
   buys nothing; residual (two colluding identities) is stated in `crypto.md`.
   Pinned by `a_forward_dated_commit_cannot_evict_a_member_in_good_standing`.
2. **Spec C3's promise was false as implemented: an unconfirmed leaf was a
   permanent, invisible lockout.** C3 says "a bogus Welcome leaves visible drift
   and gets retried automatically", but the spec's own fold-state formula
   subtracts `leaves_pending` from `pending_adds`, and a pending leaf is not in
   `pending_removals` either — zero drift on both sets. Meanwhile a re-add was
   refused ("already present or pending"), the removal was refused (its owner is
   in good standing), and the victim could not author the fix (that needs a
   CONFIRMED leaf, which is exactly what they lack). One bogus Welcome — or an
   ordinary steward crash between commit and Welcome — locked a member out of the
   whole generation with only the owner-only reset as recovery. Both halves of
   the review's suggested direction are implemented: `pending_confirmations()`
   exposes the retry obligation, and the good-standing gate on `DeclaredRemove`
   now applies **only to confirmed leaves** — dropping an unproven leaf evicts
   nobody, because the device reappears in `pending_adds` the instant it leaves,
   so the Add is simply re-driven with a fresh KeyPackage. The formula stays as
   the spec writes it; C3's prose is what was wrong, and `crypto.md` now says so.
   Pinned by `an_unconfirmed_leaf_is_visible_and_can_be_re_driven` (and the
   assertion in `joiner_confirmation_promotes_only_on_matching_tree_hash` now
   names the right invariant).
3. **The envelope cert-expiry gate judged the RAW author timestamp.** Round 1's
   claim that "an identity cannot back-date past its own certs' expiry" held only
   for `live_devices` queries, not for the gate itself. Since nothing in the
   chain forces timestamp monotonicity, an identity whose floor had already
   passed T could still author from a device whose cert died at T by claiming
   `timestamp <= T`. `MlsCommit` authz checks only that the AUTHOR is a member
   with a confirmed leaf — never that the AUTHORING DEVICE is live — so a dead
   device kept full control-plane authority: it could zero
   `events_since_last_commit` (defeating the C4 freshness ceiling indefinitely)
   and set the chain variable at will. The gate now judges at
   `self_liveness_ts(author, core.timestamp)`, the same monotone point
   `live_devices` uses. Pinned by
   `an_expired_device_cannot_author_by_back_dating`.

## Review round 3 — adjustments (implemented)

Round 3 found the two fixes below plus three minors. Each is fixed in its own
`fix(crypto): …` commit; where a fix narrows or overrides an earlier decision,
the narrowing is the authority from here on (and is documented in
`docs/modules/crypto.md`).

1. **The commit-rate rule and the freshness ceiling together bricked every
   channel with fewer than four committing identities — permanently.** The rule
   was an EPOCH-distance rule (`epoch >= last + COMMIT_RATE_MIN_EPOCH_GAP`) and
   every accepted commit advances the epoch by exactly one, so with M authors
   round-robining, an author's next turn arrives exactly M epochs later:
   sustained rekeying required M >= 4. With M <= 3 and zero drift, EVERY member
   was permanently rate-blocked once it had spent its one exempt "first commit"
   — and `FRESHNESS_CEILING_EVENTS` sealed events later the channel accepted no
   further content, forever. The reset hatch cannot rescue it twice
   (`channel_events_since_reset` only advances on sealed content and tombstones,
   and sealed content is exactly what the ceiling stopped). The spec's own
   "#private with a friend" case is M = 2. The pre-existing ceiling test passed
   only because it spent Alice's FIRST commit as the rekey; no test drove the
   second cycle, which is where every real channel lives. **Two changes, both
   pure functions of fold state:** (i) the gap SCALES —
   `commit_rate_gap() = min(COMMIT_RATE_MIN_EPOCH_GAP, distinct identities
   holding a confirmed leaf)`, floored at 1, which preserves the anti-spam
   property exactly (while anyone else holds a leaf the gap is >= 2, so no author
   takes two turns in a row) while making the client rekey cadence reachable in
   small channels; and (ii) the CEILING OVERRIDES the rule — within
   `COMMIT_RATE_CEILING_GRACE_EVENTS = 50` events of the ceiling the rate rule
   stands aside, because a rekey the ceiling itself is demanding is never spam.
   (ii) is what saves an M >= 4 channel whose only online member has already
   taken its turn: nothing but a commit advances the epoch, so no epoch-distance
   rule could ever be satisfied again. It cannot be milked — every accepted
   commit zeroes the budget, so the hatch buys at most one commit per 450 sealed
   events. Pinned by `two_member_channel_survives_repeated_freshness_cycles`,
   `three_member_channel_survives_repeated_freshness_cycles`,
   `small_channels_can_rekey_on_cadence_not_only_at_the_ceiling`,
   `a_lone_rekeyer_can_always_answer_the_freshness_ceiling`, with anti-spam held
   by `commit_rate_rule_still_blocks_spam_in_a_large_channel`.
2. **`reset_pending` was a latch cleared inside ONE event type, so ordinary
   sequences were terminal.** It was set by `MlsGroupReset` and cleared only
   inside `MlsLeafConfirmed`, on `leaves_confirmed == members × live_devices`.
   Ban a welcomed device before it confirms and the bridge's own answer — a
   Remove-commit dropping the unproven leaf (permitted since round 2) — emptied
   `pending_removals`, `pending_adds` AND `pending_confirmations`, yet sealed
   sends stayed refused and **no `MlsLeafConfirmed` could ever arrive**; a member
   joining after the reset grew `members × live_devices`, so the equality never
   held either. Escape required another owner-only reset, destroying continuity —
   the recurring "over-conservative guard creates an unexitable state" class.
   **The gate is now DERIVED** (overriding resolved ambiguity #7's "`reset_pending`
   clears only when `leaves_confirmed` equals the fold's `members × live_devices`"
   and the `reset_pending: bool` field in Task 3's record): `MlsGroupRecord`
   stores `reset_welcomed` (the leaves the reset staged) and
   `reset_incomplete() = !reset_welcomed.is_disjoint(&leaves_pending)`. Every
   path that can discharge the obligation heals the gate — a confirmation
   promotes the leaf out of `leaves_pending`, a Remove-commit drops it — and the
   predicate stays a pure function of state, so it composes from any checkpoint.
   The commit effect prunes `reset_welcomed` to the still-pending staged leaves,
   which keeps the gate scoped to the reset's OWN obligations: an ordinary
   pending join (before or after the reset) never seals the channel, and a
   dropped-then-re-added leaf is an ordinary join rather than a resurrected reset
   obligation. `MlsLeafConfirmed`'s tree-hash seeding path is likewise keyed to
   `reset_welcomed` membership rather than to the latch. Pinned by
   `a_reset_completes_when_a_stuck_leaf_is_removed_not_only_when_it_confirms`
   (both halves: discharge-by-removal, and an ordinary join not re-sealing).

## Global constraints

- **Old events are untouched.** New variants are APPENDED after
  `AttachmentRedacted`; no existing struct/variant field is added, removed, or
  reordered. `DeviceCert.expires_at` uses
  `#[serde(default, skip_serializing_if = "Option::is_none")]` so a cert
  without expiry serializes to the **identical canonical bytes** legacy certs
  were signed over — old signatures still verify (proven by test).
- **The fold stays pure and checkpoint-composable.** No I/O, no clock, no
  randomness; `replay == stepwise == compose-from-checkpoint` extended over ALL
  new state (Task 5).
- **Check-then-mutate:** every fallible check runs before any mutation (Rung-1
  idiom; a rejected event leaves state untouched, no clone/rollback).
- **Fail closed:** unknown class ⇒ unusable; `MessagePostedE2ee` without an
  E2ee `ChannelCreated` ⇒ invalid; pending removals ⇒ sealed sends invalid;
  ceiling exceeded ⇒ sealed sends invalid; incomplete reset ⇒ invalid.
- **Everything lands dormant.** No client or server code emits the new
  variants in this sub-project; `cargo test --workspace` stays green and every
  pre-existing `event_log`/`event_log_state` test passes unmodified (the
  regression gate for the legacy carve-out).
- **Owner-only this rung (spec M3):** `ChannelCreated` and `MlsGroupReset`
  require `is_owner(author)`. No new capability string is invented.
- **Constants are named, once:**
  `MAX_LIVE_DEVICES_PER_IDENTITY = 8`,
  `MAX_LIVE_KEY_PACKAGES_PER_DEVICE = 10`,
  `FRESHNESS_CEILING_EVENTS: u32 = 500`,
  `COMMIT_RATE_MIN_EPOCH_GAP: u64 = 4`,
  `RESET_MIN_CHANNEL_EVENTS: u32 = 1000`.
- **Docs discipline:** `docs/modules/crypto.md` gains the new public surface in
  the same commits (checklist in each task).

## File structure

- **Modify** `crates/farder-crypto/src/event_log.rs` — support types
  (`ChannelClass`, `DeclaredAdd`, `DeclaredRemove`, `DeleteReason`), 10 new
  `EventPayload` variants, `DeviceCertCore.expires_at` +
  `DeviceCert::create_expiring`.
- **Modify** `crates/farder-crypto/src/event_log_state.rs` — `LogState` fold
  extensions: new state, new authz arms, new effects, derived sets, query
  helpers, tiebreak comparator.
- **Create** `crates/farder-mls/tests/fold_chain.rs` — real-OpenMLS ↔ fold
  chaining integration (Task 5).
- **Modify** `docs/modules/crypto.md` — document the new public surface.

---

## Task 1: Schema — new `EventPayload` variants + support types + `DeviceCert.expires_at` (dormant data)

**Files:** modify `crates/farder-crypto/src/event_log.rs`, `docs/modules/crypto.md`.

**Interfaces produced:** the exact wire shapes every later sub-project signs and
folds. Pure data this task — no fold rules yet.

- [ ] **Step 1: Write the failing tests** (they fail to compile until Step 2):

Test names (in `event_log.rs::tests`) — spec invariants as names:

- `new_mls_variants_roundtrip_canonical_bytes` — for EVERY new variant: build
  an `Event` via `Event::next`, `to_bytes` → `from_bytes` → equality, `hash()`
  stable, `verify` under the signing device key passes.
- `existing_variant_bytes_are_untouched_by_new_variants` — a `MessagePosted`
  event built exactly like Rung 1's tests round-trips and its `hash()` is
  unchanged by re-encoding (append-only enum: old variant indices stable).
- `device_cert_without_expiry_preserves_legacy_signed_bytes` —
  `DeviceCert::create(...)` (no expiry) verifies, AND its `core` rmp bytes
  equal the hand-built 4-field encoding (i.e. `expires_at: None` is skipped in
  serialization, so certs signed before this change still verify after decode →
  re-encode).
- `expiring_device_cert_roundtrips_and_verifies` —
  `DeviceCert::create_expiring(...)` sets `expires_at: Some(t)`, round-trips,
  verifies; tampering `expires_at` breaks the signature.

- [ ] **Step 2: Implement the schema.**

In `event_log.rs`, add support types (before `EventPayload`):

```rust
/// A channel's content class — part of channel identity, immutable (spec §class).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelClass {
    Plaintext,
    E2ee,
}

/// Fold-readable declaration of an MLS Add inside `MlsCommit`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeclaredAdd {
    pub identity: PublicKey,
    pub device: DeviceId,
    pub key_package: EventRef, // the MlsKeyPackagePublished event consumed
}

/// Fold-readable declaration of an MLS Remove inside `MlsCommit`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeclaredRemove {
    pub identity: PublicKey,
    pub device: DeviceId,
}

/// Why a message tombstone exists (spec F2). `Author` claims are verified
/// against the derived view by ingest (sub-3); the fold verifies `Moderation`
/// authority itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeleteReason {
    Author,
    Moderation,
}
```

Append to `EventPayload` (AFTER `AttachmentRedacted` — append-only), with the
spec's field lists plus the resolved-ambiguity delta (`post_epoch_authenticator`):

```rust
    /// Owner-authored channel identity. No ChannelCreated => the channel does
    /// not exist to the log (legacy DB channels stay permanently plaintext).
    ChannelCreated { channel_id: u64, name: String, kind: String, class: ChannelClass, parent: Option<u64> },
    /// Authored by the owning device; server-scoped; consumed-once; capped and
    /// lifetime-bounded by log position (spec I5).
    MlsKeyPackagePublished { key_package: Vec<u8>, store_instance_hash: [u8; 32], expires_at_log_pos: u64 },
    /// One MLS commit. prev_epoch_authenticator chains onto the previous
    /// commit's declared post_epoch_authenticator (resolved ambiguity #1);
    /// post_* values are read after the author's local merge.
    MlsCommit {
        channel_id: u64, generation: u64, epoch: u64, mls_message: Vec<u8>,
        adds: Vec<DeclaredAdd>, removes: Vec<DeclaredRemove>,
        prev_epoch_authenticator: [u8; 32], post_epoch_authenticator: [u8; 32],
        post_tree_hash: [u8; 32], authz_head: EventHash, store_instance_hash: [u8; 32],
    },
    /// Welcome bytes for one (member, device); for_* are unverifiable by the
    /// fold — leaves only count once the joiner confirms.
    MlsWelcome { channel_id: u64, generation: u64, commit: EventRef, for_member: PublicKey, for_device: DeviceId, welcome: Vec<u8> },
    /// Authored by the JOINING device after processing its Welcome; promotes
    /// its leaf pending -> confirmed iff tree_hash matches the fold's record.
    MlsLeafConfirmed { channel_id: u64, generation: u64, epoch: u64, tree_hash: [u8; 32], store_instance_hash: [u8; 32] },
    /// Owner-only recovery hatch; valid only if `welcomes` covers exactly the
    /// fold's members × live_devices (non-selective reset, spec C7).
    MlsGroupReset { channel_id: u64, new_generation: u64, welcomes: Vec<EventRef> },
    /// Sealed channel content. ciphertext = MLS PrivateMessage of a padded
    /// MessageEnvelope; reply_to + caps stay OUTSIDE the seal (blind threading
    /// and cap-vs-blob validation).
    MessagePostedE2ee { channel_id: u64, generation: u64, epoch: u64, ciphertext: Vec<u8>, reply_to: Option<EventRef>, attachments: Vec<AttachmentCap>, authz_head: EventHash },
    /// Sealed edit (spec F6 — EditMessage{new_content} would ship plaintext).
    MessageEditedE2ee { channel_id: u64, target: EventRef, generation: u64, epoch: u64, ciphertext: Vec<u8>, authz_head: EventHash },
    /// Durable content-blind deletion tombstone (spec F2) — derive/reconcile
    /// consult it so deletions cannot resurrect.
    MessageDeleted { channel_id: u64, target: EventRef, reason: DeleteReason },
    /// Kills a device: cert dead, chain frozen (history stands, new events
    /// rejected), leaves become pending_removals via the bridge.
    DeviceRevoked { device: DeviceId },
```

`DeviceCertCore` gains (LAST field, skip-when-none so legacy signed bytes are
byte-identical):

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
```

`DeviceCert::create` keeps its signature (produces `expires_at: None`); add
`DeviceCert::create_expiring(identity, device_pubkey, created_at, expires_at: u64)`.

- [ ] **Step 3: Run** `cargo test -p farder-crypto event_log` — all new + all
  pre-existing tests PASS. Then `cargo build --workspace` (the client crate is
  separate: also `cd client/src-tauri && cargo build` — `DeviceCert` literals
  elsewhere must still compile; struct-update or constructor call sites are the
  risk surface).
- [ ] **Step 4:** Update `docs/modules/crypto.md` (new variants + support types
  + `create_expiring`, with the dormant status stated).
- [ ] **Step 5: Commit** — `feat(crypto): rung2 log schema — MLS/E2EE event payload variants + DeviceCert expiry (dormant)`

---

## Task 2: Fold — channel class gating, tombstones, `DeviceRevoked`, device cap, cert expiry

**Files:** modify `crates/farder-crypto/src/event_log_state.rs`, `docs/modules/crypto.md`.

**Interfaces produced:** `LogState` fields `channels`, `tombstones`,
`revoked_devices`, `devices_by_identity`, `log_pos`; `DeviceRecord.expires_at`;
queries `channel_class(u64) -> Option<ChannelClass>`, `is_tombstoned(&EventRef)`,
`is_device_revoked(&DeviceId)`, `live_devices(&PublicKey, at_ts: u64) -> Vec<DeviceId>`;
authz + effects for `ChannelCreated`, `MessageDeleted`, `DeviceRevoked`; envelope
hardening (revoked/expired device gate); the class gate on `MessagePosted`.
MLS-group variants get TEMP permissive-reject arms
(`bail!("MLS variants folded in Task 3/4")`) so nothing is silently permissive.

- [ ] **Step 1: Write the failing tests.** Names = spec invariants:

- `channel_created_is_owner_only_and_ids_are_immutable` — non-owner
  `ChannelCreated` rejected; duplicate `channel_id` rejected (class is set once
  or the channel does not exist — no class-change event exists by construction).
- `plaintext_post_is_invalid_in_an_e2ee_channel` — owner creates
  `class: E2ee`; a member's `MessagePosted { channel_id }` is rejected (the
  fail-closed half of class gating).
- `legacy_channels_without_channelcreated_stay_plaintext_writable` — the exact
  Rung-1 flow (no `ChannelCreated` anywhere) still folds green: replay
  compatibility for existing servers (resolved ambiguity #2). Also:
  `MessagePosted` into a `class: Plaintext` created channel is valid.
- `thread_child_inherits_parent_class_or_is_rejected` — `parent: Some(p)`
  requires `p` to exist and the child's `class` to equal `p`'s class; mismatch
  or unknown parent rejected (spec coexistence row 12).
- `message_deleted_writes_a_queryable_tombstone` — owner creates a Plaintext
  channel; `MessageDeleted { reason: Moderation }` by a "kick"-holder is
  accepted and `is_tombstoned(target)` flips; `reason: Moderation` WITHOUT
  "kick" is rejected; `reason: Author` requires membership only (ingest
  verifies authorship against the derived view — documented at the rule);
  duplicate tombstone for the same target rejected; `MessageDeleted` in a
  channel unknown to the log is rejected (log deletes are for log channels).
- `revoked_device_cannot_author_but_history_stands` — `DeviceRevoked` authored
  by the owning identity (from its other device) or by the server owner is
  accepted; by an unrelated identity rejected; after revocation, a new event
  signed by the revoked device is rejected at the envelope; state derived from
  its earlier events is unchanged.
- `expired_cert_cannot_author_events` — a device authorized via
  `create_expiring(.., expires_at = T)`: an event with
  `core.timestamp <= T` folds; `core.timestamp > T` is rejected (untrusted
  author clock, same acceptance as Rung-1 invite expiry — documented).
- `ninth_live_device_of_an_identity_is_rejected` — 8 `DeviceAuthorized` fold;
  the 9th is rejected (`MAX_LIVE_DEVICES_PER_IDENTITY`); after a
  `DeviceRevoked`, an additional device is accepted again (revoked ≠ live).

- [ ] **Step 2:** Run one to confirm FAIL (e.g. the class-gate test — permissive
  today).
- [ ] **Step 3: Implement.** Envelope additions in `apply` (after
  `resolve_device_pubkey`, before payload authz):
  `ensure!(!revoked)` and cert-expiry check against `event.core.timestamp`
  (both from `DeviceRecord`); `log_pos += 1` on success (in the effects
  section). `DeviceAuthorized` authz gains the live-device cap; its effect
  maintains `devices_by_identity` and records `expires_at` from the payload
  cert. New arms per the test contracts; `MessagePosted` authz gains the class
  gate. All state additions initialized in `from_genesis` (empty/zero).
- [ ] **Step 4:** `cargo test -p farder-crypto event_log_state` — new tests +
  every pre-existing test green (proves check-then-mutate held and the legacy
  carve-out preserved Rung-1 behavior). Then `cargo test -p farder-crypto`.
- [ ] **Step 5:** Update `docs/modules/crypto.md`; commit —
  `feat(crypto): rung2 fold — channel class gating, tombstones, device revocation/expiry/cap`

---

## Task 3: Fold — MLS group bookkeeping: KeyPackages, commits (CAS + chaining + rate + tiebreak), Welcomes, leaf confirmation

**Files:** modify `crates/farder-crypto/src/event_log_state.rs`, `docs/modules/crypto.md`.

**Interfaces produced:**

```rust
struct CommitRecord { event_hash: EventHash, post_tree_hash: [u8; 32] }
struct WelcomeRecord { generation: u64, for_member: PublicKey, for_device: DeviceId }
struct MlsGroupRecord {
    generation: u64,
    epoch: u64,                       // epoch the group is IN (declared+1 of last commit)
    commit_head: Option<EventHash>,
    epoch_authenticator: Option<[u8; 32]>, // last commit's declared post_epoch_authenticator
    tree_hash: Option<[u8; 32]>,
    leaves_confirmed: HashSet<(PublicKey, DeviceId)>,
    leaves_pending: HashSet<(PublicKey, DeviceId)>,
    events_since_last_commit: u32,
    last_commit_epoch_by_author: HashMap<PublicKey, u64>,
    channel_events_since_reset: u32,  // reset rate limit clock
    reset_pending: bool,
    reset_expected_tree_hash: Option<[u8; 32]>,   // resolved ambiguity #7
    commits_by_epoch: HashMap<u64, CommitRecord>, // keyed by the epoch a commit CREATED
    welcomes: HashMap<EventHash, WelcomeRecord>,
}
```

`LogState` additions: `mls_groups: HashMap<u64, MlsGroupRecord>` (created by
`ChannelCreated { class: E2ee }`, generation 0 / epoch 0),
`key_packages: HashMap<(PublicKey, DeviceId), HashMap<EventRef, u64>>` (ref →
expiry log pos, unconsumed), `consumed_key_packages: HashMap<EventRef, u64>`
(prunable past expiry), `device_store_instance: HashMap<DeviceId, [u8; 32]>`.
Public: `pending_removals(channel_id, at_ts) -> HashSet<(PublicKey, DeviceId)>`
and `pending_adds(...)` (the spec's derived pure functions:
`confirmed ∪ pending \ members×live_devices` and its complement),
`mls_current_epoch(channel_id) -> Option<(u64 /*generation*/, u64 /*epoch*/)>`
(sub-3's stale-epoch pre-check), `leaves_confirmed(channel_id)`,
`commit_discharges_drift(&LogState, ...) -> bool`, and
`compare_same_epoch_commits(&LogState, a: &Event, b: &Event) -> Ordering` — the
drift-priority tiebreak (obligation-discharging first, then canonical
`(lamport, author, event_hash)`), exported pure for Rung-3's orderer.

**Per-payload rules (the contract the tests pin):**

- `MlsKeyPackagePublished` — authz: author is a full member (approved,
  non-pending); `store_instance_hash` equals the pinned hash for this device
  (first publish pins it — a device's instance hash is immutable for its
  lifetime; store loss ⇒ self-revoke + fresh device, per spec C6);
  live-unexpired count for `(author, device)` `< MAX_LIVE_KEY_PACKAGES_PER_DEVICE`
  (expired refs do not count and are pruned on touch); `expires_at_log_pos > log_pos`.
  Effect: record ref → expiry.
- `MlsCommit` — authz (in order): channel exists with `class: E2ee`;
  `generation` matches; **epoch CAS**: `epoch == group.epoch` else the event is
  an **accepted no-op** (resolved ambiguity #8: `Ok(())`, chain head advances,
  zero MLS state change); `store_instance_hash` matches the author-device pin;
  author holds a confirmed leaf (EXEMPT: the first commit of a generation —
  bootstrap, resolved ambiguity #5); **chain**:
  `prev_epoch_authenticator == group.epoch_authenticator` (exempt for a
  generation's first commit); every `DeclaredAdd`: identity is a full
  non-banned member, device cert live (non-revoked, non-expired at
  `event.core.timestamp`), `key_package` ref is an unconsumed, unexpired
  package of exactly `(identity, device)`, the leaf is not already
  present/pending, and the **self-add rule**: if the identity already has ≥1
  confirmed leaf, the commit's author must be that same identity; every
  `DeclaredRemove`: leaf present (confirmed or pending) and EITHER the member
  is out of good standing (non-member/banned/device revoked/cert expired) OR
  author identity == removed identity (self-removal); **commit-rate rule**:
  valid unless it neither discharges drift (`commit_discharges_drift`: at least
  one add ∈ `pending_adds` or remove ∈ `pending_removals`) nor is the author's
  first commit nor `epoch >= last_commit_epoch_by_author[author] + COMMIT_RATE_MIN_EPOCH_GAP`.
  Effect: `epoch = declared + 1`; store `commit_head`,
  `epoch_authenticator = post_epoch_authenticator`, `tree_hash`,
  `commits_by_epoch[declared + 1]`; adds → `leaves_pending`, consume
  KeyPackages (move ref to `consumed_key_packages`); removes → drop from both
  leaf sets; `events_since_last_commit = 0`;
  `last_commit_epoch_by_author[author] = declared`.
- `MlsWelcome` — authz: channel E2ee; `generation == current` (author holds a
  confirmed leaf; `(for_member, for_device)` ∈ `leaves_pending`; `commit` ref ∈
  recorded `commits_by_epoch` values) OR `generation == current + 1`
  (owner-only reset staging, resolved ambiguity #6). Effect: record
  `WelcomeRecord`.
- `MlsLeafConfirmed` — authz: channel E2ee; `generation` matches; authored BY
  the joining device: `(author, event.core.device) ∈ leaves_pending`;
  `store_instance_hash` matches/pins; `tree_hash` equals
  `commits_by_epoch[epoch].post_tree_hash` — or, in a reset generation with no
  logged commits yet, seeds/matches `reset_expected_tree_hash` (resolved
  ambiguity #7). Effect: promote pending → confirmed; if `reset_pending` and
  `leaves_confirmed == members × live_devices`, clear `reset_pending`.

- [ ] **Step 1: Write the failing tests.** A `bootstrapped_e2ee` helper builds:
  owner + device, invite, member alice + device, `ChannelCreated { E2ee }`,
  KeyPackage publishes. Test names:

- `key_package_cap_and_log_position_lifetime_are_enforced` — 10 live publishes
  ok, 11th rejected; a package whose `expires_at_log_pos` has passed no longer
  counts toward the cap AND is invalid as an Add target.
- `first_commit_bootstraps_then_epoch_cas_noops_stale_commits` — creator's
  epoch-0 commit accepted (no chain expectation, author leaf confirmed);
  a second commit re-declaring epoch 0 folds `Ok` with ZERO state change except
  its author chain head (assert group record deep-equal before/after) — the
  deterministic no-op.
- `commit_chaining_rejects_build_on_a_liar` — commit k declares
  `post_epoch_authenticator = X`; commit k+1 with
  `prev_epoch_authenticator != X` is REJECTED; with `== X` accepted (the spec's
  "a liar cannot be built upon", checked blind).
- `declared_add_requires_a_live_key_package_of_a_member_in_good_standing` —
  add of a non-member / banned member / revoked device / expired cert /
  consumed ref / other-device's ref: each rejected individually.
- `remove_of_a_member_in_good_standing_is_rejected_except_self_removal` —
  steward removing a good-standing leaf rejected; the member removing their own
  device accepted; removing a banned member's leaf accepted.
- `self_add_rule_blocks_stewards_adding_a_second_device` — identity with one
  confirmed leaf: a second-device add authored by another member rejected;
  authored by the identity itself accepted (spec C5/Q12).
- `joiner_confirmation_promotes_only_on_matching_tree_hash` — after an
  add-commit, leaf is pending (drift visible via `pending_adds`), sealed
  bogus-Welcome scenario stays pending; `MlsLeafConfirmed` with wrong
  `tree_hash` rejected; correct one promotes to confirmed; a confirm authored
  by a different device of the same identity rejected.
- `store_instance_hash_is_pinned_per_device` — first `MlsKeyPackagePublished`
  pins; a later publish/commit/confirm from the same device with a different
  hash is rejected (the clone/restore poison signal, spec C6).
- `commit_rate_rule_blocks_spam_but_never_drift_discharge` — same author
  commits at epochs n and n+1 (no drift): second rejected; the same
  quick commit that discharges a `pending_removals` entry: accepted
  (`COMMIT_RATE_MIN_EPOCH_GAP`).
- `drift_priority_tiebreak_beats_a_premined_commit` — two same-epoch candidate
  commit Events, one discharging drift, one a self-update pre-mined to sort
  first canonically (lower hash): `compare_same_epoch_commits` orders the
  drift-discharger first regardless of hash grinding (spec I2 grind
  resistance), falling back to canonical order when neither/both discharge.

- [ ] **Step 2:** Run one to confirm FAIL (Task 2 left the MLS arms as
  `bail!` TEMP rejections, so accept-path tests fail).
- [ ] **Step 3: Implement** per the contracts above (check-then-mutate: the
  no-op path must decide *before* any mutation).
- [ ] **Step 4:** `cargo test -p farder-crypto` — all green.
- [ ] **Step 5:** Update `docs/modules/crypto.md`; commit —
  `feat(crypto): rung2 fold — MLS commits (CAS+chaining+rate+tiebreak), key packages, joiner-confirmed leaves`

---

## Task 4: Fold — sealed content gates (pending-removals, freshness ceiling) + non-selective reset

**Files:** modify `crates/farder-crypto/src/event_log_state.rs`, `docs/modules/crypto.md`.

**Per-payload rules:**

- `MessagePostedE2ee` — authz: channel exists with `class: E2ee` (unknown
  channel or Plaintext class ⇒ invalid — the other half of class gating);
  author is a full member holding a **confirmed** leaf; `generation` matches;
  `epoch == group.epoch` (the fold accepts only the current epoch at its log
  position — deterministic on replay); **send gates, all fail closed**:
  `pending_removals(channel, event.core.timestamp)` empty (spec I1 — the
  protocol invariant, not client courtesy);
  `events_since_last_commit < FRESHNESS_CEILING_EVENTS` (spec C4 — the blind
  rekey ceiling); `!reset_pending` (partial reset = dead channel, loudly).
  `authz_head` is carried opaque (fold does not validate — client head
  attestation; documented). Effect: `events_since_last_commit += 1`,
  `channel_events_since_reset += 1` (saturating), record attachment uploaders
  (same as `MessagePosted`, so `AttachmentRedacted` authz works on sealed
  posts).
- `MessageEditedE2ee` — same authz gates as `MessagePostedE2ee` (target
  authorship is ingest's, via the derived view — fold has no message index by
  design); target must not be tombstoned. Same counters effect.
- `MessageDeleted` (E2ee channels) — already landed in Task 2; this task adds:
  it also increments the freshness counters when the channel is E2ee.
- `MlsGroupReset` — authz: owner-only; channel E2ee;
  `new_generation == generation + 1`; rate limit:
  `channel_events_since_reset >= RESET_MIN_CHANNEL_EVENTS` OR no reset has ever
  occurred (first reset always allowed);
  **completeness (spec C7)**: every ref in `welcomes` resolves to a recorded
  `WelcomeRecord` with `generation == new_generation`, no duplicates, and the
  target set equals EXACTLY `members × live_devices(at event.core.timestamp)`
  minus the resetter's own authoring device (the resetter's leaf is the new
  group's creator — resolved ambiguity #5) — no more, no fewer.
  Effect: `generation = new_generation`; `epoch = 1`;
  `leaves_confirmed = {(author, device)}`; `leaves_pending =` welcomed set;
  clear `commits_by_epoch`, `epoch_authenticator`, `tree_hash`,
  `last_commit_epoch_by_author`, stale `welcomes`; zero both counters; set
  `reset_pending = true`, `reset_expected_tree_hash = None`.

- [ ] **Step 1: Write the failing tests.** Names = spec invariants:

- `sealed_post_requires_e2ee_class_and_a_confirmed_leaf` — `MessagePostedE2ee`
  into a Plaintext channel rejected; into an unknown channel rejected; by a
  member whose leaf is only pending rejected; by a confirmed-leaf member at the
  current epoch accepted; at a stale epoch rejected.
- `ban_then_pending_removals_gate_blocks_sealed_sends_until_rekey` — the spec's
  ban → gate → rekey sequence: member banned ⇒ `pending_removals` non-empty ⇒
  every member's `MessagePostedE2ee` rejected ⇒ a Remove-commit discharging the
  drift folds ⇒ sends accepted again.
- `freshness_ceiling_seals_the_channel_until_somebody_rekeys` — fold 500
  sealed posts after a commit: the 501st channel event is rejected; a
  self-update `MlsCommit` resets the counter; sends resume (spec C4/I1: FS
  becomes an invariant a blind host enforces).
- `sealed_edit_shares_every_send_gate_and_respects_tombstones` — edit passes
  where a post passes; blocked by pending-removals/ceiling identically; edit of
  a tombstoned target rejected.
- `reset_must_welcome_exactly_the_folds_member_set` — staged reset Welcomes
  missing one device ⇒ reset rejected; with an extra non-member device ⇒
  rejected; exact cover ⇒ accepted (non-selective reset — the unbounded
  unlogged eviction is structurally impossible).
- `partial_reset_leaves_the_channel_dead_until_all_leaves_confirm` — after an
  accepted reset, sealed sends rejected while `reset_pending`; confirmations
  arrive (first seeds the expected tree hash, a mismatching one is rejected);
  once `leaves_confirmed == members × live_devices`, sends unlock.
- `reset_is_owner_only_and_rate_limited` — non-owner reset rejected; a second
  reset before `RESET_MIN_CHANNEL_EVENTS` further channel events rejected.

- [ ] **Step 2:** Run one to confirm FAIL. **Step 3: Implement.**
- [ ] **Step 4:** `cargo test -p farder-crypto` — green.
- [ ] **Step 5:** Update `docs/modules/crypto.md`; commit —
  `feat(crypto): rung2 fold — sealed-content send gates, freshness ceiling, non-selective reset`

---

## Task 5: Checkpoint composability over all new state + real-OpenMLS chaining integration

**Files:** modify `crates/farder-crypto/src/event_log_state.rs`; create
`crates/farder-mls/tests/fold_chain.rs`; modify `docs/modules/crypto.md` +
`docs/modules/farder-mls.md` (integration note).

- [ ] **Step 1: Extend the Rung-1 invariant test.** Extend
  `replay_equals_stepwise_and_composes_from_a_checkpoint` (or add
  `..._over_all_rung2_state` beside it) to fold a log exercising EVERY new
  variant: channel creation (both classes), KeyPackage publishes, epoch-0
  commit, add-commit + Welcome + `MlsLeafConfirmed`, sealed posts/edit,
  tombstone, `DeviceRevoked`, a stale-commit no-op, staged Welcomes + reset +
  post-reset confirmations. Assert `replay == stepwise == checkpoint-resumed`
  (clone mid-log, apply the tail) across every query surface:
  `channel_class`, `mls_current_epoch`, `leaves_confirmed`, `pending_adds`,
  `pending_removals`, `is_tombstoned`, `is_device_revoked`, and (test-visible)
  the group record fields incl. `epoch_authenticator`, counters, `log_pos`.
  This is the spec's "extended `replay_equals_stepwise_and_composes_from_a_checkpoint`
  over all new state" plus its "commit-race determinism": the same log
  (winner + recorded stale no-op) folds identically from any checkpoint.
- [ ] **Step 2: Real-MLS integration** (`crates/farder-mls/tests/fold_chain.rs`
  — farder-mls already depends on farder-crypto; use the in-memory provider):

- `real_openmls_commits_chain_through_the_fold` — drive `MlsChannelGroup`
  (creator + two joiners): each real `CommitOutcome` + post-merge
  `epoch_authenticator()` is wrapped into an `MlsCommit` event
  (`prev_epoch_authenticator` from the outcome, `post_epoch_authenticator`
  from the accessor, `post_tree_hash`, `epoch` per the authored-epoch
  convention) and folded; joiners' `JoinInfo { epoch, tree_hash }` feed
  `MlsLeafConfirmed`; assert the fold accepts the whole chain and
  `leaves_confirmed` converges to the real membership — proving the fold's
  chain variables and epoch conventions match real OpenMLS values, not just
  each other.
- `fold_rejects_a_commit_that_does_not_chain_onto_the_declared_authenticator` —
  fold a real commit k, then a real commit k+1 whose event declares a
  tampered `prev_epoch_authenticator`: rejected; the honest declaration:
  accepted (the blind chain check agrees with MLS reality).

- [ ] **Step 3:** `cargo test -p farder-crypto && cargo test -p farder-mls`,
  then `cargo test --workspace` (regression gate: nothing emits the new
  variants, so server/client behavior is byte-identical — dormancy verified by
  the whole suite staying green). The client crate builds separately:
  `cd client/src-tauri && cargo build`.
- [ ] **Step 4:** Final docs pass per the CLAUDE.md checklist:
  `docs/modules/crypto.md` documents the full new `LogState` surface (queries,
  derived sets, comparator, constants, clocks, the resolved ambiguities that
  are now code contracts); `docs/modules/farder-mls.md` gains a one-line
  pointer to the fold-integration tests. Same commit.
- [ ] **Step 5: Commit** —
  `feat(crypto): rung2 fold — checkpoint composability over all MLS state + real-OpenMLS chain integration`

---

## Self-Review

**Spec sub-2 scope coverage:**
- All new `EventPayload` variants land dormant (`ChannelCreated`,
  `MlsKeyPackagePublished`, `MlsCommit`, `MlsWelcome`, `MlsLeafConfirmed`,
  `MlsGroupReset`, `MessagePostedE2ee`, `MessageEditedE2ee`, `MessageDeleted`,
  `DeviceRevoked`, `DeviceCert.expires_at`) — Task 1. `AuthzBeacon` correctly
  excluded (sealed app message, not a log event). ✅
- Fold rules: class gating fail-closed (T2), epoch CAS + authenticator chaining
  + stale-no-op (T3), pending/confirmed leaves + joiner confirmation (T3),
  `pending_removals` send gate + staleness ceiling (T4), commit-rate rule +
  drift-priority tiebreak grind resistance (T3), device/KeyPackage caps +
  lifetimes (T2/T3), tombstones (T2/T4), reset completeness + rate limit (T4),
  instance-hash pinning (T3). ✅
- Spec-named tests present: extended
  `replay_equals_stepwise_and_composes_from_a_checkpoint`, commit-race
  determinism, grind-resistance, chaining-rejects-build-on-a-liar,
  ban → gate → rekey, reset-completeness. ✅
- Rung-1 constraints preserved: pure `(prior_state, event) -> new_state`, no
  wall clock (two pure clocks: event timestamp + `log_pos`), check-then-mutate,
  reuse of sub-1 crypto verbatim, old canonical bytes untouched (dedicated
  byte-stability tests for the enum append and the `DeviceCert` skip-none
  encoding). ✅
- Q8 honored: no migration machinery anywhere; the legacy carve-out exists only
  so existing post-Rung-1 logs keep replaying — it adds no backfill path. ✅
- farder-mls consumed per its REAL surface (`CommitOutcome`,
  `epoch_authenticator()`, `JoinInfo`, in-memory provider) — only in the
  integration test crate, never as a `farder-crypto` dependency; the server
  still never links OpenMLS. ✅

**Riskiest decisions, flagged for review:** (1) the `post_epoch_authenticator`
field addition (resolved ambiguity #1) — without it the spec's chain rule is
unimplementable blind; Task 5's real-MLS test is the proof it matches OpenMLS
reality; (2) the legacy plaintext carve-out (#2) — guarded by the
Rung-1-replay regression test; (3) the reset bootstrap conventions (#5–#7) —
each pinned by a dedicated test so sub-4/5 client code has an exact contract to
build against.

**Dormancy:** no server, protocol, or client file changes in this sub-project;
`cargo test --workspace` green at every commit is the observable evidence.
Runtime behavior first changes in sub-3 (ingest) — per the spec's
protocol-churn discipline (F15), sub-2 + sub-3 land all variants so sub-4/5/6/7
are behavior-only.
