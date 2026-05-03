use farder_crypto::identity::Keypair;
use farder_protocol::{codec, server::*};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use rustls::pki_types::ServerName;
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// TLS skip verifier
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct SkipVerification;

impl rustls::client::danger::ServerCertVerifier for SkipVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dh_params: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dh_params: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ---------------------------------------------------------------------------
// QUIC endpoint helpers
// ---------------------------------------------------------------------------

fn make_client_endpoint() -> Endpoint {
    let crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipVerification))
        .with_no_client_auth();
    let client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
    ));
    let mut endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(client_config);
    endpoint
}

fn make_server_endpoint(bind_addr: SocketAddr) -> Endpoint {
    let certified =
        rcgen::generate_simple_self_signed(vec!["farder-server".to_string()]).unwrap();
    let cert_der =
        rustls::pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key_der =
        rustls::pki_types::PrivateKeyDer::try_from(certified.key_pair.serialize_der())
            .expect("key error");
    let server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .unwrap();
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto).unwrap(),
    ));
    Endpoint::server(server_config, bind_addr).unwrap()
}

// ---------------------------------------------------------------------------
// Frame I/O helpers
// ---------------------------------------------------------------------------

async fn read_frame(recv: &mut RecvStream) -> Vec<u8> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await.expect("read frame length");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    recv.read_exact(&mut payload).await.expect("read frame payload");
    payload
}

async fn write_frame(send: &mut SendStream, data: &[u8]) {
    let len = (data.len() as u32).to_be_bytes();
    send.write_all(&len).await.expect("write frame length");
    send.write_all(data).await.expect("write frame payload");
}

async fn send_frame(send: &mut SendStream, frame: &impl serde::Serialize) {
    let data = codec::encode(frame).expect("encode frame");
    write_frame(send, &data).await;
}

async fn recv_server_frame(recv: &mut RecvStream) -> ServerFrame {
    let data = read_frame(recv).await;
    codec::decode(&data).expect("decode server frame")
}

// ---------------------------------------------------------------------------
// Auth helper
// ---------------------------------------------------------------------------

async fn connect_and_auth(
    endpoint: &Endpoint,
    server_addr: SocketAddr,
    keypair: &Keypair,
    invite_code: Option<&str>,
    setup_token: Option<&str>,
) -> (Connection, SendStream, RecvStream) {
    let conn = endpoint
        .connect(server_addr, "farder-server")
        .unwrap()
        .await
        .expect("connect");
    // The server opens the bi-directional stream (so it can send the Challenge first).
    // The client accepts it here.
    let (mut send, mut recv) = conn.accept_bi().await.expect("accept_bi");

    // Receive Challenge
    let frame = recv_server_frame(&mut recv).await;
    let nonce = match frame {
        ServerFrame::Challenge { nonce } => nonce,
        other => panic!("expected Challenge, got {:?}", other),
    };

    // Sign and send Authenticate
    let signature = keypair.sign(&nonce);
    let auth_frame = ClientFrame::Authenticate {
        public_key: keypair.public_key(),
        signed_challenge: signature,
        invite_code: invite_code.map(|s| s.to_string()),
        setup_token: setup_token.map(|s| s.to_string()),
    };
    send_frame(&mut send, &auth_frame).await;

    // Receive Authenticated
    let frame = recv_server_frame(&mut recv).await;
    match frame {
        ServerFrame::Authenticated { .. } => {}
        ServerFrame::AuthError { reason } => panic!("auth error: {}", reason),
        other => panic!("expected Authenticated, got {:?}", other),
    }

    (conn, send, recv)
}

// ---------------------------------------------------------------------------
// Request / response helpers
// ---------------------------------------------------------------------------

async fn send_request(send: &mut SendStream, id: u32, request: ServerRequest) {
    let frame = ClientFrame::Request { id, body: request };
    send_frame(send, &frame).await;
}

