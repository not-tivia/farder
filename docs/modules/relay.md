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
