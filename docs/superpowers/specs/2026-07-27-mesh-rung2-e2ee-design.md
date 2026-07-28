# Mesh Rung 2 — E2EE Community Content via OpenMLS — Design Spec

**Date:** 2026-07-27
**Status:** Draft (for red-team + owner review)
**Part of:** the mesh-hosting north-star (see project memory `project_farder_mesh_hosting`). This is **Rung 2** of the ladder locked in the Rung-1 spec (`2026-06-25-mesh-signed-log-foundation-design.md`).

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

Make community (channel) message content **ciphertext at rest on the server** for channels the owner marks E2EE — so that a Rung-3 mesh host replicating the log holds nothing readable. Group key agreement, forward secrecy, and post-compromise security come from MLS (RFC 9420) via the audited OpenMLS crate; membership *authority* stays exactly where Rung 1 put it: the plaintext authz fold of the signed log.

## Non-goals (explicitly out of scope for this rung)

- Replication / multi-host (Rung 3). The design must not *block* it (fold determinism, see the commit-race section), but only the single-host path ships.
- Migrating DMs onto MLS. Today's DM path (static-static ECDH, no forward secrecy) is untouched; its weakness is noted in Risks and its migration is an open product question.
- Encrypting *metadata*: membership, permissions, channel names, event timing, message sizes, and reply-graph shape remain visible to the host and relay. The relay-as-metadata-chokepoint problem stays a later-rung constraint, unchanged.
- Encrypted server-side search, encrypted bots/widgets/webhooks inside E2EE channels (each has a stated fate in the Coexistence section; most are "non-E2EE channels only" this rung).
- History escrow for late joiners (explicit product decision below: **not built**).
- Migration of existing channels to E2EE. Fresh-start, matching Rung 1: the class is chosen at channel creation and is immutable.

## Global constraints

- **Authorization inputs stay plaintext in the log.** Membership, bans, invites, device certs, permission grants, and all MLS *control* metadata (epoch numbers, declared adds/removes) are outside the encrypted envelope — otherwise a checkpoint-holder cannot validate, and Rung 3 hosts cannot order or replicate. Only *content* is sealed. This is the honest trade: E2EE channels hide **what was said**, not **who is in the room or when they spoke**.
- **The fold stays pure and checkpoint-composable.** New MLS group state tracked by the fold (per-channel epoch head, generation, consumed key packages, leaf set) is small, deterministic, and serializes into checkpoints like everything else.
- **The server never holds a group key, ever.** No server-side member leaf, no escrow, no "server bot" inside an E2EE group. Any feature that would require it is deferred or confined to non-E2EE channels.
- **Old events are untouched.** `MessagePosted` keeps its exact shape (canonical bytes of shipped events must remain stable). New behavior arrives as *new* `EventPayload` variants appended to the enum; rmp_serde enum encoding tolerates trailing additions for new events.
- **Verify by observation** (CLAUDE.md): every E2EE claim ships with an observation test capturing the real wire/storage bytes and asserting no plaintext.

## The channel content class: `e2ee` per channel, chosen at creation

