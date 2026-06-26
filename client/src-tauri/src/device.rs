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

/// Load the per-install device signing key, generating + persisting one on first
/// use. Stored as 32 raw bytes (the device subkey is low-stakes — it grants no
/// rights without an identity-signed DeviceCert; it can be regenerated).
pub fn load_or_create_device_keypair() -> Result<Keypair, String> {
    let path = device_key_path();
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() == 32 {
            let arr: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| "bad device key".to_string())?;
            return Ok(Keypair::from_signing_key_bytes(&arr));
        }
    }
    let kp = Keypair::generate();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, kp.signing_key_bytes()).map_err(|e| e.to_string())?;
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
}

impl DeviceState {
    pub fn fresh(device: &Keypair) -> Self {
        Self {
            device_id: device_id(&device.public_key()),
            next_seq: 0,
            last_event_hash: None,
            lamport: 0,
            authorized: false,
        }
    }

    fn path(server_id: &str) -> PathBuf {
        crate::commands::farder_data_dir()
            .join("servers")
            .join(server_id)
            .join("device_state.json")
    }

    pub fn load(server_id: &str) -> Result<Option<Self>, String> {
        let path = Self::path(server_id);
        match std::fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).map(Some).map_err(|e| e.to_string()),
            Err(_) => Ok(None),
        }
    }

    pub fn save(&self, server_id: &str) -> Result<(), String> {
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
        let json = serde_json::to_string(&st).unwrap();
        let back: DeviceState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.next_seq, 3);
        assert_eq!(back.last_event_hash.as_deref(), Some("abc"));
        assert_eq!(back.lamport, 9);
        assert!(back.authorized);
        assert_eq!(back.device_id, device_id(&device.public_key()));
    }
}
