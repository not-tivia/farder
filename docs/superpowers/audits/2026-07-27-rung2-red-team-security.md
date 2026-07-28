# Mesh Rung 2 (E2EE / OpenMLS) — Red-Team Findings, Security Lens

**Date:** 2026-07-27
**Target:** `docs/superpowers/specs/2026-07-27-mesh-rung2-e2ee-design.md` (Draft)
**Baseline:** `docs/superpowers/specs/2026-06-25-mesh-signed-log-foundation-design.md` (Rung 1, shipped) and its two red-team rounds (`audits/2026-06-25-*`).
**Posture:** adversarial. The job was to break this design on paper, not to grade it. Code was read where the spec makes a claim about existing behavior (`farder-crypto/src/event_log{,_state}.rs`, `farder-server/src/{handlers,messages,retention,webhooks,polls,giveaways,connection}.rs`, `farder-protocol/src/server.rs`).

**Headline:** the spec is well-argued and honest about the trade-offs it *sees*. The failures below are not in the parts it reasons about; they are in the seam between the two authority systems it bolts together. The design makes the plaintext authz fold the authority and MLS a downstream projection — but **the fold's delivery, the projection's correctness, and the projection's freshness are all under the control of exactly the party E2EE exists to defend against**. Eight findings are Critical. Six of them share one root cause and one family of fixes.

---

## The root cause, stated once

MLS's own security argument depends on the group agreeing about the group: the ratchet tree *is* the membership, and every member verifies it. This design deliberately moves membership authority **out** of the group, into the log's authz fold, and then lets an untrusted host decide **which log events each member sees** and lets any single member **assert** (rather than prove) what a commit did.

Three consequences follow mechanically:

1. **Withheld log ⇒ stale projection.** The fold is only authoritative over a client that has actually folded the event. The server picks.
2. **Asserted commits ⇒ poisoned projection.** `DeclaredAdd`/`DeclaredRemove` are unverified metadata. The fold, the server, the drift detector, and every future Rung-3 replica trust them. Only a member who both processes *and* cross-checks catches a lie, and nothing acts on that catch.
3. **No rekey trigger except drift ⇒ no freshness.** Forward secrecy and PCS are properties of *epoch rotation*, and this design rotates epochs only when membership changes.

Almost everything below is an instance of one of those three. The three minimal fixes are: **fail-closed class + fail-closed rendering**, **in-band epoch/head attestation (tree-hash chaining + joiner confirmation)**, and **a mandatory rekey cadence**.

---

## CRITICAL

### C1 — Channel class fails *open*: absence of `ChannelE2eeEnabled` means plaintext

**Spec text:** "Absence of the event = plaintext-class." Channel *config* (existence, name, type) stayed a DB read-model at Rung 1 by explicit permission; only the e2ee flag enters the log.

**Attack.** A malicious or compromised host serves the channel row for `#private` to Alice, and simply does not deliver the `ChannelE2eeEnabled` event to her client. Her fold says plaintext-class. Her UI shows no lock. Her send path takes the plaintext branch and she posts `MessagePosted { content: "..." }`. The server's own ingest is the thing that would have rejected it — and the server is the attacker, so it accepts. Every other member's replay flags the event as fold-invalid *afterwards*, which is worthless: the plaintext is already in the attacker's hands, and Alice was never warned.

This is the classic downgrade, and it is available with zero forgery: it requires only *omission*, which Rung 1's tamper-evidence explicitly does not cover.

The variant is worse. Because the channel itself is not a log event, the attacker doesn't even need to withhold — it can present a *different channel row* (same name, new id) with no flag at all, and Alice's client has no in-log object to check it against.

**Severity: Critical.** Plaintext disclosure of the exact content the rung exists to protect, no crypto broken.

**Minimal fix.** Invert the default and put the channel in the log:
- New payload `ChannelCreated { channel_id, name, class: Plaintext | E2ee }`. Class is a field of the channel's *identity*, not a separate later event.
- Fold rule: a message event (either variant) is invalid in a channel with no `ChannelCreated` in the prior log.
- Client rule (the load-bearing half): **the client refuses to render or post to any channel it cannot resolve to a `ChannelCreated` in its own folded log.** Unknown class ⇒ channel unusable, never "assume plaintext."

Cost is small and it is the same shape Rung 1 already used to justify moving membership in-log.

---

### C2 — The host controls log delivery, so "the fold is the authority" is advisory exactly when it matters

**Spec text:** "The authz fold is the authority; the MLS group is a downstream projection of it." Ban handling relies on "clients that have folded the ban stop sending" and on a steward noticing `fold.members × devices` vs `mls_groups[ch].leaves` drift.

