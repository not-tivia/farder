# farder-crypto

> **File(s):** `crates/farder-crypto/src/identity.rs`, `key_exchange.rs`, `encryption.rs`, `media.rs`, `recovery.rs`, `event_log.rs`, `event_log_state.rs`, `lib.rs`
> **Layer:** Crypto crate
> **Last reviewed:** 2026-07-28

## Purpose

`farder-crypto` is the security core of Farder. It owns three things: (1) **cryptographic identity** — Ed25519 keypairs, signing, verification, and passphrase-protected key serialisation; (2) **DM end-to-end encryption** — X25519 ECDH key exchange derived from the Ed25519 identity keys, plus AES-256-GCM encrypt/decrypt; and (3) **voice media frame crypto** — per-call ChaCha20-Poly1305 stream-key generation, per-peer key wrapping, and AEAD seal/open of individual Opus frames. It does not implement the transport, protocol framing, or any business logic.

---

## Public interface

### `identity.rs` — `Keypair`

#### `Keypair::generate() -> Keypair`

**What it does:** generates a fresh Ed25519 keypair using the OS CSPRNG (`OsRng`).
**Returns:** a new `Keypair`. Never fails.
**Side effects:** none (purely in-memory).
**Connects to:** called by `commands::generate_keypair` (Tauri command) when a user creates a new Farder identity. Also used directly in `farder-server::auth` integration tests.

---

#### `Keypair::public_key(&self) -> PublicKey`

**What it does:** extracts the Ed25519 verifying key as a `PublicKey`.
**Returns:** a `PublicKey` (32-byte compressed Edwards point).
**Side effects:** none.
**Connects to:** used everywhere an identity must be shared — `commands.rs` `get_public_key`, `farder-node::PersonalNode::public_key`, `farder-server::auth`.

---

#### `Keypair::sign(&self, message: &[u8]) -> Vec<u8>`

**What it does:** produces a 64-byte Ed25519 signature over `message`.
**Parameters:** `message` — arbitrary bytes; typically a server-issued challenge nonce during QUIC auth handshake.
**Returns:** raw signature bytes (64 bytes).
**Side effects:** none.
**Connects to:** `client/src-tauri/src/connection.rs` calls `keypair.sign(&nonce)` during server authentication; `farder-server::auth` integration tests call it on the client side of the challenge-response.

---

#### `Keypair::signing_key_bytes(&self) -> &[u8; 32]`

**What it does:** returns a reference to the raw 32-byte Ed25519 signing-key scalar.
**Returns:** `&[u8; 32]`.
**Side effects:** none.
**Connects to:** `commands.rs` stores this in `AppState::signing_key_bytes` (a `Mutex<Option<[u8; 32]>>`). The raw bytes — not the `Keypair` struct — are what the Tauri app state actually holds, because `Keypair` is not `Send`+`Sync`. They are re-hydrated to a `Keypair` on demand via `Keypair::from_signing_key_bytes`. Also consumed by `voice/mod.rs` to derive stream-key wrapping secrets.

---

#### `Keypair::from_signing_key_bytes(bytes: &[u8; 32]) -> Keypair`

**What it does:** reconstructs a `Keypair` from a stored 32-byte signing scalar.
**Parameters:** `bytes` — raw Ed25519 signing key, as written to disk by `generate_keypair`.
**Returns:** `Keypair`. Never fails (Ed25519 accepts all 32-byte scalars as valid signing keys).
**Connects to:** `commands::load_identity` reads 32 bytes from disk and calls this.

---

#### `Keypair::export_encrypted(&self, passphrase: &str) -> Result<Vec<u8>>`

**What it does:** serialises the signing key as `salt(16) || nonce(12) || AES-256-GCM(signing_key)`, where the AES key is derived from `passphrase` via Argon2id (64 MiB memory, 3 iterations, 1 lane), configured by the shared `identity_kdf()` helper so encrypt and decrypt always agree. The derived key is zeroized from the stack after use.
**Parameters:** `passphrase` — user-supplied string; any length.
**Returns:** 76-byte blob on success (`16 + 12 + 32 plaintext + 16 GCM tag`), or an Argon2/AES error.
**Side effects:** none (caller is responsible for writing the blob to disk).
**Connects to:** `IdentityStore::seal_new` (`client/src-tauri/src/identity.rs`), the
key-at-rest path behind `create_identity`/`migrate_plaintext_identity`/`restore_identity`.

---

#### `Keypair::import_encrypted(data: &[u8], passphrase: &str) -> Result<Self>`

**What it does:** reverses `export_encrypted` — derives the AES key from `passphrase`, decrypts the signing key, and reconstructs the `Keypair`. Returns an error (not a panic) if the passphrase is wrong (AES-GCM authentication fails) or the blob is malformed.
**Parameters:** `data` — the full blob from `export_encrypted`; `passphrase` — must match the one used to export.
**Returns:** `Keypair` on success; anyhow error otherwise.
**Side effects:** zeroizes the Argon2id-derived key from the stack after use.

---

### `identity.rs` — `PublicKey`

#### `PublicKey::from_bytes(bytes: [u8; 32]) -> PublicKey`

**What it does:** wraps a raw 32-byte Ed25519 verifying key. Does NOT validate the point; use `verify` to surface invalid-key errors at verification time.
**Connects to:** used in `farder-protocol` message deserialization and throughout `farder-server`.

---

#### `PublicKey::as_bytes(&self) -> &[u8; 32]`

**What it does:** returns the underlying 32-byte compressed Edwards point.
**Connects to:** `key_exchange::derive_dm_shared_secret` and `media::wrap_stream_key_for_peer` / `unwrap_stream_key` take the bytes directly.

---

#### `PublicKey::verify(&self, message: &[u8], signature: &[u8]) -> Result<()>`

**What it does:** verifies a 64-byte Ed25519 `signature` over `message` using this key. Returns `Ok(())` if valid; an anyhow error otherwise.
**Parameters:** `message` — the original bytes that were signed; `signature` — 64 bytes produced by `Keypair::sign`.
**Returns:** `Ok(())` or an error describing the failure (invalid key bytes, invalid signature encoding, or verification failure).
**Connects to:** `farder-server::auth::verify_challenge_response` calls this to authenticate connecting clients.

