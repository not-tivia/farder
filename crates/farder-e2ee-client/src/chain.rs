//! Event-chain state and event construction.
//!
//! The vertical builds and signs every event exactly the way the Tauri client
//! does in `client/src-tauri/src/commands.rs` (`event_build_next`): an
//! [`EventCore`] with `server_id` = the log server id (genesis hash), `author`
//! = the identity public key, `device` = `device_id(device_pubkey)`, and
//! `(seq, prev, lamport)` taken from per-(server, device) chain state, then
//! [`Event::sign`] with the device subkey.
//!
//! This crate owns **no storage** for that state: [`ChainState`] is a plain
//! struct the caller supplies and persists. The Tauri layer owns the real
//! `device_state.json` file (sub-project 4b); the harness owns its own copy
//! (sub-project 4a). This module never reads or writes `device_state.json`.

use farder_crypto::event_log::{device_id, Event, EventCore, EventHash, EventPayload};
use farder_crypto::identity::Keypair;

/// The "who" of the vertical: the acting device subkey, its owning identity,
/// and the log server it acts on. Bundles the three inputs every event build
/// needs, so the lifecycle functions stay under the 7-argument clippy bound
/// and the harness constructs one per identity.
pub struct Actor<'a> {
    /// The device subkey: signs events and doubles as the MLS leaf key.
    pub device: &'a Keypair,
    /// The owning identity: `EventCore.author` and the MLS credential identity.
    pub identity: &'a Keypair,
    /// The log server id (genesis hash): `EventCore.server_id`.
    pub log_server_id: &'a str,
}

/// Per-(server, device) event-chain state — the same three fields the Tauri
/// client persists in `device_state.json` (`next_seq`, `last_event_hash`,
/// `lamport`), kept as a plain struct so any caller can round-trip it through
/// any store.
///
/// `next_seq` is the `core.seq` the next event will carry; `last_event_hash`
/// is `prev` for the next event (the chain head, `None` for the first event);
/// `lamport` is the highest lamport this device has observed, so the next
/// event carries `lamport + 1`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChainState {
    pub next_seq: u64,
    pub last_event_hash: Option<EventHash>,
    pub lamport: u64,
}

impl ChainState {
    /// Advance this chain state past `event` after the server accepted it.
    ///
    /// Mirrors the Tauri client's post-accept update
    /// (`ds.next_seq = event.core.seq + 1; ds.last_event_hash = Some(event.hash());
    /// ds.lamport = event.core.lamport`). Call only after a successful submit.
    pub fn advance(&mut self, event: &Event) {
        self.next_seq = event.core.seq + 1;
        self.last_event_hash = Some(event.hash());
        self.lamport = event.core.lamport;
    }
}

/// Build and sign the next event in this device's chain. Pure: fully
/// determined by the inputs, no I/O.
pub fn build_next_event(
    device: &Keypair,
    identity: &Keypair,
    server_id: &str,
    chain: &ChainState,
    timestamp: u64,
    payload: EventPayload,
) -> Event {
    let core = EventCore {
        server_id: server_id.to_string(),
        author: identity.public_key(),
        device: device_id(&device.public_key()),
        seq: chain.next_seq,
        prev: chain.last_event_hash.clone(),
        lamport: chain.lamport + 1,
        timestamp,
        payload,
    };
    Event::sign(core, device)
}

/// Adopt `head` (the server's claimed newest event of this device's chain) as the
/// local chain head, but ONLY if it is genuinely ours and ahead of us. The server's
/// word is never trusted: the signature is verified against THIS device's key, and
/// the author, device and log server must all be ours. Returns whether it was adopted.
pub fn adopt_own_chain_head(actor: &Actor<'_>, chain: &mut ChainState, head: &Event) -> bool {
    let ours = head.core.author == actor.identity.public_key()
        && head.core.device == device_id(&actor.device.public_key())
        && head.core.server_id == actor.log_server_id
        && head.verify(&actor.device.public_key()).is_ok();
    // `next_seq` is one past our local head, so "ahead" means seq >= next_seq.
    if !ours || head.core.seq < chain.next_seq {
        return false;
    }
    chain.advance(head);
    true
}

