# Mesh Invite/Join — Sub-project 2: Instant Invites End-to-End — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a non-owner join a mesh server with an instant invite and post over the
event log — the multi-person unblock — by emitting `InviteCreated` and
`MemberJoined` log events and resolving a presented code to its invite event.

**Architecture:** The log primitives (sub-project 1) and the `SubmitEvent` accept
path (3a/3b) already exist. This sub-project wires them end-to-end:
(a) creating an invite on a mesh server also emits a signed `InviteCreated` log
event; (b) a joiner, after connecting, resolves the code to that event and emits
`DeviceAuthorized` + `MemberJoined`, which makes them a log member who can post.

**Sequencing decision (IMPORTANT — pragmatic coexistence, not yet log-authoritative):**
The existing connection handshake is left UNCHANGED — it still authenticates the
joiner and registers them in the legacy `members` table (so they appear in the
member list with their profile), and still consumes the legacy invite to gate the
connection. The log layer is added ON TOP to authorize *posting* over the mesh
(`MessagePosted` checks the log's `is_member`, set by `MemberJoined`). The heavier
work to make the log the SOLE source of truth — dropping legacy member
registration, deriving member rows from the log, and gating all content from
non-members — is explicitly deferred to **sub-project 3**, where pending-member
content-gating already requires that infrastructure. This keeps the instant-invite
unblock low-risk and off the critical connect path.

**Tech Stack:** Rust (`farder-crypto`, `farder-server`, `farder-protocol`), Tauri
(`client/src-tauri`), TypeScript/React. Rust tests via `cargo test`; client via
`cargo build` + `npx tsc --noEmit`; the join round-trip is owner-verified on Windows.

## Global Constraints

- `code_hash` is `sha256_hex(code.as_bytes())`, computed via the SAME shared helper on both server (resolve) and client (create) — never two separate implementations. (spec §"End-to-end flow")
- Instant invites only: `requires_approval = false` everywhere in this sub-project. The approval path is sub-project 3. (spec §Decomposition #2)
- Do NOT modify the connection handshake (`connection.rs::authenticate`) or remove legacy member registration — see the sequencing decision above.
- Reuse `event_build_next` / `event_send_submit` / `DeviceState` from `client/src-tauri/src/commands.rs` + `device.rs` (the 3b send path) for emitting events client-side.
- `DeviceState` gains a `joined: bool` with `#[serde(default)]` so existing `device_state.json` files stay valid.
- The untyped Tauri seam: every new `invoke("X")` name must match a registered `#[tauri::command] fn X` in `main.rs`'s `generate_handler!`. (CLAUDE.md)

---

## File Structure

- `crates/farder-crypto/src/event_log.rs` — expose `pub fn invite_code_hash(code: &str) -> String` (canonical code→hash used by both sides).
- `crates/farder-server/src/event_ingest.rs` — add `find_invite_event_by_code(conn, code) -> Result<Option<EventHash>>` (scan `InviteCreated` events, hash-match).
- `crates/farder-protocol/src/server.rs` — add `ServerRequest::ResolveInvite { code }` and `ServerResponse::InviteResolved { invite_event: Option<String> }`.
- `crates/farder-server/src/handlers.rs` — add the `ResolveInvite` handler arm.
- `client/src-tauri/src/device.rs` — add `joined: bool` to `DeviceState`.
- `client/src-tauri/src/commands.rs` — make `create_invite` mesh-aware (emit `InviteCreated`); add `join_log_server` command.
- `client/src-tauri/src/main.rs` — register `join_log_server` in `generate_handler!`.
- `client/src/lib/tauri-bridge.ts` — `resolveInvite`/`joinLogServer` wrappers; thread `logServerId` into `createInvite`.
- `client/src/App.tsx` (+ the connect-dialog path) — after connecting with an invite to a log-mode server, call `joinLogServer`.

---

## Task 1: Canonical `invite_code_hash` helper (farder-crypto)

**Files:**
- Modify: `crates/farder-crypto/src/event_log.rs` (the private `sha256_hex` is at line 8; add a public wrapper near it)

**Interfaces:**
- Produces: `pub fn invite_code_hash(code: &str) -> String` — used by the server's
  `ResolveInvite` and the client's invite creation so `InviteCreated.code_hash`
  and a resolution lookup agree.

- [ ] **Step 1: Write the failing test**

Add to the test module in `crates/farder-crypto/src/event_log.rs` (create a `#[cfg(test)] mod tests { ... }` at the end of the file if none exists):

```rust
#[cfg(test)]
mod invite_code_hash_tests {
    use super::invite_code_hash;

    #[test]
    fn invite_code_hash_is_stable_and_distinct() {
        let h = invite_code_hash("AbCd1234");
        assert_eq!(h.len(), 64, "sha-256 hex is 64 chars");
        assert_eq!(h, invite_code_hash("AbCd1234"), "deterministic");
        assert_ne!(h, invite_code_hash("AbCd1235"), "distinct codes → distinct hashes");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p farder-crypto invite_code_hash_is_stable_and_distinct`
Expected: FAIL — compile error, `invite_code_hash` does not exist.

- [ ] **Step 3: Add the helper**

In `crates/farder-crypto/src/event_log.rs`, immediately after the private `sha256_hex` fn (around line 10), add:

```rust
/// Canonical hash of an invite code, stored as `InviteCreated.code_hash` and used
/// by the server to resolve a presented code to its invite event. The raw code is
/// never put in the log — only this hash.
pub fn invite_code_hash(code: &str) -> String {
    sha256_hex(code.as_bytes())
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p farder-crypto invite_code_hash_is_stable_and_distinct`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-crypto/src/event_log.rs
git commit -m "feat(crypto): expose invite_code_hash (canonical code->hash for invites)"
```

---

## Task 2: `ResolveInvite` — server resolves a code to its invite event

**Files:**
- Modify: `crates/farder-server/src/event_ingest.rs` (add `find_invite_event_by_code`)
- Modify: `crates/farder-protocol/src/server.rs` (add request + response variants)
- Modify: `crates/farder-server/src/handlers.rs` (add the handler arm)
- Test: in `crates/farder-server/src/event_ingest.rs` test module

**Interfaces:**
- Consumes: `invite_code_hash` (Task 1); `load_events_in_order` (event_ingest);
  `EventPayload::InviteCreated { code_hash, .. }`, `Event::hash`.
- Produces:
  - `pub fn find_invite_event_by_code(conn: &Connection, code: &str) -> Result<Option<EventHash>>`
  - `ServerRequest::ResolveInvite { code: String }`
  - `ServerResponse::InviteResolved { invite_event: Option<String> }`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `crates/farder-server/src/event_ingest.rs` (use the module's existing helpers for building a genesis + storing events; mirror the existing tests there):

```rust
    #[test]
    fn find_invite_event_by_code_matches_on_hash() {
        let conn = test_conn(); // existing helper that sets up schema + genesis
        let (owner, owner_dev, genesis) = test_owner_setup(&conn); // existing helper

        // Owner authorizes their device, then creates an invite for code "JOINME12".
        let da = device_authorized_event(&owner, &owner_dev, &genesis); // existing helper
        store_event(&conn, &da).unwrap();
        let code = "JOINME12";
        let inv = farder_crypto::event_log::Event::next(
            &owner_dev, owner.public_key(), genesis.server_id(), Some(&da), 1, 1,
            farder_crypto::event_log::EventPayload::InviteCreated {
                code_hash: farder_crypto::event_log::invite_code_hash(code),
                max_uses: 5, expires_at: 9_999_999_999, requires_approval: false,
            },
        );
        store_event(&conn, &inv).unwrap();

        // The right code resolves to the invite's event hash; a wrong code resolves to None.
        assert_eq!(find_invite_event_by_code(&conn, code).unwrap().as_deref(), Some(inv.hash().as_str()));
        assert_eq!(find_invite_event_by_code(&conn, "WRONGcode").unwrap(), None);
    }
```

NOTE to implementer: use whatever genesis/owner/device test helpers already exist
in this module (read the existing tests first). If a helper named differently
exists, adapt the calls; the assertions on `find_invite_event_by_code` are the
binding part.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p farder-server find_invite_event_by_code_matches_on_hash`
Expected: FAIL — `find_invite_event_by_code` does not exist.

- [ ] **Step 3: Implement `find_invite_event_by_code`**

In `crates/farder-server/src/event_ingest.rs`, add (it scans `InviteCreated` events; small N on a personal server):

```rust
/// Resolve a presented invite code to the hash of its `InviteCreated` event, by
/// matching `invite_code_hash(code)` against stored events. Returns `None` if no
/// invite matches (unknown/typo code). The raw code is never stored — only its hash.
pub fn find_invite_event_by_code(conn: &Connection, code: &str) -> Result<Option<EventHash>> {
    let target = farder_crypto::event_log::invite_code_hash(code);
    for event in load_events_in_order(conn)? {
        if let farder_crypto::event_log::EventPayload::InviteCreated { code_hash, .. } =
            &event.core.payload
        {
            if code_hash == &target {
                return Ok(Some(event.hash()));
            }
        }
    }
    Ok(None)
}
```

Ensure `EventHash` is in scope (it is a `String` alias from `farder_crypto::event_log`; import if needed).

- [ ] **Step 4: Add the protocol variants**

In `crates/farder-protocol/src/server.rs`, add to `ServerRequest` (near `CreateInvite`, ~line 262):

```rust
    /// Resolve an invite code to the hash of its log `InviteCreated` event, so a
    /// joiner can cite it in a `MemberJoined`. Returns None for an unknown code.
    ResolveInvite { code: String },
```

And to `ServerResponse` (near `InviteCreated`, ~line 338):

```rust
    InviteResolved { invite_event: Option<String> },
```

- [ ] **Step 5: Add the handler arm**

In `crates/farder-server/src/handlers.rs`, add an arm (place it near the `CreateInvite` arm, ~line 974). It requires no special permission — resolving a code you already hold reveals only the invite's event hash, which is needed to join:

```rust
        ServerRequest::ResolveInvite { code } => {
            let invite_event = crate::event_ingest::find_invite_event_by_code(conn, &code)?;
            ok(ServerResponse::InviteResolved { invite_event })
        }
```

(Match the surrounding arms' return idiom — if they use `ok(...)` / `ok_with(...)`, use the same; read a neighboring read-only arm like `GetMemberProfile`.)

- [ ] **Step 6: Run the test + full server suite**

Run: `cargo test -p farder-server find_invite_event_by_code_matches_on_hash && cargo test -p farder-server`
Expected: PASS — new test green, no regressions.

- [ ] **Step 7: Commit**

```bash
git add crates/farder-server/src/event_ingest.rs crates/farder-protocol/src/server.rs crates/farder-server/src/handlers.rs
git commit -m "feat(server): ResolveInvite resolves a code to its InviteCreated event hash"
```

---

## Task 3: `create_invite` emits an `InviteCreated` log event (client)

**Files:**
- Modify: `client/src-tauri/src/commands.rs` (`create_invite` ~line 2113; reuse `event_build_next`/`event_send_submit` ~3734)
- Modify: `client/src/lib/tauri-bridge.ts` (`createInvite` wrapper)
- Modify: the create-invite caller(s) in the frontend to pass `logServerId`

**Interfaces:**
- Consumes: `ServerResponse::InviteCreated { code }`; `invite_code_hash` is not
  available in Rust client? It IS — `farder_crypto::event_log::invite_code_hash`.
  `event_build_next(device, identity, server_id, prev, seq, lamport, payload)` and
  `event_send_submit(state, server_id, &event)` (existing in commands.rs);
  `DeviceState` (load/save), `load_or_create_device_keypair`, identity from
  `state.signing_key_bytes`.
- Produces: `create_invite(state, server_id, log_server_id: Option<String>, max_uses)`;
  for log-mode servers it emits a signed `InviteCreated` after obtaining the code.

- [ ] **Step 1: Add the `log_server_id` parameter and mesh emission to `create_invite`**

In `client/src-tauri/src/commands.rs`, change the `create_invite` signature to add
`log_server_id: Option<String>` (after `server_id`):

```rust
pub async fn create_invite(
    state: State<'_, Arc<AppState>>,
    server_id: String,            // connection key (address) — routes the request
    log_server_id: Option<String>, // genesis hash when log-mode; None for legacy
    max_uses: Option<u32>,
) -> Result<InviteResult, String> {
```

After the existing code that extracts `code` from `ServerResponse::InviteCreated { code }`
and BEFORE building the link/return value, add the mesh emission. Use a 30-day
expiry to mirror a reasonable default (the legacy invite has its own expiry; this
is the log invite's `expires_at`):

```rust
    // Mesh server: also record the invite as a signed InviteCreated event in the
    // log, so a joiner can cite it in their MemberJoined. Instant invite for now
    // (requires_approval = false; approval is sub-project 3).
    if let Some(log_sid) = log_server_id {
        let identity = {
            let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
            let bytes = lock.ok_or_else(|| "identity is locked".to_string())?;
            Keypair::from_signing_key_bytes(&bytes)
        };
        let device = crate::device::load_or_create_device_keypair()?;
        let mut ds = crate::device::DeviceState::load(&log_sid)?
            .unwrap_or_else(|| crate::device::DeviceState::fresh(&device));

        // First action on this server authorizes the device (mirrors submit_event).
        if !ds.authorized {
            let cert = crate::device::device_cert(&identity, &device);
            let da = event_build_next(&device, &identity, &log_sid, ds.last_event_hash.clone(),
                ds.next_seq, ds.lamport, farder_crypto::event_log::EventPayload::DeviceAuthorized { cert });
            event_send_submit(&state, &server_id, &da).await?;
            ds.next_seq = da.core.seq + 1;
            ds.last_event_hash = Some(da.hash());
            ds.lamport = da.core.lamport;
            ds.authorized = true;
            ds.save(&log_sid)?;
        }

        let expires_at = event_now_secs() + 30 * 24 * 60 * 60;
        let inv = event_build_next(&device, &identity, &log_sid, ds.last_event_hash.clone(),
            ds.next_seq, ds.lamport, farder_crypto::event_log::EventPayload::InviteCreated {
                code_hash: farder_crypto::event_log::invite_code_hash(&code),
                max_uses: max_uses.unwrap_or(0),
                expires_at,
                requires_approval: false,
            });
        event_send_submit(&state, &server_id, &inv).await?;
        ds.next_seq = inv.core.seq + 1;
        ds.last_event_hash = Some(inv.hash());
        ds.lamport = inv.core.lamport;
        ds.save(&log_sid)?;
    }
```

NOTE: `max_uses.unwrap_or(0)` — confirm the authz semantics. In sub-project 1,
`MemberJoined` requires `use_count < max_uses`, so `max_uses: 0` would block ALL
joins. The legacy `Option<u32>` treats `None` as "unlimited". To preserve
"unlimited" in the log, map `None`/`0` to `u32::MAX`:

```rust
                max_uses: max_uses.filter(|n| *n > 0).unwrap_or(u32::MAX),
```

Use that mapping (not `unwrap_or(0)`).

- [ ] **Step 2: Update the bridge wrapper**

In `client/src/lib/tauri-bridge.ts`, find `createInvite` and add `logServerId`:

```ts
export async function createInvite(serverId: string, logServerId: string | null, maxUses: number | null): Promise<InviteResult> {
  return invoke("create_invite", { serverId, logServerId, maxUses });
}
```

(Adapt to the existing `createInvite` return type / param names; keep the existing return shape.)

- [ ] **Step 3: Pass `logServerId` from the create-invite UI**

Find the component that calls `api.createInvite(...)` (search `createInvite(` under `client/src`). Pass the active server's `logServerId` (the field added in 3b's `ServerContext`), or `null` for legacy servers:

```ts
const logServerId = activeServer?.logServerId ?? null;
const result = await api.createInvite(serverId, logServerId, maxUses);
```

- [ ] **Step 4: Compile-check**

Run: `cd client/src-tauri && cargo build` then `cd client && npx tsc --noEmit`
Expected: both clean (the new code is exercised at runtime; this task is
compile-verified — owner verifies the event actually lands in Task 5).

- [ ] **Step 5: Commit**

```bash
git add client/src-tauri/src/commands.rs client/src/lib/tauri-bridge.ts client/src/<the-create-invite-component>.tsx
git commit -m "feat(client): create_invite emits an InviteCreated log event on mesh servers"
```

---

## Task 4: `join_log_server` — joiner emits `MemberJoined` (client)

**Files:**
- Modify: `client/src-tauri/src/device.rs` (`DeviceState`: add `joined: bool`)
- Modify: `client/src-tauri/src/commands.rs` (add `join_log_server`)
- Modify: `client/src-tauri/src/main.rs` (register `join_log_server`)
- Modify: `client/src/lib/tauri-bridge.ts` (`joinLogServer` wrapper)
- Modify: `client/src/App.tsx` (and the connect-dialog success path) — call `joinLogServer` after connecting with an invite to a log-mode server

**Interfaces:**
- Consumes: `ServerRequest::ResolveInvite` / `ServerResponse::InviteResolved`
  (Task 2); `event_build_next`/`event_send_submit`; `DeviceState`; identity/device
  loaders; `EventPayload::MemberJoined { member, invite }`.
- Produces: `join_log_server(state, server_id, log_server_id, invite_code) -> Result<(), String>`;
  `DeviceState.joined: bool`.

- [ ] **Step 1: Add `joined` to `DeviceState`**

In `client/src-tauri/src/device.rs`, add the field (with serde default) to the struct (after `authorized`):

```rust
    /// Whether this identity has already submitted its MemberJoined to the server.
    #[serde(default)]
    pub joined: bool,
```

And in `DeviceState::fresh`, initialize `joined: false` (add the line alongside `authorized: false`).

Update the serde round-trip test in `device.rs` to set/assert `joined` (mirror the existing `authorized` assertions): set `st.joined = true;` before serializing and `assert!(back.joined);` after.

- [ ] **Step 2: Run the device test**

Run: `cargo test -p farder-client device:: 2>/dev/null || (cd client/src-tauri && cargo test device::)`
Expected: PASS — the round-trip test covers the new field.

- [ ] **Step 3: Add the `join_log_server` command**

In `client/src-tauri/src/commands.rs`, add (reuses `event_build_next`/`event_send_submit`/`event_now_secs` from the 3b send path):

```rust
#[tauri::command]
pub async fn join_log_server(
    state: State<'_, Arc<AppState>>,
    server_id: String,       // connection key (address) — routes requests
    log_server_id: String,   // genesis hash — stamps events + keys the device chain
    invite_code: String,
) -> Result<(), String> {
    use farder_crypto::event_log::EventPayload;

    let identity = {
        let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
        let bytes = lock.ok_or_else(|| "identity is locked".to_string())?;
        Keypair::from_signing_key_bytes(&bytes)
    };
    let device = crate::device::load_or_create_device_keypair()?;
    let mut ds = crate::device::DeviceState::load(&log_server_id)?
        .unwrap_or_else(|| crate::device::DeviceState::fresh(&device));

    if ds.joined {
        return Ok(()); // already a log member on this server
    }

    // 1. Authorize this device if needed (mirrors submit_event / create_invite).
    if !ds.authorized {
        let cert = crate::device::device_cert(&identity, &device);
        let da = event_build_next(&device, &identity, &log_server_id, ds.last_event_hash.clone(),
            ds.next_seq, ds.lamport, EventPayload::DeviceAuthorized { cert });
        event_send_submit(&state, &server_id, &da).await?;
        ds.next_seq = da.core.seq + 1;
        ds.last_event_hash = Some(da.hash());
        ds.lamport = da.core.lamport;
        ds.authorized = true;
        ds.save(&log_server_id)?;
    }

    // 2. Resolve the invite code to its InviteCreated event hash.
    let resolved = bridge::send_request(&state, &server_id,
        ServerRequest::ResolveInvite { code: invite_code })
        .await.map_err(|e| e.to_string())?;
    let invite_event = match resolved {
        ServerResponse::InviteResolved { invite_event: Some(h) } => h,
        ServerResponse::InviteResolved { invite_event: None } =>
            return Err("invite not found on this server (it may not be a mesh invite)".to_string()),
        ServerResponse::Error { message } => return Err(message),
        other => return Err(format!("unexpected response to ResolveInvite: {:?}", other)),
    };

    // 3. Emit the self-signed MemberJoined citing the invite.
    let join = event_build_next(&device, &identity, &log_server_id, ds.last_event_hash.clone(),
        ds.next_seq, ds.lamport, EventPayload::MemberJoined { member: identity.public_key(), invite: invite_event });
    match event_send_submit(&state, &server_id, &join).await {
        Ok(_) => {
            ds.next_seq = join.core.seq + 1;
            ds.last_event_hash = Some(join.hash());
            ds.lamport = join.core.lamport;
            ds.joined = true;
            ds.save(&log_server_id)?;
            Ok(())
        }
        // Already a member (e.g. joined on another device): treat as success so we
        // stop retrying. The chain head advanced server-side only if accepted; on a
        // rejection nothing advanced, so just mark joined and move on.
        Err(e) if e.to_string().contains("already a member") => {
            ds.joined = true;
            ds.save(&log_server_id)?;
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}
```

Register it in `client/src-tauri/src/main.rs` `generate_handler!` (add `commands::join_log_server` to the list).

- [ ] **Step 4: Add the bridge wrapper**

In `client/src/lib/tauri-bridge.ts`:

```ts
export async function joinLogServer(serverId: string, logServerId: string, inviteCode: string): Promise<void> {
  return invoke("join_log_server", { serverId, logServerId, inviteCode });
}
```

- [ ] **Step 5: Call `joinLogServer` after connecting with an invite to a log-mode server**

In `client/src/App.tsx`, in the deep-link path (around line 92-96, after `connectServer(parsed.address, parsed.inviteCode, ...)` succeeds and `SERVER_ADDED` is dispatched), add — only when an invite code was used and the server is log-mode:

```ts
      if (parsed.inviteCode && result.server_id) {
        try { await api.joinLogServer(parsed.address, result.server_id, parsed.inviteCode); }
        catch (e) { console.error("[mesh] join_log_server failed:", e); }
      }
```

Apply the same after the connect-dialog join path (search for the other
`connectServer(... inviteCode ...)` + `SERVER_ADDED` site and mirror it). `result.server_id`
is the genesis hash from `ConnectResult` (present only for mesh servers).

- [ ] **Step 6: Compile-check**

Run: `cd client/src-tauri && cargo build` then `cd client && npx tsc --noEmit`
Expected: both clean.

- [ ] **Step 7: Commit**

```bash
git add client/src-tauri/src/device.rs client/src-tauri/src/commands.rs client/src-tauri/src/main.rs client/src/lib/tauri-bridge.ts client/src/App.tsx
git commit -m "feat(client): join_log_server emits MemberJoined so a joiner can post over the mesh"
```

---

## Task 5: Runtime verification (owner, on Windows)

**Files:** none (verification only). This task documents the runbook; the owner runs it.

- [ ] **Step 1: Full rebuild (server changed → sidecar must be rebuilt)**

```powershell
git pull
cargo build -p farder-server
.\client\src-tauri\binaries\copy-sidecar.ps1
cd client
npm run tauri dev
```
Then `Ctrl+Shift+R`.

- [ ] **Step 2: Owner creates a fresh mesh server + an invite**

- Create/own a new server (becomes log-mode; `server_id` set).
- Create an invite (the create-invite UI). Behind the scenes this now emits an `InviteCreated` log event.

- [ ] **Step 3: A second identity joins and posts**

- On a second client/identity (or a friend), join using the invite.
- Expected: the joiner appears in the member list (legacy registration), and after `join_log_server` runs they can **send a message that renders and survives a restart** (proving it went through the log as a member — not the owner).

- [ ] **Step 4: Report**

Confirm the second identity can post over the mesh. If the message is rejected
"only members may post", capture the client console (the `join_log_server` result)
— that is the path to debug.

---

## Self-Review (completed by plan author)

**Spec coverage** (against `2026-06-27-mesh-invite-join-flow-design.md`):
- Creating an invite emits `InviteCreated` with `code_hash` (hash only, never raw code), instant (`requires_approval=false`) → Task 3. ✓
- Joiner resolves code → invite event, emits `DeviceAuthorized` + self-signed `MemberJoined` → Tasks 2 + 4. ✓
- Instant invite → member immediately → falls out of sub-project 1's `MemberJoined` effect (instant → `members`); joiner can post. ✓
- "Already a member (reconnect) → no re-join" → `DeviceState.joined` guard + graceful "already a member" handling (Task 4). ✓
- Deviation flagged: legacy handshake kept (member-list visibility + connection gating); making membership log-authoritative + content-gating deferred to sub-project 3 → stated in Architecture/Global Constraints. ✓ (raise to owner)
- Out of scope (sub-project 3): approval toggle, pending/waiting screen, approval queue, server-side content gating, `MemberApproved` emission. Correctly excluded.

**Placeholder scan:** none — every code step has complete code; the only "adapt to
existing helper" notes are in test scaffolding (Task 2 Step 1, Task 3 Steps 2-3),
where the binding assertions/behavior are given explicitly.

**Type consistency:** `invite_code_hash(&str) -> String` used identically in Task 2
(server resolve) and Task 3 (client create); `ResolveInvite { code }` /
`InviteResolved { invite_event: Option<String> }` consistent across protocol
(Task 2), server handler (Task 2), and client `join_log_server` (Task 4);
`DeviceState.joined` defined in Task 4 Step 1 and used in Task 4 Step 3;
`max_uses` mapped `None/0 → u32::MAX` to preserve "unlimited" against sub-project
1's `use_count < max_uses` rule.
