//! [`E2eeTransport`] implementation that drives the real server through the
//! client's existing QUIC request path (`bridge::send_request`), per D1 of the
//! 4b plan.
//!
//! This is NOT a Tauri command — it is called directly from Rust by the
//! downstream E2EE vertical (negotiate-on-connect, the MLS steward, sealed
//! send/decrypt). It adds zero new wire machinery: `send_request` already takes
//! the whole `ServerRequest` enum, so the v2 request variants are built and the
//! v2 response variants are destructured here.
//!
//! # `&self` + `Sync` (D1 note)
//!
//! `bridge::send_request` takes `&AppState` and reaches the connection through
//! `AppState`'s interior mutability (`std::sync::Mutex`), so the transport can
//! hold a shared `&AppState` plus the `server_id` and stay `&self`. `AppState`
//! is already `Send + Sync` (Tauri manages it as `Arc<AppState>`), so a
//! `&AppState` is `Send`, each returned future is `Send`, and this type is
//! `Sync` — satisfying the vertical's `T: E2eeTransport + Sync` bound.
//!
//! The whole module is currently un-referenced: the negotiate-on-connect task
//! (T2) is the first caller. Drop the `dead_code` allowance when it lands.

#![allow(dead_code)]

use std::future::Future;

use farder_crypto::event_log::Event;
use farder_crypto::identity::PublicKey;
use farder_e2ee_client::{E2eeTransport, EventAccepted, MlsControl, TransportError, Welcomes};
use farder_protocol::server::{MessageInfoV2, ServerRequest, ServerResponse};

use crate::state::AppState;

/// An [`E2eeTransport`] bound to one connection, identified by `server_id`.
pub struct E2eeTransportImpl<'a> {
    state: &'a AppState,
    server_id: String,
}

impl<'a> E2eeTransportImpl<'a> {
    /// Bind the transport to an already-connected server.
    pub fn new(state: &'a AppState, server_id: impl Into<String>) -> Self {
        Self {
            state,
            server_id: server_id.into(),
        }
    }
}

/// Send one request and await the raw response, mapping a connection/decode
/// failure to [`TransportError::Transport`].
async fn request(
    state: &AppState,
    server_id: &str,
    req: ServerRequest,
) -> Result<ServerResponse, TransportError> {
    crate::bridge::send_request(state, server_id, req)
        .await
        .map_err(|e| TransportError::transport(e.to_string()))
}

/// Map a raw `ServerResponse` past the shared rejection arm, preserving the
/// reason string verbatim — the `stale-epoch` resync loop keys on exact
/// equality. Non-error variants pass through for the caller to destructure.
fn or_transport_error(response: ServerResponse) -> Result<ServerResponse, TransportError> {
    match response {
        ServerResponse::Error { reason } => Err(TransportError::rejected(reason)),
        other => Ok(other),
    }
}

impl<'a> E2eeTransport for E2eeTransportImpl<'a> {
    fn submit_event(
        &self,
        event: &Event,
    ) -> impl Future<Output = Result<EventAccepted, TransportError>> + Send {
        let state = self.state;
        let server_id = self.server_id.clone();
        let event = event.clone();
        async move {
            let response = request(state, &server_id, ServerRequest::SubmitEvent { event }).await?;
            match or_transport_error(response)? {
                ServerResponse::EventAccepted {
                    event_hash,
                    timestamp,
                } => Ok(EventAccepted {
                    event_hash,
                    timestamp,
                }),
                other => Err(TransportError::transport(format!(
                    "unexpected response to SubmitEvent: {other:?}"
                ))),
            }
        }
    }

    fn fetch_welcomes(
        &self,
        channel_id: Option<u64>,
        since_accept_seq: u64,
    ) -> impl Future<Output = Result<Welcomes, TransportError>> + Send {
        let state = self.state;
        let server_id = self.server_id.clone();
        async move {
            let response = request(
                state,
                &server_id,
                ServerRequest::FetchWelcomes {
                    channel_id,
                    since_accept_seq,
                },
            )
            .await?;
            match or_transport_error(response)? {
                ServerResponse::Welcomes {
                    events,
                    next_accept_seq,
                    more,
                } => Ok(Welcomes {
                    events,
                    next_accept_seq,
                    more,
                }),
                other => Err(TransportError::transport(format!(
                    "unexpected response to FetchWelcomes: {other:?}"
                ))),
            }
        }
    }

