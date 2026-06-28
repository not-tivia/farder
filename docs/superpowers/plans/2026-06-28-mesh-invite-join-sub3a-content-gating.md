# Mesh Invite/Join — Sub-project 3a: Log-Membership Content Gating — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** On a mesh server, make the event log authoritative for who may see content and who appears in the member list — so a connected identity that is not a log member (pending approval, or mid-join) gets no content and is hidden from the member list.

**Architecture:** `handle_request` already receives `state` (with `log_state`/`genesis`). Add a single central gate at its top: on a server that has a log (`log_state` is `Some`), a request that requires membership is rejected unless the caller is a log member; a small bootstrap allow-list (`SubmitEvent`, `ResolveInvite`, `GetServerInfo`) stays open so a joiner can complete their `MemberJoined`. The member list is filtered to log members. Legacy (non-log) servers are unaffected.

**Tech Stack:** Rust (`farder-server`, `farder-protocol`, `farder-crypto`). Tests via `cargo test -p farder-server`.

**Scope note (pragmatic, consistent with sub-project 2):** This does NOT rip out the legacy handshake member-registration or derive the member table from the log — it GATES by `log_state.is_member`. The legacy `members` table still provides display metadata; the log governs access. Making the table a pure derived view (and host-stamped invite expiry — the M1 item) stays a later hardening; it is not needed for the approval feature. Pending members are gated here; the approval *flow* (toggle, waiting screen, approval queue) is sub-project 3b, which builds on this gate.

## Global Constraints

- Gating applies ONLY when `state.log_state.lock().unwrap().as_ref()` is `Some` (a mesh server with a loaded log). Legacy servers (no genesis/log) keep current behavior exactly. (spec §Coexistence)
- The owner is always a log member (seeded at genesis) — the gate must never lock the owner out.
- Never hold the `log_state` lock across request dispatch — lock briefly, extract booleans, drop the lock, then gate. (The `SubmitEvent` handler locks `log_state` later; holding it would deadlock.)
- Default-DENY: every request requires membership EXCEPT an explicit allow-list (`SubmitEvent`, `ResolveInvite`, `GetServerInfo`). A new non-member-needed request must be added to the allow-list deliberately.

---

## File Structure

- `crates/farder-server/src/handlers.rs` — add `request_requires_membership(&ServerRequest) -> bool`; add the central gate at the top of `handle_request`; filter the `GetMembers` arm.

No new files. Tests live in the existing `#[cfg(test)] mod tests` in `handlers.rs`.

---

## Task 1: Central log-membership content gate

**Files:**
- Modify: `crates/farder-server/src/handlers.rs` — `handle_request` (signature at ~289; body starts ~297); add a free function `request_requires_membership`.
- Test: `handlers.rs` test module.

**Interfaces:**
- Consumes: `LogState::is_member(&PublicKey) -> bool`, `is_pending(&PublicKey) -> bool` (farder-crypto); `state.log_state: Mutex<Option<LogState>>`; the existing `err(&str) -> Result<HandleResult>` helper.
- Produces: `fn request_requires_membership(req: &ServerRequest) -> bool` (true = gated; false = allow-listed for non-members). The central gate in `handle_request`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `handlers.rs`. READ the existing handler tests first (e.g. `test_submit_event_non_member_rejected`, `test_handle_send_message`) to reuse their setup helpers — they already build a `conn`, a genesis + `log_state` with the owner as a member, and a `state`. Adapt the helper names to what actually exists; the BINDING assertions are: a non-member is rejected for a gated request, a member is allowed, and an allow-listed request is permitted for a non-member.

