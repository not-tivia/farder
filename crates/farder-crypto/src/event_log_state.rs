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
/// drift, is A's first commit there, or declares an epoch at least this many
/// epochs past A's previous commit — stops self-update spam from bouncing every
/// other member's in-flight sealed message with `stale-epoch`.
pub const COMMIT_RATE_MIN_EPOCH_GAP: u64 = 4;

/// Spec C4/I1: the blind rekey ceiling. Once this many channel events have
/// accumulated since the last accepted commit, sealed content becomes invalid —
/// the channel stops accepting new content until somebody rekeys, so forward
/// secrecy is an invariant a host that cannot read a word enforces.
pub const FRESHNESS_CEILING_EVENTS: u32 = 500;

/// Spec C7: reset rate limit — at most one `MlsGroupReset` per channel per this
/// many channel events. A channel's FIRST reset is always allowed.
pub const RESET_MIN_CHANNEL_EVENTS: u32 = 1000;

/// A device authorized within this server's log (identity ↔ signing subkey).
#[derive(Clone, Debug)]
struct DeviceRecord {
    identity: PublicKey,
    device_pubkey: PublicKey,
    /// Cert expiry (unix seconds) from the `DeviceAuthorized` cert, if any.
    /// Judged against `event.core.timestamp` — the untrusted author clock,
    /// the same acceptance Rung 1 made for invite expiry.
    expires_at: Option<u64>,
}

/// A channel known to the log (from `ChannelCreated`). The class is immutable:
/// no class-change event exists by construction. A channel ABSENT from this map
/// is a legacy DB channel — permanently plaintext (replay carve-out).
#[derive(Clone, Debug)]
struct ChannelRecord {
    /// name/kind/parent are recorded for sub-3's derive path; the fold itself
    /// reads only `class` (and resolves `parent` at creation time).
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    kind: String,
    class: ChannelClass,
    #[allow(dead_code)]
    parent: Option<u64>,
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
    /// no commit accepted yet in this generation (bootstrap: chain + confirmed
    /// -leaf checks are exempt for the generation's first commit, resolved
    /// ambiguity #5).
    epoch_authenticator: Option<[u8; 32]>,
    #[allow(dead_code)] // read by sub-3 (derive/diagnostics)
    tree_hash: Option<[u8; 32]>,
    leaves_confirmed: HashSet<(PublicKey, DeviceId)>,
    leaves_pending: HashSet<(PublicKey, DeviceId)>,
    /// Freshness-ceiling counter (spec C4): channel events (sealed content and
    /// tombstones) since the last accepted commit. Reset to 0 by every accepted
    /// commit and by a reset; `>= FRESHNESS_CEILING_EVENTS` seals the channel.
    events_since_last_commit: u32,
    /// Commit-rate clock (spec I3): the DECLARED epoch of each author's last
    /// accepted commit in this channel.
    last_commit_epoch_by_author: HashMap<PublicKey, u64>,
    /// Reset rate-limit clock (spec C7): channel events since the last accepted
    /// `MlsGroupReset`. A channel's first reset ignores it (generation 0).
    channel_events_since_reset: u32,
    /// True from an accepted `MlsGroupReset` until every welcomed leaf confirms.
    /// While set, sealed content is invalid — a partial reset is a dead channel,
    /// loudly, never a silent partition.
    reset_pending: bool,
    /// The tree hash seeded by the FIRST `MlsLeafConfirmed` of a reset
    /// generation (its add-commit is never a log event, so there is no
    /// `commits_by_epoch` entry to check against — resolved ambiguity #7).
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
            reset_pending: false,
            reset_expected_tree_hash: None,
            commits_by_epoch: HashMap::new(),
            welcomes: HashMap::new(),
        }
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
#[derive(Clone, Debug)]
struct InviteRecord {
    max_uses: u32,
    expires_at: u64,
    use_count: u32,
    requires_approval: bool,
}

