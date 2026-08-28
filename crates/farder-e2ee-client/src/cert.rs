//! The production source of Gate 2's trust anchor.
//!
//! [`process_incoming_commit`](crate::commit::process_incoming_commit) resolves a
//! [`DeviceCert`] through the [`DeviceCertResolver`](crate::commit::DeviceCertResolver)
//! trait and runs `credential::verify_leaf_binding` against it. This module is
//! the *real* implementation of that trait — the one that fetches the identity's
//! `DeviceAuthorized` events from the log over the transport and verifies each
//! cert before serving it. It exists so a future implementer can never be
//! tempted to hand Gate 2 a cert taken from the commit under validation, which
//! would defeat the impostor-leaf check entirely (sub-project 1's carry-forward
//! M1: the device key must come from a verified `DeviceCert` bound to
//! `(author, device)`, **never from the event itself**).
//!
//! # What is verified, and what is deliberately NOT
//!
//! The resolver checks exactly five things about a cert before returning it:
//!
//! 1. **Source** — the cert was fetched from the log (via
//!    [`E2eeTransport::fetch_device_certs`]), never synthesized from the commit
//!    being validated.
//! 2. **Binding** — `cert.core.identity` equals the requested member AND
//!    `cert.core.device_id` equals the requested device.
//! 3. **Signature** — [`DeviceCert::verify`] passes: the embedded `device_id`
//!    matches `device_pubkey`, and the identity key signed the cert core.
//! 4. **Revocation** — the device is not named by any `DeviceRevoked` event in
//!    the fetched stream (sub-5 C1).
//! 5. **Expiry** — the winning cert's `DeviceCertCore.expires_at`, if present,
//!    has not yet passed the client's local clock (sub-5 C1).
//!
//! Revocation and expiry are *fold state* in a `LogState`, and this crate has no
//! local `LogState`; the resolver reconstructs the two liveness bits it needs
//! from the fetched stream instead of keeping a full fold. A cert that verifies
//! here is therefore a **cryptographic binding** that is also currently
//! un-revoked and un-expired as far as the resolver can tell — but the client's
//! local clock is the expiry authority (see [`resolve_device_cert`]), so do not
//! read "resolved" as "the server agrees it is live".
//!
//! Sub-5 (S1) widened the server's `FetchDeviceCerts` stream to mix the
//! identity's `DeviceRevoked` events in alongside its `DeviceAuthorized` ones.
//! A non-`DeviceAuthorized` payload (told apart by decoding the event's payload
//! enum) is never a cert source.

use std::collections::{HashMap, HashSet};

use farder_crypto::event_log::{DeviceCert, Event, EventPayload};
use farder_crypto::identity::PublicKey;
use farder_mls::group::DeclaredMember;

use crate::chain::event_now_secs;
use crate::channel::E2eeError;
use crate::commit::DeviceCertResolver;
use crate::transport::E2eeTransport;

/// A [`DeviceCertResolver`] backed by certs that have ALREADY been fetched from
/// the log and verified by [`resolve_device_cert`].
///
/// Build it with [`build_cert_resolver`] before calling
/// [`process_incoming_commit`](crate::commit::process_incoming_commit), which is
/// sync. The map is keyed exactly like the trait's lookup: `(identity, device)`.
pub struct VerifiedCertResolver {
    certs: HashMap<(PublicKey, String), DeviceCert>,
}

impl DeviceCertResolver for VerifiedCertResolver {
    fn device_cert(&self, identity: &PublicKey, device: &str) -> Option<DeviceCert> {
        self.certs.get(&(identity.clone(), device.to_string())).cloned()
    }
}

