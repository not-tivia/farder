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

This spec is the **walking skeleton of Rung 1**: stand up the entire spine (server identity → signed event log → validation → derived read-model). **Order matters (revised after the red-team review — see the final section):** the **authorization-defining events come first** — genesis (owner), membership (join/leave), and the permission basis — so the demonstrating action, **posting a message**, validates purely by *replaying the log*, never against out-of-log database state. Richer role / channel-ACL / ban event types are layered on as Rung-1 follow-ons, but the invariant — *authorization is a pure function of prior log events* — holds from the first message.

## Goal

Re-found a server's source of truth from "whatever the single host's SQLite database says" to "an append-only log of **author-signed events** that any node can verify," with the SQLite database demoted to a **derived read-cache rebuilt from the log**. Give every server a **permanent cryptographic identity**. Ship value on its own: a **tamper-evident server** — even the host cannot forge or silently alter message history.

Single-host only. Payloads stay **plaintext** at this rung (encryption is Rung 2). **Fresh-start:** the log model applies to **newly created servers**; existing servers are disposable test environments and are not migrated.

## Non-goals (explicitly out of scope for this slice)

- Encryption of event payloads (Rung 2).
- Replication / multiple hosts / gossip sync (Rung 3).
- **(Revised after red-team.)** Deferred to Rung-1 follow-on specs: *richer* role definitions, channel/category ACL overrides, ban/timeout, and invites as event types. **Not** deferred: **membership (join/leave) and the permission basis for posting are in the log from the start**, because message-event validity must be replayable from the log alone (see Global Constraints). Channel *config* (names/types) may remain DB read-model this slice as long as it does not gate message-authorship validity beyond "channel exists."
- Migrating existing servers.
- Full file-handling hardening (server-side content sniffing/allowlist, download-filename sanitization, media auto-render limits) — tracked as a **separate near-term track** alongside the already-merged SSRF fix; not part of Rung 1. Attachment *modeling* in the log IS in scope (see "Attachments as revocable capabilities").

## Global Constraints

- **Author-signed, not host-signed.** Events MUST be built and signed by their **author's client** (the identity key), never by the server — that signature is the entire tamper-evidence property. The server only *validates, stores, and broadcasts*.
- **Fresh-start.** New servers create a genesis; existing servers keep working unchanged. No migration code.
- **Single source of truth = the log.** The `messages` table becomes a materialized view derived from message events; it must be reconstructible from the `events` table alone (verified on startup / rebuildable).
- **Replayable authorization (red-team requirement).** Whether an event is authorized MUST be a deterministic function of the *prior log* (membership + permission events) — so any node replaying the same log reaches the identical verdict. Never validate an event against out-of-log mutable state. This is the reason membership + the permission basis are log events from Rung 1.
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
EventPayload =
  | MessagePosted { channel_id, content, reply_to: Option<EventRef>, attachments: Vec<AttachmentCap> }
  // Authorization-defining events — IN Rung 1 (needed for replayable message authz):
  | MemberJoined { member, via_invite: Option<EventRef> }
  | MemberRemoved { member }                 // leave/kick
  | PermissionGranted { member, capability } // the minimal "can post" basis; richer roles are follow-ons
  // Follow-on event types (Rung-1 follow-on specs): MessageEdited, MessageDeleted, AttachmentRedacted,
  // ChannelCreated, RoleCreated/Assigned, ChannelAclSet, MemberBanned/Timeout, InviteCreated, ...

