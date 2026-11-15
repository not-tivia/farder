//! The gateway, against (1) a fake QUIC server that pins down the pump's own behaviour and
//! (2) the REAL farder-server binary, through the real handshake and requests, as a browser
//! would speak them.

use farder_crypto::identity::Keypair;
use farder_gateway::{serve, GatewayConfig, Route};
use farder_protocol::{
    codec,
    server::{ClientFrame, ServerFrame, ServerRequest, ServerResponse},
};
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as Ws;

type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn provider() { let _ = rustls::crypto::ring::default_provider().install_default(); }

async fn start_gateway(cfg: GatewayConfig) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    tokio::spawn(async move { let _ = serve(l, cfg).await; });
    a
}

// ---------------------------------------------------------------- fake server

/// A QUIC server that opens its main stream, says "hello" as one frame, then echoes every frame
/// back upper-cased. Returns its address and the SHA-256 of its certificate.
async fn fake_server() -> (SocketAddr, Vec<u8>) {
    provider();
    let cert = rcgen::generate_simple_self_signed(vec!["farder-server".to_string()]).unwrap();
    let der = cert.cert.der().to_vec();
    let fp = Sha256::digest(&der).to_vec();
    let key = rustls::pki_types::PrivateKeyDer::try_from(cert.key_pair.serialize_der()).unwrap();
    let crypto = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(vec![rustls::pki_types::CertificateDer::from(der)], key).unwrap();
    let cfg = quinn::ServerConfig::with_crypto(Arc::new(quinn::crypto::rustls::QuicServerConfig::try_from(crypto).unwrap()));
    let ep = quinn::Endpoint::server(cfg, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = ep.local_addr().unwrap();
    tokio::spawn(async move {
        while let Some(inc) = ep.accept().await {
            tokio::spawn(async move {
                let Ok(conn) = inc.await else { return };
                let Ok((mut s, mut r)) = conn.open_bi().await else { return };
                let write = |b: Vec<u8>| async move { b };
                let hello = write(b"hello".to_vec()).await;
                s.write_all(&(hello.len() as u32).to_be_bytes()).await.unwrap();
                s.write_all(&hello).await.unwrap();
                loop {
                    let mut l = [0u8; 4];
                    if r.read_exact(&mut l).await.is_err() { return; }
                    let mut b = vec![0u8; u32::from_be_bytes(l) as usize];
                    if r.read_exact(&mut b).await.is_err() { return; }
                    let up = b.to_ascii_uppercase();
                    if s.write_all(&(up.len() as u32).to_be_bytes()).await.is_err() { return; }
                    if s.write_all(&up).await.is_err() { return; }
                }
            });
        }
    });
    (addr, fp)
}

async fn ws(gw: SocketAddr, name: &str) -> Result<WsStream, tokio_tungstenite::tungstenite::Error> {
    tokio_tungstenite::connect_async(format!("ws://{gw}/ws/{name}")).await.map(|(s, _)| s)
}

async fn next_bin(w: &mut WsStream) -> Option<Vec<u8>> {
    match tokio::time::timeout(Duration::from_secs(10), w.next()).await {
        Ok(Some(Ok(Ws::Binary(b)))) => Some(b),
        _ => None,
    }
}

fn cfg_with(name: &str, addr: SocketAddr, fp: Option<Vec<u8>>) -> GatewayConfig {
    let mut c = GatewayConfig::default();
    c.routes.insert(name.to_string(), Route { addr, cert_fp: fp });
    c
}

#[tokio::test]
async fn frames_pass_both_ways_one_message_per_frame() {
    let (srv, fp) = fake_server().await;
    let gw = start_gateway(cfg_with("t", srv, Some(fp))).await;
    let mut w = ws(gw, "t").await.unwrap();
    assert_eq!(next_bin(&mut w).await.unwrap(), b"hello");
    w.send(Ws::Binary(b"ping one".to_vec())).await.unwrap();
    w.send(Ws::Binary(b"ping two".to_vec())).await.unwrap();
    assert_eq!(next_bin(&mut w).await.unwrap(), b"PING ONE");
    assert_eq!(next_bin(&mut w).await.unwrap(), b"PING TWO");
}

#[tokio::test]
async fn an_unlisted_server_name_is_refused() {
    let (srv, fp) = fake_server().await;
    let gw = start_gateway(cfg_with("t", srv, Some(fp))).await;
    assert!(ws(gw, "somewhere-else").await.is_err(), "a browser must not reach a server the host did not list");
}

#[tokio::test]
async fn a_certificate_that_does_not_match_the_pin_is_refused() {
    let (srv, _real) = fake_server().await;
    let gw = start_gateway(cfg_with("t", srv, Some(vec![0u8; 32]))).await;
    let mut w = ws(gw, "t").await.unwrap();
    assert!(next_bin(&mut w).await.is_none(), "no frame may arrive from a server whose certificate is not the pinned one");
}

#[tokio::test]
async fn text_messages_and_oversized_frames_close_the_connection() {
    let (srv, fp) = fake_server().await;
    let mut c = cfg_with("t", srv, Some(fp));
    c.max_frame = 1000;
    let gw = start_gateway(c).await;
    let mut w = ws(gw, "t").await.unwrap();
    assert_eq!(next_bin(&mut w).await.unwrap(), b"hello");
    w.send(Ws::Text("hi".into())).await.unwrap();
    assert!(next_bin(&mut w).await.is_none());
    let mut w2 = ws(gw, "t").await.unwrap();
    assert_eq!(next_bin(&mut w2).await.unwrap(), b"hello");
    let _ = w2.send(Ws::Binary(vec![b'a'; 5000])).await;
    assert!(next_bin(&mut w2).await.is_none());
}

#[tokio::test]
async fn one_address_cannot_hold_more_than_its_share_of_connections() {
    let (srv, fp) = fake_server().await;
    let mut c = cfg_with("t", srv, Some(fp));
    c.max_per_ip = 2;
    let gw = start_gateway(c).await;
    let mut a = ws(gw, "t").await.unwrap();
    let mut b = ws(gw, "t").await.unwrap();
    assert!(next_bin(&mut a).await.is_some() && next_bin(&mut b).await.is_some());
    let mut third = ws(gw, "t").await.unwrap();
    assert!(next_bin(&mut third).await.is_none(), "the third connection from one address must be dropped");
    // Closing one frees its place.
    drop(a);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut fourth = ws(gw, "t").await.unwrap();
    assert_eq!(next_bin(&mut fourth).await.unwrap(), b"hello");
}

// ------------------------------------------------------------- the real server

struct RealServer { child: std::process::Child, addr: SocketAddr, token: String, _dir: tempfile::TempDir }
impl Drop for RealServer { fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); } }

