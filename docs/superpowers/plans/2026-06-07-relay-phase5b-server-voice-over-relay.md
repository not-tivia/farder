# Relay Phase 5b (server core) — Voice Over Relay — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Farder server send and receive voice over the relay's datagram routing, so relayed clients can eventually do voice — building the server half and the relay's authoritative handle-stamp, with the client wiring + real-audio verification deferred.

**Architecture:** The relay stamps each bridged stream with the client's `u32` handle (authoritative correlation). The server reads that stamp, binds `handle <-> member` at auth, registers a `VoiceSink::Relayed{relay, handle}`, runs one voice datagram loop on the relay connection (demux by source handle, fan out tagged by recipient handle), and unifies fan-out behind a `VoiceSink` abstraction. The now-superseded 5a control-stream announce is removed.

**Tech Stack:** Rust, quinn 0.11 (QUIC datagrams), `bytes::Bytes`, tokio, `farder-protocol`, the `media_stream` frame/ingress layer.

**Spec:** `docs/superpowers/specs/2026-06-07-relay-phase5b-server-voice-over-relay-design.md`

---

## Context for the implementer

- **Two crates change:** `farder-relay` (the real relay: stamp handles, drop the 5a announce) and `farder-server` (consume the stamp + route voice). `farder-protocol` loses two variants.
- **`farder-relay` is a binary crate** — its tests live in `#[cfg(test)] mod tests` inside `router.rs`. `farder-server` has both in-file unit tests and `tests/relay_mode.rs` integration tests (which use an **inline relay double**, because the bin crate can't be imported).
- quinn datagrams: `conn.read_datagram().await -> Result<bytes::Bytes>`; `conn.send_datagram(Bytes) -> Result<(), SendDatagramError>` (best-effort). Both ends must enable datagram buffers on their `TransportConfig`.
- The handle stamp is a **raw 4-byte big-endian `u32`** written to the stream before the existing `RelayStreamRole` frame — NOT a length-framed `Message`.
- Media frames: `media_stream::build_media_frame(kind, seq, &session_id, ciphertext)` builds one; the 16-byte `session_id` sits at bytes `12..28`; `on_frame_ingress(&mut StreamState, &MediaConfig, &sending_conn_pk, &frame, now_ms) -> IngressDecision::{Forward{recipients: Vec<SessionId>}, Drop(_)}`. See `media_stream.rs` test `sealed_sender_no_pubkey_in_frame_header` (~line 588) for the exact pattern to build channel state + a frame.
- Run all tests from `/home/deez/farder`: `cargo test --workspace`. Per-crate: `cargo test -p farder-relay`, `cargo test -p farder-server`, `cargo test -p farder-server --test relay_mode`.

---

## File structure

- `crates/farder-relay/src/router.rs` — `bridge_client` stamps the handle; remove control stream + `announce`; `RegisteredServer` simplified; rework 5a tests.
- `crates/farder-protocol/src/messages.rs` — remove `RelayClientConnected`/`RelayClientDisconnected` + tests.
- `crates/farder-server/src/state.rs` — `VoiceSink` enum; `voice_connections: HashMap<[u8;32], VoiceSink>`; `relay_voice_handles` map.
- `crates/farder-server/src/connection.rs` — extract `process_inbound_voice_frame`; fan-out via `VoiceSink`; register `VoiceSink::Direct`.
- `crates/farder-server/src/relay.rs` — read stamp; thread handle + relay conn; register relayed sink; enable datagrams; spawn relay voice loop.
- `crates/farder-server/tests/relay_mode.rs` — relay double stamps the handle (keeps existing tests green through the stamp-reading server).
- `docs/modules/relay.md`, `docs/modules/server-relay.md` — update.

---

## Task 1: Relay — stamp the handle on bridged streams

**Files:** Modify `crates/farder-relay/src/router.rs` (`bridge_client` + `handle_connect` call site + a new test).

- [ ] **Step 1: Write the failing test**

In `router.rs` `mod tests`, add a test that a bridged stream begins with the client's handle (cross-checked against the 5a control-stream announce, which still exists at this point). It reuses `register_capturing_server` (reads the control stream for the announced handle) and adds a stamped-stream read:

```rust
    #[tokio::test]
    async fn bridged_stream_is_stamped_with_client_handle() {
        let (relay, _state) = start_relay().await;
        let id = vec![9u8; 16];
        // Capturing server: its accept_bi yields bridged streams; we read the
        // 4-byte handle stamp off the first one. It also still reads the 5a
        // control-stream announce so we can cross-check the handle value.
        let ep = test_client_endpoint_with_datagrams();
        let conn = ep.connect(relay, "farder-relay").unwrap().await.unwrap();
        let (mut sreg, mut rreg) = conn.open_bi().await.unwrap();
        let reg = codec::encode(&Message::RelayRegister { server_id: id.clone() }).unwrap();
        write_message(&mut sreg, &reg).await.unwrap();
        let ack = read_message(&mut rreg).await.unwrap();
        assert!(matches!(codec::decode::<Message>(&ack).unwrap(), Message::RelayRegistered));

        // The relay opens TWO streams to us once a client connects+bridges: the
        // 5a control stream (carries RelayClientConnected) and the bridged client
        // stream (carries the 4-byte handle stamp + the client's bytes). We read
        // the first 4 bytes of whichever bridged (non-control) stream we get.
        let server_conn = conn.clone();

        // Client connects and opens ONE bi-stream so the relay bridges it.
        let cep = test_client_endpoint_with_datagrams();
        let cconn = cep.connect(relay, "farder-relay").unwrap().await.unwrap();
        let (mut cs, mut cr) = cconn.open_bi().await.unwrap();
        let m = codec::encode(&Message::RelayConnect { destination_id: id }).unwrap();
        write_message(&mut cs, &m).await.unwrap();
        let reply = read_message(&mut cr).await.unwrap();
        assert!(matches!(codec::decode::<Message>(&reply).unwrap(), Message::RelayConnected));
        let (mut bs, _br) = cconn.open_bi().await.unwrap();
        bs.write_all(b"after-the-stamp").await.unwrap();

        // Server side: accept streams; the bridged client stream starts with the
        // 4-byte handle stamp followed by "after-the-stamp".
        let mut found_handle: Option<u32> = None;
        for _ in 0..3 {
            let (_s, mut r) = tokio::time::timeout(Duration::from_secs(5), server_conn.accept_bi())
                .await.expect("accept_bi timed out").unwrap();
            let mut h = [0u8; 4];
            if r.read_exact(&mut h).await.is_ok() {
                let rest = r.read_to_end(64).await.unwrap_or_default();
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
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p farder-relay bridged_stream_is_stamped_with_client_handle`
Expected: FAIL — the bridged stream starts with `"after-the-stamp"` (no stamp), so no accepted stream matches the `[4-byte handle] ++ "after-the-stamp"` shape and `found_handle` is `None`.

- [ ] **Step 3: Stamp the handle in `bridge_client`**

In `router.rs`, change `bridge_client` to take the handle and write it on each server-bound stream before copying. Replace the function with:

```rust
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
```

- [ ] **Step 4: Pass the handle at the call site**

In `handle_connect`, the `bridge_client(client_conn, server_conn)` call becomes `bridge_client(client_conn, server_conn, handle)`. (The `handle` is already assigned earlier in that function for the 5a forward task.)

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p farder-relay bridged_stream_is_stamped_with_client_handle`
Expected: PASS.

- [ ] **Step 6: Run the full relay suite**

Run: `cargo test -p farder-relay`
Expected: PASS. (The existing bridge tests — `bridges_client_to_registered_server`, `reregistration_routes_to_newest_server` — now go through a stamped stream. Their server doubles read with `read_to_end`, which will now include the 4-byte prefix. **If either fails because the echoed bytes now include the stamp, that is expected — fix those two doubles to read+discard the 4-byte handle prefix before echoing**, mirroring how a real server strips it. Make that fix in this step and note it.)

- [ ] **Step 7: Commit**

```bash
git add crates/farder-relay/src/router.rs
git commit -m "Relay: stamp the routing handle on bridged streams (Phase 5b)"
```

---

## Task 2: Relay + protocol — remove the superseded 5a control-stream announce

**Files:** Modify `crates/farder-relay/src/router.rs` (remove control stream, `announce`, rework tests) + `crates/farder-protocol/src/messages.rs` (remove two variants + tests).

- [ ] **Step 1: Remove the protocol variants and their tests**

In `crates/farder-protocol/src/messages.rs`, delete the `RelayClientConnected { handle: u32 }` and `RelayClientDisconnected { handle: u32 }` enum variants, and delete the tests `test_roundtrip_relay_client_connected` and `test_roundtrip_relay_client_disconnected`.

- [ ] **Step 2: Remove the control stream + announce in the relay**

In `router.rs`:
- In `handle_register`, delete the `conn.open_bi()` control-stream creation and the `control` field stored in `RegisteredServer`. Simplify `RegisteredServer` so it holds only what remains needed (the `conn`). If `RegisteredServer` now has a single field, you may collapse the map value back to `Connection` directly — choose whichever keeps `handle_connect`/the route task readable; update `RegisteredServer`'s definition and all constructors/reads accordingly.
- In `handle_connect`, delete the two `announce(&control, ...)` calls and stop fetching `control` from the registry (fetch only `conn`). Keep the handle assignment, the `state.clients` insert/remove, the forward-task spawn, and `bridge_client(..., handle)`.
- Delete the `announce` helper function.
- If `Mutex` is now unused in `router.rs`, remove its import.

- [ ] **Step 3: Rework the 5a tests that depended on the announce**

In `router.rs` `mod tests`:
- Delete the test `announces_client_connect_and_disconnect`.
- Change `register_capturing_server` so it no longer reads control-stream announcements. It should still return the connection and a datagram receiver, plus a way to learn a client's handle **from the stamp**: spawn a task that `accept_bi`'s bridged streams and, for each, reads the 4-byte handle prefix and forwards it on an `mpsc::UnboundedReceiver<u32>`. New signature:
  `async fn register_capturing_server(relay, id) -> (Connection, mpsc::UnboundedReceiver<u32> /*handles from stamps*/, mpsc::UnboundedReceiver<Bytes> /*datagrams*/)`.
  Implementation sketch for the stamp reader:
  ```rust
  let (h_tx, h_rx) = mpsc::unbounded_channel();
  let h_conn = conn.clone();
  tokio::spawn(async move {
      while let Ok((_s, mut r)) = h_conn.accept_bi().await {
          let mut hb = [0u8; 4];
          if r.read_exact(&mut hb).await.is_ok() {
              let _ = h_tx.send(u32::from_be_bytes(hb));
              // drain the rest of the bridged stream so it doesn't block
              tokio::spawn(async move { let _ = r.read_to_end(1 << 20).await; });
          }
      }
  });
  ```
- Update `forwards_client_datagram_tagged_with_handle`, `routes_server_datagram_to_correct_client`, and `unknown_handle_datagram_is_dropped`: each client must now **open one bi-stream** (so the relay bridges + stamps it) and the test learns the handle from the `mpsc::UnboundedReceiver<u32>` (stamp) instead of from a `RelayClientConnected` message. Concretely, after `client_connect_dg(...)` returns, do `let (mut bs, _br) = client.open_bi().await.unwrap(); bs.write_all(b"x").await.unwrap();` then `let handle = recv_timeout(&mut handle_rx, "handle stamp").await;`. The rest of each assertion (forward tagging / selective routing / unknown-handle drop) is unchanged.

- [ ] **Step 4: Build and run the relay suite**

Run: `cargo test -p farder-relay`
Expected: PASS. No reference to `RelayClientConnected`/`RelayClientDisconnected` remains; the datagram tests learn the handle via the stamp.

- [ ] **Step 5: Confirm the protocol crate is clean**

Run: `cargo test -p farder-protocol`
Expected: PASS (the two removed tests are gone; the rest pass).

- [ ] **Step 6: Commit**

```bash
git add crates/farder-relay/src/router.rs crates/farder-protocol/src/messages.rs
git commit -m "Remove superseded 5a control-stream announce; handle now correlated via stream stamp (Phase 5b)"
```

---

## Task 3: Server state — VoiceSink abstraction

**Files:** Modify `crates/farder-server/src/state.rs`; modify the direct-mode insert + fan-out call site in `crates/farder-server/src/connection.rs`.

- [ ] **Step 1: Write the failing unit test**

In `crates/farder-server/src/state.rs`, add a `#[cfg(test)] mod tests` (or extend an existing one) with a loopback test proving the prefix behavior. Add a small helper to make a connected pair (or reuse one if present):

```rust
#[cfg(test)]
mod voice_sink_tests {
    use super::*;
    use bytes::Bytes;

    async fn loopback_pair() -> (quinn::Connection, quinn::Connection) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        // server endpoint
        let cert = rcgen::generate_simple_self_signed(vec!["t".into()]).unwrap();
        let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
        let key = rustls::pki_types::PrivateKeyDer::try_from(cert.key_pair.serialize_der()).unwrap();
        let mut scfg = quinn::ServerConfig::with_single_cert(vec![cert_der], key).unwrap();
        {
            let mut t = quinn::TransportConfig::default();
            t.datagram_receive_buffer_size(Some(1 << 20));
            t.datagram_send_buffer_size(1 << 20);
            scfg.transport_config(std::sync::Arc::new(t));
        }
        let sep = quinn::Endpoint::server(scfg, "127.0.0.1:0".parse().unwrap()).unwrap();
        let saddr = sep.local_addr().unwrap();
        #[derive(Debug)] struct Skip;
        impl rustls::client::danger::ServerCertVerifier for Skip {
            fn verify_server_cert(&self,_:&rustls::pki_types::CertificateDer<'_>,_:&[rustls::pki_types::CertificateDer<'_>],_:&rustls::pki_types::ServerName<'_>,_:&[u8],_:rustls::pki_types::UnixTime)->std::result::Result<rustls::client::danger::ServerCertVerified,rustls::Error>{Ok(rustls::client::danger::ServerCertVerified::assertion())}
            fn verify_tls12_signature(&self,_:&[u8],_:&rustls::pki_types::CertificateDer<'_>,_:&rustls::DigitallySignedStruct)->std::result::Result<rustls::client::danger::HandshakeSignatureValid,rustls::Error>{Ok(rustls::client::danger::HandshakeSignatureValid::assertion())}
            fn verify_tls13_signature(&self,_:&[u8],_:&rustls::pki_types::CertificateDer<'_>,_:&rustls::DigitallySignedStruct)->std::result::Result<rustls::client::danger::HandshakeSignatureValid,rustls::Error>{Ok(rustls::client::danger::HandshakeSignatureValid::assertion())}
            fn supported_verify_schemes(&self)->Vec<rustls::SignatureScheme>{rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()}
        }
        let crypto = rustls::ClientConfig::builder().dangerous().with_custom_certificate_verifier(std::sync::Arc::new(Skip)).with_no_client_auth();
        let mut ccfg = quinn::ClientConfig::new(std::sync::Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap()));
        let mut t = quinn::TransportConfig::default();
        t.datagram_receive_buffer_size(Some(1 << 20));
        t.datagram_send_buffer_size(1 << 20);
        ccfg.transport_config(std::sync::Arc::new(t));
        let mut cep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        cep.set_default_client_config(ccfg);
        let server_fut = tokio::spawn(async move { sep.accept().await.unwrap().await.unwrap() });
        let client = cep.connect(saddr, "t").unwrap().await.unwrap();
        let server = server_fut.await.unwrap();
        std::mem::forget(cep);
        (client, server)
    }

    #[tokio::test]
    async fn relayed_sink_prefixes_handle_direct_does_not() {
        let (a, b) = loopback_pair().await;
        // Relayed: a's sink sends [handle][frame]; b reads it.
        let sink = VoiceSink::Relayed { relay: a.clone(), handle: 0x01020304 };
        sink.send_datagram(Bytes::from_static(b"frame")).unwrap();
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), b.read_datagram()).await.unwrap().unwrap();
        assert_eq!(got.as_ref(), &[0x01,0x02,0x03,0x04, b'f',b'r',b'a',b'm',b'e']);

        // Direct: sends the frame unchanged.
        let sink = VoiceSink::Direct(a.clone());
        sink.send_datagram(Bytes::from_static(b"frame")).unwrap();
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), b.read_datagram()).await.unwrap().unwrap();
        assert_eq!(got.as_ref(), b"frame");
    }
}
```

(If `rcgen`/`rustls` aren't already `dev-dependencies` of `farder-server`, they are regular deps — check `Cargo.toml`; `farder-server` already uses quinn + rustls. Add `rcgen` to `[dev-dependencies]` if missing.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p farder-server relayed_sink_prefixes_handle_direct_does_not`
Expected: FAIL to compile — `VoiceSink` doesn't exist.

- [ ] **Step 3: Define `VoiceSink` and change the maps**

In `crates/farder-server/src/state.rs`:

Add the enum (near the top, after imports):

```rust
/// Where a member's voice datagrams are sent. Direct clients have their own
/// QUIC connection; relayed clients share the server's single relay connection
/// and are addressed by their relay-assigned routing handle (Phase 5b).
pub enum VoiceSink {
    Direct(quinn::Connection),
    Relayed { relay: quinn::Connection, handle: u32 },
}

impl VoiceSink {
    pub fn send_datagram(&self, frame: bytes::Bytes) -> Result<(), quinn::SendDatagramError> {
        match self {
            VoiceSink::Direct(c) => c.send_datagram(frame),
            VoiceSink::Relayed { relay, handle } => {
                let mut tagged = Vec::with_capacity(4 + frame.len());
                tagged.extend_from_slice(&handle.to_be_bytes());
                tagged.extend_from_slice(&frame);
                relay.send_datagram(tagged.into())
            }
        }
    }
}
```

Change the field (line ~58) from
`pub voice_connections: RwLock<HashMap<[u8; 32], quinn::Connection>>,`
to
`pub voice_connections: RwLock<HashMap<[u8; 32], VoiceSink>>,`
and add, next to it:
`pub relay_voice_handles: RwLock<HashMap<u32, [u8; 32]>>,`

In the constructor (line ~76) add `relay_voice_handles: RwLock::new(HashMap::new()),` alongside the existing `voice_connections: RwLock::new(HashMap::new()),`.

- [ ] **Step 4: Update direct-mode insert and fan-out to use `VoiceSink`**

In `crates/farder-server/src/connection.rs`:
- Line ~643 insert: `voice_conns.insert(pk_bytes, conn.clone());` becomes `voice_conns.insert(pk_bytes, crate::state::VoiceSink::Direct(conn.clone()));`
- Fan-out (~lines 774-778): `if let Some(peer_conn) = voice_conns.get(conn_pk) { let _ = peer_conn.send_datagram(bytes.clone()); }` becomes `if let Some(sink) = voice_conns.get(conn_pk) { let _ = sink.send_datagram(bytes.clone()); }`
- Line ~821 remove (`voice_connections.write().await.remove(&pk_bytes);`) is unchanged.

- [ ] **Step 5: Run the test + the existing voice tests**

Run: `cargo test -p farder-server relayed_sink_prefixes_handle_direct_does_not`
Expected: PASS.
Run: `cargo test -p farder-server voice` and `cargo test -p farder-server --test relay_mode`
Expected: PASS (direct voice behavior unchanged; relay_mode unaffected so far).

- [ ] **Step 6: Commit**

```bash
git add crates/farder-server/src/state.rs crates/farder-server/src/connection.rs crates/farder-server/Cargo.toml
git commit -m "Server: VoiceSink abstraction (Direct vs Relayed) for voice fan-out (Phase 5b)"
```

---

## Task 4: Server — extract `process_inbound_voice_frame` + keystone routing test

**Files:** Modify `crates/farder-server/src/connection.rs` (extract the frame-processing core; direct loop calls it; add the routing test).

- [ ] **Step 1: Write the failing keystone test**

In `connection.rs` `#[cfg(test)] mod tests` (add the module if absent), add a test that builds a two-member voice channel, registers both as `Relayed` sinks on one shared loopback "relay" connection, and asserts a frame from Alice is fanned out tagged with Bob's handle (and Bob only). Reuse the `loopback_pair` pattern (you may factor it into a shared test helper, or inline a copy):

```rust
#[cfg(test)]
mod voice_relay_tests {
    use super::*;
    use crate::state::{ServerState, VoiceSink};
    use crate::media_stream::{build_media_frame, ServerSession, TrackKind, MediaConfig};
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use bytes::Bytes;

    // Copy the `loopback_pair` helper from Task 3 Step 1 verbatim into this test
    // module (a connected (client, server) QUIC pair with datagrams enabled).
    async fn loopback_pair() -> (quinn::Connection, quinn::Connection) { /* paste Task 3 Step 1's body */ todo!() }

    #[tokio::test]
    async fn relayed_fanout_tags_recipient_handle() {
        let state = Arc::new(ServerState::new_for_test().unwrap());
        let config = MediaConfig::default();

        // One shared "relay" connection (server -> relay). We read what the
        // server emits on it (the read side is `relay_rx`).
        let (relay_tx_side, relay_rx) = loopback_pair().await; // relay_tx_side = server's relay conn; relay_rx = relay's view

        let alice_pk = farder_crypto::identity::PublicKey::from_bytes([0xaa; 32]);
        let bob_pk = farder_crypto::identity::PublicKey::from_bytes([0xbb; 32]);
        let alice_conn = [0xaa; 32];
        let bob_conn = [0xbb; 32];
        let alice_session = [1u8; 16];
        let bob_session = [2u8; 16];
        let (h_alice, h_bob) = (10u32, 20u32);

        // Install both members in channel 99 with Audio enabled.
        {
            let mut channels = state.media.channels.write().unwrap();
            let st = channels.entry(99).or_insert_with(crate::media_stream::StreamState::new);
            for (sid, conn_pk, pk, name) in [
                (alice_session, alice_conn, alice_pk.clone(), "alice"),
                (bob_session, bob_conn, bob_pk.clone(), "bob"),
            ] {
                let mut tracks = HashSet::new();
                tracks.insert(TrackKind::Audio);
                st.sessions.insert(sid, ServerSession {
                    connection_pk: conn_pk, channel_id: 99, public_key: pk,
                    display_name: name.into(), active_tracks: tracks,
                    buckets: HashMap::new(), last_audio_frame_ms: None, last_video_frame_ms: None,
                });
            }
        }
        // Both members are relayed on the shared relay connection.
        {
            let mut vc = state.voice_connections.write().await;
            vc.insert(alice_conn, VoiceSink::Relayed { relay: relay_tx_side.clone(), handle: h_alice });
            vc.insert(bob_conn, VoiceSink::Relayed { relay: relay_tx_side.clone(), handle: h_bob });
        }

        // Alice speaks: process her frame as if it arrived from the relay.
        let frame = build_media_frame(TrackKind::Audio, 1, &alice_session, b"opaque-ct");
        process_inbound_voice_frame(&state, alice_conn, Bytes::from(frame.clone()), &config).await;

        // The server must emit exactly [h_bob][frame] on the relay connection.
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), relay_rx.read_datagram())
            .await.expect("no datagram emitted").unwrap();
        let mut expected = h_bob.to_be_bytes().to_vec();
        expected.extend_from_slice(&frame);
        assert_eq!(got.as_ref(), expected.as_slice(), "fan-out must tag the RECIPIENT (bob) handle");

        // And NOT a second datagram tagged with alice (sender excluded).
        let second = tokio::time::timeout(std::time::Duration::from_millis(300), relay_rx.read_datagram()).await;
        assert!(second.is_err(), "only one recipient (bob); sender must not be echoed");
    }
}
```

(`build_media_frame` is a `media_stream` test helper today — if it is `#[cfg(test)]`-only and not reachable from `connection.rs` tests, promote it to `pub(crate) fn build_media_frame(...)` in `media_stream.rs`. That is a small, justified visibility change; keep its body unchanged.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p farder-server relayed_fanout_tags_recipient_handle`
Expected: FAIL to compile — `process_inbound_voice_frame` doesn't exist.

- [ ] **Step 3: Extract `process_inbound_voice_frame`**

In `connection.rs`, add a module function that contains the per-frame logic currently inline in the datagram loop (the `now_ms` computation, the channel lookup by session_id, the `on_frame_ingress` decision, and the `Forward`/`Drop` handling that fans out via `VoiceSink::send_datagram`):

```rust
/// Process one inbound voice frame: find its channel, run the ingress decision,
/// and fan it out to each recipient's VoiceSink. Shared by the direct
/// per-connection datagram loop and the relay-mode datagram loop. `sending_pk`
/// is the authoritative sender (the direct connection's authed pk, or the relay
/// source handle's bound pk).
pub(crate) async fn process_inbound_voice_frame(
    state: &Arc<ServerState>,
    sending_pk: [u8; 32],
    bytes: bytes::Bytes,
    media_config: &crate::media_stream::MediaConfig,
) {
    let now_ms = {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
    };
    let raw: &[u8] = &bytes;
    let channel_id_opt: Option<u64> = if raw.len() >= crate::media_stream::MEDIA_FRAME_HEADER_LEN {
        let mut sid = [0u8; crate::media_stream::SESSION_ID_LEN];
        sid.copy_from_slice(&raw[12..12 + crate::media_stream::SESSION_ID_LEN]);
        let channels = state.media.channels.read().unwrap();
        channels.iter().find(|(_ch, st)| st.sessions.contains_key(&sid)).map(|(ch, _)| *ch)
    } else {
        None
    };
    let channel_id = match channel_id_opt {
        Some(c) => c,
        None => { tracing::trace!("[media] datagram dropped: session not found"); return; }
    };
    let decision = {
        let mut channels = state.media.channels.write().unwrap();
        if let Some(stream_state) = channels.get_mut(&channel_id) {
            crate::media_stream::on_frame_ingress(stream_state, media_config, &sending_pk, raw, now_ms)
        } else {
            crate::media_stream::IngressDecision::Drop(crate::media_stream::DropReason::UnknownSession)
        }
    };
    match decision {
        crate::media_stream::IngressDecision::Forward { recipients } => {
            let voice_conns = state.voice_connections.read().await;
            let channels = state.media.channels.read().unwrap();
            if let Some(stream_state) = channels.get(&channel_id) {
                for sid in recipients {
                    if let Some(session) = stream_state.sessions.get(&sid) {
                        if let Some(sink) = voice_conns.get(&session.connection_pk) {
                            let _ = sink.send_datagram(bytes.clone());
                        }
                    }
                }
            }
        }
        crate::media_stream::IngressDecision::Drop(_reason) => {
            tracing::trace!("[media] datagram dropped: {:?}", _reason);
        }
    }
}
```

Then replace the body of the direct per-connection datagram loop's `Ok(bytes) => { ... }` arm (the big block ~696-790) with a single call:

```rust
                Ok(bytes) => {
                    process_inbound_voice_frame(&state_for_dg, pk_bytes, bytes, &media_config).await;
                }
```

(Keep the surrounding loop, the `Err(...)` arms, and the `media_config`/`state_for_dg` captures.)

- [ ] **Step 4: Run the keystone test + regressions**

Run: `cargo test -p farder-server relayed_fanout_tags_recipient_handle`
Expected: PASS.
Run: `cargo test -p farder-server voice` and `cargo test -p farder-server --test relay_mode`
Expected: PASS (direct path now routes through the extracted fn with identical behavior).

- [ ] **Step 5: Commit**

```bash
git add crates/farder-server/src/connection.rs crates/farder-server/src/media_stream.rs
git commit -m "Server: extract process_inbound_voice_frame; fan out by VoiceSink (Phase 5b)"
```

---

## Task 5: Server relay — read the stamp, register relayed sinks, run the relay voice loop

**Files:** Modify `crates/farder-server/src/relay.rs`; modify the relay double in `crates/farder-server/tests/relay_mode.rs` to stamp the handle (so the existing tests pass through the stamp-reading server).

- [ ] **Step 1: Update the relay_mode test double to stamp the handle (RED via the server change)**

In `crates/farder-server/tests/relay_mode.rs`, the inline relay double's `relay_connect` bridges client streams. Make it stamp a per-client handle on each server-bound stream (matching the real relay). Add a handle counter and write 4 bytes before copying:

```rust
async fn relay_connect(destination_id: Vec<u8>, client_conn: Connection, mut send: SendStream, conns: ConnectionMap) {
    let dest = conns.read().await.get(&destination_id).cloned();
    match dest {
        Some(server_conn) => {
            write_framed(&mut send, &codec::encode(&Message::RelayConnected).unwrap()).await;
            // Phase 5b: assign this client a routing handle, stamp it on every
            // bridged server-bound stream (the real relay does this).
            let handle: u32 = 1; // a single client per test; any nonzero handle is fine
            loop {
                let (mut c_send, mut c_recv) = match client_conn.accept_bi().await {
                    Ok(s) => s, Err(_) => break,
                };
                let (mut s_send, mut s_recv) = match server_conn.open_bi().await {
                    Ok(s) => s, Err(_) => break,
                };
                s_send.write_all(&handle.to_be_bytes()).await.unwrap();
                tokio::spawn(async move { let _ = tokio::io::copy(&mut c_recv, &mut s_send).await; let _ = s_send.finish(); });
                tokio::spawn(async move { let _ = tokio::io::copy(&mut s_recv, &mut c_send).await; let _ = c_send.finish(); });
            }
        }
        None => {
            let err = Message::RelayError { reason: "destination not connected".to_string() };
            write_framed(&mut send, &codec::encode(&err).unwrap()).await;
            let _ = send.finish();
            client_conn.closed().await;
        }
    }
}
```

Run: `cargo test -p farder-server --test relay_mode`
Expected: FAIL — the server's `serve_relay_stream` reads a `RelayStreamRole` first, but now sees the 4-byte handle stamp, so decode fails and the relayed login/upload tests break. (This is the RED that Step 2 fixes.)

- [ ] **Step 2: Server reads the stamp before the role; threads the handle + relay conn**

In `crates/farder-server/src/relay.rs`:

`serve_relay_stream` reads the 4-byte handle first, then the role, and threads both the handle and the relay connection into the primary handler:

```rust
async fn serve_relay_stream(state: Arc<ServerState>, relay_conn: quinn::Connection, send: SendStream, mut recv: RecvStream) -> Result<()> {
    // Phase 5b: every bridged stream is prefixed with the relay-assigned 4-byte
    // routing handle (authoritative). Read it before the RelayStreamRole.
    let mut hb = [0u8; 4];
    recv.read_exact(&mut hb).await?;
    let handle = u32::from_be_bytes(hb);
    let role: RelayStreamRole = codec::decode(&read_framed(&mut recv).await?)?;
    match role {
        RelayStreamRole::Primary => run_relay_primary(state, relay_conn, handle, send, recv).await,
        RelayStreamRole::Session { token } => run_relay_aux(state, send, recv, token).await,
    }
}
```

`connect_and_serve` passes the relay `conn` into `serve_relay_stream`:

```rust
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
```

`run_relay_primary` registers the relayed `VoiceSink` + the handle->pk binding after auth, and removes both on cleanup:

```rust
async fn run_relay_primary(state: Arc<ServerState>, relay_conn: quinn::Connection, handle: u32, mut send: SendStream, mut recv: RecvStream) -> Result<()> {
    let outcome = authenticate(&state, &mut send, &mut recv).await?;
    let pk_bytes = outcome.pk_bytes;
    // Phase 5b: bind this relayed member to its routing handle for voice.
    state.voice_connections.write().await.insert(pk_bytes, crate::state::VoiceSink::Relayed { relay: relay_conn.clone(), handle });
    state.relay_voice_handles.write().await.insert(handle, pk_bytes);

    let loop_result = main_loop(
        Arc::clone(&state), outcome.public_key.clone(), outcome.is_owner, &mut send, &mut recv, outcome.event_rx,
    ).await;

    // Cleanup (cleanup_session does NOT touch voice_connections).
    state.voice_connections.write().await.remove(&pk_bytes);
    state.relay_voice_handles.write().await.remove(&handle);
    cleanup_session(&state, &outcome.public_key, outcome.pk_bytes, &outcome.event_tx, &outcome.session_token).await;
    loop_result
}
```

(`run_relay_aux` keeps its current signature; the handle read in `serve_relay_stream` is simply not passed to it.)

- [ ] **Step 3: Enable datagrams on the relay-client endpoint**

In `relay_client_endpoint`, after the `keep_alive_interval` line, add:

```rust
    transport.datagram_receive_buffer_size(Some(1 << 20));
    transport.datagram_send_buffer_size(1 << 20);
```

- [ ] **Step 4: Spawn the relay voice datagram loop**

In `connect_and_serve`, after registration succeeds (after the `info!("registered with relay ...")` line) and before the `accept_bi` loop, spawn a loop that reads voice datagrams on the relay connection, resolves the source handle to a pk, and processes the frame:

```rust
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
```

`process_inbound_voice_frame` is `pub(crate)` in `connection.rs` (Task 4), so `crate::connection::process_inbound_voice_frame` resolves. Update the `relay.rs` `//! ... Voice/datagrams are not served over the relay (deferred).` header comment to reflect that voice IS now served over the relay.

- [ ] **Step 5: Run relay_mode + voice + the whole server crate**

Run: `cargo test -p farder-server --test relay_mode`
Expected: PASS — the relayed login/upload/bad-token tests pass through the stamp (server reads `[handle][role]`).
Run: `cargo test -p farder-server`
Expected: PASS (direct voice + everything else green).

- [ ] **Step 6: Commit**

```bash
git add crates/farder-server/src/relay.rs crates/farder-server/tests/relay_mode.rs
git commit -m "Server: read handle stamp, register relayed voice sinks, run relay voice loop (Phase 5b)"
```

---

## Task 6: Docs + workspace gate

**Files:** Update `docs/modules/relay.md` and `docs/modules/server-relay.md`; run the full suite.

- [ ] **Step 1: Workspace gate**

Run: `cargo test --workspace`
Expected: PASS across all crates. If anything fails, STOP and report it (do not weaken tests).

- [ ] **Step 2: Update `docs/modules/relay.md`**

Edit `docs/modules/relay.md`: the relay now **stamps each bridged stream with the client's 4-byte handle** (authoritative correlation) and **no longer uses a control-stream announce** (the `RelayClientConnected/Disconnected` messages were removed). Update the "Datagram routing" and "Protocol messages" sections accordingly: remove the announce description and the two message names; add a sentence that each bridged stream is prefixed `[handle: u32 BE]` so the server binds the handle to the authenticated member. Keep the forward/route datagram description (unchanged).

- [ ] **Step 3: Update `docs/modules/server-relay.md`**

Edit `docs/modules/server-relay.md`: relay-mode voice is now supported. Add a short "Voice over relay (Phase 5b)" section: `serve_relay_stream` reads the 4-byte handle stamp before the `RelayStreamRole`; `run_relay_primary` registers a `VoiceSink::Relayed{relay, handle}` in `voice_connections` and `handle -> pk` in `relay_voice_handles`; `connect_and_serve` runs one voice datagram loop on the relay connection that demuxes by source handle and fans out via `process_inbound_voice_frame`; `relay_client_endpoint` enables datagrams. Note the client half (recv loop, datagram-enabled pinned endpoint, dropping the `voice_join` refusal, UI) is deferred to 5b-client and UNVERIFIED until a Windows + deployed-relay run. If the file's header says voice/datagrams are not served over the relay, fix that line.

- [ ] **Step 4: Commit**

```bash
git add docs/modules/relay.md docs/modules/server-relay.md
git commit -m "Docs: relay handle-stamp + server voice-over-relay (Phase 5b)"
```

---

## Final verification

- [ ] `cargo test --workspace` — all green (incl. `farder-relay` stamp/datagram tests, `farder-server` `voice` + `relay_mode`, the `VoiceSink` + `process_inbound_voice_frame` tests).
- [ ] `cargo build --workspace` — no warnings on the new code.
- [ ] Spec coverage: handle stamp (Task 1); announce removed + protocol cleaned (Task 2); `VoiceSink` (Task 3); extracted frame processing + fan-out-by-handle (Task 4); server reads stamp + relayed sink registration + relay voice loop + datagrams enabled (Task 5); docs (Task 6). Direct voice unchanged (regression gate, Tasks 3-5).
- [ ] **UNVERIFIED, by design:** real audio + the client datagram recv loop + the actual "member A hears member B over the relay" — deferred to 5b-client and the user's Windows + deployed-relay run.

After all tasks: use **superpowers:finishing-a-development-branch** to complete the work.
```

