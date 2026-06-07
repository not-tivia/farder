# Relay Phase 3a — Client Relay Connection — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the Tauri client connect to a relayed server *through the relay* (closing audit Gap #3 — the server sees only the relay's IP), with relay cert pinning and file transfers over the relay.

**Architecture:** Extract the client auth handshake into a `(send, recv)`-based function (mirroring the Phase 2 server refactor). Add `connect_via_relay`: connect to the relay (cert pinned), `RelayConnect{server_id}`, open a `Primary` stream marked with `RelayStreamRole::Primary`, then run the shared handshake. File transfers open `RelayStreamRole::Session{token}` streams. Direct mode is unchanged.

**Tech Stack:** Rust, quinn 0.11, rustls 0.23 (ring), rcgen (tests), farder-protocol (`RelayStreamRole`, `Message::RelayConnect` already exist), sha2.

**Spec:** `docs/superpowers/specs/2026-06-07-relay-phase3a-client-connection-design.md`

**Scope:** client crate (`client/src-tauri`) only. NO web invite directory (Phase 3b), NO relay UI (Phase 4). Voice over relay stays deferred.

**Hard invariant:** Direct-mode connection behavior unchanged. `connect_and_authenticate`'s observable behavior must be identical after the handshake extraction.

---

## File Structure

- `client/src-tauri/src/connection.rs` — extract `run_client_handshake`; add `RelayTarget` + `parse_relay_target`; add `connect_via_relay`.
- `client/src-tauri/src/tls.rs` — `make_pinned_relay_endpoint(fingerprint)` + `cert_fingerprint` helper + pinning verifier.
- `client/src-tauri/src/state.rs` — `ServerConnection { session_token: Vec<u8>, relayed: bool, ... }`.
- `client/src-tauri/src/commands.rs` — `connect_server` parses the relay form and branches; threads `session_token`/`relayed`; relay-aware upload/download; voice refusal.
- `client/src-tauri/Cargo.toml` — `rcgen` + `tempfile` dev-dependencies (for the test doubles).

---

## Task 1: Relay-target parser

**Files:** Modify `client/src-tauri/src/connection.rs`.

A relayed server's connection info is the escape-hatch link
`farder://relay/<relay_addr>/<server_id_hex>/<cert_fp_hex>/<invite_token>`.

- [ ] **Step 1: Write failing tests.** Add to `connection.rs` a `#[cfg(test)] mod tests` (or extend one):

```rust
#[cfg(test)]
mod tests {
    use super::*;

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
}
```

- [ ] **Step 2: Run to verify fail:** `cd ~/farder/client/src-tauri && cargo test parses_a_valid_relay_link 2>&1 | tail -12` — expect compile error (`RelayTarget`/`parse_relay_target` missing).

- [ ] **Step 3: Implement.** Add near the top of `connection.rs` (after the imports):

```rust
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
    let rest = s.strip_prefix("farder://relay/")?;
    let parts: Vec<&str> = rest.splitn(4, '/').collect();
    if parts.len() != 4 {
        return None;
    }
    let relay_addr: SocketAddr = parts[0].parse().ok()?;
    let server_id = hex::decode(parts[1]).ok()?;
    let cert_fp = hex::decode(parts[2]).ok()?;
    if server_id.is_empty() || cert_fp.is_empty() || parts[3].is_empty() {
        return None;
    }
    Some(RelayTarget {
        relay_addr,
        server_id,
        cert_fp,
        invite_token: parts[3].to_string(),
    })
}
```

(`hex` is already a dependency of `farder-client`.)

- [ ] **Step 4: Run to verify pass:** `cd ~/farder/client/src-tauri && cargo test parse_relay 2>&1 | tail -8` and `cargo test relay_link 2>&1 | tail -8` — expect PASS.

- [ ] **Step 5: Commit:**
```bash
cd ~/farder && git add client/src-tauri/src/connection.rs && \
git commit -m "client: relay-target link parser"
```

---

## Task 2: Pinned relay client endpoint

**Files:** Modify `client/src-tauri/src/tls.rs`.

Background: `tls.rs` has `make_client_endpoint()` (skip-verify, for direct dev servers) and a `SkipServerVerification` struct. Relay connections must PIN the relay's cert by SHA-256 fingerprint.

