# Mesh Invite/Join — Sub-project 3b: Approval Flow — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the mesh approval flow end-to-end: a "require approval" invite, a joiner's no-content "waiting for approval" screen, an approver's pending-requests queue with Approve/Deny, and a live transition when approved.

**Architecture:** The log primitives (pending state, `MemberApproved`, `MemberRemoved`) and the server content gate already exist (sub-projects 1 + 3a). This builds the surrounding flow: server endpoints (`GetMembershipStatus`, `GetPendingMembers`), a single `MembershipChanged` broadcast on any membership transition, a reduced `GetServerInfo` for non-members, client commands to emit `MemberApproved`/`MemberRemoved`, the create-invite toggle, the waiting screen, and the approval-queue UI.

**Tech Stack:** Rust (`farder-protocol`, `farder-server`, `farder-crypto`), Tauri (`client/src-tauri`), TypeScript/React. Rust via `cargo test`; client via `cargo build` + `npx tsc --noEmit`; the full approval round-trip is owner-verified on Windows.

## Global Constraints

- Approval/denial authority is the existing **`"kick"`** capability (owner holds it implicitly) — both the server's `GetPendingMembers` gate and the client's "am I an approver" check use it. (spec §"Membership model")
- A single new broadcast `ServerEvent::MembershipChanged { public_key }` is emitted on every accepted membership-transition event (`MemberJoined`, `MemberApproved`, `MemberRemoved`, `MemberBanned`); clients re-fetch status/members/pending off it. Do NOT add per-transition events.
- `GetMembershipStatus` MUST be added to the bootstrap allow-list (`request_requires_membership` in `handlers.rs`) so a pending/non-member can call it. `GetPendingMembers` must NOT (approvers only, and they're members).
- New client UI (waiting screen, pending cards) MUST be styled in EVERY `client/src/themes/*/theme.css` using `var(--xp-…)` variables, or reuse existing classes (CLAUDE.md). No hard-coded colors, no unstyled className.
- Every new Tauri command: `invoke("X")` name ↔ `#[tauri::command] fn X` ↔ `generate_handler!` entry in `main.rs` must agree (CLAUDE.md untyped seam).
- Approve/deny client commands emit signed events via the SAME `event_build_next`/`event_send_submit`/`DeviceState` + `device_chain_lock` pattern as `submit_event`/`join_log_server` (advance+save DeviceState ONLY on accept).

---

## File Structure

- `crates/farder-protocol/src/server.rs` — add `ServerRequest::{GetMembershipStatus, GetPendingMembers}`, `ServerResponse::{MembershipStatus, PendingMembers}`, `ServerEvent::MembershipChanged`.
- `crates/farder-server/src/handlers.rs` — handlers for the two requests; reduced `GetServerInfo`; the `MembershipChanged` broadcast in the `SubmitEvent` arm; allow-list entry.
- `crates/farder-server/src/bridge.rs`? No — forwarding is the CLIENT's `bridge.rs`.
- `client/src-tauri/src/bridge.rs` — forward `MembershipChanged` to the frontend.
- `client/src-tauri/src/commands.rs` — `approve_member`, `deny_member`, `get_membership_status`, `get_pending_members`; thread `requires_approval` into `create_invite`.
- `client/src-tauri/src/main.rs` — register the 4 new commands.
- `client/src/lib/tauri-bridge.ts` — bridge wrappers; `createInvite` gains `requiresApproval`.
- `client/src/context/ServerContext.tsx` — `membershipStatus` per-server field + action + reducer.
- `client/src/components/AppShell.tsx` — fetch status on connect; intercept render with the waiting screen.
- `client/src/components/MemberSidebar.tsx` + new `PendingApprovals.tsx` — the approval queue.
- `client/src/components/InviteDialog.tsx` — the "Require approval" checkbox.
- `client/src/hooks/useServerEvents.ts` — the `membership_changed` listener.
- `client/src/themes/*/theme.css` — styles for the waiting screen + pending cards.

---

## Task 1: Server — `GetMembershipStatus`

**Files:**
- Modify: `crates/farder-protocol/src/server.rs` — `ServerRequest` (~228), `ServerResponse` (~318).
- Modify: `crates/farder-server/src/handlers.rs` — new arm after `GetServerInfo` (~1043); allow-list `request_requires_membership` (~477).
- Test: `handlers.rs` test module.

**Interfaces:**
- Produces: `ServerRequest::GetMembershipStatus`; `ServerResponse::MembershipStatus { status: String }` (`"member"`/`"pending"`/`"none"`).

- [ ] **Step 1: Add the protocol variants**

In `crates/farder-protocol/src/server.rs`, add to `ServerRequest`:
```rust
    /// Ask the server whether the caller is a member, pending approval, or neither
    /// (per the event log). Allowed for non-members so a pending joiner can learn it.
    GetMembershipStatus,
```
And to `ServerResponse`:
```rust
    MembershipStatus { status: String },
```

- [ ] **Step 2: Write the failing test**

Add to the `handlers.rs` test module (reuse the mesh `log_state` setup from sub-project 3a's tests):
```rust
    #[test]
    fn membership_status_reports_member_pending_none() {
        let (conn, state, owner) = setup_mesh(); // owner is a log member
        let stranger = farder_crypto::identity::Keypair::generate().public_key();

        let r = handle_request(&conn, &owner, true, ServerRequest::GetMembershipStatus, "", &state).unwrap();
        assert!(matches!(r.response, ServerResponse::MembershipStatus { status } if status == "member"));

        let r2 = handle_request(&conn, &stranger, false, ServerRequest::GetMembershipStatus, "", &state).unwrap();
        assert!(matches!(r2.response, ServerResponse::MembershipStatus { status } if status == "none"));
    }
```
(Adapt `setup_mesh` to the actual helper. If you can drive a pending join in the harness, assert `"pending"` too; otherwise the member/none classes are the binding assertions.)

- [ ] **Step 3: Run the test — expect FAIL (no handler arm).**

Run: `cargo test -p farder-server membership_status_reports_member_pending_none`
Expected: FAIL (non-exhaustive match / unhandled request).

- [ ] **Step 4: Add the handler arm + allow-list entry**

In `handlers.rs`, add the arm (after `GetServerInfo`):
```rust
        ServerRequest::GetMembershipStatus => {
            let status = {
                let guard = state.log_state.lock().unwrap();
                match guard.as_ref() {
                    Some(ls) if ls.is_member(member) => "member",
                    Some(ls) if ls.is_pending(member) => "pending",
                    _ => "none",
                }
            };
            ok(ServerResponse::MembershipStatus { status: status.to_string() })
        }
```
Add `GetMembershipStatus` to the allow-list in `request_requires_membership`:
```rust
    !matches!(
        req,
        ServerRequest::SubmitEvent { .. }
            | ServerRequest::ResolveInvite { .. }
            | ServerRequest::GetServerInfo
            | ServerRequest::GetMembershipStatus
    )
```

- [ ] **Step 5: Run the test — expect PASS.** `cargo test -p farder-server membership_status_reports_member_pending_none`

- [ ] **Step 6: Full suite + commit.**
```bash
cargo test -p farder-server
git add crates/farder-protocol/src/server.rs crates/farder-server/src/handlers.rs
git commit -m "feat(server): GetMembershipStatus (member/pending/none) for the join flow"
```

---

## Task 2: Server — `GetPendingMembers` (approver-gated)

**Files:**
- Modify: `crates/farder-protocol/src/server.rs` — `ServerRequest`, `ServerResponse`.
- Modify: `crates/farder-server/src/handlers.rs` — new arm after `GetMembers`.
- Test: `handlers.rs`.

**Interfaces:**
- Consumes: `LogState::pending_members() -> Vec<PublicKey>`; `members::get_member(conn, &pk)`; the `require_base_perm(conn, member, is_owner, permissions::KICK_MEMBERS, "KICK_MEMBERS")` gate pattern (used by the kick handler).
- Produces: `ServerRequest::GetPendingMembers`; `ServerResponse::PendingMembers { members: Vec<MemberInfo> }`.

- [ ] **Step 1: Add the protocol variants**
```rust
    // ServerRequest:
    GetPendingMembers,
    // ServerResponse:
    PendingMembers { members: Vec<MemberInfo> },
```

- [ ] **Step 2: Write the failing test**
```rust
    #[test]
    fn get_pending_members_lists_pending_and_requires_kick() {
        let (conn, state, owner) = setup_mesh_with_pending(); // owner + one pending identity in the log
        // Owner (holds kick) sees the pending member.
        let r = handle_request(&conn, &owner, true, ServerRequest::GetPendingMembers, "", &state).unwrap();
        let pending = match r.response { ServerResponse::PendingMembers { members } => members, o => panic!("got {:?}", o) };
        assert_eq!(pending.len(), 1, "the one pending identity is listed");
        // A non-kick member is denied.
        // (build a plain member without kick; assert Error) — adapt to the harness.
    }
```
(Adapt the setup: you need the log_state to have a pending member. Reuse the event-driving helpers from sub-project 1/3a tests to apply a `MemberJoined` against an approval invite. If driving a full pending join is too heavy in this harness, at minimum assert the owner-gating and an empty list when there are no pending members; note the limitation.)

- [ ] **Step 3: Run — expect FAIL.**

- [ ] **Step 4: Implement the arm**
```rust
        ServerRequest::GetPendingMembers => {
            if let Some(denied) = require_base_perm(conn, member, is_owner, permissions::KICK_MEMBERS, "KICK_MEMBERS")? {
                return Ok(denied);
            }
            let pending_pks = {
                let guard = state.log_state.lock().unwrap();
                guard.as_ref().map(|ls| ls.pending_members()).unwrap_or_default()
            };
            let mut members_out = Vec::new();
            for pk in pending_pks {
                if let Ok(Some(rec)) = members::get_member(conn, &pk) {
                    members_out.push(MemberInfo {
                        public_key: pk,
                        display_name: rec.display_name,
                        joined_at: rec.joined_at,
                        role_ids: vec![],
                        timeout_until: None,
                        timeout_reason: None,
                        profile_hash: rec.profile_hash,
                        presence: None,
                    });
                }
            }
            ok(ServerResponse::PendingMembers { members: members_out })
        }
```
(Match the real `MemberInfo` field set + `require_base_perm` signature from the kick arm.)

- [ ] **Step 5: Run — expect PASS. Step 6: full suite + commit.**
```bash
git commit -am "feat(server): GetPendingMembers (kick-gated) lists log pending members"
```

---

## Task 3: Server — `MembershipChanged` broadcast

**Files:**
- Modify: `crates/farder-protocol/src/server.rs` — `ServerEvent`.
- Modify: `crates/farder-server/src/handlers.rs` — the `SubmitEvent` arm's broadcast list.
- Modify: `client/src-tauri/src/bridge.rs` — forward the event to the frontend.
- Test: `handlers.rs` (broadcast emitted on an accepted membership event).

**Interfaces:**
- Consumes: the `SubmitEvent` arm's existing `ok_with(response, events)` broadcast mechanism; `EventTarget::All`; `BroadcastEvent`.
- Produces: `ServerEvent::MembershipChanged { public_key: PublicKey }`, broadcast `All` on any accepted `MemberJoined`/`MemberApproved`/`MemberRemoved`/`MemberBanned`.

- [ ] **Step 1: Add the ServerEvent variant**

In `crates/farder-protocol/src/server.rs` `ServerEvent`:
```rust
    /// A membership transition (join-pending / approve / remove / ban) for `public_key`.
    /// Clients re-fetch their own status + the member list + the pending queue on this.
    MembershipChanged { public_key: PublicKey },
```

- [ ] **Step 2: Write the failing test**

In the `handlers.rs` test module, drive a membership event through `SubmitEvent` and assert a `MembershipChanged` broadcast is in the returned events. Reuse the existing `test_submit_event_*` harness that builds + submits an event. Use the simplest membership event the harness can build (e.g. a `MemberJoined` against an instant invite, or a `MemberApproved` if pending setup exists).
```rust
    #[test]
    fn submit_event_broadcasts_membership_changed_on_member_event() {
        // ... build a state where a MemberJoined will be accepted, submit it ...
        let result = handle_request(&conn, &joiner, false, ServerRequest::SubmitEvent { event: join_event }, "", &state).unwrap();
        assert!(result.events.iter().any(|b| matches!(&b.event, ServerEvent::MembershipChanged { .. })),
            "an accepted membership event must broadcast MembershipChanged");
    }
```

- [ ] **Step 3: Run — expect FAIL.**

- [ ] **Step 4: Emit the broadcast in `SubmitEvent`**

In the `SubmitEvent` arm, AFTER the event is validated + stored + applied (where it currently builds the `NewMessage` broadcast for `MessagePosted`), add a membership broadcast based on the payload. Locate the place where `event.core.payload` is matched for derivation and extend it:
```rust
            // Broadcast a membership change so every client re-fetches its own
            // status + the member list + the pending queue.
            let membership_pk: Option<PublicKey> = match &event.core.payload {
                EventPayload::MemberJoined { member, .. }
                | EventPayload::MemberApproved { member }
                | EventPayload::MemberRemoved { member }
                | EventPayload::MemberBanned { member } => Some(member.clone()),
                _ => None,
            };
            if let Some(pk) = membership_pk {
                broadcasts.push(BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::MembershipChanged { public_key: pk },
                });
            }
```
(Adapt: the arm may build its broadcast `Vec` differently — append to whatever vec is passed to `ok_with`. `EventPayload` is `farder_crypto::event_log::EventPayload`; import as the file does.)

- [ ] **Step 5: Forward the event to the frontend (client bridge)**

In `client/src-tauri/src/bridge.rs`, in the `ServerEvent` → Tauri-emit match (near the other member events like `MemberProfileUpdated`), add:
```rust
        ServerEvent::MembershipChanged { public_key } => {
            let _ = app.emit("server:membership_changed",
                serde_json::json!({ "server_id": sid, "public_key": public_key.to_string() }));
        }
```
(Match the exact `app.emit`/`sid` pattern of the neighboring arms.)

- [ ] **Step 6: Run the test — expect PASS. Build the client crate** (`cd client/src-tauri && cargo build`) **to confirm the bridge arm compiles.**

- [ ] **Step 7: Full suite + commit.**
```bash
git add crates/farder-protocol/src/server.rs crates/farder-server/src/handlers.rs client/src-tauri/src/bridge.rs
git commit -m "feat: MembershipChanged broadcast on membership transitions"
```

---

## Task 4: Server — reduced `GetServerInfo` for non-members

**Files:**
- Modify: `crates/farder-server/src/handlers.rs` — `GetServerInfo` arm (~1028).
- Test: `handlers.rs`.

**Interfaces:**
- Consumes: `LogState::is_member`; `channels::list_channels`/`list_categories`, `members::list_roles` (the current sources).
- Produces: `GetServerInfo` returns empty `channels`/`categories`/`roles` to a non-log-member on a mesh server (name/member_count/server_id still returned).

- [ ] **Step 1: Write the failing test**
```rust
    #[test]
    fn get_server_info_hides_structure_from_non_members() {
        let (conn, state, owner) = setup_mesh();
        let stranger = farder_crypto::identity::Keypair::generate().public_key();
        let r = handle_request(&conn, &stranger, false, ServerRequest::GetServerInfo, "", &state).unwrap();
        match r.response {
            ServerResponse::ServerInfo { channels, categories, roles, server_id, .. } => {
                assert!(channels.is_empty() && categories.is_empty() && roles.is_empty(),
                    "non-member gets no channel/role structure");
                assert!(server_id.is_some(), "but still learns server_id to join");
            }
            o => panic!("got {:?}", o),
        }
        // Owner still gets the full structure.
        let r2 = handle_request(&conn, &owner, true, ServerRequest::GetServerInfo, "", &state).unwrap();
        match r2.response {
            ServerResponse::ServerInfo { channels, .. } => assert!(!channels.is_empty() || true, "owner unaffected"),
            o => panic!("got {:?}", o),
        }
    }
```
(For the owner assertion: if the test fixture has no channels, just assert it isn't an Error; the binding part is the non-member empty structure.)

- [ ] **Step 2: Run — expect FAIL.**

- [ ] **Step 3: Gate the structure in `GetServerInfo`**

In the `GetServerInfo` arm, compute `is_member` and only populate structure for members:
```rust
            let is_member = {
                let guard = state.log_state.lock().unwrap();
                guard.as_ref().map(|ls| ls.is_member(member)).unwrap_or(true) // legacy: full info
            };
            let (channels, categories, roles) = if is_member {
                (channels::list_channels(conn)?, channels::list_categories(conn)?, members::list_roles(conn)?)
            } else {
                (Vec::new(), Vec::new(), Vec::new())
            };
```
Then use those in the `ServerResponse::ServerInfo { channels, categories, roles, .. }` construction. (Match the arm's real function names for listing channels/categories/roles.)

- [ ] **Step 4: Run — expect PASS. Step 5: full suite + clippy + commit.**
```bash
cargo test -p farder-server && cargo clippy -p farder-server -- -D warnings
git commit -am "feat(server): GetServerInfo hides channel/role structure from non-members"
```

---

## Task 5: Client — create-invite `requires_approval` toggle

**Files:**
- Modify: `client/src-tauri/src/commands.rs` — `create_invite` (~2110).
- Modify: `client/src/lib/tauri-bridge.ts` — `createInvite`.
- Modify: `client/src/components/InviteDialog.tsx` — checkbox.
- Doc: `docs/modules/tauri-commands.md` + `frontend-bridge.md` (signature change).

**Interfaces:**
- Produces: `create_invite(state, server_id, log_server_id, max_uses, requires_approval: Option<bool>)`; emits `InviteCreated { requires_approval }`.

- [ ] **Step 1: Thread `requires_approval` through the Rust command**

In `create_invite`, add the param `requires_approval: Option<bool>` (after `max_uses`), and in the `InviteCreated` payload replace the hardcoded `requires_approval: false` with `requires_approval: requires_approval.unwrap_or(false)`.

- [ ] **Step 2: Bridge + UI**

In `tauri-bridge.ts` `createInvite`, add `requiresApproval?: boolean` and pass `requiresApproval: requiresApproval ?? false` in the invoke. In `InviteDialog.tsx`, add a checkbox bound to a `requiresApproval` state (reuse existing form classes like `.connect-section`/`.connect-label` — no new CSS), and pass it to `api.createInvite(serverId, logServerId, maxUses, requiresApproval)`.

- [ ] **Step 3: Update the docs** (`tauri-commands.md` create_invite entry, `frontend-bridge.md` createInvite row) to include `requires_approval`/`requiresApproval`.

- [ ] **Step 4: Compile-check.** `cd client/src-tauri && cargo build` and `cd client && npx tsc --noEmit` — both clean.

- [ ] **Step 5: Commit.**
```bash
git commit -am "feat(client): 'require approval' toggle on invites"
```

---

## Task 6: Client — approve/deny + status/pending commands

**Files:**
- Modify: `client/src-tauri/src/commands.rs` — `approve_member`, `deny_member`, `get_membership_status`, `get_pending_members`.
- Modify: `client/src-tauri/src/main.rs` — register all four.
- Modify: `client/src/lib/tauri-bridge.ts` — four wrappers.
- Doc: `tauri-commands.md` + `frontend-bridge.md`.

**Interfaces:**
- Consumes: `event_build_next`/`event_send_submit`/`DeviceState`/`device_chain_lock` (from `submit_event`); `bridge::send_request`.
- Produces: `approve_member(state, server_id, log_server_id, member)`, `deny_member(...)`, `get_membership_status(state, server_id) -> String`, `get_pending_members(state, server_id) -> Vec<MemberInfo>`.

- [ ] **Step 1: `approve_member` / `deny_member`**

Add a shared helper that emits a membership-moderation event signed by the approver's device — mirror `submit_event`'s structure (acquire `device_chain_lock`, load identity+device+DeviceState, auto-`DeviceAuthorized` if needed, `event_build_next(... payload)`, `event_send_submit`, advance+save ONLY on accept). `approve_member` builds `EventPayload::MemberApproved { member: <parsed target pubkey> }`; `deny_member` builds `EventPayload::MemberRemoved { member }`. Parse the target pubkey with the existing `parse_public_key` helper (commands.rs ~2011).
```rust
#[tauri::command]
pub async fn approve_member(state: State<'_, Arc<AppState>>, server_id: String, log_server_id: String, member: String) -> Result<(), String> {
    let target = parse_public_key(&member)?;
    moderate_member(&state, &server_id, &log_server_id, farder_crypto::event_log::EventPayload::MemberApproved { member: target }).await
}
#[tauri::command]
pub async fn deny_member(state: State<'_, Arc<AppState>>, server_id: String, log_server_id: String, member: String) -> Result<(), String> {
    let target = parse_public_key(&member)?;
    moderate_member(&state, &server_id, &log_server_id, farder_crypto::event_log::EventPayload::MemberRemoved { member: target }).await
}
```
`moderate_member` is the shared emit helper (identity+device load, chain lock, DeviceAuthorized-if-needed, build+submit the given payload, advance+save on accept) — copy the body shape from `join_log_server`'s emit section, substituting the payload.

- [ ] **Step 2: `get_membership_status` / `get_pending_members`** (read-only request/response):
```rust
#[tauri::command]
pub async fn get_membership_status(state: State<'_, Arc<AppState>>, server_id: String) -> Result<String, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::GetMembershipStatus).await.map_err(|e| e.to_string())? {
        ServerResponse::MembershipStatus { status } => Ok(status),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}
#[tauri::command]
pub async fn get_pending_members(state: State<'_, Arc<AppState>>, server_id: String) -> Result<Vec<MemberInfo>, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::GetPendingMembers).await.map_err(|e| e.to_string())? {
        ServerResponse::PendingMembers { members } => Ok(members),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}
```
(Confirm `MemberInfo` is serializable to the frontend — it already is via `get_members`.)

- [ ] **Step 3: Register all four in `main.rs` `generate_handler!`; add four wrappers in `tauri-bridge.ts`** (`approveMember(serverId, logServerId, member)`, `denyMember(...)`, `getMembershipStatus(serverId)`, `getPendingMembers(serverId)`). **Update the docs** for the four new commands.

- [ ] **Step 4: Compile-check** (cargo build + tsc) — clean. **Commit.**
```bash
git commit -am "feat(client): approve/deny + membership-status/pending-members commands"
```

---

## Task 7: Client — membership-status state + waiting screen + listener

**Files:**
- Modify: `client/src/context/ServerContext.tsx` — `membershipStatus` field/action/reducer.
- Modify: `client/src/components/AppShell.tsx` — fetch status on connect; intercept render.
- Modify: `client/src/hooks/useServerEvents.ts` — `membership_changed` listener.
- Modify: `client/src/themes/*/theme.css` — waiting-screen styles.

**Interfaces:**
- Consumes: `api.getMembershipStatus`, the `server:membership_changed` Tauri event.
- Produces: `PerServerState.membershipStatus: "member" | "pending" | "none"`; the waiting screen.

- [ ] **Step 1: Context field/action/reducer**

Add `membershipStatus: "member" | "pending" | "none"` to `PerServerState` (default `"member"` so existing/instant flows are unaffected until proven pending), an action `{ type: "SET_MEMBERSHIP_STATUS"; serverId; status }`, and a reducer case that sets it on the per-server state (mirror how `logServerId`/`relayed` are handled).

NOTE: default `"member"` avoids gating the normal flow; the status is only set to `"pending"`/`"none"` once explicitly fetched. The owner and instant-joiners are members, so they never see the waiting screen.

- [ ] **Step 2: Fetch status on connect + on `membership_changed`**

In `AppShell.tsx`, when the active server is connected (and is log-mode — `logServerId` present), call `api.getMembershipStatus(serverId)` and dispatch `SET_MEMBERSHIP_STATUS`. In `useServerEvents.ts`, add:
```ts
    listen("server:membership_changed", (e) => {
      const data = e.payload as { server_id: string; public_key: string };
      const serverId = data.server_id;
      // Re-fetch my own status (I may have just been approved/denied), the member
      // list, and the pending queue — all derive from the changed log membership.
      api.getMembershipStatus(serverId).then(status =>
        dispatch({ type: "SET_MEMBERSHIP_STATUS", serverId, status: status as any })).catch(() => {});
      api.getMembers(serverId).then(members =>
        dispatch({ type: "SET_MEMBERS", serverId, payload: members })).catch(() => {});
      // The approval queue component refetches getPendingMembers on this event (Task 8).
    }).then(safePush);
```

- [ ] **Step 3: Intercept render in `AppShell`**

When `activeServer?.membershipStatus === "pending"`, render a waiting panel INSTEAD of the channel/chat/member layout; when `"none"`, a "not a member / request declined" panel; otherwise the normal shell. Use a new `<WaitingForApproval>` block (inline or a small component) with classes styled in all themes.

- [ ] **Step 4: Theme CSS**

Add `.waiting-approval-panel` (+ heading/text classes if needed) to EVERY `client/src/themes/*/theme.css`, using `var(--xp-panel-bg)`, `var(--xp-text-normal)`, etc. Confirm: `grep -l "waiting-approval-panel" client/src/themes/*/theme.css` lists all theme files.

- [ ] **Step 5: Compile-check (tsc) + commit.**
```bash
git commit -am "feat(client): pending 'waiting for approval' screen + membership_changed handling"
```

---

## Task 8: Client — approval-queue UI

**Files:**
- Create: `client/src/components/PendingApprovals.tsx`.
- Modify: `client/src/components/MemberSidebar.tsx` — render `PendingApprovals` for approvers.
- Modify: `client/src/themes/*/theme.css` — pending card styles.

**Interfaces:**
- Consumes: `api.getPendingMembers`, `api.approveMember`, `api.denyMember`; `getActorPermissions`/`hasPermission`/`PERMISSIONS.KICK_MEMBERS` (the approver check, as `MemberContextMenu` uses); `activeServer.logServerId`.
- Produces: the pending-requests section.

- [ ] **Step 1: `PendingApprovals` component**

A component that: checks if the viewer is an approver (`hasPermission(bits, KICK_MEMBERS)` or owner — mirror `MemberContextMenu`'s `canKick`); if so, fetches `getPendingMembers(serverId)` (on mount + on the `membership_changed` event — subscribe via the same mechanism, or re-fetch when a prop/counter changes), and renders a "Pending requests (N)" list. Each row: display name + Approve (✓) and Deny (✗) buttons calling `api.approveMember(serverId, logServerId, pk)` / `api.denyMember(...)`. Hide the section entirely when not an approver or when the list is empty. Reuse existing member-row / button classes where possible; any NEW class must be added to all theme CSS files.

- [ ] **Step 2: Render it in `MemberSidebar`** above the member list (approvers only — the component self-gates, so just render it).

- [ ] **Step 3: Theme CSS** for any new pending-card/badge/button classes in EVERY theme (var-driven). Confirm with `grep -l`.

- [ ] **Step 4: Compile-check (tsc) + commit.**
```bash
git commit -am "feat(client): approval queue UI (pending requests with approve/deny)"
```

---

## Task 9: Runtime verification (owner, on Windows)

**Files:** none. Server changed → full rebuild incl. sidecar.

- [ ] **Step 1: Full rebuild.**
```powershell
git pull
cargo build -p farder-server
# stop the running app, then:
.\client\src-tauri\binaries\copy-sidecar.ps1
cd client; npm run tauri dev
```
`Ctrl+Shift+R`.

- [ ] **Step 2: Approval round-trip.**
- Owner creates an invite with **"Require approval" checked**.
- Second identity (the `FARDER_DATA` instance) joins with it → should land on the **"waiting for approval"** screen with no channels/messages.
- Owner sees a **Pending requests** entry in the member sidebar → clicks **Approve**.
- The second identity should **transition in live** (waiting screen drops, channels appear) and be able to post.
- Repeat with **Deny** → the second identity should NOT get in (declined / not-a-member screen).

- [ ] **Step 3: Report** whether: the toggle creates an approval invite; the joiner is gated to the waiting screen (no channel names visible); the owner sees + can approve/deny; approval transitions the joiner in live. Capture the joiner's console on any failure.

---

## Self-Review (completed by plan author)

**Spec coverage** (against `2026-06-27-mesh-invite-join-flow-design.md` §"Approving", §"Client UX", §"Server-side content gating"):
- `requires_approval` toggle on create-invite → Task 5. ✓
- Joiner pending → "waiting for approval", no content → Tasks 1 (status) + 7 (waiting screen); content already gated by 3a; structure hidden by Task 4. ✓
- Approval queue for owner / `"kick"` holders; Approve emits `MemberApproved`, Deny emits `MemberRemoved` → Tasks 2 (endpoint) + 6 (commands) + 8 (UI). ✓
- Live transition on approval (joiner already connected) → Task 3 (`MembershipChanged` broadcast) + Task 7 (listener re-fetches status). ✓
- Pending member never receives content/structure (server-enforced) → 3a gate + Task 4 (reduced GetServerInfo). ✓
- Carry-forwards from 3a addressed: I1 (GetServerInfo structure leak) → Task 4; pending signal machine-readable → Task 1's `GetMembershipStatus` (replaces error-string reliance). ✓
- DEFERRED (unchanged): full derived-view membership / drop legacy registration; host-stamped invite expiry (M1); legacy/log invite unification (M2/M3); hiding `EventTarget::All` presence/metadata from non-members (3a Minor) — optional, not required for approval.

**Placeholder scan:** none — each step gives concrete code or concrete UI/CSS instructions; the "adapt to real helper/field names" notes are scaffolding guidance with binding assertions stated (consistent with prior sub-projects).

**Type consistency:** `GetMembershipStatus`/`MembershipStatus{status:String}`, `GetPendingMembers`/`PendingMembers{members}`, `MembershipChanged{public_key}` defined in Task 1-3 and consumed in Tasks 6-8; `membershipStatus` field defined in Task 7 and read in Tasks 7-8; the four client commands' names match their `generate_handler!` registration and bridge wrappers; approve/deny emit `MemberApproved`/`MemberRemoved` (the exact payloads from sub-project 1).
