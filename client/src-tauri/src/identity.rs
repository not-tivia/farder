//! Identity key storage. The on-disk file at `<data dir>/identity.key` holds
//! EITHER a legacy 32-byte plaintext key (pre-encryption builds) OR an
//! encrypted blob from `Keypair::export_encrypted` (16 salt + 12 nonce + 48
//! ct+tag = 76 bytes). We detect which by length and gate access behind a
//! 4-digit PIN.
//!
//! `IdentityStore` takes an explicit directory so it is unit-testable without
//! touching the user's real home. The `#[tauri::command]` wrappers (added in a
//! later task) run the blocking crypto off the UI thread and load the unlocked
//! key into `AppState`.

use crate::state::AppState;
use farder_crypto::identity::Keypair;
use farder_crypto::recovery;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;

const KEY_FILE: &str = "identity.key";
const PLAINTEXT_LEN: usize = 32;
const MIN_ENCRYPTED_LEN: usize = 16 + 12 + 32 + 16; // 76

#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IdentityStatus {
    None,
    Plaintext,
    Encrypted,
}

#[derive(Serialize, Debug)]
#[serde(tag = "kind", content = "detail")]
pub enum IdentityError {
    IncorrectPin,
    InvalidPhrase,
    BadPin,
    Corrupt(String),
    Io(String),
}

pub struct CreatedIdentity {
    pub key_bytes: [u8; 32],
    pub public_key: String,
    pub recovery_phrase: String,
}

pub struct UnlockedIdentity {
    pub key_bytes: [u8; 32],
    pub public_key: String,
}

#[derive(Serialize)]
pub struct CreateIdentityResult {
    pub public_key: String,
    pub recovery_phrase: String,
}

pub struct IdentityStore {
    dir: PathBuf,
}

impl IdentityStore {
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn default_location() -> Self {
        Self::at(crate::commands::farder_data_dir())
    }

    fn key_path(&self) -> PathBuf {
        self.dir.join(KEY_FILE)
    }

    /// Classify the on-disk file by length without decrypting.
    pub fn status(&self) -> IdentityStatus {
        match std::fs::metadata(self.key_path()) {
            Ok(m) if m.len() as usize == PLAINTEXT_LEN => IdentityStatus::Plaintext,
            Ok(_) => IdentityStatus::Encrypted,
            Err(_) => IdentityStatus::None,
        }
    }
}

fn validate_pin(pin: &str) -> Result<(), IdentityError> {
    if pin.len() == 4 && pin.chars().all(|c| c.is_ascii_digit()) {
        Ok(())
    } else {
        Err(IdentityError::BadPin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, IdentityStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = IdentityStore::at(dir.path().to_path_buf());
        (dir, store)
    }

    #[test]
    fn status_none_when_absent() {
        let (_d, s) = store();
        assert_eq!(s.status(), IdentityStatus::None);
    }

    #[test]
    fn status_plaintext_for_32_byte_file() {
        let (_d, s) = store();
        std::fs::write(s.key_path(), [0u8; 32]).unwrap();
        assert_eq!(s.status(), IdentityStatus::Plaintext);
    }

    #[test]
    fn status_encrypted_for_blob() {
        let (_d, s) = store();
        std::fs::write(s.key_path(), vec![0u8; MIN_ENCRYPTED_LEN]).unwrap();
        assert_eq!(s.status(), IdentityStatus::Encrypted);
    }

    #[test]
    fn pin_validation() {
        assert!(validate_pin("1234").is_ok());
        assert!(validate_pin("000").is_err());
        assert!(validate_pin("12345").is_err());
        assert!(validate_pin("12a4").is_err());
    }
}
