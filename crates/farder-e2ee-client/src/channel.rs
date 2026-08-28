//! E2EE channel lifecycle: create the channel, publish a KeyPackage, and make
//! the creator's bootstrap commit.
//!
//! This is Task 2 of the 4a vertical. It is deliberately **not** the join
//! path (Task 3), steward add (Task 4), send/receive (Task 5), resync
//! (Task 6), or any server emit site (Task 7).

use std::fmt;
use std::path::{Path, PathBuf};

use farder_crypto::event_log::{ChannelClass, EventPayload, E2EE_CHANNEL_ID_FLOOR};
use farder_mls::credential::{credential_with_key, generate_key_package, DeviceSigner};
use farder_mls::group::{DeclaredMember, MlsChannelGroup};
use farder_mls::store::{FarderMlsStore, StoreResumeError};
use tls_codec::Serialize as TlsSerialize;

use crate::chain::{build_next_event, event_now_secs, Actor, ChainState};
use crate::channel_key::ChannelKey;
use crate::transport::{E2eeTransport, TransportError};

/// Everything [`create_e2ee_channel`] needs to describe the channel being
/// created. `name` / `kind` / `parent` are logged opaquely (the fold reads
/// only `class` and `creator`); `class` is always `E2ee` here.
pub struct ChannelSpec {
    pub key: ChannelKey,
    pub name: String,
    pub kind: String,
    pub parent: Option<u64>,
}

/// Log-position lifetime granted to a published KeyPackage
/// (`MlsKeyPackagePublished.expires_at_log_pos`).
///
/// # Why the client cannot compute the server's `log_pos`
///
/// The fold requires `expires_at_log_pos > log_pos` (`event_log_state.rs:1021`),
/// where `log_pos` is the number of accepted events folded so far — a
/// **server-wide** counter incremented once per accepted event, across every
/// member and device (`event_log_state.rs:452-455, 746`). The client only
/// tracks its **own** device chain (`ChainState.next_seq`), which is a strict
/// *lower bound* on `log_pos` but says nothing about every other device's
/// events. No `ServerResponse` variant exposes the server's current log
/// position: `EventAccepted` carries only `event_hash` + `timestamp`, and
/// neither `ServerInfo`/`ServerInfoV2` nor the fetch surfaces return it.
///
/// So the client cannot know `log_pos` locally. The defensible strategy is a
/// large-but-**finite** window past this device's own contribution: the
/// publish is rejected only if the server has already accepted more than
/// `KEY_PACKAGE_LIFETIME_LOG_POSITIONS` events *beyond this device's own*,
/// which is ~1.1 trillion — unreachable for any realistic server while keeping
/// the value a concrete `u64` so the fold's lifetime + live-cap rules still
/// mean something. A device's live KeyPackages stay naturally bounded (~1 in
/// steady state) because each rekey consumes the previous one, so the
/// 10-live-per-device cap is not a practical concern.
pub const KEY_PACKAGE_LIFETIME_LOG_POSITIONS: u64 = 1 << 40;

/// Canonical MLS group id for a channel at a generation, stable across
/// create / resume / reset so `MlsChannelGroup::load` finds the group that was
/// created under the same bytes. The store is already per-channel
/// (`servers/{log_server_id}/mls/{channel_id}.sqlite`), but the generation is
/// part of the group's identity because a reset mints a brand-new group.
pub fn channel_group_id(log_server_id: &str, channel_id: u64, generation: u64) -> String {
    format!("{log_server_id}/{channel_id}/generation-{generation}")
}

/// Persist the store instance hash beside the MLS store (Task 1's
/// `instance_hash_path` layout), as the raw 32 bytes. It is the value every
/// later `FarderMlsStore::resume` needs; losing it makes resume impossible
/// (terminal).
pub fn persist_store_instance_hash(
    data_dir: &Path,
    key: &ChannelKey,
    hash: &[u8; 32],
) -> Result<(), E2eeError> {
    let path = key.instance_hash_path(data_dir).map_err(E2eeError::chain)?;
    let parent = path
        .parent()
        .ok_or_else(|| E2eeError::chain("instance hash path has no parent"))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| E2eeError::Mls(anyhow::anyhow!("create mls dir: {e}")))?;
    std::fs::write(&path, hash)
        .map_err(|e| E2eeError::Mls(anyhow::anyhow!("write instance hash: {e}")))?;
    Ok(())
}

