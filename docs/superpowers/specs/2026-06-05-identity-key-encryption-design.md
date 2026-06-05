# Identity Key Encryption at Rest — Design Spec

**Date:** 2026-06-05
**Status:** Approved (design); ready for implementation plan
**Audit origin:** `docs/superpowers/audits/2026-06-05-privacy-security-wiring-audit.md` Gap #2 (HIGH)

## Problem

The Farder identity private key (a 32-byte Ed25519 signing key) is written to
`~/.farder/identity.key` as **raw plaintext** by `generate_keypair`
(`client/src-tauri/src/commands.rs:71`) and read back the same way by
`load_identity` (`commands.rs:82`). Anyone who can read the user's disk obtains
the key that authenticates them to every server and derives every DM shared
secret. The encryption machinery to fix this already exists in `farder-crypto`
(`Keypair::export_encrypted` / `import_encrypted`, Argon2 + AES-256-GCM) but is
never called.

## Goal

Encrypt the identity key at rest behind a user-chosen **4-digit PIN**, required
on every app launch. Provide a one-time **recovery phrase** so a forgotten PIN
does not permanently destroy the identity. Migrate existing plaintext keys
transparently on next launch.

## Product decisions (settled)

- **PIN format:** 4-digit numeric, entered on every launch.
- **Brute-force defense:** because 4 digits is only 10,000 combinations, the
  Argon2 work factor is increased so each guess costs ~0.3–0.5s (acceptable as a
  once-per-launch delay; turns a seconds-long offline crack into hours/days).
- **Forgot-PIN:** a 24-word BIP39 recovery phrase is shown once at setup; it
  restores the key and lets the user set a new PIN.
- **No lockout counter:** an attacker with the file bypasses the app and attacks
  the blob directly, so an in-app attempt counter is false security. Argon2 cost
  is the real defense. The UI simply lets the user retry on a wrong PIN.

## Architecture

A new focused module `client/src-tauri/src/identity.rs` owns the on-disk identity
file: its format, plaintext-vs-encrypted detection, the PIN-gated
encrypt/decrypt orchestration, migration, and recovery. The cryptographic
primitives stay in `farder-crypto`. The Tauri commands in `commands.rs` become
thin wrappers delegating to `identity.rs`. This keeps `commands.rs` (already
~2000 lines) from growing further and makes the identity logic independently
unit-testable.

```
frontend gate (React)
   │  invoke(...)
   ▼
commands.rs  (thin #[tauri::command] wrappers)
   │  delegate
   ▼
identity.rs  (file I/O, format detection, migration, recovery, AppState load)
   │  uses
   ▼
farder-crypto  (Keypair::export_encrypted/import_encrypted, Argon2+AES-256-GCM)
                + new recovery-phrase helpers (BIP39)
```

### On-disk format & detection

No new format flag is introduced. Detection is by byte length of
`~/.farder/identity.key`:

| Length | Meaning | Action |
|--------|---------|--------|
| file absent | no identity | show Set-PIN / Restore screen |
| exactly 32 bytes | legacy plaintext key | migrate on next launch |
| ≥ 76 bytes | encrypted blob (`export_encrypted` output: 16 salt + 12 nonce + 48 ct+tag) | normal unlock |

`export_encrypted` already guards `data.len() < 16+12+32+16` on import, so a
short/corrupt file fails cleanly.

### Argon2 hardening

`Keypair::export_encrypted` / `import_encrypted` currently use
`Argon2::default()`. They will be changed to use an explicit, stronger
`argon2::Params` (target ≈ 64 MiB memory, 3 iterations, 1 lane — final values
tuned in implementation to land in the ~0.3–0.5s range on a typical machine).
Both encrypt and decrypt MUST use identical params or decryption fails. Because
no persisted data uses these functions yet, changing the cost has no migration
impact. The chosen params are encoded as constants in `farder-crypto` so encrypt
and decrypt cannot drift.

### Recovery phrase

A new pair of helpers in `farder-crypto` (e.g. `recovery::phrase_from_key(&[u8;32]) -> String`
and `recovery::key_from_phrase(&str) -> Result<[u8;32]>`) wrap the BIP39 `bip39`
crate. The 32-byte key is the BIP39 *entropy*, producing a 24-word mnemonic with
a built-in checksum (catches typos on restore). The phrase encodes the raw key,
so it is as sensitive as the key itself — the UI must warn accordingly. Restore
parses the phrase back to 32 bytes and re-encrypts under a new PIN.

## Tauri commands

All are thin wrappers over `identity.rs`. Names below are the `invoke("...")`
strings; each MUST be registered in `generate_handler!` in `main.rs` (the
untyped-seam check from CLAUDE.md).