AttachmentCap = { content_hash, declared_type, size, uploader }  // content-addressed capability, NOT a server-local file id
```

Attachments are referenced by **content hash** (a *capability*), never by the server-local numeric `file_id` — numeric ids aren't meaningful across hosts/replication, and a content-addressed reference is what the revocable-capability model (below) needs. The local `files` store stays the read-model and maps `content_hash <-> file_id` so today's upload/render path keeps working; **file bytes are never part of the event/log.**

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
   5. **Authorization — replayable from the log.** Fold the *prior* log (genesis → membership → permission events) to compute current membership + permissions, then check `author` is a member with the post capability in `channel_id`. This MUST derive from log events, **not** from out-of-log mutable tables, so any replaying node reaches the same verdict. The derived permission state may be *cached* in the DB, but that cache is a projection of the log, never an independent authority.
   6. **Limits**: content ≤ 8000 chars and other existing `SendMessage` checks.
4. **Append + derive.** Append the raw event to the new `events` table (append-only). Update the derived `messages` table (the materialized view) so existing reads/pagination/search keep working. Bump the server's view of the author's chain head + a server-wide max-lamport (so the server can sanity-check future events and assign its own outbound events later).
5. **Broadcast.** Broadcast a `NewMessage` event that now **carries the full signed event**, so other clients can (a) verify the author's signature themselves and (b) advance their own Lamport clock (`max(seen, event.lamport)`).
6. **Recipients verify + order.** Receiving clients verify the signature, fold the event into their local view, and order by the canonical rule. (Full client-side verification UI/indicator is optional this slice; the wire format must carry the signature so verification is *possible* and clocks stay correct.)

## Storage

- **New `events` table** (append-only, the source of truth): stores the raw canonical event bytes + indexed columns for query/validation: `event_hash` (PK), `server_id`, `author`, `seq`, `lamport`, `payload_type`, `channel_id` (nullable, denormalized for message lookups), `created_local_ts`. Unique index `(author, seq)`; index `(channel_id, lamport, author, event_hash)` for ordered history.
- **`messages` table = derived view.** Keep its existing shape/columns (so `fetch_history`, FTS search, replies, reactions keep working) but populate it from message events. Its numeric `id` stays for client compat; add a column mapping `id <-> event_hash`. Ordering served to clients follows the canonical rule (by lamport/author/hash), surfaced through the existing pagination API.
- **Rebuildability:** on startup the server verifies the `messages` view is consistent with `events` (or rebuilds it by replaying message events). The `events` table alone is sufficient to reconstruct all message state.
- **Permission read-model:** current membership + permissions are also a derived projection of the log (folded from membership/permission events), cached for fast authorization checks — never an independent source of truth.

## Attachments as revocable capabilities (red-team change)

The log records the **immutable fact** that a message references an attachment, but the **bytes are a separate, revocable, garbage-collectable object** — so takedown / retention / redaction can remove content without rewriting (or being blocked by) the append-only log. This avoids baking "immutable signed reference → permanent blob" into the foundation (the malware / illegal-content + retention trap the red-team flagged).

- A `MessagePosted` event carries, per attachment, an `AttachmentCap` **capability descriptor** (`content_hash`, `declared_type`, `size`, `uploader`) — content-addressed, tamper-evident about *what* is referenced, but **not** a promise of permanent availability.
- The **blob** lives in the content-addressed `files` store, governed by **mutable** state outside the immutable event: `validation_policy_version`, `moderation_state` (ok / quarantined / taken-down), and retention. A host may **garbage-collect or refuse to serve** a blob per that state and `retention_secs`, independently of the (immutable) reference event.
- **Redaction (à la Matrix):** a takedown is itself a signed event (`AttachmentRedacted { content_hash }`, by an authorized owner/mod) that flips moderation state and authorizes GC of the bytes. The reference event stays (history honestly says "a file was here"); the payload is removable. Generalizes to message-body redaction later.
- **Dedupe + policy (red-team #5):** the blob record stores the validation verdict + `validation_policy_version`; on reuse of an existing hash, re-evaluate if the policy changed — so a file accepted under an old policy isn't silently reused under a stricter one. Keep per-reference records distinct from the blob record so attribution/moderation isn't collapsed into "same hash."
- **Forward note (Rung 2/3):** when attachments become E2E-encrypted + replicated, this capability model is exactly what lets hosts **not replicate / not serve** untrusted or taken-down blobs (the "blind distributor" problem) — encrypted blob, opt-in download, GC-able by capability.

(Server-side magic-byte sniffing / content-type allowlist of the bytes is part of the **separate file-hardening track**, not Rung 1 — but the capability descriptor is where its verdict is recorded.)

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
- **Stable server identity:** `server_id = hash(genesis)`, owner cryptographically fixed — closing today's "throwaway cert / first-connector-is-owner / relay can't verify ownership" gaps (with client pinning + relay binding — see the next section).
- **Mesh-ready ordering:** causal/Lamport order is host-independent, so Rung 3 replication inherits a consistent timeline for free.

## Server identity pinning (red-team change)

A stable `server_id = hash(genesis)` only protects you if clients **pin** it (the red-team flagged impersonation / malicious-history substitution).

- **Client pinning (IN scope for Rung 1):** on first successful connect, the client pins the server's genesis (trust-on-first-use) — stores `server_id` + owner pubkey for that saved server. On reconnect, if the presented genesis/owner differs, **hard-warn and refuse silent continuation** (user must explicitly re-accept) — same posture as an SSH host-key change.
- **Relay-ownership binding (moved up from "deferred"):** the relay must require a **genesis-owner signature** to register/serve a `server_id`, closing today's "anyone can claim a `server_id`" gap. It depends only on the genesis existing, so it lands with Rung 1 or its immediate follow-on — and becomes essential the moment any host can register (Rung 3).

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
- **Authorization is replayable from the log** (red-team change) — membership + the permission basis are log events from Rung 1; message authz is a pure fold of the prior log, never out-of-log tables. Richer roles/ACLs/bans/invites are follow-on event types.
- **Attachments are content-addressed revocable capabilities** (red-team change) — the log holds an immutable reference; bytes are a separate GC-able/redactable object.
- **Client pins the server genesis** (red-team change); relay-ownership binding lands with/after Rung 1.
- **Ordering = (lamport, author, event_hash)** — deterministic, host-independent, mesh-ready.
- **Log is truth; `messages` table is a derived, rebuildable view.**

## Future rungs (context, not built here)

Rung 1 follow-ons: richer roles / channel-ACLs / bans / invites as event types (membership + the permission basis are already in Rung 1); client-side "verified" indicators. Then Rung 2 (channel E2EE over the same event payloads — adopting **OpenMLS** for group keys), Rung 3 (gossip replication + multi-host — the chain/clock/causal-order machinery built here is exactly what sync needs), Rung 4 (anchors + scale).

## Changes from the external red-team review (2026-06-25)

An independent adversarial review (run through Codex; brief at `docs/superpowers/audits/2026-06-25-mesh-design-red-team-brief.md`) was performed on this design + the current codebase. What was folded into this spec:

- **Replayable authorization** (review #8): message-event validity must derive only from the log → membership + the permission basis are now in-log from Rung 1 (was: read from DB tables). Prevents a Rung-3 redo where hosts with divergent DB state disagree on whether a message was authorized.
- **Revocable attachment capabilities** (review #7): the log holds an immutable content-addressed *reference*; the bytes are a separate GC-able / redactable object — append-only must not make malware/illegal-content takedown or retention impossible.
- **Server-identity pinning + relay-ownership binding** (review #9): client pins the genesis (TOFU + warn-on-change); relay registration requires a genesis-owner signature.
- **Dedupe re-evaluates under policy version** (review #5).

Acted on **outside** this spec:
- **SSRF in `FetchUrl` (review #2, Critical) — FIXED + MERGED** (`crate::ssrf`: manual redirect loop re-validating that every hop resolves to a globally-routable IP).
- **File-handling hardening (review #1/#3/#4)** — server-side magic-byte sniffing + content-type allowlist, download-filename sanitization, media auto-render limits — a **separate near-term track**, not Rung 1.

Carried forward as constraints for **later rungs** (explicitly NOT solved here): group-key management + abuse handling in a member-hosted mesh (the review's "single most likely sinker"); the E2EE-blob "blind distributor" problem; **multi-device-with-one-identity chain forking** (needs per-device subkeys/chains — design before Rung 3); Sybil / ban-evasion; relay as a metadata chokepoint. Strategic: **adopt OpenMLS for group keys** (never hand-roll group crypto); "adopt Matrix wholesale" was considered and **rejected** (it's federation-of-servers, not member-mesh).
