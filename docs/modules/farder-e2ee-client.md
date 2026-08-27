# farder-e2ee-client

> **File(s):** `crates/farder-e2ee-client/src/{lib,transport,channel_key,chain,channel,join,commit,sealed}.rs`
> **Layer:** Crypto crate (client-side only — the server NEVER links this crate)
> **Last reviewed:** 2026-08-26

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

**Status:** Tasks 1-5 COMPLETE (transport seam + channel create / KeyPackage
publish / bootstrap / join + leaf confirmation / steward add + the two
receive-side gates / sealed send + receive), per
`docs/superpowers/plans/2026-08-26-mesh-rung2-sub4a-sealed-vertical.md`.

---

## The transport seam

### `trait E2eeTransport`

Exactly four calls, all `async` (each desugars to
`fn … -> impl Future<Output = …> + Send`, because `async fn` in a public trait
trips the `async_fn_in_trait` lint and is not object-safe):

- `submit_event(&Event) -> Result<EventAccepted, TransportError>`
- `fetch_welcomes(channel_id: Option<u64>, since_accept_seq: u64) -> Result<Welcomes, TransportError>`
- `fetch_key_packages(member: &PublicKey, device: &str) -> Result<Vec<Vec<u8>>, TransportError>`
- `fetch_history_v2(channel_id, before_id, limit) -> Result<Vec<MessageInfoV2>, TransportError>`

The method signatures mirror `farder-protocol::server` request/response shapes.
`channel_id` on `fetch_welcomes` **narrows, never widens**. `#[cfg(test)]`
`testing::FakeTransport` is an in-memory double for unit tests.

### `TransportError`

`ServerRejected { reason }` vs `Transport(String)`. The machine-readable case
is `is_stale_epoch()`, which matches the **bare** `"stale-epoch"` reason string
exactly — the server returns it unprefixed (fact A2.2), so a substring check
for `"event rejected"` would miss it. The resync loop (Task 6) keys on this.

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
  supplies the log-valid trust anchor for Gate 2. The cryptographic binding is
  checked separately by `verify_leaf_binding`.
- `IncomingCommitOutcome::{ Applied, OutOfOrder, LeafBindingFailure }` — the
  typed result: success, an ordering gap/replay (blocked, not skipped), or an
  impostor-leaf rejection.
- `AddMemberOutcome { commit_event_hash, welcome_event_hash, local_epoch,
  post_epoch_authenticator, post_tree_hash }` — `post_epoch_authenticator` is
  read from `group.epoch_authenticator()` immediately after the merge (it is
  NOT a field on `CommitOutcome`, finding F1).

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
  `E2eeError::Transport` — Task 6's resync loop keys on
  `TransportError::is_stale_epoch()`.
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

### `E2eeError`

The crate's one error type. Notable variants:

- `StaleEpochDiverged { local_epoch }` — an own-commit was rejected as
  `stale-epoch` (see the divergence contract).
- `NotConfirmed` — a sealed send before our leaf was confirmed (local refusal).
- `SealedOverCap { reason }` — a sealed message exceeded a size cap before
  submission (content chars, pre-seal bytes, or ciphertext bytes).
- `StoreResumeTerminal(StoreResumeError)` — the store could not be resumed;
  terminal for that store, never papered over.
- `ChannelIdBelowFloor`, `Chain(String)`, `Mls(anyhow::Error)`, `Transport(TransportError)`.

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
- **Task 8 harness** (`tests/e2ee_two_client.rs`) — drives two identities through
  this crate over a real QUIC `E2eeTransport`, exercising the shipped vertical.
