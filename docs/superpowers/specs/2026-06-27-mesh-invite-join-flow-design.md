# Mesh Invite / Join Flow — Design Spec

**Status:** Approved (brainstorm 2026-06-27)
**Parent:** `docs/superpowers/specs/2026-06-25-mesh-signed-log-foundation-design.md` (Rung 1)
**Builds on:** sub-projects 1 (event crypto), 2 (authz state machine), 3a (server ingest), 3b (client send path) — all merged.

## Goal

Let people other than the server owner become real, signed members of a mesh
server's event log, so they can post over the mesh — via an invite/join flow
that looks and feels like Farder's existing invite experience.

## Problem / context

Membership currently exists in **two disconnected systems** on a mesh server:

- **Legacy (today's working join):** an invite code is consumed in the transport
  handshake (`connection.rs::authenticate_new_member`), which registers the
  joiner in the SQLite `members` table. They appear in the member list and can
  use the legacy `SendMessage` path.
- **The signed log (the mesh):** membership lives as `MemberJoined` events. The
  authz rules for invites/joins are already built and unit-tested in
  `event_log_state.rs`, but **nothing emits those events**. So a joiner is never
  added to `LogState.members`, and their `MessagePosted` is rejected
  ("only members may post"). Only the owner — seeded as a member at genesis —
  can post over the mesh.

This feature bridges the gap: joining a mesh server emits the signed log events
that make the joiner a member of the log, not just the SQLite table.

## Approach

**The log is the single source of truth for membership.** Reuse the existing
invite-code experience, but creating an invite and joining write *signed events*
to the log. The SQLite `members` table becomes a derived view of the log
(exactly as `messages` already is). A rejected alternative — keep the legacy
SQLite join and "mirror" a copy into the log — was discarded: it creates two
sources of truth that drift, and it cannot honor the rule that a `MemberJoined`
must be signed by the joiner themselves.

This flow applies **only to mesh servers** (those with a genesis / `server_id`).
Legacy non-mesh servers keep their current invite/join unchanged.

## Membership model (decided)

- **Instant on valid invite is the default** (anyone holding a working invite
  becomes a full member immediately — like Farder/Discord today).
- **Approval-required invites are also supported**, as a per-invite toggle.
  A join against an approval invite leaves the joiner *pending* until an
  approver promotes them.
- **Who can approve:** anyone holding the existing **`"kick"`** capability
  (member management) — not just the owner. The owner holds every capability
  implicitly. This is the "reuse the permission system" decision.
- **A pending member sees a "waiting to be approved" screen with no content**
  — gated both client-side and server-side (see §"Server-side content gating").

## End-to-end flow

### Creating an invite (owner, or anyone with the `"invite"` capability)
- Same create-invite action as today → produces a shareable code/link.
- On a mesh server it also emits an `InviteCreated` log event carrying
  `max_uses`, `expires_at`, and the new `requires_approval` flag. Only the
  **hash** of the code is stored in the log (`code_hash`), never the raw code,
  so the log never leaks a working invite.

### Joining (the new member)
1. The joiner pastes the code and connects; the transport authenticates their
   identity (as today).
2. The client auto-bootstraps over the log:
   - emits `DeviceAuthorized` (authorize this device — already built in 3b), then
   - emits a self-signed `MemberJoined { member: self, invite }` citing the
     matching `InviteCreated` event. The joiner learns the invite's event hash
     by asking the host to **resolve the code to its invite event** during the
     join (the host holds the log).
3. **Instant invite** → the joiner is added to `members` immediately; they can post.
4. **Approval invite** → the joiner is added to `pending`; their client shows the
   waiting screen; they cannot post and receive no content.

### Approving (owner, or anyone with `"kick"`)
- Approvers see a **Pending requests** list (with a count badge). Approving emits
  `MemberApproved { member }` (signed by the approver; requires `"kick"`). That
  moves the member `pending → members`. The host broadcasts the event; the
  pending joiner is still connected, so their client drops the waiting screen and
  they're in — no reconnect.
- **Denying emits `MemberRemoved { member }`** (requires `"kick"`), which clears
  the pending entry. Because pending state lives in the log, denial must be a
  signed event too — otherwise the pending entry would reappear on replay.

## Log additions

Three small additions on top of the existing, tested authz surface.

### 1. New field on `InviteCreated`
```
EventPayload::InviteCreated { code_hash: String, max_uses: u32, expires_at: u64, requires_approval: bool }
```
`requires_approval` is added with `#[serde(default)]` (defaults to `false`) so
existing serialized events remain valid.

### 2. New event `MemberApproved`
```
EventPayload::MemberApproved { member: PublicKey }
```
**Authz (in `LogState::apply`):**
- signer must hold the `"kick"` capability (`has_capability(author, "kick")`);
- `member` must currently be in `pending`;
- ban gate applies as for every event (a banned identity cannot be approved).

**Effect:** remove `member` from `pending`, insert into `members`.

### 3. `pending` state in `LogState`
Add a `pending` set (e.g. `HashSet<PublicKey>`) alongside the existing
`members`/`banned`/`capabilities`/`devices`/`invites`/`chains`.

**Changed authz for `MemberJoined`** (existing rules unchanged: self-authored,
invite exists, `use_count < max_uses`, not expired, ban gate):
- if the cited invite's `requires_approval` is `true` → insert `member` into `pending`;
- else → insert `member` into `members` (today's behavior).
- **The invite "use" is consumed on `MemberJoined` either way** — a denied
  request still spends a use (bounds request-spam; matches today's
  consume-on-join semantics). Denial does not refund.

`MessagePosted` authz is unchanged (`is_member(author)`); pending members are not
in `members`, so the existing rule already prevents them from posting — no new
rule needed.

**`MemberRemoved` effect extended:** it currently removes the target from
`members`; extend it to also remove the target from `pending`, so it doubles as
the "deny a pending request" event. Its existing authz (self-leave, or
`"kick"`) already fits denial (an approver holds `"kick"`).

**Query helpers** to add on `LogState`: `is_pending(&PublicKey) -> bool` and a
way to list pending members (for the server to serve the approval queue and to
gate content).

## Server-side content gating

A pending member is connected (transport-authenticated) but must receive **no
channel or message content** until approved — enforced on the host, not just
hidden in the client. Concretely: requests that serve channel lists, messages,
or other member-visible content must check the log's membership and return
empty/denied for a member who is `pending` (or not a member at all). This makes
"no content until approved" a real guarantee — essential once channels are
encrypted, where a pending member must never receive the content at all.

## Client UX

Four surfaces, all reusing existing patterns:

1. **Create-invite dialog** — add one checkbox: *"Require approval to join."*
   Off by default. On a mesh server, creating the invite also emits
   `InviteCreated` to the log.
2. **Joining** — unchanged for the joiner (paste code, connect). The
   device-authorize + join events happen automatically and invisibly. Instant
   invite → straight in; approval invite → waiting screen.
3. **Waiting screen** — "Waiting to be approved…", no channels/messages. The
   client listens for the broadcast `MemberApproved` (it is already connected)
   and transitions in when approved; shows "request declined" if denied.
4. **Approval queue** — a **Pending requests** list with a count badge, visible
   to the owner / anyone with `"kick"`; each row has **Approve** / **Deny**.
   Approve emits `MemberApproved`; Deny drops the request.

## Coexistence with legacy

- Legacy (non-mesh) servers: invite/join completely unchanged.
- Mesh servers: the transport still authenticates identity; the **log is
  authoritative** for membership and the SQLite `members` table is a derived
  view (rows derived from `MemberJoined`/`MemberApproved`, like message rows are
  derived today). Reconciliation on startup mirrors the existing
  `reconcile_messages` pattern.
- **No backfill** of pre-existing legacy members in old servers (old servers are
  disposable test environments). Going forward every mesh join uses this flow.

## Edge cases

- Expired / used-up invite → join rejected, surfaced with a clear message.
- Already a member (reconnect) → client detects it is already in `LogState.members`
  and connects normally without re-emitting `MemberJoined`.
- Banned identity → cannot join even with a valid invite (ban gate supersedes
  everything — already enforced).
- Denied while pending → "request declined"; reconnecting keeps the joiner
  pending (it is in the log) until approved or denied.
- Two approvers approve simultaneously → the second `MemberApproved` finds the
  member no longer in `pending` and is a harmless no-op/reject (idempotent at the
  app level).
- **Rung-1 limitation:** with a single host, if the host is offline nobody can
  join (no one to accept/sequence the events). Expected until mesh replication
  lands in a later rung. State this in the UI where relevant.

## Decomposition (build order)

Three sub-projects, each with its own implementation plan:

1. **Log primitives** — pure Rust in `farder-crypto`, fully unit-tested, no
   runtime: `requires_approval` field, `MemberApproved` event, `pending` state +
   authz rules + query helpers.
2. **Instant invites, end-to-end** (server + client): create-invite emits
   `InviteCreated`; join auto-emits `DeviceAuthorized` + `MemberJoined`; server
   derives membership from the log and gates non-members from content. **This
   alone delivers a working multi-person mesh** (instant invites) — the core
   unblock. Owner verifies the live round-trip on Windows.
3. **Approval path**: the `requires_approval` toggle UI, pending state
   end-to-end, waiting screen, approval-queue UI, `MemberApproved` emission,
   server-side content gating for pending members, broadcast-on-approval.

Instant ships first (1–2); approval follows (3). Both are in this design.

## Testing

- **Crypto state machine (unit):** `requires_approval` branching in
  `MemberJoined` (instant → members, approval → pending); `MemberApproved` authz
  (only `"kick"`-capable signers; only when target is pending); pending → member
  transition; pending member cannot post; ban supersedes approval; use-count
  consumed on join including denied requests.
- **Server (integration):** join bootstrap accepted
  (`DeviceAuthorized` → `MemberJoined` → `MessagePosted`); pending member gated
  from channel/message content; `MemberApproved` broadcast reaches the joiner;
  member rows derived + reconciled from the log.
- **Client (runtime, owner-verified on Windows):** owner creates an invite, a
  second identity joins and posts (instant); and the approval variant — second
  identity joins an approval invite → waiting screen → owner approves → joiner
  drops in and posts.

## Security carry-forwards (from the parent mesh spec)

- **M1 device binding:** every event's device pubkey is derived only from a
  verified `DeviceCert` bound to `core.author`/`core.device` — never trusted from
  the event. `MemberJoined`/`MemberApproved` validation rides on this.
- **Self-authored joins:** `MemberJoined.member == author`; a join cannot be
  forged for someone else.
- **Approval is capability-gated** (`"kick"`), never inferred from connection
  state; the host runs the same pure `apply` fold to accept the event.
- **Ban supersedes** join and approval.
- **Pending content gating is server-enforced**, not client-cosmetic.
