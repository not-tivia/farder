# farder-e2ee-client

> **File(s):** `crates/farder-e2ee-client/src/{lib,transport,channel_key,chain,channel,join,commit,cert,sealed,resync,rekey,drift,revoke,reset,device}.rs`
> **Layer:** Crypto crate (client-side only — the server NEVER links this crate)
> **Last reviewed:** 2026-08-27

## Purpose

Transport-agnostic E2EE **control-plane** logic for Farder's sealed channels
(mesh rung 2, sub-project 4): create the E2EE channel, publish KeyPackages,
bootstrap the creator's group, join from a Welcome, confirm the joiner's leaf,
and (in later tasks) add members, send/receive sealed messages and resync on
`stale-epoch`. It talks to a server only through the [`E2eeTransport`] trait,
so the same crate is driven by the real Tauri client (sub-project 4b) and the
in-process harness (sub-project 4a) against the *shipped* code.

This crate owns **no storage** and **no networking**. It deliberately has no
`quinn`/`tauri` dependency; `ChainState` is a plain struct the caller persists,
and the MLS store is opened/closed by the caller via the lifecycle helpers
below. The server emit sites, the Tauri command layer, and the harness are
other tasks.

**Status:** Tasks 1-6 COMPLETE (transport seam + channel create / KeyPackage
publish / bootstrap / join + leaf confirmation / steward add + the two
receive-side gates / sealed send + receive / bounded stale-epoch resync), plus
Task 10 (production `DeviceCertResolver` + `FetchDeviceCerts` protocol surface),
per
`docs/superpowers/plans/2026-08-26-mesh-rung2-sub4a-sealed-vertical.md`.
Sub-5a lifecycle: C1 (revocation/expiry-aware cert resolver), C2 (rekey +
cadence, `rekey.rs`), C3 (drift discharge, `drift.rs`), C4 (`DeviceRevoked`
emission, `revoke.rs`), C5 (group reset, `reset.rs`) and C6 (multi-device
self-add, `device.rs`) are COMPLETE.

---

## The transport seam

### `trait E2eeTransport`

Exactly six calls, all `async` (each desugars to
`fn … -> impl Future<Output = …> + Send`, because `async fn` in a public trait
trips the `async_fn_in_trait` lint and is not object-safe):

- `submit_event(&Event) -> Result<EventAccepted, TransportError>`
- `fetch_welcomes(channel_id: Option<u64>, since_accept_seq: u64) -> Result<Welcomes, TransportError>`
- `fetch_mls_control(channel_id: u64, since_accept_seq: u64) -> Result<MlsControl, TransportError>`
- `fetch_key_packages(member: &PublicKey, device: &str) -> Result<Vec<Vec<u8>>, TransportError>`
- `fetch_device_certs(identity: &PublicKey) -> Result<Vec<Vec<u8>>, TransportError>`
- `fetch_history_v2(channel_id, before_id, limit) -> Result<Vec<MessageInfoV2>, TransportError>`

The method signatures mirror `farder-protocol::server` request/response shapes.
`channel_id` on `fetch_welcomes` **narrows, never widens**. `fetch_mls_control`
serves one channel's MLS control plane (`MlsCommit` / `MlsWelcome` /
`MlsLeafConfirmed` / `MlsGroupReset`) with the same `next_accept_seq` + `more`
cursor contract as `fetch_welcomes`. `fetch_device_certs` serves one identity's
device-lifecycle events — `DeviceAuthorized` plus, since sub-5 S1, `DeviceRevoked`
— the production source of the Gate 2 trust anchor (see
`cert.rs` below). `#[cfg(test)]` `testing::FakeTransport` is an in-memory double
for unit tests; `MlsControl` and `Welcomes` are the two page-shaped value structs
the trait hands back.

### `TransportError`

`ServerRejected { reason }` vs `Transport(String)`. The machine-readable case
is `is_stale_epoch()`, which matches the **bare** `"stale-epoch"` reason string
exactly — the server returns it unprefixed (fact A2.2), so a substring check
for `"event rejected"` would miss it. Since finding F6 the server emits it for
`MessagePostedE2ee` / `MessageEditedE2ee` too, not just `MlsCommit`, so it is
the one signal the resync loop keys on for a sealed send that lost the epoch
race.

The sub-5a lifecycle added three more fold-rejection predicates, each keyed on
the fold's **verbatim** rejection string (wrapped by the server as
`"event rejected: …"`):

- `is_commit_rate_limited()` — `"commit-rate rule: …"` (a non-drift-discharging
  commit is not permitted yet).