/// Fetch and verify the [`DeviceCert`] for one `(identity, device)`.
///
/// This is the production counterpart to the test doubles: it asks the transport
/// for the identity's device-lifecycle events (`DeviceAuthorized` +
/// `DeviceRevoked` since sub-5 S1), decodes each one, and returns a cert only if
/// every check in the module doc passes. A non-matching device, a
/// non-`DeviceAuthorized` payload (e.g. `DeviceRevoked`), or a cert whose
/// signature does not verify all fail **closed**: a non-matching/foreign event is
/// skipped, a tampered cert is skipped, and if nothing survives the result is
/// `None` — which
/// [`process_incoming_commit`](crate::commit::process_incoming_commit) turns into
/// [`LeafBindingFailure`](crate::commit::IncomingCommitOutcome::LeafBindingFailure).
///
/// The resolver is revocation- and expiry-aware (sub-5 C1): a device named by
/// any `DeviceRevoked` in the stream is never returned, and the winning cert is
/// rejected if its `expires_at` has already passed. The newest matching,
/// verifying, un-revoked cert wins (events arrive oldest-first).
pub async fn resolve_device_cert<T: E2eeTransport + Sync>(
    transport: &T,
    identity: &PublicKey,
    device: &str,
) -> Result<Option<DeviceCert>, E2eeError> {
    let events = transport.fetch_device_certs(identity).await?;

    // First pass: collect every device this identity has revoked. `DeviceRevoked
    // { device }` names the REVOKED device in its payload, NOT the event's
    // authoring `core.device` (which is the revoker — the owner, or another of
    // the identity's devices). A device that appears here must never have its
    // cert returned, regardless of where the revocation sits in the
    // (oldest-first) stream. The events are trusted as log-accepted: the server
    // only serves `DeviceRevoked` rows that target one of this identity's
    // authorized devices.
    let mut revoked: HashSet<String> = HashSet::new();
    for bytes in &events {
        let event = Event::from_bytes(bytes)
            .map_err(|e| E2eeError::Mls(anyhow::anyhow!("decode device-lifecycle event: {e}")))?;
        if let EventPayload::DeviceRevoked { device } = &event.core.payload {
            revoked.insert(device.clone());
        }
    }

    let mut found: Option<DeviceCert> = None;
    for bytes in events {
        let event = Event::from_bytes(&bytes)
            .map_err(|e| E2eeError::Mls(anyhow::anyhow!("decode device-lifecycle event: {e}")))?;
        // Only `DeviceAuthorized` carries a cert; a `DeviceRevoked` (or any
        // other) payload is not a cert source. Revocations are folded through
        // the `revoked` set collected above, not here.
        let EventPayload::DeviceAuthorized { cert } = &event.core.payload else {
            continue;
        };
        // Only a cert that names exactly this identity and exactly this device,
        // and that verifies under the identity key, is eligible. Anything else is
        // skipped: an identity may hold many devices, and a tampered cert must
        // never be handed out.
        if &cert.core.identity != identity || cert.core.device_id.as_str() != device {
            continue;
        }
        // Revocation-aware: a cert for a device named in any `DeviceRevoked`
        // event is dead on the server; never hand it to Gate 2, even if it is
        // the newest verifying cert in the stream.
        if revoked.contains(&cert.core.device_id) {
            continue;
        }
        if cert.verify().is_err() {
            continue;
        }
        found = Some(cert.clone());
    }

    // Expiry-aware: check the WINNING (newest verifying) cert, not any older
    // fallback — a device re-authorized with an expiring cert that has since
    // lapsed is dead, even if an older non-expiring cert for it survives in the
    // log. The clock is the CLIENT'S local time (`event_now_secs`), not the
    // device's own untrusted `core.timestamp`: a revoked/expired device's clock
    // cannot be trusted, and cert expiry is a wall-clock property. Local time is
    // the conservative bound — a clock-skewed client may reject a cert
    // marginally early (fail-closed), but will never serve an expired one.
    if let Some(cert) = &found {
        if let Some(expires_at) = cert.core.expires_at {
            if expires_at < event_now_secs() {
                return Ok(None);
            }
        }
    }

    Ok(found)
}

