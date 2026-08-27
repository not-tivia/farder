//! The join path: fetch Welcomes addressed to this device, join the MLS group
//! from the Welcome, and confirm our own leaf so we may send sealed content.
//!
//! This is Task 3 of the 4a vertical. It is deliberately **not** steward add
//! (Task 4), sealed send/receive (Task 5), resync (Task 6), or the server
//! emit sites (Task 7).
//!
//! # Store lifecycle contract (the critical footgun)
//!
//! [`crate::channel::publish_key_package`] stores a KeyPackage's **private key
//! material** in the provider's storage. [`MlsChannelGroup::join_from_welcome`]
//! needs that same material to decrypt the Welcome. A joiner that publishes
//! with store A and joins with a freshly-created store B fails, and the failure
//! surfaces as a corrupt/foreign Welcome rather than a provider mistake.
//!
//! The contract, therefore:
//!
//! - **create-once, at KeyPackage-publish time**: call [`create_joiner_store`]
//!   once per channel, then publish the KeyPackage from the store it returns.
//! - **resume thereafter**: every later session calls [`resume_store`] to
//!   reopen that same on-disk store before [`join_channel`].
//! - **resume errors are terminal**: [`E2eeError::StoreResumeTerminal`] is
//!   surfaced, never papered over by deleting + re-creating the store (that
//!   would silently destroy group state and the sender-ratchet counter).
//!
//! # Tree-hash honesty
//!
//! `MlsLeafConfirmed` must cite `tree_hash` equal to the cited epoch's commit
//! `post_tree_hash`. [`JoinInfo::tree_hash`] is documented to equal the adding
//! commit's `post_tree_hash`, so this module submits it verbatim. There is **no
//! local cross-check** against the steward's *declared* `post_tree_hash`: this
//! crate's transport seam cannot fetch the adding `MlsCommit` (only messages
//! via `fetch_history_v2`). A lying steward that declared a wrong
//! `post_tree_hash` is therefore only caught by the fold, which rejects the
//! confirmation — fail-closed, but not pre-empted client-side.

use std::path::Path;

use farder_crypto::event_log::{device_id, Event, EventPayload};
use farder_crypto::identity::PublicKey;
use farder_mls::group::{JoinInfo, MlsChannelGroup};
use farder_mls::store::FarderMlsStore;

use crate::chain::{build_next_event, event_now_secs, Actor, ChainState};
use crate::channel::{persist_store_instance_hash, read_store_instance_hash, E2eeError};
use crate::channel_key::ChannelKey;
use crate::transport::{E2eeTransport, TransportError};

/// One Welcome addressed to this device, extracted from the raw signed
/// `MlsWelcome` event bytes the server returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingWelcome {
    pub channel_id: u64,
    pub generation: u64,
    /// The raw serialized `MlsMessageOut` Welcome bytes, exactly as logged.
    pub welcome: Vec<u8>,
}

/// A member's **local** belief about whether it may send sealed content.
///
/// This crate holds no server `LogState`, so the only evidence it has is
/// whether its own `MlsLeafConfirmed` was accepted. `can_send` is therefore a
/// LOCAL BELIEF, not authoritative fold truth: the fold may still reject a
/// sealed send for a stale epoch, a pending removal, a freshness-ceiling hit,
/// or an incomplete reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendEligibility {
    confirmed: bool,
}

impl SendEligibility {
    /// The pre-confirmation state: our own `MlsLeafConfirmed` has not been
    /// accepted, so [`Self::can_send`] is `false`.
    pub fn not_confirmed() -> Self {
        Self { confirmed: false }
    }

    /// The post-confirmation state (also returned by [`confirm_leaf`]). Public
    /// so a caller that already knows its leaf is confirmed (e.g. a resumed
    /// session) can reconstruct the belief without re-running `confirm_leaf`.
    pub fn confirmed() -> Self {
        Self { confirmed: true }
    }

    /// Whether this member may send sealed content (local belief — see the type
    /// doc for the honest caveat).
    pub fn can_send(&self) -> bool {
        self.confirmed
    }