/// Build, sign and submit the next event of this device's chain; if the server
/// refuses it for a chain reason (our local head is behind the server's, e.g. a
/// lost acceptance), fetch our own head, adopt it via [`adopt_own_chain_head`],
/// rebuild + re-sign on it, and submit ONCE more. Never loops; if the head is
/// missing, forged or not ahead, the ORIGINAL error is returned. On success the
/// chain is advanced past the accepted event. When the chain was adopted, the
/// caller must persist `chain` even if the retry fails (it is still correct).
pub async fn submit_with_chain_recovery<T: crate::transport::E2eeTransport>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    timestamp: u64,
    payload: EventPayload,
) -> Result<(Event, crate::transport::EventAccepted), crate::transport::TransportError> {
    submit_with_chain_recovery_guarded(transport, actor, chain, timestamp, payload, |_| true).await
}

/// [`submit_with_chain_recovery`] with a guard on the adopted head. After a head is
/// adopted, `may_resubmit(&head)` decides whether the payload may be submitted again.
/// Own MLS commits pass a guard that refuses when `head` IS this same commit
/// ([`is_own_commit_for`]): the staged commit was then in fact accepted, so it is never
/// resubmitted. When the guard refuses, the chain stays adopted (the caller must persist
/// it) and a NON-rejection transport error ("no answer") is returned, so the caller
/// leaves the staged commit in place for `settle_staged_commit` to resolve.
pub async fn submit_with_chain_recovery_guarded<T: crate::transport::E2eeTransport>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    timestamp: u64,
    payload: EventPayload,
    may_resubmit: impl Fn(&Event) -> bool,
) -> Result<(Event, crate::transport::EventAccepted), crate::transport::TransportError> {
    let event = build_next_event(actor.device, actor.identity, actor.log_server_id, chain, timestamp, payload.clone());
    let first_err = match transport.submit_event(&event).await {
        Ok(accepted) => {
            chain.advance(&event);
            return Ok((event, accepted));
        }
        Err(e) => e,
    };
    if !first_err.is_chain_refused() {
        return Err(first_err);
    }
    let device = device_id(&actor.device.public_key());
    let Ok(Some(head)) = transport.fetch_own_chain_head(&device).await else {
        return Err(first_err);
    };
    if !adopt_own_chain_head(actor, chain, &head) {
        return Err(first_err);
    }
    if !may_resubmit(&head) {
        return Err(crate::transport::TransportError::transport(
            "chain refused, and the adopted head is this same commit: it was already accepted; settle the staged commit instead of resubmitting",
        ));
    }
    let retry = build_next_event(actor.device, actor.identity, actor.log_server_id, chain, timestamp, payload);
    let accepted = transport.submit_event(&retry).await?;
    chain.advance(&retry);
    Ok((retry, accepted))
}

/// [`submit_with_chain_recovery_guarded`] for an own `MlsCommit` (rekey, add, discharge,
/// reset bootstrap). Never submits the commit twice: if the adopted head is this same
/// commit it is not resubmitted, and a chain refusal that recovery could not resolve is
/// reported as a no-answer transport error rather than a rejection — in both cases the
/// caller keeps the commit STAGED for `settle_staged_commit` (a rejection would make it
/// discard a commit the log may in fact hold). Callers must persist `chain` afterwards.
pub async fn submit_commit_with_chain_recovery<T: crate::transport::E2eeTransport>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    timestamp: u64,
    payload: EventPayload,
) -> Result<(Event, crate::transport::EventAccepted), crate::transport::TransportError> {
    let probe = payload.clone();
    submit_with_chain_recovery_guarded(transport, actor, chain, timestamp, payload, |head| {
        !is_own_commit_for(head, &probe)
    })
    .await
    .map_err(|e| {
        if e.is_chain_refused() {
            crate::transport::TransportError::transport(format!(
                "chain refused and not recovered ({}); the commit stays staged for settle",
                e.rejection_reason()
            ))
        } else {
            e
        }
    })
}