- [ ] **Step 1: Write a failing test** for the fingerprint helper. Append to `tls.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_sha256_of_der() {
        let der = [1u8, 2, 3, 4, 5];
        use sha2::{Digest, Sha256};
        let expected = Sha256::digest(der).to_vec();
        assert_eq!(cert_fingerprint(&der), expected);
        assert_eq!(cert_fingerprint(&der).len(), 32);
    }
}
```

- [ ] **Step 2: Run to verify fail:** `cd ~/farder/client/src-tauri && cargo test fingerprint_is_sha256 2>&1 | tail -10` — expect compile error.

- [ ] **Step 3: Implement.** Add to `tls.rs`:

```rust
/// SHA-256 fingerprint of a DER-encoded certificate (the value pinned in a
/// relay link).
pub fn cert_fingerprint(cert_der: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(cert_der).to_vec()
}

/// TLS verifier that accepts a server cert iff its SHA-256 fingerprint matches
/// the expected (pinned) value. Used for the relay hop so a network attacker
/// cannot impersonate the relay.
#[derive(Debug)]
struct PinnedVerification {
    expected_fp: Vec<u8>,
}

impl rustls::client::danger::ServerCertVerifier for PinnedVerification {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if cert_fingerprint(end_entity.as_ref()) == self.expected_fp {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("relay cert fingerprint mismatch".into()))
        }
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

/// Build a QUIC client endpoint that pins the relay's cert by fingerprint.
pub fn make_pinned_relay_endpoint(expected_fp: Vec<u8>) -> Result<Endpoint> {
    let crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedVerification { expected_fp }))
        .with_no_client_auth();
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(std::time::Duration::from_secs(15)));
    let mut client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?,
    ));
    client_config.transport_config(Arc::new(transport));
    let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}
```

(`sha2` is already a dependency. Confirm `Endpoint`/`Arc`/`Result` are imported in `tls.rs` — they are, used by `make_client_endpoint`.)

- [ ] **Step 4: Run to verify pass:** `cd ~/farder/client/src-tauri && cargo test fingerprint 2>&1 | tail -8` — expect PASS.

- [ ] **Step 5: Commit:**
```bash
cd ~/farder && git add client/src-tauri/src/tls.rs && \
git commit -m "client: pinned relay client endpoint (cert fingerprint)"
```

---

## Task 3: Extract `run_client_handshake` + add `connect_via_relay`

**Files:** Modify `client/src-tauri/src/connection.rs`.

- [ ] **Step 1: Extract the handshake.** Replace the body of `connect_and_authenticate` Steps 3-5 (`connection.rs:83-109`) with a call to a new function. Add:

```rust
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
```

And rewrite `connect_and_authenticate`'s tail (after `accept_bi`) to:

```rust
    let (mut send, mut recv) = conn
        .accept_bi()
        .await
        .context("failed to accept bi-stream from server")?;
    let session_token =
        run_client_handshake(&mut send, &mut recv, keypair, invite_code, setup_token).await?;
    Ok((conn, send, recv, session_token))
```

- [ ] **Step 2: Add `connect_via_relay`.** Add to `connection.rs` (needs `use farder_protocol::{messages::Message, server::RelayStreamRole};` — add to the existing `farder_protocol` import):

```rust
/// Connect to a relayed server THROUGH its relay. The relay bridges the client's
/// streams to the server, so the server only ever sees the relay's address
/// (closes Gap #3). The caller must pass an endpoint that pins the relay's cert
/// (see `tls::make_pinned_relay_endpoint`).
pub async fn connect_via_relay(
    endpoint: Endpoint,
    target: &RelayTarget,
    keypair: &Keypair,
    invite_code: Option<String>,
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

    // Open the primary stream, mark it Primary, then run the normal handshake.
    let (mut send, mut recv) = conn.open_bi().await.context("open primary stream")?;
    write_frame(&mut send, &codec::encode(&RelayStreamRole::Primary)?).await?;
    let session_token =
        run_client_handshake(&mut send, &mut recv, keypair, invite_code, setup_token).await?;
    Ok((conn, send, recv, session_token))
}
```

