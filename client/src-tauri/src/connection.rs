use anyhow::{bail, Context, Result};
use farder_crypto::identity::Keypair;
use farder_protocol::{
    codec,
    messages::Message,
    server::{ClientFrame, RelayStreamRole, ServerFrame},
};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use std::net::SocketAddr;

const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024; // 16 MB

/// Connection info for a relayed server, parsed from the escape-hatch link
/// `farder://relay/<relay_addr>/<server_id_hex>/<cert_fp_hex>/<invite_token>`.
#[derive(Clone, Debug)]
pub struct RelayTarget {
    pub relay_addr: SocketAddr,
    pub server_id: Vec<u8>,
    pub cert_fp: Vec<u8>,
    pub invite_token: String,
}

/// Parse a relay link, or `None` if `s` is not a well-formed relay link (e.g. a
/// direct `farder://addr/code` link or anything else).
pub fn parse_relay_target(s: &str) -> Option<RelayTarget> {
    // Compact default-relay form: farder://relayd/<server_id_hex>/<token>
    // (token may be empty). Expanded from the compiled-in default relay; a
    // build with no default relay cannot resolve these (returns None).
    if let Some(rest) = s.strip_prefix("farder://relayd/") {
        let (relay_addr, cert_fp) = crate::default_relay::default_relay()?;
        let mut parts = rest.splitn(2, '/');
        let server_id = hex::decode(parts.next()?).ok()?;
        if server_id.is_empty() {
            return None;
        }
        let invite_token = parts.next().unwrap_or("").to_string();
        return Some(RelayTarget { relay_addr, server_id, cert_fp, invite_token });
    }

    let rest = s.strip_prefix("farder://relay/")?;
    let parts: Vec<&str> = rest.splitn(4, '/').collect();
    if parts.len() != 4 {
        return None;
    }
    let relay_addr: SocketAddr = parts[0].parse().ok()?;
    let server_id = hex::decode(parts[1]).ok()?;
    let cert_fp = hex::decode(parts[2]).ok()?;
    if server_id.is_empty() || cert_fp.is_empty() {
        return None;
    }
    Some(RelayTarget {
        relay_addr,
        server_id,
        cert_fp,
        invite_token: parts[3].to_string(),
    })
}

/// Build a relay deep link from a target and a (new) invite code:
/// `farder://relay/<relay_addr>/<server_id_hex>/<cert_fp_hex>/<code>`.
pub fn build_relay_link(target: &RelayTarget, code: &str) -> String {
    format!(
        "farder://relay/{}/{}/{}/{}",
        target.relay_addr,
        hex::encode(&target.server_id),
        hex::encode(&target.cert_fp),
        code
    )
}

/// Build the COMPACT deep link for a server on the compiled-in default relay:
/// `farder://relayd/<server_id_hex>/<token>`. Only valid when the server's
/// relay is the default (the parser re-expands from default_relay()).
pub fn build_compact_relay_link(server_id: &[u8], code: &str) -> String {
    format!("farder://relayd/{}/{}", hex::encode(server_id), code)
}

/// Read a length-prefixed frame (4-byte big-endian length header).
pub async fn read_frame(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .context("failed to read frame length")?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_SIZE {
        bail!("frame too large: {} bytes", len);
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf)
        .await
        .context("failed to read frame body")?;
    Ok(buf)
}

/// Write a length-prefixed frame (4-byte big-endian length header).
pub async fn write_frame(send: &mut SendStream, data: &[u8]) -> Result<()> {
    let len = (data.len() as u32).to_be_bytes();
    send.write_all(&len)
        .await
        .context("failed to write frame length")?;
    send.write_all(data)
        .await
        .context("failed to write frame body")?;
    Ok(())
}

/// Encode and send a `ClientFrame` over the stream.
pub async fn send_client_frame(send: &mut SendStream, frame: &ClientFrame) -> Result<()> {
    let encoded = codec::encode(frame).context("failed to encode ClientFrame")?;
    write_frame(send, &encoded).await
}

