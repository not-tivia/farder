# Identity Key Encryption at Rest — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Encrypt the Farder identity private key at rest behind a 4-digit PIN (required every launch), with a 24-word BIP39 recovery phrase and transparent migration of existing plaintext keys.

**Architecture:** Crypto primitives live in `farder-crypto` (strengthened Argon2 in `export_encrypted`/`import_encrypted`, plus a new `recovery` module). A new focused `client/src-tauri/src/identity.rs` module owns the on-disk key file (format detection, create/unlock/migrate/restore) via an `IdentityStore` struct that takes an explicit directory so it is unit-testable; it also hosts the thin `#[tauri::command]` wrappers (matching the `themes.rs` pattern). A React `IdentityGate` runs before the main app and walks the user through Set-PIN / Enter-PIN / Migrate / Restore.

**Tech Stack:** Rust, `argon2` 0.5, `aes-gcm`, `bip39` 2, Tauri 2, React + TypeScript.

**Spec:** `docs/superpowers/specs/2026-06-05-identity-key-encryption-design.md`

**Verification note (CLAUDE.md):** Rust logic is TDD'd and runnable in this environment. The React gate **cannot** be exercised in WSL (needs the GUI); its tasks are verified by `npx tsc --noEmit` and must be marked **UNVERIFIED — needs Windows GUI run** until the user confirms on `C:\Users\Deez\farder`.

---

## File Structure

**`crates/farder-crypto/`**
- `Cargo.toml` — add `bip39 = "2"`.
- `src/identity.rs` — replace `Argon2::default()` in `export_encrypted`/`import_encrypted` with a shared hardened `identity_kdf()`.
- `src/recovery.rs` *(new)* — `phrase_from_key` / `key_from_phrase` (BIP39).
- `src/lib.rs` — `pub mod recovery;`.

**`tests/`** (root workspace; can see `farder-crypto` only)
- `security_observation.rs` — add `encrypted_identity_blob_is_not_the_raw_private_key`.

**`client/src-tauri/`** (separate workspace)
- `Cargo.toml` — add `[dev-dependencies] tempfile = "3"`.
- `src/identity.rs` *(new)* — `IdentityStore` + IPC types + the 5 `#[tauri::command]`s + tests.
- `src/commands.rs` — make `farder_data_dir` `pub(crate)`; later remove `generate_keypair`/`load_identity`/`key_path`.
- `src/main.rs` — `mod identity;`; register the 5 commands; later drop the 2 old ones.

**`client/src/`**
- `lib/tauri-bridge.ts` — add 5 bindings + types; later remove `generateKeypair`/`loadIdentity`.
- `components/IdentityGate.tsx` *(new)* — the gate + sub-screens.
- `App.tsx` — mount the gate before the main init flow.
- `components/ConnectDialog.tsx` — stop minting keypairs directly.

**`docs/modules/`**
- `tauri-commands.md` — add the 5 commands, remove the 2 old.
- `ARCHITECTURE.md` — note the identity-at-rest path.

---

## Task 1: Harden Argon2 in the identity key-wrapping crypto

**Files:**
- Modify: `crates/farder-crypto/src/identity.rs` (the `export_encrypted`/`import_encrypted` methods near lines 41-78)
- Test: same file's `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test** — append to the tests module in `crates/farder-crypto/src/identity.rs`:

```rust
    #[test]
    fn encrypted_export_roundtrips_and_rejects_wrong_pin() {
        let kp = Keypair::generate();
        let raw = *kp.signing_key_bytes();
        let blob = kp.export_encrypted("1234").expect("encrypt");
        // Not the raw key, and right/wrong passphrase behave correctly.
        assert_ne!(blob.as_slice(), &raw[..]);
        let back = Keypair::import_encrypted(&blob, "1234").expect("decrypt");
        assert_eq!(back.signing_key_bytes(), &raw);
        assert!(Keypair::import_encrypted(&blob, "0000").is_err());
    }
```

- [ ] **Step 2: Add the shared KDF helper.** Near the top of `crates/farder-crypto/src/identity.rs` (after the `use` block), add:

```rust
/// Argon2id parameters used to wrap the identity key at rest. Tuned high on
/// purpose: a 4-digit PIN is only 10,000 combinations, so each guess is made
/// expensive (~0.3-0.5s) to blunt offline brute force. Runs once per launch.
/// MUST be identical for encrypt and decrypt, or decryption fails.
fn identity_kdf() -> argon2::Argon2<'static> {
    // 64 MiB memory, 3 iterations, 1 lane, 32-byte output.
    let params = argon2::Params::new(64 * 1024, 3, 1, Some(32))
        .expect("valid argon2 params");
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
}
```

- [ ] **Step 3: Use the helper in both methods.** In `export_encrypted`, replace:

```rust
        argon2::Argon2::default()
            .hash_password_into(passphrase.as_bytes(), &salt, &mut derived_key)
            .map_err(|e| anyhow::anyhow!("argon2 error: {}", e))?;
```

with:

```rust
        identity_kdf()
            .hash_password_into(passphrase.as_bytes(), &salt, &mut derived_key)
            .map_err(|e| anyhow::anyhow!("argon2 error: {}", e))?;
```

In `import_encrypted`, replace the identical `argon2::Argon2::default()` call (the one using `salt`, not `&salt`) the same way:

```rust
        identity_kdf()
            .hash_password_into(passphrase.as_bytes(), salt, &mut derived_key)
            .map_err(|e| anyhow::anyhow!("argon2 error: {}", e))?;
```

- [ ] **Step 4: Run the tests**

Run: `cd ~/farder && cargo test -p farder-crypto identity::`
Expected: PASS, including `encrypted_export_roundtrips_and_rejects_wrong_pin` and the existing identity tests.

- [ ] **Step 5: Commit**

```bash
cd ~/farder && git add crates/farder-crypto/src/identity.rs && \
git commit -m "crypto: harden Argon2 for identity key-at-rest wrapping"
```

