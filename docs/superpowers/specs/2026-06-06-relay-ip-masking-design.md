# Relay / IP-Masking — Design Spec

**Date:** 2026-06-06
**Status:** Approved (architecture + decomposition); Phase 1 ready to plan
**Audit origin:** `docs/superpowers/audits/2026-06-05-privacy-security-wiring-audit.md` Gap #3 (MED)

## Problem

Farder promises "IP masking via relays," but the relay is dead code: it is never
wired into the client, so every connection is direct and the destination server
sees the client's real IP. The relay crate (`crates/farder-relay`) is also only a
sketch even in isolation — nothing registers a server with it, it bridges streams
but not voice datagrams, it uses a throwaway self-signed cert, and it assumes one
client per server.

## Goal

A working **rendezvous relay**: a server and a client meet at a relay so that
**neither side learns the other's real IP**. The server only ever sees the
relay's address; the client only ever sees the relay's address. Relay use is
**opt-in per server**; direct (non-relayed) servers keep working unchanged.

## Trust model (explicit)

A one-hop relay **shifts trust, it does not remove it**. The relay can see each
side's IP and all non-DM traffic that passes through it (DMs remain end-to-end
encrypted; channel messages and metadata are visible to the relay exactly as they
are to the server). This buys privacy only because the relay is run by a party
you trust *more* than the server you're reaching (or that you self-host). This is
the same posture as trusting Signal's servers to broker connections — but Farder
keeps a self-host escape hatch Signal does not. **Real anonymity (multi-hop /
onion routing) is a non-goal.**

## Decisions (settled in brainstorming)

| Decision | Choice |
|----------|--------|
| What it protects | **Both** sides hidden — neither client nor server sees the other's IP (rendezvous relay) |
| Who runs relays | **Farder default relay + self-host option** — server operators may point at their own |
| Invites | **Short hosted links** (`farder.com/invite/abc123`); a Farder "invite directory" resolves the code to `{relay, server-id, token}`. Self-hosters may use the raw `farder://relay/server-id/token` form to avoid the directory |
| Voice over relay | **Deferred** — relayed servers carry text/files/presence first; voice is a later phase and is cleanly disabled until then |
| Direct servers | **Unchanged and default**; relay is opt-in per server |

## Architecture

### Topology (a relayed server)

```
   Server (no open ports)                                  Client
        │ 1. dials OUT to its relay,                          │ 3. resolves invite,
        │    sends RelayRegister{server_id},                  │    connects to relay,
        │    keeps the connection open                        │    sends RelayConnect{server_id}
        ▼                                                     ▼
     ┌──────────────────────────  RELAY  ──────────────────────────┐
     │  registry: server_id -> server's control Connection          │
     │  on connect: pair client <-> a fresh bi-stream to the server │
     │  copy bytes both ways (blind byte pipe)                      │
     └──────────────────────────────────────────────────────────────┘
   Server's remote_address() == relay. Client never learns server IP. Relay
   sees both IPs + non-DM bytes (accepted trust model).
```

The client↔server protocol already runs over essentially **one bidirectional
stream** (a single send/recv pair multiplexing requests, responses, and events by
id; voice uses separate QUIC datagrams). That is what makes a byte-pipe relay
viable: per client, the relay bridges one client bi-stream to one server-side
bi-stream, and the server treats each such stream as a client session. Voice
datagrams are **not** bridged in v1 (deferred).

### Addressing + invite directory

- A relayed server's connection info is `{relay_addr, server_id, invite_token}`.
- **Invite directory** (small Farder-run web service, part of `website/`): the
  server registers `code -> {relay_addr, server_id, token}` when it mints an
  invite; the client resolves `farder.com/invite/<code>` to that record via an
  API, then connects through the relay. This is the Discord model.
- **Escape hatch:** the raw `farder://<relay_addr>/<server_id>/<token>` deep-link
  works without the directory, for self-hosters who want zero Farder involvement.
- **Directory privacy note:** the directory learns *which server a code maps to*
  and *who resolves codes* (resolver IP). It never sees message content, PINs, or
  DMs. Keep its storage and logging minimal.

## Component changes (whole feature)

- **Relay** (`crates/farder-relay`): registration protocol + registry lifecycle,
  per-client bi-stream bridging, a stable verifiable cert. *(Phase 1.)*
- **Server** (`crates/farder-server`): a relay mode — dial out, register, and
  serve each client over a bridged stream instead of accepting direct
  connections. *(Phase 2 — the meatiest change.)*
