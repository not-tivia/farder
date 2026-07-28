//! Channel group operations: create / join-from-Welcome / add / remove /
//! self-update, with exposure of `epoch_authenticator` / `tree_hash` for
//! commit chaining and declared-vs-actual validation.
//!
//! [`CommitOutcome`] carries exactly the values sub-2's `MlsCommit` event
//! will carry (`prev_epoch_authenticator` captured **before** merging,
//! `post_tree_hash` after). [`ProcessedCommit`] is the member-side view of
//! someone else's commit; [`verify_declared_matches_actual`] is the
//! lying-commit detection the spec's commit-chaining section mandates on the
//! member side (the fold-side authenticator chain is sub-2).
//!
//! [`ActualLeaf`] — on [`ProcessedCommit::actual_adds`] and via
//! [`MlsChannelGroup::leaves`] — carries each **real** leaf's credential and
//! signature key, so sub-2 can run the spec's §Leaves rule
//! ([`crate::credential::verify_leaf_binding`]) against actual tree state
//! rather than credential bytes alone.
//!
//! Epoch conventions:
//! - [`CommitOutcome::epoch`] / [`ProcessedCommit::epoch`] are the epoch the
//!   commit was **authored in** (what `MlsCommit.epoch` carries; the fold
//!   checks it against `current_epoch`). Merging moves the group to
//!   `epoch + 1`.
//! - [`JoinInfo::epoch`] is the joiner's **starting** epoch (the epoch the
//!   adding commit moved the group into, i.e. adding-commit `epoch + 1`).

use anyhow::{anyhow, bail, Context, Result};
use farder_crypto::event_log::DeviceId;
use farder_crypto::identity::PublicKey;
use openmls::prelude::{
    tls_codec::Deserialize as TlsDeserialize, Credential, CredentialWithKey, GroupId, KeyPackage,
    KeyPackageIn, LeafNodeParameters, MlsGroup, MlsGroupCreateConfig, MlsGroupJoinConfig,
    MlsMessageBodyIn, MlsMessageBodyOut, MlsMessageIn, MlsMessageOut, ProcessedMessage,
    ProcessedMessageContent, ProtocolMessage, ProtocolVersion, SenderRatchetConfiguration,
    StagedCommit, StagedWelcome, PURE_PLAINTEXT_WIRE_FORMAT_POLICY,
};
use openmls_traits::OpenMlsProvider;

use crate::credential::{decode_credential_identity, DeviceSigner};
use crate::envelope::{check_preseal_limits, pad_to_bucket, unpad, MessageEnvelope};
use crate::{CIPHERSUITE, MAX_PAST_EPOCHS};

/// A group member as declared in fold-readable form: the spec's
/// `DeclaredAdd`/`DeclaredRemove` `{ identity, device }` shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeclaredMember {
    pub identity: PublicKey,
    pub device: DeviceId,
}

/// An **actual** leaf as it sits in the ratchet tree: the declared
/// `{ identity, device }` decoded from the leaf credential **plus** the raw
/// credential and leaf signature key. Carrying the real key pair is what lets
/// sub-2 run the spec's leaf-binding rule
/// ([`crate::credential::verify_leaf_binding`]) against actual tree state —
/// credential bytes alone can be cloned by an attacker onto a self-minted
/// KeyPackage whose signature/HPKE keys are attacker-owned, which OpenMLS
/// accepts (KeyPackages are self-signed; duplicate credential identities are
/// legal). Only `signature_key` vs. an identity-signed `DeviceCert` tells a
/// genuine leaf from that impostor.
#[derive(Debug, Clone)]
pub struct ActualLeaf {
    /// The `{ identity, device }` the leaf's credential *claims*.
    pub member: DeclaredMember,
    /// The leaf's credential exactly as it sits in the tree.
    pub credential: Credential,
    /// The leaf's signature public key bytes exactly as they sit in the tree.
    pub signature_key: Vec<u8>,
}

/// Everything sub-2's `MlsCommit` event needs from a commit we authored.
#[derive(Debug, Clone)]
pub struct CommitOutcome {
    /// Serialized `MlsMessageOut` carrying the commit (a `PublicMessage`).
    pub commit_bytes: Vec<u8>,
    /// Serialized `MlsMessageOut` carrying the Welcome, when adds were made.
    pub welcome_bytes: Option<Vec<u8>>,
    /// The epoch authenticator we held **before** merging this commit.
    pub prev_epoch_authenticator: [u8; 32],
    /// The tree hash **after** merging this commit.
    pub post_tree_hash: [u8; 32],
    /// The epoch this commit was authored in (pre-merge).
    pub epoch: u64,
    pub adds: Vec<DeclaredMember>,
    pub removes: Vec<DeclaredMember>,
}

/// The member-side view of someone else's commit, after processing+merging.
#[derive(Debug, Clone)]
pub struct ProcessedCommit {
    /// The actually-added leaves, with real credential + signature key so the
    /// caller (sub-2) can run [`crate::credential::verify_leaf_binding`] on
    /// each new leaf — the spec's §Leaves rule for commit processing.
    pub actual_adds: Vec<ActualLeaf>,
    pub actual_removes: Vec<DeclaredMember>,
    /// The tree hash after merging the commit.
    pub post_tree_hash: [u8; 32],
    /// The epoch the commit was authored in (pre-merge).
    pub epoch: u64,
}

/// The values a joiner's `MlsLeafConfirmed` (sub-2) will cite.
#[derive(Debug, Clone)]
pub struct JoinInfo {
    /// The joiner's starting epoch.
    pub epoch: u64,
    /// The tree hash of the group the joiner landed in (equals the adding
    /// commit's `post_tree_hash`).
    pub tree_hash: [u8; 32],
}

/// One MLS group per E2EE channel, wrapping [`MlsGroup`] with the spec's
/// fixed configuration: the MTI ciphersuite, ratchet-tree extension ON,
/// `max_past_epochs = 3`, `PURE_PLAINTEXT_WIRE_FORMAT_POLICY` for handshake
/// framing, default sender-ratchet configuration, and `padding_size(0)`
/// (bucket padding is ours, applied to the plaintext envelope — Task 4).
pub struct MlsChannelGroup {
    inner: MlsGroup,
    /// Current tree hash, maintained across every state transition (OpenMLS
    /// 0.8.1 gates `MlsGroup::tree_hash()` behind its `test-utils` feature,
    /// so we track it via the ungated `GroupContext` paths).
    tree_hash: [u8; 32],
}

