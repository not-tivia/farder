# Mesh Rung 2 — Sub-project 7a: Local History Store (headless) — Implementation Plan

**Goal:** make an E2EE channel's history survive an app restart, without writing a
plaintext archive to disk. Today it does not survive at all, and nothing is
persisted; the naive fix (persist what we decrypted) would be a privacy
regression. Both halves land together or neither does.

**Spec:** `docs/superpowers/specs/2026-07-27-mesh-rung2-e2ee-design.md` sub-project 7
(plus lines 362-364: the local decrypted store is *required*, at-rest wrapping is
in scope, and purge obligations are a compliant-client rule). Baseline: `main`
@ `dc86b06`, 905 workspace tests green.

**Why this jumped the queue ahead of sub-5b (owner's call, 2026-08-27):** 5b's
central copy is "history is gone for that device", which is meaningless while
history is gone for *everyone* on *every restart*.

## The finding that motivates this (verified, not assumed)

Opening a sealed message consumes that generation's ratchet key **in the
persisted store**, so the same ciphertext can never be opened twice — pinned by
`opening_a_sealed_message_twice_is_impossible_so_history_needs_a_local_store`
(`f0e7ffe`), which hands a byte-for-byte clone back to the same on-disk store and
watches it fail. 4b holds decrypted content in frontend memory only (decision D4).
So after a restart the client re-fetches the ciphertext, cannot open it, and every
previously-read message renders `🔒 Encrypted message — couldn't decrypt` under a
banner counting them (`Message.tsx:633`, `ChatPanel.tsx:205`).

## Scope decisions (verified against recon)

**D1 — a NEW root-workspace crate `crates/farder-history`, not `client/src-tauri`.**
Same reason as 4a's Decision 1: the client crate carries its own `[workspace]`, so
root integration tests cannot link it and a harness written there would test a
reimplementation. Tauri commands stay thin wrappers.

**D2 — plain `rusqlite` + application-level AEAD per row, NOT SQLCipher.** No new
C dependency, and rusqlite is already the project's storage reach. Each row keeps
`channel_id`, `message_id`, `event_hash`, `timestamp` in the clear for indexing,
ordering, retention sweeps and tombstone purges, and seals `{author, content,
reply_to, attachments}` into one AES-256-GCM blob. **The metadata delta is
deliberate and must be documented, not hidden:** the plaintext columns are exactly
what the *server host already stores* for every sealed row, so they reveal nothing
new to an attacker who has the host's database; content and authorship — the part
the host cannot see — never touch the disk unsealed.

**D3 — the author is a blind index, not a column.** `author_tag = HMAC(store_key,
author_pk)` so anonymize-on-leave and per-author purges can run by index without
decrypting rows and without storing the author in the clear.

**D4 — the store key is HKDF-derived from the identity signing key**
(domain-separated, `"farder-history-store-v1"`), not a second Argon2 pass over the
PIN. Rationale: the PIN is already entered at every launch (`IdentityGate` shows
`enter-pin` whenever an encrypted identity exists) and the unlocked identity key
already lives in `AppState.signing_key_bytes`, so the archive key is available
exactly when the identity is unlocked — **zero added friction, which retires the
spec's open question Q11 about unlock friction rather than answering it.** A PIN
change re-wraps `identity.key` only and does not touch the archive; the recovery
phrase restores the identity and therefore the archive key. Precedent for deriving
an encryption key from the identity signing key already exists in this codebase
(`farder-crypto/src/key_exchange.rs: ed25519_sk_to_x25519`). The spec says
"encrypted under the PIN-derived key"; this is that property reached by one fewer
KDF, and the deviation is recorded here on purpose.

**D5 — search is decrypt-and-scan over one channel, not a blind token index.**
Bounded, personal-scale data; a token index leaks equality patterns for no
throughput we need. Revisit only with a measured complaint.

## The honesty problem this sub-project must not create

PIN-wrapping the archive while its neighbours sit in the clear would be theatre.
Verified on disk today: **the device signing key is written as 32 raw bytes**
(`client/src-tauri/src/device.rs:40`) and **the MLS store is an unencrypted sqlite
file** (`crates/farder-mls/src/store.rs:152`, `Connection::open`) holding the group's
ratchet secrets. The identity key IS wrapped (Argon2id 64 MiB/3 + AES-256-GCM) and
is what authenticates a connection (`connection.rs:168` signs the challenge with
`AppState.signing_key_bytes`), so a seized laptop cannot fetch from the server —
but anyone who *already holds ciphertext* (the host, or any member) plus that
laptop reads everything that device could read. Task H exists so we do not ship a
sealed archive next to the keys that open it.

## Tasks

### The store (headless, harness-tested)

- [x] **T1 — the crate skeleton + schema.** New `crates/farder-history` in the root
      workspace. `HistoryStore::open(path, store_key)`, schema with the D2 columns,
      the D3 blind index, a schema-version row, and `WITHOUT ROWID` where it pays.
      No client wiring yet.
- [x] **T2 — seal/unseal one row.** AES-256-GCM per row with a per-row random nonce
      stored beside the blob; AAD binds `(channel_id, message_id, event_hash)` so a
      row cannot be moved between channels or messages and still open. Round-trip
      + wrong-key + moved-row tests.
- [x] **T3 — put/get/paginate.** `put(record)` (idempotent on `(channel_id,
      message_id, event_hash)`), `page(channel_id, before_id, limit)` mirroring
      `fetch_history_v2`'s shape so the frontend merge is a drop-in.
- [x] **T4 — the purge obligations (the compliant-client rule).** `purge_message`
      (tombstone / `MessageDeleted`), `purge_attachment` (redaction),
      `purge_before(channel_id, ts)` (retention expiry), `purge_author(tag)`
      (anonymize-on-leave). Each is a DELETE by index — no decryption, no scan.
- [x] **T5 — the observation test (named deliverable).** Write a known needle
      through the real put path, then scan **every table and every column** of the
      closed database file for that needle, exactly like `assert_no_plaintext_anywhere`
      — plus a positive control that the scanner finds a needle deliberately written
      in the clear, so the test cannot pass by being blind.
- [x] **T6 — search.** `search(channel_id, query, limit)`, decrypt-and-scan per D5,
      case-insensitive, returning the same record shape as `page`.

### Wiring (the part that makes history actually come back)

- [x] **T7 — key derivation + lifecycle.** `history_key` derived per D4 at unlock,
      held in `AppState` beside `signing_key_bytes`, zeroized on lock. Never written
      to disk, never logged, never crosses the Tauri boundary.
- [x] **T8 — Tauri commands.** `history_put` / `history_page` / `history_search` /
      `history_purge_*` as thin wrappers, each documented in
      `docs/modules/tauri-commands.md` in the SAME commit, and each name
      cross-checked by `scripts/seam_audit.py` (the untyped seam that shipped a
      broken voice join before).
- [x] **T9 — write on decrypt.** `useSealedDecrypt` persists every successful
      decrypt exactly once. This is the ONLY writer; a decrypt that fails writes
      nothing (never cache a failure as history).
- [x] **T10 — read before decrypt.** On channel open, a sealed row already in the
      local store renders from it and is NEVER handed to `decrypt_sealed_message`.
      This is what makes restart work, and it also protects the ratchet: today a
      restart burns a key per message for nothing.
- [x] **T11 — fold the purges.** `MessageDeleted` (and the redaction/retention paths
      the client already sees) call the T4 purges, so a delete that lands while the
      client is running does not survive in the local archive.

### Honesty (Task H — do not ship a sealed archive next to bare keys)

- [ ] **H1 — wrap the device signing key** at rest with the same scheme, with a
      one-time migration from the existing raw-32-byte file.
- [ ] **H2 — assess encrypting the MLS store's values** through its `Codec` seam
      (`SqliteStorageProvider<RmpCodec, Connection>` — the codec is OURS, so an
      encrypting codec seals every value openmls writes). **Assess, then decide in
      the plan before building:** it is a breaking on-disk change needing either a
      migration or a stated wipe, and the wipe cost is real. Record the verdict here
      either way.

## Gates
- `cargo test --workspace` ≥ 905, never fewer.
- `cargo clippy -p farder-history --no-deps -- -D warnings` clean; no new warnings.
- Client crate builds separately; `cd client && npx tsc --noEmit`.
- `git ls-files --eol` after scripted edits (CRLF trap).
- The T5 observation test and its positive control both green.

## Review discipline
Break every load-bearing guard and watch its test fail. The load-bearing items:
the AAD binding (T2), the decrypt-once bypass (T10 — a bug here silently burns
ratchet keys), each purge (T4/T11), and the observation scanner itself (T5's
positive control IS that check).

## Carry-forwards (recorded, not done)
- Export/import of one's own store (spec's "own-device store export/import decision
  surface") — a GUI surface, deferred to 7b with the search UI.
- DM history is out of scope: different key mechanism, unchanged by this rung.
- If H2 lands as "wipe", the owner needs the wipe stated in the 7b release note.
