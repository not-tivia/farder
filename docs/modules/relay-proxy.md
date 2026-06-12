# Module: relay fetch proxy (`crates/farder-relay/src/proxy.rs`, `router.rs`)

> **File(s):** `crates/farder-relay/src/proxy.rs`, `crates/farder-relay/src/router.rs`
> **Layer:** Server crate (relay)
> **Last reviewed:** 2026-06-11

## Purpose

The relay doubles as a privacy fetch proxy for invite previews. A client sends
`ProxyInvitePreview` and the relay asks the target server "what is behind this
code?" on the client's behalf, so the client's IP never reaches the server host.
`proxy.rs` owns the fetch logic, SSRF guard, TTL cache, and outbound endpoint.
`router.rs` owns the per-connection dispatch and per-IP rate limiting.

This is phase one of the relay fetch proxy foundation. External embed fetches
are planned on the same infrastructure.

---

## Key type: `PreviewContext`

`PreviewContext` (`router.rs`) is the shared, relay-wide preview state:

| Field | Type | Role |
|---|---|---|
| `cache` | `PreviewCache` | 60 s TTL cache keyed by `(target, code)` |
| `limiter` | `ConnectionLimiter` | 30 previews / min / IP rate bucket |
| `out_endpoint` | `Endpoint` | IPv4-only QUIC endpoint for direct-server dials |

A single `Arc<PreviewContext>` is created at startup via `new_preview_context()`
and cloned into every connection handler.

---

## Public interface

### `handle_preview(target, code, conn, send, state, preview)` (router.rs)

**What it does:** the per-connection entry point for a `ProxyInvitePreview`
request. Applies all guardrails in order, then sends
`ProxyInvitePreviewResult` and waits for the client to drain before dropping
the connection.

**Guardrail sequence:**
1. **Code-length cap (256 chars)** — oversized codes are answered as `Invalid`
   (not `Unavailable`); this prevents cache poisoning with huge keys while
   revealing nothing about the server.
2. **Per-IP rate limit (30/min)** — exceeded requests are answered as
   `Unavailable`. The `ConnectionLimiter` guard is admitted and immediately
   released (used as a pure rate limiter, not a concurrency cap, because the
   global connection limiter already bounds concurrency).
3. **TTL cache** — on a hit, returns the cached `PreviewOutcome` without
   contacting the server.
4. **Fetch** — delegates to `proxy::fetch_preview` with a 5 s relay-side
   `tokio::time::timeout`. Timeout collapses to `Unavailable`.
5. **Cache write** — stores the fresh outcome before replying.

**Reply pattern:** sends `ProxyInvitePreviewResult`, calls `send.finish()`, then
awaits `client_conn.closed()` so the buffered reply reaches the peer before the
connection is torn down.

**Connects to:** `proxy::fetch_preview` (the actual network fetch);
`proxy::cache_key` (cache-key derivation); `limits::ConnectionLimiter`
(rate bucket).

---

### `fetch_preview(target, code, registered, out_endpoint) -> PreviewOutcome` (proxy.rs)

**What it does:** resolves a `PreviewTarget` and speaks the preview exchange
(Challenge → GetInvitePreview → InvitePreview / InvitePreviewError) against the
target server. All errors (transport, parse, timeout) collapse to `Unavailable`
at the call site.

**Two fetch paths:**

#### Registered (relay-registered server)

The relay opens a new bi-stream on the already-open server control connection
(`server_conn.open_bi()`), writes the **handle-0 prefix** (`0u32` as 4-byte
big-endian) followed by a `RelayStreamRole::Primary` frame, then speaks
`ask_server`. Handle 0 is the reserved relay-originated sentinel — the relay
writes it authoritatively; no client connection ever receives handle 0 from the
relay's allocator (which starts at 1). This is why a Farder client cannot
forge a preview stream: it never holds a connection to the server's control
path, and the handle byte is written by the relay before the server sees the
stream.

#### Direct (dial on behalf of requester)

Parses the address string, applies the **SSRF guard** (`is_global_ip`), then
dials a fresh QUIC connection via `out_endpoint`. The server's cert is accepted
without CA verification (matching the Farder ecosystem's existing trust model for
direct servers). The connection is closed with `conn.close(0, b"preview done")`
after the exchange. This path collapses to `Unavailable` on parse error, SSRF
refusal, or any transport failure.

---

### `ask_server(send, recv, code) -> Result<PreviewOutcome>` (proxy.rs, private)

**What it does:** speaks the preview sub-protocol on an established stream pair.
1. Reads the server's `Challenge` frame (via `read_capped`, 16 KB cap).
2. Sends `ClientFrame::GetInvitePreview { code }`.
3. Reads the reply: `InvitePreview { .. }` → `Preview`; `InvitePreviewError` →
   `Invalid`; anything else → `Unavailable`.

The 16 KB answer cap (`ANSWER_CAP`) prevents a misbehaving server from sending
unbounded data on this transient connection.

