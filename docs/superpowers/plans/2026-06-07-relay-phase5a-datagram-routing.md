# Relay Phase 5a — Datagram Routing Core — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Teach the Farder relay to forward and route voice datagrams between a server and its relayed clients using a per-client routing handle, without breaking existing (Phase-2) relay traffic.

**Architecture:** The relay gains (a) datagrams enabled on its QUIC endpoint, (b) a per-client `u32` handle assigned at `RelayConnect`, (c) a reliable relay→server control stream announcing `RelayClientConnected/Disconnected { handle }`, (d) a forward path that tags each client→server datagram with the source handle, and (e) a route path that delivers each server→client datagram to the client named by its handle prefix. The relay forwards encrypted media bytes blind. A server that doesn't understand handles/datagrams keeps working (the control stream errors-and-drops per-stream; unread datagrams are dropped).

**Tech Stack:** Rust, quinn 0.11 (QUIC + datagrams), `bytes::Bytes`, tokio, `farder-protocol` (`codec`/`Message`).

**Spec:** `docs/superpowers/specs/2026-06-07-relay-phase5a-datagram-routing-design.md`

---

## Context for the implementer

- The relay crate (`crates/farder-relay`) is a **binary** crate (`main.rs`, no `lib.rs`). All tests are `#[cfg(test)] mod tests` **inside** the source files — there is NO `tests/` directory and you cannot add external integration tests (they can't import a bin crate). Put new tests in the relevant source file's test module.
- `router.rs` already has a rich `#[cfg(test)] mod tests` with helpers you will reuse and extend: `ensure_provider()`, `SkipVerify`, `test_client_endpoint()`, `start_relay()`, `register_echo_server()`, `client_connect()`. Read it before starting.
- quinn datagram API: `conn.read_datagram().await -> Result<bytes::Bytes>` (errors when the connection closes); `conn.send_datagram(b: bytes::Bytes) -> Result<(), SendDatagramError>` (errors if the peer didn't advertise datagram support — treat as best-effort, ignore the error). Datagrams require BOTH endpoints to set datagram buffer sizes on their `TransportConfig`.
- The server side that must keep working: `crates/farder-server/src/relay.rs` spawns a task per accepted stream and logs+drops per-stream errors (line ~104-108), so an unexpected control stream is harmless. The Phase-2 server never reads datagrams on its relay connection.
- Run the whole workspace test suite from `/home/deez/farder` with `cargo test --workspace`. Run only the relay crate's tests with `cargo test -p farder-relay`.

---

## File structure

- `crates/farder-protocol/src/messages.rs` — add `RelayClientConnected`/`RelayClientDisconnected` variants + round-trip tests.
- `crates/farder-relay/Cargo.toml` — add `bytes = "1"` dependency.
- `crates/farder-relay/src/listener.rs` — enable datagrams on the relay endpoint.
- `crates/farder-relay/src/router.rs` — `RelayState`/`RegisteredServer`/`SharedState`; reshape `serve`/`handle_connection`/`handle_register`/`handle_connect`; open + hold the per-server control stream; assign handles; announce connect/disconnect; spawn the forward + route datagram tasks. New tests in its `mod tests`.
- `crates/farder-relay/src/datagram.rs` *(new)* — `forward_client_datagrams` + `route_server_datagrams` loop helpers.
- `crates/farder-relay/src/main.rs` — `mod datagram;` + `router::new_state()`.
- `docs/modules/relay.md` *(new)* — relay module doc covering the datagram-routing surface.

---

## Task 1: Protocol — handle announcement messages

**Files:**
- Modify: `crates/farder-protocol/src/messages.rs`

- [ ] **Step 1: Write the failing tests**

Add these two tests inside `crates/farder-protocol/src/messages.rs`'s `#[cfg(test)] mod tests` (after `test_roundtrip_relay_registered`):

```rust
    #[test]
    fn test_roundtrip_relay_client_connected() {
        let encoded = codec::encode(&Message::RelayClientConnected { handle: 42 }).expect("encode failed");
        let decoded: Message = codec::decode(&encoded).expect("decode failed");
        match decoded {
            Message::RelayClientConnected { handle } => assert_eq!(handle, 42),
            other => panic!("expected RelayClientConnected, got {other:?}"),
        }
    }

    #[test]
    fn test_roundtrip_relay_client_disconnected() {
        let encoded = codec::encode(&Message::RelayClientDisconnected { handle: 7 }).expect("encode failed");
        let decoded: Message = codec::decode(&encoded).expect("decode failed");
        match decoded {
            Message::RelayClientDisconnected { handle } => assert_eq!(handle, 7),
            other => panic!("expected RelayClientDisconnected, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p farder-protocol test_roundtrip_relay_client`
Expected: FAIL to compile — `no variant named RelayClientConnected`.

- [ ] **Step 3: Add the variants**

In `crates/farder-protocol/src/messages.rs`, add to the `Message` enum (after the `RelayRegistered` line):

```rust
    RelayClientConnected { handle: u32 },
    RelayClientDisconnected { handle: u32 },
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p farder-protocol test_roundtrip_relay_client`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/farder-protocol/src/messages.rs
git commit -m "Add relay handle-announcement protocol messages (Phase 5a)"
```

---

## Task 2: Enable datagrams on the relay endpoint

**Files:**
- Modify: `crates/farder-relay/Cargo.toml`
- Modify: `crates/farder-relay/src/listener.rs:53-59` (transport config)
- Test: new test + helper in `crates/farder-relay/src/router.rs` `mod tests`

- [ ] **Step 1: Add the `bytes` dependency**

In `crates/farder-relay/Cargo.toml`, under `[dependencies]` (after the `rcgen = "0.13"` line):

```toml
bytes = "1"
```

- [ ] **Step 2: Add a datagram-enabled client endpoint helper to the test module**

In `crates/farder-relay/src/router.rs`, inside `#[cfg(test)] mod tests`, add this helper next to `test_client_endpoint`:

```rust
    /// Like `test_client_endpoint`, but with QUIC datagrams enabled — needed to
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
```

- [ ] **Step 3: Write the failing test**

In the same `mod tests`, add:

```rust
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
        // and the unwrap below panics — which is exactly the pre-implementation failure.
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
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `cargo test -p farder-relay relay_endpoint_supports_datagrams`
Expected: FAIL — `send_datagram(...).unwrap()` panics with `UnsupportedByPeer` (relay endpoint has no datagram support yet).

- [ ] **Step 5: Enable datagrams on the endpoint**

In `crates/farder-relay/src/listener.rs`, in `create_endpoint`, after the existing `transport.max_idle_timeout(...)` block (before `server_config.transport_config(...)` at line 59), add:

```rust
    transport.datagram_receive_buffer_size(Some(1 << 20));
    transport.datagram_send_buffer_size(1 << 20);
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p farder-relay relay_endpoint_supports_datagrams`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/farder-relay/Cargo.toml crates/farder-relay/src/listener.rs crates/farder-relay/src/router.rs
git commit -m "Enable QUIC datagrams on the relay endpoint (Phase 5a)"
```

---

## Task 3: Reshape relay state (RelayState + RegisteredServer + control stream)

This is a structural refactor: introduce `RelayState` (servers + clients + handle counter), make each registered server carry a relay→server control stream, and thread the new shared state through. **No handle/datagram behavior yet** — just the scaffolding, with all existing tests still passing (this is the first backward-compat checkpoint: the echo-server doubles must tolerate the relay opening a control stream to them).

**Files:**
- Modify: `crates/farder-relay/src/router.rs` (state types, `serve`, `handle_connection`, `handle_register`, `handle_connect`, and the test helpers `start_relay`/`registry_clears_on_server_disconnect`)
- Modify: `crates/farder-relay/src/main.rs:28`

- [ ] **Step 1: Replace the state types and constructor**

In `crates/farder-relay/src/router.rs`, replace the top-of-file imports and the `ConnectionMap`/`new_connection_map` definitions.

Replace lines 1-13:

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
```

with:

```rust
use anyhow::Result;
use farder_protocol::{codec, messages::Message};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use std::collections::HashMap;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

/// A server registered with the relay: its control connection plus the
/// relay->server control stream used to announce client handles (Phase 5a).
pub struct RegisteredServer {
    pub conn: Connection,
    pub control: Arc<Mutex<SendStream>>,
}

/// All relay routing state: registered servers, live relayed clients keyed by
/// their routing handle, and the monotonic handle allocator.
pub struct RelayState {
    pub servers: RwLock<HashMap<Vec<u8>, RegisteredServer>>,
    pub clients: RwLock<HashMap<u32, Connection>>,
    pub next_handle: AtomicU32,
}

pub type SharedState = Arc<RelayState>;

pub fn new_state() -> SharedState {
    Arc::new(RelayState {
        servers: RwLock::new(HashMap::new()),
        clients: RwLock::new(HashMap::new()),
        // Handle 0 is reserved (never assigned) so it can read as "no handle".
        next_handle: AtomicU32::new(1),
    })
}
```

- [ ] **Step 2: Update `serve` and `handle_connection` signatures**

In `serve` (currently line 18), change the parameter type and the `.clone()`:

```rust
pub async fn serve(
    endpoint: Endpoint,
    state: SharedState,
    limiter: std::sync::Arc<crate::limits::ConnectionLimiter>,
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
        tokio::spawn(async move {
            let _guard = guard; // held for the connection's lifetime
            match incoming.await {
                Ok(conn) => {
                    if let Err(e) = handle_connection(conn, state).await {
                        warn!("connection error: {}", e);
                    }
                }
                Err(e) => warn!("incoming connection failed: {}", e),
            }
        });
    }
    Ok(())
}
```

And `handle_connection`:

```rust
pub async fn handle_connection(conn: Connection, state: SharedState) -> Result<()> {
    let remote = conn.remote_address();
    info!("new connection from {}", remote);
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
        _ => {
            warn!("unexpected first message from {}", remote);
            Ok(())
        }
    }
}
```

- [ ] **Step 3: Update `handle_register` to open + hold the control stream**

Replace `handle_register` (currently lines 73-101) with:

```rust
/// A server registers under `server_id`; open a relay->server control stream
/// (used to announce client handles in Phase 5a), hold its connection open, and
/// remove it from the registry when it closes. A duplicate id replaces the
/// previous registration (server reconnect).
async fn handle_register(
    server_id: Vec<u8>,
    conn: Connection,
    mut send: SendStream,
    state: SharedState,
) -> Result<()> {
    // Dedicated relay->server control stream for handle announcements.
    let (control_send, _control_recv) = conn.open_bi().await?;
    let control = Arc::new(Mutex::new(control_send));
    {
        let mut map = state.servers.write().await;
        if map
            .insert(server_id.clone(), RegisteredServer { conn: conn.clone(), control })
            .is_some()
        {
            warn!("server id re-registered, replacing previous ({} bytes)", server_id.len());
        } else {
            info!("server registered ({} bytes id)", server_id.len());
        }
    }
    let ack = codec::encode(&Message::RelayRegistered)?;
    write_message(&mut send, &ack).await?;

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
```

- [ ] **Step 4: Update `handle_connect` to read from the new registry**

Replace the lookup at the top of `handle_connect` (currently lines 111-114) so it reads the `RegisteredServer`:

```rust
    let dest = {
        let map = state.servers.read().await;
        map.get(&destination_id).map(|r| r.conn.clone())
    };
```

Change the function signature's last parameter from `connections: ConnectionMap` to `state: SharedState`. The rest of `handle_connect` (the `Some(server_conn) => { ... bridge_client(...) }` and `None => { ... }` arms) is unchanged in this task.

- [ ] **Step 5: Update `main.rs`**

In `crates/farder-relay/src/main.rs`, change line 28:

```rust
    let connections = router::new_state();
```

(The variable is still passed as `router::serve(endpoint, connections, limiter)`.)

- [ ] **Step 6: Update the existing test helpers to the new state type**

In `router.rs` `mod tests`:

In `start_relay` (currently line 246), change the return type and constructor:

```rust
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
        tokio::spawn(serve(ep, state.clone(), limiter));
        std::mem::forget(dir);
        (addr, state)
    }