```rust
    #[test]
    fn non_member_is_gated_from_content_but_can_bootstrap() {
        // setup_mesh() should return (conn, state, owner_pk) where `state` has a
        // genesis + log_state seeding the owner as a member. Reuse whatever the
        // existing submit_event tests use to stand up a log_state.
        let (conn, state, owner) = setup_mesh();

        // A stranger who is NOT a log member.
        let stranger = farder_crypto::identity::Keypair::generate().public_key();

        // Gated request (FetchHistory) is rejected for the non-member...
        let r = handle_request(&conn, &stranger, false,
            ServerRequest::FetchHistory { channel_id: 1, before: None, limit: 50 },
            "", &state).unwrap();
        assert!(matches!(r.response, ServerResponse::Error { .. }), "non-member must be denied content");

        // ...but the owner (a log member) is allowed.
        let r2 = handle_request(&conn, &owner, true,
            ServerRequest::FetchHistory { channel_id: 1, before: None, limit: 50 },
            "", &state).unwrap();
        assert!(!matches!(r2.response, ServerResponse::Error { reason } if reason.contains("member")),
            "a log member must not be gated");

        // Allow-listed bootstrap request (GetServerInfo) is permitted for the non-member.
        let r3 = handle_request(&conn, &stranger, false, ServerRequest::GetServerInfo, "", &state).unwrap();
        assert!(!matches!(r3.response, ServerResponse::Error { reason } if reason.contains("member")),
            "bootstrap requests must stay open to non-members");
    }
```

