# Farder Client v1: Server Chat UI — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Tauri desktop client that connects to a Farder server, authenticates with Ed25519, and provides a Windows XP-themed 3-panel interface for browsing channels, messaging, reactions, and threads.

**Architecture:** Tauri 2 Rust backend manages the QUIC connection — handles auth, sends requests, reads events. React frontend calls typed IPC commands and listens for Tauri events. XP Luna Blue theme via custom CSS. React Context + useReducer for state management. No external libraries beyond React and Tauri.

**Tech Stack:**
- Tauri 2 + React 18 + TypeScript 5 + Vite
- Quinn 0.11 (QUIC client in Tauri backend)
- farder-protocol + farder-crypto (shared types, codec, auth)
- Custom CSS (XP Luna theme)

**Spec:** `docs/specs/2026-04-03-farder-client-v1-server-chat-design.md`

---

## File Structure

### Tauri Backend (Rust) — `client/src-tauri/src/`

```
main.rs           # Tauri entry point (existing, will be rewritten)
state.rs          # AppState with ConnectionState (existing, will be rewritten)
commands.rs       # All IPC commands: identity + server (existing, will be extended)
connection.rs     # NEW: QUIC connection management, auth flow, frame I/O
bridge.rs         # NEW: Request-response dispatch, background event reader
tls.rs            # NEW: SkipServerVerification for self-signed certs
```

### React Frontend (TypeScript) — `client/src/`

```
main.tsx                     # React entry (existing, keep as-is)
App.tsx                      # Root component (existing, rewrite)
context/
  ServerContext.tsx           # NEW: Global state — connection, channels, messages, members
hooks/
  useServerEvents.ts         # NEW: Tauri event listeners → context dispatch
  useTauriCommand.ts         # NEW: Typed async IPC wrapper
components/
  ConnectDialog.tsx          # NEW: Identity generation + server connect form
  AppShell.tsx               # NEW: TitleBar + 3-panel layout
  TitleBar.tsx               # NEW: XP window chrome
  ChannelSidebar.tsx         # NEW: Server header, categories, channel list, user footer
  ChatPanel.tsx              # NEW: Channel header, message list, message input
  Message.tsx                # NEW: Single message with reactions, thread link, attachments
  MessageInput.tsx           # NEW: Text input with send button
  ReactionPicker.tsx         # NEW: Emoji grid popup
  MemberSidebar.tsx          # NEW: Member list grouped by role
  ThreadPanel.tsx            # NEW: Thread view
styles/
  xp-theme.css              # NEW: XP Luna Blue global theme
lib/
  tauri-bridge.ts            # Typed IPC wrappers (existing, will be extended)
  types.ts                   # NEW: TypeScript types matching farder-protocol
```

### Deleted Files (from Phase 1 scaffold)

```
client/src/components/Setup.tsx      # Replaced by ConnectDialog
client/src/components/Chat.tsx       # Replaced by ChatPanel
client/src/components/Contacts.tsx   # Not needed (DMs deferred)
client/src/components/Settings.tsx   # Not needed (deferred)
```

---

## Task 1: Tauri Backend — TLS & Connection

**Files:**
- Create: `client/src-tauri/src/tls.rs`
- Create: `client/src-tauri/src/connection.rs`
- Modify: `client/src-tauri/Cargo.toml`

- [ ] **Step 1: Add Quinn and related deps to Cargo.toml**

Add to `client/src-tauri/Cargo.toml` under `[dependencies]`:

```toml
quinn = "0.11"
rustls = { version = "0.23", features = ["ring"] }
hex = "0.4"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

- [ ] **Step 2: Create TLS skip verifier**

`client/src-tauri/src/tls.rs`:

```rust
use std::sync::Arc;

#[derive(Debug)]
pub struct SkipServerVerification;

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self, _: &[u8], _: &rustls::pki_types::CertificateDer<'_>, _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self, _: &[u8], _: &rustls::pki_types::CertificateDer<'_>, _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub fn make_client_endpoint() -> anyhow::Result<quinn::Endpoint> {
    let crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();
    let client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?,
    ));
    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}
```

- [ ] **Step 3: Create connection module**

`client/src-tauri/src/connection.rs`:

```rust
use anyhow::{Context, Result};
use farder_crypto::identity::{Keypair, PublicKey};
use farder_protocol::{codec, server::*};
use quinn::{Connection, RecvStream, SendStream};

// ── Frame I/O (same as server) ──────────────────────────────────────

pub async fn read_frame(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await.context("read frame length")?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        anyhow::bail!("frame too large: {} bytes", len);
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await.context("read frame payload")?;
    Ok(buf)
}

pub async fn write_frame(send: &mut SendStream, data: &[u8]) -> Result<()> {
    let len = (data.len() as u32).to_be_bytes();
    send.write_all(&len).await?;
    send.write_all(data).await?;
    Ok(())
}

pub async fn send_client_frame(send: &mut SendStream, frame: &ClientFrame) -> Result<()> {
    let data = codec::encode(frame)?;
    write_frame(send, &data).await
}

pub async fn recv_server_frame(recv: &mut RecvStream) -> Result<ServerFrame> {
    let data = read_frame(recv).await?;
    codec::decode(&data).context("decode server frame")
}

// ── Authentication ──────────────────────────────────────────────────

/// Connect to a server, authenticate, return (Connection, SendStream, session_token).
/// The RecvStream is consumed by the background reader (caller handles that).
pub async fn connect_and_authenticate(
    endpoint: &quinn::Endpoint,
    address: &str,
    keypair: &Keypair,
    invite_code: Option<&str>,
    setup_token: Option<&str>,
) -> Result<(Connection, SendStream, RecvStream, Vec<u8>)> {
    let addr = address.parse().context("invalid server address")?;
    let conn = endpoint.connect(addr, "farder-server")?.await.context("QUIC connect failed")?;

    // Server opens the main bi-stream (server sends Challenge first)
    let (mut send, mut recv) = conn.accept_bi().await.context("accept main bi-stream")?;

    // Receive Challenge
    let challenge = match recv_server_frame(&mut recv).await? {
        ServerFrame::Challenge { nonce } => nonce,
        ServerFrame::AuthError { reason } => anyhow::bail!("auth error: {}", reason),
        other => anyhow::bail!("expected Challenge, got {:?}", other),
    };

    // Sign and authenticate
    let signature = keypair.sign(&challenge);
    let auth_frame = ClientFrame::Authenticate {
        public_key: keypair.public_key(),
        signed_challenge: signature,
        invite_code: invite_code.map(String::from),
        setup_token: setup_token.map(String::from),
    };
    send_client_frame(&mut send, &auth_frame).await?;

    // Receive result
    match recv_server_frame(&mut recv).await? {
        ServerFrame::Authenticated { session_token } => {
            Ok((conn, send, recv, session_token))
        }
        ServerFrame::AuthError { reason } => anyhow::bail!("authentication failed: {}", reason),
        other => anyhow::bail!("expected Authenticated, got {:?}", other),
    }
}
```

- [ ] **Step 4: Verify compilation**

Run: `cd /home/deez/farder/client/src-tauri && cargo check`

Expected: Compiles (warnings about unused code OK).

- [ ] **Step 5: Commit**

```bash
git add client/src-tauri/
git commit -m "feat(client): add QUIC connection module with TLS and auth flow"
```

---

## Task 2: Tauri Backend — State, Bridge & Commands

**Files:**
- Rewrite: `client/src-tauri/src/state.rs`
- Create: `client/src-tauri/src/bridge.rs`
- Rewrite: `client/src-tauri/src/commands.rs`
- Rewrite: `client/src-tauri/src/main.rs`

- [ ] **Step 1: Rewrite state.rs with ConnectionState**

`client/src-tauri/src/state.rs`:

```rust
use farder_crypto::identity::Keypair;
use farder_protocol::server::{ServerFrame, ServerResponse};
use quinn::SendStream;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub type ResponseSender = oneshot::Sender<ServerResponse>;

