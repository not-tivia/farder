//! Task H1: the lifecycle harness (sub-project 5a).
//!
//! Drives the `farder-e2ee-client` lifecycle primitives — rekey, drift
//! discharge, revocation, group reset, multi-device self-add, store
//! re-provisioning — over a REAL in-process QUIC server (the same shape as
//! `e2ee_two_client.rs`), not the `FakeTransport` the crate's unit tests use.
//! Each of the five spec tests is an OBSERVATION: it drives the real fold on
//! the wire and asserts the observable consequence (a send gate refusal, an
//! un-decryptable ciphertext, a re-join confirmation), not a re-stated code
//! fact.
//!
//! Where a spec test is impractical to trigger exactly (the 500-event
//! freshness ceiling is deterministic but expensive; the "ghost-Welcome" only
//! materializes as drift once its never-confirming holder leaves good
//! standing), the closest faithful analogue is driven and flagged as such in
//! the test's doc comment.

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
    add_member, add_own_device, bootstrap_group, build_next_event, confirm_leaf,
    create_e2ee_channel, create_joiner_store, discharge_drift, event_now_secs,
    fetch_pending_welcomes, join_channel, publish_key_package, receive_sealed, rekey_channel,
    reprovision_device, reset_group, resume_store, send_sealed, Actor, ChainState,
    ChannelKey, ChannelSpec, DriftDischargeContext, E2eeTransport, EventAccepted, MlsControl,
    OwnDeviceContext, RekeyContext, ReprovisionContext, ReprovisionLive, ResetContext,
    SealContext, SealedOutcome, SendEligibility, StewardContext, TransportError, Welcomes,
};
use farder_mls::group::{DeclaredMember, MlsChannelGroup};
use farder_mls::store::FarderMlsStore;
use farder_protocol::server::{
    ClientFrame, MessageInfoV2, ServerFrame, ServerRequest, ServerResponse, SERVER_PROTOCOL_VERSION,
};
use farder_protocol::codec;
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use rustls::pki_types::ServerName;

/// Distinctive plaintexts, each asserted (in the observation test) to appear
/// in NO database table at the byte level.
const OWNER_PLAINTEXT: &str = "FARDER-CANARY-lifecycle-owner-sealed-1c8f3a";
const JOINER_PLAINTEXT: &str = "FARDER-CANARY-lifecycle-joiner-sealed-2e7d9b";

static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_dir(name: &str) -> PathBuf {
    let n = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "farder-e2ee-lifecycle-{name}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

// ---------------------------------------------------------------------------
// TLS + QUIC + transport (the same shape as `e2ee_two_client.rs`)
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
    let (mut send, mut recv) = conn.accept_bi().await.expect("accept_bi");

    let frame = recv_server_frame(&mut recv).await;
    let nonce = match frame {
        ServerFrame::Challenge { nonce } => nonce,
        other => panic!("expected Challenge, got {other:?}"),
    };

    let signature = keypair.sign(&nonce);
    let auth = ClientFrame::Authenticate {
        public_key: keypair.public_key(),
        signed_challenge: signature,
        invite_code: invite_code.map(|s| s.to_string()),
        setup_token: setup_token.map(|s| s.to_string()),
    };
    send_frame(&mut send, &auth).await;

    let frame = recv_server_frame(&mut recv).await;
    match frame {
        ServerFrame::Authenticated { .. } => {}
        ServerFrame::AuthError { reason } => panic!("auth error: {reason}"),
        other => panic!("expected Authenticated, got {other:?}"),
    }

    let transport = QuicE2eeTransport::new(send, recv);

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

async fn request(t: &QuicE2eeTransport, body: ServerRequest) -> ServerResponse {
    t.round_trip(body).await.expect("round-trip")
}

async fn submit_raw(t: &QuicE2eeTransport, event: &Event) {
    match request(t, ServerRequest::SubmitEvent { event: event.clone() }).await {
        ServerResponse::EventAccepted { .. } => {}
        other => panic!("expected EventAccepted, got {other:?}"),
    }
}

async fn spawn_server() -> (SocketAddr, Arc<farder_server::state::ServerState>, String) {
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

    let tmp_dir = std::env::temp_dir().join(format!("farder-e2ee-lifecycle-{}", std::process::id()));
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
    (actual_addr, state, setup_hex)
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

fn actor<'a>(identity: &'a Keypair, device: &'a Keypair, server_id: &'a str) -> Actor<'a> {
    Actor {
        device,
        identity,
        log_server_id: server_id,
    }
}

