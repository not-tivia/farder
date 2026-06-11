# Profile Sync (Avatars + Custom Status) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Profile pictures (global + per-server override) and a custom status sync to servers as signed profiles and render in the member list, chat, and profile popups.

**Architecture:** The client builds a per-server *effective profile* (override avatar ?? global avatar, global status, current display name), wraps it in the existing `farder_crypto::profile::SignedProfile`, and pushes it post-auth via a new `ServerRequest::UpdateProfile`. The server verifies the signature, stores the serialized blob in the existing `members.avatar` BLOB column plus a new `profile_hash` column, and broadcasts `MemberProfileUpdated`. Member lists carry only the hash; clients fetch full profiles once per hash via `GetMemberProfile`, verify the signature, and disk-cache by hash.

**Tech Stack:** Rust (rmp_serde/MessagePack, rusqlite, sha2/hex, ed25519 via farder-crypto), Tauri commands, React/TypeScript.

**Spec:** `docs/superpowers/specs/2026-06-11-profile-sync-avatar-status-design.md`

**Branch:** create `profile-sync` from `main` before Task 1 (`git checkout -b profile-sync`). Finish with ff-merge to main per project workflow.

---

### Task 1: SignedProfile byte/hash helpers (farder-crypto)

The profile blob that travels and is hashed must have ONE canonical encoding. Put it next to the type.

**Files:**
- Modify: `crates/farder-crypto/src/profile.rs`

- [ ] **Step 1: Write the failing tests** — append inside the existing `mod tests` in `profile.rs`:

```rust
    #[test]
    fn test_profile_bytes_roundtrip() {
        let keypair = Keypair::generate();
        let profile = SignedProfile::create(&keypair, "Alice".to_string(), Some(vec![1, 2, 3]), Some("hi".to_string()));
        let bytes = profile.to_bytes();
        let decoded = SignedProfile::from_bytes(&bytes).unwrap();
        assert!(decoded.verify().is_ok());
        assert_eq!(decoded.display_name(), "Alice");
        assert_eq!(decoded.data.avatar.as_deref(), Some(&[1u8, 2, 3][..]));
    }

    #[test]
    fn test_profile_hash_is_stable_and_changes_with_content() {
        let keypair = Keypair::generate();
        let p1 = SignedProfile::create(&keypair, "Alice".to_string(), None, None);
        let bytes1 = p1.to_bytes();
        assert_eq!(profile_hash_hex(&bytes1), profile_hash_hex(&bytes1));
        assert_eq!(profile_hash_hex(&bytes1).len(), 64);
        let p2 = SignedProfile::create(&keypair, "Alice".to_string(), None, Some("x".to_string()));
        assert_ne!(profile_hash_hex(&bytes1), profile_hash_hex(&p2.to_bytes()));
    }

    #[test]
    fn test_from_bytes_rejects_garbage() {
        assert!(SignedProfile::from_bytes(&[0xFF, 0x00, 0x12]).is_err());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p farder-crypto profile -- --nocapture`
Expected: compile FAILURE (`to_bytes`, `from_bytes`, `profile_hash_hex` not found).

- [ ] **Step 3: Implement** — add to `profile.rs` (sha2 + hex are already deps of farder-crypto):

```rust
use sha2::{Digest, Sha256};

impl SignedProfile {
    // ... existing methods ...

    pub fn to_bytes(&self) -> Vec<u8> {
        rmp_serde::to_vec(self).expect("profile serialization cannot fail")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        rmp_serde::from_slice(bytes).context("failed to decode signed profile")
    }
}

/// Canonical hash of a serialized SignedProfile: SHA-256 hex of the bytes
/// produced by `to_bytes`. Used as the cache key and change detector everywhere.
pub fn profile_hash_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}
```

