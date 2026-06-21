use anyhow::Result;
use farder_protocol::{codec, messages::Message};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use std::collections::HashMap;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// A server registered with the relay: its QUIC connection.
pub struct RegisteredServer {
    pub conn: Connection,
}

/// All relay routing state: registered servers, live relayed clients keyed by
/// their routing handle, and the monotonic handle allocator.
pub struct RelayState {
    pub servers: RwLock<HashMap<Vec<u8>, RegisteredServer>>,
    pub clients: RwLock<HashMap<u32, Connection>>,
    pub next_handle: AtomicU32,
}

pub type SharedState = Arc<RelayState>;

/// Everything the preview proxy needs at dispatch time.
pub struct PreviewContext {
    pub cache: crate::proxy::PreviewCache,
    pub limiter: crate::limits::ConnectionLimiter,
    pub out_endpoint: Endpoint,
}

pub fn new_preview_context() -> Result<Arc<PreviewContext>> {
    Ok(Arc::new(PreviewContext {
        cache: crate::proxy::PreviewCache::new(),
        // Rate-only: effectively no concurrent cap (the connection limiter
        // already caps connections); 30 previews/min/IP.
        limiter: crate::limits::ConnectionLimiter::new(usize::MAX, 30, std::time::Duration::from_secs(60)),
        out_endpoint: crate::proxy::outbound_endpoint()?,
    }))
}

/// Everything the embed proxy needs at dispatch time.
pub struct EmbedContext {
    pub cache: crate::embed::EmbedCache,
    pub limiter: crate::limits::ConnectionLimiter,
    pub media_limiter: crate::limits::ConnectionLimiter,
    pub fetcher: std::sync::Arc<crate::embed::SafeFetcher>,
}

pub fn new_embed_context() -> Result<Arc<EmbedContext>> {
    Ok(Arc::new(EmbedContext {
        cache: crate::embed::EmbedCache::new(),
        // 30 metadata previews/min/IP (same posture as invite previews).
        limiter: crate::limits::ConnectionLimiter::new(usize::MAX, 30, std::time::Duration::from_secs(60)),
        // Separate, tighter bucket for bandwidth-heavy media fetches.
        media_limiter: crate::limits::ConnectionLimiter::new(usize::MAX, 60, std::time::Duration::from_secs(60)),
        fetcher: std::sync::Arc::new(crate::embed::SafeFetcher::new()?),
    }))
}

pub fn new_state() -> SharedState {
    Arc::new(RelayState {
        servers: RwLock::new(HashMap::new()),
        clients: RwLock::new(HashMap::new()),
        // Handle 0 is reserved (never assigned) so it can read as "no handle".
        next_handle: AtomicU32::new(1),
    })
}

/// Accept loop: spawn a handler per incoming QUIC connection, gated by the
/// abuse-control limiter (global cap + per-IP rate limit). Over-limit
/// connections are refused before the handshake.
pub async fn serve(
    endpoint: Endpoint,
    state: SharedState,
    limiter: std::sync::Arc<crate::limits::ConnectionLimiter>,
    preview: Arc<PreviewContext>,
    embed: Arc<EmbedContext>,
) -> Result<()> {
    while let Some(incoming) = endpoint.accept().await {
        let ip = incoming.remote_address().ip();
        let guard = match limiter.try_admit(ip, std::time::Instant::now()) {
            Some(g) => g,
            None => {
                warn!("refused connection from {} (over limit)", ip);
                incoming.refuse();
                continue;
            }
        };
        let state = state.clone();
        let preview = preview.clone();
        let embed = embed.clone();
        tokio::spawn(async move {
            let _guard = guard; // held for the connection's lifetime
            match incoming.await {
                Ok(conn) => {
                    if let Err(e) = handle_connection(conn, state, preview, embed).await {
                        warn!("connection error: {}", e);
                    }
                }
                Err(e) => warn!("incoming connection failed: {}", e),
            }
        });
    }
    Ok(())
}

pub async fn handle_connection(conn: Connection, state: SharedState, preview: Arc<PreviewContext>, embed: Arc<EmbedContext>) -> Result<()> {
    let remote = conn.remote_address();
    info!("new connection from {}", remote);
    // The first bi-stream carries the role-establishing message.
    let (send, mut recv) = conn.accept_bi().await?;
    let buf = read_message(&mut recv).await?;
    let msg: Message = codec::decode(&buf)?;
    match msg {
        Message::RelayRegister { server_id } => {
            handle_register(server_id, conn, send, state).await
        }
        Message::RelayConnect { destination_id } => {
            handle_connect(destination_id, conn, send, state).await
        }
        Message::ProxyInvitePreview { target, code } => {
            handle_preview(target, code, conn, send, state, preview).await
        }
        Message::ProxyLinkEmbed { url } => {
            handle_link_embed(url, conn, send, embed).await
        }
        Message::ProxyMedia { url } => {
            handle_media(url, conn, send, embed).await
        }
        _ => {
            warn!("unexpected first message from {}", remote);
            Ok(())
        }
    }
}

