//! Task 8: the headless two-client E2EE harness.
//!
//! Two independent identities, in one process, against an in-process server:
//! the owner creates an E2EE channel, both identities publish KeyPackages, the
//! owner bootstraps the MLS group and adds the joiner, the joiner fetches its
//! Welcome, joins and confirms its leaf, then both exchange sealed messages that
//! the other actually decrypts. An observation test proves the plaintext reached
//! NO table, and a negative test proves a server member that is NOT in the MLS
//! group cannot decrypt the ciphertext it can fetch.
//!
//! The whole point (plan Decision 1) is that this harness exercises the SHIPPED
//! vertical — `farder-e2ee-client` — over a real QUIC connection, not a
//! reimplementation. The only transport-specific code here is the
//! [`QuicE2eeTransport`] that adapts the trait to the wire; every lifecycle and
//! crypto decision is the crate's.

mod common;

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use farder_crypto::event_log::{
    device_id, invite_code_hash, DeviceCert, Event, EventPayload, E2EE_CHANNEL_ID_FLOOR,
};
use farder_crypto::identity::{Keypair, PublicKey};
use farder_e2ee_client::{
    add_member, bootstrap_group, confirm_leaf, create_e2ee_channel, create_joiner_store,
    event_now_secs, fetch_pending_welcomes, join_channel, publish_key_package, receive_sealed,
    resume_store, send_sealed, Actor, ChainState, ChannelKey, ChannelSpec, E2eeTransport,
    EventAccepted, MlsControl, SealContext, SealedOutcome, SendEligibility, StewardContext,
    TransportError, Welcomes,
};
use farder_mls::credential::{credential_with_key, DeviceSigner};
use farder_mls::group::{DeclaredMember, MlsChannelGroup};
use farder_mls::store::FarderMlsStore;
use farder_protocol::server::{
    ClientFrame, MessageInfoV2, ServerFrame, ServerRequest, ServerResponse, SERVER_PROTOCOL_VERSION,
};
use farder_protocol::codec;
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use rustls::pki_types::ServerName;

/// The channel id the owner chooses (must be at/above the E2EE floor).
const CHANNEL_ID: u64 = E2EE_CHANNEL_ID_FLOOR + 4242;

/// Distinctive plaintexts the harness asserts decrypt exactly, and which the
/// observation test asserts appear in NO table.
const OWNER_PLAINTEXT: &str = "FARDER-CANARY-owner-sealed-4b91a0";
const JOINER_PLAINTEXT: &str = "FARDER-CANARY-joiner-sealed-7d3c22";

static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_dir(name: &str) -> PathBuf {
    let n = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "farder-e2ee-two-client-{name}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

// ---------------------------------------------------------------------------
// TLS skip verifier (self-signed test server)
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
    let cert_der = rustls::pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(certified.key_pair.serialize_der())
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
// Frame I/O helpers (panicking: used only in the auth preamble, where a failure
// is a test bug, not something to route through the transport error type)
// ---------------------------------------------------------------------------

async fn read_frame(recv: &mut RecvStream) -> Result<Vec<u8>, String> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| format!("read frame length: {e}"))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    recv.read_exact(&mut payload)
        .await
        .map_err(|e| format!("read frame payload: {e}"))?;
    Ok(payload)
}

async fn write_frame(send: &mut SendStream, data: &[u8]) -> Result<(), String> {
    let len = (data.len() as u32).to_be_bytes();
    send.write_all(&len)
        .await
        .map_err(|e| format!("write frame length: {e}"))?;
    send.write_all(data)
        .await
        .map_err(|e| format!("write frame payload: {e}"))?;
    Ok(())
}

async fn send_frame(send: &mut SendStream, frame: &impl serde::Serialize) {
    let data = codec::encode(frame).expect("encode frame");
    write_frame(send, &data).await.expect("write frame");
}

async fn recv_server_frame(recv: &mut RecvStream) -> ServerFrame {
    let data = read_frame(recv).await.expect("read frame");
    codec::decode(&data).expect("decode server frame")
}