fn create_config() -> MlsGroupCreateConfig {
    MlsGroupCreateConfig::builder()
        .ciphersuite(CIPHERSUITE)
        .use_ratchet_tree_extension(true)
        .max_past_epochs(MAX_PAST_EPOCHS)
        .wire_format_policy(PURE_PLAINTEXT_WIRE_FORMAT_POLICY)
        .sender_ratchet_configuration(SenderRatchetConfiguration::default())
        .padding_size(0)
        .build()
}

fn join_config() -> MlsGroupJoinConfig {
    MlsGroupJoinConfig::builder()
        .use_ratchet_tree_extension(true)
        .max_past_epochs(MAX_PAST_EPOCHS)
        .wire_format_policy(PURE_PLAINTEXT_WIRE_FORMAT_POLICY)
        .sender_ratchet_configuration(SenderRatchetConfiguration::default())
        .padding_size(0)
        .build()
}

fn hash32(bytes: &[u8]) -> Result<[u8; 32]> {
    bytes
        .try_into()
        .map_err(|_| anyhow!("expected a 32-byte hash, got {} bytes", bytes.len()))
}

fn declared_member(credential: &Credential) -> Result<DeclaredMember> {
    let (identity, device) = decode_credential_identity(credential.serialized_content())
        .context("member credential does not decode as a farder credential")?;
    Ok(DeclaredMember { identity, device })
}

fn actual_leaf(credential: &Credential, signature_key: &[u8]) -> Result<ActualLeaf> {
    Ok(ActualLeaf {
        member: declared_member(credential)?,
        credential: credential.clone(),
        signature_key: signature_key.to_vec(),
    })
}

/// Sort key for order-insensitive set comparison of declared members.
fn sorted(mut members: Vec<DeclaredMember>) -> Vec<DeclaredMember> {
    members.sort_by(|a, b| {
        (a.identity.as_bytes(), &a.device).cmp(&(b.identity.as_bytes(), &b.device))
    });
    members
}

/// Run [`MlsGroup::process_message`] with panics contained to a clean `Err`.
///
/// OpenMLS 0.8.1 fires `debug_assert!(false, "Ciphertext decryption failed")`
/// on AEAD failure (`framing/private_message_in.rs`), so in **debug builds**
/// (`cargo test`, `npm run tauri dev`) a tampered/undecryptable
/// `PrivateMessage` panics inside `process_message` where release builds
/// return `Err`. These bytes are attacker-suppliable (sub-4 feeds server
/// input into this path), so the panic is contained here rather than pushed
/// onto every consumer: both build profiles get the same clean `Err`.
fn process_message_contained(
    inner: &mut MlsGroup,
    provider: &impl OpenMlsProvider,
    message: ProtocolMessage,
) -> Result<ProcessedMessage> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        inner.process_message(provider, message)
    }))
    .map_err(|payload| {
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_owned());
        anyhow!("process message rejected undecryptable ciphertext: {msg}")
    })?
    .map_err(|e| anyhow!("process message: {e}"))
}

/// What a staged commit did: added leaves in **actual** form (credential +
/// signature key straight from the proposal's leaf node), removes in declared
/// form.
struct StagedSummary {
    adds: Vec<ActualLeaf>,
    removes: Vec<DeclaredMember>,
    post_tree_hash: [u8; 32],
    /// The epoch the commit was authored in (post-merge epoch minus one).
    authored_epoch: u64,
}

/// Extract adds/removes and the post-merge context from a staged commit,
/// resolving removed leaf indices against the (pre-merge) tree.
fn staged_commit_summary(inner: &MlsGroup, staged: &StagedCommit) -> Result<StagedSummary> {
    let mut adds = Vec::new();
    for add in staged.add_proposals() {
        let leaf = add.add_proposal().key_package().leaf_node();
        adds.push(actual_leaf(
            leaf.credential(),
            leaf.signature_key().as_slice(),
        )?);
    }
    let mut removes = Vec::new();
    for remove in staged.remove_proposals() {
        let index = remove.remove_proposal().removed();
        let member = inner
            .members()
            .find(|m| m.index == index)
            .with_context(|| format!("remove proposal targets unknown leaf index {index:?}"))?;
        removes.push(declared_member(&member.credential)?);
    }
    let post_tree_hash = hash32(staged.group_context().tree_hash())?;
    let post_epoch = staged.group_context().epoch().as_u64();
    let authored_epoch = post_epoch
        .checked_sub(1)
        .context("staged commit moves the group into epoch 0")?;
    Ok(StagedSummary {
        adds,
        removes,
        post_tree_hash,
        authored_epoch,
    })
}

impl MlsChannelGroup {
    /// Create a new one-member group for a channel, with the creator's leaf
    /// bound to `credential_with_key` (see `credential::credential_with_key`).
    pub fn create(
        provider: &impl OpenMlsProvider,
        signer: &DeviceSigner,
        credential_with_key: CredentialWithKey,
        channel_group_id: &[u8],
    ) -> Result<Self> {
        let inner = MlsGroup::new_with_group_id(
            provider,
            signer,
            &create_config(),
            GroupId::from_slice(channel_group_id),
            credential_with_key,
        )
        .map_err(|e| anyhow!("create group: {e}"))?;
        // OpenMLS 0.8.1 exposes no ungated tree-hash accessor on a live
        // group; export a (ratchet-tree-free) GroupInfo and read the group
        // context from it.
        let group_info = inner
            .export_group_info(provider.crypto(), signer, false)
            .map_err(|e| anyhow!("export group info: {e}"))?;
        let MlsMessageBodyOut::GroupInfo(gi) = group_info.body() else {
            bail!("export_group_info did not return a GroupInfo body");
        };
        let tree_hash = hash32(gi.group_context().tree_hash())?;
        Ok(Self { inner, tree_hash })
    }

