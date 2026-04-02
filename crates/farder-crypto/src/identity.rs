use ed25519_dalek::{SigningKey, VerifyingKey, Signature, Signer, Verifier};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::fmt;
use anyhow::{Result, Context, bail};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use zeroize::Zeroize;

pub struct Keypair {
    signing_key: SigningKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PublicKey {
    bytes: [u8; 32],
}

impl Keypair {
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        Self { signing_key }
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey { bytes: self.signing_key.verifying_key().to_bytes() }
    }

    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        let signature: Signature = self.signing_key.sign(message);
        signature.to_bytes().to_vec()
    }

    pub fn signing_key_bytes(&self) -> &[u8; 32] {
        self.signing_key.as_bytes()
    }

    pub fn export_encrypted(&self, passphrase: &str) -> Result<Vec<u8>> {
        let salt: [u8; 16] = rand::random();
        let nonce_bytes: [u8; 12] = rand::random();
        let mut derived_key = [0u8; 32];
        argon2::Argon2::default()
            .hash_password_into(passphrase.as_bytes(), &salt, &mut derived_key)
            .map_err(|e| anyhow::anyhow!("argon2 error: {}", e))?;
        let cipher = Aes256Gcm::new_from_slice(&derived_key).context("failed to create cipher")?;
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher.encrypt(nonce, self.signing_key.as_bytes().as_ref())
            .map_err(|e| anyhow::anyhow!("encryption error: {}", e))?;
        derived_key.zeroize();
        let mut output = Vec::with_capacity(16 + 12 + ciphertext.len());
        output.extend_from_slice(&salt);
        output.extend_from_slice(&nonce_bytes);
        output.extend_from_slice(&ciphertext);
        Ok(output)
    }

    pub fn import_encrypted(data: &[u8], passphrase: &str) -> Result<Self> {
        if data.len() < 16 + 12 + 32 + 16 { bail!("encrypted key data too short"); }
        let salt = &data[..16];
        let nonce_bytes = &data[16..28];
        let ciphertext = &data[28..];
        let mut derived_key = [0u8; 32];
        argon2::Argon2::default()
            .hash_password_into(passphrase.as_bytes(), salt, &mut derived_key)
            .map_err(|e| anyhow::anyhow!("argon2 error: {}", e))?;
        let cipher = Aes256Gcm::new_from_slice(&derived_key).context("failed to create cipher")?;
        let nonce = Nonce::from_slice(nonce_bytes);
        let plaintext = cipher.decrypt(nonce, ciphertext)
            .map_err(|_| anyhow::anyhow!("decryption failed - wrong passphrase?"))?;
        derived_key.zeroize();
        let key_bytes: [u8; 32] = plaintext.try_into()
            .map_err(|_| anyhow::anyhow!("decrypted key wrong length"))?;
        let signing_key = SigningKey::from_bytes(&key_bytes);
        Ok(Self { signing_key })
    }
}

impl PublicKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self { Self { bytes } }
    pub fn as_bytes(&self) -> &[u8; 32] { &self.bytes }
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<()> {
        let verifying_key = VerifyingKey::from_bytes(&self.bytes).context("invalid public key")?;
        let sig = Signature::from_slice(signature).context("invalid signature")?;
        verifying_key.verify(message, &sig).context("signature verification failed")
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "vk_{}", hex::encode(self.bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keypair_generation() {
        let keypair = Keypair::generate();
        let pub_key = keypair.public_key();
        assert_eq!(pub_key.as_bytes().len(), 32);
    }

    #[test]
    fn test_sign_and_verify() {
        let keypair = Keypair::generate();
        let message = b"hello farder";
        let sig = keypair.sign(message);
        let pub_key = keypair.public_key();
        assert!(pub_key.verify(message, &sig).is_ok());
    }

    #[test]
    fn test_verify_wrong_message_fails() {
        let keypair = Keypair::generate();
        let message = b"hello farder";
        let sig = keypair.sign(message);
        let pub_key = keypair.public_key();
        assert!(pub_key.verify(b"different message", &sig).is_err());
    }

    #[test]
    fn test_verify_wrong_key_fails() {
        let keypair1 = Keypair::generate();
        let keypair2 = Keypair::generate();
        let message = b"hello farder";
        let sig = keypair1.sign(message);
        let pub_key2 = keypair2.public_key();
        assert!(pub_key2.verify(message, &sig).is_err());
    }

    #[test]
    fn test_public_key_display_format() {
        let keypair = Keypair::generate();
        let pub_key = keypair.public_key();
        let display = format!("{}", pub_key);
        assert!(display.starts_with("vk_"));
        assert_eq!(display.len(), 3 + 64);
    }

    #[test]
    fn test_keypair_export_import() {
        let keypair = Keypair::generate();
        let passphrase = "hunter2";
        let exported = keypair.export_encrypted(passphrase).expect("export failed");
        let imported = Keypair::import_encrypted(&exported, passphrase).expect("import failed");
        assert_eq!(keypair.signing_key_bytes(), imported.signing_key_bytes());
        assert_eq!(keypair.public_key(), imported.public_key());
    }

    #[test]
    fn test_keypair_import_wrong_passphrase_fails() {
        let keypair = Keypair::generate();
        let exported = keypair.export_encrypted("correct-pass").expect("export failed");
        let result = Keypair::import_encrypted(&exported, "wrong-pass");
        assert!(result.is_err());
    }
}
