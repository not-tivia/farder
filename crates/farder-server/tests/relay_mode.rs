//! End-to-end integration tests for relay-mode serving (Phase 2, Task 6).
//!
//! Each test stands up three real components on localhost over QUIC:
//!   1. A minimal **relay** test-double (an inline copy of the Phase-1 relay's
//!      register/connect/bridge logic — `farder-relay` is a binary crate and so
//!      cannot be imported; the bridge is ~40 lines, so we re-create it here and
//!      clearly mark it as a test double).
//!   2. A real **server** in relay-mode via `farder_server::relay::serve_via_relay`,
//!      which dials the relay, registers under its `server_id`, and serves each
//!      bridged stream with the production auth / main_loop / file-transfer code.
//!   3. A simulated **client** that connects THROUGH the relay (RelayConnect),
//!      then runs the real client protocol (auth handshake, requests, uploads)
//!      exactly as the Phase-3 client will.
//!
//! This proves the layers are wired together end to end — not just that each
//! compiles. All traffic flows relay -> server; the server never sees the client
//! directly.

use farder_crypto::identity::Keypair;
use farder_protocol::{
    codec,
    messages::Message,
    server::{
        ChannelType, ClientFrame, RelayStreamRole, ServerFrame, ServerRequest, ServerResponse,
        UploadRequest, UploadResponse, DownloadRequest, DownloadResponse,
    },
};
use farder_server::state::ServerState;
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// 4-byte big-endian length-prefixed framing.
// MUST match connection.rs `read_frame`/`write_frame`, relay.rs
// `read_framed`/`write_framed`, and the relay's `read_message`/`write_message`.
// ---------------------------------------------------------------------------

async fn write_framed(send: &mut SendStream, data: &[u8]) {
    send.write_all(&(data.len() as u32).to_be_bytes()).await.unwrap();
    send.write_all(data).await.unwrap();
}

async fn read_framed(recv: &mut RecvStream) -> Vec<u8> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await.unwrap();
    let n = u32::from_be_bytes(len) as usize;
    assert!(n <= 16 * 1024 * 1024, "frame too large: {n}");
    let mut buf = vec![0u8; n];
    recv.read_exact(&mut buf).await.unwrap();
    buf
}

fn ensure_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

// ---------------------------------------------------------------------------
// Skip-verify QUIC client endpoint (matches relay.rs / e2e patterns;
// real cert pinning is Phase 3).
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct SkipVerify;
impl rustls::client::danger::ServerCertVerifier for SkipVerify {
    fn verify_server_cert(
        &self,
        _e: &rustls::pki_types::CertificateDer<'_>,
        _i: &[rustls::pki_types::CertificateDer<'_>],
        _n: &rustls::pki_types::ServerName<'_>,
        _o: &[u8],
        _t: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _m: &[u8],
        _c: &rustls::pki_types::CertificateDer<'_>,
        _d: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _m: &[u8],
        _c: &rustls::pki_types::CertificateDer<'_>,
        _d: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn test_client_endpoint() -> Endpoint {
    let crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipVerify))
        .with_no_client_auth();
    let client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
    ));
    let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(client_config);
    endpoint
}

// ---------------------------------------------------------------------------
// Minimal in-test relay (test double of the Phase-1 relay in
// `farder-relay/src/router.rs`). It accepts QUIC connections; the first
// bi-stream carries either a RelayRegister (server control channel) or a
// RelayConnect (client wanting to be bridged to a registered server). For a
// connected client, every NEW bi-stream the client opens is bridged to a fresh
// bi-stream on the server's control connection, copying bytes both ways. This
// is exactly the behaviour `serve_via_relay` expects from the real relay.
// ---------------------------------------------------------------------------

type ConnectionMap = Arc<RwLock<HashMap<Vec<u8>, Connection>>>;

/// Build a self-signed QUIC *server* endpoint for the test relay.
fn relay_server_endpoint() -> Endpoint {
    let cert = rcgen::generate_simple_self_signed(vec!["farder-relay".to_string()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(cert.key_pair.serialize_der()).unwrap();
    let server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .unwrap();
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto).unwrap(),
    ));
    Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap()
}

