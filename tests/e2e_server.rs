use farder_crypto::identity::Keypair;
use farder_protocol::{codec, server::*};
use quinn::{Endpoint, RecvStream, SendStream};
use rustls::pki_types::ServerName;
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
) -> (SendStream, RecvStream) {
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

    (send, recv)
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

    let state = Arc::new(farder_server::state::ServerState::new(
        conn,
        "Test Server".to_string(),
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
                // The server opens the bi-directional stream so it can immediately send the
                // Challenge frame. (In QUIC/quinn the accepting side only wakes when the opening
                // side writes; since our protocol has the server send first, the server must be
                // the stream opener.)
                let (send, recv) = conn.open_bi().await.unwrap();
                let _ = farder_server::connection::handle_client(state, send, recv).await;
            });
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let client_endpoint = make_client_endpoint();

    // 2. Owner connects with setup token
    let owner_kp = Keypair::generate();
    let (mut owner_send, mut owner_recv) =
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
    let (mut user_send, mut user_recv) =
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
}
