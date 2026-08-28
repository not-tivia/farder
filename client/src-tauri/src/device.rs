//! Per-install device subkey + per-(server, device) chain/clock state for the
//! mesh event log. Events are signed by the DEVICE subkey; the identity key
//! authorizes the device via a DeviceCert.

use std::path::PathBuf;

use farder_crypto::event_log::{device_id, DeviceCert};
use farder_crypto::identity::Keypair;
use serde::{Deserialize, Serialize};

fn device_key_path() -> PathBuf {
    crate::commands::farder_data_dir().join("device.key")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// HKDF info for the key that wraps the device subkey at rest. Versioned:
/// changing it is a key rotation that makes every existing wrapped file
/// unreadable.
const DEVICE_KEY_INFO: &[u8] = b"farder-device-key-v1";

/// Wrapped-file layout: a 12-byte nonce followed by the AES-256-GCM ciphertext
/// of the 32-byte subkey. 32 bytes exactly means a LEGACY plaintext file.
const NONCE_LEN: usize = 12;

fn wrap_device_key(identity_seed: &[u8; 32], subkey: &[u8; 32]) -> Result<Vec<u8>, String> {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    let kek = farder_history::derive_local_key(identity_seed, DEVICE_KEY_INFO);
    let cipher = Aes256Gcm::new_from_slice(&kek).map_err(|e| e.to_string())?;
    let nonce_bytes: [u8; NONCE_LEN] = rand::random();
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), subkey.as_slice())
        .map_err(|_| "wrap device key".to_string())?;
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&ct);
    Ok(out)
}

fn unwrap_device_key(identity_seed: &[u8; 32], blob: &[u8]) -> Result<[u8; 32], String> {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    if blob.len() <= NONCE_LEN {
        return Err("device key file is truncated".to_string());
    }
    let kek = farder_history::derive_local_key(identity_seed, DEVICE_KEY_INFO);
    let cipher = Aes256Gcm::new_from_slice(&kek).map_err(|e| e.to_string())?;
    let plain = cipher
        .decrypt(Nonce::from_slice(&blob[..NONCE_LEN]), &blob[NONCE_LEN..])
        .map_err(|_| "device key does not belong to this identity".to_string())?;
    plain
        .as_slice()
        .try_into()
        .map_err(|_| "device key has the wrong length".to_string())
}

/// Load the per-install device signing key, generating + persisting one on first
/// use, **wrapped at rest** under a key derived from the unlocked identity.
///
/// It used to be written as 32 raw bytes, justified as "low-stakes — it grants no
/// rights without an identity-signed DeviceCert". That reasoning does not hold:
/// the DeviceCert authorizing this subkey is PUBLIC in the log, so the bare file
/// is the whole of what it takes to author events as this device. It also sat
/// next to the sealed history archive, which made wrapping that archive theatre.
///
/// A legacy 32-byte plaintext file is migrated in place on first load: read,
/// wrap, overwrite. Every caller already unlocks the identity immediately before
/// calling this, so the seed is passed in rather than fetched from ambient state.
pub fn load_or_create_device_keypair(identity_seed: &[u8; 32]) -> Result<Keypair, String> {
    let path = device_key_path();
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() == 32 {
            // LEGACY plaintext file: adopt the key, then re-persist it wrapped.
            let arr: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| "bad device key".to_string())?;
            let wrapped = wrap_device_key(identity_seed, &arr)?;
            std::fs::write(&path, &wrapped).map_err(|e| e.to_string())?;
            return Ok(Keypair::from_signing_key_bytes(&arr));
        }
        if !bytes.is_empty() {
            let arr = unwrap_device_key(identity_seed, &bytes)?;
            return Ok(Keypair::from_signing_key_bytes(&arr));
        }
    }
    let kp = Keypair::generate();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let wrapped = wrap_device_key(identity_seed, kp.signing_key_bytes())?;
    std::fs::write(&path, &wrapped).map_err(|e| e.to_string())?;
    Ok(kp)
}

