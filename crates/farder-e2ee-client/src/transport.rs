//! The transport seam: [`E2eeTransport`] plus the error type and the small
//! value structs the trait hands back.
//!
//! The trait's method signatures mirror the real server request/response shapes
//! in `farder-protocol::server` — see the per-method docs. The transport is the
//! only place this crate touches the wire; everything above it operates on
//! plain Rust values.

use std::fmt;
use std::future::Future;

use farder_crypto::event_log::Event;
use farder_crypto::identity::PublicKey;
use farder_protocol::server::MessageInfoV2;

/// The payload of `ServerResponse::EventAccepted` (`farder-protocol::server`),
/// mirrored field-for-field so the transport can hand back exactly what the
/// server accepted. The fields and their semantics are identical to the
/// protocol variant; this is a client-side value type, not a second wire type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventAccepted {
    /// Hex event hash the server assigned the accepted event.
    pub event_hash: String,
    /// The accepted event's `core.timestamp` (seconds, device wall-clock claim),
    /// echoed back by the server.
    pub timestamp: u64,
}

/// The payload of `ServerResponse::Welcomes` (`farder-protocol::server`),
/// mirrored field-for-field. `events` are raw signed `Event` bytes
/// (`rmp_serde`), oldest-first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Welcomes {
    pub events: Vec<Vec<u8>>,
    pub next_accept_seq: u64,
    pub more: bool,
}

/// The payload of `ServerResponse::MlsControl` (`farder-protocol::server`),
/// mirrored field-for-field. `events` are raw signed `Event` bytes
/// (`rmp_serde`), oldest-first, for one channel's MLS control plane
/// (`MlsCommit`, `MlsWelcome`, `MlsLeafConfirmed`, `MlsGroupReset`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MlsControl {
    pub events: Vec<Vec<u8>>,
    pub next_accept_seq: u64,
    pub more: bool,
}

/// A failure of the transport layer, as opposed to a rejection inside the
/// vertical's own logic.
///
/// [`TransportError::is_stale_epoch`] is the one machine-readable case the
/// resync loop keys on, and it exists specifically because the server reports
/// that condition as a *bare* reason string that a substring check for
/// `"event rejected"` would miss (see fact A2.2 of the 4a plan).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// The server answered `ServerResponse::Error { reason }`. `reason` is the
    /// server's reason string, verbatim and unprefixed.
    ServerRejected { reason: String },
    /// A transport/IO/serialization failure, or a response that did not match
    /// the variant the request expected.
    Transport(String),
}

impl TransportError {
    /// Build a [`TransportError::ServerRejected`] from a bare server reason.
    pub fn rejected(reason: impl Into<String>) -> Self {
        Self::ServerRejected {
            reason: reason.into(),
        }
    }

    /// Build a [`TransportError::Transport`] for a connection/IO/decode failure
    /// or an unexpected response variant.
    pub fn transport(message: impl Into<String>) -> Self {
        Self::Transport(message.into())
    }

