//! C4 of the 5a lifecycle: emit the `DeviceRevoked` log event.
//!
//! [`revoke_device`] submits a `DeviceRevoked { device }` event — the fold's
//! "kill this device" primitive (authority-note fact 2,
//! `event_log_state.rs:996-1010`). On acceptance the device's cert is dead and
//! its chain frozen (any further event from it is rejected), and its MLS leaf
//! becomes drift lazily via `pending_removals`, which C3's
//! [`crate::drift::discharge_drift`] later discharges.
//!
//! This module emits the EVENT only. The fold decides whether it is authorized
//! by the author's role; the client distinguishes the two call shapes only in
//! how it chooses `actor` and the target device id (see [`revoke_device`]).

use farder_crypto::event_log::EventPayload;

use crate::chain::{build_next_event, event_now_secs, Actor, ChainState};
use crate::channel::E2eeError;
use crate::transport::E2eeTransport;

/// The result of a successful [`revoke_device`].
///
/// A `DeviceRevoked` merges no MLS state locally, so unlike the own-commit
/// outcomes (`CommitSubmitted` / `RekeyOutcome` / `DriftDischargeOutcome`) there
/// is no `local_epoch` and no divergence caveat: acceptance is simply the
/// server recording the event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokeOutcome {
    /// Server-assigned hash of the accepted `DeviceRevoked` event.
    pub event_hash: String,
}

