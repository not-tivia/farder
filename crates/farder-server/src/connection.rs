use crate::{attachments, auth, events::EventTarget, handlers, members, state::ServerState};
use anyhow::{Context, Result};
use farder_crypto::identity::PublicKey;
use farder_protocol::{codec, server::*};
use quinn::{RecvStream, SendStream};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Frame I/O
// ---------------------------------------------------------------------------

const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024; // 16 MB

async fn read_frame(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .context("failed to read frame length")?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME_SIZE {
        anyhow::bail!("frame too large: {} bytes", len);
    }
    let mut payload = vec![0u8; len as usize];
    recv.read_exact(&mut payload)
        .await
        .context("failed to read frame payload")?;
    Ok(payload)
}

async fn write_frame(send: &mut SendStream, data: &[u8]) -> Result<()> {
    let len = data.len() as u32;
    send.write_all(&len.to_be_bytes())
        .await
        .context("failed to write frame length")?;
    send.write_all(data)
        .await
        .context("failed to write frame payload")?;
    Ok(())
}

async fn send_server_frame(send: &mut SendStream, frame: &ServerFrame) -> Result<()> {
    let data = codec::encode(frame).context("failed to encode server frame")?;
    write_frame(send, &data).await
}

async fn recv_client_frame(recv: &mut RecvStream) -> Result<ClientFrame> {
    let data = read_frame(recv).await?;
    codec::decode(&data).context("failed to decode client frame")
}

// ---------------------------------------------------------------------------
// Upload stream handler
// ---------------------------------------------------------------------------