/// Head of one `(author, device)` chain.
#[derive(Clone, Debug)]
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
            tombstones: HashSet::new(),
            revoked_devices: HashSet::new(),
            devices_by_identity: HashMap::new(),
            log_pos: 0,
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
    /// An identity's live devices at `at_ts`: authorized, non-revoked, and
    /// cert-unexpired at that (untrusted, author-claimed) timestamp. Sorted so
    /// the result is deterministic for every fold consumer.
    pub fn live_devices(&self, pk: &PublicKey, at_ts: u64) -> Vec<DeviceId> {
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
                    .is_none_or(|t| at_ts <= t)
            })
            .cloned()
            .collect();
        live.sort();
        live
    }

    /// The spec's MLS target set: every full member's live devices at `at_ts`.
    /// Pending (unapproved) members are not in `members`, so they are excluded.
    fn member_leaf_set(&self, at_ts: u64) -> HashSet<(PublicKey, DeviceId)> {
        let mut set = HashSet::new();
        for m in &self.members {
            for d in self.live_devices(m, at_ts) {
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
        // Expiry is judged against `event.core.timestamp` — the untrusted
        // author clock, the same acceptance Rung 1 made for invite expiry.
        // For DeviceAuthorized the expiry comes from the payload cert itself
        // (self-bootstrap: registering with an already-expired cert is invalid).
        let cert_expiry = match &event.core.payload {
            EventPayload::DeviceAuthorized { cert } => cert.core.expires_at,
            _ => self.devices.get(&event.core.device).and_then(|r| r.expires_at),
        };
        if let Some(t) = cert_expiry {
            ensure!(event.core.timestamp <= t, "device cert has expired");
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
        Ok(())
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
                // Bootstrap (resolved ambiguity #5): the first commit of a
                // generation has nothing to chain to and no confirmed tree yet —
                // both the chain check and the confirmed-leaf requirement are
                // exempt. `epoch_authenticator` is None exactly then.
                if let Some(expected) = group.epoch_authenticator {
                    ensure!(
                        group
                            .leaves_confirmed
                            .contains(&(author.clone(), event.core.device.clone())),
                        "commit author does not hold a confirmed leaf"
                    );
                    // Chain (spec C3): a commit must chain onto the authenticator
                    // the previously accepted commit DECLARED. A liar therefore
                    // cannot be built upon — the next honest commit fails here.
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
                        self.live_devices(&add.identity, event.core.timestamp)
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
                    ensure!(
                        group.leaves_confirmed.contains(&leaf)
                            || group.leaves_pending.contains(&leaf),
                        "declared remove of an absent leaf"
                    );
                    // Bridge rule: a good-standing member's leaf may only be
                    // removed by that member itself (self-removal of a device).
                    let good_standing = self.is_member(&rem.identity)
                        && !self.is_banned(&rem.identity)
                        && self
                            .live_devices(&rem.identity, event.core.timestamp)
                            .contains(&rem.device);
                    ensure!(
                        !good_standing || author == &rem.identity,
                        "cannot remove a leaf of a member in good standing (except self-removal)"
                    );
                }
                // Commit-rate rule (spec I3): drift discharge is NEVER blocked;
                // otherwise the author's first commit, or an epoch gap of at
                // least COMMIT_RATE_MIN_EPOCH_GAP, is required.
                let rate_ok = self.commit_discharges_drift(event)
                    || match group.last_commit_epoch_by_author.get(author) {
                        None => true,
                        Some(&last) => *epoch >= last + COMMIT_RATE_MIN_EPOCH_GAP,
                    };
                ensure!(
                    rate_ok,
                    "commit-rate rule: a non-drift-discharging commit must be its author's first or at least {COMMIT_RATE_MIN_EPOCH_GAP} epochs past their previous one"
                );
                Ok(Authorized::Apply)
            }

            EventPayload::MlsWelcome { channel_id, generation, commit, for_member, for_device, .. } => {
                let group = self
                    .mls_groups
                    .get(channel_id)
                    .context("MlsWelcome cites a channel with no E2ee group")?;
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
                ensure!(
                    *generation == group.generation,
                    "leaf confirmation generation does not match the group"
                );
                // Authored BY the joining device (spec C3: the joiner itself is
                // the only party that can prove its Welcome worked).
                ensure!(
                    group
                        .leaves_pending
                        .contains(&(author.clone(), event.core.device.clone())),
                    "leaf confirmation must come from the joining device of a pending leaf"
                );
                self.check_instance_pin(&event.core.device, store_instance_hash)?;
                if let Some(rec) = group.commits_by_epoch.get(epoch) {
                    ensure!(
                        rec.post_tree_hash == *tree_hash,
                        "confirmed tree hash does not match the cited epoch's commit"
                    );
                } else if group.reset_pending {
                    // Reset generation (resolved ambiguity #7): its add-commit
                    // is never a log event, so the FIRST confirmation seeds the
                    // expected tree hash; every later one must match it.
                    if let Some(expected) = group.reset_expected_tree_hash {
                        ensure!(
                            expected == *tree_hash,
                            "confirmed tree hash does not match the reset generation's seeded tree hash"
                        );
                    }
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

            EventPayload::MlsGroupReset { channel_id, new_generation, welcomes } => {
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
                // occurred here ⇒ the first reset is always allowed.
                ensure!(
                    group.generation == 0
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
        // Partial reset (spec C7) = dead channel, loudly.
        ensure!(
            !group.reset_pending,
            "group reset is incomplete: the channel is sealed until every welcomed leaf confirms"
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
            EventPayload::MessagePosted { attachments, .. } => {
                for cap in attachments {
                    self.attachment_uploaders
                        .entry(cap.content_hash.clone())
                        .or_insert_with(|| cap.uploader.clone());
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
                // A tombstone is a channel event: in an E2ee channel it spends
                // freshness budget like sealed content does (no-op elsewhere).
                self.bump_channel_counters(*channel_id);
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
                let bootstrap = group.epoch_authenticator.is_none();
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

            EventPayload::MlsLeafConfirmed { channel_id, tree_hash, store_instance_hash, .. } => {
                self.pin_instance(&event.core.device, store_instance_hash);
                // Computed before the mutable group borrow; only consulted on
                // the reset path.
                let full_set = self.member_leaf_set(event.core.timestamp);
                let group = self
                    .mls_groups
                    .get_mut(channel_id)
                    .expect("authz verified the group exists");
                let leaf = (event.core.author.clone(), event.core.device.clone());
                group.leaves_pending.remove(&leaf);
                group.leaves_confirmed.insert(leaf);
                if group.reset_pending {
                    // First confirmation of a reset generation seeds the tree
                    // hash every later confirmation must match (ambiguity #7).
                    if group.reset_expected_tree_hash.is_none() {
                        group.reset_expected_tree_hash = Some(*tree_hash);
                    }
                    // A reset stops being pending only when EVERY member device
                    // holds a confirmed leaf (partial reset = dead channel).
                    if group.leaves_confirmed == full_set {
                        group.reset_pending = false;
                    }
                }
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

            EventPayload::MlsGroupReset { channel_id, new_generation, welcomes } => {
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
                group.leaves_pending = welcomed;
                group.events_since_last_commit = 0;
                group.channel_events_since_reset = 0;
                group.last_commit_epoch_by_author.clear();
                group.commits_by_epoch.clear();
                // Stale (older-generation) Welcomes can never be cited again.
                group.welcomes.retain(|_, w| w.generation >= *new_generation);
                group.reset_pending = true;
                group.reset_expected_tree_hash = None;
            }
        }
    }

    /// Spend one channel event of an E2ee channel's freshness and reset budgets
    /// (saturating — a jammed counter stays jammed, which fails closed). No-op
    /// for channels without an MLS group.
    fn bump_channel_counters(&mut self, channel_id: u64) {
        if let Some(group) = self.mls_groups.get_mut(&channel_id) {
            group.events_since_last_commit = group.events_since_last_commit.saturating_add(1);
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
        Ev::next(dev, author.clone(), sid.to_string(), Some(prev), prev.core.lamport + 1, 500,
            EP::MlsCommit {
                channel_id: CH, generation: 0, epoch, mls_message: vec![0xC0],
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
        assert!(f.st.pending_adds(CH, 500).is_empty(), "pending leaf absorbs the declared add");

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
        new_generation: u64, welcomes: Vec<EventRef>,
    ) -> Ev {
        Ev::next(dev, author.clone(), sid.to_string(), Some(prev), prev.core.lamport + 1, 500,
            EP::MlsGroupReset { channel_id: CH, new_generation, welcomes })
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
        // resurrected), and the tombstone itself spends freshness budget.
        let del = Ev::next(&f.owner_dev, owner_pk.clone(), f.sid.clone(), Some(&f.owner_prev),
            f.owner_prev.core.lamport + 1, 500,
            EP::MessageDeleted { channel_id: CH, target: target.clone(), reason: DeleteReason::Author });
        f.st.apply(&del).expect("the tombstone folds");
        f.owner_prev = del;
        let dead = sealed_edit(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 0, 3, target);
        assert_rejected_for(&f.st, &dead, "tombstoned");
        assert_eq!(
            f.st.mls_groups.get(&CH).unwrap().events_since_last_commit, 2,
            "the resumed edit and the tombstone each spent one channel event"
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
        let missing = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, vec![w_alice.clone()]);
        assert_rejected_for(&f.st, &missing, "cover exactly");

        let w_bob = stage_welcome(&mut f, 1, &bob_pk, &bob_did);

        // An extra, non-member device ⇒ rejected.
        let stranger = Keypair::generate().public_key();
        let w_stranger = stage_welcome(&mut f, 1, &stranger, &"f".repeat(64));
        let extra = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1,
            vec![w_alice.clone(), w_bob.clone(), w_stranger]);
        assert_rejected_for(&f.st, &extra, "cover exactly");

        // Duplicate refs ⇒ rejected; the resetter's own device ⇒ rejected (it
        // is the new generation's creator, never a welcomed leaf).
        let dup = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1,
            vec![w_alice.clone(), w_alice.clone(), w_bob.clone()]);
        assert_rejected_for(&f.st, &dup, "duplicate reference");
        let w_owner = stage_welcome(&mut f, 1, &owner_pk, &owner_did);
        let self_too = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1,
            vec![w_alice.clone(), w_bob.clone(), w_owner]);
        assert_rejected_for(&f.st, &self_too, "cover exactly");

        // A wrong-generation ref ⇒ rejected (staging is per generation).
        let stale_gen = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 2,
            vec![w_alice.clone(), w_bob.clone()]);
        assert_rejected_for(&f.st, &stale_gen, "advance the generation");

        // Exact cover folds.
        let ok = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, vec![w_alice, w_bob]);
        f.st.apply(&ok).expect("an exact-cover reset folds");
        assert_eq!(f.st.mls_current_epoch(CH), Some((1, 1)), "the new generation starts at epoch 1");
        let g = f.st.mls_groups.get(&CH).unwrap();
        assert!(g.reset_pending);
        assert_eq!(g.leaves_confirmed, HashSet::from([(owner_pk.clone(), owner_did)]));
        assert_eq!(g.leaves_pending, HashSet::from([(alice_pk, alice_did), (bob_pk, bob_did)]));
        assert!(g.epoch_authenticator.is_none(), "the new generation's first commit is a bootstrap");
        assert!(g.commits_by_epoch.is_empty() && g.last_commit_epoch_by_author.is_empty());
        assert_eq!(g.events_since_last_commit, 0);
        assert_eq!(g.channel_events_since_reset, 0);
        assert!(g.reset_expected_tree_hash.is_none());
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
        let reset = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, vec![w_alice, w_bob]);
        f.st.apply(&reset).expect("the exact-cover reset folds");
        f.owner_prev = reset;

        // While the reset is pending the channel is DEAD, loudly — not a silent
        // partition (spec C7).
        let blocked = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 1, 1, vec![]);
        assert_rejected_for(&f.st, &blocked, "reset is incomplete");

        // The FIRST confirmation seeds the tree hash every later one must match
        // (the reset generation's add-commit is never a log event, ambiguity #7).
        let ca = leaf_confirm_gen(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 1, 1, TR, ALICE_STORE);
        f.st.apply(&ca).expect("alice's post-reset confirmation folds");
        f.alice_prev = ca;
        assert_eq!(f.st.mls_groups.get(&CH).unwrap().reset_expected_tree_hash, Some(TR));
        assert_rejected_for(&f.st, &blocked, "reset is incomplete");

        // A confirmation on a DIFFERENT tree is rejected: everyone must land on
        // the same tree or the reset never completes.
        let bad = leaf_confirm_gen(&bob_dev, &bob_pk, &f.sid, &bob_last, 1, 1, [78u8; 32], [4u8; 32]);
        assert_rejected_for(&f.st, &bad, "seeded tree hash");

        let cb = leaf_confirm_gen(&bob_dev, &bob_pk, &f.sid, &bob_last, 1, 1, TR, [4u8; 32]);
        f.st.apply(&cb).expect("bob's confirmation on the seeded tree folds");
        assert!(!f.st.mls_groups.get(&CH).unwrap().reset_pending, "the reset completes");
        let unlocked = sealed_post(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, CH, 1, 1, vec![]);
        f.st.apply(&unlocked).expect("sends unlock once every welcomed leaf confirms");
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
        let by_alice = group_reset(&f.alice_dev, &alice_pk, &f.sid, &f.alice_prev, 1, vec![w_alice.clone()]);
        assert_rejected_for(&f.st, &by_alice, "only the owner");

        // A channel's FIRST reset is always allowed.
        let r1 = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 1, vec![w_alice]);
        f.st.apply(&r1).expect("the owner's first reset folds");
        f.owner_prev = r1;

        // A second reset before RESET_MIN_CHANNEL_EVENTS further channel events
        // is rate-limited.
        let w2 = stage_welcome(&mut f, 2, &alice_pk, &alice_did);
        let r2 = group_reset(&f.owner_dev, &owner_pk, &f.sid, &f.owner_prev, 2, vec![w2]);
        let err = f.st.clone().apply(&r2).expect_err("a second reset must be rate-limited");
        assert!(err.to_string().contains("rate limit"), "unexpected rejection: {err}");

        f.st.mls_groups.get_mut(&CH).unwrap().channel_events_since_reset = RESET_MIN_CHANNEL_EVENTS;
        f.st.apply(&r2).expect("the rate limit clears after enough channel events");
        assert_eq!(f.st.mls_current_epoch(CH), Some((2, 1)));
    }
}