/// Read and decode a `ServerFrame` from the stream.
pub async fn recv_server_frame(recv: &mut RecvStream) -> Result<ServerFrame> {
    let buf = read_frame(recv).await?;
    let frame: ServerFrame = codec::decode(&buf).context("failed to decode ServerFrame")?;
    Ok(frame)
}

/// Connect to a Farder server, perform the challenge-response authentication,
/// and return the active connection, send/recv streams, and session token.
///
/// Auth flow:
///   1. Client connects via QUIC.
///   2. Server opens the main bi-stream; client accepts it.
///   3. Server sends `Challenge { nonce }`.
///   4. Client signs the nonce and sends `Authenticate { ... }`.
///   5. Server responds with `Authenticated { session_token }` or `AuthError`.
pub async fn connect_and_authenticate(
    endpoint: Endpoint,
    address: SocketAddr,
    keypair: &Keypair,
    invite_code: Option<String>,
    setup_token: Option<String>,
) -> Result<(Connection, SendStream, RecvStream, Vec<u8>)> {
    // Step 1: establish QUIC connection
    let conn = endpoint
        .connect(address, "farder-server")
        .context("failed to initiate QUIC connection")?
        .await
        .context("QUIC handshake failed")?;

    // Step 2: server opens the main bi-directional stream
    let (mut send, mut recv) = conn
        .accept_bi()
        .await
        .context("failed to accept bi-stream from server")?;

    // Steps 3-5: run the shared challenge-response handshake.
    let session_token =
        run_client_handshake(&mut send, &mut recv, keypair, invite_code, setup_token).await?;
    Ok((conn, send, recv, session_token))
}

/// Run the challenge-response auth handshake over an established stream pair.
/// Shared by the direct and relay connect paths. Returns the session token.
pub async fn run_client_handshake(
    send: &mut SendStream,
    recv: &mut RecvStream,
    keypair: &Keypair,
    invite_code: Option<String>,
    setup_token: Option<String>,
) -> Result<Vec<u8>> {
    let nonce = match recv_server_frame(recv).await? {
        ServerFrame::Challenge { nonce } => nonce,
        other => bail!("expected Challenge, got {:?}", other),
    };
    let signed_challenge = keypair.sign(&nonce);
    let public_key = keypair.public_key();
    let auth_frame = ClientFrame::Authenticate {
        public_key,
        signed_challenge,
        invite_code,
        setup_token,
    };
    send_client_frame(send, &auth_frame)
        .await
        .context("failed to send Authenticate frame")?;
    match recv_server_frame(recv).await? {
        ServerFrame::Authenticated { session_token } => Ok(session_token),
        ServerFrame::AuthError { reason } => bail!("authentication failed: {}", reason),
        other => bail!("unexpected frame after auth: {:?}", other),
    }
}

/// Connect to a relayed server THROUGH its relay. The relay bridges the client's
/// streams to the server, so the server only ever sees the relay's address
/// (closes Gap #3). The caller must pass an endpoint that pins the relay's cert
/// (see `tls::make_pinned_relay_endpoint`).
///
/// Authentication uses `target.invite_token` (extracted from the relay link)
/// as the invite code — no separate invite code parameter is needed.
pub async fn connect_via_relay(
    endpoint: Endpoint,
    target: &RelayTarget,
    keypair: &Keypair,
    setup_token: Option<String>,
) -> Result<(Connection, SendStream, RecvStream, Vec<u8>)> {
    let conn = endpoint
        .connect(target.relay_addr, "farder-relay")
        .context("failed to initiate relay connection")?
        .await
        .context("relay QUIC handshake failed")?;

    // RelayConnect handshake on the first bi-stream.
    let (mut rc_send, mut rc_recv) = conn.open_bi().await.context("open relay control stream")?;
    let connect_msg = codec::encode(&Message::RelayConnect {
        destination_id: target.server_id.clone(),
    })?;
    write_frame(&mut rc_send, &connect_msg).await?;
    match codec::decode::<Message>(&read_frame(&mut rc_recv).await?)? {
        Message::RelayConnected => {}
        Message::RelayError { reason } => bail!("relay refused: {}", reason),
        other => bail!("unexpected relay reply: {:?}", other),
    }
    // Control stream is done; finish it gracefully (avoid a reset frame).
    let _ = rc_send.finish();

    // Open the primary stream, mark it Primary, then run the normal handshake.
    let (mut send, mut recv) = conn.open_bi().await.context("open primary stream")?;
    write_frame(&mut send, &codec::encode(&RelayStreamRole::Primary)?).await?;
    // An empty token means "no invite" -- the owner auto-claims a fresh server.
    let invite = if target.invite_token.is_empty() {
        None
    } else {
        Some(target.invite_token.clone())
    };
    let session_token = run_client_handshake(&mut send, &mut recv, keypair, invite, setup_token).await?;
    Ok((conn, send, recv, session_token))
}