async fn recv_response(recv: &mut RecvStream) -> (u32, ServerResponse) {
    loop {
        let frame = recv_server_frame(recv).await;
        match frame {
            ServerFrame::Response { request_id, body } => return (request_id, body),
            ServerFrame::Event(_) => {
                // Skip event frames, keep waiting for the response
                continue;
            }
            other => panic!("unexpected frame while waiting for response: {:?}", other),
        }
    }
}

// ---------------------------------------------------------------------------
// The e2e test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_e2e_server_bootstrap_and_chat() {
    // Install rustls crypto provider
    rustls::crypto::ring::default_provider().install_default().ok();

    // 1. Set up server in-process with in-memory DB
    let bind_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let server_endpoint = make_server_endpoint(bind_addr);
    let actual_addr = server_endpoint.local_addr().unwrap();

    // Create ServerState with in-memory DB, @everyone role, blank template
    let conn = farder_server::db::open_in_memory().unwrap();
    farder_server::members::create_role(
        &conn,
        "@everyone",
        farder_server::permissions::DEFAULT_EVERYONE,
        None,
        0,
        true,
    )
    .unwrap();
    let templates = farder_server::templates::list_builtin_templates();
    let blank = templates
        .iter()
        .find(|t| t.template.name == "Blank")
        .unwrap();
    farder_server::templates::apply_template(&conn, blank).unwrap();

    let tmp_dir = std::env::temp_dir().join(format!("farder-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let state = Arc::new(farder_server::state::ServerState::new(
        conn,
        "Test Server".to_string(),
        tmp_dir.to_string_lossy().to_string(),
        50 * 1024 * 1024,
    ));
    let setup_token = farder_server::auth::generate_setup_token();
    let setup_hex = hex::encode(&setup_token);
    *state.setup_token.lock().unwrap() = Some(setup_token);

    // Spawn server accept loop
    let server_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            let incoming = match server_endpoint.accept().await {
                Some(inc) => inc,
                None => break,
            };
            let state = Arc::clone(&server_state);
            tokio::spawn(async move {
                let conn = incoming.await.unwrap();
                // handle_connection opens the bi-stream and runs the full auth + main loop.
                let _ = farder_server::connection::handle_connection(state, conn).await;
            });
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let client_endpoint = make_client_endpoint();

    // 2. Owner connects with setup token
    let owner_kp = Keypair::generate();
    let (_owner_conn, mut owner_send, mut owner_recv) =
        connect_and_auth(&client_endpoint, actual_addr, &owner_kp, None, Some(&setup_hex)).await;

    // 3. Owner creates invite
    send_request(
        &mut owner_send,
        1,
        ServerRequest::CreateInvite {
            max_uses: Some(5),
            expires_in_secs: None,
            target_channel: None,
        },
    )
    .await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    let invite_code = match resp {
        ServerResponse::InviteCreated { code } => code,
        other => panic!("expected InviteCreated, got {:?}", other),
    };

    // 4. Owner gets server info, finds general channel
    send_request(&mut owner_send, 2, ServerRequest::GetServerInfo).await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    let general_channel_id = match resp {
        ServerResponse::ServerInfo { channels, .. } => {
            assert!(!channels.is_empty());
            channels[0].id
        }
        other => panic!("expected ServerInfo, got {:?}", other),
    };

    // 5. Owner subscribes to general channel
    send_request(
        &mut owner_send,
        3,
        ServerRequest::Subscribe {
            channel_ids: vec![general_channel_id],
        },
    )
    .await;
    let _ = recv_response(&mut owner_recv).await;

    // 6. Second user joins with invite
    let user_kp = Keypair::generate();
    let (user_conn, mut user_send, mut user_recv) =
        connect_and_auth(&client_endpoint, actual_addr, &user_kp, Some(&invite_code), None).await;

    // User subscribes
    send_request(
        &mut user_send,
        1,
        ServerRequest::Subscribe {
            channel_ids: vec![general_channel_id],
        },
    )
    .await;
    let _ = recv_response(&mut user_recv).await;

    // 7. User sends message
    send_request(
        &mut user_send,
        2,
        ServerRequest::SendMessage {
            channel_id: general_channel_id,
            content: "Hello from the new member!".to_string(),
            reply_to: None,
            attachment_ids: vec![],
        },
    )
    .await;
    let (_, resp) = recv_response(&mut user_recv).await;
    let msg_id = match resp {
        ServerResponse::MessageSent { id, .. } => id,
        other => panic!("expected MessageSent, got {:?}", other),
    };
    assert!(msg_id > 0);

    // 8. Owner fetches history
    send_request(
        &mut owner_send,
        4,
        ServerRequest::FetchHistory {
            channel_id: general_channel_id,
            before_id: None,
            limit: 50,
        },
    )
    .await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    match resp {
        ServerResponse::History { messages } => {
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].content, "Hello from the new member!");
            assert_eq!(messages[0].author, user_kp.public_key());
        }
        other => panic!("expected History, got {:?}", other),
    }

    // 9. Owner searches
    send_request(
        &mut owner_send,
        5,
        ServerRequest::Search {
            query: "Hello".to_string(),
            channel_id: Some(general_channel_id),
            limit: 10,
        },
    )
    .await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    match resp {
        ServerResponse::SearchResults { messages } => assert_eq!(messages.len(), 1),
        other => panic!("expected SearchResults, got {:?}", other),
    }

    // ---- FILE ATTACHMENT FLOW ----

    // 10. User uploads a file on a NEW bi-stream
    let file_data = b"This is test file content for the e2e attachment test!";
    let file_hash = {
        let mut h = Sha256::new();
        h.update(file_data);
        format!("{:x}", h.finalize())
    };

    let (mut up_send, mut up_recv) = user_conn.open_bi().await.unwrap();
    let upload_req = UploadRequest {
        channel_id: general_channel_id,
        file_name: "test-document.txt".to_string(),
        file_size: file_data.len() as u64,
        hash: file_hash.clone(),
        mime_type: "text/plain".to_string(),
        width: None,
        height: None,
        duration_secs: None,
    };
    send_frame(&mut up_send, &upload_req).await;

    // Expect Ready response
    let resp_bytes = read_frame(&mut up_recv).await;
    let resp: UploadResponse = codec::decode(&resp_bytes).unwrap();
    match resp {
        UploadResponse::Ready => {}
        other => panic!("expected Ready, got {:?}", other),
    }

    // Send file bytes
    up_send.write_all(file_data).await.unwrap();
    up_send.finish().unwrap();

    // Expect Complete response
    let resp_bytes = read_frame(&mut up_recv).await;
    let resp: UploadResponse = codec::decode(&resp_bytes).unwrap();
    let file_id = match resp {
        UploadResponse::Complete { file_id } => file_id,
        other => panic!("expected Complete, got {:?}", other),
    };
    assert!(file_id > 0);

    // 11. User sends message with attachment on main bi-stream
    send_request(&mut user_send, 3, ServerRequest::SendMessage {
        channel_id: general_channel_id,
        content: "here's a file for you".to_string(),
        reply_to: None,
        attachment_ids: vec![file_id],
    }).await;
    let (_, resp) = recv_response(&mut user_recv).await;
    match resp {
        ServerResponse::MessageSent { .. } => {}
        other => panic!("expected MessageSent, got {:?}", other),
    }

    // 12. Owner fetches history and verifies attachment is present
    send_request(&mut owner_send, 6, ServerRequest::FetchHistory {
        channel_id: general_channel_id,
        before_id: None,
        limit: 50,
    }).await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    match resp {
        ServerResponse::History { messages } => {
            let with_attach = messages.iter().find(|m| m.content == "here's a file for you").unwrap();
            assert_eq!(with_attach.attachments.len(), 1);
            assert_eq!(with_attach.attachments[0].name, "test-document.txt");
            assert_eq!(with_attach.attachments[0].size, file_data.len() as u64);
            assert_eq!(with_attach.attachments[0].mime_type, "text/plain");
        }
        other => panic!("expected History, got {:?}", other),
    }

    // 13. Test dedup: upload the same file again, should get same file_id
    let (mut up_send2, mut up_recv2) = user_conn.open_bi().await.unwrap();
    let upload_req2 = UploadRequest {
        channel_id: general_channel_id,
        file_name: "test-document-copy.txt".to_string(),
        file_size: file_data.len() as u64,
        hash: file_hash.clone(),
        mime_type: "text/plain".to_string(),
        width: None,
        height: None,
        duration_secs: None,
    };
    send_frame(&mut up_send2, &upload_req2).await;

    // Should get Complete immediately (no Ready, no bytes needed)
    let resp_bytes = read_frame(&mut up_recv2).await;
    let resp: UploadResponse = codec::decode(&resp_bytes).unwrap();
    let file_id2 = match resp {
        UploadResponse::Complete { file_id } => file_id,
        other => panic!("expected Complete (dedup), got {:?}", other),
    };
    assert_eq!(file_id, file_id2); // same file reused

    // ---- THREAD & REACTION FLOW ----

    // 14. User adds a reaction to their message
    send_request(&mut user_send, 4, ServerRequest::AddReaction {
        message_id: msg_id,
        emoji: "👍".to_string(),
    }).await;
    let (_, resp) = recv_response(&mut user_recv).await;
    match resp {
        ServerResponse::Ok => {}
        other => panic!("expected Ok for AddReaction, got {:?}", other),
    }

    // 15. Owner adds a different reaction to the same message
    send_request(&mut owner_send, 7, ServerRequest::AddReaction {
        message_id: msg_id,
        emoji: "❤️".to_string(),
    }).await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    match resp {
        ServerResponse::Ok => {}
        other => panic!("expected Ok for AddReaction, got {:?}", other),
    }

    // 16. Owner fetches history and verifies reactions are present
    send_request(&mut owner_send, 8, ServerRequest::FetchHistory {
        channel_id: general_channel_id,
        before_id: None,
        limit: 50,
    }).await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    match resp {
        ServerResponse::History { messages } => {
            // Find the message that was reacted to
            let reacted_msg = messages.iter().find(|m| m.id == msg_id).unwrap();
            assert!(reacted_msg.reactions.len() >= 2, "expected at least 2 reaction groups, got {}", reacted_msg.reactions.len());
        }
        other => panic!("expected History, got {:?}", other),
    }

    // 17. Owner creates a thread on the message
    send_request(&mut owner_send, 9, ServerRequest::CreateThread {
        message_id: msg_id,
        name: Some("discussion thread".to_string()),
    }).await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    match resp {
        ServerResponse::Ok => {}
        other => panic!("expected Ok for CreateThread, got {:?}", other),
    }

    // 18. Owner fetches history again and verifies thread metadata
    send_request(&mut owner_send, 10, ServerRequest::FetchHistory {
        channel_id: general_channel_id,
        before_id: None,
        limit: 50,
    }).await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    match resp {
        ServerResponse::History { messages } => {
            let threaded_msg = messages.iter().find(|m| m.id == msg_id).unwrap();
            assert!(threaded_msg.thread_id.is_some(), "expected thread_id to be set");
            assert_eq!(threaded_msg.thread_message_count, Some(0)); // no replies yet
        }
        other => panic!("expected History, got {:?}", other),
    }

    // ---- DATA DELETION FLOW ----

    // 19. User requests deletion
    send_request(&mut user_send, 5, ServerRequest::RequestDeletion).await;
    let (_, resp) = recv_response(&mut user_recv).await;
    match resp {
        ServerResponse::Ok => {}
        other => panic!("expected Ok for RequestDeletion, got {:?}", other),
    }

    // 20. User checks deletion status
    send_request(&mut user_send, 6, ServerRequest::GetDeletionStatus).await;
    let (_, resp) = recv_response(&mut user_recv).await;
    match resp {
        ServerResponse::DeletionStatusResp { status } => {
            assert!(status.pending);
            assert!(status.requested_at.is_some());
            assert!(status.expires_at.is_some());
        }
        other => panic!("expected DeletionStatusResp, got {:?}", other),
    }

    // 21. User cancels deletion
    send_request(&mut user_send, 7, ServerRequest::CancelDeletion).await;
    let (_, resp) = recv_response(&mut user_recv).await;
    match resp {
        ServerResponse::Ok => {}
        other => panic!("expected Ok for CancelDeletion, got {:?}", other),
    }

    // 22. Verify no longer pending
    send_request(&mut user_send, 8, ServerRequest::GetDeletionStatus).await;
    let (_, resp) = recv_response(&mut user_recv).await;
    match resp {
        ServerResponse::DeletionStatusResp { status } => assert!(!status.pending),
        other => panic!("expected DeletionStatusResp, got {:?}", other),
    }

    // ---- DM FLOW ----

    // Owner opens a DM with the user
    send_request(&mut owner_send, 11, ServerRequest::OpenDm {
        target_key: user_kp.public_key(),
    }).await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    let dm_channel_id = match resp {
        ServerResponse::DmOpened { channel, participant } => {
            assert_eq!(channel.channel_type, ChannelType::Dm);
            assert!(!participant.display_name.is_empty());
            channel.id
        }
        other => panic!("expected DmOpened, got {:?}", other),
    };

    // Owner sends a DM
    send_request(&mut owner_send, 12, ServerRequest::SendMessage {
        channel_id: dm_channel_id,
        content: "hey, private message!".to_string(),
        reply_to: None,
        attachment_ids: vec![],
    }).await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    match resp {
        ServerResponse::MessageSent { .. } => {}
        other => panic!("expected MessageSent in DM, got {:?}", other),
    }

    // Owner lists DMs
    send_request(&mut owner_send, 13, ServerRequest::ListDms).await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    match resp {
        ServerResponse::DmList { dms } => {
            assert_eq!(dms.len(), 1);
            assert!(dms[0].last_message.is_some());
            assert_eq!(dms[0].last_message.as_ref().unwrap().content, "hey, private message!");
        }
        other => panic!("expected DmList, got {:?}", other),
    }

    // Opening the same DM again returns the same channel (idempotent)
    send_request(&mut owner_send, 14, ServerRequest::OpenDm {
        target_key: user_kp.public_key(),
    }).await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    match resp {
        ServerResponse::DmOpened { channel, .. } => {
            assert_eq!(channel.id, dm_channel_id); // same channel
        }
        other => panic!("expected DmOpened (idempotent), got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Auto-claim owner e2e test
// ---------------------------------------------------------------------------

/// Fallible version of read_frame that returns Err on connection loss.
async fn try_read_frame(recv: &mut RecvStream) -> Result<Vec<u8>, String> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| format!("connection closed: {}", e))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    recv.read_exact(&mut payload)
        .await
        .map_err(|e| format!("connection closed: {}", e))?;
    Ok(payload)
}

