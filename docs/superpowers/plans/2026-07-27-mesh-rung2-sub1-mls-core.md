# Mesh Rung 2 — Sub-project 1: `farder-mls` Core (Pure-Rust OpenMLS Wrapper) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the pure-Rust `farder-mls` crate wrapping **openmls 0.8.1**: group ops (create / join-from-Welcome / add / remove / self-update), message sealing with the **padding bucket ladder**, the **device-subkey signer adapter** and **length-prefixed credential binding** to `farder-crypto`'s Ed25519 device subkeys, exposure of `epoch_authenticator` / `tree_hash` for commit chaining, declared-vs-actual validation helpers, and sqlite storage wiring with **`store_instance_id` + no-resume**. No server, no UI, no protocol/log changes — the `EventPayload` variants and fold rules are sub-2, ingest is sub-3, the client vertical is sub-4.

**Verified before this plan was written (spec's stop-condition):** `openmls = "=0.8.1"` **resolves and builds** from crates.io on our toolchain (rustc **1.94.1**, above the spec's 1.91 MSRV note), pulling `openmls_rust_crypto 0.5.1`, `openmls_traits 0.5.0`, and `openmls_sqlite_storage 0.2.0` (rusqlite 0.32). A scratch probe project compiled the full stack in ~32 s. Every API this plan names (`epoch_authenticator()`, `tree_hash()`, `add_members`/`remove_members`/`self_update`, `StagedWelcome::new_from_welcome` → `into_group`, `process_message`, staged-commit `add_proposals()`/`remove_proposals()`, `MlsGroupCreateConfig::padding_size`, `Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`, `openmls_traits::signatures::Signer`, `SqliteStorageProvider<C: Codec, ConnectionRef>::run_migrations`) was grep-confirmed in the vendored 0.8.1 sources.

**Architecture:** A new workspace crate `crates/farder-mls`, consumed later by the client crate only (the server never links it — spec constraint). It depends on `farder-crypto` for `Keypair`/`PublicKey`/`DeviceCert`/`device_id` and wraps OpenMLS behind a small, farder-shaped surface: `MlsChannelGroup` (one instance per E2EE channel group), `CommitOutcome` (carries exactly the values sub-2's `MlsCommit` event needs: `prev_epoch_authenticator`, `post_tree_hash`, declared adds/removes, serialized commit + Welcome bytes), `MessageEnvelope` (the sealed `{ content, attachment_keys, filenames, mimes }` body), and `FarderMlsStore` (the `OpenMlsProvider` implementation — in-memory for tests, sqlite + instance binding for real use). Everything is synchronous plain-library code (spec: callers use `spawn_blocking`; not this crate's concern).

**Tech Stack:** Rust; `openmls =0.8.1` + `openmls_rust_crypto =0.5.1` + `openmls_traits =0.5.0` + `openmls_sqlite_storage =0.2.0` + `rusqlite 0.32` (all `=`-pinned per the spec's pre-1.0-churn risk); `farder-crypto` (Ed25519 identity/device keys, `DeviceCert`); `serde` + `rmp-serde` (envelope + storage codec); `sha2`, `hex`, `rand`, `anyhow`, `thiserror`-free (match `farder-crypto`'s anyhow style).

## Global Constraints

