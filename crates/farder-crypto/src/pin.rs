use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PinHash {
    salt: [u8; 16],
    hash: [u8; 32],
}

impl PinHash {
    pub fn create(pin: &str) -> Result<Self> {
        Self::validate_pin(pin)?;
        let salt: [u8; 16] = rand::random();
        let hash = Self::derive(pin, &salt)?;
        Ok(Self { salt, hash })
    }

    pub fn verify(&self, pin: &str) -> bool {
        match Self::derive(pin, &self.salt) {
            Ok(derived) => derived == self.hash,
            Err(_) => false,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(48);
        out.extend_from_slice(&self.salt);
        out.extend_from_slice(&self.hash);
        out
    }

    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        if data.len() != 48 { bail!("pin hash data must be 48 bytes"); }
        let mut salt = [0u8; 16];
        let mut hash = [0u8; 32];
        salt.copy_from_slice(&data[..16]);
        hash.copy_from_slice(&data[16..]);
        Ok(Self { salt, hash })
    }

    fn validate_pin(pin: &str) -> Result<()> {
        if pin.len() != 4 || !pin.chars().all(|c| c.is_ascii_digit()) {
            bail!("PIN must be exactly 4 digits");
        }
        Ok(())
    }

    fn derive(pin: &str, salt: &[u8; 16]) -> Result<[u8; 32]> {
        let mut hash = [0u8; 32];
        argon2::Argon2::default()
            .hash_password_into(pin.as_bytes(), salt, &mut hash)
            .map_err(|e| anyhow::anyhow!("argon2 error: {}", e))?;
        Ok(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pin_hash_and_verify() {
        let pin_hash = PinHash::create("1234").expect("create failed");
        assert!(pin_hash.verify("1234"));
    }

    #[test]
    fn test_pin_wrong_value_fails() {
        let pin_hash = PinHash::create("1234").expect("create failed");
        assert!(!pin_hash.verify("5678"));
    }

    #[test]
    fn test_pin_must_be_4_digits() {
        // Too short
        assert!(PinHash::create("123").is_err());
        // Too long
        assert!(PinHash::create("12345").is_err());
        // Non-digits
        assert!(PinHash::create("12ab").is_err());
        // Empty
        assert!(PinHash::create("").is_err());
        // Correct
        assert!(PinHash::create("0000").is_ok());
        assert!(PinHash::create("9999").is_ok());
    }

    #[test]
    fn test_pin_serialization() {
        let pin_hash = PinHash::create("4242").expect("create failed");
        let bytes = pin_hash.to_bytes();
        assert_eq!(bytes.len(), 48);
        let restored = PinHash::from_bytes(bytes).expect("from_bytes failed");
        assert!(restored.verify("4242"));
        assert!(!restored.verify("0000"));
    }
}
