//! Identity key storage. The on-disk file at `<data dir>/identity.key` holds
//! EITHER a legacy 32-byte plaintext key (pre-encryption builds) OR an
//! encrypted blob from `Keypair::export_encrypted` (16 salt + 12 nonce + 48
//! ct+tag = 76 bytes). We detect which by length and gate access behind a
//! 4-digit PIN.
//!
//! `IdentityStore` takes an explicit directory so it is unit-testable without
//! touching the user's real home. The `#[tauri::command]` wrappers at the
//! bottom of this file run the blocking crypto off the UI thread and load the
//! unlocked key into `AppState`.

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

    /// Atomically write the encrypted blob to the key file (temp + rename).
    fn write_blob(&self, blob: &[u8]) -> Result<(), IdentityError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| IdentityError::Io(e.to_string()))?;
        let tmp = self.dir.join("identity.key.tmp");
        std::fs::write(&tmp, blob).map_err(|e| IdentityError::Io(e.to_string()))?;
        std::fs::rename(&tmp, self.key_path()).map_err(|e| IdentityError::Io(e.to_string()))?;
        Ok(())
    }

    /// Encrypt `keypair` under `pin`, persist it, and return its recovery phrase.
    fn seal_new(&self, keypair: Keypair, pin: &str) -> Result<CreatedIdentity, IdentityError> {
        let blob = keypair
            .export_encrypted(pin)
            .map_err(|e| IdentityError::Io(format!("encrypt failed: {e}")))?;
        self.write_blob(&blob)?;
        let phrase = recovery::phrase_from_key(keypair.signing_key_bytes())
            .map_err(|e| IdentityError::Io(format!("phrase failed: {e}")))?;
        Ok(CreatedIdentity {
            key_bytes: *keypair.signing_key_bytes(),
            public_key: keypair.public_key().to_string(),
            recovery_phrase: phrase,
        })
    }

    /// New user: generate a fresh key, encrypt under `pin`, persist.
    pub fn create(&self, pin: &str) -> Result<CreatedIdentity, IdentityError> {
        validate_pin(pin)?;
        self.seal_new(Keypair::generate(), pin)
    }

    /// Forgot-PIN path: rebuild the key from its recovery phrase and re-store
    /// it encrypted under a new `pin`.
    pub fn restore(&self, phrase: &str, pin: &str) -> Result<UnlockedIdentity, IdentityError> {
        validate_pin(pin)?;
        let bytes = recovery::key_from_phrase(phrase).map_err(|_| IdentityError::InvalidPhrase)?;
        let created = self.seal_new(Keypair::from_signing_key_bytes(&bytes), pin)?;
        Ok(UnlockedIdentity {
            key_bytes: created.key_bytes,
            public_key: created.public_key,
        })
    }

    /// One-time: read the legacy 32-byte plaintext key and re-store it
    /// encrypted under `pin`. The key value is preserved.
    pub fn migrate(&self, pin: &str) -> Result<CreatedIdentity, IdentityError> {
        validate_pin(pin)?;
        let raw = std::fs::read(self.key_path()).map_err(|e| IdentityError::Io(e.to_string()))?;
        let bytes: [u8; 32] = raw
            .as_slice()
            .try_into()
            .map_err(|_| IdentityError::Corrupt("plaintext key not 32 bytes".into()))?;
        self.seal_new(Keypair::from_signing_key_bytes(&bytes), pin)
    }

    /// Returning user: decrypt the blob with `pin`.
    pub fn unlock(&self, pin: &str) -> Result<UnlockedIdentity, IdentityError> {
        let data = std::fs::read(self.key_path()).map_err(|e| IdentityError::Io(e.to_string()))?;
        if data.len() < MIN_ENCRYPTED_LEN {
            return Err(IdentityError::Corrupt("encrypted key too short".into()));
        }
        let keypair =
            Keypair::import_encrypted(&data, pin).map_err(|_| IdentityError::IncorrectPin)?;
        Ok(UnlockedIdentity {
            key_bytes: *keypair.signing_key_bytes(),
            public_key: keypair.public_key().to_string(),
        })
    }
}

fn validate_pin(pin: &str) -> Result<(), IdentityError> {
    if pin.len() == 4 && pin.chars().all(|c| c.is_ascii_digit()) {
        Ok(())
    } else {
        Err(IdentityError::BadPin)
    }
}

// ---------------------------------------------------------------------------
// Tauri commands — thin wrappers. The Argon2 work is blocking, so each runs on
// a blocking thread to avoid freezing the webview, then loads the unlocked key
// into AppState. Only the PUBLIC key (and recovery phrase) cross to the
// frontend; the private key never does.
// ---------------------------------------------------------------------------

fn store_key(state: &Arc<AppState>, key_bytes: [u8; 32]) -> Result<(), IdentityError> {
    let mut lock = state
        .signing_key_bytes
        .lock()
        .map_err(|e| IdentityError::Io(format!("state lock poisoned: {e}")))?;
    *lock = Some(key_bytes);
    Ok(())
}

