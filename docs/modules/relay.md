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

## Datagram routing (Phase 5a)

Each relayed client gets a `u32` **handle** at `RelayConnect`. The relay:
- announces `RelayClientConnected { handle }` / `RelayClientDisconnected { handle }`
  to the destination server over its reliable control stream (so the server learns
  every client's lane, even silent listeners);
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

`RelayRegister`/`RelayRegistered`, `RelayConnect`/`RelayConnected`/`RelayError`,
and the Phase-5a `RelayClientConnected { handle }` / `RelayClientDisconnected { handle }`.

## Out of scope (Phase 5b)

The server reading/producing tagged datagrams (voice fan-out over the relay), the
client enabling datagrams on its pinned relay endpoint and re-enabling voice, and
real-audio end-to-end verification. 5a is the relay half only, tested headlessly
with doubles in `router.rs`'s `mod tests`.

## Tests

`crates/farder-relay/src/router.rs` `#[cfg(test)] mod tests` (real-QUIC loopback,
doubles): handle announce on connect/disconnect, datagram forward tagging, selective
routing between two clients, unknown-handle drop, plus the Phase-1/2 bridge/registry
tests (the backward-compat checkpoint). `farder-server`'s `relay_mode` integration
tests are the cross-crate backward-compat gate.