- `is_freshness_ceiling_reached()` — `"freshness ceiling reached: …"` (the
  fold's guarantee a rekey is now permitted).
- `is_sealed_pending_removals()` — `"channel is sealed until a rekey discharges
  its pending removals"` (the reactive drift signal: a dead leaf seals the
  channel until a remove-commit discharges `pending_removals`; see `drift.rs`).
- `is_device_cap_reached()` — `"identity already has the maximum number of live
  devices"` (the live-device cap at `DeviceAuthorized`; C6's `authorize_device`
  maps it to `E2eeError::DeviceCapReached`).

`rejection_reason()` returns the server reason verbatim (with the
`"event rejected: "` prefix when present).

---

## Public interface (Tasks 1-3)

### Event chain (`chain.rs`)

- `Actor<'a> { device, identity, log_server_id }` — the "who" of every event:
  the signing device subkey, its owning identity, and the log server it acts on.
- `ChainState { next_seq, last_event_hash, lamport }` — per-(server, device)
  event-chain state, plain data the caller persists (this crate owns no file).
  `advance(&Event)` moves it past a just-accepted event.
- `build_next_event(device, identity, server_id, chain, timestamp, payload) -> Event`
  — builds + signs the next event exactly like the Tauri client's
  `event_build_next`; pure, no I/O.
- `event_now_secs() -> u64` — untrusted `core.timestamp` claim (ingest bounds
  it to 300 s ahead of server time).

### Channel key + on-disk layout (`channel_key.rs`)

- `ChannelKey { log_server_id, channel_id }` + `ChannelKey::new` — identifies
  one MLS group; validates `log_server_id` on construction **and** every path
  build (hex-only, ≤128 chars — the same path-traversal guard shape as
  `device::validate_server_id`).
- `mls_store_path(data_dir)` → `servers/{log_server_id}/mls/{channel_id}.sqlite`;
  `instance_hash_path(data_dir)` → the 32-byte hash beside it.
- `validate_log_server_id(&str) -> Result<(), String>` — the raw guard.

### Channel lifecycle (`channel.rs`)

- `create_e2ee_channel(transport, actor, chain, spec, data_dir) -> CreateChannelOutcome`
  — submits `ChannelCreated { class: E2ee }` (the server materializes the row),
  then `FarderMlsStore::create` + `MlsChannelGroup::create` at generation 0 and
  persists the instance hash. Rejects a channel id below
  `E2EE_CHANNEL_ID_FLOOR` (1 << 32).
- `publish_key_package(transport, actor, chain, store, store_instance_hash) -> KeyPackageOutcome`
  — generates a KeyPackage, TLS-serializes it, submits
  `MlsKeyPackagePublished { key_package, store_instance_hash, expires_at_log_pos }`.
  `expires_at_log_pos = chain.next_seq + 1 + KEY_PACKAGE_LIFETIME_LOG_POSITIONS`
  (the client cannot observe the server-wide `log_pos`, so it grants a large but
  finite window — see the constant's doc).
- `bootstrap_group(transport, actor, chain, key, group, store, store_instance_hash) -> CommitSubmitted`
  — the creator-only first commit (generation 0 → epoch 1) that confirms the
  creator's own leaf and makes the channel addable (fact A2.5). Emits `MlsCommit`
  from the real `CommitOutcome`.
- `channel_group_id(log_server_id, channel_id, generation) -> String` — the
  canonical group id (`"server/channel/generation-N"`).
- `persist_store_instance_hash` / `read_store_instance_hash` — write/read the
  instance hash beside the store.
- `KEY_PACKAGE_LIFETIME_LOG_POSITIONS` (= 1 << 40) — the KeyPackage log-position
  lifetime window.

### Join (`join.rs`)

- `fetch_pending_welcomes(transport, actor, channel_id, since_accept_seq) -> Vec<PendingWelcome>`
  — paginates per fact A2.8: loop on `more`, feeding `next_accept_seq` back as
  the next `since_accept_seq`; the cursor advances past non-matching rows, so it
  never restarts from 0. Decodes each raw signed `MlsWelcome` and keeps only the
  one addressed to `(our identity, our device)` — the server filters `for_member`
  but **not** `for_device`, so per-device filtering is this client's job. The
  steward's event signature is not verified (no device cert at hand; the
  Welcome's own MLS framing provides integrity).
- `create_joiner_store(data_dir, key) -> (FarderMlsStore, [u8; 32])` — create a
  joiner's fresh store + persist the hash (create-once; see the lifecycle
  contract below).
- `resume_store(data_dir, key) -> (FarderMlsStore, [u8; 32])` — reopen it from
  disk with the persisted hash (terminal on mismatch/poison).
- `join_channel(store, welcome_bytes) -> (MlsChannelGroup, JoinInfo)` — local
  join from Welcome; no event submitted.
- `confirm_leaf(transport, actor, chain, key, store_instance_hash, pending, join_info) -> LeafConfirmation`
  — submits `MlsLeafConfirmed { channel_id, generation, epoch, tree_hash,
  store_instance_hash }`, authored **by the joining device**. `epoch`/`tree_hash`
  come from `JoinInfo`; `tree_hash` equals the adding commit's `post_tree_hash`
  by construction (see "Tree-hash honesty").
- `SendEligibility` (`not_confirmed()` / `confirmed()`; `can_send()`,
  `ensure_can_send()`) and `LeafConfirmation { event_hash, epoch, eligibility }`
  with `can_send()` — the local send-eligibility belief (see below).
- Re-exported: `farder_mls::group::JoinInfo { epoch, tree_hash }`.

### Steward add + receive-side commit processing (`commit.rs`)

- `add_member(transport, actor, chain, ctx, group, member) -> AddMemberOutcome`
  (async) — the steward path: `fetch_key_packages` → `decode_key_package`
  (fails closed on a non-farder credential, and refuses a package whose
  credential does not claim the exact member being added) →
  `MlsChannelGroup::add_members` → submit `MlsCommit` (declared adds/removes +
  the real chaining values) → submit `MlsWelcome` whose `commit` cites the
  accepted `MlsCommit`'s event hash. A `stale-epoch` rejection surfaces as
  `E2eeError::StaleEpochDiverged` (the add has already merged locally).
- `StewardContext<'a> { key, generation, store, store_instance_hash }` — the
  "where am I committing" bundle (keeps `add_member` under the 7-arg clippy
  bound).
- `process_incoming_commit(store, group, commit_bytes, declared, certs) -> IncomingCommitOutcome`
  (sync, **delivery-agnostic** — no transport) — apply someone else's commit,
  in order, through **two gates**:
  1. **Ordering** — `declared.epoch` must equal the group's current epoch;
     a gap or replay returns `IncomingCommitOutcome::OutOfOrder` without
     merging.
  2. **Gate 1** — `MlsChannelGroup::process_commit_checked` ONLY (never
     `process_commit`): declared adds/removes/post-tree-hash must match the
     staged commit, else it is discarded unmerged and the error surfaced.
  3. **Gate 2** — every `ProcessedCommit::actual_adds` entry must pass
     `credential::verify_leaf_binding` against a `DeviceCert` that
     `DeviceCertResolver` attests is log-valid for that `(identity, device)`.
     A failure returns `IncomingCommitOutcome::LeafBindingFailure`
     (equivocation-class); because Gate 1 already merged, the local group is
     poisoned and must be resynced/aborted, never continued.
- `DeclaredCommit { epoch, adds, removes, post_tree_hash }` — the declared
  fields of the `MlsCommit` event, grouped for `process_incoming_commit`.
- `DeviceCertResolver` — trait; `device_cert(identity, device) -> Option<DeviceCert>`
  supplies the log-valid trust anchor for Gate 2. The production implementation
  is [`cert::VerifiedCertResolver`] (built by [`cert::build_cert_resolver`]); the
  cert must come from the log, never from the commit under validation. The
  cryptographic binding is checked separately by `verify_leaf_binding`.
- `IncomingCommitOutcome::{ Applied, OutOfOrder, LeafBindingFailure }` — the
  typed result: success, an ordering gap/replay (blocked, not skipped), or an
  impostor-leaf rejection.
- `AddMemberOutcome { commit_event_hash, welcome_event_hash, local_epoch,
  post_epoch_authenticator, post_tree_hash }` — `post_epoch_authenticator` is
  read from `group.epoch_authenticator()` immediately after the merge (it is
  NOT a field on `CommitOutcome`, finding F1).

### Device cert resolver (`cert.rs`) — Task 10

The production [`DeviceCertResolver`] trust anchor, closing finding F7 (Gate 2
had no production cert source). `resolve_device_cert(transport, identity, device)`
is the primitive: it fetches the identity's device-lifecycle events
(`DeviceAuthorized` + `DeviceRevoked`, since sub-5 S1) over
`fetch_device_certs`, decodes each one, and returns a [`DeviceCert`] only after
**all five** checks pass. `build_cert_resolver(transport, members)` batches it
into a sync [`VerifiedCertResolver`] for [`process_incoming_commit`] (which is
sync and runs Gate 2 over the already-merged commit).

- `resolve_device_cert` → `Result<Option<DeviceCert>>` (async) — fetch + decode
  + verify one `(identity, device)`.
- `build_cert_resolver(transport, &[DeclaredMember]) -> Result<VerifiedCertResolver>`
  (async) — resolves the certs for a commit's declared adds; an unresolvable
  member is simply absent from the map, so Gate 2 fails closed for it.
- `VerifiedCertResolver` — `impl DeviceCertResolver` over an in-memory map.

**What it verifies:**

- **Source** (fetched from the log, never from the commit under validation),
  **binding** (`cert.core.identity == identity` AND
  `cert.core.device_id == device`), and **signature** (`DeviceCert::verify()`:
  `device_id` matches `device_pubkey` and the identity key signed the core).
- **Revocation** (sub-5 C1) — it collects every `DeviceRevoked { device }` in
  the fetched stream (the payload names the VICTIM; `core.device` is the
  revoker) and never returns a cert for a device in that set, even if it is the
  newest verifying cert in the stream.
- **Expiry** (sub-5 C1) — if the winning (newest verifying) cert's
  `DeviceCertCore.expires_at` has passed the client's **local clock**
  (`event_now_secs`), it is rejected. The local clock is used deliberately: a
  revoked/expired device's own `core.timestamp` cannot be trusted, and cert
  expiry is a wall-clock property — local time is the conservative bound (may
  fail closed marginally early on a skewed clock, never serves an expired cert).
- Since sub-5 S1 the fetch stream mixes `DeviceRevoked` in; a
  non-`DeviceAuthorized` payload is never a cert source.

### Rekey (`rekey.rs`) — sub-5a C2

A *rekey* is [`MlsChannelGroup::self_update`] — an own-commit with empty
adds/removes. Because the crate holds no fold `LogState`, the commit-rate rule
and the freshness ceiling are only observable reactively (the fold's rejection
strings) or via a **local cadence**:

- `rekey_channel(transport, actor, chain, ctx, group) -> RekeyOutcome` (async) —
  run `self_update`, submit the empty-adds/removes `MlsCommit` with the real
  chaining values, advance the chain on accept. A `stale-epoch` rejection is
  `E2eeError::StaleEpochDiverged`; a `"commit-rate rule:"` rejection is
  `E2eeError::RekeyRateLimited`. It never loops.
- `RekeyContext<'a> { key, generation, store, store_instance_hash }` /
  `RekeyOutcome { event_hash, local_epoch, post_epoch_authenticator,
  post_tree_hash }`.
- `should_rekey(ceiling_signalled, cadence, now_secs) -> RekeyDecision` — a pure,
  total decision function: a `"freshness ceiling reached"` signal forces a rekey;
  otherwise the proactive cadence (`REKEY_SEALED_SEND_INTERVAL` /
  `REKEY_WALL_CLOCK_SECS`) rekeys only when
  `rekey_permitted_by_rate_rule(...)` (a local re-derivation of
  `event_log_state.rs:1187-1203`) says the commit-rate rule would accept it.
- `RekeyCadence`, `RekeyDecision::{ Rekey(RekeyTrigger), Hold(HoldReason) }`,
  `RekeyTrigger::{ CeilingSignalled, Proactive }`, `HoldReason::{ Cadence,
  RateRule }`, `REKEY_SEALED_SEND_INTERVAL` (= 100), `REKEY_WALL_CLOCK_SECS`
  (= 7 days).

### Drift discharge (`drift.rs`) — sub-5a C3

When a member's device is revoked/expired or a member is banned/kicked, the
fold puts the dead leaf in `pending_removals`, which SEALS the channel
(`event_log_state.rs:576-587, 1445-1448`). A rekey does NOT discharge drift —
`commit_discharges_drift` requires a commit whose declared `removes` intersect
`pending_removals` (`event_log_state.rs:636-646`). Drift discharge is therefore
a distinct operation: a [`MlsChannelGroup::remove_members`] commit listing the
dead `(identity, device)` leaves.

- `discharge_drift(transport, actor, chain, ctx, group, dead_leaves) -> DriftDischargeOutcome`
  (async) — run `remove_members` over `dead_leaves` and submit the `MlsCommit`
  with `removes` = the actually-removed leaves. Mirrors
  `bootstrap_group`/`add_member`/`rekey_channel`'s submit + chain-advance.
  - A `stale-epoch` rejection → `E2eeError::StaleEpochDiverged` (the discharge
    race: another member won the epoch CAS; `remove_members` already merged
    locally, so the caller must resync and retry once — no loop).
  - A `"commit-rate rule:"` rejection → `E2eeError::RekeyRateLimited` (a
    genuine discharge is commit-rate-exempt, so this means the removes did NOT
    intersect `pending_removals` — the dead-leaf set was wrong).
  - An absent target leaf errors at `remove_members` **before** any submit
    (`E2eeError::Mls`), and an empty `dead_leaves` is refused up front — no
    silent no-op, no spin.
- `DriftDischargeContext<'a> { key, generation, store, store_instance_hash }` /
  `DriftDischargeOutcome { event_hash, local_epoch, post_epoch_authenticator,
  post_tree_hash }`.
- `dead_leaves_from_revocation(revoked_device, members) -> Vec<DeclaredMember>` —
  the minimal helper for the reactive signal: the `DeviceRevoked { device }`
  payload names only the REVOKED device (not its identity), so the identity half
  is resolved against the group's member list. Returns every leaf whose `device`
  matches, empty when the device is not in the group.
- `E2eeError::is_sealed_pending_removals()` /
  `TransportError::is_sealed_pending_removals()` — the reactive drift predicate
  keyed on `"channel is sealed until a rekey discharges its pending removals"`.

The reactive wiring (detect the sealed-send rejection / `DeviceRevoked` /
`MembershipChanged` broadcast and call `discharge_drift`) is H1/5b, not here —
C3 provides the primitive + the predicate + the helper.

### Device revocation (`revoke.rs`) — sub-5a C4

The **emission** half of revocation (the fold's `DeviceRevoked` authz is
`event_log_state.rs:996-1010`, authority-note fact 2). On accept the device's
cert is dead, its chain frozen, and its MLS leaf becomes drift lazily via
`pending_removals` (discharged by `drift.rs`).

- `revoke_device(transport, actor, chain, device_id: String) -> RevokeOutcome`
  (async) — build `DeviceRevoked { device }` via `build_next_event`, submit, and
  advance the chain only on accept (the same pattern as `publish_key_package`).
  `device_id` is the **victim**'s hex SHA-256 id
  (`farder_crypto::event_log::device_id(&device_pubkey)`); `core.device` is the
  authoring device, `core.author` is `actor.identity`. Two call shapes, chosen
  by the caller via `Actor` + target (the fn emits the identical payload either
  way — the fold decides):
  - **Self-revoke** — the identity revokes one of its own devices, from any of
    its devices (including the revoked device itself); the `author == rec.identity`
    arm authorizes. This is the form C7 (store re-provisioning) calls.
  - **Owner-revoke** — the server owner revokes a member's device; the
    `is_owner(author)` arm authorizes.
  `DeviceRevoked` merges no MLS state, so there is no divergence contract: any
  rejection surfaces as `E2eeError::Transport` with the reason preserved
  verbatim (`rejection_reason()`), notably `"device already revoked"`,
  `"revocation cites an unknown device"`, and `"only the owning identity or the
  server owner may revoke a device"`.
- `RevokeOutcome { event_hash }` — the accepted event's hash.

### Group reset (`reset.rs`) — sub-5a C5

The owner's "big hammer" to recover a broken/diverged channel: tear the MLS
group down and rebuild it at `generation + 1`. The fold forces this exact
sequence (`event_log_state.rs:1342-1394`, `:1239-1246`, `:1284-1316`), and
`reset_group` builds it from the lower-level `MlsChannelGroup` methods — it is
NOT `add_member` (which submits an `MlsCommit` at the CURRENT generation; a
reset stages Welcomes for the NEXT generation with no accepted commit, because
the new generation has no group in the fold until the reset lands).

- `reset_group(transport, actor, chain, ctx, generation, members) -> ResetOutcome`
  (async, owner) — mint a FRESH one-member group at `generation + 1`, add every
  member in ONE commit (so every welcomed leaf lands at the SAME post-tree-hash;
  one add per member would give each a different tree hash and only the last
  could confirm), stage one next-generation `MlsWelcome` per member
  (owner-only), then submit `MlsGroupReset { new_generation, welcomes,
  post_tree_hash }`. The `commit` ref on each staged Welcome is a documented
  sentinel — the reset generation's add-commit is never a log event, and the
  fold's next-generation Welcome arm never reads it. There is no divergence
  caveat (the fresh group is never submitted as a commit, so no epoch CAS to
  lose); a rejection surfaces as `E2eeError::Transport` with the fold's reason.
- `ResetContext<'a> { key, store, store_instance_hash }` — shared by the
  resetter and a welcomed member. `ResetOutcome { event_hash, new_generation,
  post_tree_hash }`.
- `join_reset(transport, actor, chain, ctx, welcome, reset_post_tree_hash) -> LeafConfirmation`
  (async, member) — `join_channel` from the staged Welcome, then confirm the
  leaf with `tree_hash == reset_post_tree_hash` (the confirmation wall's anchor
  for a reset generation, whose add-commit is never a log event —
  `event_log_state.rs:1284-1316`). It also fails closed locally if the joined
  group's real `JoinInfo.tree_hash` does not match the declared hash, before
  emitting a doomed confirmation.
- `member_live_leaves(transport, identity) -> Vec<DeclaredMember>` (async) — the
  exact-cover helper: enumerate one identity's LIVE devices (authorized,
  un-revoked, un-expired) from the log, revocation- and expiry-aware like
  `cert.rs`'s resolver. The reset caller passes the complete current
  member × live-device set minus the owner's own device as `members` (the fold's
  non-selective-reset rule); this is the CALLER's responsibility.

