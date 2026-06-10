use crate::bridge;
use crate::connection::connect_and_authenticate;
use crate::connection::connect_via_relay;
use crate::state::{AppState, ServerConnection};
use crate::tls::make_client_endpoint;
use farder_crypto::identity::Keypair;
use farder_protocol::server::{
    CategoryInfo, ChannelInfo, MemberInfo, MessageInfo, RoleInfo, ServerRequest, ServerResponse,
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
}

#[derive(serde::Serialize)]
pub struct SendMessageResult {
    pub id: u64,
    pub timestamp: u64,
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
pub fn set_display_name(name: String) -> Result<(), String> {
    let path = profile_path();
    let mut data: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    data["display_name"] = serde_json::json!(name);
    std::fs::write(&path, data.to_string()).map_err(|e| e.to_string())
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
pub async fn set_avatar(file_path: String) -> Result<String, String> {
    let data = std::fs::read(&file_path).map_err(|e| e.to_string())?;
    let avatar_path = farder_data_dir().join("avatar.png");
    std::fs::write(&avatar_path, &data).map_err(|e| e.to_string())?;

    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
    let mime = if file_path.ends_with(".png") {
        "image/png"
    } else if file_path.ends_with(".gif") {
        "image/gif"
    } else {
        "image/jpeg"
    };
    Ok(format!("data:{};base64,{}", mime, b64))
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

    // Save to settings for get_last_server compatibility (non-fatal).
    let _ = save_last_server(address.clone());

    // Fetch initial server info.
    let response = bridge::send_request(&state, &address, ServerRequest::GetServerInfo)
        .await
        .map_err(|e| e.to_string())?;

    match response {
        ServerResponse::ServerInfo { name, member_count, channels, categories, roles, owner_public_key } => {
            *server_conn.server_name.lock().unwrap() = name.clone();
            // Save to persistent server list
            save_server_entry(&address, &name);
            Ok(ConnectResult { server_name: name, member_count, channels, categories, roles, owner_public_key, relayed })
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
        ServerResponse::ServerInfo { name, member_count, channels, categories, roles, owner_public_key } => {
            Ok(ConnectResult { server_name: name, member_count, channels, categories, roles, owner_public_key, relayed: false })
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

/// Upload a file via a new QUIC bi-stream and return the file_id.
#[tauri::command]
pub async fn upload_file(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    channel_id: u64,
    file_path: String,
) -> Result<u64, String> {
    upload_file_internal_with_channel(&state, &server_id, channel_id, &file_path).await
}

/// Internal helper: upload a file to a server without a specific channel (channel_id = 0).
/// Used by book.rs to cache uploaded images per-server without going through the Tauri command layer.
pub(crate) async fn upload_file_internal(
    state: &AppState,
    server_id: &str,
    file_path: &str,
) -> Result<u64, String> {
    upload_file_internal_with_channel(state, server_id, 0, file_path).await
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
) -> Result<u64, String> {
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

    // Send UploadRequest
    let req = farder_protocol::server::UploadRequest {
        channel_id,
        file_name,
        file_size: data.len() as u64,
        hash,
        mime_type,
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
                farder_protocol::server::UploadResponse::Complete { file_id } => Ok(file_id),
                farder_protocol::server::UploadResponse::Error { reason } => Err(reason),
                _ => Err("unexpected upload response".to_string()),
            }
        }
        farder_protocol::server::UploadResponse::Complete { file_id } => Ok(file_id), // dedup
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

            // For images, return as base64 data URL
            let is_image = mime_type.starts_with("image/");
            if is_image {
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
    server_id: String,
    max_uses: Option<u32>,
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
                    let deep_link = crate::connection::build_relay_link(&target, &code);
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
) -> Result<(), String> {
    let pk = parse_public_key(&member_key)?;
    let response = bridge::send_request(&state, &server_id, ServerRequest::KickMember { member_key: pk })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn ban_member(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    member_key: String,
    reason: Option<String>,
) -> Result<(), String> {
    let pk = parse_public_key(&member_key)?;
    let response = bridge::send_request(&state, &server_id, ServerRequest::BanMember { member_key: pk, reason })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected: {:?}", other)),
    }
}

#[tauri::command]
pub async fn unban_member(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    member_key: String,
) -> Result<(), String> {
    let pk = parse_public_key(&member_key)?;
    let response = bridge::send_request(&state, &server_id, ServerRequest::UnbanMember { member_key: pk })
        .await
        .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::Ok => Ok(()),
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
static RECORDING_PATH: Mutex<Option<String>> = Mutex::new(None);

/// Start recording audio from the default input device. Writes WAV to a temp file.
#[tauri::command]
pub async fn start_recording() -> Result<(), String> {
    use std::sync::atomic::Ordering;
    if RECORDING.load(Ordering::SeqCst) {
        return Err("already recording".to_string());
    }
    RECORDING.store(true, Ordering::SeqCst);

    let tmp_dir = std::env::temp_dir();
    let filename = format!("farder_voice_{}.wav", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
    let path = tmp_dir.join(&filename);
    let path_str = path.to_string_lossy().to_string();
    if let Ok(mut g) = RECORDING_PATH.lock() {
        *g = Some(path_str.clone());
    }

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
            let device = host
                .default_input_device()
                .ok_or_else(|| "no input device available".to_string())?;
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
        Ok(Ok(())) => Ok(()),
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
#[tauri::command]
pub async fn stop_recording() -> Result<String, String> {
    use std::sync::atomic::Ordering;
    RECORDING.store(false, Ordering::SeqCst);
    // Give the recording thread a moment to finalize the WAV. Async sleep so we
    // don't block a worker thread.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let path = RECORDING_PATH
        .lock()
        .map_err(|_| "recording state poisoned".to_string())?
        .take()
        .ok_or("no recording in progress")?;
    if !std::path::Path::new(&path).exists() {
        return Err("recording failed — no audio file was written".to_string());
    }
    Ok(path)
}

/// Play a WAV file on the default output device. Reads the entire file and
/// feeds samples through a cpal output stream, then drops the stream when done.
/// Used by the "Test mic" flow to play back a just-recorded WAV.
#[tauri::command]
pub async fn play_audio_file(path: String) -> Result<(), String> {
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
        let device = host
            .default_output_device()
            .ok_or_else(|| "no output device available".to_string())?;
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
pub async fn voice_leave(
    voice: State<'_, Arc<crate::voice::VoiceController>>,
) -> Result<(), String> {
    voice.leave().await
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
        // Retry until the server has registered with the relay (or time out).
        let connected = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                match crate::connection::connect_via_relay(endpoint.clone(), &target, &keypair, None).await {
                    Ok(t) => return t,
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(300)).await,
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
        } => {
            *server_conn.server_name.lock().unwrap() = srv_name.clone();
            Ok(serde_json::json!({
                "address": address,
                "server_name": srv_name,
                "member_count": member_count,
                "channels": channels,
                "categories": categories,
                "roles": roles,
                "owner_public_key": owner_public_key,
                "relayed": relayed,
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
