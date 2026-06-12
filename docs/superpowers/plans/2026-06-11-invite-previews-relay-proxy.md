# Invite Previews via Relay Fetch Proxy — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Invite cards (and the join dialog) show server name + member/online counts, fetched through the relay so viewer IPs never touch server hosts.

**Architecture:** Servers answer a new pre-auth, invite-code-gated `GetInvitePreview` question (sent instead of `Authenticate` after the Challenge). The relay gains a `ProxyInvitePreview` first-message request: for a registered (relayed) server it opens a stream on the server's registration connection stamped with the RESERVED handle 0 (relay-originated marker); for a direct server it dials out with a permissive QUIC client endpoint (matching how Farder clients already trust direct servers). Guardrails: per-IP preview rate limit, 5s timeout, SSRF refusal of non-global addresses, 16KB answer cap, 60s TTL cache. The client gets one Tauri command `get_invite_preview(link)` that picks the right relay, and the frontend renders loading/preview/invalid/unavailable states.

**Tech Stack:** Rust (quinn QUIC, rmp_serde via `farder_protocol::codec`), Tauri, React/TypeScript.

**Spec:** `docs/superpowers/specs/2026-06-11-invite-previews-relay-proxy-design.md`

**Branch:** create `invite-previews` from `main` before Task 1. Finish with ff-merge + push per project workflow. NOTE: this feature requires a relay REDEPLOY on the VPS afterward (owner-driven).

**Verified codebase facts (read before writing this plan — trust these):**
- Relay first-message dispatch: `crates/farder-relay/src/router.rs` `handle_connection` (~line 68) matches `Message::RelayRegister`/`RelayConnect`; `read_message`/`write_message` are 4-byte BE length + `codec` (rmp_serde) frames; `RelayState { servers, clients, next_handle }`; handle 0 reserved (never assigned, `next_handle` starts at 1).
- Relay tests (`router.rs` `mod tests`) have `start_relay()`, `test_client_endpoint()`, `SkipVerify`, `register_echo_server` — real-QUIC doubles.
- `crates/farder-relay/src/limits.rs` `ConnectionLimiter::new(max_connections, rate_per_window, window)` + `try_admit(ip, now) -> Option<ConnectionGuard>`.
- Server auth: `crates/farder-server/src/connection.rs` `authenticate(state, send, recv)` sends `ServerFrame::Challenge` then matches `recv_client_frame`; non-Authenticate frames currently hit a catch-all that sends `AuthError` and bails. `send_server_frame` exists. `ServerState` has `server_name: String`, `clients: RwLock<HashMap<[u8;32], EventSender>>`, `db: Mutex<Connection>`.
- `crates/farder-server/src/invites.rs:73` `pub fn validate_invite(conn: &Connection, code: &str) -> Result<Result<InviteInfo, String>>` (NESTED result: outer = DB error, inner = validity).
- Relay-mode server: `crates/farder-server/src/relay.rs` `serve_relay_stream` reads 4-byte stamp, currently `ensure!(handle != 0)`, then framed `RelayStreamRole`; `run_relay_primary` calls `authenticate` then binds voice handle.
- Server-side integration harness: `crates/farder-server/tests/relay_mode.rs` — relay DOUBLE (`start_relay() -> SocketAddr`, `relay_register` stores server conns in a `ConnectionMap`), `start_relay_server` boots a REAL relay-mode server, `login_primary` does the real handshake, `request()` helper sends ServerRequests; `write_framed`/`read_framed` helpers.
- Client: `client/src-tauri/src/connection.rs` `parse_relay_target(&str) -> Option<RelayTarget { relay_addr, server_id, cert_fp, invite_token }>` handles BOTH `farder://relay/<addr>/<sid>/<fp>/<token>` and compact `farder://relayd/<sid>/<token>` (expands default relay); `write_frame`/`read_frame` are `pub`; direct connections use SNI `"farder-server"`; relay connections use SNI `"farder-relay"`. `client/src-tauri/src/tls.rs:122` `pub fn make_pinned_relay_endpoint(expected_fp: Vec<u8>) -> Result<Endpoint>`. `client/src-tauri/src/default_relay.rs` `default_relay() -> Option<(SocketAddr, Vec<u8>)>`.
- Frontend: `client/src/components/InviteEmbed.tsx` (badge + Join only), `client/src/components/JoinConfirmModal.tsx` (has `relayed` prop), `client/src/lib/invite.ts` `parseInviteLink`. Themes: `client/src/themes/{discord-dark,xp-luna-blue,hello-kitty}/theme.css`, no default styling, vars only.
- Protocol rollout rule: new enum variants = update servers/relay before clients; preview paths are throwaway connections so version skew degrades to "unavailable" (NEVER a reconnect loop — disco-ball lesson 2026-06-11: any auto-fired request must fail quiet and once).

---

### Task 1: Protocol additions

**Files:**
- Modify: `crates/farder-protocol/src/messages.rs`
- Modify: `crates/farder-protocol/src/server.rs`

- [ ] **Step 1: Write the failing tests.** In `messages.rs` `mod tests` append:

```rust
    #[test]
    fn test_roundtrip_proxy_invite_preview() {
        let msg = Message::ProxyInvitePreview {
            target: PreviewTarget::Registered { server_id: vec![1u8; 32] },
            code: "AbCd1234".to_string(),
        };
        let encoded = codec::encode(&msg).expect("encode failed");
        match codec::decode::<Message>(&encoded).expect("decode failed") {
            Message::ProxyInvitePreview { target: PreviewTarget::Registered { server_id }, code } => {
                assert_eq!(server_id, vec![1u8; 32]);
                assert_eq!(code, "AbCd1234");
            }
            other => panic!("wrong variant: {other:?}"),
        }

        let msg = Message::ProxyInvitePreview {
            target: PreviewTarget::Direct { addr: "203.0.113.7:4433".to_string() },
            code: "x".to_string(),
        };
        let encoded = codec::encode(&msg).unwrap();
        assert!(matches!(
            codec::decode::<Message>(&encoded).unwrap(),
            Message::ProxyInvitePreview { target: PreviewTarget::Direct { .. }, .. }
        ));

        for outcome in [
            PreviewOutcome::Preview { server_name: "The Spot".into(), member_count: 12, online_count: 3 },
            PreviewOutcome::Invalid,
            PreviewOutcome::Unavailable,
        ] {
            let msg = Message::ProxyInvitePreviewResult { outcome: outcome.clone() };
            let encoded = codec::encode(&msg).unwrap();
            match codec::decode::<Message>(&encoded).unwrap() {
                Message::ProxyInvitePreviewResult { outcome: o } => assert_eq!(o, outcome),
                other => panic!("wrong variant: {other:?}"),
            }
        }
    }
```

