//! Relay-mode server transport. Instead of binding a public listener, the
//! server dials a relay, registers under its stable `server_id`, and serves
//! each relay-bridged stream as a client session -- reusing the same auth /
//! main_loop / file-transfer code as direct mode. The server never learns a
//! client's real address (only its relay connection). Voice/datagrams ARE
//! served over the relay via UDP datagrams prefixed with the 4-byte routing
//! handle assigned by the relay (Phase 5b).

use crate::connection::{authenticate, cleanup_session, handle_auxiliary_stream, main_loop};
use crate::state::ServerState;
use anyhow::{Context, Result};
use farder_protocol::{codec, messages::Message, server::RelayStreamRole};
use quinn::{Endpoint, RecvStream, SendStream};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

const SERVER_ID_FILE: &str = "server_id";

/// Load the server's stable 32-byte relay id from `<data_dir>/server_id`,
/// generating and persisting one on first run.
pub fn load_or_generate_server_id(data_dir: &Path) -> Result<[u8; 32]> {
    std::fs::create_dir_all(data_dir)?;
    let path = data_dir.join(SERVER_ID_FILE);
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() == 32 {
            let mut id = [0u8; 32];
            id.copy_from_slice(&bytes);
            return Ok(id);
        }
    }
    let id: [u8; 32] = rand::random();
    std::fs::write(&path, id)?;
    Ok(id)
}

// 4-byte big-endian length-prefixed framing. MUST match connection.rs read_frame/write_frame
// AND the relay's read_message/write_message. (Confirm against connection.rs.)
async fn write_framed(send: &mut SendStream, data: &[u8]) -> Result<()> {
    send.write_all(&(data.len() as u32).to_be_bytes()).await?;
    send.write_all(data).await?;
    Ok(())
}
async fn read_framed(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await?;
    let n = u32::from_be_bytes(len) as usize;
    anyhow::ensure!(n <= 16 * 1024 * 1024, "relay frame too large");
    let mut buf = vec![0u8; n];
    recv.read_exact(&mut buf).await?;
    Ok(buf)
}

/// QUIC client endpoint that accepts the relay's cert (pinning is Phase 3).
fn relay_client_endpoint() -> Result<Endpoint> {
    #[derive(Debug)]
    struct SkipVerify;
    impl rustls::client::danger::ServerCertVerifier for SkipVerify {
        fn verify_server_cert(&self, _e: &rustls::pki_types::CertificateDer<'_>, _i: &[rustls::pki_types::CertificateDer<'_>], _n: &rustls::pki_types::ServerName<'_>, _o: &[u8], _t: rustls::pki_types::UnixTime) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> { Ok(rustls::client::danger::ServerCertVerified::assertion()) }
        fn verify_tls12_signature(&self, _m: &[u8], _c: &rustls::pki_types::CertificateDer<'_>, _d: &rustls::DigitallySignedStruct) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> { Ok(rustls::client::danger::HandshakeSignatureValid::assertion()) }
        fn verify_tls13_signature(&self, _m: &[u8], _c: &rustls::pki_types::CertificateDer<'_>, _d: &rustls::DigitallySignedStruct) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> { Ok(rustls::client::danger::HandshakeSignatureValid::assertion()) }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> { rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes() }
    }
    let crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipVerify))
        .with_no_client_auth();
    let mut cfg = quinn::ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?));
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(std::time::Duration::from_secs(15)));
    transport.datagram_receive_buffer_size(Some(1 << 20));
    transport.datagram_send_buffer_size(1 << 20);
    cfg.transport_config(Arc::new(transport));
    let mut ep = Endpoint::client("0.0.0.0:0".parse()?)?;
    ep.set_default_client_config(cfg);
    Ok(ep)
}

