use crate::bridge;
use crate::connection::connect_and_authenticate;
use crate::connection::connect_via_relay;
use crate::state::{AppState, ServerConnection};
use crate::tls::make_client_endpoint;
use farder_crypto::identity::Keypair;
use farder_protocol::server::{
    BotAlertInfo, CategoryInfo, ChannelInfo, ChannelInfoV2, CommandInfo, EventInfo, GiveawayInfo,
    MemberInfo, MessageInfo, MessageInfoV2, PollInfo, ReminderInfo, RoleInfo, ServerRequest,
    ServerResponse, WebhookInfo, SERVER_PROTOCOL_VERSION,
};
use std::collections::HashMap;
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, State};

// ---------------------------------------------------------------------------
// Profile helpers
// ---------------------------------------------------------------------------

pub(crate) fn farder_data_dir() -> std::path::PathBuf {
    let dir = if let Ok(custom) = std::env::var("FARDER_DATA") {
        std::path::PathBuf::from(custom)
    } else {
        dirs::home_dir()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
            .join(".farder")
    };
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn profile_path() -> std::path::PathBuf {
    farder_data_dir().join("profile.json")
}

fn settings_path() -> std::path::PathBuf {
    farder_data_dir().join("settings.json")
}

// ---------------------------------------------------------------------------
// IPC return types
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct ConnectResult {
    pub server_name: String,
    pub member_count: u32,
    pub channels: Vec<ChannelInfo>,
    pub categories: Vec<CategoryInfo>,
    pub roles: Vec<RoleInfo>,
    pub owner_public_key: Option<farder_crypto::identity::PublicKey>,
    pub relayed: bool,
    pub server_id: Option<String>,
}

#[derive(serde::Serialize)]
pub struct SendMessageResult {
    pub id: u64,
    pub timestamp: u64,
}

/// The v2 server-info surface (`GetServerInfoV2`): `ConnectResult`'s shape
/// minus the connection-only `relayed` flag, with the class-carrying channel
/// list. Field names mirror `ConnectResult` so the frontend can reuse the same
/// consumption path once it threads `class` through (T3).
#[derive(serde::Serialize)]
pub struct ServerInfoV2Result {
    pub server_name: String,
    pub member_count: u32,
    pub channels: Vec<ChannelInfoV2>,
    pub categories: Vec<CategoryInfo>,
    pub roles: Vec<RoleInfo>,
    pub owner_public_key: Option<farder_crypto::identity::PublicKey>,
    pub server_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Identity commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_public_key(state: State<'_, Arc<AppState>>) -> Option<String> {
    let lock = state.signing_key_bytes.lock().ok()?;
    lock.as_ref().map(|bytes| {
        Keypair::from_signing_key_bytes(bytes).public_key().to_string()
    })
}

// ---------------------------------------------------------------------------
// Display name commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn set_display_name(state: State<'_, Arc<AppState>>, name: String) -> Result<(), String> {
    let trimmed = name.trim().to_string();
    if trimmed.is_empty() {
        return Err("display name cannot be empty".to_string());
    }
    if trimmed.chars().count() > 128 {
        return Err("display name too long (max 128 characters)".to_string());
    }
    let path = profile_path();
    let mut data: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    data["display_name"] = serde_json::json!(trimmed);
    std::fs::write(&path, data.to_string()).map_err(|e| e.to_string())?;
    // Propagate to every connected server so member lists + message authorship
    // show the new name (mirrors set_profile_status). Without this the name only
    // lived locally and servers kept the auto-assigned "vk_…" → "Anonymous".
    let state_arc = Arc::clone(state.inner());
    tokio::spawn(async move { crate::profile_sync::push_profile_everywhere(&state_arc).await; });
    Ok(())
}

#[tauri::command]
pub fn get_display_name() -> Option<String> {
    let path = profile_path();
    let data = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v["display_name"].as_str().map(|s| s.to_string())
}

#[tauri::command]
pub fn set_bio(bio: String) -> Result<(), String> {
    let path = profile_path();
    let mut data: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    data["bio"] = serde_json::json!(bio);
    std::fs::write(&path, data.to_string()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_bio() -> Option<String> {
    let path = profile_path();
    let data = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v["bio"].as_str().map(|s| s.to_string())
}

#[tauri::command]
pub fn set_profile_color(color: String) -> Result<(), String> {
    let path = profile_path();
    let mut data: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    data["banner_color"] = serde_json::json!(color);
    std::fs::write(&path, data.to_string()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_profile_color() -> Option<String> {
    let path = profile_path();
    let data = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v["banner_color"].as_str().map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Avatar commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn set_avatar(state: State<'_, Arc<AppState>>, file_path: String) -> Result<String, String> {
    let data = std::fs::read(&file_path).map_err(|e| e.to_string())?;
    crate::profile_sync::validate_avatar_bytes(&data)?;
    let avatar_path = farder_data_dir().join("avatar.png");
    std::fs::write(&avatar_path, &data).map_err(|e| e.to_string())?;
    let state_arc = Arc::clone(state.inner());
    tokio::spawn(async move { crate::profile_sync::push_profile_everywhere(&state_arc).await; });
    Ok(image_data_url(&data))
}

#[tauri::command]
pub fn get_avatar() -> Option<String> {
    let avatar_path = farder_data_dir().join("avatar.png");
    if !avatar_path.exists() {
        return None;
    }
    let data = std::fs::read(&avatar_path).ok()?;
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
    Some(format!("data:image/png;base64,{}", b64))
}

// ---------------------------------------------------------------------------
// Server avatar commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn set_server_avatar(server_id: String, file_path: String) -> Result<String, String> {
    let data = std::fs::read(&file_path).map_err(|e| e.to_string())?;
    let dir = farder_data_dir().join("server_avatars");
    let _ = std::fs::create_dir_all(&dir);
    let safe_name = server_id.replace([':', '.', '/'], "_");
    let avatar_path = dir.join(format!("{}.png", safe_name));
    std::fs::write(&avatar_path, &data).map_err(|e| e.to_string())?;

    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
    Ok(format!("data:image/png;base64,{}", b64))
}

#[tauri::command]
pub fn get_server_avatar(server_id: String) -> Option<String> {
    let safe_name = server_id.replace([':', '.', '/'], "_");
    let avatar_path = farder_data_dir()
        .join("server_avatars")
        .join(format!("{}.png", safe_name));
    if !avatar_path.exists() {
        return None;
    }
    let data = std::fs::read(&avatar_path).ok()?;
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
    Some(format!("data:image/png;base64,{}", b64))
}

/// Build a data: URL for raw image bytes, sniffing the mime from magic bytes.
pub(crate) fn image_data_url(data: &[u8]) -> String {
    let mime = if data.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        "image/png"
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if data.starts_with(b"GIF8") {
        "image/gif"
    } else if data.len() > 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "application/octet-stream"
    };
    use base64::Engine;
    format!("data:{};base64,{}", mime, base64::engine::general_purpose::STANDARD.encode(data))
}

#[tauri::command]
pub fn get_profile_status() -> Option<String> {
    let data = std::fs::read_to_string(profile_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v["status"].as_str().map(|s| s.to_string())
}

#[tauri::command]
pub async fn set_profile_status(
    state: State<'_, Arc<AppState>>,
    status: Option<String>,
) -> Result<(), String> {
    let trimmed = status.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(s) = &trimmed {
        if s.chars().count() > 128 {
            return Err("status too long (max 128 characters)".to_string());
        }
    }
    let path = profile_path();
    let mut data: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    match &trimmed {
        Some(s) => data["status"] = serde_json::json!(s),
        None => data["status"] = serde_json::Value::Null,
    }
    std::fs::write(&path, data.to_string()).map_err(|e| e.to_string())?;
    let state_arc = Arc::clone(state.inner());
    tokio::spawn(async move { crate::profile_sync::push_profile_everywhere(&state_arc).await; });
    Ok(())
}

#[tauri::command]
pub async fn set_server_avatar_override(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    file_path: String,
) -> Result<String, String> {
    let data = std::fs::read(&file_path).map_err(|e| e.to_string())?;
    crate::profile_sync::validate_avatar_bytes(&data)?;
    std::fs::write(crate::profile_sync::override_path(&server_id), &data).map_err(|e| e.to_string())?;
    crate::profile_sync::push_profile(&state, &server_id).await
        .map_err(|e| format!("saved locally, but couldn't sync to this server: {}", e))?;
    Ok(image_data_url(&data))
}

#[tauri::command]
pub async fn clear_server_avatar_override(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<(), String> {
    let _ = std::fs::remove_file(crate::profile_sync::override_path(&server_id));
    crate::profile_sync::push_profile(&state, &server_id).await
        .map_err(|e| format!("saved locally, but couldn't sync to this server: {}", e))?;
    Ok(())
}

#[tauri::command]
pub fn get_server_avatar_override(server_id: String) -> Option<String> {
    let data = std::fs::read(crate::profile_sync::override_path(&server_id)).ok()?;
    Some(image_data_url(&data))
}

#[derive(serde::Serialize)]
pub struct MemberProfileView {
    pub avatar_data_url: Option<String>,
    pub status: Option<String>,
}

/// Resolve a member's profile by its hash: disk cache first, otherwise fetch
/// from the server and verify (signature, key match, hash match) before caching.
/// LAZY ONLY — never call at module load (PIN-lock; see eb1511d lesson).
#[tauri::command]
pub async fn get_member_profile(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    public_key: String,
    profile_hash: Option<String>,
) -> Result<Option<MemberProfileView>, String> {
    use farder_crypto::profile::{profile_hash_hex, SignedProfile};

    let Some(hash) = profile_hash else { return Ok(None) };
    // The hash is used as a filename — accept only 64 hex chars.
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(None);
    }
    // Normalize to lowercase: hex::encode produces lowercase, so uppercase
    // input would always miss the cache and mismatch the hash check.
    let hash = hash.to_lowercase();

    // Parse the public key once up front so both paths share the same value.
    let pk = parse_public_key(&public_key)?;

    let cache_dir = farder_data_dir().join("profile_cache");
    let _ = std::fs::create_dir_all(&cache_dir);
    let cache_path = cache_dir.join(&hash);

    // Try to load a verified SignedProfile from the on-disk cache.
    // Cache is keyed by hash alone and the hash comes from the server's member
    // list — re-check the key binding so a lying server can't repoint one
    // member's hash at another's cached profile.
    fn load_verified(
        bytes: &[u8],
        pk: &farder_crypto::identity::PublicKey,
    ) -> Option<SignedProfile> {
        let signed = SignedProfile::from_bytes(bytes).ok()?;
        signed.verify().ok()?;
        if signed.data.public_key != *pk {
            return None;
        }
        Some(signed)
    }

    // Try the cache first; any failure (missing, corrupt, tampered, wrong key)
    // resolves to None and we fall through to one shared network fetch.
    let cached: Option<SignedProfile> = match std::fs::read(&cache_path) {
        Ok(cached_bytes) => {
            let verified = load_verified(&cached_bytes, &pk);
            if verified.is_none() {
                // Corrupt, tampered, or wrong-key cache entry — self-heal.
                let _ = std::fs::remove_file(&cache_path);
            }
            verified
        }
        Err(_) => None,
    };

    let signed: SignedProfile = match cached {
        Some(s) => s,
        None => {
            let response = bridge::send_request(
                &state, &server_id,
                ServerRequest::GetMemberProfile { member_key: pk.clone() },
            ).await.map_err(|e| e.to_string())?;
            let bytes = match response {
                ServerResponse::MemberProfile { profile: Some(b), .. } => b,
                ServerResponse::MemberProfile { profile: None, .. } => return Ok(None),
                ServerResponse::Error { reason } => return Err(reason),
                other => return Err(format!("unexpected response: {:?}", other)),
            };
            let signed = SignedProfile::from_bytes(&bytes).map_err(|e| e.to_string())?;
            signed.verify().map_err(|_| "profile signature invalid".to_string())?;
            if signed.data.public_key != pk {
                return Err("profile public key mismatch".to_string());
            }
            if profile_hash_hex(&bytes) != hash {
                return Err("profile hash mismatch".to_string());
            }
            let _ = std::fs::write(&cache_path, &bytes);
            signed
        }
    };

    Ok(Some(MemberProfileView {
        avatar_data_url: signed.data.avatar.as_deref().map(image_data_url),
        status: signed.data.status,
    }))
}

// ---------------------------------------------------------------------------
// Invite preview command
// ---------------------------------------------------------------------------

#[derive(Clone, serde::Serialize)]
pub struct InvitePreviewResult {
    /// "ok" | "invalid" | "unavailable" | "none" (none = link carries no
    /// previewable invite code, e.g. setup-token or bare-address links).
    pub status: String,
    pub server_name: Option<String>,
    pub member_count: Option<u32>,
    pub online_count: Option<u32>,
}

/// Session-scoped preview cache: link → (when, result). 60s TTL mirrors the
/// relay-side cache; previews are point-in-time data.
static PREVIEW_CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, InvitePreviewResult)>>> =
    std::sync::OnceLock::new();

fn preview_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, InvitePreviewResult)>> {
    PREVIEW_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

async fn fetch_preview_via_relay(
    relay_addr: std::net::SocketAddr,
    relay_fp: Vec<u8>,
    target: farder_protocol::messages::PreviewTarget,
    code: String,
) -> Result<farder_protocol::messages::PreviewOutcome, String> {
    use farder_protocol::messages::Message;
    let endpoint = crate::tls::make_pinned_relay_endpoint(relay_fp).map_err(|e| e.to_string())?;
    let conn = endpoint
        .connect(relay_addr, "farder-relay")
        .map_err(|e| e.to_string())?
        .await
        .map_err(|e| e.to_string())?;
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
    let msg = farder_protocol::codec::encode(&Message::ProxyInvitePreview { target, code })
        .map_err(|e| e.to_string())?;
    crate::connection::write_frame(&mut send, &msg)
        .await
        .map_err(|e| e.to_string())?;
    let reply_bytes = crate::connection::read_frame(&mut recv)
        .await
        .map_err(|e| e.to_string())?;
    let reply: Message = farder_protocol::codec::decode(&reply_bytes).map_err(|e| e.to_string())?;
    conn.close(0u32.into(), b"preview done");
    // endpoint is dropped here, which is fine — the connection is already closed
    match reply {
        Message::ProxyInvitePreviewResult { outcome } => Ok(outcome),
        other => Err(format!("unexpected relay reply: {:?}", other)),
    }
}

/// Fetch an invite preview through a relay (the link's own relay for relayed
/// invites; the default Farder relay for direct invites). Throwaway connection;
/// never touches session connections. LAZY ONLY (PIN-lock rule) — needs no
/// identity at all: previews are anonymous.
#[tauri::command]
pub async fn get_invite_preview(link: String) -> Result<InvitePreviewResult, String> {
    use farder_protocol::messages::{PreviewOutcome, PreviewTarget};

    let none_result = InvitePreviewResult {
        status: "none".into(),
        server_name: None,
        member_count: None,
        online_count: None,
    };

    // Cache first.
    {
        let cache = preview_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, hit)) = cache.get(&link) {
            if at.elapsed() < std::time::Duration::from_secs(60) {
                return Ok(hit.clone());
            }
        }
    }

    // Web-wrapped invites (https://farder.gg/join/<b64>) — the form create_invite
    // hands out — carry the real deep link inside; unwrap before parsing.
    let effective: String =
        crate::connection::unwrap_web_invite(&link).unwrap_or_else(|| link.clone());

    // Work out (relay endpoint, target, code) from the link form.
    let (relay_addr, relay_fp, target, code) =
        if let Some(t) = crate::connection::parse_relay_target(&effective) {
            if t.invite_token.is_empty() {
                return Ok(none_result);
            }
            (
                t.relay_addr,
                t.cert_fp.clone(),
                PreviewTarget::Registered { server_id: t.server_id.clone() },
                t.invite_token.clone(),
            )
        } else if let Some((addr, code)) = crate::connection::parse_direct_invite(&effective) {
            let Some((def_addr, def_fp)) = crate::default_relay::default_relay() else {
                return Ok(none_result); // no default relay in this build → no direct previews
            };
            (def_addr, def_fp, PreviewTarget::Direct { addr }, code)
        } else {
            return Ok(none_result);
        };

    // 8s client-side budget (the relay's own budget is 5s).
    let outcome = match tokio::time::timeout(
        std::time::Duration::from_secs(8),
        fetch_preview_via_relay(relay_addr, relay_fp, target, code),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(_)) | Err(_) => PreviewOutcome::Unavailable,
    };

    let result = match outcome {
        PreviewOutcome::Preview { server_name, member_count, online_count } => InvitePreviewResult {
            status: "ok".into(),
            server_name: Some(server_name.chars().take(80).collect()),
            member_count: Some(member_count),
            online_count: Some(online_count),
        },
        PreviewOutcome::Invalid => InvitePreviewResult {
            status: "invalid".into(),
            server_name: None,
            member_count: None,
            online_count: None,
        },
        PreviewOutcome::Unavailable => InvitePreviewResult {
            status: "unavailable".into(),
            server_name: None,
            member_count: None,
            online_count: None,
        },
    };

    preview_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(link, (std::time::Instant::now(), result.clone()));
    Ok(result)
}

// ---------------------------------------------------------------------------
// Link embed command
// ---------------------------------------------------------------------------

/// Session-scoped embed cache: url → (when, result). 5-minute TTL; relay
/// caches embeds for 1h so a client cache of 5m is conservative and correct.
static LINK_EMBED_CACHE: std::sync::OnceLock<
    std::sync::Mutex<
        std::collections::HashMap<String, (std::time::Instant, farder_protocol::messages::EmbedOutcome)>,
    >,
> = std::sync::OnceLock::new();

fn link_embed_cache(
) -> &'static std::sync::Mutex<
    std::collections::HashMap<String, (std::time::Instant, farder_protocol::messages::EmbedOutcome)>,
> {
    LINK_EMBED_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Resolve a rich embed for an external URL through the default relay. Throwaway
/// connection; never touches session connections. LAZY ONLY (PIN-lock rule):
/// needs no identity — embeds are anonymous.
#[tauri::command]
pub async fn get_link_embed(url: String) -> Result<farder_protocol::messages::EmbedOutcome, String> {
    use farder_protocol::messages::Message;

    // 5-minute client cache (embeds are stable; relay caches 1h).
    {
        let cache = link_embed_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, hit)) = cache.get(&url) {
            if at.elapsed() < std::time::Duration::from_secs(300) {
                return Ok(hit.clone());
            }
        }
    }

    let Some((relay_addr, relay_fp)) = crate::default_relay::default_relay() else {
        return Ok(farder_protocol::messages::EmbedOutcome::Unavailable);
    };
    let endpoint = crate::tls::make_pinned_relay_endpoint(relay_fp).map_err(|e| e.to_string())?;
    let conn = endpoint
        .connect(relay_addr, "farder-relay")
        .map_err(|e| e.to_string())?
        .await
        .map_err(|e| e.to_string())?;
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
    let msg = farder_protocol::codec::encode(&Message::ProxyLinkEmbed { url: url.clone() })
        .map_err(|e| e.to_string())?;
    crate::connection::write_frame(&mut send, &msg)
        .await
        .map_err(|e| e.to_string())?;
    let reply_bytes = crate::connection::read_frame(&mut recv)
        .await
        .map_err(|e| e.to_string())?;
    let reply: Message = farder_protocol::codec::decode(&reply_bytes).map_err(|e| e.to_string())?;
    conn.close(0u32.into(), b"embed done");

    let outcome = match reply {
        Message::ProxyLinkEmbedResult { outcome } => outcome,
        other => return Err(format!("unexpected relay reply: {:?}", other)),
    };
    {
        let mut cache = link_embed_cache().lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(url, (std::time::Instant::now(), outcome.clone()));
    }
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// Media proxy command
// ---------------------------------------------------------------------------

const MAX_PROXIED_MEDIA: usize = 25 * 1024 * 1024;

#[derive(serde::Serialize)]
pub struct ProxiedMedia {
    pub content_type: String,
    pub data_base64: String,
}

/// Pull a media asset (thumbnail or direct video) through the default relay and
/// return it base64-encoded for the webview to wrap in a Blob URL. The webview
/// never fetches the CDN directly (IP-leak protection).
#[tauri::command]
pub async fn get_proxied_media(url: String) -> Result<ProxiedMedia, String> {
    use farder_protocol::messages::Message;
    use base64::Engine;

    let Some((relay_addr, relay_fp)) = crate::default_relay::default_relay() else {
        return Err("no default relay".into());
    };
    let endpoint = crate::tls::make_pinned_relay_endpoint(relay_fp).map_err(|e| e.to_string())?;
    let conn = endpoint.connect(relay_addr, "farder-relay")
        .map_err(|e| e.to_string())?.await.map_err(|e| e.to_string())?;
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
    let msg = farder_protocol::codec::encode(&Message::ProxyMedia { url }).map_err(|e| e.to_string())?;
    crate::connection::write_frame(&mut send, &msg).await.map_err(|e| e.to_string())?;

    // First frame: header or unavailable.
    let hdr_bytes = crate::connection::read_frame(&mut recv).await.map_err(|e| e.to_string())?;
    let hdr: Message = farder_protocol::codec::decode(&hdr_bytes).map_err(|e| e.to_string())?;
    let (content_type, total_len) = match hdr {
        Message::ProxyMediaHeader { content_type, total_len } => (content_type, total_len),
        Message::ProxyMediaUnavailable => { conn.close(0u32.into(), b"done"); return Err("media unavailable".into()); }
        other => { conn.close(0u32.into(), b"done"); return Err(format!("unexpected: {:?}", other)); }
    };
    // Then the raw length-framed bytes (4-byte BE length + raw bytes).
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await.map_err(|e| e.to_string())?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_PROXIED_MEDIA {
        conn.close(0u32.into(), b"media too large");
        return Err("media too large".into());
    }
    if len as u64 != total_len { conn.close(0u32.into(), b"done"); return Err("length mismatch".into()); }
    let mut data = vec![0u8; len];
    recv.read_exact(&mut data).await.map_err(|e| e.to_string())?;
    conn.close(0u32.into(), b"media done");

    Ok(ProxiedMedia {
        content_type,
        data_base64: base64::engine::general_purpose::STANDARD.encode(&data),
    })
}

// ---------------------------------------------------------------------------
// Settings commands
// ---------------------------------------------------------------------------

fn read_settings() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_settings(map: serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let value = serde_json::Value::Object(map);
    let pretty = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    std::fs::write(settings_path(), pretty).map_err(|e| e.to_string())
}

pub(crate) fn settings_get(key: &str) -> Option<serde_json::Value> {
    read_settings().get(key).cloned()
}

pub(crate) fn settings_set(key: &str, value: serde_json::Value) -> Result<(), String> {
    let mut map = read_settings();
    map.insert(key.to_string(), value);
    write_settings(map)
}

#[tauri::command]
pub fn save_last_server(address: String) -> Result<(), String> {
    settings_set("address", serde_json::Value::String(address))
}

#[tauri::command]
pub fn get_last_server() -> Option<String> {
    settings_get("address").and_then(|v| v.as_str().map(|s| s.to_string()))
}

// ---------------------------------------------------------------------------
// Voice settings (mic mode, PTT key, per-peer volumes)
// ---------------------------------------------------------------------------

pub(crate) fn read_voice_mode() -> String {
    settings_get("voice_mode")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "OpenMic".to_string())
}

pub(crate) fn read_ptt_key() -> String {
    settings_get("ptt_key")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "Backquote".to_string())
}