/// Read the persisted store instance hash back (the resume counterpart of
/// [`persist_store_instance_hash`]). Errors if the file is absent or not
/// exactly 32 bytes.
pub fn read_store_instance_hash(data_dir: &Path, key: &ChannelKey) -> Result<[u8; 32], E2eeError> {
    let path = key.instance_hash_path(data_dir).map_err(E2eeError::chain)?;
    let bytes = std::fs::read(&path)
        .map_err(|e| E2eeError::Mls(anyhow::anyhow!("read instance hash: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| E2eeError::chain("instance hash file is not 32 bytes"))
}

/// An error from the E2EE vertical.
#[derive(Debug)]
pub enum E2eeError {
    /// The transport failed or the server rejected a request (any rejection
    /// except the bare `"stale-epoch"` own-commit rejection, which surfaces as
    /// [`E2eeError::StaleEpochDiverged`]).
    Transport(TransportError),
    /// The server rejected an own-commit as `stale-epoch`. This is the
    /// divergence contract: OpenMLS merged the commit **locally and
    /// immediately** (`MlsChannelGroup::self_update` / `add_members` /
    /// `remove_members` stage-and-merge before any submit), so the local group
    /// is now one epoch AHEAD of the server's view. The caller must NOT keep
    /// using that group — it must rebuild/resync local group state from the
    /// log (Task 6) before doing anything else. This is never silently
    /// swallowed and never reported as success.
    StaleEpochDiverged { local_epoch: u64 },
    /// The server refused a rekey commit under the commit-rate rule
    /// (`event_log_state.rs:1187-1203`): the author has already committed in
    /// this channel and the epoch gap has not yet elapsed, so the rekey is not
    /// permitted YET. This is a *policy* refusal at the cited epoch, not the
    /// bare `"stale-epoch"` epoch-CAS bounce — but note `self_update` still
    /// merged locally, so the local group is one epoch ahead of the server and
    /// must not be reused until resynced (same divergence caveat as
    /// [`E2eeError::StaleEpochDiverged`]). A caller must NOT retry the rekey in
    /// a loop: when to rekey later is the cadence policy's job
    /// ([`crate::rekey`]), keyed off the `"freshness ceiling reached"` send
    /// rejection or a sufficient epoch gap.
    RekeyRateLimited { reason: String },
    /// A `channel_id` below `E2EE_CHANNEL_ID_FLOOR` — the id must stay clear
    /// of the legacy DB `channels` AUTOINCREMENT space.
    ChannelIdBelowFloor { channel_id: u64 },
    /// The event chain state is inconsistent for the requested operation.
    Chain(String),
    /// MLS / store / serialization / filesystem failure.
    Mls(anyhow::Error),
    /// A sealed send was attempted before this device's own leaf was confirmed.
    /// This is a **local** refusal (fact A2.6): the fold rejects with
    /// `"sealed content author does not hold a confirmed leaf"` until
    /// `MlsLeafConfirmed` lands, so the client refuses up front rather than
    /// round-tripping a doomed event. Task 5's `send_sealed` keys on this.
    NotConfirmed,
    /// A sealed message exceeded a size cap before submission. Covers the
    /// client-side pre-seal caps (`MAX_CONTENT_CHARS` / `MAX_PRESEAL_BYTES`,
    /// enforced *before* sealing) and the server's
    /// `MAX_E2EE_CIPHERTEXT_BYTES` ciphertext cap (enforced after sealing,
    /// before submit). `reason` describes which cap and by how much.
    SealedOverCap { reason: String },
    /// The on-disk MLS store could not be resumed. Terminal for that store:
    /// `InstanceMismatch` / `MissingInstanceId` mean the store is cloned,
    /// restored or poisoned, and deleting + re-creating it in place would
    /// silently destroy group state — the caller must self-`DeviceRevoked` and
    /// re-provision (sub-5's job). This is never papered over here.
    StoreResumeTerminal(StoreResumeError),
    /// The resync loop gave up after exhausting its bounds: the send kept
    /// losing the epoch race and the local group could not be made to converge
    /// with the server's (see [`crate::resync`]). `attempts` is how many resync
    /// attempts were made; `last_epoch` is the group's epoch when the loop
    /// stopped. Equivocation-class — surfaced instead of looping silently.
    ResyncEquivocation { attempts: usize, last_epoch: u64 },
    /// F4 (terminal): while resyncing, an incoming commit passed Gate 1 (its
    /// declared metadata matched) but failed Gate 2 (leaf binding) — an
    /// impostor leaf. Because Gate 1 already merged the commit and farder-mls
    /// offers no rollback, the local group is POISONED. The resync aborts and
    /// surfaces this rather than continuing or retrying through it.
    ResyncPoisoned { member: DeclaredMember, reason: String },
}

impl E2eeError {
    pub(crate) fn chain(msg: impl Into<String>) -> Self {
        Self::Chain(msg.into())
    }

    /// True iff the server rejected an own-commit as `stale-epoch` — i.e. the
    /// local group is one epoch ahead of the server and must be resynced
    /// before further use. This is the machine-readable signal Task 6 keys on.
    pub fn is_stale_epoch_diverged(&self) -> bool {
        matches!(self, Self::StaleEpochDiverged { .. })
    }

    /// True iff the server refused a rekey under the commit-rate rule — "you
    /// may not rekey yet". See [`E2eeError::RekeyRateLimited`].
    pub fn is_rekey_rate_limited(&self) -> bool {
        matches!(self, Self::RekeyRateLimited { .. })
    }

    /// True iff a sealed send was rejected because the freshness ceiling was
    /// reached — the fold's guarantee that a rekey is now permitted, so the
    /// caller should rekey and retry. Keys on the `"freshness ceiling reached"`
    /// reason inside [`E2eeError::Transport`].
    pub fn is_freshness_ceiling_reached(&self) -> bool {
        matches!(self, Self::Transport(e) if e.is_freshness_ceiling_reached())
    }

    /// True iff a sealed send was rejected because the channel has pending
    /// removals (drift): the fold's guarantee that the channel is sealed until
    /// a remaining confirmed member authors a drift-discharging remove-commit.
    /// Keys on the `"channel is sealed until a rekey discharges its pending
    /// removals"` reason inside [`E2eeError::Transport`]. This is the reactive
    /// drift signal the caller maps to [`crate::drift::discharge_drift`].
    pub fn is_sealed_pending_removals(&self) -> bool {
        matches!(self, Self::Transport(e) if e.is_sealed_pending_removals())
    }
}

impl fmt::Display for E2eeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "{e}"),
            Self::StaleEpochDiverged { local_epoch } => write!(
                f,
                "server rejected our commit as stale-epoch; the local group is now at epoch \
                 {local_epoch}, one ahead of the server — resync from the log before continuing"
            ),
            Self::RekeyRateLimited { reason } => {
                write!(f, "rekey refused by the commit-rate rule (not permitted yet): {reason}")
            }
            Self::ChannelIdBelowFloor { channel_id } => write!(
                f,
                "channel id {channel_id} is below the E2EE floor {E2EE_CHANNEL_ID_FLOOR}"
            ),
            Self::Chain(msg) => write!(f, "event chain: {msg}"),
            Self::Mls(e) => write!(f, "mls: {e}"),
            Self::NotConfirmed => {
                write!(f, "cannot send sealed content before this device's leaf is confirmed")
            }
            Self::SealedOverCap { reason } => write!(f, "sealed message over cap: {reason}"),
            Self::StoreResumeTerminal(e) => write!(f, "MLS store resume is terminal: {e}"),
            Self::ResyncEquivocation { attempts, last_epoch } => write!(
                f,
                "resync gave up after {attempts} attempts (group still at epoch {last_epoch}): \
                 the send kept losing the epoch race"
            ),
            Self::ResyncPoisoned { member, reason } => write!(
                f,
                "resync hit an impostor leaf for {} / {} — the group is poisoned and cannot \
                 be rolled back: {reason}",
                member.identity, member.device
            ),
        }
    }
}

