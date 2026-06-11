use crate::identity::{Keypair, PublicKey};
use anyhow::{Result, Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileData {
    pub public_key: PublicKey,
    pub display_name: String,
    pub avatar: Option<Vec<u8>>,
    pub status: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedProfile {
    pub data: ProfileData,
    pub signature: Vec<u8>,
}

impl SignedProfile {
    pub fn create(keypair: &Keypair, display_name: String, avatar: Option<Vec<u8>>, status: Option<String>) -> Self {
        let data = ProfileData {
            public_key: keypair.public_key(),
            display_name, avatar, status,
        };
        let serialized = rmp_serde::to_vec(&data).expect("profile serialization cannot fail");
        let signature = keypair.sign(&serialized);
        Self { data, signature }
    }

    pub fn verify(&self) -> Result<()> {
        let serialized = rmp_serde::to_vec(&self.data).context("failed to serialize profile for verification")?;
        self.data.public_key.verify(&serialized, &self.signature)
    }

    pub fn display_name(&self) -> &str { &self.data.display_name }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Keypair;

    #[test]
    fn test_create_and_verify_profile() {
        let keypair = Keypair::generate();
        let profile = SignedProfile::create(&keypair, "Alice".to_string(), None, None);
        assert!(profile.verify().is_ok());
        assert_eq!(profile.display_name(), "Alice");
    }

    #[test]
    fn test_tampered_profile_fails_verification() {
        let keypair = Keypair::generate();
        let mut profile = SignedProfile::create(&keypair, "Alice".to_string(), None, None);
        profile.data.display_name = "Mallory".to_string();
        assert!(profile.verify().is_err());
    }

    #[test]
    fn test_profile_with_all_fields() {
        let keypair = Keypair::generate();
        let avatar = vec![0u8, 1, 2, 3, 255];
        let status = Some("Online".to_string());
        let profile = SignedProfile::create(&keypair, "Bob".to_string(), Some(avatar), status);
        assert!(profile.verify().is_ok());
        assert_eq!(profile.display_name(), "Bob");
        assert!(profile.data.avatar.is_some());
        assert_eq!(profile.data.status.as_deref(), Some("Online"));
    }

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
}
