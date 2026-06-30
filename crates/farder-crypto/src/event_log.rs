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

/// Canonical hash of an invite code, stored as `InviteCreated.code_hash` and used
/// by the server to resolve a presented code to its invite event. The raw code is
/// never put in the log — only this hash.
pub fn invite_code_hash(code: &str) -> String {
    sha256_hex(code.as_bytes())
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

    /// Valid iff the embedded `device_id` matches `device_pubkey` AND the
    /// identity key signed the core.
    pub fn verify(&self) -> Result<()> {
        if self.core.device_id != device_id(&self.core.device_pubkey) {
            anyhow::bail!("device_id does not match device_pubkey");
        }
        let bytes = rmp_serde::to_vec(&self.core).context("serialize devicecert core")?;
        self.core.identity.verify(&bytes, &self.signature)
    }
}

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
    InviteCreated { code_hash: String, max_uses: u32, expires_at: u64, #[serde(default)] requires_approval: bool },
    MemberJoined { member: PublicKey, invite: EventRef },
    MemberApproved { member: PublicKey },
    MemberRemoved { member: PublicKey },
    MemberBanned { member: PublicKey },
    MemberUnbanned { member: PublicKey },
    PermissionGranted { member: PublicKey, capability: String },
    /// Take down an attachment: delete its bytes + mark it redacted. Authorized by
    /// the original uploader or a moderator (holds "kick"). Content-hash-keyed so it
    /// is meaningful across hosts/replication (file_ids are server-local).
    AttachmentRedacted { content_hash: String },
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
        sha256_hex(&self.to_bytes())
    }

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
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Different owner -> different id.
        let other = Keypair::generate();
        let d = genesis_for(&other, "friends", [1u8; 16]);
        assert_ne!(a.server_id(), d.server_id());
    }

    #[test]
    fn genesis_from_bytes_rejects_garbage() {
        assert!(Genesis::from_bytes(&[0xFF, 0x00, 0x12]).is_err());
    }

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
        // Tampered created_at -> signature no longer matches.
        let mut cert = DeviceCert::create(&identity, &device.public_key(), 1);
        cert.core.created_at = 2;
        assert!(cert.verify().is_err());
        // device_id that doesn't match the embedded device_pubkey.
        let mut cert2 = DeviceCert::create(&identity, &device.public_key(), 1);
        cert2.core.device_id = device_id(&Keypair::generate().public_key());
        assert!(cert2.verify().is_err());
    }

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
}

#[cfg(test)]
mod invite_code_hash_tests {
    use super::invite_code_hash;

    #[test]
    fn invite_code_hash_is_stable_and_distinct() {
        let h = invite_code_hash("AbCd1234");
        assert_eq!(h.len(), 64, "sha-256 hex is 64 chars");
        assert_eq!(h, invite_code_hash("AbCd1234"), "deterministic");
        assert_ne!(h, invite_code_hash("AbCd1235"), "distinct codes -> distinct hashes");
    }
}