impl std::error::Error for E2eeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(e) => Some(e),
            Self::Mls(e) => e.source(),
            Self::StoreResumeTerminal(e) => Some(e),
            _ => None,
        }
    }
}

impl From<TransportError> for E2eeError {
    fn from(e: TransportError) -> Self {
        Self::Transport(e)
    }
}

/// The result of [`create_e2ee_channel`].
pub struct CreateChannelOutcome {
    /// Server-assigned hash of the accepted `ChannelCreated` event.
    pub event_hash: String,
    pub channel_id: u64,
    /// The freshly created MLS group at epoch 0 (creator's leaf, unconfirmed
    /// until the bootstrap commit — fact A2.5).
    pub group: MlsChannelGroup,
    /// The sqlite store backing the group. Drop it when done; a later
    /// `resume` re-opens it from disk with the persisted instance hash.
    pub store: FarderMlsStore,
    /// The instance hash, also persisted to disk beside the store.
    pub store_instance_hash: [u8; 32],
}

/// The result of [`publish_key_package`].
#[derive(Debug)]
pub struct KeyPackageOutcome {
    pub event_hash: String,
    /// The raw RFC 9420 (TLS-encoded) KeyPackage bytes, exactly as published.
    pub key_package: Vec<u8>,
    pub expires_at_log_pos: u64,
}