pub struct AppState {
    pub keypair: Mutex<Option<Keypair>>,
    pub send_stream: Mutex<Option<SendStream>>,
    pub next_request_id: AtomicU32,
    pub pending_requests: Mutex<HashMap<u32, ResponseSender>>,
    pub event_reader_handle: Mutex<Option<JoinHandle<()>>>,
    pub connected: Mutex<bool>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            keypair: Mutex::new(None),
            send_stream: Mutex::new(None),
            next_request_id: AtomicU32::new(1),
            pending_requests: Mutex::new(HashMap::new()),
            event_reader_handle: Mutex::new(None),
            connected: Mutex::new(false),
        }
    }

    pub fn next_id(&self) -> u32 {
        self.next_request_id.fetch_add(1, Ordering::SeqCst)
    }
}
```

- [ ] **Step 2: Create bridge.rs — request dispatch and event reader**

`client/src-tauri/src/bridge.rs`:

```rust
use crate::connection;
use crate::state::AppState;
use anyhow::Result;
use farder_protocol::{codec, server::*};
use quinn::RecvStream;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

/// Send a request on the main bi-stream and wait for the matching response.
pub async fn send_request(
    state: &AppState,
    request: ServerRequest,
) -> Result<ServerResponse> {
    let id = state.next_id();
    let (tx, rx) = tokio::sync::oneshot::channel();

    // Register the pending request
    {
        let mut pending = state.pending_requests.lock().unwrap();
        pending.insert(id, tx);
    }

    // Send the frame
    let frame = ClientFrame::Request { id, body: request };
    {
        let mut send_lock = state.send_stream.lock().unwrap();
        let send = send_lock.as_mut().ok_or_else(|| anyhow::anyhow!("not connected"))?;
        let data = codec::encode(&frame)?;
        // We need to write synchronously while holding the lock, but write_all is async.
        // Use a block_on approach or store the SendStream in a tokio Mutex instead.
        // For simplicity, we'll use a Vec buffer and send it.
        let len_bytes = (data.len() as u32).to_be_bytes();
        let mut buf = Vec::with_capacity(4 + data.len());
        buf.extend_from_slice(&len_bytes);
        buf.extend_from_slice(&data);
        // Store the buffer; the actual write needs to be async
        // This is a design tension — we need a tokio::sync::Mutex for SendStream
        drop(send_lock);
        // Actually, let's restructure: use tokio::sync::Mutex
    }

    // Wait for response
    let response = rx.await.map_err(|_| anyhow::anyhow!("response channel closed"))?;
    Ok(response)
}

/// Spawn background task that reads ServerFrames and routes them.
pub fn spawn_event_reader(
    app_handle: AppHandle,
    state: Arc<AppState>,
    mut recv: RecvStream,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let frame = match connection::recv_server_frame(&mut recv).await {
                Ok(f) => f,
                Err(e) => {
                    let _ = app_handle.emit("server:disconnected", serde_json::json!({ "reason": e.to_string() }));
                    *state.connected.lock().unwrap() = false;
                    break;
                }
            };

            match frame {
                ServerFrame::Response { request_id, body } => {
                    let mut pending = state.pending_requests.lock().unwrap();
                    if let Some(sender) = pending.remove(&request_id) {
                        let _ = sender.send(body);
                    }
                }
                ServerFrame::Event(event) => {
                    emit_server_event(&app_handle, event);
                }
                _ => {}
            }
        }
    })
}

fn emit_server_event(app: &AppHandle, event: ServerEvent) {
    let _ = match event {
        ServerEvent::NewMessage { message } => app.emit("server:new_message", &message),
        ServerEvent::MessageEdited { message_id, channel_id, new_content, edited_at } =>
            app.emit("server:message_edited", serde_json::json!({ "message_id": message_id, "channel_id": channel_id, "new_content": new_content, "edited_at": edited_at })),
        ServerEvent::MessageDeleted { message_id, channel_id } =>
            app.emit("server:message_deleted", serde_json::json!({ "message_id": message_id, "channel_id": channel_id })),
        ServerEvent::ReactionAdded { message_id, channel_id, emoji, public_key } =>
            app.emit("server:reaction_added", serde_json::json!({ "message_id": message_id, "channel_id": channel_id, "emoji": emoji, "public_key": public_key.to_string() })),
        ServerEvent::ReactionRemoved { message_id, channel_id, emoji, public_key } =>
            app.emit("server:reaction_removed", serde_json::json!({ "message_id": message_id, "channel_id": channel_id, "emoji": emoji, "public_key": public_key.to_string() })),
        ServerEvent::MemberJoined { public_key, display_name } =>
            app.emit("server:member_joined", serde_json::json!({ "public_key": public_key.to_string(), "display_name": display_name })),
        ServerEvent::MemberLeft { public_key } =>
            app.emit("server:member_left", serde_json::json!({ "public_key": public_key.to_string() })),
        ServerEvent::ChannelCreated { channel } => app.emit("server:channel_created", &channel),
        ServerEvent::ChannelUpdated { channel } => app.emit("server:channel_updated", &channel),
        ServerEvent::ChannelDeleted { channel_id } =>
            app.emit("server:channel_deleted", serde_json::json!({ "channel_id": channel_id })),
        ServerEvent::TypingStarted { channel_id, public_key } =>
            app.emit("server:typing", serde_json::json!({ "channel_id": channel_id, "public_key": public_key.to_string() })),
        _ => Ok(()),
    };
}
```

Note: The `send_request` function has a design tension — `SendStream` needs async writes but `std::sync::Mutex` can't be held across `.await`. The fix: use `tokio::sync::Mutex` for `send_stream` in `AppState`. Update `state.rs` accordingly:

```rust
pub send_stream: tokio::sync::Mutex<Option<SendStream>>,
```

And the send function becomes:

```rust
pub async fn send_request(state: &AppState, request: ServerRequest) -> Result<ServerResponse> {
    let id = state.next_id();
    let (tx, rx) = tokio::sync::oneshot::channel();
    { state.pending_requests.lock().unwrap().insert(id, tx); }

    let frame = ClientFrame::Request { id, body: request };
    let data = codec::encode(&frame)?;
    {
        let mut send = state.send_stream.lock().await;
        let stream = send.as_mut().ok_or_else(|| anyhow::anyhow!("not connected"))?;
        connection::write_frame(stream, &data).await?;
    }

    rx.await.map_err(|_| anyhow::anyhow!("response channel closed"))
}
```

- [ ] **Step 3: Rewrite commands.rs with server commands**

`client/src-tauri/src/commands.rs`:

```rust
use crate::{bridge, connection, state::AppState, tls};
use farder_crypto::identity::Keypair;
use farder_protocol::server::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, State};

// ── Serializable types for IPC ──────────────────────────────────────

#[derive(Serialize)]
pub struct ConnectResult {
    pub server_name: String,
    pub member_count: u32,
    pub channels: Vec<ChannelInfo>,
    pub categories: Vec<CategoryInfo>,
    pub roles: Vec<RoleInfo>,
}

#[derive(Serialize)]
pub struct SendMessageResult {
    pub id: u64,
    pub timestamp: u64,
}

// ── Identity commands (existing, updated) ───────────────────────────

#[tauri::command]
pub fn generate_keypair(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let keypair = Keypair::generate();
    let pk = keypair.public_key().to_string();
    *state.keypair.lock().unwrap() = Some(keypair);
    Ok(pk)
}

#[tauri::command]
pub fn get_public_key(state: State<'_, Arc<AppState>>) -> Result<Option<String>, String> {
    let kp = state.keypair.lock().unwrap();
    Ok(kp.as_ref().map(|k| k.public_key().to_string()))
}

// ── Server commands ─────────────────────────────────────────────────

#[tauri::command]
pub async fn connect_server(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    address: String,
    invite_code: Option<String>,
    setup_token: Option<String>,
) -> Result<ConnectResult, String> {
    let keypair = {
        let kp = state.keypair.lock().unwrap();
        kp.as_ref().ok_or("no identity — generate a keypair first")?.clone()
    };

    // Note: Keypair doesn't implement Clone. We need to work around this.
    // Store the signing key bytes and reconstruct, or change the approach.
    // For now, the keypair is consumed. We need to re-think state management here.
    // The simplest fix: have generate_keypair store the raw key bytes, and reconstruct as needed.

    let endpoint = tls::make_client_endpoint().map_err(|e| e.to_string())?;
    let (conn, send, recv, _session_token) = connection::connect_and_authenticate(
        &endpoint,
        &address,
        &keypair,
        invite_code.as_deref(),
        setup_token.as_deref(),
    ).await.map_err(|e| e.to_string())?;

    // Store send stream
    *state.send_stream.lock().await = Some(send);
    *state.connected.lock().unwrap() = true;

    // Spawn event reader
    let handle = bridge::spawn_event_reader(app, Arc::clone(&state), recv);
    *state.event_reader_handle.lock().unwrap() = Some(handle);

    // Fetch server info
    let info = bridge::send_request(&state, ServerRequest::GetServerInfo)
        .await
        .map_err(|e| e.to_string())?;

    match info {
        ServerResponse::ServerInfo { name, member_count, channels, categories, roles } => {
            Ok(ConnectResult { server_name: name, member_count, channels, categories, roles })
        }
        ServerResponse::Error { reason } => Err(reason),
        _ => Err("unexpected response".to_string()),
    }
}

