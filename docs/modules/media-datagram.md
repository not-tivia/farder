# Module: media-datagram (`crates/farder-protocol/src/media_datagram.rs`)

> **File(s):** `crates/farder-protocol/src/media_datagram.rs`
> **Layer:** Protocol crate
> **Last reviewed:** 2026-06-12

## Purpose

Provides a unified media-datagram transport: a 26-byte cleartext outer header
plus fragment/reassemble, so large (video) frames ride the QUIC datagram path.
The relay, server, and client dispatcher all route on the outer header WITHOUT
needing keys. The payload behind the header is the existing `farder-crypto`
sealed frame (the AEAD security boundary, unchanged) — possibly split across
several datagrams. Audio frames fit in a single datagram and gain only the
26-byte header overhead. This module deliberately does NOT perform encryption
or decryption; it handles routing/fragmentation metadata only.

---

## Wire format

```
[ 26-byte cleartext outer header ][ payload (fragment of a sealed frame) ]
```

### Outer header layout

| Offset | Size | Field | Description |
|---|---|---|---|
| 0 | 1 | `version` | Always `0x03` (`MEDIA_DGRAM_VERSION`). Distinct from the inner sealed-frame version `0x02`. |
| 1 | 1 | `track_kind` | `0x01` = Audio, `0x02` = Video (matches `farder-crypto::media` constants). |
| 2 | 16 | `session_id` | The 16-byte call session ID (same as in the sealed frame). Used for routing; no private key needed to read it. |
| 18 | 4 | `frame_id` | `u32` big-endian. Monotonically increasing per sender per track; used to group fragments of the same frame. |
| 22 | 2 | `frag_index` | `u16` big-endian. Zero-based fragment index within this frame. |
| 24 | 2 | `frag_count` | `u16` big-endian. Total number of fragments for this frame. Invariant: `frag_index < frag_count`. |

Total: `MEDIA_DGRAM_HEADER_LEN = 26` bytes.

---

## Public interface

### `OuterHeader` (struct)

Fields: `track_kind: TrackKind`, `session_id: SessionId`, `frame_id: u32`,
`frag_index: u16`, `frag_count: u16`.

---

### `OuterHeader::write_to(&self, out: &mut Vec<u8>)`

**What it does:** appends the 26-byte header to `out` in the wire order above.
**Parameters:** `out` — the buffer to write into; bytes are appended, not replaced.
**Returns / emits:** nothing (infallible).
**Side effects:** pushes 26 bytes onto `out`.
**Connects to:** `fragment()` calls this once per datagram it constructs.

---

### `OuterHeader::parse(buf: &[u8]) -> Result<(OuterHeader, &[u8]), MediaDgramError>`

**What it does:** parses the first 26 bytes of `buf` as an outer header,
validates version, track kind, and the `frag_index < frag_count` invariant,
and returns the parsed header plus the remaining payload slice.
**Parameters:** `buf` — the raw datagram bytes, starting at the outer header.
**Returns / emits:** on success — `(OuterHeader, payload_slice)` where
`payload_slice = &buf[26..]`. On error — a `MediaDgramError`:
- `TooShort` — fewer than 26 bytes.
- `BadVersion(u8)` — first byte is not `0x03`.
- `BadTrackKind(u8)` — byte 1 is not a known track kind.
- `BadFragmentation` — `frag_count == 0` or `frag_index >= frag_count`.

**Side effects:** none (zero-copy borrow of `buf`).
**Connects to:** `MediaInboundDispatcher::dispatch` (client), `voice::recv`
reassemble loop, `on_frame_ingress` (server) — all call this before doing anything
else with a received datagram.

---

### `fragment(track_kind, session_id, frame_id, sealed, max_payload) -> Vec<Vec<u8>>`

**What it does:** splits a sealed frame into one or more complete datagrams
(each = 26-byte outer header + payload slice). If the sealed frame fits within
`max_payload` bytes, a single datagram is returned (`frag_count = 1`). Otherwise
the sealed bytes are split in order into `ceil(len / max_payload)` datagrams.
**Parameters:**
- `track_kind` — `TrackKind::Audio` or `TrackKind::Video`.
- `session_id` — the call session ID (`&SessionId`, 16 bytes).
- `frame_id` — the frame sequence number for this sender/track.
- `sealed` — the AEAD-sealed frame bytes to fragment.
- `max_payload` — maximum payload bytes per datagram; `0` is coerced to `1`.