In `server.rs` `mod tests` append:

```rust
    #[test]
    fn test_invite_preview_frames_roundtrip() {
        let f = ClientFrame::GetInvitePreview { code: "AbCd1234".to_string() };
        let bytes = codec::encode(&f).unwrap();
        match codec::decode::<ClientFrame>(&bytes).unwrap() {
            ClientFrame::GetInvitePreview { code } => assert_eq!(code, "AbCd1234"),
            other => panic!("wrong variant: {other:?}"),
        }

        let f = ServerFrame::InvitePreview { server_name: "The Spot".into(), member_count: 12, online_count: 3 };
        let bytes = codec::encode(&f).unwrap();
        match codec::decode::<ServerFrame>(&bytes).unwrap() {
            ServerFrame::InvitePreview { server_name, member_count, online_count } => {
                assert_eq!(server_name, "The Spot");
                assert_eq!(member_count, 12);
                assert_eq!(online_count, 3);
            }
            other => panic!("wrong variant: {other:?}"),
        }

        let f = ServerFrame::InvitePreviewError { reason: "invalid".into() };
        let bytes = codec::encode(&f).unwrap();
        assert!(matches!(codec::decode::<ServerFrame>(&bytes).unwrap(), ServerFrame::InvitePreviewError { .. }));
    }
```