async fn handle_upload_stream(
    state: &Arc<ServerState>,
    member_key: &PublicKey,
    is_owner: bool,
    mut send: SendStream,
    mut recv: RecvStream,
    req: UploadRequest,
) -> Result<()> {
    // 1. Validate size
    if req.file_size > state.max_file_size {
        let resp = codec::encode(&UploadResponse::Error {
            reason: format!("file too large (max {} bytes)", state.max_file_size),
        })?;
        write_frame(&mut send, &resp).await?;
        return Ok(());
    }
    if req.file_name.is_empty() || req.file_name.len() > 255 {
        let resp = codec::encode(&UploadResponse::Error {
            reason: "invalid file name".to_string(),
        })?;
        write_frame(&mut send, &resp).await?;
        return Ok(());
    }

    // 2. Permission check — drop the guard before any await.
    let perm_ok = {
        let db = state.db.lock().unwrap();
        let perms = crate::handlers::resolve_member_perms_pub(&db, member_key, req.channel_id, is_owner)?;
        crate::permissions::has(perms, crate::permissions::SEND_MESSAGES)
    };
    if !perm_ok {
        let resp = codec::encode(&UploadResponse::Error {
            reason: "missing SEND_MESSAGES permission".to_string(),
        })?;
        write_frame(&mut send, &resp).await?;
        return Ok(());
    }

    // 3. Check dedup — drop the guard before any await.
    let existing_id: Option<u64> = {
        let db = state.db.lock().unwrap();
        attachments::get_file_by_hash(&db, &req.hash)?.map(|r| r.id)
    };
    if let Some(file_id) = existing_id {
        let resp = codec::encode(&UploadResponse::Complete { file_id })?;
        write_frame(&mut send, &resp).await?;
        return Ok(());
    }

    // 4. Send Ready
    let resp = codec::encode(&UploadResponse::Ready)?;
    write_frame(&mut send, &resp).await?;

    // 5. Read file bytes
    let mut file_data: Vec<u8> = Vec::with_capacity(req.file_size as usize);
    let mut remaining = req.file_size;
    while remaining > 0 {
        let chunk_size = std::cmp::min(remaining as usize, 65536);
        let mut buf = vec![0u8; chunk_size];
        recv.read_exact(&mut buf[..chunk_size])
            .await
            .context("stream closed before all bytes received")?;
        file_data.extend_from_slice(&buf[..chunk_size]);
        remaining -= chunk_size as u64;
    }

    // 6. Store file — release the db lock before any await.
    let store_result: Result<u64> = {
        let db = state.db.lock().unwrap();
        attachments::store_file(
            &db,
            &state.storage_dir,
            member_key,
            &req.file_name,
            &file_data,
            &req.hash,
            &req.mime_type,
        )
    };
    match store_result {
        Ok(file_id) => {
            let resp = codec::encode(&UploadResponse::Complete { file_id })?;
            write_frame(&mut send, &resp).await?;
        }
        Err(e) => {
            let resp = codec::encode(&UploadResponse::Error {
                reason: e.to_string(),
            })?;
            write_frame(&mut send, &resp).await?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Download stream handler
// ---------------------------------------------------------------------------

async fn handle_download_stream(
    state: &Arc<ServerState>,
    member_key: &PublicKey,
    is_owner: bool,
    mut send: SendStream,
    req: DownloadRequest,
) -> Result<()> {
    // 1. Get file record
    let file = {
        let db = state.db.lock().unwrap();
        attachments::get_file(&db, req.file_id)?
    };
    let file = match file {
        Some(f) => f,
        None => {
            let resp = codec::encode(&DownloadResponse::Error {
                reason: "file not found".to_string(),
            })?;
            write_frame(&mut send, &resp).await?;
            return Ok(());
        }
    };

    // 2. Permission check: member needs VIEW_CHANNEL + READ_MESSAGES for at least one channel
    //    that this file is attached to via a message.
    let has_access = {
        let db = state.db.lock().unwrap();
        if is_owner {
            true
        } else {
            let channel_ids: Vec<u64> = {
                let mut stmt = db.prepare(
                    "SELECT DISTINCT m.channel_id \
                     FROM message_attachments ma \
                     JOIN messages m ON m.id = ma.message_id \
                     WHERE ma.file_id = ?1",
                )?;
                let rows = stmt.query_map(rusqlite::params![req.file_id as i64], |row| {
                    Ok(row.get::<_, i64>(0)? as u64)
                })?;
                let ids: Vec<u64> = rows.filter_map(|r| r.ok()).collect();
                ids
            };
            channel_ids.iter().any(|ch_id| {
                crate::handlers::resolve_member_perms_pub(&db, member_key, *ch_id, false)
                    .map(|p| {
                        crate::permissions::has(
                            p,
                            crate::permissions::VIEW_CHANNEL | crate::permissions::READ_MESSAGES,
                        )
                    })
                    .unwrap_or(false)
            })
        }
    };

    if !has_access {
        let resp = codec::encode(&DownloadResponse::Error {
            reason: "access denied".to_string(),
        })?;
        write_frame(&mut send, &resp).await?;
        return Ok(());
    }

    // 3. Send metadata
    let resp = codec::encode(&DownloadResponse::Start {
        file_name: file.original_name.clone(),
        file_size: file.size,
        hash: file.hash.clone(),
        mime_type: file.mime_type.clone(),
    })?;
    write_frame(&mut send, &resp).await?;

    // 4. Stream file bytes
    let path = attachments::content_path(&state.storage_dir, &file.hash);
    let file_bytes = tokio::fs::read(&path).await?;
    send.write_all(&file_bytes).await?;
    send.finish()?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Auxiliary stream dispatcher
// ---------------------------------------------------------------------------

async fn handle_auxiliary_stream(
    state: &Arc<ServerState>,
    member_key: &PublicKey,
    is_owner: bool,
    send: SendStream,
    mut recv: RecvStream,
) -> Result<()> {
    let data = read_frame(&mut recv).await?;

    // Try UploadRequest first
    if let Ok(req) = codec::decode::<UploadRequest>(&data) {
        return handle_upload_stream(state, member_key, is_owner, send, recv, req).await;
    }

    // Try DownloadRequest
    if let Ok(req) = codec::decode::<DownloadRequest>(&data) {
        return handle_download_stream(state, member_key, is_owner, send, req).await;
    }

    anyhow::bail!("unknown auxiliary stream type");
}

// ---------------------------------------------------------------------------
// Main handler (public)
// ---------------------------------------------------------------------------

pub async fn handle_connection(state: Arc<ServerState>, conn: quinn::Connection) -> Result<()> {
    let (mut send, mut recv) = conn.open_bi().await?;

    // Step 1: Send challenge
    let nonce = auth::generate_challenge();
    send_server_frame(&mut send, &ServerFrame::Challenge { nonce }).await?;

    // Step 2: Receive authentication frame
    let client_frame = recv_client_frame(&mut recv).await?;
    let (public_key, signed_challenge, invite_code, setup_token) = match client_frame {
        ClientFrame::Authenticate {
            public_key,
            signed_challenge,
            invite_code,
            setup_token,
        } => (public_key, signed_challenge, invite_code, setup_token),
        _ => {
            let _ = send_server_frame(
                &mut send,
                &ServerFrame::AuthError {
                    reason: "expected Authenticate frame".to_string(),
                },
            )
            .await;
            anyhow::bail!("expected Authenticate frame");
        }
    };

    // Step 3: Verify signature
    if let Err(e) = auth::verify_challenge(&public_key, &nonce, &signed_challenge) {
        let reason = format!("signature verification failed: {}", e);
        let _ = send_server_frame(&mut send, &ServerFrame::AuthError { reason: reason.clone() }).await;
        anyhow::bail!("{}", reason);
    }

    // Step 4: Check existing vs new member
    let pk_bytes = *public_key.as_bytes();
    let mut setup_token_used = false;
    let auth_result: Result<(), String> = {
        let conn_db = state.db.lock().unwrap();
        let existing = members::get_member(&conn_db, &public_key)?;
        if let Some(_member) = existing {
            match auth::authenticate_existing_member(&conn_db, &public_key)? {
                Ok(()) => Ok(()),
                Err(reason) => Err(reason),
            }
        } else {
            let display_name = format!("vk_{}", hex::encode(&pk_bytes[..4]));
            let active_setup_token = state.setup_token.lock().unwrap().clone();
            match auth::authenticate_new_member(
                &conn_db,
                &public_key,
                &display_name,
                invite_code.as_deref(),
                setup_token.as_deref(),
                active_setup_token.as_ref(),
            )? {
                Ok(()) => {
                    if setup_token.is_some() {
                        drop(conn_db);
                        let mut st = state.setup_token.lock().unwrap();
                        if st.is_some() {
                            *st = None;
                            setup_token_used = true;
                        }
                        drop(st);
                    }
                    Ok(())
                }
                Err(reason) => Err(reason),
            }
        }
    };

    // If the setup token was just consumed, set the owner.
    if setup_token_used && auth_result.is_ok() {
        let mut owner = state.owner.write().await;
        *owner = Some(public_key.clone());
    }

    // Step 5: Send Authenticated or AuthError
    match &auth_result {
        Err(reason) => {
            let _ = send_server_frame(
                &mut send,
                &ServerFrame::AuthError {
                    reason: reason.clone(),
                },
            )
            .await;
            anyhow::bail!("auth rejected: {}", reason);
        }
        Ok(()) => {}
    }

    let session_token = auth::generate_session_token();
    send_server_frame(
        &mut send,
        &ServerFrame::Authenticated {
            session_token: session_token.to_vec(),
        },
    )
    .await?;

    info!("client authenticated: {}", public_key);

    // Step 6: Register client in state.clients
    let (event_tx, event_rx) = mpsc::channel::<ServerEvent>(64);
    {
        let mut clients = state.clients.write().await;
        clients.insert(pk_bytes, event_tx);
    }

    // Step 7: Broadcast MemberJoined to all
    let display_name = {
        let conn_db = state.db.lock().unwrap();
        members::get_member(&conn_db, &public_key)?
            .map(|m| m.display_name)
            .unwrap_or_else(|| format!("vk_{}", hex::encode(&pk_bytes[..4])))
    };

    broadcast_event(
        &state,
        EventTarget::All,
        ServerEvent::MemberJoined {
            public_key: public_key.clone(),
            display_name,
        },
    )
    .await;

    // Step 8: Check is_owner
    let is_owner = {
        let owner = state.owner.read().await;
        owner.as_ref().map(|o| o == &public_key).unwrap_or(false)
    };

    // Step 9: Spawn auxiliary stream acceptor
    let conn_clone = conn.clone();
    let state_clone = Arc::clone(&state);
    let member_clone = public_key.clone();
    let stream_acceptor = tokio::spawn(async move {
        loop {
            match conn_clone.accept_bi().await {
                Ok((s, r)) => {
                    let st = Arc::clone(&state_clone);
                    let mk = member_clone.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_auxiliary_stream(&st, &mk, is_owner, s, r).await {
                            tracing::debug!("auxiliary stream error: {}", e);
                        }
                    });
                }
                Err(_) => break,
            }
        }
    });

    // Step 10: Enter main loop
    let loop_result = main_loop(
        Arc::clone(&state),
        public_key.clone(),
        is_owner,
        &mut send,
        &mut recv,
        event_rx,
    )
    .await;

    // Abort the stream acceptor on disconnect
    stream_acceptor.abort();

    // Step 11: Cleanup on disconnect
    {
        let mut clients = state.clients.write().await;
        clients.remove(&pk_bytes);
    }
    {
        let mut subs = state.subscriptions.write().await;
        for (_channel_id, subscribers) in subs.iter_mut() {
            subscribers.remove(&pk_bytes);
        }
    }
    broadcast_event(
        &state,
        EventTarget::All,
        ServerEvent::MemberLeft {
            public_key: public_key.clone(),
        },
    )
    .await;

    if let Err(e) = loop_result {
        warn!("client {} disconnected with error: {}", public_key, e);
    } else {
        info!("client {} disconnected cleanly", public_key);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Main loop (private)
// ---------------------------------------------------------------------------

async fn main_loop(
    state: Arc<ServerState>,
    member_key: PublicKey,
    is_owner: bool,
    send: &mut SendStream,
    recv: &mut RecvStream,
    mut event_rx: mpsc::Receiver<ServerEvent>,
) -> Result<()> {
    loop {
        tokio::select! {
            // Client request branch
            frame_result = recv_client_frame(recv) => {
                let frame = match frame_result {
                    Ok(f) => f,
                    Err(e) => {
                        return Err(e.context("failed to receive client frame"));
                    }
                };

                match frame {
                    ClientFrame::Request { id, body } => {
                        match body {
                            ServerRequest::Subscribe { channel_ids } => {
                                // Update subscriptions: remove from all, add to requested
                                let pk_bytes = *member_key.as_bytes();
                                {
                                    let mut subs = state.subscriptions.write().await;
                                    // Remove from all channels
                                    for subscribers in subs.values_mut() {
                                        subscribers.remove(&pk_bytes);
                                    }
                                    // Add to requested channels
                                    for channel_id in channel_ids {
                                        subs.entry(channel_id)
                                            .or_insert_with(HashSet::new)
                                            .insert(pk_bytes);
                                    }
                                }
                                let response = ServerFrame::Response {
                                    request_id: id,
                                    body: ServerResponse::Ok,
                                };
                                send_server_frame(send, &response).await?;
                            }
                            request => {
                                // Lock db, call handler, drop lock before await
                                let handle_result = {
                                    let conn = state.db.lock().unwrap();
                                    handlers::handle_request(&conn, &member_key, is_owner, request)
                                };

                                match handle_result {
                                    Err(e) => {
                                        let response = ServerFrame::Response {
                                            request_id: id,
                                            body: ServerResponse::Error {
                                                reason: format!("internal error: {}", e),
                                            },
                                        };
                                        send_server_frame(send, &response).await?;
                                    }
                                    Ok(mut result) => {
                                        // Patch server name into ServerInfo responses
                                        if let ServerResponse::ServerInfo { ref mut name, .. } = result.response {
                                            *name = state.server_name.clone();
                                        }

                                        let response = ServerFrame::Response {
                                            request_id: id,
                                            body: result.response,
                                        };
                                        send_server_frame(send, &response).await?;

                                        // Broadcast events
                                        for broadcast in result.events {
                                            broadcast_event(&state, broadcast.target, broadcast.event).await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {
                        // Unexpected frame type during main loop
                        let response = ServerFrame::Response {
                            request_id: 0,
                            body: ServerResponse::Error {
                                reason: "unexpected frame type".to_string(),
                            },
                        };
                        send_server_frame(send, &response).await?;
                    }
                }
            }

            // Event branch
            event = event_rx.recv() => {
                match event {
                    Some(ev) => {
                        send_server_frame(send, &ServerFrame::Event(ev)).await?;
                    }
                    None => {
                        // Channel closed
                        return Ok(());
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Broadcasting (public)
// ---------------------------------------------------------------------------

pub async fn broadcast_event(state: &ServerState, target: EventTarget, event: ServerEvent) {
    match target {
        EventTarget::All => {
            let clients = state.clients.read().await;
            for sender in clients.values() {
                let _ = sender.try_send(event.clone());
            }
        }
        EventTarget::Subscribers(channel_id) => {
            let subs = state.subscriptions.read().await;
            if let Some(subscriber_keys) = subs.get(&channel_id) {
                let clients = state.clients.read().await;
                for pk_bytes in subscriber_keys {
                    if let Some(sender) = clients.get(pk_bytes) {
                        let _ = sender.try_send(event.clone());
                    }
                }
            }
        }
    }
}