/// Try to authenticate a client, returning Ok((conn, send, recv)) on success
/// or Err(reason) if the server sends AuthError or closes the connection.
async fn try_connect_and_auth(
    endpoint: &Endpoint,
    server_addr: SocketAddr,
    keypair: &Keypair,
    invite_code: Option<&str>,
    setup_token: Option<&str>,
) -> Result<(Connection, SendStream, RecvStream), String> {
    let conn = endpoint
        .connect(server_addr, "farder-server")
        .unwrap()
        .await
        .expect("connect");
    let (mut send, mut recv) = conn.accept_bi().await.expect("accept_bi");

    // Receive Challenge
    let frame = recv_server_frame(&mut recv).await;
    let nonce = match frame {
        ServerFrame::Challenge { nonce } => nonce,
        other => panic!("expected Challenge, got {:?}", other),
    };

    // Sign and send Authenticate
    let signature = keypair.sign(&nonce);
    let auth_frame = ClientFrame::Authenticate {
        public_key: keypair.public_key(),
        signed_challenge: signature,
        invite_code: invite_code.map(|s| s.to_string()),
        setup_token: setup_token.map(|s| s.to_string()),
    };
    send_frame(&mut send, &auth_frame).await;

    // Receive Authenticated or AuthError (server may close connection after AuthError)
    let data = try_read_frame(&mut recv).await?;
    let frame: ServerFrame = codec::decode(&data).expect("decode server frame");
    match frame {
        ServerFrame::Authenticated { .. } => Ok((conn, send, recv)),
        ServerFrame::AuthError { reason } => Err(reason),
        other => panic!("expected Authenticated or AuthError, got {:?}", other),
    }
}