---

#### `PublicKey` — `Display` (`to_string()`)

**What it does:** formats the key as `vk_<64 hex chars>`, e.g. `vk_3b6a27bcc...`.
**Connects to:** `bridge.rs` emits public keys via `.to_string()` in Tauri events (e.g. `server:new_message`, `server:member_joined`). The TypeScript side receives this as a plain string and compares/stores it as `"vk_<hex>"`. See **Known gotchas** below.

---

#### `PublicKey` — `Serialize` / `Deserialize` (serde)

**What it does:** serializes as `{ "bytes": [b0, b1, ..., b31] }` — a JSON object with a `bytes` array, NOT a string.
**Connects to:** `farder-protocol` uses serde to encode `PublicKey` inside `ServerEvent` and `ServerResponse` payloads. TypeScript types in `client/src/lib/types.ts` model this as `{ bytes: number[] }`. Use `publicKeyToString()` (`client/src/lib/types.ts`) to convert to the `"vk_<hex>"` string form before string comparison or passing as a Tauri command argument.

---

### `key_exchange.rs` — X25519 ECDH helpers

#### `derive_dm_shared_secret(our_ed_sk: &[u8; 32], their_ed_pk: &[u8; 32]) -> Result<[u8; 32], &'static str>`

**What it does:** derives a 32-byte symmetric secret for use as an AES-256-GCM key in DM encryption. It converts both the local Ed25519 signing key and the remote Ed25519 verifying key to their X25519 (Curve25519 Montgomery-form) equivalents via the standard birational map used by libsodium/Signal, then performs X25519 Diffie-Hellman. The secret is symmetric: `A.derive(their=B) == B.derive(their=A)`.
**Parameters:** `our_ed_sk` — raw 32-byte Ed25519 signing key scalar; `their_ed_pk` — raw 32-byte Ed25519 verifying key.
**Returns:** 32-byte raw X25519 shared secret on success; a `&'static str` error if `their_ed_pk` is not a valid compressed Edwards point.
**Side effects:** none (no state mutation, no I/O).
**Connects to:** called by `commands::dm_encrypt` and `commands::dm_decrypt` (the Tauri DM E2EE path), and by `media::wrap_stream_key_for_peer` / `media::unwrap_stream_key` (the voice key-wrapping path).

---

#### `ed25519_sk_to_x25519(ed_sk: &[u8; 32]) -> StaticSecret`

**What it does:** converts an Ed25519 signing-key scalar to an X25519 `StaticSecret` using SHA-512 + RFC 7748 clamping — the same derivation as `libsodium::crypto_sign_ed25519_sk_to_curve25519`. This is a deterministic, one-way transformation.
**Returns:** X25519 `StaticSecret`.
**Connects to:** called only by `derive_dm_shared_secret`.

---

#### `ed25519_pk_to_x25519(ed_pk: &[u8; 32]) -> Result<PublicKey, &'static str>`

**What it does:** converts an Ed25519 verifying key (compressed Edwards Y point) to an X25519 `PublicKey` via the birational map (Edwards → Montgomery). This is the same transformation used by Signal and libsodium.
**Returns:** X25519 `PublicKey` on success; `"invalid ed25519 public key bytes"` or `"failed to decompress ed25519 public key"` if the input is not a valid compressed Edwards point.
**Connects to:** called only by `derive_dm_shared_secret`.

---

#### `SessionKeypair` (struct)

A fresh ephemeral X25519 keypair used in the farder-node session-key-exchange protocol (the older `PersonalNode`-level DM path, distinct from the Tauri client DM path). Not used for voice.

#### `SessionKeypair::generate() -> SessionKeypair`

**What it does:** generates a new ephemeral X25519 keypair.

#### `SessionKeypair::derive_shared_secret(&self, peer_public: &x25519_dalek::PublicKey) -> [u8; 32]`

**What it does:** performs X25519 DH with `peer_public`, returning the raw 32-byte shared secret.
**Connects to:** `farder-node::PersonalNode::complete_key_exchange` calls this to seed the per-peer AES key used by `encryption::encrypt`/`decrypt` in the node-level DM path.

---

### `encryption.rs` — AES-256-GCM generic encrypt/decrypt

#### `encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>>`

**What it does:** encrypts `plaintext` with AES-256-GCM using a freshly generated random 12-byte nonce. Output wire layout: `nonce(12) || ciphertext || GCM-tag(16)`. Total output length is `plaintext.len() + 28`.
**Parameters:** `key` — 32-byte AES-256 key (typically a raw X25519 shared secret); `plaintext` — arbitrary bytes.
**Returns:** the full blob on success; anyhow error if cipher initialisation fails (practically impossible with a 32-byte key).
**Side effects:** calls `rand::random()` for the nonce.
**Connects to:** called by `commands::dm_encrypt` (DM E2EE Tauri path), `farder-node::PersonalNode::prepare_dm` (node-level DM path), and `media::wrap_stream_key_for_peer` (voice key-wrapping path).

---

#### `decrypt(key: &[u8; 32], data: &[u8]) -> Result<Vec<u8>>`

**What it does:** decrypts a blob produced by `encrypt`. Reads the nonce from the first 12 bytes, authenticates and decrypts the remainder. Returns an error — not a panic — on wrong key, wrong nonce, or tampered ciphertext (GCM authentication failure).
**Parameters:** `key` — same 32-byte key used to encrypt; `data` — full output from `encrypt` (minimum 28 bytes).
**Returns:** plaintext bytes on success; anyhow error on authentication failure or truncated input.
**Connects to:** called by `commands::dm_decrypt`, `farder-node::PersonalNode::receive_dm`, and `media::unwrap_stream_key`.

---

### `media.rs` — Voice stream-key lifecycle

#### `derive_stream_key() -> [u8; 32]`

**What it does:** generates a cryptographically random 32-byte key for use as a ChaCha20-Poly1305 stream key. This key is generated ONCE per (session, track) and then distributed to all peers individually via `wrap_stream_key_for_peer`.
**Returns:** 32 random bytes.
**Side effects:** calls `rand::random()`.
**Connects to:** `voice/mod.rs` calls this when the local client joins or re-keys a voice channel.