async fn real_server() -> RealServer {
    let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/farder-server");
    assert!(bin.exists(), "build the server first: cargo build -p farder-server ({})", bin.display());
    let port = { let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap(); s.local_addr().unwrap().port() };
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("server.log");
    let out = std::fs::File::create(&log).unwrap();
    let child = std::process::Command::new(&bin)
        .args(["--bind", &format!("127.0.0.1:{port}"), "--name", "GW Test", "--db"]).arg(dir.path().join("s.db"))
        .arg("--storage-dir").arg(dir.path().join("files")).arg("--data-dir").arg(dir.path().join("sd"))
        .stdout(out.try_clone().unwrap()).stderr(out).spawn().unwrap();
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let t = std::fs::read_to_string(&log).unwrap_or_default();
        if t.contains("Server listening") {
            let clean: String = t.chars().filter(|c| !c.is_control() || *c == '\n').collect();
            let tok = clean.split("token instead: ").nth(1).map(|x| x.split_whitespace().next().unwrap().trim_matches(|c: char| !c.is_ascii_hexdigit()).to_string()).expect("setup token in the log");
            return RealServer { child, addr: format!("127.0.0.1:{port}").parse().unwrap(), token: tok, _dir: dir };
        }
    }
    panic!("server did not start: {}", std::fs::read_to_string(&log).unwrap_or_default());
}

async fn send_frame(w: &mut WsStream, f: &ClientFrame) { w.send(Ws::Binary(codec::encode(f).unwrap())).await.unwrap(); }
async fn recv_frame(w: &mut WsStream) -> ServerFrame { codec::decode(&next_bin(w).await.expect("a frame")).unwrap() }

