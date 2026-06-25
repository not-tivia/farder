# Mesh Foundation — Signed Event Log (Rung 1, messages slice) — Design Spec

**Date:** 2026-06-25
**Status:** Approved (brainstorm complete, ready for implementation plan)
**Part of:** the mesh-hosting north-star (see project memory `project_farder_mesh_hosting`). This is **Rung 1**, the keystone the rest of the mesh clips onto.

## Where this sits

The mesh roadmap, in order:
1. **Rung 1 (this spec): make a server's truth a *signed logbook*** — single-host, no encryption yet.
2. Rung 2: channel content end-to-end encryption (group keys; hosts hold sealed envelopes).
3. Rung 3: replication + multi-host (the actual mesh — failover first, then full member-hosting).
4. Rung 4: availability anchors + large communities.

This spec is the **walking skeleton of Rung 1**: stand up the entire spine (server identity → signed event log → validation → derived read-model) but prove it **end-to-end on a single action — posting a message** — before converting channels/roles/joins/bans in follow-on specs.

## Goal

Re-found a server's source of truth from "whatever the single host's SQLite database says" to "an append-only log of **author-signed events** that any node can verify," with the SQLite database demoted to a **derived read-cache rebuilt from the log**. Give every server a **permanent cryptographic identity**. Ship value on its own: a **tamper-evident server** — even the host cannot forge or silently alter message history.

Single-host only. Payloads stay **plaintext** at this rung (encryption is Rung 2). **Fresh-start:** the log model applies to **newly created servers**; existing servers are disposable test environments and are not migrated.

## Non-goals (explicitly out of scope for this slice)

- Encryption of event payloads (Rung 2).
- Replication / multiple hosts / gossip sync (Rung 3).
- Converting non-message state (channels, roles, members, bans, invites) to events — those stay in the existing DB tables for this slice; only **messages** flow through the log. Authorization for message events is still read from the existing `members`/`roles` permission tables.
- Migrating existing servers.
- Relay registration hardening (binding the relay `server_id` to the genesis) — a natural later win, noted but not built here.

## Global Constraints

- **Author-signed, not host-signed.** Events MUST be built and signed by their **author's client** (the identity key), never by the server — that signature is the entire tamper-evidence property. The server only *validates, stores, and broadcasts*.
- **Fresh-start.** New servers create a genesis; existing servers keep working unchanged. No migration code.
- **Single source of truth = the log.** The `messages` table becomes a materialized view derived from message events; it must be reconstructible from the `events` table alone (verified on startup / rebuildable).
- **Reuse existing crypto.** Signing uses the existing Ed25519 identity (`farder-crypto` `Keypair`/`PublicKey`, same key used for auth challenge + DM exchange). Hashing reuses whatever collision-resistant hash `farder-crypto` already uses for profile hashing (SHA-256 or BLAKE3 — the plan confirms the existing dep; do not add a new hash dependency if one exists).
- **Client compatibility.** Existing client flows (send, history pagination, reply, edit, reactions) must keep working. The derived `messages` table keeps a numeric id for client compatibility; canonical identity is the event hash.

## The data model

### Server genesis & identity

On **new** server creation, the server persists a **genesis record** (content-addressed; no signature needed — its hash *is* the identity, so tampering changes the id):

```
Genesis {
  version: u16,            // schema version
  name: String,            // server display name at creation
  owner: PublicKey,        // the creating client's identity key = the owner
  created_at: u64,         // unix seconds
  nonce: [u8; 16],         // random, so two same-name/same-owner servers differ
}
server_id = hash(canonical_bytes(Genesis))
```

- The genesis establishes the **owner** cryptographically (no more "first member to connect wins"). The owner's authority flows from being named in the genesis and signing events with that key.
- `server_id` is stable across restarts (persisted), unlike today's throwaway cert.
- Persisted alongside the DB (new `genesis` single-row table or a `genesis.json` in the data dir; plan decides).

### Event

Every logged action is an `Event` — a per-author, hash-chained, signed record:

```
Event {
  server_id: ServerId,        // binds the event to this server's genesis
  author: PublicKey,          // who created it
  seq: u64,                   // author's own sequence number (0-based, +1 per event by this author)
  prev: Option<EventHash>,    // hash of this author's PREVIOUS event (None for seq 0)
  lamport: u64,               // logical clock: 1 + max(lamport of every event this author had seen)
  timestamp: u64,             // author's wall-clock claim — UNTRUSTED, tiebreak only
  payload: EventPayload,      // the action
  signature: Signature,       // author's Ed25519 sig over canonical_bytes(all fields above)
}
EventHash = hash(canonical_bytes(Event))   // includes the signature; content id used in `prev` and as the event's id
```