---

## Task 2: Recovery-phrase module (BIP39)

**Files:**
- Modify: `crates/farder-crypto/Cargo.toml`
- Modify: `crates/farder-crypto/src/lib.rs`
- Create: `crates/farder-crypto/src/recovery.rs`

- [ ] **Step 1: Add the dependency.** In `crates/farder-crypto/Cargo.toml`, under `[dependencies]`, add:

```toml
bip39 = "2"
```

- [ ] **Step 2: Register the module.** In `crates/farder-crypto/src/lib.rs`, add after `pub mod profile;`:

```rust
pub mod recovery;
```

- [ ] **Step 3: Write the module with its failing tests.** Create `crates/farder-crypto/src/recovery.rs`:

```rust
//! Human-writable recovery phrase for the identity key (BIP39, 24 words).
//!
//! The 32-byte Ed25519 signing key is used directly as BIP39 entropy, so the
//! phrase encodes the key itself and is as sensitive as the key. The BIP39
//! checksum catches typos when the user restores.

use anyhow::{anyhow, Result};
use bip39::Mnemonic;

/// Encode a 32-byte key as a 24-word BIP39 phrase.
pub fn phrase_from_key(key: &[u8; 32]) -> Result<String> {
    let mnemonic =
        Mnemonic::from_entropy(key).map_err(|e| anyhow!("failed to build recovery phrase: {e}"))?;
    Ok(mnemonic.to_string())
}

/// Decode a 24-word BIP39 phrase back to a 32-byte key. Fails on a bad
/// checksum, unknown words, or wrong length.
pub fn key_from_phrase(phrase: &str) -> Result<[u8; 32]> {
    let mnemonic =
        Mnemonic::parse(phrase.trim()).map_err(|e| anyhow!("invalid recovery phrase: {e}"))?;
    let entropy = mnemonic.to_entropy();
    let key: [u8; 32] = entropy
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("recovery phrase does not encode a 32-byte key"))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrase_roundtrips_to_the_same_key() {
        let key: [u8; 32] = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
            25, 26, 27, 28, 29, 30, 31, 32,
        ];
        let phrase = phrase_from_key(&key).expect("encode");
        assert_eq!(phrase.split_whitespace().count(), 24);
        let back = key_from_phrase(&phrase).expect("decode");
        assert_eq!(back, key);
    }

    #[test]
    fn tampered_phrase_fails_checksum() {
        let key = [7u8; 32];
        let phrase = phrase_from_key(&key).expect("encode");
        // Swap the first word for another valid word -> checksum breaks.
        let mut words: Vec<&str> = phrase.split_whitespace().collect();
        words[0] = if words[0] == "zoo" { "abandon" } else { "zoo" };
        let tampered = words.join(" ");
        assert!(key_from_phrase(&tampered).is_err());
    }

    #[test]
    fn garbage_phrase_fails() {
        assert!(key_from_phrase("not a real recovery phrase at all").is_err());
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cd ~/farder && cargo test -p farder-crypto recovery::`
Expected: PASS (3 tests). If a `bip39` 2.x method name differs (`from_entropy` / `parse` / `to_entropy`), adjust to the crate's actual API and re-run — the behavior, not the exact spelling, is what matters.

- [ ] **Step 5: Commit**

```bash
cd ~/farder && git add crates/farder-crypto/Cargo.toml crates/farder-crypto/src/lib.rs crates/farder-crypto/src/recovery.rs && \
git commit -m "crypto: add BIP39 recovery-phrase module for identity key"
```

---

## Task 3: Workspace observation test — blob is not the raw key

**Files:**
- Modify: `tests/security_observation.rs` (this file already defines `contains_subslice` and imports `Keypair`)

- [ ] **Step 1: Add the observation test.** Append to `tests/security_observation.rs`:

```rust
/// The audit (2026-06-05) found the identity key was stored as raw plaintext.
/// This is the standing regression guard for the fix: the encrypted blob the
/// real key-wrapping path produces must neither equal nor contain the raw
/// private key, must reopen under the right PIN, and must reject the wrong one.
#[test]
fn encrypted_identity_blob_is_not_the_raw_private_key() {
    let kp = Keypair::generate();
    let raw = *kp.signing_key_bytes();
    let blob = kp.export_encrypted("1234").expect("encrypt identity");

    assert_ne!(blob.as_slice(), &raw[..], "blob equals the raw private key");
    assert!(
        !contains_subslice(&blob, &raw),
        "raw private key bytes appear verbatim inside the encrypted blob"
    );

    let back = Keypair::import_encrypted(&blob, "1234").expect("decrypt identity");
    assert_eq!(back.signing_key_bytes(), &raw);
    assert!(
        Keypair::import_encrypted(&blob, "9999").is_err(),
        "wrong PIN must fail"
    );
}
```

- [ ] **Step 2: Run it**

Run: `cd ~/farder && cargo test --test security_observation`
Expected: PASS — now 3 tests (the 2 DM tests plus this one).

- [ ] **Step 3: Commit**

```bash
cd ~/farder && git add tests/security_observation.rs && \
git commit -m "test: observation guard that identity blob is not the raw key"
```

---

## Task 4: `IdentityStore` scaffold + status detection

**Files:**
- Modify: `client/src-tauri/Cargo.toml` (add dev-dependency)
- Modify: `client/src-tauri/src/commands.rs` (make `farder_data_dir` visible)
- Modify: `client/src-tauri/src/main.rs` (declare the module)
- Create: `client/src-tauri/src/identity.rs`

- [ ] **Step 1: Add the test dependency.** Append to `client/src-tauri/Cargo.toml`:

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Expose the data dir.** In `client/src-tauri/src/commands.rs`, change:

```rust
fn farder_data_dir() -> std::path::PathBuf {
```

to:

```rust
pub(crate) fn farder_data_dir() -> std::path::PathBuf {
```