    /// Refuse a sealed send locally unless confirmed (fact A2.6): before
    /// `MlsLeafConfirmed` the fold rejects with `"sealed content author does
    /// not hold a confirmed leaf"`, so the client refuses up front with a typed
    /// error rather than round-tripping a doomed event. Task 5's `send_sealed`
    /// must call this first.
    pub fn ensure_can_send(&self) -> Result<(), E2eeError> {
        if self.confirmed {
            Ok(())
        } else {
            Err(E2eeError::NotConfirmed)
        }
    }
}

/// The outcome of a successful [`confirm_leaf`]: our `MlsLeafConfirmed` was
/// accepted.
#[derive(Debug)]
pub struct LeafConfirmation {
    /// Server-assigned hash of the accepted `MlsLeafConfirmed` event.
    pub event_hash: String,
    /// The epoch our leaf landed in (the joiner's starting epoch).
    pub epoch: u64,
    /// Local send-eligibility belief, now confirmed.
    pub eligibility: SendEligibility,
}

impl LeafConfirmation {
    /// Whether this member may send sealed content — `true` once our own
    /// `MlsLeafConfirmed` was accepted (see [`SendEligibility`] for the honest
    /// limitation).
    pub fn can_send(&self) -> bool {
        self.eligibility.can_send()
    }
}

/// Fetch every pending Welcome addressed to this device, paginating to
/// exhaustion per fact A2.8: feed the returned `next_accept_seq` back as the
/// next `since_accept_seq` and loop while `more == true`. The server's cursor
/// advances past **non-matching** rows too, so a restart-from-0 loop would
/// never make progress — this loop keeps the cursor monotonic.
///
/// `channel_id` narrows the fetch (it never widens it). The server already
/// filters by `for_member`; this fn additionally filters by `for_device` (a
/// member with several devices receives one Welcome per device) and does not
/// verify the steward's event signature — the Welcome's own MLS framing
/// provides integrity, and the steward's device cert is not at hand here.
pub async fn fetch_pending_welcomes<T: E2eeTransport + Sync>(
    transport: &T,
    actor: &Actor<'_>,
    channel_id: Option<u64>,
    since_accept_seq: u64,
) -> Result<Vec<PendingWelcome>, E2eeError> {
    let my_identity = actor.identity.public_key();
    let my_device = device_id(&actor.device.public_key());

    let mut out = Vec::new();
    let mut cursor = since_accept_seq;
    loop {
        let page = transport.fetch_welcomes(channel_id, cursor).await?;
        for bytes in &page.events {
            if let Some(welcome) = match_pending_welcome(bytes, &my_identity, &my_device)? {
                out.push(welcome);
            }
        }
        if !page.more {
            break;
        }
        // A `more == true` page that did not advance the cursor would loop
        // forever. The server guarantees progress (the cursor advances past
        // non-matching rows), but surface a non-advancing cursor loudly rather
        // than spin (this codebase's recurring unexitable-state bug class).
        if page.next_accept_seq <= cursor {
            return Err(TransportError::transport(
                "fetch_welcomes returned more=true without advancing the cursor",
            )
            .into());
        }
        cursor = page.next_accept_seq;
    }
    Ok(out)
}

/// Decode one raw signed event and, if it is an `MlsWelcome` addressed to
/// `(my_identity, my_device)`, return it as a [`PendingWelcome`].
fn match_pending_welcome(
    bytes: &[u8],
    my_identity: &PublicKey,
    my_device: &str,
) -> Result<Option<PendingWelcome>, E2eeError> {
    let event = Event::from_bytes(bytes).map_err(|e| {
        TransportError::transport(format!("decode welcome event bytes: {e}"))
    })?;
    let EventPayload::MlsWelcome {
        channel_id,
        generation,
        for_member,
        for_device,
        welcome,
        ..
    } = &event.core.payload
    else {
        // The server only returns MlsWelcome rows, but a transport bug or a
        // hand-rolled fake should not crash the loop: skip non-Welcome rows.
        return Ok(None);
    };
    if for_member != my_identity || for_device.as_str() != my_device {
        return Ok(None);
    }
    Ok(Some(PendingWelcome {
        channel_id: *channel_id,
        generation: *generation,
        welcome: welcome.clone(),
    }))
}

