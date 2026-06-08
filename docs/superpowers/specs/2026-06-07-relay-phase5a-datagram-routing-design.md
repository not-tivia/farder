# Relay Phase 5a — Datagram Routing Core — Design Spec

**Date:** 2026-06-07
**Status:** Approved (design); ready to plan
**Parent design:** `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md`
**Depends on:** Phase 1 relay (RelayRegister/RelayConnect/bridge, persistent cert) and
Phase 2 server relay-mode — both merged.

## Problem

Voice is **disabled over relayed servers** (Phases 3a/4). Voice media travels as
**QUIC datagrams** (unreliable, low-latency — a lost packet is a glitch, not a
stall). But the relay only bridges **bidirectional streams** (`tokio::io::copy` in
`bridge_client`); it has **no datagram handling at all**, and its QUIC endpoint
doesn't even enable datagrams. So a relayed client's voice packets reach the relay
and stop.

The structural obstacle: datagrams are **per-QUIC-connection**. The relay holds one
connection to each registered server (shared by *all* that server's relayed
clients) and one connection to each client. A datagram has no routing information —
when the relay receives a voice packet on a client's connection, nothing tells it
which server connection to forward on; when it receives one on a server's
connection, nothing tells it which of that server's many clients it's for. Voice
fan-out on the server side (Phase 1 direct mode) sends per-recipient using each
client's own `quinn::Connection` — which doesn't exist over the relay.

## Goal & scope boundary

Build the **relay half** of voice-over-relay: teach the relay to forward and route
voice datagrams between a server and its relayed clients, using a small per-client
routing handle. This is the hard, novel, **headlessly testable** core.

**Explicitly out of scope (deferred to Phase 5b):** the server's voice fan-out
rewired to send/receive over the relay's single connection using these handles; the
client wiring; and any **real-audio / end-to-end** verification (needs a deployed
relay + two real clients + audio — impossible in WSL). 5a ships the relay
capability plus headless tests with **doubles** (a fake server + fake client
exchanging tagged datagrams through the real relay).

A hard constraint on 5a: **it must not break a Phase-2 server that knows nothing
about handles or datagrams.** See "Backward compatibility" below.

## Decisions (settled)

| Decision | Choice |
|----------|--------|
| Media transport over relay | **Datagrams** (unreliable), forwarded blind. NOT voice-over-a-reliable-stream (head-of-line blocking would stall audio). |
| Per-client routing id | A relay-assigned **`u32` handle**, unique relay-wide, assigned when a client connects via `RelayConnect`. |
| Handle announce channel | A **reliable** relay→server control stream (one per registered server), carrying `RelayClientConnected`/`RelayClientDisconnected { handle }`. NOT over the lossy datagram path — a dropped announce would strand a client (esp. listen-only). |
| Datagram tagging | A **4-byte big-endian `u32` handle prefix** on every media datagram crossing the relay↔server connection. Client↔relay datagrams are untagged (the relay knows the client by its connection). |
| Relay reads media? | **No.** The relay forwards the encrypted media bytes blind (privacy preserved — it never holds a stream key). |
| Server cooperation | A non-5b server must keep working: the control stream errors-and-drops harmlessly, forwarded datagrams are ignored. |

## Architecture

Three new relay capabilities, plus enabling datagrams on the endpoint.

### 0 — Enable datagrams on the relay endpoint (`listener.rs`)

`create_endpoint` builds a `quinn::TransportConfig` (Phase-1/DR `max_idle_timeout`
already set). Add datagram support: `transport.datagram_receive_buffer_size(Some(N))`
and `transport.datagram_send_buffer_size(N)` (N a sensible default, e.g. 1 MiB
recv). Without this, `read_datagram`/`send_datagram` on relay-side connections fail
and the peer sees datagrams as unsupported.

### 1 — Per-client routing handles (`router.rs`)

A relay-wide registry of live client connections by handle:

- `next_handle: Arc<AtomicU32>` — monotonic; `fetch_add(1)` per admitted client.
  Handle `0` is reserved/never assigned (so it can never collide with a real
  client; it also reads as "no handle" in logs).
- `clients: Arc<RwLock<HashMap<u32, Connection>>>` — handle → that client's
  `quinn::Connection`. Inserted in `handle_connect` right after the
  `RelayConnect`/`RelayConnected` match succeeds and the destination server is
  found; removed when the client's connection/bridge ends.