#[tauri::command]
pub async fn disconnect_server(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    *state.send_stream.lock().await = None;
    *state.connected.lock().unwrap() = false;
    if let Some(handle) = state.event_reader_handle.lock().unwrap().take() {
        handle.abort();
    }
    Ok(())
}

#[tauri::command]
pub async fn send_message(
    state: State<'_, Arc<AppState>>,
    channel_id: u64,
    content: String,
    reply_to: Option<u64>,
) -> Result<SendMessageResult, String> {
    let resp = bridge::send_request(&state, ServerRequest::SendMessage {
        channel_id, content, reply_to, attachment_ids: vec![],
    }).await.map_err(|e| e.to_string())?;
    match resp {
        ServerResponse::MessageSent { id, timestamp } => Ok(SendMessageResult { id, timestamp }),
        ServerResponse::Error { reason } => Err(reason),
        _ => Err("unexpected response".to_string()),
    }
}

#[tauri::command]
pub async fn fetch_history(
    state: State<'_, Arc<AppState>>,
    channel_id: u64,
    before_id: Option<u64>,
    limit: Option<u32>,
) -> Result<Vec<MessageInfo>, String> {
    let resp = bridge::send_request(&state, ServerRequest::FetchHistory {
        channel_id, before_id, limit: limit.unwrap_or(50),
    }).await.map_err(|e| e.to_string())?;
    match resp {
        ServerResponse::History { messages } => Ok(messages),
        ServerResponse::Error { reason } => Err(reason),
        _ => Err("unexpected response".to_string()),
    }
}

#[tauri::command]
pub async fn subscribe_channels(
    state: State<'_, Arc<AppState>>,
    channel_ids: Vec<u64>,
) -> Result<(), String> {
    let resp = bridge::send_request(&state, ServerRequest::Subscribe { channel_ids })
        .await.map_err(|e| e.to_string())?;
    match resp {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        _ => Err("unexpected response".to_string()),
    }
}

#[tauri::command]
pub async fn get_members(state: State<'_, Arc<AppState>>) -> Result<Vec<MemberInfo>, String> {
    let resp = bridge::send_request(&state, ServerRequest::GetMembers)
        .await.map_err(|e| e.to_string())?;
    match resp {
        ServerResponse::Members { members } => Ok(members),
        ServerResponse::Error { reason } => Err(reason),
        _ => Err("unexpected response".to_string()),
    }
}

#[tauri::command]
pub async fn add_reaction(
    state: State<'_, Arc<AppState>>,
    message_id: u64,
    emoji: String,
) -> Result<(), String> {
    let resp = bridge::send_request(&state, ServerRequest::AddReaction { message_id, emoji })
        .await.map_err(|e| e.to_string())?;
    match resp {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        _ => Err("unexpected response".to_string()),
    }
}

#[tauri::command]
pub async fn remove_reaction(
    state: State<'_, Arc<AppState>>,
    message_id: u64,
    emoji: String,
) -> Result<(), String> {
    let resp = bridge::send_request(&state, ServerRequest::RemoveReaction { message_id, emoji })
        .await.map_err(|e| e.to_string())?;
    match resp {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        _ => Err("unexpected response".to_string()),
    }
}

#[tauri::command]
pub async fn create_thread(
    state: State<'_, Arc<AppState>>,
    message_id: u64,
    name: Option<String>,
) -> Result<(), String> {
    let resp = bridge::send_request(&state, ServerRequest::CreateThread { message_id, name })
        .await.map_err(|e| e.to_string())?;
    match resp {
        ServerResponse::Ok => Ok(()),
        ServerResponse::Error { reason } => Err(reason),
        _ => Err("unexpected response".to_string()),
    }
}
```

- [ ] **Step 4: Rewrite main.rs**

`client/src-tauri/src/main.rs`:

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod bridge;
mod commands;
mod connection;
mod state;
mod tls;

use state::AppState;
use std::sync::Arc;

fn main() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    tauri::Builder::default()
        .manage(Arc::new(AppState::new()))
        .invoke_handler(tauri::generate_handler![
            commands::generate_keypair,
            commands::get_public_key,
            commands::connect_server,
            commands::disconnect_server,
            commands::send_message,
            commands::fetch_history,
            commands::subscribe_channels,
            commands::get_members,
            commands::add_reaction,
            commands::remove_reaction,
            commands::create_thread,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

- [ ] **Step 5: Verify compilation**

Run: `cd /home/deez/farder/client/src-tauri && cargo check`

Note: There will likely be compilation issues around `Keypair` not implementing `Clone`, `SendStream` mutex types, etc. The implementer should resolve these by:
- Storing keypair as raw `[u8; 32]` bytes and reconstructing `Keypair` from `SigningKey::from_bytes` when needed
- Using `tokio::sync::Mutex` for `send_stream` (async-safe)
- Ensuring `farder_crypto::identity::Keypair` has a way to reconstruct from stored bytes (it has `signing_key_bytes()` and can be reconstructed via `Keypair::import_encrypted` or by adding a `from_signing_key_bytes` constructor)

- [ ] **Step 6: Commit**

```bash
git add client/src-tauri/
git commit -m "feat(client): implement Tauri backend with server connection, IPC commands, and event bridge"
```

---

## Task 3: TypeScript Types & Tauri Bridge

**Files:**
- Create: `client/src/lib/types.ts`
- Rewrite: `client/src/lib/tauri-bridge.ts`

- [ ] **Step 1: Create TypeScript types matching protocol**

`client/src/lib/types.ts`:

```typescript
export interface ChannelInfo {
  id: number;
  name: string;
  channel_type: "Text" | "Announcement" | "Thread";
  category_id: number | null;
  position: number;
  topic: string | null;
  nsfw: boolean;
  slow_mode_secs: number;
  retention_secs: number | null;
  thread_parent_message_id: number | null;
}

export interface CategoryInfo {
  id: number;
  name: string;
  position: number;
}

export interface RoleInfo {
  id: number;
  name: string;
  permissions: number;
  color: string | null;
  position: number;
}

export interface MemberInfo {
  public_key: { bytes: number[] };
  display_name: string;
  joined_at: number;
  role_ids: number[];
}

export interface AttachmentInfo {
  id: number;
  file_id: number;
  name: string;
  size: number;
  mime_type: string;
  width: number | null;
  height: number | null;
  duration_secs: number | null;
}

export interface ReactionGroup {
  emoji: string;
  count: number;
  me: boolean;
}

export interface MessageInfo {
  id: number;
  channel_id: number;
  author: { bytes: number[] };
  content: string;
  timestamp: number;
  edited_at: number | null;
  reply_to: number | null;
  pinned: boolean;
  attachments: AttachmentInfo[];
  reactions: ReactionGroup[];
  thread_id: number | null;
  thread_message_count: number | null;
}

export interface ConnectResult {
  server_name: string;
  member_count: number;
  channels: ChannelInfo[];
  categories: CategoryInfo[];
  roles: RoleInfo[];
}

export interface SendMessageResult {
  id: number;
  timestamp: number;
}

// Sentinel key for deleted users (all zeros)
export const DELETED_USER_KEY = new Array(32).fill(0);

export function publicKeyToString(pk: { bytes: number[] }): string {
  return "vk_" + pk.bytes.map(b => b.toString(16).padStart(2, "0")).join("");
}

export function isDeletedUser(pk: { bytes: number[] }): boolean {
  return pk.bytes.every(b => b === 0);
}
```

- [ ] **Step 2: Rewrite tauri-bridge.ts with all commands**

`client/src/lib/tauri-bridge.ts`:

```typescript
import { invoke } from "@tauri-apps/api/core";
import type { ConnectResult, MessageInfo, MemberInfo, SendMessageResult } from "./types";

export async function generateKeypair(): Promise<string> {
  return invoke<string>("generate_keypair");
}

