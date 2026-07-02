//! Inbound HTTP ingress for incoming webhooks. Accepts `POST /webhook/:server_id/:token`,
//! rate-limits by peer IP, caps the body at 64 KiB, then forwards the request to the
//! registered farder-server over the existing QUIC tunnel as a `RelayStreamRole::Webhook`
//! stream and relays the server's 2-byte BE status code back as the HTTP response status.

use axum::{
    extract::{ConnectInfo, Path, State},
    http::StatusCode,
    routing::post,
    Router,
};
use farder_protocol::{codec, server::RelayStreamRole};
use quinn::Connection;
use std::{net::SocketAddr, sync::Arc};

use crate::router::SharedState;

pub async fn serve(
    bind: SocketAddr,
    state: SharedState,
    limiter: Arc<crate::limits::ConnectionLimiter>,
) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/webhook/:server_id/:token", post(handle))
        .with_state((state, limiter));
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("webhook HTTP listening on {}", bind);
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

async fn handle(
    State((state, limiter)): State<(SharedState, Arc<crate::limits::ConnectionLimiter>)>,
    Path((server_id, token)): Path<(String, String)>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    body: axum::body::Bytes,
) -> StatusCode {
    if limiter.try_admit(peer.ip(), std::time::Instant::now()).is_none() {
        return StatusCode::TOO_MANY_REQUESTS;
    }
    if body.len() > 64 * 1024 {
        return StatusCode::PAYLOAD_TOO_LARGE;
    }
    let Ok(id_bytes) = hex::decode(&server_id) else {
        return StatusCode::NOT_FOUND;
    };
    let conn = {
        let map = state.servers.read().await;
        map.get(&id_bytes).map(|r| r.conn.clone())
    };
    let Some(conn) = conn else {
        return StatusCode::NOT_FOUND;
    };
    match forward(&conn, token, body.to_vec()).await {
        Ok(code) => StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY),
        Err(_) => StatusCode::BAD_GATEWAY,
    }
}

async fn forward(conn: &Connection, token: String, body: Vec<u8>) -> anyhow::Result<u16> {
    let (mut s, mut r) = conn.open_bi().await?;
    // Reserved handle 0 marks this as a relay-originated stream (same convention
    // as the invite-preview path in proxy.rs / router.rs).
    s.write_all(&0u32.to_be_bytes()).await?;
    crate::proxy::write_framed(
        &mut s,
        &codec::encode(&RelayStreamRole::Webhook { token, body })?,
    )
    .await?;
    // Server writes 2-byte big-endian HTTP-ish status code then closes the stream.
    let ack = crate::proxy::read_framed(&mut r).await?;
    if ack.len() < 2 {
        anyhow::bail!("short webhook ack ({} bytes)", ack.len());
    }
    Ok(u16::from_be_bytes([ack[0], ack[1]]))
}
