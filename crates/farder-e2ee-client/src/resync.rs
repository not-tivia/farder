//! Task 6 of the 4a vertical: the bounded stale-epoch resync loop.
//!
//! When a sealed send loses an epoch race, the server bounces it with the bare
//! `"stale-epoch"` reason (finding F6: the ingest pre-check now fires for
//! `MessagePostedE2ee` / `MessageEditedE2ee` too, not just `MlsCommit`). The
//! remedy is the same for both sealed payload kinds: fetch the winning commits
//! from the log, process them through the two receive-side gates, re-seal at
//! the new epoch, and resubmit. This module owns that loop and its two
//! termination bounds.
//!
//! # Why it is bounded (the recurring unexitable-state bug class)
//!
//! A client that keeps losing the race against a fast committer could resync
//! forever: every fetch yields a fresh commit, every resubmit is stale again.
//! The loop therefore carries TWO independent bounds and must terminate under
//! every transport behaviour:
//!
//! 1. **Unproductive bound** — an attempt whose resync did not advance the
//!    group's epoch was unproductive. [`MAX_UNPRODUCTIVE_RESYNC_ATTEMPTS`]
//!    consecutive unproductive attempts surface
//!    [`E2eeError::ResyncEquivocation`].
//! 2. **Total bound** — [`MAX_TOTAL_RESYNC_ATTEMPTS`] caps the loop no matter
//!    whether the epoch keeps advancing, so a client racing a fast committer
//!    stops instead of spinning forever.
//!
//! Both bounds are pinned by tests that assert termination, not just a happy
//! path.
//!
//! # The F4 poisoned-group contract
//!
//! [`process_incoming_commit`] runs Gate 1 (declared-vs-actual check) and
//! Gate 2 (leaf binding) on every fetched commit. Gate 1 merges the commit
//! BEFORE Gate 2 can inspect the actually-added leaves, and farder-mls offers
//! no rollback — so a [`IncomingCommitOutcome::LeafBindingFailure`] means the
//! impostor leaf is ALREADY in the local group. That is terminal for the group
//! instance: this loop aborts with [`E2eeError::ResyncPoisoned`] and never
//! retries through it.

use farder_crypto::event_log::{Event, EventPayload};
use farder_mls::group::MlsChannelGroup;
use farder_mls::store::FarderMlsStore;

use crate::chain::{Actor, ChainState};
use crate::channel::E2eeError;
use crate::commit::{
    process_incoming_commit, DeclaredCommit, DeviceCertResolver, IncomingCommitOutcome,
};
use crate::join::SendEligibility;
use crate::sealed::{send_sealed, SealContext, SealedSendOutcome};
use crate::transport::{E2eeTransport, TransportError};

/// Consecutive resync attempts that made no progress before the loop surfaces
/// [`E2eeError::ResyncEquivocation`]. "No progress" means the group's epoch did
/// not advance between attempts — the fetch yielded no winning commit we could
/// apply.
pub const MAX_UNPRODUCTIVE_RESYNC_ATTEMPTS: usize = 3;

/// Absolute cap on resync attempts, regardless of progress. A client that keeps
/// losing the race against a fast committer still stops here rather than
/// spinning forever.
pub const MAX_TOTAL_RESYNC_ATTEMPTS: usize = 10;

/// The outcome of a successful [`send_sealed_resync`]: the send result plus the
/// control-plane cursor the caller should persist and feed back in as
/// `since_accept_seq` on the next call (this crate owns no storage).
#[derive(Debug)]
pub struct ResyncOutcome {
    pub send: SealedSendOutcome,
    pub next_accept_seq: u64,
}

/// The fixed inputs for one [`send_sealed_resync`] call beyond the
/// [`SealContext`]: this device's send-eligibility belief and the caller's
/// persisted control-plane cursor (`since_accept_seq`), which this crate owns
/// no storage for. Bundled (like [`SealContext`] / `StewardContext`) to keep
/// `send_sealed_resync` under the clippy argument bound.
pub struct ResyncRequest<'a> {
    pub eligibility: &'a SendEligibility,
    pub since_accept_seq: u64,
}

