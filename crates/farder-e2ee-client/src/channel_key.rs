//! [`ChannelKey`] and the on-disk layout for a channel's MLS store.
//!
//! The layout mirrors the client's existing per-server state convention
//! (`client/src-tauri/src/device.rs`): `servers/{server_id}/device_state.json`.
//! For an E2EE channel the MLS store lives at
//! `servers/{log_server_id}/mls/{channel_id}.sqlite`, with the store instance
//! hash persisted *beside* it (fact A2.12: `FarderMlsStore::create` refuses an
//! existing path and `resume` needs the hash stored durably).

use std::path::{Path, PathBuf};

/// Identifies one MLS group: the server whose event log hosts it, plus the
/// channel id the client chose (see `E2EE_CHANNEL_ID_FLOOR`).
///
/// `log_server_id` is a server-supplied hex SHA-256 id. Because it is used to
/// build a filesystem path, it is validated on construction *and* on every path
/// build (defense in depth, same guard shape as `device::validate_server_id`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ChannelKey {
    pub log_server_id: String,
    pub channel_id: u64,
}

/// Validate a server-supplied id before it is used as a path component.
///
/// The shape mirrors `client/src-tauri/src/device.rs::validate_server_id`:
/// non-empty, hex-only, and length-bounded, so a malicious server cannot smuggle
/// path separators or `..` into a directory name (path traversal).
pub fn validate_log_server_id(log_server_id: &str) -> Result<(), String> {
    if log_server_id.len() <= 128
        && !log_server_id.is_empty()
        && log_server_id.chars().all(|c| c.is_ascii_hexdigit())
    {
        Ok(())
    } else {
        Err("invalid log_server_id".to_string())
    }
}

impl ChannelKey {
    /// Construct a key, validating `log_server_id`.
    pub fn new(log_server_id: impl Into<String>, channel_id: u64) -> Result<Self, String> {
        let log_server_id = log_server_id.into();
        validate_log_server_id(&log_server_id)?;
        Ok(Self {
            log_server_id,
            channel_id,
        })
    }

    /// Absolute path to the MLS store, under `data_dir`.
    ///
    /// `servers/{log_server_id}/mls/{channel_id}.sqlite`.
    pub fn mls_store_path(&self, data_dir: &Path) -> Result<PathBuf, String> {
        validate_log_server_id(&self.log_server_id)?;
        Ok(self.mls_dir(data_dir).join(format!("{}.sqlite", self.channel_id)))
    }

    /// Absolute path to the store instance hash, persisted beside the MLS
    /// store.
    ///
    /// `servers/{log_server_id}/mls/{channel_id}.instance_hash`.
    pub fn instance_hash_path(&self, data_dir: &Path) -> Result<PathBuf, String> {
        validate_log_server_id(&self.log_server_id)?;
        Ok(self
            .mls_dir(data_dir)
            .join(format!("{}.instance_hash", self.channel_id)))
    }

    fn mls_dir(&self, data_dir: &Path) -> PathBuf {
        data_dir
            .join("servers")
            .join(&self.log_server_id)
            .join("mls")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_ID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    #[test]
    fn rejects_path_traversal_in_log_server_id() {
        assert!(ChannelKey::new("../../etc", 1).is_err());
        assert!(ChannelKey::new("../..\\windows\\system32", 1).is_err());
        assert!(ChannelKey::new("a/b", 1).is_err());
    }

    #[test]
    fn rejects_non_hex_empty_and_overlong_ids() {
        // Non-hex characters (dash, space, `g`).
        assert!(ChannelKey::new("abc-123", 1).is_err());
        assert!(ChannelKey::new("dead beef", 1).is_err());
        assert!(ChannelKey::new("zz", 1).is_err());
        // Empty.
        assert!(ChannelKey::new("", 1).is_err());
        // Over the 128-char bound.
        let overlong = "a".repeat(129);
        assert!(ChannelKey::new(&overlong, 1).is_err());
        // Exactly 128 is fine.
        let max = "a".repeat(128);
        assert!(ChannelKey::new(&max, 1).is_ok());
    }

    #[test]
    fn accepts_valid_id_and_builds_expected_layout() {
        let key = ChannelKey::new(VALID_ID.to_string(), 42).unwrap();
        let data_dir = Path::new("/home/deez/.local/share/farder");

        assert_eq!(
            key.mls_store_path(data_dir).unwrap(),
            data_dir
                .join("servers")
                .join(VALID_ID)
                .join("mls")
                .join("42.sqlite")
        );
        assert_eq!(
            key.instance_hash_path(data_dir).unwrap(),
            data_dir
                .join("servers")
                .join(VALID_ID)
                .join("mls")
                .join("42.instance_hash")
        );
    }

    #[test]
    fn path_build_revalidates_a_hand_built_key() {
        // A ChannelKey can be constructed without `new` (struct literal), so the
        // path builders must not trust the stored id.
        let key = ChannelKey {
            log_server_id: "../../etc".to_string(),
            channel_id: 7,
        };
        assert!(key.mls_store_path(Path::new("/tmp")).is_err());
        assert!(key.instance_hash_path(Path::new("/tmp")).is_err());
    }
}