The exact-cover and confirmation-wall rules are validated against a real
`LogState` replay in `reset.rs`'s tests (mirroring `farder-mls/tests/fold_chain.rs`).

### Multi-device self-add (`device.rs`) — sub-5a C6

The "I am adding a SECOND device to my own identity" path. The fold's self-add
rule (`event_log_state.rs:1136-1145`) means: once an identity holds a confirmed
leaf, only that identity may add its further devices (`author == add.identity`),
so the add-commit is authored by an **existing confirmed device of the same
identity** while the *new* device is the one being added.

- `add_own_device(transport, ctx, new_chain, steward, steward_chain, group) -> AddOwnDeviceOutcome`
  (async) — the one-shot orchestration, in order:
  1. **The new device authorizes itself** — [`authorize_device`] submits
     `DeviceAuthorized { cert }` (`cert = DeviceCert::create(identity,
     &new_device_pubkey, now)`): the **identity** key signs the cert (binding the
     new device to the identity), the **new device** key signs the event
     (`event_log_state.rs:781-802`).
  2. **The new device publishes a KeyPackage** — reused [`publish_key_package`],
     from the new device's own store (its KeyPackage private material lives there).
  3. **The existing confirmed device self-adds the new device** — reused
     [`add_member`], targeting the new device's `(identity, device_id)`; the
     steward's identity equals the added identity, so the self-add rule holds.
  `steward` is the existing device (its `identity` must equal `ctx.identity`,
  guarded up front), `new_chain`/`steward_chain` are the two per-(server, device)
  chains, and `group` is the existing device's loaded group. A `stale-epoch`
  rejection of the add surfaces `E2eeError::StaleEpochDiverged` (same divergence
  contract as `add_member`).