/// A server registers under `server_id`; hold its connection open and remove it
/// from the registry when it closes. A duplicate id replaces the previous
/// registration (server reconnect).
async fn handle_register(
    server_id: Vec<u8>,
    conn: Connection,
    mut send: SendStream,
    state: SharedState,
) -> Result<()> {
    {
        let mut map = state.servers.write().await;
        if map
            .insert(server_id.clone(), RegisteredServer { conn: conn.clone() })
            .is_some()
        {
            warn!("server id re-registered, replacing previous ({} bytes)", server_id.len());
        } else {
            info!("server registered ({} bytes id)", server_id.len());
        }
    }
    let ack = codec::encode(&Message::RelayRegistered)?;
    write_message(&mut send, &ack).await?;

    // Route datagrams the server sends back to the right relayed client.
    let route_state = state.clone();
    let route_conn = conn.clone();
    tokio::spawn(async move {
        crate::datagram::route_server_datagrams(route_conn, route_state).await;
    });

    // Keep the control connection alive; clean up on close.
    conn.closed().await;
    let mut map = state.servers.write().await;
    if let Some(existing) = map.get(&server_id) {
        // Only remove if it is still OUR connection (not a newer re-registration).
        if existing.conn.stable_id() == conn.stable_id() {
            map.remove(&server_id);
            info!("server unregistered ({} bytes id)", server_id.len());
        }
    }
    Ok(())
}

/// A client asks for `destination_id`; bridge it to the registered server, or
/// reply with an error if none is registered.
async fn handle_connect(
    destination_id: Vec<u8>,
    client_conn: Connection,
    mut send: SendStream,
    state: SharedState,
) -> Result<()> {
    let dest = {
        let map = state.servers.read().await;
        map.get(&destination_id).map(|r| r.conn.clone())
    };
    match dest {
        Some(server_conn) => {
            let ack = codec::encode(&Message::RelayConnected)?;
            write_message(&mut send, &ack).await?;

            // Assign this client a routing handle and record it.
            let handle = state.next_handle.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            state.clients.write().await.insert(handle, client_conn.clone());

            // Forward the client's voice datagrams to the server, tagged with its handle.
            let fwd_client = client_conn.clone();
            let fwd_server = server_conn.clone();
            tokio::spawn(async move {
                crate::datagram::forward_client_datagrams(fwd_client, fwd_server, handle).await;
            });

            // Bridge the client's streams (blocks until the client disconnects).
            let bridge_result = bridge_client(client_conn, server_conn, handle).await;

            // Cleanup: remove the handle.
            state.clients.write().await.remove(&handle);

            bridge_result
        }
        None => {
            let err = codec::encode(&Message::RelayError {
                reason: "destination not connected".to_string(),
            })?;
            write_message(&mut send, &err).await?;
            // Flush the reply and keep the connection alive until the client
            // has drained it; otherwise dropping `client_conn` here would tear
            // the connection down before the buffered error reaches the peer.
            let _ = send.finish();
            client_conn.closed().await;
            Ok(())
        }
    }
}

/// Answer a ProxyInvitePreview: rate-limit → cache → fetch (5s budget) →
/// reply ProxyInvitePreviewResult and let the requester drain.
async fn handle_preview(
    target: farder_protocol::messages::PreviewTarget,
    code: String,
    client_conn: Connection,
    mut send: SendStream,
    state: SharedState,
    preview: Arc<PreviewContext>,
) -> Result<()> {
    use farder_protocol::messages::PreviewOutcome;
    let ip = client_conn.remote_address().ip();
    let now = std::time::Instant::now();

    // Invite codes are 8 chars today; anything huge is abuse — it would flow
    // into the cache key and be forwarded to the target server. Uniform
    // Invalid (not Unavailable): a hostile requester learns nothing.
    let outcome = if code.len() > 256 {
        PreviewOutcome::Invalid
    } else if preview.limiter.try_admit(ip, now).is_none() {
        // Guard dropped immediately when admitted — we use it as a pure rate
        // limiter here, not a concurrency cap.
        PreviewOutcome::Unavailable
    } else {
        let key = crate::proxy::cache_key(&target, &code);
        match preview.cache.get(&key, now) {
            Some(hit) => hit,
            None => {
                let registered = match &target {
                    farder_protocol::messages::PreviewTarget::Registered { server_id } => {
                        state.servers.read().await.get(server_id).map(|r| r.conn.clone())
                    }
                    _ => None,
                };
                let fresh = tokio::time::timeout(
                    crate::proxy::PREVIEW_TIMEOUT,
                    crate::proxy::fetch_preview(&target, &code, registered, &preview.out_endpoint),
                )
                .await
                .unwrap_or(PreviewOutcome::Unavailable);
                preview.cache.put(key, fresh.clone(), std::time::Instant::now());
                fresh
            }
        }
    };

    let reply = codec::encode(&Message::ProxyInvitePreviewResult { outcome })?;
    write_message(&mut send, &reply).await?;
    // Same drain pattern as handle_connect's error path: finish and wait so the
    // buffered reply reaches the peer before the connection drops.
    let _ = send.finish();
    client_conn.closed().await;
    Ok(())
}