---

#### `wrap_stream_key_for_peer(stream_key: &[u8; 32], my_ed_sk: &[u8; 32], peer_ed_pk: &[u8; 32]) -> Result<Vec<u8>>`

**What it does:** encrypts `stream_key` (32 bytes) for exactly one peer by deriving the per-pair AES key via `derive_dm_shared_secret`, then calling `encryption::encrypt`. The output is a 60-byte blob (`nonce(12) || stream_key(32) || tag(16)`). Only the intended peer can unwrap it because only they can reproduce the same shared secret from the other side.
**Parameters:** `my_ed_sk` — our 32-byte Ed25519 signing-key scalar; `peer_ed_pk` — the peer's 32-byte Ed25519 verifying key; `stream_key` — the key to wrap.
**Returns:** 60-byte wrapped key on success; anyhow error if the peer key is invalid.
**Connects to:** `voice/mod.rs` calls this once per connected peer when generating a new stream key. The resulting blob is sent to each peer as the `wrapped_key` field in a `StreamKeyOffer` server event.

---

#### `unwrap_stream_key(wrapped: &[u8], my_ed_sk: &[u8; 32], sender_ed_pk: &[u8; 32]) -> Result<[u8; 32]>`

**What it does:** decrypts a `wrapped_key` blob received in a `StreamKeyOffer` event. Derives the shared secret from our signing key and the sender's verifying key, then calls `encryption::decrypt`, and validates the decrypted length is exactly 32 bytes.
**Parameters:** `wrapped` — the 60-byte blob from `wrap_stream_key_for_peer`; `my_ed_sk` — our 32-byte signing key scalar; `sender_ed_pk` — the sender's raw 32-byte verifying key (from the `sender` field of the `StreamKeyOffer` event).
**Returns:** the raw 32-byte stream key on success; anyhow error if the secret derivation fails, decryption fails (wrong key / tampered), or the decrypted length is unexpected.
**Connects to:** `voice/mod.rs::on_stream_key_offer` calls this when the bridge delivers a `StreamKeyOffer` event.

---

### `media.rs` — Per-frame AEAD (ChaCha20-Poly1305)

#### `seal_media_frame(key, seq, session_id, header_aad, speaker_pk, codec_payload) -> Result<Vec<u8>>`

**What it does:** encrypts `speaker_pk(32) || codec_payload` under `key` using ChaCha20-Poly1305. The AEAD nonce is derived deterministically from `session_id` and `seq` (see `media_frame_nonce` below). `header_aad` — the 28-byte frame header — is bound to the ciphertext as additional authenticated data; any tampering of version, type, track ID, seq, or session ID is detected on open.
**Parameters:** `key` — 32-byte ChaCha20-Poly1305 stream key; `seq` — monotonically increasing u64 frame counter; `session_id` — 16-byte session identifier; `header_aad` — the 28-byte frame header (use `media_frame_header_aad`); `speaker_pk` — 32-byte raw verifying key of the speaker; `codec_payload` — encoded audio/video bytes.
**Returns:** ciphertext including the 16-byte Poly1305 tag on success; anyhow error on AEAD failure.
**Connects to:** called by `seal_audio_packet_to_wire` (see below), which is the hot-path used by `voice/send.rs::SendTask`.

---

#### `open_media_frame(key, seq, session_id, header_aad, ciphertext) -> Result<([u8; 32], Vec<u8>)>`

**What it does:** decrypts and authenticates a ciphertext produced by `seal_media_frame`. Reconstructs the same deterministic nonce from `session_id` + `seq` and verifies the AAD.
**Returns:** `(speaker_pk, codec_payload)` on success; anyhow error on authentication failure, wrong key, mismatched seq/session_id, or tampered AAD.
**Connects to:** called by `open_audio_wire_frame` (see below), used by `voice/recv.rs::RecvTask`.

---

#### `seal_audio_packet_to_wire(key, seq, session_id, speaker_pk, opus_packet) -> Result<Vec<u8>>`

**What it does:** one-shot hot-path helper: builds the 28-byte audio frame header (version `0x02`, type `0x01 AUDIO`, track_id `0`, codec_id `0`, seq big-endian, session_id), then calls `seal_media_frame` with that header as AAD, and concatenates `header || ciphertext`. Wire layout: `header(28) || ciphertext+tag`.
**Returns:** complete wire frame bytes on success.
**Connects to:** `voice/send.rs::SendTask` calls this in the hot encode→encrypt→send loop.

---

#### `open_audio_wire_frame(key, wire) -> Result<(u64, [u8; 32], Vec<u8>)>`

**What it does:** one-shot receive helper — parses the 28-byte header from `wire`, validates version and type bytes, extracts `seq` and `session_id`, then calls `open_media_frame`. Returns `(seq, speaker_pk, opus_packet)`.
**Returns:** `(seq, speaker_pk, opus_packet)` on success; anyhow error if the frame is too short, has an unexpected version/type, or AEAD authentication fails.
**Connects to:** `voice/recv.rs::RecvTask` calls this on every inbound datagram.

---

### `media.rs` — Nonce / AAD helpers

#### `media_frame_nonce(session_id: &SessionId, seq: u64) -> [u8; 12]`

**What it does:** constructs the deterministic 12-byte nonce: `session_id[0..4] || seq.to_be_bytes()`. Unique by construction as long as `seq` is monotonic within a session; u64 wraps after ~1.8 × 10^19 frames (practically never).
**Connects to:** called by `seal_media_frame` and `open_media_frame`; not normally called directly.

---

#### `media_frame_header_aad(buf: &[u8]) -> &[u8]`

**What it does:** returns `&buf[..28]` — a slice of the first `MEDIA_FRAME_HEADER_LEN` bytes. A thin helper to make the AAD extraction explicit at call sites.
**Connects to:** used internally by `open_audio_wire_frame`; available to the server-side `farder-server::media_stream` as well.

---

### `recovery.rs` — BIP39 recovery phrase encoding

#### `recovery::phrase_from_key(key: &[u8; 32]) -> Result<String>`