(Both test mods already import `codec` — match the file's existing test imports.)

- [ ] **Step 2:** `cargo test -p farder-protocol test_roundtrip_proxy_invite_preview test_invite_preview_frames_roundtrip` → compile FAILURE (unknown variants/types).

- [ ] **Step 3: Add the types.** In `messages.rs`, above the `Message` enum:

```rust
/// What the relay should query for an invite preview.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum PreviewTarget {
    /// A server registered with THIS relay (relayed server).
    Registered { server_id: Vec<u8> },
    /// A direct server the relay should dial on the requester's behalf.
    Direct { addr: String },
}

/// Result of an invite-preview lookup, as relayed back to the requester.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum PreviewOutcome {
    Preview { server_name: String, member_count: u32, online_count: u32 },
    /// The server answered: the code is invalid/expired/exhausted. Uniform on
    /// purpose — invalid codes reveal nothing about the server.
    Invalid,
    /// Timeout, dial failure, SSRF refusal, rate-limit refusal, or an
    /// undecodable answer.
    Unavailable,
}
```

Append to the `Message` enum (after `DmFileComplete`):

```rust
    /// Ask the relay to fetch an invite preview on the requester's behalf
    /// (relay fetch proxy, phase one). First message on a fresh connection.
    ProxyInvitePreview { target: PreviewTarget, code: String },
    ProxyInvitePreviewResult { outcome: PreviewOutcome },
```

In `server.rs`: append to `ClientFrame`:

```rust
    /// Pre-auth invite preview: sent INSTEAD of Authenticate after the
    /// Challenge. Valid-code-gated; the connection is throwaway.
    GetInvitePreview { code: String },
```

Append to `ServerFrame` (after `AuthError`):

```rust
    InvitePreview { server_name: String, member_count: u32, online_count: u32 },
    /// Uniform for invalid/expired/exhausted codes — reveals nothing.
    InvitePreviewError { reason: String },
```

- [ ] **Step 4:** `cargo test -p farder-protocol` (all green) and `cargo build --workspace` (the server's `authenticate` match has a catch-all `_ =>` arm so no constructor breaks; if anything else fails to compile, fix the match exhaustively with inert arms and note it). Also `cd client/src-tauri && cargo build`.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-protocol/src/messages.rs crates/farder-protocol/src/server.rs
git commit -m "protocol: invite-preview frames and relay proxy messages"
```

---

### Task 2: Server answers GetInvitePreview (+ accept relay-originated handle 0)

**Files:**
- Modify: `crates/farder-server/src/connection.rs` (the `authenticate` fn's client_frame match)
- Modify: `crates/farder-server/src/relay.rs` (`serve_relay_stream`, `run_relay_primary`)
- Test: `crates/farder-server/tests/relay_mode.rs`

- [ ] **Step 1: Write the failing integration test.** In `tests/relay_mode.rs`: first, make the relay double's connection map reachable — change `start_relay()` to return `(SocketAddr, ConnectionMap)` (it already owns the map; clone the Arc out) and update the existing call sites (`let (relay, _conns) = start_relay().await;`). Then append:

```rust
/// Act as the RELAY itself: open a preview stream on the server's registration
/// connection — stamped with reserved handle 0 — and ask GetInvitePreview.
async fn relay_preview(server_conn: &Connection, code: &str) -> ServerFrame {
    let (mut s, mut r) = server_conn.open_bi().await.unwrap();
    s.write_all(&0u32.to_be_bytes()).await.unwrap(); // relay-originated marker
    write_framed(&mut s, &codec::encode(&RelayStreamRole::Primary).unwrap()).await;
    // Server speaks Challenge first; a preview client ignores the nonce.
    let frame: ServerFrame = codec::decode(&read_framed(&mut r).await).unwrap();
    assert!(matches!(frame, ServerFrame::Challenge { .. }), "expected Challenge first");
    let ask = ClientFrame::GetInvitePreview { code: code.to_string() };
    write_framed(&mut s, &codec::encode(&ask).unwrap()).await;
    codec::decode(&read_framed(&mut r).await).unwrap()
}

#[tokio::test]
async fn invite_preview_over_relay_stamp_zero() {
    ensure_provider();
    let (relay, conns) = start_relay().await;
    let (server_id, _state) = start_relay_server(relay).await;

    // Owner logs in (auto-claim) and creates an invite — the only valid code.
    let kp = Keypair::generate();
    let (_ep, conn) = client_via_relay(relay, &server_id).await;
    let (mut send, mut recv, _token) = login_primary(&conn, &kp).await;
    let resp = request(&mut send, &mut recv, 1, ServerRequest::CreateInvite {
        max_uses: None, expires_in_secs: None, target_channel: None,
    }).await;
    let code = match resp {
        ServerResponse::InviteCreated { code } => code,
        other => panic!("expected InviteCreated, got {other:?}"),
    };

    // Grab the server's registration connection from the relay double's map.
    let server_conn = {
        let map = conns.lock().await;
        map.get(server_id.as_slice()).expect("server registered").clone()
    };

    // Valid code → preview with name + counts (owner is the 1 member, online).
    match relay_preview(&server_conn, &code).await {
        ServerFrame::InvitePreview { server_name, member_count, online_count } => {
            assert!(!server_name.is_empty(), "preview must carry the server name");
            assert_eq!(member_count, 1, "owner is the only member");
            assert_eq!(online_count, 1, "owner is connected");
        }
        other => panic!("expected InvitePreview, got {other:?}"),
    }

    // Invalid code → uniform error, nothing leaked.
    match relay_preview(&server_conn, "ZZZZZZZZ").await {
        ServerFrame::InvitePreviewError { reason } => assert_eq!(reason, "invalid"),
        other => panic!("expected InvitePreviewError, got {other:?}"),
    }

    // The preview never registered a member: count is still 1.
    let resp = request(&mut send, &mut recv, 2, ServerRequest::GetMembers).await;
    match resp {
        ServerResponse::Members { members } => assert_eq!(members.len(), 1),
        other => panic!("expected Members, got {other:?}"),
    }
}
```

(Adapt the `ConnectionMap` access pattern to its actual type — read the harness top; it may be `Arc<Mutex<HashMap<Vec<u8>, Connection>>>` with `.lock().await` for tokio Mutex or `.lock().unwrap()` for std. Match what `relay_register`/`relay_connect` do.)

- [ ] **Step 2:** `cargo test -p farder-server --test relay_mode invite_preview_over_relay_stamp_zero` → FAIL: today the server REJECTS handle 0 (`relay sent reserved routing handle 0`), so `relay_preview` times out/errors before any frame.

- [ ] **Step 3: Accept handle 0 for Primary streams.** In `crates/farder-server/src/relay.rs` `serve_relay_stream`, REMOVE the blanket `anyhow::ensure!(handle != 0, ...)` and gate per-role:

```rust
    let mut hb = [0u8; 4];
    recv.read_exact(&mut hb).await?;
    let handle = u32::from_be_bytes(hb);
    let role: RelayStreamRole = codec::decode(&read_framed(&mut recv).await?)?;
    match role {
        // Handle 0 = RELAY-ORIGINATED stream (invite preview). Real client
        // streams always carry a relay-stamped handle >= 1 (the relay stamps
        // authoritatively; clients cannot forge the prefix). Preview streams
        // never authenticate, so 0 is rejected at the auth/voice-binding point
        // in run_relay_primary instead of here.
        RelayStreamRole::Primary => run_relay_primary(state, relay_conn, handle, send, recv).await,
        RelayStreamRole::Session { token } => {
            anyhow::ensure!(handle != 0, "relay sent reserved routing handle 0");
            run_relay_aux(state, send, recv, token).await
        }
    }
```

And in `run_relay_primary`, immediately AFTER `let outcome = authenticate(...).await?;` add:

```rust
    // A relay-originated (handle 0) stream must never reach an authenticated
    // session: previews bail inside authenticate(). Belt-and-braces.
    anyhow::ensure!(handle != 0, "reserved handle 0 cannot authenticate");
```

- [ ] **Step 4: Answer the preview in `authenticate`.** In `crates/farder-server/src/connection.rs`, in `authenticate`'s `match client_frame`, add an arm ABOVE the catch-all `_ =>` arm:

```rust
        ClientFrame::GetInvitePreview { code } => {
            // Pre-auth, code-gated preview (relay fetch proxy phase one). The
            // connection is throwaway: answer one frame and bail out of auth.
            let valid_member_count = {
                let conn_db = state.db.lock().unwrap();
                match invites::validate_invite(&conn_db, &code)? {
                    Ok(_info) => Some(members::list_members(&conn_db)?.len() as u32),
                    Err(_reason) => None, // uniform: invalid/expired/exhausted all look the same
                }
            };
            match valid_member_count {
                Some(member_count) => {
                    let online_count = state.clients.read().await.len() as u32;
                    send_server_frame(send, &ServerFrame::InvitePreview {
                        server_name: state.server_name.clone(),
                        member_count,
                        online_count,
                    }).await?;
                }
                None => {
                    send_server_frame(send, &ServerFrame::InvitePreviewError {
                        reason: "invalid".to_string(),
                    }).await?;
                }
            }
            anyhow::bail!("served invite preview (throwaway connection, not an auth failure)");
        }
```

(`invites` and `members` are already imported at the top of connection.rs — verify; the nested `validate_invite` result is `Result<Result<InviteInfo, String>>`: `?` the outer, match the inner. The `bail!` ends the connection task — both direct and relay callers treat stream errors as a closed stream, which is exactly the throwaway behavior wanted. The bail message makes server logs self-explanatory.)

- [ ] **Step 5:** `cargo test -p farder-server --test relay_mode` (4 tests incl. the new one, all green) and `cargo test -p farder-server` (all green — the changed handle-0 gating must not break the existing 3 relay_mode tests or unit tests).

- [ ] **Step 6: Commit**

```bash
git add crates/farder-server/src/connection.rs crates/farder-server/src/relay.rs crates/farder-server/tests/relay_mode.rs
git commit -m "server: answer pre-auth GetInvitePreview; accept relay-originated handle-0 preview streams"
```

---

### Task 3: Relay proxy module (fetch, SSRF guard, cache, rate limit)

**Files:**
- Create: `crates/farder-relay/src/proxy.rs`
- Modify: `crates/farder-relay/src/main.rs` (mod decl + context construction)
- Modify: `crates/farder-relay/src/router.rs` (dispatch arm + context plumbing)

- [ ] **Step 1: Create `proxy.rs`** with unit-testable pieces and their tests:

```rust
//! Invite-preview fetch proxy (relay fetch proxy, phase one). The relay asks a
//! target server "what's behind this invite code?" on a requester's behalf so
//! the requester's IP never touches the server host. Guardrails: per-IP rate
//! limit (router-level), 5s timeout, SSRF refusal of non-global addresses,
//! 16KB answer cap, 60s TTL cache.

use anyhow::Result;
use farder_protocol::codec;
use farder_protocol::messages::{PreviewOutcome, PreviewTarget};
use farder_protocol::server::{ClientFrame, RelayStreamRole, ServerFrame};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::debug;

pub const PREVIEW_TIMEOUT: Duration = Duration::from_secs(5);
const ANSWER_CAP: usize = 16 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(60);
const CACHE_MAX_ENTRIES: usize = 1024;

// ---------------------------------------------------------------------------
// SSRF guard
// ---------------------------------------------------------------------------

/// Only globally-routable addresses may be dialed on a requester's behalf —
/// the relay must not be usable to probe its own host or private networks.
pub fn is_global_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64 // 100.64/10 CGNAT
                || v4.octets() == [192, 0, 0, 0]
                )
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            !(v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || (seg[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
                || (seg[0] & 0xffc0) == 0xfe80 // fe80::/10 link local
                )
        }
    }
}