/// Answer a ProxyLinkEmbed: rate-limit → cache → resolve (10s budget) → reply.
async fn handle_link_embed(
    url: String,
    client_conn: Connection,
    mut send: SendStream,
    embed: Arc<EmbedContext>,
) -> Result<()> {
    use farder_protocol::messages::EmbedOutcome;
    let ip = client_conn.remote_address().ip();
    let now = std::time::Instant::now();

    let outcome = if url.len() > 2048 {
        EmbedOutcome::Unavailable
    } else if embed.limiter.try_admit(ip, now).is_none() {
        EmbedOutcome::Unavailable
    } else {
        let key = crate::embed::embed_cache_key(&url);
        match embed.cache.get(&key, now) {
            Some(hit) => hit,
            None => {
                let fresh = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    crate::embed::resolve_embed(&url, embed.fetcher.as_ref()),
                ).await.unwrap_or(EmbedOutcome::Unavailable);
                embed.cache.put(key, fresh.clone(), std::time::Instant::now());
                fresh
            }
        }
    };

    let reply = codec::encode(&Message::ProxyLinkEmbedResult { outcome })?;
    write_message(&mut send, &reply).await?;
    let _ = send.finish();
    client_conn.closed().await;
    Ok(())
}

/// Answer a ProxyMedia: rate-limit → fetch (validated, capped) → reply a
/// ProxyMediaHeader then the raw bytes length-framed, or ProxyMediaUnavailable.
async fn handle_media(
    url: String,
    client_conn: Connection,
    mut send: SendStream,
    embed: Arc<EmbedContext>,
) -> Result<()> {
    let ip = client_conn.remote_address().ip();
    let now = std::time::Instant::now();

    let result = if url.len() > 2048 || embed.media_limiter.try_admit(ip, now).is_none() {
        None
    } else {
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            crate::embed::fetch_media(embed.fetcher.client(), &url),
        ).await.ok().and_then(|r| r.ok())
    };

    match result {
        Some((content_type, bytes)) => {
            let hdr = codec::encode(&Message::ProxyMediaHeader {
                content_type,
                total_len: bytes.len() as u64,
            })?;
            write_message(&mut send, &hdr).await?;
            // Raw bytes follow, length-framed (4-byte BE len + bytes).
            send.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
            send.write_all(&bytes).await?;
        }
        None => {
            let msg = codec::encode(&Message::ProxyMediaUnavailable)?;
            write_message(&mut send, &msg).await?;
        }
    }
    let _ = send.finish();
    client_conn.closed().await;
    Ok(())
}

/// Bridge every bi-stream the client opens to a fresh bi-stream on the server's
/// control connection, copying bytes both ways. Each server-bound stream is
/// prefixed with the client's 4-byte big-endian routing handle (Phase 5b), so
/// the server can authoritatively bind the handle to the authenticated member.
/// Each writer is finished on EOF so the peer sees the end of the stream.
async fn bridge_client(client_conn: Connection, server_conn: Connection, handle: u32) -> Result<()> {
    loop {
        let (mut c_send, mut c_recv) = match client_conn.accept_bi().await {
            Ok(s) => s,
            Err(_) => break, // client connection closed
        };
        let (mut s_send, mut s_recv) = match server_conn.open_bi().await {
            Ok(s) => s,
            Err(e) => {
                warn!("could not open server stream: {}", e);
                break;
            }
        };
        // Authoritative handle stamp: written by the relay, not the client.
        if let Err(e) = s_send.write_all(&handle.to_be_bytes()).await {
            warn!("could not stamp handle on bridged stream: {}", e);
            break;
        }
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut c_recv, &mut s_send).await;
            let _ = s_send.finish();
        });
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut s_recv, &mut c_send).await;
            let _ = c_send.finish();
        });
    }
    Ok(())
}

pub async fn read_message(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        anyhow::bail!("message too large: {} bytes", len);
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(buf)
}

