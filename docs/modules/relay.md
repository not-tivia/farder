# Module: relay (`crates/farder-relay`)

**Purpose:** the Farder rendezvous relay. Servers register; clients connect by
server id; the relay bridges their streams and (Phase 5a) routes voice datagrams,
so neither side learns the other's IP. It forwards encrypted bytes blind.

See the umbrella design `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md`.

## Connection lifecycle

- `listener::create_endpoint(bind, data_dir)` -- QUIC server endpoint with a
  persistent self-signed cert (`relay_cert.der`/`relay_key.der`, key 0600), a 60s
  idle timeout, and **datagrams enabled** (1 MiB send/recv buffers).
- `router::serve(endpoint, state, limiter)` -- accept loop, gated by the abuse
  limiter (global cap + per-IP rate). Each connection's first bi-stream carries a
  `RelayRegister { server_id }` (server) or `RelayConnect { destination_id }` (client).

## State (`router::RelayState`, `SharedState = Arc<RelayState>`)

- `servers: RwLock<HashMap<Vec<u8>, RegisteredServer>>` -- registered servers by id.
  `RegisteredServer { conn, control }` where `control` is the relay->server control
  stream (`Arc<Mutex<SendStream>>`).
- `clients: RwLock<HashMap<u32, Connection>>` -- live relayed clients by routing handle.
- `next_handle: AtomicU32` -- monotonic handle allocator (starts at 1; 0 is reserved).

## Datagram routing (Phase 5b)

Each relayed client gets a `u32` **handle** at `RelayConnect`. The relay:
- **stamps each bridged stream** with the client's 4-byte big-endian handle
  (`[handle: u32 BE]`) as a prefix written before the `RelayStreamRole` frame, so
  the server can bind the handle to the authenticated member (authoritative
  correlation -- no control-stream announce needed);
- **forwards** client->server datagrams tagged `[handle:u32 BE][payload]`
  (`datagram::forward_client_datagrams`);
- **routes** server->client datagrams by stripping the `[handle]` prefix and
  delivering to that client's connection (`datagram::route_server_datagrams`);
  unknown handles are dropped.

The relay never reads the media payload (privacy preserved). Datagram sends are
best-effort (dropped if a peer hasn't enabled datagrams -- e.g. a pre-5b server/client).

## Invite-preview proxy (relay fetch proxy, phase one)

The relay now accepts a third connection role in addition to `RelayRegister` and
`RelayConnect`: `ProxyInvitePreview`. A client sends this as the first message on
a fresh connection to ask the relay to fetch an invite preview on its behalf. The
relay dispatches it to `handle_preview` (in `router.rs`), which applies rate
limiting, the TTL cache, and a 5 s fetch budget, then replies with
`ProxyInvitePreviewResult` and closes the connection.

The relay doubles as a privacy fetch proxy: the client's IP never reaches the
target server. This is the same QUIC infrastructure the relay uses for normal
client bridging, extended with a short-lived anonymous connection type.

See `docs/modules/relay-proxy.md` for the full guardrail reference (SSRF, rate
bucket, timeout, code-length cap, cache pressure valve).

## serve_relay_stream — handle-0 gating (relay-originated streams)

When the relay opens a bi-stream on the server's control connection to fetch a
preview, it prefixes the stream with `0u32` (4 bytes big-endian) as the routing
handle — the **reserved handle-0 sentinel**. The relay's client-handle allocator
starts at 1, so no real client stream ever carries handle 0. The server's
`serve_relay_stream` reads this prefix before the `RelayStreamRole` frame and
uses it to distinguish relay-originated preview streams from client streams.

## run_relay_primary — cleanup-then-bail rule

If a `RelayStreamRole::Primary` stream arrives stamped with handle 0 and the
call to `authenticate()` somehow succeeds (only possible if a malicious relay
operator sends a forged 0-stamped stream that carries a real client auth frame),
`run_relay_primary` performs full session cleanup — removes the client from
`state.clients`, `state.voice_connections`, and `state.relay_voice_handles`,
revokes the session token — and then bails with an error. Without this cleanup
the ghost entry would inflate the online count and the session token would never
expire. Preview streams that hit the `GetInvitePreview` arm of `authenticate()`
bail before registration occurs; this guard is the backstop for the case where a
non-preview frame slips through on a 0-stamped stream.

## Backward compatibility

A server/client that predates Phase 5a keeps working: the relay's control stream is
just an unexpected stream the server logs and drops per-task; forwarded datagrams it
never reads are dropped by QUIC. No relayed text/file traffic is affected.

## Protocol messages (`farder-protocol`)

`RelayRegister`/`RelayRegistered`, `RelayConnect`/`RelayConnected`/`RelayError`.
The Phase-5a `RelayClientConnected`/`RelayClientDisconnected` control-stream
announce messages were REMOVED in Phase 5b; the handle stamp on the bridged
stream replaces them.

## Out of scope (5b-client)

The client enabling datagrams on its pinned relay endpoint, running the relayed
recv loop, dropping the `voice_join` refusal, and un-greying the voice UI. These
are deferred to Phase 5b-client and are UNVERIFIED until a Windows + deployed-relay
run.

## Tests

`crates/farder-relay/src/router.rs` `#[cfg(test)] mod tests` (real-QUIC loopback,
doubles): bridged-stream handle stamp, datagram forward tagging, selective routing
between two clients, unknown-handle drop, plus the Phase-1/2 bridge/registry tests
(the backward-compat checkpoint). `farder-server`'s `relay_mode` integration tests
are the cross-crate backward-compat gate.