**Returns / emits:** a `Vec<Vec<u8>>` of ready-to-send datagrams. Always returns
at least one element (even for an empty sealed frame). The `frag_count` field in
every header equals `dgrams.len()`; `frag_index` is zero-based.
**Side effects:** allocates one `Vec<u8>` per fragment.
**Connects to:** `voice::send` calls this after sealing a frame, before sending
each resulting datagram over the QUIC connection.

Precondition: `sealed.len() <= max_payload * 65535`. Audio frames are far below
this; violating it is a bug (debug-asserted).

---

### `Reassembler` (struct)

Reassembles datagrams of one peer-track into complete sealed frames. Internally
keyed by `frame_id`. Default capacity is 4 in-progress frames.

---

### `Reassembler::new() -> Self`

**What it does:** creates a `Reassembler` with the default capacity of 4
in-progress frames.

---

### `Reassembler::with_capacity(max_frames: usize) -> Self`

**What it does:** creates a `Reassembler` that keeps at most `max_frames`
incomplete frames in memory at once. `max_frames = 0` is coerced to `1`.
**Parameters:** `max_frames` — the in-progress frame buffer bound.

---

### `Reassembler::accept(&mut self, header: &OuterHeader, payload: &[u8]) -> Option<Vec<u8>>`

**What it does:** feeds one fragment into the reassembler. Returns the completed
sealed frame when all fragments of a frame have arrived; otherwise returns `None`.

Single-fragment fast path: if `header.frag_count == 1`, the payload is returned
immediately with no buffering (a copy, but no HashMap entry).

Drop-late / drop-incomplete: when the number of in-progress frames exceeds
`max_frames`, the least-recently-touched incomplete frame is evicted. Frames
that never complete are silently dropped; no error is returned.

Duplicate fragments are idempotent (the slot is already filled; `received` is
not incremented again). A `frag_count` mismatch for an existing `frame_id`
restarts that frame's buffer cleanly.

**Parameters:**
- `header` — the parsed outer header. Panics if `frag_index >= frag_count`
  (this invariant is guaranteed by `OuterHeader::parse`; only hand-constructed
  headers can violate it).
- `payload` — the payload slice from the same datagram.

**Returns / emits:** `Some(sealed_frame)` if this datagram completed a frame;
`None` otherwise.
**Connects to:** `voice::recv` calls this in the inbound datagram loop, then
passes the reassembled `sealed` bytes to `farder_crypto::media::open`.

---

### `Reassembler::in_progress_len(&self) -> usize`

**What it does:** returns the number of incomplete frames currently buffered.
Useful for diagnostics and tests.

---

## Constants

| Constant | Value | Meaning |
|---|---|---|
| `MEDIA_DGRAM_VERSION` | `0x03` | Outer header version byte. Distinct from inner sealed-frame version `0x02`. |
| `MEDIA_DGRAM_HEADER_LEN` | `26` | Size of the cleartext outer header in bytes. |
| `DEFAULT_MAX_DGRAM_PAYLOAD` | `1100` | Conservative per-datagram payload cap when the QUIC connection's `max_datagram_size` is unknown. Audio frames are far below this and never fragment. Phase C derives the real cap from the connection. |

---

## Inner vs. outer: the security boundary

The **outer header** is cleartext and unauthenticated. It exists so that the
relay, server, and client dispatcher can route or bandwidth-account a datagram
without holding any cryptographic keys. Specifically:

- The relay reads only the 4-byte `[handle]` prefix it stamps on every
  datagram — it does NOT read the outer header at all. It forwards blind.
- The server (`on_frame_ingress`, `process_inbound_voice_frame`) reads the outer
  header for session routing and bandwidth accounting.
- The client dispatcher (`MediaInboundDispatcher::dispatch`) reads the outer
  header to route the datagram to the right `RecvTask` by `session_id`.
- The client recv task (`voice::recv`) reads the outer header to reassemble
  fragments, then passes the reassembled bytes to `farder_crypto::media::open`.

The **inner sealed frame** is the AEAD security boundary (`farder_crypto::media`
seal/open, version `0x02`). It is carried unchanged as the datagram payload.
Its AAD binds the session ID, sequence number, and track type to the ciphertext,
so a frame that is reassembled under the wrong key or session fails to open
(decrypt error) and is dropped.

**Security note on the unauthenticated outer header:** tampering with the outer
header can only misroute or drop a frame — it cannot inject content. Even if an
adversary (malicious relay or server) rewrites the `session_id` field in the
outer header, the inner sealed frame's AAD will not match the target session's
key, so `open` returns an error and the frame is discarded. This is the same
threat model as before Phase A: a malicious relay or server could already drop
or delay datagrams; they could not inject decryptable content. The outer header
adds no new attack surface for content injection.

