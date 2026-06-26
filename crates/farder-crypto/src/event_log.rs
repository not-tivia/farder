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
}
