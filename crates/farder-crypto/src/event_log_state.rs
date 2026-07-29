//! The authorization state machine: folds a server's signed event log into the
//! current membership / bans / capabilities / devices / invites, validating every
//! event against the per-payload signing rules. Pure (no I/O), so it replays
//! deterministically and composes from any checkpoint.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use anyhow::{bail, ensure, Context, Result};

use crate::event_log::{
    ChannelClass, DeleteReason, DeviceId, Event, EventHash, EventPayload, EventRef, Genesis,
    ServerId,
};
use crate::identity::PublicKey;

/// Spec C5/I8: at most this many live (non-revoked, cert-unexpired) devices per
/// identity. Revoking a device frees its slot — revoked ≠ live.
pub const MAX_LIVE_DEVICES_PER_IDENTITY: usize = 8;

/// Spec I5: at most this many live (unconsumed, `expires_at_log_pos > log_pos`)
/// KeyPackages per device. Expired refs do not count and are pruned on touch.
pub const MAX_LIVE_KEY_PACKAGES_PER_DEVICE: usize = 10;

/// Spec I3: a commit by author A in a channel is invalid unless it discharges
/// drift, is A's first commit there, or declares an epoch at least
/// `MlsGroupRecord::commit_rate_gap()` epochs past A's previous commit — stops
/// self-update spam from bouncing every other member's in-flight sealed message
/// with `stale-epoch`. This is the CEILING on that gap; the gap actually
/// enforced scales down with the number of identities that can take a turn (see
/// `commit_rate_gap`), because a raw gap of 4 is unsatisfiable in a channel with
/// fewer than 4 committing identities.
pub const COMMIT_RATE_MIN_EPOCH_GAP: u64 = 4;

/// Spec C4 + I3 (review round 3): once a channel's freshness budget is within
/// this many events of `FRESHNESS_CEILING_EVENTS`, the commit-rate rule stops
/// applying — a rekey the ceiling itself is demanding is never spam. Without
/// this hatch, a channel whose only online member has already taken its turn
/// burns its budget and seals permanently: nothing but a commit advances the
/// epoch, so nothing can ever satisfy an epoch-distance rule again. The hatch
/// cannot be milked, because every accepted commit zeroes the budget: it buys
/// at most one commit per `FRESHNESS_CEILING_EVENTS - GRACE` sealed events.
pub const COMMIT_RATE_CEILING_GRACE_EVENTS: u32 = 50;

/// Spec C4/I1: the blind rekey ceiling. Once this many channel events have
/// accumulated since the last accepted commit, sealed content becomes invalid —
/// the channel stops accepting new content until somebody rekeys, so forward
/// secrecy is an invariant a host that cannot read a word enforces.
pub const FRESHNESS_CEILING_EVENTS: u32 = 500;

/// Spec C7: reset rate limit — at most one `MlsGroupReset` per channel per this
/// many channel events. A channel's FIRST reset is always allowed.
pub const RESET_MIN_CHANNEL_EVENTS: u32 = 1000;

/// A device authorized within this server's log (identity ↔ signing subkey).
/// `PartialEq` (like the other fold records) exists for the checkpoint-
/// composability invariant test, which compares whole folded states field by
/// field — nothing in the fold itself compares records.
#[derive(Clone, Debug, PartialEq)]
struct DeviceRecord {
    identity: PublicKey,
    device_pubkey: PublicKey,
    /// Cert expiry (unix seconds) from the `DeviceAuthorized` cert, if any.
    /// NOT judged against the raw `event.core.timestamp` (which is entirely
    /// author-chosen): the envelope gate and `live_devices` judge it at
    /// `self_liveness_ts` (the identity's monotone floor), and every
    /// cross-identity derivation judges it at `judged_liveness_ts` (that floor,
    /// capped by the log's corroborated clock).
    expires_at: Option<u64>,
}

/// A channel known to the log (from `ChannelCreated`). The class is immutable:
/// no class-change event exists by construction. A channel ABSENT from this map
/// is a legacy DB channel — permanently plaintext (replay carve-out).
#[derive(Clone, Debug, PartialEq)]
struct ChannelRecord {
    /// name/kind/parent are recorded for sub-3's derive path; the fold itself
    /// reads only `class`, `creator` (and resolves `parent` at creation time).
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    kind: String,
    class: ChannelClass,
    #[allow(dead_code)]
    parent: Option<u64>,
    /// The identity that authored `ChannelCreated` (owner-only this rung). The
    /// ONLY identity allowed to author a generation's bootstrap `MlsCommit`,
    /// where no confirmed leaf exists to check against.
    creator: PublicKey,
}

/// One accepted `MlsCommit`, keyed in `MlsGroupRecord.commits_by_epoch` by the
/// epoch it CREATED (declared epoch + 1) — the epoch a joiner lands in.
#[derive(Clone, Debug, PartialEq)]
struct CommitRecord {
    event_hash: EventHash,
    post_tree_hash: [u8; 32],
}

/// One recorded `MlsWelcome`, keyed by its event hash. `MlsGroupReset`'s
/// completeness check resolves its `welcomes` refs against these.
#[derive(Clone, Debug, PartialEq)]
struct WelcomeRecord {
    generation: u64,
    for_member: PublicKey,
    for_device: DeviceId,
}

/// Per-channel MLS control-plane bookkeeping (spec sub-2). Created by
/// `ChannelCreated { class: E2ee }` at generation 0 / epoch 0. The fold never
/// runs MLS — it chains the values commits DECLARE (resolved ambiguity #1) so
/// a liar cannot be built upon.
#[derive(Clone, Debug, PartialEq)]
struct MlsGroupRecord {
    generation: u64,
    /// The epoch the group is IN (declared epoch + 1 of the last accepted commit).
    epoch: u64,
    #[allow(dead_code)] // read by sub-3 (stale-epoch bounce diagnostics)
    commit_head: Option<EventHash>,
    /// The last accepted commit's declared `post_epoch_authenticator` — the
    /// value the NEXT commit's `prev_epoch_authenticator` must equal. `None` =
    /// no commit accepted yet in this generation, which exempts that commit from
    /// the CHAIN check (there is nothing to chain to). It does NOT exempt the
    /// confirmed-leaf check: that one is keyed to `leaves_confirmed` being empty
    /// and is then creator-only, so a post-reset generation — whose
    /// `leaves_confirmed` already holds the resetter — is not up for grabs.
    epoch_authenticator: Option<[u8; 32]>,
    #[allow(dead_code)] // read by sub-3 (derive/diagnostics)
    tree_hash: Option<[u8; 32]>,
    leaves_confirmed: HashSet<(PublicKey, DeviceId)>,
    leaves_pending: HashSet<(PublicKey, DeviceId)>,
    /// Freshness-ceiling counter (spec C4): SEALED CONTENT ONLY since the last
    /// accepted commit — never tombstones, whose targets are opaque to the fold
    /// (spending the ceiling on them let any member seal an E2ee channel on
    /// demand with fabricated tombstones). Reset to 0 by every accepted commit
    /// and by a reset; `>= FRESHNESS_CEILING_EVENTS` seals the channel.
    events_since_last_commit: u32,
    /// Commit-rate clock (spec I3): the DECLARED epoch of each author's last
    /// accepted commit in this channel.
    last_commit_epoch_by_author: HashMap<PublicKey, u64>,
    /// Reset rate-limit clock (spec C7): channel events since the last accepted
    /// `MlsGroupReset`. A channel's first reset ignores it (generation 0).
    channel_events_since_reset: u32,
    /// The leaves an accepted `MlsGroupReset` STAGED (its Welcome set), minus
    /// the obligations that are VOID because the fold no longer owes their
    /// holder a leaf at all (banned, kicked, device revoked or cert-expired —
    /// pruned by the commit effect). The reset generation is incomplete — and
    /// sealed content invalid — exactly while one of these leaves is still
    /// unconfirmed (`reset_incomplete`). See that method for why this is a
    /// derived gate and not the latch it replaced.
    reset_welcomed: HashSet<(PublicKey, DeviceId)>,
    /// The reset generation's expected tree hash, as DECLARED by the resetter
    /// on `MlsGroupReset` (its add-commit is never a log event, so there is no
    /// `commits_by_epoch` entry to check against — resolved ambiguity #7). The
    /// resetter is the new group's creator by construction, so it is the one
    /// party that knows the real value; anchoring here rather than on the first
    /// confirmation stops one malicious welcomed device from poisoning every
    /// honest confirmation with a first-writer-wins bogus hash.
    reset_expected_tree_hash: Option<[u8; 32]>,
    /// Accepted commits keyed by the epoch each CREATED (declared + 1).
    commits_by_epoch: HashMap<u64, CommitRecord>,
    /// Recorded Welcomes, keyed by event hash (`MlsGroupReset` resolves its
    /// refs against these).
    welcomes: HashMap<EventHash, WelcomeRecord>,
}

impl MlsGroupRecord {
    fn new() -> Self {
        Self {
            generation: 0,
            epoch: 0,
            commit_head: None,
            epoch_authenticator: None,
            tree_hash: None,
            leaves_confirmed: HashSet::new(),
            leaves_pending: HashSet::new(),
            events_since_last_commit: 0,
            last_commit_epoch_by_author: HashMap::new(),
            channel_events_since_reset: 0,
            reset_welcomed: HashSet::new(),
            reset_expected_tree_hash: None,
            commits_by_epoch: HashMap::new(),
            welcomes: HashMap::new(),
        }
    }

    /// How many DISTINCT identities hold a confirmed leaf — i.e. how many
    /// parties can actually take a turn at committing in this group.
    fn committing_identities(&self) -> u64 {
        self.leaves_confirmed.iter().map(|(pk, _)| pk).collect::<HashSet<_>>().len() as u64
    }

    /// The commit-rate gap this group enforces (spec I3, corrected in review
    /// round 3): `min(COMMIT_RATE_MIN_EPOCH_GAP, committing identities)`.
    ///
    /// The rule is an EPOCH-distance rule, and every accepted commit advances
    /// the epoch by exactly one — so with M identities round-robining, an
    /// author's next turn arrives exactly M epochs later. A fixed gap of 4 is
    /// therefore unsatisfiable whenever M < 4: once every member had spent the
    /// one exempt "first commit", nobody could ever commit again, and 500 sealed
    /// events later the freshness ceiling sealed the channel permanently. The
    /// spec's own "#private with a friend" channel is M = 2.
    ///
    /// Scaling the gap to M keeps the property the rule exists for — an author
    /// still cannot take two turns in a row while anyone else holds a leaf — and
    /// makes the client rekey cadence reachable in small channels instead of
    /// leaving them dependent on the ceiling hatch. A one-identity group has
    /// nobody to bounce, so its gap is 1 (rekey freely).
    fn commit_rate_gap(&self) -> u64 {
        COMMIT_RATE_MIN_EPOCH_GAP.min(self.committing_identities().max(1))
    }

    /// Whether a group reset is still incomplete (spec C7) — the gate that makes
    /// a partial reset a dead channel, loudly, rather than a silent partition.
    ///
    /// DERIVED, deliberately: it is true exactly while a leaf the reset STAGED
    /// is still unconfirmed. The predecessor was a `bool` latch set by
    /// `MlsGroupReset` and cleared in ONE place — inside `MlsLeafConfirmed`,
    /// when `leaves_confirmed` reached `members × live_devices` — which made
    /// several ordinary sequences TERMINAL. Ban a welcomed device before it
    /// confirms and the bridge's own answer (a Remove-commit dropping the
    /// unproven leaf) emptied `pending_removals`, `pending_adds` and
    /// `pending_confirmations`, yet sealed sends stayed refused and no
    /// confirmation could ever arrive again; a member joining after the reset
    /// grew `members × live_devices` so the equality never held either. Escape
    /// required another owner-only reset, destroying continuity.
    ///
    /// As a predicate over leaf state it self-heals on every path that GENUINELY
    /// discharges the obligation, and it stays a pure function of state, so it
    /// composes from any checkpoint. There are exactly two such paths:
    ///
    /// - the staged leaf CONFIRMS (it lands in `leaves_confirmed` — the promise
    ///   the reset made is kept); or
    /// - the fold stops owing its holder a leaf at all (banned, kicked, device
    ///   revoked, cert expired), in which case the commit effect prunes it out
    ///   of `reset_welcomed` for good. That prune is also what stops a
    ///   discharged obligation from being RESURRECTED: when such a holder later
    ///   returns and is re-added, the pending leaf is an ordinary join, not a
    ///   revived reset obligation.
    ///
    /// Deliberately NOT a discharge path: a Remove-commit dropping a staged leaf
    /// whose holder is still in good standing. Removing a pending-only leaf is
    /// open to any member (an unproven Add is not a tree member — see the
    /// `DeclaredRemove` rule), and a generation's first commit is exempt from
    /// the rate rule, so keying the gate to `leaves_pending` alone let the FIRST
    /// welcomed device to confirm evict every peer that had not confirmed yet,
    /// reopen the channel, and leave them permanently outside it while
    /// `pending_adds` — which gates nothing — quietly listed them: precisely the
    /// silent partition spec C7 exists to make impossible. The evicted member's
    /// Add is simply re-driven, and the channel wakes when they hold their leaf.
    fn reset_incomplete(&self) -> bool {
        self.reset_welcomed.iter().any(|leaf| !self.leaves_confirmed.contains(leaf))
    }

    /// Whether the freshness ceiling is close enough to demand a rekey NOW
    /// (spec C4) — in which case the commit-rate rule stands aside.
    fn ceiling_demands_rekey(&self) -> bool {
        self.events_since_last_commit.saturating_add(COMMIT_RATE_CEILING_GRACE_EVENTS)
            >= FRESHNESS_CEILING_EVENTS
    }
}

/// Outcome of payload authorization. `StaleCommitNoOp` is an `MlsCommit` that
/// lost the epoch CAS: ACCEPTED (Ok, chain head + log_pos advance) but with
/// zero MLS state change — the Rung-3-deterministic no-op (resolved ambiguity
/// #8; ingest's distinct `stale-epoch` bounce is sub-3's job).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Authorized {
    Apply,
    StaleCommitNoOp,
}

/// An open invite, keyed in `LogState.invites` by its `InviteCreated` event hash.
#[derive(Clone, Debug, PartialEq)]
struct InviteRecord {
    max_uses: u32,
    expires_at: u64,
    use_count: u32,
    requires_approval: bool,
}

/// Head of one `(author, device)` chain.
#[derive(Clone, Debug, PartialEq)]
struct ChainHead {
    seq: u64,
    hash: EventHash,
}

/// Authorization state derived by folding a server's event log. Construct only
/// via `from_genesis`, then advance with `apply`.
#[derive(Clone, Debug)]
pub struct LogState {
    server_id: ServerId,
    owner: PublicKey,
    members: HashSet<PublicKey>,
    pending: HashSet<PublicKey>,
    banned: HashSet<PublicKey>,
    capabilities: HashMap<PublicKey, HashSet<String>>,
    devices: HashMap<DeviceId, DeviceRecord>,
    invites: HashMap<EventHash, InviteRecord>,
    chains: HashMap<(PublicKey, DeviceId), ChainHead>,
    /// content_hash -> first uploader seen (from MessagePosted caps); authz basis for self-takedown.
    attachment_uploaders: HashMap<String, PublicKey>,
    /// content hashes that have been redacted.
    redacted_attachments: HashSet<String>,
    /// Channels known to the log, from `ChannelCreated` (Rung 2). Absence =
    /// legacy plaintext channel.
    channels: HashMap<u64, ChannelRecord>,
    /// Channel ids the log has seen plaintext `MessagePosted` events in while
    /// they were UNKNOWN to it (the legacy carve-out). Such a channel can never
    /// be declared afterwards: a `ChannelCreated { E2ee }` over plaintext
    /// history would hang a lock icon on messages every host already read, and
    /// the fold — not just sub-3's `messages`-table check — must refuse it, so
    /// a Rung-3 replica replaying from genesis refuses it too.
    plaintext_history_channels: HashSet<u64>,
    /// Durable deletion tombstones (spec F2): targets of accepted `MessageDeleted`
    /// events — derive/reconcile consult these so deletions cannot resurrect.
    tombstones: HashSet<EventRef>,
    /// Devices killed by `DeviceRevoked`: cert dead, chain frozen (their history
    /// stands; new events from them are rejected at the envelope).
    revoked_devices: HashSet<DeviceId>,
    /// Every device ever authorized, per identity. Liveness (revoked/expired) is
    /// filtered by `live_devices`, never by removal — history must stand.
    devices_by_identity: HashMap<PublicKey, HashSet<DeviceId>>,
    /// Count of accepted events — the pure log-position clock (drives KeyPackage
    /// lifetimes; never wall time).
    log_pos: u64,
    /// Per-identity monotone timestamp floor: the greatest `core.timestamp` any
    /// accepted event of that identity has claimed. Device liveness/expiry for
    /// an identity is judged at `max(at_ts, floor)`, so an identity can never
    /// back-date its way past its own certs' expiry or the live-device cap
    /// (`event.core.timestamp` is author-chosen — see `live_devices`).
    identity_clock: HashMap<PublicKey, u64>,
    /// The log's **corroborated clock**: the greatest timestamp at least TWO
    /// distinct identities have claimed (the second-largest `identity_clock`
    /// value). Derived from `identity_clock` on every accepted event, so a
    /// checkpoint carries it and it can only move forward.
    ///
    /// It is the CEILING the fold judges one identity's device liveness at when
    /// ANOTHER identity's claimed timestamp asks the question — see
    /// `judged_liveness_ts`. A plain global maximum would not do: one author
    /// could raise it alone (with a single harmless forward-dated event) and
    /// then declare everybody else's expiring certs dead. Requiring two distinct
    /// identities means a lone compromised member cannot move the log's clock at
    /// all, while an ordinary server — where several identities keep claiming
    /// roughly-real times — tracks real time closely.
    corroborated_clock: u64,
    /// Per-E2ee-channel MLS bookkeeping, created by `ChannelCreated { E2ee }`.
    mls_groups: HashMap<u64, MlsGroupRecord>,
    /// Live (unconsumed) KeyPackages per (identity, device): ref → expiry log
    /// pos. Consuming an Add moves the ref to `consumed_key_packages`.
    key_packages: HashMap<(PublicKey, DeviceId), HashMap<EventRef, u64>>,
    /// Consumed KeyPackage refs → expiry log pos. Kept so a consumed ref can
    /// never be replayed as an Add target; prunable past expiry (spec I5).
    consumed_key_packages: HashMap<EventRef, u64>,
    /// The pinned MLS store-instance hash per device (spec C6): first
    /// `MlsKeyPackagePublished` pins it; any later publish/commit/confirm from
    /// the device with a different hash is rejected (clone/restore poison).
    device_store_instance: HashMap<DeviceId, [u8; 32]>,
}

impl LogState {
    /// Initial state: the genesis owner is the sole member and root of authority.
    pub fn from_genesis(g: &Genesis) -> Self {
        let mut members = HashSet::new();
        members.insert(g.owner.clone());
        Self {
            server_id: g.server_id(),
            owner: g.owner.clone(),
            members,
            pending: HashSet::new(),
            banned: HashSet::new(),
            capabilities: HashMap::new(),
            devices: HashMap::new(),
            invites: HashMap::new(),
            chains: HashMap::new(),
            attachment_uploaders: HashMap::new(),
            redacted_attachments: HashSet::new(),
            channels: HashMap::new(),
            plaintext_history_channels: HashSet::new(),
            tombstones: HashSet::new(),
            revoked_devices: HashSet::new(),
            devices_by_identity: HashMap::new(),
            log_pos: 0,
            identity_clock: HashMap::new(),
            corroborated_clock: 0,
            mls_groups: HashMap::new(),
            key_packages: HashMap::new(),
            consumed_key_packages: HashMap::new(),
            device_store_instance: HashMap::new(),
        }
    }

    pub fn server_id(&self) -> &ServerId {
        &self.server_id
    }
    pub fn owner(&self) -> &PublicKey {
        &self.owner
    }
    pub fn is_owner(&self, pk: &PublicKey) -> bool {
        pk == &self.owner
    }
    pub fn is_member(&self, pk: &PublicKey) -> bool {
        self.members.contains(pk)
    }
    /// A member who joined via an approval-required invite and has not yet been approved.
    pub fn is_pending(&self, pk: &PublicKey) -> bool {
        self.pending.contains(pk)
    }
    /// All members currently awaiting approval (for the approval queue / content gating).
    pub fn pending_members(&self) -> Vec<PublicKey> {
        self.pending.iter().cloned().collect()
    }
    pub fn is_banned(&self, pk: &PublicKey) -> bool {
        self.banned.contains(pk)
    }
    /// The owner holds every capability; everyone else holds only what was granted.
    pub fn has_capability(&self, pk: &PublicKey, cap: &str) -> bool {
        self.is_owner(pk) || self.capabilities.get(pk).is_some_and(|c| c.contains(cap))
    }
    /// Whether this attachment (by content hash) has been redacted.
    pub fn is_attachment_redacted(&self, hash: &str) -> bool {
        self.redacted_attachments.contains(hash)
    }
    /// The recorded (first) uploader of an attachment hash, if any MessagePosted cited it.
    pub fn attachment_uploader(&self, hash: &str) -> Option<&PublicKey> {
        self.attachment_uploaders.get(hash)
    }
    /// The class of a channel known to the log. `None` = no `ChannelCreated`
    /// exists — a legacy DB channel (permanently plaintext) or no channel at all.
    pub fn channel_class(&self, channel_id: u64) -> Option<ChannelClass> {
        self.channels.get(&channel_id).map(|c| c.class)
    }
    /// Whether an accepted `MessageDeleted` tombstone exists for this event ref.
    pub fn is_tombstoned(&self, target: &str) -> bool {
        self.tombstones.contains(target)
    }
    /// Whether this device has been killed by an accepted `DeviceRevoked`.
    pub fn is_device_revoked(&self, device: &str) -> bool {
        self.revoked_devices.contains(device)
    }
    /// The log-position clock: number of accepted events folded so far.
    pub fn log_pos(&self) -> u64 {
        self.log_pos
    }
    /// An identity's live devices as judged AT ITS OWN CLOCK: authorized,
    /// non-revoked, and cert-unexpired at `self_liveness_ts(pk, at_ts)` — NOT at
    /// the raw `at_ts`. Sorted so the result is deterministic for every fold
    /// consumer.
    ///
    /// `at_ts` comes from `event.core.timestamp`, which is entirely
    /// author-chosen; judging expiry there alone let an identity register past
    /// the live-device cap (claim a future time so its own certs look dead).
    /// Liveness is therefore judged at a MONOTONE point: the identity's own
    /// clock floor (`identity_clock`) can only move forward, so an identity can
    /// never be judged at a moment earlier than one it already claimed — the
    /// cap holds at *every* timestamp.
    ///
    /// This is the SELF judgment (and the diagnostic query): an identity is
    /// entitled to move its own clock forward, because doing so only kills its
    /// own devices. When one identity's claim asks about ANOTHER identity's
    /// devices — every `members × live_devices` derivation, every declared
    /// add/remove — the fold uses `judged_live_devices` instead, which is
    /// additionally capped by the log's corroborated clock.
    pub fn live_devices(&self, pk: &PublicKey, at_ts: u64) -> Vec<DeviceId> {
        self.live_devices_at(pk, self.self_liveness_ts(pk, at_ts))
    }

    /// An identity's live devices as the FOLD judges them for cross-identity
    /// decisions: at `judged_liveness_ts` (the identity's own floor, capped by
    /// the log's corroborated clock).
    fn judged_live_devices(&self, pk: &PublicKey, at_ts: u64) -> Vec<DeviceId> {
        self.live_devices_at(pk, self.judged_liveness_ts(pk, at_ts))
    }

    /// Whether the fold still OWES this leaf a place in the tree: its holder is
    /// a full member, not banned, and the device is live as the fold judges it.
    /// The single definition behind both the `DeclaredRemove` bridge rule and
    /// the reset-obligation prune, so "removal is legitimate" and "the reset no
    /// longer owes this leaf" can never drift apart.
    fn leaf_holder_in_good_standing(
        &self,
        identity: &PublicKey,
        device: &DeviceId,
        at_ts: u64,
    ) -> bool {
        self.is_member(identity)
            && !self.is_banned(identity)
            && self.judged_live_devices(identity, at_ts).contains(device)
    }

    /// The shared filter: devices of `pk` that are neither revoked nor
    /// cert-expired at the already-resolved judgment point `at`.
    fn live_devices_at(&self, pk: &PublicKey, at: u64) -> Vec<DeviceId> {
        let Some(devs) = self.devices_by_identity.get(pk) else {
            return Vec::new();
        };
        let mut live: Vec<DeviceId> = devs
            .iter()
            .filter(|d| !self.revoked_devices.contains(*d))
            .filter(|d| {
                self.devices
                    .get(*d)
                    .and_then(|r| r.expires_at)
                    .is_none_or(|t| at <= t)
            })
            .cloned()
            .collect();
        live.sort();
        live
    }

    /// The monotone point an identity's OWN claim is judged at: never earlier
    /// than the greatest timestamp that identity itself has claimed.
    fn self_liveness_ts(&self, pk: &PublicKey, at_ts: u64) -> u64 {
        at_ts.max(self.identity_floor(pk))
    }

    /// The point the fold judges identity `pk`'s device liveness at when the
    /// question is asked by someone else's claimed timestamp:
    /// `clamp(at_ts, identity_floor(pk), corroborated_clock)` — written as
    /// `max(floor, min(at_ts, ceiling))` because the floor may legitimately sit
    /// above the ceiling (an identity that has run ahead of the log).
    ///
    /// The lower bound stops back-dating below what `pk` itself has already
    /// claimed; the upper bound stops FORWARD-dating, which was the live hole:
    /// a commit author claiming a far-future `core.timestamp` made every OTHER
    /// member's expiring cert look dead, so `good_standing` collapsed and the
    /// non-selective-removal rule (spec C7) authorized a silent eviction — and
    /// the same claim shrank `members × live_devices` for `pending_adds`,
    /// `commit_discharges_drift` and the reset's exact-cover check. A claimed
    /// timestamp is now only credible for judging OTHER identities up to the
    /// moment the log itself corroborates (`corroborated_clock`: claimed by at
    /// least two distinct identities).
    ///
    /// **Residual (documented, not closed here):** two colluding identities can
    /// still push the corroborated clock forward, and an author can still
    /// back-date below the expiry of a different identity that went silent
    /// before its cert died and never claimed anything after it. Sub-3 bounds
    /// `core.timestamp` against server time at ingest.
    fn judged_liveness_ts(&self, pk: &PublicKey, at_ts: u64) -> u64 {
        self.identity_floor(pk).max(at_ts.min(self.corroborated_clock))
    }

    fn identity_floor(&self, pk: &PublicKey) -> u64 {
        self.identity_clock.get(pk).copied().unwrap_or(0)
    }

    /// The spec's MLS target set: every full member's live devices, judged at
    /// the fold's cross-identity liveness point. Pending (unapproved) members
    /// are not in `members`, so they are excluded.
    fn member_leaf_set(&self, at_ts: u64) -> HashSet<(PublicKey, DeviceId)> {
        let mut set = HashSet::new();
        for m in &self.members {
            for d in self.judged_live_devices(m, at_ts) {
                set.insert((m.clone(), d));
            }
        }
        set
    }

    /// Spec-derived pure function: leaves the group holds that the authz fold
    /// says should NOT be there — `(confirmed ∪ pending) \ (members × live_devices)`.
    /// Non-empty ⇒ sealed sends are blocked until a Remove-commit discharges
    /// the drift. Empty for channels without an MLS group.
    pub fn pending_removals(&self, channel_id: u64, at_ts: u64) -> HashSet<(PublicKey, DeviceId)> {
        let Some(group) = self.mls_groups.get(&channel_id) else {
            return HashSet::new();
        };
        let target = self.member_leaf_set(at_ts);
        group
            .leaves_confirmed
            .union(&group.leaves_pending)
            .filter(|leaf| !target.contains(*leaf))
            .cloned()
            .collect()
    }

    /// The complement of `pending_removals`: member devices the group should
    /// hold but does not — `(members × live_devices) \ (confirmed ∪ pending)`.
    pub fn pending_adds(&self, channel_id: u64, at_ts: u64) -> HashSet<(PublicKey, DeviceId)> {
        let Some(group) = self.mls_groups.get(&channel_id) else {
            return HashSet::new();
        };
        self.member_leaf_set(at_ts)
            .into_iter()
            .filter(|leaf| {
                !group.leaves_confirmed.contains(leaf) && !group.leaves_pending.contains(leaf)
            })
            .collect()
    }

    /// The group's UNCONFIRMED leaves: declared by an accepted Add-commit, never
    /// promoted by an `MlsLeafConfirmed`. This is the retry obligation, and it
    /// is deliberately NOT visible in `pending_adds` (a pending leaf is excluded
    /// from that set by the spec's own formula), so a steward that only watched
    /// `pending_adds` would never learn that a joiner's Welcome never worked.
    /// A leaf that lingers here is re-driven by removing it (permitted for
    /// pending-only leaves) and re-adding with a fresh KeyPackage.
    /// Empty for channels without an MLS group.
    pub fn pending_confirmations(&self, channel_id: u64) -> HashSet<(PublicKey, DeviceId)> {
        self.mls_groups
            .get(&channel_id)
            .map(|g| g.leaves_pending.clone())
            .unwrap_or_default()
    }

    /// The (generation, epoch) an E2ee channel's group is currently in — sub-3's
    /// stale-epoch pre-check. `None` = the channel has no MLS group.
    pub fn mls_current_epoch(&self, channel_id: u64) -> Option<(u64, u64)> {
        self.mls_groups.get(&channel_id).map(|g| (g.generation, g.epoch))
    }

    /// The joiner-confirmed leaves of a channel's group (drift detection runs on
    /// these, never on pending leaves). Empty for channels without a group.
    pub fn leaves_confirmed(&self, channel_id: u64) -> HashSet<(PublicKey, DeviceId)> {
        self.mls_groups
            .get(&channel_id)
            .map(|g| g.leaves_confirmed.clone())
            .unwrap_or_default()
    }

    /// Whether an `MlsCommit` event discharges an outstanding fold obligation:
    /// at least one declared add ∈ `pending_adds` or remove ∈ `pending_removals`
    /// (at the event's claimed timestamp). `false` for non-commit payloads.
    pub fn commit_discharges_drift(&self, event: &Event) -> bool {
        let EventPayload::MlsCommit { channel_id, adds, removes, .. } = &event.core.payload
        else {
            return false;
        };
        let at_ts = event.core.timestamp;
        let padds = self.pending_adds(*channel_id, at_ts);
        let premoves = self.pending_removals(*channel_id, at_ts);
        adds.iter().any(|a| padds.contains(&(a.identity.clone(), a.device.clone())))
            || removes.iter().any(|r| premoves.contains(&(r.identity.clone(), r.device.clone())))
    }

