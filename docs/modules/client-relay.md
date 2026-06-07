# Module: client relay connection (`client/src-tauri/src/connection.rs` + `tls.rs`)

**Purpose:** lets the Tauri client connect to a **relayed** server *through its
relay*, so the destination server sees only the relay's IP (closes audit Gap #3).
Added in relay Phase 3a. Direct connections are unchanged.

See the umbrella design `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md`
and the server side `docs/modules/server-relay.md`.

## Connection-info form

A relayed server is reachable via the escape-hatch link
`farder://relay/<relay_addr>/<server_id_hex>/<cert_fp_hex>/<invite_token>`.
`parse_relay_target(&str) -> Option<RelayTarget>` parses it; a string that isn't a
relay link returns `None` (so the direct path runs). Pretty `farder.com/invite`
links that resolve to this form are Phase 3b.

## Public surface

- `parse_relay_target(s) -> Option<RelayTarget>` (`connection.rs`) — parse the relay
  link into `RelayTarget { relay_addr, server_id, cert_fp, invite_token }`.
- `connect_via_relay(endpoint, &target, keypair, setup_token) -> Result<(Connection, SendStream, RecvStream, Vec<u8>)>`
  — connect to the relay (cert pinned), `RelayConnect{server_id}` → `RelayConnected`,
  open the primary stream marked `RelayStreamRole::Primary`, run the shared
  `run_client_handshake` (authenticating with `target.invite_token`), return the
  connection + streams + session token.
- `run_client_handshake(send, recv, keypair, invite_code, setup_token)` — the
  challenge→authenticate→token handshake, shared by the direct
  (`connect_and_authenticate`) and relay paths.
- `tls::make_pinned_relay_endpoint(fingerprint) -> Result<Endpoint>` +
  `tls::cert_fingerprint(&[u8]) -> Vec<u8>` — a QUIC client endpoint that accepts
  the relay's cert ONLY if its SHA-256 fingerprint matches the pinned value
  (prevents relay impersonation / MITM). NOT skip-verify.

## How `connect_server` chooses the path (`commands.rs`)

`connect_server` calls `parse_relay_target(&address)`: `Some` → pinned endpoint +
`connect_via_relay` (`relayed = true`); `None` → existing direct path
(`make_client_endpoint` + `connect_and_authenticate`, `relayed = false`). The
resulting `ServerConnection` stores `session_token` and `relayed`. Saving the relay
link as the server's address means reconnect on relaunch re-parses it — no
`ServerEntry` schema change.

## Relay-mode behaviour (`ServerConnection.relayed`)

- **Files:** `upload_file_internal_with_channel`, `download_file_internal`, and
  `add_favorite` call `write_relay_session_marker(send, conn)` — a no-op for direct
  connections, but for relayed ones it writes `RelayStreamRole::Session{token}`
  before the Upload/Download request so the server demuxes the stream to the right
  member.
- **Voice:** `voice_join` returns "voice is not available over a relay yet" for a
  relayed connection (datagrams aren't relayed). The datagram recv loop is not
  spawned for relayed connections.

## Trust / limits

- The relay's cert is pinned by fingerprint from the connection info (Phase 3a).
  The Farder default relay's fingerprint can be bundled when that relay is deployed.
- The relay still sees the client's IP + non-DM traffic by design (it's the trusted
  hop); pinning prevents a *different* attacker from impersonating it. DMs stay E2EE.
- Voice over relay is deferred.

## Tests

`client/src-tauri/src/connection.rs` `#[cfg(test)] mod relay_it` — real-QUIC,
headless, via a relay double + a mock server (the client crate can't depend on
`farder-server`): the Gap #3 observation (server sees the relay's address, never the
client's), cert-pinning rejection of a wrong fingerprint, and the file-stream
`Session` marker. `parse_relay_target` and `cert_fingerprint` have unit tests.
