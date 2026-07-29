# Mesh Rung 2 — E2EE Community Content via OpenMLS — Design Spec

**Date:** 2026-07-27
**Status:** Draft **rev 2** — red-team findings folded in (see `docs/superpowers/audits/2026-07-27-rung2-red-team-security.md` and `-product.md`)
**Part of:** the mesh-hosting north-star (see project memory `project_farder_mesh_hosting`). This is **Rung 2** of the ladder locked in the Rung-1 spec (`2026-06-25-mesh-signed-log-foundation-design.md`).

## Revision note (what changed in rev 2)

Two red-team passes broke the rev-1 draft in one place: rev 1 made the plaintext authz fold the authority and MLS a projection of it, but left **log delivery, projection correctness, and projection freshness** all under the control of the party E2EE exists to defend against (a malicious host, or any single lying member). It also mis-specified the codebase it lands in: enforcement was written against the log path while most content the product ships today is written by legacy paths that never become log events.

Rev 2 folds in eight structural fixes, all of which change fold schema or event shapes and therefore had to land **before sub-project 1**:

1. **Channels are log objects with a class** (`ChannelCreated { class }`), and unknown class = unusable, never "assume plaintext" — fail closed (was: absence of a flag ⇒ plaintext).
2. **In-band head attestation** (`authz_head` on every commit and sealed message, plus a periodic `AuthzBeacon`) so a withholding host is detectable inside the group.
3. **Commits are chained to the real tree** (`prev_epoch_authenticator` + `post_tree_hash`, chained by the fold) and **leaves only count when the joiner confirms them** (`MlsLeafConfirmed`) — a lying commit can no longer silently disarm the membership bridge.
4. **A mandatory rekey cadence with a blind-fold-enforced ceiling**, so the forward secrecy and post-compromise security the spec claims are actually delivered.
5. **A single server-side write choke point** plus **fail-closed rendering** on the client, so no legacy or server-authored plaintext path can write into (or be rendered inside) an E2EE channel.
6. **Non-selective group reset** — a reset is only valid if it re-Welcomes exactly the fold's member set.
7. **MLS-store instance binding** (no-resume on clone/restore) to make AES-GCM nonce reuse impossible after a backup restore or profile copy.
8. **KeyPackage lifetimes + caps, device caps, self-add rule, commit-rate rule, deterministic drift-priority tiebreak** — the set of small fold rules that close the DoS/lockout/ghost-device family.

Everything the red teams raised that is a genuine *product* decision rather than a design fix has been moved to **Open product questions** with a recommended default. Minor findings are recorded in **Known accepted risks / minor log** with their disposition.

## Where this sits

The mesh roadmap, in order:
1. Rung 1 (SHIPPED): a server's truth is a signed, replayable event log — single host, plaintext payloads.
2. **Rung 2 (this spec): channel content end-to-end encryption** — group keys via **OpenMLS**; the host stores sealed envelopes it cannot read.
3. Rung 3: replication + multi-host (members hosting ciphertext they cannot read — Rung 2 is its prerequisite).
4. Rung 4: availability anchors + large communities.

Rung 1 locked three constraints this spec must preserve and one strategic decision it must honor:

- **Checkpoint-composable authz fold** — `LogState::apply` is a pure `(prior_state, event) -> new_state`; replay == stepwise, composable from any checkpoint (proven by test `replay_equals_stepwise_and_composes_from_a_checkpoint`). Every new piece of state this spec adds to the fold must keep that property.
- **Content-addressed revocable attachment capabilities** — bytes live outside the log, GC-able; the cap descriptor is what a blind host validates.
- **Device subkey chains** — events are signed by per-device Ed25519 subkeys certified by the identity key; chains keyed `(author, device)`.
- **Never hand-roll group crypto — adopt OpenMLS** (Rung-1 decision, locked). This spec uses OpenMLS for *all* group key agreement; the only crypto we write is glue (credential binding, envelope framing) and per-file symmetric sealing using existing `farder-crypto` primitives.

## Goal

Make community (channel) message content **ciphertext at rest on the server** for channels created as E2EE — so that a Rung-3 mesh host replicating the log holds nothing readable. Group key agreement comes from MLS (RFC 9420) via the audited OpenMLS crate; membership *authority* stays exactly where Rung 1 put it: the plaintext authz fold of the signed log.

**Honest statement of the crypto property** (corrected in rev 2): forward secrecy and post-compromise security are **bounded by the rekey interval**, not absolute. MLS gives FS/PCS per epoch transition; a design that only rotates epochs on membership change delivers neither on a quiet channel. This spec therefore makes a rekey cadence a protocol obligation with a fold-enforced ceiling (see "Freshness"), and states the bound in product copy: *"messages older than your last rekey are protected only as well as your device is."*

## Non-goals (explicitly out of scope for this rung)

- Replication / multi-host (Rung 3). The design must not *block* it (fold determinism, see the commit-race section), but only the single-host path ships.
- Migrating DMs onto MLS. Today's DM path (static-static ECDH, no forward secrecy) is untouched; its weakness is noted in Risks and its migration is an open product question.
- Encrypting *metadata*: membership, permissions, channel names, event timing, message sizes, and reply-graph shape remain visible to the host and relay. Length hiding gets a partial answer this rung (padding ladder, see Metadata), but the relay-as-metadata-chokepoint problem stays a later-rung constraint.
- Encrypted server-side search, encrypted bots/widgets/webhooks inside E2EE channels (each has a stated fate in the Coexistence section; most are "non-E2EE channels only" this rung).
- History escrow for late joiners (design decision below: **not built**; its cost is stated honestly rather than oversold).
- Converting existing plaintext channels to E2EE. Fresh-start, matching Rung 1: the class is chosen at channel creation and is immutable.
- Identity-key rotation (pre-existing Rung-1 gap). Rev 2 does, however, close the *consequence* that Rung 2 would otherwise add: silent unbounded device addition into an E2EE group (see Multi-device).

## Global constraints

- **Authorization inputs stay plaintext in the log.** Membership, bans, invites, device certs, permission grants, channel class, and all MLS *control* metadata (epoch numbers, declared adds/removes, tree hashes) are outside the encrypted envelope — otherwise a checkpoint-holder cannot validate, and Rung 3 hosts cannot order or replicate. Only *content* is sealed. This is the honest trade: E2EE channels hide **what was said**, not **who is in the room or when they spoke**.
- **The fold stays pure and checkpoint-composable.** All new MLS state tracked by the fold is small, deterministic, derived only from `(prior_state, event)`, and serializes into checkpoints. **No wall-clock inputs in the fold** — every "freshness" and "rate" rule below is expressed in log positions/event counts, never in seconds.
- **The server never holds a group key, ever.** No server-side member leaf, no escrow, no "server bot" inside an E2EE group. Any feature that would require it is deferred or confined to non-E2EE channels. Consequence the owner must know: a self-hosted always-on sidecar **cannot** steward keys for its own community, by design.
- **Fail closed, everywhere.** Unknown channel class ⇒ channel unusable. Unresolvable class, unverifiable message, missing tombstone knowledge, unknown protocol version, stale epoch beyond the ceiling, pending removals outstanding ⇒ refuse, don't degrade to plaintext. Every "absence of X means the permissive thing" rule from rev 1 is inverted.
- **Old events are untouched.** Existing `EventPayload` variants keep their exact canonical bytes. New behavior arrives as *new* variants. See "Protocol compatibility" — rev 1's compat claim covered the wrong direction and is corrected.
- **Verify by observation** (CLAUDE.md): every E2EE claim ships with an observation test capturing the real wire/storage bytes and asserting no plaintext — **one per content-producing path**, enumerated in sub-project 3/4.

## The channel content class: a property of the channel's identity, in the log