**Attack (withholding / truncation).** The host bans nothing and forges nothing. It simply stops delivering the `MemberBanned` event — and every subsequent event from that author's `(author, device)` chain — to the channel's online members, while continuing to deliver everyone else's `MessagePostedE2ee` normally. Result:

- No client folds the ban, so the send-blocker never engages.
- No steward sees drift, so no Remove commit is ever built.
- Mitigation (2) ("ingest rejects application messages at pre-rekey epochs once the remove-commit is accepted") is circular — it only fires *after* a rekey that will never happen.
- The banned member keeps decrypting the live channel indefinitely.

Rung 1's per-`(author, device)` seq chain detects *gaps*, not *truncation*: withholding a suffix of a chain is invisible to a client that has never seen the later events. The same primitive gives the attacker three more variants:
- withhold a `DeviceAuthorized` from the victim while showing it to a steward (feeds C5);
- withhold an `MlsCommit` from a subset of members, partitioning them at different epochs — they cannot distinguish "no commits since my last epoch" from "the server is lying," and the honest `stale-epoch` bounce is indistinguishable from a censoring one;
- bounce a client's sends with `stale-epoch` forever while feeding it an old commit stream, so the **server chooses which epoch a client encrypts to** — the spec's "invisible to the user" retry loop is a perfect cover for this.

The spec's whole ban story is written as though the log were a broadcast medium. It is a request/response API served by the adversary.

**Severity: Critical.** Continued read access for a removed member, indefinitely, with no detection surface anywhere in the design.

**Minimal fix — in-band head attestation.** Corroborate the fold view *inside* the group, where the server cannot forge or omit:
- Every `MessagePostedE2ee` and every `MlsCommit` carries the author's `authz_head: EventHash` (the log head they folded).
- Clients compare incoming `authz_head`s against their own. A peer citing a head the client has never seen ⇒ the client fetches it; if the server cannot produce it, that is a **provable equivocation** — surface it hard (this is the same UX posture as Rung 1's genesis-pin change warning) and fail closed on sending.
- Additionally, an inexpensive periodic MLS application message (`AuthzBeacon { authz_head, epoch }`) so a quiet channel still cross-checks.

This one mechanism also closes the epoch-partition and epoch-pinning variants, and it composes with C3's fix.

---

### C3 — A single lying commit permanently disarms the membership bridge (this is *not* "DoS-shaped")

**Spec text:** "The DECLARED fields duplicate the commit's semantic intent in fold-readable form... Members verify declared == actual when processing; a mismatch is a provable, signed lie (recovery: group reset)." Risks calls this a "declared-metadata trust gap... DoS-shaped, provable, punishable — not silent compromise."

**Attack.** Bob is banned. Mallory (any member) submits an `MlsCommit` that **declares** `removes: [bob/device1]` but whose actual MLS content is a plain self-update removing nobody. Check what each party does:

- **The fold** validates the declaration (bob is banned ⇒ his removal is authorized ✓), advances the epoch, and **removes bob's leaf from `mls_groups[ch].leaves`**.
- **The drift detector** — which reads `fold.members × devices` vs `leaves`, i.e. the *fold's* leaf set, not the actual tree — now sees **zero drift**. No steward will ever try again.
- **Ingest** now happily accepts application messages at the new epoch, because the "rekey landed."
- **Bob** is still in the actual ratchet tree and decrypts everything from here to the heat death of the channel.

Detection requires a member to (a) be online, (b) process this specific commit, and (c) run the declared-vs-actual cross-check, and then (d) get a human to issue `MlsGroupReset`. Under C2 the liar can arrange to be the only one processing. And a member who joins *after* this commit can never detect it at all — they have no view of the pre-join tree.

The same shape gives a cheaper, more targeted attack. `MlsWelcome { for_member, for_device, welcome }` — the `for_*` fields are likewise unverified against the Welcome's actual HPKE recipient. Mallory adds Bob (consuming Bob's KeyPackage, so the fold marks Bob's leaf **present**) but encrypts the Welcome to garbage. Bob can never join, the fold shows him fully provisioned, no steward retries, and Bob's UI says "waiting for a member to grant keys" forever — indistinguishable from ordinary steward-liveness lag. **A single member can silently and permanently lock a chosen member out of an E2EE channel.**

The spec's severity assessment is inverted: this is silent confidentiality compromise (bob keeps reading) and silent availability denial (bob never joins), not a noisy DoS.

**Severity: Critical.**

**Minimal fix — bind the declaration to the tree, and confirm leaves from the joiner:**
1. `MlsCommit` carries `prev_epoch_authenticator` and `post_tree_hash` (both RFC 9420 values). The fold **chains** them: a commit is invalid unless its `prev_epoch_authenticator` equals the value the previous accepted commit declared. A liar cannot be built upon — the next honest commit's chain check fails immediately and *deterministically, in the fold*, i.e. on the server and on every replica, without anyone implementing MLS.
2. Every member that processes a commit verifies the real tree hash against `post_tree_hash`; a mismatch is a hard, loud, in-band failure (not a "surface 'group needs reset'" hint).
3. **Joiner-confirmed leaves.** The fold does **not** mark a leaf present on the steward's `DeclaredAdd`. It marks it *pending*, and promotes it only when the joiner itself authors `MlsLeafConfirmed { channel_id, generation, epoch, tree_hash }` from that device. Drift detection then runs on joiner-confirmed state, so a bogus Welcome leaves visible drift and gets retried automatically.

Fix (3) is small and independently closes the ghost-Welcome lockout, part of C5, and I8.

---

### C4 — The forward secrecy and PCS the spec claims are not delivered: there is no rekey cadence

**Spec text (Goal):** "forward secrecy, and post-compromise security come from MLS (RFC 9420)". The no-escrow decision is justified as "Forward secrecy *is* the product here."

**Attack.** MLS gives FS and PCS *per epoch transition*. This design has exactly one trigger for an epoch transition: membership drift (join / kick / ban / device change). Nothing else in the spec commits an Update. So:

- A stable private channel — five friends, no churn for three months — sits on **one epoch** for three months. Seize any member's device today and you decrypt every message back to the last time somebody joined. `max_past_epochs = 3` is irrelevant when there have been zero epoch changes.
- **PCS is strictly worse.** PCS requires the *compromised party* to commit an Update. If Mallory steals Alice's MLS store (not her identity key — just the sqlite file), Alice is still a perfectly legitimate member, there is no drift, no steward acts, and the epoch secret never rotates. **The compromise never heals.** The design has no mechanism by which it could.

This is the sharpest claim-vs-deliver gap in the spec, and it is load-bearing twice over: the no-escrow decision, which costs real product value (no pre-join history, device loss = history loss), is *paid for* with a forward-secrecy property the design does not actually provide. Meanwhile the local decrypted store (I4) hands every long-tenured member the full archive anyway.

**Severity: Critical.**

**Minimal fix.** A mandatory, deterministic rekey cadence:
- Each member's client commits a self-Update when `now - last_own_update > T` (jittered) or after N messages sent, whichever first, using the existing steward nomination + epoch CAS so the cadence does not thrash.
- Fold-enforced ceiling: `MessagePostedE2ee` is invalid if the channel's current epoch is older than `T_max` (measurable deterministically from the log — use the accepted commit's own lamport/position, not wall-clock, to keep the fold pure). A channel that has not rekeyed within the window **stops accepting new content until it does**. That converts FS from a hope into an invariant the blind server enforces.
- Restate the Goal honestly: FS/PCS bounded by the rekey interval, not absolute.

