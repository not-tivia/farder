//! Browser gateway: turns a WebSocket (what a browser can open) into the QUIC stream a Farder
//! server expects (what a browser cannot open). See docs/web-guest-join-design.md, step 2.
//!
//! It is a DUMB FRAME PUMP, on purpose:
//!
//! * One WebSocket binary message = one Farder frame (the server's own 4-byte length prefix is
//!   added/removed here). The gateway never decodes, signs or invents a frame, and never holds a
//!   key: the guest's identity lives in the browser, so the gateway cannot speak for the guest.
//! * It does see normal-channel traffic in the clear, exactly as a relay does today. The guest
//!   page says so.
//! * Only the server's MAIN stream is carried (chat, history, members). No uploads, no voice.
//!
//! Guardrails: a route table (a browser can only reach servers the host listed), a cap on
//! simultaneous connections per client address, a cap on frame size, and an idle timeout.

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use quinn::{ClientConfig, Endpoint};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::Message as Ws;

/// A server the gateway may reach.
#[derive(Clone, Debug)]
pub struct Route {
    pub addr: SocketAddr,
    /// SHA-256 of the server's certificate. `None` means "do not check" and is for a server on
    /// the same machine or LAN only (the server makes a fresh certificate each start).
    pub cert_fp: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct GatewayConfig {
    /// URL path name (`/ws/<name>`) -> server.
    pub routes: HashMap<String, Route>,
    /// Simultaneous connections allowed from one client address.
    pub max_per_ip: usize,
    /// A connection with no traffic either way for this long is closed.
    pub idle: Duration,
    /// Largest frame a browser may send (the server accepts more; a guest has no need).
    pub max_frame: usize,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self { routes: HashMap::new(), max_per_ip: 8, idle: Duration::from_secs(300), max_frame: 1024 * 1024 }
    }
}

#[derive(Debug)]
struct PinVerifier(Option<Vec<u8>>);
impl rustls::client::danger::ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _i: &[rustls::pki_types::CertificateDer<'_>],
        _n: &rustls::pki_types::ServerName<'_>,
        _o: &[u8],
        _t: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if let Some(fp) = &self.0 {
            if Sha256::digest(end_entity.as_ref()).as_slice() != fp.as_slice() {
                return Err(rustls::Error::General("server certificate does not match the pinned fingerprint".into()));
            }
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, _m: &[u8], _c: &rustls::pki_types::CertificateDer<'_>, _d: &rustls::DigitallySignedStruct) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(&self, _m: &[u8], _c: &rustls::pki_types::CertificateDer<'_>, _d: &rustls::DigitallySignedStruct) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
    }
}

fn client_endpoint(fp: Option<Vec<u8>>) -> Result<Endpoint> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let crypto = rustls::ClientConfig::builder().dangerous().with_custom_certificate_verifier(Arc::new(PinVerifier(fp))).with_no_client_auth();
    let cfg = ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?));
    let mut ep = Endpoint::client("0.0.0.0:0".parse().unwrap())?;
    ep.set_default_client_config(cfg);
    Ok(ep)
}

/// Counts open connections per client address; a slot is released when its guard drops.
#[derive(Default)]
struct Limiter(Mutex<HashMap<IpAddr, usize>>);
struct Slot(Arc<Limiter>, IpAddr);
impl Limiter {
    fn take(self: &Arc<Self>, ip: IpAddr, max: usize) -> Option<Slot> {
        let mut m = self.0.lock().unwrap();
        let n = m.entry(ip).or_insert(0);
        if *n >= max { return None; }
        *n += 1;
        Some(Slot(self.clone(), ip))
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        let mut m = self.0 .0.lock().unwrap();
        if let Some(n) = m.get_mut(&self.1) {
            *n = n.saturating_sub(1);
            if *n == 0 { m.remove(&self.1); }
        }
    }
}

/// Serve forever on an already-bound listener.
pub async fn serve(listener: TcpListener, cfg: GatewayConfig) -> Result<()> {
    let cfg = Arc::new(cfg);
    let limiter = Arc::new(Limiter::default());
    loop {
        let (tcp, peer) = listener.accept().await?;
        let (cfg, limiter) = (cfg.clone(), limiter.clone());
        tokio::spawn(async move {
            if let Err(e) = handle(tcp, peer.ip(), cfg, limiter).await {
                tracing::debug!("gateway connection from {peer} ended: {e:#}");
            }
        });
    }
}

async fn handle(tcp: TcpStream, ip: IpAddr, cfg: Arc<GatewayConfig>, limiter: Arc<Limiter>) -> Result<()> {
    let mut chosen: Option<Route> = None;
    let routes = cfg.routes.clone();
    let ws = tokio_tungstenite::accept_hdr_async_with_config(
        tcp,
        |req: &Request, resp: Response| -> std::result::Result<Response, ErrorResponse> {
            let name = req.uri().path().strip_prefix("/ws/").unwrap_or("");
            match routes.get(name) {
                Some(r) => { chosen = Some(r.clone()); Ok(resp) }
                None => Err(tokio_tungstenite::tungstenite::http::Response::builder().status(StatusCode::NOT_FOUND).body(Some("no such server".to_string())).unwrap()),
            }
        },
        Some({
            let mut c = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
            c.max_message_size = Some(cfg.max_frame);
            c.max_frame_size = Some(cfg.max_frame);
            c
        }),
    )
    .await
    .context("websocket handshake")?;
    let route = chosen.context("no route")?;
    let _slot = limiter.take(ip, cfg.max_per_ip).context("too many connections from this address")?;

    let ep = client_endpoint(route.cert_fp.clone())?;
    let conn = ep.connect(route.addr, "farder-server")?.await.context("could not reach the server")?;
    // The server opens the main stream (Challenge first); accept it.
    let (mut qsend, mut qrecv) = conn.accept_bi().await.context("server did not open its main stream")?;
    let (mut wtx, mut wrx) = ws.split();

    let idle = cfg.idle;
    let max_frame = cfg.max_frame;
    // server -> browser
    let down = async {
        loop {
            let mut len = [0u8; 4];
            qrecv.read_exact(&mut len).await?;
            let n = u32::from_be_bytes(len) as usize;
            if n > 16 * 1024 * 1024 { bail!("server frame too large: {n}"); }
            let mut buf = vec![0u8; n];
            qrecv.read_exact(&mut buf).await?;
            wtx.send(Ws::Binary(buf)).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };
    // browser -> server
    let up = async {
        while let Some(msg) = wrx.next().await {
            match msg? {
                Ws::Binary(b) => {
                    if b.len() > max_frame { bail!("browser frame too large"); }
                    qsend.write_all(&(b.len() as u32).to_be_bytes()).await?;
                    qsend.write_all(&b).await?;
                }
                Ws::Close(_) => break,
                Ws::Ping(_) | Ws::Pong(_) => {}
                // Text frames are not part of the protocol.
                _ => bail!("only binary messages are allowed"),
            }
        }
        Ok::<(), anyhow::Error>(())
    };
    // Either side finishing ends the connection; so does going quiet for `idle`.
    tokio::select! {
        r = down => r,
        r = up => r,
        _ = tokio::time::sleep(idle) => { tracing::debug!("idle timeout"); Ok(()) }
    }
}
