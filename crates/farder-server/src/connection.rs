use crate::{auth, events::EventTarget, handlers, members, state::ServerState};
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
// Main handler (public)
// ---------------------------------------------------------------------------

pub async fn handle_client(
    state: Arc<ServerState>,
    mut send: SendStream,
    mut recv: RecvStream,
) -> Result<()> {
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
    // Track whether the setup token was consumed so we can do the async owner update after all
    // std::sync::MutexGuards are dropped.
    let mut setup_token_used = false;
    let auth_result: Result<(), String> = {
        let conn = state.db.lock().unwrap();
        let existing = members::get_member(&conn, &public_key)?;
        if let Some(_member) = existing {
            // Existing member: check banned/revoked
            match auth::authenticate_existing_member(&conn, &public_key)? {
                Ok(()) => Ok(()),
                Err(reason) => Err(reason),
            }
        } else {
            // New member: need invite or setup token
            let display_name = format!("vk_{}", hex::encode(&pk_bytes[..4]));
            let active_setup_token = state.setup_token.lock().unwrap().clone();
            match auth::authenticate_new_member(
                &conn,
                &public_key,
                &display_name,
                invite_code.as_deref(),
                setup_token.as_deref(),
                active_setup_token.as_ref(),
            )? {
                Ok(()) => {
                    // If setup token was used, clear it; the async owner update happens below.
                    if setup_token.is_some() {
                        drop(conn); // release db lock
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

    // If the setup token was just consumed, set the owner (async, so must be outside the sync block).
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
        let conn = state.db.lock().unwrap();
        members::get_member(&conn, &public_key)?
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

    // Step 9: Enter main loop
    let loop_result = main_loop(
        Arc::clone(&state),
        public_key.clone(),
        is_owner,
        &mut send,
        &mut recv,
        event_rx,
    )
    .await;

    // Step 10: Cleanup on disconnect
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