export async function getPublicKey(): Promise<string | null> {
  return invoke<string | null>("get_public_key");
}

export async function connectServer(
  address: string,
  inviteCode?: string,
  setupToken?: string
): Promise<ConnectResult> {
  return invoke<ConnectResult>("connect_server", {
    address,
    inviteCode: inviteCode || null,
    setupToken: setupToken || null,
  });
}

export async function disconnectServer(): Promise<void> {
  return invoke("disconnect_server");
}

export async function sendMessage(
  channelId: number,
  content: string,
  replyTo?: number
): Promise<SendMessageResult> {
  return invoke<SendMessageResult>("send_message", {
    channelId,
    content,
    replyTo: replyTo || null,
  });
}

export async function fetchHistory(
  channelId: number,
  beforeId?: number,
  limit?: number
): Promise<MessageInfo[]> {
  return invoke<MessageInfo[]>("fetch_history", {
    channelId,
    beforeId: beforeId || null,
    limit: limit || null,
  });
}

export async function subscribeChannels(channelIds: number[]): Promise<void> {
  return invoke("subscribe_channels", { channelIds });
}

export async function getMembers(): Promise<MemberInfo[]> {
  return invoke<MemberInfo[]>("get_members");
}

export async function addReaction(messageId: number, emoji: string): Promise<void> {
  return invoke("add_reaction", { messageId, emoji });
}

export async function removeReaction(messageId: number, emoji: string): Promise<void> {
  return invoke("remove_reaction", { messageId, emoji });
}

export async function createThread(messageId: number, name?: string): Promise<void> {
  return invoke("create_thread", { messageId, name: name || null });
}
```

- [ ] **Step 3: Commit**

```bash
git add client/src/lib/
git commit -m "feat(client): add TypeScript types and typed Tauri IPC bridge"
```

---

## Task 4: XP Luna Theme CSS

**Files:**
- Create: `client/src/styles/xp-theme.css`

- [ ] **Step 1: Create the XP Luna Blue theme**

`client/src/styles/xp-theme.css`:

```css
/* ── XP Luna Blue Theme for Farder ─────────────────────────────── */

* { margin: 0; padding: 0; box-sizing: border-box; }

:root {
  --xp-blue-dark: #003C74;
  --xp-blue: #0058E6;
  --xp-blue-light: #3389FF;
  --xp-sidebar-dark: #1941A5;
  --xp-sidebar: #3169C6;
  --xp-sidebar-light: #4A7FD4;
  --xp-window-bg: #ECE9D8;
  --xp-panel-bg: #F1EFE2;
  --xp-border: #ACA899;
  --xp-border-dark: #716F64;
  --xp-input-border: #7F9DB9;
  --xp-text: #000000;
  --xp-text-secondary: #555555;
  --xp-text-muted: #888888;
  --xp-white: #FFFFFF;
  --xp-link: #0066CC;
  --xp-green: #00B300;
  --xp-red: #CC0000;
  --xp-font: "Tahoma", "Segoe UI", sans-serif;
  --xp-font-size: 11px;
  --xp-font-size-small: 10px;
}

html, body, #root {
  height: 100%;
  width: 100%;
  overflow: hidden;
  font-family: var(--xp-font);
  font-size: var(--xp-font-size);
  color: var(--xp-text);
  background: var(--xp-window-bg);
  user-select: none;
}

/* ── Title Bar ─────────────────────────────────────────────────── */

.titlebar {
  height: 30px;
  background: linear-gradient(180deg, var(--xp-blue-light) 0%, var(--xp-blue) 45%, var(--xp-blue-dark) 100%);
  display: flex;
  align-items: center;
  padding: 0 4px;
  -webkit-app-region: drag;
}

.titlebar-title {
  color: white;
  font-weight: bold;
  font-size: 12px;
  padding-left: 6px;
  text-shadow: 1px 1px 2px rgba(0,0,0,0.3);
  flex: 1;
}

.titlebar-buttons {
  display: flex;
  gap: 2px;
  -webkit-app-region: no-drag;
}

