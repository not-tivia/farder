# Mesh Foundation — Signed Event Log (Rung 1) — Design Spec

**Date:** 2026-06-25
**Status:** Approved (brainstorm complete, ready for implementation plan)
**Part of:** the mesh-hosting north-star (see project memory `project_farder_mesh_hosting`). This is **Rung 1**, the keystone the rest of the mesh clips onto.

## Where this sits

The mesh roadmap, in order:
1. **Rung 1 (this spec): make a server's truth a *signed logbook*** — single-host, no encryption yet.
2. Rung 2: channel content end-to-end encryption (group keys; hosts hold sealed envelopes).
3. Rung 3: replication + multi-host (the actual mesh — failover first, then full member-hosting).
4. Rung 4: availability anchors + large communities.

This spec covers the design of all of Rung 1. The red-team's two passes grew Rung 1 from "messages on a log" into "a small authorization state machine on a log," so — per round-2's recommendation — **Rung 1 is built as four sequential sub-projects, each its own implementation plan**, rather than one big push:

1. **Genesis + event crypto + device-chain schema** — server genesis/identity, the `Event`/`DeviceCert` types, canonical-bytes + sign/verify/hash, and the `(author, device)` chain model. No app behavior yet; pure foundation + tests.
2. **Minimal authz log state machine** — the authz-core event types (`DeviceAuthorized`, `InviteCreated`, `MemberJoined`, `MemberRemoved`, `MemberBanned/Unbanned`, `PermissionGranted`), their signing/validation rules, and the fold that derives current membership + permissions from the log.
3. **Message posting over the log** — `MessagePosted` validated purely by replaying #2; the `events` table as source of truth + the `messages` table as a derived view; the new submit/broadcast protocol; client signing + per-device chain state.
4. **Attachment capability references** — `AttachmentCap`, cap-vs-bytes validation, the revocable/redactable blob model, existence-oracle gating.

The invariant established from sub-project #3 onward: *authorization is a pure function of prior log events.* The rest of this document is the shared design those four plans draw from.

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
  author: PublicKey,          // the IDENTITY this event belongs to (membership/perms attach here)
  device: DeviceId,           // WHICH of the identity's devices signed it = hash(device_pubkey)
  seq: u64,                   // sequence within THIS (author, device) chain (0-based, +1 per event)
  prev: Option<EventHash>,    // hash of this (author,device) chain's previous event (None for seq 0)
  lamport: u64,               // logical clock: 1 + max(lamport of every event this device had seen)
  timestamp: u64,             // device's wall-clock claim — UNTRUSTED, tiebreak only
  payload: EventPayload,      // the action
  signature: Signature,       // the DEVICE subkey's Ed25519 sig over canonical_bytes(all fields above)
}
EventHash = hash(canonical_bytes(Event))   // includes the signature; content id used in `prev` and as the event's id
```

**Device subkeys (red-team round-2 #1 — set in the schema NOW, even though multi-device
UX may ship later; retrofitting the chain identity would be a painful migration).** Each
device an identity uses has its OWN Ed25519 subkey; the IDENTITY key signs a one-time
certificate authorizing it, and **events are signed by the device subkey, not the identity
key**:

```
DeviceCert {
  identity: PublicKey,        // the owning identity (the chain's `author`)
  device_pubkey: PublicKey,   // the device's signing subkey
  device_id: DeviceId,        // = hash(device_pubkey)
  created_at: u64,
  signature: Signature,       // the IDENTITY key's sig over the above — proves the identity authorized this device
}
```

Verifying an event = (1) the `DeviceCert` is valid and identity-signed (so `device` is
authorized by `author`), AND (2) the event's signature verifies under the device subkey.
The cert enters the log as a `DeviceAuthorized` event. The chain is therefore keyed by
`(author, device)`: the same identity signing from two devices runs **two parallel chains
that merge by causal/Lamport order** (like two authors) instead of forking a single one.
Device revocation is a later-rung signed event.

`EventPayload` for this slice (extensible enum; only the first variant is built now):

```
EventPayload =
  | MessagePosted { channel_id, content, reply_to: Option<EventRef>, attachments: Vec<AttachmentCap> }
  // --- Authorization core — IN Rung 1 (message authz must be a pure fold of these) ---
  | DeviceAuthorized { cert: DeviceCert }            // an identity adds a device subkey (see above)
  | InviteCreated { code_hash, max_uses, expires_at }
  | MemberJoined { member, invite: EventRef }        // joiner-authored; MUST cite a valid InviteCreated
  | MemberRemoved { member }                         // voluntary leave (self) OR kick (authority)
  | MemberBanned { member }                          // authz-REVOKING (round-2 #3): bans the IDENTITY
  | MemberUnbanned { member }
  | PermissionGranted { member, capability }         // by an authority that already holds grant authority
  // --- Follow-ons (Rung-1 follow-on specs) ---
  // MessageEdited, MessageDeleted, AttachmentRedacted, ChannelCreated, RoleCreated/Assigned,
  // ChannelAclSet, MemberTimeout(+reason/audit), DeviceRevoked, ...

