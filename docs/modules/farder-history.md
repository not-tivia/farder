# farder-history — the local decrypted-history store

> **File(s):** `crates/farder-history/src/lib.rs`
> **Layer:** Crypto crate (client-side only; the server never links it)
> **Last reviewed:** 2026-08-27

## Purpose

An E2EE channel's history can only live on a member's own device, so this crate
is where it lives — sealed.

Opening a sealed message **consumes that generation's ratchet key in the
persisted MLS store**, so the same ciphertext can never be opened twice (pinned
by `farder-e2ee-client`'s
`opening_a_sealed_message_twice_is_impossible_so_history_needs_a_local_store`).
Sub-4b held decrypted content in frontend memory only, so restarting the app made
every previously-read message render `🔒 Encrypted message — couldn't decrypt`.
Re-fetching from the server does not help: it returns ciphertext, and MLS has
already deleted the key that opens it. A local store is therefore not an
optimization — without it an E2EE channel has no history at all.

Persisting decrypted text is only acceptable if it is encrypted at rest, so this
crate has **no plaintext mode**, not even for tests.

---

## What is sealed, and what is not

| On disk | Why |
|---|---|
| `author`, `content`, `reply_to`, `attachments` | Sealed in one AES-256-GCM blob per row — this is the part the server host genuinely cannot see. |
| `channel_id`, `message_id`, `event_hash`, `timestamp` | **Deliberately in the clear.** Ordering, pagination, retention sweeps and tombstone purges become index operations that never decrypt anything. These four are exactly what the host already stores for every sealed row, so they tell an attacker holding this file nothing they could not get from the host's database. |
| `author_tag` | An HMAC **blind index** over the author, so anonymize-on-leave purges run by index — without storing the author in the clear and without decrypting rows. |

The AEAD's associated data binds each row to `(channel_id, message_id,
event_hash)`, so a sealed blob relocated to another message inside the file fails
to open rather than silently decrypting somewhere it does not belong.

## Keys

`derive_keys(identity_signing_key)` HKDF-expands two domain-separated subkeys —
one for row AEAD, one for the blind index. They must differ: sharing them would
let a leaked row key forge author tags.

The identity key is already PIN-wrapped at rest (Argon2id + AES-256-GCM) and is
already unlocked at every launch, so the archive is protected by the PIN the user
already types — **no second prompt, no second Argon2 pass**. Deriving an
encryption key from the identity signing key follows the precedent set by
`farder_crypto::key_exchange::ed25519_sk_to_x25519`. A PIN change re-wraps
`identity.key` only and does not touch the archive; the recovery phrase restores
the identity and therefore the archive key.

`HistoryKeys` is `ZeroizeOnDrop` and its `Debug` is redacted — a key that prints
is a key that reaches a log file.

---

## Public interface

- `derive_keys(&[u8; 32]) -> HistoryKeys` — the two subkeys (see above).
- `author_tag(&HistoryKeys, author: &[u8]) -> Vec<u8>` — the keyed blind index.
- `HistoryStore::open(path, keys)` / `open_in_memory(keys)` — create or open the
  database. `open_in_memory` is for tests that do not exercise the file itself;
  the observation test deliberately does not use it.
- `put(&HistoryRecord)` — store one decrypted message. Idempotent on
  `(channel_id, message_id, event_hash)`; a duplicate write replaces the row
  rather than producing a second one.
- `get(channel_id, message_id) -> Option<HistoryRecord>` — one message.
- `page(channel_id, before_id, limit) -> Vec<HistoryRecord>` — newest-first,
  mirroring `fetch_history_v2`'s shape so the frontend merge is a drop-in.
- `search(channel_id, query, limit)` — case-insensitive substring search within
  one channel. Decrypt-and-scan on purpose: personal-scale data, and a blind
  token index would leak equality patterns for throughput we do not need.
- `count(channel_id)` — row count (diagnostics + tests).

### Purge obligations (the compliant-client rule)

The spec makes these a client obligation: server-side those mechanisms operate on
ciphertext, so *end to end* they only work if the client purges its own copy.
Each is a DELETE by index — no decryption, no scan.

- `purge_message(channel_id, message_id)` — fold a `MessageDeleted` tombstone.
- `purge_before(channel_id, before_ts)` — retention expiry.
- `purge_author(author)` — anonymize-on-leave, via the blind index.
- `redact_attachment(channel_id, message_id, attachment)` — the one purge that
  needs the key, because attachments live inside the sealed blob: it re-seals the
  row without that ref.

---

## Tests worth knowing

- `the_stored_file_contains_no_plaintext_content_or_author` — the named
  deliverable. Writes a needle through the real `put` path, closes the database,
  then scans every column of every table **and the raw file bytes** (freelist
  pages, stale WAL frames) for it.
- `the_observation_scanner_finds_a_needle_that_is_really_there` — the positive
  control. Without it, a scanner that quietly looked in the wrong place would
  make the observation test pass while the store leaked everything.
- `a_row_moved_to_another_message_id_fails_to_open` — pins the AAD binding.

Both load-bearing guards were verified by breaking them: dropping the AAD fails
the moved-row test, and writing rows in the clear fails the observation test.

---

## Connects to

- **Written by** the client's decrypt path: every successful
  `decrypt_sealed_message` is persisted exactly once (a failed decrypt writes
  nothing — a failure is never cached as history).
- **Read by** the channel-open path *before* decrypting: a sealed row already in
  the store renders from it and is never handed to `decrypt_sealed_message`
  again. That is what makes restart work, and it also stops a restart from
  burning one ratchet key per message for nothing.
- **Not** used for DMs: those use a different key mechanism and are unchanged by
  this rung.