NOTE to implementer: match `FetchHistory`'s real field names/variant shape from `farder-protocol` (the map shows `FetchHistory` at handlers.rs:429 — read the actual variant). If a `setup_mesh`-style helper doesn't exist, build the `log_state` inline the way the existing `test_submit_event_*` tests do (genesis → `LogState::from_genesis` → put it in the state's `log_state` Mutex).

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p farder-server non_member_is_gated_from_content_but_can_bootstrap`
Expected: FAIL — the non-member's `FetchHistory` currently succeeds (no gate).

- [ ] **Step 3: Add the allow-list classifier**

Add near the top of `handlers.rs` (a free function, outside the impl):

```rust
/// Whether a request requires the caller to be a LOG member on a mesh server.
/// Default-deny: everything is gated EXCEPT a small bootstrap allow-list that a
/// not-yet-member must be able to call — submit their join/device events, resolve
/// an invite code, and read server info. Adding to this list is a deliberate act.
fn request_requires_membership(req: &ServerRequest) -> bool {
    !matches!(
        req,
        ServerRequest::SubmitEvent { .. }
            | ServerRequest::ResolveInvite { .. }
            | ServerRequest::GetServerInfo
    )
}
```

- [ ] **Step 4: Add the central gate at the top of `handle_request`**

In `handle_request`, immediately after the parameters are in scope and before the `match request { .. }` dispatch, insert:

```rust
    // Mesh content gate: when this server has an event log, the log is
    // authoritative for membership. A caller who is not a log member may only
    // make bootstrap requests (submit join events, resolve an invite, read
    // server info); everything else is rejected. Legacy servers (no log) skip
    // this entirely. Lock briefly + drop before dispatch (SubmitEvent re-locks).
    let membership = {
        let guard = state.log_state.lock().unwrap();
        guard.as_ref().map(|ls| (ls.is_member(member), ls.is_pending(member)))
    };
    if let Some((is_log_member, is_pending)) = membership {
        if !is_log_member && request_requires_membership(&request) {
            return err(if is_pending {
                "pending approval: waiting for a moderator to approve your join"
            } else {
                "not a member of this server"
            });
        }
    }
```

(Match the real names: the param is `member: &PublicKey`; confirm the `err(...)` helper returns `Result<HandleResult>` as the surrounding arms use.)

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p farder-server non_member_is_gated_from_content_but_can_bootstrap`
Expected: PASS.

- [ ] **Step 6: Run the full server suite**

Run: `cargo test -p farder-server`
Expected: PASS. If existing tests that call gated requests as a non-member on a mesh-state break, they were relying on the un-gated behavior — adjust them to use a member caller (the gate is correct). Note any such test you change.

- [ ] **Step 7: Commit**

```bash
git add crates/farder-server/src/handlers.rs
git commit -m "feat(server): gate content behind log membership on mesh servers"
```

---

## Task 2: Filter the member list to log members

**Files:**
- Modify: `crates/farder-server/src/handlers.rs` — the `GetMembers` arm (~1001-1028).
- Test: `handlers.rs` test module.

**Interfaces:**
- Consumes: `members::list_members(conn) -> Vec<MemberRecord>` (each has `public_key: PublicKey`); `LogState::is_member`.
- Produces: a `GetMembers` response that, on a mesh server, excludes non-log-members (e.g. pending).

- [ ] **Step 1: Write the failing test**

Add to the `handlers.rs` test module. Set up a mesh server where the owner is a log member and a second identity is registered in the legacy `members` table but is NOT a log member (simulating a pending/not-yet-approved join). Reuse the inline log_state setup from Task 1.

```rust
    #[test]
    fn get_members_hides_non_log_members_on_mesh() {
        let (conn, state, owner) = setup_mesh();
        // A second identity registered in the legacy members table but NOT in the log.
        let pending = farder_crypto::identity::Keypair::generate().public_key();
        members::register_member(&conn, &pending, "vk_pending").unwrap();

        // GetMembers (called by the owner, a member) must include the owner and
        // EXCLUDE the non-log member.
        let r = handle_request(&conn, &owner, true, ServerRequest::GetMembers, "", &state).unwrap();
        let listed = match r.response {
            ServerResponse::Members { members } => members,
            other => panic!("expected Members, got {:?}", other),
        };
        let keys: Vec<_> = listed.iter().map(|m| m.public_key.clone()).collect();
        assert!(keys.contains(&owner), "owner (log member) shown");
        assert!(!keys.contains(&pending), "non-log member hidden from the member list");
    }
```

NOTE: match the real `ServerResponse::Members` shape and `MemberInfo.public_key` field from `farder-protocol`. If `register_member`'s signature differs, adapt.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p farder-server get_members_hides_non_log_members_on_mesh`
Expected: FAIL — `list_members` returns the pending identity too.

- [ ] **Step 3: Filter in the `GetMembers` arm**

In the `GetMembers` arm, after fetching records from `members::list_members(conn)` and BEFORE building the `MemberInfo` vec, retain only log members on mesh servers:

```rust
        let mut records = members::list_members(conn)?;
        // Mesh server: the log is authoritative — hide anyone not yet a log member
        // (e.g. a pending-approval join). Legacy servers keep the full list.
        {
            let guard = state.log_state.lock().unwrap();
            if let Some(ls) = guard.as_ref() {
                records.retain(|m| ls.is_member(&m.public_key));
            }
        }
```

(Adapt the binding name — if the arm currently does `for m in members::list_members(conn)? { .. }`, refactor to bind `let mut records = ...; records.retain(...);` then iterate `records`. Keep the rest of the `MemberInfo` construction identical.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p farder-server get_members_hides_non_log_members_on_mesh`
Expected: PASS.

- [ ] **Step 5: Run the full server suite + clippy**

Run: `cargo test -p farder-server && cargo clippy -p farder-server -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/farder-server/src/handlers.rs
git commit -m "feat(server): filter the member list to log members on mesh servers"
```

---

## Self-Review (completed by plan author)

**Spec coverage** (against `2026-06-27-mesh-invite-join-flow-design.md` §"Server-side content gating"):
- Pending members receive NO channel/message content (server-enforced, not client-cosmetic) → Task 1 central gate. ✓
- Pending members are hidden from the member list → Task 2. ✓
- Bootstrap (a not-yet-member completing their join) still works → allow-list (`SubmitEvent`/`ResolveInvite`/`GetServerInfo`) in Task 1. ✓
- Legacy servers unaffected → gate only fires when `log_state` is `Some`. ✓
- Owner never locked out → owner is a genesis log member. ✓
- DEFERRED (documented, not in 3a): full derived-view membership + dropping legacy registration; host-stamped invite expiry (M1); legacy/log invite unification (M2). These are later hardening, not required for the approval feature.
- Sub-project 3b (separate plan) owns: the `requires_approval` toggle, the joiner's "am I pending?" status query + waiting screen, the approval-queue endpoint + UI, and `MemberApproved`/deny emission + broadcast.

**Placeholder scan:** none — every code step is complete; the "adapt to real test helper/variant names" notes are test-scaffolding guidance with the binding assertions given explicitly (same pattern used successfully in sub-project 2).

**Type consistency:** `request_requires_membership(&ServerRequest) -> bool` defined in Task 1 and used by the same task's gate; `LogState::is_member`/`is_pending` used identically in both tasks; the `log_state` brief-lock-then-drop pattern is consistent across the gate (Task 1) and the `GetMembers` filter (Task 2).