AttachmentCap = { content_hash, declared_type, size, uploader }  // content-addressed capability, NOT a server-local file id
```

### Authorization events & signing rules (red-team round-2 #2)

An authz event is valid only if **authored by a key that the *prior log* already grants the
relevant authority** — otherwise anyone could self-join or self-grant. Per payload (`author`
= the signing device's identity):

| Event | Valid when authored by | Extra checks (folded from the prior log) |
|-------|------------------------|------------------------------------------|
| `DeviceAuthorized` | the **identity itself** (`cert.identity == author`, identity-signed cert) | the identity is a current, non-banned member (or it's the owner's first device at genesis) |
| `InviteCreated` | a member who holds **invite authority** (owner, or a granted capability) | not banned; within any invite-rate limits |
| `MemberJoined` | the **joining member** (`member == author`) | cites an `InviteCreated` that is unexpired and under `max_uses` in the prior log; joiner not currently banned |
| `MemberRemoved` | the **member themselves** (leave) **or** a member with **kick authority** | target is a current member |
| `MemberBanned` / `MemberUnbanned` | a member with **ban authority** (owner/mod) | cannot ban the owner; ban supersedes any later `MemberJoined` by that identity |
| `PermissionGranted` | a member who **already holds that grant authority** in the prior log (owner is the root of authority via the genesis) | target is a current, non-banned member |
| `MessagePosted` | any **current, non-banned member with the post capability** in `channel_id` | (the demonstrating action) |

The **owner** (named in the genesis) is the root of authority: at genesis the owner implicitly
holds all capabilities and is the only one who can bootstrap the first grants/invites. A banned
identity's subsequent events (from any device) are rejected. These rules ARE the "fold the prior
log" computation referenced in the message-flow authorization step.

Attachments are referenced by **content hash** (a *capability*), never by the server-local numeric `file_id` — numeric ids aren't meaningful across hosts/replication, and a content-addressed reference is what the revocable-capability model (below) needs. The local `files` store stays the read-model and maps `content_hash <-> file_id` so today's upload/render path keeps working; **file bytes are never part of the event/log.**

- **Per-device chain** (`(author, device)` + `seq` + `prev`): each *device* of an identity has its own append-only chain, so the same identity signing from two devices does NOT fork — concurrent device chains merge by causal/Lamport order, exactly like two authors do. The server detects **gaps** (missing seq) and **forks** (two events sharing a `prev`) per `(author, device)` chain.
- **Lamport clock**: a logical time that works without a central sequencer (mesh-ready). The device sets `lamport = 1 + max lamport of all events it has observed`.

### Ordering

Canonical display order within a channel is a **deterministic total order**, computed from the log (not a database auto-counter):

```
order by (lamport ASC, then author bytes ASC, then event_hash ASC)
```

This is a stable, host-independent tiebreak: any node with the same set of events derives the identical order. At this single-host rung the server computes it; the same rule will hold under replication. `timestamp` is display-only and never used for ordering (it's untrusted).

## Message flow (the proven vertical)

1. **Client builds + signs.** On send, the client constructs a `MessagePosted` event: fills `server_id`, `author` (identity), `device` (this device's id), `seq`/`prev` from its **per-`(server, device)` chain state**, `lamport` from its Lamport clock (`1 + max seen`), `timestamp`, `payload`; computes `EventHash`; signs with the **device subkey**. It tracks chain + clock state per `(server, device)` (new client-side store). On a device's first event to a server it also emits a `DeviceAuthorized` (identity-signed cert) if the server hasn't seen this device.
2. **Submit.** Client sends the signed event to the server (protocol change below).
3. **Server validates.** The server, on receipt:
   1. `server_id` matches this server's genesis.
   2. **Device + signature**: the `device` has a valid identity-signed `DeviceCert` in the log (or this is an accompanying `DeviceAuthorized`); the event's signature verifies under the **device subkey**.
   3. **Chain**: `seq` is the expected next for this `(author, device)` chain (`prev_seq + 1`), and `prev` equals the hash of that chain's last stored event (or `seq == 0 && prev == None` for the first). Reject gaps/forks.
   4. **Lamport** strictly greater than this `(author, device)` chain's previous lamport.
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
- **Cap-vs-bytes validation (round-2 #5):** accepting a `MessagePosted` does NOT trust the cap's fields. If the referenced `content_hash` isn't already in the blob store, the attachment enters a **pending / quarantined** state — the message renders but the attachment is **not downloadable** until a matching blob exists. When the blob is present, the cap's `content_hash`, `size`, normalized type, and recorded `uploader` / `validation_policy_version` / `moderation_state` MUST match the actual stored blob metadata; a mismatch (lying about type/size, or referencing a hash you never uploaded) keeps it quarantined. This stops "reference a hash I don't own" and type/size spoofing.
- **Redaction (à la Matrix) — honest scope (round-2 #4):** a takedown is a signed `AttachmentRedacted { content_hash }` (authorized owner/mod) that flips moderation state and authorizes GC of the bytes. The reference event stays (history honestly says "a file was here"); the payload is removable. **This is protocol-level takedown for *compliant* clients/hosts — it does NOT, and cannot, stop a malicious host that already holds the bytes from serving them out of band.** Do not claim Byzantine-proof takedown.
- **Existence-oracle hardening (round-2 #6):** content-addressing makes a known hash a "does this server hold file X" oracle (e.g. probing for known-abusive-material fingerprints). So **attachment availability/fetch is gated on channel/message authorization** (you can only probe/fetch a blob via a message you're authorized to see), and absent / redacted / not-yet-present responses are made **intentionally uniform** so they don't leak which case it is.
- **Dedupe + policy (round-2 #5):** the blob record stores the validation verdict + `validation_policy_version`; on reuse of an existing hash, re-evaluate if the policy changed — so a file accepted under an old policy isn't silently reused under a stricter one. Per-reference records stay distinct from the blob record so attribution/moderation isn't collapsed into "same hash."
- **Forward note (Rung 2/3):** when attachments become E2E-encrypted + replicated, this capability model is exactly what lets hosts **not replicate / not serve** untrusted or taken-down blobs (the "blind distributor" problem) — encrypted blob, opt-in download, GC-able by capability.

(Server-side magic-byte sniffing / content-type allowlist of the bytes is part of the **separate file-hardening track**, not Rung 1 — but the capability descriptor is where its verdict is recorded.)

## Protocol changes (`farder-protocol`)

- New types: `Genesis`, `ServerId`, `DeviceId`, `DeviceCert`, `Event`, `EventPayload` (incl. the authz-core variants + `MessagePosted`), `EventHash`, `EventRef`, `AttachmentCap` (`reply_to`/`invite` reference another event — numeric id for client compat mapped to event hash; plan decides the exact `EventRef` shape).
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

- **Rust (`farder-crypto`)**: event + `DeviceCert` canonical-bytes determinism; **device-cert sign/verify** (identity authorizes device) AND event sign/verify (device subkey); tamper detection (flip a byte → verify fails); genesis id stability; hash linking.
- **Rust (`farder-server`)**: validation matrix — good event accepted + appended + derived; rejected with **no append** for bad signature, unknown/uncertified device, wrong server_id, seq gap/fork, non-monotonic lamport, over-limit. **Authz signing rules:** self-`MemberJoined` without a valid unexpired invite → rejected; self-`PermissionGranted` → rejected; `MessagePosted` by non-member or banned identity → rejected; **a `MemberBanned` supersedes a later `MemberJoined` by that identity** (can't rejoin past a ban). **Multi-device:** the same identity signing from two devices yields two valid, non-forking chains. **Attachment caps:** a cap referencing a missing hash is **quarantined / not downloadable**; type/size mismatch vs the stored blob → quarantined. **Ordering** of concurrent events — two distinct authors AND two devices of one identity — is deterministic per `(lamport, author, event_hash)`. Derived `messages` view rebuilt from `events` matches the live view; history pagination returns canonical order. Reuse the existing single-threaded harness.
- **Seam/integration**: a signed event submitted via the new request path is logged, broadcast carrying its signature, and a second client verifies it; restart → `messages` view rebuilds from `events` and history is identical.
- **Frontend type-check**: `npx tsc --noEmit` clean for the client changes (chain/clock store, send path, verification). No JS test runner (per CLAUDE.md).

## Open questions resolved (decisions locked)

- **Author signs, not host** — required for tamper-evidence.
- **Fresh-start, no migration** — old servers are disposable test envs.
- **Plaintext payloads this rung** — encryption is Rung 2.
- **Authorization is replayable from the log** (round-1) — membership, **bans**, invites, device-authorization, and the permission basis are log events in the Rung-1 authz core, each with explicit signing rules; message authz is a pure fold of the prior log, never out-of-log tables. Richer roles / channel-ACLs are follow-ons.
- **Device subkeys + per-`(identity, device)` chains** (round-2) — baked into the event schema now; the same identity on two devices runs parallel chains and does not fork.
- **Attachments are content-addressed revocable capabilities** (round-1) with cap-vs-bytes validation, existence-oracle gating, and compliant-host (not Byzantine-proof) redaction (round-2).
- **Client pins the server genesis** (round-1); relay-ownership binding lands with/after Rung 1.
- **Rung 1 ships as four sub-projects** (round-2): genesis+crypto+chain → authz state machine → message posting → attachments.
- **Ordering = (lamport, author, event_hash)** — deterministic, host-independent, mesh-ready.
- **Log is truth; `messages` table is a derived, rebuildable view.**

## Future rungs (context, not built here)

Rung 1 follow-ons: richer roles / channel-ACLs / timeouts / device-revocation as event types (membership, bans, invites, device-authorization + the permission basis are already in the Rung-1 authz core); client-side "verified" indicators. Then Rung 2 (channel E2EE over the same event payloads — adopting **OpenMLS** for group keys), Rung 3 (gossip replication + multi-host — the chain/clock/causal-order machinery built here is exactly what sync needs), Rung 4 (anchors + scale).

## Changes from the external red-team review (2026-06-25)

An independent adversarial review (run through Codex; brief at `docs/superpowers/audits/2026-06-25-mesh-design-red-team-brief.md`) was performed on this design + the current codebase. What was folded into this spec:

- **Replayable authorization** (review #8): message-event validity must derive only from the log → membership + the permission basis are now in-log from Rung 1 (was: read from DB tables). Prevents a Rung-3 redo where hosts with divergent DB state disagree on whether a message was authorized.
- **Revocable attachment capabilities** (review #7): the log holds an immutable content-addressed *reference*; the bytes are a separate GC-able / redactable object — append-only must not make malware/illegal-content takedown or retention impossible.
- **Server-identity pinning + relay-ownership binding** (review #9): client pins the genesis (TOFU + warn-on-change); relay registration requires a genesis-owner signature.
- **Dedupe re-evaluates under policy version** (review #5).

Acted on **outside** this spec:
- **SSRF in `FetchUrl` (review #2, Critical) — FIXED + MERGED** (`crate::ssrf`: manual redirect loop re-validating that every hop resolves to a globally-routable IP).
- **File-handling hardening (review #1/#3/#4)** — server-side magic-byte sniffing + content-type allowlist, download-filename sanitization, media auto-render limits — a **separate near-term track**, not Rung 1.

### Round 2 (2026-06-25)

A second adversarial pass (brief: `docs/superpowers/audits/2026-06-25-mesh-design-red-team-round2.md`) confirmed round-1 was directionally right but caught foundation holes; all folded in above:

- **Device subchains in the schema NOW** (round-2 #1, High): the `Event` chain is keyed by `(author, device)` with identity-signed `DeviceCert`s and device-subkey signatures — so multi-device doesn't fork. Was previously deferred; deferring would have forced an event-identity migration. **This was the highest-risk item; it is now addressed in Rung 1.**
- **Authz-event signing rules fully specified** (round-2 #2, High): the per-payload "valid when authored by …" table prevents self-join / self-grant.
- **Minimal bans in the authz core** (round-2 #3, High): `MemberBanned/Unbanned` are in Rung 1 — a removed member could otherwise rejoin; a ban is an authz-revoking fact the state machine must know.
- **Attachment cap-vs-bytes validation + existence-oracle gating + honest redaction scope** (round-2 #4/#5/#6, Medium).
- **Rung 1 re-split into 4 sub-projects** (round-2): keeps the grown scope manageable.

Still carried forward as **later-rung** constraints (NOT solved here): group-key management + abuse handling in a member-hosted mesh (the "single most likely sinker"); the E2EE-blob "blind distributor" problem; Sybil / ban-evasion; relay as a metadata chokepoint. Known **disclosed residual** on the merged SSRF fix (round-2 #7): resolve-then-dial-by-host leaves a narrow DNS-rebind race — best later fix is to pin the resolved IP into the connection. Strategic: **adopt OpenMLS for group keys** (never hand-roll group crypto); "adopt Matrix wholesale" considered and **rejected** (federation-of-servers, not member-mesh).