/// Connect to the relay, register under `server_id`, and serve bridged streams.
/// Reconnects with capped backoff if the relay connection drops.
pub async fn serve_via_relay(state: Arc<ServerState>, relay_addr: SocketAddr, server_id: [u8; 32]) -> Result<()> {
    let endpoint = relay_client_endpoint()?;
    let mut backoff = Duration::from_millis(500);
    loop {
        match connect_and_serve(&endpoint, relay_addr, server_id, &state).await {
            Ok(()) => backoff = Duration::from_millis(500),
            Err(e) => warn!("relay session ended: {}; reconnecting", e),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

async fn connect_and_serve(endpoint: &Endpoint, relay_addr: SocketAddr, server_id: [u8; 32], state: &Arc<ServerState>) -> Result<()> {
    let conn = endpoint.connect(relay_addr, "farder-relay").context("connect relay")?.await.context("relay handshake")?;
    let (mut send, mut recv) = conn.open_bi().await?;
    write_framed(&mut send, &codec::encode(&Message::RelayRegister { server_id: server_id.to_vec() })?).await?;
    let ack: Message = codec::decode(&read_framed(&mut recv).await?)?;
    anyhow::ensure!(matches!(ack, Message::RelayRegistered), "relay did not confirm registration");
    info!("registered with relay {}", relay_addr);

    {
        let dg_state = Arc::clone(state);
        let dg_conn = conn.clone();
        tokio::spawn(async move {
            let media_config = crate::media_stream::MediaConfig::default();
            loop {
                match dg_conn.read_datagram().await {
                    Ok(dg) => {
                        if dg.len() < 4 { continue; }
                        let handle = u32::from_be_bytes([dg[0], dg[1], dg[2], dg[3]]);
                        let sender_pk = { dg_state.relay_voice_handles.read().await.get(&handle).copied() };
                        if let Some(pk) = sender_pk {
                            crate::connection::process_inbound_voice_frame(&dg_state, pk, dg.slice(4..), &media_config).await;
                        }
                    }
                    Err(_) => break, // relay connection closed
                }
            }
        });
    }

    loop {
        let (s, r) = conn.accept_bi().await?;
        let state = Arc::clone(state);
        let relay_conn = conn.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_relay_stream(state, relay_conn, s, r).await {
                tracing::debug!("relay stream ended: {}", e);
            }
        });
    }
}

async fn serve_relay_stream(state: Arc<ServerState>, relay_conn: quinn::Connection, send: SendStream, mut recv: RecvStream) -> Result<()> {
    // Phase 5b: every bridged stream is prefixed with the relay-assigned 4-byte
    // routing handle (authoritative). Read it before the RelayStreamRole.
    let mut hb = [0u8; 4];
    recv.read_exact(&mut hb).await?;
    let handle = u32::from_be_bytes(hb);
    anyhow::ensure!(handle != 0, "relay sent reserved routing handle 0");
    let role: RelayStreamRole = codec::decode(&read_framed(&mut recv).await?)?;
    match role {
        RelayStreamRole::Primary => run_relay_primary(state, relay_conn, handle, send, recv).await,
        // The routing handle is consumed above but unused for Session streams:
        // auxiliary streams are authenticated by their session token, not the handle.
        RelayStreamRole::Session { token } => run_relay_aux(state, send, recv, token).await,
    }
}

async fn run_relay_primary(state: Arc<ServerState>, relay_conn: quinn::Connection, handle: u32, mut send: SendStream, mut recv: RecvStream) -> Result<()> {
    let outcome = authenticate(&state, &mut send, &mut recv).await?;
    let pk_bytes = outcome.pk_bytes;
    // Phase 5b: bind this relayed member to its routing handle for voice.
    state.voice_connections.write().await.insert(pk_bytes, crate::state::VoiceSink::Relayed { relay: relay_conn.clone(), handle });
    state.relay_voice_handles.write().await.insert(handle, pk_bytes);

    let loop_result = main_loop(
        Arc::clone(&state), outcome.public_key.clone(), outcome.is_owner, &mut send, &mut recv, outcome.event_rx,
    ).await;

    state.voice_connections.write().await.remove(&pk_bytes);
    state.relay_voice_handles.write().await.remove(&handle);
    cleanup_session(&state, &outcome.public_key, outcome.pk_bytes, &outcome.event_tx, &outcome.session_token).await;
    loop_result
}

async fn run_relay_aux(state: Arc<ServerState>, send: SendStream, recv: RecvStream, token: Vec<u8>) -> Result<()> {
    let token: [u8; 32] = token.as_slice().try_into().map_err(|_| anyhow::anyhow!("bad session token length"))?;
    let (member_key, is_owner) = state.lookup_session(&token).await.ok_or_else(|| anyhow::anyhow!("unknown or expired session token"))?;
    handle_auxiliary_stream(&state, &member_key, is_owner, send, recv).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_id_persists_and_differs_per_dir() {
        let d1 = tempfile::tempdir().unwrap();
        let a = load_or_generate_server_id(d1.path()).unwrap();
        let b = load_or_generate_server_id(d1.path()).unwrap();
        assert_eq!(a, b, "server_id must persist across calls in the same dir");
        let d2 = tempfile::tempdir().unwrap();
        let c = load_or_generate_server_id(d2.path()).unwrap();
        assert_ne!(a, c, "fresh dir gets its own id");
    }
}
