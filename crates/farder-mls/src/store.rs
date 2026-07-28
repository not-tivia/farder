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
}

impl fmt::Display for RmpCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RmpCodecError::Encode(e) => write!(f, "rmp encode: {e}"),
            RmpCodecError::Decode(e) => write!(f, "rmp decode: {e}"),
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
        }
    }
}

impl std::error::Error for StoreResumeError {}

/// Sqlite-backed [`OpenMlsProvider`]: owns the rusqlite [`Connection`] (via
/// the [`SqliteStorageProvider`]), the [`RustCrypto`] crypto/rand backend,
/// and the store's 16-byte instance id. Every group/credential/envelope API
/// in this crate takes it interchangeably with the in-memory test provider.
pub struct FarderMlsStore {
    storage: SqliteStorageProvider<RmpCodec, Connection>,
    crypto: RustCrypto,
    instance_id: [u8; 16],
}

impl OpenMlsProvider for FarderMlsStore {
    type CryptoProvider = RustCrypto;
    type RandProvider = RustCrypto;
    type StorageProvider = SqliteStorageProvider<RmpCodec, Connection>;

    fn storage(&self) -> &Self::StorageProvider {
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

        let mut instance_id = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut instance_id);
        conn.execute(
            "CREATE TABLE farder_store_meta (instance_id BLOB NOT NULL)",
            [],
        )
        .context("create farder_store_meta table")?;
        conn.execute(
            "INSERT INTO farder_store_meta (instance_id) VALUES (?1)",
            [&instance_id[..]],
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

        Self::finish_open(conn, instance_id).map_err(StoreResumeError::Io)
    }

    /// Shared tail of `create`/`resume`: wrap the connection in the OpenMLS
    /// sqlite provider and apply its migrations (idempotent).
    fn finish_open(conn: Connection, instance_id: [u8; 16]) -> Result<Self> {
        let mut storage: SqliteStorageProvider<RmpCodec, Connection> =
            SqliteStorageProvider::new(conn);
        storage
            .run_migrations()
            .context("run openmls_sqlite_storage migrations")?;
        Ok(Self {
            storage,
            crypto: RustCrypto::default(),
            instance_id,
        })
    }
}

/// Read the store's instance id, requiring exactly one well-formed 16-byte
/// row. A missing table, zero rows, multiple rows, or a wrong-sized blob are
/// all [`StoreResumeError::MissingInstanceId`] — a store whose identity is
/// absent or ambiguous is treated as poisoned, never resumed.
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