/// The result of [`bootstrap_group`].
///
/// **Acceptance is not advancement** (fact A2.1): a commit that lost the epoch
/// race is *accepted* as `StaleCommitNoOp` — chain head + `log_pos` advance,
/// zero MLS state change. The server's ingest pre-check makes that no-op path
/// unreachable for a live submit (a stale commit is rejected up front with the
/// bare `"stale-epoch"`), but this struct deliberately does **not** claim the
/// server's MLS state advanced: `local_epoch` is the *local* group's post-merge
/// epoch. Task 6 (resync) adds the follow-up read of `mls_current_epoch` that
/// turns "accepted" into "took effect".
#[derive(Debug)]
pub struct CommitSubmitted {
    pub event_hash: String,
    /// The epoch the LOCAL group is in after merging our own commit. Equal to
    /// the server's new epoch iff the server folded the commit (not a no-op).
    pub local_epoch: u64,
    pub post_epoch_authenticator: [u8; 32],
    pub post_tree_hash: [u8; 32],
}

/// Remove the store + instance-hash files left behind by a partially-created
/// channel, so a failed [`create_e2ee_channel`] can be retried (the no-resume
/// rule makes `FarderMlsStore::create` refuse an existing path). Armed until
/// the create succeeds.
struct ChannelCreateCleanup {
    store_path: PathBuf,
    hash_path: PathBuf,
    armed: bool,
}

impl Drop for ChannelCreateCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.store_path);
            let _ = std::fs::remove_file(&self.hash_path);
        }
    }
}

/// Create an E2EE channel: submit the owner-only `ChannelCreated { class: E2ee }`
/// event, then create the on-disk MLS store and the one-member MLS group, and
/// persist the store instance hash.
///
/// The server materializes the `channels` row inside the accept transaction
/// (fact A2.10) — this crate only submits the log event. The channel id is
/// client-chosen but must be at/above [`E2EE_CHANNEL_ID_FLOOR`] (fact A2.9).
pub async fn create_e2ee_channel<T: E2eeTransport + Sync>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    spec: &ChannelSpec,
    data_dir: &Path,
) -> Result<CreateChannelOutcome, E2eeError> {
    let key = &spec.key;
    if key.channel_id < E2EE_CHANNEL_ID_FLOOR {
        return Err(E2eeError::ChannelIdBelowFloor {
            channel_id: key.channel_id,
        });
    }

    // 1. Submit ChannelCreated first: only on acceptance do we mint any local
    //    MLS state.
    let created = build_next_event(
        actor.device,
        actor.identity,
        &key.log_server_id,
        chain,
        event_now_secs(),
        EventPayload::ChannelCreated {
            channel_id: key.channel_id,
            name: spec.name.clone(),
            kind: spec.kind.clone(),
            class: ChannelClass::E2ee,
            parent: spec.parent,
        },
    );
    let accepted = transport.submit_event(&created).await?;
    chain.advance(&created);

    // 2. Create the store + group, persisting the instance hash.
    let store_path = key.mls_store_path(data_dir).map_err(E2eeError::chain)?;
    let hash_path = key.instance_hash_path(data_dir).map_err(E2eeError::chain)?;
    let mut cleanup = ChannelCreateCleanup {
        store_path: store_path.clone(),
        hash_path,
        armed: true,
    };
    if let Some(parent) = store_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| E2eeError::Mls(anyhow::anyhow!("create mls dir: {e}")))?;
    }
    let (store, _instance_id) = FarderMlsStore::create(&store_path)
        .map_err(|e| E2eeError::Mls(e.context("create MLS store")))?;
    let store_instance_hash = store.store_instance_hash();
    let group = MlsChannelGroup::create(
        &store,
        &DeviceSigner(actor.device),
        credential_with_key(actor.device, &actor.identity.public_key()),
        channel_group_id(&key.log_server_id, key.channel_id, 0).as_bytes(),
    )
    .map_err(|e| E2eeError::Mls(e.context("create MLS group")))?;
    persist_store_instance_hash(data_dir, key, &store_instance_hash)?;

    cleanup.armed = false;
    Ok(CreateChannelOutcome {
        event_hash: accepted.event_hash,
        channel_id: key.channel_id,
        group,
        store,
        store_instance_hash,
    })
}

