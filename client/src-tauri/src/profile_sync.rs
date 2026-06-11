//! Builds the per-server effective signed profile and pushes it to servers.
//!
//! Effective profile = (per-server avatar override ?? global avatar.png),
//! global status, current display name — signed fresh per push. A hash of the
//! last successfully pushed profile is tracked per server in
//! `pushed_profiles.json` so reconnects don't re-upload unchanged profiles.

use std::path::PathBuf;
use std::sync::Arc;

use farder_crypto::identity::Keypair;
use farder_crypto::profile::{profile_hash_hex, SignedProfile};
use farder_protocol::server::{ServerRequest, ServerResponse};

use crate::commands::farder_data_dir;
use crate::state::AppState;

fn overrides_dir() -> PathBuf {
    let d = farder_data_dir().join("profile_overrides");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn safe_server_name(server_id: &str) -> String {
    server_id.replace([':', '.', '/'], "_")
}

pub(crate) fn override_path(server_id: &str) -> PathBuf {
    overrides_dir().join(format!("{}.img", safe_server_name(server_id)))
}

/// Override avatar if set for this server, else the global avatar, else none.
pub(crate) fn effective_avatar_bytes(server_id: &str) -> Option<Vec<u8>> {
    if let Ok(bytes) = std::fs::read(override_path(server_id)) {
        return Some(bytes);
    }
    std::fs::read(farder_data_dir().join("avatar.png")).ok()
}

fn read_profile_field(field: &str) -> Option<String> {
    let data = std::fs::read_to_string(farder_data_dir().join("profile.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v[field].as_str().map(|s| s.to_string())
}

pub(crate) fn build_signed_profile(keypair: &Keypair, server_id: &str) -> SignedProfile {
    let display_name = read_profile_field("display_name").unwrap_or_else(|| "Anonymous".to_string());
    let status = read_profile_field("status").filter(|s| !s.is_empty());
    SignedProfile::create(keypair, display_name, effective_avatar_bytes(server_id), status)
}

// --- last-pushed-hash tracking ---------------------------------------------

fn pushed_map_path() -> PathBuf {
    farder_data_dir().join("pushed_profiles.json")
}

fn read_pushed_map() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(pushed_map_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn pushed_hash(server_id: &str) -> Option<String> {
    read_pushed_map().get(server_id)?.as_str().map(|s| s.to_string())
}

fn record_pushed(server_id: &str, hash: &str) {
    let mut map = read_pushed_map();
    map.insert(server_id.to_string(), serde_json::json!(hash));
    let _ = std::fs::write(pushed_map_path(), serde_json::Value::Object(map).to_string());
}

// --- pushing ----------------------------------------------------------------

/// Push the effective profile to one connected server if it changed since the
/// last successful push. No-op when the identity is locked.
pub(crate) async fn push_profile(state: &AppState, server_id: &str) -> Result<(), String> {
    let keypair = {
        let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
        match lock.as_ref() {
            Some(bytes) => Keypair::from_signing_key_bytes(bytes),
            None => return Ok(()), // locked — nothing to push yet
        }
    };
    let bytes = build_signed_profile(&keypair, server_id).to_bytes();
    let hash = profile_hash_hex(&bytes);
    if pushed_hash(server_id).as_deref() == Some(hash.as_str()) {
        return Ok(());
    }
    match crate::bridge::send_request(state, server_id, ServerRequest::UpdateProfile { profile: bytes }).await {
        Ok(ServerResponse::Ok) => {
            record_pushed(server_id, &hash);
            Ok(())
        }
        Ok(ServerResponse::Error { reason }) => Err(reason),
        Ok(other) => Err(format!("unexpected response: {:?}", other)),
        Err(e) => Err(e.to_string()),
    }
}

/// Push to every connected server (each gets its own effective profile, so
/// per-server overrides are respected automatically).
pub(crate) async fn push_profile_everywhere(state: &Arc<AppState>) {
    let ids: Vec<String> = state.servers.lock().unwrap().keys().cloned().collect();
    for id in ids {
        if let Err(e) = push_profile(state, &id).await {
            eprintln!("[profile-sync] push to {} failed: {}", id, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // One combined test: FARDER_DATA is process-global, so parallel tests would
    // race if split. Everything filesystem-dependent lives here.
    #[test]
    fn test_override_resolution_and_pushed_map() {
        let tmp = std::env::temp_dir().join(format!("farder-profile-sync-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe { std::env::set_var("FARDER_DATA", &tmp); }

        // No avatars at all -> None.
        assert!(effective_avatar_bytes("srv-a").is_none());

        // Global only -> global for every server.
        std::fs::write(tmp.join("avatar.png"), [1u8, 2, 3]).unwrap();
        assert_eq!(effective_avatar_bytes("srv-a").as_deref(), Some(&[1u8, 2, 3][..]));
        assert_eq!(effective_avatar_bytes("srv-b").as_deref(), Some(&[1u8, 2, 3][..]));

        // Override on srv-a wins there, global elsewhere.
        std::fs::write(override_path("srv-a"), [9u8]).unwrap();
        assert_eq!(effective_avatar_bytes("srv-a").as_deref(), Some(&[9u8][..]));
        assert_eq!(effective_avatar_bytes("srv-b").as_deref(), Some(&[1u8, 2, 3][..]));

        // Signed profile picks up display name + status + per-server avatar.
        std::fs::write(
            tmp.join("profile.json"),
            r#"{"display_name":"Tester","status":"hi there"}"#,
        ).unwrap();
        let kp = Keypair::generate();
        let p = build_signed_profile(&kp, "srv-a");
        assert!(p.verify().is_ok());
        assert_eq!(p.display_name(), "Tester");
        assert_eq!(p.data.status.as_deref(), Some("hi there"));
        assert_eq!(p.data.avatar.as_deref(), Some(&[9u8][..]));

        // Pushed-map roundtrip.
        assert!(pushed_hash("srv-a").is_none());
        record_pushed("srv-a", "abc123");
        assert_eq!(pushed_hash("srv-a").as_deref(), Some("abc123"));
        record_pushed("srv-a", "def456");
        assert_eq!(pushed_hash("srv-a").as_deref(), Some("def456"));

        unsafe { std::env::remove_var("FARDER_DATA"); }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
