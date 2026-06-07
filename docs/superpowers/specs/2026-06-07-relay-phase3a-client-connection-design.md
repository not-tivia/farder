# Relay Phase 3a — Client Relay Connection — Design Spec

**Date:** 2026-06-07
**Status:** Approved (design); ready to plan
**Parent design:** `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md` (Phase 3)
**Depends on:** Phase 1 (relay) + Phase 2 (server relay-mode) — both merged.
**Closes:** audit Gap #3 (`docs/superpowers/audits/2026-06-05-privacy-security-wiring-audit.md`).

## Problem

The relay (Phase 1) and the relay-mode server (Phase 2) exist, but **no client can
connect through a relay**. The Tauri client's `connect_and_authenticate`
(`client/src-tauri/src/connection.rs:63`) only does direct connections: it
`endpoint.connect(socket_addr)` and **accepts** the server-opened primary stream
(`:79 accept_bi`). So every client connection is still direct, and the server sees
the client's real IP — Gap #3 is open until the client can route through a relay.

## Goal

The client can connect to a **relayed** server through its relay, so the server
sees only the relay's address. A connection is relayed iff its saved record / link
carries relay info; otherwise the existing direct path runs unchanged. Scope is the
Rust client only — pretty invite links / the invite directory are **Phase 3b**.

## Decisions (settled)