(Put the `use sha2::...` at the top of the file with the other imports.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p farder-crypto profile`
Expected: all profile tests PASS (3 new + 3 existing).

- [ ] **Step 5: Commit**

```bash
git add crates/farder-crypto/src/profile.rs
git commit -m "crypto: canonical bytes + hash for SignedProfile"
```

---

### Task 2: Protocol additions

**Files:**
- Modify: `crates/farder-protocol/src/server.rs`
- Modify: `crates/farder-server/src/handlers.rs` (3 `MemberInfo` construction sites break — fix in this task so the workspace compiles)

- [ ] **Step 1: Write the failing test** — append inside the existing `mod tests` at the bottom of `crates/farder-protocol/src/server.rs`:

```rust
    #[test]
    fn test_profile_protocol_roundtrip() {
        let kp = farder_crypto::identity::Keypair::generate();
        let req = ClientFrame::Request {
            id: 7,
            body: ServerRequest::UpdateProfile { profile: vec![1, 2, 3] },
        };
        let bytes = rmp_serde::to_vec(&req).unwrap();
        let decoded: ClientFrame = rmp_serde::from_slice(&bytes).unwrap();
        match decoded {
            ClientFrame::Request { id: 7, body: ServerRequest::UpdateProfile { profile } } => {
                assert_eq!(profile, vec![1, 2, 3]);
            }
            other => panic!("unexpected decode: {:?}", other),
        }

        let ev = ServerFrame::Event(ServerEvent::MemberProfileUpdated {
            public_key: kp.public_key(),
            profile_hash: Some("ab".repeat(32)),
        });
        let bytes = rmp_serde::to_vec(&ev).unwrap();
        let _: ServerFrame = rmp_serde::from_slice(&bytes).unwrap();

        // Old MemberInfo encodings (without profile_hash) must still decode.
        let resp = ServerResponse::MemberProfile { member_key: kp.public_key(), profile: None };
        let bytes = rmp_serde::to_vec(&resp).unwrap();
        let _: ServerResponse = rmp_serde::from_slice(&bytes).unwrap();
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p farder-protocol test_profile_protocol_roundtrip`
Expected: compile FAILURE (unknown variants `UpdateProfile`, `MemberProfileUpdated`, `MemberProfile`).

- [ ] **Step 3: Add the protocol surface** in `crates/farder-protocol/src/server.rs`:

To `MemberInfo` (after `timeout_reason`, keeping the `serde(default)` forward-compat pattern):

```rust
    #[serde(default)]
    pub profile_hash: Option<String>,
```

To `ServerRequest` (after `GetMembers`):

```rust
    /// Store the sender's signed profile (serialized `farder_crypto::profile::SignedProfile`).
    UpdateProfile { profile: Vec<u8> },
    /// Fetch a member's stored signed profile blob.
    GetMemberProfile { member_key: PublicKey },
```

To `ServerResponse` (after `Members`):

```rust
    MemberProfile { member_key: PublicKey, profile: Option<Vec<u8>> },
```

To `ServerEvent` (after `MemberTimeoutChanged`):

```rust
    MemberProfileUpdated {
        public_key: PublicKey,
        #[serde(default)]
        profile_hash: Option<String>,
    },
```

- [ ] **Step 4: Fix the three broken `MemberInfo` constructors** in `crates/farder-server/src/handlers.rs` (the new field makes them compile errors):

In `ServerRequest::GetMembers` (~line 1004): MemberRecord doesn't have the hash yet (Task 3); use the placeholder `profile_hash: None,` for now — Task 3 wires the real value.
In `ServerRequest::OpenDm` (~line 1214) and `ServerRequest::ListDms` (~line 1251): same, add `profile_hash: None,`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p farder-protocol test_profile_protocol_roundtrip && cargo build --workspace`
Expected: test PASS, workspace builds (the client crate is a separate workspace — `cd client/src-tauri && cargo build` must also pass; it doesn't construct `MemberInfo` so it should).

- [ ] **Step 6: Commit**

```bash
git add crates/farder-protocol/src/server.rs crates/farder-server/src/handlers.rs
git commit -m "protocol: UpdateProfile/GetMemberProfile requests, MemberProfileUpdated event, MemberInfo.profile_hash"
```

---

### Task 3: Server storage (db migration + members.rs)

**Files:**
- Modify: `crates/farder-server/src/db.rs`
- Modify: `crates/farder-server/src/members.rs`

- [ ] **Step 1: Write the failing tests** — append inside `mod tests` in `members.rs`:

```rust
    #[test]
    fn test_set_and_get_member_profile() {
        let conn = db::open_in_memory().unwrap();
        let pk = gen_pk();
        register_member(&conn, &pk, "Alice").unwrap();

        // No profile initially.
        assert!(get_member_profile(&conn, &pk).unwrap().is_none());
        assert!(get_member(&conn, &pk).unwrap().unwrap().profile_hash.is_none());

        let blob = vec![9u8, 8, 7];
        set_member_profile(&conn, &pk, &blob, "deadbeef").unwrap();

        assert_eq!(get_member_profile(&conn, &pk).unwrap().as_deref(), Some(&blob[..]));
        assert_eq!(
            get_member(&conn, &pk).unwrap().unwrap().profile_hash.as_deref(),
            Some("deadbeef")
        );
        // list_members carries the hash too.
        let listed = list_members(&conn).unwrap();
        assert_eq!(listed[0].profile_hash.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn test_get_member_profile_unknown_member_is_none() {
        let conn = db::open_in_memory().unwrap();
        assert!(get_member_profile(&conn, &gen_pk()).unwrap().is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p farder-server members::tests::test_set_and_get_member_profile`
Expected: compile FAILURE (`set_member_profile` not found, no `profile_hash` field).

- [ ] **Step 3: Add the migration** in `db.rs` `init_schema`, after the `timeout_until` migration block, following the established pragma-check pattern:

```rust
    // Members: profile_hash column for signed-profile sync. The pre-existing
    // (never used) `avatar` BLOB column now stores the member's serialized
    // SignedProfile; profile_hash is its SHA-256 hex (cache key for clients).
    let has_profile_hash: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(members)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        cols.iter().any(|c| c == "profile_hash")
    };
    if !has_profile_hash {
        conn.execute("ALTER TABLE members ADD COLUMN profile_hash TEXT NULL", [])?;
    }
```

- [ ] **Step 4: Extend members.rs.** Add `pub profile_hash: Option<String>` to `MemberRecord`. Update BOTH queries that build it:

`get_member`: SELECT becomes `"SELECT public_key, display_name, joined_at, banned, revoked, profile_hash FROM members WHERE public_key = ?1"`; the row closure gains `let profile_hash: Option<String> = row.get(5)?;`, the tuple gains it, and the `MemberRecord` literal gains `profile_hash`.

`list_members`: same change (`SELECT ... , profile_hash FROM members WHERE banned = 0 AND revoked = 0`, `row.get(5)?`, field in the literal).

Then add the two operations (next to `register_member`):

```rust
pub fn set_member_profile(conn: &Connection, pk: &PublicKey, profile: &[u8], hash: &str) -> Result<()> {
    conn.execute(
        "UPDATE members SET avatar = ?2, profile_hash = ?3 WHERE public_key = ?1",
        params![pk.as_bytes().as_slice(), profile, hash],
    )?;
    Ok(())
}

pub fn get_member_profile(conn: &Connection, pk: &PublicKey) -> Result<Option<Vec<u8>>> {
    let blob: Option<Option<Vec<u8>>> = conn
        .query_row(
            "SELECT avatar FROM members WHERE public_key = ?1",
            params![pk.as_bytes().as_slice()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(blob.flatten())
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p farder-server members::`
Expected: ALL members tests PASS (new + existing).

- [ ] **Step 6: Commit**

```bash
git add crates/farder-server/src/db.rs crates/farder-server/src/members.rs
git commit -m "server: store signed profile blob + hash on members"
```

---

### Task 4: Server handlers (UpdateProfile / GetMemberProfile / hash in GetMembers)

**Files:**
- Modify: `crates/farder-server/src/handlers.rs`

- [ ] **Step 1: Write the failing tests** — append inside `mod tests` in `handlers.rs` (reuse the existing `setup`/`add_member`/`fake_state` helpers):

```rust
    fn make_profile(kp: &farder_crypto::identity::Keypair, status: Option<&str>) -> Vec<u8> {
        farder_crypto::profile::SignedProfile::create(
            kp, "Tester".to_string(), None, status.map(|s| s.to_string()),
        ).to_bytes()
    }

    #[test]
    fn test_update_profile_stores_and_broadcasts() {
        let (conn, _owner_pk) = setup();
        let kp = farder_crypto::identity::Keypair::generate();
        members::register_member(&conn, &kp.public_key(), "Tester").unwrap();

        let blob = make_profile(&kp, Some("hello"));
        let expected_hash = farder_crypto::profile::profile_hash_hex(&blob);

        let result = handle_request(
            &conn, &kp.public_key(), false,
            ServerRequest::UpdateProfile { profile: blob.clone() },
            "", &fake_state(),
        ).unwrap();

        assert!(matches!(result.response, ServerResponse::Ok));
        assert_eq!(result.events.len(), 1);
        match &result.events[0].event {
            ServerEvent::MemberProfileUpdated { public_key, profile_hash } => {
                assert_eq!(public_key, &kp.public_key());
                assert_eq!(profile_hash.as_deref(), Some(expected_hash.as_str()));
            }
            other => panic!("expected MemberProfileUpdated, got {:?}", other),
        }
        assert_eq!(members::get_member_profile(&conn, &kp.public_key()).unwrap().as_deref(), Some(&blob[..]));
    }

    #[test]
    fn test_update_profile_rejects_wrong_key() {
        let (conn, _owner_pk) = setup();
        let kp_signer = farder_crypto::identity::Keypair::generate();
        let mallory = add_member(&conn, "Mallory");
        let blob = make_profile(&kp_signer, None);

        // Mallory (authenticated) submits a profile signed by someone else.
        let result = handle_request(
            &conn, &mallory, false,
            ServerRequest::UpdateProfile { profile: blob },
            "", &fake_state(),
        ).unwrap();
        assert!(matches!(result.response, ServerResponse::Error { .. }));
        assert!(members::get_member_profile(&conn, &mallory).unwrap().is_none());
    }

    #[test]
    fn test_update_profile_rejects_tampered_and_oversize_status() {
        let (conn, _owner_pk) = setup();
        let kp = farder_crypto::identity::Keypair::generate();
        members::register_member(&conn, &kp.public_key(), "Tester").unwrap();

        // Tampered bytes: flip a byte mid-blob.
        let mut blob = make_profile(&kp, Some("ok"));
        let mid = blob.len() / 2;
        blob[mid] ^= 0xFF;
        let result = handle_request(
            &conn, &kp.public_key(), false,
            ServerRequest::UpdateProfile { profile: blob },
            "", &fake_state(),
        ).unwrap();
        assert!(matches!(result.response, ServerResponse::Error { .. }));

        // 129-char status.
        let long = "x".repeat(129);
        let blob = make_profile(&kp, Some(&long));
        let result = handle_request(
            &conn, &kp.public_key(), false,
            ServerRequest::UpdateProfile { profile: blob },
            "", &fake_state(),
        ).unwrap();
        assert!(matches!(result.response, ServerResponse::Error { .. }));
    }

    #[test]
    fn test_get_member_profile_roundtrip_and_members_hash() {
        let (conn, owner_pk) = setup();
        let kp = farder_crypto::identity::Keypair::generate();
        members::register_member(&conn, &kp.public_key(), "Tester").unwrap();
        let blob = make_profile(&kp, None);
        let hash = farder_crypto::profile::profile_hash_hex(&blob);
        handle_request(
            &conn, &kp.public_key(), false,
            ServerRequest::UpdateProfile { profile: blob.clone() },
            "", &fake_state(),
        ).unwrap();

        // Another member fetches it.
        let result = handle_request(
            &conn, &owner_pk, true,
            ServerRequest::GetMemberProfile { member_key: kp.public_key() },
            "", &fake_state(),
        ).unwrap();
        match result.response {
            ServerResponse::MemberProfile { member_key, profile } => {
                assert_eq!(member_key, kp.public_key());
                assert_eq!(profile.as_deref(), Some(&blob[..]));
            }
            other => panic!("expected MemberProfile, got {:?}", other),
        }

        // GetMembers carries the hash.
        let result = handle_request(
            &conn, &owner_pk, true, ServerRequest::GetMembers, "", &fake_state(),
        ).unwrap();
        match result.response {
            ServerResponse::Members { members: infos } => {
                let me = infos.iter().find(|m| m.public_key == kp.public_key()).unwrap();
                assert_eq!(me.profile_hash.as_deref(), Some(hash.as_str()));
            }
            other => panic!("expected Members, got {:?}", other),
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p farder-server test_update_profile`
Expected: FAIL — `UpdateProfile` match arm missing (non-exhaustive match compile error is the expected failure mode here).

- [ ] **Step 3: Implement the handlers** in `handle_request`, in the "Info queries" section after `GetMembers`:

```rust
        ServerRequest::UpdateProfile { profile } => {
            // 2.5 MB ceiling on the whole signed blob (avatar cap is 2 MB inside).
            if profile.len() > 2_621_440 {
                return err("profile too large (max 2.5 MB)");
            }
            let signed = match farder_crypto::profile::SignedProfile::from_bytes(&profile) {
                Ok(p) => p,
                Err(_) => return err("malformed profile"),
            };
            if signed.verify().is_err() {
                return err("profile signature invalid");
            }
            if &signed.data.public_key != member {
                return err("profile public key does not match authenticated member");
            }
            if let Some(status) = &signed.data.status {
                if status.chars().count() > 128 {
                    return err("status too long (max 128 characters)");
                }
            }
            if let Some(avatar) = &signed.data.avatar {
                if let Err(e) = crate::image_validation::validate_image(avatar, true) {
                    return err(&format!("avatar rejected: {}", e));
                }
            }

            let hash = attachments::compute_sha256(&profile);
            members::set_member_profile(conn, member, &profile, &hash)?;
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::MemberProfileUpdated {
                        public_key: member.clone(),
                        profile_hash: Some(hash),
                    },
                }],
            )
        }

        ServerRequest::GetMemberProfile { member_key } => {
            let profile = members::get_member_profile(conn, &member_key)?;
            ok(ServerResponse::MemberProfile { member_key, profile })
        }
```

(Match the exact names of the local helpers in handlers.rs: `err(...)`, `ok(...)`, `ok_with(...)`, `BroadcastEvent`, `EventTarget` are all already in scope/imported there. `attachments::compute_sha256` exists at `attachments.rs:15` and equals `profile_hash_hex` — both SHA-256 hex.)

- [ ] **Step 4: Wire the real hash into the three placeholder sites from Task 2.**

`GetMembers` (~line 1004): the loop already has `m: MemberRecord` — replace `profile_hash: None,` with `profile_hash: m.profile_hash.clone(),` (place the `MemberInfo` field assignment before `m.public_key`/`m.display_name` moves, or bind it first — simplest is `profile_hash: m.profile_hash.clone(),` listed BEFORE the moves of other `m` fields in the literal, which Rust allows in any order as long as the clone happens; if the borrow checker complains, bind `let profile_hash = m.profile_hash.clone();` above the literal).

`OpenDm` (~line 1214): replace with `profile_hash: target_record.profile_hash.clone(),`.
`ListDms` (~line 1251): replace with `profile_hash: other_record.profile_hash.clone(),`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p farder-server`
Expected: ALL server tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/farder-server/src/handlers.rs
git commit -m "server: verify+store signed profiles, serve them, broadcast updates"
```

---

### Task 5: Client Rust — profile build + push logic

**Files:**
- Create: `client/src-tauri/src/profile_sync.rs`
- Modify: `client/src-tauri/src/main.rs` (add `mod profile_sync;` and register new commands)
- Modify: `client/src-tauri/src/commands.rs` (`set_avatar` pushes; `connect_server` pushes post-connect; new status/override commands)

- [ ] **Step 1: Create `profile_sync.rs` with unit tests.** Full file:

```rust
//! Builds the per-server effective signed profile and pushes it to servers.
//!
//! Effective profile = (per-server avatar override ?? global avatar.png),
//! global status, current display name — signed fresh per push. A hash of the
//! last successfully pushed profile is tracked per server in
//! `pushed_profiles.json` so reconnects don't re-upload unchanged profiles.

use std::path::PathBuf;
use std::sync::Arc;

use farder_crypto::identity::Keypair;
use farder_crypto::profile::{profile_hash_hex, SignedProfile};
use farder_protocol::server::{ServerRequest, ServerResponse};

use crate::commands::farder_data_dir;
use crate::state::AppState;

fn overrides_dir() -> PathBuf {
    let d = farder_data_dir().join("profile_overrides");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn safe_server_name(server_id: &str) -> String {
    server_id.replace([':', '.', '/'], "_")
}

pub(crate) fn override_path(server_id: &str) -> PathBuf {
    overrides_dir().join(format!("{}.img", safe_server_name(server_id)))
}

/// Override avatar if set for this server, else the global avatar, else none.
pub(crate) fn effective_avatar_bytes(server_id: &str) -> Option<Vec<u8>> {
    if let Ok(bytes) = std::fs::read(override_path(server_id)) {
        return Some(bytes);
    }
    std::fs::read(farder_data_dir().join("avatar.png")).ok()
}

fn read_profile_field(field: &str) -> Option<String> {
    let data = std::fs::read_to_string(farder_data_dir().join("profile.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v[field].as_str().map(|s| s.to_string())
}

pub(crate) fn build_signed_profile(keypair: &Keypair, server_id: &str) -> SignedProfile {
    let display_name = read_profile_field("display_name").unwrap_or_else(|| "Anonymous".to_string());
    let status = read_profile_field("status").filter(|s| !s.is_empty());
    SignedProfile::create(keypair, display_name, effective_avatar_bytes(server_id), status)
}

// --- last-pushed-hash tracking -------------------------------------------

fn pushed_map_path() -> PathBuf {
    farder_data_dir().join("pushed_profiles.json")
}

fn read_pushed_map() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(pushed_map_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn pushed_hash(server_id: &str) -> Option<String> {
    read_pushed_map().get(server_id)?.as_str().map(|s| s.to_string())
}

fn record_pushed(server_id: &str, hash: &str) {
    let mut map = read_pushed_map();
    map.insert(server_id.to_string(), serde_json::json!(hash));
    let _ = std::fs::write(pushed_map_path(), serde_json::Value::Object(map).to_string());
}

// --- pushing ---------------------------------------------------------------

/// Push the effective profile to one connected server if it changed since the
/// last successful push. No-op when the identity is locked.
pub(crate) async fn push_profile(state: &AppState, server_id: &str) -> Result<(), String> {
    let keypair = {
        let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
        match lock.as_ref() {
            Some(bytes) => Keypair::from_signing_key_bytes(bytes),
            None => return Ok(()), // locked — nothing to push yet
        }
    };
    let bytes = build_signed_profile(&keypair, server_id).to_bytes();
    let hash = profile_hash_hex(&bytes);
    if pushed_hash(server_id).as_deref() == Some(hash.as_str()) {
        return Ok(());
    }
    match crate::bridge::send_request(state, server_id, ServerRequest::UpdateProfile { profile: bytes }).await {
        Ok(ServerResponse::Ok) => {
            record_pushed(server_id, &hash);
            Ok(())
        }
        Ok(ServerResponse::Error { reason }) => Err(reason),
        Ok(other) => Err(format!("unexpected response: {:?}", other)),
        Err(e) => Err(e.to_string()),
    }
}

/// Push to every connected server (each gets its own effective profile, so
/// per-server overrides are respected automatically).
pub(crate) async fn push_profile_everywhere(state: &Arc<AppState>) {
    let ids: Vec<String> = state.servers.lock().unwrap().keys().cloned().collect();
    for id in ids {
        if let Err(e) = push_profile(state, &id).await {
            eprintln!("[profile-sync] push to {} failed: {}", id, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // One combined test: FARDER_DATA is process-global, so parallel tests would
    // race if split. Everything filesystem-dependent lives here.
    #[test]
    fn test_override_resolution_and_pushed_map() {
        let tmp = std::env::temp_dir().join(format!("farder-profile-sync-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("FARDER_DATA", &tmp);

        // No avatars at all -> None.
        assert!(effective_avatar_bytes("srv-a").is_none());

        // Global only -> global for every server.
        std::fs::write(tmp.join("avatar.png"), [1u8, 2, 3]).unwrap();
        assert_eq!(effective_avatar_bytes("srv-a").as_deref(), Some(&[1u8, 2, 3][..]));
        assert_eq!(effective_avatar_bytes("srv-b").as_deref(), Some(&[1u8, 2, 3][..]));

        // Override on srv-a wins there, global elsewhere.
        std::fs::write(override_path("srv-a"), [9u8]).unwrap();
        assert_eq!(effective_avatar_bytes("srv-a").as_deref(), Some(&[9u8][..]));
        assert_eq!(effective_avatar_bytes("srv-b").as_deref(), Some(&[1u8, 2, 3][..]));

        // Signed profile picks up display name + status + per-server avatar.
        std::fs::write(
            tmp.join("profile.json"),
            r#"{"display_name":"Tester","status":"hi there"}"#,
        ).unwrap();
        let kp = Keypair::generate();
        let p = build_signed_profile(&kp, "srv-a");
        assert!(p.verify().is_ok());
        assert_eq!(p.display_name(), "Tester");
        assert_eq!(p.data.status.as_deref(), Some("hi there"));
        assert_eq!(p.data.avatar.as_deref(), Some(&[9u8][..]));

        // Pushed-map roundtrip.
        assert!(pushed_hash("srv-a").is_none());
        record_pushed("srv-a", "abc123");
        assert_eq!(pushed_hash("srv-a").as_deref(), Some("abc123"));
        record_pushed("srv-a", "def456");
        assert_eq!(pushed_hash("srv-a").as_deref(), Some("def456"));

        std::env::remove_var("FARDER_DATA");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
```

- [ ] **Step 2: Register the module.** In `client/src-tauri/src/main.rs`, add `mod profile_sync;` alongside the other `mod` declarations.

- [ ] **Step 3: Run the test**

Run: `cd client/src-tauri && cargo test profile_sync::`
Expected: PASS. (If `farder_data_dir` or `send_request` visibility errors appear: `farder_data_dir` is already `pub(crate)`; make `bridge::send_request` `pub(crate)` if it isn't already `pub`.)

- [ ] **Step 4: New status/override Tauri commands** in `commands.rs` (place after `get_server_avatar`). Also add the shared data-url helper and modify `set_avatar`:

```rust
/// Build a data: URL for raw image bytes, sniffing the mime from magic bytes.
pub(crate) fn image_data_url(data: &[u8]) -> String {
    let mime = if data.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        "image/png"
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if data.starts_with(b"GIF8") {
        "image/gif"
    } else if data.len() > 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "application/octet-stream"
    };
    use base64::Engine;
    format!("data:{};base64,{}", mime, base64::engine::general_purpose::STANDARD.encode(data))
}

#[tauri::command]
pub fn get_profile_status() -> Option<String> {
    let data = std::fs::read_to_string(profile_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v["status"].as_str().map(|s| s.to_string())
}

#[tauri::command]
pub async fn set_profile_status(
    state: State<'_, Arc<AppState>>,
    status: Option<String>,
) -> Result<(), String> {
    let trimmed = status.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(s) = &trimmed {
        if s.chars().count() > 128 {
            return Err("status too long (max 128 characters)".to_string());
        }
    }
    let path = profile_path();
    let mut data: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    match &trimmed {
        Some(s) => data["status"] = serde_json::json!(s),
        None => data["status"] = serde_json::Value::Null,
    }
    std::fs::write(&path, data.to_string()).map_err(|e| e.to_string())?;
    crate::profile_sync::push_profile_everywhere(state.inner()).await;
    Ok(())
}

#[tauri::command]
pub async fn set_server_avatar_override(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    file_path: String,
) -> Result<String, String> {
    let data = std::fs::read(&file_path).map_err(|e| e.to_string())?;
    std::fs::write(crate::profile_sync::override_path(&server_id), &data).map_err(|e| e.to_string())?;
    let _ = crate::profile_sync::push_profile(&state, &server_id).await;
    Ok(image_data_url(&data))
}

#[tauri::command]
pub async fn clear_server_avatar_override(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<(), String> {
    let _ = std::fs::remove_file(crate::profile_sync::override_path(&server_id));
    let _ = crate::profile_sync::push_profile(&state, &server_id).await;
    Ok(())
}

#[tauri::command]
pub fn get_server_avatar_override(server_id: String) -> Option<String> {
    let data = std::fs::read(crate::profile_sync::override_path(&server_id)).ok()?;
    Some(image_data_url(&data))
}
```

Note: `push_profile` takes `&AppState`; `State<'_, Arc<AppState>>` derefs — `&state` coerces via two derefs; if inference complains use `state.inner().as_ref()`.

- [ ] **Step 5: Make `set_avatar` push.** Change its signature and tail (keep the local write + data-url return; reuse the new sniffing helper):

```rust
#[tauri::command]
pub async fn set_avatar(state: State<'_, Arc<AppState>>, file_path: String) -> Result<String, String> {
    let data = std::fs::read(&file_path).map_err(|e| e.to_string())?;
    let avatar_path = farder_data_dir().join("avatar.png");
    std::fs::write(&avatar_path, &data).map_err(|e| e.to_string())?;
    crate::profile_sync::push_profile_everywhere(state.inner()).await;
    Ok(image_data_url(&data))
}
```

(The old hand-rolled extension-based mime block is replaced by `image_data_url`; the frontend call signature is unchanged — `state` is injected by Tauri.)

- [ ] **Step 6: Push after connect.** In `connect_server` (`commands.rs` ~line 565), inside the `ServerResponse::ServerInfo` success arm, immediately BEFORE `Ok(ConnectResult { ... })`:

```rust
            // Sync our signed profile to this server in the background.
            {
                let state_arc: Arc<AppState> = Arc::clone(state.inner());
                let sid = address.clone();
                tokio::spawn(async move {
                    if let Err(e) = crate::profile_sync::push_profile(&state_arc, &sid).await {
                        eprintln!("[profile-sync] push after connect to {} failed: {}", sid, e);
                    }
                });
            }
```

- [ ] **Step 7: Register the new commands** in the `generate_handler![...]` list in `client/src-tauri/src/main.rs`: `get_profile_status`, `set_profile_status`, `set_server_avatar_override`, `clear_server_avatar_override`, `get_server_avatar_override` (set_avatar is already there).

- [ ] **Step 8: Build + test**

Run: `cd client/src-tauri && cargo build && cargo test profile_sync::`
Expected: builds clean, test passes.

- [ ] **Step 9: Commit**

```bash
git add client/src-tauri/src/profile_sync.rs client/src-tauri/src/commands.rs client/src-tauri/src/main.rs
git commit -m "client: build, sign, and push effective profiles (global+override avatar, status)"
```

---

### Task 6: Client Rust — fetch + cache member profiles, bridge event

**Files:**
- Modify: `client/src-tauri/src/commands.rs` (new `get_member_profile` command)
- Modify: `client/src-tauri/src/bridge.rs` (dispatch `MemberProfileUpdated`)
- Modify: `client/src-tauri/src/main.rs` (register command)

- [ ] **Step 1: Add the fetch command** in `commands.rs` (after `get_server_avatar_override`). It verifies signature + key + hash BEFORE caching; the disk cache is immutable (hash-keyed):

```rust
#[derive(serde::Serialize)]
pub struct MemberProfileView {
    pub avatar_data_url: Option<String>,
    pub status: Option<String>,
}

/// Resolve a member's profile by its hash: disk cache first, otherwise fetch
/// from the server and verify (signature, key match, hash match) before caching.
/// LAZY ONLY — never call at module load (PIN-lock; see eb1511d lesson).
#[tauri::command]
pub async fn get_member_profile(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    public_key: String,
    profile_hash: Option<String>,
) -> Result<Option<MemberProfileView>, String> {
    use farder_crypto::profile::{profile_hash_hex, SignedProfile};

    let Some(hash) = profile_hash else { return Ok(None) };
    // The hash is used as a filename — accept only 64 hex chars.
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let cache_dir = farder_data_dir().join("profile_cache");
    let _ = std::fs::create_dir_all(&cache_dir);
    let cache_path = cache_dir.join(&hash);

    let bytes = match std::fs::read(&cache_path) {
        Ok(b) => b,
        Err(_) => {
            let pk = parse_public_key(&public_key)?;
            let response = bridge::send_request(
                &state, &server_id,
                ServerRequest::GetMemberProfile { member_key: pk.clone() },
            ).await.map_err(|e| e.to_string())?;
            let bytes = match response {
                ServerResponse::MemberProfile { profile: Some(b), .. } => b,
                ServerResponse::MemberProfile { profile: None, .. } => return Ok(None),
                ServerResponse::Error { reason } => return Err(reason),
                other => return Err(format!("unexpected response: {:?}", other)),
            };
            let signed = SignedProfile::from_bytes(&bytes).map_err(|e| e.to_string())?;
            signed.verify().map_err(|_| "profile signature invalid".to_string())?;
            if signed.data.public_key != pk {
                return Err("profile public key mismatch".to_string());
            }
            if profile_hash_hex(&bytes) != hash {
                return Err("profile hash mismatch".to_string());
            }
            let _ = std::fs::write(&cache_path, &bytes);
            bytes
        }
    };

    let signed = SignedProfile::from_bytes(&bytes).map_err(|e| e.to_string())?;
    Ok(Some(MemberProfileView {
        avatar_data_url: signed.data.avatar.as_deref().map(image_data_url),
        status: signed.data.status,
    }))
}
```

(`parse_public_key` already exists at `commands.rs:1509` — reuse it, don't redefine.)

- [ ] **Step 2: Bridge the event.** In `client/src-tauri/src/bridge.rs` `dispatch_event`, add an arm next to `MemberTimeoutChanged`:

```rust
        ServerEvent::MemberProfileUpdated { public_key, profile_hash } =>
            app.emit("server:member_profile_updated", serde_json::json!({ "server_id": sid, "public_key": public_key.to_string(), "profile_hash": profile_hash })),
```

- [ ] **Step 3: Register `get_member_profile`** in `generate_handler![...]` in `main.rs`.

- [ ] **Step 4: Build**

Run: `cd client/src-tauri && cargo build`
Expected: clean build. Also verify the seam: `grep -n "get_member_profile\|set_profile_status\|set_server_avatar_override\|clear_server_avatar_override\|get_server_avatar_override\|get_profile_status" client/src-tauri/src/main.rs` shows all six registered.

- [ ] **Step 5: Commit**

```bash
git add client/src-tauri/src/commands.rs client/src-tauri/src/bridge.rs client/src-tauri/src/main.rs
git commit -m "client: fetch+verify+cache member profiles, bridge profile-updated event"
```

---

### Task 7: Frontend — types, bridge functions, event listener, hook

**Files:**
- Modify: `client/src/lib/types.ts`
- Modify: `client/src/lib/tauri-bridge.ts`
- Modify: `client/src/hooks/useServerEvents.ts`
- Create: `client/src/hooks/useMemberProfile.ts`

- [ ] **Step 1: Extend `MemberInfo`** in `types.ts`:

```ts
export interface MemberInfo {
  public_key: { bytes: number[] };
  display_name: string;
  joined_at: number;
  role_ids: number[];
  timeout_until?: number | null;
  timeout_reason?: string | null;
  profile_hash?: string | null;
}
```

- [ ] **Step 2: Bridge functions** in `tauri-bridge.ts` (next to the existing avatar functions):

```ts
export interface MemberProfileView {
  avatar_data_url: string | null;
  status: string | null;
}

export async function getMemberProfile(serverId: string, publicKey: string, profileHash: string): Promise<MemberProfileView | null> {
  return invoke<MemberProfileView | null>("get_member_profile", { serverId, publicKey, profileHash });
}

export async function getProfileStatus(): Promise<string | null> {
  return invoke<string | null>("get_profile_status");
}

export async function setProfileStatus(status: string | null): Promise<void> {
  return invoke<void>("set_profile_status", { status });
}

export async function setServerAvatarOverride(serverId: string, filePath: string): Promise<string> {
  return invoke<string>("set_server_avatar_override", { serverId, filePath });
}

export async function clearServerAvatarOverride(serverId: string): Promise<void> {
  return invoke<void>("clear_server_avatar_override", { serverId });
}

export async function getServerAvatarOverride(serverId: string): Promise<string | null> {
  return invoke<string | null>("get_server_avatar_override", { serverId });
}
```

- [ ] **Step 3: Event listener** in `useServerEvents.ts`, next to the `server:member_joined` listener (same refetch pattern):

```ts
    listen("server:member_profile_updated", (e) => {
      const data = e.payload as { server_id: string };
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      api.getMembers(serverId).then(members => {
        dispatch({ type: "SET_MEMBERS", serverId, payload: members });
      }).catch((err) => console.error("[members] refresh on profile update failed:", err));
    }).then(safePush);
```

- [ ] **Step 4: Create `client/src/hooks/useMemberProfile.ts`** — full file:

```ts
import { useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";

export interface MemberProfile {
  avatarUrl: string | null;
  status: string | null;
}

const EMPTY: MemberProfile = { avatarUrl: null, status: null };

// Profiles are immutable per hash, so a module-level cache is safe: each hash
// resolves over the wire at most once per app session.
const cache = new Map<string, MemberProfile>();
const pending = new Map<string, Promise<MemberProfile>>();

export function useMemberProfile(
  serverId: string,
  publicKey: string,
  profileHash?: string | null,
): MemberProfile {
  const [profile, setProfile] = useState<MemberProfile>(
    profileHash ? cache.get(profileHash) ?? EMPTY : EMPTY,
  );

  useEffect(() => {
    if (!profileHash) { setProfile(EMPTY); return; }
    const hit = cache.get(profileHash);
    if (hit) { setProfile(hit); return; }
    let cancelled = false;
    let p = pending.get(profileHash);
    if (!p) {
      p = api.getMemberProfile(serverId, publicKey, profileHash)
        .then((v): MemberProfile => {
          const result: MemberProfile = v
            ? { avatarUrl: v.avatar_data_url ?? null, status: v.status ?? null }
            : EMPTY;
          cache.set(profileHash, result);
          pending.delete(profileHash);
          return result;
        })
        .catch((): MemberProfile => { pending.delete(profileHash); return EMPTY; });
      pending.set(profileHash, p);
    }
    p.then(r => { if (!cancelled) setProfile(r); });
    return () => { cancelled = true; };
  }, [serverId, publicKey, profileHash]);

  return profile;
}
```

- [ ] **Step 5: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add client/src/lib/types.ts client/src/lib/tauri-bridge.ts client/src/hooks/useServerEvents.ts client/src/hooks/useMemberProfile.ts
git commit -m "client ui: profile bridge functions, hash-cached useMemberProfile hook, live update listener"
```

---

### Task 8: Frontend rendering — shared avatar, member list, chat, profile popup

**Files:**
- Create: `client/src/components/MemberAvatar.tsx`
- Modify: `client/src/components/MemberSidebar.tsx`
- Modify: `client/src/components/Message.tsx` (~line 321)
- Modify: `client/src/components/UserProfilePopup.tsx`

- [ ] **Step 1: Create `MemberAvatar.tsx`** — one component for every letter-circle site; image when a profile avatar exists, letter fallback otherwise:

```tsx
import { useMemberProfile } from "../hooks/useMemberProfile";

interface Props {
  serverId: string;
  publicKey?: string;            // omit when unknown -> always letter fallback
  profileHash?: string | null;
  name: string;
  className: string;             // keeps each site's existing class (member-avatar-mini, message-avatar, ...)
}

export default function MemberAvatar({ serverId, publicKey, profileHash, name, className }: Props) {
  const { avatarUrl } = useMemberProfile(serverId, publicKey ?? "", publicKey ? profileHash : null);
  return (
    <span className={className}>
      {avatarUrl
        ? <img className="avatar-img" src={avatarUrl} alt="" />
        : (name || "?").charAt(0).toUpperCase()}
    </span>
  );
}
```

- [ ] **Step 2: Member list.** In `MemberSidebar.tsx`, hooks can't run inside `.map`, so extract the row into a component in the same file. Replace the `sortedMembers.map(...)` body's avatar/name block:

Add imports: `import MemberAvatar from "./MemberAvatar";` and `import { useMemberProfile } from "../hooks/useMemberProfile";`

Add above `export default function MemberSidebar()`:

```tsx
function MemberRow({ member, serverId, showModBadges, onClick, onContextMenu }: {
  member: MemberInfo;
  serverId: string;
  showModBadges: boolean;
  onClick: (e: React.MouseEvent) => void;
  onContextMenu: (e: React.MouseEvent) => void;
}) {
  const pkStr = publicKeyToString(member.public_key);
  const { status } = useMemberProfile(serverId, pkStr, member.profile_hash);
  return (
    <div className="member-item" onClick={onClick} onContextMenu={onContextMenu}>
      <MemberAvatar
        className="member-avatar-mini"
        serverId={serverId}
        publicKey={pkStr}
        profileHash={member.profile_hash}
        name={member.display_name}
      />
      <span className="online-dot" />
      <span className="member-text">
        <span className="member-name">{member.display_name}</span>
        {status && <span className="member-status">{status}</span>}
      </span>
      {showModBadges && (
        <TimedOutBadge untilMs={member.timeout_until} reason={member.timeout_reason} />
      )}
    </div>
  );
}
```

And the map becomes (serverId is already in scope; skip rendering rows when `serverId` is null):

```tsx
        {serverId && sortedMembers.map((member) => (
          <MemberRow
            key={member.public_key.bytes.join(",")}
            member={member}
            serverId={serverId}
            showModBadges={showModBadges}
            onClick={(e) => setProfilePopup({ member, x: e.clientX, y: e.clientY })}
            onContextMenu={(e) => {
              e.preventDefault();
              setContextMenu({ target: member, position: { x: e.clientX, y: e.clientY } });
            }}
          />
        ))}
```

- [ ] **Step 3: Chat messages.** In `Message.tsx` (~line 321) replace:

```tsx
          <span className="message-avatar">{(displayName || "?").charAt(0).toUpperCase()}</span>
```

with (Message.tsx already has `serverId`, `member` — which may be undefined — and `pkStr` in scope; check the surrounding code for the exact variable carrying the author's pk string and use that):

```tsx
          <MemberAvatar
            className="message-avatar"
            serverId={serverId}
            publicKey={member ? pkStr : undefined}
            profileHash={member?.profile_hash}
            name={displayName || "?"}
          />
```

Add the import: `import MemberAvatar from "./MemberAvatar";`

- [ ] **Step 4: Profile popup.** In `UserProfilePopup.tsx`:

(a) Resolve the remote profile at the top of the component:

```tsx
  const { avatarUrl: remoteAvatarUrl, status: remoteStatus } = useMemberProfile(serverId, pkStr, member.profile_hash);
```

(b) Own-profile state additions:

```tsx
  const [overrideUrl, setOverrideUrl] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [editingStatus, setEditingStatus] = useState(false);
  const [statusInput, setStatusInput] = useState("");
```

and extend the existing `isSelf` effect:

```tsx
      api.getServerAvatarOverride(serverId).then(url => { if (url) setOverrideUrl(url); });
      api.getProfileStatus().then(s => { if (s) setStatus(s); });
```

(c) Displayed avatar: `const shownAvatar = isSelf ? (overrideUrl ?? avatarUrl) : remoteAvatarUrl;` — use `shownAvatar` in the avatar `<div>`/`<img>` instead of `avatarUrl`.

(d) Replace the single Change button with a self-avatar control group:

```tsx
          {isSelf && (
            <div className="avatar-change-group">
              <button className="avatar-change-btn" onClick={async () => {
                const path = await api.pickFile();
                if (path) {
                  const url = await api.setAvatar(path);
                  setAvatarUrl(url);
                }
              }}>Change</button>
              <button className="avatar-change-btn" onClick={async () => {
                const path = await api.pickFile();
                if (path) {
                  const url = await api.setServerAvatarOverride(serverId, path);
                  setOverrideUrl(url);
                }
              }}>This server</button>
              {overrideUrl && (
                <button className="avatar-change-btn" onClick={async () => {
                  await api.clearServerAvatarOverride(serverId);
                  setOverrideUrl(null);
                }}>Reset</button>
              )}
            </div>
          )}
```

(e) Status section, directly under the name/id block (before the divider). Others see `remoteStatus`; self sees an editable line styled like the bio editor:

```tsx
          {(isSelf || remoteStatus) && (
            <div className="profile-card-status">
              {isSelf ? (
                editingStatus ? (
                  <input
                    className="profile-card-status-input"
                    value={statusInput}
                    maxLength={128}
                    autoFocus
                    onChange={(e) => setStatusInput(e.target.value)}
                    onKeyDown={async (e) => {
                      if (e.key === "Enter") {
                        const v = statusInput.trim() || null;
                        await api.setProfileStatus(v);
                        setStatus(v);
                        setEditingStatus(false);
                      }
                      if (e.key === "Escape") setEditingStatus(false);
                    }}
                    placeholder="Set a status..."
                  />
                ) : (
                  <span onClick={() => { setEditingStatus(true); setStatusInput(status || ""); }} style={{ cursor: "text" }}>
                    {status || "Set a status..."}
                  </span>
                )
              ) : (
                <span>{remoteStatus}</span>
              )}
            </div>
          )}
```

Add imports: `import { useMemberProfile } from "../hooks/useMemberProfile";`

- [ ] **Step 5: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add client/src/components/MemberAvatar.tsx client/src/components/MemberSidebar.tsx client/src/components/Message.tsx client/src/components/UserProfilePopup.tsx
git commit -m "client ui: picture avatars + status in member list, chat, and profile popup"
```

---

### Task 9: Theme CSS (ALL themes — required, see CLAUDE.md)

New classes introduced: `.avatar-img`, `.member-text`, `.member-status`, `.avatar-change-group`, `.profile-card-status`, `.profile-card-status-input`.

**Files:**
- Modify: `client/src/themes/discord-dark/theme.css`
- Modify: `client/src/themes/xp-luna-blue/theme.css`
- Modify: `client/src/themes/hello-kitty/theme.css`

- [ ] **Step 1: Add to EVERY theme file** (colors via theme vars only; in `xp-luna-blue` use the same var-fallback trick the join-relay-badge needed if a var is missing — check how `.join-relay-note` is declared there and mirror it):

```css
/* Profile sync: picture avatars + statuses */
.avatar-img {
  width: 100%;
  height: 100%;
  border-radius: 50%;
  object-fit: cover;
  display: block;
}
.member-text {
  display: flex;
  flex-direction: column;
  min-width: 0;
  flex: 1;
}
.member-status {
  font-size: 11px;
  color: var(--xp-text-muted, var(--xp-text-normal));
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.avatar-change-group {
  display: flex;
  gap: 4px;
  flex-wrap: wrap;
}
.profile-card-status {
  font-size: 12px;
  color: var(--xp-text-normal);
  margin: 4px 0;
}
.profile-card-status-input {
  width: 100%;
  font-size: 12px;
  background: var(--xp-input-bg, var(--xp-panel-bg));
  color: var(--xp-text-normal);
  border: 1px solid var(--xp-border);
  padding: 2px 6px;
}
```

IMPORTANT for the executor: these var names (`--xp-text-muted`, `--xp-input-bg`) must be checked against each theme's `:root` block — if a theme defines different names for muted text / input backgrounds, use THAT theme's vars instead; the fallback `var(--xp-…, var(--xp-text-normal))` pattern is the safety net. `.member-avatar-mini` and `.message-avatar` already exist in every theme — do NOT restyle them; the `<img>` fills whatever box they already define.

- [ ] **Step 2: Verify coverage**

Run: `grep -l "avatar-img" client/src/themes/*/theme.css`
Expected: all three theme files listed.

- [ ] **Step 3: Commit**

```bash
git add client/src/themes/*/theme.css
git commit -m "themes: style profile avatars, member status, popup status editor in all themes"
```

---

### Task 10: Docs + full verification

**Files:**
- Modify: `docs/modules/tauri-commands.md` (6 new commands: get_profile_status, set_profile_status, set_server_avatar_override, clear_server_avatar_override, get_server_avatar_override, get_member_profile — name, params, return, side effects, matching `invoke()` name; plus note set_avatar now pushes)
- Modify: `docs/modules/tauri-bridge.md` (new event `server:member_profile_updated` {server_id, public_key, profile_hash} + its useServerEvents listener)
- Create: `docs/modules/profile-sync.md` (use `docs/modules/_TEMPLATE.md`: what the module does, effective-profile resolution, pushed_profiles.json, profile_cache, verification rules)
- Modify: `ARCHITECTURE.md` (one line: profiles sync as signed blobs, hash-cached)
- Modify: `docs/modules/members.md` or the server module doc covering members/handlers if present (set_member_profile/get_member_profile, UpdateProfile/GetMemberProfile)

- [ ] **Step 1: Write the docs per the checklist above.** Follow `_TEMPLATE.md`. Every new public surface from Tasks 1–8 gets an entry in the SAME commit.

- [ ] **Step 2: Full verification suite**

```bash
cd ~/farder && cargo test --workspace
cd ~/farder/client/src-tauri && cargo build && cargo test profile_sync::
cd ~/farder/client && npx tsc --noEmit
```

Expected: all green.

- [ ] **Step 3: Seam audit** — every `invoke("X")` added must be registered:

```bash
cd ~/farder && for c in get_member_profile set_profile_status get_profile_status set_server_avatar_override clear_server_avatar_override get_server_avatar_override; do
  grep -q "$c" client/src-tauri/src/main.rs && grep -rq "invoke[<(].*\"$c\"" client/src/lib/tauri-bridge.ts && echo "OK $c" || echo "MISSING $c";
done
```

Expected: six `OK` lines.

- [ ] **Step 4: Commit docs**

```bash
git add docs/ ARCHITECTURE.md
git commit -m "docs: profile sync module, commands, bridge event"
```

- [ ] **Step 5: Mark UNVERIFIED-at-runtime.** Per CLAUDE.md, this feature is UNVERIFIED until the owner's Windows run: pull on `C:\Users\Deez\farder` (MUST be on `main` after merge — check `git branch --show-current`), rebuild (frontend hot-reloads; the Rust client needs a rebuild: kill farder processes → `cargo build -p farder-server` → `copy-sidecar.ps1` → restart `npm run tauri dev`), then: change picture → second client sees it in member list + chat; set a per-server picture → differs per server; set status → visible to the other client; GIF avatar animates. Note: the SERVER binary must also be updated (protocol additions) — local servers are respawned from the freshly built sidecar, so the normal Windows loop covers it.

---

## Self-review notes (done at plan time)

- **Spec coverage:** payload/wire/server/client-rust/frontend/CSS/docs/verification all mapped to Tasks 1–10. Removal sync (avatar/status set to None) works through the same push path (`SignedProfile` with `None` fields; `set_profile_status(null)` and override-clear push). Pre-upload client-side size check from the spec ("friendly error before uploading") is covered server-side with a clear error; the client surfaces the `Err` string — acceptable; UI pre-check can ride a later polish pass if the owner wants a prettier message.
- **Type consistency:** `profile_hash` is `Option<String>` end-to-end; hash always = SHA-256 hex of the serialized SignedProfile (`profile_hash_hex` == `attachments::compute_sha256`); command names match bridge invoke strings (seam audit in Task 10).
- **Known judgment calls:** profiles of members who never pushed render letter-circles (hash None); `DmEntry`/`DmOpened` participants carry real hashes (DM avatars themselves are out of scope but the data flows for free); `member_profile_updated` listener refetches members (matches the member_joined pattern) rather than patching one row.