/// Create a joiner's fresh MLS store for one channel and persist its instance
/// hash beside it — the **create-once-at-publish-time** half of the store
/// lifecycle contract. The returned store MUST be the one passed to
/// [`crate::channel::publish_key_package`], and later reopened via
/// [`resume_store`]: its KeyPackage private material lives here and
/// [`MlsChannelGroup::join_from_welcome`] needs it to decrypt the Welcome.
///
/// `FarderMlsStore::create` refuses an existing path, so this fails on a
/// second call for the same channel (deliberate — never silently recreate).
pub fn create_joiner_store(
    data_dir: &Path,
    key: &ChannelKey,
) -> Result<(FarderMlsStore, [u8; 32]), E2eeError> {
    let store_path = key.mls_store_path(data_dir).map_err(E2eeError::chain)?;
    if let Some(parent) = store_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| E2eeError::Mls(anyhow::anyhow!("create mls dir: {e}")))?;
    }
    let (store, _instance_id) = FarderMlsStore::create(&store_path)
        .map_err(|e| E2eeError::Mls(e.context("create joiner MLS store")))?;
    let hash = store.store_instance_hash();
    persist_store_instance_hash(data_dir, key, &hash)?;
    Ok((store, hash))
}

/// Resume the on-disk store for a channel — the **resume-thereafter** half of
/// the store lifecycle contract. Reads the persisted instance hash and passes
/// it to `FarderMlsStore::resume`.
///
/// A resume failure is [`E2eeError::StoreResumeTerminal`] and must NOT be
/// recovered by deleting + re-creating the store (that would silently destroy
/// group state); the caller self-`DeviceRevoked`s and re-provisions (sub-5).
pub fn resume_store(
    data_dir: &Path,
    key: &ChannelKey,
) -> Result<(FarderMlsStore, [u8; 32]), E2eeError> {
    let store_path = key.mls_store_path(data_dir).map_err(E2eeError::chain)?;
    let hash = read_store_instance_hash(data_dir, key)?;
    let store = FarderMlsStore::resume(&store_path, &hash).map_err(E2eeError::StoreResumeTerminal)?;
    Ok((store, hash))
}

/// Join the group from Welcome bytes (local: no events are submitted). Returns
/// the joined group plus the [`JoinInfo`] a later [`confirm_leaf`] cites.
///
/// `store` must be the SAME store the KeyPackage was published from — see the
/// module-level store lifecycle contract.
pub fn join_channel(
    store: &FarderMlsStore,
    welcome_bytes: &[u8],
) -> Result<(MlsChannelGroup, JoinInfo), E2eeError> {
    MlsChannelGroup::join_from_welcome(store, welcome_bytes)
        .map_err(|e| E2eeError::Mls(e.context("join channel from welcome")))
}