/// Publish this device's KeyPackage to the log: generate it, serialize it to
/// its RFC 9420 bytes, and submit `MlsKeyPackagePublished { key_package,
/// store_instance_hash, expires_at_log_pos }`.
///
/// See [`KEY_PACKAGE_LIFETIME_LOG_POSITIONS`] for how `expires_at_log_pos` is
/// chosen given that the client cannot observe the server's `log_pos`.
pub async fn publish_key_package<T: E2eeTransport + Sync>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    store: &FarderMlsStore,
    store_instance_hash: &[u8; 32],
) -> Result<KeyPackageOutcome, E2eeError> {
    // 1. Generate + serialize the KeyPackage.
    let bundle = generate_key_package(store, actor.device, &actor.identity.public_key())
        .map_err(|e| E2eeError::Mls(e.context("generate key package")))?;
    let key_package = bundle
        .key_package()
        .tls_serialize_detached()
        .map_err(|e| E2eeError::Mls(anyhow::anyhow!("serialize key package: {e}")))?;

    // 2. Log-position lifetime (see the constant's doc comment for why this is
    //    the honest, defensible value).
    let expires_at_log_pos = chain.next_seq + 1 + KEY_PACKAGE_LIFETIME_LOG_POSITIONS;

    // 3. Build + submit.
    let event = build_next_event(
        actor.device,
        actor.identity,
        actor.log_server_id,
        chain,
        event_now_secs(),
        EventPayload::MlsKeyPackagePublished {
            key_package: key_package.clone(),
            store_instance_hash: *store_instance_hash,
            expires_at_log_pos,
        },
    );
    let accepted = transport.submit_event(&event).await?;
    chain.advance(&event);

    Ok(KeyPackageOutcome {
        event_hash: accepted.event_hash,
        key_package,
        expires_at_log_pos,
    })
}

