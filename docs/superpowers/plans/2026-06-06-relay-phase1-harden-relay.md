# Relay Phase 1 — Harden the Relay — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn `crates/farder-relay` from a sketch into a working, tested rendezvous server: a server can register under an id, a client can connect by that id, and bytes bridge correctly both ways — proven by integration tests over real QUIC.

**Architecture:** Add `RelayRegister`/`RelayRegistered` to the protocol. The relay keeps a registry of `server_id -> Connection`. A registering server's connection is held open as a control channel; when a client connects with a matching id, the relay bridges each client bi-stream to a fresh bi-stream it opens on the server's control connection (blind byte copy both ways). The relay's self-signed cert becomes persistent so a later phase can pin it.

**Tech Stack:** Rust, quinn 0.11, rustls 0.23 (ring), rcgen 0.13, tokio, rmp_serde.

**Spec:** `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md` (Phase 1 section)

**Scope guard:** Phase 1 touches ONLY `crates/farder-protocol` and `crates/farder-relay`. NO server (`crates/farder-server`) or client (`client/src-tauri`) changes — those are Phases 2–3. No voice/datagram forwarding. No invite directory. No client-side cert pinning (only the relay cert becomes stable).

---

## File Structure

- `crates/farder-protocol/src/messages.rs` — add `RelayRegister { server_id }` and `RelayRegistered` variants + codec tests.
- `crates/farder-relay/src/config.rs` — add a `--data-dir` arg (where the stable cert lives).
- `crates/farder-relay/src/listener.rs` — `load_or_generate_cert` (persistent cert) + `create_endpoint(bind, data_dir)`.
- `crates/farder-relay/src/router.rs` — registry + register/connect/bridge rewrite, `serve()`, and the integration tests.
- `crates/farder-relay/src/main.rs` — install the rustls crypto provider; pass `data_dir`; call `serve()`.
- `crates/farder-relay/Cargo.toml` — add `tempfile` dev-dependency.

---

## Task 1: Protocol — add relay registration messages

**Files:**
- Modify: `crates/farder-protocol/src/messages.rs` (the `Message` enum near line 5; tests module near the bottom)

- [ ] **Step 1: Add a failing codec test.** In the `#[cfg(test)] mod tests` block of `crates/farder-protocol/src/messages.rs`, add:

```rust
    #[test]
    fn test_roundtrip_relay_register() {
        let msg = Message::RelayRegister { server_id: vec![9u8, 8, 7, 6] };
        let encoded = codec::encode(&msg).expect("encode failed");
        let decoded: Message = codec::decode(&encoded).expect("decode failed");
        match decoded {
            Message::RelayRegister { server_id } => assert_eq!(server_id, vec![9u8, 8, 7, 6]),
            other => panic!("expected RelayRegister, got {other:?}"),
        }
    }

    #[test]
    fn test_roundtrip_relay_registered() {
        let encoded = codec::encode(&Message::RelayRegistered).expect("encode failed");
        let decoded: Message = codec::decode(&encoded).expect("decode failed");
        assert!(matches!(decoded, Message::RelayRegistered));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd ~/farder && cargo test -p farder-protocol relay 2>&1 | tail -15`
Expected: compile error — `RelayRegister`/`RelayRegistered` are not variants of `Message`.

- [ ] **Step 3: Add the variants.** In `crates/farder-protocol/src/messages.rs`, add the two variants to the `Message` enum, right after `RelayError { reason: String },`:

```rust
    RelayRegister { server_id: Vec<u8> },
    RelayRegistered,
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd ~/farder && cargo test -p farder-protocol relay 2>&1 | tail -15`
Expected: PASS, including the two new tests and the existing `test_roundtrip_relay_connect`.

- [ ] **Step 5: Commit**

```bash
cd ~/farder && git add crates/farder-protocol/src/messages.rs && \
git commit -m "protocol: add RelayRegister/RelayRegistered relay messages"
```

---

## Task 2: Persistent relay certificate

**Files:**
- Modify: `crates/farder-relay/Cargo.toml` (dev-dependency)
- Modify: `crates/farder-relay/src/config.rs` (add `--data-dir`)
- Modify: `crates/farder-relay/src/listener.rs` (persistent cert)
- Modify: `crates/farder-relay/src/main.rs` (install provider, pass data_dir)

Background: today `create_endpoint` calls `rcgen::generate_simple_self_signed(...)` on every boot, so the relay's identity changes each run and a client could never pin it. We make the cert load-or-generate from disk so it is stable. Also: the relay binary currently never installs a rustls crypto provider — a latent bug we fix here so it can actually run.

- [ ] **Step 1: Add the dev-dependency.** In `crates/farder-relay/Cargo.toml`, under the existing `[dev-dependencies]` line, add:

```toml
tempfile = "3"
```

- [ ] **Step 2: Add the `--data-dir` arg.** In `crates/farder-relay/src/config.rs`, add a field to `Config` (inside the struct, after `max_connections`):

```rust
    #[arg(long, default_value = "./relay-data")]
    pub data_dir: std::path::PathBuf,
```

- [ ] **Step 3: Write the failing cert-stability test.** Append to `crates/farder-relay/src/listener.rs` a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cert_persists_across_calls_and_differs_per_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (c1, _k1) = load_or_generate_cert(dir.path()).unwrap();
        let (c2, _k2) = load_or_generate_cert(dir.path()).unwrap();
        assert_eq!(c1.as_ref(), c2.as_ref(), "cert must persist across calls in the same dir");

        let dir2 = tempfile::tempdir().unwrap();
        let (c3, _k3) = load_or_generate_cert(dir2.path()).unwrap();
        assert_ne!(c1.as_ref(), c3.as_ref(), "a fresh dir must get its own cert");
    }
}
```

- [ ] **Step 4: Run it to verify it fails**

Run: `cd ~/farder && cargo test -p farder-relay cert_persists 2>&1 | tail -15`
Expected: compile error — `load_or_generate_cert` does not exist.

- [ ] **Step 5: Implement the persistent cert.** Replace the body of `crates/farder-relay/src/listener.rs` ABOVE the test module with:

```rust
use anyhow::Result;
use quinn::Endpoint;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tracing::info;

/// Load the relay's TLS cert+key from `<data_dir>/relay_cert.der` and
/// `relay_key.der`, generating and persisting a self-signed pair on first run.
/// Persisting it gives the relay a stable identity a client can later pin.
fn load_or_generate_cert(
    data_dir: &Path,
) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    std::fs::create_dir_all(data_dir)?;
    let cert_path = data_dir.join("relay_cert.der");
    let key_path = data_dir.join("relay_key.der");

    if cert_path.exists() && key_path.exists() {
        let cert = std::fs::read(&cert_path)?;
        let key = std::fs::read(&key_path)?;
        let key = PrivateKeyDer::try_from(key).map_err(|e| anyhow::anyhow!("key parse: {}", e))?;
        return Ok((CertificateDer::from(cert), key));
    }

    let certified = rcgen::generate_simple_self_signed(vec!["farder-relay".to_string()])?;
    let cert_der = certified.cert.der().to_vec();
    let key_der = certified.key_pair.serialize_der();
    std::fs::write(&cert_path, &cert_der)?;
    std::fs::write(&key_path, &key_der)?;
    let key = PrivateKeyDer::try_from(key_der).map_err(|e| anyhow::anyhow!("key parse: {}", e))?;
    Ok((CertificateDer::from(cert_der), key))
}

pub fn create_endpoint(bind_addr: SocketAddr, data_dir: &Path) -> Result<Endpoint> {
    let (cert_der, key_der) = load_or_generate_cert(data_dir)?;
    let server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
    ));
    let endpoint = Endpoint::server(server_config, bind_addr)?;
    info!("Relay listening on {}", bind_addr);
    Ok(endpoint)
}
```

- [ ] **Step 6: Install the crypto provider and pass `data_dir` in `main.rs`.** In `crates/farder-relay/src/main.rs`, at the very start of `main()` (before anything else), add the provider install; and change the `create_endpoint` call to pass the data dir. The relevant lines become:

```rust
#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "farder_relay=info".into()),
        )
        .init();
    let config = Config::parse();
    info!("Starting Farder Relay v{}", env!("CARGO_PKG_VERSION"));
    let endpoint = listener::create_endpoint(config.bind, &config.data_dir)?;
```

(Leave the rest of `main()` unchanged for now — the accept loop is rewritten in Task 3. `rustls` is already a dependency of the crate, so no Cargo change is needed for the provider install.)

- [ ] **Step 7: Run the cert test and build the crate**

Run: `cd ~/farder && cargo test -p farder-relay cert_persists 2>&1 | tail -8 && cargo build -p farder-relay 2>&1 | tail -4`
Expected: the cert test PASSES; the crate builds (warnings OK).

- [ ] **Step 8: Commit**

```bash
cd ~/farder && git add crates/farder-relay/Cargo.toml crates/farder-relay/src/config.rs crates/farder-relay/src/listener.rs crates/farder-relay/src/main.rs && \
git commit -m "relay: persistent self-signed cert + install crypto provider"
```

---

## Task 3: Registry, register/connect/bridge, and integration tests

**Files:**
- Modify: `crates/farder-relay/src/router.rs` (rewrite the connection handling + add `serve()` + tests)
- Modify: `crates/farder-relay/src/main.rs` (call `serve()` instead of the inline accept loop)

This is the core. The relay must:
- treat the first message on a new connection as either `RelayRegister` (a server) or `RelayConnect` (a client);
- for a server: store `server_id -> Connection`, reply `RelayRegistered`, hold the connection open, and remove it from the registry when it closes;
- for a client: look up the id; if present reply `RelayConnected` and bridge, else reply `RelayError`;
- bridge per client bi-stream to a fresh bi-stream opened on the server's connection, copying bytes both ways and finishing each writer on EOF.

- [ ] **Step 1: Add `serve()` and rewrite the handlers.** Replace the contents of `crates/farder-relay/src/router.rs` from the top of the file down to (but NOT including) the existing `read_message`/`write_message` functions with the following. Keep `read_message` and `write_message` exactly as they are at the bottom of the file.

```rust
use anyhow::Result;
use farder_protocol::{codec, messages::Message};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