/// Start the test-double relay on an ephemeral port; return its address.
async fn start_relay() -> SocketAddr {
    ensure_provider();
    let ep = relay_server_endpoint();
    let addr = ep.local_addr().unwrap();
    let conns: ConnectionMap = Arc::new(RwLock::new(HashMap::new()));
    tokio::spawn(async move {
        while let Some(incoming) = ep.accept().await {
            let conns = conns.clone();
            tokio::spawn(async move {
                let conn = match incoming.await {
                    Ok(c) => c,
                    Err(_) => return,
                };
                let (send, mut recv) = match conn.accept_bi().await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let msg: Message = match codec::decode(&read_framed(&mut recv).await) {
                    Ok(m) => m,
                    Err(_) => return,
                };
                match msg {
                    Message::RelayRegister { server_id } => {
                        relay_register(server_id, conn, send, conns).await;
                    }
                    Message::RelayConnect { destination_id } => {
                        relay_connect(destination_id, conn, send, conns).await;
                    }
                    _ => {}
                }
            });
        }
    });
    addr
}

async fn relay_register(server_id: Vec<u8>, conn: Connection, mut send: SendStream, conns: ConnectionMap) {
    conns.write().await.insert(server_id.clone(), conn.clone());
    write_framed(&mut send, &codec::encode(&Message::RelayRegistered).unwrap()).await;
    // Hold the control channel open; clean up on close.
    conn.closed().await;
    let mut map = conns.write().await;
    if let Some(existing) = map.get(&server_id) {
        if existing.stable_id() == conn.stable_id() {
            map.remove(&server_id);
        }
    }
}

async fn relay_connect(destination_id: Vec<u8>, client_conn: Connection, mut send: SendStream, conns: ConnectionMap) {
    let dest = conns.read().await.get(&destination_id).cloned();
    match dest {
        Some(server_conn) => {
            write_framed(&mut send, &codec::encode(&Message::RelayConnected).unwrap()).await;
            // Bridge every client bi-stream to a fresh server bi-stream.
            loop {
                let (mut c_send, mut c_recv) = match client_conn.accept_bi().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let (mut s_send, mut s_recv) = match server_conn.open_bi().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    let _ = tokio::io::copy(&mut c_recv, &mut s_send).await;
                    let _ = s_send.finish();
                });
                tokio::spawn(async move {
                    let _ = tokio::io::copy(&mut s_recv, &mut c_send).await;
                    let _ = c_send.finish();
                });
            }
        }
        None => {
            let err = Message::RelayError { reason: "destination not connected".to_string() };
            write_framed(&mut send, &codec::encode(&err).unwrap()).await;
            let _ = send.finish();
            client_conn.closed().await;
        }
    }
}

// ---------------------------------------------------------------------------
// Test harness: start a real relay-mode server registered with the relay.
// ---------------------------------------------------------------------------

/// Spawn `serve_via_relay` for a fresh in-memory server and wait until it has
/// registered with the relay (poll by attempting a client connect until it is
/// no longer rejected — but simpler: just sleep-poll a RelayConnect probe).
async fn start_relay_server(relay: SocketAddr) -> ([u8; 32], Arc<ServerState>) {
    let mut state = ServerState::new_for_test().unwrap();
    // Give each test its own storage dir so uploaded files don't collide.
    let storage = tempfile::tempdir().unwrap();
    state.storage_dir = storage.path().to_string_lossy().to_string();
    std::mem::forget(storage); // keep dir alive for the process
    let state = Arc::new(state);
    let server_id: [u8; 32] = rand::random();
    let state_for_task = Arc::clone(&state);
    tokio::spawn(async move {
        let _ = farder_server::relay::serve_via_relay(state_for_task, relay, server_id).await;
    });
    (server_id, state)
}