/// True iff `head` is an `MlsCommit` for the same channel, generation and epoch as
/// `payload` (itself an `MlsCommit`) — i.e. our own earlier submit of this commit.
pub fn is_own_commit_for(head: &Event, payload: &EventPayload) -> bool {
    match (&head.core.payload, payload) {
        (
            EventPayload::MlsCommit { channel_id: c1, generation: g1, epoch: e1, .. },
            EventPayload::MlsCommit { channel_id: c2, generation: g2, epoch: e2, .. },
        ) => c1 == c2 && g1 == g2 && e1 == e2,
        _ => false,
    }
}

/// The device wall-clock now, in unix seconds — the untrusted `core.timestamp`
/// claim every event carries (ingest bounds it to 300 s ahead of server time).
pub fn event_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::event_log::EventPayload;

    pub(super) fn msg_payload(n: u64) -> EventPayload {
        EventPayload::MessagePosted {
            channel_id: 1,
            content: format!("m{n}"),
            reply_to: None,
            attachments: vec![],
        }
    }

    #[test]
    fn build_next_event_chains_signs_and_verifies() {
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let mut chain = ChainState::default();

        let e0 = build_next_event(
            &device,
            &identity,
            "server-hex",
            &chain,
            100,
            msg_payload(0),
        );
        assert_eq!(e0.core.seq, 0);
        assert_eq!(e0.core.prev, None);
        assert_eq!(e0.core.lamport, 1);
        assert_eq!(e0.core.server_id, "server-hex");
        assert_eq!(e0.core.author, identity.public_key());
        assert_eq!(e0.core.device, device_id(&device.public_key()));
        assert!(e0.verify(&device.public_key()).is_ok());

        chain.advance(&e0);
        assert_eq!(chain.next_seq, 1);
        assert_eq!(chain.last_event_hash.as_deref(), Some(e0.hash().as_str()));
        assert_eq!(chain.lamport, 1);

        let e1 = build_next_event(
            &device,
            &identity,
            "server-hex",
            &chain,
            101,
            msg_payload(1),
        );
        assert_eq!(e1.core.seq, 1);
        assert_eq!(e1.core.prev.as_deref(), Some(e0.hash().as_str()));
        assert_eq!(e1.core.lamport, 2);
        assert!(e1.verify(&device.public_key()).is_ok());
        assert_ne!(e1.hash(), e0.hash());
    }

    #[test]
    fn a_second_device_runs_an_independent_chain_under_the_same_identity() {
        let identity = Keypair::generate();
        let dev_a = Keypair::generate();
        let dev_b = Keypair::generate();
        let chain = ChainState::default();

        let a = build_next_event(&dev_a, &identity, "srv", &chain, 1, msg_payload(0));
        let b = build_next_event(&dev_b, &identity, "srv", &chain, 1, msg_payload(0));
        assert_eq!(a.core.author, b.core.author);
        assert_ne!(a.core.device, b.core.device);
        assert_ne!(a.hash(), b.hash());
    }

    // ---- recovery from a lost answer, across the operation kinds (issue #16) ----

    const SERVER_ID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
    const CHAIN_REFUSAL: &str = "non-contiguous seq (gap or fork)";

    /// A genuine event of this device's chain at the chain's current head, as if an
    /// earlier submit whose answer was lost HAD been accepted. It is never submitted
    /// to the fake; it is only served back as the device's own head.
    fn lost_event(identity: &Keypair, device: &Keypair, chain: &ChainState) -> Event {
        build_next_event(device, identity, SERVER_ID, chain, 5, msg_payload(0))
    }

    fn stale_actor<'a>(identity: &'a Keypair, device: &'a Keypair) -> Actor<'a> {
        Actor { device, identity, log_server_id: SERVER_ID }
    }

    #[tokio::test]
    async fn channel_create_recovers_from_a_chain_refusal_via_the_served_own_head() {
        use crate::channel::{create_e2ee_channel, ChannelSpec};
        let transport = crate::testing::FakeTransport::new();
        let (identity, device) = (Keypair::generate(), Keypair::generate());
        let a = stale_actor(&identity, &device);
        let mut chain = ChainState::default();
        transport.serve_own_head(lost_event(&identity, &device, &chain));
        transport.reject_next(CHAIN_REFUSAL);
        let key = crate::channel_key::ChannelKey::new(SERVER_ID.to_string(), farder_crypto::event_log::E2EE_CHANNEL_ID_FLOOR + 31).unwrap();
        let spec = ChannelSpec { key, name: "v".into(), kind: "text".into(), parent: None };
        let dir = std::env::temp_dir().join(format!("farder-chain-recover-create-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        create_e2ee_channel(&transport, &a, &mut chain, &spec, &dir).await.expect("recovers");

        let sent = transport.submitted();
        assert_eq!(sent.len(), 2, "one refused attempt, one retry");
        assert_eq!(sent[0].core.seq, 0);
        assert_eq!(sent[1].core.seq, 1, "retried on the adopted head");
        assert_eq!(chain.next_seq, 2);
    }

    #[tokio::test]
    async fn channel_create_with_a_forged_head_does_not_recover() {
        use crate::channel::{create_e2ee_channel, ChannelSpec};
        let transport = crate::testing::FakeTransport::new();
        let (identity, device) = (Keypair::generate(), Keypair::generate());
        let a = stale_actor(&identity, &device);
        let mut chain = ChainState::default();
        // Signed by a different key than our device.
        let forged = lost_event(&identity, &Keypair::generate(), &chain);
        transport.serve_own_head(forged);
        transport.reject_next(CHAIN_REFUSAL);
        let key = crate::channel_key::ChannelKey::new(SERVER_ID.to_string(), farder_crypto::event_log::E2EE_CHANNEL_ID_FLOOR + 32).unwrap();
        let spec = ChannelSpec { key, name: "v".into(), kind: "text".into(), parent: None };
        let dir = std::env::temp_dir().join(format!("farder-chain-forged-create-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let err = create_e2ee_channel(&transport, &a, &mut chain, &spec, &dir).await;

        assert!(err.is_err());
        assert_eq!(transport.submit_count(), 1, "no retry on a forged head");
        assert_eq!(chain, ChainState::default(), "nothing adopted");
    }

    #[tokio::test]
    async fn key_package_publish_recovers_and_rejects_a_forged_head() {
        use crate::channel::{create_e2ee_channel, publish_key_package, ChannelSpec};
        for forged in [false, true] {
            let transport = crate::testing::FakeTransport::new();
            let (identity, device) = (Keypair::generate(), Keypair::generate());
            let a = stale_actor(&identity, &device);
            let mut chain = ChainState::default();
            let key = crate::channel_key::ChannelKey::new(SERVER_ID.to_string(), farder_crypto::event_log::E2EE_CHANNEL_ID_FLOOR + 33 + forged as u64).unwrap();
            let spec = ChannelSpec { key, name: "v".into(), kind: "text".into(), parent: None };
            let dir = std::env::temp_dir().join(format!("farder-chain-kp-{forged}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let created = create_e2ee_channel(&transport, &a, &mut chain, &spec, &dir).await.unwrap();
            let other = Keypair::generate();
            let signer = if forged { &other } else { &device };
            transport.serve_own_head(lost_event(&identity, signer, &chain));
            transport.reject_next(CHAIN_REFUSAL);
            let before = transport.submit_count();

            let res = publish_key_package(&transport, &a, &mut chain, &created.store, &created.store_instance_hash).await;

            if forged {
                assert!(res.is_err());
                assert_eq!(transport.submit_count(), before + 1);
            } else {
                res.expect("recovers");
                assert_eq!(transport.submit_count(), before + 2);
                assert_eq!(chain.next_seq, 3);
            }
        }
    }
}