/// What a browser does: take the Challenge, sign it with ITS OWN key, authenticate.
async fn login(gw: SocketAddr, kp: &Keypair, invite: Option<String>, setup: Option<String>) -> Result<WsStream, String> {
    let mut w = ws(gw, "real").await.map_err(|e| e.to_string())?;
    let ServerFrame::Challenge { nonce } = recv_frame(&mut w).await else { return Err("no challenge".into()) };
    send_frame(&mut w, &ClientFrame::Authenticate { public_key: kp.public_key(), signed_challenge: kp.sign(&nonce), invite_code: invite, setup_token: setup }).await;
    match recv_frame(&mut w).await {
        ServerFrame::Authenticated { .. } => Ok(w),
        ServerFrame::AuthError { reason } => Err(reason),
        other => Err(format!("unexpected {other:?}")),
    }
}

async fn ask(w: &mut WsStream, id: u32, body: ServerRequest) -> ServerResponse {
    send_frame(w, &ClientFrame::Request { id, body }).await;
    loop {
        if let ServerFrame::Response { request_id, body } = recv_frame(w).await { if request_id == id { return body; } }
    }
}

#[tokio::test]
async fn a_browser_speaking_the_real_protocol_through_the_gateway_reaches_the_real_server() {
    let _ = tracing_subscriber::fmt().with_env_filter("farder_gateway=debug").with_test_writer().try_init();
    let srv = real_server().await;
    let gw = start_gateway(cfg_with("real", srv.addr, None)).await;
    let owner = Keypair::generate();
    let mut o = login(gw, &owner, None, Some(srv.token.clone())).await.expect("owner claims the server through the gateway");
    match ask(&mut o, 1, ServerRequest::GetServerInfo).await {
        ServerResponse::ServerInfo { name, .. } => assert_eq!(name, "GW Test"),
        other => panic!("{other:?}"),
    }
    // Guest links: off by default; the owner turns them on and makes one.
    assert!(matches!(ask(&mut o, 2, ServerRequest::CreateGuestInvite { max_uses: Some(1), expires_in_secs: Some(600) }).await, ServerResponse::Error { .. }));
    assert!(matches!(ask(&mut o, 3, ServerRequest::SetGuestLinks { enabled: true }).await, ServerResponse::GuestLinks { enabled: true }));
    let ServerResponse::InviteCreated { code } = ask(&mut o, 4, ServerRequest::CreateGuestInvite { max_uses: Some(1), expires_in_secs: Some(600) }).await else { panic!("no guest invite") };

    // A guest, with their own throwaway key, joins from a "browser".
    let guest = Keypair::generate();
    let mut g = login(gw, &guest, Some(code.clone()), None).await.expect("the guest joins with the guest link");
    match ask(&mut g, 1, ServerRequest::GetServerInfo).await {
        ServerResponse::ServerInfo { name, .. } => assert_eq!(name, "GW Test"),
        other => panic!("{other:?}"),
    }
    // The guest is limited: no invites of either kind.
    assert!(matches!(ask(&mut g, 2, ServerRequest::CreateInvite { max_uses: None, expires_in_secs: None, target_channel: None }).await, ServerResponse::Error { .. }));
    assert!(matches!(ask(&mut g, 3, ServerRequest::CreateGuestInvite { max_uses: None, expires_in_secs: None }).await, ServerResponse::Error { .. }));
    // The link was single use.
    let again = login(gw, &Keypair::generate(), Some(code), None).await;
    assert!(again.is_err(), "a used-up guest link must not admit a second person");
    // A guest who loses the link's keys cannot be impersonated: a wrong signature is refused.
    let mut w = ws(gw, "real").await.unwrap();
    let ServerFrame::Challenge { nonce } = recv_frame(&mut w).await else { panic!() };
    let imposter = Keypair::generate();
    send_frame(&mut w, &ClientFrame::Authenticate { public_key: guest.public_key(), signed_challenge: imposter.sign(&nonce), invite_code: None, setup_token: None }).await;
    assert!(matches!(recv_frame(&mut w).await, ServerFrame::AuthError { .. }));
}
