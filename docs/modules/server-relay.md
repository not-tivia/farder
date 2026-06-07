# Module: server relay-mode (`crates/farder-server/src/relay.rs`)

**Purpose:** lets a Farder server run *relay-only* — instead of binding a public
listener, it dials a relay, registers under a stable `server_id`, and serves each
relay-bridged stream as a client session. This hides the server's IP (no direct
listener). Added in relay Phase 2. Direct mode (no `--relay`) is unchanged.

See the parent design `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md`
and `docs/modules/tauri-voice.md`/`server-connection.md` for the surrounding flow.

## How it's turned on

`farder-server --relay <addr> [--data-dir <dir>]`. When `--relay` is set,
`main.rs` calls `relay::serve_via_relay` instead of binding the direct endpoint.
`--data-dir` (default `./server-data`) holds the persisted `server_id`.

## Public surface

- `load_or_generate_server_id(data_dir: &Path) -> Result<[u8; 32]>` — load the
  stable 32-byte `server_id` from `<data_dir>/server_id`, generating+persisting
  one on first run. This is the id the server registers under and that invites
  will encode (Phase 3).
- `serve_via_relay(state: Arc<ServerState>, relay_addr: SocketAddr, server_id: [u8; 32]) -> Result<()>`
  — dial the relay, send `Message::RelayRegister { server_id }`, then loop
  `accept_bi()` on the relay connection, serving each bridged stream. Reconnects
  with capped backoff (500 ms → 30 s) if the relay connection drops.

## Per-stream dispatch (the token demux)

Every relay-bridged stream opens with a `farder_protocol::server::RelayStreamRole`
frame (4-byte big-endian length + rmp_serde, matching `connection.rs`):

- `Primary` → `run_relay_primary`: runs the existing `connection::authenticate`
  (challenge → verify → `ServerFrame::Authenticated { session_token }`), which
  registers the token in the session registry, then `connection::main_loop`, then
  `connection::cleanup_session` (removes the token). Mirrors direct mode minus the
  connection-specific voice/aux/datagram loops.
- `Session { token }` → `run_relay_aux`: `state.lookup_session(token)` →
  `(member_key, is_owner)` → `connection::handle_auxiliary_stream` (file
  upload/download). An unknown/expired token closes only that stream.

The relay multiplexes all clients onto the server's one connection with no
per-client grouping; the session-token registry IS the grouping mechanism.

## Session registry (`ServerState`, `crates/farder-server/src/state.rs`)

- `register_session(token: [u8; 32], public_key: PublicKey, is_owner: bool)` —
  insert on successful primary auth.
- `lookup_session(token: &[u8; 32]) -> Option<(PublicKey, bool)>` — authorize a
  relay file stream.
- `remove_session(token: &[u8; 32])` — remove when the primary session ends.

Populated in both modes (uniform); only relay file streams consult it (direct
aux streams stay connection-trusted).

## Refactor seam (`crates/farder-server/src/connection.rs`)

The per-client core is `pub(crate)`: `authenticate` (→ `AuthOutcome`),
`cleanup_session`, `main_loop`, `handle_auxiliary_stream`. Direct
`handle_connection` and `relay::run_relay_primary` both compose them; the relay
path simply omits the `quinn::Connection`-specific aux-acceptor and datagram
loops (voice over relay is deferred).

## Trust / limits (this phase)

- The server accepts the relay's cert without pinning (pinning is Phase 3).
- No voice/datagrams over the relay (deferred).
- `server_id` is a public routing handle, not a secret.
- Relay abuse controls (who may register, rate limits) are deferred.

## Tests

`crates/farder-server/tests/relay_mode.rs` — real-QUIC end-to-end through a
minimal in-test relay double: relayed login + request, file upload/download over
a `Session` stream, and bad-token rejection (primary session unaffected).
`server_id` persistence is unit-tested in `relay.rs`.
