use crate::{attachments, auth, channels, events::EventTarget, handlers, invites, members, permissions, state::{EventSender, ServerState}};
use anyhow::{Context, Result};
use farder_crypto::identity::PublicKey;
use farder_protocol::{codec, server::*};
use quinn::{RecvStream, SendStream};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{mpsc, Semaphore};
use tokio::time::Duration;
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

    // 2b. Rate limit — 10 uploads/min per user.
    let pk_bytes = *member_key.as_bytes();
    if !state.upload_limiter.allow(&pk_bytes) {
        let resp = codec::encode(&UploadResponse::Error {
            reason: "upload rate limit exceeded — please slow down".to_string(),
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

    // 5. Stream bytes to a temp file, computing SHA-256 as we go.
    //    Wrap the entire read loop in a 5-minute timeout.
    let tmp_name = format!(".tmp_{}", rand::random::<u64>());
    let tmp_path = std::path::PathBuf::from(&state.storage_dir).join(&tmp_name);

    let read_result = tokio::time::timeout(
        Duration::from_secs(300),
        async {
            use tokio::io::AsyncWriteExt;
            let mut tmp_file = tokio::fs::File::create(&tmp_path)
                .await
                .context("failed to create temp file")?;
            let mut remaining = req.file_size;
            while remaining > 0 {
                let chunk_size = std::cmp::min(remaining as usize, 65536);
                let mut buf = vec![0u8; chunk_size];
                recv.read_exact(&mut buf[..chunk_size])
                    .await
                    .context("stream closed before all bytes received")?;
                tmp_file
                    .write_all(&buf[..chunk_size])
                    .await
                    .context("failed to write to temp file")?;
                remaining -= chunk_size as u64;
            }
            tmp_file.flush().await.context("failed to flush temp file")?;
            Ok::<(), anyhow::Error>(())
        },
    )
    .await;

    let read_result = match read_result {
        Ok(r) => r,
        Err(_elapsed) => {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            let resp = codec::encode(&UploadResponse::Error {
                reason: "upload timed out".to_string(),
            })?;
            write_frame(&mut send, &resp).await?;
            return Ok(());
        }
    };

    if let Err(e) = read_result {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        let resp = codec::encode(&UploadResponse::Error {
            reason: e.to_string(),
        })?;
        write_frame(&mut send, &resp).await?;
        return Ok(());
    }

    // 6. Atomically check-dedup + verify hash + move to content-addressed path + insert DB record.
    //    All done inside a single DB lock to guard against concurrent uploads of the same hash.
    let store_result: Result<u64> = {
        let db = state.db.lock().unwrap();
        attachments::store_or_reuse_from_temp_file(
            &db,
            &state.storage_dir,
            member_key,
            &req.file_name,
            &tmp_path,
            &req.hash,
            &req.mime_type,
            req.file_size,
            req.width,
            req.height,
            req.duration_secs,
        )
    };
    match store_result {
        Ok(file_id) => {
            let resp = codec::encode(&UploadResponse::Complete { file_id })?;
            write_frame(&mut send, &resp).await?;
        }
        Err(e) => {
            // temp file was already cleaned up by store_or_reuse_from_temp_file on error
            let _ = tokio::fs::remove_file(&tmp_path).await;
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
// URL fetch handler (async — runs outside DB lock)
// ---------------------------------------------------------------------------

async fn handle_fetch_url(
    state: &Arc<ServerState>,
    member: &PublicKey,
    is_owner: bool,
    url: &str,
    channel_id: u64,
) -> Result<u64, String> {
    // Permission check
    {
        let db = state.db.lock().unwrap();
        let perms = handlers::resolve_member_perms_pub(&db, member, channel_id, is_owner)
            .map_err(|e| e.to_string())?;
        if !permissions::has(perms, permissions::SEND_MESSAGES) {
            return Err("missing SEND_MESSAGES permission".to_string());
        }
    }

    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("invalid URL".to_string());
    }
    if url.len() > 2048 {
        return Err("URL too long".to_string());
    }

    // SSRF-guarded async HTTP fetch (no DB lock held). Follow redirects
    // ourselves (cap 4) and re-validate on EVERY hop that the host resolves to a
    // globally routable IP, so a member cannot make the server probe its own
    // host or private network (cloud-metadata 169.254.169.254, localhost, LAN)
    // directly or via a redirect / DNS trick. Mirrors the relay's embed fetcher.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("http client error: {}", e))?;

    let mut current = url.to_string();
    let mut hops = 0;
    let response = loop {
        // Re-assert http(s)-only on EVERY hop (not just the initial url), so a
        // redirect can't switch to file://, gopher://, etc. Explicit > implicit
        // for a security boundary.
        if !current.starts_with("http://") && !current.starts_with("https://") {
            return Err("URL refused: only http(s) is allowed".to_string());
        }
        if !crate::ssrf::resolves_to_global(&current).await {
            return Err("URL refused: resolves to a private or non-routable address".to_string());
        }
        let resp = client
            .get(&current)
            .send()
            .await
            .map_err(|e| format!("fetch failed: {}", e))?;
        if resp.status().is_redirection() {
            hops += 1;
            if hops > 4 {
                return Err("fetch failed: too many redirects".to_string());
            }
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| "fetch failed: redirect without location".to_string())?;
            current = reqwest::Url::parse(&current)
                .map_err(|e| format!("invalid URL: {}", e))?
                .join(loc)
                .map_err(|e| format!("invalid redirect target: {}", e))?
                .to_string();
            continue;
        }
        break resp;
    };

    if !response.status().is_success() {
        return Err(format!("fetch failed: HTTP {}", response.status()));
    }

    let content_type = response.headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .split(';').next()
        .unwrap_or("application/octet-stream")
        .trim()
        .to_string();

    let file_name = current.rsplit('/').next()
        .unwrap_or("download")
        .split('?').next()
        .unwrap_or("download")
        .to_string();

    let data = response.bytes().await
        .map_err(|e| format!("failed to read response: {}", e))?;

    if data.len() > 10 * 1024 * 1024 {
        return Err("fetched file too large (max 10MB)".to_string());
    }

    let hash = attachments::compute_sha256(&data);

    // Now lock DB briefly to store
    let db = state.db.lock().unwrap();
    let file_id = attachments::store_or_reuse(
        &db,
        &state.storage_dir,
        member,
        &file_name,
        &data,
        &hash,
        &content_type,
        None, None, None,
    ).map_err(|e| e.to_string())?;

    Ok(file_id)
}

// ---------------------------------------------------------------------------
// Auxiliary stream dispatcher
// ---------------------------------------------------------------------------

pub(crate) async fn handle_auxiliary_stream(
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
// Authentication (shared by direct + relay paths)
// ---------------------------------------------------------------------------

/// Result of a successful authentication handshake on a (send, recv) pair.
pub(crate) struct AuthOutcome {
    pub public_key: PublicKey,
    pub pk_bytes: [u8; 32],
    pub is_owner: bool,
    pub session_token: [u8; 32],
    pub event_rx: tokio::sync::mpsc::Receiver<ServerEvent>,
    pub event_tx: EventSender,
}

/// Run the auth handshake (challenge → authenticate → register) over the given
/// control streams. On success, the client is registered in `state.clients`,
/// the session token is registered in the session registry, and `MemberJoined`
/// has been broadcast. Does NOT touch `voice_connections` (direct-mode only).
pub(crate) async fn authenticate(
    state: &Arc<ServerState>,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Result<AuthOutcome> {
    // Step 1: Send challenge
    let nonce = auth::generate_challenge();
    send_server_frame(send, &ServerFrame::Challenge { nonce }).await?;

    // Step 2: Receive authentication frame
    let client_frame = recv_client_frame(recv).await?;
    let (public_key, signed_challenge, invite_code, setup_token) = match client_frame {
        ClientFrame::Authenticate {
            public_key,
            signed_challenge,
            invite_code,
            setup_token,
        } => (public_key, signed_challenge, invite_code, setup_token),
        ClientFrame::GetInvitePreview { code } => {
            // Pre-auth, code-gated preview (relay fetch proxy phase one). The
            // connection is throwaway: answer one frame and bail out of auth.
            let valid_member_count = {
                let conn_db = state.db.lock().unwrap();
                match invites::validate_invite(&conn_db, &code)? {
                    Ok(_info) => Some(members::list_members(&conn_db)?.len() as u32),
                    Err(_reason) => None, // uniform: invalid/expired/exhausted all look the same
                }
            };
            match valid_member_count {
                Some(member_count) => {
                    let online_count = state.clients.read().await.len() as u32;
                    send_server_frame(send, &ServerFrame::InvitePreview {
                        server_name: state.server_name.clone(),
                        member_count,
                        online_count,
                    }).await?;
                }
                None => {
                    send_server_frame(send, &ServerFrame::InvitePreviewError {
                        reason: "invalid".to_string(),
                    }).await?;
                }
            }
            anyhow::bail!("served invite preview (throwaway connection, not an auth failure)");
        }
        _ => {
            let _ = send_server_frame(
                send,
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
        let _ = send_server_frame(send, &ServerFrame::AuthError { reason: reason.clone() }).await;
        anyhow::bail!("{}", reason);
    }

    // Step 4: Check existing vs new member
    let pk_bytes = *public_key.as_bytes();
    let mut setup_token_used = false;
    let mut auto_claimed = false;
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
                Ok(claimed) => {
                    auto_claimed = claimed;
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

    // If the setup token was just consumed OR this is the first member (auto-claim),
    // set the owner.
    if (setup_token_used || auto_claimed) && auth_result.is_ok() {
        let mut owner = state.owner.write().await;
        *owner = Some(public_key.clone());
    }

    // Step 5: Send Authenticated or AuthError
    match &auth_result {
        Err(reason) => {
            let _ = send_server_frame(
                send,
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
        send,
        &ServerFrame::Authenticated {
            session_token: session_token.to_vec(),
        },
    )
    .await?;

    info!("client authenticated: {}", public_key);

    // Step 8: Check is_owner (computed before registry inserts so the session
    // registry records the correct owner flag in a single insert).
    let is_owner = {
        let owner = state.owner.read().await;
        owner.as_ref().map(|o| o == &public_key).unwrap_or(false)
    };

    // Step 6: Register client in state.clients
    let (event_tx, event_rx) = mpsc::channel::<ServerEvent>(64);
    let our_event_tx = event_tx.clone();
    {
        let mut clients = state.clients.write().await;
        clients.insert(pk_bytes, event_tx);
    }

    // Register the session token once, with the correct is_owner flag.
    state.register_session(session_token, public_key.clone(), is_owner).await;

    // Step 7: Broadcast MemberJoined to all
    let display_name = {
        let conn_db = state.db.lock().unwrap();
        members::get_member(&conn_db, &public_key)?
            .map(|m| m.display_name)
            .unwrap_or_else(|| format!("vk_{}", hex::encode(&pk_bytes[..4])))
    };

    broadcast_event(
        state,
        EventTarget::All,
        ServerEvent::MemberJoined {
            public_key: public_key.clone(),
            display_name,
        },
    )
    .await;

    Ok(AuthOutcome {
        public_key,
        pk_bytes,
        is_owner,
        session_token,
        event_rx,
        event_tx: our_event_tx,
    })
}

// ---------------------------------------------------------------------------
// Session cleanup (shared by direct + relay paths)
// ---------------------------------------------------------------------------

/// Tear down per-client registry state when the primary session ends: remove the
/// session token, evict from `state.clients` (only if still ours), drop all
/// subscriptions, and broadcast `MemberLeft`. Does NOT touch `voice_connections`
/// (direct-mode only).
pub(crate) async fn cleanup_session(
    state: &Arc<ServerState>,
    public_key: &PublicKey,
    pk_bytes: [u8; 32],
    event_tx: &EventSender,
    session_token: &[u8; 32],
) {
    state.remove_session(session_token).await;

    // Only remove from the clients map if WE'RE still the registered sender —
    // otherwise a newer connection from the same identity has taken over and
    // we'd evict its entry, killing the live session.
    {
        let mut clients = state.clients.write().await;
        let still_ours = clients
            .get(&pk_bytes)
            .map(|existing| existing.same_channel(event_tx))
            .unwrap_or(false);
        if still_ours {
            clients.remove(&pk_bytes);
        }
    }
    {
        let mut subs = state.subscriptions.write().await;
        for (_channel_id, subscribers) in subs.iter_mut() {
            subscribers.remove(&pk_bytes);
        }
    }
    { state.presences.write().unwrap().remove(&pk_bytes); }
    broadcast_event(
        state,
        EventTarget::All,
        ServerEvent::MemberPresenceUpdated {
            public_key: public_key.clone(),
            presence: None,
        },
    )
    .await;
    broadcast_event(
        state,
        EventTarget::All,
        ServerEvent::MemberLeft {
            public_key: public_key.clone(),
        },
    )
    .await;
}

// ---------------------------------------------------------------------------
// Main handler (public)
// ---------------------------------------------------------------------------

pub async fn handle_connection(state: Arc<ServerState>, conn: quinn::Connection) -> Result<()> {
    let (mut send, mut recv) = conn.open_bi().await?;

    let AuthOutcome {
        public_key,
        pk_bytes,
        is_owner,
        session_token,
        event_rx,
        event_tx,
    } = authenticate(&state, &mut send, &mut recv).await?;

    // Direct-only: register the connection for voice.
    {
        let mut voice_conns = state.voice_connections.write().await;
        voice_conns.insert(pk_bytes, crate::state::VoiceSink::Direct(conn.clone()));
    }

    // Step 9: Spawn auxiliary stream acceptor
    let conn_clone = conn.clone();
    let state_clone = Arc::clone(&state);
    let member_clone = public_key.clone();
    let semaphore = Arc::new(Semaphore::new(10)); // max 10 concurrent uploads/downloads
    let stream_acceptor = tokio::spawn(async move {
        loop {
            match conn_clone.accept_bi().await {
                Ok((s, r)) => {
                    let permit = match semaphore.clone().try_acquire_owned() {
                        Ok(p) => p,
                        Err(_) => {
                            // At capacity; drop this stream.
                            tracing::debug!("auxiliary stream rejected: at capacity");
                            continue;
                        }
                    };
                    let st = Arc::clone(&state_clone);
                    let mk = member_clone.clone();
                    tokio::spawn(async move {
                        let _permit = permit; // held until task completes
                        if let Err(e) = handle_auxiliary_stream(&st, &mk, is_owner, s, r).await {
                            tracing::debug!("auxiliary stream error: {}", e);
                        }
                    });
                }
                Err(_) => break,
            }
        }
    });

    // Step 9b: Spawn per-connection QUIC datagram fanout loop.
    //
    // For each inbound datagram:
    //   1. Call on_frame_ingress — pure decision function (forward / drop).
    //   2. On Forward: look up each recipient's connection via SessionId →
    //      connection_pk (from media.channels) → quinn::Connection
    //      (from voice_connections), then send_datagram.
    //   3. On Drop: silently discard.
    let conn_for_dg = conn.clone();
    let state_for_dg = Arc::clone(&state);
    let media_config = crate::media_stream::MediaConfig::default();
    let datagram_task = tokio::spawn(async move {
        loop {
            match conn_for_dg.read_datagram().await {
                Ok(bytes) => {
                    process_inbound_voice_frame(&state_for_dg, pk_bytes, bytes, &media_config).await;
                }
                Err(quinn::ConnectionError::ApplicationClosed { .. })
                | Err(quinn::ConnectionError::ConnectionClosed { .. })
                | Err(quinn::ConnectionError::LocallyClosed)
                | Err(quinn::ConnectionError::TimedOut) => break,
                Err(e) => {
                    tracing::debug!("[media] datagram read error: {}", e);
                    break;
                }
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

    // Abort background tasks on disconnect
    stream_acceptor.abort();
    datagram_task.abort();

    // Step 11: Cleanup on disconnect.
    // Direct-only: drop the voice connection registration.
    state.voice_connections.write().await.remove(&pk_bytes);
    cleanup_session(&state, &public_key, pk_bytes, &event_tx, &session_token).await;

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

pub(crate) async fn main_loop(
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
                            ServerRequest::FetchUrl { url, channel_id } => {
                                // Handle URL fetch async — can't hold DB lock during HTTP request
                                let result = handle_fetch_url(&state, &member_key, is_owner, &url, channel_id).await;
                                match result {
                                    Ok(file_id) => {
                                        let response = ServerFrame::Response {
                                            request_id: id,
                                            body: ServerResponse::UrlFetched { file_id },
                                        };
                                        send_server_frame(send, &response).await?;
                                    }
                                    Err(reason) => {
                                        let response = ServerFrame::Response {
                                            request_id: id,
                                            body: ServerResponse::Error { reason },
                                        };
                                        send_server_frame(send, &response).await?;
                                    }
                                }
                            }
                            request => {
                                // Rate-limit AddReaction before dispatching to handler
                                if matches!(request, ServerRequest::AddReaction { .. }) {
                                    let pk_bytes = *member_key.as_bytes();
                                    if !state.reaction_limiter.allow(&pk_bytes) {
                                        let response = ServerFrame::Response {
                                            request_id: id,
                                            body: ServerResponse::Error {
                                                reason: "reaction rate limit exceeded — please slow down".to_string(),
                                            },
                                        };
                                        send_server_frame(send, &response).await?;
                                        continue;
                                    }
                                }

                                // Lock db, call handler, drop lock before await
                                let handle_result = {
                                    let conn = state.db.lock().unwrap();
                                    handlers::handle_request(&conn, &member_key, is_owner, request, &state.storage_dir, &state)
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
                                        // Patch server name + owner pubkey into ServerInfo responses
                                        if let ServerResponse::ServerInfo {
                                            ref mut name,
                                            ref mut owner_public_key,
                                            ..
                                        } = result.response {
                                            *name = state.server_name.clone();
                                            *owner_public_key = state.owner.read().await.clone();
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

                                        // Clean up orphaned files from disk
                                        for fid in &result.orphaned_file_ids {
                                            let db = state.db.lock().unwrap();
                                            let _ = attachments::cleanup_orphaned_file(&db, &state.storage_dir, *fid);
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
        EventTarget::Members(pks) => {
            let clients = state.clients.read().await;
            for pk in pks {
                if let Some(sender) = clients.get(pk.as_bytes()) {
                    let _ = sender.try_send(event.clone());
                }
            }
        }
        EventTarget::PermissionHolders(perm_bit) => {
            let clients = state.clients.read().await;
            let owner_pk = state.owner.read().await.clone();
            let conn = state.db.lock().unwrap();
            for (pk_bytes, sender) in clients.iter() {
                let pk = farder_crypto::identity::PublicKey::from_bytes(*pk_bytes);
                let is_owner = owner_pk
                    .as_ref()
                    .map(|o| o.as_bytes() == pk.as_bytes())
                    .unwrap_or(false);
                if let Ok(perms) =
                    crate::handlers::resolve_member_server_perms(&conn, &pk, is_owner)
                {
                    if crate::permissions::has(perms, perm_bit) {
                        let _ = sender.try_send(event.clone());
                    }
                }
            }
        }
        // Media-stream targets — dispatching implemented in MST-10.
        EventTarget::MediaStreamJoin { .. } => {}
        EventTarget::MediaStreamLeave { .. } => {}
        EventTarget::MediaTrackEnabled { .. } => {}
        EventTarget::MediaTrackDisabled { .. } => {}
        EventTarget::MediaSetDeafen { .. } => {}
    }
}

/// Process one inbound voice frame: find its channel, run the ingress decision,
/// and fan it out to each recipient's VoiceSink. Shared by the direct
/// per-connection datagram loop and the relay-mode datagram loop. `sending_pk`
/// is the authoritative sender (the direct connection's authed pk, or the relay
/// source handle's bound pk).
pub(crate) async fn process_inbound_voice_frame(
    state: &Arc<ServerState>,
    sending_pk: [u8; 32],
    bytes: bytes::Bytes,
    media_config: &crate::media_stream::MediaConfig,
) {
    let now_ms = {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
    };
    let raw: &[u8] = &bytes;
    let channel_id_opt: Option<u64> = match farder_protocol::media_datagram::OuterHeader::parse(raw) {
        Ok((header, _payload)) => {
            let channels = state.media.channels.read().unwrap();
            channels
                .iter()
                .find(|(_ch, st)| st.sessions.contains_key(&header.session_id))
                .map(|(ch, _)| *ch)
        }
        Err(_) => None,
    };
    let channel_id = match channel_id_opt {
        Some(c) => c,
        None => { tracing::trace!("[media] datagram dropped: session not found"); return; }
    };
    let decision = {
        let mut channels = state.media.channels.write().unwrap();
        if let Some(stream_state) = channels.get_mut(&channel_id) {
            crate::media_stream::on_frame_ingress(stream_state, media_config, &sending_pk, raw, now_ms)
        } else {
            crate::media_stream::IngressDecision::Drop(crate::media_stream::DropReason::UnknownSession)
        }
    };
    match decision {
        crate::media_stream::IngressDecision::Forward { recipients } => {
            let voice_conns = state.voice_connections.read().await;
            let channels = state.media.channels.read().unwrap();
            if let Some(stream_state) = channels.get(&channel_id) {
                for sid in recipients {
                    if let Some(session) = stream_state.sessions.get(&sid) {
                        if let Some(sink) = voice_conns.get(&session.connection_pk) {
                            let _ = sink.send_datagram(bytes.clone());
                        }
                    }
                }
            }
        }
        crate::media_stream::IngressDecision::Drop(_reason) => {
            tracing::trace!("[media] datagram dropped: {:?}", _reason);
        }
    }
}

#[cfg(test)]
mod voice_relay_tests {
    use super::*;

    fn outer_audio_dgram(session: &[u8; 16], ciphertext: &[u8]) -> bytes::Bytes {
        use farder_protocol::media_datagram::OuterHeader;
        use farder_protocol::server::TrackKind;
        let mut v = Vec::new();
        OuterHeader {
            track_kind: TrackKind::Audio,
            session_id: *session,
            frame_id: 0,
            frag_index: 0,
            frag_count: 1,
        }
        .write_to(&mut v);
        v.extend_from_slice(ciphertext);
        bytes::Bytes::from(v)
    }

    async fn loopback_pair() -> (quinn::Connection, quinn::Connection) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = rcgen::generate_simple_self_signed(vec!["t".into()]).unwrap();
        let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
        let key = rustls::pki_types::PrivateKeyDer::try_from(cert.key_pair.serialize_der()).unwrap();
        let mut scfg = quinn::ServerConfig::with_single_cert(vec![cert_der], key).unwrap();
        {
            let mut t = quinn::TransportConfig::default();
            t.datagram_receive_buffer_size(Some(1 << 20));
            t.datagram_send_buffer_size(1 << 20);
            scfg.transport_config(std::sync::Arc::new(t));
        }
        let sep = quinn::Endpoint::server(scfg, "127.0.0.1:0".parse().unwrap()).unwrap();
        let saddr = sep.local_addr().unwrap();
        #[derive(Debug)] struct Skip;
        impl rustls::client::danger::ServerCertVerifier for Skip {
            fn verify_server_cert(&self,_:&rustls::pki_types::CertificateDer<'_>,_:&[rustls::pki_types::CertificateDer<'_>],_:&rustls::pki_types::ServerName<'_>,_:&[u8],_:rustls::pki_types::UnixTime)->std::result::Result<rustls::client::danger::ServerCertVerified,rustls::Error>{Ok(rustls::client::danger::ServerCertVerified::assertion())}
            fn verify_tls12_signature(&self,_:&[u8],_:&rustls::pki_types::CertificateDer<'_>,_:&rustls::DigitallySignedStruct)->std::result::Result<rustls::client::danger::HandshakeSignatureValid,rustls::Error>{Ok(rustls::client::danger::HandshakeSignatureValid::assertion())}
            fn verify_tls13_signature(&self,_:&[u8],_:&rustls::pki_types::CertificateDer<'_>,_:&rustls::DigitallySignedStruct)->std::result::Result<rustls::client::danger::HandshakeSignatureValid,rustls::Error>{Ok(rustls::client::danger::HandshakeSignatureValid::assertion())}
            fn supported_verify_schemes(&self)->Vec<rustls::SignatureScheme>{rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()}
        }
        let crypto = rustls::ClientConfig::builder().dangerous().with_custom_certificate_verifier(std::sync::Arc::new(Skip)).with_no_client_auth();
        let mut ccfg = quinn::ClientConfig::new(std::sync::Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap()));
        let mut t = quinn::TransportConfig::default();
        t.datagram_receive_buffer_size(Some(1 << 20));
        t.datagram_send_buffer_size(1 << 20);
        ccfg.transport_config(std::sync::Arc::new(t));
        let mut cep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        cep.set_default_client_config(ccfg);
        let server_fut = tokio::spawn(async move { sep.accept().await.unwrap().await.unwrap() });
        let client = cep.connect(saddr, "t").unwrap().await.unwrap();
        let server = server_fut.await.unwrap();
        std::mem::forget(cep);
        (client, server)
    }

    #[tokio::test]
    async fn relayed_fanout_tags_recipient_handle() {
        use crate::state::{ServerState, VoiceSink};
        use crate::media_stream::{ServerSession, MediaConfig, StreamState};
        use farder_protocol::server::TrackKind;
        use std::collections::{HashMap, HashSet};
        use std::sync::Arc;

        let state = Arc::new(ServerState::new_for_test().unwrap());
        let config = MediaConfig::default();

        // One shared "relay" connection. relay_tx_side = the server's relay conn;
        // relay_rx = the relay's view (we read what the server emits).
        let (relay_tx_side, relay_rx) = loopback_pair().await;

        let alice_pk = farder_crypto::identity::PublicKey::from_bytes([0xaa; 32]);
        let bob_pk = farder_crypto::identity::PublicKey::from_bytes([0xbb; 32]);
        let alice_conn = [0xaa; 32];
        let bob_conn = [0xbb; 32];
        let alice_session = [1u8; 16];
        let bob_session = [2u8; 16];
        let (h_alice, h_bob) = (10u32, 20u32);

        {
            let mut channels = state.media.channels.write().unwrap();
            let st = channels.entry(99).or_insert_with(StreamState::new);
            for (sid, conn_pk, pk, name) in [
                (alice_session, alice_conn, alice_pk.clone(), "alice"),
                (bob_session, bob_conn, bob_pk.clone(), "bob"),
            ] {
                let mut tracks = HashSet::new();
                tracks.insert(TrackKind::Audio);
                st.sessions.insert(sid, ServerSession {
                    connection_pk: conn_pk, channel_id: 99, public_key: pk,
                    display_name: name.into(), active_tracks: tracks,
                    buckets: HashMap::new(), last_audio_frame_ms: None, last_video_frame_ms: None,
                    last_screen_audio_frame_ms: None,
                });
            }
        }
        {
            let mut vc = state.voice_connections.write().await;
            vc.insert(alice_conn, VoiceSink::Relayed { relay: relay_tx_side.clone(), handle: h_alice });
            vc.insert(bob_conn, VoiceSink::Relayed { relay: relay_tx_side.clone(), handle: h_bob });
        }

        let frame = outer_audio_dgram(&alice_session, b"opaque-ct");
        process_inbound_voice_frame(&state, alice_conn, frame.clone(), &config).await;

        let got = tokio::time::timeout(std::time::Duration::from_secs(5), relay_rx.read_datagram())
            .await.expect("no datagram emitted").unwrap();
        let mut expected = h_bob.to_be_bytes().to_vec();
        expected.extend_from_slice(&frame);
        assert_eq!(got.as_ref(), expected.as_slice(), "fan-out must tag the RECIPIENT (bob) handle");

        let second = tokio::time::timeout(std::time::Duration::from_millis(300), relay_rx.read_datagram()).await;
        assert!(second.is_err(), "only one recipient (bob); sender must not be echoed");
    }
}
