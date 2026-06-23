use crate::state::{AppState, ServerConnection};
use anyhow::{Context, Result};
use farder_protocol::{
    codec,
    server::{ClientFrame, ServerEvent, ServerFrame, ServerRequest, ServerResponse},
};
use quinn::RecvStream;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
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
        ServerEvent::MemberProfileUpdated { public_key, profile_hash } =>
            app.emit("server:member_profile_updated", serde_json::json!({ "server_id": sid, "public_key": public_key.to_string(), "profile_hash": profile_hash })),
        ServerEvent::MemberPresenceUpdated { public_key, presence } =>
            app.emit("server:member_presence_updated", serde_json::json!({ "server_id": sid, "public_key": public_key.to_string(), "presence": presence })),
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
        ServerEvent::StreamKeyOffer { session_id, sender, kind, wrapped_key, .. } => {
            if let Some(ctrl) = app.try_state::<Arc<crate::voice::VoiceController>>() {
                let ctrl = (*ctrl).clone();
                tokio::spawn(async move {
                    ctrl.on_stream_key_offer(session_id, kind, sender, wrapped_key).await;
                });
            }
            Ok(())
        }
        ServerEvent::TrackEnabled { session_id, kind, .. } => {
            // We need the peer's PublicKey to register/display them. The
            // StreamKeyOffer that precedes TrackEnabled carries `sender`,
            // which the controller has already stashed in `peer_pubkeys` keyed
            // by session_id. Look it up there.
            if let Some(ctrl) = app.try_state::<Arc<crate::voice::VoiceController>>() {
                let ctrl = (*ctrl).clone();
                tokio::spawn(async move {
                    let peer_pk = ctrl.peer_pubkey_for(&session_id).await;
                    if let Some(pk) = peer_pk {
                        ctrl.on_peer_track_enabled(session_id, pk, kind).await;
                    } else {
                        eprintln!(
                            "[voice] TrackEnabled before StreamKeyOffer for session {:?}; skipping",
                            session_id
                        );
                    }
                });
            }
            Ok(())
        }
        ServerEvent::TrackDisabled { session_id, kind, .. } => {
            if let Some(ctrl) = app.try_state::<Arc<crate::voice::VoiceController>>() {
                let ctrl = (*ctrl).clone();
                tokio::spawn(async move {
                    ctrl.on_peer_track_disabled(session_id, kind).await;
                });
            }
            Ok(())
        }
        ServerEvent::StreamLeft { session_id, .. } => {
            if let Some(ctrl) = app.try_state::<Arc<crate::voice::VoiceController>>() {
                let ctrl = (*ctrl).clone();
                tokio::spawn(async move {
                    ctrl.on_peer_stream_left(session_id).await;
                });
            }
            Ok(())
        }
        ServerEvent::TrackActivityChanged { session_id, kind, active, .. } => {
            if let Some(ctrl) = app.try_state::<Arc<crate::voice::VoiceController>>() {
                let ctrl = (*ctrl).clone();
                tokio::spawn(async move {
                    ctrl.on_peer_activity(session_id, kind, active).await;
                });
            }
            Ok(())
        }
        ServerEvent::StreamStateChanged { session_id, muted, deafened, .. } => {
            if let Some(ctrl) = app.try_state::<Arc<crate::voice::VoiceController>>() {
                let ctrl = (*ctrl).clone();
                tokio::spawn(async move {
                    ctrl.on_peer_stream_state(session_id, muted, deafened).await;
                });
            }
            Ok(())
        }
        ServerEvent::StreamJoined { session_id, muted, deafened, .. } => {
            if let Some(ctrl) = app.try_state::<Arc<crate::voice::VoiceController>>() {
                let ctrl = (*ctrl).clone();
                tokio::spawn(async move {
                    ctrl.on_peer_stream_joined(session_id, muted, deafened).await;
                });
            }
            Ok(())
        }
        // Voice-channel presence (roster). Emit so the frontend's
        // server:voice_joined / server:voice_left listeners keep the
        // participant list live as peers join and leave. public_key is sent as
        // its to_string() form ("vk_<hex>") to match the getVoiceState snapshot.
        ServerEvent::MediaJoined { channel_id, public_key, display_name } =>
            app.emit("server:voice_joined", serde_json::json!({ "server_id": sid, "channel_id": channel_id, "public_key": public_key.to_string(), "display_name": display_name })),
        ServerEvent::MediaLeft { channel_id, public_key } =>
            app.emit("server:voice_left", serde_json::json!({ "server_id": sid, "channel_id": channel_id, "public_key": public_key.to_string() })),
        ServerEvent::StreamCallIncoming { .. }
        | ServerEvent::StreamCallEnded { .. } => Ok(()), // DM call signaling; no roster UI yet
        _ => Ok(()),
    };
}
