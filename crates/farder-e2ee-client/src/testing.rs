//! In-memory [`E2eeTransport`] double for unit tests.
//!
//! Kept `#[cfg(test)]` rather than a `testing` feature because every current
//! and planned consumer is a unit test *inside this crate* (Tasks 2-6). Task
//! 8's harness drives the real QUIC transport against an in-process server —
//! it does not want a fake — so there is no non-test consumer that would need
//! the double compiled into the shipped library.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::Mutex;

use farder_crypto::event_log::Event;
use farder_crypto::identity::PublicKey;

use crate::transport::{E2eeTransport, EventAccepted, MlsControl, TransportError, Welcomes};

/// Records every submitted [`Event`] and returns either a default accept or a
/// programmed per-submission result (FIFO). Programmed rejections inject
/// server reasons verbatim — notably the bare `"stale-epoch"` the resync loop
/// keys on.
#[derive(Default)]
pub struct FakeTransport {
    submitted: Mutex<Vec<Event>>,
    responses: Mutex<VecDeque<Result<EventAccepted, TransportError>>>,
    key_packages: Mutex<HashMap<(PublicKey, String), Vec<Vec<u8>>>>,
    device_certs: Mutex<HashMap<PublicKey, Vec<Vec<u8>>>>,
}

impl FakeTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// All events submitted so far, in submission order.
    pub fn submitted(&self) -> Vec<Event> {
        self.submitted.lock().unwrap().clone()
    }

    /// Number of `submit_event` calls so far.
    pub fn submit_count(&self) -> usize {
        self.submitted.lock().unwrap().len()
    }

    /// Program the next `submit_event` call to be rejected with `reason`.
    pub fn reject_next(&self, reason: &str) {
        self.responses
            .lock()
            .unwrap()
            .push_back(Err(TransportError::rejected(reason)));
    }

    /// Serve the raw signed `MlsKeyPackagePublished` event bytes that
    /// `fetch_key_packages(member, device)` should return, keyed exactly the
    /// way the server keys them: by the event's `(author, device)`.
    pub fn serve_key_packages(
        &self,
        member: &PublicKey,
        device: &str,
        events: Vec<Vec<u8>>,
    ) {
        self.key_packages
            .lock()
            .unwrap()
            .insert((member.clone(), device.to_string()), events);
    }

    /// Serve the raw signed `DeviceAuthorized` event bytes that
    /// `fetch_device_certs(identity)` should return, keyed by the identity
    /// public key (the event's `author`).
    pub fn serve_device_certs(&self, identity: &PublicKey, events: Vec<Vec<u8>>) {
        self.device_certs.lock().unwrap().insert(identity.clone(), events);
    }

    /// Program the next `submit_event` call to be accepted with `event_hash`.
    pub fn accept_next(&self, event_hash: &str) {
        self.responses.lock().unwrap().push_back(Ok(EventAccepted {
            event_hash: event_hash.to_string(),
            timestamp: 0,
        }));
    }

    fn default_accept(event: &Event) -> Result<EventAccepted, TransportError> {
        // Mirrors the server: `event_hash = event.hash()`.
        Ok(EventAccepted {
            event_hash: event.hash(),
            timestamp: event.core.timestamp,
        })
    }
}

impl E2eeTransport for FakeTransport {
    fn submit_event(
        &self,
        event: &Event,
    ) -> impl Future<Output = Result<EventAccepted, TransportError>> + Send {
        let event = event.clone();
        let result = {
            self.submitted.lock().unwrap().push(event.clone());
            let mut responses = self.responses.lock().unwrap();
            responses
                .pop_front()
                .unwrap_or_else(|| Self::default_accept(&event))
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
        _since_accept_seq: u64,
    ) -> impl Future<Output = Result<MlsControl, TransportError>> + Send {
        async move {
            Ok(MlsControl {
                events: Vec::new(),
                next_accept_seq: 0,
                more: false,
            })
        }
    }

    fn fetch_key_packages(
        &self,
        member: &PublicKey,
        device: &str,
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, TransportError>> + Send {
        let events = self
            .key_packages
            .lock()
            .unwrap()
            .get(&(member.clone(), device.to_string()))
            .cloned()
            .unwrap_or_default();
        async move { Ok(events) }
    }

    fn fetch_device_certs(
        &self,
        identity: &PublicKey,
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, TransportError>> + Send {
        let events = self
            .device_certs
            .lock()
            .unwrap()
            .get(identity)
            .cloned()
            .unwrap_or_default();
        async move { Ok(events) }
    }

    fn fetch_history_v2(
        &self,
        _channel_id: u64,
        _before_id: Option<u64>,
        _limit: u32,
    ) -> impl Future<Output = Result<Vec<farder_protocol::server::MessageInfoV2>, TransportError>> + Send {
        async move { Ok(Vec::new()) }
    }
}
