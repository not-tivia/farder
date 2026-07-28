//! The authorization state machine: folds a server's signed event log into the
//! current membership / bans / capabilities / devices / invites, validating every
//! event against the per-payload signing rules. Pure (no I/O), so it replays
//! deterministically and composes from any checkpoint.

use std::collections::{HashMap, HashSet};

use anyhow::{ensure, Context, Result};

use crate::event_log::{DeviceId, Event, EventHash, EventPayload, Genesis, ServerId};
use crate::identity::PublicKey;

/// A device authorized within this server's log (identity ↔ signing subkey).
#[derive(Clone, Debug)]
struct DeviceRecord {
    identity: PublicKey,
    device_pubkey: PublicKey,
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
        // Ban gate: a banned identity cannot act from any device.
        ensure!(!self.is_banned(&event.core.author), "author is banned");
        // Per-(author, device) chain continuity.
        self.check_chain(event)?;

        // --- Payload authorization (read-only) ---
        self.check_payload_authz(event)?;

        // --- Effects (only reached once every check passed) ---
        self.apply_payload_effect(event, &device_pubkey);
        self.advance_chain(event);
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

    /// Per-payload authorization (read-only). Tasks 3–4 fill the membership /
    /// moderation / message arms; this task handles only `DeviceAuthorized` (whose
    /// cert checks already happened in `resolve_device_pubkey`) and permits the
    /// rest so the envelope can be tested in isolation.
    fn check_payload_authz(&self, event: &Event) -> Result<()> {
        let author = &event.core.author;
        match &event.core.payload {
            EventPayload::DeviceAuthorized { .. } => Ok(()),

            EventPayload::InviteCreated { .. } => {
                ensure!(self.has_capability(author, "invite"), "missing 'invite' capability");
                Ok(())
            }

            EventPayload::MemberJoined { member, invite } => {
                ensure!(member == author, "MemberJoined must be self-authored");
                ensure!(!self.is_member(author), "already a member");
                ensure!(!self.is_pending(author), "already pending approval");
                let inv = self.invites.get(invite).context("join cites an unknown invite")?;
                ensure!(inv.use_count < inv.max_uses, "invite has no uses left");
                ensure!(event.core.timestamp <= inv.expires_at, "invite has expired");
                Ok(())
            }

            EventPayload::MemberApproved { member } => {
                ensure!(self.has_capability(author, "kick"), "missing 'kick' capability");
                ensure!(self.is_pending(member), "target is not pending approval");
                Ok(())
            }

            EventPayload::MessagePosted { .. } => {
                ensure!(self.is_member(author), "only members may post");
                Ok(())
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
                Ok(())
            }

            EventPayload::MemberBanned { member } => {
                ensure!(self.has_capability(author, "ban"), "missing 'ban' capability");
                ensure!(!self.is_owner(member), "the owner cannot be banned");
                Ok(())
            }

            EventPayload::MemberUnbanned { .. } => {
                ensure!(self.has_capability(author, "ban"), "missing 'ban' capability");
                Ok(())
            }

            EventPayload::PermissionGranted { member, capability } => {
                ensure!(self.is_member(member), "grantee is not a member");
                ensure!(
                    self.is_owner(author) || self.has_capability(author, capability),
                    "cannot grant a capability you do not hold"
                );
                Ok(())
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
                Ok(())
            }

            // TEMP (fail closed, Task 1): the Rung-2 MLS/E2EE variants are pure
            // schema so far — their fold rules land in Tasks 2–4. Explicitly
            // listed (no `_`) so adding a variant without a rule cannot be
            // silently permissive.
            EventPayload::ChannelCreated { .. }
            | EventPayload::MlsKeyPackagePublished { .. }
            | EventPayload::MlsCommit { .. }
            | EventPayload::MlsWelcome { .. }
            | EventPayload::MlsLeafConfirmed { .. }
            | EventPayload::MlsGroupReset { .. }
            | EventPayload::MessagePostedE2ee { .. }
            | EventPayload::MessageEditedE2ee { .. }
            | EventPayload::MessageDeleted { .. }
            | EventPayload::DeviceRevoked { .. } => {
                anyhow::bail!("MLS/E2EE variants are folded in Tasks 2-4")
            }
        }
    }

    /// Apply the state effect of an authorized event. Infallible: authorization
    /// already passed. Tasks 3–4 fill the remaining arms.
    fn apply_payload_effect(&mut self, event: &Event, device_pubkey: &PublicKey) {
        match &event.core.payload {
            EventPayload::DeviceAuthorized { .. } => {
                self.devices.insert(
                    event.core.device.clone(),
                    DeviceRecord {
                        identity: event.core.author.clone(),
                        device_pubkey: device_pubkey.clone(),
                    },
                );
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

            // TEMP (Task 1): unreachable — check_payload_authz rejects every
            // Rung-2 MLS/E2EE variant until their fold rules land in Tasks 2–4.
            EventPayload::ChannelCreated { .. }
            | EventPayload::MlsKeyPackagePublished { .. }
            | EventPayload::MlsCommit { .. }
            | EventPayload::MlsWelcome { .. }
            | EventPayload::MlsLeafConfirmed { .. }
            | EventPayload::MlsGroupReset { .. }
            | EventPayload::MessagePostedE2ee { .. }
            | EventPayload::MessageEditedE2ee { .. }
            | EventPayload::MessageDeleted { .. }
            | EventPayload::DeviceRevoked { .. } => {
                unreachable!("rung-2 variants are rejected by check_payload_authz until Tasks 2-4")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Keypair;
    use crate::event_log::{device_id, AttachmentCap, DeviceCert, Event as Ev, EventPayload as EP};

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
}