- `authorize_device(transport, actor, chain) -> DeviceAuthorizedOutcome` (async) —
  the primitive behind step 1: submit `DeviceAuthorized { cert }` and advance the
  chain on accept. `actor.identity` signs the cert; `actor.device` signs the
  event. A live-device-cap rejection surfaces as `E2eeError::DeviceCapReached`
  with the fold's reason preserved verbatim.
- `OwnDeviceContext<'a> { identity, new_device, new_store,
  new_store_instance_hash, steward }` — the fixed inputs for the orchestration
  (two actors, two chains, so bundled to stay under the clippy arg bound).
- `DeviceAuthorizedOutcome { event_hash, cert }` / `AddOwnDeviceOutcome {
  device_authorized_hash, key_package_hash, commit_event_hash, welcome_event_hash,
  local_epoch, post_epoch_authenticator, post_tree_hash }`.

**Device cap (8):** the fold enforces the live-device cap at `DeviceAuthorized`
(`event_log_state.rs:840-849`; live = non-revoked + cert-unexpired, at most
`MAX_LIVE_DEVICES_PER_IDENTITY` = 8). The crate holds no fold `LogState`, so it
cannot count live devices client-side — instead `authorize_device` surfaces the
fold's verbatim `"identity already has the maximum number of live devices"`
rejection as `E2eeError::DeviceCapReached` (via
`TransportError::is_device_cap_reached()`), never silently swallowed.