pub(crate) fn read_peer_volumes() -> std::collections::HashMap<String, f32> {
    settings_get("peer_volumes")
        .and_then(|v| serde_json::from_value::<std::collections::HashMap<String, f32>>(v).ok())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Data-saver embed setting
// ---------------------------------------------------------------------------

pub(crate) fn read_data_saver_embeds() -> bool {
    settings_get("data_saver_embeds")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

#[tauri::command]
pub fn get_data_saver_embeds() -> bool {
    read_data_saver_embeds()
}

#[tauri::command]
pub fn set_data_saver_embeds(enabled: bool) -> Result<(), String> {
    settings_set("data_saver_embeds", serde_json::json!(enabled))
}

// ---------------------------------------------------------------------------
// Presence settings
// ---------------------------------------------------------------------------

pub(crate) fn read_presence_enabled() -> bool {
    settings_get("presence_enabled").and_then(|v| v.as_bool()).unwrap_or(false)
}

pub(crate) fn read_presence_music() -> bool {
    settings_get("presence_music").and_then(|v| v.as_bool()).unwrap_or(false)
}

#[tauri::command]
pub fn get_presence_enabled() -> bool {
    read_presence_enabled()
}

#[tauri::command]
pub fn set_presence_enabled(enabled: bool) -> Result<(), String> {
    settings_set("presence_enabled", serde_json::json!(enabled))
}

#[tauri::command]
pub fn get_presence_music() -> bool {
    read_presence_music()
}

#[tauri::command]
pub fn set_presence_music(enabled: bool) -> Result<(), String> {
    settings_set("presence_music", serde_json::json!(enabled))
}

#[tauri::command]
pub fn get_voice_mode() -> String {
    read_voice_mode()
}

#[tauri::command]
pub fn set_voice_mode(mode: String) -> Result<(), String> {
    // Accept only the two known values; default unknown to OpenMic.
    let normalized = if mode == "PushToTalk" {
        "PushToTalk"
    } else {
        "OpenMic"
    };
    settings_set("voice_mode", serde_json::Value::String(normalized.to_string()))
}

#[tauri::command]
pub fn get_ptt_key() -> String {
    read_ptt_key()
}

#[tauri::command]
pub fn set_ptt_key(key: String) -> Result<(), String> {
    settings_set("ptt_key", serde_json::Value::String(key))
}

#[tauri::command]
pub fn get_peer_volumes() -> std::collections::HashMap<String, f32> {
    read_peer_volumes()
}

/// Clamp + persist one peer's volume into the `peer_volumes` settings map.
pub(crate) fn persist_peer_volume(pubkey_hex: &str, volume: f32) -> Result<(), String> {
    let mut map = read_peer_volumes();
    map.insert(pubkey_hex.to_string(), volume.clamp(0.0, 2.0));
    let value = serde_json::to_value(map).map_err(|e| e.to_string())?;
    settings_set("peer_volumes", value)
}

/// Mic sensitivity (0-100, higher = more sensitive). Default 85.
pub(crate) fn read_voice_sensitivity() -> u32 {
    settings_get("voice_sensitivity")
        .and_then(|v| v.as_u64())
        .map(|n| n.min(100) as u32)
        .unwrap_or(85)
}

#[tauri::command]
pub fn get_voice_sensitivity() -> u32 {
    read_voice_sensitivity()
}

#[tauri::command]
pub async fn set_voice_sensitivity(
    voice: State<'_, Arc<crate::voice::VoiceController>>,
    value: u32,
) -> Result<(), String> {
    let clamped = value.min(100);
    settings_set("voice_sensitivity", serde_json::Value::from(clamped))?;
    // Applies live to the active call's send task.
    voice.set_speak_threshold(crate::voice::sensitivity_to_threshold(clamped));
    Ok(())
}

// ---------------------------------------------------------------------------
// Audio device settings
// ---------------------------------------------------------------------------

/// Read the saved input device name (None = system default).
pub(crate) fn read_input_device() -> Option<String> {
    settings_get("input_device")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
}

/// Read the saved output device name (None = system default).
pub(crate) fn read_output_device() -> Option<String> {
    settings_get("output_device")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
}

#[derive(serde::Serialize)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub is_default: bool,
}

/// Enumerate available audio input devices via cpal. Returns the system
/// default marked with `is_default: true`. Each `name` can be passed to
/// `set_input_device` to persist the selection.
#[tauri::command]
pub fn list_input_devices() -> Result<Vec<AudioDeviceInfo>, String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let default_name = host.default_input_device().and_then(|d| d.name().ok());
    let devices = host.input_devices().map_err(|e| format!("input_devices: {e}"))?;
    let mut out = Vec::new();
    for (i, dev) in devices.enumerate() {
        let name = dev.name().unwrap_or_else(|_| format!("device-{i}"));
        let is_default = default_name.as_deref() == Some(name.as_str());
        out.push(AudioDeviceInfo { name, is_default });
    }
    Ok(out)
}

/// Enumerate available audio output devices via cpal. Returns the system
/// default marked with `is_default: true`. Each `name` can be passed to
/// `set_output_device` to persist the selection.
#[tauri::command]
pub fn list_output_devices() -> Result<Vec<AudioDeviceInfo>, String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let default_name = host.default_output_device().and_then(|d| d.name().ok());
    let devices = host.output_devices().map_err(|e| format!("output_devices: {e}"))?;
    let mut out = Vec::new();
    for (i, dev) in devices.enumerate() {
        let name = dev.name().unwrap_or_else(|_| format!("device-{i}"));
        let is_default = default_name.as_deref() == Some(name.as_str());
        out.push(AudioDeviceInfo { name, is_default });
    }
    Ok(out)
}

#[tauri::command]
pub fn get_input_device() -> Option<String> {
    read_input_device()
}

#[tauri::command]
pub fn set_input_device(name: Option<String>) -> Result<(), String> {
    match name {
        Some(n) => settings_set("input_device", serde_json::Value::String(n)),
        None => {
            let mut map = read_settings();
            map.remove("input_device");
            write_settings(map)
        }
    }
}

#[tauri::command]
pub fn get_output_device() -> Option<String> {
    read_output_device()
}

#[tauri::command]
pub fn set_output_device(name: Option<String>) -> Result<(), String> {
    match name {
        Some(n) => settings_set("output_device", serde_json::Value::String(n)),
        None => {
            let mut map = read_settings();
            map.remove("output_device");
            write_settings(map)
        }
    }
}

// ---------------------------------------------------------------------------
// Saved servers list
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct LocalServerConfig {
    pub data_dir: String,
    pub template: String,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct ServerEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub local: Option<LocalServerConfig>,
}

fn servers_list_path() -> std::path::PathBuf {
    farder_data_dir().join("servers.json")
}