#[tauri::command]
pub fn identity_status() -> IdentityStatus {
    IdentityStore::default_location().status()
}

#[tauri::command]
pub async fn create_identity(
    state: State<'_, Arc<AppState>>,
    pin: String,
) -> Result<CreateIdentityResult, IdentityError> {
    let created = tauri::async_runtime::spawn_blocking(move || {
        IdentityStore::default_location().create(&pin)
    })
    .await
    .map_err(|e| IdentityError::Io(format!("task join error: {e}")))??;
    store_key(state.inner(), created.key_bytes)?;
    Ok(CreateIdentityResult {
        public_key: created.public_key,
        recovery_phrase: created.recovery_phrase,
    })
}

#[tauri::command]
pub async fn unlock_identity(
    state: State<'_, Arc<AppState>>,
    pin: String,
) -> Result<String, IdentityError> {
    let unlocked = tauri::async_runtime::spawn_blocking(move || {
        IdentityStore::default_location().unlock(&pin)
    })
    .await
    .map_err(|e| IdentityError::Io(format!("task join error: {e}")))??;
    store_key(state.inner(), unlocked.key_bytes)?;
    Ok(unlocked.public_key)
}

#[tauri::command]
pub async fn migrate_plaintext_identity(
    state: State<'_, Arc<AppState>>,
    pin: String,
) -> Result<CreateIdentityResult, IdentityError> {
    let created = tauri::async_runtime::spawn_blocking(move || {
        IdentityStore::default_location().migrate(&pin)
    })
    .await
    .map_err(|e| IdentityError::Io(format!("task join error: {e}")))??;
    store_key(state.inner(), created.key_bytes)?;
    Ok(CreateIdentityResult {
        public_key: created.public_key,
        recovery_phrase: created.recovery_phrase,
    })
}

#[tauri::command]
pub async fn restore_identity(
    state: State<'_, Arc<AppState>>,
    phrase: String,
    pin: String,
) -> Result<String, IdentityError> {
    let unlocked = tauri::async_runtime::spawn_blocking(move || {
        IdentityStore::default_location().restore(&phrase, &pin)
    })
    .await
    .map_err(|e| IdentityError::Io(format!("task join error: {e}")))??;
    store_key(state.inner(), unlocked.key_bytes)?;
    Ok(unlocked.public_key)
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

    #[test]
    fn create_then_unlock_roundtrips_and_hides_key() {
        let (_d, s) = store();
        let created = s.create("1234").expect("create");
        assert_eq!(created.recovery_phrase.split_whitespace().count(), 24);

        // OBSERVATION: the on-disk bytes are NOT the raw private key.
        let on_disk = std::fs::read(s.key_path()).unwrap();
        assert_ne!(on_disk.as_slice(), &created.key_bytes[..]);
        assert!(on_disk.len() >= MIN_ENCRYPTED_LEN);

        // Right PIN reopens to the same key; wrong PIN fails.
        let unlocked = s.unlock("1234").expect("unlock");
        assert_eq!(unlocked.key_bytes, created.key_bytes);
        assert_eq!(unlocked.public_key, created.public_key);
        assert!(matches!(s.unlock("0000"), Err(IdentityError::IncorrectPin)));
    }

    #[test]
    fn create_rejects_bad_pin() {
        let (_d, s) = store();
        assert!(matches!(s.create("12"), Err(IdentityError::BadPin)));
    }

    #[test]
    fn restore_from_phrase_rebuilds_identity() {
        let (_d, s) = store();
        let created = s.create("1234").expect("create");
        let phrase = created.recovery_phrase.clone();

        // A fresh store (new device) restores from the phrase under a new PIN.
        let (_d2, s2) = store();
        let restored = s2.restore(&phrase, "5678").expect("restore");
        assert_eq!(restored.key_bytes, created.key_bytes);
        assert_eq!(s2.unlock("5678").unwrap().key_bytes, created.key_bytes);

        assert!(matches!(
            s2.restore("totally invalid phrase here", "5678"),
            Err(IdentityError::InvalidPhrase)
        ));
    }

    #[test]
    fn migrate_plaintext_is_lossless_and_encrypts() {
        let (_d, s) = store();
        // Simulate a legacy plaintext identity on disk.
        let original = Keypair::generate();
        let raw = *original.signing_key_bytes();
        std::fs::write(s.key_path(), raw).unwrap();
        assert_eq!(s.status(), IdentityStatus::Plaintext);

        let created = s.migrate("1234").expect("migrate");
        // Same key preserved (lossless)...
        assert_eq!(created.key_bytes, raw);
        // ...now stored encrypted (not the raw bytes) and classed Encrypted.
        let on_disk = std::fs::read(s.key_path()).unwrap();
        assert_ne!(on_disk.as_slice(), &raw[..]);
        assert_eq!(s.status(), IdentityStatus::Encrypted);
        // Unlocks with the chosen PIN.
        assert_eq!(s.unlock("1234").unwrap().key_bytes, raw);
    }
}
