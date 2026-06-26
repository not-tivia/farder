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
        // Different owner -> different id.
        let other = Keypair::generate();
        let d = genesis_for(&other, "friends", [1u8; 16]);
        assert_ne!(a.server_id(), d.server_id());
    }

    #[test]
    fn genesis_from_bytes_rejects_garbage() {
        assert!(Genesis::from_bytes(&[0xFF, 0x00, 0x12]).is_err());
    }
}