fn load_server_entries() -> Vec<ServerEntry> {
    std::fs::read_to_string(servers_list_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_server_entry(address: &str, name: &str) {
    save_server_entry_with_config(address, name, None);
}

fn save_server_entry_with_config(address: &str, name: &str, local: Option<LocalServerConfig>) {
    let mut entries = load_server_entries();
    if !entries.iter().any(|e| e.id == address) {
        entries.push(ServerEntry { id: address.to_string(), name: name.to_string(), local });
        let _ = std::fs::write(servers_list_path(), serde_json::to_string(&entries).unwrap());
    }
}

fn remove_server_entry(address: &str) {
    let mut entries = load_server_entries();
    entries.retain(|e| e.id != address);
    let _ = std::fs::write(servers_list_path(), serde_json::to_string(&entries).unwrap());
}

#[tauri::command]
pub fn get_saved_servers() -> Vec<ServerEntry> {
    load_server_entries()
}

// ---------------------------------------------------------------------------
// Server commands
// ---------------------------------------------------------------------------

/// Negotiate the protocol version on a freshly-connected server (D3).
///
/// This is PER-CONNECTION state the server clears on disconnect, so it must run
/// on every connect and reconnect — never just the first. An older sidecar
/// cannot decode `NegotiateProtocol` (unknown rmp_serde variant name breaks the
/// whole frame) and will either answer `Error` or drop the connection; either
/// way we log a `[protocol]` line and keep going rather than hard-fail. The v1
/// surfaces still work, and any v2 command will then surface the server's own
/// `UPGRADE_REQUIRED` refusal.
async fn negotiate_protocol(state: &AppState, server_id: &str) {
    let negotiated = bridge::send_request(
        state,
        server_id,
        ServerRequest::NegotiateProtocol { client_version: SERVER_PROTOCOL_VERSION },
    )
    .await;
    match negotiated {
        Ok(ServerResponse::ProtocolVersion { server_version, min_client_version_for_e2ee }) => {
            eprintln!(
                "[protocol] {}: negotiated client={} server={} min_client_for_e2ee={}",
                server_id, SERVER_PROTOCOL_VERSION, server_version, min_client_version_for_e2ee
            );
        }
        Ok(other) => eprintln!(
            "[protocol] {}: NegotiateProtocol returned {:?}; treating as v1",
            server_id, other
        ),
        Err(e) => eprintln!(
            "[protocol] {}: negotiate failed: {}; treating as v1",
            server_id, e
        ),
    }
}

/// Connect to a Farder server, authenticate, and return initial server info.
#[tauri::command]
pub async fn connect_server(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    address: String,
    invite_code: Option<String>,
    setup_token: Option<String>,
) -> Result<ConnectResult, String> {
    // Reconstruct keypair from stored bytes.
    let keypair = {
        let lock = state
            .signing_key_bytes
            .lock()
            .map_err(|e| e.to_string())?;
        match lock.as_ref() {
            Some(bytes) => Keypair::from_signing_key_bytes(bytes),
            None => return Err("no identity keypair set — unlock your identity first".to_string()),
        }
    };

    let (endpoint, conn, send, recv, session_token, relayed) =
        if let Some(target) = crate::connection::parse_relay_target(&address) {
            let endpoint = crate::tls::make_pinned_relay_endpoint(target.cert_fp.clone())
                .map_err(|e| e.to_string())?;
            let (conn, send, recv, token) =
                connect_via_relay(endpoint.clone(), &target, &keypair, setup_token)
                    .await
                    .map_err(|e| e.to_string())?;
            (endpoint, conn, send, recv, token, true)
        } else {
            let addr: std::net::SocketAddr = address
                .parse()
                .map_err(|e: std::net::AddrParseError| e.to_string())?;
            let endpoint = make_client_endpoint().map_err(|e| e.to_string())?;
            let (conn, send, recv, token) =
                connect_and_authenticate(endpoint.clone(), addr, &keypair, invite_code, setup_token)
                    .await
                    .map_err(|e| e.to_string())?;
            (endpoint, conn, send, recv, token, false)
        };

    let media_dispatcher = std::sync::Arc::new(crate::voice::MediaInboundDispatcher::default());
    // Voice datagrams flow over BOTH direct and relayed connections: the relay
    // strips the routing handle before delivering, so the client sees raw frames
    // identical to direct mode (Phase 5b-client).
    {
        let dispatcher_for_loop = media_dispatcher.clone();
        let conn_for_loop = conn.clone();
        tokio::spawn(async move {
            loop {
                match conn_for_loop.read_datagram().await {
                    Ok(bytes) => dispatcher_for_loop.dispatch(bytes).await,
                    Err(quinn::ConnectionError::ApplicationClosed { .. })
                    | Err(quinn::ConnectionError::ConnectionClosed { .. })
                    | Err(quinn::ConnectionError::LocallyClosed)
                    | Err(quinn::ConnectionError::TimedOut) => break,
                    Err(_) => break,
                }
            }
        });
    }

    let server_conn = Arc::new(ServerConnection {
        endpoint,
        connection: conn,
        send_stream: tokio::sync::Mutex::new(send),
        next_request_id: AtomicU32::new(1),
        pending_requests: Mutex::new(HashMap::new()),
        event_reader_handle: Mutex::new(None),
        server_name: Mutex::new(String::new()),
        media_dispatcher,
        session_token,
        relayed,
    });

    let handle = bridge::spawn_event_reader(app, address.clone(), Arc::clone(&server_conn), recv);
    *server_conn.event_reader_handle.lock().unwrap() = Some(handle);

    {
        let mut servers = state.servers.lock().unwrap();
        servers.insert(address.clone(), Arc::clone(&server_conn));
    }

    // Fetch initial server info.
    let response = bridge::send_request(&state, &address, ServerRequest::GetServerInfo)
        .await
        .map_err(|e| e.to_string())?;

    match response {
        ServerResponse::ServerInfo { name, member_count, channels, categories, roles, owner_public_key, server_id } => {
            *server_conn.server_name.lock().unwrap() = name.clone();
            // Save to persistent server list
            save_server_entry(&address, &name);
            // Sync our signed profile to this server in the background.
            {
                let state_arc: Arc<AppState> = Arc::clone(state.inner());
                let sid = address.clone();
                tokio::spawn(async move {
                    if let Err(e) = crate::profile_sync::push_profile_on_connect(&state_arc, &sid).await {
                        eprintln!("[profile-sync] push after connect to {} failed: {}", sid, e);
                    }
                });
            }
            // Negotiate v2 BEFORE returning: the frontend's next call (T3) is
            // `get_server_info_v2`, which the server refuses with
            // `UPGRADE_REQUIRED` unless this connection has already negotiated.
            negotiate_protocol(&state, &address).await;

            Ok(ConnectResult { server_name: name, member_count, channels, categories, roles, owner_public_key, relayed, server_id })
        }
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Disconnect from a specific server and remove it from the map.
#[tauri::command]
pub async fn disconnect_server(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<(), String> {
    let conn = {
        let mut servers = state.servers.lock().unwrap();
        servers.remove(&server_id)
    };
    if let Some(c) = conn {
        if let Some(handle) = c.event_reader_handle.lock().unwrap().take() {
            handle.abort();
        }
    }
    remove_server_entry(&server_id);
    Ok(())
}

/// List all currently connected servers.
#[tauri::command]
pub fn list_servers(state: State<'_, Arc<AppState>>) -> Vec<ServerEntry> {
    let servers = state.servers.lock().unwrap();
    servers.iter().map(|(addr, conn)| ServerEntry {
        id: addr.clone(),
        name: conn.server_name.lock().unwrap().clone(),
        local: None,
    }).collect()
}

/// Re-fetch server info for a connected server.
#[tauri::command]
pub async fn get_server_info(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<ConnectResult, String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::GetServerInfo)
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::ServerInfo { name, member_count, channels, categories, roles, owner_public_key, server_id } => {
            Ok(ConnectResult { server_name: name, member_count, channels, categories, roles, owner_public_key, relayed: false, server_id })
        }
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Re-fetch server info with per-channel classes (the v2 surface).
///
/// Mirrors [`get_server_info`] but sends `GetServerInfoV2`, so the returned
/// channel list carries each channel's [`ChannelInfoV2::class`]. Requires the
/// connection to have negotiated v2 (done automatically in `connect_server`
/// and `create_local_server`); before negotiation the server refuses with
/// `UPGRADE_REQUIRED`.
#[tauri::command]
pub async fn get_server_info_v2(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<ServerInfoV2Result, String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::GetServerInfoV2)
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::ServerInfoV2 { name, member_count, channels, categories, roles, owner_public_key, server_id } => {
            Ok(ServerInfoV2Result { server_name: name, member_count, channels, categories, roles, owner_public_key, server_id })
        }
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Send a chat message to a channel.
#[tauri::command]
pub async fn send_message(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
    content: String,
    reply_to: Option<u64>,
    attachment_ids: Vec<u64>,
) -> Result<SendMessageResult, String> {
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::SendMessage {
            channel_id,
            content,
            reply_to,
            attachment_ids,
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    match response {
        ServerResponse::MessageSent { id, timestamp } => Ok(SendMessageResult { id, timestamp }),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Fetch message history for a channel.
#[tauri::command]
pub async fn fetch_history(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
    before_id: Option<u64>,
    limit: Option<u32>,
) -> Result<Vec<MessageInfo>, String> {
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::FetchHistory {
            channel_id,
            before_id,
            limit: limit.unwrap_or(50),
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    match response {
        ServerResponse::History { messages } => Ok(messages),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Fetch message history for a channel, carrying sealed rows (the v2 surface).
///
/// Mirrors [`fetch_history`] but sends `FetchHistoryV2` and returns
/// [`MessageInfoV2`] rows (base message + `is_e2ee`/`sealed`/`event_hash`).
/// Requires the connection to have negotiated v2 (done automatically in
/// `connect_server` and `create_local_server`).
#[tauri::command]
pub async fn fetch_history_v2(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
    before_id: Option<u64>,
    limit: Option<u32>,
) -> Result<Vec<MessageInfoV2>, String> {
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::FetchHistoryV2 {
            channel_id,
            before_id,
            limit: limit.unwrap_or(50),
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    match response {
        ServerResponse::HistoryV2 { messages } => Ok(messages),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Subscribe to events for the given channel IDs.
#[tauri::command]
pub async fn subscribe_channels(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_ids: Vec<u64>,
) -> Result<(), String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::Subscribe { channel_ids })
        .await
        .map_err(|e| e.to_string())?;

    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Get all server members.
#[tauri::command]
pub async fn get_members(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<Vec<MemberInfo>, String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::GetMembers)
        .await
        .map_err(|e| e.to_string())?;

    match response {
        ServerResponse::Members { members } => Ok(members),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// Voice-channel presence (server roster). Distinct from the audio pipeline
// (`voice_join` etc.): these update who the server lists as present in a voice
// channel and broadcast MediaJoined/MediaLeft to all members. The frontend
// calls join_voice + get_voice_state alongside the audio engine on join.
// ---------------------------------------------------------------------------

/// Register presence in a voice channel's server-side roster.
#[tauri::command]
pub async fn join_voice(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
) -> Result<(), String> {
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::JoinChannelMedia { channel_id },
    )
    .await
    .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Remove presence from a voice channel's server-side roster.
#[tauri::command]
pub async fn leave_voice(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
) -> Result<(), String> {
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::LeaveChannelMedia { channel_id },
    )
    .await
    .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Fetch the current voice-channel roster (who is present).
#[tauri::command]
pub async fn get_voice_state(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
) -> Result<Vec<farder_protocol::server::VoiceMember>, String> {
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::GetMediaState { channel_id },
    )
    .await
    .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::MediaStateResp { participants } => Ok(participants),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Add a reaction to a message.
#[tauri::command]
pub async fn add_reaction(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    message_id: u64,
    emoji: String,
    file_id: Option<u64>,
) -> Result<(), String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::AddReaction { message_id, emoji, file_id })
        .await
        .map_err(|e| e.to_string())?;

    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Remove a reaction from a message.
#[tauri::command]
pub async fn remove_reaction(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    message_id: u64,
    emoji: String,
    file_id: Option<u64>,
) -> Result<(), String> {
    let response =
        bridge::send_request(&state, &server_id, ServerRequest::RemoveReaction { message_id, emoji, file_id })
            .await
            .map_err(|e| e.to_string())?;

    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Create a thread from a message.
#[tauri::command]
pub async fn create_thread(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    message_id: u64,
    name: Option<String>,
) -> Result<(), String> {
    let response =
        bridge::send_request(&state, &server_id, ServerRequest::CreateThread { message_id, name })
            .await
            .map_err(|e| e.to_string())?;

    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// File upload commands
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadOutcome {
    pub file_id: u64,
    pub content_hash: String,
    pub declared_type: String,
    pub size: u64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentCapInput {
    pub content_hash: String,
    pub declared_type: String,
    pub size: u64,
}

/// Open a native file picker dialog and return the selected file path.
#[tauri::command]
pub async fn pick_file() -> Result<Option<String>, String> {
    let path = tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new().pick_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(path.map(|p| p.to_string_lossy().to_string()))
}

/// Upload a file via a new QUIC bi-stream and return the upload outcome (file_id + cap fields).
#[tauri::command]
pub async fn upload_file(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
    file_path: String,
) -> Result<UploadOutcome, String> {
    upload_file_internal_with_channel(&state, &server_id, channel_id, &file_path).await
}

/// Internal helper: upload a file to a server without a specific channel (channel_id = 0).
/// Used by book.rs to cache uploaded images per-server without going through the Tauri command layer.
pub(crate) async fn upload_file_internal(
    state: &AppState,
    server_id: &str,
    file_path: &str,
) -> Result<u64, String> {
    upload_file_internal_with_channel(state, server_id, 0, file_path)
        .await
        .map(|o| o.file_id)
}

/// Write the RelayStreamRole::Session marker on a file-transfer stream when the
/// connection is relayed (the server demuxes relay file streams by this token).
/// A no-op for direct connections.
async fn write_relay_session_marker(
    send: &mut quinn::SendStream,
    conn: &crate::state::ServerConnection,
) -> Result<(), String> {
    if conn.relayed {
        let role = farder_protocol::server::RelayStreamRole::Session {
            token: conn.session_token.clone(),
        };
        crate::connection::write_frame(
            send,
            &farder_protocol::codec::encode(&role).map_err(|e| e.to_string())?,
        )
        .await
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Core upload logic shared by the Tauri command and the internal helper.
async fn upload_file_internal_with_channel(
    state: &AppState,
    server_id: &str,
    channel_id: u64,
    file_path: &str,
) -> Result<UploadOutcome, String> {
    use sha2::{Digest, Sha256};

    // Read file from disk
    let data = std::fs::read(file_path).map_err(|e| e.to_string())?;
    let file_name = std::path::Path::new(file_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());

    // Compute SHA-256
    let hash = format!("{:x}", Sha256::digest(&data));

    let mime_type = match file_name.rsplit('.').next() {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("wav") => "audio/wav",
        Some("mp3") => "audio/mpeg",
        Some("ogg") => "audio/ogg",
        Some("m4a") => "audio/mp4",
        Some("flac") => "audio/flac",
        Some("webm") => "audio/webm",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        _ => "application/octet-stream",
    }
    .to_string();

    // Open a new bi-stream on the existing connection
    let conn = state.get_server(server_id).map_err(|e| e.to_string())?;
    let quic_conn = conn.connection.clone();
    let (mut send, mut recv) = quic_conn.open_bi().await.map_err(|e| e.to_string())?;

    // Relayed connections demux file streams by a Session role marker.
    write_relay_session_marker(&mut send, &conn).await?;

    // Capture size before moving data into the request.
    let size = data.len() as u64;

    // Send UploadRequest
    let req = farder_protocol::server::UploadRequest {
        channel_id,
        file_name,
        file_size: size,
        hash: hash.clone(),
        mime_type: mime_type.clone(),
        width: None,
        height: None,
        duration_secs: None,
    };
    let req_bytes = farder_protocol::codec::encode(&req).map_err(|e| e.to_string())?;
    crate::connection::write_frame(&mut send, &req_bytes)
        .await
        .map_err(|e| e.to_string())?;

    // Read response
    let resp_bytes = crate::connection::read_frame(&mut recv)
        .await
        .map_err(|e| e.to_string())?;
    let resp: farder_protocol::server::UploadResponse =
        farder_protocol::codec::decode(&resp_bytes).map_err(|e| e.to_string())?;

    match resp {
        farder_protocol::server::UploadResponse::Ready => {
            // Send file bytes
            send.write_all(&data).await.map_err(|e| e.to_string())?;
            send.finish().map_err(|e| e.to_string())?;

            // Read Complete response
            let resp2_bytes = crate::connection::read_frame(&mut recv)
                .await
                .map_err(|e| e.to_string())?;
            let resp2: farder_protocol::server::UploadResponse =
                farder_protocol::codec::decode(&resp2_bytes).map_err(|e| e.to_string())?;
            match resp2 {
                farder_protocol::server::UploadResponse::Complete { file_id } => Ok(UploadOutcome {
                    file_id,
                    content_hash: hash.clone(),
                    declared_type: mime_type.clone(),
                    size,
                }),
                farder_protocol::server::UploadResponse::Error { reason } => Err(reason),
                _ => Err("unexpected upload response".to_string()),
            }
        }
        farder_protocol::server::UploadResponse::Complete { file_id } => Ok(UploadOutcome {
            file_id,
            content_hash: hash.clone(),
            declared_type: mime_type.clone(),
            size,
        }), // dedup
        farder_protocol::server::UploadResponse::Error { reason } => Err(reason),
    }
}

// ---------------------------------------------------------------------------
// File download commands
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct DownloadResult {
    pub data_url: Option<String>,
    pub file_name: String,
    pub mime_type: String,
    pub saved_path: Option<String>,
}

/// Download a file by file_id. Returns a base64 data URL for images, or saves to disk for other types.
#[tauri::command]
pub async fn download_file(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    file_id: u64,
) -> Result<DownloadResult, String> {
    download_file_internal(&state, &server_id, file_id).await
}

pub(crate) async fn download_file_internal(
    state: &AppState,
    server_id: &str,
    file_id: u64,
) -> Result<DownloadResult, String> {
    let conn = state.get_server(server_id).map_err(|e| e.to_string())?;
    let quic_conn = conn.connection.clone();
    let (mut send, mut recv) = quic_conn.open_bi().await.map_err(|e| e.to_string())?;

    // Relayed connections demux file streams by a Session role marker.
    write_relay_session_marker(&mut send, &conn).await?;

    // Send DownloadRequest
    let req = farder_protocol::server::DownloadRequest { file_id };
    let req_bytes = farder_protocol::codec::encode(&req).map_err(|e| e.to_string())?;
    crate::connection::write_frame(&mut send, &req_bytes).await.map_err(|e| e.to_string())?;

    // Read response
    let resp_bytes = crate::connection::read_frame(&mut recv).await.map_err(|e| e.to_string())?;
    let resp: farder_protocol::server::DownloadResponse =
        farder_protocol::codec::decode(&resp_bytes).map_err(|e| e.to_string())?;

    match resp {
        farder_protocol::server::DownloadResponse::Start { file_name, file_size, hash: _, mime_type } => {
            // Read all bytes
            let mut data = Vec::with_capacity(file_size as usize);
            let mut remaining = file_size;
            while remaining > 0 {
                let mut buf = vec![0u8; std::cmp::min(remaining as usize, 65536)];
                match recv.read(&mut buf).await {
                    Ok(Some(n)) if n > 0 => {
                        data.extend_from_slice(&buf[..n]);
                        remaining -= n as u64;
                    }
                    _ => break,
                }
            }

            // Images and audio render inline in the chat, so return their
            // bytes as a base64 data URL; everything else saves to disk.
            let inline = mime_type.starts_with("image/") || mime_type.starts_with("audio/");
            if inline {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
                let data_url = format!("data:{};base64,{}", mime_type, b64);
                Ok(DownloadResult { data_url: Some(data_url), file_name, mime_type, saved_path: None })
            } else {
                // Save to downloads directory
                let downloads = dirs::download_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default());
                let save_path = downloads.join(&file_name);
                std::fs::write(&save_path, &data).map_err(|e| e.to_string())?;
                Ok(DownloadResult { data_url: None, file_name, mime_type, saved_path: Some(save_path.to_string_lossy().to_string()) })
            }
        }
        farder_protocol::server::DownloadResponse::Error { reason } => Err(reason),
    }
}

// ---------------------------------------------------------------------------
// URL fetch proxy command
// ---------------------------------------------------------------------------

/// Ask the server to fetch a URL and store it as an attachment, returning the file_id.
#[tauri::command]
pub async fn fetch_url(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    url: String,
    channel_id: u64,
) -> Result<u64, String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::FetchUrl { url, channel_id })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::UrlFetched { file_id } => Ok(file_id),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// Admin commands
// ---------------------------------------------------------------------------

/// Search messages by full-text query.
#[tauri::command]
pub async fn search_messages(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    query: String,
    channel_id: Option<u64>,
    limit: Option<u32>,
) -> Result<Vec<MessageInfo>, String> {
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::Search { query, channel_id, limit: limit.unwrap_or(20) },
    )
    .await
    .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::SearchResults { messages } => Ok(messages),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

/// Create a new channel on the server.
#[tauri::command]
pub async fn create_channel(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    name: String,
    channel_type: String,
    category_id: Option<u64>,
) -> Result<(), String> {
    use farder_protocol::server::ChannelType;
    let ch_type = match channel_type.as_str() {
        "Announcement" => ChannelType::Announcement,
        "Voice" => ChannelType::Voice,
        _ => ChannelType::Text,
    };
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::CreateChannel { name, channel_type: ch_type, category_id, position: None },
    )
    .await
    .map_err(|e| e.to_string())?;

    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Create a new category on the server.
#[tauri::command]
pub async fn create_category(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    name: String,
) -> Result<(), String> {
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::CreateCategory { name, position: None },
    )
    .await
    .map_err(|e| e.to_string())?;

    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Delete a channel on the server.
#[tauri::command]
pub async fn delete_channel(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
) -> Result<(), String> {
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::DeleteChannel { channel_id },
    ).await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Delete a category on the server.
#[tauri::command]
pub async fn delete_category(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    category_id: u64,
) -> Result<(), String> {
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::DeleteCategory { category_id },
    ).await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Update channel settings.
#[tauri::command]
pub async fn update_channel(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
    name: Option<String>,
    topic: Option<String>,
    nsfw: Option<bool>,
    slow_mode_secs: Option<u32>,
    category_id: Option<u64>,
    set_category: Option<bool>,
    position: Option<u32>,
) -> Result<(), String> {
    // Convert flat params to Option<Option<u64>>:
    // set_category=true + category_id=Some(x) → Some(Some(x)) (move to category)
    // set_category=true + category_id=None → Some(None) (uncategorize)
    // set_category=None/false → None (don't change)
    let cat = if set_category.unwrap_or(false) {
        Some(category_id)
    } else {
        None
    };
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::UpdateChannel {
            channel_id,
            name,
            topic,
            nsfw,
            slow_mode_secs,
            retention_secs: None,
            category_id: cat,
            position,
        },
    ).await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

/// Update category settings.
#[tauri::command]
pub async fn update_category(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    category_id: u64,
    name: Option<String>,
    position: Option<u32>,
) -> Result<(), String> {
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::UpdateCategory {
            category_id,
            name,
            position,
        },
    ).await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

/// Set per-role permission override for a channel.
#[tauri::command]
pub async fn set_channel_override(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
    role_id: u64,
    allow: u64,
    deny: u64,
) -> Result<(), String> {
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::SetChannelOverride { channel_id, role_id, allow, deny },
    ).await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// Account deletion commands
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct DeletionStatusResult {
    pub pending: bool,
    pub requested_at: Option<u64>,
    pub expires_at: Option<u64>,
}

#[tauri::command]
pub async fn request_deletion(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<(), String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::RequestDeletion)
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn cancel_deletion(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<(), String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::CancelDeletion)
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn get_deletion_status(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<DeletionStatusResult, String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::GetDeletionStatus)
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::DeletionStatusResp { status } => Ok(DeletionStatusResult {
            pending: status.pending,
            requested_at: status.requested_at,
            expires_at: status.expires_at,
        }),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// Favorites commands
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct FavoriteEntry {
    pub id: String,
    pub file_name: String,
    pub mime_type: String,
    pub data_url: String,
    pub source_server: String,
    pub original_url: Option<String>,
    pub favorited_at: u64,
}

fn favorites_dir() -> std::path::PathBuf {
    let dir = farder_data_dir().join("favorites");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn favorites_index_path() -> std::path::PathBuf {
    farder_data_dir().join("favorites.json")
}

fn load_favorites_index() -> Vec<FavoriteEntry> {
    let path = favorites_index_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_favorites_index(entries: &[FavoriteEntry]) -> Result<(), String> {
    let path = favorites_index_path();
    let json = serde_json::to_string_pretty(entries).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn add_favorite(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    file_id: u64,
    original_url: Option<String>,
) -> Result<FavoriteEntry, String> {
    use sha2::Digest;

    let conn = state.get_server(&server_id).map_err(|e| e.to_string())?;
    let quic_conn = conn.connection.clone();
    let (mut send, mut recv) = quic_conn.open_bi().await.map_err(|e| e.to_string())?;

    // Relayed connections demux file streams by a Session role marker.
    write_relay_session_marker(&mut send, &conn).await?;

    let req = farder_protocol::server::DownloadRequest { file_id };
    let req_bytes = farder_protocol::codec::encode(&req).map_err(|e| e.to_string())?;
    crate::connection::write_frame(&mut send, &req_bytes).await.map_err(|e| e.to_string())?;

    let resp_bytes = crate::connection::read_frame(&mut recv).await.map_err(|e| e.to_string())?;
    let resp: farder_protocol::server::DownloadResponse =
        farder_protocol::codec::decode(&resp_bytes).map_err(|e| e.to_string())?;

    match resp {
        farder_protocol::server::DownloadResponse::Start { file_name, file_size, mime_type, .. } => {
            let mut data = Vec::with_capacity(file_size as usize);
            let mut remaining = file_size;
            while remaining > 0 {
                let mut buf = vec![0u8; std::cmp::min(remaining as usize, 65536)];
                match recv.read(&mut buf).await {
                    Ok(Some(n)) if n > 0 => {
                        data.extend_from_slice(&buf[..n]);
                        remaining -= n as u64;
                    }
                    _ => break,
                }
            }

            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
            let data_url = format!("data:{};base64,{}", mime_type, b64);

            let id = format!("{:x}", sha2::Sha256::digest(&data));
            let img_path = favorites_dir().join(&id);
            std::fs::write(&img_path, &data).map_err(|e| e.to_string())?;

            let server_name = conn.server_name.lock().unwrap().clone();
            let source_server = if server_name.is_empty() { server_id } else { server_name };

            let entry = FavoriteEntry {
                id: id.clone(),
                file_name,
                mime_type,
                data_url,
                source_server,
                original_url,
                favorited_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            };

            let mut entries = load_favorites_index();
            if !entries.iter().any(|e| e.id == id) {
                entries.push(entry.clone());
                save_favorites_index(&entries)?;
            }

            Ok(entry)
        }
        farder_protocol::server::DownloadResponse::Error { reason } => Err(reason),
    }
}

#[tauri::command]
pub fn list_favorites() -> Result<Vec<FavoriteEntry>, String> {
    Ok(load_favorites_index())
}

#[tauri::command]
pub fn remove_favorite(id: String) -> Result<(), String> {
    let mut entries = load_favorites_index();
    entries.retain(|e| e.id != id);
    save_favorites_index(&entries)?;
    let img_path = favorites_dir().join(&id);
    let _ = std::fs::remove_file(img_path);
    Ok(())
}

// ---------------------------------------------------------------------------
// Typing indicator command
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn send_typing(state: State<'_, Arc<AppState>>, server_id: String, channel_id: u64) -> Result<(), String> {
    let _ = bridge::send_request(&state, &server_id, ServerRequest::Typing { channel_id })
        .await;
    Ok(()) // Fire and forget — don't care about errors
}

// ---------------------------------------------------------------------------
// DM commands
// ---------------------------------------------------------------------------

fn parse_public_key(key_str: &str) -> Result<farder_crypto::identity::PublicKey, String> {
    let hex_str = key_str.strip_prefix("vk_").unwrap_or(key_str);
    let bytes = hex::decode(hex_str).map_err(|e| e.to_string())?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| "invalid key length".to_string())?;
    Ok(farder_crypto::identity::PublicKey::from_bytes(arr))
}

#[tauri::command]
pub async fn open_dm(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    target_key: String,
) -> Result<serde_json::Value, String> {
    let pk = parse_public_key(&target_key)?;
    let response = bridge::send_request(&state, &server_id, ServerRequest::OpenDm { target_key: pk })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::DmOpened { channel, participant } => {
            Ok(serde_json::json!({ "channel": channel, "participant": participant }))
        }
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn list_dms(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::ListDms)
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::DmList { dms } => {
            Ok(dms.into_iter().map(|d| serde_json::to_value(d).unwrap()).collect())
        }
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn block_user(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    target_key: String,
) -> Result<(), String> {
    let pk = parse_public_key(&target_key)?;
    let response = bridge::send_request(&state, &server_id, ServerRequest::BlockUser { target_key: pk })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn unblock_user(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    target_key: String,
) -> Result<(), String> {
    let pk = parse_public_key(&target_key)?;
    let response = bridge::send_request(&state, &server_id, ServerRequest::UnblockUser { target_key: pk })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// Invite commands
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct InviteResult {
    pub code: String,
    pub link: String,
    pub deep_link: String,
}

#[tauri::command]
pub async fn create_invite(
    state: State<'_, Arc<AppState>>,
    server_id: String,             // connection key (address) — routes the request
    log_server_id: Option<String>, // genesis hash when log-mode; None for legacy
    max_uses: Option<u32>,
    requires_approval: Option<bool>,
) -> Result<InviteResult, String> {
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::CreateInvite { max_uses, expires_in_secs: None, target_channel: None },
    )
    .await
    .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::InviteCreated { code } => {
            use base64::Engine;
            let (encoded, deep_link) =
                if let Some(target) = crate::connection::parse_relay_target(&server_id) {
                    // Servers on the compiled-in default relay get the compact
                    // form (drops the embedded addr + 64-char fingerprint).
                    let on_default = crate::default_relay::default_relay()
                        .map(|(addr, fp)| addr == target.relay_addr && fp == target.cert_fp)
                        .unwrap_or(false);
                    let deep_link = if on_default {
                        crate::connection::build_compact_relay_link(&target.server_id, &code)
                    } else {
                        crate::connection::build_relay_link(&target, &code)
                    };
                    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(deep_link.as_bytes());
                    (encoded, deep_link)
                } else {
                    let plain = format!("{}/{}", server_id, code);
                    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(plain.as_bytes());
                    let deep_link = format!("farder://{}/{}", server_id, code);
                    (encoded, deep_link)
                };
            let link = format!("https://farder.gg/join/{}", encoded);

            // Mesh server: also record the invite as a signed InviteCreated event
            // in the log, so a joiner can cite it in their MemberJoined.
            // Instant invite for now (requires_approval = false; approval is sub-project 3).
            if let Some(log_sid) = log_server_id {
                // Serialize chain writes against submit_event and join_log_server.
                let _chain_guard = state.device_chain_lock.lock().await;

                let identity = {
                    let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
                    let bytes = lock.ok_or_else(|| "identity is locked".to_string())?;
                    Keypair::from_signing_key_bytes(&bytes)
                };
                let device = crate::device::load_or_create_device_keypair(identity.signing_key_bytes())?;
                let mut ds = crate::device::DeviceState::load(&log_sid)?
                    .unwrap_or_else(|| crate::device::DeviceState::fresh(&device));

                // First action on this server authorizes the device (mirrors submit_event).
                if !ds.authorized {
                    let cert = crate::device::device_cert(&identity, &device);
                    let da = event_build_next(
                        &device,
                        &identity,
                        &log_sid,
                        ds.last_event_hash.clone(),
                        ds.next_seq,
                        ds.lamport,
                        farder_crypto::event_log::EventPayload::DeviceAuthorized { cert },
                    );
                    event_send_submit(&state, &server_id, &da).await?;
                    ds.next_seq = da.core.seq + 1;
                    ds.last_event_hash = Some(da.hash());
                    ds.lamport = da.core.lamport;
                    ds.authorized = true;
                    ds.save(&log_sid)?;
                }

                let expires_at = event_now_secs() + 30 * 24 * 60 * 60;
                let inv = event_build_next(
                    &device,
                    &identity,
                    &log_sid,
                    ds.last_event_hash.clone(),
                    ds.next_seq,
                    ds.lamport,
                    farder_crypto::event_log::EventPayload::InviteCreated {
                        code_hash: farder_crypto::event_log::invite_code_hash(&code),
                        max_uses: max_uses.filter(|n| *n > 0).unwrap_or(u32::MAX),
                        expires_at,
                        requires_approval: requires_approval.unwrap_or(false),
                    },
                );
                event_send_submit(&state, &server_id, &inv).await?;
                ds.next_seq = inv.core.seq + 1;
                ds.last_event_hash = Some(inv.hash());
                ds.lamport = inv.core.lamport;
                ds.save(&log_sid)?;
            }

            Ok(InviteResult { code, link, deep_link })
        }
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// Message edit / delete commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn edit_message(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    message_id: u64,
    new_content: String,
) -> Result<(), String> {
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::EditMessage { message_id, new_content },
    )
    .await
    .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn delete_message(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    message_id: u64,
) -> Result<(), String> {
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::DeleteMessage { message_id },
    )
    .await
    .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// Role management commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn create_role(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    name: String,
    permissions: u64,
    color: Option<String>,
) -> Result<(), String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::CreateRole {
        name,
        permissions,
        color,
        position: None,
        hoist: None,
    }).await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn delete_role(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    role_id: u64,
) -> Result<(), String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::DeleteRole { role_id })
        .await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn add_bot(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    coin_id: String,
    label: String,
) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::AddBot { coin_id, label })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub async fn add_custom_bot(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    name: String,
    source_url: String,
    value_path: String,
    unit: Option<String>,
) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::AddCustomBot { name, source_url, value_path, unit })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub async fn remove_bot(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    bot_public_key: String,
) -> Result<(), String> {
    let pk = parse_public_key(&bot_public_key)?;
    match bridge::send_request(&state, &server_id, ServerRequest::RemoveBot { bot_public_key: pk })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub async fn get_bot_poll_interval(state: State<'_, Arc<AppState>>, server_id: String) -> Result<u64, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::GetBotPollInterval).await.map_err(|e| e.to_string())? {
        ServerResponse::BotPollInterval { secs } => Ok(secs),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub async fn set_bot_poll_interval(state: State<'_, Arc<AppState>>, server_id: String, secs: u64) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::SetBotPollInterval { secs }).await.map_err(|e| e.to_string())? {
        ServerResponse::Ok => Ok(()),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// Bot alert commands (owner-gated on server; subscribe/list are any-member)
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn add_bot_alert(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    bot_public_key: String,
    metric: String,
    comparator: String,
    threshold: f64,
) -> Result<(), String> {
    let pk = parse_public_key(&bot_public_key)?;
    match bridge::send_request(
        &state,
        &server_id,
        ServerRequest::AddBotAlert { bot_public_key: pk, metric, comparator, threshold },
    )
    .await
    .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub async fn remove_bot_alert(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    alert_id: i64,
) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::RemoveBotAlert { alert_id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub async fn list_bot_alerts(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    bot_public_key: String,
) -> Result<Vec<BotAlertInfo>, String> {
    let pk = parse_public_key(&bot_public_key)?;
    match bridge::send_request(
        &state,
        &server_id,
        ServerRequest::ListBotAlerts { bot_public_key: pk },
    )
    .await
    .map_err(|e| e.to_string())?
    {
        ServerResponse::BotAlerts { alerts } => Ok(alerts),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub async fn subscribe_bot(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    bot_public_key: String,
) -> Result<(), String> {
    let pk = parse_public_key(&bot_public_key)?;
    match bridge::send_request(
        &state,
        &server_id,
        ServerRequest::SubscribeBot { bot_public_key: pk },
    )
    .await
    .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub async fn unsubscribe_bot(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    bot_public_key: String,
) -> Result<(), String> {
    let pk = parse_public_key(&bot_public_key)?;
    match bridge::send_request(
        &state,
        &server_id,
        ServerRequest::UnsubscribeBot { bot_public_key: pk },
    )
    .await
    .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub async fn list_my_subscriptions(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<Vec<String>, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::ListMySubscriptions)
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::MySubscriptions { bot_public_keys } => {
            Ok(bot_public_keys.iter().map(|pk| pk.to_string()).collect())
        }
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// Webhook management commands (MANAGE_SERVER gated on server)
// ---------------------------------------------------------------------------

/// IPC return type for create_webhook / regenerate_webhook_token.
/// Includes the relay server_id_hex so the client can build the ingest URL.
/// Shown once; the token is never retrievable after this response.
#[derive(serde::Serialize)]
pub struct WebhookTokenResult {
    pub id: i64,
    pub token: String,
    pub server_id_hex: Option<String>,
}

/// Create an incoming webhook for a channel. Returns id, token, and
/// server_id_hex (relay server hex id for URL building; None on direct servers).
#[tauri::command]
pub async fn create_webhook(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
    name: String,
) -> Result<WebhookTokenResult, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::CreateWebhook { channel_id, name })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::WebhookToken { id, token, server_id_hex } => Ok(WebhookTokenResult { id, token, server_id_hex }),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// List all webhooks for a channel (no tokens returned).
#[tauri::command]
pub async fn list_webhooks(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
) -> Result<Vec<WebhookInfo>, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::ListWebhooks { channel_id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Webhooks { webhooks } => Ok(webhooks),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Delete a webhook by id.
#[tauri::command]
pub async fn delete_webhook(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    id: i64,
) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::DeleteWebhook { id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Rotate the secret token for a webhook. Returns id, new_token, and
/// server_id_hex for URL building. Token shown once; never retrievable.
#[tauri::command]
pub async fn regenerate_webhook_token(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    id: i64,
) -> Result<WebhookTokenResult, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::RegenerateWebhookToken { id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::WebhookToken { id, token, server_id_hex } => Ok(WebhookTokenResult { id, token, server_id_hex }),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// Slash command management (MANAGE_SERVER gated for add/delete; all members
// can list and run).
// ---------------------------------------------------------------------------

/// List all slash commands registered on this server.
#[tauri::command]
pub async fn list_commands(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<Vec<CommandInfo>, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::ListCommands {})
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Commands { commands } => Ok(commands),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Register a new slash command. `kind` is "text" or "api"; the remaining
/// fields are kind-specific and optional.
#[tauri::command]
pub async fn add_command(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    name: String,
    trigger: String,
    description: String,
    kind: String,
    body_text: Option<String>,
    url_template: Option<String>,
    value_path: Option<String>,
    response_template: Option<String>,
    unit: Option<String>,
) -> Result<(), String> {
    match bridge::send_request(
        &state,
        &server_id,
        ServerRequest::AddCommand {
            name,
            trigger,
            description,
            kind,
            body_text,
            url_template,
            value_path,
            response_template,
            unit,
        },
    )
    .await
    .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Delete a slash command by id (MANAGE_SERVER gated).
#[tauri::command]
pub async fn delete_command(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    id: i64,
) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::DeleteCommand { id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Invoke a slash command. On success the server broadcasts the bot response
/// to the channel; returns Ok. On failure (unknown trigger, rate-limit, etc.)
/// returns Err so the invoker can display the reason without posting to the channel.
///
/// Returns `Some(text)` when the server answered with `Notice` — an
/// invoker-only confirmation that posts NOTHING to the channel (the `/remind`
/// kind); `None` for the ordinary post-to-channel kinds. Callers that don't
/// care simply discard the value.
#[tauri::command]
pub async fn run_command(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    trigger: String,
    channel_id: u64,
    args: String,
) -> Result<Option<String>, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::RunCommand { trigger, channel_id, args })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(None),
        ServerResponse::Notice { text } => Ok(Some(text)),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub async fn update_role(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    role_id: u64,
    name: Option<String>,
    permissions: Option<u64>,
    color: Option<String>,
    position: Option<u32>,
    hoist: Option<bool>,
) -> Result<(), String> {
    let response = bridge::send_request(&state, &server_id,
        ServerRequest::UpdateRole { role_id, name, permissions, color, position, hoist })
        .await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn assign_role(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    member_key: String,
    role_id: u64,
) -> Result<(), String> {
    let pk = parse_public_key(&member_key)?;
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::AssignRole { member_key: pk, role_id },
    )
    .await
    .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn remove_role(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    member_key: String,
    role_id: u64,
) -> Result<(), String> {
    let pk = parse_public_key(&member_key)?;
    let response = bridge::send_request(
        &state,
        &server_id,
        ServerRequest::RemoveRole { member_key: pk, role_id },
    )
    .await
    .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn kick_member(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    member_key: String,
    log_server_id: Option<String>,
) -> Result<(), String> {
    let pk = parse_public_key(&member_key)?;
    let response = bridge::send_request(&state, &server_id, ServerRequest::KickMember { member_key: pk.clone() })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => {
            if let Some(log_sid) = log_server_id {
                moderate_member(
                    &state,
                    &server_id,
                    &log_sid,
                    farder_crypto::event_log::EventPayload::MemberRemoved { member: pk },
                )
                .await?;
            }
            Ok(())
        }
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn ban_member(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    member_key: String,
    log_server_id: Option<String>,
    reason: Option<String>,
) -> Result<(), String> {
    let pk = parse_public_key(&member_key)?;
    let response = bridge::send_request(&state, &server_id, ServerRequest::BanMember { member_key: pk.clone(), reason })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => {
            if let Some(log_sid) = log_server_id {
                moderate_member(
                    &state,
                    &server_id,
                    &log_sid,
                    farder_crypto::event_log::EventPayload::MemberBanned { member: pk },
                )
                .await?;
            }
            Ok(())
        }
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn unban_member(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    member_key: String,
    log_server_id: Option<String>,
) -> Result<(), String> {
    let pk = parse_public_key(&member_key)?;
    let response = bridge::send_request(&state, &server_id, ServerRequest::UnbanMember { member_key: pk.clone() })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => {
            if let Some(log_sid) = log_server_id {
                moderate_member(
                    &state,
                    &server_id,
                    &log_sid,
                    farder_crypto::event_log::EventPayload::MemberUnbanned { member: pk },
                )
                .await?;
            }
            Ok(())
        }
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn list_banned(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<Vec<farder_protocol::server::BannedMember>, String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::ListBanned)
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::BannedMembers { entries } => Ok(entries),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn timeout_member(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    member_key: String,
    until_ms: u64,
    reason: Option<String>,
) -> Result<(), String> {
    let pk = parse_public_key(&member_key)?;
    let response = bridge::send_request(&state, &server_id, ServerRequest::TimeoutMember { member_key: pk, until_ms, reason })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn remove_timeout(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    member_key: String,
) -> Result<(), String> {
    let pk = parse_public_key(&member_key)?;
    let response = bridge::send_request(&state, &server_id, ServerRequest::RemoveTimeout { member_key: pk })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn list_audit_events(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    before_id: Option<u64>,
    limit: u32,
) -> Result<Vec<farder_protocol::server::AuditEvent>, String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::ListAuditEvents { before_id, limit })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::AuditEventsList { events } => Ok(events),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// Voice message helper
// ---------------------------------------------------------------------------

/// Decode base64 audio data and write it to a temp file; returns the file path.
#[tauri::command]
pub fn save_temp_audio(data: String) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(&data).map_err(|e| e.to_string())?;
    let tmp_dir = std::env::temp_dir();
    let filename = format!("farder_voice_{}.webm", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
    let path = tmp_dir.join(&filename);
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

// Global recording state
static RECORDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
// The live recording's (session id, wav path). The id rides WITH the path so
// stop_recording can check-and-take atomically under one lock — a stale stop
// must never be able to pass an entry check, sleep, and then steal a NEWER
// session's path (the race the voice recorder hit under StrictMode).
static RECORDING_PATH: Mutex<Option<(u64, String)>> = Mutex::new(None);
// Monotonic id of the CURRENT recording session. `stop_recording(Some(id))`
// only stops a matching session, so a stale stop (e.g. from React StrictMode's
// dev double-mount, or any late async cleanup) is a harmless no-op instead of
// killing a newer recording it doesn't own.
static RECORDING_SESSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Start recording audio from the saved input device (or system default).
/// Writes WAV to a temp file. Returns the new recording's session id; pass it
/// to `stop_recording` so only the owner can stop this recording.
#[tauri::command]
pub async fn start_recording() -> Result<u64, String> {
    use std::sync::atomic::Ordering;
    // compare_exchange closes the check-then-store race between two
    // concurrent starts (both seeing false, both storing true).
    if RECORDING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("already recording".to_string());
    }
    let session = RECORDING_SESSION.fetch_add(1, Ordering::SeqCst) + 1;

    let tmp_dir = std::env::temp_dir();
    let filename = format!("farder_voice_{}.wav", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
    let path = tmp_dir.join(&filename);
    let path_str = path.to_string_lossy().to_string();
    if let Ok(mut g) = RECORDING_PATH.lock() {
        *g = Some((session, path_str.clone()));
    }

    // Read the saved input device name before moving into spawn_blocking.
    let saved_input_device = read_input_device();

    // The cpal stream must be created, used, and dropped on one thread, so all
    // setup happens inside spawn_blocking. Report the setup result back over a
    // oneshot so a failure (no device, bad config, file create, stream build)
    // surfaces to the caller instead of silently panicking a detached worker.
    let (setup_tx, setup_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();

    tauri::async_runtime::spawn_blocking(move || {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        type RecWriter = hound::WavWriter<std::io::BufWriter<std::fs::File>>;
        let built = (|| -> Result<(cpal::Stream, Arc<Mutex<Option<RecWriter>>>), String> {
            let host = cpal::default_host();
            // Use the saved input device by name, or fall back to the system default.
            let device = match saved_input_device.as_deref() {
                None => host
                    .default_input_device()
                    .ok_or_else(|| "no input device available".to_string())?,
                Some(name) => host
                    .input_devices()
                    .map_err(|e| format!("input_devices: {e}"))?
                    .find(|d| d.name().map(|n| n == name).unwrap_or(false))
                    .or_else(|| host.default_input_device())
                    .ok_or_else(|| "no input device available".to_string())?,
            };
            let config = device
                .default_input_config()
                .map_err(|e| format!("input config: {e}"))?;
            let spec = hound::WavSpec {
                channels: config.channels(),
                sample_rate: config.sample_rate().0,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let writer = Arc::new(Mutex::new(Some(
                hound::WavWriter::create(&path, spec).map_err(|e| format!("create wav: {e}"))?,
            )));
            let stream = match config.sample_format() {
                cpal::SampleFormat::F32 => {
                    let wc = Arc::clone(&writer);
                    device.build_input_stream(
                        &config.into(),
                        move |data: &[f32], _: &cpal::InputCallbackInfo| {
                            if let Ok(mut guard) = wc.lock() {
                                if let Some(w) = guard.as_mut() {
                                    for &sample in data {
                                        let _ = w.write_sample((sample * 32767.0) as i16);
                                    }
                                }
                            }
                        },
                        |err| eprintln!("recording error: {err}"),
                        None,
                    ).map_err(|e| format!("build input stream: {e}"))?
                }
                cpal::SampleFormat::I16 => {
                    let wc = Arc::clone(&writer);
                    device.build_input_stream(
                        &config.into(),
                        move |data: &[i16], _: &cpal::InputCallbackInfo| {
                            if let Ok(mut guard) = wc.lock() {
                                if let Some(w) = guard.as_mut() {
                                    for &sample in data {
                                        let _ = w.write_sample(sample);
                                    }
                                }
                            }
                        },
                        |err| eprintln!("recording error: {err}"),
                        None,
                    ).map_err(|e| format!("build input stream: {e}"))?
                }
                other => return Err(format!("unsupported sample format: {other:?}")),
            };
            stream.play().map_err(|e| format!("stream.play: {e}"))?;
            Ok((stream, writer))
        })();

        match built {
            Ok((stream, writer)) => {
                let _ = setup_tx.send(Ok(()));
                while RECORDING.load(std::sync::atomic::Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                drop(stream);
                if let Ok(mut guard) = writer.lock() {
                    if let Some(w) = guard.take() {
                        let _ = w.finalize();
                    }
                }
            }
            Err(e) => {
                RECORDING.store(false, std::sync::atomic::Ordering::SeqCst);
                let _ = setup_tx.send(Err(e));
            }
        }
    });

    match setup_rx.await {
        Ok(Ok(())) => Ok(session),
        Ok(Err(e)) => {
            // cpal setup failed inside spawn_blocking; flag was already reset
            // there (Err arm at line ~1904), but reset path/flag here too for
            // belt-and-suspenders clarity.
            RECORDING.store(false, Ordering::SeqCst);
            if let Ok(mut g) = RECORDING_PATH.lock() { *g = None; }
            Err(e)
        }
        Err(_) => {
            // The spawn_blocking thread panicked or was dropped before sending
            // the setup result — flag was never reset in that thread. Reset it
            // here so subsequent start_recording calls are not permanently wedged.
            RECORDING.store(false, Ordering::SeqCst);
            if let Ok(mut g) = RECORDING_PATH.lock() { *g = None; }
            Err("recording thread ended before it started".to_string())
        }
    }
}

/// Stop recording and return the path to the WAV file.
/// `session`: pass the id returned by `start_recording` to stop ONLY that
/// recording (a mismatched id errors without touching the live one). `None`
/// stops whatever is recording (used to recover a wedged/orphaned recording).
#[tauri::command]
pub async fn stop_recording(session: Option<u64>) -> Result<String, String> {
    use std::sync::atomic::Ordering;
    // Atomically validate the session AND claim the path under one lock. The
    // earlier version checked the session, slept 500ms, THEN took the path —
    // letting a stale stop pass the check while its target was still current,
    // sleep through a newer session starting, and steal the newer path.
    let path = {
        let mut g = RECORDING_PATH
            .lock()
            .map_err(|_| "recording state poisoned".to_string())?;
        match g.as_ref() {
            None => return Err("no recording in progress".to_string()),
            Some((owner, _)) => {
                if let Some(s) = session {
                    if s != *owner {
                        return Err("stale recording session".to_string());
                    }
                }
                let (_, p) = g.take().expect("checked Some above");
                p
            }
        }
    };
    // We own the live recording: signal the cpal thread to stop, then give it a
    // moment to finalize the WAV. Async sleep so we don't block a worker thread.
    RECORDING.store(false, Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    if !std::path::Path::new(&path).exists() {
        return Err("recording failed — no audio file was written".to_string());
    }
    Ok(path)
}

/// Play a WAV file on the saved output device (or system default). Reads the entire file and
/// feeds samples through a cpal output stream, then drops the stream when done.
/// Used by the "Test mic" flow to play back a just-recorded WAV.
#[tauri::command]
pub async fn play_audio_file(path: String) -> Result<(), String> {
    // Read the saved output device name before moving into spawn_blocking.
    let saved_output_device = read_output_device();
    tauri::async_runtime::spawn_blocking(move || {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        // Open and decode the WAV file.
        let mut reader = hound::WavReader::open(&path)
            .map_err(|e| format!("open wav: {e}"))?;
        let spec = reader.spec();
        let sample_rate = spec.sample_rate;
        let channels = spec.channels as usize;

        // Decode all samples to f32.
        let samples_f32: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
            (hound::SampleFormat::Int, 16) => reader
                .samples::<i16>()
                .filter_map(|s| s.ok())
                .map(|s| s as f32 / 32768.0)
                .collect(),
            (hound::SampleFormat::Int, 32) => reader
                .samples::<i32>()
                .filter_map(|s| s.ok())
                .map(|s| s as f32 / 2147483648.0)
                .collect(),
            (hound::SampleFormat::Float, _) => reader
                .samples::<f32>()
                .filter_map(|s| s.ok())
                .collect(),
            (fmt, bits) => {
                return Err(format!("unsupported WAV format: {fmt:?} {bits}bit"));
            }
        };

        if samples_f32.is_empty() {
            return Ok(());
        }

        let host = cpal::default_host();
        // Use the saved output device by name, or fall back to the system default.
        let device = match saved_output_device.as_deref() {
            None => host
                .default_output_device()
                .ok_or_else(|| "no output device available".to_string())?,
            Some(name) => host
                .output_devices()
                .map_err(|e| format!("output_devices: {e}"))?
                .find(|d| d.name().map(|n| n == name).unwrap_or(false))
                .or_else(|| host.default_output_device())
                .ok_or_else(|| "no output device available".to_string())?,
        };
        let config = device
            .default_output_config()
            .map_err(|e| format!("output config: {e}"))?;
        let dev_rate = config.sample_rate().0;
        let dev_channels = config.channels() as usize;

        // Resample from WAV rate to device rate if they differ.
        let resampled: Vec<f32> = if dev_rate != sample_rate {
            // Simple linear interpolation resampler (same approach as audio_cpal.rs).
            let ratio = sample_rate as f64 / dev_rate as f64;
            let out_len = (samples_f32.len() as f64 / channels as f64 / ratio) as usize * channels;
            let mut out = Vec::with_capacity(out_len);
            // Resample per channel then interleave.
            for ch in 0..channels {
                let channel_samples: Vec<f32> = samples_f32
                    .iter()
                    .skip(ch)
                    .step_by(channels)
                    .copied()
                    .collect();
                let out_frames = (channel_samples.len() as f64 / ratio) as usize;
                let mut ch_out: Vec<f32> = Vec::with_capacity(out_frames);
                let mut pos: f64 = 0.0;
                while (pos + 1.0) < channel_samples.len() as f64 {
                    let lo = pos.floor() as usize;
                    let hi = lo + 1;
                    let frac = (pos - lo as f64) as f32;
                    let a = channel_samples[lo];
                    let b = if hi < channel_samples.len() { channel_samples[hi] } else { a };
                    ch_out.push(a + (b - a) * frac);
                    pos += ratio;
                }
                // Interleave: we need all channels at the same frame count.
                if out.is_empty() {
                    out.resize(ch_out.len() * channels, 0.0);
                }
                for (i, s) in ch_out.iter().enumerate() {
                    if ch + i * channels < out.len() {
                        out[ch + i * channels] = *s;
                    }
                }
            }
            out
        } else {
            samples_f32.clone()
        };

        // Upmix or downmix to device channel count.
        // WAV channels -> device channels by repeating or dropping.
        let final_samples: Vec<f32> = if dev_channels == channels {
            resampled
        } else {
            // Per-frame: take the WAV frame and replicate mono to all dev channels,
            // or downmix multi-ch WAV to the device's channel count.
            let wav_frames = resampled.len() / channels.max(1);
            let mut out = Vec::with_capacity(wav_frames * dev_channels);
            for frame in 0..wav_frames {
                let start = frame * channels;
                for dc in 0..dev_channels {
                    let src = if channels == 1 {
                        resampled[start]
                    } else {
                        // Average to mono then broadcast, or map channel by channel.
                        let wch = dc.min(channels - 1);
                        resampled[start + wch]
                    };
                    out.push(src);
                }
            }
            out
        };

        // Shared cursor into the final sample buffer.
        let samples_arc = std::sync::Arc::new(final_samples);
        let cursor = std::sync::Arc::new(std::sync::Mutex::new(0usize));
        let done_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let samples_cb = std::sync::Arc::clone(&samples_arc);
        let cursor_cb = std::sync::Arc::clone(&cursor);
        let done_cb = std::sync::Arc::clone(&done_flag);

        let build_and_play = |fmt: cpal::SampleFormat| -> Result<cpal::Stream, String> {
            match fmt {
                cpal::SampleFormat::F32 => {
                    let s = std::sync::Arc::clone(&samples_cb);
                    let c = std::sync::Arc::clone(&cursor_cb);
                    let d = std::sync::Arc::clone(&done_cb);
                    device.build_output_stream(
                        &config.clone().into(),
                        move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                            let mut pos = c.lock().unwrap();
                            let total = s.len();
                            for o in out.iter_mut() {
                                if *pos < total {
                                    *o = s[*pos];
                                    *pos += 1;
                                } else {
                                    *o = 0.0;
                                    d.store(true, std::sync::atomic::Ordering::SeqCst);
                                }
                            }
                        },
                        |err| eprintln!("[play_audio_file] cpal error: {err}"),
                        None,
                    ).map_err(|e| format!("build_output_stream: {e}"))
                }
                cpal::SampleFormat::I16 => {
                    let s = std::sync::Arc::clone(&samples_cb);
                    let c = std::sync::Arc::clone(&cursor_cb);
                    let d = std::sync::Arc::clone(&done_cb);
                    device.build_output_stream(
                        &config.clone().into(),
                        move |out: &mut [i16], _: &cpal::OutputCallbackInfo| {
                            let mut pos = c.lock().unwrap();
                            let total = s.len();
                            for o in out.iter_mut() {
                                if *pos < total {
                                    *o = (s[*pos] * 32767.0) as i16;
                                    *pos += 1;
                                } else {
                                    *o = 0;
                                    d.store(true, std::sync::atomic::Ordering::SeqCst);
                                }
                            }
                        },
                        |err| eprintln!("[play_audio_file] cpal error: {err}"),
                        None,
                    ).map_err(|e| format!("build_output_stream: {e}"))
                }
                cpal::SampleFormat::I32 => {
                    let s = std::sync::Arc::clone(&samples_cb);
                    let c = std::sync::Arc::clone(&cursor_cb);
                    let d = std::sync::Arc::clone(&done_cb);
                    device.build_output_stream(
                        &config.clone().into(),
                        move |out: &mut [i32], _: &cpal::OutputCallbackInfo| {
                            let mut pos = c.lock().unwrap();
                            let total = s.len();
                            for o in out.iter_mut() {
                                if *pos < total {
                                    *o = (s[*pos] * 2147483647.0) as i32;
                                    *pos += 1;
                                } else {
                                    *o = 0;
                                    d.store(true, std::sync::atomic::Ordering::SeqCst);
                                }
                            }
                        },
                        |err| eprintln!("[play_audio_file] cpal error: {err}"),
                        None,
                    ).map_err(|e| format!("build_output_stream: {e}"))
                }
                other => Err(format!("unsupported output sample format: {other:?}")),
            }
        };

        let stream = build_and_play(config.sample_format())?;
        stream.play().map_err(|e| format!("stream.play: {e}"))?;

        // Poll until playback finishes (all samples consumed) or we time out
        // (~10 seconds max to avoid a hang if the done flag is never set).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if done_flag.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            {
                let pos = cursor.lock().unwrap();
                if *pos >= samples_arc.len() {
                    break;
                }
            }
            if std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // Small tail so the last buffer can drain through the device.
        std::thread::sleep(std::time::Duration::from_millis(200));
        drop(stream);
        Ok(())
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

// ---------------------------------------------------------------------------
// Desktop notification command
// ---------------------------------------------------------------------------

/// Show a desktop notification using notify-send (Linux) or equivalent.
/// Falls back silently on unsupported platforms or when notify-send is absent.
#[tauri::command]
pub fn show_notification(title: String, body: String) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .arg(&title)
            .arg(&body)
            .spawn();
    }
    #[cfg(target_os = "windows")]
    {
        // On Windows, use PowerShell to show a toast notification.
        let script = format!(
            "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null; \
             $template = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02); \
             $template.SelectSingleNode('//text[@id=1]').InnerText = '{}'; \
             $template.SelectSingleNode('//text[@id=2]').InnerText = '{}'; \
             [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('Farder').Show([Windows.UI.Notifications.ToastNotification]::new($template));",
            title.replace('\'', "''"),
            body.replace('\'', "''")
        );
        let _ = std::process::Command::new("powershell")
            .args(["-WindowStyle", "Hidden", "-Command", &script])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            body.replace('"', "\\\""),
            title.replace('"', "\\\"")
        );
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .spawn();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Notification preferences commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_notification_prefs() -> Result<serde_json::Value, String> {
    let path = farder_data_dir().join("notifications.json");
    let data = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        // Default prefs
        serde_json::json!({
            "dmNotifications": "all",
            "dmAllowedUsers": [],
            "servers": {},
            "mentionNotifications": true,
            "keywords": [],
            "mutedUsers": []
        }).to_string()
    });
    let v: serde_json::Value = serde_json::from_str(&data).map_err(|e| e.to_string())?;
    Ok(v)
}

#[tauri::command]
pub fn save_notification_prefs(prefs: serde_json::Value) -> Result<(), String> {
    let path = farder_data_dir().join("notifications.json");
    std::fs::write(&path, prefs.to_string()).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// DM E2EE commands
// ---------------------------------------------------------------------------

/// Encrypt a plaintext string for a DM peer, returning hex-encoded ciphertext.
///
/// The shared secret is derived from our Ed25519 signing key and the peer's
/// Ed25519 verifying key via X25519 ECDH.  The resulting 32-byte secret is
/// used directly as the AES-256-GCM key.
#[tauri::command]
pub fn dm_encrypt(
    state: State<'_, Arc<AppState>>,
    their_public_key: String,
    plaintext: String,
) -> Result<String, String> {
    let our_sk = {
        let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
        lock.ok_or_else(|| "no identity — unlock your identity first".to_string())?
    };
    let their_pk = parse_public_key(&their_public_key)?;
    let shared = farder_crypto::key_exchange::derive_dm_shared_secret(&our_sk, their_pk.as_bytes())
        .map_err(|e| e.to_string())?;
    let ciphertext = farder_crypto::encryption::encrypt(&shared, plaintext.as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(hex::encode(ciphertext))
}

/// Decrypt a hex-encoded ciphertext from a DM peer, returning the plaintext string.
#[tauri::command]
pub fn dm_decrypt(
    state: State<'_, Arc<AppState>>,
    their_public_key: String,
    ciphertext_hex: String,
) -> Result<String, String> {
    let our_sk = {
        let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
        lock.ok_or_else(|| "no identity — unlock your identity first".to_string())?
    };
    let their_pk = parse_public_key(&their_public_key)?;
    let shared = farder_crypto::key_exchange::derive_dm_shared_secret(&our_sk, their_pk.as_bytes())
        .map_err(|e| e.to_string())?;
    let ciphertext = hex::decode(&ciphertext_hex).map_err(|e| e.to_string())?;
    let plaintext_bytes = farder_crypto::encryption::decrypt(&shared, &ciphertext)
        .map_err(|e| e.to_string())?;
    String::from_utf8(plaintext_bytes).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Media stream commands (replaces the old voice arms)
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn join_channel_media(state: State<'_, Arc<AppState>>, server_id: String, channel_id: u64) -> Result<(), String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::JoinChannelMedia { channel_id })
        .await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn leave_channel_media(state: State<'_, Arc<AppState>>, server_id: String, channel_id: u64) -> Result<(), String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::LeaveChannelMedia { channel_id })
        .await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn get_media_state(state: State<'_, Arc<AppState>>, server_id: String, channel_id: u64) -> Result<serde_json::Value, String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::GetMediaState { channel_id })
        .await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::MediaStateResp { participants } => Ok(serde_json::to_value(participants).unwrap()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn join_stream(state: State<'_, Arc<AppState>>, server_id: String, channel_id: u64) -> Result<Vec<u8>, String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::JoinStream { channel_id })
        .await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::StreamSessionStarted { session_id } => Ok(session_id.to_vec()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn leave_stream(state: State<'_, Arc<AppState>>, server_id: String) -> Result<(), String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::LeaveStream)
        .await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

fn parse_track_kind(kind: &str) -> Result<farder_protocol::server::TrackKind, String> {
    match kind {
        "audio" | "Audio" => Ok(farder_protocol::server::TrackKind::Audio),
        "video" | "Video" => Ok(farder_protocol::server::TrackKind::Video),
        "screenAudio" | "screen_audio" | "ScreenAudio" => {
            Ok(farder_protocol::server::TrackKind::ScreenAudio)
        }
        other => Err(format!("invalid track kind: {other}")),
    }
}

#[tauri::command]
pub async fn enable_track(state: State<'_, Arc<AppState>>, server_id: String, kind: String) -> Result<(), String> {
    let kind = parse_track_kind(&kind)?;
    let response = bridge::send_request(&state, &server_id, ServerRequest::EnableTrack { kind })
        .await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn disable_track(state: State<'_, Arc<AppState>>, server_id: String, kind: String) -> Result<(), String> {
    let kind = parse_track_kind(&kind)?;
    let response = bridge::send_request(&state, &server_id, ServerRequest::DisableTrack { kind })
        .await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn set_deafen(state: State<'_, Arc<AppState>>, server_id: String, deafened: bool) -> Result<(), String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::SetDeafen { deafened })
        .await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn offer_stream_key(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    kind: String,
    wrapped_keys: Vec<(Vec<u8>, Vec<u8>)>,
) -> Result<(), String> {
    use farder_crypto::identity::PublicKey;
    let kind = parse_track_kind(&kind)?;
    let wrapped: Vec<(PublicKey, Vec<u8>)> = wrapped_keys.into_iter()
        .map(|(pk_bytes, wrapped)| {
            if pk_bytes.len() != 32 { return Err("pubkey must be 32 bytes".to_string()); }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&pk_bytes);
            Ok((PublicKey::from_bytes(arr), wrapped))
        })
        .collect::<Result<_, _>>()?;
    let response = bridge::send_request(&state, &server_id,
        ServerRequest::OfferStreamKey { kind, wrapped_keys: wrapped })
        .await.map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// Voice controller commands (sub-project #3.3 Task 11)
//
// Thin wrappers around `VoiceController`. The controller owns the audio
// pipeline + per-peer recv tasks; these commands just translate frontend
// invocations into controller calls and (for `voice_join`) construct the
// per-call `QuinnServerSession` adapter that wraps the right
// `ServerConnection` from `AppState::servers`.
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn voice_join(
    voice: State<'_, Arc<crate::voice::VoiceController>>,
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
) -> Result<(), String> {
    let server_conn = state.get_server(&server_id)?;
    // Apply the saved mic sensitivity before the send task spawns.
    voice.set_speak_threshold(crate::voice::sensitivity_to_threshold(read_voice_sensitivity()));
    let session = crate::voice_bridge::QuinnServerSession::new(
        Arc::clone(&state),
        server_id.clone(),
    )?;
    let config = crate::voice::JoinConfig {
        mode: if read_voice_mode() == "PushToTalk" {
            crate::voice::VoiceMode::PushToTalk
        } else {
            crate::voice::VoiceMode::OpenMic
        },
        peer_volumes: read_peer_volumes(),
        connection: Some(server_conn.connection.clone()),
        input_device: read_input_device(),
        output_device: read_output_device(),
    };
    voice
        .join_with_config(
            channel_id,
            Arc::new(session) as Arc<dyn crate::voice::ServerSession>,
            config,
        )
        .await
}

#[tauri::command]
pub async fn voice_toggle_transmit(
    voice: State<'_, Arc<crate::voice::VoiceController>>,
) -> Result<bool, String> {
    Ok(voice.toggle_transmit().await)
}

#[tauri::command]
pub async fn voice_set_peer_volume(
    voice: State<'_, Arc<crate::voice::VoiceController>>,
    pubkey_hex: String,
    volume: f32,
) -> Result<(), String> {
    let clamped = volume.clamp(0.0, 2.0);
    persist_peer_volume(&pubkey_hex, clamped)?;
    voice.set_peer_volume(pubkey_hex, clamped).await
}

#[tauri::command]
pub async fn voice_set_screen_audio_gain(
    voice: State<'_, Arc<crate::voice::VoiceController>>,
    pubkey_hex: String,
    gain: f32,
) -> Result<(), String> {
    voice.set_screen_audio_gain(pubkey_hex, gain).await
}

#[tauri::command]
pub async fn voice_leave(
    voice: State<'_, Arc<crate::voice::VoiceController>>,
) -> Result<(), String> {
    voice.leave().await
}

#[tauri::command]
pub async fn list_display_sources() -> Result<Vec<crate::display::DisplaySource>, String> {
    crate::display::make_display_backend().enumerate_sources()
}

#[tauri::command]
pub async fn voice_start_screen_share(
    voice: State<'_, Arc<crate::voice::VoiceController>>,
    fps: u32,
    max_width: u32,
    max_height: u32,
    source_id: Option<String>,
    audio_device_id: Option<String>,
) -> Result<(), String> {
    voice.start_screen_share(fps, max_width, max_height, source_id, audio_device_id).await
}

#[tauri::command]
pub async fn voice_stop_screen_share(
    voice: State<'_, Arc<crate::voice::VoiceController>>,
) -> Result<(), String> {
    voice.stop_screen_share().await
}

#[tauri::command]
pub async fn voice_request_keyframe(
    voice: State<'_, Arc<crate::voice::VoiceController>>,
) -> Result<(), String> {
    voice.request_keyframe().await;
    Ok(())
}

#[tauri::command]
pub async fn voice_set_mute(
    voice: State<'_, Arc<crate::voice::VoiceController>>,
    muted: bool,
) -> Result<(), String> {
    voice.set_mute(muted).await
}

#[tauri::command]
pub async fn voice_set_deafen(
    voice: State<'_, Arc<crate::voice::VoiceController>>,
    deafened: bool,
) -> Result<(), String> {
    voice.set_deafen(deafened).await
}

#[tauri::command]
pub async fn voice_get_state(
    voice: State<'_, Arc<crate::voice::VoiceController>>,
) -> Result<crate::voice::VoiceState, String> {
    Ok(voice.state().await)
}

#[tauri::command]
pub async fn list_audio_output_devices() -> Result<Vec<crate::screen_audio::OutputDevice>, String> {
    crate::screen_audio::list_output_devices()
}

// ---------------------------------------------------------------------------
// Local server management commands
// ---------------------------------------------------------------------------

/// Resolve the create-server relay choice into an optional (relay addr, cert fp).
/// `None` means a direct server. Validates self-host inputs.
fn resolve_relay_choice(
    mode: &str,
    addr: Option<&str>,
    fp: Option<&str>,
) -> Result<Option<(std::net::SocketAddr, Vec<u8>)>, String> {
    match mode {
        "direct" => Ok(None),
        "farder" => crate::default_relay::default_relay()
            .map(Some)
            .ok_or_else(|| "the Farder default relay is not configured in this build".to_string()),
        "selfhost" => {
            let addr = addr.unwrap_or("").trim();
            let fp = fp.unwrap_or("").trim();
            let sock: std::net::SocketAddr = addr
                .parse()
                .map_err(|_| format!("invalid relay address '{}' (expected host:port)", addr))?;
            let fp_bytes = hex::decode(fp).map_err(|_| "relay fingerprint must be hexadecimal".to_string())?;
            if fp_bytes.len() != 32 {
                return Err(format!(
                    "relay fingerprint must be 64 hex characters (32 bytes); got {} bytes",
                    fp_bytes.len()
                ));
            }
            Ok(Some((sock, fp_bytes)))
        }
        other => Err(format!("unknown relay mode '{}'", other)),
    }
}

#[tauri::command]
pub async fn create_local_server(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    procs: State<'_, crate::server_manager::ServerProcesses>,
    name: String,
    template: String,
    privacy: String,
    icon_path: Option<String>,
    relay_mode: String,            // "farder" | "selfhost" | "direct"
    relay_addr: Option<String>,    // self-host only
    relay_fp: Option<String>,      // self-host only
) -> Result<serde_json::Value, String> {
    // Refuse to create a duplicate local server. Names are unique per machine
    // because they map to a single data directory — letting "1" exist twice
    // means two server processes fighting over one SQLite database, which
    // produces hard-to-diagnose corruption.
    let trimmed_name = name.trim();
    if trimmed_name.is_empty() {
        return Err("server name cannot be empty".to_string());
    }
    if let Some(dup) = load_server_entries()
        .into_iter()
        .find(|e| e.local.is_some() && e.name.eq_ignore_ascii_case(trimmed_name))
    {
        return Err(format!(
            "A local server named '{}' already exists at {}. Pick a different name or delete the existing one first.",
            dup.name, dup.id
        ));
    }

    let relay = resolve_relay_choice(&relay_mode, relay_addr.as_deref(), relay_fp.as_deref())?;

    // Load the owner keypair up front (needed for both paths).
    let keypair = {
        let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
        match lock.as_ref() {
            Some(bytes) => Keypair::from_signing_key_bytes(bytes),
            None => return Err("no identity keypair set -- unlock your identity first".to_string()),
        }
    };

    // Generate a stable server_id (used only in relay mode).
    let server_id: [u8; 32] = rand::random();

    let (info, child) = crate::server_manager::spawn_server(
        &name,
        &template,
        &privacy,
        relay.as_ref().map(|(a, _)| (*a, server_id)),
    )?;
    let port = info.port;
    let relayed = info.relayed;
    let local_data_dir = info.data_dir.clone();
    let local_template = info.template.clone();
    procs.register(info, child);

    // Connect + obtain the entry id (relay link or 127.0.0.1:port).
    let (conn, send, recv, session_token, address, endpoint) = if let Some((relay_addr, cert_fp)) = relay {
        let target = crate::connection::RelayTarget {
            relay_addr,
            server_id: server_id.to_vec(),
            cert_fp: cert_fp.clone(),
            invite_token: String::new(), // owner: no invite
        };
        let endpoint = match crate::tls::make_pinned_relay_endpoint(cert_fp.clone()) {
            Ok(e) => e,
            Err(e) => {
                let _ = crate::server_manager::stop_server(&procs, port);
                return Err(e.to_string());
            }
        };
        // Wait for the just-spawned local server to register with the relay,
        // then connect. We give it a ~1s head start and poll GENTLY (every
        // ~1.5s) rather than hammering: rapid retries open a fresh relay
        // connection each time and would trip the relay's per-IP rate limit,
        // which then refuses the server's own registration too (deadlocking it).
        let connected = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            loop {
                match crate::connection::connect_via_relay(endpoint.clone(), &target, &keypair, None).await {
                    Ok(t) => return t,
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(1500)).await,
                }
            }
        })
        .await;
        let (conn, send, recv, session_token) = match connected {
            Ok(t) => t,
            Err(_) => {
                crate::server_manager::stop_server(&procs, port)?;
                return Err("the relayed server did not register with the relay within 30 seconds".to_string());
            }
        };
        let link = crate::connection::build_relay_link(&target, "");
        (conn, send, recv, session_token, link, endpoint)
    } else {
        let address = format!("127.0.0.1:{}", port);
        // Wait for the direct server to be ready (poll up to 5 seconds).
        let ready = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(endpoint) = crate::tls::make_client_endpoint() {
                    if let Ok(addr) = address.parse::<std::net::SocketAddr>() {
                        if let Ok(connecting) = endpoint.connect(addr, "farder-server") {
                            if let Ok(conn) = connecting.await {
                                conn.close(0u32.into(), b"probe");
                                return;
                            }
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        })
        .await;
        if ready.is_err() {
            crate::server_manager::stop_server(&procs, port)?;
            return Err("server failed to start within 5 seconds".to_string());
        }
        let endpoint = make_client_endpoint().map_err(|e| e.to_string())?;
        let addr: std::net::SocketAddr = address.parse().map_err(|e: std::net::AddrParseError| e.to_string())?;
        let (conn, send, recv, session_token) =
            match connect_and_authenticate(endpoint.clone(), addr, &keypair, None, None).await {
                Ok(t) => t,
                Err(e) => {
                    let _ = crate::server_manager::stop_server(&procs, port);
                    return Err(e.to_string());
                }
            };
        (conn, send, recv, session_token, address, endpoint)
    };

    let media_dispatcher = std::sync::Arc::new(crate::voice::MediaInboundDispatcher::default());
    {
        let dispatcher_for_loop = media_dispatcher.clone();
        let conn_for_loop = conn.clone();
        tokio::spawn(async move {
            loop {
                match conn_for_loop.read_datagram().await {
                    Ok(bytes) => dispatcher_for_loop.dispatch(bytes).await,
                    Err(quinn::ConnectionError::ApplicationClosed { .. })
                    | Err(quinn::ConnectionError::ConnectionClosed { .. })
                    | Err(quinn::ConnectionError::LocallyClosed)
                    | Err(quinn::ConnectionError::TimedOut) => break,
                    Err(_) => break,
                }
            }
        });
    }

    // Store connection
    let server_conn = Arc::new(ServerConnection {
        endpoint,
        connection: conn,
        send_stream: tokio::sync::Mutex::new(send),
        next_request_id: AtomicU32::new(1),
        pending_requests: Mutex::new(HashMap::new()),
        event_reader_handle: Mutex::new(None),
        server_name: Mutex::new(name.clone()),
        media_dispatcher,
        session_token,
        relayed,
    });

    let handle = bridge::spawn_event_reader(app.clone(), address.clone(), Arc::clone(&server_conn), recv);
    *server_conn.event_reader_handle.lock().unwrap() = Some(handle);

    {
        let mut servers = state.servers.lock().unwrap();
        servers.insert(address.clone(), Arc::clone(&server_conn));
    }

    // Save to server list (with local config so we can respawn on relaunch)
    save_server_entry_with_config(&address, &name, Some(LocalServerConfig {
        data_dir: local_data_dir,
        template: local_template,
    }));

    // Set server avatar if provided
    if let Some(path) = icon_path {
        if let Ok(data) = std::fs::read(&path) {
            let dir = farder_data_dir().join("server_avatars");
            let _ = std::fs::create_dir_all(&dir);
            let safe_name = address.replace([':', '.', '/'], "_");
            let avatar_path = dir.join(format!("{}.png", safe_name));
            let _ = std::fs::write(&avatar_path, &data);
        }
    }

    // Fetch server info
    let response = bridge::send_request(
        &state,
        &address,
        ServerRequest::GetServerInfo,
    )
    .await
    .map_err(|e| e.to_string())?;

    match response {
        ServerResponse::ServerInfo {
            name: srv_name,
            member_count,
            channels,
            categories,
            roles,
            owner_public_key,
            server_id: log_server_id,
        } => {
            *server_conn.server_name.lock().unwrap() = srv_name.clone();
            // Sync our signed profile to the newly created server in the background.
            {
                let state_arc: Arc<AppState> = Arc::clone(state.inner());
                let sid = address.clone();
                tokio::spawn(async move {
                    if let Err(e) = crate::profile_sync::push_profile_on_connect(&state_arc, &sid).await {
                        eprintln!("[profile-sync] push after create to {} failed: {}", sid, e);
                    }
                });
            }
            // Negotiate v2 on this freshly-created connection too: a server the
            // user just created is the FIRST one they'll open, so its connection
            // must be v2-negotiated before the frontend's `get_server_info_v2`.
            negotiate_protocol(&state, &address).await;

            Ok(serde_json::json!({
                "address": address,
                "server_name": srv_name,
                "member_count": member_count,
                "channels": channels,
                "categories": categories,
                "roles": roles,
                "owner_public_key": owner_public_key,
                "relayed": relayed,
                "server_id": log_server_id,
            }))
        }
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub fn stop_local_server(
    procs: State<'_, crate::server_manager::ServerProcesses>,
    port: u16,
) -> Result<(), String> {
    crate::server_manager::stop_server(&procs, port)
}

#[tauri::command]
pub fn get_local_servers(
    procs: State<'_, crate::server_manager::ServerProcesses>,
) -> Vec<crate::server_manager::ManagedServer> {
    procs.list()
}

#[tauri::command]
pub fn list_templates() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "id": "blank",
            "name": "Blank",
            "description": "Empty server — start from scratch"
        }),
        serde_json::json!({
            "id": "friend-group",
            "name": "Friends",
            "description": "Casual hangout for a small group of friends"
        }),
        serde_json::json!({
            "id": "gaming-community",
            "name": "Gaming",
            "description": "Voice lobbies, LFG, and game channels"
        }),
        serde_json::json!({
            "id": "organization",
            "name": "Organization",
            "description": "Teams, projects, and announcements"
        }),
        serde_json::json!({
            "id": "public-community",
            "name": "Community",
            "description": "Public community with moderation tools"
        }),
    ]
}

/// Find and kill any orphan farder-server processes pointing at the given
/// data directory. Used at startup to clean up zombies left over from prior
/// dev sessions where the Tauri exit hook didn't fire (Ctrl+C in dev,
/// kill -9, OOM, crashes). Each orphan against the same SQLite DB is a
/// concurrency hazard.
///
/// Best-effort: uses `pgrep -af farder-server` on Unix; no-op on Windows.
fn reap_orphan_servers_for_data_dir(data_dir: &str) {
    #[cfg(unix)]
    {
        let output = match std::process::Command::new("pgrep")
            .args(["-af", "farder-server"])
            .output()
        {
            Ok(o) => o,
            Err(_) => return, // pgrep not available — skip silently
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let our_pid = std::process::id();
        for line in stdout.lines() {
            let mut parts = line.splitn(2, ' ');
            let pid_str = match parts.next() {
                Some(p) => p,
                None => continue,
            };
            let cmdline = parts.next().unwrap_or("");
            // Only kill orphans whose --db arg matches OUR data directory.
            if !cmdline.contains(data_dir) {
                continue;
            }
            let pid: u32 = match pid_str.parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            if pid == our_pid {
                continue;
            }
            // SIGKILL — these processes have already escaped graceful shutdown.
            let _ = std::process::Command::new("kill")
                .args(["-9", pid_str])
                .status();
            eprintln!("[restart] Killed orphan farder-server pid {} for data dir {}", pid, data_dir);
        }
    }
    let _ = data_dir; // silence unused-var on non-unix
}

/// Restart any locally-managed servers from the saved server list.
/// Called on app startup before connecting.
#[tauri::command]
pub fn restart_local_servers(
    procs: State<'_, crate::server_manager::ServerProcesses>,
) -> Vec<ServerEntry> {
    let entries = load_server_entries();
    let mut restarted = Vec::new();

    for entry in &entries {
        if let Some(ref local) = entry.local {
            reap_orphan_servers_for_data_dir(&local.data_dir);

            // A relayed server's id is its relay link (stable across restarts);
            // respawn it in relay mode and keep the same id. A direct server gets
            // a fresh local port (and id) each launch.
            let relay_addr = crate::connection::parse_relay_target(&entry.id).map(|t| t.relay_addr);
            match crate::server_manager::spawn_server_with_data_dir(
                &entry.name,
                &local.template,
                &local.data_dir,
                relay_addr,
            ) {
                Ok((info, child)) => {
                    let new_id = if relay_addr.is_some() {
                        entry.id.clone() // relay link is stable
                    } else {
                        format!("127.0.0.1:{}", info.port)
                    };
                    eprintln!("[restart] Respawned '{}' as {}", entry.name, new_id);
                    procs.register(info, child);
                    restarted.push(ServerEntry {
                        id: new_id,
                        name: entry.name.clone(),
                        local: entry.local.clone(),
                    });
                }
                Err(e) => {
                    eprintln!("Failed to restart local server '{}': {}", entry.name, e);
                }
            }
        } else {
            restarted.push(entry.clone());
        }
    }

    // Update the saved entries with new addresses (ports may differ)
    let _ = std::fs::write(servers_list_path(), serde_json::to_string(&restarted).unwrap());

    restarted
}

// ---------------------------------------------------------------------------
// submit_event — build + sign a MessagePosted event and submit it to the server
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct EventAcceptedResult {
    pub event_hash: String,
    pub timestamp: u64,
}

#[tauri::command]
pub async fn submit_event(
    state: State<'_, Arc<AppState>>,
    server_id: String,     // connection key (address) — routes the request to the right server
    log_server_id: String, // genesis hash — stamps EventCore.server_id and keys the device chain
    channel_id: u64,
    content: String,
    reply_to: Option<String>, // event-hash ref; None for top-level
    attachments: Vec<AttachmentCapInput>,
) -> Result<EventAcceptedResult, String> {
    use farder_crypto::event_log::EventPayload;

    // Identity (must be unlocked) + device key.
    let identity = {
        let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
        let bytes = lock.ok_or_else(|| "identity is locked".to_string())?;
        Keypair::from_signing_key_bytes(&bytes)
    };
    let device = crate::device::load_or_create_device_keypair(identity.signing_key_bytes())?;

    // Serialize all chain writes: load → mutate → save must not interleave with
    // concurrent commands (join_log_server, create_invite) that touch the same
    // per-(server,device) state file.  tokio::sync::Mutex is held across awaits.
    let _chain_guard = state.device_chain_lock.lock().await;

    // Per-(server, device) chain state. Keyed by the log server_id (genesis hash).
    let mut ds = crate::device::DeviceState::load(&log_server_id)?
        .unwrap_or_else(|| crate::device::DeviceState::fresh(&device));

    // 1. First time on this server: authorize the device.
    if !ds.authorized {
        let cert = crate::device::device_cert(&identity, &device);
        let da = event_build_next(
            &device,
            &identity,
            &log_server_id,
            ds.last_event_hash.clone(),
            ds.next_seq,
            ds.lamport,
            EventPayload::DeviceAuthorized { cert },
        );
        event_send_submit(&state, &server_id, &da).await?;
        ds.next_seq = da.core.seq + 1;
        ds.last_event_hash = Some(da.hash());
        ds.lamport = da.core.lamport;
        ds.authorized = true;
        ds.save(&log_server_id)?;
    }

    // 2. Build + submit the message event, chaining from the stored head.
    // Map AttachmentCapInputs -> AttachmentCaps, stamping uploader = caller's identity.
    // NOTE: pure construction — no network I/O; correctness is verified in the inline
    // test in farder-crypto's event_log tests (see AttachmentCap round-trip test there).
    let caps: Vec<farder_crypto::event_log::AttachmentCap> = attachments
        .into_iter()
        .map(|a| farder_crypto::event_log::AttachmentCap {
            content_hash: a.content_hash,
            declared_type: a.declared_type,
            size: a.size,
            uploader: identity.public_key(),
        })
        .collect();
    let msg = event_build_next(
        &device,
        &identity,
        &log_server_id,
        ds.last_event_hash.clone(),
        ds.next_seq,
        ds.lamport,
        EventPayload::MessagePosted {
            channel_id,
            content,
            reply_to,
            attachments: caps,
        },
    );
    let result = event_send_submit(&state, &server_id, &msg).await?;

    // 3. Advance + persist chain state ONLY on confirmed acceptance.
    ds.next_seq = msg.core.seq + 1;
    ds.last_event_hash = Some(msg.hash());
    ds.lamport = msg.core.lamport;
    ds.save(&log_server_id)?;
    Ok(result)
}

/// Build the next chained event. We only store the prev hash (not the prev
/// Event), so we construct EventCore directly and sign it.
fn event_build_next(
    device: &Keypair,
    identity: &Keypair,
    server_id: &str,
    prev: Option<String>,
    seq: u64,
    lamport_observed: u64,
    payload: farder_crypto::event_log::EventPayload,
) -> farder_crypto::event_log::Event {
    use farder_crypto::event_log::{device_id, Event, EventCore};
    let core = EventCore {
        server_id: server_id.to_string(),
        author: identity.public_key(),
        device: device_id(&device.public_key()),
        seq,
        prev,
        lamport: lamport_observed + 1,
        timestamp: event_now_secs(),
        payload,
    };
    Event::sign(core, device)
}

fn event_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn event_send_submit(
    state: &AppState,
    server_id: &str,
    event: &farder_crypto::event_log::Event,
) -> Result<EventAcceptedResult, String> {
    let response = bridge::send_request(
        state,
        server_id,
        ServerRequest::SubmitEvent { event: event.clone() },
    )
    .await
    .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::EventAccepted { event_hash, timestamp } => {
            Ok(EventAcceptedResult { event_hash, timestamp })
        }
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// run_e2ee — the shared skeleton for the E2EE vertical (T7-T10)
//
// Every E2EE command (create / add-members / publish-key-package / steward)
// loads the identity + device keypair, then runs a `!Send` body: the vertical
// holds `&FarderMlsStore` across awaits, and `FarderMlsStore` is `Send` but NOT
// `Sync` (rusqlite's Connection holds a RefCell), so those futures are `!Send`
// and cannot run on a Tauri command's `Send` future. The body therefore runs on
// a dedicated current-thread runtime inside `spawn_blocking`, under the same
// `device_chain_lock` critical section as `submit_event`, after the device is
// authorized and the `ChainState` is built from `DeviceState`.
// ---------------------------------------------------------------------------

/// Run one E2EE body on the nested runtime, wired for identity + device +
/// device-chain lock. The body receives the already-authorized `DeviceState`
/// and the `ChainState` built from it; it owns both and must persist them
/// itself (the same load → mutate → save contract as the direct commands).
async fn run_e2ee<F, Fut, T>(
    state: &Arc<AppState>,
    server_id: String,
    log_server_id: String,
    body: F,
) -> Result<T, String>
where
    F: FnOnce(
            Arc<AppState>,
            String,
            String,
            Keypair,
            Keypair,
            crate::device::DeviceState,
            farder_e2ee_client::ChainState,
        ) -> Fut
        + Send
        + 'static,
    Fut: std::future::Future<Output = Result<T, String>>,
    T: Send + 'static,
{
    // Identity (must be unlocked) + device key. Both are Send, so they move
    // into the blocking task where the `Actor` borrows them across awaits.
    let identity = {
        let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
        let bytes = lock.ok_or_else(|| "identity is locked".to_string())?;
        Keypair::from_signing_key_bytes(&bytes)
    };
    let device = crate::device::load_or_create_device_keypair(identity.signing_key_bytes())?;

    let state_arc = Arc::clone(state);
    let result = tokio::task::spawn_blocking(move || -> Result<T, String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;

        rt.block_on(async move {
            // Serialize all chain writes: load → mutate → save must not
            // interleave with concurrent commands (submit_event, create_invite,
            // join_log_server) that touch the same per-(server, device) state
            // file. Held across awaits.
            let _chain_guard = state_arc.device_chain_lock.lock().await;

            // Per-(server, device) chain state. Keyed by the log server_id.
            let mut ds = crate::device::DeviceState::load(&log_server_id)?
                .unwrap_or_else(|| crate::device::DeviceState::fresh(&device));

            authorize_device_if_needed(
                &state_arc,
                &server_id,
                &log_server_id,
                &identity,
                &device,
                &mut ds,
            )
            .await?;

            let chain = farder_e2ee_client::ChainState {
                next_seq: ds.next_seq,
                last_event_hash: ds.last_event_hash.clone(),
                lamport: ds.lamport,
            };

            // The body gets its own clone: the chain guard still borrows
            // `state_arc` and is dropped at the end of this block.
            body(
                Arc::clone(&state_arc),
                server_id,
                log_server_id,
                identity,
                device,
                ds,
                chain,
            )
            .await
        })
    })
    .await
    .map_err(|e| e.to_string())??;

    Ok(result)
}

// ---------------------------------------------------------------------------
// create_e2ee_channel — owner creates an E2EE channel (LOG event, not
// CreateChannel): submit ChannelCreated { class: E2ee }, mint the group + store,
// publish the owner's KeyPackage, then bootstrap so the owner's leaf is
// confirmed. Persists both the device chain and the per-channel MLS state.
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct CreateE2eeChannelResult {
    pub channel_id: u64,
    pub class: String,
    pub event_hash: String,
}

#[tauri::command]
pub async fn create_e2ee_channel(
    state: State<'_, Arc<AppState>>,
    server_id: String,     // connection key (address) — routes the request
    log_server_id: String, // genesis hash — stamps EventCore.server_id + keys the device chain
    name: String,
) -> Result<CreateE2eeChannelResult, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("channel name cannot be empty".to_string());
    }

    run_e2ee(
        state.inner(),
        server_id,
        log_server_id,
        move |state, server_id, log_server_id, identity, device, ds, chain| async move {
            run_create_e2ee_channel(
                state, server_id, log_server_id, identity, device, name, ds, chain,
            )
            .await
        },
    )
    .await
}

/// The non-Send body of [`create_e2ee_channel`], driven on a current-thread
/// runtime by [`run_e2ee`] (which already holds the device-chain lock,
/// authorized the device, and built `chain` from `ds`).
async fn run_create_e2ee_channel(
    state: Arc<AppState>,
    server_id: String,
    log_server_id: String,
    identity: Keypair,
    device: Keypair,
    name: String,
    mut ds: crate::device::DeviceState,
    mut chain: farder_e2ee_client::ChainState,
) -> Result<CreateE2eeChannelResult, String> {
    use farder_crypto::event_log::E2EE_CHANNEL_ID_FLOOR;
    use farder_e2ee_client::{
        bootstrap_group, create_e2ee_channel, publish_key_package, Actor, ChainState, ChannelKey,
        ChannelSpec,
    };

    // channel_id is CLIENT-chosen and must clear the legacy DB AUTOINCREMENT
    // space (>= E2EE_CHANNEL_ID_FLOOR). The crate validates the floor; we pick
    // a fresh value at/above it (never a hand-rolled small id).
    let channel_id = E2EE_CHANNEL_ID_FLOOR + (rand::random::<u32>() as u64);
    let key = ChannelKey::new(log_server_id.clone(), channel_id)?;
    let spec = ChannelSpec {
        key: key.clone(),
        name,
        // This rung the server only accepts `kind == "text"` with no parent
        // (event_ingest::materialize_channel_created), so encrypted channels are
        // text-only and uncategorized regardless of the legacy type select.
        kind: "text".to_string(),
        parent: None,
    };

    let actor = Actor {
        device: &device,
        identity: &identity,
        log_server_id: &log_server_id,
    };
    let transport = crate::e2ee_transport::E2eeTransportImpl::new(&state, server_id.clone());
    let data_dir = farder_data_dir();

    // Advance + persist the device chain only on accepted events. Each of the
    // three crate calls submits exactly one event and advances `chain` only
    // after the server accepts it, so we sync + save after each (mirrors the
    // per-event save in submit_event).
    let sync_chain = |ds: &mut crate::device::DeviceState, chain: &ChainState| {
        ds.next_seq = chain.next_seq;
        ds.last_event_hash = chain.last_event_hash.clone();
        ds.lamport = chain.lamport;
    };

    // 2. Submit ChannelCreated { class: E2ee } + mint the group/store.
    let mut created = create_e2ee_channel(&transport, &actor, &mut chain, &spec, &data_dir)
        .await
        .map_err(|e| e.to_string())?;
    sync_chain(&mut ds, &chain);
    ds.save(&log_server_id)?;

    // 3. Publish this device's KeyPackage so members can add it (T8).
    let _published = publish_key_package(
        &transport,
        &actor,
        &mut chain,
        &created.store,
        &created.store_instance_hash,
    )
    .await
    .map_err(|e| e.to_string())?;
    sync_chain(&mut ds, &chain);
    ds.save(&log_server_id)?;

    // 4. Bootstrap the creator's own leaf (generation 0 / epoch 0 -> 1).
    let commit = bootstrap_group(
        &transport,
        &actor,
        &mut chain,
        &key,
        &mut created.group,
        &created.store,
        &created.store_instance_hash,
    )
    .await
    .map_err(|e| e.to_string())?;
    sync_chain(&mut ds, &chain);
    ds.save(&log_server_id)?;

    // 5. Persist the per-channel MLS state so T9/T10 can resume the group.
    crate::mls_state::MlsChannelState {
        generation: 0,
        epoch: commit.local_epoch,
        store_instance_hash: hex::encode(created.store_instance_hash),
        confirmed: true,
        cursor: 0,
        poisoned: None,
    }
    .save(&log_server_id, channel_id)?;

    // 6. Auto-add current server members (T8): every current member's device is
    //    added to the group so the fresh channel is usable without another
    //    click. Best-effort per member — a member with no (or a non-farder)
    //    key package is skipped with a log line, never failing the create; a
    //    stale-epoch divergence is terminal and surfaces as an error.
    add_current_members_to_group(
        &state,
        &server_id,
        &log_server_id,
        channel_id,
        &identity,
        &device,
        &mut ds,
        &mut chain,
        &key,
        0,
        &created.store,
        &created.store_instance_hash,
        &mut created.group,
    )
    .await?;

    Ok(CreateE2eeChannelResult {
        channel_id,
        class: "E2ee".to_string(),
        event_hash: created.event_hash,
    })
}

// ---------------------------------------------------------------------------
// add_members_to_e2ee_channel + publish_own_key_package (T8)
//
// Both run the same `spawn_blocking` + nested current-thread runtime bridge as
// `create_e2ee_channel` (T7): the E2EE vertical holds `&FarderMlsStore` across
// awaits, and `FarderMlsStore` is `Send` but NOT `Sync`, so those futures are
// `!Send` and cannot run on a Tauri command's `Send` future. The whole
// sequence therefore runs on a dedicated current-thread runtime inside
// `spawn_blocking`, under the same `device_chain_lock` critical section as
// `submit_event` / `create_e2ee_channel`.
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct AddMembersResult {
    pub added: Vec<AddedE2eeMember>,
    pub skipped: Vec<SkippedE2eeMember>,
}

#[derive(serde::Serialize)]
pub struct AddedE2eeMember {
    pub identity: String,
    pub device: String,
    pub epoch: u64,
}

#[derive(serde::Serialize)]
pub struct SkippedE2eeMember {
    pub identity: String,
    pub device: Option<String>,
    pub reason: String,
}

#[derive(serde::Serialize)]
pub struct PublishOwnKeyPackageResult {
    pub channel_id: u64,
    pub event_hash: String,
}

/// Authorize this device on the server (one `DeviceAuthorized` event) the first
/// time it acts — mirrors the head of `run_create_e2ee_channel` and
/// `submit_event`. Advances + persists `ds` only on acceptance.
async fn authorize_device_if_needed(
    state: &AppState,
    server_id: &str,
    log_server_id: &str,
    identity: &Keypair,
    device: &Keypair,
    ds: &mut crate::device::DeviceState,
) -> Result<(), String> {
    use farder_crypto::event_log::EventPayload;

    if ds.authorized {
        return Ok(());
    }
    let cert = crate::device::device_cert(identity, device);
    let da = event_build_next(
        device,
        identity,
        log_server_id,
        ds.last_event_hash.clone(),
        ds.next_seq,
        ds.lamport,
        EventPayload::DeviceAuthorized { cert },
    );
    event_send_submit(state, server_id, &da).await?;
    ds.next_seq = da.core.seq + 1;
    ds.last_event_hash = Some(da.hash());
    ds.lamport = da.core.lamport;
    ds.authorized = true;
    ds.save(log_server_id)?;
    Ok(())
}

/// Add every current server member's device to `group` (the steward side of the
/// membership bridge at channel-creation time). Reuses the crate's
/// `fetch_key_packages` / `decode_key_package` / `add_member` — never a
/// hand-rolled MLS path.
///
/// Resilience contract: a member (or one of its devices) with NO published
/// key package, or a key package that fails the farder-credential gate, is
/// SKIPPED with a clear `eprintln!` and a structured reason — it must not fail
/// the whole channel create. A `stale-epoch` divergence from `add_member` is
/// terminal (the local group is then one epoch ahead of the server) and aborts.
///
/// Advances + saves both the device chain and `mls_state.json` after each
/// accepted commit, so a later failure leaves an honest resume point.
async fn add_current_members_to_group(
    state: &AppState,
    server_id: &str,
    log_server_id: &str,
    channel_id: u64,
    identity: &Keypair,
    device: &Keypair,
    ds: &mut crate::device::DeviceState,
    chain: &mut farder_e2ee_client::ChainState,
    key: &farder_e2ee_client::ChannelKey,
    generation: u64,
    store: &farder_mls::store::FarderMlsStore,
    store_instance_hash: &[u8; 32],
    group: &mut farder_mls::group::MlsChannelGroup,
) -> Result<AddMembersResult, String> {
    use farder_crypto::event_log::{Event, EventPayload};
    use farder_e2ee_client::{add_member, Actor, E2eeTransport, StewardContext};
    use farder_mls::group::{decode_key_package, DeclaredMember};

    let members: Vec<MemberInfo> = {
        let response = bridge::send_request(state, server_id, ServerRequest::GetMembers)
            .await
            .map_err(|e| e.to_string())?;
        match response {
            ServerResponse::Members { members } => members,
            ServerResponse::Error { reason } => return Err(reason),
            other => return Err(format!("unexpected response to GetMembers: {:?}", other)),
        }
    };

    let actor = Actor {
        device,
        identity,
        log_server_id,
    };
    let transport = crate::e2ee_transport::E2eeTransportImpl::new(state, server_id.to_string());
    let ctx = StewardContext {
        key,
        generation,
        store,
        store_instance_hash,
    };

    let mut result = AddMembersResult {
        added: Vec::new(),
        skipped: Vec::new(),
    };

    for member in members {
        if member.public_key == identity.public_key() {
            continue; // the owner is already the creator + bootstrapped leaf
        }
        let member_identity = member.public_key.clone();
        let member_name = member.display_name.clone();

        // Enumerate this identity's LIVE devices (the roster carries only the
        // identity, not the device id — leaves are per (identity, device)).
        //
        // `member_live_leaves` is the crate's single definition of "live":
        // authorized, un-revoked (sub-5 S1 widened this fetch to carry
        // `DeviceRevoked`), and cert-unexpired — the same three facts the fold
        // checks in `declared add of a device that is not live (unknown,
        // revoked, or cert-expired)` (`event_log_state.rs:1114-1119`).
        //
        // Filtering HERE is load-bearing, not tidiness (finding F3). `add_member`
        // merges the add into the local group BEFORE it submits, and there is no
        // rollback, so authoring an add the fold will refuse leaves this
        // steward's group one epoch ahead of the server — diverged, and
        // recoverable only by re-provisioning. A revoked-but-still-authorized
        // device is the reachable case: `reprovision_device` revokes the old
        // device and publishes a KeyPackage for the fresh one, and that publish
        // is itself an auto-add trigger, so the old device's surviving
        // KeyPackages would be offered to `add_member` on the very next run.
        let device_ids: Vec<String> = {
            match farder_e2ee_client::member_live_leaves(&transport, &member_identity).await {
                Ok(leaves) => leaves.into_iter().map(|l| l.device).collect(),
                Err(e) => {
                    let reason = format!("fetch device certs: {e}");
                    eprintln!(
                        "E2EE add: skipping member {} ({member_identity}) — {reason}",
                        member_name
                    );
                    result.skipped.push(SkippedE2eeMember {
                        identity: member_identity.to_string(),
                        device: None,
                        reason,
                    });
                    continue;
                }
            }
        };

        if device_ids.is_empty() {
            let reason = "no authorized device".to_string();
            eprintln!(
                "E2EE add: skipping member {} ({member_identity}) — {reason}",
                member_name
            );
            result.skipped.push(SkippedE2eeMember {
                identity: member_identity.to_string(),
                device: None,
                reason,
            });
            continue;
        }

        for device_id in device_ids {
            // Idempotency guard (REQUIRED before any auto-add trigger): skip any
            // `(identity, device)` already present as a leaf in the group, so a
            // re-add never submits a redundant `MlsCommit` — which would advance
            // the epoch and could trip the server's commit-rate rule. We use
            // `leaves()` (the ACTUAL view: real credential + signature key), not
            // `members()` (the claimed view), matching the add loop's own
            // security posture. A non-farder leaf in the tree fails the whole
            // add (fail-closed) rather than being silently treated as absent.
            {
                let already_present = match group.leaves() {
                    Ok(leaves) => leaves
                        .iter()
                        .any(|l| l.member.identity == member_identity && l.member.device == device_id),
                    Err(e) => return Err(format!("read group leaves: {e}")),
                };
                if already_present {
                    eprintln!(
                        "E2EE add: skipping member {} ({member_identity}) device \
                         {device_id} — already a member",
                        member_name
                    );
                    result.skipped.push(SkippedE2eeMember {
                        identity: member_identity.to_string(),
                        device: Some(device_id.clone()),
                        reason: "already a member".to_string(),
                    });
                    continue;
                }
            }

            // 1. Fetch + decode the newest key package, failing closed on a
            //    non-farder credential BEFORE `add_member` merges anything. This
            //    is what lets a member with no package (or a bad one) be skipped
            //    precisely instead of aborted. `add_member` re-fetches +
            //    re-decodes internally — this is the sanctioned pre-gate, not a
            //    reimplementation of the add path.
            let key_package_bytes: Vec<u8> = {
                let events =
                    match transport.fetch_key_packages(&member_identity, &device_id).await {
                        Ok(events) => events,
                        Err(e) => {
                            let reason = format!("fetch key packages: {e}");
                            eprintln!(
                                "E2EE add: skipping member {} ({member_identity}) device \
                                 {device_id} — {reason}",
                                member_name
                            );
                            result.skipped.push(SkippedE2eeMember {
                                identity: member_identity.to_string(),
                                device: Some(device_id.clone()),
                                reason,
                            });
                            continue;
                        }
                    };
                let Some(last) = events.into_iter().last() else {
                    let reason = "no published key package".to_string();
                    eprintln!(
                        "E2EE add: skipping member {} ({member_identity}) device \
                         {device_id} — {reason}",
                        member_name
                    );
                    result.skipped.push(SkippedE2eeMember {
                        identity: member_identity.to_string(),
                        device: Some(device_id.clone()),
                        reason,
                    });
                    continue;
                };
                let kp_event = match Event::from_bytes(&last) {
                    Ok(event) => event,
                    Err(e) => {
                        let reason = format!("decode key package event: {e}");
                        eprintln!(
                            "E2EE add: skipping member {} ({member_identity}) device \
                             {device_id} — {reason}",
                            member_name
                        );
                        result.skipped.push(SkippedE2eeMember {
                            identity: member_identity.to_string(),
                            device: Some(device_id.clone()),
                            reason,
                        });
                        continue;
                    }
                };
                match &kp_event.core.payload {
                    EventPayload::MlsKeyPackagePublished { key_package, .. } => key_package.clone(),
                    _ => {
                        let reason =
                            "fetch_key_packages returned a non-key-package event".to_string();
                        eprintln!(
                            "E2EE add: skipping member {} ({member_identity}) device \
                             {device_id} — {reason}",
                            member_name
                        );
                        result.skipped.push(SkippedE2eeMember {
                            identity: member_identity.to_string(),
                            device: Some(device_id.clone()),
                            reason,
                        });
                        continue;
                    }
                }
            };

            if let Err(e) = decode_key_package(store, &key_package_bytes) {
                let reason = format!("key package fails the farder-credential gate: {e}");
                eprintln!(
                    "E2EE add: skipping member {} ({member_identity}) device {device_id} — \
                     {reason}",
                    member_name
                );
                result.skipped.push(SkippedE2eeMember {
                    identity: member_identity.to_string(),
                    device: Some(device_id.clone()),
                    reason,
                });
                continue;
            }

            // 2. The crate's full steward add: re-fetch, re-decode, add locally,
            //    then submit MlsCommit + MlsWelcome. Any error here is a skip
            //    EXCEPT a stale-epoch divergence (the local group is then one
            //    epoch ahead of the server — continuing would emit doomed commits).
            let declared = DeclaredMember {
                identity: member_identity.clone(),
                device: device_id.clone(),
            };
            match add_member(&transport, &actor, chain, &ctx, group, &declared).await {
                Ok(outcome) => {
                    ds.next_seq = chain.next_seq;
                    ds.last_event_hash = chain.last_event_hash.clone();
                    ds.lamport = chain.lamport;
                    ds.save(log_server_id)?;
                    crate::mls_state::MlsChannelState {
                        generation,
                        epoch: outcome.local_epoch,
                        store_instance_hash: hex::encode(store_instance_hash),
                        confirmed: true,
                        cursor: 0,
                        poisoned: None,
                    }
                    .save(log_server_id, channel_id)?;
                    result.added.push(AddedE2eeMember {
                        identity: member_identity.to_string(),
                        device: device_id.clone(),
                        epoch: outcome.local_epoch,
                    });
                }
                Err(e) if e.is_stale_epoch_diverged() => {
                    return Err(format!(
                        "add member {member_identity} / {device_id} diverged (stale-epoch): \
                         the local group is one epoch ahead of the server — aborting: {e}"
                    ));
                }
                Err(e) => {
                    let reason = format!("add member: {e}");
                    eprintln!(
                        "E2EE add: skipping member {} ({member_identity}) device {device_id} — \
                         {reason}",
                        member_name
                    );
                    result.skipped.push(SkippedE2eeMember {
                        identity: member_identity.to_string(),
                        device: Some(device_id.clone()),
                        reason,
                    });
                }
            }
        }
    }

    Ok(result)
}

/// Auto-add current server members to an existing E2EE channel's group.
/// Standalone entry for retry/T9/T10; `create_e2ee_channel` calls the same
/// [`add_current_members_to_group`] helper inline after bootstrap.
#[tauri::command]
pub async fn add_members_to_e2ee_channel(
    state: State<'_, Arc<AppState>>,
    server_id: String,     // connection key (address) — routes the request
    log_server_id: String, // genesis hash — keys the device chain
    channel_id: u64,
) -> Result<AddMembersResult, String> {
    run_e2ee(
        state.inner(),
        server_id,
        log_server_id,
        move |state, server_id, log_server_id, identity, device, ds, chain| async move {
            run_add_members_to_e2ee_channel(
                state, server_id, log_server_id, channel_id, identity, device, ds, chain,
            )
            .await
        },
    )
    .await
}

/// The non-Send body of [`add_members_to_e2ee_channel`]: resume the store +
/// group this device persisted at create time, then run the add loop.
async fn run_add_members_to_e2ee_channel(
    state: Arc<AppState>,
    server_id: String,
    log_server_id: String,
    channel_id: u64,
    identity: Keypair,
    device: Keypair,
    mut ds: crate::device::DeviceState,
    mut chain: farder_e2ee_client::ChainState,
) -> Result<AddMembersResult, String> {
    use farder_e2ee_client::{resume_store, ChannelKey};

    let key = ChannelKey::new(log_server_id.clone(), channel_id)?;
    let data_dir = farder_data_dir();

    // Resume the MLS store + group this device persisted at create/bootstrap
    // time. `resume_store` is terminal on a missing/mismatched instance hash.
    let (store, store_instance_hash) = resume_store(&data_dir, &key).map_err(|e| e.to_string())?;
    let generation = crate::mls_state::MlsChannelState::load(&log_server_id, channel_id)?
        .map(|s| s.generation)
        .unwrap_or(0);
    let mut group = farder_mls::group::MlsChannelGroup::load(
        &store,
        &farder_mls::credential::DeviceSigner(&device),
        farder_e2ee_client::channel_group_id(&log_server_id, channel_id, generation).as_bytes(),
    )
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "no MLS group for this channel".to_string())?;

    add_current_members_to_group(
        &state,
        &server_id,
        &log_server_id,
        channel_id,
        &identity,
        &device,
        &mut ds,
        &mut chain,
        &key,
        generation,
        &store,
        &store_instance_hash,
        &mut group,
    )
    .await
}

/// Publish THIS device's KeyPackage for an E2EE channel so a steward can add it
/// to the group. A member calls this when it first opens an E2EE channel it is
/// not yet a confirmed member of (T9/T10 wire the trigger). Creates (or resumes)
/// the joiner store, publishes, and records `mls_state.json` (`confirmed: false`).
#[tauri::command]
pub async fn publish_own_key_package(
    state: State<'_, Arc<AppState>>,
    server_id: String,     // connection key (address) — routes the request
    log_server_id: String, // genesis hash — keys the device chain
    channel_id: u64,
) -> Result<PublishOwnKeyPackageResult, String> {
    run_e2ee(
        state.inner(),
        server_id,
        log_server_id,
        move |state, server_id, log_server_id, identity, device, ds, chain| async move {
            run_publish_own_key_package(
                state, server_id, log_server_id, channel_id, identity, device, ds, chain,
            )
            .await
        },
    )
    .await
}

/// The non-Send body of [`publish_own_key_package`]: create-once or resume the
/// joiner store, publish the KeyPackage, and record the resume point.
async fn run_publish_own_key_package(
    state: Arc<AppState>,
    server_id: String,
    log_server_id: String,
    channel_id: u64,
    identity: Keypair,
    device: Keypair,
    mut ds: crate::device::DeviceState,
    mut chain: farder_e2ee_client::ChainState,
) -> Result<PublishOwnKeyPackageResult, String> {
    use farder_e2ee_client::{
        create_joiner_store, publish_key_package, resume_store, Actor, ChannelKey,
    };

    let key = ChannelKey::new(log_server_id.clone(), channel_id)?;
    let data_dir = farder_data_dir();

    // Create-once or resume the joiner store: the KeyPackage's private material
    // must live in the same store that later joins from the Welcome (the store
    // lifecycle contract in `join.rs`).
    let store_path = key.mls_store_path(&data_dir).map_err(|e| e.to_string())?;
    let (store, store_instance_hash) = if store_path.exists() {
        resume_store(&data_dir, &key).map_err(|e| e.to_string())?
    } else {
        create_joiner_store(&data_dir, &key).map_err(|e| e.to_string())?
    };

    let actor = Actor {
        device: &device,
        identity: &identity,
        log_server_id: &log_server_id,
    };
    let transport = crate::e2ee_transport::E2eeTransportImpl::new(&state, server_id.clone());

    let published = publish_key_package(&transport, &actor, &mut chain, &store, &store_instance_hash)
        .await
        .map_err(|e| e.to_string())?;

    ds.next_seq = chain.next_seq;
    ds.last_event_hash = chain.last_event_hash.clone();
    ds.lamport = chain.lamport;
    ds.save(&log_server_id)?;

    // Record the resume point: not yet joined/confirmed (T9/T10 complete the join).
    crate::mls_state::MlsChannelState {
        generation: 0,
        epoch: 0,
        store_instance_hash: hex::encode(store_instance_hash),
        confirmed: false,
        cursor: 0,
        poisoned: None,
    }
    .save(&log_server_id, channel_id)?;

    Ok(PublishOwnKeyPackageResult {
        channel_id,
        event_hash: published.event_hash,
    })
}

// ---------------------------------------------------------------------------
// process_mls_control_events — the steward (T9): advance a member's MLS group
// when someone else commits, and join + confirm our own leaf from a Welcome.
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct ProcessMlsControlEventsResult {
    pub channel_id: u64,
    /// `"advanced"` (the group advanced, possibly zero steps) or
    /// `"equivocation"` (F4: the group is POISONED by an impostor leaf and must
    /// never be used further — the frontend maps this to the "channel state
    /// could not be confirmed" terminal state).
    pub outcome: String,
    /// Set when `outcome == "equivocation"`; the impostor-leaf detail.
    pub reason: Option<String>,
    /// How many accepted steps (applied commits / joined welcomes) ran.
    pub processed: usize,
    pub epoch: u64,
    pub generation: u64,
    pub confirmed: bool,
    pub cursor: u64,
}

/// Advance one E2EE channel's MLS group to the server's current control-plane
/// state: resume the store + group, fetch outstanding
/// `MlsCommit`/`MlsWelcome`/`MlsLeafConfirmed`/`MlsGroupReset` events from the
/// recorded cursor, apply each commit in order through the crate's two
/// receive-side gates, and confirm our own leaf when a Welcome is aimed at us.
#[tauri::command]
pub async fn process_mls_control_events(
    state: State<'_, Arc<AppState>>,
    server_id: String,     // connection key (address) — routes the request
    log_server_id: String, // genesis hash — keys the device chain
    channel_id: u64,
) -> Result<ProcessMlsControlEventsResult, String> {
    run_e2ee(
        state.inner(),
        server_id,
        log_server_id,
        move |state, server_id, log_server_id, identity, device, ds, chain| async move {
            run_process_mls_control_events(
                state,
                server_id,
                log_server_id,
                channel_id,
                identity,
                device,
                ds,
                chain,
            )
            .await
        },
    )
    .await
}

/// The non-Send body of [`process_mls_control_events`], driven on a
/// current-thread runtime by [`run_e2ee`].
async fn run_process_mls_control_events(
    state: Arc<AppState>,
    server_id: String,
    log_server_id: String,
    channel_id: u64,
    identity: Keypair,
    device: Keypair,
    mut ds: crate::device::DeviceState,
    mut chain: farder_e2ee_client::ChainState,
) -> Result<ProcessMlsControlEventsResult, String> {
    use farder_crypto::event_log::{device_id, EventPayload};
    use farder_e2ee_client::{
        build_cert_resolver, channel_group_id, confirm_leaf, fetch_mls_control_exhaustive,
        join_channel, process_incoming_commit, resume_store, Actor, ChannelKey, DeclaredCommit,
        IncomingCommitOutcome, PendingWelcome,
    };
    use farder_mls::group::{DeclaredMember, MlsChannelGroup};

    let key = ChannelKey::new(log_server_id.clone(), channel_id)?;
    let data_dir = farder_data_dir();

    // No store => this device has never published a KeyPackage for the channel
    // (the joiner store is created on first open, T8), so there is nothing to
    // steward yet. Report an honest no-op advance rather than erroring: the
    // steward fires on every control event, including for channels we have not
    // opened.
    let store_path = key.mls_store_path(&data_dir).map_err(|e| e.to_string())?;
    if !store_path.exists() {
        return Ok(ProcessMlsControlEventsResult {
            channel_id,
            outcome: "advanced".to_string(),
            reason: None,
            processed: 0,
            epoch: 0,
            generation: 0,
            confirmed: false,
            cursor: 0,
        });
    }

    let (store, store_instance_hash) = resume_store(&data_dir, &key).map_err(|e| e.to_string())?;

    let mut mls = crate::mls_state::MlsChannelState::load(&log_server_id, channel_id)?
        .unwrap_or(crate::mls_state::MlsChannelState {
            generation: 0,
            epoch: 0,
            store_instance_hash: hex::encode(store_instance_hash),
            confirmed: false,
            cursor: 0,
            poisoned: None,
        });

    // Terminal from a previous run (F4): the impostor leaf is already merged
    // and cannot be rolled back. Refuse to continue, ever.
    if let Some(reason) = mls.poisoned.clone() {
        return Ok(ProcessMlsControlEventsResult {
            channel_id,
            outcome: "equivocation".to_string(),
            reason: Some(reason),
            processed: 0,
            epoch: mls.epoch,
            generation: mls.generation,
            confirmed: mls.confirmed,
            cursor: mls.cursor,
        });
    }

    let generation = mls.generation;
    let cursor = mls.cursor;

    // `None` until we have joined from our own Welcome (a member that published
    // its KeyPackage but has not yet been added).
    let mut group = MlsChannelGroup::load(
        &store,
        &farder_mls::credential::DeviceSigner(&device),
        channel_group_id(&log_server_id, channel_id, generation).as_bytes(),
    )
    .map_err(|e| e.to_string())?;

    let actor = Actor {
        device: &device,
        identity: &identity,
        log_server_id: &log_server_id,
    };
    let transport = crate::e2ee_transport::E2eeTransportImpl::new(&state, server_id.clone());

    let (events, next_cursor) = fetch_mls_control_exhaustive(&transport, channel_id, cursor)
        .await
        .map_err(|e| e.to_string())?;

    let my_identity = identity.public_key();
    let my_device = device_id(&device.public_key());

    let mut processed = 0usize;

    for event in &events {
        match &event.core.payload {
            EventPayload::MlsCommit {
                epoch,
                mls_message,
                adds,
                removes,
                post_tree_hash,
                ..
            } => {
                // Before we have joined from our own Welcome, every commit in
                // the control plane predates our membership (the Welcome carries
                // the post-add tree, so we join at its epoch directly). Skip
                // them — once joined, later commits are applied below.
                let Some(g) = group.as_mut() else { continue };

                let declared = DeclaredCommit {
                    epoch: *epoch,
                    adds: adds.clone(),
                    removes: removes.clone(),
                    post_tree_hash: *post_tree_hash,
                };
                // Gate 2's trust anchor is built over the commit's declared
                // adds, fetched from the log (never from the commit itself).
                let members: Vec<DeclaredMember> = declared
                    .adds
                    .iter()
                    .map(|a| DeclaredMember {
                        identity: a.identity.clone(),
                        device: a.device.clone(),
                    })
                    .collect();
                let certs = build_cert_resolver(&transport, &members)
                    .await
                    .map_err(|e| e.to_string())?;

                match process_incoming_commit(&store, g, mls_message, &declared, &certs)
                    .map_err(|e| e.to_string())?
                {
                    IncomingCommitOutcome::Applied { epoch, .. } => {
                        mls.epoch = epoch;
                        processed += 1;
                        // Persist after each accepted step so a later failure
                        // leaves an honest resume point (the cursor is only
                        // advanced at the end — a gap is retried, never skipped).
                        mls.save(&log_server_id, channel_id)?;
                    }
                    IncomingCommitOutcome::OutOfOrder { current_epoch, received_epoch }
                        if received_epoch < current_epoch =>
                    {
                        // Replay of a commit we already applied (e.g. after a
                        // crash before the cursor persisted): skip it.
                    }
                    IncomingCommitOutcome::OutOfOrder { received_epoch, .. } => {
                        return Err(format!(
                            "commit at epoch {received_epoch} arrived out of order — a gap \
                             blocks advancing the group"
                        ));
                    }
                    IncomingCommitOutcome::LeafBindingFailure { member, reason } => {
                        // F4 (terminal): Gate 1 already merged the impostor
                        // leaf and farder-mls offers no rollback. Persist the
                        // poisoned flag and stop — never retry, never continue.
                        let reason = format!(
                            "impostor leaf for {} / {}: {reason}",
                            member.identity, member.device
                        );
                        mls.poisoned = Some(reason.clone());
                        mls.cursor = next_cursor;
                        mls.save(&log_server_id, channel_id)?;
                        return Ok(ProcessMlsControlEventsResult {
                            channel_id,
                            outcome: "equivocation".to_string(),
                            reason: Some(reason),
                            processed,
                            epoch: mls.epoch,
                            generation: mls.generation,
                            confirmed: mls.confirmed,
                            cursor: next_cursor,
                        });
                    }
                }
            }
            EventPayload::MlsWelcome {
                channel_id: wc,
                generation: wgen,
                for_member,
                for_device,
                welcome,
                ..
            } => {
                // Only a Welcome addressed to THIS device advances us; others'
                // Welcomes are skipped (they are for the steward/other members).
                if for_member != &my_identity || for_device.as_str() != my_device {
                    continue;
                }
                if group.is_none() {
                    let (g, join_info) =
                        join_channel(&store, welcome).map_err(|e| e.to_string())?;
                    group = Some(g);

                    let pending = PendingWelcome {
                        channel_id: *wc,
                        generation: *wgen,
                        welcome: welcome.clone(),
                    };
                    let confirmation = confirm_leaf(
                        &transport,
                        &actor,
                        &mut chain,
                        &key,
                        &store_instance_hash,
                        &pending,
                        &join_info,
                    )
                    .await
                    .map_err(|e| e.to_string())?;

                    // `confirm_leaf` advanced the device chain; sync + persist
                    // the same way the other E2EE commands do.
                    ds.next_seq = chain.next_seq;
                    ds.last_event_hash = chain.last_event_hash.clone();
                    ds.lamport = chain.lamport;
                    ds.save(&log_server_id)?;

                    mls.epoch = confirmation.epoch;
                    mls.generation = *wgen;
                    mls.confirmed = true;
                    processed += 1;
                    mls.save(&log_server_id, channel_id)?;
                }
                // Already joined (replay / idempotent): nothing to do.
            }
            // MlsLeafConfirmed / MlsGroupReset / anything else: not needed to
            // advance the group. MlsGroupReset (generation bump) is out of T9's
            // scope — it arrives here and is skipped, not mis-handled.
            _ => {}
        }
    }

    // Advance the fetch cursor past everything we just consumed, so the next
    // run starts after this page (idempotent: an already-applied commit is a
    // replay, an un-joined member skips pre-membership commits).
    mls.cursor = next_cursor;
    mls.save(&log_server_id, channel_id)?;

    Ok(ProcessMlsControlEventsResult {
        channel_id,
        outcome: "advanced".to_string(),
        reason: None,
        processed,
        epoch: mls.epoch,
        generation: mls.generation,
        confirmed: mls.confirmed,
        cursor: next_cursor,
    })
}

// ---------------------------------------------------------------------------
// send_sealed_message — seal + submit one E2EE channel message (T10)
//
// Wraps the crate's proven `send_sealed` path (build MessageEnvelope with empty
// attachment vecs, enforce the client-side caps, seal, submit
// `MessagePostedE2ee`); on a bare `stale-epoch` rejection it runs the crate's
// bounded `send_sealed_resync` loop. Both call the crate, never a hand-rolled
// MLS path. Runs under `run_e2ee` (spawn_blocking + nested runtime) because the
// vertical holds `&FarderMlsStore` across awaits.
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct SendSealedMessageResult {
    /// Server-assigned hash of the accepted `MessagePostedE2ee` event.
    pub event_hash: String,
    /// The epoch the ciphertext was sealed in (the group's current epoch at
    /// seal time; sealing never advances it).
    pub epoch: u64,
}

#[tauri::command]
pub async fn send_sealed_message(
    state: State<'_, Arc<AppState>>,
    server_id: String,     // connection key (address) — routes the request
    log_server_id: String, // genesis hash — stamps the event + keys the device chain
    channel_id: u64,
    content: String,
    reply_to: Option<String>, // event-hash ref; None for top-level (legacy numeric replies are not mapped yet)
) -> Result<SendSealedMessageResult, String> {
    let content = content.trim().to_string();
    if content.is_empty() {
        return Err("message content cannot be empty".to_string());
    }

    run_e2ee(
        state.inner(),
        server_id,
        log_server_id,
        move |state, server_id, log_server_id, identity, device, ds, chain| async move {
            run_send_sealed_message(
                state,
                server_id,
                log_server_id,
                channel_id,
                identity,
                device,
                ds,
                chain,
                content,
                reply_to,
            )
            .await
        },
    )
    .await
}

/// The non-Send body of [`send_sealed_message`], driven on a current-thread
/// runtime by [`run_e2ee`] (which already holds the device-chain lock and
/// authorized the device). `send_sealed` advances `chain` only after the server
/// accepts the event, so the chain is synced + persisted once on success.
async fn run_send_sealed_message(
    state: Arc<AppState>,
    server_id: String,
    log_server_id: String,
    channel_id: u64,
    identity: Keypair,
    device: Keypair,
    mut ds: crate::device::DeviceState,
    mut chain: farder_e2ee_client::ChainState,
    content: String,
    reply_to: Option<String>,
) -> Result<SendSealedMessageResult, String> {
    use farder_e2ee_client::{
        channel_group_id, resume_store, send_sealed, send_sealed_resync, Actor, ChannelKey,
        ResyncRequest, SealContext, SendEligibility,
    };
    use farder_mls::credential::DeviceSigner;
    use farder_mls::group::MlsChannelGroup;

    let key = ChannelKey::new(log_server_id.clone(), channel_id)?;
    let data_dir = farder_data_dir();

    let (store, store_instance_hash) = resume_store(&data_dir, &key).map_err(|e| e.to_string())?;
    let mut mls = crate::mls_state::MlsChannelState::load(&log_server_id, channel_id)?
        .unwrap_or(crate::mls_state::MlsChannelState {
            generation: 0,
            epoch: 0,
            store_instance_hash: hex::encode(store_instance_hash),
            confirmed: false,
            cursor: 0,
            poisoned: None,
        });

    // F4 (terminal): the group is poisoned by an impostor leaf. Never send into
    // it; surface the equivocation state rather than continuing.
    if let Some(reason) = mls.poisoned.clone() {
        return Err(format!(
            "channel state could not be confirmed (poisoned): {reason}"
        ));
    }

    let generation = mls.generation;
    let mut group = MlsChannelGroup::load(
        &store,
        &DeviceSigner(&device),
        channel_group_id(&log_server_id, channel_id, generation).as_bytes(),
    )
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "no MLS group for this channel".to_string())?;

    let actor = Actor {
        device: &device,
        identity: &identity,
        log_server_id: &log_server_id,
    };
    let transport = crate::e2ee_transport::E2eeTransportImpl::new(&state, server_id.clone());

    let ctx = SealContext {
        key: &key,
        generation,
        store: &store,
        content: &content,
        reply_to: reply_to.clone(),
    };
    let eligibility = if mls.confirmed {
        SendEligibility::confirmed()
    } else {
        SendEligibility::not_confirmed()
    };

    // Keep-alive (sub-5b K1): the fold seals a channel's sealed content once the
    // freshness ceiling is reached, and 5a's answer (`rekey_channel`) shipped
    // dormant with no caller — so an ordinary channel stopped accepting messages
    // after ~500 of them, permanently. `send_sealed_keepalive` rekeys once and
    // retries; every other rejection, including the divergence class, surfaces
    // unchanged. Pinned end-to-end by the harness's
    // `the_freshness_ceiling_does_not_brick_a_channel`.
    let rekey_ctx = farder_e2ee_client::RekeyContext {
        key: &key,
        generation,
        store: &store,
        store_instance_hash: &store_instance_hash,
    };
    let drift_ctx = farder_e2ee_client::DriftDischargeContext {
        key: &key,
        generation,
        store: &store,
        store_instance_hash: &store_instance_hash,
    };

    // First attempt with an EMPTY dead-leaf set: deriving it costs a roster
    // fetch plus a cert fetch per member, far too much to spend on every
    // message. A drift seal is rare, so we pay for the answer only when the
    // channel actually reports one.
    let first = farder_e2ee_client::send_sealed_keepalive(
        &transport,
        &actor,
        &mut chain,
        &ctx,
        &farder_e2ee_client::KeepaliveRepairs {
            rekey: &rekey_ctx,
            drift: &drift_ctx,
            dead: &[],
        },
        &mut group,
        &eligibility,
    )
    .await;

    let first = match first {
        Err(ref e) if e.is_sealed_pending_removals() => {
            // Now it is worth the round trips: derive whom the fold no longer
            // owes a leaf, discharge, and retry (sub-5b K3).
            let live =
                live_channel_leaves(&transport, &state, &server_id, &identity, &device).await?;
            let dead = farder_e2ee_client::dead_leaves(&group, &live).map_err(|e| e.to_string())?;
            farder_e2ee_client::send_sealed_keepalive(
                &transport,
                &actor,
                &mut chain,
                &ctx,
                &farder_e2ee_client::KeepaliveRepairs {
                    rekey: &rekey_ctx,
                    drift: &drift_ctx,
                    dead: &dead,
                },
                &mut group,
                &eligibility,
            )
            .await
        }
        other => other,
    };

    let outcome = match first {
        Ok(k) => k.send,
        // F6: the bare "stale-epoch" rejection also fires for
        // MessagePostedE2ee. Resync is bounded (unproductive + total caps) and
        // terminates under every transport behaviour; it never spins.
        Err(farder_e2ee_client::E2eeError::Transport(e)) if e.is_stale_epoch() => {
            let request = ResyncRequest {
                eligibility: &eligibility,
                since_accept_seq: mls.cursor,
            };
            let certs =
                build_roster_cert_resolver(&transport, &state, &server_id, &identity).await?;
            let resynced = match send_sealed_resync(
                &transport,
                &actor,
                &mut chain,
                &ctx,
                &mut group,
                &request,
                &certs,
            )
            .await
            {
                Ok(resynced) => resynced,
                // F4 (terminal): an impostor leaf is already merged and cannot
                // be rolled back. Persist the poisoned flag so a later send or
                // decrypt refuses this group, then surface the equivocation.
                Err(farder_e2ee_client::E2eeError::ResyncPoisoned { member, reason }) => {
                    let reason = format!(
                        "impostor leaf for {} / {}: {reason}",
                        member.identity, member.device
                    );
                    mls.poisoned = Some(reason.clone());
                    mls.save(&log_server_id, channel_id)?;
                    return Err(format!(
                        "channel state could not be confirmed (poisoned): {reason}"
                    ));
                }
                Err(e) => return Err(e.to_string()),
            };
            // Persist the advanced control-plane cursor + the group's new epoch
            // so the next resync/steward starts where this one left off.
            mls.cursor = resynced.next_accept_seq;
            mls.epoch = group.epoch();
            mls.save(&log_server_id, channel_id)?;
            resynced.send
        }
        Err(e) => return Err(e.to_string()),
    };

    // Advance + persist the device chain on acceptance (mirrors submit_event).
    ds.next_seq = chain.next_seq;
    ds.last_event_hash = chain.last_event_hash.clone();
    ds.lamport = chain.lamport;
    ds.save(&log_server_id)?;

    Ok(SendSealedMessageResult {
        event_hash: outcome.event_hash,
        epoch: outcome.epoch,
    })
}

/// Build the Gate-2 trust anchor for the bounded resync loop: a
/// [`farder_e2ee_client::VerifiedCertResolver`] over every current server
/// member's authorized devices. A `stale-epoch` winning commit may add a
/// member, and `send_sealed_resync` resolves each fetched commit's declared
/// `adds` against this resolver — so it must cover the roster, fetched from the
/// log via the crate's own `resolve_device_cert` (never synthesized from the
/// commit). Reuses the same device-enumeration shape as
/// `add_current_members_to_group`.
/// The client's view of which `(identity, device)` leaves are still ENTITLED to
/// be in an E2EE channel's group: every current roster member crossed with their
/// live (authorized, un-revoked, un-expired) devices, plus our own leaf.
///
/// Subtracting this from the group's ACTUAL leaf view yields the fold's
/// `pending_removals` — the drift that seals the channel (sub-5b K3). A banned
/// member vanishes from the roster; a revoked or expired device vanishes from
/// `member_live_leaves`. Deliberately computed ONLY when a send comes back
/// sealed: it costs a roster fetch plus one cert fetch per member, which is far
/// too much to spend on every message.
async fn live_channel_leaves(
    transport: &crate::e2ee_transport::E2eeTransportImpl<'_>,
    state: &AppState,
    server_id: &str,
    identity: &Keypair,
    own_device: &Keypair,
) -> Result<Vec<farder_mls::group::DeclaredMember>, String> {
    use farder_crypto::event_log::device_id;
    use farder_mls::group::DeclaredMember;

    let members: Vec<MemberInfo> = {
        let response = bridge::send_request(state, server_id, ServerRequest::GetMembers)
            .await
            .map_err(|e| e.to_string())?;
        match response {
            ServerResponse::Members { members } => members,
            ServerResponse::Error { reason } => return Err(reason),
            other => return Err(format!("unexpected response to GetMembers: {other:?}")),
        }
    };

    let mut live: Vec<DeclaredMember> = vec![DeclaredMember {
        identity: identity.public_key(),
        device: device_id(&own_device.public_key()),
    }];
    for member in members {
        if member.public_key == identity.public_key() {
            continue;
        }
        let leaves = farder_e2ee_client::member_live_leaves(transport, &member.public_key)
            .await
            .map_err(|e| e.to_string())?;
        live.extend(leaves);
    }
    Ok(live)
}

async fn build_roster_cert_resolver(
    transport: &crate::e2ee_transport::E2eeTransportImpl<'_>,
    state: &AppState,
    server_id: &str,
    identity: &Keypair,
) -> Result<farder_e2ee_client::VerifiedCertResolver, String> {
    use farder_crypto::event_log::Event;
    use farder_e2ee_client::{build_cert_resolver, E2eeTransport};
    use farder_mls::group::DeclaredMember;

    let members: Vec<MemberInfo> = {
        let response = bridge::send_request(state, server_id, ServerRequest::GetMembers)
            .await
            .map_err(|e| e.to_string())?;
        match response {
            ServerResponse::Members { members } => members,
            ServerResponse::Error { reason } => return Err(reason),
            other => return Err(format!("unexpected response to GetMembers: {other:?}")),
        }
    };

    let mut declared: Vec<DeclaredMember> = Vec::new();
    for member in members {
        // Our own leaf is already in the group; Gate 2 only resolves a commit's
        // declared ADDS (other members), so self is irrelevant here.
        if member.public_key == identity.public_key() {
            continue;
        }
        let events = transport
            .fetch_device_certs(&member.public_key)
            .await
            .map_err(|e| e.to_string())?;
        let mut ids: Vec<String> = events
            .iter()
            .filter_map(|bytes| Event::from_bytes(bytes).ok().map(|ev| ev.core.device))
            .collect();
        ids.sort();
        ids.dedup();
        for device_id in ids {
            declared.push(DeclaredMember {
                identity: member.public_key.clone(),
                device: device_id,
            });
        }
    }
    build_cert_resolver(transport, &declared)
        .await
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// decrypt_sealed_message — open ONE ciphertext (T10)
//
// The ratchet is consumed on open (4a): this command is the ONLY place that
// opens a given ciphertext, and it calls `receive_sealed` exactly once. The
// ciphertext is taken by value and moved into `receive_sealed`; the returned
// `SealedOutcome` carries the bytes in neither variant, so a re-open is
// structurally impossible. The frontend must call this once per sealed message
// and cache the result (see useSealedDecrypt.ts).
//
// This is deliberately NOT `run_e2ee`: decryption is a local, read-only open
// that needs only the device subkey + on-disk store (not the unlocked identity,
// and not device authorization). It holds `device_chain_lock` so the
// synchronous store open cannot race a concurrent send/steward write to the
// same sqlite store.
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecryptSealedMessageResult {
    /// The ciphertext opened and decoded to a valid [`MessageEnvelope`].
    Decrypted {
        envelope: farder_mls::envelope::MessageEnvelope,
    },
    /// The ciphertext could not be opened — tampered, wrong group/epoch, no
    /// local group yet, or a poisoned group. The generation's decryption key is
    /// already consumed; a retry on the same bytes is impossible and pointless.
    Undecryptable { reason: String },
}

#[tauri::command]
pub async fn decrypt_sealed_message(
    state: State<'_, Arc<AppState>>,
    server_id: String,     // connection key (address) — only used for diagnostics (the store is keyed by log_server_id)
    log_server_id: String, // genesis hash — keys the per-channel MLS store
    channel_id: u64,
    ciphertext: Vec<u8>,
) -> Result<DecryptSealedMessageResult, String> {
    // The device subkey is now WRAPPED under the identity (sub-7a H1), so an
    // open needs the identity unlocked. That is a deliberate narrowing of this
    // command's old contract ("needs only the device subkey + on-disk store"):
    // reading sealed history while the app is locked was a hole in the very
    // property the PIN exists to provide. It stays fail-closed rather than an
    // Err, matching how this command reports every other local failure.
    let identity_seed = {
        let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
        match *lock {
            Some(bytes) => bytes,
            None => {
                return Ok(DecryptSealedMessageResult::Undecryptable {
                    reason: "identity is locked".to_string(),
                })
            }
        }
    };
    let device = crate::device::load_or_create_device_keypair(&identity_seed)?;
    let data_dir = farder_data_dir();
    let key = farder_e2ee_client::ChannelKey::new(log_server_id.clone(), channel_id)?;

    // Serialize with the other E2EE commands that write the same sqlite store
    // (send/steward) so a concurrent `open_message` never hits SQLITE_BUSY.
    let _store_guard = state.device_chain_lock.lock().await;

    // A missing store (never published a KeyPackage / not yet added) is the
    // honest "cannot decrypt yet" case, not a command failure — surfaced as
    // Undecryptable so the frontend renders a fail-closed state (T11 refines
    // the channel-level "waiting for keys" interstitial).
    let (store, store_instance_hash) = match farder_e2ee_client::resume_store(&data_dir, &key) {
        Ok(store) => store,
        Err(e) => {
            return Ok(DecryptSealedMessageResult::Undecryptable {
                reason: format!("no usable MLS store for channel {channel_id} on {server_id}: {e}"),
            });
        }
    };

    let mls = crate::mls_state::MlsChannelState::load(&log_server_id, channel_id)?
        .unwrap_or(crate::mls_state::MlsChannelState {
            generation: 0,
            epoch: 0,
            store_instance_hash: hex::encode(store_instance_hash),
            confirmed: false,
            cursor: 0,
            poisoned: None,
        });

    if let Some(reason) = mls.poisoned.clone() {
        return Ok(DecryptSealedMessageResult::Undecryptable {
            reason: format!("channel state could not be confirmed (poisoned): {reason}"),
        });
    }

    let generation = mls.generation;
    let mut group = match farder_mls::group::MlsChannelGroup::load(
        &store,
        &farder_mls::credential::DeviceSigner(&device),
        farder_e2ee_client::channel_group_id(&log_server_id, channel_id, generation).as_bytes(),
    ) {
        Ok(Some(group)) => group,
        Ok(None) => {
            return Ok(DecryptSealedMessageResult::Undecryptable {
                reason: format!("no MLS group for channel {channel_id} on {server_id}"),
            });
        }
        Err(e) => return Err(e.to_string()),
    };

    // The ratchet is consumed on open: `receive_sealed` takes the ciphertext BY
    // VALUE and is invoked exactly once here — the bytes are moved in and never
    // referenced again, so there is no path that re-opens them.
    match farder_e2ee_client::receive_sealed(&store, &mut group, ciphertext) {
        farder_e2ee_client::SealedOutcome::Decrypted(envelope) => {
            Ok(DecryptSealedMessageResult::Decrypted { envelope })
        }
        farder_e2ee_client::SealedOutcome::Undecryptable { reason } => {
            Ok(DecryptSealedMessageResult::Undecryptable { reason })
        }
    }
}

// ---------------------------------------------------------------------------
// redact_attachment — moderator redacts an attachment from the log
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn redact_attachment(
    state: State<'_, Arc<AppState>>,
    server_id: String,     // connection key (address) — routes the request
    log_server_id: String, // genesis hash — stamps EventCore.server_id + keys the device chain
    content_hash: String,
) -> Result<EventAcceptedResult, String> {
    use farder_crypto::event_log::EventPayload;

    // Identity (must be unlocked) + device key.
    let identity = {
        let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
        let bytes = lock.ok_or_else(|| "identity is locked".to_string())?;
        Keypair::from_signing_key_bytes(&bytes)
    };
    let device = crate::device::load_or_create_device_keypair(identity.signing_key_bytes())?;

    // Serialize all chain writes: load → mutate → save must not interleave with
    // concurrent commands (join_log_server, submit_event) that touch the same
    // per-(server,device) state file.  tokio::sync::Mutex is held across awaits.
    let _chain_guard = state.device_chain_lock.lock().await;

    // Per-(server, device) chain state. Keyed by the log server_id (genesis hash).
    let mut ds = crate::device::DeviceState::load(&log_server_id)?
        .unwrap_or_else(|| crate::device::DeviceState::fresh(&device));

    // 1. First time on this server: authorize the device.
    if !ds.authorized {
        let cert = crate::device::device_cert(&identity, &device);
        let da = event_build_next(
            &device,
            &identity,
            &log_server_id,
            ds.last_event_hash.clone(),
            ds.next_seq,
            ds.lamport,
            EventPayload::DeviceAuthorized { cert },
        );
        event_send_submit(&state, &server_id, &da).await?;
        ds.next_seq = da.core.seq + 1;
        ds.last_event_hash = Some(da.hash());
        ds.lamport = da.core.lamport;
        ds.authorized = true;
        ds.save(&log_server_id)?;
    }

    // 2. Build + submit the AttachmentRedacted event, chaining from the stored head.
    let redact = event_build_next(
        &device,
        &identity,
        &log_server_id,
        ds.last_event_hash.clone(),
        ds.next_seq,
        ds.lamport,
        EventPayload::AttachmentRedacted { content_hash },
    );
    let result = event_send_submit(&state, &server_id, &redact).await?;

    // 3. Advance + persist chain state ONLY on confirmed acceptance.
    ds.next_seq = redact.core.seq + 1;
    ds.last_event_hash = Some(redact.hash());
    ds.lamport = redact.core.lamport;
    ds.save(&log_server_id)?;
    Ok(result)
}

// ---------------------------------------------------------------------------
// join_log_server — joiner emits MemberJoined so they can post to the log
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn join_log_server(
    state: State<'_, Arc<AppState>>,
    server_id: String,       // connection key (address) — routes requests
    log_server_id: String,   // genesis hash — stamps events + keys the device chain
    invite_code: String,
) -> Result<(), String> {
    use farder_crypto::event_log::EventPayload;

    let identity = {
        let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
        let bytes = lock.ok_or_else(|| "identity is locked".to_string())?;
        Keypair::from_signing_key_bytes(&bytes)
    };
    let device = crate::device::load_or_create_device_keypair(identity.signing_key_bytes())?;

    // Serialize chain writes against submit_event and create_invite.
    let _chain_guard = state.device_chain_lock.lock().await;

    let mut ds = crate::device::DeviceState::load(&log_server_id)?
        .unwrap_or_else(|| crate::device::DeviceState::fresh(&device));

    if ds.joined {
        return Ok(()); // already a log member on this server
    }

    // 1. Authorize this device if needed (mirrors submit_event / create_invite).
    if !ds.authorized {
        let cert = crate::device::device_cert(&identity, &device);
        let da = event_build_next(&device, &identity, &log_server_id, ds.last_event_hash.clone(),
            ds.next_seq, ds.lamport, EventPayload::DeviceAuthorized { cert });
        event_send_submit(&state, &server_id, &da).await?;
        ds.next_seq = da.core.seq + 1;
        ds.last_event_hash = Some(da.hash());
        ds.lamport = da.core.lamport;
        ds.authorized = true;
        ds.save(&log_server_id)?;
    }

    // 2. Resolve the invite code to its InviteCreated event hash.
    let resolved = bridge::send_request(&state, &server_id,
        ServerRequest::ResolveInvite { code: invite_code })
        .await.map_err(|e| e.to_string())?;
    let invite_event = match resolved {
        ServerResponse::InviteResolved { invite_event: Some(h) } => h,
        ServerResponse::InviteResolved { invite_event: None } =>
            return Err("invite not found on this server (it may not be a mesh invite)".to_string()),
        ServerResponse::Error { reason } => return Err(reason),
        other => return Err(format!("unexpected response to ResolveInvite: {:?}", other)),
    };

    // 3. Emit the self-signed MemberJoined citing the invite.
    let join = event_build_next(&device, &identity, &log_server_id, ds.last_event_hash.clone(),
        ds.next_seq, ds.lamport, EventPayload::MemberJoined { member: identity.public_key(), invite: invite_event });
    match event_send_submit(&state, &server_id, &join).await {
        Ok(_) => {
            ds.next_seq = join.core.seq + 1;
            ds.last_event_hash = Some(join.hash());
            ds.lamport = join.core.lamport;
            ds.joined = true;
            ds.save(&log_server_id)?;
            Ok(())
        }
        // Already a member (e.g. joined on another device): treat as success so we
        // stop retrying. The chain head advanced server-side only if accepted; on a
        // rejection nothing advanced, so just mark joined and move on.
        Err(e) if e.to_string().contains("already a member") => {
            ds.joined = true;
            ds.save(&log_server_id)?;
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Membership approval — approve_member / deny_member
// ---------------------------------------------------------------------------

/// Shared helper: emit any membership-moderation event signed by the approver's
/// device. Mirrors `join_log_server`'s emit section: acquires the chain lock,
/// loads identity+device+DeviceState, auto-emits DeviceAuthorized if needed,
/// builds+submits the given payload, and advances+saves chain state ONLY on Ok.
async fn moderate_member(
    state: &AppState,
    server_id: &str,
    log_server_id: &str,
    payload: farder_crypto::event_log::EventPayload,
) -> Result<(), String> {
    use farder_crypto::event_log::EventPayload;

    let identity = {
        let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
        let bytes = lock.ok_or_else(|| "identity is locked".to_string())?;
        Keypair::from_signing_key_bytes(&bytes)
    };
    let device = crate::device::load_or_create_device_keypair(identity.signing_key_bytes())?;

    // Serialize chain writes against submit_event and join_log_server.
    let _chain_guard = state.device_chain_lock.lock().await;

    let mut ds = crate::device::DeviceState::load(log_server_id)?
        .unwrap_or_else(|| crate::device::DeviceState::fresh(&device));

    // 1. Authorize this device if needed (mirrors submit_event / join_log_server).
    if !ds.authorized {
        let cert = crate::device::device_cert(&identity, &device);
        let da = event_build_next(
            &device,
            &identity,
            log_server_id,
            ds.last_event_hash.clone(),
            ds.next_seq,
            ds.lamport,
            EventPayload::DeviceAuthorized { cert },
        );
        event_send_submit(state, server_id, &da).await?;
        ds.next_seq = da.core.seq + 1;
        ds.last_event_hash = Some(da.hash());
        ds.lamport = da.core.lamport;
        ds.authorized = true;
        ds.save(log_server_id)?;
    }

    // 2. Build + submit the moderation event, chaining from the stored head.
    let ev = event_build_next(
        &device,
        &identity,
        log_server_id,
        ds.last_event_hash.clone(),
        ds.next_seq,
        ds.lamport,
        payload,
    );
    match event_send_submit(state, server_id, &ev).await {
        Ok(_) => {
            ds.next_seq = ev.core.seq + 1;
            ds.last_event_hash = Some(ev.hash());
            ds.lamport = ev.core.lamport;
            ds.save(log_server_id)?;
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Approve a pending member: emits a signed `MemberApproved { member }` event.
#[tauri::command]
pub async fn approve_member(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    log_server_id: String,
    member: String,
) -> Result<(), String> {
    let target = parse_public_key(&member)?;
    moderate_member(
        &state,
        &server_id,
        &log_server_id,
        farder_crypto::event_log::EventPayload::MemberApproved { member: target },
    )
    .await
}

/// Deny / remove a pending member: emits a signed `MemberRemoved { member }` event.
#[tauri::command]
pub async fn deny_member(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    log_server_id: String,
    member: String,
) -> Result<(), String> {
    let target = parse_public_key(&member)?;
    moderate_member(
        &state,
        &server_id,
        &log_server_id,
        farder_crypto::event_log::EventPayload::MemberRemoved { member: target },
    )
    .await
}

/// Return the caller's membership status on this server: "member" / "pending" / "none".
/// Allowed for non-members so a pending joiner can poll their own status.
#[tauri::command]
pub async fn get_membership_status(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<String, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::GetMembershipStatus)
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::MembershipStatus { status } => Ok(status),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Return the list of members currently awaiting approval.
/// Gated server-side to holders of KICK_MEMBERS and the owner.
#[tauri::command]
pub async fn get_pending_members(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<Vec<MemberInfo>, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::GetPendingMembers)
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::PendingMembers { members } => Ok(members),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// Poll & giveaway widget interactions
// ---------------------------------------------------------------------------

/// Full poll state for the widget: the shared `PollInfo` plus the requester's
/// own vote (self-only — never another member's).
#[derive(serde::Serialize)]
pub struct PollState {
    pub poll: PollInfo,
    pub my_vote: Option<u32>,
}

/// Fetch a poll's current state (counts, closed flag) plus my own vote.
/// The widget's state-recovery path on mount/reconnect/server switch.
#[tauri::command]
pub async fn get_poll(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    poll_id: i64,
) -> Result<PollState, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::GetPoll { poll_id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Poll { poll, my_vote } => Ok(PollState { poll, my_vote }),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Cast (or change) my vote on a poll option. The server broadcasts the
/// updated counts as `server:poll_updated`.
#[tauri::command]
pub async fn vote_poll(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    poll_id: i64,
    option_index: u32,
) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::VotePoll { poll_id, option_index })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Retract my vote on an open poll.
#[tauri::command]
pub async fn retract_vote(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    poll_id: i64,
) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::RetractVote { poll_id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Close a poll early (creator or MANAGE_SERVER — enforced server-side).
#[tauri::command]
pub async fn close_poll(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    poll_id: i64,
) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::ClosePoll { poll_id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Map a `GiveawayInfo` to its frontend JSON shape: `winner` becomes the
/// "vk_<hex>" string form (the spec'd TS type — matching the pk-to-string
/// convention for standalone keys in bridge.rs) instead of serde's `{ bytes }`
/// object; every other field keeps its plain serde encoding (`creator` stays
/// `{ bytes }`, same shape as `MessageInfo.author`). Used by both the
/// `get_giveaway` command and the `server:giveaway_updated` event in bridge.rs
/// so the two paths can never drift.
pub(crate) fn giveaway_json(g: &GiveawayInfo) -> serde_json::Value {
    let mut v = serde_json::to_value(g).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "winner".to_string(),
            match &g.winner {
                Some(pk) => serde_json::Value::String(pk.to_string()),
                None => serde_json::Value::Null,
            },
        );
    }
    v
}

/// Full giveaway state for the widget: the shared giveaway state (frontend
/// JSON shape via `giveaway_json`) plus whether I have entered (self-only).
#[derive(serde::Serialize)]
pub struct GiveawayState {
    pub giveaway: serde_json::Value,
    pub my_entered: bool,
}

/// Fetch a giveaway's current state plus whether I have entered.
/// The widget's state-recovery path on mount/reconnect/server switch.
#[tauri::command]
pub async fn get_giveaway(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    giveaway_id: i64,
) -> Result<GiveawayState, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::GetGiveaway { giveaway_id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Giveaway { giveaway, my_entered } => {
            Ok(GiveawayState { giveaway: giveaway_json(&giveaway), my_entered })
        }
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Enter an open giveaway (idempotent — already-entered is Ok).
#[tauri::command]
pub async fn enter_giveaway(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    giveaway_id: i64,
) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::EnterGiveaway { giveaway_id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Leave an open giveaway (idempotent).
#[tauri::command]
pub async fn leave_giveaway(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    giveaway_id: i64,
) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::LeaveGiveaway { giveaway_id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Cancel an open giveaway (creator or MANAGE_SERVER — enforced server-side).
#[tauri::command]
pub async fn cancel_giveaway(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    giveaway_id: i64,
) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::CancelGiveaway { giveaway_id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Redraw a finished giveaway's winner (creator or MANAGE_SERVER — enforced
/// server-side).
#[tauri::command]
pub async fn reroll_giveaway(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    giveaway_id: i64,
) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::RerollGiveaway { giveaway_id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// Event widget interactions
// ---------------------------------------------------------------------------

/// Full event state for the widget: the shared `EventInfo` plus MY OWN RSVP
/// (self-only — the broadcast never carries it). `EventInfo` needs no
/// `giveaway_json`-style remapping: it has no `Option<PublicKey>`, so plain
/// serde gives the frontend shape (`creator` as `{ bytes }`, same as
/// `PollInfo.creator`).
#[derive(serde::Serialize)]
pub struct EventState {
    pub event: EventInfo,
    pub my_rsvp: Option<String>,
}

/// Fetch an event's current state (roster, counts, status) plus my own RSVP.
/// The widget's state-recovery path on mount/reconnect/server switch.
/// Missing/invisible events come back as the server's opaque "event not found".
#[tauri::command]
pub async fn get_event(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    event_id: i64,
) -> Result<EventState, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::GetEvent { event_id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Event { event, my_rsvp } => Ok(EventState { event, my_rsvp }),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Set (or change) my RSVP on an upcoming event — "going" | "maybe" | "no".
/// The server broadcasts the updated roster as `server:event_updated`.
#[tauri::command]
pub async fn rsvp_event(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    event_id: i64,
    response: String,
) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::RsvpEvent { event_id, response })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Withdraw my RSVP entirely (the poll-retract idiom: pressing my own option).
#[tauri::command]
pub async fn clear_rsvp(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    event_id: i64,
) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::ClearRsvp { event_id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Cancel an upcoming event (creator or MANAGE_SERVER — enforced server-side).
/// The server DMs every responder once.
#[tauri::command]
pub async fn cancel_event(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    event_id: i64,
) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::CancelEvent { event_id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Edit an upcoming event. FULL REPLACE of the editable fields (no partial
/// patch): every field is sent every time, validated exactly as on creation.
/// Creator or MANAGE_SERVER — enforced server-side.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn edit_event(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    event_id: i64,
    title: String,
    description: Option<String>,
    location: Option<String>,
    starts_at: u64,
    remind_lead: Option<u64>,
) -> Result<(), String> {
    match bridge::send_request(
        &state,
        &server_id,
        ServerRequest::EditEvent { event_id, title, description, location, starts_at, remind_lead },
    )
    .await
    .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// Personal reminders
// ---------------------------------------------------------------------------

/// My own pending reminders, soonest first. Owner-scoped server-side by the
/// authenticated connection key — the request carries no owner field.
#[tauri::command]
pub async fn list_my_reminders(
    state: State<'_, Arc<AppState>>,
    server_id: String,
) -> Result<Vec<ReminderInfo>, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::ListMyReminders)
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::MyReminders { reminders } => Ok(reminders),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Cancel one of MY pending reminders. Someone else's id gets the same opaque
/// "reminder not found" as a nonexistent one.
#[tauri::command]
pub async fn cancel_reminder(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    reminder_id: i64,
) -> Result<(), String> {
    match bridge::send_request(&state, &server_id, ServerRequest::CancelReminder { reminder_id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// The channel's OPEN widgets for the active-widgets bar. Shared state only —
/// no per-viewer fields (`my_vote`/`my_entered`/`my_rsvp` stay exclusive to
/// `get_poll` / `get_giveaway` / `get_event`; the chip dropdown's widget pulls
/// those via its own mount fetch). Giveaways go through `giveaway_json` so the
/// frontend shape (`winner` as a "vk_<hex>" string) can never drift from the
/// `get_giveaway` and `server:giveaway_updated` paths; events pass through raw
/// (no optional pubkey to remap).
#[derive(serde::Serialize)]
pub struct ActiveWidgets {
    pub polls: Vec<PollInfo>,
    pub giveaways: Vec<serde_json::Value>,
    pub events: Vec<EventInfo>,
}

/// Fetch a channel's open polls + giveaways + upcoming events (each list
/// oldest-first, 20 combined server-side). Visibility failures come back as
/// the server's opaque "channel not found".
#[tauri::command]
pub async fn list_active_widgets(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
) -> Result<ActiveWidgets, String> {
    match bridge::send_request(&state, &server_id, ServerRequest::ListActiveWidgets { channel_id })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerResponse::ActiveWidgets { polls, giveaways, events } => Ok(ActiveWidgets {
            polls,
            giveaways: giveaways.iter().map(giveaway_json).collect(),
            events,
        }),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

// ---------------------------------------------------------------------------

// Public re-export so other modules can resolve paths under ~/.farder/.
pub(crate) fn farder_data_dir_pub() -> std::path::PathBuf {
    farder_data_dir()
}

#[cfg(test)]
mod voice_settings_tests {
    use super::*;

    // Settings I/O is process-global (a single ~/.farder/settings.json keyed
    // off the FARDER_DATA env var), so serialize tests that mutate it.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Point FARDER_DATA at a fresh temp dir for the duration of `f`, so the
    /// real `read_settings`/`write_settings` helpers operate on an isolated
    /// settings.json. Mirrors how the crate already isolates `farder_data_dir`.
    fn with_temp_config<F: FnOnce()>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var("FARDER_DATA").ok();
        let tmp = std::env::temp_dir().join(format!(
            "farder-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("FARDER_DATA", &tmp);
        let _ = std::fs::remove_file(settings_path());
        f();
        let _ = std::fs::remove_dir_all(&tmp);
        match prev {
            Some(v) => std::env::set_var("FARDER_DATA", v),
            None => std::env::remove_var("FARDER_DATA"),
        }
    }

    #[test]
    fn voice_mode_defaults_to_open_mic_and_round_trips() {
        with_temp_config(|| {
            assert_eq!(read_voice_mode(), "OpenMic");
            assert_eq!(read_ptt_key(), "Backquote");

            // set_voice_mode normalizes unknown values to OpenMic.
            set_voice_mode("PushToTalk".to_string()).unwrap();
            assert_eq!(read_voice_mode(), "PushToTalk");
            set_voice_mode("garbage".to_string()).unwrap();
            assert_eq!(read_voice_mode(), "OpenMic");

            set_ptt_key("KeyV".to_string()).unwrap();
            assert_eq!(read_ptt_key(), "KeyV");
        });
    }

    #[test]
    fn persist_peer_volume_clamps_and_roundtrips() {
        with_temp_config(|| {
            persist_peer_volume("deadbeef", 9.0).unwrap();
            assert_eq!(read_peer_volumes().get("deadbeef"), Some(&2.0));
            persist_peer_volume("deadbeef", -3.0).unwrap();
            assert_eq!(read_peer_volumes().get("deadbeef"), Some(&0.0));
        });
    }

    #[test]
    fn data_saver_embeds_defaults_to_false_and_round_trips() {
        with_temp_config(|| {
            // Default must be false (auto-show embeds).
            assert!(!read_data_saver_embeds());

            // Round-trip: enable, verify, disable, verify.
            set_data_saver_embeds(true).unwrap();
            assert!(read_data_saver_embeds());

            set_data_saver_embeds(false).unwrap();
            assert!(!read_data_saver_embeds());
        });
    }
}

#[cfg(test)]
mod relay_choice_tests {
    use super::*;

    #[test]
    fn direct_resolves_to_none() {
        assert!(resolve_relay_choice("direct", None, None).unwrap().is_none());
    }

    #[test]
    fn farder_resolves_to_the_default_relay() {
        let r = resolve_relay_choice("farder", None, None).unwrap();
        assert!(r.is_some(), "default relay is configured");
    }

    #[test]
    fn selfhost_validates_addr_and_fingerprint() {
        let ok = resolve_relay_choice("selfhost", Some("1.2.3.4:4433"), Some(&"ab".repeat(32))).unwrap();
        assert!(ok.is_some());
        assert!(resolve_relay_choice("selfhost", Some("nope"), Some(&"ab".repeat(32))).is_err());
        assert!(resolve_relay_choice("selfhost", Some("1.2.3.4:4433"), Some("zz")).is_err());
        assert!(resolve_relay_choice("selfhost", Some("1.2.3.4:4433"), Some("abcd")).is_err()); // not 32 bytes
    }
}

#[cfg(test)]
mod link_embed_tests {
    use super::*;

    #[test]
    fn link_embed_cache_roundtrip() {
        use farder_protocol::messages::EmbedOutcome;
        let c = link_embed_cache();
        {
            let mut m = c.lock().unwrap();
            m.insert("u".into(), (std::time::Instant::now(), EmbedOutcome::Unsupported));
        }
        let hit = {
            let m = c.lock().unwrap();
            m.get("u").map(|(_, v)| v.clone())
        };
        assert_eq!(hit, Some(EmbedOutcome::Unsupported));
    }
}

#[cfg(test)]
mod submit_event_tests {
    use super::*;
    use farder_crypto::event_log::EventPayload;

    /// Build an event via `event_build_next` and assert it verifies under the
    /// device key and that the chaining fields are set correctly.
    #[test]
    fn build_next_verifies_and_chains() {
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let server_id = "a".repeat(64); // fake hex server_id (64 hex chars = 32 bytes)

        // seq=0, no prev
        let ev0 = event_build_next(
            &device,
            &identity,
            &server_id,
            None,
            0,
            0,
            EventPayload::MessagePosted {
                channel_id: 1,
                content: "hello".to_string(),
                reply_to: None,
                attachments: vec![],
            },
        );
        assert!(ev0.verify(&device.public_key()).is_ok());
        assert_eq!(ev0.core.seq, 0);
        assert!(ev0.core.prev.is_none());
        assert_eq!(ev0.core.lamport, 1); // lamport_observed(0) + 1

        // seq=1, prev = hash of ev0
        let hash0 = ev0.hash();
        let ev1 = event_build_next(
            &device,
            &identity,
            &server_id,
            Some(hash0.clone()),
            1,
            ev0.core.lamport,
            EventPayload::MessagePosted {
                channel_id: 1,
                content: "world".to_string(),
                reply_to: None,
                attachments: vec![],
            },
        );
        assert!(ev1.verify(&device.public_key()).is_ok());
        assert_eq!(ev1.core.seq, 1);
        assert_eq!(ev1.core.prev.as_deref(), Some(hash0.as_str()));
        assert_eq!(ev1.core.lamport, 2);
    }
}
