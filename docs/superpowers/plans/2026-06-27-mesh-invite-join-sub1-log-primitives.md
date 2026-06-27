# Mesh Invite/Join — Sub-project 1: Log Primitives — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the log-level primitives for approval-gated joins — an
`InviteCreated.requires_approval` flag, a `pending` membership state, a
`MemberApproved` event, and denial via `MemberRemoved` — to the pure authz state
machine, fully unit-tested with no runtime dependency.

**Architecture:** All changes are in `farder-crypto` (`event_log.rs` defines the
signed event payloads; `event_log_state.rs` is the pure `(state, event) -> Result<()>`
authz fold). A `MemberJoined` against an approval-required invite places the
joiner in a new `pending` set instead of `members`; a `MemberApproved` (signed by
a holder of the existing `"kick"` capability) promotes pending → member; a
`MemberRemoved` clears either members or pending (so it doubles as "deny").

**Tech Stack:** Rust, `anyhow`, `serde`/`rmp_serde`. Tests via `cargo test -p farder-crypto`.

## Global Constraints

- All work is in `crates/farder-crypto/` — pure logic, no I/O, no runtime. (spec §Decomposition #1)
- The capability that gates approval/denial is the existing `"kick"` string. (spec §Membership model)
- `requires_approval` is added with `#[serde(default)]` so already-serialized `InviteCreated` events stay valid (default `false` = instant). (spec §"Log additions" #1)
- An invite "use" is consumed on `MemberJoined` regardless of instant-vs-approval; denial does not refund. (spec §"Log additions" #3)
- `MessagePosted` authz is unchanged — pending members are simply not in `members`, so the existing `is_member` rule already blocks them. (spec §"Log additions" #3)
- Follow existing idioms: `ensure!`/`Context` from `anyhow`, check-then-mutate (authz in `check_payload_authz`, effects in `apply_payload_effect`, no partial mutation on error).

---

## File Structure

- `crates/farder-crypto/src/event_log.rs` — add `requires_approval` to the
  `InviteCreated` payload; add the `MemberApproved` payload variant.
- `crates/farder-crypto/src/event_log_state.rs` — add `requires_approval` to the
  internal `InviteRecord`; add a `pending: HashSet<PublicKey>` to `LogState` with
  `is_pending`/`pending_members` queries; branch `MemberJoined` on approval;
  add `MemberApproved` authz + effect; extend `MemberRemoved` to clear pending.

No new files. All tests live in the existing `#[cfg(test)] mod tests` in
`event_log_state.rs` (and a serde round-trip in `event_log.rs` if that file has a
test module; otherwise put it in `event_log_state.rs`).

---

## Task 1: `requires_approval` flag + `pending` state + approval-gated join

**Files:**
- Modify: `crates/farder-crypto/src/event_log.rs:115` (the `InviteCreated` variant)
- Modify: `crates/farder-crypto/src/event_log_state.rs` — `InviteRecord` (~21-26),
  `LogState` struct (~38-47), `from_genesis` (~51-64), query helpers (~75-84),
  `MemberJoined` authz arm (~194-201), `InviteCreated` effect arm (~252-257),
  `MemberJoined` effect arm (~258-263), test helper `invite` (~415-418) and its
  caller in `join_requires_a_valid_invite_and_blocks_self_join` (~428).

**Interfaces:**
- Consumes: existing `LogState` (`is_member`, `is_owner`, `has_capability`,
  `members`, `invites`, `InviteRecord { max_uses, expires_at, use_count }`),
  `EventPayload::{InviteCreated, MemberJoined}`.
- Produces (later tasks + sub-projects rely on these):
  - `EventPayload::InviteCreated { code_hash: String, max_uses: u32, expires_at: u64, requires_approval: bool }`
  - `LogState::is_pending(&self, pk: &PublicKey) -> bool`
  - `LogState::pending_members(&self) -> Vec<PublicKey>`
  - `pending: HashSet<PublicKey>` field on `LogState`.

- [ ] **Step 1: Add the schema field and scaffold the pending state (compile-green, no behavior change yet)**

In `crates/farder-crypto/src/event_log.rs`, change the `InviteCreated` variant (line 115):

```rust
    InviteCreated { code_hash: String, max_uses: u32, expires_at: u64, #[serde(default)] requires_approval: bool },
```

In `crates/farder-crypto/src/event_log_state.rs`, add `requires_approval` to `InviteRecord`:

```rust
/// An open invite, keyed in `LogState.invites` by its `InviteCreated` event hash.
#[derive(Clone, Debug)]
struct InviteRecord {
    max_uses: u32,
    expires_at: u64,
    use_count: u32,
    requires_approval: bool,
}
```

Add a `pending` field to `LogState` (after `members`):

```rust
    members: HashSet<PublicKey>,
    pending: HashSet<PublicKey>,
    banned: HashSet<PublicKey>,
```

Initialize it in `from_genesis` (alongside `banned: HashSet::new()`):

```rust
            members,
            pending: HashSet::new(),
            banned: HashSet::new(),
```

Add the query helpers next to `is_member` (after the `is_member` method):

```rust
    /// A member who joined via an approval-required invite and has not yet been approved.
    pub fn is_pending(&self, pk: &PublicKey) -> bool {
        self.pending.contains(pk)
    }
    /// All members currently awaiting approval (for the approval queue / content gating).
    pub fn pending_members(&self) -> Vec<PublicKey> {
        self.pending.iter().cloned().collect()
    }
```

Set `requires_approval` in the `InviteCreated` effect arm (replace lines ~252-257):

```rust
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
```

Update the test helper `invite` (replace the helper near line 415) to carry the flag, and update its one existing caller:

```rust
    fn invite(dev: &Keypair, author: &PublicKey, sid: &str, prev: &Ev, seq_lamport: u64, max_uses: u32, expires_at: u64, requires_approval: bool) -> Ev {
        Ev::next(dev, author.clone(), sid.to_string(), Some(prev), seq_lamport, 10,
            EP::InviteCreated { code_hash: "c".into(), max_uses, expires_at, requires_approval })
    }
```

In `join_requires_a_valid_invite_and_blocks_self_join`, update the call (around line 428) to pass `false`:

```rust
        let inv = invite(&owner_dev, &owner.public_key(), &sid, &da, 1, 5, 9999, false);
```

- [ ] **Step 2: Verify it compiles and all existing tests still pass**

Run: `cargo test -p farder-crypto`
Expected: PASS — all existing tests green (no behavior changed yet; the approval flag is plumbed but not acted on).

- [ ] **Step 3: Write the failing behavior test**

Add to the `tests` module in `crates/farder-crypto/src/event_log_state.rs`:

```rust
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
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `cargo test -p farder-crypto approval_invite_lands_joiner_in_pending_not_members`
Expected: FAIL — the joiner is inserted into `members` (the unchanged effect), so
`is_pending` is false and `!is_member` fails.

- [ ] **Step 5: Implement the approval branch in `MemberJoined`**

In `check_payload_authz`, replace the `MemberJoined` arm (lines ~194-201) to also reject a re-join while pending:

```rust
            EventPayload::MemberJoined { member, invite } => {
                ensure!(member == author, "MemberJoined must be self-authored");
                ensure!(!self.is_member(author), "already a member");
                ensure!(!self.is_pending(author), "already pending approval");
                let inv = self.invites.get(invite).context("join cites an unknown invite")?;
                ensure!(inv.use_count < inv.max_uses, "invite has no uses left");
                ensure!(event.core.timestamp <= inv.expires_at, "invite has expired");
                Ok(())
            }
```

In `apply_payload_effect`, replace the `MemberJoined` arm (lines ~258-263) to branch on the invite:

```rust
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
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p farder-crypto approval_invite_lands_joiner_in_pending_not_members`
Expected: PASS.

- [ ] **Step 7: Run the full crate suite + clippy**

Run: `cargo test -p farder-crypto && cargo clippy -p farder-crypto -- -D warnings`
Expected: PASS — all tests green, no clippy warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/farder-crypto/src/event_log.rs crates/farder-crypto/src/event_log_state.rs
git commit -m "feat(crypto): approval-required invites land joiners in a pending state"
```

---

## Task 2: `MemberApproved` event — promote pending → member

**Files:**
- Modify: `crates/farder-crypto/src/event_log.rs` (add the `MemberApproved` variant after `MemberJoined`, ~line 116)
- Modify: `crates/farder-crypto/src/event_log_state.rs` (`check_payload_authz` and `apply_payload_effect` get a `MemberApproved` arm; tests)

**Interfaces:**
- Consumes: `LogState` (`has_capability`, `is_pending`, `pending`, `members`),
  Task 1's `pending` state.
- Produces: `EventPayload::MemberApproved { member: PublicKey }` and its authz
  (signer holds `"kick"`, target is pending) + effect (pending → members).

- [ ] **Step 1: Add the `MemberApproved` payload variant**

In `crates/farder-crypto/src/event_log.rs`, add after the `MemberJoined` line (116):

```rust
    MemberApproved { member: PublicKey },
```

This makes the two `match` statements in `event_log_state.rs` non-exhaustive — they
will fail to compile until Step 3 adds the arms. That is expected.

- [ ] **Step 2: Write the failing test**

Add to the `tests` module in `event_log_state.rs`:

```rust
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
```

- [ ] **Step 3: Run the test to verify it fails (compile error: non-exhaustive match)**

Run: `cargo test -p farder-crypto member_approved_promotes_pending_and_requires_kick`
Expected: FAIL — compile error, the `match` arms for `MemberApproved` are missing.

- [ ] **Step 4: Implement authz + effect**

In `check_payload_authz`, add an arm (place it after the `MemberJoined` arm):

```rust
            EventPayload::MemberApproved { member } => {
                ensure!(self.has_capability(author, "kick"), "missing 'kick' capability");
                ensure!(self.is_pending(member), "target is not pending approval");
                Ok(())
            }
```

In `apply_payload_effect`, add an arm (place it after the `MemberJoined` arm):

```rust
            EventPayload::MemberApproved { member } => {
                self.pending.remove(member);
                self.members.insert(member.clone());
            }
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p farder-crypto member_approved_promotes_pending_and_requires_kick`
Expected: PASS.

- [ ] **Step 6: Run the full crate suite + clippy**

Run: `cargo test -p farder-crypto && cargo clippy -p farder-crypto -- -D warnings`
Expected: PASS — all green, no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/farder-crypto/src/event_log.rs crates/farder-crypto/src/event_log_state.rs
git commit -m "feat(crypto): MemberApproved event promotes a pending member (requires 'kick')"
```

---

## Task 3: `MemberRemoved` clears pending (denial) + ban supersedes approval

**Files:**
- Modify: `crates/farder-crypto/src/event_log_state.rs` (`MemberRemoved` authz arm ~208-215, effect arm ~265-268; tests)

**Interfaces:**
- Consumes: Task 1's `pending` state, existing `MemberRemoved` authz/effect, the
  existing ban gate (`is_banned` check that runs before payload handling in `apply`).
- Produces: `MemberRemoved` that accepts a pending target (denial) and clears it
  from `pending`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `event_log_state.rs`:

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p farder-crypto member_removed_denies_a_pending_request`
Expected: FAIL — `MemberRemoved` authz requires `is_member(member)`, but a pending
target is not a member, so `apply` returns an error and `st.apply(&deny)` panics on
`.expect(...)`. (`ban_supersedes_a_pending_join` may already pass — that's fine; it
locks in the ban-gate behavior as a regression guard.)

- [ ] **Step 3: Extend `MemberRemoved` authz and effect to cover pending**

In `check_payload_authz`, replace the `MemberRemoved` arm (lines ~208-215):

```rust
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
```

In `apply_payload_effect`, replace the `MemberRemoved` arm (lines ~265-268) to also clear pending:

```rust
            EventPayload::MemberRemoved { member } => {
                self.members.remove(member);
                self.pending.remove(member);
                self.capabilities.remove(member);
            }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p farder-crypto member_removed_denies_a_pending_request ban_supersedes_a_pending_join`
Expected: PASS.

- [ ] **Step 5: Run the full crate suite + clippy**

Run: `cargo test -p farder-crypto && cargo clippy -p farder-crypto -- -D warnings`
Expected: PASS — all green, no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/farder-crypto/src/event_log_state.rs
git commit -m "feat(crypto): MemberRemoved clears pending (deny a join request)"
```

---

## Self-Review (completed by plan author)

**Spec coverage** (against `2026-06-27-mesh-invite-join-flow-design.md` §"Log additions"):
- `requires_approval` field with serde default → Task 1, Step 1. ✓
- `MemberApproved` event, authz (`"kick"` + target pending), effect (pending→members) → Task 2. ✓
- `pending` state + `is_pending`/`pending_members` queries → Task 1. ✓
- `MemberJoined` branches instant→members / approval→pending; use consumed either way; can't re-join while pending → Task 1, Step 5. ✓
- `MessagePosted` unchanged; pending can't post → asserted in Task 1 test. ✓
- `MemberRemoved` extended to clear pending (denial) → Task 3. ✓
- Ban supersedes pending → Task 3, `ban_supersedes_a_pending_join`. ✓
- Out of scope here (sub-projects 2–3): server ingest, client UX, content gating, code↔invite resolution. Correctly excluded.

**Placeholder scan:** none — every code step shows complete code and exact commands.

**Type consistency:** `requires_approval: bool` is consistent across the payload,
`InviteRecord`, and the `invite` test helper; `MemberApproved { member: PublicKey }`
matches in payload, authz, effect, and tests; `is_pending`/`pending_members`
signatures match between definition (Task 1) and use (Tasks 2–3).
