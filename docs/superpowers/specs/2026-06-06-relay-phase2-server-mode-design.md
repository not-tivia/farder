# Relay Phase 2 — Server Relay-Mode — Design Spec

**Date:** 2026-06-06
**Status:** Approved (design); ready to plan
**Parent design:** `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md` (Phase 2)
**Depends on:** Phase 1 (relay rendezvous server) — DONE, merged to `main`.

## Problem

The relay (Phase 1) can register a server and bridge client streams to it, but the
**server cannot yet operate through a relay**. Today `connection::handle_connection`
(`crates/farder-server/src/connection.rs:407`) assumes a direct `quinn::Connection`
(one connection = one client): it `open_bi()`s the primary control stream
(`:408`), runs the auth handshake (`:409-505`) and `main_loop` (`:768`), and
separately accepts auxiliary bi-streams for file transfer (`:559` → `handle_auxiliary_stream` `:381`)
and handles voice datagrams (`:596`/`:684`). Over a relay, all clients' streams
arrive multiplexed on the server's single relay connection with no per-connection
grouping, so this model does not work as-is.

## Goal

Let a server run in **relay-only mode**: dial a relay, register under a stable
`server_id`, and serve each relay-bridged stream as a client session — reusing the
existing auth + `main_loop` + file-transfer code. Direct mode is unchanged.

## Decisions (settled in brainstorming)

| Decision | Choice |
|----------|--------|
| Direct + relay coexistence | **Relay-only** — a relayed server binds NO public listener (a direct listener would leak its IP). No relay address → direct mode, untouched. |
| File attachments over relay | **Carried** (not deferred) via the session-token demux below. |
| Stream demux over relay | **Approach A — session tokens.** Each relay-bridged stream opens with a role marker; primary streams authenticate and register a token; file streams present the token so the server knows whose they are. |
| Voice over relay | **Deferred** — relay-mode does no datagram handling. |
| How relay-mode is turned on | A `--relay <addr>` server arg this phase; friendly UI is Phase 4. |
| `server_id` | A stable 32-byte id persisted in the server's data dir on first run; registered with the relay and (Phase 3) encoded in invites. |
| Relay trust | Server accepts the relay's cert for now; pinning the relay identity is Phase 3. |

## Architecture

### Relay-mode serve loop

When `--relay <addr>` is set, the server does NOT call `make_server_endpoint` /
bind a listener. Instead it:

1. Loads-or-generates its stable `server_id` from the data dir.
2. Opens a QUIC **client** connection to the relay, opens a bi-stream, sends
   `Message::RelayRegister { server_id }`, and reads `RelayRegistered`.
3. Loops `relay_conn.accept_bi()` — **each accepted bi-stream is one client's one
   stream**, bridged by the relay. Spawn a handler per stream.
4. On relay-connection loss, reconnect (re-register) with backoff.

### Per-stream role marker

Every relay-bridged stream opens with a small first frame identifying its role
(a new protocol type, e.g. `RelayStreamRole`):

- **`Primary`** → run the normal client session: the existing auth handshake
  (challenge → verify → `ServerFrame::Authenticated { session_token }`), **store
  the token** in the session registry, then `main_loop`. When the session ends,
  **remove the token**.
- **`Session { token }`** → a file-transfer stream: look the token up in the
  registry to recover `{ member_key, is_owner }`, then run the existing
  `handle_auxiliary_stream` logic (UploadRequest / DownloadRequest).

The role marker is **relay-mode only**. Direct mode keeps its current shape
(server-opened primary stream; aux streams trusted by connection and dispatched by
decoding UploadRequest/DownloadRequest), so the direct path needs no marker.

### Session registry

`ServerState` gains a session registry: `Mutex<HashMap<[u8; 32], SessionInfo>>`
where `SessionInfo { member_key: PublicKey, is_owner: bool }`. The server already
mints a token via `auth::generate_session_token()` (`auth.rs:13`) and sends it in
`ServerFrame::Authenticated` (`connection.rs:505-509`) — today it is unused. This
phase gives it a job:

- **Insert** `token -> SessionInfo` right after a successful primary auth.
- **Remove** it when that primary session ends (handler returns / stream closes).
- **Look up** by token to authorize relay file-transfer streams.

The registry is populated in BOTH modes (uniform), but only **relay** file streams
consult it; direct aux streams stay connection-trusted. The registry is the
per-client grouping mechanism the relay does not provide.

### The refactor (the meaty part)

Extract the per-client core from `handle_connection` into a reusable handler over a
`(SendStream, RecvStream)` pair. Concretely:

- `authenticate(state, &mut send, &mut recv) -> Result<AuthOutcome>` where
  `AuthOutcome { member_key, is_owner, session_token, event_rx }` — the current
  Steps 1-4 + token mint + `Authenticated` reply + event-channel setup/registration
  (today inline in `handle_connection`).
