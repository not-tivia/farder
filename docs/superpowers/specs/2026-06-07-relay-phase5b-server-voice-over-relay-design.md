# Relay Phase 5b (server core) — Voice Over Relay — Design Spec

**Date:** 2026-06-07
**Status:** Approved (design); ready to plan
**Parent design:** `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md`
**Builds on:** Phase 5a (relay datagram-routing core) — merged. Relay assigns each
relayed client a `u32` handle, forwards client->server voice datagrams tagged
`[handle BE][payload]`, and routes server->client datagrams by stripping the handle
prefix (`crates/farder-relay/src/datagram.rs`).

## Problem

Voice still does not work on relayed servers. Phase 5a taught the *relay* to forward
and route voice datagrams by per-client handle, but neither the **server** nor the
**client** uses that capability yet. The server's voice fan-out
(`crates/farder-server/src/connection.rs` ~677-802) reads datagrams **per
connection** and sends to each recipient via that recipient's **own**
`quinn::Connection` (`voice_connections: HashMap<[u8;32], Connection>`). A relayed
client has **no per-client connection on the server** — every relayed client shares
the server's single connection to the relay. So the server can neither receive a
relayed client's voice nor fan voice out to relayed recipients.

This phase builds the **server half** so voice flows end-to-end over the relay's
datagram routing. The **client half** (enabling datagrams on the pinned relay
endpoint, the relayed recv loop, dropping the `voice_join` refusal, un-greying the
UI) and **real-audio verification** are deferred to a follow-up (5b-client), because
none of that can be exercised in this environment (no audio, no GUI, no deployed
relay).

## The central problem: handle <-> member correlation

To fan voice out over the relay, the server must tag each outgoing datagram with the
**recipient's** handle, and to authenticate incoming voice it must know which member
a **source** handle belongs to. The server identifies members by their
**authenticated public key** on the bridged primary stream; the relay assigns the
handles. These must be bound **securely** — a relayed client must not be able to
claim another client's handle (which would let it hijack or spoof another member's
audio).

**Decision — authoritative handle stamp on the bridged stream.** The relay, which
is the authority on handles, **stamps the client's handle (`[handle: u32 BE]`) onto
every bridged stream** it opens to the server (prepended before the copied client
bytes, in `bridge_client`). The server reads those 4 bytes first on every relay
stream, then the existing `RelayStreamRole` frame. On the **primary** stream, after
the client authenticates, the server binds `handle <-> connection_pk`. A relayed
client cannot forge this: the prefix comes from the relay, not from client-controlled
bytes. This works for **every** member, including silent listen-only members (each
has a primary stream), which a "learn the handle from the first voice frame" approach
would not.

**Consequence — the 5a control-stream announce is superseded and removed.** Phase 5a
announced `RelayClientConnected/Disconnected { handle }` over a dedicated relay->server
control stream. That cannot bind a handle to a *member* (the relay never sees auth),
and the stamp-on-stream both connects (handle arrives with the primary stream) and
disconnects (the primary stream closes). So 5b **removes** the control stream, the
`announce` helper, and the now-unused `RelayClientConnected`/`RelayClientDisconnected`
protocol variants. The 5a pieces that **stay** are exactly what 5b consumes: handle
allocation (`next_handle`), the `clients` map, and the forward/route datagram loops.
Affected 5a relay tests are reworked to learn a client's handle from the stamped
bridged stream instead of the announce.

## Decisions (settled)

| Decision | Choice |
|----------|--------|
| Handle<->member correlation | **Relay stamps `[handle BE]` on every bridged stream** (authoritative); server reads it before `RelayStreamRole`; binds at primary-stream auth. |
| 5a control-stream announce | **Removed** (superseded by the stamp). Control stream + `announce` + the two protocol variants deleted. |
| Outgoing fan-out | A **`VoiceSink`** abstraction per member: `Direct(Connection)` or `Relayed { relay: Connection, handle: u32 }`. `voice_connections` becomes `HashMap<[u8;32], VoiceSink>`. Fan-out calls `sink.send_datagram(frame)` uniformly. |
| Incoming relay voice | **One** datagram loop on the **relay connection** (not per-client): read `[handle][frame]`, resolve `handle -> connection_pk` (authoritative sender), process the frame through the shared path. |
| Frame processing | **Extracted** into one function used by both the direct per-connection loop and the relay loop (DRY). |
| Stream-key offer | **No change** — it already rides the bridged primary stream as a `ServerEvent::StreamKeyOffer`, so it works over the relay today (confirmed). |
| Scope | **Server core only.** Client wiring + UI re-enable + real-audio verification deferred to 5b-client. |

## Architecture

### Part A — Relay: stamp the handle, drop the announce (`crates/farder-relay`)

