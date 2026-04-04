use crate::connection::{recv_server_frame, send_client_frame};
use crate::state::AppState;
use anyhow::{bail, Context, Result};
use farder_protocol::server::{ClientFrame, ServerEvent, ServerFrame, ServerRequest, ServerResponse};
use quinn::RecvStream;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::task::JoinHandle;

/// Send a request to the server and await the response via a oneshot channel.
pub async fn send_request(state: &AppState, request: ServerRequest) -> Result<ServerResponse> {
    let id = state.next_id();
    let (tx, rx) = tokio::sync::oneshot::channel::<ServerResponse>();

    {
        let mut pending = state
            .pending_requests
            .lock()
            .map_err(|e| anyhow::anyhow!("pending_requests lock poisoned: {}", e))?;
        pending.insert(id, tx);
    }

    // Lock the async send stream and write the frame.
    {
        let mut guard = state.send_stream.lock().await;
        let send = guard
            .as_mut()
            .context("not connected to a server")?;
        let frame = ClientFrame::Request { id, body: request };
        send_client_frame(send, &frame)
            .await
            .context("failed to write request frame")?;
    }

    // Await the response from the event reader task.
    let response = rx.await.context("response channel closed (disconnected?)")?;
    Ok(response)
}

/// Spawn the background task that reads `ServerFrame`s and dispatches them.
///
/// - `Response` frames are routed to the matching pending oneshot sender.
/// - `Event` frames are emitted as Tauri frontend events.
/// - On connection error the `server:disconnected` event is emitted.
pub fn spawn_event_reader(
    app: AppHandle,
    state: Arc<AppState>,
    mut recv: RecvStream,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match recv_server_frame(&mut recv).await {
                Err(e) => {
                    eprintln!("[bridge] connection error: {}", e);
                    let _ = app.emit("server:disconnected", ());
                    // Mark as disconnected.
                    if let Ok(mut c) = state.connected.lock() {
                        *c = false;
                    }
                    break;
                }
                Ok(frame) => match frame {
                    ServerFrame::Response { request_id, body } => {
                        let sender = {
                            let mut pending = match state.pending_requests.lock() {
                                Ok(p) => p,
                                Err(_) => break,
                            };
                            pending.remove(&request_id)
                        };
                        if let Some(tx) = sender {
                            let _ = tx.send(body);
                        }
                    }
                    ServerFrame::Event(event) => {
                        dispatch_event(&app, event);
                    }
                    // Challenge / Authenticated / AuthError only arrive during connect_and_authenticate.
                    _ => {}
                },
            }
        }
    })
}

/// Emit a Tauri frontend event for the given `ServerEvent`.
fn dispatch_event(app: &AppHandle, event: ServerEvent) {
    match event {
        ServerEvent::NewMessage { message } => {
            let _ = app.emit("server:new_message", &message);
        }
        ServerEvent::MessageEdited { message_id, channel_id, new_content, edited_at } => {
            let _ = app.emit(
                "server:message_edited",
                serde_json::json!({
                    "message_id": message_id,
                    "channel_id": channel_id,
                    "new_content": new_content,
                    "edited_at": edited_at,
                }),
            );
        }
        ServerEvent::MessageDeleted { message_id, channel_id } => {
            let _ = app.emit(
                "server:message_deleted",
                serde_json::json!({ "message_id": message_id, "channel_id": channel_id }),
            );
        }
        ServerEvent::ReactionAdded { message_id, channel_id, emoji, public_key } => {
            let _ = app.emit(
                "server:reaction_added",
                serde_json::json!({
                    "message_id": message_id,
                    "channel_id": channel_id,
                    "emoji": emoji,
                    "public_key": public_key.to_string(),
                }),
            );
        }
        ServerEvent::ReactionRemoved { message_id, channel_id, emoji, public_key } => {
            let _ = app.emit(
                "server:reaction_removed",
                serde_json::json!({
                    "message_id": message_id,
                    "channel_id": channel_id,
                    "emoji": emoji,
                    "public_key": public_key.to_string(),
                }),
            );
        }
        ServerEvent::MemberJoined { public_key, display_name } => {
            let _ = app.emit(
                "server:member_joined",
                serde_json::json!({
                    "public_key": public_key.to_string(),
                    "display_name": display_name,
                }),
            );
        }
        ServerEvent::MemberLeft { public_key } => {
            let _ = app.emit(
                "server:member_left",
                serde_json::json!({ "public_key": public_key.to_string() }),
            );
        }
        ServerEvent::ChannelCreated { channel } => {
            let _ = app.emit("server:channel_created", channel);
        }
        ServerEvent::ChannelUpdated { channel } => {
            let _ = app.emit("server:channel_updated", channel);
        }
        ServerEvent::ChannelDeleted { channel_id } => {
            let _ = app.emit(
                "server:channel_deleted",
                serde_json::json!({ "channel_id": channel_id }),
            );
        }
        ServerEvent::CategoryCreated { category } => {
            let _ = app.emit("server:category_created", &category);
        }
        ServerEvent::CategoryUpdated { category } => {
            let _ = app.emit("server:category_updated", &category);
        }
        ServerEvent::CategoryDeleted { category_id } => {
            let _ = app.emit("server:category_deleted", serde_json::json!({ "category_id": category_id }));
        }
        ServerEvent::TypingStarted { channel_id, public_key } => {
            let _ = app.emit(
                "server:typing",
                serde_json::json!({
                    "channel_id": channel_id,
                    "public_key": public_key.to_string(),
                }),
            );
        }
        // All other events are intentionally ignored by the client.
        _ => {}
    }
}