| Decision | Choice |
|----------|--------|
| Relay vs direct | Driven by the connection info: relay info present → relay path; absent → direct path (unchanged). |
| Who opens the primary stream | In relay mode the **client opens** it (so the relay bridges it) and prefixes `RelayStreamRole::Primary`; the server sends `Challenge` first, as in Phase 2. (Direct mode: server opens, client accepts — unchanged.) |
| File transfers over relay | Client opens a relay-bridged bi-stream prefixed with `RelayStreamRole::Session { token }` (the login token), matching the server's demux. |
| Voice over relay | Disabled — a voice attempt on a relayed server returns a clear error (datagrams aren't relayed). UI hiding is Phase 4. |
| Relay trust | **Cert pinning** — the client verifies the relay's cert against a fingerprint carried in the connection info (NOT skip-verify). Security-critical part of this phase. |
| Connection-info format | Raw escape-hatch link `farder://relay/<relay_addr>/<server_id_hex>/<cert_fp_hex>/<invite_token>`; Phase 3b wraps it in pretty links. |

## Architecture

### Shared auth handshake (extraction)

Mirror the Phase 2 server refactor on the client. Extract the auth handshake
(currently inline in `connect_and_authenticate` `:79-109`: read `Challenge`, send
`Authenticate`, read `Authenticated { session_token }`) into a function over a
`(SendStream, RecvStream)` pair, e.g. `run_client_handshake(send, recv, keypair,
invite_code, setup_token) -> Result<Vec<u8> /*session_token*/>`. Both paths reuse
it:

- **Direct** `connect_and_authenticate` (unchanged behavior): `connect(socket)` →
  `accept_bi()` → `run_client_handshake(send, recv, ...)`.
- **Relay** `connect_via_relay` (new): connect to the relay (pinned), `RelayConnect`
  handshake, `open_bi()` the primary, write `RelayStreamRole::Primary`, then
  `run_client_handshake(send, recv, ...)`.

### Relay connect path (`connect_via_relay`)

```
endpoint(pinned to relay_cert_fp).connect(relay_addr, "farder-relay")
 -> open_bi(); write_framed(RelayConnect { destination_id: server_id });
    read RelayConnected   (else surface RelayError reason)
 -> open_bi()  (the primary stream)
 -> write_framed(RelayStreamRole::Primary)
 -> run_client_handshake(send, recv, keypair, invite_code, setup_token) -> session_token
 -> return (relay_conn, primary_send, primary_recv, session_token)
```

Framing for `RelayConnect`/`RelayStreamRole` is the 4-byte big-endian length prefix
used by `connection.rs` `read_frame`/`write_frame` and the relay — confirmed
wire-compatible in Phase 2.

### Cert pinning (`client/src-tauri/src/tls.rs`)

Add a pinning client-endpoint builder alongside the existing `make_client_endpoint`
(which skips verification, for direct dev servers). The pinning verifier accepts the
relay's cert iff `sha256(cert_der) == expected_fingerprint`. `connect_via_relay`
uses the pinned endpoint with the fingerprint from the connection info. (The default
Farder relay's fingerprint can be bundled later; for now it travels in the link.)

### File transfers over relay

`upload_file_internal_with_channel` (`commands.rs:776`, opens `quic_conn.open_bi()`
at `:807`) and `download_file_internal` (`:874`) gain a relay-aware step: when the
`ServerConnection` is relayed, after `open_bi()` on the relay connection, first
`write_framed(RelayStreamRole::Session { token: session_token })`, THEN proceed with
the existing `UploadRequest`/`DownloadRequest` protocol. Direct mode is unchanged.

### `ServerConnection` changes (`client/src-tauri/src/state.rs`)

`connect_server` currently DISCARDS the session token (`let (conn, send, recv,
_session_token) = ...`, `commands.rs:406`). Add to `ServerConnection`:
- `session_token: Vec<u8>` — needed to mark relay `Session` streams.
- `relayed: bool` (or `relay: Option<()>` marker) — so upload/download know to write
  the `Session` marker, and so a voice attempt can be refused.

### `connect_server` branch (`commands.rs:380`)

`connect_server` parses its `address` argument. If it is a relay connection form
(starts with `farder://relay/` or an equivalent struct field), parse
`{ relay_addr, server_id, cert_fp, token }`, build the pinned endpoint, and call
`connect_via_relay`; otherwise the existing direct parse + `connect_and_authenticate`
runs. The resulting `ServerConnection` records `session_token` and `relayed`.

### Saved-server records (`ServerEntry`, `commands.rs:333`)

`ServerEntry` gains optional relay fields so a relayed server reconnects correctly
on relaunch (the parsed relay info is persisted, not just a socket address).

### Voice disabled on relayed servers

The voice join path checks `ServerConnection.relayed` and returns a clear error
("voice is not available over a relay yet") instead of attempting datagrams. No
datagram receive loop is needed for relayed connections.

## File structure

- `client/src-tauri/src/connection.rs` — extract `run_client_handshake`; add
  `connect_via_relay`; the `RelayConnect`/`RelayStreamRole` framing helpers.
- `client/src-tauri/src/tls.rs` — pinning client-endpoint builder + a fingerprint
  helper.
- `client/src-tauri/src/state.rs` — `ServerConnection { session_token, relayed }`.
- `client/src-tauri/src/commands.rs` — parse relay connection form in
  `connect_server`; thread `session_token`/`relayed`; relay-aware upload/download;
  voice-refusal; `ServerEntry` relay fields.
- `crates/farder-protocol` — reuse existing `RelayStreamRole` and `Message::RelayConnect`
  (already present). A small parser for the `farder://relay/...` form may live in the
  client or protocol crate.

## Data flow (relayed message + file)

```
client connect_via_relay --(relay)--> server (relay-mode)
  primary stream: RelayStreamRole::Primary -> auth -> session_token
  request/response/events over the primary stream
client upload: open_bi on relay conn -> RelayStreamRole::Session{token}
  -> UploadRequest + bytes -> stored
Server's remote_address() == relay's addr (Gap #3 closed).
```

## Error handling

- Relay unreachable / handshake fail → surface a clear "could not reach relay" error.
- `RelayError { reason }` from the relay (e.g. server not registered) → surface the
  reason ("server is offline").
- Cert fingerprint mismatch → refuse the connection with a security error (possible
  relay impersonation) — do NOT fall back to skip-verify.
- Auth failure over relay → same `AuthError` surfacing as direct mode.
- Voice attempt on a relayed server → clear "not available over relay" error.

## Testing

Headless (no GUI), driving the **real client connect path** (`connect_via_relay`).

**Crate-boundary note:** `farder-client` (the Tauri binary crate) does NOT depend on
`farder-server`, and the root workspace tests cannot see the client crate. So the
client-side tests live IN the client crate and use **test doubles** for the other
hops (the same approach Phase 2 used — its `relay_mode.rs` uses a relay double):
- a minimal **relay double** (register map + bridge, ~40 lines, mirroring
  `farder-relay`'s router), and
- a minimal **mock server** (a quinn endpoint that, per bridged stream, reads the
  `RelayStreamRole`, runs the server side of the auth handshake — send `Challenge`,
  read `Authenticate`, send `Authenticated{token}` — and records the
  `remote_address()` it observes).

Tests (in `client/src-tauri`, e.g. `connection.rs` `#[cfg(test)]` or a `tests/`
target if a lib target is added):

1. **Gap #3 observation:** drive `connect_via_relay` through the relay double to the
   mock server; assert the mock server observed the **relay double's**
   `remote_address()`, never the client endpoint's, and that the handshake completes
   with a session token. (The REAL server's relay behaviour is already covered by
   Phase 2's `relay_mode.rs`; this asserts the CLIENT half closes the loop.)
2. **File over relay:** with the mock server accepting a `Session{token}` stream,
   assert the client's relay-aware upload writes the `Session` marker + `UploadRequest`
   in the right order (the mock asserts it receives them).
3. **Cert pinning:** the pinning verifier accepts the matching fingerprint and
   rejects a wrong one (unit-testable on the verifier directly; plus an endpoint-level
   test that a wrong fingerprint fails the relay handshake).
4. **Direct mode unchanged:** existing direct client/connection tests still pass; the
   extracted `run_client_handshake` does not alter the direct path.

(The full GUI flow — opening an invite, connecting from the UI — is exercised on the
Windows build once Phase 3b/4 add the user-facing pieces; 3a's connection logic is
verified headlessly here.)

## Out of scope / deferred

- Pretty `farder.com/invite/<code>` links and the invite directory — **Phase 3b**.
- Relay UI (mark a server relayed, relayed-server voice hidden) — **Phase 4**.
- Voice over relay — later phase.
- Bundling the Farder default relay's fingerprint into the client — when the default
  relay is deployed.
