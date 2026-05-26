use crate::state::{AppState, ServerConnection};
use anyhow::{Context, Result};
use farder_protocol::{
    codec,
    server::{ClientFrame, ServerEvent, ServerFrame, ServerRequest, ServerResponse},
};
use quinn::RecvStream;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::task::JoinHandle;

/// Send a request to the server identified by `server_id` and await the response.
pub async fn send_request(state: &AppState, server_id: &str, request: ServerRequest) -> Result<ServerResponse> {
    let conn = state.get_server(server_id).map_err(|e| anyhow::anyhow!("{}", e))?;
    let id = conn.next_id();
    let (tx, rx) = tokio::sync::oneshot::channel();
    conn.pending_requests.lock().unwrap().insert(id, tx);

    let frame = ClientFrame::Request { id, body: request };
    let data = codec::encode(&frame)?;
    {
        let mut send = conn.send_stream.lock().await;
        crate::connection::write_frame(&mut send, &data).await.context("failed to write request frame")?;
    }

    rx.await.context("response channel closed")
}

/// Spawn the background task that reads `ServerFrame`s and dispatches them.
///
/// - `Response` frames are routed to the matching pending oneshot sender.
/// - `Event` frames are emitted as Tauri frontend events (with `server_id`).
/// - On connection error the `server:disconnected` event is emitted.
pub fn spawn_event_reader(
    app: AppHandle,
    server_id: String,
    conn: Arc<ServerConnection>,
    mut recv: RecvStream,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match crate::connection::recv_server_frame(&mut recv).await {
                Err(_) => {
                    let _ = app.emit("server:disconnected", serde_json::json!({ "server_id": &server_id }));
                    break;
                }
                Ok(frame) => match frame {
                    ServerFrame::Response { request_id, body } => {
                        if let Some(tx) = conn.pending_requests.lock().unwrap().remove(&request_id) {
                            let _ = tx.send(body);
                        }
                    }
                    ServerFrame::Event(event) => dispatch_event(&app, &server_id, event),
                    _ => {}
                },
            }
        }
    })
}

/// Emit a Tauri frontend event for the given `ServerEvent`, including `server_id`.
fn dispatch_event(app: &AppHandle, server_id: &str, event: ServerEvent) {
    let sid = server_id;
    let _ = match event {
        ServerEvent::NewMessage { message } =>
            app.emit("server:new_message", serde_json::json!({ "server_id": sid, "message": message })),
        ServerEvent::MessageEdited { message_id, channel_id, new_content, edited_at } =>
            app.emit("server:message_edited", serde_json::json!({ "server_id": sid, "message_id": message_id, "channel_id": channel_id, "new_content": new_content, "edited_at": edited_at })),
        ServerEvent::MessageDeleted { message_id, channel_id } =>
            app.emit("server:message_deleted", serde_json::json!({ "server_id": sid, "message_id": message_id, "channel_id": channel_id })),
        ServerEvent::ReactionAdded { message_id, channel_id, emoji, public_key, file_id } =>
            app.emit("server:reaction_added", serde_json::json!({ "server_id": sid, "message_id": message_id, "channel_id": channel_id, "emoji": emoji, "public_key": public_key.to_string(), "file_id": file_id })),
        ServerEvent::ReactionRemoved { message_id, channel_id, emoji, public_key, file_id } =>
            app.emit("server:reaction_removed", serde_json::json!({ "server_id": sid, "message_id": message_id, "channel_id": channel_id, "emoji": emoji, "public_key": public_key.to_string(), "file_id": file_id })),
        ServerEvent::MemberJoined { public_key, display_name } =>
            app.emit("server:member_joined", serde_json::json!({ "server_id": sid, "public_key": public_key.to_string(), "display_name": display_name })),
        ServerEvent::MemberLeft { public_key } =>
            app.emit("server:member_left", serde_json::json!({ "server_id": sid, "public_key": public_key.to_string() })),
        ServerEvent::MemberBanned { public_key, reason } =>
            app.emit("server:member_banned", serde_json::json!({ "server_id": sid, "public_key": public_key.to_string(), "reason": reason })),
        ServerEvent::MemberUnbanned { public_key } =>
            app.emit("server:member_unbanned", serde_json::json!({ "server_id": sid, "public_key": public_key.to_string() })),
        ServerEvent::MemberTimeoutChanged { public_key, until_ms, reason } =>
            app.emit("server:member_timeout_changed", serde_json::json!({ "server_id": sid, "public_key": public_key.to_string(), "until_ms": until_ms, "reason": reason })),
        ServerEvent::YouWereKicked =>
            app.emit("server:you_were_kicked", serde_json::json!({ "server_id": sid })),
        ServerEvent::YouWereBanned { reason } =>
            app.emit("server:you_were_banned", serde_json::json!({ "server_id": sid, "reason": reason })),
        ServerEvent::AuditEventCreated { event } =>
            app.emit("server:audit_event_created", serde_json::json!({ "server_id": sid, "event": event })),
        ServerEvent::ChannelCreated { channel } =>
            app.emit("server:channel_created", serde_json::json!({ "server_id": sid, "channel": channel })),
        ServerEvent::ChannelUpdated { channel } =>
            app.emit("server:channel_updated", serde_json::json!({ "server_id": sid, "channel": channel })),
        ServerEvent::ChannelDeleted { channel_id } =>
            app.emit("server:channel_deleted", serde_json::json!({ "server_id": sid, "channel_id": channel_id })),
        ServerEvent::TypingStarted { channel_id, public_key } =>
            app.emit("server:typing", serde_json::json!({ "server_id": sid, "channel_id": channel_id, "public_key": public_key.to_string() })),
        ServerEvent::CategoryCreated { category } =>
            app.emit("server:category_created", serde_json::json!({ "server_id": sid, "category": category })),
        ServerEvent::CategoryUpdated { category } =>
            app.emit("server:category_updated", serde_json::json!({ "server_id": sid, "category": category })),
        ServerEvent::CategoryDeleted { category_id } =>
            app.emit("server:category_deleted", serde_json::json!({ "server_id": sid, "category_id": category_id })),
        ServerEvent::DmCreated { channel, participant } =>
            app.emit("server:dm_created", serde_json::json!({ "server_id": sid, "channel": channel, "participant": participant })),
        ServerEvent::RoleCreated { role } =>
            app.emit("server:role_created", serde_json::json!({ "server_id": sid, "role": role })),
        ServerEvent::RoleDeleted { role_id } =>
            app.emit("server:role_deleted", serde_json::json!({ "server_id": sid, "role_id": role_id })),
        ServerEvent::RoleUpdated { role } =>
            app.emit("server:role_updated", serde_json::json!({ "server_id": sid, "role": role })),
        ServerEvent::MediaJoined { .. }
        | ServerEvent::MediaLeft { .. }
        | ServerEvent::StreamJoined { .. }
        | ServerEvent::StreamLeft { .. }
        | ServerEvent::TrackEnabled { .. }
        | ServerEvent::TrackDisabled { .. }
        | ServerEvent::TrackActivityChanged { .. }
        | ServerEvent::StreamCallIncoming { .. }
        | ServerEvent::StreamCallEnded { .. }
        | ServerEvent::StreamKeyOffer { .. } => Ok(()), // TODO(MST UI)
        _ => Ok(()),
    };
}
