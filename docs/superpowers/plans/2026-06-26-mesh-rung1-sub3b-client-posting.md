# Mesh Rung 1 — Sub-project 3b: Client Posting Over the Log — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the client post messages over the signed event log: harden the server's derived-view (the §2 backlog item), expose `server_id` on connect so the client knows a server runs the log, add a client device subkey + per-`(server,device)` chain state, and a `submit_event` path that builds+signs `MessagePosted` events and routes the send UI to it for log-mode servers.

**Architecture:** Additive. The server gains a transactional store+derive and startup reconciliation, and advertises `server_id` in its info response. The client gains a `device.rs` module (a per-install Ed25519 device subkey, an identity-signed `DeviceCert`, and a persisted per-`(server,device)` `DeviceState`), a `submit_event` Tauri command (auto-authorizes the device on first use, builds events via `farder_crypto::event_log::Event::next`, advances+persists the chain state), and a frontend that routes message sends to `submit_event` when the connected server is log-mode.

**Tech Stack:** Rust (Tauri client `src-tauri` + `farder-server` + `farder-protocol`), `farder-crypto::event_log`, `serde_json` (device state file), React/TS. No new deps (`farder-crypto` is already a client dep).

## Global Constraints

- **Additive:** legacy `send_message`/`SendMessage` stays; `submit_event`/`SubmitEvent` is added in parallel. The frontend chooses by server capability.
- **Reuse the merged crypto verbatim:** `Event::next(device, author, server_id, prev, lamport_observed, timestamp, payload)`, `DeviceCert::create(identity, device_pubkey, created_at)`, `device_id(&pubkey)`, `EventPayload::MessagePosted { channel_id, content, reply_to, attachments }`. Do not re-implement.
- **Device binding (M1 carry-forward):** the client signs events with the DEVICE subkey; the `author` is the identity; the device must be authorized on the server (a `DeviceAuthorized` event) before any other event — the client emits that automatically on first use per server.
- **Chain integrity:** the client tracks `next_seq`/`last_event_hash`/`lamport` per `(server_id, device_id)`, persisted, and only advances on a confirmed `EventAccepted`. On a rejection or transport error, it does NOT advance (so it can retry/resync) — mirrors the presence give-up discipline.
- **Verifiable vs runtime:** Tasks 1–3 are Rust/unit-verifiable (cargo test). Tasks 4–5 (the live send round-trip + frontend) compile-check (cargo build / `npx tsc --noEmit`) but their END-TO-END behavior needs the owner's Windows runtime test — call this out; do not claim runtime-verified.
- **Deferred (NOT here):** client-side verification of received broadcast events (a 3c follow-on — the server doesn't broadcast the signed event yet); attachments over the log (sub-project 4); per-channel post ACLs.

---

## File Structure

- **Modify** `crates/farder-server/src/handlers.rs` — wrap `store_event` + `derive_message_row` in a transaction (§2 part 1); add `server_id` to the `GetServerInfo` response.
- **Modify** `crates/farder-server/src/event_ingest.rs` — add `reconcile_messages(conn) -> Result<usize>` (§2 part 2).
- **Modify** `crates/farder-server/src/main.rs` — call `reconcile_messages` on startup (after `build_log_state`).
- **Modify** `crates/farder-protocol/src/server.rs` — add `server_id: Option<String>` to the server-info response variant (`ServerInfo`/equivalent).
- **Modify** `client/src-tauri/src/commands.rs` — add `server_id` to `ConnectResult`; populate it; add the `submit_event` command.
- **Create** `client/src-tauri/src/device.rs` — device keypair (generate/persist), `DeviceCert` helper, `DeviceState` (per-`(server,device)` chain/clock, load/save).
- **Modify** `client/src-tauri/src/{main.rs,lib.rs}` — register `submit_event` in `generate_handler!`; `mod device;`.
- **Modify** `client/src/lib/{tauri-bridge.ts,types.ts}` — `submitEvent(...)`; `ConnectResult.server_id`.
- **Modify** `client/src/components/MessageInput.tsx` — route `handleSend` to `submitEvent` when the active server is log-mode (`server_id` present).

---

## Task 1: §2 server hardening — transactional store+derive + startup reconciliation

**Files:**
- Modify: `crates/farder-server/src/handlers.rs` (SubmitEvent arm)
- Modify: `crates/farder-server/src/event_ingest.rs` (add `reconcile_messages`)
- Modify: `crates/farder-server/src/main.rs` (call on startup)

**Interfaces:**
- Produces: `event_ingest::reconcile_messages(conn: &Connection) -> Result<usize>` (derives any `MessagePosted` event lacking a `messages` row; returns count repaired).

- [ ] **Step 1: Wrap store+derive in a transaction (failing-safe)**

In `crates/farder-server/src/handlers.rs`, in the `SubmitEvent` arm, replace the two sequential calls

```rust
            crate::event_ingest::store_event(conn, &event)
                .map_err(|e| anyhow::anyhow!("failed to store event: {}", e))?;
            let derived_id = crate::event_ingest::derive_message_row(conn, &event)
                .map_err(|e| anyhow::anyhow!("failed to derive message: {}", e))?;
```

with a single atomic transaction (rusqlite `unchecked_transaction` works on a shared `&Connection`):

```rust
            let derived_id = {
                let tx = conn.unchecked_transaction()
                    .map_err(|e| anyhow::anyhow!("failed to begin tx: {}", e))?;
                crate::event_ingest::store_event(&tx, &event)
                    .map_err(|e| anyhow::anyhow!("failed to store event: {}", e))?;
                let id = crate::event_ingest::derive_message_row(&tx, &event)
                    .map_err(|e| anyhow::anyhow!("failed to derive message: {}", e))?;
                tx.commit().map_err(|e| anyhow::anyhow!("failed to commit event: {}", e))?;
                id
            };
```

(`Transaction` derefs to `Connection`, so the `&Connection`-taking helpers accept `&tx`. The in-memory `*ls_guard = Some(trial)` commit stays AFTER this block, so a tx failure returns `Err` before the state advances.)

- [ ] **Step 2: Add `reconcile_messages` + its test (write failing test first)**

In `crates/farder-server/src/event_ingest.rs`, add:

```rust
/// Repair drift: derive a `messages` row for any stored `MessagePosted` event
/// whose `event_hash` has no corresponding `messages` row (e.g. a crash between
/// store_event and derive_message_row). The event log is the source of truth.
/// Returns the number of rows repaired.
pub fn reconcile_messages(conn: &Connection) -> Result<usize> {
    // Collect message-events that lack a derived row.
    let missing: Vec<Vec<u8>> = {
        let mut stmt = conn.prepare(
            "SELECT e.event_body FROM events e \
             LEFT JOIN messages m ON m.event_hash = e.event_hash \
             WHERE e.payload_type = 'MessagePosted' AND m.event_hash IS NULL \
             ORDER BY e.accept_seq ASC",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
        let mut v = Vec::new();
        for row in rows { v.push(row?); }
        v
    };
    let mut repaired = 0;
    for body in missing {
        let event: Event = rmp_serde::from_slice(&body).context("decode event for reconcile")?;
        if derive_message_row(conn, &event)?.is_some() {
            repaired += 1;
        }
    }
    Ok(repaired)
}
```

> If the implementer's Task-2 (sub-3a) avoided `rmp_serde` directly, use `Event::from_bytes(&body)` instead of `rmp_serde::from_slice` — match what `load_events_in_order` does.

Add a test to `event_ingest.rs` `mod tests`:

```rust
    #[test]
    fn reconcile_derives_missing_message_rows() {
        let conn = crate::db::open_in_memory().unwrap();
        let owner = Keypair::generate();
        let dev = Keypair::generate();
        let g = genesis(&owner);
        save_genesis(&conn, &g).unwrap();
        let da = Event::next(&dev, owner.public_key(), g.server_id(), None, 0, 1,
            EP::DeviceAuthorized { cert: DeviceCert::create(&owner, &dev.public_key(), 1) });
        let msg = Event::next(&dev, owner.public_key(), g.server_id(), Some(&da), 1, 2,
            EP::MessagePosted { channel_id: 1, content: "drifted".into(), reply_to: None, attachments: vec![] });
        // Store events but DO NOT derive the message row (simulate the crash window).
        store_event(&conn, &da).unwrap();
        store_event(&conn, &msg).unwrap();
        let before: i64 = conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap();
        assert_eq!(before, 0);
        // Reconcile derives the missing row, and is idempotent (a second run repairs nothing).
        assert_eq!(reconcile_messages(&conn).unwrap(), 1);
        assert_eq!(reconcile_messages(&conn).unwrap(), 0);
        let (content, eh): (String, String) = conn.query_row(
            "SELECT content, event_hash FROM messages", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(content, "drifted");
        assert_eq!(eh, msg.hash());
    }
```

- [ ] **Step 3: Call reconcile on startup**

In `crates/farder-server/src/main.rs`, in the startup block that calls `build_log_state` (added in sub-3a Task 4), after the genesis/log_state are set, add:

```rust
        {
            let conn = state.db.lock().unwrap();
            let repaired = crate::event_ingest::reconcile_messages(&conn).unwrap_or(0);
            if repaired > 0 {
                tracing::info!("reconciled {} event-sourced messages missing from the view", repaired);
            }
        }
```

(Match the binary-crate import prefix the implementer used in sub-3a — `farder_server::event_ingest::` if in `main.rs` of the binary.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p farder-server event_ingest::tests::reconcile_derives_missing_message_rows`
Then: `cargo test -p farder-server` — whole crate green.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-server/src/handlers.rs crates/farder-server/src/event_ingest.rs crates/farder-server/src/main.rs
git commit -m "fix(server): atomic store+derive (tx) + startup messages-from-events reconciliation"
```

---

## Task 2: Client `device.rs` — device subkey, DeviceCert, per-server chain state

**Files:**
- Create: `client/src-tauri/src/device.rs`
- Modify: `client/src-tauri/src/lib.rs` or `main.rs` (`mod device;`)

**Interfaces:**
- Consumes: `farder_crypto::identity::{Keypair, PublicKey}`, `farder_crypto::event_log::{DeviceCert, device_id}`, `crate::commands::farder_data_dir` (the data-dir helper used by settings).
- Produces:
  - `load_or_create_device_keypair() -> Result<Keypair, String>` (persist a per-install device Ed25519 key at `<data_dir>/device.key`, generate on first use)
  - `DeviceState { device_id, next_seq, last_event_hash: Option<String>, lamport }` + `DeviceState::load(server_id) -> Result<Option<Self>, String>` + `save(&self, server_id) -> Result<(), String>`
  - `device_cert(identity: &Keypair, device: &Keypair) -> DeviceCert`

- [ ] **Step 1: Write the module + tests**

Create `client/src-tauri/src/device.rs`:

```rust
//! Per-install device subkey + per-(server, device) chain/clock state for the
//! mesh event log. Events are signed by the DEVICE subkey; the identity key
//! authorizes the device via a DeviceCert.

use std::path::PathBuf;

use farder_crypto::event_log::{device_id, DeviceCert};
use farder_crypto::identity::Keypair;
use serde::{Deserialize, Serialize};

fn device_key_path() -> PathBuf {
    crate::commands::farder_data_dir().join("device.key")
}

/// Load the per-install device signing key, generating + persisting one on first
/// use. Stored as 32 raw bytes (the device subkey is low-stakes — it grants no
/// rights without an identity-signed DeviceCert; it can be regenerated).
pub fn load_or_create_device_keypair() -> Result<Keypair, String> {
    let path = device_key_path();
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() == 32 {
            let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| "bad device key".to_string())?;
            return Ok(Keypair::from_signing_key_bytes(&arr));
        }
    }
    let kp = Keypair::generate();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, kp.signing_key_bytes()).map_err(|e| e.to_string())?;
    Ok(kp)
}

/// The identity authorizes the device subkey.
pub fn device_cert(identity: &Keypair, device: &Keypair) -> DeviceCert {
    DeviceCert::create(identity, &device.public_key(), crate::db_now())
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeviceState {
    pub device_id: String,
    pub next_seq: u64,
    pub last_event_hash: Option<String>,
    pub lamport: u64,
    /// Whether this device has already submitted its DeviceAuthorized to the server.
    pub authorized: bool,
}

impl DeviceState {
    pub fn fresh(device: &Keypair) -> Self {
        Self {
            device_id: device_id(&device.public_key()),
            next_seq: 0,
            last_event_hash: None,
            lamport: 0,
            authorized: false,
        }
    }

    fn path(server_id: &str) -> PathBuf {
        crate::commands::farder_data_dir().join("servers").join(server_id).join("device_state.json")
    }

    pub fn load(server_id: &str) -> Result<Option<Self>, String> {
        let path = Self::path(server_id);
        match std::fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).map(Some).map_err(|e| e.to_string()),
            Err(_) => Ok(None),
        }
    }

    pub fn save(&self, server_id: &str) -> Result<(), String> {
        let path = Self::path(server_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_cert_authorizes_and_verifies() {
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let cert = device_cert(&identity, &device);
        assert!(cert.verify().is_ok());
        assert_eq!(cert.core.identity, identity.public_key());
        assert_eq!(cert.core.device_id, device_id(&device.public_key()));
    }

    #[test]
    fn device_state_fresh_and_serde_roundtrip() {
        let device = Keypair::generate();
        let mut st = DeviceState::fresh(&device);
        assert_eq!(st.next_seq, 0);
        assert!(st.last_event_hash.is_none());
        assert!(!st.authorized);
        st.next_seq = 3;
        st.last_event_hash = Some("abc".into());
        st.lamport = 9;
        st.authorized = true;
        let json = serde_json::to_string(&st).unwrap();
        let back: DeviceState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.next_seq, 3);
        assert_eq!(back.last_event_hash.as_deref(), Some("abc"));
        assert_eq!(back.lamport, 9);
        assert!(back.authorized);
        assert_eq!(back.device_id, device_id(&device.public_key()));
    }
}
```

> `crate::db_now()` / `crate::commands::farder_data_dir`: use the ACTUAL helpers — the recon shows `farder_data_dir()` exists; for a timestamp use whatever the client uses (e.g. `std::time::SystemTime` epoch secs, or an existing `now()` helper). If none, inline `std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()`.

Add `mod device;` to `client/src-tauri/src/lib.rs` (or `main.rs` — match where other client modules are declared).

- [ ] **Step 2: Run tests**

Run: `cd client/src-tauri && cargo test device::`
Expected: 2 tests pass.
Then `cargo build` (client crate compiles).

- [ ] **Step 3: Commit**

```bash
git add client/src-tauri/src/device.rs client/src-tauri/src/lib.rs
git commit -m "feat(client): device subkey + DeviceCert + per-(server,device) chain state"
```

---

## Task 3: Expose `server_id` on connect (protocol + server + client ConnectResult)

**Files:**
- Modify: `crates/farder-protocol/src/server.rs` (add `server_id: Option<String>` to the server-info response)
- Modify: `crates/farder-server/src/handlers.rs` (`GetServerInfo` fills it from `state.genesis`)
- Modify: `client/src-tauri/src/commands.rs` (`ConnectResult.server_id` + populate it on connect)
- Modify: `client/src/lib/types.ts` (`ConnectResult.server_id`)

**Interfaces:**
- Produces: the client learns the connected server's `server_id` (= genesis hash) when it runs the log; `None`/absent for legacy servers.

- [ ] **Step 1: Protocol — add the field**

In `crates/farder-protocol/src/server.rs`, add to the server-info response variant (the one carrying `name`/`member_count`/`channels`...), with `#[serde(default)]` for backward-compat:

```rust
        #[serde(default)]
        server_id: Option<String>,
```

- [ ] **Step 2: Server — fill it**

In the `GetServerInfo` handler in `handlers.rs`, set `server_id` from the in-memory genesis:

```rust
        server_id: state.genesis.lock().unwrap().as_ref().map(|g| g.server_id()),
```

(Match the response-construction site; this is just one added field.)

- [ ] **Step 3: Client — thread it into ConnectResult**

In `client/src-tauri/src/commands.rs`: add `pub server_id: Option<String>` to `ConnectResult`; when unpacking the `ServerInfo` response in `connect_server`, copy `server_id` into the result. In `client/src/lib/types.ts`, add `server_id?: string | null` to the `ConnectResult` interface.

- [ ] **Step 4: Build/check**

Run: `cargo build -p farder-protocol && cargo build -p farder-server && cargo test -p farder-server`
Run: `cd client && npx tsc --noEmit`
Expected: all clean (the field is additive + serde-default).

- [ ] **Step 5: Commit**

```bash
git add crates/farder-protocol/src/server.rs crates/farder-server/src/handlers.rs client/src-tauri/src/commands.rs client/src/lib/types.ts
git commit -m "feat: expose server_id on connect so the client knows a log-mode server"
```

---

## Task 4: `submit_event` Tauri command (RUNTIME-VERIFIED) 

> **Runtime note:** this task compiles + can have a thin unit test for the event-building helper, but the actual submit→accept round-trip needs the owner's Windows run. Mark UNVERIFIED-at-runtime in the report.

**Files:**
- Modify: `client/src-tauri/src/commands.rs` (add `submit_event`)
- Modify: `client/src-tauri/src/main.rs` (register in `generate_handler!`)

**Interfaces:**
- Consumes: `device::{load_or_create_device_keypair, device_cert, DeviceState}`, `AppState.signing_key_bytes`, `bridge::send_request`, `farder_crypto::event_log::{Event, EventPayload}`.
- Produces: `submit_event(state, server_id, channel_id, content, reply_to) -> Result<EventAcceptedResult, String>`.

- [ ] **Step 1: Write the command**

In `client/src-tauri/src/commands.rs`:

```rust
#[derive(serde::Serialize)]
pub struct EventAcceptedResult { pub event_hash: String, pub timestamp: u64 }

#[tauri::command]
pub async fn submit_event(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    server_id: String,
    channel_id: u64,
    content: String,
    reply_to: Option<String>, // event-hash ref; None for top-level (Rung-1 reply mapping is a follow-on)
) -> Result<EventAcceptedResult, String> {
    use farder_crypto::event_log::{Event, EventPayload};

    // Identity (must be unlocked) + device key.
    let identity = {
        let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
        let bytes = lock.ok_or_else(|| "identity is locked".to_string())?;
        farder_crypto::identity::Keypair::from_signing_key_bytes(&bytes)
    };
    let device = crate::device::load_or_create_device_keypair()?;

    // Per-(server, device) chain state.
    let mut ds = crate::device::DeviceState::load(&server_id)?
        .unwrap_or_else(|| crate::device::DeviceState::fresh(&device));

    // 1. First time on this server: authorize the device.
    if !ds.authorized {
        let cert = crate::device::device_cert(&identity, &device);
        let da = Event::next(&device, identity.public_key(), server_id.clone(),
            None, ds.lamport, now_secs(), EventPayload::DeviceAuthorized { cert });
        send_submit(&state, &server_id, &da).await?;
        ds.next_seq = da.core.seq + 1;
        ds.last_event_hash = Some(da.hash());
        ds.lamport = da.core.lamport;
        ds.authorized = true;
        ds.save(&server_id)?;
    }

    // 2. Build + submit the message event, chaining from the stored head.
    let prev_hash = ds.last_event_hash.clone();
    let msg = build_next(&device, &identity, &server_id, prev_hash, ds.next_seq, ds.lamport,
        EventPayload::MessagePosted { channel_id, content, reply_to, attachments: vec![] });
    let result = send_submit(&state, &server_id, &msg).await?;

    // 3. Advance + persist chain state ONLY on confirmed acceptance.
    ds.next_seq = msg.core.seq + 1;
    ds.last_event_hash = Some(msg.hash());
    ds.lamport = msg.core.lamport;
    ds.save(&server_id)?;
    Ok(result)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// Build the next event WITHOUT a prev Event in hand — we only stored the prev hash,
// so construct the core directly (Event::next needs the prev Event for seq+hash; here
// we already know seq + prev_hash from DeviceState, so sign the core ourselves).
fn build_next(
    device: &farder_crypto::identity::Keypair,
    identity: &farder_crypto::identity::Keypair,
    server_id: &str,
    prev: Option<String>,
    seq: u64,
    lamport_observed: u64,
    payload: farder_crypto::event_log::EventPayload,
) -> farder_crypto::event_log::Event {
    use farder_crypto::event_log::{device_id, Event, EventCore};
    let core = EventCore {
        server_id: server_id.to_string(),
        author: identity.public_key(),
        device: device_id(&device.public_key()),
        seq,
        prev,
        lamport: lamport_observed + 1,
        timestamp: now_secs(),
        payload,
    };
    Event::sign(core, device)
}

async fn send_submit(state: &AppState, server_id: &str, event: &farder_crypto::event_log::Event)
    -> Result<EventAcceptedResult, String>
{
    let response = bridge::send_request(state, server_id,
        ServerRequest::SubmitEvent { event: event.clone() }).await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::EventAccepted { event_hash, timestamp } => Ok(EventAcceptedResult { event_hash, timestamp }),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}
```

> **API check:** `EventCore`/`Event::sign` must be `pub` in `farder_crypto::event_log` (they are — used by `Event::next`). If `EventCore` isn't exported, use `Event::next` with a synthesized prev — but the cleaner fix is to confirm `EventCore` + `Event::sign` are `pub` (they are per sub-project 1). Adjust the `bridge::send_request` borrow (it takes `&AppState`; `state` here is `tauri::State<Arc<AppState>>` — deref as the existing `send_message` does).

- [ ] **Step 2: Register the command**

Add `commands::submit_event` to the `generate_handler![ ... ]` list in `client/src-tauri/src/main.rs`.

- [ ] **Step 3: Compile-check**

Run: `cd client/src-tauri && cargo build`
Expected: clean. (The seam is exercised at runtime; a thin unit test of `build_next` producing a verifiable event is optional but encouraged.)

- [ ] **Step 4: Commit**

```bash
git add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs
git commit -m "feat(client): submit_event command — device-authorize + sign + submit message events"
```

---

## Task 5: Frontend routing (RUNTIME-VERIFIED)

> **Runtime note:** compile-checks via `npx tsc --noEmit`; behavior (a sent message appearing over the log) needs the owner's Windows run.

**Files:**
- Modify: `client/src/lib/tauri-bridge.ts` (`submitEvent`)
- Modify: `client/src/components/MessageInput.tsx` (route `handleSend`)

**Interfaces:**
- Consumes: `ConnectResult.server_id` (Task 3) to know the active server is log-mode.

- [ ] **Step 1: Bridge fn**

In `client/src/lib/tauri-bridge.ts`:

```ts
export async function submitEvent(
  serverId: string, channelId: number, content: string, replyTo?: string | null,
): Promise<{ event_hash: string; timestamp: number }> {
  return invoke("submit_event", { serverId, channelId, content, replyTo: replyTo ?? null });
}
```

- [ ] **Step 2: Route the send**

In `client/src/components/MessageInput.tsx` `handleSend`, when the active server has a `server_id` (log-mode), call `submitEvent` instead of `sendMessage`:

```ts
// Pull the active server's server_id from context (set from ConnectResult).
const logMode = !!activeServer?.serverId; // wire serverId through the server-context state
if (logMode) {
  await api.submitEvent(serverId, channelId, messageContent, null);
} else {
  await api.sendMessage(serverId, channelId, messageContent, replyTo, attachmentIds);
}
```

(Thread `server_id` from `ConnectResult` into the per-server context state — follow how `relayed`/`owner_public_key` are stored in `ServerContext`. Attachments + replies over the log are follow-ons, so the log path sends text only for now; if the message has attachments, fall back to `sendMessage` until sub-project 4.)

- [ ] **Step 3: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add client/src/lib/tauri-bridge.ts client/src/components/MessageInput.tsx client/src/context/ServerContext.tsx
git commit -m "feat(client): route message sends to submit_event for log-mode servers"
```

---

## Self-Review

**Spec coverage (sub-project 3b = client posting over the log):**
- §2 server hardening (transaction + startup reconciliation) → Task 1 ✅
- `server_id` exposed on connect → Task 3 ✅
- Device subkey + identity-signed DeviceCert + per-`(server,device)` chain state → Task 2 ✅
- Build+sign events with the device subkey, auto-authorize the device, advance chain only on acceptance → Task 4 ✅
- Frontend routes to the log path for log-mode servers → Task 5 ✅
- M1 device binding honored (device subkey signs; identity authorizes via cert; server validates) ✅
- Deferred + documented: received-event verification (3c), attachments over the log (4), reply mapping, per-channel ACLs ✅

**Verifiability:** Tasks 1–3 are cargo/tsc-verifiable (server tx + reconcile test; device unit tests; protocol/server build + tsc). Tasks 4–5 compile-check but their **end-to-end send→accept→render needs the owner's Windows runtime test** — the dispatch + report must mark them UNVERIFIED-at-runtime.

**Type consistency:** `DeviceState`/`device_cert`/`load_or_create_device_keypair`, `Event::{next,sign}`/`EventCore`, `ServerRequest::SubmitEvent`/`ServerResponse::EventAccepted`, `ConnectResult.server_id` used consistently. `now_secs()` is the single timestamp source on the client send path.

**Integration caveats for the implementer (call out in dispatch):**
- Confirm `farder_crypto::event_log::{EventCore, Event::sign}` are `pub` (sub-project 1) — Task 4 constructs a core directly because only the prev *hash* is persisted, not the prev *Event*.
- Match `farder_data_dir`, the client `now`/timestamp helper, the `mod` declaration site, the `ConnectResult`/`ServerInfo` field-add sites, and how `ServerContext` stores per-server connect metadata.
- The device key file is low-stakes (grants nothing without a cert) — plaintext 32 bytes is acceptable; do not over-engineer encryption here.
