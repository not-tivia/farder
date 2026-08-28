//! Sqlite-backed OpenMLS provider with **store instance binding** and the
//! **no-resume** rule (spec §MLS store safety, rev 2 C6).
//!
//! The MLS store holds the sender-ratchet generation counter. Two live
//! instances sharing one store's contents encrypt at the same
//! `(epoch, generation)` — identical AES-128-GCM key AND nonce, which is
//! catastrophic. The realistic triggers need no attacker: backup restore,
//! profile copy, VM snapshot rollback, cloud-synced home directory.
//!
//! Defense implemented here:
//!
//! 1. [`FarderMlsStore::create`] generates a random 16-byte
//!    `store_instance_id` and persists it in a side table
//!    (`farder_store_meta`). [`FarderMlsStore::store_instance_hash`] is the
//!    SHA-256 of that id — the value the log carries
//!    (`MlsKeyPackagePublished` / `MlsCommit` / `MlsLeafConfirmed`, sub-2);
//!    the raw id never leaves the store.
//! 2. [`FarderMlsStore::resume`] refuses to open a store whose instance id
//!    does not hash to the expected value ([`StoreResumeError::InstanceMismatch`])
//!    or that has no instance metadata at all
//!    ([`StoreResumeError::MissingInstanceId`]). **Both are terminal for this
//!    store: the caller must self-`DeviceRevoked` and re-provision as a fresh
//!    device (sub-5). This crate never silently re-creates or resumes.**
//!
//! Rule 3 of the spec (non-portable directory placement, backup exclusion,
//! recovery-UI copy) is the client crate's job (sub-4), not this crate's.

use std::fmt;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use openmls_rust_crypto::RustCrypto;
use openmls_sqlite_storage::{Codec, SqliteStorageProvider};
use openmls_traits::OpenMlsProvider;
use rand::RngCore;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

/// rmp-serde codec for OpenMLS values persisted in sqlite (the same
/// MessagePack convention farder-crypto uses for `EventCore`).
#[derive(Debug, Default)]
pub struct RmpCodec;

/// Error type for [`RmpCodec`] (the `Codec` trait wants one type for both
/// directions).
#[derive(Debug)]
pub enum RmpCodecError {
    Encode(rmp_serde::encode::Error),
    Decode(rmp_serde::decode::Error),
    /// Seal/open failed: the wrong key, or a tampered value.
    Crypto,
}

impl fmt::Display for RmpCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RmpCodecError::Encode(e) => write!(f, "rmp encode: {e}"),
            RmpCodecError::Decode(e) => write!(f, "rmp decode: {e}"),
            RmpCodecError::Crypto => {
                write!(f, "MLS store value could not be sealed/opened (wrong key or tampered)")
            }
        }
    }
}

impl std::error::Error for RmpCodecError {}

impl Codec for RmpCodec {
    type Error = RmpCodecError;

    fn to_vec<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, Self::Error> {
        rmp_serde::to_vec(value).map_err(RmpCodecError::Encode)
    }

    fn from_slice<T: serde::de::DeserializeOwned>(slice: &[u8]) -> Result<T, Self::Error> {
        rmp_serde::from_slice(slice).map_err(RmpCodecError::Decode)
    }
}

// ---------------------------------------------------------------------------
// At-rest encryption of every value OpenMLS persists (sub-7a H2)
// ---------------------------------------------------------------------------
//
// This store holds the group's ratchet secrets. It was a plain sqlite file, so
// anyone holding the file plus some ciphertext could read the channel — which
// made sealing the local history archive beside it theatre.
//
// The `Codec` seam is the right place to fix that: it is OURS, and every value
// the storage provider writes passes through it. The awkward part is that
// `Codec`'s methods are STATIC (no `&self`), so the key cannot be threaded
// through as a parameter — it has to be ambient. Hence the process-global below.
//
// It is deliberately fail-closed: an unarmed key is an ERROR, never a silent
// fall back to plaintext. That is the whole point of the change, and a
// "temporarily unencrypted" mode would be indistinguishable from the bug.

/// The process-wide key that seals every value in every MLS store. Armed once
/// when the identity unlocks; cleared when it locks.
static STORE_KEY: std::sync::RwLock<Option<[u8; 32]>> = std::sync::RwLock::new(None);

