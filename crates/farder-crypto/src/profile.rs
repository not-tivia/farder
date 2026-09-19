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
    /// Which bundled profile effect to draw behind this person's card, by id
    /// (`"bats"`, `"snow"`, …). An ID, never an asset: the client renders from
    /// what it shipped with, so viewing a profile fetches nothing from anywhere
    /// and a profile cannot make your client download a stranger's file.
    ///
    /// `#[serde(default)]` is load-bearing. This struct is serialized COMPACTLY
    /// (as an array), so a profile written before this field existed has one
    /// fewer element — measured: it decodes here with `effect: None`, while a
    /// profile written WITH the field fails to decode on a client built before
    /// it ("array had incorrect length"). Old profiles keep working; a new
    /// profile needs a client new enough to know the field. Both directions are
    /// pinned by tests below.
    #[serde(default)]
    pub effect: Option<String>,
}

/// `ProfileData` exactly as it was before the `effect` field, for verifying
/// signatures made then. Borrowed rather than owned so the check costs no
/// copies of an avatar.
#[derive(Serialize)]
struct LegacyProfileData<'a> {
    public_key: &'a PublicKey,
    display_name: &'a String,
    avatar: &'a Option<Vec<u8>>,
    status: &'a Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedProfile {
    pub data: ProfileData,
    pub signature: Vec<u8>,
}

impl SignedProfile {
    pub fn create(keypair: &Keypair, display_name: String, avatar: Option<Vec<u8>>, status: Option<String>) -> Self {
        Self::create_with_effect(keypair, display_name, avatar, status, None)
    }

    /// As [`Self::create`], carrying a bundled effect id.
    pub fn create_with_effect(
        keypair: &Keypair,
        display_name: String,
        avatar: Option<Vec<u8>>,
        status: Option<String>,
        effect: Option<String>,
    ) -> Self {
        let data = ProfileData {
            public_key: keypair.public_key(),
            display_name, avatar, status, effect,
        };
        let serialized = rmp_serde::to_vec(&data).expect("profile serialization cannot fail");
        let signature = keypair.sign(&serialized);
        Self { data, signature }
    }

    pub fn verify(&self) -> Result<()> {
        let serialized = rmp_serde::to_vec(&self.data).context("failed to serialize profile for verification")?;
        if self.data.public_key.verify(&serialized, &self.signature).is_ok() {
            return Ok(());
        }
        // Profiles signed before `effect` existed were signed over FOUR fields,
        // and this struct now serializes five. Without this the signature check
        // fails for every profile anyone has ever pushed — names and avatars
        // would vanish across the network until each person happened to publish
        // a new one.
        //
        // Only attempted when there is no effect: a profile that carries one
        // cannot have been signed in a format that had nowhere to put it, so
        // this can never be used to smuggle an unsigned effect past the check.
        if self.data.effect.is_none() {
            let legacy = LegacyProfileData {
                public_key: &self.data.public_key,
                display_name: &self.data.display_name,
                avatar: &self.data.avatar,
                status: &self.data.status,
            };
            let legacy_bytes =
                rmp_serde::to_vec(&legacy).context("failed to serialize legacy profile form")?;
            return self.data.public_key.verify(&legacy_bytes, &self.signature);
        }
        self.data.public_key.verify(&serialized, &self.signature)
    }

    pub fn display_name(&self) -> &str { &self.data.display_name }

    /// The bundled effect id, if this profile names one.
    pub fn effect(&self) -> Option<&str> { self.data.effect.as_deref() }

    pub fn to_bytes(&self) -> Vec<u8> {
        rmp_serde::to_vec(self).expect("profile serialization cannot fail")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        rmp_serde::from_slice(bytes).context("failed to decode signed profile")
    }
}

/// Canonical hash of a serialized SignedProfile: SHA-256 hex of the bytes
/// produced by `to_bytes`. Used as the cache key and change detector everywhere.
///
/// Deliberately a free function over raw bytes (not a method that re-serializes):
/// consumers must hash the EXACT bytes they received/stored, so the hash always
/// matches the wire/cache bytes even if serialization were to change.
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
        assert_eq!(decoded.data.status.as_deref(), Some("hi"));
        assert_eq!(decoded.data.public_key, profile.data.public_key);
    }

    #[test]
    fn test_profile_hash_is_stable_and_changes_with_content() {
        let keypair = Keypair::generate();
        let p1 = SignedProfile::create(&keypair, "Alice".to_string(), None, None);
        let bytes1 = p1.to_bytes();
        assert_eq!(p1.to_bytes(), p1.to_bytes());
        assert_eq!(profile_hash_hex(&p1.to_bytes()), profile_hash_hex(&bytes1));
        assert_eq!(profile_hash_hex(&bytes1).len(), 64);
        let p2 = SignedProfile::create(&keypair, "Alice".to_string(), None, Some("x".to_string()));
        assert_ne!(profile_hash_hex(&bytes1), profile_hash_hex(&p2.to_bytes()));
    }

    #[test]
    fn test_from_bytes_rejects_garbage() {
        assert!(SignedProfile::from_bytes(&[0xFF, 0x00, 0x12]).is_err());
    }

    /// The format gained `effect` after shipping, and this struct serializes
    /// COMPACTLY — as an array, where a missing trailing field is a SHORTER
    /// array rather than an absent key. Which direction survives decides who has
    /// to rebuild, so it is stated here rather than assumed.
    #[test]
    fn a_profile_written_before_effect_existed_still_decodes_and_verifies() {
        #[derive(serde::Serialize)]
        struct LegacyData {
            public_key: PublicKey,
            display_name: String,
            avatar: Option<Vec<u8>>,
            status: Option<String>,
        }
        #[derive(serde::Serialize)]
        struct LegacyProfile { data: LegacyData, signature: Vec<u8> }

        let kp = Keypair::generate();
        let legacy = LegacyData {
            public_key: kp.public_key(),
            display_name: "Old Timer".into(),
            avatar: None,
            status: Some("still here".into()),
        };
        let signature = kp.sign(&rmp_serde::to_vec(&legacy).unwrap());
        let bytes = rmp_serde::to_vec(&LegacyProfile { data: legacy, signature }).unwrap();

        let decoded = SignedProfile::from_bytes(&bytes).expect("an old profile must still decode");
        assert_eq!(decoded.display_name(), "Old Timer");
        assert_eq!(decoded.effect(), None, "absent, not a decode failure");
        decoded.verify().expect("and its signature still verifies");
    }

    #[test]
    fn the_signature_covers_the_effect() {
        let kp = Keypair::generate();
        let p = SignedProfile::create_with_effect(&kp, "Bat Fan".into(), None, None, Some("bats".into()));
        let back = SignedProfile::from_bytes(&p.to_bytes()).unwrap();
        assert_eq!(back.effect(), Some("bats"));
        back.verify().expect("verifies");

        // Your effect is yours: a server that rewrites it breaks the signature.
        let mut forged = back;
        forged.data.effect = Some("snow".into());
        assert!(forged.verify().is_err());
    }
}