**What it does:** encodes a 32-byte Ed25519 signing key as a 24-word BIP39
mnemonic phrase. The phrase encodes the key itself directly, so it is as
sensitive as the raw key — treat it with the same secrecy.
**Parameters:** `key` — the 32-byte signing-key scalar.
**Returns:** space-joined 24-word BIP39 phrase on success; anyhow error if
encoding fails.
**Side effects:** none (purely in-memory).
**Connects to:** called by `IdentityStore` (`client/src-tauri/src/identity.rs`)
whenever a new or migrated identity is created, to produce the recovery phrase
returned to the frontend by `create_identity`, `migrate_plaintext_identity`, and
`restore_identity`.

---

#### `recovery::key_from_phrase(phrase: &str) -> Result<[u8; 32]>`

**What it does:** decodes a 24-word BIP39 mnemonic phrase back to the 32-byte
Ed25519 signing key. Validates checksum, rejects unknown words, and checks that
the decoded length is exactly 32 bytes.
**Parameters:** `phrase` — space-separated 24-word BIP39 string.
**Returns:** the raw 32-byte signing key on success; anyhow error on bad
checksum, unknown words, or wrong length (error surfaces as `InvalidPhrase` to
the Tauri caller in `restore_identity`).
**Side effects:** none.
**Connects to:** called by `IdentityStore::restore` inside
`restore_identity`.

---

## State it owns

This crate is stateless — it has no global state and no persistent handles. All inputs and outputs are plain values or byte slices.

## Events emitted

None. This is a pure-function library crate.

## Events / requests consumed

None directly. Callers pass values in and receive values out.

---

## Integration map

- **`identity.rs` (`IdentityStore`) in `client/src-tauri`** — `create_identity`, `migrate_plaintext_identity`, and `restore_identity` call `recovery::phrase_from_key` to produce BIP39 recovery phrases; `restore_identity` calls `recovery::key_from_phrase` to rebuild the key.
- **`commands.rs`** (DM E2EE path) — `dm_encrypt`/`dm_decrypt` Tauri commands call `key_exchange::derive_dm_shared_secret` then `encryption::encrypt`/`decrypt`. The shared secret is derived fresh on every call; it is not cached in `AppState`.
- **`farder-node::PersonalNode`** (node-level DM path) — uses `SessionKeypair::derive_shared_secret` for an older ephemeral-key exchange; stores the secret in `PeerManager`. This path is separate from, and independent of, the Tauri command DM path.
- **`voice/mod.rs`** (`VoiceController`) — calls `derive_stream_key` + `wrap_stream_key_for_peer` when generating a per-call key offer, and `unwrap_stream_key` in `on_stream_key_offer` when receiving one from a peer.
- **`voice/send.rs`** (`SendTask`) — calls `seal_audio_packet_to_wire` on every Opus packet in the hot send loop.
- **`voice/recv.rs`** (`RecvTask`) — calls `open_audio_wire_frame` on every inbound datagram.
- **`farder-server::auth`** — calls `PublicKey::verify` to authenticate the client challenge-response during QUIC handshake; calls `Keypair::sign` in tests.
- **`client/src/lib/types.ts`** — models `PublicKey` as `{ bytes: number[] }` when received via serde, and provides `publicKeyToString()` to convert to `"vk_<hex>"` form for string comparison and React key use.

---

## Known gotchas

### Dual `PublicKey` representation across the TS boundary

`PublicKey` has TWO different representations that are both in active use and must not be mixed up:

| Context | Form | Example |
|---|---|---|
| Serde (JSON, protocol messages, `ServerEvent` payloads) | `{ "bytes": [0, 1, …, 31] }` | `{ bytes: [59, 106, …] }` |
| Display / Tauri events emitted from `bridge.rs` / command return values | `"vk_<64 hex chars>"` | `"vk_3b6a27bc…"` |