`EventPayload` for this slice (extensible enum; only the first variant is built now):

```
EventPayload = MessagePosted {
  channel_id: u64,
  content: String,
  reply_to: Option<EventRef>,
  attachment_ids: Vec<u64>,   // references into the existing `files` store (unchanged this rung)
}
// future rungs add: MessageEdited, MessageDeleted, ChannelCreated, MemberJoined, RoleAssigned, MemberBanned, ...
```

Attachments (the `files` table + upload path) are **unchanged** at this rung — a message event merely *references* already-uploaded file ids, preserving today's attachment behavior. File bytes are not part of the event/log.

- **Per-author chain** (`seq` + `prev`): each author has an append-only chain. The server can detect **gaps** (missing seq) and **forks** (two events with the same `prev`) — tamper-evidence and, later, the basis for sync.
- **Lamport clock**: gives a logical time that works without a central sequencer (mesh-ready). The author sets `lamport = 1 + max lamport of all events it has observed`.

### Ordering

Canonical display order within a channel is a **deterministic total order**, computed from the log (not a database auto-counter):

```
order by (lamport ASC, then author bytes ASC, then event_hash ASC)
```

This is a stable, host-independent tiebreak: any node with the same set of events derives the identical order. At this single-host rung the server computes it; the same rule will hold under replication. `timestamp` is display-only and never used for ordering (it's untrusted).

## Message flow (the proven vertical)

1. **Client builds + signs.** On send, the client (which holds the unlocked identity key) constructs a `MessagePosted` event: fills `server_id`, `author` (self), `seq`/`prev` from its **per-server chain state**, `lamport` from its **per-server Lamport clock** (`1 + max seen`), `timestamp`, `payload`; computes `EventHash`; signs. It tracks chain + clock state per server (new client-side store, mirrors the `lastChannel`/dedup patterns but persisted per server identity).
2. **Submit.** Client sends the signed event to the server (protocol change below).
3. **Server validates.** The server, on receipt:
   1. `server_id` matches this server's genesis.
   2. **Signature** verifies against `author`.
   3. **Chain**: `seq` is the author's expected next (`prev_seq + 1`), and `prev` equals the hash of the author's last stored event (or `seq == 0 && prev == None` for the first). Reject gaps/forks.
   4. **Lamport** strictly greater than the author's previous lamport.
   5. **Authorization** (from the *existing* DB perm tables for this slice): `author` is a non-banned member with `SEND_MESSAGES` in `channel_id` — reuse `resolve_member_perms` (`handlers.rs`).
   6. **Limits**: content ≤ 8000 chars and other existing `SendMessage` checks.
4. **Append + derive.** Append the raw event to the new `events` table (append-only). Update the derived `messages` table (the materialized view) so existing reads/pagination/search keep working. Bump the server's view of the author's chain head + a server-wide max-lamport (so the server can sanity-check future events and assign its own outbound events later).
5. **Broadcast.** Broadcast a `NewMessage` event that now **carries the full signed event**, so other clients can (a) verify the author's signature themselves and (b) advance their own Lamport clock (`max(seen, event.lamport)`).
6. **Recipients verify + order.** Receiving clients verify the signature, fold the event into their local view, and order by the canonical rule. (Full client-side verification UI/indicator is optional this slice; the wire format must carry the signature so verification is *possible* and clocks stay correct.)

## Storage

- **New `events` table** (append-only, the source of truth): stores the raw canonical event bytes + indexed columns for query/validation: `event_hash` (PK), `server_id`, `author`, `seq`, `lamport`, `payload_type`, `channel_id` (nullable, denormalized for message lookups), `created_local_ts`. Unique index `(author, seq)`; index `(channel_id, lamport, author, event_hash)` for ordered history.
- **`messages` table = derived view.** Keep its existing shape/columns (so `fetch_history`, FTS search, replies, reactions keep working) but populate it from message events. Its numeric `id` stays for client compat; add a column mapping `id <-> event_hash`. Ordering served to clients follows the canonical rule (by lamport/author/hash), surfaced through the existing pagination API.
- **Rebuildability:** on startup the server verifies the `messages` view is consistent with `events` (or rebuilds it by replaying message events). The `events` table alone is sufficient to reconstruct all message state.

## Protocol changes (`farder-protocol`)

- New types: `Genesis`, `ServerId`, `Event`, `EventPayload`, `EventHash`, `EventRef` (how `reply_to` references another event — numeric id for client compat mapped to event hash, plan decides the exact shape).
- `ServerRequest::SendMessage` is replaced/augmented so the client submits a **signed `MessagePosted` event** rather than a bare `{channel_id, content, reply_to, attachment_ids}`. (Plan decides: new `ServerRequest::SubmitEvent { event }` vs. evolving `SendMessage`. Prefer a dedicated `SubmitEvent` so later event types reuse it.)
- `ServerEvent::NewMessage` carries the full signed `Event` (not just the rendered message), so peers can verify + clock-advance.
- The server's auth/genesis handshake exposes `server_id` (= genesis hash) to clients on connect so they bind their chain/clock state to the right server.

## Crypto (`farder-crypto`)

- Add canonical serialization + hashing + signing/verification helpers for `Event` and `Genesis` (deterministic byte encoding — reuse the existing canonical-bytes approach used for `SignedProfile`). `sign_event(keypair, event_fields) -> Event`, `verify_event(event) -> bool`, `event_hash(event) -> EventHash`, `genesis_id(genesis) -> ServerId`.
- These live in `farder-crypto` (the higher-trust, compile-checked layer), not the server.

## Client (`client/src-tauri`)

- **Per-server chain & clock state** (new persisted store): for each `(identity, server_id)`, track `next_seq`, `last_event_hash` (chain head), and `lamport`. Updated on every event the client authors *and* every event it receives (lamport advance). Fail-safe; rebuildable from the server's history on first connect (the client can ask for its own chain head to resync `next_seq`/`prev`).
- **Event building + signing** on send (replaces the bare send path in `commands.rs::send_message`).
- **Verification** of received events (at minimum verify signatures of incoming `NewMessage` events; surface tamper detection in logs now, UI later).

## Security properties delivered

- **Tamper-evidence:** the host cannot forge a message (no author signature) or alter one (changes the hash/signature) or silently drop one from an author's chain (gap detectable). This holds even though the host still stores plaintext.
- **Stable server identity:** `server_id = hash(genesis)`, owner cryptographically fixed — closing today's "throwaway cert / first-connector-is-owner / relay can't verify ownership" gaps (relay binding itself deferred).
- **Mesh-ready ordering:** causal/Lamport order is host-independent, so Rung 3 replication inherits a consistent timeline for free.

## Error handling / edge cases

- **Chain gap/fork** (seq skipped, or two events share a `prev`): reject the event with a specific error; the client resyncs its chain head from the server. (Under single-host this should only happen on client bugs or a reset; the resync path also seeds Rung 3.)
- **Clock skew / non-monotonic lamport:** reject; client recomputes from `max seen + 1`.
- **Bad signature / wrong server_id / unauthorized author / over-limit content:** reject with the existing error surface; nothing is appended.
- **Derived-view drift:** on startup, inconsistency between `events` and `messages` triggers a rebuild of `messages` from `events` (log wins).
- **Client chain state lost/corrupt:** treat as "no local state," fetch the author's chain head from the server, resume. Never sign with a guessed seq.

## Testing

- **Rust (`farder-crypto`)**: event canonical-bytes determinism; sign/verify round-trip; tamper detection (flip a byte → verify fails); genesis id stability; hash linking.
- **Rust (`farder-server`)**: validation matrix — good event accepted + appended + view updated; bad signature / wrong server_id / seq gap / fork / non-monotonic lamport / unauthorized author / over-limit all rejected with no append; ordering of concurrent events from two authors is deterministic and matches the canonical rule; derived `messages` view rebuilt from `events` matches the live view; history pagination returns canonical order. Reuse the existing single-threaded test harness.
- **Seam/integration**: a signed event submitted via the new request path is logged, broadcast carrying its signature, and a second client verifies it; restart → `messages` view rebuilds from `events` and history is identical.
- **Frontend type-check**: `npx tsc --noEmit` clean for the client changes (chain/clock store, send path, verification). No JS test runner (per CLAUDE.md).

## Open questions resolved (decisions locked)

- **Author signs, not host** — required for tamper-evidence.
- **Fresh-start, no migration** — old servers are disposable test envs.
- **Plaintext payloads this rung** — encryption is Rung 2.
- **Only messages through the log this slice** — other state stays in DB tables; message-event authorization reads existing perms.
- **Ordering = (lamport, author, event_hash)** — deterministic, host-independent, mesh-ready.
- **Log is truth; `messages` table is a derived, rebuildable view.**

## Future rungs (context, not built here)

Rung 1 follow-ons: convert channels/roles/members/bans/invites to events; relay `server_id` binding; client-side "verified" indicators. Then Rung 2 (channel E2EE over the same event payloads), Rung 3 (gossip replication + multi-host — the chain/clock/causal-order machinery built here is exactly what sync needs), Rung 4 (anchors + scale).