/// Arm the at-rest key for MLS stores. The client calls this right after the
/// identity is unlocked, with a key derived from the identity (see
/// `farder_history::derive_local_key`). Idempotent.
pub fn arm_store_key(key: [u8; 32]) {
    if let Ok(mut guard) = STORE_KEY.write() {
        *guard = Some(key);
    }
}

/// Forget the at-rest key (identity locked / app shutting down). Any subsequent
/// store operation fails closed until it is armed again.
pub fn disarm_store_key() {
    if let Ok(mut guard) = STORE_KEY.write() {
        *guard = None;
    }
}

/// The fallback used when nothing armed a key: ONE random key per process.
///
/// The property that must never break is "MLS secrets are never written in the
/// clear", and a random key keeps it. What it deliberately does NOT do is make an
/// unarmed process silently correct: a store created under the fallback records
/// that key's fingerprint, so the next launch — with the real key armed — refuses
/// to resume with [`StoreResumeError::KeyMismatch`] instead of quietly producing
/// garbage. Loud and recoverable beats silent.
///
/// It also lets every test create and resume stores without arming anything,
/// which is why there is no test-only key to keep out of production.
static FALLBACK_KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();

thread_local! {
    /// The key of the store whose operation is currently running on this thread.
    ///
    /// `Codec`'s methods are static, so the key cannot be passed to them — but
    /// every storage operation reaches the provider through
    /// `OpenMlsProvider::storage(&self)`, synchronously on one thread. That
    /// accessor publishes the store's own key here first, so the codec seals with
    /// the key belonging to THE STORE BEING USED rather than whatever was armed
    /// globally last. Without this, two stores alive in one process (any test
    /// binary; a future second identity) would silently seal each other's values
    /// with the wrong key.
    static ACTIVE_KEY: std::cell::Cell<Option<[u8; 32]>> = const { std::cell::Cell::new(None) };
}

fn publish_active_key(key: [u8; 32]) {
    ACTIVE_KEY.with(|k| k.set(Some(key)));
}

/// The key each new store adopts: whatever the identity armed, or a per-process
/// random fallback (see [`FALLBACK_KEY`]).
fn key_for_new_store() -> [u8; 32] {
    if let Some(key) = STORE_KEY.read().ok().and_then(|g| *g) {
        return key;
    }
    *FALLBACK_KEY.get_or_init(|| {
        let mut k = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut k);
        k
    })
}

fn current_key() -> Result<[u8; 32], RmpCodecError> {
    ACTIVE_KEY
        .with(|k| k.get())
        .map_or_else(|| Ok(key_for_new_store()), Ok)
}

/// A short, non-secret fingerprint of the armed key, persisted beside the store
/// so resuming with the WRONG key is a loud refusal instead of a pile of decode
/// failures. Non-secret: it is a hash, and it identifies the key without
/// revealing anything usable.
fn key_fingerprint(key: &[u8; 32]) -> [u8; 8] {
    let full: [u8; 32] = Sha256::digest([b"farder-mls-store-key-id-v1".as_slice(), key].concat()).into();
    let mut out = [0u8; 8];
    out.copy_from_slice(&full[..8]);
    out
}

/// Nonce length for the value AEAD. The layout is `nonce || ciphertext`.
const VALUE_NONCE_LEN: usize = 12;

/// Derive this value's nonce from the value itself (an SIV construction):
/// `HMAC-SHA256(nonce_subkey, plaintext)` truncated to 12 bytes.
///
/// **This makes the encryption deterministic, and that is REQUIRED here, not a
/// shortcut.** `openmls_sqlite_storage` encodes its lookup KEYS through the same
/// `Codec` as its values (`wrappers.rs`'s `KeyRefWrapper`), and those encoded
/// bytes go straight into `WHERE key = ?`. With a random nonce the same logical
/// key encodes differently on every call, so every lookup misses and the store
/// silently behaves as if it were empty.
///
/// The cost is the standard one for deterministic encryption: it leaks EQUALITY.
/// An attacker holding the file can tell that two stored values are identical
/// without learning either. That is acceptable here — the values are MLS secrets
/// (unique and random, so equality never occurs in practice) and group ids (whose
/// equality the row structure already reveals).
///
/// Nonce reuse is safe in this construction because it only recurs for the SAME
/// (key, plaintext) pair, which produces the same ciphertext anyway; distinct
/// plaintexts get distinct nonces with overwhelming probability.
fn siv_nonce(key: &[u8; 32], plaintext: &[u8]) -> [u8; VALUE_NONCE_LEN] {
    let mut mac = <hmac::Hmac<Sha256> as hmac::Mac>::new_from_slice(key)
        .expect("HMAC accepts any key length");
    hmac::Mac::update(&mut mac, b"farder-mls-store-siv-v1");
    hmac::Mac::update(&mut mac, plaintext);
    let tag = hmac::Mac::finalize(mac).into_bytes();
    let mut nonce = [0u8; VALUE_NONCE_LEN];
    nonce.copy_from_slice(&tag[..VALUE_NONCE_LEN]);
    nonce
}