### Sealed send + receive (`sealed.rs`)

- `send_sealed(transport, actor, chain, ctx, group, eligibility) -> SealedSendOutcome`
  (async) — build a `MessageEnvelope` (attachments are sub-6, so
  `attachment_keys`/`filenames`/`mimes` are empty vecs), then submit
  `MessagePostedE2ee { channel_id, generation, epoch, ciphertext, reply_to,
  attachments: vec![], authz_head }` citing the group's **current** epoch. The
  gates, in order, each returning a typed [`E2eeError`] and none of them
  round-tripping:
  1. `SendEligibility::ensure_can_send()` — pre-confirmation send is refused
     locally as `E2eeError::NotConfirmed` (fact A2.6).
  2. `check_preseal_limits` — content ≤ `MAX_CONTENT_CHARS` AND encoded
     envelope ≤ `MAX_PRESEAL_BYTES`, enforced **before** sealing, failing as
     `E2eeError::SealedOverCap`.
  3. `MlsChannelGroup::seal_message` — encode → pad → encrypt.
  4. `MAX_E2EE_CIPHERTEXT_BYTES` — a cheap pre-submit check of the server's
     ciphertext cap (unreachable for any envelope that passed step 2, kept as
     insurance against a framing-cost regression), failing as
     `E2eeError::SealedOverCap`.
  `authz_head` is this device's own folded chain head (`chain.last_event_hash`),
  carried opaque. A `stale-epoch` rejection is NOT handled here (a sealed send
  is not a commit; it merges nothing locally), so any rejection surfaces as
  `E2eeError::Transport` — `resync::send_sealed_resync` (below) wraps this and
  keys on `TransportError::is_stale_epoch()`.
