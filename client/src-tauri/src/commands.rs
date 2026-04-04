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

const KEY_FILE: &str = "farder-identity.key";

fn key_path() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_default().join(KEY_FILE)
}

#[tauri::command]
pub fn generate_keypair(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    // Check if saved key exists on disk first
    let path = key_path();
    let keypair = if path.exists() {
        let bytes: [u8; 32] = std::fs::read(&path)
            .map_err(|e| e.to_string())?
            .try_into()
            .map_err(|_| "invalid key file".to_string())?;
        eprintln!("[identity] loaded existing key from {}", path.display());
        Keypair::from_signing_key_bytes(&bytes)
    } else {
        let kp = Keypair::generate();
        std::fs::write(&path, kp.signing_key_bytes()).map_err(|e| e.to_string())?;
        eprintln!("[identity] generated and saved new key to {}", path.display());
        kp
    };
    let public_key = keypair.public_key().to_string();
    let mut lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
    *lock = Some(*keypair.signing_key_bytes());
    Ok(public_key)
}

#[tauri::command]
pub fn get_public_key(state: State<'_, Arc<AppState>>) -> Option<String> {
    // Try state first, then disk
    let lock = state.signing_key_bytes.lock().ok()?;
    if let Some(bytes) = lock.as_ref() {
        return Some(Keypair::from_signing_key_bytes(bytes).public_key().to_string());
    }
    drop(lock);
    let path = key_path();
    if path.exists() {
        let bytes: [u8; 32] = std::fs::read(&path).ok()?.try_into().ok()?;
        Some(Keypair::from_signing_key_bytes(&bytes).public_key().to_string())
    } else {
        None
    }
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
