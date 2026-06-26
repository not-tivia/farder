# Mesh Rung 1 — Sub-project 1: Genesis + Event Crypto + Device-Chain Schema — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the pure cryptographic foundation of the mesh event log — server genesis identity, device certificates, the signed `Event` type, and the `(author, device)` chain builder — in `farder-crypto`, with no protocol/server/client behavior yet.

**Architecture:** A single new module `crates/farder-crypto/src/event_log.rs`, mirroring the existing `profile.rs` pattern exactly: data structs derive serde, the signed-over bytes are `rmp_serde::to_vec(&core)`, signatures use the existing `Keypair`/`PublicKey` Ed25519 API, and content ids are SHA-256 hex of canonical bytes. Everything is a pure function of its inputs (no I/O, no randomness inside the types — callers pass nonces/timestamps), which keeps it trivially testable and satisfies the spec's "checkpoint-friendly / pure fold" constraint.

**Tech Stack:** Rust, `ed25519-dalek` (via `farder-crypto`'s `Keypair`/`PublicKey`), `sha2` (SHA-256), `serde` + `rmp-serde` (MessagePack canonical bytes), `hex`, `anyhow`. All already dependencies of `farder-crypto`.

## Global Constraints

- **Author/device-signed, not host-signed.** Events are signed by the **device subkey**; the chain's `author` is the **identity**; the identity authorizes devices via an identity-signed `DeviceCert`. (Spec: device subkeys in the schema now.)
- **Mirror `profile.rs` idioms verbatim:** sign `rmp_serde::to_vec(&core)`; `.expect("… cannot fail")` on serialization of in-memory structs; `.context(…)` on deserialization; content id = SHA-256 hex of canonical bytes.
- **Reuse existing crypto only** — no new crypto or hash dependency. Hash = SHA-256 (same as `profile_hash_hex`).
- **Pure foundation, no behavior.** No protocol messages, no server/client wiring, no DB. Those are sub-projects 2–4. This module compiles and is fully unit-tested in isolation via `cargo test -p farder-crypto`.
- **Checkpoint-friendly:** keep everything a pure function of explicit inputs (e.g. the chain builder takes the previous event + observed lamport as parameters), so later rungs can fold from any starting state, not only genesis.
- **Ed25519 is deterministic** (RFC 8032), so hashing a signed `Event` (signature included) is stable — relied on for `Event::hash`.

---

## File Structure

- **Create** `crates/farder-crypto/src/event_log.rs` — all of sub-project 1: `Genesis`, `DeviceCert`/`DeviceCertCore`, `EventPayload`/`AttachmentCap`, `EventCore`/`Event`, the id/hash helpers, the `(author, device)` chain builder, and the unit tests. One focused module, matching the crate's flat single-file style (`identity.rs`, `profile.rs`).
- **Modify** `crates/farder-crypto/src/lib.rs` — add `pub mod event_log;`.

Type aliases (hex SHA-256 strings, matching `profile_hash_hex`'s `String` style; type-safety newtypes are a deliberate later refinement, noted but out of scope):
- `pub type ServerId = String;` — hex SHA-256 of canonical `Genesis` bytes.
- `pub type DeviceId = String;` — hex SHA-256 of a device public key.
- `pub type EventHash = String;` — hex SHA-256 of canonical signed-`Event` bytes.
- `pub type EventRef = EventHash;` — a content-addressed reference to another event.

---

## Task 1: Genesis + server identity

**Files:**
- Create: `crates/farder-crypto/src/event_log.rs`
- Modify: `crates/farder-crypto/src/lib.rs` (add module declaration)
- Test: in-module `#[cfg(test)] mod tests` in `event_log.rs`

**Interfaces:**
- Produces: `Genesis { version: u16, name: String, owner: PublicKey, created_at: u64, nonce: [u8;16] }`; `Genesis::to_bytes()/from_bytes()`; `Genesis::server_id() -> ServerId`; type aliases `ServerId`/`DeviceId`/`EventHash`/`EventRef`; private `sha256_hex(&[u8]) -> String`.
- Consumes: `farder_crypto::identity::PublicKey`.

- [ ] **Step 1: Declare the module**

In `crates/farder-crypto/src/lib.rs`, add the line (keep the list alphabetical-ish, after `pub mod encryption;`):

```rust
pub mod event_log;
```

- [ ] **Step 2: Write the failing test**

Create `crates/farder-crypto/src/event_log.rs` with the imports, the type aliases, the hash helper, the `Genesis` struct, and this test (implementation of `server_id` intentionally left to step 4 — but to make the file compile so the test *runs and fails on assertion*, include the full `Genesis` + `sha256_hex` now; the test asserts behavior):

```rust
use crate::identity::{Keypair, PublicKey};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Hex SHA-256 of arbitrary bytes — the content-id primitive for the event log
/// (mirrors profile::profile_hash_hex; kept local to this module).
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub type ServerId = String; // hex SHA-256 of canonical Genesis bytes
pub type DeviceId = String; // hex SHA-256 of a device public key
pub type EventHash = String; // hex SHA-256 of canonical signed-Event bytes
pub type EventRef = EventHash; // content-addressed reference to another event

/// The content-addressed root that defines a server. Not signed — its hash IS
/// its identity, so any tampering changes the id. The `owner` is cryptographically
/// fixed here; `nonce` makes two same-name/same-owner servers distinct.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Genesis {
    pub version: u16,
    pub name: String,
    pub owner: PublicKey,
    pub created_at: u64,
    pub nonce: [u8; 16],
}

impl Genesis {
    pub fn to_bytes(&self) -> Vec<u8> {
        rmp_serde::to_vec(self).expect("genesis serialization cannot fail")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        rmp_serde::from_slice(bytes).context("failed to decode genesis")
    }

    /// Content-addressed server id: hex SHA-256 of the canonical genesis bytes.
    pub fn server_id(&self) -> ServerId {
        sha256_hex(&self.to_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn genesis_for(owner: &Keypair, name: &str, nonce: [u8; 16]) -> Genesis {
        Genesis {
            version: 1,
            name: name.to_string(),
            owner: owner.public_key(),
            created_at: 1_700_000_000,
            nonce,
        }
    }

    #[test]
    fn genesis_id_is_stable_and_64_hex_chars() {
        let owner = Keypair::generate();
        let g = genesis_for(&owner, "friends", [7u8; 16]);
        let id1 = g.server_id();
        assert_eq!(id1, g.server_id(), "server_id must be deterministic");
        assert_eq!(id1.len(), 64, "SHA-256 hex is 64 chars");
        // Round-trips through bytes without changing identity.
        let decoded = Genesis::from_bytes(&g.to_bytes()).unwrap();
        assert_eq!(decoded.server_id(), id1);
        assert_eq!(decoded, g);
    }

    #[test]
    fn genesis_id_changes_with_content() {
        let owner = Keypair::generate();
        let a = genesis_for(&owner, "friends", [1u8; 16]);
        let b = genesis_for(&owner, "friends", [2u8; 16]); // different nonce only
        let c = genesis_for(&owner, "enemies", [1u8; 16]); // different name only
        assert_ne!(a.server_id(), b.server_id());
        assert_ne!(a.server_id(), c.server_id());
        // Different owner → different id.
        let other = Keypair::generate();
        let d = genesis_for(&other, "friends", [1u8; 16]);
        assert_ne!(a.server_id(), d.server_id());
    }

    #[test]
    fn genesis_from_bytes_rejects_garbage() {
        assert!(Genesis::from_bytes(&[0xFF, 0x00, 0x12]).is_err());
    }
}
```

- [ ] **Step 3: Run the tests to verify they pass (this task's impl is complete in the file above)**

Run: `cargo test -p farder-crypto event_log::tests::genesis`
Expected: 3 tests pass (`genesis_id_is_stable_and_64_hex_chars`, `genesis_id_changes_with_content`, `genesis_from_bytes_rejects_garbage`).

> Note: because `Genesis` + `sha256_hex` are small and self-contained, this task writes the implementation and the tests together; the "failing" state is simply the module not existing / not compiling before step 2. If you prefer strict red-green, stub `server_id` to `String::new()` first, watch `genesis_id_is_stable_and_64_hex_chars` fail the length assertion, then fill it in.

- [ ] **Step 4: Commit**

```bash
git add crates/farder-crypto/src/event_log.rs crates/farder-crypto/src/lib.rs
git commit -m "feat(crypto): mesh event-log Genesis + content-addressed server_id"
```

---

## Task 2: DeviceCert + device id

**Files:**
- Modify: `crates/farder-crypto/src/event_log.rs` (add `DeviceCert`, `device_id`, tests)

**Interfaces:**
- Consumes: `Keypair`, `PublicKey`, `sha256_hex`, `DeviceId` (Task 1).
- Produces: `device_id(&PublicKey) -> DeviceId`; `DeviceCertCore { identity, device_pubkey, device_id, created_at }`; `DeviceCert { core, signature }`; `DeviceCert::create(identity: &Keypair, device_pubkey: &PublicKey, created_at: u64) -> DeviceCert`; `DeviceCert::verify(&self) -> Result<()>`.

- [ ] **Step 1: Write the failing test**

Add to `event_log.rs` (above the `#[cfg(test)]` module), the device id helper + types with a STUBBED verify so the test compiles and fails:

```rust
/// A device's id: hex SHA-256 of its public key bytes.
pub fn device_id(device_pubkey: &PublicKey) -> DeviceId {
    sha256_hex(device_pubkey.as_bytes())
}

/// The fields an identity signs to authorize one of its device subkeys.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceCertCore {
    pub identity: PublicKey,      // the owning identity (an Event's `author`)
    pub device_pubkey: PublicKey, // the device's signing subkey
    pub device_id: DeviceId,      // = device_id(device_pubkey)
    pub created_at: u64,
}

/// An identity-signed authorization of a device subkey. Events are signed by the
/// device subkey; this proves the identity stands behind that device.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceCert {
    pub core: DeviceCertCore,
    pub signature: Vec<u8>, // the IDENTITY key's sig over canonical(core)
}

impl DeviceCert {
    pub fn create(identity: &Keypair, device_pubkey: &PublicKey, created_at: u64) -> Self {
        let core = DeviceCertCore {
            identity: identity.public_key(),
            device_pubkey: device_pubkey.clone(),
            device_id: device_id(device_pubkey),
            created_at,
        };
        let bytes = rmp_serde::to_vec(&core).expect("devicecert serialization cannot fail");
        let signature = identity.sign(&bytes);
        Self { core, signature }
    }

    pub fn verify(&self) -> Result<()> {
        anyhow::bail!("not implemented") // STUB — replace in step 3
    }
}
```

Add these tests inside `mod tests`:

```rust
    #[test]
    fn device_id_is_hash_of_pubkey() {
        let dev = Keypair::generate();
        assert_eq!(device_id(&dev.public_key()).len(), 64);
        assert_eq!(device_id(&dev.public_key()), device_id(&dev.public_key()));
        let other = Keypair::generate();
        assert_ne!(device_id(&dev.public_key()), device_id(&other.public_key()));
    }

    #[test]
    fn devicecert_create_and_verify() {
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let cert = DeviceCert::create(&identity, &device.public_key(), 1_700_000_000);
        assert_eq!(cert.core.identity, identity.public_key());
        assert_eq!(cert.core.device_id, device_id(&device.public_key()));
        assert!(cert.verify().is_ok());
    }

    #[test]
    fn devicecert_tampered_or_wrong_identity_fails() {
        let identity = Keypair::generate();
        let device = Keypair::generate();
        // Tampered created_at → signature no longer matches.
        let mut cert = DeviceCert::create(&identity, &device.public_key(), 1);
        cert.core.created_at = 2;
        assert!(cert.verify().is_err());
        // device_id that doesn't match the embedded device_pubkey.
        let mut cert2 = DeviceCert::create(&identity, &device.public_key(), 1);
        cert2.core.device_id = device_id(&Keypair::generate().public_key());
        assert!(cert2.verify().is_err());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p farder-crypto event_log::tests::devicecert`
Expected: FAIL — `devicecert_create_and_verify` fails because `verify` bails "not implemented".

- [ ] **Step 3: Implement `verify`**

Replace the stubbed `verify` body with:

```rust
    /// Valid iff the embedded `device_id` matches `device_pubkey` AND the
    /// identity key signed the core.
    pub fn verify(&self) -> Result<()> {
        if self.core.device_id != device_id(&self.core.device_pubkey) {
            anyhow::bail!("device_id does not match device_pubkey");
        }
        let bytes = rmp_serde::to_vec(&self.core).context("serialize devicecert core")?;
        self.core.identity.verify(&bytes, &self.signature)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p farder-crypto event_log::tests::device`
Expected: PASS (`device_id_is_hash_of_pubkey`, `devicecert_create_and_verify`, `devicecert_tampered_or_wrong_identity_fails`).

- [ ] **Step 5: Commit**

```bash
git add crates/farder-crypto/src/event_log.rs
git commit -m "feat(crypto): mesh DeviceCert + device id (identity authorizes device subkeys)"
```

---

## Task 3: Event payload + signed Event + event hash

**Files:**
- Modify: `crates/farder-crypto/src/event_log.rs` (add `AttachmentCap`, `EventPayload`, `EventCore`, `Event`, signing/verify/hash, tests)

**Interfaces:**
- Consumes: `Keypair`, `PublicKey`, `DeviceCert`, `device_id`, `sha256_hex`, the type aliases.
- Produces:
  - `AttachmentCap { content_hash: String, declared_type: String, size: u64, uploader: PublicKey }`
  - `EventPayload` enum (the variants below)
  - `EventCore { server_id, author, device, seq, prev, lamport, timestamp, payload }`
  - `Event { core, signature }`
  - `Event::sign(core: EventCore, device: &Keypair) -> Event`
  - `Event::verify(&self, device_pubkey: &PublicKey) -> Result<()>`
  - `Event::to_bytes()/from_bytes()`; `Event::hash(&self) -> EventHash`

- [ ] **Step 1: Write the failing test**

Add the types + a STUBBED `hash` (returns `String::new()`) so the file compiles and the hash test fails:

```rust
/// A content-addressed reference to an attachment's bytes (not the bytes
/// themselves, which live in the file store). Round-2: the cap is validated
/// against the actual blob in sub-project 4; here it is just the descriptor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttachmentCap {
    pub content_hash: String, // hex SHA-256 of the file bytes
    pub declared_type: String, // MIME, e.g. "image/png"
    pub size: u64,
    pub uploader: PublicKey,
}

/// The action an event records. The authorization core (everything except
/// MessagePosted) gets its signing/validation rules in sub-project 2; here the
/// variants are pure data so they can be signed and round-tripped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EventPayload {
    MessagePosted {
        channel_id: u64,
        content: String,
        reply_to: Option<EventRef>,
        attachments: Vec<AttachmentCap>,
    },
    DeviceAuthorized { cert: DeviceCert },
    InviteCreated { code_hash: String, max_uses: u32, expires_at: u64 },
    MemberJoined { member: PublicKey, invite: EventRef },
    MemberRemoved { member: PublicKey },
    MemberBanned { member: PublicKey },
    MemberUnbanned { member: PublicKey },
    PermissionGranted { member: PublicKey, capability: String },
}

/// The signed-over body of an event. `author` is the IDENTITY; `device` says
/// which of its devices signed; `(author, device)` keys the chain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventCore {
    pub server_id: ServerId,
    pub author: PublicKey,
    pub device: DeviceId,
    pub seq: u64,
    pub prev: Option<EventHash>,
    pub lamport: u64,
    pub timestamp: u64, // device wall-clock claim — UNTRUSTED, tiebreak only
    pub payload: EventPayload,
}

/// A signed event. The signature is by the DEVICE subkey over canonical(core).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub core: EventCore,
    pub signature: Vec<u8>,
}

impl Event {
    /// Sign a fully-formed core with the device subkey.
    pub fn sign(core: EventCore, device: &Keypair) -> Self {
        let bytes = rmp_serde::to_vec(&core).expect("event serialization cannot fail");
        let signature = device.sign(&bytes);
        Self { core, signature }
    }

    /// Verify the device-subkey signature over the core. (Whether `device` is
    /// authorized by `core.author` is a SEPARATE check via DeviceCert, done at
    /// validation time in later sub-projects.)
    pub fn verify(&self, device_pubkey: &PublicKey) -> Result<()> {
        let bytes = rmp_serde::to_vec(&self.core).context("serialize event core")?;
        device_pubkey.verify(&bytes, &self.signature)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        rmp_serde::to_vec(self).expect("event serialization cannot fail")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        rmp_serde::from_slice(bytes).context("failed to decode event")
    }

    /// Content id: hex SHA-256 of the canonical signed-event bytes (signature
    /// included — Ed25519 is deterministic, so this is stable). Used as the
    /// event's id and in `prev`.
    pub fn hash(&self) -> EventHash {
        String::new() // STUB — replace in step 3
    }
}
```

Add these tests inside `mod tests`. Add a helper at the top of `mod tests`:

```rust
    fn a_message(seq: u64, prev: Option<EventHash>, server_id: &str, author: &PublicKey, device: &Keypair) -> Event {
        let core = EventCore {
            server_id: server_id.to_string(),
            author: author.clone(),
            device: device_id(&device.public_key()),
            seq,
            prev,
            lamport: seq + 1,
            timestamp: 1_700_000_000 + seq,
            payload: EventPayload::MessagePosted {
                channel_id: 1,
                content: format!("msg {seq}"),
                reply_to: None,
                attachments: vec![],
            },
        };
        Event::sign(core, device)
    }
```

```rust
    #[test]
    fn event_sign_and_verify() {
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let ev = a_message(0, None, "srv", &identity.public_key(), &device);
        // Verifies under the signing DEVICE key.
        assert!(ev.verify(&device.public_key()).is_ok());
        // Fails under a different device key.
        assert!(ev.verify(&Keypair::generate().public_key()).is_err());
    }

    #[test]
    fn event_tamper_breaks_signature() {
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let mut ev = a_message(0, None, "srv", &identity.public_key(), &device);
        ev.core.payload = EventPayload::MessagePosted {
            channel_id: 1, content: "EVIL".to_string(), reply_to: None, attachments: vec![],
        };
        assert!(ev.verify(&device.public_key()).is_err());
    }

    #[test]
    fn event_hash_is_stable_unique_and_roundtrips() {
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let a = a_message(0, None, "srv", &identity.public_key(), &device);
        let b = a_message(1, Some(a.hash()), "srv", &identity.public_key(), &device);
        assert_eq!(a.hash().len(), 64);
        assert_eq!(a.hash(), a.hash(), "hash deterministic");
        assert_ne!(a.hash(), b.hash(), "different content → different hash");
        // Round-trip bytes preserves the hash.
        let decoded = Event::from_bytes(&a.to_bytes()).unwrap();
        assert_eq!(decoded.hash(), a.hash());
        assert_eq!(decoded, a);
    }

    #[test]
    fn event_with_attachment_cap_roundtrips() {
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let core = EventCore {
            server_id: "srv".to_string(),
            author: identity.public_key(),
            device: device_id(&device.public_key()),
            seq: 0, prev: None, lamport: 1, timestamp: 1,
            payload: EventPayload::MessagePosted {
                channel_id: 2,
                content: "pic".to_string(),
                reply_to: None,
                attachments: vec![AttachmentCap {
                    content_hash: "abcd".to_string(),
                    declared_type: "image/png".to_string(),
                    size: 1234,
                    uploader: identity.public_key(),
                }],
            },
        };
        let ev = Event::sign(core, &device);
        let decoded = Event::from_bytes(&ev.to_bytes()).unwrap();
        assert_eq!(decoded, ev);
        assert!(decoded.verify(&device.public_key()).is_ok());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p farder-crypto event_log::tests::event_hash_is_stable_unique_and_roundtrips`
Expected: FAIL — `hash` returns `""`, so `a.hash().len()` is 0, not 64.

- [ ] **Step 3: Implement `hash`**

Replace the stubbed `Event::hash` body with:

```rust
    pub fn hash(&self) -> EventHash {
        sha256_hex(&self.to_bytes())
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p farder-crypto event_log::tests::event`
Expected: PASS — `event_sign_and_verify`, `event_tamper_breaks_signature`, `event_hash_is_stable_unique_and_roundtrips`, `event_with_attachment_cap_roundtrips`.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-crypto/src/event_log.rs
git commit -m "feat(crypto): mesh signed Event + EventPayload + content-addressed event hash"
```

---

## Task 4: The `(author, device)` chain builder

**Files:**
- Modify: `crates/farder-crypto/src/event_log.rs` (add `Event::next`, tests)

**Interfaces:**
- Consumes: `Event`, `EventCore`, `EventPayload`, `Keypair`, `PublicKey`, `device_id`.
- Produces: `Event::next(device: &Keypair, author: PublicKey, server_id: ServerId, prev: Option<&Event>, lamport_observed: u64, timestamp: u64, payload: EventPayload) -> Event` — builds + signs the next event in this device's chain (computes `seq`/`prev`/`lamport`).

- [ ] **Step 1: Write the failing test**

Add the STUBBED builder to `impl Event` (so it compiles and the test fails on the seq assertion):

```rust
    /// Build + sign the NEXT event in this device's chain. Pure: pass the
    /// previous event (or None for the first) and the max lamport this device
    /// has observed; the result is fully determined by the inputs.
    pub fn next(
        device: &Keypair,
        author: PublicKey,
        server_id: ServerId,
        prev: Option<&Event>,
        lamport_observed: u64,
        timestamp: u64,
        payload: EventPayload,
    ) -> Self {
        let _ = (author, server_id, prev, lamport_observed, timestamp, payload);
        // STUB — replace in step 3
        Event::sign(
            EventCore {
                server_id: String::new(),
                author: device.public_key(),
                device: device_id(&device.public_key()),
                seq: 999,
                prev: None,
                lamport: 0,
                timestamp: 0,
                payload: EventPayload::MemberRemoved { member: device.public_key() },
            },
            device,
        )
    }
```

Add these tests inside `mod tests`:

```rust
    fn msg_payload(n: u64) -> EventPayload {
        EventPayload::MessagePosted { channel_id: 1, content: format!("m{n}"), reply_to: None, attachments: vec![] }
    }

    #[test]
    fn chain_first_event_then_links() {
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let id = identity.public_key();

        let e0 = Event::next(&device, id.clone(), "srv".to_string(), None, 0, 100, msg_payload(0));
        assert_eq!(e0.core.seq, 0);
        assert_eq!(e0.core.prev, None);
        assert_eq!(e0.core.lamport, 1); // 0 observed + 1
        assert_eq!(e0.core.server_id, "srv");
        assert_eq!(e0.core.author, id);
        assert_eq!(e0.core.device, device_id(&device.public_key()));
        assert!(e0.verify(&device.public_key()).is_ok());

        let e1 = Event::next(&device, id.clone(), "srv".to_string(), Some(&e0), 5, 101, msg_payload(1));
        assert_eq!(e1.core.seq, 1);
        assert_eq!(e1.core.prev, Some(e0.hash())); // links to e0
        assert_eq!(e1.core.lamport, 6); // 5 observed + 1
        assert!(e1.verify(&device.public_key()).is_ok());
    }

    #[test]
    fn two_devices_of_one_identity_run_independent_chains() {
        let identity = Keypair::generate();
        let dev_a = Keypair::generate();
        let dev_b = Keypair::generate();
        let id = identity.public_key();

        // Both devices author seq 0 under the SAME identity — NOT a fork, because
        // the chain is keyed by (author, device), and the device ids differ.
        let a0 = Event::next(&dev_a, id.clone(), "srv".to_string(), None, 0, 1, msg_payload(0));
        let b0 = Event::next(&dev_b, id.clone(), "srv".to_string(), None, 0, 1, msg_payload(0));
        assert_eq!(a0.core.seq, 0);
        assert_eq!(b0.core.seq, 0);
        assert_eq!(a0.core.author, b0.core.author); // same identity
        assert_ne!(a0.core.device, b0.core.device);  // different devices
        assert_ne!(a0.hash(), b0.hash());            // distinct events
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p farder-crypto event_log::tests::chain_first_event_then_links`
Expected: FAIL — stub returns `seq: 999`, so `assert_eq!(e0.core.seq, 0)` fails.

- [ ] **Step 3: Implement `Event::next`**

Replace the stubbed body with:

```rust
    pub fn next(
        device: &Keypair,
        author: PublicKey,
        server_id: ServerId,
        prev: Option<&Event>,
        lamport_observed: u64,
        timestamp: u64,
        payload: EventPayload,
    ) -> Self {
        let (seq, prev_hash) = match prev {
            Some(p) => (p.core.seq + 1, Some(p.hash())),
            None => (0, None),
        };
        let core = EventCore {
            server_id,
            author,
            device: device_id(&device.public_key()),
            seq,
            prev: prev_hash,
            lamport: lamport_observed + 1,
            timestamp,
            payload,
        };
        Event::sign(core, device)
    }
```

- [ ] **Step 4: Run the tests to verify they pass + the whole crate is green**

Run: `cargo test -p farder-crypto event_log::tests::`
Expected: PASS — `chain_first_event_then_links`, `two_devices_of_one_identity_run_independent_chains`, and all prior event_log tests.

Run: `cargo test -p farder-crypto`
Expected: the full crate passes (event_log added, nothing else broken).

- [ ] **Step 5: Commit**

```bash
git add crates/farder-crypto/src/event_log.rs
git commit -m "feat(crypto): mesh (author,device) chain builder (Event::next)"
```

---

## Self-Review

**Spec coverage (sub-project 1 = "genesis + event crypto + device-chain schema"):**
- Server genesis/identity → Task 1 (`Genesis`, `server_id`). ✅
- Device subkeys + identity-signed cert → Task 2 (`DeviceCert`, `device_id`). ✅
- The `Event`/`EventPayload`/`AttachmentCap` types + canonical-bytes sign/verify/hash → Task 3. ✅
- `(author, device)` chain model (seq/prev/lamport builder) → Task 4. ✅
- "Pure function of inputs / checkpoint-friendly" → builder takes prev + observed lamport as params; no internal state/randomness. ✅
- Authz-event *signing/validation rules* are explicitly NOT here (sub-project 2); these variants are pure data this sub-project — consistent with the spec's decomposition. ✅
- AttachmentCap *validation against bytes* is NOT here (sub-project 4) — descriptor only. ✅

**Placeholder scan:** every code step contains complete code; the only "stubs" are deliberate red-green starting points that are replaced in the same task with shown code. No "TBD"/"add error handling"/etc. ✅

**Type consistency:** `ServerId`/`DeviceId`/`EventHash`/`EventRef` defined in Task 1 and used consistently; `device_id`/`sha256_hex` defined once and reused; `EventCore` field names (`server_id`, `author`, `device`, `seq`, `prev`, `lamport`, `timestamp`, `payload`) identical across Tasks 3–4; `Event::sign`/`verify`/`hash`/`next` signatures stable. ✅

**Note for the reviewer:** the type aliases are bare `String` (matching `profile_hash_hex`); newtypes for `ServerId`/`DeviceId`/`EventHash` would add type-safety and are a reasonable follow-up, but are deliberately out of scope to keep the foundation minimal and codebase-consistent.