- **Spec contracts are exact.** Ciphersuite `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (RFC 9420 MTI). Application messages `PrivateMessage`, handshake `PublicMessage` (`PURE_PLAINTEXT_WIRE_FORMAT_POLICY` — in OpenMLS the wire-format policy governs handshake framing; application messages are always PrivateMessage). Ratchet-tree extension ON (self-contained Welcomes). `max_past_epochs = 3`, `SenderRatchetConfiguration::default()`. Padding ladder `[256, 1024, 4096, 16384, 40960]` bytes, applied to the **plaintext envelope before sealing** (OpenMLS's own `padding_size` is a modulus, not a ladder — leave it 0 and do bucket padding ourselves so the buckets are exact and testable).
- **Credential identity encoding is normative (spec M1):** `"farder-mls-cred-v1" || u8(len(identity_pubkey)) || identity_pubkey || u32_be(len(device_id_bytes)) || device_id_bytes`. Bare concatenation is forbidden; decoding is strict (no trailing bytes, lengths must match exactly).
- **Leaf signature key = the device subkey.** The MLS leaf signs with the same Ed25519 `Keypair` that signs log events, via a `Signer` adapter. Binding validation (`verify_leaf_binding`) checks credential identity/device ↔ leaf signature key ↔ an identity-signed `DeviceCert` — the *fold-validity/expiry/revocation* half of that check is sub-2's job; this crate checks the cryptographic binding given a cert the caller already trusts.
- **No protocol/server/client wiring, no log events, no fold changes.** `MlsCommit`/`MlsWelcome`/etc. are sub-2 types; this crate only *produces the values* they will carry.
- **Store-instance safety (spec C6):** the sqlite store records a random 16-byte `store_instance_id` at creation; `resume()` takes the expected instance hash and **refuses to open on mismatch** (distinct error, no silent re-create) — the caller's only recovery is re-provisioning as a fresh device. The in-memory provider is for tests only.
- **TDD, invariants as test names.** Each task: failing test → implement → green → commit. The spec's sub-1 test list (three-device groups; add/remove/rekey; FS observation; declared-vs-actual + tree-hash mismatch; padding buckets; wrong-suite/wrong-key failures; store-instance no-resume) maps onto named tests below — all must exist by the end of Task 5.
- **Docs discipline (CLAUDE.md):** the new crate gets `docs/modules/farder-mls.md` and an `ARCHITECTURE.md` mention in the same series of commits (folded into Task 5's final step).
- **Do not commit `Cargo.lock` churn unrelated to farder-mls**; the lock update from adding the new crate is expected and committed with Task 1.

---

## File Structure

- **Modify** `Cargo.toml` (workspace root) — add `"crates/farder-mls"` to `members`; add `farder-mls = { path = "crates/farder-mls" }` to `[workspace.dependencies]`.
- **Create** `crates/farder-mls/Cargo.toml`
- **Create** `crates/farder-mls/src/lib.rs` — constants, error helpers, re-exports.
- **Create** `crates/farder-mls/src/credential.rs` — Task 2.
- **Create** `crates/farder-mls/src/group.rs` — Task 3.
- **Create** `crates/farder-mls/src/envelope.rs` — Task 4.
- **Create** `crates/farder-mls/src/store.rs` — Task 5.
- **Create** `docs/modules/farder-mls.md`; **modify** `ARCHITECTURE.md` — Task 5.

---

## Task 1: Crate skeleton — pinned OpenMLS deps compiling in the workspace

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/farder-mls/Cargo.toml`, `crates/farder-mls/src/lib.rs`
- Test: in-module `#[cfg(test)]` in `lib.rs`

**Interfaces:**
- Produces: `pub const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;`, `pub const PADDING_BUCKETS: [usize; 5] = [256, 1024, 4096, 16384, 40960];`, `pub const MAX_PAST_EPOCHS: usize = 3;`, `pub const MAX_PRESEAL_BYTES: usize = 32 * 1024;`, `pub const MAX_CONTENT_CHARS: usize = 8000;`
- Consumes: `openmls`, `openmls_rust_crypto` (test provider).

- [ ] **Step 1: Workspace membership**

Add `"crates/farder-mls"` to `members` in the root `Cargo.toml` and `farder-mls = { path = "crates/farder-mls" }` under `[workspace.dependencies]`.

- [ ] **Step 2: Crate manifest with `=`-pinned deps**

`crates/farder-mls/Cargo.toml`:

```toml
[package]
name = "farder-mls"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
openmls = "=0.8.1"
openmls_rust_crypto = "=0.5.1"
openmls_traits = "=0.5.0"
openmls_sqlite_storage = "=0.2.0"
rusqlite = { version = "0.32", features = ["bundled"] }
farder-crypto = { workspace = true }
serde = { workspace = true }
rmp-serde = "1"
anyhow = { workspace = true }
sha2 = "0.10"
hex = "0.4"
rand = "0.8"
```

- [ ] **Step 3: Write the failing/skeleton test**