    /// Load a persisted group from the provider's storage (the store-resume
    /// path — see `store::FarderMlsStore::resume`). Returns `Ok(None)` when
    /// the storage holds no group under `channel_group_id`.
    ///
    /// Needs the device `signer` because OpenMLS 0.8.1 exposes no ungated
    /// tree-hash accessor on a live group: the cached tree hash is re-derived
    /// by exporting a (ratchet-tree-free, signed) `GroupInfo`.
    pub fn load(
        provider: &impl OpenMlsProvider,
        signer: &DeviceSigner,
        channel_group_id: &[u8],
    ) -> Result<Option<Self>> {
        let Some(inner) =
            MlsGroup::load(provider.storage(), &GroupId::from_slice(channel_group_id))
                .map_err(|e| anyhow!("load group from storage: {e}"))?
        else {
            return Ok(None);
        };
        let group_info = inner
            .export_group_info(provider.crypto(), signer, false)
            .map_err(|e| anyhow!("export group info: {e}"))?;
        let MlsMessageBodyOut::GroupInfo(gi) = group_info.body() else {
            bail!("export_group_info did not return a GroupInfo body");
        };
        let tree_hash = hash32(gi.group_context().tree_hash())?;
        Ok(Some(Self { inner, tree_hash }))
    }

    /// Join a group from serialized Welcome bytes (self-contained via the
    /// ratchet-tree extension). Returns the group plus the [`JoinInfo`] the
    /// joiner's `MlsLeafConfirmed` (sub-2) will cite.
    pub fn join_from_welcome(
        provider: &impl OpenMlsProvider,
        welcome_bytes: &[u8],
    ) -> Result<(Self, JoinInfo)> {
        let message = MlsMessageIn::tls_deserialize_exact(welcome_bytes)
            .map_err(|e| anyhow!("welcome bytes do not parse as an MLS message: {e}"))?;
        let MlsMessageBodyIn::Welcome(welcome) = message.extract() else {
            bail!("message is not a Welcome");
        };
        let staged = StagedWelcome::new_from_welcome(provider, &join_config(), welcome, None)
            .map_err(|e| anyhow!("stage welcome: {e}"))?;
        let context = staged.group_context();
        let info = JoinInfo {
            epoch: context.epoch().as_u64(),
            tree_hash: hash32(context.tree_hash())?,
        };
        let inner = staged
            .into_group(provider)
            .map_err(|e| anyhow!("welcome into group: {e}"))?;
        Ok((
            Self {
                inner,
                tree_hash: info.tree_hash,
            },
            info,
        ))
    }

    /// Add members by their KeyPackages; commits and merges immediately.
    pub fn add_members(
        &mut self,
        provider: &impl OpenMlsProvider,
        signer: &DeviceSigner,
        key_packages: &[KeyPackage],
    ) -> Result<CommitOutcome> {
        let (prev_auth, epoch) = self.pre_commit_state();
        let (commit, welcome, _group_info) = self
            .inner
            .add_members(provider, signer, key_packages)
            .map_err(|e| anyhow!("add members: {e}"))?;
        self.finish_own_commit(provider, commit, Some(welcome), prev_auth, epoch)
    }

    /// Remove members, resolving each [`DeclaredMember`] to leaf indices by
    /// credential; commits and merges immediately.
    ///
    /// Removes **every** leaf whose credential claims the target
    /// `{ identity, device }` — duplicate credential identities are legal in
    /// MLS (an impostor can clone a real member's credential bytes onto its
    /// own leaf), so resolving to a single "first match" would be ambiguous.
    /// A target matching no leaf is an error.
    pub fn remove_members(
        &mut self,
        provider: &impl OpenMlsProvider,
        signer: &DeviceSigner,
        members: &[DeclaredMember],
    ) -> Result<CommitOutcome> {
        let mut indices = Vec::with_capacity(members.len());
        for target in members {
            let matched: Vec<_> = self
                .inner
                .members()
                .filter_map(|m| {
                    let declared = declared_member(&m.credential).ok()?;
                    (&declared == target).then_some(m.index)
                })
                .collect();
            if matched.is_empty() {
                bail!("no group member matches declared device {}", target.device);
            }
            indices.extend(matched);
        }
        indices.sort_unstable_by_key(|i| i.u32());
        indices.dedup();
        let (prev_auth, epoch) = self.pre_commit_state();
        let (commit, welcome, _group_info) = self
            .inner
            .remove_members(provider, signer, &indices)
            .map_err(|e| anyhow!("remove members: {e}"))?;
        self.finish_own_commit(provider, commit, welcome, prev_auth, epoch)
    }

    /// Rotate our own leaf secrets (the rekey-cadence primitive); commits and
    /// merges immediately.
    pub fn self_update(
        &mut self,
        provider: &impl OpenMlsProvider,
        signer: &DeviceSigner,
    ) -> Result<CommitOutcome> {
        let (prev_auth, epoch) = self.pre_commit_state();
        let bundle = self
            .inner
            .self_update(provider, signer, LeafNodeParameters::default())
            .map_err(|e| anyhow!("self update: {e}"))?;
        let welcome = bundle.to_welcome_msg();
        let (commit, _, _) = bundle.into_contents();
        self.finish_own_commit(provider, commit, welcome, prev_auth, epoch)
    }

    /// Process someone else's commit: verify + stage via OpenMLS, report the
    /// **actual** adds/removes and post-merge tree hash, then merge.
    pub fn process_commit(
        &mut self,
        provider: &impl OpenMlsProvider,
        commit_bytes: &[u8],
    ) -> Result<ProcessedCommit> {
        let message = MlsMessageIn::tls_deserialize_exact(commit_bytes)
            .map_err(|e| anyhow!("commit bytes do not parse as an MLS message: {e}"))?;
        let protocol_message = message
            .try_into_protocol_message()
            .map_err(|e| anyhow!("commit message is not a protocol message: {e}"))?;
        let processed = process_message_contained(&mut self.inner, provider, protocol_message)
            .context("process commit")?;
        let ProcessedMessageContent::StagedCommitMessage(staged) = processed.into_content() else {
            bail!("message is not a commit");
        };
        let summary = staged_commit_summary(&self.inner, &staged)?;
        self.inner
            .merge_staged_commit(provider, *staged)
            .map_err(|e| anyhow!("merge staged commit: {e}"))?;
        self.tree_hash = summary.post_tree_hash;
        Ok(ProcessedCommit {
            actual_adds: summary.adds,
            actual_removes: summary.removes,
            post_tree_hash: summary.post_tree_hash,
            epoch: summary.authored_epoch,
        })
    }