#[tokio::test]
async fn test_auto_claim_first_connection() {
    // Install rustls crypto provider
    rustls::crypto::ring::default_provider().install_default().ok();

    // 1. Set up server in-process with in-memory DB and NO setup token
    let bind_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let server_endpoint = make_server_endpoint(bind_addr);
    let actual_addr = server_endpoint.local_addr().unwrap();

    let conn = farder_server::db::open_in_memory().unwrap();
    farder_server::members::create_role(
        &conn,
        "@everyone",
        farder_server::permissions::DEFAULT_EVERYONE,
        None,
        0,
        true,
    )
    .unwrap();
    let templates = farder_server::templates::list_builtin_templates();
    let blank = templates
        .iter()
        .find(|t| t.template.name == "Blank")
        .unwrap();
    farder_server::templates::apply_template(&conn, blank).unwrap();

    let tmp_dir = std::env::temp_dir().join(format!("farder-e2e-autoclaim-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let state = Arc::new(farder_server::state::ServerState::new(
        conn,
        "Auto-Claim Test Server".to_string(),
        tmp_dir.to_string_lossy().to_string(),
        50 * 1024 * 1024,
    ));
    // No setup token is set — the server has zero members and no owner.

    // Spawn server accept loop
    let server_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            let incoming = match server_endpoint.accept().await {
                Some(inc) => inc,
                None => break,
            };
            let state = Arc::clone(&server_state);
            tokio::spawn(async move {
                let conn = incoming.await.unwrap();
                let _ = farder_server::connection::handle_connection(state, conn).await;
            });
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let client_endpoint = make_client_endpoint();

    // 2. First client connects with NO invite and NO setup token — should auto-claim
    let owner_kp = Keypair::generate();
    let result = try_connect_and_auth(
        &client_endpoint,
        actual_addr,
        &owner_kp,
        None,
        None,
    )
    .await;
    assert!(result.is_ok(), "first connection should succeed via auto-claim, got: {:?}", result.err());
    let (_owner_conn, mut owner_send, mut owner_recv) = result.unwrap();

    // 3. Verify the client is the owner and member_count == 1
    send_request(&mut owner_send, 1, ServerRequest::GetServerInfo).await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    match resp {
        ServerResponse::ServerInfo { member_count, .. } => {
            assert_eq!(member_count, 1, "auto-claimed owner should be the only member");
        }
        other => panic!("expected ServerInfo, got {:?}", other),
    }

    // 4. Second client connects with NO invite and NO setup token — should be REJECTED
    let intruder_kp = Keypair::generate();
    let result = try_connect_and_auth(
        &client_endpoint,
        actual_addr,
        &intruder_kp,
        None,
        None,
    )
    .await;
    assert!(result.is_err(), "second connection without invite should be rejected");
    let reason = result.unwrap_err();
    // The server sends AuthError then closes the connection. Depending on timing,
    // the client may receive the AuthError frame or see a connection-closed error.
    assert!(
        reason == "no invite code or setup token provided"
            || reason.contains("connection closed"),
        "unexpected rejection reason: {}",
        reason,
    );
}