pub async fn write_message(send: &mut SendStream, data: &[u8]) -> Result<()> {
    let len = (data.len() as u32).to_be_bytes();
    send.write_all(&len).await?;
    send.write_all(data).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use quinn::Endpoint;
    use rustls::pki_types::ServerName;
    use std::net::SocketAddr;
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn ensure_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    // Test-only verifier: accept any relay cert (Phase 3 adds real pinning).
    #[derive(Debug)]
    struct SkipVerify;
    impl rustls::client::danger::ServerCertVerifier for SkipVerify {
        fn verify_server_cert(
            &self,
            _e: &rustls::pki_types::CertificateDer<'_>,
            _i: &[rustls::pki_types::CertificateDer<'_>],
            _n: &ServerName<'_>,
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

    /// Like `test_client_endpoint`, but with QUIC datagrams enabled -- needed to
    /// exercise the relay's datagram forward/route paths.
    fn test_client_endpoint_with_datagrams() -> Endpoint {
        let crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipVerify))
            .with_no_client_auth();
        let mut client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
        ));
        let mut transport = quinn::TransportConfig::default();
        transport.datagram_receive_buffer_size(Some(1 << 20));
        transport.datagram_send_buffer_size(1 << 20);
        client_config.transport_config(Arc::new(transport));
        let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(client_config);
        endpoint
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

    /// Start a relay on an ephemeral port; return its address and the registry.
    async fn start_relay() -> (SocketAddr, SharedState) {
        ensure_provider();
        let dir = tempfile::tempdir().unwrap();
        let ep = crate::listener::create_endpoint("127.0.0.1:0".parse().unwrap(), dir.path())
            .unwrap();
        let addr = ep.local_addr().unwrap();
        let state = new_state();
        let limiter = std::sync::Arc::new(crate::limits::ConnectionLimiter::new(
            10_000, 10_000, std::time::Duration::from_secs(60),
        ));
        let preview = new_preview_context().unwrap();
        let embed = new_embed_context().unwrap();
        tokio::spawn(serve(ep, state.clone(), limiter, preview, embed));
        std::mem::forget(dir);
        (addr, state)
    }

    /// Connect to the relay and register as a server under `id`, spawning an
    /// echo loop that mirrors any bytes on accepted bi-streams.
    async fn register_echo_server(relay: SocketAddr, id: Vec<u8>) -> Connection {
        let ep = test_client_endpoint();
        let conn = ep.connect(relay, "farder-relay").unwrap().await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let reg = codec::encode(&Message::RelayRegister { server_id: id }).unwrap();
        write_message(&mut send, &reg).await.unwrap();
        let ack = read_message(&mut recv).await.unwrap();
        let ack: Message = codec::decode(&ack).unwrap();
        assert!(matches!(ack, Message::RelayRegistered));
        let echo_conn = conn.clone();
        tokio::spawn(async move {
            while let Ok((mut s, mut r)) = echo_conn.accept_bi().await {
                tokio::spawn(async move {
                    // Discard the 4-byte handle stamp the relay prepends (Phase 5b).
                    if r.read_exact(&mut [0u8; 4]).await.is_err() {
                        return;
                    }
                    let _ = tokio::io::copy(&mut r, &mut s).await;
                    let _ = s.finish();
                });
            }
        });
        // Keep the client endpoint alive.
        std::mem::forget(ep);
        conn
    }

    /// Like `register_echo_server`, but echoes bytes UPPERCASED so its replies
    /// are distinguishable from the verbatim echo server (used to prove which
    /// registrant the relay routed to).
    async fn register_uppercase_server(relay: SocketAddr, id: Vec<u8>) -> Connection {
        let ep = test_client_endpoint();
        let conn = ep.connect(relay, "farder-relay").unwrap().await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let reg = codec::encode(&Message::RelayRegister { server_id: id }).unwrap();
        write_message(&mut send, &reg).await.unwrap();
        let ack = read_message(&mut recv).await.unwrap();
        let ack: Message = codec::decode(&ack).unwrap();
        assert!(matches!(ack, Message::RelayRegistered));
        let echo_conn = conn.clone();
        tokio::spawn(async move {
            while let Ok((mut s, mut r)) = echo_conn.accept_bi().await {
                tokio::spawn(async move {
                    // Discard the 4-byte handle stamp the relay prepends (Phase 5b).
                    if r.read_exact(&mut [0u8; 4]).await.is_err() {
                        return;
                    }
                    let data = r.read_to_end(64 * 1024).await.unwrap_or_default();
                    let upper: Vec<u8> = data.iter().map(|b| b.to_ascii_uppercase()).collect();
                    let _ = s.write_all(&upper).await;
                    let _ = s.finish();
                });
            }
        });
        std::mem::forget(ep);
        conn
    }

    /// Connect as a client and send RelayConnect; return the first-stream reply.
    async fn client_connect(relay: SocketAddr, id: Vec<u8>) -> (Connection, Message) {
        let ep = test_client_endpoint();
        let conn = ep.connect(relay, "farder-relay").unwrap().await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let msg = codec::encode(&Message::RelayConnect { destination_id: id }).unwrap();
        write_message(&mut send, &msg).await.unwrap();
        let reply = read_message(&mut recv).await.unwrap();
        let reply: Message = codec::decode(&reply).unwrap();
        std::mem::forget(ep);
        (conn, reply)
    }

    #[tokio::test]
    async fn bridges_client_to_registered_server() {
        let (relay, _conns) = start_relay().await;
        let id = vec![1u8; 16];
        let _server = register_echo_server(relay, id.clone()).await;

        let (client, reply) = client_connect(relay, id).await;
        assert!(matches!(reply, Message::RelayConnected));

        // Data flows on a NEW bi-stream the client opens; the relay bridges it.
        let (mut send, mut recv) = client.open_bi().await.unwrap();
        send.write_all(b"hello through the relay").await.unwrap();
        send.finish().unwrap();
        let echoed = recv.read_to_end(64 * 1024).await.unwrap();
        assert_eq!(echoed, b"hello through the relay");
    }

    #[tokio::test]
    async fn unknown_destination_errors() {
        let (relay, _conns) = start_relay().await;
        let (_client, reply) = client_connect(relay, vec![2u8; 16]).await;
        match reply {
            Message::RelayError { reason } => assert!(reason.contains("not connected")),
            other => panic!("expected RelayError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn registry_clears_on_server_disconnect() {
        let (relay, conns) = start_relay().await;
        let id = vec![3u8; 16];
        let server = register_echo_server(relay, id.clone()).await;
        for _ in 0..50 {
            if conns.servers.read().await.contains_key(&id) { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(conns.servers.read().await.contains_key(&id));

        server.close(0u32.into(), b"bye");
        for _ in 0..50 {
            if !conns.servers.read().await.contains_key(&id) { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!conns.servers.read().await.contains_key(&id), "registry entry must be removed on disconnect");

        let (_client, reply) = client_connect(relay, id).await;
        assert!(matches!(reply, Message::RelayError { .. }));
    }

    #[tokio::test]
    async fn reregistration_routes_to_newest_server() {
        let (relay, _conns) = start_relay().await;
        let id = vec![4u8; 16];
        // First registrant would echo UPPERCASED; second echoes verbatim and
        // must replace the first in the registry.
        let _first = register_uppercase_server(relay, id.clone()).await;
        let _second = register_echo_server(relay, id.clone()).await;

        let (client, reply) = client_connect(relay, id).await;
        assert!(matches!(reply, Message::RelayConnected));
        let (mut send, mut recv) = client.open_bi().await.unwrap();
        send.write_all(b"route me").await.unwrap();
        send.finish().unwrap();
        let echoed = tokio::time::timeout(Duration::from_secs(5), recv.read_to_end(64 * 1024))
            .await
            .expect("bridge to newest registrant timed out")
            .unwrap();
        // Verbatim (lowercase) proves the SECOND registrant served it; the first
        // would have returned "ROUTE ME".
        assert_eq!(echoed, b"route me", "relay must route to the newest registrant");
    }

    #[tokio::test]
    async fn over_cap_connection_is_refused() {
        ensure_provider();
        let dir = tempfile::tempdir().unwrap();
        let ep = crate::listener::create_endpoint("127.0.0.1:0".parse().unwrap(), dir.path()).unwrap();
        let addr = ep.local_addr().unwrap();
        let state = new_state();
        let limiter = std::sync::Arc::new(crate::limits::ConnectionLimiter::new(
            1, 10_000, std::time::Duration::from_secs(60),
        ));
        let preview = new_preview_context().unwrap();
        let embed = new_embed_context().unwrap();
        tokio::spawn(serve(ep, state, limiter, preview, embed));
        std::mem::forget(dir);

        // First connection: open it and keep it alive so it holds the only slot.
        let ep1 = test_client_endpoint();
        let c1 = ep1.connect(addr, "farder-relay").unwrap().await.unwrap();
        let _s1 = c1.open_bi().await.unwrap();
        std::mem::forget(ep1);

        // Second connection: refused by the cap. quinn's refuse() sends a
        // CONNECTION_CLOSE during the handshake, so the client's connect().await
        // resolves to an error. (A non-refused 2nd connection would instead
        // succeed and stay open, so this assertion fails iff the cap weren't
        // enforced.)
        let ep2 = test_client_endpoint();
        let attempt = ep2.connect(addr, "farder-relay").unwrap().await;
        assert!(attempt.is_err(), "connection over the cap must be refused");
        std::mem::forget(ep2);
        let _ = c1;
    }

    /// Register as a server that does NOT echo; instead it reads the 4-byte
    /// handle stamp from each bridged bi-stream the relay opens (Phase 5b) and
    /// captures any datagrams the relay forwards. Returns the connection, a
    /// receiver of handles learned from the stamp, and a datagram receiver.
    async fn register_capturing_server(
        relay: SocketAddr,
        id: Vec<u8>,
    ) -> (Connection, mpsc::UnboundedReceiver<u32>, mpsc::UnboundedReceiver<Bytes>) {
        let ep = test_client_endpoint_with_datagrams();
        let conn = ep.connect(relay, "farder-relay").unwrap().await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let reg = codec::encode(&Message::RelayRegister { server_id: id }).unwrap();
        write_message(&mut send, &reg).await.unwrap();
        let ack = read_message(&mut recv).await.unwrap();
        let ack: Message = codec::decode(&ack).unwrap();
        assert!(matches!(ack, Message::RelayRegistered));

        // Read the 4-byte handle stamp from each bridged stream the relay opens.
        let (h_tx, h_rx) = mpsc::unbounded_channel();
        let h_conn = conn.clone();
        tokio::spawn(async move {
            while let Ok((_s, mut r)) = h_conn.accept_bi().await {
                let mut hb = [0u8; 4];
                if r.read_exact(&mut hb).await.is_ok() {
                    let _ = h_tx.send(u32::from_be_bytes(hb));
                    tokio::spawn(async move { let _ = r.read_to_end(1 << 20).await; });
                }
            }
        });

        // Capture forwarded datagrams.
        let (dg_tx, dg_rx) = mpsc::unbounded_channel();
        let dg_conn = conn.clone();
        tokio::spawn(async move {
            while let Ok(b) = dg_conn.read_datagram().await {
                let _ = dg_tx.send(b);
            }
        });

        std::mem::forget(ep);
        (conn, h_rx, dg_rx)
    }

    /// Connect as a client over a datagram-enabled endpoint; return the
    /// connection (so the test can send/recv datagrams) and the first reply.
    async fn client_connect_dg(relay: SocketAddr, id: Vec<u8>) -> (Connection, Endpoint, Message) {
        let ep = test_client_endpoint_with_datagrams();
        let conn = ep.connect(relay, "farder-relay").unwrap().await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let msg = codec::encode(&Message::RelayConnect { destination_id: id }).unwrap();
        write_message(&mut send, &msg).await.unwrap();
        let reply = read_message(&mut recv).await.unwrap();
        let reply: Message = codec::decode(&reply).unwrap();
        (conn, ep, reply)
    }

    /// Receive from an mpsc receiver with a timeout, panicking on timeout.
    async fn recv_timeout<T>(rx: &mut mpsc::UnboundedReceiver<T>, what: &str) -> T {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .unwrap_or_else(|| panic!("channel closed waiting for {what}"))
    }

    #[tokio::test]
    async fn forwards_client_datagram_tagged_with_handle() {
        let (relay, _state) = start_relay().await;
        let id = vec![6u8; 16];
        let (_server, mut handle_rx, mut dg_rx) = register_capturing_server(relay, id.clone()).await;

        let (client, _client_ep, reply) = client_connect_dg(relay, id).await;
        assert!(matches!(reply, Message::RelayConnected));
        // Open one bi-stream so the relay bridges + stamps it; learn the handle from the stamp.
        let (mut bs, _br) = client.open_bi().await.unwrap();
        bs.write_all(b"x").await.unwrap();
        let handle = recv_timeout(&mut handle_rx, "handle stamp").await;

        client.send_datagram(Bytes::from_static(b"voicepacket")).unwrap();
        let got = recv_timeout(&mut dg_rx, "forwarded datagram").await;

        let mut expected = handle.to_be_bytes().to_vec();
        expected.extend_from_slice(b"voicepacket");
        assert_eq!(got.as_ref(), expected.as_slice(), "server must receive [handle][payload]");
    }

    #[tokio::test]
    async fn routes_server_datagram_to_correct_client() {
        let (relay, _state) = start_relay().await;
        let id = vec![7u8; 16];
        let (server, mut handle_rx, _dg_rx) = register_capturing_server(relay, id.clone()).await;

        // Two clients connect; open a bi-stream per client and read each handle from the stamp
        // receiver before connecting the next client so ordering stays deterministic.
        let (client1, _ep1, _r1) = client_connect_dg(relay, id.clone()).await;
        let (mut bs1, _br1) = client1.open_bi().await.unwrap();
        bs1.write_all(b"x").await.unwrap();
        let h1 = recv_timeout(&mut handle_rx, "handle stamp 1").await;

        let (client2, _ep2, _r2) = client_connect_dg(relay, id).await;
        let (mut bs2, _br2) = client2.open_bi().await.unwrap();
        bs2.write_all(b"x").await.unwrap();
        let h2 = recv_timeout(&mut handle_rx, "handle stamp 2").await;

        assert_ne!(h1, h2);

        // Server sends a datagram tagged for client 2 only.
        let mut for_c2 = h2.to_be_bytes().to_vec();
        for_c2.extend_from_slice(b"hello-two");
        server.send_datagram(Bytes::from(for_c2)).unwrap();

        // Client 2 receives the stripped payload.
        let got2 = tokio::time::timeout(Duration::from_secs(5), client2.read_datagram())
            .await
            .expect("client 2 timed out")
            .unwrap();
        assert_eq!(got2.as_ref(), b"hello-two");

        // Client 1 receives nothing within a short window (selective routing).
        let none = tokio::time::timeout(Duration::from_millis(300), client1.read_datagram()).await;
        assert!(none.is_err(), "client 1 must not receive a datagram tagged for client 2");
    }

    #[tokio::test]
    async fn unknown_handle_datagram_is_dropped() {
        let (relay, _state) = start_relay().await;
        let id = vec![8u8; 16];
        let (server, mut handle_rx, _dg_rx) = register_capturing_server(relay, id.clone()).await;

        let (client, _ep, _r) = client_connect_dg(relay, id).await;
        // Open one bi-stream so the relay bridges + stamps it; learn the handle from the stamp.
        let (mut bs, _br) = client.open_bi().await.unwrap();
        bs.write_all(b"x").await.unwrap();
        let handle = recv_timeout(&mut handle_rx, "handle stamp").await;

        // A datagram tagged with a never-assigned handle must be dropped (no panic).
        let mut bogus = 999_999u32.to_be_bytes().to_vec();
        bogus.extend_from_slice(b"nowhere");
        server.send_datagram(Bytes::from(bogus)).unwrap();

        // A subsequent valid datagram still routes - proving the loop survived.
        let mut valid = handle.to_be_bytes().to_vec();
        valid.extend_from_slice(b"real");
        server.send_datagram(Bytes::from(valid)).unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), client.read_datagram())
            .await
            .expect("valid datagram after a bogus one timed out")
            .unwrap();
        assert_eq!(got.as_ref(), b"real");
    }

    #[tokio::test]
    async fn bridged_stream_is_stamped_with_client_handle() {
        let (relay, _state) = start_relay().await;
        let id = vec![9u8; 16];
        let ep = test_client_endpoint_with_datagrams();
        let conn = ep.connect(relay, "farder-relay").unwrap().await.unwrap();
        let (mut sreg, mut rreg) = conn.open_bi().await.unwrap();
        let reg = codec::encode(&Message::RelayRegister { server_id: id.clone() }).unwrap();
        write_message(&mut sreg, &reg).await.unwrap();
        let ack = read_message(&mut rreg).await.unwrap();
        assert!(matches!(codec::decode::<Message>(&ack).unwrap(), Message::RelayRegistered));

        let server_conn = conn.clone();

        let cep = test_client_endpoint_with_datagrams();
        let cconn = cep.connect(relay, "farder-relay").unwrap().await.unwrap();
        let (mut cs, mut cr) = cconn.open_bi().await.unwrap();
        let m = codec::encode(&Message::RelayConnect { destination_id: id }).unwrap();
        write_message(&mut cs, &m).await.unwrap();
        let reply = read_message(&mut cr).await.unwrap();
        assert!(matches!(codec::decode::<Message>(&reply).unwrap(), Message::RelayConnected));
        let (mut bs, _br) = cconn.open_bi().await.unwrap();
        bs.write_all(b"after-the-stamp").await.unwrap();
        bs.finish().unwrap();

        let mut found_handle: Option<u32> = None;
        for _ in 0..3 {
            let (_s, mut r) = tokio::time::timeout(Duration::from_secs(5), server_conn.accept_bi())
                .await.expect("accept_bi timed out").unwrap();
            let mut h = [0u8; 4];
            if r.read_exact(&mut h).await.is_ok() {
                let rest = tokio::time::timeout(Duration::from_secs(3), r.read_to_end(64))
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .unwrap_or_default();
                if rest == b"after-the-stamp" {
                    found_handle = Some(u32::from_be_bytes(h));
                    break;
                }
            }
        }
        let handle = found_handle.expect("bridged stream must start with a 4-byte handle stamp");
        assert_ne!(handle, 0, "handle 0 is reserved");
        std::mem::forget(ep);
        std::mem::forget(cep);
    }

    /// A fake farder-server double that answers the preview protocol on every
    /// accepted/bridged stream: reads the 4-byte stamp + Primary role, sends a
    /// Challenge, then answers GetInvitePreview (valid code "GOOD" only).
    async fn register_preview_server(relay: SocketAddr, id: Vec<u8>) -> Connection {
        use farder_protocol::server::{ClientFrame, RelayStreamRole, ServerFrame};
        let ep = test_client_endpoint();
        let conn = ep.connect(relay, "farder-relay").unwrap().await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let reg = codec::encode(&Message::RelayRegister { server_id: id }).unwrap();
        write_message(&mut send, &reg).await.unwrap();
        let ack: Message = codec::decode(&read_message(&mut recv).await.unwrap()).unwrap();
        assert!(matches!(ack, Message::RelayRegistered));
        let serve_conn = conn.clone();
        tokio::spawn(async move {
            while let Ok((mut s, mut r)) = serve_conn.accept_bi().await {
                tokio::spawn(async move {
                    let mut stamp = [0u8; 4];
                    if r.read_exact(&mut stamp).await.is_err() { return; }
                    assert_eq!(u32::from_be_bytes(stamp), 0, "preview streams must be stamped with reserved handle 0");
                    let role: RelayStreamRole = codec::decode(&read_message(&mut r).await.unwrap()).unwrap();
                    assert!(matches!(role, RelayStreamRole::Primary));
                    let ch = codec::encode(&ServerFrame::Challenge { nonce: [7u8; 32] }).unwrap();
                    write_message(&mut s, &ch).await.unwrap();
                    let frame: ClientFrame = codec::decode(&read_message(&mut r).await.unwrap()).unwrap();
                    let answer = match frame {
                        ClientFrame::GetInvitePreview { code } if code == "GOOD" =>
                            ServerFrame::InvitePreview { server_name: "Proxied".into(), member_count: 5, online_count: 2 },
                        ClientFrame::GetInvitePreview { .. } =>
                            ServerFrame::InvitePreviewError { reason: "invalid".into() },
                        other => panic!("unexpected frame: {other:?}"),
                    };
                    write_message(&mut s, &codec::encode(&answer).unwrap()).await.unwrap();
                    let _ = s.finish();
                });
            }
        });
        std::mem::forget(ep);
        conn
    }

    async fn ask_preview(relay: SocketAddr, target: farder_protocol::messages::PreviewTarget, code: &str) -> farder_protocol::messages::PreviewOutcome {
        let ep = test_client_endpoint();
        let conn = ep.connect(relay, "farder-relay").unwrap().await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let msg = codec::encode(&Message::ProxyInvitePreview { target, code: code.to_string() }).unwrap();
        write_message(&mut send, &msg).await.unwrap();
        let reply: Message = codec::decode(&read_message(&mut recv).await.unwrap()).unwrap();
        std::mem::forget(ep);
        match reply {
            Message::ProxyInvitePreviewResult { outcome } => outcome,
            other => panic!("expected ProxyInvitePreviewResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn proxies_preview_to_registered_server() {
        use farder_protocol::messages::{PreviewOutcome, PreviewTarget};
        let (relay, _state) = start_relay().await;
        let id = vec![21u8; 16];
        let _server = register_preview_server(relay, id.clone()).await;

        match ask_preview(relay, PreviewTarget::Registered { server_id: id.clone() }, "GOOD").await {
            PreviewOutcome::Preview { server_name, member_count, online_count } => {
                assert_eq!(server_name, "Proxied");
                assert_eq!(member_count, 5);
                assert_eq!(online_count, 2);
            }
            other => panic!("expected Preview, got {other:?}"),
        }

        assert!(matches!(
            ask_preview(relay, PreviewTarget::Registered { server_id: id }, "BAD").await,
            PreviewOutcome::Invalid
        ));
    }

    #[tokio::test]
    async fn unknown_registered_target_is_unavailable() {
        use farder_protocol::messages::{PreviewOutcome, PreviewTarget};
        let (relay, _state) = start_relay().await;
        assert!(matches!(
            ask_preview(relay, PreviewTarget::Registered { server_id: vec![99u8; 16] }, "GOOD").await,
            PreviewOutcome::Unavailable
        ));
    }

    #[tokio::test]
    async fn direct_preview_refuses_private_addresses() {
        use farder_protocol::messages::{PreviewOutcome, PreviewTarget};
        let (relay, _state) = start_relay().await;
        // Loopback target: SSRF guard must refuse WITHOUT dialing (instant, no 5s timeout).
        // NOTE: the direct-target happy path is not tested with a real server here because
        // the SSRF guard correctly refuses 127.0.0.1, which is the only address a test server
        // can bind. The guard's correctness is verified by the ssrf_guard_refuses_non_global
        // unit test in proxy.rs, and the ask_server code path is exercised by the
        // proxies_preview_to_registered_server test above. Do NOT weaken the guard for tests.
        let started = std::time::Instant::now();
        let outcome = ask_preview(relay, PreviewTarget::Direct { addr: "127.0.0.1:4433".into() }, "GOOD").await;
        assert!(matches!(outcome, PreviewOutcome::Unavailable));
        assert!(started.elapsed() < Duration::from_secs(3), "refusal must be immediate, not a timeout");
    }

    #[tokio::test]
    async fn oversized_code_is_rejected_as_invalid() {
        use farder_protocol::messages::{PreviewOutcome, PreviewTarget};
        let (relay, _state) = start_relay().await;
        let id = vec![22u8; 16];
        let _server = register_preview_server(relay, id.clone()).await;
        let huge = "x".repeat(100_000);
        let started = std::time::Instant::now();
        let outcome = ask_preview(relay, PreviewTarget::Registered { server_id: id }, &huge).await;
        assert!(matches!(outcome, PreviewOutcome::Invalid));
        assert!(started.elapsed() < Duration::from_secs(3), "must reject without contacting the server");
    }

    #[tokio::test]
    async fn relay_endpoint_supports_datagrams() {
        ensure_provider();
        let dir = tempfile::tempdir().unwrap();
        let ep = crate::listener::create_endpoint("127.0.0.1:0".parse().unwrap(), dir.path()).unwrap();
        let addr = ep.local_addr().unwrap();
        std::mem::forget(dir);

        // Relay side: accept one connection, read one datagram.
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            if let Some(inc) = ep.accept().await {
                if let Ok(conn) = inc.await {
                    if let Ok(dg) = conn.read_datagram().await {
                        let _ = tx.send(dg.to_vec());
                    }
                }
            }
        });

        // Client side: connect and send a datagram. If the relay endpoint did
        // NOT advertise datagram support, send_datagram() returns UnsupportedByPeer
        // and the unwrap below panics -- which is exactly the pre-implementation failure.
        let cep = test_client_endpoint_with_datagrams();
        let conn = cep.connect(addr, "farder-relay").unwrap().await.unwrap();
        conn.send_datagram(bytes::Bytes::from_static(b"ping")).unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("relay did not receive the datagram")
            .unwrap();
        assert_eq!(got, b"ping");
        std::mem::forget(cep);
    }

    /// Spin up a QUIC server that behaves like a direct-mode farder-server:
    /// for each incoming connection it OPENS a bi-stream (server-initiated,
    /// mirroring `crates/farder-server/src/connection.rs`), sends a Challenge,
    /// reads GetInvitePreview, and answers with InvitePreview or InvitePreviewError.
    async fn start_direct_preview_server() -> SocketAddr {
        use farder_protocol::server::{ClientFrame, ServerFrame};
        ensure_provider();
        let dir = tempfile::tempdir().unwrap();
        let ep = crate::listener::create_endpoint("127.0.0.1:0".parse().unwrap(), dir.path()).unwrap();
        let addr = ep.local_addr().unwrap();
        std::mem::forget(dir);
        tokio::spawn(async move {
            while let Some(inc) = ep.accept().await {
                tokio::spawn(async move {
                    let Ok(conn) = inc.await else { return };
                    // Direct mode: the SERVER opens the primary stream.
                    let Ok((mut s, mut r)) = conn.open_bi().await else { return };
                    let ch = codec::encode(&ServerFrame::Challenge { nonce: [9u8; 32] }).unwrap();
                    write_message(&mut s, &ch).await.unwrap();
                    let Ok(buf) = read_message(&mut r).await else { return };
                    let frame: ClientFrame = codec::decode(&buf).unwrap();
                    let answer = match frame {
                        ClientFrame::GetInvitePreview { code } if code == "GOOD" =>
                            ServerFrame::InvitePreview { server_name: "DirectSrv".into(), member_count: 3, online_count: 1 },
                        ClientFrame::GetInvitePreview { .. } =>
                            ServerFrame::InvitePreviewError { reason: "invalid".into() },
                        other => panic!("unexpected frame: {other:?}"),
                    };
                    write_message(&mut s, &codec::encode(&answer).unwrap()).await.unwrap();
                    let _ = s.finish();
                    // Hold the connection open until the peer closes.
                    conn.closed().await;
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn direct_preview_exchange_speaks_server_initiated_stream() {
        use farder_protocol::messages::PreviewOutcome;
        let addr = start_direct_preview_server().await;
        let ep = crate::proxy::outbound_endpoint().unwrap();
        let conn = ep.connect(addr, "farder-server").unwrap().await.unwrap();
        match crate::proxy::preview_over_direct_conn(&conn, "GOOD").await.unwrap() {
            PreviewOutcome::Preview { server_name, member_count, online_count } => {
                assert_eq!(server_name, "DirectSrv");
                assert_eq!(member_count, 3);
                assert_eq!(online_count, 1);
            }
            other => panic!("expected Preview, got {other:?}"),
        }
        conn.close(0u32.into(), b"done");
        let conn2 = ep.connect(addr, "farder-server").unwrap().await.unwrap();
        assert!(matches!(
            crate::proxy::preview_over_direct_conn(&conn2, "BAD").await.unwrap(),
            PreviewOutcome::Invalid
        ));
        conn2.close(0u32.into(), b"done");
        std::mem::forget(ep);
    }
}
