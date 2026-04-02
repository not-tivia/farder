use crate::identity::{Keypair, PublicKey};
use anyhow::{Result, Context};
use serde::{Deserialize, Serialize};

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
}