    /// The group's current epoch.
    pub fn epoch(&self) -> u64 {
        self.inner.epoch().as_u64()
    }

    /// The current epoch authenticator (RFC 9420; 32 bytes under this suite).
    pub fn epoch_authenticator(&self) -> [u8; 32] {
        self.inner
            .epoch_authenticator()
            .as_slice()
            .try_into()
            .expect("epoch authenticator is SHA-256-sized under the pinned suite")
    }

    /// The current tree hash (32 bytes under this suite).
    pub fn tree_hash(&self) -> [u8; 32] {
        self.tree_hash
    }

    /// All current members in declared `{ identity, device }` form. Errs if
    /// any leaf carries a credential that is not a farder credential.
    ///
    /// This is the *claimed* view only — for anything security-relevant use
    /// [`Self::leaves`], which also carries each leaf's real signature key.
    pub fn members(&self) -> Result<Vec<DeclaredMember>> {
        Ok(self.leaves()?.into_iter().map(|l| l.member).collect())
    }

    /// All current leaves in **actual** form: credential + leaf signature key
    /// per member, so the caller (sub-2) can run
    /// [`crate::credential::verify_leaf_binding`] against real tree state.
    /// Errs if any leaf carries a credential that is not a farder credential.
    pub fn leaves(&self) -> Result<Vec<ActualLeaf>> {
        self.inner
            .members()
            .map(|m| actual_leaf(&m.credential, &m.signature_key))
            .collect()
    }

    /// Seal a [`MessageEnvelope`] as an MLS application message
    /// (`PrivateMessage`): encode → pre-seal limits → pad to the bucket
    /// ladder → encrypt. Returns the serialized `MlsMessageOut` bytes.
    pub fn seal_message(
        &mut self,
        provider: &impl OpenMlsProvider,
        signer: &DeviceSigner,
        envelope: &MessageEnvelope,
    ) -> Result<Vec<u8>> {
        let encoded = envelope.encode()?;
        check_preseal_limits(envelope, encoded.len())?;
        let padded = pad_to_bucket(&encoded)?;
        let message = self
            .inner
            .create_message(provider, signer, &padded)
            .map_err(|e| anyhow!("seal message: {e}"))?;
        message
            .to_bytes()
            .map_err(|e| anyhow!("serialize sealed message: {e}"))
    }

    /// Open a sealed application message from another member: decrypt via
    /// OpenMLS → strict unpad → decode the envelope.
    pub fn open_message(
        &mut self,
        provider: &impl OpenMlsProvider,
        bytes: &[u8],
    ) -> Result<MessageEnvelope> {
        let message = MlsMessageIn::tls_deserialize_exact(bytes)
            .map_err(|e| anyhow!("sealed bytes do not parse as an MLS message: {e}"))?;
        let protocol_message = message
            .try_into_protocol_message()
            .map_err(|e| anyhow!("sealed message is not a protocol message: {e}"))?;
        let processed = process_message_contained(&mut self.inner, provider, protocol_message)
            .context("open message")?;
        let ProcessedMessageContent::ApplicationMessage(app) = processed.into_content() else {
            bail!("message is not an application message");
        };
        let encoded = unpad(&app.into_bytes())?;
        MessageEnvelope::decode(&encoded)
    }

    fn pre_commit_state(&self) -> ([u8; 32], u64) {
        (self.epoch_authenticator(), self.epoch())
    }

    /// Read declared members + post state from our own staged commit, merge
    /// it, and assemble the [`CommitOutcome`].
    ///
    /// On ANY error the staged pending commit is cleared (memory + storage):
    /// OpenMLS stages and **persists** the pending commit as part of
    /// `add_members`/`remove_members`/`self_update` before this method runs,
    /// so without the clear a failure here (e.g. a summary that bails on an
    /// undecodable credential) would wedge the group in
    /// `MlsGroupState::PendingCommit` — every later mutator fails with
    /// "a pending commit exists", and the wedge survives store restarts.
    fn finish_own_commit(
        &mut self,
        provider: &impl OpenMlsProvider,
        commit: MlsMessageOut,
        welcome: Option<MlsMessageOut>,
        prev_epoch_authenticator: [u8; 32],
        epoch: u64,
    ) -> Result<CommitOutcome> {
        let result =
            self.finish_own_commit_inner(provider, commit, welcome, prev_epoch_authenticator, epoch);
        if result.is_err() {
            // Belt-and-braces: never leave a failed own commit staged. A
            // no-op if the commit already merged (or none is pending).
            if let Err(clear_err) = self.inner.clear_pending_commit(provider.storage()) {
                return result.with_context(|| {
                    format!("additionally failed to clear the staged pending commit: {clear_err}")
                });
            }
        }
        result
    }

    fn finish_own_commit_inner(
        &mut self,
        provider: &impl OpenMlsProvider,
        commit: MlsMessageOut,
        welcome: Option<MlsMessageOut>,
        prev_epoch_authenticator: [u8; 32],
        epoch: u64,
    ) -> Result<CommitOutcome> {
        let staged = self
            .inner
            .pending_commit()
            .context("commit operation left no pending commit")?;
        let summary = staged_commit_summary(&self.inner, staged)?;
        if summary.authored_epoch != epoch {
            bail!(
                "staged commit epoch {} does not match group epoch {epoch}",
                summary.authored_epoch
            );
        }
        self.inner
            .merge_pending_commit(provider)
            .map_err(|e| anyhow!("merge pending commit: {e}"))?;
        self.tree_hash = summary.post_tree_hash;
        let commit_bytes = commit
            .to_bytes()
            .map_err(|e| anyhow!("serialize commit: {e}"))?;
        let welcome_bytes = welcome
            .map(|w| w.to_bytes())
            .transpose()
            .map_err(|e| anyhow!("serialize welcome: {e}"))?;
        Ok(CommitOutcome {
            commit_bytes,
            welcome_bytes,
            prev_epoch_authenticator,
            post_tree_hash: summary.post_tree_hash,
            epoch,
            adds: summary.adds.into_iter().map(|l| l.member).collect(),
            removes: summary.removes,
        })
    }
}