`src/lib.rs`: the constants above, `pub mod` declarations added task-by-task (none yet), and a smoke test proving the pinned stack links and supports our suite:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use openmls::prelude::*;
    use openmls_rust_crypto::OpenMlsRustCrypto;
    use openmls_traits::OpenMlsProvider;

    #[test]
    fn pinned_openmls_stack_supports_the_mti_ciphersuite() {
        let provider = OpenMlsRustCrypto::default();
        assert!(provider.crypto().supports(CIPHERSUITE).is_ok());
        assert_eq!(CIPHERSUITE.signature_algorithm(), SignatureScheme::ED25519);
    }

    #[test]
    fn padding_ladder_is_strictly_increasing_and_caps_at_40kib() {
        assert!(PADDING_BUCKETS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(*PADDING_BUCKETS.last().unwrap(), 40 * 1024);
        assert!(MAX_PRESEAL_BYTES < *PADDING_BUCKETS.last().unwrap());
    }
}
```

(Exact `supports`/`signature_algorithm` call shapes may need a one-line adjustment against the real prelude — the assertion *behavior* is the contract.)

- [ ] **Step 4: Build + test**

Run: `cargo build --workspace` — the whole workspace must still build with the new member.
Run: `cargo test -p farder-mls`
Expected: both tests pass. **STOP RULE:** if `=0.8.1` (or the pinned companion crates) fail to resolve or build here, STOP the plan and report — do not substitute versions or roll a different crypto design. (Pre-verified green on 2026-07-28, rustc 1.94.1.)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/farder-mls/
git commit -m "feat(mls): farder-mls crate skeleton pinned to openmls 0.8.1 (MTI ciphersuite, padding ladder constants)"
```

---

## Task 2: Credential binding — length-prefixed encoding, device-subkey signer, KeyPackages

**Files:**
- Create: `crates/farder-mls/src/credential.rs`; modify `lib.rs` (`pub mod credential;`)

**Interfaces:**
- Consumes: `farder_crypto::identity::{Keypair, PublicKey}`, `farder_crypto::event_log::{DeviceCert, DeviceId, device_id}`, `openmls_traits::signatures::{Signer, SignerError}`, `openmls::prelude::*`.
- Produces:
  - `pub fn encode_credential_identity(identity: &PublicKey, device: &DeviceId) -> Vec<u8>`
  - `pub fn decode_credential_identity(bytes: &[u8]) -> anyhow::Result<(PublicKey, DeviceId)>`
  - `pub struct DeviceSigner<'a>(pub &'a Keypair);` implementing `openmls_traits::signatures::Signer` (`sign` → `Keypair::sign`, `signature_scheme` → `SignatureScheme::ED25519`)
  - `pub fn generate_key_package(provider: &impl OpenMlsProvider, device: &Keypair, identity: &PublicKey) -> anyhow::Result<KeyPackageBundle>` — `KeyPackage::builder().build(CIPHERSUITE, provider, &DeviceSigner(device), CredentialWithKey { credential: BasicCredential::new(encode_credential_identity(..)).into(), signature_key: device.public_key().as_bytes().to_vec().into() })`
  - `pub fn verify_leaf_binding(credential: &Credential, leaf_signature_key: &[u8], cert: &DeviceCert) -> anyhow::Result<()>` — decode the credential; require identity == `cert.core.identity`, device == `cert.core.device_id`, `leaf_signature_key == cert.core.device_pubkey.as_bytes()`, and `cert.verify()` passes. (Fold-status checks — membership, revocation, expiry — are sub-2.)

- [ ] **Step 1: Write the failing tests** (encoding implemented, `verify_leaf_binding` stubbed to `bail!`):

```rust
    #[test]
    fn credential_identity_roundtrips_via_length_prefixed_encoding() { /* encode → decode == input; prefix == b"farder-mls-cred-v1" */ }
    #[test]
    fn credential_decoding_rejects_truncated_trailing_or_bad_prefix_bytes() { /* strict decode: truncation, one extra byte, wrong magic all Err */ }
    #[test]
    fn key_package_is_signed_by_the_device_subkey_under_the_pinned_suite() { /* generate_key_package; leaf signature key bytes == device pubkey; ciphersuite == CIPHERSUITE */ }
    #[test]
    fn leaf_binding_accepts_a_matching_identity_signed_device_cert() { /* DeviceCert::create(identity_kp, &device.public_key(), t); verify_leaf_binding ok */ }
    #[test]
    fn leaf_binding_rejects_wrong_device_wrong_identity_or_tampered_cert() { /* other device's cert; cert for other identity; tampered cert.core → all Err */ }
```