// ---------------------------------------------------------------------------
// TTL cache
// ---------------------------------------------------------------------------

pub struct PreviewCache {
    entries: Mutex<HashMap<(String, String), (Instant, PreviewOutcome)>>,
}

impl PreviewCache {
    pub fn new() -> Self {
        Self { entries: Mutex::new(HashMap::new()) }
    }

    pub fn get(&self, key: &(String, String), now: Instant) -> Option<PreviewOutcome> {
        let map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        map.get(key).and_then(|(at, v)| {
            (now.duration_since(*at) < CACHE_TTL).then(|| v.clone())
        })
    }

    pub fn put(&self, key: (String, String), value: PreviewOutcome, now: Instant) {
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= CACHE_MAX_ENTRIES {
            // Cheap pressure valve: drop expired entries; if still full, start over.
            map.retain(|_, (at, _)| now.duration_since(*at) < CACHE_TTL);
            if map.len() >= CACHE_MAX_ENTRIES {
                map.clear();
            }
        }
        map.insert(key, (now, value));
    }
}

pub fn cache_key(target: &PreviewTarget, code: &str) -> (String, String) {
    let t = match target {
        PreviewTarget::Registered { server_id } => format!("r:{}", hex::encode(server_id)),
        PreviewTarget::Direct { addr } => format!("d:{}", addr),
    };
    (t, code.to_string())
}

// ---------------------------------------------------------------------------
// Outbound QUIC endpoint (direct-server dials)
// ---------------------------------------------------------------------------

/// Permissive client endpoint for dialing DIRECT farder servers — they use
/// self-signed certs and the Farder client itself accepts them the same way,
/// so this matches the ecosystem's existing trust model for direct connects.
pub fn outbound_endpoint() -> Result<Endpoint> {
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
        .with_custom_certificate_verifier(std::sync::Arc::new(SkipVerify))
        .with_no_client_auth();
    let cfg = quinn::ClientConfig::new(std::sync::Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?,
    ));
    let mut ep = Endpoint::client("0.0.0.0:0".parse()?)?;
    ep.set_default_client_config(cfg);
    Ok(ep)
}

// ---------------------------------------------------------------------------
// The fetch itself
// ---------------------------------------------------------------------------