/// Emit a `DeviceRevoked { device }` event and advance the chain on acceptance,
/// following the crate's submit-and-advance-on-accept pattern (see
/// [`crate::channel::publish_key_package`]).
///
/// The payload names the **victim** device — `device_id` is its hex SHA-256 id
/// (compute it with `farder_crypto::event_log::device_id(&device_pubkey)`).
/// `core.device` is the **authoring** device (`actor.device`) and `core.author`
/// is `actor.identity`, exactly like every other event this crate builds.
///
/// # The two call shapes (the fold decides, not this fn)
///
/// The fold's `DeviceRevoked` authz (`event_log_state.rs:996-1010`) accepts an
/// event whose `author` is EITHER the victim device's owning identity OR the
/// server owner. This fn emits the identical payload either way; the caller
/// chooses the shape by which [`Actor`] it passes and which device id it names:
///
/// - **Self-revoke** — `actor.identity` owns the target device and `device_id`
///   names one of that identity's own devices. Any of the identity's devices
///   may author it, *including the revoked device itself* (self-revoke). This
///   is the form store re-provisioning (C7) calls when an on-disk MLS store is
///   terminal.
/// - **Owner-revoke** — `actor.identity` is the **server owner** and `device_id`
///   names a *member's* device. The fold's `is_owner(author)` arm authorizes it.
///
/// # Rejections
///
/// A `DeviceRevoked` merges no MLS state locally, so there is no divergence
/// contract and no `stale-epoch` handling: any rejection surfaces as
/// [`E2eeError::Transport`] with the server's reason preserved verbatim via
/// [`crate::transport::TransportError::rejection_reason`] — notably
/// `"device already revoked"`, `"revocation cites an unknown device"`, and
/// `"only the owning identity or the server owner may revoke a device"`. It is
/// never silently swallowed and never reported as success.
pub async fn revoke_device<T: E2eeTransport + Sync>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    device_id: String,
) -> Result<RevokeOutcome, E2eeError> {
    let event = build_next_event(
        actor.device,
        actor.identity,
        actor.log_server_id,
        chain,
        event_now_secs(),
        EventPayload::DeviceRevoked { device: device_id },
    );

    // Submit; the fold authorizes (or rejects) by the author's role. Advance
    // only on accept.
    let accepted = transport.submit_event(&event).await?;
    chain.advance(&event);

    Ok(RevokeOutcome {
        event_hash: accepted.event_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::event_log::{device_id, EventPayload};
    use farder_crypto::identity::Keypair;

    use crate::chain::Actor;
    use crate::testing::FakeTransport;

    const SERVER_ID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    fn actor<'a>(identity: &'a Keypair, device: &'a Keypair) -> Actor<'a> {
        Actor {
            device,
            identity,
            log_server_id: SERVER_ID,
        }
    }

    #[tokio::test]
    async fn self_revoke_emits_device_revoked_authored_by_the_identity_and_advances_on_accept() {
        let transport = FakeTransport::new();
        let identity = Keypair::generate();
        let authoring_device = Keypair::generate();
        let lost_device = Keypair::generate();
        let a = actor(&identity, &authoring_device);
        let mut chain = ChainState::default();
        let target = device_id(&lost_device.public_key());

        let outcome = revoke_device(&transport, &a, &mut chain, target.clone())
            .await
            .unwrap();

        // Exactly one event, and it is the DeviceRevoked naming the target.
        assert_eq!(transport.submit_count(), 1);
        let last = transport.submitted().into_iter().last().expect("one event");
        match &last.core.payload {
            EventPayload::DeviceRevoked { device } => assert_eq!(device, &target),
            other => panic!("expected DeviceRevoked, got {other:?}"),
        }
        // Authored by the identity, signed by the authoring device.
        assert_eq!(last.core.author, identity.public_key());
        assert_eq!(last.core.device, device_id(&authoring_device.public_key()));

        // The chain advanced past the accepted event.
        assert_eq!(chain.next_seq, 1);
        assert_eq!(chain.last_event_hash.as_deref(), Some(outcome.event_hash.as_str()));
        assert_eq!(outcome.event_hash, last.hash());
    }

    #[tokio::test]
    async fn the_emitted_event_is_signed_by_the_device_key_and_verifies() {
        // Self-revoke edge: the authoring device may even be the one being
        // revoked (authority-note fact 2).
        let transport = FakeTransport::new();
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let a = actor(&identity, &device);
        let mut chain = ChainState::default();
        let target = device_id(&device.public_key()); // revoke itself

        revoke_device(&transport, &a, &mut chain, target.clone())
            .await
            .unwrap();

        let last = transport.submitted().into_iter().last().expect("one event");
        assert!(last.verify(&device.public_key()).is_ok());
        match &last.core.payload {
            EventPayload::DeviceRevoked { device } => assert_eq!(device, &target),
            other => panic!("expected DeviceRevoked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn owner_revoke_emits_the_same_payload_with_the_owner_as_author() {
        // The owner-revoke shape: identical payload, but `actor.identity` is the
        // server owner and the target is a member's device — the fold's
        // `is_owner(author)` arm authorizes it. The fn itself emits whatever
        // author the caller passes.
        let transport = FakeTransport::new();
        let owner = Keypair::generate();
        let owner_device = Keypair::generate();
        let member_device = Keypair::generate();
        let a = actor(&owner, &owner_device);
        let mut chain = ChainState::default();
        let target = device_id(&member_device.public_key());

        let outcome = revoke_device(&transport, &a, &mut chain, target.clone())
            .await
            .unwrap();

        let last = transport.submitted().into_iter().last().expect("one event");
        match &last.core.payload {
            EventPayload::DeviceRevoked { device } => assert_eq!(device, &target),
            other => panic!("expected DeviceRevoked, got {other:?}"),
        }
        assert_eq!(last.core.author, owner.public_key());
        assert_eq!(last.core.device, device_id(&owner_device.public_key()));
        assert_eq!(outcome.event_hash, last.hash());
    }

    #[tokio::test]
    async fn a_rejection_surfaces_the_server_reason_not_silently_swallowed() {
        let transport = FakeTransport::new();
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let a = actor(&identity, &device);
        let mut chain = ChainState::default();

        transport.reject_next("event rejected: device already revoked");

        let err = revoke_device(&transport, &a, &mut chain, device_id(&device.public_key()))
            .await
            .unwrap_err();

        // The reason is preserved verbatim, reachable without string-sniffing
        // the Display impl.
        match err {
            E2eeError::Transport(t) => {
                assert_eq!(
                    t.rejection_reason(),
                    "event rejected: device already revoked"
                );
            }
            other => panic!("expected Transport error, got {other:?}"),
        }
        // Acceptance advances the chain; a rejection does not.
        assert_eq!(chain.next_seq, 0);
        assert_eq!(chain.last_event_hash, None);
    }
}