pub type ConnectionMap = Arc<RwLock<HashMap<Vec<u8>, Connection>>>;

pub fn new_connection_map() -> ConnectionMap {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Accept loop: spawn a handler per incoming QUIC connection.
pub async fn serve(endpoint: Endpoint, connections: ConnectionMap) -> Result<()> {
    while let Some(incoming) = endpoint.accept().await {
        let connections = connections.clone();
        tokio::spawn(async move {
            match incoming.await {
                Ok(conn) => {
                    if let Err(e) = handle_connection(conn, connections).await {
                        warn!("connection error: {}", e);
                    }
                }
                Err(e) => warn!("incoming connection failed: {}", e),
            }
        });
    }
    Ok(())
}

pub async fn handle_connection(conn: Connection, connections: ConnectionMap) -> Result<()> {
    let remote = conn.remote_address();
    info!("new connection from {}", remote);
    // The first bi-stream carries the role-establishing message.
    let (mut send, mut recv) = conn.accept_bi().await?;
    let buf = read_message(&mut recv).await?;
    let msg: Message = codec::decode(&buf)?;
    match msg {
        Message::RelayRegister { server_id } => {
            handle_register(server_id, conn, send, connections).await
        }
        Message::RelayConnect { destination_id } => {
            handle_connect(destination_id, conn, send, connections).await
        }
        _ => {
            warn!("unexpected first message from {}", remote);
            Ok(())
        }
    }
}

/// A server registers under `server_id`; hold its connection open as a control
/// channel and remove it from the registry when it closes. A duplicate id
/// replaces the previous registration (server reconnect).
async fn handle_register(
    server_id: Vec<u8>,
    conn: Connection,
    mut send: SendStream,
    connections: ConnectionMap,
) -> Result<()> {
    {
        let mut map = connections.write().await;
        if map.insert(server_id.clone(), conn.clone()).is_some() {
            warn!("server id re-registered, replacing previous ({} bytes)", server_id.len());
        } else {
            info!("server registered ({} bytes id)", server_id.len());
        }
    }
    let ack = codec::encode(&Message::RelayRegistered)?;
    write_message(&mut send, &ack).await?;

    // Keep the control connection alive; clean up on close.
    conn.closed().await;
    let mut map = connections.write().await;
    if let Some(existing) = map.get(&server_id) {
        // Only remove if it is still OUR connection (not a newer re-registration).
        if existing.stable_id() == conn.stable_id() {
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
    connections: ConnectionMap,
) -> Result<()> {
    let dest = {
        let map = connections.read().await;
        map.get(&destination_id).cloned()
    };
    match dest {
        Some(server_conn) => {
            let ack = codec::encode(&Message::RelayConnected)?;
            write_message(&mut send, &ack).await?;
            bridge_client(client_conn, server_conn).await
        }
        None => {
            let err = codec::encode(&Message::RelayError {
                reason: "destination not connected".to_string(),
            })?;
            write_message(&mut send, &err).await?;
            Ok(())
        }
    }
}

/// Bridge every bi-stream the client opens to a fresh bi-stream on the server's
/// control connection, copying bytes both ways. Each writer is finished on EOF
/// so the peer sees the end of the stream.
async fn bridge_client(client_conn: Connection, server_conn: Connection) -> Result<()> {
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
```

- [ ] **Step 2: Point `main.rs` at `serve()`.** In `crates/farder-relay/src/main.rs`, replace the inline accept loop:

```rust
    let connections = router::new_connection_map();
    while let Some(incoming) = endpoint.accept().await {
        let conn = incoming.await?;
        let connections = connections.clone();
        tokio::spawn(async move {
            if let Err(e) = router::handle_connection(conn, connections).await {
                tracing::warn!("Connection error: {}", e);
            }
        });
    }
    Ok(())
```

with:

```rust
    let connections = router::new_connection_map();
    router::serve(endpoint, connections).await?;
    Ok(())
```

- [ ] **Step 3: Add the integration test module.** Append to `crates/farder-relay/src/router.rs` (after `write_message`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use quinn::Endpoint;
    use rustls::pki_types::ServerName;
    use std::net::SocketAddr;
    use std::time::Duration;

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
    async fn start_relay() -> (SocketAddr, ConnectionMap) {
        ensure_provider();
        let dir = tempfile::tempdir().unwrap();
        let ep = crate::listener::create_endpoint("127.0.0.1:0".parse().unwrap(), dir.path())
            .unwrap();
        let addr = ep.local_addr().unwrap();
        let conns = new_connection_map();
        tokio::spawn(serve(ep, conns.clone()));
        // Keep the tempdir alive for the test process lifetime.
        std::mem::forget(dir);
        (addr, conns)
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
                    let _ = tokio::io::copy(&mut r, &mut s).await;
                    let _ = s.finish();
                });
            }
        });
        // Keep the client endpoint alive.
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
        // Wait until the registry actually has the entry.
        for _ in 0..50 {
            if conns.read().await.contains_key(&id) { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(conns.read().await.contains_key(&id));

        server.close(0u32.into(), b"bye");
        // Wait until cleanup removes it.
        for _ in 0..50 {
            if !conns.read().await.contains_key(&id) { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!conns.read().await.contains_key(&id), "registry entry must be removed on disconnect");

        let (_client, reply) = client_connect(relay, id).await;
        assert!(matches!(reply, Message::RelayError { .. }));
    }

    #[tokio::test]
    async fn reregistration_routes_to_newest_server() {
        let (relay, _conns) = start_relay().await;
        let id = vec![4u8; 16];
        let _first = register_echo_server(relay, id.clone()).await;
        let _second = register_echo_server(relay, id.clone()).await; // replaces first

        let (client, reply) = client_connect(relay, id).await;
        assert!(matches!(reply, Message::RelayConnected));
        let (mut send, mut recv) = client.open_bi().await.unwrap();
        send.write_all(b"route me").await.unwrap();
        send.finish().unwrap();
        // If routed to a non-echoing server the read would hang, so bound it.
        let echoed = tokio::time::timeout(Duration::from_secs(5), recv.read_to_end(64 * 1024))
            .await
            .expect("bridge to newest registrant timed out")
            .unwrap();
        assert_eq!(echoed, b"route me");
    }
}
```

- [ ] **Step 4: Run the integration tests**

Run: `cd ~/farder && cargo test -p farder-relay 2>&1 | tail -20`
Expected: all relay tests PASS — `cert_persists...`, `bridges_client_to_registered_server`, `unknown_destination_errors`, `registry_clears_on_server_disconnect`, `reregistration_routes_to_newest_server`.

- [ ] **Step 5: Build the binary to confirm `main.rs` still compiles**

Run: `cd ~/farder && cargo build -p farder-relay 2>&1 | tail -4`
Expected: builds, no errors.

- [ ] **Step 6: Commit**

```bash
cd ~/farder && git add crates/farder-relay/src/router.rs crates/farder-relay/src/main.rs && \
git commit -m "relay: registry + register/connect/bridge rewrite + integration tests"
```

---

## Final verification

- [ ] **Whole workspace still green:**

Run: `cd ~/farder && cargo test --workspace 2>&1 | tail -25`
Expected: all pass, including the new `farder-protocol` relay codec tests and the `farder-relay` tests. No regressions elsewhere.

- [ ] **Docs:** update `docs/modules/` if a relay module doc exists; if not, this is internal infrastructure with no public Tauri/crate surface change yet (the relay's user-facing surface arrives in Phases 2–4), so note in the commit that module docs for the relay follow when it is wired in. Update the Phase-1 checkbox/status in `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md` to mark Phase 1 done.

- [ ] **Finish the branch:** use superpowers:finishing-a-development-branch.

## Notes for the implementer

- This phase is fully testable in this environment (no GUI). The integration tests spin up real QUIC endpoints on `127.0.0.1` ephemeral ports — they are the verification; there is no UNVERIFIED GUI gap here.
- `std::mem::forget` on the tempdir/endpoints in the test helpers is deliberate: it keeps them alive for the test process without threading ownership back through every helper. Tests are short-lived processes, so the leak is harmless.
- Do NOT touch `crates/farder-server` or `client/src-tauri` — wiring the server and client to actually use the relay is Phases 2 and 3.
- `Config.max_connections` is left unenforced in Phase 1 (it stays exactly as today, unused). Relay abuse controls — connection limits, rate limiting, authenticating who may register — are explicitly deferred per the spec's "Out of scope" note and revisited before a public default relay is deployed. Do not add enforcement here.