---

### C5 — Ghost-device injection: identity-key compromise silently buys a permanent reading leaf, with no device transparency

**Spec text:** "Identity-key compromise remains out of scope (no identity rotation exists; Rung-1 known gap, **unchanged**, in Risks)."

It is not unchanged. Rung 1's consequence of a stolen identity key was "the attacker can post as you" — noisy, attributable, socially detectable. Rung 2's consequence is:

1. Attacker mints a `DeviceCert` with the stolen identity key.
2. Publishes `DeviceAuthorized` + `MlsKeyPackagePublished`.
3. The fold validates all of it — cert is identity-signed, identity is a current non-banned member. Every rule passes.
4. Any steward (per spec, "any steward may do it") adds the leaf to **every E2EE channel group** and sends Welcomes.
5. The attacker's device **never posts anything.** It reads.

There is no notification obligation to the victim, no obligation for other members to see or acknowledge a leaf-set change, and no ceiling on devices per identity. Combined with C2 (withhold the `DeviceAuthorized` from the victim's own client while showing it to the steward), the victim's device list never changes *from their point of view*. This is precisely the attack that Signal/WhatsApp safety-number-change notifications and CONIKS/Keybase device transparency exist to make visible, and the design has the raw material for it — the log *is* a transparency log, the leaf set *is* in the fold — but no protocol or UI obligation to surface it.

"Out of scope" is a defensible answer for *identity rotation*. It is not a defensible answer for *silent unbounded device addition into an E2EE group*.

**Severity: Critical.**

**Minimal fix (does not require identity rotation):**
1. **Self-add rule.** If identity X already has ≥1 confirmed leaf in a group, an `MlsCommit` adding another device of X is valid **only if authored by an existing device of X**. Only a *first* device may be steward-added. A stolen identity key alone then cannot obtain read access while any real device of the victim is alive — it turns a silent compromise into an event the victim's own client must participate in and can refuse.
2. **Device-list transparency in the UI.** A leaf-set change in an E2EE channel is a rendered, in-channel system notice: "a new device of Alice can now read #private." Non-dismissible, like the genesis-pin warning. Plus a per-identity device count in the member list.
3. Fold-enforced cap on live devices per identity (e.g. 8) — bounds the blast radius and the Welcome cost.

---

### C6 — MLS store clone/restore ⇒ (epoch, generation) reuse ⇒ AES-GCM nonce reuse

**Spec text:** MLS state lives in `openmls_sqlite_storage` "per `(identity, server)` under the existing data dir." Crash/restart/state-loss is never discussed.

**Attack (no attacker required — a backup is enough).** The MLS store holds the sender-ratchet **generation counter**. If two live instances share the same leaf and the same store contents, they will each encrypt an application message at the same `(epoch, generation)` — the same key and the same nonce under AES-128-GCM. Key+nonce reuse in GCM is catastrophic: XOR of plaintexts, and forgery of the authentication tag via the recovered authentication key.

Realistic triggers, all of which a desktop app's users will do:
- Restoring the app data dir from a backup after a crash, then continuing to send.
- Copying the profile to a second machine "so I can chat from the laptop too" (this is the *obvious* user action, and the design never says it's forbidden — it says multi-device is supported, just via a *new* device).
- A VM/container snapshot rollback.
- A cloud-synced home directory (Dropbox/OneDrive/`~` sync), which will happily replicate a sqlite file to two machines.

The spec puts this file in "the existing data dir" — the same dir users already know they can copy because `identity.key` is portable by design (24-word recovery). We have actively trained the user toward the one action that breaks the crypto.

**Severity: Critical.** Total confidentiality and integrity loss for the affected epochs, with no adversary and no exotic conditions.

**Minimal fix.**
1. **Instance binding.** On MLS store creation, generate a random `store_instance_id` and publish its hash in the device's `MlsKeyPackagePublished` / first commit. On startup, if the store's `store_instance_id` does not match what the log records for this `(author, device)`, **refuse to resume**: force `DeviceRevoked` + fresh device rather than reusing the ratchet.
2. **Reuse Rung 1's resync signal.** The client already resyncs its chain head from the server when local chain state is lost or behind. Add the inverse rule: **if the log shows this `(author, device)` chain head ahead of local state, the MLS store is poisoned** — never resume, always re-key as a new device. Rung 1 built the detector; Rung 2 just has to fail closed on it.
3. Store the MLS DB outside anything the user is told is portable, mark it non-backup-eligible on platforms that support it, and say plainly in the recovery UI that restoring it is unsafe.

---

### C7 — `MlsGroupReset` bypasses the authz fold entirely: selective re-Welcome is a silent, unlogged eviction

**Spec text:** `MlsGroupReset { channel_id, new_generation }`, "authored by owner or manage-channels holder... starts generation+1 with fresh Welcomes." Risks: "a malicious `manage_channels` holder can nuke a group's continuity (**not its confidentiality**)."

That parenthetical is wrong.

**Attack.** Mallory holds `manage_channels`. She issues `MlsGroupReset`, then creates generation *n+1* and sends Welcomes to **only the subset she likes**. Nothing in the spec requires the new generation's leaf set to equal the fold's member set. The excluded members:
- have no `MemberRemoved` / `MemberBanned` event — the fold still says they are members in good standing;
- see "waiting for a member to grant keys," which is a **normal, expected state** in this design (steward liveness is an acknowledged limitation), so the UI actively camouflages the attack;
- cannot be helped by other stewards, because a steward must be *inside* the group to commit — and they aren't.

So a single `manage_channels` holder performs an unbounded, undetectable, unauditable eviction from a private channel while the log's authority state says nothing happened. The one mechanism the whole design rests on — "membership authority lives in the plaintext fold where everyone can see it" — has a hole big enough to drive the entire membership through.

Second-order: reset is also asymmetric evidence destruction. Members with a local store keep their plaintext; anyone who reinstalls, and any moderator investigating later, loses it.

**Severity: Critical.**

**Minimal fix.** Make reset non-selective, enforced by the blind fold:
- `MlsGroupReset` is valid only when accompanied (same event, or within a bounded window the fold tracks) by `MlsWelcome`s covering **exactly** the fold's current `members × devices` set for that channel — no more, no fewer.
- The fold refuses `MessagePostedE2ee` in the new generation until the confirmed leaf set (per C3's joiner confirmation) equals the fold's member set. A partial reset is therefore a **dead channel**, loudly, rather than a silent partition.
- Rate-limit resets in the fold (one per channel per N events) and require the UI confirmation wall the Risks section already suggests.

---

### C8 — Plaintext still reaches E2EE channels through paths the coexistence table never audits

The spec's coexistence table has nine rows and a stated principle: "old events are untouched; new behavior arrives as *new* `EventPayload` variants." That framing hides the problem — **the plaintext paths are not all events.**

Confirmed in the current tree:

1. **`EditMessage { message_id, new_content: String }`** (`farder-protocol/src/server.rs:334`) is a plain request carrying the **entire new message body in plaintext**. There is no `MessageEditedE2ee`, no fold rule forbidding it in an E2EE channel, and no row in the coexistence table. Editing a sealed message hands its full text to the host.
2. **`AddReaction { emoji, file_id }` / `RemoveReaction`** (`server.rs:380`) send content-bearing data with per-user attribution, and `ReactionAdded` broadcasts `{message_id, emoji, public_key}`. In an E2EE channel the host learns exactly who reacted with what to which sealed message — an emoji reaction is content. Also absent from the table.
3. **The legacy plaintext send path is alive and reachable from the frontend**: `client/src/lib/tauri-bridge.ts:175` → `invoke("send_message", { content })` → `ServerRequest::SendMessage` (`client/src-tauri/src/commands.rs:1150`). If both `SendMessage` and `SubmitEvent` exist during the Rung-2 transition and the `SendMessage` handler is not class-aware, plaintext lands in an E2EE channel from any call site that wasn't converted — retry/outbox, translation, attachment captions, anything.
4. **Server-authored messages bypass the log entirely.** `webhooks.rs:185`, `polls.rs:326`, `giveaways.rs:289/398`, and the slash-command path at `connection.rs:1223` all call `messages::insert_message*` directly — unsigned rows straight into the derived view. The spec's defense is "the config UI refuses attaching a webhook to an E2EE channel." That is **UI-level enforcement of a confidentiality boundary**, and the insertion functions do not consult channel class at all.

(4) is the worst of the four, because it inverts into a spoofing attack: **a malicious host can inject arbitrary unsigned plaintext rows into an E2EE channel and every client will render them as legitimate content**, since the client reads the derived `messages` view and the design nowhere says "in an E2EE channel, render only what decrypted." The lock icon then certifies host-authored text.

This is the exact failure mode CLAUDE.md names as Farder's known killer (the untyped `invoke` seam; "this is exactly how voice-channel join shipped broken"), applied to a confidentiality boundary instead of a feature.

**Severity: Critical.**

**Minimal fix.**
- **Fail-closed rendering:** in an E2EE channel the client renders **only** rows that it decrypted from a `MessagePostedE2ee` whose signature it verified. Anything else is dropped and counted, with a visible "N messages could not be verified" marker. This single client rule neutralizes (4) and any future leak of this class.
- Class-aware refusal in the *server*, not the config UI: `insert_message*` takes the channel class and hard-errors on E2EE; `SendMessage`, `EditMessage`, `AddReaction` are rejected server-side for E2EE channels.
- Add `MessageEditedE2ee` (sealed) or explicitly drop edit support in E2EE channels and say so in the table. Same decision required for reactions — sealed reactions or no reactions; "unaddressed" is not an option.
- **Enumerate every content-producing call site** (send, edit, reactions, outbox/retry, slash commands, webhooks, polls, giveaways, translation, embeds, bot posts) in the implementation plan, and ship one observation test per path per CLAUDE.md: drive the real path against an E2EE channel, capture stored bytes, assert no plaintext substring.

---

## IMPORTANT

### I1 — The ban-to-rekey window is a client-side courtesy, not a protocol invariant

The spec's first mitigation is "clients that have folded the ban **stop sending**." That is voluntary: a patched client, a client that hasn't folded (C2), or simply a client on an older build keeps sending, and the banned member decrypts. The design has the material to make this mandatory and blind.

**Fix.** The fold already knows `members × devices` and `leaves`. Derive `pending_removals = leaves \ (members × devices)` — pure, deterministic, checkpoint-composable. **Ingest rejects `MessagePostedE2ee` while `pending_removals` is non-empty.** The window becomes "the channel is sealed until rekey," enforced by a server that cannot read a word of it. Combined with C4's staleness ceiling, both freshness invariants are enforced by the blind host.

### I2 — At Rung 3 the commit tiebreak is grindable, so a member can block a Remove forever

The spec calls the "epoch-stale commit = deterministic no-op" rule "what keeps Rung 2 from baking in a Rung-3 redesign," so it has to be right *now*. Under Rung 3 the winner at a given epoch is chosen by canonical order `(lamport, author, event_hash)` — and round-1 already established that `event_hash` is **grindable** (a sender can mine content for a favorable hash) and `lamport` is self-asserted. So a member who wants a Remove never to land pre-mines a competing self-update commit at the same epoch that sorts first, and repeats each epoch. The honest Remove becomes a no-op, deterministically, on every replica. Cost: a few million hashes per epoch.

**Fix.** Do not let arbitrary commits win by lexical tiebreak. Order same-epoch candidates by (1) whether the commit discharges an outstanding removal obligation in the fold, then (2) canonical order. Still a pure function of `(prior_state, event)`, still composes from a checkpoint, and it makes the drift-correcting commit unblockable.

### I3 — Any member can commit: commit-spam DoS and targeted KeyPackage burn

"Steward = any current member's client" with no authority check and no rate limit.

- **Commit spam.** One member spams self-update commits. Every other member's in-flight `MessagePostedE2ee` bounces `stale-epoch`; with `max_past_epochs = 3` and a fast spammer, honest in-flight messages become permanently undecryptable, and every member burns CPU/bandwidth processing the stream. Cheap channel-wide DoS.
- **KeyPackage burn.** `consumed_key_packages` is keyed by `EventRef` and is **not scoped per channel**, so a device needs one KeyPackage per E2EE channel. A malicious steward cycles add → (authorized) remove → add against a victim, burning two of the victim's KeyPackages per cycle, until the pool is empty. The victim must be *online* to top up — so an offline member can be pinned in "waiting for keys" indefinitely. Pairs with C3's ghost-Welcome for a durable lockout.

**Fix.** Fold-enforced, deterministic: a commit by author A in channel C is invalid unless it discharges drift **or** A's previous commit in C is ≥ K epochs back. Cap live KeyPackages per device (e.g. 10) and give KeyPackages a lifetime (see I5). Require Adds of a device to be self-authored where possible (C5's fix), which removes most of the burn surface.

### I4 — The local decrypted store voids retention, redaction, and mod-delete — and undercuts the no-escrow argument

Coexistence row 7b claims "History pagination / retention GC / anonymize / mod delete / attachment redaction — **works-on-ciphertext**." For the server's copy, yes. For the actual data, no: every member holds a permanent plaintext copy in the new client-side SQLite, and nothing in the design propagates deletion to it. In E2EE channels, `retention_secs`, `DeleteMessage`, `AttachmentRedacted`, and the anonymize-on-leave flow (`retention.rs:52`) are **cosmetic**. Rung 1 treated "append-only vs right-to-be-forgotten" as a sharp contradiction and answered it with the honest, compliant-host redaction scoping; Rung 2 silently un-answers it and doesn't say so.

Second, the no-escrow argument is undercut by the design's own local store. The stated reason to refuse a history key is that "one compromised member leaks the *entire channel archive forever*." But a member present since channel creation **has** that archive, in plaintext, on disk, and — by explicit decision — unencrypted at rest (wrapping under the PIN key is deferred to a fast-follow). Farder users have been trained by `identity.key` to expect PIN protection of local secrets. The real delta bought by no-escrow is narrower than the rhetoric: "*new* members can't read old messages." Worth having; not worth the argument as written.

**Fix.** (a) State the local store as a compliant-client obligation, exactly parallel to Rung-1 redaction: retention/delete/redaction events MUST purge the local store, and the spec must say plainly that this is not enforceable against a malicious member. (b) Promote at-rest wrapping from fast-follow to in-scope — it is the difference between "seized laptop reveals the archive" and "seized laptop reveals nothing." (c) Rewrite the no-escrow justification to the honest delta.

### I5 — Checkpoint composability holds for the fold and breaks for MLS; and pruning `consumed_key_packages` enables KeyPackage replay

Two distinct problems under the Rung-1 forward plan of windowed/pruned replication with signed checkpoints.

**(a) The commit stream is not prunable.** The spec is right that its fold state composes. But MLS cannot skip commits, so a member offline across a pruned window can never reach the current epoch. The spec's answer — "rejoins via the reset/re-add path" — means in practice `MlsGroupReset`, a group-wide nuke (and now, per C7, an authority hole). External commits are listed as "a future optimization, not used this rung." They are not an optimization; they are the **load-bearing mechanism that makes pruning safe**, and the ratchet-tree extension this design already enables is exactly their prerequisite. Decide now: either declare `MlsCommit` events permanently retention-exempt and checkpoint-mandatory (accepting per-channel storage that grows with churn forever, on member devices, at Rung 3) or adopt external commits in this rung. Deferring is choosing the first by default, and choosing it silently.

**(b) `consumed_key_packages` is unbounded and dangerous to prune.** It grows forever and must be in every checkpoint. A future pruned checkpoint will look at a set of stale `EventRef`s and reasonably drop them — at which point **KeyPackage reuse becomes possible**, which breaks the single-use property MLS relies on for Welcome forward secrecy. The design does not use MLS's own answer: `MlsKeyPackagePublished` should carry a **lifetime** and the fold should reject any KeyPackage past it, so the consumed-set can be pruned to the live window *safely and deterministically*.

### I6 — E2EE voids all server-side file policy, and moves an attacker-controlled filename to the client unchecked

The spec concedes only that `validate_image` is skipped. The real scope is larger: with `declared_type = "application/octet-stream"` always and ciphertext bytes, the **entire** separate file-hardening track — magic-byte sniffing, content-type allowlist, and download-filename sanitization — is inoperative in E2EE channels. So E2EE channels are the malware-delivery bypass around every server-side content control the project is building.

The new client-side hazard is specific: **the real filename now travels inside the ciphertext**, is fully attacker-controlled, and is never seen by the server's sanitizer. The client writes it to disk on download. That is a fresh path-traversal / extension-masquerade surface (`../../.ssh/authorized_keys`, `invoice.pdf.exe`, RTL-override tricks) created by this design and not mentioned anywhere in it.

**Fix.** State the full scope in the coexistence table (not just image validation). Require the client, on decrypt and before any disk write or render: sanitize the in-ciphertext filename (basename only, extension allowlist, strip bidi controls), sniff magic bytes against the allowlist, and enforce the same policy version the server would have. Test it as a hostile-input case, not a happy path.

### I7 — Metadata is undersold in four specific, fixable ways

The spec's blanket concession ("membership, timing, sizes, reply edges") covers less than it implies.

- **No padding ⇒ ciphertext length is a plaintext-length oracle.** OpenMLS does not pad by default. With a 16 KiB cap and no padding, "yes"/"no", a URL vs a paragraph, and typing-cadence-plus-length fingerprints are all readable. The spec files this under "out of scope" — but a bucketed padding ladder is a configuration decision inside `farder-mls`, near-free, and should be a default, not a deferral.
- **Mention/notify routing is unspecified and leaks either way.** Row 8 says mention parsing is "already client-side" and previews go generic — but the server still has to know *whom to notify*. Either the client sends a plaintext mention list (a **content-derived** leak: exactly who was named in each sealed message, which the spec never concedes) or mentions simply don't notify in E2EE channels (a feature loss the spec never states). Pick one, in writing.
- **Per-channel membership becomes globally public.** `MlsWelcome { channel_id, for_member, for_device }` and `MlsCommit.removes` broadcast, in the server-wide log, exactly who can read which channel. That is fine at this rung (all members are in all groups) but it **directly defeats the channel-ACL follow-on the per-channel-group choice is justified by**: the moment channels are role-restricted, the log tells every member the private channel's roster. Scope the Welcome/commit fetch surface, or accept and document that channel rosters are never private.
- **The commit/Welcome stream is a high-resolution social feed.** Joins, leaves, device additions, and device losses per channel, timestamped, in the clear. That is a sharper signal than "membership is visible."

### I8 — MLS state loss without device loss leaves a healthy-looking, permanently deaf leaf

Distinct from C6. If the MLS store is deleted or corrupted but the device subkey (in identity storage) survives, the device keeps signing valid log events while being unable to decrypt or commit. Every steward's drift detector sees a present, healthy leaf. Nobody re-adds it. Every other member keeps encrypting to a leaf that can never decrypt, with **no error surface anywhere** — the sender sees success, the receiver sees nothing. The design's device-loss flow assumes the *device* is gone; it has no state for "device alive, MLS state gone."

**Fix.** C3's joiner-confirmed leaf state gives the detector: a leaf that has not confirmed the current generation is *pending*, not present, so drift is visible and self-healing. Plus an explicit client-side path — on detecting a missing/unopenable MLS store, self-`DeviceRevoked` + fresh `DeviceAuthorized` + KeyPackages, with UI that says history for that device is gone (same copy as device loss).

---

## MINOR

- **M1 — `canonical(identity_pubkey || device_id)` is underspecified.** `DeviceId` is a `String` (hex SHA-256, `event_log.rs:22`) and `PublicKey` is raw bytes; the spec writes bare concatenation. Specify a length-prefixed, domain-separated encoding for the MLS credential identity so no two `(identity, device)` pairs can ever produce the same bytes. Cheap now, ugly later.
- **M2 — New `EventPayload` variants are not backward-*decodable*.** The spec says "rmp_serde enum encoding tolerates trailing additions." That is true of *encoding stability for existing variants*; it is not forward compatibility for *old readers*. rmp encodes the variant index, so a Rung-1 client or an older Rung-3 replica hits a hard deserialize failure on `MlsCommit` — it cannot parse, cannot verify, cannot advance its Lamport clock, and will compute a divergent fold if it skips. Define the version gate explicitly and make it fail closed ("this server requires a newer client"), not silently skip.
- **M3 — `manage_channels` is invented without a grant path.** The fold's capabilities today are ad-hoc strings (`invite`, `kick`, `ban`) and `PermissionGranted` accepts any string (`event_log_state.rs`). The spec gates `ChannelE2eeEnabled` and `MlsGroupReset` — one of them an authority hole per C7 — on a capability that has no definition, no default holder, and no bootstrap rule. Define it, or gate on owner-only for this rung.
- **M4 — Size caps are asserted, not specified.** The 8000-char rule becomes client-enforced (a malicious client gets the full 16 KiB — harmless, but say so), and the Risks section names per-variant caps for Welcome/commit as "needed" without specifying them. Welcome size is O(group size) with the ratchet-tree extension; pick the numbers in this spec, since ingest has to enforce them blind.
- **M5 — Device subkeys never expire or rotate.** `DeviceCert` has no lifetime and no rotation path, and the same key now serves as both log-signing key and MLS leaf signature key. The cross-protocol-reuse argument in the spec (disjoint signed-byte domains) is sound for forgery, but it means one key with two revocation authorities, two lifetimes, and no rotation. Add a `DeviceCert` expiry field now — it costs one field and is a migration later.

---

## Verdict

**The single most likely thing to sink Rung 2 as designed:** C4 combined with I4. The spec pays a real product price (no pre-join history, device loss = history loss, no bots/polls/search in private channels) to buy forward secrecy — and then does not rotate epochs, and keeps a permanent unencrypted plaintext archive on every member's disk. If someone audits this after it ships, the finding is "you took all the costs of forward secrecy and shipped almost none of the benefit." Fixing it is cheap (a rekey cadence and at-rest wrapping); shipping without it makes the honesty the spec prides itself on load-bearing in the wrong direction.

**The most under-rated finding:** C3. The spec explicitly classifies the declared-vs-actual gap as "DoS-shaped, provable, punishable — not silent compromise." It is precisely silent compromise: one lying commit removes a banned member from the fold's leaf set, clears the drift that would have triggered a real removal, and leaves them decrypting forever. Tree-hash chaining plus joiner-confirmed leaves fixes it, and the same fix pays for C5, C7, I3, and I8.

**Is Rung 2 safe to build as specified?** No — but the gaps are additive, not architectural. Nothing here says "don't use MLS," "don't use per-channel groups," or "don't make the log the DS." Those three choices are right. Six changes should land in the spec before sub-project 1 starts, because each one changes the fold's schema or the event shapes, and retrofitting the fold is exactly the pain Rung 1's round-2 review existed to prevent:

1. **`ChannelCreated { channel_id, class }` in the log; unknown class = unusable, never plaintext** (C1).
2. **`authz_head` on commits and application messages** (C2).
3. **`prev_epoch_authenticator` + `post_tree_hash` on `MlsCommit`, chained by the fold; `MlsLeafConfirmed` from joiners** (C3, C5, C7, I3, I8).
4. **Rekey-cadence ceiling and `pending_removals` gate, both enforced by the blind fold** (C4, I1).
5. **`MlsGroupReset` must re-Welcome exactly the fold's member set** (C7).
6. **KeyPackage lifetimes** so `consumed_key_packages` is safely prunable (I5).

Everything else (C6, C8, I2, I4, I6, I7) is implementation-and-copy discipline that the sub-project plans can absorb — provided C8's fail-closed rendering rule and C6's no-resume rule are written down as requirements rather than left to be discovered at test time.

**On the Rung-3 claim.** The spec asserts that the deterministic-no-op rule is "what keeps Rung 2 from baking in a Rung-3 redesign." It is necessary but not sufficient: I2 (grindable tiebreak lets a member block Removes forever) and I5 (the commit stream cannot be pruned, and pruning the consumed-KeyPackage set enables replay) are both Rung-3 landmines planted by decisions made in *this* spec. They are cheap to defuse now and expensive later — the same argument that put device subchains into Rung 1.