- [ ] **Step 3: Declare the module.** In `client/src-tauri/src/main.rs`, add `mod identity;` to the module list (alphabetical, after `mod display;`):

```rust
mod identity;
```

- [ ] **Step 4: Create the module with status() and its test.** Create `client/src-tauri/src/identity.rs`:

```rust
//! Identity key storage. The on-disk file at `<data dir>/identity.key` holds
//! EITHER a legacy 32-byte plaintext key (pre-encryption builds) OR an
//! encrypted blob from `Keypair::export_encrypted` (16 salt + 12 nonce + 48
//! ct+tag = 76 bytes). We detect which by length and gate access behind a
//! 4-digit PIN.
//!
//! `IdentityStore` takes an explicit directory so it is unit-testable without
//! touching the user's real home. The `#[tauri::command]` wrappers at the
//! bottom run the blocking crypto off the UI thread and load the unlocked key
//! into `AppState`.

use crate::state::AppState;
use farder_crypto::identity::Keypair;
use farder_crypto::recovery;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;

const KEY_FILE: &str = "identity.key";
const PLAINTEXT_LEN: usize = 32;
const MIN_ENCRYPTED_LEN: usize = 16 + 12 + 32 + 16; // 76

#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IdentityStatus {
    None,
    Plaintext,
    Encrypted,
}

#[derive(Serialize, Debug)]
#[serde(tag = "kind", content = "detail")]
pub enum IdentityError {
    IncorrectPin,
    InvalidPhrase,
    BadPin,
    Corrupt(String),
    Io(String),
}

pub struct CreatedIdentity {
    pub key_bytes: [u8; 32],
    pub public_key: String,
    pub recovery_phrase: String,
}

pub struct UnlockedIdentity {
    pub key_bytes: [u8; 32],
    pub public_key: String,
}

#[derive(Serialize)]
pub struct CreateIdentityResult {
    pub public_key: String,
    pub recovery_phrase: String,
}

pub struct IdentityStore {
    dir: PathBuf,
}

impl IdentityStore {
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn default_location() -> Self {
        Self::at(crate::commands::farder_data_dir())
    }

    fn key_path(&self) -> PathBuf {
        self.dir.join(KEY_FILE)
    }

    /// Classify the on-disk file by length without decrypting.
    pub fn status(&self) -> IdentityStatus {
        match std::fs::metadata(self.key_path()) {
            Ok(m) if m.len() as usize == PLAINTEXT_LEN => IdentityStatus::Plaintext,
            Ok(_) => IdentityStatus::Encrypted,
            Err(_) => IdentityStatus::None,
        }
    }
}