// ---------------------------------------------------------------------------
// The transport: `E2eeTransport` over the real QUIC connection
// ---------------------------------------------------------------------------

/// Adapts [`E2eeTransport`] to a live QUIC connection's request/response bi
/// stream. The trait methods take `&self` but writing a `quinn::SendStream`
/// needs `&mut`, so both streams sit behind a `tokio::sync::Mutex` (held across
/// `.await`). The trait is deliberately not object-safe, so this is a concrete
/// type, never `dyn`.
struct QuicE2eeTransport {
    send: tokio::sync::Mutex<SendStream>,
    recv: tokio::sync::Mutex<RecvStream>,
    next_id: AtomicU32,
}

impl QuicE2eeTransport {
    fn new(send: SendStream, recv: RecvStream) -> Self {
        Self {
            send: tokio::sync::Mutex::new(send),
            recv: tokio::sync::Mutex::new(recv),
            next_id: AtomicU32::new(1),
        }
    }

    /// One request/response round trip on the connection's request stream,
    /// skipping any broadcast `Event` frames the server interleaves.
    async fn round_trip(&self, body: ServerRequest) -> Result<ServerResponse, TransportError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = ClientFrame::Request { id, body };
        let data = codec::encode(&frame)
            .map_err(|e| TransportError::transport(format!("encode request: {e}")))?;
        {
            let mut send = self.send.lock().await;
            write_frame(&mut send, &data)
                .await
                .map_err(TransportError::transport)?;
        }
        loop {
            let bytes = {
                let mut recv = self.recv.lock().await;
                read_frame(&mut recv)
                    .await
                    .map_err(TransportError::transport)?
            };
            let frame: ServerFrame = codec::decode(&bytes)
                .map_err(|e| TransportError::transport(format!("decode response: {e}")))?;
            match frame {
                ServerFrame::Response { request_id, body } if request_id == id => return Ok(body),
                ServerFrame::Event(_) | ServerFrame::Response { .. } => continue,
                other => {
                    return Err(TransportError::transport(format!(
                        "unexpected frame while awaiting response: {other:?}"
                    )));
                }
            }
        }
    }
}

impl E2eeTransport for QuicE2eeTransport {
    fn submit_event(
        &self,
        event: &Event,
    ) -> impl Future<Output = Result<EventAccepted, TransportError>> + Send {
        let event = event.clone();
        async move {
            match self.round_trip(ServerRequest::SubmitEvent { event }).await? {
                ServerResponse::EventAccepted { event_hash, timestamp } => {
                    Ok(EventAccepted { event_hash, timestamp })
                }
                ServerResponse::Error { reason } => Err(TransportError::rejected(reason)),
                other => Err(TransportError::transport(format!(
                    "unexpected SubmitEvent response: {other:?}"
                ))),
            }
        }
    }

    fn fetch_welcomes(
        &self,
        channel_id: Option<u64>,
        since_accept_seq: u64,
    ) -> impl Future<Output = Result<Welcomes, TransportError>> + Send {
        async move {
            match self
                .round_trip(ServerRequest::FetchWelcomes { channel_id, since_accept_seq })
                .await?
            {
                ServerResponse::Welcomes { events, next_accept_seq, more } => {
                    Ok(Welcomes { events, next_accept_seq, more })
                }
                ServerResponse::Error { reason } => Err(TransportError::rejected(reason)),
                other => Err(TransportError::transport(format!(
                    "unexpected FetchWelcomes response: {other:?}"
                ))),
            }
        }
    }

    fn fetch_mls_control(
        &self,
        channel_id: u64,
        since_accept_seq: u64,
    ) -> impl Future<Output = Result<MlsControl, TransportError>> + Send {
        async move {
            match self
                .round_trip(ServerRequest::FetchMlsControl { channel_id, since_accept_seq })
                .await?
            {
                ServerResponse::MlsControl { events, next_accept_seq, more } => {
                    Ok(MlsControl { events, next_accept_seq, more })
                }
                ServerResponse::Error { reason } => Err(TransportError::rejected(reason)),
                other => Err(TransportError::transport(format!(
                    "unexpected FetchMlsControl response: {other:?}"
                ))),
            }
        }
    }