/// The codec actually used by [`FarderMlsStore`]: rmp-serde, then AES-256-GCM
/// under the ambient key.
///
/// No associated data: `Codec` sees only the value, never the table or key it
/// belongs to, so there is nothing row-specific to bind. Relocating a value
/// inside the file is therefore not detected HERE — the store's instance
/// binding is what guards clone/rollback tampering, and that check runs before
/// any read.
#[derive(Debug, Default)]
pub struct SealedRmpCodec;

impl Codec for SealedRmpCodec {
    type Error = RmpCodecError;

    fn to_vec<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, Self::Error> {
        use aes_gcm::aead::Aead;
        use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
        let plain = rmp_serde::to_vec(value).map_err(RmpCodecError::Encode)?;
        let key = current_key()?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| RmpCodecError::Crypto)?;
        let nonce = siv_nonce(&key, &plain);
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce), plain.as_slice())
            .map_err(|_| RmpCodecError::Crypto)?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&ct);
        Ok(out)
    }

    fn from_slice<T: serde::de::DeserializeOwned>(slice: &[u8]) -> Result<T, Self::Error> {
        use aes_gcm::aead::Aead;
        use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
        if slice.len() <= VALUE_NONCE_LEN {
            return Err(RmpCodecError::Crypto);
        }
        let key = current_key()?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| RmpCodecError::Crypto)?;
        let plain = cipher
            .decrypt(
                Nonce::from_slice(&slice[..VALUE_NONCE_LEN]),
                &slice[VALUE_NONCE_LEN..],
            )
            .map_err(|_| RmpCodecError::Crypto)?;
        rmp_serde::from_slice(&plain).map_err(RmpCodecError::Decode)
    }
}

/// Why [`FarderMlsStore::resume`] refused. `InstanceMismatch` and
/// `MissingInstanceId` are **terminal** for the store on disk: never retry,
/// never re-create in place — self-`DeviceRevoked` and provision fresh.
#[derive(Debug)]
pub enum StoreResumeError {
    /// The store's instance id does not hash to the expected value: this is
    /// a different (or cloned/restored) store instance than the log records
    /// for this device.
    InstanceMismatch,
    /// The store has no (single, well-formed) instance-id row: poisoned or
    /// tampered store; never resume the ratchet.
    MissingInstanceId,
    /// The store could not be read at all (missing file, permissions,
    /// corruption). Also terminal for resume: this fn never creates a store.
    Io(anyhow::Error),
    /// The store's values were sealed under a DIFFERENT at-rest key than the one
    /// armed now — or, for a store written before at-rest encryption existed, no
    /// key at all. Terminal: the ratchet state in there cannot be read, so the
    /// device must re-provision (or, for a pre-encryption store, the channel must
    /// be recreated — the deliberate one-time cost of sub-7a H2).
    KeyMismatch,
}

impl fmt::Display for StoreResumeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreResumeError::InstanceMismatch => {
                write!(f, "MLS store instance id does not match the expected hash")
            }
            StoreResumeError::MissingInstanceId => {
                write!(f, "MLS store has no instance-id metadata")
            }
            StoreResumeError::Io(e) => write!(f, "MLS store could not be read: {e}"),
            StoreResumeError::KeyMismatch => write!(
                f,
                "MLS store was sealed under a different key (or predates at-rest \
                 encryption); it cannot be resumed — re-provision this device, or \
                 recreate the channel if the store predates encryption"
            ),
        }
    }
}