**Sealed-sender invariant:** the outer header contains no public key or sender
identity. The session ID is an ephemeral 16-byte value; it does not reveal who
sent the frame to an observer who does not hold the call keys.

**Changing the outer format:** outer version `0x03` is distinct from the inner
`0x02` so parsers can fail fast. Because `parse` rejects any byte that is not
`0x03`, old and new peers cannot exchange media datagrams if the outer format
changes — both must rebuild. The inner sealed-frame format is independent and
unchanged.

---

## Who reads / uses it

| Caller | Location | What it does |
|---|---|---|
| `on_frame_ingress` | `crates/farder-server/src/media_stream.rs` | Parses outer header for bandwidth accounting and session routing on the server side. |
| `process_inbound_voice_frame` | `crates/farder-server/src/connection.rs` | Reads `session_id` from the outer header to look up the target voice channel and forward the datagram. |
| `MediaInboundDispatcher::dispatch` | `client/src-tauri/src/voice/mod.rs` | Parses outer header to route the datagram to the correct peer's `RecvTask` by `session_id`. |
| `voice::recv` | `client/src-tauri/src/voice/recv.rs` | Parses outer header, feeds into `Reassembler`, then calls `farder_crypto::media::open` on the reassembled frame. |
| `voice::send` | `client/src-tauri/src/voice/send.rs` | Calls `fragment()` after sealing, sends each resulting datagram over QUIC. |
| Relay | `crates/farder-relay/` | Does NOT read the outer header. Forwards datagrams blind, reading only its own 4-byte `[handle]` prefix. |

---

## State it owns

| Field | Type | What it tracks, when it's mutated |
|---|---|---|
| `Reassembler::frames` | `HashMap<u32, Partial>` | In-progress frames keyed by `frame_id`; entries added on first fragment, removed on completion or eviction. |
| `Reassembler::clock` | `u64` | Monotonic touch counter for LRU eviction; incremented on every multi-fragment datagram accepted. |
| `Reassembler::max_frames` | `usize` | Immutable bound on `frames.len()` after construction. |

## Events emitted

None. This is a pure data-transformation crate with no async I/O or event bus.

## Integration map

- **`farder_crypto::media`** — the sealed frame this module fragments and
  reassembles is produced by `farder_crypto::media::seal` and consumed by
  `farder_crypto::media::open`. This module carries the sealed bytes as an opaque
  payload; it never reads or writes ciphertext fields.
- **`farder-protocol::server::TrackKind`** — the `track_kind` field in
  `OuterHeader` uses this shared enum.
- **`crates/farder-server/src/media_stream.rs`** — `on_frame_ingress` is the
  server-side ingress that routes on the outer header.
- **`client/src-tauri/src/voice/mod.rs`** — `MediaInboundDispatcher::dispatch`
  is the client-side dispatcher that routes on the outer header.
- **`client/src-tauri/src/voice/recv.rs`** — the recv task owns one
  `Reassembler` per peer-track and drives it with `accept`.
- **`client/src-tauri/src/voice/send.rs`** — the send task calls `fragment`
  after each `seal` call.

## Known gotchas

- **The outer header is unauthenticated, but that is safe by design.** See the
  security note above. Do not add routing logic that trusts outer header fields
  without verifying the inner sealed frame succeeds.

- **`Reassembler` is per-peer-track, not global.** Each `RecvTask` in `voice::recv`
  holds its own `Reassembler`. A `Reassembler` should never be shared between
  peers, because `frame_id` is meaningful only within a single sender's sequence.

- **Single-fragment fast path bypasses the HashMap entirely.** Audio frames
  always take this path (they never fragment under `DEFAULT_MAX_DGRAM_PAYLOAD =
  1100`). This means adding a `Reassembler` to the audio path has zero buffering
  cost today; it only matters when video is introduced.

- **`frag_count` mismatch on an existing `frame_id` silently discards the
  partially buffered frame.** This handles protocol edge cases (e.g. a sender
  retransmits with a different split size). It does not signal an error to the
  caller; the evicted partial frame is simply lost.

- **Outer version `0x03` vs. inner version `0x02`.** The inner sealed-frame
  format is owned by `farder-crypto` and has version byte `0x02`. The outer
  header version `0x03` is deliberately different so that a parser that reads the
  first byte can distinguish them. Do not reuse `0x02` for the outer header.