/// Build a sync [`DeviceCertResolver`] for the members a commit declares, by
/// resolving each one's cert from the log over the transport.
///
/// `members` is normally `declared.adds` (as `DeclaredMember`s). A member with
/// no verifying cert is simply absent from the map, so Gate 2 fails closed for
/// it rather than erroring the whole batch.
pub async fn build_cert_resolver<T: E2eeTransport + Sync>(
    transport: &T,
    members: &[DeclaredMember],
) -> Result<VerifiedCertResolver, E2eeError> {
    let mut certs = HashMap::new();
    for member in members {
        if let Some(cert) = resolve_device_cert(transport, &member.identity, &member.device).await? {
            certs.insert((member.identity.clone(), member.device.clone()), cert);
        }
    }
    Ok(VerifiedCertResolver { certs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::event_log::device_id;
    use farder_crypto::identity::Keypair;
    use std::sync::Mutex;

    use crate::transport::{EventAccepted, MlsControl, TransportError, Welcomes};
    use std::future::Future;

    /// A transport that serves one identity's `DeviceAuthorized` events and
    /// nothing else — enough to drive `resolve_device_cert` without the real
    /// server.
    struct CertTransport {
        events: Mutex<Vec<Vec<u8>>>,
    }

    impl CertTransport {
        fn new(events: Vec<Vec<u8>>) -> Self {
            Self { events: Mutex::new(events) }
        }
    }

    impl E2eeTransport for CertTransport {
        fn submit_event(
            &self,
            event: &farder_crypto::event_log::Event,
        ) -> impl Future<Output = Result<EventAccepted, TransportError>> + Send {
            let event = event.clone();
            async move {
                Ok(EventAccepted { event_hash: event.hash(), timestamp: event.core.timestamp })
            }
        }

        fn fetch_welcomes(
            &self,
            _channel_id: Option<u64>,
            _since_accept_seq: u64,
        ) -> impl Future<Output = Result<Welcomes, TransportError>> + Send {
            async move { Ok(Welcomes { events: vec![], next_accept_seq: 0, more: false }) }
        }

        fn fetch_mls_control(
            &self,
            _channel_id: u64,
            _since_accept_seq: u64,
        ) -> impl Future<Output = Result<MlsControl, TransportError>> + Send {
            async move { Ok(MlsControl { events: vec![], next_accept_seq: 0, more: false }) }
        }

        fn fetch_key_packages(
            &self,
            _member: &PublicKey,
            _device: &str,
        ) -> impl Future<Output = Result<Vec<Vec<u8>>, TransportError>> + Send {
            async move { Ok(vec![]) }
        }

        fn fetch_device_certs(
            &self,
            _identity: &PublicKey,
        ) -> impl Future<Output = Result<Vec<Vec<u8>>, TransportError>> + Send {
            let events = self.events.lock().unwrap().clone();
            async move { Ok(events) }
        }

        fn fetch_history_v2(
            &self,
            _channel_id: u64,
            _before_id: Option<u64>,
            _limit: u32,
        ) -> impl Future<Output = Result<Vec<farder_protocol::server::MessageInfoV2>, TransportError>> + Send {
            async move { Ok(vec![]) }
        }
    }

    /// Sign a `DeviceAuthorized` event for `(identity, device)` and return its
    /// raw signed bytes.
    fn device_authorized_bytes(
        identity: &Keypair,
        device: &Keypair,
        created_at: u64,
    ) -> Vec<u8> {
        let cert = DeviceCert::create(identity, &device.public_key(), created_at);
        let event = farder_crypto::event_log::Event::next(
            device,
            identity.public_key(),
            "server".to_string(),
            None,
            0,
            created_at,
            EventPayload::DeviceAuthorized { cert },
        );
        event.to_bytes()
    }

    /// Sign a `DeviceRevoked` event revoking `revoked_device`, authored by
    /// `identity` from `revoker` (any of its devices may revoke a sibling), and
    /// return its raw signed bytes. Mirrors production: the payload names the
    /// VICTIM, while `core.device` is the revoker's device.
    fn device_revoked_bytes(
        identity: &Keypair,
        revoker: &Keypair,
        revoked_device: &str,
        timestamp: u64,
    ) -> Vec<u8> {
        let event = farder_crypto::event_log::Event::next(
            revoker,
            identity.public_key(),
            "server".to_string(),
            None,
            0,
            timestamp,
            EventPayload::DeviceRevoked { device: revoked_device.to_string() },
        );
        event.to_bytes()
    }

    #[tokio::test]
    async fn returns_the_cert_for_a_genuine_identity_and_device() {
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let bytes = device_authorized_bytes(&identity, &device, 1);
        let transport = CertTransport::new(vec![bytes]);

        let cert = resolve_device_cert(&transport, &identity.public_key(), &device_id(&device.public_key()))
            .await
            .unwrap()
            .expect("a genuine cert must resolve");
        assert_eq!(cert.core.identity, identity.public_key());
        assert_eq!(cert.core.device_id, device_id(&device.public_key()));
    }

    #[tokio::test]
    async fn returns_none_when_the_cert_names_a_different_device() {
        let identity = Keypair::generate();
        let device_a = Keypair::generate();
        let device_b = Keypair::generate();
        // The cert is for device_a; we ask for device_b.
        let bytes = device_authorized_bytes(&identity, &device_a, 1);
        let transport = CertTransport::new(vec![bytes]);

        let cert = resolve_device_cert(&transport, &identity.public_key(), &device_id(&device_b.public_key()))
            .await
            .unwrap();
        assert!(cert.is_none(), "a cert for a different device must not resolve");
    }

    #[tokio::test]
    async fn returns_none_for_a_cert_whose_signature_does_not_verify() {
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let mut bytes = device_authorized_bytes(&identity, &device, 1);

        // Tamper with the cert signature: decode the event, corrupt the cert's
        // signature bytes, re-encode. `DeviceCert::verify` must reject it.
        let mut event = farder_crypto::event_log::Event::from_bytes(&bytes).unwrap();
        let EventPayload::DeviceAuthorized { cert } = &mut event.core.payload else {
            panic!("expected DeviceAuthorized");
        };
        cert.signature = vec![0u8; 64];
        bytes = event.to_bytes();

        let transport = CertTransport::new(vec![bytes]);
        let cert = resolve_device_cert(&transport, &identity.public_key(), &device_id(&device.public_key()))
            .await
            .unwrap();
        assert!(cert.is_none(), "a tampered cert must fail closed");
    }

    #[tokio::test]
    async fn a_resolver_serves_only_members_it_resolved() {
        // `build_cert_resolver` resolves a genuine member and leaves an
        // unresolvable one absent, so Gate 2 fails closed for the latter.
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let bytes = device_authorized_bytes(&identity, &device, 1);

        let t = CertTransport::new(vec![bytes]);
        let member = DeclaredMember {
            identity: identity.public_key(),
            device: device_id(&device.public_key()),
        };
        let resolver = build_cert_resolver(&t, std::slice::from_ref(&member)).await.unwrap();
        assert!(resolver.device_cert(&identity.public_key(), &device_id(&device.public_key())).is_some());

        // A member we never asked for is absent.
        let other = Keypair::generate();
        assert!(resolver.device_cert(&other.public_key(), "nope").is_none());
    }

    /// Pins the DEVICE filter: an identity may hold several authorized devices,
    /// and a cert for the WRONG device must never be handed to Gate 2. This is
    /// defense-in-depth -- `verify_leaf_binding` independently checks the
    /// device, so a wrong-device cert would be rejected there too -- but the
    /// resolver is a public primitive whose contract is "the cert for the
    /// requested device", and pinning it prevents a future caller from relying
    /// on `verify_leaf_binding` to catch a mismatch it was never asked to catch.
    /// Pins the IDENTITY half of the binding check: a batch containing another
    /// identity's cert must never resolve to it, even though that cert is
    /// genuine and verifies under ITS OWN identity key.
    #[tokio::test]
    async fn a_cert_for_a_different_identity_is_never_returned_even_when_batched() {
        let identity_a = Keypair::generate();
        let identity_b = Keypair::generate();
        let shared_device = Keypair::generate();

        // Two DIFFERENT identities, but the SAME device id -- so the device
        // check cannot distinguish them and only the identity filter can. If the
        // identity filter is removed, the resolver returns the last matching
        // cert (identity_b's) and this test's identity assertion fails.
        let bytes = vec![
            device_authorized_bytes(&identity_a, &shared_device, 1),
            device_authorized_bytes(&identity_b, &shared_device, 2),
        ];
        let transport = CertTransport::new(bytes);

        let cert = resolve_device_cert(&transport, &identity_a.public_key(), &device_id(&shared_device.public_key()))
            .await
            .unwrap()
            .expect("a genuine cert must resolve");
        assert_eq!(
            cert.core.identity,
            identity_a.public_key(),
            "the resolver must return the requested identity's cert, not another identity's"
        );
    }

    #[tokio::test]
    async fn a_cert_for_a_different_device_is_never_returned_even_when_batched() {
        let identity = Keypair::generate();
        let device_a = Keypair::generate();
        let device_b = Keypair::generate();

        // Both devices are genuinely authorized by this identity; the server
        // returns the whole batch (it keys the fetch by identity, not device).
        let bytes = vec![
            device_authorized_bytes(&identity, &device_a, 1),
            device_authorized_bytes(&identity, &device_b, 2),
        ];
        let transport = CertTransport::new(bytes);

        // Ask for device_a: we must get device_a's cert, not device_b's.
        let cert = resolve_device_cert(&transport, &identity.public_key(), &device_id(&device_a.public_key()))
            .await
            .unwrap()
            .expect("a genuine cert must resolve");
        assert_eq!(
            cert.core.device_id,
            device_id(&device_a.public_key()),
            "the resolver must return the cert for the requested device, not a sibling device"
        );
        assert_eq!(cert.core.identity, identity.public_key());
    }

    #[tokio::test]
    async fn a_revoked_devices_cert_is_not_returned_even_when_newest() {
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let revoker = Keypair::generate(); // a sibling device of the identity
        let dev_id = device_id(&device.public_key());

        // The device is authorized, then revoked. The revocation names the
        // VICTIM in its payload while the event is authored by the revoker —
        // reading `core.device` off the revoke event would give the wrong id.
        let auth = device_authorized_bytes(&identity, &device, 1);
        let revoke = device_revoked_bytes(&identity, &revoker, &dev_id, 2);

        let transport = CertTransport::new(vec![auth, revoke]);
        let cert = resolve_device_cert(&transport, &identity.public_key(), &dev_id)
            .await
            .unwrap();
        assert!(cert.is_none(), "a revoked device's cert must never resolve");
    }

    #[tokio::test]
    async fn a_cert_with_a_past_expiry_is_not_returned() {
        let identity = Keypair::generate();
        let device = Keypair::generate();
        let dev_id = device_id(&device.public_key());

        // expires_at = 100 unix seconds is far in the past (today is ~1.78e9),
        // so this cert is unambiguously expired regardless of clock skew.
        let cert = DeviceCert::create_expiring(&identity, &device.public_key(), 1, 100);
        let event = farder_crypto::event_log::Event::next(
            &device,
            identity.public_key(),
            "server".to_string(),
            None,
            0,
            1,
            EventPayload::DeviceAuthorized { cert },
        );
        let transport = CertTransport::new(vec![event.to_bytes()]);

        let resolved = resolve_device_cert(&transport, &identity.public_key(), &dev_id)
            .await
            .unwrap();
        assert!(resolved.is_none(), "a cert whose expiry is in the past must fail closed");
    }

    #[tokio::test]
    async fn revoking_one_device_does_not_block_the_identitys_other_devices() {
        let identity = Keypair::generate();
        let device_a = Keypair::generate();
        let device_b = Keypair::generate();
        let dev_a_id = device_id(&device_a.public_key());
        let dev_b_id = device_id(&device_b.public_key());

        let auth_a = device_authorized_bytes(&identity, &device_a, 1);
        let auth_b = device_authorized_bytes(&identity, &device_b, 2);
        // Revoke device_b from device_a.
        let revoke_b = device_revoked_bytes(&identity, &device_a, &dev_b_id, 3);

        let transport = CertTransport::new(vec![auth_a, auth_b, revoke_b]);

        // device_a (un-revoked) still resolves...
        let cert = resolve_device_cert(&transport, &identity.public_key(), &dev_a_id)
            .await
            .unwrap()
            .expect("revoking a sibling device must not block this device's cert");
        assert_eq!(cert.core.device_id, dev_a_id);
        // ...while device_b (revoked) does not.
        let revoked = resolve_device_cert(&transport, &identity.public_key(), &dev_b_id)
            .await
            .unwrap();
        assert!(revoked.is_none(), "the revoked device's cert must not resolve");
    }
}