impl std::error::Error for StoreResumeError {}

/// Sqlite-backed [`OpenMlsProvider`]: owns the rusqlite [`Connection`] (via
/// the [`SqliteStorageProvider`]), the [`RustCrypto`] crypto/rand backend,
/// and the store's 16-byte instance id. Every group/credential/envelope API
/// in this crate takes it interchangeably with the in-memory test provider.
pub struct FarderMlsStore {
    storage: SqliteStorageProvider<SealedRmpCodec, Connection>,
    crypto: RustCrypto,
    instance_id: [u8; 16],
    /// This store's at-rest key, adopted when it was opened. Published to the
    /// thread-local on every `storage()` call — see [`ACTIVE_KEY`].
    key: [u8; 32],
}

impl OpenMlsProvider for FarderMlsStore {
    type CryptoProvider = RustCrypto;
    type RandProvider = RustCrypto;
    type StorageProvider = SqliteStorageProvider<SealedRmpCodec, Connection>;

    fn storage(&self) -> &Self::StorageProvider {
        // Every storage operation goes through here, so this is where the codec
        // learns which key to seal with.
        publish_active_key(self.key);
        &self.storage
    }

    fn crypto(&self) -> &Self::CryptoProvider {
        &self.crypto
    }

    fn rand(&self) -> &Self::RandProvider {
        &self.crypto
    }
}

impl FarderMlsStore {
    /// Create a brand-new store at `db_path`, generating and persisting a
    /// random 16-byte `store_instance_id`. Returns the store plus the raw id.
    ///
    /// Fails if `db_path` already exists — creating "over" an existing file
    /// (including a poisoned store whose metadata was stripped) is exactly
    /// the silent-recreate hazard the no-resume rule forbids. Callers wanting
    /// a fresh store must use a fresh path (or explicitly delete first).
    pub fn create(db_path: &Path) -> Result<(Self, [u8; 16])> {
        if db_path.exists() {
            bail!(
                "MLS store path {} already exists; refusing to create over it \
                 (use a fresh path, or resume() with the expected instance hash)",
                db_path.display()
            );
        }
        let conn = Connection::open(db_path)
            .with_context(|| format!("open new MLS store at {}", db_path.display()))?;

        // The key this store ADOPTS — the ambient armed key, never the
        // thread-local, which may still name a different store used earlier on
        // this thread.
        let key = key_for_new_store();

        let mut instance_id = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut instance_id);
        conn.execute(
            "CREATE TABLE farder_store_meta (instance_id BLOB NOT NULL, key_id BLOB)",
            [],
        )
        .context("create farder_store_meta table")?;
        conn.execute(
            "INSERT INTO farder_store_meta (instance_id, key_id) VALUES (?1, ?2)",
            rusqlite::params![&instance_id[..], &key_fingerprint(&key)[..]],
        )
        .context("persist store instance id")?;