- `SealContext<'a> { key, generation, store, content, reply_to }` — the fixed
  inputs for one send (bundled like `StewardContext` to stay under the clippy
  arg bound).
- `SealedSendOutcome { event_hash, epoch }` — `epoch` is the group's current
  epoch at seal time (sealing never advances the epoch).
- `receive_sealed(store, group, ciphertext: Vec<u8>) -> SealedOutcome` (sync) —
  open exactly one ciphertext. **Takes the ciphertext by value** and returns an
  outcome that cannot be fed back in, so a second `open_message` on the same
  bytes is structurally impossible (see the module doc in `sealed.rs`).
  `SealedOutcome::{ Decrypted(MessageEnvelope), Undecryptable { reason } }` —
  never a plaintext fallback, never a retry. The OpenMLS AEAD `debug_assert`
  panic is contained to a clean `Err` by farder-mls in both build profiles, so
  tampered ciphertext yields `Undecryptable`, never a panic.

### Resync (`resync.rs`) — Task 6

- `fetch_mls_control_exhaustive(transport, channel_id, since_accept_seq) -> (Vec<Event>, u64)`
  (async) — fetch one channel's MLS control plane to exhaustion, decoding each
  raw signed event and returning them oldest-first plus the final cursor.
  Pagination mirrors `fetch_pending_welcomes` (fact A2.8): loop while `more`,
  feeding `next_accept_seq` back as `since_accept_seq`; a `more == true` page
  that does not advance the cursor is surfaced as a transport error rather than
  spun on (commit `a2afff8` fixed the server-side version of that stall; this
  is the client-side guard).
