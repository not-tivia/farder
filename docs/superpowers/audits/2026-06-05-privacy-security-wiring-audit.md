# Privacy / Security Wiring Audit — 2026-06-05

**Question this audit answers:** Farder's product promise is privacy and security —
E2EE DMs, IP masking via relays, cryptographic identity. *Are those features
actually wired onto the real path, or do they merely exist in the code?*

**Method (per CLAUDE.md):** verify by **observation**, not code-reading. Where a
guarantee is testable, drive the real path and assert on the bytes. Where the
finding is about absence (a step not on the path), confirm by direct inspection
of the call site plus a repo-wide search proving the secure path is never called.

Status legend: **VERIFIED** (observed good) | **GAP** (observed missing/dead) |
**OK** (inspected, no issue).

---

## Summary

| # | Area | Verdict | Evidence |
|---|------|---------|----------|
| 1 | DM end-to-end encryption | **VERIFIED** | observation tests pass — plaintext absent from wire bytes; peer-only decrypt |
| 2 | Identity private key at rest | **GAP (HIGH)** | key written as raw 32 bytes; `export_encrypted`/PIN never called |
| 3 | Relay IP masking | **GAP (MED)** | relay crate is dead code — never wired into the client connect path |
| 4 | Private key never on wire / logged / to frontend | **OK** | only public key + signature sent; no key logging; `Debug` not derived |
| 5 | Voice stream-key handling | **OK** | random per-session, memory-only, wrapped per-peer with AES-256-GCM |

The headline product claim that holds up under observation is **DM E2EE**. The two
gaps are both "the secure mechanism exists in a crate but is not on the real
path" — exactly the failure mode CLAUDE.md was written to catch.

---

## 1. DM end-to-end encryption — VERIFIED

**Real path:** `MessageInput.tsx handleSend()` detects a DM and calls
`api.dmEncrypt(peerPk, text)` **before** `api.sendMessage(...)`; if encryption
fails it aborts the send (no plaintext fallback). The Tauri command
`dm_encrypt` (`client/src-tauri/src/commands.rs:1989`) derives an X25519/Ed25519
shared secret (`key_exchange::derive_dm_shared_secret`) and AES-256-GCM encrypts
(`encryption::encrypt`, `crates/farder-crypto/src/encryption.rs:4`). The string
that reaches `send_message` — and therefore the wire — is `hex::encode(ciphertext)`.
The server stores `content` verbatim and never decrypts
(`crates/farder-server/src/handlers.rs` SendMessage arm → `messages::insert_message`).

**Observation (the new evidence):** `tests/security_observation.rs` drives the
exact crypto path and asserts on the bytes:

- `dm_content_on_wire_is_ciphertext_not_plaintext` — a distinctive plaintext
  canary is **not** present (whole, nor any 4+ char word) in either the raw
  ciphertext or the hex content string that traverses the network; the intended
  peer recovers the exact plaintext (symmetric ECDH); an outsider with the wrong
  key gets a different secret and GCM **rejects** decryption.
- `encoded_dm_protocol_frame_never_contains_plaintext` — drives the node relay
  path `prepare_dm` → `codec::encode` and asserts the canary is absent from the
  **entire** encoded protocol frame (envelope + metadata), not just the
  ciphertext field; the receiver still decodes and decrypts it.

Both tests pass (`cargo test --test security_observation`). This upgrades the DM
claim from "the code calls encrypt()" to "the plaintext is observably absent
from the bytes that leave the process."

**No plaintext-on-the-wire path found:** there is no `SendDm`-style variant
carrying raw text, no preview/subject field, and DM `content` is the only
body-bearing field. Typing indicators carry no message content.

---

## 2. Identity private key at rest — GAP (HIGH)

**Observation:** `generate_keypair` writes the key with
`std::fs::write(&path, keypair.signing_key_bytes())`
(`client/src-tauri/src/commands.rs:71`) — the **raw 32-byte Ed25519 private
key**, no encryption. `load_identity` reads it straight back
(`commands.rs:82`). The file is `~/.farder/identity.key` (or `$FARDER_DATA`).

**The secure mechanism exists but is never called.** `Keypair::export_encrypted`
/ `import_encrypted` (Argon2 KDF + AES-256-GCM, `crates/farder-crypto/src/identity.rs:41`)
and the `PinHash` module (Argon2, `crates/farder-crypto/src/pin.rs`) are fully
implemented and unit-tested, but a repo-wide search shows **zero** call sites
outside the crypto crate's own tests:

```
grep -rn "export_encrypted|import_encrypted|PinHash" --include=*.rs .   # → nothing in client/ or node/
```

**Impact:** anyone with read access to the user's disk (malware, backup, shared
machine, stolen device) obtains the full identity private key — the key that
authenticates the user to every server and derives every DM shared secret. This
directly undercuts the "cryptographic identity" promise.

**Recommended fix:** wire `export_encrypted`/`import_encrypted` into
`generate_keypair`/`load_identity`, gated by a PIN/passphrase via the existing
`PinHash` path. `load_identity` becomes async/needs the PIN. Decide UX: PIN
required on every launch vs. OS keychain-backed. This is a self-contained change
behind the same two Tauri commands.

---

## 3. Relay IP masking — GAP (MED)

**Observation:** the relay crate (`crates/farder-relay/`) is correctly designed —
it terminates the client QUIC connection and opens a **separate** upstream
connection to the server (`router.rs:73 to.open_bi()`), so the server's
`remote_address()` would be the relay's, and it forwards raw bytes with no
source-address injection (no X-Forwarded-For-equivalent in any protocol message).
**But it is never used by the client.** Confirmed by observation:

```
grep -rn "relay_address" --include=*.rs .
  crates/farder-node/src/peer_manager.rs:12:  pub relay_address: Option<String>,   # field
  crates/farder-node/src/peer_manager.rs:27:  relay_address: None,                 # only ever None
```

`relay_address` is set to `None` and **never assigned a value nor read
anywhere**. `connect_server` (`commands.rs:408`) takes a single direct
`address: String` and calls `connect_and_authenticate` straight to it
(`connection.rs`). There is no relay parameter, no relay-vs-direct decision, and
no relay UI. The `farder-relay` binary is standalone dead code on the client
path.

**Impact:** in the shipped client, every connection is direct, so the
destination server sees the client's **real IP** (`main.rs:122` logs
`remote_address()`). The "IP masking via relays" promise is **not in effect**.

**Recommended fix:** wire a relay address through `connect_server` →
`connect_and_authenticate`: connect to the relay, send `Message::RelayConnect {
destination_id }`, then run the normal auth handshake over the bridged stream.
Then verify by observation: a test that connects through the relay and asserts
the server observes the relay's `remote_address()`, never the client's. Until
then, the privacy claim should not be advertised as active.

---

## 4. Private key never leaves the device — OK

- **On the wire:** only `public_key` + `signed_challenge` are sent in
  `ClientFrame::Authenticate` (`connection.rs:~90`); the private key signs the
  challenge but is never transmitted. No protocol message carries a private key.
- **Logs:** no `println!`/`eprintln!` of key bytes anywhere in `farder-crypto`
  or the client key paths; `Keypair` deliberately does **not** derive `Debug`,
  preventing accidental `{:?}` leakage.
- **Tauri boundary:** `generate_keypair`/`load_identity`/`get_public_key` return
  only the **public** key string; private bytes never cross to the frontend.

(In-memory the key is held as a plain `[u8;32]` in `AppState` and is not zeroized
on drop — minor hardening opportunity, not a wire/at-rest leak.)

---

## 5. Voice stream keys — OK

`media::derive_stream_key()` is fresh `rand::random()` per voice-channel join,
held in memory only, never persisted. It is wrapped per-recipient with
`wrap_stream_key_for_peer` (ECDH shared secret + AES-256-GCM) before being sent
via `offer_stream_key`, so only the wrapped ciphertext crosses the wire
(`client/src-tauri/src/voice/mod.rs:~526`). No plaintext stream key on the wire.

---

## What changed in this audit

- Added `tests/security_observation.rs` (2 tests, passing) + registered it in
  `Cargo.toml`. This is the start of the "end-to-end security tests" item — the
  DM guarantee now has runtime, observation-based coverage that will catch
  regressions.

## Recommended follow-ups (for product owner decision)

1. **Encrypt the identity key at rest** (Gap #2) — highest impact, self-contained.
2. **Wire the relay into the connect path** (Gap #3) — or stop advertising IP
   masking as active until it is. Larger change.
3. Extend observation tests to the full server round-trip (assert the server's
   stored `content` is ciphertext) and, once #3 lands, the relay IP assertion.
4. Minor: zeroize the in-memory private key on drop.