- [ ] **Step 3: Build + run existing tests:** `cd ~/farder/client/src-tauri && cargo build 2>&1 | tail -6 && cargo test connection:: 2>&1 | tail -8` — expect it builds and the Task 1 parser tests still pass. (Behavior of `connect_via_relay`/`run_client_handshake` over the network is verified in Task 6.)

- [ ] **Step 4: Commit:**
```bash
cd ~/farder && git add client/src-tauri/src/connection.rs && \
git commit -m "client: extract run_client_handshake; add connect_via_relay"
```

---

## Task 4: `ServerConnection` stores session token + relayed flag; `connect_server` branch

**Files:** Modify `client/src-tauri/src/state.rs`, `client/src-tauri/src/commands.rs`.

- [ ] **Step 1: Add fields to `ServerConnection`** (`state.rs`):

```rust
    /// Session token issued at login; presented on relay file-transfer streams.
    pub session_token: Vec<u8>,
    /// True if this connection is routed through a relay (so file streams need a
    /// RelayStreamRole::Session marker and voice is unavailable).
    pub relayed: bool,
```

- [ ] **Step 2: Branch `connect_server`** (`commands.rs:380`). The function currently parses `address` as a `SocketAddr` and calls `connect_and_authenticate`, discarding the token (`:399-406`). Change it so: if `connection::parse_relay_target(&address)` returns `Some(target)`, build a pinned endpoint and use `connect_via_relay`; otherwise the existing direct path. Capture the token + relayed flag. Replace the block around `:399-406`:

```rust
    let (endpoint, conn, send, recv, session_token, relayed) =
        if let Some(target) = crate::connection::parse_relay_target(&address) {
            let endpoint =
                crate::tls::make_pinned_relay_endpoint(target.cert_fp.clone()).map_err(|e| e.to_string())?;
            let (conn, send, recv, token) =
                crate::connection::connect_via_relay(endpoint.clone(), &target, &keypair, invite_code, setup_token)
                    .await
                    .map_err(|e| e.to_string())?;
            (endpoint, conn, send, recv, token, true)
        } else {
            let addr: std::net::SocketAddr = address
                .parse()
                .map_err(|e: std::net::AddrParseError| e.to_string())?;
            let endpoint = make_client_endpoint().map_err(|e| e.to_string())?;
            let (conn, send, recv, token) =
                connect_and_authenticate(endpoint.clone(), addr, &keypair, invite_code, setup_token)
                    .await
                    .map_err(|e| e.to_string())?;
            (endpoint, conn, send, recv, token, false)
        };
```

- [ ] **Step 3: Populate the new fields** where `ServerConnection { ... }` is built (a bit further down in `connect_server`). Add `session_token` and `relayed` to the struct literal:

```rust
        session_token,
        relayed,
```

(The variable `session_token` was previously `_session_token` and discarded — it is now bound and stored. Remove the leading underscore everywhere it was discarded.)

- [ ] **Step 4: Build + tests:** `cd ~/farder/client/src-tauri && cargo build 2>&1 | tail -6 && cargo test 2>&1 | tail -6` — expect builds, existing tests pass. (Also confirm any OTHER place that constructs `ServerConnection` — e.g. `create_local_server` — is updated with `session_token`/`relayed`; grep `ServerConnection {` and fix all constructions, using `relayed: false` + the local token for direct/local servers.)

- [ ] **Step 5: Commit:**
```bash
cd ~/farder && git add client/src-tauri/src/state.rs client/src-tauri/src/commands.rs && \
git commit -m "client: ServerConnection stores session token + relayed flag; connect_server relay branch"
```

