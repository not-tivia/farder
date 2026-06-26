# Mesh Rung 1 — Sub-project 2: Authorization Log State Machine — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the pure authorization state machine that folds a server's event log into "who is a member / banned / holds which capabilities / which devices and invites exist," validates every event against the per-payload signing rules, and is composable from any checkpoint — all in `farder-crypto`, no I/O.

**Architecture:** A new module `crates/farder-crypto/src/event_log_state.rs` with one type `LogState` and a single `apply(&mut self, event) -> Result<()>` that (1) runs envelope checks (server match, device resolution + signature, ban gate, per-`(author,device)` chain continuity), (2) checks per-payload authorization read-only, then (3) applies the effect. **Check-then-mutate**: all fallible checks run before any mutation, so a rejected event leaves the state untouched with no clone/rollback. Built on sub-project 1's `farder-crypto::event_log` types.

**Tech Stack:** Rust, `std::collections::{HashMap, HashSet}`, `anyhow`. Consumes `farder-crypto::event_log` (`Genesis`, `Event`, `EventPayload`, `DeviceCert`, `device_id`, the id aliases). No new dependencies.

## Global Constraints

- **Replayable / checkpoint-friendly:** `apply` is a pure `(state, event) -> Result<state'>` step (no I/O, no clock, no randomness); folding from genesis or from a mid-log checkpoint yields identical state. Verified by a test in Task 5.
- **Device binding (carry-forward M1 from sub-project 1's review):** the signing device's public key is ALWAYS derived from a verified `DeviceCert` bound to the event — for `DeviceAuthorized`, from the cert embedded in the payload (self-bootstrap); for every other event, from a recorded `DeviceRecord` whose `identity == event.core.author` and whose `device_id == event.core.device`. **Never** trust `Event::verify` with an attacker-chosen key.
- **Ban supersedes everything:** an event whose `author` is currently banned is rejected before any payload handling — so a banned identity cannot rejoin or act from any device (round-2 #3).
- **Owner is the root of authority** (named in the genesis): implicitly holds every capability; bootstrapped as the sole member at `from_genesis`.
- **Check-then-mutate:** no partial mutation on the error path.
- **Reuse sub-project 1 verbatim** — `event.verify(&pubkey)`, `event.hash()`, `cert.verify()`, `device_id(&pk)`; do not re-implement crypto.
- **Minimal scope:** richer roles, channel/category ACL overrides, timeouts, and device revocation are later sub-projects/follow-ons. Posting authorization here = "current non-banned member" (channel-scoped post permission is a follow-on). Document, don't build beyond this.

---

## File Structure

- **Create** `crates/farder-crypto/src/event_log_state.rs` — `LogState` + private records (`DeviceRecord`, `InviteRecord`, `ChainHead`), `from_genesis`, the query helpers, `apply` (envelope + per-payload authz + effects), `replay`, and tests.
- **Modify** `crates/farder-crypto/src/lib.rs` — add `pub mod event_log_state;`.

**Authority model (decisions for this sub-project, documented here as the source of truth):**
- Capabilities are `String` (`"invite"`, `"kick"`, `"ban"`). The owner holds all implicitly.
- `DeviceAuthorized` requires only a valid identity-signed cert matching the author/device — **no membership needed** (it registers an identity↔device link and grants nothing). This resolves the bootstrap chicken-and-egg: a new member's chain is `DeviceAuthorized` (seq 0) → `MemberJoined` (seq 1, gated by invite).
- `InviteCreated` requires the author hold `"invite"`. `MemberRemoved` is valid if author == member (leave) or author holds `"kick"`. `MemberBanned`/`MemberUnbanned` require `"ban"` (and you cannot ban the owner). `PermissionGranted{member, capability}` requires the author be owner OR already hold `capability` (you can only grant what you have). `MessagePosted` requires the author be a current non-banned member.

---

## Task 1: `LogState` + genesis init + query helpers

**Files:**
- Create: `crates/farder-crypto/src/event_log_state.rs`
- Modify: `crates/farder-crypto/src/lib.rs`

**Interfaces:**
- Consumes: `farder_crypto::event_log::{Genesis, ServerId, DeviceId, EventHash}`, `farder_crypto::identity::PublicKey`.
- Produces: `LogState` (opaque), `LogState::from_genesis(&Genesis) -> LogState`, `is_owner/is_member/is_banned(&PublicKey) -> bool`, `has_capability(&PublicKey, &str) -> bool`, `server_id() -> &ServerId`, `owner() -> &PublicKey`.

- [ ] **Step 1: Declare the module**

In `crates/farder-crypto/src/lib.rs`, add after `pub mod event_log;`:

```rust
pub mod event_log_state;
```

- [ ] **Step 2: Create the module with `LogState` + helpers + tests**

Create `crates/farder-crypto/src/event_log_state.rs`:

```rust
//! The authorization state machine: folds a server's signed event log into the
//! current membership / bans / capabilities / devices / invites, validating every
//! event against the per-payload signing rules. Pure (no I/O), so it replays
//! deterministically and composes from any checkpoint.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, ensure, Context, Result};

use crate::event_log::{device_id, DeviceId, Event, EventHash, EventPayload, Genesis, ServerId};
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
    banned: HashSet<PublicKey>,
    capabilities: HashMap<PublicKey, HashSet<String>>,
    devices: HashMap<DeviceId, DeviceRecord>,
    invites: HashMap<EventHash, InviteRecord>,
    chains: HashMap<(PublicKey, DeviceId), ChainHead>,
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
            banned: HashSet::new(),
            capabilities: HashMap::new(),
            devices: HashMap::new(),
            invites: HashMap::new(),
            chains: HashMap::new(),
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
    pub fn is_banned(&self, pk: &PublicKey) -> bool {
        self.banned.contains(pk)
    }
    /// The owner holds every capability; everyone else holds only what was granted.
    pub fn has_capability(&self, pk: &PublicKey, cap: &str) -> bool {
        self.is_owner(pk) || self.capabilities.get(pk).map_or(false, |c| c.contains(cap))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Keypair;

    fn genesis(owner: &Keypair) -> Genesis {
        Genesis {
            version: 1,
            name: "t".to_string(),
            owner: owner.public_key(),
            created_at: 1,
            nonce: [0u8; 16],
        }
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
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p farder-crypto event_log_state::tests::from_genesis_seeds_owner_as_member_with_all_authority`
Expected: PASS. (Pure data + queries; written complete.)

- [ ] **Step 4: Commit**

```bash
git add crates/farder-crypto/src/event_log_state.rs crates/farder-crypto/src/lib.rs
git commit -m "feat(crypto): mesh LogState skeleton + genesis init + authority queries"
```

---

## Task 2: `apply` envelope (device binding, signature, ban gate, chain) + `DeviceAuthorized`

**Files:**
- Modify: `crates/farder-crypto/src/event_log_state.rs`

**Interfaces:**
- Consumes: Task 1's `LogState`; `Event`, `EventPayload::DeviceAuthorized`, `DeviceCert` (via the payload), `device_id`, `event.verify`, `event.hash`.
- Produces: `LogState::apply(&mut self, event: &Event) -> Result<()>`; private `resolve_device_pubkey`, `check_chain`, `advance_chain`. After this task, `DeviceAuthorized` is fully handled; all other payloads pass authz permissively (filled in Tasks 3–4) so envelope behavior can be tested in isolation.

- [ ] **Step 1: Write the failing test**

Add to `event_log_state.rs` (inside `impl LogState`, after `has_capability`):

```rust
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
        match &event.core.payload {
            EventPayload::DeviceAuthorized { .. } => Ok(()),
            _ => Ok(()), // TEMP — replaced in Tasks 3 and 4
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
            _ => {} // TEMP — replaced in Tasks 3 and 4
        }
    }
```

Add these tests inside `mod tests` (and this helper at the top of `mod tests`):

```rust
    use crate::event_log::{AttachmentCap, DeviceCert, Event as Ev, EventPayload as EP};

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
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p farder-crypto event_log_state::tests::device_authorized_bootstraps_and_binds`
Expected: FAIL — `apply`/`resolve_device_pubkey`/etc. don't exist yet until you paste Step 1's code; once pasted, this and the envelope tests should pass. (If you prefer strict red: paste the tests first, watch them fail to compile, then add the impl from Step 1.)

- [ ] **Step 3: (Implementation is Step 1's code.)** Ensure Step 1's `impl LogState` additions are in place.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p farder-crypto event_log_state::tests`
Expected: PASS — `from_genesis_…`, `device_authorized_bootstraps_and_binds`, `envelope_rejections`, `banned_author_is_rejected_even_with_valid_signature`.
Then: `cargo test -p farder-crypto` — whole crate green.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-crypto/src/event_log_state.rs
git commit -m "feat(crypto): mesh LogState apply envelope (device binding, sig, ban gate, chain) + DeviceAuthorized"
```

---

## Task 3: Membership authz — `InviteCreated`, `MemberJoined`, `MessagePosted`

**Files:**
- Modify: `crates/farder-crypto/src/event_log_state.rs`

**Interfaces:**
- Consumes: Task 2's `apply`/`check_payload_authz`/`apply_payload_effect`.
- Produces: real authz + effects for `InviteCreated`, `MemberJoined`, `MessagePosted` (replacing their permissive arms).

- [ ] **Step 1: Write the failing test**

Add these tests inside `mod tests`:

```rust
    fn invite(dev: &Keypair, author: &PublicKey, sid: &str, prev: &Ev, seq_lamport: u64, max_uses: u32, expires_at: u64) -> Ev {
        Ev::next(dev, author.clone(), sid.to_string(), Some(prev), seq_lamport, 10,
            EP::InviteCreated { code_hash: "c".into(), max_uses, expires_at })
    }

    #[test]
    fn join_requires_a_valid_invite_and_blocks_self_join() {
        let owner = Keypair::generate();
        let owner_dev = Keypair::generate();
        let (mut st, da) = bootstrapped(&owner, &owner_dev);
        let sid = st.server_id().clone();

        // Owner creates an invite (owner holds "invite").
        let inv = invite(&owner_dev, &owner.public_key(), &sid, &da, 1, 5, 9999);
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
        let bad = invite(&m_dev, &mallory.public_key(), &sid, &m_da, 1, 5, 9999);
        assert!(st.clone().apply(&bad).is_err(), "no 'invite' capability → rejected");

        // Owner invite with max_uses = 1: a second join must fail.
        let inv = invite(&owner_dev, &owner.public_key(), &sid, &da, 1, 1, 9999);
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
```

- [ ] **Step 2: Run a test to verify it fails**

Run: `cargo test -p farder-crypto event_log_state::tests::only_members_can_post`
Expected: FAIL — `only_members_can_post` fails because the permissive `_ => Ok(())` arm currently lets the non-member's post through.

- [ ] **Step 3: Implement the authz + effects**

Replace `check_payload_authz`'s body with:

```rust
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
                let inv = self.invites.get(invite).context("join cites an unknown invite")?;
                ensure!(inv.use_count < inv.max_uses, "invite has no uses left");
                ensure!(event.core.timestamp <= inv.expires_at, "invite has expired");
                Ok(())
            }

            EventPayload::MessagePosted { .. } => {
                ensure!(self.is_member(author), "only members may post");
                Ok(())
            }

            _ => Ok(()), // remaining arms filled in Task 4
        }
    }
```

Replace `apply_payload_effect`'s body with:

```rust
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
            EventPayload::InviteCreated { max_uses, expires_at, .. } => {
                self.invites.insert(
                    event.hash(),
                    InviteRecord { max_uses: *max_uses, expires_at: *expires_at, use_count: 0 },
                );
            }
            EventPayload::MemberJoined { member, invite } => {
                self.members.insert(member.clone());
                if let Some(inv) = self.invites.get_mut(invite) {
                    inv.use_count += 1;
                }
            }
            EventPayload::MessagePosted { .. } => {} // no authz-state change
            _ => {} // remaining arms filled in Task 4
        }
    }
```

> Note on invite expiry: `event.core.timestamp` is the (untrusted) author clock; for single-host Rung 1 this is acceptable for expiry, and the test uses a far-future `expires_at`. A trusted-time source is a later concern.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p farder-crypto event_log_state::tests`
Expected: PASS — including `join_requires_a_valid_invite_and_blocks_self_join`, `invite_requires_authority_and_enforces_max_uses`, `only_members_can_post`.
Then `cargo test -p farder-crypto` — whole crate green.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-crypto/src/event_log_state.rs
git commit -m "feat(crypto): mesh authz — invites, member-join (invite-gated), member-only posting"
```

---

## Task 4: Moderation + permission authz — `MemberRemoved`, `MemberBanned`/`Unbanned`, `PermissionGranted`

**Files:**
- Modify: `crates/farder-crypto/src/event_log_state.rs`

**Interfaces:**
- Consumes: Task 3's authz/effect functions.
- Produces: real authz + effects for `MemberRemoved`, `MemberBanned`, `MemberUnbanned`, `PermissionGranted` (replacing the remaining `_ => Ok(())`/`_ => {}` arms).

- [ ] **Step 1: Write the failing test**

Add these tests inside `mod tests`. Helper to grant a capability from the owner:

```rust
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
            owner_prev.core.seq + 1, owner_prev.core.lamport + 1, 100,
            EP::InviteCreated { code_hash: "c".into(), max_uses: 10, expires_at: 9999 });
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
                owner_prev.core.seq + 1, owner_prev.core.lamport + 1, 100,
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
            victim_last.core.seq + 1, 50, EP::MemberBanned { member: owner.public_key() });
        assert!(st.clone().apply(&bad).is_err(), "no 'ban' capability → rejected");

        // Cannot ban the owner.
        let ban_owner = Ev::next(&mod_dev, mod_pk.clone(), sid.clone(), Some(&mod_last),
            mod_last.core.seq + 1, 51, EP::MemberBanned { member: owner.public_key() });
        assert!(st.clone().apply(&ban_owner).is_err(), "owner cannot be banned");

        // Mod bans the victim.
        let ban = Ev::next(&mod_dev, mod_pk.clone(), sid.clone(), Some(&mod_last),
            mod_last.core.seq + 1, 52, EP::MemberBanned { member: victim_pk.clone() });
        st.apply(&ban).expect("authorized ban succeeds");
        assert!(st.is_banned(&victim_pk));
        assert!(!st.is_member(&victim_pk));

        // The banned victim cannot act (e.g. post) from their device.
        let post = Ev::next(&victim_dev, victim_pk.clone(), sid.clone(), Some(&victim_last),
            victim_last.core.seq + 1, 53, EP::MessagePosted { channel_id: 1, content: "x".into(), reply_to: None, attachments: vec![] });
        assert!(st.clone().apply(&post).is_err(), "banned author cannot post");

        // The banned victim cannot rejoin (ban supersedes a fresh invite+join).
        let inv2 = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&owner_prev),
            owner_prev.core.seq + 1, 60, EP::InviteCreated { code_hash: "c2".into(), max_uses: 5, expires_at: 9999 });
        st.apply(&inv2).unwrap();
        let rejoin = Ev::next(&victim_dev, victim_pk.clone(), sid.clone(), Some(&victim_last),
            victim_last.core.seq + 1, 61, EP::MemberJoined { member: victim_pk.clone(), invite: inv2.hash() });
        assert!(st.clone().apply(&rejoin).is_err(), "banned identity cannot rejoin");

        // Unban (mod has 'ban') then the victim can rejoin.
        let unban = Ev::next(&mod_dev, mod_pk.clone(), sid.clone(), Some(&ban),
            ban.core.seq + 1, 62, EP::MemberUnbanned { member: victim_pk.clone() });
        st.apply(&unban).expect("authorized unban succeeds");
        assert!(!st.is_banned(&victim_pk));
        let rejoin2 = Ev::next(&victim_dev, victim_pk.clone(), sid.clone(), Some(&victim_last),
            victim_last.core.seq + 1, 63, EP::MemberJoined { member: victim_pk.clone(), invite: inv2.hash() });
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
            alice_last.core.seq + 1, 70, EP::MemberRemoved { member: alice_pk.clone() });
        st.apply(&leave).expect("self-leave succeeds");
        assert!(!st.is_member(&alice_pk));

        // Re-add alice, then a non-'kick' member tries to kick her → rejected.
        let (alice2, _ad2, _al2) = add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, None);
        let (bob, bob_dev, bob_last) = add_member_with_cap(&mut st, &owner, &owner_dev, &mut owner_prev, None);
        let kick = Ev::next(&bob_dev, bob.public_key(), sid.clone(), Some(&bob_last),
            bob_last.core.seq + 1, 80, EP::MemberRemoved { member: alice2.public_key() });
        assert!(st.clone().apply(&kick).is_err(), "kick without 'kick' capability is rejected");

        // Owner (root authority) can kick.
        let owner_kick = Ev::next(&owner_dev, owner.public_key(), sid.clone(), Some(&owner_prev),
            owner_prev.core.seq + 1, 81, EP::MemberRemoved { member: alice2.public_key() });
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
            alice_last.core.seq + 1, 90, EP::PermissionGranted { member: carol.public_key(), capability: "invite".into() });
        st.apply(&g_ok).expect("can grant a capability you hold");
        assert!(st.has_capability(&carol.public_key(), "invite"));

        // alice tries to grant "ban" — rejected (she doesn't hold it).
        let g_bad = Ev::next(&alice_dev, alice.public_key(), sid.clone(), Some(&g_ok),
            g_ok.core.seq + 1, 91, EP::PermissionGranted { member: carol.public_key(), capability: "ban".into() });
        assert!(st.clone().apply(&g_bad).is_err(), "cannot grant a capability you do not hold");
    }
```

- [ ] **Step 2: Run a test to verify it fails**

Run: `cargo test -p farder-crypto event_log_state::tests::grant_only_what_you_hold`
Expected: FAIL — the permissive `_ => Ok(())` currently lets `g_bad` through.

- [ ] **Step 3: Implement the remaining authz + effects**

In `check_payload_authz`, replace the trailing `_ => Ok(())` arm with:

```rust
            EventPayload::MemberRemoved { member } => {
                ensure!(self.is_member(member), "target is not a member");
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
```

In `apply_payload_effect`, replace the trailing `_ => {}` arm with:

```rust
            EventPayload::MemberRemoved { member } => {
                self.members.remove(member);
                self.capabilities.remove(member);
            }
            EventPayload::MemberBanned { member } => {
                self.banned.insert(member.clone());
                self.members.remove(member);
                self.capabilities.remove(member);
            }
            EventPayload::MemberUnbanned { member } => {
                self.banned.remove(member);
            }
            EventPayload::PermissionGranted { member, capability } => {
                self.capabilities.entry(member.clone()).or_default().insert(capability.clone());
            }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p farder-crypto event_log_state::tests`
Expected: PASS — including `ban_requires_authority_supersedes_rejoin_and_unban_restores_joinability`, `leave_vs_kick_authority`, `grant_only_what_you_hold`.
Then `cargo test -p farder-crypto`.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-crypto/src/event_log_state.rs
git commit -m "feat(crypto): mesh authz — remove/kick, ban/unban (supersedes rejoin), capability grants"
```

---

## Task 5: `replay` convenience + checkpoint-friendliness

**Files:**
- Modify: `crates/farder-crypto/src/event_log_state.rs`

**Interfaces:**
- Produces: `LogState::replay(genesis: &Genesis, events: &[Event]) -> Result<LogState>` — fold genesis + apply each event in order, short-circuiting on the first rejection.

- [ ] **Step 1: Write the failing test**

Add to `impl LogState` a STUBBED replay so the test compiles + fails:

```rust
    /// Fold a genesis + an ordered slice of events into the resulting state,
    /// rejecting on the first invalid event. Equivalent to `from_genesis` then
    /// `apply` in sequence.
    pub fn replay(genesis: &Genesis, events: &[Event]) -> Result<Self> {
        let _ = (genesis, events);
        bail!("not implemented") // STUB — replace in step 3
    }
```

Add this test inside `mod tests`:

```rust
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
            EP::InviteCreated { code_hash: "c".into(), max_uses: 5, expires_at: 9999 });
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
        assert_eq!(replayed.is_member(&alice.public_key()), true);
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
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p farder-crypto event_log_state::tests::replay_equals_stepwise_and_composes_from_a_checkpoint`
Expected: FAIL — stub bails "not implemented".

- [ ] **Step 3: Implement `replay`**

Replace the stubbed body with:

```rust
    pub fn replay(genesis: &Genesis, events: &[Event]) -> Result<Self> {
        let mut state = Self::from_genesis(genesis);
        for event in events {
            state.apply(event)?;
        }
        Ok(state)
    }
```

- [ ] **Step 4: Run the tests to verify they pass + the whole crate**

Run: `cargo test -p farder-crypto event_log_state::tests`
Expected: PASS (all event_log_state tests).
Run: `cargo test -p farder-crypto`
Expected: whole crate green, zero warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-crypto/src/event_log_state.rs
git commit -m "feat(crypto): mesh LogState::replay + checkpoint-composability test"
```

---

## Self-Review

**Spec coverage (sub-project 2 = "minimal authz log state machine"):**
- Per-payload signing/validation rules table → `check_payload_authz` covers all 8 variants (Tasks 2–4). ✅
- The fold deriving membership/permissions from the log → `LogState` + `apply` + `replay`. ✅
- M1 device binding (carry-forward) → `resolve_device_pubkey` derives the key only from a verified cert bound to author/device; `Event::verify` is never called with an event-supplied key. ✅
- Ban supersedes everything → ban gate before payload handling; banned author rejected; `MemberJoined` by a banned identity rejected (author == member, author banned → rejected). ✅
- Owner = root of authority → `from_genesis` seeds owner; `has_capability`/`is_owner` short-circuit. ✅
- Checkpoint-friendly / pure fold → `apply` is `(state, event) -> Result<()>` with no I/O; Task 5 proves composition from a mid-log checkpoint. ✅
- Check-then-mutate (no partial mutation) → all `ensure!`/`?` run before `apply_payload_effect`/`advance_chain`. ✅
- Out of scope (documented, not built): richer roles, channel ACLs, timeouts, device revocation, trusted time for expiry. ✅
- **Lamport monotonicity is deliberately NOT checked here** — it's an *ordering* concern, not *authorization*, and within-chain integrity is already guaranteed by strict `seq` + `prev` hash-linking. The "lamport strictly greater than the chain's previous" validation from the spec's message-flow belongs with the ordering/derived-view logic in sub-project 3, not the authz state machine. (Pre-empting the obvious reviewer question.) ✅

**Placeholder scan:** the only stubs are deliberate red-green starts (`check_payload_authz`'s permissive arm in Task 2; `replay` in Task 5), each replaced with shown code in the same task. No "TBD"/"handle edge cases". ✅

**Type consistency:** `LogState` field names, the private records (`DeviceRecord`/`InviteRecord`/`ChainHead`), and `apply`/`resolve_device_pubkey`/`check_chain`/`advance_chain`/`check_payload_authz`/`apply_payload_effect`/`replay` signatures are identical across tasks. Capability strings (`"invite"`/`"kick"`/`"ban"`) are used consistently between authz checks and the tests. Event/payload field access (`event.core.author`, `event.core.device`, `event.core.payload`, `event.hash()`, `cert.core.identity/device_id/device_pubkey`) matches sub-project 1's actual API. ✅

**Reviewer note (security focus):** the highest-risk surfaces are (a) `resolve_device_pubkey`'s M1 binding (a bug here = signature-bypass) and (b) the ban gate ordering (must precede payload handling). Both have dedicated negative tests; reviewers should additionally probe for any authz arm reachable without the ban gate, and any way to make `resolve_device_pubkey` return an attacker-chosen key.