fn validate_pin(pin: &str) -> Result<(), IdentityError> {
    if pin.len() == 4 && pin.chars().all(|c| c.is_ascii_digit()) {
        Ok(())
    } else {
        Err(IdentityError::BadPin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, IdentityStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = IdentityStore::at(dir.path().to_path_buf());
        (dir, store)
    }

    #[test]
    fn status_none_when_absent() {
        let (_d, s) = store();
        assert_eq!(s.status(), IdentityStatus::None);
    }

    #[test]
    fn status_plaintext_for_32_byte_file() {
        let (_d, s) = store();
        std::fs::write(s.key_path(), [0u8; 32]).unwrap();
        assert_eq!(s.status(), IdentityStatus::Plaintext);
    }

    #[test]
    fn status_encrypted_for_blob() {
        let (_d, s) = store();
        std::fs::write(s.key_path(), vec![0u8; MIN_ENCRYPTED_LEN]).unwrap();
        assert_eq!(s.status(), IdentityStatus::Encrypted);
    }

    #[test]
    fn pin_validation() {
        assert!(validate_pin("1234").is_ok());
        assert!(validate_pin("000").is_err());
        assert!(validate_pin("12345").is_err());
        assert!(validate_pin("12a4").is_err());
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cd ~/farder/client/src-tauri && cargo test identity::`
Expected: PASS (4 tests). The crate compiles with the new module and dev-dependency.

- [ ] **Step 6: Commit**

```bash
cd ~/farder && git add client/src-tauri/Cargo.toml client/src-tauri/src/commands.rs client/src-tauri/src/main.rs client/src-tauri/src/identity.rs && \
git commit -m "client: scaffold IdentityStore with on-disk status detection"
```

---

## Task 5: `create` and `unlock`

**Files:**
- Modify: `client/src-tauri/src/identity.rs`

- [ ] **Step 1: Write the failing tests.** Add to the `tests` module in `client/src-tauri/src/identity.rs`:

```rust
    #[test]
    fn create_then_unlock_roundtrips_and_hides_key() {
        let (_d, s) = store();
        let created = s.create("1234").expect("create");
        assert_eq!(created.recovery_phrase.split_whitespace().count(), 24);

        // OBSERVATION: the on-disk bytes are NOT the raw private key.
        let on_disk = std::fs::read(s.key_path()).unwrap();
        assert_ne!(on_disk.as_slice(), &created.key_bytes[..]);
        assert!(on_disk.len() >= MIN_ENCRYPTED_LEN);

        // Right PIN reopens to the same key; wrong PIN fails.
        let unlocked = s.unlock("1234").expect("unlock");
        assert_eq!(unlocked.key_bytes, created.key_bytes);
        assert_eq!(unlocked.public_key, created.public_key);
        assert!(matches!(s.unlock("0000"), Err(IdentityError::IncorrectPin)));
    }

    #[test]
    fn create_rejects_bad_pin() {
        let (_d, s) = store();
        assert!(matches!(s.create("12"), Err(IdentityError::BadPin)));
    }
```

- [ ] **Step 2: Implement `create`, `unlock`, and the shared `seal_new`/`write_blob`.** Add these methods inside `impl IdentityStore` (after `status`):

```rust
    /// Atomically write the encrypted blob to the key file (temp + rename).
    fn write_blob(&self, blob: &[u8]) -> Result<(), IdentityError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| IdentityError::Io(e.to_string()))?;
        let tmp = self.dir.join("identity.key.tmp");
        std::fs::write(&tmp, blob).map_err(|e| IdentityError::Io(e.to_string()))?;
        std::fs::rename(&tmp, self.key_path()).map_err(|e| IdentityError::Io(e.to_string()))?;
        Ok(())
    }

    /// Encrypt `keypair` under `pin`, persist it, and return its recovery phrase.
    fn seal_new(&self, keypair: Keypair, pin: &str) -> Result<CreatedIdentity, IdentityError> {
        let blob = keypair
            .export_encrypted(pin)
            .map_err(|e| IdentityError::Io(format!("encrypt failed: {e}")))?;
        self.write_blob(&blob)?;
        let phrase = recovery::phrase_from_key(keypair.signing_key_bytes())
            .map_err(|e| IdentityError::Io(format!("phrase failed: {e}")))?;
        Ok(CreatedIdentity {
            key_bytes: *keypair.signing_key_bytes(),
            public_key: keypair.public_key().to_string(),
            recovery_phrase: phrase,
        })
    }

    /// New user: generate a fresh key, encrypt under `pin`, persist.
    pub fn create(&self, pin: &str) -> Result<CreatedIdentity, IdentityError> {
        validate_pin(pin)?;
        self.seal_new(Keypair::generate(), pin)
    }

    /// Returning user: decrypt the blob with `pin`.
    pub fn unlock(&self, pin: &str) -> Result<UnlockedIdentity, IdentityError> {
        let data = std::fs::read(self.key_path()).map_err(|e| IdentityError::Io(e.to_string()))?;
        if data.len() < MIN_ENCRYPTED_LEN {
            return Err(IdentityError::Corrupt("encrypted key too short".into()));
        }
        let keypair =
            Keypair::import_encrypted(&data, pin).map_err(|_| IdentityError::IncorrectPin)?;
        Ok(UnlockedIdentity {
            key_bytes: *keypair.signing_key_bytes(),
            public_key: keypair.public_key().to_string(),
        })
    }
```

- [ ] **Step 3: Run the tests**

Run: `cd ~/farder/client/src-tauri && cargo test identity::`
Expected: PASS (6 tests).

- [ ] **Step 4: Commit**

```bash
cd ~/farder && git add client/src-tauri/src/identity.rs && \
git commit -m "client: IdentityStore create + unlock (encrypted at rest)"
```

---

## Task 6: `migrate` (existing plaintext key)

**Files:**
- Modify: `client/src-tauri/src/identity.rs`

- [ ] **Step 1: Write the failing test.** Add to the `tests` module:

```rust
    #[test]
    fn migrate_plaintext_is_lossless_and_encrypts() {
        let (_d, s) = store();
        // Simulate a legacy plaintext identity on disk.
        let original = Keypair::generate();
        let raw = *original.signing_key_bytes();
        std::fs::write(s.key_path(), raw).unwrap();
        assert_eq!(s.status(), IdentityStatus::Plaintext);

        let created = s.migrate("1234").expect("migrate");
        // Same key preserved (lossless)...
        assert_eq!(created.key_bytes, raw);
        // ...now stored encrypted (not the raw bytes) and classed Encrypted.
        let on_disk = std::fs::read(s.key_path()).unwrap();
        assert_ne!(on_disk.as_slice(), &raw[..]);
        assert_eq!(s.status(), IdentityStatus::Encrypted);
        // Unlocks with the chosen PIN.
        assert_eq!(s.unlock("1234").unwrap().key_bytes, raw);
    }
```

- [ ] **Step 2: Implement `migrate`.** Add inside `impl IdentityStore`:

```rust
    /// One-time: read the legacy 32-byte plaintext key and re-store it
    /// encrypted under `pin`. The key value is preserved.
    pub fn migrate(&self, pin: &str) -> Result<CreatedIdentity, IdentityError> {
        validate_pin(pin)?;
        let raw = std::fs::read(self.key_path()).map_err(|e| IdentityError::Io(e.to_string()))?;
        let bytes: [u8; 32] = raw
            .as_slice()
            .try_into()
            .map_err(|_| IdentityError::Corrupt("plaintext key not 32 bytes".into()))?;
        self.seal_new(Keypair::from_signing_key_bytes(&bytes), pin)
    }
```

- [ ] **Step 3: Run the tests**

Run: `cd ~/farder/client/src-tauri && cargo test identity::`
Expected: PASS (7 tests).

- [ ] **Step 4: Commit**

```bash
cd ~/farder && git add client/src-tauri/src/identity.rs && \
git commit -m "client: IdentityStore migrate plaintext key to encrypted"
```

---

## Task 7: `restore` (from recovery phrase)

**Files:**
- Modify: `client/src-tauri/src/identity.rs`

- [ ] **Step 1: Write the failing test.** Add to the `tests` module:

```rust
    #[test]
    fn restore_from_phrase_rebuilds_identity() {
        let (_d, s) = store();
        let created = s.create("1234").expect("create");
        let phrase = created.recovery_phrase.clone();

        // A fresh store (new device) restores from the phrase under a new PIN.
        let (_d2, s2) = store();
        let restored = s2.restore(&phrase, "5678").expect("restore");
        assert_eq!(restored.key_bytes, created.key_bytes);
        assert_eq!(s2.unlock("5678").unwrap().key_bytes, created.key_bytes);

        assert!(matches!(
            s2.restore("totally invalid phrase here", "5678"),
            Err(IdentityError::InvalidPhrase)
        ));
    }
```

- [ ] **Step 2: Implement `restore`.** Add inside `impl IdentityStore`:

```rust
    /// Forgot-PIN path: rebuild the key from its recovery phrase and re-store
    /// it encrypted under a new `pin`.
    pub fn restore(&self, phrase: &str, pin: &str) -> Result<UnlockedIdentity, IdentityError> {
        validate_pin(pin)?;
        let bytes = recovery::key_from_phrase(phrase).map_err(|_| IdentityError::InvalidPhrase)?;
        let created = self.seal_new(Keypair::from_signing_key_bytes(&bytes), pin)?;
        Ok(UnlockedIdentity {
            key_bytes: created.key_bytes,
            public_key: created.public_key,
        })
    }
```

- [ ] **Step 3: Run the tests**

Run: `cd ~/farder/client/src-tauri && cargo test identity::`
Expected: PASS (8 tests).

- [ ] **Step 4: Commit**

```bash
cd ~/farder && git add client/src-tauri/src/identity.rs && \
git commit -m "client: IdentityStore restore from BIP39 recovery phrase"
```

---

## Task 8: Tauri commands + registration (old commands kept for now)

**Files:**
- Modify: `client/src-tauri/src/identity.rs` (append the commands)
- Modify: `client/src-tauri/src/main.rs` (register them)

- [ ] **Step 1: Add the command wrappers.** Append to `client/src-tauri/src/identity.rs` (outside the `tests` module):

```rust
// ---------------------------------------------------------------------------
// Tauri commands — thin wrappers. The Argon2 work is blocking, so each runs on
// a blocking thread to avoid freezing the webview, then loads the unlocked key
// into AppState. Only the PUBLIC key (and recovery phrase) cross to the
// frontend; the private key never does.
// ---------------------------------------------------------------------------

fn store_key(state: &Arc<AppState>, key_bytes: [u8; 32]) -> Result<(), IdentityError> {
    let mut lock = state
        .signing_key_bytes
        .lock()
        .map_err(|e| IdentityError::Io(format!("state lock poisoned: {e}")))?;
    *lock = Some(key_bytes);
    Ok(())
}

#[tauri::command]
pub fn identity_status() -> IdentityStatus {
    IdentityStore::default_location().status()
}

#[tauri::command]
pub async fn create_identity(
    state: State<'_, Arc<AppState>>,
    pin: String,
) -> Result<CreateIdentityResult, IdentityError> {
    let created = tauri::async_runtime::spawn_blocking(move || {
        IdentityStore::default_location().create(&pin)
    })
    .await
    .map_err(|e| IdentityError::Io(format!("task join error: {e}")))??;
    store_key(state.inner(), created.key_bytes)?;
    Ok(CreateIdentityResult {
        public_key: created.public_key,
        recovery_phrase: created.recovery_phrase,
    })
}

#[tauri::command]
pub async fn unlock_identity(
    state: State<'_, Arc<AppState>>,
    pin: String,
) -> Result<String, IdentityError> {
    let unlocked = tauri::async_runtime::spawn_blocking(move || {
        IdentityStore::default_location().unlock(&pin)
    })
    .await
    .map_err(|e| IdentityError::Io(format!("task join error: {e}")))??;
    store_key(state.inner(), unlocked.key_bytes)?;
    Ok(unlocked.public_key)
}

#[tauri::command]
pub async fn migrate_plaintext_identity(
    state: State<'_, Arc<AppState>>,
    pin: String,
) -> Result<CreateIdentityResult, IdentityError> {
    let created = tauri::async_runtime::spawn_blocking(move || {
        IdentityStore::default_location().migrate(&pin)
    })
    .await
    .map_err(|e| IdentityError::Io(format!("task join error: {e}")))??;
    store_key(state.inner(), created.key_bytes)?;
    Ok(CreateIdentityResult {
        public_key: created.public_key,
        recovery_phrase: created.recovery_phrase,
    })
}

#[tauri::command]
pub async fn restore_identity(
    state: State<'_, Arc<AppState>>,
    phrase: String,
    pin: String,
) -> Result<String, IdentityError> {
    let unlocked = tauri::async_runtime::spawn_blocking(move || {
        IdentityStore::default_location().restore(&phrase, &pin)
    })
    .await
    .map_err(|e| IdentityError::Io(format!("task join error: {e}")))??;
    store_key(state.inner(), unlocked.key_bytes)?;
    Ok(unlocked.public_key)
}
```

- [ ] **Step 2: Register the commands.** In `client/src-tauri/src/main.rs`, inside the `tauri::generate_handler![ ... ]` list, add after `commands::get_public_key,`:

```rust
            identity::identity_status,
            identity::create_identity,
            identity::unlock_identity,
            identity::migrate_plaintext_identity,
            identity::restore_identity,
```

- [ ] **Step 3: Build to verify the commands compile and register**

Run: `cd ~/farder/client/src-tauri && cargo build 2>&1 | tail -5`
Expected: builds (warnings OK). No "unresolved" or trait errors.

- [ ] **Step 4: Commit**

```bash
cd ~/farder && git add client/src-tauri/src/identity.rs client/src-tauri/src/main.rs && \
git commit -m "client: register identity_status/create/unlock/migrate/restore commands"
```

---

## Task 9: Frontend bridge bindings

**Files:**
- Modify: `client/src/lib/tauri-bridge.ts` (the Identity section around lines 30-46)

- [ ] **Step 1: Add the new bindings and types.** In `client/src/lib/tauri-bridge.ts`, in the `// ── Identity` section, add (do NOT remove `generateKeypair`/`loadIdentity` yet — they are removed in Task 12):

```ts
export type IdentityStatus = "none" | "plaintext" | "encrypted";

export interface CreateIdentityResult {
  public_key: string;
  recovery_phrase: string;
}

export async function identityStatus(): Promise<IdentityStatus> {
  return invoke<IdentityStatus>("identity_status");
}

export async function createIdentity(pin: string): Promise<CreateIdentityResult> {
  return invoke<CreateIdentityResult>("create_identity", { pin });
}

export async function unlockIdentity(pin: string): Promise<string> {
  return invoke<string>("unlock_identity", { pin });
}

export async function migratePlaintextIdentity(pin: string): Promise<CreateIdentityResult> {
  return invoke<CreateIdentityResult>("migrate_plaintext_identity", { pin });
}

export async function restoreIdentity(phrase: string, pin: string): Promise<string> {
  return invoke<string>("restore_identity", { phrase, pin });
}
```

- [ ] **Step 2: Type-check**

Run: `cd ~/farder/client && npx tsc --noEmit`
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
cd ~/farder && git add client/src/lib/tauri-bridge.ts && \
git commit -m "client: add identity-gate bridge bindings"
```

---

## Task 10: `IdentityGate` component

**Files:**
- Create: `client/src/components/IdentityGate.tsx`

- [ ] **Step 1: Create the component.** Create `client/src/components/IdentityGate.tsx`:

```tsx
import { useEffect, useState, type ReactNode } from "react";
import * as api from "../lib/tauri-bridge";

type Screen = "loading" | "set-pin" | "enter-pin" | "migrate" | "restore" | "show-phrase";

// Reusable 4-digit PIN field (digits only, max length 4).
function PinField({
  value,
  onChange,
  autoFocus,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  autoFocus?: boolean;
  placeholder?: string;
}) {
  return (
    <input
      className="connect-input"
      type="password"
      inputMode="numeric"
      autoFocus={autoFocus}
      placeholder={placeholder ?? "4-digit PIN"}
      value={value}
      maxLength={4}
      onChange={(e) => onChange(e.target.value.replace(/\D/g, "").slice(0, 4))}
    />
  );
}

export default function IdentityGate({ onUnlocked }: { onUnlocked: () => void }) {
  const [screen, setScreen] = useState<Screen>("loading");
  const [pin, setPin] = useState("");
  const [pin2, setPin2] = useState("");
  const [phrase, setPhrase] = useState("");
  const [recoveryPhrase, setRecoveryPhrase] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .identityStatus()
      .then((s) =>
        setScreen(s === "none" ? "set-pin" : s === "plaintext" ? "migrate" : "enter-pin"),
      )
      .catch(() => setScreen("set-pin"));
  }, []);

  const reset = () => {
    setPin("");
    setPin2("");
    setError(null);
  };

  async function handleCreate() {
    if (pin.length !== 4) return setError("PIN must be 4 digits.");
    if (pin !== pin2) return setError("PINs do not match.");
    setBusy(true);
    setError(null);
    try {
      const res = await api.createIdentity(pin);
      setRecoveryPhrase(res.recovery_phrase);
      setScreen("show-phrase");
    } catch (e) {
      setError("Could not create identity. Please try again.");
      console.error("[identity] create failed:", e);
    } finally {
      setBusy(false);
    }
  }

  async function handleMigrate() {
    if (pin.length !== 4) return setError("PIN must be 4 digits.");
    if (pin !== pin2) return setError("PINs do not match.");
    setBusy(true);
    setError(null);
    try {
      const res = await api.migratePlaintextIdentity(pin);
      setRecoveryPhrase(res.recovery_phrase);
      setScreen("show-phrase");
    } catch (e) {
      setError("Could not secure your account. Please try again.");
      console.error("[identity] migrate failed:", e);
    } finally {
      setBusy(false);
    }
  }

  async function handleUnlock() {
    setBusy(true);
    setError(null);
    try {
      await api.unlockIdentity(pin);
      onUnlocked();
    } catch (e) {
      setError("Incorrect PIN.");
      setPin("");
      console.error("[identity] unlock failed:", e);
    } finally {
      setBusy(false);
    }
  }

  async function handleRestore() {
    if (pin.length !== 4) return setError("New PIN must be 4 digits.");
    setBusy(true);
    setError(null);
    try {
      await api.restoreIdentity(phrase.trim(), pin);
      onUnlocked();
    } catch (e) {
      setError("That recovery phrase or PIN was not accepted.");
      console.error("[identity] restore failed:", e);
    } finally {
      setBusy(false);
    }
  }

  const shell = (title: string, body: ReactNode) => (
    <div className="connect-screen">
      <div className="connect-dialog">
        <div className="connect-dialog-titlebar">{title}</div>
        <div className="connect-dialog-body" style={{ padding: 24 }}>
          {body}
          {error && <p style={{ color: "var(--danger, #d9534f)", marginTop: 8 }}>{error}</p>}
        </div>
      </div>
    </div>
  );

  if (screen === "loading") return shell("Farder", <p>Loading...</p>);

  if (screen === "set-pin")
    return shell(
      "Set a PIN",
      <>
        <p>Choose a 4-digit PIN. You'll enter it each time you open Farder. It encrypts your identity on this device.</p>
        <PinField value={pin} onChange={setPin} autoFocus placeholder="Choose PIN" />
        <PinField value={pin2} onChange={setPin2} placeholder="Confirm PIN" />
        <button className="connect-button" disabled={busy} onClick={handleCreate}>
          {busy ? "Creating..." : "Create identity"}
        </button>
        <button className="connect-link" onClick={() => { reset(); setScreen("restore"); }}>
          Restore from recovery phrase
        </button>
      </>,
    );

  if (screen === "migrate")
    return shell(
      "Secure your account",
      <>
        <p>Your identity key was stored unprotected. Set a 4-digit PIN now to encrypt it on this device.</p>
        <PinField value={pin} onChange={setPin} autoFocus placeholder="Choose PIN" />
        <PinField value={pin2} onChange={setPin2} placeholder="Confirm PIN" />
        <button className="connect-button" disabled={busy} onClick={handleMigrate}>
          {busy ? "Securing..." : "Secure account"}
        </button>
      </>,
    );

  if (screen === "enter-pin")
    return shell(
      "Enter your PIN",
      <>
        <PinField value={pin} onChange={setPin} autoFocus />
        <button
          className="connect-button"
          disabled={busy || pin.length !== 4}
          onClick={handleUnlock}
        >
          {busy ? "Unlocking..." : "Unlock"}
        </button>
        <button className="connect-link" onClick={() => { reset(); setScreen("restore"); }}>
          Forgot PIN? Restore from recovery phrase
        </button>
      </>,
    );

  if (screen === "restore")
    return shell(
      "Restore from recovery phrase",
      <>
        <p>Enter your 24-word recovery phrase and choose a new 4-digit PIN.</p>
        <textarea
          className="connect-input"
          rows={3}
          placeholder="word1 word2 word3 ..."
          value={phrase}
          onChange={(e) => setPhrase(e.target.value)}
        />
        <PinField value={pin} onChange={setPin} placeholder="New PIN" />
        <button className="connect-button" disabled={busy} onClick={handleRestore}>
          {busy ? "Restoring..." : "Restore"}
        </button>
        <button className="connect-link" onClick={() => { reset(); setScreen("enter-pin"); }}>
          Back
        </button>
      </>,
    );

  // show-phrase
  return shell(
    "Save your recovery phrase",
    <>
      <p>
        <strong>Write these 24 words down and keep them safe.</strong> They are the
        only way to recover your account if you forget your PIN. Anyone with this
        phrase can access your account.
      </p>
      <p style={{ fontFamily: "monospace", background: "var(--input-bg, #00000022)", padding: 12, borderRadius: 6, wordSpacing: 4 }}>
        {recoveryPhrase}
      </p>
      <button className="connect-button" onClick={onUnlocked}>
        I've saved it - continue
      </button>
    </>,
  );
}
```

- [ ] **Step 2: Type-check**

Run: `cd ~/farder/client && npx tsc --noEmit`
Expected: no errors. (If `.connect-input` / `.connect-button` / `.connect-link` classes don't exist in the theme CSS, that's purely cosmetic and does not affect tsc — styling polish is deferred to the Windows run.)

- [ ] **Step 3: Commit**

```bash
cd ~/farder && git add client/src/components/IdentityGate.tsx && \
git commit -m "client: IdentityGate component (set/enter/migrate/restore PIN)"
```

---

## Task 11: Mount the gate; stop ConnectDialog minting keys

**Files:**
- Modify: `client/src/App.tsx` (the `AppInner` component, lines ~17-90)
- Modify: `client/src/components/ConnectDialog.tsx` (`handleContinue` ~line 121, `handleJoin` ~line 186)

- [ ] **Step 1: Gate the app behind unlock.** In `client/src/App.tsx`, add the import near the other component imports:

```tsx
import IdentityGate from "./components/IdentityGate";
```

Add an `unlocked` state and gate the init effect. Change the top of `AppInner`:

```tsx
function AppInner() {
  const { state, dispatch } = useApp();
  const [initializing, setInitializing] = useState(true);
  const [unlocked, setUnlocked] = useState(false);
  useServerEvents();
```

Change the init effect's guard so it only runs once the identity is unlocked — replace `useEffect(() => { if (initStarted) return; initStarted = true; async function init() {` ... and its first two lines:

```tsx
  useEffect(() => {
    if (!unlocked) return;
    if (initStarted) return;
    initStarted = true;
    async function init() {
      dispatch({ type: "SET_IDENTITY" });

      // Restart any locally-managed servers first, then get the updated list
```

(Removing the `const key = await api.loadIdentity(); if (!key) { ... return; }` lines — the gate guarantees an identity is loaded into `AppState` before `unlocked` flips true.) Update the effect's dependency array to `[unlocked]`.

Then, before the `if (initializing)` block, render the gate when not yet unlocked:

```tsx
  if (!unlocked) {
    return <IdentityGate onUnlocked={() => setUnlocked(true)} />;
  }
```

- [ ] **Step 2: Stop ConnectDialog from generating keypairs.** In `client/src/components/ConnectDialog.tsx`:

In `handleContinue`, replace:

```tsx
      const key = await api.generateKeypair();
      await api.setDisplayName(trimmed);
      setPubKey(key);
```

with (the identity already exists — just fetch the public key and set the name):

```tsx
      const key = await api.getPublicKey();
      await api.setDisplayName(trimmed);
      if (key) setPubKey(key);
```

In `handleJoin`, replace the whole `if (!pubKey) { ... }` block:

```tsx
    if (!pubKey) {
      try {
        const key = await api.generateKeypair();
        setPubKey(key);
        dispatch({ type: "SET_IDENTITY" });
      } catch (e) {
        setError(String(e));
        setLoading(false);
        return;
      }
    }
```

with:

```tsx
    if (!pubKey) {
      const key = await api.getPublicKey();
      if (key) {
        setPubKey(key);
        dispatch({ type: "SET_IDENTITY" });
      }
    }
```

Also in ConnectDialog's own `init()` effect, replace `api.loadIdentity()` with `api.getPublicKey()` (the gate has already loaded the key; we only need the public key string):

```tsx
      const [existingKey, existingName] = await Promise.allSettled([
        api.getPublicKey(),
        api.getDisplayName(),
      ]);
```

- [ ] **Step 3: Type-check**

Run: `cd ~/farder/client && npx tsc --noEmit`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
cd ~/farder && git add client/src/App.tsx client/src/components/ConnectDialog.tsx && \
git commit -m "client: gate app behind IdentityGate; ConnectDialog no longer mints keys"
```

---

## Task 12: Remove the plaintext-writing commands

**Files:**
- Modify: `client/src-tauri/src/commands.rs` (delete `generate_keypair`, `load_identity`, and the now-unused `key_path`)
- Modify: `client/src-tauri/src/main.rs` (drop their registrations)
- Modify: `client/src/lib/tauri-bridge.ts` (delete `generateKeypair`, `loadIdentity`)
- Modify: `client/src-tauri/src/AppShell.tsx` reference — see Step 4

- [ ] **Step 1: Delete the backend commands.** In `client/src-tauri/src/commands.rs`, delete the entire `key_path` fn (the `fn key_path() -> ... { farder_data_dir().join("identity.key") }`), the `generate_keypair` command, and the `load_identity` command (the three items shown in `commands.rs:62-88`). Leave `get_public_key` intact.

- [ ] **Step 2: Drop their registrations.** In `client/src-tauri/src/main.rs`, remove the lines:

```rust
            commands::generate_keypair,
            commands::load_identity,
```

- [ ] **Step 3: Remove the bridge bindings.** In `client/src/lib/tauri-bridge.ts`, delete:

```ts
export async function generateKeypair(): Promise<string> {
  return invoke<string>("generate_keypair");
}

export async function loadIdentity(): Promise<string | null> {
  return invoke<string | null>("load_identity");
}
```

- [ ] **Step 4: Fix the remaining caller in AppShell.** In `client/src/components/AppShell.tsx` (around line 81), replace the `await api.loadIdentity();` call with `await api.getPublicKey();` (AppShell only needs to confirm the identity is present; the gate already loaded it):

```tsx
          await api.getPublicKey();
```

- [ ] **Step 5: Verify no stragglers**

Run: `cd ~/farder && grep -rn "loadIdentity\|generateKeypair\|generate_keypair\|load_identity" client/src client/src-tauri/src`
Expected: NO results (all references gone).

- [ ] **Step 6: Build + type-check**

Run: `cd ~/farder/client/src-tauri && cargo build 2>&1 | tail -3 && cd ~/farder/client && npx tsc --noEmit`
Expected: Rust builds; tsc reports no errors.

- [ ] **Step 7: Commit**

```bash
cd ~/farder && git add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs client/src/lib/tauri-bridge.ts client/src/components/AppShell.tsx && \
git commit -m "client: remove plaintext-writing generate_keypair/load_identity"
```

---

## Task 13: Documentation (per CLAUDE.md documentation discipline)

**Files:**
- Modify: `docs/modules/tauri-commands.md`
- Modify: `ARCHITECTURE.md`

- [ ] **Step 1: Document the new commands.** In `docs/modules/tauri-commands.md`, in the Identity group, REMOVE the `generate_keypair` and `load_identity` entries and ADD:

```markdown
### `identity_status() -> IdentityStatus`

**What it does:** classifies `<data dir>/identity.key` by length without
decrypting — `"none"` (absent), `"plaintext"` (legacy 32-byte key needing
migration), or `"encrypted"`. Drives which screen the IdentityGate shows.
**invoke name:** `"identity_status"` → `identityStatus()`.

### `create_identity(pin) -> { public_key, recovery_phrase }`

**What it does:** generates a fresh identity, encrypts it under a 4-digit PIN
(Argon2id + AES-256-GCM), writes the blob, loads the key into `AppState`, and
returns the public key plus a 24-word BIP39 recovery phrase. Async (crypto runs
off the UI thread). **invoke name:** `"create_identity"` → `createIdentity(pin)`.

### `unlock_identity(pin) -> public_key`

**What it does:** decrypts the stored blob with the PIN and loads the key into
`AppState`; returns the public key. Errors with `IncorrectPin` on a wrong PIN
(no lockout). **invoke name:** `"unlock_identity"` → `unlockIdentity(pin)`.

### `migrate_plaintext_identity(pin) -> { public_key, recovery_phrase }`

**What it does:** one-time — reads the legacy plaintext key, re-stores it
encrypted under the PIN (value preserved), loads it, and returns the recovery
phrase. **invoke name:** `"migrate_plaintext_identity"` → `migratePlaintextIdentity(pin)`.

### `restore_identity(phrase, pin) -> public_key`

**What it does:** rebuilds the key from a 24-word recovery phrase, re-stores it
encrypted under a new PIN, loads it. Errors with `InvalidPhrase` on a bad
checksum. **invoke name:** `"restore_identity"` → `restoreIdentity(phrase, pin)`.

All five live in `client/src-tauri/src/identity.rs` (logic on `IdentityStore`).
The private key never crosses the Tauri boundary — only the public key and the
recovery phrase do.
```

- [ ] **Step 2: Note the path in ARCHITECTURE.md.** In `ARCHITECTURE.md`, add a line to the identity/security section:

```markdown
- **Identity at rest:** `client/src-tauri/src/identity.rs` (`IdentityStore`)
  stores the Ed25519 key encrypted (Argon2id + AES-256-GCM) behind a 4-digit
  PIN; `farder-crypto::recovery` provides a BIP39 recovery phrase. See
  `docs/superpowers/audits/2026-06-05-privacy-security-wiring-audit.md` Gap #2.
```

- [ ] **Step 3: Commit**

```bash
cd ~/farder && git add docs/modules/tauri-commands.md ARCHITECTURE.md && \
git commit -m "docs: document identity-at-rest commands and architecture"
```

---

## Final verification

- [ ] **Rust suite green:**

Run: `cd ~/farder && cargo test --workspace 2>&1 | tail -20 && cd ~/farder/client/src-tauri && cargo test 2>&1 | tail -20`
Expected: all pass, including `security_observation` (3 tests) and `identity::` (8 tests).

- [ ] **Frontend type-checks:**

Run: `cd ~/farder/client && npx tsc --noEmit`
Expected: no errors.

- [ ] **Mark the GUI flow UNVERIFIED until the Windows run.** Per CLAUDE.md, the gate (set PIN → recovery phrase → relaunch → enter PIN, plus the one-time migrate for the existing plaintext key) cannot be exercised in WSL. State plainly that it is UNVERIFIED and ask the user to run it on `C:\Users\Deez\farder` and confirm: first launch prompts to secure the existing key, relaunch asks for the PIN, a wrong PIN is rejected, and restore-from-phrase works.

- [ ] **Update the audit.** Flip Gap #2 in `docs/superpowers/audits/2026-06-05-privacy-security-wiring-audit.md` from **GAP (HIGH)** to **FIXED (pending GUI verification)** and reference this plan.

- [ ] **Finish the branch:** use superpowers:finishing-a-development-branch.
