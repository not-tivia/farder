//! Test fixture for the browser guest page: starts a real farder-server and the gateway, plays the
//! OWNER (claims the server, turns guest links on, makes a guest link with its signed invite event,
//! posts a first message) and prints the link parts. Leave it running while a browser opens the page.
//!
//!   cargo build -p farder-server && cargo run -p farder-gateway --example guest_fixture
use farder_crypto::event_log::{invite_code_hash, DeviceCert, Event, EventPayload};
use farder_crypto::identity::Keypair;
use farder_gateway::{serve, GatewayConfig, Route};
use farder_protocol::{codec, server::*};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as Ws;

type W = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn recv(w: &mut W) -> ServerFrame {
    loop {
        if let Some(Ok(Ws::Binary(b))) = w.next().await { return codec::decode(&b).unwrap(); }
    }
}
async fn ask(w: &mut W, id: u32, body: ServerRequest) -> ServerResponse {
    w.send(Ws::Binary(codec::encode(&ClientFrame::Request { id, body }).unwrap())).await.unwrap();
    loop { if let ServerFrame::Response { request_id, body } = recv(w).await { if request_id == id { return body; } } }
}

#[tokio::main]
async fn main() {
    let port: u16 = { let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap(); s.local_addr().unwrap().port() };
    let dir = std::env::temp_dir().join("farder-guest-fixture");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/farder-server");
    let log = dir.join("server.log");
    let out = std::fs::File::create(&log).unwrap();
    let mut child = std::process::Command::new(&bin)
        .args(["--bind", &format!("127.0.0.1:{port}"), "--name", "Friends Server", "--db"]).arg(dir.join("s.db"))
        .arg("--storage-dir").arg(dir.join("files")).arg("--data-dir").arg(dir.join("sd"))
        .stdout(out.try_clone().unwrap()).stderr(out).spawn().unwrap();
    let token = loop {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let t = std::fs::read_to_string(&log).unwrap_or_default();
        if t.contains("Server listening") {
            let clean: String = t.chars().filter(|c| !c.is_control() || *c == '\n').collect();
            break clean.split("token instead: ").nth(1).unwrap().split_whitespace().next().unwrap().trim_matches(|c: char| !c.is_ascii_hexdigit()).to_string();
        }
    };
    let mut cfg = GatewayConfig::default();
    cfg.routes.insert("friends".into(), Route { addr: format!("127.0.0.1:{port}").parse().unwrap(), cert_fp: None });
    let l = tokio::net::TcpListener::bind("127.0.0.1:8480").await.unwrap();
    tokio::spawn(async move { let _ = serve(l, cfg).await; });

    // The owner, through the gateway.
    let owner = Keypair::generate();
    let dev = Keypair::generate();
    let (mut w, _) = tokio_tungstenite::connect_async("ws://127.0.0.1:8480/ws/friends").await.unwrap();
    let ServerFrame::Challenge { nonce } = recv(&mut w).await else { panic!() };
    w.send(Ws::Binary(codec::encode(&ClientFrame::Authenticate { public_key: owner.public_key(), signed_challenge: owner.sign(&nonce), invite_code: None, setup_token: Some(token) }).unwrap())).await.unwrap();
    assert!(matches!(recv(&mut w).await, ServerFrame::Authenticated { .. }));
    let ServerResponse::ServerInfo { server_id: Some(sid), channels, .. } = ask(&mut w, 1, ServerRequest::GetServerInfo).await else { panic!("info") };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let da = Event::next(&dev, owner.public_key(), sid.clone(), None, 0, now, EventPayload::DeviceAuthorized { cert: DeviceCert::create(&owner, &dev.public_key(), now) });
    assert!(!matches!(ask(&mut w, 2, ServerRequest::SubmitEvent { event: da.clone() }).await, ServerResponse::Error { .. }), "owner device");
    assert!(matches!(ask(&mut w, 3, ServerRequest::SetGuestLinks { enabled: true }).await, ServerResponse::GuestLinks { enabled: true }));
    let ServerResponse::InviteCreated { code } = ask(&mut w, 4, ServerRequest::CreateGuestInvite { max_uses: Some(5), expires_in_secs: Some(3600) }).await else { panic!("invite") };
    let inv = Event::next(&dev, owner.public_key(), sid.clone(), Some(&da), da.core.lamport, now, EventPayload::InviteCreated { code_hash: invite_code_hash(&code), max_uses: 5, expires_at: now + 3600, requires_approval: false });
    let r = ask(&mut w, 5, ServerRequest::SubmitEvent { event: inv }).await;
    assert!(!matches!(r, ServerResponse::Error { .. }), "invite event: {r:?}");
    let general = channels.iter().find(|c| c.name == "general").unwrap().id;
    let r = ask(&mut w, 6, ServerRequest::SendMessage { channel_id: general, content: "Welcome to the friends server!".into(), reply_to: None, attachment_ids: vec![] }).await;
    assert!(matches!(r, ServerResponse::MessageSent { .. }), "{r:?}");
    println!("GUEST_READY server=friends code={code}");
    // Keep the owner connected (and the server up) until killed.
    let _keep = &mut w;
    tokio::signal::ctrl_c().await.unwrap();
    let _ = child.kill();
}