/// Parse a DIRECT invite link into (server_addr, invite_code), mirroring the
/// frontend's parseInviteLink rules for the two direct forms that carry a code:
/// `farder://host:port/code` and bare `host:port/code`. Setup-token links and
/// code-less links return None (no preview possible).
pub fn parse_direct_invite(s: &str) -> Option<(String, String)> {
    let trimmed = s.trim();
    if trimmed.starts_with("farder://relay") {
        return None; // relay forms are parse_relay_target's job
    }
    let rest = trimmed.strip_prefix("farder://").unwrap_or(trimmed);
    let (addr, token) = rest.split_once('/')?;
    if token.is_empty() || token.starts_with("setup:") {
        return None;
    }
    // addr must look like host:port (same loose rule the frontend uses).
    if !addr.contains(':') {
        return None;
    }
    Some((addr.to_string(), token.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_relayd_link_expands_from_the_default_relay() {
        let (def_addr, def_fp) = crate::default_relay::default_relay().expect("default relay configured");
        let sid = vec![7u8; 32];
        let link = build_compact_relay_link(&sid, "tok123");
        assert_eq!(link, format!("farder://relayd/{}/tok123", hex::encode(&sid)));
        let parsed = parse_relay_target(&link).expect("relayd link must parse");
        assert_eq!(parsed.relay_addr, def_addr);
        assert_eq!(parsed.cert_fp, def_fp);
        assert_eq!(parsed.server_id, sid);
        assert_eq!(parsed.invite_token, "tok123");
    }

    #[test]
    fn relayd_link_with_empty_token_parses() {
        let link = build_compact_relay_link(&vec![7u8; 32], "");
        let parsed = parse_relay_target(&link).expect("empty-token relayd link must parse");
        assert!(parsed.invite_token.is_empty());
    }

    #[test]
    fn full_form_relay_links_still_parse() {
        let target = RelayTarget {
            relay_addr: "1.2.3.4:4433".parse().unwrap(),
            server_id: vec![1u8; 32],
            cert_fp: vec![2u8; 32],
            invite_token: "abc".into(),
        };
        let link = build_relay_link(&target, "abc");
        assert!(parse_relay_target(&link).is_some(), "backward compat: full form must keep parsing");
    }

    #[test]
    fn parses_a_valid_relay_link() {
        let t = parse_relay_target(
            "farder://relay/1.2.3.4:4433/aabb/ccdd/inv123",
        )
        .expect("valid");
        assert_eq!(t.relay_addr, "1.2.3.4:4433".parse().unwrap());
        assert_eq!(t.server_id, vec![0xaa, 0xbb]);
        assert_eq!(t.cert_fp, vec![0xcc, 0xdd]);
        assert_eq!(t.invite_token, "inv123");
    }

    #[test]
    fn rejects_non_relay_or_malformed() {
        assert!(parse_relay_target("farder://1.2.3.4:4435/inv").is_none()); // direct form
        assert!(parse_relay_target("farder://relay/1.2.3.4:4433/aabb").is_none()); // too few parts
        assert!(parse_relay_target("https://example.com").is_none());
        assert!(parse_relay_target("farder://relay/notanaddr/aa/bb/t").is_none()); // bad addr
    }

    #[test]
    fn relay_link_round_trips_an_empty_owner_token() {
        let addr: std::net::SocketAddr = "45.77.70.199:4433".parse().unwrap();
        let target = RelayTarget {
            relay_addr: addr,
            server_id: vec![1u8; 32],
            cert_fp: vec![2u8; 32],
            invite_token: String::new(),
        };
        let link = build_relay_link(&target, ""); // owner link: empty token
        let parsed = parse_relay_target(&link).expect("empty-token link must parse");
        assert_eq!(parsed.relay_addr, addr);
        assert_eq!(parsed.server_id, vec![1u8; 32]);
        assert_eq!(parsed.cert_fp, vec![2u8; 32]);
        assert!(parsed.invite_token.is_empty());
    }

    #[test]
    fn build_relay_link_roundtrips_with_new_code() {
        let target = RelayTarget {
            relay_addr: "1.2.3.4:4433".parse().unwrap(),
            server_id: vec![0xaa, 0xbb],
            cert_fp: vec![0xcc, 0xdd],
            invite_token: "old".into(),
        };
        let link = build_relay_link(&target, "NEWCODE");
        assert_eq!(link, "farder://relay/1.2.3.4:4433/aabb/ccdd/NEWCODE");
        let back = parse_relay_target(&link).expect("parses");
        assert_eq!(back.relay_addr, target.relay_addr);
        assert_eq!(back.server_id, target.server_id);
        assert_eq!(back.cert_fp, target.cert_fp);
        assert_eq!(back.invite_token, "NEWCODE");
    }

    #[test]
    fn direct_invite_links_parse_and_relay_or_tokenless_forms_do_not() {
        assert_eq!(
            parse_direct_invite("farder://203.0.113.7:4433/AbCd1234"),
            Some(("203.0.113.7:4433".to_string(), "AbCd1234".to_string()))
        );
        assert_eq!(
            parse_direct_invite("203.0.113.7:4433/AbCd1234"),
            Some(("203.0.113.7:4433".to_string(), "AbCd1234".to_string()))
        );
        assert!(parse_direct_invite("farder://203.0.113.7:4433/setup:aabb").is_none());
        assert!(parse_direct_invite("203.0.113.7:4433").is_none());
        assert!(parse_direct_invite("farder://relay/1.2.3.4:1/aa/bb/code").is_none());
        assert!(parse_direct_invite("farder://relayd/aabb/code").is_none());
        assert!(parse_direct_invite("AbCd1234").is_none());
    }
}

// ===========================================================================
// Relay integration tests (Phase 3a, Task 6) — close audit Gap #3.
//
// These tests drive the REAL `connect_via_relay` through a relay test-double
// to a mock server, over real QUIC on localhost. `farder-client` is a binary
// crate and CANNOT depend on `farder-server`, so the relay and server here are
// test doubles; the CLIENT (`connect_via_relay`) is the real code under test.
//
// This INVERTS Phase 2's `crates/farder-server/tests/relay_mode.rs`, where the
// client was the double and the server was real. The relay double + 4-byte BE
// framing + `ensure_provider` patterns are lifted from that harness.
//
// The payoff assertion: the mock server's observed `remote_address()` equals
// the relay double's UPSTREAM socket address (the relay's outbound socket
// toward the mock server), NOT the real client's endpoint address — proving the
// server never sees the client's IP.
// ===========================================================================
#[cfg(test)]
mod relay_it {
    use super::*;
    use farder_protocol::server::UploadRequest;
    use quinn::{Connection, Endpoint};
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::RwLock;

    fn ensure_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    // -- 4-byte big-endian framing (matches super::read_frame/write_frame) ----

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

    // -- skip-verify QUIC client endpoint (used by the mock server to dial the
    //    relay; the real client uses the PINNED endpoint from `crate::tls`) ----

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

    fn skip_verify_client_endpoint() -> Endpoint {
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

    // -- relay double (lifted from relay_mode.rs, with the roles INVERTED:
    //    here the SERVER registers and the real CLIENT connects) --------------

    type ConnectionMap = Arc<RwLock<HashMap<Vec<u8>, Connection>>>;

    /// Build a self-signed QUIC *server* endpoint for the relay double, and
    /// return it alongside its cert DER (so the test can pin its fingerprint).
    fn relay_server_endpoint() -> (Endpoint, Vec<u8>) {
        let cert = rcgen::generate_simple_self_signed(vec!["farder-relay".to_string()]).unwrap();
        let cert_der_bytes = cert.cert.der().to_vec();
        let cert_der = rustls::pki_types::CertificateDer::from(cert_der_bytes.clone());
        let key_der =
            rustls::pki_types::PrivateKeyDer::try_from(cert.key_pair.serialize_der()).unwrap();
        let server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
            .unwrap();
        let server_config = quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto).unwrap(),
        ));
        let endpoint =
            Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
        (endpoint, cert_der_bytes)
    }

    /// Start the relay double on an ephemeral port. Returns its listen address
    /// and its real cert DER (for fingerprint pinning by the test).
    async fn start_relay() -> (SocketAddr, Vec<u8>) {
        ensure_provider();
        let (ep, cert_der) = relay_server_endpoint();
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
        (addr, cert_der)
    }

    async fn relay_register(
        server_id: Vec<u8>,
        conn: Connection,
        mut send: SendStream,
        conns: ConnectionMap,
    ) {
        conns.write().await.insert(server_id.clone(), conn.clone());
        write_framed(&mut send, &codec::encode(&Message::RelayRegistered).unwrap()).await;
        conn.closed().await;
        let mut map = conns.write().await;
        if let Some(existing) = map.get(&server_id) {
            if existing.stable_id() == conn.stable_id() {
                map.remove(&server_id);
            }
        }
    }

    async fn relay_connect(
        destination_id: Vec<u8>,
        client_conn: Connection,
        mut send: SendStream,
        conns: ConnectionMap,
    ) {
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
                let err = Message::RelayError {
                    reason: "destination not connected".to_string(),
                };
                write_framed(&mut send, &codec::encode(&err).unwrap()).await;
                let _ = send.finish();
                client_conn.closed().await;
            }
        }
    }

    // -- mock server: dials the relay double (as a QUIC *client*), registers
    //    under `server_id`, then serves each bridged bi-stream by role --------

    /// What the mock server records for the test to assert on.
    #[derive(Default)]
    struct Observed {
        /// The `remote_address()` the server saw on a Primary handshake stream's
        /// connection. Per Gap #3 this MUST be the relay's upstream address.
        remote_addr: Option<SocketAddr>,
        /// Token + first frame received on a `Session` stream (file-framing test).
        session_token: Option<Vec<u8>>,
        session_first_frame: Option<Vec<u8>>,
    }

    /// Start a mock server: it dials the relay, registers under `server_id`,
    /// and serves bridged streams. Returns:
    ///   - the relay's UPSTREAM address as observed by the mock server
    ///     (i.e. `connection.remote_address()` of the server<-relay link), and
    ///   - the shared `Observed` record the handshake handler fills in.
    async fn start_mock_server(
        relay_addr: SocketAddr,
        server_id: Vec<u8>,
    ) -> (SocketAddr, Arc<Mutex<Observed>>) {
        let ep = skip_verify_client_endpoint();
        let conn = ep.connect(relay_addr, "farder-relay").unwrap().await.unwrap();

        // The address the server sees for its link to the relay. Every bridged
        // stream rides this same connection, so every stream's
        // `remote_address()` is this upstream relay socket — never the client's.
        let relay_upstream = conn.remote_address();

        // Register: RelayRegister -> RelayRegistered on the control stream.
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        write_framed(
            &mut send,
            &codec::encode(&Message::RelayRegister {
                server_id: server_id.clone(),
            })
            .unwrap(),
        )
        .await;
        match codec::decode::<Message>(&read_framed(&mut recv).await).unwrap() {
            Message::RelayRegistered => {}
            other => panic!("expected RelayRegistered, got {other:?}"),
        }

        let observed = Arc::new(Mutex::new(Observed::default()));
        let observed_task = observed.clone();
        let conn_task = conn.clone();
        tokio::spawn(async move {
            // Keep the endpoint + control stream alive for the connection's life.
            let _keep_ep = ep;
            let _keep_send = send;
            let _keep_recv = recv;
            loop {
                let (s_send, s_recv) = match conn_task.accept_bi().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let observed = observed_task.clone();
                let remote = conn_task.remote_address();
                tokio::spawn(async move {
                    serve_bridged_stream(s_send, s_recv, remote, observed).await;
                });
            }
        });

        (relay_upstream, observed)
    }

    /// Serve one bridged bi-stream: read its `RelayStreamRole`, then run the
    /// SERVER side of the handshake (Primary) or record the marker (Session).
    async fn serve_bridged_stream(
        mut send: SendStream,
        mut recv: RecvStream,
        remote: SocketAddr,
        observed: Arc<Mutex<Observed>>,
    ) {
        let role: RelayStreamRole = match codec::decode(&read_framed(&mut recv).await) {
            Ok(r) => r,
            Err(_) => return,
        };
        match role {
            RelayStreamRole::Primary => {
                // Record the address we observed for this client (must be relay's).
                observed.lock().unwrap().remote_addr = Some(remote);

                // Server -> Challenge { nonce }
                write_framed(
                    &mut send,
                    &codec::encode(&ServerFrame::Challenge {
                        nonce: [7u8; 32],
                    })
                    .unwrap(),
                )
                .await;
                // Client -> Authenticate (we accept without verifying the sig).
                let frame: ClientFrame =
                    match codec::decode(&read_framed(&mut recv).await) {
                        Ok(f) => f,
                        Err(_) => return,
                    };
                assert!(
                    matches!(frame, ClientFrame::Authenticate { .. }),
                    "expected Authenticate, got {frame:?}"
                );
                // Server -> Authenticated { session_token }
                write_framed(
                    &mut send,
                    &codec::encode(&ServerFrame::Authenticated {
                        session_token: vec![9u8; 32],
                    })
                    .unwrap(),
                )
                .await;
            }
            RelayStreamRole::Session { token } => {
                // Record the token and the next frame for the file-framing test.
                let first = read_framed(&mut recv).await;
                let mut g = observed.lock().unwrap();
                g.session_token = Some(token);
                g.session_first_frame = Some(first);
            }
        }
    }

    /// Build the real, PINNED client endpoint for the relay's cert and return a
    /// `RelayTarget` aimed at it. `cert_fp` in the target is left empty: the
    /// pinning happens in the endpoint, not in `connect_via_relay`.
    fn pinned_client(relay_cert_der: &[u8], relay_addr: SocketAddr, server_id: Vec<u8>) -> (Endpoint, RelayTarget) {
        let fp = crate::tls::cert_fingerprint(relay_cert_der);
        let endpoint = crate::tls::make_pinned_relay_endpoint(fp).unwrap();
        let target = RelayTarget {
            relay_addr,
            server_id,
            cert_fp: vec![],
            invite_token: "inv".into(),
        };
        (endpoint, target)
    }

    // -- Tests ---------------------------------------------------------------

    /// Gap #3: drive the REAL `connect_via_relay` through the relay double to
    /// the mock server, and assert the server only ever saw the relay's address.
    #[tokio::test]
    async fn client_connects_via_relay_and_server_sees_relay_addr() {
        ensure_provider();
        let server_id = vec![1u8; 16];

        let (relay_addr, relay_cert_der) = start_relay().await;
        let (relay_upstream_addr, observed) =
            start_mock_server(relay_addr, server_id.clone()).await;

        let (endpoint, target) = pinned_client(&relay_cert_der, relay_addr, server_id.clone());
        let kp = Keypair::generate();
        let (_conn, _send, _recv, token) =
            connect_via_relay(endpoint, &target, &kp, None)
                .await
                .expect("relay connect");

        assert_eq!(
            token,
            vec![9u8; 32],
            "got the session token from the (mock) server"
        );

        // Gap #3: the address the mock server observed is the RELAY's upstream
        // socket, NOT the real client's endpoint. The client never dials the
        // server directly — the mock server only accepts the relay's connection.
        let observed_addr = observed
            .lock()
            .unwrap()
            .remote_addr
            .expect("server observed an address");
        assert_eq!(
            observed_addr, relay_upstream_addr,
            "server must see the relay's address, never the client's"
        );
    }

    /// Cert pinning: a wrong pinned fingerprint must make the relay QUIC
    /// handshake fail, so `connect_via_relay` errors out.
    #[tokio::test]
    async fn wrong_fingerprint_is_rejected() {
        ensure_provider();
        let server_id = vec![2u8; 16];

        let (relay_addr, _relay_cert_der) = start_relay().await;
        let (_relay_upstream_addr, _observed) =
            start_mock_server(relay_addr, server_id.clone()).await;

        // Pin a fingerprint that does NOT match the relay's real cert.
        let endpoint = crate::tls::make_pinned_relay_endpoint(vec![0u8; 32]).unwrap();
        let target = RelayTarget {
            relay_addr,
            server_id,
            cert_fp: vec![],
            invite_token: "inv".into(),
        };
        let kp = Keypair::generate();
        let result = connect_via_relay(endpoint, &target, &kp, None).await;
        assert!(
            result.is_err(),
            "a wrong pinned fingerprint must reject the relay's cert (got Ok)"
        );
    }

    /// File framing: after a successful relay connect, open a fresh bridged
    /// bi-stream, tag it `Session { token }`, send an `UploadRequest`, and
    /// assert the mock server saw the Session marker with the right token
    /// followed by the readable request bytes.
    #[tokio::test]
    async fn relay_file_stream_sends_session_marker() {
        ensure_provider();
        let server_id = vec![3u8; 16];

        let (relay_addr, relay_cert_der) = start_relay().await;
        let (_relay_upstream_addr, observed) =
            start_mock_server(relay_addr, server_id.clone()).await;

        let (endpoint, target) = pinned_client(&relay_cert_der, relay_addr, server_id.clone());
        let kp = Keypair::generate();
        let (conn, _send, _recv, token) =
            connect_via_relay(endpoint, &target, &kp, None)
                .await
                .expect("relay connect");
        assert_eq!(token, vec![9u8; 32]);

        // Open a NEW bridged bi-stream, tag it as a Session, send an UploadRequest.
        let (mut f_send, _f_recv) = conn.open_bi().await.unwrap();
        write_framed(
            &mut f_send,
            &codec::encode(&RelayStreamRole::Session {
                token: token.clone(),
            })
            .unwrap(),
        )
        .await;
        let upload_req = UploadRequest {
            channel_id: 42,
            file_name: "fox.txt".to_string(),
            file_size: 11,
            hash: "deadbeef".to_string(),
            mime_type: "text/plain".to_string(),
            width: None,
            height: None,
            duration_secs: None,
        };
        let encoded_req = codec::encode(&upload_req).unwrap();
        write_framed(&mut f_send, &encoded_req).await;

        // Poll until the server records the Session stream (bridged async).
        let mut saw = None;
        for _ in 0..200 {
            if let Some(tok) = observed.lock().unwrap().session_token.clone() {
                saw = Some(tok);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let saw_token = saw.expect("server never received the Session marker");
        assert_eq!(
            saw_token, token,
            "server must see the Session stream tagged with our session token"
        );
        let first_frame = observed
            .lock()
            .unwrap()
            .session_first_frame
            .clone()
            .expect("server received a frame after the Session marker");
        assert_eq!(
            first_frame, encoded_req,
            "server must receive the UploadRequest bytes after the Session marker"
        );
        // Sanity: those bytes decode back to the request we sent.
        let decoded: UploadRequest = codec::decode(&first_frame).unwrap();
        assert_eq!(decoded.file_name, "fox.txt");
        assert_eq!(decoded.channel_id, 42);
    }
}