- `run_primary_session(state, &mut send, &mut recv) -> Result<()>` — calls
  `authenticate`, inserts the token into the registry, runs `main_loop`, and removes
  the token on exit.
- `handle_connection(state, conn)` (DIRECT, behavior unchanged): `conn.open_bi()` →
  `run_primary_session` over that stream, PLUS the existing aux-stream accept loop
  (`:559`) and datagram loop (`:596`) it already spawns.
- `serve_via_relay(state, relay_addr)` (NEW): the relay-mode serve loop; per accepted
  stream, read the role marker and dispatch to `run_primary_session` (Primary) or the
  token-validated `handle_auxiliary_stream` (Session).

Because `main_loop` (`:768`) and the file handlers (`:381`, `handle_upload_stream`,
`handle_download_stream`) are **already `(send, recv)`-based and do not touch the raw
`Connection`**, this is an extraction, not a rewrite. The only `Connection`-specific
features (the aux accept loop and datagrams) stay in the direct `handle_connection`;
relay mode replaces them with the per-stream dispatch + the token registry.

### File structure

- `crates/farder-protocol/src/messages.rs` (or `server.rs`) — add `RelayStreamRole`
  (`Primary` | `Session { token: Vec<u8> }`) + codec test.
- `crates/farder-server/src/state.rs` — add the session registry to `ServerState`.
- `crates/farder-server/src/connection.rs` — extract `authenticate` /
  `run_primary_session`; keep direct `handle_connection` behavior; add the
  token-validated relay aux path.
- `crates/farder-server/src/relay.rs` *(new)* — `serve_via_relay` (dial, register,
  accept-loop, per-stream dispatch, reconnect/backoff) + server-id persistence.
- `crates/farder-server/src/main.rs` — `--relay <addr>` arg; if set, run
  `serve_via_relay` instead of binding a direct listener.
- `crates/farder-server/src/config` / args — the `--relay` flag.

## Data flow (relayed client request + file upload)

```
Client --(via relay)--> Server relay_conn.accept_bi()  [stream #1]
  stream #1: RelayStreamRole::Primary
    -> server sends Challenge; client Authenticates; server verifies
    -> server mints token, sends Authenticated{token}, registry.insert(token -> {member,owner})
    -> main_loop: requests/responses/events over stream #1
Client opens another stream --(via relay)--> accept_bi()  [stream #2]
  stream #2: RelayStreamRole::Session{token}
    -> registry.lookup(token) -> {member, owner}
    -> read UploadRequest -> handle_upload_stream(member, owner, send, recv, req)
When stream #1 ends -> registry.remove(token)
```

The server's view of every relayed client is its relay connection only; it never
learns a client's real address (the privacy property, asserted end-to-end in Phase 3).

## Error handling

- Unknown/expired token on a `Session` stream → error frame + close that stream
  (does not affect the primary session).
- Relay connection lost → tear down in-flight relayed sessions for that connection,
  reconnect to the relay with capped backoff, re-register.
- Malformed role marker / first frame → close the stream with a warning.
- Auth failure on a Primary stream → existing `ServerFrame::AuthError` path, close.

## Testing (headless — no GUI)

Integration tests over real QUIC: start a Phase-1 relay, start a server in
relay-mode (it registers), and drive a **simulated client through the relay**:

1. **Login + request:** client connects to the relay, `RelayConnect{server_id}`,
   opens a stream, sends `RelayStreamRole::Primary`, completes the auth handshake,
   issues a simple `ServerRequest`, and gets the expected response.
2. **File over relay:** after login, open a second stream,
   `RelayStreamRole::Session{token}`, send an `UploadRequest` + bytes, and assert
   the file is stored (and downloadable via a `Session` download stream).
3. **Bad token:** a `Session` stream with a random token is rejected without
   affecting the primary session.
4. **Direct mode unchanged:** existing direct-connection server tests still pass
   (regression guard) — the refactor must not alter direct behavior.
5. **Server identity persists:** `server_id` is stable across restarts (same dir).

## Out of scope / deferred

- Voice over relay (datagrams) — later phase.
- Client-side relay support and the invite/addressing changes — **Phase 3** (which
  also adds the Gap #3 observation test: server sees the relay's address, never the
  client's).
- Relay UI — Phase 4.
- Pinning the relay's cert from the server — Phase 3.
- Relay abuse controls — deferred (parent spec out-of-scope note).

## Note on size

Phase 2 is the largest sub-project (refactor + registry + relay serve loop +
file-over-relay + identity/config + tests). The implementation plan may split it
into ordered tasks (e.g., protocol marker → session registry → extract session
handler → relay serve loop + dispatch → file-over-relay → server config/identity →
integration tests), each leaving the build green and direct-mode tests passing.