```

In `over_cap_connection_is_refused` (currently line 402), change `let conns = new_connection_map();` to `let state = new_state();` and the spawn to `tokio::spawn(serve(ep, state, limiter));`.

In `registry_clears_on_server_disconnect` (currently line 354), update the three registry reads from `conns.read().await.contains_key(&id)` to `conns.servers.read().await.contains_key(&id)`. The binding from `start_relay` is `let (relay, conns) = start_relay().await;` — keep the name `conns` (now a `SharedState`); only the access path changes:

```rust
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
```

The other tests (`bridges_client_to_registered_server`, `unknown_destination_errors`, `reregistration_routes_to_newest_server`) bind `let (relay, _conns) = start_relay().await;` and don't touch the registry internals — they need no change beyond compiling against the new type.

- [ ] **Step 7: Run the relay tests to verify the refactor is green (backward-compat checkpoint)**

Run: `cargo test -p farder-relay`
Expected: PASS — all existing tests (`bridges_client_to_registered_server`, `unknown_destination_errors`, `registry_clears_on_server_disconnect`, `reregistration_routes_to_newest_server`, `over_cap_connection_is_refused`, `relay_endpoint_supports_datagrams`, `cert_persists_...`) pass. This proves the echo-server doubles tolerate the relay opening a control stream to them.

- [ ] **Step 8: Commit**

```bash
git add crates/farder-relay/src/router.rs crates/farder-relay/src/main.rs
git commit -m "Reshape relay state into RelayState with per-server control stream (Phase 5a)"
```

---

## Task 4: Assign handles and announce connect/disconnect

Give each relayed client a `u32` handle, record it in `state.clients`, and announce `RelayClientConnected`/`RelayClientDisconnected` to the destination server over its control stream.

**Files:**
- Modify: `crates/farder-relay/src/router.rs` (`handle_connect` + new test helper + new test)

- [ ] **Step 1: Write the failing test**

In `router.rs` `mod tests`, add a server double that captures control-stream messages, and a datagram-enabled client connector:

```rust
    use bytes::Bytes;
    use tokio::sync::mpsc;

    /// Register as a server that does NOT echo; instead it captures the relay's
    /// control-stream announcements (the relay opens that stream to us) and any
    /// datagrams the relay forwards. Returns the connection plus receivers.
    async fn register_capturing_server(
        relay: SocketAddr,
        id: Vec<u8>,
    ) -> (Connection, mpsc::UnboundedReceiver<Message>, mpsc::UnboundedReceiver<Bytes>) {
        let ep = test_client_endpoint_with_datagrams();
        let conn = ep.connect(relay, "farder-relay").unwrap().await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let reg = codec::encode(&Message::RelayRegister { server_id: id }).unwrap();
        write_message(&mut send, &reg).await.unwrap();
        let ack = read_message(&mut recv).await.unwrap();
        let ack: Message = codec::decode(&ack).unwrap();
        assert!(matches!(ack, Message::RelayRegistered));

        // The relay opens ONE control stream to us; read framed control messages.
        let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
        let ctrl_conn = conn.clone();
        tokio::spawn(async move {
            if let Ok((_s, mut r)) = ctrl_conn.accept_bi().await {
                while let Ok(buf) = read_message(&mut r).await {
                    if let Ok(m) = codec::decode::<Message>(&buf) {
                        let _ = ctrl_tx.send(m);
                    }
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
        (conn, ctrl_rx, dg_rx)
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
    async fn announces_client_connect_and_disconnect() {
        let (relay, _state) = start_relay().await;
        let id = vec![5u8; 16];
        let (_server, mut ctrl_rx, _dg_rx) = register_capturing_server(relay, id.clone()).await;

        let (client, client_ep, reply) = client_connect_dg(relay, id).await;
        assert!(matches!(reply, Message::RelayConnected));

        let connected = recv_timeout(&mut ctrl_rx, "RelayClientConnected").await;
        let handle = match connected {
            Message::RelayClientConnected { handle } => handle,
            other => panic!("expected RelayClientConnected, got {other:?}"),
        };
        assert_ne!(handle, 0, "handle 0 is reserved");

        // Drop the client; the relay must announce its disconnect with the same handle.
        client.close(0u32.into(), b"bye");
        drop(client_ep);
        let disconnected = recv_timeout(&mut ctrl_rx, "RelayClientDisconnected").await;
        match disconnected {
            Message::RelayClientDisconnected { handle: h } => assert_eq!(h, handle),
            other => panic!("expected RelayClientDisconnected, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p farder-relay announces_client_connect_and_disconnect`
Expected: FAIL — times out waiting for `RelayClientConnected` (the relay does not announce yet).

- [ ] **Step 3: Implement handle assignment + announcements in `handle_connect`**

Replace the body of `handle_connect`'s `Some(...)` arm. The full function becomes:

```rust
async fn handle_connect(
    destination_id: Vec<u8>,
    client_conn: Connection,
    mut send: SendStream,
    state: SharedState,
) -> Result<()> {
    let dest = {
        let map = state.servers.read().await;
        map.get(&destination_id).map(|r| (r.conn.clone(), r.control.clone()))
    };
    match dest {
        Some((server_conn, control)) => {
            let ack = codec::encode(&Message::RelayConnected)?;
            write_message(&mut send, &ack).await?;

            // Assign this client a routing handle and record it.
            let handle = state.next_handle.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            state.clients.write().await.insert(handle, client_conn.clone());

            // Announce the new lane to the destination server (reliable control stream).
            announce(&control, &Message::RelayClientConnected { handle }).await;

            // Bridge the client's streams (blocks until the client disconnects).
            let bridge_result = bridge_client(client_conn, server_conn).await;

            // Cleanup: drop the handle and announce the disconnect.
            state.clients.write().await.remove(&handle);
            announce(&control, &Message::RelayClientDisconnected { handle }).await;

            bridge_result
        }
        None => {
            let err = codec::encode(&Message::RelayError {
                reason: "destination not connected".to_string(),
            })?;
            write_message(&mut send, &err).await?;
            let _ = send.finish();
            client_conn.closed().await;
            Ok(())
        }
    }
}

/// Write a control message to a server's relay->server control stream.
/// Best-effort: a write failure (server gone) is logged, not fatal.
async fn announce(control: &Arc<Mutex<SendStream>>, msg: &Message) {
    let encoded = match codec::encode(msg) {
        Ok(b) => b,
        Err(e) => {
            warn!("failed to encode control message: {}", e);
            return;
        }
    };
    let mut guard = control.lock().await;
    if let Err(e) = write_message(&mut guard, &encoded).await {
        warn!("failed to write control message: {}", e);
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p farder-relay announces_client_connect_and_disconnect`
Expected: PASS.

- [ ] **Step 5: Run the whole relay suite (no regressions)**

Run: `cargo test -p farder-relay`
Expected: PASS (all prior tests still green).

- [ ] **Step 6: Commit**

```bash
git add crates/farder-relay/src/router.rs
git commit -m "Assign relay client handles and announce connect/disconnect (Phase 5a)"
```

---

## Task 5: Forward and route voice datagrams

Add the two datagram loops: forward (client→relay→server, tagged with the source handle) and route (server→relay→client, delivered by the destination handle prefix).

**Files:**
- Create: `crates/farder-relay/src/datagram.rs`
- Modify: `crates/farder-relay/src/main.rs` (`mod datagram;`)
- Modify: `crates/farder-relay/src/router.rs` (spawn the forward task in `handle_connect`, the route task in `handle_register`; new tests)

- [ ] **Step 1: Create the datagram loop helpers**

Create `crates/farder-relay/src/datagram.rs`:

```rust
//! Voice-datagram forwarding and routing for relayed connections (Phase 5a).
//!
//! The relay forwards encrypted media datagrams BLIND between a server and its
//! relayed clients, using a per-client `u32` routing handle:
//!   - forward (client -> relay -> server): prefix each datagram with the
//!     source client's handle (4 bytes, big-endian).
//!   - route   (server -> relay -> client): read the destination handle prefix,
//!     strip it, and deliver the payload to that client's connection.

use crate::router::SharedState;
use quinn::Connection;
use tracing::debug;

/// Forward every datagram the client sends to the destination server, tagged
/// with the client's routing handle. Ends when the client connection closes.
pub async fn forward_client_datagrams(client_conn: Connection, server_conn: Connection, handle: u32) {
    loop {
        match client_conn.read_datagram().await {
            Ok(dg) => {
                let mut tagged = Vec::with_capacity(4 + dg.len());
                tagged.extend_from_slice(&handle.to_be_bytes());
                tagged.extend_from_slice(&dg);
                // Best-effort: drop if the server can't take datagrams.
                if let Err(e) = server_conn.send_datagram(tagged.into()) {
                    debug!("drop forwarded datagram (handle {}): {}", handle, e);
                }
            }
            Err(_) => break, // client gone
        }
    }
}

/// Route every datagram the server sends to the client named by its 4-byte
/// big-endian handle prefix. Unknown/closed handles are dropped. Ends when the
/// server connection closes.
pub async fn route_server_datagrams(server_conn: Connection, state: SharedState) {
    loop {
        match server_conn.read_datagram().await {
            Ok(dg) => {
                if dg.len() < 4 {
                    continue; // malformed; no handle prefix
                }
                let handle = u32::from_be_bytes([dg[0], dg[1], dg[2], dg[3]]);
                let payload = dg.slice(4..);
                let client = { state.clients.read().await.get(&handle).cloned() };
                if let Some(c) = client {
                    if let Err(e) = c.send_datagram(payload) {
                        debug!("drop routed datagram (handle {}): {}", handle, e);
                    }
                }
            }
            Err(_) => break, // server gone
        }
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/farder-relay/src/main.rs`, add to the module list at the top (after `mod config;`):

```rust
mod datagram;
```

- [ ] **Step 3: Spawn the route task when a server registers**

In `router.rs` `handle_register`, after writing the `RelayRegistered` ack and before `conn.closed().await`, add:

```rust
    // Route datagrams the server sends back to the right relayed client.
    let route_state = state.clone();
    let route_conn = conn.clone();
    tokio::spawn(async move {
        crate::datagram::route_server_datagrams(route_conn, route_state).await;
    });
```

- [ ] **Step 4: Spawn the forward task when a client connects**

In `router.rs` `handle_connect`'s `Some(...)` arm, immediately after the `RelayClientConnected` announce and before `bridge_client(...)`, add:

```rust
            // Forward the client's voice datagrams to the server, tagged with its handle.
            let fwd_client = client_conn.clone();
            let fwd_server = server_conn.clone();
            tokio::spawn(async move {
                crate::datagram::forward_client_datagrams(fwd_client, fwd_server, handle).await;
            });
```

- [ ] **Step 5: Write the failing tests**

In `router.rs` `mod tests`, add:

```rust
    #[tokio::test]
    async fn forwards_client_datagram_tagged_with_handle() {
        let (relay, _state) = start_relay().await;
        let id = vec![6u8; 16];
        let (_server, mut ctrl_rx, mut dg_rx) = register_capturing_server(relay, id.clone()).await;

        let (client, _client_ep, reply) = client_connect_dg(relay, id).await;
        assert!(matches!(reply, Message::RelayConnected));
        let handle = match recv_timeout(&mut ctrl_rx, "RelayClientConnected").await {
            Message::RelayClientConnected { handle } => handle,
            other => panic!("got {other:?}"),
        };

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
        let (server, mut ctrl_rx, _dg_rx) = register_capturing_server(relay, id.clone()).await;

        // Two clients connect; capture both handles in announce order.
        let (client1, _ep1, _r1) = client_connect_dg(relay, id.clone()).await;
        let h1 = match recv_timeout(&mut ctrl_rx, "connect 1").await {
            Message::RelayClientConnected { handle } => handle,
            other => panic!("got {other:?}"),
        };
        let (client2, _ep2, _r2) = client_connect_dg(relay, id).await;
        let h2 = match recv_timeout(&mut ctrl_rx, "connect 2").await {
            Message::RelayClientConnected { handle } => handle,
            other => panic!("got {other:?}"),
        };
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
        let (server, mut ctrl_rx, _dg_rx) = register_capturing_server(relay, id.clone()).await;

        let (client, _ep, _r) = client_connect_dg(relay, id).await;
        let handle = match recv_timeout(&mut ctrl_rx, "connect").await {
            Message::RelayClientConnected { handle } => handle,
            other => panic!("got {other:?}"),
        };

        // A datagram tagged with a never-assigned handle must be dropped (no panic).
        let mut bogus = 999_999u32.to_be_bytes().to_vec();
        bogus.extend_from_slice(b"nowhere");
        server.send_datagram(Bytes::from(bogus)).unwrap();

        // A subsequent valid datagram still routes — proving the loop survived.
        let mut valid = handle.to_be_bytes().to_vec();
        valid.extend_from_slice(b"real");
        server.send_datagram(Bytes::from(valid)).unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), client.read_datagram())
            .await
            .expect("valid datagram after a bogus one timed out")
            .unwrap();
        assert_eq!(got.as_ref(), b"real");
    }
```

- [ ] **Step 6: Run the new tests to verify they pass**

Run: `cargo test -p farder-relay forwards_client_datagram_tagged_with_handle routes_server_datagram_to_correct_client unknown_handle_datagram_is_dropped`
Expected: PASS (3 tests). (If you ran them before Steps 1-4, they'd fail — forward/route not wired. Optionally verify by stashing the impl, but it's sufficient to confirm green here.)

- [ ] **Step 7: Run the full relay suite**

Run: `cargo test -p farder-relay`
Expected: PASS (all tests).

- [ ] **Step 8: Commit**

```bash
git add crates/farder-relay/src/datagram.rs crates/farder-relay/src/main.rs crates/farder-relay/src/router.rs
git commit -m "Forward and route voice datagrams through the relay (Phase 5a)"
```

---

## Task 6: Backward-compatibility gate + module doc

Verify a Phase-2 server (which knows nothing about handles/datagrams) still works against the new relay, and document the relay's datagram-routing surface.

**Files:**
- Create: `docs/modules/relay.md`
- Modify: `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md` (status note)

- [ ] **Step 1: Run the workspace suite (backward-compat gate)**

Run: `cargo test --workspace`
Expected: PASS. In particular the Phase-2 server tests `cargo test -p farder-server --test relay_mode` must pass unchanged — this proves the relay opening a control stream + forwarding datagrams to a non-5b server does NOT break relayed text/file traffic (the server logs+drops the unexpected control stream per-task; it never reads datagrams). If anything in `relay_mode` fails, STOP and treat it as a real backward-compat regression (do not weaken the test) — investigate the server's per-stream error isolation in `crates/farder-server/src/relay.rs`.

- [ ] **Step 2: Confirm the workspace builds clean (no warnings on the new code)**

Run: `cargo build -p farder-relay`
Expected: builds with no warnings about unused `RegisteredServer`/`RelayState` fields or the `datagram` module.

- [ ] **Step 3: Write the relay module doc**

Create `docs/modules/relay.md`:

```markdown
# Module: relay (`crates/farder-relay`)

**Purpose:** the Farder rendezvous relay. Servers register; clients connect by
server id; the relay bridges their streams and (Phase 5a) routes voice datagrams,
so neither side learns the other's IP. It forwards encrypted bytes blind.

See the umbrella design `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md`.

## Connection lifecycle

- `listener::create_endpoint(bind, data_dir)` — QUIC server endpoint with a
  persistent self-signed cert (`relay_cert.der`/`relay_key.der`, key 0600), a 60s
  idle timeout, and **datagrams enabled** (1 MiB send/recv buffers).
- `router::serve(endpoint, state, limiter)` — accept loop, gated by the abuse
  limiter (global cap + per-IP rate). Each connection's first bi-stream carries a
  `RelayRegister { server_id }` (server) or `RelayConnect { destination_id }` (client).

## State (`router::RelayState`, `SharedState = Arc<RelayState>`)

- `servers: RwLock<HashMap<Vec<u8>, RegisteredServer>>` — registered servers by id.
  `RegisteredServer { conn, control }` where `control` is the relay->server control
  stream (`Arc<Mutex<SendStream>>`).
- `clients: RwLock<HashMap<u32, Connection>>` — live relayed clients by routing handle.
- `next_handle: AtomicU32` — monotonic handle allocator (starts at 1; 0 is reserved).

## Datagram routing (Phase 5a)

Each relayed client gets a `u32` **handle** at `RelayConnect`. The relay:
- announces `RelayClientConnected { handle }` / `RelayClientDisconnected { handle }`
  to the destination server over its reliable control stream (so the server learns
  every client's lane, even silent listeners);
- **forwards** client->server datagrams tagged `[handle:u32 BE][payload]`
  (`datagram::forward_client_datagrams`);
- **routes** server->client datagrams by stripping the `[handle]` prefix and
  delivering to that client's connection (`datagram::route_server_datagrams`);
  unknown handles are dropped.

The relay never reads the media payload (privacy preserved). Datagram sends are
best-effort (dropped if a peer hasn't enabled datagrams — e.g. a pre-5b server/client).

## Backward compatibility

A server/client that predates Phase 5a keeps working: the relay's control stream is
just an unexpected stream the server logs and drops per-task; forwarded datagrams it
never reads are dropped by QUIC. No relayed text/file traffic is affected.

## Protocol messages (`farder-protocol`)

`RelayRegister`/`RelayRegistered`, `RelayConnect`/`RelayConnected`/`RelayError`,
and the Phase-5a `RelayClientConnected { handle }` / `RelayClientDisconnected { handle }`.

## Out of scope (Phase 5b)

The server reading/producing tagged datagrams (voice fan-out over the relay), the
client enabling datagrams on its pinned relay endpoint and re-enabling voice, and
real-audio end-to-end verification. 5a is the relay half only, tested headlessly
with doubles in `router.rs`'s `mod tests`.

## Tests

`crates/farder-relay/src/router.rs` `#[cfg(test)] mod tests` (real-QUIC loopback,
doubles): handle announce on connect/disconnect, datagram forward tagging, selective
routing between two clients, unknown-handle drop, plus the Phase-1/2 bridge/registry
tests (the backward-compat checkpoint). `farder-server`'s `relay_mode` integration
tests are the cross-crate backward-compat gate.
```

- [ ] **Step 4: Update the umbrella spec status**

In `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md`, find the status/phase section and add a note that **Phase 5a (relay datagram-routing core) is implemented**; Phase 5b (server/client voice wiring + real-audio verification) remains. (Match the existing status-note style in that file; if there is a per-phase status list, append the 5a entry.)

- [ ] **Step 5: Commit**

```bash
git add docs/modules/relay.md docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md
git commit -m "Document relay datagram-routing surface; mark Phase 5a done (Phase 5a)"
```

---

## Final verification

- [ ] `cargo test --workspace` — all green.
- [ ] `cargo build --workspace` — no warnings on the new relay code.
- [ ] Spec coverage: datagrams enabled (Task 2); handles (Task 4); control stream + announcements (Tasks 3-4); forward path (Task 5); route path (Task 5); backward-compat invariant (Tasks 3 & 6); protocol messages (Task 1); doc (Task 6). All spec sections covered.

After all tasks: use **superpowers:finishing-a-development-branch** to complete the work.
```