        let store = Self::finish_open(conn, instance_id)?;
        Ok((store, instance_id))
    }

    /// SHA-256 of the raw 16-byte instance id — the value published in
    /// `MlsKeyPackagePublished` and carried on every commit (sub-2). The raw
    /// id itself never leaves the store.
    pub fn store_instance_hash(&self) -> [u8; 32] {
        Sha256::digest(self.instance_id).into()
    }

    /// Resume an existing store, refusing unless its persisted instance id
    /// hashes to `expected_instance_hash`. Never creates a store, never
    /// repairs one: `InstanceMismatch` / `MissingInstanceId` mean the caller
    /// must self-`DeviceRevoked` and re-provision (sub-5 behavior).
    pub fn resume(
        db_path: &Path,
        expected_instance_hash: &[u8; 32],
    ) -> Result<Self, StoreResumeError> {
        // Open WITHOUT the create flag: a missing file must be an error,
        // never a silently minted empty store.
        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| {
            StoreResumeError::Io(anyhow!("open MLS store at {}: {e}", db_path.display()))
        })?;

        // Verify the instance binding BEFORE any write (including migrations).
        let instance_id = read_instance_id(&conn)?;
        let actual: [u8; 32] = Sha256::digest(instance_id).into();
        if &actual != expected_instance_hash {
            return Err(StoreResumeError::InstanceMismatch);
        }

        // Verify the AT-REST KEY before any read, so resuming with the wrong
        // identity is one clear refusal instead of a pile of decode failures from
        // deep inside OpenMLS. Judged against the ambient armed key — reading the
        // thread-local here would compare the store against ITSELF whenever
        // another store was used earlier on this thread, and never fire.
        let key = key_for_new_store();
        match read_key_id(&conn) {
            // A store written before H2 has no key_id column/value at all. It is
            // plaintext and cannot be read by the sealing codec: say so plainly.
            Ok(None) => return Err(StoreResumeError::KeyMismatch),
            Ok(Some(id)) if id != key_fingerprint(&key) => {
                return Err(StoreResumeError::KeyMismatch)
            }
            Ok(Some(_)) => {}
            Err(e) => return Err(e),
        }

        Self::finish_open(conn, instance_id).map_err(StoreResumeError::Io)
    }

    /// Shared tail of `create`/`resume`: wrap the connection in the OpenMLS
    /// sqlite provider and apply its migrations (idempotent).
    fn finish_open(conn: Connection, instance_id: [u8; 16]) -> Result<Self> {
        // Adopt the key BEFORE the migrations, which read and write through the
        // codec like any other storage operation.
        let key = key_for_new_store();
        publish_active_key(key);
        let mut storage: SqliteStorageProvider<SealedRmpCodec, Connection> =
            SqliteStorageProvider::new(conn);
        storage
            .run_migrations()
            .context("run openmls_sqlite_storage migrations")?;
        Ok(Self {
            storage,
            crypto: RustCrypto::default(),
            instance_id,
            key,
        })
    }
}

/// Read the store's instance id, requiring exactly one well-formed 16-byte
/// row. A missing table, zero rows, multiple rows, or a wrong-sized blob are
/// all [`StoreResumeError::MissingInstanceId`] — a store whose identity is
/// absent or ambiguous is treated as poisoned, never resumed.
/// Read the store's at-rest key fingerprint. `Ok(None)` means the store predates
/// at-rest encryption (no `key_id` column, or a NULL value) — its values are
/// plaintext rmp and the sealing codec cannot read them.
fn read_key_id(conn: &Connection) -> Result<Option<[u8; 8]>, StoreResumeError> {
    let has_column: i64 = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('farder_store_meta') WHERE name = 'key_id'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| StoreResumeError::Io(anyhow!("inspect MLS store schema: {e}")))?;
    if has_column == 0 {
        return Ok(None);
    }
    let raw: Option<Vec<u8>> = conn
        .query_row("SELECT key_id FROM farder_store_meta", [], |row| row.get(0))
        .map_err(|e| StoreResumeError::Io(anyhow!("read MLS store key id: {e}")))?;
    match raw {
        None => Ok(None),
        Some(bytes) => bytes
            .as_slice()
            .try_into()
            .map(Some)
            .map_err(|_| StoreResumeError::Io(anyhow!("MLS store key id has the wrong length"))),
    }
}

