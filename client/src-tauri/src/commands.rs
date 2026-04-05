use crate::bridge;
use crate::connection::connect_and_authenticate;
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
// Saved servers list
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct ServerEntry {
    pub id: String,
    pub name: String,
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
    let mut entries = load_server_entries();
    if !entries.iter().any(|e| e.id == address) {
        entries.push(ServerEntry { id: address.to_string(), name: name.to_string() });
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

    let server_conn = Arc::new(ServerConnection {
        endpoint,
        connection: conn,
        send_stream: tokio::sync::Mutex::new(send),
        next_request_id: AtomicU32::new(1),
        pending_requests: Mutex::new(HashMap::new()),
        event_reader_handle: Mutex::new(None),
        server_name: Mutex::new(String::new()),
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
        ServerResponse::ServerInfo { name, member_count, channels, categories, roles } => {
            *server_conn.server_name.lock().unwrap() = name.clone();
            // Save to persistent server list
            save_server_entry(&address, &name);
            Ok(ConnectResult { server_name: name, member_count, channels, categories, roles })
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
        ServerResponse::ServerInfo { name, member_count, channels, categories, roles } => {
            Ok(ConnectResult { server_name: name, member_count, channels, categories, roles })
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

/// Add a reaction to a message.
#[tauri::command]
pub async fn add_reaction(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    message_id: u64,
    emoji: String,
) -> Result<(), String> {
    let response = bridge::send_request(&state, &server_id, ServerRequest::AddReaction { message_id, emoji })
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
) -> Result<(), String> {
    let response =
        bridge::send_request(&state, &server_id, ServerRequest::RemoveReaction { message_id, emoji })
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
    use sha2::{Digest, Sha256};

    // Read file from disk
    let data = std::fs::read(&file_path).map_err(|e| e.to_string())?;
    let file_name = std::path::Path::new(&file_path)
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
    let conn = state.get_server(&server_id).map_err(|e| e.to_string())?;
    let quic_conn = conn.connection.clone();
    let (mut send, mut recv) = quic_conn.open_bi().await.map_err(|e| e.to_string())?;

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
    let conn = state.get_server(&server_id).map_err(|e| e.to_string())?;
    let quic_conn = conn.connection.clone();
    let (mut send, mut recv) = quic_conn.open_bi().await.map_err(|e| e.to_string())?;

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
            // Build the https://farder.gg/join/ link using server_id as the address
            use base64::Engine;
            let plain = format!("{}/{}", server_id, code);
            let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(plain.as_bytes());
            let link = format!("https://farder.gg/join/{}", encoded);
            let deep_link = format!("farder://{}/{}", server_id, code);
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