**Decision: per-channel, not all-or-nothing; class is part of channel creation and lives in the log.** A channel is created either **plaintext-class** (today's behavior, full server feature set) or **E2EE-class** (content sealed, degraded server features). The class is immutable.

Why per-channel and not server-wide:

- The July feature set the owner actively uses (ticker bots, webhooks, slash commands, polls/giveaways — see Coexistence) *fundamentally requires* server-readable content. All-or-nothing E2EE would kill features shipped this month or gut the E2EE promise with a key-holding server. Per-channel lets both be honestly what they claim.
- It matches the product story: "#general has the bots and the polls; #private is sealed."

### Log representation (rev 2 — fail closed)

Rev 1 put a `ChannelE2eeEnabled { channel_id }` event in the log and said "absence of the event = plaintext-class." That fails **open**: a host that simply never delivers the flag downgrades the victim's client to the plaintext send branch, with no forgery, and Rung 1's tamper-evidence explicitly does not cover omission. Worse, the channel itself was a DB row, so there was no in-log object to check the row against.

Rev 2:

- New payload **`ChannelCreated { channel_id, name, kind, class: Plaintext | E2ee, parent: Option<u64> }`** — the class is a field of the channel's *identity*, authored by the owner (this rung: owner-only, see M3 disposition) at creation time.
- Fold rules:
  - Either message variant is **invalid** in a channel with no prior `ChannelCreated`.
  - `MessagePosted` (plaintext) is invalid in an `E2ee` channel; `MessagePostedE2ee` is invalid in a `Plaintext` channel.
  - There is **no** class-change event. Class is set once, at creation, or the channel does not exist to the log.
- **Client rule (the load-bearing half): the client refuses to render or post to any channel it cannot resolve to a `ChannelCreated` in its own folded log.** Unknown class ⇒ channel shown as unavailable with an explicit "this channel's security level could not be verified" state. Never "assume plaintext."
- **Atomicity** (product finding F5): channel creation is one operation from the client's perspective — the server creates the DB row only after accepting `ChannelCreated`, and a channel with no accepted class event is hidden/read-only. There is no window in which a classless channel accepts writes.
- **Legacy channels** (pre-existing DB channels with no `ChannelCreated`) are permanently plaintext-class and cannot be flipped; ingest additionally refuses to accept a `ChannelCreated` for a `channel_id` that already has rows in the `messages` table (Rung-2-only belt-and-braces; the fold rule alone is correct for Rung-3 fresh replay).

**Threat-model consequences, stated honestly:**

- In a **plaintext-class** channel, the host (and at Rung 3, every mesh host) reads everything. The class is displayed in the UI (no lock icon; a one-time creation notice: "bots, webhooks and the server can read this channel").
- In an **E2EE-class** channel, the host stores only MLS ciphertext + control metadata. It still sees: who posted, when, roughly how big (padded to buckets), reply-to edges, attachment count/sizes, and full channel membership. A member's compromised device reveals everything that device could decrypt (its local store + current-window epochs).
- A server whose channels are all plaintext-class is not "partially E2EE" — the UI must never imply otherwise.

## MLS mapping

### Library, ciphersuite, storage

- **openmls 0.8.1** (MIT; SRLabs-audited May 2026, all findings remediated in 0.8.1; actively maintained). Sync API — call from tokio via `spawn_blocking`. Pre-1.0 API churn is a named risk.
- Crypto provider: stock **`openmls_rust_crypto`**. Storage: **`openmls_sqlite_storage`** (rusqlite-based) in the *client* crate. MLS state lives client-side only; the server never runs MLS group operations.
- Ciphersuite: **`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`** — the RFC 9420 mandatory-to-implement suite; its Ed25519 signature scheme matches Farder's identity/device keys.
- MSRV note: OpenMLS main targets rustc 1.91+; the implementation plan confirms our toolchain before locking the dep.

### MLS store safety: instance binding and no-resume (rev 2, C6)

The MLS store holds the sender-ratchet **generation counter**. Two live instances sharing one store's contents will encrypt at the same `(epoch, generation)` — identical key and nonce under AES-128-GCM, which is catastrophic (plaintext XOR recovery, tag forgery). The realistic triggers require no attacker at all: restoring the data dir from backup, copying the profile to a second machine, a VM snapshot rollback, or a cloud-synced home directory. Farder has actively trained users that the data dir is portable (`identity.key` + 24-word recovery), so this must be designed against, not documented around.

Rules:

1. **Instance binding.** On MLS store creation the client generates a random 16-byte `store_instance_id`; its hash is published in the device's `MlsKeyPackagePublished` and carried on every `MlsCommit` and `MlsLeafConfirmed` the device authors. On startup, if the local store's `store_instance_id` does not match the value the log records for this `(author, device)`, the client **refuses to resume**: it self-`DeviceRevoked`s and provisions as a fresh device.
2. **Poison detection reuses Rung 1's signal.** Rung 1 already detects "local chain state behind the log." Rev 2 adds the fail-closed inverse: **if the log shows this `(author, device)` chain head ahead of local state, the MLS store is presumed poisoned** — never resume the ratchet, always re-provision as a new device.
3. **Placement.** The MLS DB lives in a subdirectory explicitly marked non-portable, excluded from any in-app backup/export flow, marked non-backup-eligible where the platform supports it, and the recovery UI says plainly: *restoring or copying this folder is unsafe; provision a new device instead*.

Cost, stated: a user who restores a backup loses that device's MLS state (and therefore its ability to decrypt without a local-store copy) and must be re-added. That is the correct trade against nonce reuse.

### Group granularity: one MLS group per E2EE channel

- **Per-server group** would mean every member of the server can decrypt every E2EE channel — channel-granular privacy would be a UI fiction, and all channels' epoch cadence would couple.
- **Per-channel group** gives channel-granular readability (forward-compatible with the Rung-1 follow-on channel-ACLs), contains blast radius, keeps commit streams independent.
- Cost: N groups → N Welcomes on join and N remove-commits on ban, plus per-channel MLS state and per-channel KeyPackage consumption (see KeyPackage lifetimes).

**Membership rule this rung:** every current approved server member's device belongs to every E2EE channel's group (channel-ACL subsets arrive with the ACL follow-on). Note the metadata consequence recorded under Metadata: because Welcomes and commits are log-public, **per-channel rosters are public server-wide** — which must be resolved before the channel-ACL follow-on ships, or ACL'd channels will leak their rosters.

### Leaves: one per (identity, device), bound to the DeviceCert chain

Per RFC 9750 convention, **each device is its own MLS leaf**, mapping one-to-one onto Rung 1's `(author, device)` chain model:

- **Leaf signature key = the device subkey** (Ed25519, same key that signs log events). Validators check the MLS leaf's signature key equals the `device_pubkey` in an identity-signed `DeviceCert` present in the log. Cross-protocol reuse is judged safe (MLS signs under RFC 9420 domain-separation labels; our event signing signs rmp bytes of `EventCore`, which begin with the server-id string — the signed byte domains cannot collide), and remains a flagged risk with a contained alternative (a third MLS-only subkey inside `farder-mls`).
- **Leaf credential**: a basic credential whose identity bytes are a **length-prefixed, domain-separated encoding** (M1 folded in): `"farder-mls-cred-v1" || u8(len(identity_pubkey)) || identity_pubkey || u32(len(device_id_bytes)) || device_id_bytes`. Bare concatenation is forbidden — `DeviceId` is a hex `String` and `PublicKey` is raw bytes, so unprefixed concatenation is ambiguous.
- Every member processing a commit MUST verify: credential identity/device match the leaf signature key via a log-valid, non-expired, non-revoked `DeviceCert`, and the identity is a current, approved, non-banned member per the authz fold. An Add violating this is an invalid commit.
- **`DeviceCert` gains an `expires_at` field** (M5 folded in — one field now, a migration later). Expired certs cannot author events and their leaves must be removed by the membership bridge.
- **KeyPackages**: each device generates and publishes KeyPackages signed by its device subkey, with a **lifetime** and a **live cap** (see below).

### Wire formats

- **Application messages: `PrivateMessage`** (sealed).
- **Handshake (commits/proposals): `PublicMessage`** — membership and epoch progression are already public by constraint; plaintext framing lets the server order by epoch and lets any auditor cross-check declared metadata against actual proposals.
- **Welcome self-containment**: groups use the ratchet-tree extension so a joiner needs nothing else. Cost: Welcome size grows with group size — capped explicitly (see Size caps) with out-of-log tree serving as the escape hatch.
- **Padding (I7, folded in):** application messages are padded to a **bucket ladder** before sealing — 256 B, 1 KiB, 4 KiB, 16 KiB, 40 KiB. OpenMLS does not pad by default; without this, ciphertext length is a plaintext-length oracle ("yes"/"no" vs a paragraph is readable). The ladder is a `farder-mls` configuration constant, near-free, and is a default rather than a deferral.

## Transport over the log: new `EventPayload` variants

The signed event log **is the Delivery Service**: it stores/hands out KeyPackages, queues Welcomes, fans out commits, and supplies the **total order of commits per group** that MLS demands. All new variants are signed/chained/authorized exactly like existing events.

```
| ChannelCreated { channel_id, name, kind, class, parent: Option<u64> }
    // Owner-authored. Class is immutable and part of channel identity.
    // No ChannelCreated => the channel does not exist to the log => unusable.

| MlsKeyPackagePublished { key_package: Vec<u8>, store_instance_hash: [u8;32],
                           expires_at_log_pos: u64 }
    // Authored by the owning device; server-scoped. Consumed-once per the fold.
    // LIFETIME (I5): invalid as an Add target once the channel's log position passes
    // expires_at_log_pos, which lets consumed_key_packages be pruned to the live
    // window safely (otherwise pruning re-enables KeyPackage replay).
    // CAP: at most 10 live (unconsumed, unexpired) KeyPackages per device.

| MlsCommit { channel_id, generation, epoch, mls_message: Vec<u8>,
              adds: Vec<DeclaredAdd>, removes: Vec<DeclaredRemove>,
              prev_epoch_authenticator: [u8;32], post_tree_hash: [u8;32],
              authz_head: EventHash, store_instance_hash: [u8;32] }
    // DeclaredAdd    = { identity, device, key_package: EventRef }
    // DeclaredRemove = { identity, device }
    // Declared fields duplicate intent in fold-readable form (the fold cannot
    // maintain ratchet trees without becoming an MLS implementation), but rev 2 no
    // longer TRUSTS them alone: prev_epoch_authenticator/post_tree_hash chain the
    // declaration to the real tree (see Commit chaining) and authz_head attests
    // which log view the author folded (see Head attestation).

| MlsWelcome { channel_id, generation, commit: EventRef,
               for_member: PublicKey, for_device: DeviceId, welcome: Vec<u8> }
    // Welcome bytes are encrypted to the joiner's KeyPackage init key.
    // for_* fields are UNVERIFIABLE by the fold — which is why leaves are only
    // "present" once the joiner confirms (next variant).

| MlsLeafConfirmed { channel_id, generation, epoch, tree_hash: [u8;32],
                     store_instance_hash: [u8;32] }
    // Authored by the JOINING device after successfully processing its Welcome.
    // The fold promotes its leaf from `pending` to `confirmed` only on this event
    // and only if tree_hash matches the post_tree_hash of the cited epoch's commit
    // — or, for a leaf the reset ITSELF staged, the reset's declared post_tree_hash
    // (see MlsGroupReset; that anchor is scoped to those leaves and to them only).

| AuthzBeacon { channel_id, authz_head: EventHash, epoch: u64 }
    // Sent as a sealed MLS application message (NOT a log event) on a jittered
    // cadence by any online member, so a quiet channel still cross-checks log views
    // inside the group where the host can neither forge nor omit.

| MlsGroupReset { channel_id, new_generation, welcomes: Vec<EventRef>,
                 post_tree_hash: [u8;32] }
    // Recovery hatch, owner-only this rung. Valid ONLY if `welcomes` covers exactly
    // the fold's current members×devices set for the channel — no more, no fewer
    // (see Non-selective reset). Rate-limited by the fold.
    // post_tree_hash: the new generation's real tree hash. The reset generation's
    // add-commit is never a log event, so post-reset MlsLeafConfirmed has no
    // commit to check against; the resetter IS the new group's creator, so its
    // declaration is the anchor (never first-writer-wins by the first confirmer,
    // which one malicious welcomed device could poison for everyone).

| MessagePostedE2ee { channel_id, generation, epoch, ciphertext: Vec<u8>,
                      reply_to: Option<EventRef>, attachments: Vec<AttachmentCap>,
                      authz_head: EventHash }
    // ciphertext = MLS PrivateMessage sealing
    //   { content: String, attachment_keys: [...], filenames: [...], mimes: [...] }
    // padded to the bucket ladder. reply_to + attachment caps stay OUTSIDE the seal:
    // the server must thread replies and validate caps-vs-blobs blind.
    // Size cap: 40 KiB ciphertext (rev 1's 16 KiB was smaller than the 32 KiB
    // worst case of the 8000-char rule it replaces — see Size caps).

| MessageEditedE2ee { channel_id, target: EventRef, generation, epoch,
                      ciphertext: Vec<u8>, authz_head: EventHash }
    // Sealed edit. Without it, editing is a silent regression in E2EE channels
    // and EditMessage{new_content: String} would ship the whole body plaintext.

| MessageDeleted { channel_id, target: EventRef, reason: DeleteReason }
    // Tombstone (product finding F2). delete_message hard-deletes the derived row
    // while reconcile_messages re-derives any log message lacking one at every
    // startup => deletions resurrect. Content-blind delete is the ONLY moderation
    // mechanism in E2EE channels, so the tombstone is load-bearing, not cleanup.
    // Derivation and reconcile both consult tombstones. Clients MUST purge the
    // local store on fold (see Local store obligations).

| DeviceRevoked { device: DeviceId }
    // Authored by the owning identity (or the server owner for abuse); fold marks
    // the cert dead, freezes the chain (existing events stay valid, new ones
    // rejected), and obligates leaf removal via the membership bridge.
```

### Fold state added to `LogState`

All deterministic, all checkpoint-serialized:

```
channels: HashMap<u64, { name, kind, class, parent }>          // from ChannelCreated
mls_groups: HashMap<u64, {
    generation, epoch,
    commit_head: EventHash,
    epoch_authenticator: [u8;32],      // chained; see Commit chaining
    tree_hash: [u8;32],
    leaves_confirmed: HashSet<(PublicKey, DeviceId)>,
    leaves_pending:   HashSet<(PublicKey, DeviceId)>,
    events_since_last_commit: u32,     // freshness ceiling, log-derived
    last_commit_epoch_by_author: HashMap<PublicKey, u64>,  // commit-rate rule
    resets_since: u32,                 // reset rate limit
}>
consumed_key_packages: HashMap<EventRef, u64>   // -> expiry log pos, prunable
live_key_packages: HashMap<(PublicKey, DeviceId), u8>   // cap enforcement
revoked_devices: HashSet<DeviceId>
device_certs: HashMap<DeviceId, { identity, expires_at }>
tombstones: HashSet<EventRef>
```

Derived (not stored) per channel, pure functions of the above:
`pending_removals = leaves_confirmed ∪ leaves_pending \ (members × live_devices)`
`pending_adds     = (members × live_devices) \ (leaves_confirmed ∪ leaves_pending)`

## Head attestation: making "the fold is the authority" true (C2)

Rev 1's ban story was written as though the log were a broadcast medium. It is a request/response API served by the adversary. A host that delivers everything *except* a `MemberBanned` (and that chain's suffix) defeats the entire membership bridge with pure omission: no client folds the ban, so the send-blocker never engages; no steward sees drift, so no Remove commit is built; and rev 1's second mitigation ("ingest rejects pre-rekey epochs once the remove-commit is accepted") is circular, because the rekey never happens. Rung 1's per-chain sequence numbers detect *gaps*, not *truncation*.