- `send_sealed_resync(transport, actor, chain, ctx, group, request, certs) -> ResyncOutcome`
  (async) — [`send_sealed`] with automatic resync on a `stale-epoch` rejection:
  fetch the winning commits, apply them in order through
  [`process_incoming_commit`]'s two gates, re-seal at the new epoch, resubmit.
  The loop is bounded **twice** and must terminate under every transport
  behaviour:
  1. **Unproductive bound** — [`MAX_UNPRODUCTIVE_RESYNC_ATTEMPTS`] = 3
     consecutive attempts whose resync did not advance the group's epoch surface
     [`E2eeError::ResyncEquivocation`].
  2. **Total bound** — [`MAX_TOTAL_RESYNC_ATTEMPTS`] = 10 caps the loop no matter
     whether the epoch keeps advancing, so a client racing a fast committer
     stops instead of spinning forever.
  Both bounds are pinned by tests that assert termination, not just a happy
  path. "Unproductive" means the group's epoch did not advance between attempts
  (the fetch yielded no winning commit we could apply).
  **F4 poisoned group:** if applying a fetched commit returns
  `IncomingCommitOutcome::LeafBindingFailure`, the impostor leaf is already
  merged (Gate 1 runs before Gate 2, and farder-mls offers no rollback), so the
  loop aborts with [`E2eeError::ResyncPoisoned`] and never retries through it.
- `ResyncRequest<'a> { eligibility, since_accept_seq }` — the fixed inputs
  beyond the `SealContext`: the send-eligibility belief and the caller's
  persisted control-plane cursor (this crate owns no storage).
- `ResyncOutcome { send, next_accept_seq }` — the send result plus the advanced
  cursor for the caller to persist.
- `MAX_UNPRODUCTIVE_RESYNC_ATTEMPTS` (= 3) / `MAX_TOTAL_RESYNC_ATTEMPTS` (= 10).

### `E2eeError`

The crate's one error type. Notable variants:

- `StaleEpochDiverged { local_epoch }` — an own-commit was rejected as
  `stale-epoch` (see the divergence contract).
- `NotConfirmed` — a sealed send before our leaf was confirmed (local refusal).
- `SealedOverCap { reason }` — a sealed message exceeded a size cap before
  submission (content chars, pre-seal bytes, or ciphertext bytes).
- `StoreResumeTerminal(StoreResumeError)` — the store could not be resumed;
  terminal for that store, never papered over.
- `ResyncEquivocation { attempts, last_epoch }` — the resync loop gave up after
  exhausting its bounds; the send kept losing the epoch race (see "Resync"
  below).
- `ResyncPoisoned { member, reason }` — F4, terminal: resync processed a commit
  that failed leaf binding, so the impostor leaf is already merged and the
  local group is poisoned.
- `RekeyRateLimited { reason }` — the fold refused a commit under the
  commit-rate rule (a rekey, or a drift discharge whose removes did not
  discharge anything). Same divergence caveat as `StaleEpochDiverged`.
- `DeviceCapReached { reason }` — the fold refused a `DeviceAuthorized` under
  the live-device cap (8); surfaced by `device.rs`'s `authorize_device`.
- `ChannelIdBelowFloor`, `Chain(String)`, `Mls(anyhow::Error)`, `Transport(TransportError)`.