TypeScript types in `types.ts` use `{ bytes: number[] }` for the serde form. Call `publicKeyToString(pk)` before passing a key as a Tauri command argument (e.g. `dm_encrypt`'s `theirPublicKey` param) or using it as a React key or for string comparison with keys already stored as `"vk_…"`. Mixing the two forms silently produces wrong results — comparisons always fail, and command calls produce a parse error on the Rust side.

### The DM shared secret is the raw X25519 output — no KDF

`derive_dm_shared_secret` returns the raw X25519 output bytes and uses them directly as the AES-256-GCM key in `dm_encrypt`/`dm_decrypt`. There is no HKDF or domain-separation step. This is sufficient for current use but means the same bytes are reused across every DM message between a given pair of users. If DM forward-secrecy or per-message key derivation is ever needed, this is the place to add it.

### `AppState` stores signing-key bytes, not a `Keypair`

`Keypair` is not `Send + Sync`, so `AppState` stores `Mutex<Option<[u8; 32]>>` (the raw scalar). Every command that needs to sign or derive a shared secret must call `Keypair::from_signing_key_bytes` to reconstruct the `Keypair` on the fly. This is cheap (no crypto work), but it means there is no single "current keypair" object — it is reconstructed at each call site.

### `Keypair::from_signing_key_bytes` never fails — invalid keys are silent

`SigningKey::from_bytes` accepts any 32-byte value without validation. An invalid key will not fail here; it will produce a keypair that signs with garbage or has a mismatched public key. The only protection is that keys written to disk by `generate_keypair` are always valid.

### Voice stream keys are per-call, not per-session

`derive_stream_key` generates a fresh random key. `VoiceController` calls this every time it sends a `StreamKeyOffer` — but whether that is truly once per voice-channel join or repeated during re-keys is controlled by `voice/mod.rs`, not by this crate. The crypto layer itself provides no replay protection beyond the `seq` nonce counter.

---

## `event_log.rs` — the signed event type system

`event_log.rs` defines the data structures that form the mesh server's immutable audit log. All types are `Serialize`/`Deserialize` via `rmp_serde` (MessagePack on the wire, JSON-compatible for test tooling).

### `EventPayload` — variant reference

| Variant | Fields | Authorization rule (enforced by `LogState::apply`) |
|---|---|---|
| `MessagePosted` | `channel_id`, `content`, `reply_to?`, `attachments: Vec<AttachmentCap>` | Any active (non-pending) member |
| `DeviceAuthorized` | `cert: DeviceCert` | Identity matches the cert's identity field |
| `InviteCreated` | `code_hash`, `max_uses`, `expires_at`, `requires_approval` | Owner or member holding `"create_invite"` |
| `MemberJoined` | `member`, `invite` | Invite valid and unused |
| `MemberApproved` | `member` | Owner or member holding `"kick"` |
| `MemberRemoved` | `member` | Owner or member holding `"kick"` |
| `MemberBanned` | `member` | Owner or member holding `"kick"` |
| `MemberUnbanned` | `member` | Owner or member holding `"kick"` |
| `PermissionGranted` | `member`, `capability` | Owner or member who already holds the capability |
| `AttachmentRedacted` | `content_hash: String` | Author is the recorded uploader OR holds `"kick"`; hash must be known; not already redacted |

#### Rung-2 MLS/E2EE variants (DORMANT — nothing emits them yet)

Appended after `AttachmentRedacted` (the enum is **append-only**: existing
variants never change shape or order, so old events' canonical bytes and hashes
are stable — pinned by `existing_variant_bytes_are_untouched_by_new_variants`).
Nothing emits these yet, but **every variant's fold rule is now live in
`LogState::apply`** (sub-2 Tasks 2–4): channel class gating, tombstones and
device revocation (Task 2), the MLS group bookkeeping (Task 3), and the
sealed-content send gates plus non-selective reset (Task 4). Ingest handling
(size caps, `stale-epoch` bounces, derive) is sub-3.

| Variant | Fields | Fold rule |
|---|---|---|
| `ChannelCreated` | `channel_id: u64`, `name`, `kind`, `class: ChannelClass`, `parent: Option<u64>` | Owner-only (spec M3, no new capability string); `channel_id` must be new (class is set once or the channel does not exist to the log — no class-change event exists); `parent: Some(p)` requires `p` known to the log AND the child's `class` equal to `p`'s (thread children inherit their parent's class) |
| `MlsKeyPackagePublished` | `key_package: Vec<u8>`, `store_instance_hash: [u8;32]`, `expires_at_log_pos: u64` | Full member only; `store_instance_hash` must match/pin the device's instance pin; `expires_at_log_pos > log_pos` at publish; at most `MAX_LIVE_KEY_PACKAGES_PER_DEVICE = 10` live (unconsumed, unexpired) refs per device — expired refs stop counting and are pruned on touch. Effect: record ref → expiry log pos |
| `MlsCommit` | `channel_id`, `generation`, `epoch`, `mls_message: Vec<u8>`, `adds: Vec<DeclaredAdd>`, `removes: Vec<DeclaredRemove>`, `prev_epoch_authenticator: [u8;32]`, `post_epoch_authenticator: [u8;32]`, `post_tree_hash: [u8;32]`, `authz_head: EventHash`, `store_instance_hash: [u8;32]` | Channel must have an E2ee group; `generation` must match; **epoch CAS**: a commit re-declaring a past epoch folds `Ok` as an **accepted no-op** (only chain head + `log_pos` advance — the Rung-3-deterministic loser); instance-hash pin; author must hold a **confirmed** leaf and `prev_epoch_authenticator` must equal the previous commit's declared `post_epoch_authenticator` (BOTH exempt for a generation's first commit — bootstrap, its author's leaf becomes confirmed); each `DeclaredAdd` needs a full non-banned member, a live device, an unconsumed unexpired KeyPackage of exactly that `(identity, device)`, an absent leaf, and the **self-add rule** (an identity with a confirmed leaf may only be extended by itself); each `DeclaredRemove` needs a present leaf that is out of good standing OR self-removal; **commit-rate rule** (`COMMIT_RATE_MIN_EPOCH_GAP = 4`): drift-discharging commits are never blocked, others need to be the author's first or ≥ 4 epochs past their previous. `post_epoch_authenticator` is the author's post-merge `epoch_authenticator()` — the value the NEXT commit must chain onto (plan resolved ambiguity #1); a liar wedges the chain and cannot be built upon. `authz_head` is opaque to the fold (client head attestation) |
| `MlsWelcome` | `channel_id`, `generation`, `commit: EventRef`, `for_member: PublicKey`, `for_device: DeviceId`, `welcome: Vec<u8>` | `generation == current`: author must hold a confirmed leaf, target leaf must be pending, cited commit must be fold-recorded. `generation == current + 1`: owner-only reset staging (Welcomes precede the content-addressed reset, ambiguity #6). `for_*` unverifiable by the fold — recording a Welcome NEVER promotes a leaf; only the joiner's confirmation does |
| `MlsLeafConfirmed` | `channel_id`, `generation`, `epoch`, `tree_hash: [u8;32]`, `store_instance_hash: [u8;32]` | Must be authored by the joining device of a **pending** leaf; instance-hash pin; `tree_hash` must equal `commits_by_epoch[epoch].post_tree_hash` — or, in a reset generation with no logged commits, seed/match `reset_expected_tree_hash` (ambiguity #7). Effect: pending → confirmed; clears `reset_pending` once confirmed = members × live devices |
| `MlsGroupReset` | `channel_id`, `new_generation: u64`, `welcomes: Vec<EventRef>` | Owner-only (spec M3); channel must have an E2ee group; `new_generation == generation + 1`; **rate limit** — `channel_events_since_reset >= RESET_MIN_CHANNEL_EVENTS = 1000`, except a channel's FIRST reset (generation 0), which is always allowed; **completeness (spec C7)** — every ref must resolve to a fold-recorded `MlsWelcome` staged for `new_generation`, with no duplicate refs, and the welcomed `(member, device)` set must equal EXACTLY `members × live_devices(event.timestamp)` **minus the resetter's own authoring device** (which becomes the new group's creator). Effect: `generation = new_generation`, `epoch = 1`, `leaves_confirmed = {resetter}`, `leaves_pending = welcomed`, `commit_head`/`epoch_authenticator`/`tree_hash`/`reset_expected_tree_hash` cleared (the cleared authenticator is the new generation's bootstrap marker), `commits_by_epoch` + `last_commit_epoch_by_author` cleared, older-generation `welcomes` dropped, both counters zeroed, `reset_pending = true` |
| `MessagePostedE2ee` | `channel_id`, `generation`, `epoch`, `ciphertext: Vec<u8>`, `reply_to: Option<EventRef>`, `attachments: Vec<AttachmentCap>`, `authz_head: EventHash` | Channel must be known to the log with `class: E2ee` (unknown or Plaintext ⇒ invalid — the other half of class gating); author must be a full member holding a **confirmed** leaf for the signing device; `generation` and `epoch` must both match the group's current values (a stale epoch is invalid — deterministic on replay). **Send gates, all fail closed:** `pending_removals` empty (spec I1), `events_since_last_commit < FRESHNESS_CEILING_EVENTS = 500` (spec C4), `!reset_pending` (spec C7). `authz_head` is opaque to the fold; `reply_to`/caps stay outside the seal for blind threading/validation. Effect: record attachment uploaders (so `AttachmentRedacted` works on sealed posts) + spend one channel event on both counters |
| `MessageEditedE2ee` | `channel_id`, `target: EventRef`, `generation`, `epoch`, `ciphertext: Vec<u8>`, `authz_head: EventHash` | Every gate `MessagePostedE2ee` applies, plus: `target` must not be tombstoned (deletions cannot be resurrected). Target *authorship* is ingest's (sub-3, via the derived view) — the fold has no per-message index by design. Same counter effect |
| `MessageDeleted` | `channel_id`, `target: EventRef`, `reason: DeleteReason` | Channel must be known to the log (log deletes are for log channels); target not already tombstoned; `Moderation` requires `"kick"` (the fold verifies moderation authority itself); `Author` requires membership only — target *authorship* needs a per-message index the fold deliberately omits, so ingest (sub-3) verifies it against the derived `messages` table. Effect: durable tombstone, queryable via `is_tombstoned` — deletions cannot resurrect, and in an E2ee channel it spends one channel event on both freshness counters (a tombstone is content, blind-wise) |
| `DeviceRevoked` | `device: DeviceId` | Device must be known and not already revoked; author must be the owning identity (any of its devices — self-revoke included) or the server owner. Effect: cert dead, chain frozen — history stands (records are kept), new events from the device are rejected at the envelope |

Support types (all in `event_log.rs`):

- `ChannelClass` — `Plaintext | E2ee` (`Copy`, part of channel identity, immutable).
- `DeclaredAdd { identity: PublicKey, device: DeviceId, key_package: EventRef }` — fold-readable Add declaration inside `MlsCommit`.
- `DeclaredRemove { identity: PublicKey, device: DeviceId }` — fold-readable Remove declaration.
- `DeleteReason` — `Author | Moderation` (`Copy`). `Author` claims are verified against the derived view by ingest (sub-3); the fold verifies `Moderation` authority itself.

#### `EventPayload::AttachmentRedacted { content_hash: String }`

Signals that the bytes for the attachment identified by `content_hash` (hex SHA-256) should be permanently deleted and the attachment replaced by a tombstone. Authorization is content-addressed and log-derived: the server derives the uploader from `LogState::attachment_uploader`, so the right applies globally even if the attachment was uploaded to a different node.

Authz rules enforced by `LogState::apply_payload_check`:
1. `content_hash` must appear in `LogState::attachment_uploaders` (a `MessagePosted` event must have cited it first).
2. The event author must equal the recorded uploader, OR hold the `"kick"` capability.
3. `content_hash` must NOT already be in `LogState::redacted_attachments` (double-redact rejected).

### `AttachmentCap`

```
pub struct AttachmentCap {
    pub content_hash: String,   // hex SHA-256 of the file bytes
    pub declared_type: String,  // MIME type, e.g. "image/png"
    pub size: u64,
    pub uploader: PublicKey,
}
```

Embedded in `MessagePosted.attachments`. Validated at ingest by `event_ingest::derive_attachments` (size/mime/uploader must match the stored blob).

### `DeviceCert` expiry (Rung 2, dormant)

`DeviceCertCore` gained an optional **last** field:

```
#[serde(default, skip_serializing_if = "Option::is_none")]
pub expires_at: Option<u64>,   // unix seconds; checked against event.core.timestamp by the fold (untrusted author clock)
```

Because `None` is skipped in serialization, a cert made by `DeviceCert::create`
(unchanged signature, always `expires_at: None`) produces the **byte-identical
4-field canonical encoding** legacy certs were signed over — pre-Rung-2
signatures verify unchanged after decode → re-encode (pinned by
`device_cert_without_expiry_preserves_legacy_signed_bytes`).

New constructor: `DeviceCert::create_expiring(identity: &Keypair, device_pubkey: &PublicKey, created_at: u64, expires_at: u64) -> DeviceCert`
— identical to `create` but signs `expires_at: Some(expires_at)`. Expiry is
**enforced by the fold** (sub-2 Task 2): `LogState::apply` rejects any event
whose author-device cert has `expires_at < event.core.timestamp` — including
the `DeviceAuthorized` self-registration itself. Nothing emits expiring certs
yet.

---

## `event_log_state.rs` — authorization state machine (`LogState`)

`LogState` folds the ordered sequence of validated events into the current membership, capabilities, device bindings, and attachment redaction state. It is pure (no I/O); replays deterministically from any checkpoint via `LogState::replay(genesis, events)`.

### New fields introduced by mesh-4b

| Field | Type | Populated by | Purpose |
|---|---|---|---|
| `attachment_uploaders` | `HashMap<String, PublicKey>` | `MessagePosted` effect (`or_insert_with`) | Maps `content_hash` → first uploader seen; first-writer-wins. Authz basis for self-takedown. |
| `redacted_attachments` | `HashSet<String>` | `AttachmentRedacted` effect | Set of `content_hash` values that have been redacted. Once in this set, the hash is permanently blocked from re-upload via the authz check. |

### Rung-2 fold state (sub-2 Task 2)

| Field | Type | Populated by | Purpose |
|---|---|---|---|
| `channels` | `HashMap<u64, ChannelRecord { name, kind, class, parent }>` | `ChannelCreated` effect | Channels known to the log. **Absence = legacy plaintext channel** (the replay carve-out: `MessagePosted` into an unknown channel stays valid, exactly Rung-1 behavior; into a known `E2ee` channel it is rejected). |
| `tombstones` | `HashSet<EventRef>` | `MessageDeleted` effect | Durable content-blind deletion tombstones (spec F2). |
| `revoked_devices` | `HashSet<DeviceId>` | `DeviceRevoked` effect | Dead devices. Checked at the **envelope**: a revoked device can author nothing new (not even a fresh `DeviceAuthorized`); its history stands. |
| `devices_by_identity` | `HashMap<PublicKey, HashSet<DeviceId>>` | `DeviceAuthorized` effect | Every device ever authorized per identity; liveness is filtered by `live_devices`, never by removal. |
| `log_pos` | `u64` | `+1` per accepted event | The pure log-position clock (drives KeyPackage lifetimes in Task 3). No wall clock enters the fold. |

`DeviceRecord` also gained `expires_at: Option<u64>` (from the `DeviceAuthorized`
cert). Two envelope gates run right after signature verification, before the ban
gate: **revocation** (`revoked_devices`) and **cert expiry** — judged against
`event.core.timestamp`, the untrusted author clock (the same acceptance Rung 1
made for invite expiry).

Constant: `MAX_LIVE_DEVICES_PER_IDENTITY = 8` (pub). `DeviceAuthorized` is
rejected when the author already has 8 live (non-revoked, cert-unexpired at the
event's timestamp) devices; revoking a device frees its slot.

### Rung-2 MLS fold state (sub-2 Task 3)

| Field | Type | Populated by | Purpose |
|---|---|---|---|
| `mls_groups` | `HashMap<u64, MlsGroupRecord>` | `ChannelCreated { E2ee }` (generation 0 / epoch 0), then commit/welcome/confirm effects | Per-E2ee-channel MLS control-plane bookkeeping. |
| `key_packages` | `HashMap<(PublicKey, DeviceId), HashMap<EventRef, u64>>` | `MlsKeyPackagePublished` effect | Live (unconsumed) KeyPackage refs → expiry **log position**; expired refs are pruned on touch. |
| `consumed_key_packages` | `HashMap<EventRef, u64>` | `MlsCommit` effect (Add consumption) | Consumed refs → expiry log pos, so a consumed ref can never be replayed as an Add target; prunable past expiry (spec I5). |
| `device_store_instance` | `HashMap<DeviceId, [u8;32]>` | First `MlsKeyPackagePublished` (or commit/confirm) per device | The pinned MLS store-instance hash (spec C6); any later publish/commit/confirm with a different hash is rejected — the clone/restore poison signal. |

`MlsGroupRecord` (private) holds: `generation`, `epoch` (the epoch the group is
IN = declared + 1 of the last accepted commit), `commit_head`,
`epoch_authenticator` (**the chain variable** — the last commit's declared
`post_epoch_authenticator`; `None` marks a generation's bootstrap),
`tree_hash`, `leaves_confirmed` / `leaves_pending` (`HashSet<(PublicKey,
DeviceId)>`), `events_since_last_commit` (freshness-ceiling counter: channel
events since the last commit — zeroed by every accepted commit and by a reset),
`last_commit_epoch_by_author` (commit-rate clock),
`channel_events_since_reset` (reset rate-limit clock), `reset_pending`,
`reset_expected_tree_hash`, `commits_by_epoch: HashMap<u64, CommitRecord>`
(keyed by the epoch a commit **created**), `welcomes: HashMap<EventHash,
WelcomeRecord>`.

Constants (pub): `MAX_LIVE_KEY_PACKAGES_PER_DEVICE = 10`,
`COMMIT_RATE_MIN_EPOCH_GAP = 4`.

### Rung-2 sealed-content gates and reset (sub-2 Task 4)

Constants (pub): `FRESHNESS_CEILING_EVENTS: u32 = 500`,
`RESET_MIN_CHANNEL_EVENTS: u32 = 1000`.

Both sealed variants share one private gate helper, so an edit is blocked
exactly where a post is. In gate order: **class** (unknown channel or
`Plaintext` ⇒ invalid) → **full member** → **confirmed leaf** for the signing
device → **generation** → **epoch** (current only) → **`pending_removals` empty**
→ **freshness ceiling** → **`!reset_pending`**. Each fails closed, and each is a
*protocol* invariant a blind host enforces, not client courtesy:

- **Pending-removals gate (spec I1).** From the moment a ban / kick / device
  revocation / cert expiry creates drift, the channel is sealed **for everyone**
  until a Remove-commit discharges it. A patched or uninformed client cannot
  keep sending.
- **Freshness ceiling (spec C4).** `events_since_last_commit` counts sealed
  posts, sealed edits and tombstones in E2ee channels (saturating). At 500 the
  channel stops accepting content until somebody rekeys — forward secrecy
  becomes an invariant rather than a hope. Any accepted commit zeroes it.
- **Partial reset (spec C7).** After an accepted `MlsGroupReset`,
  `reset_pending` stays set until `leaves_confirmed` equals
  `members × live_devices`; sealed sends are invalid throughout, so a partial
  reset is a **dead channel, loudly**, never a silent partition. The reset
  generation's add-commit is never a log event, so the first `MlsLeafConfirmed`
  seeds `reset_expected_tree_hash` and every later one must match it
  (ambiguity #7).

`authz_head` is carried but **never read by the fold**: head attestation is a
client-side mechanism (peers compare cited heads against their own folded
history), documented at the rule site.

**Stale-commit no-op:** `apply` keeps its `Result<()>` signature. An `MlsCommit`
that loses the epoch CAS returns `Ok(())` but skips all payload effects — only
the author's chain head and `log_pos` advance, zero MLS state change — so any
converged event set folds to the same winner on every replica (plan resolved
ambiguity #8). Ingest's distinct `stale-epoch` bounce is sub-3's job, served by
`mls_current_epoch`.

### Rung-2 purity + checkpoint composability (sub-2 Task 5)

The fold stays exactly what Rung 1 made it: a pure
`(prior_state, event) -> new_state` step, so `replay == stepwise ==
resume-from-any-checkpoint` over **all** the Rung-2 state above. Two rules keep
it that way, and both are now load-bearing for MLS:

- **Two deterministic clocks, never wall time.** `event.core.timestamp` (the
  untrusted author claim) drives device-cert expiry, `live_devices`, and every
  `members × live_devices` derivation; `log_pos` (count of accepted events)
  drives KeyPackage lifetimes. Both are inputs or folded state, so a checkpoint
  carries them.
- **Check-then-mutate.** Every fallible check runs before any mutation, so a
  rejected event leaves the state byte-identical (no clone/rollback anywhere).

Pinned by `replay_equals_stepwise_and_composes_from_a_checkpoint_over_all_rung2_state`:
one log exercising **every** Rung-2 variant (both channel classes, KeyPackage
publishes, the bootstrap commit, an add-commit + Welcome + joiner confirmation,
a **stale commit accepted as a no-op**, sealed post + edit, tombstones in both
classes, an expiring cert + `DeviceRevoked`, staged Welcomes + `MlsGroupReset` +
post-reset confirmations + the send that proves the unlock) is folded by replay,
stepwise, and by resuming from **every** checkpoint position; all three are
compared field-by-field over the whole `LogState` and over every public query
surface. The stale commit is asserted to move only the chain head and `log_pos`
(the spec's commit-race determinism: the same event set folds to the same winner
on every replica). The private fold records derive `PartialEq` solely for that
comparison — nothing in the fold compares records.

**Validated against real OpenMLS, not just itself:**
`crates/farder-mls/tests/fold_chain.rs` drives a real `MlsChannelGroup`
(creator + two joiners, in-memory provider) and feeds real values through the
fold — `CommitOutcome::{commit_bytes, prev_epoch_authenticator, post_tree_hash,
epoch}` plus the post-merge `epoch_authenticator()` become `MlsCommit` events,
real TLS-serialized KeyPackages become `MlsKeyPackagePublished` events (read
back through `decode_key_package`), and each joiner's `JoinInfo { epoch,
tree_hash }` becomes its `MlsLeafConfirmed`. The tests assert the chain rule
holds on real values (`prev_epoch_authenticator` of commit *k+1* IS the
authenticator commit *k* declared), that a tampered declaration is rejected while
the honest declaration of the same MLS commit is accepted, and that
`leaves_confirmed` / `mls_current_epoch` converge on the real tree membership and
epoch. That is the proof for plan ambiguity #1 (the added
`post_epoch_authenticator` field) and #5 (the epoch conventions).

### Public query methods

#### `is_attachment_redacted(hash: &str) -> bool`

Returns `true` if `hash` is in `redacted_attachments`. Used by `handlers.rs` to gate ingest (double-redact rejection) and optionally by the download path.

#### `attachment_uploader(hash: &str) -> Option<&PublicKey>`

Returns the recorded first uploader of `hash`, or `None` if no `MessagePosted` event has cited it. Used by `handlers.rs` to resolve `by_moderator` before broadcasting `ServerEvent::AttachmentRedacted`.

#### `channel_class(channel_id: u64) -> Option<ChannelClass>`

The class of a channel known to the log. `None` = no `ChannelCreated` exists — a legacy DB channel (permanently plaintext) or no channel at all.

#### `is_tombstoned(target: &str) -> bool`

Whether an accepted `MessageDeleted` tombstone exists for this event ref. Sub-3's derive/reconcile paths consult it so deletions cannot resurrect.

#### `is_device_revoked(device: &str) -> bool`

Whether the device has been killed by an accepted `DeviceRevoked`.

#### `live_devices(pk: &PublicKey, at_ts: u64) -> Vec<DeviceId>`

An identity's live devices at `at_ts`: authorized, non-revoked, and cert-unexpired at that (untrusted, author-claimed) timestamp. Sorted for determinism. Feeds the device cap, the `members × live_devices` MLS target set (drift + reset completeness), and the reset-completion check.

#### `log_pos() -> u64`

The log-position clock: count of accepted events folded so far (a stale-commit
no-op still counts — it is an accepted event). Drives KeyPackage lifetimes.

#### `pending_removals(channel_id: u64, at_ts: u64) -> HashSet<(PublicKey, DeviceId)>`

The spec's derived drift set: leaves the group holds that the authz fold says
should not be there — `(confirmed ∪ pending) \ (members × live_devices(at_ts))`.
Non-empty ⇒ sealed sends are blocked until a Remove-commit discharges it.
Empty for channels without an MLS group.

#### `pending_adds(channel_id: u64, at_ts: u64) -> HashSet<(PublicKey, DeviceId)>`

The complement: member devices the group should hold but does not —
`(members × live_devices) \ (confirmed ∪ pending)`. Stewards read this to build
Add-commits.

#### `mls_current_epoch(channel_id: u64) -> Option<(u64, u64)>`

`(generation, epoch)` the channel's group is currently in — sub-3's stale-epoch
pre-check. `None` = no MLS group (unknown or Plaintext channel).

#### `leaves_confirmed(channel_id: u64) -> HashSet<(PublicKey, DeviceId)>`

The joiner-confirmed leaves (drift detection runs on these, never on pending
leaves — a bogus Welcome therefore leaves visible drift).

#### `commit_discharges_drift(event: &Event) -> bool`

Whether an `MlsCommit` event discharges an outstanding fold obligation (an add
∈ `pending_adds` or a remove ∈ `pending_removals` at the event's timestamp).
`false` for non-commit payloads.

#### `compare_same_epoch_commits(a: &Event, b: &Event) -> Ordering`

The drift-priority tiebreak for same-epoch candidate commits (spec I2, exported
pure for Rung 3's orderer): an obligation-discharging commit orders first
regardless of hash grinding or self-asserted lamport; when neither or both
discharge, canonical order `(lamport, author, event_hash)`.

### Integration map

- **`event_log_state.rs`** is consumed by `farder-server::handlers.rs` (the `SubmitEvent` arm), which holds a single `Arc<Mutex<Option<LogState>>>` per server. `handlers.rs` acquires the mutex, trial-applies the event to validate authz, then commits to DB in a transaction, and on success replaces the mutex value with the advanced state.
- **`event_log.rs`** is consumed by `farder-crypto` itself (signing/verification), `farder-server::event_ingest` (persistence), and `client/src-tauri/src/commands.rs` (building events on the client side).
