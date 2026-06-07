# Relay Phase 2 — Server Relay-Mode — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a Farder server run relay-only — dial a relay, register under a stable `server_id`, and serve each relay-bridged stream (login or file-transfer) by reusing the existing stream-based auth/`main_loop`/file handlers.

**Architecture:** Each relay-bridged stream opens with a `RelayStreamRole` marker. `Primary` → run the existing auth handshake (which already mints a `session_token`), store `token → {member, is_owner}` in the (already-present but unused) `ServerState.sessions` registry, then `main_loop`. `Session{token}` → look the token up and run the existing file-transfer handler. The per-client core is extracted into reusable `(send, recv)`-based functions; direct mode keeps its exact current behavior.

**Tech Stack:** Rust, quinn 0.11, rustls 0.23 (ring), rmp_serde, tokio.

**Spec:** `docs/superpowers/specs/2026-06-06-relay-phase2-server-mode-design.md`

**Depends on:** Phase 1 (relay) — merged. **Phase 3** (client + invites + the Gap #3 observation test) and **Phase 4** (UI) follow.

**Hard invariant:** Direct-mode behavior must be preserved exactly. The existing `farder-server` test suite (204 tests) is the regression guard and MUST stay green after every task.

---

## File Structure

- `crates/farder-protocol/src/server.rs` — add `RelayStreamRole` enum + codec test.
- `crates/farder-server/src/state.rs` — add `is_owner` to `SessionInfo`; add `register_session`/`lookup_session`/`remove_session` on `ServerState`.
- `crates/farder-server/src/connection.rs` — extract `authenticate()` + `cleanup_session()`; rewrite direct `handle_connection` to use them (behavior-preserving); wire session register/remove. Make `authenticate`, `cleanup_session`, `main_loop`, `handle_auxiliary_stream` reachable from the new relay module (`pub(crate)`).
- `crates/farder-server/src/relay.rs` *(new)* — `serve_via_relay`, `server_id` persistence, per-stream dispatch (`run_relay_primary`, `run_relay_aux`), and the relay-register framing + skip-verify client endpoint.
- `crates/farder-server/src/lib.rs` — `pub mod relay;`.
- `crates/farder-server/src/main.rs` — `--relay <addr>` arg; if set, run `serve_via_relay` instead of binding a direct listener.

---

## Task 1: Protocol — `RelayStreamRole`

**Files:** Modify `crates/farder-protocol/src/server.rs` (the `ClientFrame`/`ServerFrame` area + tests).

- [ ] **Step 1: Add a failing codec test.** In the tests module of `crates/farder-protocol/src/server.rs` (or add one if none — match the crate's `#[cfg(test)] mod tests` + `use crate::codec;` pattern from `messages.rs`), add:

```rust
    #[test]
    fn test_roundtrip_relay_stream_role() {
        let p = RelayStreamRole::Primary;
        let back: RelayStreamRole = codec::decode(&codec::encode(&p).unwrap()).unwrap();
        assert!(matches!(back, RelayStreamRole::Primary));

        let s = RelayStreamRole::Session { token: vec![1u8, 2, 3] };
        let back: RelayStreamRole = codec::decode(&codec::encode(&s).unwrap()).unwrap();
        match back {
            RelayStreamRole::Session { token } => assert_eq!(token, vec![1u8, 2, 3]),
            other => panic!("expected Session, got {other:?}"),
        }
    }
```

(If `server.rs` has no `use crate::codec;` in its test module, add it. Confirm `codec` is the rmp_serde wrapper used elsewhere.)

- [ ] **Step 2: Run to verify it fails:** `cd ~/farder && cargo test -p farder-protocol relay_stream_role 2>&1 | tail -12` — expect a compile error (`RelayStreamRole` undefined).

- [ ] **Step 3: Add the type.** In `crates/farder-protocol/src/server.rs`, near the `ClientFrame` enum, add:

```rust
/// First frame on every relay-bridged stream, identifying its role. Relay-mode
/// only; direct connections do not use it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RelayStreamRole {
    /// A new client session: the server runs the auth handshake on this stream.
    Primary,
    /// A file-transfer stream for an already-authenticated session, identified
    /// by the session token the server issued at login.
    Session { token: Vec<u8> },
}
```

(Confirm `serde::{Serialize, Deserialize}` are already imported in `server.rs`; they are used by `ClientFrame`/`ServerFrame`.)

- [ ] **Step 4: Run to verify it passes:** `cd ~/farder && cargo test -p farder-protocol relay_stream_role 2>&1 | tail -8` — expect PASS.

- [ ] **Step 5: Commit:**
```bash
cd ~/farder && git add crates/farder-protocol/src/server.rs && \
git commit -m "protocol: add RelayStreamRole (relay-bridged stream marker)"
```

---

## Task 2: Session registry on `ServerState`

**Files:** Modify `crates/farder-server/src/state.rs`.

Background: `ServerState.sessions: RwLock<HashMap<[u8; 32], SessionInfo>>` already exists but is unused; `SessionInfo { public_key, expires_at }` is defined but never constructed. We add `is_owner` and three helper methods.

- [ ] **Step 1: Add `is_owner` to `SessionInfo`.** In `crates/farder-server/src/state.rs`, change the struct:

```rust
pub struct SessionInfo {
    pub public_key: PublicKey,
    pub is_owner: bool,
    pub expires_at: u64,
}
```

- [ ] **Step 2: Write a failing test.** Append to `state.rs` a test module (or extend the existing one):

```rust
#[cfg(test)]
mod session_registry_tests {
    use super::*;

    fn empty_state() -> ServerState {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        ServerState::new(conn, "t".into(), "/tmp/farder-test-files".into(), 1024)
    }

    #[tokio::test]
    async fn session_register_lookup_remove() {
        let state = empty_state();
        let pk = farder_crypto::identity::Keypair::generate().public_key();
        let token = [7u8; 32];
        assert!(state.lookup_session(&token).await.is_none());
        state.register_session(token, pk.clone(), true).await;
        let got = state.lookup_session(&token).await.expect("present");
        assert_eq!(got.0, pk);
        assert!(got.1, "is_owner preserved");
        state.remove_session(&token).await;
        assert!(state.lookup_session(&token).await.is_none());
    }
}
```

(Check `crate::db::init_db` is the real DB-initialiser used by other server tests; if the helper differs, match what the existing `state.rs`/`db.rs` tests use to build an in-memory `ServerState`.)

- [ ] **Step 3: Run to verify it fails:** `cd ~/farder && cargo test -p farder-server session_register_lookup_remove 2>&1 | tail -15` — expect a compile error (methods missing).

- [ ] **Step 4: Add the methods.** In `crates/farder-server/src/state.rs`, inside `impl ServerState`, add:

```rust
    /// Register an authenticated session by its login token (relay file streams
    /// look this up to learn whose stream they are). `expires_at` is far-future;
    /// the entry is removed when the primary session ends.
    pub async fn register_session(&self, token: [u8; 32], public_key: PublicKey, is_owner: bool) {
        let mut sessions = self.sessions.write().await;
        sessions.insert(token, SessionInfo { public_key, is_owner, expires_at: u64::MAX });
    }

    /// Look up a session token, returning `(public_key, is_owner)` if present.
    pub async fn lookup_session(&self, token: &[u8; 32]) -> Option<(PublicKey, bool)> {
        let sessions = self.sessions.read().await;
        sessions.get(token).map(|s| (s.public_key.clone(), s.is_owner))
    }

    /// Remove a session token (called when the primary session ends).
    pub async fn remove_session(&self, token: &[u8; 32]) {
        let mut sessions = self.sessions.write().await;
        sessions.remove(token);
    }
```

- [ ] **Step 5: Run to verify it passes + no regressions:** `cd ~/farder && cargo test -p farder-server session_register_lookup_remove 2>&1 | tail -8 && cargo test -p farder-server 2>&1 | tail -4` — expect the new test passes and the full suite stays green.

- [ ] **Step 6: Commit:**
```bash
cd ~/farder && git add crates/farder-server/src/state.rs && \
git commit -m "server: session registry (register/lookup/remove) on ServerState"
```

---

## Task 3: Refactor — extract `authenticate` + `cleanup_session` (direct mode preserved)

**Files:** Modify `crates/farder-server/src/connection.rs`.

Goal: pull the per-client core out of `handle_connection` so it can be reused by the relay path, **without changing direct-mode behavior**. The existing 204-test suite is the safety net.

The current `handle_connection` (`:407`) does, in order: open primary bi-stream (`:408`); auth Steps 1-5 + owner-set + mint `session_token` + send `Authenticated` (`:409-514`); register event channel in `state.clients` + `voice_connections.insert(conn)` (`:516-526`); broadcast `MemberJoined` (`:528-544`); compute `is_owner` (`:546-550`); spawn aux-stream acceptor on `conn` (`:552-581`); spawn datagram loop on `conn` (`:583-708`); `main_loop` (`:711-719`); abort tasks; cleanup (`:725-759`).

- [ ] **Step 1: Introduce `AuthOutcome` and extract `authenticate`.** Add a struct and a function. `authenticate` contains exactly the current Steps 1-8 logic (`:409` through `:550`) EXCEPT the `voice_connections.insert` at `:524-526` (that stays in direct `handle_connection`, since it needs `conn`). It additionally calls `state.register_session(...)` after minting the token. Signature and shape:

```rust
pub(crate) struct AuthOutcome {
    pub public_key: PublicKey,
    pub pk_bytes: [u8; 32],
    pub is_owner: bool,
    pub session_token: [u8; 32],
    pub event_rx: tokio::sync::mpsc::Receiver<ServerEvent>,
    pub event_tx: EventSender,
}

/// Run the auth handshake over an established stream pair, register the client's
/// event channel + session token, and broadcast MemberJoined. Used by both the
/// direct connection handler and the relay serve loop. Does NOT touch the raw
/// quinn::Connection (the caller owns connection-specific setup like voice).
pub(crate) async fn authenticate(
    state: &Arc<ServerState>,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Result<AuthOutcome> {
    // ... Steps 1-5 (challenge/receive/verify/member-check), owner-set, mint token,
    //     send Authenticated  — copied verbatim from handle_connection :409-514 ...
    // After sending Authenticated, register the session token:
    state.register_session(session_token, public_key.clone(), /* is_owner computed below */ false).await;
    // ... Step 6 event channel (the :517-522 block, WITHOUT voice_connections.insert) ...
    // ... Step 7 broadcast MemberJoined (:528-544) ...
    // ... Step 8 compute is_owner (:546-550) ...
    // Update the registry now that is_owner is known:
    state.register_session(session_token, public_key.clone(), is_owner).await;
    Ok(AuthOutcome { public_key, pk_bytes, is_owner, session_token, event_rx, event_tx: our_event_tx })
}
```

Note: `register_session` is called twice (once after token mint, then re-inserted with the correct `is_owner` after Step 8) — the second insert overwrites the first. Alternatively, move the `is_owner` computation (`:546-550`) ABOVE the token registration and register once with the correct value; prefer that (single registration) if it does not change behavior. Keep all DB/lock ordering identical to the original to avoid deadlocks.

- [ ] **Step 2: Extract `cleanup_session`.** Pull the cleanup at `:725-753` (clients-map removal with the `still_ours` guard, subscriptions removal, broadcast `MemberLeft`) into a function, and add session-token removal. Keep `voice_connections.remove` OUT of it (direct-only). Signature:

```rust
pub(crate) async fn cleanup_session(
    state: &Arc<ServerState>,
    public_key: &PublicKey,
    pk_bytes: [u8; 32],
    event_tx: &EventSender,
    session_token: &[u8; 32],
) {
    state.remove_session(session_token).await;
    // ... clients-map still_ours removal (:729-738) ...
    // ... subscriptions removal (:740-745) ...
    // ... broadcast MemberLeft (:746-753) ...
}
```

- [ ] **Step 3: Rewrite direct `handle_connection` to use them (behavior-preserving).** It becomes:

```rust
pub async fn handle_connection(state: Arc<ServerState>, conn: quinn::Connection) -> Result<()> {
    let (mut send, mut recv) = conn.open_bi().await?;
    let outcome = authenticate(&state, &mut send, &mut recv).await?;
    let AuthOutcome { public_key, pk_bytes, is_owner, session_token, event_rx, event_tx } = outcome;

    // Direct-only: register the connection for voice + spawn aux/datagram loops.
    {
        let mut voice_conns = state.voice_connections.write().await;
        voice_conns.insert(pk_bytes, conn.clone());
    }
    // (aux-stream acceptor spawn — the existing :552-581 block, using is_owner/public_key)
    // (datagram loop spawn — the existing :583-708 block)

    let loop_result = main_loop(
        Arc::clone(&state), public_key.clone(), is_owner, &mut send, &mut recv, event_rx,
    ).await;

    stream_acceptor.abort();
    datagram_task.abort();
    state.voice_connections.write().await.remove(&pk_bytes);
    cleanup_session(&state, &public_key, pk_bytes, &event_tx, &session_token).await;

    if let Err(e) = loop_result {
        warn!("client {} disconnected with error: {}", public_key, e);
    } else {
        info!("client {} disconnected cleanly", public_key);
    }
    Ok(())
}
```

The aux acceptor (`:552-581`) and datagram loop (`:583-708`) blocks move verbatim into this function between `voice_conns.insert` and `main_loop`, keeping their `stream_acceptor`/`datagram_task` handles for the `.abort()` calls. `member_clone`/`conn_clone`/`state_clone` etc. come from `public_key`/`conn`/`state` exactly as before.

- [ ] **Step 4: Make the reused items reachable from the relay module.** Change `main_loop` and `handle_auxiliary_stream` from `async fn` to `pub(crate) async fn` (they are called by `relay.rs` in Task 4). `authenticate` and `cleanup_session` are already `pub(crate)`.

- [ ] **Step 5: Verify NO behavior change — full suite green.** Run: `cd ~/farder && cargo test -p farder-server 2>&1 | tail -8` — expect ALL 204 tests still pass. Also `cargo test --test e2e_server 2>&1 | tail -6` (the workspace e2e server test) — expect pass. If any direct-mode test fails, the extraction changed behavior — fix until identical. Do NOT weaken or delete tests.

- [ ] **Step 6: Commit:**
```bash
cd ~/farder && git add crates/farder-server/src/connection.rs && \
git commit -m "server: extract authenticate/cleanup_session; wire session registry (direct mode unchanged)"
```

---

## Task 4: Relay-mode serve loop (`relay.rs`)

**Files:** Create `crates/farder-server/src/relay.rs`; modify `crates/farder-server/src/lib.rs` (`pub mod relay;`).

This dials the relay, registers under a persisted `server_id`, and serves each bridged stream by reading its `RelayStreamRole`.

- [ ] **Step 1: Register the module.** In `crates/farder-server/src/lib.rs`, add `pub mod relay;` (alphabetical with the other `pub mod` lines).

- [ ] **Step 2: Write the module.** Create `crates/farder-server/src/relay.rs`:

```rust
//! Relay-mode server transport. Instead of binding a public listener, the
//! server dials a relay, registers under its stable `server_id`, and serves
//! each relay-bridged stream as a client session — reusing the same auth /
//! main_loop / file-transfer code as direct mode. The server never learns a
//! client's real address (only its relay connection), which is the privacy
//! property. Voice/datagrams are not served over the relay (deferred).

use crate::connection::{authenticate, cleanup_session, handle_auxiliary_stream, main_loop};
use crate::state::ServerState;
use anyhow::{Context, Result};
use farder_protocol::{codec, messages::Message, server::RelayStreamRole};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
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

/// Length-prefixed frame read/write matching the relay's wire framing
/// (4-byte big-endian length + payload). Used only for the RelayRegister
/// handshake with the relay.
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

/// Build a QUIC client endpoint that accepts the relay's cert (pinning the
/// relay identity is Phase 3).
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
    let cfg = quinn::ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?));
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
            Ok(()) => { backoff = Duration::from_millis(500); }
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

    loop {
        let (s, r) = conn.accept_bi().await?; // each bridged client stream
        let state = Arc::clone(state);
        tokio::spawn(async move {
            if let Err(e) = serve_relay_stream(state, s, r).await {
                tracing::debug!("relay stream ended: {}", e);
            }
        });
    }
}

/// Read the role marker and dispatch a single bridged stream.
async fn serve_relay_stream(state: Arc<ServerState>, mut send: SendStream, mut recv: RecvStream) -> Result<()> {
    let role: RelayStreamRole = codec::decode(&read_framed(&mut recv).await?)?;
    match role {
        RelayStreamRole::Primary => run_relay_primary(state, send, recv).await,
        RelayStreamRole::Session { token } => run_relay_aux(state, send, recv, token).await,
    }
}

async fn run_relay_primary(state: Arc<ServerState>, mut send: SendStream, mut recv: RecvStream) -> Result<()> {
    let outcome = authenticate(&state, &mut send, &mut recv).await?;
    let loop_result = main_loop(
        Arc::clone(&state), outcome.public_key.clone(), outcome.is_owner, &mut send, &mut recv, outcome.event_rx,
    ).await;
    cleanup_session(&state, &outcome.public_key, outcome.pk_bytes, &outcome.event_tx, &outcome.session_token).await;
    loop_result
}

async fn run_relay_aux(state: Arc<ServerState>, send: SendStream, recv: RecvStream, token: Vec<u8>) -> Result<()> {
    let token: [u8; 32] = token.as_slice().try_into().map_err(|_| anyhow::anyhow!("bad session token length"))?;
    let (member_key, is_owner) = state.lookup_session(&token).await.ok_or_else(|| anyhow::anyhow!("unknown or expired session token"))?;
    handle_auxiliary_stream(&state, &member_key, is_owner, send, recv).await
}
```

Notes for the implementer: `read_framed`/`write_framed` must match the relay's `read_message`/`write_message` (4-byte BE length + payload) — they do. The role marker and the auth frames it precedes use the SAME framing as `connection.rs` `read_frame`/`write_frame`; confirm `read_frame`/`write_frame` also use a 4-byte BE length prefix so `read_framed` here is wire-compatible with what `authenticate` expects next on the stream. If `connection.rs`'s frame length prefix differs (e.g. varint), make `read_framed`/`write_framed` match `connection.rs`'s framing instead — the role marker must be readable by the same codec the rest of the session uses. `rand` is a workspace dep; if it is not already a dependency of `farder-server`, add `rand = "0.8"` to its `Cargo.toml`.

- [ ] **Step 3: Build to confirm it compiles:** `cd ~/farder && cargo build -p farder-server 2>&1 | tail -6` — expect it builds (warnings OK). Fix visibility/import errors (e.g. ensure Task 3 made `main_loop`/`handle_auxiliary_stream` `pub(crate)`).

- [ ] **Step 4: Commit:**
```bash
cd ~/farder && git add crates/farder-server/src/relay.rs crates/farder-server/src/lib.rs crates/farder-server/Cargo.toml && \
git commit -m "server: relay-mode serve loop (dial, register, per-stream dispatch)"
```

---

## Task 5: `--relay` flag in `main.rs`

**Files:** Modify `crates/farder-server/src/main.rs`.

- [ ] **Step 1: Add the arg.** In the `Args` struct in `crates/farder-server/src/main.rs`, add:

```rust
    /// If set, run relay-only: register with this relay instead of binding a
    /// public listener (hides the server's IP).
    #[arg(long)]
    relay: Option<SocketAddr>,

    /// Directory for the server's stable relay identity (server_id).
    #[arg(long, default_value = "./server-data")]
    data_dir: std::path::PathBuf,
```

- [ ] **Step 2: Branch on relay vs direct.** After `ServerState` is constructed and wrapped in `Arc` (find where `state` is built and the accept loop begins, around `make_server_endpoint`/the `loop` at `:113`), replace the direct accept loop with a branch. Direct mode keeps the EXACT existing loop; relay mode calls `serve_via_relay`:

```rust
    if let Some(relay_addr) = args.relay {
        let server_id = farder_server::relay::load_or_generate_server_id(&args.data_dir)?;
        info!("Relay-only mode: registering with relay {}", relay_addr);
        farder_server::relay::serve_via_relay(state, relay_addr, server_id).await?;
        return Ok(());
    }

    // (existing direct-mode endpoint bind + accept loop unchanged below)
    let endpoint = make_server_endpoint(args.bind)?;
    info!("Server listening on {}", args.bind);
    loop { /* ... existing ... */ }
```

(Adjust the `farder_server::` path to the actual crate name if `main.rs` refers to its own crate differently; the relay module is `crate::relay` from within the binary if `main.rs` is part of the lib, or `farder_server::relay` if separate. Match how `main.rs` already references other modules.)

- [ ] **Step 3: Build:** `cd ~/farder && cargo build -p farder-server 2>&1 | tail -5` — expect builds. Confirm `rustls::crypto::ring::default_provider().install_default()` is already called in server `main()` (the server builds rustls configs, so it must be — if not, the relay client endpoint needs it; add `let _ = rustls::crypto::ring::default_provider().install_default();` at the top of `main()`).

- [ ] **Step 4: Commit:**
```bash
cd ~/farder && git add crates/farder-server/src/main.rs && \
git commit -m "server: --relay flag runs relay-only mode"
```

---

## Task 6: Integration tests — server-over-relay end to end

**Files:** Create `crates/farder-server/tests/relay_mode.rs` (integration test; `farder-server` is a lib+bin, so `tests/` can use `farder_server::*`).

This starts a real relay + a server in relay-mode + a simulated client through the relay. The simulated client mirrors what the real client will do in Phase 3.

- [ ] **Step 1: Write the test harness + login/request test.** Create `crates/farder-server/tests/relay_mode.rs`:

```rust
// End-to-end: relay (Phase 1) + server in relay-mode + a simulated client that
// connects THROUGH the relay, logs in, and issues a request. Proves the server
// serves clients it can only reach via the relay (never their real address).

use farder_crypto::identity::Keypair;
use farder_protocol::server::RelayStreamRole;
use farder_protocol::{codec, messages::Message};
use std::sync::Arc;
use std::time::Duration;

// NOTE: this harness needs helpers to (a) start a relay on an ephemeral port,
// (b) start a server in relay-mode pointed at it, and (c) drive a client through
// the relay. The relay start mirrors farder-relay's test helper; the server
// start calls farder_server::relay::serve_via_relay with an in-memory ServerState
// (use the same in-memory ServerState constructor the other farder-server tests
// use). Frame helpers (4-byte BE length) match the server's wire framing.
//
// Implementer: build these helpers from the existing test utilities in
// farder-server's test suite + farder-relay's router tests (skip-verify client
// endpoint, ensure_provider). Keep assertions strong.

// ... helpers: ensure_provider(), client_endpoint(), start_relay() -> (SocketAddr, ...),
//     start_relay_server(relay_addr) -> server_id, connect_through_relay(relay, server_id) -> Connection,
//     write_framed/read_framed ...

#[tokio::test]
async fn relayed_client_logs_in_and_makes_a_request() {
    // 1. start relay; 2. start server in relay-mode (registers); wait for registration.
    // 3. client: connect to relay, RelayConnect{server_id}, RelayConnected.
    // 4. client opens a stream, writes RelayStreamRole::Primary, then runs the auth
    //    handshake (read Challenge, send Authenticate with a setup/invite as the
    //    other server tests do), expects ServerFrame::Authenticated{session_token}.
    // 5. client sends a ServerRequest (e.g. the same first request the e2e_server
    //    test uses) and asserts the expected ServerFrame response.
    // Assert: the whole exchange succeeds purely over the relay.
}
```

Because this harness is substantial and must match existing server test utilities, the implementer should model it on `tests/e2e_server.rs` (how it builds a server + authenticates a client) and `crates/farder-relay/src/router.rs`'s test module (relay start + skip-verify client endpoint), adapting the client to (a) go through the relay and (b) prefix its primary stream with `RelayStreamRole::Primary`. Keep the assertions concrete (a real request/response round-trip), not just "connected".

- [ ] **Step 2: Run it:** `cd ~/farder && cargo test -p farder-server --test relay_mode 2>&1 | tail -20` — expect the login+request test passes. Debug real async/QUIC issues as needed; do not weaken assertions.

- [ ] **Step 3: Add the file-upload-over-relay test.** Add a second `#[tokio::test]` that, after login, opens a NEW stream through the relay, writes `RelayStreamRole::Session { token }` (the token from login), sends an `UploadRequest` + bytes (mirroring how `tests/e2e_server.rs` or the upload handler tests exercise uploads), and asserts the file is stored (and optionally downloadable via a `Session` download stream). Run it; expect pass.

- [ ] **Step 4: Add the bad-token test.** Add a `#[tokio::test]`: after login, open a `Session` stream with a random 32-byte token; assert the server rejects it (stream errors/closes) WITHOUT affecting the primary session (a subsequent request on the primary stream still succeeds). Run it; expect pass.

- [ ] **Step 5: Add the server_id persistence test.** A unit-style test (can live in `relay.rs` under `#[cfg(test)]` or here): `load_or_generate_server_id(dir)` returns the same 32 bytes across two calls on the same dir, and different bytes for a fresh dir. Run it; expect pass.

- [ ] **Step 6: Commit:**
```bash
cd ~/farder && git add crates/farder-server/tests/relay_mode.rs crates/farder-server/src/relay.rs && \
git commit -m "server: integration tests for relay-mode (login, file upload, bad token, server_id)"
```

---

## Final verification

- [ ] **Whole workspace green, direct mode intact:**

Run: `cd ~/farder && cargo test --workspace 2>&1 | tail -30`
Expected: all pass — the 204 farder-server tests (direct mode, regression guard), the new relay-mode tests, farder-relay (5), farder-protocol (incl. RelayStreamRole), and the workspace e2e tests.

- [ ] **Docs:** add a `docs/modules/server-relay.md` (or extend an existing server module doc) describing `serve_via_relay`, the `RelayStreamRole` marker, and the session registry; update `ARCHITECTURE.md`'s relay line to note the server can now run relay-only. Update the Phase-2 status in `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md`.

- [ ] **Finish the branch:** use superpowers:finishing-a-development-branch.

## Notes for the implementer

- **Direct mode is sacred.** Task 3 is a pure extraction; if any of the 204 existing tests changes behavior, the refactor is wrong — fix until identical. Never edit a test to make the refactor "pass".
- Fully headless: every test runs real QUIC on `127.0.0.1` ephemeral ports. There is no GUI/UNVERIFIED gap in Phase 2 (the real client lands in Phase 3).
- Voice/datagrams are intentionally absent from the relay path. Do not add datagram handling to `serve_via_relay`.
- The Gap #3 privacy observation (server sees relay addr, never client addr) is asserted with the REAL client in Phase 3; Phase 2 proves the server serves correctly over the relay.
