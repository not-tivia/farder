//! Per-channel MLS group state (`mls_state.json`) — the client-side resume
//! record for one E2EE channel's MLS group, the analog of `device.rs`'s
//! `DeviceState` but for the group rather than the event chain (Task 7 of
//! sub-project 4b).
//!
//! The MLS store itself lives at `servers/{log_server_id}/mls/{channel_id}.sqlite`
//! plus the raw 32-byte instance hash beside it — both owned by the
//! `farder-e2ee-client` crate. This file records the small amount of metadata
//! later tasks need to resume the group without re-deriving it from the log:
//! the generation, the local epoch, the (hex) store instance hash, whether
//! this device's own leaf has been confirmed, the control-plane fetch cursor,
//! and a terminal "poisoned" flag (set on a Gate-2 `LeafBindingFailure`, per
//! 4a's finding F4). T9 (steward) and T10 (decrypt) read it back.
//!
//! Plain JSON, keyed by the validated hex `log_server_id`, mirroring the
//! path-traversal guard in `device.rs` (and the crate's
//! `validate_log_server_id`, which this module reuses).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A single channel's persisted MLS group state.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MlsChannelState {
    /// The group generation (0 for the group minted at creation).
    pub generation: u64,
    /// The local group epoch at last save.
    pub epoch: u64,
    /// The 64-char hex store instance hash (`FarderMlsStore::store_instance_hash`),
    /// also persisted raw beside the store by the crate.
    pub store_instance_hash: String,
    /// Whether this device's own leaf has been confirmed.
    pub confirmed: bool,
    /// The MLS control-plane fetch cursor (`next_accept_seq`) the steward
    /// feeds back as `since_accept_seq` on its next `fetch_mls_control` (T9).
    /// `#[serde(default)]` so a pre-T9 record without the field resumes from 0.
    #[serde(default)]
    pub cursor: u64,
    /// `Some(reason)` once the group hit a `LeafBindingFailure` (finding F4 of
    /// the 4a vertical): the impostor leaf is already merged and cannot be
    /// rolled back, so the group is POISONED and must never be used further.
    /// The steward (T9) checks this on entry and refuses to continue; the
    /// frontend (T11) maps it to the "could not be confirmed" state.
    /// `#[serde(default)]` so a pre-T9 record without the field is healthy.
    #[serde(default)]
    pub poisoned: Option<String>,
}

impl MlsChannelState {
    fn path(log_server_id: &str, channel_id: u64) -> Result<PathBuf, String> {
        farder_e2ee_client::validate_log_server_id(log_server_id)?;
        Ok(crate::commands::farder_data_dir()
            .join("servers")
            .join(log_server_id)
            .join("mls")
            .join(format!("{channel_id}.mls_state.json")))
    }

    // `load` is the resume counterpart T9 (steward) / T10 (decrypt) use; T7
    // only writes, so it is dead until those land.
    #[allow(dead_code)]
    pub fn load(log_server_id: &str, channel_id: u64) -> Result<Option<Self>, String> {
        let path = Self::path(log_server_id, channel_id)?;
        match std::fs::read_to_string(&path) {
            Ok(data) => {
                let state: Self = serde_json::from_str(&data).map_err(|e| e.to_string())?;
                state.validate()?;
                Ok(Some(state))
            }
            Err(_) => Ok(None),
        }
    }

    pub fn save(&self, log_server_id: &str, channel_id: u64) -> Result<(), String> {
        let path = Self::path(log_server_id, channel_id)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }

    /// Validate the loaded hex field (defense in depth, same shape as
    /// `device.rs` server-id validation): the instance hash must be exactly
    /// 64 hex chars so a corrupted or partial write never silently resumes.
    #[allow(dead_code)] // only reachable via `load` (T9/T10)
    fn validate(&self) -> Result<(), String> {
        if self.store_instance_hash.len() == 64
            && self.store_instance_hash.chars().all(|c| c.is_ascii_hexdigit())
        {
            Ok(())
        } else {
            Err("invalid store_instance_hash".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mls_channel_state_serde_roundtrip_and_validation() {
        let state = MlsChannelState {
            generation: 0,
            epoch: 1,
            store_instance_hash: "ab".repeat(32),
            confirmed: true,
            cursor: 7,
            poisoned: None,
        };
        assert!(state.validate().is_ok());

        let json = serde_json::to_string(&state).unwrap();
        let back: MlsChannelState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, state);
        assert!(back.validate().is_ok());
    }

    #[test]
    fn mls_channel_state_rejects_a_non_hex_or_short_hash() {
        let non_hex = MlsChannelState {
            generation: 0,
            epoch: 0,
            store_instance_hash: "zz".repeat(32),
            confirmed: false,
            cursor: 0,
            poisoned: None,
        };
        assert!(non_hex.validate().is_err());

        let short = MlsChannelState {
            generation: 0,
            epoch: 0,
            store_instance_hash: "ab".repeat(31),
            confirmed: false,
            cursor: 0,
            poisoned: None,
        };
        assert!(short.validate().is_err());
    }

    #[test]
    fn mls_channel_state_loads_a_pre_t9_record_with_default_cursor_and_poison() {
        // A record written before T9 has no `cursor` / `poisoned` fields. The
        // serde defaults must fill them (cursor 0, healthy) so an upgrade never
        // refuses to resume a previously-working group.
        let legacy = format!(
            r#"{{"generation":0,"epoch":2,"store_instance_hash":"{}","confirmed":true}}"#,
            "ab".repeat(32)
        );
        let state: MlsChannelState = serde_json::from_str(&legacy).unwrap();
        assert_eq!(state.cursor, 0);
        assert_eq!(state.poisoned, None);
        assert!(state.validate().is_ok());
    }
}