/// The identity authorizes the device subkey.
pub fn device_cert(identity: &Keypair, device: &Keypair) -> DeviceCert {
    DeviceCert::create(identity, &device.public_key(), now_secs())
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeviceState {
    pub device_id: String,
    pub next_seq: u64,
    pub last_event_hash: Option<String>,
    pub lamport: u64,
    /// Whether this device has already submitted its DeviceAuthorized to the server.
    pub authorized: bool,
    /// Whether this identity has already submitted its MemberJoined to the server.
    #[serde(default)]
    pub joined: bool,
}

impl DeviceState {
    pub fn fresh(device: &Keypair) -> Self {
        Self {
            device_id: device_id(&device.public_key()),
            next_seq: 0,
            last_event_hash: None,
            lamport: 0,
            authorized: false,
            joined: false,
        }
    }

    fn path(server_id: &str) -> PathBuf {
        crate::commands::farder_data_dir()
            .join("servers")
            .join(server_id)
            .join("device_state.json")
    }

    /// `server_id` is a hex SHA-256 from the server; reject anything else so a
    /// malicious server can't smuggle path separators / `..` into the directory
    /// name (path traversal). Length-bounded too.
    fn validate_server_id(server_id: &str) -> Result<(), String> {
        if server_id.len() <= 128 && !server_id.is_empty() && server_id.chars().all(|c| c.is_ascii_hexdigit()) {
            Ok(())
        } else {
            Err("invalid server_id".to_string())
        }
    }

    pub fn load(server_id: &str) -> Result<Option<Self>, String> {
        Self::validate_server_id(server_id)?;
        let path = Self::path(server_id);
        match std::fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).map(Some).map_err(|e| e.to_string()),
            Err(_) => Ok(None),
        }
    }

    pub fn save(&self, server_id: &str) -> Result<(), String> {
        Self::validate_server_id(server_id)?;
        let path = Self::path(server_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_cert_authorizes_and_verifies() {
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let cert = device_cert(&identity, &device);
        assert!(cert.verify().is_ok());
        assert_eq!(cert.core.identity, identity.public_key());
        assert_eq!(cert.core.device_id, device_id(&device.public_key()));
    }

    #[test]
    fn device_state_fresh_and_serde_roundtrip() {
        let device = Keypair::generate();
        let mut st = DeviceState::fresh(&device);
        assert_eq!(st.next_seq, 0);
        assert!(st.last_event_hash.is_none());
        assert!(!st.authorized);
        st.next_seq = 3;
        st.last_event_hash = Some("abc".into());
        st.lamport = 9;
        st.authorized = true;
        st.joined = true;
        let json = serde_json::to_string(&st).unwrap();
        let back: DeviceState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.next_seq, 3);
        assert_eq!(back.last_event_hash.as_deref(), Some("abc"));
        assert_eq!(back.lamport, 9);
        assert!(back.authorized);
        assert!(back.joined);
        assert_eq!(back.device_id, device_id(&device.public_key()));
    }
}

#[cfg(test)]
mod at_rest_tests {
    use super::*;

    /// One combined test: `FARDER_DATA` is process-global, so split tests race.
    #[test]
    fn the_device_key_is_wrapped_at_rest_and_a_legacy_plaintext_file_migrates() {
        let tmp = std::env::temp_dir().join(format!("farder-devkey-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe { std::env::set_var("FARDER_DATA", &tmp) };
        let path = device_key_path();
        let identity = [11u8; 32];

        // 1. First use mints a key and writes it WRAPPED — never 32 raw bytes.
        let kp = load_or_create_device_keypair(&identity).unwrap();
        let on_disk = std::fs::read(&path).unwrap();
        assert_ne!(on_disk.len(), 32, "a 32-byte file is the legacy plaintext shape");
        assert!(
            !on_disk.windows(32).any(|w| w == kp.signing_key_bytes().as_slice()),
            "the raw device subkey must not appear anywhere in the file"
        );

        // 2. It round-trips for the same identity.
        let again = load_or_create_device_keypair(&identity).unwrap();
        assert_eq!(again.signing_key_bytes(), kp.signing_key_bytes());

        // 3. A DIFFERENT identity cannot unwrap it.
        let err = match load_or_create_device_keypair(&[12u8; 32]) {
            Err(e) => e,
            Ok(_) => panic!("another identity must not be able to unwrap this device key"),
        };
        assert!(err.contains("does not belong to this identity"), "got {err}");

        // 4. A legacy 32-byte plaintext file is adopted AND migrated in place.
        let legacy = Keypair::generate();
        std::fs::write(&path, legacy.signing_key_bytes()).unwrap();
        let loaded = load_or_create_device_keypair(&identity).unwrap();
        assert_eq!(
            loaded.signing_key_bytes(),
            legacy.signing_key_bytes(),
            "migration must keep the SAME device key — a new one would orphan the DeviceCert in the log"
        );
        let after = std::fs::read(&path).unwrap();
        assert_ne!(after.len(), 32, "the legacy file must have been rewritten wrapped");
        assert!(
            !after.windows(32).any(|w| w == legacy.signing_key_bytes().as_slice()),
            "the migrated file still contains the raw key"
        );

        unsafe { std::env::remove_var("FARDER_DATA") };
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