/// [`send_sealed`] with automatic resync on a `stale-epoch` rejection.
///
/// On the bare `"stale-epoch"` reason, fetch the channel's MLS control plane
/// (via [`fetch_mls_control_exhaustive`]), apply every winning commit in order
/// through [`process_incoming_commit`]'s two gates, then re-seal at the new
/// epoch and resubmit. The loop is bounded twice (see the module doc): at most
/// [`MAX_UNPRODUCTIVE_RESYNC_ATTEMPTS`] attempts that do not advance the epoch,
/// and at most [`MAX_TOTAL_RESYNC_ATTEMPTS`] attempts in total. Either bound
/// surfaces [`E2eeError::ResyncEquivocation`] — the send is not retried into
/// the ground. A [`E2eeError::ResyncPoisoned`] aborts immediately (F4): the
/// local group is poisoned and must not be used further.
///
/// `request.since_accept_seq` is the caller's persisted control-plane cursor
/// (start at 0 the first time); the advanced cursor is returned on `Ok` for the
/// caller to persist.
pub async fn send_sealed_resync<T: E2eeTransport + Sync>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    ctx: &SealContext<'_>,
    group: &mut MlsChannelGroup,
    request: &ResyncRequest<'_>,
    certs: &impl DeviceCertResolver,
) -> Result<ResyncOutcome, E2eeError> {
    let mut unproductive = 0usize;
    let mut attempts = 0usize;
    let mut last_epoch = group.epoch();
    let mut cursor = request.since_accept_seq;

    loop {
        match send_sealed(transport, actor, chain, ctx, group, request.eligibility).await {
            Ok(send) => return Ok(ResyncOutcome { send, next_accept_seq: cursor }),
            Err(E2eeError::Transport(e)) if e.is_stale_epoch() => {
                attempts += 1;

                // Fetch and apply the winning commits: this advances the group
                // to the server's current epoch (or errors, e.g. F4 poison).
                let (events, next) =
                    fetch_mls_control_exhaustive(transport, ctx.key.channel_id, cursor).await?;
                cursor = next;
                apply_commits(group, ctx.store, certs, events)?;

                let new_epoch = group.epoch();
                if new_epoch == last_epoch {
                    unproductive += 1;
                    if unproductive >= MAX_UNPRODUCTIVE_RESYNC_ATTEMPTS {
                        return Err(E2eeError::ResyncEquivocation {
                            attempts,
                            last_epoch: new_epoch,
                        });
                    }
                } else {
                    unproductive = 0;
                    last_epoch = new_epoch;
                }

                if attempts >= MAX_TOTAL_RESYNC_ATTEMPTS {
                    return Err(E2eeError::ResyncEquivocation {
                        attempts,
                        last_epoch: new_epoch,
                    });
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// Fetch one channel's MLS control plane to exhaustion, decoding each raw
/// signed event and returning them oldest-first alongside the final cursor.
///
/// Pagination mirrors [`crate::join::fetch_pending_welcomes`] (fact A2.8): loop
/// while `more`, feeding `next_accept_seq` back as `since_accept_seq`. A
/// `more == true` page that does not advance the cursor is surfaced as a
/// transport error rather than spun on (commit `a2afff8` fixed the server-side
/// version of that stall; this is the client-side guard, kept consistent with
/// Task 3's welcome loop).
pub async fn fetch_mls_control_exhaustive<T: E2eeTransport + Sync>(
    transport: &T,
    channel_id: u64,
    since_accept_seq: u64,
) -> Result<(Vec<Event>, u64), E2eeError> {
    let mut out = Vec::new();
    let mut cursor = since_accept_seq;
    loop {
        let page = transport.fetch_mls_control(channel_id, cursor).await?;
        for bytes in &page.events {
            let event = Event::from_bytes(bytes).map_err(|e| {
                TransportError::transport(format!("decode mls control event: {e}"))
            })?;
            out.push(event);
        }
        if !page.more {
            return Ok((out, page.next_accept_seq));
        }
        if page.next_accept_seq <= cursor {
            return Err(TransportError::transport(
                "fetch_mls_control returned more=true without advancing the cursor",
            )
            .into());
        }
        cursor = page.next_accept_seq;
    }
}

/// Apply every `MlsCommit` in `events`, oldest-first, through
/// [`process_incoming_commit`]'s two gates. Non-commit control events
/// (`MlsWelcome` / `MlsLeafConfirmed` / `MlsGroupReset`) are skipped — they are
/// not needed to advance the group. A gap (a commit ahead of the group's
/// current epoch) and a Gate-1 mismatch are surfaced as errors; a Gate-2
/// [`IncomingCommitOutcome::LeafBindingFailure`] is surfaced as
/// [`E2eeError::ResyncPoisoned`] (F4: the group is poisoned and must not be
/// used further).
fn apply_commits(
    group: &mut MlsChannelGroup,
    store: &FarderMlsStore,
    certs: &impl DeviceCertResolver,
    events: Vec<Event>,
) -> Result<(), E2eeError> {
    for event in events {
        let EventPayload::MlsCommit {
            mls_message,
            adds,
            removes,
            post_tree_hash,
            epoch,
            ..
        } = &event.core.payload
        else {
            continue;
        };
        let declared = DeclaredCommit {
            epoch: *epoch,
            adds: adds.clone(),
            removes: removes.clone(),
            post_tree_hash: *post_tree_hash,
        };
        match process_incoming_commit(store, group, mls_message, &declared, certs)? {
            IncomingCommitOutcome::Applied { .. } => {}
            IncomingCommitOutcome::OutOfOrder { current_epoch, received_epoch }
                if received_epoch < current_epoch =>
            {
                // Replay of a commit we already applied: skip it.
            }
            IncomingCommitOutcome::OutOfOrder { received_epoch, .. } => {
                return Err(E2eeError::chain(format!(
                    "resync: commit at epoch {received_epoch} arrived out of order — a gap \
                     blocks advancing the group"
                )));
            }
            IncomingCommitOutcome::LeafBindingFailure { member, reason } => {
                return Err(E2eeError::ResyncPoisoned { member, reason });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::event_log::{device_id, DeclaredAdd, EventPayload as EP};
    use farder_crypto::identity::Keypair;
    use farder_mls::credential::{credential_with_key, generate_key_package, DeviceSigner};
    use farder_mls::group::decode_key_package;
    use std::collections::{HashMap, VecDeque};
    use std::future::Future;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tls_codec::Serialize as TlsSerialize;

    use crate::channel::channel_group_id;
    use crate::channel_key::ChannelKey;
    use crate::transport::{EventAccepted, MlsControl, Welcomes};

    const SERVER_ID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

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

    /// A mid-life chain state with a non-empty head, so `send_sealed`'s
    /// `authz_head` requirement is satisfiable without the full lifecycle.
    fn mid_chain() -> ChainState {
        ChainState {
            next_seq: 5,
            last_event_hash: Some("0f".repeat(32)),
            lamport: 5,
        }
    }

    /// A resolver that knows no certs — valid here because every test commit is
    /// a `self_update` with no adds, so Gate 2 never queries it.
    struct EmptyCerts;

    impl DeviceCertResolver for EmptyCerts {
        fn device_cert(&self, _identity: &farder_crypto::identity::PublicKey, _device: &str) -> Option<farder_crypto::event_log::DeviceCert> {
            None
        }
    }

    /// A two-member group on disk: alice creates the group and adds bob, bob
    /// joins. Both end at epoch 1, each on its own store.
    struct TwoMember {
        alice_id: Keypair,
        alice_dev: Keypair,
        alice_store: FarderMlsStore,
        alice_group: MlsChannelGroup,
        bob_id: Keypair,
        bob_dev: Keypair,
        bob_store: FarderMlsStore,
        bob_group: MlsChannelGroup,
    }

    fn two_member(channel_id: u64) -> TwoMember {
        let dir = temp_dir("two-member");
        let k = key(channel_id);

        let alice_id = Keypair::generate();
        let alice_dev = Keypair::generate();
        let bob_id = Keypair::generate();
        let bob_dev = Keypair::generate();

        let alice_store_path = {
            let mut p = k.mls_store_path(&dir).unwrap();
            p.set_file_name(format!("{}.alice.sqlite", channel_id));
            p
        };
        std::fs::create_dir_all(alice_store_path.parent().unwrap()).unwrap();
        let (alice_store, _) = FarderMlsStore::create(&alice_store_path).unwrap();
        let mut alice_group = MlsChannelGroup::create(
            &alice_store,
            &DeviceSigner(&alice_dev),
            credential_with_key(&alice_dev, &alice_id.public_key()),
            channel_group_id(SERVER_ID, channel_id, 0).as_bytes(),
        )
        .unwrap();

        let bob_store_path = {
            let mut p = k.mls_store_path(&dir).unwrap();
            p.set_file_name(format!("{}.bob.sqlite", channel_id));
            p
        };
        std::fs::create_dir_all(bob_store_path.parent().unwrap()).unwrap();
        let (bob_store, _) = FarderMlsStore::create(&bob_store_path).unwrap();

        let bob_bundle = generate_key_package(&bob_store, &bob_dev, &bob_id.public_key()).unwrap();
        let bob_kp_bytes = bob_bundle.key_package().tls_serialize_detached().unwrap();
        let bob_kp = decode_key_package(&alice_store, &bob_kp_bytes).unwrap();

        let add_outcome = alice_group
            .add_members(&alice_store, &DeviceSigner(&alice_dev), &[bob_kp])
            .unwrap();
        let welcome = add_outcome.welcome_bytes.clone().unwrap();
        let (bob_group, _) = MlsChannelGroup::join_from_welcome(&bob_store, &welcome).unwrap();

        TwoMember {
            alice_id,
            alice_dev,
            alice_store,
            alice_group,
            bob_id,
            bob_dev,
            bob_store,
            bob_group,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        use std::sync::atomic::AtomicU64;
        static DIR_SEQ: AtomicU64 = AtomicU64::new(0);
        let n = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "farder-e2ee-client-resync-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Wrap an alice `CommitOutcome` into a signed `MlsCommit` event's bytes.
    /// The event signature is a throwaway device — the resync loop decodes but
    /// never verifies it (integrity rides on the MLS commit framing itself).
    fn commit_event(channel_id: u64, o: &farder_mls::group::CommitOutcome) -> Vec<u8> {
        let dev = Keypair::generate();
        let id = Keypair::generate();
        crate::chain::build_next_event(
            &dev,
            &id,
            SERVER_ID,
            &ChainState::default(),
            crate::chain::event_now_secs(),
            EP::MlsCommit {
                channel_id,
                generation: 0,
                epoch: o.epoch,
                mls_message: o.commit_bytes.clone(),
                adds: vec![],
                removes: vec![],
                prev_epoch_authenticator: o.prev_epoch_authenticator,
                post_epoch_authenticator: [0u8; 32],
                post_tree_hash: o.post_tree_hash,
                authz_head: "0f".repeat(32),
                store_instance_hash: [0u8; 32],
            },
        )
        .to_bytes()
    }

    /// A transport that rejects the first N `submit_event` calls with
    /// `"stale-epoch"` (then accepts), and serves one queued `MlsCommit` event
    /// per `fetch_mls_control` call as a single non-truncated page.
    struct ResyncTransport {
        commits: Mutex<VecDeque<Vec<u8>>>,
        reject_count: AtomicUsize,
        submit_calls: AtomicUsize,
    }

    impl ResyncTransport {
        fn new() -> Self {
            Self {
                commits: Mutex::new(VecDeque::new()),
                reject_count: AtomicUsize::new(0),
                submit_calls: AtomicUsize::new(0),
            }
        }

        fn reject_then_accept(&self, n: usize) {
            self.reject_count.store(n, Ordering::SeqCst);
        }

        fn serve_commit(&self, bytes: Vec<u8>) {
            self.commits.lock().unwrap().push_back(bytes);
        }

        fn submit_calls(&self) -> usize {
            self.submit_calls.load(Ordering::SeqCst)
        }
    }

    impl E2eeTransport for ResyncTransport {
        fn submit_event(
            &self,
            event: &Event,
        ) -> impl Future<Output = Result<EventAccepted, TransportError>> + Send {
            let call = self.submit_calls.fetch_add(1, Ordering::SeqCst);
            let event = event.clone();
            let result = if call < self.reject_count.load(Ordering::SeqCst) {
                Err(TransportError::rejected("stale-epoch"))
            } else {
                Ok(EventAccepted {
                    event_hash: event.hash(),
                    timestamp: event.core.timestamp,
                })
            };
            async move { result }
        }

        fn fetch_welcomes(
            &self,
            _channel_id: Option<u64>,
            _since_accept_seq: u64,
        ) -> impl Future<Output = Result<Welcomes, TransportError>> + Send {
            async move {
                Ok(Welcomes {
                    events: Vec::new(),
                    next_accept_seq: 0,
                    more: false,
                })
            }
        }

        fn fetch_mls_control(
            &self,
            _channel_id: u64,
            since_accept_seq: u64,
        ) -> impl Future<Output = Result<MlsControl, TransportError>> + Send {
            let events = self
                .commits
                .lock()
                .unwrap()
                .pop_front()
                .into_iter()
                .collect::<Vec<_>>();
            let next_accept_seq = since_accept_seq + 1;
            async move {
                Ok(MlsControl {
                    events,
                    next_accept_seq,
                    more: false,
                })
            }
        }

        fn fetch_key_packages(
            &self,
            _member: &farder_crypto::identity::PublicKey,
            _device: &str,
        ) -> impl Future<Output = Result<Vec<Vec<u8>>, TransportError>> + Send {
            async move { Ok(Vec::new()) }
        }

        fn fetch_history_v2(
            &self,
            _channel_id: u64,
            _before_id: Option<u64>,
            _limit: u32,
        ) -> impl Future<
            Output = Result<Vec<farder_protocol::server::MessageInfoV2>, TransportError>,
        > + Send {
            async move { Ok(Vec::new()) }
        }
    }

    /// A transport that serves `fetch_mls_control` pages as a function of the
    /// requested cursor, recording every cursor — for the pagination tests.
    struct PagedControlTransport {
        pages: HashMap<u64, MlsControl>,
        requested: Mutex<Vec<u64>>,
    }

    impl PagedControlTransport {
        fn requested(&self) -> Vec<u64> {
            self.requested.lock().unwrap().clone()
        }
    }

    impl E2eeTransport for PagedControlTransport {
        fn submit_event(
            &self,
            event: &Event,
        ) -> impl Future<Output = Result<EventAccepted, TransportError>> + Send {
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
            _channel_id: Option<u64>,
            _since_accept_seq: u64,
        ) -> impl Future<Output = Result<Welcomes, TransportError>> + Send {
            async move {
                Ok(Welcomes {
                    events: Vec::new(),
                    next_accept_seq: 0,
                    more: false,
                })
            }
        }

        fn fetch_mls_control(
            &self,
            _channel_id: u64,
            since_accept_seq: u64,
        ) -> impl Future<Output = Result<MlsControl, TransportError>> + Send {
            self.requested.lock().unwrap().push(since_accept_seq);
            let page = self.pages.get(&since_accept_seq).cloned().unwrap_or(MlsControl {
                events: Vec::new(),
                next_accept_seq: since_accept_seq,
                more: false,
            });
            async move { Ok(page) }
        }

        fn fetch_key_packages(
            &self,
            _member: &farder_crypto::identity::PublicKey,
            _device: &str,
        ) -> impl Future<Output = Result<Vec<Vec<u8>>, TransportError>> + Send {
            async move { Ok(Vec::new()) }
        }

        fn fetch_history_v2(
            &self,
            _channel_id: u64,
            _before_id: Option<u64>,
            _limit: u32,
        ) -> impl Future<
            Output = Result<Vec<farder_protocol::server::MessageInfoV2>, TransportError>,
        > + Send {
            async move { Ok(Vec::new()) }
        }
    }

    /// A signed `MlsCommit` event whose bytes decode but whose commit is never
    /// actually applied (used only to give the pagination loop an event to
    /// carry). The payload shape is real; the MLS bytes are filler.
    fn dummy_control_event(channel_id: u64) -> Vec<u8> {
        let dev = Keypair::generate();
        let id = Keypair::generate();
        crate::chain::build_next_event(
            &dev,
            &id,
            SERVER_ID,
            &ChainState::default(),
            crate::chain::event_now_secs(),
            EP::MlsCommit {
                channel_id,
                generation: 0,
                epoch: 0,
                mls_message: vec![1, 2, 3],
                adds: vec![],
                removes: vec![],
                prev_epoch_authenticator: [0u8; 32],
                post_epoch_authenticator: [0u8; 32],
                post_tree_hash: [0u8; 32],
                authz_head: "0f".repeat(32),
                store_instance_hash: [0u8; 32],
            },
        )
        .to_bytes()
    }

    // ---- resync loop: the three termination assertions ----

    #[tokio::test]
    async fn resync_retries_until_the_transport_accepts() {
        let mut f = two_member(1 << 60);
        let channel_id = 1 << 60;
        let k = key(channel_id);
        let transport = ResyncTransport::new();

        // Alice commits twice, so bob (at epoch 1) falls two epochs behind.
        let u1 = f.alice_group.self_update(&f.alice_store, &DeviceSigner(&f.alice_dev)).unwrap();
        let u2 = f.alice_group.self_update(&f.alice_store, &DeviceSigner(&f.alice_dev)).unwrap();
        transport.serve_commit(commit_event(channel_id, &u1));
        transport.serve_commit(commit_event(channel_id, &u2));
        // Bob's first two sends lose the race; the third is accepted.
        transport.reject_then_accept(2);

        let bob_actor = actor(&f.bob_id, &f.bob_dev);
        let mut bob_chain = mid_chain();
        let ctx = SealContext {
            key: &k,
            generation: 0,
            store: &f.bob_store,
            content: "hello",
            reply_to: None,
        };
        let certs = EmptyCerts;
        let request = ResyncRequest {
            eligibility: &SendEligibility::confirmed(),
            since_accept_seq: 0,
        };

        let outcome = send_sealed_resync(
            &transport,
            &bob_actor,
            &mut bob_chain,
            &ctx,
            &mut f.bob_group,
            &request,
            &certs,
        )
        .await
        .unwrap();

        assert_eq!(outcome.send.epoch, 3, "the retry sealed at the winning epoch");
        assert_eq!(f.bob_group.epoch(), 3, "bob caught up with alice's two commits");
        assert_eq!(transport.submit_calls(), 3, "two stale sends, one accepted");
    }

    #[tokio::test]
    async fn resync_terminates_with_equivocation_when_the_epoch_never_advances() {
        let mut f = two_member((1 << 60) + 2);
        let channel_id = (1 << 60) + 2;
        let k = key(channel_id);
        let transport = ResyncTransport::new();
        // Always reject, and serve no commits: every attempt is unproductive.
        transport.reject_then_accept(usize::MAX);

        let bob_actor = actor(&f.bob_id, &f.bob_dev);
        let mut bob_chain = mid_chain();
        let ctx = SealContext {
            key: &k,
            generation: 0,
            store: &f.bob_store,
            content: "doomed",
            reply_to: None,
        };
        let certs = EmptyCerts;
        let request = ResyncRequest {
            eligibility: &SendEligibility::confirmed(),
            since_accept_seq: 0,
        };

        let err = send_sealed_resync(
            &transport,
            &bob_actor,
            &mut bob_chain,
            &ctx,
            &mut f.bob_group,
            &request,
            &certs,
        )
        .await
        .unwrap_err();

        match err {
            E2eeError::ResyncEquivocation { attempts, last_epoch } => {
                assert_eq!(attempts, MAX_UNPRODUCTIVE_RESYNC_ATTEMPTS);
                assert_eq!(last_epoch, 1, "the epoch never advanced");
            }
            other => panic!("expected ResyncEquivocation, got {other:?}"),
        }
        assert_eq!(f.bob_group.epoch(), 1);
        assert_eq!(transport.submit_calls(), MAX_UNPRODUCTIVE_RESYNC_ATTEMPTS);
    }

    #[tokio::test]
    async fn resync_terminates_even_when_the_epoch_keeps_advancing() {
        let mut f = two_member((1 << 60) + 3);
        let channel_id = (1 << 60) + 3;
        let k = key(channel_id);
        let transport = ResyncTransport::new();
        // Always reject, but serve a fresh winning commit every attempt: the
        // epoch advances productively yet the send still loses every time. The
        // TOTAL bound must still stop the loop.
        transport.reject_then_accept(usize::MAX);
        for _ in 0..MAX_TOTAL_RESYNC_ATTEMPTS {
            let u = f.alice_group.self_update(&f.alice_store, &DeviceSigner(&f.alice_dev)).unwrap();
            transport.serve_commit(commit_event(channel_id, &u));
        }

        let bob_actor = actor(&f.bob_id, &f.bob_dev);
        let mut bob_chain = mid_chain();
        let ctx = SealContext {
            key: &k,
            generation: 0,
            store: &f.bob_store,
            content: "always losing",
            reply_to: None,
        };
        let certs = EmptyCerts;
        let request = ResyncRequest {
            eligibility: &SendEligibility::confirmed(),
            since_accept_seq: 0,
        };

        let err = send_sealed_resync(
            &transport,
            &bob_actor,
            &mut bob_chain,
            &ctx,
            &mut f.bob_group,
            &request,
            &certs,
        )
        .await
        .unwrap_err();

        match err {
            E2eeError::ResyncEquivocation { attempts, last_epoch } => {
                assert_eq!(attempts, MAX_TOTAL_RESYNC_ATTEMPTS);
                assert_eq!(last_epoch, 1 + MAX_TOTAL_RESYNC_ATTEMPTS as u64);
            }
            other => panic!("expected ResyncEquivocation, got {other:?}"),
        }
        assert_eq!(f.bob_group.epoch(), 1 + MAX_TOTAL_RESYNC_ATTEMPTS as u64);
        assert_eq!(transport.submit_calls(), MAX_TOTAL_RESYNC_ATTEMPTS);
    }

    #[tokio::test]
    async fn resync_aborts_on_a_leaf_binding_failure_and_does_not_retry() {
        // F4: a commit whose actual add fails Gate 2 (leaf binding) has already
        // been merged, so the local group is poisoned. The resync loop must
        // abort with ResyncPoisoned and must NOT retry through it.
        let mut f = two_member((1 << 60) + 6);
        let channel_id = (1 << 60) + 6;
        let k = key(channel_id);
        let transport = ResyncTransport::new();
        transport.reject_then_accept(usize::MAX); // the first send is stale

        // Alice adds charlie: a real add whose leaf will fail Gate 2 against an
        // empty resolver (no log-valid cert), standing in for an impostor leaf.
        let charlie_id = Keypair::generate();
        let charlie_dev = Keypair::generate();
        let charlie_bundle =
            generate_key_package(&f.alice_store, &charlie_dev, &charlie_id.public_key()).unwrap();
        let charlie_kp_bytes = charlie_bundle.key_package().tls_serialize_detached().unwrap();
        let charlie_kp = decode_key_package(&f.alice_store, &charlie_kp_bytes).unwrap();
        let add = f
            .alice_group
            .add_members(&f.alice_store, &DeviceSigner(&f.alice_dev), &[charlie_kp])
            .unwrap();

        // Wrap alice's add into a signed MlsCommit event carrying the declared
        // add, so Gate 1 passes and Gate 2 is what fires.
        let dev = Keypair::generate();
        let id = Keypair::generate();
        let commit = crate::chain::build_next_event(
            &dev,
            &id,
            SERVER_ID,
            &ChainState::default(),
            crate::chain::event_now_secs(),
            EP::MlsCommit {
                channel_id,
                generation: 0,
                epoch: add.epoch,
                mls_message: add.commit_bytes.clone(),
                adds: vec![DeclaredAdd {
                    identity: charlie_id.public_key(),
                    device: device_id(&charlie_dev.public_key()),
                    key_package: "0f".repeat(32),
                }],
                removes: vec![],
                prev_epoch_authenticator: add.prev_epoch_authenticator,
                post_epoch_authenticator: [0u8; 32],
                post_tree_hash: add.post_tree_hash,
                authz_head: "0f".repeat(32),
                store_instance_hash: [0u8; 32],
            },
        )
        .to_bytes();
        transport.serve_commit(commit);

        let bob_actor = actor(&f.bob_id, &f.bob_dev);
        let mut bob_chain = mid_chain();
        let ctx = SealContext {
            key: &k,
            generation: 0,
            store: &f.bob_store,
            content: "into a poisoned group",
            reply_to: None,
        };
        let certs = EmptyCerts;
        let request = ResyncRequest {
            eligibility: &SendEligibility::confirmed(),
            since_accept_seq: 0,
        };

        let err = send_sealed_resync(
            &transport,
            &bob_actor,
            &mut bob_chain,
            &ctx,
            &mut f.bob_group,
            &request,
            &certs,
        )
        .await
        .unwrap_err();

        match err {
            E2eeError::ResyncPoisoned { member, reason } => {
                assert_eq!(member.identity, charlie_id.public_key());
                assert_eq!(member.device, device_id(&charlie_dev.public_key()));
                assert!(!reason.is_empty());
            }
            other => panic!("expected ResyncPoisoned, got {other:?}"),
        }
        // The abort is terminal: exactly ONE send happened (the initial stale
        // one). Nothing was retried through the poisoned group.
        assert_eq!(transport.submit_calls(), 1);
    }

    // ---- fetch_mls_control_exhaustive: pagination + the stall guard ----

    #[tokio::test]
    async fn fetch_mls_control_exhaustive_paginates_to_reach_a_commit_behind_many_rows() {
        let channel_id = (1 << 60) + 4;
        let target = dummy_control_event(channel_id);

        // The target sits behind two full empty pages (rows for OTHER channels).
        let mut pages = HashMap::new();
        pages.insert(
            0,
            MlsControl {
                events: Vec::new(),
                next_accept_seq: 500,
                more: true,
            },
        );
        pages.insert(
            500,
            MlsControl {
                events: Vec::new(),
                next_accept_seq: 1000,
                more: true,
            },
        );
        pages.insert(
            1000,
            MlsControl {
                events: vec![target],
                next_accept_seq: 1001,
                more: false,
            },
        );
        let transport = PagedControlTransport {
            pages,
            requested: Mutex::new(Vec::new()),
        };

        let (events, cursor) = fetch_mls_control_exhaustive(&transport, channel_id, 0)
            .await
            .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(cursor, 1001);
        // The cursor advanced monotonically and never restarted from 0.
        assert_eq!(transport.requested(), vec![0, 500, 1000]);
    }

    #[tokio::test]
    async fn fetch_mls_control_exhaustive_errors_on_a_more_page_that_does_not_advance() {
        let mut pages = HashMap::new();
        pages.insert(
            0,
            MlsControl {
                events: Vec::new(),
                next_accept_seq: 0, // same cursor: would loop forever
                more: true,
            },
        );
        let transport = PagedControlTransport {
            pages,
            requested: Mutex::new(Vec::new()),
        };

        let err = fetch_mls_control_exhaustive(&transport, (1 << 60) + 5, 0)
            .await
            .unwrap_err();

        assert!(matches!(err, E2eeError::Transport(_)), "got {err:?}");
        assert_eq!(transport.requested(), vec![0], "it bailed, not spun");
    }
}