async fn read_capped(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    anyhow::ensure!(len <= ANSWER_CAP, "preview answer too large: {} bytes", len);
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn write_framed(send: &mut SendStream, data: &[u8]) -> Result<()> {
    send.write_all(&(data.len() as u32).to_be_bytes()).await?;
    send.write_all(data).await?;
    Ok(())
}

/// Speak the preview exchange on an established (send, recv) pair the server
/// treats as a primary/control stream: Challenge → GetInvitePreview → answer.
async fn ask_server(send: &mut SendStream, recv: &mut RecvStream, code: &str) -> Result<PreviewOutcome> {
    let first: ServerFrame = match codec::decode(&read_capped(recv).await?) {
        Ok(f) => f,
        Err(_) => return Ok(PreviewOutcome::Unavailable),
    };
    if !matches!(first, ServerFrame::Challenge { .. }) {
        return Ok(PreviewOutcome::Unavailable);
    }
    write_framed(send, &codec::encode(&ClientFrame::GetInvitePreview { code: code.to_string() })?).await?;
    match codec::decode::<ServerFrame>(&read_capped(recv).await?) {
        Ok(ServerFrame::InvitePreview { server_name, member_count, online_count }) => {
            Ok(PreviewOutcome::Preview { server_name, member_count, online_count })
        }
        Ok(ServerFrame::InvitePreviewError { .. }) => Ok(PreviewOutcome::Invalid),
        _ => Ok(PreviewOutcome::Unavailable),
    }
}

/// Resolve and fetch a preview for `target`. Errors collapse to Unavailable at
/// the call site; this returns Result for `?` ergonomics on transport ops.
pub async fn fetch_preview(
    target: &PreviewTarget,
    code: &str,
    registered: Option<Connection>,
    out_endpoint: &Endpoint,
) -> PreviewOutcome {
    let attempt: Result<PreviewOutcome> = async {
        match target {
            PreviewTarget::Registered { .. } => {
                let Some(server_conn) = registered else {
                    return Ok(PreviewOutcome::Unavailable);
                };
                let (mut s, mut r) = server_conn.open_bi().await?;
                // Reserved handle 0 marks the stream as relay-originated.
                s.write_all(&0u32.to_be_bytes()).await?;
                write_framed(&mut s, &codec::encode(&RelayStreamRole::Primary)?).await?;
                ask_server(&mut s, &mut r, code).await
            }
            PreviewTarget::Direct { addr } => {
                let sock: SocketAddr = match addr.parse() {
                    Ok(s) => s,
                    Err(_) => return Ok(PreviewOutcome::Unavailable),
                };
                if !is_global_ip(sock.ip()) {
                    debug!("preview refused: non-global address {}", sock);
                    return Ok(PreviewOutcome::Unavailable);
                }
                let conn = out_endpoint.connect(sock, "farder-server")?.await?;
                let (mut s, mut r) = conn.open_bi().await?;
                let outcome = ask_server(&mut s, &mut r, code).await;
                conn.close(0u32.into(), b"preview done");
                outcome
            }
        }
    }
    .await;
    attempt.unwrap_or(PreviewOutcome::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssrf_guard_refuses_non_global() {
        for bad in [
            "127.0.0.1", "10.0.0.1", "172.16.5.5", "192.168.1.1", "169.254.0.7",
            "0.0.0.0", "100.64.0.1", "::1", "fe80::1", "fc00::1", "::",
        ] {
            assert!(!is_global_ip(bad.parse().unwrap()), "{bad} must be refused");
        }
        for good in ["203.0.113.7", "45.77.70.199", "2607:f8b0::1"] {
            assert!(is_global_ip(good.parse().unwrap()), "{good} must be allowed");
        }
    }

    #[test]
    fn cache_ttl_and_pressure() {
        let cache = PreviewCache::new();
        let t0 = Instant::now();
        let key = ("r:aa".to_string(), "code".to_string());
        assert!(cache.get(&key, t0).is_none());
        cache.put(key.clone(), PreviewOutcome::Invalid, t0);
        assert_eq!(cache.get(&key, t0), Some(PreviewOutcome::Invalid));
        // Within TTL.
        assert!(cache.get(&key, t0 + Duration::from_secs(59)).is_some());
        // Expired.
        assert!(cache.get(&key, t0 + Duration::from_secs(61)).is_none());
    }

    #[test]
    fn cache_key_distinguishes_targets_and_codes() {
        let a = cache_key(&PreviewTarget::Registered { server_id: vec![1] }, "x");
        let b = cache_key(&PreviewTarget::Registered { server_id: vec![2] }, "x");
        let c = cache_key(&PreviewTarget::Direct { addr: "1.2.3.4:1".into() }, "x");
        let d = cache_key(&PreviewTarget::Registered { server_id: vec![1] }, "y");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }
}
```

(`hex` — check `crates/farder-relay/Cargo.toml`; add `hex = "0.4"` if absent, it's a workspace-wide dep elsewhere.)

- [ ] **Step 2:** Add `mod proxy;` to `main.rs`. Run `cargo test -p farder-relay proxy::` → 3 unit tests green.

- [ ] **Step 3: Wire dispatch in `router.rs`.** Add a context that travels with `serve`:

At the top of router.rs:

```rust
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
```

Change `serve(endpoint, state, limiter)` → `serve(endpoint, state, limiter, preview: Arc<PreviewContext>)` and `handle_connection(conn, state)` → `handle_connection(conn, state, preview: Arc<PreviewContext>)`, threading it through the spawn. Add the match arm in `handle_connection`:

```rust
        Message::ProxyInvitePreview { target, code } => {
            handle_preview(target, code, conn, send, state, preview).await
        }
```

And the handler:

```rust
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

    let outcome = if preview.limiter.try_admit(ip, now).is_none() {
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
```

Update `main.rs` to build the context and pass it to `serve`. Update ALL existing `serve(...)`/`start_relay()` call sites in router.rs tests (build a context with `new_preview_context().unwrap()`).

- [ ] **Step 4: e2e tests in router.rs `mod tests`** (reuse `start_relay`, `test_client_endpoint`):

```rust
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
        let started = std::time::Instant::now();
        let outcome = ask_preview(relay, PreviewTarget::Direct { addr: "127.0.0.1:4433".into() }, "GOOD").await;
        assert!(matches!(outcome, PreviewOutcome::Unavailable));
        assert!(started.elapsed() < Duration::from_secs(3), "refusal must be immediate, not a timeout");
    }
```

(NOTE for the direct-target HAPPY path: the SSRF guard correctly refuses 127.0.0.1, which is also the only address a test server can listen on — so the direct happy path is covered indirectly: `ask_server` is the same code path the registered test exercises, and `is_global_ip` has its own unit tests. Do NOT weaken the guard for tests. State this in a comment next to the test.)

- [ ] **Step 5:** `cargo test -p farder-relay` — ALL relay tests green (old 14 + 3 proxy units + 3 new e2e; existing tests updated for the new `serve` signature).

- [ ] **Step 6: Commit**

```bash
git add crates/farder-relay/src/proxy.rs crates/farder-relay/src/router.rs crates/farder-relay/src/main.rs crates/farder-relay/Cargo.toml
git commit -m "relay: invite-preview fetch proxy with rate limit, SSRF guard, TTL cache"
```

---

### Task 4: Client Rust — `get_invite_preview` command

**Files:**
- Modify: `client/src-tauri/src/connection.rs` (add `parse_direct_invite`)
- Modify: `client/src-tauri/src/commands.rs` (new command)
- Modify: `client/src-tauri/src/main.rs` (register)

- [ ] **Step 1: Direct-link parsing (TDD).** In `client/src-tauri/src/connection.rs`, next to `parse_relay_target`, add with tests:

```rust
/// Parse a DIRECT invite link into (server_addr, invite_code), mirroring the
/// frontend's parseInviteLink rules for the two direct forms that carry a code:
/// `farder://host:port/code` and bare `host:port/code`. Setup-token links and
/// code-less links return None (no preview possible).
pub fn parse_direct_invite(s: &str) -> Option<(String, String)> {
    let trimmed = s.trim();
    if trimmed.starts_with("farder://relay") {
        return None; // relay forms are parse_relay_target's job
    }
    let rest = trimmed.strip_prefix("farder://").unwrap_or(trimmed);
    let (addr, token) = rest.split_once('/')?;
    if token.is_empty() || token.starts_with("setup:") {
        return None;
    }
    // addr must look like host:port (same loose rule the frontend uses).
    if !addr.contains(':') {
        return None;
    }
    Some((addr.to_string(), token.to_string()))
}
```

Tests in the same file's `mod tests`:

```rust
    #[test]
    fn direct_invite_links_parse_and_relay_or_tokenless_forms_do_not() {
        assert_eq!(
            parse_direct_invite("farder://203.0.113.7:4433/AbCd1234"),
            Some(("203.0.113.7:4433".to_string(), "AbCd1234".to_string()))
        );
        assert_eq!(
            parse_direct_invite("203.0.113.7:4433/AbCd1234"),
            Some(("203.0.113.7:4433".to_string(), "AbCd1234".to_string()))
        );
        assert!(parse_direct_invite("farder://203.0.113.7:4433/setup:aabb").is_none());
        assert!(parse_direct_invite("203.0.113.7:4433").is_none());
        assert!(parse_direct_invite("farder://relay/1.2.3.4:1/aa/bb/code").is_none());
        assert!(parse_direct_invite("farder://relayd/aabb/code").is_none());
        assert!(parse_direct_invite("AbCd1234").is_none());
    }
```

Run `cd client/src-tauri && cargo test direct_invite` (fails → implement → passes).

- [ ] **Step 2: The command.** In `commands.rs` (after `get_member_profile`):

```rust
#[derive(Clone, serde::Serialize)]
pub struct InvitePreviewResult {
    /// "ok" | "invalid" | "unavailable" | "none" (none = link carries no
    /// previewable invite code, e.g. setup-token or bare-address links).
    pub status: String,
    pub server_name: Option<String>,
    pub member_count: Option<u32>,
    pub online_count: Option<u32>,
}

/// Session-scoped preview cache: link → (when, result). 60s TTL mirrors the
/// relay-side cache; previews are point-in-time data.
static PREVIEW_CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, InvitePreviewResult)>>> =
    std::sync::OnceLock::new();

fn preview_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, InvitePreviewResult)>> {
    PREVIEW_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Fetch an invite preview through a relay (the link's own relay for relayed
/// invites; the default Farder relay for direct invites). Throwaway connection;
/// never touches session connections. LAZY ONLY (PIN-lock rule) — but note it
/// needs no identity at all: previews are anonymous.
#[tauri::command]
pub async fn get_invite_preview(link: String) -> Result<InvitePreviewResult, String> {
    use farder_protocol::messages::{Message, PreviewOutcome, PreviewTarget};

    let none = |_: ()| InvitePreviewResult { status: "none".into(), server_name: None, member_count: None, online_count: None };

    // Cache first.
    {
        let cache = preview_cache().lock().map_err(|e| e.to_string())?;
        if let Some((at, hit)) = cache.get(&link) {
            if at.elapsed() < std::time::Duration::from_secs(60) {
                return Ok(hit.clone());
            }
        }
    }

    // Work out (relay endpoint, target, code) from the link form.
    let (relay_addr, relay_fp, target, code) =
        if let Some(t) = crate::connection::parse_relay_target(&link) {
            if t.invite_token.is_empty() {
                return Ok(none(()));
            }
            (t.relay_addr, t.cert_fp.clone(), PreviewTarget::Registered { server_id: t.server_id.clone() }, t.invite_token.clone())
        } else if let Some((addr, code)) = crate::connection::parse_direct_invite(&link) {
            let Some((def_addr, def_fp)) = crate::default_relay::default_relay() else {
                return Ok(none(())); // no default relay in this build → no direct previews
            };
            (def_addr, def_fp, PreviewTarget::Direct { addr }, code)
        } else {
            return Ok(none(()));
        };

    // Throwaway pinned connection to the relay.
    let outcome = async {
        let endpoint = crate::tls::make_pinned_relay_endpoint(relay_fp).map_err(|e| e.to_string())?;
        let conn = endpoint
            .connect(relay_addr, "farder-relay").map_err(|e| e.to_string())?
            .await.map_err(|e| e.to_string())?;
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
        let msg = farder_protocol::codec::encode(&Message::ProxyInvitePreview { target, code })
            .map_err(|e| e.to_string())?;
        crate::connection::write_frame(&mut send, &msg).await.map_err(|e| e.to_string())?;
        let reply: Message = farder_protocol::codec::decode(
            &crate::connection::read_frame(&mut recv).await.map_err(|e| e.to_string())?,
        ).map_err(|e| e.to_string())?;
        conn.close(0u32.into(), b"preview done");
        match reply {
            Message::ProxyInvitePreviewResult { outcome } => Ok(outcome),
            other => Err(format!("unexpected relay reply: {:?}", other)),
        }
    };

    // 8s client-side budget (the relay's own budget is 5s).
    let outcome = match tokio::time::timeout(std::time::Duration::from_secs(8), outcome).await {
        Ok(Ok(o)) => o,
        Ok(Err(_)) | Err(_) => PreviewOutcome::Unavailable,
    };

    let result = match outcome {
        PreviewOutcome::Preview { server_name, member_count, online_count } => InvitePreviewResult {
            status: "ok".into(),
            server_name: Some(server_name),
            member_count: Some(member_count),
            online_count: Some(online_count),
        },
        PreviewOutcome::Invalid => InvitePreviewResult { status: "invalid".into(), server_name: None, member_count: None, online_count: None },
        PreviewOutcome::Unavailable => InvitePreviewResult { status: "unavailable".into(), server_name: None, member_count: None, online_count: None },
    };

    preview_cache().lock().map_err(|e| e.to_string())?
        .insert(link, (std::time::Instant::now(), result.clone()));
    Ok(result)
}
```

(Check `make_pinned_relay_endpoint`'s exact signature — it takes `Vec<u8>`. If `default_relay()` returns the fp as `Vec<u8>` this slots straight in. Truncate the server name defensively: after a successful Preview, `server_name.chars().take(80).collect()` — a hostile server shouldn't get a paragraph into the card; do this in the `Preview` match arm.)

- [ ] **Step 3:** Register `get_invite_preview` in `generate_handler![]` in `main.rs`.

- [ ] **Step 4:** `cd client/src-tauri && cargo build` (clean) + `cargo test direct_invite` (pass). Seam grep: `grep -c "get_invite_preview" src/main.rs` ≥ 1.

- [ ] **Step 5: Commit**

```bash
git add client/src-tauri/src/connection.rs client/src-tauri/src/commands.rs client/src-tauri/src/main.rs
git commit -m "client: get_invite_preview command (throwaway relay-proxied lookup, 60s cache)"
```

---

### Task 5: Frontend — hook, InviteEmbed v2, JoinConfirm name

**Files:**
- Modify: `client/src/lib/tauri-bridge.ts`
- Create: `client/src/hooks/useInvitePreview.ts`
- Modify: `client/src/components/InviteEmbed.tsx`
- Modify: `client/src/components/JoinConfirmModal.tsx` (+ its render site(s) — find with `grep -rn "JoinConfirmModal" client/src --include="*.tsx"`)

- [ ] **Step 1: Bridge function** in `tauri-bridge.ts`:

```ts
export interface InvitePreviewResult {
  status: "ok" | "invalid" | "unavailable" | "none";
  server_name: string | null;
  member_count: number | null;
  online_count: number | null;
}

export async function getInvitePreview(link: string): Promise<InvitePreviewResult> {
  return invoke<InvitePreviewResult>("get_invite_preview", { link });
}
```

- [ ] **Step 2: Hook** — create `client/src/hooks/useInvitePreview.ts`:

```ts
import { useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";

export interface InvitePreview {
  status: "loading" | "ok" | "invalid" | "unavailable" | "none";
  serverName: string | null;
  memberCount: number | null;
  onlineCount: number | null;
}

const NONE: InvitePreview = { status: "none", serverName: null, memberCount: null, onlineCount: null };

// Session cache by link. The Rust side has its own 60s TTL cache; this one just
// prevents re-invoking per render/mount.
const cache = new Map<string, InvitePreview>();
const pending = new Map<string, Promise<InvitePreview>>();

export function useInvitePreview(link?: string | null): InvitePreview {
  const [preview, setPreview] = useState<InvitePreview>(
    link ? cache.get(link) ?? { ...NONE, status: "loading" } : NONE,
  );

  useEffect(() => {
    if (!link) { setPreview(NONE); return; }
    const hit = cache.get(link);
    if (hit) { setPreview(hit); return; }
    setPreview({ ...NONE, status: "loading" });
    let cancelled = false;
    let p = pending.get(link);
    if (!p) {
      p = api.getInvitePreview(link)
        .then((v): InvitePreview => {
          const result: InvitePreview = {
            status: v.status,
            serverName: v.server_name ?? null,
            memberCount: v.member_count ?? null,
            onlineCount: v.online_count ?? null,
          };
          // Don't pin transient failures for the whole session — allow a
          // retry on the next mount.
          if (v.status !== "unavailable") cache.set(link, result);
          pending.delete(link);
          return result;
        })
        .catch((): InvitePreview => { pending.delete(link); return { ...NONE, status: "unavailable" }; });
      pending.set(link, p);
    }
    p.then((r) => { if (!cancelled) setPreview(r); });
    return () => { cancelled = true; };
  }, [link]);

  return preview;
}
```

- [ ] **Step 3: InviteEmbed v2** — replace the component body:

```tsx
import { useApp } from "../context/ServerContext";
import { parseInviteLink } from "../lib/invite";
import { useInvitePreview } from "../hooks/useInvitePreview";

interface InviteEmbedProps {
  link: string;
}

export default function InviteEmbed({ link }: InviteEmbedProps) {
  const { dispatch } = useApp();
  const address = parseInviteLink(link).address ?? "";
  const relayed = /^farder:\/\/relayd?\//i.test(address);
  const preview = useInvitePreview(link);

  return (
    <div className="invite-embed">
      <div className="invite-embed-title">
        {preview.status === "ok" && preview.serverName ? preview.serverName : "Server invite"}
      </div>
      {preview.status === "ok" && (
        <div className="invite-embed-counts">
          {preview.memberCount ?? 0} members · {preview.onlineCount ?? 0} online
        </div>
      )}
      {preview.status === "loading" && (
        <div className="invite-embed-state">Loading preview…</div>
      )}
      {preview.status === "invalid" && (
        <div className="invite-embed-state">Invite invalid or expired</div>
      )}
      {preview.status === "unavailable" && (
        <div className="invite-embed-state">Preview unavailable</div>
      )}
      <div className={`join-relay-note ${relayed ? "relayed" : "direct"}`}>
        <span className="join-relay-badge">{relayed ? "RELAYED" : "DIRECT"}</span>
        <span>{relayed ? "Your IP stays hidden from the host." : "The host can see your IP address."}</span>
      </div>
      <button
        className="xp-button invite-embed-join"
        onClick={() => dispatch({ type: "OPEN_JOIN_CONFIRM", link })}
        disabled={preview.status === "invalid"}
      >
        Join
      </button>
    </div>
  );
}
```

- [ ] **Step 4: JoinConfirmModal name.** Add an optional `link` prop; resolve the preview inside (hooks fine — it's a component) and use the name when available:

```tsx
import { useState } from "react";
import { useInvitePreview } from "../hooks/useInvitePreview";

export default function JoinConfirmModal({
  relayed,
  link,
  onConfirm,
  onCancel,
}: {
  relayed: boolean;
  link?: string;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const [showInfo, setShowInfo] = useState(false);
  const preview = useInvitePreview(link);
  return (
    /* keep the existing JSX; change ONLY the lead paragraph: */
    /* <p>You&apos;ve been invited to a Farder server. Join it?</p>  becomes: */
    <p>
      {preview.status === "ok" && preview.serverName
        ? <>You&apos;ve been invited to <strong>{preview.serverName}</strong>. Join it?</>
        : <>You&apos;ve been invited to a Farder server. Join it?</>}
    </p>
    /* ...rest unchanged */
  );
}
```

Then find the render site(s) (`grep -rn "<JoinConfirmModal" client/src --include="*.tsx"` — expected App.tsx) and pass `link={...}` from the join-confirm state already in scope there (the same link the modal confirms — read the surrounding code; it holds the link to connect with).

- [ ] **Step 5:** `cd client && npx tsc --noEmit` → clean.

- [ ] **Step 6: Commit**

```bash
git add client/src/lib/tauri-bridge.ts client/src/hooks/useInvitePreview.ts client/src/components/InviteEmbed.tsx client/src/components/JoinConfirmModal.tsx client/src/App.tsx
git commit -m "client ui: live invite previews on cards and the join dialog"
```

(Adjust the `git add` list to the actual render-site file(s) touched.)

---

### Task 6: Theme CSS (ALL themes)

New classes from Task 5: `.invite-embed-counts`, `.invite-embed-state`. (`.invite-embed-title` already exists and now sometimes carries a server name — no change needed unless it lacks ellipsis handling; check and add `overflow: hidden; text-overflow: ellipsis; white-space: nowrap;` per theme if missing.)

**Files:**
- Modify: `client/src/themes/discord-dark/theme.css`
- Modify: `client/src/themes/xp-luna-blue/theme.css`
- Modify: `client/src/themes/hello-kitty/theme.css`

- [ ] **Step 1:** Read each theme's `:root` and existing `.invite-embed*` rules; append a commented block per theme using THAT theme's variables (muted text var for both classes; the profile-sync block added 2026-06-11 shows each theme's muted-text idiom — mirror it):

```css
/* Invite previews */
.invite-embed-counts {
  font-size: 12px;
  color: var(--xp-text-muted);
  margin: 2px 0;
}
.invite-embed-state {
  font-size: 12px;
  font-style: italic;
  color: var(--xp-text-muted);
  margin: 2px 0;
}
```

(In `xp-luna-blue`, verify `--xp-text-muted` exists — it does (used by `.member-status`); otherwise use that theme's documented fallback pattern.) Also add the ellipsis trio to `.invite-embed-title` in any theme missing it.

- [ ] **Step 2:** `grep -l "invite-embed-counts" client/src/themes/*/theme.css` → all three files. `cd client && npx tsc --noEmit` still clean.

- [ ] **Step 3: Commit**

```bash
git add client/src/themes/*/theme.css
git commit -m "themes: invite preview counts + state styling in all themes"
```

---

### Task 7: Docs + full verification

**Files:**
- Create: `docs/modules/relay-proxy.md` (use `docs/modules/_TEMPLATE.md`: PreviewContext, fetch flow for both targets, the handle-0 convention, all five guardrails, cache semantics)
- Modify: `docs/modules/relay.md` (handle-0 = relay-originated preview streams; serve_relay_stream gating change)
- Modify: `docs/modules/server-handlers.md` or the doc covering `connection.rs` auth (the GetInvitePreview pre-auth arm; uniform invalid answer)
- Modify: `docs/modules/tauri-commands.md` (`get_invite_preview`: params, return, the relay-selection rule, 60s cache)
- Modify: `docs/modules/frontend-hooks.md` IF it exists (`useInvitePreview`); else skip
- Modify: `ARCHITECTURE.md` (one line: the relay doubles as a privacy fetch proxy; invite previews are phase one, external embeds next)
- Modify: `docs/deploy/relay.md` (note: relays must be redeployed to serve previews; older relays degrade to "Preview unavailable")

- [ ] **Step 1:** Write the docs, matching each file's existing format.

- [ ] **Step 2: Full gates** — all must be green; if anything fails STOP and report (do not patch code in a docs task):

```bash
cd /home/deez/farder && cargo test --workspace 2>&1 | grep -E "^test result" 
cd /home/deez/farder/client/src-tauri && cargo build 2>&1 | tail -2 && cargo test 2>&1 | grep -E "^test result"
cd /home/deez/farder/client && npx tsc --noEmit
cd /home/deez/farder && grep -q "get_invite_preview" client/src-tauri/src/main.rs && grep -q '"get_invite_preview"' client/src/lib/tauri-bridge.ts && echo "OK seam"
```

- [ ] **Step 3: Commit**

```bash
git add docs/ ARCHITECTURE.md
git commit -m "docs: relay fetch proxy (invite previews), preview command, deploy note"
```

- [ ] **Step 4: Owner rollout note** (goes in the final report, not code): merge + push; then VPS: `git pull && docker compose -f deploy/relay/docker-compose.yml up -d --build`; then Windows: kill farder processes → `cargo build -p farder-server` → `copy-sidecar.ps1` from REPO ROOT → restart tauri dev. Verify: paste a relayed invite → card fills with name/counts; expired/garbage code → "Invite invalid or expired"; stop the relay container briefly → "Preview unavailable". UNVERIFIED until that run per CLAUDE.md.

---

## Self-review notes (done at plan time)

- **Spec coverage:** pre-auth gated question (T2), unified proxy both targets (T3), relay-selection rule (T4), all five guardrails (T3: limiter/timeout/SSRF/cap/cache), client cache (T4+T5), three card states + JoinConfirm name (T5), themes (T6), rollout + old-version degradation (T2 throwaway bail, T7 docs/report), uniform invalid answer (T2), counts staleness accepted (spec). Out-of-scope items untouched.
- **Type consistency:** `PreviewTarget`/`PreviewOutcome` defined T1, used T3/T4; `InvitePreviewResult.status` strings match the TS union; `GetInvitePreview { code }`/`InvitePreview{server_name,member_count,online_count}` identical at T1/T2/T3.
- **Known judgment calls:** direct-target happy path can't be e2e-tested headlessly (SSRF guard correctly refuses loopback — the shared `ask_server` path is covered via the registered test + guard unit tests); preview rate-limit refusal returns `Unavailable` (not a distinct status) — deliberate, keeps the wire surface small; `usize::MAX` concurrent cap on the preview limiter is intentional (rate-only use).