    fn fetch_mls_control(
        &self,
        channel_id: u64,
        since_accept_seq: u64,
    ) -> impl Future<Output = Result<MlsControl, TransportError>> + Send {
        let state = self.state;
        let server_id = self.server_id.clone();
        async move {
            let response = request(
                state,
                &server_id,
                ServerRequest::FetchMlsControl {
                    channel_id,
                    since_accept_seq,
                },
            )
            .await?;
            match or_transport_error(response)? {
                ServerResponse::MlsControl {
                    events,
                    next_accept_seq,
                    more,
                } => Ok(MlsControl {
                    events,
                    next_accept_seq,
                    more,
                }),
                other => Err(TransportError::transport(format!(
                    "unexpected response to FetchMlsControl: {other:?}"
                ))),
            }
        }
    }

    fn fetch_key_packages(
        &self,
        member: &PublicKey,
        device: &str,
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, TransportError>> + Send {
        let state = self.state;
        let server_id = self.server_id.clone();
        let member = member.clone();
        let device = device.to_string();
        async move {
            let response =
                request(state, &server_id, ServerRequest::FetchKeyPackages { member, device })
                    .await?;
            match or_transport_error(response)? {
                ServerResponse::KeyPackages { events } => Ok(events),
                other => Err(TransportError::transport(format!(
                    "unexpected response to FetchKeyPackages: {other:?}"
                ))),
            }
        }
    }

    fn fetch_device_certs(
        &self,
        identity: &PublicKey,
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, TransportError>> + Send {
        let state = self.state;
        let server_id = self.server_id.clone();
        let identity = identity.clone();
        async move {
            let response =
                request(state, &server_id, ServerRequest::FetchDeviceCerts { identity }).await?;
            match or_transport_error(response)? {
                ServerResponse::DeviceCerts { events } => Ok(events),
                other => Err(TransportError::transport(format!(
                    "unexpected response to FetchDeviceCerts: {other:?}"
                ))),
            }
        }
    }

    fn fetch_history_v2(
        &self,
        channel_id: u64,
        before_id: Option<u64>,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<MessageInfoV2>, TransportError>> + Send {
        let state = self.state;
        let server_id = self.server_id.clone();
        async move {
            let response = request(
                state,
                &server_id,
                ServerRequest::FetchHistoryV2 {
                    channel_id,
                    before_id,
                    limit,
                },
            )
            .await?;
            match or_transport_error(response)? {
                ServerResponse::HistoryV2 { messages } => Ok(messages),
                other => Err(TransportError::transport(format!(
                    "unexpected response to FetchHistoryV2: {other:?}"
                ))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejection_reason_is_preserved_verbatim() {
        // The resync loop matches `stale-epoch` for exact equality on the reason
        // string; `or_transport_error` must not prefix or rewrite it.
        let err = or_transport_error(ServerResponse::Error {
            reason: "stale-epoch".to_string(),
        })
        .unwrap_err();
        assert_eq!(err, TransportError::rejected("stale-epoch"));
        assert!(err.is_stale_epoch());
    }

    #[test]
    fn prefixed_rejection_is_rejected_but_not_stale_epoch() {
        // Every other SubmitEvent rejection is "event rejected: <detail>". A
        // substring match for "event rejected" would miss the bare "stale-epoch";
        // this pins that the transport maps both verbatim and that the predicate
        // distinguishes them.
        let err = or_transport_error(ServerResponse::Error {
            reason: "event rejected: bad signature".to_string(),
        })
        .unwrap_err();
        assert_eq!(err, TransportError::rejected("event rejected: bad signature"));
        assert!(!err.is_stale_epoch());
    }

    #[test]
    fn non_error_response_passes_through_for_destructuring() {
        assert!(matches!(
            or_transport_error(ServerResponse::Ok),
            Ok(ServerResponse::Ok)
        ));
    }
}