/// Connect a fresh client through the relay to `server_id`. Returns the bridged
/// client connection (on which the client opens role-tagged streams).
async fn client_via_relay(relay: SocketAddr, server_id: &[u8; 32]) -> (Endpoint, Connection) {
    // Retry the RelayConnect a few times: the server may not have registered yet.
    for _ in 0..100 {
        let ep = test_client_endpoint();
        let conn = ep.connect(relay, "farder-relay").unwrap().await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let msg = Message::RelayConnect { destination_id: server_id.to_vec() };
        write_framed(&mut send, &codec::encode(&msg).unwrap()).await;
        let reply: Message = codec::decode(&read_framed(&mut recv).await).unwrap();
        match reply {
            Message::RelayConnected => return (ep, conn),
            Message::RelayError { .. } => {
                // Server not registered yet; close and retry.
                drop(conn);
                drop(ep);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => panic!("unexpected relay reply: {other:?}"),
        }
    }
    panic!("server never registered with relay");
}

/// Open a Primary stream through the relay and run the real auth handshake.
/// The first client on a fresh server auto-claims owner (no invite/setup token),
/// matching the production `authenticate` path. Returns the (stream pair, the
/// session token issued by the server).
async fn login_primary(
    conn: &Connection,
    keypair: &Keypair,
) -> (SendStream, RecvStream, Vec<u8>) {
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    // Role tag: Primary.
    write_framed(&mut send, &codec::encode(&RelayStreamRole::Primary).unwrap()).await;

    // 1. Server -> Challenge { nonce }
    let frame: ServerFrame = codec::decode(&read_framed(&mut recv).await).unwrap();
    let nonce = match frame {
        ServerFrame::Challenge { nonce } => nonce,
        other => panic!("expected Challenge, got {other:?}"),
    };

    // 2. Client -> Authenticate (sign the nonce; no invite/setup -> auto-claim owner)
    let signed = keypair.sign(&nonce);
    let auth = ClientFrame::Authenticate {
        public_key: keypair.public_key(),
        signed_challenge: signed,
        invite_code: None,
        setup_token: None,
    };
    write_framed(&mut send, &codec::encode(&auth).unwrap()).await;

    // 3. Server -> Authenticated { session_token }
    let frame: ServerFrame = codec::decode(&read_framed(&mut recv).await).unwrap();
    let token = match frame {
        ServerFrame::Authenticated { session_token } => session_token,
        other => panic!("expected Authenticated, got {other:?}"),
    };
    (send, recv, token)
}

/// Send a `Request` on an established Primary stream and read the response body.
async fn request(send: &mut SendStream, recv: &mut RecvStream, id: u32, body: ServerRequest) -> ServerResponse {
    let frame = ClientFrame::Request { id, body };
    write_framed(send, &codec::encode(&frame).unwrap()).await;
    loop {
        let resp: ServerFrame = codec::decode(&read_framed(recv).await).unwrap();
        match resp {
            ServerFrame::Response { request_id, body } => {
                assert_eq!(request_id, id, "response id must match request id");
                return body;
            }
            // The server may push events (e.g. MemberJoined) interleaved; skip
            // them and keep reading until we get our Response.
            ServerFrame::Event(_) => continue,
            other => panic!("expected Response, got {other:?}"),
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[tokio::test]
async fn relayed_client_logs_in_and_makes_a_request() {
    ensure_provider();
    let relay = start_relay().await;
    let (server_id, _state) = start_relay_server(relay).await;

    let (_ep, conn) = client_via_relay(relay, &server_id).await;
    let keypair = Keypair::generate();
    let (mut send, mut recv, token) = login_primary(&conn, &keypair).await;
    assert_eq!(token.len(), 32, "session token must be 32 bytes");

    // Real request over the relay: GetServerInfo. The server patches in the
    // real name + owner pubkey, proving the production main_loop served it.
    let resp = request(&mut send, &mut recv, 1, ServerRequest::GetServerInfo).await;
    match resp {
        ServerResponse::ServerInfo { name, member_count, owner_public_key, .. } => {
            assert_eq!(name, "Test Server", "server name must be served from state");
            assert_eq!(member_count, 1, "the relayed client is the sole member");
            assert_eq!(
                owner_public_key.as_ref(),
                Some(&keypair.public_key()),
                "first relayed client must have auto-claimed owner"
            );
        }
        other => panic!("expected ServerInfo, got {other:?}"),
    }
}

#[tokio::test]
async fn relayed_client_uploads_a_file() {
    ensure_provider();
    let relay = start_relay().await;
    let (server_id, _state) = start_relay_server(relay).await;

    let (_ep, conn) = client_via_relay(relay, &server_id).await;
    let keypair = Keypair::generate();
    let (mut send, mut recv, token) = login_primary(&conn, &keypair).await;

    // Owner creates a channel to upload into, then learns its id via GetServerInfo.
    let resp = request(
        &mut send,
        &mut recv,
        10,
        ServerRequest::CreateChannel {
            name: "general".to_string(),
            channel_type: ChannelType::Text,
            category_id: None,
            position: Some(0),
        },
    )
    .await;
    assert!(matches!(resp, ServerResponse::Ok), "channel creation should succeed: {resp:?}");

    let resp = request(&mut send, &mut recv, 11, ServerRequest::GetServerInfo).await;
    let channel_id = match resp {
        ServerResponse::ServerInfo { channels, .. } => {
            channels.iter().find(|c| c.name == "general").map(|c| c.id).expect("general channel present")
        }
        other => panic!("expected ServerInfo, got {other:?}"),
    };

    // Upload a file on a NEW relay-bridged Session stream (auth'd by token).
    let file_bytes = b"the quick brown fox jumps over the lazy dog -- relayed upload".to_vec();
    let hash = farder_server::attachments::compute_sha256(&file_bytes);

    let (mut u_send, mut u_recv) = conn.open_bi().await.unwrap();
    write_framed(
        &mut u_send,
        &codec::encode(&RelayStreamRole::Session { token: token.clone() }).unwrap(),
    )
    .await;
    let upload_req = UploadRequest {
        channel_id,
        file_name: "fox.txt".to_string(),
        file_size: file_bytes.len() as u64,
        hash: hash.clone(),
        mime_type: "text/plain".to_string(),
        width: None,
        height: None,
        duration_secs: None,
    };
    write_framed(&mut u_send, &codec::encode(&upload_req).unwrap()).await;

    // Server -> Ready, then we stream the bytes, then Complete { file_id }.
    let resp: UploadResponse = codec::decode(&read_framed(&mut u_recv).await).unwrap();
    assert!(matches!(resp, UploadResponse::Ready), "expected Ready, got {resp:?}");
    u_send.write_all(&file_bytes).await.unwrap();
    let resp: UploadResponse = codec::decode(&read_framed(&mut u_recv).await).unwrap();
    let file_id = match resp {
        UploadResponse::Complete { file_id } => file_id,
        other => panic!("expected Complete, got {other:?}"),
    };

    // Prove the file is actually stored AND downloadable through the relay:
    // open another Session stream and download it, asserting the exact bytes.
    let (mut d_send, mut d_recv) = conn.open_bi().await.unwrap();
    write_framed(
        &mut d_send,
        &codec::encode(&RelayStreamRole::Session { token: token.clone() }).unwrap(),
    )
    .await;
    write_framed(&mut d_send, &codec::encode(&DownloadRequest { file_id }).unwrap()).await;
    let resp: DownloadResponse = codec::decode(&read_framed(&mut d_recv).await).unwrap();
    match resp {
        DownloadResponse::Start { file_name, file_size, hash: dl_hash, .. } => {
            assert_eq!(file_name, "fox.txt");
            assert_eq!(file_size, file_bytes.len() as u64);
            assert_eq!(dl_hash, hash, "stored hash must match");
        }
        other => panic!("expected Start, got {other:?}"),
    }
    // Remaining stream bytes are the file contents.
    let got = d_recv.read_to_end(1024 * 1024).await.unwrap();
    assert_eq!(got, file_bytes, "downloaded bytes must match what we uploaded");
}

#[tokio::test]
async fn bad_session_token_is_rejected() {
    ensure_provider();
    let relay = start_relay().await;
    let (server_id, _state) = start_relay_server(relay).await;

    let (_ep, conn) = client_via_relay(relay, &server_id).await;
    let keypair = Keypair::generate();
    let (mut send, mut recv, _token) = login_primary(&conn, &keypair).await;

    // Open a Session stream with a RANDOM (never-issued) token. The server's
    // run_relay_aux does lookup_session(token) and errs, tearing down the stream
    // before any auxiliary protocol runs. We send a DownloadRequest and expect
    // NO valid DownloadResponse — the stream closes / read fails instead.
    let bogus: Vec<u8> = (0..32u8).map(|_| rand::random()).collect();
    let (mut b_send, mut b_recv) = conn.open_bi().await.unwrap();
    write_framed(
        &mut b_send,
        &codec::encode(&RelayStreamRole::Session { token: bogus }).unwrap(),
    )
    .await;
    write_framed(&mut b_send, &codec::encode(&DownloadRequest { file_id: 1 }).unwrap()).await;

    // The bridged stream should yield no usable DownloadResponse: either the
    // read fails (stream reset/closed) or it returns EOF with no frame.
    let rejected = match tokio::time::timeout(Duration::from_secs(5), b_recv.read_to_end(64 * 1024)).await {
        Err(_elapsed) => panic!("server hung on a bad token instead of rejecting the stream"),
        Ok(Err(_)) => true, // stream reset / connection error — rejected
        Ok(Ok(bytes)) => {
            // Closed cleanly with no decodable DownloadResponse payload.
            bytes.is_empty() || codec::decode::<DownloadResponse>(&strip_frame(&bytes)).is_err()
        }
    };
    assert!(rejected, "a random session token must NOT be served");

    // The PRIMARY stream's session is unaffected: a follow-up request still works.
    let resp = request(&mut send, &mut recv, 99, ServerRequest::GetServerInfo).await;
    assert!(
        matches!(resp, ServerResponse::ServerInfo { .. }),
        "primary session must keep working after a bad-token stream was rejected: {resp:?}"
    );
}

/// Strip a single 4-byte length prefix if present, returning the inner payload
/// (used only to attempt-decode whatever, if anything, the rejected stream sent).
fn strip_frame(bytes: &[u8]) -> Vec<u8> {
    if bytes.len() < 4 {
        return bytes.to_vec();
    }
    let n = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    if bytes.len() >= 4 + n {
        bytes[4..4 + n].to_vec()
    } else {
        bytes.to_vec()
    }
}