/// Make the creator's bootstrap commit at generation 0 / epoch 0 — the commit
/// that confirms the creator's own leaf and makes the channel addable (fact
/// A2.5). Emits an `MlsCommit` populated from the real commit outcome.
///
/// # Divergence contract (the critical hazard)
///
/// `MlsChannelGroup::self_update` (like `add_members` / `remove_members`)
/// merges the commit **locally and immediately**. So by the time this fn
/// submits the `MlsCommit`, the local group is already at `epoch + 1`. If the
/// server rejects that submit with the bare `"stale-epoch"` reason, this fn
/// returns [`E2eeError::StaleEpochDiverged`] and the local group is one epoch
/// AHEAD of the server — the caller must resync from the log (Task 6), never
/// keep using the group. Acceptance (`Ok`) is not proof of advancement either
/// (fact A2.1): a commit that lost the epoch race is accepted as a no-op.
pub async fn bootstrap_group<T: E2eeTransport + Sync>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    key: &ChannelKey,
    group: &mut MlsChannelGroup,
    store: &FarderMlsStore,
    store_instance_hash: &[u8; 32],
) -> Result<CommitSubmitted, E2eeError> {
    // The bootstrap commit attests the log head the author had folded. For a
    // fresh creator that is exactly its own chain head (the ChannelCreated it
    // just submitted) — the minimal honest value.
    let authz_head = chain.last_event_hash.clone().ok_or_else(|| {
        E2eeError::chain(
            "bootstrap commit needs a prior event (ChannelCreated) to attest its folded head",
        )
    })?;

    // 1. Perform the commit locally. self_update merges immediately.
    let outcome = group
        .self_update(store, &DeviceSigner(actor.device))
        .map_err(|e| E2eeError::Mls(e.context("bootstrap self-update")))?;
    debug_assert!(outcome.adds.is_empty() && outcome.removes.is_empty());
    let post_epoch_authenticator = group.epoch_authenticator();
    debug_assert_eq!(group.epoch(), outcome.epoch + 1);

    // 2. Build the MlsCommit event from the real CommitOutcome.
    let event = build_next_event(
        actor.device,
        actor.identity,
        &key.log_server_id,
        chain,
        event_now_secs(),
        EventPayload::MlsCommit {
            channel_id: key.channel_id,
            generation: 0,
            epoch: outcome.epoch,
            mls_message: outcome.commit_bytes,
            adds: vec![],
            removes: vec![],
            prev_epoch_authenticator: outcome.prev_epoch_authenticator,
            post_epoch_authenticator,
            post_tree_hash: outcome.post_tree_hash,
            authz_head,
            store_instance_hash: *store_instance_hash,
        },
    );

    // 3. Submit; surface a stale-epoch rejection as the explicit divergence
    //    error rather than a generic transport error.
    let accepted = match transport.submit_event(&event).await {
        Ok(a) => a,
        Err(e) if e.is_stale_epoch() => {
            return Err(E2eeError::StaleEpochDiverged {
                local_epoch: group.epoch(),
            });
        }
        Err(e) => return Err(e.into()),
    };
    chain.advance(&event);

    Ok(CommitSubmitted {
        event_hash: accepted.event_hash,
        local_epoch: group.epoch(),
        post_epoch_authenticator,
        post_tree_hash: outcome.post_tree_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::Actor;
    use farder_crypto::event_log::{device_id, Event, EventPayload};
    use farder_crypto::identity::Keypair;
    use farder_mls::group::decode_key_package;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::testing::FakeTransport;

    const SERVER_ID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let n = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "farder-e2ee-client-{}-{name}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn key(channel_id: u64) -> ChannelKey {
        ChannelKey::new(SERVER_ID.to_string(), channel_id).unwrap()
    }

    fn actor<'a>(identity: &'a Keypair, device: &'a Keypair) -> Actor<'a> {
        Actor {
            device,
            identity,
            log_server_id: SERVER_ID,
        }
    }

    fn spec(k: ChannelKey) -> ChannelSpec {
        ChannelSpec {
            key: k,
            name: "vault".to_string(),
            kind: "text".to_string(),
            parent: None,
        }
    }

    fn submitted_payloads(transport: &FakeTransport) -> Vec<EventPayload> {
        transport
            .submitted()
            .into_iter()
            .map(|e| e.core.payload)
            .collect()
    }

    fn last_submitted(transport: &FakeTransport) -> Event {
        transport.submitted().into_iter().last().expect("one event")
    }

    #[test]
    fn channel_group_id_is_stable_and_generation_scoped() {
        assert_eq!(
            channel_group_id("srv", 42, 0),
            "srv/42/generation-0".to_string()
        );
        assert_ne!(
            channel_group_id("srv", 42, 0),
            channel_group_id("srv", 42, 1)
        );
    }

    #[test]
    fn persist_and_read_store_instance_hash_roundtrips() {
        let dir = temp_dir("hash");
        let k = key(E2EE_CHANNEL_ID_FLOOR + 1);
        let hash = [7u8; 32];
        persist_store_instance_hash(&dir, &k, &hash).unwrap();
        assert_eq!(read_store_instance_hash(&dir, &k).unwrap(), hash);
    }

    #[test]
    fn read_store_instance_hash_rejects_a_wrong_sized_file() {
        let dir = temp_dir("hash-bad");
        let k = key(E2EE_CHANNEL_ID_FLOOR + 2);
        let path = k.instance_hash_path(&dir).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, [1u8; 31]).unwrap();
        assert!(read_store_instance_hash(&dir, &k).is_err());
    }

    #[tokio::test]
    async fn create_e2ee_channel_rejects_a_below_floor_channel_id() {
        let transport = FakeTransport::new();
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let a = actor(&identity, &device);
        let mut chain = ChainState::default();
        let s = spec(key(E2EE_CHANNEL_ID_FLOOR - 1));
        let result = create_e2ee_channel(&transport, &a, &mut chain, &s, &temp_dir("below-floor")).await;
        let err = match result {
            Ok(_) => panic!("a below-floor channel id must be rejected"),
            Err(e) => e,
        };
        assert!(matches!(
            err,
            E2eeError::ChannelIdBelowFloor { channel_id: c } if c == E2EE_CHANNEL_ID_FLOOR - 1
        ));
        assert_eq!(transport.submit_count(), 0, "nothing submitted for a bad id");
    }

    #[tokio::test]
    async fn create_e2ee_channel_submits_channel_created_and_mints_a_group() {
        let transport = FakeTransport::new();
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let a = actor(&identity, &device);
        let mut chain = ChainState::default();
        let dir = temp_dir("create");
        let channel_id = E2EE_CHANNEL_ID_FLOOR + 7;
        let k = key(channel_id);
        let s = spec(k.clone());

        let outcome = create_e2ee_channel(&transport, &a, &mut chain, &s, &dir).await.unwrap();

        // The submitted event is an owner ChannelCreated with class E2ee.
        let payloads = submitted_payloads(&transport);
        assert_eq!(payloads.len(), 1);
        assert!(matches!(
            &payloads[0],
            EventPayload::ChannelCreated { channel_id: cid, name, kind, class, parent }
                if *cid == channel_id && name == "vault" && kind == "text"
                    && *class == ChannelClass::E2ee && *parent == None
        ));

        // The local group is a fresh one-member (creator) group at epoch 0.
        assert_eq!(outcome.group.epoch(), 0);
        let members = outcome.group.members().unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].identity, identity.public_key());
        assert_eq!(members[0].device, device_id(&device.public_key()));

        // The instance hash was persisted and matches the store's.
        assert_eq!(read_store_instance_hash(&dir, &k).unwrap(), outcome.store_instance_hash);
        assert_eq!(outcome.store.store_instance_hash(), outcome.store_instance_hash);

        // The chain advanced past the ChannelCreated event.
        assert_eq!(chain.next_seq, 1);
        assert_eq!(chain.last_event_hash.as_deref(), Some(outcome.event_hash.as_str()));
    }

    #[tokio::test]
    async fn publish_key_package_roundtrips_through_decode_key_package() {
        let transport = FakeTransport::new();
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let a = actor(&identity, &device);
        let mut chain = ChainState::default();
        let dir = temp_dir("publish");
        let s = spec(key(E2EE_CHANNEL_ID_FLOOR + 9));

        let created = create_e2ee_channel(&transport, &a, &mut chain, &s, &dir).await.unwrap();

        let published = publish_key_package(
            &transport,
            &a,
            &mut chain,
            &created.store,
            &created.store_instance_hash,
        )
        .await
        .unwrap();

        // The published bytes decode back into a pinned-suite, farder-credential
        // KeyPackage (the store is a valid OpenMlsProvider for decoding).
        let kp = decode_key_package(&created.store, &published.key_package).unwrap();
        assert_eq!(kp.ciphersuite(), farder_mls::CIPHERSUITE);
        let (cred_identity, cred_device) =
            farder_mls::credential::decode_credential_identity(
                kp.leaf_node().credential().serialized_content(),
            )
            .unwrap();
        assert_eq!(cred_identity, identity.public_key());
        assert_eq!(cred_device, device_id(&device.public_key()));

        // The expiry is a finite value strictly past this device's own position.
        assert!(published.expires_at_log_pos > chain.next_seq);

        // The submitted event carries the same bytes + store hash + expiry.
        let last = last_submitted(&transport);
        match &last.core.payload {
            EventPayload::MlsKeyPackagePublished {
                key_package,
                store_instance_hash,
                expires_at_log_pos,
            } => {
                assert_eq!(key_package, &published.key_package);
                assert_eq!(store_instance_hash, &created.store_instance_hash);
                assert_eq!(*expires_at_log_pos, published.expires_at_log_pos);
            }
            other => panic!("expected MlsKeyPackagePublished, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bootstrap_group_advances_epoch_and_confirms_the_creator_leaf_shape() {
        let transport = FakeTransport::new();
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let a = actor(&identity, &device);
        let mut chain = ChainState::default();
        let dir = temp_dir("bootstrap");
        let k = key(E2EE_CHANNEL_ID_FLOOR + 11);
        let s = spec(k.clone());

        let mut created = create_e2ee_channel(&transport, &a, &mut chain, &s, &dir).await.unwrap();

        let outcome = bootstrap_group(
            &transport,
            &a,
            &mut chain,
            &k,
            &mut created.group,
            &created.store,
            &created.store_instance_hash,
        )
        .await
        .unwrap();

        // Local group advanced 0 -> 1.
        assert_eq!(outcome.local_epoch, 1);
        assert_eq!(created.group.epoch(), 1);
        assert_eq!(created.group.leaves().unwrap().len(), 1);
        assert_eq!(created.group.members().unwrap()[0].identity, identity.public_key());

        // The MlsCommit event carries the real CommitOutcome values.
        let last = last_submitted(&transport);
        match &last.core.payload {
            EventPayload::MlsCommit {
                channel_id,
                generation,
                epoch,
                adds,
                removes,
                prev_epoch_authenticator,
                post_epoch_authenticator,
                post_tree_hash,
                authz_head,
                store_instance_hash,
                ..
            } => {
                assert_eq!(*channel_id, k.channel_id);
                assert_eq!(*generation, 0);
                assert_eq!(*epoch, 0);
                assert!(adds.is_empty() && removes.is_empty());
                assert_eq!(post_epoch_authenticator, &outcome.post_epoch_authenticator);
                assert_eq!(post_tree_hash, &outcome.post_tree_hash);
                assert_ne!(prev_epoch_authenticator, post_epoch_authenticator);
                assert_eq!(store_instance_hash, &created.store_instance_hash);
                // The folded head is the ChannelCreated event this chain just made.
                assert_eq!(authz_head, &created.event_hash);
            }
            other => panic!("expected MlsCommit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_stale_bootstrap_commit_surfaces_as_diverged_not_success() {
        let transport = FakeTransport::new();
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let a = actor(&identity, &device);
        let mut chain = ChainState::default();
        let dir = temp_dir("stale-bootstrap");
        let k = key(E2EE_CHANNEL_ID_FLOOR + 13);
        let s = spec(k.clone());

        let mut created = create_e2ee_channel(&transport, &a, &mut chain, &s, &dir).await.unwrap();

        // Program the bootstrap submit to be rejected as stale-epoch.
        transport.reject_next("stale-epoch");

        let err = bootstrap_group(
            &transport,
            &a,
            &mut chain,
            &k,
            &mut created.group,
            &created.store,
            &created.store_instance_hash,
        )
        .await
        .unwrap_err();

        assert!(err.is_stale_epoch_diverged(), "expected divergence, got {err}");
        match err {
            E2eeError::StaleEpochDiverged { local_epoch } => {
                // The local group already advanced to epoch 1 (ahead of the server).
                assert_eq!(local_epoch, 1);
            }
            other => panic!("expected StaleEpochDiverged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bootstrap_requires_a_prior_event_to_attest_its_folded_head() {
        let transport = FakeTransport::new();
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let a = actor(&identity, &device);
        let mut chain = ChainState::default(); // empty: no prior event
        let dir = temp_dir("bootstrap-no-prev");
        let k = key(E2EE_CHANNEL_ID_FLOOR + 15);

        // Build a store + group directly (bypassing create_e2ee_channel so the
        // chain stays empty).
        let store_path = k.mls_store_path(&dir).unwrap();
        std::fs::create_dir_all(store_path.parent().unwrap()).unwrap();
        let (store, _) = FarderMlsStore::create(&store_path).unwrap();
        let mut group = MlsChannelGroup::create(
            &store,
            &DeviceSigner(&device),
            credential_with_key(&device, &identity.public_key()),
            channel_group_id(SERVER_ID, k.channel_id, 0).as_bytes(),
        )
        .unwrap();

        let err = bootstrap_group(
            &transport,
            &a,
            &mut chain,
            &k,
            &mut group,
            &store,
            &store.store_instance_hash(),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, E2eeError::Chain(_)));
        assert_eq!(transport.submit_count(), 0);
    }
}