// ---------------------------------------------------------------------------
// Setup: an owner, alone (test 4) or owner + joiner (tests 1/2/3/5)
// ---------------------------------------------------------------------------

/// The owner's fully materialized channel state after mesh-join + create +
/// bootstrap (epoch 1, owner's leaf confirmed). Test 4 builds on this.
struct OwnerCore {
    state: Arc<farder_server::state::ServerState>,
    addr: SocketAddr,
    _endpoint: Endpoint,
    owner_kp: Keypair,
    owner_dev: Keypair,
    server_id: String,
    invite_code: String,
    owner_transport: QuicE2eeTransport,
    _owner_conn: Connection,
    owner_chain: ChainState,
    key: ChannelKey,
    owner_store: FarderMlsStore,
    owner_hash: [u8; 32],
    owner_group: MlsChannelGroup,
}

async fn setup_owner_core(channel_id: u64) -> OwnerCore {
    let (addr, state, setup_hex) = spawn_server().await;
    let endpoint = make_client_endpoint();

    let owner_kp = Keypair::generate();
    let owner_dev = Keypair::generate();
    let (owner_transport, owner_conn) =
        connect_identity(&endpoint, addr, &owner_kp, None, Some(&setup_hex)).await;

    // Owner mesh-log join.
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

    // The vertical: create + bootstrap.
    let key = ChannelKey::new(server_id.clone(), channel_id).unwrap();
    let owner_dir = temp_dir("owner");
    let owner_actor = actor(&owner_kp, &owner_dev, &server_id);
    let spec = ChannelSpec {
        key: key.clone(),
        name: "lifecycle-vault".to_string(),
        kind: "text".to_string(),
        parent: None,
    };
    let mut created = create_e2ee_channel(
        &owner_transport,
        &owner_actor,
        &mut owner_chain,
        &spec,
        &owner_dir,
    )
    .await
    .unwrap();
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

    OwnerCore {
        state,
        addr,
        _endpoint: endpoint,
        owner_kp,
        owner_dev,
        server_id,
        invite_code,
        owner_transport,
        _owner_conn: owner_conn,
        owner_chain,
        key,
        owner_store: created.store,
        owner_hash: created.store_instance_hash,
        owner_group: created.group,
    }
}

/// A two-client channel: owner + joiner, both leaves confirmed (unless
/// `confirm_joiner` is false, in which case the joiner's leaf is left pending —
/// the "ghost-Welcome" state test 2 needs).
struct TwoClientSetup {
    core: OwnerCore,
    joiner_kp: Keypair,
    joiner_dev: Keypair,
    joiner_transport: QuicE2eeTransport,
    _joiner_conn: Connection,
    joiner_store: FarderMlsStore,
    joiner_group: MlsChannelGroup,
}