/// Confirm our own leaf: submit `MlsLeafConfirmed { channel_id, generation,
/// epoch, tree_hash, store_instance_hash }`, authored **by the joining device**
/// (`event_log_state.rs:1271-1277` requires the confirmation's
/// `(author, device)` to be a pending leaf, i.e. the joiner itself).
///
/// `epoch` / `tree_hash` come from [`join_channel`]'s [`JoinInfo`]; `tree_hash`
/// equals the adding commit's `post_tree_hash` by construction, and is not
/// locally cross-checked against the steward's *declared* value (no fetch
/// surface for the adding `MlsCommit` — see the module doc).
pub async fn confirm_leaf<T: E2eeTransport + Sync>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    key: &ChannelKey,
    store_instance_hash: &[u8; 32],
    pending: &PendingWelcome,
    join_info: &JoinInfo,
) -> Result<LeafConfirmation, E2eeError> {
    if key.channel_id != pending.channel_id {
        return Err(E2eeError::chain(format!(
            "confirming channel {} but the Welcome is for channel {}",
            key.channel_id, pending.channel_id
        )));
    }

    let event = build_next_event(
        actor.device,
        actor.identity,
        &key.log_server_id,
        chain,
        event_now_secs(),
        EventPayload::MlsLeafConfirmed {
            channel_id: key.channel_id,
            generation: pending.generation,
            epoch: join_info.epoch,
            tree_hash: join_info.tree_hash,
            store_instance_hash: *store_instance_hash,
        },
    );
    let accepted = transport.submit_event(&event).await?;
    chain.advance(&event);

    Ok(LeafConfirmation {
        event_hash: accepted.event_hash,
        epoch: join_info.epoch,
        eligibility: SendEligibility::confirmed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::identity::Keypair;
    use farder_mls::credential::{credential_with_key, generate_key_package, DeviceSigner};
    use farder_mls::group::decode_key_package;
    use tls_codec::Serialize as TlsSerialize;

    use crate::testing::FakeTransport;
    use crate::transport::{EventAccepted, Welcomes};

    const SERVER_ID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    fn key(channel_id: u64) -> ChannelKey {
        ChannelKey::new(SERVER_ID.to_string(), channel_id).unwrap()
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static DIR_SEQ: AtomicU64 = AtomicU64::new(0);
        let n = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "farder-e2ee-client-join-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// A (joiner store, joiner identity, joiner device, welcome bytes, epoch,
    /// tree_hash) bundle for the join/confirm tests. The steward runs on a
    /// separate store so the joiner's store lifecycle is exercised faithfully.
    fn joined_fixture(
        channel_id: u64,
    ) -> (
        FarderMlsStore,
        Keypair,
        Keypair,
        [u8; 32],
        PendingWelcome,
        JoinInfo,
    ) {
        let dir = temp_dir("fixture");
        let k = key(channel_id);

        let steward_identity = Keypair::generate();
        let steward_device = Keypair::generate();
        let joiner_identity = Keypair::generate();
        let joiner_device = Keypair::generate();

        // Steward's store: a separate file so the joiner's own create-once
        // store at the canonical path is exercised faithfully.
        let steward_store_path = {
            let mut p = k.mls_store_path(&dir).unwrap();
            p.set_file_name(format!("{}.steward.sqlite", channel_id));
            p
        };
        std::fs::create_dir_all(steward_store_path.parent().unwrap()).unwrap();
        let (steward_store, _) = FarderMlsStore::create(&steward_store_path).unwrap();
        let mut steward_group = MlsChannelGroup::create(
            &steward_store,
            &DeviceSigner(&steward_device),
            credential_with_key(&steward_device, &steward_identity.public_key()),
            crate::channel::channel_group_id(SERVER_ID, channel_id, 0).as_bytes(),
        )
        .unwrap();

        // Joiner's store: created via the crate's own create-once helper.
        let (joiner_store, joiner_hash) = create_joiner_store(&dir, &k).unwrap();

        // Joiner's KeyPackage, generated FROM the joiner's store so its private
        // material lives there (the whole point of the footgun contract).
        let joiner_bundle =
            generate_key_package(&joiner_store, &joiner_device, &joiner_identity.public_key())
                .unwrap();
        let joiner_kp_bytes = joiner_bundle.key_package().tls_serialize_detached().unwrap();
        let joiner_kp = decode_key_package(&steward_store, &joiner_kp_bytes).unwrap();

        let add_outcome = steward_group
            .add_members(
                &steward_store,
                &DeviceSigner(&steward_device),
                &[joiner_kp],
            )
            .unwrap();
        let welcome = add_outcome.welcome_bytes.clone().unwrap();
        let join_info = JoinInfo {
            epoch: add_outcome.epoch + 1,
            tree_hash: add_outcome.post_tree_hash,
        };

        let pending = PendingWelcome {
            channel_id,
            generation: 0,
            welcome,
        };

        (
            joiner_store,
            joiner_identity,
            joiner_device,
            joiner_hash,
            pending,
            join_info,
        )
    }

    /// Build a signed `MlsWelcome` event addressed to `(identity, device)`,
    /// signed by an unrelated steward device (the join path does not verify the
    /// steward's signature — only `for_member`/`for_device` matter here).
    fn welcome_event(
        steward_device: &Keypair,
        steward_identity: &Keypair,
        for_identity: &Keypair,
        for_device: &Keypair,
        channel_id: u64,
        generation: u64,
        welcome: Vec<u8>,
    ) -> Vec<u8> {
        let chain = ChainState::default();
        build_next_event(
            steward_device,
            steward_identity,
            SERVER_ID,
            &chain,
            event_now_secs(),
            EventPayload::MlsWelcome {
                channel_id,
                generation,
                commit: "0f".repeat(32),
                for_member: for_identity.public_key(),
                for_device: device_id(&for_device.public_key()),
                welcome,
            },
        )
        .to_bytes()
    }

    /// A scripted `E2eeTransport` for the pagination test: serves welcome pages
    /// as a function of `since_accept_seq` and records every cursor requested.
    struct PaginatedWelcomeTransport {
        pages: std::collections::HashMap<u64, Welcomes>,
        requested: std::sync::Mutex<Vec<u64>>,
    }

    impl PaginatedWelcomeTransport {
        fn requested(&self) -> Vec<u64> {
            self.requested.lock().unwrap().clone()
        }
    }

    impl E2eeTransport for PaginatedWelcomeTransport {
        fn submit_event(
            &self,
            event: &Event,
        ) -> impl std::future::Future<Output = Result<EventAccepted, TransportError>> + Send {
            let event = event.clone();
            async move {
                Ok(EventAccepted {
                    event_hash: event.hash(),
                    timestamp: event.core.timestamp,
                })
            }
        }

        fn fetch_welcomes(
            &self,
            channel_id: Option<u64>,
            since_accept_seq: u64,
        ) -> impl std::future::Future<Output = Result<Welcomes, TransportError>> + Send {
            self.requested.lock().unwrap().push(since_accept_seq);
            let _ = channel_id;
            let page = self
                .pages
                .get(&since_accept_seq)
                .cloned()
                .unwrap_or(Welcomes {
                    events: Vec::new(),
                    next_accept_seq: since_accept_seq,
                    more: false,
                });
            async move { Ok(page) }
        }

        fn fetch_key_packages(
            &self,
            _member: &PublicKey,
            _device: &str,
        ) -> impl std::future::Future<Output = Result<Vec<Vec<u8>>, TransportError>> + Send {
            async move { Ok(Vec::new()) }
        }

        fn fetch_history_v2(
            &self,
            _channel_id: u64,
            _before_id: Option<u64>,
            _limit: u32,
        ) -> impl std::future::Future<
            Output = Result<Vec<farder_protocol::server::MessageInfoV2>, TransportError>,
        > + Send {
            async move { Ok(Vec::new()) }
        }
    }

    #[tokio::test]
    async fn fetch_pending_welcomes_paginates_past_unrelated_rows_to_reach_the_target() {
        let joiner_identity = Keypair::generate();
        let joiner_device = Keypair::generate();
        let steward_identity = Keypair::generate();
        let steward_device = Keypair::generate();
        let channel_id = 1 << 40;

        let target = welcome_event(
            &steward_device,
            &steward_identity,
            &joiner_identity,
            &joiner_device,
            channel_id,
            0,
            vec![1, 2, 3, 4],
        );

        // The target sits behind >500 rows for OTHER recipients: two full
        // 500-row pages (empty of our matches) precede it.
        let mut pages = std::collections::HashMap::new();
        pages.insert(
            0,
            Welcomes {
                events: Vec::new(),
                next_accept_seq: 500,
                more: true,
            },
        );
        pages.insert(
            500,
            Welcomes {
                events: Vec::new(),
                next_accept_seq: 1000,
                more: true,
            },
        );
        pages.insert(
            1000,
            Welcomes {
                events: vec![target.clone()],
                next_accept_seq: 1001,
                more: false,
            },
        );
        let transport = PaginatedWelcomeTransport {
            pages,
            requested: std::sync::Mutex::new(Vec::new()),
        };

        let actor = Actor {
            device: &joiner_device,
            identity: &joiner_identity,
            log_server_id: SERVER_ID,
        };

        let found = fetch_pending_welcomes(&transport, &actor, Some(channel_id), 0)
            .await
            .unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].channel_id, channel_id);
        assert_eq!(found[0].generation, 0);
        assert_eq!(found[0].welcome, vec![1, 2, 3, 4]);

        // The cursor must have advanced monotonically (never restarted from 0).
        assert_eq!(transport.requested(), vec![0, 500, 1000]);
    }

    #[test]
    fn match_pending_welcome_ignores_welcomes_for_other_devices_and_non_welcome_rows() {
        let joiner_identity = Keypair::generate();
        let joiner_device = Keypair::generate();
        let other_device = Keypair::generate();
        let steward_identity = Keypair::generate();
        let steward_device = Keypair::generate();
        let channel_id = 1 << 41;

        // A Welcome for the SAME identity but a DIFFERENT device must be
        // ignored: the server filters `for_member` only, so per-device
        // filtering is this client's job.
        let wrong_device = welcome_event(
            &steward_device,
            &steward_identity,
            &joiner_identity,
            &other_device,
            channel_id,
            0,
            vec![9, 9],
        );
        let ours = welcome_event(
            &steward_device,
            &steward_identity,
            &joiner_identity,
            &joiner_device,
            channel_id,
            0,
            vec![7, 7],
        );

        // A non-MlsWelcome row (channel filtering happens server-side via the
        // transport's `channel_id`; match_pending_welcome only matches the
        // (identity, device) the server did not already filter).
        let non_welcome = build_next_event(
            &steward_device,
            &steward_identity,
            SERVER_ID,
            &ChainState::default(),
            event_now_secs(),
            EventPayload::MessagePosted {
                channel_id,
                content: "hi".to_string(),
                reply_to: None,
                attachments: vec![],
            },
        )
        .to_bytes();

        let my_identity = joiner_identity.public_key();
        let my_device = device_id(&joiner_device.public_key());
        assert!(match_pending_welcome(&wrong_device, &my_identity, &my_device)
            .unwrap()
            .is_none());
        assert!(match_pending_welcome(&non_welcome, &my_identity, &my_device)
            .unwrap()
            .is_none());
        let matched = match_pending_welcome(&ours, &my_identity, &my_device)
            .unwrap()
            .unwrap();
        assert_eq!(matched.channel_id, channel_id);
        assert_eq!(matched.welcome, vec![7, 7]);
    }

    #[test]
    fn create_joiner_store_then_resume_roundtrips_the_same_instance_hash() {
        let dir = temp_dir("store-lifecycle");
        let k = key(1 << 42);

        let (store, hash) = create_joiner_store(&dir, &k).unwrap();
        assert_eq!(store.store_instance_hash(), hash);
        drop(store);

        let (_resumed, resumed_hash) = resume_store(&dir, &k).unwrap();
        assert_eq!(resumed_hash, hash);
    }

    #[test]
    fn create_joiner_store_refuses_to_recreate_over_an_existing_store() {
        let dir = temp_dir("store-recreate");
        let k = key(1 << 43);
        create_joiner_store(&dir, &k).unwrap();
        // A second create on the same channel must fail (create-once contract).
        assert!(create_joiner_store(&dir, &k).is_err());
    }

    #[test]
    fn resume_store_surfaces_a_terminal_error_when_the_hash_file_is_absent() {
        let dir = temp_dir("store-no-hash");
        let k = key(1 << 44);
        // No create_joiner_store call: the instance-hash file does not exist.
        // (`unwrap_err` needs the Ok type to be Debug; FarderMlsStore is not.)
        match resume_store(&dir, &k) {
            Ok(_) => panic!("resume with no persisted hash must fail"),
            Err(e) => assert!(matches!(e, E2eeError::Mls(_)), "got {e:?}"),
        }
    }

    #[test]
    fn join_channel_joins_from_a_real_welcome_and_reports_epoch_and_tree_hash() {
        let (joiner_store, joiner_identity, joiner_device, _hash, pending, join_info) =
            joined_fixture(1 << 45);

        let (group, got_info) = join_channel(&joiner_store, &pending.welcome).unwrap();

        // The joiner lands in the epoch the adding commit moved the group into.
        assert_eq!(got_info.epoch, join_info.epoch);
        assert_eq!(got_info.tree_hash, join_info.tree_hash);
        assert_eq!(group.epoch(), got_info.epoch);
        assert_eq!(group.tree_hash(), got_info.tree_hash);

        // Both members are present.
        let members = group.members().unwrap();
        assert!(members.iter().any(|m| m.identity == joiner_identity.public_key()
            && m.device == device_id(&joiner_device.public_key())));
    }

    #[tokio::test]
    async fn confirm_leaf_submits_an_event_authored_by_the_joining_device_with_the_right_values() {
        let transport = FakeTransport::new();
        let k = key(1 << 46);
        let (_joiner_store, joiner_identity, joiner_device, hash, pending, join_info) =
            joined_fixture(k.channel_id);

        let actor = Actor {
            device: &joiner_device,
            identity: &joiner_identity,
            log_server_id: SERVER_ID,
        };
        let mut chain = ChainState::default();

        let confirmation = confirm_leaf(
            &transport,
            &actor,
            &mut chain,
            &k,
            &hash,
            &pending,
            &join_info,
        )
        .await
        .unwrap();

        assert!(confirmation.can_send());
        assert_eq!(confirmation.epoch, join_info.epoch);

        let events = transport.submitted();
        assert_eq!(events.len(), 1);
        let event = &events[0];
        // Authored BY the joining device.
        assert_eq!(event.core.author, joiner_identity.public_key());
        assert_eq!(event.core.device, device_id(&joiner_device.public_key()));
        assert_eq!(event.core.server_id, SERVER_ID);

        match &event.core.payload {
            EventPayload::MlsLeafConfirmed {
                channel_id,
                generation,
                epoch,
                tree_hash,
                store_instance_hash,
            } => {
                assert_eq!(*channel_id, k.channel_id);
                assert_eq!(*generation, pending.generation);
                assert_eq!(*epoch, join_info.epoch);
                assert_eq!(*tree_hash, join_info.tree_hash);
                assert_eq!(store_instance_hash, &hash);
            }
            other => panic!("expected MlsLeafConfirmed, got {other:?}"),
        }

        assert_eq!(chain.next_seq, 1);
        assert_eq!(chain.last_event_hash.as_deref(), Some(confirmation.event_hash.as_str()));
    }

    #[tokio::test]
    async fn confirm_leaf_rejects_a_welcome_for_a_different_channel() {
        let transport = FakeTransport::new();
        let k = key(1 << 47);
        let (_joiner_store, joiner_identity, joiner_device, hash, pending, join_info) =
            joined_fixture(k.channel_id);

        // Cross the channel ids: the key says X, the Welcome says Y.
        let wrong_key = key(1 << 48);
        let actor = Actor {
            device: &joiner_device,
            identity: &joiner_identity,
            log_server_id: SERVER_ID,
        };
        let mut chain = ChainState::default();

        let err = confirm_leaf(
            &transport,
            &actor,
            &mut chain,
            &wrong_key,
            &hash,
            &pending,
            &join_info,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, E2eeError::Chain(_)), "got {err:?}");
        assert_eq!(transport.submit_count(), 0, "nothing submitted for a mismatch");
    }

    #[test]
    fn pre_confirmation_send_is_refused_with_a_typed_error() {
        let pre = SendEligibility::not_confirmed();
        assert!(!pre.can_send());
        match pre.ensure_can_send() {
            Err(E2eeError::NotConfirmed) => {}
            other => panic!("expected NotConfirmed, got {other:?}"),
        }

        let post = SendEligibility::confirmed();
        assert!(post.can_send());
        assert!(post.ensure_can_send().is_ok());
    }
}