Rev 2 corroborates the fold view **inside the group**, where the server can neither forge nor omit:

- Every `MlsCommit`, `MessagePostedE2ee`, and `MessageEditedE2ee` carries `authz_head: EventHash` — the log head the author had folded when authoring.
- Receiving clients compare each `authz_head` against their own folded history. A peer citing a head the client has never seen ⇒ the client fetches it. **If the server cannot produce it, that is provable equivocation** — surfaced hard (same UX posture as Rung 1's genesis-pin-change warning: non-dismissible, channel marked unsafe) and the client **fails closed on sending**.
- `AuthzBeacon` (a sealed application message, jittered, cheap) keeps the cross-check alive in a quiet channel.
- The same mechanism closes the two nastier variants: **epoch partitioning** (delivering commits to a subset, so members sit at different epochs and cannot distinguish censorship from quiet) and **epoch pinning** (bouncing a client's sends with `stale-epoch` forever while feeding it an old commit stream — rev 1's "invisible to the user" retry was perfect cover for the server choosing which epoch a client encrypts to). Rev 2: the retry is **not** invisible — after 3 consecutive `stale-epoch` bounces without a newly-processed commit, the client surfaces "cannot confirm channel state" and stops sending.

## Commit chaining and joiner-confirmed leaves (C3, C5, C7, I3, I8)

Rev 1 classified the declared-vs-actual gap as "DoS-shaped, provable, punishable — not silent compromise." That was inverted. One lying commit — declare `removes: [banned_bob]`, actually self-update — passes the fold (bob is banned, so his removal is authorized), advances the epoch, **removes bob's leaf from the fold's leaf set**, and therefore zeroes the drift the detector reads. No steward ever retries. Bob decrypts forever. The cheaper variant is worse: a Welcome with valid `for_member` but a garbage HPKE recipient consumes the victim's KeyPackage, marks the leaf present, and pins the victim on "waiting for keys" — indistinguishable from ordinary steward lag.

Three rules fix the family:

1. **Epoch-authenticator chaining, enforced by the blind fold.** `MlsCommit` carries `prev_epoch_authenticator` and `post_tree_hash` (RFC 9420 values). A commit is invalid unless its `prev_epoch_authenticator` equals the authenticator the previously accepted commit declared for that `(channel, generation)`. A liar therefore **cannot be built upon**: the next honest commit's chain check fails immediately, deterministically, in the fold — on the server and on every Rung-3 replica — without anyone implementing MLS. Recovery is the reset hatch, and the divergence is provable evidence.
2. **Every member that processes a commit verifies the real tree hash against `post_tree_hash`.** A mismatch is a hard, loud, in-band failure that marks the channel unsafe and blocks sending — not rev 1's "surface 'group needs reset'" hint.
3. **Joiner-confirmed leaves.** A `DeclaredAdd` marks the leaf **pending**, never present. It is promoted to **confirmed** only when the joining device itself authors `MlsLeafConfirmed` with a matching `tree_hash`. Drift detection runs on `leaves_confirmed`, so a bogus Welcome leaves visible drift and gets retried automatically. This single rule also fixes: the ghost-Welcome lockout (C3), the healthy-looking-but-deaf leaf after MLS state loss (I8), and it makes C7's "reset must cover everyone" checkable.

## Freshness: mandatory rekey cadence and the blind ceiling (C4, I1)

Rev 1 had exactly one epoch-transition trigger: membership drift. Consequences: a stable five-friend channel with no churn for three months sits on **one epoch** for three months (seize any member's device and read everything back to the last join; `max_past_epochs = 3` is moot when there have been zero transitions). PCS is worse — steal Alice's MLS store (not her identity key, just the sqlite file) and there is no drift, no steward action, and **the compromise never heals**. Meanwhile the no-escrow decision, which costs real product value, was *paid for* with exactly this property.

Rev 2:

- **Client cadence (the time dimension, client-enforced, stated as such):** each member's client commits a self-Update when `now - last_own_update > T_update` (default **7 days**, jittered ±20%) or after **200** messages sent in the channel, whichever comes first, using the existing steward nomination + epoch CAS so the cadence does not thrash.
- **Fold ceiling (the enforceable dimension, blind-host-enforced, log-derived — no wall clock):** the fold counts `events_since_last_commit` per channel. When it exceeds **C_max = 500**, `MessagePostedE2ee` / `MessageEditedE2ee` become **invalid** — the channel stops accepting new content until somebody rekeys. Forward secrecy stops being a hope and becomes an invariant a server that cannot read a word enforces.
- **Pending-removals gate (I1):** rev 1's "clients that folded the ban stop sending" is voluntary — a patched client, an unpatched build, or a client kept unaware by C2 keeps sending. Rev 2 makes it a protocol invariant: **ingest and the fold reject `MessagePostedE2ee` while `pending_removals` is non-empty.** The channel is sealed-until-rekey, enforced blind.
- **Product cost, stated for the owner:** on a channel where nobody has been online long enough to rekey, sending is blocked with "this channel needs a member to refresh its keys — it will unlock when someone comes online." The strictness numbers are an open product question (availability vs. privacy).
- The Goal section's FS/PCS claim is restated as bounded by the rekey interval.

## Ordering: MLS epochs on a Lamport log — and what happens when commits race

- **At Rung 2 the single host is the sequencer it always was.** `SubmitEvent` ingest is serial check-then-mutate against the live `LogState`. `MlsCommit` is valid iff `generation` matches, `epoch == current_epoch`, the epoch-authenticator chain matches, Adds reference unconsumed/unexpired KeyPackages of current cert-valid non-revoked members, Removes are bridge-authorized, and the commit-rate rule passes. The fold then records `epoch += 1`, `commit_head`, `epoch_authenticator`, `tree_hash`, resets `events_since_last_commit`.
- **Race:** two members commit against epoch n concurrently; the first to reach the server wins, the second fails ingest with `stale-epoch` and resyncs (process winner, rebuild, resubmit) — RFC 9750's strongly-consistent DS model implemented as a compare-and-set.
- **Rung-3 forward-compatibility:** an epoch-stale `MlsCommit` is a **deterministic no-op** in the fold (recorded, ignored, zero state change), so any converged event set folds to the same winner on every node.
- **Drift-priority tiebreak (I2, rev 2).** Rev 1 broke same-epoch ties by canonical order `(lamport, author, event_hash)`. Round-1 of the Rung-1 red team already established that `event_hash` is **grindable** and `lamport` self-asserted — so a member who wants a Remove never to land pre-mines a competing same-epoch self-update that sorts first, every epoch, deterministically on every replica, for a few million hashes. Rev 2 orders same-epoch candidates by: **(1) whether the commit discharges an outstanding fold obligation (`pending_removals` first, then `pending_adds`), then (2) canonical order.** Still pure, still checkpoint-composable, and the drift-correcting commit becomes unblockable.
- **Commit-rate rule (I3):** a commit by author A in channel C is invalid unless it discharges drift **or** A's previous commit in C is ≥ **K = 4** epochs back. Without it, one member spams self-updates, every other member's in-flight sealed message bounces `stale-epoch`, and with `max_past_epochs = 3` honest in-flight messages become permanently undecryptable — a cheap channel-wide DoS.
- **Commit skipping is impossible** (MLS property): a returning member processes every accepted commit in order. **Therefore `MlsCommit` events are declared permanently retention-exempt and checkpoint-mandatory this rung** (I5a): the commit stream is not prunable, and pretending otherwise would silently choose unbounded per-channel growth. **External commits** are the load-bearing mechanism that makes pruning safe at Rung 3/4 — not "a future optimization" — and the ratchet-tree extension enabled here is their prerequisite. Named as a Rung-3 prerequisite, not built now.
- Application-message tolerance: `SenderRatchetConfiguration` defaults plus `max_past_epochs = 3`. Ingest only accepts new application messages citing the current epoch.

## The membership bridge: the authz fold drives MLS

**The authz fold is the authority; the MLS group is a downstream projection of it** — and rev 2 gives the projection three properties rev 1 assumed: delivery attestation (head attestation), correctness (commit chaining), and freshness (rekey ceiling).

- **Join:** `MemberJoined` (+ `MemberApproved` where required) makes the identity a member. Its devices publish KeyPackages. A **steward** commits Adds + emits Welcomes per E2EE channel; the joiner authors `MlsLeafConfirmed`. Until then the member is a member-without-keys: sees the channel, sees ciphertext arrive, decrypts nothing. UI is proactively honest: "keys arrive when another member is online — this can take a while."
- **Leave / kick / ban / device revocation / cert expiry:** the fold's `pending_removals` becomes non-empty, **sends are blocked channel-wide**, and a steward commits the Remove, rotating the epoch.
- **Steward = any current member's client, deterministically nominated, racing safely.** No new role: every online client watching the log compares `members × live_devices` against `leaves_confirmed`; the client whose `(identity, device)` hashes lowest among online confirmed-leaf holders acts first, others after a short timeout. Races are safe (epoch CAS + drift-priority tiebreak). Fold backstops: a commit is invalid if it Adds a non-member/banned/revoked/expired leaf, or Removes a leaf whose member is in good standing (except self-removal of one's own device).
- **The desync window, stated honestly:** between a ban landing and the remove-commit landing, the banned member still holds current-epoch keys. Rev 2 mitigations, in order of force: (1) **fold/ingest refuse new sealed content while `pending_removals` is non-empty** (protocol invariant, not client courtesy); (2) head attestation makes a withheld ban detectable in-band; (3) the window is typically seconds while any member is online. What we do NOT claim: retroactive protection.
- **Total steward loss:** the group cannot rekey or welcome; recovery is `MlsGroupReset` (below), and **prior history is undecryptable for everyone who lost state** — E2EE working as designed.
- **Poisoned commit:** the epoch-authenticator chain makes it a dead end automatically (the next honest commit cannot chain onto it), the tree-hash check makes it loud, and the signed event is permanent evidence. Recovery is reset.

### Non-selective reset (C7)

Rev 1's Risks said a malicious `manage_channels` holder "can nuke a group's continuity (not its confidentiality)." Wrong: nothing required the new generation to Welcome everyone. A reset plus selective Welcomes is an **unbounded, unlogged eviction** from a private channel while the fold still says every excluded member is in good standing — and the excluded see "waiting for keys," a normal state, so the UI camouflages the attack; and no other steward can help them, because a steward must be inside the group.

Rev 2:

- `MlsGroupReset { channel_id, new_generation, welcomes }` is valid **only if `welcomes` covers exactly the fold's current `members × live_devices` set** for that channel — no more, no fewer. Enforced by the blind fold.
- The fold refuses `MessagePostedE2ee` in the new generation until every leaf the reset staged is accounted for — confirmed, or **void** because the fold no longer owes its holder a leaf at all (banned, kicked, device revoked, cert expired), which a Remove-commit then clears (the bridge's answer when a welcomed device is banned or lost before confirming). A Remove-commit dropping a staged leaf whose holder is still in **good standing** accounts for nothing: dropping a pending-only leaf is open to any member, so treating it as a discharge would let the first welcomed device to confirm evict its co-staged peers and reopen the channel around them — the very partition this rule exists to prevent. Post-reset confirmations are validated against the `post_tree_hash` the **resetter** declared, and only for the leaves that reset itself staged (the declaration never expires, so unscoped it would let any later ordinary join confirm against a stale public hash). A partial reset is therefore a **dead channel, loudly**, not a silent partition.
- Reset is **owner-only this rung** (see M3 disposition), fold-rate-limited (one per channel per 1000 channel events), and gated behind a UI confirmation wall that names what is lost.
- Second-order honesty: reset is asymmetric evidence destruction — members with a local store keep plaintext; reinstallers and later investigators do not.

## Content containment: one write choke point and fail-closed rendering (C8, F1)

This is the finding that would have shipped a lock icon over plaintext. Rev 1 enforced class at **`SubmitEvent` ingest only**, but most content in the shipped product never becomes a log event:

- legacy `SendMessage` → `messages::insert_message` (`handlers.rs:484`), still reachable from `tauri-bridge.ts:175` → `commands.rs:1150`;
- slash-command replies (`connection.rs:1223`), incoming webhooks (`webhooks.rs:185`), poll create (`polls.rs:326`), giveaway create/sweeper (`giveaways.rs:398`, `:289`) — all call `insert_message*` directly, **unsigned, straight into the derived view**;
- sticker/GIF/voice sends always use `api.sendMessage`;
- and the client **actively falls back to the legacy plaintext path** for replies, URL-image auto-fetch, and inline `:emoji:` attachments (`MessageInput.tsx:277-296`), while `api.fetchUrl` makes the server fetch and store the image in plaintext.

Rev 1's stated defense — "the config UI refuses attaching a webhook to an E2EE channel" — is UI-level enforcement of a confidentiality boundary. Worse, (4) inverts into spoofing: **a malicious host can inject unsigned plaintext rows into an E2EE channel and every client renders them as legitimate**, with the lock icon certifying host-authored text. This is precisely CLAUDE.md's named killer seam.

Rev 2 requirements (owned by sub-project 3 for the server half, 4 for the client half):

1. **One server-side choke point.** `messages::insert_message` / `insert_message_with_author_name` take the channel class and **hard-error for any E2EE channel unless the write is derived from a verified `MessagePostedE2ee` / `MessageEditedE2ee`**. Not the config UI. Not the handler. The function every path funnels through.
2. **Class-aware rejection at the request layer** for `SendMessage`, `EditMessage`, `AddReaction`/`RemoveReaction`, `RunCommand`, webhook-create, poll/giveaway create, `FetchUrl`, thread create — each returns a clear "not available in encrypted channels" error.
3. **Fail-closed rendering (the client rule that neutralizes the whole class).** In an E2EE channel the client renders **only** rows it decrypted from a signature-verified `MessagePostedE2ee`. Everything else is dropped and counted, with a visible **"N messages could not be verified"** marker. A host-injected row is invisible-but-flagged, never rendered as content.
4. **No client fallback, ever.** The E2EE send branch has no legacy path: replies go over the log (see reply mapping), URL auto-fetch and inline-emoji auto-attach are disabled in E2EE channels, sticker/GIF are disabled this rung, voice messages ride the sealed attachment path.
5. **One observation test per content-producing path** (send, edit, reply, reaction attempt, sticker/GIF/voice, slash command, webhook, poll, giveaway, sweeper, translation, embed, bot post, outbox retry): drive the real path against an E2EE channel, capture stored bytes, assert no plaintext substring and no unverified row rendered.

## Protocol compatibility and rollout (F3, M2)

Rev 1 claimed "rmp_serde enum encoding tolerates trailing additions." That is only true for **new readers of old data**. Measured against our codec (`farder-protocol/src/codec.rs`, rmp_serde 1.x):

- old reader + struct with one added trailing field → `Err(LengthMismatch)`
- old reader + new enum variant → `Err(Syntax("unknown variant"))`
- old reader + old variant from new code → Ok

The owner runs a perpetually mixed fleet (multi-machine, known stale-build lag). Naively adding `is_e2ee` to `ChannelInfo`, a marker to `MessageInfo`, or new `ServerEvent`s would make un-updated clients fail to decode frames **including in plaintext channels**. Rules, now normative:

- **Never add or reorder fields on an existing struct or variant that an old client receives.** New data rides in **new** request/response/event variants that only new clients ask for. E2EE metadata reaches the client through a new `ChannelInfoV2` / `MessageInfoV2` fetch surface, not by mutating the shipped ones.
- **Explicit version gate, fail closed.** Handshake carries a protocol version; a server with E2EE channels tells an old client "this server requires a newer client" for those channels rather than shipping it frames it cannot parse. An old client never silently skips an event it cannot decode (skipping would diverge its fold and its Lamport clock).
- **Defined old-client behavior in an E2EE channel:** the channel is listed but not enterable, with upgrade copy. It must never render ciphertext rows as hex garbage next to a working plaintext composer.
- **Upgrade order stated:** server first, then clients. The server accepts old clients (degraded, no E2EE); a new client against an old server sees no E2EE channels.

## History for late joiners: the decision, honestly re-argued

**Decision: no history escrow.** A new member, a reset-recovered group, or a from-scratch device sees no pre-join history in E2EE channels. The UI says so at the join boundary.

Rev 1's justification overreached, and rev 2 states the honest delta (I4). The argument against a long-lived escrowed "history key" stands: it is exactly the mechanism RFC 9750 warns "may reduce the FS and PCS guarantees provided by MLS," it makes every member's device a full-archive decryption oracle, and it poisons Rung 3. But the design's own **local decrypted store** means a member present since channel creation already holds the full archive in plaintext on disk. So:

- **What no-escrow actually buys:** *new* members cannot read old messages, and a seized device yields only that member's tenure — not the channel's whole life. That is worth having. It is not "the archive doesn't exist."
- **Local decrypted store** (new client-side SQLite per E2EE channel) is required, because MLS deletes decryption keys aggressively — senders cannot re-read their own messages from server ciphertext outside `max_past_epochs`. `FetchHistory` still returns ciphertext rows; the client renders old history from its local store.
- **At-rest wrapping is IN SCOPE this rung** (promoted from rev 1's fast-follow): the local store is encrypted under the PIN-derived key, like `identity.key`. It is the difference between "a seized laptop reveals the archive" and "reveals nothing," and Farder users already expect PIN protection of local secrets. Friction cost is an open product question.
- **Local store obligations (compliant-client rule, stated as unenforceable against a malicious member):** folding `MessageDeleted`, `AttachmentRedacted`, retention expiry, or the anonymize-on-leave flow **MUST** purge the corresponding rows from the local store. This is exactly parallel to Rung 1's compliant-host redaction scoping. Coexistence row 7b is corrected accordingly: server-side those mechanisms work on ciphertext; *end to end* they are compliant-client obligations.
- The member-to-member "share my history with a newcomer" feature remains an app-layer social action with a visible actor, deferred, and an open product question.

## Attachments in E2EE channels

- Sender generates a random 32-byte per-file key, seals bytes with the existing `farder-crypto` AES-256-GCM primitive, uploads the **ciphertext**. The `AttachmentCap` references the ciphertext (`content_hash = SHA-256(ciphertext)`, ciphertext `size`, `declared_type = "application/octet-stream"`). Per-file key + real filename + real MIME travel **inside** the message ciphertext.
- Server-side cap-vs-blob validation (hash/size/uploader) works unchanged on ciphertext. Redaction and GC/retention work unchanged.
- **The full scope of what E2EE voids (I6, corrected):** not just `validate_image`. The **entire** server-side file-hardening track — magic-byte sniffing, content-type allowlist, download-filename sanitization — is inoperative for E2EE-channel blobs. E2EE channels are therefore the bypass around every server-side content control the project builds.
- **New client-side hazard, and the requirement that answers it:** the real filename now travels inside the ciphertext, is fully attacker-controlled, and no server sanitizer ever sees it. Before **any** disk write or render, the client MUST: take the basename only, reject path separators and traversal, strip bidi/RTL-override and control characters, enforce the extension allowlist, and sniff magic bytes against the same allowlist version the server would have applied. Tested as a hostile-input case (`../../.ssh/authorized_keys`, `invoice.pdf.exe`, RTL tricks), not a happy path.
- Privacy note: random per-file keys make ciphertext hashes uncorrelatable across uploads/servers — strictly better than Rung 1 for the existence-oracle concern. Attachment count and (bucketed) sizes still leak.
- Attachment readability inherits message readability exactly.

## Multi-device, device loss, and device transparency (C5, I8)

Rev 1 said identity-key compromise was "unchanged from Rung 1." It is not: Rung 1's consequence was noisy, attributable impersonation; Rung 2's is a leaf that **only reads**, in every E2EE channel, forever, with no victim notification, no leaf-change visibility, and no device cap. Rev 2 closes the consequence without solving identity rotation:

1. **Self-add rule.** If identity X already has ≥1 **confirmed** leaf in a group, an `MlsCommit` adding another device of X is valid **only if authored by an existing device of X**. Only a *first* (or all-leaves-lost) device may be steward-added. A stolen identity key alone therefore cannot obtain read access while any real device of the victim is alive — the compromise becomes an event the victim's own client must participate in and can refuse.
2. **Device-list transparency in the UI.** A leaf-set change in an E2EE channel renders as an in-channel system notice — *"a new device of Alice can now read #private"* — non-dismissible, same posture as the genesis-pin warning. Plus a per-identity device count in the member list.
3. **Fold-enforced cap of 8 live devices per identity** — bounds blast radius and Welcome cost.
4. **`DeviceCert` expiry** (M5) is checked by the fold; expired certs' leaves become `pending_removals`.

Flows:

- **Adding a device**: new device generates its subkey; the identity signs a `DeviceCert`; the device emits `DeviceAuthorized` + `MlsKeyPackagePublished`; **an existing device of the same identity** commits the Add and emits Welcomes; the new device confirms its leaf. No prior history (same FS rule); own-device local-store sync is an app-layer follow-on.
- **Retiring a device**: `DeviceRevoked` → `pending_removals` → steward Remove → epoch rotates.
- **Device loss**: identity survives (24-word recovery); the lost device's subkey and MLS state do not. `DeviceRevoked` (identity-authored) → `DeviceAuthorized` + KeyPackages for the new device → steward removes old leaves and adds the new one. **Channel history is gone for that user** unless they had a second device or a local-store backup — stated plainly in recovery UI.
- **MLS state lost but device alive (I8)**: distinct from device loss and previously unhandled — the device kept signing valid events while unable to decrypt, every steward saw a healthy leaf, and senders saw success while the receiver got nothing, with no error surface anywhere. Rev 2: joiner-confirmed leaves make it visible as drift, and the client, on detecting a missing/unopenable/instance-mismatched MLS store, **self-revokes and re-provisions** with the same "history for that device is gone" copy.
- **Identity-key rotation** remains out of scope (Rung-1 gap), now with its Rung-2 consequence contained.

## Metadata: what leaks, stated fully (I7)

- **Length:** partially fixed — bucket padding ladder (256 B / 1 KiB / 4 KiB / 16 KiB / 40 KiB) is a default, not a deferral. Bucket boundaries still leak coarse size.
- **Mentions / notifications:** rev 1 left this unspecified, which leaks either way — the server must know whom to notify, so either the client ships a plaintext mention list (a **content-derived** leak: exactly who was named in each sealed message) or mentions simply do not notify in E2EE channels (a feature loss). This is a genuine product trade and is an **open product question**; the design default until answered is **no server-side mention routing in E2EE channels** (fail closed: no content-derived data leaves).
- **Push notifications (F13, corrected):** today's push is content-free (`NotifyPending { count }`, `farder-notify/src/push.rs:28`) — no content, no channel name. Rev 1's row claimed previews "become generic," inventing a richer present and *adding* channel-name exposure. Nothing changes; the notify service keeps seeing a count.
- **Per-channel rosters are public server-wide.** `MlsWelcome { channel_id, for_member, for_device }` and `MlsCommit.removes` broadcast who can read which channel. Harmless this rung (all members are in all groups) but it **directly defeats the channel-ACL follow-on that per-channel groups were justified by**. Recorded as a Rung-3/ACL prerequisite: either scope the Welcome/commit fetch surface, or accept and document that channel rosters are never private.
- **The commit/Welcome stream is a high-resolution social feed** — joins, leaves, device additions, device losses, per channel, timestamped, in the clear. Sharper than "membership is visible."
- **Link embeds (F7):** in E2EE channels embeds are **click-to-load**. `useLinkEmbed` currently auto-fetches on render via the relay, and URLs are content — auto-fetch would send every pasted URL in a sealed channel to the design's own named metadata chokepoint. The "Load preview" chip already exists; one boolean plumb.

## Size caps (M4, F8)

Specified here because ingest enforces them blind:

| Variant | Cap | Rationale |
|---|---|---|
| `MessagePostedE2ee` ciphertext | **40 KiB** | 8000 chars is up to 32 KiB of UTF-8 (CJK/emoji) + in-band keys/filenames/MIME + MLS framing + padding. Rev 1's 16 KiB would hard-bounce legal messages. Client rule becomes byte-based: ≤ 8000 chars **and** ≤ 32 KiB pre-seal. |
| `MessageEditedE2ee` ciphertext | 40 KiB | same |
| `MlsCommit.mls_message` | 256 KiB | multi-KeyPackage commits |
| `MlsWelcome.welcome` | 256 KiB | O(group size) with ratchet-tree extension; at the cap, out-of-log tree serving is the escape hatch |
| `MlsKeyPackagePublished.key_package` | 8 KiB | |
| live KeyPackages per device | 10 | |
| live devices per identity | 8 | |

A malicious client can use the full 40 KiB rather than 8000 chars — harmless, and stated.

## What the server (and a future mesh host) can still validate blind

- Event signatures, DeviceCert chains (now incl. expiry), `(author, device)` seq/prev continuity, Lamport monotonicity, server-id binding.
- The full authz fold: membership, bans, approvals, invites, permission grants — plaintext by constraint.
- MLS control-plane: generation/epoch CAS, **epoch-authenticator chaining**, declared adds against membership + unconsumed/unexpired KeyPackages, declared removes against bridge rules, one winner per epoch with **drift-priority tiebreak**, commit-rate rule, device/KeyPackage caps, **reset completeness**, `pending_removals` send gate, **rekey staleness ceiling**.
- Attachment cap-vs-blob (hash/size/uploader), redaction, retention GC, anonymization, **tombstones**.
- Rate limits, per-variant size caps, channel-class rules — and, new in rev 2, **the `insert_message*` choke point** so non-log write paths cannot reach an E2EE channel.

**Content-blind moderation:** moderators keep takedown *mechanics* — delete/redact on event-hash and content-hash, never on content — and rev 2 makes deletion durable via the `MessageDeleted` tombstone (rev 1's delete resurrected on the next server restart, which is fatal when blind delete is the only moderation tool). What mods lose is *reading* what they remove; moderation in E2EE channels is report-driven. A structured report flow is an open product question.

## Coexistence: fate of every plaintext-touching feature

Verdict classes: **works-on-ciphertext** | **server-features-channel-only** (plaintext channels only; refused server-side in E2EE channels) | **client-side-redesign** (in scope) | **deferred**. No feature gets the "server holds a group key" treatment — banned by constraint. Every "refused" verdict below names a **server-side** enforcement point, never a UI one.

| # | Feature | Verdict | Notes |
|---|---------|---------|-------|
| 1 | Ticker / custom-monitor bots | **works unchanged** | Presence-only; never touches channel content. |
| 2 | Bot alert DMs | **works unchanged** | Corrected rationale (F12): these are **server-encrypted, not E2E** — `bots.rs:474-512` holds bot secret keys and seals server-side. Server-managed bots are server-trusted by definition. |
| 3 | Incoming webhooks | **server-features-channel-only** | External sender has no group key. Enforced at webhook-create **and** at the `insert_message*` choke point. |
| 4 | Slash commands (text/api) | **server-features-channel-only** | `RunCommand` rejected server-side for E2EE channels. Client-composed/relay-fetched replies: deferred. |
| 5 | Polls + giveaways (incl. sweeper announcement) | **server-features-channel-only** | Create rejected server-side; sweeper cannot write into E2EE channels (choke point). Encrypted widgets + member-run draw: deferred (redesign, not a port). |
| 6a | Link embeds (relay `ProxyLinkEmbed`) | **client-side-redesign (in scope)** | Corrected (F7): auto-fetch leaks URLs (= content) to the relay. **Click-to-load in E2EE channels.** |
| 6b | `FetchUrl` auto-attach | **server-features-channel-only** | Server-fetched blob is server-visible. Rejected server-side; client no longer falls back to it. |
| 7a | Search (server FTS) | **client-side-redesign (in scope)** | E2EE channels never enter `messages_fts`; client searches its local (now PIN-wrapped) store. |
| 7b | History pagination / retention GC / anonymize / mod delete / attachment redaction | **works-on-ciphertext server-side; compliant-client obligation end-to-end** | Corrected (F2, I4): needs the `MessageDeleted` tombstone (or deletions would resurrect on restart via `reconcile_messages`), and local-store purge is a client obligation unenforceable against a malicious member. |
| 8 | Push / notify | **works unchanged** | Corrected (F13): already content-free (`NotifyPending { count }`). Nothing to degrade. |
| 8b | Mentions (who gets pinged) | **open product question; default = no mention routing in E2EE channels** | Either a plaintext mention list (content-derived leak) or no pings. Fail closed until decided. |
| 9a | Attachment cap validation | **works-on-ciphertext** | Pure metadata comparison. |
| 9b | Server file policy: image validation, magic-byte sniffing, type allowlist, download-filename sanitization | **skipped for E2EE blobs; replaced by mandatory client-side policy** | Corrected scope (I6). Client sanitizes the in-ciphertext filename and sniffs magic bytes before any write/render. |
| 10 | **Message edits** | **client-side-redesign (in scope)** | New (F6). `EditMessage { new_content: String }` (`protocol/server.rs:334`) would ship the whole body plaintext. Rejected server-side in E2EE channels; sealed `MessageEditedE2ee` instead. |
| 11 | **Reactions** | **refused in E2EE channels this rung; open product question** | New (F6/C8). `AddReaction{emoji}` + `ReactionAdded{message_id, emoji, public_key}` would tell the host exactly who reacted with what to which sealed message. Rejected server-side; sealed reactions are a possible later build. |
| 12 | **Threads** | **refused in E2EE channels this rung** | New (F6). A thread off a sealed parent is created via the legacy path = plaintext thread under a sealed message. Class inheritance rule specified (`ChannelCreated.parent` ⇒ child inherits parent class); E2EE thread groups deferred. |
| 13 | **Pins** | **works-on-ciphertext** | Id-keyed. Pin *preview* surfaces must render via the fail-closed decrypt path, never raw rows. |
| 14 | **Shareable widget links / active-widgets bar** (shipped this week) | **works, with stated correlation caveat** | New (F6). Client-side detection on decrypted content; `GetPoll` is id-keyed. Caveat: interacting with a linked widget seconds after a sealed message arrives gives the host a timing correlation. |
| 15 | **Stickers / GIFs** | **deferred (refused in E2EE channels)** | New (F1). Both always used `api.sendMessage`; GIFs are remote URLs needing relay-proxied client fetch + sealed re-upload. |
| 16 | **Voice messages** | **works via sealed attachment path (in scope, sub-6)** | New (F1). It is a file; it rides the per-file-key path. |
| 17 | **Replies** | **prerequisite work, in scope (sub-3 + sub-4)** | New (F9). The shipped log path drops replies (`MessageInput.tsx:283` TODO — legacy `replyTo` is a numeric id). E2EE channels are log-only, so the event-hash ↔ message-id mapping is a hard prerequisite, not an assumption. |
| 18 | **Legacy `SendMessage` / server-authored `insert_message*` paths** | **hard-gated (in scope, sub-3)** | New (C8/F1). The choke point + fail-closed rendering. |
| — | 8000-char content check | **replaced** | Server enforces 40 KiB ciphertext; client enforces 8000 chars **and** ≤32 KiB pre-seal. |

## Existing servers and existing members (F4)

Genesis is created exactly once, at first-owner-claim (`connection.rs:587-610`, gated on `setup_token_used || auto_claimed`). A server whose owner was established before Rung 1 has **no genesis**, `log_state` is `None`, and `SubmitEvent` is rejected outright (`handlers.rs:1952`). Since every Rung-2 mechanism is a log event, **E2EE is structurally unavailable on every pre-Rung-1 server** unless something changes. Second layer: members who joined via the legacy path are absent from `LogState.members`, so on log servers the owner's *longest-standing* members fail the MLS Add rule and can never receive keys.

**DECIDED 2026-07-28 (owner): option (b) — fresh servers only.** Rationale: the platform currently has no users beyond the owner's own testing servers, so migration machinery is not worth a sub-project; existing servers are simply recreated if a private channel is wanted. Conditional sub-project 8 is **dropped**. Ingest and the fold's bootstrap rules assume a post-Rung-1 server (genesis present) with no backfill path; `SubmitEvent`'s existing "no genesis" rejection stands as the only answer for pre-Rung-1 servers. The two shapes considered, for the record:

- **(a) Migration flow** — an owner-triggered "establish the log on this server" action (genesis + owner-signed `MemberJoined`-equivalents backfilling existing members + `ChannelCreated` backfill for existing channels, all plaintext-class). NOT built; may be revived if real communities exist before Rung 2 ships.
- **(b) Fresh servers only** — E2EE requires a post-Rung-1 server; existing communities must be recreated to use private channels. **← chosen.**

## Client changes (`client/src-tauri` + frontend)

- New `farder-mls` wrapper crate consumed here; MLS state in `openmls_sqlite_storage` per `(identity, server)` in the **non-portable** subdirectory, with instance binding and no-resume.
- **Steward, cadence, and MLS logic live in the client *crate* as plain library code** driven by a headless harness (see sub-4) — not in the Tauri command layer — so it is testable in WSL where the GUI cannot run.
- Channel-creation UI: class choice with an honest explainer; **plaintext channels get their own one-time notice** ("bots and the server can read this channel").
- Channel header: lock icon and class copy; **E2EE composer gets a distinct affordance** (placeholder "Encrypted message…" + a border color from `var(--xp-…)` in **every** theme per CLAUDE.md) to cut mis-post risk (F16).
- States: "waiting for keys (arrives when a member is online)", "no history before you joined", "N messages could not be verified", "channel needs a key refresh", "channel state could not be confirmed" (equivocation), "a new device of X can now read this channel".
- Send path: E2EE channels route through pad → encrypt → `SubmitEvent`, with **no legacy fallback**; `stale-epoch` triggers process-pending-commits → re-encrypt → retry, and after 3 unproductive retries surfaces the equivocation warning instead of looping silently.
- Receive path: verify signature → process commits in order (checking `post_tree_hash`) → decrypt → **fail-closed render** → feed the local store; compare `authz_head`s and fetch unknown heads.
- Local store: PIN-wrapped SQLite; purge on tombstone/redaction/retention/anonymize; client-side search index.
- Steward loop: drift on `leaves_confirmed`; KeyPackage pool top-up with lifetimes; self-Update cadence; `MlsLeafConfirmed` on join; runs even when the app is backgrounded (F10).
- Attachment path: seal/unseal, filename sanitization + magic-byte sniffing before write/render.
- Frontend seam discipline per CLAUDE.md: every new `invoke("...")` matched to a registered `#[tauri::command]`; docs updated in the same commits.

## Server changes (`farder-server`, `farder-protocol`)

- Ingest: accept the new payload variants through `SubmitEvent`; the fold (in `farder-crypto`) carries all new validation; ingest adds per-variant size caps, the `messages`-table emptiness check for `ChannelCreated`, and `stale-epoch` as a distinct error code.
- **Choke point:** `insert_message*` takes channel class and refuses E2EE writes not derived from a verified sealed event; `SendMessage`/`EditMessage`/reactions/`RunCommand`/webhook/poll/giveaway/`FetchUrl`/thread-create all rejected for E2EE channels at the request layer.
- Derivation: `MessagePostedE2ee` derives a `messages` row with an `is_e2ee` marker and opaque ciphertext, **skipping FTS**; `reply_to: EventRef` → derived-row id mapping (F9); `MessageEditedE2ee` updates in place; `MessageDeleted` writes a tombstone that **`derive_message_row` and `reconcile_messages` both consult** (F2).
- New fetch surfaces (**new variants, not mutated ones** — see Protocol compatibility): filtered fetch for `MlsWelcome{for_member,for_device}`, unconsumed KeyPackages per identity, `ChannelInfoV2`/`MessageInfoV2` with class/e2ee markers.
- Nothing in the server links OpenMLS. The server's MLS knowledge is opaque bytes plus fold-validated declared fields.

## Sub-projects (each independently shippable + testable)

**Protocol-churn discipline (F15):** all new `EventPayload` variants, protocol request/response/event variants, and fetch surfaces land in **sub-2 and sub-3, dormant until used**, so sub-4/5/6/7 are behavior-only inside already-shipped types — one protocol upheaval (workspace build + separate client-crate build + WebView2 hard-reload) instead of five.

1. **`farder-mls` core — pure Rust, no runtime.** Wraps openmls 0.8.1: create/join(Welcome)/add/remove/self-update, seal/open application messages, **padding ladder**, device-subkey signer adapter, length-prefixed credential encoding, exposure of `epoch_authenticator`/`tree_hash` for chaining, validation helpers members run against fold state, sqlite storage wiring with **`store_instance_id` + no-resume**, envelope (de)serialization. Tests: three-device in-memory groups; add/remove/rekey; **FS observation test** (removed member cannot decrypt post-removal; joiner cannot decrypt pre-join); declared-vs-actual and tree-hash mismatch detection; padding buckets; wrong-suite/wrong-key failures; store-instance mismatch refuses to resume.
2. **Log schema + fold: the full MLS control plane (all variants, dormant-capable).** In `farder-crypto`: `ChannelCreated`, `MlsKeyPackagePublished` (lifetime/cap), `MlsCommit` (chaining + `authz_head`), `MlsWelcome`, `MlsLeafConfirmed`, `MlsGroupReset` (completeness), `MessagePostedE2ee`, `MessageEditedE2ee`, `MessageDeleted`, `DeviceRevoked`, `DeviceCert.expires_at`; fold extensions: class gating (fail-closed), epoch CAS + authenticator chaining, pending/confirmed leaves, `pending_removals` send gate, staleness ceiling, commit-rate rule, drift-priority tiebreak, device/KeyPackage caps, tombstones, stale-commit-as-no-op. Tests: full authz matrix; **commit-race determinism** (replay == stepwise == from-checkpoint); grind-resistance of the tiebreak (a pre-mined competing commit does **not** beat a drift-discharging one); chaining rejects build-on-a-liar; ban → gate → rekey; reset-completeness; extended `replay_equals_stepwise_and_composes_from_a_checkpoint` over all new state.
3. **Server ingest + DS duties + legacy-path lockdown.** Accept/validate/store/broadcast the new variants; per-variant size caps; class enforcement at ingest; **the `insert_message*` choke point + request-layer refusals for every non-log write path**; tombstone-aware derive/reconcile; FTS skip + `is_e2ee`; reply event-hash ↔ id mapping; Welcome/KeyPackage/V2 fetch surfaces; `stale-epoch` error; protocol version gate. Tests: validation matrix (plaintext-in-E2EE, stale epoch, consumed/expired KeyPackage, non-member Add, good-standing Remove, incomplete reset, exceeded caps, pending-removals gate, staleness ceiling); **delete survives restart/reconcile**; derived-view rebuild parity; retention/redaction on ciphertext; **one observation test per content-producing path** asserting no plaintext reaches an E2EE channel.
4. **Client E2EE vertical + headless two-client harness (named deliverable, F14).** Class-aware creation UI → KeyPackage publication → steward add/Welcome → `MlsLeafConfirmed` → sealed send → decrypt → **fail-closed render** → local store + client search → stale-epoch resync with equivocation surfacing → lock/pending/no-history/unverified UI states, styled per theme rules. The harness drives two protocol clients in one process against a test server (`tests/e2e_server.rs` pattern) so sub-4/5 are verifiable in WSL; the owner's manual Windows run is reduced to one final smoke. **Observation tests:** real send path's wire bytes and stored row are ciphertext with no plaintext substring; second client decrypts; non-member/banned client cannot; a host-injected plaintext row is **not rendered**.
5. **Membership lifecycle, rekey cadence, multi-device, transparency.** Steward drift loop on confirmed leaves with nomination + races; self-Update cadence + ceiling behavior; `DeviceRevoked`; self-add rule; device cap; device-loss and **MLS-state-loss-without-device-loss** recovery; leaf-change notices; `MlsGroupReset` + completeness + confirmation wall. Tests: ban → send gate engages → rekey → captured old state cannot decrypt new traffic (observation); ghost-Welcome leaves visible drift and self-heals; stale channel blocks sends then unblocks after rekey; device-loss rejoin; partial reset is refused.
6. **E2EE attachments + client-side file policy.** Per-file-key seal/unseal with keys in-band; octet-stream caps on ciphertext; voice messages on the sealed path; **filename sanitization + magic-byte sniffing before any write/render**; redaction/GC verified on cipherblobs. Tests: cap-vs-cipherblob; round-trip; non-member fetch denied; redaction deletes bytes; hostile filenames (traversal, double extension, bidi) neutralized.
7. **Local history store hardening.** PIN-wrapped at-rest local store; purge-on-tombstone/redaction/retention/anonymize; client search over the wrapped store; own-device store export/import decision surface. Tests: locked store yields no plaintext on disk (observation); tombstone purges local rows; retention expiry purges.
8. ~~(Conditional on Q8) Existing-server migration.~~ **DROPPED per Q8 answer (b) — fresh servers only.**

Order: 1 → 2 → 3 → 4 strictly; then 5, 6, 7 in any order (7 may precede 5 if the owner prioritizes at-rest protection). Each lands with its docs per the documentation-discipline checklist.

## Risks (honest)

- **OpenMLS pre-1.0 churn.** 0.6→0.7→0.8 each broke APIs. Contained by the `farder-mls` wrapper and pinning 0.8.1. MSRV 1.91 to confirm at plan time.
- **FS/PCS are bounded by the rekey interval, not absolute.** With cadence + ceiling this is an invariant rather than a hope, but the bound is real and must appear in product copy.
- **The ban-to-rekey window.** Now sealed (sends blocked) rather than merely discouraged, at the cost of availability: a channel with no online member cannot accept messages after a ban until someone rekeys. Retroactive protection is still impossible.
- **Steward liveness.** Joins wait for an online key-holding member; a fully idle channel welcomes nobody, and **the owner's always-on sidecar cannot help, by design**. External commits are the eventual fix.
- **Declared-metadata trust gap is narrowed, not eliminated.** Chaining makes a liar a dead end and a loud one; it does not prevent the first lie, which still costs a reset.
- **Local plaintext archive.** Even PIN-wrapped, an unlocked device holds the member's full tenure of history. No-escrow buys "new members can't read old messages," not "the archive doesn't exist."
- **Device-subkey reuse as MLS leaf key.** Judged safe (disjoint signed-byte domains, MLS labels); a future audit should examine it; the third-subkey alternative is contained inside `farder-mls`.
- **Metadata.** Padding helps length; membership, timing, reply edges, sender identities, and **per-channel rosters** remain visible to host + relay. Roster visibility must be resolved before channel-ACLs ship.
- **Commit stream is permanently non-prunable this rung** (retention-exempt, checkpoint-mandatory). Per-channel storage grows with churn on member devices at Rung 3 until external commits land.
- **DM crypto now lags channel crypto.** After this rung, E2EE channels have bounded FS/PCS; DMs still use static-static ECDH with no ratchet.
- **Group reset is a big hammer** — now non-selective and owner-only, but still destroys continuity and destroys evidence asymmetrically.
- **Identity-key compromise/rotation** remains unsolved; its Rung-2 read-access consequence is contained by the self-add rule, cap, and transparency notices, not eliminated (a compromise while *all* the victim's devices are dead still yields a first-device add).
- **Mixed-fleet risk.** The compat rules only work if they are followed on every future protocol change; one added field to a shipped struct breaks un-updated clients in plaintext channels too.

## Open product questions (owner decides — plain language)

1. **What should "create channel" default to?** Plaintext (all features: bots, polls, webhooks, server search) with "Private (encrypted)" as an explicit opt-in — or ask every time? *Recommended default: default to plaintext, with a clearly-labelled "Private (end-to-end encrypted)" option that lists the trade-offs in one line.*
2. **New members see nothing from before they joined, forever.** Is that the product you want for private channels, or should the optional "a member can hand their copy of the history to a newcomer" feature be prioritized? *Recommended default: ship no-history now; build member-to-member history sharing as the first fast-follow, because your servers are small and quiet and an empty private channel is a bad first impression.*
3. **Moderating messages you cannot read.** In private channels a moderator deletes a message by pointing at it, based on someone's report, without reading it. Do you want a built-in "report this message" button that attaches the reporter's decrypted copy as evidence, or is "screenshot and tell a mod" fine at launch? *Recommended default: fine at launch; build the report button as a fast-follow.*
4. **Bots, polls, giveaways, webhooks and slash commands will not work in private channels** — the only way to make them work would be handing the server the keys, which would end the privacy promise. Confirm that's acceptable? *Recommended default: accept; never give the server keys.*
5. **Should private direct messages be upgraded next?** Today's DMs use weaker crypto than private channels will after this work (one fixed key per pair, forever). *Recommended default: yes, as the next rung ("a DM is a two-person group"), after Rung 2 ships.*
6. **How strict should the "keys must be refreshed" rule be?** To actually deliver the privacy benefit, a private channel must periodically refresh its keys, and a channel that has gone too long without a refresh **stops accepting new messages until someone opens the app**. Stricter = better privacy, more "this channel is temporarily locked" moments. *Recommended default: refresh roughly weekly or every 200 messages; lock after 500 unrefreshed events. Loosen if it annoys you in practice.*
7. **Losing your only device means losing your private-channel history**, even after recovering your identity with the 24-word phrase — the phrase restores who you are, not what you could read. Ship that with clear copy, or push "back up / sync your history to your other device" up the roadmap? *Recommended default: ship with clear copy; the local-history backup is a fast-follow.*
8. ~~Your existing servers cannot use private channels without a migration step.~~ **ANSWERED 2026-07-28: (b) fresh servers only** — owner: current servers are testing-only with no outside users, so take whichever option is less work; sub-project 8 dropped. Revisit (a) only if real communities predate Rung 2 shipping.
9. **Should @mentions ping people in private channels?** For the server to send a ping, it has to be told who was mentioned — which tells the host exactly who was named in each sealed message. Options: (a) no pings in private channels (nothing leaks); (b) pings work, and the host learns the list of who was mentioned. *Recommended default: (a) no pings at launch — it is the fail-closed choice — with a per-channel opt-in later if it hurts.*
10. **Should emoji reactions work in private channels?** As built today, a reaction tells the server who reacted with which emoji to which message — readable content, next to a lock icon. Options: (a) reactions off in private channels this rung; (b) build encrypted reactions (real extra work, roughly half a sub-project); (c) allow them and accept that reactions are visible to the host. *Recommended default: (a) off this rung, with (b) as a follow-on.*
11. **Should your saved private-channel history be locked behind your PIN on disk?** Locking it means a stolen or seized laptop reveals nothing; it also means you must unlock before reading old messages, and search only works while unlocked. *Recommended default: yes, lock it — it is the difference between "seized laptop reveals the archive" and "reveals nothing."*
12. **Adding a second device to your account will require your first device to be online and approve it.** This stops a stolen identity key from silently adding a hidden reader to your private channels. It also means you cannot set up a new machine while your old one is off or dead (you would recover as a fresh device and lose that history). Accept? *Recommended default: accept — silent hidden readers are the worse failure.*
13. **Stickers and GIFs will not work in private channels this rung** (they rely on the server fetching or hosting content). Voice messages will work. Accept, or is one of them important enough to build now? *Recommended default: accept; revisit if you miss them.*

## Known accepted risks / minor log

Disposition of the red teams' minor findings, and the residual risks accepted knowingly:

- **M1 — credential identity encoding.** *Folded in:* length-prefixed, domain-separated `"farder-mls-cred-v1"` encoding. Bare concatenation of a raw `PublicKey` and a hex `DeviceId` string is forbidden.
- **M2 — rmp variant-index encoding breaks old readers.** *Folded in* as the Protocol compatibility section (new-variants-only rule, explicit fail-closed version gate, defined old-client behavior, stated upgrade order).
- **M3 — `manage_channels` had no definition or grant path.** *Folded in by narrowing:* `ChannelCreated` and `MlsGroupReset` are **owner-only this rung**. No new capability string is invented; a real capability definition + grant path is a follow-on.
- **M4 — size caps asserted, not specified.** *Folded in:* the Size caps table. Accepted residual: a malicious client may use the full 40 KiB rather than 8000 chars.
- **M5 — `DeviceCert` had no expiry/rotation and now serves two protocols.** *Partially folded in:* `expires_at` added now (one field now, a migration later) and checked by the fold. **Accepted:** no rotation path, and one key still carries two revocation authorities; a future audit should look at the cross-protocol reuse.
- **Accepted: first-lie cost.** Chaining makes a lying commit a dead end, but the first lie still forces a group reset.
- **Accepted: roster visibility.** Per-channel rosters are public server-wide this rung; recorded as a prerequisite to fix before channel-ACLs.
- **Accepted: non-prunable commit stream** until external commits land at Rung 3/4.
- **Accepted: bucketed length leak** — padding hides exact length, not the bucket.
- **Accepted: widget-link timing correlation** in E2EE channels.
- **Accepted: identity-key compromise with all victim devices dead** still yields a first-device add.

## Decisions locked (pending owner review of the open questions)

- **Per-channel content class**, part of channel identity via **`ChannelCreated { class }`** in the log, immutable, **fail-closed** (unresolvable class ⇒ unusable channel, never plaintext).
- **OpenMLS 0.8.1** (MTI ciphersuite; `openmls_rust_crypto` + `openmls_sqlite_storage`), client-side only, wrapped in `farder-mls`; the server never holds group keys or runs MLS.
- **One MLS group per E2EE channel**; leaves are `(identity, device)` with the device subkey as leaf signature key, bound via a log-valid non-expired `DeviceCert`, with a length-prefixed credential identity.
- **MLS store instance binding + no-resume** on clone/restore/rollback, to make AES-GCM nonce reuse structurally impossible.
- **The log is the Delivery Service**; the fold's per-channel `(generation, epoch)` compare-and-set is MLS's total order; losing commits are rejected at ingest and are deterministic fold no-ops forever; same-epoch ties are broken **drift-first**, then canonically.
- **Commits are chained to the real tree** (`prev_epoch_authenticator` + `post_tree_hash`), and **leaves count only when the joiner confirms them** (`MlsLeafConfirmed`).
- **Head attestation** (`authz_head` on commits and sealed messages + periodic sealed `AuthzBeacon`), with visible, send-blocking failure on unproducible heads.
- **Freshness is enforced blind**: mandatory client rekey cadence, fold-enforced staleness ceiling, and a `pending_removals` send gate.
- **Additional devices are self-added**, capped at 8 per identity, and leaf changes are surfaced in-channel.
- **`MlsGroupReset` is non-selective, owner-only, rate-limited**, and leaves the channel dead until confirmed leaves match the fold's member set.
- **One server-side write choke point + fail-closed client rendering**: no legacy or server-authored plaintext path can write into, or be rendered inside, an E2EE channel.
- **`MessageDeleted` tombstones** make content-blind moderation durable across restart/reconcile.
- **No history escrow**, argued on the honest delta; **local store PIN-wrapped in scope**, with purge obligations on delete/redaction/retention.
- **E2EE attachments**: client-sealed cipherblobs under per-file keys carried inside message ciphertext, with **mandatory client-side filename sanitization and magic-byte sniffing** replacing the voided server file policy.
- **`DeviceRevoked` + `DeviceCert.expires_at` enter the log this rung.**
- **Sealed edits** (`MessageEditedE2ee`); reactions, threads, stickers/GIFs, embedded auto-fetch, and mention routing are **refused server-side** in E2EE channels this rung.
- **Protocol compatibility rules are normative**: never mutate shipped structs/variants; new data in new variants; fail-closed version gate; server-first upgrade order.
- **Rung 2 ships as seven sub-projects** (plus a conditional eighth for existing-server migration), `farder-mls` first, with all protocol variants landing dormant in sub-2/3.