| Command | Signature | Behavior |
|---------|-----------|----------|
| `identity_status` | `() -> IdentityStatus` | Returns `None` \| `Plaintext` \| `Encrypted` based on the file-length detection above. Drives which gate screen the UI shows. |
| `create_identity` | `(pin: String) -> CreateIdentityResult` | New user. Validates PIN is 4 digits; generates a fresh `Keypair`; encrypts with the PIN; writes the blob; loads the key into `AppState`. Returns `{ public_key, recovery_phrase }`. |
| `unlock_identity` | `(pin: String) -> Result<String, UnlockError>` | Returning user. Reads the encrypted blob; `import_encrypted(pin)`; on success loads the key into `AppState` and returns `public_key`; on failure returns a typed `IncorrectPin` error (no counter). |
| `migrate_plaintext_identity` | `(pin: String) -> CreateIdentityResult` | One-time. Reads the 32-byte plaintext key; encrypts it under the PIN; overwrites the file with the blob; loads into `AppState`. Returns `{ public_key, recovery_phrase }` (so the migrating user also gets their phrase). |
| `restore_identity` | `(phrase: String, pin: String) -> Result<String, RestoreError>` | Parses the BIP39 phrase to a key (typed `InvalidPhrase` error on bad checksum/words); validates the PIN; encrypts under the new PIN; writes the blob; loads into `AppState`. Returns `public_key`. |

`generate_keypair` and `load_identity` are **removed** (not left as a parallel
path): the plaintext-writing `std::fs::write(path, signing_key_bytes())` must not
survive anywhere, or it re-opens the gap. All current callers move to the gate
flow:
- `App.tsx:39` / `AppShell.tsx:81` `loadIdentity()` → the gate's
  `identity_status` + `unlock`/`migrate` branch.
- `ConnectDialog.tsx:131,188` `generateKeypair()` → these must NOT mint a key
  directly anymore; identity creation happens only in the gate via
  `create_identity` (so every new key is encrypted at birth). ConnectDialog
  assumes an identity already exists by the time it runs.

### Result/error types (serde, returned across the Tauri boundary)

```
enum IdentityStatus { None, Plaintext, Encrypted }            // serde-tagged
struct CreateIdentityResult { public_key: String, recovery_phrase: String }
enum UnlockError  { IncorrectPin, Corrupt(String), Io(String) }
enum RestoreError { InvalidPhrase, BadPin, Io(String) }
```

Only the **public** key and the recovery phrase cross to the frontend; the
private key never does (preserves audit finding #4).

## Frontend gate

A gate component runs before the main app. On mount it calls `identity_status()`
and branches:

- **`None`** → *Set PIN* screen: enter + confirm 4-digit PIN → `create_identity`
  → show the 24-word recovery phrase with a "write this down, it unlocks your
  account" warning and a "Restore from recovery phrase" link as an alternate
  entry point.
- **`Plaintext`** → *Secure your account* screen: one-time prompt explaining the
  key was unprotected, set a 4-digit PIN → `migrate_plaintext_identity` → show
  recovery phrase.
- **`Encrypted`** → *Enter PIN* screen: 4-digit entry → `unlock_identity`;
  wrong PIN shows "Incorrect PIN" inline and lets the user retry; a "Forgot PIN?
  Restore from recovery phrase" link → restore flow.
- **Restore flow** → enter 24-word phrase + new PIN → `restore_identity`.

Only after a command loads the key into `AppState` does the app proceed to its
normal connect/loading path. The current unconditional `loadIdentity()` calls in
`App.tsx:39` and `AppShell.tsx:81` are replaced by this gated flow.

## Error handling

- Wrong PIN → `UnlockError::IncorrectPin` → inline retry, no lockout.
- Corrupt/short blob → `UnlockError::Corrupt` → surface a clear message and offer
  the restore-from-phrase path (the user's escape hatch).
- Bad recovery phrase → `RestoreError::InvalidPhrase` (BIP39 checksum) → inline
  message, let them re-enter.
- PIN not exactly 4 digits → validation error before any crypto runs.
- File I/O errors → typed `Io(String)`, surfaced to the user.

## Testing

Most logic is testable by **observation** with no GUI (this is the point —
unlike the original gap, the fix is provable):

**`farder-crypto` unit tests**
- `export_encrypted`/`import_encrypted` round-trip under the new hardened params;
  wrong passphrase fails (GCM auth).
- `recovery::phrase_from_key` → `key_from_phrase` round-trips to the identical 32
  bytes; a tampered phrase fails the BIP39 checksum.

**`identity.rs` unit tests (drive the real module against a temp dir)**
- `create_identity` writes a blob whose bytes **do not equal** the raw private
  key (the inverse of the audit's current state — closes the gap, observably),
  and which `unlock_identity` reopens with the right PIN.
- Wrong PIN → `IncorrectPin`; the key is **not** loaded into state.
- `identity_status` returns `Plaintext` for a 32-byte file, `Encrypted` for a
  blob, `None` for an absent file.
- `migrate_plaintext_identity` turns a 32-byte plaintext file into a blob that
  (a) no longer equals the raw key and (b) unlocks with the PIN; the recovered
  key equals the original (migration is lossless).
- `restore_identity` from a phrase produces a blob that unlocks and yields the
  original key.

**Workspace observation test (extends `tests/security_observation.rs`)**
- A test asserting that the on-disk identity artifact produced by the real
  encryption path is not the raw private key — the standing regression guard for
  this audit finding.

## Out of scope (YAGNI / future)

- Change-PIN UI (re-encrypt under a new PIN) — straightforward later add.
- OS-keychain storage as an alternative to PIN.
- Zeroizing the in-memory key on drop (audit finding #4 minor; separate).
- The relay/IP-masking feature (audit Gap #3) is the **next** spec, brainstormed
  separately after this ships.

## Dependencies

- `bip39` crate (recovery phrase) added to `farder-crypto`.
- No new frontend deps (gate is plain React + existing styles).