---

## SSRF guardrail (`is_global_ip`)

Refuses any address that is not globally routable, including:

- Loopback (`127.0.0.0/8`, `::1`)
- Private (`10/8`, `172.16/12`, `192.168/16`)
- Link-local (`169.254/16`, `fe80::/10`)
- Broadcast, multicast, unspecified (`0.0.0.0`, `::`)
- CGNAT (`100.64/10`)
- Unique-local IPv6 (`fc00::/7`)
- **v4-mapped IPv6** (`::ffff:127.0.0.1` etc.) — the mapped v4 address is
  extracted and judged by v4 rules, closing the classic SSRF bypass.
- 6to4 (`2002::/16`) and NAT64 (`64:ff9b::/96`) tunnels — refused wholesale
  because they embed v4 addresses that cannot be safely judged after the tunnel
  prefix.

The SSRF guard applies only to `Direct` targets. `Registered` targets use the
already-open relay-server control connection, which was established by the server
itself at registration time.

---

## TTL cache (`PreviewCache`)

`PreviewCache` is a `Mutex<HashMap<(String, String), (Instant, PreviewOutcome)>>`
keyed by `(canonical_target_string, code)`. TTL is 60 s. On `put`:

- If the map has reached `CACHE_MAX_ENTRIES` (1024), expired entries are pruned
  first. If still at capacity after pruning, the map is cleared entirely
  (pressure valve — avoids unbounded growth under adversarial load without
  requiring LRU bookkeeping).

Cache hits bypass all network I/O. The client also has a 60 s session-scoped
cache in `commands.rs`, so a cached relay answer is double-cached.

---

## Outbound socket note (`outbound_endpoint`)

The direct-dial endpoint binds `0.0.0.0:0` (IPv4 only). IPv6 direct targets
therefore fail the QUIC connect and collapse to `Unavailable`. The SSRF guard
(`is_global_ip`) is still written to be v6-correct so the guard remains sound if
the endpoint is ever upgraded to a dual-stack (`[::]`) socket.

---

## State it owns

| Field / variable | Type | What it tracks, when it's mutated |
|---|---|---|
| `PreviewContext::cache` | `PreviewCache` (Mutex-wrapped HashMap) | Cached outcomes keyed by (target, code); written on every fresh fetch |
| `PreviewContext::limiter` | `ConnectionLimiter` | Per-IP rate bucket (30/min); reset by sliding window |
| `PreviewContext::out_endpoint` | `quinn::Endpoint` | Persistent outbound QUIC endpoint for direct dials; never mutated after creation |

## Events emitted

None. The relay sends `ProxyInvitePreviewResult` directly over the stream and
closes the connection — no Tauri events, no broadcast.

## Integration map

- **`router::handle_connection`** — dispatches `ProxyInvitePreview` here after
  reading the first message on a new connection.
- **`limits::ConnectionLimiter`** — the preview context creates its own
  rate-only limiter (no concurrency cap) separate from the global connection
  limiter.
- **`farder_protocol::messages`** — `PreviewTarget`, `PreviewOutcome`,
  `Message::ProxyInvitePreview`, `Message::ProxyInvitePreviewResult`.
- **`farder_protocol::server`** — `ClientFrame::GetInvitePreview`,
  `ServerFrame::Challenge`, `ServerFrame::InvitePreview`,
  `ServerFrame::InvitePreviewError`, `RelayStreamRole::Primary`.
- **Server `connection.rs`** — the `authenticate()` function handles
  `GetInvitePreview` pre-auth and returns a throwaway `InvitePreview` /
  `InvitePreviewError` frame (see `docs/modules/server-handlers.md`).

## Known gotchas

- **Handle 0 is the relay-originated sentinel, not a client handle.** The relay's
  handle allocator starts at 1; handle 0 is only ever written by the relay itself
  on preview streams. The server's `serve_relay_stream` enforces this: Primary
  streams that somehow authenticate with handle 0 have their session cleaned up
  immediately and the connection is dropped.

- **The 5 s relay-side timeout and 8 s client-side timeout are intentionally
  staggered.** The relay gives itself 5 s; the client waits 8 s. This ensures
  the relay's result always arrives (or collapses to `Unavailable`) before the
  client's own timeout fires, so the client never races the relay.

- **Cache key is `(canonical_target_string, code)`**, not the raw
  `PreviewTarget`. The canonical form is `"r:<hex server_id>"` for `Registered`
  and `"d:<addr>"` for `Direct`. Two requests for the same server over different
  link representations will produce the same key.

- **SSRF guard applies even if `is_global_ip` passes v4-mapped addresses that
  look public.** The mapped-v4 extraction in `is_global_ip` handles
  `::ffff:8.8.8.8` correctly (passes) and `::ffff:127.0.0.1` correctly (fails).
  Do not weaken the v4-mapped branch — it is the most commonly exploited SSRF
  bypass in relay-style proxies.