/// Strictly decode serialized KeyPackage bytes: TLS-decode, verify signatures
/// and validity via OpenMLS, require the pinned ciphersuite, and require the
/// leaf credential to decode as a **farder credential**
/// ([`decode_credential_identity`]). This is how KeyPackages received from
/// the log (sub-2) become [`KeyPackage`]s that
/// [`MlsChannelGroup::add_members`] accepts. Garbage, truncation, tampering,
/// wrong suites, and non-farder credentials are all errors.
///
/// The credential gate fails closed at this boundary on purpose: an
/// MLS-valid KeyPackage with a non-farder credential would otherwise pass
/// into `add_members`, whose staged commit fails only *after* OpenMLS has
/// persisted it — see `finish_own_commit`'s pending-commit clearing for the
/// second line of defense.
pub fn decode_key_package(
    provider: &impl OpenMlsProvider,
    bytes: &[u8],
) -> Result<KeyPackage> {
    let key_package_in = KeyPackageIn::tls_deserialize_exact(bytes)
        .map_err(|e| anyhow!("key package bytes do not decode: {e}"))?;
    let key_package = key_package_in
        .validate(provider.crypto(), ProtocolVersion::Mls10)
        .map_err(|e| anyhow!("key package is invalid: {e}"))?;
    if key_package.ciphersuite() != CIPHERSUITE {
        bail!(
            "key package uses ciphersuite {:?}, expected {:?}",
            key_package.ciphersuite(),
            CIPHERSUITE
        );
    }
    decode_credential_identity(key_package.leaf_node().credential().serialized_content())
        .context("key package credential is not a farder credential")?;
    Ok(key_package)
}