    fn fetch_key_packages(
        &self,
        member: &PublicKey,
        device: &str,
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, TransportError>> + Send {
        let member = member.clone();
        let device = device.to_string();
        async move {
            match self
                .round_trip(ServerRequest::FetchKeyPackages { member, device })
                .await?
            {
                ServerResponse::KeyPackages { events } => Ok(events),
                ServerResponse::Error { reason } => Err(TransportError::rejected(reason)),
                other => Err(TransportError::transport(format!(
                    "unexpected FetchKeyPackages response: {other:?}"
                ))),
            }
        }
    }

    fn fetch_device_certs(
        &self,
        identity: &PublicKey,
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, TransportError>> + Send {
        let identity = identity.clone();
        async move {
            match self
                .round_trip(ServerRequest::FetchDeviceCerts { identity })
                .await?
            {
                ServerResponse::DeviceCerts { events } => Ok(events),
                ServerResponse::Error { reason } => Err(TransportError::rejected(reason)),
                other => Err(TransportError::transport(format!(
                    "unexpected FetchDeviceCerts response: {other:?}"
                ))),
            }
        }
    }

    fn fetch_history_v2(
        &self,
        channel_id: u64,
        before_id: Option<u64>,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<MessageInfoV2>, TransportError>> + Send {
        async move {
            match self
                .round_trip(ServerRequest::FetchHistoryV2 { channel_id, before_id, limit })
                .await?
            {
                ServerResponse::HistoryV2 { messages } => Ok(messages),
                ServerResponse::Error { reason } => Err(TransportError::rejected(reason)),
                other => Err(TransportError::transport(format!(
                    "unexpected FetchHistoryV2 response: {other:?}"
                ))),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Connection + auth + negotiation
// ---------------------------------------------------------------------------

/// Connect, run the `Challenge` → `Authenticate` → `Authenticated` preamble,
/// then `NegotiateProtocol { client_version: 2 }` — on EVERY connection (fact
/// A2.7). Returns the v2-negotiated transport plus the connection handle (kept
/// alive by the caller).
async fn connect_identity(
    endpoint: &Endpoint,
    server_addr: SocketAddr,
    keypair: &Keypair,
    invite_code: Option<&str>,
    setup_token: Option<&str>,
) -> (QuicE2eeTransport, Connection) {
    let conn = endpoint
        .connect(server_addr, "farder-server")
        .unwrap()
        .await
        .expect("connect");
    // The server opens the bi stream (so it can send the Challenge first).
    let (mut send, mut recv) = conn.accept_bi().await.expect("accept_bi");

    // Challenge.
    let frame = recv_server_frame(&mut recv).await;
    let nonce = match frame {
        ServerFrame::Challenge { nonce } => nonce,
        other => panic!("expected Challenge, got {other:?}"),
    };

    // Authenticate.
    let signature = keypair.sign(&nonce);
    let auth = ClientFrame::Authenticate {
        public_key: keypair.public_key(),
        signed_challenge: signature,
        invite_code: invite_code.map(|s| s.to_string()),
        setup_token: setup_token.map(|s| s.to_string()),
    };
    send_frame(&mut send, &auth).await;

    // Authenticated.
    let frame = recv_server_frame(&mut recv).await;
    match frame {
        ServerFrame::Authenticated { .. } => {}
        ServerFrame::AuthError { reason } => panic!("auth error: {reason}"),
        other => panic!("expected Authenticated, got {other:?}"),
    }

    let transport = QuicE2eeTransport::new(send, recv);

    // Negotiate the v2 protocol on EVERY connection, before anything else.
    let resp = transport
        .round_trip(ServerRequest::NegotiateProtocol {
            client_version: SERVER_PROTOCOL_VERSION,
        })
        .await
        .expect("negotiate protocol");
    match resp {
        ServerResponse::ProtocolVersion { server_version, .. } => {
            assert_eq!(server_version, SERVER_PROTOCOL_VERSION);
        }
        other => panic!("expected ProtocolVersion, got {other:?}"),
    }

    (transport, conn)
}

// ---------------------------------------------------------------------------
// Small request helpers
// ---------------------------------------------------------------------------

async fn request(t: &QuicE2eeTransport, body: ServerRequest) -> ServerResponse {
    t.round_trip(body).await.expect("round-trip")
}

async fn submit_raw(t: &QuicE2eeTransport, event: &Event) {
    match request(t, ServerRequest::SubmitEvent { event: event.clone() }).await {
        ServerResponse::EventAccepted { .. } => {}
        other => panic!("expected EventAccepted, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Server setup
// ---------------------------------------------------------------------------

async fn spawn_server() -> (SocketAddr, Arc<farder_server::state::ServerState>, Endpoint, String) {
    rustls::crypto::ring::default_provider().install_default().ok();

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
        false,
    )
    .unwrap();
    let templates = farder_server::templates::list_builtin_templates();
    let blank = templates.iter().find(|t| t.template.name == "Blank").unwrap();
    farder_server::templates::apply_template(&conn, blank).unwrap();

    let tmp_dir = std::env::temp_dir().join(format!("farder-e2ee-{}", std::process::id()));
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
    (actual_addr, state, client_endpoint, setup_hex)
}

// ---------------------------------------------------------------------------
// The full E2EE path (driven through the shipped vertical)
// ---------------------------------------------------------------------------

struct DriveResult {
    server_id: String,
    channel_id: u64,
    invite_code: String,
    owner_pk: PublicKey,
    owner_ciphertext: Vec<u8>,
    joiner_ciphertext: Vec<u8>,
}

async fn fetch_ciphertext_by_author(
    t: &QuicE2eeTransport,
    channel_id: u64,
    author: &PublicKey,
) -> Vec<u8> {
    let msgs = t
        .fetch_history_v2(channel_id, None, 10)
        .await
        .expect("fetch history v2");
    msgs.into_iter()
        .find(|m| m.is_e2ee && &m.base.author == author)
        .and_then(|m| m.sealed)
        .expect("sealed ciphertext for author not found")
}

async fn drive_full_path(
    endpoint: &Endpoint,
    addr: SocketAddr,
    _state: &Arc<farder_server::state::ServerState>,
    setup_hex: &str,
    owner_plaintext: &str,
    joiner_plaintext: &str,
) -> DriveResult {
    // ---- Owner connects with the setup token ----
    let owner_kp = Keypair::generate();
    let owner_dev = Keypair::generate();
    let (owner_transport, _owner_conn) =
        connect_identity(endpoint, addr, &owner_kp, None, Some(setup_hex)).await;

    // Owner creates an invite (legacy) + learns the log server id.
    let invite_code = match request(
        &owner_transport,
        ServerRequest::CreateInvite {
            max_uses: Some(5),
            expires_in_secs: None,
            target_channel: None,
        },
    )
    .await
    {
        ServerResponse::InviteCreated { code } => code,
        other => panic!("expected InviteCreated, got {other:?}"),
    };
    let server_id = match request(&owner_transport, ServerRequest::GetServerInfo).await {
        ServerResponse::ServerInfo {
            server_id: Some(id), ..
        } => id,
        other => panic!("expected ServerInfo with server_id, got {other:?}"),
    };

    // ---- Owner mesh-log join (DeviceAuthorized -> InviteCreated) ----
    let now = event_now_secs();
    let owner_cert = DeviceCert::create(&owner_kp, &owner_dev.public_key(), now);
    let owner_da = Event::next(
        &owner_dev,
        owner_kp.public_key(),
        server_id.clone(),
        None,
        0,
        now,
        EventPayload::DeviceAuthorized { cert: owner_cert },
    );
    submit_raw(&owner_transport, &owner_da).await;
    let invite_event = Event::next(
        &owner_dev,
        owner_kp.public_key(),
        server_id.clone(),
        Some(&owner_da),
        owner_da.core.lamport,
        now,
        EventPayload::InviteCreated {
            code_hash: invite_code_hash(&invite_code),
            max_uses: 5,
            expires_at: now + 3600,
            requires_approval: false,
        },
    );
    submit_raw(&owner_transport, &invite_event).await;
    let mut owner_chain = ChainState::default();
    owner_chain.advance(&owner_da);
    owner_chain.advance(&invite_event);

    // ---- Joiner connects with the invite ----
    let joiner_kp = Keypair::generate();
    let joiner_dev = Keypair::generate();
    let (joiner_transport, _joiner_conn) =
        connect_identity(endpoint, addr, &joiner_kp, Some(&invite_code), None).await;

    // ---- Joiner mesh-log join (DeviceAuthorized -> ResolveInvite -> MemberJoined) ----
    let joiner_cert = DeviceCert::create(&joiner_kp, &joiner_dev.public_key(), now);
    let joiner_da = Event::next(
        &joiner_dev,
        joiner_kp.public_key(),
        server_id.clone(),
        None,
        invite_event.core.lamport,
        now,
        EventPayload::DeviceAuthorized { cert: joiner_cert },
    );
    submit_raw(&joiner_transport, &joiner_da).await;
    let invite_event_hash = match request(
        &joiner_transport,
        ServerRequest::ResolveInvite { code: invite_code.clone() },
    )
    .await
    {
        ServerResponse::InviteResolved {
            invite_event: Some(h),
        } => h,
        other => panic!("expected a resolved invite event, got {other:?}"),
    };
    assert_eq!(invite_event_hash, invite_event.hash());
    let joiner_join = Event::next(
        &joiner_dev,
        joiner_kp.public_key(),
        server_id.clone(),
        Some(&joiner_da),
        joiner_da.core.lamport,
        now,
        EventPayload::MemberJoined {
            member: joiner_kp.public_key(),
            invite: invite_event_hash,
        },
    );
    submit_raw(&joiner_transport, &joiner_join).await;
    let mut joiner_chain = ChainState::default();
    joiner_chain.advance(&joiner_da);
    joiner_chain.advance(&joiner_join);

    // ---- The vertical ----
    let key = ChannelKey::new(server_id.clone(), CHANNEL_ID).unwrap();
    let owner_data_dir = temp_dir("owner");
    let joiner_data_dir = temp_dir("joiner");

    let owner_actor = Actor {
        device: &owner_dev,
        identity: &owner_kp,
        log_server_id: &server_id,
    };
    let joiner_actor = Actor {
        device: &joiner_dev,
        identity: &joiner_kp,
        log_server_id: &server_id,
    };

    // 1. Owner creates the E2EE channel (a log event, not `CreateChannel`).
    let spec = ChannelSpec {
        key: key.clone(),
        name: "sealed-vault".to_string(),
        kind: "text".to_string(),
        parent: None,
    };
    let mut created = create_e2ee_channel(&owner_transport, &owner_actor, &mut owner_chain, &spec, &owner_data_dir)
        .await
        .unwrap();
    assert_eq!(created.channel_id, CHANNEL_ID);

    // 2. Owner bootstraps the group (confirms the creator's leaf).
    bootstrap_group(
        &owner_transport,
        &owner_actor,
        &mut owner_chain,
        &key,
        &mut created.group,
        &created.store,
        &created.store_instance_hash,
    )
    .await
    .unwrap();

    // 3. Joiner publishes its KeyPackage (create the store once, publish from it).
    let (joiner_store, joiner_hash) = create_joiner_store(&joiner_data_dir, &key).unwrap();
    publish_key_package(
        &joiner_transport,
        &joiner_actor,
        &mut joiner_chain,
        &joiner_store,
        &joiner_hash,
    )
    .await
    .unwrap();
    drop(joiner_store); // exercise the resume path below

    // 4. Owner adds the joiner (fetches the KeyPackage, commits, sends Welcome).
    let member = DeclaredMember {
        identity: joiner_kp.public_key(),
        device: device_id(&joiner_dev.public_key()),
    };
    let steward_ctx = StewardContext {
        key: &key,
        generation: 0,
        store: &created.store,
        store_instance_hash: &created.store_instance_hash,
    };
    add_member(
        &owner_transport,
        &owner_actor,
        &mut owner_chain,
        &steward_ctx,
        &mut created.group,
        &member,
    )
    .await
    .unwrap();

    // 5. Joiner resumes its store, fetches its Welcome, joins, confirms its leaf.
    let (joiner_store, joiner_hash) = resume_store(&joiner_data_dir, &key).unwrap();
    let welcomes = fetch_pending_welcomes(&joiner_transport, &joiner_actor, Some(CHANNEL_ID), 0)
        .await
        .unwrap();
    assert_eq!(welcomes.len(), 1, "the joiner should find exactly one Welcome");
    let pending = &welcomes[0];
    let (mut joiner_group, join_info) = join_channel(&joiner_store, &pending.welcome).unwrap();
    let confirmation = confirm_leaf(
        &joiner_transport,
        &joiner_actor,
        &mut joiner_chain,
        &key,
        &joiner_hash,
        pending,
        &join_info,
    )
    .await
    .unwrap();
    assert!(confirmation.can_send());

    // 6. Owner sends a sealed message; the joiner fetches the ciphertext and
    //    decrypts it, asserting the EXACT plaintext.
    let owner_seal = SealContext {
        key: &key,
        generation: 0,
        store: &created.store,
        content: owner_plaintext,
        reply_to: None,
    };
    send_sealed(
        &owner_transport,
        &owner_actor,
        &mut owner_chain,
        &owner_seal,
        &mut created.group,
        &SendEligibility::confirmed(),
    )
    .await
    .unwrap();

    let owner_ciphertext =
        fetch_ciphertext_by_author(&joiner_transport, CHANNEL_ID, &owner_kp.public_key()).await;
    match receive_sealed(&joiner_store, &mut joiner_group, owner_ciphertext.clone()) {
        SealedOutcome::Decrypted(env) => assert_eq!(env.content, owner_plaintext),
        SealedOutcome::Undecryptable { reason } => {
            panic!("joiner could not decrypt the owner's sealed message: {reason}")
        }
    }

    // 7. Joiner replies sealed; the owner fetches and decrypts that.
    let joiner_seal = SealContext {
        key: &key,
        generation: 0,
        store: &joiner_store,
        content: joiner_plaintext,
        reply_to: None,
    };
    send_sealed(
        &joiner_transport,
        &joiner_actor,
        &mut joiner_chain,
        &joiner_seal,
        &mut joiner_group,
        &confirmation.eligibility,
    )
    .await
    .unwrap();

    let joiner_ciphertext =
        fetch_ciphertext_by_author(&owner_transport, CHANNEL_ID, &joiner_kp.public_key()).await;
    match receive_sealed(&created.store, &mut created.group, joiner_ciphertext.clone()) {
        SealedOutcome::Decrypted(env) => assert_eq!(env.content, joiner_plaintext),
        SealedOutcome::Undecryptable { reason } => {
            panic!("owner could not decrypt the joiner's sealed reply: {reason}")
        }
    }

    DriveResult {
        server_id,
        channel_id: CHANNEL_ID,
        invite_code,
        owner_pk: owner_kp.public_key(),
        owner_ciphertext,
        joiner_ciphertext,
    }
}

// ---------------------------------------------------------------------------
// Test 1 — the full path (the headline)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_clients_seal_and_decrypt_end_to_end() {
    let (addr, state, endpoint, setup_hex) = spawn_server().await;
    let result =
        drive_full_path(&endpoint, addr, &state, &setup_hex, OWNER_PLAINTEXT, JOINER_PLAINTEXT)
            .await;

    // The exact-plaintext assertions happen inside `drive_full_path` (owner ->
    // joiner, then joiner -> owner). Re-assert the observable contract here so
    // the headline test reads as a whole: two ciphertexts were exchanged, and
    // neither is the plaintext it decrypts to.
    assert_eq!(result.channel_id, CHANNEL_ID);
    assert!(!result.owner_ciphertext.is_empty());
    assert!(!result.joiner_ciphertext.is_empty());
    assert_ne!(result.owner_ciphertext.as_slice(), OWNER_PLAINTEXT.as_bytes());
    assert_ne!(result.joiner_ciphertext.as_slice(), JOINER_PLAINTEXT.as_bytes());
    // Two different directions produced two different ciphertexts.
    assert_ne!(result.owner_ciphertext, result.joiner_ciphertext);
}

// ---------------------------------------------------------------------------
// Test 2 — observation: no plaintext reaches any table
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_plaintext_reaches_any_table() {
    let (addr, state, endpoint, setup_hex) = spawn_server().await;
    let _ = drive_full_path(&endpoint, addr, &state, &setup_hex, OWNER_PLAINTEXT, JOINER_PLAINTEXT)
        .await;

    // After the whole exchange, the plaintexts must appear in NO table, at the
    // byte level. The self-check that this observer actually finds a planted
    // needle lives in `crates/farder-server/tests/e2ee_observation.rs`.
    let guard = state.db.lock().unwrap();
    common::assert_no_plaintext_anywhere(&guard, OWNER_PLAINTEXT);
    common::assert_no_plaintext_anywhere(&guard, JOINER_PLAINTEXT);
}

// ---------------------------------------------------------------------------
// Test 3 — negative: a server member NOT in the MLS group cannot decrypt
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_server_member_not_in_the_mls_group_cannot_decrypt() {
    let (addr, state, endpoint, setup_hex) = spawn_server().await;
    let result =
        drive_full_path(&endpoint, addr, &state, &setup_hex, OWNER_PLAINTEXT, JOINER_PLAINTEXT)
            .await;

    // A third identity becomes a LOG member of the server (so it can fetch the
    // channel's history) but is never added to the MLS group.
    let third_kp = Keypair::generate();
    let third_dev = Keypair::generate();
    let (third_transport, _third_conn) =
        connect_identity(&endpoint, addr, &third_kp, Some(&result.invite_code), None).await;

    let now = event_now_secs();
    let third_cert = DeviceCert::create(&third_kp, &third_dev.public_key(), now);
    let third_da = Event::next(
        &third_dev,
        third_kp.public_key(),
        result.server_id.clone(),
        None,
        0,
        now,
        EventPayload::DeviceAuthorized { cert: third_cert },
    );
    submit_raw(&third_transport, &third_da).await;
    let invite_hash = match request(
        &third_transport,
        ServerRequest::ResolveInvite {
            code: result.invite_code.clone(),
        },
    )
    .await
    {
        ServerResponse::InviteResolved {
            invite_event: Some(h),
        } => h,
        other => panic!("expected a resolved invite event, got {other:?}"),
    };
    let third_join = Event::next(
        &third_dev,
        third_kp.public_key(),
        result.server_id.clone(),
        Some(&third_da),
        third_da.core.lamport,
        now,
        EventPayload::MemberJoined {
            member: third_kp.public_key(),
            invite: invite_hash,
        },
    );
    submit_raw(&third_transport, &third_join).await;

    // The third identity CAN fetch the ciphertext (it is a server member with
    // READ_MESSAGES) — this is exactly what must still fail to decrypt.
    let ciphertext =
        fetch_ciphertext_by_author(&third_transport, result.channel_id, &result.owner_pk).await;
    assert!(!ciphertext.is_empty(), "the third identity must be able to fetch the ciphertext");

    // Give the third identity its OWN, unrelated MLS group (it was never added
    // to the owner's). Decryption must fail closed.
    let third_dir = temp_dir("third");
    std::fs::create_dir_all(&third_dir).unwrap();
    let third_store_path = third_dir.join("solo.sqlite");
    let (third_store, _) = FarderMlsStore::create(&third_store_path).unwrap();
    let mut third_group = MlsChannelGroup::create(
        &third_store,
        &DeviceSigner(&third_dev),
        credential_with_key(&third_dev, &third_kp.public_key()),
        b"third-identity-unrelated-group",
    )
    .unwrap();

    match receive_sealed(&third_store, &mut third_group, ciphertext) {
        SealedOutcome::Undecryptable { reason } => assert!(!reason.is_empty()),
        SealedOutcome::Decrypted(_) => {
            panic!("a server member not in the MLS group decrypted the ciphertext")
        }
    }
}
