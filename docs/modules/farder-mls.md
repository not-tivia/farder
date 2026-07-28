# farder-mls

> **File(s):** `crates/farder-mls/src/lib.rs`, `crates/farder-mls/src/credential.rs`
> **Layer:** Crypto crate (client-side only — the server NEVER links this crate)
> **Last reviewed:** 2026-07-28

## Purpose

Pure-Rust wrapper around **OpenMLS 0.8.1** for Farder's E2EE channel groups
(mesh rung 2, sub-project 1). It will own group lifecycle (create / join from
Welcome / add / remove / self-update), message sealing with a padding bucket
ladder, the device-subkey signer adapter, and sqlite storage with store-instance
binding. It deliberately does NOT define protocol events, fold rules, ingest
checks, or any server/client wiring — those are later sub-projects that consume
this crate's values.

**Status:** constants + credential binding (`credential.rs`). Group ops,
envelopes, and storage land task-by-task per
`docs/superpowers/plans/2026-07-27-mesh-rung2-sub1-mls-core.md`.

---

## Public interface

### `CIPHERSUITE: Ciphersuite`

The single ciphersuite every Farder group uses:
`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (RFC 9420's mandatory-to-implement
suite: X25519 HPKE, AES-128-GCM, SHA-256, Ed25519 signatures). Ed25519 matches
`farder-crypto`'s device subkeys, which will double as MLS leaf signature keys.

### `PADDING_BUCKETS: [usize; 5]` = `[256, 1024, 4096, 16384, 40960]`

Byte sizes of the padding ladder applied to the **plaintext envelope before
sealing**, so ciphertext length is not a plaintext-length oracle. An envelope
that would exceed the top 40 KiB bucket is refused, never truncated. (OpenMLS's
own `padding_size` is a modulus and stays 0; bucket padding is this crate's.)

### `MAX_PAST_EPOCHS: usize` = `3`

How many past epochs' decryption secrets a member retains for late-arriving
application messages (spec contract for group config).

### `MAX_PRESEAL_BYTES: usize` = `32768` / `MAX_CONTENT_CHARS: usize` = `8000`

Client-side pre-seal caps (spec "Size caps" row): content ≤ 8000 chars AND the
encoded envelope ≤ 32 KiB. The complementary 40 KiB *ciphertext* cap is
server-ingest's job (sub-3), not this crate's.

### `credential` module — leaf ↔ device-subkey binding

- `encode_credential_identity(identity: &PublicKey, device: &DeviceId) -> Vec<u8>`
  — the **normative** (spec M1) credential identity bytes:
  `"farder-mls-cred-v1" || u8(len(pubkey)) || pubkey || u32_be(len(device_id)) || device_id`.
  Bare concatenation is forbidden (`DeviceId` is a hex `String`, ambiguous
  unprefixed).
- `decode_credential_identity(bytes: &[u8]) -> Result<(PublicKey, DeviceId)>`
  — **strict** decode: wrong prefix, truncation, trailing bytes, or a pubkey
  length ≠ 32 are all errors.
- `DeviceSigner<'a>(pub &'a Keypair)` — `openmls_traits::signatures::Signer`
  adapter; the MLS leaf signs with the same Ed25519 device subkey that signs
  log events (`signature_scheme()` = ED25519).
- `generate_key_package(provider, device: &Keypair, identity: &PublicKey) -> Result<KeyPackageBundle>`
  — builds a KeyPackage under `CIPHERSUITE` whose leaf credential is the
  encoded `(identity, device_id)` and whose leaf signature key is the device
  subkey.
- `verify_leaf_binding(credential: &Credential, leaf_signature_key: &[u8], cert: &DeviceCert) -> Result<()>`
  — checks the *cryptographic* binding only: credential identity/device match
  the cert, leaf key == certified `device_pubkey`, and `cert.verify()` passes.
  Fold-status checks (membership, revocation, expiry) are sub-2's job — callers
  pass a cert they already trust per fold state.

---

## Dependency pinning (deliberate)

`openmls =0.8.1`, `openmls_rust_crypto =0.5.1`, `openmls_traits =0.5.0`,
`openmls_sqlite_storage =0.2.0` are all `=`-pinned: OpenMLS is pre-1.0 and each
minor release has broken APIs. Do not bump these casually — the wrapper surface
was written against these exact versions.

## Integration map

- **farder-crypto** — supplies `Keypair`/`PublicKey`/`DeviceCert`/`device_id`;
  the MLS leaf will sign with the same Ed25519 device subkey that signs log
  events (judged safe: disjoint signed-byte domains).
- **Workspace note:** `openmls_sqlite_storage` requires `rusqlite 0.32`; because
  cargo allows only one `links = "sqlite3"` native lib per graph, the whole
  workspace (`farder-node`, `farder-notify`, `farder-server`) is harmonized on
  rusqlite 0.32.

## Known gotchas

- Everything here is synchronous plain-library code; async callers must use
  `spawn_blocking` (caller policy, not this crate's).
- The server must never grow a dependency on this crate — E2EE group keys are
  client-only by design.