**Decision: per-channel, not all-or-nothing.** A channel is created either **plaintext-class** (today's behavior, full server feature set) or **E2EE-class** (content sealed, degraded server features). The flag is immutable after creation.

Why per-channel and not server-wide:

- The July feature set the owner actively uses (ticker bots, webhooks, slash commands, polls/giveaways — see Coexistence) *fundamentally requires* server-readable content. All-or-nothing E2EE would kill features shipped this month or gut the E2EE promise with a key-holding server. Per-channel lets both be honestly what they claim.
- It matches the product story: "#general has the bots and the polls; #private is sealed."

**Threat-model consequences, stated honestly:**

- In a **plaintext-class** channel, the host (and at Rung 3, every mesh host) reads everything. The class is displayed in the UI (no lock icon; a one-time notice at creation). Nothing about Rung 2 improves plaintext channels.
- In an **E2EE-class** channel, the host stores only MLS ciphertext + control metadata. It still sees: who posted, when, how big, reply-to edges, attachment count/sizes, and full channel membership. A member's compromised device reveals everything that device could decrypt (its local plaintext store + current-window epochs).
- A server whose channels are all plaintext-class is not "partially E2EE" — the UI must never imply otherwise.

**Log representation.** Channel config stayed DB read-model at Rung 1, permitted because it didn't gate event validity beyond "channel exists." The e2ee flag *does* gate validity (a plaintext `MessagePosted` into an E2EE channel must be rejected by any replaying node), so it enters the log: new payload `ChannelE2eeEnabled { channel_id }`, authored by the owner or a member holding `manage_channels`, valid only if the channel has no prior message events (fold tracks a `channels_with_messages: HashSet<u64>`). Absence of the event = plaintext-class. Fold rules: `MessagePosted` is invalid in an E2EE channel; `MessagePostedE2ee` (below) is invalid in a plaintext channel.

## MLS mapping

### Library, ciphersuite, storage

- **openmls 0.8.1** (MIT; SRLabs-audited May 2026, all findings remediated in 0.8.1; actively maintained). Sync API — call from tokio via `spawn_blocking`. Pre-1.0 API churn is a named risk.
- Crypto provider: stock **`openmls_rust_crypto`**. Storage: **`openmls_sqlite_storage`** (rusqlite-based, in the OpenMLS workspace) in the *client* crate — the sqlx variant's sync/async story is undocumented, so the rusqlite one is the safe pick. MLS state lives client-side only; the server never runs MLS group operations.
- Ciphersuite: **`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`** — the RFC 9420 mandatory-to-implement suite, and its Ed25519 signature scheme matches Farder's identity/device keys.
- MSRV note: OpenMLS main targets rustc 1.91+; the implementation plan confirms our toolchain before locking the dep.

### Group granularity: one MLS group per E2EE channel

Argued against the alternative (one group per server):

- **Per-server group** would mean every member of the server can decrypt every E2EE channel. Channel-granular privacy — the entire reason to encrypt a *channel* — would be a UI fiction. It also couples all channels' epoch cadence: any membership change rekeys everything at once.
- **Per-channel group** gives channel-granular readability (forward-compatible with the Rung-1 follow-on channel-ACLs: when a channel is restricted to a role, only those members get Welcomes), contains blast radius (one poisoned group ≠ all channels), and keeps commit streams independent.
- Cost: N groups → N Welcomes on join and N remove-commits on ban, and per-channel MLS state. Acceptable: E2EE channels are expected to be few per server, and the steward logic (below) batches naturally.

**Membership rule this rung:** every current approved server member's device belongs to every E2EE channel's group (channel-ACL subsets arrive with the ACL follow-on; the group machinery is already granular enough to support it — that's the point of per-channel).

### Leaves: one per (identity, device), bound to the DeviceCert chain

Per RFC 9750 convention, **each device is its own MLS leaf** with its own cryptographic state. This maps one-to-one onto Rung 1's `(author, device)` chain model:

- **Leaf signature key = the device subkey** (Ed25519, same key that signs log events). Binding is direct: validators check the MLS leaf's signature key equals the `device_pubkey` in an identity-signed `DeviceCert` present in the log. Cross-protocol reuse is acceptable here because MLS signs under RFC 9420 domain-separation labels and our event signing signs rmp bytes of `EventCore` (which begin with the server-id string) — the signed byte domains cannot collide. (Alternative — a third per-device MLS-only subkey certified by the device key — adds a link to the chain for marginal benefit; rejected, revisit-able, noted in Risks.)
- **Leaf credential**: a basic credential whose identity bytes are `canonical(identity_pubkey || device_id)`. Every member processing a commit MUST verify: credential identity/device match the leaf signature key via a log-valid `DeviceCert`, and the identity is a current, non-banned, approved member per the authz fold. An Add violating this is an invalid commit.
- **KeyPackages**: each device generates and publishes KeyPackages (HPKE init keys + leaf material) signed by its device subkey — the *new key material* this rung introduces. Publication path is the log (next section).

### Wire formats

- **Application messages: `PrivateMessage`** (sealed) — obviously.
- **Handshake (commits/proposals): `PublicMessage`** (plaintext framing). Rationale: membership and epoch progression are *already* public in the authz fold by constraint; plaintext handshake lets the server read epoch/group-id for ordering and lets any auditor cross-check declared metadata (below) against actual proposals. No secrecy is lost that the design didn't already concede.
- **Welcome self-containment**: groups are created with the ratchet-tree-extension enabled so Welcomes carry the tree and a joiner needs nothing else. Cost: Welcome size grows with group size — acceptable at community scale, flagged in Risks with the out-of-log tree-serving fallback.

## Transport over the log: new `EventPayload` variants

The signed event log **is the Delivery Service**: it stores/hands-out KeyPackages, queues Welcomes, fans out commits, and (via ingest + fold) supplies the one thing MLS demands that Lamport clocks don't give — a **total order of commits per group**. New variants (appended to the enum; all signed/chained/authorized exactly like existing events):

```
| MlsKeyPackagePublished { key_package: Vec<u8> }
    // authored by the device that owns it; server-scoped (usable for any channel group).
    // Consumed-once semantics enforced by the fold (see below). Devices keep a small
    // pool published ahead of time and top it up; freshness by created_at + client rotation.

| MlsCommit { channel_id, generation, epoch, mls_message: Vec<u8>,
              adds: Vec<DeclaredAdd>, removes: Vec<DeclaredRemove> }
    // DeclaredAdd    = { identity, device, key_package: EventRef }
    // DeclaredRemove = { identity, device }
    // The DECLARED fields duplicate the commit's semantic intent in fold-readable form,
    // because the fold cannot maintain ratchet trees (leaf-index -> identity mapping)
    // without becoming an MLS implementation. Members verify declared == actual when
    // processing; a mismatch is a provable, signed lie (recovery: group reset, below).

| MlsWelcome { channel_id, generation, commit: EventRef,
               for_member: PublicKey, for_device: DeviceId, welcome: Vec<u8> }
    // Welcome bytes are encrypted to the joiner's KeyPackage init key — safe to log publicly.

| MlsGroupReset { channel_id, new_generation }
    // The recovery hatch: authored by owner or manage-channels holder; abandons the
    // current group (poisoned commit, total steward loss) and starts generation+1
    // with fresh Welcomes. Drastic, rare, log-visible, auditable.

| MessagePostedE2ee { channel_id, generation, epoch, ciphertext: Vec<u8>,
                      reply_to: Option<EventRef>, attachments: Vec<AttachmentCap> }
    // ciphertext = MLS PrivateMessage sealing { content: String, attachment_keys: [...] }.
    // reply_to + attachment caps stay OUTSIDE the seal: the server must thread replies
    // and validate caps-vs-blobs blind. Size cap: 16 KiB ciphertext (replaces the
    // 8000-char plaintext check, which the server can no longer perform).

| DeviceRevoked { device: DeviceId }
    // Promoted from Rung-1 follow-on because E2EE makes it load-bearing: authored by the
    // owning identity (or the server owner for abuse); fold marks the device's cert dead;
    // its chain is frozen (existing events stay valid; new events rejected); stewards
    // must Remove its leaves (membership bridge, below).
```

Fold state added to `LogState` (all deterministic, all checkpoint-serialized):
`mls_groups: HashMap<channel_id, { generation, epoch, commit_head: EventHash, leaves: HashSet<(PublicKey, DeviceId)> }>`, `consumed_key_packages: HashSet<EventRef>`, `channels_with_messages: HashSet<u64>`, `e2ee_channels: HashSet<u64>`, `revoked_devices: HashSet<DeviceId>`.

## Ordering: MLS epochs on a Lamport log — and what happens when commits race

MLS requires **one agreed linear order of commits per group**; Rung 1's per-`(author,device)` chains + Lamport clock deliberately avoid a total order. The reconciliation:

- **At Rung 2 the single host is the sequencer it always was.** `SubmitEvent` ingest is serial check-then-mutate against the live `LogState`. Fold rule for `MlsCommit`: valid iff `generation` matches the channel's current generation AND `epoch == current_epoch` (the commit is built *against* epoch n and advances the group to n+1; the fold records `epoch := epoch + 1`, `commit_head := event_hash`). Adds must reference unconsumed `MlsKeyPackagePublished` events for devices that are current, cert-valid, non-revoked members; removes must target current leaves whose removal the membership bridge authorizes (below).
- **Race:** two members commit against epoch n concurrently. The first to reach the server wins and is appended; the second **fails validation at ingest** (stale epoch) and is rejected with a specific `stale-epoch` error. The losing client resyncs: fetch + process the winning commit, rebuild its intended changes as a new commit against n+1, resubmit. This is exactly RFC 9750's strongly-consistent DS model ("the DS is trusted to break ties"), implemented as the fold's compare-and-set on `(channel_id, generation, epoch)`.
- **Rung-3 forward-compatibility (design now, ship later):** under replication, a losing commit may already sit in some host's log before sync converges. The fold therefore treats an epoch-stale `MlsCommit` as a **deterministic no-op** (recorded, ignored, zero state change) rather than a hard error — so when Rung 3 folds a converged event set in canonical `(lamport, author, event_hash)` order, every node picks the identical winner per epoch and skips the identical losers. Rung 2 additionally rejects losers at ingest (better UX, nothing enters the log), but the *fold* never assumes ingest caught them. This one rule is what keeps Rung 2 from baking in a Rung-3 redesign.
- **Commit skipping is impossible** (MLS property): a returning member must process every accepted commit in order. The log already retains them all; catch-up = fetch `MlsCommit` events for the channel since your last-processed epoch, process serially. A device that has fallen off a future pruned window (Rung 3/4) rejoins via the reset/re-add path; external commits are noted as a future optimization, not used this rung.
- Application-message tolerance: `SenderRatchetConfiguration` defaults plus `max_past_epochs = 3` so slightly-stale `MessagePostedE2ee` events already in flight during a rekey still decrypt client-side. Ingest, however, only *accepts new* application messages citing the current epoch — a stale-epoch send bounces with a resync error (client processes pending commits, re-encrypts, retries). This also hard-closes the post-ban send window server-side (below).

## The membership bridge: the authz fold drives MLS

**The authz fold is the authority; the MLS group is a downstream projection of it.** MLS never decides who is a member — it only catches up to what the log already ruled.

- **Join:** `MemberJoined` (+ `MemberApproved` where required) makes the identity a member per the fold. Each of its devices publishes `MlsKeyPackagePublished`. A **steward** then commits Adds + emits `MlsWelcome`s for each E2EE channel. Until that lands, the new member is a member-without-keys: they see the channel exists, see ciphertext arriving, decrypt nothing. UI: "waiting for a member to grant keys."
- **Leave / kick / ban / device revocation:** `MemberRemoved` / `MemberBanned` / `DeviceRevoked` in the fold obligates removal of the corresponding leaves. A steward commits the Remove, rotating the epoch so the removed party's keys are dead for all *future* messages (MLS PCS).
- **Steward = any current member's client, deterministically nominated, racing safely.** No new role: every online client watching the log detects membership-vs-leaf drift (`fold.members×devices` vs `mls_groups[ch].leaves`). To avoid thundering herds, the client whose `(identity, device)` hashes lowest among online leaf-holders acts first; anyone else acts after a short timeout. Races are safe by construction: the epoch CAS means exactly one corrective commit wins, and a loser's rebase finds the drift already fixed and stands down. Fold-enforced consistency backstop: **any** commit is invalid if it Adds a non-member/banned/revoked leaf, and invalid if it Removes a leaf whose member is still in good standing (except self-removal of one's own device — the device-retirement flow).
- **The desync window, stated honestly:** between a ban landing in the log and the remove-commit landing, the banned member still holds current epoch keys and **can decrypt messages sent in that window**. Mitigations, in order of force: (1) clients that have folded the ban **stop sending** in that channel until the rekey commit lands ("sealed-pending-rekey" send blocker); (2) ingest rejects application messages at pre-rekey epochs once the remove-commit is accepted; (3) the window is typically seconds while any member is online. What we do NOT claim: retroactive protection — everything the banned member could decrypt before removal, they keep. That is MLS working as designed, and the UI copy must not overpromise.
- **Total steward loss** (no member online for a long gap, or the only key-holding devices are lost): the group cannot rekey or welcome. Recovery hatch: `MlsGroupReset` starts a fresh generation — new group, fresh Welcomes from whoever resets, **prior history undecryptable for everyone who lost state** (it is E2EE working as designed). Log-visible and authority-gated, so abuse is auditable.
- **Poisoned commit** (declared fields ≠ actual MLS content — a provable lie by a signed author, or an MLS-invalid commit that ingest's blind checks couldn't catch): members refuse to process it, surface "group needs reset," and an authority issues `MlsGroupReset`. The signed event is permanent evidence for moderation of the liar. Deliberately simple: no in-band supersede/rollback machinery; resets are expected to be near-never.

## History for late joiners: the product decision

**Decision: no history escrow. A new member (or a reset-recovered group, or a from-scratch device) sees no pre-join history in E2EE channels.** The UI says so plainly at the join boundary ("Messages before you joined are end-to-end encrypted and not available to you").

Justification, over the alternative (a per-group "history key" escrowed and handed to joiners):

- A long-lived history key is exactly the mechanism RFC 9750 warns "may reduce the FS and PCS guarantees provided by MLS" — one compromised member (or one subpoenaed/seized device) leaks the *entire channel archive forever*, converting MLS's per-epoch compartmentalization back into a single static secret. That is the DM-crypto weakness this rung exists to escape, reintroduced at group scale.
- It poisons Rung 3: mesh hosts are supposed to hold nothing readable; a history key circulating among all members makes every member's device a full-archive decryption oracle, maximizing the value of compromising any one of them.
- Forward secrecy *is* the product here (privacy-centric platform); "new people can't read old private messages" is a defensible, explainable property — Signal ships it.

Consequences the design absorbs instead:

- **Members keep their own history**: each client maintains a **local decrypted message store** per E2EE channel (new client-side SQLite), because MLS deletes decryption keys aggressively — senders cannot re-read their own messages from server ciphertext outside the `max_past_epochs` window. Server `FetchHistory` still returns ciphertext rows (pagination/ordering work fine); the client renders old E2EE history from its local store, not from re-decryption.
- The local store is plaintext-at-rest on the member's device (same posture as every E2EE messenger's local DB). At-rest protection (wrap under the PIN-derived key, like `identity.key`) is a fast-follow noted in Risks, not a blocker.
- A deliberate, owner-visible escape valve exists *outside* the crypto: a member who has history can quote/forward it. A future optional "share history with new member" feature would be an explicit member-to-member transfer (member exports from their local store over an encrypted direct channel) — an app-layer social action with a visible actor, not a protocol escrow. Deferred; listed as an open product question.

## Attachments in E2EE channels

The Rung-1 capability model was built for this ("encrypted blob, opt-in download, GC-able by capability"):

- Sender generates a random 32-byte per-file key, seals the file bytes with the existing `farder-crypto` AES-256-GCM primitive, uploads the **ciphertext** blob. The `AttachmentCap` (outside the MLS seal, server-validated) references the **ciphertext**: `content_hash = SHA-256(ciphertext)`, `size = ciphertext length`, `declared_type = "application/octet-stream"` always. The per-file key + real filename + real MIME travel **inside** the `MessagePostedE2ee` ciphertext.
- Server-side cap-vs-blob validation (hash/size/uploader match) **works unchanged on ciphertext**. Server-side image validation is impossible and is **skipped for E2EE-channel blobs** (client-side validation on decrypt is best-effort; the render path already treats attachments as untrusted). Redaction (`AttachmentRedacted` by content-hash) and GC/retention work unchanged — deleting ciphertext bytes needs no key.
- Privacy note: random per-file keys make ciphertext hashes **uncorrelatable** across uploads/servers — the existence-oracle concern from Rung 1 gets strictly better for E2EE blobs. Metadata still leaks: attachment count and ciphertext sizes are visible; padding is out of scope (Risks).
- Because the per-file key rides in the message ciphertext, attachment readability inherits message readability exactly: no pre-join history ⇒ no pre-join attachments; ban+rekey ⇒ no new attachment keys. (Old blobs a banned member already fetched are theirs — same honesty as messages.)

## Multi-device and device loss / revocation

- **Adding a device**: new device generates its subkey, the identity signs a `DeviceCert`, device emits `DeviceAuthorized` + `MlsKeyPackagePublished` events. The identity's *existing* device is the preferred steward for adding the new leaves (it is the party with the clearest interest and the least trust cost); any steward may do it — the fold-enforced Add rules make it safe regardless. The new device receives Welcomes and decrypts from its join epoch forward. **It does not receive prior history** (same FS rule); optional local-store sync between a user's own devices is an app-layer follow-on, deferred.
- **Retiring a device gracefully**: the device (or another of the identity's devices) emits `DeviceRevoked`; stewards Remove its leaves; epoch rotates.
- **Device loss**: the identity survives (24-word recovery phrase); the lost device's subkey and MLS state do not. Flow: recover identity on a new device → `DeviceRevoked` for the lost device (identity-authored) → `DeviceAuthorized` + KeyPackages for the new one → steward removes old leaves + adds new ones (one commit). The lost device is cryptographically evicted from all future epochs. **Channel history is gone for that user** unless they had a second device or a local-store backup — stated plainly in the recovery UI.
- **Identity-key compromise** remains out of scope (no identity rotation exists; Rung-1 known gap, unchanged, in Risks).

## What the server (and a future mesh host) can still validate blind

Everything Rung 1 validates, none of which touched content:

- Event signatures, DeviceCert chains, `(author, device)` seq/prev continuity, Lamport monotonicity, server-id binding.
- The full authz fold: membership, bans, approvals, invites, permission grants — all plaintext by constraint.
- MLS control-plane sanity: generation/epoch CAS, declared-adds against membership + unconsumed KeyPackages, declared-removes against the bridge rules, one winner per epoch.
- Attachment cap-vs-blob (hash/size/uploader), redaction, retention GC, anonymization — all ciphertext-safe.
- Rate limits, ciphertext size caps, channel-class rules (no plaintext into E2EE channels and vice versa).

**Content-blind moderation:** moderators keep takedown *mechanics* — `DeleteMessage` / redaction key on **event-hash and content-hash, never on content**. What they lose is *reading* what they remove in E2EE channels: moderation there is report-driven (a member who can read it reports it). A structured report flow (member forwards decrypted content + the event-hash as evidence) is deferred; until then, reports are social (screenshots/DMs) and the mod acts on the cited event-hash. Stated as an open product question because it changes the mod experience materially.

## Coexistence: fate of every plaintext-touching server feature

Verdict classes: **works-on-ciphertext** | **server-features-channel-only** (stays plaintext in non-E2EE channels; unavailable in E2EE channels) | **client-side-redesign** (in scope this rung) | **deferred** (with rationale). No feature gets the "server holds a group key" treatment — that class is banned by constraint. The July owner-visible set (items 1–5) survives untouched because it lives in plaintext-class channels.

| # | Feature | Verdict | Notes |
|---|---------|---------|-------|
| 1 | Ticker / custom-monitor bots | **works unchanged** | Presence-only; never touches channel content. |
| 2 | Bot price alerts / bot DMs | **works unchanged** | Already E2EE-shaped per-recipient DMs; orthogonal to channels. |
| 3 | Incoming webhooks | **server-features-channel-only** | External sender has no group key; config UI refuses attaching a webhook to an E2EE channel. Making the server a key-holding encryptor is banned. |
| 4 | Slash commands (text/api kinds) | **server-features-channel-only** | Server parses args + composes replies. Client-composed/-fetched command replies inside E2EE channels: deferred (relay-proxied fetch like embeds is the future shape). |
| 5 | Polls + giveaways (widgets) | **server-features-channel-only** | Server parses, stores, and re-announces content (incl. the sweeper's winner announcement). Client-composed encrypted widgets + a key-holding-member draw flow: deferred — a full redesign, not a port. |
| 6a | Link embeds (relay `ProxyLinkEmbed`) | **works unchanged** | Already client-extracted + relay-proxied; content never hits the server. |
| 6b | `FetchUrl` auto-attach | **server-features-channel-only** | Server-fetched blob is inherently server-visible. Client-fetch-via-relay + encrypted upload: deferred. |
| 7a | Search (server FTS) | **client-side-redesign (in scope)** | E2EE channels never enter `messages_fts` (ingest skips FTS for `MessagePostedE2ee`); client searches its local decrypted store. Plaintext channels keep server FTS. |
| 7b | History pagination / retention GC / anonymize / mod delete / attachment redaction | **works-on-ciphertext** | All id/timestamp/hash-keyed. Mod delete becomes content-blind (see moderation note). |
| 8 | Mentions / notifications | **works, degraded previews** | Mention parsing is already client-side. Push/notify previews for E2EE messages become generic ("New message in #channel") — notify service only ever sees ciphertext. |
| 9a | Attachment cap validation (`derive_attachments`) | **works-on-ciphertext** | Pure metadata comparison against the stored (cipher)blob. |
| 9b | Server image validation (`validate_image`) | **skipped for E2EE blobs** | Needs plaintext bytes; client-side best-effort validation on decrypt instead. |
| — | 8000-char content check | **replaced** | Server enforces a 16 KiB ciphertext cap for `MessagePostedE2ee`; the client enforces the 8000-char rule pre-encryption. |

## Client changes (`client/src-tauri` + frontend)

- New `farder-mls` wrapper crate is consumed here; MLS state in `openmls_sqlite_storage` DB per `(identity, server)` under the existing data dir.
- Channel-creation UI: content-class choice (plaintext vs E2EE) with an honest explainer; lock icon + class-appropriate copy in channel headers; "waiting for keys" and "history before you joined is unavailable" states.
- Send path: E2EE channels route through encrypt-then-`SubmitEvent` (`MessagePostedE2ee`); stale-epoch bounce triggers process-pending-commits → re-encrypt → retry, invisible to the user.
- Receive path: decrypt application messages; process commits in order; advance epoch state; feed the **local decrypted store** (new SQLite: per-channel plaintext history + search index).
- Steward loop: watch fold-vs-leaves drift; publish KeyPackage pool top-ups; issue Add/Remove commits + Welcomes per the nomination rule.
- Frontend seam discipline per CLAUDE.md: every new `invoke("...")` matched to a registered `#[tauri::command]`; docs updated in the same commits.

## Server changes (`farder-server`, `farder-protocol`)

- Ingest: accept the new payload variants through the existing `SubmitEvent` path; the fold (in `farder-crypto`) carries all new validation; ingest adds the ciphertext size cap and channel-class checks; `stale-epoch` becomes a distinct error code for client resync.
- Derivation: `MessagePostedE2ee` derives a `messages` row with an `is_e2ee` marker and ciphertext content (opaque hex/bytes), **skipping FTS**; attachments derive as today against ciphertext blobs.
- New fetch surfaces (protocol): filtered event fetch for `MlsWelcome{for_member,for_device}` and unconsumed `MlsKeyPackagePublished` per identity — both are just indexed log queries, no new trust.
- Nothing in the server links OpenMLS. The server's MLS knowledge is: opaque bytes + the declared fields the fold validates.

## Sub-projects (each independently shippable + testable, mirroring Rung 1)

1. **`farder-mls` core — pure Rust, no runtime.** New crate wrapping openmls 0.8.1: group create/join(Welcome)/add/remove/self-update, encrypt/decrypt application messages, device-subkey signer adapter, credential binding + the validation helpers members run against fold state, sqlite storage provider wiring, envelope (de)serialization for the new payload bodies. Tests: three-device in-memory groups end-to-end; add/remove/rekey; **forward-secrecy observation test** (removed member's state cannot decrypt post-removal ciphertext; new member cannot decrypt pre-join ciphertext); declared-vs-actual mismatch detection; wrong-suite/wrong-key failures. No server, no client, no UI — exactly Rung 1 sub-project 1's shape.
2. **Log schema + fold: MLS control plane.** In `farder-crypto`: the six new `EventPayload` variants, `ChannelE2eeEnabled`, `DeviceRevoked`, and the fold extensions (epoch CAS, generation, consumed KeyPackages, leaf-set bridge rules, channel-class gating, stale-commit-as-deterministic-no-op). Tests: the full authz matrix for new variants; **commit-race determinism** (two commits at one epoch → same winner from replay, stepwise, and from a mid-stream checkpoint); ban→remove-commit consistency; extended `replay_equals_stepwise_and_composes_from_a_checkpoint` covering MLS state. Pure Rust.
3. **Server ingest + DS duties.** Accept/validate/store/broadcast the new variants; ciphertext caps; class enforcement; FTS skip + `is_e2ee` derivation; Welcome/KeyPackage fetch queries; `stale-epoch` error surface. Tests: validation matrix (reject plaintext-in-E2EE, stale epoch, consumed KeyPackage, non-member Add, good-standing Remove); derived view rebuild parity including E2EE rows; retention/redaction on ciphertext.
4. **Client E2EE vertical (the proven slice).** Channel-class creation UI → KeyPackage publication → steward add/Welcome → encrypted send → decrypt/render → local store + client search → stale-epoch resync → lock/pending/no-history UI states, styled per theme rules. **Observation tests per CLAUDE.md:** capture the real send path's wire bytes and the server's stored row → assert ciphertext, no plaintext substring; second client decrypts; non-member/banned client cannot.
5. **Membership lifecycle + multi-device.** Steward drift loop (join/kick/ban/leave → Add/Remove commits) with nomination + race handling; `DeviceRevoked` flow; device-loss recovery path; `MlsGroupReset` hatch + its authority gating and UI. Tests: ban lands → send-blocker engages → rekey → old member's captured state cannot decrypt new traffic (observation); device-loss rejoin; reset produces a working new generation.
6. **E2EE attachments.** Client-side seal/unseal with per-file keys in-band; octet-stream caps on ciphertext; image-validation skip wiring; redaction/GC verified on cipherblobs. Tests: cap-vs-cipherblob validation; recipient round-trip; non-member fetch denied; redaction deletes bytes.

Order matters (1→2→3→4 strictly; 5 and 6 after 4, either order). Each lands with its docs per the documentation-discipline checklist.

## Risks (honest)

- **OpenMLS pre-1.0 churn.** 0.6→0.7→0.8 each broke APIs. Contained by the `farder-mls` wrapper crate (one place absorbs churn) and pinning 0.8.1. MSRV 1.91 assumption needs confirmation at plan time.
- **The ban-to-rekey window.** Seconds-to-minutes of decryptable traffic for a just-banned member whenever no steward is prompt; the send-blocker mitigates but a fully offline channel population extends it. Documented, not solved — it is inherent to client-held keys.
- **Steward liveness.** Joins wait for an online key-holding member; a fully-idle channel cannot welcome anyone. Acceptable for a community product (someone is around), with `MlsGroupReset` as the last resort; external commits are the future fix if it bites.
- **Declared-metadata trust gap.** The fold validates *declared* adds/removes; a lying member forces a group reset (DoS-shaped, provable, punishable via the signed evidence — not silent compromise). The alternative (server maintaining ratchet trees) creeps toward a server-side MLS implementation and was rejected.
- **Local plaintext store at rest.** Members' devices hold decrypted history unencrypted on disk initially. Fast-follow: wrap under the PIN-derived key like `identity.key`. Same posture as mainstream E2EE messengers' local DBs, but must be stated in privacy copy.
- **Device-subkey reuse as MLS leaf key.** Judged safe (disjoint signed-byte domains, MLS labels), but it is a cross-protocol reuse a future audit should examine; the alternative third subkey is a contained change inside `farder-mls` if needed.
- **Metadata is not hidden.** Membership, timing, sizes, reply edges, sender identities — all visible to host + relay. The relay chokepoint from Rung 1 carries forward untouched. Padding/cover traffic out of scope.
- **DM crypto now lags channel crypto.** After this rung, E2EE *channels* have FS/PCS; DMs still use static-static ECDH with no ratchet. An awkward inversion — flagged to the owner below.
- **Welcome/commit event sizes.** Ratchet-tree-in-Welcome and multi-KeyPackage commits are the largest events the log has carried; per-variant size caps needed at ingest, and very large groups may eventually force out-of-log tree serving.
- **Group reset is a big hammer.** Authority-gated and log-audited, but a malicious `manage_channels` holder can nuke a group's continuity (not its confidentiality). Consistent with existing authority semantics; still worth a UI confirmation wall.
- **Identity-key compromise/rotation** remains unsolved (pre-existing Rung-1 gap): a stolen identity key can authorize new devices. Out of scope; noted so nobody mistakes Rung 2 for covering it.

## Open product questions (owner decides — plain language)

1. **Default channel type at creation:** should "create channel" default to plaintext (all features, bots, polls) with E2EE as the opt-in "private" choice — or ask every time? (Recommendation: default plaintext, an explicit "Private (end-to-end encrypted)" option with a one-line trade-off list: no bots/webhooks/polls/server-search/previews, no pre-join history.)
2. **No pre-join history — acceptable?** New members of an E2EE channel see nothing from before they joined, ever, by design. Is that the product you want for private channels, or do you want the optional "a member can share their history with a newcomer" feature prioritized (a visible social action, not automatic)?
3. **Moderation you can't read:** in E2EE channels, mods delete by pointing at a message, not by reading it — they act on member reports. Is a built-in report flow (member attaches the decrypted content as evidence) needed at launch, or is "screenshot + tell a mod" fine for now?
4. **Bots/polls/webhooks stay out of E2EE channels** — confirmed acceptable? The alternative (server holds channel keys so bots can post) would quietly break the E2EE promise; we recommend never doing it.
5. **Should DMs move onto MLS next?** Today's DMs have weaker crypto (one static key per pair forever, no forward secrecy) than post-Rung-2 channels. Recommendation: yes, as a fast-follow rung ("a DM is a 2-person MLS group"), but it is scope you must want.
6. **Notification previews for E2EE channels** become generic ("New message in #channel"). OK, or do you want an option to decrypt previews on-device where the platform allows it (more work, later)?
7. **Device-loss messaging:** losing your only device means losing E2EE channel history even after identity recovery — the recovery phrase restores who you are, not what you could read. Comfortable shipping that with clear UI copy, or does that push local-store backup/device-sync up the roadmap?

## Decisions locked (pending owner review of the open questions)

- **Per-channel content class**, immutable at creation, recorded in the log (`ChannelE2eeEnabled`), gating message-variant validity in the fold.
- **OpenMLS 0.8.1** (MTI ciphersuite X25519/AES-128-GCM/Ed25519; `openmls_rust_crypto` + `openmls_sqlite_storage`), client-side only, wrapped in a new `farder-mls` crate; the server never holds group keys or runs MLS.
- **One MLS group per E2EE channel**; leaves are `(identity, device)` with the device subkey as leaf signature key, bound via the log's DeviceCert.
- **The log is the Delivery Service**: KeyPackages, Commits (PublicMessage + fold-readable declared adds/removes), Welcomes, and sealed application messages are new signed `EventPayload` variants; the fold's per-channel `(generation, epoch)` compare-and-set is the total order MLS needs; losing commits are rejected at ingest at Rung 2 and are deterministic fold no-ops forever (Rung-3-safe).
- **Authz fold is authority, MLS is projection**: fold-enforced Add/Remove consistency; steward clients reconcile drift; the ban-to-rekey window and its mitigations stated honestly.
- **No history escrow**: forward secrecy is kept; late joiners see no pre-join content; members hold their own history in a local decrypted store.
- **E2EE attachments**: client-sealed cipherblobs under per-file keys carried inside message ciphertext; caps reference ciphertext; server validation/redaction/GC blind.
- **`DeviceRevoked` enters the log this rung** (promoted from follow-on); device loss = identity survives, history does not.
- **Recovery hatch = `MlsGroupReset`** (new generation), authority-gated, log-audited; no in-band commit rollback machinery.
- **Rung 2 ships as six sub-projects**, `farder-mls` (pure Rust, no runtime) first.
