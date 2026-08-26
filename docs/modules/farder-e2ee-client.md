# farder-e2ee-client

> **File(s):** `crates/farder-e2ee-client/src/{lib,transport,channel_key,chain,channel,join}.rs`
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

**Status:** Tasks 1-3 COMPLETE (transport seam + channel create / KeyPackage
publish / bootstrap / join + leaf confirmation), per
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

### `E2eeError`

The crate's one error type. Notable variants:

- `StaleEpochDiverged { local_epoch }` — an own-commit was rejected as
  `stale-epoch` (see the divergence contract).
- `NotConfirmed` — a sealed send before our leaf was confirmed (local refusal).
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