Predicate methods (machine-readable signals over the fold's rejection strings):
`is_stale_epoch_diverged()`, `is_rekey_rate_limited()`,
`is_freshness_ceiling_reached()`, `is_sealed_pending_removals()`,
`is_device_cap_reached()`.

---

## Store lifecycle contract (the critical footgun)

`credential::generate_key_package` stores a KeyPackage's **private key material**
in the provider's storage, and `MlsChannelGroup::join_from_welcome` needs that
same material to decrypt the Welcome. Therefore a joiner that publishes with
store A and joins with a freshly-created store B fails — and the failure looks
like a corrupt/foreign Welcome, not a provider mistake.

- **Create once, at KeyPackage-publish time:** `create_joiner_store` per channel,
  then `publish_key_package` from the store it returns.
- **Resume thereafter:** `resume_store` reopens that same on-disk store before
  `join_channel`.
- **Resume errors are terminal:** `E2eeError::StoreResumeTerminal` is surfaced.
  Never delete + recreate the store to "recover" — that silently destroys group
  state and the sender-ratchet counter; self-`DeviceRevoked` + re-provision is
  sub-5's job.

`FarderMlsStore::create` refuses an existing path, so a second `create_joiner_store`
for the same channel fails rather than silently recreating.

## `can_send()` and its honest limitation

This crate has **no local `LogState`**, so `can_send()` is derived from the one
piece of evidence it has: whether its own `MlsLeafConfirmed` was accepted. It is
a **local belief, not authoritative fold truth** — the fold may still reject a
sealed send for a stale epoch, a pending removal, a freshness-ceiling hit, or an
incomplete reset. Before confirmation the fold rejects with `"sealed content
author does not hold a confirmed leaf"`, so `ensure_can_send()` refuses locally
with `E2eeError::NotConfirmed` rather than round-tripping a doomed event.

## Divergence contract (from Task 2)

`MlsChannelGroup::self_update` / `add_members` / `remove_members` merge a commit
**locally and immediately**, so by the time the `MlsCommit` is submitted the
local group is one epoch ahead. If the server rejects that submit with the bare
`"stale-epoch"`, the client returns `E2eeError::StaleEpochDiverged { local_epoch }`
and **must not keep using the group** — it must resync local group state from the
log (Task 6). This is never silently swallowed and never reported as success.

## Tree-hash honesty (join confirmation)

`MlsLeafConfirmed.tree_hash` must equal the cited epoch's commit `post_tree_hash`.
`JoinInfo.tree_hash` equals the adding commit's `post_tree_hash` by construction,
so `confirm_leaf` submits it verbatim. There is **no local cross-check** against
the steward's *declared* `post_tree_hash` (the transport seam cannot fetch the
adding `MlsCommit`, only messages), so a lying steward that declared a wrong
value is caught only by the fold, which rejects the confirmation — fail-closed,
not pre-empted client-side.

---

## Integration map

- **farder-mls** — supplies `MlsChannelGroup`, `JoinInfo`, `FarderMlsStore`,
  `StoreResumeError`, and the credential/KeyPackage helpers.
- **farder-crypto** — supplies `Event`/`EventPayload`/`device_id`/`ChannelClass`
  and the identity `Keypair`/`PublicKey`.
- **farder-protocol** — supplies the `MessageInfoV2` the `fetch_history_v2`
  method returns.
- **farder-server** — the emit sites (Task 7) make the events this crate submits
  live; this crate never links the server.
- **farder-client (Tauri, sub-4b)** — the T9 steward command
  `process_mls_control_events` (see `tauri-commands.md`) is the receive-side
  consumer: it resumes the store, calls `fetch_mls_control_exhaustive` from a
  persisted cursor, applies each `MlsCommit` through `process_incoming_commit`
  (Gate 1 + Gate 2 via `build_cert_resolver`), and `join_channel` +
  `confirm_leaf` on our own Welcome. A `LeafBindingFailure` is persisted as a
  terminal "poisoned" flag and surfaced as an equivocation outcome, never
  continued past.
- **Task 8 harness** (`tests/e2ee_two_client.rs`) — drives two identities through
  this crate over a real QUIC `E2eeTransport`, exercising the shipped vertical.

---

## How to run the two-client harness

The Task 8 headless harness lives in the root package's `tests/e2ee_two_client.rs`
(registered as a `[[test]]` in the root `Cargo.toml`). It drives two independent
identities through this crate against an in-process QUIC server — the shipped
vertical, not a reimplementation (plan Decision 1).

```
cargo test --test e2ee_two_client
```

Three tests:

- `two_clients_seal_and_decrypt_end_to_end` — the full path: create the E2EE
  channel → publish KeyPackages → bootstrap → add the joiner → fetch Welcome /
  join / confirm the leaf → sealed exchange, asserting the **exact plaintext**
  in both directions.
- `no_plaintext_reaches_any_table` — after the exchange, byte-scans every table
  of the in-process server's database and asserts the sealed plaintexts appear
  nowhere.
- `a_server_member_not_in_the_mls_group_cannot_decrypt` — a third identity that
  is a server log member (so it *can* fetch the ciphertext) but not in the MLS
  group cannot decrypt it.

The no-plaintext observer is shared with
`crates/farder-server/tests/e2ee_observation.rs` via `tests/common/mod.rs`; the
self-check that keeps that assertion from going vacuously green is
`the_observer_finds_a_needle_that_is_really_there` in that file.