**Note on saved-server reconnect (spec's "ServerEntry relay fields"):** no schema
change is needed. `connect_server` already persists its `address` argument via
`save_last_server` and keys `AppState.servers` by it. When the address IS the
relay link (`farder://relay/...`), reconnect on relaunch passes that same string
back into `connect_server`, which re-parses it via `parse_relay_target` and takes
the relay path again. So persisting the relay link as the address string covers
reconnect without adding fields to `ServerEntry`. (If a manual check shows the
saved address is normalized/validated as a `SocketAddr` somewhere that would
reject a relay link, relax that one spot to allow the relay form.)

---

## Task 5: Relay-aware file transfer + voice refusal

**Files:** Modify `client/src-tauri/src/commands.rs`. Possibly `client/src-tauri/src/voice/*` or wherever voice-join is initiated.

- [ ] **Step 1: Relay-aware upload.** In `upload_file_internal_with_channel` (`commands.rs:776`), right after `let (mut send, mut recv) = quic_conn.open_bi()...` (`:807`), before sending `UploadRequest`, add the Session marker when relayed:

```rust
    if conn.relayed {
        let role = farder_protocol::server::RelayStreamRole::Session { token: conn.session_token.clone() };
        crate::connection::write_frame(&mut send, &farder_protocol::codec::encode(&role).map_err(|e| e.to_string())?)
            .await
            .map_err(|e| e.to_string())?;
    }
```

(`conn` is the `Arc<ServerConnection>` from `state.get_server(server_id)` already bound at `:805`.)

- [ ] **Step 2: Relay-aware download.** In `download_file_internal` (`commands.rs:877`), right after `let (mut send, mut recv) = quic_conn.open_bi()...` (`:884`), before sending `DownloadRequest`, add the same Session-marker block (using `conn.relayed`/`conn.session_token`; `conn` is bound at `:882`).

- [ ] **Step 3: Voice refusal.** Find where a voice join is initiated for a server connection (grep `voice_join` / `join_voice` / the command that starts a voice call — likely `commands.rs` `voice_join`). At the start of that path, refuse if the connection is relayed:

```rust
    if conn.relayed {
        return Err("voice is not available over a relay yet".to_string());
    }
```

(Place it after the `ServerConnection` is fetched. If voice-join does not have a `ServerConnection` handle readily, add the check at the nearest point where the relayed flag is known. Keep it minimal — a clear error, no datagram attempt.)

- [ ] **Step 4: Build + tests:** `cd ~/farder/client/src-tauri && cargo build 2>&1 | tail -6 && cargo test 2>&1 | tail -6` — expect builds + tests pass.

- [ ] **Step 5: Commit:**
```bash
cd ~/farder && git add client/src-tauri/src/commands.rs && \
git commit -m "client: relay-aware file transfer (Session marker) + voice refused over relay"
```

---

## Task 6: Integration tests — Gap #3 via doubles

**Files:** Modify `client/src-tauri/src/connection.rs` (add a `#[cfg(test)]` integration module — the client is a binary crate, so in-module tests can call `connect_via_relay`). Modify `client/src-tauri/Cargo.toml` (dev-deps).

This drives the REAL `connect_via_relay` through a relay double to a mock server, mirroring `crates/farder-server/tests/relay_mode.rs` (read it for the relay-double + framing patterns — but here the SERVER is the double and the CLIENT is real).

- [ ] **Step 1: Add dev-deps.** In `client/src-tauri/Cargo.toml` `[dev-dependencies]`, add (if absent):
```toml
rcgen = "0.13"
tempfile = "3"
```

- [ ] **Step 2: Write the harness + Gap #3 test.** Add a `#[cfg(test)] mod relay_it` to `connection.rs`. It needs:
  - `ensure_provider()` — `let _ = rustls::crypto::ring::default_provider().install_default();`
  - a **relay double**: a quinn server endpoint (self-signed via rcgen) that, per connection, reads the first bi-stream's first message; on `Message::RelayRegister{server_id}` stores the connection; on `Message::RelayConnect{server_id}` replies `RelayConnected` and bridges each subsequent client bi-stream to a fresh `open_bi` on the registered server connection (copy both ways, finish on EOF). (Lift this ~40 lines from `crates/farder-server/tests/relay_mode.rs`.) Capture the relay's cert DER so the test can compute its fingerprint for pinning.
  - a **mock server**: a quinn client endpoint (skip-verify) that dials the relay double, registers (`RelayRegister{server_id}` → `RelayRegistered`), then per accepted bridged bi-stream reads the `RelayStreamRole`; for `Primary` runs the SERVER side of the handshake (send `ServerFrame::Challenge{nonce}`, read the `ClientFrame::Authenticate`, verify or accept the signature, send `ServerFrame::Authenticated{session_token}`) and records the `remote_address()` it observed for that connection; for `Session{token}` records the token (used by the file test).

```rust
#[cfg(test)]
mod relay_it {
    use super::*;
    // ... ensure_provider(), relay double, mock server (see guidance above) ...

    #[tokio::test]
    async fn client_connects_via_relay_and_server_sees_relay_addr() {
        ensure_provider();
        // 1. start relay double on 127.0.0.1:0 (ephemeral); capture its cert DER + addr.
        // 2. start mock server; it registers with the relay under server_id and records
        //    the remote_address of the connection it serves.
        // 3. real client: make_pinned_relay_endpoint(sha256(relay_cert_der));
        //    parse_relay_target("farder://relay/<relay_addr>/<server_id_hex>/<fp_hex>/inv")
        //    -> connect_via_relay(...). Assert it returns Ok with a session token.
        // 4. ASSERT: the mock server's recorded remote_address == the RELAY's address
        //    (the relay's client-endpoint addr), NOT the real client's endpoint addr.
        //    This is the Gap #3 observation: the server never sees the client's IP.
    }
}
```

Fill in the harness fully (model the relay double + framing on `relay_mode.rs`). Keep assertions concrete: the test must FAIL if `connect_via_relay` connected directly to the server (it can't — there's no direct server) or if the role markers were wrong (the mock server's handshake would not complete).

- [ ] **Step 3: Run it:** `cd ~/farder/client/src-tauri && cargo test relay_it 2>&1 | tail -25` — expect the Gap #3 test passes. Debug real async/QUIC as needed; do not weaken the assertion that the server sees the relay's address.

- [ ] **Step 4: Add the cert-pinning test.** Add a test that `connect_via_relay` with a WRONG fingerprint (e.g. `make_pinned_relay_endpoint(vec![0u8;32])`) FAILS the relay handshake (the pinned verifier rejects the relay's real cert), while the correct fingerprint succeeds. Run it; expect pass.

- [ ] **Step 5: Add the file-over-relay test.** After a successful `connect_via_relay`, open a bi-stream on the returned connection, write `RelayStreamRole::Session{token}` then an `UploadRequest`, and assert the mock server received the `Session` marker with the correct token followed by the upload request (the mock server records what it got on Session streams). This proves the client's relay file-stream framing. Run it; expect pass.

- [ ] **Step 6: Commit:**
```bash
cd ~/farder && git add client/src-tauri/src/connection.rs client/src-tauri/Cargo.toml && \
git commit -m "client: integration tests — connect via relay closes Gap #3 (server sees relay addr) + pinning + file framing"
```

---

## Final verification

- [ ] **Client crate green:** `cd ~/farder/client/src-tauri && cargo test 2>&1 | tail -10` — all pass (parser, pinning, relay integration). `cargo build 2>&1 | tail -3`.
- [ ] **Frontend type-check unaffected:** `cd ~/farder/client && npx tsc --noEmit` — no errors (this phase is Rust-only, but confirm nothing broke).
- [ ] **Workspace untouched:** `cd ~/farder && cargo test --workspace 2>&1 | tail -6` — server/relay/protocol still green (this phase doesn't change them; the `RelayStreamRole`/`RelayConnect` types it consumes are unchanged).
- [ ] **Update the audit + spec status:** flip audit Gap #3 in `docs/superpowers/audits/2026-06-05-privacy-security-wiring-audit.md` to **FIXED (client closes the loop; observation test asserts the server sees the relay's address)**, and mark Phase 3a done in `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md`. Add/extend a module doc for the client relay path.
- [ ] **Finish the branch:** use superpowers:finishing-a-development-branch.

## Notes for the implementer
- Direct mode is sacred: `connect_and_authenticate`'s behavior must be unchanged by the handshake extraction. The existing client tests + a clean build are the guard.
- The Gap #3 test uses doubles because `farder-client` cannot depend on `farder-server`. The REAL server's relay behaviour is already proven by Phase 2's `relay_mode.rs`; this test proves the CLIENT half and the end-to-end observation (server sees relay addr).
- Voice over relay stays deferred — only the refusal is added here.
- The full GUI flow (opening a relay invite from the UI) is Phase 3b/4; 3a is verified headlessly.