- **`router.rs` `bridge_client`** gains a `handle: u32` parameter. For each bridged
  stream, after `server_conn.open_bi()`, it **writes `handle.to_be_bytes()` (4 bytes)
  to the server side first**, then spawns the existing bidirectional `tokio::io::copy`.
  (The client->server copy is unchanged; only a 4-byte prefix is injected once at the
  start of the server-bound half.) `handle_connect` passes the client's handle.
- **Remove** from `handle_register`: the `conn.open_bi()` control stream and the held
  `RegisteredServer.control`. `RegisteredServer` collapses back to just `conn` (or a
  tuple) — keep whatever the forward/route loops need.
- **Remove** the `announce` helper and its two call sites in `handle_connect`.
- **Remove** `RelayClientConnected`/`RelayClientDisconnected` from
  `farder-protocol::Message`.
- **Keep** `next_handle`, the `clients` map, and `datagram::{forward_client_datagrams,
  route_server_datagrams}` unchanged.
- **Rework the affected 5a tests** (`router.rs` `mod tests`): the
  `register_capturing_server` double reads the 4-byte handle stamp off the bridged
  stream it accepts (instead of reading control-stream announcements); the
  forward/route datagram tests learn a client's handle by having the client open one
  bi-stream and reading the stamp the relay injects. Delete the
  `announces_client_connect_and_disconnect` test.

### Part B — Server: read the stamp, sink abstraction, relay voice loop (`crates/farder-server`)

- **`relay.rs` `serve_relay_stream`** reads the **4-byte handle** first
  (`read_exact`), then the existing `RelayStreamRole` frame. It threads the handle
  into `run_relay_primary` (and `run_relay_aux`, which may ignore it).
- **`relay.rs` `run_relay_primary`**, after `authenticate` yields the member's
  `public_key`/`pk_bytes`, registers the member's voice sink as
  `VoiceSink::Relayed { relay: <the server's relay connection>, handle }` in
  `voice_connections`, and records `handle -> pk_bytes` in a new `relay_voice_handles`
  map (for incoming-frame sender resolution). Both are removed in `cleanup_session`.
  `run_relay_primary` needs access to the relay `Connection` — thread it in from
  `connect_and_serve` (which owns it).
- **`relay.rs` `relay_client_endpoint`** enables datagrams on its `TransportConfig`
  (`datagram_receive_buffer_size(Some(1<<20))` + `datagram_send_buffer_size(1<<20)`),
  matching the direct endpoint — otherwise the relay's forwarded datagrams can't be
  received and the server's outgoing voice datagrams can't be sent.
- **`relay.rs` `connect_and_serve`** spawns **one** voice datagram loop on the relay
  connection: `loop { let dg = relay_conn.read_datagram().await?; if dg.len() < 4
  { continue } let handle = u32::from_be(dg[0..4]); let pk = relay_voice_handles.get(handle);
  process_inbound_voice_frame(state, pk, dg.slice(4..)) }`. Unknown handle -> drop.
- **`state.rs`** `voice_connections` becomes `HashMap<[u8;32], VoiceSink>`; add
  `relay_voice_handles: RwLock<HashMap<u32, [u8;32]>>`. Define:
  ```rust
  pub enum VoiceSink {
      Direct(quinn::Connection),
      Relayed { relay: quinn::Connection, handle: u32 },
  }
  impl VoiceSink {
      pub fn send_datagram(&self, frame: bytes::Bytes) -> Result<(), quinn::SendDatagramError> {
          match self {
              VoiceSink::Direct(c) => c.send_datagram(frame),
              VoiceSink::Relayed { relay, handle } => {
                  let mut tagged = Vec::with_capacity(4 + frame.len());
                  tagged.extend_from_slice(&handle.to_be_bytes());
                  tagged.extend_from_slice(&frame);
                  relay.send_datagram(tagged.into())
              }
          }
      }
  }
  ```