    /// True iff the server rejected the request with the bare `"stale-epoch"`
    /// reason.
    ///
    /// The server returns this as `ServerResponse::Error { reason:
    /// "stale-epoch" }` — NOT prefixed with `"event rejected:"` like every
    /// other `SubmitEvent` rejection. It is compared for exact equality on the
    /// reason, which is the whole point: a substring match for
    /// `"event rejected"` would return false here and the resync loop would
    /// never run.
    pub fn is_stale_epoch(&self) -> bool {
        matches!(self, Self::ServerRejected { reason } if reason == "stale-epoch")
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServerRejected { reason } => write!(f, "server rejected request: {reason}"),
            Self::Transport(message) => write!(f, "transport error: {message}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// The transport seam the E2EE vertical runs over.
///
/// # Sync vs async
///
/// This trait is **async** because the real implementation in 4b is
/// `client/src-tauri/src/bridge.rs::send_request`, which is itself async (it
/// awaits a QUIC connection over `tokio`). A sync trait would force the 4b
/// wrapper to block on a runtime, or the vertical to be rewritten, for no
/// benefit. The harness (4a) also runs on `tokio`.
///
/// # Why `-> impl Future + Send`, not `async fn` / `#[async_trait]`
///
/// No sibling crate under `crates/*` uses `async-trait`, so we do not add that
/// dependency here. Native `async fn` in a public trait triggers the
/// `async_fn_in_trait` lint (and thus `-D warnings`) because it cannot spell out
/// auto-trait bounds, and it is also not object-safe. Instead each method
/// desugars to an ordinary `fn` returning `impl Future<...> + Send`: the `Send`
/// bound is what the `async fn` form cannot express, and it lets later tasks
/// `tokio::spawn` these futures. Trade-off: the trait is still not object-safe
/// (`dyn E2eeTransport` is impossible), which is fine — the vertical takes the
/// transport as a generic (`T: E2eeTransport`). If a future caller genuinely
/// needs `dyn`, switch to `#[async_trait]` then, not before.
///
/// # Implementing
///
/// Because the trait methods return `impl Future`, an implementation returns an
/// `async move` block rather than declaring `async fn`:
///
/// ```ignore
/// impl E2eeTransport for MyTransport {
///     fn submit_event(&self, event: &Event) -> impl Future<Output = Result<EventAccepted, TransportError>> + Send {
///         let event = event.clone();
///         async move {
///             // ... build `ServerRequest::SubmitEvent`, send it, destructure
///             // `ServerResponse::EventAccepted`, and map `Error { reason }`
///             // to `Err(TransportError::rejected(reason))`.
///         }
///     }
///     // ...
/// }
/// ```
pub trait E2eeTransport {
    /// Submit a signed event (`ServerRequest::SubmitEvent`) and await
    /// acceptance. Returns the `event_hash` + `timestamp` from
    /// `ServerResponse::EventAccepted`, or the transport error for a rejection.
    fn submit_event(
        &self,
        event: &Event,
    ) -> impl Future<Output = Result<EventAccepted, TransportError>> + Send;

    /// Fetch MLS Welcomes addressed to the caller
    /// (`ServerRequest::FetchWelcomes`). `channel_id` narrows the result; it
    /// never widens it. Returns the payload of `ServerResponse::Welcomes`.
    ///
    /// Pagination (fact A2.8): feed the returned `next_accept_seq` back as
    /// `since_accept_seq` and loop while `more == true`. The cursor advances
    /// past non-matching rows too, so never restart from 0.
    fn fetch_welcomes(
        &self,
        channel_id: Option<u64>,
        since_accept_seq: u64,
    ) -> impl Future<Output = Result<Welcomes, TransportError>> + Send;

    /// Fetch one channel's MLS control plane
    /// (`ServerRequest::FetchMlsControl`): raw signed `Event` bytes for
    /// `MlsCommit` / `MlsWelcome` / `MlsLeafConfirmed` / `MlsGroupReset`,
    /// oldest-first. Returns the payload of `ServerResponse::MlsControl`. This
    /// is how a member advances (or rebuilds) its group when ANOTHER member
    /// commits — the winning commit's bytes are only reachable through here
    /// (finding F3).
    ///
    /// Pagination mirrors `fetch_welcomes` (fact A2.8): feed the returned
    /// `next_accept_seq` back as `since_accept_seq` and loop while
    /// `more == true`. The cursor advances past non-matching rows too, so never
    /// restart from 0.
    fn fetch_mls_control(
        &self,
        channel_id: u64,
        since_accept_seq: u64,
    ) -> impl Future<Output = Result<MlsControl, TransportError>> + Send;

    /// Fetch the published KeyPackages for one member's device
    /// (`ServerRequest::FetchKeyPackages`). Returns the raw signed `Event`
    /// bytes from `ServerResponse::KeyPackages`, oldest-first.
    fn fetch_key_packages(
        &self,
        member: &PublicKey,
        device: &str,
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, TransportError>> + Send;

    /// Fetch the `DeviceAuthorized` events for one identity
    /// (`ServerRequest::FetchDeviceCerts`). Returns the raw signed `Event`
    /// bytes from `ServerResponse::DeviceCerts`, oldest-first.
    ///
    /// This is the ONLY production source of the `DeviceCert`s the receive-side
    /// leaf-binding gate (Gate 2) verifies against — a cert must come from HERE
    /// (the log), **never** from the commit under validation. See
    /// [`crate::cert`] for the production [`crate::commit::DeviceCertResolver`]
    /// built on top of this.
    fn fetch_device_certs(
        &self,
        identity: &PublicKey,
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, TransportError>> + Send;

    /// Fetch channel history that can carry sealed rows
    /// (`ServerRequest::FetchHistoryV2`). Returns the `messages` from
    /// `ServerResponse::HistoryV2`.
    fn fetch_history_v2(
        &self,
        channel_id: u64,
        before_id: Option<u64>,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<MessageInfoV2>, TransportError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_epoch_predicate_matches_the_bare_reason_string() {
        let e = TransportError::rejected("stale-epoch");
        assert!(e.is_stale_epoch(), "the bare 'stale-epoch' reason must match");
    }

    #[test]
    fn stale_epoch_predicate_rejects_an_event_rejected_prefixed_string() {
        // Every other SubmitEvent rejection is `ServerResponse::Error { reason:
        // "event rejected: <detail>" }`. A substring check for "event rejected"
        // would MISS the bare "stale-epoch" — this test pins that the predicate
        // distinguishes the two.
        let e = TransportError::rejected("event rejected: stale epoch");
        assert!(!e.is_stale_epoch());
    }

    #[test]
    fn stale_epoch_predicate_rejects_other_rejection_and_transport_errors() {
        assert!(!TransportError::rejected("event rejected: bad signature").is_stale_epoch());
        assert!(!TransportError::rejected("stale-epoch: extra").is_stale_epoch());
        assert!(!TransportError::transport("connection reset").is_stale_epoch());
    }

    #[test]
    fn transport_error_display_and_source_are_sane() {
        let e = TransportError::rejected("stale-epoch");
        assert_eq!(e.to_string(), "server rejected request: stale-epoch");
        // Implements std::error::Error so `?` into anyhow works in later tasks.
        let as_err: &dyn std::error::Error = &e;
        assert!(as_err.source().is_none());
    }
}