These are created in `new_connection_map`'s sibling (a new `RelayState` or added
fields threaded through `handle_connection`). To keep the change contained, the
registry of servers and the new client/handle maps are grouped into one
`RelayState` struct passed where `ConnectionMap` is passed today.

### 2 — Per-server control stream + handle announcements (`router.rs`, `farder-protocol`)

When a server registers (`handle_register`, after `RelayRegistered` is sent), the
relay **opens one dedicated control bi-stream to that server** and keeps its
`SendStream`. The server's registry entry becomes a struct:

```rust
struct RegisteredServer {
    conn: Connection,
    control: Arc<Mutex<SendStream>>, // relay -> server control channel
}
```

(`ConnectionMap` becomes `HashMap<Vec<u8>, RegisteredServer>`.)

Two new `farder-protocol` `Message` variants (length-framed like the existing relay
messages, encoded via `codec`):

```rust
RelayClientConnected { handle: u32 },
RelayClientDisconnected { handle: u32 },
```

- On a client connecting (`handle_connect`, after assigning handle `h` and locating
  the destination server): write `RelayClientConnected { handle: h }` to that
  server's control stream (length-framed). This tells the server "a new relayed
  client exists on lane `h`" reliably — even a client who never speaks gets a lane,
  so the server can fan voice *to* them.
- On that client's connection/bridge ending: write
  `RelayClientDisconnected { handle: h }` and remove `h` from `clients`.

The control stream is **distinct** from bridged client streams: it's opened **by the
relay** (server side never writes a `RelayStreamRole` on it) and carries only these
control messages. 5b's server reads it; a Phase-2 server treats the unexpected
stream as a decode error and drops that one stream (harmless — see Backward compat).

### 3 — Forward path: client → relay → server (`router.rs`)

When bridging a client (`handle_connect` → alongside `bridge_client`), spawn a
datagram-forward task for that client:

```text
loop {
    let dg = client_conn.read_datagram().await?;   // ends when client disconnects
    let mut tagged = Vec::with_capacity(4 + dg.len());
    tagged.extend_from_slice(&handle.to_be_bytes()); // source lane
    tagged.extend_from_slice(&dg);
    let _ = server_conn.send_datagram(tagged.into()); // best-effort; drop on error
}
```

The server thus receives every relayed voice packet prefixed with the **source**
client's handle. (5b's server uses the source handle for bookkeeping; the media
frame itself still self-identifies the speaker, so 5a doesn't depend on the server
trusting the tag for correctness — it's routing metadata.)

### 4 — Route path: server → relay → client (`router.rs`)

When a server registers, spawn one datagram-route task on its connection:

```text
loop {
    let dg = server_conn.read_datagram().await?;   // ends when server disconnects
    if dg.len() < 4 { continue; }                  // malformed; drop
    let handle = u32::from_be_bytes(dg[0..4]);
    let payload = dg.slice(4..);
    if let Some(client_conn) = clients.read().get(&handle).cloned() {
        let _ = client_conn.send_datagram(payload);  // best-effort
    } // unknown handle (client gone) -> drop silently
}
```

The server fans out by sending one datagram per recipient, each prefixed with that
**recipient's** handle; the relay strips the prefix and delivers the encrypted
payload to that client's connection.

### Lifetime / cleanup