- **Client** (`client/src-tauri`): a connect-via-relay path; resolve the new
  invite/saved-server format. *(Phase 3 — closes Gap #3.)*
- **Invite directory** (`website/` + a resolve API): map codes to connection
  info. *(Phase 3, alongside addressing.)*
- **UI**: mark a server relayed (default vs custom relay) at create/edit. *(Phase 4.)*
- **Default relay deployment**: stand up the hosted relay (ops). Code uses a
  configurable default address; the actual deployment is separate and may lag
  (self-hosters point at their own meanwhile).

## Decomposition (each its own spec → plan → implement)

1. **Harden the relay** into a working, tested rendezvous server. *(This spec
   details it below.)*
2. **Server relay-mode** — dial out, register, serve clients over the relay.
3. **Client relay-mode + invite directory + addressing** — connect via relay;
   resolve clean invite links. **Closes audit Gap #3** with an observation test.
4. **UI** — mark a server relayed (default vs custom relay).
5. *(Later)* voice over relay; and, separately, deploy the hosted default relay.

We design phases 2–5 when we reach them.

---

## Phase 1 — Harden the relay (implementable scope)

**Goal:** turn `crates/farder-relay` from a sketch into a working, tested
rendezvous server: servers can register, clients can connect by `server_id`, and
bytes bridge correctly both ways — provable by an integration test using real
QUIC endpoints. No client/server integration yet (those are Phases 2–3).

### Protocol additions (`crates/farder-protocol/src/messages.rs`)

Add two variants to `Message` (keep the existing `RelayConnect`/`RelayConnected`/
`RelayError`):

```rust
RelayRegister { server_id: Vec<u8> },
RelayRegistered,
```

`server_id` is an opaque routing key in Phase 1 (the test supplies one). Phase 2
binds it to the server's stable public identity. Add a codec round-trip test for
both new variants.

### Relay behaviour (`crates/farder-relay/src/router.rs`)

- **Register flow:** when the first message on a new connection is
  `RelayRegister { server_id }`, insert `server_id -> Connection` into the
  registry, reply `RelayRegistered`, and keep the connection open as the server's
  control connection. A new registration for an existing `server_id` **replaces**
  the old entry (server reconnect; log a warning). Remove the entry when the
  connection closes (await `conn.closed()` in a cleanup task).
- **Connect flow:** when the first message is `RelayConnect { destination_id }`,
  look up the registry. If present, reply `RelayConnected`, then **bridge**. If
  absent, reply `RelayError { reason: "destination not connected" }` and close.
- **Bridging (per client):** loop `accept_bi()` on the **client** connection; for
  each client bi-stream, `open_bi()` a fresh stream on the **server** control
  connection and copy bytes in both directions (`tokio::io::copy` each way), so
  every client request stream maps to its own server-side stream. Tear the pair
  down cleanly when either side's stream ends. (Replaces the current
  `bridge_one_direction` loop, which opens a server stream per *direction* and
  mishandles pairing.)
- Respect `max_connections` and the existing 16 MiB message-size cap.

### Stable cert (`crates/farder-relay/src/listener.rs`)

Today `create_endpoint` generates a throwaway self-signed cert on every boot, so
a client can never pin the relay. Change it to **load a persisted cert/key from
the relay's data dir, generating and saving one on first run** (so the relay has
a stable identity that Phase 3 can pin in the invite/directory record). Phase 1's
tests trust the test relay's cert directly.

### `main.rs` wiring

`handle_connection` must branch on whether the first message is `RelayRegister`
(server) or `RelayConnect` (client). The accept loop already spawns a task per
incoming connection; registration just keeps that task alive holding the control
connection until close.

### Tests (`crates/farder-relay` integration test, real QUIC on ephemeral ports)

1. **Round-trip bridge:** start the relay; a mock "server" endpoint dials it and
   sends `RelayRegister{id}`, then echoes any bytes it receives on accepted
   bi-streams; a mock "client" endpoint dials the relay, sends
   `RelayConnect{id}`, gets `RelayConnected`, opens a bi-stream, writes a payload,
   and reads the echo back. Assert the payload round-trips through the relay.
2. **Unknown destination:** client `RelayConnect{unknown_id}` → receives
   `RelayError`.
3. **Registry cleanup:** after the mock server disconnects, a subsequent
   `RelayConnect{id}` → `RelayError` (entry was removed).
4. **Re-registration replaces:** registering the same `server_id` twice routes to
   the second connection.
5. **Protocol codec:** round-trip `RelayRegister`/`RelayRegistered`.

### Phase 1 explicitly NOT included

- No server or client code changes (Phases 2–3).
- No datagram/voice forwarding (deferred).
- No invite directory / addressing (Phase 3).
- No cert *pinning* on the client side yet (Phase 3); Phase 1 only makes the
  relay cert stable.

## Verification of the overall goal

The audit Gap #3 close happens in **Phase 3** via an observation test: a client
connecting through the relay to a real server, asserting the server observes the
relay's `remote_address()`, never the client's. Phase 1's tests prove the relay
mechanics; the privacy guarantee is asserted once the real client/server paths
exist.

## Out of scope / deferred

- Multi-hop / onion routing (anonymity) — non-goal.
- Voice over relay — later phase.
- Hosted default-relay deployment — ops task, may lag the code.
- Relay-side abuse controls (rate limiting, auth of who may register) — revisit
  before running a public default relay; not needed for Phase 1's local mechanics.