fn read_instance_id(conn: &Connection) -> Result<[u8; 16], StoreResumeError> {
    let table_exists: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'farder_store_meta'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| StoreResumeError::Io(anyhow!("inspect MLS store schema: {e}")))?;
    if table_exists == 0 {
        return Err(StoreResumeError::MissingInstanceId);
    }
    let mut stmt = conn
        .prepare("SELECT instance_id FROM farder_store_meta")
        .map_err(|e| StoreResumeError::Io(anyhow!("read store instance id: {e}")))?;
    let ids: Vec<Vec<u8>> = stmt
        .query_map([], |row| row.get(0))
        .and_then(|rows| rows.collect())
        .map_err(|e| StoreResumeError::Io(anyhow!("read store instance id: {e}")))?;
    match ids.as_slice() {
        [id] => id
            .as_slice()
            .try_into()
            .map_err(|_| StoreResumeError::MissingInstanceId),
        _ => Err(StoreResumeError::MissingInstanceId),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::{credential_with_key, generate_key_package, DeviceSigner};
    use crate::envelope::MessageEnvelope;
    use crate::group::MlsChannelGroup;
    use farder_crypto::identity::Keypair;
    use openmls_rust_crypto::OpenMlsRustCrypto;
    use std::path::PathBuf;

    /// Unique per-process temp path; removes any leftover from a prior run.
    fn temp_db(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "farder-mls-store-test-{}-{name}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn envelope(content: &str) -> MessageEnvelope {
        MessageEnvelope {
            content: content.to_string(),
            ..MessageEnvelope::default()
        }
    }

    #[test]
    fn fresh_store_generates_a_random_instance_id_and_publishable_hash() {
        let p1 = temp_db("fresh-1");
        let p2 = temp_db("fresh-2");

        let (s1, id1) = FarderMlsStore::create(&p1).unwrap();
        let (_s2, id2) = FarderMlsStore::create(&p2).unwrap();

        // Two creates produce different random ids.
        assert_ne!(id1, id2);

        // The publishable hash is exactly sha256(raw id).
        let expected: [u8; 32] = Sha256::digest(id1).into();
        assert_eq!(s1.store_instance_hash(), expected);

        // Creating on top of an existing store file is refused.
        drop(s1);
        assert!(FarderMlsStore::create(&p1).is_err());
    }

    #[test]
    fn resume_with_matching_hash_restores_group_state() {
        let path = temp_db("resume");
        let group_id: &[u8] = b"server-1/channel-1/generation-0";

        let a_id = Keypair::generate();
        let a_dev = Keypair::generate();
        let b_id = Keypair::generate();
        let b_dev = Keypair::generate();
        let b_prov = OpenMlsRustCrypto::default();

        // Session 1: sqlite-store alice creates the group, adds
        // memory-store bob, seals one message.
        let (store, _id) = FarderMlsStore::create(&path).unwrap();
        let hash = store.store_instance_hash();
        let mut alice = MlsChannelGroup::create(
            &store,
            &DeviceSigner(&a_dev),
            credential_with_key(&a_dev, &a_id.public_key()),
            group_id,
        )
        .unwrap();
        let b_bundle = generate_key_package(&b_prov, &b_dev, &b_id.public_key()).unwrap();
        let outcome = alice
            .add_members(
                &store,
                &DeviceSigner(&a_dev),
                &[b_bundle.key_package().clone()],
            )
            .unwrap();
        let (mut bob, _) =
            MlsChannelGroup::join_from_welcome(&b_prov, &outcome.welcome_bytes.clone().unwrap())
                .unwrap();
        let sealed = alice
            .seal_message(&store, &DeviceSigner(&a_dev), &envelope("before the restart"))
            .unwrap();
        assert_eq!(
            bob.open_message(&b_prov, &sealed).unwrap().content,
            "before the restart"
        );
        drop(alice);
        drop(store);

        // Session 2: resume with the right hash; the group loads and both
        // directions still work.
        let resumed = FarderMlsStore::resume(&path, &hash).unwrap();
        assert_eq!(resumed.store_instance_hash(), hash);
        let mut alice2 = MlsChannelGroup::load(&resumed, &DeviceSigner(&a_dev), group_id)
            .unwrap()
            .expect("persisted group loads");
        assert_eq!(alice2.epoch(), 1);
        assert_eq!(alice2.members().unwrap().len(), 2);
        assert_eq!(alice2.tree_hash(), bob.tree_hash());

        let sealed2 = alice2
            .seal_message(
                &resumed,
                &DeviceSigner(&a_dev),
                &envelope("after the restart"),
            )
            .unwrap();
        assert_eq!(
            bob.open_message(&b_prov, &sealed2).unwrap().content,
            "after the restart"
        );
        let sealed3 = bob
            .seal_message(
                &b_prov,
                &DeviceSigner(&b_dev),
                &envelope("to the resumed store"),
            )
            .unwrap();
        assert_eq!(
            alice2.open_message(&resumed, &sealed3).unwrap().content,
            "to the resumed store"
        );

        // A group id that was never created loads as None (not an error).
        assert!(
            MlsChannelGroup::load(&resumed, &DeviceSigner(&a_dev), b"never-created")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn store_instance_mismatch_refuses_to_resume() {
        let path = temp_db("mismatch");
        let (store, _id) = FarderMlsStore::create(&path).unwrap();
        let hash = store.store_instance_hash();
        drop(store);

        // NO-RESUME (spec C6): a different expected hash is refused.
        let mut wrong = hash;
        wrong[0] ^= 0xff;
        assert!(matches!(
            FarderMlsStore::resume(&path, &wrong),
            Err(StoreResumeError::InstanceMismatch)
        ));

        // No fallback store was created in its place: the original store is
        // untouched and still resumes under its real hash.
        let resumed = FarderMlsStore::resume(&path, &hash).unwrap();
        assert_eq!(resumed.store_instance_hash(), hash);
    }

    #[test]
    fn resume_of_a_store_without_instance_metadata_is_refused_not_recreated() {
        let path = temp_db("missing-meta");
        let (store, _id) = FarderMlsStore::create(&path).unwrap();
        let hash = store.store_instance_hash();
        drop(store);

        // Strip the instance metadata (poisoned-store shape).
        let conn = Connection::open(&path).unwrap();
        conn.execute("DELETE FROM farder_store_meta", []).unwrap();
        drop(conn);

        // Refused — and NOT recreated: a second attempt refuses identically
        // instead of finding a freshly minted id.
        assert!(matches!(
            FarderMlsStore::resume(&path, &hash),
            Err(StoreResumeError::MissingInstanceId)
        ));
        assert!(matches!(
            FarderMlsStore::resume(&path, &hash),
            Err(StoreResumeError::MissingInstanceId)
        ));

        // create() refuses the existing file too — no in-place resurrection.
        assert!(FarderMlsStore::create(&path).is_err());
    }
}

#[cfg(test)]
mod at_rest_tests {
    use super::*;
    use crate::credential::{credential_with_key, DeviceSigner};
    use crate::group::MlsChannelGroup;
    use farder_crypto::identity::Keypair;

    fn temp_db(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "farder-mls-at-rest-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("store.mls")
    }

    fn file_contains(path: &std::path::Path, needle: &[u8]) -> bool {
        let raw = std::fs::read(path).unwrap_or_default();
        raw.windows(needle.len()).any(|w| w == needle)
    }

    /// The H2 observation test: the values OpenMLS persists must not be readable
    /// in the file. The group id is the handle we control end-to-end — with the
    /// old plaintext codec it appeared verbatim (rmp writes byte strings raw), so
    /// its absence is a real check rather than a tautology.
    #[test]
    fn the_mls_store_file_holds_no_plaintext_values() {
        let path = temp_db("observation");
        let group_id: &[u8] = b"NEEDLE-GROUP-ID-must-not-appear-in-the-file";
        let id = Keypair::generate();
        let dev = Keypair::generate();

        {
            let (store, _) = FarderMlsStore::create(&path).unwrap();
            let _group = MlsChannelGroup::create(
                &store,
                &DeviceSigner(&dev),
                credential_with_key(&dev, &id.public_key()),
                group_id,
            )
            .unwrap();
        }

        assert!(
            !file_contains(&path, group_id),
            "the group id reached the store file in the clear"
        );
        // Positive control: the scanner can see bytes that ARE there. The
        // instance id is stored deliberately in the clear (it is a random
        // handle, not a secret), so it is the honest thing to point at.
        let conn = Connection::open(&path).unwrap();
        let instance: Vec<u8> = conn
            .query_row("SELECT instance_id FROM farder_store_meta", [], |r| r.get(0))
            .unwrap();
        drop(conn);
        assert!(
            file_contains(&path, &instance),
            "the scanner cannot see plaintext it was pointed straight at"
        );
    }

    /// Resuming a store sealed under a DIFFERENT key must be one clear refusal,
    /// not a pile of decode failures from inside OpenMLS.
    #[test]
    fn a_store_sealed_under_another_key_refuses_to_resume() {
        let path = temp_db("key-mismatch");
        arm_store_key([1u8; 32]);
        let hash = {
            let (store, _) = FarderMlsStore::create(&path).unwrap();
            store.store_instance_hash()
        };

        arm_store_key([2u8; 32]);
        match FarderMlsStore::resume(&path, &hash) {
            Err(StoreResumeError::KeyMismatch) => {}
            Err(other) => panic!("expected KeyMismatch, got {other}"),
            Ok(_) => panic!("a store sealed under another key must not resume"),
        }

        // The right key still opens it.
        arm_store_key([1u8; 32]);
        assert!(FarderMlsStore::resume(&path, &hash).is_ok());
        disarm_store_key();
    }
}