- Client handle is removed and `RelayClientDisconnected` announced when
  `read_datagram` (or the bridge) returns `Err`/the connection closes. A single
  cleanup point (e.g. when the client's bridge task exits) owns both, so the handle
  is never leaked and the announce is sent exactly once.
- The forward task ends naturally when `client_conn.read_datagram()` errors (client
  gone). The route task ends when `server_conn.read_datagram()` errors (server
  gone); on server loss, its `RegisteredServer` entry is already removed by the
  existing Phase-1 registry cleanup.
- `send_datagram` errors are **best-effort dropped** (logged at debug), never fatal
  to the loop — voice tolerates loss, and a peer that hasn't enabled datagrams
  (a non-5b client/server) simply never receives them.

## Backward compatibility (must-hold invariant)

A Phase-2 server (no 5b changes) connects to a 5a relay. The relay will: open a
control stream to it and forward datagrams to it. This **must not break** existing
relayed text/file traffic:

- **Control stream:** the Phase-2 server's `serve_relay_stream` accepts the stream
  and tries to read a `RelayStreamRole` first frame; the control message fails that
  decode, so the server logs and drops **that one stream**. Other bridged client
  streams (real sessions) are unaffected. Verified by re-running the existing
  Phase-2 `relay_mode` integration tests unchanged.
- **Forwarded datagrams:** the Phase-2 server never calls `read_datagram` on its
  relay connection, so tagged datagrams are simply never read (QUIC drops them when
  the recv buffer fills). No error surfaces to the server's stream handling.
- **Existing relay tests** (`farder-relay` router tests, `farder-server`
  `relay_mode`) must still pass. The plan re-runs them as a gate.

So 5a is purely additive at the wire level: a 5b-aware server uses the control
stream + datagrams; a 5b-unaware server ignores them.

## Protocol additions (`farder-protocol/src/messages.rs`)

```rust
RelayClientConnected { handle: u32 },
RelayClientDisconnected { handle: u32 },
```

Encode/decode round-trip unit tests alongside the existing relay-message tests.

## File structure

- `crates/farder-protocol/src/messages.rs` — two new `Message` variants + tests.
- `crates/farder-relay/src/listener.rs` — enable datagrams on the endpoint.
- `crates/farder-relay/src/router.rs` — `RelayState` (servers + clients + handle
  counter); `handle_register` opens/holds the control stream and spawns the
  route task; `handle_connect` assigns the handle, inserts the client, announces
  connect, spawns the forward task; cleanup announces disconnect + removes the
  handle. `ConnectionMap`/`RegisteredServer` reshaped.
- `crates/farder-relay/src/datagram.rs` *(new, optional)* — the forward/route loop
  helpers, kept out of `router.rs` if it grows unwieldy (router.rs is already the
  largest relay file). Pure functions over `(Connection, Connection, handle,
  clients)` so they're unit-testable and `router.rs` stays focused.
- `crates/farder-relay/tests/datagram_routing.rs` *(new)* — the doubles integration
  test (below).

## Testing (headless)

All 5a behavior is testable without audio or GUI, using real QUIC loopback + doubles
(the pattern Phase 1/2/3a already use):

1. **Protocol round-trip:** `RelayClientConnected`/`Disconnected` encode→decode.
2. **Handle assignment + announce (integration):** a fake server connects +
   `RelayRegister`s, then reads its control stream. A fake client connects +
   `RelayConnect`s. Assert the fake server receives `RelayClientConnected { handle }`
   with a non-zero handle. Disconnect the client; assert
   `RelayClientDisconnected { handle }` for the same handle.
3. **Forward path (client→server):** the fake client `send_datagram(payload)`;
   assert the fake server reads a datagram equal to `handle.to_be_bytes() ++ payload`
   (correct source tag + intact bytes).
4. **Route path (server→client):** the fake server `send_datagram(handle ++ E)`;
   assert the fake client reads exactly `E` (prefix stripped, delivered to the right
   client). With **two** fake clients (handles h1, h2), assert a datagram tagged h2
   reaches client 2 and **not** client 1 (routing is selective).
5. **Unknown-handle drop:** the fake server sends a datagram tagged with a handle
   that was never assigned; assert no panic and the live client receives nothing.
6. **Backward-compat gate:** the existing `farder-relay` router tests and
   `farder-server` `relay_mode` integration tests pass unchanged.

The doubles enable datagrams on their own endpoints; that's what makes 3–5
observable. Real Phase-3a clients / Phase-2 servers don't enable datagrams yet —
that's 5b.

## Out of scope / deferred (Phase 5b and beyond)

- **Server voice-over-relay:** the server reading tagged datagrams off its relay
  connection, mapping source/recipient handles ↔ members, and fanning out by writing
  `handle ++ frame` datagrams to the relay instead of per-client connections;
  enabling datagrams on the server's relay-client endpoint; reading the relay
  control stream to learn handles.
- **Client wiring:** enabling datagrams on the pinned relay endpoint
  (`make_pinned_relay_endpoint`), spawning the voice datagram recv loop for relayed
  connections, removing the "voice not available over relay" refusal.
- **Stream-key handling for relayed voice** (the offer already rides the bridged
  control stream; confirm end-to-end in 5b).
- **Real-audio / two-client end-to-end verification** — user's Windows + deployed
  relay step.
- **Voice UI re-enable** on relayed servers (Phase 4 disabled it).
