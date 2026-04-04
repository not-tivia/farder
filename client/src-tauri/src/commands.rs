use crate::bridge;
use crate::connection::connect_and_authenticate;
use crate::state::AppState;
use crate::tls::make_client_endpoint;
use farder_crypto::identity::Keypair;
use farder_protocol::server::{
    CategoryInfo, ChannelInfo, MemberInfo, MessageInfo, RoleInfo, ServerRequest, ServerResponse,
};
use std::sync::Arc;
use tauri::{AppHandle, State};

// ---------------------------------------------------------------------------
// Profile helpers
// ---------------------------------------------------------------------------

fn farder_data_dir() -> std::path::PathBuf {
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
}

#[derive(serde::Serialize)]
pub struct SendMessageResult {
    pub id: u64,
    pub timestamp: u64,
}

// ---------------------------------------------------------------------------
// Identity commands
// ---------------------------------------------------------------------------

fn key_path() -> std::path::PathBuf {
    farder_data_dir().join("identity.key")
}

#[tauri::command]
pub fn generate_keypair(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    // Always generate a fresh key, overwriting any saved one
    let keypair = Keypair::generate();
    let path = key_path();
    std::fs::write(&path, keypair.signing_key_bytes()).map_err(|e| e.to_string())?;
    let public_key = keypair.public_key().to_string();
    let mut lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
    *lock = Some(*keypair.signing_key_bytes());
    Ok(public_key)
}

/// Load a previously saved identity from disk, or return null if none exists.
#[tauri::command]
pub fn load_identity(state: State<'_, Arc<AppState>>) -> Option<String> {
    let path = key_path();
    let bytes: [u8; 32] = std::fs::read(&path).ok()?.try_into().ok()?;
    let keypair = Keypair::from_signing_key_bytes(&bytes);
    let public_key = keypair.public_key().to_string();
    let mut lock = state.signing_key_bytes.lock().ok()?;
    *lock = Some(bytes);
    Some(public_key)
}

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
    let json = serde_json::json!({ "display_name": name });
    std::fs::write(&path, json.to_string()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_display_name() -> Option<String> {
    let path = profile_path();
    let data = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v["display_name"].as_str().map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Settings commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn save_last_server(address: String) -> Result<(), String> {
    let path = settings_path();
    let json = serde_json::json!({ "address": address });
    std::fs::write(&path, json.to_string()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_last_server() -> Option<String> {
    let path = settings_path();
    let data = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v["address"].as_str().map(|s| s.to_string())
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
            None => return Err("no identity keypair set — call generate_keypair first".to_string()),
        }
    };

    let addr: std::net::SocketAddr = address
        .parse()
        .map_err(|e: std::net::AddrParseError| e.to_string())?;

    let endpoint = make_client_endpoint().map_err(|e| e.to_string())?;

    let (conn, send, recv, _session_token) =
        connect_and_authenticate(endpoint.clone(), addr, &keypair, invite_code, setup_token)
            .await
            .map_err(|e| e.to_string())?;

    // Store endpoint + connection (both must stay alive or QUIC closes).
    {
        let mut ep = state.endpoint.lock().map_err(|e| e.to_string())?;
        *ep = Some(endpoint);
    }
    {
        let mut c = state.connection.lock().map_err(|e| e.to_string())?;
        *c = Some(conn);
    }
    {
        let mut ss = state.send_stream.lock().await;
        *ss = Some(send);
    }
    {
        let mut c = state.connected.lock().map_err(|e| e.to_string())?;
        *c = true;
    }

    // Spawn the background event reader.
    let handle = bridge::spawn_event_reader(app, Arc::clone(&state), recv);
    {
        let mut h = state.event_reader_handle.lock().map_err(|e| e.to_string())?;
        *h = Some(handle);
    }

    // Persist the server address for next launch (non-fatal).
    let _ = save_last_server(address);

    // Fetch initial server info.
    let response = bridge::send_request(&state, ServerRequest::GetServerInfo)
        .await
        .map_err(|e| e.to_string())?;

    match response {
        ServerResponse::ServerInfo { name, member_count, channels, categories, roles } => {
            Ok(ConnectResult { server_name: name, member_count, channels, categories, roles })
        }
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Disconnect from the current server.
#[tauri::command]
pub async fn disconnect_server(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    // Clear endpoint, connection, and send stream.
    {
        let mut ep = state.endpoint.lock().map_err(|e| e.to_string())?;
        *ep = None;
    }
    {
        let mut c = state.connection.lock().map_err(|e| e.to_string())?;
        *c = None;
    }
    {
        let mut ss = state.send_stream.lock().await;
        *ss = None;
    }
    // Abort the event reader task.
    {
        let mut h = state.event_reader_handle.lock().map_err(|e| e.to_string())?;
        if let Some(handle) = h.take() {
            handle.abort();
        }
    }
    {
        let mut c = state.connected.lock().map_err(|e| e.to_string())?;
        *c = false;
    }
    Ok(())
}

/// Send a chat message to a channel.
#[tauri::command]
pub async fn send_message(
    state: State<'_, Arc<AppState>>,
    channel_id: u64,
    content: String,
    reply_to: Option<u64>,
) -> Result<SendMessageResult, String> {
    let response = bridge::send_request(
        &state,
        ServerRequest::SendMessage {
            channel_id,
            content,
            reply_to,
            attachment_ids: vec![],
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
    channel_id: u64,
    before_id: Option<u64>,
    limit: Option<u32>,
) -> Result<Vec<MessageInfo>, String> {
    let response = bridge::send_request(
        &state,
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
    channel_ids: Vec<u64>,
) -> Result<(), String> {
    let response = bridge::send_request(&state, ServerRequest::Subscribe { channel_ids })
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
pub async fn get_members(state: State<'_, Arc<AppState>>) -> Result<Vec<MemberInfo>, String> {
    let response = bridge::send_request(&state, ServerRequest::GetMembers)
        .await
        .map_err(|e| e.to_string())?;

    match response {
        ServerResponse::Members { members } => Ok(members),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

/// Add a reaction to a message.
#[tauri::command]
pub async fn add_reaction(
    state: State<'_, Arc<AppState>>,
    message_id: u64,
    emoji: String,
) -> Result<(), String> {
    let response = bridge::send_request(&state, ServerRequest::AddReaction { message_id, emoji })
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
    message_id: u64,
    emoji: String,
) -> Result<(), String> {
    let response =
        bridge::send_request(&state, ServerRequest::RemoveReaction { message_id, emoji })
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
    message_id: u64,
    name: Option<String>,
) -> Result<(), String> {
    let response =
        bridge::send_request(&state, ServerRequest::CreateThread { message_id, name })
            .await
            .map_err(|e| e.to_string())?;

    match response {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
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
    max_uses: Option<u32>,
) -> Result<InviteResult, String> {
    let response = bridge::send_request(
        &state,
        ServerRequest::CreateInvite { max_uses, expires_in_secs: None, target_channel: None },
    )
    .await
    .map_err(|e| e.to_string())?;
    match response {
        ServerResponse::InviteCreated { code } => {
            // Build the https://farder.gg/join/ link using saved server address
            let address = get_last_server().unwrap_or_else(|| "localhost:4435".to_string());
            use base64::Engine;
            let plain = format!("{}/{}", address, code);
            let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(plain.as_bytes());
            let link = format!("https://farder.gg/join/{}", encoded);
            let deep_link = format!("farder://{}/{}", address, code);
            Ok(InviteResult { code, link, deep_link })
        }
        ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}