- [ ] **Step 2: Run to verify the binding tests fail** — `cargo test -p farder-mls credential` (stub bails).
- [ ] **Step 3: Implement `verify_leaf_binding`** per the interface above.
- [ ] **Step 4: Run to green** — `cargo test -p farder-mls credential` all pass.
- [ ] **Step 5: Commit**

```bash
git add crates/farder-mls/src/
git commit -m "feat(mls): device-subkey signer, length-prefixed credential binding, KeyPackage generation"
```

---

## Task 3: Group ops — create/join/add/remove/self-update, chaining exposure, declared-vs-actual

**Files:**
- Create: `crates/farder-mls/src/group.rs`; modify `lib.rs` (`pub mod group;`)

**Interfaces:**
- Produces:
  - `pub struct MlsChannelGroup { inner: MlsGroup }` — config: `CIPHERSUITE`, `use_ratchet_tree_extension(true)`, `max_past_epochs(3)`, `PURE_PLAINTEXT_WIRE_FORMAT_POLICY`, `SenderRatchetConfiguration::default()`, `padding_size(0)` (bucket padding is ours, Task 4).
  - `pub struct DeclaredMember { pub identity: PublicKey, pub device: DeviceId }`
  - `pub struct CommitOutcome { pub commit_bytes: Vec<u8>, pub welcome_bytes: Option<Vec<u8>>, pub prev_epoch_authenticator: [u8; 32], pub post_tree_hash: [u8; 32], pub epoch: u64, pub adds: Vec<DeclaredMember>, pub removes: Vec<DeclaredMember> }` — exactly what sub-2's `MlsCommit { … prev_epoch_authenticator, post_tree_hash, adds, removes }` event will carry; `prev_epoch_authenticator` is captured **before** merging, `post_tree_hash` after.
  - `pub struct ProcessedCommit { pub actual_adds: Vec<DeclaredMember>, pub actual_removes: Vec<DeclaredMember>, pub post_tree_hash: [u8; 32], pub epoch: u64 }`
  - Methods (all take `provider: &impl OpenMlsProvider`; mutators also take `signer: &DeviceSigner`):
    - `create(provider, signer, credential_with_key_of_creator, channel_group_id: &[u8]) -> Result<Self>` (`MlsGroup::new_with_group_id`)
    - `join_from_welcome(provider, welcome_bytes: &[u8]) -> Result<(Self, JoinInfo)>` where `JoinInfo { epoch: u64, tree_hash: [u8; 32] }` — the values the joiner's `MlsLeafConfirmed` (sub-2) will cite. Uses `StagedWelcome::new_from_welcome(…, ratchet_tree: None)` (self-contained via the extension) → `into_group`.
    - `add_members(provider, signer, key_packages: &[KeyPackage]) -> Result<CommitOutcome>` / `remove_members(provider, signer, members: &[DeclaredMember]) -> Result<CommitOutcome>` (resolves leaf indices by credential) / `self_update(provider, signer) -> Result<CommitOutcome>`
    - `process_commit(provider, commit_bytes: &[u8]) -> Result<ProcessedCommit>` — `process_message` → `StagedCommitMessage` → read `add_proposals()`/`remove_proposals()` (decoding each credential back to `DeclaredMember`) → `merge_staged_commit`.
    - Accessors: `epoch() -> u64`, `epoch_authenticator() -> [u8; 32]`, `tree_hash() -> [u8; 32]`, `members() -> Vec<DeclaredMember>` (32-byte arrays via `try_into` — both are SHA-256-sized under this suite).
  - `pub fn verify_declared_matches_actual(processed: &ProcessedCommit, declared_adds: &[DeclaredMember], declared_removes: &[DeclaredMember], declared_post_tree_hash: &[u8; 32]) -> anyhow::Result<()>` — order-insensitive set equality on adds/removes plus tree-hash equality; any mismatch is `Err` (the member-side check the spec's commit-chaining section mandates; the *fold-side* authenticator chain is sub-2).

- [ ] **Step 1: Write the failing tests** (test helper: `fn member() -> (Keypair /*identity*/, Keypair /*device*/, OpenMlsRustCrypto)`; three in-memory members alice/bob/carol):

```rust
    #[test]
    fn three_devices_form_a_group_via_add_commit_and_welcome() { /* alice creates; adds bob+carol from their KeyPackages; both join_from_welcome; members() agree on all three; epochs equal */ }
    #[test]
    fn commit_outcome_chains_prev_epoch_authenticator_across_epochs() { /* outcome_n.prev_epoch_authenticator == the epoch_authenticator() alice held before committing; after bob processes, bob.epoch_authenticator() == alice's post value; a second commit's prev == that */ }
    #[test]
    fn processed_commit_reports_actual_adds_removes_and_matching_tree_hash() { /* remove carol; bob's ProcessedCommit lists exactly carol in actual_removes; post_tree_hash == outcome.post_tree_hash == bob.tree_hash() after merge */ }
    #[test]
    fn declared_vs_actual_mismatch_is_detected() { /* alice self_updates but the "event" declares removes=[carol]: verify_declared_matches_actual errs; and a wrong declared_post_tree_hash errs (the lying-commit detection of spec §Commit chaining) */ }
    #[test]
    fn join_info_tree_hash_matches_the_commits_post_tree_hash() { /* bob's JoinInfo.tree_hash == the adding CommitOutcome.post_tree_hash — the MlsLeafConfirmed promotion rule's data dependency */ }
    #[test]
    fn add_with_wrong_suite_or_garbage_key_package_fails() { /* corrupted KeyPackage bytes refuse to decode/add */ }
    #[test]
    fn self_update_rotates_the_epoch_without_membership_change() { /* rekey cadence primitive: epoch+1, members unchanged, authenticator changed */ }
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p farder-mls group` (types stubbed with `bail!` bodies compile, tests fail).
- [ ] **Step 3: Implement** `MlsChannelGroup` + `verify_declared_matches_actual` per the interfaces.
- [ ] **Step 4: Run to green** — `cargo test -p farder-mls group`, then `cargo test -p farder-mls` (nothing else broken).
- [ ] **Step 5: Commit**

```bash
git add crates/farder-mls/src/
git commit -m "feat(mls): channel group ops with epoch-authenticator/tree-hash chaining and declared-vs-actual validation"
```

---

## Task 4: Message sealing — envelope, padding ladder, seal/open, FS observation tests

**Files:**
- Create: `crates/farder-mls/src/envelope.rs`; modify `lib.rs` (`pub mod envelope;`), `group.rs` (seal/open methods)

**Interfaces:**
- Produces:
  - `#[derive(Serialize, Deserialize, PartialEq, Debug)] pub struct MessageEnvelope { pub content: String, pub attachment_keys: Vec<[u8; 32]>, pub filenames: Vec<String>, pub mimes: Vec<String> }` — rmp-serde canonical bytes (the exact sealed body named by spec §Wire formats / `MessagePostedE2ee`).
  - `pub fn pad_to_bucket(bytes: &[u8]) -> anyhow::Result<Vec<u8>>` — output is `u32_be(len) || bytes || 0x00…` sized to the smallest `PADDING_BUCKETS` entry ≥ `len + 4`; `Err` if it exceeds the 40 KiB top bucket (**refuse, never truncate**).
  - `pub fn unpad(padded: &[u8]) -> anyhow::Result<Vec<u8>>` — strict: total length must be exactly one of the buckets, prefix length must fit, tail need not be zero-checked (padding is inside the AEAD, integrity comes from MLS).
  - `pub fn check_preseal_limits(envelope: &MessageEnvelope, encoded_len: usize) -> anyhow::Result<()>` — `content.chars().count() <= MAX_CONTENT_CHARS` **and** `encoded_len <= MAX_PRESEAL_BYTES` (the client-rule half of the spec's Size-caps row; the 40 KiB ciphertext cap is ingest's, sub-3).
  - On `MlsChannelGroup`: `seal_message(provider, signer, envelope: &MessageEnvelope) -> Result<Vec<u8>>` (encode → limits → pad → `create_message` → serialized `MlsMessageOut`, a PrivateMessage) and `open_message(provider, bytes: &[u8]) -> Result<MessageEnvelope>` (`process_message` → `ApplicationMessage` → unpad → decode).

- [ ] **Step 1: Write the failing tests:**

```rust
    #[test]
    fn padded_sizes_land_exactly_on_the_bucket_ladder() { /* representative lens incl. 0, 251, 252, 253, 1020, 40956 → exact bucket sizes; 40957+ → Err */ }
    #[test]
    fn oversize_content_is_refused_not_truncated() { /* >8000 chars or >32KiB encoded → seal_message Err; envelope unchanged */ }
    #[test]
    fn sealed_message_roundtrips_between_members() { /* alice seals (content + one attachment key/filename/mime); bob opens; envelopes equal */ }
    #[test]
    fn sealed_bytes_contain_no_plaintext_substring() { /* OBSERVATION (CLAUDE.md): the serialized wire bytes contain neither the content bytes, the filename, the mime, nor the attachment key */ }
    #[test]
    fn two_envelopes_in_the_same_bucket_seal_to_equal_length_ciphertexts() { /* "yes" vs a 200-char paragraph: identical ciphertext lengths — the length-oracle fix */ }
    #[test]
    fn removed_member_cannot_decrypt_post_removal() { /* FS OBSERVATION: remove carol, everyone merges; alice seals; carol.open_message errs */ }
    #[test]
    fn joiner_cannot_decrypt_pre_join_messages() { /* FS OBSERVATION: alice+bob exchange sealed msgs; carol joins later; carol cannot open the earlier bytes */ }
    #[test]
    fn tampered_ciphertext_and_wrong_group_fail_to_open() { /* bit-flip → Err; a second unrelated group's member → Err */ }
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p farder-mls envelope` (pad/unpad stubbed).
- [ ] **Step 3: Implement** pad/unpad/limits and the group seal/open methods.
- [ ] **Step 4: Run to green** — `cargo test -p farder-mls`.
- [ ] **Step 5: Commit**

```bash
git add crates/farder-mls/src/
git commit -m "feat(mls): sealed message envelope with padding bucket ladder + FS observation tests"
```

---

## Task 5: Sqlite storage wiring — store instance binding and no-resume

**Files:**
- Create: `crates/farder-mls/src/store.rs`; modify `lib.rs` (`pub mod store;`)
- Create: `docs/modules/farder-mls.md`; modify `ARCHITECTURE.md`

**Interfaces:**
- Produces:
  - `pub struct RmpCodec;` implementing `openmls_sqlite_storage::Codec` via rmp-serde.
  - `pub struct FarderMlsStore { … }` — owns the `rusqlite::Connection`, a `SqliteStorageProvider<RmpCodec, Connection>` (with `run_migrations()` applied), and `openmls_rust_crypto::RustCrypto`; implements `openmls_traits::OpenMlsProvider` so every Task-2/3/4 API takes it interchangeably with the in-memory test provider.
  - `pub fn create(db_path: &Path) -> anyhow::Result<(Self, [u8; 16])>` — fails if the file already holds a `farder_store_meta` row; generates a random 16-byte `store_instance_id` (`rand::rngs::OsRng`), persists it in a side table `farder_store_meta(instance_id BLOB NOT NULL)`, returns it. Callers (sub-2/4) publish `store_instance_hash` in `MlsKeyPackagePublished` and carry it on commits.
  - `pub fn store_instance_hash(&self) -> [u8; 32]` — SHA-256 of the raw 16-byte id (what the log carries; the raw id never leaves the store).
  - `pub fn resume(db_path: &Path, expected_instance_hash: &[u8; 32]) -> Result<Self, StoreResumeError>` with `pub enum StoreResumeError { InstanceMismatch, MissingInstanceId, Io(anyhow::Error) }` — **`InstanceMismatch`/`MissingInstanceId` are terminal for this store: the caller must self-`DeviceRevoked` and re-provision (sub-5 behavior); this crate never silently re-creates or resumes** (spec §MLS store safety rules 1–2; rule 3, directory placement/exclusion, is the client crate's job in sub-4).

- [ ] **Step 1: Write the failing tests** (use `tempfile`-style paths under `std::env::temp_dir()`):

```rust
    #[test]
    fn fresh_store_generates_a_random_instance_id_and_publishable_hash() { /* two creates → different ids; hash == sha256(id); create on an existing store file errs */ }
    #[test]
    fn resume_with_matching_hash_restores_group_state() { /* create store; create group + seal one message; drop; resume with the right hash; group loads (MlsGroup::load) and can still seal/open with a memory-store peer */ }
    #[test]
    fn store_instance_mismatch_refuses_to_resume() { /* NO-RESUME (spec C6): resume with a different expected hash → StoreResumeError::InstanceMismatch; no fallback store is created */ }
    #[test]
    fn resume_of_a_store_without_instance_metadata_is_refused_not_recreated() { /* raw sqlite file with the meta row deleted → MissingInstanceId (poisoned-store posture, never resume the ratchet) */ }
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p farder-mls store`.
- [ ] **Step 3: Implement** `RmpCodec`, `FarderMlsStore`, `create`/`resume`/`store_instance_hash`.
- [ ] **Step 4: Run to green + full sweep** — `cargo test -p farder-mls`, then `cargo test --workspace` (note per project memory: the Tauri client crate builds separately, but nothing here touches it yet).
- [ ] **Step 5: Docs** — write `docs/modules/farder-mls.md` (public surface: constants, credential fns, `MlsChannelGroup` methods, envelope fns, `FarderMlsStore`, and the no-resume contract) and add the crate + "client-side only; server never links MLS" line to `ARCHITECTURE.md`.
- [ ] **Step 6: Commit**

```bash
git add crates/farder-mls/src/ docs/modules/farder-mls.md ARCHITECTURE.md
git commit -m "feat(mls): sqlite store with instance binding + no-resume; farder-mls module docs"
```

---

## Self-Review

**Spec §Sub-projects item 1 coverage:**
- Wraps openmls 0.8.1: create/join(Welcome)/add/remove/self-update → Task 3. ✅
- Seal/open application messages + **padding ladder** → Task 4 (`PrivateMessage`, buckets 256 B–40 KiB, refuse-over-40 KiB). ✅
- Device-subkey signer adapter + length-prefixed credential encoding → Task 2 (`DeviceSigner`, `"farder-mls-cred-v1"` u8/u32-prefixed encoding, strict decode). ✅
- Exposure of `epoch_authenticator`/`tree_hash` for chaining → Task 3 (`CommitOutcome.prev_epoch_authenticator`/`post_tree_hash`, accessors, `JoinInfo`). ✅
- Validation helpers members run against fold state → Tasks 2–3 (`verify_leaf_binding`, `verify_declared_matches_actual`) — fold-status/authenticator-chain rules themselves are sub-2, as decomposed by the spec. ✅
- Sqlite storage wiring with **`store_instance_id` + no-resume** → Task 5. ✅
- Envelope (de)serialization → Task 4 (`MessageEnvelope` rmp bytes). ✅
- Spec's test list → named tests: three-device groups (T3), add/remove/rekey (T3), FS observation both directions (T4), declared-vs-actual + tree-hash mismatch (T3), padding buckets (T4 + equal-length observation), wrong-suite/wrong-key failures (T3/T4), store-instance mismatch refuses to resume (T5). ✅

**Pinned-version stop condition:** discharged up front — 0.8.1 resolved and built on rustc 1.94.1 before this plan was written; Task 1 re-verifies inside the workspace and STOPs on failure rather than improvising. ✅

**Out-of-scope discipline:** no `EventPayload` variants, no fold state, no ingest, no Tauri/UI, no `AuthzBeacon`/head-attestation logic (those consume this crate's seal/open in sub-2+), no `spawn_blocking` policy, no non-portable-directory placement (sub-4). ✅

**Consistency:** every OpenMLS symbol named here was grep-verified in the vendored 0.8.1 source; `CommitOutcome`/`JoinInfo` fields line up one-to-one with the spec's `MlsCommit`/`MlsLeafConfirmed` event fields sub-2 will define; `DeclaredMember` matches the spec's `DeclaredAdd`/`DeclaredRemove` `{ identity, device }` shape (the `key_package: EventRef` half of `DeclaredAdd` is log-layer data, sub-2). ✅