/// The member-side lying-commit check (spec §Commit chaining): the declared
/// adds/removes and post tree hash of an `MlsCommit` event must equal what
/// actually happened when the commit was processed. Order-insensitive on the
/// member sets; any mismatch is an error.
///
/// This compares the *declared* `{ identity, device }` sets only. It does NOT
/// prove the added leaves are genuine — an impostor leaf can clone a real
/// member's credential bytes over attacker-owned keys and pass this check.
/// Callers MUST additionally run
/// [`crate::credential::verify_leaf_binding`] on each
/// [`ProcessedCommit::actual_adds`] entry against a log-valid `DeviceCert`
/// (the spec's §Leaves rule).
pub fn verify_declared_matches_actual(
    processed: &ProcessedCommit,
    declared_adds: &[DeclaredMember],
    declared_removes: &[DeclaredMember],
    declared_post_tree_hash: &[u8; 32],
) -> Result<()> {
    let actual_adds: Vec<DeclaredMember> = processed
        .actual_adds
        .iter()
        .map(|l| l.member.clone())
        .collect();
    if sorted(actual_adds) != sorted(declared_adds.to_vec()) {
        bail!(
            "declared adds do not match actual adds (declared {}, actual {})",
            declared_adds.len(),
            processed.actual_adds.len()
        );
    }
    if sorted(processed.actual_removes.clone()) != sorted(declared_removes.to_vec()) {
        bail!(
            "declared removes do not match actual removes (declared {}, actual {})",
            declared_removes.len(),
            processed.actual_removes.len()
        );
    }
    if &processed.post_tree_hash != declared_post_tree_hash {
        bail!("declared post tree hash does not match the actual tree");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::{
        credential_with_key, encode_credential_identity, generate_key_package, verify_leaf_binding,
    };
    use farder_crypto::event_log::{device_id, DeviceCert};
    use farder_crypto::identity::Keypair;
    use openmls::prelude::tls_codec::Serialize as TlsSerialize;
    use openmls::prelude::BasicCredential;
    use openmls_rust_crypto::OpenMlsRustCrypto;

    /// (identity keypair, device keypair, provider) for one test member.
    fn member() -> (Keypair, Keypair, OpenMlsRustCrypto) {
        (
            Keypair::generate(),
            Keypair::generate(),
            OpenMlsRustCrypto::default(),
        )
    }

    fn declared(identity: &Keypair, device: &Keypair) -> DeclaredMember {
        DeclaredMember {
            identity: identity.public_key(),
            device: device_id(&device.public_key()),
        }
    }

    /// A self-minted KeyPackage whose credential *claims*
    /// `(victim_identity, victim_device)` but whose signature/HPKE keys are
    /// attacker-owned. OpenMLS accepts it (KeyPackages are self-signed by
    /// their own key; duplicate credential identities are legal) — only the
    /// leaf-binding check against a `DeviceCert` can tell it apart.
    fn impostor_key_package(
        provider: &OpenMlsRustCrypto,
        attacker_device: &Keypair,
        victim_identity: &Keypair,
        victim_device: &Keypair,
    ) -> KeyPackage {
        let credential: Credential = BasicCredential::new(encode_credential_identity(
            &victim_identity.public_key(),
            &device_id(&victim_device.public_key()),
        ))
        .into();
        let credential_with_key = CredentialWithKey {
            credential,
            signature_key: attacker_device.public_key().as_bytes().to_vec().into(),
        };
        KeyPackage::builder()
            .build(
                crate::CIPHERSUITE,
                provider,
                &DeviceSigner(attacker_device),
                credential_with_key,
            )
            .unwrap()
            .key_package()
            .clone()
    }

    /// An MLS-valid KeyPackage under the pinned suite whose credential is
    /// NOT a farder credential (arbitrary identity bytes): TLS-valid and
    /// self-consistently signed, so only the farder-credential gate in
    /// `decode_key_package` can refuse it.
    fn non_farder_key_package(provider: &OpenMlsRustCrypto, device: &Keypair) -> KeyPackage {
        let credential: Credential =
            BasicCredential::new(b"not-a-farder-credential".to_vec()).into();
        let credential_with_key = CredentialWithKey {
            credential,
            signature_key: device.public_key().as_bytes().to_vec().into(),
        };
        KeyPackage::builder()
            .build(
                crate::CIPHERSUITE,
                provider,
                &DeviceSigner(device),
                credential_with_key,
            )
            .unwrap()
            .key_package()
            .clone()
    }

    /// A three-member group: alice creates, adds bob + carol, both join.
    #[allow(clippy::type_complexity)]
    fn three_member_group() -> (
        (Keypair, Keypair, OpenMlsRustCrypto, MlsChannelGroup),
        (Keypair, Keypair, OpenMlsRustCrypto, MlsChannelGroup),
        (Keypair, Keypair, OpenMlsRustCrypto, MlsChannelGroup),
        CommitOutcome,
        JoinInfo,
        JoinInfo,
    ) {
        let (a_id, a_dev, a_prov) = member();
        let (b_id, b_dev, b_prov) = member();
        let (c_id, c_dev, c_prov) = member();

        let mut alice = MlsChannelGroup::create(
            &a_prov,
            &DeviceSigner(&a_dev),
            credential_with_key(&a_dev, &a_id.public_key()),
            b"server-1/channel-1/generation-0",
        )
        .unwrap();

        let b_bundle = generate_key_package(&b_prov, &b_dev, &b_id.public_key()).unwrap();
        let c_bundle = generate_key_package(&c_prov, &c_dev, &c_id.public_key()).unwrap();
        // Route bob's KeyPackage through the byte-level decode path sub-2
        // will use for KeyPackages read from the log.
        let b_kp_bytes = b_bundle.key_package().tls_serialize_detached().unwrap();
        let b_kp = decode_key_package(&a_prov, &b_kp_bytes).unwrap();

        let outcome = alice
            .add_members(
                &a_prov,
                &DeviceSigner(&a_dev),
                &[b_kp, c_bundle.key_package().clone()],
            )
            .unwrap();
        let welcome = outcome.welcome_bytes.clone().unwrap();
        let (bob, b_join) = MlsChannelGroup::join_from_welcome(&b_prov, &welcome).unwrap();
        let (carol, c_join) = MlsChannelGroup::join_from_welcome(&c_prov, &welcome).unwrap();

        (
            (a_id, a_dev, a_prov, alice),
            (b_id, b_dev, b_prov, bob),
            (c_id, c_dev, c_prov, carol),
            outcome,
            b_join,
            c_join,
        )
    }

    #[test]
    fn three_devices_form_a_group_via_add_commit_and_welcome() {
        let ((a_id, a_dev, _, alice), (b_id, b_dev, _, bob), (c_id, c_dev, _, carol), outcome, _, _) =
            three_member_group();

        assert_eq!(alice.epoch(), 1);
        assert_eq!(bob.epoch(), 1);
        assert_eq!(carol.epoch(), 1);

        let expected = sorted(vec![
            declared(&a_id, &a_dev),
            declared(&b_id, &b_dev),
            declared(&c_id, &c_dev),
        ]);
        assert_eq!(sorted(alice.members().unwrap()), expected);
        assert_eq!(sorted(bob.members().unwrap()), expected);
        assert_eq!(sorted(carol.members().unwrap()), expected);

        assert_eq!(
            sorted(outcome.adds.clone()),
            sorted(vec![declared(&b_id, &b_dev), declared(&c_id, &c_dev)])
        );
        assert!(outcome.removes.is_empty());
        assert_eq!(outcome.epoch, 0);
    }

    #[test]
    fn commit_outcome_chains_prev_epoch_authenticator_across_epochs() {
        let (a_id, a_dev, a_prov) = member();
        let (b_id, b_dev, b_prov) = member();

        let mut alice = MlsChannelGroup::create(
            &a_prov,
            &DeviceSigner(&a_dev),
            credential_with_key(&a_dev, &a_id.public_key()),
            b"chain-test",
        )
        .unwrap();

        let auth_epoch0 = alice.epoch_authenticator();
        let b_bundle = generate_key_package(&b_prov, &b_dev, &b_id.public_key()).unwrap();
        let add_outcome = alice
            .add_members(
                &a_prov,
                &DeviceSigner(&a_dev),
                &[b_bundle.key_package().clone()],
            )
            .unwrap();
        assert_eq!(add_outcome.prev_epoch_authenticator, auth_epoch0);

        let (mut bob, _) =
            MlsChannelGroup::join_from_welcome(&b_prov, &add_outcome.welcome_bytes.clone().unwrap())
                .unwrap();
        assert_eq!(bob.epoch_authenticator(), alice.epoch_authenticator());

        // Second commit: its prev must equal the post-add authenticator.
        let auth_epoch1 = alice.epoch_authenticator();
        assert_ne!(auth_epoch1, auth_epoch0);
        let update_outcome = alice.self_update(&a_prov, &DeviceSigner(&a_dev)).unwrap();
        assert_eq!(update_outcome.prev_epoch_authenticator, auth_epoch1);

        bob.process_commit(&b_prov, &update_outcome.commit_bytes)
            .unwrap();
        assert_eq!(bob.epoch_authenticator(), alice.epoch_authenticator());
        assert_ne!(bob.epoch_authenticator(), auth_epoch1);
    }

    #[test]
    fn processed_commit_reports_actual_adds_removes_and_matching_tree_hash() {
        let (
            (_a_id, a_dev, a_prov, mut alice),
            (_b_id, _b_dev, b_prov, mut bob),
            (c_id, c_dev, _, _carol),
            _,
            _,
            _,
        ) = three_member_group();

        let carol_member = declared(&c_id, &c_dev);
        let outcome = alice
            .remove_members(&a_prov, &DeviceSigner(&a_dev), &[carol_member.clone()])
            .unwrap();
        assert_eq!(outcome.removes, vec![carol_member.clone()]);

        let processed = bob.process_commit(&b_prov, &outcome.commit_bytes).unwrap();
        assert!(processed.actual_adds.is_empty());
        assert_eq!(processed.actual_removes, vec![carol_member]);
        assert_eq!(processed.post_tree_hash, outcome.post_tree_hash);
        assert_eq!(bob.tree_hash(), outcome.post_tree_hash);
        assert_eq!(processed.epoch, outcome.epoch);
        assert_eq!(alice.tree_hash(), bob.tree_hash());
    }

    #[test]
    fn declared_vs_actual_mismatch_is_detected() {
        let (
            (_a_id, a_dev, a_prov, mut alice),
            (_b_id, _b_dev, b_prov, mut bob),
            (c_id, c_dev, _, _carol),
            _,
            _,
            _,
        ) = three_member_group();

        // Alice self-updates, but the "event" lies: declares removes=[carol].
        let outcome = alice.self_update(&a_prov, &DeviceSigner(&a_dev)).unwrap();
        let processed = bob.process_commit(&b_prov, &outcome.commit_bytes).unwrap();

        // Honest declaration passes.
        verify_declared_matches_actual(&processed, &[], &[], &outcome.post_tree_hash).unwrap();

        // The lying-commit declaration is detected.
        let carol_member = declared(&c_id, &c_dev);
        assert!(verify_declared_matches_actual(
            &processed,
            &[],
            &[carol_member.clone()],
            &outcome.post_tree_hash
        )
        .is_err());
        assert!(verify_declared_matches_actual(
            &processed,
            &[carol_member],
            &[],
            &outcome.post_tree_hash
        )
        .is_err());

        // A lying post tree hash is detected too.
        let mut wrong_tree_hash = outcome.post_tree_hash;
        wrong_tree_hash[0] ^= 0xff;
        assert!(verify_declared_matches_actual(&processed, &[], &[], &wrong_tree_hash).is_err());
    }

    #[test]
    fn join_info_tree_hash_matches_the_commits_post_tree_hash() {
        let (_, (_, _, _, bob), (_, _, _, carol), outcome, b_join, c_join) = three_member_group();
        assert_eq!(b_join.tree_hash, outcome.post_tree_hash);
        assert_eq!(c_join.tree_hash, outcome.post_tree_hash);
        assert_eq!(b_join.epoch, outcome.epoch + 1);
        assert_eq!(bob.tree_hash(), outcome.post_tree_hash);
        assert_eq!(carol.tree_hash(), outcome.post_tree_hash);
    }

    #[test]
    fn add_with_wrong_suite_or_garbage_key_package_fails() {
        let (b_id, b_dev, b_prov) = member();
        let bundle = generate_key_package(&b_prov, &b_dev, &b_id.public_key()).unwrap();
        let valid_bytes = bundle.key_package().tls_serialize_detached().unwrap();
        let decoder_prov = OpenMlsRustCrypto::default();

        // The untampered bytes decode fine.
        decode_key_package(&decoder_prov, &valid_bytes).unwrap();

        // Truncation is refused.
        assert!(decode_key_package(&decoder_prov, &valid_bytes[..valid_bytes.len() - 1]).is_err());

        // A flipped ciphersuite field is refused (wrong/unknown suite, and the
        // signature no longer covers the bytes).
        let mut wrong_suite = valid_bytes.clone();
        wrong_suite[2] ^= 0xff;
        wrong_suite[3] ^= 0xff;
        assert!(decode_key_package(&decoder_prov, &wrong_suite).is_err());

        // A corrupted signature tail is refused.
        let mut bad_sig = valid_bytes.clone();
        let last = bad_sig.len() - 1;
        bad_sig[last] ^= 0xff;
        assert!(decode_key_package(&decoder_prov, &bad_sig).is_err());

        // Pure garbage is refused.
        assert!(decode_key_package(&decoder_prov, b"not a key package").is_err());
    }

    #[test]
    fn decode_key_package_rejects_mls_valid_non_farder_credential() {
        // TLS-valid, OpenMLS-valid, pinned suite — but the credential is not
        // a farder credential. The documented gate for KeyPackages read from
        // the log must fail closed here, BEFORE add_members can stage (and
        // persist) a commit that only fails later.
        let outsider_dev = Keypair::generate();
        let outsider_prov = OpenMlsRustCrypto::default();
        let kp = non_farder_key_package(&outsider_prov, &outsider_dev);
        let bytes = kp.tls_serialize_detached().unwrap();

        let decoder_prov = OpenMlsRustCrypto::default();
        let err = decode_key_package(&decoder_prov, &bytes).unwrap_err();
        assert!(
            err.to_string().contains("not a farder credential"),
            "expected the farder-credential gate to reject, got: {err}"
        );
    }

    #[test]
    fn failed_add_commit_does_not_wedge_the_group_in_pending_commit() {
        // The reviewer's empirical wedge scenario: a non-farder-credential
        // KeyPackage handed straight to add_members (bypassing the
        // decode_key_package gate). OpenMLS stages AND persists the pending
        // commit before finish_own_commit's summary bails on the
        // undecodable credential — without clear_pending_commit on the error
        // path, every subsequent mutator fails with "a pending commit
        // exists".
        let (a_id, a_dev, a_prov) = member();
        let mut alice = MlsChannelGroup::create(
            &a_prov,
            &DeviceSigner(&a_dev),
            credential_with_key(&a_dev, &a_id.public_key()),
            b"wedge-test",
        )
        .unwrap();

        let outsider_dev = Keypair::generate();
        let outsider_prov = OpenMlsRustCrypto::default();
        let kp = non_farder_key_package(&outsider_prov, &outsider_dev);

        let epoch_before = alice.epoch();
        let auth_before = alice.epoch_authenticator();
        let tree_before = alice.tree_hash();
        assert!(alice
            .add_members(&a_prov, &DeviceSigner(&a_dev), &[kp])
            .is_err());

        // The failed commit left no trace: state unchanged, and the group is
        // NOT stuck in PendingCommit — a self_update still succeeds.
        assert_eq!(alice.epoch(), epoch_before);
        assert_eq!(alice.epoch_authenticator(), auth_before);
        assert_eq!(alice.tree_hash(), tree_before);
        let outcome = alice.self_update(&a_prov, &DeviceSigner(&a_dev)).unwrap();
        assert_eq!(outcome.epoch, epoch_before);
        assert_eq!(alice.epoch(), epoch_before + 1);

        // And further mutators keep working (the wedge was persistent, so
        // one success is not enough evidence on its own).
        alice.self_update(&a_prov, &DeviceSigner(&a_dev)).unwrap();
        assert_eq!(alice.epoch(), epoch_before + 2);
    }

    #[test]
    fn leaves_expose_signature_keys_that_pass_leaf_binding_with_real_certs() {
        let ((a_id, a_dev, _, alice), (b_id, b_dev, _, _), (c_id, c_dev, _, _), _, _, _) =
            three_member_group();
        let leaves = alice.leaves().unwrap();
        assert_eq!(leaves.len(), 3);
        for (id, dev) in [(&a_id, &a_dev), (&b_id, &b_dev), (&c_id, &c_dev)] {
            let leaf = leaves
                .iter()
                .find(|l| l.member == declared(id, dev))
                .expect("every member has a leaf");
            assert_eq!(leaf.signature_key, dev.public_key().as_bytes().to_vec());
            let cert = DeviceCert::create(id, &dev.public_key(), 1_700_000_000);
            verify_leaf_binding(&leaf.credential, &leaf.signature_key, &cert).unwrap();
        }
    }

    #[test]
    fn impostor_add_passes_declared_check_but_fails_leaf_binding_on_actual_leaf() {
        let (
            (_a_id, a_dev, a_prov, mut alice),
            (b_id, b_dev, b_prov, mut bob),
            _,
            _,
            _,
            _,
        ) = three_member_group();

        // Attacker mints a KeyPackage claiming bob's (identity, device) over
        // attacker-owned keys. The strict byte-decode path accepts it: it is
        // self-consistently signed under the pinned suite.
        let attacker_dev = Keypair::generate();
        let attacker_prov = OpenMlsRustCrypto::default();
        let impostor = impostor_key_package(&attacker_prov, &attacker_dev, &b_id, &b_dev);
        let impostor_bytes = impostor.tls_serialize_detached().unwrap();
        let impostor = decode_key_package(&a_prov, &impostor_bytes).unwrap();

        let outcome = alice
            .add_members(&a_prov, &DeviceSigner(&a_dev), &[impostor])
            .unwrap();
        let processed = bob.process_commit(&b_prov, &outcome.commit_bytes).unwrap();

        // The declared identity/device set check alone CANNOT catch this: the
        // cloned credential claims bob, and the tree hash is declared honestly.
        let declared_bob = declared(&b_id, &b_dev);
        verify_declared_matches_actual(
            &processed,
            &[declared_bob.clone()],
            &[],
            &outcome.post_tree_hash,
        )
        .unwrap();

        // But the actual added leaf carries the attacker's signature key, so
        // the spec's leaf-binding rule catches it against bob's real cert.
        let bob_cert = DeviceCert::create(&b_id, &b_dev.public_key(), 1_700_000_000);
        assert_eq!(processed.actual_adds.len(), 1);
        let added = &processed.actual_adds[0];
        assert_eq!(added.member, declared_bob);
        assert_eq!(
            added.signature_key,
            attacker_dev.public_key().as_bytes().to_vec()
        );
        assert!(verify_leaf_binding(&added.credential, &added.signature_key, &bob_cert).is_err());

        // leaves() exposes it on the standing tree too: of the two leaves
        // claiming bob, exactly the genuine one passes leaf binding.
        let bob_claiming: Vec<_> = bob
            .leaves()
            .unwrap()
            .into_iter()
            .filter(|l| l.member == declared_bob)
            .collect();
        assert_eq!(bob_claiming.len(), 2);
        let passing = bob_claiming
            .iter()
            .filter(|l| verify_leaf_binding(&l.credential, &l.signature_key, &bob_cert).is_ok())
            .count();
        assert_eq!(passing, 1);
    }

    #[test]
    fn remove_members_removes_every_leaf_claiming_the_target() {
        let (
            (_a_id, a_dev, a_prov, mut alice),
            (_b_id, _b_dev, b_prov, mut bob),
            (c_id, c_dev, _, _),
            _,
            _,
            _,
        ) = three_member_group();

        // An impostor leaf cloning carol's credential joins the tree.
        let attacker_dev = Keypair::generate();
        let attacker_prov = OpenMlsRustCrypto::default();
        let impostor = impostor_key_package(&attacker_prov, &attacker_dev, &c_id, &c_dev);
        let add = alice
            .add_members(&a_prov, &DeviceSigner(&a_dev), &[impostor])
            .unwrap();
        bob.process_commit(&b_prov, &add.commit_bytes).unwrap();
        assert_eq!(alice.members().unwrap().len(), 4);

        // Removing declared carol removes BOTH leaves that claim her — a
        // first-match resolution would ambiguously leave the impostor behind.
        let carol_member = declared(&c_id, &c_dev);
        let outcome = alice
            .remove_members(&a_prov, &DeviceSigner(&a_dev), &[carol_member.clone()])
            .unwrap();
        assert_eq!(
            outcome.removes,
            vec![carol_member.clone(), carol_member.clone()]
        );
        assert_eq!(alice.members().unwrap().len(), 2);
        assert!(!alice.members().unwrap().contains(&carol_member));

        let processed = bob.process_commit(&b_prov, &outcome.commit_bytes).unwrap();
        assert_eq!(processed.actual_removes.len(), 2);
        assert_eq!(bob.members().unwrap().len(), 2);
    }

    #[test]
    fn self_update_rotates_the_epoch_without_membership_change() {
        let (
            (_a_id, a_dev, a_prov, mut alice),
            (_b_id, _b_dev, b_prov, mut bob),
            (_c_id, _c_dev, c_prov, mut carol),
            _,
            _,
            _,
        ) = three_member_group();

        let epoch_before = alice.epoch();
        let members_before = sorted(alice.members().unwrap());
        let auth_before = alice.epoch_authenticator();

        let outcome = alice.self_update(&a_prov, &DeviceSigner(&a_dev)).unwrap();
        assert!(outcome.adds.is_empty());
        assert!(outcome.removes.is_empty());
        assert_eq!(outcome.epoch, epoch_before);

        assert_eq!(alice.epoch(), epoch_before + 1);
        assert_eq!(sorted(alice.members().unwrap()), members_before);
        assert_ne!(alice.epoch_authenticator(), auth_before);

        bob.process_commit(&b_prov, &outcome.commit_bytes).unwrap();
        carol.process_commit(&c_prov, &outcome.commit_bytes).unwrap();
        assert_eq!(bob.epoch(), epoch_before + 1);
        assert_eq!(sorted(bob.members().unwrap()), members_before);
        assert_eq!(bob.epoch_authenticator(), alice.epoch_authenticator());
        assert_eq!(carol.epoch_authenticator(), alice.epoch_authenticator());
    }
}