async fn setup_owner_joiner(channel_id: u64, confirm_joiner: bool) -> TwoClientSetup {
    let mut core = setup_owner_core(channel_id).await;

    // Joiner mesh-log join (a second identity over its own connection).
    let joiner_kp = Keypair::generate();
    let joiner_dev = Keypair::generate();
    let (joiner_transport, joiner_conn) = connect_identity(
        &core._endpoint,
        core.addr,
        &joiner_kp,
        Some(&core.invite_code),
        None,
    )
    .await;

    let now = event_now_secs();
    let joiner_cert = DeviceCert::create(&joiner_kp, &joiner_dev.public_key(), now);
    let joiner_da = Event::next(
        &joiner_dev,
        joiner_kp.public_key(),
        core.server_id.clone(),
        None,
        core.owner_chain.lamport,
        now,
        EventPayload::DeviceAuthorized { cert: joiner_cert },
    );
    submit_raw(&joiner_transport, &joiner_da).await;
    let invite_event_hash = match request(
        &joiner_transport,
        ServerRequest::ResolveInvite {
            code: core.invite_code.clone(),
        },
    )
    .await
    {
        ServerResponse::InviteResolved {
            invite_event: Some(h),
        } => h,
        other => panic!("expected a resolved invite event, got {other:?}"),
    };
    let joiner_join = Event::next(
        &joiner_dev,
        joiner_kp.public_key(),
        core.server_id.clone(),
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

    let joiner_dir = temp_dir("joiner");
    let owner_actor = actor(&core.owner_kp, &core.owner_dev, &core.server_id);
    let joiner_actor = actor(&joiner_kp, &joiner_dev, &core.server_id);

    // Joiner publishes its KeyPackage.
    let (joiner_store, joiner_hash) = create_joiner_store(&joiner_dir, &core.key).unwrap();
    publish_key_package(
        &joiner_transport,
        &joiner_actor,
        &mut joiner_chain,
        &joiner_store,
        &joiner_hash,
    )
    .await
    .unwrap();

    // Owner adds the joiner.
    let member = DeclaredMember {
        identity: joiner_kp.public_key(),
        device: device_id(&joiner_dev.public_key()),
    };
    let steward_ctx = StewardContext {
        key: &core.key,
        generation: 0,
        store: &core.owner_store,
        store_instance_hash: &core.owner_hash,
    };
    add_member(
        &core.owner_transport,
        &owner_actor,
        &mut core.owner_chain,
        &steward_ctx,
        &mut core.owner_group,
        &member,
    )
    .await
    .unwrap();

    // Joiner joins + confirms (unless the caller wants the pending ghost leaf).
    let joiner_group = if confirm_joiner {
        let welcomes =
            fetch_pending_welcomes(&joiner_transport, &joiner_actor, Some(channel_id), 0)
                .await
                .unwrap();
        assert_eq!(welcomes.len(), 1, "the joiner should find exactly one Welcome");
        let pending = &welcomes[0];
        let (group, info) = join_channel(&joiner_store, &pending.welcome).unwrap();
        confirm_leaf(
            &joiner_transport,
            &joiner_actor,
            &mut joiner_chain,
            &core.key,
            &joiner_hash,
            pending,
            &info,
        )
        .await
        .unwrap();
        group
    } else {
        // No join: leave the joiner's leaf pending. Build an empty placeholder
        // group so the struct's field is inhabited (never used by test 2).
        MlsChannelGroup::create(
            &joiner_store,
            &farder_mls::credential::DeviceSigner(&joiner_dev),
            farder_mls::credential::credential_with_key(&joiner_dev, &joiner_kp.public_key()),
            farder_e2ee_client::channel_group_id(&core.server_id, channel_id, 999).as_bytes(),
        )
        .unwrap()
    };

    TwoClientSetup {
        core,
        joiner_kp,
        joiner_dev,
        joiner_transport,
        _joiner_conn: joiner_conn,
        joiner_store,
        joiner_group,
    }
}

// ---------------------------------------------------------------------------
// Test 1 — ban -> send gate -> rekey -> forward secrecy
// ---------------------------------------------------------------------------

/// SPEC TEST 1 (exact): a banned member's confirmed leaf becomes drift
/// (`pending_removals` seals the channel), a sealed send is refused, the drift
/// is discharged and the group rekeyed, and a PRE-rekey snapshot of the channel
/// state (the banned joiner's group at epoch 2) can no longer decrypt the
/// post-rekey traffic (forward secrecy). The plaintext appears in NO table.
#[tokio::test]
async fn ban_send_gate_rekey_forward_secrecy() {
    let channel_id = E2EE_CHANNEL_ID_FLOOR + 50_101;
    let s = setup_owner_joiner(channel_id, true).await;
    let TwoClientSetup {
        core:
            OwnerCore {
                state,
                owner_kp,
                owner_dev,
                server_id,
                owner_transport,
                mut owner_chain,
                key,
                owner_store,
                owner_hash,
                mut owner_group,
                ..
            },
        joiner_kp,
        joiner_dev,
        joiner_transport,
        joiner_store,
        mut joiner_group,
        ..
    } = s;

    let owner_actor = actor(&owner_kp, &owner_dev, &server_id);

    // Positive control: before the ban the joiner (epoch 2) CAN decrypt the
    // owner's traffic — so the later failure is forward secrecy, not a
    // never-working joiner.
    let pre_plain = OWNER_PLAINTEXT;
    let pre_ctx = SealContext {
        key: &key,
        generation: 0,
        store: &owner_store,
        content: pre_plain,
        reply_to: None,
    };
    send_sealed(
        &owner_transport,
        &owner_actor,
        &mut owner_chain,
        &pre_ctx,
        &mut owner_group,
        &SendEligibility::confirmed(),
    )
    .await
    .unwrap();
    let pre_ct =
        fetch_ciphertext_by_author(&joiner_transport, channel_id, &owner_kp.public_key()).await;
    match receive_sealed(&joiner_store, &mut joiner_group, pre_ct) {
        SealedOutcome::Decrypted(env) => assert_eq!(env.content, pre_plain),
        SealedOutcome::Undecryptable { reason } => {
            panic!("joiner failed the pre-ban positive control: {reason}")
        }
    }

    // Ban the joiner (owner holds the 'ban' capability).
    let ban = build_next_event(
        &owner_dev,
        &owner_kp,
        &server_id,
        &owner_chain,
        event_now_secs(),
        EventPayload::MemberBanned {
            member: joiner_kp.public_key(),
        },
    );
    match owner_transport.submit_event(&ban).await {
        Ok(_) => owner_chain.advance(&ban),
        Err(e) => panic!("ban rejected: {e}"),
    }

    // The send gate engages: the channel is sealed until the drift discharges.
    let blocked_ctx = SealContext {
        key: &key,
        generation: 0,
        store: &owner_store,
        content: "this must not go out",
        reply_to: None,
    };
    let err = send_sealed(
        &owner_transport,
        &owner_actor,
        &mut owner_chain,
        &blocked_ctx,
        &mut owner_group,
        &SendEligibility::confirmed(),
    )
    .await
    .unwrap_err();
    assert!(
        err.is_sealed_pending_removals(),
        "expected the pending-removals send gate, got {err}"
    );

    // Discharge the drift: remove the banned joiner's confirmed leaf.
    let joiner_member = DeclaredMember {
        identity: joiner_kp.public_key(),
        device: device_id(&joiner_dev.public_key()),
    };
    let dctx = DriftDischargeContext {
        key: &key,
        generation: 0,
        store: &owner_store,
        store_instance_hash: &owner_hash,
    };
    discharge_drift(
        &owner_transport,
        &owner_actor,
        &mut owner_chain,
        &dctx,
        &mut owner_group,
        &[joiner_member],
    )
    .await
    .unwrap();

    // Rekey (self_update): rotate the group's secrets past the removed leaf.
    let rctx = RekeyContext {
        key: &key,
        generation: 0,
        store: &owner_store,
        store_instance_hash: &owner_hash,
    };
    rekey_channel(
        &owner_transport,
        &owner_actor,
        &mut owner_chain,
        &rctx,
        &mut owner_group,
    )
    .await
    .unwrap();

    // Post-rekey traffic.
    let post_plain = JOINER_PLAINTEXT;
    let post_ctx = SealContext {
        key: &key,
        generation: 0,
        store: &owner_store,
        content: post_plain,
        reply_to: None,
    };
    send_sealed(
        &owner_transport,
        &owner_actor,
        &mut owner_chain,
        &post_ctx,
        &mut owner_group,
        &SendEligibility::confirmed(),
    )
    .await
    .unwrap();
    let post_ct =
        fetch_ciphertext_by_author(&owner_transport, channel_id, &owner_kp.public_key()).await;

    // Forward secrecy: the joiner's PRE-rekey group (epoch 2) cannot decrypt
    // the POST-rekey ciphertext (epoch 4).
    match receive_sealed(&joiner_store, &mut joiner_group, post_ct) {
        SealedOutcome::Undecryptable { reason } => assert!(!reason.is_empty()),
        SealedOutcome::Decrypted(_) => {
            panic!("a banned member's pre-rekey state decrypted post-rekey traffic")
        }
    }

    // Observation: the two plaintexts reached NO table.
    let guard = state.db.lock().unwrap();
    common::assert_no_plaintext_anywhere(&guard, pre_plain);
    common::assert_no_plaintext_anywhere(&guard, post_plain);
}

// ---------------------------------------------------------------------------
// Test 2 — ghost-Welcome drift self-heals
// ---------------------------------------------------------------------------

/// SPEC TEST 2 (closest faithful analogue): a Welcome is staged for a member
/// who never confirms, leaving a PENDING leaf. The pending leaf only becomes
/// *visible* drift once its holder leaves good standing — here, the owner bans
/// the never-confirming joiner, which moves the pending leaf into
/// `pending_removals` and seals the channel. The drift-discharge path drops the
/// unproven leaf and the channel un-seals (a sealed send succeeds again). This
/// exercises `discharge_drift` on a PENDING leaf, the "ghost-Welcome self-heal"
/// path (distinct from test 1, which removes a CONFIRMED leaf).
#[tokio::test]
async fn ghost_welcome_drift_self_heals() {
    let channel_id = E2EE_CHANNEL_ID_FLOOR + 50_102;
    let s = setup_owner_joiner(channel_id, false).await; // joiner never confirms
    let TwoClientSetup {
        core:
            OwnerCore {
                owner_kp,
                owner_dev,
                server_id,
                owner_transport,
                mut owner_chain,
                key,
                owner_store,
                owner_hash,
                mut owner_group,
                ..
            },
        joiner_kp,
        joiner_dev,
        ..
    } = s;

    let owner_actor = actor(&owner_kp, &owner_dev, &server_id);

    // The joiner's leaf is pending, not confirmed — and while it is a member in
    // good standing it produces no drift. Ban it: the ghost leaf becomes drift.
    let ban = build_next_event(
        &owner_dev,
        &owner_kp,
        &server_id,
        &owner_chain,
        event_now_secs(),
        EventPayload::MemberBanned {
            member: joiner_kp.public_key(),
        },
    );
    match owner_transport.submit_event(&ban).await {
        Ok(_) => owner_chain.advance(&ban),
        Err(e) => panic!("ban rejected: {e}"),
    }

    // Visible drift: the channel is sealed.
    let blocked_ctx = SealContext {
        key: &key,
        generation: 0,
        store: &owner_store,
        content: "blocked by the ghost leaf",
        reply_to: None,
    };
    let err = send_sealed(
        &owner_transport,
        &owner_actor,
        &mut owner_chain,
        &blocked_ctx,
        &mut owner_group,
        &SendEligibility::confirmed(),
    )
    .await
    .unwrap_err();
    assert!(
        err.is_sealed_pending_removals(),
        "expected the pending-removals send gate, got {err}"
    );

    // Drift-discharge drops the unproven (pending) leaf and un-seals.
    let ghost_member = DeclaredMember {
        identity: joiner_kp.public_key(),
        device: device_id(&joiner_dev.public_key()),
    };
    let dctx = DriftDischargeContext {
        key: &key,
        generation: 0,
        store: &owner_store,
        store_instance_hash: &owner_hash,
    };
    discharge_drift(
        &owner_transport,
        &owner_actor,
        &mut owner_chain,
        &dctx,
        &mut owner_group,
        &[ghost_member],
    )
    .await
    .unwrap();

    // Un-sealed: a sealed send succeeds again.
    let ok_ctx = SealContext {
        key: &key,
        generation: 0,
        store: &owner_store,
        content: "the channel self-healed",
        reply_to: None,
    };
    send_sealed(
        &owner_transport,
        &owner_actor,
        &mut owner_chain,
        &ok_ctx,
        &mut owner_group,
        &SendEligibility::confirmed(),
    )
    .await
    .unwrap();
}

// ---------------------------------------------------------------------------
// Test 3 — stale channel blocks sends, then unblocks after rekey
// ---------------------------------------------------------------------------

/// SPEC TEST 3 (exact — the 500-event freshness ceiling IS deterministic):
/// 500 sealed posts exhaust the freshness budget, the 501st send is refused
/// with the `"freshness ceiling reached"` gate, a rekey (self_update) — which
/// the ceiling override permits regardless of the commit-rate rule — refreshes
/// the budget, and sends succeed again.
#[tokio::test]
async fn stale_channel_blocks_then_unblocks_after_rekey() {
    let channel_id = E2EE_CHANNEL_ID_FLOOR + 50_103;
    let s = setup_owner_joiner(channel_id, true).await;
    let TwoClientSetup {
        core:
            OwnerCore {
                owner_kp,
                owner_dev,
                server_id,
                owner_transport,
                mut owner_chain,
                key,
                owner_store,
                owner_hash,
                mut owner_group,
                ..
            },
        ..
    } = s;

    let owner_actor = actor(&owner_kp, &owner_dev, &server_id);

    // Spend the whole freshness budget at the current epoch (500 sealed posts).
    const CEILING: u32 = 500;
    for i in 0..CEILING {
        let fill_ctx = SealContext {
            key: &key,
            generation: 0,
            store: &owner_store,
            content: "ceil-fill",
            reply_to: None,
        };
        send_sealed(
            &owner_transport,
            &owner_actor,
            &mut owner_chain,
            &fill_ctx,
            &mut owner_group,
            &SendEligibility::confirmed(),
        )
        .await
        .unwrap_or_else(|e| panic!("sealed post {i} should fold, got {e}"));
    }

    // The next send hits the freshness ceiling.
    let over_ctx = SealContext {
        key: &key,
        generation: 0,
        store: &owner_store,
        content: "over the ceiling",
        reply_to: None,
    };
    let err = send_sealed(
        &owner_transport,
        &owner_actor,
        &mut owner_chain,
        &over_ctx,
        &mut owner_group,
        &SendEligibility::confirmed(),
    )
    .await
    .unwrap_err();
    assert!(
        err.is_freshness_ceiling_reached(),
        "expected the freshness-ceiling gate, got {err}"
    );

    // Rekey: the ceiling override makes this permitted despite the rate rule.
    let rctx = RekeyContext {
        key: &key,
        generation: 0,
        store: &owner_store,
        store_instance_hash: &owner_hash,
    };
    rekey_channel(
        &owner_transport,
        &owner_actor,
        &mut owner_chain,
        &rctx,
        &mut owner_group,
    )
    .await
    .unwrap();

    // Unblocked: a fresh send succeeds at the new epoch.
    let fresh_ctx = SealContext {
        key: &key,
        generation: 0,
        store: &owner_store,
        content: "sends resume after the rekey",
        reply_to: None,
    };
    send_sealed(
        &owner_transport,
        &owner_actor,
        &mut owner_chain,
        &fresh_ctx,
        &mut owner_group,
        &SendEligibility::confirmed(),
    )
    .await
    .unwrap();
}

// ---------------------------------------------------------------------------
// Test 4 — device-loss rejoin via store re-provisioning
// ---------------------------------------------------------------------------

/// SPEC TEST 4 (exact — the multi-device case C7 names for H1): the owner's
/// SECOND device O2 confirms a leaf, then loses its on-disk store (resume is
/// terminal). The recovery path `reprovision_device` self-revokes the old O2
/// device, mints a FRESH device O2', and the still-healthy O1 steward self-adds
/// it; O2' then joins and confirms — the identity has rejoined. The old leaf's
/// drift is left to a later discharge (reported, not papered over).
#[tokio::test]
async fn device_loss_rejoin_via_reprovision() {
    let channel_id = E2EE_CHANNEL_ID_FLOOR + 50_104;
    let core = setup_owner_core(channel_id).await;
    let OwnerCore {
        owner_kp,
        owner_dev,
        server_id,
        owner_transport,
        mut owner_chain,
        key,
        owner_store,
        owner_hash,
        mut owner_group,
        ..
    } = core;

    let owner_actor = actor(&owner_kp, &owner_dev, &server_id);

    // 1. Owner self-adds a second device O2.
    let o2_dev = Keypair::generate();
    let o2_dir = temp_dir("o2");
    let (o2_store, o2_hash) = create_joiner_store(&o2_dir, &key).unwrap();
    let steward_ctx = StewardContext {
        key: &key,
        generation: 0,
        store: &owner_store,
        store_instance_hash: &owner_hash,
    };
    let o2_ctx = OwnDeviceContext {
        identity: &owner_kp,
        new_device: &o2_dev,
        new_store: &o2_store,
        new_store_instance_hash: &o2_hash,
        steward: &steward_ctx,
    };
    let mut o2_chain = ChainState::default();
    add_own_device(
        &owner_transport,
        &o2_ctx,
        &mut o2_chain,
        &owner_actor,
        &mut owner_chain,
        &mut owner_group,
    )
    .await
    .unwrap();

    // 2. O2 joins and confirms its leaf.
    let o2_actor = actor(&owner_kp, &o2_dev, &server_id);
    let welcomes = fetch_pending_welcomes(&owner_transport, &o2_actor, Some(channel_id), 0)
        .await
        .unwrap();
    assert_eq!(welcomes.len(), 1, "O2 should find exactly one Welcome");
    let pending = &welcomes[0];
    let (o2_group, o2_join) = join_channel(&o2_store, &pending.welcome).unwrap();
    let confirmation = confirm_leaf(
        &owner_transport,
        &o2_actor,
        &mut o2_chain,
        &key,
        &o2_hash,
        pending,
        &o2_join,
    )
    .await
    .unwrap();
    assert!(confirmation.can_send(), "O2 confirmed its leaf");

    // 3. O2 loses its store: the file is gone, resume is terminal.
    drop(o2_group);
    drop(o2_store);
    std::fs::remove_file(key.mls_store_path(&o2_dir).unwrap()).unwrap();
    match resume_store(&o2_dir, &key) {
        Ok(_) => panic!("resume of a lost store must be terminal"),
        Err(farder_e2ee_client::E2eeError::StoreResumeTerminal(_)) => {}
        Err(other) => panic!("expected StoreResumeTerminal, got {other}"),
    }

    // 4. Reprovision: O2 (key still in hand) self-revokes, and the healthy O1
    //    steward self-adds a FRESH device O2'.
    let o2_fresh_dev = Keypair::generate();
    let o2_fresh_dir = temp_dir("o2-fresh");
    let (o2_fresh_store, o2_fresh_hash) = create_joiner_store(&o2_fresh_dir, &key).unwrap();
    let fresh_steward_ctx = StewardContext {
        key: &key,
        generation: 0,
        store: &owner_store,
        store_instance_hash: &owner_hash,
    };
    let repro_ctx = ReprovisionContext {
        old_device: &o2_dev,
        fresh: OwnDeviceContext {
            identity: &owner_kp,
            new_device: &o2_fresh_dev,
            new_store: &o2_fresh_store,
            new_store_instance_hash: &o2_fresh_hash,
            steward: &fresh_steward_ctx,
        },
    };
    let mut o2_old_chain = o2_chain;
    let mut o2_fresh_chain = ChainState::default();
    let _repro = reprovision_device(
        &owner_transport,
        &repro_ctx,
        ReprovisionLive {
            old_chain: &mut o2_old_chain,
            new_chain: &mut o2_fresh_chain,
            steward: owner_actor,
            steward_chain: &mut owner_chain,
            group: &mut owner_group,
        },
    )
    .await
    .unwrap();

    // 5. O2' joins and confirms: the identity has rejoined.
    let o2_fresh_actor = actor(&owner_kp, &o2_fresh_dev, &server_id);
    let fresh_welcomes =
        fetch_pending_welcomes(&owner_transport, &o2_fresh_actor, Some(channel_id), 0)
            .await
            .unwrap();
    assert_eq!(fresh_welcomes.len(), 1, "O2' should find exactly one Welcome");
    let fresh_pending = &fresh_welcomes[0];
    let (_fresh_group, fresh_join) = join_channel(&o2_fresh_store, &fresh_pending.welcome).unwrap();
    let confirmation2 = confirm_leaf(
        &owner_transport,
        &o2_fresh_actor,
        &mut o2_fresh_chain,
        &key,
        &o2_fresh_hash,
        fresh_pending,
        &fresh_join,
    )
    .await
    .unwrap();
    assert!(confirmation2.can_send(), "the fresh device rejoined the channel");
}

// ---------------------------------------------------------------------------
// Test 5 — partial reset is refused with the exact-cover error
// ---------------------------------------------------------------------------

/// SPEC TEST 5 (exact): `reset_group` with a member MISSING from the reset's
/// member set (empty, in a channel that still owes the joiner a leaf) is
/// rejected by the fold with the `"non-selective reset"` exact-cover error.
#[tokio::test]
async fn partial_reset_is_refused_with_the_exact_cover_error() {
    let channel_id = E2EE_CHANNEL_ID_FLOOR + 50_105;
    let s = setup_owner_joiner(channel_id, true).await;
    let TwoClientSetup {
        core:
            OwnerCore {
                owner_kp,
                owner_dev,
                server_id,
                owner_transport,
                mut owner_chain,
                key,
                owner_store,
                owner_hash,
                ..
            },
        ..
    } = s;

    let owner_actor = actor(&owner_kp, &owner_dev, &server_id);
    let ctx = ResetContext {
        key: &key,
        store: &owner_store,
        store_instance_hash: &owner_hash,
    };

    // Reset with NO members: the fold's member_leaf_set still owes the joiner a
    // leaf, so the empty Welcome cover is a non-selective (partial) reset.
    let err = reset_group(&owner_transport, &owner_actor, &mut owner_chain, &ctx, 0, &[])
        .await
        .unwrap_err();

    match err {
        farder_e2ee_client::E2eeError::Transport(t) => {
            assert!(
                t.rejection_reason().contains("non-selective reset"),
                "expected the exact-cover error, got: {}",
                t.rejection_reason()
            );
        }
        other => panic!("expected a Transport rejection, got {other}"),
    }
}