.titlebar-btn {
  width: 21px;
  height: 21px;
  border: 1px solid rgba(255,255,255,0.3);
  border-radius: 3px;
  background: linear-gradient(180deg, #4E91D9 0%, #2567B8 100%);
  color: white;
  font-size: 10px;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
}
.titlebar-btn:hover { background: linear-gradient(180deg, #6EAAEE 0%, #3577C8 100%); }
.titlebar-btn.close { background: linear-gradient(180deg, #E08356 0%, #C5512F 100%); }
.titlebar-btn.close:hover { background: linear-gradient(180deg, #F09868 0%, #D56A45 100%); }

/* ── Main Layout ───────────────────────────────────────────────── */

.main-layout {
  display: flex;
  height: calc(100% - 30px);
}

/* ── Channel Sidebar ───────────────────────────────────────────── */

.channel-sidebar {
  width: 200px;
  background: linear-gradient(180deg, var(--xp-sidebar) 0%, var(--xp-sidebar-dark) 100%);
  color: white;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.server-header {
  padding: 10px 12px;
  font-weight: bold;
  font-size: 13px;
  border-bottom: 1px solid var(--xp-sidebar-dark);
  text-shadow: 1px 1px 2px rgba(0,0,0,0.3);
}

.category-name {
  padding: 6px 10px 2px;
  font-size: var(--xp-font-size-small);
  color: #B8D4FF;
  text-transform: uppercase;
  letter-spacing: 0.5px;
}

.channel-list {
  flex: 1;
  overflow-y: auto;
}

.channel-item {
  padding: 4px 12px 4px 16px;
  cursor: pointer;
  display: flex;
  align-items: center;
  gap: 4px;
}
.channel-item:hover { background: rgba(255,255,255,0.1); }
.channel-item.active { background: rgba(255,255,255,0.2); font-weight: bold; }
.channel-item::before { content: "#"; opacity: 0.6; margin-right: 2px; }
.channel-item.thread::before { content: "↳"; }
.channel-item.announcement::before { content: "📢"; }

.user-footer {
  padding: 8px 10px;
  background: var(--xp-sidebar-dark);
  border-top: 1px solid #0D2E7A;
  font-size: var(--xp-font-size-small);
  display: flex;
  align-items: center;
  gap: 6px;
}
.user-footer .online-dot { color: var(--xp-green); }

/* ── Chat Panel ────────────────────────────────────────────────── */

.chat-panel {
  flex: 1;
  display: flex;
  flex-direction: column;
  background: var(--xp-white);
}

.channel-header {
  padding: 8px 12px;
  background: var(--xp-window-bg);
  border-bottom: 1px solid var(--xp-border);
  font-weight: bold;
}
.channel-header .topic {
  font-weight: normal;
  color: var(--xp-text-secondary);
  font-size: var(--xp-font-size-small);
}

.message-list {
  flex: 1;
  overflow-y: auto;
  padding: 8px 12px;
}

.message {
  margin-bottom: 8px;
  line-height: 1.4;
}

.message-header {
  display: flex;
  align-items: baseline;
  gap: 6px;
}

.message-author {
  font-weight: bold;
  cursor: pointer;
}

.message-timestamp {
  font-size: var(--xp-font-size-small);
  color: var(--xp-text-muted);
}

.message-content {
  margin-top: 2px;
  word-wrap: break-word;
}

.message-content.deleted {
  color: var(--xp-text-muted);
  font-style: italic;
}

.reaction-bar {
  display: flex;
  gap: 4px;
  margin-top: 4px;
  flex-wrap: wrap;
}

.reaction {
  display: flex;
  align-items: center;
  gap: 3px;
  padding: 1px 6px;
  border: 1px solid var(--xp-border);
  border-radius: 3px;
  background: var(--xp-panel-bg);
  cursor: pointer;
  font-size: var(--xp-font-size-small);
}
.reaction:hover { background: #E5E2D5; }
.reaction.me { border-color: var(--xp-blue); background: #E8F0FC; }

.thread-link {
  margin-top: 4px;
  font-size: var(--xp-font-size-small);
  color: var(--xp-link);
  cursor: pointer;
}
.thread-link:hover { text-decoration: underline; }

/* ── Message Input ─────────────────────────────────────────────── */

.message-input-area {
  padding: 8px;
  background: var(--xp-window-bg);
  border-top: 1px solid var(--xp-border);
  display: flex;
  gap: 6px;
}

.message-input {
  flex: 1;
  padding: 4px 6px;
  border: 1px solid var(--xp-input-border);
  border-radius: 0;
  font-family: var(--xp-font);
  font-size: var(--xp-font-size);
  outline: none;
}
.message-input:focus { border-color: var(--xp-blue); }

.xp-button {
  padding: 3px 12px;
  background: var(--xp-window-bg);
  border: 1px solid var(--xp-border-dark);
  border-radius: 3px;
  font-family: var(--xp-font);
  font-size: var(--xp-font-size);
  cursor: pointer;
  background: linear-gradient(180deg, #FFFFFF 0%, #ECE9D8 50%, #D6D2C4 100%);
}
.xp-button:hover { background: linear-gradient(180deg, #FFFFFF 0%, #F0EDE3 50%, #E2DED4 100%); }
.xp-button:active { background: linear-gradient(180deg, #D6D2C4 0%, #ECE9D8 100%); }

/* ── Member Sidebar ────────────────────────────────────────────── */

.member-sidebar {
  width: 180px;
  background: var(--xp-panel-bg);
  border-left: 1px solid var(--xp-border);
  overflow-y: auto;
  padding: 8px;
}

.role-group-header {
  font-size: var(--xp-font-size-small);
  color: var(--xp-text-secondary);
  padding: 4px 0 2px;
  text-transform: uppercase;
}

.member-item {
  padding: 2px 4px;
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: var(--xp-font-size);
}
.member-item .online-dot { font-size: 8px; }
.member-item .online { color: var(--xp-green); }
.member-item .offline { color: var(--xp-text-muted); }

/* ── Connect Dialog ────────────────────────────────────────────── */

.connect-dialog {
  display: flex;
  align-items: center;
  justify-content: center;
  height: 100%;
  background: linear-gradient(135deg, #1941A5 0%, #3169C6 50%, #4A7FD4 100%);
}

.connect-box {
  background: var(--xp-window-bg);
  border: 2px solid var(--xp-blue);
  border-radius: 8px;
  padding: 24px;
  width: 380px;
  box-shadow: 4px 4px 12px rgba(0,0,0,0.3);
}

.connect-box h2 {
  text-align: center;
  margin-bottom: 16px;
  font-size: 16px;
}

.connect-box label {
  display: block;
  margin-bottom: 4px;
  font-weight: bold;
  font-size: var(--xp-font-size);
}

.connect-box input {
  width: 100%;
  padding: 4px 6px;
  margin-bottom: 12px;
  border: 1px solid var(--xp-input-border);
  font-family: var(--xp-font);
  font-size: var(--xp-font-size);
}

.connect-box .identity-section {
  background: var(--xp-panel-bg);
  border: 1px solid var(--xp-border);
  padding: 8px;
  margin-bottom: 12px;
  font-size: var(--xp-font-size-small);
}

.connect-box .pk-display {
  font-family: monospace;
  font-size: 10px;
  word-break: break-all;
  color: var(--xp-text-secondary);
}

.error-text { color: var(--xp-red); font-size: var(--xp-font-size-small); margin-bottom: 8px; }

/* ── Emoji Picker ──────────────────────────────────────────────── */

.reaction-picker {
  position: absolute;
  background: var(--xp-window-bg);
  border: 1px solid var(--xp-border-dark);
  border-radius: 3px;
  padding: 6px;
  display: grid;
  grid-template-columns: repeat(8, 1fr);
  gap: 2px;
  box-shadow: 2px 2px 6px rgba(0,0,0,0.2);
  z-index: 100;
}

.reaction-picker button {
  width: 28px;
  height: 28px;
  border: none;
  background: none;
  cursor: pointer;
  font-size: 16px;
  border-radius: 3px;
}
.reaction-picker button:hover { background: #D6D2C4; }

/* ── Scrollbar (WebKit) ────────────────────────────────────────── */

::-webkit-scrollbar { width: 16px; }
::-webkit-scrollbar-track { background: var(--xp-panel-bg); border-left: 1px solid var(--xp-border); }
::-webkit-scrollbar-thumb {
  background: linear-gradient(90deg, #C8C5BA 0%, #ACA899 100%);
  border: 1px solid var(--xp-border);
  border-radius: 0;
}
::-webkit-scrollbar-thumb:hover { background: #B8B5AA; }
::-webkit-scrollbar-button { height: 16px; background: var(--xp-window-bg); border: 1px solid var(--xp-border); }
```

- [ ] **Step 2: Import theme in main.tsx**

Update `client/src/main.tsx`:

```tsx
import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles/xp-theme.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
```

- [ ] **Step 3: Commit**

```bash
git add client/src/styles/ client/src/main.tsx
git commit -m "feat(client): add Windows XP Luna Blue theme CSS"
```

---

## Task 5: React Context & Event Hooks

**Files:**
- Create: `client/src/context/ServerContext.tsx`
- Create: `client/src/hooks/useServerEvents.ts`

- [ ] **Step 1: Create ServerContext with reducer**

`client/src/context/ServerContext.tsx`:

```tsx
import { createContext, useContext, useReducer, ReactNode } from "react";
import type { ChannelInfo, CategoryInfo, RoleInfo, MemberInfo, MessageInfo } from "../lib/types";

interface ServerState {
  connected: boolean;
  serverName: string;
  channels: ChannelInfo[];
  categories: CategoryInfo[];
  roles: RoleInfo[];
  members: MemberInfo[];
  currentChannelId: number | null;
  messages: Record<number, MessageInfo[]>;  // channelId -> messages
  threadChannelId: number | null;  // if viewing a thread
}

type Action =
  | { type: "CONNECTED"; serverName: string; channels: ChannelInfo[]; categories: CategoryInfo[]; roles: RoleInfo[] }
  | { type: "DISCONNECTED" }
  | { type: "SET_MEMBERS"; members: MemberInfo[] }
  | { type: "SELECT_CHANNEL"; channelId: number }
  | { type: "SET_MESSAGES"; channelId: number; messages: MessageInfo[] }
  | { type: "NEW_MESSAGE"; message: MessageInfo }
  | { type: "MESSAGE_EDITED"; messageId: number; channelId: number; newContent: string; editedAt: number }
  | { type: "MESSAGE_DELETED"; messageId: number; channelId: number }
  | { type: "REACTION_ADDED"; messageId: number; channelId: number; emoji: string; publicKey: string }
  | { type: "REACTION_REMOVED"; messageId: number; channelId: number; emoji: string; publicKey: string }
  | { type: "MEMBER_JOINED"; publicKey: string; displayName: string }
  | { type: "MEMBER_LEFT"; publicKey: string }
  | { type: "CHANNEL_CREATED"; channel: ChannelInfo }
  | { type: "CHANNEL_DELETED"; channelId: number }
  | { type: "VIEW_THREAD"; channelId: number | null }
  | { type: "PREPEND_MESSAGES"; channelId: number; messages: MessageInfo[] };

const initialState: ServerState = {
  connected: false,
  serverName: "",
  channels: [],
  categories: [],
  roles: [],
  members: [],
  currentChannelId: null,
  messages: {},
  threadChannelId: null,
};

function reducer(state: ServerState, action: Action): ServerState {
  switch (action.type) {
    case "CONNECTED":
      return { ...state, connected: true, serverName: action.serverName, channels: action.channels, categories: action.categories, roles: action.roles };
    case "DISCONNECTED":
      return { ...initialState };
    case "SET_MEMBERS":
      return { ...state, members: action.members };
    case "SELECT_CHANNEL":
      return { ...state, currentChannelId: action.channelId, threadChannelId: null };
    case "SET_MESSAGES":
      return { ...state, messages: { ...state.messages, [action.channelId]: action.messages } };
    case "PREPEND_MESSAGES": {
      const existing = state.messages[action.channelId] || [];
      return { ...state, messages: { ...state.messages, [action.channelId]: [...action.messages, ...existing] } };
    }
    case "NEW_MESSAGE": {
      const chId = action.message.channel_id;
      const existing = state.messages[chId] || [];
      return { ...state, messages: { ...state.messages, [chId]: [...existing, action.message] } };
    }
    case "MESSAGE_EDITED": {
      const msgs = (state.messages[action.channelId] || []).map(m =>
        m.id === action.messageId ? { ...m, content: action.newContent, edited_at: action.editedAt } : m
      );
      return { ...state, messages: { ...state.messages, [action.channelId]: msgs } };
    }
    case "MESSAGE_DELETED": {
      const msgs = (state.messages[action.channelId] || []).filter(m => m.id !== action.messageId);
      return { ...state, messages: { ...state.messages, [action.channelId]: msgs } };
    }
    case "REACTION_ADDED": {
      const msgs = (state.messages[action.channelId] || []).map(m => {
        if (m.id !== action.messageId) return m;
        const existing = m.reactions.find(r => r.emoji === action.emoji);
        if (existing) {
          return { ...m, reactions: m.reactions.map(r => r.emoji === action.emoji ? { ...r, count: r.count + 1 } : r) };
        }
        return { ...m, reactions: [...m.reactions, { emoji: action.emoji, count: 1, me: false }] };
      });
      return { ...state, messages: { ...state.messages, [action.channelId]: msgs } };
    }
    case "REACTION_REMOVED": {
      const msgs = (state.messages[action.channelId] || []).map(m => {
        if (m.id !== action.messageId) return m;
        return { ...m, reactions: m.reactions.map(r => r.emoji === action.emoji ? { ...r, count: Math.max(0, r.count - 1) } : r).filter(r => r.count > 0) };
      });
      return { ...state, messages: { ...state.messages, [action.channelId]: msgs } };
    }
    case "MEMBER_JOINED":
      return { ...state, members: [...state.members, { public_key: { bytes: [] }, display_name: action.displayName, joined_at: 0, role_ids: [] } as any] };
    case "MEMBER_LEFT":
      return { ...state, members: state.members.filter(m => m.display_name !== action.publicKey) };
    case "CHANNEL_CREATED":
      return { ...state, channels: [...state.channels, action.channel] };
    case "CHANNEL_DELETED":
      return { ...state, channels: state.channels.filter(c => c.id !== action.channelId) };
    case "VIEW_THREAD":
      return { ...state, threadChannelId: action.channelId };
    default:
      return state;
  }
}

const ServerContext = createContext<{ state: ServerState; dispatch: React.Dispatch<Action> } | null>(null);

export function ServerProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(reducer, initialState);
  return <ServerContext.Provider value={{ state, dispatch }}>{children}</ServerContext.Provider>;
}

export function useServer() {
  const ctx = useContext(ServerContext);
  if (!ctx) throw new Error("useServer must be used within ServerProvider");
  return ctx;
}
```

- [ ] **Step 2: Create event listener hook**

`client/src/hooks/useServerEvents.ts`:

```typescript
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useServer } from "../context/ServerContext";

export function useServerEvents() {
  const { dispatch } = useServer();

  useEffect(() => {
    const unlisten: (() => void)[] = [];

    (async () => {
      unlisten.push(await listen("server:new_message", (e) => {
        dispatch({ type: "NEW_MESSAGE", message: e.payload as any });
      }));
      unlisten.push(await listen("server:message_edited", (e) => {
        const p = e.payload as any;
        dispatch({ type: "MESSAGE_EDITED", messageId: p.message_id, channelId: p.channel_id, newContent: p.new_content, editedAt: p.edited_at });
      }));
      unlisten.push(await listen("server:message_deleted", (e) => {
        const p = e.payload as any;
        dispatch({ type: "MESSAGE_DELETED", messageId: p.message_id, channelId: p.channel_id });
      }));
      unlisten.push(await listen("server:reaction_added", (e) => {
        const p = e.payload as any;
        dispatch({ type: "REACTION_ADDED", messageId: p.message_id, channelId: p.channel_id, emoji: p.emoji, publicKey: p.public_key });
      }));
      unlisten.push(await listen("server:reaction_removed", (e) => {
        const p = e.payload as any;
        dispatch({ type: "REACTION_REMOVED", messageId: p.message_id, channelId: p.channel_id, emoji: p.emoji, publicKey: p.public_key });
      }));
      unlisten.push(await listen("server:member_joined", (e) => {
        const p = e.payload as any;
        dispatch({ type: "MEMBER_JOINED", publicKey: p.public_key, displayName: p.display_name });
      }));
      unlisten.push(await listen("server:member_left", (e) => {
        const p = e.payload as any;
        dispatch({ type: "MEMBER_LEFT", publicKey: p.public_key });
      }));
      unlisten.push(await listen("server:channel_created", (e) => {
        dispatch({ type: "CHANNEL_CREATED", channel: e.payload as any });
      }));
      unlisten.push(await listen("server:channel_deleted", (e) => {
        const p = e.payload as any;
        dispatch({ type: "CHANNEL_DELETED", channelId: p.channel_id });
      }));
      unlisten.push(await listen("server:disconnected", () => {
        dispatch({ type: "DISCONNECTED" });
      }));
    })();

    return () => { unlisten.forEach(fn => fn()); };
  }, [dispatch]);
}
```

- [ ] **Step 3: Commit**

```bash
git add client/src/context/ client/src/hooks/
git commit -m "feat(client): add React context with reducer and Tauri event listener hook"
```

---

## Task 6: React Components — Shell & Connect Dialog

**Files:**
- Rewrite: `client/src/App.tsx`
- Create: `client/src/components/ConnectDialog.tsx`
- Create: `client/src/components/AppShell.tsx`
- Create: `client/src/components/TitleBar.tsx`
- Delete: `client/src/components/Setup.tsx`, `Chat.tsx`, `Contacts.tsx`, `Settings.tsx`

- [ ] **Step 1: Create TitleBar**

`client/src/components/TitleBar.tsx`:

```tsx
interface TitleBarProps { title: string; }

export default function TitleBar({ title }: TitleBarProps) {
  return (
    <div className="titlebar">
      <span className="titlebar-title">{title}</span>
      <div className="titlebar-buttons">
        <button className="titlebar-btn">—</button>
        <button className="titlebar-btn">□</button>
        <button className="titlebar-btn close">✕</button>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Create ConnectDialog**

`client/src/components/ConnectDialog.tsx`:

```tsx
import { useState, useEffect } from "react";
import { useServer } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";

export default function ConnectDialog() {
  const { dispatch } = useServer();
  const [publicKey, setPublicKey] = useState<string | null>(null);
  const [address, setAddress] = useState("127.0.0.1:4435");
  const [inviteCode, setInviteCode] = useState("");
  const [setupToken, setSetupToken] = useState("");
  const [error, setError] = useState("");
  const [connecting, setConnecting] = useState(false);

  useEffect(() => {
    api.getPublicKey().then(pk => setPublicKey(pk));
  }, []);

  async function handleGenerateKey() {
    try {
      const pk = await api.generateKeypair();
      setPublicKey(pk);
    } catch (e) { setError(String(e)); }
  }

  async function handleConnect() {
    if (!publicKey) { setError("Generate an identity first"); return; }
    setError("");
    setConnecting(true);
    try {
      const result = await api.connectServer(
        address,
        inviteCode || undefined,
        setupToken || undefined,
      );
      const members = await api.getMembers();
      dispatch({
        type: "CONNECTED",
        serverName: result.server_name,
        channels: result.channels,
        categories: result.categories,
        roles: result.roles,
      });
      dispatch({ type: "SET_MEMBERS", members });
    } catch (e) {
      setError(String(e));
    } finally {
      setConnecting(false);
    }
  }

  return (
    <div className="connect-dialog">
      <div className="connect-box">
        <h2>Farder</h2>

        <div className="identity-section">
          {publicKey ? (
            <>
              <div>Identity:</div>
              <div className="pk-display">{publicKey}</div>
            </>
          ) : (
            <button className="xp-button" onClick={handleGenerateKey}>Generate Identity</button>
          )}
        </div>

        <label>Server Address</label>
        <input value={address} onChange={e => setAddress(e.target.value)} placeholder="127.0.0.1:4435" />

        <label>Invite Code (for joining)</label>
        <input value={inviteCode} onChange={e => setInviteCode(e.target.value)} placeholder="Optional" />

        <label>Setup Token (for first-run owner)</label>
        <input value={setupToken} onChange={e => setSetupToken(e.target.value)} placeholder="Optional" />

        {error && <div className="error-text">{error}</div>}

        <button className="xp-button" onClick={handleConnect} disabled={connecting} style={{ width: "100%", marginTop: 8 }}>
          {connecting ? "Connecting..." : "Connect"}
        </button>
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Create AppShell (placeholder panels)**

`client/src/components/AppShell.tsx`:

```tsx
import TitleBar from "./TitleBar";
import ChannelSidebar from "./ChannelSidebar";
import ChatPanel from "./ChatPanel";
import MemberSidebar from "./MemberSidebar";
import { useServer } from "../context/ServerContext";

export default function AppShell() {
  const { state } = useServer();
  return (
    <>
      <TitleBar title={`${state.serverName} — Farder`} />
      <div className="main-layout">
        <ChannelSidebar />
        <ChatPanel />
        <MemberSidebar />
      </div>
    </>
  );
}
```

- [ ] **Step 4: Rewrite App.tsx**

`client/src/App.tsx`:

```tsx
import { ServerProvider, useServer } from "./context/ServerContext";
import { useServerEvents } from "./hooks/useServerEvents";
import ConnectDialog from "./components/ConnectDialog";
import AppShell from "./components/AppShell";

function AppInner() {
  const { state } = useServer();
  useServerEvents();
  return state.connected ? <AppShell /> : <ConnectDialog />;
}

export default function App() {
  return (
    <ServerProvider>
      <AppInner />
    </ServerProvider>
  );
}
```

- [ ] **Step 5: Delete old scaffold files**

Delete: `client/src/components/Setup.tsx`, `client/src/components/Chat.tsx`, `client/src/components/Contacts.tsx`, `client/src/components/Settings.tsx`

- [ ] **Step 6: Commit**

```bash
git add client/src/
git commit -m "feat(client): add XP-themed connect dialog, app shell, and title bar components"
```

---

## Task 7: React Components — Channel Sidebar & Member List

**Files:**
- Create: `client/src/components/ChannelSidebar.tsx`
- Create: `client/src/components/MemberSidebar.tsx`

- [ ] **Step 1: Create ChannelSidebar**

`client/src/components/ChannelSidebar.tsx`:

```tsx
import { useServer } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";
import { publicKeyToString } from "../lib/types";
import { useEffect, useState } from "react";

export default function ChannelSidebar() {
  const { state, dispatch } = useServer();
  const [publicKey, setPublicKey] = useState("");

  useEffect(() => {
    api.getPublicKey().then(pk => { if (pk) setPublicKey(pk); });
  }, []);

  const nonThreadChannels = state.channels.filter(c => c.channel_type !== "Thread");

  async function selectChannel(channelId: number) {
    dispatch({ type: "SELECT_CHANNEL", channelId });
    dispatch({ type: "VIEW_THREAD", channelId: null });
    await api.subscribeChannels([channelId]);
    const messages = await api.fetchHistory(channelId);
    dispatch({ type: "SET_MESSAGES", channelId, messages: messages.reverse() });
  }

  // Group channels by category
  const uncategorized = nonThreadChannels.filter(c => c.category_id === null);
  const byCategory = state.categories.map(cat => ({
    category: cat,
    channels: nonThreadChannels.filter(c => c.category_id === cat.id),
  }));

  return (
    <div className="channel-sidebar">
      <div className="server-header">{state.serverName}</div>
      <div className="channel-list">
        {uncategorized.length > 0 && (
          <>
            {uncategorized.map(ch => (
              <div
                key={ch.id}
                className={`channel-item ${ch.channel_type === "Announcement" ? "announcement" : ""} ${state.currentChannelId === ch.id ? "active" : ""}`}
                onClick={() => selectChannel(ch.id)}
              >
                {ch.name}
              </div>
            ))}
          </>
        )}
        {byCategory.map(({ category, channels }) => (
          <div key={category.id}>
            <div className="category-name">{category.name}</div>
            {channels.map(ch => (
              <div
                key={ch.id}
                className={`channel-item ${ch.channel_type === "Announcement" ? "announcement" : ""} ${state.currentChannelId === ch.id ? "active" : ""}`}
                onClick={() => selectChannel(ch.id)}
              >
                {ch.name}
              </div>
            ))}
          </div>
        ))}
      </div>
      <div className="user-footer">
        <span className="online-dot">●</span>
        <span>{publicKey ? publicKey.substring(0, 14) + "..." : "..."}</span>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Create MemberSidebar**

`client/src/components/MemberSidebar.tsx`:

```tsx
import { useServer } from "../context/ServerContext";
import { publicKeyToString } from "../lib/types";

export default function MemberSidebar() {
  const { state } = useServer();

  // Group by highest role
  const roleMap = new Map(state.roles.map(r => [r.id, r]));
  const sorted = [...state.members].sort((a, b) => {
    const aMaxPos = Math.max(0, ...a.role_ids.map(id => roleMap.get(id)?.position ?? 0));
    const bMaxPos = Math.max(0, ...b.role_ids.map(id => roleMap.get(id)?.position ?? 0));
    return bMaxPos - aMaxPos;
  });

  return (
    <div className="member-sidebar">
      <div className="role-group-header">Members — {state.members.length}</div>
      {sorted.map((member, i) => (
        <div key={i} className="member-item">
          <span className="online-dot online">●</span>
          <span>{member.display_name}</span>
        </div>
      ))}
    </div>
  );
}
```

- [ ] **Step 3: Commit**

```bash
git add client/src/components/ChannelSidebar.tsx client/src/components/MemberSidebar.tsx
git commit -m "feat(client): add channel sidebar with categories and member list"
```

---

## Task 8: React Components — Chat Panel & Messages

**Files:**
- Create: `client/src/components/ChatPanel.tsx`
- Create: `client/src/components/Message.tsx`
- Create: `client/src/components/MessageInput.tsx`
- Create: `client/src/components/ReactionPicker.tsx`
- Create: `client/src/components/ThreadPanel.tsx`

- [ ] **Step 1: Create Message component**

`client/src/components/Message.tsx`:

```tsx
import { useState } from "react";
import type { MessageInfo } from "../lib/types";
import { publicKeyToString, isDeletedUser } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import { useServer } from "../context/ServerContext";
import ReactionPicker from "./ReactionPicker";

// Simple color hash for author names
function authorColor(pk: string): string {
  const colors = ["#3169C6", "#C00000", "#008000", "#8B008B", "#FF6600", "#006666", "#660066", "#993300"];
  let hash = 0;
  for (let i = 0; i < pk.length; i++) hash = ((hash << 5) - hash + pk.charCodeAt(i)) | 0;
  return colors[Math.abs(hash) % colors.length];
}

interface Props { message: MessageInfo; members: Map<string, string>; }

export default function Message({ message, members }: Props) {
  const { dispatch } = useServer();
  const [showPicker, setShowPicker] = useState(false);
  const pkStr = publicKeyToString(message.author);
  const displayName = isDeletedUser(message.author) ? "Deleted User" : (members.get(pkStr) || pkStr.substring(0, 14) + "...");
  const isDeleted = isDeletedUser(message.author);
  const time = new Date(message.timestamp * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

  async function handleReaction(emoji: string) {
    const existing = message.reactions.find(r => r.emoji === emoji);
    if (existing?.me) {
      await api.removeReaction(message.id, emoji);
    } else {
      await api.addReaction(message.id, emoji);
    }
  }

  return (
    <div className="message">
      <div className="message-header">
        <span className="message-author" style={{ color: isDeleted ? "#888" : authorColor(pkStr) }}>
          {displayName}
        </span>
        <span className="message-timestamp">{time}</span>
        {message.edited_at && <span className="message-timestamp">(edited)</span>}
      </div>
      <div className={`message-content ${isDeleted ? "deleted" : ""}`}>
        {message.content}
      </div>
      {message.attachments.length > 0 && (
        <div style={{ marginTop: 4, fontSize: 10, color: "#666" }}>
          {message.attachments.map((a, i) => (
            <span key={i}>📎 {a.name} ({(a.size / 1024).toFixed(1)} KB){i < message.attachments.length - 1 ? ", " : ""}</span>
          ))}
        </div>
      )}
      {message.reactions.length > 0 && (
        <div className="reaction-bar">
          {message.reactions.map(r => (
            <span key={r.emoji} className={`reaction ${r.me ? "me" : ""}`} onClick={() => handleReaction(r.emoji)}>
              {r.emoji} {r.count}
            </span>
          ))}
          <span className="reaction" onClick={() => setShowPicker(!showPicker)}>+</span>
        </div>
      )}
      {!message.reactions.length && !isDeleted && (
        <div className="reaction-bar" style={{ opacity: 0 }} onMouseEnter={e => (e.currentTarget.style.opacity = "1")} onMouseLeave={e => (e.currentTarget.style.opacity = "0")}>
          <span className="reaction" onClick={() => setShowPicker(!showPicker)}>+</span>
        </div>
      )}
      {showPicker && (
        <ReactionPicker onSelect={(emoji) => { handleReaction(emoji); setShowPicker(false); }} onClose={() => setShowPicker(false)} />
      )}
      {message.thread_id && (
        <div className="thread-link" onClick={() => dispatch({ type: "VIEW_THREAD", channelId: message.thread_id! })}>
          💬 {message.thread_message_count ?? 0} replies
        </div>
      )}
    </div>
  );
}
```

- [ ] **Step 2: Create ReactionPicker**

`client/src/components/ReactionPicker.tsx`:

```tsx
interface Props { onSelect: (emoji: string) => void; onClose: () => void; }

const COMMON_EMOJI = ["👍", "👎", "❤️", "😂", "😮", "😢", "😡", "🎉", "🔥", "👀", "💯", "✅", "❌", "🙏", "👏", "🤔"];

export default function ReactionPicker({ onSelect, onClose }: Props) {
  return (
    <div className="reaction-picker">
      {COMMON_EMOJI.map(e => (
        <button key={e} onClick={() => onSelect(e)}>{e}</button>
      ))}
    </div>
  );
}
```

- [ ] **Step 3: Create MessageInput**

`client/src/components/MessageInput.tsx`:

```tsx
import { useState, KeyboardEvent } from "react";
import * as api from "../lib/tauri-bridge";

interface Props { channelId: number; }

export default function MessageInput({ channelId }: Props) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);

  async function send() {
    if (!text.trim() || sending) return;
    setSending(true);
    try {
      await api.sendMessage(channelId, text.trim());
      setText("");
    } catch (e) {
      console.error("send failed:", e);
    } finally {
      setSending(false);
    }
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  }

  return (
    <div className="message-input-area">
      <input
        className="message-input"
        value={text}
        onChange={e => setText(e.target.value)}
        onKeyDown={handleKeyDown}
        placeholder={`Message #channel...`}
      />
      <button className="xp-button" onClick={send} disabled={sending}>Send</button>
    </div>
  );
}
```

- [ ] **Step 4: Create ChatPanel**

`client/src/components/ChatPanel.tsx`:

```tsx
import { useEffect, useRef, useMemo } from "react";
import { useServer } from "../context/ServerContext";
import Message from "./Message";
import MessageInput from "./MessageInput";
import ThreadPanel from "./ThreadPanel";
import { publicKeyToString } from "../lib/types";

export default function ChatPanel() {
  const { state } = useServer();
  const listRef = useRef<HTMLDivElement>(null);

  // If viewing a thread, show ThreadPanel instead
  if (state.threadChannelId) {
    return <ThreadPanel />;
  }

  const channelId = state.currentChannelId;
  const channel = state.channels.find(c => c.id === channelId);
  const messages = channelId ? (state.messages[channelId] || []) : [];

  // Build member display name map
  const memberMap = useMemo(() => {
    const map = new Map<string, string>();
    state.members.forEach(m => {
      map.set(publicKeyToString(m.public_key), m.display_name);
    });
    return map;
  }, [state.members]);

  // Auto-scroll to bottom on new messages
  useEffect(() => {
    if (listRef.current) {
      listRef.current.scrollTop = listRef.current.scrollHeight;
    }
  }, [messages.length]);

  if (!channelId || !channel) {
    return (
      <div className="chat-panel" style={{ display: "flex", alignItems: "center", justifyContent: "center", color: "#888" }}>
        Select a channel to start chatting
      </div>
    );
  }

  return (
    <div className="chat-panel">
      <div className="channel-header">
        # {channel.name}
        {channel.topic && <span className="topic"> — {channel.topic}</span>}
      </div>
      <div className="message-list" ref={listRef}>
        {messages.map(msg => (
          <Message key={msg.id} message={msg} members={memberMap} />
        ))}
      </div>
      <MessageInput channelId={channelId} />
    </div>
  );
}
```

- [ ] **Step 5: Create ThreadPanel**

`client/src/components/ThreadPanel.tsx`:

```tsx
import { useEffect, useRef, useMemo } from "react";
import { useServer } from "../context/ServerContext";
import Message from "./Message";
import MessageInput from "./MessageInput";
import * as api from "../lib/tauri-bridge";
import { publicKeyToString } from "../lib/types";

export default function ThreadPanel() {
  const { state, dispatch } = useServer();
  const listRef = useRef<HTMLDivElement>(null);
  const threadId = state.threadChannelId!;
  const thread = state.channels.find(c => c.id === threadId);
  const messages = state.messages[threadId] || [];

  const memberMap = useMemo(() => {
    const map = new Map<string, string>();
    state.members.forEach(m => {
      map.set(publicKeyToString(m.public_key), m.display_name);
    });
    return map;
  }, [state.members]);

  useEffect(() => {
    (async () => {
      await api.subscribeChannels([threadId]);
      const msgs = await api.fetchHistory(threadId);
      dispatch({ type: "SET_MESSAGES", channelId: threadId, messages: msgs.reverse() });
    })();
  }, [threadId]);

  useEffect(() => {
    if (listRef.current) listRef.current.scrollTop = listRef.current.scrollHeight;
  }, [messages.length]);

  return (
    <div className="chat-panel">
      <div className="channel-header">
        <span style={{ cursor: "pointer", marginRight: 8 }} onClick={() => dispatch({ type: "VIEW_THREAD", channelId: null })}>
          ← Back
        </span>
        Thread: {thread?.name || "Thread"}
      </div>
      <div className="message-list" ref={listRef}>
        {messages.map(msg => (
          <Message key={msg.id} message={msg} members={memberMap} />
        ))}
      </div>
      <MessageInput channelId={threadId} />
    </div>
  );
}
```

- [ ] **Step 6: Commit**

```bash
git add client/src/components/
git commit -m "feat(client): add chat panel, message display, reactions, threads, and input components"
```

---

## Task 9: Verify & Polish

**Files:**
- Various minor fixes

- [ ] **Step 1: Verify Tauri backend compiles**

Run: `cd /home/deez/farder/client/src-tauri && cargo check`

Fix any compilation errors. Common issues:
- `Keypair` Clone: store key bytes instead, reconstruct when needed
- Import paths for farder-protocol types
- Serde derives needed for IPC return types

- [ ] **Step 2: Verify frontend compiles**

Run: `cd /home/deez/farder/client && npm run build`

Fix any TypeScript errors.

- [ ] **Step 3: Verify Tauri app builds**

Run: `cd /home/deez/farder/client && npm run tauri build -- --debug`

Or for dev mode: `cd /home/deez/farder/client && npm run tauri dev`

- [ ] **Step 4: Manual smoke test**

1. Start a farder-server: `cargo run -p farder-server -- --bind 127.0.0.1:4435`
2. Note the setup token from the console
3. Launch the client: `cd client && npm run tauri dev`
4. Generate identity, enter server address and setup token, connect
5. Verify: channel list shows, can send messages, they appear in the chat

- [ ] **Step 5: Commit any fixes**

```bash
git add -A
git commit -m "fix(client): polish and fix compilation issues"
```

---

## Self-Review Results

**Spec coverage:**
- Tauri backend manages QUIC connection ✅ Tasks 1-2
- IPC commands for all server operations ✅ Task 2
- Tauri events for server pushes ✅ Task 2
- React Context + useReducer state management ✅ Task 5
- XP Luna Blue theme CSS ✅ Task 4
- 3-panel layout (channel sidebar | chat | members) ✅ Tasks 6-8
- Connect dialog with identity + server form ✅ Task 6
- Channel sidebar with categories ✅ Task 7
- Message display with reactions ✅ Task 8
- Reaction picker ✅ Task 8
- Thread view ✅ Task 8
- Message input ✅ Task 8
- Member list ✅ Task 7
- TypeScript types matching protocol ✅ Task 3
- Typed IPC bridge ✅ Task 3

**Placeholder scan:** The `send_request` function in bridge.rs has a design tension comment that was resolved (use tokio::sync::Mutex). The implementer should follow the final version. No actual placeholders.

**Type consistency:** `MessageInfo`, `ChannelInfo`, `CategoryInfo`, `RoleInfo`, `MemberInfo`, `AttachmentInfo`, `ReactionGroup`, `ConnectResult`, `SendMessageResult` — consistent across TypeScript types, bridge functions, and Tauri commands.