    /// The drift-priority tiebreak for two same-epoch candidate commits (spec
    /// I2, exported pure for Rung 3's orderer): an obligation-discharging commit
    /// orders FIRST regardless of hash grinding or self-asserted lamport; when
    /// neither or both discharge, canonical order `(lamport, author, event_hash)`.
    pub fn compare_same_epoch_commits(&self, a: &Event, b: &Event) -> Ordering {
        match (self.commit_discharges_drift(a), self.commit_discharges_drift(b)) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => (a.core.lamport, a.core.author.as_bytes(), a.hash())
                .cmp(&(b.core.lamport, b.core.author.as_bytes(), b.hash())),
        }
    }

    /// Spec C6: if the device has a pinned store-instance hash, `hash` must
    /// match it (a mismatch is the clone/restore poison signal). An unpinned
    /// device passes — the effect pins it (matches-or-pins semantics).
    fn check_instance_pin(&self, device: &DeviceId, hash: &[u8; 32]) -> Result<()> {
        if let Some(pinned) = self.device_store_instance.get(device) {
            ensure!(
                pinned == hash,
                "store_instance_hash does not match this device's pinned instance (possible clone/restore)"
            );
        }
        Ok(())
    }

    fn pin_instance(&mut self, device: &DeviceId, hash: &[u8; 32]) {
        self.device_store_instance.entry(device.clone()).or_insert(*hash);
    }

    /// Fold a genesis + an ordered slice of events into the resulting state,
    /// rejecting on the first invalid event. Equivalent to `from_genesis` then
    /// `apply` in sequence.
    pub fn replay(genesis: &Genesis, events: &[Event]) -> Result<Self> {
        let mut state = Self::from_genesis(genesis);
        for event in events {
            state.apply(event)?;
        }
        Ok(state)
    }

    /// Validate and apply one event. CHECK-THEN-MUTATE: every fallible check runs
    /// before any mutation, so on `Err` the state is unchanged.
    pub fn apply(&mut self, event: &Event) -> Result<()> {
        // --- Envelope (payload-independent) ---
        ensure!(
            event.core.server_id == self.server_id,
            "event server_id does not match this server"
        );
        // Device binding (M1): derive the signing key from a verified cert bound
        // to this author/device, then verify the event signature under it.
        let device_pubkey = self.resolve_device_pubkey(event)?;
        event.verify(&device_pubkey).context("event signature is invalid")?;
        // Revocation gate (Rung 2): a revoked device's chain is frozen — its
        // history stands, but it can author nothing new (not even a fresh
        // DeviceAuthorized: the cert is dead for good).
        ensure!(
            !self.revoked_devices.contains(&event.core.device),
            "device has been revoked"
        );
        // Cert-expiry gate (Rung 2): an expired cert cannot author events.
        // Judged at the author's MONOTONE clock — `max(core.timestamp, the
        // greatest timestamp this identity has already claimed)` — not at the
        // raw claim. The raw claim alone let a dead device keep full
        // control-plane authority: an identity whose clock had already passed
        // its cert's expiry could still author `MlsCommit` / `MlsWelcome` /
        // `MlsLeafConfirmed` from that device simply by back-dating (nothing in
        // the chain forces timestamp monotonicity), zeroing
        // `events_since_last_commit` — and with it the C4 freshness ceiling —
        // and rewriting the chain variable at will.
        // For DeviceAuthorized the expiry comes from the payload cert itself
        // (self-bootstrap: registering with an already-expired cert is invalid).
        let cert_expiry = match &event.core.payload {
            EventPayload::DeviceAuthorized { cert } => cert.core.expires_at,
            _ => self.devices.get(&event.core.device).and_then(|r| r.expires_at),
        };
        if let Some(t) = cert_expiry {
            ensure!(
                self.self_liveness_ts(&event.core.author, event.core.timestamp) <= t,
                "device cert has expired"
            );
        }
        // Ban gate: a banned identity cannot act from any device.
        ensure!(!self.is_banned(&event.core.author), "author is banned");
        // Per-(author, device) chain continuity.
        self.check_chain(event)?;

        // --- Payload authorization (read-only) ---
        let authorized = self.check_payload_authz(event)?;

        // --- Effects (only reached once every check passed) ---
        // A stale commit (lost epoch CAS) is ACCEPTED but skips payload effects:
        // only the chain head and log_pos (both envelope accounting, not MLS
        // state) advance — the deterministic no-op (resolved ambiguity #8).
        if authorized == Authorized::Apply {
            self.apply_payload_effect(event, &device_pubkey);
        }
        self.advance_chain(event);
        self.log_pos += 1;
        // Envelope accounting (runs for the stale-commit no-op too): the
        // author's monotone timestamp floor. Once an identity has claimed a
        // moment, none of its later events can be judged as if they were
        // earlier — see `live_devices`.
        let floor = self.identity_clock.entry(event.core.author.clone()).or_insert(0);
        *floor = (*floor).max(event.core.timestamp);
        // ...and the log's corroborated clock (second-largest floor). Recomputed
        // AFTER the event's checks, so an author's own claim never raises the
        // ceiling its own event is judged against.
        self.recompute_corroborated_clock();
        Ok(())
    }

    /// Refresh `corroborated_clock` = the greatest timestamp claimed by at least
    /// two DISTINCT identities (the second-largest `identity_clock` value). One
    /// scan of a small map per accepted event; keeping it precomputed keeps the
    /// derived-set queries O(members) rather than O(members × identities).
    fn recompute_corroborated_clock(&mut self) {
        let (mut top1, mut top2) = (0u64, 0u64);
        for &v in self.identity_clock.values() {
            if v > top1 {
                top2 = top1;
                top1 = v;
            } else if v > top2 {
                top2 = v;
            }
        }
        self.corroborated_clock = top2;
    }

    /// M1: the signing device's public key, derived ONLY from a verified cert.
    /// For `DeviceAuthorized`, the cert is in the payload (self-bootstrap); for any
    /// other event the device must already be recorded and bound to this author.
    fn resolve_device_pubkey(&self, event: &Event) -> Result<PublicKey> {
        if let EventPayload::DeviceAuthorized { cert } = &event.core.payload {
            cert.verify().context("device cert is invalid")?;
            ensure!(
                cert.core.identity == event.core.author,
                "device cert identity does not match event author"
            );
            ensure!(
                cert.core.device_id == event.core.device,
                "device cert device_id does not match event device"
            );
            return Ok(cert.core.device_pubkey.clone());
        }
        let rec = self
            .devices
            .get(&event.core.device)
            .context("event signed by an unauthorized device")?;
        ensure!(
            rec.identity == event.core.author,
            "device is authorized to a different identity"
        );
        Ok(rec.device_pubkey.clone())
    }

    fn check_chain(&self, event: &Event) -> Result<()> {
        let key = (event.core.author.clone(), event.core.device.clone());
        match self.chains.get(&key) {
            None => {
                ensure!(event.core.seq == 0, "first event of a chain must have seq 0");
                ensure!(
                    event.core.prev.is_none(),
                    "first event of a chain must have prev = None"
                );
            }
            Some(head) => {
                ensure!(
                    event.core.seq == head.seq + 1,
                    "non-contiguous seq (gap or fork)"
                );
                ensure!(
                    event.core.prev.as_deref() == Some(head.hash.as_str()),
                    "prev does not link to the chain head (fork)"
                );
            }
        }
        Ok(())
    }

    fn advance_chain(&mut self, event: &Event) {
        let key = (event.core.author.clone(), event.core.device.clone());
        self.chains
            .insert(key, ChainHead { seq: event.core.seq, hash: event.hash() });
    }

    /// Per-payload authorization (read-only — CHECK-THEN-MUTATE: no state may
    /// change here). Returns the verdict `apply` acts on: normally `Apply`;
    /// `StaleCommitNoOp` for an `MlsCommit` that lost the epoch CAS.
    fn check_payload_authz(&self, event: &Event) -> Result<Authorized> {
        let author = &event.core.author;
        match &event.core.payload {
            EventPayload::DeviceAuthorized { .. } => {
                // Live-device cap (spec C5): live = non-revoked + cert-unexpired
                // at this event's (untrusted) timestamp — revoked or expired
                // devices free their slot.
                ensure!(
                    self.live_devices(author, event.core.timestamp).len()
                        < MAX_LIVE_DEVICES_PER_IDENTITY,
                    "identity already has the maximum number of live devices"
                );
                Ok(Authorized::Apply)
            }

            EventPayload::InviteCreated { .. } => {
                ensure!(self.has_capability(author, "invite"), "missing 'invite' capability");
                Ok(Authorized::Apply)
            }

            EventPayload::MemberJoined { member, invite } => {
                ensure!(member == author, "MemberJoined must be self-authored");
                ensure!(!self.is_member(author), "already a member");
                ensure!(!self.is_pending(author), "already pending approval");
                let inv = self.invites.get(invite).context("join cites an unknown invite")?;
                ensure!(inv.use_count < inv.max_uses, "invite has no uses left");
                ensure!(event.core.timestamp <= inv.expires_at, "invite has expired");
                Ok(Authorized::Apply)
            }

            EventPayload::MemberApproved { member } => {
                ensure!(self.has_capability(author, "kick"), "missing 'kick' capability");
                ensure!(self.is_pending(member), "target is not pending approval");
                Ok(Authorized::Apply)
            }

            EventPayload::MessagePosted { channel_id, .. } => {
                ensure!(self.is_member(author), "only members may post");
                // Class gate (fail closed where it matters): a channel the log
                // knows as E2ee never accepts plaintext posts. A channel UNKNOWN
                // to the log is a legacy plaintext channel and stays writable —
                // the replay carve-out that keeps pre-Rung-2 logs folding
                // (plan resolved ambiguity #2, spec Q8 fresh-servers-only).
                if let Some(ch) = self.channels.get(channel_id) {
                    ensure!(
                        ch.class == ChannelClass::Plaintext,
                        "plaintext MessagePosted is invalid in an E2ee channel"
                    );
                }
                Ok(Authorized::Apply)
            }

            EventPayload::MemberRemoved { member } => {
                ensure!(
                    self.is_member(member) || self.is_pending(member),
                    "target is neither a member nor pending"
                );
                ensure!(
                    member == author || self.has_capability(author, "kick"),
                    "must be the member (leave) or hold 'kick'"
                );
                Ok(Authorized::Apply)
            }

            EventPayload::MemberBanned { member } => {
                ensure!(self.has_capability(author, "ban"), "missing 'ban' capability");
                ensure!(!self.is_owner(member), "the owner cannot be banned");
                Ok(Authorized::Apply)
            }

            EventPayload::MemberUnbanned { .. } => {
                ensure!(self.has_capability(author, "ban"), "missing 'ban' capability");
                Ok(Authorized::Apply)
            }

            EventPayload::PermissionGranted { member, capability } => {
                ensure!(self.is_member(member), "grantee is not a member");
                ensure!(
                    self.is_owner(author) || self.has_capability(author, capability),
                    "cannot grant a capability you do not hold"
                );
                Ok(Authorized::Apply)
            }
            EventPayload::AttachmentRedacted { content_hash } => {
                let uploader = self
                    .attachment_uploaders
                    .get(content_hash)
                    .context("redaction cites an unknown attachment")?;
                ensure!(
                    author == uploader || self.has_capability(author, "kick"),
                    "must be the uploader or hold 'kick'"
                );
                ensure!(
                    !self.redacted_attachments.contains(content_hash),
                    "attachment already redacted"
                );
                Ok(Authorized::Apply)
            }

            EventPayload::ChannelCreated { channel_id, class, parent, .. } => {
                // Owner-only this rung (spec M3) — no new capability string.
                ensure!(self.is_owner(author), "only the owner may create channels");
                // channel_id is immutable identity: the class is set exactly once
                // at creation or the channel does not exist to the log (no
                // class-change event exists by construction).
                ensure!(
                    !self.channels.contains_key(channel_id),
                    "channel_id already exists (channel identity is immutable)"
                );
                // ...and a channel that already carried plaintext history under
                // the legacy carve-out can never be declared afterwards. Without
                // this the fold would accept `ChannelCreated { E2ee }` over a
                // plaintext log — a lock icon on messages every host already
                // read. The fold rule is self-sufficient (Rung-3 fresh replay),
                // not just sub-3's `messages`-table belt-and-braces.
                ensure!(
                    !self.plaintext_history_channels.contains(channel_id),
                    "channel already has plaintext history in the log (a legacy channel is permanently plaintext)"
                );
                // Thread children inherit their parent's class (spec coexistence
                // row 12): an unknown or class-mismatched parent is rejected.
                if let Some(p) = parent {
                    let parent_rec = self
                        .channels
                        .get(p)
                        .context("thread parent channel is unknown to the log")?;
                    ensure!(
                        parent_rec.class == *class,
                        "thread child must inherit its parent's class"
                    );
                }
                Ok(Authorized::Apply)
            }

            EventPayload::MessageDeleted { channel_id, target, reason } => {
                // Log deletes are for log channels: a channel with no
                // ChannelCreated is unknown to the log and cannot be moderated
                // through it.
                ensure!(
                    self.channels.contains_key(channel_id),
                    "MessageDeleted cites a channel unknown to the log"
                );
                ensure!(!self.tombstones.contains(target), "target already tombstoned");
                match reason {
                    // The fold verifies moderation authority itself.
                    DeleteReason::Moderation => {
                        ensure!(self.has_capability(author, "kick"), "missing 'kick' capability");
                    }
                    // Verifying an Author claim needs target authorship, which
                    // needs a per-message index the fold's state deliberately
                    // omits — ingest (sub-3) checks authorship against the
                    // derived `messages` table; the fold gates membership only.
                    DeleteReason::Author => {
                        ensure!(self.is_member(author), "only members may delete their messages");
                    }
                }
                Ok(Authorized::Apply)
            }

            EventPayload::DeviceRevoked { device } => {
                let rec = self
                    .devices
                    .get(device)
                    .context("revocation cites an unknown device")?;
                ensure!(!self.revoked_devices.contains(device), "device already revoked");
                // The owning identity kills its own device (from any of its
                // devices, including the revoked one itself — self-revoke), or
                // the server owner does, for abuse.
                ensure!(
                    *author == rec.identity || self.is_owner(author),
                    "only the owning identity or the server owner may revoke a device"
                );
                Ok(Authorized::Apply)
            }

            EventPayload::MlsKeyPackagePublished { store_instance_hash, expires_at_log_pos, .. } => {
                // Full members only (approved, non-pending — pending identities
                // are never in `members`, checked explicitly for defense).
                ensure!(
                    self.is_member(author) && !self.is_pending(author),
                    "only full members may publish key packages"
                );
                self.check_instance_pin(&event.core.device, store_instance_hash)?;
                // Log-position lifetime (spec I5): must expire in the future.
                ensure!(
                    *expires_at_log_pos > self.log_pos,
                    "key package is already expired at its publish log position"
                );
                // Live cap: expired refs do not count (they are pruned on touch,
                // in the effect — never here: authz is read-only).
                let live = self
                    .key_packages
                    .get(&(author.clone(), event.core.device.clone()))
                    .map_or(0, |m| m.values().filter(|&&exp| exp > self.log_pos).count());
                ensure!(
                    live < MAX_LIVE_KEY_PACKAGES_PER_DEVICE,
                    "device already has the maximum number of live key packages"
                );
                Ok(Authorized::Apply)
            }

            EventPayload::MlsCommit {
                channel_id,
                generation,
                epoch,
                adds,
                removes,
                prev_epoch_authenticator,
                store_instance_hash,
                ..
            } => {
                // Groups exist only for E2ee channels, so presence covers the
                // class gate (fail closed: Plaintext/unknown channel ⇒ no group).
                let group = self
                    .mls_groups
                    .get(channel_id)
                    .context("MlsCommit cites a channel with no E2ee group")?;
                ensure!(
                    *generation == group.generation,
                    "commit generation does not match the group"
                );
                // Epoch CAS (spec: one winner per epoch). A loser is an ACCEPTED
                // no-op — recorded, ignored, zero MLS state change — so any
                // converged event set folds identically on every replica.
                if *epoch != group.epoch {
                    return Ok(Authorized::StaleCommitNoOp);
                }
                self.check_instance_pin(&event.core.device, store_instance_hash)?;
                // Control-plane authority is re-checked against the authz fold
                // on EVERY commit, never inferred from leaf membership alone: a
                // kicked or departed identity keeps its confirmed leaf until a
                // Remove-commit lands, and must not be able to advance the
                // epoch, rewrite the chain variable or reset the freshness
                // budget in the meantime.
                ensure!(
                    self.is_member(author) && !self.is_pending(author),
                    "only full members may author MLS commits"
                );
                let leaf = (author.clone(), event.core.device.clone());
                if group.leaves_confirmed.is_empty() {
                    // Generation bootstrap (resolved ambiguity #5): nobody holds
                    // a confirmed leaf yet, so the confirmed-leaf rule cannot
                    // apply — the channel's CREATOR (owner-only this rung) is
                    // then the only identity that may author it. Without this,
                    // ANY identity that can register a device could seize a
                    // fresh `ChannelCreated { E2ee }` group (or any post-reset
                    // generation), brick it for its real creator, and hold a
                    // confirmed leaf in it.
                    let creator = self.channels.get(channel_id).map(|c| &c.creator);
                    ensure!(
                        creator == Some(author),
                        "a generation's bootstrap commit must be authored by the channel's creator"
                    );
                } else {
                    ensure!(
                        group.leaves_confirmed.contains(&leaf),
                        "commit author does not hold a confirmed leaf"
                    );
                }
                // Chain (spec C3): a commit must chain onto the authenticator
                // the previously accepted commit DECLARED. A liar therefore
                // cannot be built upon — the next honest commit fails here.
                // Exempt only when there is nothing to chain to
                // (`epoch_authenticator` is None exactly at a generation's
                // first commit).
                if let Some(expected) = group.epoch_authenticator {
                    ensure!(
                        *prev_epoch_authenticator == expected,
                        "commit does not chain onto the previous commit's declared authenticator"
                    );
                }
                for add in adds {
                    ensure!(
                        self.is_member(&add.identity) && !self.is_pending(&add.identity),
                        "declared add of a non-member"
                    );
                    ensure!(!self.is_banned(&add.identity), "declared add of a banned identity");
                    ensure!(
                        self.judged_live_devices(&add.identity, event.core.timestamp)
                            .contains(&add.device),
                        "declared add of a device that is not live (unknown, revoked, or cert-expired)"
                    );
                    ensure!(
                        !self.consumed_key_packages.contains_key(&add.key_package),
                        "declared add cites a consumed key package"
                    );
                    ensure!(
                        self.key_packages
                            .get(&(add.identity.clone(), add.device.clone()))
                            .and_then(|m| m.get(&add.key_package))
                            .is_some_and(|&exp| exp > self.log_pos),
                        "declared add does not cite a live key package of exactly that (identity, device)"
                    );
                    let leaf = (add.identity.clone(), add.device.clone());
                    ensure!(
                        !group.leaves_confirmed.contains(&leaf)
                            && !group.leaves_pending.contains(&leaf),
                        "declared add of a leaf that is already present or pending"
                    );
                    // Self-add rule (spec C5/Q12): once an identity holds a
                    // confirmed leaf, only that identity may add its further
                    // devices — a stolen identity key alone cannot obtain read
                    // access while a real device of the victim is alive.
                    if group.leaves_confirmed.iter().any(|(pk, _)| pk == &add.identity) {
                        ensure!(
                            author == &add.identity,
                            "self-add rule: only the identity itself may add its additional devices"
                        );
                    }
                }
                for rem in removes {
                    let leaf = (rem.identity.clone(), rem.device.clone());
                    let confirmed = group.leaves_confirmed.contains(&leaf);
                    ensure!(
                        confirmed || group.leaves_pending.contains(&leaf),
                        "declared remove of an absent leaf"
                    );
                    // Bridge rule: a good-standing member's CONFIRMED leaf may
                    // only be removed by that member itself (self-removal of a
                    // device) — that is the rule that makes an unlogged eviction
                    // structurally impossible (spec C7).
                    //
                    // An UNCONFIRMED (pending-only) leaf is exempt: it is an
                    // unproven Add, never a member of the tree. Gating it by
                    // good standing was a permanent, invisible lockout — a
                    // pending leaf produces no `pending_adds` drift, could not
                    // be re-added ("already present or pending") and could not
                    // be removed, and its owner cannot author the commit that
                    // would fix it (that needs a confirmed leaf). One bogus
                    // Welcome — or a steward crashing between commit and
                    // Welcome (spec C3's ghost-Welcome) — locked the victim out
                    // of the whole generation, recoverable only by an
                    // owner-only reset. Removing it evicts nobody: the device
                    // reappears immediately in `pending_adds`, so the Add is
                    // simply re-driven. Adds are open to any member, so their
                    // inverse must be too, or the first Add wins forever. (The
                    // exemption does NOT let the drop discharge a reset
                    // obligation — see `MlsGroupRecord::reset_incomplete`.)
                    if confirmed {
                        let good_standing = self.leaf_holder_in_good_standing(
                            &rem.identity,
                            &rem.device,
                            event.core.timestamp,
                        );
                        ensure!(
                            !good_standing || author == &rem.identity,
                            "cannot remove a leaf of a member in good standing (except self-removal)"
                        );
                    }
                }
                // Commit-rate rule (spec I3): drift discharge is NEVER blocked;
                // neither is a rekey the freshness ceiling is demanding (spec
                // C4 — that rekey is the opposite of spam, and blocking it
                // sealed channels permanently); otherwise the author's first
                // commit, or an epoch gap of at least the group's
                // `commit_rate_gap()`, is required.
                let gap = group.commit_rate_gap();
                let rate_ok = self.commit_discharges_drift(event)
                    || group.ceiling_demands_rekey()
                    || match group.last_commit_epoch_by_author.get(author) {
                        None => true,
                        Some(&last) => *epoch >= last + gap,
                    };
                ensure!(
                    rate_ok,
                    "commit-rate rule: a non-drift-discharging commit must be its author's first or at least {gap} epochs past their previous one"
                );
                Ok(Authorized::Apply)
            }

            EventPayload::MlsWelcome { channel_id, generation, commit, for_member, for_device, .. } => {
                let group = self
                    .mls_groups
                    .get(channel_id)
                    .context("MlsWelcome cites a channel with no E2ee group")?;
                // Same re-check as `MlsCommit`: a confirmed leaf is not standing
                // authority — a kicked identity keeps its leaf until a
                // Remove-commit lands and must not be able to welcome joiners.
                ensure!(
                    self.is_member(author) && !self.is_pending(author),
                    "only full members may author MLS welcomes"
                );
                if *generation == group.generation {
                    // Normal join flow: only a confirmed-leaf holder can welcome,
                    // the target leaf must be pending, and the cited commit must
                    // be one the fold recorded.
                    ensure!(
                        group
                            .leaves_confirmed
                            .contains(&(author.clone(), event.core.device.clone())),
                        "welcome author does not hold a confirmed leaf"
                    );
                    ensure!(
                        group
                            .leaves_pending
                            .contains(&(for_member.clone(), for_device.clone())),
                        "welcome target leaf is not pending"
                    );
                    ensure!(
                        group.commits_by_epoch.values().any(|c| &c.event_hash == commit),
                        "welcome cites a commit the fold has not recorded"
                    );
                } else if *generation == group.generation + 1 {
                    // Reset staging (resolved ambiguity #6): Welcomes for the
                    // NEXT generation precede the owner's MlsGroupReset (whose
                    // refs are content-addressed) — owner-only, like the reset.
                    ensure!(
                        self.is_owner(author),
                        "next-generation (reset-staging) welcomes are owner-only"
                    );
                } else {
                    bail!("welcome generation is neither the current one nor current + 1");
                }
                Ok(Authorized::Apply)
            }

            EventPayload::MlsLeafConfirmed { channel_id, generation, epoch, tree_hash, store_instance_hash } => {
                let group = self
                    .mls_groups
                    .get(channel_id)
                    .context("MlsLeafConfirmed cites a channel with no E2ee group")?;
                // A leaf may only be promoted to confirmed while its holder is
                // still a full member (a pending leaf of someone kicked between
                // Welcome and confirmation stays pending — it is drift, and the
                // pending-removals gate keeps the channel sealed until a
                // Remove-commit discharges it).
                ensure!(
                    self.is_member(author) && !self.is_pending(author),
                    "only full members may confirm an MLS leaf"
                );
                ensure!(
                    *generation == group.generation,
                    "leaf confirmation generation does not match the group"
                );
                // Authored BY the joining device (spec C3: the joiner itself is
                // the only party that can prove its Welcome worked).
                let leaf = (author.clone(), event.core.device.clone());
                ensure!(
                    group.leaves_pending.contains(&leaf),
                    "leaf confirmation must come from the joining device of a pending leaf"
                );
                self.check_instance_pin(&event.core.device, store_instance_hash)?;
                if let Some(rec) = group.commits_by_epoch.get(epoch) {
                    ensure!(
                        rec.post_tree_hash == *tree_hash,
                        "confirmed tree hash does not match the cited epoch's commit"
                    );
                } else if let (true, Some(expected)) =
                    (group.reset_welcomed.contains(&leaf), group.reset_expected_tree_hash)
                {
                    // Reset generation (resolved ambiguity #7): the reset's own
                    // add-commit is never a log event, so a leaf the reset
                    // STAGED has no `commits_by_epoch` entry to check against.
                    // The anchor is the tree hash the RESETTER declared on
                    // `MlsGroupReset` — it is the new group's creator by
                    // construction, so it is the one party that knows the real
                    // value. (This replaces first-writer-wins seeding by the
                    // first confirmation, under which one malicious welcomed
                    // device could confirm a bogus hash first and every honest
                    // confirmation would then be rejected, wedging the
                    // generation.)
                    //
                    // BOTH conditions are load-bearing. `reset_expected_tree_hash`
                    // is set once per reset and never cleared, so on its own it
                    // would let ANY pending leaf — including an ORDINARY join
                    // many commits later — confirm by citing an epoch with no
                    // recorded commit and quoting the reset's long-public
                    // `post_tree_hash`, instead of its real add-commit's epoch
                    // and that commit's tree hash. That leaf would hold a
                    // confirmed leaf with zero binding to the real tree, falsely
                    // discharging `pending_confirmations` (the silent partition
                    // C7 forbids) and unlocking sealed sends + commit authoring.
                    // Scoping to `reset_welcomed` keeps the anchor on the leaves
                    // the reset actually staged; a staged leaf that is still
                    // unconfirmed survives the commit effect's prune, so a LATE
                    // confirmation still works however many commits land first.
                    ensure!(
                        expected == *tree_hash,
                        "confirmed tree hash does not match the tree hash the reset declared"
                    );
                } else {
                    bail!("leaf confirmation cites an epoch with no recorded commit");
                }
                Ok(Authorized::Apply)
            }

            // `authz_head` is carried OPAQUE by both sealed variants: head
            // attestation is a client-side mechanism (peers compare it against
            // their own folded history), so the fold neither reads nor
            // validates it — see spec "Head attestation".
            EventPayload::MessagePostedE2ee { channel_id, generation, epoch, .. } => {
                self.check_sealed_send(event, *channel_id, *generation, *epoch)?;
                Ok(Authorized::Apply)
            }

            EventPayload::MessageEditedE2ee { channel_id, target, generation, epoch, .. } => {
                self.check_sealed_send(event, *channel_id, *generation, *epoch)?;
                // A tombstoned message cannot be resurrected by an edit. Target
                // AUTHORSHIP is not checked here: it needs a per-message index
                // the fold's state deliberately omits — ingest (sub-3) verifies
                // it against the derived `messages` table.
                ensure!(!self.tombstones.contains(target), "edit target is tombstoned");
                Ok(Authorized::Apply)
            }

            EventPayload::MlsGroupReset { channel_id, new_generation, welcomes, .. } => {
                // Owner-only this rung (spec M3), same as ChannelCreated.
                ensure!(self.is_owner(author), "only the owner may reset an MLS group");
                let group = self
                    .mls_groups
                    .get(channel_id)
                    .context("MlsGroupReset cites a channel with no E2ee group")?;
                ensure!(
                    *new_generation == group.generation + 1,
                    "a reset must advance the generation by exactly one"
                );
                // Rate limit (spec C7). Generation 0 ⇒ no reset has ever
                // occurred here ⇒ the first reset is always allowed. An
                // INCOMPLETE reset is exempt too: while the reset is incomplete
                // the channel accepts no sealed content, so its rate-limit
                // clock cannot advance — without this exemption a single
                // welcomed device that never confirms (lost device, poisoned
                // MLS store — exactly what the hatch exists for) would lock the
                // channel out of the only recovery it has. A reset that never
                // completed is not a spam vector.
                ensure!(
                    group.generation == 0
                        || group.reset_incomplete()
                        || group.channel_events_since_reset >= RESET_MIN_CHANNEL_EVENTS,
                    "reset rate limit: at most one reset per {RESET_MIN_CHANNEL_EVENTS} channel events"
                );
                // Completeness (spec C7): the staged Welcomes must cover EXACTLY
                // the fold's members × live_devices, minus the resetter's own
                // authoring device (which becomes the new group's creator —
                // resolved ambiguity #5). No more, no fewer: a reset plus
                // selective Welcomes would be an unbounded, unlogged eviction.
                let mut welcomed: HashSet<(PublicKey, DeviceId)> = HashSet::new();
                let mut seen: HashSet<&EventRef> = HashSet::new();
                for r in welcomes {
                    ensure!(seen.insert(r), "reset welcomes contain a duplicate reference");
                    let rec = group
                        .welcomes
                        .get(r)
                        .context("reset cites a welcome the fold has not recorded")?;
                    ensure!(
                        rec.generation == *new_generation,
                        "reset cites a welcome staged for a different generation"
                    );
                    welcomed.insert((rec.for_member.clone(), rec.for_device.clone()));
                }
                let mut target = self.member_leaf_set(event.core.timestamp);
                target.remove(&(author.clone(), event.core.device.clone()));
                ensure!(
                    welcomed == target,
                    "non-selective reset: welcomes must cover exactly the fold's members x live devices (minus the resetter's own device)"
                );
                Ok(Authorized::Apply)
            }
        }
    }

    /// The send gates every sealed-content variant shares (spec C4/I1 + class
    /// gating). Read-only. All gates fail CLOSED: an unknown channel, a
    /// plaintext channel, an unconfirmed leaf, a stale epoch, outstanding
    /// pending removals, an exhausted freshness budget, or an incomplete reset
    /// each make the event invalid.
    fn check_sealed_send(
        &self,
        event: &Event,
        channel_id: u64,
        generation: u64,
        epoch: u64,
    ) -> Result<()> {
        let author = &event.core.author;
        // Class gate (the other half of Task 2's): sealed content is valid ONLY
        // in a channel the log knows as E2ee. Unknown channel ⇒ invalid (a
        // legacy channel is permanently plaintext).
        let class = self
            .channel_class(channel_id)
            .context("sealed content cites a channel unknown to the log")?;
        ensure!(
            class == ChannelClass::E2ee,
            "sealed content is invalid in a Plaintext channel"
        );
        let group = self
            .mls_groups
            .get(&channel_id)
            .expect("every E2ee channel is created with an MLS group");
        ensure!(
            self.is_member(author) && !self.is_pending(author),
            "only full members may send sealed content"
        );
        // Drift detection runs on CONFIRMED leaves only: a declared-but-
        // unconfirmed leaf cannot speak (spec C3).
        ensure!(
            group
                .leaves_confirmed
                .contains(&(author.clone(), event.core.device.clone())),
            "sealed content author does not hold a confirmed leaf"
        );
        ensure!(
            generation == group.generation,
            "sealed content generation does not match the group"
        );
        // Only the current epoch at this log position — deterministic on replay.
        ensure!(epoch == group.epoch, "sealed content does not cite the group's current epoch");
        // Pending-removals gate (spec I1): a protocol invariant, not client
        // courtesy — the channel is sealed-until-rekey, enforced blind.
        ensure!(
            self.pending_removals(channel_id, event.core.timestamp).is_empty(),
            "channel is sealed until a rekey discharges its pending removals"
        );
        // Freshness ceiling (spec C4).
        ensure!(
            group.events_since_last_commit < FRESHNESS_CEILING_EVENTS,
            "freshness ceiling reached: the channel is sealed until somebody rekeys"
        );
        // Partial reset (spec C7) = dead channel, loudly. Derived from the
        // staged leaves, so discharging the obligation by REMOVAL reopens the
        // channel exactly as confirmation does — see `reset_incomplete`.
        ensure!(
            !group.reset_incomplete(),
            "group reset is incomplete: the channel is sealed until every welcomed leaf confirms or is removed"
        );
        Ok(())
    }

    /// Apply the state effect of an authorized event. Infallible: authorization
    /// already passed (CHECK-THEN-MUTATE).
    fn apply_payload_effect(&mut self, event: &Event, device_pubkey: &PublicKey) {
        match &event.core.payload {
            EventPayload::DeviceAuthorized { cert } => {
                self.devices.insert(
                    event.core.device.clone(),
                    DeviceRecord {
                        identity: event.core.author.clone(),
                        device_pubkey: device_pubkey.clone(),
                        expires_at: cert.core.expires_at,
                    },
                );
                self.devices_by_identity
                    .entry(event.core.author.clone())
                    .or_default()
                    .insert(event.core.device.clone());
            }
            EventPayload::InviteCreated { max_uses, expires_at, requires_approval, .. } => {
                self.invites.insert(
                    event.hash(),
                    InviteRecord {
                        max_uses: *max_uses,
                        expires_at: *expires_at,
                        use_count: 0,
                        requires_approval: *requires_approval,
                    },
                );
            }
            EventPayload::MemberJoined { member, invite } => {
                let requires_approval =
                    self.invites.get(invite).is_some_and(|i| i.requires_approval);
                if requires_approval {
                    self.pending.insert(member.clone());
                } else {
                    self.members.insert(member.clone());
                }
                if let Some(inv) = self.invites.get_mut(invite) {
                    inv.use_count += 1;
                }
            }
            EventPayload::MemberApproved { member } => {
                self.pending.remove(member);
                self.members.insert(member.clone());
            }
            EventPayload::MessagePosted { channel_id, attachments, .. } => {
                for cap in attachments {
                    self.attachment_uploaders
                        .entry(cap.content_hash.clone())
                        .or_insert_with(|| cap.uploader.clone());
                }
                // A plaintext post into a channel the log does not know is the
                // legacy carve-out: record the id so the channel can never be
                // declared (E2ee or otherwise) over its plaintext history.
                if !self.channels.contains_key(channel_id) {
                    self.plaintext_history_channels.insert(*channel_id);
                }
            }
            EventPayload::MemberRemoved { member } => {
                self.members.remove(member);
                self.pending.remove(member);
                self.capabilities.remove(member);
            }
            EventPayload::MemberBanned { member } => {
                self.banned.insert(member.clone());
                self.members.remove(member);
                self.pending.remove(member);
                self.capabilities.remove(member);
            }
            EventPayload::MemberUnbanned { member } => {
                self.banned.remove(member);
            }
            EventPayload::PermissionGranted { member, capability } => {
                self.capabilities.entry(member.clone()).or_default().insert(capability.clone());
            }
            EventPayload::AttachmentRedacted { content_hash } => {
                self.redacted_attachments.insert(content_hash.clone());
            }

            EventPayload::ChannelCreated { channel_id, name, kind, class, parent } => {
                self.channels.insert(
                    *channel_id,
                    ChannelRecord {
                        name: name.clone(),
                        kind: kind.clone(),
                        class: *class,
                        parent: *parent,
                        creator: event.core.author.clone(),
                    },
                );
                // An E2ee channel is born with its MLS group bookkeeping:
                // generation 0, epoch 0 (the creator authors the first logged
                // commit at epoch 0 — resolved ambiguity #5).
                if *class == ChannelClass::E2ee {
                    self.mls_groups.insert(*channel_id, MlsGroupRecord::new());
                }
            }
            EventPayload::MessageDeleted { channel_id, target, .. } => {
                self.tombstones.insert(target.clone());
                // A tombstone is a channel event for the RESET clock, but it
                // does NOT spend forward-secrecy budget: only sealed content
                // does (spec C4 — the ceiling exists so key material stops
                // covering unbounded plaintext). `MessageDeleted` targets are
                // opaque to the fold, so spending freshness here let ANY member
                // seal an E2ee channel on demand with FRESHNESS_CEILING_EVENTS
                // fabricated tombstones.
                self.bump_reset_counter(*channel_id);
            }
            EventPayload::DeviceRevoked { device } => {
                // The device stays in `devices`/`devices_by_identity` — its
                // history stands; liveness queries filter by this set.
                self.revoked_devices.insert(device.clone());
            }

            EventPayload::MlsKeyPackagePublished { store_instance_hash, expires_at_log_pos, .. } => {
                self.pin_instance(&event.core.device, store_instance_hash);
                let log_pos = self.log_pos;
                let entry = self
                    .key_packages
                    .entry((event.core.author.clone(), event.core.device.clone()))
                    .or_default();
                // Prune expired refs on touch (spec I5) — deterministic: driven
                // only by the log-position clock.
                entry.retain(|_, exp| *exp > log_pos);
                entry.insert(event.hash(), *expires_at_log_pos);
            }

            EventPayload::MlsCommit {
                channel_id,
                epoch,
                adds,
                removes,
                post_epoch_authenticator,
                post_tree_hash,
                store_instance_hash,
                ..
            } => {
                // Only reached when the epoch CAS passed (a stale commit is a
                // no-op that never enters this fn).
                //
                // Reset obligations that are VOID — staged for a holder the fold
                // no longer owes a leaf (banned, kicked, device revoked, cert
                // expired) — are computed against the state as it stands BEFORE
                // this commit's effects, which is the same picture the
                // `DeclaredRemove` authz judged. See the prune below.
                let void_obligations: HashSet<(PublicKey, DeviceId)> = self
                    .mls_groups
                    .get(channel_id)
                    .map(|g| {
                        g.reset_welcomed
                            .iter()
                            .filter(|(pk, dev)| {
                                !self.leaf_holder_in_good_standing(pk, dev, event.core.timestamp)
                            })
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                self.pin_instance(&event.core.device, store_instance_hash);
                // Consume the cited KeyPackages: live → consumed (a consumed
                // ref can never be an Add target again).
                for add in adds {
                    if let Some(m) =
                        self.key_packages.get_mut(&(add.identity.clone(), add.device.clone()))
                    {
                        if let Some(exp) = m.remove(&add.key_package) {
                            self.consumed_key_packages.insert(add.key_package.clone(), exp);
                        }
                    }
                }
                let event_hash = event.hash();
                let group = self
                    .mls_groups
                    .get_mut(channel_id)
                    .expect("authz verified the group exists");
                // A bootstrap commit's author IS the tree by construction. The
                // authz rule that let it through is either "nothing to chain
                // to" or "no confirmed leaf exists yet, so the creator speaks";
                // both are captured here, before any mutation.
                let bootstrap =
                    group.epoch_authenticator.is_none() || group.leaves_confirmed.is_empty();
                group.epoch = *epoch + 1;
                group.commit_head = Some(event_hash.clone());
                group.epoch_authenticator = Some(*post_epoch_authenticator);
                group.tree_hash = Some(*post_tree_hash);
                group.commits_by_epoch.insert(
                    *epoch + 1,
                    CommitRecord { event_hash, post_tree_hash: *post_tree_hash },
                );
                for add in adds {
                    group.leaves_pending.insert((add.identity.clone(), add.device.clone()));
                }
                for rem in removes {
                    let leaf = (rem.identity.clone(), rem.device.clone());
                    group.leaves_confirmed.remove(&leaf);
                    group.leaves_pending.remove(&leaf);
                }
                if bootstrap {
                    // The generation's first commit: its author IS the tree by
                    // construction (resolved ambiguity #5) — leaf confirmed.
                    let leaf = (event.core.author.clone(), event.core.device.clone());
                    group.leaves_pending.remove(&leaf);
                    group.leaves_confirmed.insert(leaf);
                }
                // Keep the reset's outstanding-obligation set honest: an
                // obligation to a holder the fold no longer owes a leaf is VOID
                // (that is the bridge's answer for a welcomed device that is
                // banned or lost before confirming), and dropping it here means
                // a later re-add of that leaf is an ordinary pending join rather
                // than a resurrected reset obligation. Removing a staged leaf
                // whose holder is still in GOOD STANDING discharges nothing —
                // otherwise the first confirmer could evict its co-staged peers
                // and reopen the channel around them. See
                // `MlsGroupRecord::reset_incomplete`.
                group.reset_welcomed.retain(|leaf| !void_obligations.contains(leaf));
                group.events_since_last_commit = 0;
                group
                    .last_commit_epoch_by_author
                    .insert(event.core.author.clone(), *epoch);
            }

            EventPayload::MlsWelcome { channel_id, generation, for_member, for_device, .. } => {
                let hash = event.hash();
                let group = self
                    .mls_groups
                    .get_mut(channel_id)
                    .expect("authz verified the group exists");
                group.welcomes.insert(
                    hash,
                    WelcomeRecord {
                        generation: *generation,
                        for_member: for_member.clone(),
                        for_device: for_device.clone(),
                    },
                );
            }

            EventPayload::MlsLeafConfirmed { channel_id, store_instance_hash, .. } => {
                self.pin_instance(&event.core.device, store_instance_hash);
                let group = self
                    .mls_groups
                    .get_mut(channel_id)
                    .expect("authz verified the group exists");
                let leaf = (event.core.author.clone(), event.core.device.clone());
                // Nothing clears a latch here, and nothing seeds a tree hash:
                // promoting the leaf out of `leaves_pending` IS what completes
                // the reset (via the derived `reset_incomplete` predicate), and
                // the reset generation's expected tree hash was declared by the
                // resetter itself.
                group.leaves_pending.remove(&leaf);
                group.leaves_confirmed.insert(leaf);
            }

            EventPayload::MessagePostedE2ee { channel_id, attachments, .. } => {
                // Same uploader bookkeeping as MessagePosted, so AttachmentRedacted
                // authz works on sealed posts too (caps stay outside the seal).
                for cap in attachments {
                    self.attachment_uploaders
                        .entry(cap.content_hash.clone())
                        .or_insert_with(|| cap.uploader.clone());
                }
                self.bump_channel_counters(*channel_id);
            }

            EventPayload::MessageEditedE2ee { channel_id, .. } => {
                self.bump_channel_counters(*channel_id);
            }

            EventPayload::MlsGroupReset { channel_id, new_generation, welcomes, post_tree_hash } => {
                let creator = (event.core.author.clone(), event.core.device.clone());
                let group = self
                    .mls_groups
                    .get_mut(channel_id)
                    .expect("authz verified the group exists");
                let welcomed: HashSet<(PublicKey, DeviceId)> = welcomes
                    .iter()
                    .filter_map(|r| group.welcomes.get(r))
                    .map(|w| (w.for_member.clone(), w.for_device.clone()))
                    .collect();
                group.generation = *new_generation;
                // The new generation starts at epoch 1: creation plus the single
                // implicit add-commit that produced the staged Welcomes (that
                // commit is never a log event — resolved ambiguity #5).
                group.epoch = 1;
                group.commit_head = None;
                // `epoch_authenticator = None` is the bootstrap marker: the new
                // generation's first logged commit is exempt from the chain and
                // confirmed-leaf checks, exactly like generation 0's.
                group.epoch_authenticator = None;
                group.tree_hash = None;
                group.leaves_confirmed = HashSet::from([creator]);
                group.leaves_pending = welcomed.clone();
                // The reset's outstanding obligations: while any of these is
                // still pending the generation is incomplete and the channel
                // accepts no sealed content (derived, not latched).
                group.reset_welcomed = welcomed;
                group.events_since_last_commit = 0;
                group.channel_events_since_reset = 0;
                group.last_commit_epoch_by_author.clear();
                group.commits_by_epoch.clear();
                // Stale (older-generation) Welcomes can never be cited again.
                group.welcomes.retain(|_, w| w.generation >= *new_generation);
                // The resetter knows the new group's real tree hash (it created
                // it): every post-reset confirmation is validated against this.
                group.reset_expected_tree_hash = Some(*post_tree_hash);
            }
        }
    }

    /// Spend one channel event of an E2ee channel's freshness AND reset budgets
    /// (saturating — a jammed counter stays jammed, which fails closed). Sealed
    /// content only. No-op for channels without an MLS group.
    fn bump_channel_counters(&mut self, channel_id: u64) {
        if let Some(group) = self.mls_groups.get_mut(&channel_id) {
            group.events_since_last_commit = group.events_since_last_commit.saturating_add(1);
        }
        self.bump_reset_counter(channel_id);
    }

    /// Spend one channel event of an E2ee channel's RESET rate-limit budget
    /// only, leaving the forward-secrecy ceiling untouched (tombstones).
    fn bump_reset_counter(&mut self, channel_id: u64) {
        if let Some(group) = self.mls_groups.get_mut(&channel_id) {
            group.channel_events_since_reset = group.channel_events_since_reset.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Keypair;
    use crate::event_log::{device_id, AttachmentCap, DeviceCert, Event as Ev, EventPayload as EP};
    use crate::event_log::{ChannelClass, DeclaredAdd, DeclaredRemove, DeleteReason};

    fn genesis(owner: &Keypair) -> Genesis {
        Genesis {
            version: 1,
            name: "t".to_string(),
            owner: owner.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        }
    }

    /// Build a server with the owner's first device authorized, returning
    /// (state, owner_keypair, owner_device_keypair). The owner's chain is at
    /// seq 0 (the DeviceAuthorized).
    fn bootstrapped(owner: &Keypair, owner_dev: &Keypair) -> (LogState, Ev) {
        let g = genesis(owner);
        let mut st = LogState::from_genesis(&g);
        let cert = DeviceCert::create(owner, &owner_dev.public_key(), 1);
        let da = Ev::next(
            owner_dev,
            owner.public_key(),
            g.server_id(),
            None,
            0,
            1,
            EP::DeviceAuthorized { cert },
        );
        st.apply(&da).expect("owner device authorization should succeed");
        (st, da)
    }

    fn msg(dev: &Keypair, author: &PublicKey, server_id: &str, prev: &Ev, lamport: u64) -> Ev {
        Ev::next(
            dev,
            author.clone(),
            server_id.to_string(),
            Some(prev),
            lamport,
            10,
            EP::MessagePosted { channel_id: 1, content: "hi".to_string(), reply_to: None, attachments: vec![] },
        )
    }

    #[test]
    fn from_genesis_seeds_owner_as_member_with_all_authority() {
        let owner = Keypair::generate();
        let st = LogState::from_genesis(&genesis(&owner));
        let o = owner.public_key();
        assert_eq!(st.owner(), &o);
        assert!(st.is_owner(&o));
        assert!(st.is_member(&o));
        assert!(!st.is_banned(&o));
        // Owner implicitly holds any capability.
        assert!(st.has_capability(&o, "invite"));
        assert!(st.has_capability(&o, "ban"));
        assert!(st.has_capability(&o, "anything"));
        // A stranger holds nothing and is not a member.
        let stranger = Keypair::generate().public_key();
        assert!(!st.is_member(&stranger));
        assert!(!st.has_capability(&stranger, "invite"));
        assert_eq!(st.server_id(), &genesis(&owner).server_id());
    }

    #[test]
    fn device_authorized_bootstraps_and_binds() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (st, da) = bootstrapped(&owner, &owner_dev);
        // The device is now recorded (a follow-on event from it passes device resolution).
        assert!(st.devices.contains_key(&device_id(&owner_dev.public_key())));
        assert_eq!(da.core.seq, 0);
    }

    #[test]
    fn envelope_rejections() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (st0, da) = bootstrapped(&owner, &owner_dev);
        let sid = st0.server_id().clone();

        // Wrong server_id.
        {
            let mut st = st0.clone();
            let bad = msg(&owner_dev, &owner.public_key(), "OTHER", &da, 2);
            assert!(st.apply(&bad).is_err());
        }
        // Unauthorized device (a device never DeviceAuthorized).
        {
            let mut st = st0.clone();
            let ghost = Keypair::generate();
            let e = Ev::next(&ghost, owner.public_key(), sid.clone(), None, 0, 1,
                EP::MessagePosted { channel_id: 1, content: "x".into(), reply_to: None, attachments: vec![] });
            assert!(st.apply(&e).is_err(), "event from an uncertified device must be rejected");
        }
        // Device bound to a different identity (owner_dev is owner's; claim a stranger authored).
        {
            let mut st = st0.clone();
            let stranger = Keypair::generate();
            let e = msg(&owner_dev, &stranger.public_key(), &sid, &da, 2);
            assert!(st.apply(&e).is_err(), "device authorized to another identity must be rejected");
        }
        // Chain gap (seq jumps) and fork (wrong prev).
        {
            let mut st = st0.clone();
            let gap = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&da), 2, 10,
                EP::MessagePosted { channel_id: 1, content: "x".into(), reply_to: None, attachments: vec![] });
            // tamper seq to create a gap (re-sign so the signature is valid but seq is wrong)
            let mut core = gap.core.clone();
            core.seq = 5;
            let forged = Ev::sign(core, &owner_dev);
            assert!(st.apply(&forged).is_err(), "seq gap must be rejected");
        }
    }

    #[test]
    fn banned_author_is_rejected_even_with_valid_signature() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        // Manually mark the owner banned (state-level) to prove the ban gate fires
        // before payload handling. (Real bans arrive via MemberBanned in Task 4.)
        st.banned.insert(owner.public_key());
        let e = msg(&owner_dev, &owner.public_key(), st.server_id(), &da, 2);
        assert!(st.apply(&e).is_err(), "a banned author's event must be rejected");
    }

    fn invite(dev: &Keypair, author: &PublicKey, sid: &str, prev: &Ev, seq_lamport: u64, max_uses: u32, expires_at: u64, requires_approval: bool) -> Ev {
        Ev::next(dev, author.clone(), sid.to_string(), Some(prev), seq_lamport, 10,
            EP::InviteCreated { code_hash: "c".into(), max_uses, expires_at, requires_approval })
    }

    #[test]
    fn join_requires_a_valid_invite_and_blocks_self_join() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();

        // Owner creates an invite (owner holds "invite").
        let inv = invite(&owner_dev, &owner.public_key(), &sid, &da, 1, 5, 9999, false);
        st.apply(&inv).expect("owner can create an invite");

        // A newcomer: authorize a device, then join citing the invite.
        let alice = Keypair::generate();
        let alice_dev = Keypair::generate();
        let acert = DeviceCert::create(&alice, &alice_dev.public_key(), 1);
        let a_da = Ev::next(&alice_dev, alice.public_key(), sid.clone(), None, 0, 2,
            EP::DeviceAuthorized { cert: acert });
        st.apply(&a_da).expect("anyone may register their own device");

        // Self-join WITHOUT a valid invite ref → rejected.
        let bad_join = Ev::next(&alice_dev, alice.public_key(), sid.clone(), Some(&a_da), 1, 3,
            EP::MemberJoined { member: alice.public_key(), invite: "nonexistent".into() });
        assert!(st.clone().apply(&bad_join).is_err(), "join citing a non-existent invite must fail");

        // Valid join.
        let join = Ev::next(&alice_dev, alice.public_key(), sid.clone(), Some(&a_da), 2, 3,
            EP::MemberJoined { member: alice.public_key(), invite: inv.hash() });
        st.apply(&join).expect("valid invite join succeeds");
        assert!(st.is_member(&alice.public_key()));

        // Join where member != author (admit someone else) → rejected.
        let bob = Keypair::generate();
        let steal = Ev::next(&alice_dev, alice.public_key(), sid.clone(), Some(&join), 3, 4,
            EP::MemberJoined { member: bob.public_key(), invite: inv.hash() });
        assert!(st.clone().apply(&steal).is_err(), "MemberJoined must be self-authored");
    }

    #[test]
    fn invite_requires_authority_and_enforces_max_uses() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();

        // A non-member/non-authority cannot create an invite.
        let mallory = Keypair::generate();
        let m_dev = Keypair::generate();
        let mcert = DeviceCert::create(&mallory, &m_dev.public_key(), 1);
        let m_da = Ev::next(&m_dev, mallory.public_key(), sid.clone(), None, 0, 1,
            EP::DeviceAuthorized { cert: mcert });
        st.apply(&m_da).unwrap();
        let bad = invite(&m_dev, &mallory.public_key(), &sid, &m_da, 1, 5, 9999, false);
        assert!(st.clone().apply(&bad).is_err(), "no 'invite' capability → rejected");

        // Owner invite with max_uses = 1: a second join must fail.
        let inv = invite(&owner_dev, &owner.public_key(), &sid, &da, 1, 1, 9999, false);
        st.apply(&inv).unwrap();
        for name in ["a", "b"] {
            let u = Keypair::generate();
            let ud = Keypair::generate();
            let cert = DeviceCert::create(&u, &ud.public_key(), 1);
            let uda = Ev::next(&ud, u.public_key(), sid.clone(), None, 0, 1, EP::DeviceAuthorized { cert });
            st.apply(&uda).unwrap();
            let join = Ev::next(&ud, u.public_key(), sid.clone(), Some(&uda), 1, 2,
                EP::MemberJoined { member: u.public_key(), invite: inv.hash() });
            let r = st.apply(&join);
            if name == "a" { assert!(r.is_ok(), "first use ok"); }
            else { assert!(r.is_err(), "max_uses exceeded must fail"); }
        }
    }

    #[test]
    fn only_members_can_post() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();
        // Owner (a member) can post.
        let m = msg(&owner_dev, &owner.public_key(), &sid, &da, 2);
        st.apply(&m).expect("member can post");
        // A non-member with a registered device cannot post.
        let nm = Keypair::generate();
        let nmd = Keypair::generate();
        let cert = DeviceCert::create(&nm, &nmd.public_key(), 1);
        let nmda = Ev::next(&nmd, nm.public_key(), sid.clone(), None, 0, 1, EP::DeviceAuthorized { cert });
        st.apply(&nmda).unwrap();
        let bad = Ev::next(&nmd, nm.public_key(), sid.clone(), Some(&nmda), 1, 3,
            EP::MessagePosted { channel_id: 1, content: "x".into(), reply_to: None, attachments: vec![] });
        assert!(st.apply(&bad).is_err(), "non-member cannot post");
    }

    /// Make `member` a member with `cap`, returning the updated state and the
    /// member's (keypair, device-keypair, last_event) so callers can continue
    /// their chain. Owner grants directly.
    fn add_member_with_cap(
        st: &mut LogState, owner: &Keypair, owner_dev: &Keypair, owner_prev: &mut Ev,
        cap: Option<&str>,
    ) -> (Keypair, Keypair, Ev) {
        let sid = st.server_id().clone();
        // owner creates an invite, member joins, then (optional) grant.
        let inv = Ev::next(owner_dev, owner.public_key(), sid.clone(), Some(owner_prev),
            owner_prev.core.lamport + 1, 100,
            EP::InviteCreated { code_hash: "c".into(), max_uses: 10, expires_at: 9999, requires_approval: false });
        st.apply(&inv).unwrap();
        *owner_prev = inv.clone();

        let u = Keypair::generate();
        let ud = Keypair::generate();
        let cert = DeviceCert::create(&u, &ud.public_key(), 1);
        let uda = Ev::next(&ud, u.public_key(), sid.clone(), None, 0, 1, EP::DeviceAuthorized { cert });
        st.apply(&uda).unwrap();
        let join = Ev::next(&ud, u.public_key(), sid.clone(), Some(&uda), 1, 2,
            EP::MemberJoined { member: u.public_key(), invite: inv.hash() });
        st.apply(&join).unwrap();
        let last = join; // the member's chain head (their join, seq 1)

        if let Some(c) = cap {
            // Grants ride the OWNER's chain, not the member's, so `last` is unchanged.
            let grant = Ev::next(owner_dev, owner.public_key(), sid.clone(), Some(owner_prev),
                owner_prev.core.lamport + 1, 100,
                EP::PermissionGranted { member: u.public_key(), capability: c.to_string() });
            st.apply(&grant).unwrap();
            *owner_prev = grant;
        }
        (u, ud, last)
    }

    #[test]
    fn ban_requires_authority_supersedes_rejoin_and_unban_restores_joinability() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();
        let mut owner_prev = da;

        // A "ban"-capable mod, and a victim member.
        let (_mod_k, mod_dev, mod_last) = add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, Some("ban"));
        let mod_pk = _mod_k.public_key();
        let (victim_k, victim_dev, victim_last) = add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, None);
        let victim_pk = victim_k.public_key();

        // A non-authority cannot ban.
        let bad = Ev::next(&victim_dev, victim_pk.clone(), sid.clone(), Some(&victim_last),
            victim_last.core.lamport + 1, 50, EP::MemberBanned { member: owner.public_key() });
        assert!(st.clone().apply(&bad).is_err(), "no 'ban' capability → rejected");

        // Cannot ban the owner.
        let ban_owner = Ev::next(&mod_dev, mod_pk.clone(), sid.clone(), Some(&mod_last),
            mod_last.core.lamport + 1, 51, EP::MemberBanned { member: owner.public_key() });
        assert!(st.clone().apply(&ban_owner).is_err(), "owner cannot be banned");

        // Mod bans the victim.
        let ban = Ev::next(&mod_dev, mod_pk.clone(), sid.clone(), Some(&mod_last),
            mod_last.core.lamport + 1, 52, EP::MemberBanned { member: victim_pk.clone() });
        st.apply(&ban).expect("authorized ban succeeds");
        assert!(st.is_banned(&victim_pk));
        assert!(!st.is_member(&victim_pk));

        // The banned victim cannot act (e.g. post) from their device.
        let post = Ev::next(&victim_dev, victim_pk.clone(), sid.clone(), Some(&victim_last),
            victim_last.core.lamport + 1, 53, EP::MessagePosted { channel_id: 1, content: "x".into(), reply_to: None, attachments: vec![] });
        assert!(st.clone().apply(&post).is_err(), "banned author cannot post");

        // The banned victim cannot rejoin (ban supersedes a fresh invite+join).
        let inv2 = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&owner_prev),
            owner_prev.core.lamport + 1, 60, EP::InviteCreated { code_hash: "c2".into(), max_uses: 5, expires_at: 9999, requires_approval: false });
        st.apply(&inv2).unwrap();
        let rejoin = Ev::next(&victim_dev, victim_pk.clone(), sid.clone(), Some(&victim_last),
            victim_last.core.lamport + 1, 61, EP::MemberJoined { member: victim_pk.clone(), invite: inv2.hash() });
        assert!(st.clone().apply(&rejoin).is_err(), "banned identity cannot rejoin");

        // Unban (mod has 'ban') then the victim can rejoin.
        let unban = Ev::next(&mod_dev, mod_pk.clone(), sid.clone(), Some(&ban),
            ban.core.lamport + 1, 62, EP::MemberUnbanned { member: victim_pk.clone() });
        st.apply(&unban).expect("authorized unban succeeds");
        assert!(!st.is_banned(&victim_pk));
        let rejoin2 = Ev::next(&victim_dev, victim_pk.clone(), sid.clone(), Some(&victim_last),
            victim_last.core.lamport + 1, 63, EP::MemberJoined { member: victim_pk.clone(), invite: inv2.hash() });
        st.apply(&rejoin2).expect("unbanned identity can rejoin");
        assert!(st.is_member(&victim_pk));
    }

    #[test]
    fn leave_vs_kick_authority() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();
        let mut owner_prev = da;
        let (alice, alice_dev, alice_last) = add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, None);
        let alice_pk = alice.public_key();

        // Voluntary leave (author == member) ok.
        let leave = Ev::next(&alice_dev, alice_pk.clone(), sid.clone(), Some(&alice_last),
            alice_last.core.lamport + 1, 70, EP::MemberRemoved { member: alice_pk.clone() });
        st.apply(&leave).expect("self-leave succeeds");
        assert!(!st.is_member(&alice_pk));

        // Re-add alice, then a non-'kick' member tries to kick her → rejected.
        let (alice2, _ad2, _al2) = add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, None);
        let (bob, bob_dev, bob_last) = add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, None);
        let kick = Ev::next(&bob_dev, bob.public_key(), sid.clone(), Some(&bob_last),
            bob_last.core.lamport + 1, 80, EP::MemberRemoved { member: alice2.public_key() });
        assert!(st.clone().apply(&kick).is_err(), "kick without 'kick' capability is rejected");

        // Owner (root authority) can kick.
        let owner_kick = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&owner_prev),
            owner_prev.core.lamport + 1, 81, EP::MemberRemoved { member: alice2.public_key() });
        st.apply(&owner_kick).expect("owner can kick");
        assert!(!st.is_member(&alice2.public_key()));
    }

    #[test]
    fn grant_only_what_you_hold() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();
        let mut owner_prev = da;

        // Owner grants "invite" to alice; alice can then grant "invite" onward but
        // NOT "ban" (which she doesn't hold).
        let (alice, alice_dev, alice_last) = add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, Some("invite"));
        assert!(st.has_capability(&alice.public_key(), "invite"));

        let (carol, _cd, _cl) = add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, None);
        // alice grants "invite" to carol — allowed (alice holds it).
        let g_ok = Ev::next(&alice_dev, alice.public_key(), sid.clone(), Some(&alice_last),
            alice_last.core.lamport + 1, 90, EP::PermissionGranted { member: carol.public_key(), capability: "invite".into() });
        st.apply(&g_ok).expect("can grant a capability you hold");
        assert!(st.has_capability(&carol.public_key(), "invite"));

        // alice tries to grant "ban" — rejected (she doesn't hold it).
        let g_bad = Ev::next(&alice_dev, alice.public_key(), sid.clone(), Some(&g_ok),
            g_ok.core.lamport + 1, 91, EP::PermissionGranted { member: carol.public_key(), capability: "ban".into() });
        assert!(st.clone().apply(&g_bad).is_err(), "cannot grant a capability you do not hold");
    }

    #[test]
    fn replay_equals_stepwise_and_composes_from_a_checkpoint() {
        // Build a valid log: owner device, invite, alice device, alice join, alice post.
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let g = genesis(&owner);
        let sid = g.server_id();

        let da = Ev::next(&owner_dev, owner.public_key(), sid.clone(), None, 0, 1,
            EP::DeviceAuthorized { cert: DeviceCert::create(&owner, &owner_dev.public_key(), 1) });
        let inv = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&da), 1, 2,
            EP::InviteCreated { code_hash: "c".into(), max_uses: 5, expires_at: 9999, requires_approval: false });
        let alice = Keypair::generate();
        let alice_dev = Keypair::generate();
        let a_da = Ev::next(&alice_dev, alice.public_key(), sid.clone(), None, 0, 3,
            EP::DeviceAuthorized { cert: DeviceCert::create(&alice, &alice_dev.public_key(), 1) });
        let join = Ev::next(&alice_dev, alice.public_key(), sid.clone(), Some(&a_da), 1, 4,
            EP::MemberJoined { member: alice.public_key(), invite: inv.hash() });
        let post = Ev::next(&alice_dev, alice.public_key(), sid.clone(), Some(&join), 2, 5,
            EP::MessagePosted { channel_id: 1, content: "hi".into(), reply_to: None, attachments: vec![] });
        let log = vec![da, inv, a_da, join, post];

        // replay == stepwise apply.
        let replayed = LogState::replay(&g, &log).expect("valid log replays");
        let mut stepwise = LogState::from_genesis(&g);
        for e in &log { stepwise.apply(e).unwrap(); }
        assert!(replayed.is_member(&alice.public_key()));
        assert_eq!(replayed.is_member(&alice.public_key()), stepwise.is_member(&alice.public_key()));
        assert_eq!(replayed.devices.len(), stepwise.devices.len());

        // Checkpoint composability: applying the TAIL to a CLONE of a mid-log state
        // yields the same membership as replaying the whole thing — proving apply
        // is a pure (state, event) step that composes from any starting state.
        let mut mid = LogState::from_genesis(&g);
        for e in &log[..3] { mid.apply(e).unwrap(); }   // up to alice's DeviceAuthorized
        let mut resumed = mid.clone();
        for e in &log[3..] { resumed.apply(e).unwrap(); } // join + post
        assert_eq!(resumed.is_member(&alice.public_key()), replayed.is_member(&alice.public_key()));

        // A log with an invalid event (post before join) is rejected by replay.
        let owner2 = Keypair::generate();
        let od2 = Keypair::generate();
        let g2 = genesis(&owner2);
        let nm = Keypair::generate();
        let nmd = Keypair::generate();
        let nm_da = Ev::next(&nmd, nm.public_key(), g2.server_id(), None, 0, 1,
            EP::DeviceAuthorized { cert: DeviceCert::create(&nm, &nmd.public_key(), 1) });
        let bad_post = Ev::next(&nmd, nm.public_key(), g2.server_id(), Some(&nm_da), 1, 2,
            EP::MessagePosted { channel_id: 1, content: "x".into(), reply_to: None, attachments: vec![] });
        let _ = (owner2, od2);
        assert!(LogState::replay(&g2, &[nm_da, bad_post]).is_err(), "non-member post must reject the replay");
    }

    #[test]
    fn member_approved_promotes_pending_and_requires_kick() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();

        // Owner creates an approval invite; Alice joins → pending.
        let inv = invite(&owner_dev, &owner.public_key(), &sid, &da, 1, 5, 9999, true);
        st.apply(&inv).unwrap();
        let alice = Keypair::generate();
        let alice_dev = Keypair::generate();
        let acert = DeviceCert::create(&alice, &alice_dev.public_key(), 1);
        let a_da = Ev::next(&alice_dev, alice.public_key(), sid.clone(), None, 0, 2,
            EP::DeviceAuthorized { cert: acert });
        st.apply(&a_da).unwrap();
        let join = Ev::next(&alice_dev, alice.public_key(), sid.clone(), Some(&a_da), 2, 3,
            EP::MemberJoined { member: alice.public_key(), invite: inv.hash() });
        st.apply(&join).unwrap();
        assert!(st.is_pending(&alice.public_key()));

        // A non-"kick" member cannot approve. Make Bob a plain member first.
        let inv2 = invite(&owner_dev, &owner.public_key(), &sid, &inv, 2, 5, 9999, false);
        st.apply(&inv2).unwrap();
        let bob = Keypair::generate();
        let bob_dev = Keypair::generate();
        let bcert = DeviceCert::create(&bob, &bob_dev.public_key(), 1);
        let b_da = Ev::next(&bob_dev, bob.public_key(), sid.clone(), None, 0, 5,
            EP::DeviceAuthorized { cert: bcert });
        st.apply(&b_da).unwrap();
        let bjoin = Ev::next(&bob_dev, bob.public_key(), sid.clone(), Some(&b_da), 2, 6,
            EP::MemberJoined { member: bob.public_key(), invite: inv2.hash() });
        st.apply(&bjoin).unwrap();
        let bob_try = Ev::next(&bob_dev, bob.public_key(), sid.clone(), Some(&bjoin), 3, 7,
            EP::MemberApproved { member: alice.public_key() });
        assert!(st.clone().apply(&bob_try).is_err(), "a member without 'kick' cannot approve");

        // The owner (holds every capability) approves Alice → member, not pending.
        let approve = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&inv2), 3, 8,
            EP::MemberApproved { member: alice.public_key() });
        st.apply(&approve).expect("owner can approve");
        assert!(st.is_member(&alice.public_key()), "approved → member");
        assert!(!st.is_pending(&alice.public_key()), "approved → no longer pending");

        // Approving someone who is not pending is rejected.
        let again = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&approve), 4, 9,
            EP::MemberApproved { member: alice.public_key() });
        assert!(st.clone().apply(&again).is_err(), "cannot approve a non-pending identity");
    }

    #[test]
    fn member_removed_denies_a_pending_request() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();

        // Approval invite; Alice joins → pending.
        let inv = invite(&owner_dev, &owner.public_key(), &sid, &da, 1, 5, 9999, true);
        st.apply(&inv).unwrap();
        let alice = Keypair::generate();
        let alice_dev = Keypair::generate();
        let acert = DeviceCert::create(&alice, &alice_dev.public_key(), 1);
        let a_da = Ev::next(&alice_dev, alice.public_key(), sid.clone(), None, 0, 2,
            EP::DeviceAuthorized { cert: acert });
        st.apply(&a_da).unwrap();
        let join = Ev::next(&alice_dev, alice.public_key(), sid.clone(), Some(&a_da), 2, 3,
            EP::MemberJoined { member: alice.public_key(), invite: inv.hash() });
        st.apply(&join).unwrap();
        assert!(st.is_pending(&alice.public_key()));

        // Owner denies the request via MemberRemoved → no longer pending, not a member.
        let deny = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&inv), 2, 4,
            EP::MemberRemoved { member: alice.public_key() });
        st.apply(&deny).expect("owner ('kick') can remove a pending request");
        assert!(!st.is_pending(&alice.public_key()), "denied → no longer pending");
        assert!(!st.is_member(&alice.public_key()), "denied → not a member");
    }

    #[test]
    fn ban_supersedes_a_pending_join() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();

        let inv = invite(&owner_dev, &owner.public_key(), &sid, &da, 1, 5, 9999, true);
        st.apply(&inv).unwrap();
        let alice = Keypair::generate();
        let alice_dev = Keypair::generate();
        let acert = DeviceCert::create(&alice, &alice_dev.public_key(), 1);
        let a_da = Ev::next(&alice_dev, alice.public_key(), sid.clone(), None, 0, 2,
            EP::DeviceAuthorized { cert: acert });
        st.apply(&a_da).unwrap();
        let join = Ev::next(&alice_dev, alice.public_key(), sid.clone(), Some(&a_da), 2, 3,
            EP::MemberJoined { member: alice.public_key(), invite: inv.hash() });
        st.apply(&join).unwrap();

        // Owner bans the pending identity.
        let ban = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&inv), 2, 4,
            EP::MemberBanned { member: alice.public_key() });
        st.apply(&ban).expect("owner can ban a pending identity");
        assert!(st.is_banned(&alice.public_key()));

        // The banned identity can no longer act (ban gate fires before payload).
        let post = msg(&alice_dev, &alice.public_key(), &sid, &join, 5);
        assert!(st.clone().apply(&post).is_err(), "a banned (formerly pending) identity is blocked");
    }

    #[test]
    fn ban_clears_pending_so_a_banned_identity_cannot_be_approved() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();

        // Owner creates an approval invite; alice authorizes a device and joins → pending.
        let inv = invite(&owner_dev, &owner.public_key(), &sid, &da, 1, 5, 9999, true);
        st.apply(&inv).unwrap();
        let alice = Keypair::generate();
        let alice_dev = Keypair::generate();
        let acert = DeviceCert::create(&alice, &alice_dev.public_key(), 1);
        let a_da = Ev::next(&alice_dev, alice.public_key(), sid.clone(), None, 0, 2,
            EP::DeviceAuthorized { cert: acert });
        st.apply(&a_da).unwrap();
        let join = Ev::next(&alice_dev, alice.public_key(), sid.clone(), Some(&a_da), 2, 3,
            EP::MemberJoined { member: alice.public_key(), invite: inv.hash() });
        st.apply(&join).unwrap();
        assert!(st.is_pending(&alice.public_key()), "alice should be pending before ban");

        // Owner bans the pending alice.
        let ban = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&inv), 2, 4,
            EP::MemberBanned { member: alice.public_key() });
        st.apply(&ban).expect("owner can ban a pending identity");

        // Invariant: ban must have cleared pending.
        assert!(!st.is_pending(&alice.public_key()), "ban must clear pending");
        assert!(st.is_banned(&alice.public_key()), "alice must be banned");

        // A subsequent MemberApproved for alice is rejected: she is no longer pending.
        let approve = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&ban),
            ban.core.seq + 1, ban.core.lamport + 1,
            EP::MemberApproved { member: alice.public_key() });
        assert!(
            st.clone().apply(&approve).is_err(),
            "approving a banned (non-pending) identity must be rejected"
        );
    }

    // ---- AttachmentRedacted tests ----

    #[test]
    fn uploader_can_redact_own_attachment() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();
        // Owner posts a message with an attachment cap (hash "h").
        let post = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&da), 2, 10,
            EP::MessagePosted { channel_id: 1, content: "x".into(), reply_to: None,
                attachments: vec![AttachmentCap { content_hash: "h".into(), declared_type: "image/png".into(), size: 1, uploader: owner.public_key() }] });
        st.apply(&post).unwrap();
        // Owner redacts their own attachment.
        let redact = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&post), 3, 11,
            EP::AttachmentRedacted { content_hash: "h".into() });
        assert!(st.apply(&redact).is_ok());
        assert!(st.is_attachment_redacted("h"));
    }

    #[test]
    fn moderator_can_redact_any_attachment() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();
        let mut owner_prev = da;
        // Owner posts with an attachment cap "h" (uploader = owner).
        let post = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&owner_prev),
            owner_prev.core.lamport + 1, 10,
            EP::MessagePosted { channel_id: 1, content: "x".into(), reply_to: None,
                attachments: vec![AttachmentCap { content_hash: "h".into(), declared_type: "image/png".into(), size: 1, uploader: owner.public_key() }] });
        st.apply(&post).unwrap();
        owner_prev = post;
        // Add a member with "kick" capability.
        let (mod_k, mod_dev, mod_last) =
            add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, Some("kick"));
        // Moderator (not the uploader, but holds "kick") redacts the attachment.
        let redact = Ev::next(&mod_dev, mod_k.public_key(), sid.clone(), Some(&mod_last),
            mod_last.core.lamport + 1, 20,
            EP::AttachmentRedacted { content_hash: "h".into() });
        assert!(st.apply(&redact).is_ok());
        assert!(st.is_attachment_redacted("h"));
    }

    #[test]
    fn non_uploader_non_mod_cannot_redact() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();
        let mut owner_prev = da;
        // Owner posts with attachment cap "h" (uploader = owner).
        let post = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&owner_prev),
            owner_prev.core.lamport + 1, 10,
            EP::MessagePosted { channel_id: 1, content: "x".into(), reply_to: None,
                attachments: vec![AttachmentCap { content_hash: "h".into(), declared_type: "image/png".into(), size: 1, uploader: owner.public_key() }] });
        st.apply(&post).unwrap();
        owner_prev = post;
        // Add a plain member (no "kick").
        let (member_k, member_dev, member_last) =
            add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, None);
        // Plain member (not uploader, no "kick") tries to redact → rejected.
        let redact = Ev::next(&member_dev, member_k.public_key(), sid.clone(), Some(&member_last),
            member_last.core.lamport + 1, 20,
            EP::AttachmentRedacted { content_hash: "h".into() });
        assert!(st.apply(&redact).is_err());
    }

    #[test]
    fn redacting_unknown_hash_is_rejected() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();
        // No MessagePosted citing "never-posted" has been applied.
        let redact = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&da), 2, 10,
            EP::AttachmentRedacted { content_hash: "never-posted".into() });
        assert!(st.apply(&redact).is_err());
    }

    #[test]
    fn double_redact_is_rejected() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();
        let post = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&da), 2, 10,
            EP::MessagePosted { channel_id: 1, content: "x".into(), reply_to: None,
                attachments: vec![AttachmentCap { content_hash: "h".into(), declared_type: "image/png".into(), size: 1, uploader: owner.public_key() }] });
        st.apply(&post).unwrap();
        let r1 = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&post), 3, 11,
            EP::AttachmentRedacted { content_hash: "h".into() });
        st.apply(&r1).unwrap();
        let r2 = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&r1), 4, 12,
            EP::AttachmentRedacted { content_hash: "h".into() });
        assert!(st.apply(&r2).is_err());
    }

    #[test]
    fn approval_invite_lands_joiner_in_pending_not_members() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();

        // Owner creates an APPROVAL-REQUIRED invite.
        let inv = invite(&owner_dev, &owner.public_key(), &sid, &da, 1, 5, 9999, true);
        st.apply(&inv).expect("owner can create an approval invite");

        // Newcomer authorizes a device, then joins citing the invite.
        let alice = Keypair::generate();
        let alice_dev = Keypair::generate();
        let acert = DeviceCert::create(&alice, &alice_dev.public_key(), 1);
        let a_da = Ev::next(&alice_dev, alice.public_key(), sid.clone(), None, 0, 2,
            EP::DeviceAuthorized { cert: acert });
        st.apply(&a_da).expect("device registers");
        let join = Ev::next(&alice_dev, alice.public_key(), sid.clone(), Some(&a_da), 2, 3,
            EP::MemberJoined { member: alice.public_key(), invite: inv.hash() });
        st.apply(&join).expect("join against an approval invite succeeds");

        // Joiner is PENDING, not a member, and cannot post.
        assert!(st.is_pending(&alice.public_key()), "approval join → pending");
        assert!(!st.is_member(&alice.public_key()), "approval join is NOT yet a member");
        assert_eq!(st.pending_members(), vec![alice.public_key()]);
        let post = msg(&alice_dev, &alice.public_key(), &sid, &join, 4);
        assert!(st.clone().apply(&post).is_err(), "a pending member cannot post");

        // An INSTANT invite still makes an immediate member (regression).
        let inv2 = invite(&owner_dev, &owner.public_key(), &sid, &inv, 2, 5, 9999, false);
        st.apply(&inv2).expect("owner can create an instant invite");
        let bob = Keypair::generate();
        let bob_dev = Keypair::generate();
        let bcert = DeviceCert::create(&bob, &bob_dev.public_key(), 1);
        let b_da = Ev::next(&bob_dev, bob.public_key(), sid.clone(), None, 0, 5,
            EP::DeviceAuthorized { cert: bcert });
        st.apply(&b_da).expect("device registers");
        let bjoin = Ev::next(&bob_dev, bob.public_key(), sid.clone(), Some(&b_da), 2, 6,
            EP::MemberJoined { member: bob.public_key(), invite: inv2.hash() });
        st.apply(&bjoin).expect("instant join succeeds");
        assert!(st.is_member(&bob.public_key()), "instant join → member immediately");
        assert!(!st.is_pending(&bob.public_key()));
    }

    // ---- Rung 2, Task 2: channel class gating, tombstones, revocation, expiry, device cap ----

    fn channel(
        dev: &Keypair, author: &PublicKey, sid: &str, prev: &Ev,
        channel_id: u64, class: ChannelClass, parent: Option<u64>,
    ) -> Ev {
        Ev::next(dev, author.clone(), sid.to_string(), Some(prev), prev.core.lamport + 1, 10,
            EP::ChannelCreated {
                channel_id, name: format!("ch{channel_id}"), kind: "text".into(), class, parent,
            })
    }

    fn post_in(dev: &Keypair, author: &PublicKey, sid: &str, prev: &Ev, channel_id: u64) -> Ev {
        Ev::next(dev, author.clone(), sid.to_string(), Some(prev), prev.core.lamport + 1, 10,
            EP::MessagePosted { channel_id, content: "hi".into(), reply_to: None, attachments: vec![] })
    }

    #[test]
    fn channel_created_is_owner_only_and_ids_are_immutable() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();
        let mut owner_prev = da;

        // A plain member cannot create a channel (owner-only this rung, spec M3).
        let (alice, alice_dev, alice_last) =
            add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, None);
        let bad = channel(&alice_dev, &alice.public_key(), &sid, &alice_last, 10, ChannelClass::Plaintext, None);
        assert!(st.clone().apply(&bad).is_err(), "non-owner ChannelCreated must be rejected");

        // Owner creates channel 10 as Plaintext.
        let c10 = channel(&owner_dev, &owner.public_key(), &sid, &owner_prev, 10, ChannelClass::Plaintext, None);
        st.apply(&c10).expect("owner can create a channel");
        assert_eq!(st.channel_class(10), Some(ChannelClass::Plaintext));
        assert_eq!(st.channel_class(99), None, "unknown channel has no class");

        // Duplicate channel_id — even one trying to flip the class — is rejected:
        // class is set once at creation or the channel does not exist to the log.
        let dup = channel(&owner_dev, &owner.public_key(), &sid, &c10, 10, ChannelClass::E2ee, None);
        assert!(st.clone().apply(&dup).is_err(), "duplicate channel_id must be rejected");
        assert_eq!(st.channel_class(10), Some(ChannelClass::Plaintext), "class must be unchanged");
    }

    #[test]
    fn plaintext_post_is_invalid_in_an_e2ee_channel() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();

        let c5 = channel(&owner_dev, &owner.public_key(), &sid, &da, 5, ChannelClass::E2ee, None);
        st.apply(&c5).unwrap();
        assert_eq!(st.channel_class(5), Some(ChannelClass::E2ee));

        // Even a full member (the owner) cannot post plaintext into it —
        // the fail-closed half of class gating.
        let bad = post_in(&owner_dev, &owner.public_key(), &sid, &c5, 5);
        assert!(st.apply(&bad).is_err(), "plaintext post into an E2ee channel must be rejected");
    }

    #[test]
    fn legacy_channels_without_channelcreated_stay_plaintext_writable() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();

        // The exact Rung-1 flow: MessagePosted into a channel the log has never
        // seen a ChannelCreated for — still valid (replay compatibility for
        // every existing post-Rung-1 server).
        let legacy = post_in(&owner_dev, &owner.public_key(), &sid, &da, 1);
        st.apply(&legacy).expect("legacy channel (no ChannelCreated) stays plaintext-writable");

        // And a created Plaintext-class channel accepts plaintext posts.
        let c2 = channel(&owner_dev, &owner.public_key(), &sid, &legacy, 2, ChannelClass::Plaintext, None);
        st.apply(&c2).unwrap();
        let ok = post_in(&owner_dev, &owner.public_key(), &sid, &c2, 2);
        st.apply(&ok).expect("plaintext post into a Plaintext-class channel is valid");
    }

    #[test]
    fn thread_child_inherits_parent_class_or_is_rejected() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();

        let parent = channel(&owner_dev, &owner.public_key(), &sid, &da, 5, ChannelClass::E2ee, None);
        st.apply(&parent).unwrap();

        // Matching class → accepted.
        let child_ok = channel(&owner_dev, &owner.public_key(), &sid, &parent, 6, ChannelClass::E2ee, Some(5));
        st.apply(&child_ok).expect("child inheriting the parent's class is valid");
        assert_eq!(st.channel_class(6), Some(ChannelClass::E2ee));

        // Class mismatch → rejected.
        let child_bad = channel(&owner_dev, &owner.public_key(), &sid, &child_ok, 7, ChannelClass::Plaintext, Some(5));
        assert!(st.clone().apply(&child_bad).is_err(), "class-mismatched thread child must be rejected");

        // Unknown parent → rejected.
        let child_orphan = channel(&owner_dev, &owner.public_key(), &sid, &child_ok, 8, ChannelClass::E2ee, Some(999));
        assert!(st.clone().apply(&child_orphan).is_err(), "unknown thread parent must be rejected");
    }

    #[test]
    fn message_deleted_writes_a_queryable_tombstone() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();
        let mut owner_prev = da;

        let c3 = channel(&owner_dev, &owner.public_key(), &sid, &owner_prev, 3, ChannelClass::Plaintext, None);
        st.apply(&c3).unwrap();
        owner_prev = c3;

        let (mod_k, mod_dev, mod_last) =
            add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, Some("kick"));
        let (bob, bob_dev, bob_last) =
            add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, None);

        // Targets are opaque event refs to the fold (no per-message index by
        // design — ingest verifies Author-claims against the derived view).
        let t1 = "a".repeat(64);
        let t2 = "b".repeat(64);

        // Moderation delete by a "kick"-holder → accepted, tombstone queryable.
        let del1 = Ev::next(&mod_dev, mod_k.public_key(), sid.clone(), Some(&mod_last),
            mod_last.core.lamport + 1, 20,
            EP::MessageDeleted { channel_id: 3, target: t1.clone(), reason: DeleteReason::Moderation });
        assert!(!st.is_tombstoned(&t1));
        st.apply(&del1).expect("'kick'-holder moderation delete succeeds");
        assert!(st.is_tombstoned(&t1), "tombstone must be queryable after the fold");

        // Moderation WITHOUT "kick" → rejected.
        let del_bad = Ev::next(&bob_dev, bob.public_key(), sid.clone(), Some(&bob_last),
            bob_last.core.lamport + 1, 21,
            EP::MessageDeleted { channel_id: 3, target: t2.clone(), reason: DeleteReason::Moderation });
        assert!(st.clone().apply(&del_bad).is_err(), "moderation delete without 'kick' must be rejected");

        // Author delete requires membership only (authorship is ingest's check).
        let del2 = Ev::next(&bob_dev, bob.public_key(), sid.clone(), Some(&bob_last),
            bob_last.core.lamport + 1, 22,
            EP::MessageDeleted { channel_id: 3, target: t2.clone(), reason: DeleteReason::Author });
        st.apply(&del2).expect("author delete by a member folds");
        assert!(st.is_tombstoned(&t2));

        // Duplicate tombstone for the same target → rejected.
        let dup = Ev::next(&mod_dev, mod_k.public_key(), sid.clone(), Some(&del1),
            del1.core.lamport + 1, 23,
            EP::MessageDeleted { channel_id: 3, target: t1.clone(), reason: DeleteReason::Moderation });
        assert!(st.clone().apply(&dup).is_err(), "duplicate tombstone must be rejected");

        // Delete in a channel unknown to the log → rejected (log deletes are
        // for log channels).
        let unk = Ev::next(&mod_dev, mod_k.public_key(), sid.clone(), Some(&del1),
            del1.core.lamport + 1, 24,
            EP::MessageDeleted { channel_id: 999, target: "c".repeat(64), reason: DeleteReason::Moderation });
        assert!(st.clone().apply(&unk).is_err(), "MessageDeleted in an unknown channel must be rejected");
    }

    #[test]
    fn revoked_device_cannot_author_but_history_stands() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();
        let mut owner_prev = da;

        let (alice, alice_dev_a, alice_last) =
            add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, None);
        let (mallory, mallory_dev, mallory_last) =
            add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, None);

        // Alice registers a second device B and posts from it.
        let alice_dev_b = Keypair::generate();
        let b_id = device_id(&alice_dev_b.public_key());
        let b_da = Ev::next(&alice_dev_b, alice.public_key(), sid.clone(), None, 0, 30,
            EP::DeviceAuthorized { cert: DeviceCert::create(&alice, &alice_dev_b.public_key(), 30) });
        st.apply(&b_da).unwrap();
        let b_post = post_in(&alice_dev_b, &alice.public_key(), &sid, &b_da, 1);
        st.apply(&b_post).unwrap();

        // An unrelated identity cannot revoke Alice's device.
        let bad = Ev::next(&mallory_dev, mallory.public_key(), sid.clone(), Some(&mallory_last),
            mallory_last.core.lamport + 1, 31, EP::DeviceRevoked { device: b_id.clone() });
        assert!(st.clone().apply(&bad).is_err(), "unrelated identity cannot revoke");

        // Revoking a device unknown to the log is rejected.
        let unk = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&owner_prev),
            owner_prev.core.lamport + 1, 31, EP::DeviceRevoked { device: "f".repeat(64) });
        assert!(st.clone().apply(&unk).is_err(), "unknown device cannot be revoked");

        // The server owner CAN revoke (abuse hatch) — proven on a clone.
        let owner_revoke = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&owner_prev),
            owner_prev.core.lamport + 1, 32, EP::DeviceRevoked { device: b_id.clone() });
        let mut st2 = st.clone();
        st2.apply(&owner_revoke).expect("server owner can revoke any device");
        assert!(st2.is_device_revoked(&b_id));

        // The owning identity revokes its own device from its OTHER device.
        let revoke = Ev::next(&alice_dev_a, alice.public_key(), sid.clone(), Some(&alice_last),
            alice_last.core.lamport + 1, 33, EP::DeviceRevoked { device: b_id.clone() });
        st.apply(&revoke).expect("owning identity can revoke its device");
        assert!(st.is_device_revoked(&b_id));

        // Double revocation is rejected.
        let again = Ev::next(&alice_dev_a, alice.public_key(), sid.clone(), Some(&revoke),
            revoke.core.lamport + 1, 34, EP::DeviceRevoked { device: b_id.clone() });
        assert!(st.clone().apply(&again).is_err(), "double revocation must be rejected");

        // A new event signed by the revoked device is rejected at the envelope.
        let dead = post_in(&alice_dev_b, &alice.public_key(), &sid, &b_post, 1);
        assert!(st.clone().apply(&dead).is_err(), "revoked device cannot author new events");

        // History stands: state derived from B's earlier events is unchanged.
        assert!(st.is_member(&alice.public_key()));
        assert!(st.devices.contains_key(&b_id), "the device record itself is kept");
        // Liveness excludes the revoked device but keeps device A.
        let live = st.live_devices(&alice.public_key(), 100);
        assert!(!live.contains(&b_id), "revoked device is not live");
        assert!(live.contains(&device_id(&alice_dev_a.public_key())), "sibling device stays live");
    }

    #[test]
    fn expired_cert_cannot_author_events() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, _da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();

        // A second owner device whose cert expires at T = 100.
        let dev2 = Keypair::generate();
        let dev2_id = device_id(&dev2.public_key());
        let da2 = Ev::next(&dev2, owner.public_key(), sid.clone(), None, 0, 50,
            EP::DeviceAuthorized { cert: DeviceCert::create_expiring(&owner, &dev2.public_key(), 50, 100) });
        st.apply(&da2).expect("registering before expiry succeeds");

        // timestamp <= T folds; timestamp > T is rejected (untrusted author
        // clock — the same acceptance Rung 1 made for invite expiry).
        let at_100 = Ev::next(&dev2, owner.public_key(), sid.clone(), Some(&da2), 1, 100,
            EP::MessagePosted { channel_id: 1, content: "x".into(), reply_to: None, attachments: vec![] });
        st.apply(&at_100).expect("event at the expiry boundary folds");
        let at_101 = Ev::next(&dev2, owner.public_key(), sid.clone(), Some(&at_100), 2, 101,
            EP::MessagePosted { channel_id: 1, content: "x".into(), reply_to: None, attachments: vec![] });
        assert!(st.clone().apply(&at_101).is_err(), "expired cert cannot author events");

        // Liveness respects expiry: live at 100, not live at 101.
        assert!(st.live_devices(&owner.public_key(), 100).contains(&dev2_id));
        assert!(!st.live_devices(&owner.public_key(), 101).contains(&dev2_id));

        // Registering with an ALREADY-expired cert is rejected outright.
        let dev3 = Keypair::generate();
        let da3 = Ev::next(&dev3, owner.public_key(), sid.clone(), None, 0, 200,
            EP::DeviceAuthorized { cert: DeviceCert::create_expiring(&owner, &dev3.public_key(), 10, 20) });
        assert!(st.clone().apply(&da3).is_err(), "an expired cert cannot even self-register");
    }

    #[test]
    fn ninth_live_device_of_an_identity_is_rejected() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();

        // The bootstrapped device is #1; authorize 7 more → 8 live devices.
        let mut extra = Vec::new();
        for i in 0..7u64 {
            let d = Keypair::generate();
            let e = Ev::next(&d, owner.public_key(), sid.clone(), None, 0, 40 + i,
                EP::DeviceAuthorized { cert: DeviceCert::create(&owner, &d.public_key(), 40 + i) });
            st.apply(&e).unwrap_or_else(|err| panic!("device {} should fold: {err}", i + 2));
            extra.push(d);
        }
        assert_eq!(st.live_devices(&owner.public_key(), 50).len(), MAX_LIVE_DEVICES_PER_IDENTITY);

        // The 9th live device is rejected.
        let ninth = Keypair::generate();
        let e9 = Ev::next(&ninth, owner.public_key(), sid.clone(), None, 0, 50,
            EP::DeviceAuthorized { cert: DeviceCert::create(&owner, &ninth.public_key(), 50) });
        assert!(st.clone().apply(&e9).is_err(), "ninth live device must be rejected");

        // Revoke one device → a slot frees up (revoked ≠ live) and a new
        // device is accepted again.
        let victim_id = device_id(&extra[0].public_key());
        let revoke = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&da),
            da.core.lamport + 1, 51, EP::DeviceRevoked { device: victim_id });
        st.apply(&revoke).unwrap();
        assert_eq!(st.live_devices(&owner.public_key(), 51).len(), 7);
        st.apply(&e9).expect("after a revocation, an additional device is accepted");
        assert_eq!(st.live_devices(&owner.public_key(), 51).len(), MAX_LIVE_DEVICES_PER_IDENTITY);
    }

    // ---- Rung 2, Task 3: MLS group bookkeeping ----

    /// The E2ee test channel and per-device MLS store-instance hashes.
    const CH: u64 = 5;
    const OWNER_STORE: [u8; 32] = [1u8; 32];
    const ALICE_STORE: [u8; 32] = [2u8; 32];
    /// Declared epoch authenticators / tree hashes used across the tests:
    /// `Xn` = the authenticator the commit creating epoch n+1... (indexing by
    /// the commit sequence: c0 declares post X0, c1 declares post X1, ...).
    const X0: [u8; 32] = [10u8; 32];
    const X1: [u8; 32] = [11u8; 32];
    const X2: [u8; 32] = [12u8; 32];
    const T0: [u8; 32] = [20u8; 32];
    const T1: [u8; 32] = [21u8; 32];
    const T2: [u8; 32] = [22u8; 32];

    fn kp_publish(
        dev: &Keypair, author: &PublicKey, sid: &str, prev: &Ev,
        store: [u8; 32], expires_at_log_pos: u64, ts: u64,
    ) -> Ev {
        Ev::next(dev, author.clone(), sid.to_string(), Some(prev), prev.core.lamport + 1, ts,
            EP::MlsKeyPackagePublished {
                key_package: vec![0xAB], store_instance_hash: store, expires_at_log_pos,
            })
    }

    #[allow(clippy::too_many_arguments)]
    fn mls_commit(
        dev: &Keypair, author: &PublicKey, sid: &str, prev: &Ev, epoch: u64,
        adds: Vec<DeclaredAdd>, removes: Vec<DeclaredRemove>,
        prev_auth: [u8; 32], post_auth: [u8; 32], post_tree: [u8; 32], store: [u8; 32],
    ) -> Ev {
        mls_commit_gen(dev, author, sid, prev, 0, epoch, adds, removes, prev_auth, post_auth,
            post_tree, store)
    }

    #[allow(clippy::too_many_arguments)]
    fn mls_commit_gen(
        dev: &Keypair, author: &PublicKey, sid: &str, prev: &Ev, generation: u64, epoch: u64,
        adds: Vec<DeclaredAdd>, removes: Vec<DeclaredRemove>,
        prev_auth: [u8; 32], post_auth: [u8; 32], post_tree: [u8; 32], store: [u8; 32],
    ) -> Ev {
        Ev::next(dev, author.clone(), sid.to_string(), Some(prev), prev.core.lamport + 1, 500,
            EP::MlsCommit {
                channel_id: CH, generation, epoch, mls_message: vec![0xC0],
                adds, removes,
                prev_epoch_authenticator: prev_auth, post_epoch_authenticator: post_auth,
                post_tree_hash: post_tree, authz_head: "a".repeat(64), store_instance_hash: store,
            })
    }

    fn leaf_confirm(
        dev: &Keypair, author: &PublicKey, sid: &str, prev: &Ev,
        epoch: u64, tree: [u8; 32], store: [u8; 32],
    ) -> Ev {
        leaf_confirm_gen(dev, author, sid, prev, 0, epoch, tree, store)
    }

    #[allow(clippy::too_many_arguments)]
    fn leaf_confirm_gen(
        dev: &Keypair, author: &PublicKey, sid: &str, prev: &Ev,
        generation: u64, epoch: u64, tree: [u8; 32], store: [u8; 32],
    ) -> Ev {
        Ev::next(dev, author.clone(), sid.to_string(), Some(prev), prev.core.lamport + 1, 500,
            EP::MlsLeafConfirmed {
                channel_id: CH, generation, epoch, tree_hash: tree, store_instance_hash: store,
            })
    }

    fn welcome_for(
        dev: &Keypair, author: &PublicKey, sid: &str, prev: &Ev,
        generation: u64, commit: EventRef, for_member: &PublicKey, for_device: &DeviceId,
    ) -> Ev {
        Ev::next(dev, author.clone(), sid.to_string(), Some(prev), prev.core.lamport + 1, 500,
            EP::MlsWelcome {
                channel_id: CH, generation, commit,
                for_member: for_member.clone(), for_device: for_device.clone(), welcome: vec![0xEE],
            })
    }

    fn add_of(identity: &PublicKey, device: &Keypair, kp_ref: &EventRef) -> DeclaredAdd {
        DeclaredAdd {
            identity: identity.clone(),
            device: device_id(&device.public_key()),
            key_package: kp_ref.clone(),
        }
    }

    fn rem_of(identity: &PublicKey, device: &Keypair) -> DeclaredRemove {
        DeclaredRemove { identity: identity.clone(), device: device_id(&device.public_key()) }
    }

    /// Owner + owner device, member alice + device, `ChannelCreated { E2ee }`
    /// (channel CH), and one KeyPackage published per device (which PINS each
    /// device's store-instance hash).
    struct E2eeFix {
        st: LogState,
        sid: String,
        owner: Keypair,
        owner_dev: Keypair,
        owner_prev: Ev,
        alice: Keypair,
        alice_dev: Keypair,
        alice_prev: Ev,
        alice_kp_ref: EventRef,
    }

    fn e2ee_fixture() -> E2eeFix {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();
        let mut owner_prev = da;
        let (alice, alice_dev, alice_last) =
            add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, None);
        let ch = channel(&owner_dev, &owner.public_key(), &sid, &owner_prev, CH, ChannelClass::E2ee, None);
        st.apply(&ch).expect("owner creates the E2ee channel");
        owner_prev = ch;
        let okp = kp_publish(&owner_dev, &owner.public_key(), &sid, &owner_prev, OWNER_STORE, 1_000_000, 500);
        st.apply(&okp).expect("owner key package publishes");
        owner_prev = okp;
        let akp = kp_publish(&alice_dev, &alice.public_key(), &sid, &alice_last, ALICE_STORE, 1_000_000, 500);
        st.apply(&akp).expect("alice key package publishes");
        let alice_kp_ref = akp.hash();
        E2eeFix { st, sid, owner, owner_dev, owner_prev, alice, alice_dev, alice_prev: akp, alice_kp_ref }
    }

    /// The creator's bootstrap commit: epoch 0, declares post-authenticator X0
    /// / tree T0 (nothing to chain onto; the author's leaf becomes confirmed).
    fn apply_bootstrap(f: &mut E2eeFix) {
        let c0 = mls_commit(&f.owner_dev, &f.owner.public_key(), &f.sid, &f.owner_prev,
            0, vec![], vec![], [0u8; 32], X0, T0, OWNER_STORE);
        f.st.apply(&c0).expect("bootstrap commit folds");
        f.owner_prev = c0;
    }

    /// Owner add-commit for alice at epoch 1 (chains X0 → X1, tree T1), then
    /// alice's matching confirmation at epoch 2. Leaves the group at epoch 2
    /// with both leaves confirmed and zero drift.
    fn add_and_confirm_alice(f: &mut E2eeFix) {
        let c1 = mls_commit(&f.owner_dev, &f.owner.public_key(), &f.sid, &f.owner_prev, 1,
            vec![add_of(&f.alice.public_key(), &f.alice_dev, &f.alice_kp_ref)], vec![],
            X0, X1, T1, OWNER_STORE);
        f.st.apply(&c1).expect("add-commit for alice folds");
        f.owner_prev = c1;
        let cf = leaf_confirm(&f.alice_dev, &f.alice.public_key(), &f.sid, &f.alice_prev, 2, T1, ALICE_STORE);
        f.st.apply(&cf).expect("alice's leaf confirmation folds");
        f.alice_prev = cf;
    }

    #[test]
    fn key_package_cap_and_log_position_lifetime_are_enforced() {
        let mut f = e2ee_fixture();
        let alice_pk = f.alice.public_key();
        let owner_pk = f.owner.public_key();

        // Alice already holds 1 live package (fixture); 9 more reach the cap.
        for i in 0..9 {
            let e = kp_publish(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, ALICE_STORE, 1_000_000, 500);
            f.st.apply(&e).unwrap_or_else(|err| panic!("live package {} should fold: {err}", i + 2));
            f.alice_prev = e;
        }
        let over = kp_publish(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, ALICE_STORE, 1_000_000, 500);
        assert!(f.st.clone().apply(&over).is_err(), "11th live key package must be rejected");

        // Bob: publishing a package that is already expired at its log position
        // is rejected outright.
        let (bob, bob_dev, mut bob_prev) =
            add_member_with_cap(&mut f.st, &f.owner, &f.owner_dev, &mut f.owner_prev, None);
        let bob_pk = bob.public_key();
        let dead = kp_publish(&bob_dev, &bob_pk, &f.sid, &bob_prev, [4u8; 32], f.st.log_pos(), 500);
        assert!(f.st.clone().apply(&dead).is_err(), "expires_at_log_pos <= log_pos must be rejected");

        // A short-lived package: live for exactly one more accepted event.
        let short = kp_publish(&bob_dev, &bob_pk, &f.sid, &bob_prev, [4u8; 32], f.st.log_pos() + 2, 500);
        let short_ref = short.hash();
        f.st.apply(&short).expect("a not-yet-expired package folds");
        bob_prev = short;
        // One more accepted event pushes log_pos past the expiry.
        let filler = post_in(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1);
        f.st.apply(&filler).unwrap();
        f.owner_prev = filler;

        // The expired package no longer counts toward the cap: 10 fresh
        // publishes all fold (if it counted, the 10th would exceed the cap).
        let mut fresh_ref = String::new();
        for i in 0..10 {
            let e = kp_publish(&bob_dev, &bob_pk, &f.sid, &bob_prev, [4u8; 32], 1_000_000, 500);
            fresh_ref = e.hash();
            f.st.apply(&e).unwrap_or_else(|err| panic!("fresh package {} should fold: {err}", i + 1));
            bob_prev = e;
        }

        // And the expired ref is invalid as an Add target, while a live one works.
        apply_bootstrap(&mut f);
        let bad_add = DeclaredAdd {
            identity: bob_pk.clone(),
            device: device_id(&bob_dev.public_key()),
            key_package: short_ref,
        };
        let bad = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1,
            vec![bad_add], vec![], X0, X1, T1, OWNER_STORE);
        assert!(f.st.clone().apply(&bad).is_err(), "an expired key package is invalid as an Add target");
        let good = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1,
            vec![add_of(&bob_pk, &bob_dev, &fresh_ref)], vec![], X0, X1, T1, OWNER_STORE);
        f.st.apply(&good).expect("a live key package is a valid Add target");
    }

    #[test]
    fn first_commit_bootstraps_then_epoch_cas_noops_stale_commits() {
        let mut f = e2ee_fixture();
        assert_eq!(f.st.mls_current_epoch(CH), Some((0, 0)), "E2ee creation starts gen 0 / epoch 0");
        apply_bootstrap(&mut f);
        assert_eq!(f.st.mls_current_epoch(CH), Some((0, 1)), "bootstrap commit advances to epoch 1");
        let owner_leaf = (f.owner.public_key(), device_id(&f.owner_dev.public_key()));
        assert!(
            f.st.leaves_confirmed(CH).contains(&owner_leaf),
            "the bootstrap author IS the tree — leaf confirmed without a Welcome"
        );

        // Alice re-declares epoch 0 (lost the CAS): an ACCEPTED no-op.
        let stale = mls_commit(&f.alice_dev, &f.alice.public_key(), &f.sid, &f.alice_prev,
            0, vec![], vec![], [9u8; 32], [9u8; 32], [9u8; 32], ALICE_STORE);
        let group_before = f.st.mls_groups.get(&CH).cloned().unwrap();
        let key_packages_before = f.st.key_packages.clone();
        f.st.apply(&stale).expect("a stale commit is an accepted no-op, not an error");
        assert_eq!(
            f.st.mls_groups.get(&CH).unwrap(), &group_before,
            "a stale commit must change zero MLS state"
        );
        assert_eq!(f.st.key_packages, key_packages_before, "a stale commit consumes nothing");
        assert_eq!(f.st.mls_current_epoch(CH), Some((0, 1)), "epoch unchanged");
        // ...but the author's chain head DID advance (the event is recorded).
        let chain_key = (f.alice.public_key(), device_id(&f.alice_dev.public_key()));
        assert_eq!(
            f.st.chains.get(&chain_key).unwrap().hash, stale.hash(),
            "the stale commit still advances its author's chain head"
        );
    }

    #[test]
    fn commit_chaining_rejects_build_on_a_liar() {
        let mut f = e2ee_fixture();
        apply_bootstrap(&mut f); // declared post_epoch_authenticator = X0
        let owner_pk = f.owner.public_key();
        let alice_add = vec![add_of(&f.alice.public_key(), &f.alice_dev, &f.alice_kp_ref)];

        // A commit whose prev does not equal what the previous commit DECLARED
        // is rejected — a liar cannot be built upon, checked blind.
        let on_liar = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1,
            alice_add.clone(), vec![], [99u8; 32], X1, T1, OWNER_STORE);
        assert!(
            f.st.clone().apply(&on_liar).is_err(),
            "a commit not chaining onto the declared authenticator must be rejected"
        );

        let honest = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1,
            alice_add, vec![], X0, X1, T1, OWNER_STORE);
        f.st.apply(&honest).expect("the chaining commit folds");
        assert_eq!(f.st.mls_current_epoch(CH), Some((0, 2)));
    }

    /// An owner-authored epoch-3 commit (prev X2) with exactly one declared
    /// add, applied to a CLONE — returns whether the fold rejected it.
    fn add_rejected(f: &E2eeFix, add: DeclaredAdd) -> bool {
        let c = mls_commit(&f.owner_dev, &f.owner.public_key(), &f.sid, &f.owner_prev, 3,
            vec![add], vec![], X2, [13u8; 32], [23u8; 32], OWNER_STORE);
        f.st.clone().apply(&c).is_err()
    }

    #[test]
    fn declared_add_requires_a_live_key_package_of_a_member_in_good_standing() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f); // epoch 2; alice's fixture package consumed

        // Alice self-removes her leaf so re-add scenarios are testable.
        let c2 = mls_commit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 2,
            vec![], vec![rem_of(&alice_pk, &f.alice_dev)], X1, X2, T2, ALICE_STORE);
        f.st.apply(&c2).expect("self-removal folds");
        f.alice_prev = c2;
        // Group is at epoch 3, chained on X2.

        // (a) non-member identity.
        let stranger = Keypair::generate().public_key();
        assert!(
            add_rejected(&f, DeclaredAdd {
                identity: stranger, device: "d".repeat(64), key_package: "e".repeat(64),
            }),
            "add of a non-member must be rejected"
        );

        // (b) banned member: bob joins, publishes a package, gets banned.
        let (bob, bob_dev, bob_last) =
            add_member_with_cap(&mut f.st, &f.owner, &f.owner_dev, &mut f.owner_prev, None);
        let bkp = kp_publish(&bob_dev, &bob.public_key(), &f.sid, &bob_last, [4u8; 32], 1_000_000, 500);
        f.st.apply(&bkp).unwrap();
        let bob_ref = bkp.hash();
        let ban = Ev::next(&f.owner_dev, owner_pk.clone(), f.sid.clone(), Some(&f.owner_prev),
            f.owner_prev.core.lamport + 1, 500, EP::MemberBanned { member: bob.public_key() });
        f.st.apply(&ban).unwrap();
        f.owner_prev = ban;
        assert!(
            add_rejected(&f, add_of(&bob.public_key(), &bob_dev, &bob_ref)),
            "add of a banned identity must be rejected"
        );

        // (c) revoked device: carol joins, publishes, her device is revoked.
        let (carol, carol_dev, carol_last) =
            add_member_with_cap(&mut f.st, &f.owner, &f.owner_dev, &mut f.owner_prev, None);
        let ckp = kp_publish(&carol_dev, &carol.public_key(), &f.sid, &carol_last, [5u8; 32], 1_000_000, 500);
        f.st.apply(&ckp).unwrap();
        let carol_ref = ckp.hash();
        let revoke = Ev::next(&f.owner_dev, owner_pk.clone(), f.sid.clone(), Some(&f.owner_prev),
            f.owner_prev.core.lamport + 1, 500,
            EP::DeviceRevoked { device: device_id(&carol_dev.public_key()) });
        f.st.apply(&revoke).unwrap();
        f.owner_prev = revoke;
        assert!(
            add_rejected(&f, add_of(&carol.public_key(), &carol_dev, &carol_ref)),
            "add of a revoked device must be rejected"
        );

        // (d) expired cert: dave's cert expires at t=100; commits claim t=500.
        let inv = invite(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev,
            f.owner_prev.core.lamport + 1, 5, 9999, false);
        f.st.apply(&inv).unwrap();
        f.owner_prev = inv.clone();
        let dave = Keypair::generate();
        let dave_dev = Keypair::generate();
        let d_da = Ev::next(&dave_dev, dave.public_key(), f.sid.clone(), None, 0, 50,
            EP::DeviceAuthorized { cert: DeviceCert::create_expiring(&dave, &dave_dev.public_key(), 1, 100) });
        f.st.apply(&d_da).unwrap();
        let d_join = Ev::next(&dave_dev, dave.public_key(), f.sid.clone(), Some(&d_da), 1, 50,
            EP::MemberJoined { member: dave.public_key(), invite: inv.hash() });
        f.st.apply(&d_join).unwrap();
        let dkp = kp_publish(&dave_dev, &dave.public_key(), &f.sid, &d_join, [6u8; 32], 1_000_000, 60);
        f.st.apply(&dkp).unwrap();
        let dave_ref = dkp.hash();
        assert!(
            add_rejected(&f, add_of(&dave.public_key(), &dave_dev, &dave_ref)),
            "add of a device whose cert is expired at the commit's timestamp must be rejected"
        );

        // (e) consumed ref: alice's fixture package was consumed by her add.
        assert!(
            add_rejected(&f, add_of(&alice_pk, &f.alice_dev, &f.alice_kp_ref)),
            "add citing a consumed key package must be rejected"
        );

        // (f) another device's ref: alice publishes a fresh package from her
        // FIRST device; citing it for her second device is rejected.
        let a2 = Keypair::generate();
        let a2_da = Ev::next(&a2, alice_pk.clone(), f.sid.clone(), None, 0, 500,
            EP::DeviceAuthorized { cert: DeviceCert::create(&f.alice, &a2.public_key(), 500) });
        f.st.apply(&a2_da).unwrap();
        let akp2 = kp_publish(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, ALICE_STORE, 1_000_000, 500);
        f.st.apply(&akp2).unwrap();
        let fresh_alice_ref = akp2.hash();
        f.alice_prev = akp2;
        assert!(
            add_rejected(&f, add_of(&alice_pk, &a2, &fresh_alice_ref)),
            "add citing another device's key package must be rejected"
        );

        // The counterfactual: the same commit shape with a live ref of exactly
        // the declared (identity, device) folds.
        let ok = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 3,
            vec![add_of(&alice_pk, &f.alice_dev, &fresh_alice_ref)], vec![],
            X2, [13u8; 32], [23u8; 32], OWNER_STORE);
        f.st.apply(&ok).expect("a good-standing member with a live package can be added");
    }

    #[test]
    fn remove_of_a_member_in_good_standing_is_rejected_except_self_removal() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f); // epoch 2, alice confirmed, good standing

        // A steward (the owner) removing a good-standing leaf is rejected.
        let steward_rm = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 2,
            vec![], vec![rem_of(&alice_pk, &f.alice_dev)], X1, X2, T2, OWNER_STORE);
        assert!(
            f.st.clone().apply(&steward_rm).is_err(),
            "removing a member in good standing must be rejected"
        );

        // The member removing their OWN device is allowed.
        let self_rm = mls_commit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 2,
            vec![], vec![rem_of(&alice_pk, &f.alice_dev)], X1, X2, T2, ALICE_STORE);
        f.st.apply(&self_rm).expect("self-removal of one's own device folds");
        f.alice_prev = self_rm;
        let alice_leaf = (alice_pk.clone(), device_id(&f.alice_dev.public_key()));
        assert!(!f.st.leaves_confirmed(CH).contains(&alice_leaf));

        // A banned member's leaf CAN be removed by anyone holding a leaf.
        let (bob, bob_dev, bob_last) =
            add_member_with_cap(&mut f.st, &f.owner, &f.owner_dev, &mut f.owner_prev, None);
        let bkp = kp_publish(&bob_dev, &bob.public_key(), &f.sid, &bob_last, [4u8; 32], 1_000_000, 500);
        f.st.apply(&bkp).unwrap();
        let c3 = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 3,
            vec![add_of(&bob.public_key(), &bob_dev, &bkp.hash())], vec![],
            X2, [13u8; 32], [23u8; 32], OWNER_STORE);
        f.st.apply(&c3).expect("bob's add folds");
        f.owner_prev = c3;
        let ban = Ev::next(&f.owner_dev, owner_pk.clone(), f.sid.clone(), Some(&f.owner_prev),
            f.owner_prev.core.lamport + 1, 500, EP::MemberBanned { member: bob.public_key() });
        f.st.apply(&ban).unwrap();
        f.owner_prev = ban;
        let rm_banned = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 4,
            vec![], vec![rem_of(&bob.public_key(), &bob_dev)],
            [13u8; 32], [14u8; 32], [24u8; 32], OWNER_STORE);
        f.st.apply(&rm_banned).expect("removing a banned member's leaf folds");
        let bob_leaf = (bob.public_key(), device_id(&bob_dev.public_key()));
        assert!(f.st.pending_removals(CH, 500).is_empty(), "the ban drift is discharged");
        assert!(!f.st.mls_groups.get(&CH).unwrap().leaves_pending.contains(&bob_leaf));
    }

    #[test]
    fn self_add_rule_blocks_stewards_adding_a_second_device() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f); // alice holds a CONFIRMED leaf

        // Alice's second device registers and publishes its own package.
        let a2 = Keypair::generate();
        let a2_da = Ev::next(&a2, alice_pk.clone(), f.sid.clone(), None, 0, 500,
            EP::DeviceAuthorized { cert: DeviceCert::create(&f.alice, &a2.public_key(), 500) });
        f.st.apply(&a2_da).unwrap();
        let a2_kp = kp_publish(&a2, &alice_pk, &f.sid, &a2_da, [3u8; 32], 1_000_000, 500);
        f.st.apply(&a2_kp).unwrap();
        let a2_ref = a2_kp.hash();

        // A steward (the owner) adding alice's second device is rejected: only
        // the identity itself may extend its own read set (spec C5/Q12).
        let steward_add = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 2,
            vec![add_of(&alice_pk, &a2, &a2_ref)], vec![], X1, X2, T2, OWNER_STORE);
        assert!(
            f.st.clone().apply(&steward_add).is_err(),
            "a steward adding a second device of a leaf-holding identity must be rejected"
        );

        // The same add authored by alice (from her confirmed device) folds.
        let self_add = mls_commit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 2,
            vec![add_of(&alice_pk, &a2, &a2_ref)], vec![], X1, X2, T2, ALICE_STORE);
        f.st.apply(&self_add).expect("the identity itself can add its second device");
        let a2_leaf = (alice_pk.clone(), device_id(&a2.public_key()));
        assert!(f.st.mls_groups.get(&CH).unwrap().leaves_pending.contains(&a2_leaf));
    }

    #[test]
    fn joiner_confirmation_promotes_only_on_matching_tree_hash() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        let alice_did = device_id(&f.alice_dev.public_key());
        let alice_leaf = (alice_pk.clone(), alice_did.clone());

        // Before any commit, alice's device is visible drift (a pending add).
        assert!(f.st.pending_adds(CH, 500).contains(&alice_leaf));
        apply_bootstrap(&mut f);
        let c1 = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1,
            vec![add_of(&alice_pk, &f.alice_dev, &f.alice_kp_ref)], vec![],
            X0, X1, T1, OWNER_STORE);
        f.st.apply(&c1).expect("add-commit folds");
        f.owner_prev = c1.clone();

        // Declared ≠ present: the leaf is pending, never confirmed by the add.
        assert!(!f.st.leaves_confirmed(CH).contains(&alice_leaf));
        // The declared add is absorbed by `leaves_pending`, so it is NOT
        // `pending_adds` drift — the retry obligation lives in
        // `pending_confirmations` instead (spec C3's "gets retried
        // automatically" runs on that set, not on the drift sets).
        assert!(f.st.pending_adds(CH, 500).is_empty(), "pending leaf absorbs the declared add");
        assert!(
            f.st.pending_confirmations(CH).contains(&alice_leaf),
            "...and the unconfirmed leaf is the visible retry obligation instead"
        );

        // A Welcome from a non-confirmed author is rejected; so is one citing
        // an unrecorded commit.
        let self_welcome = welcome_for(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev,
            0, c1.hash(), &alice_pk, &alice_did);
        assert!(f.st.clone().apply(&self_welcome).is_err(), "welcome author must hold a confirmed leaf");
        let bad_ref = welcome_for(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev,
            0, "9".repeat(64), &alice_pk, &alice_did);
        assert!(f.st.clone().apply(&bad_ref).is_err(), "welcome must cite a recorded commit");

        // The owner's Welcome is recorded — but recording a Welcome (even a
        // sealed bogus one) NEVER promotes the leaf: it stays pending.
        let w = welcome_for(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev,
            0, c1.hash(), &alice_pk, &alice_did);
        f.st.apply(&w).expect("owner welcome folds");
        f.owner_prev = w;
        assert_eq!(f.st.mls_groups.get(&CH).unwrap().welcomes.len(), 1);
        assert!(!f.st.leaves_confirmed(CH).contains(&alice_leaf), "a Welcome alone never confirms");

        // Confirmation with the WRONG tree hash is rejected.
        let wrong = leaf_confirm(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 2, [99u8; 32], ALICE_STORE);
        assert!(f.st.clone().apply(&wrong).is_err(), "mismatched tree hash must be rejected");

        // A DIFFERENT device of the same identity cannot confirm the leaf.
        let a2 = Keypair::generate();
        let a2_da = Ev::next(&a2, alice_pk.clone(), f.sid.clone(), None, 0, 500,
            EP::DeviceAuthorized { cert: DeviceCert::create(&f.alice, &a2.public_key(), 500) });
        f.st.apply(&a2_da).unwrap();
        let other_dev = leaf_confirm(&a2, &alice_pk, &f.sid, &a2_da, 2, T1, [3u8; 32]);
        assert!(
            f.st.clone().apply(&other_dev).is_err(),
            "only the joining device itself may confirm its leaf"
        );

        // The joining device with the matching tree hash promotes to confirmed.
        let ok = leaf_confirm(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 2, T1, ALICE_STORE);
        f.st.apply(&ok).expect("matching confirmation folds");
        assert!(f.st.leaves_confirmed(CH).contains(&alice_leaf));
        assert!(f.st.mls_groups.get(&CH).unwrap().leaves_pending.is_empty());
    }

    #[test]
    fn store_instance_hash_is_pinned_per_device() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();

        // The fixture's first publish pinned each device; a different hash on a
        // later publish is the clone/restore poison signal — rejected.
        let repin = kp_publish(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, [9u8; 32], 1_000_000, 500);
        assert!(f.st.clone().apply(&repin).is_err(), "publish with a different instance hash must be rejected");

        apply_bootstrap(&mut f);

        // A commit from a pinned device with a different hash is rejected.
        let bad_commit = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1,
            vec![add_of(&alice_pk, &f.alice_dev, &f.alice_kp_ref)], vec![],
            X0, X1, T1, [9u8; 32]);
        assert!(f.st.clone().apply(&bad_commit).is_err(), "commit with a different instance hash must be rejected");
        let c1 = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1,
            vec![add_of(&alice_pk, &f.alice_dev, &f.alice_kp_ref)], vec![],
            X0, X1, T1, OWNER_STORE);
        f.st.apply(&c1).expect("pinned-hash commit folds");
        f.owner_prev = c1;

        // A confirmation from a pinned device with a different hash is rejected.
        let bad_confirm = leaf_confirm(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 2, T1, [9u8; 32]);
        assert!(f.st.clone().apply(&bad_confirm).is_err(), "confirm with a different instance hash must be rejected");
        let ok = leaf_confirm(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 2, T1, ALICE_STORE);
        f.st.apply(&ok).expect("pinned-hash confirmation folds");
    }

    #[test]
    fn commit_rate_rule_blocks_spam_but_never_drift_discharge() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        apply_bootstrap(&mut f);          // owner's commit at epoch 0
        add_and_confirm_alice(&mut f);    // owner's commit at epoch 1
        assert!(f.st.pending_adds(CH, 500).is_empty());
        assert!(f.st.pending_removals(CH, 500).is_empty());

        // Same author, epochs n and n+1, discharging nothing: spam — rejected
        // (epoch 2 < 1 + COMMIT_RATE_MIN_EPOCH_GAP, and not a first commit).
        let spam = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 2,
            vec![], vec![], X1, X2, T2, OWNER_STORE);
        assert!(
            f.st.clone().apply(&spam).is_err(),
            "a quick non-drift-discharging self-update must be rate-blocked"
        );

        // Ban alice: her confirmed leaf becomes a pending removal.
        let ban = Ev::next(&f.owner_dev, owner_pk.clone(), f.sid.clone(), Some(&f.owner_prev),
            f.owner_prev.core.lamport + 1, 500, EP::MemberBanned { member: alice_pk.clone() });
        f.st.apply(&ban).unwrap();
        f.owner_prev = ban;
        let alice_leaf = (alice_pk.clone(), device_id(&f.alice_dev.public_key()));
        assert!(f.st.pending_removals(CH, 500).contains(&alice_leaf));

        // The SAME quick cadence, now discharging the drift: never blocked.
        let discharge = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 2,
            vec![], vec![rem_of(&alice_pk, &f.alice_dev)], X1, X2, T2, OWNER_STORE);
        f.st.apply(&discharge).expect("a drift-discharging commit is never rate-blocked");
        assert!(f.st.pending_removals(CH, 500).is_empty());
    }

    #[test]
    fn drift_priority_tiebreak_beats_a_premined_commit() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f); // epoch 2, both leaves confirmed

        // Bob: a member with a live package and no leaf — outstanding drift.
        let (bob, bob_dev, bob_last) =
            add_member_with_cap(&mut f.st, &f.owner, &f.owner_dev, &mut f.owner_prev, None);
        let bkp = kp_publish(&bob_dev, &bob.public_key(), &f.sid, &bob_last, [4u8; 32], 1_000_000, 500);
        f.st.apply(&bkp).unwrap();
        let bob_add = add_of(&bob.public_key(), &bob_dev, &bkp.hash());
        assert!(f.st.pending_adds(CH, 500).contains(&(bob.public_key(), device_id(&bob_dev.public_key()))));

        // Candidate A (alice) discharges the drift. Candidate B (owner) is a
        // self-update PRE-MINED to sort first canonically — lamport is
        // self-asserted (the same grindable surface as the event hash), so B
        // claims lamport 1, far below A's.
        let a = mls_commit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 2,
            vec![bob_add.clone()], vec![], X1, X2, T2, ALICE_STORE);
        let premined = Ev::next(&f.owner_dev, owner_pk.clone(), f.sid.clone(), Some(&f.owner_prev), 0, 500,
            EP::MlsCommit {
                channel_id: CH, generation: 0, epoch: 2, mls_message: vec![0xC0],
                adds: vec![], removes: vec![],
                prev_epoch_authenticator: X1, post_epoch_authenticator: X2,
                post_tree_hash: T2, authz_head: "a".repeat(64), store_instance_hash: OWNER_STORE,
            });
        assert!(premined.core.lamport < a.core.lamport, "the pre-mined commit sorts first canonically");
        assert!(f.st.commit_discharges_drift(&a));
        assert!(!f.st.commit_discharges_drift(&premined));

        // Drift priority beats canonical order in BOTH argument orders.
        assert_eq!(f.st.compare_same_epoch_commits(&a, &premined), Ordering::Less);
        assert_eq!(f.st.compare_same_epoch_commits(&premined, &a), Ordering::Greater);

        // Neither discharges → canonical (lamport, author, hash) decides.
        let alice_self = mls_commit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 2,
            vec![], vec![], X1, X2, T2, ALICE_STORE);
        assert_eq!(
            f.st.compare_same_epoch_commits(&premined, &alice_self),
            Ordering::Less,
            "with no drift discharged on either side, the lower lamport wins"
        );
        assert_eq!(f.st.compare_same_epoch_commits(&alice_self, &premined), Ordering::Greater);

        // Both discharge → canonical order again.
        let owner_add = Ev::next(&f.owner_dev, owner_pk.clone(), f.sid.clone(), Some(&f.owner_prev), 0, 500,
            EP::MlsCommit {
                channel_id: CH, generation: 0, epoch: 2, mls_message: vec![0xC0],
                adds: vec![bob_add], removes: vec![],
                prev_epoch_authenticator: X1, post_epoch_authenticator: X2,
                post_tree_hash: T2, authz_head: "a".repeat(64), store_instance_hash: OWNER_STORE,
            });
        assert!(f.st.commit_discharges_drift(&owner_add));
        assert!(owner_add.core.lamport < a.core.lamport);
        assert_eq!(f.st.compare_same_epoch_commits(&owner_add, &a), Ordering::Less);
        assert_eq!(f.st.compare_same_epoch_commits(&a, &owner_add), Ordering::Greater);
    }

    // ---- Rung 2, Task 4: sealed-content send gates + non-selective reset ----

    #[allow(clippy::too_many_arguments)]
    fn sealed_post(
        dev: &Keypair, author: &PublicKey, sid: &str, prev: &Ev,
        channel_id: u64, generation: u64, epoch: u64, attachments: Vec<AttachmentCap>,
    ) -> Ev {
        Ev::next(dev, author.clone(), sid.to_string(), Some(prev), prev.core.lamport + 1, 500,
            EP::MessagePostedE2ee {
                channel_id, generation, epoch, ciphertext: vec![0xF0; 8],
                reply_to: None, attachments, authz_head: "a".repeat(64),
            })
    }

    #[allow(clippy::too_many_arguments)]
    fn sealed_edit(
        dev: &Keypair, author: &PublicKey, sid: &str, prev: &Ev,
        generation: u64, epoch: u64, target: EventRef,
    ) -> Ev {
        Ev::next(dev, author.clone(), sid.to_string(), Some(prev), prev.core.lamport + 1, 500,
            EP::MessageEditedE2ee {
                channel_id: CH, target, generation, epoch,
                ciphertext: vec![0xF1; 8], authz_head: "a".repeat(64),
            })
    }

    fn group_reset(
        dev: &Keypair, author: &PublicKey, sid: &str, prev: &Ev,
        new_generation: u64, welcomes: Vec<EventRef>, post_tree_hash: [u8; 32],
    ) -> Ev {
        Ev::next(dev, author.clone(), sid.to_string(), Some(prev), prev.core.lamport + 1, 500,
            EP::MlsGroupReset { channel_id: CH, new_generation, welcomes, post_tree_hash })
    }

    /// Assert the fold rejects `event` for the EXPECTED reason (a bare
    /// `is_err()` would also pass if some earlier gate fired first).
    fn assert_rejected_for(st: &LogState, event: &Ev, expected: &str) {
        let err = st
            .clone()
            .apply(event)
            .expect_err(&format!("expected rejection containing {expected:?}"));
        assert!(
            err.to_string().contains(expected),
            "expected a rejection containing {expected:?}, got: {err}"
        );
    }

    fn ban_of(f: &mut E2eeFix, member: &PublicKey) {
        let owner_pk = f.owner.public_key();
        let e = Ev::next(&f.owner_dev, owner_pk, f.sid.clone(), Some(&f.owner_prev),
            f.owner_prev.core.lamport + 1, 500, EP::MemberBanned { member: member.clone() });
        f.st.apply(&e).expect("ban folds");
        f.owner_prev = e;
    }

    /// Owner-staged next-generation Welcome (reset staging, resolved ambiguity
    /// #6) — returns its event ref for the reset's `welcomes` list.
    fn stage_welcome(
        f: &mut E2eeFix, generation: u64, for_member: &PublicKey, for_device: &DeviceId,
    ) -> EventRef {
        let owner_pk = f.owner.public_key();
        let w = welcome_for(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, generation,
            "c".repeat(64), for_member, for_device);
        f.st.apply(&w).expect("owner-staged next-generation welcome folds");
        let r = w.hash();
        f.owner_prev = w;
        r
    }

    #[test]
    fn sealed_post_requires_e2ee_class_and_a_confirmed_leaf() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();

        // A Plaintext channel exists alongside the E2ee one.
        let pch = channel(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 6, ChannelClass::Plaintext, None);
        f.st.apply(&pch).unwrap();
        f.owner_prev = pch;
        apply_bootstrap(&mut f); // owner holds a confirmed leaf; group at epoch 1

        let into_plaintext = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 6, 0, 1, vec![]);
        assert_rejected_for(&f.st, &into_plaintext, "invalid in a Plaintext channel");
        let into_unknown = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 99, 0, 1, vec![]);
        assert_rejected_for(&f.st, &into_unknown, "channel unknown to the log");

        // Alice is ADDED but has not confirmed: a pending leaf may not speak.
        let c1 = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1,
            vec![add_of(&alice_pk, &f.alice_dev, &f.alice_kp_ref)], vec![], X0, X1, T1, OWNER_STORE);
        f.st.apply(&c1).expect("add-commit folds");
        f.owner_prev = c1;
        let pending_send = sealed_post(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, CH, 0, 2, vec![]);
        assert_rejected_for(&f.st, &pending_send, "confirmed leaf");

        // Stale epoch / wrong generation are rejected.
        let stale = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 0, 1, vec![]);
        assert_rejected_for(&f.st, &stale, "current epoch");
        let wrong_gen = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 1, 2, vec![]);
        assert_rejected_for(&f.st, &wrong_gen, "generation does not match");

        // A confirmed-leaf member at the current epoch folds — and the effect
        // records attachment uploaders (so AttachmentRedacted authz works on
        // sealed posts) and spends one channel event.
        let cap = AttachmentCap {
            content_hash: "b".repeat(64),
            declared_type: "application/octet-stream".into(),
            size: 42,
            uploader: owner_pk.clone(),
        };
        let ok = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 0, 2, vec![cap.clone()]);
        f.st.apply(&ok).expect("a confirmed-leaf member posts at the current epoch");
        assert_eq!(f.st.attachment_uploader(&cap.content_hash), Some(&owner_pk));
        let g = f.st.mls_groups.get(&CH).unwrap();
        assert_eq!(g.events_since_last_commit, 1);
        assert_eq!(g.channel_events_since_reset, 1);
    }

    #[test]
    fn ban_then_pending_removals_gate_blocks_sealed_sends_until_rekey() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f); // epoch 2, both leaves confirmed, zero drift

        let before = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 0, 2, vec![]);
        f.st.apply(&before).expect("sealed sends work with no outstanding drift");
        f.owner_prev = before;

        // Ban alice: her confirmed leaf becomes a pending removal.
        ban_of(&mut f, &alice_pk);
        let alice_leaf = (alice_pk.clone(), device_id(&f.alice_dev.public_key()));
        assert!(f.st.pending_removals(CH, 500).contains(&alice_leaf));

        // The gate is channel-wide: every member's sealed send is invalid (spec
        // I1 — a protocol invariant, not client courtesy).
        let blocked = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 0, 2, vec![]);
        assert_rejected_for(&f.st, &blocked, "pending removals");
        let by_banned = sealed_post(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, CH, 0, 2, vec![]);
        assert_rejected_for(&f.st, &by_banned, "banned");

        // A Remove-commit discharges the drift; sends resume at the new epoch.
        let rekey = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 2,
            vec![], vec![rem_of(&alice_pk, &f.alice_dev)], X1, X2, T2, OWNER_STORE);
        f.st.apply(&rekey).expect("the drift-discharging remove-commit folds");
        f.owner_prev = rekey;
        assert!(f.st.pending_removals(CH, 500).is_empty());
        let after = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 0, 3, vec![]);
        f.st.apply(&after).expect("sends resume once the rekey discharges the drift");
        assert_eq!(
            f.st.mls_groups.get(&CH).unwrap().events_since_last_commit, 1,
            "the commit zeroed the freshness counter; the resumed post spent one"
        );
    }

    #[test]
    fn freshness_ceiling_seals_the_channel_until_somebody_rekeys() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f); // epoch 2, counter freshly zeroed

        for i in 0..FRESHNESS_CEILING_EVENTS {
            let e = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 0, 2, vec![]);
            f.st.apply(&e).unwrap_or_else(|err| panic!("sealed post {} should fold: {err}", i + 1));
            f.owner_prev = e;
        }
        assert_eq!(
            f.st.mls_groups.get(&CH).unwrap().events_since_last_commit,
            FRESHNESS_CEILING_EVENTS
        );

        // The next channel event is invalid — FS becomes an invariant the blind
        // host enforces (spec C4/I1).
        let over = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 0, 2, vec![]);
        assert_rejected_for(&f.st, &over, "freshness ceiling");
        let over_edit = sealed_edit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 0, 2, "d".repeat(64));
        assert_rejected_for(&f.st, &over_edit, "freshness ceiling");

        // Alice's self-update commit (her first, so never rate-blocked) rekeys.
        let rekey = mls_commit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 2,
            vec![], vec![], X1, X2, T2, ALICE_STORE);
        f.st.apply(&rekey).expect("a self-update rekey folds");
        assert_eq!(f.st.mls_groups.get(&CH).unwrap().events_since_last_commit, 0);
        let resumed = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 0, 3, vec![]);
        f.st.apply(&resumed).expect("sends resume once somebody rekeys");
    }

    #[test]
    fn sealed_edit_shares_every_send_gate_and_respects_tombstones() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f);

        let post = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 0, 2, vec![]);
        f.st.apply(&post).expect("the post to edit folds");
        let target = post.hash();
        f.owner_prev = post;

        // An edit passes exactly where a post passes.
        let ok = sealed_edit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 0, 2, target.clone());
        f.st.apply(&ok).expect("an edit folds where a post folds");
        f.owner_prev = ok;

        // ...and is blocked by every one of the same gates.
        let stale = sealed_edit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 0, 1, target.clone());
        assert_rejected_for(&f.st, &stale, "current epoch");
        let unconfirmed = sealed_edit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 0, 2, target.clone());
        f.st.clone().apply(&unconfirmed).expect("alice holds a confirmed leaf here (control)");
        {
            // Freshness ceiling (jammed directly; the counting path has its own test).
            let mut st = f.st.clone();
            st.mls_groups.get_mut(&CH).unwrap().events_since_last_commit = FRESHNESS_CEILING_EVENTS;
            let e = sealed_edit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 0, 2, target.clone());
            let err = st.apply(&e).expect_err("the freshness ceiling seals edits identically");
            assert!(err.to_string().contains("freshness ceiling"), "{err}");
        }

        // Pending removals seal edits identically, and clear identically.
        ban_of(&mut f, &alice_pk);
        let blocked = sealed_edit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 0, 2, target.clone());
        assert_rejected_for(&f.st, &blocked, "pending removals");
        let rekey = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 2,
            vec![], vec![rem_of(&alice_pk, &f.alice_dev)], X1, X2, T2, OWNER_STORE);
        f.st.apply(&rekey).expect("the remove-commit folds");
        f.owner_prev = rekey;
        let after = sealed_edit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 0, 3, target.clone());
        f.st.apply(&after).expect("edits resume after the rekey");
        f.owner_prev = after;

        // A tombstoned target can never be edited again (deletions cannot be
        // resurrected). The tombstone spends the RESET clock but NOT freshness
        // budget — only sealed content does, so tombstones can never be used to
        // seal a channel (see `tombstone_spam_cannot_seal_an_e2ee_channel`).
        let del = Ev::next(&f.owner_dev, owner_pk.clone(), f.sid.clone(), Some(&f.owner_prev),
            f.owner_prev.core.lamport + 1, 500,
            EP::MessageDeleted { channel_id: CH, target: target.clone(), reason: DeleteReason::Author });
        f.st.apply(&del).expect("the tombstone folds");
        f.owner_prev = del;
        let dead = sealed_edit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 0, 3, target);
        assert_rejected_for(&f.st, &dead, "tombstoned");
        let g = f.st.mls_groups.get(&CH).unwrap();
        assert_eq!(
            g.events_since_last_commit, 1,
            "only the resumed edit spent freshness budget; the tombstone did not"
        );
        assert_eq!(
            g.channel_events_since_reset, 4,
            "the tombstone still counts toward the reset rate-limit clock"
        );
    }

    /// Owner + alice (confirmed leaves) + bob (member, live device, no leaf) —
    /// the member set every reset in these tests must cover exactly.
    fn reset_fixture() -> (E2eeFix, Keypair, Keypair, Ev) {
        let mut f = e2ee_fixture();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f);
        let (bob, bob_dev, bob_last) =
            add_member_with_cap(&mut f.st, &f.owner, &f.owner_dev, &mut f.owner_prev, None);
        (f, bob, bob_dev, bob_last)
    }

    #[test]
    fn reset_must_welcome_exactly_the_folds_member_set() {
        const TR: [u8; 32] = [77u8; 32];
        let (mut f, bob, bob_dev, _bob_last) = reset_fixture();
        let owner_pk = f.owner.public_key();
        let owner_did = device_id(&f.owner_dev.public_key());
        let alice_pk = f.alice.public_key();
        let alice_did = device_id(&f.alice_dev.public_key());
        let bob_pk = bob.public_key();
        let bob_did = device_id(&bob_dev.public_key());

        let w_alice = stage_welcome(&mut f, 1, &alice_pk, &alice_did);

        // Missing bob's device ⇒ rejected (the unbounded unlogged eviction the
        // completeness rule exists to make structurally impossible, spec C7).
        let missing = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, vec![w_alice.clone()], TR);
        assert_rejected_for(&f.st, &missing, "cover exactly");

        let w_bob = stage_welcome(&mut f, 1, &bob_pk, &bob_did);

        // An extra, non-member device ⇒ rejected.
        let stranger = Keypair::generate().public_key();
        let w_stranger = stage_welcome(&mut f, 1, &stranger, &"f".repeat(64));
        let extra = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1,
            vec![w_alice.clone(), w_bob.clone(), w_stranger], TR);
        assert_rejected_for(&f.st, &extra, "cover exactly");

        // Duplicate refs ⇒ rejected; the resetter's own device ⇒ rejected (it
        // is the new generation's creator, never a welcomed leaf).
        let dup = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1,
            vec![w_alice.clone(), w_alice.clone(), w_bob.clone()], TR);
        assert_rejected_for(&f.st, &dup, "duplicate reference");
        let w_owner = stage_welcome(&mut f, 1, &owner_pk, &owner_did);
        let self_too = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1,
            vec![w_alice.clone(), w_bob.clone(), w_owner], TR);
        assert_rejected_for(&f.st, &self_too, "cover exactly");

        // A wrong-generation ref ⇒ rejected (staging is per generation).
        let stale_gen = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 2,
            vec![w_alice.clone(), w_bob.clone()], TR);
        assert_rejected_for(&f.st, &stale_gen, "advance the generation");

        // Exact cover folds.
        let ok = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, vec![w_alice, w_bob], TR);
        f.st.apply(&ok).expect("an exact-cover reset folds");
        assert_eq!(f.st.mls_current_epoch(CH), Some((1, 1)), "the new generation starts at epoch 1");
        let g = f.st.mls_groups.get(&CH).unwrap();
        assert!(g.reset_incomplete(), "every staged leaf is still outstanding");
        assert_eq!(g.leaves_confirmed, HashSet::from([(owner_pk.clone(), owner_did)]));
        assert_eq!(g.leaves_pending, HashSet::from([(alice_pk, alice_did), (bob_pk, bob_did)]));
        assert!(g.epoch_authenticator.is_none(), "the new generation's first commit is a bootstrap");
        assert!(g.commits_by_epoch.is_empty() && g.last_commit_epoch_by_author.is_empty());
        assert_eq!(g.events_since_last_commit, 0);
        assert_eq!(g.channel_events_since_reset, 0);
        assert_eq!(
            g.reset_expected_tree_hash, Some(TR),
            "the resetter declared the new generation's tree hash; confirmations are judged against it"
        );
    }

    #[test]
    fn partial_reset_leaves_the_channel_dead_until_all_leaves_confirm() {
        const TR: [u8; 32] = [77u8; 32];
        let (mut f, bob, bob_dev, bob_last) = reset_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        let alice_did = device_id(&f.alice_dev.public_key());
        let bob_pk = bob.public_key();
        let bob_did = device_id(&bob_dev.public_key());

        let w_alice = stage_welcome(&mut f, 1, &alice_pk, &alice_did);
        let w_bob = stage_welcome(&mut f, 1, &bob_pk, &bob_did);
        let reset = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, vec![w_alice, w_bob], TR);
        f.st.apply(&reset).expect("the exact-cover reset folds");
        f.owner_prev = reset;

        // While a staged leaf is outstanding the channel is DEAD, loudly — not
        // a silent partition (spec C7).
        let blocked = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 1, 1, vec![]);
        assert_rejected_for(&f.st, &blocked, "reset is incomplete");

        // The new generation is NOT up for grabs: its `epoch_authenticator` is
        // cleared (bootstrap marker), but the resetter's leaf is confirmed, so
        // only a confirmed-leaf holder may commit into it.
        let grab = mls_commit_gen(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 1, 1,
            vec![], vec![], [0u8; 32], X1, T1, ALICE_STORE);
        assert_rejected_for(&f.st, &grab, "confirmed leaf");

        // First-writer-wins is gone: a malicious welcomed device that confirms a
        // bogus tree hash FIRST is simply rejected, so it can no longer poison
        // every honest confirmation that follows it.
        let poison = leaf_confirm_gen(&bob_dev, &bob_pk, &f.sid, &bob_last, 1, 1, [78u8; 32], [4u8; 32]);
        assert_rejected_for(&f.st, &poison, "the tree hash the reset declared");

        // Confirmations are judged against the tree hash the RESETTER declared
        // (the reset generation's add-commit is never a log event, ambiguity #7)
        // — not against whichever welcomed device happens to confirm first.
        assert_eq!(f.st.mls_groups.get(&CH).unwrap().reset_expected_tree_hash, Some(TR));
        let ca = leaf_confirm_gen(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 1, 1, TR, ALICE_STORE);
        f.st.apply(&ca).expect("alice's post-reset confirmation folds");
        f.alice_prev = ca;
        assert_rejected_for(&f.st, &blocked, "reset is incomplete");

        // A confirmation on a DIFFERENT tree is rejected: everyone must land on
        // the tree the resetter really built, or the reset never completes.
        let bad = leaf_confirm_gen(&bob_dev, &bob_pk, &f.sid, &bob_last, 1, 1, [78u8; 32], [4u8; 32]);
        assert_rejected_for(&f.st, &bad, "the tree hash the reset declared");

        let cb = leaf_confirm_gen(&bob_dev, &bob_pk, &f.sid, &bob_last, 1, 1, TR, [4u8; 32]);
        f.st.apply(&cb).expect("bob's confirmation on the declared tree folds");
        assert!(!f.st.mls_groups.get(&CH).unwrap().reset_incomplete(), "the reset completes");
        let unlocked = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 1, 1, vec![]);
        f.st.apply(&unlocked).expect("sends unlock once every welcomed leaf confirms");
    }

    /// `reset_expected_tree_hash` is set once by `MlsGroupReset` and NEVER
    /// cleared, so the confirmation path that uses it must stay scoped to the
    /// leaves that reset actually STAGED. Unscoped, the anchor outlived its
    /// generation's reset: for the rest of the channel's life ANY pending leaf —
    /// including an ordinary join many commits later — could confirm by citing
    /// an epoch with no recorded commit and quoting the reset's long-public
    /// `post_tree_hash`, taking a confirmed leaf with ZERO binding to the real
    /// tree (a silent partition, plus sealed-send and commit-authoring rights).
    #[test]
    fn the_reset_tree_hash_anchor_binds_only_the_leaves_the_reset_staged() {
        const TR: [u8; 32] = [77u8; 32];
        let (mut f, bob, bob_dev, bob_last) = reset_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        let alice_did = device_id(&f.alice_dev.public_key());
        let bob_pk = bob.public_key();
        let bob_did = device_id(&bob_dev.public_key());

        let w_alice = stage_welcome(&mut f, 1, &alice_pk, &alice_did);
        let w_bob = stage_welcome(&mut f, 1, &bob_pk, &bob_did);
        let reset = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, vec![w_alice, w_bob], TR);
        f.st.apply(&reset).expect("the exact-cover reset folds");
        f.owner_prev = reset;

        // Alice confirms; bob stays staged and confirms LATE, after ordinary
        // commits have landed in the generation.
        let ca = leaf_confirm_gen(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 1, 1, TR, ALICE_STORE);
        f.st.apply(&ca).expect("alice's post-reset confirmation folds");
        f.alice_prev = ca;

        // Dave joins the generation the ORDINARY way: a real add-commit at
        // epoch 1, which records `commits_by_epoch[2] = T1`.
        let (dave, dave_dev, dave_last) =
            add_member_with_cap(&mut f.st, &f.owner, &f.owner_dev, &mut f.owner_prev, None);
        let dave_pk = dave.public_key();
        let dkp = kp_publish(&dave_dev, &dave_pk, &f.sid, &dave_last, [6u8; 32], 1_000_000, 500);
        f.st.apply(&dkp).expect("dave's key package publishes");
        let add = mls_commit_gen(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, 1,
            vec![add_of(&dave_pk, &dave_dev, &dkp.hash())], vec![], [0u8; 32], X1, T1, OWNER_STORE);
        f.st.apply(&add).expect("dave's add-commit folds");
        f.owner_prev = add;

        // Dave's leaf is bound to HIS add-commit: a wrong hash at its real epoch
        // is rejected...
        let wrong = leaf_confirm_gen(&dave_dev, &dave_pk, &f.sid, &dkp, 1, 2, [66u8; 32], [6u8; 32]);
        assert_rejected_for(&f.st, &wrong, "does not match the cited epoch's commit");
        // ...and so is the reset anchor: dave was never staged by the reset, so
        // he cannot escape the commits table by naming an epoch that has no
        // commit and quoting the reset's public tree hash.
        let anchor_theft = leaf_confirm_gen(&dave_dev, &dave_pk, &f.sid, &dkp, 1, 999, TR, [6u8; 32]);
        assert_rejected_for(&f.st, &anchor_theft, "cites an epoch with no recorded commit");
        assert!(
            f.st.pending_confirmations(CH).contains(&(dave_pk.clone(), device_id(&dave_dev.public_key()))),
            "the rejected confirmations discharged nothing"
        );

        // The legitimate paths both still work: dave against his own commit...
        let dave_ok = leaf_confirm_gen(&dave_dev, &dave_pk, &f.sid, &dkp, 1, 2, T1, [6u8; 32]);
        f.st.apply(&dave_ok).expect("dave confirms against his own add-commit's tree hash");
        // ...and bob LATE against the reset anchor, with no `commits_by_epoch`
        // entry for the epoch his (never-logged) reset add-commit created.
        let bob_late = leaf_confirm_gen(&bob_dev, &bob_pk, &f.sid, &bob_last, 1, 1, TR, [4u8; 32]);
        f.st.apply(&bob_late).expect("a still-staged leaf may confirm late against the reset anchor");
        assert!(!f.st.mls_groups.get(&CH).unwrap().reset_incomplete(), "the reset completes");
        let unlocked = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 1, 2, vec![]);
        f.st.apply(&unlocked).expect("sends unlock once every staged leaf has confirmed");
    }

    #[test]
    fn reset_is_owner_only_and_rate_limited() {
        let mut f = e2ee_fixture();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f);
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        let alice_did = device_id(&f.alice_dev.public_key());

        let w_alice = stage_welcome(&mut f, 1, &alice_pk, &alice_did);

        // Owner-only this rung (spec M3): a confirmed-leaf member cannot reset.
        let by_alice = group_reset(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 1, vec![w_alice.clone()], T1);
        assert_rejected_for(&f.st, &by_alice, "only the owner");

        // A channel's FIRST reset is always allowed.
        let r1 = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, vec![w_alice], T1);
        f.st.apply(&r1).expect("the owner's first reset folds");
        f.owner_prev = r1;

        // An INCOMPLETE reset is exempt from the rate limit: while the reset is
        // incomplete the channel accepts no sealed content, so its
        // rate-limit clock CANNOT advance. Without the exemption, one welcomed
        // device that never confirms (lost device, poisoned MLS store — exactly
        // what the hatch exists for) would lock the channel out of its only
        // recovery. A reset that never completed is not a spam vector.
        assert!(f.st.mls_groups.get(&CH).unwrap().reset_incomplete());
        let w2 = stage_welcome(&mut f, 2, &alice_pk, &alice_did);
        let r2 = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 2, vec![w2], T2);
        f.st.apply(&r2).expect("a stuck reset can always be re-run");
        f.owner_prev = r2;

        // Completing the generation re-arms the limit.
        let ca = leaf_confirm_gen(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 2, 1, T2, ALICE_STORE);
        f.st.apply(&ca).expect("alice's post-reset confirmation folds");
        f.alice_prev = ca;
        assert!(!f.st.mls_groups.get(&CH).unwrap().reset_incomplete(), "the reset completed");

        let w3 = stage_welcome(&mut f, 3, &alice_pk, &alice_did);
        let early = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 3, vec![w3.clone()], T3);
        assert_rejected_for(&f.st, &early, "rate limit");

        // ...and it clears only through REAL channel events (500 sealed posts,
        // a rekey to refresh the freshness budget, 500 more) — a valid event
        // sequence reaches the unlocked state, so nothing here pokes private
        // fold state to get there.
        for i in 0..FRESHNESS_CEILING_EVENTS {
            let e = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 2, 1, vec![]);
            f.st.apply(&e).unwrap_or_else(|err| panic!("post {} should fold: {err}", i + 1));
            f.owner_prev = e;
        }
        let rekey = mls_commit_gen(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 2, 1,
            vec![], vec![], [0u8; 32], X1, T1, OWNER_STORE);
        f.st.apply(&rekey).expect("the generation's first logged commit folds");
        f.owner_prev = rekey;
        for i in 0..FRESHNESS_CEILING_EVENTS {
            let e = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 2, 2, vec![]);
            f.st.apply(&e).unwrap_or_else(|err| panic!("post {} should fold: {err}", i + 501));
            f.owner_prev = e;
        }
        assert_eq!(
            f.st.mls_groups.get(&CH).unwrap().channel_events_since_reset,
            RESET_MIN_CHANNEL_EVENTS
        );
        let r3 = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 3, vec![w3], T3);
        f.st.apply(&r3).expect("the rate limit clears after enough channel events");
        assert_eq!(f.st.mls_current_epoch(CH), Some((3, 1)));
    }

    // ---- Review round 1: control-plane authority + clock hardening ----

    /// The generation-bootstrap commit is the one commit with no confirmed leaf
    /// to check against — so it is CREATOR-only. Anything weaker lets any
    /// log-known device seize a fresh E2ee group (brick it for its real creator
    /// and hold a confirmed leaf in it) for the price of one event.
    #[test]
    fn bootstrap_commit_is_creator_only_and_never_a_strangers() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let owner_did = device_id(&f.owner_dev.public_key());

        // A stranger who never joined registers a device (Rung 1 requires no
        // membership for that) and tries to seize the fresh group at epoch 0.
        let stranger = Keypair::generate();
        let s_dev = Keypair::generate();
        let s_da = Ev::next(&s_dev, stranger.public_key(), f.sid.clone(), None, 0, 500,
            EP::DeviceAuthorized { cert: DeviceCert::create(&stranger, &s_dev.public_key(), 1) });
        f.st.apply(&s_da).expect("device registration needs no membership (Rung-1 behavior)");
        let seize = mls_commit(&s_dev, &stranger.public_key(), &f.sid, &s_da, 0,
            vec![], vec![], [0u8; 32], X0, T0, [7u8; 32]);
        assert_rejected_for(&f.st, &seize, "only full members may author MLS commits");

        // A plain member cannot bootstrap it either.
        let by_alice = mls_commit(&f.alice_dev, &f.alice.public_key(), &f.sid, &f.alice_prev, 0,
            vec![], vec![], [0u8; 32], X0, T0, ALICE_STORE);
        assert_rejected_for(&f.st, &by_alice, "authored by the channel's creator");

        // Only the creator's bootstrap folds — and it leaves exactly one leaf,
        // so no stranger leaf can strand the channel in permanent drift.
        apply_bootstrap(&mut f);
        assert_eq!(
            f.st.leaves_confirmed(CH),
            HashSet::from([(owner_pk, owner_did)]),
            "the creator's leaf is the whole tree after the bootstrap"
        );
        assert!(f.st.pending_removals(CH, 500).is_empty());
    }

    /// A confirmed leaf is not standing authority: it survives a kick until a
    /// Remove-commit lands, and in that window its holder must not be able to
    /// drive the control plane.
    #[test]
    fn kicked_member_cannot_drive_the_mls_control_plane() {
        let mut f = e2ee_fixture();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f); // epoch 2, both leaves confirmed
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        let alice_did = device_id(&f.alice_dev.public_key());

        let kick = Ev::next(&f.owner_dev, owner_pk.clone(), f.sid.clone(), Some(&f.owner_prev),
            f.owner_prev.core.lamport + 1, 500, EP::MemberRemoved { member: alice_pk.clone() });
        f.st.apply(&kick).expect("the kick folds");
        f.owner_prev = kick;
        assert!(!f.st.is_member(&alice_pk));
        assert!(
            f.st.leaves_confirmed(CH).contains(&(alice_pk.clone(), alice_did.clone())),
            "her leaf survives until a Remove-commit lands — that is the drift the gate covers"
        );

        // She can no longer advance the epoch / rewrite the chain variable /
        // zero the freshness budget, welcome anyone, or confirm a leaf.
        let commit = mls_commit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 2,
            vec![], vec![], X1, X2, T2, ALICE_STORE);
        assert_rejected_for(&f.st, &commit, "only full members may author MLS commits");
        let welcome = welcome_for(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 0,
            "c".repeat(64), &alice_pk, &"f".repeat(64));
        assert_rejected_for(&f.st, &welcome, "only full members may author MLS welcomes");
        let confirm = leaf_confirm(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 2, T1, ALICE_STORE);
        assert_rejected_for(&f.st, &confirm, "only full members may confirm an MLS leaf");

        // The members who are left repair the drift.
        let rekey = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 2,
            vec![], vec![rem_of(&alice_pk, &f.alice_dev)], X1, X2, T2, OWNER_STORE);
        f.st.apply(&rekey).expect("the owner's remove-commit folds");
        assert!(f.st.pending_removals(CH, 500).is_empty());
    }

    /// `event.core.timestamp` is author-chosen, so device liveness is judged at
    /// a MONOTONE per-identity floor. Neither half of the old bypass survives:
    /// the live-device cap cannot be pumped, and expiry drift cannot be hidden
    /// by back-dating a send.
    #[test]
    fn a_chosen_timestamp_cannot_pump_the_device_cap_or_hide_expiry_drift() {
        // (a) Device cap.
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, _da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();
        let mallory = Keypair::generate();
        let mal_pk = mallory.public_key();
        for i in 0..MAX_LIVE_DEVICES_PER_IDENTITY {
            let d = Keypair::generate();
            let e = Ev::next(&d, mal_pk.clone(), sid.clone(), None, 0, 100,
                EP::DeviceAuthorized {
                    cert: DeviceCert::create_expiring(&mallory, &d.public_key(), 1, 100),
                });
            st.apply(&e).unwrap_or_else(|err| panic!("device {} should fold: {err}", i + 1));
        }
        assert_eq!(st.live_devices(&mal_pk, 100).len(), MAX_LIVE_DEVICES_PER_IDENTITY);
        let ninth = Keypair::generate();
        let at_100 = Ev::next(&ninth, mal_pk.clone(), sid.clone(), None, 0, 100,
            EP::DeviceAuthorized {
                cert: DeviceCert::create_expiring(&mallory, &ninth.public_key(), 1, 100),
            });
        assert!(st.clone().apply(&at_100).is_err(), "the 9th live device is rejected");

        // Claiming t=200 (where the first eight certs are dead) does buy a slot
        // — but the claim is MONOTONE: mallory can never be judged at an
        // earlier moment again, so the cap holds at every timestamp.
        let future = Ev::next(&ninth, mal_pk.clone(), sid.clone(), None, 0, 200,
            EP::DeviceAuthorized {
                cert: DeviceCert::create_expiring(&mallory, &ninth.public_key(), 150, 300),
            });
        st.apply(&future).expect("registering after the old certs died is legitimate");
        assert_eq!(
            st.live_devices(&mal_pk, 100).len(), 1,
            "back-dating cannot resurrect the expired eight (the cap is a hard blast-radius bound)"
        );
        assert!(st.live_devices(&mal_pk, 0).len() <= MAX_LIVE_DEVICES_PER_IDENTITY);

        // (b) Expiry drift in an E2ee channel cannot be hidden by back-dating.
        const B_STORE: [u8; 32] = [8u8; 32];
        let mut f = e2ee_fixture();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f); // epoch 2
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();

        // Alice adds a SECOND device whose cert expires at 600 (self-add rule:
        // only she may), and it confirms into the group.
        let b_dev = Keypair::generate();
        let b_did = device_id(&b_dev.public_key());
        let b_da = Ev::next(&b_dev, alice_pk.clone(), f.sid.clone(), None, 0, 500,
            EP::DeviceAuthorized {
                cert: DeviceCert::create_expiring(&f.alice, &b_dev.public_key(), 100, 600),
            });
        f.st.apply(&b_da).expect("alice's second device registers");
        let b_kp = kp_publish(&b_dev, &alice_pk, &f.sid, &b_da, B_STORE, 1_000_000, 500);
        f.st.apply(&b_kp).expect("the second device publishes a key package");
        let add_b = mls_commit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 2,
            vec![DeclaredAdd { identity: alice_pk.clone(), device: b_did.clone(), key_package: b_kp.hash() }],
            vec![], X1, X2, T2, ALICE_STORE);
        f.st.apply(&add_b).expect("alice's self-add commit folds");
        f.alice_prev = add_b;
        let cb = leaf_confirm(&b_dev, &alice_pk, &f.sid, &b_kp, 3, T2, B_STORE);
        f.st.apply(&cb).expect("the second device confirms");
        assert!(f.st.pending_removals(CH, 500).is_empty(), "no drift while the cert is live");

        // Alice authors anything at t=700 — past that cert's expiry. Her own
        // clock floor moves, so device B is dead from now on at EVERY claimed
        // timestamp, and the drift it creates seals the channel.
        let later = Ev::next(&f.alice_dev, alice_pk.clone(), f.sid.clone(), Some(&f.alice_prev),
            f.alice_prev.core.lamport + 1, 700,
            EP::MessagePosted { channel_id: 1, content: "hi".into(), reply_to: None, attachments: vec![] });
        f.st.apply(&later).expect("alice posts at t=700 from her live device");
        assert!(
            f.st.pending_removals(CH, 500).contains(&(alice_pk.clone(), b_did)),
            "an expired leaf is drift even when the query claims an earlier moment"
        );
        // `sealed_post` claims t=500 — before the expiry — and is STILL refused.
        let backdated = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 0, 3, vec![]);
        assert_rejected_for(&f.st, &backdated, "pending removals");
    }

    // ---- Review round 2 ----

    /// Extra declared authenticators / tree hashes for the round-2 tests, which
    /// run the group past epoch 3.
    const X3: [u8; 32] = [15u8; 32];
    const X4: [u8; 32] = [16u8; 32];
    const T3: [u8; 32] = [25u8; 32];
    const T4: [u8; 32] = [26u8; 32];
    const B_STORE_R2: [u8; 32] = [8u8; 32];

    /// An `MlsCommit` with an explicit `core.timestamp` — the shared helper
    /// always claims 500, and the claimed moment is exactly the knob the
    /// forward-dating attack turns.
    #[allow(clippy::too_many_arguments)]
    fn mls_commit_at(
        dev: &Keypair, author: &PublicKey, sid: &str, prev: &Ev, epoch: u64,
        adds: Vec<DeclaredAdd>, removes: Vec<DeclaredRemove>,
        prev_auth: [u8; 32], post_auth: [u8; 32], post_tree: [u8; 32], store: [u8; 32], ts: u64,
    ) -> Ev {
        Ev::next(dev, author.clone(), sid.to_string(), Some(prev), prev.core.lamport + 1, ts,
            EP::MlsCommit {
                channel_id: CH, generation: 0, epoch, mls_message: vec![0xC0], adds, removes,
                prev_epoch_authenticator: prev_auth, post_epoch_authenticator: post_auth,
                post_tree_hash: post_tree, authz_head: "a".repeat(64), store_instance_hash: store,
            })
    }

    /// A plaintext post at an explicit timestamp (channel 1 is unknown to the
    /// log — the legacy carve-out — so it needs no channel setup).
    fn post_at(dev: &Keypair, author: &PublicKey, sid: &str, prev: &Ev, ts: u64) -> Ev {
        Ev::next(dev, author.clone(), sid.to_string(), Some(prev), prev.core.lamport + 1, ts,
            EP::MessagePosted { channel_id: 1, content: "hi".into(), reply_to: None, attachments: vec![] })
    }

    /// Alice registers, self-adds and confirms a SECOND device whose cert
    /// expires at t=600. Requires the fixture at epoch 2 with alice confirmed;
    /// leaves the group at epoch 3 (chain head X2, tree T2). Returns the
    /// device, its id, and its chain head.
    fn alice_adds_an_expiring_second_device(f: &mut E2eeFix) -> (Keypair, DeviceId, Ev) {
        let alice_pk = f.alice.public_key();
        let b_dev = Keypair::generate();
        let b_did = device_id(&b_dev.public_key());
        let b_da = Ev::next(&b_dev, alice_pk.clone(), f.sid.clone(), None, 0, 500,
            EP::DeviceAuthorized {
                cert: DeviceCert::create_expiring(&f.alice, &b_dev.public_key(), 100, 600),
            });
        f.st.apply(&b_da).expect("alice's second device registers");
        let b_kp = kp_publish(&b_dev, &alice_pk, &f.sid, &b_da, B_STORE_R2, 1_000_000, 500);
        f.st.apply(&b_kp).expect("the second device publishes a key package");
        let add_b = mls_commit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 2,
            vec![DeclaredAdd {
                identity: alice_pk.clone(), device: b_did.clone(), key_package: b_kp.hash(),
            }],
            vec![], X1, X2, T2, ALICE_STORE);
        f.st.apply(&add_b).expect("alice's self-add commit folds");
        f.alice_prev = add_b;
        let cb = leaf_confirm(&b_dev, &alice_pk, &f.sid, &b_kp, 3, T2, B_STORE_R2);
        f.st.apply(&cb).expect("the second device confirms");
        (b_dev, b_did, cb)
    }

    /// Spec C7's non-selective-removal rule is what makes an unlogged eviction
    /// structurally impossible — and a FORWARD-dated `core.timestamp` walked
    /// straight through it: claim a far-future moment and every OTHER member's
    /// expiring cert is judged dead, so `good_standing` collapses and the
    /// Remove is authorized (and counts as a drift discharge, and shrinks the
    /// reset's exact-cover set). The per-identity floor from round 1 only
    /// stopped BACK-dating. Liveness of an identity the author does not own is
    /// now capped by the log's CORROBORATED clock, which one identity cannot
    /// move — not even by forward-dating a harmless event first.
    #[test]
    fn a_forward_dated_commit_cannot_evict_a_member_in_good_standing() {
        let mut f = e2ee_fixture();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f); // epoch 2
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        let alice_did = device_id(&f.alice_dev.public_key());
        let (_b_dev, b_did, _b_prev) = alice_adds_an_expiring_second_device(&mut f);
        let b_leaf = (alice_pk.clone(), b_did.clone());
        assert!(f.st.leaves_confirmed(CH).contains(&b_leaf), "the victim's leaf is in the group");

        let evict = |prev: &Ev, ts: u64| {
            mls_commit_at(&f.owner_dev, &owner_pk, &f.sid, prev, 3, vec![],
                vec![DeclaredRemove { identity: alice_pk.clone(), device: b_did.clone() }],
                X2, X3, T3, OWNER_STORE, ts)
        };

        // The honest claim is refused (the cert is live at t=500)...
        assert_rejected_for(&f.st, &evict(&f.owner_prev, 500), "good standing");
        // ...and so is the far-future claim, which used to be ACCEPTED.
        let attack = evict(&f.owner_prev, 9_999_999);
        assert_rejected_for(&f.st, &attack, "good standing");
        assert!(
            !f.st.commit_discharges_drift(&attack),
            "a forward-dated eviction must not count as a drift discharge either (that bypassed the commit-rate rule)"
        );

        // Poisoning the log's clock first does not help: the corroborated clock
        // is the timestamp TWO distinct identities have claimed, so a lone
        // author cannot move the ceiling they are judged against.
        let poison = post_at(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 9_999_999);
        f.st.apply(&poison).expect("nothing stops an author claiming a wild timestamp");
        f.owner_prev = poison;
        assert_rejected_for(&f.st, &evict(&f.owner_prev, 9_999_999), "good standing");
        assert!(
            f.st.leaves_confirmed(CH).contains(&b_leaf),
            "the victim is still in the group, and the fold still sees her there"
        );

        // The same claim also shrank `members x live_devices`, so a partial
        // reset (the OTHER unlogged eviction, spec C7) looked like exact cover.
        let w1 = welcome_for(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1,
            "c".repeat(64), &alice_pk, &alice_did);
        f.st.apply(&w1).expect("next-generation welcome staging folds");
        f.owner_prev = w1.clone();
        let partial = Ev::next(&f.owner_dev, owner_pk.clone(), f.sid.clone(), Some(&f.owner_prev),
            f.owner_prev.core.lamport + 1, 9_999_999,
            EP::MlsGroupReset {
                channel_id: CH, new_generation: 1, welcomes: vec![w1.hash()], post_tree_hash: T3,
            });
        assert_rejected_for(&f.st, &partial, "non-selective reset");

        // The honest expiry path still works: once the LOG's own clock (two
        // distinct identities) has moved past t=600, the dead cert is visible
        // drift and the removal folds.
        let alice_later = post_at(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 700);
        f.st.apply(&alice_later).expect("alice posts at t=700 from her live device");
        f.alice_prev = alice_later;
        assert!(
            f.st.pending_removals(CH, 700).contains(&b_leaf),
            "an expired cert is drift once the log corroborates the moment"
        );
        let rekey = evict(&f.owner_prev, 700);
        f.st.apply(&rekey).expect("removing a cert-expired leaf folds when the log's clock says so");
        assert!(f.st.pending_removals(CH, 700).is_empty(), "the drift is discharged");
    }

    /// Spec C3 promises that a bogus Welcome "leaves visible drift and gets
    /// retried automatically". The fold-state formula says otherwise: a leaf
    /// stuck in `leaves_pending` is subtracted from `pending_adds` AND absent
    /// from `pending_removals`, so it produced zero drift — while a re-add was
    /// refused ("already present or pending"), the removal was refused (its
    /// owner is in good standing), and the victim could not author the fix
    /// (that needs a CONFIRMED leaf). One bogus Welcome, or a steward crashing
    /// between commit and Welcome, was a permanent invisible lockout with only
    /// the owner-only reset as recovery. Now: the obligation is visible in
    /// `pending_confirmations`, and an unproven leaf can be dropped and
    /// re-driven — which evicts nobody, because the device reappears in
    /// `pending_adds` the moment it is dropped.
    #[test]
    fn an_unconfirmed_leaf_is_visible_and_can_be_re_driven() {
        let mut f = e2ee_fixture();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f); // epoch 2
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();

        // Bob joins and publishes two key packages (the second is the retry's).
        let (bob, bob_dev, bob_last) =
            add_member_with_cap(&mut f.st, &f.owner, &f.owner_dev, &mut f.owner_prev, None);
        let bob_pk = bob.public_key();
        let bob_leaf = (bob_pk.clone(), device_id(&bob_dev.public_key()));
        let kp1 = kp_publish(&bob_dev, &bob_pk, &f.sid, &bob_last, [4u8; 32], 1_000_000, 500);
        f.st.apply(&kp1).expect("bob's first key package publishes");
        let kp2 = kp_publish(&bob_dev, &bob_pk, &f.sid, &kp1, [4u8; 32], 1_000_000, 500);
        f.st.apply(&kp2).expect("bob's second key package publishes");

        // The add-commit lands; bob's Welcome never works, so he never confirms.
        let add = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 2,
            vec![add_of(&bob_pk, &bob_dev, &kp1.hash())], vec![], X1, X2, T2, OWNER_STORE);
        f.st.apply(&add).expect("the add-commit folds");
        f.owner_prev = add;

        // Both derived drift sets are silent about him...
        assert!(f.st.pending_adds(CH, 500).is_empty(), "a pending leaf is subtracted from pending_adds");
        assert!(f.st.pending_removals(CH, 500).is_empty(), "and it is not drift to remove either");
        // ...but the retry obligation is exposed, so a steward can see it.
        assert_eq!(
            f.st.pending_confirmations(CH), HashSet::from([bob_leaf.clone()]),
            "the unconfirmed leaf is the visible obligation"
        );
        // A re-add is still refused (the leaf occupies the slot).
        let re_add = mls_commit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 3,
            vec![add_of(&bob_pk, &bob_dev, &kp2.hash())], vec![], X2, X3, T3, ALICE_STORE);
        assert_rejected_for(&f.st, &re_add, "already present or pending");

        // Dropping the unproven leaf is now permitted — and it evicts nobody:
        // bob is a member in good standing, so his device is `pending_adds`
        // drift the instant it leaves the group.
        let drop = mls_commit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 3,
            vec![], vec![rem_of(&bob_pk, &bob_dev)], X2, X3, T3, ALICE_STORE);
        f.st.apply(&drop).expect("an unconfirmed leaf can be dropped");
        f.alice_prev = drop;
        assert!(f.st.pending_confirmations(CH).is_empty());
        assert!(
            f.st.pending_adds(CH, 500).contains(&bob_leaf),
            "the dropped device is immediately visible drift — the add is simply re-driven"
        );

        // ...and the retry goes through with a fresh KeyPackage.
        let redo = mls_commit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 4,
            vec![add_of(&bob_pk, &bob_dev, &kp2.hash())], vec![], X3, X4, T4, ALICE_STORE);
        f.st.apply(&redo).expect("the re-add discharges the drift and folds");
        f.alice_prev = redo;
        let cf = leaf_confirm(&bob_dev, &bob_pk, &f.sid, &kp2, 5, T4, [4u8; 32]);
        f.st.apply(&cf).expect("bob's confirmation folds");
        assert!(f.st.leaves_confirmed(CH).contains(&bob_leaf), "bob is really in the group now");
        assert!(f.st.pending_adds(CH, 500).is_empty() && f.st.pending_confirmations(CH).is_empty());

        // A CONFIRMED leaf of a member in good standing is still untouchable.
        let evict = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 5,
            vec![], vec![rem_of(&bob_pk, &bob_dev)], X4, [17u8; 32], [27u8; 32], OWNER_STORE);
        assert_rejected_for(&f.st, &evict, "good standing");
    }

    /// The envelope's cert-expiry gate judged the RAW author timestamp, so a
    /// device whose cert had died kept full control-plane authority by
    /// back-dating: nothing in the chain forces timestamp monotonicity, and
    /// `MlsCommit` authz never checks that the AUTHORING device is live. Such a
    /// commit zeroes `events_since_last_commit` — defeating the C4 freshness
    /// ceiling for good — and sets the chain variable. Expiry is now judged at
    /// the identity's monotone clock.
    #[test]
    fn an_expired_device_cannot_author_by_back_dating() {
        let mut f = e2ee_fixture();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f); // epoch 2
        let alice_pk = f.alice.public_key();
        let (b_dev, _b_did, b_prev) = alice_adds_an_expiring_second_device(&mut f);

        // Alice's clock moves past the second device's expiry (t=600).
        let later = post_at(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 700);
        f.st.apply(&later).expect("alice posts at t=700 from her live device");
        f.alice_prev = later;

        // The dead device back-dates to t=500 — where its cert was still alive.
        let backdated_post = post_at(&b_dev, &alice_pk, &f.sid, &b_prev, 500);
        assert_rejected_for(&f.st, &backdated_post, "device cert has expired");
        let backdated_commit = mls_commit_at(&b_dev, &alice_pk, &f.sid, &b_prev, 3, vec![], vec![],
            X2, X3, T3, B_STORE_R2, 500);
        assert_rejected_for(&f.st, &backdated_commit, "device cert has expired");
        assert_eq!(
            f.st.mls_current_epoch(CH), Some((0, 3)),
            "the dead device moved neither the epoch nor the chain variable"
        );
        assert_ne!(
            f.st.mls_groups.get(&CH).unwrap().epoch_authenticator, Some(X3),
            "...and did not get to declare the next authenticator"
        );
    }

    /// Tombstones carry unvalidated targets (the fold has no message index by
    /// design), so they must never spend forward-secrecy budget — otherwise any
    /// member seals any E2ee channel on demand for the price of 500 events.
    #[test]
    fn tombstone_spam_cannot_seal_an_e2ee_channel() {
        let mut f = e2ee_fixture();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f);
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();

        for i in 0..=FRESHNESS_CEILING_EVENTS {
            let e = Ev::next(&f.alice_dev, alice_pk.clone(), f.sid.clone(), Some(&f.alice_prev),
                f.alice_prev.core.lamport + 1, 500,
                EP::MessageDeleted {
                    channel_id: CH, target: format!("{i:064x}"), reason: DeleteReason::Author,
                });
            f.st.apply(&e).unwrap_or_else(|err| panic!("tombstone {i} should fold: {err}"));
            f.alice_prev = e;
        }
        let g = f.st.mls_groups.get(&CH).unwrap();
        assert_eq!(g.events_since_last_commit, 0, "tombstones spend no freshness budget");
        assert_eq!(
            g.channel_events_since_reset, FRESHNESS_CEILING_EVENTS + 1,
            "they do count toward the reset rate-limit clock"
        );
        let post = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 0, 2, vec![]);
        f.st.apply(&post).expect("the channel is still writable after tombstone spam");
    }

    /// The legacy carve-out is one-way: a channel that already carried
    /// plaintext can never be declared afterwards. The FOLD refuses it, so a
    /// Rung-3 replica replaying from genesis refuses it too — sub-3's
    /// `messages`-table check is belt-and-braces, not the only defense.
    #[test]
    fn a_channel_with_plaintext_history_can_never_be_declared() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();
        let owner_pk = owner.public_key();

        let legacy = post_in(&owner_dev, &owner_pk, &sid, &da, 9);
        st.apply(&legacy).expect("legacy plaintext post folds (the carve-out)");

        let upgrade = channel(&owner_dev, &owner_pk, &sid, &legacy, 9, ChannelClass::E2ee, None);
        assert_rejected_for(&st, &upgrade, "plaintext history");
        let redeclare = channel(&owner_dev, &owner_pk, &sid, &legacy, 9, ChannelClass::Plaintext, None);
        assert_rejected_for(&st, &redeclare, "plaintext history");
        assert_eq!(st.channel_class(9), None, "channel 9 stays a legacy plaintext channel");

        // An unused channel id is unaffected.
        let fresh = channel(&owner_dev, &owner_pk, &sid, &legacy, 10, ChannelClass::E2ee, None);
        st.apply(&fresh).expect("an unused channel id can still be created as E2ee");
    }

    // ---- Review round 3: the commit-rate rule vs. real channel sizes ----

    /// Deterministic per-epoch chain values: the X*/T* constants do not stretch
    /// to the dozen epochs the freshness-cycle tests walk through.
    fn auth_at(epoch: u64) -> [u8; 32] {
        let mut v = [0xA0u8; 32];
        v[..8].copy_from_slice(&epoch.to_le_bytes());
        v
    }
    fn tree_at(epoch: u64) -> [u8; 32] {
        let mut v = [0x7Eu8; 32];
        v[..8].copy_from_slice(&epoch.to_le_bytes());
        v
    }

    /// Fold owner-authored sealed posts at `epoch` until the channel's freshness
    /// budget is exactly exhausted, and assert the ceiling then bites. Returns
    /// how many posts the channel accepted in this cycle (never 0).
    fn fill_to_ceiling(f: &mut E2eeFix, epoch: u64) -> u32 {
        let owner_pk = f.owner.public_key();
        let mut n = 0;
        while f.st.mls_groups[&CH].events_since_last_commit < FRESHNESS_CEILING_EVENTS {
            let e = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 0, epoch, vec![]);
            f.st.apply(&e).unwrap_or_else(|err| panic!("sealed post {} at epoch {epoch}: {err}", n + 1));
            f.owner_prev = e;
            n += 1;
        }
        assert!(n > 0, "the cycle must have accepted real content");
        let over = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 0, epoch, vec![]);
        assert_rejected_for(&f.st, &over, "freshness ceiling");
        n
    }

    /// One self-update rekey at the group's current `epoch`, chaining onto
    /// `prev_auth`. Returns the fold's verdict, so a test can assert both
    /// acceptance and rate-blocking; on rejection the author's chain head (like
    /// every other piece of state) is left untouched.
    #[allow(clippy::too_many_arguments)]
    fn try_rekey(
        st: &mut LogState, sid: &str, dev: &Keypair, author: &PublicKey, prev: &mut Ev,
        epoch: u64, prev_auth: [u8; 32], store: [u8; 32],
    ) -> Result<()> {
        let c = mls_commit(dev, author, sid, prev, epoch, vec![], vec![], prev_auth,
            auth_at(epoch), tree_at(epoch), store);
        st.apply(&c)?;
        *prev = c;
        Ok(())
    }

    /// Join one more member into the E2ee fixture's group: invite + device +
    /// KeyPackage, an owner add-commit at `epoch` (drift-discharging, so never
    /// rate-blocked) and the joiner's own leaf confirmation.
    fn join_and_confirm(
        f: &mut E2eeFix, epoch: u64, prev_auth: [u8; 32], store: [u8; 32],
    ) -> (Keypair, Keypair, Ev) {
        let owner_pk = f.owner.public_key();
        let (u, ud, last) =
            add_member_with_cap(&mut f.st, &f.owner, &f.owner_dev, &mut f.owner_prev, None);
        let kp = kp_publish(&ud, &u.public_key(), &f.sid, &last, store, 1_000_000, 500);
        f.st.apply(&kp).expect("the joiner's key package publishes");
        let add = mls_commit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, epoch,
            vec![add_of(&u.public_key(), &ud, &kp.hash())], vec![],
            prev_auth, auth_at(epoch), tree_at(epoch), OWNER_STORE);
        f.st.apply(&add).expect("an add-commit discharges drift, so it is never rate-blocked");
        f.owner_prev = add;
        let cf = leaf_confirm(&ud, &u.public_key(), &f.sid, &kp, epoch + 1, tree_at(epoch), store);
        f.st.apply(&cf).expect("the joiner confirms its leaf");
        (u, ud, cf)
    }

    /// THE round-3 CRITICAL. Every accepted commit advances the epoch by exactly
    /// one, so with M round-robining authors a member's next turn arrives M
    /// epochs later — and the rate rule demanded a RAW gap of 4. With M <= 3 and
    /// no drift, every member was permanently rate-blocked the moment it had
    /// spent its one exempt "first commit", and 500 sealed events later the
    /// freshness ceiling sealed the channel FOREVER (the reset hatch cannot
    /// rescue it twice: `channel_events_since_reset` only advances on content
    /// the ceiling has already stopped). The spec's own "#private with a friend"
    /// channel is M = 2. This drives four full ceiling cycles — the second is
    /// where every real channel lives, and where the old rule died.
    #[test]
    fn two_member_channel_survives_repeated_freshness_cycles() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        apply_bootstrap(&mut f);       // owner commits at epoch 0 -> group epoch 1
        add_and_confirm_alice(&mut f); // owner commits at epoch 1 -> group epoch 2
        let mut prev_auth = X1;
        let mut epoch = 2;
        let mut delivered = 0u32;

        for cycle in 0..4 {
            delivered += fill_to_ceiling(&mut f, epoch);
            // The ceiling is demanding a rekey; SOMEBODY must be able to answer
            // it. Alternate, so no cycle rides on another member's one exempt
            // first commit.
            let r = if cycle % 2 == 0 {
                try_rekey(&mut f.st, &f.sid, &f.alice_dev, &alice_pk, &mut f.alice_prev,
                    epoch, prev_auth, ALICE_STORE)
            } else {
                try_rekey(&mut f.st, &f.sid, &f.owner_dev, &owner_pk, &mut f.owner_prev,
                    epoch, prev_auth, OWNER_STORE)
            };
            r.unwrap_or_else(|e| panic!("cycle {cycle}: the rekey the ceiling demands must fold: {e}"));
            prev_auth = auth_at(epoch);
            epoch += 1;
            assert_eq!(f.st.mls_current_epoch(CH), Some((0, epoch)));
            assert_eq!(f.st.mls_groups[&CH].events_since_last_commit, 0, "the rekey refreshed the budget");
        }

        // ...and content really does keep flowing afterwards.
        let after = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 0, epoch, vec![]);
        f.st.apply(&after).expect("a two-member channel keeps accepting content indefinitely");
        assert!(delivered >= 4 * FRESHNESS_CEILING_EVENTS, "four full ceiling cycles were delivered");
    }

    /// The same, one member up: M = 3 was equally fatal (an author's turn comes
    /// 3 epochs later, the rule demanded 4).
    #[test]
    fn three_member_channel_survives_repeated_freshness_cycles() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f); // group epoch 2, authenticator X1
        let (bob, bob_dev, mut bob_prev) = join_and_confirm(&mut f, 2, X1, [4u8; 32]);
        let bob_pk = bob.public_key();
        let mut prev_auth = auth_at(2);
        let mut epoch = 3;
        assert_eq!(f.st.leaves_confirmed(CH).len(), 3, "three confirmed leaves, three identities");

        for cycle in 0..5 {
            fill_to_ceiling(&mut f, epoch);
            let r = match cycle % 3 {
                0 => try_rekey(&mut f.st, &f.sid, &f.alice_dev, &alice_pk, &mut f.alice_prev,
                        epoch, prev_auth, ALICE_STORE),
                1 => try_rekey(&mut f.st, &f.sid, &bob_dev, &bob_pk, &mut bob_prev,
                        epoch, prev_auth, [4u8; 32]),
                _ => try_rekey(&mut f.st, &f.sid, &f.owner_dev, &owner_pk, &mut f.owner_prev,
                        epoch, prev_auth, OWNER_STORE),
            };
            r.unwrap_or_else(|e| panic!("cycle {cycle}: the rekey the ceiling demands must fold: {e}"));
            prev_auth = auth_at(epoch);
            epoch += 1;
        }
        let after = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 0, epoch, vec![]);
        f.st.apply(&after).expect("a three-member channel keeps accepting content indefinitely");
    }

    /// Fix (c) on its own: the enforced gap is
    /// `min(COMMIT_RATE_MIN_EPOCH_GAP, committing identities)`, so a small
    /// channel can round-robin its rekeys on the CLIENT cadence (weekly / 200
    /// messages) instead of only when the ceiling is about to bite. Without the
    /// scaling, a two-member channel's forward-secrecy window would silently
    /// stretch to "whenever the ceiling grace opens", which is not the bound the
    /// spec sells.
    #[test]
    fn small_channels_can_rekey_on_cadence_not_only_at_the_ceiling() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f); // owner's last commit: epoch 1; group epoch 2

        // A completely fresh freshness budget — no ceiling pressure anywhere.
        assert_eq!(f.st.mls_groups[&CH].events_since_last_commit, 0);
        try_rekey(&mut f.st, &f.sid, &f.alice_dev, &alice_pk, &mut f.alice_prev, 2, X1, ALICE_STORE)
            .expect("alice's first cadence rekey");
        assert_eq!(f.st.mls_groups[&CH].events_since_last_commit, 0);
        try_rekey(&mut f.st, &f.sid, &f.owner_dev, &owner_pk, &mut f.owner_prev, 3, auth_at(2), OWNER_STORE)
            .expect("the owner's turn comes round after 2 epochs in a 2-identity channel");
        try_rekey(&mut f.st, &f.sid, &f.alice_dev, &alice_pk, &mut f.alice_prev, 4, auth_at(3), ALICE_STORE)
            .expect("and alice's again");

        // Turn-taking, not free-for-all: neither member may take two turns in a
        // row, because the gap is still >= 2 while two identities hold leaves.
        let twice = mls_commit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 5, vec![], vec![],
            auth_at(4), auth_at(5), tree_at(5), ALICE_STORE);
        assert_rejected_for(&f.st, &twice, "commit-rate rule");
    }

    /// Fix (a) on its own: when the ceiling is about to seal the channel, the
    /// rate rule steps aside — a rekey the ceiling itself is demanding is never
    /// spam. This is what saves a channel whose only online member has already
    /// taken its turn (an M >= 4 channel where nobody else is around to advance
    /// the epoch), and it closes again the moment the budget is refreshed.
    #[test]
    fn a_lone_rekeyer_can_always_answer_the_freshness_ceiling() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f); // group epoch 2, authenticator X1
        let (_bob, _bd, _bp) = join_and_confirm(&mut f, 2, X1, [4u8; 32]);
        let (_carol, _cd, _cp) = join_and_confirm(&mut f, 3, auth_at(2), [5u8; 32]);
        assert_eq!(f.st.leaves_confirmed(CH).len(), 4, "a full-size channel: the gap is the full 4");
        let mut epoch = 4;
        let mut prev_auth = auth_at(3);

        // Alice takes her turn while the budget is fresh...
        try_rekey(&mut f.st, &f.sid, &f.alice_dev, &alice_pk, &mut f.alice_prev, epoch, prev_auth, ALICE_STORE)
            .expect("alice's first commit is exempt");
        prev_auth = auth_at(epoch);
        epoch += 1;

        // ...and is now rate-blocked, correctly, while there is budget left.
        let early = mls_commit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, epoch, vec![], vec![],
            prev_auth, auth_at(epoch), tree_at(epoch), ALICE_STORE);
        assert_rejected_for(&f.st, &early, "commit-rate rule");

        // Nobody else ever comes online, so nothing advances the epoch and the
        // channel burns its whole budget. The rate rule must now yield, or the
        // ceiling seals a channel whose only present member is willing to rekey.
        fill_to_ceiling(&mut f, epoch);
        try_rekey(&mut f.st, &f.sid, &f.alice_dev, &alice_pk, &mut f.alice_prev, epoch, prev_auth, ALICE_STORE)
            .expect("the ceiling demands a rekey and the only member present must be able to author it");
        prev_auth = auth_at(epoch);
        epoch += 1;
        let resumed = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 0, epoch, vec![]);
        f.st.apply(&resumed).expect("content flows again");
        f.owner_prev = resumed;

        // The hatch is not a standing licence: with the budget refreshed, the
        // same author is rate-blocked again.
        let again = mls_commit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, epoch, vec![], vec![],
            prev_auth, auth_at(epoch), tree_at(epoch), ALICE_STORE);
        assert_rejected_for(&f.st, &again, "commit-rate rule");
    }

    /// The anti-spam property the rate rule exists for (spec I3) is untouched in
    /// a channel big enough to have it: one member cannot bounce everybody
    /// else's in-flight sealed messages with back-to-back self-updates.
    #[test]
    fn commit_rate_rule_still_blocks_spam_in_a_large_channel() {
        let mut f = e2ee_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        apply_bootstrap(&mut f);
        add_and_confirm_alice(&mut f);
        let (bob, bob_dev, mut bob_prev) = join_and_confirm(&mut f, 2, X1, [4u8; 32]);
        let (carol, carol_dev, mut carol_prev) = join_and_confirm(&mut f, 3, auth_at(2), [5u8; 32]);
        let (bob_pk, carol_pk) = (bob.public_key(), carol.public_key());
        assert_eq!(f.st.leaves_confirmed(CH).len(), 4);

        // Alice takes her turn at epoch 4, then tries to hog every epoch after
        // it — each attempt is rejected until the full gap of 4 has passed.
        try_rekey(&mut f.st, &f.sid, &f.alice_dev, &alice_pk, &mut f.alice_prev, 4, auth_at(3), ALICE_STORE)
            .expect("alice's first commit");
        let mut prev_auth = auth_at(4);
        for epoch in 5..8 {
            let spam = mls_commit(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, epoch, vec![], vec![],
                prev_auth, auth_at(epoch), tree_at(epoch), ALICE_STORE);
            assert_rejected_for(&f.st, &spam, "commit-rate rule");
            // Somebody else takes the turn instead, so the epoch still moves
            // (the owner last committed at epoch 3, so its turn is epoch 7).
            let r = match epoch {
                5 => try_rekey(&mut f.st, &f.sid, &bob_dev, &bob_pk, &mut bob_prev,
                        epoch, prev_auth, [4u8; 32]),
                6 => try_rekey(&mut f.st, &f.sid, &carol_dev, &carol_pk, &mut carol_prev,
                        epoch, prev_auth, [5u8; 32]),
                _ => try_rekey(&mut f.st, &f.sid, &f.owner_dev, &owner_pk, &mut f.owner_prev,
                        epoch, prev_auth, OWNER_STORE),
            };
            r.unwrap_or_else(|e| panic!("another member's turn at epoch {epoch}: {e}"));
            prev_auth = auth_at(epoch);
        }
        // Four epochs on, alice's turn comes round again.
        try_rekey(&mut f.st, &f.sid, &f.alice_dev, &alice_pk, &mut f.alice_prev, 8, prev_auth, ALICE_STORE)
            .expect("alice may commit again once the full gap has passed");
    }

    /// THE round-3 IMPORTANT: `reset_pending` was a latch cleared inside ONE
    /// event type (`MlsLeafConfirmed`), so a welcomed device that is BANNED
    /// before it confirms left the channel in a terminal state — the bridge's
    /// own answer (a Remove-commit dropping the unproven leaf) emptied
    /// `pending_removals`, `pending_adds` AND `pending_confirmations`, yet
    /// sealed sends stayed refused and no confirmation could ever arrive. Only
    /// another owner reset escaped, destroying continuity. The gate is now
    /// DERIVED — the reset generation is incomplete iff a leaf the reset STAGED
    /// is still unconfirmed — so it self-heals on removal exactly as it does on
    /// confirmation. (Recurring bug class: an over-conservative guard creating
    /// an unexitable state.)
    #[test]
    fn a_reset_completes_when_a_stuck_leaf_is_removed_not_only_when_it_confirms() {
        const TR: [u8; 32] = [77u8; 32];
        let (mut f, bob, bob_dev, _bob_last) = reset_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        let alice_did = device_id(&f.alice_dev.public_key());
        let bob_pk = bob.public_key();
        let bob_did = device_id(&bob_dev.public_key());
        let bob_leaf = (bob_pk.clone(), bob_did.clone());

        let w_alice = stage_welcome(&mut f, 1, &alice_pk, &alice_did);
        let w_bob = stage_welcome(&mut f, 1, &bob_pk, &bob_did);
        let reset = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, vec![w_alice, w_bob], TR);
        f.st.apply(&reset).expect("the exact-cover reset folds");
        f.owner_prev = reset;

        // Alice confirms; bob is banned before he ever does.
        let ca = leaf_confirm_gen(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 1, 1, TR, ALICE_STORE);
        f.st.apply(&ca).expect("alice's post-reset confirmation folds");
        f.alice_prev = ca;
        ban_of(&mut f, &bob_pk);
        assert!(f.st.pending_removals(CH, 500).contains(&bob_leaf), "the banned joiner is drift");
        let sealed = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 1, 1, vec![]);
        assert_rejected_for(&f.st, &sealed, "pending removals");

        // The bridge's own answer: a Remove-commit drops the unproven leaf. No
        // `MlsLeafConfirmed` for bob can ever arrive after this — he is banned,
        // and his leaf is gone.
        let rekey = mls_commit_gen(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, 1,
            vec![], vec![rem_of(&bob_pk, &bob_dev)], [0u8; 32], X1, T1, OWNER_STORE);
        f.st.apply(&rekey).expect("the remove-commit folds");
        f.owner_prev = rekey;

        // Every drift set the fold exposes is now empty...
        assert!(f.st.pending_removals(CH, 500).is_empty());
        assert!(f.st.pending_adds(CH, 500).is_empty());
        assert!(f.st.pending_confirmations(CH).is_empty());
        assert_eq!(
            f.st.leaves_confirmed(CH),
            HashSet::from([(owner_pk.clone(), device_id(&f.owner_dev.public_key())), (alice_pk, alice_did)]),
            "the confirmed leaves are exactly the fold's member devices"
        );
        // ...so the channel must be sendable. (This is the assertion that failed
        // before the gate was derived: the latch stayed set forever.)
        let after = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 1, 2, vec![]);
        f.st.apply(&after).expect("a reset generation whose drift was discharged by REMOVAL is complete");
        f.owner_prev = after;

        // And the derived gate is scoped to the reset's OWN staged leaves: an
        // ordinary join into the completed generation leaves a pending leaf
        // behind, and that must NOT re-seal the channel (which a naive
        // "any pending leaf ⇒ incomplete" derivation would do).
        let (dave, dave_dev, dave_last) =
            add_member_with_cap(&mut f.st, &f.owner, &f.owner_dev, &mut f.owner_prev, None);
        let dkp = kp_publish(&dave_dev, &dave.public_key(), &f.sid, &dave_last, [6u8; 32], 1_000_000, 500);
        f.st.apply(&dkp).expect("dave's key package publishes");
        let add = mls_commit_gen(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, 2,
            vec![add_of(&dave.public_key(), &dave_dev, &dkp.hash())], vec![], X1, X2, T2, OWNER_STORE);
        f.st.apply(&add).expect("dave's add-commit folds");
        f.owner_prev = add;
        assert_eq!(f.st.pending_confirmations(CH).len(), 1, "dave's leaf is the outstanding obligation");
        let still_open = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 1, 3, vec![]);
        f.st.apply(&still_open).expect("an ordinary pending join never seals the channel");
    }

    /// The other side of the derived gate: it must NOT be dischargeable by
    /// EVICTING a co-staged member who is in good standing. Removing a
    /// pending-only leaf is open to any member (round 2 — an unproven Add is not
    /// a tree member, and gating it was an invisible lockout), and a
    /// generation's first commit is exempt from the rate rule, so the FIRST
    /// welcomed device to confirm could otherwise drop every peer that had not
    /// confirmed yet, reopen the channel, and leave them permanently outside it
    /// while `pending_adds` — which gates nothing — quietly listed them. That is
    /// exactly the silent partition C7 exists to make impossible.
    #[test]
    fn a_first_confirmer_cannot_evict_a_co_staged_member_and_reopen_the_channel() {
        const TR: [u8; 32] = [77u8; 32];
        let (mut f, bob, bob_dev, bob_last) = reset_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        let alice_did = device_id(&f.alice_dev.public_key());
        let bob_pk = bob.public_key();
        let bob_did = device_id(&bob_dev.public_key());
        let bob_leaf = (bob_pk.clone(), bob_did.clone());

        let w_alice = stage_welcome(&mut f, 1, &alice_pk, &alice_did);
        let w_bob = stage_welcome(&mut f, 1, &bob_pk, &bob_did);
        let reset = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, vec![w_alice, w_bob], TR);
        f.st.apply(&reset).expect("the exact-cover reset folds");
        f.owner_prev = reset;

        // Alice confirms first. Bob is a member in good standing with a live
        // device — no ban, no revocation, no drift of any kind.
        let ca = leaf_confirm_gen(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 1, 1, TR, ALICE_STORE);
        f.st.apply(&ca).expect("alice's post-reset confirmation folds");
        f.alice_prev = ca;
        assert!(f.st.pending_removals(CH, 500).is_empty(), "bob is in good standing");

        // ALICE — not the owner — drops bob's still-pending leaf. The commit is
        // authorized (pending-only leaves carry no good-standing gate, and it is
        // her first commit of the generation, so the rate rule exempts it)...
        let evict = mls_commit_gen(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 1, 1,
            vec![], vec![rem_of(&bob_pk, &bob_dev)], [0u8; 32], X1, T1, ALICE_STORE);
        f.st.apply(&evict).expect("dropping an unproven leaf stays authorized (round 2)");
        f.alice_prev = evict;

        // ...but it discharges NOTHING: the reset owes bob a confirmed leaf and
        // he still has not got one, so the channel stays dead, loudly.
        assert!(
            f.st.mls_groups.get(&CH).unwrap().reset_incomplete(),
            "evicting a co-staged member in good standing must not complete the reset"
        );
        let sealed = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 1, 2, vec![]);
        assert_rejected_for(&f.st, &sealed, "reset is incomplete");
        assert!(f.st.pending_adds(CH, 500).contains(&bob_leaf), "bob is visibly owed a leaf");

        // Recovery is the ordinary one — re-drive bob's Add and let him confirm.
        let bkp = kp_publish(&bob_dev, &bob_pk, &f.sid, &bob_last, [4u8; 32], 1_000_000, 500);
        f.st.apply(&bkp).expect("bob's key package publishes");
        let re_add = mls_commit_gen(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, 2,
            vec![add_of(&bob_pk, &bob_dev, &bkp.hash())], vec![], X1, X2, T2, OWNER_STORE);
        f.st.apply(&re_add).expect("bob's re-add folds");
        f.owner_prev = re_add;
        assert!(f.st.mls_groups.get(&CH).unwrap().reset_incomplete(), "still owed until he confirms");
        let cb = leaf_confirm_gen(&bob_dev, &bob_pk, &f.sid, &bkp, 1, 3, T2, [4u8; 32]);
        f.st.apply(&cb).expect("bob's confirmation folds");
        assert!(
            !f.st.mls_groups.get(&CH).unwrap().reset_incomplete(),
            "the reset completes once bob actually holds his leaf"
        );
        let unlocked = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 1, 3, vec![]);
        f.st.apply(&unlocked).expect("sends unlock once every staged leaf is confirmed");
    }

    /// M1: the `reset_welcomed` prune is what stops a discharged obligation from
    /// being resurrected. A staged leaf whose holder left good standing is
    /// dropped from the obligation set for good, so when that holder returns and
    /// is re-added, the pending leaf is an ORDINARY join — it must not re-seal
    /// the channel as a revived reset obligation.
    #[test]
    fn a_discharged_reset_obligation_is_not_resurrected_by_a_later_re_add() {
        const TR: [u8; 32] = [77u8; 32];
        let (mut f, bob, bob_dev, bob_last) = reset_fixture();
        let owner_pk = f.owner.public_key();
        let alice_pk = f.alice.public_key();
        let alice_did = device_id(&f.alice_dev.public_key());
        let bob_pk = bob.public_key();
        let bob_did = device_id(&bob_dev.public_key());

        let w_alice = stage_welcome(&mut f, 1, &alice_pk, &alice_did);
        let w_bob = stage_welcome(&mut f, 1, &bob_pk, &bob_did);
        let reset = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, vec![w_alice, w_bob], TR);
        f.st.apply(&reset).expect("the exact-cover reset folds");
        f.owner_prev = reset;
        let ca = leaf_confirm_gen(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 1, 1, TR, ALICE_STORE);
        f.st.apply(&ca).expect("alice's post-reset confirmation folds");
        f.alice_prev = ca;

        // Bob is banned before confirming: the reset no longer owes him a leaf,
        // and the Remove-commit that clears the drift discharges the obligation.
        ban_of(&mut f, &bob_pk);
        let drop = mls_commit_gen(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, 1,
            vec![], vec![rem_of(&bob_pk, &bob_dev)], [0u8; 32], X1, T1, OWNER_STORE);
        f.st.apply(&drop).expect("the remove-commit folds");
        f.owner_prev = drop;
        assert!(!f.st.mls_groups.get(&CH).unwrap().reset_incomplete(), "the obligation is discharged");
        let open = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 1, 2, vec![]);
        f.st.apply(&open).expect("the channel is sendable again");
        f.owner_prev = open;

        // Bob comes back and is re-added with the SAME (identity, device) leaf.
        let unban = Ev::next(&f.owner_dev, owner_pk.clone(), f.sid.clone(), Some(&f.owner_prev),
            f.owner_prev.core.lamport + 1, 500, EP::MemberUnbanned { member: bob_pk.clone() });
        f.st.apply(&unban).expect("unban folds");
        f.owner_prev = unban;
        let inv = Ev::next(&f.owner_dev, owner_pk.clone(), f.sid.clone(), Some(&f.owner_prev),
            f.owner_prev.core.lamport + 1, 500,
            EP::InviteCreated { code_hash: "c2".into(), max_uses: 10, expires_at: 9999, requires_approval: false });
        f.st.apply(&inv).expect("the re-invite folds");
        f.owner_prev = inv.clone();
        let rejoin = Ev::next(&bob_dev, bob_pk.clone(), f.sid.clone(), Some(&bob_last),
            bob_last.core.lamport + 1, 500,
            EP::MemberJoined { member: bob_pk.clone(), invite: inv.hash() });
        f.st.apply(&rejoin).expect("bob rejoins");
        let bkp = kp_publish(&bob_dev, &bob_pk, &f.sid, &rejoin, [4u8; 32], 1_000_000, 500);
        f.st.apply(&bkp).expect("bob's key package publishes");
        let re_add = mls_commit_gen(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, 2,
            vec![add_of(&bob_pk, &bob_dev, &bkp.hash())], vec![], X1, X2, T2, OWNER_STORE);
        f.st.apply(&re_add).expect("bob's re-add folds");
        f.owner_prev = re_add;

        // Without the prune, bob's leaf re-enters the reset's obligation set and
        // the channel seals itself again on a join that has nothing to do with
        // the reset.
        assert_eq!(f.st.pending_confirmations(CH), HashSet::from([(bob_pk, bob_did)]));
        assert!(
            !f.st.mls_groups.get(&CH).unwrap().reset_incomplete(),
            "a re-add after a discharged obligation is an ordinary join"
        );
        let still_open = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 1, 3, vec![]);
        f.st.apply(&still_open).expect("an ordinary pending join never re-seals the channel");
    }

    // ---- Rung 2, Task 5: checkpoint composability over ALL the new state ----

    /// Whole-state structural equality: EVERY `LogState` field, so a fold path
    /// that diverges anywhere (not just in the queried surfaces) fails loudly.
    fn assert_fold_equal(a: &LogState, b: &LogState, ctx: &str) {
        assert_eq!(a.server_id, b.server_id, "{ctx}: server_id");
        assert_eq!(a.owner, b.owner, "{ctx}: owner");
        assert_eq!(a.members, b.members, "{ctx}: members");
        assert_eq!(a.pending, b.pending, "{ctx}: pending");
        assert_eq!(a.banned, b.banned, "{ctx}: banned");
        assert_eq!(a.capabilities, b.capabilities, "{ctx}: capabilities");
        assert_eq!(a.devices, b.devices, "{ctx}: devices");
        assert_eq!(a.invites, b.invites, "{ctx}: invites");
        assert_eq!(a.chains, b.chains, "{ctx}: chains");
        assert_eq!(a.attachment_uploaders, b.attachment_uploaders, "{ctx}: attachment_uploaders");
        assert_eq!(a.redacted_attachments, b.redacted_attachments, "{ctx}: redacted_attachments");
        assert_eq!(a.channels, b.channels, "{ctx}: channels");
        assert_eq!(
            a.plaintext_history_channels, b.plaintext_history_channels,
            "{ctx}: plaintext_history_channels"
        );
        assert_eq!(a.identity_clock, b.identity_clock, "{ctx}: identity_clock");
        assert_eq!(a.corroborated_clock, b.corroborated_clock, "{ctx}: corroborated_clock");
        assert_eq!(a.tombstones, b.tombstones, "{ctx}: tombstones");
        assert_eq!(a.revoked_devices, b.revoked_devices, "{ctx}: revoked_devices");
        assert_eq!(a.devices_by_identity, b.devices_by_identity, "{ctx}: devices_by_identity");
        assert_eq!(a.log_pos, b.log_pos, "{ctx}: log_pos");
        assert_eq!(a.mls_groups, b.mls_groups, "{ctx}: mls_groups");
        assert_eq!(a.key_packages, b.key_packages, "{ctx}: key_packages");
        assert_eq!(a.consumed_key_packages, b.consumed_key_packages, "{ctx}: consumed_key_packages");
        assert_eq!(a.device_store_instance, b.device_store_instance, "{ctx}: device_store_instance");
    }

    /// A snapshot of every PUBLIC query surface (the contract sub-3 and the
    /// client consume), taken over the ids/channels/targets of one log.
    #[derive(Debug, PartialEq)]
    struct Surface {
        log_pos: u64,
        classes: Vec<Option<ChannelClass>>,
        epochs: Vec<Option<(u64, u64)>>,
        confirmed: Vec<Vec<(Vec<u8>, DeviceId)>>,
        adds: Vec<Vec<(Vec<u8>, DeviceId)>>,
        removals: Vec<Vec<(Vec<u8>, DeviceId)>>,
        tombstoned: Vec<bool>,
        revoked: Vec<bool>,
        live: Vec<Vec<DeviceId>>,
        membership: Vec<(bool, bool, bool)>,
    }

    fn sorted_leaves(s: HashSet<(PublicKey, DeviceId)>) -> Vec<(Vec<u8>, DeviceId)> {
        let mut v: Vec<(Vec<u8>, DeviceId)> =
            s.into_iter().map(|(pk, d)| (pk.as_bytes().to_vec(), d)).collect();
        v.sort();
        v
    }

    fn surface(
        st: &LogState, channels: &[u64], ids: &[PublicKey], devices: &[DeviceId],
        targets: &[EventRef], at_ts: u64,
    ) -> Surface {
        Surface {
            log_pos: st.log_pos(),
            classes: channels.iter().map(|c| st.channel_class(*c)).collect(),
            epochs: channels.iter().map(|c| st.mls_current_epoch(*c)).collect(),
            confirmed: channels.iter().map(|c| sorted_leaves(st.leaves_confirmed(*c))).collect(),
            adds: channels.iter().map(|c| sorted_leaves(st.pending_adds(*c, at_ts))).collect(),
            removals: channels
                .iter()
                .map(|c| sorted_leaves(st.pending_removals(*c, at_ts)))
                .collect(),
            tombstoned: targets.iter().map(|t| st.is_tombstoned(t)).collect(),
            revoked: devices.iter().map(|d| st.is_device_revoked(d)).collect(),
            live: ids.iter().map(|i| st.live_devices(i, at_ts)).collect(),
            membership: ids
                .iter()
                .map(|i| (st.is_member(i), st.is_pending(i), st.is_banned(i)))
                .collect(),
        }
    }

    /// The spec's "extended `replay_equals_stepwise_and_composes_from_a_checkpoint`
    /// over all new state" (plus its commit-race determinism): one log that
    /// exercises EVERY Rung-2 variant — both channel classes, KeyPackages, the
    /// bootstrap commit, an add-commit + Welcome + joiner confirmation, a STALE
    /// commit (accepted no-op), sealed post/edit, tombstones in both classes,
    /// cert expiry + `DeviceRevoked`, staged Welcomes + reset + post-reset
    /// confirmations — folds identically by replay, stepwise, and resume from
    /// EVERY checkpoint position.
    #[test]
    fn replay_equals_stepwise_and_composes_from_a_checkpoint_over_all_rung2_state() {
        const PLAIN: u64 = 1;
        const BOB_STORE: [u8; 32] = [3u8; 32];
        const TR: [u8; 32] = [77u8; 32];
        const TS: u64 = 500;

        let owner = Keypair::generate();
        let od = Keypair::generate();
        let alice = Keypair::generate();
        let ad = Keypair::generate();
        let bob = Keypair::generate();
        let bd = Keypair::generate();
        let bd2 = Keypair::generate();

        let g = genesis(&owner);
        let sid = g.server_id();
        let owner_pk = owner.public_key();
        let alice_pk = alice.public_key();
        let bob_pk = bob.public_key();
        let owner_did = device_id(&od.public_key());
        let alice_did = device_id(&ad.public_key());
        let bob_did = device_id(&bd.public_key());
        let bob2_did = device_id(&bd2.public_key());

        // --- Rung-1 spine: devices, invite, two joins. Bob has a SECOND device
        // whose cert expires at 400 (so it is not live at the MLS timestamps)
        // and which is revoked besides. ---
        let da_o = Ev::next(&od, owner_pk.clone(), sid.clone(), None, 0, 1,
            EP::DeviceAuthorized { cert: DeviceCert::create(&owner, &od.public_key(), 1) });
        let da_a = Ev::next(&ad, alice_pk.clone(), sid.clone(), None, 0, 1,
            EP::DeviceAuthorized { cert: DeviceCert::create(&alice, &ad.public_key(), 1) });
        let da_b = Ev::next(&bd, bob_pk.clone(), sid.clone(), None, 0, 1,
            EP::DeviceAuthorized { cert: DeviceCert::create(&bob, &bd.public_key(), 1) });
        let inv = Ev::next(&od, owner_pk.clone(), sid.clone(), Some(&da_o), 1, 100,
            EP::InviteCreated { code_hash: "c".into(), max_uses: 10, expires_at: 9999, requires_approval: false });
        let join_a = Ev::next(&ad, alice_pk.clone(), sid.clone(), Some(&da_a), 1, 2,
            EP::MemberJoined { member: alice_pk.clone(), invite: inv.hash() });
        let join_b = Ev::next(&bd, bob_pk.clone(), sid.clone(), Some(&da_b), 1, 2,
            EP::MemberJoined { member: bob_pk.clone(), invite: inv.hash() });
        let da_b2 = Ev::next(&bd2, bob_pk.clone(), sid.clone(), None, 0, 300,
            EP::DeviceAuthorized {
                cert: DeviceCert::create_expiring(&bob, &bd2.public_key(), 100, 400),
            });
        let revoke_b2 = Ev::next(&bd, bob_pk.clone(), sid.clone(), Some(&join_b), 2, 310,
            EP::DeviceRevoked { device: bob2_did.clone() });

        // --- Both channel classes, a plaintext post and its tombstone. ---
        let ch_plain = channel(&od, &owner_pk, &sid, &inv, PLAIN, ChannelClass::Plaintext, None);
        let ch_e2ee = channel(&od, &owner_pk, &sid, &ch_plain, CH, ChannelClass::E2ee, None);
        let post_plain = post_in(&ad, &alice_pk, &sid, &join_a, PLAIN);
        let del_plain = Ev::next(&ad, alice_pk.clone(), sid.clone(), Some(&post_plain),
            post_plain.core.lamport + 1, 20,
            EP::MessageDeleted { channel_id: PLAIN, target: post_plain.hash(), reason: DeleteReason::Author });

        // --- MLS control plane: key packages, bootstrap + add commit, a stale
        // commit (accepted no-op), Welcome, joiner confirmation. ---
        let kp_o = kp_publish(&od, &owner_pk, &sid, &ch_e2ee, OWNER_STORE, 1_000_000, TS);
        let kp_a = kp_publish(&ad, &alice_pk, &sid, &del_plain, ALICE_STORE, 1_000_000, TS);
        let kp_b = kp_publish(&bd, &bob_pk, &sid, &revoke_b2, BOB_STORE, 1_000_000, TS);
        let c0 = mls_commit(&od, &owner_pk, &sid, &kp_o, 0, vec![], vec![], [0u8; 32], X0, T0, OWNER_STORE);
        let c1 = mls_commit(&od, &owner_pk, &sid, &c0, 1,
            vec![add_of(&alice_pk, &ad, &kp_a.hash())], vec![], X0, X1, T1, OWNER_STORE);
        // Commit-race determinism: a commit re-declaring the now-past epoch 1 is
        // ACCEPTED and recorded, with zero MLS state change, on every replica.
        let stale = mls_commit(&od, &owner_pk, &sid, &c1, 1, vec![], vec![], X1, X2, T2, OWNER_STORE);
        let w_alice = welcome_for(&od, &owner_pk, &sid, &stale, 0, c1.hash(), &alice_pk, &alice_did);
        let cf_alice = leaf_confirm(&ad, &alice_pk, &sid, &kp_a, 2, T1, ALICE_STORE);

        // --- Sealed content (post + edit), a moderation tombstone in the E2ee
        // channel (which spends the RESET clock only, never freshness). ---
        let cap = AttachmentCap {
            content_hash: "d".repeat(64), declared_type: "image/png".into(),
            size: 3, uploader: owner_pk.clone(),
        };
        let sp1 = sealed_post(&od, &owner_pk, &sid, &w_alice, CH, 0, 2, vec![cap]);
        let sp2 = sealed_post(&ad, &alice_pk, &sid, &cf_alice, CH, 0, 2, vec![]);
        let se = sealed_edit(&ad, &alice_pk, &sid, &sp2, 0, 2, sp2.hash());
        let del_e2ee = Ev::next(&od, owner_pk.clone(), sid.clone(), Some(&sp1), sp1.core.lamport + 1, TS,
            EP::MessageDeleted { channel_id: CH, target: sp1.hash(), reason: DeleteReason::Moderation });

        // --- Non-selective reset: staged next-generation Welcomes, the reset,
        // both post-reset confirmations, then a send that proves the unlock. ---
        let w1_alice = welcome_for(&od, &owner_pk, &sid, &del_e2ee, 1, "c".repeat(64), &alice_pk, &alice_did);
        let w1_bob = welcome_for(&od, &owner_pk, &sid, &w1_alice, 1, "c".repeat(64), &bob_pk, &bob_did);
        let reset = group_reset(&od, &owner_pk, &sid, &w1_bob, 1, vec![w1_alice.hash(), w1_bob.hash()], TR);
        let cfa1 = leaf_confirm_gen(&ad, &alice_pk, &sid, &se, 1, 1, TR, ALICE_STORE);
        let cfb1 = leaf_confirm_gen(&bd, &bob_pk, &sid, &kp_b, 1, 1, TR, BOB_STORE);
        let sp3 = sealed_post(&od, &owner_pk, &sid, &reset, CH, 1, 1, vec![]);

        let plain_target = post_plain.hash();
        let sealed_target = sp1.hash();
        let log = vec![
            da_o, da_a, da_b, inv, join_a, join_b, da_b2, revoke_b2, ch_plain, ch_e2ee,
            post_plain, del_plain, kp_o, kp_a, kp_b, c0, c1, stale, w_alice, cf_alice,
            sp1, sp2, se, del_e2ee, w1_alice, w1_bob, reset, cfa1, cfb1, sp3,
        ];
        let stale_idx = 17;

        // --- replay == stepwise ---
        let replayed = LogState::replay(&g, &log).expect("the whole Rung-2 log replays");
        let mut stepwise = LogState::from_genesis(&g);
        for e in &log {
            stepwise.apply(e).expect("stepwise apply of a valid log");
        }
        assert_fold_equal(&replayed, &stepwise, "replay vs stepwise");

        // The log is not vacuous: it really did exercise every surface.
        assert_eq!(replayed.log_pos(), log.len() as u64, "every event was accepted");
        assert_eq!(replayed.channel_class(PLAIN), Some(ChannelClass::Plaintext));
        assert_eq!(replayed.channel_class(CH), Some(ChannelClass::E2ee));
        assert_eq!(replayed.channel_class(999), None, "unknown channel = legacy plaintext");
        assert_eq!(replayed.mls_current_epoch(CH), Some((1, 1)), "generation 1 after the reset");
        assert_eq!(
            replayed.leaves_confirmed(CH),
            HashSet::from([
                (owner_pk.clone(), owner_did.clone()),
                (alice_pk.clone(), alice_did.clone()),
                (bob_pk.clone(), bob_did.clone()),
            ]),
            "every welcomed leaf confirmed, so the reset completed"
        );
        assert!(replayed.pending_adds(CH, TS).is_empty() && replayed.pending_removals(CH, TS).is_empty());
        assert!(replayed.is_tombstoned(&plain_target) && replayed.is_tombstoned(&sealed_target));
        assert!(replayed.is_device_revoked(&bob2_did) && !replayed.is_device_revoked(&bob_did));
        assert_eq!(replayed.live_devices(&bob_pk, TS), vec![bob_did.clone()], "revoked+expired device is not live");
        assert_eq!(replayed.attachment_uploader(&"d".repeat(64)), Some(&owner_pk), "sealed caps stay outside the seal");

        let channels = [PLAIN, CH, 999];
        let ids = [owner_pk.clone(), alice_pk.clone(), bob_pk.clone()];
        let devices = [owner_did.clone(), alice_did, bob_did, bob2_did];
        let targets = [plain_target.clone(), sealed_target.clone(), "e".repeat(64)];
        let expected = surface(&replayed, &channels, &ids, &devices, &targets, TS);
        assert_eq!(
            surface(&stepwise, &channels, &ids, &devices, &targets, TS), expected,
            "every query surface must agree between replay and stepwise"
        );

        // --- The stale commit is an ACCEPTED no-op: only envelope accounting
        // (chain head + log_pos) moves; every byte of MLS state stands still. ---
        let mut before = LogState::replay(&g, &log[..stale_idx]).expect("prefix replays");
        let group_before = before.mls_groups.get(&CH).cloned();
        let kps_before = before.key_packages.clone();
        let pos_before = before.log_pos();
        before.apply(&log[stale_idx]).expect("a stale commit is accepted");
        assert_eq!(before.mls_groups.get(&CH).cloned(), group_before, "stale commit changed MLS state");
        assert_eq!(before.key_packages, kps_before, "stale commit consumed a key package");
        assert_eq!(before.log_pos(), pos_before + 1, "an accepted event still advances log_pos");
        assert_eq!(
            before.chains[&(owner_pk.clone(), owner_did.clone())].hash,
            log[stale_idx].hash(),
            "the stale commit's author chain head advances"
        );

        // --- Checkpoint composability from EVERY position: fold the prefix,
        // clone it (the checkpoint), apply the tail — identical state. ---
        for cut in 0..=log.len() {
            let checkpoint = LogState::replay(&g, &log[..cut]).expect("every prefix replays");
            let mut resumed = checkpoint.clone();
            for e in &log[cut..] {
                resumed.apply(e).unwrap_or_else(|err| panic!("tail from checkpoint {cut}: {err}"));
            }
            assert_fold_equal(&resumed, &replayed, &format!("resume from checkpoint {cut}"));
            assert_eq!(
                surface(&resumed, &channels, &ids, &devices, &targets, TS), expected,
                "query surfaces must agree after resuming from checkpoint {cut}"
            );
        }
    }
}
