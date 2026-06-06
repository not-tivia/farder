//! Human-writable recovery phrase for the identity key (BIP39, 24 words).
//!
//! The 32-byte Ed25519 signing key is used directly as BIP39 entropy, so the
//! phrase encodes the key itself and is as sensitive as the key. The BIP39
//! checksum catches typos when the user restores.

use anyhow::{anyhow, Result};
use bip39::Mnemonic;

/// Encode a 32-byte key as a 24-word BIP39 phrase.
pub fn phrase_from_key(key: &[u8; 32]) -> Result<String> {
    let mnemonic =
        Mnemonic::from_entropy(key).map_err(|e| anyhow!("failed to build recovery phrase: {e}"))?;
    Ok(mnemonic.to_string())
}

/// Decode a 24-word BIP39 phrase back to a 32-byte key. Fails on a bad
/// checksum, unknown words, or wrong length.
pub fn key_from_phrase(phrase: &str) -> Result<[u8; 32]> {
    let mnemonic =
        Mnemonic::parse(phrase.trim()).map_err(|e| anyhow!("invalid recovery phrase: {e}"))?;
    let entropy = mnemonic.to_entropy();
    let key: [u8; 32] = entropy
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("recovery phrase does not encode a 32-byte key"))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrase_roundtrips_to_the_same_key() {
        let key: [u8; 32] = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
            25, 26, 27, 28, 29, 30, 31, 32,
        ];
        let phrase = phrase_from_key(&key).expect("encode");
        assert_eq!(phrase.split_whitespace().count(), 24);
        let back = key_from_phrase(&phrase).expect("decode");
        assert_eq!(back, key);
    }

    #[test]
    fn tampered_phrase_fails_checksum() {
        let key = [7u8; 32];
        let phrase = phrase_from_key(&key).expect("encode");
        // Swap the first word for another valid word -> checksum breaks.
        let mut words: Vec<&str> = phrase.split_whitespace().collect();
        words[0] = if words[0] == "zoo" { "abandon" } else { "zoo" };
        let tampered = words.join(" ");
        assert!(key_from_phrase(&tampered).is_err());
    }

    #[test]
    fn garbage_phrase_fails() {
        assert!(key_from_phrase("not a real recovery phrase at all").is_err());
    }
}