- **`connection.rs`** — direct mode unchanged except: where it registers a client's
  `Connection` in `voice_connections`, it now stores `VoiceSink::Direct(conn)`. The
  fan-out block (~761-782) calls `sink.send_datagram(bytes.clone())` instead of
  `peer_conn.send_datagram(...)`. The inbound frame-processing core (parse session_id
  -> channel -> `on_frame_ingress` -> fan out to recipients' sinks) is **extracted**
  into `process_inbound_voice_frame(state, sending_pk, frame)`, called by both the
  direct per-connection loop and the relay loop. Direct mode passes the connection's
  authed `pk_bytes`; relay mode passes the `handle -> pk` lookup.

### Data flow (relayed voice, end to end)

```
client A speaks
  -> client A sends datagram [frame]            (to the relay; 5b-client enables this)
  -> relay forwards [handleA][frame]            (5a forward path)
  -> server relay voice loop: handleA -> pkA, process_inbound_voice_frame(pkA, frame)
  -> on_frame_ingress -> recipients {B}
  -> sink_B = Relayed{relay, handleB}; sink_B.send_datagram(frame)
       => server sends [handleB][frame] on the relay connection
  -> relay routes: strip handleB -> client B's connection (5a route path)
  -> client B receives [frame]                  (5b-client recv loop dispatches it)
```
5b-server implements everything except the two client-side `(5b-client)` steps.

## Protocol changes (`farder-protocol`)

- **Remove** `Message::RelayClientConnected` and `Message::RelayClientDisconnected`
  (and their tests). The handle is now a raw 4-byte big-endian stream prefix, not a
  `Message`.

## Backward compatibility

- **Direct mode is unchanged** in behavior: `VoiceSink::Direct` wraps the same
  per-client `Connection`, the same per-connection datagram loop runs, fan-out is
  identical. The existing direct voice tests must stay green.
- **Relay <-> server wire changes** (the handle stamp; no control stream). The relay
  and the relay-mode server are not yet deployed and move together, so this is safe;
  the `relay_mode` integration tests are updated to the stamped-stream format and must
  pass. (A hypothetical old relay-mode server talking to a new relay would mis-read
  the stamp — not a concern pre-deployment.)

## File structure

- `crates/farder-protocol/src/messages.rs` — remove the two relay-client variants + tests.
- `crates/farder-relay/src/router.rs` — `bridge_client` stamps the handle; remove
  control stream + `announce`; `RegisteredServer` simplified; rework 5a tests.
- `crates/farder-server/src/state.rs` — `VoiceSink` enum; `voice_connections` ->
  `HashMap<[u8;32], VoiceSink>`; `relay_voice_handles` map.
- `crates/farder-server/src/relay.rs` — read handle stamp; thread handle + relay conn;
  register relayed `VoiceSink`; enable datagrams on `relay_client_endpoint`; spawn the
  relay voice loop.
- `crates/farder-server/src/connection.rs` — extract `process_inbound_voice_frame`;
  fan-out via `VoiceSink::send_datagram`; register `VoiceSink::Direct`.
- `crates/farder-server/tests/relay_mode.rs` — extend with a relayed-voice routing test.
- `docs/modules/relay.md`, `docs/modules/server-relay.md` — update for the stamp +
  voice-over-relay server path.

## Testing (headless)

All 5b-server behavior is testable without audio or GUI, with real-QUIC doubles:

1. **`VoiceSink` unit test:** `Relayed{handle}.send_datagram(frame)` produces
   `[handle BE] ++ frame`; `Direct` sends the frame unchanged. (Use a loopback
   connection pair to observe the bytes.)
2. **Handle-stamp read (server):** a relay double opens a bridged stream stamped
   `[handle]` then a `RelayStreamRole::Primary` + auth; assert the server binds
   `handle <-> pk` (observable via the relayed-voice routing test below).
3. **Relayed-voice routing (integration, `relay_mode.rs`):** stand up the relay
   double + the server in relay mode; bring up **two** relayed members (each auths on a
   stamped primary stream and joins the same voice channel). Feed a voice frame for
   member A (send `[handleA][frame]` to the server on the relay connection); assert the
   server emits `[handleB][frame]` (and NOT `[handleA]...`) on the relay connection —
   i.e. it demuxed by source handle, decided recipients, and fanned out tagged by the
   recipient handle. Also assert an unknown source handle is dropped.
4. **Relay tests reworked:** the 5a forward/route datagram tests learn the handle via
   the stamp and still pass; `bridge_client` stamping doesn't break the Phase-1/2
   bridge tests (the server-side test doubles read the 4-byte prefix).
5. **Backward-compat gate:** `cargo test --workspace` green, including the existing
   **direct** voice tests (unchanged behavior) and the updated `relay_mode` tests.

What is **not** verifiable here (deferred to 5b-client + your Windows/deployed-relay
run): real audio capture/playback, the client datagram recv loop, the actual
"member A hears member B" end-to-end.

## Out of scope (Phase 5b-client and beyond)

- **Client wiring:** enable datagrams on `make_pinned_relay_endpoint`
  (`client/src-tauri/src/tls.rs`); spawn the voice datagram recv loop for relayed
  connections (`commands.rs` ~423, currently `if !relayed`); remove the
  `voice_join` "voice is not available over a relay yet" refusal (`commands.rs` ~2200).
- **UI re-enable:** un-grey voice channels on relayed servers + drop the toast
  (`client/src/components/ChannelSidebar.tsx` ~368-374).
- **Real-audio / two-client end-to-end verification** — needs a deployed relay + two
  clients with audio (your Windows step).
